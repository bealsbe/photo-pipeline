//! Search the library (Ctrl+F): a page of its own, on the side rail beside
//! the calendar.
//!
//! - **One field.**  The query is the page's title.  While typing, the
//!   list under it offers things search found (objects and scenes, never
//!   people, fursuits or animals), months and years with photos, and
//!   recent searches; its footer teaches `-word` and dates.  ↑ ↓ choose,
//!   Enter or Tab takes one (a thing or a date goes into the field, a
//!   recent search runs), Enter alone searches what was typed and moves to
//!   the results.  Esc closes the list and never leaves the page; the back
//!   button and the rail do.
//! - **The start page** (nothing searched): recent searches, *Things in
//!   your photos* (a click searches one) and ways to search.
//! - **Results best first**, in one list on the photos' grey: the grid of
//!   a library day, read-only, with each photo's date under it (the date,
//!   time and file name on hover).  A burst is one stacked tile at the
//!   rank of its best frame, as in the grid: G or its tab opens it, Shift+G
//!   every one.  ← → move, Space is quick look, Enter the viewer.  A strip
//!   says what was applied besides the words (dates, words left out, a
//!   name it can't search yet).  Dates alone show every photo from them in
//!   the order taken, and the heading, count and end line say so.
//! - **Animal words** (decision 12b): the fursuits the engine left out are
//!   counted at the end, with *Show fursuits too*; shown, they are a list
//!   of their own after the real animals, never mixed in, with *Hide
//!   fursuits* on its heading.  The viewer steps through one list only.
//! - **Open day ›** shows the selected photo's day in the calendar, with
//!   the photo selected; its back button returns here, query and place
//!   kept.
//! - **Before the first index** the page says what indexing costs and
//!   offers it; the download dialog asks once (`search_setup`).  Indexing
//!   then shows in the status bar, failures in a banner.
//!
//! The engine (`crate::search::Engine`, through `search_link`) answers on
//! the bus; the records of a query's hits are built on a worker from the
//! library index the calendar holds, reading no file.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, mouse_area, opaque, pin, row, scrollable, stack, text, text_input};
use iced::{Alignment, Background, Border, Element, Length, Padding, Point, Task, Theme};

use super::kit::{self, tip};
use super::search_link::Link;
use super::search_results::{self as res, Layout, Pick, Results};
use super::style;
use super::{App, Message, View};
use crate::bus::Bus;
use crate::imaging::thumbs::{OWNER_SEARCH, PRIORITY_PREFETCH, PRIORITY_VISIBLE, ThumbCache};
use crate::ml::download::DownloadState;
use crate::ml::models::ModelId;
use crate::ml::{CardFailure, Device};
use crate::model::bursts::RunMark;
use crate::model::collection::RecordId;
use crate::model::library_index::{FileEntry, LibraryIndex, MONTH_ABBR, MONTH_NAME};
use crate::search::{Hit, Parsed, PassKind, Status, Stopped, Thing, estimate};
use crate::settings::Settings;
use crate::ui::grid::{self, CellLabel, Grid, GridData, GridEvent, Restack, Section};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::side_menu::RAIL_W;
use crate::ui::theme::{self as t, alpha};
use crate::util;

/// The field's widget id (focus, select all).
pub const FIELD: &str = "search-field";
/// The field's height: larger than a control, the query being the title.
const FIELD_H: f32 = 36.0;
const HEADER_H: f32 = 60.0;
const SEARCH_CACHE_BYTES: usize = 96 * 1024 * 1024;
/// While the first index runs, a search shown is run again as it reaches
/// more photos, at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(3);

/// The download dialog's two stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// What will be downloaded, its licence, what indexing costs.
    Ask,
    /// Progress, then done, or what failed.
    Progress,
}

pub struct Dialog {
    pub stage: Stage,
    /// Show sources and checksums.
    pub sums: bool,
}

/// A model's download, as last reported.
#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub model: ModelId,
    pub done: u64,
    pub total: u64,
    pub state: DownloadState,
}

/// How much of the library a search sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Everything indexed (or what *Index new exports* lets in).
    Whole,
    /// The first index (or a rebuild) is running: newest days first.
    Growing,
    /// The first index stopped part-way (the card, the share, ×).
    Stopped,
}

pub fn coverage(st: &Status) -> Coverage {
    match &st.pass {
        Some(p) if p.kind != PassKind::NewExports => Coverage::Growing,
        None if st.stopped.is_some() && st.first_index_asked && st.indexed_shots < st.library_shots => Coverage::Stopped,
        _ => Coverage::Whole,
    }
}

pub struct Search {
    pub link: Link,
    /// The engine's status, as last reported.
    pub status: Status,
    pub downloads: Vec<Download>,

    /// What is in the field.
    pub query: String,
    /// The completions list is open under the field.
    pub completions: bool,
    /// What it offers for the query (made on each keystroke, from memory).
    pub offer: res::Offer,
    pub hi: Option<usize>,
    /// *Things in your photos* as the engine last sent them, most photos
    /// first (what is offered is filtered by kind again).
    pub things: Arc<Vec<Thing>>,
    /// Where the back button returns, and what it says.
    pub from: View,
    pub from_label: String,

    seq: u64,
    /// The query in flight, and whether it only refreshes the one shown.
    pub pending: Option<(u64, bool)>,
    /// The words of the results shown (or on their way).
    pub ran: Option<String>,
    /// What was searched has nothing to look for (only a name, which
    /// phase 2 can't search, or only words to leave out): no query ran,
    /// and this says why.
    pub no_words: Option<Parsed>,
    pub results: Option<Arc<Results>>,
    /// The fursuits an animal word left out are shown, as their own list.
    pub fursuits_shown: bool,
    /// Why the last query couldn't run.
    pub error: Option<String>,
    pub layout: Layout,
    /// Bursts stacked unless toggled (G, a tab); Shift+G flips them all.
    stack_all: bool,
    flipped: HashSet<u64>,
    pub selection: HashSet<RecordId>,
    pub anchor: Option<RecordId>,
    pub collapsed: HashSet<NaiveDate>,
    pub scroll: f32,
    pub scroll_seq: u64,
    pub reveal: (u64, Option<RecordId>),
    pub key: u64,
    before: (Vec<Section>, Vec<Vec<Option<RunMark>>>),
    restack_seq: u64,
    restack_focus: Option<(RecordId, RecordId)>,
    pub thumbs: ThumbCache,
    pub thumb_size: f32,
    /// When the shown search last ran, and on how many indexed photos.
    last_run: (Instant, usize),
    /// The line after the last result, and a quieter one under it.
    pub end_line: String,
    end_quiet: &'static str,
    /// "12 fursuits that look like dogs are left out.", while they are.
    pub left_out_line: String,
    /// Thumbnails pending, as last told to the engine.
    busy_sent: usize,

    pub dialog: Option<Dialog>,
    /// The card or library banner was closed (the status bar keeps the way
    /// back).
    pub banner_hidden: bool,
    /// The indexing steps, over the status bar.
    pub steps: bool,
    /// Where the running pass began (what its progress and its end are
    /// measured from).
    pub pass_start: Option<super::search_setup::PassStart>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Input(String),
    Submit,
    Clear,
    /// A completion from the list under the field.
    Take(usize),
    /// A search from the start page (a recent one, an example).
    Run(String),
    CloseList,
    Back,
    OpenDay,
    Grid(GridEvent),
    Built(u64, Option<Arc<Results>>),
    /// Index the library… (opens the download dialog).
    IndexLibrary,
    /// Index the rest, after the first index stopped part-way.
    IndexRest,
    Dialog(DialogMsg),
    /// The status bar's ×: stop indexing, or the download.
    Stop,
    Steps(bool),
    TryCard,
    TryAgain,
    OpenPrefs,
    HideBanner,
}

#[derive(Debug, Clone)]
pub enum DialogMsg {
    Download,
    Cancel,
    Close,
    Sums,
    Retry,
    StopDownload,
    /// Show the download under way (the page, the status bar).
    Show,
}

