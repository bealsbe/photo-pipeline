//! The burst sheet: every frame of one burst laid out over the viewer's
//! photo area, the largest tiles that fit, in shooting order, on the
//! neutral backdrop.  The tiles can be made bigger, up to one frame a row
//! ([`layout_at`]); the sheet then runs on below the photo area and
//! scrolls.  Each tile wears its marks as the grid draws them, a
//! small frame number (with the analysis' hint beside it: the burst's
//! sharpest frame, or one that looks out of focus), and (while frames are
//! being picked to compare) the grid's select checkbox; the frame the
//! viewer is on is outlined in the accent.
//!
//! This module is geometry and drawing only.  The image stage owns the
//! state (hover, the outline's slide, the zoom between a tile and the whole
//! photo) and decides what to draw; the app builds a [`Sheet`] from its
//! records and thumbnail caches each time the view is built.
//!
//! The zoom is one picture throughout: the frame's photo grows from its
//! tile to the stage ([`map_rect`] carries the whole sheet with it, so the
//! other tiles fly outwards) while the rest of the sheet fades, as the
//! calendar zooms into a day.

use std::sync::Arc;
use std::time::Instant;

use iced::advanced::image::Renderer as _;
use iced::advanced::renderer::Renderer as _;
use iced::widget::image::FilterMethod;
use iced::{Color, Point, Rectangle, Size};

use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::loader::FullImage;
use crate::imaging::thumbs::Thumb;

/// Around the sheet.
pub const MARGIN: f32 = 16.0;
/// Between tiles.
pub const GAP: f32 = 10.0;
/// From a mount's edge to its photo.
pub const PAD: f32 = 4.0;
/// Until a thumbnail says otherwise, a frame is 3:2.
pub const DEFAULT_ASPECT: f32 = 1.5;
/// The sheet's zoom into a frame and back out (as the calendar's zooms).
pub const ZOOM_MS: f32 = 270.0;

/// One frame of the burst.
#[derive(Clone)]
pub struct Tile {
    pub thumb: Option<Arc<Thumb>>,
    /// A sharper picture the loader already holds (the camera's preview or
    /// the full image), for tiles bigger than the thumbnail.
    pub image: Option<Arc<FullImage>>,
    /// Width ÷ height.
    pub aspect: f32,
    /// The frame's number in its burst (1-based).
    pub number: usize,
    pub pruned: bool,
    pub marked: bool,
    /// Left, top, right, bottom as fractions.
    pub crop: Option<[f32; 4]>,
    /// Picked to compare.
    pub picked: bool,
    /// No thumbnail, and none on its way: the stage asks for it.
    pub wanted: bool,
    /// The analysis' hint, beside the frame number (none once decided).
    pub hint: Option<crate::model::hints::Plate>,
}

/// What the stage shows of a burst.
#[derive(Clone)]
pub struct Sheet {
    /// Which burst this is; a new one resets hover and the outline's slide.
    pub key: u64,
    pub tiles: Arc<[Tile]>,
    /// The frame the viewer is on.
    pub outline: usize,
    /// The sheet is on screen; `false` while one frame fills the stage
    /// (the sheet is then what zooming out goes back to).
    pub shown: bool,
    /// Frames are being picked: every tile shows its checkbox.
    pub picking: bool,
    /// The tile size chosen (0: fit; see [`layout_at`]).
    pub size: f32,
}

impl Sheet {
    pub fn aspects(&self) -> Vec<f32> {
        self.tiles.iter().map(|t| t.aspect).collect()
    }
}

// ── layout ──────────────────────────────────────────────────────────────────

/// One tile's place on the sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// The tile's share of the grid.
    pub slot: Rectangle,
    /// The photo, fitted in the slot and centred.
    pub photo: Rectangle,
    /// The mount around the photo.
    pub mount: Rectangle,
}

fn slot_size(area: Size, cols: usize, rows: usize) -> (f32, f32) {
    let (cols, rows) = (cols.max(1) as f32, rows.max(1) as f32);
    let w = (area.width - 2.0 * MARGIN - GAP * (cols - 1.0)) / cols;
    let h = (area.height - 2.0 * MARGIN - GAP * (rows - 1.0)) / rows;
    (w.max(0.0).floor(), h.max(0.0).floor())
}

/// A photo of `aspect` fitted in `r`, centred.
pub fn fit_rect(aspect: f32, r: Rectangle) -> Rectangle {
    let a = aspect.max(0.01);
    let w = r.width.min(r.height * a).max(0.0);
    let h = w / a;
    rect((r.x + (r.width - w) / 2.0).round(), (r.y + (r.height - h) / 2.0).round(), w.round(), h.round())
}

/// Columns and rows: the grid that shows the most photo.  On a tie the
/// wider grid wins.
pub fn shape(aspects: &[f32], area: Size) -> (usize, usize) {
    let n = aspects.len().max(1);
    let mut best = (n, 1);
    let mut best_score = f32::NEG_INFINITY;
    for cols in (1..=n).rev() {
        let rows = n.div_ceil(cols);
        let (w, h) = slot_size(area, cols, rows);
        let photo = rect(0.0, 0.0, (w - 2.0 * PAD).max(0.0), (h - 2.0 * PAD).max(0.0));
        let score: f32 = aspects.iter().map(|&a| fit_rect(a, photo)).map(|r| r.width * r.height).sum();
        if score > best_score * 1.0001 || best_score == f32::NEG_INFINITY {
            best = (cols, rows);
            best_score = score;
        }
    }
    best
}

