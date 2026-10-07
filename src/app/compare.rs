//! The compare view: two to four photos side by side in the viewer window,
//! with one linked zoom and pan, for picking the best of near-identical
//! frames.
//!
//! Compare borrows the viewer's window.  An open viewer is set aside and
//! comes back as it was on leaving; with none open, the viewer window opens
//! for the compare and closes again on leaving.  Keep and prune apply to the
//! focused photo and reach its RAW+JPG pair, as everywhere else.  Photos
//! load as in the viewer (the camera's preview first, then the full image)
//! and stay pinned in the loader's cache while they are on screen.
//!
//! **Frames of a burst** (C on the viewer's burst sheet) open the same view
//! as a level below the sheet: Esc goes back to it, the outline on the
//! frame that had focus here.  The panes change without resetting the
//! view, so a zoom carries on as frames change: `,` and `.` swap the
//! outlined pane's frame for the previous or next one of the burst not
//! already shown, and PgUp / PgDn page through the burst.  `1` zooms every
//! pane to 100 % on its own AF point (the camera's FocusLocation), still
//! linked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, image as iimage, row, text};
use iced::{Alignment, Element, Length, Padding, Rectangle, Size, Task, window};

use super::kit;
use super::style;
use super::viewer::{self, Source, Viewer};
use super::{App, Message, View};
use crate::imaging::loader::FullImage;
use crate::imaging::thumbs::ThumbCache;
use crate::model::collection::{Collection, RecordId};
use crate::model::record::FileType;
use crate::ui::compare_stage::{self, CompareEvent, CompareStage, MAX_PANES, Pane};
use crate::ui::icons::{self, Glyph};
use crate::ui::image_stage::ViewCmd;
use crate::ui::paint;
use crate::ui::theme as t;
use crate::util;

/// Until a photo or its thumbnail says otherwise, lay out for 3:2.
const DEFAULT_ASPECT: f32 = 1.5;

pub struct Compare {
    /// The viewer whose window this is; it comes back on leaving.
    viewer: Viewer,
    /// The window was opened for this compare, so leaving closes it.
    own_window: bool,
    pub items: Vec<RecordId>,
    pub focus: usize,
    images: Vec<Option<Arc<FullImage>>>,
    failed: Vec<bool>,
    /// Each photo's shape from its thumbnail, to lay out with until the
    /// photo itself arrives.
    hints: Vec<f32>,
    /// Changes with each new set of photos (the stage resets its view).
    key: u64,
    cmd: (u64, ViewCmd),
    zoom: f32,
    stage: Size,
    /// Comparing frames of a burst: the burst the panes show frames of.
    pub burst: Option<BurstView>,
}

/// The burst the panes show frames of.
pub struct BurstView {
    /// Every frame of the burst, in capture order (one record per shot).
    pub frames: Vec<RecordId>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Fit,
    Actual,
    ZoomIn,
    ZoomOut,
    TogglePrune,
    ToggleMark,
    /// Show the focused photo on its own, in the viewer.
    OpenInViewer,
    Leave,
    Stage(CompareEvent),
    Allocated(PathBuf, Result<iimage::Allocation, iimage::Error>),
    /// Frames of a burst: the outlined pane shows the previous / next frame.
    Step(i32),
    /// Frames of a burst: the previous / next set of frames (`i32::MIN` /
    /// `MAX`: the first / last).
    Page(i32),
}

impl Compare {
    pub fn id(&self) -> window::Id {
        self.viewer.id
    }

    /// The viewer whose window this is (its window state stays current).
    pub fn viewer_mut(&mut self) -> &mut Viewer {
        &mut self.viewer
    }

    pub fn title(&self) -> String {
        match &self.burst {
            Some(_) => format!("Compare {} frames of a burst", self.items.len()),
            None => format!("Compare {} photos", self.items.len()),
        }
    }

    /// For the test driver: the zoom, and the burst's frames on show.
    pub fn debug_state(&self) -> String {
        let burst = self.burst.as_ref().map(|b| format!(" burst={} frames shown={:?}", b.frames.len(), self.frame_positions())).unwrap_or_default();
        format!("zoom={:.0}%{burst}", self.zoom * 100.0)
    }

