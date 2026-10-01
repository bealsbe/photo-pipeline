//! The compare view's stage: two to four photos side by side, one linked
//! zoom and pan, a focus ring, and each photo's marks.
//!
//! The view is linked: every pane shows the same scale (1.0 = 100 %)
//! centred on the same point, given as a fraction of each photo, so 100 %
//! on one photo shows the same spot on all of them.  Fit is per pane.  As
//! with the viewer's stage, this widget owns the transform and its
//! animation; the app sends commands (fit, 100 %, zoom) and the focus, and
//! hears back which pane was clicked.
//!
//! The layout picks the grid (a row, a column or 2 × 2) that shows the most
//! photo for the window and the photos' shapes.  Each photo sits in a mount
//! with its name underneath, as in the grid; the focused one wears the
//! accent ring, which slides when focus moves.

use std::sync::Arc;
use std::time::Instant;

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse as amouse};
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Vector, window};

use super::image_stage::ViewCmd;
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::loader::FullImage;

/// The most photos compared at once.
pub const MAX_PANES: usize = 4;
/// Around the panes, and between them.
const MARGIN: f32 = 8.0;
const GAP: f32 = 8.0;
/// Mount edge to photo.
const PAD: f32 = 4.0;
/// The name strip under each photo.
const CAPTION_H: f32 = 26.0;
const MAX_SCALE: f32 = 32.0;
const STEP: f32 = 1.2;
const ZOOM_MS: f32 = 260.0;

// ── layout ──────────────────────────────────────────────────────────────────

/// One pane's place on the stage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// All of the pane: its photo's viewport and the caption strip below.
    pub slot: Rectangle,
    /// Where the photo is drawn (its viewport).
    pub photo: Rectangle,
    /// Where a photo smaller than the viewport sits: towards the middle of
    /// the stage, so fitted photos sit together rather than each floating
    /// in its own half.
    pub align: (f32, f32),
}

impl Cell {
    pub fn port(&self) -> Port {
        Port { size: self.photo.size(), align: self.align }
    }

    /// The mount around the part of the photo in view: it hugs a fitted
    /// photo, as the grid's mounts do, and grows to the whole pane as the
    /// photo is zoomed in.
    fn mount(&self, visible: Rectangle) -> Rectangle {
        rect(visible.x - PAD, visible.y - PAD, visible.width + 2.0 * PAD, visible.height + PAD + CAPTION_H)
    }
}

/// A photo of `aspect` fitted in a cell's viewport, aligned as it will be.
fn fitted(aspect: f32, cell: &Cell) -> Rectangle {
    let r = cell.photo;
    let a = aspect.max(0.01);
    let w = r.width.min(r.height * a);
    let h = w / a;
    rect((r.x + (r.width - w) * cell.align.0).round(), (r.y + (r.height - h) * cell.align.1).round(), w.round(), h.round())
}

/// Towards the middle of a run of `n`: the first leans forward, the last
/// back, the rest (and a lone one) centre.
fn lean(i: usize, n: usize) -> f32 {
    if n <= 1 || (i > 0 && i + 1 < n) {
        0.5
    } else if i == 0 {
        1.0
    } else {
        0.0
    }
}

/// The photo viewport of each pane in a `cols` × `rows` grid over `area`.
fn photo_size(area: Size, cols: usize, rows: usize) -> Size {
    let (cw, ch) = cell_size(area, cols, rows);
    Size::new((cw - 2.0 * PAD).max(0.0), (ch - PAD - CAPTION_H).max(0.0))
}

fn cell_size(area: Size, cols: usize, rows: usize) -> (f32, f32) {
    let cw = (area.width - 2.0 * MARGIN - GAP * (cols.max(1) - 1) as f32) / cols.max(1) as f32;
    let ch = (area.height - 2.0 * MARGIN - GAP * (rows.max(1) - 1) as f32) / rows.max(1) as f32;
    (cw.max(0.0).floor(), ch.max(0.0).floor())
}

/// How much of a viewport a photo of `aspect` (width ÷ height) fills when
/// fitted, in square pixels.
fn fitted_area(aspect: f32, vp: Size) -> f32 {
    if vp.width <= 0.0 || vp.height <= 0.0 || aspect <= 0.0 {
        return 0.0;
    }
    let w = vp.width.min(vp.height * aspect);
    w * w / aspect
}

/// Columns and rows for photos of `aspects` in `area`: whichever grid shows
/// the most photo.  On a tie the wider grid wins.
pub fn shape(aspects: &[f32], area: Size) -> (usize, usize) {
    let n = aspects.len().max(1);
    let mut best = (n, 1);
    let mut best_score = f32::NEG_INFINITY;
    for cols in (1..=n).rev() {
        let rows = n.div_ceil(cols);
        let vp = photo_size(area, cols, rows);
        let score: f32 = aspects.iter().map(|&a| fitted_area(a, vp)).sum();
        if score > best_score * 1.0001 || best_score == f32::NEG_INFINITY {
            best = (cols, rows);
            best_score = score;
        }
    }
    best
}

