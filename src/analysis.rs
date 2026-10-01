//! The analysis pass: measure each shot's embedded preview
//! (`imaging::quality`) and find identical files, off the UI thread.
//! Started only when asked (decision 2); `app::quality` starts it and
//! keeps the results.
//!
//! - **One job per shot** (the caller passes which files to measure: a
//!   pair's JPG, else the file), in the order given (the grid's, so what
//!   is on screen comes first).  Files whose cached entry still holds are
//!   reported straight away without being read.
//! - **A few threads** ([`THREADS`]): a shot costs ~0.3–0.46 MB of reads
//!   and ~5 ms of CPU, so on a network share the pass is bound by bandwidth
//!   and more threads would only slow the thumbnails down.  Workers also
//!   step back while the grid waits for thumbnails ([`Handle::set_busy`]).
//! - **Results stream** as [`BgEvent::Analysis`] batches: every 100 results
//!   or 250 ms, with progress.
//! - **Duplicates** come last: only files sharing a type, size and capture
//!   time with another are read: 64 KB from the start, and 64 KB from the
//!   middle and the end of those whose starts match (`model::duplicates`).
//! - **Cancellable** at any point (a folder change, quitting); what was
//!   measured is still saved to the cache, entries standing alone.
//! - If the folder stops answering (a share gone away), the pass stops and
//!   says so; running it again resumes from the cache.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};

use crate::bus::{BgEvent, Bus};
use crate::imaging::quality::{self, Features, Plane};
use crate::model::collection::{Collection, RecordId};
use crate::model::duplicates;
use crate::model::quality_cache::{self, Entry, QualityCache};
use crate::model::record::{FileType, MTime};

/// Worker threads measuring previews.
pub const THREADS: usize = 4;
const BATCH: usize = 100;
const BATCH_MS: u64 = 250;
/// Bytes read from the start, middle and end of a file to tell same-size
/// files apart.
const SAMPLE_BYTES: u64 = 64 * 1024;
/// Failures in a row after which the folder is checked for being there.
const FAILURE_RUN: usize = 8;

