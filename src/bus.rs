//! One channel from every background worker to the UI.
//!
//! Workers (scanner, thumbnailers, the viewer loader, indexers, exports) send
//! [`BgEvent`]s on an unbounded channel.  A single subscription drains it and
//! delivers events to `update` in batches, so thousands of thumbnails arriving
//! during a fast scroll cost a handful of UI updates, not one each.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use iced::futures::channel::mpsc;
use iced::futures::{Stream, StreamExt};
use parking_lot::Mutex;

use crate::exif_panel::Sections;
use crate::imaging::loader::FullImage;
use crate::imaging::thumbs::{OwnerId, Thumb};
use crate::model::library_index::LibraryIndex;
use crate::model::record::PhotoRecord;
use crate::ops::edit_copy::CopyReport;
use crate::ops::library::ExportReport;

pub type Bus = mpsc::UnboundedSender<BgEvent>;

#[derive(Clone)]
pub enum BgEvent {
    ScanBatch { scan: u64, records: Arc<Vec<PhotoRecord>> },
    ScanProgress { scan: u64, count: usize },
    ScanComplete { scan: u64, count: usize },
    ScanError { scan: u64, message: String },

    Thumb { owner: OwnerId, path: PathBuf, thumb: Arc<Thumb> },
    Full { path: PathBuf, result: Result<Arc<FullImage>, String> },
    /// The embedded preview shown while `Full` decodes (`None`: there is none).
    Preview { path: PathBuf, image: Option<Arc<FullImage>> },

    Exif { key: (PathBuf, Option<PathBuf>), sections: Sections },
    ExifTool { path: PathBuf, flat: Vec<(String, String)> },

    IndexProgress { job: u64, count: usize },
    /// A library index.  `done` is false for the one built from the saved
    /// tree while the job goes on to check the disk for changes.
    IndexReady { job: u64, index: Arc<LibraryIndex>, done: bool },
    /// The job finished without a new index: the one shown is current
    /// (`checked`), or there was nothing saved to show yet.
    IndexIdle { job: u64, checked: bool },
    IndexError { job: u64, message: String },
    /// A day's grid data for index generation `generation`.  `complete` is
    /// false for a first delivery sent while capture times are still read.
    DayRecords { date: NaiveDate, generation: u64, data: Arc<crate::app::calendar::DayData>, complete: bool },

    ExportProgress { done: usize, total: usize },
    ExportDone(Arc<ExportReport>),
    CopyProgress { files: usize, files_total: usize, bytes: u64, bytes_total: u64 },
    CopyDone(Arc<CopyReport>),

    Trashed(Arc<crate::ops::trash::TrashResult>),
    Sorted(Arc<SortResult>),
    /// An undo or redo that ran on a worker finished.
    Undo(Arc<crate::app::undo::Done>),
    /// The desktop's global menu came or went.
    GlobalMenu(bool),
    /// A command chosen in the global menu.
    MenuCommand(crate::app::commands::Cmd),
    /// Sidecar saves started or stopped failing.
    Saver(Arc<crate::model::saver::Report>),
    /// Where a folder lives and whether it answered (`ops::netfs::probe`).
    NetProbe { path: PathBuf, seq: u64, probe: Arc<crate::ops::netfs::Probe> },
    /// Results of an analysis pass (`crate::analysis`), with its progress.
    Analysis { run: u64, batch: Arc<crate::analysis::Batch> },