    /// Frames of a burst: where each pane's frame sits in the burst.
    fn frame_positions(&self) -> Vec<usize> {
        let Some(b) = &self.burst else { return Vec::new() };
        self.items.iter().filter_map(|id| b.frames.iter().position(|f| f == id)).collect()
    }

    fn aspects(&self) -> Vec<f32> {
        self.images
            .iter()
            .zip(&self.hints)
            .map(|(img, &hint)| img.as_ref().map(|i| i.width.max(1) as f32 / i.height.max(1) as f32).unwrap_or(hint))
            .collect()
    }

    fn cells(&self) -> Vec<compare_stage::Cell> {
        compare_stage::layout(&self.aspects(), Rectangle::with_size(self.stage))
    }

    /// The viewport every photo is fitted to (they are all alike).
    fn pane_size(&self) -> Size {
        self.cells().first().map(|c| c.photo.size()).filter(|s| s.width > 1.0 && s.height > 1.0).unwrap_or(Size::new(760.0, 560.0))
    }
}

// ── opening and leaving ─────────────────────────────────────────────────────

/// The photos selected in the grid, in display order.  A RAW+JPG pair
/// counts once, as its JPG (what the grid shows).
fn selected(app: &App) -> Vec<RecordId> {
    match app.view {
        View::Grid => app.derived.visible.iter().copied().filter(|id| app.grid.selection.contains(id)).collect(),
        View::Calendar | View::Search | View::Subjects => Vec::new(),
    }
}

/// The command is available: 2 to 4 photos are selected.
pub fn can_open(app: &App) -> bool {
    let n = match app.view {
        View::Grid => app.grid.selection.len(),
        View::Calendar | View::Search | View::Subjects => 0,
    };
    (2..=MAX_PANES).contains(&n)
}

/// Compare the selection (the C key).
pub fn open_selected(app: &mut App) -> Task<Message> {
    if app.view.is_library() {
        return Task::none();
    }
    let ids = selected(app);
    match ids.len() {
        2..=MAX_PANES => open(app, ids),
        n if n > MAX_PANES => app.show_status(format!("Compare shows up to {MAX_PANES} photos at a time. {n} are selected."), 5000),
        _ => app.show_status("Select 2 to 4 photos to compare: Ctrl+click them, or press S to select.", 5000),
    }
}

fn aspect_hint(coll: &Collection, thumbs: &ThumbCache, id: RecordId) -> f32 {
    coll.get(id)
        .and_then(|r| thumbs.get(&r.path))
        .filter(|th| th.ok && th.width() > 0 && th.height() > 0)
        .map(|th| th.width() as f32 / th.height() as f32)
        .unwrap_or(DEFAULT_ASPECT)
}

pub fn open(app: &mut App, items: Vec<RecordId>) -> Task<Message> {
    if items.is_empty() {
        return Task::none();
    }
    let mut tasks = Vec::new();
    if app.compare.is_none() {
        let own_window = app.viewer.is_none();
        if own_window {
            // The viewer window opens on the first photo, which the compare
            // needs anyway, and lends itself to the compare at once.
            tasks.push(viewer::open(app, Source::Working, vec![items[0]], 0, false));
        } else {
            // The viewer's look-ahead isn't wanted while comparing.
            app.loader.clear_queue();
        }
        let Some(v) = app.viewer.take() else { return Task::batch(tasks) };
        if !own_window {
            tasks.push(window::gain_focus(v.id));
        }
        app.compare = Some(Compare {
            viewer: v,
            own_window,
            items: Vec::new(),
            focus: 0,
            images: Vec::new(),
            failed: Vec::new(),
            hints: Vec::new(),
            key: 0,
            cmd: (0, ViewCmd::None),
            zoom: 1.0,
            stage: Size::ZERO,
            burst: None,
        });
    } else if let Some(c) = &app.compare {
        tasks.push(window::gain_focus(c.id()));
    }
    let hints: Vec<f32> = items.iter().map(|&id| aspect_hint(&app.coll, &app.thumbs, id)).collect();
    let Some(c) = app.compare.as_mut() else { return Task::batch(tasks) };
    c.key += 1;
    c.focus = 0;
    c.images = vec![None; items.len()];
    c.failed = vec![false; items.len()];
    c.hints = hints;
    c.items = items;
    c.burst = None;
    tasks.push(load(app));
    Task::batch(tasks)
}