impl Search {
    pub fn new(bus: Bus, settings: &Settings) -> Search {
        let link = Link::new(bus, settings.search_device, settings.search_index_exports);
        let (status, things) = (link.status(), link.things());
        let pass_start = super::search_setup::pass_start(None, &status);
        Search {
            link,
            status,
            downloads: Vec::new(),
            query: String::new(),
            completions: false,
            offer: res::Offer::default(),
            hi: None,
            things,
            from: View::Grid,
            from_label: "Grid".into(),
            seq: 0,
            pending: None,
            ran: None,
            no_words: None,
            results: None,
            fursuits_shown: false,
            error: None,
            layout: Layout::default(),
            stack_all: true,
            flipped: HashSet::new(),
            selection: HashSet::new(),
            anchor: None,
            collapsed: HashSet::new(),
            scroll: 0.0,
            scroll_seq: 0,
            reveal: (0, None),
            key: 0,
            before: (Vec::new(), Vec::new()),
            restack_seq: 0,
            restack_focus: None,
            thumbs: ThumbCache::new(OWNER_SEARCH, SEARCH_CACHE_BYTES),
            thumb_size: 180.0,
            last_run: (Instant::now(), 0),
            end_line: String::new(),
            end_quiet: "",
            left_out_line: String::new(),
            busy_sent: 0,
            dialog: None,
            banner_hidden: false,
            steps: false,
            pass_start,
        }
    }

    /// Results are on the page (possibly none found).
    pub fn has_results(&self) -> bool {
        self.ran.is_some() && self.results.is_some()
    }

    /// The focused result: the one last clicked or stepped to, while
    /// selected, else a lone selected one.
    pub fn focus(&self) -> Option<RecordId> {
        let sel = &self.selection;
        self.anchor.filter(|a| sel.contains(a)).or_else(|| if sel.len() == 1 { sel.iter().next().copied() } else { None })
    }

    /// Bursts are stacked (Shift+G's state on the page).
    pub fn all_stacked(&self) -> bool {
        self.stack_all
    }

    /// The focused result is in a burst (stacked or open).
    pub fn focused_burst(&self) -> bool {
        self.focus().is_some_and(|id| self.layout.frames_of(id).is_some())
    }

    /// A model download is under way.
    pub fn downloading(&self) -> bool {
        self.downloads.iter().any(|d| !d.state.is_final())
    }

    fn stacked(&self, key: u64) -> bool {
        self.stack_all != self.flipped.contains(&key)
    }

    /// Photos found (frames inside stacks too), the fursuits left out not
    /// counted.
    pub fn photos(&self) -> usize {
        self.results.as_ref().map_or(0, |r| r.found.len())
    }

    /// Lay the results out again (bursts stacked or opened, new results),
    /// keeping the selection.  `animate` slides the tiles to their new
    /// places, holding `focus` still (else the middle row).
    fn relayout(&mut self, animate: bool, focus: Option<RecordId>) {
        let Some(r) = self.results.clone() else {
            self.layout = Layout::default();
            return;
        };
        (self.end_line, self.end_quiet) = res::end_lines(&r);
        self.left_out_line = if r.fursuits.is_empty() { String::new() } else { res::fursuits_left_out(r.fursuits.len(), r.animal.as_deref()) };
        let new = res::layout(&res::parts(&r, self.fursuits_shown), |k| self.stacked(k));
        let old = std::mem::replace(&mut self.layout, new);
        if animate {
            self.before = (old.sections, old.runs);
            self.restack_seq += 1;
            self.restack_focus = focus.and_then(|a| Some((a, self.layout.tile_of(a)?)));
        }
        let (sel, anchor) = res::regather(&self.layout, &self.selection, self.anchor);
        self.selection = sel;
        self.anchor = anchor;
        self.key = self.key.wrapping_add(1);
    }
}

fn m(msg: Msg) -> Message {
    Message::Search(msg)
}

/// The models search needs (phase 1: the search model).
pub fn search_models() -> Vec<ModelId> {
    crate::ml::models::search_models().iter().map(|m| m.id).collect()
}

// ── getting there and back ──────────────────────────────────────────────────

/// Ctrl+F, the rail or the View menu: the page, its field focused with its
/// text selected, so typing replaces the last query.
pub fn open(app: &mut App) -> Task<Message> {
    let t = if app.view != View::Search { app.switch_view(View::Search) } else { Task::none() };
    Task::batch([t, focus_field()])
}

fn focus_field() -> Task<Message> {
    iced::widget::operation::focus::<Message>(FIELD).chain(iced::widget::operation::select_all(FIELD))
}

fn unfocus_field() -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus::<()>()).discard()
}

/// The view is switching to the page from `from` (see `App::switch_view`).
pub fn arriving(app: &mut App, from: View) {
    if from != View::Search {
        app.search.from = from;
        app.search.from_label = back_label(app, from);
    }
    app.search.completions = false;
    app.calendar.want_index();
}

/// What the back button says: the place it returns to, as that place names
/// itself ("Grid", "Jun 2024").
fn back_label(app: &App, from: View) -> String {
    match from {
        View::Calendar => {
            let c = &app.calendar;
            match c.mode {
                super::calendar::Mode::Year => c.year.to_string(),
                _ => format!("{} {}", MONTH_ABBR[(c.month.clamp(1, 12) - 1) as usize], c.year),
            }
        }
        _ => "Grid".into(),
    }
}

pub fn back(app: &mut App) -> Task<Message> {
    let to = app.search.from;
    app.switch_view(if to == View::Search { View::Grid } else { to })
}

/// Open day ›: the selected photo's day in the calendar, with it selected.
pub fn open_day(app: &mut App) -> Task<Message> {
    let s = &app.search;
    let Some((id, r)) = s.focus().zip(s.results.clone()) else { return Task::none() };
    let (Some(day), Some(path)) = (r.day_of(id), r.coll.get(id).map(|x| x.path.clone())) else { return Task::none() };
    let t = app.switch_view(View::Calendar);
    app.calendar.open_from_search(day, path);
    let status = app.calendar.status_text();
    Task::batch([t, app.show_status(status, 0)])
}

// ── searching ───────────────────────────────────────────────────────────────

/// Search for `words` and move to the results.
fn run(app: &mut App, words: String) -> Task<Message> {
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = &mut app.search;
    s.query = words.clone();
    s.completions = false;
    s.hi = None;
    s.no_words = None;
    if words.is_empty() {
        s.ran = None;
        s.results = None;
        s.error = None;
        s.pending = None;
        s.layout = Layout::default();
        s.selection.clear();
        s.anchor = None;
        return Task::none();
    }
    if s.status.indexed_shots == 0 {
        return app.show_status("Nothing is indexed yet, so there is nothing to search.", 5000);
    }
    // Nothing to look for (only a name, which phase 2 can't search, or only
    // words to leave out): nothing runs, and the page says why.  Dates
    // alone do run: every photo from them, in the order taken.
    let parsed = crate::search::parse(&words);
    if parsed.is_empty() && parsed.dates.is_none() {
        s.no_words = Some(parsed);
        s.ran = Some(words);
        s.results = None;
        s.error = None;
        s.pending = None;
        s.layout = Layout::default();
        s.selection.clear();
        s.anchor = None;
        return unfocus_field();
    }
    app.settings.remember_search(&words);
    app.settings.save_in_background();
    app.search.ran = Some(words.clone());
    query(app, words, false);
    unfocus_field()
}

/// Send the query; `refresh` runs the one shown again (indexing reached
/// more photos, the closeness changed), keeping the place.
fn query(app: &mut App, words: String, refresh: bool) {
    let closeness = app.settings.closeness();
    let s = &mut app.search;
    s.seq += 1;
    s.pending = Some((s.seq, refresh));
    s.last_run = (Instant::now(), s.status.indexed_shots);
    s.link.query(s.seq, &words, closeness);
}

/// Run the search shown again (the closeness changed, more is indexed).
pub fn rerun(app: &mut App) {
    if let Some(w) = app.search.ran.clone().filter(|_| app.search.results.is_some()) {
        query(app, w, true);
    }
}

/// A query's answer (`BgEvent::SearchResults`): its hits, the fursuits an
/// animal word left out, and the query as the engine applied it.
pub struct Reply {
    pub hits: Arc<Vec<Hit>>,
    pub error: Option<String>,
    pub understood: Arc<Parsed>,
    pub fursuits: Arc<Vec<Hit>>,
    pub animal: Option<String>,
}

/// A query's answer: build its records on a worker.
pub fn on_results(app: &mut App, seq: u64, reply: Reply) -> Task<Message> {
    if app.search.pending.map(|p| p.0) != Some(seq) {
        return Task::none();
    }
    if let Some(e) = reply.error.clone() {
        let s = &mut app.search;
        s.pending = None;
        s.error = Some(e);
        s.results = None;
        s.layout = Layout::default();
        return Task::none();
    }
    app.search.error = None;
    let Some(index) = app.calendar.index.clone() else {
        return Task::done(m(Msg::Built(seq, None)));
    };
    let words = app.search.ran.clone().unwrap_or_default();
    let work = move || Arc::new(build(seq, words, &reply, &index));
    Task::perform(super::preferences::on_worker("search-results", work), move |r| m(Msg::Built(seq, r)))
}

