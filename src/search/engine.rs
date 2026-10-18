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
use super::step::KindStep;
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
use crate::model::decisions::{Recorder, Via};
use crate::model::subjects::{Face, FaceRef, Framing, GroupId, Kind, LabelBox, LabelEdit, LabelTarget, PendingWhy, PhotoLabels, Refused, SubjectId, Subjects};

mod record;

/// What a worker brought back for drawn boxes: per photo, the faces
/// recognised (with their landmarks and tiles) and the heads embedded;
/// or the card's failure, which stops the work and leaves them pending.
#[derive(Default)]
pub struct DrawnDone {
    faces: Vec<(Key, Vec<crate::ml::client::FaceFound>)>,
    heads: Vec<(Key, Vec<crate::ml::heads::Head>)>,
    card: Option<CardFailure>,
    /// A helper or file error, in words (the boxes stay pending and are
    /// tried at the next chance).
    error: Option<String>,
}

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
    Merge { into: GroupId, others: Vec<GroupId>, via: Via },
    SetHidden(GroupId, bool),
    SetCover(SubjectId, Option<FaceRef>, crate::model::decisions::At),
    Forget(SubjectId),
    ForgetKind(Kind),
    Faces { seq: u64, group: GroupId },
    Answer { group: GroupId, faces: Vec<FaceRef>, yes: Option<bool>, via: Via },
    Suggestions { seq: u64, group: GroupId },
    Undo,
    // The viewer's labels.
    PhotoLabels { seq: u64, target: LabelTarget },
    /// A photo's content keys, read on a worker (JPG first): answer
    /// `seq` with the first the index holds.
    Keyed { seq: u64, keys: Vec<Key> },
    Label(LabelEdit),
    /// Whole groups (or heads picked in them) are not of their kind.
    RemoveBoxes { kind: Kind, targets: Vec<(GroupId, Option<Vec<FaceRef>>)> },
    /// A review's X: these boxes (a press's frames) are not of `kind`.
    NotHeads { kind: Kind, faces: Vec<FaceRef>, at: crate::model::decisions::At },
    /// The review's X taken back (Z).
    RestoreHeads { kind: Kind, faces: Vec<FaceRef> },
    /// Match drawn boxes now (sent to itself after a label edit).
    StartDrawn,
    /// Drawn boxes matched (or not) on the card, from their worker.
    Drawn(Box<DrawnDone>),
    /// A regroup finished on its worker (either kind; `kind` says).
    Regrouped(Box<super::step::Regrouped>),
    // Fursuits (phase 4, decision 11).  Sent by the fursuit API, which
    // the pages call once they switch from their stand-in.
    SetFindHeads(bool),
    FindHeads,
    AnswerHeads(bool),
    StopHeads,
    FindMore { seq: u64, group: GroupId },
    // Fursuit merging, stage 2.
    MergeSuggestions { seq: u64, focus: Option<GroupId>, min_photos: usize, judge: bool },
    GroupCheck { seq: u64, group: GroupId },
    SetGrouping(heads::Grouping),
    HeadsOrder { seq: u64, kind: Kind, heads: Vec<FaceRef> },
    NearGroups { seq: u64, kind: Kind, heads: Vec<FaceRef> },
    ApplyEdit { seq: u64, edit: crate::model::subjects::GroupEdit, via: Via },
    NotSame { a: GroupId, b: GroupId, a_heads: Vec<FaceRef>, b_heads: Vec<FaceRef>, via: Via },
    /// A group is not one subject: its heads are kept apart.
    Dissolve { group: GroupId, at: crate::model::decisions::At },
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
    /// Every decision sent so far written to the record (the exit).
    FlushDecisions(Sender<()>),
    /// *Export…* the record with the named groups.
    ExportRecord { into: PathBuf, reply: Sender<std::io::Result<(PathBuf, usize)>> },
    Shutdown,
}

