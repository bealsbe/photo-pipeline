//! The search index on disk and in memory (plan §6).
//!
//! **Keys.**  A file's [`Key`] is a 128-bit digest of its size and first
//! 64 KB: the EXIF with the capture time, sub-seconds and the maker note's
//! frame counters live there, so different frames never share a key, and
//! Sort, export (copy or move, renamed or not) and copies to other disks
//! keep it.  A cropped JPG re-encoded by a Move export is new bytes and is
//! indexed again.
//!
//! **Files**, under `~/.cache/photo_pipeline/search/<digest of the
//! library root>/`:
//! - `paths.bin`: the path map, (library-relative path, size, mtime) →
//!   key, written whole with `fsutil::atomic_write`, so a check after an
//!   export reads nothing for files that haven't changed;
//! - `vectors-<model>.bin`: a sealed header block (magic, version, model
//!   id, weights SHA-256, preprocessing version, dimension), then sealed
//!   blocks of rows (key, capture info, flags, f16 vector) appended as
//!   batches arrive.  A torn last block is dropped.  Rows whose key no
//!   path maps to any more are dropped when the file is compacted (at
//!   most once a session, on loading).  A file of another model or
//!   preprocessing is not read: the index is built again.
//!
//! Nothing here touches the library.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::query::{f16_to_f32, f32_to_f16};
use crate::ml::models::{self, ModelId, ModelInfo};
use crate::model::binfile::{Reader, Writer};
use crate::model::record::{FileType, MTime, ShotMeta};
use crate::ops::fsutil;

/// Bytes of a file that go into its key, with its size.
pub const KEY_HEAD_BYTES: usize = 64 * 1024;

const PATHS_MAGIC: &[u8; 8] = b"PPSRCHP\0";
const PATHS_VERSION: u32 = 1;
const VECTORS_MAGIC: &[u8; 8] = b"PPSRCHV\0";
const VECTORS_VERSION: u32 = 1;
const FLAG_FIRST_ASKED: u8 = 1;
const ROW_ACCIDENTAL: u8 = 1;
const NONE_U16: u16 = u16::MAX;
/// Dropped rows beyond this share of the file get it compacted on load.
const COMPACT_SHARE: f64 = 0.1;

/// A content key: survives renames, moves and copies.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Key(pub [u8; 16]);

impl Key {
    /// The key of a file whose size and first bytes are given (`head` may
    /// be longer than [`KEY_HEAD_BYTES`]; only those count, and a shorter
    /// file counts whole).
    pub fn of(size: u64, head: &[u8]) -> Key {
        let mut h = Md5::new();
        h.update(size.to_le_bytes());
        h.update(&head[..head.len().min(KEY_HEAD_BYTES)]);
        Key(h.finalize().into())
    }

    /// Read a file's head and compute its key (one 64 KB read with
    /// read-ahead off).
    pub fn read(path: &Path) -> io::Result<Key> {
        use std::io::Read;
        let mut f = crate::exif::open_for_header(path)?;
        let size = f.metadata()?.len();
        let mut buf = Vec::with_capacity(KEY_HEAD_BYTES);
        Read::by_ref(&mut f).take(KEY_HEAD_BYTES as u64).read_to_end(&mut buf)?;
        Ok(Key::of(size, &buf))
    }

    /// The first 64 bits, for a session-stable `u64` id.
    pub fn short(&self) -> u64 {
        u64::from_le_bytes(self.0[..8].try_into().expect("16 bytes"))
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({self})")
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// What the index knows about one shot besides its vector.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Record {
    pub key: Key,
    pub capture: Option<NaiveDateTime>,
    pub meta: ShotMeta,
    /// Looks accidental: kept, never a result.
    pub accidental: bool,
}

/// One file the path map knows.
#[derive(Debug, Clone, PartialEq)]
pub struct PathEntry {
    pub file_type: FileType,
    pub size: u64,
    /// `MTime::timestamp` bits.
    pub mtime: u64,
    pub key: Key,
}

impl PathEntry {
    pub fn new(file_type: FileType, size: u64, mtime: &MTime, key: Key) -> PathEntry {
        PathEntry { file_type, size, mtime: mtime.timestamp.to_bits(), key }
    }