/// The records for `hits` and the fursuits left out, from the library index
/// (on a worker).
fn build(seq: u64, words: String, reply: &Reply, index: &LibraryIndex) -> Results {
    use std::collections::HashMap;
    // Each day's files by path, made once per day the hits touch.
    let mut days: HashMap<NaiveDate, HashMap<&Path, &FileEntry>> = HashMap::new();
    for h in reply.hits.iter().chain(reply.fursuits.iter()) {
        days.entry(h.day).or_insert_with(|| index.get(h.day).map(|d| d.files.iter().map(|f| (f.path.as_path(), f)).collect()).unwrap_or_default());
    }
    let answer = res::Answer { fursuits: &reply.fursuits, animal: reply.animal.clone(), understood: (*reply.understood).clone() };
    res::build(seq, words, &reply.hits, answer, |d, p| days.get(&d).and_then(|m| m.get(p)).map(|e| (*e).clone()))
}

fn on_built(app: &mut App, seq: u64, built: Option<Arc<Results>>) -> Task<Message> {
    let s = &mut app.search;
    let Some((want, refresh)) = s.pending else { return Task::none() };
    if want != seq {
        return Task::none();
    }
    s.pending = None;
    let Some(r) = built else { return Task::none() };
    // Selection by file, so a refresh (new records) keeps it.
    let old = s.results.take();
    let path_of = |id: RecordId| old.as_ref().and_then(|o| o.coll.get(id)).map(|x| x.path.clone());
    let kept: Vec<_> = s.selection.iter().filter_map(|&id| path_of(id)).collect();
    let anchor = s.anchor.and_then(path_of);
    s.results = Some(r.clone());
    s.selection = kept.iter().filter_map(|p| r.coll.id_of(p)).collect();
    s.anchor = anchor.and_then(|p| r.coll.id_of(&p));
    if !refresh {
        s.fursuits_shown = false;
        s.flipped.clear();
        s.collapsed.clear();
        s.selection.clear();
        s.anchor = None;
        s.scroll = 0.0;
        s.scroll_seq += 1;
        s.thumbs.clear_queue(&app.workers);
    }
    s.relayout(false, None);
    // A new search selects its first result, ready for Space, Enter or Open day.
    if !refresh {
        if let Some(&first) = s.layout.tiles.first() {
            s.selection = [first].into();
            s.anchor = Some(first);
        }
    }
    super::quicklook::follow(app)
}

// ── what the engine says ────────────────────────────────────────────────────

pub fn on_status(app: &mut App, new: Arc<Status>) -> Task<Message> {
    let old = std::mem::replace(&mut app.search.status, (*new).clone());
    let st = &app.search.status;
    let mut tasks = Vec::new();
    if st.stopped.is_some() && old.stopped != st.stopped {
        app.search.banner_hidden = false;
    }
    let st = &app.search.status;
    if st.helper_restarts > old.helper_restarts {
        tasks.push(app.show_status("The search helper stopped and was started again. Indexing carried on.", 6000));
    }
    let st = &app.search.status;
    if matches!(old.stopped, Some(Stopped::Card(_))) && st.stopped.is_none() && st.device.chosen == Device::Gpu {
        tasks.push(app.show_status("The graphics card passed its check. Indexing carries on.", 6000));
    }
    // Where the pass that just ended began; the one now shown, if any.
    let start = app.search.pass_start;
    app.search.pass_start = super::search_setup::pass_start(start, &app.search.status);
    let st = &app.search.status;
    if let (Some(p), None, None) = (&old.pass, &st.pass, &st.stopped) {
        let ended = super::search_setup::pass_ended(p, start, st);
        app.search.steps = false;
        // A pass of new exports that added nothing says nothing.
        if let Some(text) = ended {
            tasks.push(app.show_status(text, 8000));
            rerun(app);
        }
    } else {
        // Searching while the first index runs: the results fill in.
        let s = &app.search;
        let grew = s.status.indexed_shots.saturating_sub(s.last_run.1);
        let enough = grew * 50 >= s.status.library_shots.max(1);
        if coverage(&s.status) == Coverage::Growing && app.view == View::Search && s.pending.is_none() && enough && s.last_run.0.elapsed() >= REFRESH_EVERY {
            rerun(app);
        }
    }
    Task::batch(tasks)
}

pub fn on_download(app: &mut App, model: ModelId, done: u64, total: u64, state: DownloadState) -> Task<Message> {
    let s = &mut app.search;
    let failed = super::search_setup::failed(&state);
    let d = Download { model, done, total, state };
    match s.downloads.iter_mut().find(|x| x.model == model) {
        Some(x) => *x = d,
        None => s.downloads.push(d),
    }
    // A failure shows in the dialog, reopened if it was closed.
    if failed {
        s.dialog = Some(Dialog { stage: Stage::Progress, sums: false });
    }
    Task::none()
}

/// The things search found changed (the index grew, the vocabulary was
/// worked out again).
pub fn on_things(app: &mut App, things: Arc<Vec<Thing>>) {
    if *things == *app.search.things {
        return;
    }
    app.search.things = things;
    if app.search.completions {
        refresh_offer(app);
    }
}

/// Work out what the list under the field offers for the query, from
/// memory: the things found, the calendar's days, the recent searches.
fn refresh_offer(app: &mut App) {
    let s = &mut app.search;
    let days = app.calendar.index.as_ref().map(|ix| ix.days.keys());
    s.offer = res::offer(&s.query, &s.things, days.into_iter().flatten(), &app.settings.search_recent);
    if s.hi.is_some_and(|h| h >= s.offer.picks.len()) {
        s.hi = None;
    }
}

/// The library was read again: the engine looks for new photos.
pub fn on_library(app: &mut App, index: Arc<LibraryIndex>) {
    app.search.link.on_library(index);
}

/// Tell the engine how many thumbnails the views wait for: the pass steps
/// back while any.  Called after each batch of background events; sends
/// only a change.
pub fn note_busy(app: &mut App) {
    let pending = app.thumbs.pending() + app.calendar.thumbs.pending() + app.search.thumbs.pending();
    if pending != app.search.busy_sent {
        app.search.busy_sent = pending;
        app.search.link.set_busy(pending);
    }
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Input(text) => {
            let s = &mut app.search;
            s.query = text;
            s.completions = true;
            s.hi = None;
            refresh_offer(app);
            Task::none()
        }
        Msg::Submit => {
            let s = &app.search;
            match s.hi.filter(|_| s.completions) {
                Some(i) => take(app, i),
                None => {
                    let q = app.search.query.clone();
                    run(app, q)
                }
            }
        }
        Msg::Take(i) => take(app, i),
        Msg::Run(words) => run(app, words),
        Msg::Clear => {
            let t = run(app, String::new());
            Task::batch([t, focus_field()])
        }
        Msg::CloseList => {
            app.search.completions = false;
            app.search.hi = None;
            Task::none()
        }
        Msg::Back => back(app),
        Msg::OpenDay => open_day(app),
        Msg::Grid(ev) => on_grid(app, ev),
        Msg::Built(seq, r) => on_built(app, seq, r),
        Msg::IndexLibrary => {
            app.search.dialog = Some(Dialog { stage: Stage::Ask, sums: false });
            Task::none()
        }
        Msg::IndexRest => {
            app.search.link.start_first_index(&search_models());
            app.show_status("Indexing the rest of the library.", 5000)
        }
        Msg::Dialog(d) => super::search_setup::dialog_update(app, d),
        Msg::Stop => {
            let s = &app.search;
            let downloading = s.downloading();
            let text = if downloading {
                "Stopped the download. Nothing was indexed.".to_string()
            } else {
                format!(
                    "Stopped indexing at {} of {} photos. What was indexed is kept, and search works on it.",
                    util::thousands(s.status.indexed_shots as u64),
                    util::thousands(s.status.library_shots as u64)
                )
            };
            if downloading {
                s.link.cancel_downloads();
            } else {
                s.link.stop_indexing();
            }
            app.search.steps = false;
            app.show_status(text, 0)
        }
        Msg::Steps(open) => {
            app.search.steps = open;
            Task::none()
        }
        Msg::TryCard => {
            app.search.banner_hidden = false;
            app.search.link.try_card_again();
            Task::none()
        }
        Msg::TryAgain => {
            app.search.link.try_card_again();
            Task::none()
        }
        Msg::OpenPrefs => super::preferences::open_at(app, super::preferences::Control::SearchDevice(app.settings.search_device)),
        Msg::HideBanner => {
            app.search.banner_hidden = true;
            Task::none()
        }
    }
}