    /// Library search: everything the views show about it changed
    /// (`crate::search::Engine`).
    SearchStatus(Arc<crate::search::Status>),
    /// A query's answer: hits best first (those carrying the query word
    /// as a hidden tag first, then by similarity), already cut by
    /// closeness, and how many shots were searched.  `error` is set when
    /// the query couldn't run (no helper, no index); `hits` is then
    /// empty.  `understood` is the query as the engine applied it (its
    /// words, exclusions, dates; `names` are recognised and not searched
    /// in phase 2: with no words beside them nothing runs and `hits` is
    /// empty without an error).  Dates with no words browse the period:
    /// every indexed shot of those days in shooting order, oldest first,
    /// `score` 0, minus what a search for each `-word` would show.  When
    /// the query names a real animal and
    /// not fursuits (`animal` is that word, singular), the fursuits the
    /// contrast left out come in `fursuits`, best first, never
    /// overlapping `hits` (the page's *Show fursuits too* section needs
    /// no second query); otherwise `fursuits` is empty and `animal`
    /// `None`.  `subjects` are the groups the query was narrowed to
    /// (phase 3): those passed to `Engine::query` and the `@name`s that
    /// resolved to a named subject; a name that resolved to nobody is
    /// absent (the page tells by comparing with `understood.names`).
    /// With subjects and no words, `hits` come clearest first (UI 12),
    /// `score` the clarity.
    SearchResults {
        seq: u64,
        hits: Arc<Vec<crate::search::Hit>>,
        indexed: usize,
        error: Option<String>,
        understood: Arc<crate::search::Parsed>,
        fursuits: Arc<Vec<crate::search::Hit>>,
        animal: Option<String>,
        subjects: Arc<Vec<crate::model::subjects::GroupId>>,
        /// `@name`s typed (not picked as chips) that named a person and a
        /// fursuit both: the person was searched; here is the fursuit of
        /// that name, for the page to offer ("also a fursuit called …").
        /// Never merged into the hits (the user, 2026-10-03: the kinds'
        /// photos are never one list).
        also_fursuits: Arc<Vec<(String, crate::model::subjects::GroupId)>>,
    },
    /// The *Subjects* page's state (every kind; a tab shows one): sent
    /// whenever the groups, names or counts change, throttled while a
    /// pass runs (`crate::search::Engine::subjects` reads the latest at
    /// any time).
    Subjects(Arc<crate::model::subjects::Subjects>),
    /// A subjects edit was taken back (`Engine::undo`): what was undone,
    /// for the status bar ("Merged 2 groups into Someone").  A fresh
    /// `Subjects` comes with it.
    SubjectsUndone(String),
    /// A group's faces, clearest first (`Engine::faces`), for the split
    /// view.  `group` is as asked.
    SubjectFaces { seq: u64, group: crate::model::subjects::GroupId, faces: Arc<Vec<crate::model::subjects::Face>> },
    /// The viewer's labels for a photo (`Engine::photo_labels`): its
    /// faces and fursuit heads with their names, for request `seq`.
    PhotoLabels { seq: u64, labels: Arc<crate::model::subjects::PhotoLabels> },
    /// Suggestions for review (`Engine::suggestions`): one entry per burst
    /// (its frames' faces, best first), most likely first; for a cluster,
    /// its own members to check.  Never a face of another kind.
    Suggestions { seq: u64, group: crate::model::subjects::GroupId, list: Arc<Vec<Vec<crate::model::subjects::Face>>> },
    /// The close-call judge's verdicts for the request `seq` (a fursuit's
    /// `suggestions` or `find_more`), as they land, nearest first: the
    /// entries new since the last event (one per burst, as `Suggestions`)
    /// it offers, and those it left out; `done` of `total` close calls.
    /// The first follows the request's `Suggestions` at once (with the
    /// verdicts already kept); the last has `done == total`.  Fursuits
    /// only.
    Judged {
        seq: u64,
        group: crate::model::subjects::GroupId,
        offered: Arc<Vec<Vec<crate::model::subjects::Face>>>,
        left_out: Arc<Vec<Vec<crate::model::subjects::Face>>>,
        done: usize,
        total: usize,
        /// Close calls past the cap (`search::judging::MAX_CLOSE_CALLS`)
        /// this request won't judge: the next request (the review again,
        /// *Find more*) judges them.  The same on every event of a request.
        beyond: usize,
    },
    /// A fursuit group's split check (`Engine::group_check`): its heads
    /// with their distance to the group's mean, farthest first; the pairs
    /// of its heads seen in one photo; a split proposed around the first
    /// such pair (the part with its first head, the part with its second).
    /// Empty for anything but a fursuit group.
    GroupCheck {
        seq: u64,
        group: crate::model::subjects::GroupId,
        far: Arc<Vec<(crate::model::subjects::FaceRef, f32)>>,
        pairs: Arc<Vec<(crate::model::subjects::FaceRef, crate::model::subjects::FaceRef)>>,
        split: Option<Arc<(Vec<crate::model::subjects::FaceRef>, Vec<crate::model::subjects::FaceRef>)>>,
    },
    /// Which group holds each of `heads` now (`Engine::locate`), in order;
    /// `None` for one in no group.
    Located { seq: u64, groups: Arc<Vec<Option<crate::model::subjects::GroupId>>> },
    /// A set of fursuit heads, farthest from the rest of the set first
    /// (`Engine::heads_order`).
    HeadsOrder { seq: u64, far: Arc<Vec<(crate::model::subjects::FaceRef, f32)>> },
    /// Every group of a kind with its distance to some heads (the group
    /// editor's panel, nearest first): `Engine::near_groups`.
    NearGroups { seq: u64, list: Arc<Vec<(crate::model::subjects::GroupId, f32)>> },
    /// The group editor's *Apply* went through (one undo step): its words.
    EditApplied { seq: u64, words: String },
    /// ... or was refused, nothing changed: why, and the subject that was
    /// gone, if that was it.
    EditRefused { seq: u64, why: String, gone: Option<crate::model::subjects::SubjectId> },
    /// Pairs of fursuit groups that may be one suit
    /// (`Engine::merge_suggestions`), nearest first: those offered now
    /// (by distance, or with verdicts already kept).  `focus`: the group
    /// asked about (its page's *Similar groups*), else the whole tab.
    /// `judging`: close pairs are being compared by the judge, and
    /// `MergeJudged` follows.  Fursuits only.
    MergeSuggestions { seq: u64, focus: Option<crate::model::subjects::GroupId>, list: Arc<Vec<crate::search::merging::GroupPair>>, judging: bool },
    /// The judge's verdicts on the pairs of groups of request `seq`, as
    /// they land: pairs it thinks are one suit (`offered`), and those it
    /// doesn't (`left_out`, with what it saw); `done` of `total` pairs;
    /// `beyond`: pairs past the cap, for the next request.  The last has
    /// `done == total`.
    MergeJudged {
        seq: u64,
        offered: Arc<Vec<crate::search::merging::GroupPair>>,
        left_out: Arc<Vec<crate::search::merging::GroupPair>>,
        done: usize,
        total: usize,
        beyond: usize,
    },
    /// *Things in your photos*: the hidden tags' objects and scenes with
    /// their photo counts, most first; sent when the index loads and
    /// whenever the tags change (`crate::search::Engine::things` reads
    /// the latest at any time).
    SearchThings(Arc<Vec<crate::search::Thing>>),
    /// A model download's progress (`crate::ml::download`): `done` bytes of
    /// `total`, of which `fetched` came over the network in this download
    /// (files already in place count in `done` only).
    ModelDownload { model: crate::ml::models::ModelId, done: u64, fetched: u64, total: u64, state: crate::ml::download::DownloadState },
}

