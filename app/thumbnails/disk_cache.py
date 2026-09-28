"""
Persistent disk thumbnail cache.

Cache key: MD5 of "{absolute_path}:{mtime_timestamp}:{file_size}:{thumb_size}"
Cache dir:  ~/.cache/photo_pipeline/thumbs/{thumb_size}/

Thumbnails are stored as JPEG files named by their cache key hash.  JPEG is
used rather than PNG because the cache sits directly in the thumbnail hot
path and PNG is expensive at both ends: for a 300×200 thumbnail, PNG costs
4.66 ms to write and 0.72 ms to read at 80 KB, against 0.16 ms / 0.20 ms at
17 KB for JPEG.  The PNG encode alone cost about as much as decoding a RAW
preview, so it roughly doubled the price of every cache miss — and a
10 000-file shoot took 800 MB of cache instead of 170 MB.

`.png` entries written by earlier versions are still read, so upgrading does
not throw away an existing cache; they are simply never written again.

Thread-safe for concurrent reads; concurrent writes of the same key are
idempotent (both writers produce equivalent content) so no locking is needed.
"""
from __future__ import annotations

import hashlib
import os
import uuid
from pathlib import Path
from typing import Optional

from PySide6.QtGui import QImage

from app.models.photo_record import PhotoRecord

# High enough that JPEG artefacts are invisible at thumbnail scale.
_JPEG_QUALITY = 92


class DiskThumbnailCache:
    """
    Persistent store for decoded thumbnail QImages.

    Designed to be used by ThumbnailWorker (runs in thread-pool workers).
    All operations are file I/O only — no Qt object sharing across threads.
    """

    def __init__(
        self,
        thumb_size: int,
        cache_dir: Optional[Path] = None,
    ) -> None:
        if cache_dir is None:
            cache_dir = Path.home() / ".cache" / "photo_pipeline" / "thumbs"
        self._dir = cache_dir / str(thumb_size)
        self._dir.mkdir(parents=True, exist_ok=True)
        self._thumb_size = thumb_size

    # ------------------------------------------------------------------ #
    # Public                                                               #
    # ------------------------------------------------------------------ #

    def get(self, record: PhotoRecord) -> Optional[QImage]:
        """Return the cached QImage for *record*, or None on cache miss."""
        digest = self._digest(record)
        for suffix in (".jpg", ".png"):   # ".png" = written by an older version
            p = self._dir / f"{digest}{suffix}"
            if p.exists():
                img = QImage(str(p))
                if not img.isNull():
                    return img
        return None

    def put(self, record: PhotoRecord, image: QImage) -> None:
        """Write *image* to the disk cache for *record* (no-op if already cached)."""
        digest = self._digest(record)
        p = self._dir / f"{digest}.jpg"
        if p.exists() or (self._dir / f"{digest}.png").exists():
            return
        # Unique temp name — two workers may race on the same key, and a
        # shared ".tmp" would let one rename the other's half-written file
        # into place.
        tmp = self._dir / f"{digest}.{uuid.uuid4().hex}.tmp"
        try:
            image.save(str(tmp), "JPG", _JPEG_QUALITY)
            os.replace(str(tmp), str(p))
        except Exception:  # noqa: BLE001
            tmp.unlink(missing_ok=True)

    def clear(self) -> None:
        """Delete all cached thumbnails for this thumb_size."""
        for pattern in ("*.jpg", "*.png", "*.tmp"):
            for f in self._dir.glob(pattern):
                try:
                    f.unlink(missing_ok=True)
                except OSError:
                    pass

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _digest(self, record: PhotoRecord) -> str:
        key = (
            f"{record.path.as_posix()}"
            f":{record.modified_time.timestamp()}"
            f":{record.file_size}"
            f":{self._thumb_size}"
        )
        return hashlib.md5(key.encode()).hexdigest()
