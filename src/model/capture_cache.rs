//! Capture times remembered per scanned folder.  Reading a capture time
//! means opening the file and reading its header: on a network share about a
//! millisecond and a half a file even with sixteen reads in flight, so a
//! 5 000-file shoot took seconds to rescan, and every launch rescans the last
//! folder.  An entry is used only while the file's size and mtime are what
//! they were when it was read; any other file is read as before.
//!
//! Each entry also keeps the shot metadata read from the same header (the
//! sub-second time, the press sequence number and the AF point), which is
//! what finds bursts.
//!
//! Saved to `~/.cache/photo_pipeline/capture-times/<md5 of the folder>.bin`
//! after each complete scan, holding exactly the files that scan found.
//! A file written by an older version is not read (its entries lack the
//! shot metadata), so the next scan reads every header once and rewrites it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use md5::{Digest, Md5};

use super::binfile::{Reader, Writer};
use super::record::{MTime, ShotMeta};

const MAGIC: &[u8; 8] = b"PPCAPTM\0";
/// 2: entries carry [`ShotMeta`].
const VERSION: u32 = 2;

/// A capture time as read, with the size and mtime the file had then.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeEntry {
    pub size: u64,
    /// `MTime::timestamp` bits.
    pub mtime: u64,
    pub time: Option<NaiveDateTime>,
}

impl TimeEntry {
    pub fn new(size: u64, mtime: &MTime, time: Option<NaiveDateTime>) -> Self {
        Self { size, mtime: mtime.timestamp.to_bits(), time }
    }

    /// The capture time, if the file is still as it was when read.
    pub fn valid_for(&self, size: u64, mtime: &MTime) -> Option<Option<NaiveDateTime>> {
        (self.size == size && self.mtime == mtime.timestamp.to_bits()).then_some(self.time)
    }
}

pub fn write_times(w: &mut Writer, times: &HashMap<PathBuf, TimeEntry>) {
    w.u32(times.len() as u32);
    for (path, e) in times {
        w.path(path);
        w.u64(e.size);
        w.u64(e.mtime);
        w.time(e.time);
    }
}

pub fn read_times(r: &mut Reader<'_>) -> Option<HashMap<PathBuf, TimeEntry>> {
    let n = r.count(21)?;
    let mut times = HashMap::with_capacity(n);
    for _ in 0..n {
        let path = r.path()?;
        let (size, mtime, time) = (r.u64()?, r.u64()?, r.time()?);
        times.insert(path, TimeEntry { size, mtime, time });
    }
    Some(times)
}

/// A capture time and the shot metadata read with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShotEntry {
    pub time: TimeEntry,
    pub meta: ShotMeta,
}

impl ShotEntry {
    /// What was read, if the file is still as it was when read.
    pub fn valid_for(&self, size: u64, mtime: &MTime) -> Option<(Option<NaiveDateTime>, ShotMeta)> {
        Some((self.time.valid_for(size, mtime)?, self.meta))
    }
}

/// Bit flags for the optional [`ShotMeta`] fields.
const HAS_SUBSEC: u8 = 1;
const HAS_SEQUENCE: u8 = 2;
const HAS_FOCUS: u8 = 4;

fn write_meta(w: &mut Writer, m: &ShotMeta) {
    let flags = m.subsec_ms.map_or(0, |_| HAS_SUBSEC) | m.sequence.map_or(0, |_| HAS_SEQUENCE) | m.focus.map_or(0, |_| HAS_FOCUS);
    w.u8(flags);
    w.u16(m.subsec_ms.unwrap_or(0));
    w.u16(m.sequence.unwrap_or(0));
    let [fx, fy] = m.focus.unwrap_or([0, 0]);
    w.u16(fx);
    w.u16(fy);
    w.u32(m.camera);
}

fn read_meta(r: &mut Reader<'_>) -> Option<ShotMeta> {
    let flags = r.u8()?;
    let (subsec, sequence, fx, fy, camera) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?, r.u32()?);
    Some(ShotMeta {
        subsec_ms: (flags & HAS_SUBSEC != 0).then_some(subsec),
        sequence: (flags & HAS_SEQUENCE != 0).then_some(sequence),
        focus: (flags & HAS_FOCUS != 0).then_some([fx, fy]),
        camera,
    })
}

/// The fewest bytes one entry takes: path length, size, mtime, time flag,
/// metadata.
const ENTRY_MIN: usize = 4 + 8 + 8 + 1 + 13;

fn write_shots(w: &mut Writer, shots: &HashMap<PathBuf, ShotEntry>) {
    w.u32(shots.len() as u32);
    for (path, e) in shots {
        w.path(path);
        w.u64(e.time.size);
        w.u64(e.time.mtime);
        w.time(e.time.time);
        write_meta(w, &e.meta);
    }
}

fn read_shots(r: &mut Reader<'_>) -> Option<HashMap<PathBuf, ShotEntry>> {
    let n = r.count(ENTRY_MIN)?;
    let mut shots = HashMap::with_capacity(n);
    for _ in 0..n {
        let path = r.path()?;
        let (size, mtime, time) = (r.u64()?, r.u64()?, r.time()?);
        let meta = read_meta(r)?;
        shots.insert(path, ShotEntry { time: TimeEntry { size, mtime, time }, meta });
    }
    Some(shots)
}

/// One folder's remembered capture times, and what this scan saw.
pub struct CaptureCache {
    root: PathBuf,
    known: HashMap<PathBuf, ShotEntry>,
    seen: HashMap<PathBuf, ShotEntry>,
}

