//! What an analysis measured, remembered per folder, so analysing a folder
//! again (or reopening it) reads nothing from files that haven't changed.
//!
//! Saved to `~/.cache/photo_pipeline/quality/<md5 of the folder>.bin` on
//! `binfile`.  Per file: its size and mtime when measured, the
//! measurements (shot representatives only), and the digests the duplicate
//! check took (only files that shared a type and size with another).
//!
//! **Keys survive Sort.**  A file is keyed by its folder relative to the
//! scanned one with a `RAW`/`JPG` level stripped (`util::canonical_parent`),
//! plus its name, so moving files into `RAW/` and `JPG/` keeps every
//! entry.  An entry holds only while the file's size and mtime are what they
//! were (a move keeps both).
//!
//! **Versions.**  The file format has a `VERSION`; the measurements an
//! [`ALGO`](crate::imaging::quality::ALGO) version.  A file of another
//! format is not read.  One of another `ALGO` keeps its digests, which
//! don't depend on it, and drops its measurements, which are taken again.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};

use super::binfile::{Reader, Writer};
use super::record::MTime;
use crate::imaging::quality::{self, Features, Source, TILES};
use crate::util::canonical_parent;

const MAGIC: &[u8; 8] = b"PPQUALT\0";
const VERSION: u32 = 1;

/// One file as last analysed.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub size: u64,
    /// `MTime::timestamp` bits.
    pub mtime: u64,
    pub features: Option<Features>,
    /// Digest of the file's size and first 64 KB.
    pub head: Option<u64>,
    /// Digest of 64 KB from its middle and its end, taken when the head
    /// matched another file's.
    pub samples: Option<u64>,
}

impl Entry {
    pub fn new(size: u64, mtime: &MTime) -> Entry {
        Entry { size, mtime: mtime.timestamp.to_bits(), features: None, head: None, samples: None }
    }

    /// Still describes the file, as it is now?
    pub fn holds_for(&self, size: u64, mtime: &MTime) -> bool {
        self.size == size && self.mtime == mtime.timestamp.to_bits()
    }
}

/// The key for `path` within the folder `root`: its canonical folder
/// relative to `root` (absolute when outside it), and its name.
pub fn key(root: &Path, path: &Path) -> PathBuf {
    let parent = canonical_parent(path);
    let rel = parent.strip_prefix(root).map(Path::to_path_buf).unwrap_or(parent);
    rel.join(path.file_name().unwrap_or_default())
}

/// One folder's remembered analysis.
#[derive(Debug, Default)]
pub struct QualityCache {
    pub root: PathBuf,
    pub entries: HashMap<PathBuf, Entry>,
}

impl QualityCache {
    /// The cache for `root` kept in `file` (empty when there is none or it
    /// can't be read).
    pub fn load_file(file: &Path, root: &Path) -> QualityCache {
        let entries = fs::read(file).ok().and_then(|d| decode(&d, root)).unwrap_or_default();
        QualityCache { root: root.to_path_buf(), entries }
    }

    /// The entry for `path` if it still holds for the file as it is.
    pub fn get(&self, path: &Path, size: u64, mtime: &MTime) -> Option<&Entry> {
        self.entries.get(&key(&self.root, path)).filter(|e| e.holds_for(size, mtime))
    }

    /// Write `entries` (keyed by [`key`]) as `root`'s cache in `file`.
    pub fn save_file(file: &Path, root: &Path, entries: &HashMap<PathBuf, Entry>) -> std::io::Result<()> {
        crate::ops::fsutil::ensure_parent(file)?;
        crate::ops::fsutil::atomic_write(file, &encode(root, entries, quality::ALGO), true)
    }
}

/// `~/.cache/photo_pipeline/quality/<md5 of the folder>.bin`
pub fn file_for(root: &Path) -> Option<PathBuf> {
    let mut h = Md5::new();
    h.update(root.as_os_str().as_encoded_bytes());
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(cache_dir()?.join(format!("{digest}.bin")))
}

pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cache").join("photo_pipeline").join("quality"))
}

const HAS_FEATURES: u8 = 1;
const HAS_HEAD: u8 = 2;
const HAS_SAMPLES: u8 = 4;
const HAS_FOCUS: u8 = 8;

fn f32_bits(w: &mut Writer, v: f32) {
    w.u32(v.to_bits());
}

fn read_f32(r: &mut Reader<'_>) -> Option<f32> {
    Some(f32::from_bits(r.u32()?))
}

fn write_features(w: &mut Writer, f: &Features) {
    w.u8(f.source as u8);
    w.u16(f.width);
    w.u16(f.height);
    w.u8(f.orientation);
    for &t in &f.tiles {
        w.u16(t);
    }
    w.0.extend_from_slice(&f.clipped);
    f32_bits(w, f.global);
    f32_bits(w, f.focus.unwrap_or(0.0));
    f32_bits(w, f.entropy);
    w.0.extend_from_slice(&f.levels);
    f32_bits(w, f.mean);
    w.u64(f.dhash);
    w.u64(f.phash);
}

fn read_features(r: &mut Reader<'_>, has_focus: bool) -> Option<Features> {
    let source = Source::from_u8(r.u8()?)?;
    let (width, height, orientation) = (r.u16()?, r.u16()?, r.u8()?);
    let mut tiles = [0u16; TILES];
    for t in tiles.iter_mut() {
        *t = r.u16()?;
    }
    let clipped: [u8; TILES] = r.take(TILES)?.try_into().ok()?;
    let global = read_f32(r)?;
    let focus = read_f32(r)?;
    let entropy = read_f32(r)?;
    let levels: [u8; 5] = r.take(5)?.try_into().ok()?;
    let mean = read_f32(r)?;
    let (dhash, phash) = (r.u64()?, r.u64()?);
    Some(Features {
        source,
        width,
        height,
        orientation,
        tiles,
        clipped,
        global,
        focus: has_focus.then_some(focus),
        entropy,
        levels,
        mean,
        dhash,
        phash,
    })
}

/// The fewest bytes one entry takes: key length, size, mtime, flags.
const ENTRY_MIN: usize = 4 + 8 + 8 + 1;

fn encode(root: &Path, entries: &HashMap<PathBuf, Entry>, algo: u32) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + entries.len() * 380));
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.u32(algo);
    w.path(root);
    w.u32(entries.len() as u32);
    // Sorted, so an unchanged analysis writes an unchanged file.
    let mut keys: Vec<&PathBuf> = entries.keys().collect();
    keys.sort();
    for k in keys {
        let e = &entries[k];
        w.path(k);
        w.u64(e.size);
        w.u64(e.mtime);
        let focus = e.features.as_ref().is_some_and(|f| f.focus.is_some());
        let flags = e.features.as_ref().map_or(0, |_| HAS_FEATURES)
            | e.head.map_or(0, |_| HAS_HEAD)
            | e.samples.map_or(0, |_| HAS_SAMPLES)
            | if focus { HAS_FOCUS } else { 0 };
        w.u8(flags);
        if let Some(f) = &e.features {
            write_features(&mut w, f);
        }
        if let Some(h) = e.head {
            w.u64(h);
        }
        if let Some(c) = e.samples {
            w.u64(c);
        }
    }
    w.seal()
}