    pub fn holds_for(&self, size: u64, mtime: &MTime) -> bool {
        self.size == size && self.mtime == mtime.timestamp.to_bits()
    }
}

/// The path map: files of the library (relative paths) → keys, as last
/// seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathMap {
    pub root: PathBuf,
    /// The first index was asked for (so new exports are indexed on their
    /// own from then on).
    pub first_index_asked: bool,
    pub entries: HashMap<PathBuf, PathEntry>,
}

impl PathMap {
    /// The key for `rel` if the map holds it unchanged.
    pub fn key_for(&self, rel: &Path, size: u64, mtime: &MTime) -> Option<Key> {
        self.entries.get(rel).filter(|e| e.holds_for(size, mtime)).map(|e| e.key)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer(Vec::with_capacity(64 + self.entries.len() * 80));
        w.0.extend_from_slice(PATHS_MAGIC);
        w.u32(PATHS_VERSION);
        w.path(&self.root);
        w.u8(if self.first_index_asked { FLAG_FIRST_ASKED } else { 0 });
        w.u32(self.entries.len() as u32);
        let mut keys: Vec<&PathBuf> = self.entries.keys().collect();
        keys.sort();
        for k in keys {
            let e = &self.entries[k];
            w.path(k);
            w.u8(e.file_type as u8);
            w.u64(e.size);
            w.u64(e.mtime);
            w.0.extend_from_slice(&e.key.0);
        }
        w.seal()
    }

    fn decode(data: &[u8], root: &Path) -> Option<PathMap> {
        let mut r = Reader::open(data)?;
        if r.take(8)? != PATHS_MAGIC || r.u32()? != PATHS_VERSION {
            return None;
        }
        if r.path()? != root {
            return None;
        }
        let flags = r.u8()?;
        let n = r.count(4 + 1 + 8 + 8 + 16)?;
        let mut entries = HashMap::with_capacity(n);
        for _ in 0..n {
            let p = r.path()?;
            let file_type = match r.u8()? {
                0 => FileType::Jpg,
                1 => FileType::Raw,
                _ => return None,
            };
            let (size, mtime) = (r.u64()?, r.u64()?);
            let key = Key(r.take(16)?.try_into().ok()?);
            entries.insert(p, PathEntry { file_type, size, mtime, key });
        }
        r.done().then_some(PathMap { root: root.to_path_buf(), first_index_asked: flags & FLAG_FIRST_ASKED != 0, entries })
    }
}

/// The vectors and records, in memory.
#[derive(Default)]
pub struct Vectors {
    pub dim: usize,
    pub records: Vec<Record>,
    /// Row index by key.
    pub by_key: HashMap<Key, usize>,
    /// `records.len() × dim`, f16 bits, row-major.
    pub data: Vec<u16>,
}

impl Vectors {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.by_key.contains_key(key)
    }

    /// Row `i` as f32.
    pub fn row(&self, i: usize) -> Vec<f32> {
        self.data[i * self.dim..(i + 1) * self.dim].iter().map(|&h| f16_to_f32(h)).collect()
    }

    /// Add rows whose keys are new; returns how many were added.  `dim`
    /// is taken from the first row ever added.
    fn push(&mut self, rows: &[(Record, Vec<f32>)]) -> usize {
        let mut added = 0;
        for (rec, v) in rows {
            if self.by_key.contains_key(&rec.key) || v.is_empty() {
                continue;
            }
            if self.dim == 0 {
                self.dim = v.len();
            }
            if v.len() != self.dim {
                continue;
            }
            self.by_key.insert(rec.key, self.records.len());
            self.records.push(rec.clone());
            self.data.extend(v.iter().map(|&x| f32_to_f16(x)));
            added += 1;
        }
        added
    }

    /// Bytes the file holds for these rows: one sealed block.
    fn block(rows: &[(Record, Vec<f32>)]) -> Vec<u8> {
        let dim = rows.first().map_or(0, |(_, v)| v.len());
        let mut w = Writer(Vec::with_capacity(16 + rows.len() * (16 + 24 + dim * 2)));
        w.u32(rows.len() as u32);
        for (rec, v) in rows {
            w.0.extend_from_slice(&rec.key.0);
            w.time(rec.capture);
            w.u16(rec.meta.subsec_ms.unwrap_or(NONE_U16));
            w.u16(rec.meta.sequence.unwrap_or(NONE_U16));
            w.u32(rec.meta.camera);
            w.u8(if rec.accidental { ROW_ACCIDENTAL } else { 0 });
            for &x in v {
                w.u16(f32_to_f16(x));
            }
        }
        w.seal()
    }

    /// Rows of one sealed block (none if it is damaged).
    fn read_block(data: &[u8], dim: usize) -> Option<Vec<(Record, Vec<f32>)>> {
        let mut r = Reader::open(data)?;
        let n = r.count(16 + 1 + 4 + 4 + 1 + dim * 2)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let key = Key(r.take(16)?.try_into().ok()?);
            let capture = r.time()?;
            let subsec = r.u16()?;
            let sequence = r.u16()?;
            let camera = r.u32()?;
            let flags = r.u8()?;
            let mut v = Vec::with_capacity(dim);
            for _ in 0..dim {
                v.push(f16_to_f32(r.u16()?));
            }
            let meta = ShotMeta { subsec_ms: (subsec != NONE_U16).then_some(subsec), sequence: (sequence != NONE_U16).then_some(sequence), focus: None, camera };
            out.push((Record { key, capture, meta, accidental: flags & ROW_ACCIDENTAL != 0 }, v));
        }
        r.done().then_some(out)
    }
}

