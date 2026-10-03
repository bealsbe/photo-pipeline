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
use std::time::{Duration, Instant};

use chrono::{NaiveDate, NaiveDateTime};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use super::faces::People;
use super::index::{Index, Key, PathEntry, Record};
use super::parse::{self, Parsed};
use super::tags::{Tags, VocabVectors};
use super::{FacesStatus, Hit, PassKind, PassProgress, Status, Stopped, Thing, pass, query, vocab};
use crate::bus::{BgEvent, Bus};
use crate::ml::client::{Client, Embedder, EmbedderFactory, HelperError, StartError};
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device, SelfCheck};
use crate::model::bursts::Bursts;
use crate::model::collection::Collection;
use crate::model::library_index::LibraryIndex;
use crate::model::record::{FileType, PhotoRecord};
use crate::model::subjects::{Face, FaceRef, Framing, GroupId, Kind, Refused, SubjectId, Subjects};

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
    Query { seq: u64, text: String, closeness: f32, subjects: Vec<GroupId> },
    Busy(usize),
    // People (phase 3).
    SetFindFaces(bool),
    SetFaceModel(ModelId),
    FindFaces,
    NameGroup(GroupId, String),
    Rename(SubjectId, String),
    Merge { into: GroupId, others: Vec<GroupId> },
    SetHidden(GroupId, bool),
    Forget(SubjectId),
    ForgetAll,
    Faces { seq: u64, group: GroupId },
    MoveFaces { from: GroupId, faces: Vec<FaceRef>, to: Option<GroupId> },
    Answer { group: GroupId, faces: Vec<FaceRef>, yes: Option<bool> },
    Suggestions { seq: u64, group: GroupId },
    Undo,
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
    /// The *Subjects* page's state, as last built.
    subjects: Mutex<Arc<Subjects>>,
}

/// What the engine is built on; tests swap the pieces that touch the
/// machine.
pub struct Config {
    /// Makes the helper.
    pub factory: EmbedderFactory,
    /// Whether the search model's files are in place (stats them).
    pub installed: Box<dyn Fn(ModelId) -> bool + Send>,
    /// Where this library's index lives (the cache folder by default).
    pub index_dir: Option<PathBuf>,
    /// The user's subjects file (`~/.local/share/photo-pipeline/subjects.json`
    /// by default; `None` keeps it in memory only, for tests).
    pub subjects_path: Option<PathBuf>,
}

impl Config {
    /// The app's: the real helper, the registry, the cache, the user's
    /// data folder.
    pub fn real() -> Config {
        Config { factory: Client::factory(), installed: Box::new(models::installed), index_dir: None, subjects_path: super::faces::default_file_path() }
    }
}

impl Engine {
    /// Start the engine with the real helper.  `device`, `index_exports`,
    /// `find_faces` and `face_model` are the saved preferences.
    pub fn new(bus: Bus, device: Device, index_exports: bool, find_faces: bool, face_model: ModelId) -> Engine {
        Engine::with_config(bus, device, index_exports, find_faces, face_model, Config::real())
    }

