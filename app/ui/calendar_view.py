"""
CalendarView — browse the export library by date.

Architecture
------------
CalendarView (QWidget)
  ├─ header  (#calendarBar) — ‹ › title, Month|Year toggle, Today, day tools
  └─ _Stage  — stack of pages + a transition overlay on top
       ├─ _MessagePage   no library / indexing / empty library
       ├─ _YearPage      12 mini-months, days shaded by how many shots
       ├─ _MonthPage     day cells with a photo mosaic
       ├─ _DayPage       a read-only GroupedGridView of one day's photos
       └─ _Transition    paints grabbed frames for slides and zooms

Month and year pages are single custom-painted widgets rather than a widget
per cell: a year is 365 squares and a month up to 168 thumbnail tiles, and
hover / fade repaints only touch the cells that changed (update(rect)).

Motion
------
Every page change goes through _Transition: the outgoing page is grabbed,
the change is applied, the incoming page is grabbed, and the overlay animates
between the two frames — a horizontal slide for prev/next, a zoom into / out
of a cell for year ↔ month ↔ day.  Input that arrives mid-animation grabs the
overlay's *current* frame as the new starting point, so rapid navigation
chases the latest target instead of snapping.

Thumbnails
----------
The view owns its own ThumbnailGenerator.  The main window's generator is
cleared on every working-folder scan, which would otherwise wipe the
calendar's previews.  Both share the on-disk cache (same directory, same
generation size), and preview records are built from the index's stat
exactly as the scanner builds them, so the disk-cache keys match.
"""
from __future__ import annotations

import calendar as _cal
import time
from collections import OrderedDict
from datetime import date as _date, datetime
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

