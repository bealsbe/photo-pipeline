"""
Full-resolution image loader for the single-image viewer.

FullImageWorker (QRunnable)
    Loads one image at full resolution in a thread-pool worker.
    RAW files: rawpy full postprocess (no half_size — viewer quality).
    JPG/other: QImageReader without downscaling.
    Falls back to QImageReader if rawpy fails.

ImageLoader (QObject, main thread)
    Byte-budgeted LRU cache of QPixmaps — full-res images are large.
    request(record)  — load now, emit image_ready when done.
    preload(record)  — low-priority background fetch.
    clear_queue()    — flush unstarted workers (call before navigating).
    get_cached(path) — synchronous cache hit.

Cache sizing
------------
The budget is in bytes, not entries, because "an image" spans two orders of
magnitude.  A 12 MP phone JPEG is 48 MB as a QPixmap; a 42 MP camera JPEG at
7952×5304 is 169 MB.  A flat nine-entry cache therefore meant anything from
430 MB to 1.52 GB, and on a modern body it was the latter — measured peak RSS
of 2.0 GB just holding the viewer's LRU.

The preload radius follows from the budget rather than being fixed, so the
loader never decodes a neighbour it is about to evict: at 12 MP the budget
holds sixteen frames and the full ±2 is preloaded, while at 42 MP it holds
four and the radius drops to ±1.  _CACHE_FLOOR keeps the current image and its
immediate neighbours resident no matter how large a single frame is.
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
    QRunnable,
    QThreadPool,
    Signal,
)
from PySide6.QtGui import QImage, QImageReader, QPixmap

from app.models.photo_record import FileType, PhotoRecord

try:
    import rawpy as _rawpy
    import numpy as _np
    _RAWPY_AVAILABLE = True
except ImportError:
    _RAWPY_AVAILABLE = False

_CACHE_BYTES = 768 * 1024 * 1024   # ~4 frames from a 42 MP body, ~16 from a phone
_CACHE_FLOOR = 3                   # current image ± 1, resident at any frame size
_PRELOAD_RADIUS = 2                # requested radius; trimmed to fit the budget
_PRIO_NOW    = 10
_PRIO_AHEAD  = 1


def _pixmap_bytes(pixmap: QPixmap) -> int:
    """Approximate resident size of *pixmap* in bytes."""
    return max(1, pixmap.width() * pixmap.height() * max(1, pixmap.depth()) // 8)


# ──────────────────────────────────────────────────────────────────────────────
# Minimal LRU (local copy — avoids importing from generator)
# ──────────────────────────────────────────────────────────────────────────────

class _LRU:
    """LRU bounded by total bytes, with a floor on the number of entries."""

    def __init__(self, max_bytes: int, floor: int = 1) -> None:
        self._d: OrderedDict[Path, QPixmap] = OrderedDict()
        self._sizes: dict[Path, int] = {}
        self._max_bytes = max_bytes
        self._floor = max(1, floor)
        self._bytes = 0

    def get(self, k: Path) -> Optional[QPixmap]:
        if k not in self._d:
            return None
        self._d.move_to_end(k)
        return self._d[k]

    def put(self, k: Path, v: QPixmap) -> None:
        if k in self._d:
            self._bytes -= self._sizes.pop(k, 0)
            self._d.move_to_end(k)
        size = _pixmap_bytes(v)
        self._d[k] = v
        self._sizes[k] = size
        self._bytes += size
        # Never evict below the floor: dropping a neighbour the viewer is
        # about to slide to would make navigation re-decode it.
        while self._bytes > self._max_bytes and len(self._d) > self._floor:
            old, _ = self._d.popitem(last=False)
            self._bytes -= self._sizes.pop(old, 0)

    def __contains__(self, k: Path) -> bool:
        return k in self._d

    def clear(self) -> None:
        self._d.clear()
        self._sizes.clear()
        self._bytes = 0

    @property
    def nbytes(self) -> int:
        return self._bytes

    @property
    def largest_entry(self) -> int:
        """Bytes of the biggest pixmap currently held (0 when empty)."""
        return max(self._sizes.values(), default=0)


# ──────────────────────────────────────────────────────────────────────────────
# Worker
# ──────────────────────────────────────────────────────────────────────────────

class _Signals(QObject):
    done: Signal = Signal(object, object)   # (Path, QImage)
    failed: Signal = Signal(object, str)    # (Path, error_message)


class FullImageWorker(QRunnable):
    """Loads a single full-resolution image and emits a QImage."""

    def __init__(self, record: PhotoRecord, signals: _Signals) -> None:
        super().__init__()
        self.record = record
        self.signals = signals
        self.setAutoDelete(True)

    def run(self) -> None:
        try:
            img = self._load()
            if img.isNull():
                self.signals.failed.emit(self.record.path, "Could not decode image")
            else:
                self.signals.done.emit(self.record.path, img)
        except Exception as exc:  # noqa: BLE001
            self.signals.failed.emit(self.record.path, str(exc))

    # ------------------------------------------------------------------ #

    def _load(self) -> QImage:
        if self.record.file_type == FileType.RAW and _RAWPY_AVAILABLE:
            img = self._load_raw()
            if not img.isNull():
                return img
        return self._load_qt(str(self.record.path))

    def _load_raw(self) -> QImage:
        try:
            with _rawpy.imread(str(self.record.path)) as raw:
                # Try embedded JPEG first (fast, camera-rendered)
                try:
                    thumb = raw.extract_thumb()
                    if thumb.format == _rawpy.ThumbFormat.JPEG:
                        img = self._from_jpeg_bytes(bytes(thumb.data))
                        if not img.isNull():
                            return img
                except Exception:
                    pass

                # Full demosaic — no half_size for viewer quality
                rgb = raw.postprocess(
                    use_camera_wb=True,
                    no_auto_bright=False,
                    output_bps=8,
                )
                arr = _np.ascontiguousarray(rgb, dtype=_np.uint8)
                h, w = arr.shape[:2]
                img = QImage(arr.data, w, h, int(arr.strides[0]), QImage.Format_RGB888)
                return img.copy()
        except Exception:
            return QImage()

    def _load_qt(self, path: str) -> QImage:
        reader = QImageReader(path)
        reader.setAutoTransform(True)
        img = reader.read()
        return img if not img.isNull() else QImage()

    def _from_jpeg_bytes(self, data: bytes) -> QImage:
        ba = QByteArray(data)
        buf = QBuffer(ba)
        buf.open(QIODevice.ReadOnly)
        reader = QImageReader()
        reader.setDevice(buf)
        reader.setAutoTransform(True)
        img = reader.read()
        return img if not img.isNull() else QImage()


# ──────────────────────────────────────────────────────────────────────────────
# Loader (main-thread owner)
# ──────────────────────────────────────────────────────────────────────────────

class ImageLoader(QObject):
    """
    Manages full-resolution image loading for the viewer.

    Signals
    -------
    image_ready(Path, QPixmap)   — fired when an image is decoded and cached
    load_failed(Path, str)       — fired when decoding fails
    """

    image_ready: Signal = Signal(object, object)   # (Path, QPixmap)
    load_failed: Signal = Signal(object, str)      # (Path, error_msg)

    def __init__(self, parent: Optional[QObject] = None) -> None:
        super().__init__(parent)
        self._pool = QThreadPool()
        self._pool.setMaxThreadCount(3)   # 1 current + 2 preload
        self._cache = _LRU(_CACHE_BYTES, floor=_CACHE_FLOOR)
        self._loading: Set[Path] = set()
        self._signals = _Signals()
        self._signals.done.connect(self._on_done)
        self._signals.failed.connect(self._on_failed)

    # ------------------------------------------------------------------ #
    # Public API                                                           #
    # ------------------------------------------------------------------ #

    def request(self, record: PhotoRecord) -> None:
        """Load record at high priority.  Emits image_ready when done."""
        cached = self._cache.get(record.path)
        if cached is not None:
            self.image_ready.emit(record.path, cached)
            return
        if record.path not in self._loading:
            self._loading.add(record.path)
            self._pool.start(FullImageWorker(record, self._signals), _PRIO_NOW)

    def preload(self, record: PhotoRecord) -> None:
        """Queue record at low priority (preload adjacent images)."""
        if record.path in self._cache or record.path in self._loading:
            return
        self._loading.add(record.path)
        self._pool.start(FullImageWorker(record, self._signals), _PRIO_AHEAD)

    def preload_range(self, records: list, center: int, radius: int = _PRELOAD_RADIUS) -> None:
        """
        Preload up to *radius* images on each side of *center* index.

        The radius is trimmed to what the cache budget can actually hold, so a
        42 MP shoot preloads ±1 rather than decoding a ±2 neighbour that the
        next put() would immediately evict.
        """
        radius = min(radius, self._affordable_radius())
        lo = max(0, center - radius)
        hi = min(len(records) - 1, center + radius)
        for i in range(lo, hi + 1):
            if i != center:
                self.preload(records[i])

    def _affordable_radius(self) -> int:
        """
        How many neighbours fit in the budget alongside the current image.

        Derived from the largest frame seen so far, which is what the next
        decode is likely to cost.  Before anything has loaded there is nothing
        to measure, so the caller's requested radius stands.
        """
        largest = self._cache.largest_entry
        if largest <= 0:
            return _PRELOAD_RADIUS
        capacity = max(_CACHE_FLOOR, _CACHE_BYTES // largest)
        return max(1, (capacity - 1) // 2)

    def get_cached(self, path: Path) -> Optional[QPixmap]:
        return self._cache.get(path)

    def clear_queue(self) -> None:
        """Remove queued-but-not-started workers; reset loading set."""
        self._pool.clear()
        self._loading.clear()

    def shutdown(self) -> None:
        # Drop queued-but-not-started workers so we don't wait for them.
        # Any worker already running holds no Qt UI references and will
        # finish safely in the background after this returns.
        self._pool.clear()
        self._pool.waitForDone(0)

    # ------------------------------------------------------------------ #
    # Slots                                                                #
    # ------------------------------------------------------------------ #

    def _on_done(self, path: Path, image: QImage) -> None:
        self._loading.discard(path)
        pixmap = QPixmap.fromImage(image)
        self._cache.put(path, pixmap)
        self.image_ready.emit(path, pixmap)

    def _on_failed(self, path: Path, msg: str) -> None:
        self._loading.discard(path)
        self.load_failed.emit(path, msg)
