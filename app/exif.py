"""
Container-level metadata reading — no pixel decode, no third-party deps.

Both the scanner (capture time, for date grouping) and the thumbnail generator
(orientation + embedded previews) need to peek inside image containers without
paying for a decode.  They previously did it twice, with two private TIFF
walkers; this module is the single implementation.

Two container shapes are handled:

**TIFF** — ARW, CR2, NEF, DNG, RW2 and friends are all TIFF containers, so the
IFD chain sits at the front of the file.  Pillow cannot identify most vendor
RAW dialects (0 of 80 ARW files in testing), so walking the IFDs directly is
both faster and more reliable.

**JPEG** — metadata lives in APP segments before the scan data: EXIF in APP1
(a complete TIFF stream), and on Sony/Canon/Fujifilm/Panasonic bodies an MPF
preview index in APP2.

Reads are bounded: a JPEG's segments are found within the first
:data:`_HEADER_BYTES`, and TIFF IFD offsets are resolved with seeks rather than
by slurping the file, so an 84 MB RAW costs a few small reads.
"""
from __future__ import annotations

import struct
from datetime import datetime
from pathlib import Path
from typing import Iterator, Optional, Tuple

# APP1/APP2 segments live in the first few hundred KB; 256 KB is generous.
_HEADER_BYTES = 262_144

_SOI  = b"\xff\xd8"
_APP1 = 0xE1
_APP2 = 0xE2
_SOS  = 0xDA          # start of scan — no metadata segments past here

_TAG_ORIENTATION  = 0x0112
_TAG_DATETIME     = 0x0132   # IFD0 DateTime
_TAG_THUMB_OFFSET = 0x0201   # JPEGInterchangeFormat
_TAG_THUMB_LENGTH = 0x0202   # JPEGInterchangeFormatLength
_TAG_EXIF_IFD     = 0x8769
_TAG_DT_ORIGINAL  = 0x9003   # DateTimeOriginal
_TAG_DT_DIGITIZED = 0x9004   # DateTimeDigitized
_TAG_MP_ENTRY     = 0xB002

_DATE_TAGS = (_TAG_DT_ORIGINAL, _TAG_DT_DIGITIZED, _TAG_DATETIME)


# ────────────────────────────────────────────────────────────────────────────
# Byte sources
# ────────────────────────────────────────────────────────────────────────────

class _Source:
    """Random-access byte reader; returns b'' rather than raising past EOF."""

    def read(self, offset: int, size: int) -> bytes:    # pragma: no cover
        raise NotImplementedError


class _BytesSource(_Source):
    """A TIFF stream already held in memory (the EXIF blob inside APP1)."""

    def __init__(self, buf: bytes) -> None:
        self._buf = buf

    def read(self, offset: int, size: int) -> bytes:
        if offset < 0 or offset >= len(self._buf):
            return b""
        return self._buf[offset:offset + size]


class _FileSource(_Source):
    """A TIFF container on disk (a RAW file), read by seeking."""

    def __init__(self, fh) -> None:
        self._fh = fh

    def read(self, offset: int, size: int) -> bytes:
        if offset < 0:
            return b""
        try:
            self._fh.seek(offset)
            return self._fh.read(size)
        except OSError:
            return b""


# ────────────────────────────────────────────────────────────────────────────
# TIFF structure
# ────────────────────────────────────────────────────────────────────────────

def _byte_order(buf: bytes) -> Optional[str]:
    """Return the struct prefix for a TIFF header, or None if not a TIFF."""
    if buf[:2] == b"II":
        return "<"
    if buf[:2] == b"MM":
        return ">"
    return None


