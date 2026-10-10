//! Preferences (Ctrl+,): the app's settings gathered on one page, in a
//! window of its own like the Keyboard shortcuts, so it can stay open beside
//! the grid and the viewer while its changes show there.
//!
//! - **Nothing to apply.**  Every control changes the setting at once and
//!   saves it in the background.  The page reads the same state as the
//!   menus and the viewer's toggles, so each shows the others' changes.
//! - **The global menu** is linked when the app starts, so changing it says
//!   it takes effect at the next start.  `PP_NO_GLOBAL_MENU` still forces
//!   it off, and the page says so.
//! - **Storage** sizes are counted on a worker (shown as "…" until then),
//!   again whenever the window comes back to the front.  Each cache can be
//!   cleared after a confirmation that says what clearing costs.  The
//!   folder of marks waiting to be saved is shown, never cleared: it holds
//!   marks that exist nowhere else.
//! - **Keyboard.**  iced has no focus for buttons or switches, so the page
//!   keeps its own: Tab and Shift+Tab move a focus ring, Space or Enter
//!   presses, ← and → change the thumbnail size and how close search
//!   matches must be, Esc closes.
//! - **People** (with Search): *Find people's faces* (on; turning it off
//!   offers, inline, to forget what was found), the face model with
//!   *Change…* (every face recognised again, names kept), the smallest
//!   group to show, and *Forget all…* behind a confirmation that says what
//!   is deleted.
//! - **Search** (`search_prefs` words): where models run, how close
//!   matches must be (a slider with a reset to the default), indexing new
//!   exports, and the recent searches.  Its index and models show in
//!   Storage, cleared or deleted after a confirmation that says the cost.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::advanced::widget::operation::{Operation, Outcome, scrollable::AbsoluteOffset};
use iced::advanced::widget::{self as adv, Id};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, column, container, row, scrollable, slider, stack, text, toggler};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Rectangle, Size, Task, Theme, Vector, window};

use super::commands::Cmd;
use super::preferences_storage::{self as storage, Cache, Cleared, Survey, Usage};
use crate::ml::judge::JudgeChoice;
use crate::ml::{CardFailure, Device, SelfCheck};
use crate::search::{CLOSENESS_DEFAULT, CLOSENESS_MAX, CLOSENESS_MIN, Stopped, estimate};
use super::{App, FilterMsg, FolderTarget, Message, dialogs, kit, style, viewer_advance, viewer_filmstrip};
use crate::settings::{self, Settings};
use crate::ui::grid::{MAX_SIZE, MIN_SIZE};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme as t;
use crate::util;

const SCROLL: &str = "prefs-page";
/// Sizes are counted again on coming to the front, at most this often.
const RECOUNT_AFTER: Duration = Duration::from_secs(3);
/// The page's width, centred in a wider window.
const PAGE_W: f32 = 640.0;

/// Every control the keyboard can reach, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Advance,
    ShowPruned,
    FileNames,
    StackBursts,
    ThumbSize,
    Filmstrip,
    BurstSheet,
    ViewerSize,
    Library,
    EditCopyFolder,
    EditCopySubfolder,
    GlobalMenu,
    /// Run models on: the graphics card or the processor.
    SearchDevice(Device),
    /// Check the card again (its self-check against the processor).
    CheckCard,
    /// How close matches must be (the slider; ← → move it).
    Closeness,
    /// Back to the default closeness.
    ClosenessReset,
    IndexExports,
    ForgetSearches,
    /// *Find people's faces*.
    FindFaces,
    /// The inline offer after turning faces off: keep or forget them.
    KeepFaces,
    ForgetFaces,
    /// *Face model* ▸ *Change…*.
    FaceModel,
    /// *Smallest group to show*: 3, 5 or 10 photos.
    SmallestGroup(usize),
    /// *Forget all…* (people).
    ForgetPeople,
    /// *Find fursuit heads*.
    FindHeads,
    /// *Show People and fursuits in the sidebar*.
    RailSubjects,
    /// Look at the fursuit photos not looked at yet (the catch-up).
    FindHeadsNow,
    /// *Forget all…* (fursuits).
    ForgetFursuits,
    /// *Keep a record of my corrections*.
    KeepCorrections,
    /// The record: *Export…* and *Delete the record…*.
    ExportCorrections,
    DeleteCorrections,
    /// *Grouping*: Tighter, Balanced or Looser.
    Grouping(crate::search::heads::Grouping),
    /// *Close-call judge*: one of the choices.
    Judge(JudgeChoice),
    /// Download the chosen judge (chosen, not in place).
    JudgeDownload,
    /// Give the heads found before v2 the search model's vectors.
    UpdateHeads,
    /// Storage ▸ a judge's row: open its folder, delete it.
    JudgeOpen(JudgeChoice),
    JudgeDelete(JudgeChoice),
    Open(Cache),
    Clear(Cache),
    SearchRebuild,
    SearchClear,
    ModelsOpen,
    ModelsDelete,
    OpenUnsaved,
    Close,
}

impl Control {
    fn id(self) -> &'static str {
        match self {
            Control::Advance => "pref-advance",
            Control::ShowPruned => "pref-show-pruned",
            Control::FileNames => "pref-names",
            Control::StackBursts => "pref-stack-bursts",
            Control::ThumbSize => "pref-thumb-size",
            Control::Filmstrip => "pref-filmstrip",
            Control::BurstSheet => "pref-burst-sheet",
            Control::ViewerSize => "pref-viewer-size",
            Control::Library => "pref-library",
            Control::EditCopyFolder => "pref-edit-copy",
            Control::EditCopySubfolder => "pref-edit-copy-subfolder",
            Control::GlobalMenu => "pref-global-menu",
            Control::SearchDevice(Device::Gpu) => "pref-search-gpu",
            Control::SearchDevice(Device::Cpu) => "pref-search-cpu",
            Control::CheckCard => "pref-check-card",
            Control::Closeness => "pref-closeness",
            Control::ClosenessReset => "pref-closeness-reset",
            Control::IndexExports => "pref-index-exports",
            Control::ForgetSearches => "pref-forget-searches",
            Control::FindFaces => "pref-find-faces",
            Control::KeepFaces => "pref-keep-faces",
            Control::ForgetFaces => "pref-forget-faces",
            Control::FaceModel => "pref-face-model",
            Control::SmallestGroup(3) => "pref-smallest-3",
            Control::SmallestGroup(10) => "pref-smallest-10",
            Control::SmallestGroup(_) => "pref-smallest-5",
            Control::ForgetPeople => "pref-forget-people",
            Control::FindHeads => "pref-find-heads",
            Control::RailSubjects => "pref-rail-subjects",
            Control::FindHeadsNow => "pref-find-heads-now",
            Control::ForgetFursuits => "pref-forget-fursuits",
            Control::KeepCorrections => "pref-keep-corrections",
            Control::ExportCorrections => "pref-export-corrections",
            Control::DeleteCorrections => "pref-delete-corrections",
            Control::Grouping(crate::search::heads::Grouping::Tighter) => "pref-grouping-tighter",
            Control::Grouping(crate::search::heads::Grouping::Balanced) => "pref-grouping-balanced",
            Control::Grouping(crate::search::heads::Grouping::Looser) => "pref-grouping-looser",
            Control::Judge(JudgeChoice::Off) => "pref-judge-off",
            Control::Judge(JudgeChoice::Gemma4) => "pref-judge-gemma4",
            Control::Judge(JudgeChoice::Qwen3VlReranker) => "pref-judge-qwen",
            Control::JudgeDownload => "pref-judge-download",
            Control::UpdateHeads => "pref-update-heads",
            Control::JudgeOpen(JudgeChoice::Qwen3VlReranker) => "pref-judge-open-qwen",
            Control::JudgeOpen(_) => "pref-judge-open-gemma4",
            Control::JudgeDelete(JudgeChoice::Qwen3VlReranker) => "pref-judge-delete-qwen",
            Control::JudgeDelete(_) => "pref-judge-delete-gemma4",
            Control::SearchRebuild => "pref-search-rebuild",
            Control::SearchClear => "pref-search-clear",
            Control::ModelsOpen => "pref-models-open",
            Control::ModelsDelete => "pref-models-delete",
            Control::Open(Cache::Thumbs) => "pref-open-thumbs",
            Control::Open(Cache::Library) => "pref-open-library",
            Control::Open(Cache::CaptureTimes) => "pref-open-capture",
            Control::Open(Cache::Quality) => "pref-open-quality",
            Control::Clear(Cache::Thumbs) => "pref-clear-thumbs",
            Control::Clear(Cache::Library) => "pref-clear-library",
            Control::Clear(Cache::CaptureTimes) => "pref-clear-capture",
            Control::Clear(Cache::Quality) => "pref-clear-quality",
            Control::OpenUnsaved => "pref-open-unsaved",
            Control::Close => "pref-close",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A control was clicked.
    Press(Control),
    ThumbSize(f32),
    /// The thumbnail-size slider was let go: save.
    ThumbSizeDone,
    Surveyed(u64, Option<Survey>),
    ClearConfirmed,
    ConfirmCancel,
    Cleared(Cache, Option<(Cleared, Usage)>),
    /// The closeness slider moved (its place on the track, 0 closer to 1
    /// looser; see `closeness_to_track`).
    Closeness(f32),
    /// The slider was let go: save, and search again.
    ClosenessDone,
    /// Bring a control into view (after the window has opened).
    Reveal(Control),
    /// Count the sizes again (after the engine cleared or deleted).
    Recount,
    /// The record of corrections was exported: the folder made and the
    /// entries copied, or what went wrong.
    Exported(Result<(PathBuf, usize), String>),
    /// A face model picked in *Change the face model*.
    PickFaceModel(crate::ml::models::ModelId),
    /// Change to it (or cancel: `false`).
    ChangeFaceModel(bool),
}

/// What a confirmation asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Cache(Cache),
    SearchRebuild,
    SearchClear,
    ModelsDelete,
    /// Forget every face found and every name given.
    ForgetPeople,
    /// Forget every fursuit head found and every fursuit name given.
    ForgetFursuits,
    /// Delete a close-call judge's model.
    DeleteJudge(JudgeChoice),
    /// Delete the record of corrections.
    DeleteCorrections,
}

/// The Preferences window and what it has counted.
pub struct Prefs {
    pub window: Option<window::Id>,
    /// The global menu was offered when the app started (it is linked only
    /// then, so a change waits for the next start).
    started_with_global_menu: bool,
    focus: Option<Control>,
    /// Draw the focus ring: after a key press, not after a click.
    ring: bool,
    survey: Option<Survey>,
    counting: bool,
    survey_seq: u64,
    surveyed_at: Option<Instant>,
    clearing: Option<Cache>,
    /// The last clear, for a word under that cache's size.
    last_clear: Option<(Cache, Cleared)>,
    /// What a confirmation is asking about.
    confirm: Option<Ask>,
    /// *Find people's faces* was just turned off: offer to forget them.
    faces_offer: bool,
    /// *Change the face model* is open, with the model picked.
    face_change: Option<crate::ml::models::ModelId>,
    /// What the last export of the record did, for its row.
    exported: Option<String>,
}

impl Prefs {
    pub fn new(s: &Settings) -> Self {
        Self {
            window: None,
            started_with_global_menu: s.global_menu_wanted(settings::global_menu_forced_off()),
            focus: None,
            ring: false,
            survey: None,
            counting: false,
            survey_seq: 0,
            surveyed_at: None,
            clearing: None,
            last_clear: None,
            confirm: None,
            faces_offer: false,
            face_change: None,
            exported: None,
        }
    }

    fn usage(&self, c: Cache) -> Option<Usage> {
        self.survey.as_ref()?.caches.iter().find(|(k, _)| *k == c).map(|(_, u)| *u)
    }

    /// The search index's and the models' sizes, once counted.
    fn search_storage(&self) -> Option<&storage::SearchStorage> {
        self.survey.as_ref().map(|s| &s.search)
    }

    /// One line for the test driver.
    pub fn debug_state(&self) -> String {
        let sizes: Vec<String> = Cache::ALL
            .iter()
            .map(|&c| match self.usage(c) {
                Some(u) => format!("{c:?}={}B/{}files/exists={}", u.bytes, u.files, u.exists),
                None => format!("{c:?}=…"),
            })
            .collect();
        let journal = self.survey.as_ref().map(|s| format!("{:?}", s.journal)).unwrap_or_else(|| "…".into());
        format!(
            "open={} focus={:?} ring={} confirm={:?} clearing={:?} counting={} sizes=[{}] search={:?} journal={journal} last_clear={:?} faces_offer={} face_change={:?}",
            self.window.is_some(),
            self.focus,
            self.ring,
            self.confirm,
            self.clearing,
            self.counting,
            sizes.join(", "),
            self.search_storage(),
            self.last_clear,
            self.faces_offer,
            self.face_change,
        )
    }
}

fn m(msg: Msg) -> Message {
    Message::Prefs(msg)
}

/// Run `f` on a thread of its own; `None` if it panicked.  (The search
/// page runs its own work here too.)
pub(super) async fn on_worker<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        let _ = tx.send(f());
    });
    if spawned.is_err() {
        return None;
    }
    rx.await.ok()
}

// ── the window ──────────────────────────────────────────────────────────────

/// Ctrl+, (or the menus): open the window, or bring it to the front.
pub fn open(app: &mut App) -> Task<Message> {
    if let Some(id) = app.prefs.window {
        return window::gain_focus(id);
    }
    let (id, opened) = window::open(window::Settings {
        size: Size::new(700.0, 840.0),
        position: window::Position::Centered,
        min_size: Some(Size::new(540.0, 420.0)),
        ..super::window_settings()
    });
    let p = &mut app.prefs;
    p.window = Some(id);
    p.focus = None;
    p.ring = false;
    p.confirm = None;
    p.last_clear = None;
    Task::batch([opened.map(Message::WindowOpened), recount(app)])
}

