//! Bursts in the viewer.  Arriving at a burst from outside it — stepping
//! from the photo before or after, Home / End, a filmstrip click, opening
//! the viewer on one of its frames — shows the burst's **sheet**: every
//! frame as a tile over the photo area (`ui::burst_sheet`), the one arrived
//! on outlined.  From there:
//!
//! * the arrow keys move the outline, Home / End to the first / last frame;
//! * a click, Enter or a zoom in (+, the wheel) zooms into a frame, which
//!   is then the ordinary viewer: ← → step through the burst's frames, and
//!   Esc, a zoom out at fit, B or "All frames" zoom back out to the sheet;
//! * P, M and U mark the outlined frame (advance after marking moves the
//!   outline along the sheet);
//! * Ctrl+click or Space picks frames, and C compares them (the outlined
//!   frame and its neighbours when none are picked) in the compare view,
//!   whose Esc comes back here;
//! * stepping past either end, or PgUp / PgDn, leaves the burst: the photo
//!   before or after it, or that burst's sheet.
//!
//! A burst here is a run of two or more consecutive photos in the viewer's
//! order from one shutter press (`model::bursts`), so a filter that hides
//! some frames shows the rest, and the frames are numbered in their burst.
//! With "Show a burst's frames first" off, arriving shows the frame as
//! before; B (in the grid, the list, quick look or the viewer) still opens
//! the sheet.  Library photos have no bursts.

use std::collections::HashSet;
use std::ops::Range;

use iced::keyboard::{Key, key::Named};
use iced::{Rectangle, Task};

use super::viewer::{self, Source, Viewer};
use super::{App, Message, View};
use crate::imaging::thumbs::{GENERATION_SIZE, PRIORITY_VISIBLE};
use crate::model::bursts::{BurstId, Bursts};
use crate::model::collection::RecordId;
use crate::ui::burst_sheet::{self, Sheet, Tile};
use crate::ui::compare_stage::MAX_PANES;
use crate::ui::image_stage::SheetEvent;

/// Compare from the sheet with nothing picked: the outlined frame and this
/// many around it.
const NEIGHBOURHOOD: usize = 3;
/// Previews fetched for tiles bigger than their thumbnails (the loader
/// keeps a dozen).
const SHEET_PREVIEWS: usize = 12;

/// How the viewer lands on a photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Land {
    /// By the rule: a burst's sheet when arriving from outside the burst
    /// (with the preference on), else the photo.
    Auto,
    /// The burst's sheet, whatever the preference (B).
    Sheet,
    /// The photo itself (Enter in quick look or compare).
    Frame,
}

/// A burst in the viewer's order: its first frame and how many there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunKey {
    first: RecordId,
    len: usize,
}

impl RunKey {
    fn id(self) -> u64 {
        ((self.first as u64) << 32) | self.len as u64
    }
}

/// The viewer's burst state.
#[derive(Debug, Default)]
pub struct SheetUi {
    /// The sheet is on screen (else one frame is).
    pub on: bool,
    /// The burst whose sheet is open: zooming out of one of its frames, or
    /// Esc, goes back to it.
    pub open: Option<RunKey>,
    /// Frames picked to compare.
    pub picked: HashSet<RecordId>,
    /// A word in the top bar about the last thing tried (cleared on the
    /// next move).
    pub note: Option<String>,
}

// ── the rules (pure) ────────────────────────────────────────────────────────

/// The burst run around item `i`: consecutive items from one burst, two or
/// more of them.
pub fn run_at(of: &[Option<BurstId>], i: usize) -> Option<Range<usize>> {
    let b = (*of.get(i)?)?;
    let mut lo = i;
    while lo > 0 && of[lo - 1] == Some(b) {
        lo -= 1;
    }
    let mut hi = i + 1;
    while hi < of.len() && of[hi] == Some(b) {
        hi += 1;
    }
    (hi - lo >= 2).then_some(lo..hi)
}

/// Whether landing on a photo of burst `to` (from a photo of burst `from`,
/// or of none) shows the sheet.
pub fn shows_sheet(land: Land, pref: bool, from: Option<RunKey>, to: Option<RunKey>) -> bool {
    match land {
        Land::Sheet => to.is_some(),
        Land::Frame => false,
        Land::Auto => pref && to.is_some() && from != to,
    }
}