/// Match drawn boxes through the helper (on a worker): the faces through
/// the face model (their landmarks looked for in the box), the heads
/// through the search model on the head crop.  A restarted helper is
/// asked once more; a failing card stops the rest.
fn match_drawn(embedder: &dyn Embedder, faces: &[(Key, PathBuf, Vec<[f32; 4]>)], heads: &[crate::ml::heads::KnownHeads]) -> DrawnDone {
    use crate::ml::client::DrawnFaces;
    let mut done = DrawnDone::default();
    let fail = |done: &mut DrawnDone, e: HelperError| match e {
        HelperError::Card(f) => done.card = Some(f),
        HelperError::Restarted => done.error = Some("the helper started again".into()),
        HelperError::Broken(m) => done.error = Some(m),
    };
    if !faces.is_empty() {
        let files: Vec<(PathBuf, FileType)> = faces.iter().map(|(_, p, _)| (p.clone(), FileType::from_path(p).unwrap_or(FileType::Jpg))).collect();
        let want = Want::drawn(faces.iter().map(|(_, p, b)| DrawnFaces { path: p.clone(), boxes: b.clone() }).collect());
        let mut r = embedder.embed_images(&files, &want);
        if matches!(r, Err(HelperError::Restarted)) {
            r = embedder.embed_images(&files, &want);
        }
        match r {
            Ok(results) => {
                for ((key, _, _), res) in faces.iter().zip(results) {
                    match res.result {
                        // The file changed since it was indexed: its boxes
                        // wait for the next index.
                        Ok(e) if e.key == *key => done.faces.push((*key, e.faces)),
                        Ok(_) => {}
                        Err(e) => done.error = Some(e.message),
                    }
                }
            }
            Err(e) => {
                fail(&mut done, e);
                return done;
            }
        }
    }
    if !heads.is_empty() {
        let mut r = embedder.embed_heads(heads);
        if matches!(r, Err(HelperError::Restarted)) {
            r = embedder.embed_heads(heads);
        }
        match r {
            Ok(results) => {
                for (job, res) in heads.iter().zip(results) {
                    match res.result {
                        Ok(p) if p.key == job.key => done.heads.push((job.key, p.heads)),
                        Ok(_) => {}
                        Err(e) => done.error = Some(e.message),
                    }
                }
            }
            Err(e) => fail(&mut done, e),
        }
    }
    done
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
    /// The record of the user's corrections (`model::decisions`), beside
    /// the subjects file.
    decisions: Recorder,
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
        // The record lives beside the user's subjects file (in memory
        // only when that is, for tests).
        let decisions = Recorder::new(subjects_path.as_ref().map(|p| p.with_file_name(crate::model::decisions::FILE_NAME)));
        let recorder = decisions.clone();
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
                    merge_judging: None,
                    judge_token: 0,
                    embedder_has_faces: false,
                    drawn_running: false,
                    decisions: recorder,
                    answered: HashMap::new(),
                    not_heads: HashMap::new(),
                }
                .run()
            })
            .expect("spawn search-engine");
        Engine { tx, shared, decisions }
    }

    /// The record of the user's corrections: its switch, *Delete*,
    /// *Export* (a cheap handle; its writing is on its own thread).
    pub fn decisions(&self) -> Recorder {
        self.decisions.clone()
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
    /// *Export…*: the record of corrections and the named groups as they
    /// are now (`groups.json`), names replaced by codes, into a new
    /// folder in `into`; the folder and the entries copied.  **Blocks**
    /// until written: call it on a worker.
    pub fn export_record(&self, into: &Path) -> std::io::Result<(PathBuf, usize)> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(Cmd::ExportRecord { into: into.to_path_buf(), reply: tx });
        rx.recv().map_err(|_| std::io::Error::other("the search engine stopped"))?
    }

    /// Wait (at most `limit`) until the edits sent so far are in the
    /// decision record: the exit's last step, so a correction made just
    /// before quitting isn't lost.  **Blocks** for that long at most.
    pub fn flush_decisions(&self, limit: Duration) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(Cmd::FlushDecisions(tx));
        let _ = rx.recv_timeout(limit);
    }

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
    /// kinds differ.  Ctrl+Z ([`Engine::undo`]) takes it back.  `via`:
    /// how the page came to it, for the decision record.
    pub fn merge(&self, into: GroupId, others: Vec<GroupId>, via: Via) {
        self.send(Cmd::Merge { into, others, via });
    }

    /// Hide a group from its tab and from completions (its photos still
    /// come up by name), or show it again.
    pub fn set_hidden(&self, id: GroupId, hidden: bool) {
        self.send(Cmd::SetHidden(id, hidden));
    }

    /// A named group's photo: one of its faces, or `None` for the
    /// automatic pick again.  One undo step, recorded as `cover` (made
    /// `at` there); the tile changes at once (nothing regroups).
    pub fn set_cover(&self, id: SubjectId, face: Option<FaceRef>, at: crate::model::decisions::At) {
        self.send(Cmd::SetCover(id, face, at));
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

    /// A review answer: `Some(true)` adds the faces to the group,
    /// `Some(false)` is "not this person" (out of the members, never
    /// suggested again), `None` takes an answer back.  One call covers a
    /// burst's frames.
    pub fn answer(&self, group: GroupId, faces: Vec<FaceRef>, yes: Option<bool>, via: Via) {
        self.send(Cmd::Answer { group, faces, yes, via });
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

    /// The viewer's labels for a photo: `BgEvent::PhotoLabels { seq, .. }`
    /// with its faces and fursuit heads, named or not.  A photo whose key
    /// the index doesn't hold answers with no key (nothing was looked for
    /// in it: decision 14).  A library file's key comes from the path
    /// map; any other file's head is read on a worker, never here.
    pub fn photo_labels(&self, seq: u64, target: LabelTarget) {
        self.send(Cmd::PhotoLabels { seq, target });
    }

    /// A change on a box of the viewer's labels: name it, change its
    /// kind, draw one, remove one.  One undo step each ([`Engine::undo`]);
    /// refused, with nothing changed, when it would mix kinds.  A drawn
    /// box's vector is made on the graphics card only; until then it is
    /// kept and listed as pending.  A fresh `BgEvent::Subjects` follows.
    pub fn label(&self, edit: LabelEdit) {
        self.send(Cmd::Label(edit));
    }

    /// "Not fursuit heads" (or "Not faces") on groups of `kind`: each
    /// target's boxes (`None`: every member; else the ones picked) go to
    /// the removed list, as the viewer's "Not a fursuit head" does, in
    /// one undo step; refused, with nothing changed, when a group is of
    /// another kind.  A fresh `BgEvent::Subjects` follows.
    pub fn remove_boxes(&self, kind: Kind, targets: Vec<(GroupId, Option<Vec<FaceRef>>)>) {
        self.send(Cmd::RemoveBoxes { kind, targets });
    }

    /// X in a review ("Same fursuit?", "Possibly the same fursuit"):
    /// these boxes of `kind` (the press's frames, or a group's shown head)
    /// are not heads (or faces) at all.  They go to the removed list, as
    /// the viewer's removal does, in one undo step, written to the record
    /// as made `at` there.  Refused, with nothing changed, when a box isn't
    /// a detection of `kind`.
    pub fn not_heads(&self, kind: Kind, faces: Vec<FaceRef>, at: crate::model::decisions::At) {
        self.send(Cmd::NotHeads { kind, faces, at });
    }

    /// The "Same …?" review's X taken back (Z): the boxes come back, into
    /// the subjects they were in; one step, recorded as an undo of it.
    pub fn restore_heads(&self, kind: Kind, faces: Vec<FaceRef>) {
        self.send(Cmd::RestoreHeads { kind, faces });
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

    /// *Possibly the same fursuit* (stage 2): pairs of fursuit groups
    /// whose members are close on average, nearest first
    /// (`BgEvent::MergeSuggestions`), in the step's own space and
    /// cut-offs (raw or whitened).  Groups of fewer than `min_photos`
    /// photos (a burst counting as one) are left out unless named; hidden ones always; pairs the
    /// user said are not the same never come back.  `focus`: only that
    /// group's pairs (its page's *Similar groups*).  With `judge` and a
    /// judge that can run, the judge compares the close pairs on the card
    /// (`BgEvent::MergeJudged`); otherwise the suggest cut alone decides.
    /// Suggestions only: nothing is merged.
    pub fn merge_suggestions(&self, seq: u64, focus: Option<GroupId>, min_photos: usize, judge: bool) {
        self.send(Cmd::MergeSuggestions { seq, focus, min_photos, judge });
    }

    /// Preferences ▸ Fursuits ▸ *Grouping*: how tight the automatic
    /// fursuit groups are.  A change regroups on a worker
    /// (`HeadsStatus.regrouping` while it runs); names, answers, "not the
    /// same" and labels point at heads and stay.  The cut-offs in force are
    /// `Status.heads.cuts()`.
    pub fn set_fursuit_grouping(&self, g: heads::Grouping) {
        self.send(Cmd::SetGrouping(g));
    }

    /// A set of fursuit heads farthest from the rest of the set first
    /// (`BgEvent::HeadsOrder`): the sides of a split being arranged.
    pub fn heads_order(&self, seq: u64, kind: Kind, heads: Vec<FaceRef>) {
        self.send(Cmd::HeadsOrder { seq, kind, heads });
    }

    /// Every group of `kind` with its average-linkage distance to `heads`
    /// (`BgEvent::NearGroups`): the group editor's panel, nearest first.
    pub fn near_groups(&self, seq: u64, kind: Kind, heads: Vec<FaceRef>) {
        self.send(Cmd::NearGroups { seq, kind, heads });
    }

    /// The group editor's *Apply*, as one undo step
    /// (`BgEvent::EditApplied`, or `EditRefused` with nothing changed).
    pub fn apply_edit(&self, seq: u64, edit: crate::model::subjects::GroupEdit, via: Via) {
        self.send(Cmd::ApplyEdit { seq, edit, via });
    }

    /// A fursuit group's split check (stage 3): `BgEvent::GroupCheck`
    /// with its heads farthest from the group first, the heads seen
    /// together in one photo, and a proposed split around them.  Nothing
    /// moves: the user splits through `move_faces`.
    pub fn group_check(&self, seq: u64, group: GroupId) {
        self.send(Cmd::GroupCheck { seq, group });
    }

    /// "Not the same": two fursuit groups a suggestion offered are
    /// different suits; kept by their heads (`a_heads`, `b_heads`, as
    /// shown) in the user's file, one undo step.
    pub fn not_same(&self, a: GroupId, b: GroupId, a_heads: Vec<FaceRef>, b_heads: Vec<FaceRef>, via: Via) {
        self.send(Cmd::NotSame { a, b, a_heads, b_heads, via });
    }

    /// Dissolve a group of either kind: it isn't one person or one
    /// fursuit, only lookalikes.  Its heads leave it (a subject's name and
    /// answers go) and are never put in one automatic group together
    /// again; each stays a face or head, free for any other group.  One
    /// undo step, recorded as `dissolve`, made `at` there.  A fresh
    /// `BgEvent::Subjects` follows.
    pub fn dissolve(&self, group: GroupId, at: crate::model::decisions::At) {
        self.send(Cmd::Dissolve { group, at });
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
    /// Each indexed shot's files (JPG and RAW) → its key: the viewer's
    /// labels find a library photo's key without reading it.
    by_path: HashMap<PathBuf, Key>,
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
    /// The merge suggestions the judge is comparing (stage 2); never at
    /// the same time as a review's close calls.
    merge_judging: Option<MergeJudging>,
    judge_token: u64,
    /// The running helper was started with the face models.
    embedder_has_faces: bool,
    /// Drawn boxes are being matched on a worker (one batch at a time).
    drawn_running: bool,
    /// The record of the user's corrections.
    decisions: Recorder,
    /// Review answers recorded this session, by their heads: an answer
    /// taken back names its entry.
    answered: HashMap<Vec<FaceRef>, (String, Kind)>,
    /// A review's "Not a fursuit head" this session, by its boxes (in
    /// order): the memberships they lost and the drawn boxes that went,
    /// for its take back.
    not_heads: HashMap<Vec<FaceRef>, super::faces::Taken>,
}

/// One pair of groups the judge compares: its head pairs and their
/// answers so far (`Some(None)`: couldn't be judged, a no).
struct MergeCall {
    pair: super::merging::GroupPair,
    questions: Vec<(FaceRef, FaceRef)>,
    answers: Vec<Option<Option<bool>>>,
}

/// The merge suggestions being compared for request `seq`.
struct MergeJudging {
    seq: u64,
    token: u64,
    calls: Vec<MergeCall>,
    /// Indices into `calls` still to settle, nearest first.
    pending: Vec<usize>,
    done: usize,
    total: usize,
    beyond: usize,
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
                    self.start_drawn();
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
                        self.start_drawn();
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
                    let before = self.before_name_group(&id, &name);
                    let r = self.people.name_group(&id, name);
                    if r.is_ok() {
                        self.record(before, Via::Hand(crate::model::decisions::At::Name));
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Rename(id, name) => {
                    let id = GroupId::Subject(id);
                    let before = self.before_name_group(&id, &name);
                    let r = self.people.name_group(&id, name);
                    if r.is_ok() {
                        self.record(before, Via::Hand(crate::model::decisions::At::Name));
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Merge { into, others, via } => {
                    let before = self.before_merge(&into, &others);
                    let r = self.people.merge(&into, &others);
                    if r.is_ok() {
                        self.record(before, via);
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::SetHidden(id, hidden) => {
                    let r = self.people.set_hidden(&id, hidden);
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::SetCover(id, face, at) => {
                    let before = self.before_cover(&id, face);
                    match self.people.set_cover(&id, face) {
                        Ok(_) => {
                            self.record(before, Via::Hand(at));
                            // Only the tile changes: no regroup.
                            self.publish_subjects();
                        }
                        Err(e) => eprintln!("subjects: refused: {e}"),
                    }
                }
                Cmd::Forget(id) => {
                    let r = self.people.forget(&id);
                    self.after_edit(r);
                }
                Cmd::ForgetKind(kind) => {
                    // The record's entries of the kind go with it.
                    self.decisions.forget_kind(kind);
                    self.answered.retain(|_, (_, k)| *k != kind);
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
                Cmd::Answer { group, faces, yes, via } => {
                    let before = match yes {
                        Some(y) => self.before_answer(&group, &faces, y),
                        None => None,
                    };
                    // Taking an answer back is an undo of its entry.
                    let undone = if yes.is_none() { self.answered_entry(&faces) } else { None };
                    let r = self.people.answer(&group, faces, yes);
                    if r.is_ok() {
                        match yes {
                            Some(_) => self.record(before, via),
                            None => self.record_undo(undone, true),
                        }
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::Suggestions { seq, group } => self.send_suggestions(seq, group, false),
                Cmd::PhotoLabels { seq, target } => self.photo_labels(seq, target),
                Cmd::Keyed { seq, keys } => {
                    let key = keys.into_iter().find(|k| self.indexed(k));
                    self.send_labels(seq, key);
                }
                Cmd::Label(edit) => {
                    let draws = matches!(edit, LabelEdit::Add { .. } | LabelEdit::Kind { .. });
                    let before = self.before_label(&edit);
                    let r = self.people.label(&edit);
                    if r.is_ok() {
                        let entry = self.after_label(&edit, before);
                        self.record(entry, Via::Hand(crate::model::decisions::At::Viewer));
                    }
                    if let Ok(what) = &r {
                        if std::env::var_os("PP_PROFILE").is_some() {
                            eprintln!("labels: {what}");
                        }
                    }
                    self.after_edit(r.map(|_| ()));
                    // After what is queued (the page asking for the labels
                    // again): starting the helper takes seconds, and the
                    // box should show, pending, at once.
                    if draws {
                        let _ = self.me.send(Cmd::StartDrawn);
                    }
                }
                Cmd::NotHeads { kind, faces, at } => {
                    // Only detections of the kind: never a mix.
                    let all_of_kind = faces.iter().all(|f| self.is_detection(kind, f));
                    let r = if all_of_kind {
                        let before = self.before_not_heads(kind, &faces);
                        let r = self.people.remove_faces(kind, &faces);
                        if let Ok((_, taken)) = &r {
                            self.not_heads.insert(faces.clone(), taken.clone());
                            self.record(before, Via::Hand(at));
                        }
                        r.map(|_| ())
                    } else {
                        Err(Refused::MixedKinds)
                    };
                    self.after_edit(r);
                }
                Cmd::RestoreHeads { kind, faces } => {
                    // Commands run in order: the X this takes back was
                    // applied before it, whatever came in between.
                    let taken = self.not_heads.remove(&faces).unwrap_or_default();
                    let r = self.people.restore_faces(kind, &faces, &taken);
                    if r.is_ok() {
                        let undone = self.answered_entry(&faces);
                        self.record_undo(undone, true);
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::RemoveBoxes { kind, targets } => {
                    let before = self.before_remove_boxes(kind, &targets);
                    let r = self.people.remove_boxes(kind, &targets);
                    if let Ok((what, heads)) = &r {
                        if std::env::var_os("PP_PROFILE").is_some() {
                            eprintln!("labels: {what}");
                        }
                        let at = if kind == Kind::Fursuit { crate::model::decisions::At::Heads } else { crate::model::decisions::At::Faces };
                        self.record(Worker::removed_heads(before, heads), Via::Hand(at));
                    }
                    self.after_edit(r.map(|_| ()));
                }
                Cmd::StartDrawn => self.start_drawn(),
                Cmd::Drawn(done) => self.on_drawn(*done),
                Cmd::FindMore { seq, group } => self.send_suggestions(seq, group, true),
                Cmd::MergeSuggestions { seq, focus, min_photos, judge } => self.send_merge_suggestions(seq, focus, min_photos, judge),
                Cmd::SetGrouping(g) => {
                    if self.fursuits.set_grouping(g) {
                        self.people.set_other_clusters(Kind::Fursuit, Vec::new());
                        self.fursuits.status.regrouping = true;
                        self.start_heads_regroup();
                        // Nothing to group (no heads with vectors yet): done.
                        if !self.heads_regroup_running {
                            self.fursuits.status.regrouping = false;
                        }
                        self.publish_subjects();
                    }
                    self.publish_heads();
                }
                Cmd::HeadsOrder { seq, kind, heads } => {
                    let far = self.far_order_of(kind, &heads);
                    let _ = self.bus.unbounded_send(BgEvent::HeadsOrder { seq, far: Arc::new(far) });
                }
                Cmd::NearGroups { seq, kind, heads } => {
                    let list = self.near_groups_of(kind, &heads);
                    let _ = self.bus.unbounded_send(BgEvent::NearGroups { seq, list: Arc::new(list) });
                }
                Cmd::ApplyEdit { seq, edit, via } => match self.people.plan_edit(&edit) {
                    Err(e) => {
                        let gone = match &e {
                            Refused::NoSuchSubject(s) => Some(s.clone()),
                            _ => None,
                        };
                        let _ = self.bus.unbounded_send(BgEvent::EditRefused { seq, why: e.to_string(), gone });
                    }
                    Ok(plan) => {
                        let before = self.before_edit(&plan);
                        match self.people.apply_edit(&plan) {
                            Ok((words, _)) => {
                                self.record(before, via);
                                let _ = self.bus.unbounded_send(BgEvent::EditApplied { seq, words });
                                self.after_edit(Ok(()));
                            }
                            Err(e) => {
                                let _ = self.bus.unbounded_send(BgEvent::EditRefused { seq, why: e.to_string(), gone: None });
                                self.after_edit(Err(e));
                            }
                        }
                    }
                },
                Cmd::GroupCheck { seq, group } => {
                    self.fursuits.step();
                    // A person's group: its faces farthest first (the two
                    // groups side by side), no photo pairs or proposal.
                    let (far, pairs, split) = match self.fursuits.check(&self.people.file, &group) {
                        Some(c) => c,
                        None => (self.people.far_order(&group).unwrap_or_default(), Vec::new(), None),
                    };
                    let _ = self.bus.unbounded_send(BgEvent::GroupCheck { seq, group, far: Arc::new(far), pairs: Arc::new(pairs), split: split.map(Arc::new) });
                }
                Cmd::NotSame { a, b, a_heads, b_heads, via } => {
                    let before = self.before_not_same(&a, &b, &a_heads, &b_heads);
                    let r = self.people.not_same(&a, &b, a_heads, b_heads);
                    if r.is_ok() {
                        self.record(before, via);
                    }
                    self.after_edit(r);
                }
                Cmd::Dissolve { group, at } => {
                    let before = self.before_dissolve(&group);
                    let r = self.people.dissolve(&group);
                    if let Ok((what, _)) = &r {
                        if std::env::var_os("PP_PROFILE").is_some() {
                            eprintln!("subjects: {what}");
                        }
                        self.record(before, Via::Hand(at));
                    }
                    self.after_edit(r.map(|_| ()));
                }
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
                    if let Some((what, recorded)) = self.people.undo_recorded() {
                        self.record_undo(recorded, false);
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
                            if std::mem::take(&mut self.fursuits.status.regrouping) {
                                self.publish_heads();
                            }
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
                Cmd::ExportRecord { into, reply } => self.export_record(&into, reply),
                Cmd::FlushDecisions(reply) => {
                    self.decisions.flush();
                    let _ = reply.send(());
                }
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
    /// Any set of a kind's heads farthest from the rest of the set first.
    fn far_order_of(&mut self, kind: Kind, heads: &[FaceRef]) -> Vec<(FaceRef, f32)> {
        let Some(step) = self.edit_step(kind) else { return Vec::new() };
        let want: HashSet<&FaceRef> = heads.iter().collect();
        let refs = step.refs();
        let rows: Vec<usize> = refs.iter().enumerate().filter(|(_, r)| want.contains(r)).map(|(i, _)| i).collect();
        super::splitting::far_order(&step.rows(), &rows).into_iter().map(|(m, d)| (refs[m], d)).collect()
    }

    /// Every group of `kind` on the page now, with its average linkage to
    /// `heads`, nearest first (groups without vectors last).
    fn near_groups_of(&mut self, kind: Kind, heads: &[FaceRef]) -> Vec<(GroupId, f32)> {
        let snapshot = self.shared.subjects.lock().clone();
        let groups: Vec<(GroupId, Vec<FaceRef>)> = snapshot.groups.iter().filter(|g| g.kind == kind).filter_map(|g| self.people.members(&g.id).ok().map(|(_, m)| (g.id.clone(), m))).collect();
        let Some(step) = self.edit_step(kind) else { return groups.into_iter().map(|(g, _)| (g, 2.0)).collect() };
        let index: HashMap<&FaceRef, usize> = step.refs().iter().enumerate().map(|(i, r)| (r, i)).collect();
        let rows_of = |faces: &[FaceRef]| -> Vec<usize> { faces.iter().filter_map(|f| index.get(f).copied()).collect() };
        let mine = rows_of(heads);
        let all: Vec<Vec<usize>> = groups.iter().map(|(_, m)| rows_of(m)).collect();
        let mut sets: Vec<&[usize]> = vec![&mine];
        sets.extend(all.iter().map(|v| v.as_slice()));
        let rows = step.rows();
        let means = super::merging::means(&rows, &sets);
        let mut out: Vec<(GroupId, f32)> = groups
            .into_iter()
            .enumerate()
            .map(|(i, (g, _))| {
                let d = if mine.is_empty() || all[i].is_empty() { 2.0 } else { 1.0 - means[0].iter().zip(&means[i + 1]).map(|(x, y)| x * y).sum::<f32>() };
                (g, d)
            })
            .collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }

    /// The kind's vectors, for the editor's orders and distances.
    fn edit_step(&mut self, kind: Kind) -> Option<&dyn super::step::KindStep> {
        match kind {
            Kind::Person => self.people.store.as_ref().map(|s| s as &dyn super::step::KindStep),
            Kind::Fursuit => self.fursuits.step().map(|s| s as &dyn super::step::KindStep),
            Kind::Pet => None,
        }
    }

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

    // ---- the viewer's labels ----

    /// The index holds this photo (its faces and heads were looked for
    /// there, decision 14).
    /// A box is a detection (found or drawn) of `kind`.
    fn is_detection(&self, kind: Kind, f: &FaceRef) -> bool {
        match kind {
            Kind::Person => self.people.store.as_ref().is_some_and(|s| s.detection(f).is_some()),
            Kind::Fursuit => self.fursuits.store.as_ref().is_some_and(|s| s.detection(f).is_some()),
            Kind::Pet => false,
        }
    }

    fn indexed(&self, key: &Key) -> bool {
        self.index.as_ref().is_some_and(|ix| ix.vectors.contains(key))
    }

    /// Find the photo's key and answer with its labels: a library file's
    /// from the path map at once; any other file's by reading its head on
    /// a worker (`Cmd::Keyed` brings it back).
    fn photo_labels(&mut self, seq: u64, target: LabelTarget) {
        let (jpg, raw) = match target {
            LabelTarget::Key(k) => {
                let key = self.indexed(&k).then_some(k);
                return self.send_labels(seq, key);
            }
            LabelTarget::Files { jpg, raw } => (jpg, raw),
        };
        if self.index.as_ref().is_none_or(|ix| ix.is_empty()) {
            return self.send_labels(seq, None);
        }
        self.ensure_resolver();
        let known = self.resolver.as_ref().and_then(|r| jpg.iter().chain(raw.iter()).find_map(|p| r.by_path.get(p).copied()));
        if let Some(k) = known {
            return self.send_labels(seq, Some(k));
        }
        // Not a library file the index knows: a copy of one has the same
        // content key.  Read it off this thread.
        let me = self.me.clone();
        std::thread::Builder::new()
            .name("labels-key".into())
            .spawn(move || {
                let keys: Vec<Key> = jpg.iter().chain(raw.iter()).filter_map(|p| Key::read(p).ok()).collect();
                let _ = me.send(Cmd::Keyed { seq, keys });
            })
            .expect("spawn labels-key");
    }

    fn send_labels(&mut self, seq: u64, key: Option<Key>) {
        let labels = match key {
            Some(k) => self.labels_of(k),
            None => PhotoLabels::default(),
        };
        let _ = self.bus.unbounded_send(BgEvent::PhotoLabels { seq, labels: Arc::new(labels) });
    }

    /// A photo's boxes: the faces found and drawn (less those removed),
    /// then the fursuit heads found and drawn, each with its name.
    fn labels_of(&self, key: Key) -> PhotoLabels {
        let file = &self.people.file;
        let mut boxes: Vec<LabelBox> = Vec::new();
        let named = |kind: Kind, face: &FaceRef| -> (Option<SubjectId>, Option<String>) {
            match file.member_of(kind, face) {
                Some(s) => (Some(s.id.clone()), s.name.clone()),
                None => (None, None),
            }
        };
        for kind in [Kind::Person, Kind::Fursuit] {
            let removed = file.removed_index(kind);
            let found: Vec<FaceRef> = match kind {
                Kind::Person => self.people.store.as_ref().map(|s| s.faces_of(&key).iter().map(|d| d.face_ref(key)).collect()).unwrap_or_default(),
                _ => self.fursuits.store.as_ref().map(|s| s.heads_of(&key).iter().map(|d| d.head_ref(key)).collect()).unwrap_or_default(),
            };
            for face in found.into_iter().filter(|f| !removed.covers(f)) {
                if boxes.iter().any(|b| b.face == face && b.kind == kind) {
                    continue;
                }
                let (subject, name) = named(kind, &face);
                boxes.push(LabelBox { face, kind, subject, name, drawn: false, pending: false });
            }
            for face in file.added_of(kind).filter(|f| f.key == key).copied().collect::<Vec<_>>() {
                boxes.retain(|b| !(b.face == face && b.kind == kind));
                let matched = match kind {
                    Kind::Person => self.people.store.as_ref().is_some_and(|s| s.is_drawn(&face) && s.has_vector(&face)),
                    _ => self.fursuits.store.as_ref().is_some_and(|s| s.is_drawn(&face) && s.has_vector(&face)),
                };
                let (subject, name) = named(kind, &face);
                boxes.push(LabelBox { face, kind, subject, name, drawn: true, pending: !matched });
            }
        }
        let pending = boxes.iter().any(|b| b.pending).then(|| self.pending_why(boxes.iter().any(|b| b.pending && b.kind == Kind::Person)));
        PhotoLabels { key: Some(key), boxes, pending }
    }

    /// Why drawn boxes wait: the first thing in the way.
    fn pending_why(&self, faces: bool) -> PendingWhy {
        if self.device != Device::Gpu {
            PendingWhy::ProcessorChosen
        } else if matches!(self.shared.status.lock().stopped, Some(Stopped::Card(_))) {
            PendingWhy::NoCard
        } else if !self.models_ready() {
            PendingWhy::NoSearchModel
        } else if faces && !self.faces_ready() {
            PendingWhy::NoFaceModel
        } else {
            PendingWhy::Matching
        }
    }

    /// Match the drawn boxes that have no vector yet, on the graphics card
    /// only (the user's rule for this work: a processor chosen, or a card
    /// that failed, leaves them pending, never on the processor).  One
    /// batch at a time on a worker; `Cmd::Drawn` brings the results.
    fn start_drawn(&mut self) {
        if self.drawn_running || self.device != Device::Gpu || !self.models_ready() {
            return;
        }
        if matches!(self.shared.status.lock().stopped, Some(Stopped::Card(_))) {
            return;
        }
        self.ensure_resolver();
        let Some(r) = &self.resolver else { return };
        let file = &self.people.file;
        // Per photo: the boxes still to match, with the shot's file.
        let mut faces: HashMap<Key, (PathBuf, Vec<[f32; 4]>)> = HashMap::new();
        let mut heads: HashMap<Key, (PathBuf, FileType, Vec<[f32; 4]>)> = HashMap::new();
        if self.faces_ready() {
            for f in file.added_of(Kind::Person) {
                if self.people.store.as_ref().is_some_and(|s| s.is_drawn(f) && s.has_vector(f)) || self.people.store.is_none() {
                    continue;
                }
                let Some(shot) = r.shots.get(&f.key) else { continue };
                let Some(path) = shot.jpg.clone().or_else(|| shot.raw.clone()) else { continue };
                faces.entry(f.key).or_insert_with(|| (path, Vec::new())).1.push(f.bbox);
            }
        }
        for f in file.added_of(Kind::Fursuit) {
            if self.fursuits.store.as_ref().is_none_or(|s| s.is_drawn(f) && s.has_vector(f)) {
                continue;
            }
            let Some(shot) = r.shots.get(&f.key) else { continue };
            let (path, ft) = match (&shot.jpg, &shot.raw) {
                (Some(j), _) => (j.clone(), FileType::Jpg),
                (None, Some(raw)) => (raw.clone(), FileType::Raw),
                _ => continue,
            };
            heads.entry(f.key).or_insert_with(|| (path, ft, Vec::new())).2.push(f.bbox);
        }
        if faces.is_empty() && heads.is_empty() {
            return;
        }
        // Faces need a helper with the face models: start one again when
        // the running one has none, unless a pass or a scan is using it
        // (their end comes back here).
        if !faces.is_empty() && self.embedder.is_some() && !self.embedder_has_faces {
            if self.pass.is_some() || self.scan.is_some() {
                faces.clear();
                if heads.is_empty() {
                    return;
                }
            } else {
                self.drop_embedder();
            }
        }
        let Some(embedder) = self.image_embedder() else { return };
        if !self.embedder_has_faces {
            faces.clear();
        }
        if faces.is_empty() && heads.is_empty() {
            return;
        }
        self.drawn_running = true;
        let me = self.me.clone();
        let face_jobs: Vec<(Key, PathBuf, Vec<[f32; 4]>)> = faces.into_iter().map(|(k, (p, b))| (k, p, b)).collect();
        let head_jobs: Vec<crate::ml::heads::KnownHeads> = heads.into_iter().map(|(key, (path, file_type, boxes))| crate::ml::heads::KnownHeads { path, file_type, key, boxes }).collect();
        std::thread::Builder::new()
            .name("labels-drawn".into())
            .spawn(move || {
                let _t = crate::profile::span("labels", "drawn boxes");
                let done = match_drawn(embedder.as_ref(), &face_jobs, &head_jobs);
                let _ = me.send(Cmd::Drawn(Box::new(done)));
            })
            .expect("spawn labels-drawn");
    }

    /// Drawn boxes matched: their detections, vectors and tiles go into
    /// the caches (only boxes the user's file still lists), then both
    /// kinds group again.  A failing card stops here, as any card work.
    fn on_drawn(&mut self, done: DrawnDone) {
        self.drawn_running = false;
        let mut changed = false;
        if let Some(store) = &mut self.people.store {
            for (key, found) in &done.faces {
                let keep: Vec<crate::ml::client::FaceFound> = found.iter().filter(|f| self.people.file.is_added(Kind::Person, &FaceRef::new(*key, f.bbox)) && !f.vector.is_empty()).cloned().collect();
                if keep.is_empty() {
                    continue;
                }
                match store.add_drawn(*key, &keep) {
                    Ok(()) => changed = true,
                    Err(e) => eprintln!("labels: couldn't keep a drawn face: {e}"),
                }
            }
        }
        if let Some(store) = &mut self.fursuits.store {
            for (key, found) in &done.heads {
                let keep: Vec<crate::ml::heads::Head> = found.iter().filter(|h| self.people.file.is_added(Kind::Fursuit, &FaceRef::new(*key, h.bbox)) && !h.vector.is_empty()).cloned().collect();
                if keep.is_empty() {
                    continue;
                }
                match store.add_drawn(*key, &keep) {
                    Ok(()) => changed = true,
                    Err(e) => eprintln!("labels: couldn't keep a drawn head: {e}"),
                }
            }
        }
        if let Some(e) = &done.error {
            eprintln!("labels: drawn boxes not matched yet: {e}");
        }
        let failed = done.card.is_some() || done.error.is_some();
        if let Some(f) = done.card {
            self.drop_embedder();
            self.card_failed(f);
        }
        if changed {
            self.people.assign_new();
            self.start_regroup();
            self.start_heads_regroup();
            self.refresh_counts();
        }
        // Subjects (and with it the viewer's labels) follow either way:
        // a box may have stopped waiting, or a card failed.
        self.publish_subjects();
        // Boxes drawn meanwhile.
        if changed && !failed {
            self.start_drawn();
        }
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
        self.merge_judging = None;
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

    /// *Possibly the same fursuit*: pairs of fursuit groups close on
    /// average, in the step's space (raw or whitened) and cut-offs.  With
    /// the judge (`judge` asked and able to run) every pair up to sure-no
    /// is in play: a sure pair (under sure-yes, both covers clear enough)
    /// is offered at once, the rest are compared on up to three head pairs
    /// each, nearest first, at most `merging::MAX_PAIRS` pairs of groups
    /// a request; without it, the suggest cut alone.
    fn send_merge_suggestions(&mut self, seq: u64, focus: Option<GroupId>, min_photos: usize, judge: bool) {
        use super::merging::{self, GroupPair};
        self.ensure_resolver();
        self.fursuits.step();
        let empty = |me: &Self| {
            let _ = me.bus.unbounded_send(BgEvent::MergeSuggestions { seq, focus: focus.clone(), list: Arc::new(Vec::new()), judging: false });
        };
        let Some(cuts) = self.fursuits.cuts() else { return empty(self) };
        // The grouping choice's cuts in the step's space (raw or whitened).
        let (sure_yes, sure_no, suggest) = (cuts.merge_sure, cuts.merge_far, cuts.merge_suggest);
        let use_judge = judge && self.judge_status().active(self.device);
        let groups = self.fursuits.merge_groups(&self.people.file);
        // The groups as rows of the step, the cover first then clearest
        // first; small unnamed groups (folded away on the tab) left out.
        let mut cands: Vec<(merging::Candidate, Vec<Face>)> = Vec::new();
        {
            let Some(store) = self.fursuits.store.as_ref() else { return empty(self) };
            let by_ref: HashMap<FaceRef, usize> = store.refs().iter().enumerate().map(|(i, r)| (*r, i)).collect();
            // Heads the user removed in the viewer ("Not a fursuit
            // head"), or drew and took back, are in no pair, even if a
            // subject still lists one (found again after a rebuild).
            let out = super::step::Exclusion::new(&self.people.file, Kind::Fursuit);
            for (id, members, named, cover) in groups {
                let mut faces: Vec<Face> = members.iter().filter(|f| by_ref.contains_key(f) && !out.excludes(store, f)).filter_map(|f| self.resolve_face(f)).collect();
                // The tab's own count: a burst is one photo.
                let photos = crate::model::subjects::count_bursts(&faces);
                if faces.is_empty() || (!named && photos < min_photos) {
                    continue;
                }
                faces.sort_by(|a, b| b.clarity.total_cmp(&a.clarity));
                if let Some(c) = cover.and_then(|c| faces.iter().position(|f| f.face == c)) {
                    let f = faces.remove(c);
                    faces.insert(0, f);
                }
                let rows: Vec<usize> = faces.iter().filter_map(|f| by_ref.get(&f.face).copied()).collect();
                let heads: Vec<FaceRef> = faces.iter().map(|f| f.face).collect();
                cands.push((merging::Candidate { id, rows, heads }, faces));
            }
        }
        let Some(store) = self.fursuits.store.as_ref() else { return empty(self) };
        let rows = store.rows();
        let refs = store.refs();
        let lists: Vec<&[usize]> = cands.iter().map(|(c, _)| c.rows.as_slice()).collect();
        let means = merging::means(&rows, &lists);
        let focus_ix = match &focus {
            Some(f) => match cands.iter().position(|(c, _)| &c.id == f) {
                Some(i) => Some(i),
                None => return empty(self),
            },
            None => None,
        };
        let cut = if use_judge { sure_no } else { suggest };
        let told = self.people.file.not_same_index(Kind::Fursuit);
        let near: Vec<(usize, usize, f32)> = merging::pairs(&means, cut, focus_ix).into_iter().filter(|&(i, j, _)| !crate::model::subjects::told_apart(&told, &cands[i].0.heads, &cands[j].0.heads)).collect();
        let shown = |faces: &[Face]| Arc::new(faces.iter().take(4).cloned().collect::<Vec<_>>());
        let pair_of = |i: usize, j: usize, d: f32, sure: bool| GroupPair { a: cands[i].0.id.clone(), b: cands[j].0.id.clone(), distance: d, a_heads: shown(&cands[i].1), b_heads: shown(&cands[j].1), sure, verdict: None };
        if !use_judge {
            let list: Vec<GroupPair> = near.iter().map(|&(i, j, d)| pair_of(i, j, d, true)).collect();
            let _ = self.bus.unbounded_send(BgEvent::MergeSuggestions { seq, focus, list: Arc::new(list), judging: false });
            return;
        }
        // The judge compares the close pairs; sure and clear ones go at once.
        let mut offered = Vec::new();
        let mut calls: Vec<MergeCall> = Vec::new();
        for &(i, j, d) in &near {
            let (a, b) = (&cands[i].0, &cands[j].0);
            let clear = store.clarity(&a.heads[0]).min(store.clarity(&b.heads[0])) >= judging::CLARITY_MIN;
            if d < sure_yes && clear {
                offered.push(pair_of(i, j, d, true));
                continue;
            }
            let questions: Vec<(FaceRef, FaceRef)> = merging::questions(&rows, &a.rows, &b.rows, &means[i], &means[j]).into_iter().filter_map(|(x, y)| Some((*refs.get(x)?, *refs.get(y)?))).collect();
            let n = questions.len();
            calls.push(MergeCall { pair: pair_of(i, j, d, false), questions, answers: vec![None; n] });
        }
        // Verdicts kept answer at once; the rest wait for the judge, the
        // nearest first, up to the cap in head pairs.
        let (mut left_out, mut pending) = (Vec::new(), Vec::new());
        let (mut done, mut total) = (0, 0);
        let choice = self.judge_choice;
        for (ci, call) in calls.iter_mut().enumerate() {
            for (qi, &(x, y)) in call.questions.iter().enumerate() {
                if let Some(v) = self.verdicts().get(x, y) {
                    call.answers[qi] = Some(Some(v.same(choice)));
                }
            }
            if call.answers.iter().all(Option::is_some) {
                total += 1;
                done += 1;
                let v = merging::verdict(&call.answers.iter().map(|a| a.flatten()).collect::<Vec<_>>());
                call.pair.verdict = Some(v);
                if v.same() { offered.push(call.pair.clone()) } else { left_out.push(call.pair.clone()) }
                continue;
            }
            pending.push(ci);
        }
        // The nearest pairs of groups only; the rest wait for the next request.
        let (now, beyond) = merging::batch(pending.len());
        pending.truncate(now);
        total += now;
        let _ = self.bus.unbounded_send(BgEvent::MergeSuggestions { seq, focus, list: Arc::new(offered), judging: true });
        let _ = self.bus.unbounded_send(BgEvent::MergeJudged { seq, offered: Arc::new(Vec::new()), left_out: Arc::new(left_out), done, total, beyond });
        self.forget_judging();
        self.judge_token += 1;
        let token = self.judge_token;
        let none = pending.is_empty();
        self.merge_judging = Some(MergeJudging { seq, token, calls, pending, done, total, beyond });
        if none {
            self.judge_state = JudgeState::Idle;
            self.publish_judge();
        } else {
            self.merge_judge_go();
        }
    }

    /// Hand the head pairs of the merge suggestions still to settle to the
    /// runner (as `judge_go` does for a review's close calls: never while
    /// a pass or a head scan holds the card).
    fn merge_judge_go(&mut self) {
        self.ensure_resolver();
        let Some(j) = &self.merge_judging else { return };
        if j.pending.is_empty() {
            return;
        }
        if self.pass.is_some() || self.scan.is_some() {
            self.judge_state = JudgeState::WaitingForCard;
            self.publish_judge();
            return;
        }
        let token = j.token;
        let mut wanted: Vec<(usize, usize, FaceRef, FaceRef)> = Vec::new();
        for &ci in &j.pending {
            for (qi, &(x, y)) in j.calls[ci].questions.iter().enumerate() {
                if j.calls[ci].answers[qi].is_none() {
                    wanted.push((ci, qi, x, y));
                }
            }
        }
        let mut tasks = Vec::new();
        let mut seen = HashSet::new();
        let mut unshowable = Vec::new();
        for (ci, qi, x, y) in wanted {
            let crops = (self.resolve_face(&x).and_then(|f| self.judge_crop(&f)), self.resolve_face(&y).and_then(|f| self.judge_crop(&f)));
            match crops {
                (Some(a), Some(b)) => {
                    let key = PairKey::new(x, y);
                    if seen.insert(key) {
                        tasks.push(Task { token, key, ask: PairAsk { a, b } });
                    }
                }
                // A photo no longer in the library: that head pair can't
                // be judged, and counts as a no.
                _ => unshowable.push((ci, qi)),
            }
        }
        if let Some(j) = self.merge_judging.as_mut() {
            for (ci, qi) in unshowable {
                j.calls[ci].answers[qi] = Some(None);
            }
        }
        self.settle_merges();
        if tasks.is_empty() {
            self.judge_state = JudgeState::Idle;
            self.publish_judge();
            return;
        }
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
        let (done, total) = self.merge_judging.as_ref().map_or((0, 0), |j| (j.done, j.total));
        self.judge_state = JudgeState::Comparing { done, total };
        self.publish_judge();
    }

    /// A head pair of the merge suggestions judged (`None`: it couldn't
    /// be): every pair of groups asking it takes the answer.
    fn on_merge_verdict(&mut self, key: PairKey, same: Option<bool>) {
        let Some(j) = self.merge_judging.as_mut() else { return };
        for &ci in &j.pending {
            let call = &mut j.calls[ci];
            for qi in 0..call.questions.len() {
                let (x, y) = call.questions[qi];
                if call.answers[qi].is_none() && PairKey::new(x, y) == key {
                    call.answers[qi] = Some(same);
                }
            }
        }
        self.settle_merges();
    }

    /// Pairs of groups whose head pairs are all answered: their verdict
    /// (the majority; a head pair that couldn't be judged is a no), sent
    /// as it lands.
    fn settle_merges(&mut self) {
        // A pair whose shown head was removed since it was asked ("Not a
        // fursuit head" in the review) is settled nowhere: the review asks
        // for the pairs again.
        let out = self.people.file.removed_index(Kind::Fursuit);
        let Some(j) = self.merge_judging.as_mut() else { return };
        let (mut offered, mut left_out) = (Vec::new(), Vec::new());
        let mut gone = 0;
        let calls = &mut j.calls;
        j.pending.retain(|&ci| {
            let call = &mut calls[ci];
            if !call.answers.iter().all(Option::is_some) {
                return true;
            }
            let v = super::merging::verdict(&call.answers.iter().map(|a| a.flatten()).collect::<Vec<_>>());
            call.pair.verdict = Some(v);
            if [call.pair.a_heads.first(), call.pair.b_heads.first()].into_iter().flatten().any(|f| out.covers(&f.face)) {
                gone += 1;
            } else if v.same() {
                offered.push(call.pair.clone());
            } else {
                left_out.push(call.pair.clone());
            }
            false
        });
        if offered.is_empty() && left_out.is_empty() && gone == 0 {
            return;
        }
        j.done += offered.len() + left_out.len() + gone;
        let _ = self.bus.unbounded_send(BgEvent::MergeJudged { seq: j.seq, offered: Arc::new(offered), left_out: Arc::new(left_out), done: j.done, total: j.total, beyond: j.beyond });
        self.judge_state = if j.pending.is_empty() { JudgeState::Idle } else { JudgeState::Comparing { done: j.done, total: j.total } };
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
        if self.judging.as_ref().is_some_and(|j| !j.pending.is_empty()) || self.merge_judging.as_ref().is_some_and(|j| !j.pending.is_empty()) {
            self.judge_state = JudgeState::WaitingForCard;
        }
        self.publish_judge();
    }

    /// The card is free again: close calls held for it carry on.
    fn judge_resume(&mut self) {
        if matches!(self.judge_state, JudgeState::WaitingForCard) && self.pass.is_none() && self.scan.is_none() {
            self.judge_go();
            self.merge_judge_go();
        }
    }

    fn on_judge_event(&mut self, e: judging::Event) {
        match e {
            judging::Event::Loading => {
                if self.judging.as_ref().is_some_and(|j| !j.pending.is_empty()) || self.merge_judging.as_ref().is_some_and(|j| !j.pending.is_empty()) {
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
                if self.merge_judging.as_ref().is_some_and(|m| m.token == token) {
                    let same = result.as_ref().ok().map(|v| v.same(self.judge_choice));
                    self.on_merge_verdict(key, same);
                    self.publish_judge();
                    return;
                }
                // Heads removed since the call was asked ("Not a fursuit
                // head" in the review) are offered nowhere.
                let out = self.people.file.removed_index(Kind::Fursuit);
                let Some(j) = self.judging.as_mut().filter(|j| j.token == token) else { return };
                let Some(pos) = j.pending.iter().position(|&i| PairKey::new(j.calls[i].candidate, j.calls[i].member) == key) else { return };
                let i = j.pending.remove(pos);
                j.done += 1;
                let (mut offered, mut left_out) = (Vec::new(), Vec::new());
                let removed = j.calls[i].faces.iter().any(|f| out.covers(&f.face));
                match &result {
                    _ if removed => {}
                    Ok(v) if v.same(self.judge_choice) => offered.push(j.calls[i].faces.clone()),
                    Ok(_) => left_out.push(j.calls[i].faces.clone()),
                    Err(why) => eprintln!("judge: a close call couldn't be judged: {why}"),
                }
                let _ = self.bus.unbounded_send(BgEvent::Judged { seq: j.seq, group: j.group.clone(), offered: Arc::new(offered), left_out: Arc::new(left_out), done: j.done, total: j.total, beyond: j.beyond });
                self.judge_state = if j.pending.is_empty() { JudgeState::Idle } else { JudgeState::Judging { group: j.group.clone(), done: j.done, total: j.total } };
            }
            judging::Event::Idle => {
                if self.judging.as_ref().is_none_or(|j| j.pending.is_empty()) && self.merge_judging.as_ref().is_none_or(|j| j.pending.is_empty()) && !matches!(self.judge_state, JudgeState::WaitingForCard) {
                    self.judge_state = JudgeState::Idle;
                }
            }
            judging::Event::Failed(err) => {
                self.runner = None;
                self.judging = None;
                self.merge_judging = None;
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
            self.start_drawn();
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
        // A chosen group photo shows while its detection is still found.
        let found = |kind: Kind, f: &FaceRef| match kind {
            Kind::Person => self.people.store.as_ref().is_none_or(|s| s.detection(f).is_some()),
            Kind::Fursuit => self.fursuits.store.as_ref().is_none_or(|s| s.detection(f).is_some()),
            Kind::Pet => true,
        };
        let mut snapshot = (*self.people.build_snapshot_found(&|f| self.resolve_face(f), &found)).clone();
        self.fursuits.extend_snapshot(&mut snapshot, &|f| self.resolve_face(f));
        snapshot.seen_together = self.fursuits.seen_together(&self.people.file);
        // What dissolves left on their own: counted, not shown as groups.
        let loose = self.people.loose_groups();
        if !loose.is_empty() {
            let ids: HashSet<&GroupId> = loose.iter().map(|(id, _, _)| id).collect();
            snapshot.groups.retain(|g| !ids.contains(&g.id));
            for (_, kind, n) in &loose {
                *snapshot.loose.entry(*kind).or_default() += n;
            }
        }
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
        if root_changed {
            self.start_drawn();
        }
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
        // Faces drawn in the viewer need the face model too, when it is
        // in place, even with *Find people's faces* off.
        let with_faces = self.find_faces || (self.faces_ready() && self.people.file.added_of(Kind::Person).next().is_some());
        match (self.factory)(Spec::images(self.device, with_faces.then_some(self.face_model)).with_heads(with_heads)) {
            Ok(e) => {
                self.embedder_has_heads = with_heads;
                self.embedder_has_faces = with_faces;
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
                // A rebuild forgot the heads with the rest, so its end is
                // a first index again.
                match kind {
                    PassKind::FirstIndex | PassKind::Rebuild => self.offer_head_scan(HeadWhy::FirstIndex, false),
                    PassKind::NewExports => self.offer_head_scan(HeadWhy::NewExports, false),
                    PassKind::Faces => {}
                }
            }
            // Boxes the user drew that lost their vectors (a rebuild, a
            // clear) or waited for the card are matched now.
            self.start_drawn();
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
        let mut by_path: HashMap<PathBuf, Key> = HashMap::with_capacity(ix.vectors.len());
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
                for p in jpg.iter().chain(raw.iter()) {
                    by_path.insert(p.clone(), key);
                }
                shots.insert(key, Shot { day: day.date, jpg, raw, mtime: fe.mtime.local, burst: None });
            }
        }
        let bursts = Bursts::find(&coll);
        for (id, key) in key_of_record {
            if let (Some(b), Some(s)) = (bursts.of(id), shots.get_mut(&key)) {
                s.burst = Some(b as u64);
            }
        }
        self.resolver = Some(Resolver { library_ptr: ptr, paths_len: ix.paths.entries.len(), shots, by_path });
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

    /// What request `seq` of merge suggestions brought: the first list,
    /// then each `MergeJudged` (offered, left out, done, total), until one
    /// has `done == total` (or none comes within `wait`).
    #[allow(clippy::type_complexity)]
    fn merges(ev: &mut Events, seq: u64, wait: Duration) -> (Option<Vec<super::super::merging::GroupPair>>, Vec<(Vec<super::super::merging::GroupPair>, Vec<super::super::merging::GroupPair>, usize, usize)>) {
        let stream = &mut ev.stream;
        ev.rt.block_on(async {
            use iced::futures::StreamExt;
            let deadline = Instant::now() + wait;
            let (mut list, mut judged) = (None, Vec::new());
            while Instant::now() < deadline {
                let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await else { continue };
                for e in batch {
                    match e {
                        BgEvent::MergeSuggestions { seq: s, list: l, judging, .. } if s == seq => {
                            list = Some((*l).clone());
                            if !judging {
                                return (list, judged);
                            }
                        }
                        BgEvent::MergeJudged { seq: s, offered, left_out, done, total, .. } if s == seq => {
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

    /// The groups of a fursuit library by their cover's photo, once the
    /// grouping has them all.
    fn fursuit_groups(e: &Engine, n: usize) -> Vec<(GroupId, Key)> {
        let started = Instant::now();
        loop {
            let g: Vec<(GroupId, Key)> = e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).map(|g| (g.id.clone(), g.cover.face.key)).collect();
            if g.len() == n {
                return g;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Merge suggestions through the engine (stage 2), whitened: the pair
    /// of groups under the suggest cut by distance alone; with the judge,
    /// every pair up to sure-no compared (one head pair each here: the
    /// groups are single heads), offered or left out as it says, its
    /// verdicts kept; "not the same" keeps a pair out, through a regroup,
    /// and Ctrl+Z brings it back; a merge is the existing merge, one undo
    /// step; nothing merges by itself.
    #[test]
    fn merge_suggestions_by_distance_by_the_judge_and_not_the_same() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0; B 0.699 from it (apart from A: over the group cut 0.69,
        // under the suggest cut 0.711); C 0.758 from A (the judge's band);
        // E far from all.
        let specs = [(1, 0), (2, Suits::at(0.699)), (3, -Suits::at(0.758)), (4, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("merges", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        let fake = Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::Alternate));
        let f2 = fake.clone();
        e.set_judge_factory(Box::new(move |_| f2.clone() as Arc<dyn crate::ml::judge_client::Judge>));
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let groups = fursuit_groups(&e, 4);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let (a, b, c) = (id(1), id(2), id(3));
        let pair_keys = |l: &[super::super::merging::GroupPair]| -> Vec<(GroupId, GroupId)> { l.iter().map(|p| p.key()).collect() };
        let k = |x: &GroupId, y: &GroupId| super::super::merging::order(x.clone(), y.clone());
        // The judge off: the suggest cut alone.
        e.merge_suggestions(1, None, 1, false);
        let (list, judged) = merges(&mut ev, 1, Duration::from_secs(2));
        let list = list.expect("suggestions");
        assert_eq!(pair_keys(&list), [k(&a, &b)]);
        assert!((list[0].distance - 0.699).abs() < 0.002, "average linkage in the step's space: {}", list[0].distance);
        assert!(list[0].sure && judged.is_empty());
        assert_eq!(list[0].a_heads[0].face.key, if list[0].a == a { key_of(1) } else { key_of(2) }, "each side shows its cover first");
        // A group's own pairs; a group of fewer photos than asked is left out.
        e.merge_suggestions(2, Some(c.clone()), 1, false);
        assert!(merges(&mut ev, 2, Duration::from_secs(2)).0.unwrap().is_empty(), "C is in no pair under the suggest cut");
        e.merge_suggestions(3, None, 2, false);
        assert!(merges(&mut ev, 3, Duration::from_secs(2)).0.unwrap().is_empty(), "single-photo groups unnamed: folded away, not offered");
        // The judge on: every pair up to sure-no is compared, nearest first.
        e.set_judge(JudgeChoice::Gemma4);
        wait_for(&e, "the choice", |s| s.judge.active(Device::Gpu));
        e.merge_suggestions(4, None, 1, true);
        let (list, judged) = merges(&mut ev, 4, Duration::from_secs(5));
        assert!(list.expect("suggestions").is_empty(), "nothing under sure-yes: all compared");
        assert_eq!((judged[0].2, judged[0].3), (0, 2));
        let offered: Vec<_> = judged.iter().flat_map(|j| pair_keys(&j.0)).collect();
        let left: Vec<_> = judged.iter().flat_map(|j| pair_keys(&j.1)).collect();
        assert_eq!((offered, left), (vec![k(&a, &b)], vec![k(&a, &c)]), "nearest first: yes, then no");
        let v = judged.iter().flat_map(|j| j.0.iter()).next().unwrap().verdict.unwrap();
        assert_eq!((v.yes, v.no, v.unjudged), (1, 0, 0), "single heads: one head pair asked");
        assert_eq!(fake.pairs.lock().len(), 2);
        assert!(matches!(&fake.pairs.lock()[1].a, crate::ml::judge::Crop::Head { .. }), "head crops");
        // Asked again: the verdicts kept answer at once.
        e.merge_suggestions(5, None, 1, true);
        let (list, judged) = merges(&mut ev, 5, Duration::from_secs(2));
        assert_eq!(pair_keys(&list.unwrap()), [k(&a, &b)]);
        assert_eq!((pair_keys(&judged[0].1), judged[0].2, judged[0].3), (vec![k(&a, &c)], 2, 2));
        assert_eq!(fake.pairs.lock().len(), 2, "nothing compared twice");
        assert_eq!(e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).count(), 4, "the judge merges nothing");
        e.set_judge(JudgeChoice::Off);
        // Not the same: kept out, through a regroup; Ctrl+Z brings it back.
        e.not_same(a.clone(), b.clone(), vec![], vec![], Via::default());
        e.merge_suggestions(6, None, 1, false);
        assert!(merges(&mut ev, 6, Duration::from_secs(2)).0.unwrap().is_empty());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        let _ = fursuit_groups(&e, 4);
        std::thread::sleep(Duration::from_millis(200));
        e.merge_suggestions(7, None, 1, false);
        assert!(merges(&mut ev, 7, Duration::from_secs(2)).0.unwrap().is_empty(), "not offered again after a regroup");
        e.undo();
        std::thread::sleep(Duration::from_millis(200));
        e.merge_suggestions(8, None, 1, false);
        assert_eq!(pair_keys(&merges(&mut ev, 8, Duration::from_secs(2)).0.unwrap()), [k(&a, &b)], "taken back: offered again");
        // A merge (the user's) is the existing merge: one group fewer, one
        // undo step; the pair is gone.
        e.merge(a.clone(), vec![b.clone()], Via::default());
        let _ = fursuit_groups(&e, 3);
        e.merge_suggestions(9, None, 1, false);
        assert!(!pair_keys(&merges(&mut ev, 9, Duration::from_secs(2)).0.unwrap()).contains(&k(&a, &b)));
        e.undo();
        let _ = fursuit_groups(&e, 4);
        // Kinds never mix: a person's id is no fursuit group.
        e.merge_suggestions(10, Some(GroupId::Subject(SubjectId("0".repeat(32)))), 1, false);
        assert!(merges(&mut ev, 10, Duration::from_secs(2)).0.unwrap().is_empty());
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// X in the reviews through the engine: the heads go to the removed
    /// list (never offered again, in the review or the merge suggestions),
    /// one undo step, recorded as `remove` (one head) or `remove_many`
    /// (a burst's frames) made `at` the review; the review's take back
    /// restores them and records an undo naming it; X in the pair review
    /// is taken back by the ordinary undo; a box that isn't a detection of
    /// the kind is refused, nothing changed, nothing recorded.
    #[test]
    fn x_in_the_reviews_removes_heads_records_them_and_takes_back() {
        use crate::model::decisions::{Act, At, Entry};
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0; B 0.699 from it (a suggestion for A); C and D far.
        let specs = [(1, 0), (2, Suits::at(0.699)), (3, 1800), (4, -1000)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("review-x", &refs);
        let user = d.join("user").join("subjects.json");
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(user.clone()), downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
        let groups = fursuit_groups(&e, 4);
        let a = groups.iter().find(|g| g.1 == key_of(1)).unwrap().0.clone();
        e.name_group(a, "Suit".into());
        let started = Instant::now();
        let suit = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit")) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        let entries = |n: usize| -> Vec<Entry> {
            let started = Instant::now();
            loop {
                let got = e.decisions().entries();
                if got.len() >= n {
                    return got;
                }
                assert!(started.elapsed() < Duration::from_secs(5), "{} of {n}: {got:?}", got.len());
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let offered = |ev: &mut Events, seq: u64| -> Vec<FaceRef> {
            e.suggestions(seq, suit.clone());
            review(ev, seq, Duration::from_secs(2)).0.unwrap_or_default().iter().flatten().map(|f| f.face).collect()
        };
        assert_eq!(offered(&mut ev, 1), [head(2)], "B is suggested");
        let base = e.decisions().entries().len();
        // A box of another kind (no face is there): refused, nothing kept.
        e.not_heads(Kind::Person, vec![head(2)], At::Review);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(e.decisions().entries().len(), base);
        assert_eq!(offered(&mut ev, 2), [head(2)]);
        // X on B in the review: gone, recorded as `remove` at the review.
        e.not_heads(Kind::Fursuit, vec![head(2)], At::Review);
        let got = entries(base + 1);
        let x = got.last().unwrap().clone();
        assert!(matches!(&x.act, Act::Remove { head: h } if h.0 == head(2)), "{x:?}");
        assert_eq!(x.how.as_ref().unwrap().at, Some(At::Review));
        assert_eq!(serde_json::to_value(&x).unwrap()["how"]["at"], "review");
        assert!(offered(&mut ev, 3).is_empty(), "never offered again");
        let _ = fursuit_groups(&e, 3);
        e.merge_suggestions(4, None, 1, false);
        assert!(merges(&mut ev, 4, Duration::from_secs(2)).0.unwrap().is_empty(), "nor in the merge suggestions");
        // Z in the review: B back, an undo naming the X.
        e.restore_heads(Kind::Fursuit, vec![head(2)]);
        let got = entries(base + 2);
        assert!(matches!(&got.last().unwrap().act, Act::Undo { undoes, of } if *undoes == x.id && of == "remove"), "{:?}", got.last());
        assert_eq!(offered(&mut ev, 5), [head(2)], "offered again");
        // A burst's frames: one `remove_many` entry, one step.
        e.not_heads(Kind::Fursuit, vec![head(3), head(4)], At::Review);
        let got = entries(base + 3);
        assert!(matches!(&got.last().unwrap().act, Act::RemoveMany { heads, removed: 2, .. } if heads.len() == 2), "{:?}", got.last());
        let _ = fursuit_groups(&e, 2);
        e.restore_heads(Kind::Fursuit, vec![head(3), head(4)]);
        let _ = fursuit_groups(&e, 4);
        // X in the pair review: `merge-review`; the ordinary undo takes it
        // back.
        e.not_heads(Kind::Fursuit, vec![head(2)], At::MergeReview);
        let got = entries(base + 5);
        let x = got.last().unwrap().clone();
        assert_eq!(serde_json::to_value(&x).unwrap()["how"]["at"], "merge-review");
        let _ = fursuit_groups(&e, 3);
        e.undo();
        let got = entries(base + 6);
        assert!(matches!(&got.last().unwrap().act, Act::Undo { undoes, .. } if *undoes == x.id), "{:?}", got.last());
        let _ = fursuit_groups(&e, 4);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// X on an unnamed group's cover in its check, with the regroup and the
    /// snapshot landing before Z: the group's id changes (it follows its
    /// clearest head), the review's page follows it by the heads it knew
    /// (`subjects_person::follow_cluster`), and Z still restores the head
    /// and records the undo; the group comes back under its old id, and is
    /// followed there too.
    #[test]
    fn x_on_a_groups_cover_then_a_regroup_then_z_still_restores() {
        use crate::model::decisions::{Act, At};
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.03)), (3, -Suits::at(0.03)), (4, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("review-x-cover", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, _rx) = crate::bus::channel();
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(d.join("user").join("subjects.json")), downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
        let known: std::collections::HashSet<FaceRef> = (1..=3).map(head).collect();
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let groups = fursuit_groups(&e, 2);
        let (old, cover) = groups.iter().find(|g| g.1 != key_of(4)).cloned().unwrap();
        let cover = (1..=3).map(head).find(|h| h.key == cover).unwrap();
        // X on the cover; the regroup and its snapshot land.
        e.not_heads(Kind::Fursuit, vec![cover], At::Review);
        let started = Instant::now();
        let moved = loop {
            let s = e.subjects();
            if s.group(&old).is_none() && s.groups.iter().filter(|g| g.kind == Kind::Fursuit).count() == 2 {
                break s;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", s.groups);
            std::thread::sleep(Duration::from_millis(20));
        };
        let now = crate::app::subjects_person::follow_cluster(&moved, Kind::Fursuit, &known).expect("followed to its new id");
        let g = moved.group(&now).unwrap();
        assert!(g.cover.face != cover && known.contains(&g.cover.face) && g.photos == 2, "{g:?}");
        assert!(crate::app::subjects_person::follow_cluster(&moved, Kind::Person, &known).is_none(), "never another kind's");
        // Z: the head is back, the undo recorded, the group back as it was.
        e.restore_heads(Kind::Fursuit, vec![cover]);
        let started = Instant::now();
        let back = loop {
            let s = e.subjects();
            if s.group(&old).is_some_and(|g| g.photos == 3) {
                break s;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", s.groups);
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(crate::app::subjects_person::follow_cluster(&back, Kind::Fursuit, &known), Some(old));
        let started = Instant::now();
        loop {
            let got = e.decisions().entries();
            if let [.., x, z] = got.as_slice() {
                if matches!(&z.act, Act::Undo { undoes, of } if *undoes == x.id && of == "remove") {
                    break;
                }
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{got:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        e.shutdown();
        let file = crate::model::subjects::SubjectsFile::load(&d.join("user").join("subjects.json"));
        assert!(!file.removed_index(Kind::Fursuit).covers(&cover), "not left removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The decision record through the engine: each decision (a merge,
    /// its undo, "not the same", a split, a review answer and its taking
    /// back, label edits) appends one self-contained entry, measured
    /// before the change, with the heads by key and box, how it was
    /// reached and the library's state; Clear and Rebuild keep it; the
    /// switch off writes nothing; *Forget all fursuits* takes their
    /// entries out.  The file is beside the subjects file, 0600.
    #[test]
    fn every_decision_is_recorded_and_the_record_survives_clear_and_rebuild() {
        use crate::model::decisions::{Act, At, Entry, Via};
        use crate::search::merging::GroupVerdict;
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0; B 0.699 from it; C 0.758 from A; E far from all.
        let specs = [(1, 0), (2, Suits::at(0.699)), (3, -Suits::at(0.758)), (4, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("decisions", &refs);
        let user = d.join("user").join("subjects.json");
        let record = d.join("user").join(crate::model::decisions::FILE_NAME);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, _rx) = crate::bus::channel();
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(user.clone()), downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        assert_eq!(e.decisions().path(), Some(record.as_path()), "beside the subjects file");
        // Not the default choice, so the entries show the one in force.
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
        let groups = fursuit_groups(&e, 4);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let (a, b) = (id(1), id(2));
        let until = |n: usize| -> Vec<Entry> {
            let started = Instant::now();
            loop {
                let got = e.decisions().entries();
                if got.len() >= n {
                    return got;
                }
                assert!(started.elapsed() < Duration::from_secs(5), "{} of {n} entries: {got:?}", got.len());
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // Every entry stands alone: its kind, the library, how.
        let alone = |x: &Entry| {
            assert_eq!(x.kind, Kind::Fursuit);
            let l = x.lib.as_ref().expect("the library's state");
            assert!(l.white && l.heads == 4 && l.model == heads::VECTOR_MODEL.dir_name(), "{l:?}");
            let g = heads::GroupingCuts::of(heads::Grouping::Tighter, true);
            assert_eq!((l.grouping, l.cuts.group, l.cuts.suggest, l.cuts.merge_suggest, l.cuts.merge_far), (Some(g.grouping), g.group, heads::CUTOFFS.suggest, Some(g.merge_suggest), Some(g.merge_far)), "the setting and cut-offs in force");
            assert!(x.how.is_some());
        };
        // A merge offered by distance: both sides by their heads, the
        // other's distance measured in the step's space.
        e.merge(a.clone(), vec![b.clone()], Via::Pair { at: At::MergeReview, distance: 0.6991, sure: true, verdict: None });
        let got = until(1);
        let merge = &got[0];
        alone(merge);
        let Act::Merge { groups } = &merge.act else { panic!("{merge:?}") };
        assert_eq!((groups[0].heads[0].0, groups[1].heads[0].0, groups[0].n, groups[1].n), (head(1), head(2), 1, 1), "the kept group first");
        assert!((groups[1].d.unwrap() - 0.699).abs() < 0.002, "{:?}", groups[1].d);
        let how = merge.how.as_ref().unwrap();
        assert_eq!((how.by.as_str(), how.at, how.d, how.sure), ("distance", Some(At::MergeReview), Some(0.6991), Some(true)));
        // Undo appends, naming it.
        let _ = fursuit_groups(&e, 3);
        e.undo();
        let got = until(2);
        assert!(matches!(&got[1].act, Act::Undo { undoes, of } if *undoes == merge.id && of == "merge"), "{:?}", got[1]);
        let groups = fursuit_groups(&e, 4);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let (a, c, far) = (id(1), id(3), id(4));
        // "Not the same", the judge's majority against.
        e.not_same(a.clone(), c.clone(), vec![head(1)], vec![head(3)], Via::Pair { at: At::MergeReview, distance: 0.758, sure: false, verdict: Some(GroupVerdict { yes: 0, no: 2, unjudged: 1 }) });
        let got = until(3);
        alone(&got[2]);
        let how = got[2].how.as_ref().unwrap();
        assert_eq!((how.by.as_str(), how.yes, how.no, how.unseen), ("judge", Some(0), Some(2), Some(1)));
        assert!(matches!(&got[2].act, Act::NotSame { groups } if groups[0].heads[0].0 == head(1) && groups[1].heads[0].0 == head(3)));
        // A split proposed around two heads of one photo, applied from the
        // editor after two heads were moved by hand: one `edit` entry.
        let _ = far;
        let edit = crate::model::subjects::GroupEdit { kind: Kind::Fursuit, new_names: vec![None], moves: vec![(head(4), crate::model::subjects::EditDest::New(0))], ..Default::default() };
        e.apply_edit(9, edit, Via::Editor { proposal: true, edited: 2 });
        let got = until(4);
        alone(&got[3]);
        assert!(matches!(&got[3].act, Act::Edit { groups, out, .. } if groups[0].n == 1 && out[0].from == Some(0) && matches!(out[0].to, crate::model::decisions::To::New { new: 0, .. }) && out[0].heads[0].0 == head(4)), "{:?}", got[3]);
        let how = got[3].how.as_ref().unwrap();
        assert_eq!((how.by.as_str(), how.at, how.edited), ("proposal", Some(At::Editor), Some(2)));
        // A review answer: C is A, by distance; the nearest member measured.
        e.answer(a.clone(), vec![head(3)], Some(true), Via::Review { judge: None, check: false });
        let got = until(5);
        alone(&got[4]);
        assert!(matches!(&got[4].act, Act::Answer { yes: true, group, heads } if group.heads[0].0 == head(1) && heads[0].0 == head(3)));
        let how = got[4].how.as_ref().unwrap();
        assert_eq!((how.by.as_str(), how.pair.map(|p| (p[0].0, p[1].0))), ("distance", Some((head(3), head(1)))));
        assert!((how.d.unwrap() - 0.758).abs() < 0.002, "{how:?}");
        // Taken back in the review: an undo naming the answer.
        let answered = got[4].id.clone();
        let named = e.subjects().groups.iter().find(|g| g.kind == Kind::Fursuit && matches!(g.id, GroupId::Subject(_))).map(|g| g.id.clone()).unwrap();
        e.answer(named, vec![head(3)], None, Via::Review { judge: None, check: false });
        let got = until(6);
        assert!(matches!(&got[5].act, Act::Undo { undoes, of } if *undoes == answered && of == "answer"), "{:?}", got[5]);
        // Label edits in the viewer: a name, a removal.
        e.label(LabelEdit::Name { face: head(2), kind: Kind::Fursuit, to: crate::model::subjects::NameTo::New("Suit".into()) });
        e.label(LabelEdit::Remove { face: head(4), kind: Kind::Fursuit });
        let got = until(8);
        alone(&got[6]);
        assert!(matches!(&got[6].act, Act::Name { head: Some(h), name: Some(n), .. } if h.0 == head(2) && n == "Suit"), "{:?}", got[6]);
        assert!(matches!(&got[7].act, Act::Remove { head: h } if h.0 == head(4)));
        assert_eq!(got[7].how.as_ref().unwrap().at, Some(At::Viewer));
        // Export: the log and the named groups now, by codes.
        let out = d.join("export");
        std::fs::create_dir_all(&out).unwrap();
        let (dir, n) = e.export_record(&out).unwrap();
        assert_eq!(n, 8);
        let (entries, groups) = crate::model::group_export::read_export(&dir).unwrap();
        let groups = groups.expect("groups.json");
        assert_eq!(entries.len(), 8);
        let suit = groups.groups.iter().find(|g| g.heads.iter().any(|h| h.0 == head(2))).expect("the named head's group");
        assert_eq!((suit.kind, suit.photos), (Kind::Fursuit, Some(1)));
        assert!(matches!(&entries[6].act, Act::Name { name: Some(n), .. } if *n == suit.code), "one code in both files");
        assert!(groups.removed.iter().any(|b| b.kind == Kind::Fursuit && b.head.0 == head(4)), "the viewer's removal");
        assert_eq!(groups.kinds.iter().find(|k| k.kind == Kind::Fursuit).and_then(|k| k.lib.grouping), Some(heads::Grouping::Tighter));
        let text = std::fs::read_to_string(dir.join(crate::model::group_export::FILE_NAME)).unwrap() + &std::fs::read_to_string(dir.join(crate::model::decisions::FILE_NAME)).unwrap();
        assert!(!text.contains("Suit"), "no names");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&record).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Clear and Rebuild keep the record, byte for byte.
        let before = std::fs::read(&record).unwrap();
        e.clear_index();
        wait_for(&e, "cleared", |s| s.indexed_shots == 0);
        e.rebuild_index();
        wait_for(&e, "rebuilt", |s| s.indexed_shots == 4 && s.pass.is_none());
        e.flush_decisions(Duration::from_secs(2));
        assert_eq!(std::fs::read(&record).unwrap(), before, "Clear and Rebuild keep the record");
        // The switch off: nothing is written.
        e.decisions().set_on(false);
        e.label(LabelEdit::Remove { face: head(1), kind: Kind::Fursuit });
        e.undo();
        e.flush_decisions(Duration::from_secs(2));
        assert_eq!(std::fs::read(&record).unwrap(), before, "off writes nothing");
        // ... but taking back a correction the record holds still says so.
        e.decisions().set_on(true);
        e.label(LabelEdit::Remove { face: head(3), kind: Kind::Fursuit });
        let got = until(9);
        let removed = got[8].id.clone();
        e.decisions().set_on(false);
        e.undo();
        let got = until(10);
        assert!(matches!(&got[9].act, Act::Undo { undoes, of } if *undoes == removed && of == "remove"), "{:?}", got[9]);
        e.label(LabelEdit::Remove { face: head(3), kind: Kind::Fursuit });
        e.flush_decisions(Duration::from_secs(2));
        assert_eq!(e.decisions().entries().len(), 10, "nothing new while off");
        e.decisions().set_on(true);
        // A group photo chosen: one `cover` entry, and the tile shows it
        // without a regroup.
        let suit = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit")).map(|g| g.id.clone()).expect("the named head's group");
        let GroupId::Subject(sid) = suit.clone() else { panic!("a named group is a subject") };
        e.set_cover(sid, Some(head(2)), At::Heads);
        let got = until(11);
        assert!(got[10].kind == Kind::Fursuit && got[10].lib.is_some());
        assert!(matches!(&got[10].act, Act::Cover { group, head: Some(h) } if group.name.as_deref() == Some("Suit") && h.0 == head(2)), "{:?}", got[10]);
        assert_eq!(got[10].how.as_ref().unwrap().at, Some(At::Heads));
        let started = Instant::now();
        while e.subjects().chosen_cover(&suit) != Some(head(2)) {
            assert!(started.elapsed() < Duration::from_secs(5), "the snapshot shows the chosen head");
            std::thread::sleep(Duration::from_millis(10));
        }
        // "Not fursuit heads" on a group: one entry, the group as it was
        // and the heads removed.
        e.remove_boxes(Kind::Fursuit, vec![(suit, None)]);
        let got = until(12);
        // After the rebuild the heads are raw again (too few to whiten):
        // the entry says so, with the raw cuts of the same choice.
        let l = got[11].lib.as_ref().unwrap();
        let g = heads::GroupingCuts::of(heads::Grouping::Tighter, false);
        assert_eq!((l.white, l.grouping, l.cuts.group, l.cuts.merge_suggest), (false, Some(g.grouping), g.group, Some(g.merge_suggest)), "{l:?}");
        assert!(matches!(&got[11].act, Act::RemoveMany { groups, heads, removed: 1 } if groups[0].name.as_deref() == Some("Suit") && heads[0].0 == head(2)), "{:?}", got[11]);
        // Forget all fursuits: their entries go (here, all of them).
        e.forget_kind(Kind::Fursuit);
        e.flush_decisions(Duration::from_secs(2));
        e.decisions().flush();
        assert!(!record.exists(), "the fursuit entries are forgotten");
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A head removed in the viewer ("Not a fursuit head") is in no merge
    /// suggestion: not as an automatic group, and not as a subject that
    /// still lists it (the removal recorded a thousandth off, as a
    /// rebuild finds a box again).  Taken back, the pair comes back.
    #[test]
    fn a_removed_head_is_never_in_a_merge_suggestion() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0; B 0.699 from it: one pair under the suggest cut.
        let specs = [(1, 0), (2, Suits::at(0.699)), (3, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("merges-removed", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 3 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let groups = fursuit_groups(&e, 3);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let pair_keys = |l: &[super::super::merging::GroupPair]| -> Vec<(GroupId, GroupId)> { l.iter().map(|p| p.key()).collect() };
        e.merge_suggestions(1, None, 1, false);
        assert_eq!(pair_keys(&merges(&mut ev, 1, Duration::from_secs(2)).0.unwrap()), [super::super::merging::order(id(1), id(2))]);
        // B's group named: a subject that lists B's head.
        e.name_group(id(2), "Other".into());
        let started = Instant::now();
        while !e.subjects().groups.iter().any(|g| g.name.as_deref() == Some("Other")) {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        }
        // Removed a thousandth off: the subject still lists the exact box.
        let b_off = FaceRef::new(key_of(2), [0.301, 0.2, 0.3, 0.4]);
        e.label(LabelEdit::Remove { face: b_off, kind: Kind::Fursuit });
        std::thread::sleep(Duration::from_millis(300));
        e.merge_suggestions(2, None, 1, false);
        assert!(merges(&mut ev, 2, Duration::from_secs(2)).0.unwrap().is_empty(), "the removed head is in no pair");
        // The other way round: A removed exactly (its automatic group goes).
        e.undo();
        std::thread::sleep(Duration::from_millis(300));
        e.merge_suggestions(3, None, 1, false);
        assert_eq!(merges(&mut ev, 3, Duration::from_secs(2)).0.unwrap().len(), 1, "taken back: the pair again");
        e.label(LabelEdit::Remove { face: FaceRef::new(key_of(1), [0.3, 0.2, 0.3, 0.4]), kind: Kind::Fursuit });
        let _ = fursuit_groups(&e, 2);
        e.merge_suggestions(4, None, 1, false);
        assert!(merges(&mut ev, 4, Duration::from_secs(2)).0.unwrap().is_empty());
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Not fursuit heads" on a whole group through the engine: the group
    /// goes from the tab, its heads from the merge suggestions, in one
    /// undo step; a person's group among them is refused and changes
    /// nothing; a rebuild (the heads found again) never brings them back.
    #[test]
    fn a_whole_group_removed_is_gone_through_a_rebuild_and_one_undo() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.699)), (3, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("remove-group", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        // Written for the tight grouping: three suits, three groups.
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 3 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let groups = fursuit_groups(&e, 3);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        // Mixed with a person's group: refused, nothing changes.
        let person = GroupId::Subject(SubjectId("0".repeat(32)));
        e.remove_boxes(Kind::Fursuit, vec![(id(2), None), (person, None)]);
        std::thread::sleep(Duration::from_millis(200));
        let _ = fursuit_groups(&e, 3);
        // B's whole group: gone, and with it A and B's pair.
        e.remove_boxes(Kind::Fursuit, vec![(id(2), None)]);
        let _ = fursuit_groups(&e, 2);
        e.merge_suggestions(1, None, 1, false);
        assert!(merges(&mut ev, 1, Duration::from_secs(2)).0.unwrap().is_empty());
        assert_eq!(e.subjects().undoable.as_deref(), Some("Removed a fursuit head"));
        // A rebuild finds the head again: still removed.
        e.rebuild_index();
        assert!(ev.pass_starts().contains(&PassKind::Rebuild), "the rebuild ran");
        wait_for(&e, "the rebuild and its heads", |s| s.indexed_shots == 3 && s.pass.is_none() && s.heads.found == 3 && s.heads.step == heads::Step::Idle);
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        std::thread::sleep(Duration::from_millis(300));
        let after = fursuit_groups(&e, 2);
        assert!(after.iter().all(|g| g.1 != key_of(2)), "the removed head grouped nowhere: {after:?}");
        // One undo brings the group back.
        e.undo();
        let _ = fursuit_groups(&e, 3);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// With the judge on, a pair under sure-yes is offered at once only
    /// when both covers are clear enough (`judging::CLARITY_MIN`): a dim,
    /// doubtful head is shown to the judge however close (the vectors
    /// pull bad heads together).
    #[test]
    fn a_sure_pair_goes_to_the_judge_when_a_cover_is_unclear() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}{}", if n == 3 { "dim-" } else { "" }, if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0; B 0.5 from it (clear); C 0.5 from it the other way (dim).
        let specs = [(1, 0), (2, Suits::at(0.5)), (3, -Suits::at(0.5))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("merges-dim", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        let fake = Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::Yes));
        let f2 = fake.clone();
        e.set_judge_factory(Box::new(move |_| f2.clone() as Arc<dyn crate::ml::judge_client::Judge>));
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 3 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        // A and B group together; C alone.  Name A's group, then move B out
        // to a group of its own.
        let groups = fursuit_groups(&e, 2);
        let ab = groups.iter().find(|g| g.1 != key_of(3)).unwrap().0.clone();
        e.name_group(ab, "Suit".into());
        let started = Instant::now();
        let suit = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit")) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        let b_ref = FaceRef::new(key_of(2), [0.3, 0.2, 0.3, 0.4]);
        let _ = &suit;
        let edit = crate::model::subjects::GroupEdit { kind: Kind::Fursuit, new_names: vec![None], moves: vec![(b_ref, crate::model::subjects::EditDest::New(0))], ..Default::default() };
        e.apply_edit(9, edit, Via::default());
        // Three groups, B's of its own: a listing from before the move
        // landed can hold three groups too, so wait for B's.
        let started = Instant::now();
        let groups = loop {
            let g = fursuit_groups(&e, 3);
            if [2, 3].iter().all(|&n| g.iter().any(|x| x.1 == key_of(n))) {
                break g;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{g:?}");
            std::thread::sleep(Duration::from_millis(20));
        };
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let k = |x: &GroupId, y: &GroupId| super::super::merging::order(x.clone(), y.clone());
        e.set_judge(JudgeChoice::Gemma4);
        wait_for(&e, "the choice", |s| s.judge.active(Device::Gpu));
        e.merge_suggestions(1, None, 1, true);
        let (list, judged) = merges(&mut ev, 1, Duration::from_secs(5));
        assert_eq!(list.unwrap().iter().map(|p| (p.key(), p.sure)).collect::<Vec<_>>(), [(k(&suit, &id(2)), true)], "clear and under sure-yes: at once");
        let offered: Vec<_> = judged.iter().flat_map(|j| j.0.iter().map(|p| p.key())).collect();
        assert_eq!(offered, [k(&suit, &id(3))], "the dim one was compared, then offered");
        assert_eq!(fake.pairs.lock().len(), 1);
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// [`Suits`], with a second head on photos named `-two<tenths>-`: at
    /// that angle, beside the first.
    struct TwoHeads;

    impl Embedder for TwoHeads {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            Suits.device_report()
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
            Suits.embed_images(files, want)
        }
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Suits.encode_text(texts)
        }
        fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<crate::ml::heads::HeadsResult>, HelperError> {
            let mut out = Suits.find_heads(files)?;
            for r in &mut out {
                let stem = crate::util::stem(&r.path);
                let Some(two) = stem.split('-').find_map(|w| w.strip_prefix("two")).and_then(|d| d.trim_start_matches('m').parse::<f32>().ok()) else { continue };
                let sign = if stem.contains("-twom") { -1.0 } else { 1.0 };
                let a = (sign * two / 10.0).to_radians();
                if let Ok(ph) = &mut r.result {
                    ph.heads.push(crate::ml::heads::Head { bbox: [0.65, 0.2, 0.25, 0.35], score: 0.5, size: 300, sharpness: 0.2, vector: vec![a.cos(), a.sin()], tile: Vec::new() });
                }
            }
            Ok(out)
        }
    }

    /// The split check through the engine (stage 3): a group holding two
    /// heads of one photo is flagged (fursuits only), its heads come
    /// farthest from the rest first, a split is proposed around the two,
    /// and "Check this group" goes farthest first.  A head the viewer's
    /// labels removed is in none of it.  Nothing moves.
    #[test]
    fn a_group_with_two_heads_of_one_photo_is_flagged_and_ordered_least_alike_first() {
        // Heads at 0, ±0.05 and 0.1 (one suit), and on photo 4 a second
        // head 0.3 the other way: all one group by average linkage.
        let name = |n: u32, a: i32, two: Option<i32>| format!("{n:04}-fursuit-90-{}{}{}", two.map_or(String::new(), |t| format!("two{}{}-", if t < 0 { "m" } else { "" }, t.abs())), if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0, None), (2, Suits::at(0.05), None), (3, -Suits::at(0.05), None), (4, Suits::at(0.1), Some(-Suits::at(0.3)))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a, two)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a, two)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("split", &refs);
        let factory: EmbedderFactory = Box::new(|_| Ok(Arc::new(TwoHeads) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 5 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let key4 = Key::of(1, lib_path(&d, &files[3]).as_os_str().as_encoded_bytes());
        let second = FaceRef::new(key4, [0.65, 0.2, 0.25, 0.35]);
        let first4 = FaceRef::new(key4, [0.3, 0.2, 0.3, 0.4]);
        let groups = fursuit_groups(&e, 1);
        let g = groups[0].0.clone();
        let started = Instant::now();
        while !e.subjects().seen_together.contains(&g) {
            assert!(started.elapsed() < Duration::from_secs(5), "the flag: {:?}", e.subjects().seen_together);
            std::thread::sleep(Duration::from_millis(20));
        }
        let check = |ev: &mut Events, seq: u64| -> (Vec<(FaceRef, f32)>, Vec<(FaceRef, FaceRef)>, Option<(Vec<FaceRef>, Vec<FaceRef>)>) {
            let stream = &mut ev.stream;
            ev.rt.block_on(async {
                use iced::futures::StreamExt;
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await else { continue };
                    for e in batch {
                        if let BgEvent::GroupCheck { seq: s, far, pairs, split, .. } = e {
                            if s == seq {
                                return ((*far).clone(), (*pairs).clone(), split.map(|x| (*x).clone()));
                            }
                        }
                    }
                }
                panic!("no check");
            })
        };
        e.group_check(1, g.clone());
        let (far, pairs, split) = check(&mut ev, 1);
        assert_eq!(far.len(), 5);
        assert_eq!(far[0].0, second, "the other suit's head comes first");
        assert!(far.windows(2).all(|w| w[0].1 >= w[1].1));
        assert_eq!(pairs.len(), 1);
        let (with_first, with_second) = split.expect("a proposal");
        assert!(with_first.contains(&first4) && with_second.contains(&second) && !with_second.contains(&first4), "{with_first:?} / {with_second:?}");
        assert_eq!(with_first.len() + with_second.len(), 5, "every head on one side");
        assert_eq!(e.subjects().groups.iter().find(|x| x.id == g).map(|x| x.photos), Some(4), "nothing moved");
        // "Check this group": farthest first.
        e.suggestions(2, g.clone());
        let (list, _) = review(&mut ev, 2, Duration::from_secs(2));
        assert_eq!(list.expect("the group's own heads")[0][0].face, second, "the stray first");
        // A person's group has no split check.
        e.group_check(3, GroupId::Subject(SubjectId("0".repeat(32))));
        assert!(check(&mut ev, 3).0.is_empty());
        // The viewer's label removes the second head: out of everything.
        e.label(crate::model::subjects::LabelEdit::Remove { face: second, kind: Kind::Fursuit });
        let started = Instant::now();
        while e.subjects().seen_together.contains(&g) || e.subjects().groups.iter().filter(|x| x.kind == Kind::Fursuit).count() != 1 {
            assert!(started.elapsed() < Duration::from_secs(5), "the flag stays: {:?}", e.subjects().seen_together);
            std::thread::sleep(Duration::from_millis(20));
        }
        let g = fursuit_groups(&e, 1)[0].0.clone();
        e.group_check(4, g);
        let (far, pairs, split) = check(&mut ev, 4);
        assert!(far.iter().all(|(f, _)| *f != second) && pairs.is_empty() && split.is_none());
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Preferences ▸ Fursuits ▸ *Grouping* through the engine: the cuts
    /// follow the choice in the step's space; a change regroups (two heads
    /// 0.70 apart are two groups Tighter, one Balanced) and the user's
    /// names stay; the review's per-head cut-offs don't move.
    #[test]
    fn the_grouping_choice_regroups_and_keeps_names() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        // A at 0 and B 0.70 from it; C far, named later.
        let specs = [(1, 0), (2, Suits::at(0.70)), (3, -1700), (4, -1710)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("grouping", &refs);
        let config = Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        assert_eq!(e.status().heads.grouping, heads::Grouping::Balanced, "Balanced by default");
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        e.set_whitening(super::super::whiten::Whitening::identity(2));
        wait_for(&e, "the whitening", |s| s.heads.whitened);
        let c = e.status().heads.cuts();
        assert_eq!((c.grouping, c.whitened, c.group, c.merge_suggest), (heads::Grouping::Tighter, true, 0.69, 0.711));
        let _ = fursuit_groups(&e, 3);
        // C and its neighbour named.
        let key3 = Key::of(1, lib_path(&d, &files[2]).as_os_str().as_encoded_bytes());
        let c_group = fursuit_groups(&e, 3).into_iter().find(|g| g.1 == key3 || g.1 == Key::of(1, lib_path(&d, &files[3]).as_os_str().as_encoded_bytes())).unwrap().0;
        e.name_group(c_group, "Suit".into());
        wait_for(&e, "named", |_| e.subjects().groups.iter().any(|g| g.name.as_deref() == Some("Suit")));
        // Balanced: A and B are one group now; the name stays.
        e.set_fursuit_grouping(heads::Grouping::Balanced);
        wait_for(&e, "regrouped", |s| s.heads.grouping == heads::Grouping::Balanced && !s.heads.regrouping);
        let groups = fursuit_groups(&e, 2);
        assert!(e.subjects().groups.iter().any(|g| g.name.as_deref() == Some("Suit") && g.photos == 2), "the name and its heads stay: {groups:?}");
        assert_eq!(e.status().heads.cuts().group, 0.72);
        // Tighter again: two groups again.
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        wait_for(&e, "regrouped", |s| s.heads.grouping == heads::Grouping::Tighter && !s.heads.regrouping);
        let _ = fursuit_groups(&e, 3);
        assert!(e.subjects().groups.iter().any(|g| g.name.as_deref() == Some("Suit")));
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Below `whiten::MIN_HEADS` heads the step groups the raw vectors
    /// with their own cut-offs; the suggestions use the same space and
    /// cuts (suggest 0.097; the judge's band 0.082 to 0.124).
    #[test]
    fn merge_suggestions_use_the_raw_cut_offs_below_the_whitening() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.0955)), (3, -Suits::at(0.11)), (4, 1800)];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("merges-raw", &refs);
        let suits: EmbedderFactory = Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>));
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let config = Config { factory: suits, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.set_fursuit_grouping(heads::Grouping::Tighter);
        let fake = Arc::new(crate::ml::judge_client::Fake::new(crate::ml::judge_client::FakeMode::Yes));
        let f2 = fake.clone();
        e.set_judge_factory(Box::new(move |_| f2.clone() as Arc<dyn crate::ml::judge_client::Judge>));
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        assert!(!e.status().heads.whitened);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let groups = fursuit_groups(&e, 4);
        let id = |n: usize| groups.iter().find(|g| g.1 == key_of(n)).unwrap().0.clone();
        let k = |x: &GroupId, y: &GroupId| super::super::merging::order(x.clone(), y.clone());
        e.merge_suggestions(1, None, 1, false);
        let list = merges(&mut ev, 1, Duration::from_secs(2)).0.unwrap();
        assert_eq!(list.iter().map(|p| p.key()).collect::<Vec<_>>(), [k(&id(1), &id(2))], "raw suggest cut");
        assert!((list[0].distance - 0.0955).abs() < 0.001, "raw distance: {}", list[0].distance);
        // With the judge: the raw band (C at 0.11 is in it).
        e.set_judge(JudgeChoice::Gemma4);
        wait_for(&e, "the choice", |s| s.judge.active(Device::Gpu));
        e.merge_suggestions(2, None, 1, true);
        let (_, judged) = merges(&mut ev, 2, Duration::from_secs(5));
        let offered: HashSet<(GroupId, GroupId)> = judged.iter().flat_map(|j| j.0.iter().map(|p| p.key())).collect();
        assert_eq!(offered, [k(&id(1), &id(2)), k(&id(1), &id(3))].into_iter().collect(), "both in the raw band, both said yes");
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

    /// The group editor's *Apply* through the engine, with a regroup
    /// landing between opening the editor and applying (raw: the grouping
    /// choice changed; whitened: the space changed): the heads are found
    /// where they are then, it applies as one step, one undo takes it
    /// back, and the record has one `edit` entry.
    #[test]
    fn an_edit_applies_after_a_regroup_and_undoes_in_one_step() {
        for whiten in [false, true] {
            let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
            let specs = [(1, 0), (2, Suits::at(0.1)), (3, -Suits::at(0.9)), (4, -Suits::at(0.95))];
            let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
            let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
            let (lib, d) = library_of(if whiten { "edit-white" } else { "edit-raw" }, &refs);
            let config = Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
            let (bus, _rx) = crate::bus::channel();
            let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
            e.on_library(lib.clone());
            e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
            wait_for(&e, "the heads", |s| s.heads.found == 4 && s.heads.step == heads::Step::Idle && s.pass.is_none());
            let _ = fursuit_groups(&e, 2);
            let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
            let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
            // The editor opens on {1, 2}: head 2 goes to the group of {3, 4}
            // (known by its heads), head 1 into a new group named Rook.
            let edit = crate::model::subjects::GroupEdit {
                kind: Kind::Fursuit,
                targets: vec![crate::model::subjects::EditTarget::ClusterOf(vec![head(3), head(4)])],
                new_names: vec![Some("Rook".into())],
                moves: vec![(head(2), crate::model::subjects::EditDest::Group(0)), (head(1), crate::model::subjects::EditDest::New(0))],
                ..Default::default()
            };
            // A regroup lands meanwhile.
            if whiten {
                e.set_whitening(super::super::whiten::Whitening::identity(2));
                wait_for(&e, "the whitening", |s| s.heads.whitened);
            } else {
                e.set_fursuit_grouping(heads::Grouping::Looser);
                wait_for(&e, "the regroup", |s| s.heads.grouping == heads::Grouping::Looser && !s.heads.regrouping);
            }
            let _ = fursuit_groups(&e, 2);
            e.apply_edit(1, edit, Via::Editor { proposal: false, edited: 2 });
            let started = Instant::now();
            let sizes = |e: &Engine| {
                let mut v: Vec<(Option<String>, usize)> = e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).map(|g| (g.name.clone(), g.photos)).collect();
                v.sort();
                v
            };
            while sizes(&e) != [(None, 3), (Some("Rook".to_string()), 1)] {
                assert!(started.elapsed() < Duration::from_secs(5), "whiten={whiten}: {:?}", sizes(&e));
                std::thread::sleep(Duration::from_millis(20));
            }
            e.undo();
            let started = Instant::now();
            while sizes(&e) != [(None, 2), (None, 2)] {
                assert!(started.elapsed() < Duration::from_secs(5), "whiten={whiten}, after undo: {:?}", sizes(&e));
                std::thread::sleep(Duration::from_millis(20));
            }
            e.shutdown();
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// A fursuit head taken out of its group never rejoins it when the
    /// grouping runs again, and can still be sent to another group.
    #[test]
    fn a_taken_out_head_never_rejoins_its_group_after_a_regroup() {
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.05)), (3, Suits::at(0.08)), (4, -Suits::at(0.9)), (5, -Suits::at(0.95))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("edit-takeout", &refs);
        let config = Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 5 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        let _ = fursuit_groups(&e, 2);
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
        use crate::model::subjects::{EditDest, EditTarget, GroupEdit};
        e.apply_edit(1, GroupEdit { kind: Kind::Fursuit, moves: vec![(head(3), EditDest::TakenOut)], ..GroupEdit::default() }, Via::Editor { proposal: false, edited: 1 });
        // The group of 1 and 2 is a subject now, 3 rejected for it; the
        // regroup after the edit leaves 3 out of it.
        let started = Instant::now();
        let holder = loop {
            let s = e.subjects();
            if let Some(g) = s.groups.iter().find(|g| g.kind == Kind::Fursuit && matches!(g.id, GroupId::Subject(_))) {
                if g.photos == 2 && !e.status().heads.regrouping {
                    break g.id.clone();
                }
            }
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", s.groups);
            std::thread::sleep(Duration::from_millis(20));
        };
        e.set_fursuit_grouping(heads::Grouping::Looser);
        wait_for(&e, "the regroup", |s| s.heads.grouping == heads::Grouping::Looser && !s.heads.regrouping);
        std::thread::sleep(Duration::from_millis(200));
        let photos_of = |e: &Engine, id: &GroupId| e.subjects().groups.iter().find(|g| g.id == *id).map(|g| g.photos);
        assert_eq!(photos_of(&e, &holder), Some(2), "3 never back in");
        // Still free for another group: sent to the group of 4 and 5.
        e.apply_edit(2, GroupEdit { kind: Kind::Fursuit, targets: vec![EditTarget::ClusterOf(vec![head(4), head(5)])], moves: vec![(head(3), EditDest::Group(0))], ..GroupEdit::default() }, Via::Editor { proposal: false, edited: 1 });
        let started = Instant::now();
        while !e.subjects().groups.iter().any(|g| g.kind == Kind::Fursuit && g.photos == 3) {
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", e.subjects().groups);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(photos_of(&e, &holder), Some(2));
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Dissolving a group through the engine: it leaves the tab, and its
    /// heads aren't a pile of one-head groups (they are counted, not
    /// shown); a regroup (another grouping choice) never makes it again;
    /// the heads stay fursuit heads, free to be sent to another group;
    /// one undo puts the group back exactly (its id and photos).  A named
    /// group's name goes with it.  Each is recorded as `dissolve`, its
    /// undo as an `undo` of it, and the export codes the name and lists
    /// the dissolved heads in `groups.json`.
    #[test]
    fn a_dissolved_group_stays_apart_through_a_regroup_and_undoes_exactly() {
        use crate::model::decisions::{Act, At, Entry};
        let name = |n: u32, a: i32| format!("{n:04}-fursuit-90-{}{}", if a < 0 { "m" } else { "p" }, a.abs());
        let specs = [(1, 0), (2, Suits::at(0.05)), (3, Suits::at(0.08)), (4, -Suits::at(0.9)), (5, -Suits::at(0.95))];
        let files: Vec<(String, String)> = specs.iter().map(|&(n, a)| (format!("2025/Mar-2025/{:02}-Mar-2025", n), format!("{}.JPG", name(n, a)))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, f)| (d.as_str(), f.as_str())).collect();
        let (lib, d) = library_of("dissolve", &refs);
        let user = d.join("user").join("subjects.json");
        let config = Config { factory: Box::new(|_| Ok(Arc::new(Suits) as Arc<dyn Embedder>)), installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(user.clone()), downloader: no_downloads() };
        let (bus, _rx) = crate::bus::channel();
        let e = Engine::with_config(bus, Device::Gpu, true, false, ModelId::AuraFace, true, config);
        e.on_library(lib);
        e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
        wait_for(&e, "the heads", |s| s.heads.found == 5 && s.heads.step == heads::Step::Idle && s.pass.is_none());
        let key_of = |n: usize| Key::of(1, lib_path(&d, &files[n - 1]).as_os_str().as_encoded_bytes());
        let head = |n: usize| FaceRef::new(key_of(n), [0.3, 0.2, 0.3, 0.4]);
        let _ = fursuit_groups(&e, 2);
        let before: Vec<(GroupId, usize)> = {
            let s = e.subjects();
            let mut v: Vec<(GroupId, usize)> = s.groups.iter().filter(|g| g.kind == Kind::Fursuit).map(|g| (g.id.clone(), g.photos)).collect();
            v.sort_by_key(|x| x.1);
            v
        };
        assert_eq!(before.iter().map(|x| x.1).collect::<Vec<_>>(), [2, 3], "{before:?}");
        let three = before[1].0.clone();
        let loose = |e: &Engine| e.subjects().loose.get(&Kind::Fursuit).copied().unwrap_or(0);
        let wait_loose = |e: &Engine, n: usize, groups: usize| {
            let started = Instant::now();
            while loose(e) != n || e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).count() != groups || e.status().heads.regrouping {
                assert!(started.elapsed() < Duration::from_secs(5), "loose {} {:?}", loose(e), e.subjects().groups);
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // A person's group isn't a fursuit's: a stale id is refused.
        e.dissolve(GroupId::Cluster(12345), At::Editor);
        // Dissolved: the three heads leave the tab, counted, not shown.
        e.dissolve(three.clone(), At::Editor);
        wait_loose(&e, 3, 1);
        assert_eq!(e.subjects().undoable.as_deref(), Some("Dissolved a group of 3 fursuit heads"));
        let file = crate::model::subjects::SubjectsFile::load(&user);
        assert_eq!((file.written_version(), file.dissolved.len(), file.dissolved[0].heads.len(), file.removed.len()), (2, 1, 3, 0), "kept by heads; nothing removed");
        assert_eq!(e.status().heads.found, 5, "they stay fursuit heads");
        // Another grouping choice groups everything again: still apart.
        e.set_fursuit_grouping(heads::Grouping::Looser);
        wait_for(&e, "the regroup", |s| s.heads.grouping == heads::Grouping::Looser && !s.heads.regrouping);
        wait_loose(&e, 3, 1);
        // Free for another group: one sent to the group of 4 and 5.
        use crate::model::subjects::{EditDest, EditTarget, GroupEdit};
        e.apply_edit(1, GroupEdit { kind: Kind::Fursuit, targets: vec![EditTarget::ClusterOf(vec![head(4), head(5)])], moves: vec![(head(2), EditDest::Group(0))], ..GroupEdit::default() }, Via::Editor { proposal: false, edited: 1 });
        wait_loose(&e, 2, 1);
        assert!(e.subjects().groups.iter().any(|g| g.kind == Kind::Fursuit && g.photos == 3), "{:?}", e.subjects().groups);
        e.undo();
        wait_loose(&e, 3, 1);
        // One undo: the group exactly as it was.
        e.set_fursuit_grouping(heads::Grouping::Balanced);
        wait_for(&e, "the regroup", |s| s.heads.grouping == heads::Grouping::Balanced && !s.heads.regrouping);
        e.undo();
        wait_loose(&e, 0, 2);
        let mut after: Vec<(GroupId, usize)> = e.subjects().groups.iter().filter(|g| g.kind == Kind::Fursuit).map(|g| (g.id.clone(), g.photos)).collect();
        after.sort_by_key(|x| x.1);
        assert_eq!(after, before, "back exactly");
        assert_eq!(crate::model::subjects::SubjectsFile::load(&user).written_version(), 1, "nothing optional left");
        // Named, then dissolved: the name goes; undo brings it back.
        e.name_group(three.clone(), "Suit A".into());
        let started = Instant::now();
        let named = loop {
            if let Some(g) = e.subjects().groups.iter().find(|g| g.name.as_deref() == Some("Suit A")) {
                break g.id.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        e.dissolve(named.clone(), At::Menu);
        wait_loose(&e, 3, 1);
        assert!(e.subjects().groups.iter().all(|g| g.name.is_none()), "the name went");
        assert_eq!(e.subjects().undoable.as_deref(), Some("Dissolved Suit A"));
        // The record: each dissolve, the undo naming the first.
        let got: Vec<Entry> = {
            e.flush_decisions(Duration::from_secs(2));
            e.decisions().entries()
        };
        let dissolves: Vec<&Entry> = got.iter().filter(|x| x.act.name() == "dissolve").collect();
        assert_eq!(dissolves.len(), 2, "{got:?}");
        let Act::Dissolve { group, heads: hs, dissolved, named: was_named, spread } = &dissolves[0].act else { panic!() };
        assert_eq!((group.n, hs.len(), *dissolved, *was_named, group.name.clone()), (3, 3, 3, false, None));
        assert!(hs.iter().any(|h| h.0 == head(1)) && spread.is_some_and(|s| s > 0.0 && s < 0.2), "{spread:?}");
        let how = dissolves[0].how.as_ref().unwrap();
        assert_eq!((how.by.as_str(), how.at), ("hand", Some(At::Editor)));
        let lib = dissolves[0].lib.as_ref().unwrap();
        assert_eq!(lib.grouping, Some(heads::Grouping::Balanced), "the grouping choice in force");
        assert!(got.iter().any(|x| matches!(&x.act, Act::Undo { undoes, of } if *undoes == dissolves[0].id && of == "dissolve")), "the undo names it");
        let Act::Dissolve { group, named: was_named, .. } = &dissolves[1].act else { panic!() };
        assert_eq!((group.name.as_deref(), *was_named, dissolves[1].how.as_ref().unwrap().at), (Some("Suit A"), true, Some(At::Menu)));
        // The export: no name, a code; the dissolved heads in groups.json.
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (dir, _) = e.export_record(&out).unwrap();
        let text = std::fs::read_to_string(dir.join(crate::model::decisions::FILE_NAME)).unwrap() + &std::fs::read_to_string(dir.join(crate::model::group_export::FILE_NAME)).unwrap();
        assert!(!text.contains("Suit A"), "no name leaves the computer");
        let (entries, groups) = crate::model::group_export::read_export(&dir).unwrap();
        let coded = entries.iter().find_map(|x| match &x.act {
            Act::Dissolve { group, named: true, .. } => group.name.clone(),
            _ => None,
        });
        assert!(coded.as_deref().is_some_and(|c| c.starts_with("fursuit-")), "{coded:?}");
        let groups = groups.expect("groups.json");
        assert!(groups.groups.is_empty(), "the dissolved group isn't a named group any more");
        assert_eq!(groups.dissolved.len(), 1);
        assert_eq!((groups.dissolved[0].kind, groups.dissolved[0].heads.len()), (Kind::Fursuit, 3));
        e.undo();
        wait_loose(&e, 0, 2);
        assert!(e.subjects().groups.iter().any(|g| g.name.as_deref() == Some("Suit A") && g.photos == 3), "undo brings the name back");
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

    /// A rebuild forgets the heads with the index, so its end looks for
    /// them again as a first index does: on the card the scan follows
    /// unasked; on the processor the fursuit photos are put to the user.
    #[test]
    fn a_rebuild_looks_for_fursuit_heads_again() {
        let files = [
            ("2025/Mar-2025/12-Mar-2025", "0001-fursuit-90.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0002-fursuit-80.JPG"),
            ("2025/Mar-2025/12-Mar-2025", "0003-dog-60.JPG"),
            ("2025/Mar-2025/13-Mar-2025", "0004-fursuit-70.JPG"),
        ];
        for (device, name) in [(Device::Gpu, "rebuild-heads-gpu"), (Device::Cpu, "rebuild-heads-cpu")] {
            let axes: EmbedderFactory = Box::new(|_| Ok(Arc::new(Axes) as Arc<dyn Embedder>));
            let (bus, _rx) = crate::bus::channel();
            let (lib, d) = library_of(name, &files);
            let config = Config { factory: axes, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
            let e = Engine::with_config(bus, device, true, false, ModelId::AuraFace, true, config);
            e.on_library(lib);
            wait_for(&e, "the library", |s| s.library_shots == 4);
            e.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2]);
            wait_for(&e, "the first index", |s| s.indexed_shots == 4 && s.pass.is_none());
            if device == Device::Cpu {
                wait_for(&e, "the question", |s| matches!(s.heads.step, heads::Step::Asking(_)));
                e.answer_heads(true);
            }
            wait_for(&e, "the first scan", |s| s.heads.scanned == 3 && s.heads.step == heads::Step::Idle);
            e.rebuild_index();
            wait_for(&e, "the rebuild", |s| s.indexed_shots == 4 && s.pass.is_none());
            if device == Device::Gpu {
                wait_for(&e, "the scan after the rebuild", |s| s.heads.scanned == 3 && s.heads.found == 3 && s.heads.step == heads::Step::Idle);
            } else {
                wait_for(&e, "the question after the rebuild", |s| matches!(s.heads.step, heads::Step::Asking(_)));
                assert_eq!(e.status().heads.step, heads::Step::Asking(heads::Offer { photos: 3, why: HeadWhy::FirstIndex }));
            }
            let _ = std::fs::remove_dir_all(&d);
        }
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

    // ---- the viewer's labels ----

    /// Faces for the labels' tests: a file named `…-face…` has one face
    /// at [0.3, 0.2, 0.2, 0.3] (its vector from the name); a drawn face is
    /// recognised at its own box (its vector from the box); a drawn head
    /// gets a vector from its box.  Search vectors as [`Axes`].  Counts
    /// the drawn work, and can fail as a card does.
    struct Labeller {
        drawn_faces: std::sync::atomic::AtomicUsize,
        drawn_heads: std::sync::atomic::AtomicUsize,
        card_fails: bool,
    }

    const FOUND: [f32; 4] = [0.3, 0.2, 0.2, 0.3];

    impl Labeller {
        fn new(card_fails: bool) -> Labeller {
            Labeller { drawn_faces: Default::default(), drawn_heads: Default::default(), card_fails }
        }

        fn vector(seed: f32) -> Vec<f32> {
            let v = [seed.cos(), seed.sin(), 0.3];
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            v.iter().map(|x| x / n).collect()
        }

        fn face(bbox: [f32; 4], seed: f32) -> crate::ml::client::FaceFound {
            crate::ml::client::FaceFound { bbox, landmarks: [[bbox[0] + 0.05, bbox[1] + 0.1]; 5], score: 0.95, size_px: 200.0, sharpness: 300.0, vector: Labeller::vector(seed), tile: vec![0xff, 0xd8] }
        }
    }

    impl Embedder for Labeller {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            Axes.device_report()
        }
        fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<crate::ml::client::ImageResult>, HelperError> {
            use crate::ml::client::FaceAsk;
            if self.card_fails {
                return Err(HelperError::Card(CardFailure::Crashed));
            }
            let mut out = Axes.embed_images(files, want)?;
            for r in &mut out {
                let Ok(e) = &mut r.result else { continue };
                if !want.search {
                    e.vector.clear();
                }
                e.faces = match &want.faces {
                    FaceAsk::Detect if crate::util::stem(&r.path).contains("face") => vec![Labeller::face(FOUND, 0.1)],
                    FaceAsk::Drawn { files } => {
                        self.drawn_faces.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        files.iter().filter(|d| d.path == r.path).flat_map(|d| d.boxes.iter().map(|b| Labeller::face(*b, b[0] * 10.0))).collect()
                    }
                    _ => Vec::new(),
                };
            }
            Ok(out)
        }
        fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            Axes.encode_text(texts)
        }
        fn embed_heads(&self, files: &[crate::ml::heads::KnownHeads]) -> Result<Vec<crate::ml::heads::HeadsResult>, HelperError> {
            if self.card_fails {
                return Err(HelperError::Card(CardFailure::Crashed));
            }
            self.drawn_heads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(files
                .iter()
                .map(|j| {
                    let heads = j.boxes.iter().map(|b| crate::ml::heads::Head { bbox: *b, score: 0.0, size: 300, sharpness: 0.2, vector: Labeller::vector(b[1] * 10.0), tile: Vec::new() }).collect();
                    crate::ml::heads::HeadsResult { path: j.path.clone(), result: Ok(crate::ml::heads::PhotoHeads { key: j.key, heads }) }
                })
                .collect())
        }
    }

    impl Events {
        /// The labels answering request `seq`.
        fn labels(&mut self, seq: u64) -> PhotoLabels {
            let stream = &mut self.stream;
            self.rt.block_on(async {
                use iced::futures::StreamExt;
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await else { continue };
                    for ev in batch {
                        if let BgEvent::PhotoLabels { seq: s, labels } = ev {
                            if s == seq {
                                return (*labels).clone();
                            }
                        }
                    }
                }
                panic!("no labels for request {seq}");
            })
        }
    }

    /// Ask for a photo's labels until `ok` holds (a drawn box's vector
    /// comes back from its worker a moment later).
    fn labels_until(e: &Engine, ev: &mut Events, seq: &mut u64, target: LabelTarget, what: &str, ok: impl Fn(&PhotoLabels) -> bool) -> PhotoLabels {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            *seq += 1;
            e.photo_labels(*seq, target.clone());
            let l = ev.labels(*seq);
            if ok(&l) {
                return l;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {l:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn labels_library(name: &str) -> (Arc<LibraryIndex>, PathBuf, Vec<PathBuf>) {
        let files = [("2025/Mar-2025/12-Mar-2025", "0001-face.JPG"), ("2025/Mar-2025/12-Mar-2025", "0002-face.JPG"), ("2025/Mar-2025/12-Mar-2025", "0003-plain.JPG")];
        let (lib, d) = library_of(name, &files);
        let paths = files.iter().map(|(day, f)| lib_path(&d, &(day.to_string(), f.to_string()))).collect();
        (lib, d, paths)
    }

    /// The viewer's labels through the engine, on the card: a photo's
    /// faces come by its path; one removed stays removed; a face and a
    /// fursuit head drawn on a photo with none are matched on the card
    /// (pending until then) and join their kinds' groups; Rebuild clears
    /// the caches but keeps every label: the removed face is not found
    /// again, the drawn boxes are matched again at the pass's end; a
    /// photo the index doesn't hold says so; undo takes changes back one
    /// at a time; nothing is ever written beside the photos.
    #[test]
    fn labels_are_kept_through_a_rebuild_and_drawn_boxes_are_matched_on_the_card() {
        let (lib, d, paths) = labels_library("labels-rebuild");
        let fake = Arc::new(Labeller::new(false));
        let f2 = fake.clone();
        let asked: Arc<Mutex<Vec<(Device, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let factory: EmbedderFactory = Box::new(move |spec: Spec| {
            log.lock().push((spec.device, spec.face_model.is_some()));
            Ok(f2.clone() as Arc<dyn Embedder>)
        });
        let subjects = d.join("data/subjects.json");
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: Some(subjects.clone()), downloader: no_downloads() };
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let e = Engine::with_config(bus, Device::Gpu, true, true, ModelId::AuraFace, false, config);
        e.on_library(lib.clone());
        e.start_first_index(&[ModelId::Siglip2, ModelId::Yunet, ModelId::AuraFace]);
        wait_for(&e, "the first index with faces", |s| s.indexed_shots == 3 && s.pass.is_none() && s.faces.found == 2);
        let mut seq = 0;
        let files = |p: &PathBuf| LabelTarget::Files { jpg: Some(p.clone()), raw: None };
        // A photo's face, by its path; unnamed.
        let l = labels_until(&e, &mut ev, &mut seq, files(&paths[0]), "the face", |l| l.boxes.len() == 1);
        let key1 = l.key.expect("in the library");
        assert_eq!((l.boxes[0].kind, l.boxes[0].name.clone(), l.boxes[0].drawn, l.pending), (Kind::Person, None, false, None));
        let found = l.boxes[0].face;
        // Named, then removed.
        e.label(LabelEdit::Name { face: found, kind: Kind::Person, to: crate::model::subjects::NameTo::New("Someone".into()) });
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key1), "the name", |l| l.boxes.first().is_some_and(|b| b.name.as_deref() == Some("Someone")));
        assert!(l.boxes[0].subject.is_some());
        e.label(LabelEdit::Remove { face: found, kind: Kind::Person });
        labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key1), "the removal", |l| l.boxes.is_empty());
        // Drawn on the plain photo: a face and a fursuit head.
        let l = labels_until(&e, &mut ev, &mut seq, files(&paths[2]), "the plain photo", |l| l.key.is_some());
        let key3 = l.key.unwrap();
        assert!(l.boxes.is_empty());
        let drawn_face = FaceRef::new(key3, [0.1, 0.1, 0.2, 0.25]);
        let drawn_head = FaceRef::new(key3, [0.55, 0.15, 0.3, 0.35]);
        e.label(LabelEdit::Add { face: drawn_face, kind: Kind::Person, name: Some(crate::model::subjects::NameTo::New("Someone".into())) });
        e.label(LabelEdit::Add { face: drawn_head, kind: Kind::Fursuit, name: Some(crate::model::subjects::NameTo::New("Suit".into())) });
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key3), "the drawn boxes matched", |l| l.boxes.len() == 2 && l.boxes.iter().all(|b| b.drawn && !b.pending));
        assert_eq!(l.boxes.iter().map(|b| (b.kind, b.name.clone().unwrap())).collect::<Vec<_>>(), [(Kind::Person, "Someone".to_string()), (Kind::Fursuit, "Suit".to_string())]);
        assert!(fake.drawn_faces.load(std::sync::atomic::Ordering::Relaxed) >= 1 && fake.drawn_heads.load(std::sync::atomic::Ordering::Relaxed) >= 1);
        assert!(asked.lock().iter().all(|(d, _)| *d == Device::Gpu), "the card only: {:?}", asked.lock());
        // The kinds never mix: the person is a person, the suit a fursuit.
        let all = e.subjects();
        assert!(all.groups.iter().any(|g| g.kind == Kind::Person && g.name.as_deref() == Some("Someone")));
        assert!(all.groups.iter().any(|g| g.kind == Kind::Fursuit && g.name.as_deref() == Some("Suit")));
        // The user's file holds the labels (version 2); nothing beside the photos.
        let saved = crate::model::subjects::SubjectsFile::load(&subjects);
        assert_eq!((saved.version, saved.added.len(), saved.removed.len()), (crate::model::subjects::FILE_VERSION, 2, 1));
        let beside: Vec<_> = std::fs::read_dir(paths[0].parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(beside.len(), 3, "only the photos: {beside:?}");
        // Rebuild: the caches go, the labels stay.
        let drawn_before = fake.drawn_faces.load(std::sync::atomic::Ordering::Relaxed);
        e.rebuild_index();
        wait_for(&e, "the rebuild", |s| s.indexed_shots == 3 && s.pass.is_none());
        labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key1), "the removed face still removed", |l| l.key.is_some() && l.boxes.is_empty());
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key3), "the drawn boxes matched again", |l| l.boxes.len() == 2 && l.boxes.iter().all(|b| !b.pending));
        assert!(l.boxes.iter().all(|b| b.drawn && b.name.is_some()));
        assert!(fake.drawn_faces.load(std::sync::atomic::Ordering::Relaxed) > drawn_before, "matched again after the rebuild");
        // The other face photo still has its face (not removed).
        let l = labels_until(&e, &mut ev, &mut seq, files(&paths[1]), "the other face", |l| l.boxes.len() == 1);
        assert_eq!(l.boxes[0].face.bbox, FaceRef::new(key3, FOUND).bbox);
        // A photo the index doesn't hold.
        let elsewhere = d.join("elsewhere.JPG");
        std::fs::write(&elsewhere, "not in the library").unwrap();
        let l = labels_until(&e, &mut ev, &mut seq, files(&elsewhere), "a stranger", |_| true);
        assert_eq!(l, PhotoLabels::default(), "not in the library: nothing looked for");
        // Undo: the head drawn, then the face drawn, then the removal.
        e.undo();
        labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key3), "the head taken back", |l| l.boxes.len() == 1 && l.boxes[0].kind == Kind::Person);
        e.undo();
        e.undo();
        labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key1), "the removal taken back", |l| l.boxes.len() == 1 && l.boxes[0].name.as_deref() == Some("Someone"));
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A drawn box needs the card: with the processor chosen it is kept
    /// and waits (no helper asked for it), and a card that fails leaves it
    /// waiting and says so; never the processor.  A kind change waits the
    /// same way, and leaves its old kind at once.
    #[test]
    fn a_drawn_box_waits_for_the_card_and_never_goes_to_the_processor() {
        let (lib, d, paths) = labels_library("labels-card");
        let fake = Arc::new(Labeller::new(false));
        let broken = Arc::new(Labeller::new(true));
        let asked: Arc<Mutex<Vec<(Device, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let (f2, b2) = (fake.clone(), broken.clone());
        let factory: EmbedderFactory = Box::new(move |spec: Spec| {
            log.lock().push((spec.device, spec.text_only));
            // The processor works; the card fails once chosen.
            Ok(if spec.device == Device::Cpu { f2.clone() as Arc<dyn Embedder> } else { b2.clone() as Arc<dyn Embedder> })
        });
        let config = Config { factory, installed: Box::new(|_| true), index_dir: Some(d.join("index")), subjects_path: None, downloader: no_downloads() };
        let (bus, rx) = crate::bus::channel();
        let mut ev = Events::new(&rx);
        let e = Engine::with_config(bus, Device::Cpu, true, true, ModelId::AuraFace, false, config);
        e.on_library(lib.clone());
        e.start_first_index(&[ModelId::Siglip2, ModelId::Yunet, ModelId::AuraFace]);
        wait_for(&e, "the first index on the processor", |s| s.indexed_shots == 3 && s.pass.is_none() && s.faces.found == 2);
        let mut seq = 0;
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Files { jpg: Some(paths[2].clone()), raw: None }, "the plain photo", |l| l.key.is_some());
        let key3 = l.key.unwrap();
        let before = asked.lock().len();
        e.label(LabelEdit::Add { face: FaceRef::new(key3, [0.1, 0.1, 0.2, 0.25]), kind: Kind::Person, name: None });
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key3), "the drawn face", |l| l.boxes.len() == 1);
        assert!(l.boxes[0].pending && l.boxes[0].drawn);
        assert_eq!(l.pending, Some(PendingWhy::ProcessorChosen));
        // A kind change on a found face: out of people at once, a pending head.
        let l1 = labels_until(&e, &mut ev, &mut seq, LabelTarget::Files { jpg: Some(paths[0].clone()), raw: None }, "the face", |l| l.boxes.len() == 1);
        e.label(LabelEdit::Kind { face: l1.boxes[0].face, from: Kind::Person, to: Kind::Fursuit, name: None });
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(l1.key.unwrap()), "the kind change", |l| l.boxes.len() == 1 && l.boxes[0].kind == Kind::Fursuit);
        assert!(l.boxes[0].pending);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(asked.lock().len(), before, "nothing started for the drawn boxes on the processor");
        assert_eq!(fake.drawn_faces.load(std::sync::atomic::Ordering::Relaxed) + fake.drawn_heads.load(std::sync::atomic::Ordering::Relaxed), 0);
        // The card chosen, and failing: still waiting, the failure said.
        e.set_device(Device::Gpu);
        wait_for(&e, "the card's failure", |s| matches!(s.stopped, Some(Stopped::Card(_))));
        let l = labels_until(&e, &mut ev, &mut seq, LabelTarget::Key(key3), "still waiting", |l| l.pending == Some(PendingWhy::NoCard));
        assert!(l.boxes[0].pending);
        assert_eq!(fake.drawn_faces.load(std::sync::atomic::Ordering::Relaxed), 0, "never on the processor");
        assert!(asked.lock()[before..].iter().all(|(d, text_only)| *d == Device::Gpu || *text_only), "no processor helper for image work: {:?}", asked.lock());
        e.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }
}
