//! The library calendar (Python `CalendarView`): year ↔ month ↔ day.
//!
//! The index is built on a worker; pages are painted by
//! `ui::calendar_stage`; a day opens as a read-only grid of that day's
//! photos.  Every page change animates — a slide for prev/next, a zoom into
//! or out of a cell for year ↔ month ↔ day — and input arriving mid-animation
//! starts from the frame on screen, so rapid navigation chains smoothly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, column, container, row, stack, text};
use iced::{Alignment, Element, Length, Padding, Rectangle, Size, Task};

use super::kit;
use super::style;
use super::{App, Message, View};
use crate::bus::{BgEvent, Bus};
use crate::imaging::thumbs::{OWNER_CALENDAR, PRIORITY_PREFETCH, PRIORITY_VISIBLE, ThumbCache, ThumbWorkers};
use crate::model::collection::{Collection, RecordId};
use crate::model::filter;
use crate::model::library_cache::{self, Store};
use crate::model::library_index::{self, LibraryIndex, MONTH_ABBR, MONTH_NAME, month_shift};
use crate::model::record::PhotoRecord;
use crate::settings::Settings;
use crate::ui::calendar_stage::{self, CalendarStage, Kind, Page, Scene, StageEvent};
use crate::ui::grid::{self, Grid, GridData, GridEvent, Section};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme as t;
use crate::util::plural;

const SLIDE_MS: f32 = 240.0;
const ZOOM_MS: f32 = 270.0;
const DAY_WAIT: Duration = Duration::from_millis(160);
const DAY_CACHE: usize = 8;
const SETTLE: Duration = Duration::from_millis(80);
/// Coming back to the calendar within this long doesn't check the disk again.
const RECHECK: Duration = Duration::from_secs(5);
/// A day waits for up to this many EXIF reads before it is shown; a day
/// needing more shows at once and settles its order as the reads land.
const DAY_READ_NOW: usize = 200;

/// What an index job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Show the saved tree, if there is one.  Reads only the cache.
    Load,
    /// Show the saved tree, then re-read the folders whose mtime moved.
    Check,
    /// Re-read every folder (Refresh).
    Deep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Year,
    Month,
    Day,
}

/// One opened day: its records as a collection, with pairs detected.
pub struct DayData {
    pub coll: Arc<Collection>,
    pub sections: Vec<Section>,
    pub ids: Vec<RecordId>,
}

struct Transition {
    old: Scene,
    new: Scene,
    kind: Kind,
    start: Instant,
    ms: f32,
}

impl Transition {
    fn t(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.ms).clamp(0.0, 1.0)
    }
}

struct Pending {
    date: NaiveDate,
    kind: Kind,
    deadline: Instant,
}

pub struct Calendar {
    pub root: Option<PathBuf>,
    pub index: Option<Arc<LibraryIndex>>,
    /// The library's saved tree and capture times.
    store: Arc<Mutex<Store>>,
    /// The disk should be checked for changes.
    stale: bool,
    /// The next check should re-read every folder.
    deep_wanted: bool,
    /// When a check last confirmed the index.
    checked_at: Option<Instant>,
    active: bool,
    job: u64,
    running: Option<Scan>,
    cancel: Option<Arc<AtomicBool>>,
    indexing: bool,
    /// Bumped when loaded days go stale; day loads started before are ignored.
    generation: u64,
    /// The day on screen changed on disk; reload it, keeping it up meanwhile.
    reload_shown: Option<NaiveDate>,
    progress: usize,
    error: Option<String>,
    pub mode: Mode,
    pub year: i32,
    pub month: u32,
    pub day: Option<NaiveDate>,
    position_restored: bool,
    days: Vec<(NaiveDate, Arc<DayData>)>,
    requested: HashSet<NaiveDate>,
    pending: Option<Pending>,
    transition: Option<Transition>,
    pub thumbs: ThumbCache,
    pub stage_size: Size,
    day_thumb: f32,
    day_selection: HashSet<RecordId>,
    day_collapsed: HashSet<NaiveDate>,
    day_scroll_seq: u64,
    prefetch_at: Option<Instant>,
    bus: crate::bus::Bus,
    hovered: Option<NaiveDate>,
    /// A day opened from search or a person's photos (Open day ›): its
    /// back button returns there, and the calendar goes back to where it
    /// was.
    from_search: Option<Position>,
    /// Where that back button returns, and what it says ("Search",
    /// "Person A").
    back_to: (View, String),
    /// The photo to select once the day's records are in.
    pick: Option<PathBuf>,
    /// Bring the selected photo into view (bumped with it).
    day_reveal: (u64, Option<RecordId>),
}

/// Where the calendar was: its page and day.
#[derive(Debug, Clone, Copy)]
struct Position {
    mode: Mode,
    year: i32,
    month: u32,
    day: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Stage(StageEvent),
    Grid(GridEvent),
    Back,
    /// Back to the search (or the person's photos) the day was opened
    /// from.
    BackToSearch,
    Prev,
    Next,
    SetMode(Mode),
    Today,
    Refresh,
    OpenFolder,
    MessageAction,
}

impl Calendar {
    pub fn new(s: &Settings, bus: crate::bus::Bus) -> Self {
        let today = Local::now().date_naive();
        let mut c = Calendar {
            root: None,
            index: None,
            store: Arc::new(Mutex::new(Store::new(PathBuf::new()))),
            stale: true,
            deep_wanted: false,
            checked_at: None,
            active: false,
            job: 0,
            running: None,
            cancel: None,
            indexing: false,
            generation: 0,
            reload_shown: None,
            progress: 0,
            error: None,
            mode: Mode::Month,
            year: today.year(),
            month: today.month(),
            day: None,
            position_restored: false,
            days: Vec::new(),
            requested: HashSet::new(),
            pending: None,
            transition: None,
            thumbs: ThumbCache::new(OWNER_CALENDAR, super::CALENDAR_CACHE_BYTES),
            stage_size: Size::ZERO,
            day_thumb: 180.0,
            day_selection: HashSet::new(),
            day_collapsed: HashSet::new(),
            day_scroll_seq: 0,
            prefetch_at: None,
            bus,
            hovered: None,
            from_search: None,
            back_to: (View::Search, "Search".into()),
            pick: None,
            day_reveal: (0, None),
        };
        if (1..=12).contains(&s.calendar_month) && s.calendar_year > 1900 && s.calendar_year < 3000 {
            c.year = s.calendar_year;
            c.month = s.calendar_month;
            c.position_restored = true;
        }
        match s.calendar_mode.as_str() {
            "year" => c.mode = Mode::Year,
            "month" => c.mode = Mode::Month,
            _ => {}
        }
        c
    }

