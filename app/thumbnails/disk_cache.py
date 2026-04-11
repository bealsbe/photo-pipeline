"""
Persistent disk thumbnail cache.

Cache key: MD5 of "{absolute_path}:{mtime_timestamp}:{file_size}:{thumb_size}"
Cache dir:  ~/.cache/photo_pipeline/thumbs/{thumb_size}/

Thumbnails are stored as PNG files named by their cache key hash.
Thread-safe for concurrent reads; concurrent writes of the same key are
idempotent (both writers produce identical content) so no locking is needed.
"""
from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Optional

from PySide6.QtGui import QImage

from app.models.photo_record import PhotoRecord


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
        p = self._cache_path(record)
        if not p.exists():
            return None
        img = QImage(str(p))
        return img if not img.isNull() else None

    def put(self, record: PhotoRecord, image: QImage) -> None:
        """Write *image* to the disk cache for *record* (no-op if already cached)."""
        p = self._cache_path(record)
        if not p.exists():
            try:
                image.save(str(p), "PNG")
            except Exception:  # noqa: BLE001
                pass  # disk full, permissions, etc. — degrade silently

    def clear(self) -> None:
        """Delete all cached thumbnails for this thumb_size."""
        for f in self._dir.glob("*.png"):
            try:
                f.unlink(missing_ok=True)
            except OSError:
                pass

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _cache_path(self, record: PhotoRecord) -> Path:
        key = (
            f"{record.path.as_posix()}"
            f":{record.modified_time.timestamp()}"
            f":{record.file_size}"
            f":{self._thumb_size}"
        )
        digest = hashlib.md5(key.encode()).hexdigest()
        return self._dir / f"{digest}.png"
