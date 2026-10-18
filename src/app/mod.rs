//! The application: state, messages and the update loop (the Python
//! `MainWindow`).  Views live in `main_view`, `viewer` and `calendar`;
//! modal dialogs in `dialogs`.

pub mod bulk;
pub mod calendar;
pub mod commands;
pub mod compare;
pub mod dialogs;
pub mod download_speed;
pub mod fursuit_art;
pub mod fursuit_judge;
pub mod fursuits;
pub mod global_menu;
pub mod hints_ui;
pub mod kit;
pub mod main_view;
pub mod quicklook;
pub mod quality;
pub mod results_pane;
pub mod network;
pub mod people;
pub mod search;
pub mod search_link;
pub mod search_results;
pub mod search_setup;
pub mod preferences;
pub mod preferences_storage;
pub mod style;
pub mod subjects;
pub mod subjects_fursuits;
pub mod columns;
pub mod subjects_merge;
pub mod subjects_editor;
pub mod subjects_cover;
pub mod subjects_split;
pub mod subjects_person;
pub mod subjects_remove;
pub mod subjects_review;
pub mod testing;
pub mod undo;
pub mod viewer;
pub mod viewer_advance;
pub mod viewer_burst;
pub mod viewer_filmstrip;
pub mod viewer_labels;
pub mod viewer_window;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use iced::keyboard::{self, Key, key::Named};
use iced::{Event, Point, Size, Subscription, Task, event, window};

use crate::bus::{self, BgEvent, Bus};
use crate::imaging::loader::Loader;
use crate::imaging::tile_previews::TilePreviews;
use crate::imaging::thumbs::{self, OWNER_CALENDAR, OWNER_MAIN, ThumbCache, ThumbWorkers};
use crate::model::bursts::{Bursts, RunMark, Stacking};
use crate::model::collection::{Collection, RecordId};
use crate::model::filter::{self, FilterState, SortKey};
use crate::model::hints::{self, HintFilter, Hints};
use crate::model::sidecar;
use crate::scan::{self, ScanHandle};
use crate::settings::Settings;
use crate::ui::grid::{GridEvent, MAX_SIZE, MIN_SIZE, Section};
use crate::util;
use commands::Cmd;

const ZOOM_STEP: f32 = 20.0;
const MAIN_CACHE_BYTES: usize = 192 * 1024 * 1024;
const CALENDAR_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// The Wayland app id and X11 window class.  It names the desktop entry
/// `install.sh` installs (`assets/<APP_ID>.desktop.in`): KWin and the Plasma
/// taskbar find the app's icon through it, and show the generic Wayland logo
/// when nothing matches.
pub const APP_ID: &str = "io.github.bealsbe.PhotoPipeline";

