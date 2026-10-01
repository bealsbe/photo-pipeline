//! Background sidecar saving that doesn't lose marks when a folder goes away.
//!
//! - **One at a time, in order**, on the saver's own thread: on a network
//!   share one save takes 10–300 ms, which held up every P or M.  A save of
//!   a sidecar that already has one waiting replaces it: only the newest
//!   contents matter.
//! - **A failed save is kept** (the newest contents per sidecar) and written
//!   again by [`Saver::retry`], so marks made while a share is away reach it
//!   when it's back.  [`Saver::read`] returns what is waiting, so a folder
//!   opened meanwhile shows its newest marks, not the sidecar on disk.
//! - **A failed save is also copied to this computer**
//!   (`~/.local/share/photo-pipeline/unsaved/`), so quitting, or a crash,
//!   while the share is away loses nothing.  The next start writes the copy
//!   to its folder, unless that folder's sidecar changed in the meantime
//!   (the Python app, another machine): then the folder's version stays and
//!   the copy is moved to `unsaved/kept/`, and the app says so.
//! - [`Saver::flush`] waits, for a bounded time, until everything queued has
//!   been tried; a write stuck on a dead mount can't hang the caller.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};

use super::sidecar::{Kind, atomic_write_text};

/// How long [`Saver::flush`] waits.
pub const FLUSH_WAIT: Duration = Duration::from_secs(2);
/// How long dropping the saver waits for its thread before keeping what's
/// left on this computer and going.
const DROP_WAIT: Duration = Duration::from_secs(3);
/// Sidecar kinds the journal knows by name.
const KINDS: [Kind; 3] = [Kind::Prune, Kind::Export, Kind::Crops];
/// Digest standing for "no sidecar file".
const ABSENT: &str = "absent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveError {
    pub errno: Option<i32>,
    pub message: String,
}

impl SaveError {
    fn from_io(e: &io::Error) -> Self {
        Self { errno: e.raw_os_error(), message: e.to_string() }
    }
}

/// A sidecar whose newest contents aren't on disk after at least one try.
#[derive(Debug, Clone)]
pub struct Unsaved {
    pub folder: PathBuf,
    pub kind: Kind,
    pub error: SaveError,
    /// A copy is kept on this computer (it survives quitting).
    pub kept_locally: bool,
    /// Carried over from an earlier session.
    pub from_earlier: bool,
}

/// Sent from the saver's thread when something changed.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Every save not on disk yet after a failed try, by folder.
    pub unsaved: Vec<Unsaved>,
    /// Saves that had failed and are now written.
    pub saved: Vec<(PathBuf, Kind)>,
    /// Copies from an earlier session not written because the folder's
    /// sidecar changed since; each is kept at the path given.
    pub set_aside: Vec<(PathBuf, Kind, PathBuf)>,
}

type WriteFn = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

#[derive(Clone)]
enum Payload {
    /// A sidecar's new text, or `None` to remove it.
    Text { file: Arc<str>, text: Option<Arc<str>> },
    /// Written by a closure: can be retried, not read back or kept locally.
    #[allow(dead_code)] // through `Saver::save`; the app's sidecars are text
    Func(WriteFn),
}

type Key = (PathBuf, Kind);

struct Entry {
    payload: Payload,
    generation: u64,
    error: Option<SaveError>,
    queued: bool,
    kept_locally: bool,
    /// Restored from the journal: written only over the sidecar the app last
    /// knew (its digest; `None`: not known).
    restored: Option<Option<String>>,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    order: VecDeque<Key>,
    busy: Option<Key>,
    /// When the write in `busy` started.
    busy_since: Option<Instant>,
    stop: bool,
    exited: bool,
    generation: u64,
    /// Digest of each sidecar on disk, as last written or read.
    known: HashMap<PathBuf, String>,
}