/// One file of the folder.
#[derive(Debug, Clone)]
pub struct FileJob {
    pub path: PathBuf,
    pub file_type: FileType,
    pub size: u64,
    pub mtime: MTime,
    /// Measure it (it stands for a shot); otherwise it only takes part in
    /// the duplicate check and keeps its cache entry.
    pub measure: bool,
    /// The AF point, as fractions of the photo as shown.
    pub focus: Option<(f32, f32)>,
    /// When the file says it was shot (capture ms, press sequence, camera),
    /// from its own header.  Identical files say the same, so files that
    /// differ here are never read to compare them.
    pub capture: Option<(i64, Option<u16>, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Measure what the cache doesn't hold, and check for duplicates.
    Full,
    /// Only what the cache already holds: no file is read.
    CachedOnly,
}

pub struct Request {
    pub run: u64,
    pub root: PathBuf,
    /// Every file of the folder, measured ones in the order wanted.
    pub files: Vec<FileJob>,
    pub mode: Mode,
    pub threads: usize,
    /// The cache file to read and update (`quality_cache::file_for`), or
    /// none at all.
    pub cache: Option<PathBuf>,
}

/// Part of a pass's results.
#[derive(Debug, Default)]
pub struct Batch {
    pub features: Vec<(PathBuf, Arc<Features>)>,
    pub failed: Vec<(PathBuf, String)>,
    /// Shots measured, cached or failed so far, of `total`.
    pub done: usize,
    pub total: usize,
    /// Groups of identical files, once the check has run.
    pub duplicates: Option<Vec<Vec<PathBuf>>>,
    pub finished: Option<Finished>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finished {
    /// Every shot was measured (or failed) and the duplicate check ran.
    pub complete: bool,
    /// Why the pass stopped early, when it did for a reason.
    pub error: Option<String>,
    pub measured: usize,
    pub cached: usize,
    pub failed: usize,
    /// Bytes read for the duplicate check, and its share of `elapsed`.
    pub hashed_bytes: u64,
    pub hashing: Duration,
    pub elapsed: Duration,
}

/// A running pass.
pub struct Handle {
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicUsize>,
}

impl Handle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Thumbnails the grid is waiting for: workers step back while any are.
    pub fn set_busy(&self, pending: usize) {
        self.busy.store(pending, Ordering::Relaxed);
    }
}

/// Every file of `coll`: one job to measure per shot (a pair's JPG, else
/// the file), those of `first` (record ids, either half of a pair) first in
/// that order, then the rest in collection order; then every other file,
/// for the duplicate check.
pub fn jobs(coll: &Collection, first: &[RecordId]) -> Vec<FileJob> {
    let rep = |id: RecordId| -> RecordId {
        match (coll.get(id).map(|r| r.file_type), coll.find_pair(id)) {
            (Some(FileType::Raw), Some(p)) => p,
            _ => id,
        }
    };
    let mut order: Vec<RecordId> = Vec::with_capacity(coll.len());
    let mut seen = HashSet::new();
    for &id in first.iter().chain(coll.ids()) {
        let r = rep(id);
        if coll.get(r).is_some() && seen.insert(r) {
            order.push(r);
        }
    }
    let job = |id: RecordId, measure: bool| {
        coll.get(id).map(|r| FileJob {
            path: r.path.clone(),
            file_type: r.file_type,
            size: r.file_size,
            mtime: r.modified,
            measure,
            // The AF point: the JPG's, else its RAW's.
            focus: r.shot.focus_point().or_else(|| coll.find_pair(id).and_then(|p| coll.get(p)).and_then(|p| p.shot.focus_point())),
            capture: r.capture_time.map(|t| (t.and_utc().timestamp_millis() + r.shot.subsec_ms.unwrap_or(0) as i64, r.shot.sequence, r.shot.camera)),
        })
    };
    let mut files: Vec<FileJob> = order.iter().filter_map(|&id| job(id, true)).collect();
    files.extend(coll.ids().iter().filter(|&&id| !seen.contains(&id)).filter_map(|&id| job(id, false)));
    files
}

/// Run a pass on its own thread, reporting to `bus`.
pub fn start(req: Request, bus: Bus) -> Handle {
    let cancel = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicUsize::new(0));
    let (c, b) = (cancel.clone(), busy.clone());
    let run = req.run;
    std::thread::Builder::new()
        .name("analysis".into())
        .spawn(move || {
            execute(req, &c, &b, &mut |batch| {
                let _ = bus.unbounded_send(BgEvent::Analysis { run, batch: Arc::new(batch) });
            })
        })
        .expect("spawn analysis");
    Handle { cancel, busy }
}

