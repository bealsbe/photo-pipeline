"""
Background file scanner.

ScanWorker runs in a QThread and emits one signal per discovered file
so the UI can update incrementally without blocking.
"""
from __future__ import annotations

import os
from datetime import datetime
from pathlib import Path

from PySide6.QtCore import QThread, Signal

from app.models.photo_record import (
    FileType,
    PhotoRecord,
    RAW_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
)


class ScanWorker(QThread):
    """
    Walks *root_path* (optionally recursively) and emits a PhotoRecord
    for every supported image file found.

    Signals
    -------
    file_found(PhotoRecord)
        Emitted for each file as it is discovered.
    progress(int)
        Running total of files found so far (for status-bar updates).
    scan_complete(int)
        Emitted once when the walk finishes; carries the final file count.
    scan_error(str)
        Emitted if an unrecoverable exception occurs.
    """

    file_found: Signal = Signal(object)
    progress: Signal = Signal(int)
    scan_complete: Signal = Signal(int)
    scan_error: Signal = Signal(str)

    def __init__(self, root_path: Path, recursive: bool = True) -> None:
        super().__init__()
        self.root_path = root_path
        self.recursive = recursive
        self._cancelled = False

    def cancel(self) -> None:
        """Request cancellation.  The thread will stop at the next file."""
        self._cancelled = True

    # ------------------------------------------------------------------ #
    # Thread entry-point                                                   #
    # ------------------------------------------------------------------ #

    def run(self) -> None:
        try:
            count = 0

            if self.recursive:
                entries = os.walk(self.root_path)
            else:
                # Simulate os.walk for a single directory
                try:
                    names = os.listdir(self.root_path)
                except OSError as exc:
                    self.scan_error.emit(str(exc))
                    return
                entries = [(str(self.root_path), [], names)]

            for dirpath, dirnames, filenames in entries:
                if self._cancelled:
                    break

                # Skip hidden directories in-place to avoid descending
                dirnames[:] = [d for d in dirnames if not d.startswith(".")]

                for filename in filenames:
                    if self._cancelled:
                        break

                    ext = Path(filename).suffix.lower()
                    if ext not in SUPPORTED_EXTENSIONS:
                        continue

                    filepath = Path(dirpath) / filename
                    try:
                        stat = filepath.stat()
                    except OSError:
                        continue  # unreadable — skip silently

                    file_type = (
                        FileType.RAW if ext in RAW_EXTENSIONS else FileType.JPG
                    )
                    record = PhotoRecord(
                        path=filepath,
                        file_type=file_type,
                        file_size=stat.st_size,
                        modified_time=datetime.fromtimestamp(stat.st_mtime),
                    )
                    self.file_found.emit(record)
                    count += 1
                    # Throttle progress signals to every 10 files
                    if count % 10 == 0:
                        self.progress.emit(count)

            self.scan_complete.emit(count)

        except Exception as exc:  # noqa: BLE001
            self.scan_error.emit(str(exc))
