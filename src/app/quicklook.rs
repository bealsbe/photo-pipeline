//! Quick look (Space in the grid): the focused photo large, over the grid,
//! inside the main window — a check without opening the viewer.
//!
//! It steps through the grid's open days in grid order and takes the grid's
//! selection along (scrolling the grid beneath to follow), so closing lands
//! where you were.  P and M mark the photo shown.  The pictures come from the
//! viewer's loader: the grid's thumbnail is drawn at once, the camera's
//! embedded preview replaces it a few milliseconds later, then the full
//! image when it has decoded (while browsing, once you pause on a photo).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::image as iimage;
use iced::{Element, Point, Rectangle, Size, Task};

use super::{App, Message, View, viewer, viewer_burst};
use crate::imaging::loader::FullImage;
use crate::model::collection::RecordId;
use crate::model::record::FileType;
use crate::ui::quick_look::{self as ql, QuickLook as Widget, QuickLookEvent, Shown};
use crate::ui::side_menu::RAIL_W;
use crate::ui::theme as t;

pub struct QuickLook {
    /// The photo shown.
    pub id: RecordId,
    /// Where it sat in the grid's order when last seen there.  While it is
    /// hidden (pruned, with "Show pruned" off) the photos after it have
    /// moved up into its place, so this is where browsing carries on.
    hint: usize,
    path: PathBuf,
    preview: Option<Arc<FullImage>>,
    full: Option<Arc<FullImage>>,
    failed: Option<String>,
    key: u64,
    /// The widget's size (the grid's), once it has been laid out.
    viewport: Size,
    shown_at: Instant,
}

/// While stepping, the full image is asked for once the photo has been
/// shown this long: browsing runs on previews, and a full decode started
/// for every photo passed would hold up the next one's preview.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone)]
pub enum Msg {
    Widget(QuickLookEvent),
    Allocated(PathBuf, Result<iimage::Allocation, iimage::Error>),
    /// [`SETTLE`] after showing the photo with this key.
    Settled(u64),
}

// ── navigation ──────────────────────────────────────────────────────────────

/// The photos quick look steps through: the grid's open days, in order.
pub struct Nav {
    ids: Vec<RecordId>,
    /// Index of each day's first photo.
    starts: Vec<usize>,
}

/// Where the shown photo is in a [`Nav`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pos {
    At(usize),
    /// Not in the grid; it would sit just before this index.
    Gap(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Prev,
    Next,
    First,
    Last,
    PrevDay,
    NextDay,
}

impl Nav {
    pub fn new<'a>(days: impl IntoIterator<Item = &'a [RecordId]>) -> Nav {
        let (mut ids, mut starts) = (Vec::new(), Vec::new());
        for day in days.into_iter().filter(|d| !d.is_empty()) {
            starts.push(ids.len());
            ids.extend_from_slice(day);
        }
        Nav { ids, starts }
    }

    fn of(app: &App) -> Nav {
        Nav::new(app.derived.sections.iter().filter(|s| !app.grid.collapsed.contains(&s.date)).map(|s| s.ids.as_slice()))
    }

    pub fn locate(&self, id: RecordId, hint: usize) -> Pos {
        match self.ids.iter().position(|&x| x == id) {
            Some(i) => Pos::At(i),
            None => Pos::Gap(hint.min(self.ids.len())),
        }
    }

    /// The index a move goes to, or `None` when it goes nowhere.
    pub fn target(&self, pos: Pos, mv: Move) -> Option<usize> {
        let n = self.ids.len();
        if n == 0 {
            return None;
        }
        // The first index after the position, and the position as a bound:
        // a day starting before `at` is behind it, at or after `ahead` in front.
        let (at, ahead) = match pos {
            Pos::At(i) => (i, i + 1),
            Pos::Gap(h) => (h, h),
        };
        let to = match mv {
            Move::Prev => at.checked_sub(1),
            Move::Next => (ahead < n).then_some(ahead),
            Move::First => Some(0),
            Move::Last => Some(n - 1),
            Move::PrevDay => self.starts.iter().rev().copied().find(|&s| s < at),
            Move::NextDay => self.starts.iter().copied().find(|&s| s >= ahead),
        };
        to.filter(|&i| pos != Pos::At(i))
    }

    /// Where the grid's selection goes on closing: the photo, or the one
    /// that took its place.
    pub fn landing(&self, pos: Pos) -> Option<usize> {
        match pos {
            Pos::At(i) => Some(i),
            Pos::Gap(h) if h < self.ids.len() => Some(h),
            Pos::Gap(_) => self.ids.len().checked_sub(1),
        }
    }
}