/// The pass itself, on the calling thread; `emit` gets the batches.
pub fn execute(req: Request, cancel: &AtomicBool, busy: &AtomicUsize, emit: &mut dyn FnMut(Batch)) -> Finished {
    let started = Instant::now();
    let cache = match &req.cache {
        Some(file) => QualityCache::load_file(file, &req.root),
        None => QualityCache { root: req.root.clone(), ..QualityCache::default() },
    };
    let mut fin = Finished::default();
    let total = req.files.iter().filter(|f| f.measure).count();
    // What the cache will hold: every file still here, as known or as
    // measured now.  Files that are gone drop out.
    let mut keep: HashMap<PathBuf, Entry> = HashMap::with_capacity(req.files.len());
    let mut todo: Vec<usize> = Vec::new();
    let mut batch = Batch { total, ..Batch::default() };
    for (i, f) in req.files.iter().enumerate() {
        let known = cache.get(&f.path, f.size, &f.mtime).cloned();
        let entry = known.unwrap_or_else(|| Entry::new(f.size, &f.mtime));
        if f.measure {
            match &entry.features {
                Some(feat) => {
                    fin.cached += 1;
                    batch.features.push((f.path.clone(), Arc::new(feat.clone())));
                }
                None if req.mode == Mode::Full => todo.push(i),
                None => {}
            }
        }
        keep.insert(quality_cache::key(&req.root, &f.path), entry);
    }
    batch.done = fin.cached;
    emit(std::mem::replace(&mut batch, Batch { total, done: fin.cached, ..Batch::default() }));

    let mut stopped: Option<String> = None;
    if !todo.is_empty() {
        stopped = measure_all(&req, &todo, cancel, busy, &mut keep, &mut fin, emit);
    }
    let mut complete = stopped.is_none() && !cancel.load(Ordering::Relaxed) && req.mode == Mode::Full;
    let mut dups: Option<Vec<Vec<PathBuf>>> = None;
    if stopped.is_none() && !cancel.load(Ordering::Relaxed) {
        let t = Instant::now();
        let (groups, bytes, done) = find_duplicates(&req, &mut keep, cancel);
        fin.hashing = t.elapsed();
        fin.hashed_bytes = bytes;
        complete &= done;
        dups = Some(groups);
    }
    // Saved when anything changed, also after a cancel: entries stand alone.
    if let (Some(file), Mode::Full) = (&req.cache, req.mode) {
        if keep != cache.entries {
            let _ = QualityCache::save_file(file, &req.root, &keep);
        }
    }
    fin.complete = complete;
    fin.error = stopped;
    fin.elapsed = started.elapsed();
    let done = fin.cached + fin.measured + fin.failed;
    emit(Batch { total, done, duplicates: dups, finished: Some(fin.clone()), ..Batch::default() });
    fin
}

