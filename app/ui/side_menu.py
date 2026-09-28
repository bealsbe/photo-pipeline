"""
SideMenu — the left navigation rail, expandable like a hamburger menu.

Collapsed it is a 48 px strip of icons.  The ☰ button at the top slides it
open to show each command's name and shortcut; choosing a command, clicking
outside, pressing Escape or clicking ☰ again slides it shut.

Why it overlays instead of pushing the content aside
----------------------------------------------------
Widening a docked sidebar resizes everything to its right on every animation
frame, and the grouped grid re-flows thousands of cells per resize — the
drawer would stutter exactly where it should feel smooth.  So the rail keeps a
fixed 48 px slot in the layout (placeholder()) and the panel itself floats
above the content, widening over it behind a fading scrim.  Nothing under it
re-lays out.

The buttons are QToolButtons driven by setDefaultAction(), so checked /
enabled / tooltip state follows the QActions — the same ones the menu bar
uses, which keeps each shortcut bound exactly once.
"""
from __future__ import annotations

from typing import List, Optional

from PySide6.QtCore import (
    QEasingCurve,
    QEvent,
    QObject,
    QPointF,
    QRect,
    QRectF,
    QSize,
    Qt,
    QVariantAnimation,
    Signal,
)
from PySide6.QtGui import (
    QAction,
    QColor,
    QFont,
    QFontMetrics,
    QIcon,
    QKeySequence,
    QPainter,
    QPen,
    QShortcut,
)
from PySide6.QtWidgets import (
    QFrame,
    QSizePolicy,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

RAIL_W    = 48          # collapsed width
_MARGIN   = 4           # panel inner margin; buttons are RAIL_W - 2·_MARGIN wide
_BTN_H    = 40
_ICON     = 18
_TEXT_X   = 42          # label start, in button coordinates
_ANIM_MS  = 220

_BG        = QColor(0x14, 0x14, 0x2a)
_TEXT      = QColor(0xc8, 0xc8, 0xd8)
_TEXT_OFF  = QColor(0x50, 0x50, 0x70)
_HINT      = QColor(0x58, 0x58, 0x7a)
_ACCENT    = QColor(0xff, 0x6d, 0x00)


def _accent(alpha: int) -> QColor:
    c = QColor(_ACCENT)
    c.setAlpha(alpha)
    return c


def _font(px: int, weight: QFont.Weight = QFont.Normal) -> QFont:
    f = QFont()
    f.setPixelSize(px)
    f.setWeight(weight)
    return f


def _with_alpha(c: QColor, a: float) -> QColor:
    c = QColor(c)
    c.setAlphaF(c.alphaF() * max(0.0, min(1.0, a)))
    return c


# ── pieces ────────────────────────────────────────────────────────────────────

class _MenuButton(QToolButton):
    """Icon on the rail, label + shortcut revealed as the panel widens."""

    def __init__(self, action: QAction, hint: str, menu: "SideMenu") -> None:
        super().__init__(menu)
        self._menu = menu
        self._hint = hint
        self._hover = False
        self.setDefaultAction(action)
        self.setFixedHeight(_BTN_H)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.setCursor(Qt.PointingHandCursor)
        self.setFocusPolicy(Qt.NoFocus)     # keep keyboard focus on the grid

    def label(self) -> str:
        # iconText() is text() without the & mnemonic or trailing "…"
        return self.defaultAction().iconText()

    def hint(self) -> str:
        return self._hint

    def sizeHint(self) -> QSize:
        return QSize(RAIL_W - 2 * _MARGIN, _BTN_H)

    def minimumSizeHint(self) -> QSize:
        return self.sizeHint()

    def event(self, ev) -> bool:
        # With the labels showing, a tooltip only repeats them.
        if ev.type() == QEvent.ToolTip and self._menu.progress() > 0.5:
            ev.ignore()
            return True
        return super().event(ev)

    def enterEvent(self, ev) -> None:
        self._hover = True
        self.update()
        super().enterEvent(ev)

    def leaveEvent(self, ev) -> None:
        self._hover = False
        self.update()
        super().leaveEvent(ev)

    def paintEvent(self, _ev) -> None:
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        r = QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5)
        enabled = self.isEnabled()
        checked = self.isChecked()

        if checked:
            p.setPen(QPen(_accent(115), 1))
            p.setBrush(_accent(38))
        elif self.isDown() and enabled:
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(46))
        elif self._hover and enabled:
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(26))
        else:
            p.setPen(Qt.NoPen)
            p.setBrush(Qt.NoBrush)
        p.drawRoundedRect(r, 6, 6)

        mode = QIcon.Normal if enabled else QIcon.Disabled
        pm = self.icon().pixmap(QSize(_ICON, _ICON), self.devicePixelRatioF(), mode)
        x = (RAIL_W - 2 * _MARGIN - _ICON) / 2
        p.drawPixmap(QPointF(x, (self.height() - _ICON) / 2), pm)

        t = self._menu.progress()
        if t <= 0.0:
            return
        # Labels fade in over the back half of the slide, so the first frames
        # don't show a word chopped off by the panel edge.
        a = max(0.0, (t - 0.35) / 0.65)
        w = self.width()
        hint_w = 0
        if self._hint:
            p.setFont(_font(10, QFont.DemiBold))
            hint_w = p.fontMetrics().horizontalAdvance(self._hint) + 12
            p.setPen(_with_alpha(_HINT if enabled else _TEXT_OFF, a))
            p.drawText(QRectF(w - hint_w - 6, 0, hint_w, self.height()),
                       Qt.AlignRight | Qt.AlignVCenter, self._hint)
        p.setFont(_font(12, QFont.DemiBold if checked else QFont.Normal))
        colour = _ACCENT if checked else (_TEXT if enabled else _TEXT_OFF)
        p.setPen(_with_alpha(colour, a))
        room = max(0, w - _TEXT_X - hint_w - 8)
        # Clip rather than elide: mid-slide the label is uncovered by the
        # panel edge, and an ellipsis that jumps along with it looks broken.
        p.setClipRect(QRectF(_TEXT_X, 0, room, self.height()))
        p.drawText(QRectF(_TEXT_X, 0, max(room, 400), self.height()),
                   Qt.AlignLeft | Qt.AlignVCenter, self.label())


