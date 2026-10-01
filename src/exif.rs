//! Container-level metadata — no pixel decode.
//!
//! The scanner needs capture times and the thumbnailer needs orientation and
//! the camera's embedded previews.  With the capture time the scan also
//! takes [`ShotMeta`] from the same bytes (sub-second time; on Sony bodies
//! the press sequence number and AF point from the maker note), which is
//! what finds bursts.  Two container shapes are handled:
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

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132;
const TAG_THUMB_OFFSET: u16 = 0x0201;
const TAG_THUMB_LENGTH: u16 = 0x0202;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_DT_ORIGINAL: u16 = 0x9003;
const TAG_DT_DIGITIZED: u16 = 0x9004;
const TAG_MAKER_NOTE: u16 = 0x927C;
const TAG_SUBSEC: u16 = 0x9290;
const TAG_SUBSEC_ORIGINAL: u16 = 0x9291;
const TAG_SUBSEC_DIGITIZED: u16 = 0x9292;
const TAG_MP_ENTRY: u16 = 0xB002;
/// Sony maker note: the AF point (sensor width, height, x, y).
const SONY_FOCUS_LOCATION: u16 = 0x2027;
/// Sony maker note: the frame's number within its shutter press (1 for the
/// first, 0 for a single shot, 65535 for none).
const SONY_SEQUENCE: u16 = 0xB04A;
/// A maker note with more entries than this is not one we understand.
const MAKER_NOTE_MAX_ENTRIES: usize = 512;

/// What the header says about how a frame was shot, beyond its capture
/// time: enough to find the frames of one shutter press and where the
/// camera focused.  Read with the capture time, from the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ShotMeta {
    /// Milliseconds past the capture time's second (EXIF SubSecTimeOriginal),
    /// if the camera recorded them.
    pub subsec_ms: Option<u16>,
    /// The frame's place in its shutter press (Sony SequenceNumber): 1 for
    /// the first frame of a press, 0 for a single shot.
    pub sequence: Option<u16>,
    /// The camera's AF point as fractions of the photo as it is shown
    /// (orientation applied), each scaled to 0–65535.
    pub focus: Option<[u16; 2]>,
    /// A hash of the camera's make and model (0 when unknown): frames from
    /// different bodies never share a burst.
    pub camera: u32,
}

impl ShotMeta {
    /// The AF point as fractions (0–1) of the photo as shown.
    pub fn focus_point(&self) -> Option<(f32, f32)> {
        self.focus.map(|[x, y]| (x as f32 / 65535.0, y as f32 / 65535.0))
    }
}

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

