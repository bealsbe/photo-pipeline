//! The indexing pass, modelled on `analysis`: one job per shot
//! (a pair's JPG, else the file), newest day first, sent to the helper in
//! batches; results stream back as [`Batch`]es; cancellable; it yields
//! while the grid waits for thumbnails; if the library stops answering it
//! stops and says so; if the card fails it stops and says what failed, and
//! nothing moves to the processor.
//!
//! The pass owns no files: the engine plans it from the library index and
//! the path map ([`plan`]) and writes what comes back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use super::index::{PathMap, Vectors};
use super::Stopped;
use crate::ml::client::{Embedder, Embedding, FaceSpot, FileError, HelperError, KnownFaces, Want};
use crate::model::library_index::LibraryIndex;
use crate::model::record::{FileType, MTime};
use crate::ops::netfs::{self, Failure};

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
    /// Faces already found in it, to recognise again with the chosen
    /// model (a face-model change); `None` in every other pass.
    pub known: Option<Vec<FaceSpot>>,
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
    /// Shots whose file the helper couldn't read, unchanged since: skipped
    /// until the file changes or the index is rebuilt.
    pub unreadable: usize,
    /// Shots in the library.
    pub total: usize,
}

/// Compare the library with the path map and the vectors: every shot
/// whose representative file is not mapped unchanged to a key the
/// vectors hold becomes a job, unless the file is remembered as
/// unreadable at its current size and mtime.  Days come newest first.
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
            if paths.is_failed(&rel, fe.size, &fe.mtime) {
                out.unreadable += 1;
                continue;
            }
            out.jobs.push(Job { path: fe.path.clone(), file_type: fe.file_type, rel, size: fe.size, mtime: fe.mtime, day: day.date, known: None });
        }
    }
    out
}

/// Whether a failure is the file's own (empty, damaged, not a photo, no
/// permission), so it is remembered and the file isn't read again until
/// it changes, rather than the share's (a network error, the file gone)
/// or the model's, which pass.  The helper reports a model failure for
/// every file of its chunk as `model: …` (`ml::worker`).
pub fn is_files_own(err: &FileError) -> bool {
    matches!(netfs::classify_errno(err.errno), Failure::Other) && !err.message.starts_with("model: ")
}

/// A path relative to the library root (absolute when outside it).
pub fn rel_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).map(Path::to_path_buf).unwrap_or_else(|_| path.to_path_buf())
}

pub struct Request {
    pub root: PathBuf,
    pub jobs: Vec<Job>,
    /// Shots in the library, for the progress line.
    pub total: usize,
    /// What the helper gives for each shot: the search vector, and the
    /// faces when they are on.
    pub want: Want,
}

/// Part of a pass's results.
#[derive(Debug, Default)]
pub struct Batch {
    pub embedded: Vec<(Job, Embedding)>,
    /// With why, and the errno when it was the OS's (see
    /// [`is_files_own`]).
    pub failed: Vec<(Job, FileError)>,
    /// Shots done (embedded or failed) plus those already indexed, of
    /// `total`.
    pub done: usize,
    pub total: usize,
    pub oldest_day: Option<NaiveDate>,
    /// The helper stopped on the processor and was started again.
    pub restarted: bool,
    /// The embedded shots were looked at for faces (the request asked):
    /// a shot with none found still counts as scanned.
    pub faces_scanned: bool,
    pub finished: Option<Finished>,
}

/// What a chunk of jobs asks the helper for: the pass's want, except
/// that jobs carrying known faces ask for those to be recognised again.
pub fn want_for(chunk: &[Job], want: &Want) -> Want {
    if chunk.iter().any(|j| j.known.is_some()) {
        Want::known(chunk.iter().filter_map(|j| j.known.as_ref().map(|k| KnownFaces { path: j.path.clone(), faces: k.clone() })).collect())
    } else {
        want.clone()
    }
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
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicUsize>,
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
pub fn start(req: Request, embedder: Arc<dyn Embedder>, already: usize, emit: impl FnMut(Batch) + Send + 'static) -> Handle {
    let cancel = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicUsize::new(0));
    let (c, b) = (cancel.clone(), busy.clone());
    let mut emit = emit;
    std::thread::Builder::new()
        .name("search-pass".into())
        .spawn(move || {
            execute(req, embedder.as_ref(), already, &c, &b, &mut emit);
        })
        .expect("spawn search-pass");
    Handle { cancel, busy }
}

