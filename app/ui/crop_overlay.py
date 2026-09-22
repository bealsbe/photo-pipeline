"""
CropOverlay — the interactive crop rectangle drawn over the image view.

The overlay is a transparent child of the QGraphicsView's *viewport*, so it
shares the viewport coordinate system.  It converts between viewport pixels and
the image's own pixel space with `view.mapToScene` / `view.mapFromScene`, which
means pan and zoom are handled for free: the crop rect stays welded to the
photo no matter how the view is transformed.

The rect itself is held in normalised 0–1 image coordinates (see
`app.models.crop.Crop`) so it transfers unchanged between a RAW and its JPG.

Interaction
-----------
drag inside     move the rect (clamped to the frame)
drag a corner   resize from the opposite corner
drag an edge    resize that edge; under an aspect lock the perpendicular
                dimension grows symmetrically about the rect centre
double-click    reset to the full frame
"""
from __future__ import annotations

from enum import Enum, auto
from typing import Optional, Tuple

from PySide6.QtCore import QPoint, QPointF, QRect, Qt, Signal
from PySide6.QtGui import QColor, QCursor, QPainter, QPen
from PySide6.QtWidgets import QWidget

from app.models.crop import MIN_EDGE, AspectRatio, Crop

# ── palette ────────────────────────────────────────────────────────────────── #

_ORANGE      = QColor(0xff, 0x6d, 0x00)
_DIM         = QColor(0, 0, 0, 165)      # outside-the-crop scrim
_GRID        = QColor(255, 255, 255, 58)
_EDGE        = QColor(255, 255, 255, 190)

_HANDLE_LEN   = 22      # arm length of a corner bracket, px
_HANDLE_THICK = 3
_GRAB         = 14      # hit-test slop around an edge/corner, px


class _Region(Enum):
    NONE   = auto()
    INSIDE = auto()
    TOP    = auto()
    BOTTOM = auto()
    LEFT   = auto()
    RIGHT  = auto()
    TL     = auto()
    TR     = auto()
    BL     = auto()
    BR     = auto()


_CURSORS = {
    _Region.INSIDE: Qt.SizeAllCursor,
    _Region.TOP:    Qt.SizeVerCursor,
    _Region.BOTTOM: Qt.SizeVerCursor,
    _Region.LEFT:   Qt.SizeHorCursor,
    _Region.RIGHT:  Qt.SizeHorCursor,
    _Region.TL:     Qt.SizeFDiagCursor,
    _Region.BR:     Qt.SizeFDiagCursor,
    _Region.TR:     Qt.SizeBDiagCursor,
    _Region.BL:     Qt.SizeBDiagCursor,
}

_CORNERS = (_Region.TL, _Region.TR, _Region.BL, _Region.BR)
_EDGES   = (_Region.TOP, _Region.BOTTOM, _Region.LEFT, _Region.RIGHT)