/// What every window shares: the app id and icon, and close requests routed
/// through `update`.
pub fn window_settings() -> window::Settings {
    window::Settings {
        icon: crate::ui::icons::window_icon(),
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific { application_id: APP_ID.into(), ..Default::default() },
        ..window::Settings::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Grid,
    Calendar,
    /// Search the library (not kept between runs: the app starts where
    /// search was opened from).
    Search,
    /// People and fursuits (not kept between runs either).
    Subjects,
}

impl View {
    /// A library page: read-only, no filter bar, no marking.
    pub fn is_library(self) -> bool {
        matches!(self, View::Calendar | View::Search | View::Subjects)
    }

    /// The saved `view_index`, numbered as the Python app numbers its views
    /// (0 was its list view, gone here).  Search isn't saved: the app starts
    /// where search was opened from.
    fn index(self) -> u32 {
        match self {
            View::Grid => 1,
            View::Calendar => 2,
            View::Search | View::Subjects => 1,
        }
    }

    /// A saved list view (0), or anything unknown, opens the grid.
    fn from_index(i: u32) -> View {
        match i {
            2 => View::Calendar,
            _ => View::Grid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTarget {
    Working,
    Library,
    ExportLibrary,
    EditCopy,
    /// The edit-copy destination, chosen in Preferences.
    EditCopySetting,
    /// Where Preferences exports the record of corrections.
    ExportCorrections,
}

/// Filter bar state (what the widgets show).
#[derive(Debug, Clone)]
pub struct FilterUi {
    /// Preferences ▸ Show pruned photos: pruned photos stay in All and
    /// Marked.  The Pruned filter shows them either way, To review never.
    pub show_pruned: bool,
    /// At most one of these three is on (see [`Show`]).
    pub marked_only: bool,
    /// Only photos neither pruned nor marked.
    pub to_review: bool,
    /// Only pruned photos.
    pub pruned_only: bool,
    pub sort_key: SortKey,
    pub sort_asc: bool,
    pub date_on: bool,
    pub date_from: NaiveDate,
    pub date_to: NaiveDate,
    /// Only photos with this hint (once analysed).  Not saved: hints
    /// belong to one folder's analysis.
    pub hint: Option<HintFilter>,
}

impl FilterUi {
    fn defaults() -> Self {
        let today = Local::now().date_naive();
        Self {
            show_pruned: false,
            marked_only: false,
            to_review: false,
            pruned_only: false,
            sort_key: SortKey::Date,
            sort_asc: true,
            date_on: false,
            date_from: NaiveDate::from_ymd_opt(chrono::Datelike::year(&today), 1, 1).unwrap_or(today),
            date_to: today,
            hint: None,
        }
    }

    pub fn state(&self) -> FilterState {
        let state = FilterState {
            show_pruned: self.show_pruned,
            show_unmarked: !self.marked_only,
            date_from: self.date_on.then_some(self.date_from),
            date_to: self.date_on.then_some(self.date_to),
            sort_key: self.sort_key,
            sort_asc: self.sort_asc,
            ..FilterState::default()
        };
        match self.show() {
            Show::ToReview => state.only_to_review(),
            Show::Pruned => state.only_pruned(),
            Show::All | Show::Marked => state,
        }
    }

    /// Which photos the "Show" choice lets through.
    pub fn show(&self) -> Show {
        if self.to_review {
            Show::ToReview
        } else if self.marked_only {
            Show::Marked
        } else if self.pruned_only {
            Show::Pruned
        } else {
            Show::All
        }
    }

    fn set_show(&mut self, show: Show) {
        self.to_review = show == Show::ToReview;
        self.marked_only = show == Show::Marked;
        self.pruned_only = show == Show::Pruned;
    }
}

/// The filter bar's four-way choice: every photo, only those still to
/// review (neither pruned nor marked), only those marked for export, or
/// only those pruned.  One choice rather than toggles, because "not yet
/// pruned or marked" can't hold with either of the last two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    All,
    ToReview,
    Marked,
    Pruned,
}

#[derive(Debug, Clone)]
pub enum FilterMsg {
    ToggleMarkedOnly,
    Sort(SortKey),
    ToggleDate,
    OpenDate(bool),
    PickDate(bool, NaiveDate),
    DatePopupMonth(i32),
    CloseDatePopup,
    ClearDates,
    ResetAll,
    Zoom(f32),
    Show(Show),
    /// The hints filter (from the Hints menu), or none.
    Hint(Option<HintFilter>),
}

#[derive(Debug, Clone)]
pub struct DatePopup {
    pub from: bool,
    pub year: i32,
    pub month: u32,
}

#[derive(Debug, Clone)]
pub enum Message {
    Bg(Vec<BgEvent>),
    Event(window::Id, Event, event::Status),
    CloseRequested(window::Id),
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Monitor(Option<Size>),
    ScaleFactor(f32),
    Tick,
    Cmd(Cmd),
    SideBurger,
    SideCollapse,
    MenuOpen(Option<usize>),
    MenuHover(Option<usize>),
    FolderBarCollapse,
    PickFolder(FolderTarget),
    FolderPicked(FolderTarget, Option<PathBuf>),
    Filter(FilterMsg),
    Grid(GridEvent),
    Calendar(calendar::Msg),
    Viewer(viewer::Msg),
    QuickLook(quicklook::Msg),
    Compare(compare::Msg),
    Dialog(dialogs::Msg),
    StatusExpire(u64),
    ShortcutsClose,
    Script(usize),
    ScriptShot(String, window::Screenshot),
    Noop,
    GlobalMenuLinked(Result<(), String>),
    /// The status bar's Undo button: undo that step if it is the latest.
    UndoStep(u64),
    Net(network::Msg),
    Prefs(preferences::Msg),
    /// The analysis: the Hints menu, stopping, the banner.
    Hints(hints_ui::Msg),
    /// A bulk action's confirmation.
    Bulk(bulk::Msg),
    /// The search page, its download dialog and indexing.
    Search(search::Msg),
    /// People and fursuits.
    Subjects(subjects::Msg),
    /// The fursuit-head step: its offers, the processor's question, stop.
    Fursuits(subjects_fursuits::Msg),
    Judge(fursuit_judge::Msg),
}

impl Message {
    /// For the profiler.
    fn kind(&self) -> &'static str {
        match self {
            Message::Bg(_) => "Bg",
            Message::Event(_, Event::Keyboard(_), _) => "Event:key",
            Message::Event(..) => "Event:window",
            Message::CloseRequested(_) => "CloseRequested",
            Message::WindowOpened(_) => "WindowOpened",
            Message::WindowClosed(_) => "WindowClosed",
            Message::Monitor(_) => "Monitor",
            Message::ScaleFactor(_) => "ScaleFactor",
            Message::Tick => "Tick",
            Message::Cmd(_) => "Cmd",
            Message::SideBurger | Message::SideCollapse => "SideMenu",
            Message::MenuOpen(_) | Message::MenuHover(_) => "Menu",
            Message::FolderBarCollapse | Message::PickFolder(_) | Message::FolderPicked(..) => "Folder",
            Message::Filter(_) => "Filter",
            Message::Grid(_) => "Grid",
            Message::Calendar(_) => "Calendar",
            Message::Viewer(_) => "Viewer",
            Message::QuickLook(_) => "QuickLook",
            Message::Compare(_) => "Compare",
            Message::Dialog(_) => "Dialog",
            Message::StatusExpire(_) => "StatusExpire",
            Message::ShortcutsClose => "ShortcutsClose",
            Message::Script(_) | Message::ScriptShot(..) => "Script",
            Message::Noop => "Noop",
            Message::GlobalMenuLinked(_) => "GlobalMenuLinked",
            Message::UndoStep(_) => "UndoStep",
            Message::Net(_) => "Net",
            Message::Prefs(_) => "Prefs",
            Message::Hints(_) => "Hints",
            Message::Bulk(_) => "Bulk",
            Message::Search(_) => "Search",
            Message::Subjects(_) => "Subjects",
            Message::Fursuits(_) => "Fursuits",
            Message::Judge(_) => "Judge",
        }
    }
}

pub struct Status {
    pub text: String,
    seq: u64,
    /// A button after the text: (label, what it sends).
    pub action: Option<(String, Message)>,
}

pub struct Derived {
    /// Every photo shown, by day (a RAW+JPG pair once).
    pub days: Vec<Section>,
    /// What the grid shows, by day: `days` with each stacked burst folded
    /// into one tile, its cover.  Selection, arrow keys and quick look go
    /// by these tiles.
    pub sections: Vec<Section>,
    /// Burst strips and stacks for each section's tiles (parallel to
    /// `sections`).
    pub burst_runs: Vec<Vec<Option<RunMark>>>,
    /// The grid's tiles in order (`sections`, flattened).
    pub tiles: Vec<RecordId>,
    /// Deduped visible records in display order, frames inside stacks
    /// included (what the viewer steps through).
    pub visible: Vec<RecordId>,
    /// The grid as it was before the last burst was stacked or opened, and
    /// the focused tile before and after, so the grid can animate the
    /// change (see `GridUi::restack_seq`).
    pub before: Vec<Section>,
    pub before_runs: Vec<Vec<Option<RunMark>>>,
    pub restack_focus: Option<(RecordId, RecordId)>,
    pub accepted: usize,
    pub key: u64,
    /// Culling progress over the whole working folder, ignoring filters.
    pub progress: filter::Progress,
    /// Per section and tile (parallel to `sections`): spliced to the next
    /// tile, the next press of the same moment.
    pub moment_links: Vec<Vec<bool>>,
    /// Shots shown (before the hints filter) with each hint, by
    /// `HintFilter::index`.
    pub hint_counts: [usize; 5],
    /// With the duplicates filter on: how many shown copies Prune duplicate
    /// copies would prune (those still to review).
    pub copies_to_prune: usize,
}

pub struct GridUi {
    pub selection: HashSet<RecordId>,
    pub anchor: Option<RecordId>,
    pub select_mode: bool,
    pub collapsed: HashSet<NaiveDate>,
    pub scroll: f32,
    pub scroll_seq: u64,
    /// Bumped when bursts are stacked or opened (the grid animates it).
    pub restack_seq: u64,
}

pub struct App {
    pub main_id: Option<window::Id>,
    pub settings: Settings,
    bus: Bus,
    rx: bus::Receiver,
    pub workers: ThumbWorkers,
    pub thumbs: ThumbCache,
    pub loader: Loader,
    /// Pictures for a burst sheet's tiles bigger than their thumbnails.
    pub tile_previews: TilePreviews,
    /// The burst sheet's tile size (0: fit), chosen with Ctrl+wheel,
    /// Ctrl+= / Ctrl+− or the sheet's slider; kept for the session, across
    /// bursts (`ui::burst_sheet::layout_at`).
    pub sheet_tiles: f32,
    /// Writes sidecars off the UI thread.
    saver: sidecar::Saver,
    /// Undo / redo for this working-folder session.
    history: undo::History,
    pub scale_factor: f32,
    pub monitor: Option<Size>,
    window_size: Size,
    window_pos: Option<Point>,

    pub coll: Collection,
    /// The working folder's bursts (shutter presses), from shot metadata.
    pub bursts: Bursts,
    /// The analysis (run on demand) and what it suggests.
    pub quality: quality::Quality,
    /// Which bursts the grid shows folded into stacks.
    pub stacking: Stacking,
    pub folder: Option<PathBuf>,
    pub library: Option<PathBuf>,
    scan: Option<ScanHandle>,
    scan_seq: u64,
    pub scanning: bool,
    scan_dirty: bool,
    last_refresh: Instant,
    /// When the current scan started (profiling).
    scan_started: Instant,

    pub view: View,
    pub filter: FilterUi,
    pub thumb_size: f32,
    pub derived: Derived,
    pub grid: GridUi,

    pub side_expanded: bool,
    pub folder_bar_visible: bool,
    pub folder_bar_collapsed: bool,
    /// File names under the grid's photos (View ▸ Show file names).
    pub show_names: bool,
    pub menu: Option<usize>,
    pub menu_hover: Option<usize>,
    pub date_popup: Option<DatePopup>,
    pub status: Status,
    pub modal: Option<dialogs::Modal>,
    /// A bulk action waiting to be confirmed (in its own window).
    pub bulk: Option<bulk::Confirm>,
    pub viewer: Option<viewer::Viewer>,
    /// Quick look over the grid (Space).
    pub quicklook: Option<quicklook::QuickLook>,
    /// The compare view, in the viewer's window (holding the viewer aside).
    pub compare: Option<compare::Compare>,
    pub shortcuts: Option<window::Id>,
    /// The Preferences window (Ctrl+,).
    pub prefs: preferences::Prefs,
    pub calendar: calendar::Calendar,
    /// The search page (Ctrl+F) and the engine's link.
    pub search: search::Search,
    /// People and fursuits (Ctrl+4).
    pub subjects: subjects::Subjects,
    /// Network shares: probes, failing saves, the banner, the quit prompt.
    pub net: network::Net,
    quitting: bool,
    pub script: Vec<String>,
    /// A scripted grid scroll in progress (test driver).
    pub script_scroll: Option<testing::ScrollDrive>,
    /// The KDE global menu (None without a session bus).
    pub global_menu: Option<global_menu::GlobalMenu>,
    global_menu_linked: bool,
    global_menu_present: bool,
}

pub fn fmt_section_date(d: NaiveDate) -> String {
    let today = Local::now().date_naive();
    let long = format!("{} {}, {}", d.format("%B"), chrono::Datelike::day(&d), chrono::Datelike::year(&d));
    if d == today {
        format!("Today, {long}")
    } else if Some(d) == today.pred_opt() {
        format!("Yesterday, {long}")
    } else {
        format!("{}, {long}", d.format("%A"))
    }
}

impl App {
    pub fn boot() -> (App, Task<Message>) {
        let settings = Settings::load();
        let (bus, rx) = bus::channel();
        let workers = ThumbWorkers::new(bus.clone());
        let loader = Loader::new(bus.clone());
        let tile_previews = TilePreviews::new(bus.clone());
        let saver = {
            let bus = bus.clone();
            sidecar::Saver::new(move |report| {
                let _ = bus.unbounded_send(BgEvent::Saver(std::sync::Arc::new(report)));
            })
        };
        let calendar = calendar::Calendar::new(&settings, bus.clone());
        let search = search::Search::new(bus.clone(), &settings);
        let subjects = subjects::Subjects::new(settings.subjects_tab, &search.link);
        let global_menu = global_menu::GlobalMenu::start(bus.clone(), settings.global_menu_wanted(crate::settings::global_menu_forced_off()));

        let mut filter = FilterUi::defaults();
        filter.show_pruned = settings.filter_pruned;
        filter.marked_only = settings.filter_marked_only;
        filter.to_review = settings.filter_to_review && !filter.marked_only;
        filter.pruned_only = settings.filter_pruned_only && !filter.marked_only && !filter.to_review;
        filter.sort_key = SortKey::parse(&settings.sort_key);
        filter.sort_asc = settings.sort_asc;
        let from = NaiveDate::parse_from_str(&settings.filter_date_from, "%Y-%m-%d").ok();
        let to = NaiveDate::parse_from_str(&settings.filter_date_to, "%Y-%m-%d").ok();
        filter.date_on = from.is_some() || to.is_some();
        if let Some(f) = from {
            filter.date_from = f;
        }
        if let Some(t) = to {
            filter.date_to = t;
        }

        let size = Size::new(settings.window_width.max(640.0), settings.window_height.max(420.0));
        let position = match (settings.window_x, settings.window_y) {
            (Some(x), Some(y)) => window::Position::Specific(Point::new(x, y)),
            _ => window::Position::Centered,
        };
        let (id, open) = window::open(window::Settings {
            size,
            position,
            maximized: settings.window_maximized,
            min_size: Some(Size::new(640.0, 420.0)),
            ..window_settings()
        });

        let mut app = App {
            main_id: Some(id),
            thumb_size: (settings.thumb_size as f32).clamp(MIN_SIZE, MAX_SIZE),
            view: View::from_index(settings.view_index),
            folder_bar_visible: settings.folder_bar_visible,
            folder_bar_collapsed: settings.folder_bar_collapsed,
            show_names: settings.show_file_names,
            stacking: Stacking::new(settings.stack_bursts),
            prefs: preferences::Prefs::new(&settings),
            settings,
            bus,
            rx,
            workers,
            thumbs: ThumbCache::new(OWNER_MAIN, MAIN_CACHE_BYTES),
            loader,
            tile_previews,
            sheet_tiles: 0.0,
            saver,
            history: undo::History::new(),
            scale_factor: 1.0,
            monitor: None,
            window_size: size,
            window_pos: None,
            coll: Collection::new(),
            bursts: Bursts::default(),
            quality: quality::Quality::default(),
            folder: None,
            library: None,
            scan: None,
            scan_seq: 0,
            scanning: false,
            scan_dirty: false,
            last_refresh: Instant::now(),
            scan_started: Instant::now(),
            filter,
            derived: Derived {
                days: Vec::new(),
                sections: Vec::new(),
                burst_runs: Vec::new(),
                tiles: Vec::new(),
                visible: Vec::new(),
                before: Vec::new(),
                before_runs: Vec::new(),
                restack_focus: None,
                accepted: 0,
                key: 0,
                progress: filter::Progress::default(),
                moment_links: Vec::new(),
                hint_counts: [0; 5],
                copies_to_prune: 0,
            },
            grid: GridUi {
                selection: HashSet::new(),
                anchor: None,
                select_mode: false,
                collapsed: HashSet::new(),
                scroll: 0.0,
                scroll_seq: 0,
                restack_seq: 0,
            },
            side_expanded: false,
            menu: None,
            menu_hover: None,
            date_popup: None,
            status: Status { text: "Ready — open a folder to begin".into(), seq: 0, action: None },
            modal: None,
            bulk: None,
            viewer: None,
            quicklook: None,
            compare: None,
            shortcuts: None,
            calendar,
            search,
            subjects,
            net: network::Net::default(),
            quitting: false,
            script: testing::load(),
            script_scroll: None,
            global_menu,
            global_menu_linked: false,
            global_menu_present: false,
        };

        // Library folder, then the view (so the calendar knows its library).
        // Not checked here: on a share that's away, even is_dir() can block
        // the window from opening.  The calendar keeps its saved index and
        // the probe marks the library as unreachable.
        let lib = PathBuf::from(&app.settings.library_folder);
        let mut tasks = vec![open.map(Message::WindowOpened)];
        if !app.settings.library_folder.is_empty() {
            let lib = util::normalize(&lib);
            app.library = Some(lib.clone());
            app.calendar.set_library_root(app.library.clone(), &app.workers);
            tasks.push(network::folder_changed(&mut app, &lib, false));
        }
        let view = app.view;
        tasks.push(app.switch_view(view));
        // The scan checks the folder on its own thread; one that's gone
        // or unreachable shows the network banner.
        let last = PathBuf::from(&app.settings.last_folder);
        if !app.settings.last_folder.is_empty() {
            tasks.push(app.start_scan(util::normalize(&last)));
        }
        if !app.script.is_empty() {
            tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(800)), |_| Message::Script(0)));
        }
        (app, Task::batch(tasks))
    }

    pub fn title(&self, id: window::Id) -> String {
        if Some(id) == self.main_id {
            return "Photo Pipeline".into();
        }
        if let Some(v) = &self.viewer {
            if v.id == id {
                return v.title(self);
            }
        }
        if let Some(c) = self.compare.as_ref().filter(|c| c.id() == id) {
            return c.title();
        }
        if Some(id) == self.shortcuts {
            return "Keyboard Shortcuts".into();
        }
        if Some(id) == self.prefs.window {
            return "Preferences".into();
        }
        String::new()
    }

    pub fn theme(&self, _id: window::Id) -> iced::Theme {
        style::theme()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run_with(self.rx.clone(), bus::stream).map(Message::Bg),
            event::listen_with(|ev, status, id| match &ev {
                Event::Keyboard(keyboard::Event::KeyPressed { .. }) => Some(Message::Event(id, ev, status)),
                Event::Window(window::Event::Resized(_))
                | Event::Window(window::Event::Moved(_))
                | Event::Window(window::Event::Rescaled(_))
                | Event::Window(window::Event::Focused) => Some(Message::Event(id, ev, status)),
                _ => None,
            }),
            window::close_requests().map(Message::CloseRequested),
            window::close_events().map(Message::WindowClosed),
        ];
        if self.calendar.animating() || self.scan_dirty || self.script_scroll.is_some() {
            subs.push(window::frames().map(|_| Message::Tick));
        }
        // A download's speed, and a stall (when no progress comes at all).
        if self.search.downloads.iter().any(|d| d.state == crate::ml::download::DownloadState::Downloading) {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Search(search::Msg::Clock)));
        }
        Subscription::batch(subs)
    }

    pub fn view(&self, id: window::Id) -> iced::Element<'_, Message> {
        if Some(id) == self.main_id {
            let _t = crate::profile::span("view", "main");
            return main_view::view(self);
        }
        if let Some(v) = &self.viewer {
            if v.id == id {
                let _t = crate::profile::span("view", "viewer");
                let body = network::in_window(self, viewer::view(self, v));
                return match bulk::overlay(self, id) {
                    Some(dialog) => iced::widget::stack![body, dialog].width(iced::Length::Fill).height(iced::Length::Fill).into(),
                    None => body,
                };
            }
        }
        if let Some(c) = self.compare.as_ref().filter(|c| c.id() == id) {
            let _t = crate::profile::span("view", "compare");
            return network::in_window(self, compare::view(self, c));
        }
        if Some(id) == self.shortcuts {
            return dialogs::shortcuts_view();
        }
        if Some(id) == self.prefs.window {
            return preferences::view(self);
        }
        iced::widget::space().into()
    }

    // ── status ──────────────────────────────────────────────────────────

    pub fn show_status(&mut self, text: impl Into<String>, timeout_ms: u64) -> Task<Message> {
        self.status.text = text.into();
        self.status.action = None;
        self.status.seq += 1;
        let seq = self.status.seq;
        if timeout_ms == 0 {
            return Task::none();
        }
        Task::perform(tokio::time::sleep(Duration::from_millis(timeout_ms)), move |_| Message::StatusExpire(seq))
    }

    /// A status with a button after it ("Moved 6 files to the Trash  Undo").
    /// It stays until another status replaces it.
    pub fn show_status_action(&mut self, text: impl Into<String>, label: &str, msg: Message) -> Task<Message> {
        let t = self.show_status(text, 0);
        self.status.action = Some((label.into(), msg));
        t
    }

    fn update_status_count(&mut self) {
        if self.view.is_library() || self.coll.is_empty() || self.scanning {
            return;
        }
        let total = self.coll.len();
        let shown = self.derived.accepted;
        let text = if shown != total {
            format!("Showing {shown} of {total} files (filter active)")
        } else {
            format!("{total} files")
        };
        let _ = self.show_status(text, 0);
    }

    // ── derived views ───────────────────────────────────────────────────

    pub fn refresh_views(&mut self) {
        let _t = crate::profile::span("refresh", "views");
        let state = self.filter.state();
        // Bursts change only when files come, go or pair up (not on marks);
        // they are found once the scan has paired the folder.
        if self.scanning {
            if !self.bursts.is_empty() {
                self.bursts = Bursts::default();
            }
        } else if !self.bursts.is_current(&self.coll) {
            self.bursts = Bursts::find(&self.coll);
        }
        // Hints first: the hints filter needs them, and a stack's cover is
        // its sharpest frame once analysed.
        quality::refresh(self);
        let sorted = filter::visible_sorted(&self.coll, &state);
        let mut visible = filter::dedup_pairs(&sorted, &self.coll);
        let mut accepted = sorted.len();
        self.derived.hint_counts = hint_counts(&self.quality.hints, &self.coll, &visible);
        if let Some(f) = self.filter.hint {
            let (h, coll) = (&self.quality.hints, &self.coll);
            let has = |id: RecordId| shot_has(h, coll, f, id);
            visible.retain(|&id| has(id));
            accepted = sorted.iter().filter(|&&id| has(id)).count();
        }
        let groups = filter::group_by_date(&visible, &self.coll, &state);
        self.derived.days = groups
            .into_iter()
            .map(|g| Section { label: fmt_section_date(g.date), date: g.date, photos: g.ids.len(), ids: g.ids })
            .collect();
        self.build_tiles();
        self.derived.accepted = accepted;
        self.derived.visible = self.derived.days.iter().flat_map(|s| s.ids.iter().copied()).collect();
        self.derived.copies_to_prune = match self.filter.hint {
            Some(HintFilter::Duplicates) => self.quality.hints.prune_duplicates(&self.coll, &self.derived.visible).prune.len(),
            _ => 0,
        };
        // Pairs form when the scan completes; counting shots before then
        // would count every file.
        self.derived.progress = if self.scanning { filter::Progress::default() } else { filter::progress(&self.coll) };
        self.derived.key = self.derived.key.wrapping_add(1);
        self.keep_grid_selection();
        self.last_refresh = Instant::now();
    }

    /// The grid's tiles from `derived.days`, each stacked burst folded into
    /// its cover.  Cheap (no sorting, no burst finding), so stacking and
    /// opening bursts runs only this.
    fn build_tiles(&mut self) {
        let _t = crate::profile::span("refresh", "grid tiles");
        let flipped = self.stacking.flipped(&self.bursts);
        let all = self.stacking.all();
        let stacked = |b| all != flipped.contains(&b);
        let mut sections = Vec::with_capacity(self.derived.days.len());
        let mut runs = Vec::with_capacity(self.derived.days.len());
        let mut links = Vec::with_capacity(self.derived.days.len());
        let h = &self.quality.hints;
        for day in &self.derived.days {
            let (ids, marks) = self.bursts.tiles(&day.ids, stacked, |run| self.quality.sharpest_of(run), &self.coll);
            // Presses of one moment (once analysed) are spliced together.
            links.push(if h.is_empty() { Vec::new() } else { hints::moment_links(&ids, &marks, |id| h.joined_moment(id)) });
            sections.push(Section { date: day.date, label: day.label.clone(), photos: day.photos, ids });
            runs.push(marks);
        }
        self.derived.tiles = sections.iter().flat_map(|s| s.ids.iter().copied()).collect();
        self.derived.sections = sections;
        self.derived.burst_runs = runs;
        self.derived.moment_links = links;
    }

    /// The grid's selection once its tiles have changed: a photo folded
    /// into a stack hands its selection (and focus) to the stack; anything
    /// else no longer on screen is dropped.
    fn keep_grid_selection(&mut self) {
        let on_grid: HashSet<RecordId> = self.derived.tiles.iter().copied().collect();
        let all_there = self.grid.selection.iter().all(|id| on_grid.contains(id)) && self.grid.anchor.is_none_or(|a| on_grid.contains(&a));
        if all_there {
            return;
        }
        let covers = self.stack_covers();
        let home = |id: RecordId| self.bursts.of(id).and_then(|b| covers.get(&b).copied());
        let (selection, anchor) = regather(&self.grid.selection, self.grid.anchor, &on_grid, home);
        self.grid.selection = selection;
        self.grid.anchor = anchor;
    }

    /// Each stacked burst's tile (its first, should a sort split it).
    fn stack_covers(&self) -> HashMap<crate::model::bursts::BurstId, RecordId> {
        let mut covers = HashMap::new();
        for (sec, marks) in self.derived.sections.iter().zip(&self.derived.burst_runs) {
            for (&id, m) in sec.ids.iter().zip(marks) {
                if let Some(m) = m.filter(|m| m.stacked) {
                    covers.entry(m.burst).or_insert(id);
                }
            }
        }
        covers
    }

    /// `id` is a stacked burst's tile in the grid (its cover photo).
    pub fn is_stack(&self, id: RecordId) -> bool {
        self.bursts.of(id).is_some_and(|b| self.stack_covers().get(&b) == Some(&id))
    }

    /// What P or M on a stacked burst says instead of marking.
    pub fn stack_mark_hint(&mut self) -> Task<Message> {
        self.show_status("A stacked burst can't be marked. Open it (B or Enter) to mark its frames, or press G to unstack it.", 5000)
    }

    /// The tile that shows `id` in the grid: itself, or the stack it is
    /// folded into.
    pub fn tile_of(&self, id: RecordId) -> Option<RecordId> {
        if self.derived.tiles.contains(&id) {
            return Some(id);
        }
        let b = self.bursts.of(id)?;
        self.stack_covers().get(&b).copied()
    }

    /// Fold bursts or open them again after `stacking` changed: only the
    /// grid's tiles are rebuilt (bursts aren't found again), and the grid
    /// animates the change, holding `anchor` (the tile acted on, or the
    /// focused one) where it was on screen.
    pub fn restack(&mut self, anchor: Option<RecordId>) {
        let _t = crate::profile::span("refresh", "restack");
        self.derived.before = std::mem::take(&mut self.derived.sections);
        self.derived.before_runs = std::mem::take(&mut self.derived.burst_runs);
        self.build_tiles();
        self.keep_grid_selection();
        self.derived.restack_focus = anchor.and_then(|a| Some((a, self.tile_of(a)?)));
        self.derived.key = self.derived.key.wrapping_add(1);
        self.grid.restack_seq += 1;
    }

    /// The grid's focused photo: the one last clicked or stepped to, while
    /// selected, else a lone selected photo.
    pub fn grid_focus(&self) -> Option<RecordId> {
        let sel = &self.grid.selection;
        self.grid.anchor.filter(|a| sel.contains(a)).or_else(|| if sel.len() == 1 { sel.iter().next().copied() } else { None })
    }

    /// The burst of the grid's focused photo (what G stacks or opens).
    pub fn focused_burst(&self) -> Option<crate::model::bursts::BurstId> {
        self.bursts.of(self.grid_focus()?)
    }

    /// Stack `id`'s burst in the grid, or open it (G, or its tab).
    pub fn toggle_burst(&mut self, id: RecordId) -> Task<Message> {
        let Some(b) = self.bursts.of(id) else {
            let name = self.coll.get(id).map(|r| r.stem()).unwrap_or_default();
            return self.show_status(format!("{name} isn't part of a burst."), 4000);
        };
        self.stacking.toggle(&self.bursts, b);
        self.restack(Some(id));
        quicklook::follow(self)
    }

    /// Shift+G: every burst stacked, or every frame shown (saved).
    fn toggle_all_bursts(&mut self) -> Task<Message> {
        let all = !self.stacking.all();
        self.stacking.set_all(all);
        self.settings.stack_bursts = all;
        self.settings.save_in_background();
        let anchor = self.grid_focus();
        self.restack(anchor);
        let n = self.bursts.len();
        let text = match (all, n) {
            (_, 0) => "No bursts in this folder.".to_string(),
            (true, n) => format!("{} burst{} stacked. G opens the selected one.", util::thousands(n as u64), util::s(n)),
            (false, _) => "Every frame of every burst shows.".to_string(),
        };
        let status = self.show_status(text, 4000);
        Task::batch([status, quicklook::follow(self)])
    }

    // ── scanning ────────────────────────────────────────────────────────

    pub fn start_scan(&mut self, path: PathBuf) -> Task<Message> {
        let path = util::normalize(&path);
        if let Some(old) = self.scan.take() {
            old.cancel();
        }
        // A working-folder viewer shows records from the old folder (and so
        // does a compare, which hands the window back to its viewer first).
        compare::release(self);
        let mut close = Task::none();
        if self.viewer.as_ref().is_some_and(|v| !v.read_only) {
            if let Some(v) = self.viewer.take() {
                close = window::close(v.id);
            }
        }
        self.folder = Some(path.clone());
        self.quicklook = None;
        self.bulk = None;
        // Hints belong to the folder analysed.
        self.filter.hint = None;
        quality::on_folder_changed(self);
        self.stacking.forget();
        self.coll.clear();
        self.history.clear();
        self.thumbs.clear(&self.workers);
        self.grid.selection.clear();
        self.grid.anchor = None;
        self.grid.scroll = 0.0;
        self.grid.scroll_seq += 1;
        self.refresh_views();
        self.scan_seq += 1;
        self.scan_started = Instant::now();
        self.scanning = true;
        self.scan_dirty = false;
        let _ = self.show_status("Scanning…", 0);
        let net = network::folder_changed(self, &path, true);
        self.scan = Some(scan::start(self.scan_seq, path, self.bus.clone(), true));
        Task::batch([close, net])
    }

    fn on_scan_complete(&mut self, count: usize) -> Task<Message> {
        crate::profile::record(("scan", "open to complete"), self.scan_started.elapsed());
        let _t = crate::profile::span("scan", "finishing on the UI thread");
        self.scanning = false;
        self.scan_dirty = false;
        // Every RAW+JPG with the same name in the same folder is a pair.
        self.coll.build_pairs();
        if let Err(e) = self.load_marks() {
            // Marks that can't be read mustn't be overwritten by new ones:
            // show nothing until the folder can be read again.
            self.coll.clear();
            self.refresh_views();
            let name = self.folder.as_deref().map(util::file_name).unwrap_or_default();
            let t = self.show_status(format!("Couldn't read the marks in {name}: {e}"), 0);
            return Task::batch([t, network::on_scan_error(self, &e.to_string())]);
        }
        self.history.reset_baseline(&self.coll);
        self.refresh_views();
        let pairs = self.coll.stats().paired / 2;
        let t = self.show_status(
            format!("Found {count} file{}, {pairs} RAW+JPG pair{}", util::s(count), util::s(pairs)),
            0,
        );
        self.update_status_count();
        // What an earlier analysis of this folder found, from the cache (no
        // photo is read; analysing again is only ever on request).
        let cached = quality::start_cached(self);
        Task::batch([t, cached])
    }

    /// Apply the folder's sidecars.  Saves still waiting (or failing) win
    /// over what's on disk.  `Err` when they can't be read because the
    /// folder can't be reached.
    fn load_marks(&mut self) -> std::io::Result<()> {
        let Some(folder) = self.folder.clone() else { return Ok(()) };
        // A save still on its way would otherwise be read back stale.
        self.saver.settle(&folder);
        let read = |kind, file| match self.saver.read(&folder, kind, file) {
            Ok(text) => Ok(text),
            Err(e) if crate::ops::netfs::classify_error(&e) == crate::ops::netfs::Failure::Other => Ok(None),
            Err(e) => Err(e),
        };
        let pruned = read(sidecar::Kind::Prune, sidecar::PRUNE_FILE)?.and_then(|t| sidecar::parse_prune_marks(&folder, &t));
        let marked = read(sidecar::Kind::Export, sidecar::EXPORT_FILE)?.and_then(|t| sidecar::parse_export_marks(&folder, &t));
        let crops = read(sidecar::Kind::Crops, sidecar::CROPS_FILE)?.map(|t| sidecar::parse_crops(&folder, &t)).unwrap_or_default();
        if let Some(pruned) = pruned {
            for p in &pruned {
                if let Some(id) = self.coll.id_of(p) {
                    if let Some(r) = self.coll.get_mut(id) {
                        r.is_pruned = true;
                    }
                }
            }
        }
        if let Some(marked) = marked {
            for p in &marked {
                if let Some(id) = self.coll.id_of(p) {
                    if let Some(r) = self.coll.get_mut(id) {
                        r.is_marked = true;
                    }
                }
            }
        }
        for (p, crop) in crops {
            if let Some(id) = self.coll.id_of(&p) {
                if let Some(r) = self.coll.get_mut(id) {
                    r.crop = Some(crop);
                }
            }
        }
        self.coll.recount_flags();
        Ok(())
    }

    // ── persistence ─────────────────────────────────────────────────────

    /// Sidecar saves are queued for the saver's thread (failures come back
    /// as `BgEvent::Saver` and are kept and retried); they return a task
    /// for symmetry with the other updates.
    pub fn save_prune_marks(&mut self) -> Task<Message> {
        let Some(folder) = self.folder.clone() else { return Task::none() };
        let paths: Vec<PathBuf> = self.coll.iter().filter(|r| r.is_pruned).map(|r| r.path.clone()).collect();
        let text = sidecar::prune_text(&folder, paths.iter().map(|p| p.as_path()));
        self.saver.save_text(folder, sidecar::Kind::Prune, sidecar::PRUNE_FILE, Some(text));
        network::after_save(self)
    }

    pub fn save_export_marks(&mut self) -> Task<Message> {
        let Some(folder) = self.folder.clone() else { return Task::none() };
        let paths: Vec<PathBuf> = self.coll.iter().filter(|r| r.is_marked).map(|r| r.path.clone()).collect();
        let text = sidecar::export_text(&folder, paths.iter().map(|p| p.as_path()));
        self.saver.save_text(folder, sidecar::Kind::Export, sidecar::EXPORT_FILE, Some(text));
        network::after_save(self)
    }

    pub fn save_crops(&mut self) -> Task<Message> {
        let Some(folder) = self.folder.clone() else { return Task::none() };
        let crops: Vec<(PathBuf, crate::model::crop::Crop)> = self
            .coll
            .iter()
            .filter(|r| r.is_cropped())
            .filter_map(|r| r.crop.clone().map(|c| (r.path.clone(), c)))
            .collect();
        let text = sidecar::crops_text(&folder, crops.iter().map(|(p, c)| (p.as_path(), c)));
        self.saver.save_text(folder, sidecar::Kind::Crops, sidecar::CROPS_FILE, text);
        network::after_save(self)
    }


    fn save_session(&mut self) {
        let s = &mut self.settings;
        s.view_index = if self.view == View::Search { self.search.from.index() } else { self.view.index() };
        s.filter_pruned = self.filter.show_pruned;
        s.filter_marked_only = self.filter.marked_only;
        s.filter_to_review = self.filter.to_review;
        s.filter_pruned_only = self.filter.pruned_only;
        s.sort_key = self.filter.sort_key.as_str().into();
        s.sort_asc = self.filter.sort_asc;
        s.filter_date_from = if self.filter.date_on { self.filter.date_from.format("%Y-%m-%d").to_string() } else { String::new() };
        s.filter_date_to = if self.filter.date_on { self.filter.date_to.format("%Y-%m-%d").to_string() } else { String::new() };
        s.last_folder = self.folder.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        s.library_folder = self.library.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        s.folder_bar_visible = self.folder_bar_visible;
        s.folder_bar_collapsed = self.folder_bar_collapsed;
        s.show_file_names = self.show_names;
        s.thumb_size = self.thumb_size as u32;
        let (mode, year, month) = self.calendar.save_state();
        s.calendar_mode = mode;
        s.calendar_year = year;
        s.calendar_month = month;
        s.window_width = self.window_size.width;
        s.window_height = self.window_size.height;
        if let Some(p) = self.window_pos {
            s.window_x = Some(p.x);
            s.window_y = Some(p.y);
        }
        let _ = self.settings.save();
    }

    // ── views ───────────────────────────────────────────────────────────

    pub fn switch_view(&mut self, v: View) -> Task<Message> {
        let was = self.view;
        self.view = v;
        self.menu = None;
        if v != was {
            self.quicklook = None;
        }
        // Back from a day opened from search: search's own back button
        // still returns where search was opened from.
        let from = if was == View::Calendar && self.calendar.is_from_search() { self.calendar.back_view() } else { was };
        if v != View::Calendar {
            self.calendar.leave_search_day();
        }
        if v == View::Search {
            self.calendar.deactivate();
            search::arriving(self, from);
            let text = self.calendar.status_text();
            return self.show_status(text, 0);
        }
        if v == View::Subjects {
            self.calendar.deactivate();
            let t = subjects::arriving(self);
            let text = self.calendar.status_text();
            return Task::batch([t, self.show_status(text, 0)]);
        }
        if v == View::Calendar {
            self.calendar.activate(&self.workers);
            let text = self.calendar.status_text();
            return self.show_status(text, 0);
        }
        self.calendar.deactivate();
        let mut t = Task::none();
        if self.coll.is_empty() && !self.scanning {
            t = self.show_status("Ready — open a folder to begin", 0);
        }
        self.update_status_count();
        t
    }

    // ── prune / mark / crop propagation ─────────────────────────────────

    /// Copy each record's flag to its pair, like `_refresh_prune`.
    fn propagate(&mut self, ids: &[RecordId], get: fn(&crate::model::record::PhotoRecord) -> bool, set: fn(&mut crate::model::record::PhotoRecord, bool)) -> Vec<RecordId> {
        let mut all: Vec<RecordId> = ids.to_vec();
        let mut i = 0;
        while i < all.len() {
            let id = all[i];
            if let (Some(pair), Some(v)) = (self.coll.find_pair(id), self.coll.get(id).map(get)) {
                if self.coll.get(pair).map(get) != Some(v) {
                    if let Some(p) = self.coll.get_mut(pair) {
                        set(p, v);
                    }
                    all.push(pair);
                }
            }
            i += 1;
        }
        all
    }

    pub fn refresh_prune(&mut self, ids: &[RecordId]) -> Task<Message> {
        let all = self.propagate(ids, |r| r.is_pruned, |r, v| r.is_pruned = v);
        undo::note(self, undo::Field::Prune, &all);
        self.coll.recount_flags();
        let place = self.review_place();
        self.refresh_views();
        self.keep_review_place(place);
        self.update_status_count();
        self.save_prune_marks()
    }

    pub fn refresh_mark(&mut self, ids: &[RecordId]) -> Task<Message> {
        let all = self.propagate(ids, |r| r.is_marked, |r, v| r.is_marked = v);
        undo::note(self, undo::Field::Mark, &all);
        self.coll.recount_flags();
        let place = self.review_place();
        self.refresh_views();
        self.keep_review_place(place);
        self.update_status_count();
        self.save_export_marks()
    }

    /// A photo can leave the view as soon as its mark changes: under To
    /// review, Marked or Pruned when it no longer fits, and under All when
    /// it is pruned with Show pruned photos off.  Note where the single
    /// selected photo sat, so the selection can move on to the photo that
    /// takes its place and the pile can be worked through with P and M
    /// alone.  (When the photo stays, its selection does too, and
    /// `keep_review_place` does nothing.)
    fn review_place(&self) -> Option<usize> {
        match self.view {
            View::Grid if !self.grid.select_mode && self.grid.selection.len() == 1 => {
                let id = self.grid.selection.iter().next()?;
                self.derived.tiles.iter().position(|x| x == id)
            }
            _ => None,
        }
    }

    fn keep_review_place(&mut self, place: Option<usize>) {
        let Some(i) = place else { return };
        match self.view {
            View::Grid if self.grid.selection.is_empty() => {
                if let Some(&id) = self.derived.tiles.get(i).or(self.derived.tiles.last()) {
                    self.grid.selection.insert(id);
                    self.grid.anchor = Some(id);
                }
            }
            _ => {}
        }
    }

    pub fn refresh_crop(&mut self, ids: &[RecordId]) -> Task<Message> {
        let mut all: Vec<RecordId> = ids.to_vec();
        let mut i = 0;
        while i < all.len() {
            let id = all[i];
            if let Some(pair) = self.coll.find_pair(id) {
                let crop = self.coll.get(id).and_then(|r| r.crop.clone());
                if self.coll.get(pair).map(|p| p.crop.clone()) != Some(crop.clone()) {
                    if let Some(p) = self.coll.get_mut(pair) {
                        p.crop = crop;
                    }
                    all.push(pair);
                }
            }
            i += 1;
        }
        undo::note(self, undo::Field::Crop, &all);
        let t1 = self.save_crops();
        let n = self.coll.iter().filter(|r| r.is_cropped()).count();
        let touched_cropped = all.first().and_then(|&id| self.coll.get(id)).is_some_and(|r| r.is_cropped());
        let verb = if touched_cropped { "Crop saved" } else { "Crop cleared" };
        let t2 = self.show_status(format!("{verb}. {n} file{} in this folder {} cropped.", util::s(n), if n == 1 { "is" } else { "are" }), 4000);
        Task::batch([t1, t2])
    }

    fn toggle_flag_selected(&mut self, prune: bool) -> Task<Message> {
        let mut hint = Task::none();
        let ids: Vec<RecordId> = match self.view {
            // A stacked burst can't be marked at all: only photos you can
            // see on their own are.
            View::Grid => {
                let selected: Vec<RecordId> =
                    self.derived.tiles.iter().copied().filter(|id| self.grid.selection.contains(id)).collect();
                let open: Vec<RecordId> = selected.iter().copied().filter(|&id| !self.is_stack(id)).collect();
                if open.len() < selected.len() {
                    hint = self.stack_mark_hint();
                }
                open
            }
            View::Calendar | View::Search | View::Subjects => return Task::none(),
        };
        if ids.is_empty() {
            return hint;
        }
        let marked = if prune {
            let all = ids.iter().all(|&id| self.coll.get(id).is_some_and(|r| r.is_pruned));
            for &id in &ids {
                if let Some(r) = self.coll.get_mut(id) {
                    r.is_pruned = !all;
                }
            }
            self.refresh_prune(&ids)
        } else {
            let all = ids.iter().all(|&id| self.coll.get(id).is_some_and(|r| r.is_marked));
            for &id in &ids {
                if let Some(r) = self.coll.get_mut(id) {
                    r.is_marked = !all;
                }
            }
            self.refresh_mark(&ids)
        };
        Task::batch([marked, hint])
    }

    // ── selection ───────────────────────────────────────────────────────

    fn section_of(&self, id: RecordId) -> Option<usize> {
        self.derived.sections.iter().position(|s| s.ids.contains(&id))
    }

    fn grid_click(&mut self, id: RecordId, mods: keyboard::Modifiers) {
        if self.grid.select_mode {
            if !self.grid.selection.remove(&id) {
                self.grid.selection.insert(id);
            }
            self.grid.anchor = Some(id);
            return;
        }
        let sec = self.section_of(id);
        // Browse mode keeps the highlight within one section.
        let same: HashSet<RecordId> =
            sec.map(|s| self.derived.sections[s].ids.iter().copied().collect()).unwrap_or_default();
        self.grid.selection.retain(|x| same.contains(x));
        if mods.control() {
            if !self.grid.selection.remove(&id) {
                self.grid.selection.insert(id);
            }
            self.grid.anchor = Some(id);
        } else if mods.shift() {
            let ids = sec.map(|s| self.derived.sections[s].ids.clone()).unwrap_or_default();
            match self.grid.anchor.and_then(|a| ids.iter().position(|&x| x == a)) {
                Some(a) => {
                    let b = ids.iter().position(|&x| x == id).unwrap_or(a);
                    let (lo, hi) = (a.min(b), a.max(b));
                    self.grid.selection = ids[lo..=hi].iter().copied().collect();
                }
                None => {
                    self.grid.selection = [id].into();
                    self.grid.anchor = Some(id);
                }
            }
        } else {
            self.grid.selection = [id].into();
            self.grid.anchor = Some(id);
        }
    }

    pub fn set_select_mode(&mut self, on: bool) {
        self.grid.select_mode = on;
        if !on {
            self.grid.selection.clear();
        }
    }

    // ── viewer ──────────────────────────────────────────────────────────

    /// Open the viewer on `id`, stepping through the grid's order, landing
    /// on a burst's frame as `land` says.
    pub(super) fn open_viewer_working(&mut self, id: RecordId, land: viewer_burst::Land) -> Task<Message> {
        let items = self.derived.visible.clone();
        let index = items.iter().position(|&x| x == id).unwrap_or(0);
        viewer::open_as(self, viewer::Source::Working, items, index, false, land)
    }

    // ── keyboard ────────────────────────────────────────────────────────

    pub fn on_key_public(&mut self, id: window::Id, key: Key, mods: keyboard::Modifiers) -> Task<Message> {
        self.on_key(id, key, mods, false)
    }

    fn on_key(&mut self, id: window::Id, key: Key, mods: keyboard::Modifiers, captured: bool) -> Task<Message> {
        // The quit prompt is in every window, and takes their keys.
        if self.net.quit.is_some() && Some(id) != self.shortcuts {
            return network::on_key(self, &key);
        }
        // A bulk action's question takes its window's keys.
        if self.bulk.as_ref().is_some_and(|b| b.window == id) {
            return bulk::on_key(self, &key);
        }
        if let Some(v) = &self.viewer {
            if v.id == id {
                if captured {
                    return Task::none();
                }
                return viewer::on_key(self, key, mods);
            }
        }
        if self.compare.as_ref().is_some_and(|c| c.id() == id) {
            if captured {
                return Task::none();
            }
            return compare::on_key(self, key, mods);
        }
        if Some(id) == self.shortcuts {
            if key == Key::Named(Named::Escape) {
                return self.update(Message::ShortcutsClose);
            }
            return Task::none();
        }
        if Some(id) == self.prefs.window {
            return preferences::on_key(self, key, mods);
        }
        if Some(id) != self.main_id {
            return Task::none();
        }

        // Open menu: Escape / arrows / Enter.
        if let Some(m) = self.menu {
            return main_view::menu_key(self, m, &key);
        }
        if self.date_popup.is_some() {
            if key == Key::Named(Named::Escape) {
                self.date_popup = None;
            }
            return Task::none();
        }
        // The search page's download dialog takes the keys while open.
        if self.search.dialog.is_some() {
            return search_setup::dialog_key(self, &key);
        }
        // Esc on the search page closes its list, even when the field took
        // it, and never leaves the page.
        if self.view == View::Search && key == Key::Named(Named::Escape) && self.quicklook.is_none() && self.modal.is_none() {
            if self.search.steps {
                self.search.steps = false;
                return Task::none();
            }
            return search::escape(self);
        }
        // Esc on People and fursuits, even when a field took it: a
        // dialog, a menu or the name field first, then the page below.
        if self.view == View::Subjects && key == Key::Named(Named::Escape) && self.quicklook.is_none() && self.modal.is_none() {
            return subjects::escape(self);
        }
        // So does the calendar's choice of folders for a day.
        if self.view == View::Calendar && self.calendar.choice.is_some() && self.modal.is_none() {
            return calendar::choice_key(self, &key);
        }
        // The Hints menu, like the date picker, takes the keys while open.
        if self.quality.menu {
            if key == Key::Named(Named::Escape) {
                self.quality.menu = false;
            }
            return Task::none();
        }
        if self.net.quit.is_some() {
            return network::on_key(self, &key);
        }
        if self.modal.is_some() {
            if captured {
                return Task::none();
            }
            return dialogs::on_key(self, &key, mods);
        }
        if captured {
            return Task::none();
        }
        // Menu mnemonics (the global menu handles its own).
        if mods.alt() && !mods.control() && !self.global_menu_on() {
            if let Key::Character(c) = &key {
                let c = c.to_lowercase();
                if let Some(i) = commands::MENUS.iter().position(|m| m.1.to_string() == c) {
                    self.menu = Some(i);
                    self.menu_hover = None;
                }
            }
            return Task::none();
        }
        if self.quicklook.is_some() {
            if let Some(t) = quicklook::on_key(self, &key, mods) {
                return t;
            }
        }
        // The viewer's own commands (Shift+arrows) leave their keys to the
        // page here: the grid's Shift+arrows extend its selection.
        if let Some(cmd) = Cmd::from_key(&key, mods).filter(|c| !c.viewer_only()) {
            let enabled = !self.view.is_library() || !matches!(cmd, Cmd::SelectMode) || self.view == View::Subjects;
            if enabled {
                return self.update(Message::Cmd(cmd));
            }
        }
        if self.side_expanded && key == Key::Named(Named::Escape) {
            self.side_expanded = false;
            return Task::none();
        }
        match self.view {
            View::Calendar => calendar::on_key(self, &key, mods),
            View::Search => search::on_key(self, &key, mods),
            View::Subjects => subjects::on_key(self, &key, mods),
            View::Grid => self.grid_key(&key, mods),
        }
    }

    fn grid_key(&mut self, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
        match key {
            Key::Character(c) if !mods.control() && (c.as_str() == "p" || c.as_str() == "P") => self.toggle_flag_selected(true),
            Key::Named(Named::Delete) => self.toggle_flag_selected(true),
            Key::Character(c) if !mods.control() && (c.as_str() == "m" || c.as_str() == "M") => self.toggle_flag_selected(false),
            Key::Character(c) if mods.control() && c.as_str().eq_ignore_ascii_case("a") => {
                self.grid.selection = self
                    .derived
                    .sections
                    .iter()
                    .filter(|s| !self.grid.collapsed.contains(&s.date))
                    .flat_map(|s| s.ids.iter().copied())
                    .collect();
                Task::none()
            }
            Key::Named(Named::Escape) => {
                self.grid.selection.clear();
                if self.grid.select_mode {
                    self.set_select_mode(false);
                }
                Task::none()
            }
            Key::Named(Named::Enter) => match self.grid.anchor.filter(|a| self.grid.selection.contains(a)) {
                Some(id) if !self.grid.select_mode => self.open_viewer_working(id, viewer_burst::Land::Auto),
                _ => Task::none(),
            },
            Key::Named(Named::ArrowLeft) | Key::Named(Named::ArrowRight) if !self.grid.select_mode => {
                let next = arrow_step(&self.derived.tiles, self.grid.anchor, *key == Key::Named(Named::ArrowRight));
                if let Some(n) = next {
                    self.grid_click(n, if mods.shift() { keyboard::Modifiers::SHIFT } else { keyboard::Modifiers::empty() });
                    if mods.shift() {
                        // keep anchor for ranges
                    } else {
                        self.grid.anchor = Some(n);
                    }
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    // ── update ──────────────────────────────────────────────────────────

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let _t = crate::profile::span("update", message.kind());
        let task = self.update_inner(message);
        self.sync_global_menu();
        task
    }

    /// The global menu is showing this window's menus (so the in-window
    /// menu bar steps aside).
    pub fn global_menu_on(&self) -> bool {
        self.global_menu_linked && self.global_menu_present
    }

    /// Publish enabled / checked state to the global menu.
    fn sync_global_menu(&self) {
        let Some(gm) = &self.global_menu else { return };
        let menus = commands::MENUS
            .iter()
            .map(|(_, _, items)| {
                items
                    .iter()
                    .map(|c| {
                        c.map(|cmd| global_menu::Item {
                            cmd,
                            enabled: main_view::cmd_enabled(self, cmd),
                            checked: main_view::cmd_checked(self, cmd),
                        })
                    })
                    .collect()
            })
            .collect();
        gm.update(global_menu::Model { menus });
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::Bg(events) => self.on_bg(events),
            Message::Tick => {
                if self.scan_dirty && self.last_refresh.elapsed() >= Duration::from_millis(200) {
                    self.scan_dirty = false;
                    self.refresh_views();
                }
                self.calendar.tick();
                self.calendar_frame();
                testing::drive_scroll(self);
                Task::none()
            }
            Message::Event(id, ev, status) => match ev {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    self.on_key(id, key, modifiers, status == event::Status::Captured)
                }
                Event::Window(window::Event::Focused) if Some(id) == self.prefs.window => preferences::on_focused(self),
                Event::Window(we)
                    if self.viewer.as_ref().is_some_and(|v| v.id == id) || self.compare.as_ref().is_some_and(|c| c.id() == id) =>
                {
                    viewer_window::on_event(self, we)
                }
                Event::Window(window::Event::Resized(size)) if Some(id) == self.main_id => {
                    self.window_size = size;
                    Task::none()
                }
                Event::Window(window::Event::Moved(p)) if Some(id) == self.main_id => {
                    self.window_pos = Some(p);
                    Task::none()
                }
                Event::Window(window::Event::Rescaled(f)) if Some(id) == self.main_id => {
                    self.scale_factor = f;
                    Task::none()
                }
                _ => Task::none(),
            },
            Message::WindowOpened(id) => {
                if Some(id) == self.main_id {
                    let mut tasks = vec![window::monitor_size(id).map(Message::Monitor), window::scale_factor(id).map(Message::ScaleFactor)];
                    if let Some(gm) = &self.global_menu {
                        let (service, conn) = (gm.service(), gm.connection());
                        tasks.push(window::run(id, move |w| global_menu::register_window(w, service, conn)).map(Message::GlobalMenuLinked));
                    }
                    return Task::batch(tasks);
                }
                Task::none()
            }
            Message::GlobalMenuLinked(result) => {
                if let Err(e) = &result {
                    eprintln!("global menu: {e}");
                }
                self.global_menu_linked = result.is_ok();
                Task::none()
            }
            Message::Monitor(m) => {
                self.monitor = m;
                Task::none()
            }
            Message::ScaleFactor(f) => {
                self.scale_factor = f;
                Task::none()
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.main_id {
                    return self.quit();
                }
                if self.viewer.as_ref().is_some_and(|v| v.id == id) {
                    return viewer::close(self);
                }
                if self.compare.as_ref().is_some_and(|c| c.id() == id) {
                    return compare::close_requested(self);
                }
                if Some(id) == self.shortcuts {
                    self.shortcuts = None;
                    return window::close(id);
                }
                if Some(id) == self.prefs.window {
                    return preferences::close(self);
                }
                window::close(id)
            }
            Message::WindowClosed(id) => {
                if Some(id) == self.main_id {
                    network::on_exit(self);
                    return iced::exit();
                }
                if self.viewer.as_ref().is_some_and(|v| v.id == id) {
                    self.viewer = None;
                }
                if self.bulk.as_ref().is_some_and(|b| b.window == id) {
                    self.bulk = None;
                }
                if self.compare.as_ref().is_some_and(|c| c.id() == id) {
                    compare::window_closed(self);
                }
                if Some(id) == self.shortcuts {
                    self.shortcuts = None;
                }
                if Some(id) == self.prefs.window {
                    preferences::on_closed(self);
                }
                if self.quitting && self.main_id.is_none() {
                    network::on_exit(self);
                    return iced::exit();
                }
                Task::none()
            }
            Message::StatusExpire(seq) => {
                if seq == self.status.seq {
                    self.status.text.clear();
                    self.status.action = None;
                }
                Task::none()
            }
            Message::UndoStep(serial) => undo::undo_step(self, serial),
            Message::Cmd(cmd) => self.run_cmd(cmd),
            Message::SideBurger => {
                self.side_expanded = !self.side_expanded;
                Task::none()
            }
            Message::SideCollapse => {
                self.side_expanded = false;
                Task::none()
            }
            Message::MenuOpen(m) => {
                self.menu = m;
                self.menu_hover = None;
                Task::none()
            }
            Message::MenuHover(h) => {
                self.menu_hover = h;
                Task::none()
            }
            Message::FolderBarCollapse => {
                self.folder_bar_collapsed = !self.folder_bar_collapsed;
                self.settings.folder_bar_collapsed = self.folder_bar_collapsed;
                let _ = self.settings.save();
                Task::none()
            }
            Message::PickFolder(target) => self.pick_folder(target),
            Message::FolderPicked(target, path) => self.folder_picked(target, path),
            Message::Filter(m) => self.on_filter(m),
            Message::Grid(ev) => self.on_grid(ev),
            Message::Calendar(m) => calendar::update(self, m),
            Message::Viewer(m) => viewer::update(self, m),
            Message::QuickLook(m) => quicklook::update(self, m),
            Message::Compare(m) => compare::update(self, m),
            Message::Dialog(m) => dialogs::update(self, m),
            Message::Script(i) => testing::step(self, i),
            Message::ScriptShot(path, shot) => {
                testing::save_shot(&path, &shot);
                Task::none()
            }
            Message::ShortcutsClose => match self.shortcuts.take() {
                Some(id) => window::close(id),
                None => Task::none(),
            },
            Message::Net(m) => network::update(self, m),
            Message::Prefs(m) => preferences::update(self, m),
            Message::Hints(m) => hints_ui::update(self, m),
            Message::Bulk(m) => bulk::update(self, m),
            Message::Search(m) => search::update(self, m),
            Message::Subjects(m) => subjects::update(self, m),
            Message::Fursuits(m) => subjects_fursuits::update(self, m),
            Message::Judge(m) => fursuit_judge::update(self, m),
        }
    }

    fn quit(&mut self) -> Task<Message> {
        // Marks not saved yet: wait for them, or say where they are.
        if let Some(t) = network::hold_quit(self) {
            return t;
        }
        self.quit_now()
    }

    pub(crate) fn quit_now(&mut self) -> Task<Message> {
        self.save_session();
        network::on_exit(self);
        self.quitting = true;
        if let Some(s) = self.scan.take() {
            s.cancel();
        }
        quality::cancel(self);
        self.search.link.shutdown();
        self.calendar.shutdown();
        self.loader.shutdown();
        self.tile_previews.shutdown();
        let mut tasks = Vec::new();
        compare::release(self);
        if self.viewer.is_some() {
            tasks.push(viewer::close(self));
        }
        if let Some(id) = self.shortcuts.take() {
            tasks.push(window::close(id));
        }
        if let Some(id) = self.prefs.window.take() {
            tasks.push(window::close(id));
        }
        if let Some(id) = self.main_id.take() {
            tasks.push(window::close(id));
        }
        tasks.push(iced::exit());
        Task::batch(tasks)
    }

    fn run_cmd(&mut self, cmd: Cmd) -> Task<Message> {
        // Any command closes the side menu; Show menu labels flips it from
        // how it was.
        let was_expanded = std::mem::take(&mut self.side_expanded);
        self.menu = None;
        match cmd {
            Cmd::Open => self.pick_folder(FolderTarget::Working),
            Cmd::Export => dialogs::open_export(self),
            Cmd::EditCopy => dialogs::open_edit_copy(self),
            Cmd::Review => dialogs::open_review(self),
            Cmd::Sort => dialogs::open_sort(self),
            Cmd::GridView => self.switch_view(View::Grid),
            Cmd::CalendarView => self.switch_view(View::Calendar),
            Cmd::Search => search::open(self),
            Cmd::Subjects => subjects::open(self),
            Cmd::FursuitMerges => subjects_merge::open_review(self),
            Cmd::QuickLook => quicklook::toggle(self),
            // S on the People tab picks several groups.
            Cmd::SelectMode if self.view == View::Subjects => {
                subjects::select_mode(self);
                Task::none()
            }
            Cmd::SelectMode => {
                let on = !self.grid.select_mode;
                self.set_select_mode(on);
                Task::none()
            }
            Cmd::PruneSelected => {
                let ids: Vec<RecordId> = self.derived.visible.iter().copied().filter(|id| self.grid.selection.contains(id)).collect();
                if ids.is_empty() {
                    return Task::none();
                }
                let all = ids.iter().all(|&id| self.coll.get(id).is_some_and(|r| r.is_pruned));
                for &id in &ids {
                    if let Some(r) = self.coll.get_mut(id) {
                        r.is_pruned = !all;
                    }
                }
                self.refresh_prune(&ids)
            }
            Cmd::MarkSelected => {
                let ids: Vec<RecordId> = self.derived.visible.iter().copied().filter(|id| self.grid.selection.contains(id)).collect();
                if ids.is_empty() {
                    return Task::none();
                }
                let all = ids.iter().all(|&id| self.coll.get(id).is_some_and(|r| r.is_marked));
                for &id in &ids {
                    if let Some(r) = self.coll.get_mut(id) {
                        r.is_marked = !all;
                    }
                }
                self.refresh_mark(&ids)
            }
            // On People and fursuits: the last change to people.
            Cmd::Undo if self.view == View::Subjects => subjects::undo(self),
            Cmd::Undo => undo::undo(self),
            Cmd::Redo => undo::redo(self),
            Cmd::Quit => self.quit(),
            Cmd::Shortcuts => self.open_shortcuts(),
            Cmd::SideMenu => {
                self.side_expanded = !was_expanded;
                Task::none()
            }
            Cmd::FolderBar => {
                self.folder_bar_visible = !self.folder_bar_visible;
                Task::none()
            }
            Cmd::ToReview => {
                // The library pages have no filter bar to show the change.
                if self.view.is_library() {
                    return Task::none();
                }
                let show = if self.filter.to_review { Show::All } else { Show::ToReview };
                self.on_filter(FilterMsg::Show(show))
            }
            Cmd::FileNames => {
                self.show_names = !self.show_names;
                self.settings.show_file_names = self.show_names;
                self.settings.save_in_background();
                Task::none()
            }
            // On People and fursuits: two groups side by side.
            Cmd::Compare if self.view == View::Subjects => subjects_editor::compare_key(self),
            Cmd::Compare => compare::open_selected(self),
            Cmd::ReviewBurst => viewer_burst::open_from_main(self),
            Cmd::StackBurst if self.view == View::Search => search::toggle_focused(self),
            Cmd::StackBurst if self.view == View::Subjects => subjects_person::toggle_focused(self),
            Cmd::StackBurst if self.view != View::Grid => Task::none(),
            Cmd::StackBurst => match self.grid_focus() {
                Some(id) => self.toggle_burst(id),
                None => self.show_status("Select a photo to stack its burst.", 4000),
            },
            // On the search page, for its results only.
            Cmd::StackBursts if self.view == View::Search => search::toggle_all(self),
            Cmd::StackBursts if self.view == View::Subjects => subjects_person::toggle_all(self),
            // A saved preference: from the menu, the keys or Preferences,
            // whichever view is showing.
            Cmd::StackBursts => self.toggle_all_bursts(),
            Cmd::AdvanceAfterMark => viewer_advance::toggle(self),
            Cmd::PrevBurst => viewer_burst::skip(self, -1),
            Cmd::NextBurst => viewer_burst::skip(self, 1),
            Cmd::LabelFaces => viewer_labels::toggle(self),
            Cmd::SheetBigger => viewer_burst::resize(self, 1.0),
            Cmd::SheetSmaller => viewer_burst::resize(self, -1.0),
            Cmd::Preferences => preferences::open(self),
            Cmd::Analyse => hints_ui::analyse(self),
            Cmd::KeepThisPruneRest => bulk::open(self, bulk::Action::KeepThis),
            Cmd::PruneSoft => bulk::open(self, bulk::Action::Soft),
            Cmd::PruneAccidental => bulk::open(self, bulk::Action::Accidental),
            Cmd::PruneCopies => bulk::open(self, bulk::Action::Copies),
            Cmd::ProblemsOnly => {
                if self.view.is_library() {
                    return Task::none();
                }
                let f = if self.filter.hint == Some(HintFilter::Problems) { None } else { Some(HintFilter::Problems) };
                self.on_filter(FilterMsg::Hint(f))
            }
        }
    }

    pub fn open_shortcuts(&mut self) -> Task<Message> {
        if let Some(id) = self.shortcuts {
            return window::gain_focus(id);
        }
        let (id, open) = window::open(window::Settings {
            size: Size::new(520.0, 820.0),
            position: window::Position::Centered,
            min_size: Some(Size::new(480.0, 400.0)),
            ..window_settings()
        });
        self.shortcuts = Some(id);
        open.map(Message::WindowOpened)
    }

    pub fn pick_folder(&mut self, target: FolderTarget) -> Task<Message> {
        let (title, start) = match target {
            FolderTarget::Working => ("Open Working Folder", self.folder.clone()),
            FolderTarget::Library | FolderTarget::ExportLibrary => ("Set Library Folder", self.library.clone()),
            FolderTarget::EditCopy => ("Folder to Copy Photos Into", dialogs::edit_copy_base(self)),
            FolderTarget::EditCopySetting => {
                let s = &self.settings.edit_copy_folder;
                ("Folder to Copy Photos Into", (!s.is_empty()).then(|| PathBuf::from(s)))
            }
            FolderTarget::ExportCorrections => ("Folder to Export the Record Into", None),
        };
        let start = start.or_else(dirs::home_dir).unwrap_or_default();
        let dialog = rfd::AsyncFileDialog::new().set_title(title).set_directory(start);
        Task::perform(async move { dialog.pick_folder().await.map(|h| h.path().to_path_buf()) }, move |p| {
            Message::FolderPicked(target, p)
        })
    }

    fn folder_picked(&mut self, target: FolderTarget, path: Option<PathBuf>) -> Task<Message> {
        let Some(path) = path.map(|p| util::normalize(&p)) else { return Task::none() };
        match target {
            FolderTarget::Working => self.start_scan(path),
            FolderTarget::Library | FolderTarget::ExportLibrary => {
                self.set_library(path.clone());
                self.settings.save_in_background();
                if target == FolderTarget::ExportLibrary {
                    dialogs::export_library_changed(self, path);
                }
                Task::none()
            }
            FolderTarget::EditCopy => {
                dialogs::edit_copy_folder_changed(self, path);
                Task::none()
            }
            FolderTarget::EditCopySetting => {
                preferences::edit_copy_folder_picked(self, path);
                Task::none()
            }
            FolderTarget::ExportCorrections => {
                let export = preferences::export_corrections(self, path);
                let recount = Task::perform(tokio::time::sleep(std::time::Duration::from_millis(600)), |_| Message::Prefs(preferences::Msg::Recount));
                Task::batch([export, recount])
            }
        }
    }

    pub fn set_library(&mut self, path: PathBuf) {
        self.library = Some(path.clone());
        self.settings.library_folder = path.to_string_lossy().into_owned();
        network::probe(self, &path);
        self.calendar.set_library_root(Some(path), &self.workers);
    }

    /// Preferences ▸ Show pruned photos: whether pruned photos stay in All
    /// and Marked.  Saved at once, as every preference is.
    pub fn toggle_show_pruned(&mut self) -> Task<Message> {
        self.filter.show_pruned = !self.filter.show_pruned;
        self.settings.filter_pruned = self.filter.show_pruned;
        self.settings.save_in_background();
        self.refresh_views();
        self.update_status_count();
        Task::none()
    }

    fn on_filter(&mut self, m: FilterMsg) -> Task<Message> {
        // A filter changed: the Hints menu has done its job.
        self.quality.menu = false;
        match m {
            FilterMsg::ToggleMarkedOnly => {
                let show = if self.filter.marked_only { Show::All } else { Show::Marked };
                self.filter.set_show(show);
            }
            FilterMsg::Show(show) => {
                if self.filter.show() == show {
                    return Task::none();
                }
                self.filter.set_show(show);
            }
            FilterMsg::Hint(f) => {
                if self.filter.hint == f {
                    return Task::none();
                }
                self.filter.hint = f;
            }
            FilterMsg::Sort(k) => {
                if self.filter.sort_key == k {
                    self.filter.sort_asc = !self.filter.sort_asc;
                } else {
                    self.filter.sort_key = k;
                    self.filter.sort_asc = true;
                }
            }
            FilterMsg::ToggleDate => {
                self.filter.date_on = !self.filter.date_on;
                self.date_popup = None;
            }
            FilterMsg::OpenDate(from) => {
                let d = if from { self.filter.date_from } else { self.filter.date_to };
                self.date_popup = Some(DatePopup { from, year: chrono::Datelike::year(&d), month: chrono::Datelike::month(&d) });
                return Task::none();
            }
            FilterMsg::DatePopupMonth(delta) => {
                if let Some(p) = &mut self.date_popup {
                    let (y, m) = crate::model::library_index::month_shift(p.year, p.month, delta);
                    p.year = y;
                    p.month = m;
                }
                return Task::none();
            }
            FilterMsg::CloseDatePopup => {
                self.date_popup = None;
                return Task::none();
            }
            FilterMsg::PickDate(from, d) => {
                if from {
                    self.filter.date_from = d;
                } else {
                    self.filter.date_to = d;
                }
                self.date_popup = None;
            }
            FilterMsg::ClearDates => {
                let today = Local::now().date_naive();
                self.filter.date_from = NaiveDate::from_ymd_opt(chrono::Datelike::year(&today), 1, 1).unwrap_or(today);
                self.filter.date_to = today;
            }
            // Filters only: Show pruned photos is a preference now.
            FilterMsg::ResetAll => {
                self.filter.set_show(Show::All);
                self.filter.date_on = false;
                self.filter.hint = None;
                self.date_popup = None;
            }
            FilterMsg::Zoom(v) => {
                let snapped = ((v / ZOOM_STEP).floor() * ZOOM_STEP).clamp(MIN_SIZE, MAX_SIZE);
                self.thumb_size = snapped;
                return Task::none();
            }
        }
        self.refresh_views();
        self.update_status_count();
        Task::none()
    }

    fn on_grid(&mut self, ev: GridEvent) -> Task<Message> {
        match ev {
            GridEvent::Click { id, modifiers } => {
                self.grid_click(id, modifiers);
                Task::none()
            }
            // A stacked burst opens on all its frames (the sheet).
            GridEvent::Activate(id) if self.is_stack(id) => self.open_viewer_working(id, viewer_burst::Land::Sheet),
            GridEvent::Activate(id) => self.open_viewer_working(id, viewer_burst::Land::Auto),
            GridEvent::ClearSelection => {
                self.grid.selection.clear();
                Task::none()
            }
            GridEvent::ToggleBurst(id) => self.toggle_burst(id),
            GridEvent::ToggleCollapse(d) => {
                if !self.grid.collapsed.remove(&d) {
                    self.grid.collapsed.insert(d);
                }
                Task::none()
            }
            GridEvent::Chip(d) => {
                if !self.grid.select_mode {
                    return Task::none();
                }
                if let Some(sec) = self.derived.sections.iter().find(|s| s.date == d) {
                    let all = sec.ids.iter().all(|id| self.grid.selection.contains(id));
                    for id in &sec.ids {
                        if all {
                            self.grid.selection.remove(id);
                        } else {
                            self.grid.selection.insert(*id);
                        }
                    }
                }
                Task::none()
            }
            GridEvent::ZoomStep(step) => {
                let new = ((self.thumb_size + step as f32 * ZOOM_STEP) / ZOOM_STEP).floor() * ZOOM_STEP;
                self.thumb_size = new.clamp(MIN_SIZE, MAX_SIZE);
                Task::none()
            }
            GridEvent::PinchTo(size) => {
                self.thumb_size = size.clamp(MIN_SIZE, MAX_SIZE);
                Task::none()
            }
            GridEvent::Thumbs { visible, nearby, settle } => {
                if settle {
                    self.thumbs.clear_queue(&self.workers);
                }
                for id in visible {
                    if let Some(r) = self.coll.get(id) {
                        self.thumbs.request(&self.workers, r, thumbs::PRIORITY_VISIBLE);
                    }
                }
                for id in nearby {
                    if let Some(r) = self.coll.get(id) {
                        self.thumbs.request(&self.workers, r, thumbs::PRIORITY_PREFETCH);
                    }
                }
                Task::none()
            }
            GridEvent::Scrolled(y) => {
                self.grid.scroll = y;
                Task::none()
            }
            GridEvent::SelectDone => {
                self.set_select_mode(false);
                Task::none()
            }
            GridEvent::SectionLink(_) | GridEvent::FooterLink => Task::none(),
        }
    }

    fn on_bg(&mut self, events: Vec<BgEvent>) -> Task<Message> {
        let mut tasks = Vec::new();
        for ev in events {
            let _t = crate::profile::span("bg", ev.name());
            match ev {
                BgEvent::ScanBatch { scan, records } => {
                    if scan != self.scan_seq {
                        continue;
                    }
                    if self.coll.is_empty() {
                        crate::profile::record(("scan", "open to first batch"), self.scan_started.elapsed());
                    }
                    for r in records.iter() {
                        self.coll.add(r.clone());
                    }
                    self.scan_dirty = true;
                }
                BgEvent::ScanProgress { scan, count } => {
                    if scan == self.scan_seq {
                        tasks.push(self.show_status(format!("Scanning… {count} files found"), 0));
                    }
                }
                BgEvent::ScanComplete { scan, count } => {
                    crate::profile::milestone("scan complete");
                    if scan == self.scan_seq {
                        self.scan = None;
                        tasks.push(self.on_scan_complete(count));
                    }
                }
                BgEvent::ScanError { scan, message } => {
                    if scan == self.scan_seq {
                        self.scanning = false;
                        tasks.push(self.show_status(format!("Scan error: {message}"), 0));
                        tasks.push(network::on_scan_error(self, &message));
                    }
                }
                BgEvent::Thumb { owner, path, thumb } => {
                    crate::profile::milestone("first thumbnail");
                    if owner == OWNER_MAIN {
                        if !thumb.ok {
                            network::on_load_failed(self);
                        }
                        self.thumbs.deliver(path, thumb);
                    } else if owner == OWNER_CALENDAR {
                        self.calendar.thumbs.deliver(path, thumb);
                    } else if owner == thumbs::OWNER_SEARCH {
                        self.search.pane.thumbs.deliver(path, thumb);
                    } else if owner == thumbs::OWNER_PERSON {
                        if let Some(p) = self.subjects.person.as_mut() {
                            p.pane.thumbs.deliver(path, thumb);
                        }
                    }
                }
                BgEvent::Full { path, result } => {
                    if result.is_err() {
                        network::on_load_failed(self);
                    }
                    let err = result.as_ref().err().cloned();
                    tasks.push(viewer::on_loaded(self, path.clone(), result.clone()));
                    tasks.push(compare::on_loaded(self, &path, &result));
                    tasks.push(quicklook::on_loaded(self, &path, err.as_deref()));
                }
                BgEvent::TilePreview { path, edge, image } => {
                    self.tile_previews.deliver(&path, edge, image);
                }
                BgEvent::Preview { path, image } => {
                    tasks.push(viewer::on_preview(self, path.clone(), image));
                    tasks.push(compare::on_preview(self, &path));
                    tasks.push(quicklook::on_loaded(self, &path, None));
                }
                BgEvent::Exif { key, sections } => viewer::on_exif(self, key, sections),
                BgEvent::ExifTool { path, flat } => viewer::on_exiftool(self, path, flat),
                BgEvent::IndexProgress { job, count } => self.calendar.on_progress(job, count),
                BgEvent::IndexReady { job, index, done } => {
                    if done {
                        network::on_library_checked(self, true);
                    }
                    let shown = index.clone();
                    if let Some(text) = self.calendar.on_index_ready(job, index, done) {
                        if self.view.is_library() {
                            tasks.push(self.show_status(text, 0));
                        }
                        // The library as checked: search looks for new
                        // photos in it.
                        if done {
                            search::on_library(self, shown);
                        }
                    }
                }
                BgEvent::IndexIdle { job, checked } => {
                    if checked {
                        network::on_library_checked(self, true);
                    }
                    self.calendar.on_index_idle(job, checked)
                }
                BgEvent::Saver(report) => tasks.push(network::on_saver_report(self, report)),
                BgEvent::NetProbe { path, seq, probe } => tasks.push(network::on_probe(self, path, seq, probe)),
                BgEvent::IndexError { job, message } => {
                    network::on_library_checked(self, false);
                    if let Some(text) = self.calendar.on_index_error(job, message) {
                        if self.view.is_library() {
                            tasks.push(self.show_status(text, 0));
                        }
                    }
                }
                BgEvent::DayRecords { date, generation, data, complete } => {
                    self.calendar.on_day_records(date, generation, data, complete)
                }
                BgEvent::ExportProgress { done, total } => dialogs::on_export_progress(self, done, total),
                BgEvent::ExportDone(report) => tasks.push(dialogs::on_export_done(self, report)),
                BgEvent::CopyProgress { files, files_total, bytes, bytes_total } => {
                    dialogs::on_copy_progress(self, files, files_total, bytes, bytes_total)
                }
                BgEvent::CopyDone(report) => tasks.push(dialogs::on_copy_done(self, report)),
                BgEvent::Trashed(result) => tasks.push(dialogs::on_trashed(self, result)),
                BgEvent::Sorted(result) => tasks.push(self.on_sorted_event(result)),
                BgEvent::Undo(done) => tasks.push(undo::on_done(self, done)),
                BgEvent::GlobalMenu(present) => self.global_menu_present = present,
                BgEvent::MenuCommand(cmd) => tasks.push(self.update(Message::Cmd(cmd))),
                BgEvent::Analysis { run, batch } => tasks.push(quality::on_batch(self, run, batch)),
                BgEvent::SearchStatus(status) => tasks.push(search::on_status(self, status)),
                BgEvent::SearchResults { seq, hits, indexed: _, error, understood, fursuits, animal, subjects, also_fursuits } => {
                    let reply = search::Reply { hits, error, understood, fursuits, animal, subjects, also_fursuits };
                    // A person's page asks with numbers of its own.
                    if seq >= subjects_person::SEQ_BASE {
                        tasks.push(subjects_person::on_results(self, seq, reply));
                    } else {
                        tasks.push(search::on_results(self, seq, reply));
                    }
                }
                BgEvent::SearchThings(things) => search::on_things(self, things),
                BgEvent::Subjects(all) => {
                    tasks.push(subjects::on_subjects(self, all));
                    viewer_labels::on_subjects(self);
                }
                BgEvent::SubjectsUndone(what) => {
                    viewer_labels::on_undone(self, &what);
                    tasks.push(subjects::on_undone(self, what));
                }
                BgEvent::PhotoLabels { seq, labels } => viewer_labels::on_labels(self, seq, labels),
                BgEvent::SubjectFaces { seq, group, faces } => tasks.push(subjects::on_faces(self, seq, group, faces)),
                BgEvent::Suggestions { seq, group, list } => tasks.push(subjects::on_suggestions(self, seq, group, list)),
                BgEvent::Judged { seq, group, offered, left_out, done, total, beyond } => tasks.push(fursuit_judge::on_judged(self, seq, group, offered, left_out, done, total, beyond)),
                BgEvent::GroupCheck { seq, group, far, pairs, split } => {
                    if subjects_editor::on_check(self, seq, &pairs, split.as_deref()) {
                        tasks.push(subjects::refresh_crops(self));
                    } else {
                        tasks.push(subjects_split::on_check(self, seq, group, far, pairs, split));
                    }
                }
                BgEvent::HeadsOrder { seq, far } => {
                    if subjects_editor::on_heads_order(self, seq, &far) {
                        tasks.push(subjects::refresh_crops(self));
                    }
                }
                BgEvent::NearGroups { seq, list } => {
                    if subjects_editor::on_near(self, seq, &list) {
                        tasks.push(subjects::refresh_crops(self));
                    }
                }
                BgEvent::EditApplied { seq, words } => tasks.push(subjects_editor::on_applied(self, seq, &words)),
                BgEvent::EditRefused { seq, why, gone } => tasks.push(subjects_editor::on_refused(self, seq, &why, gone)),
                BgEvent::MergeSuggestions { seq, focus, list, judging } => tasks.push(subjects_merge::on_merge_suggestions(self, seq, focus, list, judging)),
                BgEvent::MergeJudged { seq, offered, left_out, done, total, beyond } => tasks.push(subjects_merge::on_merge_judged(self, seq, offered, left_out, done, total, beyond)),
                BgEvent::ModelDownload { model, done, fetched, total, state } => tasks.push(search::on_download(self, model, done, fetched, total, state)),
            }
        }
        // The search pass steps back while thumbnails are wanted.
        search::note_busy(self);
        if self.scan_dirty && self.last_refresh.elapsed() >= Duration::from_millis(200) {
            self.scan_dirty = false;
            self.refresh_views();
        }
        Task::batch(tasks)
    }

    /// Remove records whose files are gone (trashed or moved out).
    pub fn remove_records(&mut self, paths: &[PathBuf]) {
        let ids: Vec<RecordId> = paths.iter().filter_map(|p| self.coll.id_of(p)).collect();
        for id in &ids {
            self.coll.remove(*id);
            self.grid.selection.remove(id);
        }
        quicklook::on_removed(self, &ids);
        // A surviving half is no longer part of a pair.
        self.coll.build_pairs();
        if let Some(v) = &mut self.viewer {
            if !v.read_only {
                v.items.retain(|x| !ids.contains(x));
            }
        }
        compare::on_removed(self, &ids);
        self.refresh_views();
        // The bursts are found again: the viewer's follow.
        if let Some(v) = self.viewer.as_mut().filter(|v| !v.read_only) {
            viewer_burst::items_changed(&self.bursts, v);
        }
    }

    pub fn bus(&self) -> Bus {
        self.bus.clone()
    }

}