/// Every tile's place: rows of equal slots in shooting order, the last row
/// left-aligned (columns stay columns for the arrow keys), the whole block
/// centred in `area`.  Slots shrink to the photos they hold, so rows of
/// landscape frames sit together rather than spread over the height.
pub fn layout(aspects: &[f32], area: Rectangle) -> Vec<Cell> {
    let n = aspects.len();
    if n == 0 {
        return Vec::new();
    }
    let (cols, rows) = shape(aspects, area.size());
    let (w, h) = slot_size(area.size(), cols, rows);
    let inner = rect(0.0, 0.0, (w - 2.0 * PAD).max(0.0), (h - 2.0 * PAD).max(0.0));
    let fitted = aspects.iter().map(|&a| fit_rect(a, inner)).fold(Size::ZERO, |m, r| Size::new(m.width.max(r.width), m.height.max(r.height)));
    let (w, h) = ((fitted.width + 2.0 * PAD).min(w), (fitted.height + 2.0 * PAD).min(h));
    let used = cols.min(n) as f32;
    let block = Size::new(used * w + (used - 1.0) * GAP, rows as f32 * h + (rows as f32 - 1.0) * GAP);
    let x0 = area.x + ((area.width - block.width) / 2.0).round();
    let y0 = area.y + ((area.height - block.height) / 2.0).round();
    aspects
        .iter()
        .enumerate()
        .map(|(i, &a)| {
            let (row, col) = ((i / cols) as f32, (i % cols) as f32);
            let slot = rect(x0 + col * (w + GAP), y0 + row * (h + GAP), w, h);
            let photo = fit_rect(a, paint::inset(slot, PAD));
            Cell { slot, photo, mount: paint::inset(photo, -PAD) }
        })
        .collect()
}

/// The tile under `p`.
pub fn hit(cells: &[Cell], p: Point) -> Option<usize> {
    cells.iter().position(|c| c.mount.contains(p))
}

// ── tile size and scrolling ─────────────────────────────────────────────────
//
// The tile size is the long edge of a tile's photo box, in logical pixels.
// The smallest is the fit layout above (every frame on the stage); the
// largest has one frame per row that still fits the stage whole.  Between
// them the tiles keep that size, as many to a row as fit across, and the
// sheet runs on below the stage and scrolls.  The size chosen is the app's
// (kept across bursts); a burst whose fit is bigger shows at its fit.

/// One Ctrl+wheel notch, Ctrl+= or Ctrl+− grows or shrinks the tiles by
/// this much.
pub const SIZE_STEP: f32 = 1.125;
/// Wheel travel in pixels (a trackpad's) that makes one size step.
const PIXELS_PER_STEP: f32 = 60.0;
/// One wheel notch scrolls the sheet this far.
pub const LINE_SCROLL: f32 = 100.0;

/// A burst's tiles laid out at one size.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    /// Every tile's place, with the sheet scrolled to the top.
    pub cells: Vec<Cell>,
    pub cols: usize,
    /// How tall the sheet is: the area's height when it fits.
    pub height: f32,
    /// The tile size shown.
    pub edge: f32,
}

impl Grid {
    /// How far the sheet scrolls in a view `view_h` tall.
    pub fn max_scroll(&self, view_h: f32) -> f32 {
        (self.height - view_h).max(0.0)
    }

    /// The cells with the sheet scrolled down by `scroll`.
    pub fn scrolled(&self, scroll: f32) -> Vec<Cell> {
        let dy = |r: Rectangle| rect(r.x, r.y - scroll, r.width, r.height);
        self.cells.iter().map(|c| Cell { slot: dy(c.slot), photo: dy(c.photo), mount: dy(c.mount) }).collect()
    }
}

/// The widest frame's shape: the photo box every tile gets.
fn box_aspect(aspects: &[f32]) -> f32 {
    let a = aspects.iter().copied().fold(0.0, f32::max);
    if a > 0.01 { a } else { DEFAULT_ASPECT }
}

/// The photo box for tile size `edge`.
fn photo_box(edge: f32, aspect: f32) -> Size {
    if aspect >= 1.0 { Size::new(edge, edge / aspect) } else { Size::new(edge * aspect, edge) }
}

/// The tile sizes a burst can show at on a stage of `area`: its fit (every
/// frame on the stage) and the largest (one frame to a row, whole on the
/// stage), never smaller than fit.
pub fn size_range(aspects: &[f32], area: Size) -> (f32, f32) {
    let fit = fit_edge(&layout(aspects, Rectangle::with_size(area)), box_aspect(aspects));
    (fit, largest_edge(aspects, area).max(fit))
}

fn fit_edge(cells: &[Cell], aspect: f32) -> f32 {
    cells.first().map_or(0.0, |c| if aspect >= 1.0 { c.slot.width } else { c.slot.height } - 2.0 * PAD).max(0.0)
}

fn largest_edge(aspects: &[f32], area: Size) -> f32 {
    let a = box_aspect(aspects);
    let w = (area.width - 2.0 * MARGIN - 2.0 * PAD).max(0.0);
    let h = (area.height - 2.0 * MARGIN - 2.0 * PAD).max(0.0);
    if a >= 1.0 { w.min(h * a) } else { (w / a).min(h) }
}

