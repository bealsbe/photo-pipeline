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
    /// `None`.
    SearchResults {
        seq: u64,
        hits: Arc<Vec<crate::search::Hit>>,
        indexed: usize,
        error: Option<String>,
        understood: Arc<crate::search::Parsed>,
        fursuits: Arc<Vec<crate::search::Hit>>,
        animal: Option<String>,
    },
    /// *Things in your photos*: the hidden tags' objects and scenes with
    /// their photo counts, most first; sent when the index loads and
    /// whenever the tags change (`crate::search::Engine::things` reads
    /// the latest at any time).
    SearchThings(Arc<Vec<crate::search::Thing>>),
    /// A model download's progress (`crate::ml::download`).
    ModelDownload { model: crate::ml::models::ModelId, done: u64, total: u64, state: crate::ml::download::DownloadState },
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
