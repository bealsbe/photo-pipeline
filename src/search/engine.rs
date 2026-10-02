//! The one object the app holds for search.  Every method is a message to
//! the engine's own thread (`search-engine`), which owns the index files,
//! the ML helper and the running pass; none of them does I/O on the
//! caller's thread.  State comes back as [`Status`] snapshots
//! (`BgEvent::SearchStatus`, and [`Engine::status`] at any time) and
//! results as `BgEvent::SearchResults`.
//!
//! Where models run is the user's choice; the engine starts a helper for
//! image work on the chosen device only.  If the card fails, the pass
//! stops and the status says what failed; nothing is sent to the
//! processor unless the user picks it (decision 3, held by the tests at
//! the end).  Queries always run their text on the processor (plan §4.2):
//! after a card failure they go through a text-only helper.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::{NaiveDate, NaiveDateTime};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use super::index::{Index, Key, PathEntry, Record};
use super::parse::{self, Parsed};
use super::tags::{Tags, VocabVectors};
use super::{Hit, PassKind, PassProgress, Status, Stopped, Thing, pass, query, vocab};
use crate::bus::{BgEvent, Bus};
use crate::ml::client::{Client, Embedder, EmbedderFactory, HelperError, StartError};
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device, SelfCheck};
use crate::model::bursts::Bursts;
use crate::model::collection::Collection;
use crate::model::library_index::LibraryIndex;
use crate::model::record::{FileType, PhotoRecord};

/// What the engine's thread is asked.
enum Cmd {
    Library(Arc<LibraryIndex>),
    StartFirstIndex(Vec<ModelId>),
    DownloadsDone(Vec<ModelId>),
    StopIndexing,
    CancelDownloads,
    TryCardAgain,
    SetDevice(Device),
    SetIndexExports(bool),
    ClearIndex,
    RebuildIndex,
    DeleteModels,
    Query { seq: u64, text: String, closeness: f32 },
    Busy(usize),
    Shutdown,
}

/// Sizes Preferences ▸ Storage shows (measured by walking folders:
/// call [`storage`] on a worker).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub index_bytes: u64,
    pub index_shots: usize,
    pub models_bytes: u64,
}

/// The app's handle: one per app.
pub struct Engine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

struct Shared {
    status: Mutex<Status>,
    /// *Things in your photos*, as last derived from the tags.
    things: Mutex<Arc<Vec<Thing>>>,
}

/// What the engine is built on; tests swap the pieces that touch the
/// machine.
pub struct Config {
    /// Makes the helper.
    pub factory: EmbedderFactory,
    /// Whether the search model's files are in place (stats them).
    pub models_ready: Box<dyn Fn() -> bool + Send>,
    /// Where this library's index lives (the cache folder by default).
    pub index_dir: Option<PathBuf>,
}

impl Config {
    /// The app's: the real helper, the registry, the cache.
    pub fn real() -> Config {
        Config { factory: Client::factory(), models_ready: Box::new(|| models::installed(ModelId::Siglip2)), index_dir: None }
    }
}

impl Engine {
    /// Start the engine with the real helper.  `device` and
    /// `index_exports` are the saved preferences.
    pub fn new(bus: Bus, device: Device, index_exports: bool) -> Engine {
        Engine::with_config(bus, device, index_exports, Config::real())
    }

    /// Start the engine on a configuration (tests).
    pub fn with_config(bus: Bus, device: Device, index_exports: bool, config: Config) -> Engine {
        let (tx, rx) = crossbeam_channel::unbounded();
        let status = Status { index_exports, device: crate::ml::DeviceStatus { chosen: device, ..Default::default() }, ..Status::default() };
        let shared = Arc::new(Shared { status: Mutex::new(status), things: Mutex::new(Arc::new(Vec::new())) });
        let s = shared.clone();
        let me = tx.clone();
        let Config { factory, models_ready, index_dir } = config;
        std::thread::Builder::new()
            .name("search-engine".into())
            .spawn(move || {
                Worker {
                    rx,
                    me,
                    bus,
                    shared: s,
                    factory,
                    models_ready,
                    index_dir,
                    device,
                    index_exports,
                    library: None,
                    index: None,
                    embedder: None,
                    text_embedder: None,
                    pass: None,
                    download: None,
                    first_index_pending: false,
                    resolver: None,
                    text_cache: HashMap::new(),
                    vocab: None,
                    vocab_tried: false,
                    tags: Tags::default(),
                }
                .run()
            })
            .expect("spawn search-engine");
        Engine { tx, shared }
    }

    /// The latest snapshot (a clone; no I/O).
    pub fn status(&self) -> Status {
        self.shared.status.lock().clone()
    }

    /// *Things in your photos* as last derived: objects and scenes with
    /// photos, most first (a cheap clone; no I/O).  Also sent as
    /// `BgEvent::SearchThings` whenever it changes.
    pub fn things(&self) -> Arc<Vec<Thing>> {
        self.shared.things.lock().clone()
    }

    /// The library's index arrived or changed (`BgEvent::IndexReady` with
    /// `done`): the engine diffs it against the path map on its thread and,
    /// once the first index has been asked for and *Index new exports* is
    /// on, queues the new files.
    pub fn on_library(&self, index: Arc<LibraryIndex>) {
        self.send(Cmd::Library(index));
    }

    /// *Index the library…* confirmed in the download dialog with these
    /// models: download what's missing, check the card, index the library.
    pub fn start_first_index(&self, models: &[ModelId]) {
        self.send(Cmd::StartFirstIndex(models.to_vec()));
    }