    pub fn save_state(&self) -> (String, i32, u32) {
        let mode = match self.mode {
            Mode::Year => "year",
            _ => "month",
        };
        (mode.into(), self.year, self.month)
    }

    /// The library is being (re)read.
    pub fn is_indexing(&self) -> bool {
        self.indexing
    }

    pub fn animating(&self) -> bool {
        self.transition.is_some() || self.pending.is_some() || self.prefetch_at.is_some()
    }

    pub fn set_library_root(&mut self, root: Option<PathBuf>, workers: &ThumbWorkers) {
        if root == self.root {
            return;
        }
        self.cancel_worker();
        self.store = Arc::new(Mutex::new(Store::new(root.clone().unwrap_or_default())));
        self.root = root;
        self.index = None;
        self.stale = true;
        self.checked_at = None;
        self.generation += 1;
        self.days.clear();
        self.requested.clear();
        self.thumbs.clear(workers);
        if self.mode == Mode::Day {
            self.mode = Mode::Month;
        }
        if !self.position_restored {
            let today = Local::now().date_naive();
            self.year = today.year();
            self.month = today.month();
        }
        self.transition = None;
        if self.root.is_some() {
            // Out of view, just have the saved library ready to show.
            self.start_index(if self.active { Scan::Check } else { Scan::Load });
        }
    }

    /// One-line state summary for the test harness.
    pub fn debug_state(&self) -> String {
        let shown = self.day.and_then(|d| self.day_data(d)).map(|data| {
            let mut v: Vec<String> = data
                .coll
                .iter()
                .map(|r| format!("{}@{}", crate::util::file_name(&r.path), r.capture_time.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or("-".into())))
                .collect();
            v.sort();
            v.join(",")
        });
        format!(
            "root={:?} indexed={} days={} files={} bytes={} indexing={} running={:?} stale={} checked={} error={:?} mode={:?} {}-{:02} day={:?} loaded={} transition={} records=[{}]",
            self.root,
            self.index.is_some(),
            self.index.as_ref().map(|i| i.len()).unwrap_or(0),
            self.index.as_ref().map(|i| i.total_files).unwrap_or(0),
            self.index.as_ref().map(|i| i.days.values().flat_map(|d| &d.files).map(|f| f.size).sum::<u64>()).unwrap_or(0),
            self.indexing,
            self.running,
            self.stale,
            self.checked_at.is_some(),
            self.error,
            self.mode,
            self.year,
            self.month,
            self.day,
            self.days.len(),
            self.transition.is_some(),
            shown.unwrap_or_default(),
        )
    }