/// The size a chosen size shows at: never under fit (a chosen size of 0
/// is fit), never over the largest.
pub fn shown_edge(chosen: f32, (fit, max): (f32, f32)) -> f32 {
    if chosen <= fit { fit } else { chosen.min(max) }
}

/// The size chosen after `steps` steps (negative: smaller) from `edge`:
/// 0 (fit) once it gets down to fit, at most the largest.
pub fn step_size(edge: f32, (fit, max): (f32, f32), steps: f32) -> f32 {
    let to = edge * SIZE_STEP.powf(steps);
    if to <= fit * 1.001 || max <= fit * 1.001 { 0.0 } else { to.min(max) }
}

/// Where the size slider's knob sits for `edge` (0 at fit, 1 at the
/// largest; the steps are even along it).
pub fn slider_pos(edge: f32, (fit, max): (f32, f32)) -> f32 {
    if max <= fit * 1.001 || fit <= 0.0 {
        return 0.0;
    }
    ((edge.max(fit) / fit).ln() / (max / fit).ln()).clamp(0.0, 1.0)
}

/// The size chosen with the slider at `pos` (0: fit).
pub fn size_at(pos: f32, (fit, max): (f32, f32)) -> f32 {
    if pos <= 0.001 || max <= fit * 1.001 || fit <= 0.0 { 0.0 } else { fit * (max / fit).powf(pos.min(1.0)) }
}

/// The tiles at `chosen` size in `area`: today's fit layout when the
/// chosen size is fit or less, else rows of tiles that size from the top,
/// as many to a row as fit across, the block centred.
pub fn layout_at(aspects: &[f32], area: Rectangle, chosen: f32) -> Grid {
    let n = aspects.len();
    let fit_cells = layout(aspects, area);
    let a = box_aspect(aspects);
    let fit = fit_edge(&fit_cells, a);
    let max = largest_edge(aspects, area.size()).max(fit);
    let edge = shown_edge(chosen, (fit, max));
    if n == 0 || edge <= fit * 1.0005 {
        // The first row's tiles: the fit's columns.
        let cols = fit_cells.iter().take_while(|c| c.slot.y == fit_cells[0].slot.y).count().max(1);
        return Grid { cells: fit_cells, cols, height: area.height, edge: fit };
    }
    let b = photo_box(edge, a);
    let (w, h) = ((b.width + 2.0 * PAD).round(), (b.height + 2.0 * PAD).round());
    let cols = (((area.width - 2.0 * MARGIN + GAP) / (w + GAP)).floor() as usize).clamp(1, n);
    let rows = n.div_ceil(cols);
    let block = Size::new(cols as f32 * w + (cols as f32 - 1.0) * GAP, rows as f32 * h + (rows as f32 - 1.0) * GAP);
    let height = (block.height + 2.0 * MARGIN).max(area.height);
    let x0 = area.x + ((area.width - block.width) / 2.0).round();
    let y0 = area.y + ((height - block.height) / 2.0).round();
    let cells = aspects
        .iter()
        .enumerate()
        .map(|(i, &asp)| {
            let (row, col) = ((i / cols) as f32, (i % cols) as f32);
            let slot = rect(x0 + col * (w + GAP), y0 + row * (h + GAP), w, h);
            let photo = fit_rect(asp, paint::inset(slot, PAD));
            Cell { slot, photo, mount: paint::inset(photo, -PAD) }
        })
        .collect();
    Grid { cells, cols, height, edge }
}

/// `scroll` kept within the sheet.
pub fn clamp_scroll(grid: &Grid, view_h: f32, scroll: f32) -> f32 {
    scroll.clamp(0.0, grid.max_scroll(view_h))
}

/// The scroll that shows tile `i` whole (its outline included), moving as
/// little as it can; a tile taller than the view shows from its top.
pub fn follow(grid: &Grid, view_h: f32, i: usize, scroll: f32) -> f32 {
    let Some(c) = grid.cells.get(i) else { return clamp_scroll(grid, view_h, scroll) };
    // The sheet's margin beside it, so the first and last rows scroll
    // right to the ends.
    let room = MARGIN;
    let (top, bottom) = (c.mount.y - room, c.mount.y + c.mount.height + room);
    let s = if bottom - top >= view_h || top < scroll {
        top
    } else if bottom > scroll + view_h {
        bottom - view_h
    } else {
        scroll
    };
    clamp_scroll(grid, view_h, s)
}

/// After a change of size: the scroll that keeps the point `frac` of the
/// way down tile `i` at `screen_y` on the stage, as far as the sheet can
/// scroll.
pub fn anchored(grid: &Grid, view_h: f32, i: usize, frac: f32, screen_y: f32) -> f32 {
    let Some(c) = grid.cells.get(i) else { return 0.0 };
    clamp_scroll(grid, view_h, c.mount.y + frac * c.mount.height - screen_y)
}

/// The tiles within `margin` of a view `view_h` tall scrolled to `scroll`.
pub fn visible(grid: &Grid, view_h: f32, scroll: f32, margin: f32) -> std::ops::Range<usize> {
    let (lo, hi) = (scroll - margin, scroll + view_h + margin);
    let shown = |c: &Cell| c.mount.y + c.mount.height > lo && c.mount.y < hi;
    let Some(first) = grid.cells.iter().position(shown) else { return 0..0 };
    let last = grid.cells.iter().rposition(shown).unwrap_or(first);
    first..last + 1
}

