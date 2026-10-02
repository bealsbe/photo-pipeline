//! What the Preferences page's Storage section measures and clears: the
//! four caches under `~/.cache/photo_pipeline`, and the folder of marks
//! waiting to be saved (`~/.local/share/photo-pipeline/unsaved`), which is
//! only ever counted.  Everything here touches the disk, so the page runs
//! it on a worker thread.
//!
//! Clearing removes what is *inside* a cache folder, never the folder
//! itself, and doesn't follow symbolic links: a link is removed, not what
//! it points at.  The thumbnail writers create their folder only when the
//! app starts, so it is put back straight away.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::imaging::thumbs::GENERATION_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cache {
    /// `thumbs/<size>/`, shared with the Python app (`imaging::thumbs::DiskCache`).
    Thumbs,
    /// `library/`, the calendar's saved index (`model::library_cache`).
    Library,
    /// `capture-times/`, per working folder (`model::capture_cache`).
    CaptureTimes,
    /// `quality/`, what analysing measured, per folder
    /// (`model::quality_cache`).
    Quality,
}

impl Cache {
    pub const ALL: [Cache; 4] = [Cache::Thumbs, Cache::Library, Cache::CaptureTimes, Cache::Quality];

    fn folder_name(self) -> &'static str {
        match self {
            Cache::Thumbs => "thumbs",
            Cache::Library => "library",
            Cache::CaptureTimes => "capture-times",
            Cache::Quality => "quality",
        }
    }

    /// The cache's folder, as the modules that write it build it.
    pub fn dir(self) -> Option<PathBuf> {
        Some(cache_root()?.join(self.folder_name()))
    }

    /// Folders that must still exist after clearing.
    fn keep(self, dir: &Path) -> Vec<PathBuf> {
        match self {
            Cache::Thumbs => vec![dir.join(GENERATION_SIZE.to_string())],
            Cache::Library | Cache::CaptureTimes | Cache::Quality => Vec::new(),
        }
    }

    /// Clear this cache (on a worker thread).
    pub fn clear(self) -> Cleared {
        match self.dir() {
            Some(dir) => clear(&dir, &self.keep(&dir)),
            None => Cleared { first_error: Some("there is no home folder".into()), ..Cleared::default() },
        }
    }
}

/// `~/.cache/photo_pipeline`: the Python app's location, kept by both apps.
fn cache_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cache").join("photo_pipeline"))
}

/// How much a folder holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub exists: bool,
    /// Disk space taken, as `du` counts it.
    pub bytes: u64,
    pub files: u64,
}

/// Measure everything under `dir`, without following links.
pub fn measure(dir: &Path) -> Usage {
    let mut u = Usage { exists: dir.is_dir(), ..Usage::default() };
    if u.exists {
        add_up(dir, &mut u);
    }
    u
}

fn add_up(dir: &Path, u: &mut Usage) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            add_up(&entry.path(), u);
        } else if let Ok(meta) = entry.metadata() {
            u.files += 1;
            u.bytes += disk_bytes(&meta);
        }
    }
}

#[cfg(unix)]
fn disk_bytes(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn disk_bytes(meta: &fs::Metadata) -> u64 {
    meta.len()
}

/// What a clear did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cleared {
    pub removed: u64,
    pub failed: u64,
    pub first_error: Option<String>,
}

impl Cleared {
    fn note(&mut self, r: io::Result<()>) {
        match r {
            Ok(()) => self.removed += 1,
            // Gone already (a writer's temp file renamed meanwhile): fine.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                self.failed += 1;
                self.first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
}

/// Remove everything inside `dir` (not `dir` itself), then make sure each
/// folder in `keep` exists.
pub fn clear(dir: &Path, keep: &[PathBuf]) -> Cleared {
    let mut c = Cleared::default();
    if dir.is_dir() {
        empty(dir, &mut c);
    }
    for k in keep {
        if let Err(e) = fs::create_dir_all(k) {
            c.first_error.get_or_insert_with(|| e.to_string());
        }
    }
    c
}

fn empty(dir: &Path, c: &mut Cleared) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            c.note(Err(e));
            return;
        }
    };
    for entry in rd.flatten() {
        let path = entry.path();
        // Not followed: a link is a file here.
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                empty(&path, c);
                // Folders aren't counted; one still holding a file that
                // couldn't go has already been counted as a failure.
                let _ = fs::remove_dir(&path);
            }
            _ => c.note(fs::remove_file(&path)),
        }
    }
}

/// The unsaved-marks folder (never cleared from the app).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Journal {
    pub exists: bool,
    /// Sidecars waiting to be written to their folders.
    pub waiting: usize,
    /// Copies set aside because the folder's own marks changed since.
    pub kept: usize,
}

