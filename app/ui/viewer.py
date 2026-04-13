"""
Single-image viewer (Phase 3).

_SlideOverlay   Single opaque child-widget that holds both the outgoing and
                incoming pre-rendered frames.  A float `progress` (0→1)
                drives the slide; both frames are drawn in one paintEvent,
                halving compositor work vs. the previous two-widget approach.

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

import time
from pathlib import Path
from typing import List, Optional

from PySide6.QtCore import (
    QEasingCurve,
    QEvent,
    QObject,
    QPointF,
    QRunnable,
    QSize,
    Qt,
    QThreadPool,
    QVariantAnimation,
    Signal,
)
from PySide6.QtGui import (
    QColor,
    QNativeGestureEvent,
    QPainter,
    QPixmap,
    QTransform,
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


_EXIFTOOL_TAGS = [
    "-Make", "-Model", "-ISO", "-ExposureTime", "-FNumber",
    "-FocalLength", "-FocalLengthIn35mmFormat", "-ExposureBiasValue",
    "-MeteringMode", "-Flash", "-WhiteBalance", "-ExposureProgram",
    "-ExposureMode", "-SceneCaptureType",
    "-LensID", "-LensModel", "-LensMake", "-LensSpec",
    "-Lens", "-LensType",
]


class _ExiftoolSignals(QObject):
    done: Signal = Signal(object, dict)   # (Path, partial-flat-dict)


class _ExiftoolWorker(QRunnable):
    """Run exiftool in a thread-pool worker; emit all EXIF fields when done."""

    def __init__(self, path: Path, signals: _ExiftoolSignals) -> None:
        super().__init__()
        self.path = path
        self.signals = signals
        self.setAutoDelete(True)

    def run(self) -> None:
        import subprocess, json as _json
        try:
            result = subprocess.run(
                ["exiftool", "-j", "-n"] + _EXIFTOOL_TAGS + [str(self.path)],
                capture_output=True, text=True, timeout=5,
            )
            if result.returncode != 0 or not result.stdout.strip():
                return
            data = _json.loads(result.stdout)
            if not data:
                return
            d = data[0]
            out: dict = {}

            # Camera
            make  = str(d.get("Make") or "").strip()
            model = str(d.get("Model") or "").strip()
            if make and model.upper().startswith(make.upper()):
                model = model[len(make):].strip()
            if make:
                out["Make"] = make
            if model:
                out["Model"] = model

            # Exposure
            iso = d.get("ISO")
            if iso:
                out["ISO"] = str(int(iso))
            et = d.get("ExposureTime")
            if et:
                s = float(et)
                out["Shutter"] = f"1/{round(1/s)} s" if 0 < s < 1.0 else f"{s:g} s"
            fn = d.get("FNumber")
            if fn:
                out["Aperture"] = f"f/{float(fn):g}"
            fl = d.get("FocalLength")
            if fl:
                out["Focal Length"] = f"{float(fl):g} mm"
            fl35 = d.get("FocalLengthIn35mmFormat")
            if fl35:
                out["35mm Equiv"] = f"{float(fl35):g} mm"
            ev = d.get("ExposureBiasValue")
            if ev is not None:
                out["EV Comp"] = f"{float(ev):+g} EV"

            # Lens
            lmodel = str(d.get("LensModel") or d.get("LensID") or d.get("Lens") or "").strip()
            lmake  = str(d.get("LensMake") or "").strip()
            if lmake and lmodel.upper().startswith(lmake.upper()):
                lmodel = lmodel[len(lmake):].strip()
            if lmake:
                out["Lens Make"] = lmake
            if lmodel:
                out["Lens Model"] = lmodel
            spec_raw = d.get("LensSpec") or d.get("LensType") or ""
            if spec_raw and not lmodel:
                out["Lens Model"] = str(spec_raw).strip()

            if out:
                self.signals.done.emit(self.path, out)
        except Exception:
            pass


def _exif_from_tiff(path: str) -> dict:
    """
    Read EXIF fields from a TIFF-container RAW file (ARW, CR2, NEF, DNG …)
    using a minimal IFD walker.  No pixel decode — header reads only.
    """
    import struct as _s

    # tag → (output_key, type)
    # type: 'str', 'short', 'rational', 'srational'
    _IFD0_TAGS: dict = {
        271: ("Make",  "str"),
        272: ("Model", "str"),
        305: ("Software", "str"),
    }
    _EXIF_TAGS: dict = {
        33434: ("_et",   "rational"),   # ExposureTime  (raw, formatted below)
        33437: ("_fn",   "rational"),   # FNumber
        34855: ("ISO",   "short"),      # ISOSpeedRatings
        37378: ("_av",   "rational"),   # ApertureValue (APEX)
        37380: ("_ev",   "srational"),  # ExposureBiasValue
        37383: ("_mm",   "short"),      # MeteringMode
        37385: ("_fl",   "short"),      # Flash
        37386: ("Focal Length", "rational"),  # FocalLength
        41987: ("_wb",   "short"),      # WhiteBalance
        41989: ("35mm Equiv", "short"), # FocalLengthIn35mmFilm
        41986: ("_em",   "short"),      # ExposureMode
        41985: ("_ep",   "short"),      # ExposureProgram (CustomRendered shares tag space)
        41990: ("_sc",   "short"),      # SceneCaptureType
    }
    _EXIF_IFD_TAG = 0x8769

    _METERING = {0: "Unknown", 1: "Average", 2: "Center-weighted",
                 3: "Spot", 4: "Multi-spot", 5: "Multi-segment", 6: "Partial"}
    _FLASH_MAP = {0: "Off", 1: "Fired", 5: "Fired (no return)", 7: "Fired (return)",
                  16: "Off (flash)", 24: "Off (auto)", 25: "Fired (auto)",
                  29: "Fired (auto, no return)", 31: "Fired (auto, return)",
                  32: "No flash", 65: "Fired (red-eye)", 71: "Fired (red-eye, no return)"}
    _WB = {0: "Auto", 1: "Manual"}
    _EXPMODE = {0: "Auto", 1: "Manual", 2: "Auto bracket"}
    _EXPPROG = {1: "Manual", 2: "Program", 3: "Aperture priority",
                4: "Shutter priority", 5: "Creative", 6: "Action", 7: "Portrait",
                8: "Landscape"}
    _SCENE = {0: "Standard", 1: "Landscape", 2: "Portrait", 3: "Night"}

    out: dict = {}
    try:
        with open(path, "rb") as f:
            hdr = f.read(8)
            if len(hdr) < 8:
                return out
            if hdr[:2] == b'II':
                end = '<'
            elif hdr[:2] == b'MM':
                end = '>'
            else:
                return out

            ifd0 = _s.unpack_from(end + 'I', hdr, 4)[0]

            def read_str(offset: int, count: int) -> str:
                f.seek(offset)
                return f.read(count).rstrip(b'\x00').decode('ascii', errors='replace').strip()

            def read_rational(offset: int) -> Optional[float]:
                f.seek(offset)
                buf = f.read(8)
                if len(buf) < 8:
                    return None
                num, den = _s.unpack(end + 'II', buf)
                return num / den if den else None

            def read_srational(offset: int) -> Optional[float]:
                f.seek(offset)
                buf = f.read(8)
                if len(buf) < 8:
                    return None
                num, den = _s.unpack(end + 'ii', buf)
                return num / den if den else None

            def read_short_inline(val_off: int) -> int:
                # SHORT stored inline: lower 2 bytes in little-endian, upper in big
                if end == '<':
                    return val_off & 0xFFFF
                else:
                    return (val_off >> 16) & 0xFFFF

            def walk(offset: int, tag_map: dict) -> dict:
                result: dict = {}
                f.seek(offset)
                buf = f.read(2)
                if len(buf) < 2:
                    return result
                n = _s.unpack(end + 'H', buf)[0]
                for _ in range(n):
                    e = f.read(12)
                    if len(e) < 12:
                        break
                    tag, typ, cnt, val_off = _s.unpack(end + 'HHII', e)
                    if tag not in tag_map:
                        continue
                    key, ttype = tag_map[tag]
                    if ttype == 'str':
                        if cnt > 4:
                            result[key] = read_str(val_off, cnt)
                        # cnt <= 4: value inline in val_off bytes (rare for these tags)
                    elif ttype == 'short':
                        result[key] = read_short_inline(val_off)
                    elif ttype == 'rational':
                        result[key] = read_rational(val_off)
                    elif ttype == 'srational':
                        result[key] = read_srational(val_off)
                return result

            exif_ifd_ptr: Optional[int] = None
            f.seek(ifd0)
            buf = f.read(2)
            n = _s.unpack(end + 'H', buf)[0]
            for _ in range(n):
                e = f.read(12)
                if len(e) < 12:
                    break
                tag, typ, cnt, val_off = _s.unpack(end + 'HHII', e)
                if tag == _EXIF_IFD_TAG:
                    exif_ifd_ptr = val_off
                elif tag in _IFD0_TAGS:
                    key, ttype = _IFD0_TAGS[tag]
                    if ttype == 'str' and cnt > 4:
                        out[key] = read_str(val_off, cnt)

            if exif_ifd_ptr:
                raw = walk(exif_ifd_ptr, _EXIF_TAGS)

                # Normalise Make/Model
                make  = out.get("Make", "").strip()
                model = out.get("Model", "").strip()
                if make and model.upper().startswith(make.upper()):
                    out["Model"] = model[len(make):].strip()

                # ISO
                if "_et" not in raw and "ISO" in raw:
                    out["ISO"] = str(int(raw["ISO"]))
                elif "ISO" in raw:
                    out["ISO"] = str(int(raw["ISO"]))

                # Shutter
                et = raw.get("_et")
                if et and et > 0:
                    out["Shutter"] = f"1/{round(1/et)} s" if et < 1.0 else f"{et:g} s"

                # Aperture
                fn = raw.get("_fn")
                if fn and fn > 0:
                    out["Aperture"] = f"f/{fn:g}"

                # Focal length
                fl = raw.get("Focal Length")
                if fl and fl > 0:
                    out["Focal Length"] = f"{fl:g} mm"
                fl35 = raw.get("35mm Equiv")
                if fl35 and fl35 > 0:
                    out["35mm Equiv"] = f"{int(fl35)} mm"

                # EV comp
                ev = raw.get("_ev")
                if ev is not None:
                    out["EV Comp"] = f"{ev:+g} EV"

                # Metering
                mm = raw.get("_mm")
                if mm is not None:
                    out["Metering"] = _METERING.get(mm, str(mm))

                # Flash
                fl_raw = raw.get("_fl")
                if fl_raw is not None:
                    out["Flash"] = _FLASH_MAP.get(fl_raw, f"0x{fl_raw:02x}")

                # White balance
                wb = raw.get("_wb")
                if wb is not None:
                    out["White Balance"] = _WB.get(wb, str(wb))

                # Exposure mode / program
                em = raw.get("_em")
                if em is not None:
                    out["Exp Mode"] = _EXPMODE.get(em, str(em))
                ep = raw.get("_ep")
                if ep is not None:
                    out["Exp Program"] = _EXPPROG.get(ep, str(ep))

                # Scene
                sc = raw.get("_sc")
                if sc is not None:
                    out["Scene"] = _SCENE.get(sc, str(sc))

    except Exception:
        pass

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

    if record.file_type == FileType.RAW:
        try:
            rp = _exif_from_tiff(str(record.path))
            for k, v in rp.items():
                flat.setdefault(k, v)   # TIFF walker fills gaps Pillow couldn't cover
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


class _ExifReadSignals(QObject):
    done: Signal = Signal(object, object, dict)  # (record_path, pair_path_or_None, sections)


class _ExifReadWorker(QRunnable):
    """Read EXIF for one image in a thread-pool worker."""

    def __init__(
        self,
        record: PhotoRecord,
        pair_record: Optional[PhotoRecord],
        signals: _ExifReadSignals,
    ) -> None:
        super().__init__()
        self.record      = record
        self.pair_record = pair_record
        self.signals     = signals
        self.setAutoDelete(True)

    def run(self) -> None:
        try:
            sections = _read_exif_fields(self.record, self.pair_record)
            pair_path = self.pair_record.path if self.pair_record else None
            self.signals.done.emit(self.record.path, pair_path, sections)
        except Exception:
            pass


# Animation tuning
_ANIM_MS          = 320
_ANIM_EASE        = QEasingCurve(QEasingCurve.OutCubic)
_ZOOM_EASE        = QEasingCurve(QEasingCurve.OutCubic)
_DRAG_COMMIT_EASE = QEasingCurve(QEasingCurve.OutCubic)
_DRAG_CANCEL_EASE = QEasingCurve(QEasingCurve.OutCubic)



def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


# ──────────────────────────────────────────────────────────────────────────────
# Slide overlay (both animation frames in a single widget)
# ──────────────────────────────────────────────────────────────────────────────

class _SlideOverlay(QWidget):
    """
    Single opaque child-widget that draws both the outgoing and incoming
    frames in one paintEvent.  A float `progress` (0.0→1.0) controls how
    far the frames have slid; `direction` is +1 (forward) or -1 (backward).

    Using one widget instead of two halves the per-frame compositing work
    compared to the previous two-`_Overlay` approach.
    """

    def __init__(
        self,
        old_shot: QPixmap,
        new_pixmap: QPixmap,
        direction: int,
        parent: QWidget,
    ) -> None:
        super().__init__(parent)
        self._old       = old_shot
        self._new_raw   = new_pixmap   # full-res; scaled on-demand in paintEvent
        self._new_fit:  Optional[QPixmap] = None  # lazily pre-rendered at correct size
        self._direction = direction
        self._progress: float = 0.0
        self.setAttribute(Qt.WA_OpaquePaintEvent)
        self.setAttribute(Qt.WA_NoSystemBackground)

    def set_progress(self, p: float) -> None:
        self._progress = p
        self.update()

    def paintEvent(self, _event) -> None:
        w = self.width()
        h = self.height()
        p = QPainter(self)

        # Lazily build the fit-scaled new frame the first time we paint.
        # This defers the SmoothTransformation cost to the first animation
        # frame instead of blocking the main thread before the anim starts.
        if self._new_fit is None:
            if self._new_raw.isNull():
                self._new_fit = QPixmap(QSize(w, h))
                self._new_fit.fill(QColor(0x0a, 0x0a, 0x12))
            else:
                size = QSize(w, h)
                result = QPixmap(size)
                result.fill(QColor(0x0a, 0x0a, 0x12))
                scaled = self._new_raw.scaled(
                    size, Qt.KeepAspectRatio, Qt.SmoothTransformation
                )
                pp = QPainter(result)
                pp.drawPixmap(
                    (w - scaled.width())  // 2,
                    (h - scaled.height()) // 2,
                    scaled,
                )
                pp.end()
                self._new_fit = result

        # old frame slides out
        p.drawPixmap(int(-w * self._progress * self._direction), 0, self._old)
        # new frame slides in from the opposite side
        p.drawPixmap(int(w * (1.0 - self._progress) * self._direction), 0, self._new_fit)
        p.end()


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

    zoom_changed:      Signal = Signal(float)        # current scale factor (1.0 = 100 %)
    drag_nav_begin:    Signal = Signal(int)          # direction: +1 fwd, -1 bwd
    drag_nav_progress: Signal = Signal(float)        # 0.0 → 1.0 as finger moves
    drag_nav_end:      Signal = Signal(bool, float)  # (committed, velocity px/s)

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
        self._anim: Optional[QVariantAnimation] = None
        self._anim_overlay: Optional[_SlideOverlay] = None

        self._fit_mode = True

        self.setTransformationAnchor(QGraphicsView.AnchorUnderMouse)
        self.setResizeAnchor(QGraphicsView.AnchorViewCenter)
        self.setRenderHints(QPainter.SmoothPixmapTransform | QPainter.Antialiasing)
        self.setDragMode(QGraphicsView.ScrollHandDrag)
        self.setBackgroundBrush(QColor(0x0a, 0x0a, 0x12))
        self.setFrameShape(QFrame.NoFrame)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        self.setVerticalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        # Full repaint each frame during overlay animations — prevents
        # partial-update artifacts when the slide overlays move.
        self.setViewportUpdateMode(QGraphicsView.FullViewportUpdate)
        self.setOptimizationFlag(QGraphicsView.DontAdjustForAntialiasing, True)
        # Never accept keyboard focus — key events must reach ImageViewer instead.
        self.setFocusPolicy(Qt.NoFocus)

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

        # Zoom animation (double-tap / animated fit↔actual)
        self._zoom_anim: Optional[QVariantAnimation] = None

        # WA_AcceptTouchEvents on the viewport lets raw touch points reach Qt's
        # gesture recogniser.  grabGesture only on the view itself — calling it
        # on the viewport too would consume the Gesture event there before it
        # could bubble up to _ImageView.event().
        self.viewport().setAttribute(Qt.WA_AcceptTouchEvents, True)
        self.grabGesture(Qt.PinchGesture)
        self._pinch_start_scale: float = 1.0
        # Live-drag gesture state
        self._drag_origin: Optional[QPointF] = None
        self._drag_px:     float = 0.0   # latest horizontal offset from origin
        self._drag_t0:     float = 0.0   # wall-clock time of press
        self._drag_active: bool  = False  # True once direction is committed
        self._drag_dir:    int   = 0      # +1 or -1
        # Overlay control state (set by begin_drag / cleared by commit/cancel)
        self._drag_progress:    float            = 0.0
        self._drag_orig_pixmap: Optional[QPixmap] = None

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

        A single _SlideOverlay draws both frames in one paintEvent so Qt
        only composites one child widget per frame instead of two.
        Any in-flight animation is cancelled first for responsive rapid nav.
        """
        self._cancel_anim()

        # Plant the new image in the underlying view (hidden behind the overlay)
        self._status.hide()
        self._item.setPixmap(new_pixmap)
        self._scene.setSceneRect(self._item.boundingRect())
        self._fit_mode = True
        self._fit_now()

        vp   = self.viewport()
        w, h = vp.width(), vp.height()

        # Pass the raw pixmap — _SlideOverlay scales it lazily on the first
        # paint tick, so the animation starts immediately with no pre-scale cost.
        ov = _SlideOverlay(old_shot, new_pixmap, direction, vp)
        ov.setGeometry(0, 0, w, h)
        ov.show()
        ov.raise_()
        self._anim_overlay = ov

        # Animate a float 0→1 progress value
        anim = QVariantAnimation(self)
        anim.setDuration(_ANIM_MS)
        anim.setEasingCurve(_ANIM_EASE)
        anim.setStartValue(0.0)
        anim.setEndValue(1.0)
        anim.valueChanged.connect(ov.set_progress)

        def _on_done() -> None:
            self._cancel_anim()
            self.setViewportUpdateMode(QGraphicsView.MinimalViewportUpdate)

        anim.finished.connect(_on_done)
        self.setViewportUpdateMode(QGraphicsView.FullViewportUpdate)
        anim.start()
        self._anim = anim

    # ------------------------------------------------------------------ #
    # Zoom API                                                             #
    # ------------------------------------------------------------------ #

    def fit_view(self, animate: bool = True) -> None:
        if animate and not self._item.pixmap().isNull():
            self._zoom_to(self._fit_scale())
        else:
            self._fit_mode = True
            self._fit_now()

    def zoom_actual(self, animate: bool = True) -> None:
        if animate and not self._item.pixmap().isNull():
            self._zoom_to(1.0)
        else:
            self._fit_mode = False
            self.resetTransform()
            self.scale(1.0, 1.0)
            self.zoom_changed.emit(1.0)

    def zoom_in(self)  -> None: self._scale_by(self._STEP)
    def zoom_out(self) -> None: self._scale_by(1 / self._STEP)

    # ------------------------------------------------------------------ #
    # Touch events                                                        #
    # ------------------------------------------------------------------ #

    def event(self, ev) -> bool:
        # ── Touchscreen pinch gesture ────────────────────────────────────
        if ev.type() == QEvent.Gesture:
            pinch = ev.gesture(Qt.PinchGesture)
            if pinch:
                if pinch.state() == Qt.GestureStarted:
                    self._pinch_start_scale = self.transform().m11()
                    if self._zoom_anim:
                        self._zoom_anim.stop()
                        self._zoom_anim = None
                new_scale = max(self._MIN_SCALE, min(self._MAX_SCALE,
                    self._pinch_start_scale * pinch.totalScaleFactor()))
                self.resetTransform()
                self.scale(new_scale, new_scale)
                self._fit_mode = False
                self.zoom_changed.emit(new_scale)
                ev.accept()
                return True

        # ── Trackpad pinch (NativeGesture / ZoomNativeGesture) ────────────
        if ev.type() == QEvent.NativeGesture:
            if isinstance(ev, QNativeGestureEvent):
                if ev.gestureType() == Qt.NativeGestureType.ZoomNativeGesture:
                    if self._zoom_anim:
                        self._zoom_anim.stop()
                        self._zoom_anim = None
                    factor = 1.0 + ev.value()
                    cur    = self.transform().m11()
                    new_scale = max(self._MIN_SCALE,
                                    min(self._MAX_SCALE, cur * factor))
                    anchor = QPointF(ev.position())
                    scene_anchor = self.mapToScene(anchor.toPoint())
                    t = QTransform()
                    t.scale(new_scale, new_scale)
                    self.setTransform(t)
                    new_vp = self.mapFromScene(scene_anchor)
                    delta  = anchor - QPointF(new_vp)
                    self.horizontalScrollBar().setValue(
                        self.horizontalScrollBar().value() - int(delta.x()))
                    self.verticalScrollBar().setValue(
                        self.verticalScrollBar().value() - int(delta.y()))
                    self._fit_mode = False
                    self.zoom_changed.emit(new_scale)
                    ev.accept()
                    return True

        return super().event(ev)

    # ------------------------------------------------------------------ #
    # Keyboard / mouse events                                             #
    # ------------------------------------------------------------------ #

    def mousePressEvent(self, ev) -> None:
        if (ev.button() == Qt.LeftButton
                and ev.source() != Qt.MouseEventNotSynthesized
                and self._fit_mode
                and self._anim is None):
            self._drag_origin = ev.position()
            self._drag_px     = 0.0
            self._drag_t0     = time.perf_counter()
            self._drag_active = False
            self._drag_dir    = 0
        super().mousePressEvent(ev)

    def mouseMoveEvent(self, ev) -> None:
        if (self._drag_origin is not None
                and ev.source() != Qt.MouseEventNotSynthesized
                and self._fit_mode):
            dx = ev.position().x() - self._drag_origin.x()
            dy = ev.position().y() - self._drag_origin.y()
            if not self._drag_active:
                # Commit to a direction once the finger has moved clearly sideways
                if abs(dx) > 8 and abs(dx) > abs(dy) * 1.5:
                    self._drag_dir    = 1 if dx < 0 else -1
                    self._drag_active = True
                    self.drag_nav_begin.emit(self._drag_dir)
            if self._drag_active:
                self._drag_px = dx
                progress = min(1.0, abs(dx) / max(1, self.viewport().width()))
                self.drag_nav_progress.emit(progress)
                return   # suppress ScrollHandDrag panning during live drag
        super().mouseMoveEvent(ev)

    def mouseReleaseEvent(self, ev) -> None:
        if ev.button() == Qt.LeftButton and self._drag_active:
            dt       = max(time.perf_counter() - self._drag_t0, 0.001)
            velocity = abs(self._drag_px) / dt          # px / s
            committed = (
                abs(self._drag_px) > self.viewport().width() * 0.35
                or velocity > 500
            )
            self.drag_nav_end.emit(committed, velocity)
        self._drag_origin = None
        self._drag_active = False
        self._drag_dir    = 0
        self._drag_px     = 0.0
        super().mouseReleaseEvent(ev)

    # ------------------------------------------------------------------ #
    # Live-drag overlay API (called by ImageViewer)                       #
    # ------------------------------------------------------------------ #

    def begin_drag(self, old_shot: QPixmap, new_pixmap: QPixmap, direction: int) -> None:
        """Plant new_pixmap behind a drag overlay that starts at progress=0."""
        self._cancel_anim()
        self._drag_orig_pixmap = self._item.pixmap()

        self._status.hide()
        self._item.setPixmap(new_pixmap)
        self._scene.setSceneRect(self._item.boundingRect())
        self._fit_mode = True
        self._fit_now()

        vp   = self.viewport()
        w, h = vp.width(), vp.height()

        ov = _SlideOverlay(old_shot, new_pixmap, direction, vp)
        ov.setGeometry(0, 0, w, h)
        ov.set_progress(0.0)
        ov.show()
        ov.raise_()
        self._anim_overlay = ov
        self._drag_progress = 0.0
        self.setViewportUpdateMode(QGraphicsView.FullViewportUpdate)

    def update_drag(self, progress: float) -> None:
        """Move overlay to match finger position (0.0 = origin, 1.0 = fully across)."""
        if self._anim_overlay is not None:
            self._drag_progress = progress
            self._anim_overlay.set_progress(progress)

    def commit_drag(self) -> None:
        """Animate the remaining distance to snap the new image into place."""
        ov = self._anim_overlay
        if ov is None:
            return
        start    = self._drag_progress
        duration = max(60, int(220 * (1.0 - start)))
        anim = QVariantAnimation(self)
        anim.setDuration(duration)
        anim.setEasingCurve(_DRAG_COMMIT_EASE)
        anim.setStartValue(start)
        anim.setEndValue(1.0)
        anim.valueChanged.connect(ov.set_progress)
        def _done() -> None:
            self._cancel_anim()
            self._drag_orig_pixmap = None
            self.setViewportUpdateMode(QGraphicsView.MinimalViewportUpdate)
        anim.finished.connect(_done)
        anim.start()
        self._anim = anim

    def cancel_drag(self) -> None:
        """Snap back: animate to progress=0 and restore the original image."""
        ov   = self._anim_overlay
        orig = self._drag_orig_pixmap
        if ov is None:
            return
        start    = self._drag_progress
        duration = max(80, int(280 * start))
        anim = QVariantAnimation(self)
        anim.setDuration(duration)
        anim.setEasingCurve(_DRAG_CANCEL_EASE)
        anim.setStartValue(start)
        anim.setEndValue(0.0)
        anim.valueChanged.connect(ov.set_progress)
        def _done() -> None:
            self._cancel_anim()
            if orig is not None and not orig.isNull():
                self._item.setPixmap(orig)
                self._scene.setSceneRect(self._item.boundingRect())
                self._fit_mode = True
                self._fit_now()
            self._drag_orig_pixmap = None
            self.setViewportUpdateMode(QGraphicsView.MinimalViewportUpdate)
        anim.finished.connect(_done)
        anim.start()
        self._anim = anim

    def keyPressEvent(self, event) -> None:
        # Arrow keys (and all other viewer shortcuts) must reach ImageViewer.
        # QGraphicsView would otherwise consume arrows for scrolling.
        event.ignore()

    def wheelEvent(self, event) -> None:
        # At fit-to-window or with Ctrl held: wheel zooms.
        # When already zoomed in without Ctrl: wheel scrolls (pan).
        if self._fit_mode or (event.modifiers() & Qt.ControlModifier):
            if self._zoom_anim:
                self._zoom_anim.stop()
                self._zoom_anim = None
            # Use pixelDelta for trackpad smooth scroll; fall back to angleDelta.
            py = event.pixelDelta().y()
            ay = event.angleDelta().y()
            if py != 0:
                # Continuous trackpad scroll — each pixel ≈ 0.2% zoom
                factor = 1.0 + py * 0.002
            elif ay != 0:
                factor = self._STEP if ay > 0 else 1.0 / self._STEP
            else:
                return
            cur = self.transform().m11()
            new_scale = max(self._MIN_SCALE, min(self._MAX_SCALE, cur * factor))
            anchor = QPointF(event.position())
            scene_anchor = self.mapToScene(anchor.toPoint())
            t = QTransform()
            t.scale(new_scale, new_scale)
            self.setTransform(t)
            new_vp = self.mapFromScene(scene_anchor)
            delta = anchor - QPointF(new_vp)
            self.horizontalScrollBar().setValue(
                self.horizontalScrollBar().value() - int(delta.x()))
            self.verticalScrollBar().setValue(
                self.verticalScrollBar().value() - int(delta.y()))
            fit = self._fit_scale()
            self._fit_mode = abs(new_scale - fit) < 1e-4
            self.zoom_changed.emit(new_scale)
            event.accept()
        else:
            super().wheelEvent(event)

    def mouseDoubleClickEvent(self, event) -> None:
        anchor = QPointF(event.position())
        if self._fit_mode:
            # Zoom in to 100 % anchored under the tap point
            self._zoom_to(1.0, anchor)
        else:
            # Zoom back to fit, always centred
            self._zoom_to(self._fit_scale())

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

    def _fit_scale(self) -> float:
        """Return what m11 would be after a fitInView without actually applying it."""
        if self._item.pixmap().isNull():
            return 1.0
        vp = self.viewport().size()
        pm = self._item.pixmap().size()
        sx = vp.width()  / pm.width()
        sy = vp.height() / pm.height()
        return min(sx, sy)

    def _scale_by(self, factor: float) -> None:
        cur = self.transform().m11()
        new = max(self._MIN_SCALE, min(self._MAX_SCALE, cur * factor))
        self.resetTransform()
        self.scale(new, new)
        self._fit_mode = False
        self.zoom_changed.emit(new)

    def _zoom_to(self, target: float, anchor: Optional[QPointF] = None) -> None:
        """
        Smoothly animate from the current scale to *target*.
        *anchor* is a viewport-space point to zoom towards (defaults to centre).
        Sets fit_mode=False unless target matches the fit scale.
        """
        if self._zoom_anim is not None:
            self._zoom_anim.stop()
            self._zoom_anim.deleteLater()
            self._zoom_anim = None

        start = self.transform().m11()
        if abs(start - target) < 0.001:
            return

        # Scene point that should stay fixed under the anchor
        if anchor is None:
            vp = self.viewport()
            anchor = QPointF(vp.width() / 2, vp.height() / 2)
        scene_anchor = self.mapToScene(anchor.toPoint())

        anim = QVariantAnimation(self)
        anim.setDuration(260)
        anim.setEasingCurve(_ZOOM_EASE)
        anim.setStartValue(start)
        anim.setEndValue(target)

        def _tick(v: float) -> None:
            t = QTransform()
            t.scale(v, v)
            self.setTransform(t)
            # Re-centre on the anchor point
            new_vp = self.mapFromScene(scene_anchor)
            delta  = anchor - QPointF(new_vp)
            sb_h   = self.horizontalScrollBar()
            sb_v   = self.verticalScrollBar()
            sb_h.setValue(sb_h.value() - int(delta.x()))
            sb_v.setValue(sb_v.value() - int(delta.y()))
            self.zoom_changed.emit(v)

        anim.valueChanged.connect(_tick)

        fit = self._fit_scale()

        def _done() -> None:
            self._zoom_anim = None
            self._fit_mode  = abs(target - fit) < 0.005
            if self._fit_mode:
                self._fit_now()   # let fitInView set exact transform

        anim.finished.connect(_done)
        self._fit_mode = False
        anim.start()
        self._zoom_anim = anim

    def _show_status(self, text: str) -> None:
        self._status.setText(text)
        self._status.setGeometry(self.rect())
        self._status.show()
        self._status.raise_()

    def _cancel_anim(self) -> None:
        """Stop in-flight animation and clean up the overlay widget."""
        if self._anim is not None:
            self._anim.stop()
            self._anim.deleteLater()
            self._anim = None
        if self._anim_overlay is not None:
            self._anim_overlay.deleteLater()
            self._anim_overlay = None



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
        self.setFocusPolicy(Qt.StrongFocus)

        screen = QApplication.primaryScreen().availableGeometry()
        w = int(screen.width()  * 0.58)
        h = int(screen.height() * 0.64)
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

        # Async EXIF reader (Pillow + rawpy — replaces blocking call)
        self._exif_pool = QThreadPool()
        self._exif_pool.setMaxThreadCount(1)
        self._exif_read_signals = _ExifReadSignals()
        self._exif_read_signals.done.connect(self._on_exif_ready)
        self._exif_pending: Optional[tuple] = None   # (record.path, pair_path|None)

        # Async exiftool for lens metadata (Makernote fallback)
        self._et_pool = QThreadPool()
        self._et_pool.setMaxThreadCount(1)
        self._et_signals = _ExiftoolSignals()
        self._et_signals.done.connect(self._on_lens_ready)
        self._et_pending: Optional[Path] = None   # path currently being queried

        # Pending transition state (button / keyboard navigation)
        self._pending_shot: Optional[QPixmap] = None
        self._nav_direction: int = 1

        # Live-drag state (touch swipe)
        self._drag_target_index: Optional[int] = None
        self._drag_committing:   bool           = False

        self._build_ui()
        self._loader.image_ready.connect(self._on_image_ready)
        self._loader.load_failed.connect(self._on_load_failed)
        self._load_current(animate=False)   # first open: no animation
        self._preload_adjacent()            # warm cache for neighbours immediately

    def showEvent(self, event) -> None:
        super().showEvent(event)
        self.setFocus()

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

        # Build bottom bar first so self._btn_zoom* / _btn_info are real
        # buttons before _build_controls references them as placeholders.
        bottom_bar = self._build_bottom_bar()

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

        root.addWidget(bottom_bar)

        self._btn_prev.clicked.connect(self.go_prev)
        self._btn_next.clicked.connect(self.go_next)
        self._btn_pair.clicked.connect(self._toggle_pair_view)
        self._btn_fit.clicked.connect(self._view.fit_view)
        self._btn_actual.clicked.connect(self._view.zoom_actual)
        self._btn_zoomin.clicked.connect(self._view.zoom_in)
        self._btn_zoomout.clicked.connect(self._view.zoom_out)
        self._view.zoom_changed.connect(self._on_zoom_changed)
        self._view.drag_nav_begin.connect(self._on_drag_begin)
        self._view.drag_nav_progress.connect(self._on_drag_progress)
        self._view.drag_nav_end.connect(self._on_drag_end)
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

    def _build_bottom_bar(self) -> QWidget:
        """
        Bottom strip: metadata text on the left, zoom+info controls on the right.
        """
        bar = QWidget()
        bar.setStyleSheet(
            "background:#0e0e1a;border-top:1px solid rgba(255,109,0,0.08);"
        )
        outer_lay = QVBoxLayout(bar)
        outer_lay.setContentsMargins(0, 0, 0, 0)
        outer_lay.setSpacing(0)

        # ── control row ────────────────────────────────────────────────── #
        ctrl = QWidget()
        ctrl.setStyleSheet(
            "background:#0e0e1a;border-bottom:1px solid rgba(255,109,0,0.06);"
        )
        ctrl_lay = QHBoxLayout(ctrl)
        ctrl_lay.setContentsMargins(8, 4, 8, 4)
        ctrl_lay.setSpacing(4)

        _btn_qss = (
            "QPushButton{background:rgba(255,109,0,0.08);color:#8888a8;"
            "border:1px solid rgba(255,109,0,0.18);border-radius:4px;"
            "font-size:12px;min-width:28px;padding:1px 6px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.18);color:#f0f0f0;"
            "border-color:rgba(255,109,0,0.45);}"
            "QPushButton:checked{background:rgba(255,109,0,0.22);color:#ff6d00;"
            "border-color:rgba(255,109,0,0.55);}"
        )

        # Re-create the real zoom / info buttons here (replacing placeholders)
        self._btn_zoomout = QPushButton("−")
        self._btn_zoomout.setFixedSize(30, 24)
        self._btn_zoomout.setStyleSheet(_btn_qss)

        self._lbl_zoom = QLabel("—")
        self._lbl_zoom.setStyleSheet(
            "color:#7878a0;font-size:11px;min-width:48px;"
        )
        self._lbl_zoom.setAlignment(Qt.AlignCenter)

        self._btn_zoomin = QPushButton("+")
        self._btn_zoomin.setFixedSize(30, 24)
        self._btn_zoomin.setStyleSheet(_btn_qss)

        self._btn_fit = QPushButton("Fit")
        self._btn_fit.setFixedSize(36, 24)
        ic_fit = _icon("expand", size=12)
        if not ic_fit.isNull():
            self._btn_fit.setIcon(ic_fit)
            self._btn_fit.setText("")
        self._btn_fit.setStyleSheet(_btn_qss)

        self._btn_actual = QPushButton("1:1")
        self._btn_actual.setFixedSize(36, 24)
        self._btn_actual.setStyleSheet(_btn_qss)

        self._btn_info = QPushButton("Info")
        self._btn_info.setFixedSize(42, 24)
        self._btn_info.setCheckable(True)
        self._btn_info.setChecked(True)
        self._btn_info.setStyleSheet(_btn_qss)

        ctrl_lay.addWidget(self._btn_zoomout)
        ctrl_lay.addWidget(self._lbl_zoom)
        ctrl_lay.addWidget(self._btn_zoomin)
        ctrl_lay.addSpacing(6)
        ctrl_lay.addWidget(self._btn_fit)
        ctrl_lay.addWidget(self._btn_actual)
        ctrl_lay.addStretch()
        ctrl_lay.addWidget(self._btn_info)

        outer_lay.addWidget(ctrl)

        # ── metadata text rows ─────────────────────────────────────────── #
        meta = QWidget()
        from PySide6.QtWidgets import QVBoxLayout as _QVBox
        meta_lay = _QVBox(meta)
        meta_lay.setContentsMargins(12, 3, 12, 3)
        meta_lay.setSpacing(1)

        self._lbl_meta = QLabel("—")
        self._lbl_meta.setStyleSheet("color:#44445a;font-size:11px;")

        self._lbl_exif = QLabel("")
        self._lbl_exif.setStyleSheet("color:#5a5a7a;font-size:11px;")
        self._lbl_exif.hide()

        self._lbl_pair_paths = QLabel("")
        self._lbl_pair_paths.setStyleSheet("color:#5a5a7a;font-size:10px;")
        self._lbl_pair_paths.hide()

        meta_lay.addWidget(self._lbl_meta)
        meta_lay.addWidget(self._lbl_exif)
        meta_lay.addWidget(self._lbl_pair_paths)
        outer_lay.addWidget(meta)

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

        # ── FILE — shown immediately (no I/O required) ────────────────────
        _section("FILE")
        file_rows = [
            ("type",     record.file_type.value),
            ("size",     _fmt_size(record.file_size)),
            ("modified", record.modified_time.strftime("%Y-%m-%d  %H:%M")),
        ]
        if record.is_paired:
            file_rows.append(("paired", "yes"))
        _form(file_rows)

        # ── PAIR ─────────────────────────────────────────────────────────
        if pair:
            _divider()
            _section("PAIR")
            if record.file_type == FileType.RAW:
                raw_r, jpg_r = record, pair
            else:
                raw_r, jpg_r = pair, record
            _form([("raw", raw_r.filename), ("jpg", jpg_r.filename)])

        self._exif_layout.addStretch()

        # ── EXIF sections — served from cache or fetched async ────────────
        jpg_pair  = pair if (pair and pair.file_type == FileType.JPG) else None
        cache_key = (record.path, jpg_pair.path if jpg_pair else None)
        if cache_key in self._exif_cache:
            # Cache hit — inject synchronously (zero I/O)
            self._inject_exif_sections(self._exif_cache[cache_key], record, jpg_pair)
        else:
            # Cache miss — dispatch to worker thread; panel shows file info now
            self._exif_pending = cache_key
            self._exif_pool.clear()
            self._exif_pool.start(
                _ExifReadWorker(record, jpg_pair, self._exif_read_signals)
            )

    def _inject_exif_sections(
        self,
        sections: dict,
        record: PhotoRecord,
        jpg_pair: Optional[PhotoRecord],
    ) -> None:
        """
        Insert EXIF section widgets before the trailing stretch.
        Also kick off async exiftool if no lens info came through.
        """
        # Helpers that write into self._exif_layout
        def _divider() -> None:
            line = QFrame()
            line.setFrameShape(QFrame.HLine)
            line.setFixedHeight(1)
            line.setStyleSheet("background: rgba(255,109,0,0.10);")
            self._exif_layout.insertWidget(self._exif_layout.count() - 1, line)

        def _section_hdr(title: str) -> None:
            hdr = QLabel(title)
            hdr.setContentsMargins(0, 10, 0, 2)
            hdr.setStyleSheet(
                "color: rgba(255,109,0,0.70); font-size: 9px; font-weight: bold;"
                "letter-spacing: 2px; background: transparent;"
            )
            self._exif_layout.insertWidget(self._exif_layout.count() - 1, hdr)

        def _form(rows: list) -> None:
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
            self._exif_layout.insertLayout(self._exif_layout.count() - 1, form)

        for section_name, rows in sections.items():
            _divider()
            _section_hdr(section_name)
            _form([(lbl.lower(), val) for lbl, val in rows])

        # Kick off exiftool async for RAW files (fills lens + any gaps the TIFF
        # walker missed), or if lens data is absent for any file type.
        _LENS_KEYS = {"Lens Make", "Lens Model", "Lens Spec"}
        has_lens = "LENS" in sections or any(
            any(lbl in _LENS_KEYS for lbl, _ in rows)
            for rows in sections.values()
        )
        need_exiftool = (record.file_type == FileType.RAW) or (not has_lens)
        if need_exiftool:
            self._et_pending = record.path
            self._et_pool.clear()
            self._et_pool.start(_ExiftoolWorker(record.path, self._et_signals))

    def _on_exif_ready(
        self,
        record_path: Path,
        pair_path: Optional[Path],
        sections: dict,
    ) -> None:
        """Worker finished reading EXIF — cache it and inject into the live panel."""
        cache_key = (record_path, pair_path)
        if cache_key != self._exif_pending:
            # Navigated away — still cache for future use
            self._exif_cache[cache_key] = sections
            return
        self._exif_pending = None
        self._exif_cache[cache_key] = sections

        # Find matching record/pair from the live viewer state
        record = self._records[self._index] if self._records else None
        if record is None or record.path != record_path:
            return
        jpg_pair = self._pair_record if (
            self._pair_record and self._pair_record.file_type == FileType.JPG
        ) else None
        self._inject_exif_sections(sections, record, jpg_pair)

    def _on_lens_ready(self, path: Path, et_flat: dict) -> None:
        """Receive async exiftool result; merge all fields and re-populate the panel."""
        if path != self._et_pending:
            return  # navigated away before exiftool finished
        self._et_pending = None

        if not et_flat:
            return

        # Determine the cache key for the current display state
        jpg_pair = self._pair_record if (
            self._pair_record and self._pair_record.file_type == FileType.JPG
        ) else None
        cache_key = (path, jpg_pair.path if jpg_pair else None)

        # Flatten the existing cached sections back to a key→value dict,
        # then fill any missing fields from exiftool results.
        existing_sections = self._exif_cache.get(cache_key, {})
        existing_flat: dict = {
            lbl: val
            for rows in existing_sections.values()
            for lbl, val in rows
        }
        merged_flat = dict(existing_flat)
        for k, v in et_flat.items():
            merged_flat.setdefault(k, v)

        # Rebuild sections from the merged flat dict (same logic as _read_exif_fields)
        _CAMERA_KEYS   = ["Make", "Model", "Software"]
        _EXPOSURE_KEYS = ["Date", "ISO", "Aperture", "Shutter", "EV Comp",
                          "Exp Program", "Exp Mode", "Metering", "Flash",
                          "White Balance", "Scene", "Subject Dist"]
        _LENS_KEYS     = ["Lens Make", "Lens Model", "Lens Spec",
                          "Focal Length", "35mm Equiv"]
        _GEO_KEYS      = ["GPS"]

        new_sections: dict = {}
        for name, keys in [
            ("CAMERA",   _CAMERA_KEYS),
            ("EXPOSURE", _EXPOSURE_KEYS),
            ("LENS",     _LENS_KEYS),
            ("GPS",      _GEO_KEYS),
        ]:
            rows = [(k, merged_flat[k]) for k in keys if k in merged_flat]
            if rows:
                new_sections[name] = rows

        if not new_sections:
            return

        # Update cache with the enriched result
        self._exif_cache[cache_key] = new_sections

        # Re-populate the panel for the current record (cache hit — instant)
        record = self._records[self._index] if self._records else None
        if record is None or record.path != path:
            return
        self._populate_exif_panel(record, self._pair_record)

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
        self._loader.preload_range(self._records, self._index, radius=2)

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

        # A live-drag commit animation is playing — let it finish.  The view
        # already has the correct pixmap; applying it again would cancel the
        # snap-to-complete animation mid-flight.
        if self._drag_committing:
            return

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
    # Live-drag swipe navigation                                          #
    # ------------------------------------------------------------------ #

    def _on_drag_begin(self, direction: int) -> None:
        """_ImageView detected a horizontal drag — set up the live overlay."""
        target = self._index + direction
        if target < 0 or target >= len(self._records):
            self._drag_target_index = None
            return

        target_record = self._records[target]
        old_shot      = self._view.capture_viewport()

        # Use cached pixmap if pre-loaded; otherwise fill with background and
        # kick off a load so it arrives as soon as possible.
        cached = self._loader.get_cached(target_record.path)
        if cached is None:
            vp     = self._view.viewport()
            cached = QPixmap(vp.width(), vp.height())
            cached.fill(QColor(0x0a, 0x0a, 0x12))
            self._loader.request(target_record)

        self._drag_target_index = target
        self._drag_committing   = False
        self._view.begin_drag(old_shot, cached, direction)

    def _on_drag_progress(self, progress: float) -> None:
        if self._drag_target_index is not None:
            self._view.update_drag(progress)

    def _on_drag_end(self, committed: bool, _velocity: float) -> None:
        idx = self._drag_target_index
        if idx is None:
            return

        if committed:
            self._drag_committing = True
            self._view.commit_drag()

            # Advance navigation state immediately so the UI is consistent
            self._index         = idx
            self._show_pair_mode = False
            self._pending_shot   = None
            record               = self._records[self._index]
            self._pair_record    = self._pair_lookup(record)
            n = len(self._records)
            self.setWindowTitle(f"Viewer — {record.filename}")
            self._lbl_name.setText(record.filename)
            self._lbl_pos.setText(f"({self._index + 1}\u202f/\u202f{n})")
            self._btn_prev.setEnabled(self._index > 0)
            self._btn_next.setEnabled(self._index < n - 1)
            self._sync_prune_btn(record)
            self._update_meta(record)
            self._preload_adjacent()

            # Clear the in-progress flag after the snap animation finishes
            from PySide6.QtCore import QTimer
            QTimer.singleShot(250, lambda: setattr(self, '_drag_committing', False))
        else:
            self._view.cancel_drag()

        self._drag_target_index = None

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
            return

        if sys.platform == "darwin":
            subprocess.Popen(["open", "-R", str(path)])
            return

        # Linux: use the org.freedesktop.FileManager1 DBus interface —
        # Dolphin, Nautilus, Nemo, and Thunar all implement it and will
        # open with the specific file selected/highlighted.
        # Avoids Qt's portal integration entirely (which fails for unregistered apps).
        file_uri = path.as_uri()
        try:
            result = subprocess.run(
                [
                    "dbus-send", "--session", "--print-reply",
                    "--dest=org.freedesktop.FileManager1",
                    "/org/freedesktop/FileManager1",
                    "org.freedesktop.FileManager1.ShowItems",
                    f"array:string:{file_uri}",
                    "string:",
                ],
                timeout=3,
                capture_output=True,
            )
            if result.returncode == 0:
                return
        except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
            pass

        # Fallback: just open the containing folder
        subprocess.Popen(["xdg-open", str(path.parent)])

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