/// Open the window (or bring it up) with `c` in view and ringed: the
/// search page's and the banner's way to Preferences ▸ Search.
pub fn open_at(app: &mut App, c: Control) -> Task<Message> {
    let opened = open(app);
    app.prefs.focus = Some(c);
    app.prefs.ring = true;
    // A window just opened has no layout yet: reveal once it has.
    let later = Task::perform(tokio::time::sleep(Duration::from_millis(200)), move |_| m(Msg::Reveal(c)));
    Task::batch([opened, reveal_search_section(), later])
}

pub fn close(app: &mut App) -> Task<Message> {
    app.prefs.confirm = None;
    match app.prefs.window.take() {
        Some(id) => window::close(id),
        None => Task::none(),
    }
}

pub fn on_closed(app: &mut App) {
    app.prefs.window = None;
    app.prefs.confirm = None;
}

/// The window came to the front: the caches may have grown meanwhile.
pub fn on_focused(app: &mut App) -> Task<Message> {
    let p = &app.prefs;
    if p.counting || p.clearing.is_some() || p.surveyed_at.is_some_and(|at| at.elapsed() < RECOUNT_AFTER) {
        return Task::none();
    }
    recount(app)
}

fn recount(app: &mut App) -> Task<Message> {
    let p = &mut app.prefs;
    p.survey_seq += 1;
    p.counting = true;
    let seq = p.survey_seq;
    let library = app.library.clone();
    let decisions = app.search.link.decisions().path().map(Path::to_path_buf);
    Task::perform(on_worker("prefs-count", move || storage::survey(library.as_deref(), decisions.as_deref())), move |s| m(Msg::Surveyed(seq, s)))
}

/// A folder chosen for the edit copies (Change… on the page).
pub fn edit_copy_folder_picked(app: &mut App, path: PathBuf) {
    app.settings.edit_copy_folder = path.to_string_lossy().into_owned();
    app.settings.save_in_background();
    dialogs::edit_copy_settings_changed(app);
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Press(c) => {
            app.prefs.focus = Some(c);
            app.prefs.ring = false;
            press(app, c)
        }
        Msg::ThumbSize(v) => {
            set_thumb_size(app, v);
            Task::none()
        }
        Msg::ThumbSizeDone => {
            app.settings.save_in_background();
            Task::none()
        }
        Msg::Surveyed(seq, survey) => {
            let p = &mut app.prefs;
            if seq == p.survey_seq {
                p.counting = false;
                p.surveyed_at = Some(Instant::now());
                if survey.is_some() {
                    p.survey = survey;
                }
            }
            Task::none()
        }
        Msg::ConfirmCancel => {
            app.prefs.confirm = None;
            Task::none()
        }
        Msg::ClearConfirmed => {
            let p = &mut app.prefs;
            let cache = match p.confirm.take() {
                None => return Task::none(),
                Some(Ask::Cache(c)) => c,
                Some(search) => return search_confirmed(app, search),
            };
            if p.clearing.is_some() {
                return Task::none();
            }
            p.clearing = Some(cache);
            p.last_clear = None;
            let work = move || {
                let cleared = cache.clear();
                let usage = cache.dir().map(|d| storage::measure(&d)).unwrap_or_default();
                (cleared, usage)
            };
            Task::perform(on_worker("prefs-clear", work), move |r| m(Msg::Cleared(cache, r)))
        }
        Msg::Cleared(cache, result) => {
            let was_counting = app.prefs.counting;
            let p = &mut app.prefs;
            p.clearing = None;
            if let Some((cleared, usage)) = result {
                if let Some(entry) = p.survey.as_mut().and_then(|s| s.caches.iter_mut().find(|(k, _)| *k == cache)) {
                    entry.1 = usage;
                }
                p.last_clear = Some((cache, cleared));
            }
            // A count that started before the clear would bring back the
            // old size: count again instead.
            if was_counting { recount(app) } else { Task::none() }
        }
        Msg::Closeness(position) => {
            app.settings.search_closeness = track_to_closeness(position);
            Task::none()
        }
        Msg::ClosenessDone => closeness_changed(app),
        Msg::Reveal(c) if matches!(c, Control::SearchDevice(_)) => reveal_search_section(),
        Msg::Reveal(c) => reveal(c),
        Msg::Recount => recount(app),
        Msg::Exported(r) => {
            let text = match r {
                Ok((dir, n)) => format!("Exported {} with a README of the format to {}.", util::plural(n as u64, "correction"), tilde(&dir)),
                Err(e) => format!("Couldn't export the record: {e}."),
            };
            app.prefs.exported = Some(text.clone());
            app.show_status(text, 8000)
        }
        Msg::PickFaceModel(f) => {
            if app.prefs.face_change.is_some() {
                app.prefs.face_change = Some(f);
            }
            Task::none()
        }
        Msg::ChangeFaceModel(go) => {
            let Some(f) = app.prefs.face_change.take() else { return Task::none() };
            if !go || super::subjects::face_model(app) == f {
                return Task::none();
            }
            app.settings.face_model = f;
            app.settings.save_in_background();
            app.search.link.set_face_model(f);
            let name = crate::ml::models::info(f).name;
            app.show_status(format!("Recognising every face again with {name}. Names and the photos you confirmed are kept."), 8000)
        }
    }
}

/// The closeness was set: save it and search again.
fn closeness_changed(app: &mut App) -> Task<Message> {
    app.settings.save_in_background();
    super::search::rerun(app);
    Task::none()
}

fn set_closeness(app: &mut App, v: f32) -> Task<Message> {
    app.settings.search_closeness = v.clamp(CLOSENESS_MIN, CLOSENESS_MAX);
    closeness_changed(app)
}

/// A search storage action was confirmed: the engine does it on its own
/// thread; the sizes are counted again after.
fn search_confirmed(app: &mut App, ask: Ask) -> Task<Message> {
    let link = &app.search.link;
    let text = match ask {
        Ask::SearchRebuild => {
            link.rebuild_index();
            "Rebuilding the search index. Search works on what has been rebuilt so far."
        }
        Ask::SearchClear => {
            link.clear_index();
            "Cleared the search index."
        }
        Ask::ModelsDelete => {
            link.delete_models();
            "Deleted the downloaded models."
        }
        Ask::ForgetFursuits => {
            link.forget_all_fursuits();
            "Forgot every fursuit head found and every fursuit name given, and the fursuit corrections in your record. Your photos are not changed."
        }
        Ask::DeleteCorrections => {
            link.decisions().delete_all();
            app.prefs.exported = None;
            "Deleted the record of your corrections. Your names and groups are not changed."
        }
        Ask::DeleteJudge(c) => {
            // The chosen judge turns off with its files; another one's
            // files go alone.
            let chosen = app.settings.fursuit_judge == c;
            if chosen {
                app.settings.fursuit_judge = JudgeChoice::Off;
                app.settings.save_in_background();
                app.search.link.set_judge(JudgeChoice::Off);
            }
            app.search.link.delete_judge(c);
            let text = if chosen {
                format!("Deleted {}, and turned the close-call judge off. The close calls it checked are remembered.", c.label())
            } else {
                format!("Deleted {}. The close calls it checked are remembered.", c.label())
            };
            let status = app.show_status(text, 6000);
            let later = Task::perform(tokio::time::sleep(Duration::from_millis(600)), |_| m(Msg::Recount));
            return Task::batch([status, later]);
        }
        Ask::ForgetPeople => {
            link.forget_all_people();
            app.subjects.crops.clear();
            "Forgot every face found and every name given, and the people corrections in your record. Your photos are not changed."
        }
        Ask::Cache(_) => return Task::none(),
    };
    let status = app.show_status(text, 6000);
    let later = Task::perform(tokio::time::sleep(Duration::from_millis(600)), |_| m(Msg::Recount));
    Task::batch([status, later])
}

/// Snap like the filter bar's slider, and remember it.
fn set_thumb_size(app: &mut App, v: f32) {
    let _ = app.on_filter(FilterMsg::Zoom(v));
    app.settings.thumb_size = app.thumb_size as u32;
}

fn press(app: &mut App, c: Control) -> Task<Message> {
    match c {
        Control::Advance => viewer_advance::toggle(app),
        Control::ShowPruned => app.toggle_show_pruned(),
        Control::FileNames => app.run_cmd(Cmd::FileNames),
        Control::StackBursts => app.run_cmd(Cmd::StackBursts),
        Control::ThumbSize => Task::none(),
        Control::Filmstrip => {
            viewer_filmstrip::toggle(app);
            Task::none()
        }
        Control::BurstSheet => {
            app.settings.viewer_burst_sheet = !app.settings.viewer_burst_sheet;
            app.settings.save_in_background();
            Task::none()
        }
        Control::ViewerSize => {
            let s = &mut app.settings;
            (s.viewer_width, s.viewer_height, s.viewer_x, s.viewer_y, s.viewer_maximized) = (0.0, 0.0, None, None, false);
            s.save_in_background();
            Task::none()
        }
        Control::Library => app.pick_folder(FolderTarget::Library),
        Control::EditCopyFolder => app.pick_folder(FolderTarget::EditCopySetting),
        Control::EditCopySubfolder => {
            app.settings.edit_copy_subfolder = !app.settings.edit_copy_subfolder;
            app.settings.save_in_background();
            dialogs::edit_copy_settings_changed(app);
            Task::none()
        }
        Control::GlobalMenu => {
            if !settings::global_menu_forced_off() {
                app.settings.global_menu = !app.settings.global_menu;
                app.settings.save_in_background();
            }
            Task::none()
        }
        Control::SearchDevice(d) => {
            if app.settings.search_device != d {
                app.settings.search_device = d;
                app.settings.save_in_background();
                app.search.link.set_device(d);
            }
            Task::none()
        }
        Control::CheckCard => {
            app.search.link.try_card_again();
            Task::none()
        }
        Control::Closeness => Task::none(),
        Control::ClosenessReset => set_closeness(app, CLOSENESS_DEFAULT),
        Control::IndexExports => {
            let on = !app.settings.search_index_exports;
            app.settings.search_index_exports = on;
            app.settings.save_in_background();
            app.search.link.set_index_exports(on);
            Task::none()
        }
        Control::ForgetSearches => {
            app.settings.forget_searches();
            app.settings.save_in_background();
            app.show_status("Recent searches forgotten.", 4000)
        }
        Control::FindFaces => {
            let on = !app.settings.find_faces;
            app.settings.find_faces = on;
            app.settings.save_in_background();
            app.search.link.set_find_faces(on);
            // Off, with faces already found: offer to forget them.
            let f = &app.search.status.faces;
            app.prefs.faces_offer = !on && (f.found > 0 || f.named > 0);
            Task::none()
        }
        Control::KeepFaces => {
            app.prefs.faces_offer = false;
            app.show_status("Finding faces is off. The faces found and the names you gave are kept.", 5000)
        }
        Control::ForgetFaces => {
            app.prefs.faces_offer = false;
            app.search.link.forget_all_people();
            app.subjects.crops.clear();
            app.show_status("Finding faces is off, and the faces found and the names you gave are forgotten.", 6000)
        }
        Control::FaceModel => {
            if app.settings.find_faces {
                let now = super::subjects::face_model(app);
                // The dialog opens on another model: the one in use is
                // already in use.
                let other = crate::ml::models::face_models().map(|m| m.id).find(|&m| m != now).unwrap_or(now);
                app.prefs.face_change = Some(other);
            }
            Task::none()
        }
        Control::SmallestGroup(n) => {
            app.settings.people_min_photos = n;
            app.settings.save_in_background();
            Task::none()
        }
        Control::ForgetPeople => {
            if people_to_forget(app) {
                app.prefs.confirm = Some(Ask::ForgetPeople);
            }
            Task::none()
        }
        Control::FindHeads => {
            let on = !app.settings.find_heads;
            app.settings.find_heads = on;
            app.settings.save_in_background();
            app.search.link.set_find_heads(on);
            if on {
                Task::none()
            } else {
                app.show_status("Finding fursuit heads is off. The heads found and the names you gave are kept.", 5000)
            }
        }
        Control::RailSubjects => {
            app.settings.rail_subjects = !app.settings.rail_subjects;
            app.settings.save_in_background();
            Task::none()
        }
        Control::FindHeadsNow => {
            if !heads_catch_up(app) {
                return Task::none();
            }
            let t = super::subjects_fursuits::update(app, super::subjects_fursuits::Msg::FindHeads);
            // The download dialog (if the models are missing) opens in the
            // main window.
            Task::batch([t, close(app)])
        }
        Control::ForgetFursuits => {
            if fursuits_to_forget(app) {
                app.prefs.confirm = Some(Ask::ForgetFursuits);
            }
            Task::none()
        }
        Control::KeepCorrections => {
            let on = !app.settings.keep_corrections;
            app.settings.keep_corrections = on;
            app.settings.save_in_background();
            app.search.link.decisions().set_on(on);
            if on {
                Task::none()
            } else {
                app.show_status("Corrections are no longer recorded. What was recorded is kept until you delete it.", 5000)
            }
        }
        Control::ExportCorrections => {
            if !corrections_kept(&app.prefs) {
                return Task::none();
            }
            app.pick_folder(FolderTarget::ExportCorrections)
        }
        Control::DeleteCorrections => {
            if corrections_kept(&app.prefs) {
                app.prefs.confirm = Some(Ask::DeleteCorrections);
            }
            Task::none()
        }
        Control::Grouping(g) => choose_grouping(app, g),
        Control::Judge(c) => choose_judge(app, c),
        Control::JudgeDownload => {
            let c = app.settings.fursuit_judge;
            if c.model().is_none() || app.search.status.judge.downloaded.contains(&c) || judge_downloading(app) {
                return Task::none();
            }
            app.search.dialog = Some(super::search::Dialog::judge(c, &app.settings));
            close(app)
        }
        Control::UpdateHeads => {
            if !heads_to_update(app) {
                return Task::none();
            }
            super::subjects_fursuits::update(app, super::subjects_fursuits::Msg::UpdateHeads)
        }
        Control::JudgeOpen(c) => {
            if let Some(d) = judge_dir(c).filter(|_| judge_exists(&app.prefs, c)) {
                dialogs::open_folder(&d);
            }
            Task::none()
        }
        Control::JudgeDelete(c) => {
            if judge_exists(&app.prefs, c) {
                app.prefs.confirm = Some(Ask::DeleteJudge(c));
            }
            Task::none()
        }
        Control::SearchRebuild | Control::SearchClear | Control::ModelsDelete => {
            if search_action_ready(app, c) {
                app.prefs.confirm = Some(match c {
                    Control::SearchRebuild => Ask::SearchRebuild,
                    Control::SearchClear => Ask::SearchClear,
                    _ => Ask::ModelsDelete,
                });
            }
            Task::none()
        }
        Control::ModelsOpen => {
            if let Some(d) = crate::ml::models::models_dir().filter(|_| models_exist(&app.prefs)) {
                dialogs::open_folder(&d);
            }
            Task::none()
        }
        Control::Open(cache) => {
            if let Some(d) = cache.dir() {
                dialogs::open_folder(&d);
            }
            Task::none()
        }
        Control::Clear(cache) => {
            if clearable(&app.prefs, cache) {
                app.prefs.confirm = Some(Ask::Cache(cache));
            }
            Task::none()
        }
        Control::OpenUnsaved => {
            if let Some(d) = crate::model::saver::default_journal_dir() {
                dialogs::open_folder(&d);
            }
            Task::none()
        }
        Control::Close => close(app),
    }
}

