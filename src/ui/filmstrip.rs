//! The viewer's filmstrip: a row of thumbnails under the photo, in the
//! viewer's order, with their prune / keep marks and crops.  The photo on
//! screen is framed in the accent; click another to jump to it.  The strip
//! glides to keep the current photo centred.  A thin bracket under a run
//! of frames from one burst marks it, as in the grid.
//!
//! One custom widget draws only the cells in view, from the same thumbnail
//! caches as the grids (the working folder's, or the calendar's for library
//! photos), so a strip over 10 000 photos costs what one over 20 does.  It
//! asks the app for the thumbnails it lacks — those around where it is
//! heading, not every cell it passes on the way — and keeps everything that
//! moves (the glide, hover, fade-ins) to itself.

use std::ops::Range;
use std::time::Instant;

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, window};

use super::paint::{self, Renderer, Tween, rect};
use super::theme as t;
use crate::imaging::thumbs::ThumbCache;
use crate::model::bursts::BurstId;
use crate::model::collection::{Collection, RecordId};
use crate::model::record::PhotoRecord;

/// The strip's height, rules included.
pub const HEIGHT: f32 = 84.0;
/// A cell's mount (square).
const CELL: f32 = 68.0;
const GAP: f32 = 6.0;
/// Space before the first cell and after the last.
const MARGIN: f32 = 10.0;
/// Between a mount's edge and its photo.
const PAD: f32 = 3.0;
const GLIDE_MS: f32 = 240.0;
/// Cells either side of the view whose thumbnails are fetched ahead.
const PREFETCH: usize = 6;
/// A burst's bracket: below the mounts (clear of the current photo's
/// frame), rising a little at the run's ends.
const BRACKET_DROP: f32 = 5.0;
const BRACKET_RISE: f32 = 7.0;

/// What the strip reports to the app.
#[derive(Debug, Clone)]
pub enum FilmEvent {
    /// The photo at this index in the viewer's order was clicked.
    Jump(usize),
    /// Thumbnails wanted: `visible` where the strip is (or is heading),
    /// `nearby` just beyond it.
    Thumbs { visible: Vec<RecordId>, nearby: Vec<RecordId> },
}

/// Cell geometry for `n` cells in a view `width` wide.  Offsets are in
/// content space: `scroll` is the content x at the view's left edge, and is
/// negative when a short strip is centred.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strip {
    pub n: usize,
    pub width: f32,
}

impl Strip {
    const STRIDE: f32 = CELL + GAP;

    pub fn content_width(&self) -> f32 {
        if self.n == 0 { 0.0 } else { 2.0 * MARGIN + self.n as f32 * CELL + (self.n - 1) as f32 * GAP }
    }

    pub fn cell_x(&self, i: usize) -> f32 {
        MARGIN + i as f32 * Self::STRIDE
    }

    /// Keep `scroll` inside the content; a strip shorter than the view sits
    /// in its middle.
    pub fn clamp(&self, scroll: f32) -> f32 {
        let max = self.content_width() - self.width;
        if max <= 0.0 { max / 2.0 } else { scroll.clamp(0.0, max) }
    }

    /// The scroll that centres cell `i`.
    pub fn centred(&self, i: usize) -> f32 {
        self.clamp(self.cell_x(i) + CELL / 2.0 - self.width / 2.0)
    }

    /// Cells at least partly in view.
    pub fn visible(&self, scroll: f32) -> Range<usize> {
        if self.n == 0 || self.width <= 0.0 {
            return 0..0;
        }
        let first = ((scroll - MARGIN - CELL) / Self::STRIDE).ceil().max(0.0) as usize;
        let last = ((scroll + self.width - MARGIN) / Self::STRIDE).floor().max(-1.0) as isize + 1;
        first.min(self.n)..(last.max(0) as usize).min(self.n)
    }

    /// The cell under view-space `x`, if any (not the gaps).
    pub fn hit(&self, scroll: f32, x: f32) -> Option<usize> {
        let cx = x + scroll - MARGIN;
        if cx < 0.0 {
            return None;
        }
        let i = (cx / Self::STRIDE).floor() as usize;
        (i < self.n && cx - i as f32 * Self::STRIDE <= CELL).then_some(i)
    }
}

struct State {
    scroll: f32,
    glide: Option<Tween>,
    current: usize,
    n: usize,
    width: f32,
    /// The cell under the cursor and when it was entered; the one before
    /// it and when it was left (hover is a quick cross-fade).
    hover: Option<(usize, Instant)>,
    hover_prev: Option<(usize, Instant)>,
    /// The user scrolled the strip: leave it there until the photo changes.
    manual: bool,
}