pub fn journal(dir: &Path) -> Journal {
    let json = |d: &Path| -> usize {
        fs::read_dir(d)
            .map(|rd| rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0)
    };
    Journal { exists: dir.is_dir(), waiting: json(dir), kept: json(&dir.join("kept")) }
}

/// The search index's and the downloaded models' sizes (the engine's
/// `storage`): cleared through the engine, never here.
pub type SearchStorage = crate::search::Storage;

/// Every number the Storage section shows.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    pub caches: Vec<(Cache, Usage)>,
    pub journal: Journal,
    pub search: SearchStorage,
}

/// Measure every cache and count the unsaved marks (on a worker thread).
pub fn survey(library: Option<&Path>) -> Survey {
    let caches = Cache::ALL.iter().map(|&c| (c, c.dir().map(|d| measure(&d)).unwrap_or_default())).collect();
    let journal = crate::model::saver::default_journal_dir().map(|d| journal(&d)).unwrap_or_default();
    Survey { caches, journal, search: crate::search::engine::storage(library) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-prefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    #[test]
    fn cache_folders_are_where_their_writers_put_them() {
        let home = dirs::home_dir().unwrap();
        let root = home.join(".cache").join("photo_pipeline");
        assert_eq!(Cache::Thumbs.dir(), Some(root.join("thumbs")));
        assert_eq!(Cache::Library.dir(), Some(root.join("library")));
        assert_eq!(Cache::CaptureTimes.dir(), Some(root.join("capture-times")));
        assert_eq!(Cache::Quality.dir(), Some(root.join("quality")));
        // The analysis writes where the Storage section looks.
        let analysed = crate::model::quality_cache::file_for(std::path::Path::new("/photos/shoot")).expect("a cache file");
        assert_eq!(analysed.parent(), Cache::Quality.dir().as_deref());
        let thumbs = Cache::Thumbs.dir().unwrap();
        assert_eq!(Cache::Thumbs.keep(&thumbs), vec![thumbs.join("300")]);
    }

    #[test]
    fn measures_files_in_nested_folders() {
        let d = dir("measure");
        put(&d.join("300/a.jpg"), 5000);
        put(&d.join("300/b.jpg"), 10);
        put(&d.join("180/c.png"), 1);
        let u = measure(&d);
        assert!(u.exists);
        assert_eq!(u.files, 3);
        // Allocated space: at least the bytes written, in whole blocks.
        assert!(u.bytes >= 5011, "{u:?}");
        assert_eq!(measure(&d.join("missing")), Usage::default());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn clearing_empties_the_folder_but_keeps_it_and_the_writers_folder() {
        let d = dir("clear");
        let cache = d.join("thumbs");
        put(&cache.join("300/a.jpg"), 100);
        put(&cache.join("300/b.0123.tmp"), 100);
        put(&cache.join("180/old.png"), 100);
        put(&cache.join("stray.txt"), 1);
        let c = clear(&cache, &[cache.join("300")]);
        assert_eq!(c, Cleared { removed: 4, failed: 0, first_error: None });
        assert!(cache.is_dir() && cache.join("300").is_dir());
        assert!(!cache.join("180").exists());
        assert_eq!(measure(&cache), Usage { exists: true, bytes: 0, files: 0 });
        // Clearing an empty or missing cache is fine.
        assert_eq!(clear(&cache, &[]).removed, 0);
        assert_eq!(clear(&d.join("missing"), &[]), Cleared::default());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn clearing_removes_links_not_what_they_point_at() {
        let d = dir("links");
        let cache = d.join("library");
        let outside = d.join("photos");
        put(&outside.join("keep.jpg"), 50);
        put(&cache.join("x.bin"), 10);
        std::os::unix::fs::symlink(&outside, cache.join("linked-folder")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep.jpg"), cache.join("linked-file")).unwrap();
        // Links are counted as entries, not followed.
        assert_eq!(measure(&cache).files, 3);
        let c = clear(&cache, &[]);
        assert_eq!((c.removed, c.failed), (3, 0));
        assert!(outside.join("keep.jpg").is_file(), "a link's target must survive");
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn journal_counts_waiting_and_set_aside_copies() {
        let d = dir("journal");
        let j = d.join("unsaved");
        assert_eq!(journal(&j), Journal::default());
        put(&j.join("0a1b.json"), 10);
        put(&j.join("2c3d.json"), 10);
        put(&j.join("2c3d.json.~tmp"), 10);
        put(&j.join("kept/0a1b-20260930-101010.json"), 10);
        assert_eq!(journal(&j), Journal { exists: true, waiting: 2, kept: 1 });
        let _ = fs::remove_dir_all(&d);
    }
}
