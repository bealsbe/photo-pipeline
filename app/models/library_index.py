"""
app/models/library_index.py — which days of the library hold photos.

Pure data, no Qt.  Built on a worker thread by LibraryIndexWorker and read by
the calendar view.

Dates come from the path first
------------------------------
The library is laid out by date (``2025/Mar-2025/12-Mar-2025/RAW/…``, see
app/ops/library.py), so the day a file belongs to is already written in its
folder name.  Reading it from there costs nothing and is also *more* correct
than re-reading EXIF: export chose the folder with pair-aware rules
(:func:`best_date`), so both halves of a pair always agree.

In priority order:

1. An ancestor folder named ``DD-Mon-YYYY`` — the day folder.
2. A renamed stem ``YYYYMMDD_HHMM_NNNN`` — libraries exported before day
   folders existed keep their files directly in the month folder, but those
   files were usually renamed.
3. EXIF capture time, then mtime — anything else dropped into the tree.

Only case 3 touches file contents, and only for the files that need it.
"""
from __future__ import annotations

import bisect
import calendar
import os
import re
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from datetime import date as _date, datetime
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

from app.exif import read_capture_time
from app.models.photo_record import (
    FileType,
    PhotoRecord,
    RAW_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
)

FileEntry = Tuple[Path, FileType, os.stat_result]

# Month abbreviations exactly as app/ops/library.py writes them.  Built from
# the same table (calendar.month_abbr) so a non-English locale still parses
# the folders it produced.
_MONTHS: Dict[str, int] = {
    calendar.month_abbr[m].lower(): m for m in range(1, 13)
}

_DAY_DIR_RE = re.compile(r"^(\d{2})-([^\W\d_]+)-(\d{4})$")
_RENAMED_RE = re.compile(r"^(\d{8})_\d{4}_\d+")

_MIN_YEAR = 1990

# Number of shading steps in the year heat map (0 = no photos).
HEAT_LEVELS = 4


def _plausible(d: _date) -> bool:
    return _MIN_YEAR <= d.year <= datetime.now().year + 1


def date_from_day_folder(name: str) -> Optional[_date]:
    """Parse a ``12-Mar-2025`` folder name, or None."""
    m = _DAY_DIR_RE.match(name)
    if not m:
        return None
    month = _MONTHS.get(m[2].lower())
    if month is None:
        return None
    try:
        d = _date(int(m[3]), month, int(m[1]))
    except ValueError:
        return None
    return d if _plausible(d) else None


def date_from_renamed_stem(stem: str) -> Optional[_date]:
    """Parse the date out of a ``20250312_1430_0007`` stem, or None."""
    m = _RENAMED_RE.match(stem)
    if not m:
        return None
    try:
        d = datetime.strptime(m[1], "%Y%m%d").date()
    except ValueError:
        return None
    return d if _plausible(d) else None


def canonical_parent(path: Path) -> Path:
    """Strip a trailing RAW/ or JPG/ level, as PhotoCollection does."""
    return path.parent.parent if path.parent.name.upper() in ("RAW", "JPG") else path.parent


def record_from_entry(entry: FileEntry) -> PhotoRecord:
    """
    A PhotoRecord for an indexed file, without reading its capture time.

    Fields match ScanWorker's exactly — DiskThumbnailCache keys on mtime and
    size, so a calendar preview hits the same cache entry as the grid did.
    """
    path, file_type, st = entry
    return PhotoRecord(
        path=path,
        file_type=file_type,
        file_size=st.st_size,
        modified_time=datetime.fromtimestamp(st.st_mtime),
    )


# ── one day ─────────────────────────────────────────────────────────────────

@dataclass
class LibraryDay:
    """Every library file shot on one calendar day."""

    date:  _date
    files: List[FileEntry] = field(default_factory=list)

    # Filled in by _finalise()
    shot_count: int = 0
    raw_count:  int = 0
    jpg_count:  int = 0
    folder:     Optional[Path] = None
    _shots:     List[List[FileEntry]] = field(default_factory=list, repr=False)
    _previews:  Dict[int, List[PhotoRecord]] = field(default_factory=dict, repr=False)

    def _finalise(self) -> None:
        self.files.sort(key=lambda e: e[0].name.lower())
        self.raw_count = sum(1 for e in self.files if e[1] == FileType.RAW)
        self.jpg_count = len(self.files) - self.raw_count

        # A RAW and JPG sharing a stem and canonical parent are one shot.
        shots: Dict[Tuple[Path, str], List[FileEntry]] = {}
        for e in self.files:
            shots.setdefault((canonical_parent(e[0]), e[0].stem.lower()), []).append(e)
        self._shots = sorted(shots.values(), key=lambda s: s[0][0].name.lower())
        self.shot_count = len(self._shots)

        # Prefer the day folder; fall back to the most common parent.
        parents: Dict[Path, int] = {}
        for e in self.files:
            p = canonical_parent(e[0])
            parents[p] = parents.get(p, 0) + 1
        day_dirs = [p for p in parents if date_from_day_folder(p.name) == self.date]
        self.folder = day_dirs[0] if day_dirs else max(parents, key=parents.get)

    def preview_records(self, n: int) -> List[PhotoRecord]:
        """
        Up to *n* shots spread evenly through the day, as PhotoRecords.

        Takes the JPG half of each shot when there is one: its embedded MPF
        preview decodes an order of magnitude faster than a RAW thumbnail.
        Cached, so the records keep their identity across repaints.
        """
        cached = self._previews.get(n)
        if cached is not None:
            return cached
        shots = self._shots
        if len(shots) <= n:
            picked = shots
        else:
            step = len(shots) / n
            picked = [shots[int(i * step + step / 2)] for i in range(n)]
        out = []
        for shot in picked:
            jpg = next((e for e in shot if e[1] == FileType.JPG), shot[0])
            out.append(record_from_entry(jpg))
        self._previews[n] = out
        return out