/// The photo quick look opens on: the grid's focused selection, else the
/// first selected, else the last focused, else the first photo.
pub fn focus(nav: &Nav, anchor: Option<RecordId>, selection: &HashSet<RecordId>) -> Option<usize> {
    let pos = |id: RecordId| nav.ids.iter().position(|&x| x == id);
    anchor
        .filter(|a| selection.contains(a))
        .and_then(pos)
        .or_else(|| nav.ids.iter().position(|id| selection.contains(id)))
        .or_else(|| anchor.and_then(pos))
        .or((!nav.ids.is_empty()).then_some(0))
}

// ── opening, stepping, closing ──────────────────────────────────────────────

pub fn toggle(app: &mut App) -> Task<Message> {
    let _t = crate::profile::span("quick look", "open or close");
    if app.quicklook.is_some() {
        return close(app);
    }
    if app.view != View::Grid || app.modal.is_some() {
        return Task::none();
    }
    let nav = Nav::of(app);
    let Some(i) = focus(&nav, app.grid.anchor, &app.grid.selection) else { return Task::none() };
    crate::profile::mark("to first frame");
    app.quicklook = Some(QuickLook {
        id: nav.ids[i],
        hint: i,
        path: PathBuf::new(),
        preview: None,
        full: None,
        failed: None,
        key: 0,
        viewport: Size::ZERO,
        shown_at: Instant::now(),
    });
    show(app, &nav, i, false)
}

pub fn close(app: &mut App) -> Task<Message> {
    let Some(q) = app.quicklook.take() else { return Task::none() };
    let nav = Nav::of(app);
    match nav.landing(nav.locate(q.id, q.hint)) {
        Some(i) => {
            select(app, nav.ids[i]);
            reveal(app, nav.ids[i], q.viewport);
        }
        None if !app.grid.select_mode => app.grid.selection.clear(),
        None => {}
    }
    if app.viewer.is_none() {
        app.loader.clear_queue();
    }
    Task::none()
}

/// After bursts were stacked or opened (G): stay on the photo shown, or
/// move to the stack it was folded into, so quick look keeps showing a
/// tile of the grid.
pub fn follow(app: &mut App) -> Task<Message> {
    let Some(id) = app.quicklook.as_ref().map(|q| q.id) else { return Task::none() };
    let nav = Nav::of(app);
    if nav.ids.contains(&id) {
        return Task::none();
    }
    match app.tile_of(id).and_then(|t| nav.ids.iter().position(|&x| x == t)) {
        Some(i) => show(app, &nav, i, false),
        None => Task::none(),
    }
}

fn step(app: &mut App, mv: Move) -> Task<Message> {
    let Some(q) = &app.quicklook else { return Task::none() };
    let nav = Nav::of(app);
    match nav.target(nav.locate(q.id, q.hint), mv) {
        Some(i) => show(app, &nav, i, true),
        None => Task::none(),
    }
}

/// The grid's selection follows the photo shown (only the focus, in
/// select mode, so a selection being built survives).
fn select(app: &mut App, id: RecordId) {
    if !app.grid.select_mode {
        app.grid.selection = [id].into();
    }
    app.grid.anchor = Some(id);
}

/// Scroll the grid beneath so `id` is in view when quick look closes.
fn reveal(app: &mut App, id: RecordId, viewport: Size) {
    if let Some(y) = crate::ui::grid::reveal_scroll(&app.derived.sections, &app.grid.collapsed, app.thumb_size, if app.show_names { crate::ui::grid::LABEL_H } else { 0.0 }, viewport, app.grid.scroll, id) {
        app.grid.scroll = y;
        app.grid.scroll_seq += 1;
    }
}

