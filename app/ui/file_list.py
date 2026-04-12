"""
FileListWidget — sortable, filterable QTableView backed by PhotoPairModel.

Paired RAW+JPG files are consolidated into a single row.  The filename shown
is always the RAW name (stem only for pairs, full name for unpaired files).
The Type column shows "RAW+JPG" for pairs, "RAW" or "JPG" for singles.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Dict, List, Optional

from PySide6.QtCore import (
    QAbstractTableModel,
    QModelIndex,
    Qt,
    Signal,
)
from PySide6.QtGui import QColor, QFont
from PySide6.QtWidgets import (
    QAbstractItemView,
    QHeaderView,
    QTableView,
)

from app.models.photo_record import FileType, PhotoRecord
from app.ui.proxy import FilterState, PhotoFilterProxy

# ── Column definitions ────────────────────────────────────────────────────── #
COL_FILE     = 0
COL_TYPE     = 1
COL_SIZE     = 2
COL_MODIFIED = 3
HEADERS = ["Filename", "Type", "Size", "Modified"]

_COL_RAW    = QColor("#7ab0ff")
_COL_JPG    = QColor("#7aba90")
_COL_PAIR   = QColor("#ff6d00")
_COL_MUTED  = QColor("#2a2a3a")


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}\u202f{unit}"
        n //= 1024
    return f"{n:.1f}\u202fTB"


# ──────────────────────────────────────────────────────────────────────────────
# Pair row
# ──────────────────────────────────────────────────────────────────────────────

@dataclass
class _PairRow:
    """One visible row: a paired (RAW + JPG) or an unpaired single file."""
    raw: Optional[PhotoRecord] = field(default=None)
    jpg: Optional[PhotoRecord] = field(default=None)

    @property
    def primary(self) -> PhotoRecord:
        return self.raw if self.raw is not None else self.jpg  # type: ignore[return-value]

    @property
    def is_paired(self) -> bool:
        return self.raw is not None and self.jpg is not None

    @property
    def display_name(self) -> str:
        """RAW stem for pairs, full filename otherwise."""
        if self.is_paired and self.raw:
            return self.raw.stem
        return self.primary.filename

    @property
    def type_label(self) -> str:
        if self.is_paired:
            return "RAW+JPG"
        return self.primary.file_type.value

    @property
    def file_size(self) -> int:
        return (self.raw.file_size if self.raw else 0) + \
               (self.jpg.file_size if self.jpg else 0)

    @property
    def modified_time(self) -> datetime:
        candidates = [r.modified_time for r in (self.raw, self.jpg) if r]
        return max(candidates)


# ──────────────────────────────────────────────────────────────────────────────
# Source model
# ──────────────────────────────────────────────────────────────────────────────

class PhotoTableModel(QAbstractTableModel):
    """
    Table model over consolidated pair rows.

    reset_records() groups PhotoRecords by pair_stem so that each RAW/JPG
    pair occupies one row.  Unpaired files get their own row.

    Qt.UserRole on column 0 returns the primary PhotoRecord so
    PhotoFilterProxy continues to work unchanged.
    """

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self._rows: List[_PairRow] = []

    # ------------------------------------------------------------------ #
    # Mutation (main thread only)                                          #
    # ------------------------------------------------------------------ #

    def reset_records(self, records: List[PhotoRecord]) -> None:
        pair_map: Dict[str, _PairRow] = {}
        rows: List[_PairRow] = []

        for r in records:
            if r.pair_stem:
                if r.pair_stem not in pair_map:
                    row = _PairRow()
                    pair_map[r.pair_stem] = row
                    rows.append(row)
                row = pair_map[r.pair_stem]
                if r.file_type == FileType.RAW:
                    row.raw = r
                else:
                    row.jpg = r
            else:
                rows.append(
                    _PairRow(raw=r) if r.file_type == FileType.RAW else _PairRow(jpg=r)
                )

        self.beginResetModel()
        self._rows = rows
        self.endResetModel()

    def append_batch(self, records: List[PhotoRecord]) -> None:
        """During scanning, records have no pair_stem yet — each gets its own row."""
        if not records:
            return
        new_rows = [
            _PairRow(raw=r) if r.file_type == FileType.RAW else _PairRow(jpg=r)
            for r in records
        ]
        first = len(self._rows)
        last  = first + len(new_rows) - 1
        self.beginInsertRows(QModelIndex(), first, last)
        self._rows.extend(new_rows)
        self.endInsertRows()

    def row_at(self, row: int) -> _PairRow:
        return self._rows[row]

    def remove_records(self, records: List[PhotoRecord]) -> None:
        path_set = {r.path for r in records}
        # Rows where both sides are removed → surgical row deletion.
        # Rows where only one side is removed → in-place mutation + dataChanged.
        fully_gone: list[int] = []
        mutated: list[int]    = []

        for i, prow in enumerate(self._rows):
            raw_gone = prow.raw is not None and prow.raw.path in path_set
            jpg_gone = prow.jpg is not None and prow.jpg.path in path_set
            if raw_gone and jpg_gone:
                fully_gone.append(i)
            elif raw_gone:
                prow.raw = None
                mutated.append(i)
            elif jpg_gone:
                prow.jpg = None
                mutated.append(i)

        # Emit dataChanged for mutated rows
        n_cols = len(HEADERS)
        for i in mutated:
            self.dataChanged.emit(self.index(i, 0), self.index(i, n_cols - 1))

        # Remove fully-gone rows in reverse order (contiguous runs where possible)
        if not fully_gone:
            return
        runs: list[tuple[int, int]] = []
        start = fully_gone[0]; end = fully_gone[0]
        for r in fully_gone[1:]:
            if r == end + 1:
                end = r
            else:
                runs.append((start, end)); start = end = r
        runs.append((start, end))
        for first, last in reversed(runs):
            self.beginRemoveRows(QModelIndex(), first, last)
            del self._rows[first:last + 1]
            self.endRemoveRows()

    def notify_records_changed(self, records: List[PhotoRecord]) -> None:
        path_set = {r.path for r in records}
        n_cols = len(HEADERS)
        for row, prow in enumerate(self._rows):
            if (prow.raw and prow.raw.path in path_set) or \
               (prow.jpg and prow.jpg.path in path_set):
                self.dataChanged.emit(
                    self.index(row, 0),
                    self.index(row, n_cols - 1),
                )

    # ------------------------------------------------------------------ #
    # QAbstractTableModel                                                  #
    # ------------------------------------------------------------------ #

    def rowCount(self, parent: QModelIndex = QModelIndex()) -> int:
        return 0 if parent.isValid() else len(self._rows)

    def columnCount(self, parent: QModelIndex = QModelIndex()) -> int:
        return 0 if parent.isValid() else len(HEADERS)

    def headerData(
        self,
        section: int,
        orientation: Qt.Orientation,
        role: int = Qt.DisplayRole,
    ) -> Optional[str]:
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return HEADERS[section]
        return None

    def data(self, index: QModelIndex, role: int = Qt.DisplayRole):
        if not index.isValid():
            return None

        prow = self._rows[index.row()]
        col  = index.column()

        if role == Qt.DisplayRole:
            if col == COL_FILE:     return prow.display_name
            if col == COL_TYPE:     return prow.type_label
            if col == COL_SIZE:     return _fmt_size(prow.file_size)
            if col == COL_MODIFIED: return prow.modified_time.strftime("%Y-%m-%d  %H:%M")

        if role == Qt.ForegroundRole:
            if prow.primary.is_pruned:
                return QColor(160, 50, 50)
            if col == COL_TYPE:
                if prow.is_paired:
                    return _COL_PAIR
                return _COL_RAW if prow.raw else _COL_JPG
            return None

        if role == Qt.FontRole:
            if prow.primary.is_pruned:
                f = QFont()
                f.setStrikeOut(True)
                return f
            return None

        if role == Qt.UserRole:
            return prow.primary

        if role == Qt.ToolTipRole and col == COL_FILE:
            lines = []
            if prow.raw:
                lines.append(f"RAW: {prow.raw.path}")
            if prow.jpg:
                lines.append(f"JPG: {prow.jpg.path}")
            return "\n".join(lines) if lines else None

        return None


# ──────────────────────────────────────────────────────────────────────────────
# View
# ──────────────────────────────────────────────────────────────────────────────

class FileListWidget(QTableView):
    """Sortable, filterable table backed by PhotoTableModel + PhotoFilterProxy."""

    item_activated: Signal = Signal(object)   # PhotoRecord
    prune_toggled:  Signal = Signal(object)   # List[PhotoRecord]

    def __init__(self, parent=None) -> None:
        super().__init__(parent)

        self._source_model = PhotoTableModel(self)
        self._proxy = PhotoFilterProxy(self)
        self._proxy.setSourceModel(self._source_model)

        self.setModel(self._proxy)
        self.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.setAlternatingRowColors(True)
        self.setShowGrid(False)
        self.setSortingEnabled(False)
        self.setWordWrap(False)

        hdr = self.horizontalHeader()
        hdr.setSectionResizeMode(COL_FILE,     QHeaderView.Stretch)
        hdr.setSectionResizeMode(COL_TYPE,     QHeaderView.ResizeToContents)
        hdr.setSectionResizeMode(COL_SIZE,     QHeaderView.ResizeToContents)
        hdr.setSectionResizeMode(COL_MODIFIED, QHeaderView.ResizeToContents)

        self.verticalHeader().setDefaultSectionSize(22)
        self.verticalHeader().hide()

        self.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
        self.setHorizontalScrollMode(QAbstractItemView.ScrollPerPixel)
        self.setStyleSheet("""
            QTableView {
                background: #0a0a12;
                gridline-color: transparent;
                border: none;
                outline: none;
            }
            QScrollBar:vertical {
                background: #0a0a12;
                width: 7px;
                border: none;
                margin: 0;
            }
            QScrollBar::handle:vertical {
                background: #2a2a40;
                border-radius: 3px;
                min-height: 24px;
            }
            QScrollBar::handle:vertical:hover   { background: rgba(255,109,0,0.45); }
            QScrollBar::handle:vertical:pressed  { background: rgba(255,109,0,0.70); }
            QScrollBar::add-line:vertical,
            QScrollBar::sub-line:vertical { height: 0; background: none; }
            QScrollBar::add-page:vertical,
            QScrollBar::sub-page:vertical { background: transparent; }
        """)

        self.activated.connect(self._on_activated)

    # ------------------------------------------------------------------ #
    # Public helpers                                                       #
    # ------------------------------------------------------------------ #

    def source_model(self) -> PhotoTableModel:
        return self._source_model

    def filter_proxy(self) -> PhotoFilterProxy:
        return self._proxy

    def apply_filter(self, state: FilterState) -> None:
        self._proxy.apply_state(state)

    def selected_records(self) -> List[PhotoRecord]:
        """Return all PhotoRecords (both RAW and JPG) for selected rows."""
        records: List[PhotoRecord] = []
        for proxy_idx in self.selectionModel().selectedRows():
            src_idx = self._proxy.mapToSource(proxy_idx)
            prow    = self._source_model.row_at(src_idx.row())
            if prow.raw:
                records.append(prow.raw)
            if prow.jpg:
                records.append(prow.jpg)
        return records

    def visible_count(self) -> int:
        return self._proxy.rowCount()

    def all_visible_records(self) -> List[PhotoRecord]:
        """Return the primary record per visible row (used by ImageViewer)."""
        records: List[PhotoRecord] = []
        for row in range(self._proxy.rowCount()):
            src  = self._proxy.mapToSource(self._proxy.index(row, 0))
            prow = self._source_model.row_at(src.row())
            records.append(prow.primary)
        return records

    def keyPressEvent(self, event) -> None:
        if event.key() in (Qt.Key_P, Qt.Key_Delete):
            records = self.selected_records()
            if records:
                all_pruned = all(r.is_pruned for r in records)
                for r in records:
                    r.is_pruned = not all_pruned
                self.prune_toggled.emit(records)
        else:
            super().keyPressEvent(event)

    def _on_activated(self, proxy_index: QModelIndex) -> None:
        src  = self._proxy.mapToSource(proxy_index)
        prow = self._source_model.row_at(src.row())
        if prow:
            self.item_activated.emit(prow.primary)