/// The record of corrections holds something (as last counted).
fn corrections_kept(p: &Prefs) -> bool {
    p.survey.as_ref().is_some_and(|s| s.decisions.0 > 0)
}

/// A folder picked for the record's export: copied there on the record's
/// own thread (a worker waits for it), never on this one.
pub fn export_corrections(app: &mut App, into: PathBuf) -> Task<Message> {
    let recorder = app.search.link.decisions();
    let work = move || recorder.export(&into).map_err(|e| e.to_string());
    Task::perform(on_worker("prefs-export", work), |r| m(Msg::Exported(r.unwrap_or_else(|| Err("the export stopped".into())))))
}

fn clearable(p: &Prefs, c: Cache) -> bool {
    p.clearing.is_none() && p.usage(c).is_some_and(|u| u.files > 0)
}

/// Something about people is kept: faces found or names given.
fn people_to_forget(app: &App) -> bool {
    let f = &app.search.status.faces;
    f.found > 0 || f.named > 0 || !app.subjects.all.groups.is_empty()
}

/// Something about fursuits is kept: heads found or names given.
fn fursuits_to_forget(app: &App) -> bool {
    let h = &app.search.status.heads;
    h.found > 0 || h.named > 0 || app.subjects.all.groups.iter().any(|g| g.kind == super::people::Kind::Fursuit)
}

/// Why `n` fursuit photos weren't looked at for heads: indexed before
/// heads were looked for, or their scan skipped (`skipped` of them).
fn not_looked_at(n: usize, skipped: usize) -> String {
    let photos = util::plural(n as u64, "photo");
    let (was, their) = if n == 1 { ("was", "its scan was") } else { ("were", "their scans were") };
    if skipped == 0 {
        format!("{photos} with fursuits {was} indexed before heads were looked for.")
    } else if skipped >= n {
        format!("{photos} with fursuits: {their} skipped.")
    } else {
        format!("{photos} with fursuits {was} indexed before heads were looked for, or {their} skipped.")
    }
}

/// Fursuit photos indexed and not looked at for heads, with nothing
/// running or waiting: Preferences offers them, with their cost.
fn heads_catch_up(app: &App) -> bool {
    app.settings.find_heads && app.search.status.heads.catch_up_wanted() && app.search.status.indexed_shots > 0
}

/// A grouping was picked: the fursuits are grouped again on the engine's
/// worker; the tab says so meanwhile.  Names and answers stay.
fn choose_grouping(app: &mut App, g: crate::search::heads::Grouping) -> Task<Message> {
    if app.settings.fursuit_grouping == g {
        return Task::none();
    }
    app.settings.fursuit_grouping = g;
    app.settings.save_in_background();
    app.search.link.set_fursuit_grouping(g);
    app.show_status(format!("Fursuit grouping: {}. The groups are made again; names, answers and labels stay.", g.label()), 6000)
}

/// What a grouping choice means, in plain words.
pub fn grouping_words(g: crate::search::heads::Grouping) -> &'static str {
    match g {
        crate::search::heads::Grouping::Tighter => "More, smaller groups; rarely mixes suits.",
        crate::search::heads::Grouping::Balanced => "Fewer split-up suits; now and then a stray to move out.",
        crate::search::heads::Grouping::Looser => "The fewest small groups; check groups for strays.",
    }
}

/// Preferences ▸ Fursuits ▸ *Grouping*: Tighter, Balanced (the default),
/// Looser, and a calm line while the groups are made again.
fn grouping_setting(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let chosen = app.settings.fursuit_grouping;
    let mut list = column![].spacing(2).padding(2);
    for g in crate::search::heads::Grouping::ALL {
        let on = g == chosen;
        let mut top = row![text(g.label()).size(t::T_BODY).font(paint::MEDIUM).color(t::INK)].spacing(8).align_y(Alignment::Center);
        if g == crate::search::heads::Grouping::default() {
            top = top.push(text("The default").size(t::T_SMALL).color(t::INK3));
        }
        let body = column![top, text(grouping_words(g)).size(t::T_SMALL).color(t::INK2)].spacing(2).width(Length::Fill);
        let dot = container(if on {
            Element::from(container(Space::new()).width(8).height(8).style(|_| container::Style { background: Some(Background::Color(t::SIGNAL)), border: Border { radius: 4.0.into(), ..Border::default() }, ..Default::default() }))
        } else {
            Space::new().into()
        })
        .width(16)
        .height(16)
        .center(16)
        .style(move |_| container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(if on { t::SIGNAL } else { t::LINE_HI }, 1.0, 8.0), ..Default::default() });
        let b = iced::widget::button(row![container(dot).padding(Padding { top: 1.0, ..Padding::ZERO }), body].spacing(t::S3))
            .width(Length::Fill)
            .padding(Padding::from([7.0, 12.0]))
            .style(move |_t, status| {
                let hot = matches!(status, iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed);
                iced::widget::button::Style { background: (on || hot).then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..iced::widget::button::Style::default() }
            })
            .on_press(m(Msg::Press(Control::Grouping(g))));
        list = list.push(focusable(p, Control::Grouping(g), b));
    }
    let mut col = column![
        text("Grouping").size(t::T_BODY).font(paint::MEDIUM).color(t::INK),
        about("How readily heads that look alike are grouped together. Tighter keeps one suit at one event per group; looser joins more of a suit, and now and then a look-alike. Possibly the same fursuit and Split into a new group fix either kind of mistake."),
        Space::new().height(t::S1),
        container(list).width(Length::Fill).style(style::group()),
    ]
    .spacing(3);
    if app.search.status.heads.regrouping {
        col = col.push(Space::new().height(t::S1));
        col = col.push(row![icons::view(Glyph::Info, 13.0, t::INK3), text("Grouping the fursuits again. Names, answers and labels stay.").size(t::T_SMALL).color(t::INK3)].spacing(6).align_y(Alignment::Center));
    }
    container(col).padding(Padding::from([t::S3, t::S4])).width(Length::Fill).into()
}

/// A close-call judge was picked: Off at once; a judge in place at once;
/// one not downloaded asks first, in the download dialog (the main
/// window's), with its size, and is chosen when that is answered.
fn choose_judge(app: &mut App, c: JudgeChoice) -> Task<Message> {
    if app.settings.fursuit_judge == c {
        return Task::none();
    }
    if c.model().is_some() && !app.search.status.judge.downloaded.contains(&c) {
        app.search.dialog = Some(super::search::Dialog::judge(c, &app.settings));
        return close(app);
    }
    app.settings.fursuit_judge = c;
    app.settings.save_in_background();
    app.search.link.set_judge(c);
    match c {
        JudgeChoice::Off => app.show_status("The close-call judge is off: close calls are matched by look alone. Its model stays downloaded until deleted in Storage.", 6000),
        _ => app.show_status(format!("{} checks close calls from now on, in Find more and the \u{201c}Same fursuit?\u{201d} review.", c.label()), 6000),
    }
}

/// The chosen judge's model is coming down now.
fn judge_downloading(app: &App) -> bool {
    app.search.downloading() && super::search_setup::judge_downloads(&app.search.downloads)
}

/// Heads found before v2 wait for the search model's vectors, with
/// nothing running or waiting.
fn heads_to_update(app: &App) -> bool {
    let h = &app.search.status.heads;
    h.vectors_missing > 0 && matches!(h.step, super::fursuits::Step::Idle)
}

/// A judge's model folder.
fn judge_dir(c: JudgeChoice) -> Option<PathBuf> {
    c.model().and_then(crate::ml::models::model_dir)
}

/// A judge's files on disk, once counted.
fn judge_bytes(p: &Prefs, c: JudgeChoice) -> Option<u64> {
    p.search_storage().map(|s| s.judges.iter().find(|(k, _)| *k == c).map_or(0, |(_, b)| *b))
}

fn judge_exists(p: &Prefs, c: JudgeChoice) -> bool {
    judge_bytes(p, c).is_some_and(|b| b > 0)
}

/// The judges with a model, in the list's order.
fn judges() -> impl Iterator<Item = JudgeChoice> {
    JudgeChoice::ALL.into_iter().filter(|c| c.model().is_some())
}

fn models_exist(p: &Prefs) -> bool {
    p.search_storage().is_some_and(|s| s.models_bytes > 0)
}

/// The delete confirmation's title per judge (static: the dialog takes a
/// `&'static str`).
const DELETE_TITLES: [(JudgeChoice, &str); 2] = [(JudgeChoice::Qwen3VlReranker, "Delete Qwen3-VL-Reranker?"), (JudgeChoice::Gemma4, "Delete Gemma 4?")];

/// Whether a search storage action can be asked for now.
fn search_action_ready(app: &App, c: Control) -> bool {
    let st = &app.search.status;
    let has_index = st.indexed_shots > 0 || app.prefs.search_storage().is_some_and(|s| s.index_bytes > 0);
    match c {
        Control::SearchRebuild => st.models_installed && has_index && st.pass.is_none(),
        Control::SearchClear => has_index,
        Control::ModelsDelete => st.models_installed || models_exist(&app.prefs),
        _ => false,
    }
}

fn remembered_viewer_size(s: &Settings) -> bool {
    s.viewer_maximized || (s.viewer_width >= super::viewer_window::MIN_SIZE.width && s.viewer_height >= super::viewer_window::MIN_SIZE.height)
}

/// The controls on the page now, in Tab order (ones that can't be used
/// right now are skipped).
fn controls(app: &App) -> Vec<Control> {
    let p = &app.prefs;
    let mut v = vec![
        Control::Advance,
        Control::ShowPruned,
        Control::FileNames,
        Control::StackBursts,
        Control::ThumbSize,
        Control::Filmstrip,
        Control::BurstSheet,
    ];
    if remembered_viewer_size(&app.settings) {
        v.push(Control::ViewerSize);
    }
    v.extend([Control::Library, Control::EditCopyFolder, Control::EditCopySubfolder]);
    v.extend([Control::SearchDevice(Device::Gpu), Control::SearchDevice(Device::Cpu)]);
    if app.settings.search_device == Device::Gpu {
        v.push(Control::CheckCard);
    }
    v.push(Control::Closeness);
    if !at_default(app.settings.closeness()) {
        v.push(Control::ClosenessReset);
    }
    v.push(Control::IndexExports);
    if !app.settings.search_recent.is_empty() {
        v.push(Control::ForgetSearches);
    }
    v.push(Control::FindFaces);
    if app.prefs.faces_offer {
        v.extend([Control::KeepFaces, Control::ForgetFaces]);
    }
    if app.settings.find_faces {
        v.push(Control::FaceModel);
    }
    v.extend(crate::settings::PEOPLE_MIN_PHOTOS_CHOICES.iter().map(|&n| Control::SmallestGroup(n)));
    if people_to_forget(app) {
        v.push(Control::ForgetPeople);
    }
    v.push(Control::RailSubjects);
    v.push(Control::FindHeads);
    if heads_catch_up(app) {
        v.push(Control::FindHeadsNow);
    }
    if heads_to_update(app) {
        v.push(Control::UpdateHeads);
    }
    v.extend(crate::search::heads::Grouping::ALL.iter().map(|&g| Control::Grouping(g)));
    v.extend(JudgeChoice::ALL.iter().map(|&c| Control::Judge(c)));
    if judge_wants_download(app) {
        v.push(Control::JudgeDownload);
    }
    if fursuits_to_forget(app) {
        v.push(Control::ForgetFursuits);
    }
    if !settings::global_menu_forced_off() {
        v.push(Control::GlobalMenu);
    }
    for c in Cache::ALL {
        if p.usage(c).is_some_and(|u| u.exists) {
            v.push(Control::Open(c));
        }
        if clearable(p, c) {
            v.push(Control::Clear(c));
        }
    }
    for c in [Control::SearchRebuild, Control::SearchClear] {
        if search_action_ready(app, c) {
            v.push(c);
        }
    }
    for c in judges() {
        if judge_exists(p, c) {
            v.extend([Control::JudgeOpen(c), Control::JudgeDelete(c)]);
        }
    }
    if models_exist(p) {
        v.push(Control::ModelsOpen);
    }
    if search_action_ready(app, Control::ModelsDelete) {
        v.push(Control::ModelsDelete);
    }
    if p.survey.as_ref().is_some_and(|s| s.journal.exists) {
        v.push(Control::OpenUnsaved);
    }
    v.push(Control::KeepCorrections);
    if corrections_kept(p) {
        v.extend([Control::ExportCorrections, Control::DeleteCorrections]);
    }
    v.push(Control::Close);
    v
}

