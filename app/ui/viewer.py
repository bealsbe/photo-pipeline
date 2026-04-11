"""
Single-image viewer (Phase 3).

_Overlay        Opaque child-widget that holds one pre-rendered frame.
                Animated via QPropertyAnimation on its `pos` property.

_ImageView      QGraphicsView — zoom, pan, fit.
                slide_to(old, new, dir)  plays the push transition.
                capture_viewport()       snapshots whatever is visible now.

ImageViewer     Standalone window: nav controls, zoom bar, metadata strip.
                Captures a viewport screenshot before navigating; plays the
                animation as soon as the new pixmap is available (instantly
                if pre-cached, deferred by load time otherwise).

Push-animation contract
-----------------------
direction = +1  (forward)  : old slides left,  new enters from right
direction = -1  (backward) : old slides right, new enters from left

Rapid key presses are handled by cancelling any in-flight animation
immediately; the current visible state (which is already the new image
in the underlying view) becomes the starting frame for the next transition.

Keyboard shortcuts
------------------
← / →  (or A / D)  previous / next image
Home / End          first / last image
Escape              close viewer
F  / Space          fit to window
1                   100 % (actual pixels)
+  / =              zoom in
-                   zoom out
Delete / P          toggle prune mark
U                   unmark (clear prune)
"""
from __future__ import annotations

from pathlib import Path
from typing import List, Optional