/// Show `nav[i]`: what the loader has now, its preview first in line and
/// its neighbours' after.  The full image is asked for now, or once the
/// photo has stayed [`SETTLE`] when `browsing`.
fn show(app: &mut App, nav: &Nav, i: usize, browsing: bool) -> Task<Message> {
    let id = nav.ids[i];
    let Some((path, ft)) = app.coll.get(id).map(|r| (r.path.clone(), r.file_type)) else { return Task::none() };
    select(app, id);
    let viewport = app.quicklook.as_ref().map(|q| q.viewport).unwrap_or(Size::ZERO);
    reveal(app, id, viewport);

    // A viewer window may be loading too; its queue isn't ours to drop.
    if app.viewer.is_none() {
        app.loader.clear_queue();
    }
    if browsing {
        app.loader.request_preview(&path, ft, true);
    } else {
        app.loader.request(&path, ft);
    }
    for j in [i + 1, i.wrapping_sub(1)] {
        let near: Option<(PathBuf, FileType)> = nav.ids.get(j).and_then(|&n| app.coll.get(n)).map(|r| (r.path.clone(), r.file_type));
        if let Some((p, t)) = near {
            app.loader.request_preview(&p, t, false);
        }
    }
    let cached = app.loader.get(&path);

    let Some(q) = app.quicklook.as_mut() else { return Task::none() };
    q.id = id;
    q.hint = i;
    q.key += 1;
    q.path = path;
    q.failed = None;
    q.shown_at = Instant::now();
    (q.preview, q.full) = match cached {
        Some(img) if img.preview => (Some(img), None),
        Some(img) => {
            crate::profile::record(("quick look", "already loaded"), q.shown_at.elapsed());
            (None, Some(img))
        }
        None => (None, None),
    };
    let key = q.key;
    let settle = if browsing && q.full.is_none() {
        Task::perform(tokio::time::sleep(SETTLE), move |_| Message::QuickLook(Msg::Settled(key)))
    } else {
        Task::none()
    };
    Task::batch([allocate(app), settle])
}

fn toggle_flag(app: &mut App, prune: bool) -> Task<Message> {
    let Some(id) = app.quicklook.as_ref().map(|q| q.id) else { return Task::none() };
    // A stacked burst can't be marked, here as in the grid.
    if app.is_stack(id) {
        return app.stack_mark_hint();
    }
    let Some(r) = app.coll.get_mut(id) else { return Task::none() };
    if prune {
        r.is_pruned = !r.is_pruned;
        app.refresh_prune(&[id])
    } else {
        r.is_marked = !r.is_marked;
        app.refresh_mark(&[id])
    }
}

/// Close, then open the viewer on the photo (back in its place in the
/// grid's order if a filter hides it now): Enter opens the photo itself, B
/// its burst's sheet.
/// Enter on quick look: the photo itself, or all the frames of a stacked
/// burst (as in the grid).
fn enter_land(app: &App) -> viewer_burst::Land {
    match app.quicklook.as_ref() {
        Some(q) if app.is_stack(q.id) => viewer_burst::Land::Sheet,
        _ => viewer_burst::Land::Frame,
    }
}

pub fn open_viewer(app: &mut App, land: viewer_burst::Land) -> Task<Message> {
    let Some((id, hint)) = app.quicklook.as_ref().map(|q| (q.id, q.hint)) else { return Task::none() };
    let closed = close(app);
    let mut items = app.derived.visible.clone();
    let index = match items.iter().position(|&x| x == id) {
        Some(i) => i,
        None => {
            let nav = Nav::of(app);
            let next = nav.ids.get(hint).copied();
            let at = next.and_then(|n| items.iter().position(|&x| x == n)).unwrap_or(items.len());
            items.insert(at, id);
            at
        }
    };
    Task::batch([closed, viewer::open_as(app, viewer::Source::Working, items, index, false, land)])
}

// ── keys and messages ───────────────────────────────────────────────────────