    pub fn activate(&mut self, _workers: &ThumbWorkers) {
        self.active = true;
        if self.root.is_some() && (self.stale || self.checked_at.is_none_or(|t| t.elapsed() >= RECHECK)) {
            match self.running {
                None => self.start_index(self.wanted_scan()),
                // Only loading the saved tree: check the disk after it.
                Some(Scan::Load) => self.stale = true,
                Some(_) => {}
            }
        }
        self.prefetch_at = Some(Instant::now() + SETTLE);
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Search builds its results from the library's index: read it, if
    /// nothing has yet (the calendar may never have been opened).
    pub fn want_index(&mut self) {
        if self.root.is_some() && self.index.is_none() && self.running.is_none() {
            self.start_index(self.wanted_scan());
        }
    }

    /// Open day › from search: `day`, with the photo at `path` selected and
    /// in view.  Shown at once (no zoom: it isn't reached from a month).
    pub fn open_from_search(&mut self, day: NaiveDate, path: PathBuf) {
        self.open_from(day, path, View::Search, "Search".into());
    }

    /// Open day › from `view` (search, a person's photos): its back button
    /// says `label` and returns there.
    pub fn open_from(&mut self, day: NaiveDate, path: PathBuf, view: View, label: String) {
        self.back_to = (view, label);
        if self.from_search.is_none() {
            self.from_search = Some(Position { mode: self.mode, year: self.year, month: self.month, day: self.day });
        }
        self.pending = None;
        self.transition = None;
        self.mode = Mode::Day;
        self.day = Some(day);
        self.year = day.year();
        self.month = day.month();
        self.after_change();
        self.pick = Some(path);
        self.apply_pick();
    }

    /// The day page was opened from search or a person's photos (its back
    /// button says so).
    pub fn is_from_search(&self) -> bool {
        self.from_search.is_some() && self.mode == Mode::Day
    }

    /// Where the day page's back button returns.
    pub fn back_view(&self) -> View {
        self.back_to.0
    }

    /// Leaving the calendar, or its search day: it goes back to the page
    /// it showed before.
    pub fn leave_search_day(&mut self) {
        if let Some(p) = self.from_search.take() {
            self.pick = None;
            self.transition = None;
            self.pending = None;
            (self.mode, self.year, self.month, self.day) = (p.mode, p.year, p.month, p.day);
            if self.mode == Mode::Day {
                self.mode = Mode::Month;
            }
        }
    }

    /// Select the photo asked for, once its day is loaded.
    fn apply_pick(&mut self) {
        let (Some(path), Some(d)) = (self.pick.clone(), self.day) else { return };
        let Some(data) = self.day_data(d).cloned() else { return };
        self.pick = None;
        let Some(id) = data.coll.id_of(&path) else { return };
        // The grid shows a pair once: whichever half is its tile.
        let tile = if data.ids.contains(&id) { Some(id) } else { data.coll.find_pair(id).filter(|p| data.ids.contains(p)) };
        if let Some(t) = tile {
            self.day_selection = [t].into();
            self.day_reveal = (self.day_reveal.0 + 1, Some(t));
        }
    }

    /// The library changed on disk (e.g. after an export): look for what changed.
    pub fn mark_stale(&mut self) {
        self.stale = true;
        self.run_queued();
    }

    /// Re-read the whole library (Refresh, F5): also catches files rewritten
    /// in place, which a folder's mtime doesn't show.
    pub fn refresh(&mut self) {
        self.stale = true;
        self.deep_wanted = true;
        self.run_queued();
    }

    pub fn shutdown(&mut self) {
        self.cancel_worker();
    }

    fn wanted_scan(&self) -> Scan {
        if self.deep_wanted { Scan::Deep } else { Scan::Check }
    }

    /// Start the check asked for while a job ran or the calendar was hidden.
    fn run_queued(&mut self) {
        if self.active && self.stale && self.cancel.is_none() && self.root.is_some() {
            self.start_index(self.wanted_scan());
        }
    }

    fn cancel_worker(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.running = None;
        self.indexing = false;
    }

    /// The job ended.  A check or deep read that succeeded vouches for the index.
    fn finish_job(&mut self, succeeded: bool) {
        self.cancel = None;
        self.indexing = false;
        if succeeded && matches!(self.running, Some(Scan::Check | Scan::Deep)) {
            self.checked_at = Some(Instant::now());
        }
        self.running = None;
    }

    fn start_index(&mut self, kind: Scan) {
        let Some(root) = self.root.clone() else {
            self.stale = true;
            return;
        };
        if kind != Scan::Load {
            self.stale = false;
        }
        if kind == Scan::Deep {
            self.deep_wanted = false;
        }
        self.job += 1;
        let job = self.job;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.running = Some(kind);
        self.indexing = true;
        self.error = None;
        self.progress = 0;
        let bus = self.bus.clone();
        let store = self.store.clone();
        let have_index = self.index.is_some();
        std::thread::Builder::new()
            .name("library-index".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    index_job(&root, kind, have_index, &store, &cancel, job, &bus)
                }));
                let failure = match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(message)) => Some(message),
                    Err(_) => Some("indexing failed".into()),
                };
                if let Some(message) = failure {
                    if !cancel.load(Ordering::Relaxed) {
                        let _ = bus.unbounded_send(BgEvent::IndexError { job, message });
                    }
                }
            })
            .ok();
    }

    pub fn on_progress(&mut self, job: u64, count: usize) {
        if job == self.job {
            self.progress = count;
        }
    }

    /// Returns status text to show when the calendar still has an index to show.
    pub fn on_index_error(&mut self, job: u64, message: String) -> Option<String> {
        if job != self.job {
            return None;
        }
        self.finish_job(false);
        // Tried again on the next visit, not straight away.
        self.stale = true;
        self.error = Some(message.clone());
        self.has_index().then(|| format!("Couldn't check the library for changes. {message}"))
    }

    /// A job ended without a new index.
    pub fn on_index_idle(&mut self, job: u64, checked: bool) {
        if job != self.job {
            return;
        }
        self.finish_job(checked);
        self.run_queued();
    }

    /// A new index (from the saved tree, or after a change on disk).
    /// Returns the status text to show.
    pub fn on_index_ready(&mut self, job: u64, index: Arc<LibraryIndex>, done: bool) -> Option<String> {
        if job != self.job || Some(&index.root) != self.root.as_ref() {
            return None;
        }
        if done {
            self.finish_job(true);
        }
        self.error = None;
        let first = self.index.is_none();
        if first && !self.position_restored {
            if let Some(latest) = index.latest_day() {
                self.year = latest.year();
                self.month = latest.month();
            }
        }
        if let Some(old) = self.index.take() {
            self.forget_changed_days(&old, &index);
        }
        if self.mode == Mode::Day && !self.day.is_some_and(|d| index.get(d).is_some()) {
            self.mode = Mode::Month;
        }
        self.index = Some(index);
        self.prefetch_at = Some(Instant::now() + SETTLE);
        // Reload what is on screen, or about to be, if it was dropped.
        if let Some(d) = self.reload_shown.take() {
            self.request_day(d, true);
        }
        if self.mode == Mode::Day {
            if let Some(d) = self.day {
                self.request_day(d, false);
            }
        }
        if let Some(d) = self.pending.as_ref().map(|p| p.date) {
            self.request_day(d, false);
        }
        self.run_queued();
        Some(self.status_text())
    }

    /// `new` replaced `old`: drop loaded days whose files changed.  The day
    /// on screen stays up until its new records land.
    fn forget_changed_days(&mut self, old: &LibraryIndex, new: &LibraryIndex) {
        let changed = |d: NaiveDate| !matches!((old.get(d), new.get(d)), (Some(a), Some(b)) if a.files == b.files);
        if !self.days.iter().any(|(d, _)| changed(*d)) && !self.requested.iter().any(|d| changed(*d)) {
            return;
        }
        let shown = if self.mode == Mode::Day { self.day } else { None };
        self.reload_shown = shown.filter(|&d| changed(d) && new.get(d).is_some());
        self.days.retain(|(d, _)| !changed(*d) || Some(*d) == self.reload_shown);
        // Loads under way read the old files.
        self.generation += 1;
        self.requested.clear();
    }

    pub fn status_text(&self) -> String {
        if self.root.is_none() {
            return "No library folder set".into();
        }
        let Some(ix) = &self.index else { return "Reading the library…".into() };
        if ix.is_empty() {
            return "The library has no photos yet".into();
        }
        let years = ix.years();
        format!(
            "The library holds {} over {}, {}–{}",
            plural(ix.total_shots as u64, "photo"),
            plural(ix.len() as u64, "day"),
            years.first().copied().unwrap_or(0),
            years.last().copied().unwrap_or(0)
        )
    }

    fn has_index(&self) -> bool {
        self.index.as_ref().is_some_and(|ix| !ix.is_empty())
    }

    pub fn day_data(&self, d: NaiveDate) -> Option<&Arc<DayData>> {
        self.days.iter().find(|(k, _)| *k == d).map(|(_, v)| v)
    }

    /// A day loaded on a worker (see `request_day`).
    pub fn on_day_records(&mut self, date: NaiveDate, generation: u64, data: Arc<DayData>, complete: bool) {
        if generation != self.generation {
            return;
        }
        if complete {
            self.requested.remove(&date);
        }
        self.days.retain(|(d, _)| *d != date);
        self.days.push((date, data));
        while self.days.len() > DAY_CACHE {
            self.days.remove(0);
        }
        if self.pending.as_ref().is_some_and(|p| p.date == date) {
            self.flush_pending();
        }
        if self.pick.is_some() && self.day == Some(date) && self.mode == Mode::Day {
            self.apply_pick();
        }
    }

    /// Load a day's records on a worker.  `force` reloads one already loaded.
    fn request_day(&mut self, d: NaiveDate, force: bool) {
        if self.requested.contains(&d) || (!force && self.day_data(d).is_some()) {
            return;
        }
        let Some(day) = self.index.as_ref().and_then(|ix| ix.get(d)) else { return };
        let files = day.files.clone();
        let (bus, store, generation) = (self.bus.clone(), self.store.clone(), self.generation);
        self.requested.insert(d);
        std::thread::Builder::new()
            .name("library-day".into())
            .spawn(move || {
                let send = |records: Vec<PhotoRecord>, complete: bool| {
                    let data = Arc::new(day_data(d, records));
                    let _ = bus.unbounded_send(BgEvent::DayRecords { date: d, generation, data, complete });
                };
                let (mut records, missing) = library_index::day_records(&files, &store.lock());
                if missing.len() > DAY_READ_NOW {
                    send(records.clone(), false);
                }
                if !missing.is_empty() {
                    let times = library_cache::read_capture_times(&missing.iter().map(|&i| &files[i]).collect::<Vec<_>>());
                    let mut st = store.lock();
                    for (&i, t) in missing.iter().zip(times) {
                        st.set_time(&files[i], t);
                        records[i].capture_time = t;
                    }
                    st.save();
                }
                send(records, true);
            })
            .ok();
    }

    // ── scenes and transitions ──────────────────────────────────────────

    fn page(&self) -> Page {
        match self.mode {
            Mode::Year => Page::Year(self.year),
            Mode::Month => Page::Month(self.year, self.month),
            Mode::Day => Page::Day(self.day.unwrap_or_else(|| Local::now().date_naive())),
        }
    }

    /// What is on screen right now (a running transition is frozen as-is).
    fn current_scene(&self, now: Instant) -> Scene {
        match &self.transition {
            Some(tr) => Scene::Transition {
                old: Box::new(collapse(tr.old.clone(), now)),
                new: Box::new(tr.new.clone()),
                kind: tr.kind,
                t: paint::out_cubic(tr.t(now)),
            },
            None => Scene::Page(self.page()),
        }
    }

    fn can_animate(&self) -> bool {
        self.active && self.has_index() && self.stage_size.width > 0.0
    }

    /// Apply a navigation change, animating between before and after.
    fn change(&mut self, apply: impl FnOnce(&mut Calendar), kind: Kind) {
        self.pending = None;
        if !self.can_animate() {
            apply(self);
            self.after_change();
            return;
        }
        let now = Instant::now();
        let old = self.current_scene(now);
        apply(self);
        self.after_change();
        let new = Scene::Page(self.page());
        let ms = match kind {
            Kind::Slide(_) => SLIDE_MS,
            Kind::Zoom { .. } => ZOOM_MS,
        };
        self.transition = Some(Transition { old, new, kind, start: now, ms });
    }

    fn after_change(&mut self) {
        if self.mode == Mode::Day {
            if let Some(d) = self.day {
                self.request_day(d, false);
            }
            self.day_selection.clear();
            self.day_scroll_seq += 1;
        }
        self.prefetch_at = Some(Instant::now() + SETTLE);
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(tr) = &self.transition {
            if tr.t(now) >= 1.0 {
                self.transition = None;
            }
        }
        if let Some(p) = &self.pending {
            if now >= p.deadline {
                self.flush_pending();
            }
        }
    }

    fn flush_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            let date = p.date;
            self.change(
                move |c| {
                    c.mode = Mode::Day;
                    c.day = Some(date);
                    c.year = date.year();
                    c.month = date.month();
                },
                p.kind,
            );
        }
    }

    fn step(&mut self, delta: i32) {
        if !self.has_index() {
            return;
        }
        match self.mode {
            Mode::Month => self.change(move |c| (c.year, c.month) = month_shift(c.year, c.month, delta), Kind::Slide(delta)),
            Mode::Year => self.change(move |c| c.year += delta, Kind::Slide(delta)),
            Mode::Day => {
                let target = self.day.and_then(|d| self.index.as_ref().and_then(|ix| ix.adjacent_day(d, delta)));
                if let Some(t) = target {
                    self.go_day(t, Kind::Slide(delta));
                }
            }
        }
    }

    fn go_today(&mut self) {
        if self.index.is_none() || self.mode == Mode::Day {
            return;
        }
        let today = Local::now().date_naive();
        if self.mode == Mode::Year {
            if today.year() != self.year {
                let d = if today.year() > self.year { 1 } else { -1 };
                self.change(move |c| c.year = today.year(), Kind::Slide(d));
            }
            return;
        }
        if (today.year(), today.month()) != (self.year, self.month) {
            self.jump_to_month(today.year(), today.month());
        }
    }

    fn jump_to_month(&mut self, year: i32, month: u32) {
        let d = if (year, month) > (self.year, self.month) { 1 } else { -1 };
        self.change(move |c| (c.year, c.month) = (year, month), Kind::Slide(d));
    }

    fn jump_nearest(&mut self) {
        let Some(ix) = self.index.clone() else { return };
        let r = if self.mode == Mode::Year {
            NaiveDate::from_ymd_opt(self.year, 7, 1)
        } else {
            NaiveDate::from_ymd_opt(self.year, self.month, 15)
        };
        let Some(near) = r.and_then(|r| ix.nearest_day(r)) else { return };
        if self.mode == Mode::Year {
            let d = if near.year() > self.year { 1 } else { -1 };
            self.change(move |c| c.year = near.year(), Kind::Slide(d));
        } else {
            self.jump_to_month(near.year(), near.month());
        }
    }

    fn zoom_out_to_year(&mut self) {
        let rect = calendar_stage::year_layout(self.stage_size, self.year).month_rect(self.month);
        self.change(|c| c.mode = Mode::Year, Kind::Zoom { rect, zoom_in: false });
    }

    fn zoom_into_month(&mut self, month: u32, rect: Rectangle) {
        self.change(move |c| (c.mode, c.month) = (Mode::Month, month), Kind::Zoom { rect, zoom_in: true });
    }

    fn set_zoom_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        match mode {
            Mode::Year => self.zoom_out_to_year(),
            _ => {
                let rect = calendar_stage::year_layout(self.stage_size, self.year).month_rect(self.month);
                self.zoom_into_month(self.month, rect);
            }
        }
    }

    fn hovered_day_month(&self) -> Option<NaiveDate> {
        self.hovered.filter(|d| d.month() == self.month && self.index.as_ref().is_some_and(|ix| ix.get(*d).is_some()))
    }

    fn zoom_step(&mut self, direction: i32) {
        if !self.has_index() {
            return;
        }
        if direction < 0 {
            match self.mode {
                Mode::Month => self.zoom_out_to_year(),
                Mode::Day => self.go_back(),
                Mode::Year => {}
            }
        } else if self.mode == Mode::Year {
            let rect = calendar_stage::year_layout(self.stage_size, self.year).month_rect(self.month);
            self.zoom_into_month(self.month, rect);
        } else if self.mode == Mode::Month {
            if let Some(d) = self.hovered_day_month() {
                self.open_day_from(d, calendar_stage::month_layout(self.stage_size, d.year(), d.month()).cell_rect(d));
            }
        }
    }

    fn go_back(&mut self) {
        if self.mode != Mode::Day {
            return;
        }
        // From search, the day page has no month behind it to zoom out to.
        self.from_search = None;
        let Some(d) = self.day else { return };
        self.pending = None;
        let rect = calendar_stage::month_layout(self.stage_size, d.year(), d.month()).cell_rect(d);
        self.change(move |c| (c.mode, c.year, c.month) = (Mode::Month, d.year(), d.month()), Kind::Zoom { rect, zoom_in: false });
    }

    fn open_day_from(&mut self, d: NaiveDate, rect: Rectangle) {
        self.go_day(d, Kind::Zoom { rect, zoom_in: true });
    }

    fn go_day(&mut self, d: NaiveDate, kind: Kind) {
        if self.day_data(d).is_some() || !self.can_animate() {
            self.change(
                move |c| {
                    c.mode = Mode::Day;
                    c.day = Some(d);
                    c.year = d.year();
                    c.month = d.month();
                },
                kind,
            );
            return;
        }
        // Hold the transition briefly so the zoom reveals a filled grid.
        self.request_day(d, false);
        self.pending = Some(Pending { date: d, kind, deadline: Instant::now() + DAY_WAIT });
    }

    /// Thumbnails for the visible month first, then its neighbours.
    fn prefetch(&mut self, workers: &ThumbWorkers) {
        let Some(ix) = self.index.clone() else { return };
        if self.mode != Mode::Month {
            return;
        }
        self.thumbs.clear_queue(workers);
        let req = |y: i32, m: u32, prio: i32, thumbs: &mut ThumbCache| {
            for day in ix.days_in_month(y, m) {
                let n = if day.shot_count >= 4 { 4 } else { 1 };
                for fe in day.preview_entries(n) {
                    thumbs.request(workers, &fe.record(), prio);
                }
            }
        };
        req(self.year, self.month, PRIORITY_VISIBLE, &mut self.thumbs);
        for delta in [1, -1] {
            let (y, m) = month_shift(self.year, self.month, delta);
            req(y, m, PRIORITY_PREFETCH, &mut self.thumbs);
        }
    }
}