/// The header block of a vectors file.
fn header_block(model: &ModelInfo, dim: usize) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(VECTORS_MAGIC);
    w.u32(VECTORS_VERSION);
    w.bytes(model.id.dir_name().as_bytes());
    w.bytes(model.weights_sha256().as_bytes());
    w.u32(model.preprocess_version);
    w.u32(dim as u32);
    w.seal()
}

/// The dimension, if the header is this model's.
fn read_header(data: &[u8], model: &ModelInfo) -> Option<usize> {
    let mut r = Reader::open(data)?;
    if r.take(8)? != VECTORS_MAGIC || r.u32()? != VECTORS_VERSION {
        return None;
    }
    if r.bytes()? != model.id.dir_name().as_bytes() || r.bytes()? != model.weights_sha256().as_bytes() {
        return None;
    }
    if r.u32()? != model.preprocess_version {
        return None;
    }
    let dim = r.u32()? as usize;
    (r.done() && dim > 0).then_some(dim)
}

/// Frame a sealed block in the file: its length, then the block.
fn framed(block: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + block.len());
    out.extend_from_slice(&(block.len() as u32).to_le_bytes());
    out.extend_from_slice(block);
    out
}

/// Split a file into its framed blocks, stopping at a torn tail.
fn blocks(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let n = u32::from_le_bytes(data[i..i + 4].try_into().expect("4 bytes")) as usize;
        let Some(b) = data.get(i + 4..i + 4 + n) else { break };
        out.push(b);
        i += 4 + n;
    }
    out
}

/// One library's index: the path map and the vectors, with their files.
pub struct Index {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub model: &'static ModelInfo,
    pub paths: PathMap,
    pub vectors: Vectors,
    /// Moves on whenever the rows change (rows added, the index cleared);
    /// what is derived from the rows (the hidden tags) is keyed by it.
    pub generation: u64,
    paths_dirty: bool,
}

impl Index {
    /// The vectors file for this model.
    fn vectors_file(dir: &Path, model: &ModelInfo) -> PathBuf {
        dir.join(format!("vectors-{}.bin", model.id.dir_name()))
    }

    fn paths_file(dir: &Path) -> PathBuf {
        dir.join("paths.bin")
    }

