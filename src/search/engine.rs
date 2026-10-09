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

use super::faces::{FaceStore, People};
use super::heads::{self, CloseCall, Decision as HeadDecision, Fursuits, HeadStore, Why as HeadWhy};
use super::judging::{self, PairCache, PairKey, Runner, Task};
use crate::ml::judge::{JudgeChoice, JudgeState, JudgeStatus, PairAsk};
use crate::ml::judge_client::{JudgeError, JudgeFactory};
use super::index::{Index, Key, PathEntry, Record};
use super::parse::{self, Parsed};
use super::tags::{Tags, VocabVectors};
use super::{FacesStatus, Hit, PassKind, PassProgress, Status, Stopped, Thing, pass, query, vocab};
use crate::bus::{BgEvent, Bus};
use crate::ml::client::{Client, Embedder, EmbedderFactory, HelperError, Spec, StartError, Want};
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
    ForgetKind(Kind),
    Faces { seq: u64, group: GroupId },
    MoveFaces { from: GroupId, faces: Vec<FaceRef>, to: Option<GroupId> },
    Answer { group: GroupId, faces: Vec<FaceRef>, yes: Option<bool> },
    Suggestions { seq: u64, group: GroupId },
    Undo,
    /// A regroup finished on its worker (either kind; `kind` says).
    Regrouped(Box<super::step::Regrouped>),
    // Fursuits (phase 4, decision 11).  Sent by the fursuit API, which
    // the pages call once they switch from their stand-in.
    SetFindHeads(bool),
    FindHeads,
    AnswerHeads(bool),
    StopHeads,
    FindMore { seq: u64, group: GroupId },
    // Fursuit matching v2.
    CatchUpHeadVectors,
    /// Tests: whiten with fewer heads than a library needs.
    #[cfg(test)]
    MinFit(usize),
    SetJudge(JudgeChoice),
    DownloadJudge(JudgeChoice),
    DeleteJudge(JudgeChoice),
    StopJudging,
    /// From the judge's runner thread.
    JudgeEvent(judging::Event),
    /// Tests: the judge to use (a stand-in).
    #[cfg(test)]
    JudgeFactory(JudgeFactory),
    /// Tests: whiten with this (hand-made vectors at chosen distances).
    #[cfg(test)]
    Whiten(Box<super::whiten::Whitening>),
    /// A whitening fit finished on its worker (`None`: too few heads).
    Whitened(Option<Box<super::whiten::Whitening>>),
    Shutdown,
}

/// Sizes Preferences ▸ Storage shows (measured by walking folders:
/// call [`storage`] on a worker).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    /// The search index's files (the face files apart).
    pub index_bytes: u64,
    pub index_shots: usize,
    pub models_bytes: u64,
    /// The faces found, their vectors and tiles (the *People* row; the
    /// user's names live elsewhere and aren't counted).
    pub faces_bytes: u64,
    /// The head cache's files (the Storage row's *Fursuits*), vector
    /// files of models no longer run included.
    pub heads_bytes: u64,
    /// The close-call judges' model files (not in `models_bytes`), and
    /// the verdicts kept beside the index (not in `index_bytes`; v2).
    pub judge_bytes: u64,
    /// Each judge's folder, in the list's order (0 when absent); they sum
    /// to `judge_bytes`.
    pub judges: Vec<(JudgeChoice, u64)>,
    pub judged_bytes: u64,
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
    /// The face tiles' index, for `Engine::crop` on any thread.
    tiles: Mutex<Option<Arc<Mutex<super::faces::TileIndex>>>>,
    /// The head tiles, likewise (phase 4).
    head_tiles: Mutex<Option<Arc<Mutex<heads::HeadTiles>>>>,
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
    /// Fetches models (`ml::download::execute`; a double in tests, which
    /// must never reach the network).
    pub downloader: Downloader,
}

/// Downloads the given models, reporting progress, and returns the ids
/// that ended checked.
pub type Downloader = Arc<dyn Fn(&[ModelId], &AtomicBool, &mut dyn FnMut(BgEvent)) -> Vec<ModelId> + Send + Sync>;

impl Config {
    /// The app's: the real helper, the registry, the cache, the user's
    /// data folder, the real downloads.
    pub fn real() -> Config {
        Config {
            factory: Client::factory(),
            installed: Box::new(models::installed),
            index_dir: None,
            subjects_path: super::faces::default_file_path(),
            downloader: Arc::new(|ids, cancel, emit| crate::ml::download::execute(ids, cancel, emit)),
        }
    }
}

/// A downloader for tests that must never fetch anything: it fails the
/// test if asked.
#[cfg(test)]
pub fn no_downloads() -> Downloader {
    Arc::new(|ids, _, _| panic!("a test asked to download {ids:?}"))
}

impl Engine {
    /// Start the engine with the real helper.  `device`, `index_exports`,
    /// `find_faces`, `face_model` and `find_heads` are the saved
    /// preferences.
    pub fn new(bus: Bus, device: Device, index_exports: bool, find_faces: bool, face_model: ModelId, find_heads: bool) -> Engine {
        Engine::with_config(bus, device, index_exports, find_faces, face_model, find_heads, Config::real())
    }

    /// Start the engine on a configuration (tests).
    pub fn with_config(bus: Bus, device: Device, index_exports: bool, find_faces: bool, face_model: ModelId, find_heads: bool, config: Config) -> Engine {
        let (tx, rx) = crossbeam_channel::unbounded();
        let faces = FacesStatus { on: find_faces, model: Some(face_model), ..FacesStatus::default() };
        let status = Status { index_exports, device: crate::ml::DeviceStatus { chosen: device, ..Default::default() }, faces, ..Status::default() };
        let shared = Arc::new(Shared { status: Mutex::new(status), things: Mutex::new(Arc::new(Vec::new())), subjects: Mutex::new(Arc::new(Subjects::default())), tiles: Mutex::new(None), head_tiles: Mutex::new(None) });
        let s = shared.clone();
        let me = tx.clone();
        let Config { factory, installed, index_dir, subjects_path, downloader } = config;
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
                    downloader,
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
                    face_pass_pending: None,
                    subjects_sent: None,
                    regroup_running: false,
                    regroup_wanted: false,
                    find_heads,
                    fursuits: Fursuits::new(find_heads),
                    scan: None,
                    head_scan_pending: None,
                    embedder_has_heads: false,
                    heads_regroup_running: false,
                    heads_regroup_wanted: false,
                    fitting: false,
                    judge_factory: crate::ml::judge_client::factory(),
                    judge_choice: JudgeChoice::Off,
                    runner: None,
                    verdicts: None,
                    judge_state: JudgeState::Idle,
                    judging: None,
                    judge_token: 0,
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

    /// A face's or head's pixels for a tile or the review, `px`×`px`
    /// (or fitted, for [`Framing::Photo`]), as `(width, height, RGBA)`.
    /// **Blocks**: reads the cached tile (a head's, else a face's), else
    /// the photo's preview; call it on a worker, never the UI thread.
    pub fn crop(&self, face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
        if framing == Framing::Face {
            let head_tiles = self.shared.head_tiles.lock().clone();
            if let Some(t) = head_tiles {
                if let Some(px) = heads::tile_pixels(&t.lock(), &face.face, px) {
                    return Some(px);
                }
            }
        }
        let tiles = self.shared.tiles.lock().clone();
        match tiles {
            Some(t) => super::faces::crop(Some(&t.lock()), face, px, framing),
            None => super::faces::crop(None, face, px, framing),
        }
    }

}

/// The fursuit API (phase 4, decision 11): what the Fursuits tab,
/// Preferences and the review call.
impl Engine {
    /// Preferences ▸ People ▸ *Forget all people* (or Fursuits ▸ *Forget
    /// all fursuits*): that kind's subjects, detections, vectors and
    /// tiles go; the other kinds are not touched.  For fursuits the head
    /// step's counts start over, so the catch-up is offered again.
    /// `forget_all` stays for Storage and a rebuild.
    pub fn forget_kind(&self, kind: Kind) {
        self.send(Cmd::ForgetKind(kind));
    }

    /// Preferences ▸ *Find fursuit heads*.  Off: no scan starts (what was
    /// found stays until *Forget all*).
    pub fn set_find_heads(&self, on: bool) {
        self.send(Cmd::SetFindHeads(on));
    }

    /// *Find fursuit heads in the N photos*: the catch-up over indexed
    /// fursuit photos not looked at (and ones skipped earlier), asked
    /// from the tab or Preferences after the cost was shown.  Downloads
    /// the two models first if need be.  On either device it starts,
    /// since the asking happened on the tab.
    pub fn find_heads(&self) {
        self.send(Cmd::FindHeads);
    }

    /// The answer to `Step::Asking` (the processor): start the scan, or
    /// skip it (its photos are remembered and offered again from the
    /// tab).
    pub fn answer_heads(&self, start: bool) {
        self.send(Cmd::AnswerHeads(start));
    }

    /// Stop a running head scan (its progress is kept).
    pub fn stop_heads(&self) {
        self.send(Cmd::StopHeads);
    }

    /// *Find more*, strict: `BgEvent::Suggestions { seq, .. }` with the
    /// next few heads (or faces) under the strict cut-off, one entry per
    /// burst; an empty list means nothing more is close enough.
    pub fn find_more(&self, seq: u64, group: GroupId) {
        self.send(Cmd::FindMore { seq, group });
    }

    /// *Update N photos' heads* (v2): the heads found before fursuit
    /// matching v2 get the search model's vectors, their photos read
    /// again on the chosen device (`HeadsStatus::vectors_missing`, the
    /// cost from `estimate::head_vector_seconds`).  Only ever started by
    /// this command, never on its own.
    pub fn catch_up_head_vectors(&self) {
        self.send(Cmd::CatchUpHeadVectors);
    }

    /// Tests: whiten with as few as `n` heads.
    #[cfg(test)]
    pub fn set_min_fit(&self, n: usize) {
        self.send(Cmd::MinFit(n));
    }

    /// Tests: judge close calls with this (a stand-in), not the helper.
    #[cfg(test)]
    pub fn set_judge_factory(&self, f: JudgeFactory) {
        self.send(Cmd::JudgeFactory(f));
    }

    /// Tests: whiten the head vectors with `w`.
    #[cfg(test)]
    pub fn set_whitening(&self, w: super::whiten::Whitening) {
        self.send(Cmd::Whiten(Box::new(w)));
    }
}

/// The close-call judge (fursuit matching v2): Preferences ▸ Fursuits ▸
/// *Close-call judge* and the review.  Suggestions only, fursuits only:
/// the judge filters what `suggestions` and `find_more` offer for a
/// fursuit subject (`BgEvent::Judged` after `BgEvent::Suggestions`);
/// nothing is named, merged or marked by it.  Card only: with the
/// processor chosen, or the judge failed, close calls use the vector
/// cut-offs as with *Off*.
impl Engine {
    /// The choice (*Off* or a judge).  Its model is downloaded by
    /// [`Engine::download_judge`], never by this.
    pub fn set_judge(&self, choice: JudgeChoice) {
        self.send(Cmd::SetJudge(choice));
    }

    /// Download a judge's model (the dialog's progress events).
    pub fn download_judge(&self, choice: JudgeChoice) {
        self.send(Cmd::DownloadJudge(choice));
    }

