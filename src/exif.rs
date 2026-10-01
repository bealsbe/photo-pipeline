//! Container-level metadata — no pixel decode.
//!
//! The scanner needs capture times and the thumbnailer needs orientation and
//! the camera's embedded previews.  Two container shapes are handled:
//!
//! **TIFF** — ARW, CR2, NEF, DNG, RW2 … keep the IFD chain at the front of
//! the file; it is walked in place with seeks, so an 84 MB RAW costs a few
//! small reads.
//!
//! **JPEG** — metadata lives in APP segments before the scan data: EXIF in
//! APP1 and, on Sony/Canon/Fujifilm/Panasonic bodies, an MPF preview index in
//! APP2.  Segments are found within the first 256 KB.
//!
//! Files are opened with read-ahead off ([`open_for_header`]): these readers
//! touch a few kilobytes, and on a network share the kernel would otherwise
//! fetch a megabyte or more per file around each read.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use chrono::NaiveDateTime;

const HEADER_BYTES: usize = 262_144;
/// Enough for the EXIF segment of a JPEG (at most 64 KB, and first or second
/// after SOI); files that keep it further in fall back to `HEADER_BYTES`.
const CAPTURE_HEAD_JPEG: usize = 73_728;
/// A RAW's IFD0 and EXIF IFD sit at the front of the file.  One read of this
/// covers them; anything further is read in place.
const CAPTURE_HEAD_RAW: usize = 65_536;

/// Open a file to read a few scattered pieces of it.  Read-ahead is turned
/// off: on a CIFS/SMB share it turned a 64 KB header read into 1–2.5 MB on
/// the wire, which made capture-time reads bandwidth-bound.
pub fn open_for_header(path: &Path) -> io::Result<File> {
    let f = File::open(path)?;
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(&f, 0, None, rustix::fs::Advice::Random);
    Ok(f)
}

const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const SOS: u8 = 0xDA;

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132;
const TAG_THUMB_OFFSET: u16 = 0x0201;
const TAG_THUMB_LENGTH: u16 = 0x0202;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_DT_ORIGINAL: u16 = 0x9003;
const TAG_DT_DIGITIZED: u16 = 0x9004;
const TAG_MP_ENTRY: u16 = 0xB002;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    fn from_header(buf: &[u8]) -> Option<Endian> {
        match buf.get(..2)? {
            b"II" => Some(Endian::Little),
            b"MM" => Some(Endian::Big),
            _ => None,
        }
    }

    pub fn u16(self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        match self {
            Endian::Little => u16::from_le_bytes(a),
            Endian::Big => u16::from_be_bytes(a),
        }
    }

    pub fn u32(self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        match self {
            Endian::Little => u32::from_le_bytes(a),
            Endian::Big => u32::from_be_bytes(a),
        }
    }

    /// A SHORT stored in a 4-byte value field (position depends on order).
    pub fn short_value(self, value: u32) -> u16 {
        match self {
            Endian::Little => (value & 0xFFFF) as u16,
            Endian::Big => (value >> 16) as u16,
        }
    }
}

/// Random-access byte source; returns fewer bytes (or none) past the end.
pub trait Source {
    fn read_at(&mut self, offset: u64, size: usize) -> Vec<u8>;
}

pub struct BytesSource<'a>(pub &'a [u8]);

impl Source for BytesSource<'_> {
    fn read_at(&mut self, offset: u64, size: usize) -> Vec<u8> {
        let off = offset as usize;
        if off >= self.0.len() {
            return Vec::new();
        }
        let end = off.saturating_add(size).min(self.0.len());
        self.0[off..end].to_vec()
    }
}

pub struct FileSource<'a>(pub &'a mut File);

/// The first bytes of a file, read in one go, with reads past them going to
/// the file itself.
struct HeadSource<'a> {
    head: Vec<u8>,
    /// The head holds the whole file.
    whole: bool,
    file: &'a mut File,
}

impl Source for HeadSource<'_> {
    fn read_at(&mut self, offset: u64, size: usize) -> Vec<u8> {
        let end = offset.saturating_add(size as u64);
        if end <= self.head.len() as u64 {
            return self.head[offset as usize..end as usize].to_vec();
        }
        if self.whole {
            return BytesSource(&self.head).read_at(offset, size);
        }
        FileSource(self.file).read_at(offset, size)
    }
}

