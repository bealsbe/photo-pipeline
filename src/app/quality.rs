//! The analysis as the app holds it: starting a pass (only when asked,
//! decision 2), taking its results in, and the suggestions worked out from
//! them (`model::hints::Hints`) for the views to show.  The views are in
//! `hints_ui` (the main window's Hints control, progress and banner),
//! `ui::hint_plate` (plates and words on photos) and `bulk` (the bulk
//! actions).  Opening a folder shows what an earlier pass cached
//! ([`start_cached`]), reading no photo.
//!
//! Measurements are kept by *canonical key* (the file's folder without a
//! `RAW`/`JPG` level, plus its name), so a Sort, which moves files into
//! those folders, keeps them, and so does a trash undone (restored files
//! get new record ids).  The suggestions are worked out again whenever the
//! collection changes or results arrive (at most twice a second while a
//! pass streams in).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;

use super::{App, Message};
use crate::analysis::{self, Batch, FileJob, Finished, Mode};
use crate::imaging::quality::Features;
use crate::model::hints::{Hints, Summary};
use crate::model::quality_cache;
use crate::util::canonical_parent;

/// Suggestions are worked out at most this often while results stream in.
const DERIVE_EVERY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Quality {
    run: u64,
    handle: Option<analysis::Handle>,
    /// (shots done, shots to do) while a pass runs.
    pub progress: Option<(usize, usize)>,
    /// Measurements by canonical key.
    features: HashMap<PathBuf, Arc<Features>>,
    /// Files that couldn't be measured, by canonical key, and why.
    pub failed: HashMap<PathBuf, String>,
    /// Identical files (`model::duplicates`), by canonical key.
    exact: Vec<Vec<PathBuf>>,
    /// Bumped whenever results change.
    revision: u64,
    /// What the views show.
    pub hints: Hints,
    derived_at: Option<Instant>,
    /// How the last pass ended.
    pub last: Option<Finished>,
    /// The running (or last) pass: a full one, or the cache only.
    mode: Option<Mode>,
    /// The Hints menu is open (the filter bar's Hints control).
    pub menu: bool,
    /// The "analysing stopped" banner was closed.
    pub banner_hidden: bool,
}

/// A path's canonical key: its folder without a `RAW`/`JPG` level, and its
/// name.  Unchanged by a Sort.
pub fn canonical_key(path: &Path) -> PathBuf {
    canonical_parent(path).join(path.file_name().unwrap_or_default())
}

impl Quality {
    pub fn running(&self) -> bool {
        self.handle.is_some()
    }

    /// A pass the user asked for is running (not just the cache being
    /// read on opening a folder).
    pub fn analysing(&self) -> bool {
        self.running() && self.mode == Some(Mode::Full)
    }

    /// Whether any results are held (from a pass, or the cache).
    pub fn has_results(&self) -> bool {
        !self.features.is_empty() || !self.exact.is_empty()
    }

    /// The frame a stacked burst shows on top: the sharpest of `run` (the
    /// frames shown) once analysed, else the first.  Ties keep the earlier.
    pub fn sharpest_of(&self, run: &[crate::model::collection::RecordId]) -> crate::model::collection::RecordId {
        let mut best = (run[0], f32::NEG_INFINITY);
        for &id in run {
            if let Some(h) = self.hints.get(id) {
                if h.score > best.1 {
                    best = (id, h.score);
                }
            }
        }
        best.0
    }

    pub fn features(&self, path: &Path) -> Option<&Arc<Features>> {
        self.features.get(&canonical_key(path))
    }

    pub fn summary(&self) -> Summary {
        self.hints.summary()
    }

    /// A line about the analysis: progress while a pass runs, what it
    /// found after (the status bar says this when a pass ends).
    pub fn status_text(&self) -> Option<String> {
        if let (Some((done, total)), true) = (self.progress, self.analysing()) {
            return Some(format!("Analysing: {} of {} shots", crate::util::thousands(done as u64), crate::util::thousands(total as u64)));
        }
        let fin = self.last.as_ref()?;
        if let Some(e) = &fin.error {
            return Some(format!("Analysing stopped: {e}"));
        }
        let s = self.summary();
        let found = super::hints_ui::findings(s.soft_only, s.accidental, self.hints.copies()).unwrap_or_else(|| "none look out of focus or accidental".into());
        Some(format!("{} of {} shots analysed: {found}; {} moments", s.measured, s.shots, s.moments))
    }


    fn clear(&mut self) {
        if let Some(h) = self.handle.take() {
            h.cancel();
        }
        // A new run number: batches still on their way are dropped.
        *self = Quality { run: self.run + 1, revision: self.revision + 1, ..Quality::default() };
    }
}

/// Analyse the working folder: measure what the cache doesn't hold and
/// look for duplicates.  Results arrive as `BgEvent::Analysis`.
pub fn start(app: &mut App) -> Task<Message> {
    start_with(app, Mode::Full)
}

