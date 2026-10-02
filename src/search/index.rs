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
//! - `vectors-<model>.bin`: a header (magic, version, model id, weights
//!   SHA-256, preprocessing version, dimension), then sealed blocks of
//!   rows (key, capture info, flags, f16 vector) appended as batches
//!   arrive.  A torn last block is dropped.  Rows whose key no path maps
//!   to any more are dropped when the file is compacted.
//!
//! Nothing here touches the library.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::model::record::{FileType, MTime, ShotMeta};

/// Bytes of a file that go into its key, with its size.
pub const KEY_HEAD_BYTES: usize = 64 * 1024;

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
    pub fn read(path: &Path) -> std::io::Result<Key> {
        use std::io::Read;
        let mut f = crate::exif::open_for_header(path)?;
        let size = f.metadata()?.len();
        let mut buf = Vec::with_capacity(KEY_HEAD_BYTES);
        f.by_ref().take(KEY_HEAD_BYTES as u64).read_to_end(&mut buf)?;
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
    /// Relative to the library root.
    pub rel: PathBuf,
    pub file_type: FileType,
    pub size: u64,
    /// `MTime::timestamp` bits.
    pub mtime: u64,
    pub key: Key,
}

impl PathEntry {
    pub fn holds_for(&self, size: u64, mtime: &MTime) -> bool {
        self.size == size && self.mtime == mtime.timestamp.to_bits()
    }
}

/// The path map: files of the library → keys, as last seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathMap {
    pub root: PathBuf,
    /// The first index was asked for (so new exports are indexed on their
    /// own from then on).
    pub first_index_asked: bool,
    pub entries: HashMap<PathBuf, PathEntry>,
}

impl PathMap {
    /// The key for `path` if the map holds it unchanged.
    pub fn key_for(&self, rel: &Path, size: u64, mtime: &MTime) -> Option<Key> {
        self.entries.get(rel).filter(|e| e.holds_for(size, mtime)).map(|e| e.key)
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

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_key_survives_a_rename_and_a_copy_but_not_a_changed_byte() {
        let d = std::env::temp_dir().join(format!("pp-search-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 253) as u8).collect();
        std::fs::write(d.join("DSC01234.JPG"), &bytes).unwrap();
        let k = Key::read(&d.join("DSC01234.JPG")).unwrap();
        std::fs::rename(d.join("DSC01234.JPG"), d.join("20250312_1430_0007.JPG")).unwrap();
        assert_eq!(Key::read(&d.join("20250312_1430_0007.JPG")).unwrap(), k);
        std::fs::copy(d.join("20250312_1430_0007.JPG"), d.join("copy.JPG")).unwrap();
        assert_eq!(Key::read(&d.join("copy.JPG")).unwrap(), k);
        let mut changed = bytes.clone();
        changed[100] ^= 0x40;
        std::fs::write(d.join("changed.JPG"), &changed).unwrap();
        assert_ne!(Key::read(&d.join("changed.JPG")).unwrap(), k);
        // A small file counts whole.
        std::fs::write(d.join("small.JPG"), b"tiny").unwrap();
        assert_eq!(Key::read(&d.join("small.JPG")).unwrap(), Key::of(4, b"tiny"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_index_lives_in_the_cache() {
        let dir = dir_for(Path::new("/mnt/nas/library")).unwrap();
        assert!(dir.starts_with(cache_dir().unwrap()));
        assert_ne!(dir, dir_for(Path::new("/mnt/nas/other")).unwrap());
    }
}