/// Measure `todo` across worker threads, emitting batches.  Returns why it
/// stopped early, if the folder went away.
fn measure_all(
    req: &Request,
    todo: &[usize],
    cancel: &AtomicBool,
    busy: &AtomicUsize,
    keep: &mut HashMap<PathBuf, Entry>,
    fin: &mut Finished,
    emit: &mut dyn FnMut(Batch),
) -> Option<String> {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (tx, rx) = crossbeam_channel::unbounded::<(usize, Result<Features, String>)>();
    let threads = req.threads.clamp(1, 16).min(todo.len());
    let total = req.files.iter().filter(|f| f.measure).count();
    let mut stopped = None;
    std::thread::scope(|s| {
        for _ in 0..threads {
            let tx = tx.clone();
            let (next, stop) = (&next, &stop);
            s.spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                        return;
                    }
                    // Step back while thumbnails are waited for (bounded, so
                    // the pass always moves on).
                    for _ in 0..10 {
                        if busy.load(Ordering::Relaxed) == 0 {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = todo.get(k) else { return };
                    let f = &req.files[i];
                    let r = quality::load(&f.path, f.file_type).map(|l| quality::measure(Plane::of(&l.plane), l.orientation, f.focus, l.source));
                    if tx.send((i, r)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(tx);
        let mut batch = Batch { total, ..Batch::default() };
        let mut last = Instant::now();
        let mut failures = 0usize;
        loop {
            let got = rx.recv_timeout(Duration::from_millis(BATCH_MS));
            let ended = matches!(got, Err(crossbeam_channel::RecvTimeoutError::Disconnected));
            if let Ok((i, r)) = got {
                let f = &req.files[i];
                match r {
                    Ok(feat) => {
                        failures = 0;
                        fin.measured += 1;
                        if let Some(e) = keep.get_mut(&quality_cache::key(&req.root, &f.path)) {
                            e.features = Some(feat.clone());
                        }
                        batch.features.push((f.path.clone(), Arc::new(feat)));
                    }
                    Err(e) => {
                        failures += 1;
                        fin.failed += 1;
                        batch.failed.push((f.path.clone(), e));
                        if failures >= FAILURE_RUN && stopped.is_none() {
                            if let Err(e) = std::fs::metadata(&req.root) {
                                stopped = Some(format!("{} can't be read: {e}", req.root.display()));
                                stop.store(true, Ordering::Relaxed);
                            }
                            failures = 0;
                        }
                    }
                }
            }
            let due = batch.features.len() + batch.failed.len() >= BATCH || last.elapsed() >= Duration::from_millis(BATCH_MS);
            if (due || ended) && (!batch.features.is_empty() || !batch.failed.is_empty()) {
                batch.done = fin.cached + fin.measured + fin.failed;
                emit(std::mem::replace(&mut batch, Batch { total, ..Batch::default() }));
                last = Instant::now();
            }
            if ended {
                break;
            }
        }
    });
    stopped
}

/// Identical files: (groups, bytes read, whether the check finished).
/// The reads are small and scattered, so on a share they are bound by
/// latency: they run across the pass's threads.
fn find_duplicates(req: &Request, keep: &mut HashMap<PathBuf, Entry>, cancel: &AtomicBool) -> (Vec<Vec<PathBuf>>, u64, bool) {
    let files: Vec<(FileType, u64)> = req.files.iter().map(|f| (f.file_type, f.size)).collect();
    let keys: Vec<PathBuf> = req.files.iter().map(|f| quality_cache::key(&req.root, &f.path)).collect();
    let read = req.mode == Mode::Full;
    // Same type and size, then the same capture as the header says (no
    // reads: on a body whose RAWs come in a few fixed sizes, most RAWs of
    // a day share their size with another).
    let buckets: Vec<Vec<usize>> =
        duplicates::size_buckets(&files).iter().flat_map(|b| duplicates::split_by(b, |i| Some(req.files[i].capture))).collect();
    let mut bytes = 0u64;
    // First 64 KB of every file sharing a type and size with another.
    let need: Vec<usize> = buckets.iter().flatten().copied().filter(|&i| read && keep[&keys[i]].head.is_none()).collect();
    for (i, d) in read_all(req, &need, cancel, |f| head_digest(&f.path, f.size)) {
        keep.get_mut(&keys[i]).expect("every file has an entry").head = Some(d);
        bytes += SAMPLE_BYTES.min(req.files[i].size);
    }
    if cancel.load(Ordering::Relaxed) {
        return (Vec::new(), bytes, false);
    }
    // Then the middle and end of those whose starts match.
    let same_head: Vec<Vec<usize>> = buckets.iter().flat_map(|b| duplicates::split_by(b, |i| keep[&keys[i]].head)).collect();
    let need: Vec<usize> = same_head.iter().flatten().copied().filter(|&i| read && keep[&keys[i]].samples.is_none()).collect();
    for (i, d) in read_all(req, &need, cancel, |f| sample_digest(&f.path, f.size)) {
        keep.get_mut(&keys[i]).expect("every file has an entry").samples = Some(d);
        bytes += (2 * SAMPLE_BYTES).min(req.files[i].size);
    }
    if cancel.load(Ordering::Relaxed) {
        return (Vec::new(), bytes, false);
    }
    let groups = same_head
        .iter()
        .flat_map(|g| duplicates::split_by(g, |i| keep[&keys[i]].samples))
        .map(|g| g.into_iter().map(|i| req.files[i].path.clone()).collect())
        .collect();
    (groups, bytes, true)
}

/// `digest` of each of `files` (indices into the request), across the
/// pass's threads; files that can't be read are left out.
fn read_all(req: &Request, files: &[usize], cancel: &AtomicBool, digest: impl Fn(&FileJob) -> io::Result<u64> + Sync) -> Vec<(usize, u64)> {
    let next = AtomicUsize::new(0);
    let out = parking_lot::Mutex::new(Vec::with_capacity(files.len()));
    std::thread::scope(|s| {
        for _ in 0..req.threads.clamp(1, 16).min(files.len()) {
            s.spawn(|| {
                while !cancel.load(Ordering::Relaxed) {
                    let Some(&i) = files.get(next.fetch_add(1, Ordering::Relaxed)) else { return };
                    if let Ok(d) = digest(&req.files[i]) {
                        out.lock().push((i, d));
                    }
                }
            });
        }
    });
    out.into_inner()
}

/// Digest of a file's size and first 64 KB, read with read-ahead off.
pub fn head_digest(path: &Path, size: u64) -> io::Result<u64> {
    digest_at(path, size, &[0])
}

/// Digest of 64 KB from the middle and the end of a file.
pub fn sample_digest(path: &Path, size: u64) -> io::Result<u64> {
    digest_at(path, size, &[size / 2, size.saturating_sub(SAMPLE_BYTES)])
}

fn digest_at(path: &Path, size: u64, offsets: &[u64]) -> io::Result<u64> {
    use std::io::{Seek, SeekFrom};
    let mut f = crate::exif::open_for_header(path)?;
    let mut h = Md5::new();
    h.update(size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE_BYTES as usize];
    for &at in offsets {
        f.seek(SeekFrom::Start(at))?;
        let n = read_full(&mut f, &mut buf)?;
        h.update(&buf[..n]);
    }
    let d = h.finalize();
    Ok(u64::from_le_bytes(d[..8].try_into().expect("16 bytes")))
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-analysis-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn job(path: &Path, measure: bool) -> FileJob {
        let meta = fs::metadata(path).unwrap();
        FileJob {
            path: path.to_path_buf(),
            file_type: FileType::from_path(path).unwrap(),
            size: meta.len(),
            mtime: MTime::from_metadata(&meta),
            measure,
            focus: None,
            capture: None,
        }
    }

    /// A JPEG with no preview inside (measured from its own image).
    fn jpeg(path: &Path, seed: u32) {
        let img = crate::imaging::quality::tests::texture(160, 120, seed);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90).encode(&img.d, 160, 120, jpeg_encoder::ColorType::Luma).unwrap();
        fs::write(path, out).unwrap();
    }

    fn run(req: Request) -> (Vec<Batch>, Finished) {
        let mut got = Vec::new();
        let fin = execute(req, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |b| got.push(b));
        (got, fin)
    }

    #[test]
    fn jobs_measure_one_file_per_shot_in_the_order_asked() {
        let mut c = Collection::new();
        let rec = |p: &str| {
            let p = PathBuf::from(p);
            let mut r = crate::model::record::PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), 5, MTime::from_unix(1, 0), None);
            if p.extension().is_some_and(|e| e == "ARW") {
                r.shot.focus = Some([100, 200]);
            }
            r
        };
        let raw = c.add(rec("/s/RAW/a.ARW"));
        let jpg = c.add(rec("/s/JPG/a.JPG"));
        let b = c.add(rec("/s/b.JPG"));
        let lone = c.add(rec("/s/c.ARW"));
        c.build_pairs();
        // The grid shows c then a (by its RAW id): those come first.
        let jobs = jobs(&c, &[lone, raw]);
        let names: Vec<(String, bool)> = jobs.iter().map(|j| (crate::util::file_name(&j.path), j.measure)).collect();
        assert_eq!(names, [("c.ARW".into(), true), ("a.JPG".into(), true), ("b.JPG".into(), true), ("a.ARW".into(), false)]);
        // The JPG takes its RAW's AF point when it has none of its own.
        assert!(jobs[1].focus.is_some());
        let _ = (jpg, b);
    }

    #[test]
    fn digests_tell_same_size_files_apart() {
        let d = scratch("digests");
        let size = 300_000;
        let mut a = vec![7u8; size];
        fs::write(d.join("a.bin"), &a).unwrap();
        fs::write(d.join("b.bin"), &a).unwrap();
        // Differs in the middle, then at the end: heads match, samples don't.
        a[size / 2 + 10] = 8;
        fs::write(d.join("c.bin"), &a).unwrap();
        a[size / 2 + 10] = 7;
        a[size - 1] = 9;
        fs::write(d.join("e.bin"), &a).unwrap();
        let hd = |n: &str| head_digest(&d.join(n), size as u64).unwrap();
        let sd = |n: &str| sample_digest(&d.join(n), size as u64).unwrap();
        assert_eq!(hd("a.bin"), hd("b.bin"));
        assert_eq!(hd("a.bin"), hd("c.bin"));
        assert_eq!(sd("a.bin"), sd("b.bin"));
        assert_ne!(sd("a.bin"), sd("c.bin"));
        assert_ne!(sd("a.bin"), sd("e.bin"));
        // Small files: the samples overlap and cover them whole.
        fs::write(d.join("s.bin"), b"short").unwrap();
        fs::write(d.join("t.bin"), b"shorT").unwrap();
        assert_ne!(head_digest(&d.join("s.bin"), 5).unwrap(), head_digest(&d.join("t.bin"), 5).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pass_measures_finds_copies_caches_and_reruns_from_the_cache() {
        let d = scratch("pass");
        let cache = Some(d.join("cache/quality.bin"));
        jpeg(&d.join("a.jpg"), 1);
        jpeg(&d.join("b.jpg"), 2);
        fs::copy(d.join("a.jpg"), d.join("a copy.jpg")).unwrap();
        fs::write(d.join("x.ARW"), vec![0u8; 1000]).unwrap();
        let files = vec![job(&d.join("a.jpg"), true), job(&d.join("b.jpg"), true), job(&d.join("a copy.jpg"), true), job(&d.join("x.ARW"), false)];
        let req = |mode| Request { run: 1, root: d.clone(), files: files.clone(), mode, threads: 2, cache: cache.clone() };
        let (batches, fin) = run(req(Mode::Full));
        assert!(fin.complete, "{fin:?}");
        assert_eq!((fin.measured, fin.cached, fin.failed), (3, 0, 0));
        let measured: usize = batches.iter().map(|b| b.features.len()).sum();
        assert_eq!(measured, 3);
        let last = batches.last().unwrap();
        assert_eq!((last.done, last.total), (3, 3));
        let dups = last.duplicates.clone().unwrap();
        assert_eq!(dups.len(), 1);
        let mut g = dups[0].clone();
        g.sort();
        assert_eq!(g, vec![d.join("a copy.jpg"), d.join("a.jpg")]);
        assert!(fin.hashed_bytes > 0);
        // Again: everything from the cache, nothing read.
        let (batches, fin) = run(req(Mode::Full));
        assert_eq!((fin.measured, fin.cached, fin.hashed_bytes), (0, 3, 0));
        assert_eq!(batches[0].features.len(), 3, "cached results come first, in one batch");
        assert_eq!(batches.last().unwrap().duplicates.as_ref().map(Vec::len), Some(1));
        // Cached only reads nothing either way.
        let (_, fin) = run(req(Mode::CachedOnly));
        assert_eq!((fin.measured, fin.cached), (0, 3));
        // A changed file is measured again.
        jpeg(&d.join("b.jpg"), 3);
        let mut files2 = files.clone();
        files2[1] = job(&d.join("b.jpg"), true);
        let (_, fin) = run(Request { run: 2, root: d.clone(), files: files2, mode: Mode::Full, threads: 2, cache: cache.clone() });
        assert_eq!((fin.measured, fin.cached), (1, 2));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unreadable_files_fail_alone_and_a_cancel_stops_the_pass() {
        let d = scratch("fail");
        jpeg(&d.join("ok.jpg"), 1);
        fs::write(d.join("bad.jpg"), b"not a jpeg at all").unwrap();
        let files = vec![job(&d.join("bad.jpg"), true), job(&d.join("ok.jpg"), true)];
        let (batches, fin) = run(Request { run: 1, root: d.clone(), files: files.clone(), mode: Mode::Full, threads: 1, cache: None });
        assert_eq!((fin.measured, fin.failed), (1, 1));
        assert!(fin.complete && fin.error.is_none());
        assert_eq!(batches.iter().map(|b| b.failed.len()).sum::<usize>(), 1);
        let cancel = AtomicBool::new(true);
        let fin = execute(Request { run: 2, root: d.clone(), files, mode: Mode::Full, threads: 1, cache: None }, &cancel, &AtomicUsize::new(0), &mut |_| {});
        assert!(!fin.complete);
        assert_eq!(fin.measured, 0);
        let _ = fs::remove_dir_all(&d);
    }
}
