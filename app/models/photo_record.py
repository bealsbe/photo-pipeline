"""
Data model for a single photo file on disk.

Each physical file is its own record regardless of pairing.
Pair relationships are tracked via pair_stem (the lowercase filename stem
shared between a RAW and JPG that originate from the same shot).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Optional


class FileType(Enum):
    JPG = "JPG"
    RAW = "RAW"


# All recognised RAW extensions (lowercase, with leading dot)
RAW_EXTENSIONS: frozenset[str] = frozenset({
    ".cr2", ".cr3",   # Canon
    ".nef", ".nrw",   # Nikon
    ".arw", ".srw",   # Sony/Samsung
    ".orf",           # Olympus
    ".rw2",           # Panasonic
    ".raf",           # Fujifilm
    ".dng",           # Adobe DNG (generic)
    ".pef",           # Pentax
    ".x3f",           # Sigma
    ".3fr",           # Hasselblad
    ".mef",           # Mamiya
    ".rwl",           # Leica
    ".erf",           # Epson
})

JPG_EXTENSIONS: frozenset[str] = frozenset({".jpg", ".jpeg"})

SUPPORTED_EXTENSIONS: frozenset[str] = RAW_EXTENSIONS | JPG_EXTENSIONS


@dataclass
class PhotoRecord:
    """
    Represents one physical image file.  Thumbnails and viewer pixmaps
    are attached lazily in later phases.
    """
    path: Path
    file_type: FileType
    file_size: int          # bytes
    modified_time: datetime
    capture_time: Optional[datetime] = None  # EXIF DateTimeOriginal; None = unknown

    # Mutable state (changes during a session)
    is_pruned: bool = False
    pair_stem: Optional[str] = None  # lowercase stem shared with a paired file

    # ------------------------------------------------------------------ #
    # Convenience properties                                               #
    # ------------------------------------------------------------------ #

    @property
    def shot_time(self) -> datetime:
        """Best available date: EXIF capture time, falling back to mtime."""
        return self.capture_time if self.capture_time is not None else self.modified_time

    @property
    def filename(self) -> str:
        return self.path.name

    @property
    def stem(self) -> str:
        return self.path.stem

    @property
    def extension(self) -> str:
        return self.path.suffix.lower()

    @property
    def is_paired(self) -> bool:
        return self.pair_stem is not None

    def __hash__(self) -> int:
        return hash(self.path)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, PhotoRecord):
            return self.path == other.path
        return NotImplemented