/// Keys while quick look is open; `None` passes the key on (commands such
/// as Space itself, which closes it, and every Ctrl shortcut).
pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Option<Task<Message>> {
    let _t = crate::profile::span("quick look", "key");
    if mods.control() || mods.alt() || mods.logo() {
        return None;
    }
    let c = match key {
        Key::Character(c) => Some(c.to_lowercase()),
        _ => None,
    };
    let mv = match (key, c.as_deref()) {
        (Key::Named(Named::ArrowLeft | Named::ArrowUp), _) | (_, Some("a")) => Move::Prev,
        (Key::Named(Named::ArrowRight | Named::ArrowDown), _) | (_, Some("d")) => Move::Next,
        (Key::Named(Named::Home), _) => Move::First,
        (Key::Named(Named::End), _) => Move::Last,
        (Key::Named(Named::PageUp), _) => Move::PrevDay,
        (Key::Named(Named::PageDown), _) => Move::NextDay,
        (Key::Named(Named::Escape), _) => return Some(close(app)),
        (Key::Named(Named::Enter), _) => return Some(open_viewer(app, enter_land(app))),
        (Key::Named(Named::Delete), _) | (_, Some("p")) => return Some(toggle_flag(app, true)),
        (_, Some("m")) => return Some(toggle_flag(app, false)),
        (_, Some("u")) => {
            let pruned = app.quicklook.as_ref().and_then(|q| app.coll.get(q.id)).is_some_and(|r| r.is_pruned);
            return Some(if pruned { toggle_flag(app, true) } else { Task::none() });
        }
        _ => return None,
    };
    Some(step(app, mv))
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Widget(QuickLookEvent::Close) => close(app),
        Msg::Widget(QuickLookEvent::Activate) => open_viewer(app, enter_land(app)),
        Msg::Widget(QuickLookEvent::Viewport(size)) => {
            // (The grid isn't scrolled to the photo yet: it would shift under
            // the fading backdrop.  Stepping and closing do that.)
            let Some(q) = app.quicklook.as_mut() else { return Task::none() };
            q.viewport = size;
            allocate(app)
        }
        Msg::Allocated(path, Ok(allocation)) => {
            app.loader.hold_allocation(&path, allocation);
            Task::none()
        }
        Msg::Allocated(_, Err(_)) => Task::none(),
        Msg::Settled(key) => {
            let Some(q) = app.quicklook.as_ref().filter(|q| q.key == key && q.full.is_none()) else { return Task::none() };
            if let Some(ft) = app.coll.get(q.id).map(|r| r.file_type) {
                let path = q.path.clone();
                app.loader.request(&path, ft);
            }
            Task::none()
        }
    }
}

/// A preview or full image arrived (the loader has it already).
pub fn on_loaded(app: &mut App, path: &Path, error: Option<&str>) -> Task<Message> {
    let Some(q) = app.quicklook.as_mut() else { return Task::none() };
    if q.path != path {
        return Task::none();
    }
    match app.loader.peek(path).cloned() {
        Some(img) if !img.preview => {
            if q.full.as_ref().is_some_and(|f| Arc::ptr_eq(f, &img)) {
                return Task::none();
            }
            crate::profile::record(("quick look", "full image in"), q.shown_at.elapsed());
            q.full = Some(img);
        }
        Some(img) => {
            if q.full.is_some() || q.preview.is_some() {
                return Task::none();
            }
            crate::profile::record(("quick look", "preview in"), q.shown_at.elapsed());
            q.preview = Some(img);
        }
        None => {
            if let Some(e) = error {
                q.failed = Some(format!("Could not load this image: {e}"));
            }
            return Task::none();
        }
    }
    allocate(app)
}

/// Records removed from the collection (trashed, moved out).
pub fn on_removed(app: &mut App, ids: &[RecordId]) {
    if app.quicklook.as_ref().is_some_and(|q| ids.contains(&q.id)) {
        app.quicklook = None;
    }
}

/// Upload the levels the widget will draw ahead of time; the allocations
/// keep them resident while the loader keeps the image.
fn allocate(app: &App) -> Task<Message> {
    let Some(q) = &app.quicklook else { return Task::none() };
    let vp = if q.viewport.width > 0.0 {
        q.viewport
    } else {
        // Not laid out yet: about the grid's size.
        Size::new((app.window_size.width - RAIL_W).max(320.0), (app.window_size.height - 180.0).max(240.0))
    };
    let mut tasks = Vec::new();
    for img in [&q.preview, &q.full].into_iter().flatten() {
        let photo = ql::photo_rect(Rectangle::new(Point::ORIGIN, vp), img.width as f32, img.height as f32);
        let i = img.level_for_scale(photo.width / img.width.max(1) as f32 * app.scale_factor);
        let coarse = (i + 1).min(img.levels.len() - 1);
        for idx in if coarse == i { vec![i] } else { vec![i, coarse] } {
            let path = q.path.clone();
            tasks.push(iimage::allocate(img.levels[idx].handle.clone()).map(move |r| Message::QuickLook(Msg::Allocated(path.clone(), r))));
        }
    }
    Task::batch(tasks)
}

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let name = |id: RecordId| app.coll.get(id).map(|r| r.filename()).unwrap_or_else(|| "?".into());
    let mut sel: Vec<String> = app.derived.visible.iter().filter(|id| app.grid.selection.contains(id)).map(|&id| name(id)).collect();
    sel.truncate(4);
    let grid = format!("grid selection={sel:?} anchor={:?} scroll={:.0}", app.grid.anchor.map(name), app.grid.scroll);
    let Some(q) = &app.quicklook else { return format!("closed; {grid}") };
    let r = app.coll.get(q.id);
    format!(
        "open {} at {:?} preview={} full={} failed={:?} pruned={} marked={} viewport={:?}; {grid}",
        name(q.id),
        position(app, q.id),
        q.preview.as_ref().map(|i| format!("{}x{}", i.levels[0].width, i.levels[0].height)).unwrap_or("-".into()),
        q.full.as_ref().map(|i| format!("{}x{}", i.width, i.height)).unwrap_or("-".into()),
        q.failed,
        r.is_some_and(|r| r.is_pruned),
        r.is_some_and(|r| r.is_marked),
        q.viewport,
    )
}