/// Where each photo goes.  Every pane is the same size (so a linked pan
/// moves them all alike); a short last row is centred.
pub fn layout(aspects: &[f32], area: Rectangle) -> Vec<Cell> {
    let n = aspects.len();
    if n == 0 {
        return Vec::new();
    }
    let (cols, rows) = shape(aspects, area.size());
    let (cw, ch) = cell_size(area.size(), cols, rows);
    (0..n)
        .map(|i| {
            let (row, col) = (i / cols, i % cols);
            let in_row = if row + 1 == rows { n - row * cols } else { cols };
            let row_w = in_row as f32 * cw + (in_row - 1) as f32 * GAP;
            let x = area.x + ((area.width - row_w) / 2.0).round() + col as f32 * (cw + GAP);
            let y = area.y + MARGIN + row as f32 * (ch + GAP);
            let slot = rect(x, y, cw, ch);
            let photo = rect(x + PAD, y + PAD, cw - 2.0 * PAD, ch - PAD - CAPTION_H);
            Cell { slot, photo, align: (lean(col, in_row), lean(row, rows)) }
        })
        .collect()
}

/// The pane an arrow key moves focus to: the nearest one whose centre lies
/// that way, favouring ones in line.  `None` at the edge.
pub fn neighbour(cells: &[Cell], from: usize, dx: i32, dy: i32) -> Option<usize> {
    let c0 = cells.get(from)?.slot.center();
    let mut best: Option<(f32, usize)> = None;
    for (i, c) in cells.iter().enumerate() {
        if i == from {
            continue;
        }
        let p = c.slot.center();
        let (along, across) = if dx != 0 { ((p.x - c0.x) * dx as f32, (p.y - c0.y).abs()) } else { ((p.y - c0.y) * dy as f32, (p.x - c0.x).abs()) };
        if along <= 1.0 {
            continue;
        }
        let score = along + 2.0 * across;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, i));
        }
    }
    best.map(|(_, i)| i)
}

// ── the linked view ─────────────────────────────────────────────────────────

/// The view every pane shares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Linked {
    /// Each photo fitted to its pane (the other fields are ignored).
    pub fit: bool,
    /// Screen pixels per photo pixel: 1.0 is 100 %.
    pub scale: f32,
    /// The photo point at the pane's centre, as a fraction of the photo.
    pub center: (f32, f32),
}

pub const FIT: Linked = Linked { fit: true, scale: 1.0, center: (0.5, 0.5) };

/// A pane's photo viewport: its size, and where a photo smaller than it
/// sits on each axis (0 left / top, ½ centred, 1 right / bottom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Port {
    pub size: Size,
    pub align: (f32, f32),
}

pub fn fit_scale(img: Size, vp: Size) -> f32 {
    if img.width <= 0.0 || img.height <= 0.0 || vp.width <= 0.0 || vp.height <= 0.0 {
        return 1.0;
    }
    (vp.width / img.width).min(vp.height / img.height)
}

/// Aligned when smaller than the pane, edge to edge otherwise (as the
/// viewer clamps, which centres).
pub fn clamp_off(img: Size, scale: f32, off: Vector, port: Port) -> Vector {
    let vp = port.size;
    let (w, h) = (img.width * scale, img.height * scale);
    let x = if w <= vp.width { (vp.width - w) * port.align.0 } else { off.x.clamp(vp.width - w, 0.0) };
    let y = if h <= vp.height { (vp.height - h) * port.align.1 } else { off.y.clamp(vp.height - h, 0.0) };
    Vector::new(x, y)
}

/// Scale and top-left offset of a photo of `img` pixels in a pane.
pub fn place(v: Linked, img: Size, port: Port) -> (f32, Vector) {
    let vp = port.size;
    if v.fit {
        let s = fit_scale(img, vp);
        return (s, clamp_off(img, s, Vector::ZERO, port));
    }
    let s = v.scale;
    let off = Vector::new(vp.width / 2.0 - v.center.0 * img.width * s, vp.height / 2.0 - v.center.1 * img.height * s);
    (s, clamp_off(img, s, off, port))
}

/// The photo point (as fractions) at the pane's centre for a placement.
pub fn center_of(img: Size, s: f32, off: Vector, vp: Size) -> (f32, f32) {
    let w = (img.width * s).max(1e-6);
    let h = (img.height * s).max(1e-6);
    ((vp.width / 2.0 - off.x) / w, (vp.height / 2.0 - off.y) / h)
}

/// `v` with its centre pulled in to where this pane can show it, so a pan
/// past the edge doesn't have to be unwound before it moves back.  On an
/// axis where the photo doesn't fill the pane the centre is the middle.
pub fn clamp(v: Linked, img: Size, port: Port) -> Linked {
    if v.fit {
        return v;
    }
    let (s, off) = place(v, img, port);
    let c = center_of(img, s, off, port.size);
    let x = if img.width * s > port.size.width { c.0 } else { 0.5 };
    let y = if img.height * s > port.size.height { c.1 } else { 0.5 };
    Linked { center: (x, y), ..v }
}

/// Zoom to `target` with the photo point under `anchor` (pane coordinates)
/// held still, in the pane whose photo is `img`, placed at `s`, `off` now.
/// Anything at or below fit is fit.
pub fn zoom_to(s: f32, off: Vector, img: Size, port: Port, target: f32, anchor: Point) -> Linked {
    let vp = port.size;
    let ns = target.min(MAX_SCALE);
    if ns <= fit_scale(img, vp) * 1.0001 {
        return FIT;
    }
    let s = s.max(1e-6);
    let ia = ((anchor.x - off.x) / s, (anchor.y - off.y) / s);
    let c = (ia.0 - (anchor.x - vp.width / 2.0) / ns, ia.1 - (anchor.y - vp.height / 2.0) / ns);
    clamp(Linked { fit: false, scale: ns, center: (c.0 / img.width.max(1.0), c.1 / img.height.max(1.0)) }, img, port)
}