    /// × on the progress: stop the pass, keeping what is done.
    pub fn stop_indexing(&self) {
        self.send(Cmd::StopIndexing);
    }

    pub fn cancel_downloads(&self) {
        self.send(Cmd::CancelDownloads);
    }

    /// *Try the card again* / *Check again*: a new helper on the card, and
    /// if it passes, the stopped work carries on.
    pub fn try_card_again(&self) {
        self.send(Cmd::TryCardAgain);
    }

    /// Preferences ▸ *Run models on*.  A running pass is stopped and
    /// started again on the new device.
    pub fn set_device(&self, device: Device) {
        self.send(Cmd::SetDevice(device));
    }

    /// Preferences ▸ *Index new exports*.
    pub fn set_index_exports(&self, on: bool) {
        self.send(Cmd::SetIndexExports(on));
    }

    /// Preferences ▸ Storage ▸ Search index ▸ Clear: stops the pass and
    /// deletes the index files.
    pub fn clear_index(&self) {
        self.send(Cmd::ClearIndex);
    }

    /// Preferences ▸ Storage ▸ Search index ▸ Rebuild: the index is
    /// cleared and every photo indexed again (search answers from what
    /// the rebuild has reached so far).
    pub fn rebuild_index(&self) {
        self.send(Cmd::RebuildIndex);
    }

    /// Preferences ▸ Storage ▸ Downloaded models ▸ Delete.
    pub fn delete_models(&self) {
        self.send(Cmd::DeleteModels);
    }

    /// Run a query (the syntax of [`parse::parse`]); `BgEvent::SearchResults
    /// { seq, .. }` answers, with the query as understood.  Queries never
    /// touch the library.
    pub fn query(&self, seq: u64, text: &str, closeness: f32) {
        self.send(Cmd::Query { seq, text: text.to_string(), closeness });
    }

    /// Thumbnails the grid is waiting for: the pass steps back while any.
    pub fn set_busy(&self, pending: usize) {
        self.send(Cmd::Busy(pending));
    }

