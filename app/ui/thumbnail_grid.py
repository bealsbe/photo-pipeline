"""
Thumbnail grid view.

PhotoGridModel      QAbstractListModel backed by PhotoRecords.
                    DecorationRole triggers lazy thumbnail requests.
                    Qt.UserRole returns the PhotoRecord (for the filter proxy).
                    OPACITY_ROLE  returns a float [0, 1] used for fade-in.

ThumbnailDelegate   Custom painter: thumbnail image, file-type badge,
                    paired indicator dot, elided filename label.
                    Thumbnails cross-fade from placeholder → loaded image.

ThumbnailGridView   QListView in IconMode.  Wraps source model in a
                    PhotoFilterProxy for sorting/filtering.
                    Exposes item_activated(PhotoRecord) for Phase 3.
"""
from __future__ import annotations

from pathlib import Path
from typing import Dict, List, Optional

from PySide6.QtCore import (
    QAbstractListModel,
    QModelIndex,
    QRect,
    QSize,
    Qt,
    QTimer,
    Signal,
)
from PySide6.QtGui import (
    QColor,
    QFont,
    QFontMetrics,
    QPainter,
    QPixmap,
)
from PySide6.QtWidgets import (
    QAbstractItemView,
    QListView,
    QStyle,
    QStyledItemDelegate,
    QStyleOptionViewItem,
)

from app.models.photo_record import FileType, PhotoRecord
from app.thumbnails.generator import (
    PRIORITY_IDLE,
    PRIORITY_PREFETCH,
    PRIORITY_VISIBLE,
    ThumbnailGenerator,
)
from app.ui.proxy import FilterState, PhotoFilterProxy

_PREFETCH_LOOKAHEAD = 30   # rows to preload beyond the visible viewport edge

# Scaled-thumbnail cache: (path, width, height) → QPixmap
# Avoids re-running SmoothTransformation on every paintEvent for the same cell.
_SCALED_CACHE: Dict[tuple, QPixmap] = {}
_SCALED_CACHE_MAX = 512

def _get_scaled(pixmap: QPixmap, path: Path, size: QSize) -> QPixmap:
    key = (path, size.width(), size.height())
    cached = _SCALED_CACHE.get(key)
    if cached is not None:
        return cached
    scaled = pixmap.scaled(size, Qt.KeepAspectRatio, Qt.SmoothTransformation)
    if len(_SCALED_CACHE) >= _SCALED_CACHE_MAX:
        # Evict oldest ~10 % when full
        for k in list(_SCALED_CACHE)[:_SCALED_CACHE_MAX // 10]:
            del _SCALED_CACHE[k]
    _SCALED_CACHE[key] = scaled
    return scaled

def invalidate_scaled_cache(path: Path) -> None:
    """Remove all entries for *path* (call when a thumbnail is replaced)."""
    for key in [k for k in _SCALED_CACHE if k[0] == path]:
        del _SCALED_CACHE[key]

OPACITY_ROLE = Qt.UserRole + 1   # kept for role-numbering consistency


def _fmt_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.0f}\u202f{unit}"
        n //= 1024
    return f"{n:.0f}\u202fTB"


# Palette from bealsbe.github.io — dark blue-black base, orange accent
_BADGE_BG = {
    FileType.RAW: QColor(0x14, 0x32, 0x60),   # dark navy blue
    FileType.JPG: QColor(0x10, 0x3a, 0x1a),   # dark forest green
}
_PLACEHOLDER_BG = {
    FileType.RAW: QColor(0x0d, 0x14, 0x26),   # deep blue-black
    FileType.JPG: QColor(0x0a, 0x0a, 0x12),   # matches grid background
}

_CELL_BG        = QColor(0x0a, 0x0a, 0x12)   # matches scroll area background
_CELL_BG_HOVER  = QColor(0x14, 0x12, 0x20)   # subtle lift on hover
_CELL_BG_SEL    = QColor(0x22, 0x12, 0x04)   # dark orange tint for selection
_BORDER_HOVER   = QColor(0xff, 0x6d, 0x00, 40)    # faint orange outline
_BORDER_SEL     = QColor(0xff, 0x6d, 0x00, 160)   # vivid orange outline