// ── frames of a burst ───────────────────────────────────────────────────────

/// Compare `shown`, frames of the burst `frames` (the viewer's burst sheet,
/// whose window this borrows), the pane at `focus` outlined.
pub fn open_frames(app: &mut App, frames: Vec<RecordId>, shown: Vec<RecordId>, focus: usize) -> Task<Message> {
    if shown.is_empty() {
        return Task::none();
    }
    let task = open(app, shown);
    let Some(c) = app.compare.as_mut() else { return task };
    c.focus = focus.min(c.items.len().saturating_sub(1));
    c.burst = Some(BurstView { frames });
    prefetch(app);
    task
}

/// Put `items` in the panes without resetting the view (a zoom carries
/// on), the outlined pane at `focus`.
fn show_frames(app: &mut App, items: Vec<RecordId>, focus: usize) -> Task<Message> {
    let hints: Vec<f32> = items.iter().map(|&id| aspect_hint(&app.coll, &app.thumbs, id)).collect();
    let Some(c) = app.compare.as_mut() else { return Task::none() };
    if items == c.items {
        c.focus = focus.min(items.len().saturating_sub(1));
        return Task::none();
    }
    c.images = vec![None; items.len()];
    c.failed = vec![false; items.len()];
    c.hints = hints;
    c.focus = focus.min(items.len().saturating_sub(1));
    c.items = items;
    // Frames paged past aren't wanted any more.
    app.loader.clear_queue();
    let t = load(app);
    prefetch(app);
    t
}

/// The camera previews of the frames either side of those shown, so the
/// next page or step shows at once.
fn prefetch(app: &mut App) {
    let Some(c) = app.compare.as_ref() else { return };
    let Some(b) = &c.burst else { return };
    let at = c.frame_positions();
    let (Some(&lo), Some(&hi)) = (at.iter().min(), at.iter().max()) else { return };
    let n = c.items.len();
    let wanted: Vec<RecordId> = b.frames[hi + 1..(hi + 1 + n).min(b.frames.len())].iter().chain(&b.frames[lo.saturating_sub(n)..lo]).copied().collect();
    for id in wanted {
        if let Some(r) = app.coll.get(id) {
            let (path, ft) = (r.path.clone(), r.file_type);
            app.loader.request_preview(&path, ft, false);
        }
    }
}

/// The outlined pane shows the frame `dir` steps on, skipping frames the
/// other panes show.
fn step(app: &mut App, dir: i32) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let Some(b) = &c.burst else { return Task::none() };
    let Some(&cur) = c.items.get(c.focus) else { return Task::none() };
    let Some(mut i) = b.frames.iter().position(|&f| f == cur) else { return Task::none() };
    loop {
        let Some(next) = i.checked_add_signed(dir as isize).filter(|&n| n < b.frames.len()) else { return Task::none() };
        i = next;
        if !c.items.contains(&b.frames[i]) {
            break;
        }
    }
    let mut items = c.items.clone();
    items[c.focus] = b.frames[i];
    let focus = c.focus;
    show_frames(app, items, focus)
}

/// The next (`dir` > 0) or previous set of frames, in order, the outline
/// staying on the same pane.
fn page(app: &mut App, dir: i32) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let Some(b) = &c.burst else { return Task::none() };
    let n = c.items.len().max(1);
    let len = b.frames.len();
    let lo = c.frame_positions().into_iter().min().unwrap_or(0);
    let start = match dir {
        i32::MIN => 0,
        i32::MAX => len.saturating_sub(n),
        d if d > 0 => (lo + n).min(len.saturating_sub(n)),
        _ => lo.saturating_sub(n),
    };
    let items = b.frames[start..(start + n).min(len)].to_vec();
    let focus = c.focus;
    show_frames(app, items, focus)
}