/// (moved or already in place: (id, new path)), (failed: (id, filename, error)).
pub type SortResult = (
    Vec<(crate::model::collection::RecordId, PathBuf)>,
    Vec<(crate::model::collection::RecordId, String, String)>,
);

impl BgEvent {
    pub fn name(&self) -> &'static str {
        match self {
            BgEvent::ScanBatch { .. } => "ScanBatch",
            BgEvent::ScanProgress { .. } => "ScanProgress",
            BgEvent::ScanComplete { .. } => "ScanComplete",
            BgEvent::ScanError { .. } => "ScanError",
            BgEvent::Thumb { .. } => "Thumb",
            BgEvent::Full { .. } => "Full",
            BgEvent::Preview { .. } => "Preview",
            BgEvent::Exif { .. } => "Exif",
            BgEvent::ExifTool { .. } => "ExifTool",
            BgEvent::IndexProgress { .. } => "IndexProgress",
            BgEvent::IndexReady { .. } => "IndexReady",
            BgEvent::IndexIdle { .. } => "IndexIdle",
            BgEvent::IndexError { .. } => "IndexError",
            BgEvent::DayRecords { .. } => "DayRecords",
            BgEvent::ExportProgress { .. } => "ExportProgress",
            BgEvent::ExportDone(_) => "ExportDone",
            BgEvent::CopyProgress { .. } => "CopyProgress",
            BgEvent::CopyDone(_) => "CopyDone",
            BgEvent::Trashed(_) => "Trashed",
            BgEvent::Sorted(_) => "Sorted",
            BgEvent::Undo(_) => "Undo",
            BgEvent::GlobalMenu(_) => "GlobalMenu",
            BgEvent::MenuCommand(_) => "MenuCommand",
            BgEvent::Saver(_) => "Saver",
            BgEvent::NetProbe { .. } => "NetProbe",
            BgEvent::Analysis { .. } => "Analysis",
            BgEvent::SearchStatus(_) => "SearchStatus",
            BgEvent::SearchResults { .. } => "SearchResults",
            BgEvent::SearchThings(_) => "SearchThings",
            BgEvent::Subjects(_) => "Subjects",
            BgEvent::SubjectsUndone(_) => "SubjectsUndone",
            BgEvent::SubjectFaces { .. } => "SubjectFaces",
            BgEvent::PhotoLabels { .. } => "PhotoLabels",
            BgEvent::Suggestions { .. } => "Suggestions",
            BgEvent::Judged { .. } => "Judged",
            BgEvent::MergeSuggestions { .. } => "MergeSuggestions",
            BgEvent::GroupCheck { .. } => "GroupCheck",
            BgEvent::Located { .. } => "Located",
            BgEvent::HeadsOrder { .. } => "HeadsOrder",
            BgEvent::NearGroups { .. } => "NearGroups",
            BgEvent::EditApplied { .. } => "EditApplied",
            BgEvent::EditRefused { .. } => "EditRefused",
            BgEvent::MergeJudged { .. } => "MergeJudged",
            BgEvent::ModelDownload { .. } => "ModelDownload",
        }
    }
}

impl fmt::Debug for BgEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The receiving end, handed to the subscription exactly once.
#[derive(Clone)]
pub struct Receiver(Arc<Mutex<Option<mpsc::UnboundedReceiver<BgEvent>>>>);

impl Hash for Receiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

pub fn channel() -> (Bus, Receiver) {
    let (tx, rx) = mpsc::unbounded();
    (tx, Receiver(Arc::new(Mutex::new(Some(rx)))))
}

/// Batches of events: whatever is queued when the first one arrives, then a
/// short pause so bursts coalesce into one UI update per frame or so.
pub fn stream(rx: &Receiver) -> impl Stream<Item = Vec<BgEvent>> + use<> {
    let taken = rx.0.lock().take();
    iced::futures::stream::unfold(taken, |rx| async move {
        let mut rx = rx?;
        let first = rx.next().await?;
        let mut batch = vec![first];
        while let Ok(ev) = rx.try_recv() {
            batch.push(ev);
            if batch.len() >= 2048 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(6)).await;
        while let Ok(ev) = rx.try_recv() {
            batch.push(ev);
            if batch.len() >= 4096 {
                break;
            }
        }
        Some((batch, Some(rx)))
    })
}