/// What the wheel does on the sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wheel {
    /// Ctrl held: the tiles grow (positive) or shrink by this many steps.
    Resize(f32),
    /// Scroll down (positive) or up by this many pixels.
    Scroll(f32),
    /// The whole sheet fits: nothing (the wheel never opens a frame).
    Nothing,
}

/// The wheel on the sheet: `dy` as iced reports it (positive away from
/// the user), in lines (a mouse's notches) or pixels (a trackpad's).
pub fn wheel(dy: f32, lines: bool, ctrl: bool, scrolls: bool) -> Wheel {
    if dy == 0.0 {
        return Wheel::Nothing;
    }
    if ctrl {
        return Wheel::Resize(if lines { dy.signum() } else { dy / PIXELS_PER_STEP });
    }
    if !scrolls {
        return Wheel::Nothing;
    }
    Wheel::Scroll(if lines { -dy * LINE_SCROLL } else { -dy })
}

// ── the zoom ────────────────────────────────────────────────────────────────

/// `r` moved with the sheet when the tile photo `anchor` is drawn at
/// `photo`: the whole sheet scales about the anchor, so the tile becomes
/// the photo and its neighbours fly out past the edges.
pub fn map_rect(r: Rectangle, anchor: Rectangle, photo: Rectangle) -> Rectangle {
    let kx = photo.width / anchor.width.max(1e-3);
    let ky = photo.height / anchor.height.max(1e-3);
    rect(photo.x + (r.x - anchor.x) * kx, photo.y + (r.y - anchor.y) * ky, r.width * kx, r.height * ky)
}

/// The zoom between a tile and the stage at eased progress `e`: where the
/// frame's photo is and how much of the sheet shows.  `from` and `to` are
/// the photo's rectangle and the sheet's opacity at either end, so a zoom
/// that turns round part-way starts from wherever the last one had got to.
pub fn zoom_at(from: (Rectangle, f32), to: (Rectangle, f32), e: f32) -> (Rectangle, f32) {
    (paint::lerp_rect(from.0, to.0, e), paint::lerp(from.1, to.1, e))
}

// ── painting ────────────────────────────────────────────────────────────────

/// How to paint the sheet this frame.
pub struct Look<'a> {
    /// Opacity of everything drawn.
    pub alpha: f32,
    /// Where sheet-space rectangles land on screen (the zoom); `None` at
    /// rest.
    pub zoom: Option<(Rectangle, Rectangle)>,
    /// Each tile's hover highlight (0–1).
    pub hover: &'a dyn Fn(usize) -> f32,
    /// The outline's mount rectangle (sliding between tiles), in sheet
    /// space.
    pub ring: Option<Rectangle>,
    /// A tile whose photo and marks the caller draws itself (the one being
    /// zoomed).
    pub skip: Option<usize>,
    pub scale_factor: f32,
    pub now: Instant,
}

impl Look<'_> {
    fn place(&self, r: Rectangle) -> Rectangle {
        match self.zoom {
            Some((anchor, photo)) => map_rect(r, anchor, photo),
            None => r,
        }
    }
}

/// A thumbnail's fade-in (0–1), from when it arrived.
pub fn arrival(arrived: Instant, now: Instant) -> f32 {
    paint::out_cubic((now.saturating_duration_since(arrived).as_secs_f32() * 1000.0 / t::MOTION_MS).clamp(0.0, 1.0))
}

/// Draw the sheet into `clip`: mounts and photos in one layer, everything
/// over the photos in the next (images draw above every quad in theirs).
pub fn paint(r: &mut Renderer, clip: Rectangle, sheet: &Sheet, cells: &[Cell], look: &Look<'_>) {
    if look.alpha <= 0.002 {
        return;
    }
    let a = |c: Color| paint::with_alpha(c, look.alpha);
    r.with_layer(clip, |r| {
        for (i, (tile, cell)) in sheet.tiles.iter().zip(cells).enumerate() {
            let mount = look.place(cell.mount);
            if mount.intersection(&clip).is_none() {
                continue;
            }
            let base = if i == sheet.outline { t::PHOTO_RAISED } else { t::PHOTO_MOUNT };
            paint::rounded(r, mount, t::R_CELL, a(paint::lerp_color(base, t::PHOTO_RAISED, (look.hover)(i))));
            if look.skip != Some(i) {
                draw_tile_photo(r, clip, tile, look.place(cell.photo), look.alpha, look.scale_factor, look.now);
            }
        }
    });
    r.with_layer(clip, |r| {
        for (i, (tile, cell)) in sheet.tiles.iter().zip(cells).enumerate() {
            if look.skip == Some(i) {
                continue;
            }
            let photo = look.place(cell.photo);
            if photo.intersection(&clip).is_none() {
                continue;
            }
            paint_tile_marks(r, clip, tile, photo, sheet.picking, look.alpha);
        }
        if let Some(ring) = look.ring {
            let ring = look.place(ring);
            paint::framed(r, paint::inset(ring, -3.0), t::R_CELL + 3.0, Color::TRANSPARENT, a(t::SIGNAL), 2.0);
        }
    });
}