/// A key press in the Preferences window.
pub fn on_key(app: &mut App, key: Key, mods: keyboard::Modifiers) -> Task<Message> {
    if app.prefs.face_change.is_some() {
        return match key {
            Key::Named(Named::Escape) => update(app, Msg::ChangeFaceModel(false)),
            Key::Named(Named::Enter) => update(app, Msg::ChangeFaceModel(true)),
            _ => Task::none(),
        };
    }
    if app.prefs.confirm.is_some() {
        return match key {
            Key::Named(Named::Escape) => update(app, Msg::ConfirmCancel),
            Key::Named(Named::Enter) => update(app, Msg::ClearConfirmed),
            _ => Task::none(),
        };
    }
    if let Key::Named(dir @ (Named::ArrowLeft | Named::ArrowRight | Named::ArrowDown | Named::ArrowUp)) = &key {
        if app.prefs.focus == Some(Control::Closeness) {
            // ← closer, → looser, as the slider runs: a step along its
            // track, so the default is reached from either end.
            let looser = matches!(*dir, Named::ArrowRight | Named::ArrowUp);
            let p = closeness_to_track(app.settings.closeness()) + if looser { TRACK_STEP } else { -TRACK_STEP };
            app.prefs.ring = true;
            return set_closeness(app, track_to_closeness(p));
        }
    }
    match &key {
        Key::Named(Named::Escape) => return close(app),
        Key::Named(Named::Tab) => return move_focus(app, if mods.shift() { -1 } else { 1 }),
        Key::Named(Named::Space | Named::Enter) if !mods.control() => {
            return match app.prefs.focus.filter(|c| controls(app).contains(c)) {
                Some(c) => {
                    app.prefs.ring = true;
                    press(app, c)
                }
                None => Task::none(),
            };
        }
        Key::Named(dir @ (Named::ArrowLeft | Named::ArrowRight | Named::ArrowDown | Named::ArrowUp)) if app.prefs.focus == Some(Control::ThumbSize) => {
            let step = if matches!(*dir, Named::ArrowLeft | Named::ArrowDown) { -super::ZOOM_STEP } else { super::ZOOM_STEP };
            let v = app.thumb_size + step;
            set_thumb_size(app, v);
            app.prefs.ring = true;
            app.settings.save_in_background();
            return Task::none();
        }
        _ => {}
    }
    // The app's own commands that make sense from here.
    match Cmd::from_key(&key, mods) {
        Some(cmd @ (Cmd::Quit | Cmd::AdvanceAfterMark | Cmd::Shortcuts | Cmd::Undo | Cmd::Redo)) => app.update(Message::Cmd(cmd)),
        _ => Task::none(),
    }
}

fn move_focus(app: &mut App, dir: i32) -> Task<Message> {
    let list = controls(app);
    let n = list.len() as i32;
    let at = app.prefs.focus.and_then(|f| list.iter().position(|&c| c == f));
    let next = match at {
        // Without a ring, Tab shows it where the focus is.
        Some(i) if !app.prefs.ring => i as i32,
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None if dir > 0 => 0,
        None => n - 1,
    };
    let c = list[next as usize];
    app.prefs.focus = Some(c);
    app.prefs.ring = true;
    reveal(c)
}

// ── scrolling the focused control into view ─────────────────────────────────

/// Finds the control and the page's scroll position, then scrolls just
/// enough to show the control.
struct Reveal {
    target: Id,
    page: Id,
    view: Option<(Rectangle, Rectangle, Vector)>,
    found: Option<Rectangle>,
    /// Put the target at the top of the page (a section), rather than
    /// scrolling just far enough to show it (a control).
    to_top: bool,
}

impl Operation for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn scrollable(&mut self, id: Option<&Id>, bounds: Rectangle, content: Rectangle, translation: Vector, _state: &mut dyn adv::operation::Scrollable) {
        if id == Some(&self.page) {
            self.view = Some((bounds, content, translation));
        }
    }

    fn finish(&self) -> Outcome<()> {
        let (Some((view, content, offset)), Some(target)) = (self.view, self.found) else { return Outcome::None };
        let wanted = if self.to_top {
            Some((target.y - content.y - t::S4).clamp(0.0, (content.height - view.height).max(0.0)))
        } else {
            reveal_offset(view.height, content.height, offset.y, target.y - content.y, target.height)
        };
        match wanted {
            Some(y) => Outcome::Chain(Box::new(adv::operation::scrollable::scroll_to(self.page.clone(), AbsoluteOffset { x: None, y: Some(y) }))),
            None => Outcome::None,
        }
    }
}

/// The scroll offset that shows a control at `top` (of `height`) on a
/// page `content_h` high, in a viewport `view_h` high now scrolled to
/// `offset`; `None` if it shows already.  It keeps some of the control's
/// surroundings in view, and near the foot of the page shows the rest.
fn reveal_offset(view_h: f32, content_h: f32, offset: f32, top: f32, height: f32) -> Option<f32> {
    const MARGIN: f32 = 48.0;
    const FOOT: f32 = 120.0;
    let max = (content_h - view_h).max(0.0);
    let bottom = top + height;
    let y = if top - MARGIN < offset {
        top - MARGIN
    } else if bottom + MARGIN > offset + view_h {
        if content_h - bottom < FOOT { max } else { bottom + MARGIN - view_h }
    } else {
        return None;
    };
    Some(y.clamp(0.0, max))
}

fn reveal(c: Control) -> Task<Message> {
    if c == Control::Close {
        return Task::none();
    }
    adv::operate(Reveal { target: Id::new(c.id()), page: Id::new(SCROLL), view: None, found: None, to_top: false }).discard()
}

/// The search section's id, to bring it to the top of the page.
const SEARCH_SECTION: &str = "pref-section-search";

fn reveal_search_section() -> Task<Message> {
    adv::operate(Reveal { target: Id::new(SEARCH_SECTION), page: Id::new(SCROLL), view: None, found: None, to_top: true }).discard()
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;

    let page = column![
        column![
            text("Preferences").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK),
            kit::secondary("Changes take effect straight away and are saved as you make them."),
        ]
        .spacing(t::S1),
        culling(app),
        grid(app),
        viewer(app),
        folders(app),
        search_section(app),
        people_section(app),
        fursuits_section(app),
        desktop(app),
        storage_section(app),
        kit::caption(format!(
            "Settings are kept in {}.",
            settings::path().map(|f| tilde(&f)).unwrap_or_else(|| "settings.json".into())
        )),
    ]
    .spacing(t::S5 + t::S2)
    .max_width(PAGE_W);

    let body = scrollable(container(page).center_x(Length::Fill).padding(Padding { top: t::S5, bottom: t::S5 + t::S2, left: t::S5, right: t::S5 }))
        .id(SCROLL)
        .style(style::scroll)
        .height(Length::Fill);

    let close_btn = focusable(p, Control::Close, kit::btn("Close", style::secondary(), Some(m(Msg::Press(Control::Close)))));
    let bar = container(
        row![kit::caption("Esc closes  ·  Tab moves between settings  ·  Space changes one"), Space::new().width(Length::Fill), close_btn]
            .align_y(Alignment::Center),
    )
    .padding(Padding::from([t::S2, t::S4]))
    .width(Length::Fill)
    .style(style::fill(t::MOUNT));

    let base = container(column![body, kit::hairline(), bar]).width(Length::Fill).height(Length::Fill).style(style::fill(t::GROUND));
    let mut layers: Vec<Element<'_, Message>> = vec![base.into()];
    if let Some(ask) = p.confirm {
        layers.push(confirm_view(app, ask));
    }
    if let Some(f) = p.face_change {
        layers.push(face_model_view(app, f));
    }
    stack(layers).width(Length::Fill).height(Length::Fill).into()
}

fn culling(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    section(
        "Culling",
        None,
        vec![
            setting(
                "Advance after marking",
                about("In the viewer, P or M moves on to the next photo once it marks one. Clearing a mark stays put."),
                row![keycap("Ctrl+Shift+A"), switch(p, Control::Advance, app.settings.viewer_advance, true)]
                    .spacing(t::S3)
                    .align_y(Alignment::Center)
                    .into(),
            ),
            setting(
                "Show pruned photos",
                about("Pruned photos stay in All and Marked, crossed out. Off, they leave as you prune them; the Pruned filter shows them either way."),
                switch(p, Control::ShowPruned, app.filter.show_pruned, true),
            ),
        ],
    )
}

fn grid(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let size = slider(MIN_SIZE..=MAX_SIZE, app.thumb_size, |v| m(Msg::ThumbSize(v)))
        .step(super::ZOOM_STEP)
        .on_release(m(Msg::ThumbSizeDone))
        .width(150)
        .style(style::zoom_slider(true));
    let size = row![
        icons::view(Glyph::Grid, 12.0, t::INK3),
        focusable(p, Control::ThumbSize, container(size).padding(Padding::from([4.0, 2.0]))),
        icons::view(Glyph::Photos, 14.0, t::INK3),
        text(format!("{} px", app.thumb_size as u32)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK2).width(48).align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    section(
        "Grid",
        None,
        vec![
            setting("Show file names", about("Each photo's name under its thumbnail, as in View ▸ Show file names."), switch(p, Control::FileNames, app.show_names, true)),
            setting(
                "Stack bursts",
                about("Each burst as one photo with its count, as in View ▸ Stack all bursts. G opens or stacks the selected one."),
                row![keycap("Shift+G"), switch(p, Control::StackBursts, app.stacking.all(), true)].spacing(t::S3).align_y(Alignment::Center).into(),
            ),
            setting("Thumbnail size", about("Also on the filter bar, or Ctrl+scroll over the grid."), size.into()),
        ],
    )
}

fn viewer(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let s = &app.settings;
    let size_note = if s.viewer_maximized {
        "The viewer opens maximised, as you last left it.".to_string()
    } else if remembered_viewer_size(s) {
        format!("The viewer opens at the size you last left it, {} × {}.", s.viewer_width.round(), s.viewer_height.round())
    } else {
        "The viewer opens at a share of the screen. Resize it and it keeps your size from then on.".to_string()
    };
    let reset: Element<'_, Message> = if remembered_viewer_size(s) {
        focusable(p, Control::ViewerSize, kit::btn("Reset", style::secondary(), Some(m(Msg::Press(Control::ViewerSize))))).into()
    } else {
        Space::new().into()
    };
    section(
        "Viewer",
        None,
        vec![
            setting(
                "Show the filmstrip",
                about("The neighbouring photos in a strip along the bottom of the viewer."),
                row![keycap("T"), switch(p, Control::Filmstrip, s.viewer_filmstrip, true)].spacing(t::S3).align_y(Alignment::Center).into(),
            ),
            setting(
                "Show a burst's frames first",
                about("Arriving at a burst, the viewer shows all its frames side by side; click one to look closer. Off, it steps through them one at a time, and B still shows them all."),
                switch(p, Control::BurstSheet, s.viewer_burst_sheet, true),
            ),
            setting("Window size", about(size_note), reset),
        ],
    )
}

fn folders(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let s = &app.settings;
    let edit_copy = (!s.edit_copy_folder.is_empty()).then(|| PathBuf::from(&s.edit_copy_folder));
    let change = |c: Control, set: bool| -> Element<'_, Message> {
        focusable(p, c, kit::btn(if set { "Change…" } else { "Choose…" }, style::secondary(), Some(m(Msg::Press(c))))).into()
    };
    let shoot = app.folder.as_deref().map(util::file_name).unwrap_or_else(|| "the shoot".into());
    section(
        "Folders",
        None,
        vec![
            setting(
                "Library",
                path_about(app.library.as_deref(), "Where Export files photos by date, and what the calendar shows. Not chosen yet."),
                change(Control::Library, app.library.is_some()),
            ),
            setting(
                "Copies for editing",
                path_about(edit_copy.as_deref(), "Where Copy marked photos for editing puts them. Not chosen yet: it asks the first time."),
                change(Control::EditCopyFolder, edit_copy.is_some()),
            ),
            setting(
                "Put the copies in a subfolder named after the shoot",
                about(format!("Each shoot's RAWs stay together, in a folder named after the working folder (now “{shoot}”).")),
                switch(p, Control::EditCopySubfolder, s.edit_copy_subfolder, true),
            ),
        ],
    )
}

fn desktop(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let forced = settings::global_menu_forced_off();
    let wanted = app.settings.global_menu_wanted(forced);
    let (tone, note): (Color, String) = if forced {
        (t::INK2, "Off for now: PP_NO_GLOBAL_MENU is set in the environment.".into())
    } else if wanted != p.started_with_global_menu {
        let what = if wanted { "The menus move to the panel" } else { "The menus come back into the window" };
        (t::WARN, format!("{what} the next time you start Photo Pipeline."))
    } else if !wanted {
        (t::INK2, "The menus stay in the window.".into())
    } else if app.global_menu_on() {
        (t::INK2, "The menus are in the panel now.".into())
    } else if app.global_menu.is_some() {
        (t::INK2, "There's no Global Menu in the panel, so the menus stay in the window.".into())
    } else {
        (t::INK2, "The desktop's menu service isn't running, so the menus stay in the window.".into())
    };
    let status = row![icons::view(Glyph::Info, 13.0, tone), text(note).size(t::T_SMALL).color(tone)]
        .spacing(6)
        .align_y(Alignment::Center);
    let about = column![
        text("When the Plasma panel has a Global Menu, the menus show there instead of in the window.").size(t::T_SMALL).color(t::INK3),
        status,
    ]
    .spacing(t::S1);
    section(
        "Desktop",
        None,
        vec![setting("Use the KDE global menu", about.into(), switch(p, Control::GlobalMenu, wanted, !forced))],
    )
}