/// Move the photos by `delta` screen pixels, as dragged in the pane whose
/// photo is `img`.
pub fn pan(v: Linked, img: Size, port: Port, delta: Vector) -> Linked {
    if v.fit {
        return v;
    }
    let c = (v.center.0 - delta.x / (img.width * v.scale).max(1e-6), v.center.1 - delta.y / (img.height * v.scale).max(1e-6));
    clamp(Linked { center: c, ..v }, img, port)
}

/// A photo's placement `e` (eased, 0–1) of the way from view `a` to `b`:
/// scale and centre move together, so the zoom heads straight for its
/// target.
pub fn between(a: Linked, b: Linked, e: f32, img: Size, port: Port) -> (f32, Vector) {
    let vp = port.size;
    let (s0, o0) = place(a, img, port);
    let (s1, o1) = place(b, img, port);
    let c0 = center_of(img, s0, o0, vp);
    let c1 = center_of(img, s1, o1, vp);
    let s = paint::lerp(s0, s1, e);
    let c = (paint::lerp(c0.0, c1.0, e), paint::lerp(c0.1, c1.1, e));
    let off = Vector::new(vp.width / 2.0 - c.0 * img.width * s, vp.height / 2.0 - c.1 * img.height * s);
    (s, clamp_off(img, s, off, port))
}

fn img_size(img: &FullImage) -> Size {
    Size::new(img.width.max(1) as f32, img.height.max(1) as f32)
}

// ── the widget ──────────────────────────────────────────────────────────────

/// One photo on the stage.
#[derive(Clone)]
pub struct Pane {
    pub image: Option<Arc<FullImage>>,
    /// Width ÷ height to lay out with until the photo arrives.
    pub aspect: f32,
    pub name: String,
    /// File-type tag, as under a grid thumbnail.
    pub tag: super::tags::Kind,
    pub marked: bool,
    pub pruned: bool,
    /// The crop (left, top, right, bottom, as fractions), framed as in the
    /// grid.
    pub crop: Option<[f32; 4]>,
    /// Shown in place of the photo ("Loading…", an error).
    pub status: Option<String>,
}

