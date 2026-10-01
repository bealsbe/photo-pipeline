//! The library's folder tree, kept between runs, so the calendar opens at
//! once and a check for changes reads only the folders that changed.
//!
//! A folder's mtime moves whenever an entry is added to it, removed from it
//! or renamed in it — which covers every way the export writes (a temp file
//! renamed into place).  So a check stats each known folder and re-reads
//! only those whose mtime moved: on a 110 000-file library that is 253
//! stats instead of reading every folder listing over the network.  What a
//! folder's mtime does not show is a file rewritten in place by other
//! software; Refresh (F5) re-reads everything for that.
//!
//! EXIF capture times read for library files are kept alongside, keyed by
//! path and valid while the file's size and mtime are unchanged.
//!
//! Saved to `~/.cache/photo_pipeline/library/<md5 of the root>.bin`.  It's a
//! cache: anything unreadable, from another version or failing its checksum
//! is ignored and the library is read in full.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use md5::{Digest, Md5};
use rayon::prelude::*;

use super::binfile::{Reader, Writer, os_from_bytes};
use super::capture_cache::{TimeEntry, read_times, write_times};
use super::library_index::FileEntry;
use super::record::{FileType, unix_mtime};

/// Folders read at once.  On a network share each read waits on the server,
/// so this is about hiding latency, not using cores.
const SCAN_THREADS: usize = 16;
/// Capture-time reads at once.  A NAS stops getting faster past about this.
const READ_THREADS: usize = 16;
/// A folder modified this close to the check (or after it) is re-read next
/// time: a second change inside its mtime's resolution wouldn't move it.
const RACY_SECS: i64 = 2;

const MAGIC: &[u8; 8] = b"PPLIBIX\0";
const VERSION: u32 = 1;

/// An mtime as the filesystem gives it: seconds and nanoseconds.
pub type Stamp = (i64, u32);

#[derive(Debug, Clone, PartialEq)]
pub struct CachedFile {
    pub name: OsString,
    pub file_type: FileType,
    pub size: u64,
    pub mtime: Stamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    /// The folder's mtime when it was read, or `None` when that can't vouch
    /// for its contents (modified too recently, or a failed read).
    pub stamp: Option<Stamp>,
    /// Sorted by name, as are `files`.
    pub subdirs: Vec<OsString>,
    pub files: Vec<CachedFile>,
}

impl Folder {
    fn same_entries(&self, other: &Folder) -> bool {
        self.subdirs == other.subdirs && self.files == other.files
    }
}

/// Every folder under the library root, keyed by its path relative to the
/// root (the root itself is the empty path).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tree {
    pub folders: HashMap<PathBuf, Folder>,
}

impl Tree {
    pub fn file_count(&self) -> usize {
        self.folders.values().map(|f| f.files.len()).sum()
    }
}

/// `root` joined with a relative folder path (without a trailing separator
/// for the root itself).
pub fn folder_path(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { root.to_path_buf() } else { root.join(rel) }
}

pub struct Scanned {
    pub tree: Tree,
    /// Some folder's contents differ from the previous tree, or there was none.
    pub changed: bool,
    /// Folders whose listing was read (the rest were vouched for by mtime).
    pub reread: usize,
}