fn storage_section(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    section(
        "Storage",
        Some("Caches only make things faster. Clearing one is safe: it is made again as you work, more slowly the first time."),
        vec![
            cache_row(p, Cache::Thumbs, "Thumbnails", "Small copies of your photos for the grid and the filmstrip. Shared with the Python app."),
            cache_row(p, Cache::Library, "Library index", "The library's folders and capture times, so the calendar opens at once."),
            cache_row(p, Cache::CaptureTimes, "Capture times", "When each photo in a working folder was taken, so folders open faster."),
            cache_row(
                p,
                Cache::Quality,
                "Photo analysis",
                "What analysing found in each folder (sharpness, exposure, fingerprints), so its hints show at once and analysing again reads only new photos.",
            ),
            search_index_row(app),
            heads_row(app),
            judge_rows(app),
            models_row(app),
            unsaved_row(app),
            corrections_switch(app),
            corrections_row(app),
        ],
    )
}

/// The value column of a storage row: a size, and a quieter line.
fn size_column<'a>(value: String, sub: Option<String>) -> Element<'a, Message> {
    let mut size = column![text(value).size(t::T_BODY).font(paint::MEDIUM).color(t::INK)].align_x(Alignment::End).width(96);
    if let Some(s) = sub {
        size = size.push(text(s).size(t::T_SMALL).color(t::INK3));
    }
    size.into()
}

fn search_index_row(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let st = &app.search.status;
    let (value, sub) = match p.search_storage() {
        None => ("…".to_string(), Some("counting".to_string())),
        Some(s) if s.index_bytes == 0 && st.indexed_shots == 0 => ("Empty".to_string(), Some("not indexed yet".to_string())),
        Some(s) => (util::fmt_bytes(s.index_bytes), Some(util::plural(st.indexed_shots as u64, "photo"))),
    };
    let rebuild = focusable(p, Control::SearchRebuild, kit::btn("Rebuild…", style::ghost(), search_action_ready(app, Control::SearchRebuild).then(|| m(Msg::Press(Control::SearchRebuild)))));
    let clear = focusable(p, Control::SearchClear, kit::btn("Clear…", style::secondary(), search_action_ready(app, Control::SearchClear).then(|| m(Msg::Press(Control::SearchClear)))));
    let path = crate::search::index::cache_dir().map(|d| tilde(&d)).unwrap_or_default();
    let about = column![
        text("What each photo looks like to search, so a search reads no photos.").size(t::T_SMALL).color(t::INK3),
        text(path).size(t::T_SMALL).color(t::INK4)
    ]
    .spacing(2);
    setting("Search index", about.into(), row![size_column(value, sub), rebuild, clear].spacing(t::S1).align_y(Alignment::Center).into())
}

/// The fursuit heads found: their boxes, vectors and tiles, beside the
/// search index.  Not a cache (it holds what the names point at), so it is
/// forgotten from Preferences ▸ Fursuits, which asks first.
fn heads_row(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let h = &app.search.status.heads;
    let (value, sub) = match p.search_storage() {
        None => ("…".to_string(), Some("counting".to_string())),
        Some(s) if s.heads_bytes + s.judged_bytes == 0 => ("Empty".to_string(), Some("nothing saved yet".to_string())),
        Some(s) => (util::fmt_bytes(s.heads_bytes + s.judged_bytes), Some(util::plural(h.found as u64, "head"))),
    };
    let about = column![
        text("The fursuit heads found in your photos, and the close calls the judge checked, so the Fursuits tab and its reviews read no photos. Forget them in Preferences ▸ Fursuits.").size(t::T_SMALL).color(t::INK3),
        text(crate::search::index::cache_dir().map(|d| tilde(&d)).unwrap_or_default()).size(t::T_SMALL).color(t::INK4)
    ]
    .spacing(2);
    // Lines up with the rows' buttons (it has none: it isn't a cache).
    let gap = Space::new().width(clear_btn_width() + t::S1 + clear_btn_width());
    setting("Fursuit heads", about.into(), row![size_column(value, sub), gap].spacing(t::S1).align_y(Alignment::Center).into())
}

fn models_row(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let (value, sub) = match p.search_storage() {
        None => ("…".to_string(), Some("counting".to_string())),
        Some(s) if s.models_bytes == 0 => ("None".to_string(), Some("nothing downloaded".to_string())),
        Some(s) => (util::fmt_bytes(s.models_bytes), Some("search, faces, heads".to_string())),
    };
    let open = focusable(p, Control::ModelsOpen, kit::btn("Open", style::ghost(), models_exist(p).then(|| m(Msg::Press(Control::ModelsOpen)))));
    let delete = focusable(p, Control::ModelsDelete, kit::btn("Delete…", style::secondary(), search_action_ready(app, Control::ModelsDelete).then(|| m(Msg::Press(Control::ModelsDelete)))));
    let path = crate::ml::models::models_dir().map(|d| tilde(&d)).unwrap_or_default();
    let about = column![
        text("The models search, faces and fursuit heads use. Deleted, each downloads again, after asking, when it's next needed.").size(t::T_SMALL).color(t::INK3),
        text(path).size(t::T_SMALL).color(t::INK4)
    ]
    .spacing(2);
    setting("Downloaded models", about.into(), row![size_column(value, sub), open, delete].spacing(t::S1).align_y(Alignment::Center).into())
}

// ── search ──────────────────────────────────────────────────────────────────

/// At the default, judged where the knob sits (the values span a
/// thousandfold, so a fixed tolerance on the value would be meaningless).
fn at_default(v: f32) -> bool {
    (closeness_to_track(v) - 0.5).abs() < 0.005
}

/// One arrow key's step along the slider's track: ten from the middle to
/// either end.
const TRACK_STEP: f32 = 0.05;

/// Where a closeness sits on the slider's track, 0 (closer, the left end)
/// to 1 (looser).  The closeness is a floor on the model's match
/// probability and spans a thousandfold, so each half of the track is
/// logarithmic: the left half covers `CLOSENESS_DEFAULT..=CLOSENESS_MAX`,
/// the right `CLOSENESS_MIN..=CLOSENESS_DEFAULT`, and the default sits in
/// the middle whatever the range.
pub(super) fn closeness_to_track(v: f32) -> f32 {
    let v = if v.is_finite() { v.clamp(CLOSENESS_MIN, CLOSENESS_MAX) } else { CLOSENESS_DEFAULT };
    let (min, def, max, l) = (CLOSENESS_MIN.ln(), CLOSENESS_DEFAULT.ln(), CLOSENESS_MAX.ln(), v.ln());
    if v >= CLOSENESS_DEFAULT { 0.5 * (max - l) / (max - def) } else { 0.5 + 0.5 * (def - l) / (def - min) }
}

/// The closeness at a place on the track (the inverse of
/// `closeness_to_track`); the middle is exactly the default.
pub(super) fn track_to_closeness(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    let (min, def, max) = (CLOSENESS_MIN.ln(), CLOSENESS_DEFAULT.ln(), CLOSENESS_MAX.ln());
    if (p - 0.5).abs() < 1e-6 {
        CLOSENESS_DEFAULT
    } else if p == 0.0 {
        CLOSENESS_MAX
    } else if p == 1.0 {
        CLOSENESS_MIN
    } else if p < 0.5 {
        (max - (max - def) * (p / 0.5)).exp()
    } else {
        (def - (def - min) * ((p - 0.5) / 0.5)).exp()
    }
}

/// What the closeness means, in words.
pub fn closeness_words(v: f32) -> &'static str {
    if at_default(v) {
        "The default: good matches, right for most searches."
    } else if v > CLOSENESS_DEFAULT {
        "Closer than the default: fewer results, all of them close."
    } else {
        "Looser than the default: more results, some only a little alike."
    }
}

fn search_section(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let s = &app.settings;
    let st = &app.search.status;
    let shots = st.library_shots;
    let gpu = s.search_device == Device::Gpu;

    // Run models on.
    let mut about_device = column![].spacing(t::S1);
    if gpu {
        about_device = about_device.push(
            text("Indexing runs on the graphics card, about 6 times faster than the processor. If the card fails, indexing stops and says why; nothing moves to the processor by itself.")
                .size(t::T_SMALL)
                .color(t::INK3),
        );
        let name = st.device.name.clone().unwrap_or_else(|| "the graphics card".into());
        let (tone, line) = match (&st.stopped, &st.device.self_check) {
            (Some(Stopped::Card(CardFailure::NoCard)), _) => (t::WARN, "No graphics card Photo Pipeline can use was found.".to_string()),
            (_, SelfCheck::Failed { .. }) => (t::WARN, "Last check: the card gave different results from the processor, so indexing is stopped until it passes.".to_string()),
            (_, SelfCheck::Passed { .. }) => (t::INK2, format!("Last check: {name} gives the same results as the processor.")),
            (_, SelfCheck::NotRun) => (t::INK2, "Not checked yet: that happens before the first index.".to_string()),
        };
        let check = focusable(
            p,
            Control::CheckCard,
            // No side padding: wrapped onto its own line, the link sits
            // flush with the words above it.
            iced::widget::button(text("Check again").size(t::T_SMALL).font(paint::MEDIUM))
                .padding(Padding::from([1.0, 0.0]))
                .style(style::link().style())
                .on_press(m(Msg::Press(Control::CheckCard))),
        );
        about_device = about_device.push(row![text(line).size(t::T_SMALL).color(tone), check].spacing(t::S2).align_y(Alignment::Center).wrap());
    } else {
        let all = estimate::describe_seconds(estimate::seconds(shots, Device::Cpu));
        let day = estimate::describe_seconds(estimate::seconds(5_000, Device::Cpu));
        about_device = about_device.push(
            text(format!(
                "Indexing this library on the processor: {all}. Each new day of 5,000 photos: {day}. Fursuit heads: {}, {} an event day, and each scan asks first.",
                super::fursuits::describe_cost(super::fursuits::library_photos(&app.search.status.heads, shots), Device::Cpu).replacen(" on the processor", "", 1),
                estimate::describe_seconds(super::fursuits::head_seconds(super::fursuits::EVENT_DAY_PHOTOS, Device::Cpu)),
            ))
                .size(t::T_SMALL)
                .color(t::INK3),
        );
    }
    let segment = |d: Device| -> Element<'_, Message> { focusable(p, Control::SearchDevice(d), kit::segment(d.label(), s.search_device == d, None, m(Msg::Press(Control::SearchDevice(d))))) };
    let device_control = kit::segmented(vec![segment(Device::Gpu), segment(Device::Cpu)]);

    // How close matches must be: closer to the left, looser to the right,
    // the default in the middle.
    let v = s.closeness();
    let slider = slider(0.0..=1.0_f32, closeness_to_track(v), |x| m(Msg::Closeness(x)))
        .step(0.01_f32)
        .on_release(m(Msg::ClosenessDone))
        .width(160)
        .style(style::zoom_slider(true));
    let ends = row![text("Closer").size(t::T_SMALL).color(t::INK3), Space::new().width(Length::Fill), text("Looser").size(t::T_SMALL).color(t::INK3)].width(160);
    let reset_on = !at_default(v);
    let reset = kit::tip(
        focusable(
            p,
            Control::ClosenessReset,
            iced::widget::button(container(icons::view(Glyph::Undo, 14.0, if reset_on { t::INK2 } else { t::INK4 })).center(Length::Fill))
                .width(24)
                .height(24)
                .padding(0)
                .style(style::ghost().radius(4.0).style())
                .on_press_maybe(reset_on.then(|| m(Msg::Press(Control::ClosenessReset)))),
        ),
        "Reset to the default (good matches)",
        true,
    );
    let closeness = row![column![focusable(p, Control::Closeness, container(slider).padding(Padding::from([4.0, 2.0]))), ends].spacing(0), reset]
        .spacing(t::S1)
        .align_y(Alignment::Center);

    let recent = s.search_recent.len();
    let forget = focusable(p, Control::ForgetSearches, kit::btn("Forget them", style::secondary(), (recent > 0).then(|| m(Msg::Press(Control::ForgetSearches)))));
    let model = crate::ml::models::search_models().first().map(|mi| {
        format!(
            "{}, from {}. {}, {}. The same for everyone, so results don't depend on a choice here.",
            mi.name,
            mi.publisher,
            estimate::describe_bytes(mi.bytes()).trim_start_matches("about "),
            mi.licence
        )
    });
    let search = section(
        "Search",
        Some("Find photos in the library by what they show. Everything stays on this computer."),
        vec![
            setting("Run models on", about_device.into(), device_control),
            setting("How close matches must be", about(format!("How alike a photo must be to what you typed to come up in a search. {}", closeness_words(v))), closeness.into()),
            setting(
                "Index new exports",
                about("After each export, its photos are added to search in the background: a line in the status bar, nothing to answer."),
                switch(p, Control::IndexExports, s.search_index_exports, true),
            ),
            setting("Search model", about(model.unwrap_or_default()), kit::caption("Fixed").into()),
            setting(
                "Recent searches",
                about(if recent == 0 { "The last 10 searches show when the search field is empty. None are kept now.".to_string() } else { format!("The last 10 searches show when the search field is empty. {} kept now.", recent) }),
                forget,
            ),
        ],
    );
    container(search).id(SEARCH_SECTION).width(Length::Fill).into()
}

