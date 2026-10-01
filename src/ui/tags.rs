//! File-type tags (RAW, JPG): a small label cut like a luggage tag — a
//! pointed end with a punched hole — tinted in the type's colour.
//!
//! A RAW+JPG pair wears one two-part tag instead of two tags: the RAW half
//! (blue, with the hole) and the JPG half (peach) joined at a perforation,
//! like a tag with its tear-off stub still attached.  It is about 20% narrower
//! than two tags side by side, which leaves the file name more room.
//!
//! The tag body is rasterised once per size and colour and cached (drawing
//! it as a mesh on every frame would cost more than the photo grid it sits
//! in); the label is ordinary text drawn on top.  Widths are measured once
//! per label, so laying out a grid of thousands of tags costs no text
//! shaping.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::advanced::image::Renderer as _;
use iced::widget::image::{FilterMethod, Handle};
use iced::{Color, Rectangle};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::model::record::{FileType, PhotoRecord};

/// Height of a tag, logical pixels.
pub const TAG_H: f32 = 16.0;
const TEXT_SIZE: f32 = 10.5;
/// Where the label starts: past the point and the hole.
const TEXT_X: f32 = 9.0;
const PAD_RIGHT: f32 = 5.0;
/// Space either side of a pair tag's perforation.
const SEAM_PAD: f32 = 4.0;

/// What a tag names: one file, or both halves of a RAW+JPG pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Raw,
    Jpg,
    Pair,
}

impl Kind {
    pub fn of(rec: &PhotoRecord) -> Kind {
        if rec.is_paired() {
            return Kind::Pair;
        }
        match rec.file_type {
            FileType::Raw => Kind::Raw,
            FileType::Jpg => Kind::Jpg,
        }
    }

    /// From the halves a list row holds.
    pub fn from_halves(raw: bool, jpg: bool) -> Option<Kind> {
        match (raw, jpg) {
            (true, true) => Some(Kind::Pair),
            (true, false) => Some(Kind::Raw),
            (false, true) => Some(Kind::Jpg),
            (false, false) => None,
        }
    }
}

/// A label's measured width (cached: labels are a handful of constants).
fn text_w(label: &str) -> f32 {
    static CACHE: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(w) = cache.lock().ok().and_then(|c| c.get(label).copied()) {
        return w;
    }
    let w = paint::text_width(label, TEXT_SIZE, paint::SEMIBOLD);
    if let Ok(mut c) = cache.lock() {
        c.insert(label.to_string(), w);
    }
    w
}

pub fn width(label: &str) -> f32 {
    (TEXT_X + text_w(label) + PAD_RIGHT).ceil()
}

/// A pair tag's geometry: (total width, perforation x, JPG label x), all
/// from the tag's left edge.
fn pair_geometry() -> (f32, f32, f32) {
    let split = (TEXT_X + text_w("RAW") + SEAM_PAD).round();
    let jpg_x = split + SEAM_PAD;
    ((jpg_x + text_w("JPG") + PAD_RIGHT).ceil(), split, jpg_x)
}

/// Width of the tag for `kind`.
pub fn kind_width(kind: Kind) -> f32 {
    match kind {
        Kind::Raw => width("RAW"),
        Kind::Jpg => width("JPG"),
        Kind::Pair => pair_geometry().0,
    }
}

/// Draw the tag for `kind` with its left edge at `x`, vertically centred on
/// `cy`; returns its width.  Draw it in a layer above any photo, since the
/// body is an image.
pub fn draw_kind(r: &mut Renderer, x: f32, cy: f32, kind: Kind, opacity: f32, clip: Rectangle) -> f32 {
    match kind {
        Kind::Raw => draw(r, x, cy, "RAW", t::RAW, opacity, clip),
        Kind::Jpg => draw(r, x, cy, "JPG", t::JPG, opacity, clip),
        Kind::Pair => draw_pair(r, x, cy, opacity, clip),
    }
}

/// Draw a tag with its left edge at `x`, vertically centred on `cy`.
/// Draw it in a layer above any photo, since the body is an image.
pub fn draw(r: &mut Renderer, x: f32, cy: f32, label: &str, tone: Color, opacity: f32, clip: Rectangle) -> f32 {
    let w = width(label);
    let y = (cy - TAG_H / 2.0).round();
    r.draw_image(
        iced::advanced::image::Image::new(body(w, None, tone, tone)).filter_method(FilterMethod::Linear).opacity(opacity),
        rect(x, y, w, TAG_H),
        clip,
    );
    paint::text(r, label, rect(x + TEXT_X, y, w - TEXT_X, TAG_H), TEXT_SIZE, paint::SEMIBOLD, paint::with_alpha(tone, opacity), HAlign::Left, clip);
    w
}