/// A key on the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    First,
    Last,
    /// Out of the burst, backwards (PgUp).
    Back,
    /// Out of the burst, forwards (PgDn).
    Forward,
}

/// What a key on the sheet does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The outline moves to this item.
    Outline(usize),
    /// Leave the burst for this item, going this way.
    Leave(usize, i32),
    Stay,
}

/// The outline at item `at` of `run` (of `len` items), the sheet in `cols`
/// columns: ← → run on through the rows and out of either end, ↑ ↓ keep to
/// their column (↓ from above a short last row lands on its last tile).
pub fn sheet_move(run: &Range<usize>, len: usize, cols: usize, at: usize, mv: Move) -> Outcome {
    let cols = cols.max(1);
    let back = || if run.start > 0 { Outcome::Leave(run.start - 1, -1) } else { Outcome::Stay };
    let forward = || if run.end < len { Outcome::Leave(run.end, 1) } else { Outcome::Stay };
    let k = at.saturating_sub(run.start);
    let n = run.len();
    let to = |k: usize| Outcome::Outline(run.start + k);
    match mv {
        Move::Left if k == 0 => back(),
        Move::Left => to(k - 1),
        Move::Right if k + 1 >= n => forward(),
        Move::Right => to(k + 1),
        Move::Up if k >= cols => to(k - cols),
        Move::Up => Outcome::Stay,
        Move::Down if k + cols < n => to(k + cols),
        // A short last row: from the row above it, onto its last tile.
        Move::Down if k / cols + 1 < n.div_ceil(cols) => to(n - 1),
        Move::Down => Outcome::Stay,
        Move::First if k == 0 => Outcome::Stay,
        Move::First => to(0),
        Move::Last if k + 1 == n => Outcome::Stay,
        Move::Last => to(n - 1),
        Move::Back => back(),
        Move::Forward => forward(),
    }
}

/// The frames to compare from the sheet: the picked ones (with the outlined
/// one if a single other is picked), else the outlined frame and, once the
/// burst is analysed, its sharpest others (`ranked`: the frames by
/// sharpness, best first), else its neighbours; in shooting order.  `Err`
/// holds how many are picked when that is too many.
pub fn compare_set(frames: &[RecordId], outline: usize, picked: &HashSet<RecordId>, ranked: &[RecordId]) -> Result<(Vec<RecordId>, usize), usize> {
    let out = frames.get(outline).copied();
    let chosen: Vec<RecordId> = frames.iter().copied().filter(|id| picked.contains(id) || (picked.len() == 1 && Some(*id) == out)).collect();
    if chosen.len() > MAX_PANES {
        return Err(picked.len());
    }
    if chosen.len() >= 2 {
        let focus = out.and_then(|o| chosen.iter().position(|&x| x == o)).unwrap_or(0);
        return Ok((chosen, focus));
    }
    let n = NEIGHBOURHOOD.min(frames.len());
    if let Some(o) = out {
        let best: Vec<RecordId> = ranked.iter().copied().filter(|&id| id != o && frames.contains(&id)).take(n - 1).collect();
        if best.len() + 1 == n {
            let chosen: Vec<RecordId> = frames.iter().copied().filter(|id| *id == o || best.contains(id)).collect();
            let focus = chosen.iter().position(|&x| x == o).unwrap_or(0);
            return Ok((chosen, focus));
        }
    }
    let start = outline.saturating_sub(1).min(frames.len() - n);
    Ok((frames[start..start + n].to_vec(), outline - start))
}

// ── the viewer's burst ──────────────────────────────────────────────────────

/// Each item's burst (none for library photos, which have no bursts).
pub fn burst_of(bursts: &Bursts, source: &Source, items: &[RecordId]) -> Vec<Option<BurstId>> {
    match source {
        Source::Working => items.iter().map(|&id| bursts.of(id)).collect(),
        Source::Library(_) => vec![None; items.len()],
    }
}

/// The burst the viewer's photo is in, as a range of its items.
pub fn run(v: &Viewer) -> Option<Range<usize>> {
    run_at(&v.burst_of, v.index)
}

fn key_of(v: &Viewer, r: &Range<usize>) -> RunKey {
    RunKey { first: v.items[r.start], len: r.len() }
}