class _BurgerButton(QToolButton):
    """☰ that morphs into a ← while the panel opens, plus the app name."""

    def __init__(self, menu: "SideMenu") -> None:
        super().__init__(menu)
        self._menu = menu
        self._hover = False
        self.setFixedHeight(_BTN_H)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.setCursor(Qt.PointingHandCursor)
        self.setFocusPolicy(Qt.NoFocus)
        self.setToolTip("Show menu labels  (Ctrl+\\)")

    def sizeHint(self) -> QSize:
        return QSize(RAIL_W - 2 * _MARGIN, _BTN_H)

    def event(self, ev) -> bool:
        if ev.type() == QEvent.ToolTip and self._menu.progress() > 0.5:
            ev.ignore()
            return True
        return super().event(ev)

    def enterEvent(self, ev) -> None:
        self._hover = True
        self.update()
        super().enterEvent(ev)

    def leaveEvent(self, ev) -> None:
        self._hover = False
        self.update()
        super().leaveEvent(ev)

    def paintEvent(self, _ev) -> None:
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        if self._hover or self.isDown():
            p.setPen(Qt.NoPen)
            p.setBrush(_accent(46 if self.isDown() else 26))
            p.drawRoundedRect(QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5), 6, 6)

        t = self._menu.progress()
        cx = (RAIL_W - 2 * _MARGIN) / 2
        cy = self.height() / 2

        def lerp(a: QPointF, b: QPointF) -> QPointF:
            return QPointF(a.x() + (b.x() - a.x()) * t, a.y() + (b.y() - a.y()) * t)

        # (hamburger endpoints) → (left-arrow endpoints)
        lines = [
            ((QPointF(cx - 8, cy - 5), QPointF(cx + 8, cy - 5)),
             (QPointF(cx - 7, cy),     QPointF(cx - 1, cy - 6))),
            ((QPointF(cx - 8, cy),     QPointF(cx + 8, cy)),
             (QPointF(cx - 7, cy),     QPointF(cx + 7, cy))),
            ((QPointF(cx - 8, cy + 5), QPointF(cx + 8, cy + 5)),
             (QPointF(cx - 7, cy),     QPointF(cx - 1, cy + 6))),
        ]
        colour = _ACCENT if (self._hover or t > 0) else _TEXT
        pen = QPen(colour, 1.8)
        pen.setCapStyle(Qt.RoundCap)
        p.setPen(pen)
        for (a0, a1), (b0, b1) in lines:
            p.drawLine(lerp(a0, b0), lerp(a1, b1))

        if t > 0:
            a = max(0.0, (t - 0.35) / 0.65)
            p.setFont(_font(13, QFont.Bold))
            p.setPen(_with_alpha(QColor(0xf0, 0xf0, 0xf0), a))
            p.drawText(QRectF(_TEXT_X, 0, self.width() - _TEXT_X - 6, self.height()),
                       Qt.AlignLeft | Qt.AlignVCenter, "Photo Pipeline")