/// Take completion `i`: a recent search runs; a thing or a date goes into
/// the field in place of the word being typed, ready for the next one.
fn take(app: &mut App, i: usize) -> Task<Message> {
    let Some(pick) = app.search.offer.picks.get(i).cloned() else { return Task::none() };
    if let Pick::Recent(words) = pick {
        return run(app, words);
    }
    let s = &mut app.search;
    s.query = res::apply(&s.query, &pick);
    s.completions = true;
    s.hi = None;
    refresh_offer(app);
    Task::batch([iced::widget::operation::focus::<Message>(FIELD), iced::widget::operation::move_cursor_to_end(FIELD)])
}

/// *Show fursuits too*, or *Hide fursuits*: the fursuits an animal word
/// left out become a list of their own after the real animals, or go.
fn show_fursuits(app: &mut App, on: bool) -> Task<Message> {
    let s = &mut app.search;
    if s.fursuits_shown == on || s.results.as_ref().is_none_or(|r| r.fursuits.is_empty()) {
        return Task::none();
    }
    s.fursuits_shown = on;
    let focus = s.focus();
    s.relayout(false, focus);
    // Shown, their list starts where the note was: bring it into view.
    if on {
        let first = s.layout.sections.get(1).and_then(|sec| sec.ids.first().copied());
        s.reveal = (s.reveal.0 + 1, first);
    }
    super::quicklook::follow(app)
}

// ── the results grid ────────────────────────────────────────────────────────

fn on_grid(app: &mut App, ev: GridEvent) -> Task<Message> {
    let s = &mut app.search;
    s.completions = false;
    match ev {
        GridEvent::Click { id, modifiers } => {
            if modifiers.control() {
                if !s.selection.remove(&id) {
                    s.selection.insert(id);
                }
            } else {
                s.selection = [id].into();
            }
            s.anchor = Some(id);
            Task::none()
        }
        GridEvent::Activate(id) => open_viewer(app, id),
        GridEvent::ClearSelection => {
            s.selection.clear();
            Task::none()
        }
        GridEvent::ToggleBurst(id) => toggle_burst(app, id),
        GridEvent::ToggleCollapse(d) => {
            if !s.collapsed.remove(&d) {
                s.collapsed.insert(d);
            }
            Task::none()
        }
        GridEvent::Chip(_) | GridEvent::SelectDone => Task::none(),
        // *Hide fursuits* on their list's heading; *Show fursuits too* in
        // the note under the results.
        GridEvent::SectionLink(d) if d == res::FURSUITS_DAY => show_fursuits(app, false),
        GridEvent::SectionLink(_) => Task::none(),
        GridEvent::FooterLink => show_fursuits(app, true),
        GridEvent::ZoomStep(step) => {
            s.thumb_size = (((s.thumb_size + step as f32 * super::ZOOM_STEP) / super::ZOOM_STEP).floor() * super::ZOOM_STEP).clamp(grid::MIN_SIZE, grid::MAX_SIZE);
            Task::none()
        }
        GridEvent::PinchTo(size) => {
            s.thumb_size = size.clamp(grid::MIN_SIZE, grid::MAX_SIZE);
            Task::none()
        }
        GridEvent::Thumbs { visible, nearby, settle } => {
            let Some(r) = s.results.clone() else { return Task::none() };
            if settle {
                s.thumbs.clear_queue(&app.workers);
            }
            for (ids, prio) in [(visible, PRIORITY_VISIBLE), (nearby, PRIORITY_PREFETCH)] {
                for id in ids {
                    if let Some(rec) = r.coll.get(id) {
                        s.thumbs.request(&app.workers, rec, prio);
                    }
                }
            }
            Task::none()
        }
        GridEvent::Scrolled(y) => {
            s.scroll = y;
            Task::none()
        }
    }
}

/// The viewer on a result (read-only, like a library day), stepping
/// through every frame of its list in the results' order (the fursuits'
/// list and the real animals' are never stepped through together); a
/// stack opens on its cover.
pub fn open_viewer(app: &mut App, id: RecordId) -> Task<Message> {
    let s = &mut app.search;
    let Some(r) = s.results.clone() else { return Task::none() };
    s.completions = false;
    let items = s.layout.visible_with(id).to_vec();
    let index = items.iter().position(|&x| x == id).unwrap_or(0);
    super::viewer::open(app, super::viewer::Source::Library(r.coll.clone()), items, index, true)
}

/// G or a burst's tab: stack the burst of `id`, or open it.
pub fn toggle_burst(app: &mut App, id: RecordId) -> Task<Message> {
    let s = &mut app.search;
    let Some(g) = s.layout.group_of(id) else { return Task::none() };
    let Some(key) = s.layout.keys[g].filter(|_| s.layout.groups[g].len() > 1) else {
        let name = s.results.as_ref().and_then(|r| r.coll.get(id)).map(|r| r.stem()).unwrap_or_default();
        return app.show_status(format!("{name} isn't part of a burst."), 4000);
    };
    if !s.flipped.remove(&key) {
        s.flipped.insert(key);
    }
    s.relayout(true, Some(id));
    super::quicklook::follow(app)
}

/// G on the page: the focused result's burst.
pub fn toggle_focused(app: &mut App) -> Task<Message> {
    match app.search.focus() {
        Some(id) => toggle_burst(app, id),
        None => app.show_status("Select a photo to stack its burst.", 4000),
    }
}

/// Shift+G on the page: every burst stacked, or every frame shown (for
/// these results; the grid's saved choice stays as it is).
pub fn toggle_all(app: &mut App) -> Task<Message> {
    let s = &mut app.search;
    s.stack_all = !s.stack_all;
    s.flipped.clear();
    let anchor = s.focus();
    s.relayout(true, anchor);
    let n = s.layout.groups.iter().filter(|g| g.len() > 1).count();
    let text = match (s.stack_all, n) {
        (_, 0) => "No bursts in these results.".to_string(),
        (true, n) => format!("{} stacked. G opens the selected one.", util::plural(n as u64, "burst")),
        (false, _) => "Every frame of every burst shows.".to_string(),
    };
    let t = app.show_status(text, 4000);
    Task::batch([t, super::quicklook::follow(app)])
}

/// Move the selection to `id` and bring it into view.
pub fn select(app: &mut App, id: RecordId) {
    let s = &mut app.search;
    s.selection = [id].into();
    s.anchor = Some(id);
    s.reveal = (s.reveal.0 + 1, Some(id));
}

// ── keys ────────────────────────────────────────────────────────────────────

/// Esc on the page (also when the field took it): it closes the
/// completions, and never leaves the page.
pub fn escape(app: &mut App) -> Task<Message> {
    app.search.completions = false;
    app.search.hi = None;
    Task::none()
}

/// Keys on the page that no command took.
pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if mods.control() || mods.alt() {
        return Task::none();
    }
    if app.search.completions {
        let n = app.search.offer.picks.len();
        let s = &mut app.search;
        match key {
            Key::Named(Named::ArrowDown) if n > 0 => {
                s.hi = Some(s.hi.map_or(0, |h| (h + 1) % n));
                return Task::none();
            }
            Key::Named(Named::ArrowUp) if n > 0 => {
                s.hi = Some(s.hi.map_or(n - 1, |h| (h + n - 1) % n));
                return Task::none();
            }
            // Tab completes: the chosen one, else the first.
            Key::Named(Named::Tab) => {
                return match s.hi.or((n > 0).then_some(0)) {
                    Some(i) => take(app, i),
                    None => Task::none(),
                };
            }
            Key::Named(Named::Enter) => {
                return match s.hi {
                    Some(i) => take(app, i),
                    None => {
                        let q = s.query.clone();
                        run(app, q)
                    }
                };
            }
            Key::Named(Named::Escape) => return escape(app),
            _ => {}
        }
    }
    match key {
        Key::Named(Named::Escape) => escape(app),
        Key::Named(Named::ArrowLeft | Named::ArrowRight) => {
            let forward = *key == Key::Named(Named::ArrowRight);
            if let Some(next) = res::step(&app.search.layout.tiles, app.search.anchor, forward) {
                select(app, next);
            }
            Task::none()
        }
        Key::Named(Named::Enter) => match app.search.focus() {
            Some(id) => open_viewer(app, id),
            None => Task::none(),
        },
        _ => Task::none(),
    }
}

// ── view ────────────────────────────────────────────────────────────────────

/// What the page's body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    NoLibrary,
    Reading,
    /// The card is chosen and there is none to use, before any index.
    NoCard,
    /// The invitation to the first index, with its costs.
    NotIndexed,
    /// The model is downloading, or the first photos are being indexed.
    Preparing,
    Start,
    /// Nothing to look for was typed (only a name, dates or words to leave
    /// out): no search ran.
    NoWords,
    Searching,
    Failed,
    NoResults,
    Results,
}