# ── whole library ───────────────────────────────────────────────────────────

class LibraryIndex:
    """Days of the library that hold photos, with per-day counts."""

    def __init__(self, root: Path, days: Dict[_date, LibraryDay]) -> None:
        self.root = root
        self.days = days
        for d in days.values():
            d._finalise()
        self._sorted: List[_date] = sorted(days)
        self.total_shots = sum(d.shot_count for d in days.values())
        self.total_files = sum(len(d.files) for d in days.values())

        # Heat thresholds are quantiles of the per-day shot counts, so a single
        # 2 000-shot wedding day does not push every other day into the
        # palest shade.
        counts = sorted(d.shot_count for d in days.values())
        if counts:
            self._thresholds = [
                counts[min(len(counts) - 1, int(len(counts) * q / HEAT_LEVELS))]
                for q in range(1, HEAT_LEVELS)
            ]
        else:
            self._thresholds = []

    def __len__(self) -> int:
        return len(self.days)

    def get(self, d: _date) -> Optional[LibraryDay]:
        return self.days.get(d)

    def years(self) -> List[int]:
        return sorted({d.year for d in self._sorted})

    def days_in_month(self, year: int, month: int) -> Dict[int, LibraryDay]:
        lo = bisect.bisect_left(self._sorted, _date(year, month, 1))
        out: Dict[int, LibraryDay] = {}
        for d in self._sorted[lo:]:
            if d.year != year or d.month != month:
                break
            out[d.day] = self.days[d]
        return out

    def month_shot_count(self, year: int, month: int) -> int:
        return sum(d.shot_count for d in self.days_in_month(year, month).values())

    def year_shot_count(self, year: int) -> int:
        return sum(self.days[d].shot_count for d in self._sorted if d.year == year)

    def adjacent_day(self, d: _date, direction: int) -> Optional[_date]:
        """The nearest day with photos after (+1) or before (-1) *d*."""
        if direction > 0:
            i = bisect.bisect_right(self._sorted, d)
            return self._sorted[i] if i < len(self._sorted) else None
        i = bisect.bisect_left(self._sorted, d) - 1
        return self._sorted[i] if i >= 0 else None

    def nearest_day(self, d: _date) -> Optional[_date]:
        """The day with photos closest to *d* (either side)."""
        after, before = self.adjacent_day(d, 1), self.adjacent_day(d, -1)
        if d in self.days:
            return d
        if after is None or before is None:
            return after or before
        return before if (d - before) <= (after - d) else after

    def latest_day(self) -> Optional[_date]:
        return self._sorted[-1] if self._sorted else None

    def heat_level(self, day: Optional[LibraryDay]) -> int:
        """0 for no photos, then 1..HEAT_LEVELS by quantile of shot count."""
        if day is None or day.shot_count == 0:
            return 0
        return 1 + bisect.bisect_left(self._thresholds, day.shot_count)


# ── building ────────────────────────────────────────────────────────────────

def build_library_index(
    root: Path,
    is_cancelled: Callable[[], bool] = lambda: False,
    pool: Optional[ThreadPoolExecutor] = None,
    progress: Optional[Callable[[int], None]] = None,
) -> Optional[LibraryIndex]:
    """
    Walk *root* and bucket every supported image by day.

    Returns None if cancelled part-way.  *pool* runs the EXIF fallback reads;
    without one they run serially.
    """
    days: Dict[_date, LibraryDay] = {}
    unresolved: List[FileEntry] = []
    seen = 0

    def add(d: _date, entry: FileEntry) -> None:
        day = days.get(d)
        if day is None:
            day = days[d] = LibraryDay(d)
        day.files.append(entry)

    # Stack of (directory, date inherited from an enclosing day folder)
    stack: List[Tuple[str, Optional[_date]]] = [(str(root), None)]
    while stack:
        if is_cancelled():
            return None
        dirpath, inherited = stack.pop()
        try:
            it = os.scandir(dirpath)
        except OSError:
            continue
        with it:
            for entry in it:
                name = entry.name
                if name.startswith("."):
                    continue
                try:
                    is_dir = entry.is_dir(follow_symlinks=False)
                except OSError:
                    continue
                if is_dir:
                    stack.append((entry.path, date_from_day_folder(name) or inherited))
                    continue
                ext = os.path.splitext(name)[1].lower()
                if ext not in SUPPORTED_EXTENSIONS:
                    continue
                try:
                    st = entry.stat()
                except OSError:
                    continue
                path = Path(entry.path)
                ft = FileType.RAW if ext in RAW_EXTENSIONS else FileType.JPG
                fe: FileEntry = (path, ft, st)
                d = inherited or date_from_renamed_stem(path.stem)
                if d is not None:
                    add(d, fe)
                else:
                    unresolved.append(fe)
                seen += 1
                if progress and seen % 500 == 0:
                    progress(seen)

    if unresolved:
        def resolve(fe: FileEntry) -> _date:
            path, ft, st = fe
            ct = read_capture_time(path, ft == FileType.RAW)
            if ct is not None and _plausible(ct.date()):
                return ct.date()
            return datetime.fromtimestamp(st.st_mtime).date()

        mapper = pool.map if pool is not None else map
        for fe, d in zip(unresolved, mapper(resolve, unresolved)):
            if is_cancelled():
                return None
            add(d, fe)

    if progress:
        progress(seen)
    return LibraryIndex(root, days)