    /// Stop the pass and the helper; the index is flushed.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

/// Sizes of the index and the models folders.  Walks them: call on a
/// worker (the Preferences storage survey).
pub fn storage(library_root: Option<&Path>) -> Storage {
    let mut out = Storage::default();
    if let Some(dir) = library_root.and_then(super::index::dir_for) {
        out.index_bytes = dir_bytes(&dir);
        out.index_shots = super::index::count_rows(&dir);
    }
    if let Some(dir) = models::models_dir() {
        out.models_bytes = dir_bytes(&dir);
    }
    out
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// A running pass, as the worker holds it.
struct Running {
    handle: pass::Handle,
    kind: PassKind,
    batches: Receiver<pass::Batch>,
}

/// Key → the shot's files and burst, built from the path map and the
/// library index when a query needs it.
struct Resolver {
    library_ptr: usize,
    paths_len: usize,
    shots: HashMap<Key, Shot>,
}

struct Shot {
    day: NaiveDate,
    jpg: Option<PathBuf>,
    raw: Option<PathBuf>,
    mtime: NaiveDateTime,
    burst: Option<u64>,
}

/// The engine's thread.
struct Worker {
    rx: Receiver<Cmd>,
    me: Sender<Cmd>,
    bus: Bus,
    shared: Arc<Shared>,
    factory: EmbedderFactory,
    models_ready: Box<dyn Fn() -> bool + Send>,
    index_dir: Option<PathBuf>,
    device: Device,
    index_exports: bool,
    library: Option<Arc<LibraryIndex>>,
    index: Option<Index>,
    /// The helper for image work, on the chosen device.
    embedder: Option<Arc<dyn Embedder>>,
    /// A text-only helper for queries while the card is unusable.
    text_embedder: Option<Arc<dyn Embedder>>,
    pass: Option<Running>,
    download: Option<crate::ml::download::DownloadHandle>,
    /// The first index starts as soon as the downloads finish.
    first_index_pending: bool,
    resolver: Option<Resolver>,
    /// Query texts encoded this session (bounded).
    text_cache: HashMap<String, Vec<f32>>,
    /// The vocabulary's text vectors (from the cache file beside the
    /// index, else encoded once), and whether encoding was tried since
    /// the last chance for it.
    vocab: Option<VocabVectors>,
    vocab_tried: bool,
    /// The hidden tags of the index's rows.
    tags: Tags,
}

const TEXT_CACHE: usize = 64;

impl Worker {
    fn run(mut self) {
        self.update(|s| s.models_installed = (self.models_ready)());
        loop {
            let cmd = if let Some(p) = &self.pass {
                crossbeam_channel::select! {
                    recv(self.rx) -> c => match c { Ok(c) => Some(c), Err(_) => break },
                    recv(p.batches) -> b => {
                        match b {
                            Ok(batch) => { self.on_batch(batch); }
                            Err(_) => { self.pass = None; }
                        }
                        None
                    }
                }
            } else {
                match self.rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => break,
                }
            };
            let Some(cmd) = cmd else { continue };
            match cmd {
                Cmd::Library(ix) => self.on_library(ix),
                Cmd::StartFirstIndex(wanted) => self.start_first_index(wanted),
                Cmd::DownloadsDone(ok) => {
                    self.download = None;
                    self.vocab_tried = false;
                    self.update(|s| s.models_installed = (self.models_ready)());
                    if self.first_index_pending && ok.contains(&ModelId::Siglip2) {
                        self.first_index_pending = false;
                        self.start_pass(PassKind::FirstIndex);
                    } else {
                        self.first_index_pending = false;
                    }
                }
                Cmd::StopIndexing => {
                    if let Some(p) = &self.pass {
                        p.handle.cancel();
                    }
                    self.first_index_pending = false;
                }
                Cmd::CancelDownloads => {
                    if let Some(d) = &self.download {
                        d.cancel();
                    }
                    self.first_index_pending = false;
                }
                Cmd::TryCardAgain => {
                    self.drop_embedder();
                    self.update(|s| {
                        s.stopped = None;
                        s.device.self_check = SelfCheck::NotRun;
                    });
                    self.resume_pass();
                }
                Cmd::SetDevice(d) => {
                    if d != self.device {
                        let was_running = self.pass.is_some();
                        self.cancel_pass();
                        self.drop_embedder();
                        self.device = d;
                        let stopped_by_card = matches!(self.shared.status.lock().stopped, Some(Stopped::Card(_)));
                        self.update(|s| {
                            s.device = crate::ml::DeviceStatus { chosen: d, ..Default::default() };
                            if stopped_by_card {
                                s.stopped = None;
                            }
                        });
                        if was_running || stopped_by_card {
                            self.resume_pass();
                        }
                    }
                }
                Cmd::SetIndexExports(on) => {
                    self.index_exports = on;
                    self.update(|s| s.index_exports = on);
                    if on {
                        self.maybe_index_new();
                    }
                }
                Cmd::ClearIndex => {
                    self.cancel_pass();
                    if let Some(ix) = &mut self.index {
                        let _ = ix.clear();
                    }
                    self.resolver = None;
                    self.update(|s| {
                        s.indexed_shots = 0;
                        s.first_index_asked = false;
                        s.stopped = None;
                        s.waiting = 0;
                    });
                    self.refresh_tags();
                }
                Cmd::RebuildIndex => {
                    self.cancel_pass();
                    if let Some(ix) = &mut self.index {
                        let _ = ix.clear();
                        ix.set_first_index_asked();
                        let _ = ix.save_paths();
                    }
                    self.resolver = None;
                    self.update(|s| {
                        s.indexed_shots = 0;
                        s.stopped = None;
                    });
                    self.refresh_tags();
                    self.start_pass(PassKind::Rebuild);
                }
                Cmd::DeleteModels => {
                    self.cancel_pass();
                    self.drop_embedder();
                    if let Some(dir) = models::models_dir() {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    self.update(|s| s.models_installed = false);
                }
                Cmd::Query { seq, text, closeness } => self.query(seq, &text, closeness),
                Cmd::Busy(n) => {
                    if let Some(p) = &self.pass {
                        p.handle.set_busy(n);
                    }
                }
                Cmd::Shutdown => break,
            }
        }
        self.cancel_pass();
        if let Some(ix) = &mut self.index {
            let _ = ix.save_paths();
        }
        self.drop_embedder();
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.shared.status.lock());
        self.publish();
    }

    fn publish(&self) {
        let snapshot = self.shared.status.lock().clone();
        let _ = self.bus.unbounded_send(BgEvent::SearchStatus(Arc::new(snapshot)));
    }

    fn drop_embedder(&mut self) {
        if let Some(e) = self.embedder.take() {
            e.shutdown();
        }
        if let Some(e) = self.text_embedder.take() {
            e.shutdown();
        }
    }

    fn cancel_pass(&mut self) {
        if let Some(p) = self.pass.take() {
            p.handle.cancel();
            // Take in what already came back, so nothing done is lost.
            while let Ok(b) = p.batches.try_recv() {
                self.absorb(&b);
            }
        }
        if let Some(ix) = &mut self.index {
            let _ = ix.save_paths();
        }
        self.refresh_tags();
        self.update(|s| s.pass = None);
    }

    /// The library arrived or changed.
    fn on_library(&mut self, lib: Arc<LibraryIndex>) {
        let root_changed = self.index.as_ref().is_none_or(|ix| ix.root != lib.root);
        if root_changed {
            self.cancel_pass();
            self.index = self.index_dir.clone().or_else(|| super::index::dir_for(&lib.root)).map(|dir| Index::load(&lib.root, &dir, models::info(ModelId::Siglip2)));
            self.resolver = None;
        }
        let live: HashSet<PathBuf> = lib.days.values().flat_map(|d| d.files.iter().map(|f| pass::rel_path(&lib.root, &f.path))).collect();
        if let Some(ix) = &mut self.index {
            ix.retain_paths(&live);
        }
        self.library = Some(lib);
        self.resolver = None;
        self.refresh_counts();
        if root_changed {
            self.vocab_tried = false;
        }
        self.refresh_tags();
        self.maybe_index_new();
    }

    /// Bring the hidden tags up to the index (new rows tagged, a cleared
    /// index started over) and publish *Things* when they changed.  The
    /// vocabulary's vectors come from the cache file beside the index, or
    /// are encoded once through the text tower (tried once per library
    /// and whenever the models arrive); without them there are no tags,
    /// and queries rank by similarity alone.
    fn refresh_tags(&mut self) {
        let Some(ix) = &self.index else { return };
        if ix.is_empty() {
            if !self.tags.is_empty() {
                self.tags = Tags::default();
                self.publish_things();
            }
            return;
        }
        let (dir, model) = (ix.dir.clone(), ix.model);
        if self.vocab.is_none() {
            self.vocab = VocabVectors::load(&dir, model);
        }
        if self.vocab.is_none() && !self.vocab_tried && (self.models_ready)() {
            self.vocab_tried = true;
            let _t = crate::profile::span("search", "vocabulary");
            match self.encode(&VocabVectors::prompts()) {
                Ok(rows) => match VocabVectors::from_rows(&rows) {
                    Some(v) => {
                        if let Err(e) = v.save(&dir, model) {
                            eprintln!("search: couldn't keep the vocabulary's vectors: {e}");
                        }
                        self.vocab = Some(v);
                    }
                    None => eprintln!("search: the helper gave the vocabulary the wrong shape"),
                },
                Err(m) => eprintln!("search: the vocabulary couldn't be encoded: {m}"),
            }
        }
        let (Some(ix), Some(vocab)) = (&self.index, &self.vocab) else { return };
        let _t = crate::profile::span("search", "tags");
        if self.tags.sync(&ix.vectors, ix.generation, vocab) {
            self.publish_things();
        }
    }

    /// Derive *Things* from the tags and send it.
    fn publish_things(&self) {
        let things = match &self.index {
            Some(ix) if !self.tags.is_empty() => {
                let live: HashSet<Key> = ix.paths.entries.values().map(|e| e.key).collect();
                let records = &ix.vectors.records;
                self.tags.things(|i| !records[i].accidental && live.contains(&records[i].key))
            }
            _ => Vec::new(),
        };
        let things = Arc::new(things);
        *self.shared.things.lock() = things.clone();
        let _ = self.bus.unbounded_send(BgEvent::SearchThings(things));
    }

    fn refresh_counts(&self) {
        let (indexed, asked) = self.index.as_ref().map_or((0, false), |ix| (ix.len(), ix.paths.first_index_asked));
        let shots = self.library.as_ref().map_or(0, |l| l.total_shots);
        self.update(|s| {
            s.library_shots = shots;
            s.indexed_shots = indexed;
            s.first_index_asked = asked;
        });
    }

    /// New exports: queued on their own once the first index was asked
    /// for and the setting is on; counted as waiting while the card is
    /// unusable.
    fn maybe_index_new(&mut self) {
        let (Some(lib), Some(ix)) = (&self.library, &self.index) else { return };
        if !ix.paths.first_index_asked || self.pass.is_some() {
            return;
        }
        let plan = pass::plan(lib, &ix.paths, &ix.vectors);
        let stopped = self.shared.status.lock().stopped.clone();
        match stopped {
            Some(Stopped::Card(_)) | Some(Stopped::Helper { .. }) => self.update(|s| s.waiting = plan.jobs.len()),
            _ if self.index_exports && !plan.jobs.is_empty() => self.start_pass(PassKind::NewExports),
            _ => self.update(|s| s.waiting = 0),
        }
    }

    fn start_first_index(&mut self, wanted: Vec<ModelId>) {
        if let Some(ix) = &mut self.index {
            ix.set_first_index_asked();
            let _ = ix.save_paths();
        }
        self.update(|s| {
            s.first_index_asked = true;
            s.stopped = None;
        });
        let missing = if (self.models_ready)() { Vec::new() } else { models::missing(&wanted) };
        if missing.is_empty() {
            self.start_pass(PassKind::FirstIndex);
            return;
        }
        if self.download.is_some() {
            self.first_index_pending = true;
            return;
        }
        self.first_index_pending = true;
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = crate::ml::download::DownloadHandle::from_flag(cancel.clone());
        let (bus, me) = (self.bus.clone(), self.me.clone());
        std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || {
                let ok = crate::ml::download::execute(&missing, &cancel, &mut |ev| drop(bus.unbounded_send(ev)));
                let _ = me.send(Cmd::DownloadsDone(ok));
            })
            .expect("spawn model-download");
        self.download = Some(handle);
    }