    /// Load the index for `root` from `dir` (empty when there is none or
    /// it belongs to another model).  Rows no path points at any more are
    /// compacted away here when there are many.
    pub fn load(root: &Path, dir: &Path, model: &'static ModelInfo) -> Index {
        let decoded = fs::read(Self::paths_file(dir)).ok().and_then(|d| PathMap::decode(&d, root));
        // Rows are compacted away only against a path map that was read:
        // without one (another root, a damaged file) nothing is known to
        // be dead, and the vectors stay.
        let paths_known = decoded.is_some();
        let paths = decoded.unwrap_or_else(|| PathMap { root: root.to_path_buf(), ..PathMap::default() });
        let mut vectors = Vectors::default();
        let mut dropped = 0usize;
        if let Ok(data) = fs::read(Self::vectors_file(dir, model)) {
            let bs = blocks(&data);
            if let Some(dim) = bs.first().and_then(|h| read_header(h, model)) {
                vectors.dim = dim;
                let live: HashSet<Key> = paths.entries.values().map(|e| e.key).collect();
                for b in &bs[1..] {
                    let Some(rows) = Vectors::read_block(b, dim) else { break };
                    let (keep, drop): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(r, _)| !paths_known || live.contains(&r.key));
                    dropped += drop.len();
                    vectors.push(&keep);
                }
            }
        }
        let mut ix = Index { root: root.to_path_buf(), dir: dir.to_path_buf(), model, paths, vectors, generation: 1, paths_dirty: false };
        if dropped > 0 && dropped as f64 > COMPACT_SHARE * (dropped + ix.vectors.len()) as f64 {
            let _ = ix.rewrite_vectors();
        }
        ix
    }

    /// Add embedded shots: rows with new keys are appended to the vectors
    /// file; every path is mapped.  Returns how many rows were added.
    pub fn add(&mut self, rows: Vec<(PathBuf, PathEntry, Record, Vec<f32>)>) -> io::Result<usize> {
        let mut new_rows = Vec::new();
        for (rel, entry, rec, v) in rows {
            if !self.vectors.contains(&rec.key) && !new_rows.iter().any(|(r, _): &(Record, Vec<f32>)| r.key == rec.key) {
                new_rows.push((rec, v));
            }
            self.paths.entries.insert(rel, entry);
            self.paths_dirty = true;
        }
        if new_rows.is_empty() {
            return Ok(0);
        }
        fs::create_dir_all(&self.dir)?;
        let file = Self::vectors_file(&self.dir, self.model);
        let fresh = !file.exists() || self.vectors.is_empty();
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&file)?;
        if fresh {
            f.set_len(0)?;
            f.write_all(&framed(&header_block(self.model, new_rows[0].1.len())))?;
        }
        f.write_all(&framed(&Vectors::block(&new_rows)))?;
        f.sync_data()?;
        self.generation += 1;
        Ok(self.vectors.push(&new_rows))
    }

    /// Note that the first index was asked for.
    pub fn set_first_index_asked(&mut self) {
        if !self.paths.first_index_asked {
            self.paths.first_index_asked = true;
            self.paths_dirty = true;
        }
    }

    /// Forget paths no longer in the library (their rows go at the next
    /// compaction).
    pub fn retain_paths(&mut self, live: &HashSet<PathBuf>) {
        let before = self.paths.entries.len();
        self.paths.entries.retain(|p, _| live.contains(p));
        if self.paths.entries.len() != before {
            self.paths_dirty = true;
        }
    }

    /// Write the path map if it changed (whole, atomically).
    pub fn save_paths(&mut self) -> io::Result<()> {
        if !self.paths_dirty {
            return Ok(());
        }
        fs::create_dir_all(&self.dir)?;
        fsutil::atomic_write(&Self::paths_file(&self.dir), &self.paths.encode(), true)?;
        self.paths_dirty = false;
        Ok(())
    }

    /// Rewrite the vectors file from memory (after dropping rows).
    fn rewrite_vectors(&mut self) -> io::Result<()> {
        if self.vectors.is_empty() {
            let _ = fs::remove_file(Self::vectors_file(&self.dir, self.model));
            return Ok(());
        }
        let mut out = framed(&header_block(self.model, self.vectors.dim));
        let rows: Vec<(Record, Vec<f32>)> = (0..self.vectors.len()).map(|i| (self.vectors.records[i].clone(), self.vectors.row(i))).collect();
        for chunk in rows.chunks(1024) {
            out.extend(framed(&Vectors::block(chunk)));
        }
        fs::create_dir_all(&self.dir)?;
        fsutil::atomic_write(&Self::vectors_file(&self.dir, self.model), &out, true)
    }

    /// Shots the index can answer for.
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Delete this library's index files (Preferences ▸ Storage ▸ Clear).
    pub fn clear(&mut self) -> io::Result<()> {
        let _ = fs::remove_file(Self::vectors_file(&self.dir, self.model));
        let _ = fs::remove_file(Self::paths_file(&self.dir));
        let _ = fs::remove_dir(&self.dir);
        self.paths = PathMap { root: self.root.clone(), ..PathMap::default() };
        self.vectors = Vectors::default();
        self.generation += 1;
        self.paths_dirty = false;
        Ok(())
    }

    /// Bytes the files take.
    #[cfg(test)]
    pub fn bytes(&self) -> u64 {
        [Self::vectors_file(&self.dir, self.model), Self::paths_file(&self.dir)].iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum()
    }
}