/// Does shot `id` (either half of a pair) have hint `f`?
fn shot_has(h: &Hints, coll: &Collection, f: HintFilter, id: RecordId) -> bool {
    f.accepts(h, id) || coll.find_pair(id).is_some_and(|p| f.accepts(h, p))
}

/// How many of `shots` have each hint (by `HintFilter::index`).
fn hint_counts(h: &Hints, coll: &Collection, shots: &[RecordId]) -> [usize; 5] {
    let _t = crate::profile::span("refresh", "hint counts");
    let mut n = [0; 5];
    if h.is_empty() {
        return n;
    }
    for &id in shots {
        // The other half is looked up once, and only if needed (it costs
        // a key built from the path).
        let mut pair: Option<Option<RecordId>> = None;
        for f in HintFilter::ALL {
            let has = f.accepts(h, id) || pair.get_or_insert_with(|| coll.find_pair(id)).is_some_and(|p| f.accepts(h, p));
            n[f.index()] += has as usize;
        }
    }
    n
}

/// The tile an arrow key moves to in the grid's `tiles` from `anchor`: the
/// next or previous one (a stack is one step), or the first when nothing
/// is focused.  Stays at either end.
fn arrow_step(tiles: &[RecordId], anchor: Option<RecordId>, forward: bool) -> Option<RecordId> {
    let at = anchor.and_then(|a| tiles.iter().position(|&x| x == a));
    match at {
        None => tiles.first().copied(),
        Some(i) if forward => tiles.get((i + 1).min(tiles.len() - 1)).copied(),
        Some(i) => tiles.get(i.saturating_sub(1)).copied(),
    }
}