pub fn page(app: &App) -> Page {
    let s = &app.search;
    if app.library.is_none() {
        return Page::NoLibrary;
    }
    if app.calendar.index.is_none() {
        return Page::Reading;
    }
    let st = &s.status;
    if st.indexed_shots == 0 {
        if s.downloading() || st.pass.is_some() {
            return Page::Preparing;
        }
        let no_card = st.device.chosen == Device::Gpu && matches!(st.stopped, Some(Stopped::Card(CardFailure::NoCard)));
        return if no_card { Page::NoCard } else { Page::NotIndexed };
    }
    match (&s.ran, &s.results, &s.error) {
        (None, _, _) => Page::Start,
        (Some(_), _, _) if s.no_words.is_some() => Page::NoWords,
        (Some(_), _, Some(_)) => Page::Failed,
        (Some(_), None, None) => Page::Searching,
        (Some(_), Some(r), None) if r.found.is_empty() => Page::NoResults,
        _ => Page::Results,
    }
}

pub fn view(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    let s = &app.search;
    let mut col = column![header(app), kit::hairline()].width(Length::Fill).height(Length::Fill);
    let p = page(app);
    if matches!(p, Page::Results | Page::NoResults) && coverage(&s.status) != Coverage::Whole {
        col = col.push(partial_strip(app));
    }
    if matches!(p, Page::Results | Page::NoResults) {
        if let Some(strip) = understood_strip(app) {
            col = col.push(strip);
        }
    }
    let body: Element<'_, Message> = match p {
        Page::NoLibrary => message_page(
            Glyph::Library,
            "Choose your library folder",
            "Search finds photos in the library, the folder Export files them into by date.".into(),
            Some(("Choose library folder…", Message::PickFolder(super::FolderTarget::Library))),
            None,
        ),
        Page::Reading => message_page(Glyph::Search, "Reading the library…", String::new(), None, None),
        Page::NoCard => no_card_page(app),
        Page::NotIndexed => invitation(app),
        Page::Preparing => preparing(app),
        Page::Start => start_page(app),
        Page::NoWords => no_words(app),
        Page::Searching => container(Space::new()).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into(),
        Page::Failed => message_page(
            Glyph::Warning,
            "Search couldn't run",
            s.error.clone().unwrap_or_default(),
            Some(("Try again", m(Msg::Submit))),
            None,
        ),
        Page::NoResults => no_results(app),
        Page::Results => results_grid(app, keyboard_ok),
    };
    col = col.push(body);
    // The list's layer is always there (empty when closed): the page's
    // widget tree keeps its shape, so the field keeps its focus.
    let list: Element<'_, Message> =
        if s.completions && !matches!(p, Page::NoLibrary | Page::Reading) { completions_layer(app) } else { Space::new().into() };
    stack![col, list].width(Length::Fill).height(Length::Fill).into()
}

/// The field's width: wide, but leaving room for the back button, the
/// count and Open day.
fn field_width(app: &App) -> f32 {
    (app.window_size_width() - RAIL_W - 380.0).clamp(260.0, 560.0)
}

fn back_width(app: &App) -> f32 {
    t::S3 + 16.0 + t::S2 + paint::text_width(&app.search.from_label, t::T_BODY, paint::MEDIUM) + t::S3
}

fn header(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let place = if s.from == View::Calendar { "calendar".to_string() } else { s.from_label.to_lowercase() };
    let back = tip(kit::icon_text_btn(Glyph::ChevronLeft, s.from_label.clone(), t::INK2, style::secondary(), Some(m(Msg::Back))), format!("Back to the {place}"), true);
    let mut r = row![back, Space::new().width(t::S2), field(app)].align_y(Alignment::Center);
    let p = page(app);
    let caption = if s.pending.is_some_and(|p| !p.1) || p == Page::Searching {
        "Searching…".to_string()
    } else if let (Page::Results, Some(r)) = (p, s.results.as_ref()) {
        // The count (and the dates, for dates alone): the list's heading
        // already says in what order.
        res::count_caption(&r.understood, s.photos(), coverage(&s.status) != Coverage::Whole)
    } else {
        String::new()
    };
    r = r.push(container(kit::caption(caption)).padding(Padding { left: t::S3, ..Padding::ZERO }));
    r = r.push(Space::new().width(Length::Fill));
    if p == Page::Results {
        let day = s.focus().and_then(|id| s.results.as_ref()?.day_of(id));
        let tone = if day.is_some() { t::INK } else { t::INK4 };
        let label = row![text("Open day").size(t::T_BODY).font(paint::MEDIUM), icons::view(Glyph::ChevronRight, 15.0, tone)].spacing(t::S1).align_y(Alignment::Center);
        let b = button(container(label).center_y(Length::Fill))
            .height(t::CONTROL_H)
            .padding(Padding { left: t::S4, right: t::S2, top: 0.0, bottom: 0.0 })
            .style(style::secondary().style())
            .on_press_maybe(day.map(|_| m(Msg::OpenDay)));
        let hint = match day {
            Some(d) => format!("Show {} in the calendar, with this photo selected", long_date(d)),
            None => "Select a photo to open its day".into(),
        };
        r = r.push(tip(b, hint, true));
    }
    container(r.padding(Padding::from([0.0, t::S3]))).height(HEADER_H).center_y(HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

pub fn long_date(d: NaiveDate) -> String {
    format!("{}, {} {}, {}", d.format("%A"), MONTH_NAME[d.month0() as usize], d.day(), d.year())
}

/// The field: the query in title type, a magnifier before it, and × to
/// clear it (or the key that comes back to it, while empty).
fn field(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let w = field_width(app);
    let input = text_input("Search the library", &s.query)
        .id(FIELD)
        .on_input(|v| m(Msg::Input(v)))
        .on_submit(m(Msg::Submit))
        .size(t::T_TITLE)
        .font(paint::MEDIUM)
        .padding(Padding { top: 7.0, bottom: 7.0, left: 36.0, right: 40.0 })
        .width(w)
        .style(field_style);
    let end: Element<'_, Message> = if s.query.is_empty() {
        container(text("Ctrl+F").size(11).font(paint::MEDIUM).color(t::INK3))
            .padding(Padding::from([0.0, 5.0]))
            .style(|_| container::Style { border: style::border(t::LINE_HI, 1.0, 4.0), ..Default::default() })
            .into()
    } else {
        let x = button(container(icons::view(Glyph::Close, 14.0, t::INK3)).center(Length::Fill))
            .width(24)
            .height(24)
            .padding(0)
            .style(style::ghost().radius(4.0).style())
            .on_press(m(Msg::Clear));
        tip(x, "Clear the search", true)
    };
    let over = row![icons::view(Glyph::Search, 18.0, t::INK3), Space::new().width(Length::Fill), end]
        .align_y(Alignment::Center)
        .padding(Padding { left: 11.0, right: 6.0, top: 0.0, bottom: 0.0 })
        .width(w)
        .height(FIELD_H);
    stack![container(input).height(FIELD_H).center_y(FIELD_H), over].into()
}

fn field_style(_t: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    let hovered = matches!(status, text_input::Status::Hovered | text_input::Status::Focused { is_hovered: true });
    let edge = if focused {
        t::SIGNAL
    } else if hovered {
        t::INK3
    } else {
        t::LINE_HI
    };
    text_input::Style {
        background: Background::Color(t::DEEP),
        border: style::border(edge, 1.0, t::R_CONTROL),
        icon: t::INK3,
        placeholder: t::INK3,
        value: t::INK,
        selection: alpha(t::SIGNAL, 0.35),
    }
}

/// The completions under the field while typing, in the Hints menu's
/// floating list: rows under their headings, each a glyph, the words (what
/// is typed set heavier) and a count, and a footer teaching the syntax.  A
/// click elsewhere on the page closes it.
fn completions_layer(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let offer = &s.offer;
    let w = field_width(app);
    let mut list = column![].spacing(2);
    let typed = s.query.trim();
    let quiet = |line: String| container(text(line).size(t::T_SMALL).color(t::INK3)).padding(Padding::from([t::S2, 10.0]));
    if offer.names_later {
        list = list.push(quiet("Searching by name comes later. For now, search by what a photo shows.".into()));
    } else if offer.picks.is_empty() {
        let line = if typed.is_empty() { "Type what you're looking for, then press Enter.".to_string() } else { format!("Press Enter to search for “{typed}”") };
        list = list.push(quiet(line));
    }
    let mut group = "";
    for (i, pick) in offer.picks.iter().enumerate() {
        if pick.group() != group {
            group = pick.group();
            let top = if i == 0 { t::S2 } else { t::S3 };
            list = list.push(container(kit::heading(group)).padding(Padding { top, bottom: 3.0, left: 10.0, right: 10.0 }));
        }
        list = list.push(completion_row(pick, i, s.hi == Some(i), &offer.stem, typed));
    }
    list = list.push(container(kit::hairline()).padding(Padding { top: t::S1, ..Padding::ZERO }));
    list = list.push(syntax_footer());
    let panel = crate::ui::motion::Appear::new(container(list).width(w).padding(t::S1).style(style::floating())).rise(-4.0);
    let x = t::S3 + back_width(app) + t::S2;
    let catcher = column![
        Space::new().height(HEADER_H),
        opaque(mouse_area(container(Space::new()).width(Length::Fill).height(Length::Fill)).on_press(m(Msg::CloseList))),
    ];
    stack![catcher, pin(opaque(panel)).position(Point::new(x, HEADER_H / 2.0 + FIELD_H / 2.0 + 4.0)).width(Length::Fill).height(Length::Fill)].into()
}

/// One completion: its glyph, its words with what is typed set heavier,
/// and its count.  `stem` is the word being typed, `typed` the whole field
/// (what a recent search matched).
fn completion_row<'a>(pick: &Pick, i: usize, hot: bool, stem: &str, typed: &str) -> Element<'a, Message> {
    let (glyph, words, count, mark): (Glyph, String, String, String) = match pick {
        Pick::Thing { word, photos, minus: false } => (Glyph::Search, word.clone(), util::plural(*photos as u64, "photo"), stem.to_string()),
        Pick::Thing { word, photos, minus: true } => (Glyph::LeaveOut, format!("-{word}"), util::plural(*photos as u64, "photo"), format!("-{stem}")),
        Pick::Date { label, days, .. } => (Glyph::Calendar, label.clone(), format!("{} with photos", util::plural(*days as u64, "day")), stem.to_string()),
        Pick::Recent(r) => (Glyph::Clock, r.clone(), String::new(), typed.to_lowercase()),
    };
    let mut line = row![icons::view(glyph, 15.0, t::INK3), container(highlighted(words, &mark)).width(Length::Fill)].spacing(10).align_y(Alignment::Center);
    if !count.is_empty() {
        line = line.push(text(count).size(t::T_SMALL).color(t::INK3));
    }
    button(line)
        .padding(Padding::from([5.0, 10.0]))
        .width(Length::Fill)
        .style(move |_t, status| {
            let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: (hot || hover).then_some(Background::Color(t::RAISED)),
                text_color: t::INK,
                border: Border { radius: t::R_CONTROL.into(), ..Border::default() },
                ..button::Style::default()
            }
        })
        .on_press(m(Msg::Take(i)))
        .into()
}