/// Read the library's tree.  With a `previous` tree, folders whose mtime is
/// unchanged are taken from it; `deep` reads every folder regardless.
/// `Ok(None)` if cancelled; `Err` if the root itself can't be read (so a
/// share that has gone away never replaces a good tree with an empty one).
pub fn scan(
    root: &Path,
    previous: Option<&Tree>,
    deep: bool,
    cancelled: &AtomicBool,
    progress: &(dyn Fn(usize) + Sync),
) -> Result<Option<Scanned>, String> {
    if let Err(e) = fs::read_dir(root) {
        return Err(format!("Couldn't read {}: {e}", root.display()));
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(SCAN_THREADS)
        .thread_name(|i| format!("library-scan-{i}"))
        .build()
        .map_err(|e| e.to_string())?;
    let seen = AtomicUsize::new(0);
    let mut folders: HashMap<PathBuf, Folder> = HashMap::new();
    let mut reread = 0;
    // Breadth first, one level at a time, each level's folders in parallel.
    let mut level = vec![PathBuf::new()];
    while !level.is_empty() {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read: Vec<(PathBuf, Option<(Folder, bool)>)> = pool.install(|| {
            level
                .into_par_iter()
                .map(|rel| {
                    if cancelled.load(Ordering::Relaxed) {
                        return (rel, None);
                    }
                    let prev = previous.and_then(|t| t.folders.get(&rel));
                    let got = read_folder(&folder_path(root, &rel), prev, deep, now, &seen, progress);
                    (rel, got)
                })
                .collect()
        });
        let mut next = Vec::new();
        for (rel, got) in read {
            let Some((folder, fresh)) = got else { continue };
            reread += fresh as usize;
            next.extend(folder.subdirs.iter().map(|n| rel.join(n)));
            folders.insert(rel, folder);
        }
        level = next;
    }
    if cancelled.load(Ordering::Relaxed) {
        return Ok(None);
    }
    progress(seen.load(Ordering::Relaxed));
    let changed = match previous {
        None => true,
        Some(p) => {
            p.folders.len() != folders.len()
                || folders.iter().any(|(k, f)| p.folders.get(k).is_none_or(|q| !q.same_entries(f)))
        }
    };
    Ok(Some(Scanned { tree: Tree { folders }, changed, reread }))
}

/// Read one folder's listing, or take `prev` when its mtime vouches for it.
/// `None` when the folder is gone.  The bool is true when it was read.
fn read_folder(
    dir: &Path,
    prev: Option<&Folder>,
    deep: bool,
    now: i64,
    seen: &AtomicUsize,
    progress: &(dyn Fn(usize) + Sync),
) -> Option<(Folder, bool)> {
    // A folder that fails for any reason but "not found" keeps what was last
    // read of it (and is read again next time), rather than dropping its
    // photos from the calendar over a network hiccup.
    let keep = |e: io::Error| match (e.kind(), prev) {
        (io::ErrorKind::NotFound, _) | (_, None) => None,
        (_, Some(p)) => Some((Folder { stamp: None, ..p.clone() }, false)),
    };
    let meta = match fs::metadata(dir) {
        Ok(m) => m,
        Err(e) => return keep(e),
    };
    if !meta.is_dir() {
        return None;
    }
    let stamp = unix_mtime(&meta);
    if let Some(p) = prev {
        if !deep && p.stamp == Some(stamp) {
            return Some((p.clone(), false));
        }
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => return keep(e),
    };
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            subdirs.push(name);
            continue;
        }
        let path = entry.path();
        let Some(file_type) = FileType::from_path(&path) else { continue };
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.is_dir() {
            continue;
        }
        files.push(CachedFile { name, file_type, size: meta.len(), mtime: unix_mtime(&meta) });
        let n = seen.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 500 == 0 {
            progress(n);
        }
    }
    subdirs.sort();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let trusted = stamp.0 < now - RACY_SECS;
    Some((Folder { stamp: trusted.then_some(stamp), subdirs, files }, true))
}

// ── capture times ───────────────────────────────────────────────────────────

/// EXIF capture times for `files`, read on a shared pool.
pub fn read_capture_times(files: &[&FileEntry]) -> Vec<Option<NaiveDateTime>> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    let read = |fe: &&FileEntry| crate::exif::read_capture_time(&fe.path, fe.file_type == FileType::Raw);
    let pool = POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(READ_THREADS)
            .thread_name(|i| format!("library-exif-{i}"))
            .build()
            .ok()
    });
    match pool {
        Some(p) => p.install(|| files.par_iter().map(read).collect()),
        None => files.iter().map(read).collect(),
    }
}

// ── the store ───────────────────────────────────────────────────────────────

/// What is known about one library between runs: its tree and the capture
/// times read so far.  Shared (behind a mutex) by the index worker and the
/// day loaders.
pub struct Store {
    root: PathBuf,
    loaded: bool,
    pub tree: Option<Arc<Tree>>,
    times: HashMap<PathBuf, TimeEntry>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root, loaded: false, tree: None, times: HashMap::new() }
    }

    fn file(&self) -> Option<PathBuf> {
        let mut h = Md5::new();
        h.update(self.root.as_os_str().as_encoded_bytes());
        let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Some(dirs::home_dir()?.join(".cache").join("photo_pipeline").join("library").join(format!("{digest}.bin")))
    }

    /// Read the saved state, once.
    pub fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let Some(data) = self.file().and_then(|f| fs::read(f).ok()) else { return };
        if let Some((tree, times)) = decode(&data, &self.root) {
            self.tree = Some(Arc::new(tree));
            self.times = times;
        }
    }

    /// Write the current state (best effort: it's a cache).
    pub fn save(&self) {
        let Some(file) = self.file() else { return };
        let Some(tree) = &self.tree else { return };
        if crate::ops::fsutil::ensure_parent(&file).is_err() {
            return;
        }
        let _ = crate::ops::fsutil::atomic_write(&file, &encode(&self.root, tree, &self.times), true);
    }

    /// A cached capture time for `fe`: `Some(None)` means it was read and
    /// has none.  `None` if not cached, or the file changed since.
    pub fn time(&self, fe: &FileEntry) -> Option<Option<NaiveDateTime>> {
        self.times.get(&fe.path)?.valid_for(fe.size, &fe.mtime)
    }

    pub fn set_time(&mut self, fe: &FileEntry, time: Option<NaiveDateTime>) {
        self.times.insert(fe.path.clone(), TimeEntry::new(fe.size, &fe.mtime, time));
    }

    /// Replace the tree, forgetting capture times of files no longer in it.
    pub fn set_tree(&mut self, tree: Arc<Tree>) {
        if !self.times.is_empty() {
            let mut present: HashSet<PathBuf> = HashSet::new();
            for (rel, folder) in &tree.folders {
                let dir = folder_path(&self.root, rel);
                present.extend(folder.files.iter().map(|f| dir.join(&f.name)));
            }
            self.times.retain(|p, _| present.contains(p));
        }
        self.tree = Some(tree);
    }
}