/// People, with Search: whether faces are found, with which model, the
/// smallest group to show, and forgetting what was found.
fn people_section(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let s = &app.settings;
    let f = &app.search.status.faces;
    let on = s.find_faces;
    let mut find = column![about(format!(
        "Looks for faces while indexing. Groups are never named for you, and groups of fewer than {} photos stay out of sight. A burst counts as one photo.",
        s.people_min_photos()
    ))]
    .spacing(t::S2);
    if p.faces_offer {
        let n = util::plural(f.found as u64, "face");
        let names = match f.named {
            0 => String::new(),
            1 => ", and the 1 name you gave".to_string(),
            k => format!(", and the {k} names you gave"),
        };
        let offer = row![
            text(format!("No new faces will be looked for. Forget the {n} already found{names}?")).size(t::T_SMALL).color(t::INK2).width(Length::Fill),
            focusable(p, Control::KeepFaces, kit::btn("Keep them", style::ghost(), Some(m(Msg::Press(Control::KeepFaces))))),
            focusable(p, Control::ForgetFaces, kit::btn("Forget them", style::danger(), Some(m(Msg::Press(Control::ForgetFaces))))),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center);
        find = find.push(container(offer).padding(Padding::from([t::S2, t::S3])).style(style::group()));
    }
    let model = super::subjects::face_model(app);
    let mi = crate::ml::models::info(model);
    let model_about = if f.model.is_some() {
        format!(
            "{}: {}, {}. Changing it recognises every face again; names and the photos you confirmed are kept.",
            short_name(mi.name),
            super::search_setup::size(mi.kept_bytes()),
            super::search_setup::capital(mi.licence)
        )
    } else {
        format!("{} unless you pick another when faces are first looked for. {}", mi.name, mi.what)
    };
    let change = focusable(p, Control::FaceModel, kit::btn("Change…", style::secondary(), (on && f.model.is_some()).then(|| m(Msg::Press(Control::FaceModel)))));
    let sizes: Vec<Element<'_, Message>> = crate::settings::PEOPLE_MIN_PHOTOS_CHOICES
        .iter()
        .map(|&n| focusable(p, Control::SmallestGroup(n), kit::segment_compact(format!("{n} photos"), s.people_min_photos() == n, m(Msg::Press(Control::SmallestGroup(n))))).into())
        .collect();
    let kept = if f.found == 0 && f.named == 0 {
        "Nothing found yet.".to_string()
    } else {
        format!("{} found, {} named.", util::plural(f.found as u64, "face"), util::thousands(f.named as u64))
    };
    let forget = focusable(p, Control::ForgetPeople, kit::btn("Forget all…", style::secondary(), people_to_forget(app).then(|| m(Msg::Press(Control::ForgetPeople)))));
    let people = section(
        "People",
        Some("Faces in your photos, grouped so you can name the people you want to find. Fursuits have a section of their own."),
        vec![
            setting("Find people's faces", find.into(), switch(p, Control::FindFaces, on, true)),
            setting("Face model", about(model_about), change.into()),
            setting("Smallest group to show", about("Smaller groups wait under Smaller groups on the People tab, so an event's strangers stay out of the way."), kit::segmented(sizes)),
            setting(
                "People found",
                about(format!("{kept} Kept on this computer, readable only by you, and never written into your photos. Not a cache: it asks first.")),
                forget,
            ),
            setting(
                "Show People and fursuits in the sidebar",
                about("Hides only the sidebar button. Ctrl+4 and View ▸ People and fursuits still open the page."),
                switch(p, Control::RailSubjects, s.rail_subjects, true),
            ),
        ],
    );
    container(people).id("pref-section-people").width(Length::Fill).into()
}

/// Fursuits, after People: whether heads are looked for, what that costs on
/// the device chosen (on the processor every scan asks first), and the
/// photos with fursuits not looked at yet.  Kept apart from people: its
/// own switch, never one setting for both.
fn fursuits_section(app: &App) -> Element<'_, Message> {
    use super::fursuits;
    let p = &app.prefs;
    let s = &app.settings;
    let h = &app.search.status.heads;
    let device = s.search_device;
    let shots = app.search.status.library_shots.max(app.search.status.indexed_shots);
    let all = fursuits::library_photos(h, shots);
    let day = estimate::describe_seconds(fursuits::head_seconds(fursuits::EVENT_DAY_PHOTOS, device));
    let cost = match device {
        Device::Gpu => format!("about {} for this library the first time, then {day} a new event day", fursuits::describe_cost(all, device).trim_start_matches("about ")),
        Device::Cpu => format!("{} for this library, {day} an event day; on the processor each scan asks first, with its cost", fursuits::describe_cost(all, device)),
    };
    let mut rows = vec![setting(
        "Find fursuit heads",
        about(format!("Looks for fursuit heads while indexing, after search and faces, only on photos with fursuits: {cost}. Nothing is named for you.")),
        switch(p, Control::FindHeads, s.find_heads, true),
    )];
    if heads_to_update(app) {
        let n = h.vectors_missing;
        let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
        let go = focusable(p, Control::UpdateHeads, kit::btn("Update them", style::secondary(), Some(m(Msg::Press(Control::UpdateHeads)))));
        rows.push(setting(
            "Heads to update",
            about(format!(
                "{} with fursuit heads {} found before fursuits were told apart by the search model. Updating reads their previews again: {} from the library{}, {}. Until then they aren't grouped or suggested; your names and answers are kept.",
                util::plural(n as u64, "photo"),
                if n == 1 { "was" } else { "were" },
                estimate::describe_bytes(fursuits::vector_bytes_read(n)),
                if on_share { " on the share" } else { "" },
                fursuits::describe_vector_cost(n, device),
            )),
            go,
        ));
    }
    rows.push(grouping_setting(app));
    rows.push(judge_setting(app));
    if heads_catch_up(app) {
        let n = h.unscanned();
        let go = focusable(p, Control::FindHeadsNow, kit::btn("Find their heads…", style::secondary(), Some(m(Msg::Press(Control::FindHeadsNow)))));
        rows.push(setting(
            "Photos not looked at yet",
            about(format!("{} Looking at them takes {}.", not_looked_at(n, h.skipped), fursuits::describe_cost(n, device))),
            go,
        ));
    }
    let found = if h.found == 0 { "Nothing found yet.".to_string() } else { format!("{} found, {} named.", util::plural(h.found as u64, "head"), util::thousands(h.named as u64)) };
    let forget = focusable(p, Control::ForgetFursuits, kit::btn("Forget all…", style::secondary(), fursuits_to_forget(app).then(|| m(Msg::Press(Control::ForgetFursuits)))));
    rows.push(setting(
        "Fursuits found",
        about(format!("{found} Kept on this computer, readable only by you, and never written into your photos. Not a cache: it asks first.")),
        forget,
    ));
    let sec = section("Fursuits", Some("Fursuit heads in your photos, grouped so you can name the fursuits you want to find. Never mixed with people."), rows);
    container(sec).id("pref-section-fursuits").width(Length::Fill).into()
}

/// Storage ▸ *Keep a record of my corrections*: the switch, for people
/// and fursuits alike, with what it keeps.  The record is the user's, not
/// a cache, so it sits with *Marks waiting to be saved*.
fn corrections_switch(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    setting(
        "Keep a record of my corrections, to improve matching later",
        about(
            "Each merge, split, review answer and label change you make to people and fursuits, with the faces or heads it was about and how it was suggested. Stays on this computer, readable only by you; never written into your photos. Clear and Rebuild keep it. Off, nothing new is added, but taking back a correction already recorded still is.",
        ),
        switch(p, Control::KeepCorrections, app.settings.keep_corrections, true),
    )
}

/// Storage ▸ *Record of corrections*: its size and where it is, with
/// *Export…* (a copy without names) and *Delete the record…*.
fn corrections_row(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let (bytes, entries) = p.survey.as_ref().map(|s| s.decisions).unwrap_or_default();
    let kept = corrections_kept(p);
    let (value, sub) = match (p.survey.is_some(), entries) {
        (false, _) => ("…".to_string(), None),
        (true, 0) => ("None".to_string(), None),
        (true, n) => (util::fmt_bytes(bytes), Some(util::plural(n as u64, "correction"))),
    };
    let mut words = column![].spacing(3);
    words = words.push(about(match &p.exported {
        Some(done) => done.clone(),
        None if kept => "Export copies it into a folder you choose, with a README describing its format. Names are replaced by codes such as fursuit-12 in the copy.".to_string(),
        None => "Nothing recorded yet.".to_string(),
    }));
    if bytes > crate::model::decisions::LARGE_BYTES {
        words = words.push(text("That's a lot: export it, then delete it to start a new one.").size(t::T_SMALL).color(t::INK2));
    }
    if let Some(path) = app.search.link.decisions().path() {
        words = words.push(text(util::elide_path(Path::new(&tilde(path)), 48)).size(t::T_SMALL).color(t::INK4));
    }
    let export = focusable(p, Control::ExportCorrections, kit::btn("Export…", style::ghost(), kept.then(|| m(Msg::Press(Control::ExportCorrections)))));
    let delete = focusable(p, Control::DeleteCorrections, kit::btn("Delete the record…", style::secondary(), kept.then(|| m(Msg::Press(Control::DeleteCorrections)))));
    setting("Record of corrections", words.into(), row![size_column(value, sub), export, delete].spacing(t::S1).align_y(Alignment::Center).into())
}

/// The judge's choice wants its download: chosen, not in place, not
/// coming down now.
fn judge_wants_download(app: &App) -> bool {
    let c = app.settings.fursuit_judge;
    c.model().is_some() && !app.search.status.judge.downloaded.contains(&c) && !judge_downloading(app)
}

/// Preferences ▸ Fursuits ▸ *Close-call judge*: what it is for, the
/// choices (each with its size and speed in plain words), and a line on
/// where it stands.  A choice list, so another judge is another row.
fn judge_setting(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let chosen = app.settings.fursuit_judge;
    let mut list = column![].spacing(2).padding(2);
    for c in JudgeChoice::ALL {
        list = list.push(focusable(p, Control::Judge(c), judge_option(app, c, c == chosen)));
    }
    let mut col = column![
        text("Close-call judge").size(t::T_BODY).font(paint::MEDIUM).color(t::INK),
        about("When Find more or the \u{201c}Same fursuit?\u{201d} review has photos too close to call, a judge can look at the two heads side by side and offer those it thinks are the same fursuit, for your Y or N. Those it thinks are different are left out, and counted, so you can still look."),
        Space::new().height(t::S1),
        container(list).width(Length::Fill).style(style::group()),
    ]
    .spacing(3);
    if let Some(note) = judge_note(app) {
        col = col.push(Space::new().height(t::S1));
        col = col.push(note);
    }
    // Another judge's files stay until deleted: say so, with their size.
    for c in judges().filter(|&c| c != chosen) {
        if let Some(b) = judge_bytes(p, c).filter(|&b| b > 0) {
            col = col.push(
                row![
                    icons::view(Glyph::Info, 13.0, t::INK3),
                    text(format!("{} stays downloaded ({}) and isn't loaded; Storage deletes it.", c.label(), util::fmt_bytes(b))).size(t::T_SMALL).color(t::INK3)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
    }
    container(col).padding(Padding::from([t::S3, t::S4])).width(Length::Fill).into()
}

/// One judge in the list: a radio, its name, its size (or that it is
/// downloaded, or coming down), and what it costs, in words.
fn judge_option(app: &App, c: JudgeChoice, on: bool) -> Element<'_, Message> {
    let j = &app.search.status.judge;
    let mut top = row![text(c.label()).size(t::T_BODY).font(paint::MEDIUM).color(t::INK)].spacing(8).align_y(Alignment::Center);
    let mut lines = column![].spacing(2);
    match c.model().map(crate::ml::models::info) {
        None => {
            top = top.push(text("The default").size(t::T_SMALL).color(t::INK3));
            lines = lines.push(text("Find more and the review offer photos by how alike the heads look, as before.").size(t::T_SMALL).color(t::INK2));
        }
        Some(mi) => {
            let coming = app.search.downloads.iter().find(|d| d.model == mi.id && !d.state.is_final());
            let right = match coming {
                Some(d) if d.state == crate::ml::download::DownloadState::Downloading => {
                    super::download_speed::with_speed(super::search_setup::progress_bytes(d.done, d.total), app.search.speed_now())
                }
                Some(d) => super::search_setup::progress_bytes(d.done, d.total),
                None if j.downloaded.contains(&c) => "Downloaded".to_string(),
                None => format!("{} to download", super::search_setup::size(mi.bytes())),
            };
            top = top.push(Space::new().width(Length::Fill));
            top = top.push(text(right).size(t::T_SMALL).color(t::INK2));
            lines = lines.push(text(super::fursuit_judge::choice_words(c).unwrap_or_default()).size(t::T_SMALL).color(t::INK2));
            lines = lines.push(text(format!("{}. Licence: {}, {}. From {}.", mi.name, mi.licence, mi.licence_plain, mi.publisher)).size(t::T_SMALL).color(t::INK3));
        }
    }
    let dot = container(if on {
        Element::from(container(Space::new()).width(8).height(8).style(|_| container::Style { background: Some(Background::Color(t::SIGNAL)), border: Border { radius: 4.0.into(), ..Border::default() }, ..Default::default() }))
    } else {
        Space::new().into()
    })
    .width(16)
    .height(16)
    .center(16)
    .style(move |_| container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(if on { t::SIGNAL } else { t::LINE_HI }, 1.0, 8.0), ..Default::default() });
    let body = column![top, lines].spacing(2).width(Length::Fill);
    iced::widget::button(row![container(dot).padding(Padding { top: 1.0, ..Padding::ZERO }), body].spacing(t::S3))
        .width(Length::Fill)
        .padding(Padding::from([7.0, 12.0]))
        .style(move |_t, status| {
            let hot = matches!(status, iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed);
            iced::widget::button::Style { background: (on || hot).then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..iced::widget::button::Style::default() }
        })
        .on_press(m(Msg::Press(Control::Judge(c))))
        .into()
}

/// Where the chosen judge stands, under the list: not downloaded (with
/// *Download…*), coming down, waiting on the processor's choice, on the
/// card now, or stopped (said as an error, never with the processor).
fn judge_note(app: &App) -> Option<Element<'_, Message>> {
    let p = &app.prefs;
    let c = app.settings.fursuit_judge;
    let j = &app.search.status.judge;
    c.model()?;
    let line = |g: Glyph, tone: Color, s: String| row![icons::view(g, 13.0, tone), text(s).size(t::T_SMALL).color(tone).width(Length::Fill)].spacing(6).align_y(Alignment::Center);
    let note: Element<'_, Message> = match &j.state {
        crate::ml::judge::JudgeState::Failed(f) => row![
            line(Glyph::GraphicsCard, t::WARN, format!("Checking close calls stopped: {} {}", lowercase_first(f.title().trim_start_matches("Indexing stopped: ")), f.detail().trim())),
            focusable(p, Control::CheckCard, kit::btn(super::search_setup::retry_label(f), style::secondary(), Some(m(Msg::Press(Control::CheckCard))))),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)
        .into(),
        crate::ml::judge::JudgeState::Broken(why) => line(Glyph::Warning, t::WARN, format!("The close-call judge couldn't run. {why}")).into(),
        _ if judge_downloading(app) => {
            // "…, about 3 minutes left" or "…, waiting for the network".
            let how = match app.search.speed_now() {
                Some(r @ (super::download_speed::Reading::Rate { .. } | super::download_speed::Reading::Stalled)) => {
                    format!(", {}", super::download_speed::note(r, super::search_setup::remaining(&app.search.downloads)))
                }
                _ => String::new(),
            };
            line(Glyph::Info, t::INK2, format!("{} is downloading{how}; close calls are checked once it's in place.", c.label())).into()
        }
        _ if !j.downloaded.contains(&c) => row![
            line(Glyph::Info, t::INK2, format!("{} isn't downloaded, so close calls are matched by look alone for now.", c.label())),
            focusable(p, Control::JudgeDownload, kit::btn("Download…", style::secondary(), Some(m(Msg::Press(Control::JudgeDownload))))),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)
        .into(),
        _ if app.settings.search_device == Device::Cpu => {
            line(Glyph::Info, t::INK2, "The processor is chosen in Search, and the judge runs on the graphics card only, so close calls are matched by look alone until the card is chosen again.".into()).into()
        }
        _ if j.loaded => line(Glyph::GraphicsCard, t::INK3, format!("{} is on the graphics card now; it lets the card go after two quiet minutes.", c.label())).into(),
        _ => return None,
    };
    Some(note)
}

