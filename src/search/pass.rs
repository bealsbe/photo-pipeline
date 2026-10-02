//! The indexing pass (plan §5.2), modelled on `analysis`: one job per shot
//! (a pair's JPG, else the file), newest day first, sent to the helper in
//! batches; results stream back as [`Batch`]es; cancellable; it yields
//! while the grid waits for thumbnails; if the library stops answering it
//! stops and says so; if the card fails it stops and says what failed, and
//! nothing moves to the processor (decision 3).
//!
//! The pass owns no files: the engine plans it from the library index and
//! the path map ([`plan`]) and writes what comes back.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use super::index::{Key, PathMap, Vectors};
use super::{PassKind, Stopped};
use crate::ml::client::{Embedder, Embedding, HelperError};
use crate::model::library_index::LibraryIndex;
use crate::model::record::{FileType, MTime};

/// Photos per request to the helper.
pub const BATCH: usize = 32;
/// Failures in a row after which the library is checked for being there.
const FAILURE_RUN: usize = 8;
/// Results are emitted at least this often.
const EMIT_MS: u64 = 250;

/// One shot to index.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The file embedded: the pair's JPG, else the file.
    pub path: PathBuf,
    pub file_type: FileType,
    /// Relative to the library root (the path map's key).
    pub rel: PathBuf,
    pub size: u64,
    pub mtime: MTime,
    pub day: NaiveDate,
}

/// What a check of the library against the index found.
#[derive(Debug, Default)]
pub struct Plan {
    /// Shots to read and embed, newest day first.
    pub jobs: Vec<Job>,
    /// Shots whose file changed path but not content would be found here
    /// after their head is read; the pass finds them when the helper
    /// returns a known key (one preview read, one inference, no new row).
    /// Shots already indexed and unchanged.
    pub unchanged: usize,
    /// Shots in the library.
    pub total: usize,
}

/// Compare the library with the path map and the vectors: every shot
/// whose representative file is not mapped unchanged to a key the
/// vectors hold becomes a job.  Days come newest first.
pub fn plan(index: &LibraryIndex, paths: &PathMap, vectors: &Vectors) -> Plan {
    let mut out = Plan::default();
    for day in index.days.values().rev() {
        for shot in day.shots() {
            let rep = shot.iter().find(|&&i| day.files[i].file_type == FileType::Jpg).or(shot.first());
            let Some(&i) = rep else { continue };
            let fe = &day.files[i];
            out.total += 1;
            let rel = rel_path(&index.root, &fe.path);
            if paths.key_for(&rel, fe.size, &fe.mtime).is_some_and(|k| vectors.contains(&k)) {
                out.unchanged += 1;
                continue;
            }
            out.jobs.push(Job { path: fe.path.clone(), file_type: fe.file_type, rel, size: fe.size, mtime: fe.mtime, day: day.date });
        }
    }
    out
}

/// A path relative to the library root (absolute when outside it).
pub fn rel_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).map(Path::to_path_buf).unwrap_or_else(|_| path.to_path_buf())
}

pub struct Request {
    pub run: u64,
    pub kind: PassKind,
    pub root: PathBuf,
    pub jobs: Vec<Job>,
    /// Shots in the library, for the progress line.
    pub total: usize,
}

/// Part of a pass's results.
#[derive(Debug, Default)]
pub struct Batch {
    pub embedded: Vec<(Job, Embedding)>,
    pub failed: Vec<(Job, String)>,
    /// Shots done (embedded or failed) plus those already indexed, of
    /// `total`.
    pub done: usize,
    pub total: usize,
    pub oldest_day: Option<NaiveDate>,
    /// The helper stopped on the processor and was started again.
    pub restarted: bool,
    pub finished: Option<Finished>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finished {
    /// Every job was embedded or failed on its own.
    pub complete: bool,
    pub stopped: Option<Stopped>,
    pub embedded: usize,
    pub failed: usize,
    pub elapsed: Duration,
}

/// A running pass.
pub struct Handle {
    cancel: std::sync::Arc<AtomicBool>,
    busy: std::sync::Arc<AtomicUsize>,
}

impl Handle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Thumbnails the grid is waiting for: the pass steps back while any
    /// are.
    pub fn set_busy(&self, pending: usize) {
        self.busy.store(pending, Ordering::Relaxed);
    }
}