/// The list's footer: the syntax, taught by example.
fn syntax_footer<'a>() -> Element<'a, Message> {
    let item = |cap: &'a str, about: &'a str| row![keycap(cap), text(about).size(t::T_SMALL).color(t::INK3)].spacing(6).align_y(Alignment::Center);
    container(row![item("-word", "leaves a word out"), item("jun 2024", "only those dates")].spacing(t::S4).align_y(Alignment::Center))
        .padding(Padding { top: t::S2, bottom: 6.0, left: 10.0, right: 10.0 })
        .into()
}

/// Typed syntax shown as a key cap, on the chrome.
fn keycap<'a>(s: &'a str) -> Element<'a, Message> {
    container(text(s).size(11).font(paint::MEDIUM).color(t::INK))
        .padding(Padding::from([1.0, 6.0]))
        .style(|_| container::Style { background: Some(Background::Color(t::RAISED)), border: style::border(t::LINE_HI, 1.0, 4.0), ..Default::default() })
        .into()
}

/// Words with the part matching `typed` (from the start of a word) in a
/// heavier weight.
fn highlighted<'a>(words: String, typed: &str) -> Element<'a, Message> {
    let lower = words.to_lowercase();
    let at = if typed.is_empty() {
        None
    } else if lower.starts_with(typed) {
        Some(0)
    } else {
        lower.find(&format!(" {typed}")).map(|i| i + 1)
    };
    match at {
        // Offsets of the lower-case match line up for ASCII words only.
        Some(i) if words.is_ascii() => {
            let (a, rest) = words.split_at(i);
            let (b, c) = rest.split_at(typed.len().min(rest.len()));
            row![
                text(a.to_string()).size(t::T_BODY).color(t::INK),
                text(b.to_string()).size(t::T_BODY).font(paint::SEMIBOLD).color(t::INK),
                text(c.to_string()).size(t::T_BODY).color(t::INK),
            ]
            .into()
        }
        _ => text(words).size(t::T_BODY).color(t::INK).into(),
    }
}

/// Over the results while search sees part of the library.
fn partial_strip(app: &App) -> Element<'_, Message> {
    let st = &app.search.status;
    let n = util::thousands(st.indexed_shots as u64);
    let mut r = row![icons::view(Glyph::Clock, 15.0, t::INK3)].spacing(t::S2).align_y(Alignment::Center);
    match coverage(st) {
        Coverage::Stopped => {
            r = r.push(
                text(format!("Searching the {n} of {} photos indexed. Indexing is stopped; the rest is searched once it carries on.", util::thousands(st.library_shots as u64)))
                    .size(t::T_SMALL)
                    .color(t::INK2)
                    .width(Length::Fill),
            );
            if matches!(st.stopped, Some(Stopped::Cancelled)) {
                r = r.push(button(text("Index the rest").size(t::T_SMALL).font(paint::MEDIUM)).padding(Padding::from([2.0, 6.0])).style(style::link().style()).on_press(m(Msg::IndexRest)));
            }
        }
        _ => {
            r = r.push(text(format!("Searching the {n} photos indexed so far, newest first. Results fill in as indexing reaches older days.")).size(t::T_SMALL).color(t::INK2));
        }
    }
    column![container(r.padding(Padding::from([t::S2, t::S3]))).width(Length::Fill).style(style::fill(t::GROUND)), kit::hairline()].into()
}

/// Over the results, quietly, what was applied besides the words: the
/// dates, the words left out, and a name it can't search yet.  So the user
/// sees why the results look as they do.  `None` when there is nothing to
/// say.
fn understood_strip(app: &App) -> Option<Element<'_, Message>> {
    let r = app.search.results.as_ref()?;
    let u = &r.understood;
    let (dates, leave) = res::understood_lines(u);
    if dates.is_none() && leave.is_none() && u.names.is_empty() {
        return None;
    }
    let item = |g: Glyph, words: String| row![icons::view(g, 14.0, t::INK3), text(words).size(t::T_SMALL).color(t::INK2)].spacing(6).align_y(Alignment::Center);
    let mut r = row![].spacing(t::S5).align_y(Alignment::Center);
    if let Some(d) = dates {
        r = r.push(item(Glyph::Calendar, d));
    }
    if let Some(l) = leave {
        r = r.push(item(Glyph::LeaveOut, l));
    }
    if !u.names.is_empty() {
        let names: Vec<String> = u.names.iter().map(|n| format!("@{n}")).collect();
        r = r.push(item(Glyph::Info, format!("Searching by name comes later, so {} was left out", res::and_list(&names))));
    }
    Some(column![container(r.wrap().vertical_spacing(t::S1)).padding(Padding::from([t::S2, t::S3])).width(Length::Fill).style(style::fill(t::GROUND)), kit::hairline()].into())
}

/// The fursuits' list's heading carries *Hide fursuits*.
const FURSUIT_LINKS: &[(NaiveDate, &str)] = &[(res::FURSUITS_DAY, "Hide fursuits")];

fn results_grid(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    let s = &app.search;
    let Some(r) = s.results.as_ref() else { return Space::new().into() };
    // The end note: the fursuits left out (with the way to show them), then
    // that this is everything, once search sees the whole library.
    let note = (!r.fursuits.is_empty() && !s.fursuits_shown).then_some((s.left_out_line.as_str(), "Show fursuits too"));
    let end = (coverage(&s.status) == Coverage::Whole).then_some((s.end_line.as_str(), s.end_quiet));
    let footer = (note.is_some() || end.is_some()).then_some(grid::Footer { note, end });
    let data = GridData {
        sections: &s.layout.sections,
        coll: &r.coll,
        thumbs: &s.thumbs,
        selection: &s.selection,
        collapsed: &s.collapsed,
        select_mode: false,
        scale_factor: app.scale_factor,
        show_names: true,
        bursts: &s.layout.runs,
        moments: &[],
        hints: None,
        restack: Some(Restack { seq: s.restack_seq, sections: &s.before.0, bursts: &s.before.1, focus: s.restack_focus }),
        label: CellLabel::Date,
        footer,
        links: if s.fursuits_shown { FURSUIT_LINKS } else { &[] },
    };
    Grid::new(data, s.thumb_size, |e| m(Msg::Grid(e)))
        .keyboard(keyboard_ok && !s.completions && app.quicklook.is_none())
        .initial_scroll(s.scroll)
        .scroll_seq(s.scroll_seq)
        .layout_key(s.key)
        .reveal(s.reveal.0, s.reveal.1)
        .into()
}