/// The selection and focus once the grid's tiles changed: each photo no
/// longer `on_grid` goes to its `home` (the stack it was folded into), or
/// is dropped.  A focus with no home stays (it only orders arrow keys).
fn regather(
    selection: &HashSet<RecordId>,
    anchor: Option<RecordId>,
    on_grid: &HashSet<RecordId>,
    home: impl Fn(RecordId) -> Option<RecordId>,
) -> (HashSet<RecordId>, Option<RecordId>) {
    let kept = selection.iter().filter_map(|&id| if on_grid.contains(&id) { Some(id) } else { home(id) }).collect();
    let anchor = anchor.map(|a| if on_grid.contains(&a) { a } else { home(a).unwrap_or(a) });
    (kept, anchor)
}

#[cfg(test)]
mod stack_tests {
    use super::*;

    #[test]
    fn arrows_step_by_tile_so_a_stack_is_one_step() {
        // Tiles 10, 20 (a stack folding 21 and 22), 30.
        let tiles = [10, 20, 30];
        assert_eq!(arrow_step(&tiles, None, true), Some(10));
        assert_eq!(arrow_step(&tiles, Some(10), true), Some(20));
        assert_eq!(arrow_step(&tiles, Some(20), true), Some(30), "past the folded frames in one step");
        assert_eq!(arrow_step(&tiles, Some(30), true), Some(30));
        assert_eq!(arrow_step(&tiles, Some(30), false), Some(20));
        assert_eq!(arrow_step(&tiles, Some(10), false), Some(10));
        assert_eq!(arrow_step(&[], None, true), None);
    }