impl Source for FileSource<'_> {
    fn read_at(&mut self, offset: u64, size: usize) -> Vec<u8> {
        if self.0.seek(SeekFrom::Start(offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::with_capacity(size.min(1 << 20));
        let _ = self.0.by_ref().take(size as u64).read_to_end(&mut buf);
        buf
    }
}

/// One IFD entry: (tag, type, count, value-or-offset).
pub type Entry = (u16, u16, u32, u32);

pub fn ifd_entries(src: &mut dyn Source, end: Endian, offset: u32) -> Vec<Entry> {
    if offset == 0 {
        return Vec::new();
    }
    let head = src.read_at(offset as u64, 2);
    if head.len() < 2 {
        return Vec::new();
    }
    let count = end.u16(&head) as usize;
    let raw = src.read_at(offset as u64 + 2, count * 12);
    (0..count.min(raw.len() / 12))
        .map(|i| {
            let e = &raw[i * 12..i * 12 + 12];
            (end.u16(&e[0..2]), end.u16(&e[2..4]), end.u32(&e[4..8]), end.u32(&e[8..12]))
        })
        .collect()
}

pub fn next_ifd(src: &mut dyn Source, end: Endian, offset: u32) -> u32 {
    if offset == 0 {
        return 0;
    }
    let head = src.read_at(offset as u64, 2);
    if head.len() < 2 {
        return 0;
    }
    let count = end.u16(&head) as u64;
    let tail = src.read_at(offset as u64 + 2 + count * 12, 4);
    if tail.len() == 4 { end.u32(&tail) } else { 0 }
}

/// Decode an EXIF ASCII field: strip NULs, replace non-ASCII, trim.
pub fn ascii(bytes: &[u8]) -> String {
    let trimmed = {
        let mut end = bytes.len();
        while end > 0 && bytes[end - 1] == 0 {
            end -= 1;
        }
        &bytes[..end]
    };
    trimmed
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '\u{fffd}' })
        .collect::<String>()
}

/// Parse `"YYYY:MM:DD HH:MM:SS"`.
pub fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim();
    if text.chars().count() < 19 {
        return None;
    }
    let head: String = text.chars().take(19).collect();
    NaiveDateTime::parse_from_str(&head, "%Y:%m:%d %H:%M:%S").ok()
}

fn capture_time_from_tiff(src: &mut dyn Source, end: Endian, ifd0: u32) -> Option<NaiveDateTime> {
    let mut exif_ifd = 0;
    let mut fallback: Option<NaiveDateTime> = None;
    for (tag, _typ, count, value) in ifd_entries(src, end, ifd0) {
        if tag == TAG_EXIF_IFD {
            exif_ifd = value;
        } else if matches!(tag, TAG_DT_ORIGINAL | TAG_DT_DIGITIZED | TAG_DATETIME) && count > 4 {
            let text = ascii(&src.read_at(value as u64, count as usize));
            let parsed = parse_datetime(&text);
            if parsed.is_some() && tag != TAG_DATETIME {
                return parsed;
            }
            fallback = fallback.or(parsed);
        }
    }
    for (tag, _typ, count, value) in ifd_entries(src, end, exif_ifd) {
        if matches!(tag, TAG_DT_ORIGINAL | TAG_DT_DIGITIZED) && count > 4 {
            let text = ascii(&src.read_at(value as u64, count as usize));
            if let Some(p) = parse_datetime(&text) {
                return Some(p);
            }
        }
    }
    fallback
}

/// (marker, payload start, payload end) for each segment up to SOS.
pub fn iter_segments(head: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut out = Vec::new();
    if head.get(..2) != Some(&[0xFF, 0xD8]) {
        return out;
    }
    let mut i = 2;
    while i + 4 <= head.len() {
        if head[i] != 0xFF {
            break;
        }
        let marker = head[i + 1];
        if marker == SOS {
            break;
        }
        let length = u16::from_be_bytes([head[i + 2], head[i + 3]]) as usize;
        if length < 2 {
            break;
        }
        out.push((marker, i + 4, i + 2 + length));
        i += 2 + length;
    }
    out
}

/// The EXIF TIFF stream inside a JPEG header:
/// (tiff bytes, byte order, IFD0 offset, file offset of the TIFF header).
pub fn exif_tiff(head: &[u8]) -> Option<(&[u8], Endian, u32, usize)> {
    for (marker, start, stop) in iter_segments(head) {
        if marker != APP1 || head.get(start..start + 6) != Some(b"Exif\0\0") {
            continue;
        }
        let base = start + 6;
        let tiff = head.get(base..stop.min(head.len()))?;
        let end = Endian::from_header(tiff)?;
        if tiff.len() < 8 {
            return None;
        }
        return Some((tiff, end, end.u32(&tiff[4..8]), base));
    }
    None
}

/// Capture time for `path`, or `None`.  Never decodes pixels and never
/// fails loudly — malformed files simply have no time.
pub fn read_capture_time(path: &Path, is_raw: bool) -> Option<NaiveDateTime> {
    let mut f = open_for_header(path).ok()?;
    if is_raw {
        let head = read_head(&mut f, CAPTURE_HEAD_RAW)?;
        if head.len() < 8 {
            return None;
        }
        let end = Endian::from_header(&head)?;
        let ifd0 = end.u32(&head[4..8]);
        let whole = head.len() < CAPTURE_HEAD_RAW;
        return capture_time_from_tiff(&mut HeadSource { head, whole, file: &mut f }, end, ifd0);
    }
    let mut head = read_head(&mut f, CAPTURE_HEAD_JPEG)?;
    if head.len() == CAPTURE_HEAD_JPEG && !exif_settled(&head) {
        let rest = read_head(&mut f, HEADER_BYTES - CAPTURE_HEAD_JPEG).unwrap_or_default();
        head.extend_from_slice(&rest);
    }
    let (tiff, end, ifd0, _) = exif_tiff(&head)?;
    capture_time_from_tiff(&mut BytesSource(tiff), end, ifd0)
}