impl State {
    fn report(&self, saved: Vec<(PathBuf, Kind)>, set_aside: Vec<(PathBuf, Kind, PathBuf)>) -> Report {
        let mut unsaved: Vec<Unsaved> = self
            .entries
            .iter()
            .filter_map(|((folder, kind), e)| {
                let error = e.error.clone()?;
                Some(Unsaved { folder: folder.clone(), kind: *kind, error, kept_locally: e.kept_locally, from_earlier: e.restored.is_some() })
            })
            .collect();
        unsaved.sort_by(|a, b| a.folder.cmp(&b.folder).then_with(|| a.kind.label().cmp(b.kind.label())));
        Report { unsaved, saved, set_aside }
    }

    fn enqueue(&mut self, key: Key, front: bool) {
        if let Some(e) = self.entries.get_mut(&key) {
            if e.queued {
                return;
            }
            e.queued = true;
            if front {
                self.order.push_front(key);
            } else {
                self.order.push_back(key);
            }
        }
    }
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a save is queued or finished, or the saver stops.
    changed: Condvar,
    journal: Option<Journal>,
}

fn digest(bytes: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn text_digest(text: Option<&str>) -> String {
    text.map(|t| digest(t.as_bytes())).unwrap_or_else(|| ABSENT.into())
}

/// Write or remove a sidecar.
fn write_text(folder: &Path, target: &Path, text: Option<&str>) -> io::Result<()> {
    match text {
        Some(t) => atomic_write_text(target, t),
        None => match fs::remove_file(target) {
            Ok(()) => Ok(()),
            // Nothing to remove — if the folder is really there; it may be
            // on a share that isn't mounted.
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::metadata(folder).map(|_| ()),
            Err(e) => Err(e),
        },
    }
}

enum Outcome {
    /// Done; the digest of what the sidecar now holds (`None` for a closure).
    Written(Option<String>),
    /// A restored copy the folder's sidecar has moved on from.
    Superseded,
    Failed(SaveError),
}

fn attempt(folder: &Path, payload: &Payload, restored: Option<&Option<String>>) -> Outcome {
    match payload {
        Payload::Func(f) => match f() {
            Ok(()) => Outcome::Written(None),
            Err(e) => Outcome::Failed(SaveError::from_io(&e)),
        },
        Payload::Text { file, text } => {
            let target = folder.join(&**file);
            let want = text_digest(text.as_deref());
            if let Some(base) = restored {
                let disk = match fs::read(&target) {
                    Ok(b) => digest(&b),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::metadata(folder) {
                        Ok(_) => ABSENT.to_string(),
                        Err(e) => return Outcome::Failed(SaveError::from_io(&e)),
                    },
                    Err(e) => return Outcome::Failed(SaveError::from_io(&e)),
                };
                if disk == want {
                    return Outcome::Written(Some(want));
                }
                if base.as_ref().is_some_and(|b| *b != disk) {
                    return Outcome::Superseded;
                }
            }
            match write_text(folder, &target, text.as_deref()) {
                Ok(()) => Outcome::Written(Some(want)),
                Err(e) => Outcome::Failed(SaveError::from_io(&e)),
            }
        }
    }
}

// ── the journal: failed saves kept on this computer ─────────────────────────

struct Journal {
    dir: PathBuf,
    /// Sidecars with a copy here → the generation of the contents copied.
    files: Mutex<HashMap<PathBuf, u64>>,
}

struct Restored {
    folder: PathBuf,
    kind: Kind,
    file: String,
    text: Option<String>,
    base: Option<String>,
}

impl Journal {
    fn path_for(&self, target: &Path) -> PathBuf {
        self.dir.join(format!("{}.json", digest(target.as_os_str().as_encoded_bytes())))
    }

    /// Keep `text` as the newest contents for `folder/file` (unless a newer
    /// generation is already kept).
    fn put(&self, folder: &Path, kind: Kind, file: &str, text: Option<&str>, base: Option<&str>, generation: u64) -> io::Result<()> {
        let target = folder.join(file);
        let mut files = self.files.lock();
        if files.get(&target).is_some_and(|&g| g >= generation) {
            return Ok(());
        }
        let folder_str = folder.to_str().ok_or_else(|| io::Error::other("the folder's name can't be stored"))?;
        let doc = json!({
            "version": 1,
            "folder": folder_str,
            "kind": kind.label(),
            "file": file,
            "text": text,
            "base": base,
            "kept_at": chrono::Local::now().to_rfc3339(),
        });
        fs::create_dir_all(&self.dir)?;
        atomic_write_text(&self.path_for(&target), &serde_json::to_string_pretty(&doc).unwrap_or_default())?;
        files.insert(target, generation);
        Ok(())
    }

    /// The contents of generation `generation` reached the folder.
    fn clear(&self, target: &Path, generation: u64) {
        let mut files = self.files.lock();
        if files.get(target).is_some_and(|&g| g <= generation) {
            let _ = fs::remove_file(self.path_for(target));
            files.remove(target);
        }
    }

    /// Move a copy that won't be written to `kept/`.
    fn set_aside(&self, target: &Path) -> Option<PathBuf> {
        let mut files = self.files.lock();
        files.remove(target)?;
        let from = self.path_for(target);
        let kept = self.dir.join("kept");
        fs::create_dir_all(&kept).ok()?;
        let stem = from.file_stem()?.to_string_lossy().into_owned();
        let to = kept.join(format!("{stem}-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        fs::rename(&from, &to).ok()?;
        Some(to)
    }

    fn load(&self) -> Vec<Restored> {
        let Ok(rd) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(doc) = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
            let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let (Some(folder), Some(label), Some(file)) = (s("folder"), s("kind"), s("file")) else { continue };
            let Some(kind) = KINDS.into_iter().find(|k| k.label() == label) else { continue };
            // A copy under another name (written by hand?) would never be cleared.
            if self.path_for(&Path::new(&folder).join(&file)) != path {
                continue;
            }
            out.push(Restored { folder: PathBuf::from(folder), kind, file, text: s("text"), base: s("base") });
        }
        out.sort_by(|a, b| a.folder.cmp(&b.folder));
        out
    }
}

/// Where failed saves are kept: `~/.local/share/photo-pipeline/unsaved`.
pub fn default_journal_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("photo-pipeline").join("unsaved"))
}