impl State {
    fn strip(&self) -> Strip {
        Strip { n: self.n, width: self.width }
    }

    /// Where the strip will come to rest.
    fn resting(&self) -> f32 {
        self.glide.map(|g| g.to).unwrap_or(self.scroll)
    }

    fn glide_to(&mut self, to: f32, now: Instant) {
        if (to - self.scroll).abs() < 0.5 {
            self.scroll = to;
            self.glide = None;
        } else {
            self.glide = Some(Tween { from: self.scroll, to, start: now, ms: GLIDE_MS });
        }
    }

    fn hover_amount(&self, i: usize, now: Instant) -> f32 {
        let k = |since: Instant| paint::progress(since, now, t::MOTION_FAST_MS);
        match (self.hover, self.hover_prev) {
            (Some((h, since)), _) if h == i => paint::out_cubic(k(since)),
            (_, Some((p, left))) if p == i => 1.0 - paint::out_cubic(k(left)),
            _ => 0.0,
        }
    }

    fn hover_fading(&self, now: Instant) -> bool {
        let busy = |since: Instant| !paint::done(since, now, t::MOTION_FAST_MS);
        self.hover.is_some_and(|(_, s)| busy(s)) || self.hover_prev.is_some_and(|(_, s)| busy(s))
    }

    fn set_hover(&mut self, i: Option<usize>, now: Instant) {
        if self.hover.map(|h| h.0) == i {
            return;
        }
        if let Some((old, _)) = self.hover {
            self.hover_prev = Some((old, now));
        }
        self.hover = i.map(|i| (i, now));
    }
}

pub struct Filmstrip<'a, Message> {
    items: &'a [RecordId],
    coll: &'a Collection,
    thumbs: &'a ThumbCache,
    current: usize,
    /// Each cell's burst, for the brackets (empty: none).
    bursts: &'a [Option<BurstId>],
    interactive: bool,
    scale_factor: f32,
    on: Box<dyn Fn(FilmEvent) -> Message + 'a>,
}

impl<'a, Message> Filmstrip<'a, Message> {
    pub fn new(
        items: &'a [RecordId],
        coll: &'a Collection,
        thumbs: &'a ThumbCache,
        current: usize,
        scale_factor: f32,
        on: impl Fn(FilmEvent) -> Message + 'a,
    ) -> Self {
        Self { items, coll, thumbs, current, bursts: &[], interactive: true, scale_factor, on: Box::new(on) }
    }

    /// Whether clicks jump (off while the crop tool holds the photo).
    pub fn interactive(mut self, on: bool) -> Self {
        self.interactive = on;
        self
    }

    /// Each cell's burst (parallel to the items): runs of two or more
    /// get a bracket.
    pub fn bursts(mut self, bursts: &'a [Option<BurstId>]) -> Self {
        self.bursts = bursts;
        self
    }

    /// Cell `i`'s place in a run of one burst's frames: (first, last), or
    /// `None` when it isn't in a run of two or more.
    fn run_mark(&self, i: usize) -> Option<(bool, bool)> {
        let b = (*self.bursts.get(i)?)?;
        let same = |j: Option<usize>| j.and_then(|j| self.bursts.get(j).copied().flatten()) == Some(b);
        let (prev, next) = (same(i.checked_sub(1)), same(Some(i + 1)));
        (prev || next).then_some((!prev, !next))
    }