// ── view ────────────────────────────────────────────────────────────────────

/// (1-based position, count) of `id` among the grid's open days.
fn position(app: &App, id: RecordId) -> Option<(usize, usize)> {
    let open = app.derived.sections.iter().filter(|s| !app.grid.collapsed.contains(&s.date));
    let (mut before, mut found, mut total) = (0, None, 0);
    for s in open {
        if found.is_none() {
            if let Some(i) = s.ids.iter().position(|&x| x == id) {
                found = Some(before + i + 1);
            }
            before += s.ids.len();
        }
        total += s.ids.len();
    }
    found.map(|i| (i, total))
}

pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let q = app.quicklook.as_ref()?;
    let r = app.coll.get(q.id)?;
    let types = if r.is_paired() {
        vec![("RAW", t::RAW), ("JPG", t::JPG)]
    } else {
        match r.file_type {
            FileType::Raw => vec![("RAW", t::RAW)],
            FileType::Jpg => vec![("JPG", t::JPG)],
        }
    };
    let shown = Shown {
        key: q.key,
        thumb: app.thumbs.get(&r.path).filter(|th| th.ok).map(|th| th.levels[0].clone()),
        preview: q.preview.clone(),
        full: q.full.clone(),
        status: q.failed.clone(),
        name: if r.is_paired() { r.stem() } else { r.filename() },
        types,
        position: position(app, q.id),
        when: r.shot_time().format("%Y-%m-%d  %H:%M:%S").to_string(),
        burst: app.bursts.of(q.id).zip(app.bursts.frame_index(q.id)).map(|(b, i)| (i + 1, app.bursts.frames(b).len())),
        hint: crate::ui::hint_plate::phrase(&app.quality.hints, q.id),
        pruned: r.is_pruned,
        marked: r.is_marked,
        crop: r.crop.clone(),
    };
    Some(Widget::new(shown, app.scale_factor, |e| Message::QuickLook(Msg::Widget(e))).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three days: 0–3, 4–5, 6–9 (ids 10.. so they differ from indices).
    fn nav() -> Nav {
        let a = [10, 11, 12, 13];
        let b = [14, 15];
        let c = [16, 17, 18, 19];
        Nav::new([&a[..], &b[..], &[][..], &c[..]])
    }

    #[test]
    fn steps_in_grid_order_and_stops_at_the_ends() {
        let n = nav();
        assert_eq!(n.target(Pos::At(0), Move::Next), Some(1));
        assert_eq!(n.target(Pos::At(3), Move::Next), Some(4), "crosses into the next day");
        assert_eq!(n.target(Pos::At(4), Move::Prev), Some(3));
        assert_eq!(n.target(Pos::At(0), Move::Prev), None);
        assert_eq!(n.target(Pos::At(9), Move::Next), None);
        assert_eq!(n.target(Pos::At(5), Move::First), Some(0));
        assert_eq!(n.target(Pos::At(5), Move::Last), Some(9));
        assert_eq!(n.target(Pos::At(0), Move::First), None, "already there");
        assert_eq!(n.target(Pos::At(9), Move::Last), None);
    }

    #[test]
    fn day_jumps_go_to_a_days_first_photo() {
        let n = nav();
        assert_eq!(n.target(Pos::At(1), Move::NextDay), Some(4));
        assert_eq!(n.target(Pos::At(4), Move::NextDay), Some(6), "the empty day is skipped");
        assert_eq!(n.target(Pos::At(7), Move::NextDay), None);
        // Back: to the start of this day first, then the day before.
        assert_eq!(n.target(Pos::At(8), Move::PrevDay), Some(6));
        assert_eq!(n.target(Pos::At(6), Move::PrevDay), Some(4));
        assert_eq!(n.target(Pos::At(0), Move::PrevDay), None);
    }

    #[test]
    fn a_hidden_photo_browses_from_where_it_was() {
        // Photo 12 (index 2) was pruned and hidden: 13 moved up into index 2.
        let n = Nav::new([&[10, 11, 13][..], &[14, 15][..]]);
        let pos = n.locate(12, 2);
        assert_eq!(pos, Pos::Gap(2));
        assert_eq!(n.target(pos, Move::Next).map(|i| n.ids[i]), Some(13));
        assert_eq!(n.target(pos, Move::Prev).map(|i| n.ids[i]), Some(11));
        assert_eq!(n.target(pos, Move::NextDay).map(|i| n.ids[i]), Some(14));
        assert_eq!(n.target(pos, Move::PrevDay).map(|i| n.ids[i]), Some(10));
        assert_eq!(n.landing(pos).map(|i| n.ids[i]), Some(13));
        // Hidden after the last photo: lands on the new last one.
        let end = n.locate(99, 7);
        assert_eq!(end, Pos::Gap(5));
        assert_eq!(n.target(end, Move::Next), None);
        assert_eq!(n.landing(end).map(|i| n.ids[i]), Some(15));
        // Nothing left at all.
        let empty = Nav::new(std::iter::empty::<&[RecordId]>());
        assert_eq!(empty.landing(empty.locate(1, 0)), None);
        assert_eq!(empty.target(Pos::Gap(0), Move::Next), None);
    }

    #[test]
    fn a_visible_photo_is_found_wherever_its_hint_points() {
        let n = nav();
        assert_eq!(n.locate(17, 0), Pos::At(7));
        assert_eq!(n.landing(Pos::At(7)), Some(7));
    }

    #[test]
    fn opens_on_the_focused_selection() {
        let n = nav();
        let sel: HashSet<RecordId> = [12, 15].into();
        assert_eq!(focus(&n, Some(15), &sel), Some(5), "the anchor, when selected");
        assert_eq!(focus(&n, Some(11), &sel), Some(2), "else the first selected in grid order");
        assert_eq!(focus(&n, Some(11), &HashSet::new()), Some(1), "else the last focused");
        assert_eq!(focus(&n, None, &HashSet::new()), Some(0), "else the first photo");
        assert_eq!(focus(&n, Some(99), &[98].into()), Some(0), "ids not in the grid are skipped");
        let empty = Nav::new(std::iter::empty::<&[RecordId]>());
        assert_eq!(focus(&empty, None, &HashSet::new()), None);
    }

    #[test]
    fn the_grid_scrolls_to_follow() {
        use crate::ui::grid::{Section, reveal_scroll};
        let day = |d: u32, ids: std::ops::Range<RecordId>| Section {
            date: chrono::NaiveDate::from_ymd_opt(2026, 7, d).unwrap(),
            label: String::new(),
            photos: ids.len(),
            ids: ids.collect(),
        };
        let sections = vec![day(1, 0..40), day(2, 40..80)];
        let none = HashSet::new();
        let size = Size::new(1000.0, 600.0);
        // The first photo is in view at the top; a far one needs a scroll down.
        assert_eq!(reveal_scroll(&sections, &none, 200.0, crate::ui::grid::LABEL_H, size, 0.0, 0), None);
        let down = reveal_scroll(&sections, &none, 200.0, crate::ui::grid::LABEL_H, size, 0.0, 70).expect("scrolls");
        assert!(down > 600.0);
        // Already there: nothing to do.  Back up to the first photo: the top.
        assert_eq!(reveal_scroll(&sections, &none, 200.0, crate::ui::grid::LABEL_H, size, down, 70), None);
        let up = reveal_scroll(&sections, &none, 200.0, crate::ui::grid::LABEL_H, size, down, 0).expect("scrolls back");
        assert!(up < 20.0, "to the top, header and all: {up}");
        // A collapsed day's photos aren't drawn; unknown ids neither.
        let collapsed: HashSet<_> = [sections[1].date].into();
        assert_eq!(reveal_scroll(&sections, &collapsed, 200.0, crate::ui::grid::LABEL_H, size, 0.0, 70), None);
        assert_eq!(reveal_scroll(&sections, &none, 200.0, crate::ui::grid::LABEL_H, size, 0.0, 999), None);
    }
}