// ── the saver ───────────────────────────────────────────────────────────────

/// Writes sidecars on a background thread (see the module docs).
pub struct Saver {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    /// Keeps failed saves in [`default_journal_dir`].  `on_report` runs on
    /// the saver's thread whenever saves start or stop failing.
    pub fn new(on_report: impl Fn(Report) + Send + 'static) -> Self {
        Self::with_journal(default_journal_dir(), on_report)
    }

    /// With the journal in `dir` (`None`: failed saves live only in memory).
    /// Copies left there by an earlier session are queued straight away.
    pub fn with_journal(dir: Option<PathBuf>, on_report: impl Fn(Report) + Send + 'static) -> Self {
        let journal = dir.map(|dir| Journal { dir, files: Mutex::new(HashMap::new()) });
        let mut state = State::default();
        if let Some(j) = &journal {
            let mut files = j.files.lock();
            for r in j.load() {
                state.generation += 1;
                files.insert(r.folder.join(&r.file), state.generation);
                let key = (r.folder, r.kind);
                let error = SaveError { errno: None, message: "kept on this computer by an earlier session".into() };
                state.entries.insert(
                    key.clone(),
                    Entry {
                        payload: Payload::Text { file: r.file.into(), text: r.text.map(Into::into) },
                        generation: state.generation,
                        error: Some(error),
                        queued: true,
                        kept_locally: true,
                        restored: Some(r.base),
                    },
                );
                state.order.push_back(key);
            }
        }
        let shared = Arc::new(Shared { state: Mutex::new(state), changed: Condvar::new(), journal });
        let worker = shared.clone();
        let thread = std::thread::Builder::new().name("sidecar-saver".into()).spawn(move || run(&worker, &on_report)).ok();
        Self { shared, thread }
    }