    /// Delete a judge's model files (Preferences ▸ Storage).
    pub fn delete_judge(&self, choice: JudgeChoice) {
        self.send(Cmd::DeleteJudge(choice));
    }

    /// Stop judging the current close calls (they stay unjudged).
    pub fn stop_judging(&self) {
        self.send(Cmd::StopJudging);
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
    storage_in(library_root.and_then(super::index::dir_for).as_deref(), models::models_dir().as_deref())
}

/// [`storage`] for an index folder and a models folder.  Every model
/// folder counts, ones no longer used too (phase 4's DINOv2 stays
/// counted until *Delete* removes it); the judges' folders are counted
/// apart.
pub fn storage_in(index_dir: Option<&Path>, models_dir: Option<&Path>) -> Storage {
    let mut out = Storage::default();
    if let Some(dir) = index_dir {
        out.index_bytes = dir_bytes(dir);
        out.index_shots = super::index::count_rows(dir);
        out.faces_bytes = FaceStore::bytes_in(dir);
        out.heads_bytes = HeadStore::bytes_in(dir);
        out.judged_bytes = PairCache::bytes_in(dir);
        out.index_bytes = out.index_bytes.saturating_sub(out.faces_bytes + out.heads_bytes + out.judged_bytes);
    }
    if let Some(dir) = models_dir {
        out.judges = JudgeChoice::ALL.iter().filter_map(|&c| Some((c, dir_bytes(&dir.join(c.model()?.dir_name()))))).collect();
        out.judge_bytes = out.judges.iter().map(|j| j.1).sum();
        out.models_bytes = dir_bytes(dir).saturating_sub(out.judge_bytes);
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
    downloader: Downloader,
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
    /// A face pass waiting for its models: the catch-up (`false`) or a
    /// model change (`true`).
    face_pass_pending: Option<bool>,
    /// When the *Subjects* snapshot was last sent (throttled in a pass).
    subjects_sent: Option<Instant>,
    /// A regroup is on its worker; another was asked for meanwhile.
    regroup_running: bool,
    regroup_wanted: bool,
    /// Preferences ▸ *Find fursuit heads* (phase 4).
    find_heads: bool,
    /// The fursuit step: its cache, its grouping, what waits for the user.
    fursuits: Fursuits,
    /// A head scan runs in place of a pass (never both at once).
    scan: Option<HeadScan>,
    /// A head scan waiting for its models to download: why, and whether
    /// the tab asked for it.
    head_scan_pending: Option<(HeadWhy, bool)>,
    /// The running helper was started with the head models.
    embedder_has_heads: bool,
    heads_regroup_running: bool,
    heads_regroup_wanted: bool,
    /// A whitening fit is on its worker (v2).
    fitting: bool,
    /// The close-call judge (v2): the choice, its runner (none until a
    /// close call needs it), the verdicts kept (choice, index folder),
    /// where it is, and the review it is judging for.
    judge_factory: JudgeFactory,
    judge_choice: JudgeChoice,
    runner: Option<Runner>,
    verdicts: Option<(JudgeChoice, Option<PathBuf>, PairCache)>,
    judge_state: JudgeState,
    judging: Option<Judging>,
    judge_token: u64,
}

/// The close calls of the request being judged.
struct Judging {
    seq: u64,
    group: GroupId,
    token: u64,
    calls: Vec<CloseCall>,
    /// Indices into `calls` still to judge, nearest first.
    pending: Vec<usize>,
    done: usize,
    total: usize,
    /// Close calls past the cap, left for the next request.
    beyond: usize,
}

/// A running head scan.
struct HeadScan {
    handle: heads::ScanHandle,
    batches: Receiver<heads::HeadBatch>,
}

const TEXT_CACHE: usize = 64;
/// While a pass runs, *Things* is sent at most this often.
const THINGS_EVERY: Duration = Duration::from_secs(2);
/// How often the idle engine looks whether built memory can go (the
/// fursuit vectors after [`Fursuits::VECTORS_IDLE`]).
const IDLE_TICK: Duration = Duration::from_secs(15);

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
                    default(IDLE_TICK) => { self.idle_tick(); None }
                }
            } else if let Some(sc) = &self.scan {
                crossbeam_channel::select! {
                    recv(self.rx) -> c => match c { Ok(c) => Some(c), Err(_) => break },
                    recv(sc.batches) -> b => {
                        match b {
                            Ok(batch) => { self.on_head_batch(batch); }
                            Err(_) => { self.scan = None; self.fursuits.stop(); self.publish_heads(); }
                        }
                        None
                    }
                    default(IDLE_TICK) => { self.idle_tick(); None }
                }
            } else {
                match self.rx.recv_timeout(IDLE_TICK) {
                    Ok(c) => Some(c),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        self.idle_tick();
                        None
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
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
                    // A helper started without the face models loads them
                    // only when started again.
                    if ok.iter().any(|m| m.is_face_model() || *m == ModelId::Yunet) {
                        self.drop_embedder();
                    }
                    // The first index waits for search's model, which may
                    // have been in place all along (only the face models
                    // were fetched): what matters is that it is ready now,
                    // not that it was in this download.  A download that
                    // failed or was cancelled leaves it missing, and
                    // nothing starts.  Faces still missing (their download
                    // failed) don't hold search back: the pass runs without
                    // them, and the tab offers the catch-up.
                    let first = std::mem::take(&mut self.first_index_pending);
                    if first && self.models_ready() {
                        self.start_pass(PassKind::FirstIndex);
                    }
                    if let Some(change) = self.face_pass_pending.take() {
                        if self.faces_ready() && self.models_ready() {
                            self.start_face_pass(change);
                        }
                    }
                    if ok.iter().any(|m| m.is_fursuit_model()) {
                        self.drop_embedder();
                    }
                    if let Some((why, asked)) = self.head_scan_pending.take() {
                        if self.heads_ready() {
                            self.offer_head_scan(why, asked);
                        }
                    }
                    self.publish_judge();
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
                    self.face_pass_pending = None;
                }
                Cmd::TryCardAgain => {
                    self.drop_embedder();
                    self.update(|s| {
                        s.stopped = None;
                        s.device.self_check = SelfCheck::NotRun;
                    });
                    self.fursuits.card_back();
                    self.publish_heads();
                    if matches!(self.judge_state, JudgeState::Failed(_)) {
                        self.runner = None;
                        self.judge_state = JudgeState::Idle;
                        self.publish_judge();
                    }
                    self.resume_pass();
                }
                Cmd::SetDevice(d) => {
                    if d != self.device {
                        let was_running = self.pass.is_some();
                        self.cancel_pass();
                        self.cancel_scan();
                        self.drop_embedder();
                        self.device = d;
                        // The judge runs on the card or not at all.
                        self.forget_judging();
                        self.drop_runner();
                        if matches!(self.judge_state, JudgeState::Failed(_)) {
                            self.judge_state = JudgeState::Idle;
                        }
                        self.publish_judge();
                        self.fursuits.card_back();
                        self.publish_heads();
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
                    // Unnamed detections are cache and go with it (§10);
                    // the user's file stays.
                    if let Some(store) = &mut self.people.store {
                        store.forget_all();
                    }
                    self.people.reset_grouping();
                    self.cancel_scan();
                    self.fursuits.forget_all();
                    self.fursuits.reset_grouping();
                    self.forget_verdicts();
                    self.publish_subjects();
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
                    if let Some(store) = &mut self.people.store {
                        store.forget_all();
                    }
                    self.people.reset_grouping();
                    self.cancel_scan();
                    self.fursuits.forget_all();
                    self.fursuits.reset_grouping();
                    self.forget_verdicts();
                    self.publish_subjects();
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
                        models::delete_all_but_judges(&dir);
                    }
                    self.update(|s| s.models_installed = false);
                }
                Cmd::Query { seq, text, closeness, subjects } => self.query(seq, &text, closeness, &subjects),
                Cmd::Busy(n) => {
                    if let Some(p) = &self.pass {
                        p.handle.set_busy(n);
                    }
                    if let Some(sc) = &self.scan {
                        sc.handle.set_busy(n);
                    }
                }
                Cmd::SetFindFaces(on) => {
                    self.find_faces = on;
                    self.update(|s| s.faces.on = on);
                }
                Cmd::SetFaceModel(model) => {
                    if models::face_models().any(|m| m.id == model) && model != self.face_model {
                        // The faces found and the user's work stay; the
                        // vectors are made again with the new model.
                        if matches!(self.pass.as_ref().map(|p| p.kind), Some(PassKind::Faces)) {
                            self.cancel_pass();
                        }
                        self.face_model = model;
                        if let Some(store) = &mut self.people.store {
                            store.set_model(model);
                        }
                        self.people.reset_grouping();
                        self.drop_embedder();
                        self.update(|s| s.faces.model = Some(model));
                        self.refresh_installed();
                        self.refresh_counts();
                        self.publish_subjects();
                        self.face_pass_or_download(true);
                    }
                }
                Cmd::FindFaces => self.face_pass_or_download(false),
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
                Cmd::ForgetKind(kind) => {
                    self.people.forget_kind(kind);
                    match kind {
                        Kind::Person => self.people.reset_grouping(),
                        Kind::Fursuit => {
                            self.cancel_scan();
                            self.fursuits.forget_all();
                            self.fursuits.reset_grouping();
                            self.forget_verdicts();
                            self.people.set_other_clusters(Kind::Fursuit, Vec::new());
                            self.publish_heads();
                        }
                        Kind::Pet => {}
                    }
                    self.refresh_counts();
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
                Cmd::Suggestions { seq, group } => self.send_suggestions(seq, group, false),
                Cmd::FindMore { seq, group } => self.send_suggestions(seq, group, true),
                Cmd::SetFindHeads(on) => {
                    self.find_heads = on;
                    self.fursuits.status.on = on;
                    if !on {
                        self.cancel_scan();
                    }
                    self.publish_heads();
                }
                Cmd::FindHeads => {
                    if self.heads_ready() {
                        self.offer_head_scan(HeadWhy::CatchUp, true);
                    } else {
                        self.head_scan_pending = Some((HeadWhy::CatchUp, true));
                        self.download_missing(&models::FURSUIT_MODELS);
                    }
                }
                Cmd::AnswerHeads(start) => {
                    if let Some((jobs, _)) = self.fursuits.answer(start) {
                        self.run_head_scan(jobs);
                    }
                    self.publish_heads();
                }
                Cmd::StopHeads => self.cancel_scan(),
                Cmd::CatchUpHeadVectors => self.catch_up_head_vectors(),
                #[cfg(test)]
                Cmd::MinFit(n) => self.fursuits.min_fit = n,
                #[cfg(test)]
                Cmd::Whiten(w) => {
                    let _ = self.me.send(Cmd::Whitened(Some(w)));
                }
                #[cfg(test)]
                Cmd::JudgeFactory(f) => {
                    self.runner = None;
                    self.judge_factory = f;
                }
                Cmd::SetJudge(choice) => {
                    if choice != self.judge_choice {
                        self.forget_judging();
                        // One judge on the card at a time: the one loaded
                        // leaves (its process gone) before another loads.
                        self.drop_runner();
                        self.verdicts = None;
                        self.judge_choice = choice;
                        self.judge_state = JudgeState::Idle;
                    }
                    self.publish_judge();
                }
                Cmd::DownloadJudge(choice) => {
                    if let Some(m) = choice.model() {
                        self.download_missing(&[m]);
                    }
                }
                Cmd::DeleteJudge(choice) => {
                    if choice == self.judge_choice {
                        self.forget_judging();
                        self.drop_runner();
                    }
                    if let Some(dir) = choice.model().and_then(models::model_dir) {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    self.publish_judge();
                }
                Cmd::StopJudging => {
                    self.forget_judging();
                    if !matches!(self.judge_state, JudgeState::Failed(_) | JudgeState::Broken(_)) {
                        self.judge_state = JudgeState::Idle;
                    }
                    self.publish_judge();
                }
                Cmd::JudgeEvent(e) => self.on_judge_event(e),
                Cmd::Whitened(w) => {
                    self.fitting = false;
                    if self.fursuits.apply_fit(w.map(|b| *b)) {
                        self.people.set_other_clusters(Kind::Fursuit, Vec::new());
                        self.publish_subjects();
                        self.start_heads_regroup();
                        self.publish_heads();
                    }
                    // Heads that arrived meanwhile may call for another.
                    self.maybe_fit(false);
                }
                Cmd::Undo => {
                    if let Some(what) = self.people.undo() {
                        let _ = self.bus.unbounded_send(BgEvent::SubjectsUndone(what));
                        self.publish_subjects();
                        self.start_regroup();
                        self.start_heads_regroup();
                    }
                }
                Cmd::Regrouped(r) => match r.kind {
                    Kind::Fursuit => {
                        self.heads_regroup_running = false;
                        if self.fursuits.apply_regroup(*r, &self.people.file) {
                            self.people.set_other_clusters(Kind::Fursuit, self.fursuits.clusters());
                            self.publish_subjects();
                        }
                        if std::mem::take(&mut self.heads_regroup_wanted) {
                            self.start_heads_regroup();
                        }
                    }
                    _ => {
                        self.regroup_running = false;
                        if self.people.apply_regroup(*r) {
                            self.publish_subjects();
                        }
                        if std::mem::take(&mut self.regroup_wanted) {
                            self.start_regroup();
                        }
                    }
                },
                Cmd::Shutdown => break,
            }
        }
        self.cancel_pass();
        if let Some(ix) = &mut self.index {
            let _ = ix.save_paths();
        }
        self.drop_embedder();
        // The judge's helper leaves with the engine (its card memory too).
        if let Some(r) = self.runner.take() {
            r.judge().unload();
        }
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

    /// The detector and the chosen face model are in place.
    fn faces_ready(&self) -> bool {
        self.missing(&models::face_set(self.face_model)).is_empty()
    }

    /// Which models are in place: search's, and the detector with the
    /// chosen face model (stats files, on this thread).
    fn refresh_installed(&mut self) {
        let search = self.models_ready();
        let faces = self.missing(&models::face_set(self.face_model)).is_empty();
        let heads = self.heads_ready();
        self.fursuits.status.models_installed = heads;
        self.update(|s| {
            s.models_installed = search;
            s.faces.installed = faces;
            s.heads.models_installed = heads;
        });
    }

    /// After a subjects edit: a refusal is logged (nothing changed); any
    /// outcome republishes the page's state.
    fn after_edit(&mut self, result: Result<(), Refused>) {
        match result {
            Err(e) => eprintln!("subjects: refused: {e}"),
            // Members and rejections moved: the clusters and the
            // suggestion counts follow, on the worker (each kind its own).
            Ok(()) => {
                self.start_regroup();
                self.start_heads_regroup();
            }
        }
        self.publish_subjects();
    }

    /// The fursuit grouping again on a worker thread (`Cmd::Regrouped`
    /// with `Kind::Fursuit` brings the result); the head vectors are
    /// built in memory for it if they weren't.
    fn start_heads_regroup(&mut self) {
        if self.heads_regroup_running {
            self.heads_regroup_wanted = true;
            return;
        }
        let Some(job) = self.fursuits.regroup_job(&self.people.file) else { return };
        self.heads_regroup_running = true;
        let me = self.me.clone();
        std::thread::Builder::new()
            .name("fursuits-regroup".into())
            .spawn(move || {
                let _t = crate::profile::span("subjects", "fursuits regroup");
                let r = super::step::regroup(&job);
                let _ = me.send(Cmd::Regrouped(Box::new(r)));
            })
            .expect("spawn fursuits-regroup");
    }

    /// Suggestions (or *Find more* with `strict`) for a group, from its
    /// own kind's step only: a fursuit group is answered from heads, any
    /// other from faces; never both.
    fn send_suggestions(&mut self, seq: u64, group: GroupId, strict: bool) {
        self.ensure_resolver();
        let r = if self.fursuits.is_fursuit_group(&self.people.file, &group) {
            // The head vectors in memory first (the fursuit work asks).
            self.fursuits.step();
            // A fursuit subject's close calls go to the judge when it
            // can run (v2); otherwise the vector cut-offs, as before.
            if self.judge_status().active(self.device) {
                if let Some(calls) = self.fursuits.close_calls(&self.people.file, &group, &|f| self.resolve_face(f)) {
                    self.judge_calls(seq, group, calls);
                    return;
                }
            }
            self.fursuits.suggestions(&self.people.file, &group, &|f| self.resolve_face(f), strict)
        } else {
            self.people.suggestions(&group, &|f| self.resolve_face(f), strict)
        };
        let list = match r {
            Ok(l) => l,
            Err(e) => {
                eprintln!("subjects: suggestions refused: {e}");
                Vec::new()
            }
        };
        let _ = self.bus.unbounded_send(BgEvent::Suggestions { seq, group, list: Arc::new(list) });
    }

    /// Nothing asked for a while: built memory that fursuit work no
    /// longer needs is let go (the user's choice: freed after use).
    fn idle_tick(&mut self) {
        if self.fursuits.release_if_idle(Instant::now()) && std::env::var_os("PP_PROFILE").is_some() {
            eprintln!("subjects: fursuit vectors released after {:?} idle", Fursuits::VECTORS_IDLE);
        }
        // The judge's helper leaves after two idle minutes on its own;
        // the status follows.  Close calls held for the card carry on.
        let loaded = self.runner.as_ref().is_some_and(Runner::loaded);
        if loaded != self.shared.status.lock().judge.loaded {
            self.publish_judge();
        }
        self.judge_resume();
    }

    // ---- the close-call judge (v2) ----

    /// Let the judge's helper go now and wait for it to leave, so its
    /// card memory is back before anything else loads.
    fn drop_runner(&mut self) {
        if let Some(r) = self.runner.take() {
            r.judge().unload();
        }
    }

    /// The judge as the pages see it.
    fn judge_status(&self) -> JudgeStatus {
        let downloaded = JudgeChoice::ALL.iter().copied().filter(|c| c.model().is_some_and(|m| (self.installed)(m))).collect();
        JudgeStatus { choice: self.judge_choice, downloaded, loaded: self.runner.as_ref().is_some_and(Runner::loaded), state: self.judge_state.clone() }
    }

    fn publish_judge(&mut self) {
        let j = self.judge_status();
        self.update(|s| s.judge = j);
    }

    /// The verdicts kept for the chosen judge in this library's folder.
    fn verdicts(&mut self) -> &mut PairCache {
        let dir = self.index.as_ref().map(|ix| ix.dir.clone());
        let valid = matches!(&self.verdicts, Some((c, d, _)) if *c == self.judge_choice && *d == dir);
        if !valid {
            let cache = match &dir {
                Some(d) => PairCache::open(d, self.judge_choice),
                None => PairCache::none(),
            };
            self.verdicts = Some((self.judge_choice, dir, cache));
        }
        &mut self.verdicts.as_mut().expect("just made").2
    }

    /// The verdicts go (Clear, Rebuild, the fursuits forgotten).
    fn forget_verdicts(&mut self) {
        self.forget_judging();
        if let Some(ix) = &self.index {
            PairCache::forget_in(&ix.dir);
        }
        self.verdicts = None;
    }

    /// The review being judged is dropped (its close calls stay
    /// unjudged; the helper stays loaded until it idles).
    fn forget_judging(&mut self) {
        self.judging = None;
        if let Some(r) = &self.runner {
            r.keep(0);
        }
    }

    /// A fursuit subject's suggestions with the judge: the sure matches
    /// and the close calls already judged at once (`Suggestions`, then a
    /// first `Judged` with those left out), the rest judged in the
    /// background, nearest first.
    fn judge_calls(&mut self, seq: u64, group: GroupId, calls: Vec<CloseCall>) {
        self.forget_judging();
        let (mut offered, mut left_out, mut pending) = (Vec::new(), Vec::new(), Vec::new());
        let (mut done, mut total) = (0, 0);
        for (i, c) in calls.iter().enumerate() {
            if c.sure {
                offered.push(c.faces.clone());
                continue;
            }
            total += 1;
            match self.verdicts().get(c.candidate, c.member) {
                Some(v) => {
                    done += 1;
                    if v.same(self.judge_choice) { offered.push(c.faces.clone()) } else { left_out.push(c.faces.clone()) }
                }
                None => pending.push(i),
            }
        }
        // The nearest close calls only; the rest wait for the next request.
        let later = pending.len().saturating_sub(judging::MAX_CLOSE_CALLS);
        pending.truncate(judging::MAX_CLOSE_CALLS);
        total -= later;
        let _ = self.bus.unbounded_send(BgEvent::Suggestions { seq, group: group.clone(), list: Arc::new(offered) });
        let _ = self.bus.unbounded_send(BgEvent::Judged { seq, group: group.clone(), offered: Arc::new(Vec::new()), left_out: Arc::new(left_out), done, total, beyond: later });
        self.judge_token += 1;
        let token = self.judge_token;
        let empty = pending.is_empty();
        self.judging = Some(Judging { seq, group, token, calls, pending, done, total, beyond: later });
        if empty {
            self.judge_state = JudgeState::Idle;
            self.publish_judge();
        } else {
            self.judge_go();
        }
    }

    /// Hand the pending close calls to the runner: never while a pass or
    /// a head scan holds the card (they wait, "waiting for the card"),
    /// and with the search helper's idle card memory let go first.
    fn judge_go(&mut self) {
        // A pass's end drops the key → shot table; the crops need it.
        self.ensure_resolver();
        let Some(j) = &self.judging else { return };
        if j.pending.is_empty() {
            return;
        }
        if self.pass.is_some() || self.scan.is_some() {
            self.judge_state = JudgeState::WaitingForCard;
            self.publish_judge();
            return;
        }
        let token = j.token;
        let items: Vec<(usize, crate::model::subjects::Face, FaceRef, FaceRef)> = j.pending.iter().map(|&i| (i, j.calls[i].faces[0].clone(), j.calls[i].candidate, j.calls[i].member)).collect();
        let mut tasks = Vec::new();
        let mut unshowable = Vec::new();
        for (i, face, cand, member) in items {
            match (self.judge_crop(&face), self.resolve_face(&member).and_then(|f| self.judge_crop(&f))) {
                (Some(a), Some(b)) => tasks.push(Task { token, key: PairKey::new(cand, member), ask: PairAsk { a, b } }),
                // A photo no longer in the library: nothing to show, so
                // nothing to judge; it counts as done, offered nowhere.
                _ => unshowable.push(i),
            }
        }
        let j = self.judging.as_mut().expect("judging");
        j.pending.retain(|i| !unshowable.contains(i));
        j.done += unshowable.len();
        let (group, done, total) = (j.group.clone(), j.done, j.total);
        if !unshowable.is_empty() {
            let _ = self.bus.unbounded_send(BgEvent::Judged { seq: j.seq, group: group.clone(), offered: Arc::new(Vec::new()), left_out: Arc::new(Vec::new()), done, total, beyond: j.beyond });
        }
        if tasks.is_empty() {
            self.judge_state = JudgeState::Idle;
            self.publish_judge();
            return;
        }
        // The search helper's sessions on the card go while the judge
        // works (they come back with the next query or pass).
        if self.device == Device::Gpu && !self.runner.as_ref().is_some_and(Runner::loaded) {
            self.drop_embedder();
        }
        if self.runner.is_none() {
            let me = self.me.clone();
            self.runner = Some(Runner::start(self.judge_choice, &self.judge_factory, move |e| drop(me.send(Cmd::JudgeEvent(e)))));
        }
        let runner = self.runner.as_ref().expect("a runner");
        runner.keep(token);
        runner.ask(tasks);
        self.judge_state = JudgeState::Judging { group, done, total };
        self.publish_judge();
    }

    /// The crop a judge is shown of a head: its photo (the JPG the head
    /// was found on, else the RAW) and its box.
    fn judge_crop(&self, f: &crate::model::subjects::Face) -> Option<crate::ml::judge::Crop> {
        let (path, t) = match (&f.jpg, &f.raw) {
            (Some(j), _) => (j, FileType::Jpg),
            (None, Some(r)) => (r, FileType::Raw),
            _ => return None,
        };
        Some(judging::crop(path, t, &f.face))
    }

    /// A pass or a head scan wants the card: the judge lets it go now;
    /// the close calls left wait.
    fn judge_yield(&mut self) {
        if let Some(r) = &self.runner {
            r.unload();
        }
        if self.judging.as_ref().is_some_and(|j| !j.pending.is_empty()) {
            self.judge_state = JudgeState::WaitingForCard;
        }
        self.publish_judge();
    }

    /// The card is free again: close calls held for it carry on.
    fn judge_resume(&mut self) {
        if matches!(self.judge_state, JudgeState::WaitingForCard) && self.pass.is_none() && self.scan.is_none() {
            self.judge_go();
        }
    }

    fn on_judge_event(&mut self, e: judging::Event) {
        match e {
            judging::Event::Loading => {
                if self.judging.as_ref().is_some_and(|j| !j.pending.is_empty()) {
                    self.judge_state = JudgeState::Loading;
                }
            }
            judging::Event::Judged { token, key, result } => {
                if let Ok(v) = &result {
                    let choice = self.judge_choice;
                    if let Err(err) = self.verdicts().add(&[(key, *v)], choice) {
                        eprintln!("judge: couldn't keep a verdict: {err}");
                    }
                }
                let Some(j) = self.judging.as_mut().filter(|j| j.token == token) else { return };
                let Some(pos) = j.pending.iter().position(|&i| PairKey::new(j.calls[i].candidate, j.calls[i].member) == key) else { return };
                let i = j.pending.remove(pos);
                j.done += 1;
                let (mut offered, mut left_out) = (Vec::new(), Vec::new());
                match &result {
                    Ok(v) if v.same(self.judge_choice) => offered.push(j.calls[i].faces.clone()),
                    Ok(_) => left_out.push(j.calls[i].faces.clone()),
                    Err(why) => eprintln!("judge: a close call couldn't be judged: {why}"),
                }
                let _ = self.bus.unbounded_send(BgEvent::Judged { seq: j.seq, group: j.group.clone(), offered: Arc::new(offered), left_out: Arc::new(left_out), done: j.done, total: j.total, beyond: j.beyond });
                self.judge_state = if j.pending.is_empty() { JudgeState::Idle } else { JudgeState::Judging { group: j.group.clone(), done: j.done, total: j.total } };
            }
            judging::Event::Idle => {
                if self.judging.as_ref().is_none_or(|j| j.pending.is_empty()) && !matches!(self.judge_state, JudgeState::WaitingForCard) {
                    self.judge_state = JudgeState::Idle;
                }
            }
            judging::Event::Failed(err) => {
                self.runner = None;
                self.judging = None;
                self.judge_state = match err {
                    JudgeError::Card(f) => JudgeState::Failed(f),
                    JudgeError::Broken(m) => JudgeState::Broken(m),
                };
            }
        }
        self.publish_judge();
    }

    /// The two head models are in place.
    fn heads_ready(&self) -> bool {
        self.missing(&models::FURSUIT_MODELS).is_empty()
    }

    /// Put the fursuit step's status in the snapshot and send it.
    fn publish_heads(&mut self) {
        self.fursuits.status.on = self.find_heads;
        self.fursuits.status.models_installed = self.heads_ready();
        self.fursuits.status.named = self.people.file.named_count(Kind::Fursuit);
        self.fursuits.refresh();
        let heads = self.fursuits.status.clone();
        self.update(|s| s.heads = heads);
    }

    /// Photos want a head scan (the first index or new exports ended, or
    /// the tab asked): decision 11's rule decides whether it starts now
    /// or waits for the user.  Never while a pass or a scan runs.
    fn offer_head_scan(&mut self, why: HeadWhy, asked: bool) {
        if !self.find_heads || self.pass.is_some() || self.scan.is_some() || !self.heads_ready() {
            return;
        }
        let jobs = {
            let (Some(lib), Some(ix)) = (&self.library, &self.index) else { return };
            self.fursuits.plan(lib, ix, &self.tags, asked)
        };
        let started = jobs.clone();
        if self.fursuits.consider(jobs, why, self.device, asked) == HeadDecision::Start {
            self.run_head_scan(started);
        }
        self.publish_heads();
    }

    /// Start the scan over `jobs` on its thread, with a helper that has
    /// the head models (started again if the running one hasn't).
    fn run_head_scan(&mut self, jobs: Vec<heads::HeadJob>) {
        if jobs.is_empty() || self.scan.is_some() {
            self.fursuits.stop();
            return;
        }
        let Some(root) = self.library.as_ref().map(|l| l.root.clone()) else { return };
        // Finding heads needs a helper with the detector; the catch-up
        // (known boxes) any image helper.
        let finding = jobs.iter().all(|j| j.boxes.is_empty());
        if finding && !self.embedder_has_heads {
            self.drop_embedder();
        }
        let Some(embedder) = self.image_embedder() else {
            self.fursuits.stop();
            return;
        };
        self.judge_yield();
        let (tx, rx) = crossbeam_channel::unbounded::<heads::HeadBatch>();
        let handle = heads::start(root, jobs, embedder, move |b| drop(tx.send(b)));
        self.scan = Some(HeadScan { handle, batches: rx });
    }

    /// A batch from the head scan: the cache grows, the new heads join
    /// the clusters; at the end a full regroup, and new exports that
    /// arrived meanwhile are looked at (the scan took the pass's place).
    fn on_head_batch(&mut self, batch: heads::HeadBatch) {
        if batch.restarted {
            self.update(|s| s.helper_restarts += 1);
        }
        let over = self.fursuits.absorb(&batch);
        self.fursuits.assign_new(&self.people.file);
        self.maybe_fit(over);
        if over {
            self.scan = None;
            if let Some(fin) = &batch.finished {
                if let Some(Stopped::Card(f)) = &fin.stopped {
                    self.drop_embedder();
                    self.card_failed(f.clone());
                }
            }
            self.publish_heads();
            self.people.set_other_clusters(Kind::Fursuit, self.fursuits.clusters());
            self.publish_subjects();
            self.start_heads_regroup();
            self.maybe_index_new();
            self.judge_resume();
        } else {
            self.publish_heads();
            self.publish_subjects_throttled(false);
        }
    }

    /// Fit the whitening again on a worker when it is due (v2:
    /// `Fursuits::fit_wanted`); `Cmd::Whitened` brings it back.  One at a
    /// time.
    fn maybe_fit(&mut self, at_end: bool) {
        if self.fitting || !self.fursuits.fit_wanted(at_end) {
            return;
        }
        let Some(dir) = self.fursuits.store.as_ref().map(|s| s.dir().to_path_buf()) else { return };
        let min = self.fursuits.min_fit;
        self.fitting = true;
        let me = self.me.clone();
        std::thread::Builder::new()
            .name("fursuits-whiten".into())
            .spawn(move || {
                let t = Instant::now();
                let w = HeadStore::fit_whitening(&dir, min);
                if let Some(w) = &w {
                    eprintln!("heads: whitening fitted on {} heads in {:.2} s", w.fitted_on, t.elapsed().as_secs_f64());
                }
                let _ = me.send(Cmd::Whitened(w.map(Box::new)));
            })
            .expect("spawn fursuits-whiten");
    }

    /// The v2 catch-up, asked for: the heads without vectors get them,
    /// on the chosen device, as a head scan (never beside a pass).
    fn catch_up_head_vectors(&mut self) {
        if self.pass.is_some() || self.scan.is_some() || !self.models_ready() {
            return;
        }
        let jobs = {
            let (Some(lib), Some(ix)) = (&self.library, &self.index) else { return };
            self.fursuits.vector_jobs(lib, ix)
        };
        if self.fursuits.start_catch_up(&jobs) {
            self.run_head_scan(jobs);
        }
        self.publish_heads();
    }

    fn cancel_scan(&mut self) {
        if let Some(sc) = self.scan.take() {
            sc.handle.cancel();
            while let Ok(b) = sc.batches.try_recv() {
                self.fursuits.absorb(&b);
            }
        }
        self.fursuits.stop();
        self.publish_heads();
        self.judge_resume();
    }

    /// Group the faces again on a worker thread (`Cmd::Regrouped` brings
    /// the result); one at a time, the next asked for meanwhile running
    /// after.
    fn start_regroup(&mut self) {
        if self.regroup_running {
            self.regroup_wanted = true;
            return;
        }
        let Some(job) = self.people.regroup_job() else { return };
        self.regroup_running = true;
        let me = self.me.clone();
        std::thread::Builder::new()
            .name("subjects-regroup".into())
            .spawn(move || {
                let _t = crate::profile::span("subjects", "regroup");
                let r = super::step::regroup(&job);
                let _ = me.send(Cmd::Regrouped(Box::new(r)));
            })
            .expect("spawn subjects-regroup");
    }

    /// While a pass runs, the snapshot is rebuilt at most every
    /// [`THINGS_EVERY`]; `now` forces it (the pass ended).
    fn publish_subjects_throttled(&mut self, now: bool) {
        if !now && self.subjects_sent.is_some_and(|t| t.elapsed() < THINGS_EVERY) {
            return;
        }
        self.publish_subjects();
    }

    /// Rebuild the *Subjects* snapshot and send it.
    fn publish_subjects(&mut self) {
        self.subjects_sent = Some(Instant::now());
        self.ensure_resolver();
        let mut snapshot = (*self.people.build_snapshot(&|f| self.resolve_face(f))).clone();
        self.fursuits.extend_snapshot(&mut snapshot, &|f| self.resolve_face(f));
        let snapshot = Arc::new(snapshot);
        self.people.set_snapshot(snapshot.clone());
        let named = self.people.file.named_count(Kind::Person);
        let suits = self.people.file.named_count(Kind::Fursuit);
        *self.shared.subjects.lock() = snapshot.clone();
        self.fursuits.status.named = suits;
        self.update(|s| {
            s.faces.named = named;
            s.heads.named = suits;
        });
        let _ = self.bus.unbounded_send(BgEvent::Subjects(snapshot));
    }

    /// A detection as a [`Face`] with its shot, from the resolver; none
    /// when the photo isn't in the library any more.
    fn resolve_face(&self, f: &FaceRef) -> Option<Face> {
        let r = self.resolver.as_ref()?;
        let shot = r.shots.get(&f.key)?;
        let rec = self.index.as_ref().and_then(|ix| ix.vectors.by_key.get(&f.key).map(|&i| &ix.vectors.records[i]));
        let capture = rec.and_then(|rec| rec.capture.map(|c| c + chrono::Duration::milliseconds(rec.meta.subsec_ms.unwrap_or(0) as i64))).unwrap_or(shot.mtime);
        let is_head = self.fursuits.store.as_ref().is_some_and(|s| s.detection(f).is_some());
        let (confirmed, clarity) = if is_head {
            (self.people.file.member_of(Kind::Fursuit, f).is_some(), self.fursuits.clarity_with_sureness(f))
        } else {
            (self.people.file.member_of(Kind::Person, f).is_some(), self.people.clarity(f))
        };
        Some(Face { face: *f, jpg: shot.jpg.clone(), raw: shot.raw.clone(), day: shot.day, capture, burst: shot.burst, clarity, confirmed })
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
            let dir = self.index_dir.clone().or_else(|| super::index::dir_for(&lib.root));
            self.index = dir.as_ref().map(|dir| Index::load(&lib.root, dir, models::info(ModelId::Siglip2)));
            // The face cache lives beside the index.
            let store = dir.as_ref().map(|dir| FaceStore::load(dir, self.face_model));
            *self.shared.tiles.lock() = store.as_ref().map(FaceStore::tiles);
            self.people.store = store;
            // The head cache too (its vectors stay on disk until asked for).
            self.cancel_scan();
            match dir.as_ref() {
                Some(dir) => self.fursuits.open(dir),
                None => self.fursuits.close(),
            }
            *self.shared.head_tiles.lock() = self.fursuits.store.as_ref().map(HeadStore::tiles);
            self.resolver = None;
            // Vectors without a whitening (an interrupted catch-up): fit.
            self.maybe_fit(false);
        }
        let live: HashSet<PathBuf> = lib.days.values().flat_map(|d| d.files.iter().map(|f| pass::rel_path(&lib.root, &f.path))).collect();
        if let Some(ix) = &mut self.index {
            ix.retain_paths(&live);
            let keys: HashSet<Key> = ix.paths.entries.values().map(|e| e.key).collect();
            if let Some(store) = &mut self.people.store {
                store.retain(&keys);
            }
            if let Some(store) = &mut self.fursuits.store {
                store.retain(&keys);
            }
        }
        self.library = Some(lib);
        self.resolver = None;
        self.refresh_counts();
        if root_changed {
            self.people.reset_grouping();
            self.fursuits.reset_grouping();
            self.start_regroup();
            // The fursuits' groups too, whenever heads have the search
            // model's vectors: whitened from `whiten::MIN_HEADS` heads, raw
            // below (v2's small-library rule).  Their rows are built for it
            // and let go again when idle.
            if self.fursuits.store.as_ref().is_some_and(|s| s.vector_count() > 0) {
                self.start_heads_regroup();
            }
        }
        self.publish_subjects();
        if root_changed {
            self.vocab_tried = false;
        }
        self.refresh_tags();
        self.fursuits.count_tagged(&self.tags);
        self.publish_heads();
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
        let (looked_at, found, unrecognised) = self.people.store.as_ref().map_or((0, 0, 0), |s| (s.looked_at(), s.found(), s.unrecognised()));
        let (scanned, heads_found) = self.fursuits.store.as_ref().map_or((0, 0), |s| (s.looked_at(), s.found()));
        self.update(|s| {
            s.library_shots = shots;
            s.indexed_shots = indexed;
            s.first_index_asked = asked;
            s.faces.looked_at = looked_at;
            s.faces.found = found;
            s.faces.unrecognised = unrecognised;
            s.heads.scanned = scanned;
            s.heads.found = heads_found;
        });
    }