// ── the pages ───────────────────────────────────────────────────────────────

/// A section heading on the photos' grey, as the grid draws a day's.
fn photo_heading<'a>(label: &'a str, note: &'a str) -> Element<'a, Message> {
    let mut r = row![text(label).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK)].spacing(10).align_y(Alignment::Center);
    if !note.is_empty() {
        r = r.push(text(note).size(t::T_SMALL).color(t::PHOTO_INK3));
    }
    r = r.push(container(Space::new().width(Length::Fill).height(1)).width(Length::Fill).style(style::fill(t::PHOTO_LINE)));
    container(r).height(grid::HEADER_H).center_y(grid::HEADER_H).padding(Padding::from([0.0, 16.0])).width(Length::Fill).into()
}

/// A recent search, or a thing found (with how many photos show it), to
/// click on the photos' grey: it runs that search.
fn word_button<'a>(words: &str, count: Option<usize>) -> Element<'a, Message> {
    let mut label = row![text(words.to_string()).size(14).font(paint::MEDIUM)].spacing(5).align_y(iced::alignment::Vertical::Bottom);
    if let Some(n) = count {
        // Lifted a pixel, so the smaller figures share the word's baseline.
        let figures = text(util::thousands(n as u64)).size(t::T_SMALL).color(t::PHOTO_INK3);
        label = label.push(container(figures).padding(Padding { bottom: 1.0, ..Padding::ZERO }));
    }
    button(label)
        .padding(Padding::from([2.0, 0.0]))
        .style(|_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style { text_color: if hot { t::SIGNAL_HI } else { t::PHOTO_INK }, ..button::Style::default() }
        })
        .on_press(m(Msg::Run(words.to_string())))
        .into()
}

/// The most things shown on the start page.
const START_THINGS: usize = 24;

/// An example search, as a key cap on the photos' grey, and what it shows.
fn example<'a>(words: String, about: String) -> Element<'a, Message> {
    let chip = button(text(words.clone()).size(t::T_SMALL).font(paint::MEDIUM))
        .padding(Padding::from([2.0, 7.0]))
        .style(|_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if hot { t::PHOTO_TRAY_HI } else { t::PHOTO_RAISED })),
                text_color: t::PHOTO_INK,
                border: style::border(t::PHOTO_LINE, 1.0, 4.0),
                ..button::Style::default()
            }
        })
        .on_press(m(Msg::Run(words)));
    row![container(chip).width(170), text(about).size(t::T_BODY).color(t::PHOTO_INK2)].align_y(Alignment::Center).into()
}

/// Nothing typed: the last searches, the things in the photos, and how to
/// search.
fn start_page(app: &App) -> Element<'_, Message> {
    let recent = &app.settings.search_recent;
    let words_block = |words: iced::widget::Row<'static, Message>| container(words.wrap().vertical_spacing(4)).padding(Padding { left: 16.0, right: 16.0, top: 0.0, bottom: 18.0 }).max_width(1000);
    let mut col = column![].padding(Padding { top: t::S1, bottom: t::S5, left: 0.0, right: 0.0 });
    if !recent.is_empty() {
        col = col.push(photo_heading("Recent searches", "The last 10. Forget them in Preferences ▸ Search."));
        let mut words = row![].spacing(18);
        for w in recent {
            words = words.push(word_button(w, None));
        }
        col = col.push(words_block(words));
    }
    // Objects and scenes only: never people, fursuits or animals.
    let things: Vec<&Thing> = res::offered(&app.search.things).take(START_THINGS).collect();
    if !things.is_empty() {
        col = col.push(photo_heading("Things in your photos", "Suggested from what search learned. Never shown on photos."));
        let mut words = row![].spacing(18);
        for th in things {
            words = words.push(word_button(&th.word, Some(th.photos)));
        }
        col = col.push(words_block(words));
    }
    col = col.push(photo_heading("Ways to search", ""));
    // Dates from the library's newest month, so the examples find something.
    let newest = app.calendar.index.as_ref().and_then(|ix| ix.days.keys().next_back().copied());
    let (year, m0) = newest.map_or((2024, 5), |d| (d.year(), d.month0()));
    let month = crate::search::parse::date_text(year, Some(m0 + 1));
    let examples = column![
        example("red car at night".into(), "Photos that look like the words, best match first".into()),
        example("fireworks -crowd".into(), "Leave a word out: fireworks, without the crowd".into()),
        example(format!("fireworks {year}"), format!("Only those dates: {year}, {month}, apr–jun {year}")),
        example(month.clone(), "Every photo from those dates, in the order they were taken".into()),
    ]
    .spacing(10)
    .padding(Padding { left: 16.0, right: 16.0, top: 0.0, bottom: 18.0 });
    col = col.push(examples);
    col = col.push(
        container(text("Search goes by what a photo shows, not its file or folder name. Results are read-only, like the calendar's days: Open day › takes you to a photo's day.").size(t::T_SMALL).color(t::PHOTO_INK3))
            .padding(Padding::from([0.0, 16.0]))
            .max_width(760),
    );
    container(scrollable(col).style(style::scroll).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::PHOTO_GROUND))
        .into()
}

fn no_results(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let Some(r) = s.results.as_ref() else { return Space::new().into() };
    let so_far = (coverage(&s.status) != Coverage::Whole).then_some(s.status.indexed_shots);
    let (title, body) = res::nothing_found(r, so_far);
    quiet_page(title, body)
}

/// Nothing to look for was typed, so nothing ran: a name, which search
/// can't find yet, or only words to leave out.  The page says so, plainly.
fn no_words(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let typed = s.ran.clone().unwrap_or_default();
    let (title, body) = match &s.no_words {
        Some(p) if !p.names.is_empty() => (
            "Searching by name comes later".to_string(),
            format!("“{typed}” is a name. For now, search by what a photo shows, such as fireworks or a red car at night."),
        ),
        _ => ("Add what to look for".to_string(), format!("“{typed}” says what to leave out. Add a few words for what the photos show, such as fireworks {typed}.")),
    };
    quiet_page(title, body)
}

/// A title and a line, centred on the photos' grey.
fn quiet_page<'a>(title: String, body: String) -> Element<'a, Message> {
    let col = column![
        Space::new().height(Length::FillPortion(2)),
        text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK).align_x(iced::alignment::Horizontal::Center),
        text(body).size(t::T_BODY).color(t::PHOTO_INK2).align_x(iced::alignment::Horizontal::Center),
        Space::new().height(Length::FillPortion(5)),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(480);
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
}

/// A page that explains itself on the chrome's ground: a glyph, a title,
/// what to know, and up to two buttons.
fn message_page<'a>(glyph: Glyph, title: &'a str, body: String, primary: Option<(&'a str, Message)>, secondary: Option<(&'a str, Message)>) -> Element<'a, Message> {
    let mut col = column![].spacing(t::S2).align_x(Alignment::Center).max_width(480);
    col = col.push(Space::new().height(Length::FillPortion(3)));
    col = col.push(icons::view(glyph, 48.0, t::INK4));
    col = col.push(Space::new().height(t::S2));
    col = col.push(text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK).align_x(iced::alignment::Horizontal::Center));
    if !body.is_empty() {
        col = col.push(text(body).size(t::T_BODY).color(t::INK2).align_x(iced::alignment::Horizontal::Center));
    }
    let mut buttons = row![].spacing(t::S2);
    if let Some((label, msg)) = primary {
        buttons = buttons.push(kit::btn(label, style::primary(), Some(msg)));
    }
    if let Some((label, msg)) = secondary {
        buttons = buttons.push(kit::btn(label, style::secondary(), Some(msg)));
    }
    col = col.push(Space::new().height(t::S2));
    col = col.push(buttons);
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(t::GROUND)).into()
}

/// The graphics card is chosen and there is none to use: nothing can be
/// indexed until the processor is chosen.
fn no_card_page(app: &App) -> Element<'_, Message> {
    let shots = app.search.status.library_shots;
    let cpu = estimate::describe_seconds(estimate::seconds(shots, Device::Cpu));
    message_page(
        Glyph::GraphicsCard,
        "There's no graphics card Photo Pipeline can use",
        format!("So nothing is indexed. To index on the processor instead ({cpu} for this library), choose it in Preferences ▸ Search."),
        Some(("Open Preferences", m(Msg::OpenPrefs))),
        Some(("Look for the card again", m(Msg::TryCard))),
    )
}