    /// Start the engine on a configuration (tests).
    pub fn with_config(bus: Bus, device: Device, index_exports: bool, find_faces: bool, face_model: ModelId, config: Config) -> Engine {
        let (tx, rx) = crossbeam_channel::unbounded();
        let faces = FacesStatus { on: find_faces, model: Some(face_model), ..FacesStatus::default() };
        let status = Status { index_exports, device: crate::ml::DeviceStatus { chosen: device, ..Default::default() }, faces, ..Status::default() };
        let shared = Arc::new(Shared { status: Mutex::new(status), things: Mutex::new(Arc::new(Vec::new())), subjects: Mutex::new(Arc::new(Subjects::default())) });
        let s = shared.clone();
        let me = tx.clone();
        let Config { factory, installed, index_dir, subjects_path } = config;
        std::thread::Builder::new()
            .name("search-engine".into())
            .spawn(move || {
                Worker {
                    rx,
                    me,
                    bus,
                    shared: s,
                    factory,
                    installed,
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
                    tried: HashSet::new(),
                    resolver: None,
                    text_cache: HashMap::new(),
                    vocab: None,
                    vocab_tried: false,
                    tags: Tags::default(),
                    things_dirty: false,
                    things_sent: None,
                    find_faces,
                    face_model,
                    people: People::load(subjects_path),
                    restrict: None,
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

    /// Run a query (the syntax of [`parse::parse`]); `BgEvent::SearchResults
    /// { seq, .. }` answers, with the query as understood.  `subjects`
    /// narrows it to photos of those groups (AND; a subject's page is a
    /// query with its chip), as do `@name`s in the text that name a
    /// subject; with no words, the photos come clearest first (UI 12).
    /// Queries never touch the library.
    pub fn query(&self, seq: u64, text: &str, closeness: f32, subjects: &[GroupId]) {
        self.send(Cmd::Query { seq, text: text.to_string(), closeness, subjects: subjects.to_vec() });
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

/// The people API (phase 3): what the *Subjects* page calls.
impl Engine {
    /// The *Subjects* page's state as last built (a cheap clone; no I/O).
    /// Also sent as `BgEvent::Subjects` whenever it changes.
    pub fn subjects(&self) -> Arc<Subjects> {
        self.shared.subjects.lock().clone()
    }

    /// Preferences ▸ People ▸ *Find people's faces*.  Off: no new faces
    /// are looked for (what was found stays until *Forget all*).
    pub fn set_find_faces(&self, on: bool) {
        self.send(Cmd::SetFindFaces(on));
    }

    /// Preferences ▸ People ▸ *Face model* ▸ *Change…*: the face vectors
    /// are computed again with `model` (the photos with faces are read
    /// again); names, members, rejections and covers are kept.  Downloads
    /// the model first if need be.
    pub fn set_face_model(&self, model: ModelId) {
        self.send(Cmd::SetFaceModel(model));
    }

    /// *Find faces in the N photos indexed*: the catch-up over an
    /// already-indexed library, only when asked (plan §5.2 step 3).
    /// Downloads the detector and the chosen model first if need be.
    pub fn find_faces(&self) {
        self.send(Cmd::FindFaces);
    }

    /// Name a group: a cluster becomes a named subject with the cluster's
    /// faces; a subject is renamed.
    pub fn name_group(&self, id: GroupId, name: String) {
        self.send(Cmd::NameGroup(id, name));
    }

    /// Rename a subject (F2).
    pub fn rename(&self, id: SubjectId, name: String) {
        self.send(Cmd::Rename(id, name));
    }

    /// Merge `others` into `into`; refused, and nothing changed, when the
    /// kinds differ.  Ctrl+Z ([`Engine::undo`]) takes it back.
    pub fn merge(&self, into: GroupId, others: Vec<GroupId>) {
        self.send(Cmd::Merge { into, others });
    }

    /// Hide a group from its tab and from completions (its photos still
    /// come up by name), or show it again.
    pub fn set_hidden(&self, id: GroupId, hidden: bool) {
        self.send(Cmd::SetHidden(id, hidden));
    }

    /// *Forget this person*: the subject goes; its faces are unnamed
    /// again.
    pub fn forget(&self, id: SubjectId) {
        self.send(Cmd::Forget(id));
    }

    /// Preferences ▸ Storage ▸ *Forget all*: every face found, every
    /// vector and tile, and the subjects file (§10).
    pub fn forget_all(&self) {
        self.send(Cmd::ForgetAll);
    }

    /// A group's faces, clearest first: `BgEvent::SubjectFaces { seq, .. }`.
    pub fn faces(&self, seq: u64, group: GroupId) {
        self.send(Cmd::Faces { seq, group });
    }

    /// Move faces out of `from`: into `to` (the same kind, else refused),
    /// or into a new unnamed group (`None`).
    pub fn move_faces(&self, from: GroupId, faces: Vec<FaceRef>, to: Option<GroupId>) {
        self.send(Cmd::MoveFaces { from, faces, to });
    }

    /// A review answer: `Some(true)` adds the faces to the group,
    /// `Some(false)` is "not this person" (out of the members, never
    /// suggested again), `None` takes an answer back.  One call covers a
    /// burst's frames.
    pub fn answer(&self, group: GroupId, faces: Vec<FaceRef>, yes: Option<bool>) {
        self.send(Cmd::Answer { group, faces, yes });
    }

    /// Suggestions for review: `BgEvent::Suggestions { seq, .. }`, one
    /// entry per burst, most likely first; for a cluster, its own members
    /// to check.
    pub fn suggestions(&self, seq: u64, group: GroupId) {
        self.send(Cmd::Suggestions { seq, group });
    }

    /// Take the last subjects edit back (name, rename, merge, hide, move,
    /// answer, forget): `BgEvent::SubjectsUndone` says what, with a fresh
    /// `BgEvent::Subjects`.
    pub fn undo(&self) {
        self.send(Cmd::Undo);
    }

    /// A face's pixels for a tile or the review, `px`×`px` (or fitted,
    /// for [`Framing::Photo`]), as `(width, height, RGBA)`.  **Blocks**:
    /// reads the cached tile, else the photo's preview; call it on a
    /// worker, never the UI thread.
    pub fn crop(&self, face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
        super::faces::crop_from_photo(face, px, framing)
    }
}

impl Engine {
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
    installed: Box<dyn Fn(ModelId) -> bool + Send>,
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
    /// Files (library-relative) the last pass couldn't embed: no plan
    /// holds them again until the library is checked again or the user
    /// asks (start, try again, the device, rebuild), so the end of one
    /// pass never starts another over the same files.
    tried: HashSet<PathBuf>,
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
    /// *Things* changed since it was last sent (held back while a pass
    /// runs, so the start page isn't rebuilt several times a second).
    things_dirty: bool,
    things_sent: Option<Instant>,
    /// Preferences ▸ People: faces looked for while indexing, and with
    /// which model.
    find_faces: bool,
    face_model: ModelId,
    /// The user's subjects and what is derived from the faces found.
    people: People,
    /// The photos the running query is narrowed to (its subjects), while
    /// it runs.
    restrict: Option<HashSet<Key>>,
}

const TEXT_CACHE: usize = 64;
/// While a pass runs, *Things* is sent at most this often.
const THINGS_EVERY: Duration = Duration::from_secs(2);

impl Worker {
    fn run(mut self) {
        self.refresh_installed();
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
                    self.refresh_installed();
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
                    self.tried.clear();
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
                        // Clearing also forgets the files that couldn't be
                        // read: a rebuild tries everything again.
                        let _ = ix.clear();
                        ix.set_first_index_asked();
                        let _ = ix.save_paths();
                    }
                    self.tried.clear();
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
                Cmd::Query { seq, text, closeness, subjects } => self.query(seq, &text, closeness, &subjects),
                Cmd::Busy(n) => {
                    if let Some(p) = &self.pass {
                        p.handle.set_busy(n);
                    }
                }
                Cmd::SetFindFaces(on) => {
                    self.find_faces = on;
                    self.update(|s| s.faces.on = on);
                }
                Cmd::SetFaceModel(model) => {
                    if models::face_models().any(|m| m.id == model) && model != self.face_model {
                        self.face_model = model;
                        self.update(|s| s.faces.model = Some(model));
                        self.refresh_installed();
                    }
                }
                Cmd::FindFaces => {
                    // The catch-up pass comes with the face cache; for now
                    // the models it needs are fetched.
                    let wanted = models::models_for(Some(self.face_model));
                    self.download_missing(&wanted);
                }
                Cmd::NameGroup(id, name) => {
                    let r = self.people.name_group(&id, name);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Rename(id, name) => {
                    let r = self.people.name_group(&GroupId::Subject(id), name);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Merge { into, others } => {
                    let r = self.people.merge(&into, &others);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::SetHidden(id, hidden) => {
                    let r = self.people.set_hidden(&id, hidden);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Forget(id) => {
                    let r = self.people.forget(&id);
                    self.after_edit(r);
                }
                Cmd::ForgetAll => {
                    self.people.forget_all();
                    self.publish_subjects();
                }
                Cmd::Faces { seq, group } => {
                    self.ensure_resolver();
                    let faces = self.people.faces(&group, &|f| self.resolve_face(f)).unwrap_or_default();
                    let _ = self.bus.unbounded_send(BgEvent::SubjectFaces { seq, group, faces: Arc::new(faces) });
                }
                Cmd::MoveFaces { from, faces, to } => {
                    let r = self.people.move_faces(&from, faces, to.as_ref());
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Answer { group, faces, yes } => {
                    let r = self.people.answer(&group, faces, yes);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Suggestions { seq, group } => {
                    let _ = self.bus.unbounded_send(BgEvent::Suggestions { seq, group, list: Arc::new(Vec::new()) });
                }
                Cmd::Undo => {
                    if let Some(what) = self.people.undo() {
                        let _ = self.bus.unbounded_send(BgEvent::SubjectsUndone(what));
                        self.publish_subjects();
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

    /// The search model's files are in place.
    fn models_ready(&self) -> bool {
        (self.installed)(ModelId::Siglip2)
    }

    /// The models among `ids` whose files aren't in place.
    fn missing(&self, ids: &[ModelId]) -> Vec<ModelId> {
        ids.iter().copied().filter(|&id| !(self.installed)(id)).collect()
    }

    /// Which models are in place: search's, and the detector with the
    /// chosen face model (stats files, on this thread).
    fn refresh_installed(&mut self) {
        let search = self.models_ready();
        let faces = self.missing(&models::face_set(self.face_model)).is_empty();
        self.update(|s| {
            s.models_installed = search;
            s.faces.installed = faces;
        });
    }

    /// After a subjects edit: a refusal is logged (nothing changed); any
    /// outcome republishes the page's state.
    fn after_edit(&mut self, result: Result<(), Refused>) {
        if let Err(e) = result {
            eprintln!("subjects: refused: {e}");
        }
        self.publish_subjects();
    }

    /// Rebuild the *Subjects* snapshot and send it.
    fn publish_subjects(&mut self) {
        self.ensure_resolver();
        let snapshot = self.people.build_snapshot(&|f| self.resolve_face(f));
        self.people.set_snapshot(snapshot.clone());
        let named = self.people.file.named_count(Kind::Person);
        *self.shared.subjects.lock() = snapshot.clone();
        self.update(|s| s.faces.named = named);
        let _ = self.bus.unbounded_send(BgEvent::Subjects(snapshot));
    }

    /// A detection as a [`Face`] with its shot, from the resolver; none
    /// when the photo isn't in the library any more.
    fn resolve_face(&self, f: &FaceRef) -> Option<Face> {
        let r = self.resolver.as_ref()?;
        let shot = r.shots.get(&f.key)?;
        let rec = self.index.as_ref().and_then(|ix| ix.vectors.by_key.get(&f.key).map(|&i| &ix.vectors.records[i]));
        let capture = rec.and_then(|rec| rec.capture.map(|c| c + chrono::Duration::milliseconds(rec.meta.subsec_ms.unwrap_or(0) as i64))).unwrap_or(shot.mtime);
        let confirmed = self.people.file.member_of(Kind::Person, f).is_some();
        Some(Face { face: *f, jpg: shot.jpg.clone(), raw: shot.raw.clone(), day: shot.day, capture, burst: shot.burst, clarity: 0.0, confirmed })
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
        self.flush_things();
        self.update(|s| s.pass = None);
    }

    /// The library arrived or changed (the calendar checked it: files
    /// the last pass couldn't embed may be tried once more; files
    /// remembered as unreadable only when their size or mtime changed —
    /// a deep re-read of the library looks the same from here, so only a
    /// rebuild retries those).
    fn on_library(&mut self, lib: Arc<LibraryIndex>) {
        self.tried.clear();
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
                self.publish_things(true);
            }
            return;
        }
        let (dir, model) = (ix.dir.clone(), ix.model);
        if self.vocab.is_none() {
            self.vocab = VocabVectors::load(&dir, model);
        }
        if self.vocab.is_none() && !self.vocab_tried && self.models_ready() {
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
            self.publish_things(false);
        }
    }

    /// Derive *Things* from the tags and send it; unless `now`, a running
    /// pass holds it back to once in [`THINGS_EVERY`] (the rest is sent
    /// when the pass ends, by [`flush_things`](Self::flush_things)).
    fn publish_things(&mut self, now: bool) {
        if !now && self.pass.is_some() && self.things_sent.is_some_and(|t| t.elapsed() < THINGS_EVERY) {
            self.things_dirty = true;
            return;
        }
        self.things_dirty = false;
        self.things_sent = Some(Instant::now());
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

    /// Send what a pass held back.
    fn flush_things(&mut self) {
        if self.things_dirty {
            self.publish_things(true);
        }
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

    /// The library against the index: what is to be indexed, less what
    /// the last pass couldn't embed (see `tried`).  Notes how many files
    /// are skipped as unreadable in the status, for the caller's next
    /// publish.
    fn check(&self) -> Option<pass::Plan> {
        let (Some(lib), Some(ix)) = (&self.library, &self.index) else { return None };
        let mut plan = pass::plan(lib, &ix.paths, &ix.vectors);
        if !self.tried.is_empty() {
            plan.jobs.retain(|j| !self.tried.contains(&j.rel));
        }
        self.shared.status.lock().unreadable = plan.unreadable;
        Some(plan)
    }

    /// New exports: queued on their own once the first index was asked
    /// for and the setting is on; counted as waiting while the card is
    /// unusable; nothing while the user's × holds.
    fn maybe_index_new(&mut self) {
        if self.pass.is_some() || !self.index.as_ref().is_some_and(|ix| ix.paths.first_index_asked) {
            return;
        }
        let Some(plan) = self.check() else { return };
        let stopped = self.shared.status.lock().stopped.clone();
        match stopped {
            // × holds until the user asks again: start, try again, rebuild.
            Some(Stopped::Cancelled) => self.publish(),
            Some(Stopped::Card(_)) | Some(Stopped::Helper { .. }) => self.update(|s| s.waiting = plan.jobs.len()),
            _ if self.index_exports && !plan.jobs.is_empty() => self.start_pass(PassKind::NewExports),
            _ => self.update(|s| s.waiting = 0),
        }
    }

    fn start_first_index(&mut self, wanted: Vec<ModelId>) {
        self.tried.clear();
        if let Some(ix) = &mut self.index {
            ix.set_first_index_asked();
            let _ = ix.save_paths();
        }
        // The dialog's face choice (decision 10): a face model among the
        // rows means faces on with that model; none means faces unticked.
        let face_model = models::face_model_in(&wanted);
        self.find_faces = face_model.is_some();
        if let Some(m) = face_model {
            self.face_model = m;
        }
        let (on, model) = (self.find_faces, self.face_model);
        self.update(|s| {
            s.first_index_asked = true;
            s.stopped = None;
            s.faces.on = on;
            s.faces.model = Some(model);
        });
        self.refresh_installed();
        let missing = self.missing(&wanted);
        if missing.is_empty() {
            self.start_pass(PassKind::FirstIndex);
            return;
        }
        self.first_index_pending = true;
        self.download_missing(&wanted);
    }

    /// Download the models among `wanted` that aren't in place, on a
    /// thread of their own; `Cmd::DownloadsDone` follows.  Nothing to do
    /// when a download is already running or nothing is missing.
    fn download_missing(&mut self, wanted: &[ModelId]) {
        let missing = self.missing(wanted);
        if missing.is_empty() || self.download.is_some() {
            return;
        }
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
        self.tried.clear();
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
        let waiting = self.check().map_or(0, |p| p.jobs.len());
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
        let Some(root) = self.library.as_ref().map(|l| l.root.clone()) else { return };
        if !self.models_ready() {
            self.update(|s| s.models_installed = false);
            return;
        }
        let Some(plan) = self.check() else { return };
        if plan.jobs.is_empty() {
            self.refresh_counts();
            self.update(|s| s.waiting = 0);
            return;
        }
        let Some(embedder) = self.image_embedder() else { return };
        let (tx, rx) = crossbeam_channel::unbounded::<pass::Batch>();
        let total = plan.total;
        // Done before this pass: indexed, and skipped as unreadable.
        let already = plan.unchanged + plan.unreadable;
        let req = pass::Request { root, jobs: plan.jobs, total };
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
            self.flush_things();
            if let Some(ix) = &mut self.index {
                let _ = ix.save_paths();
            }
            let stopped = fin.stopped.clone();
            let complete = stopped.is_none();
            if let Some(Stopped::Card(f)) = &stopped {
                self.drop_embedder();
                self.card_failed(f.clone());
            } else {
                if !complete {
                    self.check();
                }
                self.update(|s| {
                    s.pass = None;
                    s.indexed_shots = indexed;
                    s.stopped = stopped;
                });
            }
            self.resolver = None;
            // Only a pass that ran to its end looks for files that arrived
            // meanwhile.  A stopped one (×, the card, the library away)
            // waits for the user or the library; and what this pass
            // couldn't embed is in `tried`, so its end never starts
            // another pass over the same files.
            if complete {
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
        // What couldn't be embedded isn't tried again at this pass's end;
        // a file whose own content is the trouble is remembered, so it
        // isn't read again at every check until it changes.
        for (job, err) in &batch.failed {
            self.tried.insert(job.rel.clone());
            if pass::is_files_own(err) {
                ix.note_failed(job.rel.clone(), job.size, &job.mtime);
                self.resolver = None;
            }
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

    fn query(&mut self, seq: u64, text: &str, closeness: f32, subjects: &[GroupId]) {
        let indexed = self.index.as_ref().map_or(0, Index::len);
        let parsed = Arc::new(parse::parse(text));
        let animal = vocab::animal_in(&parsed.words).map(|w| w.word.to_string());
        // The groups the query is narrowed to: those passed, and each
        // `@name` that names a subject (a name of two subjects means
        // either).  A name nobody has is absent, and narrows to nothing.
        let mut groups: Vec<GroupId> = subjects.to_vec();
        let mut restrict: Option<HashSet<Key>> = None;
        let mut narrow = |keys: HashSet<Key>| {
            restrict = Some(match restrict.take() {
                Some(r) => r.intersection(&keys).copied().collect(),
                None => keys,
            });
        };
        for g in subjects {
            narrow(self.people.photos_of(g).unwrap_or_default());
        }
        for name in &parsed.names {
            let named = self.people.named(Kind::Person, name);
            let mut keys = HashSet::new();
            for g in &named {
                keys.extend(self.people.photos_of(g).unwrap_or_default());
            }
            groups.extend(named);
            narrow(keys);
        }
        groups.dedup();
        let groups = Arc::new(groups);
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
                subjects: groups.clone(),
            });
        };
        // Subjects with no words and no dates: their photos, clearest
        // first (UI 12).
        if parsed.is_empty() && parsed.dates.is_none() && parsed.exclude.is_empty() {
            if let Some(keys) = &restrict {
                let hits = self.subject_hits(keys);
                answer(hits, Vec::new(), None);
                return;
            }
        }
        // Nothing to search: a name or a `-word` alone runs nothing;
        // dates alone browse the period.
        if indexed == 0 || (parsed.is_empty() && parsed.dates.is_none()) {
            answer(Vec::new(), Vec::new(), None);
            return;
        }
        let browsing = parsed.is_empty();
        if !self.models_ready() && !(browsing && parsed.exclude.is_empty()) {
            answer(Vec::new(), Vec::new(), Some("The search model isn't downloaded.".into()));
            return;
        }
        self.restrict = restrict;
        // The prompts: the words (none when browsing dates), each
        // exclusion, the words the query's vocabulary words must beat
        // ("infrared" for snow), and "fursuit" last for the contrast on a
        // real animal's query.
        let mut prompts = Vec::new();
        if !browsing {
            prompts.push(if parsed.exact { parsed.words.clone() } else { vocab::query_prompt(&parsed.words) });
            prompts.extend(parsed.exclude.iter().map(|w| query::prompt(w)));
            prompts.extend(vocab::contrasts_in(&parsed.words).iter().map(|w| w.prompt()));
        } else {
            prompts.extend(parsed.exclude.iter().map(|w| query::prompt(w)));
        }
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
        let (hits, fursuits) = if browsing { (self.browse(&parsed, &vectors, closeness), Vec::new()) } else { self.hits(&parsed, &vectors, animal.is_some(), closeness) };
        self.restrict = None;
        if crate::profile::on() {
            eprintln!("search: {} hits ({} fursuits left out) of {indexed} in {:.1} ms", hits.len(), fursuits.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
        answer(hits, fursuits, None);
    }

    /// A subject's photos with no words typed: one hit per photo among
    /// `keys`, clearest first (UI 12): `score` is the clarity of the
    /// subject's clearest face in the photo, so doubtful members sink to
    /// the bottom.  Pure lookups; no model.
    fn subject_hits(&mut self, keys: &HashSet<Key>) -> Vec<Hit> {
        self.ensure_resolver();
        let (Some(ix), Some(r)) = (&self.index, self.resolver.as_ref()) else { return Vec::new() };
        let clarity = self.people.clarity_by_photo(keys);
        let mut hits: Vec<Hit> = keys
            .iter()
            .filter_map(|key| {
                let shot = r.shots.get(key)?;
                let rec = ix.vectors.by_key.get(key).map(|&i| &ix.vectors.records[i]);
                let capture = rec.and_then(|rec| rec.capture.map(|c| c + chrono::Duration::milliseconds(rec.meta.subsec_ms.unwrap_or(0) as i64))).unwrap_or(shot.mtime);
                let score = clarity.get(key).copied().unwrap_or(0.0);
                Some(Hit { id: key.short(), key: *key, score, tagged: false, day: shot.day, capture, burst: shot.burst, jpg: shot.jpg.clone(), raw: shot.raw.clone() })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| b.capture.cmp(&a.capture)).then_with(|| a.key.cmp(&b.key)));
        hits
    }

    /// A query of dates and no words: every indexed shot of those days in
    /// shooting order, oldest first, as browsing a period reads, with a
    /// neutral score and no cut or cap.  Accidental frames stay out.  With
    /// `-word`s (their vectors, in order), a shot is left out when a search
    /// for the word would show it: its match probability for the word
    /// reaches the closeness setting.
    fn browse(&mut self, parsed: &Parsed, excluded: &[Vec<f32>], closeness: f32) -> Vec<Hit> {
        self.ensure_resolver();
        let (Some(ix), Some(r), Some(dates)) = (&self.index, self.resolver.as_ref(), parsed.dates) else { return Vec::new() };
        let scores: Vec<Vec<f32>> = excluded.iter().map(|v| query::score_rows(v, &ix.vectors.data, ix.vectors.dim)).collect();
        let floor = query::cosine_for_probability(closeness.clamp(query::CLOSENESS_MIN, query::CLOSENESS_MAX));
        let restrict = self.restrict.as_ref();
        let mut hits: Vec<Hit> = ix
            .vectors
            .records
            .iter()
            .enumerate()
            .filter(|(i, rec)| !rec.accidental && !scores.iter().any(|s| s[*i] >= floor) && restrict.is_none_or(|r| r.contains(&rec.key)))
            .filter_map(|(_, rec)| {
                let shot = r.shots.get(&rec.key)?;
                if !dates.contains(shot.day) {
                    return None;
                }
                let capture = rec.capture.map(|c| c + chrono::Duration::milliseconds(rec.meta.subsec_ms.unwrap_or(0) as i64)).unwrap_or(shot.mtime);
                Some(Hit { id: rec.key.short(), key: rec.key, score: 0.0, tagged: false, day: shot.day, capture, burst: shot.burst, jpg: shot.jpg.clone(), raw: shot.raw.clone() })
            })
            .collect();
        hits.sort_by(|a, b| a.capture.cmp(&b.capture).then_with(|| a.key.cmp(&b.key)));
        hits
    }

    /// Score the index against the query's vectors (the words, then each
    /// exclusion and each word the query's words must beat, then
    /// "fursuit" when `contrast`), cut by closeness, and resolve the rows:
    /// the hits (tagged with the query's words first, then by similarity)
    /// and, with the contrast, the fursuits left out.
    fn hits(&mut self, parsed: &Parsed, vectors: &[Vec<f32>], contrast: bool, closeness: f32) -> (Vec<Hit>, Vec<Hit>) {
        self.ensure_resolver();
        let (Some(ix), Some(r)) = (&self.index, self.resolver.as_ref()) else { return (Vec::new(), Vec::new()) };
        let Some((words, rest)) = vectors.split_first() else { return (Vec::new(), Vec::new()) };
        let scores = query::score_rows(words, &ix.vectors.data, ix.vectors.dim);
        let others: Vec<Vec<f32>> = rest.iter().map(|v| query::score_rows(v, &ix.vectors.data, ix.vectors.dim)).collect();
        let (excluded, fursuit) = if contrast { others.split_at(others.len() - 1) } else { (others.as_slice(), &[][..]) };
        let records = &ix.vectors.records;
        let dates = parsed.dates;
        let restrict = self.restrict.as_ref();
        let skip = |i: usize| {
            let rec = &records[i];
            if rec.accidental || restrict.is_some_and(|r| !r.contains(&rec.key)) {
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
        let config = Config { factory: Box::new(|_, _| Err(StartError::Other("no".into()))), installed: Box::new(|_| false), index_dir: None, subjects_path: None };
        let e = Engine::with_config(bus, Device::Cpu, false, true, ModelId::AuraFace, config);
        let s = e.status();
        assert_eq!(s.device.chosen, Device::Cpu);
        assert!(!s.index_exports && s.not_indexed());
        e.set_index_exports(true);
        e.query(3, "camera", super::super::CLOSENESS_DEFAULT, &[]);
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
        let r = Events::new(rx).results(seq);
        (r.0, r.1, r.2)
    }

    /// Everything a results event says: hits, indexed, error, the query
    /// understood, the fursuits left out and the animal word.
    type Answer = (Vec<Hit>, usize, Option<String>, Parsed, Vec<Hit>, Option<String>);

    /// The bus's events, read as one stream (a receiver can be streamed
    /// only once).
    struct Events {
        stream: std::pin::Pin<Box<dyn iced::futures::Stream<Item = Vec<BgEvent>>>>,
        rt: tokio::runtime::Runtime,
        /// A pass was running at the last status read.
        running: bool,
    }

    impl Events {
        fn new(rx: &crate::bus::Receiver) -> Events {
            Events { stream: Box::pin(crate::bus::stream(rx)), rt: tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap(), running: false }
        }

        /// The passes that started since the last call, from the status
        /// events: each change from no pass to one.  Waits until the
        /// engine has been quiet for a moment, so a pass that would start
        /// at the end of another is seen.
        fn pass_starts(&mut self) -> Vec<PassKind> {
            let stream = &mut self.stream;
            let running = &mut self.running;
            self.rt.block_on(async {
                use iced::futures::StreamExt;
                let mut out = Vec::new();
                while let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(400), stream.next()).await {
                    for ev in batch {
                        if let BgEvent::SearchStatus(s) = ev {
                            if let Some(p) = &s.pass {
                                if !*running {
                                    out.push(p.kind);
                                }
                            }
                            *running = s.pass.is_some();
                        }
                    }
                }
                out
            })
        }

        /// The answer to query `seq`.
        fn results(&mut self, seq: u64) -> Answer {
            let stream = &mut self.stream;
            self.rt.block_on(async {
                use iced::futures::StreamExt;
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    let Some(batch) = stream.next().await else { break };
                    for ev in batch {
                        if let BgEvent::SearchResults { seq: s, hits, indexed, error, understood, fursuits, animal, subjects: _ } = ev {
                            if s == seq {
                                return ((*hits).clone(), indexed, error, (*understood).clone(), (*fursuits).clone(), animal);
                            }
                        }
                    }
                }
                panic!("no results for query {seq}");
            })
        }

        /// The Things list: the first non-empty `SearchThings` (or the
        /// engine's own once it has one).
        fn things(&mut self, e: &Engine) -> Arc<Vec<Thing>> {
            let stream = &mut self.stream;
            self.rt.block_on(async {
                use iced::futures::StreamExt;
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if !e.things().is_empty() {
                        return e.things();
                    }
                    assert!(Instant::now() < deadline, "no Things arrived");
                    if let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
                        for ev in batch {
                            if let BgEvent::SearchThings(t) = ev {
                                if !t.is_empty() {
                                    return t;
                                }
                            }
                        }
                    }
                }
            })
        }
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
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
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
        e.query(1, "dog", super::super::CLOSENESS_DEFAULT, &[]);
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
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
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
        e.query(7, target.to_str().unwrap(), super::super::CLOSENESS_DEFAULT, &[]);
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
        let e2 = Engine::with_config(bus2, Device::Gpu, true, true, ModelId::AuraFace, Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None });
        e2.on_library(lib.clone());
        wait_for(&e2, "the saved index", |s| s.indexed_shots == 14);
        e2.clear_index();
        let s = wait_for(&e2, "the clear", |s| s.indexed_shots == 0);
        assert!(!s.first_index_asked);
        e2.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A factory that hands out one `Fake`, kept so the test can read its
    /// call count.
    fn one_fake(fake: Arc<Fake>) -> EmbedderFactory {
        Box::new(move |_, _| Ok(fake.clone() as Arc<dyn Embedder>))
    }

    /// Scan a scratch library again (after its files changed).
    fn rescan(root: &Path) -> Arc<LibraryIndex> {
        let cancel = AtomicBool::new(false);
        let scanned = crate::model::library_cache::scan(root, None, true, &cancel, &|_| {}).unwrap().unwrap();
        let store = Mutex::new(crate::model::library_cache::Store::new(root.to_path_buf()));
        Arc::new(crate::model::library_index::index_from_tree(root, &scanned.tree, &store, &cancel).unwrap())
    }

    /// A file the helper can't read is read once: the first index ends
    /// and no pass follows it, a library check with nothing new starts
    /// nothing, the failure is kept on disk, and only a change to the
    /// file's size or mtime makes it a job again.
    #[test]
    fn an_unreadable_file_is_skipped_until_it_changes_and_never_loops() {
        use std::sync::atomic::Ordering::Relaxed;
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library("unreadable", 5);
        let mut f = Fake::new(Device::Gpu);
        f.fail_paths_containing = Some("0003".into());
        let fake = Arc::new(f);
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
        let mut ev = Events::new(&rx);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 5);
        e.start_first_index(&[ModelId::Siglip2]);
        let s = wait_for(&e, "the first index", |s| s.indexed_shots == 4 && s.pass.is_none());
        assert!(s.stopped.is_none(), "{s:?}");
        assert_eq!(ev.pass_starts(), [PassKind::FirstIndex], "the first index ran and nothing followed it");
        let s = e.status();
        assert_eq!((s.unreadable, s.waiting, s.pass.clone()), (1, 0, None), "{s:?}");
        assert_eq!(fake.calls.load(Relaxed), 1, "the unreadable file was read once");

        // A library check with nothing new starts nothing.
        e.on_library(lib.clone());
        assert!(ev.pass_starts().is_empty(), "no pass for a file already known to be unreadable");
        assert_eq!(fake.calls.load(Relaxed), 1);
        assert_eq!(e.status().unreadable, 1);
        let bad = d.join("lib/2025/Mar-2025/12-Mar-2025/JPG/0003.JPG");
        let saved = Index::load(&d.join("lib"), &d.join("index"), models::info(ModelId::Siglip2));
        assert!(saved.paths.is_failed(Path::new("2025/Mar-2025/12-Mar-2025/JPG/0003.JPG"), std::fs::metadata(&bad).unwrap().len(), &crate::model::record::MTime::from_metadata(&std::fs::metadata(&bad).unwrap())), "the failure is on disk: {:?}", saved.paths.failed);

        // The file changed size: tried again (and fails again, once).
        std::fs::write(&bad, "not really a jpeg 3, rewritten").unwrap();
        e.on_library(rescan(&d.join("lib")));
        assert_eq!(ev.pass_starts(), [PassKind::NewExports], "a changed file is a job again");
        assert_eq!(fake.calls.load(Relaxed), 2);
        assert_eq!(e.status().unreadable, 1);

        // Its mtime changed, the size not: tried once more.
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&bad).unwrap().set_modified(later).unwrap();
        e.on_library(rescan(&d.join("lib")));
        assert_eq!(ev.pass_starts(), [PassKind::NewExports], "a touched file is a job again");
        assert_eq!(fake.calls.load(Relaxed), 3);
        let s = e.status();
        assert_eq!((s.unreadable, s.indexed_shots, s.pass.clone(), s.stopped.clone()), (1, 4, None, None), "{s:?}");
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A failure that isn't the file's own (an I/O error, as a share that
    /// went away gives) isn't remembered, but the pass's end still starts
    /// no pass over it: the next library check tries it once more.
    #[test]
    fn a_passing_failure_is_tried_again_at_the_next_check_and_never_at_the_pass_end() {
        use std::sync::atomic::Ordering::Relaxed;
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library("transient", 3);
        let mut f = Fake::new(Device::Gpu);
        f.fail_paths_containing = Some("0001".into());
        f.fail_errno = Some(libc::EIO);
        let fake = Arc::new(f);
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
        let mut ev = Events::new(&rx);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 3);
        e.start_first_index(&[ModelId::Siglip2]);
        let s = wait_for(&e, "the first index", |s| s.indexed_shots == 2 && s.pass.is_none());
        assert!(s.stopped.is_none() && s.unreadable == 0, "{s:?}");
        assert_eq!(ev.pass_starts(), [PassKind::FirstIndex], "nothing followed the first index");
        assert_eq!(fake.calls.load(Relaxed), 1);
        e.on_library(lib.clone());
        assert_eq!(ev.pass_starts(), [PassKind::NewExports], "the check tries it once more, and that pass ends it");
        assert_eq!(fake.calls.load(Relaxed), 2);
        assert_eq!(e.status().unreadable, 0);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// × on a running first index: the batch in flight finishes, then no
    /// pass starts on its own — not at the pass's end, not for a library
    /// check, not for the setting — and `stopped` stays Cancelled until
    /// the user asks again.
    #[test]
    fn the_stop_button_holds_until_the_user_asks_again() {
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library("cancel", 320);
        let mut f = Fake::new(Device::Gpu);
        f.delay = Duration::from_millis(100);
        let fake = Arc::new(f);
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
        let mut ev = Events::new(&rx);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 320);
        e.start_first_index(&[ModelId::Siglip2]);
        wait_for(&e, "the first batch", |s| s.indexed_shots > 0 && s.pass.is_some());
        e.stop_indexing();
        let s = wait_for(&e, "the stop", |s| s.pass.is_none());
        assert_eq!(s.stopped, Some(Stopped::Cancelled), "{s:?}");
        assert!(s.indexed_shots > 0 && s.indexed_shots < 320, "stopped part-way: {s:?}");
        let indexed = s.indexed_shots;
        assert_eq!(ev.pass_starts(), [PassKind::FirstIndex], "nothing started at the stopped pass's end");

        e.on_library(lib.clone());
        e.set_index_exports(false);
        e.set_index_exports(true);
        assert!(ev.pass_starts().is_empty(), "× holds through a library check and the setting");
        let s = e.status();
        assert_eq!((s.stopped.clone(), s.indexed_shots, s.pass.clone()), (Some(Stopped::Cancelled), indexed, None), "{s:?}");

        // Asking again carries on where it stopped.
        e.try_card_again();
        let s = wait_for(&e, "the rest", |s| s.indexed_shots == 320 && s.pass.is_none());
        assert!(s.stopped.is_none(), "{s:?}");
        assert_eq!(ev.pass_starts(), [PassKind::NewExports]);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An embedder whose space is the vocabulary's axes: a text names
    /// vocabulary words and encodes to their axes (equally weighted, unit
    /// length); a file named `NNNN-word-60-other-30.JPG` embeds with
    /// cosine 0.60 along "word" and 0.30 along "other", the rest spread
    /// thin.  So a query for "dog" finds the dog files, "fursuit" beats
    /// "dog" where the file says so, and the tags are what the names say.
    struct Axes;

    impl Axes {
        fn n() -> usize {
            vocab::VOCABULARY.len() + 1
        }

        fn unit(parts: &[(usize, f32)]) -> Vec<f32> {
            let n = Self::n();
            let mut v = vec![0f32; n];
            for &(i, c) in parts {
                v[i] = c;
            }
            let used = v.iter().map(|x| x * x).sum::<f32>();
            // The remainder goes on the spare axis, so no word's axis
            // carries more than the name says.
            v[n - 1] = (1.0 - used).max(0.0).sqrt();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            v.iter_mut().for_each(|x| *x /= norm);
            v
        }

        /// `NNNN-dog-60-fursuit-30` → [(dog, 0.6), (fursuit, 0.3)].
        fn parts_of(stem: &str) -> Vec<(usize, f32)> {
            let toks: Vec<&str> = stem.split('-').collect();
            let mut out = Vec::new();
            let mut i = 1;
            while i + 1 < toks.len() {
                let word = toks[i].replace('_', " ");
                if let (Some(w), Ok(pct)) = (vocab::find(&word), toks[i + 1].parse::<f32>()) {
                    out.push((w, pct / 100.0));
                }
                i += 2;
            }
            out
        }
    }

    impl Embedder for Axes {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            (Some("Axes card".into()), SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 })
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let stem = crate::util::stem(p);
                    let frame: i64 = stem.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let base = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap().and_hms_opt(12, 0, 0).unwrap();
                    let embedding = crate::ml::client::Embedding {
                        key: Key::of(1, p.as_os_str().as_encoded_bytes()),
                        capture: Some(base + chrono::Duration::minutes(frame)),
                        meta: crate::model::record::ShotMeta::default(),
                        accidental: stem.contains("accident"),
                        vector: Axes::unit(&Axes::parts_of(&stem)),
                    };
                    crate::ml::client::ImageResult { path: p.clone(), result: Ok(embedding) }
                })
                .collect())
        }
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Ok(texts
                .iter()
                .map(|t| {
                    if let Some(w) = vocab::VOCABULARY.iter().position(|w| w.prompt.is_some_and(|p| p == t)) {
                        return Axes::unit(&[(w, 1.0)]);
                    }
                    let inner = t.strip_prefix("a photo of a ").and_then(|s| s.strip_suffix('.')).unwrap_or(t);
                    let words = vocab::words_in(inner);
                    if words.is_empty() {
                        return Axes::unit(&[]);
                    }
                    let each = 1.0 / (words.len() as f32).sqrt();
                    Axes::unit(&words.iter().map(|&w| (w, each)).collect::<Vec<_>>())
                })
                .collect())
        }
    }

    /// A scratch library of the given files (day folder, file name).
    fn library_of(name: &str, files: &[(&str, &str)]) -> (Arc<LibraryIndex>, PathBuf) {
        let d = crate::search::index::tests::scratch(name);
        let root = d.join("lib");
        for (day, file) in files {
            let folder = root.join(day).join("JPG");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join(file), format!("not a jpeg: {file}")).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let scanned = crate::model::library_cache::scan(&root, None, true, &cancel, &|_| {}).expect("scans").expect("not cancelled");
        let store = Mutex::new(crate::model::library_cache::Store::new(root.clone()));
        let ix = crate::model::library_index::index_from_tree(&root, &scanned.tree, &store, &cancel).expect("an index");
        assert_eq!(ix.total_shots, files.len());
        (Arc::new(ix), d)
    }

    /// The query syntax, the boost and the kinds rule, end to end: a real
    /// animal's query never carries a fursuit in its list (the fursuits
    /// come apart), Things holds objects and scenes only, `-word` and
    /// dates narrow, quotes are exact, `@name` searches nothing.
    #[test]
    fn the_syntax_the_tags_and_the_kinds_rule_hold_through_the_engine() {
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library_of(
            "syntax",
            &[
                ("2025/Mar-2025/12-Mar-2025", "0001-dog-60.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0002-dog-30-fursuit-50.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0003-dog-10.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0004-crowd-50-fireworks-20.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0005-fireworks-30.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0006-camera-40.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0007-fursuit-60.JPG"),
                ("2025/Mar-2025/12-Mar-2025", "0008-camera-50-accident.JPG"),
                ("2024/Jun-2024/08-Jun-2024", "0009-camera-30.JPG"),
                ("2024/Jun-2024/08-Jun-2024", "0010-dog-50.JPG"),
                ("2024/Jun-2024/08-Jun-2024", "0011-stuffed_animal-30-fursuit-50.JPG"),
                ("2023/May-2023/05-May-2023", "0012-snow-50.JPG"),
                ("2023/May-2023/05-May-2023", "0013-snow-30-infrared-50.JPG"),
                ("2023/May-2023/05-May-2023", "0014-sunset-40-infrared-60.JPG"),
            ],
        );
        let asked: Arc<Mutex<Vec<(Device, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let factory: EmbedderFactory = Box::new(move |device, text_only| {
            log.lock().push((device, text_only));
            Ok(Arc::new(Axes) as Arc<dyn Embedder>)
        });
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 14);
        e.start_first_index(&[ModelId::Siglip2]);
        wait_for(&e, "the first index", |s| s.indexed_shots == 14 && s.pass.is_none());
        let mut ev = Events::new(&rx);
        let name = |h: &Hit| crate::util::stem(h.jpg.as_deref().unwrap()).split('-').next().unwrap().to_string();
        let names = |hs: &[Hit]| hs.iter().map(name).collect::<Vec<_>>();

        // Things: objects and scenes with photos, most first; never a
        // subject kind, never the accidental frame; the plush toy that is
        // really a fursuit and the snow that is really infrared foliage
        // aren't counted.
        let things = ev.things(&e);
        let list: Vec<(String, usize)> = things.iter().map(|t| (t.word.clone(), t.photos)).collect();
        assert_eq!(list, [("cameras".to_string(), 2), ("fireworks".to_string(), 2), ("infrared".to_string(), 2), ("snow".to_string(), 1)], "{things:?}");
        assert!(things.iter().all(|t| !t.kind.is_subject()));
        assert_eq!(*e.things(), *things);
        assert!(d.join("index").join("vocab-siglip2-b16-256.bin").is_file(), "the vocabulary's vectors are kept beside the index");

        // "dog": the real dogs, tagged first (the weak one after), the
        // canine fursuit in the list of its own, never in the main one.
        e.query(1, "dog", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, indexed, error, understood, fursuits, animal) = ev.results(1);
        assert_eq!((indexed, error), (14, None));
        assert_eq!(names(&hits), ["0001", "0010", "0003"], "{hits:?}");
        assert_eq!(hits.iter().map(|h| h.tagged).collect::<Vec<_>>(), [true, true, false]);
        assert_eq!(names(&fursuits), ["0002"]);
        assert_eq!(animal.as_deref(), Some("dog"));
        assert_eq!(understood.words, "dog");
        assert!(hits.iter().all(|h| fursuits.iter().all(|f| f.id != h.id)));

        // "dog fursuit" asks for suits: no contrast, no section.
        e.query(2, "dog fursuit", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, _, fursuits, animal) = ev.results(2);
        assert!(names(&hits).contains(&"0002".to_string()) && fursuits.is_empty() && animal.is_none(), "{hits:?}");

        // Dates filter on the shot's day, before the split.
        e.query(3, "dogs 2024", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, fursuits, _) = ev.results(3);
        assert_eq!(names(&hits), ["0010"]);
        assert!(fursuits.is_empty());
        assert_eq!(understood.dates.map(|r| r.label()).as_deref(), Some("2024"));
        e.query(4, "camera jun 2024", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, ..) = ev.results(4);
        assert_eq!(names(&hits), ["0009"]);

        // "-word": more like the words than like the word.
        e.query(5, "fireworks -crowd", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, ..) = ev.results(5);
        assert_eq!(names(&hits), ["0005"], "the crowd shot with a little fireworks goes");
        assert_eq!(understood.exclude, ["crowd"]);
        e.query(6, "fireworks", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, ..) = ev.results(6);
        assert_eq!(names(&hits), ["0005", "0004"]);

        // Quotes: the prompt as typed; the accidental frame never shows.
        e.query(7, "\"camera\"", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, ..) = ev.results(7);
        assert!(understood.exact);
        assert_eq!(names(&hits), ["0006", "0009"]);
        assert!(hits.iter().all(|h| h.tagged));

        // @name nobody has: it narrows to nothing, with no error (the page
        // tells, since the name resolved to no subject); with words the
        // same, since the words are searched within the person's photos.
        e.query(8, "@Someone", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, error, understood, ..) = ev.results(8);
        assert!(hits.is_empty() && error.is_none() && understood.is_empty());
        assert_eq!(understood.names, ["Someone"]);
        e.query(9, "@Someone camera", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, ..) = ev.results(9);
        assert!(hits.is_empty(), "{hits:?}");
        assert_eq!(understood.names, ["Someone"]);

        // Dates alone browse the period: every shot of those days in
        // shooting order, oldest first, a neutral score, the accidental
        // frame out; with a `-word`, minus what a search for it would show.
        e.query(11, "2024", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, error, understood, fursuits, animal) = ev.results(11);
        assert_eq!(names(&hits), ["0009", "0010", "0011"]);
        assert!(hits.iter().all(|h| h.score == 0.0 && !h.tagged) && hits.windows(2).all(|w| w[0].capture < w[1].capture));
        assert!(error.is_none() && fursuits.is_empty() && animal.is_none() && understood.is_empty());
        assert_eq!(understood.dates.map(|r| r.label()).as_deref(), Some("2024"));
        e.query(12, "mar 2025", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, ..) = ev.results(12);
        assert_eq!(names(&hits), ["0001", "0002", "0003", "0004", "0005", "0006", "0007"], "the accidental 0008 stays out");
        e.query(13, "2025 -fursuit", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, ..) = ev.results(13);
        assert_eq!(names(&hits), ["0001", "0003", "0004", "0005", "0006"], "the two a fursuit search finds are left out");
        assert_eq!(understood.exclude, ["fursuit"]);
        e.query(14, "@Someone 2024", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, understood, ..) = ev.results(14);
        assert!(hits.is_empty(), "an unknown name narrows the dates to nothing");
        assert_eq!(understood.names, ["Someone"]);
        e.query(15, "-fursuit", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, error, ..) = ev.results(15);
        assert!(hits.is_empty() && error.is_none(), "a -word alone runs nothing");
        e.query(16, "2022", super::super::CLOSENESS_DEFAULT, &[]);
        assert!(ev.results(16).0.is_empty(), "no photos in that year");

        // The infrared contrast: "snow" counts only where snow beats
        // infrared; "infrared" finds the frames; naming both waives it.
        e.query(17, "snow", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, _, fursuits, animal) = ev.results(17);
        assert_eq!(names(&hits), ["0012"], "the infrared frame that looks like snow is simply dropped");
        assert!(fursuits.is_empty() && animal.is_none());
        e.query(18, "sunset", super::super::CLOSENESS_DEFAULT, &[]);
        assert!(ev.results(18).0.is_empty());
        e.query(19, "infrared", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, ..) = ev.results(19);
        assert_eq!(names(&hits), ["0014", "0013"]);
        assert!(hits.iter().all(|h| h.tagged));
        e.query(20, "snow infrared", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, ..) = ev.results(20);
        assert!(names(&hits).contains(&"0013".to_string()), "{hits:?}");

        // An animal nothing matches: empty, still an animal query.
        e.query(10, "cat", super::super::CLOSENESS_DEFAULT, &[]);
        let (hits, _, _, _, fursuits, animal) = ev.results(10);
        assert!(hits.is_empty() && fursuits.is_empty() && animal.as_deref() == Some("cat"));

        // Only the card was asked for anything: text and the vocabulary
        // went through the same helper (text runs on the processor inside
        // it), never a second one.
        assert!(asked.lock().iter().all(|c| *c == (Device::Gpu, false)), "{:?}", asked.lock());
        e.shutdown();
        std::thread::sleep(Duration::from_millis(50));

        // A second engine on the same index: the tags come back from the
        // cached vocabulary with no helper at all, and Clear empties Things.
        let (bus2, rx2) = crate::bus::channel();
        let asked2: Arc<Mutex<Vec<(Device, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = asked2.clone();
        let factory: EmbedderFactory = Box::new(move |device, text_only| {
            log2.lock().push((device, text_only));
            Ok(Arc::new(Axes) as Arc<dyn Embedder>)
        });
        let e2 = Engine::with_config(bus2, Device::Gpu, true, true, ModelId::AuraFace, Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None });
        e2.on_library(lib.clone());
        wait_for(&e2, "the saved index", |s| s.indexed_shots == 14);
        let things = Events::new(&rx2).things(&e2);
        assert_eq!(things.len(), 4);
        assert!(asked2.lock().is_empty(), "no helper was needed for the tags");
        e2.clear_index();
        wait_for(&e2, "the clear", |s| s.indexed_shots == 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !e2.things().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(e2.things().is_empty());
        e2.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }
}