/// Does `head` hold the whole EXIF segment, or show there is none?  False
/// when the segment walk ran off the end of the buffer first.
fn exif_settled(head: &[u8]) -> bool {
    let mut next = 2;
    for (marker, start, stop) in iter_segments(head) {
        if marker == APP1 && head.get(start..start + 6) == Some(b"Exif\0\0") {
            return stop <= head.len();
        }
        next = stop;
    }
    // Stopped at the scan data or a malformed marker, both inside the buffer.
    next + 4 <= head.len()
}

pub fn orientation_from_jpeg_head(head: &[u8]) -> u8 {
    let Some((tiff, end, ifd0, _)) = exif_tiff(head) else { return 1 };
    for (tag, _t, _c, value) in ifd_entries(&mut BytesSource(tiff), end, ifd0) {
        if tag == TAG_ORIENTATION {
            let o = end.short_value(value);
            return if (1..=8).contains(&o) { o as u8 } else { 1 };
        }
    }
    1
}

/// Largest non-primary MPF image: (absolute offset, length).
fn mpf_preview_span(head: &[u8]) -> Option<(usize, usize)> {
    for (marker, start, stop) in iter_segments(head) {
        if marker != APP2 || head.get(start..start + 4) != Some(b"MPF\0") {
            continue;
        }
        let base = start + 4;
        let mpf = head.get(base..stop.min(head.len()))?;
        let end = Endian::from_header(mpf)?;
        if mpf.len() < 8 {
            return None;
        }
        let ifd = end.u32(&mpf[4..8]);
        let mut src = BytesSource(mpf);
        let mut best: Option<(usize, usize)> = None;
        for (tag, _typ, count, value) in ifd_entries(&mut src, end, ifd) {
            if tag != TAG_MP_ENTRY {
                continue;
            }
            for i in 0..(count / 16) {
                let entry = src.read_at(value as u64 + i as u64 * 16, 16);
                if entry.len() < 16 {
                    break;
                }
                let size = end.u32(&entry[4..8]) as usize;
                let data_off = end.u32(&entry[8..12]) as usize;
                // data_off == 0 marks the primary image (the whole file).
                if data_off != 0 && size != 0 && best.is_none_or(|b| size > b.1) {
                    best = Some((base + data_off, size));
                }
            }
        }
        return best;
    }
    None
}

/// The EXIF IFD1 thumbnail: (absolute offset, length).
fn exif_thumbnail_span(head: &[u8]) -> Option<(usize, usize)> {
    let (tiff, end, ifd0, base) = exif_tiff(head)?;
    let mut src = BytesSource(tiff);
    let ifd1 = next_ifd(&mut src, end, ifd0);
    if ifd1 == 0 {
        return None;
    }
    let (mut offset, mut length) = (0usize, 0usize);
    for (tag, _t, _c, value) in ifd_entries(&mut src, end, ifd1) {
        if tag == TAG_THUMB_OFFSET {
            offset = value as usize;
        } else if tag == TAG_THUMB_LENGTH {
            length = value as usize;
        }
    }
    (offset != 0 && length != 0).then_some((base + offset, length))
}

/// Bytes of the largest embedded JPEG preview (MPF, else the EXIF
/// thumbnail).  `min_edge` rejects previews too small to be useful, judged
/// conservatively from the byte length (an N×N frame is never much smaller
/// than N²/64 bytes); pass 0 to skip.  Also returns the header read, so the
/// caller can take the primary image's orientation from it.
pub fn jpeg_preview_bytes(path: &Path, min_edge: u32) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut f = open_for_header(path).ok()?;
    let head = read_head(&mut f, HEADER_BYTES)?;
    if head.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let (offset, length) = mpf_preview_span(&head).or_else(|| exif_thumbnail_span(&head))?;
    if min_edge > 0 && (length as u64) < (min_edge as u64 * min_edge as u64) / 64 {
        return None;
    }
    let data = if offset + length <= head.len() {
        head[offset..offset + length].to_vec()
    } else {
        let mut src = FileSource(&mut f);
        let d = src.read_at(offset as u64, length);
        if d.len() != length {
            return None;
        }
        d
    };
    Some((data, head))
}

pub fn read_head(f: &mut File, n: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    f.by_ref().take(n as u64).read_to_end(&mut buf).ok()?;
    if buf.is_empty() { None } else { Some(buf) }
}

