"""
Background thumbnail generation pipeline.

Decode strategy per file type
------------------------------
JPG/JPEG
    QImageReader with setScaledSize() — libjpeg DCT-domain scaling, very fast.

RAW  (rawpy available)
    1. extract_thumb() — pull the embedded JPEG the camera already wrote.
       Almost as fast as reading a plain JPEG; no demosaicing needed.
       Handles both JPEG and BITMAP embedded formats.
    2. Full postprocess() fallback — half_size=True halves decode time;
       used only when no embedded thumbnail exists (rare).
    3. QImageReader fallback — catches formats rawpy can't open (e.g. some
       camera-vendor DNG dialects that Qt handles natively).

RAW  (rawpy not installed)
    QImageReader only — will succeed for DNG on most platforms, show a
    placeholder for proprietary RAW formats.

Priority constants (passed to QThreadPool.start)
-------------------------------------------------
Higher value = runs sooner among queued-but-not-started workers.
    PRIORITY_VISIBLE  (10) — item is being painted right now
    PRIORITY_PREFETCH  (2) — just outside the visible viewport
    PRIORITY_IDLE      (0) — background / not yet scrolled to

Cache
-----
LRU in-memory cache (default 500 entries, ~50 MB at 160 px).
Oldest entry evicted when full.

Thread pool
-----------
min(8, max(4, idealThreadCount())) workers.  Thumbnail generation is
I/O-bound on SSDs, so more threads than CPU cores pays off here.
"""
from __future__ import annotations

from collections import OrderedDict
from pathlib import Path
from typing import Optional, Set

from PySide6.QtCore import (
    QBuffer,
    QByteArray,
    QIODevice,
    QObject,
    QRect,
    QRunnable,
    QThread,
    QThreadPool,
    Qt,
    Signal,
)
from PySide6.QtGui import QColor, QFont, QImage, QImageReader, QPainter, QPixmap

from app.models.photo_record import FileType, PhotoRecord
from app.thumbnails.disk_cache import DiskThumbnailCache

# Optional rawpy — degrade gracefully if not installed
try:
    import rawpy as _rawpy
    import numpy as _np
    _RAWPY_AVAILABLE = True
except ImportError:
    _RAWPY_AVAILABLE = False

PRIORITY_VISIBLE  = 10
PRIORITY_PREFETCH = 2
PRIORITY_IDLE     = 0

_DEFAULT_CACHE_LIMIT = 500


# ──────────────────────────────────────────────────────────────────────────────
# LRU cache
# ──────────────────────────────────────────────────────────────────────────────

class _LRUCache:
    def __init__(self, maxsize: int = _DEFAULT_CACHE_LIMIT) -> None:
        self._data: OrderedDict[Path, QPixmap] = OrderedDict()
        self._maxsize = maxsize

    def get(self, key: Path) -> Optional[QPixmap]:
        if key not in self._data:
            return None
        self._data.move_to_end(key)
        return self._data[key]

    def put(self, key: Path, value: QPixmap) -> None:
        if key in self._data:
            self._data.move_to_end(key)
        self._data[key] = value
        while len(self._data) > self._maxsize:
            self._data.popitem(last=False)

    def __contains__(self, key: Path) -> bool:
        return key in self._data

    def clear(self) -> None:
        self._data.clear()

    def __len__(self) -> int:
        return len(self._data)


# ──────────────────────────────────────────────────────────────────────────────
# Worker
# ──────────────────────────────────────────────────────────────────────────────

class _WorkerSignals(QObject):
    done: Signal = Signal(object, object)  # (Path, QImage)