/// The pass itself, on the calling thread.  `already` is how many shots
/// the index held before it, counted into `done`.
pub fn execute(req: Request, embedder: &dyn Embedder, already: usize, cancel: &AtomicBool, busy: &AtomicUsize, emit: &mut dyn FnMut(Batch)) -> Finished {
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
        let want = want_for(chunk, &req.want);
        batch.faces_scanned = want.faces != crate::ml::client::FaceAsk::None;
        let results = match embedder.embed_images(&files, &want) {
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
                    batch.failed.push((job.clone(), e));
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
            let scanned = batch.faces_scanned;
            emit(std::mem::replace(&mut batch, Batch { total: req.total, done, oldest_day: oldest, faces_scanned: scanned, ..Batch::default() }));
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

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::ml::client::{FileError, ImageResult};
    use crate::search::index::Key;
    use crate::ml::{CardFailure, Device, SelfCheck};
    use crate::model::record::ShotMeta;

    /// An embedder that makes a vector from the path's bytes and can fail.
    pub struct Fake {
        pub device: Device,
        pub calls: std::sync::atomic::AtomicUsize,
        pub fail_paths_containing: Option<String>,
        /// How those fail: the file's own trouble (no errno, the default)
        /// or an OS error such as `EIO`, as a share that went away gives.
        pub fail_errno: Option<i32>,
        pub card_fails_after: Option<usize>,
        pub restart_once: std::sync::atomic::AtomicBool,
        /// Every text encoded, for tests of the device rule.
        pub texts: parking_lot::Mutex<Vec<String>>,
        /// How long each image request takes (so a pass can be stopped
        /// part-way).
        pub delay: Duration,
    }

    impl Fake {
        pub fn new(device: Device) -> Fake {
            Fake {
                device,
                calls: AtomicUsize::new(0),
                fail_paths_containing: None,
                fail_errno: None,
                card_fails_after: None,
                restart_once: AtomicBool::new(false),
                texts: parking_lot::Mutex::new(Vec::new()),
                delay: Duration::ZERO,
            }
        }

        /// A unit vector from the path's bytes (64-d, pseudo-random), so
        /// different files get different vectors and a query for a file's
        /// own path finds it.
        pub fn vector_for(path: &Path) -> Vec<f32> {
            let mut h = crate::model::binfile::fnv1a(path.as_os_str().as_encoded_bytes());
            let mut v: Vec<f32> = (0..64)
                .map(|_| {
                    h ^= h << 13;
                    h ^= h >> 7;
                    h ^= h << 17;
                    ((h % 2001) as f32 / 1000.0) - 1.0
                })
                .collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            v.iter_mut().for_each(|x| *x /= n);
            v
        }

        /// The frame number in a `NNNN.JPG` name, for fake capture times.
        fn frame(path: &Path) -> i64 {
            crate::util::stem(path).chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
        }
    }

    impl Embedder for Fake {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            (Some(format!("Fake {}", self.device.label())), SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 })
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)], _want: &Want) -> Result<Vec<ImageResult>, HelperError> {
            let calls = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if self.card_fails_after.is_some_and(|n| calls > n) {
                return Err(HelperError::Card(CardFailure::Crashed));
            }
            if self.restart_once.swap(false, Ordering::Relaxed) {
                return Err(HelperError::Restarted);
            }
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let bad = self.fail_paths_containing.as_ref().is_some_and(|s| p.to_string_lossy().contains(s.as_str()));
                    ImageResult {
                        path: p.clone(),
                        result: if bad {
                            Err(FileError { message: "can't be read".into(), errno: self.fail_errno })
                        } else {
                            // Frames 200 ms apart on one camera: a burst, as
                            // the grid would find it.
                            let n = Fake::frame(p);
                            let base = chrono::NaiveDate::from_ymd_opt(2025, 3, 12).unwrap().and_hms_opt(14, 30, 0).unwrap();
                            let capture = base + chrono::Duration::milliseconds(n * 200);
                            let meta = ShotMeta { subsec_ms: Some(((n * 200) % 1000) as u16), sequence: None, focus: None, camera: 7 };
                            Ok(Embedding { key: Key::of(1, p.as_os_str().as_encoded_bytes()), capture: Some(capture), meta, accidental: false, vector: Fake::vector_for(p), faces: Vec::new() })
                        },
                    }
                })
                .collect())
        }
        /// A text's vector is the vector of the path it names (the query
        /// prompt's wrapping stripped), so tests can ask for one photo.
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            self.texts.lock().extend(texts.iter().cloned());
            Ok(texts
                .iter()
                .map(|t| {
                    let inner = t.strip_prefix("a photo of a ").and_then(|s| s.strip_suffix('.')).unwrap_or(t);
                    Fake::vector_for(Path::new(inner))
                })
                .collect())
        }
    }

    pub fn job(n: usize, day: NaiveDate) -> Job {
        let path = PathBuf::from(format!("/mnt/nas/lib/{day}/JPG/{n:04}.JPG"));
        Job { rel: path.strip_prefix("/mnt/nas/lib").unwrap().to_path_buf(), path, file_type: FileType::Jpg, size: 100 + n as u64, mtime: MTime::from_unix(1_700_000_000, 0), day, known: None }
    }

    fn run(req: Request, fake: &Fake) -> (Vec<Batch>, Finished) {
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
        let (batches, fin) = run(Request { root: "/".into(), jobs, total: 70, want: Want::search() }, &fake);
        assert!(fin.complete && fin.stopped.is_none(), "{fin:?}");
        assert_eq!((fin.embedded, fin.failed), (69, 1));
        assert_eq!(fake.calls.load(Ordering::Relaxed), 3, "32 + 32 + 6");
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
        let (batches, fin) = run(Request { root: "/".into(), jobs, total: 100, want: Want::search() }, &fake);
        assert!(!fin.complete);
        assert_eq!(fin.stopped, Some(Stopped::Card(CardFailure::Crashed)));
        assert_eq!(fin.embedded, 32, "what was done is kept");
        assert_eq!(fake.calls.load(Ordering::Relaxed), 2, "nothing more was asked for after the failure");
        assert_eq!(batches.last().unwrap().done, 32);
    }

    #[test]
    fn a_restart_on_the_processor_repeats_the_batch_and_a_cancel_stops() {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let jobs: Vec<Job> = (0..10).map(|n| job(n, day)).collect();
        let fake = Fake::new(Device::Cpu);
        fake.restart_once.store(true, Ordering::Relaxed);
        let (batches, fin) = run(Request { root: "/".into(), jobs: jobs.clone(), total: 10, want: Want::search() }, &fake);
        assert!(fin.complete && fin.embedded == 10);
        assert!(batches.iter().any(|b| b.restarted));
        let cancel = AtomicBool::new(true);
        let fin = execute(Request { root: "/".into(), jobs, total: 10, want: Want::search() }, &Fake::new(Device::Cpu), 0, &cancel, &AtomicUsize::new(0), &mut |_| {});
        assert_eq!(fin.stopped, Some(Stopped::Cancelled));
        assert_eq!(fin.embedded, 0);
    }

    /// Only a file's own trouble is remembered: a share that went away,
    /// a file that is gone and a model that failed all pass.
    #[test]
    fn a_failure_is_the_files_own_unless_the_share_or_the_model_failed() {
        let err = |message: &str, errno: Option<i32>| FileError { message: message.into(), errno };
        assert!(is_files_own(&err("not a readable JPEG", None)));
        assert!(is_files_own(&err("RAW: unsupported file format", None)));
        assert!(is_files_own(&err("Permission denied", Some(libc::EACCES))));
        assert!(!is_files_own(&err("Input/output error", Some(libc::EIO))));
        assert!(!is_files_own(&err("Host is down", Some(libc::EHOSTDOWN))));
        assert!(!is_files_own(&err("No such file or directory", Some(libc::ENOENT))));
        assert!(!is_files_own(&err("model: the card stopped", None)));
    }
}
