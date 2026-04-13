"""
GroupedGridView — thumbnail grid with collapsible date-based sections.

Architecture
------------
GroupedGridView (QScrollArea)
  └─ _container (QWidget / QVBoxLayout)
       ├─ _DateSection  (date = 2024-12-25)
       │    ├─ _DateHeader  — clickable: date label + count chip + ▼/▶
       │    └─ _SectionGrid — QListView, no scroll bars, height = content
       ├─ _DateSection  (date = 2024-12-24)
       …
       └─ stretch

One flat PhotoGridModel + PhotoFilterProxy sit behind the view so the
rest of main_window.py can call the same API it used for ThumbnailGridView.
Visual sections are rebuilt whenever records or filter state change.
"""
from __future__ import annotations

import math
from collections import defaultdict
from datetime import date as _pydate, timedelta
from pathlib import Path
from typing import Dict, List, Optional

from PySide6.QtCore import (
    QEasingCurve,
    QEvent,
    QItemSelectionModel,
    QPropertyAnimation,
    QSize,
    Qt,
    QTimer,
    QVariantAnimation,
    Signal,
)
from PySide6.QtGui import QCursor, QNativeGestureEvent
from PySide6.QtWidgets import (
    QAbstractItemView,
    QFrame,
    QGraphicsOpacityEffect,
    QHBoxLayout,
    QLabel,
    QListView,
    QPushButton,
    QScrollArea,
    QScroller,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

from app.models.photo_record import FileType, PhotoRecord
from app.thumbnails.generator import ThumbnailGenerator
from app.ui.proxy import FilterState, PhotoFilterProxy
from app.ui.thumbnail_grid import PhotoGridModel, ThumbnailDelegate


def _canonical_parent(path: Path) -> Path:
    """Strip trailing RAW/ or JPG/ so separated pairs share the same key."""
    return path.parent.parent if path.parent.name.upper() in ("RAW", "JPG") else path.parent


def _dedup_pairs(records: List[PhotoRecord]) -> List[PhotoRecord]:
    """Return *records* with RAW entries removed when a JPG counterpart exists."""
    jpg_keys = {
        (_canonical_parent(r.path), r.pair_stem)
        for r in records
        if r.pair_stem and r.file_type == FileType.JPG
    }
    return [
        r for r in records
        if not (r.pair_stem and r.file_type == FileType.RAW
                and (_canonical_parent(r.path), r.pair_stem) in jpg_keys)
    ]

_QMAX = 16_777_215   # Qt's QWIDGETSIZE_MAX


# ── helpers ────────────────────────────────────────────────────────────────�� #

def _fmt_date(d: _pydate) -> str:
    today = _pydate.today()
    long = f"{d.strftime('%B')} {d.day}, {d.year}"   # "December 25, 2024"
    if d == today:
        return f"Today  ·  {long}"
    if d == today - timedelta(days=1):
        return f"Yesterday  ·  {long}"
    return f"{d.strftime('%A')}  ·  {long}"           # "Tuesday  ·  …"


# ── per-section grid ──────────────────────────────────────────────────────── #

class _SectionGrid(QListView):
    """
    Icon-mode QListView with no scrollbars, sized to exactly fit content.
    The parent QScrollArea handles all scrolling.
    """

    item_activated:   Signal = Signal(object)   # PhotoRecord
    prune_toggled:    Signal = Signal(object)   # List[PhotoRecord]
    selection_changed: Signal = Signal(list)     # List[PhotoRecord] — current selection

    def __init__(
        self,
        records: List[PhotoRecord],
        generator: ThumbnailGenerator,
        thumb_size: int,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._thumb_size = thumb_size

        self._model = PhotoGridModel(generator, self)
        self._model.reset_records(records)

        self.setModel(self._model)
        self.setItemDelegate(ThumbnailDelegate(thumb_size, self))
        self.setViewMode(QListView.IconMode)
        self.setResizeMode(QListView.Adjust)
        self.setWrapping(True)
        self.setUniformItemSizes(True)
        self.setSpacing(4)
        self.setMouseTracking(True)
        self.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setStyleSheet(
            "QListView { background: #0a0a12; border: none; outline: none; }"
        )
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        # Opt out of raw touch so QListView doesn't fight QScroller on the
        # parent scroll area.  Selection still works via synthesized mouse events.
        self.viewport().setAttribute(Qt.WA_AcceptTouchEvents, False)
        self._update_grid_size()
        self._select_mode: bool = False

        self.activated.connect(self._on_activated)

    # ------------------------------------------------------------------ #

    def section_model(self) -> PhotoGridModel:
        return self._model

    def reset_records(self, records: List[PhotoRecord]) -> None:
        self._model.reset_records(records)
        self.updateGeometry()

    def update_thumb_size(self, size: int) -> None:
        """Update delegate + grid metrics without rebuilding the whole section."""
        self._thumb_size = size
        # Reuse the existing delegate — avoids a new object per animation frame
        d = self.itemDelegate()
        if isinstance(d, ThumbnailDelegate):
            d.thumb_size = size
        else:
            self.setItemDelegate(ThumbnailDelegate(size, self))
        self._update_grid_size()
        self.updateGeometry()
        # Drop scaled-pixmap cache; entries at old size are wrong.
        from app.ui.thumbnail_grid import _SCALED_CACHE
        _SCALED_CACHE.clear()
        self.viewport().update()

    # ------------------------------------------------------------------ #
    # Sizing                                                               #
    # ------------------------------------------------------------------ #

    def sizeHint(self) -> QSize:
        return QSize(self.width(), self._content_height())

    def minimumSizeHint(self) -> QSize:
        return QSize(0, 0)

    def resizeEvent(self, event) -> None:
        super().resizeEvent(event)
        self.updateGeometry()

    def _content_height(self) -> int:
        n = self._model.rowCount()
        if n == 0:
            return 0
        cw = self.gridSize().width()
        ch = self.gridSize().height()
        vp_w = max(cw, self.viewport().width())
        cols = max(1, vp_w // cw)
        rows = math.ceil(n / cols)
        return rows * ch + 4

    def _update_grid_size(self) -> None:
        p  = ThumbnailDelegate.PAD
        lh = ThumbnailDelegate.LABEL_H
        ts = self._thumb_size
        self.setGridSize(QSize(ts + p * 2 + 6, ts + lh + p * 2 + 6))
        self.setIconSize(QSize(ts, ts))

    # ------------------------------------------------------------------ #
    # Selection                                                           #
    # ------------------------------------------------------------------ #

    def selectionChanged(self, selected, deselected) -> None:
        super().selectionChanged(selected, deselected)
        records = [
            idx.data(Qt.UserRole)
            for idx in self.selectionModel().selectedIndexes()
            if idx.data(Qt.UserRole)
        ]
        self.selection_changed.emit(records)
        # Dirty rects from super() can get lost when this fires as a side-effect
        # of another section's selection change.  Force a full viewport repaint
        # after the signal chain settles so stale highlights are always cleared.
        self.viewport().update()

    def selected_records(self) -> List[PhotoRecord]:
        return [
            idx.data(Qt.UserRole)
            for idx in self.selectionModel().selectedIndexes()
            if idx.data(Qt.UserRole)
        ]

    def select_all(self) -> None:
        self.selectAll()

    def deselect_all(self) -> None:
        self.clearSelection()
        self.viewport().update()

    # ------------------------------------------------------------------ #
    # Input                                                                #
    # ------------------------------------------------------------------ #

    def mousePressEvent(self, event) -> None:
        if self._select_mode and event.button() == Qt.LeftButton:
            index = self.indexAt(event.position().toPoint())
            if index.isValid():
                sm = self.selectionModel()
                if sm.isSelected(index):
                    sm.select(index, QItemSelectionModel.Deselect)
                else:
                    sm.select(index, QItemSelectionModel.Select)
            # Accept and return — don't call super() so Qt doesn't
            # clear-and-reselect via its ExtendedSelection logic.
            event.accept()
            return
        # Outside select mode: clicking empty space (no item hit) should
        # clear the selection so the orange border doesn't linger.
        if event.button() == Qt.LeftButton:
            if not self.indexAt(event.position().toPoint()).isValid():
                self.clearSelection()
                event.accept()
                return
        super().mousePressEvent(event)

    def focusOutEvent(self, event) -> None:
        """Clear selection when this grid loses focus (user clicked elsewhere)."""
        super().focusOutEvent(event)
        if not self._select_mode:
            self.deselect_all()  # includes viewport().update()

    def wheelEvent(self, event) -> None:
        # Don't consume — walk up to GroupedGridView and let it handle
        # both smooth scrolling and Ctrl+scroll zoom.
        p = self.parent()
        while p is not None:
            if isinstance(p, GroupedGridView):
                p.wheelEvent(event)
                return
            p = p.parent()
        event.ignore()

    def keyPressEvent(self, event) -> None:
        if event.key() in (Qt.Key_P, Qt.Key_Delete):
            records = [
                idx.data(Qt.UserRole)
                for idx in self.selectionModel().selectedIndexes()
                if idx.data(Qt.UserRole)
            ]
            if records:
                all_pruned = all(r.is_pruned for r in records)
                for r in records:
                    r.is_pruned = not all_pruned
                self.prune_toggled.emit(records)
        elif event.key() in (
            Qt.Key_Up, Qt.Key_Down,
            Qt.Key_PageUp, Qt.Key_PageDown,
            Qt.Key_Home, Qt.Key_End,
        ):
            # Forward scroll keys to the parent GroupedGridView
            p = self.parent()
            while p is not None:
                if isinstance(p, GroupedGridView):
                    p.keyPressEvent(event)
                    return
                p = p.parent()
            super().keyPressEvent(event)
        else:
            super().keyPressEvent(event)

    def _on_activated(self, index) -> None:
        r = index.data(Qt.UserRole)
        if r and not self._select_mode:
            self.item_activated.emit(r)


# ── date header ───────────────────────────────────────────────────────────── #

class _DateHeader(QWidget):
    """Clickable header row:  [▼/▶]  [date label]  [n items chip]  [── line]
    Clicking the chip selects / deselects all items in the section.
    Clicking anywhere else toggles collapse.
    """

    collapse_toggled:   Signal = Signal(bool)   # new collapsed state
    select_all_toggled: Signal = Signal()        # chip was clicked

    def __init__(
        self,
        label: str,
        count: int,
        collapsed: bool = False,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._collapsed = collapsed
        self.setFixedHeight(44)
        self.setCursor(QCursor(Qt.PointingHandCursor))
        self.setStyleSheet("QWidget { background: #0e0e1a; }")

        lay = QHBoxLayout(self)
        lay.setContentsMargins(14, 0, 14, 0)
        lay.setSpacing(8)

        self._arrow = QLabel("▶" if collapsed else "▼")
        self._arrow.setStyleSheet(
            "color: rgba(255,109,0,0.55); font-size: 9px; min-width: 10px;"
        )

        self._label = QLabel(label)
        self._label.setStyleSheet(
            "color: #f0f0f0; font-size: 12px; font-weight: 600;"
        )

        self._chip = QPushButton(str(count))
        self._chip.setFlat(True)
        self._chip.setCursor(QCursor(Qt.PointingHandCursor))
        self._chip.setStyleSheet(
            "QPushButton { color: #9898b8; background: rgba(255,109,0,0.10);"
            " border: 1px solid rgba(255,109,0,0.20);"
            " border-radius: 3px; padding: 1px 8px;"
            " font-size: 11px; font-weight: 600; }"
            "QPushButton:hover { background: rgba(255,109,0,0.22);"
            " border-color: rgba(255,109,0,0.50); color: #ff6d00; }"
        )
        self._chip_selected = 0   # items selected in this section
        self._chip.clicked.connect(self.select_all_toggled)

        rule = QWidget()
        rule.setFixedHeight(1)
        rule.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        rule.setStyleSheet("background: rgba(255,109,0,0.10);")

        lay.addWidget(self._arrow)
        lay.addWidget(self._label)
        lay.addWidget(self._chip)
        lay.addWidget(rule)

    def update_count(self, count: int) -> None:
        self._chip.setText(str(count))

    def set_selection_indicator(self, n_selected: int, n_total: int) -> None:
        """Update the chip to reflect how many items in the section are selected."""
        self._chip_selected = n_selected
        if n_selected == 0:
            self._chip.setText(str(n_total))
            self._chip.setStyleSheet(
                "QPushButton { color: #9898b8; background: rgba(255,109,0,0.10);"
                " border: 1px solid rgba(255,109,0,0.20);"
                " border-radius: 3px; padding: 1px 8px;"
                " font-size: 11px; font-weight: 600; }"
                "QPushButton:hover { background: rgba(255,109,0,0.22);"
                " border-color: rgba(255,109,0,0.50); color: #ff6d00; }"
            )
        else:
            label = f"✓ {n_selected}/{n_total}" if n_selected < n_total else f"✓ {n_total}"
            self._chip.setText(label)
            self._chip.setStyleSheet(
                "QPushButton { color: #ff6d00; background: rgba(255,109,0,0.20);"
                " border: 1px solid rgba(255,109,0,0.55);"
                " border-radius: 3px; padding: 1px 8px;"
                " font-size: 11px; font-weight: 700; }"
                "QPushButton:hover { background: rgba(255,109,0,0.32); }"
            )

    def mousePressEvent(self, event) -> None:
        # Chip has its own click handler — only toggle collapse on non-chip clicks
        if not self._chip.geometry().contains(event.position().toPoint()):
            self._collapsed = not self._collapsed
            self._arrow.setText("▶" if self._collapsed else "▼")
            self.collapse_toggled.emit(self._collapsed)

    def set_collapsed(self, collapsed: bool) -> None:
        self._collapsed = collapsed
        self._arrow.setText("▶" if collapsed else "▼")


# ── date section ──────────────────────────────────────────────────────────── #

class _DateSection(QWidget):
    """One date group: header + collapsible thumbnail grid."""

    item_activated:   Signal = Signal(object)
    prune_toggled:    Signal = Signal(object)
    selection_changed: Signal = Signal(list)   # List[PhotoRecord] from this section

    def __init__(
        self,
        date_key: _pydate,
        records: List[PhotoRecord],
        generator: ThumbnailGenerator,
        thumb_size: int,
        collapsed: bool = False,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._date_key  = date_key
        self._collapsed = collapsed
        self._anim: Optional[QPropertyAnimation] = None

        lay = QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(0)

        self._header = _DateHeader(_fmt_date(date_key), len(records), collapsed)
        self._grid   = _SectionGrid(records, generator, thumb_size, self)

        # Clip container: only this widget's height is animated, so Qt does not
        # need to re-run the outer VBoxLayout geometry pass on every anim tick.
        # _grid lives inside clip with no height constraint; clip does the clipping.
        self._clip = QWidget(self)
        self._clip.setStyleSheet("background: transparent;")
        clip_lay = QVBoxLayout(self._clip)
        clip_lay.setContentsMargins(0, 0, 0, 0)
        clip_lay.setSpacing(0)
        clip_lay.addWidget(self._grid)

        lay.addWidget(self._header)
        lay.addWidget(self._clip)

        self._grid.item_activated.connect(self.item_activated)
        self._grid.prune_toggled.connect(self.prune_toggled)
        self._grid.selection_changed.connect(self._on_grid_selection_changed)
        self._header.collapse_toggled.connect(self._on_toggle)
        self._header.select_all_toggled.connect(self._on_select_all_toggled)

        if collapsed:
            self._clip.setFixedHeight(0)
        else:
            self._clip.setMaximumHeight(_QMAX)

    # ------------------------------------------------------------------ #

    @property
    def date_key(self) -> _pydate:
        return self._date_key

    @property
    def is_collapsed(self) -> bool:
        return self._collapsed

    def section_model(self) -> PhotoGridModel:
        return self._grid.section_model()

    def update_thumb_size(self, size: int) -> None:
        self._grid.update_thumb_size(size)

    def set_select_mode(self, enabled: bool) -> None:
        self._grid._select_mode = enabled
        d = self._grid.itemDelegate()
        if isinstance(d, ThumbnailDelegate):
            d.select_mode = enabled
        self._grid.viewport().update()

    def select_all(self) -> None:
        self._grid.select_all()

    def deselect_all(self) -> None:
        self._grid.deselect_all()

    def selected_records(self) -> List[PhotoRecord]:
        return self._grid.selected_records()

    def record_count(self) -> int:
        return self._grid.section_model().rowCount()

    # ------------------------------------------------------------------ #

    def _on_select_all_toggled(self) -> None:
        """Chip clicked — toggle select-all / deselect-all for this section."""
        if not self._grid._select_mode:
            return
        n_sel   = len(self._grid.selected_records())
        n_total = self._grid.section_model().rowCount()
        if n_sel == n_total:
            self._grid.deselect_all()
        else:
            self._grid.select_all()

    def _on_grid_selection_changed(self, records: list) -> None:
        n_total = self._grid.section_model().rowCount()
        self._header.set_selection_indicator(len(records), n_total)
        self.selection_changed.emit(records)

    def _on_toggle(self, collapsed: bool) -> None:
        self._collapsed = collapsed
        start  = self._clip.height()
        target = 0 if collapsed else self._grid.sizeHint().height()

        if self._anim is not None:
            try:
                self._anim.stop()
            except RuntimeError:
                pass
            self._anim = None

        # Animate the clip container's fixed height.  The outer VBoxLayout sees
        # a widget with a fixed height changing, which is a single geometry
        # update per tick — far cheaper than maximumHeight which triggers a full
        # constraint solve on every frame.
        anim = QPropertyAnimation(self._clip, b"maximumHeight", self)
        anim.setDuration(110)
        anim.setEasingCurve(QEasingCurve.OutExpo)
        anim.setStartValue(start)
        anim.setEndValue(target)
        if not collapsed:
            anim.finished.connect(lambda: self._clip.setMaximumHeight(_QMAX))
        anim.finished.connect(self._on_anim_finished)
        anim.start(QPropertyAnimation.DeleteWhenStopped)
        self._anim = anim

    def _on_anim_finished(self) -> None:
        self._anim = None


# ── floating selection HUD ────────────────────────────────────────────────── #

class _SelectionBar(QFrame):
    """
    Floating pill at the bottom of the grid viewport.

    Shows:  [N selected]  [Tap to select / Done]  [Prune]  [✕ Clear]

    Fades in when ≥ 1 item is selected (or when select-mode is active),
    fades out when selection is cleared and select-mode is off.
    """

    select_mode_toggled:   Signal = Signal(bool)   # new mode state

    _BTN_DONE = (
        "QPushButton{background:rgba(255,109,0,0.30);color:#ff6d00;"
        "border:1px solid rgba(255,109,0,0.65);border-radius:5px;"
        "font-size:11px;font-weight:700;padding:4px 14px;}"
        "QPushButton:hover{background:rgba(255,109,0,0.42);}"
        "QPushButton:pressed{background:rgba(255,109,0,0.55);}"
    )
    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setFixedHeight(44)
        self.setStyleSheet(
            "QFrame{background:#0e0e1a;"
            "border:1px solid rgba(255,109,0,0.40);"
            "border-radius:8px;}"
        )

        lay = QHBoxLayout(self)
        lay.setContentsMargins(14, 0, 8, 0)
        lay.setSpacing(8)

        self._lbl = QLabel("0 selected")
        self._lbl.setStyleSheet("color:#c0c0d8;font-size:12px;font-weight:600;"
                                "background:transparent;border:none;")

        self._btn_done = QPushButton("Done")
        self._btn_done.setStyleSheet(self._BTN_DONE)

        lay.addWidget(self._lbl)
        lay.addStretch()
        lay.addWidget(self._btn_done)

        self._btn_done.clicked.connect(lambda: self.select_mode_toggled.emit(False))

        self._count        = 0
        self._mode         = False
        self._hide_on_done = False

        self._fx = QGraphicsOpacityEffect(self)
        self._fx.setOpacity(0.0)
        self.setGraphicsEffect(self._fx)

        self._opacity_anim = QVariantAnimation(self)
        self._opacity_anim.setDuration(180)
        self._opacity_anim.setEasingCurve(QEasingCurve.OutCubic)
        self._opacity_anim.valueChanged.connect(
            lambda v: self._fx.setOpacity(float(v))
        )
        self._opacity_anim.finished.connect(self._on_fade_out_done)

    def update_count(self, count: int) -> None:
        self._count = count
        self._lbl.setText(f"{count} selected" if count else "Select mode")
        self._maybe_toggle_visibility()

    def set_select_mode(self, enabled: bool) -> None:
        self._mode = enabled
        if enabled:
            self._lbl.setText(f"{self._count} selected" if self._count else "Select mode")
        self._maybe_toggle_visibility()

    def _maybe_toggle_visibility(self) -> None:
        should_show = self._count > 0 or self._mode
        if should_show and not self.isVisible():
            self.show()
            self.raise_()
            self._animate_opacity(1.0)
        elif not should_show and self.isVisible():
            self._animate_opacity(0.0, hide_on_done=True)

    def _animate_opacity(self, target: float, hide_on_done: bool = False) -> None:
        self._hide_on_done = hide_on_done
        self._opacity_anim.stop()
        self._opacity_anim.setStartValue(self._fx.opacity())
        self._opacity_anim.setEndValue(target)
        self._opacity_anim.start()

    def _on_fade_out_done(self) -> None:
        if self._hide_on_done:
            self.hide()
            self._hide_on_done = False


# ── main scroll area ──────────────────────────────────────────────────────── #

_QSS = """
    QScrollArea { background: #0a0a12; border: none; }
    QScrollBar:vertical {
        background: #0a0a12; width: 7px; border: none; margin: 0;
    }
    QScrollBar::handle:vertical {
        background: #2a2a40; border-radius: 3px; min-height: 24px;
    }
    QScrollBar::handle:vertical:hover   { background: rgba(255,109,0,0.45); }
    QScrollBar::handle:vertical:pressed { background: rgba(255,109,0,0.70); }
    QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height: 0; }
    QScrollBar::add-page:vertical, QScrollBar::sub-page:vertical { background: transparent; }
"""


class GroupedGridView(QScrollArea):
    """
    Thumbnail grid grouped by shoot date with collapsible sections.

    Exposes the same interface as ThumbnailGridView so main_window.py
    needs minimal changes.
    """

    item_activated:        Signal = Signal(object)   # PhotoRecord
    prune_toggled:         Signal = Signal(object)   # List[PhotoRecord]
    thumb_size_changed:    Signal = Signal(int)       # new size from Ctrl+scroll
    selection_changed:     Signal = Signal(list)      # List[PhotoRecord] — full current selection
    selection_mode_changed: Signal = Signal(bool)     # select mode on/off

    def __init__(
        self,
        generator: ThumbnailGenerator,
        thumb_size: int = 160,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._generator  = generator
        self._thumb_size = thumb_size

        # Flat model + proxy — keeps main_window.py compatibility
        self._flat_model = PhotoGridModel(generator, self)
        self._proxy      = PhotoFilterProxy(self)
        self._proxy.setSourceModel(self._flat_model)

        # Collapse state survives rebuilds
        self._collapsed: Dict[_pydate, bool] = {}

        # Live sections (ordered oldest→newest)
        self._sections: List[_DateSection] = []
        self._path_to_sec: Dict[Path, _DateSection] = {}

        # Debounce: avoid rebuilding every 150 ms batch during scanning
        self._rebuild_timer = QTimer(self)
        self._rebuild_timer.setSingleShot(True)
        self._rebuild_timer.setInterval(300)
        self._rebuild_timer.timeout.connect(self._rebuild)

        # Debounce for thumb-size slider: update delegates immediately,
        # defer full widget rebuild until the user stops dragging
        self._resize_timer = QTimer(self)
        self._resize_timer.setSingleShot(True)
        self._resize_timer.setInterval(250)
        self._resize_timer.timeout.connect(self._rebuild)

        # Smooth-scroll animation — accumulates target so rapid events feel fluid
        self._scroll_anim = QVariantAnimation(self)
        self._scroll_anim.setDuration(180)
        self._scroll_anim.setEasingCurve(QEasingCurve.OutCubic)
        self._scroll_anim.valueChanged.connect(
            lambda v: self.verticalScrollBar().setValue(int(v))
        )

        # Zoom animation — interpolates thumb_size so resize feels fluid
        self._zoom_anim = QVariantAnimation(self)
        self._zoom_anim.setDuration(160)
        self._zoom_anim.setEasingCurve(QEasingCurve.OutQuint)
        self._zoom_anim.valueChanged.connect(self._on_zoom_tick)

        # Scroll area
        self.setWidgetResizable(True)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setStyleSheet(_QSS)
        self.verticalScrollBar().setSingleStep(40)

        self._container = QWidget()
        self._container.setStyleSheet("background: #0a0a12;")
        self._vlay = QVBoxLayout(self._container)
        self._vlay.setContentsMargins(0, 4, 0, 8)
        self._vlay.setSpacing(0)
        self._vlay.addStretch()
        self.setWidget(self._container)

        # Touch: kinetic scroll via QScroller + pinch-to-zoom gesture
        self._pinch_start_size: int = self._thumb_size
        self.grabGesture(Qt.PinchGesture)
        QScroller.grabGesture(self, QScroller.TouchGesture)

        # Multi-select state
        self._select_mode: bool = False
        # Parent to self (the QScrollArea), NOT the viewport.
        # The viewport's content widget always paints over other viewport
        # children regardless of z-order; parenting to the scroll area itself
        # keeps the bar above the viewport in the widget stack.
        self._sel_bar = _SelectionBar(self)
        self._sel_bar.hide()
        self._sel_bar.select_mode_toggled.connect(self.set_selection_mode)

    # ------------------------------------------------------------------ #
    # Multi-select                                                        #
    # ------------------------------------------------------------------ #

    def set_selection_mode(self, enabled: bool) -> None:
        """Toggle select mode: tap toggles selection instead of opening viewer."""
        self._select_mode = enabled
        for sec in self._sections:
            sec.set_select_mode(enabled)
        self._sel_bar.set_select_mode(enabled)
        if not enabled:
            self.clear_selection()
            self._sel_bar.update_count(0)   # force bar to hide
        self.selection_mode_changed.emit(enabled)
        self._reposition_sel_bar()

    def select_all(self) -> None:
        """Select all visible items across every section."""
        for sec in self._sections:
            if not sec.is_collapsed:
                sec.select_all()

    def clear_selection(self) -> None:
        """Clear selection in all sections."""
        for sec in self._sections:
            sec.deselect_all()

    def _on_section_selection_changed(self, records: list) -> None:
        """Any section's selection changed — aggregate and notify."""
        if not self._select_mode and records:
            # Outside select mode only one item should be highlighted at a time.
            # Deselect every section except the one that just emitted so
            # highlights can't pile up across sections.
            source = self.sender()
            for sec in self._sections:
                if sec is not source:
                    sec.deselect_all()

        all_selected: List[PhotoRecord] = []
        for sec in self._sections:
            all_selected.extend(sec.selected_records())
        if self._select_mode:
            self._sel_bar.update_count(len(all_selected))
        self.selection_changed.emit(all_selected)

    def _reposition_sel_bar(self) -> None:
        bar_w = min(self.width() - 32, 520)
        x = (self.width() - bar_w) // 2
        y = self.height() - self._sel_bar.height() - 14
        self._sel_bar.setGeometry(x, y, bar_w, self._sel_bar.height())
        self._sel_bar.raise_()

    def resizeEvent(self, ev) -> None:
        super().resizeEvent(ev)
        self._reposition_sel_bar()

    # ------------------------------------------------------------------ #
    # Touch gestures                                                      #
    # ------------------------------------------------------------------ #

    def event(self, ev) -> bool:
        # ── Touchscreen pinch ────────────────────────────────────────────
        if ev.type() == QEvent.Gesture:
            pinch = ev.gesture(Qt.PinchGesture)
            if pinch:
                if pinch.state() == Qt.GestureStarted:
                    self._pinch_start_size = self._thumb_size
                new_size = max(80, min(280,
                    int(self._pinch_start_size * pinch.totalScaleFactor())))
                if new_size != self._thumb_size:
                    self.set_thumb_size(new_size)
                    self.thumb_size_changed.emit(new_size)
            ev.accept()
            return True

        # ── Trackpad pinch (NativeGesture) ───────────────────────────────
        if ev.type() == QEvent.NativeGesture:
            if isinstance(ev, QNativeGestureEvent):
                if ev.gestureType() == Qt.NativeGestureType.ZoomNativeGesture:
                    factor   = 1.0 + ev.value()
                    new_size = max(80, min(280, int(self._thumb_size * factor)))
                    if new_size != self._thumb_size:
                        self.set_thumb_size(new_size)
                        self.thumb_size_changed.emit(new_size)
                    ev.accept()
                    return True

        return super().event(ev)

    # ------------------------------------------------------------------ #
    # Public interface (mirrors ThumbnailGridView)                        #
    # ------------------------------------------------------------------ #

    def set_thumb_size(self, size: int) -> None:
        """Animate from the current thumb size to *size*.

        Rapid calls (slider drag, Ctrl+scroll bursts) are accumulated:
        if the animation is already running its end value is updated so
        the thumbnail cells smoothly chase the latest target without
        stuttering.  A full widget rebuild is deferred until input stops.
        """
        size = max(80, min(280, size))
        if size == self._thumb_size and self._zoom_anim.state() != QVariantAnimation.Running:
            return
        if self._zoom_anim.state() == QVariantAnimation.Running:
            # Accumulate — redirect the running animation to the new target
            self._zoom_anim.setEndValue(float(size))
        else:
            self._zoom_anim.stop()
            self._zoom_anim.setStartValue(float(self._thumb_size))
            self._zoom_anim.setEndValue(float(size))
            self._zoom_anim.start()
        self._resize_timer.start()   # full rebuild once input stops

    def _on_zoom_tick(self, value: float) -> None:
        """Called on every animation frame — update delegates, no full rebuild."""
        size = int(round(value))
        if size == self._thumb_size:
            return
        self._thumb_size = size
        for sec in self._sections:
            sec.update_thumb_size(size)

    def _smooth_scroll_to(self, target: int) -> None:
        """Animate the scrollbar to *target*, accumulating rapid events."""
        sb  = self.verticalScrollBar()
        end = max(sb.minimum(), min(sb.maximum(), target))
        # Start value: wherever the animation currently is (feels continuous)
        cur = (int(self._scroll_anim.currentValue())
               if self._scroll_anim.state() == QVariantAnimation.Running
               else sb.value())
        if cur == end:
            return
        self._scroll_anim.stop()
        self._scroll_anim.setStartValue(cur)
        self._scroll_anim.setEndValue(end)
        self._scroll_anim.start()

    def keyPressEvent(self, event) -> None:
        sb  = self.verticalScrollBar()
        key = event.key()
        # Use current anim target (if running) as base so rapid presses accumulate
        base = (int(self._scroll_anim.endValue())
                if self._scroll_anim.state() == QVariantAnimation.Running
                else sb.value())
        if key == Qt.Key_Up:
            self._smooth_scroll_to(base - 90)
        elif key == Qt.Key_Down:
            self._smooth_scroll_to(base + 90)
        elif key == Qt.Key_PageUp:
            self._smooth_scroll_to(base - self.viewport().height())
        elif key == Qt.Key_PageDown:
            self._smooth_scroll_to(base + self.viewport().height())
        elif key == Qt.Key_Home:
            self._smooth_scroll_to(sb.minimum())
        elif key == Qt.Key_End:
            self._smooth_scroll_to(sb.maximum())
        elif key == Qt.Key_A and event.modifiers() & Qt.ControlModifier:
            self.select_all()
        elif key == Qt.Key_Escape:
            self.clear_selection()
            if self._select_mode:
                self.set_selection_mode(False)
        else:
            super().keyPressEvent(event)

    def wheelEvent(self, event) -> None:
        if event.modifiers() & Qt.ControlModifier:
            # Ctrl+scroll → zoom thumbnails in/out
            delta = event.angleDelta().y()
            if delta == 0:
                return
            step = 20
            new_size = self._thumb_size + (step if delta > 0 else -step)
            new_size = max(80, min(280, (new_size // step) * step))
            if new_size != self._thumb_size:
                self.set_thumb_size(new_size)
                self.thumb_size_changed.emit(new_size)
            event.accept()
        else:
            # Smooth scroll — accumulate rapid wheel ticks into one animation
            delta = event.angleDelta().y()
            if delta:
                sb   = self.verticalScrollBar()
                base = (int(self._scroll_anim.endValue())
                        if self._scroll_anim.state() == QVariantAnimation.Running
                        else sb.value())
                self._smooth_scroll_to(base - round(delta / 120 * 100))
            event.accept()

    def source_model(self) -> PhotoGridModel:
        return self._flat_model

    def filter_proxy(self) -> PhotoFilterProxy:
        return self._proxy

    def reset_records(self, records: List[PhotoRecord]) -> None:
        self._rebuild_timer.stop()
        self._flat_model.reset_records(records)
        self._rebuild()

    def append_batch(self, records: List[PhotoRecord]) -> None:
        """Called during scanning — debounced rebuild."""
        self._flat_model.append_batch(records)
        self._rebuild_timer.start()

    def remove_records(self, records: List[PhotoRecord]) -> None:
        self._flat_model.remove_records(records)
        self._rebuild()

    def notify_records_changed(self, records: List[PhotoRecord]) -> None:
        """Route prune/pair state changes to the right section model."""
        by_sec: Dict[int, List[PhotoRecord]] = defaultdict(list)
        for r in records:
            sec = self._path_to_sec.get(r.path)
            if sec is not None:
                by_sec[id(sec)].append(r)
        for sec in self._sections:
            chunk = by_sec.get(id(sec))
            if chunk:
                sec.section_model().notify_records_changed(chunk)

    def apply_filter(self, state: FilterState) -> None:
        self._proxy.apply_state(state)
        self._rebuild()

    def visible_count(self) -> int:
        return self._proxy.rowCount()

    def selected_records(self) -> List[PhotoRecord]:
        """Return all records currently selected across every section grid."""
        out: List[PhotoRecord] = []
        for sec in self._sections:
            for idx in sec._grid.selectionModel().selectedIndexes():
                r = idx.data(Qt.UserRole)
                if r:
                    out.append(r)
        return out

    def all_visible_records(self) -> List[PhotoRecord]:
        raw = [
            self._proxy.index(row, 0).data(Qt.UserRole)
            for row in range(self._proxy.rowCount())
            if self._proxy.index(row, 0).data(Qt.UserRole)
        ]
        return _dedup_pairs(raw)

    # ------------------------------------------------------------------ #
    # Internal rebuild                                                     #
    # ------------------------------------------------------------------ #

    def _rebuild(self) -> None:
        """
        Synchronise section widgets with the current proxy output.

        Uses a diff strategy: sections whose date key AND record identity set
        are unchanged are kept in place (no widget churn).  Only new/removed
        date groups cause widget creation/deletion.  This turns a 200–500 ms
        full teardown into a sub-millisecond no-op for simple filter toggles
        that don't actually change which dates are visible.
        """
        scroll_pos = self.verticalScrollBar().value()

        # Snapshot collapse state from live sections
        for sec in self._sections:
            self._collapsed[sec.date_key] = sec.is_collapsed

        # Collect visible+sorted records from proxy
        all_visible: List[PhotoRecord] = []
        for row in range(self._proxy.rowCount()):
            r = self._proxy.index(row, 0).data(Qt.UserRole)
            if r:
                all_visible.append(r)

        visible = _dedup_pairs(all_visible)

        # Group by shot date
        groups: Dict[_pydate, List[PhotoRecord]] = {}
        for r in visible:
            d = r.shot_time.date()
            groups.setdefault(d, []).append(r)

        state = self._proxy.state
        reverse_sections = (state.sort_key == "date" and not state.sort_asc)
        ordered_dates = sorted(groups.keys(), reverse=reverse_sections)

        # Build a lookup of existing sections by date key
        existing: Dict[_pydate, _DateSection] = {
            sec.date_key: sec for sec in self._sections
        }

        self._container.setUpdatesEnabled(False)

        # Remove sections for dates that no longer appear
        for old_date, sec in list(existing.items()):
            if old_date not in groups:
                self._vlay.removeWidget(sec)
                sec.setParent(None)
                sec.deleteLater()
                del existing[old_date]

        # Insert / update sections in the correct order
        self._sections.clear()
        self._path_to_sec.clear()

        for insert_at, d in enumerate(ordered_dates):
            records = groups[d]
            record_paths = {r.path for r in records}

            if d in existing:
                sec = existing[d]
                # Move to correct position if needed
                current_pos = self._vlay.indexOf(sec)
                if current_pos != insert_at:
                    self._vlay.removeWidget(sec)
                    self._vlay.insertWidget(insert_at, sec)
                # Check if the record set changed
                old_paths = {
                    idx.data(Qt.UserRole).path
                    for row in range(sec.section_model().rowCount())
                    for idx in [sec.section_model().index(row)]
                    if idx.data(Qt.UserRole)
                }
                if old_paths != record_paths:
                    sec.section_model().reset_records(records)
                    sec._header.update_count(len(records))
                    # Rebuild path index for this section
                sec.set_select_mode(self._select_mode)
            else:
                collapsed = self._collapsed.get(d, False)
                sec = _DateSection(
                    d, records, self._generator, self._thumb_size,
                    collapsed=collapsed, parent=self._container,
                )
                sec.item_activated.connect(self.item_activated)
                sec.prune_toggled.connect(self.prune_toggled)
                sec.selection_changed.connect(self._on_section_selection_changed)
                sec.set_select_mode(self._select_mode)
                self._vlay.insertWidget(insert_at, sec)

            self._sections.append(sec)
            for r in records:
                self._path_to_sec[r.path] = sec

        self._container.updateGeometry()
        self._container.setUpdatesEnabled(True)
        # Clear stale Qt selection state on rebuild unless in select mode
        if not self._select_mode:
            for sec in self._sections:
                sec.deselect_all()
        # Restore scroll position after layout settles, then re-raise the
        # sel_bar so freshly inserted section widgets don't cover it.
        QTimer.singleShot(0, lambda: self.verticalScrollBar().setValue(scroll_pos))
        QTimer.singleShot(0, self._reposition_sel_bar)