class ThumbnailWorker(QRunnable):
    """Generates one thumbnail QImage in a thread-pool worker thread."""

    def __init__(
        self,
        record: PhotoRecord,
        thumb_size: int,
        signals: _WorkerSignals,
        disk_cache: Optional[DiskThumbnailCache] = None,
    ) -> None:
        super().__init__()
        self.record = record
        self.thumb_size = thumb_size
        self.signals = signals
        self._disk_cache = disk_cache
        self.setAutoDelete(True)

    def run(self) -> None:
        # 1. Check disk cache first (fast path — no decode needed)
        if self._disk_cache is not None:
            cached = self._disk_cache.get(self.record)
            if cached is not None:
                self.signals.done.emit(self.record.path, cached)
                return

        # 2. Decode the image
        image = self._generate()

        # 3. Persist to disk for next session
        if self._disk_cache is not None and not image.isNull():
            self._disk_cache.put(self.record, image)

        self.signals.done.emit(self.record.path, image)

    # ------------------------------------------------------------------ #
    # Dispatch                                                             #
    # ------------------------------------------------------------------ #

    def _generate(self) -> QImage:
        if self.record.file_type == FileType.RAW and _RAWPY_AVAILABLE:
            img = self._generate_raw_rawpy()
            if not img.isNull():
                return img
            # rawpy failed — try Qt (works for some DNG)
        img = self._generate_qimagereader(str(self.record.path))
        if not img.isNull():
            return img
        return self._make_placeholder()

    # ------------------------------------------------------------------ #
    # RAW via rawpy                                                        #
    # ------------------------------------------------------------------ #

    def _generate_raw_rawpy(self) -> QImage:
        try:
            with _rawpy.imread(str(self.record.path)) as raw:
                # ── fast path: embedded JPEG/bitmap ─────────────────── #
                try:
                    thumb = raw.extract_thumb()
                    if thumb.format == _rawpy.ThumbFormat.JPEG:
                        img = self._qimage_from_jpeg_bytes(bytes(thumb.data))
                        if not img.isNull():
                            return img
                    elif thumb.format == _rawpy.ThumbFormat.BITMAP:
                        img = self._qimage_from_numpy(thumb.data)
                        if not img.isNull():
                            return self._fit(img)
                except Exception:
                    pass  # no embedded thumb — fall through

                # ── slow path: full demosaic (half_size for speed) ───── #
                try:
                    rgb = raw.postprocess(
                        use_camera_wb=True,
                        half_size=True,      # halves decode time, fine for thumbs
                        no_auto_bright=False,
                        output_bps=8,
                    )
                    img = self._qimage_from_numpy(rgb)
                    if not img.isNull():
                        return self._fit(img)
                except Exception:
                    pass

        except Exception:
            pass

        return QImage()  # signal failure to caller

    # ------------------------------------------------------------------ #
    # JPEG / Qt-readable formats                                           #
    # ------------------------------------------------------------------ #

    def _generate_qimagereader(self, path: str) -> QImage:
        reader = QImageReader(path)
        reader.setAutoTransform(True)
        if not reader.canRead():
            return QImage()
        orig = reader.size()
        if orig.isValid() and orig.width() > 0 and orig.height() > 0:
            reader.setScaledSize(
                orig.scaled(self.thumb_size, self.thumb_size, Qt.KeepAspectRatio)
            )
        img = reader.read()
        return img if not img.isNull() else QImage()

    # ------------------------------------------------------------------ #
    # Helpers                                                              #
    # ------------------------------------------------------------------ #

    def _qimage_from_jpeg_bytes(self, data: bytes) -> QImage:
        """Decode a JPEG byte blob via QImageReader with DCT scaling."""
        ba = QByteArray(data)
        buf = QBuffer(ba)
        buf.open(QIODevice.ReadOnly)
        reader = QImageReader()
        reader.setDevice(buf)
        reader.setAutoTransform(True)
        orig = reader.size()
        if orig.isValid() and orig.width() > 0:
            reader.setScaledSize(
                orig.scaled(self.thumb_size, self.thumb_size, Qt.KeepAspectRatio)
            )
        img = reader.read()
        return img if not img.isNull() else QImage()

    def _qimage_from_numpy(self, arr) -> QImage:
        """Convert a (H, W, 3) uint8 numpy array to a QImage (safe copy)."""
        arr = _np.ascontiguousarray(arr, dtype=_np.uint8)
        h, w = arr.shape[:2]
        img = QImage(arr.data, w, h, int(arr.strides[0]), QImage.Format_RGB888)
        return img.copy()   # detach from numpy memory before array is freed

    def _fit(self, img: QImage) -> QImage:
        """Scale a QImage to fit within thumb_size × thumb_size."""
        if img.width() <= self.thumb_size and img.height() <= self.thumb_size:
            return img
        return img.scaled(
            self.thumb_size, self.thumb_size,
            Qt.KeepAspectRatio, Qt.SmoothTransformation,
        )

    def _make_placeholder(self) -> QImage:
        ts = self.thumb_size
        img = QImage(ts, ts, QImage.Format_RGB32)
        img.fill(QColor(22, 36, 60) if self.record.file_type == FileType.RAW
                 else QColor(42, 42, 42))
        painter = QPainter(img)
        painter.setPen(QColor(110, 110, 130))
        font = QFont()
        font.setPointSize(max(9, ts // 12))
        font.setBold(True)
        painter.setFont(font)
        painter.drawText(QRect(0, 0, ts, ts), Qt.AlignCenter, self.record.file_type.value)
        painter.end()
        return img


# ──────────────────────────────────────────────────────────────────────────────
# Generator (main-thread owner)
# ──────────────────────────────────────────────────────────────────────────────

class ThumbnailGenerator(QObject):
    """
    Priority-aware thumbnail generator with an LRU pixmap cache.
    """

    thumbnail_ready: Signal = Signal(object, object)  # (Path, QPixmap)

    def __init__(
        self,
        thumb_size: int = 160,
        cache_limit: int = _DEFAULT_CACHE_LIMIT,
        parent: Optional[QObject] = None,
    ) -> None:
        super().__init__(parent)
        self.thumb_size = thumb_size

        # I/O-bound work benefits from more threads than CPU cores
        n_workers = min(8, max(4, QThread.idealThreadCount()))
        self._pool = QThreadPool()
        self._pool.setMaxThreadCount(n_workers)

        self._cache = _LRUCache(cache_limit)
        self._queued: Set[Path] = set()
        self._disk_cache = DiskThumbnailCache(thumb_size)

        self._signals = _WorkerSignals()
        self._signals.done.connect(self._on_worker_done)

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    def request(self, record: PhotoRecord, priority: int = PRIORITY_IDLE) -> None:
        if record.path in self._cache or record.path in self._queued:
            return
        self._queued.add(record.path)
        self._pool.start(
            ThumbnailWorker(record, self.thumb_size, self._signals, self._disk_cache),
            priority,
        )

    def get_cached(self, path: Path) -> Optional[QPixmap]:
        return self._cache.get(path)

    def clear(self) -> None:
        self._pool.clear()
        self._queued.clear()
        self._cache.clear()

    def clear_queue(self) -> None:
        """
        Discard all queued-but-not-started workers and reset the queued set.

        Workers that are already running are unaffected — they will finish
        normally and their results land in the cache.  This is safe because
        _on_worker_done does a discard() which is a no-op for unknown paths.

        Call this before re-queuing a fresh priority batch (e.g. after a
        fast scroll) so stale mid-scroll items don't block the new visible ones.
        """
        self._pool.clear()      # removes queued-but-not-started runnables
        self._queued.clear()    # marks those paths as requestable again

    def shutdown(self) -> None:
        self._pool.waitForDone(3000)

    @property
    def cache_size(self) -> int:
        return len(self._cache)

    @property
    def rawpy_available(self) -> bool:
        return _RAWPY_AVAILABLE

    # ------------------------------------------------------------------ #
    # Slot                                                                 #
    # ------------------------------------------------------------------ #

    def _on_worker_done(self, path: Path, image: QImage) -> None:
        self._queued.discard(path)
        if not image.isNull():
            pixmap = QPixmap.fromImage(image)
            self._cache.put(path, pixmap)
            self.thumbnail_ready.emit(path, pixmap)