/// The bytes of an ASCII or UNDEFINED entry: in the entry itself when they
/// fit in four, else where it points.
fn entry_bytes(src: &mut dyn Source, end: Endian, count: u32, value: u32) -> Vec<u8> {
    if count <= 4 {
        let raw = match end {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        return raw[..count as usize].to_vec();
    }
    src.read_at(value as u64, count as usize)
}

/// EXIF SubSecTime text ("024", "86", "5") as milliseconds: the digits are
/// a decimal fraction of a second.
pub fn parse_subsec(bytes: &[u8]) -> Option<u16> {
    let digits: Vec<u16> = bytes.iter().skip_while(|b| **b == b' ').take_while(|b| b.is_ascii_digit()).map(|b| (b - b'0') as u16).collect();
    if digits.is_empty() {
        return None;
    }
    Some((0..3).map(|i| digits.get(i).copied().unwrap_or(0) * 10u16.pow(2 - i as u32)).sum())
}

/// A point given as fractions of the stored (sensor-oriented) image, moved
/// to where it shows once EXIF `orientation` is applied.
pub fn orient_point(x: f32, y: f32, orientation: u16) -> (f32, f32) {
    match orientation {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (1.0 - y, x),
        7 => (1.0 - y, 1.0 - x),
        8 => (y, 1.0 - x),
        _ => (x, y),
    }
}

/// Make and model, hashed to tell bodies apart (0 when neither is known).
fn camera_id(make: &str, model: &str) -> u32 {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() && model.is_empty() {
        return 0;
    }
    // FNV-1a: stable across runs, so it can be cached.
    let mut h: u32 = 0x811c_9dc5;
    for &b in make.as_bytes().iter().chain(b"\0").chain(model.as_bytes()) {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h.max(1)
}

/// The capture time, and (with `want_meta`) the shot metadata: sub-second
/// time, and for Sony bodies the press sequence number and AF point from
/// the maker note.  Without `want_meta` this reads exactly what the capture
/// time alone needs.
fn shot_from_tiff(src: &mut dyn Source, end: Endian, ifd0: u32, want_meta: bool) -> (Option<NaiveDateTime>, ShotMeta) {
    let mut meta = ShotMeta::default();
    let mut exif_ifd = 0;
    let mut primary: Option<NaiveDateTime> = None;
    let mut fallback: Option<NaiveDateTime> = None;
    let (mut make, mut model, mut orientation) = (String::new(), String::new(), 1u16);
    for (tag, typ, count, value) in ifd_entries(src, end, ifd0) {
        match tag {
            TAG_EXIF_IFD => exif_ifd = value,
            TAG_DT_ORIGINAL | TAG_DT_DIGITIZED | TAG_DATETIME if count > 4 && primary.is_none() => {
                let text = ascii(&src.read_at(value as u64, count as usize));
                let parsed = parse_datetime(&text);
                if parsed.is_some() && tag != TAG_DATETIME {
                    if !want_meta {
                        return (parsed, meta);
                    }
                    primary = parsed;
                }
                fallback = fallback.or(parsed);
            }
            TAG_MAKE if want_meta && typ == 2 => make = ascii(&entry_bytes(src, end, count.min(64), value)),
            TAG_MODEL if want_meta && typ == 2 => model = ascii(&entry_bytes(src, end, count.min(64), value)),
            TAG_ORIENTATION => orientation = end.short_value(value),
            _ => {}
        }
    }
    let mut exif_time: Option<NaiveDateTime> = None;
    let mut subsec: [Option<u16>; 3] = [None; 3];
    let mut maker_note: Option<(u32, u32)> = None;
    for (tag, _typ, count, value) in ifd_entries(src, end, exif_ifd) {
        match tag {
            TAG_DT_ORIGINAL | TAG_DT_DIGITIZED if count > 4 && exif_time.is_none() => {
                let text = ascii(&src.read_at(value as u64, count as usize));
                exif_time = parse_datetime(&text);
                if exif_time.is_some() && !want_meta {
                    return (exif_time, meta);
                }
            }
            TAG_SUBSEC_ORIGINAL | TAG_SUBSEC_DIGITIZED | TAG_SUBSEC if want_meta && count > 0 => {
                let slot = match tag {
                    TAG_SUBSEC_ORIGINAL => 0,
                    TAG_SUBSEC_DIGITIZED => 1,
                    _ => 2,
                };
                subsec[slot] = parse_subsec(&entry_bytes(src, end, count.min(16), value));
            }
            TAG_MAKER_NOTE if want_meta && count > 12 => maker_note = Some((value, count)),
            _ => {}
        }
    }
    let time = primary.or(exif_time).or(fallback);
    if !want_meta {
        return (time, meta);
    }
    meta.subsec_ms = subsec.iter().flatten().next().copied();
    meta.camera = camera_id(&make, &model);
    if let Some((offset, _len)) = maker_note {
        read_sony_maker_note(src, end, offset, &make, orientation, &mut meta);
    }
    (time, meta)
}

/// Sony's maker note is a plain IFD (offsets from the TIFF header), after a
/// "SONY DSC " header in JPEGs and with none in ARWs.  Reads the press
/// sequence number and the AF point; anything else is left alone.
fn read_sony_maker_note(src: &mut dyn Source, end: Endian, offset: u32, make: &str, orientation: u16, meta: &mut ShotMeta) {
    let head = src.read_at(offset as u64, 12);
    let ifd = if head.starts_with(b"SONY DSC ") || head.starts_with(b"SONY CAM ") || head.starts_with(b"SONY MOBILE") {
        offset + 12
    } else if make.trim().eq_ignore_ascii_case("SONY") {
        offset
    } else {
        return;
    };
    let count = src.read_at(ifd as u64, 2);
    if count.len() < 2 || end.u16(&count) as usize > MAKER_NOTE_MAX_ENTRIES {
        return;
    }
    for (tag, typ, count, value) in ifd_entries(src, end, ifd) {
        match (tag, typ, count) {
            (SONY_SEQUENCE, 3, 1) => meta.sequence = Some(end.short_value(value)).filter(|&s| s != u16::MAX),
            (SONY_SEQUENCE, 4, 1) => meta.sequence = u16::try_from(value).ok().filter(|&s| s != u16::MAX),
            (SONY_FOCUS_LOCATION, 3, 4) => {
                let b = src.read_at(value as u64, 8);
                if b.len() < 8 {
                    continue;
                }
                let [w, h, x, y] = [0, 2, 4, 6].map(|i| end.u16(&b[i..]) as f32);
                if w <= 0.0 || h <= 0.0 || x > w || y > h || (x == 0.0 && y == 0.0) {
                    continue;
                }
                let (fx, fy) = orient_point(x / w, y / h, orientation);
                meta.focus = Some([(fx * 65535.0).round() as u16, (fy * 65535.0).round() as u16]);
            }
            _ => {}
        }
    }
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
    read_header(path, is_raw, false).0
}

/// Capture time and shot metadata for `path`, from the same header bytes
/// the capture time alone needs (the maker note is inside them: a JPEG's
/// EXIF segment, a RAW's first 64 KB).
pub fn read_shot(path: &Path, is_raw: bool) -> (Option<NaiveDateTime>, ShotMeta) {
    read_header(path, is_raw, true)
}

fn read_header(path: &Path, is_raw: bool, want_meta: bool) -> (Option<NaiveDateTime>, ShotMeta) {
    let none = (None, ShotMeta::default());
    let Ok(mut f) = open_for_header(path) else { return none };
    if is_raw {
        let Some(head) = read_head(&mut f, CAPTURE_HEAD_RAW) else { return none };
        return shot_from_raw_head(head, &mut f, want_meta);
    }
    let Some(mut head) = read_head(&mut f, CAPTURE_HEAD_JPEG) else { return none };
    if head.len() == CAPTURE_HEAD_JPEG && !exif_settled(&head) {
        let rest = read_head(&mut f, HEADER_BYTES - CAPTURE_HEAD_JPEG).unwrap_or_default();
        head.extend_from_slice(&rest);
    }
    shot_from_jpeg_head(&head, want_meta)
}

/// From a RAW's first bytes (`file` for anything past them).
fn shot_from_raw_head(head: Vec<u8>, file: &mut File, want_meta: bool) -> (Option<NaiveDateTime>, ShotMeta) {
    let Some(end) = Endian::from_header(&head).filter(|_| head.len() >= 8) else { return (None, ShotMeta::default()) };
    let ifd0 = end.u32(&head[4..8]);
    let whole = head.len() < CAPTURE_HEAD_RAW;
    shot_from_tiff(&mut HeadSource { head, whole, file }, end, ifd0, want_meta)
}

/// From a JPEG's header bytes.
fn shot_from_jpeg_head(head: &[u8], want_meta: bool) -> (Option<NaiveDateTime>, ShotMeta) {
    match exif_tiff(head) {
        Some((tiff, end, ifd0, _)) => shot_from_tiff(&mut BytesSource(tiff), end, ifd0, want_meta),
        None => (None, ShotMeta::default()),
    }
}

/// From a TIFF-based RAW's bytes held in memory.
#[cfg(test)]
pub fn shot_from_tiff_bytes(data: &[u8]) -> (Option<NaiveDateTime>, ShotMeta) {
    let Some(end) = Endian::from_header(data).filter(|_| data.len() >= 8) else { return (None, ShotMeta::default()) };
    shot_from_tiff(&mut BytesSource(data), end, end.u32(&data[4..8]), true)
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

#[cfg(test)]
mod tests {
    use super::*;

    // The first bytes of real files (all the header the parser touches):
    // an A7 V burst's second frame (JPEG, "SONY DSC " maker note) and first
    // frame (ARW, maker note with no header), an A7 V portrait frame, an
    // A7R II portrait ARW (whole-second times), and a Canon T6 CR2.
    const A7V_JPG: &[u8] = include_bytes!("../testdata/headers/a7v-burst-frame2.jpg.head");
    const A7V_PORTRAIT: &[u8] = include_bytes!("../testdata/headers/a7v-portrait.jpg.head");
    const A7V_ARW: &[u8] = include_bytes!("../testdata/headers/a7v-burst-frame1.arw.head");
    const A7RM2_ARW: &[u8] = include_bytes!("../testdata/headers/a7rm2-portrait.arw.head");
    const T6_CR2: &[u8] = include_bytes!("../testdata/headers/t6.cr2.head");

    fn time(s: &str) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
    }

    fn focus(m: &ShotMeta) -> (f32, f32) {
        let (x, y) = m.focus_point().expect("a focus point");
        ((x * 10_000.0).round() / 10_000.0, (y * 10_000.0).round() / 10_000.0)
    }

    #[test]
    fn sony_jpeg_gives_subseconds_sequence_and_af_point() {
        let (t, m) = shot_from_jpeg_head(A7V_JPG, true);
        assert_eq!(t, time("2026-07-24 01:08:10"));
        assert_eq!((m.subsec_ms, m.sequence), (Some(981), Some(2)));
        // FocusLocation 7008 4672 3887 2180 (exiftool), landscape.
        assert_eq!(focus(&m), (0.5547, 0.4666));
        assert_ne!(m.camera, 0);
        // The capture time alone reads the same.
        assert_eq!(shot_from_jpeg_head(A7V_JPG, false), (t, ShotMeta::default()));
    }

    #[test]
    fn the_af_point_turns_with_the_photo() {
        // Orientation 8 (rotate 270 CW); FocusLocation 7008 4672 4686 1547
        // is in sensor terms, so it lands at (y, 1 - x) on the upright photo.
        let (_, m) = shot_from_jpeg_head(A7V_PORTRAIT, true);
        assert_eq!((m.subsec_ms, m.sequence), (Some(381), Some(3)));
        assert_eq!(focus(&m), (0.3311, 0.3313));
        assert_eq!(orient_point(0.25, 0.75, 6), (0.25, 0.25));
        assert_eq!(orient_point(0.25, 0.75, 3), (0.75, 0.25));
        assert_eq!(orient_point(0.25, 0.75, 1), (0.25, 0.75));
    }

    #[test]
    fn sony_arw_maker_note_has_no_header() {
        let (t, m) = shot_from_tiff_bytes(A7V_ARW);
        assert_eq!(t, time("2026-07-24 01:08:10"));
        assert_eq!((m.subsec_ms, m.sequence), (Some(814), Some(1)));
        assert_eq!(focus(&m), (0.5547, 0.4666));
        // Same body as the JPEG: frames of one camera group together.
        assert_eq!(m.camera, shot_from_jpeg_head(A7V_JPG, true).1.camera);
    }

    #[test]
    fn older_bodies_give_what_they_have() {
        // A7R II: no sub-seconds, but the sequence number, and an AF point
        // at the centre (Wide area).
        let (t, m) = shot_from_tiff_bytes(A7RM2_ARW);
        assert_eq!(t, time("2025-07-05 15:08:10"));
        assert_eq!((m.subsec_ms, m.sequence), (None, Some(2)));
        assert_eq!(focus(&m), (0.5, 0.5));
        // Canon T6: sub-seconds ("86" is 0.86 s), no Sony maker note.
        let (t, m) = shot_from_tiff_bytes(T6_CR2);
        assert_eq!(t, time("2018-09-29 23:11:19"));
        assert_eq!((m.subsec_ms, m.sequence, m.focus), (Some(860), None, None));
        assert_ne!(m.camera, shot_from_tiff_bytes(A7RM2_ARW).1.camera);
    }

    #[test]
    fn truncated_or_damaged_headers_lose_metadata_not_the_time() {
        // Cut inside the maker note: the time and sub-seconds are before it.
        let (t, m) = shot_from_tiff_bytes(&A7V_ARW[..6000]);
        assert_eq!(t, time("2026-07-24 01:08:10"));
        assert_eq!((m.subsec_ms, m.sequence, m.focus), (Some(814), None, None));
        // Every truncation parses without panicking.
        for n in (0..A7V_JPG.len()).step_by(7) {
            let _ = shot_from_jpeg_head(&A7V_JPG[..n], true);
        }
        for n in (0..A7V_ARW.len()).step_by(7) {
            let _ = shot_from_tiff_bytes(&A7V_ARW[..n]);
        }
        // Random damage anywhere: no panic, and never a wild value.
        let mut seed: u32 = 0x1234_5678;
        for _ in 0..3000 {
            let mut bad = A7V_ARW.to_vec();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % bad.len();
                bad[i] = (seed >> 8) as u8;
            }
            let (_, m) = shot_from_tiff_bytes(&bad);
            assert!(m.subsec_ms.is_none_or(|ms| ms < 1000));
        }
        // Not a Sony: a maker note isn't read as one.
        assert_eq!(shot_from_tiff_bytes(T6_CR2).1.sequence, None);
    }

    #[test]
    fn subseconds_are_a_decimal_fraction() {
        assert_eq!(parse_subsec(b"024\0"), Some(24));
        assert_eq!(parse_subsec(b"86"), Some(860));
        assert_eq!(parse_subsec(b"5"), Some(500));
        assert_eq!(parse_subsec(b"123456"), Some(123));
        assert_eq!(parse_subsec(b" 7"), Some(700));
        assert_eq!(parse_subsec(b""), None);
        assert_eq!(parse_subsec(b"  \0"), None);
    }
}