    /// Start indexing again after a stop (the card tried again, the device
    /// changed).
    fn resume_pass(&mut self) {
        let asked = self.index.as_ref().is_some_and(|ix| ix.paths.first_index_asked);
        if !asked || self.pass.is_some() {
            return;
        }
        let kind = if self.index.as_ref().is_some_and(|ix| ix.is_empty()) { PassKind::FirstIndex } else { PassKind::NewExports };
        self.start_pass(kind);
    }

    /// The embedder for image work on the chosen device, started if need
    /// be.  `None` after a failure, which is recorded in the status.
    fn image_embedder(&mut self) -> Option<Arc<dyn Embedder>> {
        if let Some(e) = &self.embedder {
            return Some(e.clone());
        }
        match (self.factory)(self.device, false) {
            Ok(e) => {
                let (name, check) = e.device_report();
                self.update(|s| {
                    s.device.name = name;
                    s.device.self_check = check;
                });
                self.embedder = Some(e.clone());
                Some(e)
            }
            Err(StartError::Card(f)) => {
                self.card_failed(f);
                None
            }
            Err(StartError::Other(message)) => {
                self.update(|s| s.stopped = Some(Stopped::Helper { message }));
                None
            }
        }
    }

    fn card_failed(&mut self, f: CardFailure) {
        let waiting = match (&self.library, &self.index) {
            (Some(lib), Some(ix)) => pass::plan(lib, &ix.paths, &ix.vectors).jobs.len(),
            _ => 0,
        };
        self.update(|s| {
            if let CardFailure::SelfCheckFailed { cosine, max_diff } = &f {
                s.device.self_check = SelfCheck::Failed { cosine: *cosine, max_diff: *max_diff };
            }
            s.stopped = Some(Stopped::Card(f));
            s.pass = None;
            s.waiting = waiting;
        });
    }

