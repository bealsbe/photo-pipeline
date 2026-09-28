"""
PhotoFilterProxy — shared sort + filter proxy used by both the list and grid views.

Ordering lives in the source model, not here
--------------------------------------------
QSortFilterProxyModel drives sorting by calling ``lessThan`` once per
comparison.  Implemented in Python that is roughly n·log n trips across the
C++/Python boundary, and it dominated everything else the app did: handing
10 000 records to a bare model took 17.5 ms, and 2077 ms with a proxy
attached.  Loading a folder paid it twice — once for the grid, once for the
file list — which was about 96% of the multi-second freeze after a scan.

So the proxy no longer sorts.  Source models expose ``set_sort`` and keep
their rows in the requested order, which Python's Timsort does in under a
millisecond for the same 10 000 records; the proxy only filters, and proxy
row order is therefore source row order.  Filtering stays here because it has
to be incremental — Qt re-runs it on insert and on dataChanged.

Source models must provide:
    record_for_row(row)   -> PhotoRecord | None
    records_in_order()    -> list[PhotoRecord]   (one entry per model row)
    set_sort(key, asc)    -> None
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date as _pydate
from typing import List, Optional

from PySide6.QtCore import QModelIndex, QSortFilterProxyModel, Qt

from app.models.photo_record import FileType, PhotoRecord


def sort_key_func(sort_key: str):
    """Return the ``key=`` callable a source model should sort records by."""
    if sort_key == "name":
        return lambda r: r.filename.lower()
    if sort_key == "size":
        return lambda r: r.file_size
    return lambda r: r.shot_time          # "date" (default)


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
    show_marked:   bool = True
    show_unmarked: bool = True
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
        # Re-filter automatically when a record changes (prune, mark, pair).
        # Note we never call sort() — ordering belongs to the source model.
        self.setDynamicSortFilter(True)

    def setSourceModel(self, model) -> None:   # noqa: N802 — Qt override
        super().setSourceModel(model)
        self._push_sort()

    def apply_state(self, state: FilterState) -> None:
        prev = (self._state.sort_key, self._state.sort_asc)
        curr = (state.sort_key,       state.sort_asc)
        self._state = state
        if prev != curr:
            self._push_sort()
        self.invalidateFilter()

    @property
    def state(self) -> FilterState:
        return self._state

    # ------------------------------------------------------------------ #
    # Ordering (delegated to the source model)                             #
    # ------------------------------------------------------------------ #

    def _push_sort(self) -> None:
        model = self.sourceModel()
        if model is not None and hasattr(model, "set_sort"):
            model.set_sort(self._state.sort_key, self._state.sort_asc)

    # ------------------------------------------------------------------ #
    # Bulk access                                                          #
    # ------------------------------------------------------------------ #

    def visible_records(self) -> List[PhotoRecord]:
        """
        Every accepted record, in proxy row order.

        Equivalent to walking rowCount() and reading Qt.UserRole, but done
        entirely in Python — no index construction, no data() dispatch.  The
        grid rebuilds from this on every filter change, so at several thousand
        records the difference is the whole cost of the rebuild.
        """
        model = self.sourceModel()
        if model is None or not hasattr(model, "records_in_order"):
            return []
        return [r for r in model.records_in_order() if r is not None and self._accept(r)]

    # ------------------------------------------------------------------ #
    # Filtering                                                            #
    # ------------------------------------------------------------------ #

    def filterAcceptsRow(
        self, source_row: int, source_parent: QModelIndex
    ) -> bool:
        model = self.sourceModel()
        if hasattr(model, "record_for_row"):
            # Direct list access — skips building a QModelIndex and dispatching
            # through data(), which is most of the per-row cost at scale.
            record = model.record_for_row(source_row)
        else:
            record = model.data(model.index(source_row, 0, source_parent), Qt.UserRole)
        return True if record is None else self._accept(record)

    def _accept(self, record: PhotoRecord) -> bool:
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
        if record.is_marked_for_export     and not s.show_marked:
            return False
        if not record.is_marked_for_export and not s.show_unmarked:
            return False

        # No date bounds is the common case — don't build a date object for it.
        if s.date_from is not None or s.date_to is not None:
            rec_date = record.shot_time.date()
            if s.date_from is not None and rec_date < s.date_from:
                return False
            if s.date_to is not None   and rec_date > s.date_to:
                return False

        return True

    # ------------------------------------------------------------------ #
    # Sorting                                                              #
    # ------------------------------------------------------------------ #
    #
    # Deliberately no lessThan(): sort() is never called on this proxy, so Qt
    # leaves rows in source order.  See the module docstring — ordering is the
    # source model's job, via set_sort().