fn lowercase_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Storage ▸ one row per close-call judge, in the list's order: its model,
/// apart from the other models, deleted on its own (the chosen one turns
/// the judge off; another's goes alone).
fn judge_rows(app: &App) -> Element<'_, Message> {
    let mut col = column![];
    for (i, c) in judges().enumerate() {
        if i > 0 {
            col = col.push(container(kit::hairline()).padding(Padding { left: t::S4, ..Padding::ZERO }));
        }
        col = col.push(judge_row(app, c));
    }
    col.into()
}

fn judge_row(app: &App, c: JudgeChoice) -> Element<'_, Message> {
    let p = &app.prefs;
    let chosen = app.settings.fursuit_judge == c;
    let (value, sub) = match judge_bytes(p, c) {
        None => ("…".to_string(), Some("counting".to_string())),
        Some(0) => ("None".to_string(), Some("not downloaded".to_string())),
        Some(b) => (util::fmt_bytes(b), Some(if chosen { "the judge chosen" } else { "not chosen now" }.to_string())),
    };
    let exists = judge_exists(p, c);
    let open = focusable(p, Control::JudgeOpen(c), kit::btn("Open", style::ghost(), exists.then(|| m(Msg::Press(Control::JudgeOpen(c))))));
    let delete = focusable(p, Control::JudgeDelete(c), kit::btn("Delete…", style::secondary(), exists.then(|| m(Msg::Press(Control::JudgeDelete(c))))));
    let what = if chosen {
        "The close-call judge chosen in Preferences ▸ Fursuits. Deleted, the judge turns off; choosing it again downloads it again, after asking."
    } else {
        "A close-call judge, not chosen now. Its files stay until deleted; choosing it again uses them as they are."
    };
    let about = column![text(what).size(t::T_SMALL).color(t::INK3), text(judge_dir(c).map(|d| tilde(&d)).unwrap_or_default()).size(t::T_SMALL).color(t::INK4)].spacing(2);
    setting(format!("Close-call judge: {}", c.label()), about.into(), row![size_column(value, sub), open, delete].spacing(t::S1).align_y(Alignment::Center).into())
}

/// A model's name without what follows a comma ("InsightFace").
fn short_name(name: &str) -> &str {
    name.split(',').next().unwrap_or(name).trim()
}

/// *Change the face model*: the models, how long finding the faces again
/// takes, and that names are kept.
fn face_model_view(app: &App, picked: crate::ml::models::ModelId) -> Element<'_, Message> {
    use crate::ml::models;
    let st = &app.search.status;
    let f = &st.faces;
    let now = f.model;
    let device = st.device.chosen;
    // About a photo in four holds a face; never more photos than faces.
    let with_faces = (st.indexed_shots / 4).min(f.found).max(f.found.min(1));
    let secs = estimate::face_model_change_seconds(with_faces, f.found, device, picked);
    let names = match f.named {
        0 => "Names you give later are kept the same way.".to_string(),
        1 => "The 1 name you gave is kept, and matched again from the faces you confirmed.".to_string(),
        n => format!("The {n} names you gave are kept, and matched again from the faces you confirmed."),
    };
    let mut list = column![].spacing(2).padding(2);
    for mi in models::face_models() {
        let on = mi.id == picked;
        let mut top = row![text(mi.name).size(t::T_BODY).font(paint::MEDIUM).color(t::INK)].spacing(8).align_y(Alignment::Center);
        if mi.recommended {
            top = top.push(text("Recommended").size(t::T_SMALL).color(t::SIGNAL));
        }
        if now == Some(mi.id) {
            top = top.push(text("In use now").size(t::T_SMALL).color(t::INK3));
        }
        top = top.push(Space::new().width(Length::Fill));
        top = top.push(text(super::search_setup::size(mi.bytes())).size(t::T_SMALL).color(t::INK2));
        let licence = format!("Licence: {}: {}. From {}, its publisher.", super::search_setup::capital(mi.licence), mi.licence_plain, mi.publisher);
        let what = mi.what.split_whitespace().collect::<Vec<_>>().join(" ");
        let col = column![top, text(what).size(t::T_SMALL).color(t::INK2), text(licence).size(t::T_SMALL).color(if mi.restricted { t::WARN } else { t::INK3 })].spacing(2).width(Length::Fill);
        let dot = container(if on { Element::from(container(Space::new()).width(8).height(8).style(|_| container::Style { background: Some(Background::Color(t::SIGNAL)), border: Border { radius: 4.0.into(), ..Border::default() }, ..Default::default() })) } else { Space::new().into() })
            .width(16)
            .height(16)
            .center(16)
            .style(move |_| container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(if on { t::SIGNAL } else { t::LINE_HI }, 1.0, 8.0), ..Default::default() });
        let id = mi.id;
        list = list.push(
            iced::widget::button(row![dot, col].spacing(t::S3))
                .width(Length::Fill)
                .padding(Padding::from([7.0, 12.0]))
                .style(move |_t, status| {
                    let hot = matches!(status, iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed);
                    iced::widget::button::Style { background: (on || hot).then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..iced::widget::button::Style::default() }
                })
                .on_press(m(Msg::PickFaceModel(id))),
        );
    }
    let same = now == Some(picked);
    let go = if same { "Already in use".to_string() } else { format!("Find faces again with {}", short_name(models::info(picked).name)) };
    let content = column![
        dialogs::header("Change the face model?", None),
        kit::secondary(format!(
            "Every face found is recognised again with the new model: {} {} {}. {names} A model not downloaded yet is fetched first ({}).",
            estimate::describe_seconds(secs),
            if device == Device::Gpu { "with" } else { "on" },
            device.phrase(),
            super::search_setup::size(models::info(picked).bytes())
        )),
        container(list).width(Length::Fill).style(style::group()),
        Space::new().height(t::S1),
        dialogs::footer(
            vec![],
            vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::ChangeFaceModel(false)))).into(), kit::btn(go, style::primary(), (!same).then(|| m(Msg::ChangeFaceModel(true)))).into()],
        ),
    ]
    .spacing(t::S3);
    dialogs::backdrop(dialogs::frame_fit(content.into(), 560.0))
}

fn count_word(c: Cache, n: u64) -> String {
    match c {
        Cache::Thumbs => util::plural(n, "thumbnail"),
        Cache::Library => format!("{} {}", util::thousands(n), if n == 1 { "library" } else { "libraries" }),
        Cache::CaptureTimes | Cache::Quality => util::plural(n, "folder"),
    }
}

fn cache_row<'a>(p: &'a Prefs, c: Cache, label: &'a str, what: &'a str) -> Element<'a, Message> {
    let usage = p.usage(c);
    let (value, sub): (String, Option<(String, Color)>) = if p.clearing == Some(c) {
        ("Clearing…".into(), None)
    } else {
        match usage {
            None => ("…".into(), Some(("counting".into(), t::INK3))),
            Some(u) if !u.exists => ("Empty".into(), Some(("nothing saved yet".into(), t::INK3))),
            Some(u) if u.files == 0 => ("Empty".into(), None),
            Some(u) => (util::fmt_bytes(u.bytes), Some((count_word(c, u.files), t::INK3))),
        }
    };
    // What the last clear did, in place of the count.
    let sub = match &p.last_clear {
        Some((k, r)) if *k == c && r.failed > 0 => Some((format!("{} couldn't be removed", util::plural(r.failed, "file")), t::REJECT)),
        Some((k, r)) if *k == c => Some((format!("cleared {}", util::plural(r.removed, "file")), t::KEEP)),
        _ => sub,
    };
    let mut size = column![text(value).size(t::T_BODY).font(paint::MEDIUM).color(t::INK)].align_x(Alignment::End).width(96);
    if let Some((s, tone)) = sub {
        size = size.push(text(s).size(t::T_SMALL).color(tone));
    }
    let exists = usage.is_some_and(|u| u.exists);
    let open = focusable(p, Control::Open(c), kit::btn("Open", style::ghost(), exists.then(|| m(Msg::Press(Control::Open(c))))));
    let clear = focusable(p, Control::Clear(c), kit::btn("Clear…", style::secondary(), clearable(p, c).then(|| m(Msg::Press(Control::Clear(c))))));
    let path = c.dir().map(|d| tilde(&d)).unwrap_or_default();
    let about = column![text(what).size(t::T_SMALL).color(t::INK3), text(path).size(t::T_SMALL).color(t::INK4)].spacing(2);
    setting(label, about.into(), row![size, open, clear].spacing(t::S1).align_y(Alignment::Center).into())
}

fn unsaved_row(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    let folders = app.net.unsaved_folders();
    let journal = p.survey.as_ref().map(|s| s.journal.clone());
    let n = folders.len();
    let (value, tone) = if n > 0 { (util::plural(n as u64, "folder"), t::WARN) } else { ("None".to_string(), t::INK2) };
    let mut about = column![].spacing(t::S1);
    if n > 0 {
        let mut names: Vec<String> = folders.iter().take(3).map(|f| format!("“{}”", util::file_name(f))).collect();
        if n > 3 {
            names.push(format!("{} more", n - 3));
        }
        let list = match names.len() {
            1 => names[0].clone(),
            k => format!("{} and {}", names[..k - 1].join(", "), names[k - 1]),
        };
        about = about.push(
            text(format!(
                "Marks for {list} couldn't be written to {} yet. They're kept on this computer and saved as soon as {} can be reached.",
                if n == 1 { "its folder" } else { "their folders" },
                if n == 1 { "it" } else { "each" },
            ))
            .size(t::T_SMALL)
            .color(t::INK2),
        );
    } else {
        about = about.push(
            text("When a folder can't be reached, such as a network share that's away, its marks are kept here until they can be saved. Nothing is waiting now.")
                .size(t::T_SMALL)
                .color(t::INK3),
        );
    }
    if let Some(k) = journal.as_ref().map(|j| j.kept).filter(|&k| k > 0) {
        about = about.push(
            text(format!(
                "{} set aside in “kept”, because the folder's own marks had changed since. They stay until you remove them.",
                if k == 1 { "1 older copy is".to_string() } else { format!("{k} older copies are") }
            ))
            .size(t::T_SMALL)
            .color(t::INK2),
        );
    }
    about = about.push(text("These are your marks, not a cache, so they can't be cleared here.").size(t::T_SMALL).color(t::INK3));
    let dir = crate::model::saver::default_journal_dir();
    about = about.push(text(dir.as_deref().map(tilde).unwrap_or_default()).size(t::T_SMALL).color(t::INK4));
    let exists = journal.as_ref().is_some_and(|j| j.exists);
    let size = column![text(value).size(t::T_BODY).font(paint::MEDIUM).color(tone)].align_x(Alignment::End).width(96);
    let open = focusable(p, Control::OpenUnsaved, kit::btn("Open", style::ghost(), exists.then(|| m(Msg::Press(Control::OpenUnsaved)))));
    // Lines up with the caches' Open buttons (no Clear here).
    let gap = Space::new().width(clear_btn_width());
    setting("Marks waiting to be saved", about.into(), row![size, open, gap].spacing(t::S1).align_y(Alignment::Center).into())
}

/// The width kit::btn gives "Clear…", plus the focus ring's margin.
fn clear_btn_width() -> f32 {
    paint::text_width("Clear…", t::T_BODY, paint::MEDIUM).ceil() + 2.0 * t::S4 + 2.0 * RING
}

