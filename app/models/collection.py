"""
In-memory collection of PhotoRecords for the current session.

Insertion order is preserved.  build_pairs() should be called once after
a scan completes to establish RAW/JPG pair relationships.
"""
from __future__ import annotations

from pathlib import Path
from typing import Dict, Iterator, List, Optional

from .photo_record import FileType, PhotoRecord


class PhotoCollection:
    """Ordered, path-keyed store of PhotoRecords."""

    def __init__(self) -> None:
        # Use a dict to keep insertion order AND O(1) path lookup
        self._records: Dict[Path, PhotoRecord] = {}

    # ------------------------------------------------------------------ #
    # Mutation                                                             #
    # ------------------------------------------------------------------ #

    def add(self, record: PhotoRecord) -> None:
        self._records[record.path] = record

    def clear(self) -> None:
        self._records.clear()

    def remove(self, record: PhotoRecord) -> None:
        self._records.pop(record.path, None)

    def update_path(self, old_path: Path, new_path: Path) -> None:
        """Re-key a record after its file has been moved."""
        record = self._records.pop(old_path, None)
        if record is not None:
            record.path = new_path
            self._records[new_path] = record

    # ------------------------------------------------------------------ #
    # Queries                                                              #
    # ------------------------------------------------------------------ #

    def get(self, path: Path) -> Optional[PhotoRecord]:
        return self._records.get(path)

    def all(self) -> List[PhotoRecord]:
        return list(self._records.values())

    def by_type(self, file_type: FileType) -> List[PhotoRecord]:
        return [r for r in self._records.values() if r.file_type == file_type]

    def paired(self) -> List[PhotoRecord]:
        return [r for r in self._records.values() if r.is_paired]

    def unpaired(self) -> List[PhotoRecord]:
        return [r for r in self._records.values() if not r.is_paired]

    def pruned(self) -> List[PhotoRecord]:
        return [r for r in self._records.values() if r.is_pruned]

    def unpruned(self) -> List[PhotoRecord]:
        return [r for r in self._records.values() if not r.is_pruned]

    def __len__(self) -> int:
        return len(self._records)

    def __iter__(self) -> Iterator[PhotoRecord]:
        return iter(self._records.values())

    # ------------------------------------------------------------------ #
    # Pair resolution                                                      #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _canonical_parent(path: Path) -> Path:
        """
        Return the effective parent directory for pair matching.

        After import separation, RAW files live in a `RAW/` subfolder and
        JPG files in `JPG/`.  To still recognise them as pairs we strip that
        one extra level so both map to the same grandparent directory.
        """
        if path.parent.name.upper() in ("RAW", "JPG"):
            return path.parent.parent
        return path.parent

    def build_pairs(self) -> None:
        """
        Match RAW and JPG records by (canonical_parent, lowercase_stem).

        Works both before separation (files in the same dir) and after
        (RAW in .../RAW/, JPG in .../JPG/).
        """
        groups: Dict[tuple, List[PhotoRecord]] = {}
        for record in self._records.values():
            key = (self._canonical_parent(record.path), record.stem.lower())
            groups.setdefault(key, []).append(record)

        for (_, stem), records in groups.items():
            types = {r.file_type for r in records}
            if FileType.RAW in types and FileType.JPG in types:
                for r in records:
                    r.pair_stem = stem
            else:
                # Clear any stale pair info from a previous scan
                for r in records:
                    r.pair_stem = None

    def find_pair(self, record: PhotoRecord) -> Optional[PhotoRecord]:
        """Return the RAW/JPG partner of *record*, or None if unpaired."""
        if not record.is_paired:
            return None
        canonical = self._canonical_parent(record.path)
        stem = record.stem.lower()
        for r in self._records.values():
            if (r is not record
                    and r.is_paired
                    and r.stem.lower() == stem
                    and self._canonical_parent(r.path) == canonical):
                return r
        return None

    # ------------------------------------------------------------------ #
    # Statistics                                                           #
    # ------------------------------------------------------------------ #

    @property
    def stats(self) -> dict:
        records = self.all()
        total = len(records)
        raw = sum(1 for r in records if r.file_type == FileType.RAW)
        return {
            "total": total,
            "raw": raw,
            "jpg": total - raw,
            "paired": sum(1 for r in records if r.is_paired),
            "unpaired": sum(1 for r in records if not r.is_paired),
            "pruned": sum(1 for r in records if r.is_pruned),
        }