    /// The jobs of a face pass over photos already indexed: for the
    /// catch-up, every indexed shot not looked at yet; for a model
    /// change, every shot whose faces lack vectors, carrying them.
    fn face_jobs(&self, change: bool) -> Vec<pass::Job> {
        let (Some(lib), Some(ix), Some(store)) = (&self.library, &self.index, &self.people.store) else { return Vec::new() };
        let needing: HashMap<Key, Vec<crate::ml::client::FaceSpot>> = if change { store.needing_vectors().into_iter().collect() } else { HashMap::new() };
        let mut jobs = Vec::new();
        for day in lib.days.values().rev() {
            for shot in day.shots() {
                let rep = shot.iter().find(|&&i| day.files[i].file_type == FileType::Jpg).or(shot.first());
                let Some(&i) = rep else { continue };
                let fe = &day.files[i];
                let rel = pass::rel_path(&lib.root, &fe.path);
                let Some(key) = ix.paths.key_for(&rel, fe.size, &fe.mtime) else { continue };
                if !ix.vectors.contains(&key) {
                    continue;
                }
                let known = if change {
                    match needing.get(&key) {
                        Some(spots) => Some(spots.clone()),
                        None => continue,
                    }
                } else {
                    if store.scanned.contains_key(&key) {
                        continue;
                    }
                    None
                };
                jobs.push(pass::Job { path: fe.path.clone(), file_type: fe.file_type, rel, size: fe.size, mtime: fe.mtime, day: day.date, known });
            }
        }
        jobs
    }