# ──────────────────────────────────────────────────────────────────────────────
# Delegate
# ──────────────────────────────────────────────────────────────────────────────

class ThumbnailDelegate(QStyledItemDelegate):
    """
    Renders each grid cell:
      ┌──────────────────┐
      │  [thumbnail img] │  ← thumb_size × thumb_size px area
      │ ●  [RAW badge]   │  ← pair dot (●) + type badge overlay
      │  filename.jpg    │  ← elided label
      └──────────────────┘

    Thumbnails cross-fade from placeholder → loaded pixmap.
    """

    PAD = 4        # px padding around thumbnail (top/sides)
    LABEL_H = 17   # px for the badge + filename row below the thumbnail
    _BW = 26       # badge width in label row
    _BH = 15       # badge height in label row

    def __init__(self, thumb_size: int = 160, parent=None) -> None:
        super().__init__(parent)
        self.thumb_size = thumb_size

    def sizeHint(self, option: QStyleOptionViewItem, index: QModelIndex) -> QSize:
        w = self.thumb_size + self.PAD * 2
        h = self.thumb_size + self.LABEL_H + self.PAD + 2  # 2 px bottom margin
        return QSize(w, h)

    def paint(
        self,
        painter: QPainter,
        option: QStyleOptionViewItem,
        index: QModelIndex,
    ) -> None:
        painter.save()

        record: Optional[PhotoRecord] = index.data(Qt.UserRole)
        pixmap: Optional[QPixmap]     = index.data(Qt.DecorationRole)
        selected = bool(option.state & QStyle.State_Selected)
        hovered  = bool(option.state & QStyle.State_MouseOver)

        rect = option.rect
        p  = self.PAD
        ts = self.thumb_size

        # ── cell background ─────────────────────────────────────────── #
        if selected:
            painter.fillRect(rect, _CELL_BG_SEL)
        elif hovered:
            painter.fillRect(rect, _CELL_BG_HOVER)
        else:
            painter.fillRect(rect, _CELL_BG)

        # ── selection / hover border (inset 1 px) ───────────────────── #
        if selected or hovered:
            painter.setBrush(Qt.NoBrush)
            painter.setPen(_BORDER_SEL if selected else _BORDER_HOVER)
            painter.drawRect(rect.adjusted(0, 0, -1, -1))

        # ── thumbnail / placeholder ──────────────────────────────────── #
        thumb_rect = QRect(rect.x() + p, rect.y() + p, ts, ts)

        if pixmap and not pixmap.isNull():
            path = record.path if record else None
            scaled = (
                _get_scaled(pixmap, path, thumb_rect.size())
                if path else
                pixmap.scaled(thumb_rect.size(), Qt.KeepAspectRatio, Qt.SmoothTransformation)
            )
            x = thumb_rect.x() + (thumb_rect.width()  - scaled.width())  // 2
            y = thumb_rect.y() + (thumb_rect.height() - scaled.height()) // 2
            painter.drawPixmap(x, y, scaled)
        else:
            self._draw_placeholder(painter, thumb_rect, record, alpha=1.0)

        # ── pruned overlay ────────────────────────────────────────────── #
        if record and record.is_pruned:
            painter.fillRect(thumb_rect, QColor(160, 30, 30, 120))
            painter.setPen(QColor(255, 100, 100))
            xf = QFont()
            xf.setPointSize(max(14, ts // 9))
            xf.setBold(True)
            painter.setFont(xf)
            painter.drawText(thumb_rect, Qt.AlignCenter, "\u2715")

        # ── label row: filename  [badge(s)] ──────────────────────────── #
        bw = self._BW
        bh = self._BH
        # Vertically centre within the LABEL_H strip
        by = thumb_rect.bottom() + (self.LABEL_H - bh) // 2
        lx = rect.x() + p       # left edge of label area
        rx = rect.x() + p + ts  # right edge of label area

        # Badge block starts from the right
        if record and record.is_paired:
            badge_total = bw * 2 + 1
        elif record:
            badge_total = bw
        else:
            badge_total = 0

        badge_x = rx - badge_total
        fn_w = badge_x - lx - 3  # 3 px gap between filename and badges
        fn_w = max(fn_w, 0)

        # Filename (left side)
        label_color = QColor(0xff, 0x6d, 0x00) if selected else QColor(0x88, 0x88, 0xa8)
        painter.setPen(label_color)
        lf = QFont()
        lf.setPointSize(8)
        painter.setFont(lf)
        fm = QFontMetrics(lf)
        filename = (record.stem if record.is_paired else record.filename) if record else ""
        fn_rect = QRect(lx, by, fn_w, bh)
        painter.drawText(fn_rect, Qt.AlignVCenter | Qt.AlignLeft,
                         fm.elidedText(filename, Qt.ElideMiddle, fn_w))

        # Badges (right side)
        if badge_total:
            bf = QFont()
            bf.setPointSize(7)
            bf.setBold(True)
            painter.setFont(bf)
            painter.setPen(Qt.white)
            if record.is_paired:
                painter.fillRect(QRect(badge_x,         by, bw, bh), _BADGE_BG[FileType.RAW])
                painter.drawText(QRect(badge_x,         by, bw, bh), Qt.AlignCenter, "RAW")
                painter.fillRect(QRect(badge_x + bw + 1, by, bw, bh), _BADGE_BG[FileType.JPG])
                painter.drawText(QRect(badge_x + bw + 1, by, bw, bh), Qt.AlignCenter, "JPG")
            else:
                painter.fillRect(QRect(badge_x, by, bw, bh),
                                 _BADGE_BG.get(record.file_type, QColor(70, 70, 70)))
                painter.drawText(QRect(badge_x, by, bw, bh), Qt.AlignCenter,
                                 record.file_type.value)

        painter.restore()

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _draw_placeholder(
        self,
        painter: QPainter,
        rect: QRect,
        record: Optional[PhotoRecord],
        alpha: float,
    ) -> None:
        """Draw the placeholder background + type label at *alpha* opacity."""
        ft = record.file_type if record else None
        bg = _PLACEHOLDER_BG.get(ft, QColor(32, 32, 36))
        painter.setOpacity(alpha)
        painter.fillRect(rect, bg)
        if record:
            painter.setPen(QColor(0x28, 0x28, 0x44))
            ph_font = QFont()
            ph_font.setPointSize(max(8, self.thumb_size // 14))
            ph_font.setBold(True)
            painter.setFont(ph_font)
            painter.drawText(rect, Qt.AlignCenter, record.file_type.value)
        painter.setOpacity(1.0)


# ──────────────────────────────────────────────────────────────────────────────
# Source model
# ──────────────────────────────────────────────────────────────────────────────

class PhotoGridModel(QAbstractListModel):
    """
    Flat list model of PhotoRecords for the grid view.

    • Qt.DisplayRole   → filename string (used by default sort proxy)
    • Qt.DecorationRole → QPixmap (from cache) or None (triggers lazy request)
    • Qt.UserRole      → PhotoRecord (used by filter proxy and delegate)
    • OPACITY_ROLE     → float [0..1] — current fade-in opacity for new thumbs
    • Qt.ToolTipRole   → multi-line tooltip
    """

    def __init__(self, generator: ThumbnailGenerator, parent=None) -> None:
        super().__init__(parent)
        self._records: List[PhotoRecord] = []
        self._path_row: Dict[Path, int]  = {}
        self._generator = generator
        generator.thumbnail_ready.connect(self._on_thumbnail_ready)

    # ------------------------------------------------------------------ #
    # Mutation (main thread only)                                          #
    # ------------------------------------------------------------------ #

    def reset_records(self, records: List[PhotoRecord]) -> None:
        self.beginResetModel()
        self._records = list(records)
        self._path_row = {r.path: i for i, r in enumerate(self._records)}
        self.endResetModel()

    def append(self, record: PhotoRecord) -> None:
        row = len(self._records)
        self.beginInsertRows(QModelIndex(), row, row)
        self._records.append(record)
        self._path_row[record.path] = row
        self.endInsertRows()

    def append_batch(self, records: List[PhotoRecord]) -> None:
        """Insert multiple records in one beginInsertRows/endInsertRows call."""
        if not records:
            return
        first = len(self._records)
        last  = first + len(records) - 1
        self.beginInsertRows(QModelIndex(), first, last)
        for r in records:
            self._path_row[r.path] = len(self._records)
            self._records.append(r)
        self.endInsertRows()

    def record_at(self, row: int) -> PhotoRecord:
        return self._records[row]

    def remove_records(self, records: List[PhotoRecord]) -> None:
        """Remove records from the model using surgical row operations."""
        path_set = {r.path for r in records}
        # Collect contiguous runs of rows to remove so we issue as few
        # beginRemoveRows calls as possible (Qt is happiest with contiguous spans).
        rows_to_remove = sorted(
            i for i, r in enumerate(self._records) if r.path in path_set
        )
        if not rows_to_remove:
            return
        # Walk runs in reverse so indices stay valid as we remove
        runs: list[tuple[int, int]] = []
        start = rows_to_remove[0]
        end   = rows_to_remove[0]
        for r in rows_to_remove[1:]:
            if r == end + 1:
                end = r
            else:
                runs.append((start, end))
                start = end = r
        runs.append((start, end))

        for first, last in reversed(runs):
            self.beginRemoveRows(QModelIndex(), first, last)
            del self._records[first:last + 1]
            self.endRemoveRows()

        # Rebuild path→row index after removal
        self._path_row = {r.path: i for i, r in enumerate(self._records)}

    def notify_records_changed(self, records: List[PhotoRecord]) -> None:
        """Emit dataChanged for rows whose records are in *records*."""
        path_set = {r.path for r in records}
        for path, row in self._path_row.items():
            if path in path_set:
                idx = self.index(row)
                self.dataChanged.emit(idx, idx)

    # ------------------------------------------------------------------ #
    # QAbstractListModel                                                   #
    # ------------------------------------------------------------------ #

    def rowCount(self, parent: QModelIndex = QModelIndex()) -> int:
        return 0 if parent.isValid() else len(self._records)

    def data(self, index: QModelIndex, role: int = Qt.DisplayRole):
        if not index.isValid():
            return None
        record = self._records[index.row()]

        if role == Qt.DisplayRole:
            return record.filename

        if role == Qt.DecorationRole:
            cached = self._generator.get_cached(record.path)
            if cached:
                return cached
            # Item is being painted → it's on screen → highest priority
            self._generator.request(record, PRIORITY_VISIBLE)
            return None

        if role == Qt.UserRole:
            return record

        if role == Qt.ToolTipRole:
            parts = [
                record.filename,
                f"{record.file_type.value}  •  {_fmt_size(record.file_size)}",
                record.modified_time.strftime("%Y-%m-%d  %H:%M"),
            ]
            if record.is_paired:
                parts.append(f"Paired  (stem: {record.pair_stem})")
            return "\n".join(parts)

        return None

    # ------------------------------------------------------------------ #
    # Slots                                                                #
    # ------------------------------------------------------------------ #

    def _on_thumbnail_ready(self, path: Path, _pixmap: QPixmap) -> None:
        invalidate_scaled_cache(path)
        row = self._path_row.get(path)
        if row is not None:
            idx = self.index(row)
            self.dataChanged.emit(idx, idx, [Qt.DecorationRole])


# ──────────────────────────────────────────────────────────────────────────────
# View
# ──────────────────────────────────────────────────────────────────────────────

_SCROLLBAR_QSS = """
    QListView {
        background: #0a0a12;
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
    QScrollBar::handle:vertical:pressed { background: rgba(255,109,0,0.70); }
    QScrollBar::add-line:vertical,
    QScrollBar::sub-line:vertical {
        height: 0;
        background: none;
    }
    QScrollBar::add-page:vertical,
    QScrollBar::sub-page:vertical {
        background: transparent;
    }
"""


class ThumbnailGridView(QListView):
    """
    Responsive icon-mode grid.  The internal PhotoFilterProxy handles
    both sorting and category filtering.

    item_activated(PhotoRecord) is reserved for the single-image viewer.
    """

    item_activated: Signal = Signal(object)  # PhotoRecord
    prune_toggled:  Signal = Signal(object)  # List[PhotoRecord]

    def __init__(self, generator: ThumbnailGenerator, thumb_size: int = 160, parent=None) -> None:
        super().__init__(parent)
        self._thumb_size = thumb_size
        self._generator  = generator

        self._source_model = PhotoGridModel(generator, self)
        self._proxy        = PhotoFilterProxy(self)
        self._proxy.setSourceModel(self._source_model)
        self._delegate     = ThumbnailDelegate(thumb_size, self)

        self.setModel(self._proxy)
        self.setItemDelegate(self._delegate)

        self.setViewMode(QListView.IconMode)
        self.setResizeMode(QListView.Adjust)
        self.setWrapping(True)
        self.setUniformItemSizes(True)
        self.setSpacing(4)
        self.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.setMouseTracking(True)

        # Pixel-accurate scrolling for buttery smooth feel
        self.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
        self.setHorizontalScrollMode(QAbstractItemView.ScrollPerPixel)

        self.setStyleSheet(_SCROLLBAR_QSS)
        self._update_grid_size()

        # Scroll-settle prefetch: fire 80 ms after scrolling stops
        self._prefetch_timer = QTimer(self)
        self._prefetch_timer.setSingleShot(True)
        self._prefetch_timer.setInterval(80)
        self._prefetch_timer.timeout.connect(self._prefetch_nearby)
        self.verticalScrollBar().valueChanged.connect(self._prefetch_timer.start)

        self.activated.connect(self._on_activated)

    # ------------------------------------------------------------------ #
    # Public helpers                                                       #
    # ------------------------------------------------------------------ #

    def source_model(self) -> PhotoGridModel:
        return self._source_model

    def filter_proxy(self) -> PhotoFilterProxy:
        return self._proxy

    def apply_filter(self, state: FilterState) -> None:
        self._proxy.apply_state(state)

    def selected_records(self) -> List[PhotoRecord]:
        records = []
        for idx in self.selectionModel().selectedIndexes():
            record = idx.data(Qt.UserRole)
            if record:
                records.append(record)
        return records

    def visible_count(self) -> int:
        return self._proxy.rowCount()

    def all_visible_records(self) -> List[PhotoRecord]:
        """Return all records in current proxy (filter + sort) order."""
        records = []
        for row in range(self._proxy.rowCount()):
            record = self._proxy.index(row, 0).data(Qt.UserRole)
            if record:
                records.append(record)
        return records

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _update_grid_size(self) -> None:
        p  = ThumbnailDelegate.PAD
        lh = ThumbnailDelegate.LABEL_H
        ts = self._thumb_size
        cell_w = ts + p * 2
        cell_h = ts + lh + p * 2
        self.setGridSize(QSize(cell_w + 6, cell_h + 6))
        self.setIconSize(QSize(ts, ts))

    def _prefetch_nearby(self) -> None:
        """
        Called ~80 ms after scrolling stops.

        1. Flush the queue (clears mid-scroll backlog).
        2. Re-queue currently visible items at PRIORITY_VISIBLE.
        3. Re-queue ±LOOKAHEAD band at PRIORITY_PREFETCH.
        """
        vp    = self.viewport().rect()
        total = self._proxy.rowCount()
        if total == 0:
            return

        first_idx = self.indexAt(vp.topLeft())
        last_idx  = self.indexAt(vp.bottomRight())

        first_row = first_idx.row() if first_idx.isValid() else 0
        last_row  = last_idx.row()  if last_idx.isValid()  else first_row

        self._generator.clear_queue()

        for row in range(first_row, last_row + 1):
            record = self._proxy.index(row, 0).data(Qt.UserRole)
            if record:
                self._generator.request(record, PRIORITY_VISIBLE)

        band_first = max(0, first_row - _PREFETCH_LOOKAHEAD)
        band_last  = min(total - 1, last_row + _PREFETCH_LOOKAHEAD)

        for row in range(band_first, band_last + 1):
            if first_row <= row <= last_row:
                continue
            record = self._proxy.index(row, 0).data(Qt.UserRole)
            if record:
                self._generator.request(record, PRIORITY_PREFETCH)

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
        record = proxy_index.data(Qt.UserRole)
        if record:
            self.item_activated.emit(record)