from PySide6.QtCore import (
    QEasingCurve,
    QEvent,
    QLocale,
    QPointF,
    QRectF,
    Qt,
    QTimer,
    QVariantAnimation,
    Signal,
)
from PySide6.QtGui import (
    QColor,
    QCursor,
    QFont,
    QKeySequence,
    QNativeGestureEvent,
    QPainter,
    QPainterPath,
    QPen,
    QPixmap,
    QShortcut,
)
from PySide6.QtWidgets import (
    QButtonGroup,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from app.models.collection import PhotoCollection
from app.models.library_index import HEAT_LEVELS, LibraryDay, LibraryIndex
from app.models.photo_record import PhotoRecord
from app.scanning.library_indexer import DayRecordsWorker, LibraryIndexWorker
from app.thumbnails.generator import (
    PRIORITY_PREFETCH,
    PRIORITY_VISIBLE,
    ThumbnailGenerator,
)
from app.ui.grouped_grid import GroupedGridView
from app.ui.icons import calendar_icon

# ── tuning ──────────────────────────────────────────────────────────────────

_GENERATION_SIZE = 300              # must match MainWindow — shares the disk cache
_CACHE_BYTES     = 128 * 1024 * 1024
_SLIDE_MS        = 240
_ZOOM_MS         = 270
_FADE_MS         = 200              # thumbnail fade-in
_HOVER_MS        = 110
_SETTLE_MS       = 80               # prefetch after navigation settles
_WHEEL_STEP      = 120              # one notch
_WHEEL_COOLDOWN  = 0.16             # s between wheel-driven page steps
_DAY_WAIT_MS     = 160              # hold a day zoom this long for records
_DAY_CACHE       = 8                # opened days kept in memory

# ── palette (see main.py) ───────────────────────────────────────────────────

_BG          = QColor(0x0a, 0x0a, 0x12)
_CELL        = QColor(0x0e, 0x0e, 0x1a)
_CELL_OUT    = QColor(0x0b, 0x0b, 0x14)
_CELL_HOVER  = QColor(0x16, 0x13, 0x22)
_TILE_EMPTY  = QColor(0x16, 0x16, 0x26)
_TEXT        = QColor(0xf0, 0xf0, 0xf0)
_TEXT_DIM    = QColor(0x78, 0x78, 0xa0)
_TEXT_FAINT  = QColor(0x3a, 0x3a, 0x52)
_ACCENT      = QColor(0xff, 0x6d, 0x00)

# Heat-map fill alpha per level (0 = no photos)
_HEAT_ALPHA = [0, 70, 125, 185, 245]
assert len(_HEAT_ALPHA) == HEAT_LEVELS + 1

_MODE_YEAR, _MODE_MONTH, _MODE_DAY = "year", "month", "day"


def _accent(alpha: int) -> QColor:
    c = QColor(_ACCENT)
    c.setAlpha(alpha)
    return c


def _lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def _lerp_color(a: QColor, b: QColor, t: float) -> QColor:
    return QColor(
        int(_lerp(a.red(),   b.red(),   t)),
        int(_lerp(a.green(), b.green(), t)),
        int(_lerp(a.blue(),  b.blue(),  t)),
        int(_lerp(a.alpha(), b.alpha(), t)),
    )


def _font(px: int, weight: QFont.Weight = QFont.Normal) -> QFont:
    f = QFont()
    f.setPixelSize(px)
    f.setWeight(weight)
    return f


def _first_weekday() -> int:
    """Locale's first day of week as a `calendar` weekday (0 = Monday)."""
    return (QLocale().firstDayOfWeek().value - 1) % 7


def _month_shift(year: int, month: int, delta: int) -> Tuple[int, int]:
    m = year * 12 + (month - 1) + delta
    return m // 12, m % 12 + 1


def _plural(n: int, word: str) -> str:
    return f"{n:,} {word}{'' if n == 1 else 's'}"


# ── shared helpers ──────────────────────────────────────────────────────────

class _CoverCache:
    """
    Pre-scaled, corner-rounded, cover-cropped thumbnail tiles.

    Keyed by (path, w, h, dpr): the delegate-style "scale on paint" would
    rerun a smooth scale for every tile of every hover frame.
    """

    def __init__(self, max_entries: int = 900) -> None:
        self._data: OrderedDict = OrderedDict()
        self._max = max_entries

    def get(self, path: Path, src: QPixmap, w: int, h: int,
            dpr: float, radius: float) -> QPixmap:
        key = (path, w, h, dpr)
        pm = self._data.get(key)
        if pm is not None:
            self._data.move_to_end(key)
            return pm
        pw, ph = max(1, round(w * dpr)), max(1, round(h * dpr))
        scaled = src.scaled(pw, ph, Qt.KeepAspectRatioByExpanding, Qt.SmoothTransformation)
        x = (scaled.width() - pw) // 2
        y = (scaled.height() - ph) // 2
        pm = QPixmap(pw, ph)
        pm.fill(Qt.transparent)
        p = QPainter(pm)
        p.setRenderHint(QPainter.Antialiasing)
        path_ = QPainterPath()
        path_.addRoundedRect(QRectF(0, 0, pw, ph), radius * dpr, radius * dpr)
        p.setClipPath(path_)
        p.drawPixmap(0, 0, scaled, x, y, pw, ph)
        p.end()
        pm.setDevicePixelRatio(dpr)
        self._data[key] = pm
        while len(self._data) > self._max:
            self._data.popitem(last=False)
        return pm

    def clear(self) -> None:
        self._data.clear()


class _Animator:
    """
    Per-key 0→1 values for hover highlights and thumbnail fade-ins.

    One 60 fps timer runs only while something is still moving; each tick
    calls *on_tick(keys)* with the keys that changed so the owner can repaint
    just those cells.
    """

    def __init__(self, owner: QWidget, on_tick: Callable[[List], None]) -> None:
        self._hover: Dict = {}          # key → (value, target)
        self._fade:  Dict = {}          # key → start time (monotonic)
        self._on_tick = on_tick
        self._last = time.monotonic()
        self._timer = QTimer(owner)
        self._timer.setInterval(16)
        self._timer.timeout.connect(self._tick)

    # hover ------------------------------------------------------------
    def set_hover(self, key, on: bool) -> None:
        if key is None:
            return
        v, _ = self._hover.get(key, (0.0, 0.0))
        self._hover[key] = (v, 1.0 if on else 0.0)
        self._kick()

    def hover(self, key) -> float:
        return self._hover.get(key, (0.0, 0.0))[0]

    # fade -------------------------------------------------------------
    def start_fade(self, key) -> None:
        self._fade[key] = time.monotonic()
        self._kick()

    def fade(self, key) -> float:
        start = self._fade.get(key)
        if start is None:
            return 1.0
        t = (time.monotonic() - start) * 1000 / _FADE_MS
        return 1.0 if t >= 1 else t * (2 - t)          # ease-out quad

    def reset(self) -> None:
        self._hover.clear()
        self._fade.clear()

    # ------------------------------------------------------------------
    def _kick(self) -> None:
        if not self._timer.isActive():
            self._last = time.monotonic()
            self._timer.start()

    def _tick(self) -> None:
        now = time.monotonic()
        step = (now - self._last) * 1000 / _HOVER_MS
        self._last = now
        changed = []
        for key, (v, target) in list(self._hover.items()):
            if v != target:
                v = min(target, v + step) if target > v else max(target, v - step)
                changed.append(key)
                if v == 0.0 and target == 0.0:
                    del self._hover[key]
                else:
                    self._hover[key] = (v, target)
        for key, start in list(self._fade.items()):
            changed.append(key)
            if (now - start) * 1000 >= _FADE_MS:
                del self._fade[key]
        if not changed:
            self._timer.stop()
            return
        self._on_tick(changed)


class _WheelNav:
    """
    Turns wheel / trackpad scrolling into discrete page steps.

    Trackpads send a stream of small deltas; accumulating to one notch and
    enforcing a short cooldown means a flick moves one page, not twelve.
    """

    def __init__(self) -> None:
        self._acc = 0
        self._last = 0.0

    def feed(self, event) -> int:
        d = event.angleDelta()
        delta = d.y() if abs(d.y()) >= abs(d.x()) else d.x()
        if delta == 0:
            return 0
        if (self._acc > 0) != (delta > 0):
            self._acc = 0
        self._acc += delta
        now = time.monotonic()
        if abs(self._acc) >= _WHEEL_STEP and now - self._last >= _WHEEL_COOLDOWN:
            step = -1 if self._acc > 0 else 1      # wheel down / swipe left → forward
            self._acc = 0
            self._last = now
            return step
        return 0


class _PinchNav:
    """Trackpad / touchscreen pinch → one zoom step per gesture."""

    def __init__(self) -> None:
        self._scale = 1.0
        self._fired = False

    def handle(self, ev) -> Optional[int]:
        """Return +1 / -1 to zoom, 0 for a consumed event, None if not a pinch."""
        if ev.type() == QEvent.NativeGesture and isinstance(ev, QNativeGestureEvent):
            gt = ev.gestureType()
            if gt == Qt.NativeGestureType.BeginNativeGesture:
                self._scale, self._fired = 1.0, False
            elif gt == Qt.NativeGestureType.ZoomNativeGesture:
                self._scale *= 1.0 + ev.value()
                return self._check()
            return 0
        if ev.type() == QEvent.Gesture:
            pinch = ev.gesture(Qt.PinchGesture)
            if pinch is None:
                return None
            if pinch.state() == Qt.GestureStarted:
                self._scale, self._fired = 1.0, False
            self._scale = pinch.totalScaleFactor()
            ev.accept()
            return self._check()
        return None

    def _check(self) -> int:
        if self._fired:
            return 0
        if self._scale > 1.25:
            self._fired = True
            return 1
        if self._scale < 0.8:
            self._fired = True
            return -1
        return 0


# ── transition overlay ──────────────────────────────────────────────────────

class _Transition(QWidget):
    """
    Paints two grabbed frames while a page change animates.

    slide  — old frame leaves, new frame enters, horizontally.
    zoom   — the new page grows out of *rect* (zoom in), or the old page
             shrinks into *rect* on the new one (zoom out).  Scaling is
             uniform: *rect* is first fitted to the page's aspect ratio.
    """

    finished = Signal()

    def __init__(self, parent: QWidget) -> None:
        super().__init__(parent)
        self.setAttribute(Qt.WA_TransparentForMouseEvents)
        self.hide()
        self._old: Optional[QPixmap] = None
        self._new: Optional[QPixmap] = None
        self._mode = "slide"
        self._dir = 1
        self._rect = QRectF()
        self._zoom_in = True
        self._t = 0.0

        self._anim = QVariantAnimation(self)
        self._anim.setStartValue(0.0)
        self._anim.setEndValue(1.0)
        self._anim.setEasingCurve(QEasingCurve.OutCubic)
        self._anim.valueChanged.connect(self._on_value)
        self._anim.finished.connect(self._on_finished)

    def running(self) -> bool:
        return self._anim.state() == QVariantAnimation.Running

    def snapshot(self) -> Optional[QPixmap]:
        """The frame currently on screen, if an animation is mid-flight."""
        return self.grab() if self.running() else None

    def set_new(self, pm: QPixmap) -> None:
        """Refresh the incoming frame (e.g. thumbnails arrived mid-animation)."""
        if self.running():
            self._new = pm
            self.update()

    def slide(self, old: QPixmap, new: QPixmap, direction: int) -> None:
        self._mode, self._dir = "slide", direction
        self._start(old, new, _SLIDE_MS)

    def zoom(self, old: QPixmap, new: QPixmap, rect: QRectF, zoom_in: bool) -> None:
        self._mode, self._zoom_in = "zoom", zoom_in
        self._rect = self._fit_aspect(rect)
        self._start(old, new, _ZOOM_MS)

    def stop(self) -> None:
        self._anim.stop()
        self._on_finished()

    # ------------------------------------------------------------------
    def _start(self, old: QPixmap, new: QPixmap, ms: int) -> None:
        self._anim.stop()
        self._old, self._new, self._t = old, new, 0.0
        self._anim.setDuration(ms)
        self.show()
        self.raise_()
        self._anim.start()

    def _fit_aspect(self, r: QRectF) -> QRectF:
        """Grow *r* about its centre to the page's aspect ratio (contain)."""
        W, H = max(1, self.width()), max(1, self.height())
        if r.isEmpty():
            return QRectF(W * 0.45, H * 0.45, W * 0.1, H * 0.1)
        scale = max(r.width() / W, r.height() / H)
        w, h = W * scale, H * scale
        c = r.center()
        return QRectF(c.x() - w / 2, c.y() - h / 2, w, h)

    def _on_value(self, v) -> None:
        self._t = float(v)
        self.update()

    def _on_finished(self) -> None:
        self.hide()
        self._old = self._new = None
        self.finished.emit()

    def paintEvent(self, _ev) -> None:
        if self._old is None or self._new is None:
            return
        p = QPainter(self)
        p.setRenderHint(QPainter.SmoothPixmapTransform)
        full = QRectF(self.rect())
        p.fillRect(full, _BG)
        t = self._t

        if self._mode == "slide":
            off = -self._dir * full.width() * t
            p.drawPixmap(full.translated(off, 0), self._old, QRectF(self._old.rect()))
            p.drawPixmap(full.translated(off + self._dir * full.width(), 0),
                         self._new, QRectF(self._new.rect()))
            return

        R = self._rect
        k = full.width() / R.width()
        # The rect that, drawn with the page's frame, maps R onto the full view
        magnified = QRectF(-R.x() * k, -R.y() * k, full.width() * k, full.height() * k)

        def lerp_rect(a: QRectF, b: QRectF, s: float) -> QRectF:
            return QRectF(_lerp(a.x(), b.x(), s), _lerp(a.y(), b.y(), s),
                          _lerp(a.width(), b.width(), s), _lerp(a.height(), b.height(), s))

        if self._zoom_in:
            # Outer page magnifies toward R and fades; inner page grows out of R.
            p.setOpacity(1.0 - t)
            p.drawPixmap(lerp_rect(full, magnified, t), self._old, QRectF(self._old.rect()))
            p.setOpacity(min(1.0, t * 1.4))
            p.drawPixmap(lerp_rect(R, full, t), self._new, QRectF(self._new.rect()))
        else:
            # Outer page settles from magnified; inner page shrinks into R.
            p.setOpacity(min(1.0, 0.25 + t))
            p.drawPixmap(lerp_rect(magnified, full, t), self._new, QRectF(self._new.rect()))
            p.setOpacity(max(0.0, 1.0 - t * 1.3))
            p.drawPixmap(lerp_rect(full, R, t), self._old, QRectF(self._old.rect()))


class _Stage(QWidget):
    """Holds the page stack with the transition overlay laid exactly over it."""

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.stack = QStackedWidget(self)
        self.overlay = _Transition(self)

    def resizeEvent(self, ev) -> None:
        super().resizeEvent(ev)
        self.stack.setGeometry(self.rect())
        self.overlay.setGeometry(self.rect())


# ── pages ───────────────────────────────────────────────────────────────────

class _MessagePage(QWidget):
    """Centred empty-state message with an optional action button."""

    action = Signal()

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setStyleSheet("background: #0a0a12;")
        lay = QVBoxLayout(self)
        lay.addStretch(3)
        self._icon = QLabel()
        self._icon.setAlignment(Qt.AlignCenter)
        self._icon.setPixmap(calendar_icon("#40405a", 56).pixmap(56, 56))
        self._title = QLabel()
        self._title.setAlignment(Qt.AlignCenter)
        self._title.setStyleSheet("color:#c8c8d8; font-size:16px; font-weight:600;")
        self._body = QLabel()
        self._body.setAlignment(Qt.AlignCenter)
        self._body.setWordWrap(True)
        self._body.setStyleSheet("color:#6a6a8a; font-size:12px;")
        self._btn = QPushButton()
        self._btn.setCursor(Qt.PointingHandCursor)
        self._btn.setStyleSheet(
            "QPushButton{background:rgba(255,109,0,0.18);color:#ff6d00;"
            "border:1px solid rgba(255,109,0,0.55);border-radius:6px;"
            "font-size:12px;font-weight:700;padding:7px 18px;}"
            "QPushButton:hover{background:rgba(255,109,0,0.30);}"
        )
        self._btn.clicked.connect(self.action)
        row = QHBoxLayout()
        row.addStretch()
        row.addWidget(self._btn)
        row.addStretch()
        for w in (self._icon, self._title, self._body):
            lay.addWidget(w)
        lay.addSpacing(8)
        lay.addLayout(row)
        lay.addStretch(4)

    def set_message(self, title: str, body: str = "", button: str = "") -> None:
        self._title.setText(title)
        self._body.setText(body)
        self._body.setVisible(bool(body))
        self._btn.setText(button)
        self._btn.setVisible(bool(button))


class _PaintedPage(QWidget):
    """
    Common plumbing for the custom-painted month and year pages: thumbnail
    lookup with fade-in, hover animation, wheel and pinch input.
    """

    step = Signal(int)                    # ±1 page
    zoom = Signal(int, QPointF)           # +1 in / -1 out, at position

    def __init__(self, generator: ThumbnailGenerator, covers: _CoverCache,
                 parent=None) -> None:
        super().__init__(parent)
        self._gen = generator
        self._covers = covers
        self._index: Optional[LibraryIndex] = None
        self._anim = _Animator(self, self._on_anim_tick)
        self._wheel = _WheelNav()
        self._pinch = _PinchNav()
        self._hover_key = None
        # Paths drawn on this page → the rect(s) to repaint when they change
        self._path_rects: Dict[Path, QRectF] = {}
        self._resizing = False
        self._resize_timer = QTimer(self)
        self._resize_timer.setSingleShot(True)
        self._resize_timer.setInterval(140)
        self._resize_timer.timeout.connect(self._end_resize)
        self.setMouseTracking(True)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAttribute(Qt.WA_OpaquePaintEvent)
        self.grabGesture(Qt.PinchGesture)

    # thumbnails ----------------------------------------------------------
    def thumbnail_arrived(self, path: Path) -> bool:
        """A thumbnail landed; fade it in if it's on this page."""
        rect = self._path_rects.get(path)
        if rect is None:
            return False
        self._anim.start_fade(path)
        self.update(rect.toAlignedRect().adjusted(-2, -2, 2, 2))
        return True

    def _draw_thumb(self, p: QPainter, record: PhotoRecord, rect: QRectF,
                    radius: float, priority: int = PRIORITY_VISIBLE) -> None:
        self._path_rects[record.path] = rect
        src = self._gen.get_cached(record.path)
        if src is None:
            self._gen.request(record, priority)
            p.setPen(Qt.NoPen)
            p.setBrush(_TILE_EMPTY)
            p.drawRoundedRect(rect, radius, radius)
            return
        alpha = self._anim.fade(record.path)
        if alpha < 1.0:
            p.setPen(Qt.NoPen)
            p.setBrush(_TILE_EMPTY)
            p.drawRoundedRect(rect, radius, radius)
        p.setOpacity(alpha)
        if self._resizing:
            # Mid-resize: scale on the fly (cheap, slightly soft) rather than
            # filling the cover cache with sizes that exist for one frame.
            path_ = QPainterPath()
            path_.addRoundedRect(rect, radius, radius)
            p.save()
            p.setClipPath(path_)
            s = max(rect.width() / src.width(), rect.height() / src.height())
            sw, sh = rect.width() / s, rect.height() / s
            p.drawPixmap(rect, src, QRectF((src.width() - sw) / 2,
                                           (src.height() - sh) / 2, sw, sh))
            p.restore()
        else:
            w, h = max(1, int(rect.width())), max(1, int(rect.height()))
            tile = self._covers.get(record.path, src, w, h,
                                    self.devicePixelRatioF(), radius)
            p.drawPixmap(QPointF(rect.x(), rect.y()), tile)
        p.setOpacity(1.0)

    # hover ---------------------------------------------------------------
    def _set_hover(self, key) -> None:
        if key == self._hover_key:
            return
        self._anim.set_hover(self._hover_key, False)
        self._anim.set_hover(key, True)
        self._hover_key = key

    def _key_rect(self, key) -> Optional[QRectF]:     # pragma: no cover
        raise NotImplementedError

    def _on_anim_tick(self, keys: List) -> None:
        for key in keys:
            r = self._path_rects.get(key) if isinstance(key, Path) else self._key_rect(key)
            if r is not None:
                self.update(r.toAlignedRect().adjusted(-3, -3, 3, 3))

    def leaveEvent(self, ev) -> None:
        self._set_hover(None)
        super().leaveEvent(ev)

    # input -----------------------------------------------------------------
    def wheelEvent(self, ev) -> None:
        if ev.modifiers() & Qt.ControlModifier:
            dy = ev.angleDelta().y()
            if dy:
                self.zoom.emit(1 if dy > 0 else -1, ev.position())
        else:
            s = self._wheel.feed(ev)
            if s:
                self.step.emit(s)
        ev.accept()

    def event(self, ev) -> bool:
        # event() already runs during __init__ (ChildAdded for the timers),
        # before the pinch tracker exists.
        pinch = self.__dict__.get("_pinch")
        r = pinch.handle(ev) if pinch is not None else None
        if r is not None:
            if r:
                self.zoom.emit(r, QPointF(self.mapFromGlobal(QCursor.pos())))
            return True
        return super().event(ev)

    def resizeEvent(self, ev) -> None:
        super().resizeEvent(ev)
        self._resizing = True
        self._resize_timer.start()
        self._relayout()

    def _end_resize(self) -> None:
        self._resizing = False
        self.update()

    def _relayout(self) -> None:              # pragma: no cover
        pass


class _MonthPage(_PaintedPage):
    """One month: weekday header over a 7-column grid of day cells."""

    day_activated = Signal(object, QRectF)     # date, cell rect
    month_jump    = Signal(int, int)           # year, month
    jump_nearest  = Signal()

    _GAP   = 6
    _HEAD  = 26
    _NUM_H = 24

    def __init__(self, generator, covers, parent=None) -> None:
        super().__init__(generator, covers, parent)
        self._year, self._month = _date.today().year, _date.today().month
        self._cells: List[Tuple[_date, QRectF]] = []
        self._weeks: List[List[_date]] = []
        self._pill = QRectF()
        self._pressed: Optional[_date] = None
        self._cal = _cal.Calendar(_first_weekday())

    # api ---------------------------------------------------------------
    def set_month(self, index: Optional[LibraryIndex], year: int, month: int) -> None:
        self._index, self._year, self._month = index, year, month
        self._weeks = self._cal.monthdatescalendar(year, month)
        self._anim.reset()
        self._hover_key = None
        self._path_rects.clear()
        self._relayout()
        self.update()

    def cell_rect(self, d: _date) -> QRectF:
        for cd, r in self._cells:
            if cd == d:
                return r
        return QRectF()

    def preview_records(self, year: int, month: int) -> List[PhotoRecord]:
        """Previews month (year, month) would draw — for prefetching."""
        if self._index is None:
            return []
        out = []
        for day in self._index.days_in_month(year, month).values():
            out.extend(day.preview_records(4 if day.shot_count >= 4 else 1))
        return out

    # layout --------------------------------------------------------------
    def _relayout(self) -> None:
        self._cells = []
        if not self._weeks:
            return
        r = QRectF(self.rect()).adjusted(14, 6, -14, -14)
        rows = len(self._weeks)
        g = self._GAP
        cw = (r.width() - g * 6) / 7
        ch = (r.height() - self._HEAD - g * (rows - 1)) / rows
        for row, week in enumerate(self._weeks):
            for col, d in enumerate(week):
                x = r.x() + col * (cw + g)
                y = r.y() + self._HEAD + row * (ch + g)
                self._cells.append((d, QRectF(x, y, cw, ch)))

    def _key_rect(self, key) -> Optional[QRectF]:
        return self.cell_rect(key) if isinstance(key, _date) else None

    def _cell_at(self, pos: QPointF) -> Optional[_date]:
        for d, r in self._cells:
            if r.contains(pos):
                return d
        return None

    def _day(self, d: _date) -> Optional[LibraryDay]:
        return self._index.get(d) if self._index is not None else None

    # painting --------------------------------------------------------------
    def paintEvent(self, ev) -> None:
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        p.fillRect(ev.rect(), _BG)
        clip = QRectF(ev.rect())

        # Weekday header
        if self._cells:
            p.setFont(_font(11, QFont.DemiBold))
            p.setPen(_TEXT_DIM)
            for d, r in self._cells[:7]:
                hr = QRectF(r.x(), r.y() - self._HEAD, r.width(), self._HEAD - 4)
                if hr.intersects(clip):
                    p.drawText(hr, Qt.AlignCenter,
                               QLocale().dayName(d.isoweekday(), QLocale.ShortFormat).upper())

        today = _date.today()
        for d, r in self._cells:
            if r.adjusted(-3, -3, 3, 3).intersects(clip):
                self._paint_cell(p, d, r, today)

        self._paint_empty_hint(p)

    def _paint_cell(self, p: QPainter, d: _date, r: QRectF, today: _date) -> None:
        in_month = d.month == self._month
        day = self._day(d)
        h = self._anim.hover(d)
        radius = 8.0

        base = _CELL if in_month else _CELL_OUT
        p.setPen(Qt.NoPen)
        p.setBrush(_lerp_color(base, _CELL_HOVER, h) if (in_month or day) else base)
        p.drawRoundedRect(r, radius, radius)

        # Day number (today gets an orange disc)
        num_rect = QRectF(r.x() + 6, r.y() + 4, 24, 20)
        p.setFont(_font(12, QFont.Bold if in_month else QFont.Normal))
        if d == today:
            p.setBrush(_ACCENT)
            p.drawEllipse(num_rect.center(), 11, 10)
            p.setPen(QColor(0x0a, 0x0a, 0x12))
        else:
            p.setPen(_TEXT if in_month else _TEXT_FAINT)
        p.drawText(num_rect, Qt.AlignCenter, str(d.day))

        if day is not None and not in_month:
            # Adjacent-month day with photos: a faint dot, click to go there
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(90))
            p.drawEllipse(QPointF(r.right() - 12, r.y() + 14), 3, 3)
        elif day is not None:
            # Count badge
            text = f"{day.shot_count:,}"
            p.setFont(_font(10, QFont.DemiBold))
            tw = p.fontMetrics().horizontalAdvance(text) + 12
            badge = QRectF(r.right() - tw - 5, r.y() + 6, tw, 16)
            p.setBrush(_accent(int(_lerp(34, 70, h))))
            p.drawRoundedRect(badge, 8, 8)
            p.setPen(_lerp_color(QColor(0xc8, 0xc8, 0xd8), _ACCENT, h))
            p.drawText(badge, Qt.AlignCenter, text)

            area = r.adjusted(5, self._NUM_H + 4, -5, -5)
            if area.height() >= 22 and area.width() >= 22:
                n = 4 if day.shot_count >= 4 and area.width() >= 56 and area.height() >= 44 else 1
                recs = day.preview_records(n)
                if n == 1:
                    tiles = [area]
                else:
                    g = 2.0
                    tw2, th2 = (area.width() - g) / 2, (area.height() - g) / 2
                    tiles = [QRectF(area.x() + (i % 2) * (tw2 + g),
                                    area.y() + (i // 2) * (th2 + g), tw2, th2)
                             for i in range(4)]
                for rec, tile in zip(recs, tiles):
                    self._draw_thumb(p, rec, tile, 5.0)
            else:
                # Too small for photos: a heat bar along the bottom
                level = self._index.heat_level(day)
                p.setPen(Qt.NoPen)
                p.setBrush(_accent(_HEAT_ALPHA[level]))
                p.drawRoundedRect(QRectF(r.x() + 6, r.bottom() - 7, r.width() - 12, 3), 1.5, 1.5)

        # Hover outline — stronger on days that open something
        if h > 0 and (in_month or day):
            pen = QPen(_accent(int(h * (150 if day else 45))))
            pen.setWidthF(1.2)
            p.setPen(pen)
            p.setBrush(Qt.NoBrush)
            p.drawRoundedRect(r.adjusted(0.6, 0.6, -0.6, -0.6), radius, radius)

    def _paint_empty_hint(self, p: QPainter) -> None:
        self._pill = QRectF()
        if self._index is None or len(self._index) == 0:
            return
        if self._index.days_in_month(self._year, self._month):
            return
        text = f"No photos in {_cal.month_name[self._month]} {self._year}   ·   Jump to nearest  ›"
        p.setFont(_font(12, QFont.DemiBold))
        w = p.fontMetrics().horizontalAdvance(text) + 36
        self._pill = QRectF((self.width() - w) / 2, self.height() - 64, w, 34)
        h = self._anim.hover("pill")
        p.setPen(QPen(_accent(int(_lerp(90, 200, h))), 1))
        p.setBrush(QColor(0x14, 0x14, 0x2a, 235))
        p.drawRoundedRect(self._pill, 17, 17)
        p.setPen(_lerp_color(QColor(0xc8, 0xc8, 0xd8), _ACCENT, h))
        p.drawText(self._pill, Qt.AlignCenter, text)

    # input -----------------------------------------------------------------
    def mouseMoveEvent(self, ev) -> None:
        pos = ev.position()
        if not self._pill.isEmpty() and self._pill.contains(pos):
            self._set_hover("pill")
            self.setCursor(Qt.PointingHandCursor)
            return
        d = self._cell_at(pos)
        self._set_hover(d)
        clickable = d is not None and (self._day(d) is not None or d.month != self._month)
        self.setCursor(Qt.PointingHandCursor if clickable else Qt.ArrowCursor)

    def _on_anim_tick(self, keys: List) -> None:
        if "pill" in keys and not self._pill.isEmpty():
            self.update(self._pill.toAlignedRect().adjusted(-2, -2, 2, 2))
        super()._on_anim_tick([k for k in keys if k != "pill"])

    def mousePressEvent(self, ev) -> None:
        if ev.button() == Qt.LeftButton:
            self._pressed = self._cell_at(ev.position())
            self.setFocus()
        super().mousePressEvent(ev)

    def mouseReleaseEvent(self, ev) -> None:
        if ev.button() != Qt.LeftButton:
            return
        pos = ev.position()
        if not self._pill.isEmpty() and self._pill.contains(pos):
            self.jump_nearest.emit()
            return
        d = self._cell_at(pos)
        if d is None or d != self._pressed:
            return
        if d.month != self._month:
            self.month_jump.emit(d.year, d.month)
        elif self._day(d) is not None:
            self.day_activated.emit(d, self.cell_rect(d))

    def mouseDoubleClickEvent(self, ev) -> None:
        # A double-click is two clicks; the first already acted.
        ev.accept()

    def hovered_day(self) -> Optional[_date]:
        return self._hover_key if isinstance(self._hover_key, _date) else None


class _YearPage(_PaintedPage):
    """Twelve mini-months; days shaded by shot count."""

    day_activated   = Signal(object, QRectF)    # date, square rect
    month_activated = Signal(int, QRectF)       # month, mini-month rect
    jump_nearest    = Signal()

    _TITLE_H = 26
    _WEEK_H  = 16

    def __init__(self, generator, covers, parent=None) -> None:
        super().__init__(generator, covers, parent)
        self._year = _date.today().year
        self._cal = _cal.Calendar(_first_weekday())
        self._months: List[Tuple[int, QRectF, QRectF]] = []   # month, box, title rect
        self._squares: Dict[_date, QRectF] = {}
        self._card = QRectF()
        self._pill = QRectF()
        self._pressed = None

    def set_year(self, index: Optional[LibraryIndex], year: int) -> None:
        self._index, self._year = index, year
        self._anim.reset()
        self._hover_key = None
        self._path_rects.clear()
        self._relayout()
        self.update()

    def month_rect(self, month: int) -> QRectF:
        for m, box, _ in self._months:
            if m == month:
                return box
        return QRectF()

    def day_rect(self, d: _date) -> QRectF:
        return self._squares.get(d, QRectF())

    # layout ------------------------------------------------------------
    def _relayout(self) -> None:
        self._months, self._squares = [], {}
        W, H = self.width(), self.height() - 30          # legend strip at the bottom
        if W <= 0 or H <= 0:
            return
        # Pick the grid (cols × rows) whose mini-months get the biggest squares
        best = None
        for cols, rows in ((6, 2), (4, 3), (3, 4), (2, 6)):
            bw = (W - 28 - (cols - 1) * 22) / cols
            bh = (H - 12 - (rows - 1) * 16) / rows
            sq = min(bw / 7, (bh - self._TITLE_H - self._WEEK_H) / 6)
            if best is None or sq > best[0]:
                best = (sq, cols, rows, bw, bh)
        sq, cols, rows, bw, bh = best
        sq = max(4.0, sq)
        block_w = sq * 7
        block_h = self._TITLE_H + self._WEEK_H + sq * 6
        x0 = (W - (block_w * cols + (cols - 1) * 22)) / 2
        y0 = 10 + max(0.0, (H - 12 - (block_h * rows + (rows - 1) * 16)) / 2)
        for i in range(12):
            month = i + 1
            col, row = i % cols, i // cols
            x = x0 + col * (block_w + 22)
            y = y0 + row * (block_h + 16)
            box = QRectF(x - 6, y - 4, block_w + 12, block_h + 8)
            title = QRectF(x, y, block_w, self._TITLE_H - 4)
            self._months.append((month, box, title))
            for r_i, week in enumerate(self._cal.monthdatescalendar(self._year, month)):
                for c_i, d in enumerate(week):
                    if d.month != month:
                        continue
                    self._squares[d] = QRectF(
                        x + c_i * sq, y + self._TITLE_H + self._WEEK_H + r_i * sq, sq, sq)
        self._sq = sq

    def _key_rect(self, key) -> Optional[QRectF]:
        if isinstance(key, _date):
            return self._squares.get(key)
        if isinstance(key, int):
            for m, _, title in self._months:
                if m == key:
                    return title
        if key == "pill":
            return self._pill
        return None

    def _on_anim_tick(self, keys: List) -> None:
        super()._on_anim_tick(keys)
        if not self._card.isEmpty():
            self.update(self._card.toAlignedRect().adjusted(-4, -4, 4, 4))

    def thumbnail_arrived(self, path: Path) -> bool:
        hit = super().thumbnail_arrived(path)
        if hit and not self._card.isEmpty():
            self.update(self._card.toAlignedRect().adjusted(-4, -4, 4, 4))
        return hit

    # painting ----------------------------------------------------------
    def paintEvent(self, ev) -> None:
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        p.fillRect(ev.rect(), _BG)
        clip = QRectF(ev.rect())
        today = _date.today()
        sq = getattr(self, "_sq", 8.0)
        inset = max(1.0, sq * 0.09)
        radius = max(1.5, sq * 0.22)
        show_nums = sq >= 20
        wd_names = [QLocale().dayName(((self._cal.firstweekday + i) % 7) + 1,
                                      QLocale.NarrowFormat) for i in range(7)]

        for month, box, title in self._months:
            if not box.adjusted(-4, -4, 4, 4).intersects(clip):
                continue
            h = self._anim.hover(month)
            count = self._index.month_shot_count(self._year, month) if self._index else 0
            p.setFont(_font(13, QFont.Bold))
            p.setPen(_lerp_color(_TEXT if count else _TEXT_DIM, _ACCENT, h))
            p.drawText(title, Qt.AlignLeft | Qt.AlignVCenter, _cal.month_name[month])
            if count:
                p.setFont(_font(10, QFont.DemiBold))
                p.setPen(_TEXT_DIM)
                p.drawText(title, Qt.AlignRight | Qt.AlignVCenter, f"{count:,}")
            if h > 0:
                p.setPen(QPen(_accent(int(h * 160)), 1.2))
                y = title.bottom() + 1
                p.drawLine(QPointF(title.x(), y), QPointF(title.x() + title.width() * h, y))

            p.setFont(_font(9, QFont.DemiBold))
            p.setPen(_TEXT_FAINT)
            wy = title.bottom() + 4
            for i, name in enumerate(wd_names):
                p.drawText(QRectF(title.x() + i * sq, wy, sq, self._WEEK_H - 4),
                           Qt.AlignCenter, name)

        p.setFont(_font(max(8, min(12, int(sq * 0.42))), QFont.DemiBold))
        for d, r in self._squares.items():
            if not r.intersects(clip):
                continue
            day = self._index.get(d) if self._index else None
            level = self._index.heat_level(day) if self._index else 0
            cell = r.adjusted(inset, inset, -inset, -inset)
            h = self._anim.hover(d)
            if h > 0 and day is not None:
                grow = h * inset * 1.4
                cell = cell.adjusted(-grow, -grow, grow, grow)
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(_HEAT_ALPHA[level]) if level else QColor(0x15, 0x15, 0x26))
            p.drawRoundedRect(cell, radius, radius)
            if d == today:
                p.setPen(QPen(_ACCENT, 1.4))
                p.setBrush(Qt.NoBrush)
                p.drawRoundedRect(cell.adjusted(-1, -1, 1, 1), radius + 1, radius + 1)
            if h > 0 and day is not None:
                p.setPen(QPen(QColor(255, 255, 255, int(h * 200)), 1.2))
                p.setBrush(Qt.NoBrush)
                p.drawRoundedRect(cell, radius, radius)
            if show_nums:
                p.setPen(QColor(0x14, 0x0a, 0x02) if level >= 3 else
                         (_TEXT if level else QColor(0x50, 0x50, 0x6a)))
                p.drawText(cell, Qt.AlignCenter, str(d.day))

        self._paint_legend(p)
        self._paint_empty_hint(p)
        self._paint_card(p)

    def _paint_legend(self, p: QPainter) -> None:
        s = 11
        y = self.height() - 22
        x = self.width() - 20 - (s + 3) * (HEAT_LEVELS + 1) - 70
        p.setFont(_font(10))
        p.setPen(_TEXT_DIM)
        p.drawText(QRectF(x - 40, y - 3, 36, s + 6), Qt.AlignRight | Qt.AlignVCenter, "Less")
        for lvl in range(HEAT_LEVELS + 1):
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(_HEAT_ALPHA[lvl]) if lvl else QColor(0x15, 0x15, 0x26))
            p.drawRoundedRect(QRectF(x + lvl * (s + 3), y, s, s), 2.5, 2.5)
        p.setPen(_TEXT_DIM)
        p.drawText(QRectF(x + (HEAT_LEVELS + 1) * (s + 3) + 4, y - 3, 60, s + 6),
                   Qt.AlignLeft | Qt.AlignVCenter, "More")

    def _paint_empty_hint(self, p: QPainter) -> None:
        self._pill = QRectF()
        if self._index is None or len(self._index) == 0:
            return
        if self._year in self._index.years():
            return
        text = f"No photos in {self._year}   ·   Jump to nearest  ›"
        p.setFont(_font(12, QFont.DemiBold))
        w = p.fontMetrics().horizontalAdvance(text) + 36
        self._pill = QRectF((self.width() - w) / 2, (self.height() - 34) / 2, w, 34)
        h = self._anim.hover("pill")
        p.setPen(QPen(_accent(int(_lerp(90, 200, h))), 1))
        p.setBrush(QColor(0x14, 0x14, 0x2a, 240))
        p.drawRoundedRect(self._pill, 17, 17)
        p.setPen(_lerp_color(QColor(0xc8, 0xc8, 0xd8), _ACCENT, h))
        p.drawText(self._pill, Qt.AlignCenter, text)

    def _paint_card(self, p: QPainter) -> None:
        """Floating preview card for the hovered day."""
        d = self._hover_key if isinstance(self._hover_key, _date) else None
        day = self._index.get(d) if (d and self._index) else None
        if day is None:
            self._card = QRectF()
            return
        h = self._anim.hover(d)
        sq = self._squares[d]
        cw, ch = 200.0, 176.0
        x = sq.center().x() - cw / 2
        y = sq.bottom() + 8
        if y + ch > self.height() - 4:
            y = sq.top() - ch - 8
        x = max(6.0, min(self.width() - cw - 6, x))
        self._card = QRectF(x, y, cw, ch)

        p.setOpacity(h)
        p.setPen(QPen(_accent(110), 1))
        p.setBrush(QColor(0x12, 0x12, 0x22, 250))
        p.drawRoundedRect(self._card, 10, 10)
        thumb = QRectF(x + 8, y + 8, cw - 16, ch - 56)
        rec = day.preview_records(1)[0]
        p.setOpacity(1.0)
        # Draw the thumb at card opacity × fade
        self._path_rects[rec.path] = thumb
        src = self._gen.get_cached(rec.path)
        if src is None:
            self._gen.request(rec, PRIORITY_VISIBLE)
            p.setOpacity(h)
            p.setPen(Qt.NoPen)
            p.setBrush(_TILE_EMPTY)
            p.drawRoundedRect(thumb, 6, 6)
        else:
            tile = self._covers.get(rec.path, src, int(thumb.width()), int(thumb.height()),
                                    self.devicePixelRatioF(), 6)
            p.setOpacity(h * self._anim.fade(rec.path))
            p.drawPixmap(thumb.topLeft(), tile)
        p.setOpacity(h)
        p.setFont(_font(12, QFont.Bold))
        p.setPen(_TEXT)
        p.drawText(QRectF(x + 10, y + ch - 44, cw - 20, 20), Qt.AlignLeft | Qt.AlignVCenter,
                   f"{d.strftime('%a')}, {_cal.month_abbr[d.month]} {d.day}")
        p.setFont(_font(11))
        p.setPen(_TEXT_DIM)
        p.drawText(QRectF(x + 10, y + ch - 24, cw - 20, 18), Qt.AlignLeft | Qt.AlignVCenter,
                   _plural(day.shot_count, "photo"))
        p.setPen(_ACCENT)
        p.drawText(QRectF(x + 10, y + ch - 24, cw - 20, 18), Qt.AlignRight | Qt.AlignVCenter,
                   "Open ›")
        p.setOpacity(1.0)

    # input ---------------------------------------------------------------
    def _hit(self, pos: QPointF):
        if not self._pill.isEmpty() and self._pill.contains(pos):
            return "pill"
        for d, r in self._squares.items():
            if r.contains(pos):
                return d
        for m, _, title in self._months:
            if title.contains(pos):
                return m
        return None

    def _set_hover(self, key) -> None:
        old_card = QRectF(self._card)
        super()._set_hover(key)
        if not old_card.isEmpty():
            self.update(old_card.toAlignedRect().adjusted(-4, -4, 4, 4))
        if isinstance(key, _date):
            # Card position depends on the new key — repaint where it lands.
            self.update()

    def mouseMoveEvent(self, ev) -> None:
        key = self._hit(ev.position())
        self._set_hover(key)
        self.setCursor(Qt.PointingHandCursor if key is not None else Qt.ArrowCursor)

    def mousePressEvent(self, ev) -> None:
        if ev.button() == Qt.LeftButton:
            self._pressed = self._hit(ev.position())
            self.setFocus()
        super().mousePressEvent(ev)

    def mouseReleaseEvent(self, ev) -> None:
        if ev.button() != Qt.LeftButton:
            return
        key = self._hit(ev.position())
        if key is None or key != self._pressed:
            return
        if key == "pill":
            self.jump_nearest.emit()
        elif isinstance(key, _date):
            if self._index and self._index.get(key) is not None:
                self.day_activated.emit(key, self._squares[key])
            else:
                self.month_activated.emit(key.month, self.month_rect(key.month))
        else:
            self.month_activated.emit(key, self.month_rect(key))

    def month_at(self, pos: QPointF) -> Optional[int]:
        for m, box, _ in self._months:
            if box.contains(pos):
                return m
        return None