/// A tile's picture in `dst`: the sharper image when the tile is bigger
/// than its thumbnail and the loader has one, else the thumbnail (fading in
/// as it arrives), else a quiet placeholder.
pub fn draw_tile_photo(r: &mut Renderer, clip: Rectangle, tile: &Tile, dst: Rectangle, alpha: f32, sf: f32, now: Instant) {
    let px = dst.width.max(dst.height) * sf;
    let thumb_px = tile.thumb.as_ref().filter(|th| th.ok).map_or(0.0, |th| th.width().max(th.height()) as f32);
    if let Some(img) = tile.image.as_ref().filter(|img| !img.levels.is_empty() && px > thumb_px * 1.15) {
        let scale = dst.width / img.width.max(1) as f32;
        let i = img.level_for_scale(scale * sf);
        let coarse = (i + 1).min(img.levels.len() - 1);
        for idx in [coarse, i] {
            let handle = img.levels[idx].handle.clone();
            r.draw_image(iced::advanced::image::Image::new(handle).filter_method(FilterMethod::Linear).opacity(alpha), dst, clip);
            if coarse == i {
                break;
            }
        }
        return;
    }
    match tile.thumb.as_ref().filter(|th| th.ok) {
        Some(th) => {
            let fade = arrival(th.arrived, now);
            let level = th.level_for(px);
            r.draw_image(iced::advanced::image::Image::new(level.handle.clone()).filter_method(FilterMethod::Linear).opacity(alpha * fade), dst, clip);
        }
        None => paint::rounded(r, dst, 2.0, paint::with_alpha(t::PHOTO_GROUND, alpha)),
    }
}

