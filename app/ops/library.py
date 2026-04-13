"""
app/ops/library.py — library folder path computation, planning, and execution.

Public API
----------
best_date(record, pair=None) -> date
    Return the most reliable shot date for a record.  Prefers JPG EXIF
    (cameras embed it most consistently), then RAW EXIF, then RAW mtime,
    then the record's own mtime.  Rejects dates outside the plausible range
    1990 – (now + 1 year).

month_folder(d) -> str
    "Jan-2025", "Feb-2025", …

library_dest(root, record, d) -> Path
    root / "2025" / "Jan-2025" / "RAW" / "IMG_001.cr3"

Resolution
    Enum: SKIP | OVERWRITE | RENAME

PlannedOp
    One file move/copy in a LibraryPlan.  Holds src, dest, conflict flag,
    chosen resolution, and the final destination after renaming.

LibraryPlan
    Given a list of PhotoRecords + a library root, builds the full set of
    PlannedOps, detects conflicts, and exposes helpers for the UI.
"""
from __future__ import annotations

import calendar
import os
import shutil
from dataclasses import dataclass, field
from datetime import date as _date, datetime
from enum import Enum
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

from app.models.photo_record import FileType, PhotoRecord

# ── date validation ───────────────────────────────────────────────────────────

_MIN_YEAR = 1990


def _valid(dt: datetime) -> bool:
    """True if *dt* falls in the plausible photo-date range."""
    return _MIN_YEAR <= dt.year <= datetime.now().year + 1


# ── date selection ────────────────────────────────────────────────────────────

def best_date(
    record: PhotoRecord,
    pair:   Optional[PhotoRecord] = None,
) -> _date:
    """
    Return the most reliable shot date for *record*.

    Pair-aware priority order
    -------------------------
    1. JPG capture_time  — cameras embed DateTimeOriginal most reliably in JPEG
    2. RAW capture_time  — present on most modern bodies
    3. RAW modified_time — RAW files are never re-saved; mtime is stable
    4. record.modified_time — last resort

    If either file's capture_time fails the plausibility check (pre-1990,
    factory default 0001-01-01, far future) it is treated as absent.
    """
    jpg = record if record.file_type == FileType.JPG else pair
    raw = record if record.file_type == FileType.RAW else pair

    if jpg and jpg.capture_time and _valid(jpg.capture_time):
        return jpg.capture_time.date()
    if raw and raw.capture_time and _valid(raw.capture_time):
        return raw.capture_time.date()
    if raw and _valid(raw.modified_time):
        return raw.modified_time.date()
    return record.modified_time.date()


# ── path building ─────────────────────────────────────────────────────────────

def month_folder(d: _date) -> str:
    """Return the month-folder name for *d*: 'Jan-2025', 'Feb-2025', …"""
    return f"{calendar.month_abbr[d.month]}-{d.year}"


def library_dest(root: Path, record: PhotoRecord, d: _date) -> Path:
    """
    Compute the destination path for *record* inside the library.

        root / "2025" / "Jan-2025" / "RAW" / "IMG_001.cr3"

    The type subfolder is "RAW" or "JPG" from FileType.value.
    """
    return (
        root
        / str(d.year)
        / month_folder(d)
        / record.file_type.value   # "RAW" or "JPG"
        / record.path.name
    )


# ── conflict resolution ───────────────────────────────────────────────────────

class Resolution(Enum):
    SKIP      = "skip"
    OVERWRITE = "overwrite"
    RENAME    = "rename"


def _next_free(dest: Path) -> Path:
    """
    Return *dest* with a numeric suffix (_1, _2, …) that does not exist.

        IMG_0001.cr3 → IMG_0001_1.cr3 → IMG_0001_2.cr3 …
    """
    stem, suffix = dest.stem, dest.suffix
    n = 1
    while True:
        candidate = dest.with_name(f"{stem}_{n}{suffix}")
        if not candidate.exists():
            return candidate
        n += 1


# ── planned operation ─────────────────────────────────────────────────────────

@dataclass
class PlannedOp:
    """One file operation within a LibraryPlan."""

    record:     PhotoRecord
    src:        Path
    dest:       Path
    date:       _date            # shot date used to build dest — kept for grouping
    conflict:   bool  = False    # True when dest already exists on disk
    resolution: Resolution = Resolution.SKIP

    # Computed by set_resolution(); equals dest unless resolution is RENAME.
    final_dest: Path = field(init=False)

    def __post_init__(self) -> None:
        self.final_dest = self.dest

    def set_resolution(self, r: Resolution) -> None:
        self.resolution = r
        self.final_dest = _next_free(self.dest) if r == Resolution.RENAME else self.dest


# ── preview grouping ──────────────────────────────────────────────────────────

@dataclass
class OpGroup:
    """
    A node in the preview tree: one year/month/type bucket.

    Display: "2025 / Jan-2025 / RAW   24 files   ⚠ 2 conflicts"
    """
    year:        int
    month:       int         # 1-12, used for chronological sorting
    month_key:   str         # "Jan-2025"
    type_label:  str         # "RAW" or "JPG"
    ops:         List[PlannedOp] = field(default_factory=list)

    @property
    def file_count(self) -> int:
        return len(self.ops)

    @property
    def conflict_count(self) -> int:
        return sum(1 for op in self.ops if op.conflict)


# ── library plan ──────────────────────────────────────────────────────────────