    /// Queue `write` as the new contents of `folder`'s `kind` sidecar.  It
    /// may run more than once (a failed save is tried again).  Prefer
    /// [`Saver::save_text`]: only text can be read back and kept locally.
    #[allow(dead_code)]
    pub fn save(&self, folder: PathBuf, kind: Kind, write: impl Fn() -> io::Result<()> + Send + Sync + 'static) {
        self.put(folder, kind, Payload::Func(Arc::new(write)));
    }

    /// Queue `text` as the new contents of `folder/file`; `None` removes it.
    pub fn save_text(&self, folder: PathBuf, kind: Kind, file: &str, text: Option<String>) {
        self.put(folder, kind, Payload::Text { file: file.into(), text: text.map(Into::into) });
    }

    fn put(&self, folder: PathBuf, kind: Kind, payload: Payload) {
        if self.thread.is_none() {
            // No thread (spawn failed): write here, as before.
            let _ = attempt(&folder, &payload, None);
            return;
        }
        let mut s = self.shared.state.lock();
        s.generation += 1;
        let generation = s.generation;
        let key = (folder, kind);
        match s.entries.get_mut(&key) {
            Some(e) => {
                e.payload = payload;
                e.generation = generation;
                e.restored = None;
            }
            None => {
                let e = Entry { payload, generation, error: None, queued: false, kept_locally: false, restored: None };
                s.entries.insert(key.clone(), e);
            }
        }
        s.enqueue(key, false);
        drop(s);
        self.shared.changed.notify_all();
    }

    /// Queue every failed save again.
    pub fn retry(&self) {
        self.requeue(|_| true, false);
    }