/// A day's grid data from its records (built on the loading thread: a big
/// day takes a frame or more).
fn day_data(date: NaiveDate, records: Vec<PhotoRecord>) -> DayData {
    // The folder is the authority on which day a file belongs to; a file
    // with no EXIF time keeps its mtime's time of day on that date.
    let mut coll = Collection::new();
    for mut r in records {
        if r.capture_time.is_none() && r.modified.local.date() != date {
            r.capture_time = Some(NaiveDateTime::new(date, r.modified.local.time()));
        }
        coll.add(r);
    }
    coll.build_pairs();
    let state = filter::FilterState::default();
    let sorted = filter::visible_sorted(&coll, &state);
    let ids = filter::dedup_pairs(&sorted, &coll);
    let sections = filter::group_by_date(&ids, &coll, &state)
        .into_iter()
        .map(|g| Section { label: super::fmt_section_date(g.date), date: g.date, photos: g.ids.len(), ids: g.ids })
        .collect();
    DayData { coll: Arc::new(coll), sections, ids }
}

/// The body of an index job, on its own thread.  Shows the saved tree first
/// when the calendar has nothing yet, then (unless only loading) checks the
/// disk and sends a new index only if something changed.
fn index_job(root: &Path, kind: Scan, have_index: bool, store: &Mutex<Store>, cancel: &AtomicBool, job: u64, bus: &Bus) -> Result<(), String> {
    let send = |ev: BgEvent| {
        if !cancel.load(Ordering::Relaxed) {
            let _ = bus.unbounded_send(ev);
        }
    };
    let previous = {
        let mut st = store.lock();
        st.load();
        st.tree.clone()
    };
    let mut shown = have_index;
    if !have_index {
        if let Some(index) = previous.as_ref().and_then(|tree| library_index::index_from_tree(root, tree, store, cancel)) {
            send(BgEvent::IndexReady { job, index: Arc::new(index), done: kind == Scan::Load });
            shown = true;
        }
    }
    if kind == Scan::Load {
        if !shown {
            send(BgEvent::IndexIdle { job, checked: false });
        }
        return Ok(());
    }
    let report = |n: usize| {
        let _ = bus.unbounded_send(BgEvent::IndexProgress { job, count: n });
    };
    let Some(scanned) = library_cache::scan(root, previous.as_deref(), kind == Scan::Deep, cancel, &report)? else {
        return Ok(());
    };
    if scanned.changed || !shown {
        let tree = Arc::new(scanned.tree);
        let Some(index) = library_index::index_from_tree(root, &tree, store, cancel) else { return Ok(()) };
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        {
            let mut st = store.lock();
            st.set_tree(tree);
            st.save();
        }
        send(BgEvent::IndexReady { job, index: Arc::new(index), done: true });
    } else {
        if scanned.reread > 0 {
            // Same contents, new mtimes: keep them, so next time trusts them.
            let mut st = store.lock();
            st.set_tree(Arc::new(scanned.tree));
            st.save();
        }
        send(BgEvent::IndexIdle { job, checked: true });
    }
    Ok(())
}