    #[test]
    fn a_frame_folded_into_a_stack_hands_its_selection_to_the_stack() {
        // 21 and 22 were folded into the stack 20; 40 left the grid (a filter).
        let on_grid: HashSet<RecordId> = [10, 20, 30].into();
        let home = |id: RecordId| (21..=22).contains(&id).then_some(20);
        let (sel, anchor) = regather(&[10, 22, 40].into(), Some(22), &on_grid, home);
        assert_eq!(sel, [10, 20].into());
        assert_eq!(anchor, Some(20));
        // Two frames of one stack select it once; a gone focus with no home stays.
        let (sel, anchor) = regather(&[21, 22].into(), Some(40), &on_grid, home);
        assert_eq!(sel, [20].into());
        assert_eq!(anchor, Some(40));
    }
}

#[cfg(test)]
mod review_filter_tests {
    use super::*;

    #[test]
    fn show_choice_is_one_of_four_and_sets_the_filter() {
        let mut f = FilterUi::defaults();
        f.show_pruned = true;
        assert_eq!(f.show(), Show::All);

        // To review: pruned stay hidden even with "Show pruned" on.
        f.set_show(Show::ToReview);
        assert_eq!((f.to_review, f.marked_only, f.pruned_only), (true, false, false));
        let st = f.state();
        assert!(!st.show_pruned && !st.show_marked && st.show_unpruned && st.show_unmarked);

        // Marked replaces it; "Show pruned" applies again.
        f.set_show(Show::Marked);
        assert_eq!((f.show(), f.to_review, f.pruned_only), (Show::Marked, false, false));
        let st = f.state();
        assert!(st.show_pruned && st.show_marked && !st.show_unmarked);

        // Pruned replaces that: pruned photos only, marked or not.
        f.set_show(Show::Pruned);
        assert_eq!((f.show(), f.to_review, f.marked_only), (Show::Pruned, false, false));
        let st = f.state();
        assert!(st.show_pruned && !st.show_unpruned && st.show_marked && st.show_unmarked);

        f.set_show(Show::All);
        assert_eq!((f.to_review, f.marked_only, f.pruned_only), (false, false, false));
        assert_eq!(f.state(), FilterState { show_pruned: true, ..FilterState::default() });
    }