    fn requeue(&self, which: impl Fn(&Path) -> bool, front: bool) {
        let mut s = self.shared.state.lock();
        let busy = s.busy.clone();
        let keys: Vec<Key> = s
            .entries
            .iter()
            .filter(|(k, e)| e.error.is_some() && !e.queued && Some(*k) != busy.as_ref() && which(&k.0))
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            s.enqueue(k, front);
        }
        drop(s);
        self.shared.changed.notify_all();
    }

    /// Wait until everything queued has been tried, at most [`FLUSH_WAIT`].
    /// False if the time ran out (a write is stuck).
    pub fn flush(&self) -> bool {
        self.flush_for(FLUSH_WAIT)
    }

    pub fn flush_for(&self, wait: Duration) -> bool {
        self.wait_for(wait, |_| true)
    }

    fn wait_for(&self, wait: Duration, which: impl Fn(&Path) -> bool) -> bool {
        if self.thread.is_none() {
            return true;
        }
        let deadline = Instant::now() + wait;
        let mut s = self.shared.state.lock();
        loop {
            let waiting = s.entries.iter().any(|(k, e)| e.queued && which(&k.0)) || s.busy.as_ref().is_some_and(|k| which(&k.0));
            if !waiting {
                return true;
            }
            if self.shared.changed.wait_until(&mut s, deadline).timed_out() {
                return false;
            }
        }
    }

    /// Before reading `folder`'s sidecars: try its failed saves again first
    /// and wait (bounded) for them, so what [`Saver::read`] returns is settled.
    pub fn settle(&self, folder: &Path) -> bool {
        self.requeue(|f| f == folder, true);
        self.wait_for(FLUSH_WAIT, |f| f == folder)
    }

    /// The newest contents of `folder`'s `kind` sidecar (`file`): what is
    /// waiting to be saved, else what is on disk.  `Ok(None)`: no sidecar.
    pub fn read(&self, folder: &Path, kind: Kind, file: &str) -> io::Result<Option<String>> {
        let key = (folder.to_path_buf(), kind);
        let waiting = match self.shared.state.lock().entries.get(&key) {
            Some(Entry { payload: Payload::Text { text, .. }, .. }) => Some(text.as_deref().map(str::to_string)),
            _ => None,
        };
        if let Some(text) = waiting {
            return Ok(text);
        }
        let target = folder.join(file);
        let text = match fs::read_to_string(&target) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        self.shared.state.lock().known.insert(target, text_digest(text.as_deref()));
        Ok(text)
    }

    /// The folder of a write that has been running for at least `at_least`
    /// (on a hung mount, a write can block for minutes before it fails).
    pub fn stuck(&self, at_least: Duration) -> Option<PathBuf> {
        let s = self.shared.state.lock();
        let since = s.busy_since?;
        (since.elapsed() >= at_least).then(|| s.busy.as_ref().map(|k| k.0.clone())).flatten()
    }

    /// Saves that failed and aren't written yet.
    pub fn unsaved(&self) -> Vec<Unsaved> {
        self.shared.state.lock().report(Vec::new(), Vec::new()).unsaved
    }

    /// Anything not on disk yet: waiting, being written, or failed.
    pub fn pending(&self) -> bool {
        !self.shared.state.lock().entries.is_empty()
    }

    /// Keep a copy of everything not on disk yet on this computer, now, from
    /// this thread: before quitting while saves fail or hang.  `Ok(n)`: n
    /// sidecars kept (0 if nothing was waiting).
    pub fn secure(&self) -> Result<usize, String> {
        let (items, closures) = {
            let s = self.shared.state.lock();
            let mut items = Vec::new();
            let mut closures = Vec::new();
            for ((folder, kind), e) in &s.entries {
                match &e.payload {
                    Payload::Text { file, text } => {
                        let base = s.known.get(&folder.join(&**file)).cloned();
                        items.push((folder.clone(), *kind, file.clone(), text.clone(), base, e.generation));
                    }
                    Payload::Func(_) => closures.push(kind.label()),
                }
            }
            (items, closures)
        };
        if items.is_empty() && closures.is_empty() {
            return Ok(0);
        }
        let Some(j) = &self.shared.journal else { return Err("there is no folder on this computer to keep them in".into()) };
        let mut kept = 0;
        let mut error = None;
        for (folder, kind, file, text, base, generation) in &items {
            match j.put(folder, *kind, file, text.as_deref(), base.as_deref(), *generation) {
                Ok(()) => {
                    kept += 1;
                    if let Some(e) = self.shared.state.lock().entries.get_mut(&(folder.clone(), *kind)) {
                        e.kept_locally = true;
                    }
                }
                Err(e) => error = Some(e.to_string()),
            }
        }
        if let Some(label) = closures.first() {
            error.get_or_insert_with(|| format!("{label} can't be kept on this computer"));
        }
        match error {
            Some(e) => Err(e),
            None => Ok(kept),
        }
    }
}

/// The saver thread: take the next queued save, write it, record the outcome.
fn run(shared: &Shared, on_report: &dyn Fn(Report)) {
    loop {
        let (key, generation, payload, restored) = {
            let mut s = shared.state.lock();
            loop {
                if let Some(key) = s.order.pop_front() {
                    let Some(e) = s.entries.get_mut(&key) else { continue };
                    if !e.queued {
                        continue;
                    }
                    e.queued = false;
                    let job = (key.clone(), e.generation, e.payload.clone(), e.restored.clone());
                    s.busy = Some(key);
                    s.busy_since = Some(Instant::now());
                    break job;
                }
                if s.stop {
                    s.exited = true;
                    shared.changed.notify_all();
                    return;
                }
                shared.changed.wait(&mut s);
            }
        };
        let outcome = attempt(&key.0, &payload, restored.as_ref());
        finish(shared, on_report, key, generation, outcome);
    }
}

enum JournalOp {
    None,
    Clear(PathBuf, u64),
    SetAside(PathBuf),
    Put { file: Arc<str>, text: Option<Arc<str>>, base: Option<String>, generation: u64 },
}

