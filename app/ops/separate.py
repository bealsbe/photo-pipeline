"""
RAW/JPG separation plan and execution.

Each file is moved to a `RAW/` or `JPG/` subdirectory inside its own
parent directory, so files in nested subdirectories are separated in place
rather than all being flattened into one folder.

Files already in the correct subfolder (e.g. a file whose parent is named
"RAW") are left untouched.

Conflict handling
-----------------
A conflict occurs when the destination path already exists AND differs from
the source path.  Two batch strategies are supported:

  "skip"    — leave conflicting files where they are (skip the move)
  "rename"  — append _1, _2, … to the stem until a free name is found

The caller picks a strategy via SeparationPlan.set_conflict_strategy().
"""
from __future__ import annotations

import os
import shutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Tuple

from app.models.photo_record import FileType, PhotoRecord


@dataclass
class MoveOp:
    """Describes one file move within a separation plan."""
    record: PhotoRecord
    dest: Path            # desired destination (may conflict)
    final_dest: Path      # actual destination after strategy applied
    conflict: bool        # True if dest already exists and != src
    skipped: bool = False # set True when strategy == "skip" and conflict


def _free_path(dest: Path) -> Path:
    """Return the first non-existing path by appending _1, _2, … to the stem."""
    if not dest.exists():
        return dest
    stem, suffix, parent = dest.stem, dest.suffix, dest.parent
    n = 1
    while True:
        candidate = parent / f"{stem}_{n}{suffix}"
        if not candidate.exists():
            return candidate
        n += 1


class SeparationPlan:
    """
    Builds and executes a RAW/JPG separation plan for a list of records.

    Usage
    -----
    plan = SeparationPlan(records)
    if plan.has_conflicts():
        plan.set_conflict_strategy("rename")   # or "skip"
    succeeded, failed = plan.execute()
    """

    def __init__(self, records: List[PhotoRecord]) -> None:
        self._strategy: str = "rename"   # default
        self._ops: List[MoveOp] = self._build(records)

    # ------------------------------------------------------------------ #
    # Public                                                               #
    # ------------------------------------------------------------------ #

    @property
    def ops(self) -> List[MoveOp]:
        return self._ops

    def has_conflicts(self) -> bool:
        return any(op.conflict for op in self._ops)

    def conflict_count(self) -> int:
        return sum(1 for op in self._ops if op.conflict)

    def movable_count(self) -> int:
        """Number of ops that will actually perform a move (not skipped, not already in place)."""
        return sum(1 for op in self._ops if not op.skipped)

    def already_in_place_count(self) -> int:
        return sum(1 for op in self._ops if op.record.path == op.dest)

    def set_conflict_strategy(self, strategy: str) -> None:
        """
        strategy: "skip" | "rename"
        Re-evaluates final_dest and skipped flags for all conflicting ops.
        """
        assert strategy in ("skip", "rename")
        self._strategy = strategy
        for op in self._ops:
            if not op.conflict:
                continue
            if strategy == "skip":
                op.skipped = True
                op.final_dest = op.record.path   # stays in place
            else:
                op.skipped = False
                op.final_dest = _free_path(op.dest)

    def execute(self) -> Tuple[
        List[Tuple[PhotoRecord, Path]],   # succeeded: (record, new_path)
        List[Tuple[PhotoRecord, str]],    # failed:    (record, error_msg)
    ]:
        """
        Perform the moves.  Each non-skipped op has its parent dir created
        if necessary and the file moved via shutil.move.

        Returns
        -------
        succeeded : [(record, new_path), ...]
            Records that were moved.  new_path is the actual destination.
        failed : [(record, error_msg), ...]
            Records that could not be moved.
        """
        succeeded: List[Tuple[PhotoRecord, Path]] = []
        failed: List[Tuple[PhotoRecord, str]] = []

        for op in self._ops:
            src = op.record.path

            # Already in the right place → count as success with unchanged path
            if src == op.dest:
                succeeded.append((op.record, src))
                continue

            # Skipped due to conflict strategy
            if op.skipped:
                continue

            try:
                op.final_dest.parent.mkdir(parents=True, exist_ok=True)
                # Capture timestamps before the move so we can restore them
                # if the OS copy-path resets mtime (e.g. cross-device move).
                st = src.stat()
                shutil.move(str(src), str(op.final_dest))
                # Restore atime + mtime so the file date is unchanged.
                try:
                    os.utime(str(op.final_dest), (st.st_atime, st.st_mtime))
                except OSError:
                    pass
                succeeded.append((op.record, op.final_dest))
            except Exception as exc:  # noqa: BLE001
                failed.append((op.record, str(exc)))

        return succeeded, failed

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    @staticmethod
    def _build(records: List[PhotoRecord]) -> List[MoveOp]:
        ops: List[MoveOp] = []
        for record in records:
            subdir = "RAW" if record.file_type == FileType.RAW else "JPG"

            # File is already inside the correct named subfolder — leave it.
            # This covers post-import files at .../RAW/foo.cr3 or .../JPG/foo.jpg.
            if record.path.parent.name.upper() == subdir.upper():
                ops.append(MoveOp(
                    record=record,
                    dest=record.path,
                    final_dest=record.path,
                    conflict=False,
                ))
                continue

            dest = record.path.parent / subdir / record.filename
            conflict = dest.exists()
            final_dest = _free_path(dest) if conflict else dest
            ops.append(MoveOp(
                record=record,
                dest=dest,
                final_dest=final_dest,
                conflict=conflict,
            ))
        return ops