    #[test]
    fn the_show_pruned_preference_never_hides_the_pruned_filter() {
        use crate::model::record::{FileType, MTime, PhotoRecord};
        let mut c = crate::model::collection::Collection::new();
        let mut add = |name: &str, pruned: bool, marked: bool| {
            let p = PathBuf::from(format!("/s/{name}"));
            let mut r = PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), 10, MTime::from_unix(1_700_000_000, 0), None);
            (r.is_pruned, r.is_marked) = (pruned, marked);
            c.add(r)
        };
        let kept = add("A.JPG", false, false);
        let pruned = add("B.JPG", true, false);
        let both = add("C.JPG", true, true);
        let marked = add("D.JPG", false, true);
        let shown = |f: &FilterUi| filter::visible_sorted(&c, &f.state());
        let mut f = FilterUi::defaults();
        for pref in [false, true] {
            f.show_pruned = pref;
            f.set_show(Show::Pruned);
            assert_eq!(shown(&f), [pruned, both], "Pruned, preference {pref}");
            f.set_show(Show::ToReview);
            assert_eq!(shown(&f), [kept], "To review, preference {pref}");
        }
        // The preference decides All and Marked.
        f.show_pruned = false;
        f.set_show(Show::All);
        assert_eq!(shown(&f), [kept, marked]);
        f.set_show(Show::Marked);
        assert_eq!(shown(&f), [marked]);
        f.show_pruned = true;
        f.set_show(Show::All);
        assert_eq!(shown(&f), [kept, pruned, both, marked]);
        f.set_show(Show::Marked);
        assert_eq!(shown(&f), [both, marked]);
    }

    #[test]
    fn to_review_keeps_the_date_range_and_sort() {
        let mut f = FilterUi::defaults();
        f.date_on = true;
        f.sort_key = SortKey::Name;
        f.sort_asc = false;
        f.set_show(Show::ToReview);
        let st = f.state();
        assert_eq!((st.date_from, st.date_to), (Some(f.date_from), Some(f.date_to)));
        assert_eq!((st.sort_key, st.sort_asc), (SortKey::Name, false));
        assert!(!st.show_pruned && !st.show_marked);
        // So does Pruned.
        f.set_show(Show::Pruned);
        let st = f.state();
        assert_eq!((st.date_from, st.date_to), (Some(f.date_from), Some(f.date_to)));
        assert_eq!((st.sort_key, st.sort_asc), (SortKey::Name, false));
    }
}