class _DayPage(QWidget):
    """One day's photos in a read-only GroupedGridView."""

    def __init__(self, generator: ThumbnailGenerator, parent=None) -> None:
        super().__init__(parent)
        self.setStyleSheet("background: #0a0a12;")
        lay = QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        self.grid = GroupedGridView(generator, thumb_size=180, read_only=True)
        lay.addWidget(self.grid)
        self._loading = QLabel("Loading…", self)
        self._loading.setStyleSheet(
            "color:#6a6a8a; font-size:12px; background: rgba(20,20,42,0.92);"
            "border:1px solid rgba(255,109,0,0.25); border-radius: 12px; padding: 5px 14px;"
        )
        self._loading.hide()

    def set_loading(self, on: bool) -> None:
        self._loading.setVisible(on)
        if on:
            self._loading.adjustSize()
            self._loading.move((self.width() - self._loading.width()) // 2, 40)
            self._loading.raise_()


# ── header helpers ──────────────────────────────────────────────────────────

_NAV_QSS = (
    "QPushButton{background:transparent;color:#c8c8d8;border:none;"
    "border-radius:6px;font-size:18px;padding:0 0 3px 0;}"
    "QPushButton:hover{background:rgba(255,109,0,0.12);color:#ff6d00;}"
    "QPushButton:pressed{background:rgba(255,109,0,0.22);}"
    "QPushButton:disabled{color:#2e2e48;}"
)
_TEXT_BTN_QSS = (
    "QPushButton{background:rgba(255,109,0,0.06);color:#c8c8d8;"
    "border:1px solid rgba(255,109,0,0.18);border-radius:6px;"
    "font-size:11px;font-weight:600;padding:5px 12px;}"
    "QPushButton:hover{background:rgba(255,109,0,0.16);color:#ff6d00;"
    "border-color:rgba(255,109,0,0.45);}"
    "QPushButton:pressed{background:rgba(255,109,0,0.26);}"
)
_ACCENT_BTN_QSS = (
    "QPushButton{background:rgba(255,109,0,0.20);color:#ff6d00;"
    "border:1px solid rgba(255,109,0,0.60);border-radius:6px;"
    "font-size:11px;font-weight:700;padding:5px 14px;}"
    "QPushButton:hover{background:rgba(255,109,0,0.32);}"
    "QPushButton:pressed{background:rgba(255,109,0,0.45);}"
)
_SEG_QSS = (
    "QPushButton{background:transparent;color:#8888a8;border:none;"
    "font-size:11px;font-weight:700;padding:5px 14px;border-radius:5px;}"
    "QPushButton:hover{color:#f0f0f0;}"
    "QPushButton:checked{background:rgba(255,109,0,0.22);color:#ff6d00;}"
)


def _chip(bg: str, fg: str) -> QLabel:
    lbl = QLabel()
    lbl.setStyleSheet(
        f"QLabel {{ background: {bg}; color: {fg}; border-radius: 3px;"
        f" padding: 2px 8px; font-size: 11px; font-weight: 600; }}"
    )
    return lbl


# ── the view ────────────────────────────────────────────────────────────────

class CalendarView(QWidget):
    """
    Library browser by date: year ↔ month ↔ day.

    Signals
    -------
    item_activated(record, records, pair_lookup)
        A photo in the day view was opened.
    open_folder_requested(Path)
        "Open as working folder" on a day.
    library_folder_requested()
        The empty state's "Set library folder…" button.
    status_changed(str)
        Summary text for the main window's status bar.
    """

    item_activated           = Signal(object, object, object)
    open_folder_requested    = Signal(object)
    library_folder_requested = Signal()
    status_changed           = Signal(str)

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setStyleSheet("background: #0a0a12;")

        self._gen = ThumbnailGenerator(thumb_size=_GENERATION_SIZE,
                                       cache_bytes=_CACHE_BYTES, parent=self)
        self._covers = _CoverCache()

        self._root: Optional[Path] = None
        self._index: Optional[LibraryIndex] = None
        self._stale = True
        self._active = False
        self._worker: Optional[LibraryIndexWorker] = None
        self._day_workers: set = set()

        today = _date.today()
        self._mode = _MODE_MONTH
        self._year, self._month = today.year, today.month
        self._day: Optional[_date] = None
        self._position_restored = False     # restore_state() supplied a month

        # Opened days: date → (records, collection)
        self._day_cache: OrderedDict = OrderedDict()
        # A day zoom waits briefly for its records so it reveals a full grid
        self._pending: Optional[Tuple[_date, Callable[[], None]]] = None
        self._pending_timer = QTimer(self)
        self._pending_timer.setSingleShot(True)
        self._pending_timer.setInterval(_DAY_WAIT_MS)
        self._pending_timer.timeout.connect(self._flush_pending)

        self._build()

        self._prefetch_timer = QTimer(self)
        self._prefetch_timer.setSingleShot(True)
        self._prefetch_timer.setInterval(_SETTLE_MS)
        self._prefetch_timer.timeout.connect(self._prefetch)

        # Re-grab the incoming frame when thumbnails land mid-animation
        self._regrab_timer = QTimer(self)
        self._regrab_timer.setSingleShot(True)
        self._regrab_timer.setInterval(40)
        self._regrab_timer.timeout.connect(self._regrab)

        self._gen.thumbnail_ready.connect(self._on_thumbnail_ready)
        self._stage.overlay.finished.connect(self._on_transition_done)
        self._show_message()

    # ------------------------------------------------------------------ #
    # Construction                                                         #
    # ------------------------------------------------------------------ #

    def _build(self) -> None:
        lay = QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(0)

        bar = QFrame()
        bar.setObjectName("calendarBar")
        bar.setFixedHeight(54)
        bar.setStyleSheet(
            "QFrame#calendarBar { background: #14142a;"
            " border-bottom: 1px solid rgba(255,109,0,0.15); }"
        )
        h = QHBoxLayout(bar)
        h.setContentsMargins(10, 0, 12, 0)
        h.setSpacing(6)

        self._btn_back = QPushButton("‹  Back")
        self._btn_back.setStyleSheet(_TEXT_BTN_QSS)
        self._btn_back.setCursor(Qt.PointingHandCursor)
        self._btn_back.setToolTip("Back to month  (Esc)")
        self._btn_back.clicked.connect(self.go_back)

        self._btn_prev = QPushButton("‹")
        self._btn_next = QPushButton("›")
        for b in (self._btn_prev, self._btn_next):
            b.setFixedSize(32, 32)
            b.setStyleSheet(_NAV_QSS)
            b.setCursor(Qt.PointingHandCursor)
            b.setFocusPolicy(Qt.NoFocus)
        self._btn_prev.clicked.connect(lambda: self._step(-1))
        self._btn_next.clicked.connect(lambda: self._step(1))

        titles = QVBoxLayout()
        titles.setSpacing(0)
        titles.setContentsMargins(6, 0, 0, 0)
        self._title = QLabel()
        self._title.setStyleSheet("color:#f0f0f0; font-size:17px; font-weight:700;"
                                  " background: transparent;")
        self._subtitle = QLabel()
        self._subtitle.setStyleSheet("color:#7878a0; font-size:11px; background: transparent;")
        titles.addStretch()
        titles.addWidget(self._title)
        titles.addWidget(self._subtitle)
        titles.addStretch()

        # Day-mode chips + action
        self._chip_raw = _chip("#0d1a30", "#5a8acc")
        self._chip_jpg = _chip("#0d2014", "#4a9a5a")
        self._btn_open = QPushButton("Open as working folder")
        self._btn_open.setStyleSheet(_ACCENT_BTN_QSS)
        self._btn_open.setCursor(Qt.PointingHandCursor)
        self._btn_open.clicked.connect(self._on_open_folder)

        # Month | Year segmented toggle
        seg = QFrame()
        seg.setObjectName("calendarSeg")
        seg.setStyleSheet("QFrame#calendarSeg { background: #0d0d1e;"
                          " border: 1px solid rgba(255,109,0,0.18); border-radius: 7px; }")
        sl = QHBoxLayout(seg)
        sl.setContentsMargins(2, 2, 2, 2)
        sl.setSpacing(2)
        self._btn_month = QPushButton("Month")
        self._btn_year = QPushButton("Year")
        self._seg_group = QButtonGroup(self)
        for b in (self._btn_month, self._btn_year):
            b.setCheckable(True)
            b.setStyleSheet(_SEG_QSS)
            b.setCursor(Qt.PointingHandCursor)
            b.setFocusPolicy(Qt.NoFocus)
            self._seg_group.addButton(b)
            sl.addWidget(b)
        self._btn_month.setChecked(True)
        self._btn_month.clicked.connect(lambda: self._set_zoom_mode(_MODE_MONTH))
        self._btn_year.clicked.connect(lambda: self._set_zoom_mode(_MODE_YEAR))
        self._seg = seg

        self._btn_today = QPushButton("Today")
        self._btn_today.setStyleSheet(_TEXT_BTN_QSS)
        self._btn_today.setCursor(Qt.PointingHandCursor)
        self._btn_today.setFocusPolicy(Qt.NoFocus)
        self._btn_today.setToolTip("Go to today  (T)")
        self._btn_today.clicked.connect(self.go_today)

        self._btn_refresh = QPushButton("⟳")
        self._btn_refresh.setFixedSize(32, 32)
        self._btn_refresh.setStyleSheet(_NAV_QSS)
        self._btn_refresh.setCursor(Qt.PointingHandCursor)
        self._btn_refresh.setFocusPolicy(Qt.NoFocus)
        self._btn_refresh.setToolTip("Re-read the library folder  (F5)")
        self._btn_refresh.clicked.connect(self.refresh)

        seg.setFixedHeight(32)
        for w in (self._btn_back, self._btn_open, self._btn_today):
            w.setFixedHeight(30)
        for w in (self._chip_raw, self._chip_jpg):
            w.setFixedHeight(22)

        v = Qt.AlignVCenter
        h.addWidget(self._btn_back, 0, v)
        h.addWidget(self._btn_prev, 0, v)
        h.addWidget(self._btn_next, 0, v)
        h.addLayout(titles)
        h.addStretch()
        h.addWidget(self._chip_raw, 0, v)
        h.addWidget(self._chip_jpg, 0, v)
        h.addSpacing(6)
        h.addWidget(self._btn_open, 0, v)
        h.addWidget(seg, 0, v)
        h.addSpacing(4)
        h.addWidget(self._btn_today, 0, v)
        h.addWidget(self._btn_refresh, 0, v)
        lay.addWidget(bar)
        self._bar = bar

        self._stage = _Stage()
        lay.addWidget(self._stage, 1)
        stack = self._stage.stack

        self._msg_page = _MessagePage()
        self._year_page = _YearPage(self._gen, self._covers)
        self._month_page = _MonthPage(self._gen, self._covers)
        self._day_page = _DayPage(self._gen)
        for w in (self._msg_page, self._year_page, self._month_page, self._day_page):
            stack.addWidget(w)

        self._msg_page.action.connect(self._on_message_action)

        self._month_page.step.connect(self._step)
        self._month_page.zoom.connect(self._on_month_zoom)
        self._month_page.day_activated.connect(self._open_day_from)
        self._month_page.month_jump.connect(self._jump_to_month)
        self._month_page.jump_nearest.connect(self._jump_nearest)

        self._year_page.step.connect(self._step)
        self._year_page.zoom.connect(self._on_year_zoom)
        self._year_page.day_activated.connect(self._open_day_from)
        self._year_page.month_activated.connect(self._zoom_into_month)
        self._year_page.jump_nearest.connect(self._jump_nearest)

        self._day_page.grid.item_activated.connect(self._on_day_item_activated)

        # Keyboard.  Scoped to this view; the paged keys are disabled in day
        # mode so the grid keeps its arrow keys.
        def sc(key, fn, paged=True):
            s = QShortcut(QKeySequence(key), self)
            s.setContext(Qt.WidgetWithChildrenShortcut)
            s.activated.connect(fn)
            (self._paged_shortcuts if paged else self._day_shortcuts).append(s)
            return s

        self._paged_shortcuts: List[QShortcut] = []
        self._day_shortcuts: List[QShortcut] = []
        for key in (Qt.Key_Left, Qt.Key_PageUp):
            sc(key, lambda: self._step(-1))
        for key in (Qt.Key_Right, Qt.Key_PageDown):
            sc(key, lambda: self._step(1))
        sc(Qt.Key_T, self.go_today)
        sc(Qt.Key_Home, self.go_today)
        sc(Qt.Key_Minus, lambda: self._zoom_step(-1))
        sc(Qt.Key_Plus, lambda: self._zoom_step(1))
        sc(Qt.Key_Equal, lambda: self._zoom_step(1))
        sc(Qt.Key_Return, lambda: self._zoom_step(1))
        sc(Qt.Key_Enter, lambda: self._zoom_step(1))
        sc(Qt.Key_Escape, lambda: self._zoom_step(-1))
        for key in (Qt.Key_Escape, Qt.Key_Backspace):
            sc(key, self.go_back, paged=False)
        sc(Qt.Key_BracketLeft, lambda: self._step(-1), paged=False)
        sc(Qt.Key_BracketRight, lambda: self._step(1), paged=False)
        f5 = QShortcut(QKeySequence(Qt.Key_F5), self)
        f5.setContext(Qt.WidgetWithChildrenShortcut)
        f5.activated.connect(self.refresh)

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    def set_library_root(self, root: Optional[Path]) -> None:
        if root == self._root:
            return
        self._cancel_worker()
        self._root = root
        self._index = None
        self._stale = True
        self._day_cache.clear()
        self._gen.clear()
        self._covers.clear()
        if self._mode == _MODE_DAY:
            self._mode = _MODE_MONTH
        if not self._position_restored:
            today = _date.today()
            self._year, self._month = today.year, today.month
        self._show_message()
        if self._active:
            self.activate()

    def library_root(self) -> Optional[Path]:
        return self._root

    def activate(self) -> None:
        """The view became visible: index the library if needed, take focus."""
        self._active = True
        if self._root is not None and self._stale and self._worker is None:
            self._start_index()
        self._focus_page()
        self.status_changed.emit(self.status_text())

    def deactivate(self) -> None:
        self._active = False

    def mark_stale(self) -> None:
        """The library changed on disk (e.g. after an export)."""
        self._stale = True
        self._day_cache.clear()
        if self._active and self._worker is None and self._root is not None:
            self._start_index()

    def refresh(self) -> None:
        self.mark_stale()

    def status_text(self) -> str:
        if self._root is None:
            return "Calendar — no library folder set"
        if self._index is None:
            return "Calendar — indexing library…"
        ix = self._index
        return (f"Library — {_plural(ix.total_shots, 'photo')} across "
                f"{_plural(len(ix), 'day')}, {ix.years()[0]}–{ix.years()[-1]}"
                if len(ix) else "Library — no photos found")

    def save_state(self) -> dict:
        mode = _MODE_MONTH if self._mode == _MODE_DAY else self._mode
        return {"mode": mode, "year": self._year, "month": self._month}

    def restore_state(self, state: dict) -> None:
        try:
            year, month = int(state.get("year", 0)), int(state.get("month", 0))
        except (TypeError, ValueError):
            return
        if 1 <= month <= 12 and 1900 < year < 3000:
            self._year, self._month = year, month
            self._position_restored = True
        if state.get("mode") in (_MODE_YEAR, _MODE_MONTH):
            self._mode = state["mode"]

    def shutdown(self) -> None:
        self._cancel_worker(wait=True)
        for w in list(self._day_workers):
            w.wait(2000)
        self._gen.clear_queue()
        self._gen.shutdown()

    # ------------------------------------------------------------------ #
    # Indexing                                                             #
    # ------------------------------------------------------------------ #

    def _start_index(self) -> None:
        self._stale = False
        worker = LibraryIndexWorker(self._root)
        worker.progress.connect(self._on_index_progress)
        worker.index_ready.connect(self._on_index_ready)
        worker.index_error.connect(self._on_index_error)
        worker.finished.connect(lambda w=worker: self._on_worker_finished(w))
        self._worker = worker
        if self._index is None:
            self._show_message()
        worker.start()

    def _cancel_worker(self, wait: bool = False) -> None:
        if self._worker is not None:
            self._worker.cancel()
            try:
                self._worker.index_ready.disconnect(self._on_index_ready)
            except (RuntimeError, TypeError):
                pass
            if wait:
                self._worker.wait(3000)
            else:
                # Keep a reference until the thread exits
                self._day_workers.add(self._worker)
            self._worker = None

    def _on_worker_finished(self, worker) -> None:
        if worker is self._worker:
            self._worker = None
            if self._stale and self._active and self._root is not None:
                self._start_index()      # changed again while indexing
        self._day_workers.discard(worker)
        worker.deleteLater()

    def _on_index_progress(self, n: int) -> None:
        if self._index is None:
            self._msg_page.set_message("Indexing library…", _plural(n, "file") + " found")

    def _on_index_error(self, msg: str) -> None:
        self._msg_page.set_message("Couldn't read the library", msg, "Try again")
        self._stale = True
        self._stage.stack.setCurrentWidget(self._msg_page)

    def _on_index_ready(self, index: LibraryIndex) -> None:
        if index.root != self._root:
            return
        first = self._index is None
        self._index = index
        self._covers.clear()
        if first and not self._position_restored:
            latest = index.latest_day()
            if latest is not None:
                self._year, self._month = latest.year, latest.month
        if len(index) == 0:
            self._show_message()
        else:
            # A fresh index for the open day reloads its records too
            if self._mode == _MODE_DAY and self._day not in index.days:
                self._mode = _MODE_MONTH
            self._apply_state()
        self.status_changed.emit(self.status_text())

    # ------------------------------------------------------------------ #
    # Page display                                                         #
    # ------------------------------------------------------------------ #

    def _show_message(self) -> None:
        if self._root is None:
            self._msg_page.set_message(
                "No library folder set",
                "The calendar shows photos exported to your library.\n"
                "Choose the library folder to browse it by date.",
                "Set library folder…",
            )
        elif self._index is None:
            self._msg_page.set_message("Indexing library…")
        else:
            self._msg_page.set_message(
                "The library is empty",
                f"No photos were found in\n{self._root}\n\nExport a shoot (Ctrl+E) to fill it.",
                "Re-scan",
            )
        self._stage.overlay.stop()
        self._stage.stack.setCurrentWidget(self._msg_page)
        self._update_header()

    def _on_message_action(self) -> None:
        if self._root is None:
            self.library_folder_requested.emit()
        else:
            self.refresh()

    def _apply_state(self) -> None:
        """Push the current mode/position into the pages and show one."""
        if self._index is None or len(self._index) == 0:
            self._show_message()
            return
        stack = self._stage.stack
        if self._mode == _MODE_YEAR:
            self._year_page.set_year(self._index, self._year)
            stack.setCurrentWidget(self._year_page)
        elif self._mode == _MODE_MONTH:
            self._month_page.set_month(self._index, self._year, self._month)
            stack.setCurrentWidget(self._month_page)
        else:
            self._load_day_into_page(self._day)
            stack.setCurrentWidget(self._day_page)
        self._update_header()
        self._focus_page()
        self._prefetch_timer.start()

    def _focus_page(self) -> None:
        w = self._stage.stack.currentWidget()
        if w is self._day_page:
            self._day_page.grid.setFocus()
        elif w is not None and self.isVisible():
            w.setFocus()

    def _can_animate(self) -> bool:
        return (self.isVisible() and self._stage.width() > 0
                and self._index is not None and len(self._index) > 0
                and self._stage.stack.currentWidget() is not self._msg_page)

    def _change(self, apply: Callable[[], None], kind: str = "slide",
                direction: int = 1, rect: QRectF = QRectF(), zoom_in: bool = True) -> None:
        """Apply a navigation change, animating between before and after."""
        # Any navigation supersedes a day zoom still waiting for its records
        self._pending = None
        self._pending_timer.stop()
        overlay = self._stage.overlay
        if not self._can_animate():
            apply()
            self._apply_state()
            return
        old = overlay.snapshot() or self._stage.stack.currentWidget().grab()
        apply()
        self._apply_state()
        new = self._stage.stack.currentWidget().grab()
        if kind == "slide":
            overlay.slide(old, new, direction)
        else:
            overlay.zoom(old, new, rect, zoom_in)

    def _on_transition_done(self) -> None:
        self._focus_page()

    def _on_thumbnail_ready(self, path: Path, _pm) -> None:
        page = self._stage.stack.currentWidget()
        hit = False
        if page is self._month_page or page is self._year_page:
            hit = page.thumbnail_arrived(path)
        elif page is self._day_page:
            hit = True
        if hit and self._stage.overlay.running():
            self._regrab_timer.start()

    def _regrab(self) -> None:
        if self._stage.overlay.running():
            self._stage.overlay.set_new(self._stage.stack.currentWidget().grab())

    # ------------------------------------------------------------------ #
    # Header                                                               #
    # ------------------------------------------------------------------ #

    def _update_header(self) -> None:
        ix = self._index
        has = ix is not None and len(ix) > 0
        mode = self._mode if has else None
        is_day = mode == _MODE_DAY

        self._btn_back.setVisible(is_day)
        self._chip_raw.setVisible(is_day)
        self._chip_jpg.setVisible(is_day)
        self._btn_open.setVisible(is_day)
        self._seg.setVisible(has and not is_day)
        self._btn_today.setVisible(has and not is_day)
        self._btn_prev.setVisible(has)
        self._btn_next.setVisible(has)
        self._btn_refresh.setVisible(self._root is not None)
        # Escape means "zoom out" on the paged views and "back" on a day, and
        # the grid needs its arrow keys — so only one set is live at a time.
        for sc in self._paged_shortcuts:
            sc.setEnabled(not is_day)
        for sc in self._day_shortcuts:
            sc.setEnabled(is_day)

        if not has:
            self._title.setText("Calendar")
            self._subtitle.setText(str(self._root) if self._root else "No library folder set")
            return

        if mode == _MODE_YEAR:
            self._btn_year.setChecked(True)
            self._title.setText(str(self._year))
            n = ix.year_shot_count(self._year)
            days = sum(1 for d in ix.days if d.year == self._year)
            self._subtitle.setText(f"{_plural(n, 'photo')} · {_plural(days, 'day')}"
                                   if n else "No photos")
            self._btn_prev.setToolTip("Previous year  (←)")
            self._btn_next.setToolTip("Next year  (→)")
        elif mode == _MODE_MONTH:
            self._btn_month.setChecked(True)
            self._title.setText(f"{_cal.month_name[self._month]} {self._year}")
            days = ix.days_in_month(self._year, self._month)
            n = sum(d.shot_count for d in days.values())
            self._subtitle.setText(f"{_plural(n, 'photo')} · {_plural(len(days), 'day')}"
                                   if n else "No photos")
            self._btn_prev.setToolTip("Previous month  (←)")
            self._btn_next.setToolTip("Next month  (→)")
        else:
            d = self._day
            day = ix.get(d)
            self._btn_back.setText(f"‹  {_cal.month_abbr[d.month]} {d.year}")
            self._title.setText(f"{d.strftime('%A')}, {_cal.month_name[d.month]} {d.day}, {d.year}")
            self._subtitle.setText(_plural(day.shot_count, "photo") if day else "")
            if day is not None:
                self._chip_raw.setText(f"{day.raw_count}  RAW")
                self._chip_jpg.setText(f"{day.jpg_count}  JPG")
                self._chip_raw.setVisible(day.raw_count > 0)
                self._chip_jpg.setVisible(day.jpg_count > 0)
                self._btn_open.setToolTip(f"Load {day.folder} as the working folder")
            self._btn_prev.setEnabled(ix.adjacent_day(d, -1) is not None)
            self._btn_next.setEnabled(ix.adjacent_day(d, 1) is not None)
            self._btn_prev.setToolTip("Previous day with photos  ([)")
            self._btn_next.setToolTip("Next day with photos  (])")
            return
        self._btn_prev.setEnabled(True)
        self._btn_next.setEnabled(True)

    # ------------------------------------------------------------------ #
    # Navigation                                                           #
    # ------------------------------------------------------------------ #

    def _step(self, delta: int) -> None:
        if self._index is None or len(self._index) == 0:
            return
        if self._mode == _MODE_MONTH:
            def apply():
                self._year, self._month = _month_shift(self._year, self._month, delta)
            self._change(apply, "slide", delta)
        elif self._mode == _MODE_YEAR:
            def apply():
                self._year += delta
            self._change(apply, "slide", delta)
        else:
            target = self._index.adjacent_day(self._day, delta)
            if target is not None:
                self._go_day(target, "slide", direction=delta)

    def go_today(self) -> None:
        if self._index is None or self._mode == _MODE_DAY:
            return
        today = _date.today()
        if self._mode == _MODE_YEAR:
            if today.year != self._year:
                d = 1 if today.year > self._year else -1
                self._change(lambda: setattr(self, "_year", today.year), "slide", d)
            return
        if (today.year, today.month) != (self._year, self._month):
            self._jump_to_month(today.year, today.month)

    def _jump_to_month(self, year: int, month: int) -> None:
        d = 1 if (year, month) > (self._year, self._month) else -1

        def apply():
            self._year, self._month = year, month
        self._change(apply, "slide", d)

    def _jump_nearest(self) -> None:
        if self._index is None:
            return
        if self._mode == _MODE_YEAR:
            ref = _date(self._year, 7, 1)
        else:
            ref = _date(self._year, self._month, 15)
        near = self._index.nearest_day(ref)
        if near is None:
            return
        if self._mode == _MODE_YEAR:
            d = 1 if near.year > self._year else -1
            self._change(lambda: setattr(self, "_year", near.year), "slide", d)
        else:
            self._jump_to_month(near.year, near.month)

    def _set_zoom_mode(self, mode: str) -> None:
        if mode == self._mode:
            return
        if mode == _MODE_YEAR:
            self._zoom_out_to_year()
        else:
            self._zoom_into_month(self._month, self._year_page.month_rect(self._month))

    def _zoom_step(self, direction: int) -> None:
        """Keyboard zoom: -1 out (day → month → year), +1 in."""
        if self._index is None or len(self._index) == 0:
            return
        if direction < 0:
            if self._mode == _MODE_MONTH:
                self._zoom_out_to_year()
            elif self._mode == _MODE_DAY:
                self.go_back()
        elif self._mode == _MODE_YEAR:
            self._zoom_into_month(self._month, self._year_page.month_rect(self._month))
        elif self._mode == _MODE_MONTH:
            d = self._month_page.hovered_day()
            if d is not None and d.month == self._month and self._index.get(d) is not None:
                self._open_day_from(d, self._month_page.cell_rect(d))

    def _on_month_zoom(self, direction: int, pos: QPointF) -> None:
        if direction < 0:
            self._zoom_out_to_year()
            return
        d = self._month_page.hovered_day()
        if d is not None and d.month == self._month and self._index.get(d) is not None:
            self._open_day_from(d, self._month_page.cell_rect(d))

    def _on_year_zoom(self, direction: int, pos: QPointF) -> None:
        if direction > 0:
            month = self._year_page.month_at(pos) or self._month
            self._zoom_into_month(month, self._year_page.month_rect(month))

    def _zoom_out_to_year(self) -> None:
        month = self._month

        def apply():
            self._mode = _MODE_YEAR
        # Rect of this month in the year page — lay it out first so it's known
        self._year_page.set_year(self._index, self._year)
        self._change(apply, "zoom", rect=self._year_page.month_rect(month), zoom_in=False)

    def _zoom_into_month(self, month: int, rect: QRectF) -> None:
        def apply():
            self._mode = _MODE_MONTH
            self._month = month
        self._change(apply, "zoom", rect=rect, zoom_in=True)

    def go_back(self) -> None:
        """Day → its month, zooming back into the day's cell."""
        if self._mode != _MODE_DAY:
            return
        d = self._day
        self._pending = None
        self._pending_timer.stop()

        def apply():
            self._mode = _MODE_MONTH
            self._year, self._month = d.year, d.month
        self._month_page.set_month(self._index, d.year, d.month)
        self._change(apply, "zoom", rect=self._month_page.cell_rect(d), zoom_in=False)

    # ------------------------------------------------------------------ #
    # Day view                                                             #
    # ------------------------------------------------------------------ #

    def _open_day_from(self, d: _date, rect: QRectF) -> None:
        self._go_day(d, "zoom", rect=rect)

    def _go_day(self, d: _date, kind: str, direction: int = 1,
                rect: QRectF = QRectF()) -> None:
        def apply():
            self._mode = _MODE_DAY
            self._day = d
            self._year, self._month = d.year, d.month

        def run():
            self._change(apply, kind, direction=direction, rect=rect, zoom_in=True)

        if d in self._day_cache or not self._can_animate():
            run()
            return
        # Start loading now, and hold the transition until the records land
        # (or _DAY_WAIT_MS passes) so the zoom reveals a filled grid.
        self._request_day(d)
        self._pending = (d, run)
        self._pending_timer.start()

    def _flush_pending(self) -> None:
        if self._pending is not None:
            _, run = self._pending
            self._pending = None
            self._pending_timer.stop()
            run()

    def _request_day(self, d: _date) -> None:
        day = self._index.get(d) if self._index else None
        if day is None:
            return
        worker = DayRecordsWorker(d, day.files)
        worker.records_ready.connect(self._on_day_records)
        worker.finished.connect(lambda w=worker: (self._day_workers.discard(w), w.deleteLater()))
        self._day_workers.add(worker)
        worker.start()

    def _on_day_records(self, d: _date, records: List[PhotoRecord]) -> None:
        # The library folder is the authority on which day a file belongs to
        # (export chose it with pair-aware rules).  A file with no EXIF time
        # would otherwise fall back to its mtime — often the copy date — and
        # the grid would file it under the wrong day.  Keep the mtime's
        # time-of-day so in-day ordering still roughly holds.
        for r in records:
            if r.capture_time is None and r.modified_time.date() != d:
                r.capture_time = datetime.combine(d, r.modified_time.time())
        coll = PhotoCollection()
        for r in records:
            coll.add(r)
        coll.build_pairs(auto_detect=True)   # export keeps pair stems equal
        self._day_cache[d] = (coll.all(), coll)
        self._day_cache.move_to_end(d)
        while len(self._day_cache) > _DAY_CACHE:
            self._day_cache.popitem(last=False)

        if self._pending is not None and self._pending[0] == d:
            self._flush_pending()
        elif self._mode == _MODE_DAY and self._day == d:
            self._load_day_into_page(d)

    def _load_day_into_page(self, d: Optional[_date]) -> None:
        grid = self._day_page.grid
        entry = self._day_cache.get(d)
        if entry is None:
            grid.reset_records([])
            self._day_page.set_loading(True)
            self._request_day(d)
            return
        self._day_page.set_loading(False)
        records, _ = entry
        if [r.path for r in grid.source_model().records_in_order()] != [r.path for r in records]:
            grid.reset_records(records)
            grid.verticalScrollBar().setValue(0)

    def _on_day_item_activated(self, record: PhotoRecord) -> None:
        entry = self._day_cache.get(self._day)
        lookup = entry[1].find_pair if entry else None
        self.item_activated.emit(record, self._day_page.grid.all_visible_records(), lookup)

    def _on_open_folder(self) -> None:
        day = self._index.get(self._day) if (self._index and self._day) else None
        if day is not None and day.folder is not None:
            self.open_folder_requested.emit(day.folder)

    # ------------------------------------------------------------------ #
    # Prefetch                                                             #
    # ------------------------------------------------------------------ #

    def _prefetch(self) -> None:
        """Visible month first, then its neighbours, once navigation settles."""
        if self._index is None or self._mode != _MODE_MONTH:
            return
        self._gen.clear_queue()
        for r in self._month_page.preview_records(self._year, self._month):
            self._gen.request(r, PRIORITY_VISIBLE)
        for delta in (1, -1):
            y, m = _month_shift(self._year, self._month, delta)
            for r in self._month_page.preview_records(y, m):
                self._gen.request(r, PRIORITY_PREFETCH)
