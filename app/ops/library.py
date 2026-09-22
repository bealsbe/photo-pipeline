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

Cropping
    A record carrying a Crop is exported cropped.  JPGs are re-encoded with
    the crop baked into the pixels; RAWs are copied byte-for-byte and get an
    XMP sidecar holding the same rect (see app/ops/crop_export.py for why).
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
from app.ops.crop_export import (
    remove_xmp_crop,
    render_cropped_jpeg,
    write_xmp_crop,
)

# ── date validation ───────────────────────────────────────────────────────────

_MIN_YEAR = 1990


def _valid(dt: datetime) -> bool:
    """True if *dt* falls in the plausible photo-date range."""
    return _MIN_YEAR <= dt.year <= datetime.now().year + 1


# ── date selection ────────────────────────────────────────────────────────────

def best_datetime(
    record: PhotoRecord,
    pair:   Optional[PhotoRecord] = None,
) -> datetime:
    """
    Return the most reliable shot *datetime* for *record* — same priority
    rules as :func:`best_date` but without truncating to a date.
    """
    jpg = record if record.file_type == FileType.JPG else pair
    raw = record if record.file_type == FileType.RAW else pair

    if jpg and jpg.capture_time and _valid(jpg.capture_time):
        return jpg.capture_time
    if raw and raw.capture_time and _valid(raw.capture_time):
        return raw.capture_time
    if raw and _valid(raw.modified_time):
        return raw.modified_time
    return record.modified_time


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
    return best_datetime(record, pair).date()


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


# ── safe copy helper ──────────────────────────────────────────────────────

def _safe_copy(src: Path, dest: Path, stat: os.stat_result) -> None:
    """Copy *src* → *dest* atomically via a temp file + os.replace().

    Writes to a ``.~tmp`` sibling, verifies the size matches *stat*, then
    atomically replaces *dest*.  If anything fails before the replace, the
    original *dest* (if any) is untouched and the temp file is cleaned up.
    """
    tmp_dest = dest.with_suffix(dest.suffix + ".~tmp")
    try:
        shutil.copy2(str(src), str(tmp_dest))
        if tmp_dest.stat().st_size != stat.st_size:
            raise RuntimeError(f"size mismatch after copy: {src.name}")
        os.replace(str(tmp_dest), str(dest))
    except Exception:
        tmp_dest.unlink(missing_ok=True)
        raise


def _clear_stale_crop(op: "PlannedOp", dest: Path) -> None:
    """
    Drop a crop left in the destination's XMP sidecar by an earlier export.

    Only runs when the user deliberately overwrote an existing file: without
    that guard, exporting an uncropped RAW would silently wipe a crop someone
    had since made in Lightroom against the same sidecar.
    """
    if op.record.file_type != FileType.RAW:
        return
    if not (op.conflict and op.resolution == Resolution.OVERWRITE):
        return
    remove_xmp_crop(dest)


def _write_with_crop(op: "PlannedOp", dest: Path, stat: os.stat_result) -> None:
    """
    Materialise *op* at *dest*, applying the record's crop.

    JPG  — re-encoded with the crop applied to the pixels.
    RAW  — copied untouched, then an XMP sidecar is written alongside it.
    """
    record = op.record
    crop   = record.crop

    if record.file_type == FileType.JPG:
        render_cropped_jpeg(op.src, dest, crop)
        return

    # RAW: the bits are sacred.  Copy, then describe the crop next to them.
    _safe_copy(op.src, dest, stat)
    write_xmp_crop(dest, crop)


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

    @property
    def pruned_count(self) -> int:
        return sum(1 for op in self.ops if op.record.is_pruned)

    @property
    def cropped_count(self) -> int:
        return sum(1 for op in self.ops if op.record.is_cropped)


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

    @property
    def cropped_count(self) -> int:
        return sum(1 for op in self.ops if op.record.is_cropped)

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
        progress:    Optional[Callable[[int, int], None]] = None, # (done, total)
        source_root: Optional[Path] = None,                       # bound for empty-dir cleanup
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
        - Records carrying a crop are written through _write_with_crop(): the
          JPG is re-encoded cropped, the RAW is copied intact beside an XMP
          sidecar.  A cropped move therefore re-encodes and then unlinks the
          source rather than renaming it.
        """
        assert mode in ("copy", "move"), f"mode must be 'copy' or 'move', got {mode!r}"

        active = [op for op in self.ops if op.resolution != Resolution.SKIP]
        total  = len(active)

        succeeded: List[PlannedOp]              = []
        failed:    List[Tuple[PlannedOp, str]]  = []

        for n, op in enumerate(active, 1):
            try:
                dest = op.final_dest

                # Guard against destinations that appeared after the plan was built
                if op.resolution != Resolution.OVERWRITE and dest.exists():
                    failed.append((op, f"destination appeared after plan: {dest.name}"))
                    if progress:
                        progress(n, total)
                    continue

                dest.parent.mkdir(parents=True, exist_ok=True)

                src = op.src
                if not src.exists():
                    failed.append((op, f"source file no longer exists: {src.name}"))
                    if progress:
                        progress(n, total)
                    continue
                stat = src.stat()

                cropped = op.record.is_cropped

                if cropped:
                    # A cropped JPG is a different file from its source, so the
                    # rename fast-path and the size-verified copy both go out
                    # the window — write the new pixels, then drop the original
                    # if this was a move.
                    _write_with_crop(op, dest, stat)
                    if mode == "move":
                        src.unlink()
                elif mode == "copy":
                    _safe_copy(src, dest, stat)
                    _clear_stale_crop(op, dest)
                else:
                    # Prefer os.rename (atomic, instant on same filesystem)
                    try:
                        os.rename(str(src), str(dest))
                    except OSError:
                        # Cross-device move: safe copy then delete src
                        _safe_copy(src, dest, stat)
                        src.unlink()
                    _clear_stale_crop(op, dest)

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

        # After a move, silently remove any source directories that are now empty.
        # Walk bottom-up through every parent that lost a file so nested empties
        # are cleaned up too (e.g. RAW/ inside a now-empty shoot folder).
        if mode == "move" and succeeded:
            dirs_to_check: set[Path] = set()
            for op in succeeded:
                dirs_to_check.add(op.src.parent)

            # Expand upward so we catch grandparent folders too, up to source_root.
            # Without a source_root we skip cleanup entirely — walking to the
            # filesystem root is never safe.
            bound = source_root
            expanded: set[Path] = set()
            if bound is not None:
                for d in dirs_to_check:
                    p = d
                    while p != bound and p != p.parent:
                        expanded.add(p)
                        p = p.parent

            for d in sorted(expanded, key=lambda p: len(p.parts), reverse=True):
                try:
                    d.rmdir()   # no-op if the directory is not empty
                except OSError:
                    pass

        return succeeded, failed