class CropOverlay(QWidget):
    """
    Interactive crop rect.  Parent must be the QGraphicsView's viewport;
    *view* is the QGraphicsView itself.
    """

    crop_changed: Signal = Signal(object)   # Crop — emitted live during a drag

    def __init__(self, view, parent=None) -> None:
        super().__init__(parent or view.viewport())
        self._view = view
        self._crop: Crop = Crop.full()
        self._aspect: Optional[AspectRatio] = None    # None = freeform
        self._image_size: Tuple[int, int] = (0, 0)

        self._drag_region: _Region = _Region.NONE
        self._drag_start_scene: Optional[QPointF] = None
        self._drag_start_crop: Optional[Crop] = None

        self.setMouseTracking(True)
        self.setAttribute(Qt.WA_NoSystemBackground, True)
        self.setAttribute(Qt.WA_TranslucentBackground, True)
        self.setFocusPolicy(Qt.NoFocus)
        self.hide()

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    @property
    def crop(self) -> Crop:
        return self._crop

    def set_image_size(self, width: int, height: int) -> None:
        """Tell the overlay the pixel dimensions of the image underneath."""
        self._image_size = (max(1, width), max(1, height))
        self.update()

    def set_crop(self, crop: Optional[Crop]) -> None:
        """Set the rect without emitting crop_changed (used when loading)."""
        self._crop = crop or Crop.full()
        self.update()

    def aspect(self) -> Optional[AspectRatio]:
        return self._aspect

    def set_aspect(self, ratio: Optional[AspectRatio], reshape: bool = True) -> None:
        """
        Apply an aspect constraint.  With *reshape* the current rect is
        immediately refitted to the new ratio, keeping its centre and staying
        inside the frame.
        """
        self._aspect = None if (ratio is None or ratio.is_free) else ratio
        if reshape and self._aspect is not None:
            self._crop = self._refit_to_aspect(self._crop, self._target_ratio())
            self._emit()
        self.update()

    def reset(self) -> None:
        """Back to the full frame, honouring any active aspect lock."""
        full = Crop.full(self._crop.aspect_label)
        if self._aspect is not None:
            full = self._refit_to_aspect(full, self._target_ratio())
        self._crop = full
        self._emit()
        self.update()

    def refresh(self) -> None:
        """Repaint after the view has panned, zoomed or resized."""
        self._sync_geometry()
        self.update()

    # ------------------------------------------------------------------ #
    # Aspect helpers                                                       #
    # ------------------------------------------------------------------ #

    def _target_ratio(self) -> Optional[float]:
        """
        The active constraint expressed in *normalised* units.

        A 16:9 crop is 16:9 in pixels, but the rect lives in 0–1 space where
        both axes are unit length — so the pixel ratio has to be divided by the
        image's own aspect to become a normalised-space ratio.
        """
        if self._aspect is None:
            return None
        pixel_ratio = self._aspect.value
        if pixel_ratio is None:
            return None
        img_w, img_h = self._image_size
        if img_w <= 0 or img_h <= 0:
            return None
        return pixel_ratio * (img_h / img_w)

    def _refit_to_aspect(self, crop: Crop, ratio: Optional[float]) -> Crop:
        """Largest rect of *ratio* that fits in the frame, centred on *crop*."""
        if ratio is None:
            return crop

        cx = (crop.left + crop.right) / 2.0
        cy = (crop.top + crop.bottom) / 2.0

        # Start from the current area, then clamp to what actually fits.
        w = crop.width
        h = w / ratio
        if h > crop.height:
            h = crop.height
            w = h * ratio

        w = min(w, 1.0)
        h = min(h, 1.0)
        # Re-derive the other edge after clamping so the ratio is exact.
        if w / ratio > 1.0:
            w = 1.0 * ratio
            h = 1.0
        else:
            h = w / ratio

        left = min(max(cx - w / 2.0, 0.0), 1.0 - w)
        top  = min(max(cy - h / 2.0, 0.0), 1.0 - h)
        return Crop(left, top, left + w, top + h, crop.aspect_label)

    # ------------------------------------------------------------------ #
    # Coordinate mapping                                                   #
    # ------------------------------------------------------------------ #

    def _sync_geometry(self) -> None:
        vp = self._view.viewport()
        if self.geometry() != vp.rect():
            self.setGeometry(vp.rect())

    def _norm_to_widget(self, crop: Crop) -> QRect:
        """Normalised rect → viewport pixel rect."""
        img_w, img_h = self._image_size
        tl = self._view.mapFromScene(QPointF(crop.left * img_w, crop.top * img_h))
        br = self._view.mapFromScene(QPointF(crop.right * img_w, crop.bottom * img_h))
        return QRect(tl, br).normalized()

    def _widget_to_norm(self, pos: QPoint) -> QPointF:
        """Viewport pixel point → normalised image point (unclamped)."""
        img_w, img_h = self._image_size
        scene = self._view.mapToScene(pos)
        return QPointF(scene.x() / img_w, scene.y() / img_h)

    # ------------------------------------------------------------------ #
    # Hit testing                                                          #
    # ------------------------------------------------------------------ #

    def _region_at(self, pos: QPoint) -> _Region:
        r = self._norm_to_widget(self._crop)
        x, y = pos.x(), pos.y()

        near_l = abs(x - r.left())   <= _GRAB
        near_r = abs(x - r.right())  <= _GRAB
        near_t = abs(y - r.top())    <= _GRAB
        near_b = abs(y - r.bottom()) <= _GRAB
        in_x   = r.left() - _GRAB <= x <= r.right()  + _GRAB
        in_y   = r.top()  - _GRAB <= y <= r.bottom() + _GRAB

        if near_l and near_t: return _Region.TL
        if near_r and near_t: return _Region.TR
        if near_l and near_b: return _Region.BL
        if near_r and near_b: return _Region.BR
        if near_t and in_x:   return _Region.TOP
        if near_b and in_x:   return _Region.BOTTOM
        if near_l and in_y:   return _Region.LEFT
        if near_r and in_y:   return _Region.RIGHT
        if r.contains(pos):   return _Region.INSIDE
        return _Region.NONE

    # ------------------------------------------------------------------ #
    # Mouse                                                                #
    # ------------------------------------------------------------------ #

    def mousePressEvent(self, ev) -> None:
        if ev.button() != Qt.LeftButton:
            ev.ignore()
            return
        region = self._region_at(ev.position().toPoint())
        if region is _Region.NONE:
            # Press outside the rect starts a brand-new crop from that corner.
            region = _Region.BR
            p = self._clamp_point(self._widget_to_norm(ev.position().toPoint()))
            self._crop = Crop(p.x(), p.y(), p.x() + MIN_EDGE, p.y() + MIN_EDGE,
                              self._crop.aspect_label)
        self._drag_region = region
        self._drag_start_scene = self._widget_to_norm(ev.position().toPoint())
        self._drag_start_crop = self._crop
        ev.accept()

    def mouseMoveEvent(self, ev) -> None:
        pos = ev.position().toPoint()

        if self._drag_region is _Region.NONE:
            self.setCursor(QCursor(_CURSORS.get(self._region_at(pos), Qt.ArrowCursor)))
            ev.accept()
            return

        now = self._widget_to_norm(pos)
        dx  = now.x() - self._drag_start_scene.x()
        dy  = now.y() - self._drag_start_scene.y()

        if self._drag_region is _Region.INSIDE:
            self._crop = self._translate(self._drag_start_crop, dx, dy)
        else:
            self._crop = self._resize(self._drag_start_crop, self._drag_region, now)

        self._emit()
        self.update()
        ev.accept()

    def mouseReleaseEvent(self, ev) -> None:
        self._drag_region = _Region.NONE
        self._drag_start_scene = None
        self._drag_start_crop = None
        self.setCursor(QCursor(_CURSORS.get(self._region_at(ev.position().toPoint()),
                                            Qt.ArrowCursor)))
        ev.accept()

    def mouseDoubleClickEvent(self, ev) -> None:
        self.reset()
        ev.accept()

    def wheelEvent(self, ev) -> None:
        # Let the graphics view keep its zoom-under-cursor behaviour.
        ev.ignore()

    # ------------------------------------------------------------------ #
    # Geometry edits                                                       #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _clamp_point(p: QPointF) -> QPointF:
        return QPointF(min(max(p.x(), 0.0), 1.0), min(max(p.y(), 0.0), 1.0))

    @staticmethod
    def _translate(crop: Crop, dx: float, dy: float) -> Crop:
        """Move without resizing — the rect stops at the frame edge."""
        dx = min(max(dx, -crop.left), 1.0 - crop.right)
        dy = min(max(dy, -crop.top), 1.0 - crop.bottom)
        return Crop(crop.left + dx, crop.top + dy,
                    crop.right + dx, crop.bottom + dy, crop.aspect_label)

    def _resize(self, crop: Crop, region: _Region, at: QPointF) -> Crop:
        at = self._clamp_point(at)
        l, t, r, b = crop.left, crop.top, crop.right, crop.bottom

        if region in (_Region.LEFT, _Region.TL, _Region.BL):
            l = min(at.x(), r - MIN_EDGE)
        if region in (_Region.RIGHT, _Region.TR, _Region.BR):
            r = max(at.x(), l + MIN_EDGE)
        if region in (_Region.TOP, _Region.TL, _Region.TR):
            t = min(at.y(), b - MIN_EDGE)
        if region in (_Region.BOTTOM, _Region.BL, _Region.BR):
            b = max(at.y(), t + MIN_EDGE)

        ratio = self._target_ratio()
        if ratio is not None:
            l, t, r, b = self._apply_ratio(l, t, r, b, region, ratio)

        return Crop(l, t, r, b, crop.aspect_label)

    @staticmethod
    def _apply_ratio(
        l: float, t: float, r: float, b: float,
        region: _Region, ratio: float,
    ) -> Tuple[float, float, float, float]:
        """
        Force width/height == *ratio*, anchored so the handle the user is
        holding stays under the cursor.

        Corners anchor the opposite corner.  Edges anchor their opposite edge
        and grow the perpendicular axis symmetrically about the centre, which
        is what every other crop tool does and what feels right in the hand.
        """
        w, h = r - l, b - t

        if region in _CORNERS:
            # Honour whichever axis the user pushed further, so dragging a
            # corner diagonally tracks the cursor instead of fighting it.
            if w / ratio >= h:
                h = w / ratio
            else:
                w = h * ratio

            anchor_right  = region in (_Region.TL, _Region.BL)   # right edge fixed
            anchor_bottom = region in (_Region.TL, _Region.TR)   # bottom edge fixed

            # Shrink to whatever actually fits before placing the rect.
            max_w = r if anchor_right else 1.0 - l
            max_h = b if anchor_bottom else 1.0 - t
            if w > max_w:
                w, h = max_w, max_w / ratio
            if h > max_h:
                h, w = max_h, max_h * ratio

            l, r = (r - w, r) if anchor_right else (l, l + w)
            t, b = (b - h, b) if anchor_bottom else (t, t + h)
            return l, t, r, b

        if region in (_Region.LEFT, _Region.RIGHT):
            h  = w / ratio
            cy = (t + b) / 2.0
            if h > 1.0:
                h, w = 1.0, ratio
                if region is _Region.LEFT:
                    l = r - w
                else:
                    r = l + w
            t = min(max(cy - h / 2.0, 0.0), 1.0 - h)
            b = t + h
            return l, t, r, b

        # TOP / BOTTOM
        w  = h * ratio
        cx = (l + r) / 2.0
        if w > 1.0:
            w, h = 1.0, 1.0 / ratio
            if region is _Region.TOP:
                t = b - h
            else:
                b = t + h
        l = min(max(cx - w / 2.0, 0.0), 1.0 - w)
        r = l + w
        return l, t, r, b

    def _emit(self) -> None:
        self.crop_changed.emit(self._crop)

    # ------------------------------------------------------------------ #
    # Painting                                                             #
    # ------------------------------------------------------------------ #

    def paintEvent(self, _ev) -> None:
        if self._image_size[0] <= 0:
            return

        r = self._norm_to_widget(self._crop)
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing, False)

        # Scrim everything outside the crop.  Four rects beat a clipped fill —
        # no path allocation per frame while the user drags.
        full = self.rect()
        p.fillRect(QRect(full.left(), full.top(), full.width(), r.top() - full.top()), _DIM)
        p.fillRect(QRect(full.left(), r.bottom(), full.width(), full.bottom() - r.bottom()), _DIM)
        p.fillRect(QRect(full.left(), r.top(), r.left() - full.left(), r.height()), _DIM)
        p.fillRect(QRect(r.right(), r.top(), full.right() - r.right(), r.height()), _DIM)

        # Rule-of-thirds guides
        p.setPen(QPen(_GRID, 1))
        for i in (1, 2):
            x = r.left() + r.width()  * i // 3
            y = r.top()  + r.height() * i // 3
            p.drawLine(x, r.top(), x, r.bottom())
            p.drawLine(r.left(), y, r.right(), y)

        # Rect outline
        p.setPen(QPen(_EDGE, 1))
        p.drawRect(r)

        # Corner brackets — drawn inside the rect so they never occlude the
        # scrim boundary the eye uses to judge the composition.
        p.setPen(QPen(_ORANGE, _HANDLE_THICK, Qt.SolidLine, Qt.FlatCap))
        arm = min(_HANDLE_LEN, r.width() // 3, r.height() // 3)
        off = _HANDLE_THICK // 2
        for cx, cy, sx, sy in (
            (r.left(),  r.top(),     1,  1),
            (r.right(), r.top(),    -1,  1),
            (r.left(),  r.bottom(),  1, -1),
            (r.right(), r.bottom(), -1, -1),
        ):
            x = cx + off * sx
            y = cy + off * sy
            p.drawLine(x, y, x + arm * sx, y)
            p.drawLine(x, y, x, y + arm * sy)

        # Edge grips
        p.setPen(QPen(_ORANGE, _HANDLE_THICK, Qt.SolidLine, Qt.FlatCap))
        grip = min(_HANDLE_LEN, r.width() // 4, r.height() // 4)
        mx, my = r.center().x(), r.center().y()
        p.drawLine(mx - grip // 2, r.top() + off, mx + grip // 2, r.top() + off)
        p.drawLine(mx - grip // 2, r.bottom() - off, mx + grip // 2, r.bottom() - off)
        p.drawLine(r.left() + off, my - grip // 2, r.left() + off, my + grip // 2)
        p.drawLine(r.right() - off, my - grip // 2, r.right() - off, my + grip // 2)

        p.end()