    /// A face pass (`PassKind::Faces`) over photos already indexed: the
    /// catch-up (`change` false) or the vectors again after a model
    /// change.  Only ever started by a command, never by a pass's end, so
    /// it can't start itself over.
    fn start_face_pass(&mut self, change: bool) {
        if self.pass.is_some() || self.scan.is_some() || !self.faces_ready() {
            return;
        }
        let Some(root) = self.library.as_ref().map(|l| l.root.clone()) else { return };
        let jobs = self.face_jobs(change);
        if jobs.is_empty() {
            self.refresh_counts();
            return;
        }
        let Some(embedder) = self.image_embedder() else { return };
        let (tx, rx) = crossbeam_channel::unbounded::<pass::Batch>();
        let total = jobs.len();
        let req = pass::Request { root, jobs, total, want: Want::faces_only() };
        self.judge_yield();
        let handle = pass::start(req, embedder, 0, move |b| drop(tx.send(b)));
        self.pass = Some(Running { handle, kind: PassKind::Faces, batches: rx });
        self.update(|s| {
            s.pass = Some(PassProgress { kind: PassKind::Faces, done: 0, total, oldest_day: None });
            s.stopped = None;
        });
    }

    /// A face pass wants its models first: start it when they arrive.
    fn face_pass_or_download(&mut self, change: bool) {
        if self.faces_ready() {
            self.start_face_pass(change);
        } else {
            self.face_pass_pending = Some(change);
            self.download_missing(&models::models_for(Some(self.face_model)));
        }
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
        // The dialog's *Fursuits* pair ticked means heads on (decision 11;
        // on the processor the scan still asks when it would start).
        self.find_heads = models::fursuits_in(&wanted);
        self.fursuits.status.on = self.find_heads;
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
        let (bus, me, downloader) = (self.bus.clone(), self.me.clone(), self.downloader.clone());
        std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || {
                let ok = downloader(&missing, &cancel, &mut |ev| drop(bus.unbounded_send(ev)));
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
        let with_heads = self.find_heads && self.heads_ready();
        match (self.factory)(Spec::images(self.device, self.find_faces.then_some(self.face_model)).with_heads(with_heads)) {
            Ok(e) => {
                self.embedder_has_heads = with_heads;
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
        self.fursuits.card_failed(f.clone());
        self.publish_heads();
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
        if self.pass.is_some() || self.scan.is_some() {
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
        let want = if self.find_faces && self.faces_ready() { Want::with_faces() } else { Want::search() };
        let req = pass::Request { root, jobs: plan.jobs, total, want };
        self.judge_yield();
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
        if batch.faces_scanned {
            self.refresh_counts();
            self.publish_subjects_throttled(batch.finished.is_some());
            if batch.finished.is_some() {
                self.start_regroup();
            }
        }
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
                self.fursuits.count_tagged(&self.tags);
                self.maybe_index_new();
                // The head step comes after search (and faces): once the
                // first index or a new-exports pass ran to its end, the
                // fursuit photos it indexed are considered (decision 11:
                // the card starts, the processor asks).  A pass that
                // started just above has the slot, and the step waits for
                // that pass's end in turn.
                match kind {
                    PassKind::FirstIndex => self.offer_head_scan(HeadWhy::FirstIndex, false),
                    PassKind::NewExports => self.offer_head_scan(HeadWhy::NewExports, false),
                    PassKind::Rebuild | PassKind::Faces => {}
                }
            }
            self.judge_resume();
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
        // The faces found (or recognised again) go into the face cache,
        // and join the clusters as they come (the full regroup runs when
        // the pass ends).
        if batch.faces_scanned {
            if let Some(store) = &mut self.people.store {
                for (job, e) in &batch.embedded {
                    let r = if job.known.is_some() { store.add_vectors(e.key, &e.faces) } else { store.add(e.key, &e.faces) };
                    if let Err(err) = r {
                        eprintln!("faces: couldn't write the face cache: {err}");
                    }
                }
            }
            self.people.assign_new();
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
        match (self.factory)(Spec::text_only()) {
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
        // A typed name is one kind's: the person when a person has it,
        // else the fursuit; a fursuit of the same name as a person is
        // reported beside the results, never searched with them (a picked
        // chip carries its own GroupId and so its kind).
        let mut also_fursuits: Vec<(String, GroupId)> = Vec::new();
        for name in &parsed.names {
            let people = self.people.named(Kind::Person, name);
            let suits = self.people.named(Kind::Fursuit, name);
            let named = if people.is_empty() {
                suits
            } else {
                also_fursuits.extend(suits.into_iter().map(|g| (name.clone(), g)));
                people
            };
            let mut keys = HashSet::new();
            for g in &named {
                keys.extend(self.people.photos_of(g).unwrap_or_default());
            }
            groups.extend(named);
            narrow(keys);
        }
        groups.dedup();
        let groups = Arc::new(groups);
        let also_fursuits = Arc::new(also_fursuits);
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
                also_fursuits: also_fursuits.clone(),
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
        let config = Config { factory: Box::new(|_| Err(StartError::Other("no".into()))), installed: Box::new(|_| false), index_dir: None, subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Cpu, false, true, ModelId::AuraFace, false, config);
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
            self.results_with_also(seq).0
        }

        /// The answer, with the fursuits of a typed name that a person
        /// has too ("also a fursuit called …").
        fn results_with_also(&mut self, seq: u64) -> (Answer, Vec<(String, GroupId)>) {
            let stream = &mut self.stream;
            self.rt.block_on(async {
                use iced::futures::StreamExt;
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    let Some(batch) = stream.next().await else { break };
                    for ev in batch {
                        if let BgEvent::SearchResults { seq: s, hits, indexed, error, understood, fursuits, animal, subjects: _, also_fursuits } = ev {
                            if s == seq {
                                return (((*hits).clone(), indexed, error, (*understood).clone(), (*fursuits).clone(), animal), (*also_fursuits).clone());
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
        let factory: EmbedderFactory = Box::new(move |spec: Spec| {
            let (device, text_only) = (spec.device, spec.text_only);
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
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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
        let e2 = Engine::with_config(bus2, Device::Gpu, true, true, ModelId::AuraFace, false, Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() });
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
        Box::new(move |_| Ok(fake.clone() as Arc<dyn Embedder>))
    }

    /// Scan a scratch library again (after its files changed).
    fn rescan(root: &Path) -> Arc<LibraryIndex> {
        let cancel = AtomicBool::new(false);
        let scanned = crate::model::library_cache::scan(root, None, true, &cancel, &|_| {}).unwrap().unwrap();
        let store = Mutex::new(crate::model::library_cache::Store::new(root.to_path_buf()));
        Arc::new(crate::model::library_index::index_from_tree(root, &scanned.tree, &store, &cancel).unwrap())
    }

    /// Models in place, except the face ones: `have_faces` says whether
    /// YuNet and AuraFace are installed; the downloader double flips it
    /// when asked to fetch them (or, with `succeed` false, fetches
    /// nothing, as a failed or cancelled download).
    fn faces_arrive_later(succeed: bool) -> (Box<dyn Fn(ModelId) -> bool + Send>, Downloader, Arc<AtomicBool>, Arc<Mutex<Vec<ModelId>>>) {
        let have_faces = Arc::new(AtomicBool::new(false));
        let asked: Arc<Mutex<Vec<ModelId>>> = Arc::new(Mutex::new(Vec::new()));
        let (h, a) = (have_faces.clone(), asked.clone());
        let installed: Box<dyn Fn(ModelId) -> bool + Send> = Box::new(move |id| id == ModelId::Siglip2 || h.load(std::sync::atomic::Ordering::Relaxed));
        let (h, a2) = (have_faces.clone(), asked.clone());
        let downloader: Downloader = Arc::new(move |ids, _, _| {
            a2.lock().extend_from_slice(ids);
            if succeed {
                h.store(true, std::sync::atomic::Ordering::Relaxed);
                ids.to_vec()
            } else {
                Vec::new()
            }
        });
        drop(a);
        (installed, downloader, have_faces, asked)
    }

    /// The user's upgrade path: search's model is in place already and
    /// only the face models are fetched.  The first index must start when
    /// they arrive (search was ready all along), with faces on; and when
    /// their download fails, search still indexes, without faces.
    #[test]
    fn the_first_index_starts_when_only_the_face_models_were_missing() {
        for succeed in [true, false] {
            let (bus, _rx) = crate::bus::channel();
            let (lib, d) = library(if succeed { "faces-arrive" } else { "faces-fail" }, 5);
            let (factory, _) = logging_factory(false);
            let (installed, downloader, _have, asked) = faces_arrive_later(succeed);
            let config = Config { factory, installed, index_dir: Some(d.join("index")), subjects_path: None, downloader };
            let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
            e.on_library(lib.clone());
            wait_for(&e, "the library", |s| s.library_shots == 5);
            assert!(e.status().models_installed && !e.status().faces.installed);
            e.start_first_index(&models::models_for(Some(ModelId::AuraFace)));
            let s = wait_for(&e, "the first index after the download", |s| s.indexed_shots == 5 && s.pass.is_none());
            assert_eq!(asked.lock().clone(), [ModelId::Yunet, ModelId::AuraFace], "only the missing models were fetched");
            assert_eq!((s.faces.installed, s.faces.looked_at), (succeed, if succeed { 5 } else { 0 }), "succeed={succeed}: {s:?}");
            assert!(s.stopped.is_none() && s.faces.on);
            e.shutdown();
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// The catch-up asked for with the face model missing: fetched, then
    /// the catch-up runs over the indexed photos.
    #[test]
    fn the_catch_up_starts_when_only_the_face_model_was_missing() {
        let (bus, _rx) = crate::bus::channel();
        let (lib, d) = library("catch-up-arrives", 4);
        let (factory, _) = logging_factory(false);
        let (installed, downloader, _have, asked) = faces_arrive_later(true);
        let config = Config { factory, installed, index_dir: Some(d.join("index")), subjects_path: None, downloader };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 4);
        // Search alone first (faces unticked in the dialog).
        e.start_first_index(&models::models_for(None));
        let s = wait_for(&e, "the first index", |s| s.indexed_shots == 4 && s.pass.is_none());
        assert_eq!((s.faces.on, s.faces.looked_at), (false, 0));
        assert!(asked.lock().is_empty(), "nothing to fetch for search alone");
        e.set_find_faces(true);
        e.find_faces();
        let s = wait_for(&e, "the catch-up after the download", |s| s.faces.looked_at == 4 && s.pass.is_none());
        assert_eq!(asked.lock().clone(), [ModelId::Yunet, ModelId::AuraFace]);
        assert!(s.faces.installed && s.stopped.is_none() && s.indexed_shots == 4);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The face catch-up covers every indexed photo not yet looked at for
    /// faces (the user, 2026-10-03): photos indexed while face finding was
    /// off, new exports included, and it runs only when asked.
    #[test]
    fn the_face_catch_up_covers_every_photo_indexed_without_faces() {
        let (bus, _rx) = crate::bus::channel();
        let (lib, d) = library("face-catch-up", 6);
        let (factory, _) = logging_factory(false);
        // Faces on and the models in place: the first index looks at
        // every photo.
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 6);
        e.start_first_index(&models::models_for(Some(ModelId::AuraFace)));
        let s = wait_for(&e, "the first index", |s| s.indexed_shots == 6 && s.pass.is_none());
        assert_eq!((s.faces.on, s.faces.looked_at), (true, 6));
        // Faces off; two exports arrive and are indexed without faces.
        e.set_find_faces(false);
        wait_for(&e, "faces off", |s| !s.faces.on);
        let day = d.join("lib/2025/Mar-2025/12-Mar-2025/JPG");
        std::fs::write(day.join("0006.JPG"), "new").unwrap();
        std::fs::write(day.join("0007.JPG"), "newer").unwrap();
        e.on_library(rescan(&d.join("lib")));
        let s = wait_for(&e, "the new exports", |s| s.indexed_shots == 8 && s.pass.is_none());
        assert_eq!(s.faces.looked_at, 6, "nothing is looked at for faces while the setting is off");
        // Nothing happens on its own; the catch-up, when asked, takes the two.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(e.status().faces.looked_at, 6);
        e.set_find_faces(true);
        e.find_faces();
        let s = wait_for(&e, "the catch-up", |s| s.faces.looked_at == 8 && s.pass.is_none());
        assert_eq!((s.indexed_shots, s.stopped), (8, None));
        // Asked again with nothing left: no pass starts.
        e.find_faces();
        std::thread::sleep(Duration::from_millis(100));
        let s = e.status();
        assert!(s.pass.is_none() && s.faces.looked_at == 8);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
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
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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
        let config = Config { factory: one_fake(fake.clone()), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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

    /// Fursuit heads at chosen distances: a file `NNNN-fursuit-90-<a>`
    /// has one head whose 2-wide vector lies at angle `<a>` (degrees
    /// tenths, `m` for minus), so its cosine distance to the head at 0 is
    /// `1 − cos a`.  Search vectors as [`Axes`] (so the files are tagged
    /// fursuit).
    struct Suits;

    impl Suits {
        fn angle(stem: &str) -> f32 {
            let last = stem.rsplit('-').next().unwrap_or("0");
            let (sign, digits) = match last.strip_prefix('m') {
                Some(d) => (-1.0, d),
                None => (1.0, last.trim_start_matches('p')),
            };
            sign * digits.parse::<f32>().unwrap_or(0.0) / 10.0
        }

        /// The tenths-of-a-degree angle at cosine distance `d` from 0.
        fn at(d: f32) -> i32 {
            ((1.0 - d).acos().to_degrees() * 10.0).round() as i32
        }
    }

    impl Embedder for Suits {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            Axes.device_report()
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
            Axes.embed_images(files, want)
        }
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Axes.encode_text(texts)
        }
        fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<crate::ml::heads::HeadsResult>, HelperError> {
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let stem = crate::util::stem(p);
                    let a = Suits::angle(&stem).to_radians();
                    let key = Key::of(1, p.as_os_str().as_encoded_bytes());
                    // `-dim-` in the name: a small, soft, doubtful head (clarity 0).
                    let (size, sharpness, score) = if stem.contains("-dim-") { (100, 0.0, 0.3) } else { (300, 0.2, 0.5) };
                    let heads = vec![crate::ml::heads::Head { bbox: [0.3, 0.2, 0.3, 0.4], score, size, sharpness, vector: vec![a.cos(), a.sin()], tile: Vec::new() }];
                    crate::ml::heads::HeadsResult { path: p.clone(), result: Ok(crate::ml::heads::PhotoHeads { key, heads }) }
                })
                .collect())
        }
    }

    /// What the review was sent for request `seq`: the `Suggestions`
    /// list, then each `Judged` (offered, left out, done, total), until
    /// one has `done == total` (or none comes within `wait`).
    #[allow(clippy::type_complexity)]
    fn review(ev: &mut Events, seq: u64, wait: Duration) -> (Option<Vec<Vec<Face>>>, Vec<(Vec<Vec<Face>>, Vec<Vec<Face>>, usize, usize)>) {
        let stream = &mut ev.stream;
        ev.rt.block_on(async {
            use iced::futures::StreamExt;
            let deadline = Instant::now() + wait;
            let (mut list, mut judged) = (None, Vec::new());
            while Instant::now() < deadline {
                let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await else { continue };
                for e in batch {
                    match e {
                        BgEvent::Suggestions { seq: s, list: l, .. } if s == seq => list = Some((*l).clone()),
                        BgEvent::Judged { seq: s, offered, left_out, done, total, .. } if s == seq => {
                            let last = done == total;
                            judged.push(((*offered).clone(), (*left_out).clone(), done, total));
                            if last {
                                return (list, judged);
                            }
                        }
                        _ => {}
                    }
                }
            }
            (list, judged)
        })
    }

    /// The close-call judge through the engine (fursuit matching v2),
    /// with the stand-in: a fursuit subject's sure matches come at once,
    /// its close calls are judged nearest first against its nearest
    /// member and offered or left out as the judge says; a candidate
    /// beyond the band is never asked about; verdicts are kept, so asking
    /// again costs nothing; nothing is named, merged or marked by the
    /// judge; with the processor chosen the vector cut-offs answer and no
    /// judge runs; a failing card stops the judge and says so, and the
    /// next review falls back to the vector cut-offs.
    #[test]
    fn the_judge_filters_a_fursuit_subjects_close_calls_and_never_names() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}{}", if n == 7 { "dim-" } else { "" }, if a < 0 { "m" } else { "p" }, a.abs());
        // The member at 0; one that groups with it (0.3); a sure match
        // (0.6) and a doubtful one under sure-yes too (0.55, a dim head:
        // the judge sees it); two close calls (0.70, 0.75); one beyond the
        // band (0.9).
        let specs = [(1, 0), (2, Suits::at(0.3)), (3, -Suits::at(0.6)), (4, -Suits::at(0.70)), (5, -Suits::at(0.75)), (6, -Suits::at(0.9)), (7, -Suits::at(0.55))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("judge", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        let fake = Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::Alternate));
        let f2 = fake.clone();
        e.set_judge_factory(Box::new(move |_| f2.clone() as Arc<dyn crate::ml::judge_client::Judge>));
        e.on_library(lib);
        wait_for(&e, "the library", |s| s.library_shots == 7);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 7 && s.heads.step == heads::Step::Idle);
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        // The member's group (with the 0.3 head) named as a fursuit.
        let key_of = |n: u32| Key::of(1, lib_path(&d, &files[n as usize - 1]).as_os_str().as_encoded_bytes());
        let started = Instant::now();
        let cluster = loop {
            let g = e.subjects().groups.iter().find(|g| g.kind == Kind::Fursuit && g.cover.face.key == key_of(1) || g.kind == Kind::Fursuit && g.cover.face.key == key_of(2)).map(|g| (g.id.clone(), g.photos));
            if let Some((id, 2)) = g {
                break id;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no group of the member: {:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        };
        e.name_group(cluster, "Suit".into());
        let started = Instant::now();
        let suit = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit")) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        let keys = |l: &[Vec<Face>]| -> Vec<Key> { l.iter().map(|b| b[0].face.key).collect() };
        // Off: the vector cut-offs (0.711), as before: the dim head, the
        // sure match and the 0.70 one, nearest first.
        e.suggestions(1, suit.clone());
        let (list, judged) = review(&mut ev, 1, Duration::from_millis(800));
        assert_eq!(keys(&list.expect("suggestions")), [key_of(7), key_of(3), key_of(4)]);
        assert!(judged.is_empty(), "no judge: no verdicts");
        // The judge on: the clear sure match at once; the dim head under
        // sure-yes and the two close calls judged, nearest first.
        e.set_judge(JudgeChoice::Gemma4);
        wait_for(&e, "the choice", |s| s.judge.choice == JudgeChoice::Gemma4 && s.judge.active(Device::Gpu));
        e.suggestions(2, suit.clone());
        let (list, judged) = review(&mut ev, 2, Duration::from_secs(5));
        assert_eq!(keys(&list.expect("suggestions")), [key_of(3)], "only the clear sure match before the judge");
        assert_eq!((judged[0].2, judged[0].3), (0, 3), "the first Judged: nothing judged yet; the dim head counts as a close call");
        let offered: Vec<Key> = judged.iter().flat_map(|j| keys(&j.0)).collect();
        let left: Vec<Key> = judged.iter().flat_map(|j| keys(&j.1)).collect();
        assert_eq!((offered, left), (vec![key_of(7), key_of(5)], vec![key_of(4)]), "nearest first: yes, no, yes");
        {
            let asked = fake.pairs.lock();
            assert_eq!(asked.len(), 3, "the head beyond the band is never asked about");
            for p in asked.iter() {
                assert!(matches!(&p.b, crate::ml::judge::Crop::Head { key, .. } if *key == key_of(1)), "against the nearest member");
            }
        }
        // Asked again: the verdicts kept answer at once.
        e.suggestions(3, suit.clone());
        let (list, judged) = review(&mut ev, 3, Duration::from_secs(2));
        assert_eq!(keys(&list.expect("suggestions")), [key_of(7), key_of(3), key_of(5)]);
        assert_eq!(judged.len(), 1);
        assert_eq!((keys(&judged[0].1), judged[0].2, judged[0].3), (vec![key_of(4)], 3, 3));
        assert_eq!(fake.pairs.lock().len(), 3, "nothing judged twice");
        // Nothing named or merged by the judge: the suit is as named.
        let g = e.subjects().groups.iter().find(|g| g.id == suit).cloned().unwrap();
        assert_eq!(g.photos, 2);
        // Another judge chosen: the loaded one leaves first (one on the
        // card at a time); its verdicts stay its own.
        assert!(crate::ml::judge_client::Judge::loaded(fake.as_ref()));
        e.set_judge(JudgeChoice::Qwen3VlReranker);
        wait_for(&e, "Qwen chosen", |s| s.judge.choice == JudgeChoice::Qwen3VlReranker);
        assert!(!crate::ml::judge_client::Judge::loaded(fake.as_ref()), "Gemma let go before another loads");
        e.suggestions(30, suit.clone());
        let (_, judged) = review(&mut ev, 30, Duration::from_secs(5));
        assert_eq!((judged[0].2, judged[0].3), (0, 3), "Qwen keeps no verdicts of Gemma's");
        e.set_judge(JudgeChoice::Gemma4);
        wait_for(&e, "Gemma again", |s| s.judge.choice == JudgeChoice::Gemma4);
        // The processor: no judge, the vector cut-offs.
        e.set_device(Device::Cpu);
        wait_for(&e, "the processor", |s| s.device.chosen == Device::Cpu);
        e.suggestions(4, suit.clone());
        let (list, judged) = review(&mut ev, 4, Duration::from_millis(800));
        assert_eq!(keys(&list.expect("suggestions")), [key_of(7), key_of(3), key_of(4)]);
        assert!(judged.is_empty(), "card only: no judging on the processor");
        // A failing card: reported, judging stops; the next review uses
        // the vector cut-offs.
        e.set_device(Device::Gpu);
        e.set_judge_factory(Box::new(|_| Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::CardFail)) as Arc<dyn crate::ml::judge_client::Judge>));
        e.forget_kind(Kind::Pet); // a no-op command, to order the queue
        let _ = std::fs::remove_file(d.join("index").join("judged-gemma4-26b-a4b-it-ud-q4km.bin"));
        e.set_judge(JudgeChoice::Off);
        e.set_judge(JudgeChoice::Gemma4);
        e.suggestions(5, suit.clone());
        wait_for(&e, "the card failure", |s| matches!(s.judge.state, crate::ml::judge::JudgeState::Failed(_)));
        assert!(!e.status().judge.active(Device::Gpu));
        e.suggestions(6, suit.clone());
        let (list, judged) = review(&mut ev, 6, Duration::from_millis(800));
        assert_eq!(keys(&list.expect("suggestions")), [key_of(7), key_of(3), key_of(4)], "the vector cut-offs");
        assert!(judged.is_empty());
        e.try_card_again();
        wait_for(&e, "tried again", |s| matches!(s.judge.state, crate::ml::judge::JudgeState::Idle));
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// [`Suits`], with search embedding held back while `slow` is set (a
    /// pass that keeps the card busy).
    struct SlowSuits(Arc<std::sync::atomic::AtomicBool>);

    impl Embedder for SlowSuits {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            Suits.device_report()
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
            while self.0.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            Suits.embed_images(files, want)
        }
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Suits.encode_text(texts)
        }
        fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<crate::ml::heads::HeadsResult>, HelperError> {
            Suits.find_heads(files)
        }
    }

    /// The judge never takes the card while a pass holds it: close calls
    /// asked for during a pass wait ("waiting for the card"), nothing is
    /// judged, and they are judged once the pass ends.
    #[test]
    fn close_calls_wait_for_the_card_while_a_pass_runs() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.3)), (3, -Suits::at(0.70)), (4, -Suits::at(0.75))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("judge-wait", &refs);
        let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = slow.clone();
        let factory: EmbedderFactory = Box::new(move |_| Ok(Arc::new(SlowSuits(s2.clone())) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        let fake = Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::Yes));
        let f2 = fake.clone();
        e.set_judge_factory(Box::new(move |_| f2.clone() as Arc<dyn crate::ml::judge_client::Judge>));
        e.set_judge(JudgeChoice::Gemma4);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key1 = Key::of(1, lib_path(&d, &files[0]).as_os_str().as_encoded_bytes());
        let started = Instant::now();
        let cluster = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.kind == Kind::Fursuit && g.photos == 2 && g.cover.face.key != Key::of(1, lib_path(&d, &files[2]).as_os_str().as_encoded_bytes())) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = key1;
        e.name_group(cluster, "Suit".into());
        let started = Instant::now();
        let suit = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit")) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        // A pass that holds the card: the face catch-up, held back.
        slow.store(true, std::sync::atomic::Ordering::Relaxed);
        e.find_faces();
        wait_for(&e, "the pass", |s| s.pass.is_some());
        e.suggestions(9, suit.clone());
        wait_for(&e, "waiting for the card", |s| s.judge.state == crate::ml::judge::JudgeState::WaitingForCard);
        std::thread::sleep(Duration::from_millis(300));
        assert!(fake.pairs.lock().is_empty() && !crate::ml::judge_client::Judge::loaded(fake.as_ref()), "nothing judged, nothing loaded while the pass holds the card");
        slow.store(false, std::sync::atomic::Ordering::Relaxed);
        let (_, judged) = review(&mut ev, 9, Duration::from_secs(10));
        let last = judged.last().expect("verdicts after the pass");
        assert_eq!((last.2, last.3), (2, 2), "both close calls judged once the card was free");
        assert_eq!(fake.pairs.lock().len(), 2);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A library with too few heads to whiten (`whiten::MIN_HEADS`; the
    /// step groups the raw vectors) has its fursuit groups again when it
    /// opens, not only after the next scan or edit.
    #[test]
    fn fursuit_groups_come_back_when_a_raw_library_opens() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.1)), (3, -Suits::at(0.9)), (4, -Suits::at(0.95))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("raw-reopen", &refs);
        let config = || Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config());
        e.on_library(lib.clone());
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.shutdown();
        std::thread::sleep(Duration::from_millis(200));
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config());
        e.on_library(lib);
        let started = Instant::now();
        while e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).count() < 2 {
            assert!(started.elapsed() < Duration::from_secs(5), "no fursuit groups at open: {:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!e.status().heads.whitened, "raw: too few heads to whiten");
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A library whose heads were whitened before (an interrupted
    /// catch-up, a restart) has its fursuit groups again when it opens,
    /// with no pass or command.
    #[test]
    fn fursuit_groups_come_back_when_a_whitened_library_opens() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.1)), (3, -Suits::at(0.9)), (4, -Suits::at(0.95))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("judge-reopen", &refs);
        let config = || Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config());
        e.on_library(lib.clone());
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        e.shutdown();
        std::thread::sleep(Duration::from_millis(200));
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config());
        e.on_library(lib);
        let started = Instant::now();
        while e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).count() < 2 {
            assert!(started.elapsed() < Duration::from_secs(5), "no fursuit groups at open: {:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(e.status().heads.whitened);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    fn lib_path(d: &Path, (day, file): &(String, String)) -> PathBuf {
        d.join("lib").join(day).join("JPG").join(file)
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
        fn embed_images(&self, files: &[(PathBuf, FileType)], _want: &Want) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
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
                        faces: Vec::new(),
                    };
                    crate::ml::client::ImageResult { path: p.clone(), result: Ok(embedding) }
                })
                .collect())
        }
        /// One head per photo whose name says fursuit, with the photo's
        /// own axis vector (so photos named alike are one suit).
        fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<crate::ml::heads::HeadsResult>, HelperError> {
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let stem = crate::util::stem(p);
                    let key = Key::of(1, p.as_os_str().as_encoded_bytes());
                    let heads = if stem.contains("fursuit") {
                        vec![crate::ml::heads::Head { bbox: [0.3, 0.2, 0.3, 0.4], score: 0.5, size: 300, sharpness: 0.2, vector: Axes::unit(&Axes::parts_of(&stem)), tile: Vec::new() }]
                    } else {
                        Vec::new()
                    };
                    crate::ml::heads::HeadsResult { path: p.clone(), result: Ok(crate::ml::heads::PhotoHeads { key, heads }) }
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

    /// Storage counts every model folder, phase 4's DINOv2 too until
    /// *Delete* removes it, and the judges' apart; the head cache counts
    /// its old vector file; Delete keeps the judge's folder (its own row).
    #[test]
    fn storage_counts_an_old_dinov2_layout_and_delete_removes_it() {
        let d = std::env::temp_dir().join(format!("pp-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (models_dir, index_dir) = (d.join("models"), d.join("index"));
        for (folder, bytes) in [("dinov2-base", 1000usize), ("siglip2-b16-256", 300), ("gemma4-26b-a4b-it-ud-q4km", 5000)] {
            std::fs::create_dir_all(models_dir.join(folder)).unwrap();
            std::fs::write(models_dir.join(folder).join("f"), vec![0u8; bytes]).unwrap();
        }
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("headvecs-dinov2-base.bin"), vec![0u8; 700]).unwrap();
        std::fs::write(index_dir.join("judged-gemma4-26b-a4b-it-ud-q4km.bin"), vec![0u8; 50]).unwrap();
        let s = storage_in(Some(&index_dir), Some(&models_dir));
        assert_eq!((s.models_bytes, s.judge_bytes), (1300, 5000), "DINOv2's folder still counts; the judge's apart");
        assert_eq!(s.judges, [(JudgeChoice::Qwen3VlReranker, 0), (JudgeChoice::Gemma4, 5000)], "per judge, in the list's order");
        assert_eq!((s.heads_bytes, s.judged_bytes), (700, 50));
        models::delete_all_but_judges(&models_dir);
        let s = storage_in(Some(&index_dir), Some(&models_dir));
        assert_eq!((s.models_bytes, s.judge_bytes), (0, 5000), "Delete removed DINOv2's folder; the judge has its own row");
        assert!(!models_dir.join("dinov2-base").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Decision 11 through the engine.  On the processor: the first
    /// index's end puts the fursuit photos to the user (`Step::Asking`),
    /// nothing scans on its own, a skip is remembered, and the tab's
    /// catch-up starts and finds the heads; the snapshot then holds
    /// fursuit groups of kind Fursuit and no people.  On the card the
    /// scan starts unasked at the first index's end.
    #[test]
    fn the_head_step_asks_on_the_processor_and_starts_on_the_card() {
        let files = [
            ("2025/Mar-2025/12-Mar-2025", "0001-fursuit-90.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0002-fursuit-80.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0003-dog-60.JPG"),
            ("2025/Mar-2025/13-Mar-2025", "0004-fursuit-70.JPG"),
        ];
        let axes: EmbedderFactory = Box::new(|_| Ok(Arc::new(Axes) as Arc<dyn Embedder>));
        let (bus, _rx) = crate::bus::channel();
        let (lib, d) = library_of("heads-cpu", &files);
        let config = Config { factory: axes, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Cpu, true, false, ModelId::AuraFace, true, config);
        // Three heads are enough to whiten here (the library needs 512).
        e.set_min_fit(2);
        e.on_library(lib);
        wait_for(&e, "the library", |s| s.library_shots == 4);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the first index", |s| s.indexed_shots == 4 && s.pass.is_none());
        wait_for(&e, "the question", |s| matches!(s.heads.step, heads::Step::Asking(_)));
        let s = e.status();
        assert_eq!(s.heads.step, heads::Step::Asking(heads::Offer { photos: 3, why: HeadWhy::FirstIndex }), "the three fursuit photos, not the dog");
        assert_eq!((s.heads.on, s.heads.models_installed, s.heads.tagged, s.heads.scanned), (true, true, 3, 0));
        e.answer_heads(false);
        wait_for(&e, "the skip", |s| s.heads.step == heads::Step::Idle);
        assert_eq!(e.status().heads.skipped, 3);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!((e.status().heads.step, e.status().heads.scanned), (heads::Step::Idle, 0), "nothing starts by itself");
        e.find_heads();
        wait_for(&e, "the catch-up", |s| s.heads.scanned == 3 && s.heads.step == heads::Step::Idle);
        assert_eq!((e.status().heads.found, e.status().heads.skipped), (3, 0));
        let started = Instant::now();
        while e.subjects().groups.iter().all(|g| g.kind != Kind::Fursuit) {
            assert!(started.elapsed() < Duration::from_secs(5), "fursuit groups never appeared: {:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        }
        let snap = e.subjects();
        assert!(snap.groups.iter().all(|g| g.kind == Kind::Fursuit && matches!(g.id, GroupId::Cluster(_)) && g.name.is_none()), "{:?}", snap.groups);
        assert_eq!(snap.groups.iter().map(|g| g.photos).sum::<usize>(), 3);
        // Forgetting people leaves the fursuits; forgetting fursuits
        // empties the head cache and offers the catch-up again.
        e.forget_kind(Kind::Person);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(e.subjects().groups.iter().map(|g| g.photos).sum::<usize>(), 3, "people's forget leaves the fursuit groups");
        assert_eq!(e.status().heads.scanned, 3);
        e.forget_kind(Kind::Fursuit);
        wait_for(&e, "the fursuits forgotten", |s| s.heads.scanned == 0 && s.heads.found == 0);
        assert!(e.subjects().groups.is_empty());
        assert_eq!(e.status().heads.unscanned(), 3, "the catch-up is on offer again");
        // The card: the scan follows the first index without a question.
        let (bus2, _rx2) = crate::bus::channel();
        let (lib2, d2) = library_of("heads-gpu", &files);
        let axes: EmbedderFactory = Box::new(|_| Ok(Arc::new(Axes) as Arc<dyn Embedder>));
        let config = Config { factory: axes, installed: Box::new(|_| true), index_dir: Some(d2.join("index")), subjects_path: None, downloader: no_downloads() };
        let e2 = Engine::with_config(bus2, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e2.on_library(lib2);
        wait_for(&e2, "the library", |s| s.library_shots == 4);
        e2.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e2, "the scan after the first index", |s| s.heads.scanned == 3 && s.heads.step == heads::Step::Idle);
        assert!(!matches!(e2.status().heads.step, heads::Step::Asking(_)));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    /// A name shared by a person and a fursuit (the user, 2026-10-03: "the
    /// one you picked").  Typed, `@Alex` is the person; the fursuit of
    /// that name comes beside the results for the page to offer, and its
    /// photos are never in the list.  A picked chip is its own GroupId:
    /// the fursuit's chip searches the fursuit alone.
    #[test]
    fn a_name_shared_across_kinds_never_unions_results() {
        let files = [
            ("2025/Mar-2025/12-Mar-2025", "0001-fursuit-90.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0002-fursuit-80.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0003-dog-60.JPG"),
        ];
        let (bus, rx) = crate::bus::channel();
        let (lib, d) = library_of("shared-name", &files);
        let key_of = |day: &str, file: &str| Key::of(1, lib.root.join(day).join("JPG").join(file).as_os_str().as_encoded_bytes());
        let person_ref = FaceRef::new(key_of(files[2].0, files[2].1), [0.1, 0.1, 0.2, 0.2]);
        let suit_ref = FaceRef::new(key_of(files[0].0, files[0].1), [0.3, 0.2, 0.3, 0.4]);
        let mut file = crate::model::subjects::SubjectsFile::default();
        file.create(Kind::Person, Some("Alex".into()), vec![person_ref]);
        let suit = file.create(Kind::Fursuit, Some("Alex".into()), vec![suit_ref]);
        let path = d.join("subjects.json");
        file.save(&path).unwrap();
        let axes: EmbedderFactory = Box::new(|_| Ok(Arc::new(Axes) as Arc<dyn Embedder>));
        let config = Config { factory: axes, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(path), downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, false, config);
        e.on_library(lib.clone());
        wait_for(&e, "the library", |s| s.library_shots == 3);
        e.start_first_index(&[ModelId::Siglip2]);
        wait_for(&e, "the first index", |s| s.indexed_shots == 3 && s.pass.is_none());
        let mut ev = Events::new(&rx);
        let names = |hits: &[Hit]| hits.iter().map(|h| crate::util::stem(h.jpg.as_deref().or(h.raw.as_deref()).unwrap())[..4].to_string()).collect::<Vec<_>>();
        // Typed: the person, and the fursuit beside.
        e.query(1, "@Alex", super::super::CLOSENESS_DEFAULT, &[]);
        let ((hits, _, error, understood, ..), also) = ev.results_with_also(1);
        assert!(error.is_none() && understood.names == ["Alex"]);
        assert_eq!(names(&hits), ["0003"], "the person's photo only");
        assert_eq!(also, [("Alex".to_string(), GroupId::Subject(suit.clone()))], "the fursuit is offered, not searched");
        // Picked: the fursuit's chip is the fursuit alone, nothing beside.
        e.query(2, "", super::super::CLOSENESS_DEFAULT, &[GroupId::Subject(suit.clone())]);
        let ((hits, ..), also) = ev.results_with_also(2);
        assert_eq!(names(&hits), ["0001"], "the suit's photo only");
        assert!(also.is_empty());
        // Both chips picked: an AND of their photos, which share none.
        e.query(3, "", super::super::CLOSENESS_DEFAULT, &[GroupId::Subject(suit.clone()), GroupId::Subject(file.subjects[0].id.clone())]);
        let ((hits, ..), _) = ev.results_with_also(3);
        assert!(hits.is_empty(), "never a union: {hits:?}");
        let _ = std::fs::remove_dir_all(&d);
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
        let factory: EmbedderFactory = Box::new(move |spec: Spec| {
            let (device, text_only) = (spec.device, spec.text_only);
            log.lock().push((device, text_only));
            Ok(Arc::new(Axes) as Arc<dyn Embedder>)
        });
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
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
        let factory: EmbedderFactory = Box::new(move |spec: Spec| {
            let (device, text_only) = (spec.device, spec.text_only);
            log2.lock().push((device, text_only));
            Ok(Arc::new(Axes) as Arc<dyn Embedder>)
        });
        let e2 = Engine::with_config(bus2, Device::Gpu, true, true, ModelId::AuraFace, false, Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() });
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