/// `~/.cache/photo_pipeline/search/`
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cache").join("photo_pipeline").join("search"))
}

/// The folder for one library's index.
pub fn dir_for(library_root: &Path) -> Option<PathBuf> {
    let mut h = Md5::new();
    h.update(library_root.as_os_str().as_encoded_bytes());
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(cache_dir()?.join(digest))
}

/// Shots in an index folder without loading it: the number of rows the
/// vectors file's blocks declare (for the Storage row, on a worker).
pub fn count_rows(dir: &Path) -> usize {
    let model = models::info(ModelId::Siglip2);
    let Ok(data) = fs::read(Index::vectors_file(dir, model)) else { return 0 };
    let bs = blocks(&data);
    if bs.first().and_then(|h| read_header(h, model)).is_none() {
        return 0;
    }
    bs[1..].iter().filter_map(|b| b.get(..4)).map(|n| u32::from_le_bytes(n.try_into().expect("4 bytes")) as usize).sum()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn keys_depend_on_size_and_the_first_64_kb_only() {
        let head: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let k = Key::of(1_000_000, &head);
        assert_eq!(k, Key::of(1_000_000, &head[..KEY_HEAD_BYTES]), "bytes past 64 KB don't count");
        let mut later = head.clone();
        later[90_000] ^= 1;
        assert_eq!(k, Key::of(1_000_000, &later));
        let mut early = head.clone();
        early[10] ^= 1;
        assert_ne!(k, Key::of(1_000_000, &early));
        assert_ne!(k, Key::of(1_000_001, &head), "the size counts");
        assert_eq!(format!("{k}").len(), 32);
        assert_eq!(k.short(), u64::from_le_bytes(k.0[..8].try_into().unwrap()));
    }

    pub fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_key_survives_a_rename_and_a_copy_but_not_a_changed_byte() {
        let d = scratch("key");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 253) as u8).collect();
        fs::write(d.join("DSC01234.JPG"), &bytes).unwrap();
        let k = Key::read(&d.join("DSC01234.JPG")).unwrap();
        fs::rename(d.join("DSC01234.JPG"), d.join("20250312_1430_0007.JPG")).unwrap();
        assert_eq!(Key::read(&d.join("20250312_1430_0007.JPG")).unwrap(), k);
        fs::copy(d.join("20250312_1430_0007.JPG"), d.join("copy.JPG")).unwrap();
        assert_eq!(Key::read(&d.join("copy.JPG")).unwrap(), k);
        let mut changed = bytes.clone();
        changed[100] ^= 0x40;
        fs::write(d.join("changed.JPG"), &changed).unwrap();
        assert_ne!(Key::read(&d.join("changed.JPG")).unwrap(), k);
        fs::write(d.join("small.JPG"), b"tiny").unwrap();
        assert_eq!(Key::read(&d.join("small.JPG")).unwrap(), Key::of(4, b"tiny"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_index_lives_in_the_cache() {
        let dir = dir_for(Path::new("/mnt/nas/library")).unwrap();
        assert!(dir.starts_with(cache_dir().unwrap()));
        assert_ne!(dir, dir_for(Path::new("/mnt/nas/other")).unwrap());
    }

    pub fn row(n: u8, dim: usize) -> (PathBuf, PathEntry, Record, Vec<f32>) {
        let key = Key([n; 16]);
        let mut v: Vec<f32> = (0..dim).map(|i| ((i as u8).wrapping_mul(n) as f32 / 255.0) - 0.5).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= norm);
        let rec = Record {
            key,
            capture: NaiveDateTime::parse_from_str("2025-03-12 14:30:07", "%Y-%m-%d %H:%M:%S").ok(),
            meta: ShotMeta { subsec_ms: Some(250), sequence: Some(n as u16 % 3), focus: None, camera: 7 },
            accidental: n % 5 == 0,
        };
        let rel = PathBuf::from(format!("2025/Mar-2025/12-Mar-2025/JPG/{n:04}.JPG"));
        (rel, PathEntry::new(FileType::Jpg, 1000 + n as u64, &MTime::from_unix(1_700_000_000, 0), key), rec, v)
    }

    #[test]
    fn the_index_round_trips_appends_and_survives_a_torn_tail() {
        let d = scratch("index");
        let root = Path::new("/mnt/nas/library");
        let model = models::info(ModelId::Siglip2);
        let mut ix = Index::load(root, &d, model);
        assert!(ix.is_empty() && !ix.paths.first_index_asked);
        let dim = 8;
        assert_eq!(ix.add((1..=10).map(|n| row(n, dim)).collect()).unwrap(), 10);
        ix.set_first_index_asked();
        ix.save_paths().unwrap();
        // A second batch, with one row already known (mapped, not added).
        let mut again: Vec<_> = (10..=12).map(|n| row(n, dim)).collect();
        again[0].0 = PathBuf::from("copy/0010.JPG");
        assert_eq!(ix.add(again).unwrap(), 2);
        ix.save_paths().unwrap();
        assert_eq!(ix.len(), 12);
        assert_eq!(ix.paths.entries.len(), 13);
        // Reload: everything back, f16 within rounding.
        let back = Index::load(root, &d, model);
        assert_eq!(back.len(), 12);
        assert!(back.paths.first_index_asked);
        assert_eq!(back.paths.entries, ix.paths.entries);
        let i = back.vectors.by_key[&Key([3; 16])];
        assert_eq!(back.vectors.records[i], ix.vectors.records[ix.vectors.by_key[&Key([3; 16])]]);
        let (orig, got) = (row(3, dim).3, back.vectors.row(i));
        assert!(orig.iter().zip(&got).all(|(a, b)| (a - b).abs() < 1e-3));
        assert!(back.vectors.records[back.vectors.by_key[&Key([5; 16])]].accidental);
        assert_eq!(count_rows(&d), 12);
        // Another library root doesn't read this path map.
        assert!(Index::load(Path::new("/elsewhere"), &d, model).paths.entries.is_empty());
        // A torn tail (a crash mid-append) loses only the last block.
        let file = Index::vectors_file(&d, model);
        let data = fs::read(&file).unwrap();
        fs::write(&file, &data[..data.len() - 5]).unwrap();
        let torn = Index::load(root, &d, model);
        assert_eq!(torn.len(), 10, "the first block survives, the torn one goes");
        // A damaged byte inside a block drops that block and the rest.
        let mut bad = data.clone();
        bad[60] ^= 0xff;
        fs::write(&file, &bad).unwrap();
        assert_eq!(Index::load(root, &d, model).len(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rows_without_paths_are_compacted_away_and_clear_removes_everything() {
        let d = scratch("compact");
        let root = Path::new("/mnt/nas/library");
        let model = models::info(ModelId::Siglip2);
        let mut ix = Index::load(root, &d, model);
        ix.add((1..=20).map(|n| row(n, 4)).collect()).unwrap();
        // Half the files leave the library.
        let live: HashSet<PathBuf> = (1..=10).map(|n| row(n, 4).0).collect();
        ix.retain_paths(&live);
        ix.save_paths().unwrap();
        let before = ix.bytes();
        let back = Index::load(root, &d, model);
        assert_eq!(back.len(), 10);
        assert!(back.bytes() < before, "compacted on load");
        assert_eq!(Index::load(root, &d, model).len(), 10, "and loads again");
        let mut back = back;
        back.clear().unwrap();
        assert!(back.is_empty() && !back.paths.first_index_asked);
        assert_eq!(back.bytes(), 0);
        assert!(!d.exists() || fs::read_dir(&d).unwrap().next().is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn another_model_means_an_empty_index() {
        let d = scratch("model");
        let root = Path::new("/mnt/nas/library");
        let model = models::info(ModelId::Siglip2);
        let mut ix = Index::load(root, &d, model);
        ix.add((1..=3).map(|n| row(n, 4)).collect()).unwrap();
        ix.save_paths().unwrap();
        // Forge a header of another preprocessing version.
        let other = ModelInfo { preprocess_version: model.preprocess_version + 1, ..*model };
        let file = Index::vectors_file(&d, model);
        let data = fs::read(&file).unwrap();
        let bs = blocks(&data);
        let mut forged = framed(&header_block(&other, 4));
        for b in &bs[1..] {
            forged.extend(framed(b));
        }
        fs::write(&file, forged).unwrap();
        assert_eq!(Index::load(root, &d, model).len(), 0);
        assert_eq!(crate::model::binfile::fnv1a(b""), 0xcbf29ce484222325);
        let _ = fs::remove_dir_all(&d);
    }
}