/// Keep nested transitions shallow: an old scene that is itself a
/// transition collapses to whichever side dominates.
fn collapse(scene: Scene, _now: Instant) -> Scene {
    match scene {
        Scene::Transition { old, new, kind, t } => match *old {
            Scene::Transition { .. } => {
                if t > 0.5 {
                    *new
                } else {
                    Scene::Transition { old: Box::new(collapse(*old, _now)), new, kind, t }
                }
            }
            o => Scene::Transition { old: Box::new(o), new, kind, t },
        },
        s => s,
    }
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let workers = &app.workers;
    let cal = &mut app.calendar;
    match msg {
        Msg::Stage(ev) => match ev {
            StageEvent::Step(d) => cal.step(d),
            StageEvent::Zoom(dir, pos) => {
                if dir < 0 {
                    if cal.mode == Mode::Month {
                        cal.zoom_out_to_year();
                    }
                } else if cal.mode == Mode::Year {
                    let lay = calendar_stage::year_layout(cal.stage_size, cal.year);
                    let month = lay.month_at(pos).unwrap_or(cal.month);
                    cal.zoom_into_month(month, lay.month_rect(month));
                } else if cal.mode == Mode::Month {
                    let lay = calendar_stage::month_layout(cal.stage_size, cal.year, cal.month);
                    if let Some((d, r)) = lay.cells.iter().find(|(_, r)| r.contains(pos)) {
                        if d.month() == cal.month && cal.index.as_ref().is_some_and(|ix| ix.get(*d).is_some()) {
                            cal.open_day_from(*d, *r);
                        }
                    }
                }
            }
            StageEvent::DayActivated(d, r) => cal.open_day_from(d, r),
            StageEvent::MonthActivated(m, r) => cal.zoom_into_month(m, r),
            StageEvent::MonthJump(y, m) => cal.jump_to_month(y, m),
            StageEvent::JumpNearest => cal.jump_nearest(),
            StageEvent::Need(entries) => {
                for fe in entries {
                    cal.thumbs.request(workers, &fe.record(), PRIORITY_VISIBLE);
                }
            }
            StageEvent::Resized(s) => cal.stage_size = s,
            StageEvent::Hover(d) => {
                cal.hovered = d;
                return Task::none();
            }
        },
        Msg::Grid(ev) => return on_day_grid(app, ev),
        Msg::BackToSearch => {
            let to = cal.back_to.0;
            return app.switch_view(to);
        }
        Msg::Back => cal.go_back(),
        Msg::Prev => cal.step(-1),
        Msg::Next => cal.step(1),
        Msg::SetMode(m) => cal.set_zoom_mode(m),
        Msg::Today => cal.go_today(),
        Msg::Refresh => cal.refresh(),
        Msg::OpenFolder => {
            let folder = cal.day.and_then(|d| cal.index.as_ref().and_then(|ix| ix.get(d)).and_then(|day| day.folder.clone()));
            if let Some(f) = folder {
                let t = app.start_scan(f);
                let t2 = app.switch_view(View::Grid);
                return Task::batch([t, t2]);
            }
        }
        Msg::MessageAction => {
            if cal.root.is_none() {
                return app.pick_folder(super::FolderTarget::Library);
            }
            cal.refresh();
        }
    }
    let status = app.calendar.status_text();
    if app.view == View::Calendar {
        return app.show_status(status, 0);
    }
    Task::none()
}