from PySide6.QtCore import (
    QEasingCurve,
    QParallelAnimationGroup,
    QPoint,
    QPropertyAnimation,
    QSize,
    Qt,
    QUrl,
    Signal,
)
from PySide6.QtGui import (
    QColor,
    QDesktopServices,
    QPainter,
    QPixmap,
)
from PySide6.QtWidgets import (
    QApplication,
    QFormLayout,
    QFrame,
    QGraphicsPixmapItem,
    QGraphicsScene,
    QGraphicsView,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from app.models.photo_record import FileType, PhotoRecord
from app.thumbnails.image_loader import ImageLoader
from app.ui.icons import icon as _icon

try:
    import rawpy as _rawpy
    _RAWPY = True
except ImportError:
    _RAWPY = False

try:
    from PIL import Image as _PILImage
    from PIL.ExifTags import TAGS as _EXIF_TAGS
    _PIL = True
except ImportError:
    _PIL = False


# ── EXIF lookup tables ─────────────────────────────────────────────────────
_METERING_MODE = {
    0: "Unknown", 1: "Average", 2: "Center-weighted", 3: "Spot",
    4: "Multi-spot", 5: "Multi-segment", 6: "Partial", 255: "Other",
}
_EXP_PROGRAM = {
    0: "Not defined", 1: "Manual", 2: "Program", 3: "Aperture priority",
    4: "Shutter priority", 5: "Creative", 6: "Action", 7: "Portrait",
    8: "Landscape",
}
_EXP_MODE = {0: "Auto", 1: "Manual", 2: "Auto bracket"}
_WHITE_BALANCE = {0: "Auto", 1: "Manual"}
_SCENE_CAPTURE = {0: "Standard", 1: "Landscape", 2: "Portrait", 3: "Night"}
_FLASH_CODES = {
    0x00: "No flash",       0x01: "Flash fired",
    0x05: "Flash, no rtn",  0x07: "Flash, strobe",
    0x08: "Off",            0x10: "Off",
    0x18: "Auto – no flash",0x19: "Auto – flash",
    0x1d: "Auto – no rtn",  0x1f: "Auto – strobe",
    0x20: "No function",    0x30: "No function",
    0x41: "Red-eye",        0x45: "Red-eye, no rtn",
    0x47: "Red-eye, strobe",0x49: "Auto, red-eye",
}


def _to_float(val) -> Optional[float]:
    """Safe rational/numeric → float, None on failure."""
    try:
        f = float(val)
        return None if f != f else f   # NaN guard
    except Exception:
        return None


def _exif_from_pillow(path: str) -> dict:
    """
    Read all photography-relevant EXIF fields from *path* via Pillow.
    Works reliably for JPEG; partially for DNG/NEF/ARW that Pillow can parse.
    Returns a flat label→value string dict.
    """
    out: dict = {}
    with _PILImage.open(path) as img:
        exif = img.getexif()
        if not exif:
            return out

        ifd = exif.get_ifd(0x8769)   # Exif sub-IFD

        # ── Camera body ──────────────────────────────────────────────────
        make  = str(exif.get(271) or "").strip()
        model = str(exif.get(272) or "").strip()
        if make and model.upper().startswith(make.upper()):
            model = model[len(make):].strip()
        if make:
            out["Make"] = make
        if model:
            out["Model"] = model

        sw = exif.get(305)
        if sw:
            out["Software"] = str(sw).strip()

        # ── Date taken ───────────────────────────────────────────────────
        dt_raw = str(
            ifd.get(36867) or ifd.get(36868) or exif.get(306) or ""
        ).strip()
        if dt_raw:
            try:
                d = dt_raw[:10].replace(":", "-")
                t = dt_raw[11:16]
                out["Date"] = f"{d}  {t}"
            except Exception:
                out["Date"] = dt_raw[:16]

        # ── Exposure ─────────────────────────────────────────────────────
        iso = ifd.get(34855) or exif.get(34855)
        if iso:
            try:
                out["ISO"] = str(int(iso))
            except Exception:
                pass

        fn = _to_float(ifd.get(33437) or exif.get(33437))
        if fn is not None:
            out["Aperture"] = f"f/{fn:g}"

        et = _to_float(ifd.get(33434) or exif.get(33434))
        if et is not None:
            out["Shutter"] = (
                f"1/{round(1/et)} s" if 0 < et < 1.0 else f"{et:g} s"
            )

        ev = _to_float(ifd.get(37380))
        if ev is not None:
            out["EV Comp"] = f"{'%+.1f' % ev} EV"

        prog = ifd.get(34850)
        if prog is not None:
            label = _EXP_PROGRAM.get(int(prog))
            if label:
                out["Exp Program"] = label

        mode = ifd.get(41986)
        if mode is not None:
            label = _EXP_MODE.get(int(mode))
            if label:
                out["Exp Mode"] = label

        met = ifd.get(37383) or exif.get(37383)
        if met is not None:
            label = _METERING_MODE.get(int(met))
            if label:
                out["Metering"] = label

        flash = ifd.get(37385) or exif.get(37385)
        if flash is not None:
            fi = int(flash)
            fired = bool(fi & 1)
            out["Flash"] = _FLASH_CODES.get(fi, "Flash fired" if fired else "No flash")

        wb = ifd.get(41987)
        if wb is not None:
            label = _WHITE_BALANCE.get(int(wb))
            if label:
                out["White Balance"] = label

        scene = ifd.get(41990)
        if scene is not None:
            label = _SCENE_CAPTURE.get(int(scene))
            if label:
                out["Scene"] = label

        # ── Focal length ─────────────────────────────────────────────────
        fl = _to_float(ifd.get(37386) or exif.get(37386))
        if fl is not None:
            out["Focal Length"] = f"{fl:g} mm"

        fl35 = ifd.get(41989)
        if fl35:
            try:
                out["35mm Equiv"] = f"{int(fl35)} mm"
            except Exception:
                pass

        # ── Subject distance ─────────────────────────────────────────────
        sd = _to_float(ifd.get(37382))
        if sd and sd > 0:
            if sd >= 1000:
                out["Subject Dist"] = f"{sd/1000:.1f} km"
            elif sd >= 1:
                out["Subject Dist"] = f"{sd:.2f} m"
            else:
                out["Subject Dist"] = f"{sd*100:.0f} cm"

        # ── Lens ─────────────────────────────────────────────────────────
        lmake = str(ifd.get(42035) or "").strip()
        lmodel = str(ifd.get(42036) or "").strip()
        if lmake and lmodel.upper().startswith(lmake.upper()):
            lmodel = lmodel[len(lmake):].strip()
        if lmake:
            out["Lens Make"] = lmake
        if lmodel:
            out["Lens Model"] = lmodel

        lspec = ifd.get(42034)
        if lspec:
            try:
                v = [_to_float(x) for x in lspec]
                if v[0] is not None and v[1] is not None:
                    fl_part = (
                        f"{v[0]:g} mm" if v[0] == v[1]
                        else f"{v[0]:g}–{v[1]:g} mm"
                    )
                    fn_part = f"  f/{v[2]:g}" if v[2] else ""
                    out["Lens Spec"] = fl_part + fn_part
            except Exception:
                pass

        # ── GPS ───────────────────────────────────────────────────────────
        gps = exif.get_ifd(0x8825)
        if gps:
            try:
                def _dms(v):
                    return sum(float(x) / 60**i for i, x in enumerate(v))
                lat = gps.get(2)
                lon = gps.get(4)
                if lat and lon:
                    lf = _dms(lat) * (1 if gps.get(1, "N") == "N" else -1)
                    lo = _dms(lon) * (1 if gps.get(3, "E") == "E" else -1)
                    out["GPS"] = f"{lf:.5f}, {lo:.5f}"
            except Exception:
                pass

    return out


def _exif_from_rawpy(path: str) -> dict:
    """Read basic EXIF from a RAW file via rawpy (fallback when Pillow can't parse)."""
    out: dict = {}
    with _rawpy.imread(path) as raw:
        m = raw.metadata
        make  = (m.camera_make  or "").strip()
        model = (m.camera_model or "").strip()
        if make and model.upper().startswith(make.upper()):
            model = model[len(make):].strip()
        if make:
            out["Make"] = make
        if model:
            out["Model"] = model
        if m.iso:
            out["ISO"] = str(int(m.iso))
        if m.aperture:
            out["Aperture"] = f"f/{m.aperture:g}"
        if m.shutter:
            s = m.shutter
            out["Shutter"] = f"1/{round(1/s)} s" if 0 < s < 1.0 else f"{s:g} s"
        if m.focal_len:
            out["Focal Length"] = f"{m.focal_len:g} mm"
    return out


def _read_exif_fields(
    record: PhotoRecord,
    pair_record: Optional["PhotoRecord"] = None,
) -> dict:
    """
    Return an ordered dict of section → [(label, value), ...] for the side panel.
    Tries Pillow first (full EXIF), falls back to rawpy for RAW files.
    If a JPG pair is provided and the RAW is missing CAMERA / EXPOSURE fields,
    the JPG's EXIF is read silently and used to fill those gaps.
    """
    flat: dict = {}

    if _PIL:
        try:
            flat = _exif_from_pillow(str(record.path))
        except Exception:
            pass

    if record.file_type == FileType.RAW and _RAWPY:
        try:
            rp = _exif_from_rawpy(str(record.path))
            for k, v in rp.items():
                flat.setdefault(k, v)   # rawpy fills gaps Pillow couldn't cover
        except Exception:
            pass

    # Sneaky fallback: if we're showing a RAW and the JPG pair exists,
    # use the JPG's EXIF to fill any CAMERA / EXPOSURE fields still missing.
    _SNEAKY_KEYS = {
        "Make", "Model", "Software",
        "Date", "ISO", "Aperture", "Shutter", "EV Comp",
        "Exp Program", "Exp Mode", "Metering", "Flash",
        "White Balance", "Scene", "Subject Dist",
        "Focal Length", "35mm Equiv", "Lens Make", "Lens Model", "Lens Spec",
    }
    if (
        pair_record is not None
        and pair_record.file_type == FileType.JPG
        and _PIL
        and any(k not in flat for k in _SNEAKY_KEYS)
    ):
        try:
            jpg_flat = _exif_from_pillow(str(pair_record.path))
            for k, v in jpg_flat.items():
                if k in _SNEAKY_KEYS:
                    flat.setdefault(k, v)
        except Exception:
            pass

    if not flat:
        return {}

    _CAMERA_KEYS   = ["Make", "Model", "Software"]
    _EXPOSURE_KEYS = ["Date", "ISO", "Aperture", "Shutter", "EV Comp",
                      "Exp Program", "Exp Mode", "Metering", "Flash",
                      "White Balance", "Scene", "Subject Dist"]
    _LENS_KEYS     = ["Lens Make", "Lens Model", "Lens Spec",
                      "Focal Length", "35mm Equiv"]
    _GEO_KEYS      = ["GPS"]

    sections: dict = {}
    for name, keys in [
        ("CAMERA",   _CAMERA_KEYS),
        ("EXPOSURE", _EXPOSURE_KEYS),
        ("LENS",     _LENS_KEYS),
        ("GPS",      _GEO_KEYS),
    ]:
        rows = [(k, flat[k]) for k in keys if k in flat]
        if rows:
            sections[name] = rows
    return sections


# Animation tuning
_ANIM_MS   = 200                        # total duration in milliseconds
_ANIM_EASE = QEasingCurve.OutQuart     # sharper deceleration — snappy, modern


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


# ──────────────────────────────────────────────────────────────────────────────
# Overlay widget (one animation frame)
# ──────────────────────────────────────────────────────────────────────────────

class _Overlay(QWidget):
    """
    Opaque child-widget of the viewport that draws exactly one pre-rendered
    QPixmap at its own size.  Its `pos` property is animated by the slide
    transition; nothing else touches it.
    """

    def __init__(self, pixmap: QPixmap, parent: QWidget) -> None:
        super().__init__(parent)
        self._pixmap = pixmap
        # Opaque so it fully covers the view behind it
        self.setAttribute(Qt.WA_OpaquePaintEvent)
        self.setAttribute(Qt.WA_NoSystemBackground)

    def paintEvent(self, _event) -> None:
        p = QPainter(self)
        # pixmap was pre-rendered at viewport size — draw 1:1, no scaling
        p.drawPixmap(0, 0, self._pixmap)


# ──────────────────────────────────────────────────────────────────────────────
# Image view (zoom + pan + slide animation)
# ──────────────────────────────────────────────────────────────────────────────

class _ImageView(QGraphicsView):
    """
    QGraphicsView with:
    • Mouse-wheel zoom anchored under cursor
    • Click-drag panning (ScrollHandDrag)
    • Fit mode that re-fits on window resize
    • Double-click to toggle fit ↔ 100 %
    • slide_to() push transition between images
    """

    zoom_changed: Signal = Signal(float)   # current scale factor (1.0 = 100 %)

    _MIN_SCALE = 0.02
    _MAX_SCALE = 32.0
    _STEP      = 1.2

    def __init__(self, parent=None) -> None:
        super().__init__(parent)

        self._scene = QGraphicsScene(self)
        self.setScene(self._scene)

        self._item = QGraphicsPixmapItem()
        self._item.setTransformationMode(Qt.SmoothTransformation)
        self._scene.addItem(self._item)

        # Status overlay (loading / error text)
        self._status = QLabel("", self)
        self._status.setAlignment(Qt.AlignCenter)
        self._status.setStyleSheet(
            "color:#999;font-size:18px;background:transparent;"
        )
        self._status.setAttribute(Qt.WA_TransparentForMouseEvents)
        self._status.hide()

        # Animation state
        self._anim_group: Optional[QParallelAnimationGroup] = None
        self._anim_overlays: List[_Overlay] = []

        self._fit_mode = True

        self.setTransformationAnchor(QGraphicsView.AnchorUnderMouse)
        self.setResizeAnchor(QGraphicsView.AnchorViewCenter)
        self.setRenderHints(QPainter.SmoothPixmapTransform | QPainter.Antialiasing)
        self.setDragMode(QGraphicsView.ScrollHandDrag)
        self.setBackgroundBrush(QColor(0x0a, 0x0a, 0x12))
        self.setFrameShape(QFrame.NoFrame)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        self.setVerticalScrollBarPolicy(Qt.ScrollBarAsNeeded)

        # Single-pixel scroll steps so drag-scroll and keyboard pan are smooth
        self.verticalScrollBar().setSingleStep(1)
        self.horizontalScrollBar().setSingleStep(1)

        _sb_qss = """
            QScrollBar:vertical {
                background: #0a0a12; width: 7px; border: none; margin: 0;
            }
            QScrollBar::handle:vertical {
                background: #2a2a40; border-radius: 3px; min-height: 24px;
            }
            QScrollBar::handle:vertical:hover    { background: rgba(255,109,0,0.45); }
            QScrollBar::handle:vertical:pressed   { background: rgba(255,109,0,0.70); }
            QScrollBar::add-line:vertical,
            QScrollBar::sub-line:vertical         { height: 0; background: none; }
            QScrollBar::add-page:vertical,
            QScrollBar::sub-page:vertical         { background: transparent; }
            QScrollBar:horizontal {
                background: #0a0a12; height: 7px; border: none; margin: 0;
            }
            QScrollBar::handle:horizontal {
                background: #2a2a40; border-radius: 3px; min-width: 24px;
            }
            QScrollBar::handle:horizontal:hover   { background: rgba(255,109,0,0.45); }
            QScrollBar::handle:horizontal:pressed  { background: rgba(255,109,0,0.70); }
            QScrollBar::add-line:horizontal,
            QScrollBar::sub-line:horizontal        { width: 0; background: none; }
            QScrollBar::add-page:horizontal,
            QScrollBar::sub-page:horizontal        { background: transparent; }
        """
        self.verticalScrollBar().setStyleSheet(_sb_qss)
        self.horizontalScrollBar().setStyleSheet(_sb_qss)

    # ------------------------------------------------------------------ #
    # Content API                                                          #
    # ------------------------------------------------------------------ #

    def set_loading(self) -> None:
        self._cancel_anim()
        self._item.setPixmap(QPixmap())
        self._show_status("Loading\u2026")

    def set_error(self, msg: str) -> None:
        self._cancel_anim()
        self._item.setPixmap(QPixmap())
        self._show_status(f"Could not load image\n{msg}")

    def set_pixmap(self, pixmap: QPixmap) -> None:
        """Set new pixmap without animation."""
        self._status.hide()
        self._item.setPixmap(pixmap)
        self._scene.setSceneRect(self._item.boundingRect())
        self._fit_mode = True
        self._fit_now()

    # ------------------------------------------------------------------ #
    # Transition API                                                       #
    # ------------------------------------------------------------------ #

    def capture_viewport(self) -> QPixmap:
        """
        Render whatever is currently visible in the viewport to a QPixmap.
        This includes any in-flight animation overlays, so it always reflects
        the true current visual state — correct for use as the 'old' frame
        even during rapid key presses.
        """
        shot = QPixmap(self.viewport().size())
        self.viewport().render(shot)
        return shot

    def slide_to(
        self,
        old_shot: QPixmap,
        new_pixmap: QPixmap,
        direction: int,
    ) -> None:
        """
        Push transition: old_shot slides out, new_pixmap slides in.

        direction  +1 → old exits left,  new enters from right  (forward)
                   -1 → old exits right, new enters from left   (backward)

        Any in-flight animation is cancelled first, so rapid key presses
        stay perfectly responsive.
        """
        self._cancel_anim()

        # Plant the new image in the underlying view (hidden behind overlays)
        self._status.hide()
        self._item.setPixmap(new_pixmap)
        self._scene.setSceneRect(self._item.boundingRect())
        self._fit_mode = True
        self._fit_now()

        vp   = self.viewport()
        w, h = vp.width(), vp.height()

        # Pre-render the new image at fit scale for its overlay
        new_shot = self._render_fit(new_pixmap, QSize(w, h))

        # Build overlays
        old_ov = _Overlay(old_shot, vp)
        old_ov.setGeometry(0, 0, w, h)
        old_ov.show()
        old_ov.raise_()

        new_ov = _Overlay(new_shot, vp)
        new_ov.setGeometry(w * direction, 0, w, h)
        new_ov.show()
        new_ov.raise_()

        self._anim_overlays = [old_ov, new_ov]

        # Build parallel animation group
        group = QParallelAnimationGroup(self)
        for ov, start, end in [
            (old_ov, QPoint(0, 0),              QPoint(-w * direction, 0)),
            (new_ov, QPoint(w * direction, 0),  QPoint(0, 0)),
        ]:
            a = QPropertyAnimation(ov, b"pos", group)
            a.setDuration(_ANIM_MS)
            a.setEasingCurve(_ANIM_EASE)
            a.setStartValue(start)
            a.setEndValue(end)
            group.addAnimation(a)

        group.finished.connect(self._cancel_anim)
        group.start()
        self._anim_group = group

    # ------------------------------------------------------------------ #
    # Zoom API                                                             #
    # ------------------------------------------------------------------ #

    def fit_view(self) -> None:
        self._fit_mode = True
        self._fit_now()

    def zoom_actual(self) -> None:
        self._fit_mode = False
        self.resetTransform()
        self.scale(1.0, 1.0)
        self.zoom_changed.emit(1.0)

    def zoom_in(self)  -> None: self._scale_by(self._STEP)
    def zoom_out(self) -> None: self._scale_by(1 / self._STEP)

    # ------------------------------------------------------------------ #
    # Events                                                               #
    # ------------------------------------------------------------------ #

    def keyPressEvent(self, event) -> None:
        # Arrow keys (and all other viewer shortcuts) must reach ImageViewer.
        # QGraphicsView would otherwise consume arrows for scrolling.
        event.ignore()

    def wheelEvent(self, event) -> None:
        # At fit-to-window or with Ctrl held: wheel zooms (original behaviour).
        # When already zoomed in without Ctrl: wheel scrolls the viewport so
        # panning a large image feels natural.
        if self._fit_mode or (event.modifiers() & Qt.ControlModifier):
            self._scale_by(self._STEP if event.angleDelta().y() > 0 else 1 / self._STEP)
        else:
            super().wheelEvent(event)

    def mouseDoubleClickEvent(self, event) -> None:
        if self._fit_mode:
            self.zoom_actual()
        else:
            self.fit_view()

    def resizeEvent(self, event) -> None:
        super().resizeEvent(event)
        self._status.setGeometry(self.rect())
        if self._fit_mode:
            self._fit_now()

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _fit_now(self) -> None:
        if not self._item.pixmap().isNull():
            self.resetTransform()
            self.fitInView(self._item, Qt.KeepAspectRatio)
            self.zoom_changed.emit(self.transform().m11())

    def _scale_by(self, factor: float) -> None:
        cur = self.transform().m11()
        new = max(self._MIN_SCALE, min(self._MAX_SCALE, cur * factor))
        self.resetTransform()
        self.scale(new, new)
        self._fit_mode = False
        self.zoom_changed.emit(new)

    def _show_status(self, text: str) -> None:
        self._status.setText(text)
        self._status.setGeometry(self.rect())
        self._status.show()
        self._status.raise_()

    def _cancel_anim(self) -> None:
        """Stop in-flight animation and clean up overlay widgets."""
        if self._anim_group is not None:
            self._anim_group.stop()
            self._anim_group.deleteLater()
            self._anim_group = None
        for ov in self._anim_overlays:
            ov.deleteLater()
        self._anim_overlays = []

    @staticmethod
    def _render_fit(pixmap: QPixmap, size: QSize) -> QPixmap:
        """
        Render *pixmap* centred + fit-in-size on a dark background.
        Used to pre-render the incoming frame for the slide overlay.
        """
        result = QPixmap(size)
        result.fill(QColor(0x0a, 0x0a, 0x12))
        if pixmap.isNull():
            return result
        scaled = pixmap.scaled(size, Qt.KeepAspectRatio, Qt.SmoothTransformation)
        p = QPainter(result)
        p.drawPixmap(
            (size.width()  - scaled.width())  // 2,
            (size.height() - scaled.height()) // 2,
            scaled,
        )
        p.end()
        return result


# ──────────────────────────────────────────────────────────────────────────────
# Viewer window
# ──────────────────────────────────────────────────────────────────────────────

class ImageViewer(QWidget):
    """
    Standalone non-modal viewer window.

    Navigation captures a viewport screenshot before moving so the push
    animation always has a crisp 'old' frame — even mid-animation.
    """

    prune_toggled: Signal = Signal(object)   # PhotoRecord

    def __init__(
        self,
        records: List[PhotoRecord],
        start_index: int,
        pair_lookup=None,
        parent=None,
    ) -> None:
        super().__init__(parent, Qt.Window)
        self.setWindowTitle("Viewer")
        self.setAttribute(Qt.WA_DeleteOnClose)

        screen = QApplication.primaryScreen().availableGeometry()
        w = int(screen.width()  * 0.72)
        h = int(screen.height() * 0.78)
        self.resize(w, h)
        self.move(
            screen.x() + (screen.width()  - w) // 2,
            screen.y() + (screen.height() - h) // 2,
        )

        self._records: List[PhotoRecord] = list(records)
        self._index: int = max(0, min(start_index, len(records) - 1))
        self._pair_lookup = pair_lookup or (lambda _: None)
        self._loader = ImageLoader(self)
        self._show_pair_mode: bool = False   # True when displaying the pair file
        self._pair_record: Optional[PhotoRecord] = None
        self._exif_cache: dict = {}          # path → sections dict, avoids re-reading

        # Pending transition state
        self._pending_shot: Optional[QPixmap] = None   # snapshot of old view
        self._nav_direction: int = 1                    # +1 fwd, -1 bwd

        self._build_ui()
        self._loader.image_ready.connect(self._on_image_ready)
        self._loader.load_failed.connect(self._on_load_failed)
        self._load_current(animate=False)   # first open: no animation

    # ------------------------------------------------------------------ #
    # Public                                                               #
    # ------------------------------------------------------------------ #

    def navigate_to(self, records: List[PhotoRecord], index: int) -> None:
        self._records = list(records)
        self._index = max(0, min(index, len(records) - 1))
        self._pending_shot = None
        self._load_current(animate=False)

    # ------------------------------------------------------------------ #
    # UI construction                                                      #
    # ------------------------------------------------------------------ #

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        root.addWidget(self._build_controls())
        self._prune_banner = self._build_prune_banner()
        root.addWidget(self._prune_banner)

        # Middle row: image view + collapsible EXIF panel
        middle = QWidget()
        mid_lay = QHBoxLayout(middle)
        mid_lay.setContentsMargins(0, 0, 0, 0)
        mid_lay.setSpacing(0)
        self._view = _ImageView()
        mid_lay.addWidget(self._view, 1)
        self._exif_panel = self._build_exif_panel()
        mid_lay.addWidget(self._exif_panel)
        root.addWidget(middle, 1)

        root.addWidget(self._build_metadata_bar())

        self._btn_prev.clicked.connect(self.go_prev)
        self._btn_next.clicked.connect(self.go_next)
        self._btn_pair.clicked.connect(self._toggle_pair_view)
        self._btn_fit.clicked.connect(self._view.fit_view)
        self._btn_actual.clicked.connect(self._view.zoom_actual)
        self._btn_zoomin.clicked.connect(self._view.zoom_in)
        self._btn_zoomout.clicked.connect(self._view.zoom_out)
        self._view.zoom_changed.connect(self._on_zoom_changed)
        self._btn_prune.clicked.connect(self._toggle_prune)
        self._btn_show_folder.clicked.connect(self._show_in_folder)
        self._btn_info.clicked.connect(self._toggle_exif_panel)

    def _build_controls(self) -> QWidget:
        bar = QWidget()
        bar.setFixedHeight(42)
        bar.setStyleSheet(
            "background:#0e0e1a;"
            "border-bottom: 1px solid rgba(255,109,0,0.12);"
        )
        lay = QHBoxLayout(bar)
        lay.setContentsMargins(8, 0, 8, 0)
        lay.setSpacing(4)

        _nav_style = (
            "QPushButton{background:rgba(255,109,0,0.08);border:1px solid rgba(255,109,0,0.20);"
            "border-radius:4px;color:#c8c8d8;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);border-color:rgba(255,109,0,0.45);}"
            "QPushButton:disabled{background:transparent;border-color:#1e1e2e;color:#2e2e48;}"
        )

        def _nav(label: str, icon_name: str) -> QPushButton:
            b = QPushButton()
            b.setFixedSize(34, 30)
            ic = _icon(icon_name, size=16)
            if not ic.isNull():
                b.setIcon(ic)
            else:
                b.setText(label)
            b.setStyleSheet(_nav_style)
            return b

        def _tool(label: str) -> QPushButton:
            b = QPushButton(label)
            b.setFixedSize(38, 28)
            b.setStyleSheet(
                "QPushButton{background:rgba(255,109,0,0.08);color:#8888a8;"
                "border:1px solid rgba(255,109,0,0.18);border-radius:4px;font-size:12px;}"
                "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
                "border-color:rgba(255,109,0,0.45);}"
            )
            return b

        self._btn_prev   = _nav("◀", "angle-left")
        self._btn_next   = _nav("▶", "angle-right")
        self._lbl_name   = QLabel()
        self._lbl_name.setStyleSheet("color:#f0f0f0;font-weight:bold;font-size:13px;")
        self._lbl_pos    = QLabel()
        self._lbl_pos.setStyleSheet("color:#44445a;font-size:12px;")
        self._lbl_zoom   = QLabel("—")
        self._lbl_zoom.setStyleSheet(
            "color:#7878a0;font-size:12px;min-width:52px;"
        )
        self._lbl_zoom.setAlignment(Qt.AlignCenter)
        self._btn_zoomout = _tool("−")
        self._btn_zoomin  = _tool("+")
        self._btn_fit     = _tool("Fit")
        ic_fit = _icon("expand", size=12)
        if not ic_fit.isNull():
            self._btn_fit.setIcon(ic_fit)
        self._btn_actual  = _tool("1:1")

        self._btn_prune = QPushButton("Mark")
        self._btn_prune.setFixedSize(62, 28)
        self._btn_prune.setCheckable(True)
        self._btn_prune.setStyleSheet(self._prune_style(False))
        ic_trash = _icon("trash-alt", size=12)
        if not ic_trash.isNull():
            self._btn_prune.setIcon(ic_trash)

        self._btn_pair = QPushButton("⇄ Pair")
        self._btn_pair.setFixedSize(66, 28)
        self._btn_pair.setCheckable(True)
        self._btn_pair.setEnabled(False)
        self._btn_pair.setStyleSheet(self._pair_btn_style(False, False))

        self._btn_show_folder = QPushButton()
        self._btn_show_folder.setFixedSize(30, 28)
        self._btn_show_folder.setToolTip("Show in folder")
        ic_folder = _icon("folder-open", size=13)
        if not ic_folder.isNull():
            self._btn_show_folder.setIcon(ic_folder)
        else:
            self._btn_show_folder.setText("📂")
        self._btn_show_folder.setStyleSheet(
            "QPushButton{background:rgba(255,109,0,0.08);color:#8888a8;"
            "border:1px solid rgba(255,109,0,0.18);border-radius:4px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
            "border-color:rgba(255,109,0,0.45);}"
        )

        self._btn_info = QPushButton("Info")
        self._btn_info.setFixedSize(42, 28)
        self._btn_info.setCheckable(True)
        self._btn_info.setChecked(True)
        self._btn_info.setStyleSheet(self._info_btn_style(True))

        lay.addWidget(self._btn_prev)
        lay.addWidget(self._btn_next)
        lay.addSpacing(10)
        lay.addWidget(self._lbl_name)
        lay.addSpacing(6)
        lay.addWidget(self._lbl_pos)
        lay.addStretch()
        lay.addWidget(self._btn_pair)
        lay.addSpacing(4)
        lay.addWidget(self._btn_show_folder)
        lay.addSpacing(4)
        lay.addWidget(self._btn_prune)
        lay.addSpacing(10)
        lay.addWidget(self._btn_info)
        lay.addSpacing(6)
        lay.addWidget(self._lbl_zoom)
        lay.addWidget(self._btn_zoomout)
        lay.addWidget(self._btn_zoomin)
        lay.addSpacing(6)
        lay.addWidget(self._btn_fit)
        lay.addWidget(self._btn_actual)
        return bar

    def _build_prune_banner(self) -> QWidget:
        bar = QWidget()
        bar.setFixedHeight(28)
        bar.setStyleSheet(
            "background:#2a0a0a;"
            "border-bottom:1px solid rgba(204,48,48,0.40);"
        )
        lay = QHBoxLayout(bar)
        lay.setContentsMargins(12, 0, 8, 0)
        lay.setSpacing(8)
        lbl = QLabel("\u2715  MARKED FOR PRUNING")
        lbl.setStyleSheet(
            "color:#cc4848;font-weight:bold;font-size:12px;letter-spacing:1px;"
        )
        btn_unmark = QPushButton("Unmark")
        btn_unmark.setFixedSize(60, 20)
        btn_unmark.setStyleSheet(
            "QPushButton{background:#3a1010;color:#cc4848;"
            "border:1px solid rgba(204,48,48,0.40);"
            "border-radius:3px;font-size:11px;}"
            "QPushButton:hover{background:#5a1818;color:#ffaaaa;}"
        )
        btn_unmark.clicked.connect(self._toggle_prune)
        lay.addWidget(lbl)
        lay.addStretch()
        lay.addWidget(btn_unmark)
        bar.hide()
        return bar

    def _build_metadata_bar(self) -> QWidget:
        bar = QWidget()
        bar.setStyleSheet(
            "background:#0e0e1a;border-top:1px solid rgba(255,109,0,0.08);"
        )
        from PySide6.QtWidgets import QVBoxLayout as _QVBox
        outer = _QVBox(bar)
        outer.setContentsMargins(12, 3, 12, 3)
        outer.setSpacing(1)

        self._lbl_meta = QLabel("—")
        self._lbl_meta.setStyleSheet("color:#44445a;font-size:11px;")

        self._lbl_exif = QLabel("")
        self._lbl_exif.setStyleSheet("color:#5a5a7a;font-size:11px;")
        self._lbl_exif.hide()

        self._lbl_pair_paths = QLabel("")
        self._lbl_pair_paths.setStyleSheet(
            "color:#5a5a7a;font-size:10px;"
        )
        self._lbl_pair_paths.hide()

        outer.addWidget(self._lbl_meta)
        outer.addWidget(self._lbl_exif)
        outer.addWidget(self._lbl_pair_paths)
        return bar

    def _build_exif_panel(self) -> QScrollArea:
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFixedWidth(250)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setVerticalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setStyleSheet(
            "QScrollArea {"
            "  background: #0a0a12;"
            "  border-left: 1px solid rgba(255,109,0,0.12);"
            "}"
            "QScrollBar:vertical {"
            "  background: #0a0a12; width: 6px; border: none; margin: 0;"
            "}"
            "QScrollBar::handle:vertical {"
            "  background: #2a2a40; border-radius: 3px; min-height: 20px;"
            "}"
            "QScrollBar::handle:vertical:hover { background: rgba(255,109,0,0.40); }"
            "QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical"
            "  { height: 0; background: none; }"
        )

        self._exif_inner = QWidget()
        self._exif_inner.setStyleSheet("background: #0a0a12;")
        self._exif_layout = QVBoxLayout(self._exif_inner)
        self._exif_layout.setContentsMargins(12, 12, 8, 12)
        self._exif_layout.setSpacing(0)
        self._exif_layout.addStretch()

        scroll.setWidget(self._exif_inner)
        return scroll

    def _populate_exif_panel(
        self,
        record: PhotoRecord,
        pair: Optional[PhotoRecord] = None,
    ) -> None:
        """Clear and rebuild the EXIF side panel for *record*."""

        # ── helpers ──────────────────────────────────────────────────────
        def _clear_layout(lay) -> None:
            if lay is None:
                return
            while lay.count():
                item = lay.takeAt(0)
                if item.widget():
                    item.widget().deleteLater()
                else:
                    _clear_layout(item.layout())

        _clear_layout(self._exif_layout)

        def _section(title: str) -> None:
            lbl = QLabel(title)
            lbl.setContentsMargins(0, 10, 0, 2)
            lbl.setStyleSheet(
                "color: rgba(255,109,0,0.70);"
                "font-size: 9px; font-weight: bold;"
                "letter-spacing: 2px;"
                "background: transparent;"
            )
            self._exif_layout.addWidget(lbl)

        def _divider() -> None:
            line = QFrame()
            line.setFrameShape(QFrame.HLine)
            line.setFixedHeight(1)
            line.setStyleSheet("background: rgba(255,109,0,0.10);")
            self._exif_layout.addWidget(line)

        def _form(rows: list) -> None:
            """Add a QFormLayout block — all label/value pairs share column widths."""
            form = QFormLayout()
            form.setContentsMargins(2, 1, 2, 4)
            form.setHorizontalSpacing(10)
            form.setVerticalSpacing(3)
            form.setLabelAlignment(Qt.AlignRight | Qt.AlignVCenter)
            form.setFormAlignment(Qt.AlignLeft | Qt.AlignTop)
            for label_text, value_text in rows:
                lw = QLabel(label_text)
                lw.setStyleSheet("color: #40405a; font-size: 11px;")
                vw = QLabel(value_text)
                vw.setStyleSheet("color: #b8b8d0; font-size: 11px;")
                vw.setWordWrap(True)
                form.addRow(lw, vw)
            self._exif_layout.addLayout(form)

        # ── FILE ─────────────────────────────────────────────────────────
        _section("FILE")
        file_rows = [
            ("type",     record.file_type.value),
            ("size",     _fmt_size(record.file_size)),
            ("modified", record.modified_time.strftime("%Y-%m-%d  %H:%M")),
        ]
        if record.is_paired:
            file_rows.append(("paired", "yes"))
        _form(file_rows)

        # ── EXIF sections (cached) ────────────────────────────────────────
        # Cache key includes the pair path so toggling RAW↔JPG re-reads
        # correctly (the JPG view doesn't borrow from the RAW).
        jpg_pair = pair if (pair and pair.file_type == FileType.JPG) else None
        cache_key = (record.path, jpg_pair.path if jpg_pair else None)
        if cache_key not in self._exif_cache:
            self._exif_cache[cache_key] = _read_exif_fields(record, jpg_pair)
        for section_name, rows in self._exif_cache[cache_key].items():
            _divider()
            _section(section_name)
            _form([(lbl.lower(), val) for lbl, val in rows])

        # ── PAIR ─────────────────────────────────────────────────────────
        if pair:
            _divider()
            _section("PAIR")
            if record.file_type == FileType.RAW:
                raw_r, jpg_r = record, pair
            else:
                raw_r, jpg_r = pair, record
            pr = [("raw", raw_r.filename), ("jpg", jpg_r.filename)]
            _form(pr)

        self._exif_layout.addStretch()

    def _toggle_exif_panel(self) -> None:
        visible = self._btn_info.isChecked()
        self._exif_panel.setVisible(visible)
        self._btn_info.setStyleSheet(self._info_btn_style(visible))

    @staticmethod
    def _info_btn_style(active: bool) -> str:
        if active:
            return (
                "QPushButton{background:rgba(255,109,0,0.15);color:#ff6d00;"
                "border:1px solid rgba(255,109,0,0.45);"
                "border-radius:4px;font-size:11px;font-weight:bold;}"
                "QPushButton:hover{background:rgba(255,109,0,0.25);}"
            )
        return (
            "QPushButton{background:rgba(255,109,0,0.06);color:#55556a;"
            "border:1px solid rgba(255,109,0,0.15);"
            "border-radius:4px;font-size:11px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.14);color:#c0c0d8;"
            "border-color:rgba(255,109,0,0.35);}"
        )

    # ------------------------------------------------------------------ #
    # Navigation                                                           #
    # ------------------------------------------------------------------ #

    def go_prev(self) -> None:
        if self._index > 0:
            self._pending_shot = self._view.capture_viewport()
            self._nav_direction = -1
            self._index -= 1
            self._load_current(animate=True)

    def go_next(self) -> None:
        if self._index < len(self._records) - 1:
            self._pending_shot = self._view.capture_viewport()
            self._nav_direction = 1
            self._index += 1
            self._load_current(animate=True)

    def go_first(self) -> None:
        if self._index != 0:
            self._pending_shot = self._view.capture_viewport()
            self._nav_direction = -1
            self._index = 0
            self._load_current(animate=True)

    def go_last(self) -> None:
        last = len(self._records) - 1
        if self._index != last:
            self._pending_shot = self._view.capture_viewport()
            self._nav_direction = 1
            self._index = last
            self._load_current(animate=True)

    def _toggle_pair_view(self) -> None:
        """Switch between the primary record and its pair."""
        if not self._pair_record:
            return
        self._show_pair_mode = not self._show_pair_mode
        display = self._pair_record if self._show_pair_mode else self._records[self._index]
        self._btn_pair.setChecked(self._show_pair_mode)
        self._btn_pair.setStyleSheet(self._pair_btn_style(self._show_pair_mode, True))
        # Label shows the type currently being displayed
        self._btn_pair.setText(f"{display.file_type.value} \u21c4")
        self.setWindowTitle(f"Viewer — {display.filename}")
        self._lbl_name.setText(display.filename)
        self._update_meta(display)
        cached = self._loader.get_cached(display.path)
        if cached:
            self._view.set_pixmap(cached)
            self._update_meta(display, dims=(cached.width(), cached.height()))
        else:
            self._view.set_loading()
            self._loader.request(display)

    def _load_current(self, animate: bool = True) -> None:
        # Reset pair mode when navigating to a new image
        self._show_pair_mode = False
        record = self._records[self._index]
        self._pair_record = self._pair_lookup(record)
        n = len(self._records)

        self.setWindowTitle(f"Viewer — {record.filename}")
        self._lbl_name.setText(record.filename)
        self._lbl_pos.setText(
            f"({self._index + 1}\u202f/\u202f{n})"
        )
        self._btn_prev.setEnabled(self._index > 0)
        self._btn_next.setEnabled(self._index < n - 1)

        # Update pair button — label shows the currently displayed type
        has_pair = self._pair_record is not None
        self._btn_pair.setEnabled(has_pair)
        self._btn_pair.setChecked(False)
        self._btn_pair.setText(
            f"{record.file_type.value} \u21c4" if has_pair else "\u21c4 Pair"
        )
        self._btn_pair.setStyleSheet(self._pair_btn_style(False, has_pair))

        self._sync_prune_btn(record)
        self._update_meta(record)

        cached = self._loader.get_cached(record.path)
        if cached:
            # Image is ready right now — animate if we have an old frame
            if animate and self._pending_shot:
                self._view.slide_to(
                    self._pending_shot, cached, self._nav_direction
                )
                self._pending_shot = None
            else:
                self._pending_shot = None
                self._view.set_pixmap(cached)
            self._update_meta(record, dims=(cached.width(), cached.height()))
        else:
            # Not cached yet — show loading state; animate when it arrives
            self._view.set_loading()
            self._loader.request(record)

        self._preload_adjacent()

    def _preload_adjacent(self) -> None:
        self._loader.clear_queue()
        if self._index + 1 < len(self._records):
            self._loader.preload(self._records[self._index + 1])
        if self._index - 1 >= 0:
            self._loader.preload(self._records[self._index - 1])

    # ------------------------------------------------------------------ #
    # Slots                                                                #
    # ------------------------------------------------------------------ #

    def _on_image_ready(self, path: Path, pixmap: QPixmap) -> None:
        # Determine which record we're currently expecting
        expected = (
            self._pair_record if self._show_pair_mode and self._pair_record
            else self._records[self._index]
        )
        if path != expected.path:
            return  # stale — navigated away while loading

        if self._pending_shot:
            # The animation was deferred until load completed
            self._view.slide_to(
                self._pending_shot, pixmap, self._nav_direction
            )
            self._pending_shot = None
        else:
            self._view.set_pixmap(pixmap)

        self._update_meta(
            self._records[self._index],
            dims=(pixmap.width(), pixmap.height()),
        )

    def _on_load_failed(self, path: Path, msg: str) -> None:
        self._pending_shot = None
        expected = (
            self._pair_record if self._show_pair_mode and self._pair_record
            else self._records[self._index]
        )
        if path == expected.path:
            self._view.set_error(msg)

    def _on_zoom_changed(self, scale: float) -> None:
        self._lbl_zoom.setText(f"{scale * 100:.0f}\u202f%")

    # ------------------------------------------------------------------ #
    # Metadata                                                             #
    # ------------------------------------------------------------------ #

    # ------------------------------------------------------------------ #
    # Prune                                                               #
    # ------------------------------------------------------------------ #

    def _toggle_prune(self) -> None:
        record = self._records[self._index]
        record.is_pruned = not record.is_pruned
        self._btn_prune.setChecked(record.is_pruned)
        self._apply_prune_style(record.is_pruned)
        self._prune_banner.setVisible(record.is_pruned)
        self.prune_toggled.emit(record)

    def _show_in_folder(self) -> None:
        """Open the system file manager with the current file selected."""
        import subprocess, sys
        record = self._records[self._index]
        path = record.path
        if sys.platform.startswith("win"):
            subprocess.Popen(["explorer", "/select,", str(path)])
        elif sys.platform == "darwin":
            subprocess.Popen(["open", "-R", str(path)])
        else:
            # Linux: try to select the file in the file manager; fall back to
            # opening the parent folder if the manager doesn't support --select.
            try:
                subprocess.Popen(["xdg-open", str(path.parent)])
            except Exception:
                QDesktopServices.openUrl(QUrl.fromLocalFile(str(path.parent)))

    def _unmark(self) -> None:
        record = self._records[self._index]
        if record.is_pruned:
            record.is_pruned = False
            self._btn_prune.setChecked(False)
            self._apply_prune_style(False)
            self._prune_banner.hide()
            self.prune_toggled.emit(record)

    @staticmethod
    def _pair_btn_style(active: bool, enabled: bool) -> str:
        if not enabled:
            return (
                "QPushButton{background:transparent;color:#2e2e48;"
                "border:1px solid #1e1e2e;border-radius:4px;font-size:11px;}"
            )
        if active:
            return (
                "QPushButton{background:rgba(255,109,0,0.20);color:#ff6d00;"
                "border:1px solid rgba(255,109,0,0.60);"
                "border-radius:4px;font-size:11px;font-weight:bold;}"
                "QPushButton:hover{background:rgba(255,109,0,0.30);}"
            )
        return (
            "QPushButton{background:rgba(255,109,0,0.08);color:#8888a8;"
            "border:1px solid rgba(255,109,0,0.18);border-radius:4px;font-size:11px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
            "border-color:rgba(255,109,0,0.45);}"
        )

    @staticmethod
    def _prune_style(active: bool) -> str:
        if active:
            return (
                "QPushButton{background:#7a1a1a;color:#ffaaaa;"
                "border:1px solid #cc3030;"
                "border-radius:4px;font-size:12px;font-weight:bold;}"
                "QPushButton:hover{background:#9a2020;color:#fff;}"
            )
        return (
            "QPushButton{background:rgba(255,109,0,0.08);color:#8888a8;"
            "border:1px solid rgba(255,109,0,0.18);"
            "border-radius:4px;font-size:12px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
            "border-color:rgba(255,109,0,0.45);}"
        )

    def _sync_prune_btn(self, record: PhotoRecord) -> None:
        self._btn_prune.blockSignals(True)
        self._btn_prune.setChecked(record.is_pruned)
        self._apply_prune_style(record.is_pruned)
        self._btn_prune.blockSignals(False)
        self._prune_banner.setVisible(record.is_pruned)

    def _apply_prune_style(self, active: bool) -> None:
        self._btn_prune.setStyleSheet(self._prune_style(active))
        # Re-apply icon since setStyleSheet can clear it on some platforms
        ic = _icon("trash-alt", size=12)
        if not ic.isNull():
            self._btn_prune.setIcon(ic)

    # ------------------------------------------------------------------ #
    # Metadata                                                             #
    # ------------------------------------------------------------------ #

    def _update_meta(
        self,
        record: PhotoRecord,
        dims: Optional[tuple] = None,
    ) -> None:
        parts = [record.file_type.value]
        if dims:
            parts.append(f"{dims[0]}\u202f×\u202f{dims[1]}\u202fpx")
        parts.append(_fmt_size(record.file_size))
        parts.append(record.modified_time.strftime("%Y-%m-%d  %H:%M"))
        if record.is_paired:
            parts.append("Paired")
        self._lbl_meta.setText("   \u2022   ".join(parts))

        # EXIF + pair paths (only read on first call per record — dims=None)
        if dims is None:
            pair = self._pair_lookup(record)
            self._populate_exif_panel(record, pair)
            self._lbl_exif.hide()
            self._lbl_pair_paths.hide()

    # ------------------------------------------------------------------ #
    # Keyboard                                                             #
    # ------------------------------------------------------------------ #

    def keyPressEvent(self, event) -> None:
        k = event.key()
        if   k in (Qt.Key_Left,  Qt.Key_A):          self.go_prev()
        elif k in (Qt.Key_Right, Qt.Key_D):           self.go_next()
        elif k == Qt.Key_Home:                         self.go_first()
        elif k == Qt.Key_End:                          self.go_last()
        elif k == Qt.Key_Escape:                       self.close()
        elif k in (Qt.Key_F, Qt.Key_Space):           self._view.fit_view()
        elif k == Qt.Key_1:                            self._view.zoom_actual()
        elif k in (Qt.Key_Plus, Qt.Key_Equal):        self._view.zoom_in()
        elif k == Qt.Key_Minus:                        self._view.zoom_out()
        elif k in (Qt.Key_Delete, Qt.Key_P):          self._toggle_prune()
        elif k == Qt.Key_U:                            self._unmark()
        elif k == Qt.Key_Question:                     self._show_shortcuts()
        else:
            super().keyPressEvent(event)

    def _show_shortcuts(self) -> None:
        from app.ui.shortcuts_dialog import KeyboardShortcutsDialog
        if not hasattr(self, "_shortcuts_dlg") or not self._shortcuts_dlg.isVisible():
            self._shortcuts_dlg = KeyboardShortcutsDialog(parent=self)
        self._shortcuts_dlg.show()
        self._shortcuts_dlg.raise_()

    def closeEvent(self, event) -> None:
        self._loader.shutdown()
        super().closeEvent(event)