def _ifd_entries(src: _Source, end: str, offset: int) -> Iterator[Tuple[int, int, int, int]]:
    """Yield (tag, type, count, value) for every entry in the IFD at *offset*."""
    if offset <= 0:
        return
    head = src.read(offset, 2)
    if len(head) < 2:
        return
    count = struct.unpack(end + "H", head)[0]
    raw = src.read(offset + 2, count * 12)
    for i in range(min(count, len(raw) // 12)):
        yield struct.unpack_from(end + "HHII", raw, i * 12)


def _next_ifd(src: _Source, end: str, offset: int) -> int:
    """Return the offset of the IFD after the one at *offset* (0 if none)."""
    if offset <= 0:
        return 0
    head = src.read(offset, 2)
    if len(head) < 2:
        return 0
    count = struct.unpack(end + "H", head)[0]
    tail = src.read(offset + 2 + count * 12, 4)
    return struct.unpack(end + "I", tail)[0] if len(tail) == 4 else 0


def _short_value(end: str, value: int) -> int:
    """Extract a SHORT from a TIFF value field (position depends on endianness)."""
    return struct.unpack(end + "HH", struct.pack(end + "I", value))[0]


def _parse_datetime(text: str) -> Optional[datetime]:
    """Parse an EXIF "YYYY:MM:DD HH:MM:SS" string."""
    text = text.strip()
    if len(text) >= 19:
        try:
            return datetime.strptime(text[:19], "%Y:%m:%d %H:%M:%S")
        except ValueError:
            pass
    return None


def _capture_time_from_tiff(src: _Source, end: str, ifd0: int) -> Optional[datetime]:
    """
    Find the best capture timestamp in a TIFF stream.

    Prefers DateTimeOriginal from the Exif sub-IFD, falling back to IFD0's
    DateTime.  Date strings are 20 bytes, so they always live at an offset
    rather than inline in the 4-byte value field.
    """
    exif_ifd = 0
    fallback: Optional[datetime] = None

    for tag, _typ, count, value in _ifd_entries(src, end, ifd0):
        if tag == _TAG_EXIF_IFD:
            exif_ifd = value
        elif tag in _DATE_TAGS and count > 4:
            text = src.read(value, count).rstrip(b"\x00").decode("ascii", "replace")
            parsed = _parse_datetime(text)
            if parsed and tag != _TAG_DATETIME:
                return parsed
            fallback = fallback or parsed

    for tag, _typ, count, value in _ifd_entries(src, end, exif_ifd):
        if tag in (_TAG_DT_ORIGINAL, _TAG_DT_DIGITIZED) and count > 4:
            text = src.read(value, count).rstrip(b"\x00").decode("ascii", "replace")
            parsed = _parse_datetime(text)
            if parsed:
                return parsed

    return fallback


# ────────────────────────────────────────────────────────────────────────────
# JPEG structure
# ────────────────────────────────────────────────────────────────────────────

def _iter_segments(head: bytes) -> Iterator[Tuple[int, int, int]]:
    """
    Yield (marker, payload_start, payload_end) for each JPEG marker segment.

    Stops at SOS — nothing useful follows the compressed scan data.
    """
    if head[:2] != _SOI:
        return
    i = 2
    while i + 4 <= len(head):
        if head[i] != 0xFF:
            return                      # desynchronised — give up
        marker = head[i + 1]
        if marker == _SOS:
            return
        length = struct.unpack_from(">H", head, i + 2)[0]
        if length < 2:
            return
        yield marker, i + 4, i + 2 + length
        i += 2 + length


def _exif_tiff(head: bytes) -> Optional[Tuple[bytes, str, int, int]]:
    """
    Locate the EXIF TIFF stream in a JPEG header.

    Returns (tiff_bytes, byte_order, ifd0_offset, absolute_base) where
    *absolute_base* is the file offset of the TIFF header, so that offsets
    inside the stream can be turned back into file positions.
    """
    for marker, start, stop in _iter_segments(head):
        if marker != _APP1 or head[start:start + 6] != b"Exif\x00\x00":
            continue
        base = start + 6
        tiff = head[base:stop]
        end = _byte_order(tiff)
        if end is None or len(tiff) < 8:
            return None
        return tiff, end, struct.unpack_from(end + "I", tiff, 4)[0], base
    return None


# ────────────────────────────────────────────────────────────────────────────
# Public API
# ────────────────────────────────────────────────────────────────────────────

def read_capture_time(path: Path, is_raw: bool) -> Optional[datetime]:
    """
    Return the capture timestamp for *path*, or None if it has none.

    Reads only container metadata — never decodes pixels — so it is cheap
    enough to run over every file in a scan.  Returns None silently on any
    malformed or unreadable file.
    """
    try:
        with open(path, "rb") as fh:
            head = fh.read(8 if is_raw else _HEADER_BYTES)
            if not head:
                return None

            if is_raw:
                # A RAW file *is* a TIFF container: walk it in place.
                end = _byte_order(head)
                if end is None or len(head) < 8:
                    return None
                ifd0 = struct.unpack_from(end + "I", head, 4)[0]
                return _capture_time_from_tiff(_FileSource(fh), end, ifd0)

            found = _exif_tiff(head)
            if found is None:
                return None
            tiff, end, ifd0, _base = found
            return _capture_time_from_tiff(_BytesSource(tiff), end, ifd0)
    except (OSError, struct.error, IndexError, ValueError):
        return None


def exif_orientation(path: Path) -> int:
    """
    Return the EXIF Orientation (1–8) from a JPEG's IFD0.

    Returns 1 (no transform) when the tag is absent or unreadable.
    """
    try:
        with open(path, "rb") as fh:
            head = fh.read(_HEADER_BYTES)
        found = _exif_tiff(head)
        if found is None:
            return 1
        tiff, end, ifd0, _base = found
        for tag, _typ, _count, value in _ifd_entries(_BytesSource(tiff), end, ifd0):
            if tag == _TAG_ORIENTATION:
                orient = _short_value(end, value)
                return orient if 1 <= orient <= 8 else 1
    except (OSError, struct.error, IndexError):
        pass
    return 1


def _mpf_preview_span(head: bytes) -> Optional[Tuple[int, int]]:
    """
    Return (absolute_offset, length) of the largest non-primary MPF image.

    MPEntry offsets are relative to the start of the MP Endian field, i.e. the
    first byte of the TIFF header following the "MPF\\0" identifier.  The
    primary image is stored with offset 0 and is skipped.
    """
    for marker, start, stop in _iter_segments(head):
        if marker != _APP2 or head[start:start + 4] != b"MPF\x00":
            continue
        base = start + 4
        mpf = head[base:stop]
        end = _byte_order(mpf)
        if end is None or len(mpf) < 8:
            return None
        try:
            ifd = struct.unpack_from(end + "I", mpf, 4)[0]
            src = _BytesSource(mpf)
            best: Optional[Tuple[int, int]] = None
            for tag, _typ, count, value in _ifd_entries(src, end, ifd):
                if tag != _TAG_MP_ENTRY:
                    continue
                for i in range(count // 16):
                    entry = src.read(value + i * 16, 16)
                    if len(entry) < 16:
                        break
                    _attr, size, data_off = struct.unpack_from(end + "III", entry, 0)
                    # data_off == 0 marks the primary image (the whole file).
                    if data_off and size and (best is None or size > best[1]):
                        best = (base + data_off, size)
            return best
        except (struct.error, IndexError):
            return None
    return None


def _exif_thumbnail_span(head: bytes) -> Optional[Tuple[int, int]]:
    """Return (absolute_offset, length) of the EXIF IFD1 thumbnail, if present."""
    found = _exif_tiff(head)
    if found is None:
        return None
    tiff, end, ifd0, base = found
    src = _BytesSource(tiff)
    ifd1 = _next_ifd(src, end, ifd0)
    if not ifd1:
        return None
    offset = length = 0
    for tag, _typ, _count, value in _ifd_entries(src, end, ifd1):
        if tag == _TAG_THUMB_OFFSET:
            offset = value
        elif tag == _TAG_THUMB_LENGTH:
            length = value
    return (base + offset, length) if offset and length else None


def jpeg_preview_bytes(path: Path, min_edge: int = 0) -> Optional[bytes]:
    """
    Return the bytes of the largest embedded JPEG preview in *path*.

    Prefers the MPF (APP2) preview — typically 1616×1080 — and falls back to
    the EXIF IFD1 thumbnail.  Returns None when the file carries no usable
    preview, in which case the caller should decode the primary image.

    *min_edge* is a cheap pre-filter for callers that need a minimum
    resolution.  The payload is not decoded here, so the test is a
    conservative estimate from the byte length: a JPEG encoding an N×N frame
    is never much smaller than N²/64 bytes at photographic quality.  Callers
    that care should still check the decoded header.  Pass 0 to skip it.
    """
    try:
        with open(path, "rb") as fh:
            head = fh.read(_HEADER_BYTES)
            if head[:2] != _SOI:
                return None

            span = _mpf_preview_span(head) or _exif_thumbnail_span(head)
            if span is None:
                return None
            offset, length = span
            if min_edge and length < (min_edge * min_edge) // 64:
                return None

            if offset + length <= len(head):
                return head[offset:offset + length]
            fh.seek(offset)
            data = fh.read(length)
            return data if len(data) == length else None
    except (OSError, struct.error, IndexError):
        return None
