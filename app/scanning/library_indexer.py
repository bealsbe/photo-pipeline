"""
Background workers for the library calendar.

LibraryIndexWorker  walks the library once and emits a LibraryIndex — which
                    days hold photos, and how many.  Dates come from folder
                    names, so this is a directory walk plus one stat per file.
DayRecordsWorker    turns one day's files into full PhotoRecords (with EXIF
                    capture times, for in-day ordering) when the user opens it.
"""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from datetime import date as _date
from pathlib import Path
from typing import List

from PySide6.QtCore import QThread, Signal

from app.models.library_index import FileEntry, build_library_index
from app.scanning.scanner import build_records

_READ_WORKERS = 8


class LibraryIndexWorker(QThread):
    """
    Signals
    -------
    progress(int)            files seen so far
    index_ready(object)      LibraryIndex — not emitted if cancelled
    index_error(str)
    """

    progress:    Signal = Signal(int)
    index_ready: Signal = Signal(object)
    index_error: Signal = Signal(str)

    def __init__(self, root: Path) -> None:
        super().__init__()
        self.root = root
        self._cancelled = False

    def cancel(self) -> None:
        self._cancelled = True

    def run(self) -> None:
        try:
            with ThreadPoolExecutor(max_workers=_READ_WORKERS) as pool:
                index = build_library_index(
                    self.root,
                    is_cancelled=lambda: self._cancelled,
                    pool=pool,
                    progress=self.progress.emit,
                )
            if index is not None and not self._cancelled:
                self.index_ready.emit(index)
        except Exception as exc:  # noqa: BLE001
            self.index_error.emit(str(exc))


class DayRecordsWorker(QThread):
    """Build PhotoRecords for one library day.  Emits records_ready(date, list)."""

    records_ready: Signal = Signal(object, object)   # (date, List[PhotoRecord])

    def __init__(self, day: _date, files: List[FileEntry]) -> None:
        super().__init__()
        self.day = day
        self.files = list(files)

    def run(self) -> None:
        with ThreadPoolExecutor(max_workers=_READ_WORKERS) as pool:
            records = build_records(self.files, pool)
        self.records_ready.emit(self.day, records)
