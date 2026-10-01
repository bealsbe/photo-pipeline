//! Background folder scan.
//!
//! The walk itself is cheap; the cost is reading a capture time out of every
//! file, which is pure I/O latency.  The same header read gives the shot
//! metadata bursts are found from (`exif::ShotMeta`), at no extra I/O.  Those reads run across a pool of eight
//! threads, and records are emitted in batches of 250 so a 10 000-file shoot
//! costs forty UI updates rather than ten thousand.  The app's scans also
//! remember capture times per folder (`model::capture_cache`), so a rescan
//! reads only files that are new or changed.
//!
//! Walk semantics follow Python's `os.walk`: top-down, depth-first, hidden
//! (dot) directories skipped, symlinked directories listed but not entered,
//! files in directory order.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::bus::{Bus, BgEvent};
use crate::exif::read_shot;
use crate::model::capture_cache::{CaptureCache, ShotEntry, TimeEntry};
use crate::model::record::{FileType, MTime, PhotoRecord};

const READ_WORKERS: usize = 8;
const BATCH: usize = 250;

pub struct ScanHandle {
    cancel: Arc<AtomicBool>,
}

impl ScanHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

type Entry = (PathBuf, FileType, fs::Metadata);

/// Scan `root` on a worker.  `remember` uses and updates the folder's
/// capture-time cache.
pub fn start(id: u64, root: PathBuf, bus: Bus, remember: bool) -> ScanHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || run(id, &root, &flag, &bus, remember))
        .expect("spawn scanner");
    ScanHandle { cancel }
}

fn run(id: u64, root: &Path, cancel: &AtomicBool, bus: &Bus, remember: bool) {
    // A folder that can't be read is an error, not an empty shoot: it's how
    // the app learns that a share has gone away.
    if let Err(e) = fs::read_dir(root) {
        let _ = bus.unbounded_send(BgEvent::ScanError { scan: id, message: e.to_string() });
        return;
    }
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(READ_WORKERS).build() {
        Ok(p) => p,
        Err(e) => {
            let _ = bus.unbounded_send(BgEvent::ScanError { scan: id, message: e.to_string() });
            return;
        }
    };
    let mut cache = remember.then(|| CaptureCache::load(root));
    let mut count = 0usize;
    let mut batch: Vec<Entry> = Vec::with_capacity(BATCH);
    let flush = |batch: &mut Vec<Entry>, count: &mut usize, cache: &mut Option<CaptureCache>| {
        if batch.is_empty() {
            return;
        }
        let records = build_records(std::mem::take(batch), &pool, cache.as_ref());
        if let Some(c) = cache {
            for r in &records {
                c.note(r.path.clone(), ShotEntry { time: TimeEntry::new(r.file_size, &r.modified, r.capture_time), meta: r.shot });
            }
        }
        *count += records.len();
        let _ = bus.unbounded_send(BgEvent::ScanBatch { scan: id, records: Arc::new(records) });
        let _ = bus.unbounded_send(BgEvent::ScanProgress { scan: id, count: *count });
    };

    walk(root, cancel, &mut |entry| {
        batch.push(entry);
        if batch.len() >= BATCH {
            flush(&mut batch, &mut count, &mut cache);
        }
    });
    let complete = !cancel.load(Ordering::Relaxed);
    if complete {
        flush(&mut batch, &mut count, &mut cache);
    }
    let _ = bus.unbounded_send(BgEvent::ScanComplete { scan: id, count });
    // Only a complete scan knows every file the folder holds.
    if let (true, Some(c)) = (complete, cache) {
        c.save();
    }
}

/// Depth-first, top-down walk matching `os.walk(root)`.
fn walk(dir: &Path, cancel: &AtomicBool, emit: &mut dyn FnMut(Entry)) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let Ok(ft) = entry.file_type() else { continue };
        // os.walk classifies with is_dir(), which follows symlinks.
        let is_dir = ft.is_dir() || (ft.is_symlink() && fs::metadata(&path).is_ok_and(|m| m.is_dir()));
        if is_dir {
            if !name.to_string_lossy().starts_with('.') && !ft.is_symlink() {
                subdirs.push(path);
            }
            continue;
        }
        let Some(file_type) = FileType::from_path(&path) else { continue };
        let Ok(meta) = fs::metadata(&path) else { continue };
        emit((path, file_type, meta));
    }
    for sub in subdirs {
        walk(&sub, cancel, emit);
    }
}

/// Build records, reading capture times and shot metadata in parallel where
/// `cache` doesn't already have them, keeping walk order.
fn build_records(batch: Vec<Entry>, pool: &rayon::ThreadPool, cache: Option<&CaptureCache>) -> Vec<PhotoRecord> {
    pool.install(|| {
        batch
            .into_par_iter()
            .map(|(path, file_type, meta)| {
                let mtime = MTime::from_metadata(&meta);
                let (ct, shot) = match cache.and_then(|c| c.get(&path, meta.len(), &mtime)) {
                    Some(known) => known,
                    None => read_shot(&path, file_type == FileType::Raw),
                };
                let mut r = PhotoRecord::new(path, file_type, meta.len(), mtime, ct);
                r.shot = shot;
                r
            })
            .collect()
    })
}
