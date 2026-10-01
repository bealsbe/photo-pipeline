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
//!   presses, ← and → change the thumbnail size, Esc closes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::advanced::widget::operation::{Operation, Outcome, scrollable::AbsoluteOffset};
use iced::advanced::widget::{self as adv, Id};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, column, container, row, scrollable, slider, stack, text, toggler};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Rectangle, Size, Task, Theme, Vector, window};

use super::commands::Cmd;
use super::preferences_storage::{self as storage, Cache, Cleared, Survey, Usage};
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
    FileNames,
    ThumbSize,
    Filmstrip,
    BurstSheet,
    ViewerSize,
    Library,
    EditCopyFolder,
    EditCopySubfolder,
    GlobalMenu,
    Open(Cache),
    Clear(Cache),
    OpenUnsaved,
    Close,
}

impl Control {
    fn id(self) -> &'static str {
        match self {
            Control::Advance => "pref-advance",
            Control::FileNames => "pref-names",
            Control::ThumbSize => "pref-thumb-size",
            Control::Filmstrip => "pref-filmstrip",
            Control::BurstSheet => "pref-burst-sheet",
            Control::ViewerSize => "pref-viewer-size",
            Control::Library => "pref-library",
            Control::EditCopyFolder => "pref-edit-copy",
            Control::EditCopySubfolder => "pref-edit-copy-subfolder",
            Control::GlobalMenu => "pref-global-menu",
            Control::Open(Cache::Thumbs) => "pref-open-thumbs",
            Control::Open(Cache::Library) => "pref-open-library",
            Control::Open(Cache::CaptureTimes) => "pref-open-capture",
            Control::Clear(Cache::Thumbs) => "pref-clear-thumbs",
            Control::Clear(Cache::Library) => "pref-clear-library",
            Control::Clear(Cache::CaptureTimes) => "pref-clear-capture",
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
    /// The cache a confirmation is asking about.
    confirm: Option<Cache>,
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
        }
    }

    fn usage(&self, c: Cache) -> Option<Usage> {
        self.survey.as_ref()?.caches.iter().find(|(k, _)| *k == c).map(|(_, u)| *u)
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
            "open={} focus={:?} ring={} confirm={:?} clearing={:?} counting={} sizes=[{}] journal={journal} last_clear={:?}",
            self.window.is_some(),
            self.focus,
            self.ring,
            self.confirm,
            self.clearing,
            self.counting,
            sizes.join(", "),
            self.last_clear,
        )
    }
}

fn m(msg: Msg) -> Message {
    Message::Prefs(msg)
}

/// Run `f` on a thread of its own; `None` if it panicked.
async fn on_worker<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
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
    Task::perform(on_worker("prefs-count", storage::survey), move |s| m(Msg::Surveyed(seq, s)))
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
            let Some(cache) = p.confirm.take() else { return Task::none() };
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
    }
}

/// Snap like the filter bar's slider, and remember it.
fn set_thumb_size(app: &mut App, v: f32) {
    let _ = app.on_filter(FilterMsg::Zoom(v));
    app.settings.thumb_size = app.thumb_size as u32;
}

fn press(app: &mut App, c: Control) -> Task<Message> {
    match c {
        Control::Advance => viewer_advance::toggle(app),
        Control::FileNames => app.run_cmd(Cmd::FileNames),
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
        Control::Open(cache) => {
            if let Some(d) = cache.dir() {
                dialogs::open_folder(&d);
            }
            Task::none()
        }
        Control::Clear(cache) => {
            if clearable(&app.prefs, cache) {
                app.prefs.confirm = Some(cache);
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

fn clearable(p: &Prefs, c: Cache) -> bool {
    p.clearing.is_none() && p.usage(c).is_some_and(|u| u.files > 0)
}

fn remembered_viewer_size(s: &Settings) -> bool {
    s.viewer_maximized || (s.viewer_width >= super::viewer_window::MIN_SIZE.width && s.viewer_height >= super::viewer_window::MIN_SIZE.height)
}

/// The controls on the page now, in Tab order (ones that can't be used
/// right now are skipped).
fn controls(app: &App) -> Vec<Control> {
    let p = &app.prefs;
    let mut v = vec![Control::Advance, Control::FileNames, Control::ThumbSize, Control::Filmstrip, Control::BurstSheet];
    if remembered_viewer_size(&app.settings) {
        v.push(Control::ViewerSize);
    }
    v.extend([Control::Library, Control::EditCopyFolder, Control::EditCopySubfolder]);
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
    if p.survey.as_ref().is_some_and(|s| s.journal.exists) {
        v.push(Control::OpenUnsaved);
    }
    v.push(Control::Close);
    v
}

/// A key press in the Preferences window.
pub fn on_key(app: &mut App, key: Key, mods: keyboard::Modifiers) -> Task<Message> {
    if app.prefs.confirm.is_some() {
        return match key {
            Key::Named(Named::Escape) => update(app, Msg::ConfirmCancel),
            Key::Named(Named::Enter) => update(app, Msg::ClearConfirmed),
            _ => Task::none(),
        };
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
        match reveal_offset(view.height, content.height, offset.y, target.y - content.y, target.height) {
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
    adv::operate(Reveal { target: Id::new(c.id()), page: Id::new(SCROLL), view: None, found: None }).discard()
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
    if let Some(cache) = p.confirm {
        layers.push(confirm_view(p, cache));
    }
    stack(layers).width(Length::Fill).height(Length::Fill).into()
}

fn culling(app: &App) -> Element<'_, Message> {
    let p = &app.prefs;
    section(
        "Culling",
        None,
        vec![setting(
            "Advance after marking",
            about("In the viewer, P or M moves on to the next photo once it marks one. Clearing a mark stays put."),
            row![keycap("Ctrl+Shift+A"), switch(p, Control::Advance, app.settings.viewer_advance, true)]
                .spacing(t::S3)
                .align_y(Alignment::Center)
                .into(),
        )],
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
                path_about(edit_copy.as_deref(), "Where Copy marked RAWs for editing puts them. Not chosen yet: it asks the first time."),
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
            unsaved_row(app),
        ],
    )
}

fn count_word(c: Cache, n: u64) -> String {
    match c {
        Cache::Thumbs => util::plural(n, "thumbnail"),
        Cache::Library => format!("{} {}", util::thousands(n), if n == 1 { "library" } else { "libraries" }),
        Cache::CaptureTimes => util::plural(n, "folder"),
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

fn confirm_view(p: &Prefs, cache: Cache) -> Element<'_, Message> {
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