/// The burst of the photo on screen, as a key.
pub fn run_key(v: &Viewer) -> Option<RunKey> {
    run(v).map(|r| key_of(v, &r))
}

/// The sheet is on screen.
pub fn on_sheet(v: &Viewer) -> bool {
    v.sheet.on && run(v).is_some()
}

/// The frame's burst is open: Esc goes back to its sheet.
pub fn sheet_open(v: &Viewer) -> bool {
    v.sheet.open.is_some() && v.sheet.open == run_key(v)
}

/// The viewer lands on item `to`, from a photo of burst `from` (or none):
/// set the sheet up for it.
pub fn arrive(v: &mut Viewer, to: usize, land: Land, from: Option<RunKey>, pref: bool) {
    land_on(&mut v.sheet, &v.items, &v.burst_of, to, land, from, pref);
}

/// [`arrive`] on the parts of the viewer it reads: the sheet shows when
/// [`shows_sheet`] says so; the burst stays open (Esc and zooming out go
/// back to its sheet) while inside it, and opens by itself on arriving with
/// the preference on; leaving it forgets the frames picked.
#[allow(clippy::too_many_arguments)]
fn land_on(ui: &mut SheetUi, items: &[RecordId], of: &[Option<BurstId>], to: usize, land: Land, from: Option<RunKey>, pref: bool) {
    let to_key = run_at(of, to).map(|r| RunKey { first: items[r.start], len: r.len() });
    let sheet = shows_sheet(land, pref, from, to_key);
    if to_key != ui.open {
        ui.picked.clear();
    }
    ui.note = None;
    let open = to_key.is_some() && (sheet || pref || ui.open == to_key);
    ui.open = if open { to_key } else { None };
    ui.on = sheet;
}

/// The viewer's items changed (some left the folder): bursts and the
/// sheet follow.
pub fn items_changed(bursts: &Bursts, v: &mut Viewer) {
    v.burst_of = burst_of(bursts, &v.source, &v.items);
    v.index = v.index.min(v.items.len().saturating_sub(1));
    let key = run_key(v);
    if key.is_none() {
        v.sheet.on = false;
    }
    if v.sheet.open != key {
        v.sheet.open = if v.sheet.on { key } else { None };
    }
}

fn tile_aspect(app: &App, id: RecordId) -> f32 {
    let Some(r) = app.coll.get(id) else { return burst_sheet::DEFAULT_ASPECT };
    if let Some(th) = app.thumbs.get(&r.path).filter(|th| th.ok && th.height() > 0) {
        return th.width() as f32 / th.height() as f32;
    }
    app.loader.peek(&r.path).filter(|img| img.height > 0).map_or(burst_sheet::DEFAULT_ASPECT, |img| img.width as f32 / img.height as f32)
}

/// The tiles' shapes, as the stage lays them out.
fn aspects(app: &App, v: &Viewer, run: &Range<usize>) -> Vec<f32> {
    v.items[run.clone()].iter().map(|&id| tile_aspect(app, id)).collect()
}

/// What the stage shows of the photo's burst (`None` outside bursts).
pub fn sheet(app: &App, v: &Viewer) -> Option<Sheet> {
    let run = run(v)?;
    let tiles: Vec<Tile> = v.items[run.clone()]
        .iter()
        .enumerate()
        .map(|(k, &id)| {
            let r = app.coll.get(id);
            let thumb = r.and_then(|r| app.thumbs.get(&r.path)).cloned();
            let wanted = thumb.is_none() && r.is_some_and(|r| !app.thumbs.is_queued(&r.path));
            Tile {
                image: r.and_then(|r| app.loader.peek(&r.path)).cloned(),
                aspect: tile_aspect(app, id),
                number: app.bursts.frame_index(id).map_or(k + 1, |f| f + 1),
                pruned: r.is_some_and(|r| r.is_pruned),
                marked: r.is_some_and(|r| r.is_marked),
                crop: r.and_then(|r| r.crop.as_ref()).filter(|c| !c.is_full()).map(|c| [c.left as f32, c.top as f32, c.right as f32, c.bottom as f32]),
                picked: v.sheet.picked.contains(&id),
                wanted,
                thumb,
                hint: app.quality.hints.plate(id).filter(|_| r.is_some_and(|r| !r.is_pruned && !r.is_marked)),
            }
        })
        .collect();
    Some(Sheet {
        key: key_of(v, &run).id(),
        tiles: tiles.into(),
        outline: v.index - run.start,
        shown: v.sheet.on,
        picking: !v.sheet.picked.is_empty(),
    })
}

