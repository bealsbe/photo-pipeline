"""
In-memory collection of PhotoRecords for the current session.

Insertion order is preserved.  build_pairs() should be called once after
a scan completes to establish RAW/JPG pair relationships.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterator, List, Optional

from .photo_record import FileType, PhotoRecord


@dataclass
class _Stats:
    """Mutable counters kept in sync with collection mutations."""
    total:    int = 0
    raw:      int = 0
    jpg:      int = 0
    paired:   int = 0   # individual records that are paired (both RAW and JPG sides)
    unpaired: int = 0
    pruned:   int = 0

    def as_dict(self) -> dict:
        return {
            "total":    self.total,
            "raw":      self.raw,
            "jpg":      self.jpg,
            "paired":   self.paired,
            "unpaired": self.unpaired,
            "pruned":   self.pruned,
        }


class PhotoCollection:
    """Ordered, path-keyed store of PhotoRecords."""

    def __init__(self) -> None:
        # Use a dict to keep insertion order AND O(1) path lookup
        self._records: Dict[Path, PhotoRecord] = {}
        self._s = _Stats()
        # Secondary index: (canonical_parent, lowercase_stem) → PhotoRecord
        # Allows O(1) pair lookup in find_pair().
        self._pair_index: Dict[tuple, List[PhotoRecord]] = {}

    # ------------------------------------------------------------------ #
    # Mutation                                                             #
    # ------------------------------------------------------------------ #

    def add(self, record: PhotoRecord) -> None:
        self._records[record.path] = record
        self._s.total += 1
        if record.file_type == FileType.RAW:
            self._s.raw += 1
        else:
            self._s.jpg += 1
        if record.is_paired:
            self._s.paired   += 1
        else:
            self._s.unpaired += 1
        if record.is_pruned:
            self._s.pruned += 1
        # pair index
        key = (self._canonical_parent(record.path), record.stem.lower())
        self._pair_index.setdefault(key, []).append(record)

    def clear(self) -> None:
        self._records.clear()
        self._pair_index.clear()
        self._s = _Stats()

    def remove(self, record: PhotoRecord) -> None:
        if record.path not in self._records:
            return
        self._records.pop(record.path)
        self._s.total -= 1
        if record.file_type == FileType.RAW:
            self._s.raw -= 1
        else:
            self._s.jpg -= 1
        if record.is_paired:
            self._s.paired   -= 1
        else:
            self._s.unpaired -= 1
        if record.is_pruned:
            self._s.pruned -= 1
        key = (self._canonical_parent(record.path), record.stem.lower())
        bucket = self._pair_index.get(key)
        if bucket:
            try:
                bucket.remove(record)
            except ValueError:
                pass
            if not bucket:
                del self._pair_index[key]

    def notify_pruned(self, record: PhotoRecord, was_pruned: bool) -> None:
        """Call after flipping record.is_pruned to keep the counter accurate."""
        if record.is_pruned and not was_pruned:
            self._s.pruned += 1
        elif not record.is_pruned and was_pruned:
            self._s.pruned -= 1

    def update_path(self, old_path: Path, new_path: Path) -> None:
        """Re-key a record after its file has been moved."""
        record = self._records.pop(old_path, None)
        if record is None:
            return
        # Remove from old pair-index bucket
        old_key = (self._canonical_parent(old_path), record.stem.lower())
        bucket = self._pair_index.get(old_key)
        if bucket:
            try:
                bucket.remove(record)
            except ValueError:
                pass
            if not bucket:
                del self._pair_index[old_key]
        record.path = new_path
        self._records[new_path] = record
        # Insert into new pair-index bucket
        new_key = (self._canonical_parent(new_path), record.stem.lower())
        self._pair_index.setdefault(new_key, []).append(record)

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
        # _pair_index is already populated by add(); reuse it for grouping
        paired_count = 0
        for records in self._pair_index.values():
            types = {r.file_type for r in records}
            if FileType.RAW in types and FileType.JPG in types:
                stem = records[0].stem.lower()
                for r in records:
                    r.pair_stem = stem
                paired_count += len(records)
            else:
                for r in records:
                    r.pair_stem = None

        self._s.paired   = paired_count
        self._s.unpaired = self._s.total - paired_count

    def find_pair(self, record: PhotoRecord) -> Optional[PhotoRecord]:
        """Return the RAW/JPG partner of *record*, or None if unpaired. O(1)."""
        if not record.is_paired:
            return None
        key = (self._canonical_parent(record.path), record.stem.lower())
        for r in self._pair_index.get(key, []):
            if r is not record:
                return r
        return None

    # ------------------------------------------------------------------ #
    # Statistics                                                           #
    # ------------------------------------------------------------------ #

    @property
    def stats(self) -> dict:
        """O(1) — counters are maintained incrementally."""
        return self._s.as_dict()