/// One fact of the invitation: what, and a quieter line under it.
fn fact<'a>(k: &'a str, v: String, sub: Option<&'a str>) -> Element<'a, Message> {
    let mut words = column![text(v).size(t::T_BODY).color(t::INK)].spacing(2).width(Length::Fill);
    if let Some(sub) = sub {
        words = words.push(text(sub).size(t::T_SMALL).color(t::INK3));
    }
    row![text(k).size(t::T_SMALL).color(t::INK3).width(88), words].spacing(t::S3).padding(Padding::from([10.0, 14.0])).into()
}

/// Before the first index: what search does, what indexing costs, and the
/// way to start it.
fn invitation(app: &App) -> Element<'_, Message> {
    let st = &app.search.status;
    let costs = super::search_setup::costs(app);
    let mut facts: Vec<Element<'_, Message>> = vec![
        fact("Reads", format!("Each photo's preview once: {}", costs.read), None),
        fact(
            "Takes",
            costs.takes.clone(),
            Some("Search works on the newest days within a minute. You can keep culling, and stop it from the status bar. Preferences ▸ Search chooses the graphics card or the processor."),
        ),
    ];
    if !st.models_installed {
        facts.push(fact("Downloads", format!("The search model, once: {}", costs.download), None));
    }
    facts.push(fact("Keeps", format!("{}, on this computer, readable only by you. Nothing about your photos is sent anywhere.", super::search_setup::capital(&costs.kept)), None));
    let mut group = column![];
    for (i, f) in facts.into_iter().enumerate() {
        if i > 0 {
            group = group.push(kit::hairline());
        }
        group = group.push(f);
    }
    let action = row![
        kit::btn("Index the library…", style::primary(), Some(m(Msg::IndexLibrary))),
        kit::caption(format!("{} in the library", util::plural(st.library_shots as u64, "photo"))),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center);
    explained(
        "Search the library by what's in your photos",
        "Type what you're looking for, such as a camera, fireworks or a red car at night. To find photos that way, Photo Pipeline first looks at each photo in the library once and keeps what it learns.",
        Some(container(group).width(Length::Fill).style(style::group()).into()),
        "After this, each export is added on its own, in the background.",
        action.into(),
    )
}

/// Downloading the model, or indexing the first photos: what is happening
/// and when search works.
fn preparing(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let (line, action): (String, Element<'_, Message>) = if s.downloading() {
        let (done, total) = s.downloads.iter().fold((0, 0), |(a, b), d| (a + d.done, b + d.total));
        (
            format!("Downloading the search model: {}. Indexing starts as soon as it's in place.", super::search_setup::progress_bytes(done, total)),
            kit::btn("Show the download", style::secondary(), Some(m(Msg::Dialog(DialogMsg::Show)))).into(),
        )
    } else {
        ("Indexing has started, newest days first. Search works on them within a minute.".to_string(), Space::new().into())
    };
    explained(
        "Getting search ready",
        "You can keep culling meanwhile: the status bar shows how far it has got, with × to stop.",
        None,
        &line,
        action,
    )
}

/// The invitation's layout: a title with the magnifier, what to know,
/// optional facts, a quieter line and the action.
fn explained<'a>(title: &'a str, about: &'a str, facts: Option<Element<'a, Message>>, note: &str, action: Element<'a, Message>) -> Element<'a, Message> {
    let mut col = column![
        row![icons::view(Glyph::Search, 26.0, t::INK), text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK)].spacing(t::S3).align_y(Alignment::Center),
        text(about).size(t::T_BODY).color(t::INK2),
    ]
    .spacing(14)
    .max_width(600);
    if let Some(f) = facts {
        col = col.push(f);
    }
    col = col.push(text(note.to_string()).size(t::T_SMALL).color(t::INK3));
    col = col.push(action);
    container(scrollable(container(col).center_x(Length::Fill).padding(Padding { top: 40.0, bottom: t::S5, left: t::S5, right: t::S5 })).style(style::scroll))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::GROUND))
        .into()
}

/// What the completions offer, for the test driver: each pick under its
/// heading, the chosen one marked.
pub fn debug_offer(app: &App) -> String {
    let s = &app.search;
    let o = &s.offer;
    let picks: Vec<String> = o
        .picks
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mark = if s.hi == Some(i) { "*" } else { "" };
            match p {
                Pick::Thing { word, photos, minus } => format!("{mark}{}: {}{word} ({photos} photos)", p.group(), if *minus { "-" } else { "" }),
                Pick::Date { label, text, days, replace } => format!("{mark}{}: {label} → {text:?} ({days} days, replaces {replace})", p.group()),
                Pick::Recent(r) => format!("{mark}{}: {r}", p.group()),
            }
        })
        .collect();
    format!("open={} query={:?} stem={:?} names_later={} picks={picks:?}", s.completions, s.query, o.stem, o.names_later)
}

/// The start page's Things, for the test driver, and what the engine sent
/// that is never offered (people, fursuits, animals).
pub fn debug_things(app: &App) -> String {
    let all = &app.search.things;
    let shown: Vec<String> = res::offered(all).take(START_THINGS).map(|t| format!("{} {}", t.word, t.photos)).collect();
    let kept_out: Vec<String> = all.iter().filter(|t| t.kind.is_subject()).map(|t| format!("{} ({:?})", t.word, t.kind)).collect();
    format!("sent={} shown={shown:?} kept_out={kept_out:?}", all.len())
}

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let s = &app.search;
    let name = |id: RecordId| s.results.as_ref().and_then(|r| r.coll.get(id)).map(|r| r.stem()).unwrap_or_else(|| "?".into());
    // Tiles and their marks side by side (looking each up would be quadratic).
    let runs: Vec<Option<RunMark>> = s.layout.runs.iter().flatten().copied().collect();
    let stacked: Vec<(RecordId, RunMark)> = s.layout.tiles.iter().zip(&runs).filter_map(|(&id, m)| m.filter(|m| m.stacked).map(|m| (id, m))).collect();
    let stacks: Vec<String> = stacked.iter().take(6).map(|&(id, m)| format!("{}x{}", name(id), m.total)).collect();
    let open = runs.iter().flatten().filter(|m| !m.stacked && m.first).count();
    let st = &s.status;
    let mut sel: Vec<String> = s.selection.iter().map(|&id| name(id)).collect();
    sel.sort();
    let sections: Vec<String> = s.layout.sections.iter().map(|x| format!("{}:{}", x.label, x.ids.len())).collect();
    let understood = s.results.as_ref().map(|r| {
        let u = &r.understood;
        format!("words={:?} exclude={:?} dates={:?} names={:?}", u.words, u.exclude, u.dates.map(|d| d.label()), u.names)
    });
    format!(
        "view={:?} page={:?} query={:?} ran={:?} pending={:?} error={:?} photos={} tiles={} sections={:?} fursuits={} fursuits_shown={} animal={:?} understood={:?} stacks={} {:?} open_bursts={} selection={:?} anchor={:?} completions={} hi={:?} recent={:?} from={:?}/{:?} coverage={:?} status=[models={} indexed={}/{} asked={} pass={:?} device={:?} stopped={:?} waiting={} restarts={} exports={}] downloads={:?} dialog={:?} banner_hidden={} steps={} closeness={:.2} scroll={} fake={}",
        app.view,
        page(app),
        s.query,
        s.ran,
        s.pending,
        s.error,
        s.photos(),
        s.layout.tiles.len(),
        sections,
        s.results.as_ref().map_or(0, |r| r.fursuits.len()),
        s.fursuits_shown,
        s.results.as_ref().and_then(|r| r.animal.clone()),
        understood,
        stacked.len(),
        stacks,
        open,
        sel,
        s.anchor.map(name),
        s.completions,
        s.hi,
        app.settings.search_recent,
        s.from,
        s.from_label,
        coverage(st),
        st.models_installed,
        st.indexed_shots,
        st.library_shots,
        st.first_index_asked,
        st.pass,
        st.device,
        st.stopped,
        st.waiting,
        st.helper_restarts,
        st.index_exports,
        s.downloads.iter().map(|d| format!("{:?}:{}/{}:{:?}", d.model, d.done, d.total, d.state)).collect::<Vec<_>>(),
        s.dialog.as_ref().map(|d| (d.stage, d.sums)),
        s.banner_hidden,
        s.steps,
        app.settings.closeness(),
        s.scroll,
        s.link.is_fake(),
    )
}