    fn start_pass(&mut self, kind: PassKind) {
        if self.pass.is_some() {
            return;
        }
        let (Some(lib), Some(ix)) = (self.library.clone(), self.index.as_ref()) else { return };
        if !(self.models_ready)() {
            self.update(|s| s.models_installed = false);
            return;
        }
        let plan = pass::plan(&lib, &ix.paths, &ix.vectors);
        if plan.jobs.is_empty() {
            self.refresh_counts();
            self.update(|s| s.waiting = 0);
            return;
        }
        let Some(embedder) = self.image_embedder() else { return };
        let (tx, rx) = crossbeam_channel::unbounded::<pass::Batch>();
        let total = plan.total;
        let already = plan.unchanged;
        let req = pass::Request { root: lib.root.clone(), jobs: plan.jobs, total };
        let handle = pass::start(req, embedder, already, move |b| drop(tx.send(b)));
        self.pass = Some(Running { handle, kind, batches: rx });
        self.update(|s| {
            s.pass = Some(PassProgress { kind, done: already, total, oldest_day: None });
            s.stopped = None;
            s.waiting = 0;
        });
    }

    /// Results from the running pass.
    fn on_batch(&mut self, batch: pass::Batch) {
        let Some(p) = &self.pass else { return };
        if batch.restarted {
            self.update(|s| s.helper_restarts += 1);
        }
        let kind = p.kind;
        self.absorb(&batch);
        self.refresh_tags();
        let indexed = self.index.as_ref().map_or(0, Index::len);
        if let Some(fin) = &batch.finished {
            self.pass = None;
            if let Some(ix) = &mut self.index {
                let _ = ix.save_paths();
            }
            let stopped = fin.stopped.clone();
            let is_card = matches!(stopped, Some(Stopped::Card(_)));
            if let Some(Stopped::Card(f)) = &stopped {
                self.drop_embedder();
                self.card_failed(f.clone());
            } else {
                self.update(|s| {
                    s.pass = None;
                    s.indexed_shots = indexed;
                    s.stopped = stopped;
                });
            }
            self.resolver = None;
            if !is_card {
                self.maybe_index_new();
            }
        } else {
            self.update(|s| {
                s.pass = Some(PassProgress { kind, done: batch.done, total: batch.total, oldest_day: batch.oldest_day });
                s.indexed_shots = indexed;
            });
        }
    }

    /// Write a batch's rows into the index.
    fn absorb(&mut self, batch: &pass::Batch) {
        let Some(ix) = &mut self.index else { return };
        let rows: Vec<(PathBuf, PathEntry, Record, Vec<f32>)> = batch
            .embedded
            .iter()
            .map(|(job, e)| {
                let entry = PathEntry::new(job.file_type, job.size, &job.mtime, e.key);
                let rec = Record { key: e.key, capture: e.capture, meta: e.meta, accidental: e.accidental };
                (job.rel.clone(), entry, rec, e.vector.clone())
            })
            .collect();
        if !rows.is_empty() {
            if let Err(e) = ix.add(rows) {
                eprintln!("search: couldn't write the index: {e}");
            }
            self.resolver = None;
        }
    }

    /// The embedder for a query's text: the image one when it is up, else
    /// a text-only helper on the processor.
    fn text_embedder(&mut self) -> Result<Arc<dyn Embedder>, String> {
        if let Some(e) = &self.embedder {
            return Ok(e.clone());
        }
        if let Some(e) = &self.text_embedder {
            return Ok(e.clone());
        }
        let stopped = self.shared.status.lock().stopped.clone();
        if !matches!(stopped, Some(Stopped::Card(_)) | Some(Stopped::Helper { .. })) {
            // Nothing has failed: the chosen device's helper serves both.
            if let Some(e) = self.image_embedder() {
                return Ok(e);
            }
        }
        match (self.factory)(Device::Cpu, true) {
            Ok(e) => {
                self.text_embedder = Some(e.clone());
                Ok(e)
            }
            Err(StartError::Card(f)) => Err(f.title().to_string()),
            Err(StartError::Other(m)) => Err(m),
        }
    }

    /// Encode texts through the text tower, going through the session's
    /// cache; one request for whatever is missing.
    fn encode(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut missing: Vec<String> = texts.iter().filter(|t| !self.text_cache.contains_key(*t)).cloned().collect();
        missing.dedup();
        if !missing.is_empty() {
            let embedder = self.text_embedder()?;
            let mut tries = 0;
            let vs = loop {
                match embedder.encode_text(&missing) {
                    Ok(vs) if vs.len() == missing.len() => break vs,
                    Ok(_) => return Err("The search helper gave no answer.".into()),
                    Err(HelperError::Restarted) if tries < 2 => tries += 1,
                    Err(e) => {
                        return Err(match e {
                            HelperError::Card(f) => f.title().to_string(),
                            HelperError::Restarted => "The search helper keeps stopping.".into(),
                            HelperError::Broken(m) => m,
                        });
                    }
                }
            };
            if self.text_cache.len() + missing.len() > TEXT_CACHE.max(missing.len()) {
                self.text_cache.clear();
            }
            for (t, v) in missing.into_iter().zip(vs) {
                self.text_cache.insert(t, v);
            }
        }
        Ok(texts.iter().map(|t| self.text_cache[t].clone()).collect())
    }