/// Run the pass on its own thread, handing batches to `emit`.
pub fn start(req: Request, mut embedder: Box<dyn Embedder>, already: usize, emit: impl FnMut(Batch) + Send + 'static) -> Handle {
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let busy = std::sync::Arc::new(AtomicUsize::new(0));
    let (c, b) = (cancel.clone(), busy.clone());
    let mut emit = emit;
    std::thread::Builder::new()
        .name("search-pass".into())
        .spawn(move || {
            execute(req, embedder.as_mut(), already, &c, &b, &mut emit);
        })
        .expect("spawn search-pass");
    Handle { cancel, busy }
}

/// The pass itself, on the calling thread.  `already` is how many shots
/// the index held before it, counted into `done`.
pub fn execute(req: Request, embedder: &mut dyn Embedder, already: usize, cancel: &AtomicBool, busy: &AtomicUsize, emit: &mut dyn FnMut(Batch)) -> Finished {
    let started = Instant::now();
    let mut fin = Finished::default();
    let mut batch = Batch { total: req.total, done: already, ..Batch::default() };
    let mut last = Instant::now();
    let mut failures = 0usize;
    let mut stopped: Option<Stopped> = None;
    let mut i = 0;
    while i < req.jobs.len() {
        if cancel.load(Ordering::Relaxed) {
            stopped = Some(Stopped::Cancelled);
            break;
        }
        // Step back while thumbnails are waited for (bounded, so the pass
        // always moves on).
        for _ in 0..10 {
            if busy.load(Ordering::Relaxed) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let chunk = &req.jobs[i..(i + BATCH).min(req.jobs.len())];
        let files: Vec<(PathBuf, FileType)> = chunk.iter().map(|j| (j.path.clone(), j.file_type)).collect();
        let results = match embedder.embed_images(&files) {
            Ok(r) => r,
            Err(HelperError::Restarted) => {
                batch.restarted = true;
                continue;
            }
            Err(HelperError::Card(f)) => {
                stopped = Some(Stopped::Card(f));
                break;
            }
            Err(HelperError::Broken(why)) => {
                stopped = Some(Stopped::LibraryAway { share: String::new(), message: format!("the search helper stopped: {why}") });
                break;
            }
        };
        for (job, r) in chunk.iter().zip(results) {
            match r.result {
                Ok(e) => {
                    failures = 0;
                    fin.embedded += 1;
                    batch.embedded.push((job.clone(), e));
                }
                Err(e) => {
                    failures += 1;
                    fin.failed += 1;
                    batch.failed.push((job.clone(), e.message));
                    if failures >= FAILURE_RUN && stopped.is_none() {
                        if let Err(err) = std::fs::metadata(&req.root) {
                            let why = crate::ops::netfs::describe_errno(err.raw_os_error()).unwrap_or("it can't be read");
                            stopped = Some(Stopped::LibraryAway { share: req.root.display().to_string(), message: why.to_string() });
                        }
                        failures = 0;
                    }
                }
            }
            batch.done += 1;
            batch.oldest_day = Some(job.day);
        }
        i += chunk.len();
        if stopped.is_some() {
            break;
        }
        if batch.embedded.len() + batch.failed.len() >= BATCH || last.elapsed() >= Duration::from_millis(EMIT_MS) {
            let done = batch.done;
            let oldest = batch.oldest_day;
            emit(std::mem::replace(&mut batch, Batch { total: req.total, done, oldest_day: oldest, ..Batch::default() }));
            last = Instant::now();
        }
    }
    fin.complete = stopped.is_none();
    fin.stopped = stopped;
    fin.elapsed = started.elapsed();
    batch.finished = Some(fin.clone());
    emit(batch);
    fin
}

/// Keys the vectors already hold among a batch's results: their rows need
/// no writing, only the path map.
pub fn known_keys(vectors: &Vectors, batch: &Batch) -> Vec<Key> {
    batch.embedded.iter().filter(|(_, e)| vectors.contains(&e.key)).map(|(_, e)| e.key).collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::ml::client::{FileError, ImageResult};
    use crate::ml::{CardFailure, Device, SelfCheck};
    use crate::model::record::ShotMeta;

    /// An embedder that makes a vector from the path's bytes and can fail.
    pub struct Fake {
        pub device: Device,
        pub calls: usize,
        pub fail_paths_containing: Option<String>,
        pub card_fails_after: Option<usize>,
        pub restart_once: bool,
    }

    impl Fake {
        pub fn new(device: Device) -> Fake {
            Fake { device, calls: 0, fail_paths_containing: None, card_fails_after: None, restart_once: false }
        }

        pub fn vector_for(path: &Path) -> Vec<f32> {
            let mut v: Vec<f32> = (0..8).map(|i| ((path.as_os_str().len() * (i + 3)) % 17) as f32 - 8.0).collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            v.iter_mut().for_each(|x| *x /= n);
            v
        }
    }

    impl Embedder for Fake {
        fn device(&self) -> Device {
            self.device
        }
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            (Some("Fake card".into()), SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 })
        }
        fn embed_images(&mut self, files: &[(PathBuf, FileType)]) -> Result<Vec<ImageResult>, HelperError> {
            self.calls += 1;
            if self.card_fails_after.is_some_and(|n| self.calls > n) {
                return Err(HelperError::Card(CardFailure::Crashed));
            }
            if self.restart_once {
                self.restart_once = false;
                return Err(HelperError::Restarted);
            }
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let bad = self.fail_paths_containing.as_ref().is_some_and(|s| p.to_string_lossy().contains(s.as_str()));
                    ImageResult {
                        path: p.clone(),
                        result: if bad {
                            Err(FileError { message: "can't be read".into(), errno: Some(libc::EIO) })
                        } else {
                            Ok(Embedding { key: Key::of(1, p.as_os_str().as_encoded_bytes()), capture: None, meta: ShotMeta::default(), accidental: false, vector: Fake::vector_for(p) })
                        },
                    }
                })
                .collect())
        }
        fn encode_text(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Ok(texts.iter().map(|t| Fake::vector_for(Path::new(t))).collect())
        }
    }

    pub fn job(n: usize, day: NaiveDate) -> Job {
        let path = PathBuf::from(format!("/mnt/nas/lib/{day}/JPG/{n:04}.JPG"));
        Job { rel: path.strip_prefix("/mnt/nas/lib").unwrap().to_path_buf(), path, file_type: FileType::Jpg, size: 100 + n as u64, mtime: MTime::from_unix(1_700_000_000, 0), day }
    }

    fn run(req: Request, fake: &mut Fake) -> (Vec<Batch>, Finished) {
        let mut got = Vec::new();
        let fin = execute(req, fake, 0, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |b| got.push(b));
        (got, fin)
    }

    #[test]
    fn a_pass_embeds_every_job_in_batches_and_counts_failures_alone() {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let jobs: Vec<Job> = (0..70).map(|n| job(n, day)).collect();
        let mut fake = Fake::new(Device::Gpu);
        fake.fail_paths_containing = Some("0007".into());
        let (batches, fin) = run(Request { run: 1, kind: PassKind::FirstIndex, root: "/".into(), jobs, total: 70 }, &mut fake);
        assert!(fin.complete && fin.stopped.is_none(), "{fin:?}");
        assert_eq!((fin.embedded, fin.failed), (69, 1));
        assert_eq!(fake.calls, 3, "32 + 32 + 6");
        let last = batches.last().unwrap();
        assert_eq!((last.done, last.total), (70, 70));
        assert_eq!(last.oldest_day, Some(day));
        assert!(last.finished.is_some());
    }

    #[test]
    fn a_card_failure_stops_the_pass_where_it_is() {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let jobs: Vec<Job> = (0..100).map(|n| job(n, day)).collect();
        let mut fake = Fake::new(Device::Gpu);
        fake.card_fails_after = Some(1);
        let (batches, fin) = run(Request { run: 1, kind: PassKind::FirstIndex, root: "/".into(), jobs, total: 100 }, &mut fake);
        assert!(!fin.complete);
        assert_eq!(fin.stopped, Some(Stopped::Card(CardFailure::Crashed)));
        assert_eq!(fin.embedded, 32, "what was done is kept");
        assert_eq!(fake.calls, 2, "nothing more was asked for after the failure");
        assert_eq!(batches.last().unwrap().done, 32);
    }

    #[test]
    fn a_restart_on_the_processor_repeats_the_batch_and_a_cancel_stops() {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let jobs: Vec<Job> = (0..10).map(|n| job(n, day)).collect();
        let mut fake = Fake::new(Device::Cpu);
        fake.restart_once = true;
        let (batches, fin) = run(Request { run: 1, kind: PassKind::NewExports, root: "/".into(), jobs: jobs.clone(), total: 10 }, &mut fake);
        assert!(fin.complete && fin.embedded == 10);
        assert!(batches.iter().any(|b| b.restarted));
        let cancel = AtomicBool::new(true);
        let fin = execute(Request { run: 2, kind: PassKind::NewExports, root: "/".into(), jobs, total: 10 }, &mut Fake::new(Device::Cpu), 0, &cancel, &AtomicUsize::new(0), &mut |_| {});
        assert_eq!(fin.stopped, Some(Stopped::Cancelled));
        assert_eq!(fin.embedded, 0);
    }
}