/// The burst as the top bar names it: its frame count, and how many of
/// them show here when a filter hides some.
pub fn title(app: &App, v: &Viewer) -> Option<String> {
    let run = run(v)?;
    let total = app.bursts.of(v.items[run.start]).map_or(run.len(), |b| app.bursts.frames(b).len());
    Some(if total > run.len() { format!("Burst of {total}, {} shown", run.len()) } else { format!("Burst of {total}") })
}

/// "Frame 3 of 13" for the photo on screen.
pub fn frame_caption(app: &App, v: &Viewer) -> Option<String> {
    run(v)?;
    let id = v.record_id()?;
    let b = app.bursts.of(id)?;
    Some(format!("Frame {} of {}", app.bursts.frame_index(id)? + 1, app.bursts.frames(b).len()))
}

/// For the test driver: the sheet's state.
pub fn debug_state(app: &App, v: &Viewer) -> String {
    let name = v.record(app).map(|r| r.filename()).unwrap_or_default();
    let picked: Vec<String> = v.items.iter().filter(|id| v.sheet.picked.contains(id)).filter_map(|&id| app.coll.get(id)).map(|r| r.stem()).collect();
    let cols = run(v).map(|r| burst_sheet::shape(&aspects(app, v, &r), v.stage_size()).0);
    format!(
        "index={} file={name} run={:?} on_sheet={} open={} cols={cols:?} picked={picked:?} note={:?} zoom={:.0}% stage={}x{}",
        v.index,
        run(v),
        on_sheet(v),
        sheet_open(v),
        v.sheet.note,
        v.zoom * 100.0,
        v.stage_size().width,
        v.stage_size().height,
    )
}

// ── moving about ────────────────────────────────────────────────────────────

/// Put the outline on item `to` of the sheet (no slide: the same burst
/// stays on screen).
pub fn set_outline(app: &mut App, to: usize) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if to == v.index || to >= v.items.len() {
        return Task::none();
    }
    v.index = to;
    v.sheet.note = None;
    viewer::show_record(app)
}

/// An arrow key, Home / End or PgUp / PgDn on the sheet.
pub fn move_outline(app: &mut App, mv: Move) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let Some(run) = run(v) else { return Task::none() };
    let cols = burst_sheet::shape(&aspects(app, v, &run), v.stage_size()).0;
    match sheet_move(&run, v.items.len(), cols, v.index, mv) {
        Outcome::Outline(i) => set_outline(app, i),
        Outcome::Leave(i, dir) => viewer::navigate(app, i, dir),
        Outcome::Stay => Task::none(),
    }
}

/// PgUp / PgDn on a frame: out of its burst.
pub fn leave(app: &mut App, dir: i32) -> Task<Message> {
    move_outline(app, if dir < 0 { Move::Back } else { Move::Forward })
}

/// Zoom into item `at` of the sheet.
pub fn zoom_in(app: &mut App, at: usize) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    if !on_sheet(v) || !run(v).is_some_and(|r| r.contains(&at)) {
        return Task::none();
    }
    let t = set_outline(app, at);
    if let Some(v) = app.viewer.as_mut() {
        v.sheet.on = false;
    }
    t
}

/// Zoom out of a frame onto its burst's sheet.
pub fn to_sheet(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    if v.crop_mode || v.sheet.on {
        return Task::none();
    }
    let Some(key) = run_key(v) else {
        let name = v.record(app).map(|r| r.stem()).unwrap_or_default();
        return app.show_status(format!("{name} isn't part of a burst."), 4000);
    };
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if v.sheet.open != Some(key) {
        v.sheet.picked.clear();
    }
    v.sheet.open = Some(key);
    v.sheet.on = true;
    v.sheet.note = None;
    v.flow.cancel();
    want_previews(app);
    Task::none()
}

/// The photo is fitted (or not loaded): zooming out goes to the sheet.
fn at_fit(v: &Viewer) -> bool {
    let Some(img) = &v.image else { return true };
    let vp = v.stage_size();
    let fit = (vp.width / img.width.max(1) as f32).min(vp.height / img.height.max(1) as f32);
    v.zoom <= fit * 1.002
}