// ── file format ─────────────────────────────────────────────────────────────
//
// Magic, version, root, the folders, the capture times (see `binfile`).

fn encode(root: &Path, tree: &Tree, times: &HashMap<PathBuf, TimeEntry>) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + tree.file_count() * 48));
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.path(root);
    w.u32(tree.folders.len() as u32);
    for (rel, f) in &tree.folders {
        w.path(rel);
        match f.stamp {
            Some(s) => {
                w.u8(1);
                w.stamp(s);
            }
            None => w.u8(0),
        }
        w.u32(f.subdirs.len() as u32);
        for s in &f.subdirs {
            w.bytes(s.as_encoded_bytes());
        }
        w.u32(f.files.len() as u32);
        for file in &f.files {
            w.bytes(file.name.as_encoded_bytes());
            w.u8(match file.file_type {
                FileType::Jpg => 0,
                FileType::Raw => 1,
            });
            w.u64(file.size);
            w.stamp(file.mtime);
        }
    }
    write_times(&mut w, times);
    w.seal()
}

fn decode(data: &[u8], root: &Path) -> Option<(Tree, HashMap<PathBuf, TimeEntry>)> {
    let mut r = Reader::open(data)?;
    if r.take(8)? != MAGIC || r.u32()? != VERSION || r.path()? != root {
        return None;
    }
    let n_folders = r.count(13)?;
    let mut folders = HashMap::with_capacity(n_folders);
    for _ in 0..n_folders {
        let rel = r.path()?;
        let stamp = match r.u8()? {
            1 => Some(r.stamp()?),
            _ => None,
        };
        let n_sub = r.count(4)?;
        let mut subdirs = Vec::with_capacity(n_sub);
        for _ in 0..n_sub {
            subdirs.push(os_from_bytes(r.bytes()?));
        }
        let n_files = r.count(25)?;
        let mut files = Vec::with_capacity(n_files);
        for _ in 0..n_files {
            let name = os_from_bytes(r.bytes()?);
            let file_type = match r.u8()? {
                0 => FileType::Jpg,
                1 => FileType::Raw,
                _ => return None,
            };
            files.push(CachedFile { name, file_type, size: r.u64()?, mtime: r.stamp()? });
        }
        folders.insert(rel, Folder { stamp, subdirs, files });
    }
    let times = read_times(&mut r)?;
    r.done().then_some((Tree { folders }, times))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;

    fn sample() -> (Tree, HashMap<PathBuf, TimeEntry>) {
        let mut folders = HashMap::new();
        folders.insert(
            PathBuf::new(),
            Folder { stamp: Some((1_700_000_000, 5)), subdirs: vec!["2025".into()], files: vec![] },
        );
        folders.insert(
            PathBuf::from("2025"),
            Folder {
                stamp: None,
                subdirs: vec![],
                files: vec![CachedFile {
                    name: "20250312_1430_0007.ARW".into(),
                    file_type: FileType::Raw,
                    size: 25_000_000,
                    mtime: (1_741_789_800, 123_456_789),
                }],
            },
        );
        let mut times = HashMap::new();
        times.insert(
            PathBuf::from("/lib/2025/a.jpg"),
            TimeEntry { size: 9, mtime: 1.5f64.to_bits(), time: NaiveDateTime::parse_from_str("2025-03-12 14:30:05", "%Y-%m-%d %H:%M:%S").ok() },
        );
        times.insert(PathBuf::from("/lib/2025/b.jpg"), TimeEntry { size: 3, mtime: 2.5f64.to_bits(), time: None });
        (Tree { folders }, times)
    }

    #[test]
    fn round_trips_and_rejects_damage() {
        let (tree, times) = sample();
        let root = Path::new("/lib");
        let data = encode(root, &tree, &times);
        let (t2, times2) = decode(&data, root).expect("decodes");
        assert_eq!(t2, tree);
        assert_eq!(times2, times);
        // Another library's cache, a flipped byte and a truncation are all refused.
        assert!(decode(&data, Path::new("/other")).is_none());
        let mut bad = data.clone();
        bad[20] ^= 1;
        assert!(decode(&bad, root).is_none());
        assert!(decode(&data[..data.len() - 3], root).is_none());
    }

    #[test]
    fn capture_times_follow_the_file() {
        let mut st = Store::new(PathBuf::from("/lib"));
        let fe = FileEntry {
            path: PathBuf::from("/lib/a.jpg"),
            file_type: FileType::Jpg,
            size: 10,
            mtime: MTime::from_unix(1_700_000_000, 0),
        };
        assert_eq!(st.time(&fe), None);
        st.set_time(&fe, None);
        assert_eq!(st.time(&fe), Some(None));
        // Rewritten in place: the cached time no longer applies.
        let changed = FileEntry { size: 11, ..fe.clone() };
        assert_eq!(st.time(&changed), None);
    }
}