fn finish(shared: &Shared, on_report: &dyn Fn(Report), key: Key, generation: u64, outcome: Outcome) {
    let mut saved = Vec::new();
    let mut changed = false;
    // Update the state; the journal's file work happens after, off the lock.
    let op = {
        let mut s = shared.state.lock();
        let current = s.entries.get(&key).is_some_and(|e| e.generation == generation);
        let target = |s: &State| match s.entries.get(&key).map(|e| &e.payload) {
            Some(Payload::Text { file, .. }) => Some(key.0.join(&**file)),
            _ => None,
        };
        match outcome {
            Outcome::Written(digest) => {
                let t = target(&*s);
                if let (Some(d), Some(t)) = (digest, &t) {
                    s.known.insert(t.clone(), d);
                }
                if current {
                    if s.entries.remove(&key).is_some_and(|e| e.error.is_some()) {
                        saved.push(key.clone());
                        changed = true;
                    }
                    t.map_or(JournalOp::None, |t| JournalOp::Clear(t, generation))
                } else {
                    JournalOp::None
                }
            }
            Outcome::Superseded => {
                let t = target(&*s);
                if current {
                    s.entries.remove(&key);
                }
                changed = true;
                t.map_or(JournalOp::None, JournalOp::SetAside)
            }
            Outcome::Failed(err) => {
                changed = true;
                let known = target(&*s).and_then(|t| s.known.get(&t).cloned());
                match s.entries.get_mut(&key) {
                    // The newest contents (a newer save may be waiting) are
                    // what to keep.
                    Some(e) => {
                        e.error = Some(err);
                        match &e.payload {
                            Payload::Text { file, text } => {
                                // A restored copy keeps the base it came with.
                                let base = match &e.restored {
                                    Some(b) => b.clone(),
                                    None => known,
                                };
                                JournalOp::Put { file: file.clone(), text: text.clone(), base, generation: e.generation }
                            }
                            Payload::Func(_) => JournalOp::None,
                        }
                    }
                    None => JournalOp::None,
                }
            }
        }
    };
    let mut set_aside = Vec::new();
    let mut kept = None;
    if let Some(j) = &shared.journal {
        match op {
            JournalOp::None => {}
            JournalOp::Clear(t, g) => j.clear(&t, g),
            JournalOp::SetAside(t) => {
                if let Some(p) = j.set_aside(&t) {
                    set_aside.push((key.0.clone(), key.1, p));
                }
            }
            JournalOp::Put { file, text, base, generation } => {
                kept = Some(j.put(&key.0, key.1, &file, text.as_deref(), base.as_deref(), generation).is_ok());
            }
        }
    }
    let report = {
        let mut s = shared.state.lock();
        if let (Some(k), Some(e)) = (kept, s.entries.get_mut(&key)) {
            e.kept_locally = k;
        }
        s.busy = None;
        s.busy_since = None;
        changed.then(|| s.report(saved, set_aside))
    };
    shared.changed.notify_all();
    if let Some(r) = report {
        on_report(r);
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        self.shared.state.lock().stop = true;
        self.shared.changed.notify_all();
        // The thread writes what is queued, then stops.  A write stuck on a
        // dead mount mustn't hold up quitting: after a while, keep what's
        // left on this computer and go.
        let deadline = Instant::now() + DROP_WAIT;
        let exited = {
            let mut s = self.shared.state.lock();
            while !s.exited {
                if self.shared.changed.wait_until(&mut s, deadline).timed_out() {
                    break;
                }
            }
            s.exited
        };
        if exited {
            let _ = thread.join();
        }
        if self.pending() {
            let _ = self.secure();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sidecar::{EXPORT_FILE, PRUNE_FILE, prune_text, read_export_marks, read_prune_marks, write_export_marks, write_prune_marks};
    use crossbeam_channel::{Receiver, unbounded};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-saver-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn saver(journal: Option<PathBuf>) -> (Saver, Receiver<Report>) {
        let (tx, rx) = unbounded();
        (Saver::with_journal(journal, move |r| drop(tx.send(r))), rx)
    }

    fn prune(folder: &Path, names: &[&str]) -> String {
        let paths: Vec<PathBuf> = names.iter().map(|n| folder.join(n)).collect();
        prune_text(folder, paths.iter().map(|p| p.as_path()))
    }

    fn next(rx: &Receiver<Report>) -> Report {
        rx.recv_timeout(Duration::from_secs(5)).expect("a report")
    }

    #[test]
    fn saver_writes_the_newest_contents_and_flushes() {
        let d = tmpdir("newest");
        let (saver, rx) = saver(None);
        for n in 1..=50 {
            let (folder, paths) = (d.clone(), vec![d.join(format!("IMG_{n:04}.JPG"))]);
            saver.save(d.clone(), Kind::Prune, move || write_prune_marks(&folder, paths.iter().map(|p| p.as_path())));
        }
        let (folder, paths) = (d.clone(), vec![d.join("A.JPG")]);
        saver.save(d.clone(), Kind::Export, move || write_export_marks(&folder, paths.iter().map(|p| p.as_path())));
        assert!(saver.flush());
        assert_eq!(read_prune_marks(&d), Some([d.join("IMG_0050.JPG")].into()));
        assert_eq!(read_export_marks(&d), Some([d.join("A.JPG")].into()));
        assert!(!saver.pending());
        assert!(rx.try_recv().is_err(), "nothing failed, nothing to report");
        // A failing save is reported and kept; dropping the saver writes
        // what's queued.
        let gone = d.join("missing-folder");
        let g = gone.clone();
        saver.save(gone, Kind::Crops, move || write_prune_marks(&g, std::iter::empty()));
        saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["LAST.JPG"])));
        drop(saver);
        assert_eq!(read_prune_marks(&d), Some([d.join("LAST.JPG")].into()));
        let r = next(&rx);
        assert_eq!(r.unsaved.len(), 1);
        assert_eq!(r.unsaved[0].kind, Kind::Crops);
        assert_eq!(r.unsaved[0].error.errno, Some(libc::ENOENT));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn failed_save_is_kept_retried_and_the_newest_wins() {
        let base = tmpdir("retry");
        let d = base.join("shoot");
        let (saver, rx) = saver(Some(base.join("journal")));
        // The folder is away: the save fails and is kept.
        saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["A.ARW"])));
        let r = next(&rx);
        assert_eq!(r.unsaved.len(), 1);
        assert!(r.unsaved[0].kept_locally);
        assert!(saver.pending());
        // Newer marks while it's away replace the kept contents.
        saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["A.ARW", "B.ARW"])));
        assert_eq!(next(&rx).unsaved.len(), 1);
        // Reading the folder's marks meanwhile gets the newest contents.
        let text = saver.read(&d, Kind::Prune, PRUNE_FILE).unwrap().unwrap();
        assert!(text.contains("B.ARW"));
        // Nothing is retried until asked.
        assert!(saver.flush());
        fs::create_dir_all(&d).unwrap();
        assert!(!d.join(PRUNE_FILE).exists());
        saver.retry();
        let r = next(&rx);
        assert!(r.unsaved.is_empty());
        assert_eq!(r.saved, vec![(d.clone(), Kind::Prune)]);
        assert_eq!(read_prune_marks(&d), Some([d.join("A.ARW"), d.join("B.ARW")].into()));
        assert!(!saver.pending());
        // The local copy went once it was saved.
        assert_eq!(fs::read_dir(base.join("journal")).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn removing_a_sidecar_from_a_missing_folder_fails() {
        let base = tmpdir("remove");
        let d = base.join("gone");
        let (saver, rx) = saver(None);
        saver.save_text(d.clone(), Kind::Crops, ".photo-pipeline-crops.json", None);
        assert_eq!(next(&rx).unsaved.len(), 1);
        fs::create_dir_all(&d).unwrap();
        saver.retry();
        assert!(next(&rx).unsaved.is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn flush_is_bounded_when_a_write_hangs() {
        let d = tmpdir("hang");
        let (saver, _rx) = saver(None);
        saver.save(d.clone(), Kind::Prune, || {
            std::thread::sleep(Duration::from_millis(800));
            Ok(())
        });
        let t = Instant::now();
        assert!(!saver.flush_for(Duration::from_millis(100)));
        assert!(t.elapsed() < Duration::from_millis(600));
        assert_eq!(saver.stuck(Duration::from_millis(50)), Some(d.clone()));
        assert_eq!(saver.stuck(Duration::from_secs(60)), None);
        assert!(saver.flush_for(Duration::from_secs(5)));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn kept_copies_are_written_by_the_next_session() {
        let base = tmpdir("restart");
        let d = base.join("shoot");
        let journal = base.join("journal");
        {
            let (saver, rx) = saver(Some(journal.clone()));
            saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["A.ARW"])));
            saver.save_text(d.clone(), Kind::Export, EXPORT_FILE, Some(r#"{"marked": ["B.JPG"]}"#.into()));
            next(&rx);
            // A save never tried (queued behind a stuck one, say) is kept too.
            assert!(saver.secure().is_ok());
        }
        assert_eq!(fs::read_dir(&journal).unwrap().count(), 2);
        // Next start, the folder is back: the copies are written.
        fs::create_dir_all(&d).unwrap();
        let (saver, rx) = saver(Some(journal.clone()));
        let mut saved = Vec::new();
        while saved.len() < 2 {
            saved.extend(next(&rx).saved);
        }
        assert!(saver.flush());
        assert_eq!(read_prune_marks(&d), Some([d.join("A.ARW")].into()));
        assert_eq!(read_export_marks(&d), Some([d.join("B.JPG")].into()));
        assert!(!saver.pending());
        assert_eq!(fs::read_dir(&journal).unwrap().filter(|e| e.as_ref().unwrap().path().is_file()).count(), 0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn kept_copy_does_not_overwrite_a_sidecar_changed_since() {
        let base = tmpdir("conflict");
        let d = base.join("shoot");
        let journal = base.join("journal");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(PRUNE_FILE), prune(&d, &["OLD.ARW"])).unwrap();
        {
            let (saver, rx) = saver(Some(journal.clone()));
            // The app read the folder's marks…
            assert!(saver.read(&d, Kind::Prune, PRUNE_FILE).unwrap().is_some());
            // …then the folder went away and a save failed.
            fs::rename(&d, base.join("moved")).unwrap();
            saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["MINE.ARW"])));
            next(&rx);
        }
        // Back, but another program changed the marks in between.
        fs::rename(base.join("moved"), &d).unwrap();
        fs::write(d.join(PRUNE_FILE), prune(&d, &["THEIRS.ARW"])).unwrap();
        let (saver, rx) = saver(Some(journal.clone()));
        let r = next(&rx);
        assert_eq!(r.set_aside.len(), 1);
        assert!(r.unsaved.is_empty());
        assert_eq!(read_prune_marks(&d), Some([d.join("THEIRS.ARW")].into()));
        let kept = &r.set_aside[0].2;
        assert!(kept.starts_with(journal.join("kept")));
        assert!(fs::read_to_string(kept).unwrap().contains("MINE.ARW"));
        assert!(!saver.pending());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn kept_copy_is_written_over_the_sidecar_it_was_based_on() {
        let base = tmpdir("based");
        let d = base.join("shoot");
        let journal = base.join("journal");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(PRUNE_FILE), prune(&d, &["OLD.ARW"])).unwrap();
        {
            let (saver, rx) = saver(Some(journal.clone()));
            assert!(saver.read(&d, Kind::Prune, PRUNE_FILE).unwrap().is_some());
            fs::rename(&d, base.join("moved")).unwrap();
            saver.save_text(d.clone(), Kind::Prune, PRUNE_FILE, Some(prune(&d, &["OLD.ARW", "NEW.ARW"])));
            next(&rx);
        }
        fs::rename(base.join("moved"), &d).unwrap();
        let (saver, rx) = saver(Some(journal.clone()));
        assert_eq!(next(&rx).saved.len(), 1);
        assert!(saver.flush());
        assert_eq!(read_prune_marks(&d), Some([d.join("OLD.ARW"), d.join("NEW.ARW")].into()));
        let _ = fs::remove_dir_all(&base);
    }
}