    fn record(&self, i: usize) -> Option<&'a PhotoRecord> {
        self.coll.get(*self.items.get(i)?)
    }

    /// Ids in `range` with no thumbnail and none on its way.
    fn missing(&self, range: Range<usize>) -> Vec<RecordId> {
        self.items[range]
            .iter()
            .copied()
            .filter(|&id| self.coll.get(id).is_some_and(|r| self.thumbs.get(&r.path).is_none() && !self.thumbs.is_queued(&r.path)))
            .collect()
    }
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for Filmstrip<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            scroll: 0.0,
            glide: None,
            current: self.current,
            n: self.items.len(),
            // Unknown until the first layout: the first event centres it.
            width: 0.0,
            hover: None,
            hover_prev: None,
            manual: false,
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let n = self.items.len();
        if st.current != self.current || st.n != n {
            st.current = self.current;
            st.n = n;
            st.manual = false;
            if st.width > 0.0 {
                let now = Instant::now();
                let from = st.glide.map(|g| g.value(now)).unwrap_or(st.scroll);
                let to = st.strip().centred(self.current);
                if (to - from).abs() > st.width * 3.0 {
                    // Home / End across a long shoot: no point gliding
                    // past thousands of cells.
                    st.scroll = to;
                    st.glide = None;
                } else {
                    st.scroll = from;
                    st.glide_to(to, now);
                }
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(HEIGHT))
    }

    fn layout(&mut self, _tree: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, Length::Fixed(HEIGHT), Size::ZERO))
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
        let b = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        st.n = self.items.len();
        if st.width != b.width {
            // First layout, or the window was resized: recentre (a strip
            // the user scrolled stays put).
            let first = st.width <= 0.0;
            st.width = b.width;
            if first || !st.manual {
                st.scroll = st.strip().centred(st.current);
                st.glide = None;
            } else {
                st.scroll = st.strip().clamp(st.scroll);
            }
        }
        if let Some(g) = st.glide {
            st.scroll = g.value(now);
            if g.progress(now) >= 1.0 {
                st.scroll = g.to;
                st.glide = None;
            }
        }
        let strip = st.strip();
        let local_x = |p: Point| p.x - b.x;

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("filmstrip", *at);
                let range = strip.visible(st.scroll);
                let fading = range.clone().any(|i| match self.record(i) {
                    Some(r) => self.thumbs.get(&r.path).is_some_and(|th| paint::arrival(th.arrived, now) < 1.0),
                    None => false,
                });
                if st.glide.is_some() && self.interactive {
                    // Cells slide under a still cursor.
                    let over = cursor.position_in(b).and_then(|p| strip.hit(st.scroll, p.x));
                    st.set_hover(over, now);
                }
                if st.glide.is_some() || st.hover_fading(now) || fading {
                    shell.request_redraw();
                }
                // Thumbnails for where the strip is heading.
                let target = strip.visible(st.resting());
                let near = target.start.saturating_sub(PREFETCH)..(target.end + PREFETCH).min(self.items.len());
                let visible = self.missing(target.clone());
                let nearby: Vec<RecordId> = self
                    .missing(near)
                    .into_iter()
                    .filter(|id| !self.items[target.clone()].contains(id))
                    .collect();
                if !visible.is_empty() || !nearby.is_empty() {
                    shell.publish((self.on)(FilmEvent::Thumbs { visible, nearby }));
                }
            }

            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let over = cursor.position_in(b).and_then(|p| if self.interactive { strip.hit(st.scroll, p.x) } else { None });
                if over != st.hover.map(|h| h.0) {
                    st.set_hover(over, now);
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::CursorLeft) => {
                if st.hover.is_some() {
                    st.set_hover(None, now);
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_over(b) else { return };
                shell.capture_event();
                if !self.interactive {
                    return;
                }
                if let Some(i) = strip.hit(st.scroll, local_x(p)) {
                    if i != self.current {
                        shell.publish((self.on)(FilmEvent::Jump(i)));
                    }
                }
            }

            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if cursor.position_over(b).is_none() {
                    return;
                }
                // Either wheel axis runs along the strip.
                let dx = match delta {
                    ScrollDelta::Lines { x, y } => (if x.abs() > y.abs() { *x } else { *y }) * Strip::STRIDE * 2.0,
                    ScrollDelta::Pixels { x, y } => {
                        if x.abs() > y.abs() {
                            *x
                        } else {
                            *y
                        }
                    }
                };
                st.glide = None;
                st.manual = true;
                st.scroll = strip.clamp(st.scroll - dx);
                if let Some(p) = cursor.position() {
                    let over = if self.interactive { strip.hit(st.scroll, local_x(p)) } else { None };
                    st.set_hover(over, now);
                }
                shell.capture_event();
                shell.request_redraw();
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
        let b = layout.bounds();
        let Some(p) = cursor.position_in(b) else { return mouse::Interaction::None };
        match st.strip().hit(st.scroll, p.x) {
            Some(i) if self.interactive && i != self.current => mouse::Interaction::Pointer,
            _ => mouse::Interaction::Idle,
        }
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
        let _t = crate::profile::span("draw", "filmstrip");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let strip = Strip { n: self.items.len(), width: b.width };
        // Between a glide's frames `st.scroll` is from the last event; the
        // glide itself is the truth.
        let scroll = st.glide.map(|g| g.value(now)).unwrap_or(st.scroll);
        let range = strip.visible(scroll);
        let top = b.y + ((HEIGHT - CELL) / 2.0).round();
        let mount_at = |i: usize| rect((b.x + strip.cell_x(i) - scroll).round(), top, CELL, CELL);

        // Mounts and photos, then (in a layer above, as images draw over
        // every quad in theirs) the marks and the current photo's frame.
        let mut placed: Vec<(usize, &PhotoRecord, Rectangle)> = Vec::with_capacity(range.len());
        r.with_layer(b, |r| {
            paint::fill(r, b, t::PHOTO_GROUND);
            paint::hline(r, b.x, b.x + b.width, b.y, 1.0, t::PHOTO_LINE);
            for i in range.clone() {
                let Some(rec) = self.record(i) else { continue };
                let mount = mount_at(i);
                let hover = st.hover_amount(i, now);
                let base = if i == self.current { t::PHOTO_RAISED } else { t::PHOTO_MOUNT };
                paint::rounded(r, mount, t::R_CELL, paint::lerp_color(base, t::PHOTO_RAISED, hover));
                let photo_box = paint::inset(mount, PAD);
                let image = match self.thumbs.get(&rec.path) {
                    Some(th) if th.ok => {
                        let (tw, thh) = (th.width() as f32, th.height() as f32);
                        let s = (photo_box.width / tw).min(photo_box.height / thh);
                        let (w, h) = ((tw * s).round(), (thh * s).round());
                        let ir = rect(
                            photo_box.x + ((photo_box.width - w) / 2.0).floor(),
                            photo_box.y + ((photo_box.height - h) / 2.0).floor(),
                            w,
                            h,
                        );
                        let level = th.level_for(w.max(h) * self.scale_factor);
                        r.draw_image(
                            iced::advanced::image::Image::new(level.handle.clone())
                                .filter_method(FilterMethod::Linear)
                                .opacity(paint::arrival(th.arrived, now)),
                            ir,
                            b,
                        );
                        ir
                    }
                    _ => {
                        paint::rounded(r, photo_box, 2.0, t::PHOTO_GROUND);
                        photo_box
                    }
                };
                placed.push((i, rec, image));
            }
        });
        r.with_layer(b, |r| {
            let current_burst = self.bursts.get(self.current).copied().flatten();
            for (i, rec, ir) in placed {
                paint_marks(r, rec, ir);
                let m = mount_at(i);
                if i == self.current {
                    paint::framed(r, rect(m.x - 3.0, m.y - 3.0, m.width + 6.0, m.height + 6.0), t::R_CELL + 3.0, Color::TRANSPARENT, t::SIGNAL, 2.0);
                }
                if let Some((first, last)) = self.run_mark(i) {
                    // The burst being looked at reads a little brighter.
                    let ink = if self.bursts.get(i).copied().flatten() == current_burst { t::PHOTO_INK2 } else { t::PHOTO_INK4 };
                    let y = (m.y + m.height + BRACKET_DROP).round();
                    let (left, right) = (m.x - BRACKET_DROP, m.x + m.width + BRACKET_DROP - 1.0);
                    let x0 = if first { left } else { m.x - GAP };
                    let x1 = if last { right + 1.0 } else { m.x + m.width };
                    paint::hline(r, x0, x1, y, 1.0, ink);
                    if first {
                        paint::vline(r, left, y - BRACKET_RISE, y + 1.0, 1.0, ink);
                    }
                    if last {
                        paint::vline(r, right, y - BRACKET_RISE, y + 1.0, 1.0, ink);
                    }
                }
            }
        });
    }
}