fn files(app: &App, c: &Compare) -> Vec<Option<(PathBuf, FileType)>> {
    c.items.iter().map(|&id| app.coll.get(id).map(|r| (r.path.clone(), r.file_type))).collect()
}

/// Ask for every photo (the focused one first: all the previews come ahead
/// of any full decode), and pin them in the cache while they are shown.
fn load(app: &mut App) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let files = files(app, c);
    let focus = c.focus;
    app.loader.pin(files.iter().flatten().map(|(p, _)| p.clone()).collect());
    let order = std::iter::once(focus).chain((0..files.len()).filter(|&i| i != focus));
    for i in order {
        if let Some(Some((p, ft))) = files.get(i) {
            app.loader.request(p, *ft);
        }
    }
    let cached: Vec<Option<Arc<FullImage>>> = files.iter().map(|f| f.as_ref().and_then(|(p, _)| app.loader.get(p))).collect();
    let Some(c) = app.compare.as_mut() else { return Task::none() };
    c.images = cached;
    allocate_all(app)
}

/// Hand the window back to the viewer, as it was (no window changes): the
/// viewer is about to show something else in it.
pub fn release(app: &mut App) {
    let Some(c) = app.compare.take() else { return };
    app.loader.pin(Vec::new());
    app.viewer = Some(c.viewer);
}

/// Leave the compare: the viewer it borrowed the window from comes back,
/// or the window closes if it was opened for the compare.
pub fn leave(app: &mut App) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    // The grid's keyboard focus follows the photo that had focus here.
    let focused = c.items.get(c.focus).copied();
    if let Some(id) = focused {
        if app.grid.selection.contains(&id) {
            app.grid.anchor = Some(id);
        }
    }
    let own_window = c.own_window;
    release(app);
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    if own_window || v.items.is_empty() {
        let mut tasks = vec![viewer::close(app)];
        if let Some(main) = app.main_id {
            tasks.push(window::gain_focus(main));
        }
        return Task::batch(tasks);
    }
    // Show the viewer's photo again (it reloads from the cache, and its
    // look-ahead and details pick up where they were); a burst's sheet
    // comes back with its outline on the frame that had focus here.
    viewer::resume(app, focused)
}

/// The window's close button.
pub fn close_requested(app: &mut App) -> Task<Message> {
    release(app);
    viewer::close(app)
}

/// The window is gone.
pub fn window_closed(app: &mut App) {
    app.compare = None;
    app.loader.pin(Vec::new());
}

/// Records left the working folder (trashed, moved away).
pub fn on_removed(app: &mut App, ids: &[RecordId]) {
    let Some(c) = app.compare.as_mut() else { return };
    if !c.viewer.read_only {
        c.viewer.items.retain(|x| !ids.contains(x));
        super::viewer_burst::items_changed(&app.bursts, &mut c.viewer);
    }
    if let Some(b) = &mut c.burst {
        b.frames.retain(|x| !ids.contains(x));
    }
    let keep: Vec<bool> = c.items.iter().map(|x| !ids.contains(x)).collect();
    if keep.iter().all(|&k| k) {
        return;
    }
    fn retain<T>(v: &mut Vec<T>, keep: &[bool]) {
        let mut it = keep.iter();
        v.retain(|_| *it.next().unwrap_or(&true));
    }
    retain(&mut c.items, &keep);
    retain(&mut c.hints, &keep);
    retain(&mut c.images, &keep);
    retain(&mut c.failed, &keep);
    c.focus = c.focus.min(c.items.len().saturating_sub(1));
    if c.items.is_empty() {
        release(app);
    }
}

// ── loading results ─────────────────────────────────────────────────────────