fn decode(data: &[u8], root: &Path) -> Option<HashMap<PathBuf, Entry>> {
    let mut r = Reader::open(data)?;
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let same_algo = r.u32()? == quality::ALGO;
    if r.path()? != root {
        return None;
    }
    let n = r.count(ENTRY_MIN)?;
    let mut out = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = r.path()?;
        let (size, mtime, flags) = (r.u64()?, r.u64()?, r.u8()?);
        let features = if flags & HAS_FEATURES != 0 { Some(read_features(&mut r, flags & HAS_FOCUS != 0)?) } else { None };
        let head = if flags & HAS_HEAD != 0 { Some(r.u64()?) } else { None };
        let samples = if flags & HAS_SAMPLES != 0 { Some(r.u64()?) } else { None };
        // Measurements of another version are taken again; digests hold.
        let features = features.filter(|_| same_algo);
        out.insert(k, Entry { size, mtime, features, head, samples });
    }
    r.done().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::quality::tests::{feats, texture};

    fn sample() -> HashMap<PathBuf, Entry> {
        let m = MTime::from_unix(1_741_789_800, 250_000_000);
        let mut f = feats(&texture(96, 64, 2));
        f.focus = Some(12.5);
        let mut g = feats(&texture(64, 96, 3));
        g.source = Source::RawPreview;
        g.orientation = 6;
        let mut out = HashMap::new();
        out.insert(PathBuf::from("a.JPG"), Entry { features: Some(f), ..Entry::new(10, &m) });
        out.insert(PathBuf::from("sub/b.ARW"), Entry { features: Some(g), head: Some(7), samples: Some(9), ..Entry::new(20, &m) });
        out.insert(PathBuf::from("b.JPG"), Entry { head: Some(u64::MAX), ..Entry::new(30, &m) });
        out
    }

    #[test]
    fn round_trips_and_rejects_damage() {
        let root = Path::new("/shoot");
        let entries = sample();
        let data = encode(root, &entries, quality::ALGO);
        assert_eq!(decode(&data, root), Some(entries.clone()));
        assert_eq!(decode(&data, Path::new("/other")), None);
        for i in [0, 9, 30, data.len() / 2, data.len() - 9] {
            let mut bad = data.clone();
            bad[i] ^= 0x10;
            assert_eq!(decode(&bad, root), None, "byte {i}");
        }
        assert_eq!(decode(&data[..data.len() - 1], root), None);
        // Writing the same thing twice gives the same bytes.
        assert_eq!(encode(root, &entries, quality::ALGO), data);
    }

    #[test]
    fn another_algorithm_keeps_digests_and_drops_measurements() {
        let root = Path::new("/shoot");
        let entries = sample();
        let data = encode(root, &entries, quality::ALGO + 1);
        let back = decode(&data, root).expect("a sound file");
        assert!(back.values().all(|e| e.features.is_none()));
        assert_eq!(back[Path::new("sub/b.ARW")].samples, Some(9));
        assert_eq!(back[Path::new("b.JPG")].head, Some(u64::MAX));
        // Another file format is not read at all.
        let mut v2 = data[..data.len() - 8].to_vec();
        v2[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
        let sum = crate::model::binfile::fnv1a(&v2);
        v2.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&v2, root), None);
    }

    #[test]
    fn keys_survive_a_sort() {
        let root = Path::new("/shoot");
        assert_eq!(key(root, Path::new("/shoot/a.JPG")), Path::new("a.JPG"));
        assert_eq!(key(root, Path::new("/shoot/JPG/a.JPG")), Path::new("a.JPG"));
        assert_eq!(key(root, Path::new("/shoot/raw/a.ARW")), Path::new("a.ARW"));
        assert_eq!(key(root, Path::new("/shoot/day 2/RAW/a.ARW")), Path::new("day 2/a.ARW"));
        assert_eq!(key(root, Path::new("/shoot/day 2/a.ARW")), Path::new("day 2/a.ARW"));
        assert_eq!(key(root, Path::new("/elsewhere/a.ARW")), Path::new("/elsewhere/a.ARW"));
        // A cached entry is found after the move, while size and mtime hold.
        let m = MTime::from_unix(1_741_789_800, 0);
        let cache = QualityCache { root: root.into(), entries: [(PathBuf::from("a.JPG"), Entry::new(10, &m))].into() };
        assert!(cache.get(Path::new("/shoot/JPG/a.JPG"), 10, &m).is_some());
        assert!(cache.get(Path::new("/shoot/JPG/a.JPG"), 11, &m).is_none());
        assert!(cache.get(Path::new("/shoot/JPG/a.JPG"), 10, &MTime::from_unix(1_741_789_801, 0)).is_none());
    }
}