/// − (or the zoom-out button): on a burst's frame at fit, back to the
/// sheet; a step that would pass fit stops at fit first.  `None` leaves it
/// to the viewer.
pub fn zoom_out(app: &mut App) -> Option<Task<Message>> {
    let v = app.viewer.as_ref()?;
    if on_sheet(v) {
        return Some(Task::none());
    }
    run(v)?;
    if at_fit(v) {
        return Some(to_sheet(app));
    }
    let img = v.image.as_ref()?;
    let vp = v.stage_size();
    let fit = (vp.width / img.width.max(1) as f32).min(vp.height / img.height.max(1) as f32);
    (v.zoom / 1.2 < fit).then(|| viewer::update(app, viewer::Msg::Fit))
}

/// P or M set a mark: where advance after marking goes.  On the sheet the
/// outline moves along it and stops at its last frame.
pub fn is_last(v: &Viewer) -> bool {
    match run(v).filter(|_| v.sheet.on) {
        Some(r) => v.index + 1 >= r.end,
        None => v.index + 1 >= v.items.len(),
    }
}

/// Advance after marking: the next frame on the sheet, else the next photo.
pub fn advance(app: &mut App, to: usize) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    match run(v).filter(|_| on_sheet(v)) {
        Some(r) if r.contains(&to) => set_outline(app, to),
        Some(_) => Task::none(),
        None => viewer::navigate(app, to, 1),
    }
}

/// A filmstrip click: within the burst on the sheet, the outline moves.
pub fn film_jump(app: &mut App, i: usize) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    if on_sheet(v) && run(v).is_some_and(|r| r.contains(&i)) {
        return set_outline(app, i);
    }
    let dir = if i > v.index { 1 } else { -1 };
    viewer::navigate(app, i, dir)
}

// ── picking and comparing ───────────────────────────────────────────────────

fn toggle_pick(app: &mut App, at: usize) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let Some(&id) = v.items.get(at).filter(|_| run(v).is_some_and(|r| r.contains(&at))) else { return Task::none() };
    if !v.sheet.picked.remove(&id) {
        v.sheet.picked.insert(id);
    }
    v.sheet.note = None;
    Task::none()
}

/// C on the sheet: compare the picked frames (or the outlined one and its
/// neighbours), in the compare view; its Esc comes back to the sheet.
pub fn compare(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let Some(run) = run(v) else { return Task::none() };
    let frames = v.items[run.clone()].to_vec();
    // Measured frames by sharpness (none before an analysis).
    let ranked: Vec<RecordId> = frames.iter().copied().filter(|&id| app.quality.hints.get(id).is_some()).collect();
    let ranked = app.quality.hints.ranked(&ranked);
    match compare_set(&frames, v.index - run.start, &v.sheet.picked, &ranked) {
        Ok((shown, focus)) => super::compare::open_frames(app, frames, shown, focus),
        Err(n) => {
            if let Some(v) = app.viewer.as_mut() {
                v.sheet.note = Some(format!("Compare shows up to {MAX_PANES} frames; {n} are picked"));
            }
            Task::none()
        }
    }
}

// ── the stage's reports ─────────────────────────────────────────────────────

pub fn on_stage(app: &mut App, ev: SheetEvent) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let Some(run) = run(v) else { return Task::none() };
    match ev {
        SheetEvent::Open(k) => zoom_in(app, run.start + k),
        SheetEvent::Pick(k) => toggle_pick(app, run.start + k),
        SheetEvent::Back => to_sheet(app),
        SheetEvent::Thumbs(tiles) => {
            let ids: Vec<RecordId> = tiles.iter().filter_map(|&k| v.items.get(run.start + k).copied()).collect();
            for id in ids {
                if let Some(r) = app.coll.get(id) {
                    app.thumbs.request(&app.workers, r, PRIORITY_VISIBLE);
                }
            }
            Task::none()
        }
    }
}