/// A full image arrived (the loader has it already).
pub fn on_loaded(app: &mut App, path: &Path, result: &Result<Arc<FullImage>, String>) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let hits: Vec<usize> = files(app, c).iter().enumerate().filter(|(_, f)| f.as_ref().is_some_and(|(p, _)| p == path)).map(|(i, _)| i).collect();
    if hits.is_empty() {
        return Task::none();
    }
    let sf = app.scale_factor;
    let Some(c) = app.compare.as_mut() else { return Task::none() };
    match result {
        Ok(img) => {
            for &i in &hits {
                c.images[i] = Some(img.clone());
                c.failed[i] = false;
            }
            allocate(c.pane_size(), sf, path, img)
        }
        Err(_) => {
            for &i in &hits {
                c.failed[i] = c.images[i].is_none();
            }
            Task::none()
        }
    }
}

/// The camera's preview arrived: show it while the full image decodes.
pub fn on_preview(app: &mut App, path: &Path) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let hits: Vec<usize> = files(app, c)
        .iter()
        .enumerate()
        .filter(|(i, f)| c.images[*i].is_none() && f.as_ref().is_some_and(|(p, _)| p == path))
        .map(|(i, _)| i)
        .collect();
    let Some(img) = app.loader.peek(path).cloned().filter(|_| !hits.is_empty()) else { return Task::none() };
    let sf = app.scale_factor;
    let Some(c) = app.compare.as_mut() else { return Task::none() };
    for &i in &hits {
        c.images[i] = Some(img.clone());
    }
    allocate(c.pane_size(), sf, path, &img)
}

/// Put the levels the stage draws at fit size (and the smallest, which it
/// draws underneath everything) on the GPU ahead of time, and keep them
/// there while the image is cached.
fn allocate(vp: Size, sf: f32, path: &Path, img: &Arc<FullImage>) -> Task<Message> {
    if img.levels.is_empty() {
        return Task::none();
    }
    let fit = compare_stage::fit_scale(Size::new(img.width as f32, img.height as f32), vp);
    let i = img.level_for_scale(fit * sf);
    let last = img.levels.len() - 1;
    let mut levels = vec![i, (i + 1).min(last), last];
    levels.dedup();
    Task::batch(levels.into_iter().map(|idx| {
        let p = path.to_path_buf();
        iimage::allocate(img.levels[idx].handle.clone()).map(move |r| Message::Compare(Msg::Allocated(p.clone(), r)))
    }))
}

fn allocate_all(app: &mut App) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    let vp = c.pane_size();
    let sf = app.scale_factor;
    let tasks: Vec<Task<Message>> = files(app, c)
        .iter()
        .zip(&c.images)
        .filter_map(|(f, img)| Some(allocate(vp, sf, &f.as_ref()?.0, img.as_ref()?)))
        .collect();
    Task::batch(tasks)
}

// ── update ──────────────────────────────────────────────────────────────────

fn focused(app: &App) -> Option<RecordId> {
    let c = app.compare.as_ref()?;
    c.items.get(c.focus).copied()
}

fn set_focus(app: &mut App, i: usize) -> Task<Message> {
    if let Some(c) = app.compare.as_mut() {
        if i < c.items.len() {
            c.focus = i;
        }
    }
    Task::none()
}