/// Show what earlier analyses of this folder left in the cache, reading
/// no photo (for opening a folder; analysing is still only on demand).
pub fn start_cached(app: &mut App) -> Task<Message> {
    start_with(app, Mode::CachedOnly)
}

fn start_with(app: &mut App, mode: Mode) -> Task<Message> {
    let Some(root) = app.folder.clone() else { return Task::none() };
    if app.scanning || app.coll.is_empty() {
        return Task::none();
    }
    cancel(app);
    let q = &mut app.quality;
    q.run += 1;
    q.last = None;
    q.failed.clear();
    q.mode = Some(mode);
    q.banner_hidden = false;
    q.menu = false;
    let files = jobs(app);
    let total = files.iter().filter(|f| f.measure).count();
    let req = analysis::Request {
        run: app.quality.run,
        cache: quality_cache::file_for(&root),
        root,
        files,
        mode,
        threads: analysis::THREADS,
    };
    app.quality.progress = Some((0, total));
    app.quality.handle = Some(analysis::start(req, app.bus()));
    Task::none()
}

/// Stop a running pass (what it measured is kept, and cached).
pub fn cancel(app: &mut App) {
    if let Some(h) = app.quality.handle.take() {
        h.cancel();
    }
    app.quality.progress = None;
}

/// A new working folder (or the same one scanned again): results go.
pub fn on_folder_changed(app: &mut App) {
    app.quality.clear();
}

/// Every file of the folder, the shots to measure in the grid's order
/// first (what is on screen gets results first).
fn jobs(app: &App) -> Vec<FileJob> {
    analysis::jobs(&app.coll, &app.derived.visible)
}

/// A batch of results from pass `run`.
pub fn on_batch(app: &mut App, run: u64, batch: Arc<Batch>) -> Task<Message> {
    if run != app.quality.run {
        return Task::none();
    }
    let q = &mut app.quality;
    let changed = !batch.features.is_empty() || batch.duplicates.is_some();
    for (path, f) in &batch.features {
        q.features.insert(canonical_key(path), f.clone());
    }
    for (path, e) in &batch.failed {
        q.failed.insert(canonical_key(path), e.clone());
    }
    if let Some(groups) = &batch.duplicates {
        q.exact = groups.iter().map(|g| g.iter().map(|p| canonical_key(p)).collect()).collect();
    }
    if changed {
        q.revision += 1;
    }
    if let Some(fin) = &batch.finished {
        q.handle = None;
        q.progress = None;
        q.last = Some(fin.clone());
        q.derived_at = None;
    } else if let Some(h) = &q.handle {
        // (Batches still on their way after a stop don't bring the
        // progress back.)
        q.progress = Some((batch.done, batch.total));
        // Thumbnails the grid waits for come first.
        h.set_busy(app.thumbs.pending());
    }
    if batch.finished.is_some() {
        // Final hints: stacks take their sharpest frame on top, and the
        // hints filter and its counts follow.
        app.refresh_views();
        let mode = app.quality.mode.unwrap_or(Mode::CachedOnly);
        return super::hints_ui::on_ended(app, mode);
    }
    // The grid's plates follow as results stream in; what the hints
    // filter shows (and the Hints menu's counts) too, when they're on
    // screen.
    if refresh(app) && (app.filter.hint.is_some() || app.quality.menu) {
        app.refresh_views();
    }
    Task::none()
}

/// Work the suggestions out again if the collection or the results have
/// changed (debounced while a pass streams in).  Called from
/// `App::refresh_views`.  Returns whether they changed.
pub fn refresh(app: &mut App) -> bool {
    let q = &app.quality;
    if app.scanning || !q.has_results() {
        if !q.hints.is_empty() {
            app.quality.hints = Hints::default();
            return true;
        }
        return false;
    }
    if q.hints.is_current(&app.coll, q.revision) {
        return false;
    }
    // While results stream in, and the files are as they were, at most
    // every DERIVE_EVERY.
    let files_same = q.hints.generation() == Some(app.coll.generation());
    if q.running() && files_same && q.derived_at.is_some_and(|t| t.elapsed() < DERIVE_EVERY) {
        return false;
    }
    let by_key: HashMap<PathBuf, &Path> = app.coll.iter().map(|r| (canonical_key(&r.path), r.path.as_path())).collect();
    let exact: Vec<Vec<PathBuf>> =
        q.exact.iter().map(|g| g.iter().filter_map(|k| by_key.get(k).map(|p| p.to_path_buf())).collect()).collect();
    let hints = Hints::derive(&app.coll, &app.bursts, |_, r| q.features.get(&canonical_key(&r.path)).map(|f| f.as_ref()), &exact, q.revision);
    app.quality.hints = hints;
    app.quality.derived_at = Some(Instant::now());
    // The grid repaints its hint plates.
    app.derived.key = app.derived.key.wrapping_add(1);
    true
}
