"""
PhotoFilterProxy — shared sort + filter proxy used by both the list and grid views.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date as _pydate
from typing import Optional

from PySide6.QtCore import QModelIndex, QSortFilterProxyModel, Qt

from app.models.photo_record import FileType, PhotoRecord


@dataclass
class FilterState:
    """Combined filter + sort snapshot shared across both views."""
    # category toggles
    show_raw:      bool = True
    show_jpg:      bool = True
    show_paired:   bool = True
    show_unpaired: bool = True
    show_pruned:   bool = True
    show_unpruned: bool = True
    # date range  (None = no bound)
    date_from: Optional[_pydate] = field(default=None)
    date_to:   Optional[_pydate] = field(default=None)
    # sort
    sort_key: str  = "date"   # "date" | "name" | "size"
    sort_asc: bool = True     # True = ascending (oldest / A-Z / smallest first)


class PhotoFilterProxy(QSortFilterProxyModel):
    """
    Drop-in replacement for QSortFilterProxyModel that adds file-type,
    pair-status, date-range filtering AND custom sorting on top of the
    standard proxy.

    Works for both FileListWidget (table) and ThumbnailGridView (grid) —
    both expose their PhotoRecord via Qt.UserRole on column 0.
    """

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self._state = FilterState()
        self.setSortCaseSensitivity(Qt.CaseInsensitive)
        self.setDynamicSortFilter(True)
        # Default: oldest first
        self.sort(0, Qt.AscendingOrder)

    def apply_state(self, state: FilterState) -> None:
        prev = (self._state.sort_key, self._state.sort_asc)
        curr = (state.sort_key,       state.sort_asc)
        self._state = state
        self.invalidateFilter()
        if prev != curr:
            order = Qt.AscendingOrder if state.sort_asc else Qt.DescendingOrder
            self.sort(0, order)

    @property
    def state(self) -> FilterState:
        return self._state

    # ------------------------------------------------------------------ #
    # Filtering                                                            #
    # ------------------------------------------------------------------ #

    def filterAcceptsRow(
        self, source_row: int, source_parent: QModelIndex
    ) -> bool:
        idx = self.sourceModel().index(source_row, 0, source_parent)
        record: PhotoRecord | None = self.sourceModel().data(idx, Qt.UserRole)
        if record is None:
            return True

        s = self._state

        if record.file_type == FileType.RAW and not s.show_raw:
            return False
        if record.file_type == FileType.JPG and not s.show_jpg:
            return False
        if record.is_paired     and not s.show_paired:
            return False
        if not record.is_paired and not s.show_unpaired:
            return False
        if record.is_pruned     and not s.show_pruned:
            return False
        if not record.is_pruned and not s.show_unpruned:
            return False

        rec_date = record.shot_time.date()
        if s.date_from is not None and rec_date < s.date_from:
            return False
        if s.date_to is not None   and rec_date > s.date_to:
            return False

        return True

    # ------------------------------------------------------------------ #
    # Sorting                                                              #
    # ------------------------------------------------------------------ #

    def lessThan(self, left: QModelIndex, right: QModelIndex) -> bool:
        rl: PhotoRecord | None = left.data(Qt.UserRole)
        rr: PhotoRecord | None = right.data(Qt.UserRole)
        if rl is None or rr is None:
            return False

        key = self._state.sort_key
        if key == "name":
            return rl.filename.lower() < rr.filename.lower()
        if key == "size":
            return rl.file_size < rr.file_size
        # "date" (default)
        return rl.shot_time < rr.shot_time