fn on_day_grid(app: &mut App, ev: GridEvent) -> Task<Message> {
    let cal = &mut app.calendar;
    let Some(d) = cal.day else { return Task::none() };
    let Some(data) = cal.day_data(d).cloned() else { return Task::none() };
    match ev {
        GridEvent::Click { id, modifiers } => {
            if modifiers.control() {
                if !cal.day_selection.remove(&id) {
                    cal.day_selection.insert(id);
                }
            } else {
                cal.day_selection = [id].into();
            }
        }
        GridEvent::Activate(id) => {
            let index = data.ids.iter().position(|&x| x == id).unwrap_or(0);
            let source = super::viewer::Source::Library(data.coll.clone());
            return super::viewer::open(app, source, data.ids.clone(), index, true);
        }
        GridEvent::ClearSelection => cal.day_selection.clear(),
        GridEvent::ToggleCollapse(date) => {
            if !cal.day_collapsed.remove(&date) {
                cal.day_collapsed.insert(date);
            }
        }
        // The library's day grid shows no bursts.
        GridEvent::Chip(_) | GridEvent::SectionLink(_) | GridEvent::FooterLink | GridEvent::SelectDone | GridEvent::Scrolled(_) | GridEvent::ToggleBurst(_) => {}
        GridEvent::ZoomStep(step) => {
            cal.day_thumb = ((cal.day_thumb + step as f32 * 20.0) / 20.0).floor() * 20.0;
            cal.day_thumb = cal.day_thumb.clamp(grid::MIN_SIZE, grid::MAX_SIZE);
        }
        GridEvent::PinchTo(s) => cal.day_thumb = s.clamp(grid::MIN_SIZE, grid::MAX_SIZE),
        GridEvent::Thumbs { visible, nearby, settle } => {
            if settle {
                cal.thumbs.clear_queue(&app.workers);
            }
            for id in visible {
                if let Some(r) = data.coll.get(id) {
                    cal.thumbs.request(&app.workers, r, PRIORITY_VISIBLE);
                }
            }
            for id in nearby {
                if let Some(r) = data.coll.get(id) {
                    cal.thumbs.request(&app.workers, r, PRIORITY_PREFETCH);
                }
            }
        }
    }
    Task::none()
}

pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if mods.control() || mods.alt() {
        return Task::none();
    }
    let cal = &mut app.calendar;
    let c = match key {
        Key::Character(c) => Some(c.to_lowercase()),
        _ => None,
    };
    let c = c.as_deref();
    if matches!(key, Key::Named(Named::F5)) {
        cal.refresh();
        return Task::none();
    }
    if cal.mode == Mode::Day {
        match (key, c) {
            (Key::Named(Named::Escape), _) | (Key::Named(Named::Backspace), _) if cal.is_from_search() => {
                let to = cal.back_to.0;
                return app.switch_view(to);
            }
            (Key::Named(Named::Escape), _) | (Key::Named(Named::Backspace), _) => cal.go_back(),
            (_, Some("[")) => cal.step(-1),
            (_, Some("]")) => cal.step(1),
            _ => {}
        }
        return Task::none();
    }
    match (key, c) {
        (Key::Named(Named::ArrowLeft), _) | (Key::Named(Named::PageUp), _) => cal.step(-1),
        (Key::Named(Named::ArrowRight), _) | (Key::Named(Named::PageDown), _) => cal.step(1),
        (_, Some("t")) | (Key::Named(Named::Home), _) => cal.go_today(),
        (_, Some("-")) | (Key::Named(Named::Escape), _) => cal.zoom_step(-1),
        (_, Some("+")) | (_, Some("=")) | (Key::Named(Named::Enter), _) => cal.zoom_step(1),
        _ => {}
    }
    Task::none()
}

/// Called every frame while animating: prefetch after navigation settles.
impl App {
    pub fn calendar_frame(&mut self) {
        if let Some(at) = self.calendar.prefetch_at {
            if Instant::now() >= at {
                self.calendar.prefetch_at = None;
                let workers = &self.workers;
                self.calendar.prefetch(workers);
            }
        }
    }
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    let cal = &app.calendar;
    let header = header(cal);
    let body: Element<'_, Message> = if !cal.has_index() {
        message_page(cal)
    } else if cal.mode == Mode::Day && cal.transition.is_none() {
        day_grid(app, keyboard_ok)
    } else {
        let now = Instant::now();
        let scene = cal.current_scene(now);
        let sf = app.scale_factor;
        let names = app.show_names;
        CalendarStage::new(scene, cal.index.as_ref(), &cal.thumbs, sf, |e| Message::Calendar(Msg::Stage(e)))
            .day_painter(move |r, bounds, date, alpha| paint_day(cal, r, bounds, date, alpha, sf, names))
            .into()
    };
    let body: Element<'_, Message> = if cal.mode == Mode::Day && cal.transition.is_none() && cal.day.is_some_and(|d| cal.day_data(d).is_none()) {
        let pill = container(kit::caption("Loading photos…")).padding(Padding::from([6.0, 14.0])).style(style::floating());
        stack![body, container(pill).center_x(Length::Fill).padding(Padding { top: 40.0, ..Padding::ZERO })].into()
    } else {
        body
    };
    column![header, kit::hairline(), body].width(Length::Fill).height(Length::Fill).into()
}

/// Day pages inside transitions are painted from the cached day data (the
/// live grid takes over once the transition ends).
fn paint_day(cal: &Calendar, r: &mut paint::Renderer, bounds: Rectangle, date: NaiveDate, alpha: f32, sf: f32, names: bool) {
    let Some(data) = cal.day_data(date) else {
        paint::fill(r, bounds, paint::with_alpha(t::PHOTO_GROUND, alpha));
        return;
    };
    let empty = HashSet::new();
    let gd = GridData {
        sections: &data.sections,
        coll: &data.coll,
        thumbs: &cal.thumbs,
        selection: &empty,
        collapsed: &cal.day_collapsed,
        select_mode: false,
        scale_factor: sf,
        show_names: names,
        bursts: &[],
        moments: &[],
        hints: None,
        restack: None,
        label: grid::CellLabel::Name,
        footer: None,
        links: &[],
    };
    let open = |d: NaiveDate| if cal.day_collapsed.contains(&d) { 0.0 } else { 1.0 };
    let ps = grid::PaintState::snapshot(0.0, cal.day_thumb, alpha, &open);
    grid::paint(r, bounds, &gd, &ps);
}

fn day_grid(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    let cal = &app.calendar;
    let Some(d) = cal.day else { return Space::new().into() };
    let Some(data) = cal.day_data(d) else {
        return container(Space::new()).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into();
    };
    let gd = GridData {
        sections: &data.sections,
        coll: &data.coll,
        thumbs: &cal.thumbs,
        selection: &cal.day_selection,
        collapsed: &cal.day_collapsed,
        select_mode: false,
        scale_factor: app.scale_factor,
        show_names: app.show_names,
        bursts: &[],
        moments: &[],
        hints: None,
        restack: None,
        label: grid::CellLabel::Name,
        footer: None,
        links: &[],
    };
    Grid::new(gd, cal.day_thumb, |e| Message::Calendar(Msg::Grid(e)))
        .keyboard(keyboard_ok)
        .scroll_seq(cal.day_scroll_seq)
        .layout_key(cal.day_scroll_seq)
        .reveal(cal.day_reveal.0, cal.day_reveal.1)
        .into()
}

fn cmsg(m: Msg) -> Option<Message> {
    Some(Message::Calendar(m))
}

fn span(photos: usize, days: usize) -> String {
    if photos == 0 {
        return "No photos".into();
    }
    format!("{} over {}", plural(photos as u64, "photo"), plural(days as u64, "day"))
}