/// Tiles bigger than their thumbnails show the camera's preview: fetch the
/// burst's (a short burst's tiles are big; a long one's never are).
pub fn want_previews(app: &mut App) {
    let Some(v) = app.viewer.as_ref() else { return };
    let Some(run) = run(v).filter(|_| v.sheet.on) else { return };
    let cells = burst_sheet::layout(&aspects(app, v, &run), Rectangle::with_size(v.stage_size()));
    let biggest = cells.iter().map(|c| c.photo.width.max(c.photo.height)).fold(0.0, f32::max);
    if biggest * app.scale_factor <= GENERATION_SIZE as f32 * 1.15 {
        return;
    }
    let mut order: Vec<usize> = run.clone().collect();
    order.sort_by_key(|&i| i.abs_diff(v.index));
    let files: Vec<_> = order.into_iter().take(SHEET_PREVIEWS).filter_map(|i| app.coll.get(v.items[i]).map(|r| (r.path.clone(), r.file_type))).collect();
    for (path, ft) in files {
        app.loader.request_preview(&path, ft, false);
    }
}

// ── keys ────────────────────────────────────────────────────────────────────

/// Keys for the burst: on the sheet, and on one of its frames.  `None`
/// leaves the key to the viewer (marks, the filmstrip, Esc on the sheet…).
pub fn on_key(app: &mut App, key: &Key, c: Option<&str>) -> Option<Task<Message>> {
    let v = app.viewer.as_ref()?;
    if v.crop_mode || v.read_only {
        return None;
    }
    if on_sheet(v) {
        let mv = match (key, c) {
            (Key::Named(Named::ArrowLeft), _) | (_, Some("a")) => Some(Move::Left),
            (Key::Named(Named::ArrowRight), _) | (_, Some("d")) => Some(Move::Right),
            (Key::Named(Named::ArrowUp), _) => Some(Move::Up),
            (Key::Named(Named::ArrowDown), _) => Some(Move::Down),
            (Key::Named(Named::Home), _) => Some(Move::First),
            (Key::Named(Named::End), _) => Some(Move::Last),
            (Key::Named(Named::PageUp), _) => Some(Move::Back),
            (Key::Named(Named::PageDown), _) => Some(Move::Forward),
            _ => None,
        };
        if let Some(mv) = mv {
            return Some(move_outline(app, mv));
        }
        let at = v.index;
        return match (key, c) {
            (Key::Named(Named::Enter), _) | (_, Some("b")) => Some(zoom_in(app, at)),
            (Key::Named(Named::Space), _) => Some(toggle_pick(app, at)),
            (_, Some("c")) => Some(compare(app)),
            _ => None,
        };
    }
    run(v)?;
    match (key, c) {
        (Key::Named(Named::Escape), _) if sheet_open(v) => Some(to_sheet(app)),
        (_, Some("b")) => Some(to_sheet(app)),
        (Key::Named(Named::PageUp), _) => Some(leave(app, -1)),
        (Key::Named(Named::PageDown), _) => Some(leave(app, 1)),
        _ => None,
    }
}

// ── B in the main window ────────────────────────────────────────────────────

/// The photo B opens the burst of: the one quick look shows, else the
/// grid's or list's selected photo (the keyboard anchor of several).
fn target(app: &App) -> Option<RecordId> {
    if let Some(q) = &app.quicklook {
        return Some(q.id);
    }
    let pick = |anchor: Option<RecordId>, sel: &HashSet<RecordId>| {
        anchor.filter(|a| sel.contains(a)).or_else(|| if sel.len() == 1 { sel.iter().next().copied() } else { None })
    };
    match app.view {
        View::Grid => pick(app.grid.anchor, &app.grid.selection),
        View::List => pick(app.list.current.or(app.list.anchor), &app.list.selection),
        View::Calendar => None,
    }
}

/// The command is available: the selected photo is in a burst.
pub fn can_open(app: &App) -> bool {
    app.view != View::Calendar && target(app).is_some_and(|id| app.bursts.of(id).is_some())
}