fn draw_pair(r: &mut Renderer, x: f32, cy: f32, opacity: f32, clip: Rectangle) -> f32 {
    let (w, split, jpg_x) = pair_geometry();
    let y = (cy - TAG_H / 2.0).round();
    r.draw_image(
        iced::advanced::image::Image::new(body(w, Some(split), t::RAW, t::JPG)).filter_method(FilterMethod::Linear).opacity(opacity),
        rect(x, y, w, TAG_H),
        clip,
    );
    let (raw, jpg) = (paint::with_alpha(t::RAW, opacity), paint::with_alpha(t::JPG, opacity));
    paint::text(r, "RAW", rect(x + TEXT_X, y, split - TEXT_X, TAG_H), TEXT_SIZE, paint::SEMIBOLD, raw, HAlign::Left, clip);
    paint::text(r, "JPG", rect(x + jpg_x, y, w - jpg_x, TAG_H), TEXT_SIZE, paint::SEMIBOLD, jpg, HAlign::Left, clip);
    w
}

/// The cached body: one tone, or two split at `split` (logical px).
fn body(w: f32, split: Option<f32>, left: Color, right: Color) -> Handle {
    type Key = (u32, u32, [u8; 4], [u8; 4]);
    static CACHE: OnceLock<Mutex<HashMap<Key, Handle>>> = OnceLock::new();
    let split2 = split.map(|s| (s * 2.0).round() as u32).unwrap_or(0);
    let key = ((w * 2.0).round() as u32, split2, left.into_rgba8(), right.into_rgba8());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(h) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return h;
    }
    let h = render(key.0, (TAG_H * 2.0) as u32, split.map(|_| split2), left, right);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, h.clone());
    }
    h
}

/// The tag outline at 2× (w, h and `split` in device pixels).  Columns left
/// of `split` take the `left` tone, the rest `right`; a split tag also gets
/// a perforated seam there.
fn render(w: u32, h: u32, split: Option<u32>, left: Color, right: Color) -> Handle {
    let mut pm = Pixmap::new(w.max(8), h.max(8)).expect("pixmap");
    let (wf, hf) = (w as f32, h as f32);
    let s = 2.0; // device pixels per logical pixel
    let inset = 0.5 * s;
    let point = 5.0 * s; // length of the pointed end
    let r = 3.0 * s;
    let (x0, y0, x1, y1) = (inset, inset, wf - inset, hf - inset);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + point, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + point, y1);
    pb.line_to(x0, (y0 + y1) / 2.0);
    pb.close();
    // The punched hole (even-odd leaves it empty).
    pb.push_circle(x0 + point * 0.95, (y0 + y1) / 2.0, 1.7 * s);
    let Some(path) = pb.finish() else { return Handle::from_rgba(1, 1, vec![0; 4]) };

    // Drawn in white, then tinted per column below.
    let mut fill = Paint::default();
    fill.set_color_rgba8(255, 255, 255, 46);
    fill.anti_alias = true;
    pm.fill_path(&path, &fill, FillRule::EvenOdd, Transform::identity(), None);
    let mut edge = Paint::default();
    edge.set_color_rgba8(255, 255, 255, 150);
    edge.anti_alias = true;
    let stroke = Stroke { width: 1.0 * s, ..Stroke::default() };
    pm.stroke_path(&path, &edge, &stroke, Transform::identity(), None);
    if let Some(sx) = split {
        // The perforation: short dashes across the tag, inside its edge.
        let mut pb = PathBuilder::new();
        pb.move_to(sx as f32, y0 + 2.5 * s);
        pb.line_to(sx as f32, y1 - 2.5 * s);
        if let Some(seam) = pb.finish() {
            let mut ink = Paint::default();
            ink.set_color_rgba8(255, 255, 255, 120);
            ink.anti_alias = true;
            let dashes = Stroke { width: 1.0 * s, dash: StrokeDash::new(vec![1.5 * s, 1.5 * s], 0.0), ..Stroke::default() };
            pm.stroke_path(&seam, &ink, &dashes, Transform::identity(), None);
        }
    }

    let (l, rt) = (left.into_rgba8(), right.into_rgba8());
    let split = split.unwrap_or(u32::MAX) as usize;
    let mut px = pm.take();
    for (i, p) in px.chunks_exact_mut(4).enumerate() {
        if p[3] == 0 {
            continue;
        }
        // Premultiplied white → straight alpha in the column's tone.
        let tone = if i % w.max(8) as usize >= split { rt } else { l };
        p[..3].copy_from_slice(&tone[..3]);
    }
    Handle::from_rgba(w.max(8), h.max(8), px)
}
