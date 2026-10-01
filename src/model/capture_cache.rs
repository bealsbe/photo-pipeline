//! Capture times remembered per scanned folder.  Reading a capture time
//! means opening the file and reading its header: on a network share about a
//! millisecond and a half a file even with sixteen reads in flight, so a
//! 5 000-file shoot took seconds to rescan, and every launch rescans the last
//! folder.  An entry is used only while the file's size and mtime are what
//! they were when it was read; any other file is read as before.
//!
//! Saved to `~/.cache/photo_pipeline/capture-times/<md5 of the folder>.bin`
//! after each complete scan, holding exactly the files that scan found.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use md5::{Digest, Md5};

use super::binfile::{Reader, Writer};
use super::record::MTime;

const MAGIC: &[u8; 8] = b"PPCAPTM\0";
const VERSION: u32 = 1;

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

/// One folder's remembered capture times, and what this scan saw.
pub struct CaptureCache {
    root: PathBuf,
    known: HashMap<PathBuf, TimeEntry>,
    seen: HashMap<PathBuf, TimeEntry>,
}

impl CaptureCache {
    /// The cache for `root` (empty if there is none or it can't be read).
    pub fn load(root: &Path) -> Self {
        let known = file_for(root).and_then(|f| fs::read(f).ok()).and_then(|d| decode(&d, root)).unwrap_or_default();
        Self { root: root.to_path_buf(), known, seen: HashMap::new() }
    }

    /// A remembered capture time for this file as it is now.
    pub fn get(&self, path: &Path, size: u64, mtime: &MTime) -> Option<Option<NaiveDateTime>> {
        self.known.get(path)?.valid_for(size, mtime)
    }

    /// What the scan found for one file (remembered or freshly read).
    pub fn note(&mut self, path: PathBuf, entry: TimeEntry) {
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

fn encode(root: &Path, times: &HashMap<PathBuf, TimeEntry>) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + times.len() * 120));
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.path(root);
    write_times(&mut w, times);
    w.seal()
}

fn decode(data: &[u8], root: &Path) -> Option<HashMap<PathBuf, TimeEntry>> {
    let mut r = Reader::open(data)?;
    if r.take(8)? != MAGIC || r.u32()? != VERSION || r.path()? != root {
        return None;
    }
    let times = read_times(&mut r)?;
    r.done().then_some(times)
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

    #[test]
    fn round_trips_and_rejects_damage() {
        let root = Path::new("/shoot");
        let mut times = HashMap::new();
        let m = MTime::from_unix(1_741_789_800, 0);
        times.insert(PathBuf::from("/shoot/a.JPG"), TimeEntry::new(5, &m, NaiveDateTime::parse_from_str("2025-03-12 14:30:05", "%Y-%m-%d %H:%M:%S").ok()));
        times.insert(PathBuf::from("/shoot/b.ARW"), TimeEntry::new(7, &m, None));
        let data = encode(root, &times);
        assert_eq!(decode(&data, root), Some(times));
        assert_eq!(decode(&data, Path::new("/other")), None);
        let mut bad = data.clone();
        bad[12] ^= 1;
        assert_eq!(decode(&bad, root), None);
        assert_eq!(decode(&data[..data.len() - 1], root), None);
    }
}
