"""
Background file scanner.

ScanWorker runs in a QThread and emits batches of PhotoRecords so the UI can
update incrementally without blocking.

Why it is threaded internally
-----------------------------
The walk itself is trivial; the cost is reading a capture timestamp out of
every file.  That is pure I/O latency — a couple of small reads per file, with
nothing to compute between them — so a serial loop spends almost all its time
waiting.  A pool of readers overlaps those waits: on a cold 137-file RAW
folder, 1.86 ms/file serial became 0.16 ms/file across 8 threads, and even
warm-cache 64 KB header reads went 0.454 → 0.112 ms/file.

Records are emitted in batches rather than one signal per file.  A 10 000-file
shoot would otherwise queue 10 000 cross-thread signal emissions, each waking
the main loop to append a single row.
"""
from __future__ import annotations

import os
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path
from typing import List, Optional, Tuple

from PySide6.QtCore import QThread, Signal

from app.exif import read_capture_time
from app.models.photo_record import (
    FileType,
    PhotoRecord,
    RAW_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
)

# Readers are I/O-bound, so oversubscribing cores pays.  Past ~8 the queue
# depth stops helping on NVMe and starts costing context switches.
_READ_WORKERS = 8

# Emitted batch size.  Large enough that signal overhead disappears, small
# enough that the grid still fills in visibly during a long scan.
_BATCH = 250


class ScanWorker(QThread):
    """
    Walks *root_path* (optionally recursively) and emits PhotoRecords for
    every supported image file found.

    Signals
    -------
    files_found(list[PhotoRecord])
        Emitted per batch as files are discovered.
    progress(int)
        Running total of files found so far (for status-bar updates).
    scan_complete(int)
        Emitted once when the walk finishes; carries the final file count.
    scan_error(str)
        Emitted if an unrecoverable exception occurs.
    """

    files_found: Signal = Signal(object)     # List[PhotoRecord]
    progress: Signal = Signal(int)
    scan_complete: Signal = Signal(int)
    scan_error: Signal = Signal(str)

    def __init__(self, root_path: Path, recursive: bool = True) -> None:
        super().__init__()
        self.root_path = root_path
        self.recursive = recursive
        self._cancelled = False

    def cancel(self) -> None:
        """Request cancellation.  The thread will stop at the next batch."""
        self._cancelled = True

    # ------------------------------------------------------------------ #
    # Thread entry-point                                                   #
    # ------------------------------------------------------------------ #

    def run(self) -> None:
        try:
            count = 0
            with ThreadPoolExecutor(max_workers=_READ_WORKERS) as pool:
                for batch in self._walk_batches():
                    if self._cancelled:
                        break
                    records = build_records(batch, pool)
                    if not records:
                        continue
                    count += len(records)
                    self.files_found.emit(records)
                    self.progress.emit(count)

            self.scan_complete.emit(count)

        except Exception as exc:  # noqa: BLE001
            self.scan_error.emit(str(exc))

    # ------------------------------------------------------------------ #
    # Internal                                                             #
    # ------------------------------------------------------------------ #

    def _walk_batches(self):
        """
        Yield lists of (path, file_type, stat_result), at most _BATCH long.

        Only cheap operations happen here — a directory walk and one stat per
        file.  Timestamp reads are left to the pool.
        """
        if self.recursive:
            entries = os.walk(self.root_path)
        else:
            try:
                names = os.listdir(self.root_path)
            except OSError as exc:
                self.scan_error.emit(str(exc))
                return
            entries = [(str(self.root_path), [], names)]

        batch: List[Tuple[Path, FileType, os.stat_result]] = []

        for dirpath, dirnames, filenames in entries:
            if self._cancelled:
                break

            # Skip hidden directories in-place to avoid descending
            dirnames[:] = [d for d in dirnames if not d.startswith(".")]

            for filename in filenames:
                if self._cancelled:
                    break

                ext = os.path.splitext(filename)[1].lower()
                if ext not in SUPPORTED_EXTENSIONS:
                    continue

                filepath = Path(dirpath) / filename
                try:
                    stat = filepath.stat()
                except OSError:
                    continue  # unreadable — skip silently

                file_type = FileType.RAW if ext in RAW_EXTENSIONS else FileType.JPG
                batch.append((filepath, file_type, stat))

                if len(batch) >= _BATCH:
                    yield batch
                    batch = []

        if batch and not self._cancelled:
            yield batch


def build_records(
    batch: List[Tuple[Path, FileType, os.stat_result]],
    pool: ThreadPoolExecutor,
) -> List[PhotoRecord]:
    """
    Read capture times for *batch* in parallel and build the records.

    Module-level so the library calendar can build records for one day's
    files with exactly the same fields — the disk thumbnail cache is keyed on
    mtime and size, so any drift here would miss the cache.
    """
    paths = [(path, file_type == FileType.RAW) for path, file_type, _ in batch]
    # map() preserves submission order, so records stay in walk order.
    capture_times: List[Optional[datetime]] = list(
        pool.map(lambda args: read_capture_time(*args), paths)
    )

    return [
        PhotoRecord(
            path=path,
            file_type=file_type,
            file_size=stat.st_size,
            modified_time=datetime.fromtimestamp(stat.st_mtime),
            capture_time=capture_time,
        )
        for (path, file_type, stat), capture_time in zip(batch, capture_times)
    ]