/// The grid's marks, drawn small: crop framing, the prune cross-out and the
/// keep check.
fn paint_marks(r: &mut Renderer, rec: &PhotoRecord, ir: Rectangle) {
    if ir.width <= 0.0 || ir.height <= 0.0 {
        return;
    }
    if let Some(crop) = rec.crop.as_ref().filter(|c| !c.is_full() && !rec.is_pruned) {
        let (w, h) = (ir.width, ir.height);
        let cx = ir.x + (crop.left as f32 * w).floor();
        let cy = ir.y + (crop.top as f32 * h).floor();
        let cw = (crop.width() as f32 * w).floor().max(1.0);
        let ch = (crop.height() as f32 * h).floor().max(1.0);
        let scrim = t::alpha(Color::BLACK, 0.6);
        paint::fill(r, rect(ir.x, ir.y, w, cy - ir.y), scrim);
        paint::fill(r, rect(ir.x, cy + ch, w, ir.y + h - (cy + ch)), scrim);
        paint::fill(r, rect(ir.x, cy, cx - ir.x, ch), scrim);
        paint::fill(r, rect(cx + cw, cy, ir.x + w - (cx + cw), ch), scrim);
    }
    if rec.is_pruned {
        paint::fill(r, ir, t::alpha(t::PHOTO_GROUND, 0.62));
        let inset = (ir.width.min(ir.height) * 0.12).max(3.0);
        let (x0, y0, x1, y1) = (ir.x + inset, ir.y + inset, ir.x + ir.width - inset, ir.y + ir.height - inset);
        paint::strokes(
            r,
            &[(Point::new(x0, y0), Point::new(x1, y1)), (Point::new(x1, y0), Point::new(x0, y1))],
            2.5,
            t::alpha(t::REJECT, 0.92),
            true,
        );
    }
    if rec.is_marked {
        let d = 16.0;
        let (bx, by) = (ir.x + ir.width - d - 3.0, ir.y + 3.0);
        let centre = Point::new(bx + d / 2.0, by + d / 2.0);
        paint::circle(r, centre, d / 2.0 + 1.0, t::alpha(Color::BLACK, 0.35), Color::TRANSPARENT, 0.0);
        paint::circle(r, centre, d / 2.0, t::KEEP, Color::TRANSPARENT, 0.0);
        let u = d / 24.0;
        let p = |x: f32, y: f32| Point::new(bx + x * u, by + y * u);
        paint::strokes(r, &[(p(7.0, 12.5), p(10.5, 16.0)), (p(10.5, 16.0), p(17.0, 8.5))], 2.0, t::ON_SIGNAL, true);
    }
}