impl Pane {
    fn aspect(&self) -> f32 {
        match &self.image {
            Some(img) => img.width.max(1) as f32 / img.height.max(1) as f32,
            None => self.aspect,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CompareEvent {
    /// A pane was clicked.
    Focus(usize),
    /// The focused photo's scale (1.0 = 100 %), for the zoom readout.
    Zoom(f32),
    /// The stage's size, for laying out and preparing the photos.
    Resized(Size),
}

pub struct CompareStage<'a, Message> {
    key: u64,
    panes: Vec<Pane>,
    focus: usize,
    cmd: (u64, ViewCmd),
    scale_factor: f32,
    on: Box<dyn Fn(CompareEvent) -> Message + 'a>,
}

impl<'a, Message> CompareStage<'a, Message> {
    /// `key` changes when a new set of photos is shown (the view resets).
    pub fn new(key: u64, panes: Vec<Pane>, focus: usize, cmd: (u64, ViewCmd), scale_factor: f32, on: impl Fn(CompareEvent) -> Message + 'a) -> Self {
        Self { key, panes, focus, cmd, scale_factor, on: Box::new(on) }
    }

    fn cells(&self, b: Rectangle) -> Vec<Cell> {
        let aspects: Vec<f32> = self.panes.iter().map(Pane::aspect).collect();
        layout(&aspects, b)
    }

    /// The pane commands zoom about: the focused one, else any with a photo.
    fn reference(&self) -> Option<(usize, &Arc<FullImage>)> {
        let focused = self.panes.get(self.focus).and_then(|p| p.image.as_ref()).map(|img| (self.focus, img));
        focused.or_else(|| self.panes.iter().enumerate().find_map(|(i, p)| p.image.as_ref().map(|img| (i, img))))
    }
}

#[derive(Clone, Copy)]
struct Anim {
    from: Linked,
    to: Linked,
    start: Instant,
}

struct Pan {
    pane: usize,
    origin: Point,
    start: Linked,
}

struct State {
    key: u64,
    view: Linked,
    anim: Option<Anim>,
    cmd_seq: u64,
    focus: usize,
    /// Where the focus ring is coming from, and since when.
    ring_from: Option<(usize, Instant)>,
    hover: Option<usize>,
    pan: Option<Pan>,
    last_click: Option<amouse::Click>,
    /// The stage size last laid out (for commands, which arrive without it).
    size: Size,
    last_zoom_pub: i32,
    reported: Size,
}

fn ease(start: Instant, ms: f32, now: Instant) -> (f32, bool) {
    let t = now.saturating_duration_since(start).as_secs_f32() * 1000.0 / ms;
    (paint::out_cubic(t), t >= 1.0)
}

impl State {
    /// A photo's placement now, following any zoom animation.
    fn placement(&self, img: Size, vp: Port, now: Instant) -> (f32, Vector) {
        match self.anim {
            Some(a) => between(a.from, a.to, ease(a.start, ZOOM_MS, now).0, img, vp),
            None => place(self.view, img, vp),
        }
    }

    /// The view as it stands now, as seen from a pane showing `img` (an
    /// animation is frozen where it is).
    fn current(&self, img: Size, vp: Port, now: Instant) -> Linked {
        match self.anim {
            Some(a) => {
                let (e, done) = ease(a.start, ZOOM_MS, now);
                if done {
                    return a.to;
                }
                let (s, off) = between(a.from, a.to, e, img, vp);
                clamp(Linked { fit: false, scale: s, center: center_of(img, s, off, vp.size) }, img, vp)
            }
            None => self.view,
        }
    }

    fn settle(&mut self, now: Instant) {
        if let Some(a) = self.anim {
            if ease(a.start, ZOOM_MS, now).1 {
                self.view = a.to;
                self.anim = None;
            }
        }
    }

    fn animate_to(&mut self, to: Linked, img: Size, vp: Port, now: Instant) {
        let from = self.current(img, vp, now);
        if from == to {
            return;
        }
        self.view = to;
        self.anim = Some(Anim { from, to, start: now });
    }

    fn set(&mut self, v: Linked) {
        self.anim = None;
        self.view = v;
    }

    fn fit_mode(&self) -> bool {
        self.anim.is_none() && self.view.fit
    }
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for CompareStage<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            key: self.key,
            view: FIT,
            anim: None,
            cmd_seq: self.cmd.0,
            focus: self.focus,
            ring_from: None,
            hover: None,
            pan: None,
            last_click: None,
            size: Size::ZERO,
            last_zoom_pub: -1,
            reported: Size::ZERO,
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        if self.key != st.key {
            st.key = self.key;
            st.set(FIT);
            st.pan = None;
            st.ring_from = None;
            st.focus = self.focus;
            st.last_zoom_pub = -1;
        }
        if self.focus != st.focus {
            st.ring_from = Some((st.focus, now));
            st.focus = self.focus;
        }
        if self.cmd.0 == st.cmd_seq {
            return;
        }
        st.cmd_seq = self.cmd.0;
        let cells = self.cells(Rectangle::with_size(st.size));
        let Some((i, img)) = self.reference() else { return };
        let Some(cell) = cells.get(i) else { return };
        let (img, vp) = (img_size(img), cell.port());
        let centre = Point::new(vp.size.width / 2.0, vp.size.height / 2.0);
        match self.cmd.1 {
            ViewCmd::Fit => st.animate_to(FIT, img, vp, now),
            ViewCmd::Actual => {
                let (s, off) = st.placement(img, vp, now);
                st.animate_to(zoom_to(s, off, img, vp, 1.0, centre), img, vp, now);
            }
            ViewCmd::ZoomIn | ViewCmd::ZoomOut => {
                let (s, off) = st.placement(img, vp, now);
                let factor = if self.cmd.1 == ViewCmd::ZoomIn { STEP } else { 1.0 / STEP };
                st.set(zoom_to(s, off, img, vp, s * factor, centre));
            }
            ViewCmd::None => {}
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _t: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        crate::ui::modifiers::observe(event);
        let b = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        st.size = b.size();
        st.settle(now);
        let cells = self.cells(b);
        let pane_at = |p: Point| cells.iter().position(|c| c.slot.contains(p));

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("compare", *at);
                if st.anim.is_some() {
                    shell.request_redraw();
                }
                if let Some((_, since)) = st.ring_from {
                    if ease(since, t::MOTION_MS, now).1 {
                        st.ring_from = None;
                    } else {
                        shell.request_redraw();
                    }
                }
                let focused = self.panes.get(self.focus).and_then(|p| p.image.as_ref()).zip(cells.get(self.focus));
                if let Some((img, cell)) = focused {
                    let s = st.placement(img_size(img), cell.port(), now).0;
                    let pub_key = (s * 1000.0).round() as i32;
                    if pub_key != st.last_zoom_pub {
                        st.last_zoom_pub = pub_key;
                        shell.publish((self.on)(CompareEvent::Zoom(s)));
                    }
                }
                if st.reported != b.size() {
                    st.reported = b.size();
                    shell.publish((self.on)(CompareEvent::Resized(b.size())));
                }
            }

            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(p) = cursor.position_in(b).and(cursor.position()) else { return };
                let Some(i) = pane_at(p) else { return };
                let Some(img) = self.panes[i].image.as_ref() else { return };
                let (img, vp) = (img_size(img), cells[i].port());
                let local = Point::new(p.x - cells[i].photo.x, p.y - cells[i].photo.y);
                let ctrl = crate::ui::modifiers::get().control();
                if st.fit_mode() || ctrl || st.anim.is_some() {
                    let factor = match delta {
                        ScrollDelta::Pixels { y, .. } if *y != 0.0 => 1.0 + y * 0.002,
                        ScrollDelta::Lines { y, .. } if *y != 0.0 => {
                            if *y > 0.0 { STEP } else { 1.0 / STEP }
                        }
                        _ => return,
                    };
                    let (s, off) = st.placement(img, vp, now);
                    st.set(zoom_to(s, off, img, vp, s * factor, local));
                } else {
                    let shift = crate::ui::modifiers::get().shift();
                    let (dx, dy) = match delta {
                        ScrollDelta::Lines { x, y } => (x * 60.0, y * 60.0),
                        ScrollDelta::Pixels { x, y } => (*x, *y),
                    };
                    let (dx, dy) = if shift { (dy, dx) } else { (dx, dy) };
                    let v = st.view;
                    st.set(pan(v, img, vp, Vector::new(dx, dy)));
                }
                shell.capture_event();
                shell.request_redraw();
            }

            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_in(b).and(cursor.position()) else { return };
                shell.capture_event();
                let Some(i) = pane_at(p) else { return };
                if i != self.focus {
                    shell.publish((self.on)(CompareEvent::Focus(i)));
                }
                let click = amouse::Click::new(p, mouse::Button::Left, st.last_click);
                st.last_click = Some(click);
                let Some(img) = self.panes[i].image.as_ref() else { return };
                let photo = cells[i].photo;
                if !photo.contains(p) {
                    return;
                }
                let (img, vp) = (img_size(img), cells[i].port());
                let local = Point::new(p.x - photo.x, p.y - photo.y);
                let (s, off) = st.placement(img, vp, now);
                if click.kind() == amouse::click::Kind::Double {
                    // Fit ↔ 100 % about the point clicked, on every photo.
                    let to = if st.fit_mode() { zoom_to(s, off, img, vp, 1.0, local) } else { FIT };
                    st.animate_to(to, img, vp, now);
                    shell.request_redraw();
                    return;
                }
                if img.width * s > vp.size.width + 0.5 || img.height * s > vp.size.height + 0.5 {
                    let v = st.current(img, vp, now);
                    st.set(v);
                    st.pan = Some(Pan { pane: i, origin: p, start: v });
                }
            }

            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(p) = cursor.position() else { return };
                if let Some(pn) = &st.pan {
                    if let (Some(img), Some(cell)) = (self.panes.get(pn.pane).and_then(|x| x.image.as_ref()), cells.get(pn.pane)) {
                        let v = pan(pn.start, img_size(img), cell.port(), p - pn.origin);
                        st.view = v;
                    }
                    shell.request_redraw();
                    return;
                }
                let hover = cursor.position_in(b).and_then(|_| pane_at(p));
                if hover != st.hover {
                    st.hover = hover;
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::CursorLeft) => {
                if st.hover.take().is_some() {
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.pan = None;
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        if st.pan.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let b = layout.bounds();
        let Some(p) = cursor.position_in(b).and(cursor.position()) else { return mouse::Interaction::None };
        let cells = self.cells(b);
        let Some(i) = cells.iter().position(|c| c.photo.contains(p)) else { return mouse::Interaction::Idle };
        if let Some(img) = self.panes[i].image.as_ref() {
            let (img, vp) = (img_size(img), cells[i].port());
            let (s, _) = st.placement(img, vp, Instant::now());
            if img.width * s > vp.size.width + 0.5 || img.height * s > vp.size.height + 0.5 {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::Idle
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        r: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let _t = crate::profile::span("draw", "compare");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let sf = self.scale_factor;
        let cells = self.cells(b);

        // Where each photo lands, and the part of it in view (for a photo
        // still loading, where it will be).
        let placed: Vec<(Option<(f32, Rectangle)>, Rectangle)> = self
            .panes
            .iter()
            .zip(&cells)
            .map(|(pane, cell)| match pane.image.as_ref() {
                Some(img) => {
                    let (s, off) = st.placement(img_size(img), cell.port(), now);
                    let dst = rect(cell.photo.x + off.x, cell.photo.y + off.y, img.width as f32 * s, img.height as f32 * s);
                    (Some((s, dst)), dst.intersection(&cell.photo).unwrap_or(fitted(pane.aspect(), cell)))
                }
                None => (None, fitted(pane.aspect(), cell)),
            })
            .collect();
        let mounts: Vec<Rectangle> = cells.iter().zip(&placed).map(|(c, (_, visible))| c.mount(*visible)).collect();

        r.with_layer(b, |r| {
            paint::fill(r, b, t::PHOTO_GROUND);
            for (i, cell) in cells.iter().enumerate() {
                let hot = st.hover == Some(i) && i != self.focus;
                paint::rounded(r, mounts[i], t::R_CELL, if hot { t::PHOTO_RAISED } else { t::PHOTO_MOUNT });
                if let (Some(img), (Some((s, dst)), _)) = (self.panes[i].image.as_ref(), placed[i]) {
                    draw_photo(r, cell.photo, img, s, dst, sf);
                }
            }
        });

        // Everything over the photos needs its own layer: images draw above
        // every quad in theirs.
        r.with_layer(b, |r| {
            for (i, pane) in self.panes.iter().enumerate() {
                let visible = placed[i].1;
                match (&placed[i].0, &pane.status) {
                    (Some((_, dst)), _) => r.with_layer(cells[i].photo, |r| {
                        if let Some(c) = pane.crop.filter(|_| !pane.pruned) {
                            draw_crop(r, *dst, c);
                        }
                        draw_marks(r, visible, pane.marked, pane.pruned);
                    }),
                    (None, Some(status)) => paint::text(r, status, visible, t::T_BODY, paint::REGULAR, t::PHOTO_INK3, HAlign::Center, visible),
                    (None, None) => {}
                }
                draw_caption(r, visible, pane, i == self.focus);
            }
            // The focus ring, sliding from the pane that had it.
            if let Some(&to) = mounts.get(self.focus) {
                let ring = match st.ring_from.and_then(|(from, since)| mounts.get(from).map(|&m| (m, since))) {
                    Some((from, since)) => paint::lerp_rect(from, to, ease(since, t::MOTION_MS, now).0),
                    None => to,
                };
                paint::framed(r, paint::inset(ring, -1.0), t::R_CELL + 1.0, Color::TRANSPARENT, t::SIGNAL, 2.0);
            }
        });
    }
}

/// The photo from a mip level close to its on-screen size, over the next
/// coarser and the smallest (which stays on the GPU), so a level still
/// uploading never leaves a hole.
fn draw_photo(r: &mut Renderer, clip: Rectangle, img: &FullImage, scale: f32, dst: Rectangle, sf: f32) {
    if dst.intersection(&clip).is_none() || img.levels.is_empty() {
        return;
    }
    let last = img.levels.len() - 1;
    let i = img.level_for_scale(scale * sf);
    let mut drawn = usize::MAX;
    for idx in [last, (i + 1).min(last), i] {
        if idx == drawn {
            continue;
        }
        drawn = idx;
        let l = &img.levels[idx];
        r.draw_image(iced::advanced::image::Image::new(l.handle.clone()).filter_method(FilterMethod::Linear), dst, clip);
    }
}

/// A crop as the grid shows it: the margins it discards scrimmed, the rest
/// framed.  Drawn in a layer clipped to the pane.
fn draw_crop(r: &mut Renderer, dst: Rectangle, c: [f32; 4]) {
    let (x0, y0) = (dst.x + c[0] * dst.width, dst.y + c[1] * dst.height);
    let (x1, y1) = (dst.x + c[2] * dst.width, dst.y + c[3] * dst.height);
    let (right, bottom) = (dst.x + dst.width, dst.y + dst.height);
    let scrim = t::alpha(Color::BLACK, 0.6);
    paint::fill(r, rect(dst.x, dst.y, dst.width, y0 - dst.y), scrim);
    paint::fill(r, rect(dst.x, y1, dst.width, bottom - y1), scrim);
    paint::fill(r, rect(dst.x, y0, x0 - dst.x, y1 - y0), scrim);
    paint::fill(r, rect(x1, y0, right - x1, y1 - y0), scrim);
    paint::framed(r, rect(x0.floor(), y0.floor(), (x1 - x0).floor().max(1.0), (y1 - y0).floor().max(1.0)), 0.0, Color::TRANSPARENT, t::alpha(Color::WHITE, 0.85), 1.0);
}

/// The grid's marks, on the part of the photo in view: a pruned photo is
/// dimmed and crossed out corner to corner, a kept one wears a green check.
fn draw_marks(r: &mut Renderer, ir: Rectangle, marked: bool, pruned: bool) {
    if pruned {
        paint::fill(r, ir, t::alpha(t::PHOTO_GROUND, 0.5));
        let inset = (ir.width.min(ir.height) * 0.08).max(6.0);
        let (x0, y0, x1, y1) = (ir.x + inset, ir.y + inset, ir.x + ir.width - inset, ir.y + ir.height - inset);
        if x1 > x0 && y1 > y0 {
            paint::strokes(
                r,
                &[(Point::new(x0, y0), Point::new(x1, y1)), (Point::new(x1, y0), Point::new(x0, y1))],
                4.0,
                t::alpha(t::REJECT, 0.9),
                true,
            );
        }
    }
    if marked {
        let d = 28.0;
        let (bx, by) = (ir.x + ir.width - d - 8.0, ir.y + 8.0);
        let centre = Point::new(bx + d / 2.0, by + d / 2.0);
        paint::circle(r, centre, d / 2.0 + 1.5, t::alpha(Color::BLACK, 0.35), Color::TRANSPARENT, 0.0);
        paint::circle(r, centre, d / 2.0, t::KEEP, Color::TRANSPARENT, 0.0);
        let u = d / 24.0;
        let p = |x: f32, y: f32| Point::new(bx + x * u, by + y * u);
        paint::strokes(r, &[(p(7.0, 12.5), p(10.5, 16.0)), (p(10.5, 16.0), p(17.0, 8.5))], 2.8, t::ON_SIGNAL, true);
    }
}

/// File name left, file-type tag right, in the strip under the photo.
fn draw_caption(r: &mut Renderer, visible: Rectangle, pane: &Pane, focused: bool) {
    let strip = rect(visible.x, visible.y + visible.height, visible.width, CAPTION_H);
    let x = strip.x + strip.width - super::tags::kind_width(pane.tag);
    let name_w = (x - strip.x - 8.0).max(0.0);
    super::tags::draw_kind(r, x, strip.center_y(), pane.tag, 1.0, strip);
    // As in the grid: shortened from the start, keeping the sequence number.
    let name = paint::elide_start(&pane.name, t::T_SMALL, paint::MEDIUM, name_w);
    let ink = if focused { t::PHOTO_INK } else { t::PHOTO_INK2 };
    paint::text(r, &name, strip, t::T_SMALL, paint::MEDIUM, ink, HAlign::Left, strip);
}

impl<'a, Message: 'a> From<CompareStage<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(s: CompareStage<'a, Message>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: f32 = 1.5;
    const PORT: f32 = 2.0 / 3.0;

    fn area(w: f32, h: f32) -> Rectangle {
        Rectangle::new(Point::new(10.0, 20.0), Size::new(w, h))
    }

    fn check_cells(cells: &[Cell], a: Rectangle) {
        for (i, c) in cells.iter().enumerate() {
            assert!(c.slot.x >= a.x && c.slot.y >= a.y, "pane {i} starts outside {a:?}: {c:?}");
            assert!(c.slot.x + c.slot.width <= a.x + a.width + 0.01, "pane {i} overruns the right edge");
            assert!(c.slot.y + c.slot.height <= a.y + a.height + 0.01, "pane {i} overruns the bottom");
            assert_eq!(c.slot.size(), cells[0].slot.size(), "panes differ in size");
            assert!(c.slot.contains(c.photo.center()) && c.photo.height < c.slot.height);
            // A fitted photo's mount stays inside its pane.
            let m = c.mount(fitted(1.5, c));
            assert!(m.x >= c.slot.x - 0.01 && m.y >= c.slot.y - 0.01 && m.x + m.width <= c.slot.x + c.slot.width + 0.51);
            assert!(m.y + m.height <= c.slot.y + c.slot.height + 0.51, "mount {m:?} overruns {:?}", c.slot);
            for (j, d) in cells.iter().enumerate().skip(i + 1) {
                assert!(c.slot.intersection(&d.slot).is_none_or(|x| x.width * x.height < 0.01), "panes {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn two_landscape_photos_sit_side_by_side_in_a_wide_window() {
        let a = area(1600.0, 900.0);
        assert_eq!(shape(&[LAND, LAND], a.size()), (2, 1));
        let cells = layout(&[LAND, LAND], a);
        check_cells(&cells, a);
        assert!(cells[0].slot.x < cells[1].slot.x);
        assert_eq!(cells[0].slot.y, cells[1].slot.y);
        assert_eq!(cells[0].slot.x, a.x + MARGIN);
    }

    #[test]
    fn two_landscape_photos_stack_in_a_tall_window() {
        let a = area(800.0, 1400.0);
        assert_eq!(shape(&[LAND, LAND], a.size()), (1, 2));
        let cells = layout(&[LAND, LAND], a);
        check_cells(&cells, a);
        assert!(cells[0].slot.y < cells[1].slot.y);
    }

    #[test]
    fn two_portraits_sit_side_by_side_even_when_square() {
        assert_eq!(shape(&[PORT, PORT], Size::new(1000.0, 1000.0)), (2, 1));
    }

    #[test]
    fn three_landscape_photos_make_two_rows_with_the_last_centred() {
        let a = area(1600.0, 900.0);
        assert_eq!(shape(&[LAND; 3], a.size()), (2, 2));
        let cells = layout(&[LAND; 3], a);
        check_cells(&cells, a);
        assert_eq!(cells[0].slot.y, cells[1].slot.y);
        assert!(cells[2].slot.y > cells[0].slot.y);
        let mid = a.x + a.width / 2.0;
        assert!((cells[2].slot.center_x() - mid).abs() <= 1.0, "last pane is centred");
    }

    #[test]
    fn three_portraits_make_one_row() {
        let a = area(1600.0, 900.0);
        assert_eq!(shape(&[PORT; 3], a.size()), (3, 1));
        check_cells(&layout(&[PORT; 3], a), a);
    }

    #[test]
    fn four_photos_make_two_by_two_unless_the_window_is_very_wide() {
        let a = area(1600.0, 900.0);
        assert_eq!(shape(&[LAND; 4], a.size()), (2, 2));
        check_cells(&layout(&[LAND; 4], a), a);
        assert_eq!(shape(&[PORT; 4], Size::new(2400.0, 800.0)), (4, 1));
        assert_eq!(shape(&[LAND; 4], Size::new(600.0, 1800.0)), (1, 4));
    }

    #[test]
    fn mixed_shapes_still_get_a_layout_that_fits() {
        let a = area(1200.0, 800.0);
        let aspects = [LAND, PORT, LAND, PORT];
        check_cells(&layout(&aspects, a), a);
        // A tiny window doesn't panic or produce negative sizes.
        let tiny = area(20.0, 20.0);
        for c in layout(&aspects, tiny) {
            assert!(c.photo.width >= 0.0 && c.photo.height >= 0.0);
        }
    }

    #[test]
    fn arrows_move_focus_to_the_pane_that_way() {
        let cells = layout(&[LAND; 4], area(1600.0, 900.0));
        // 0 1
        // 2 3
        assert_eq!(neighbour(&cells, 0, 1, 0), Some(1));
        assert_eq!(neighbour(&cells, 0, 0, 1), Some(2));
        assert_eq!(neighbour(&cells, 3, -1, 0), Some(2));
        assert_eq!(neighbour(&cells, 3, 0, -1), Some(1));
        assert_eq!(neighbour(&cells, 0, -1, 0), None);
        assert_eq!(neighbour(&cells, 1, 0, -1), None);
        // Three in 2 × 2: down from either top pane reaches the centred one.
        let three = layout(&[LAND; 3], area(1600.0, 900.0));
        assert_eq!(neighbour(&three, 0, 0, 1), Some(2));
        assert_eq!(neighbour(&three, 1, 0, 1), Some(2));
        assert!(matches!(neighbour(&three, 2, 0, -1), Some(0) | Some(1)));
        // A row: up and down go nowhere.
        let row = layout(&[PORT; 3], area(1600.0, 900.0));
        assert_eq!(neighbour(&row, 1, 0, 1), None);
        assert_eq!(neighbour(&row, 1, 1, 0), Some(2));
    }

    const IMG: Size = Size::new(6000.0, 4000.0);
    const VP: Port = Port { size: Size::new(780.0, 600.0), align: (0.5, 0.5) };

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn fit_centres_the_whole_photo() {
        let (s, off) = place(FIT, IMG, VP);
        assert!(close(s, 0.13));
        assert!(close(off.x, 0.0));
        assert!(close(off.y, (600.0 - 520.0) / 2.0));
        let c = center_of(IMG, s, off, VP.size);
        assert!(close(c.0, 0.5) && close(c.1, 0.5));
    }

    #[test]
    fn actual_size_from_fit_shows_the_middle_at_100_percent() {
        let (s, off) = place(FIT, IMG, VP);
        let v = zoom_to(s, off, IMG, VP, 1.0, Point::new(390.0, 300.0));
        assert!(!v.fit && close(v.scale, 1.0));
        assert!(close(v.center.0, 0.5) && close(v.center.1, 0.5));
        let (s1, off1) = place(v, IMG, VP);
        assert!(close(s1, 1.0));
        assert!(close(off1.x, 390.0 - 3000.0) && close(off1.y, 300.0 - 2000.0));
    }

    #[test]
    fn zooming_holds_the_point_under_the_cursor_in_every_pane() {
        let (s, off) = place(FIT, IMG, VP);
        let anchor = Point::new(200.0, 150.0);
        let under = ((anchor.x - off.x) / s / IMG.width, (anchor.y - off.y) / s / IMG.height);
        let v = zoom_to(s, off, IMG, VP, 1.0, anchor);
        // In the pane zoomed, the same photo point is still under the cursor.
        let (s1, off1) = place(v, IMG, VP);
        let after = ((anchor.x - off1.x) / s1 / IMG.width, (anchor.y - off1.y) / s1 / IMG.height);
        assert!(close(under.0, after.0) && close(under.1, after.1), "{under:?} vs {after:?}");
        // A second pane with a same-size photo shows exactly the same spot.
        assert_eq!(place(v, IMG, VP), (s1, off1));
        // A pane with a smaller frame (another camera) shows the same spot
        // as a fraction of its photo, at the same 100 %.
        let small = Size::new(4000.0, 2667.0);
        let (s2, off2) = place(v, small, VP);
        assert!(close(s2, 1.0));
        let c2 = center_of(small, s2, off2, VP.size);
        assert!(close(c2.0, v.center.0) && close(c2.1, v.center.1), "{c2:?} vs {:?}", v.center);
    }

    #[test]
    fn zooming_out_past_fit_is_fit() {
        let (s, off) = place(FIT, IMG, VP);
        assert_eq!(zoom_to(s, off, IMG, VP, s / 1.2, Point::new(10.0, 10.0)), FIT);
        let v = zoom_to(s, off, IMG, VP, 0.5, Point::new(390.0, 300.0));
        let (s1, off1) = place(v, IMG, VP);
        assert_eq!(zoom_to(s1, off1, IMG, VP, 0.05, Point::new(390.0, 300.0)), FIT);
    }

    #[test]
    fn panning_stops_at_the_edge_without_overshoot() {
        let v = Linked { fit: false, scale: 1.0, center: (0.5, 0.5) };
        // Drag far right: the photo's left edge reaches the pane's.
        let far = pan(v, IMG, VP, Vector::new(10_000.0, 0.0));
        let (_, off) = place(far, IMG, VP);
        assert!(close(off.x, 0.0));
        assert!(close(far.center.0, 390.0 / 6000.0));
        // Dragging back moves at once: nothing past the edge to unwind.
        let back = pan(far, IMG, VP, Vector::new(-100.0, 0.0));
        let (_, off2) = place(back, IMG, VP);
        assert!(close(off2.x, -100.0));
        // Fit doesn't pan.
        assert_eq!(pan(FIT, IMG, VP, Vector::new(50.0, 50.0)), FIT);
    }

    #[test]
    fn fitted_photos_lean_towards_the_middle_of_the_stage() {
        let cells = layout(&[LAND; 4], area(1600.0, 900.0));
        assert_eq!(cells[0].align, (1.0, 1.0));
        assert_eq!(cells[1].align, (0.0, 1.0));
        assert_eq!(cells[2].align, (1.0, 0.0));
        assert_eq!(cells[3].align, (0.0, 0.0));
        // Placed at fit, the left photo's right edge meets its viewport's.
        let port = cells[0].port();
        let (s, off) = place(FIT, IMG, port);
        assert!(close(off.x + IMG.width * s, port.size.width) || close(off.y + IMG.height * s, port.size.height));
        // A lone photo in the last row centres across.
        let three = layout(&[LAND; 3], area(1600.0, 900.0));
        assert_eq!(three[2].align, (0.5, 0.0));
        // Zooming in from an aligned fit starts where the photo is.
        let to = zoom_to(s, off, IMG, port, 1.0, Point::new(100.0, 100.0));
        assert_eq!(between(FIT, to, 0.0, IMG, port), (s, off));
    }

    #[test]
    fn a_zoom_animation_runs_from_one_view_to_the_other() {
        let to = Linked { fit: false, scale: 1.0, center: (0.25, 0.75) };
        assert_eq!(between(FIT, to, 0.0, IMG, VP), place(FIT, IMG, VP));
        let (s, off) = between(FIT, to, 1.0, IMG, VP);
        let (s1, off1) = place(to, IMG, VP);
        assert!(close(s, s1) && close(off.x, off1.x) && close(off.y, off1.y));
        let (sm, _) = between(FIT, to, 0.5, IMG, VP);
        assert!(sm > 0.13 && sm < 1.0);
    }
}