fn command(app: &mut App, cmd: ViewCmd) {
    if let Some(c) = app.compare.as_mut() {
        c.cmd = (c.cmd.0 + 1, cmd);
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    if app.compare.is_none() {
        return Task::none();
    }
    let reviewing = app.compare.as_ref().is_some_and(|c| c.burst.is_some());
    match msg {
        Msg::Fit => command(app, ViewCmd::Fit),
        // Comparing frames of a burst, 100 % goes to each photo's AF point.
        Msg::Actual => command(app, if reviewing { ViewCmd::Focus } else { ViewCmd::Actual }),
        Msg::Step(dir) => return step(app, dir),
        Msg::Page(dir) => return page(app, dir),
        Msg::ZoomIn => command(app, ViewCmd::ZoomIn),
        Msg::ZoomOut => command(app, ViewCmd::ZoomOut),
        Msg::TogglePrune => {
            let Some(id) = focused(app) else { return Task::none() };
            if let Some(r) = app.coll.get_mut(id) {
                r.is_pruned = !r.is_pruned;
            }
            return app.refresh_prune(&[id]);
        }
        Msg::ToggleMark => {
            let Some(id) = focused(app) else { return Task::none() };
            if let Some(r) = app.coll.get_mut(id) {
                r.is_marked = !r.is_marked;
            }
            return app.refresh_mark(&[id]);
        }
        Msg::OpenInViewer => {
            let Some(c) = app.compare.as_ref() else { return Task::none() };
            let Some(&id) = c.items.get(c.focus) else { return Task::none() };
            // Step through the grid from there, as if opened from it, on
            // the photo itself.
            let (items, index) = match app.derived.visible.iter().position(|&x| x == id) {
                Some(i) => (app.derived.visible.clone(), i),
                None => (c.items.clone(), c.focus),
            };
            return viewer::open_as(app, Source::Working, items, index, false, super::viewer_burst::Land::Frame);
        }
        Msg::Leave => return leave(app),
        Msg::Stage(ev) => match ev {
            CompareEvent::Focus(i) => return set_focus(app, i),
            CompareEvent::Zoom(z) => {
                if let Some(c) = app.compare.as_mut() {
                    c.zoom = z;
                }
            }
            CompareEvent::Resized(s) => {
                if let Some(c) = app.compare.as_mut() {
                    c.stage = s;
                }
                return allocate_all(app);
            }
        },
        Msg::Allocated(path, Ok(allocation)) => app.loader.hold_allocation(&path, allocation),
        Msg::Allocated(_, Err(_)) => {}
    }
    Task::none()
}

pub fn on_key(app: &mut App, key: Key, mods: keyboard::Modifiers) -> Task<Message> {
    let Some(c) = app.compare.as_ref() else { return Task::none() };
    if mods.control() || mods.alt() {
        return Task::none();
    }
    let ch = match &key {
        Key::Character(s) => Some(s.to_lowercase()),
        _ => None,
    };
    let n = c.items.len().max(1);
    let focus = c.focus;
    let cells = c.cells();
    let arrow = |dx: i32, dy: i32| compare_stage::neighbour(&cells, focus, dx, dy);
    if c.burst.is_some() {
        let shift = mods.shift();
        let msg = match (&key, ch.as_deref()) {
            (_, Some(",")) | (_, Some("<")) => Some(Msg::Step(-1)),
            (_, Some(".")) | (_, Some(">")) => Some(Msg::Step(1)),
            (Key::Named(Named::ArrowLeft), _) if shift => Some(Msg::Step(-1)),
            (Key::Named(Named::ArrowRight), _) if shift => Some(Msg::Step(1)),
            (Key::Named(Named::PageUp), _) => Some(Msg::Page(-1)),
            (Key::Named(Named::PageDown), _) => Some(Msg::Page(1)),
            (Key::Named(Named::Home), _) => Some(Msg::Page(i32::MIN)),
            (Key::Named(Named::End), _) => Some(Msg::Page(i32::MAX)),
            _ => None,
        };
        if let Some(m) = msg {
            return update(app, m);
        }
    }
    match (&key, ch.as_deref()) {
        (Key::Named(Named::Escape), _) => leave(app),
        (Key::Named(Named::Enter), _) => update(app, Msg::OpenInViewer),
        (Key::Named(Named::Tab), _) => {
            let to = if mods.shift() { (focus + n - 1) % n } else { (focus + 1) % n };
            set_focus(app, to)
        }
        (Key::Named(Named::ArrowLeft), _) | (_, Some("a")) => match arrow(-1, 0) {
            Some(i) => set_focus(app, i),
            None => Task::none(),
        },
        (Key::Named(Named::ArrowRight), _) | (_, Some("d")) => match arrow(1, 0) {
            Some(i) => set_focus(app, i),
            None => Task::none(),
        },
        (Key::Named(Named::ArrowUp), _) => match arrow(0, -1) {
            Some(i) => set_focus(app, i),
            None => Task::none(),
        },
        (Key::Named(Named::ArrowDown), _) => match arrow(0, 1) {
            Some(i) => set_focus(app, i),
            None => Task::none(),
        },
        (Key::Named(Named::Space), _) | (_, Some("f")) => update(app, Msg::Fit),
        (_, Some("1")) => update(app, Msg::Actual),
        (_, Some("+")) | (_, Some("=")) => update(app, Msg::ZoomIn),
        (_, Some("-")) => update(app, Msg::ZoomOut),
        (Key::Named(Named::Delete), _) | (_, Some("p")) => update(app, Msg::TogglePrune),
        (_, Some("m")) => update(app, Msg::ToggleMark),
        (_, Some("u")) => match focused(app) {
            Some(id) if app.coll.get(id).is_some_and(|r| r.is_pruned) => update(app, Msg::TogglePrune),
            _ => Task::none(),
        },
        (_, Some("?")) => app.open_shortcuts(),
        _ => Task::none(),
    }
}

// ── view ────────────────────────────────────────────────────────────────────

fn cmsg(m: Msg) -> Option<Message> {
    Some(Message::Compare(m))
}

pub fn view<'a>(app: &'a App, c: &'a Compare) -> Element<'a, Message> {
    let n = c.items.len();
    let focused = c.items.get(c.focus).and_then(|&id| app.coll.get(id));

    // ── top bar: what this is, the focused photo's marks, done ──────────
    let (title, caption) = match &c.burst {
        Some(b) => {
            let at = c.frame_positions();
            let (lo, hi) = (at.iter().min().map_or(0, |x| x + 1), at.iter().max().map_or(0, |x| x + 1));
            let shown = if lo == hi {
                format!("Frame {lo}")
            } else if hi - lo + 1 == at.len() {
                format!("Frames {lo}–{hi}")
            } else {
                format!("{} frames", at.len())
            };
            ("Compare frames", format!("{shown} of {}  ·  Shift+arrow keys change the outlined photo's frame, PgUp and PgDn page", b.frames.len()))
        }
        None => ("Compare", format!("{n} photos  ·  arrow keys or Tab move between them")),
    };
    let mut top = row![
        Space::new().width(t::S1),
        icons::view(Glyph::Compare, 18.0, t::INK2),
        text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK),
        kit::caption(caption),
        Space::new().width(Length::Fill),
    ]
    .spacing(t::S2)
    .align_y(Alignment::Center);
    if let Some(r) = focused {
        top = top.push(text(r.filename()).size(t::T_BODY).font(paint::MEDIUM).color(t::INK2));
        top = top.push(kit::tip(kit::icon_btn(Glyph::Photos, t::INK2, style::ghost(), cmsg(Msg::OpenInViewer)), "Open this photo in the viewer  (Enter)", true));
        top = top.push(kit::vrule(20.0));
        let (marked, pruned) = (r.is_marked, r.is_pruned);
        top = top.push(kit::tip(
            kit::icon_text_btn(Glyph::Keep, "Keep", if marked { t::KEEP } else { t::INK2 }, style::toggle(marked, t::KEEP), cmsg(Msg::ToggleMark)),
            "Mark this photo for export  (M)",
            true,
        ));
        top = top.push(kit::tip(
            kit::icon_text_btn(Glyph::Prune, "Prune", if pruned { t::REJECT } else { t::INK2 }, style::toggle(pruned, t::REJECT), cmsg(Msg::TogglePrune)),
            "Mark this photo to prune  (P)",
            true,
        ));
    }
    top = top.push(kit::vrule(20.0));
    let done_tip = if c.burst.is_some() { "Back to the burst's frames  (Esc)" } else { "Leave compare  (Esc)" };
    top = top.push(kit::tip(kit::btn("Done", style::secondary(), cmsg(Msg::Leave)), done_tip, true));
    let top_bar = container(top.padding(Padding::from([0.0, t::S2]))).height(48).center_y(48).width(Length::Fill).style(style::fill(t::MOUNT));

    // ── the photos ──────────────────────────────────────────────────────
    let panes: Vec<Pane> = c
        .items
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let r = app.coll.get(id);
            let image = c.images.get(i).cloned().flatten();
            let status = match (&image, c.failed.get(i).copied().unwrap_or(false)) {
                (Some(_), _) => None,
                (None, true) => Some("Could not load image".to_string()),
                (None, false) => Some("Loading…".to_string()),
            };
            Pane {
                image,
                aspect: c.hints.get(i).copied().unwrap_or(DEFAULT_ASPECT),
                name: r.map(|r| if r.is_paired() { r.stem() } else { r.filename() }).unwrap_or_default(),
                tag: r.map(crate::ui::tags::Kind::of).unwrap_or(crate::ui::tags::Kind::Jpg),
                marked: r.is_some_and(|r| r.is_marked),
                pruned: r.is_some_and(|r| r.is_pruned),
                crop: r.and_then(|r| r.crop.as_ref()).filter(|c| !c.is_full()).map(|c| [c.left as f32, c.top as f32, c.right as f32, c.bottom as f32]),
                status,
                focus: r.and_then(|r| r.shot.focus_point()),
                hint: crate::ui::hint_plate::phrase(&app.quality.hints, id),
            }
        })
        .collect();
    let stage = CompareStage::new(c.key, panes, c.focus, c.cmd, app.scale_factor, |e| Message::Compare(Msg::Stage(e)));

    // ── bottom bar: zoom, the focused photo's facts ─────────────────────
    let zoom_label = text(format!("{:.0}%", c.zoom * 100.0)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK2).width(44).center();
    let mut bottom = row![
        kit::tip(kit::icon_btn(Glyph::ZoomOut, t::INK2, style::ghost(), cmsg(Msg::ZoomOut)), "Zoom out  (−)", false),
        zoom_label,
        kit::tip(kit::icon_btn(Glyph::ZoomIn, t::INK2, style::ghost(), cmsg(Msg::ZoomIn)), "Zoom in  (+)", false),
        kit::tip(kit::icon_btn(Glyph::Fit, t::INK2, style::ghost(), cmsg(Msg::Fit)), "Fit to the window  (F)", false),
        kit::tip(
            button(container(text("1:1").size(t::T_SMALL).font(paint::MEDIUM)).center(Length::Fill))
                .width(t::CONTROL_H + 8.0)
                .height(t::CONTROL_H)
                .padding(0)
                .style(style::ghost().style())
                .on_press(Message::Compare(Msg::Actual)),
            if c.burst.is_some() { "Actual size, each on its focus point  (1)" } else { "Actual size  (1)" },
            false,
        ),
        kit::vrule(16.0),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    if let Some(r) = focused {
        let mut facts = row![].spacing(t::S4).align_y(Alignment::Center);
        facts = facts.push(text(r.file_type.label()).size(t::T_SMALL).font(paint::MEDIUM).color(if r.file_type == FileType::Raw { t::RAW } else { t::JPG }));
        if let Some(img) = c.images.get(c.focus).cloned().flatten() {
            facts = facts.push(kit::caption(format!("{} × {} px", img.width, img.height)));
        }
        facts = facts.push(kit::caption(util::fmt_size(r.file_size)));
        facts = facts.push(kit::caption(r.modified.local.format("%Y-%m-%d %H:%M:%S").to_string()));
        bottom = bottom.push(container(facts).padding(Padding::from([0.0, t::S2])));
    }
    bottom = bottom.push(Space::new().width(Length::Fill));
    bottom = bottom.push(kit::caption(if c.burst.is_some() {
        "1 shows each photo at 100 % on its focus point  ·  zoom and pan move them together"
    } else {
        "Zoom and pan move every photo together"
    }));
    let bottom_bar = container(bottom.padding(Padding::from([0.0, t::S2]))).height(44).center_y(44).width(Length::Fill).style(style::fill(t::MOUNT));

    let body = column![top_bar, kit::hairline(), stage, kit::hairline(), bottom_bar];
    container(body).width(Length::Fill).height(Length::Fill).style(style::fill(t::GROUND)).into()
}