/// B: the viewer, on the selected photo's burst sheet.
pub fn open_from_main(app: &mut App) -> Task<Message> {
    if app.view == View::Calendar {
        return Task::none();
    }
    let Some(id) = target(app) else {
        return app.show_status("Select a photo to see its burst.", 4000);
    };
    if app.bursts.of(id).is_none() {
        let name = app.coll.get(id).map(|r| r.stem()).unwrap_or_default();
        return app.show_status(format!("{name} isn't part of a burst."), 4000);
    }
    if app.quicklook.is_some() {
        return super::quicklook::open_viewer(app, Land::Sheet);
    }
    app.open_viewer_working(id, Land::Sheet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(first: RecordId, len: usize) -> Option<RunKey> {
        Some(RunKey { first, len })
    }

    #[test]
    fn a_run_is_two_or_more_consecutive_frames_of_one_burst() {
        let of = [None, Some(0), Some(0), Some(0), Some(1), None, Some(2), Some(2)];
        assert_eq!(run_at(&of, 0), None);
        assert_eq!(run_at(&of, 1), Some(1..4));
        assert_eq!(run_at(&of, 3), Some(1..4));
        // A burst with one frame showing (a filter hides the rest) is none.
        assert_eq!(run_at(&of, 4), None);
        assert_eq!(run_at(&of, 7), Some(6..8));
        assert_eq!(run_at(&of, 8), None);
    }

    #[test]
    fn arriving_from_outside_a_burst_shows_its_sheet() {
        let a = key(10, 5);
        let b = key(20, 3);
        // From a single photo, or another burst: the sheet.
        assert!(shows_sheet(Land::Auto, true, None, a));
        assert!(shows_sheet(Land::Auto, true, b, a));
        // Stepping within the burst: frame by frame.
        assert!(!shows_sheet(Land::Auto, true, a, a));
        // Not a burst: never.
        assert!(!shows_sheet(Land::Auto, true, a, None));
        assert!(!shows_sheet(Land::Sheet, true, None, None));
        // The preference off: only B shows it.
        assert!(!shows_sheet(Land::Auto, false, None, a));
        assert!(shows_sheet(Land::Sheet, false, a, a));
        // Enter in quick look or compare: the photo.
        assert!(!shows_sheet(Land::Frame, true, None, a));
    }

    #[test]
    fn arrows_move_the_outline_in_two_dimensions() {
        // Items 0..20 of 30; the burst is 5..18 (13 frames) in 4 columns:
        // rows 5-8, 9-12, 13-16, 17.
        let run = 5..18;
        let mv = |at, m| sheet_move(&run, 30, 4, at, m);
        assert_eq!(mv(6, Move::Right), Outcome::Outline(7));
        assert_eq!(mv(8, Move::Right), Outcome::Outline(9), "→ runs on to the next row");
        assert_eq!(mv(9, Move::Left), Outcome::Outline(8));
        assert_eq!(mv(10, Move::Down), Outcome::Outline(14));
        assert_eq!(mv(14, Move::Up), Outcome::Outline(10));
        assert_eq!(mv(6, Move::Up), Outcome::Stay, "the top row");
        assert_eq!(mv(17, Move::Down), Outcome::Stay, "the bottom row");
        // Above the short last row, ↓ lands on its last tile.
        assert_eq!(mv(15, Move::Down), Outcome::Outline(17));
        assert_eq!(mv(13, Move::Down), Outcome::Outline(17));
        assert_eq!(mv(11, Move::First), Outcome::Outline(5));
        assert_eq!(mv(11, Move::Last), Outcome::Outline(17));
        assert_eq!(mv(5, Move::First), Outcome::Stay);
    }

    #[test]
    fn past_either_end_leaves_the_burst() {
        let run = 5..18;
        let mv = |at, m| sheet_move(&run, 30, 4, at, m);
        assert_eq!(mv(17, Move::Right), Outcome::Leave(18, 1));
        assert_eq!(mv(5, Move::Left), Outcome::Leave(4, -1));
        assert_eq!(mv(11, Move::Forward), Outcome::Leave(18, 1));
        assert_eq!(mv(11, Move::Back), Outcome::Leave(4, -1));
        // A burst at the very start or end of the viewer has nowhere to go.
        assert_eq!(sheet_move(&(0..4), 4, 2, 0, Move::Left), Outcome::Stay);
        assert_eq!(sheet_move(&(0..4), 4, 2, 3, Move::Right), Outcome::Stay);
        assert_eq!(sheet_move(&(0..4), 4, 2, 2, Move::Forward), Outcome::Stay);
    }

    #[test]
    fn arrive_opens_the_sheet_and_keeps_it_open_while_inside() {
        let of = [None, Some(0), Some(0), Some(0), None, Some(1), Some(1)];
        let items: Vec<RecordId> = (100..107).collect();
        let mut ui = SheetUi::default();
        let mut at = 0;
        let mut go = |ui: &mut SheetUi, to: usize, land: Land, pref: bool| {
            let from = run_at(&of, at).map(|r| RunKey { first: items[r.start], len: r.len() });
            land_on(ui, &items, &of, to, land, from, pref);
            at = to;
        };
        // → from the single photo into the burst: its sheet.
        go(&mut ui, 1, Land::Auto, true);
        assert!(ui.on && ui.open == key(101, 3));
        ui.picked.insert(102);
        // Zoomed into a frame, → steps on inside the burst: still open, the
        // picks kept.
        ui.on = false;
        go(&mut ui, 2, Land::Auto, true);
        assert!(!ui.on && ui.open == key(101, 3) && ui.picked.contains(&102));
        // On past the end to a single photo: closed.
        go(&mut ui, 4, Land::Auto, true);
        assert!(!ui.on && ui.open.is_none() && ui.picked.is_empty());
        // The preference off: the next burst arrives as a frame, not open.
        go(&mut ui, 5, Land::Auto, false);
        assert!(!ui.on && ui.open.is_none());
        // B opens it, and it stays open stepping inside.
        go(&mut ui, 5, Land::Sheet, false);
        assert!(ui.on && ui.open == key(105, 2));
        ui.on = false;
        go(&mut ui, 6, Land::Auto, false);
        assert!(!ui.on && ui.open == key(105, 2));
        // Enter from quick look, preference on: the frame, with its sheet
        // to go back to (opening the viewer counts as arriving from
        // outside).
        land_on(&mut ui, &items, &of, 2, Land::Frame, None, true);
        assert!(!ui.on && ui.open == key(101, 3));
        land_on(&mut ui, &items, &of, 2, Land::Auto, None, true);
        assert!(ui.on, "opening the viewer on a frame shows its sheet");
    }

    #[test]
    fn compare_takes_the_picks_or_the_neighbourhood() {
        let frames: Vec<RecordId> = (1..=8).collect();
        let none = HashSet::new();
        // Nothing picked, not analysed: the outlined frame between its
        // neighbours.
        assert_eq!(compare_set(&frames, 4, &none, &[]), Ok((vec![4, 5, 6], 1)));
        assert_eq!(compare_set(&frames, 0, &none, &[]), Ok((vec![1, 2, 3], 0)));
        assert_eq!(compare_set(&frames, 7, &none, &[]), Ok((vec![6, 7, 8], 2)));
        assert_eq!(compare_set(&frames[..2], 1, &none, &[]), Ok((vec![1, 2], 1)));
        // Picks, in shooting order, the outline's pane focused if picked.
        let picked: HashSet<RecordId> = [7, 2, 5].into_iter().collect();
        assert_eq!(compare_set(&frames, 4, &picked, &[]), Ok((vec![2, 5, 7], 1)));
        assert_eq!(compare_set(&frames, 0, &picked, &[8, 7]), Ok((vec![2, 5, 7], 0)), "picks win over the sharpest");
        // One pick: it and the outlined frame.
        let one: HashSet<RecordId> = [3].into_iter().collect();
        assert_eq!(compare_set(&frames, 6, &one, &[]), Ok((vec![3, 7], 1)));
        // Too many.
        let lots: HashSet<RecordId> = (1..=6).collect();
        assert_eq!(compare_set(&frames, 0, &lots, &[]), Err(6));
    }

    #[test]
    fn once_analysed_compare_suggests_the_sharpest_frames() {
        let frames: Vec<RecordId> = (1..=8).collect();
        let none = HashSet::new();
        // Sharpest first: 6, 2, 7, …  The outlined frame 4 with the two
        // sharpest, in shooting order, the outline focused.
        let ranked = [6, 2, 7, 1, 3, 4, 5, 8];
        assert_eq!(compare_set(&frames, 3, &none, &ranked), Ok((vec![2, 4, 6], 1)));
        // On the sharpest itself: it and the next two.
        assert_eq!(compare_set(&frames, 5, &none, &ranked), Ok((vec![2, 6, 7], 1)));
        // Too few frames measured: the neighbours, as before.
        assert_eq!(compare_set(&frames, 3, &none, &[6]), Ok((vec![3, 4, 5], 1)));
        // Ranked frames from outside the burst don't count.
        assert_eq!(compare_set(&frames[..3], 0, &none, &[9, 3, 2]), Ok((vec![1, 2, 3], 0)));
    }
}