impl CaptureCache {
    /// The cache for `root` (empty if there is none or it can't be read).
    pub fn load(root: &Path) -> Self {
        let known = file_for(root).and_then(|f| fs::read(f).ok()).and_then(|d| decode(&d, root)).unwrap_or_default();
        Self { root: root.to_path_buf(), known, seen: HashMap::new() }
    }

    /// A remembered capture time and shot metadata for this file as it is
    /// now.
    pub fn get(&self, path: &Path, size: u64, mtime: &MTime) -> Option<(Option<NaiveDateTime>, ShotMeta)> {
        self.known.get(path)?.valid_for(size, mtime)
    }

    /// What the scan found for one file (remembered or freshly read).
    pub fn note(&mut self, path: PathBuf, entry: ShotEntry) {
        self.seen.insert(path, entry);
    }

    /// Save what this (complete) scan saw, if it differs from what was known.
    pub fn save(self) {
        if self.seen == self.known {
            return;
        }
        let Some(file) = file_for(&self.root) else { return };
        if crate::ops::fsutil::ensure_parent(&file).is_err() {
            return;
        }
        let _ = crate::ops::fsutil::atomic_write(&file, &encode(&self.root, &self.seen), true);
    }
}

fn file_for(root: &Path) -> Option<PathBuf> {
    let mut h = Md5::new();
    h.update(root.as_os_str().as_encoded_bytes());
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(dirs::home_dir()?.join(".cache").join("photo_pipeline").join("capture-times").join(format!("{digest}.bin")))
}

fn encode(root: &Path, shots: &HashMap<PathBuf, ShotEntry>) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + shots.len() * 134));
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.path(root);
    write_shots(&mut w, shots);
    w.seal()
}

fn decode(data: &[u8], root: &Path) -> Option<HashMap<PathBuf, ShotEntry>> {
    let mut r = Reader::open(data)?;
    if r.take(8)? != MAGIC || r.u32()? != VERSION || r.path()? != root {
        return None;
    }
    let shots = read_shots(&mut r)?;
    r.done().then_some(shots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_hold_only_while_the_file_is_unchanged() {
        let t = NaiveDateTime::parse_from_str("2025-03-12 14:30:05", "%Y-%m-%d %H:%M:%S").ok();
        let m = MTime::from_unix(1_741_789_800, 500_000_000);
        let e = TimeEntry::new(1000, &m, t);
        assert_eq!(e.valid_for(1000, &m), Some(t));
        assert_eq!(e.valid_for(1001, &m), None);
        assert_eq!(e.valid_for(1000, &MTime::from_unix(1_741_789_801, 0)), None);
    }

    fn sample() -> HashMap<PathBuf, ShotEntry> {
        let mut shots = HashMap::new();
        let m = MTime::from_unix(1_741_789_800, 0);
        let t = NaiveDateTime::parse_from_str("2025-03-12 14:30:05", "%Y-%m-%d %H:%M:%S").ok();
        let sony = ShotMeta { subsec_ms: Some(24), sequence: Some(3), focus: Some([38_293, 21_700]), camera: 0xdead_beef };
        shots.insert(PathBuf::from("/shoot/a.JPG"), ShotEntry { time: TimeEntry::new(5, &m, t), meta: sony });
        // An older body: whole seconds, no maker note.
        let plain = ShotMeta { camera: 7, ..ShotMeta::default() };
        shots.insert(PathBuf::from("/shoot/b.ARW"), ShotEntry { time: TimeEntry::new(7, &m, t), meta: plain });
        // Sequence 0 (a single shot) is kept apart from no sequence.
        let single = ShotMeta { subsec_ms: Some(0), sequence: Some(0), ..ShotMeta::default() };
        shots.insert(PathBuf::from("/shoot/c.JPG"), ShotEntry { time: TimeEntry::new(9, &m, None), meta: single });
        shots
    }

    #[test]
    fn round_trips_and_rejects_damage() {
        let root = Path::new("/shoot");
        let shots = sample();
        let data = encode(root, &shots);
        assert_eq!(decode(&data, root), Some(shots));
        assert_eq!(decode(&data, Path::new("/other")), None);
        let mut bad = data.clone();
        bad[12] ^= 1;
        assert_eq!(decode(&bad, root), None);
        assert_eq!(decode(&data[..data.len() - 1], root), None);
    }

    #[test]
    fn shot_metadata_holds_only_while_the_file_is_unchanged() {
        let m = MTime::from_unix(1_741_789_800, 0);
        let e = sample()[Path::new("/shoot/a.JPG")];
        assert_eq!(e.valid_for(5, &m), Some((e.time.time, e.meta)));
        assert_eq!(e.valid_for(6, &m), None);
    }

    /// A cache from before shot metadata was kept (version 1: size, mtime
    /// and time per entry) is refused rather than misread, so the scan
    /// reads those headers again.
    #[test]
    fn an_older_cache_is_read_again_not_misparsed() {
        let root = Path::new("/shoot");
        let m = MTime::from_unix(1_741_789_800, 0);
        let mut times = HashMap::new();
        for i in 0..3 {
            times.insert(PathBuf::from(format!("/shoot/{i}.JPG")), TimeEntry::new(i, &m, None));
        }
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.u32(1);
        w.path(root);
        write_times(&mut w, &times);
        let v1 = w.seal();
        assert!(Reader::open(&v1).is_some(), "a sound file");
        assert_eq!(decode(&v1, root), None);
        // Even with the version patched, its entries don't parse as v2 ones.
        let mut patched = v1[..v1.len() - 8].to_vec();
        patched[8..12].copy_from_slice(&VERSION.to_le_bytes());
        let sum = crate::model::binfile::fnv1a(&patched);
        patched.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&patched, root), None);
    }
}