fn confirm_view(app: &App, ask: Ask) -> Element<'_, Message> {
    let p = &app.prefs;
    let cache = match ask {
        Ask::Cache(c) => c,
        search => return search_confirm_view(app, search),
    };
    let u = p.usage(cache).unwrap_or_default();
    let size = util::fmt_bytes(u.bytes);
    let (title, body, ok) = match cache {
        Cache::Thumbs => (
            "Clear the thumbnail cache?",
            format!(
                "This deletes {size} of thumbnails ({}). They are made again as you browse, which is slower the first time, especially for photos on a network share.\n\nThe Python app uses the same cache, so it makes them again too.",
                util::plural(u.files, "file")
            ),
            "Clear thumbnails",
        ),
        Cache::Library => (
            "Clear the library index?",
            format!(
                "This deletes the saved index of your library ({size}). The next time you start Photo Pipeline, the calendar reads the whole library folder again (a few seconds, longer on a network share) and reads capture times again as you open days."
            ),
            "Clear the index",
        ),
        Cache::Quality => (
            "Clear the photo analysis?",
            format!(
                "This deletes what analysing found in {} ({size}). Hints on screen now stay until a folder is opened again. To see them after that, analyse the folder again: it reads each photo's preview once more, about 0.4 MB a photo, which takes longer on a network share.",
                util::plural(u.files, "folder")
            ),
            "Clear the analysis",
        ),
        Cache::CaptureTimes => (
            "Clear the capture times?",
            format!(
                "This deletes the capture times remembered for {} ({size}). Opening one of those folders again reads each photo's capture time from the file, which takes longer on a network share.",
                util::plural(u.files, "folder")
            ),
            "Clear capture times",
        ),
    };
    let content = column![
        dialogs::header(title, None),
        kit::secondary(body),
        Space::new().height(t::S2),
        dialogs::footer(
            vec![],
            vec![
                kit::btn("Cancel", style::secondary(), Some(m(Msg::ConfirmCancel))).into(),
                kit::btn(ok, style::danger(), Some(m(Msg::ClearConfirmed))).into(),
            ],
        ),
    ]
    .spacing(t::S3);
    dialogs::backdrop(dialogs::frame_fit(content.into(), 460.0))
}

/// The search index and the models: what rebuilding, clearing or deleting
/// costs.
fn search_confirm_view(app: &App, ask: Ask) -> Element<'_, Message> {
    let st = &app.search.status;
    let shots = st.library_shots;
    let d = app.settings.search_device;
    let read = estimate::describe_bytes(estimate::bytes_read(shots));
    let time = format!("{} {} {}", estimate::describe_seconds(estimate::seconds(shots, d)), if d == Device::Gpu { "with" } else { "on" }, d.phrase());
    let models = app.prefs.search_storage().map(|s| s.models_bytes).unwrap_or(0);
    let (title, body, ok, danger) = match ask {
        Ask::SearchRebuild => (
            "Rebuild the search index?",
            format!("Every photo in the library is read again: {read}, {time}. Meanwhile search works on what has been rebuilt so far."),
            "Rebuild",
            false,
        ),
        Ask::SearchClear => (
            "Clear the search index?",
            format!("Search won't work until the library is indexed again ({time}). Your photos are not changed."),
            "Clear the index",
            true,
        ),
        Ask::ForgetPeople => {
            let f = &app.search.status.faces;
            let names = match f.named {
                0 => String::new(),
                1 => " and the 1 name you gave".to_string(),
                n => format!(" and the {n} names you gave"),
            };
            (
                "Forget all people?",
                format!(
                    "This deletes the {} found in your photos{names}, with every answer from the reviews, and takes the people corrections out of your record of corrections. Your photos are not changed. To find faces again later, ask on the People tab.",
                    util::plural(f.found as u64, "face")
                ),
                "Forget all",
                true,
            )
        }
        Ask::ForgetFursuits => {
            let h = &app.search.status.heads;
            let names = match h.named {
                0 => String::new(),
                1 => " and the 1 fursuit name you gave".to_string(),
                n => format!(" and the {n} fursuit names you gave"),
            };
            (
                "Forget all fursuits?",
                format!(
                    "This deletes the {} found in your photos{names}, with every answer from the reviews, and takes the fursuit corrections out of your record of corrections. People are not touched, and your photos are not changed. Heads are found again at the next index unless finding them is turned off.",
                    util::plural(h.found as u64, "fursuit head")
                ),
                "Forget all",
                true,
            )
        }
        Ask::DeleteJudge(c) => {
            let bytes = judge_bytes(&app.prefs, c).unwrap_or(0);
            let freed = if bytes > 0 { util::fmt_bytes(bytes) } else { "Its space".to_string() };
            let body = if app.settings.fursuit_judge == c {
                format!(
                    "{freed} is freed, and the close-call judge turns off: close calls are matched by look alone again. The close calls it checked are remembered. Choosing it again in Preferences ▸ Fursuits downloads it again, after asking."
                )
            } else {
                format!("{freed} is freed. It isn't the judge chosen now, so nothing else changes. Choosing it again in Preferences ▸ Fursuits downloads it again, after asking.")
            };
            (DELETE_TITLES.iter().find(|(k, _)| *k == c).map_or("Delete the close-call judge?", |(_, t)| t), body, "Delete", true)
        }
        Ask::DeleteCorrections => {
            let (bytes, n) = app.prefs.survey.as_ref().map(|s| s.decisions).unwrap_or_default();
            (
                "Delete the record of your corrections?",
                format!(
                    "This deletes the {} recorded ({}): what later work on matching would learn from. Your names, groups and answers are not changed. Export it first if you want a copy.",
                    util::plural(n as u64, "correction"),
                    util::fmt_bytes(bytes)
                ),
                "Delete the record",
                true,
            )
        }
        _ => (
            "Delete the downloaded models?",
            format!(
                "{} is freed. Search keeps working on what is indexed; indexing new photos, or the next index, asks to download them again.",
                if models > 0 { util::fmt_bytes(models) } else { "Its space".to_string() }
            ),
            "Delete",
            true,
        ),
    };
    let content = column![
        dialogs::header(title, None),
        kit::secondary(body),
        Space::new().height(t::S2),
        dialogs::footer(
            vec![],
            vec![
                kit::btn("Cancel", style::secondary(), Some(m(Msg::ConfirmCancel))).into(),
                kit::btn(ok, if danger { style::danger() } else { style::primary() }, Some(m(Msg::ClearConfirmed))).into(),
            ],
        ),
    ]
    .spacing(t::S3);
    dialogs::backdrop(dialogs::frame_fit(content.into(), 460.0))
}

// ── pieces ──────────────────────────────────────────────────────────────────

/// The focus ring's width and gap.
const RING: f32 = 3.0;

/// A section: its heading, an optional line about it, then its settings
/// in one card.
fn section<'a>(title: &'a str, note: Option<&'a str>, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut list = column![];
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            list = list.push(container(kit::hairline()).padding(Padding { left: t::S4, ..Padding::ZERO }));
        }
        list = list.push(r);
    }
    let card = container(list).width(Length::Fill).style(|_| container::Style {
        background: Some(Background::Color(t::MOUNT)),
        border: style::border(t::LINE, 1.0, t::R_PANEL),
        ..Default::default()
    });
    let mut head = column![text(title).size(15).font(paint::SEMIBOLD).color(t::INK)].spacing(2);
    if let Some(n) = note {
        head = head.push(kit::caption(n));
    }
    column![head, card].spacing(t::S2 + 2.0).into()
}

/// One setting: what it is and what it does on the left, its control on
/// the right.
fn setting<'a>(label: impl text::IntoFragment<'a>, about: Element<'a, Message>, control: Element<'a, Message>) -> Element<'a, Message> {
    let words = column![text(label).size(t::T_BODY).font(paint::MEDIUM).color(t::INK), about].spacing(3).width(Length::Fill);
    container(row![words, control].spacing(t::S4).align_y(Alignment::Center))
        .padding(Padding::from([t::S3, t::S4]))
        .width(Length::Fill)
        .into()
}

fn about<'a>(s: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(s).size(t::T_SMALL).color(t::INK3).into()
}

/// A folder setting's line: where it points, or what it is for when unset.
fn path_about<'a>(path: Option<&Path>, unset: &'a str) -> Element<'a, Message> {
    match path {
        Some(p) => row![icons::view(Glyph::Folder, 13.0, t::INK3), text(util::elide_path(Path::new(&tilde(p)), 64)).size(t::T_SMALL).color(t::INK2)]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
        None => about(unset),
    }
}

/// Holds a control's place and rings it while the keyboard is on it.
fn focusable<'a>(p: &Prefs, c: Control, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let on = p.ring && p.focus == Some(c);
    container(content)
        .id(c.id())
        .padding(RING)
        .style(move |_| container::Style {
            border: style::border(if on { t::SIGNAL } else { Color::TRANSPARENT }, 1.5, t::R_CONTROL + RING),
            ..Default::default()
        })
        .into()
}

fn switch<'a>(p: &Prefs, c: Control, on: bool, enabled: bool) -> Element<'a, Message> {
    let tg = toggler(on).size(18).on_toggle_maybe(enabled.then_some(move |_| m(Msg::Press(c)))).style(switch_style);
    focusable(p, c, tg)
}

fn switch_style(_t: &Theme, status: toggler::Status) -> toggler::Style {
    let (on, hovered, disabled) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true, false),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false, true),
    };
    let (track, edge, knob) = match (on, disabled) {
        (_, true) => (t::RAISED, t::LINE_HI, t::INK4),
        (true, false) => (if hovered { t::SIGNAL_HI } else { t::SIGNAL }, Color::TRANSPARENT, t::INK),
        (false, false) => (if hovered { t::RAISED_HI } else { t::RAISED }, if hovered { t::INK3 } else { t::LINE_HI }, if hovered { t::INK } else { t::INK2 }),
    };
    toggler::Style {
        background: Background::Color(track),
        background_border_width: 1.0,
        background_border_color: edge,
        foreground: Background::Color(knob),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.18,
    }
}

/// A key, as the Keyboard shortcuts window shows one.
fn keycap<'a>(k: &'a str) -> Element<'a, Message> {
    container(text(k).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK2))
        .padding(Padding::from([1.0, 6.0]))
        .style(|_| container::Style {
            background: Some(Background::Color(t::RAISED)),
            border: Border { color: t::LINE_HI, width: 1.0, radius: 4.0.into() },
            ..Default::default()
        })
        .into()
}

/// `~/…` for a path in the home folder.
fn tilde(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rel) if rel.as_os_str().is_empty() => "~".into(),
        Some(rel) => format!("~/{}", rel.display()),
        None => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_scrolls_only_as_far_as_needed() {
        // Visible already: no scroll.
        assert_eq!(reveal_offset(600.0, 2000.0, 0.0, 100.0, 30.0), None);
        // Below the fold: scroll until it shows, with a margin.
        assert_eq!(reveal_offset(600.0, 2000.0, 0.0, 700.0, 30.0), Some(178.0));
        // Above: scroll back up to it, never past the top.
        assert_eq!(reveal_offset(600.0, 2000.0, 500.0, 300.0, 30.0), Some(252.0));
        assert_eq!(reveal_offset(600.0, 2000.0, 500.0, 10.0, 30.0), Some(0.0));
        // Near the foot: the rest of the page comes into view.
        assert_eq!(reveal_offset(600.0, 2000.0, 0.0, 1900.0, 30.0), Some(1400.0));
    }

    #[test]
    fn the_closeness_slider_has_the_default_in_the_middle() {
        // The ends and the middle, whatever the provisional range.
        assert_eq!(closeness_to_track(CLOSENESS_MAX), 0.0, "closer is the left end");
        assert_eq!(closeness_to_track(CLOSENESS_DEFAULT), 0.5, "the reset puts the knob in the middle");
        assert_eq!(closeness_to_track(CLOSENESS_MIN), 1.0, "looser is the right end");
        assert_eq!(track_to_closeness(0.0), CLOSENESS_MAX);
        assert_eq!(track_to_closeness(0.5), CLOSENESS_DEFAULT);
        assert_eq!(track_to_closeness(1.0), CLOSENESS_MIN);
        // Each half is logarithmic in its own range (a quarter of the way is
        // the geometric mean), and the two are inverses.
        let rel = |a: f32, b: f32| ((a - b) / b).abs();
        assert!(rel(track_to_closeness(0.25), (CLOSENESS_MAX * CLOSENESS_DEFAULT).sqrt()) < 1e-4);
        assert!(rel(track_to_closeness(0.75), (CLOSENESS_DEFAULT * CLOSENESS_MIN).sqrt()) < 1e-4);
        for i in 0..=40 {
            let p = i as f32 / 40.0;
            assert!((closeness_to_track(track_to_closeness(p)) - p).abs() < 1e-4, "{p}");
            let v = (CLOSENESS_MIN.ln() + (CLOSENESS_MAX.ln() - CLOSENESS_MIN.ln()) * p).exp();
            assert!(rel(track_to_closeness(closeness_to_track(v)), v) < 1e-4, "{v}");
        }
        // "At the default" is judged on the track: a tenth of the way to
        // either end is not the default, however small the value.
        assert!(at_default(CLOSENESS_DEFAULT));
        assert!(!at_default(track_to_closeness(0.45)) && !at_default(track_to_closeness(0.55)));
        assert!(!at_default(CLOSENESS_MIN) && !at_default(CLOSENESS_MAX));
        // Monotonic: further right is looser (a lower closeness).
        assert!(track_to_closeness(0.3) > track_to_closeness(0.5) && track_to_closeness(0.5) > track_to_closeness(0.7));
        // Out of range clamps to the ends.
        assert_eq!(track_to_closeness(-1.0), CLOSENESS_MAX);
        assert_eq!(track_to_closeness(2.0), CLOSENESS_MIN);
        assert_eq!(closeness_to_track(CLOSENESS_MAX + 5.0), 0.0);
        assert_eq!(closeness_to_track(CLOSENESS_MIN - 5.0), 1.0);
        // Ten arrow presses walk from the middle to either end.
        assert!((0.5 - 10.0 * TRACK_STEP).abs() < 1e-6);
    }

    #[test]
    fn home_paths_are_shortened() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home.join(".cache/photo_pipeline/thumbs")), "~/.cache/photo_pipeline/thumbs");
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(Path::new("/mnt/nas/library")), "/mnt/nas/library");
    }

    #[test]
    fn a_remembered_viewer_size_is_one_at_least_the_minimum() {
        assert!(!remembered_viewer_size(&Settings::default()));
        assert!(remembered_viewer_size(&Settings { viewer_width: 1200.0, viewer_height: 800.0, ..Settings::default() }));
        assert!(!remembered_viewer_size(&Settings { viewer_width: 100.0, viewer_height: 800.0, ..Settings::default() }));
        assert!(remembered_viewer_size(&Settings { viewer_maximized: true, ..Settings::default() }));
    }
}
