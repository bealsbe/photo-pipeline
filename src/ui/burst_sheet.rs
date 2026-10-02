//! The burst sheet: every frame of one burst laid out over the viewer's
//! photo area, the largest tiles that fit, in shooting order, on the
//! neutral backdrop.  Each tile wears its marks as the grid draws them, a
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