class _Separator(QWidget):
    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setFixedHeight(9)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)

    def paintEvent(self, _ev) -> None:
        p = QPainter(self)
        p.setPen(QPen(_accent(31), 1))
        y = self.height() // 2
        p.drawLine(6, y, self.width() - 6, y)


class _Scrim(QWidget):
    """Dims the content under the open panel; a click on it closes the panel."""

    clicked = Signal()

    def __init__(self, parent: QWidget) -> None:
        super().__init__(parent)
        self._t = 0.0
        self.hide()

    def set_progress(self, t: float) -> None:
        self._t = t
        self.update()

    def paintEvent(self, _ev) -> None:
        p = QPainter(self)
        p.fillRect(self.rect(), QColor(4, 4, 10, int(140 * self._t)))

    def mousePressEvent(self, ev) -> None:
        ev.accept()
        self.clicked.emit()

    def wheelEvent(self, ev) -> None:
        ev.accept()


# ── the menu ──────────────────────────────────────────────────────────────────

class SideMenu(QFrame):
    """
    Collapsible navigation rail.  *parent* must be the widget whose layout
    holds placeholder(); the panel and its scrim float over that widget.

    Signals
    -------
    expanded_changed(bool)
    """

    expanded_changed = Signal(bool)

    def __init__(self, parent: QWidget) -> None:
        super().__init__(parent)
        self.setObjectName("sideMenu")
        self._host = parent
        self._t = 0.0
        self._expanded = False
        self._buttons: List[_MenuButton] = []
        self._open_w: Optional[int] = None      # cached expanded width

        self._placeholder = QWidget()
        self._placeholder.setFixedWidth(RAIL_W)

        self._lay = QVBoxLayout(self)
        self._lay.setContentsMargins(_MARGIN, 6, _MARGIN, 6)
        self._lay.setSpacing(3)
        self._burger = _BurgerButton(self)
        self._burger.clicked.connect(self.toggle)
        self._lay.addWidget(self._burger)
        self._lay.addWidget(_Separator(self))
        self._lay.addStretch(1)

        self._scrim = _Scrim(parent)
        self._scrim.clicked.connect(self.collapse)

        self._anim = QVariantAnimation(self)
        self._anim.setDuration(_ANIM_MS)
        self._anim.setEasingCurve(QEasingCurve.OutCubic)
        self._anim.valueChanged.connect(self._set_progress)
        self._anim.finished.connect(self._on_anim_finished)

        # Escape closes the open panel.  Only live while open, so it never
        # takes Escape away from the grid or calendar.
        self._esc = QShortcut(QKeySequence(Qt.Key_Escape), parent)
        self._esc.setContext(Qt.WidgetWithChildrenShortcut)
        self._esc.activated.connect(self.collapse)
        self._esc.setEnabled(False)

        parent.installEventFilter(self)
        self._sync_geometry()
        self.raise_()

    # ------------------------------------------------------------------ #
    # Building                                                             #
    # ------------------------------------------------------------------ #

    def placeholder(self) -> QWidget:
        """The fixed-width slot to put in the host's layout."""
        return self._placeholder

    def add_action(self, action: QAction, hint: Optional[str] = None) -> None:
        """
        Add a button for *action*.  *hint* is the key shown beside the label;
        it defaults to the action's own shortcut.  Pass one for commands whose
        key is handled elsewhere (e.g. P and M live in the grid).
        """
        if hint is None:
            hint = action.shortcut().toString(QKeySequence.NativeText)
        btn = _MenuButton(action, hint, self)
        btn.triggered.connect(lambda _a: self.collapse())
        self._lay.insertWidget(self._lay.count() - 1, btn)
        self._buttons.append(btn)
        self._open_w = None

    def add_separator(self) -> None:
        self._lay.insertWidget(self._lay.count() - 1, _Separator(self))

    # ------------------------------------------------------------------ #
    # State                                                                #
    # ------------------------------------------------------------------ #

    def progress(self) -> float:
        """0 = collapsed rail, 1 = fully open."""
        return self._t

    def is_expanded(self) -> bool:
        return self._expanded

    def toggle(self) -> None:
        self.collapse() if self._expanded else self.expand()

    def expand(self) -> None:
        if self._expanded:
            return
        self._expanded = True
        self._scrim.show()
        self._scrim.raise_()
        self.raise_()
        self._esc.setEnabled(True)
        self._animate_to(1.0)
        self.expanded_changed.emit(True)

    def collapse(self) -> None:
        if not self._expanded:
            return
        self._expanded = False
        self._esc.setEnabled(False)
        self._animate_to(0.0)
        self.expanded_changed.emit(False)

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _expanded_width(self) -> int:
        """Wide enough for the longest label plus its shortcut hint."""
        if self._open_w is not None:
            return self._open_w
        lm = QFontMetrics(_font(12, QFont.DemiBold))
        hm = QFontMetrics(_font(10, QFont.DemiBold))
        need = 0
        for b in self._buttons:
            w = _TEXT_X + lm.horizontalAdvance(b.label()) + 16
            if b.hint():
                w += hm.horizontalAdvance(b.hint()) + 18
            need = max(need, w)
        self._open_w = max(210, min(300, need + 2 * _MARGIN))
        return self._open_w

    def _animate_to(self, target: float) -> None:
        self._anim.stop()
        self._anim.setStartValue(self._t)
        self._anim.setEndValue(target)
        # Keep the pace constant when reversing mid-slide
        self._anim.setDuration(max(60, int(_ANIM_MS * abs(target - self._t))))
        self._anim.start()

    def _set_progress(self, v) -> None:
        self._t = float(v)
        self._scrim.set_progress(self._t)
        self._sync_geometry()
        # Width changes repaint the buttons, but not on frames where the
        # rounded width is unchanged — label alpha still moves then.
        for w in self.findChildren(QToolButton):
            w.update()

    def _on_anim_finished(self) -> None:
        if not self._expanded:
            self._scrim.hide()

    def _sync_geometry(self) -> None:
        h = self._host.height()
        w = round(RAIL_W + (self._expanded_width() - RAIL_W) * self._t) if self._t else RAIL_W
        self.setGeometry(QRect(0, 0, w, h))
        self._scrim.setGeometry(self._host.rect())

    def eventFilter(self, obj: QObject, ev: QEvent) -> bool:
        if obj is self._host and ev.type() == QEvent.Resize:
            self._sync_geometry()
        return super().eventFilter(obj, ev)

    def paintEvent(self, _ev) -> None:
        p = QPainter(self)
        p.fillRect(self.rect(), _BG)
        p.setPen(QPen(_accent(38 + int(40 * self._t)), 1))
        x = self.width() - 1
        p.drawLine(x, 0, x, self.height())