/// A tile's marks (as the grid's), the picking checkbox, and its frame
/// number on the photo's lower left corner.
pub fn paint_tile_marks(r: &mut Renderer, clip: Rectangle, tile: &Tile, photo: Rectangle, picking: bool, alpha: f32) {
    let ts = photo.width.max(photo.height);
    super::grid::paint_marks(r, photo, ts, tile.crop, tile.pruned, tile.marked, picking.then_some(tile.picked), alpha);
    if photo.width < 48.0 || photo.height < 32.0 {
        return;
    }
    let label = tile.number.to_string();
    let tw = paint::text_width(&label, t::T_SMALL, paint::MEDIUM).ceil();
    let h = 18.0;
    // The hint's glyph follows the number in the same pill.
    let g = 12.0;
    let extra = if tile.hint.is_some() { g + 4.0 } else { 0.0 };
    let chip = rect(photo.x + 6.0, photo.y + photo.height - h - 6.0, (tw + 12.0).max(h) + extra, h);
    paint::rounded(r, chip, h / 2.0, paint::with_alpha(t::alpha(Color::BLACK, 0.6), alpha));
    let text_clip = chip.intersection(&clip).unwrap_or_default();
    let num = rect(chip.x, chip.y, chip.width - extra, h);
    paint::text(r, &label, num, t::T_SMALL, paint::MEDIUM, paint::with_alpha(t::PHOTO_INK, alpha), HAlign::Center, text_clip);
    if let Some(plate) = tile.hint {
        let at = rect((num.x + num.width - 2.0).round(), (chip.center_y() - g / 2.0).round(), g, g);
        let handle = super::icons::handle(super::hint_plate::glyph(plate), g, super::hint_plate::tone(plate));
        r.draw_image(iced::advanced::image::Image::new(handle).filter_method(FilterMethod::Linear).opacity(alpha), at, text_clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: f32 = 1.5;

    fn area(w: f32, h: f32) -> Rectangle {
        Rectangle::new(Point::new(20.0, 50.0), Size::new(w, h))
    }

    fn check(cells: &[Cell], a: Rectangle) {
        for (i, c) in cells.iter().enumerate() {
            assert!(c.mount.x >= a.x - 0.5 && c.mount.y >= a.y - 0.5, "tile {i} starts outside: {c:?}");
            assert!(c.mount.x + c.mount.width <= a.x + a.width + 0.5, "tile {i} overruns the right edge");
            assert!(c.mount.y + c.mount.height <= a.y + a.height + 0.5, "tile {i} overruns the bottom");
            assert!(c.slot.contains(c.photo.center()));
            for (j, d) in cells.iter().enumerate().skip(i + 1) {
                assert!(c.mount.intersection(&d.mount).is_none_or(|x| x.width * x.height < 1.0), "tiles {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn tiles_fill_the_area_in_shooting_order() {
        let a = area(1100.0, 640.0);
        for n in [2usize, 3, 5, 6, 13, 20, 30, 46] {
            let aspects = vec![LAND; n];
            let cells = layout(&aspects, a);
            assert_eq!(cells.len(), n);
            check(&cells, a);
            let (cols, rows) = shape(&aspects, a.size());
            assert!(cols * rows >= n && cols * (rows - 1) < n, "{n} tiles in {cols} × {rows} leave a row empty");
            // Row-major: left to right, then down.
            for i in 1..n {
                let (p, c) = (cells[i - 1].slot, cells[i].slot);
                if i % cols == 0 {
                    assert!(c.y > p.y && c.x < p.x, "tile {i} should start a new row");
                } else {
                    assert!(c.y == p.y && c.x > p.x, "tile {i} should follow on its row");
                }
            }
        }
    }

    #[test]
    fn the_grid_is_the_one_with_the_biggest_tiles() {
        // Two landscape frames in a wide stage sit side by side; in a tall
        // one, one above the other.
        assert_eq!(shape(&[LAND, LAND], Size::new(1400.0, 600.0)), (2, 1));
        assert_eq!(shape(&[LAND, LAND], Size::new(700.0, 1200.0)), (1, 2));
        // Thirteen 3:2 frames on a 16:10 stage: four columns of four rows.
        assert_eq!(shape(&[LAND; 13], Size::new(1100.0, 690.0)), (4, 4));
        // Whatever shape wins, no other gives more photo.
        let aspects = [LAND; 20];
        let size = Size::new(1300.0, 720.0);
        let (cols, rows) = shape(&aspects, size);
        let area_of = |c: usize, r: usize| {
            let (w, h) = slot_size(size, c, r);
            let f = fit_rect(LAND, rect(0.0, 0.0, w - 2.0 * PAD, h - 2.0 * PAD));
            f.width * f.height
        };
        for c in 1..=20 {
            assert!(area_of(cols, rows) >= area_of(c, 20usize.div_ceil(c)) - 1.0);
        }
    }

    #[test]
    fn a_short_last_row_keeps_the_columns() {
        let a = area(1000.0, 700.0);
        let cells = layout(&[LAND; 5], a);
        let (cols, _) = shape(&[LAND; 5], a.size());
        assert!(cols < 5);
        assert_eq!(cells[cols].slot.x, cells[0].slot.x, "the last row starts under the first column");
    }

    #[test]
    fn portrait_frames_fit_their_slots() {
        let a = area(1000.0, 700.0);
        let aspects = [LAND, 2.0 / 3.0, LAND, 2.0 / 3.0];
        let cells = layout(&aspects, a);
        check(&cells, a);
        for (c, &asp) in cells.iter().zip(&aspects) {
            assert!((c.photo.width / c.photo.height - asp).abs() < 0.02);
        }
    }

    #[test]
    fn hit_finds_the_tile_under_the_pointer() {
        let a = area(1000.0, 700.0);
        let cells = layout(&[LAND; 6], a);
        for (i, c) in cells.iter().enumerate() {
            assert_eq!(hit(&cells, c.photo.center()), Some(i));
        }
        assert_eq!(hit(&cells, Point::new(a.x + 1.0, a.y + 1.0)), None, "the margin is no tile");
    }

    #[test]
    fn the_zoom_carries_the_tile_onto_the_stage() {
        let a = area(1200.0, 700.0);
        let cells = layout(&[LAND; 9], a);
        let anchor = cells[4].photo;
        let fit = fit_rect(LAND, a);
        // At rest the sheet is where it is laid out.
        assert_eq!(map_rect(cells[0].mount, anchor, anchor), cells[0].mount);
        // Fully zoomed, the tile's photo is the fitted photo.
        let m = map_rect(anchor, anchor, fit);
        assert!((m.x - fit.x).abs() < 0.01 && (m.width - fit.width).abs() < 0.01 && (m.height - fit.height).abs() < 0.01);
        // Its neighbours keep their places relative to it, scaled up: the
        // one to the left ends up beside it, mostly past the stage's edge.
        let left = map_rect(cells[3].photo, anchor, fit);
        assert!(left.x + left.width < fit.x && left.x < a.x - left.width * 0.8, "{left:?}");
        let k = fit.width / anchor.width;
        assert!((left.width - cells[3].photo.width * k).abs() < 0.01);
    }

    #[test]
    fn a_zoom_that_turns_round_starts_where_the_last_one_was() {
        let tile = rect(100.0, 100.0, 150.0, 100.0);
        let fit = rect(0.0, 0.0, 1050.0, 700.0);
        // Zooming in: photo from tile to fit, the sheet fading out.
        assert_eq!(zoom_at((tile, 1.0), (fit, 0.0), 0.0), (tile, 1.0));
        assert_eq!(zoom_at((tile, 1.0), (fit, 0.0), 1.0), (fit, 0.0));
        let (mid, a) = zoom_at((tile, 1.0), (fit, 0.0), 0.4);
        assert!(mid.width > tile.width && mid.width < fit.width && a > 0.0 && a < 1.0);
        // Turning round part-way: the next zoom starts from that frame.
        assert_eq!(zoom_at((mid, a), (tile, 1.0), 0.0), (mid, a));
        assert_eq!(zoom_at((mid, a), (tile, 1.0), 1.0), (tile, 1.0));
    }

    /// The stage's own coordinates, as the image stage lays the sheet out.
    fn stage(w: f32, h: f32) -> Rectangle {
        Rectangle::with_size(Size::new(w, h))
    }

    #[test]
    fn tile_sizes_run_from_fit_to_one_frame_a_row() {
        let a = stage(1100.0, 640.0);
        let aspects = [LAND; 46];
        let range @ (fit, max) = size_range(&aspects, a.size());
        // Fit is today's layout; the largest is one 3:2 frame that fits the
        // stage whole (here the height limits it: 600 px tall).
        assert!((fit - (layout(&aspects, a)[0].slot.width - 2.0 * PAD)).abs() < 0.01);
        assert!((max - (640.0 - 2.0 * MARGIN - 2.0 * PAD) * LAND).abs() < 0.01, "{max}");
        assert!(fit < max / 4.0);
        // Up from fit, step by step, smoothly, to the largest, and no
        // further.
        let mut edge = fit;
        let mut steps = 0;
        loop {
            let chosen = step_size(edge, range, 1.0);
            let next = shown_edge(chosen, range);
            assert!(next >= edge && next / edge <= SIZE_STEP + 1e-3, "a step from {edge} to {next} jumps");
            if next == edge {
                break;
            }
            edge = next;
            steps += 1;
        }
        assert_eq!(edge, max);
        assert!((10..30).contains(&steps), "{steps} steps from fit to the largest");
        assert_eq!(step_size(max, range, 1.0), max, "the largest stays the largest");
        assert_eq!(step_size(max, range, 3.0), max);
        // Down again, and back to fit: chosen 0 means fit (today's sheet).
        let mut chosen = max;
        for _ in 0..100 {
            chosen = step_size(shown_edge(chosen, range), range, -1.0);
        }
        assert_eq!(chosen, 0.0);
        assert_eq!(shown_edge(0.0, range), fit);
        assert_eq!(step_size(fit * 1.05, range, -1.0), 0.0, "a step past fit lands on fit");
        // A size chosen on a longer burst never shows a shorter one smaller
        // than its own fit, and never past the largest.
        assert_eq!(shown_edge(fit * 0.5, range), fit);
        assert_eq!(shown_edge(max * 3.0, range), max);
        // A short burst already as big as it gets has nowhere to go.
        let two = size_range(&[LAND; 2], Size::new(700.0, 1200.0));
        assert_eq!(step_size(two.0, two, 1.0), 0.0);
        assert_eq!(slider_pos(two.0, two), 0.0);
    }

    #[test]
    fn the_slider_runs_evenly_from_fit_to_the_largest() {
        let range = (150.0, 1200.0);
        assert_eq!(slider_pos(150.0, range), 0.0);
        assert_eq!(slider_pos(1200.0, range), 1.0);
        assert_eq!(slider_pos(100.0, range), 0.0);
        assert_eq!(size_at(0.0, range), 0.0, "the slider's start is fit");
        for pos in [0.1f32, 0.37, 0.5, 0.9, 1.0] {
            let size = size_at(pos, range);
            assert!((slider_pos(size, range) - pos).abs() < 1e-4);
        }
        // Even: each tenth of the way multiplies the size by the same.
        let r1 = size_at(0.2, range) / size_at(0.1, range);
        let r2 = size_at(0.9, range) / size_at(0.8, range);
        assert!((r1 - r2).abs() < 1e-3);
    }

    #[test]
    fn bigger_tiles_run_on_below_the_stage() {
        let a = stage(1100.0, 640.0);
        let aspects = [LAND; 46];
        let range @ (fit, max) = size_range(&aspects, a.size());
        // At fit (or less): exactly today's layout, all on the stage.
        let g = layout_at(&aspects, a, 0.0);
        assert_eq!(g.cells, layout(&aspects, a));
        assert_eq!((g.height, g.edge), (a.height, fit));
        assert_eq!(g.cols, shape(&aspects, a.size()).0);
        assert_eq!(layout_at(&aspects, a, fit * 0.7), g);
        // Bigger: tiles that size, as many to a row as fit across, rows in
        // shooting order, the sheet taller than the stage.
        let mid = fit * 2.5;
        let g = layout_at(&aspects, a, mid);
        assert_eq!(g.edge, mid);
        assert!(g.height > a.height && g.max_scroll(a.height) > 0.0);
        for (i, c) in g.cells.iter().enumerate() {
            assert!((c.photo.width - mid).abs() <= 1.0, "tile {i} is {} wide", c.photo.width);
            assert!(c.mount.x >= a.x && c.mount.x + c.mount.width <= a.x + a.width, "tile {i} overruns the sides");
            assert!(c.mount.y >= 0.0 && c.mount.y + c.mount.height <= g.height);
            if i % g.cols != 0 {
                assert!(c.slot.y == g.cells[i - 1].slot.y && c.slot.x > g.cells[i - 1].slot.x);
            } else if i > 0 {
                assert!(c.slot.y > g.cells[i - 1].slot.y);
            }
        }
        let used = g.cols as f32 * g.cells[0].slot.width + (g.cols as f32 - 1.0) * GAP;
        assert!(used <= a.width - 2.0 * MARGIN && used + g.cells[0].slot.width + GAP > a.width - 2.0 * MARGIN, "as many to a row as fit");
        // The largest: one frame a row, whole on the stage.
        let g = layout_at(&aspects, a, max);
        assert_eq!(g.cols, 1);
        assert!(g.cells[0].mount.height <= a.height - 2.0 * MARGIN + 0.5);
        assert_eq!(layout_at(&aspects, a, max * 2.0).edge, max);
        // A wide stage limits by height too: still whole, never cut off.
        let wide = stage(2400.0, 700.0);
        let (_, wmax) = size_range(&aspects, wide.size());
        assert!(layout_at(&aspects, wide, wmax).cells[0].mount.height <= 700.0 - 2.0 * MARGIN + 0.5);
        let _ = range;
    }

    #[test]
    fn a_new_size_keeps_the_tile_in_place() {
        let a = stage(1100.0, 640.0);
        let aspects = [LAND; 60];
        let (fit, _) = size_range(&aspects, a.size());
        let before = layout_at(&aspects, a, fit * 2.0);
        // Tile 30 is part-way down the view, the pointer a third of the way
        // down it.
        let scroll = clamp_scroll(&before, a.height, before.cells[30].mount.y - 200.0);
        let m = before.cells[30].mount;
        let (frac, at) = (1.0 / 3.0, m.y + m.height / 3.0 - scroll);
        let after = layout_at(&aspects, a, fit * 2.0 * SIZE_STEP);
        let s = anchored(&after, a.height, 30, frac, at);
        let m2 = after.cells[30].mount;
        assert!((m2.y + frac * m2.height - s - at).abs() < 0.01, "the point under the pointer stays put");
        // Near the top the sheet can't scroll far enough: as far as it can.
        assert_eq!(anchored(&after, a.height, 0, 0.5, 600.0), 0.0);
        // Near the end, likewise, never past the end.
        assert_eq!(anchored(&after, a.height, 59, 0.5, 0.0), after.max_scroll(a.height));
        // From fit to the first size up: the outlined tile stays near where
        // it was.
        let fitted = layout_at(&aspects, a, 0.0);
        let y = fitted.cells[25].mount.center_y();
        let up = layout_at(&aspects, a, step_size(fit, (fit, 9999.0), 1.0));
        let s = anchored(&up, a.height, 25, 0.5, y);
        assert!((up.cells[25].mount.center_y() - s - y).abs() < 0.5);
    }

    #[test]
    fn the_view_follows_the_outline() {
        let a = stage(1100.0, 640.0);
        let aspects = [LAND; 60];
        let (fit, _) = size_range(&aspects, a.size());
        let g = layout_at(&aspects, a, fit * 3.0);
        let h = a.height;
        let shows = |i: usize, s: f32| {
            let m = g.cells[i].mount;
            m.y >= s && m.y + m.height <= s + h
        };
        // On screen already: no move.
        assert_eq!(follow(&g, h, 0, 0.0), 0.0);
        // Below the view: just enough to show it, its outline included.
        let last = g.cells.len() - 1;
        let s = follow(&g, h, last, 0.0);
        assert!(shows(last, s) && s <= g.max_scroll(h));
        // Above the view: up to it.
        let s2 = follow(&g, h, 0, s);
        assert!(shows(0, s2) && s2 == 0.0);
        // At the largest size on a short stage (two a row there: the
        // height limits it), each tile still shows whole.
        let big = layout_at(&aspects, stage(1100.0, 300.0), 9999.0);
        let s3 = follow(&big, 300.0, 5, 0.0);
        let m = big.cells[5].mount;
        assert!(m.y >= s3 && m.y + m.height <= s3 + 300.0);
    }

    #[test]
    fn only_the_tiles_near_the_view_are_shown() {
        let a = stage(1100.0, 640.0);
        let aspects = [LAND; 100];
        let (fit, _) = size_range(&aspects, a.size());
        let g = layout_at(&aspects, a, fit * 3.0);
        let on = visible(&g, a.height, 0.0, 0.0);
        assert_eq!(on.start, 0);
        assert!(on.len() < 20 && on.len() >= g.cols, "{on:?}");
        let near = visible(&g, a.height, 0.0, a.height);
        assert!(near.start == 0 && near.end > on.end && near.len() < 50);
        // Scrolled down: the window moves along.
        let mid = visible(&g, a.height, 2000.0, 0.0);
        assert!(mid.start > on.end);
        for i in mid.clone() {
            let m = g.cells[i].mount;
            assert!(m.y + m.height > 2000.0 && m.y < 2000.0 + a.height);
        }
    }

    #[test]
    fn the_wheel_scrolls_and_ctrl_wheel_sizes() {
        // Ctrl: a notch is a step; a trackpad's pixels a share of one.
        assert_eq!(wheel(1.0, true, true, false), Wheel::Resize(1.0));
        assert_eq!(wheel(-1.0, true, true, true), Wheel::Resize(-1.0));
        assert_eq!(wheel(3.0, true, true, true), Wheel::Resize(1.0), "a notch is one step, however it's reported");
        assert_eq!(wheel(30.0, false, true, true), Wheel::Resize(0.5));
        // Plain: scrolls a sheet bigger than the stage (down for a turn
        // towards the user)...
        assert_eq!(wheel(-1.0, true, false, true), Wheel::Scroll(LINE_SCROLL));
        assert_eq!(wheel(1.0, true, false, true), Wheel::Scroll(-LINE_SCROLL));
        assert_eq!(wheel(-12.0, false, false, true), Wheel::Scroll(12.0));
        // ...and does nothing on one that fits: never opens a frame.
        assert_eq!(wheel(1.0, true, false, false), Wheel::Nothing);
        assert_eq!(wheel(-1.0, true, false, false), Wheel::Nothing);
        assert_eq!(wheel(0.0, true, true, true), Wheel::Nothing);
    }

    #[test]
    fn fit_rect_centres_the_photo() {
        let a = area(1000.0, 500.0);
        let f = fit_rect(LAND, a);
        assert_eq!((f.width, f.height), (750.0, 500.0));
        assert_eq!(f.x - a.x, 125.0);
        let p = fit_rect(2.0 / 3.0, a);
        assert!((p.height - 500.0).abs() < 0.5 && (p.center_x() - a.center_x()).abs() < 0.6);
    }
}
