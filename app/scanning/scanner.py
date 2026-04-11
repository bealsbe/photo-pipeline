"""
Background file scanner.

ScanWorker runs in a QThread and emits one signal per discovered file
so the UI can update incrementally without blocking.
"""
from __future__ import annotations

import os
from datetime import datetime
from pathlib import Path
from typing import Optional

from PySide6.QtCore import QThread, Signal

from app.models.photo_record import (
    FileType,
    PhotoRecord,
    RAW_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
)


def _read_tiff_datetime(filepath: Path) -> Optional[datetime]:
    """
    Minimal TIFF IFD walker for TIFF-based RAW formats (ARW, CR2, NEF, DNG …)
    that Pillow cannot identify.  Reads only tag entries — no pixel decode.
    Handles both little-endian (II) and big-endian (MM) byte order.
    """
    import struct as _s

    _DATE_TAGS  = {306, 36867, 36868}   # DateTime, DateTimeOriginal, DateTimeDigitized
    _EXIF_IFD   = 0x8769

    def _parse(s: str) -> Optional[datetime]:
        s = s.strip()
        if len(s) >= 19:
            try:
                return datetime.strptime(s[:19], "%Y:%m:%d %H:%M:%S")
            except ValueError:
                pass
        return None

    try:
        with open(str(filepath), "rb") as f:
            hdr = f.read(8)
            if len(hdr) < 8:
                return None
            if hdr[:2] == b'II':
                end = '<'
            elif hdr[:2] == b'MM':
                end = '>'
            else:
                return None   # not a TIFF container

            ifd0 = _s.unpack_from(end + 'I', hdr, 4)[0]

            def entries(offset: int):
                f.seek(offset)
                buf = f.read(2)
                if len(buf) < 2:
                    return []
                n = _s.unpack(end + 'H', buf)[0]
                out = []
                for _ in range(n):
                    e = f.read(12)
                    if len(e) < 12:
                        break
                    out.append(_s.unpack(end + 'HHII', e))
                return out

            def read_str(offset: int, count: int) -> str:
                f.seek(offset)
                return f.read(count).rstrip(b'\x00').decode('ascii', errors='replace')

            exif_ifd = None
            for tag, _typ, cnt, val_off in entries(ifd0):
                if tag in _DATE_TAGS and cnt > 4:
                    dt = _parse(read_str(val_off, cnt))
                    if dt:
                        return dt
                if tag == _EXIF_IFD:
                    exif_ifd = val_off

            if exif_ifd:
                for tag, _typ, cnt, val_off in entries(exif_ifd):
                    if tag in _DATE_TAGS and cnt > 4:
                        dt = _parse(read_str(val_off, cnt))
                        if dt:
                            return dt
    except Exception:
        pass

    return None


def _read_capture_time(filepath: Path, file_type: "FileType") -> Optional[datetime]:
    """
    Try to read DateTimeOriginal (tag 36867) from the file's EXIF.
    Reads only headers — never decodes pixels — so it's fast enough for
    the scan loop.  Returns None silently on any failure.

    Strategy
    --------
    1. Pillow  — works for JPEG (and any RAW Pillow can identify, e.g. DNG)
    2. TIFF walker — works for Sony ARW, Canon CR2/CR3, Nikon NEF, and any
       other TIFF-based RAW that Pillow cannot open.  Pure stdlib, no deps.
    """
    # 1. Pillow: fast path for JPEG + identifiable RAW containers
    try:
        from PIL import Image as _PImg
        with _PImg.open(str(filepath)) as img:
            exif = img.getexif()
            ifd  = exif.get_ifd(0x8769)
            raw  = ifd.get(36867) or ifd.get(36868) or exif.get(306)
            if raw:
                return datetime.strptime(str(raw)[:19], "%Y:%m:%d %H:%M:%S")
    except Exception:
        pass

    # 2. Direct TIFF parse: covers ARW/CR2/NEF/DNG that Pillow can't identify
    if file_type == FileType.RAW:
        dt = _read_tiff_datetime(filepath)
        if dt:
            return dt

    return None


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
                        capture_time=_read_capture_time(filepath, file_type),
                    )
                    self.file_found.emit(record)
                    count += 1
                    # Throttle progress signals to every 10 files
                    if count % 10 == 0:
                        self.progress.emit(count)

            self.scan_complete.emit(count)

        except Exception as exc:  # noqa: BLE001
            self.scan_error.emit(str(exc))