#[cfg(test)]
mod view_index_tests {
    use super::*;

    /// The list view is gone: a settings file that last showed it opens
    /// the grid, and the rest of it still loads.
    #[test]
    fn a_saved_list_view_opens_the_grid() {
        let old: Settings = serde_json::from_str(r#"{"view_index": 0, "last_folder": "/home/me/shoot", "thumb_size": 240}"#).expect("parses");
        assert_eq!(View::from_index(old.view_index), View::Grid);
        assert_eq!((old.last_folder.as_str(), old.thumb_size), ("/home/me/shoot", 240));
        // Grid and calendar keep their numbers, so older builds and the
        // Python app read what this one saves.
        assert_eq!((View::Grid.index(), View::Calendar.index()), (1, 2));
        for v in [View::Grid, View::Calendar] {
            assert_eq!(View::from_index(v.index()), v);
        }
        assert_eq!(View::from_index(7), View::Grid, "anything unknown opens the grid too");
    }

    /// The Python app saved its list as view 0: the import opens the grid
    /// and carries the rest over.
    #[test]
    fn importing_the_python_apps_list_view_opens_the_grid() {
        let py = Settings::from_qsettings("[General]\nview_index=0\nlast_folder=/home/me/shoot\n[filter]\nmarked_only=true\n");
        assert_eq!(View::from_index(py.view_index), View::Grid);
        assert_eq!(py.last_folder, "/home/me/shoot");
        assert!(py.filter_marked_only);
        // Its grid and calendar come across as they were.
        assert_eq!(View::from_index(Settings::from_qsettings("view_index=1\n").view_index), View::Grid);
        assert_eq!(View::from_index(Settings::from_qsettings("view_index=2\n").view_index), View::Calendar);
    }
}