fn header(cal: &Calendar) -> Element<'_, Message> {
    let has = cal.has_index();
    let is_day = has && cal.mode == Mode::Day;
    let ix = cal.index.as_ref();
    let (title, subtitle): (String, String) = if !has {
        (
            "Library calendar".into(),
            cal.root.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "No library folder set".into()),
        )
    } else {
        let ix = ix.expect("index");
        match cal.mode {
            Mode::Year => (cal.year.to_string(), span(ix.year_shot_count(cal.year), ix.year_day_count(cal.year))),
            Mode::Month => {
                let days: Vec<_> = ix.days_in_month(cal.year, cal.month).collect();
                let n: usize = days.iter().map(|d| d.shot_count).sum();
                (format!("{} {}", MONTH_NAME[(cal.month - 1) as usize], cal.year), span(n, days.len()))
            }
            Mode::Day => {
                let d = cal.day.unwrap_or_else(|| Local::now().date_naive());
                let day = ix.get(d);
                (
                    format!("{}, {} {}, {}", d.format("%A"), MONTH_NAME[d.month0() as usize], d.day(), d.year()),
                    day.map(|x| plural(x.shot_count as u64, "photo")).unwrap_or_default(),
                )
            }
        }
    };
    let mut r = row![].spacing(t::S1).align_y(Alignment::Center);
    if is_day && cal.is_from_search() {
        r = r.push(kit::tip(
            kit::icon_text_btn(Glyph::ChevronLeft, cal.back_to.1.clone(), t::INK2, style::secondary(), cmsg(Msg::BackToSearch)),
            if cal.back_to.0 == View::Search { "Back to the search  (Esc)".to_string() } else { format!("Back to {}  (Esc)", cal.back_to.1) },
            true,
        ));
        r = r.push(Space::new().width(t::S2));
    } else if is_day {
        let d = cal.day.unwrap_or_else(|| Local::now().date_naive());
        r = r.push(kit::tip(
            kit::icon_text_btn(Glyph::ChevronLeft, format!("{} {}", MONTH_ABBR[d.month0() as usize], d.year()), t::INK2, style::secondary(), cmsg(Msg::Back)),
            "Back to the month  (Esc)",
            true,
        ));
        r = r.push(Space::new().width(t::S2));
    }
    if has {
        let (prev_ok, next_ok) = if is_day {
            let d = cal.day.unwrap_or_else(|| Local::now().date_naive());
            let ix = ix.expect("index");
            (ix.adjacent_day(d, -1).is_some(), ix.adjacent_day(d, 1).is_some())
        } else {
            (true, true)
        };
        let unit = match cal.mode {
            Mode::Year => "year",
            Mode::Month => "month",
            Mode::Day => "day with photos",
        };
        r = r.push(kit::tip(kit::icon_btn(Glyph::ChevronLeft, t::INK, style::ghost(), if prev_ok { cmsg(Msg::Prev) } else { None }), format!("Previous {unit}  (←)"), true));
        r = r.push(kit::tip(kit::icon_btn(Glyph::ChevronRight, t::INK, style::ghost(), if next_ok { cmsg(Msg::Next) } else { None }), format!("Next {unit}  (→)"), true));
    }
    r = r.push(
        column![text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK), kit::caption(subtitle)]
            .spacing(1)
            .padding(Padding { left: t::S2, ..Padding::ZERO }),
    );
    r = r.push(Space::new().width(Length::Fill));
    if is_day {
        let d = cal.day.unwrap_or_else(|| Local::now().date_naive());
        if let Some(day) = ix.and_then(|ix| ix.get(d)) {
            let count = |n: usize, label: &'static str, tone: iced::Color| {
                row![text(n.to_string()).size(t::T_SMALL).font(paint::MEDIUM).color(tone), kit::caption(label)].spacing(4)
            };
            if day.raw_count > 0 {
                r = r.push(count(day.raw_count, "RAW", t::RAW));
            }
            if day.jpg_count > 0 {
                r = r.push(Space::new().width(t::S2)).push(count(day.jpg_count, "JPG", t::JPG));
            }
            r = r.push(Space::new().width(t::S4));
            let tip = day.folder.as_ref().map(|f| format!("Load {} as the working folder", f.display())).unwrap_or_default();
            r = r.push(kit::tip(kit::btn("Open as working folder", style::primary(), cmsg(Msg::OpenFolder)), tip, true));
        }
    } else if has {
        r = r.push(kit::segmented(vec![
            kit::segment("Month", cal.mode == Mode::Month, None, Message::Calendar(Msg::SetMode(Mode::Month))),
            kit::segment("Year", cal.mode == Mode::Year, None, Message::Calendar(Msg::SetMode(Mode::Year))),
        ]));
        r = r.push(Space::new().width(t::S2));
        r = r.push(kit::tip(kit::btn("Today", style::secondary(), cmsg(Msg::Today)), "Go to today  (T)", true));
    }
    if cal.root.is_some() {
        r = r.push(Space::new().width(t::S1));
        r = r.push(kit::tip(kit::icon_btn(Glyph::Refresh, t::INK2, style::ghost(), cmsg(Msg::Refresh)), "Re-read the library folder  (F5)", true));
    }
    container(r.padding(Padding::from([0.0, t::S3])))
        .height(60)
        .center_y(60)
        .width(Length::Fill)
        .style(style::fill(t::MOUNT))
        .into()
}

fn message_page(cal: &Calendar) -> Element<'_, Message> {
    let (title, body, button_label): (String, String, Option<&str>) = if cal.root.is_none() {
        (
            "Choose your library folder".into(),
            "The calendar shows the photos you have exported to your library, by the day they were taken.".into(),
            Some("Choose library folder…"),
        )
    } else if let Some(err) = &cal.error {
        ("Couldn't read the library".into(), err.clone(), Some("Try again"))
    } else if cal.index.is_none() {
        (
            "Reading the library…".into(),
            if cal.progress > 0 { format!("{} found so far", plural(cal.progress as u64, "file")) } else { String::new() },
            None,
        )
    } else {
        (
            "The library is empty".into(),
            format!(
                "No photos were found in {}.\nExport a shoot (Ctrl+E) to start filling it.",
                cal.root.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            Some("Read it again"),
        )
    };
    let mut col = column![].spacing(t::S2).align_x(Alignment::Center).max_width(460);
    col = col.push(Space::new().height(Length::FillPortion(3)));
    col = col.push(icons::view(Glyph::Calendar, 48.0, t::INK4));
    col = col.push(Space::new().height(t::S2));
    col = col.push(text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK));
    if !body.is_empty() {
        col = col.push(text(body).size(t::T_BODY).color(t::INK2).align_x(iced::alignment::Horizontal::Center));
    }
    if let Some(label) = button_label {
        col = col.push(Space::new().height(t::S2));
        col = col.push(kit::btn(label, style::primary(), cmsg(Msg::MessageAction)));
    }
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(t::GROUND)).into()
}