class LibraryPlan:
    """
    Compute and manage the full set of file operations for an export or
    auto-sort into the standard library structure.

    Parameters
    ----------
    records
        Files to plan.  Pruned/unpruned filtering is the caller's
        responsibility — pass only the records you want to move/copy.
    library_root
        Root of the destination library tree.
    pair_lookup
        ``collection.find_pair`` or equivalent.  Used to cross-reference
        the partner file when choosing the best date.  Pass None to skip
        pair-aware date resolution.

    Usage
    -----
    plan = LibraryPlan(records, library_root, collection.find_pair)

    # Resolve all conflicts at once:
    plan.apply_bulk_resolution(Resolution.RENAME)

    # Or per-file:
    plan.conflicts[0].set_resolution(Resolution.OVERWRITE)

    # Hand to the executor:
    for op in plan.ops:
        execute(op.src, op.final_dest, mode)
    """

    def __init__(
        self,
        records:      List[PhotoRecord],
        library_root: Path,
        pair_lookup:  Optional[Callable[[PhotoRecord], Optional[PhotoRecord]]] = None,
    ) -> None:
        self._root = library_root
        self.ops: List[PlannedOp] = []

        for record in records:
            pair = pair_lookup(record) if pair_lookup else None
            d    = best_date(record, pair)
            dest = library_dest(library_root, record, d)
            conflict = dest.exists()
            op = PlannedOp(
                record   = record,
                src      = record.path,
                dest     = dest,
                date     = d,
                conflict = conflict,
            )
            # Non-conflicting ops are ready to go immediately.
            # Conflicting ops stay as SKIP until the user resolves them.
            if not conflict:
                op.set_resolution(Resolution.OVERWRITE)
            self.ops.append(op)

    # ── summary properties ────────────────────────────────────────────────

    @property
    def total(self) -> int:
        return len(self.ops)

    @property
    def conflicts(self) -> List[PlannedOp]:
        return [op for op in self.ops if op.conflict]

    @property
    def conflict_count(self) -> int:
        return sum(1 for op in self.ops if op.conflict)

    # ── bulk resolution ───────────────────────────────────────────────────

    def apply_bulk_resolution(self, r: Resolution) -> None:
        """Set *r* on every conflicting op that has not been individually resolved."""
        for op in self.conflicts:
            op.set_resolution(r)

    # ── preview grouping ──────────────────────────────────────────────────

    def grouped(self) -> List[OpGroup]:
        """
        Return ops collected into OpGroup buckets, sorted oldest-first.

        Bucket key: (year, month, file_type)
        """
        index: Dict[Tuple[int, int, str], OpGroup] = {}
        for op in self.ops:
            key = (op.date.year, op.date.month, op.record.file_type.value)
            if key not in index:
                index[key] = OpGroup(
                    year       = op.date.year,
                    month      = op.date.month,
                    month_key  = month_folder(op.date),
                    type_label = op.record.file_type.value,
                )
            index[key].ops.append(op)

        return sorted(index.values(), key=lambda g: (g.year, g.month, g.type_label))

    # ── execution ─────────────────────────────────────────────────────────

    def execute(
        self,
        mode:     str,                                          # "copy" | "move"
        progress: Optional[Callable[[int, int], None]] = None, # (done, total)
    ) -> Tuple[
        List[PlannedOp],                  # succeeded
        List[Tuple[PlannedOp, str]],      # failed: (op, error_message)
    ]:
        """
        Execute all non-skip ops in the plan.

        Parameters
        ----------
        mode
            "copy" — duplicate files; originals untouched.
            "move" — relocate files; tries os.rename() first (atomic on the
                     same filesystem), falls back to copy + verify + delete
                     for cross-device moves.
        progress
            Optional callback ``progress(n_done, n_total)`` called after each
            completed operation — use to drive a progress bar.

        Returns
        -------
        succeeded
            Ops that completed without error.
        failed
            (op, error_message) pairs for ops that raised an exception.

        Notes
        -----
        - Skipped ops (Resolution.SKIP) are silently omitted from both lists.
        - Destination parent directories are created as needed.
        - File timestamps (atime + mtime) are preserved after copy.
        """
        assert mode in ("copy", "move"), f"mode must be 'copy' or 'move', got {mode!r}"

        active = [op for op in self.ops if op.resolution != Resolution.SKIP]
        total  = len(active)

        succeeded: List[PlannedOp]              = []
        failed:    List[Tuple[PlannedOp, str]]  = []

        for n, op in enumerate(active, 1):
            try:
                dest = op.final_dest
                dest.parent.mkdir(parents=True, exist_ok=True)

                src  = op.src
                stat = src.stat()

                if mode == "copy":
                    shutil.copy2(str(src), str(dest))
                else:
                    # Prefer os.rename (atomic, instant on same filesystem)
                    try:
                        os.rename(str(src), str(dest))
                    except OSError:
                        # Cross-device move: copy, verify size, then delete src
                        shutil.copy2(str(src), str(dest))
                        if dest.stat().st_size != stat.st_size:
                            dest.unlink(missing_ok=True)
                            raise RuntimeError(
                                f"size mismatch after copy: {src.name}"
                            )
                        src.unlink()

                # Restore mtime so the file date is stable in the library
                try:
                    os.utime(str(dest), (stat.st_atime, stat.st_mtime))
                except OSError:
                    pass

                succeeded.append(op)

            except Exception as exc:  # noqa: BLE001
                failed.append((op, str(exc)))

            if progress:
                progress(n, total)

        return succeeded, failed