impl<'a, Message: 'a> From<Filmstrip<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(f: Filmstrip<'a, Message>) -> Self {
        Element::new(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: f32 = Strip::STRIDE;

    #[test]
    fn short_strip_is_centred() {
        let s = Strip { n: 3, width: 1000.0 };
        let content = s.content_width();
        assert_eq!(content, 2.0 * MARGIN + 3.0 * CELL + 2.0 * GAP);
        // Negative scroll: the content starts right of the view's left edge.
        assert!((s.centred(0) + (1000.0 - content) / 2.0).abs() < 1e-3);
        assert_eq!(s.centred(0), s.centred(2));
        assert_eq!(s.visible(s.centred(1)), 0..3);
    }

    #[test]
    fn long_strip_centres_and_clamps_at_the_ends() {
        let s = Strip { n: 1000, width: 740.0 };
        assert_eq!(s.centred(0), 0.0);
        assert_eq!(s.centred(999), s.content_width() - 740.0);
        let mid = s.centred(500);
        // Cell 500's centre sits at the view's centre.
        assert!((s.cell_x(500) + CELL / 2.0 - mid - 370.0).abs() < 1e-3);
        let vis = s.visible(mid);
        assert!(vis.contains(&500) && vis.len() <= (740.0 / S).ceil() as usize + 1);
    }

    #[test]
    fn visible_covers_partial_cells_only() {
        let s = Strip { n: 100, width: S * 4.0 };
        // Scrolled so cell 0 is just out of view on the left.
        let scroll = MARGIN + CELL + 0.5;
        let v = s.visible(scroll);
        assert_eq!(v.start, 1);
        // Cell 1 starts at MARGIN + S; the view ends at scroll + 4 S.
        let last_x = s.cell_x(v.end - 1);
        assert!(last_x < scroll + s.width && s.cell_x(v.end) >= scroll + s.width);
        assert_eq!(Strip { n: 0, width: 500.0 }.visible(0.0), 0..0);
    }

    #[test]
    fn hit_finds_cells_not_gaps() {
        let s = Strip { n: 10, width: 400.0 };
        assert_eq!(s.hit(0.0, MARGIN + 1.0), Some(0));
        assert_eq!(s.hit(0.0, MARGIN + CELL + GAP / 2.0), None);
        assert_eq!(s.hit(0.0, MARGIN + S + 1.0), Some(1));
        assert_eq!(s.hit(S * 2.0, MARGIN + 1.0), Some(2));
        assert_eq!(s.hit(0.0, 2.0), None);
        assert_eq!(s.hit(0.0, MARGIN + S * 10.0 + 1.0), None);
    }
}
