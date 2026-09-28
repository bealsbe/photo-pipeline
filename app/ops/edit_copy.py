"""
app/ops/edit_copy.py — copy the marked RAW files into a folder to edit from.

Unlike the library export this is not an archive step: no date tree, no
renaming, no JPGs.  Every marked shot's RAW lands flat in one folder, ready to
open in Lightroom / darktable / Capture One.  Originals are never touched.

Choosing the files
------------------
:func:`select_marked_raws` turns the marked records into RAW files:

* a marked RAW is taken as is;
* a marked JPG contributes its RAW partner — the confirmed pair if there is
  one, otherwise a RAW with the same stem in the same (canonical) folder.
  Marks only propagate across *confirmed* pairs, so without the name match a
  shot marked on its JPG thumbnail in an un-paired folder would be dropped;
* a marked JPG with no RAW at all is counted and reported, not copied;
* pruned shots are left out — a shot both marked and pruned is ambiguous, and
  copying something the user meant to delete is the worse mistake.

Names and collisions
--------------------
Files keep their camera names.  The plan never overwrites:

* the same file already at the destination (size and mtime match — copy2
  preserves mtime) is skipped, so re-running after marking a few more shots
  only copies the new ones;
* a *different* file already holding the name, or a second file in this same
  batch wanting it (two cards both with DSC01234.ARW), gets a ``_1``, ``_2``…
  suffix.  Names claimed by earlier files in the batch count as taken, not
  just names on disk.

Crops
-----
A cropped RAW gets an XMP sidecar with the crop (Camera Raw ``crs:Crop*``),
exactly as the library export writes it, so the editor opens it cropped.
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable, List, Optional, Tuple

from app.models.library_index import canonical_parent
from app.models.photo_record import FileType, PhotoRecord
from app.ops.crop_export import write_xmp_crop
from app.ops.library import _safe_copy

# FAT/exFAT store mtimes at 2 s resolution, and editing folders often live on
# external drives — allow for it when deciding "same file".
_MTIME_SLACK = 2.0


# ── choosing the files ────────────────────────────────────────────────────────

@dataclass
class MarkedRaws:
    """Result of :func:`select_marked_raws`."""
    raws:          List[PhotoRecord] = field(default_factory=list)
    matched_by_name: int = 0     # RAWs found by stem for a marked, un-paired JPG
    jpg_only:      List[PhotoRecord] = field(default_factory=list)  # no RAW exists
    pruned:        int = 0       # marked shots skipped because they are pruned


def select_marked_raws(
    records:      Iterable[PhotoRecord],
    pair_lookup:  Callable[[PhotoRecord], Optional[PhotoRecord]],
    stem_siblings: Callable[[PhotoRecord], List[PhotoRecord]],
) -> MarkedRaws:
    """Resolve every marked shot to its RAW file (see module docstring)."""
    out = MarkedRaws()
    seen: set = set()
    pruned_shots: set = set()

    def take(raw: PhotoRecord) -> None:
        if raw.path not in seen:
            seen.add(raw.path)
            out.raws.append(raw)

    for r in records:
        if not r.is_marked_for_export:
            continue
        if r.file_type == FileType.RAW:
            if r.is_pruned:
                pruned_shots.add((canonical_parent(r.path), r.stem.lower()))
                continue
            take(r)
            continue

        # A marked JPG — find the RAW of the same shot
        raw = pair_lookup(r)
        by_name = False
        if raw is None or raw.file_type != FileType.RAW:
            raw = next((s for s in stem_siblings(r) if s.file_type == FileType.RAW), None)
            by_name = raw is not None
        if raw is None:
            if r.is_pruned:
                pruned_shots.add((canonical_parent(r.path), r.stem.lower()))
            else:
                out.jpg_only.append(r)
            continue
        if r.is_pruned or raw.is_pruned:
            pruned_shots.add((canonical_parent(raw.path), raw.stem.lower()))
            continue
        if by_name and raw.path not in seen:
            out.matched_by_name += 1
        take(raw)

    out.pruned = len(pruned_shots)
    return out


# ── the plan ──────────────────────────────────────────────────────────────────

@dataclass
class CopyOp:
    record: PhotoRecord
    dest:   Path
    exists: bool = False     # identical file already there — nothing to do

    @property
    def renamed(self) -> bool:
        return self.dest.name != self.record.path.name


def _same_file(record: PhotoRecord, target: Path) -> bool:
    try:
        st = target.stat()
    except OSError:
        return False
    return (st.st_size == record.file_size
            and abs(st.st_mtime - record.modified_time.timestamp()) <= _MTIME_SLACK)


class EditCopyPlan:
    """Where each RAW goes in *dest_dir*.  Building it touches the disk read-only."""

    def __init__(self, raws: List[PhotoRecord], dest_dir: Path) -> None:
        self.dest_dir = dest_dir
        self.ops: List[CopyOp] = []

        # Capture order, so _1 suffixes go to the later shot
        ordered = sorted(raws, key=lambda r: (r.shot_time, r.path.name.lower()))
        claimed: set = set()      # lower-cased: editing drives are often exFAT/NTFS
        for r in ordered:
            target = dest_dir / r.path.name
            # Walk NAME, NAME_1, NAME_2 … up to the first free slot.  A copy
            # of this very file on the way means an earlier run put it there
            # (possibly under a suffix) — skip it rather than copy it again.
            dest, n, exists = target, 0, False
            while True:
                key = dest.name.lower()
                if key not in claimed:
                    if not os.path.lexists(dest):
                        break
                    if _same_file(r, dest):
                        exists = True
                        break
                n += 1
                dest = target.with_name(f"{target.stem}_{n}{target.suffix}")
            claimed.add(dest.name.lower())
            self.ops.append(CopyOp(r, dest, exists=exists))

    # ── summary ────────────────────────────────────────────────────────────
    @property
    def to_copy(self) -> List[CopyOp]:
        return [op for op in self.ops if not op.exists]

    @property
    def existing(self) -> List[CopyOp]:
        return [op for op in self.ops if op.exists]

    @property
    def renamed_count(self) -> int:
        return sum(1 for op in self.to_copy if op.renamed)

    @property
    def cropped_count(self) -> int:
        return sum(1 for op in self.to_copy if op.record.is_cropped)

    @property
    def total_bytes(self) -> int:
        return sum(op.record.file_size for op in self.to_copy)

    # ── execution ──────────────────────────────────────────────────────────
    def execute(
        self,
        progress:     Optional[Callable[[int, int, int, int], None]] = None,
        is_cancelled: Callable[[], bool] = lambda: False,
    ) -> Tuple[List[CopyOp], List[Tuple[CopyOp, str]], bool]:
        """
        Copy every op that isn't already there.

        progress(files_done, files_total, bytes_done, bytes_total) is called
        after each file.  Returns (copied, failed, cancelled).  A cancel stops
        between files; whatever was copied stays.
        """
        todo = self.to_copy
        total_bytes = sum(op.record.file_size for op in todo)
        done_bytes = 0
        copied: List[CopyOp] = []
        failed: List[Tuple[CopyOp, str]] = []

        for n, op in enumerate(todo, 1):
            if is_cancelled():
                return copied, failed, True
            try:
                src = op.record.path
                if not src.exists():
                    raise FileNotFoundError(f"source file no longer exists: {src.name}")
                # Re-check: something may have appeared since the plan was built.
                # This export never overwrites.
                if os.path.lexists(op.dest):
                    raise FileExistsError(f"{op.dest.name} appeared in the folder since the preview")
                op.dest.parent.mkdir(parents=True, exist_ok=True)
                _safe_copy(src, op.dest, src.stat())
                if op.record.is_cropped:
                    write_xmp_crop(op.dest, op.record.crop)
                copied.append(op)
            except Exception as exc:  # noqa: BLE001
                failed.append((op, str(exc)))
            done_bytes += op.record.file_size
            if progress:
                progress(n, len(todo), done_bytes, total_bytes)

        return copied, failed, False