    fn query(&mut self, seq: u64, text: &str, closeness: f32) {
        let indexed = self.index.as_ref().map_or(0, Index::len);
        let parsed = Arc::new(parse::parse(text));
        let animal = vocab::animal_in(&parsed.words).map(|w| w.word.to_string());
        let bus = self.bus.clone();
        let answer = |hits: Vec<Hit>, fursuits: Vec<Hit>, error: Option<String>| {
            let _ = bus.unbounded_send(BgEvent::SearchResults {
                seq,
                hits: Arc::new(hits),
                indexed,
                error,
                understood: parsed.clone(),
                fursuits: Arc::new(fursuits),
                animal: animal.clone(),
            });
        };
        if indexed == 0 || parsed.is_empty() {
            answer(Vec::new(), Vec::new(), None);
            return;
        }
        if !(self.models_ready)() {
            answer(Vec::new(), Vec::new(), Some("The search model isn't downloaded.".into()));
            return;
        }
        // The prompts: the words, each exclusion, and "fursuit" for the
        // contrast on a real animal's query.
        let mut prompts = vec![if parsed.exact { parsed.words.clone() } else { query::prompt(&parsed.words) }];
        prompts.extend(parsed.exclude.iter().map(|w| query::prompt(w)));
        if animal.is_some() {
            prompts.push(query::prompt("fursuit"));
        }
        let vectors = match self.encode(&prompts) {
            Ok(v) => v,
            Err(m) => {
                answer(Vec::new(), Vec::new(), Some(m));
                return;
            }
        };
        let t = std::time::Instant::now();
        let (hits, fursuits) = self.hits(&parsed, &vectors, animal.is_some(), closeness);
        if crate::profile::on() {
            eprintln!("search: {} hits ({} fursuits left out) of {indexed} in {:.1} ms", hits.len(), fursuits.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
        answer(hits, fursuits, None);
    }

    /// Score the index against the query's vectors (the words, then each
    /// exclusion, then "fursuit" when `contrast`), cut by closeness, and
    /// resolve the rows: the hits (tagged with the query's words first,
    /// then by similarity) and, with the contrast, the fursuits left out.
    fn hits(&mut self, parsed: &Parsed, vectors: &[Vec<f32>], contrast: bool, closeness: f32) -> (Vec<Hit>, Vec<Hit>) {
        self.ensure_resolver();
        let (Some(ix), Some(r)) = (&self.index, self.resolver.as_ref()) else { return (Vec::new(), Vec::new()) };
        let Some((words, rest)) = vectors.split_first() else { return (Vec::new(), Vec::new()) };
        let scores = query::score_rows(words, &ix.vectors.data, ix.vectors.dim);
        let others: Vec<Vec<f32>> = rest.iter().map(|v| query::score_rows(v, &ix.vectors.data, ix.vectors.dim)).collect();
        let (excluded, fursuit) = if contrast { others.split_at(others.len() - 1) } else { (others.as_slice(), &[][..]) };
        let records = &ix.vectors.records;
        let dates = parsed.dates;
        let skip = |i: usize| {
            let rec = &records[i];
            if rec.accidental {
                return true;
            }
            let Some(shot) = r.shots.get(&rec.key) else { return true };
            if dates.is_some_and(|d| !d.contains(shot.day)) {
                return true;
            }
            excluded.iter().any(|e| e[i] >= scores[i])
        };
        let cut = query::cut_by_probability(&scores, skip, closeness);
        let wanted = vocab::words_in(&parsed.words);
        let tags = &self.tags;
        let tagged = |i: usize| !wanted.is_empty() && tags.len() > i && wanted.iter().all(|&w| tags.has(i, w));
        let mut hits = Vec::with_capacity(cut.len());
        let mut fursuits = Vec::new();
        for (i, score) in cut {
            let rec = &records[i];
            let Some(shot) = r.shots.get(&rec.key) else { continue };
            let capture = rec.capture.map(|c| c + chrono::Duration::milliseconds(rec.meta.subsec_ms.unwrap_or(0) as i64)).unwrap_or(shot.mtime);
            let hit = Hit { id: rec.key.short(), key: rec.key, score, tagged: tagged(i), day: shot.day, capture, burst: shot.burst, jpg: shot.jpg.clone(), raw: shot.raw.clone() };
            if fursuit.first().is_some_and(|f| f[i] > score) {
                fursuits.push(hit);
            } else {
                hits.push(hit);
            }
        }
        // The boost: tagged first, each group still best first.
        hits.sort_by_key(|h| !h.tagged);
        (hits, fursuits)
    }

    /// Build the key → shot table when the library or the index changed.
    fn ensure_resolver(&mut self) {
        let (Some(lib), Some(ix)) = (&self.library, &self.index) else {
            self.resolver = None;
            return;
        };
        let ptr = Arc::as_ptr(lib) as usize;
        if self.resolver.as_ref().is_some_and(|r| r.library_ptr == ptr && r.paths_len == ix.paths.entries.len()) {
            return;
        }
        let _t = crate::profile::span("search", "resolver");
        let mut shots: HashMap<Key, Shot> = HashMap::with_capacity(ix.vectors.len());
        let mut coll = Collection::new();
        let mut key_of_record = Vec::new();
        for day in lib.days.values() {
            for shot in day.shots() {
                let rep = shot.iter().find(|&&i| day.files[i].file_type == FileType::Jpg).or(shot.first());
                let Some(&i) = rep else { continue };
                let fe = &day.files[i];
                let rel = pass::rel_path(&lib.root, &fe.path);
                let Some(key) = ix.paths.key_for(&rel, fe.size, &fe.mtime) else { continue };
                let Some(&row) = ix.vectors.by_key.get(&key) else { continue };
                if shots.contains_key(&key) {
                    continue;
                }
                let jpg = shot.iter().find(|&&k| day.files[k].file_type == FileType::Jpg).map(|&k| day.files[k].path.clone());
                let raw = shot.iter().find(|&&k| day.files[k].file_type == FileType::Raw).map(|&k| day.files[k].path.clone());
                let rec = &ix.vectors.records[row];
                let mut pr = PhotoRecord::new(fe.path.clone(), fe.file_type, fe.size, fe.mtime, rec.capture);
                pr.shot = rec.meta;
                let id = coll.add(pr);
                key_of_record.push((id, key));
                shots.insert(key, Shot { day: day.date, jpg, raw, mtime: fe.mtime.local, burst: None });
            }
        }
        let bursts = Bursts::find(&coll);
        for (id, key) in key_of_record {
            if let (Some(b), Some(s)) = (bursts.of(id), shots.get_mut(&key)) {
                s.burst = Some(b as u64);
            }
        }
        self.resolver = Some(Resolver { library_ptr: ptr, paths_len: ix.paths.entries.len(), shots });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::pass::tests::Fake;
    use std::time::{Duration, Instant};

    fn wait_for(e: &Engine, what: &str, f: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = e.status();
            if f(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_status_starts_from_the_saved_preferences_and_a_query_answers() {
        let (bus, _rx) = crate::bus::channel();
        let config = Config { factory: Box::new(|_, _| Err(StartError::Other("no".into()))), models_ready: Box::new(|| false), index_dir: None };
        let e = Engine::with_config(bus, Device::Cpu, false, config);
        let s = e.status();
        assert_eq!(s.device.chosen, Device::Cpu);
        assert!(!s.index_exports && s.not_indexed());
        e.set_index_exports(true);
        e.query(3, "camera", super::super::CLOSENESS_DEFAULT);
        wait_for(&e, "the setting", |s| s.index_exports);
        e.shutdown();
    }

    /// A scratch library of one day with `n` tiny "JPGs", scanned into a
    /// `LibraryIndex` the way the calendar does it.  Returns it with the
    /// scratch folder (the index goes in `<folder>/index`).
    fn library(name: &str, n: usize) -> (Arc<LibraryIndex>, PathBuf) {
        let d = crate::search::index::tests::scratch(name);
        let day = d.join("lib/2025/Mar-2025/12-Mar-2025/JPG");
        std::fs::create_dir_all(&day).unwrap();
        for i in 0..n {
            std::fs::write(day.join(format!("{i:04}.JPG")), format!("not really a jpeg {i}")).unwrap();
        }
        let root = d.join("lib");
        let cancel = AtomicBool::new(false);
        let scanned = crate::model::library_cache::scan(&root, None, true, &cancel, &|_| {}).expect("scans").expect("not cancelled");
        let store = Mutex::new(crate::model::library_cache::Store::new(root.clone()));
        let ix = crate::model::library_index::index_from_tree(&root, &scanned.tree, &store, &cancel).expect("an index");
        assert_eq!(ix.total_shots, n);
        (Arc::new(ix), d)
    }

    /// A results event for `seq`, from the bus: hits, indexed, error.
    fn results(rx: &crate::bus::Receiver, seq: u64) -> (Vec<Hit>, usize, Option<String>) {
        let r = full_results(rx, seq);
        (r.0, r.1, r.2)
    }

    /// Everything a results event for `seq` says: hits, indexed, error,
    /// the query understood, the fursuits left out and the animal word.
    fn full_results(rx: &crate::bus::Receiver, seq: u64) -> (Vec<Hit>, usize, Option<String>, Parsed, Vec<Hit>, Option<String>) {
        let mut stream = Box::pin(crate::bus::stream(rx));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            use iced::futures::StreamExt;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let Some(batch) = stream.next().await else { break };
                for ev in batch {
                    if let BgEvent::SearchResults { seq: s, hits, indexed, error, understood, fursuits, animal } = ev {
                        if s == seq {
                            return ((*hits).clone(), indexed, error, (*understood).clone(), (*fursuits).clone(), animal);
                        }
                    }
                }
            }
            panic!("no results for query {seq}");
        })
    }

    fn logging_factory(gpu_fails: bool) -> (EmbedderFactory, Arc<Mutex<Vec<(Device, bool)>>>) {
        let asked: Arc<Mutex<Vec<(Device, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let factory: EmbedderFactory = Box::new(move |device, text_only| {
            log.lock().push((device, text_only));
            if text_only {
                return Ok(Arc::new(Fake::new(Device::Cpu)) as Arc<dyn Embedder>);
            }
            match device {
                Device::Gpu if gpu_fails => Err(StartError::Card(CardFailure::SelfCheckFailed { cosine: 0.5, max_diff: 0.3 })),
                d => Ok(Arc::new(Fake::new(d))),
            }
        });
        (factory, asked)
    }

    /// Decision 3: a card that fails stops the work and never sends it to
    /// the processor; the processor indexes only once it is chosen.
    #[test]
    fn a_failing_card_never_creates_a_processor_embedder() {
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library("device-rule", 6);
        let (factory, asked) = logging_factory(true);
        let config = Config { factory, models_ready: Box::new(|| true), index_dir: Some(d.join("index")) };
        let e = Engine::with_config(bus, Device::Gpu, true, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 6);
        e.start_first_index(&[ModelId::Siglip2]);
        let s = wait_for(&e, "the card to fail", |s| s.stopped.is_some());
        assert!(matches!(s.stopped, Some(Stopped::Card(CardFailure::SelfCheckFailed { .. }))), "{s:?}");
        assert!(matches!(s.device.self_check, SelfCheck::Failed { .. }));
        assert!(s.pass.is_none() && s.indexed_shots == 0 && s.first_index_asked);
        assert_eq!(s.waiting, 6, "the shots wait for the card");
        // Trying again fails the same way; a query still gets an answer.
        e.try_card_again();
        wait_for(&e, "the second failure", |s| s.stopped.is_some());
        e.query(1, "dog", super::super::CLOSENESS_DEFAULT);
        let (hits, indexed, error) = results(&rx, 1);
        assert!(hits.is_empty() && indexed == 0 && error.is_none());
        let calls = asked.lock().clone();
        assert!(calls.iter().all(|c| *c == (Device::Gpu, false)), "only the card was asked for image work: {calls:?}");
        // Only the user's choice moves the work to the processor.
        e.set_device(Device::Cpu);
        let s = wait_for(&e, "the processor to index", |s| s.indexed_shots == 6 && s.pass.is_none());
        assert!(s.stopped.is_none() && s.waiting == 0);
        assert_eq!(s.device.chosen, Device::Cpu);
        assert!(asked.lock().contains(&(Device::Cpu, false)));
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The whole way round on a working card: the first index, a query
    /// that finds a photo by its vector, bursts, new exports on their own,
    /// clear.
    #[test]
    fn the_first_index_then_queries_then_new_exports() {
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library("round-trip", 12);
        let (factory, asked) = logging_factory(false);
        let config = Config { factory, models_ready: Box::new(|| true), index_dir: Some(d.join("index")) };
        let e = Engine::with_config(bus, Device::Gpu, true, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 12);
        // Nothing is indexed on its own before the first index is asked for.
        std::thread::sleep(Duration::from_millis(50));
        assert!(e.status().not_indexed() && asked.lock().is_empty());
        e.start_first_index(&[ModelId::Siglip2]);
        let s = wait_for(&e, "the first index", |s| s.indexed_shots == 12 && s.pass.is_none());
        assert!(s.stopped.is_none());
        assert_eq!(s.device.name.as_deref(), Some("Fake Graphics card"));
        assert!(matches!(s.device.self_check, SelfCheck::Passed { .. }));
        // A query for one photo's own vector finds it first, with its day,
        // its file and its burst (all twelve frames are one press).
        let target = lib.days.values().next().unwrap().files[5].path.clone();
        e.query(7, target.to_str().unwrap(), super::super::CLOSENESS_DEFAULT);
        let (hits, indexed, error) = results(&rx, 7);
        assert_eq!((indexed, error), (12, None));
        assert!(!hits.is_empty());
        assert_eq!(hits[0].jpg.as_deref(), Some(target.as_path()));
        assert!(hits[0].raw.is_none());
        assert!((hits[0].score - 1.0).abs() < 1e-3);
        assert_eq!(hits[0].day, NaiveDate::from_ymd_opt(2025, 3, 12).unwrap());
        assert_eq!(hits[0].capture.format("%H:%M:%S%.3f").to_string(), "14:30:01.000");
        assert!(hits[0].burst.is_some() && hits.iter().all(|h| h.burst == hits[0].burst), "{hits:?}");
        assert_eq!(hits[0].id, hits[0].key.short());
        // Two more exports: indexed on their own, the index persists.
        let day = d.join("lib/2025/Mar-2025/12-Mar-2025/JPG");
        std::fs::write(day.join("0012.JPG"), "new").unwrap();
        std::fs::write(day.join("0013.JPG"), "newer").unwrap();
        let (lib2, _) = {
            let cancel = AtomicBool::new(false);
            let root = d.join("lib");
            let scanned = crate::model::library_cache::scan(&root, None, true, &cancel, &|_| {}).unwrap().unwrap();
            let store = Mutex::new(crate::model::library_cache::Store::new(root.clone()));
            (Arc::new(crate::model::library_index::index_from_tree(&root, &scanned.tree, &store, &cancel).unwrap()), ())
        };
        e.on_library(lib2);
        wait_for(&e, "the new exports", |s| s.indexed_shots == 14 && s.pass.is_none());
        e.shutdown();
        std::thread::sleep(Duration::from_millis(50));
        let root = d.join("lib");
        let back = Index::load(&root, &d.join("index"), models::info(ModelId::Siglip2));
        assert_eq!(back.len(), 14);
        assert!(back.paths.first_index_asked);
        // Clear removes it all.
        let (bus2, _rx2) = crate::bus::channel();
        let (factory, _) = logging_factory(false);
        let e2 = Engine::with_config(bus2, Device::Gpu, true, Config { factory, models_ready: Box::new(|| true), index_dir: Some(d.join("index")) });
        e2.on_library(lib.clone());
        wait_for(&e2, "the saved index", |s| s.indexed_shots == 14);
        e2.clear_index();
        let s = wait_for(&e2, "the clear", |s| s.indexed_shots == 0);
        assert!(!s.first_index_asked);
        e2.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }
}
