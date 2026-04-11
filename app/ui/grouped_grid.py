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
    QPropertyAnimation,
    QSize,
    Qt,
    QTimer,
    QVariantAnimation,
    Signal,
)
from PySide6.QtGui import QCursor
from PySide6.QtWidgets import (
    QAbstractItemView,
    QHBoxLayout,
    QLabel,
    QListView,
    QScrollArea,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

from app.models.photo_record import FileType, PhotoRecord
from app.thumbnails.generator import ThumbnailGenerator
from app.ui.proxy import FilterState, PhotoFilterProxy
from app.ui.thumbnail_grid import PhotoGridModel, ThumbnailDelegate

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

    item_activated: Signal = Signal(object)   # PhotoRecord
    prune_toggled:  Signal = Signal(object)   # List[PhotoRecord]

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
        self._update_grid_size()

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
    # Input                                                                #
    # ------------------------------------------------------------------ #

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
        if r:
            self.item_activated.emit(r)


# ── date header ───────────────────────────────────────────────────────────── #

class _DateHeader(QWidget):
    """Clickable header row:  [▼/▶]  [date label]  [n items chip]  [── line]"""

    collapse_toggled: Signal = Signal(bool)   # new collapsed state

    def __init__(
        self,
        label: str,
        count: int,
        collapsed: bool = False,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._collapsed = collapsed
        self.setFixedHeight(36)
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

        self._chip = QLabel(str(count))
        self._chip.setStyleSheet(
            "color: #9898b8; background: rgba(255,109,0,0.10);"
            " border: 1px solid rgba(255,109,0,0.20);"
            " border-radius: 3px; padding: 1px 8px;"
            " font-size: 11px; font-weight: 600;"
        )

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

    def set_collapsed(self, collapsed: bool) -> None:
        self._collapsed = collapsed
        self._arrow.setText("▶" if collapsed else "▼")

    def mousePressEvent(self, _event) -> None:
        self._collapsed = not self._collapsed
        self._arrow.setText("▶" if self._collapsed else "▼")
        self.collapse_toggled.emit(self._collapsed)


# ── date section ──────────────────────────────────────────────────────────── #

class _DateSection(QWidget):
    """One date group: header + collapsible thumbnail grid."""

    item_activated: Signal = Signal(object)
    prune_toggled:  Signal = Signal(object)

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

        lay.addWidget(self._header)
        lay.addWidget(self._grid)

        self._grid.item_activated.connect(self.item_activated)
        self._grid.prune_toggled.connect(self.prune_toggled)
        self._header.collapse_toggled.connect(self._on_toggle)

        if collapsed:
            self._grid.setMaximumHeight(0)

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

    # ------------------------------------------------------------------ #

    def _on_toggle(self, collapsed: bool) -> None:
        self._collapsed = collapsed
        start  = self._grid.height()
        target = 0 if collapsed else self._grid.sizeHint().height()

        if self._anim is not None:
            try:
                self._anim.stop()
            except RuntimeError:
                pass   # C++ object already deleted by DeleteWhenStopped
            self._anim = None

        anim = QPropertyAnimation(self._grid, b"maximumHeight", self)
        anim.setDuration(110)
        anim.setEasingCurve(QEasingCurve.OutExpo)
        anim.setStartValue(start)
        anim.setEndValue(target)
        if not collapsed:
            anim.finished.connect(lambda: self._grid.setMaximumHeight(_QMAX))
        anim.finished.connect(self._on_anim_finished)
        anim.start(QPropertyAnimation.DeleteWhenStopped)
        self._anim = anim

    def _on_anim_finished(self) -> None:
        self._anim = None


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

    item_activated:     Signal = Signal(object)   # PhotoRecord
    prune_toggled:      Signal = Signal(object)   # List[PhotoRecord]
    thumb_size_changed: Signal = Signal(int)       # new size from Ctrl+scroll

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
        return [
            self._proxy.index(row, 0).data(Qt.UserRole)
            for row in range(self._proxy.rowCount())
            if self._proxy.index(row, 0).data(Qt.UserRole)
        ]

    # ------------------------------------------------------------------ #
    # Internal rebuild                                                     #
    # ------------------------------------------------------------------ #

    def _rebuild(self) -> None:
        """Read proxy output, group by shot date, replace section widgets."""
        scroll_pos = self.verticalScrollBar().value()

        # Snapshot collapse state
        for sec in self._sections:
            self._collapsed[sec.date_key] = sec.is_collapsed

        # Collect visible+sorted records from proxy
        all_visible: List[PhotoRecord] = []
        for row in range(self._proxy.rowCount()):
            r = self._proxy.index(row, 0).data(Qt.UserRole)
            if r:
                all_visible.append(r)

        # Deduplicate pairs: for a RAW+JPG pair show only the JPG (faster
        # thumbnails, better colour rendering).  Fall back to RAW-only if
        # there is no JPG counterpart.
        jpg_pair_keys = {
            (r.path.parent, r.pair_stem)
            for r in all_visible
            if r.pair_stem and r.file_type == FileType.JPG
        }
        visible: List[PhotoRecord] = []
        for r in all_visible:
            if r.pair_stem and r.file_type == FileType.RAW and \
                    (r.path.parent, r.pair_stem) in jpg_pair_keys:
                continue   # JPG counterpart will be shown instead
            visible.append(r)

        # Group by EXIF shot date (falls back to mtime when EXIF is absent)
        groups: Dict[_pydate, List[PhotoRecord]] = {}
        for r in visible:
            d = r.shot_time.date()
            if d not in groups:
                groups[d] = []
            groups[d].append(r)

        # Suppress repaints while we tear down / rebuild widgets to avoid flash
        self._container.setUpdatesEnabled(False)

        # Remove old sections
        for sec in self._sections:
            self._vlay.removeWidget(sec)
            sec.setParent(None)
            sec.deleteLater()
        self._sections.clear()
        self._path_to_sec.clear()

        # Insert new sections — section order follows sort direction when
        # sort key is "date"; for name/size, sections stay date-ascending.
        state = self._proxy.state
        reverse_sections = (state.sort_key == "date" and not state.sort_asc)
        insert_at = 0   # before the trailing stretch
        for d in sorted(groups.keys(), reverse=reverse_sections):
            records   = groups[d]
            collapsed = self._collapsed.get(d, False)
            sec = _DateSection(
                d, records, self._generator, self._thumb_size,
                collapsed=collapsed, parent=self._container,
            )
            sec.item_activated.connect(self.item_activated)
            sec.prune_toggled.connect(self.prune_toggled)
            self._vlay.insertWidget(insert_at, sec)
            self._sections.append(sec)
            for r in records:
                self._path_to_sec[r.path] = sec
            insert_at += 1

        self._container.updateGeometry()
        self._container.setUpdatesEnabled(True)
        # Restore scroll position — widget churn would otherwise reset it to 0
        QTimer.singleShot(0, lambda: self.verticalScrollBar().setValue(scroll_pos))
