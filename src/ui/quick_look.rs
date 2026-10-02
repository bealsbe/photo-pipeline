//! Quick look: one photo large over the grid, without leaving it.
//!
//! The app says what to show — the grid's thumbnail (on screen at once),
//! then the camera's embedded preview and the full image as each arrives,
//! and the photo's marks — and this widget draws them fitted on the neutral
//! photo ground.  It owns only what moves: the entrance (the backdrop fades
//! in as the photo settles into place, as `Appear` does for dialogs) and the
//! pop of a mark as it is set.  A click outside the photo closes it; a
//! double-click on the photo opens the viewer.
//!
//! The better pictures are drawn over the worse ones rather than instead of
//! them: a large image uploads to the GPU on a worker thread, so for a frame
//! or two the one beneath is what shows, never an empty frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse as amouse};
use iced::mouse::{self, Cursor};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, touch, window};

use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::loader::FullImage;
use crate::imaging::thumbs::Level;
use crate::model::crop::Crop;

/// Space around the photo.
const MARGIN: f32 = t::S5;
/// The caption strip under the photo.
const CAPTION_H: f32 = 44.0;
/// How far below its place the photo starts, and the scale it grows from.
const RISE: f32 = 12.0;
const GROW_FROM: f32 = 0.965;
/// After the picture changes, keep drawing this long: a large image is
/// uploaded to the GPU on a worker and nothing else asks for the frame that
/// finally shows it.
const SETTLE: Duration = Duration::from_millis(300);

/// What the app shows.
#[derive(Clone)]
pub struct Shown {
    /// Changes when another photo is shown.
    pub key: u64,
    /// The grid's thumbnail: there at once, drawn under everything else.
    pub thumb: Option<Level>,
    pub preview: Option<Arc<FullImage>>,
    pub full: Option<Arc<FullImage>>,
    /// Said in place of a picture (a decode failure).
    pub status: Option<String>,
    pub name: String,
    pub types: Vec<(&'static str, Color)>,
    /// (1-based position, count) in the grid; `None` while it is hidden.
    pub position: Option<(usize, usize)>,
    pub when: String,
    /// (1-based frame, frames) when the photo is in a burst (B shows them all).
    pub burst: Option<(usize, usize)>,
    /// What the analysis says, in a few words ("Looks out of focus",
    /// "Sharpest of 8"), and its plate.
    pub hint: Option<(String, Option<crate::model::hints::Plate>)>,
    pub pruned: bool,
    pub marked: bool,
    pub crop: Option<Crop>,
    /// A library photo (search results): no P or M to offer.
    pub read_only: bool,
}

impl Shown {
    /// The picture's proportions: the real image's when known (a preview
    /// carries the full size), else the thumbnail's.
    fn size(&self) -> Option<(f32, f32)> {
        if let Some(img) = self.full.as_ref().or(self.preview.as_ref()) {
            return Some((img.width as f32, img.height as f32));
        }
        self.thumb.as_ref().map(|l| (l.width as f32, l.height as f32))
    }

    fn images(&self) -> (usize, usize) {
        let p = |i: &Option<Arc<FullImage>>| i.as_ref().map(|a| Arc::as_ptr(a) as usize).unwrap_or(0);
        (p(&self.preview), p(&self.full))
    }
}

#[derive(Debug, Clone)]
pub enum QuickLookEvent {
    /// A click outside the photo.
    Close,
    /// A double-click on the photo.
    Activate,
    /// The widget's size (the grid's), for choosing which level to upload.
    Viewport(Size),
}

/// The area the photo is fitted into: inside the margins, above the caption.
pub fn photo_area(bounds: Rectangle) -> Rectangle {
    rect(bounds.x + MARGIN, bounds.y + MARGIN, bounds.width - 2.0 * MARGIN, bounds.height - MARGIN - CAPTION_H)
}

/// A `w`×`h` picture fitted into [`photo_area`] and centred, on whole pixels.
pub fn photo_rect(bounds: Rectangle, w: f32, h: f32) -> Rectangle {
    let area = photo_area(bounds);
    if w <= 0.0 || h <= 0.0 || area.width <= 0.0 || area.height <= 0.0 {
        return rect(area.center_x(), area.center_y(), 0.0, 0.0);
    }
    let s = (area.width / w).min(area.height / h);
    let (pw, ph) = ((w * s).round(), (h * s).round());
    rect((area.x + (area.width - pw) / 2.0).round(), (area.y + (area.height - ph) / 2.0).round(), pw, ph)
}

fn progress(start: Instant, now: Instant, ms: f32) -> f32 {
    (now.saturating_duration_since(start).as_secs_f32() * 1000.0 / ms).clamp(0.0, 1.0)
}

pub struct QuickLook<'a, Message> {
    shown: Shown,
    scale_factor: f32,
    on: Box<dyn Fn(QuickLookEvent) -> Message + 'a>,
}

impl<'a, Message> QuickLook<'a, Message> {
    pub fn new(shown: Shown, scale_factor: f32, on: impl Fn(QuickLookEvent) -> Message + 'a) -> Self {
        Self { shown, scale_factor, on: Box::new(on) }
    }

    /// Where the photo is, when one can be placed.
    fn photo(&self, bounds: Rectangle) -> Rectangle {
        let (w, h) = self.shown.size().unwrap_or((3.0, 2.0));
        photo_rect(bounds, w, h)
    }
}

struct State {
    /// The first frame: the entrance runs from here.
    opened: Option<Instant>,
    key: u64,
    images: (usize, usize),
    marks: (bool, bool),
    /// When the prune and keep marks were set (each pops in).
    pruned_at: Option<Instant>,
    marked_at: Option<Instant>,
    settle_until: Instant,
    last_click: Option<amouse::Click>,
    reported: Size,
}

impl<Message> Widget<Message, iced::Theme, Renderer> for QuickLook<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            opened: None,
            key: self.shown.key,
            images: self.shown.images(),
            marks: (self.shown.pruned, self.shown.marked),
            pruned_at: None,
            marked_at: None,
            settle_until: Instant::now() + SETTLE,
            last_click: None,
            reported: Size::ZERO,
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        let marks = (self.shown.pruned, self.shown.marked);
        if st.key != self.shown.key {
            // Another photo: its marks are simply there.
            st.key = self.shown.key;
            st.pruned_at = None;
            st.marked_at = None;
            st.last_click = None;
        } else if st.marks != marks {
            if marks.0 && !st.marks.0 {
                st.pruned_at = Some(now);
            }
            if marks.1 && !st.marks.1 {
                st.marked_at = Some(now);
            }
        }
        st.marks = marks;
        let images = self.shown.images();
        if st.images != images {
            st.images = images;
            st.settle_until = now + SETTLE;
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _tree: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
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
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                crate::profile::frame("quick look", *now);
                let start = *st.opened.get_or_insert(*now);
                if b.size() != st.reported {
                    st.reported = b.size();
                    shell.publish((self.on)(QuickLookEvent::Viewport(b.size())));
                }
                let popping = |at: Option<Instant>| at.is_some_and(|a| progress(a, *now, t::MOTION_MS) < 1.0);
                let loading = self.shown.full.is_none() && self.shown.preview.is_none() && self.shown.thumb.is_none() && self.shown.status.is_none();
                if progress(start, *now, t::MOTION_MS) < 1.0
                    || popping(st.pruned_at)
                    || popping(st.marked_at)
                    || *now < st.settle_until
                    || loading
                {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let Some(p) = cursor.position_over(b) else { return };
                shell.capture_event();
                if *button != mouse::Button::Left {
                    return;
                }
                if self.photo(b).contains(p) {
                    let click = amouse::Click::new(p, mouse::Button::Left, st.last_click);
                    st.last_click = Some(click);
                    if click.kind() == amouse::click::Kind::Double {
                        shell.publish((self.on)(QuickLookEvent::Activate));
                    }
                } else {
                    st.last_click = None;
                    shell.publish((self.on)(QuickLookEvent::Close));
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(_)) | Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                if cursor.is_over(b) {
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerPressed { position, .. }) => {
                if b.contains(*position) {
                    shell.capture_event();
                    if !self.photo(b).contains(*position) {
                        shell.publish((self.on)(QuickLookEvent::Close));
                    }
                }
            }
            Event::Touch(touch::Event::FingerMoved { position, .. } | touch::Event::FingerLifted { position, .. }) => {
                if b.contains(*position) {
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        // Anything but `None`, so the grid beneath sees no hover.
        if cursor.is_over(layout.bounds()) { mouse::Interaction::Idle } else { mouse::Interaction::None }
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
        let _t = crate::profile::span("draw", "quick look");
        crate::profile::since("quick look", "to first frame");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let s = &self.shown;
        // Entrance: 0 → 1.
        let k = st.opened.map(|o| paint::out_cubic(progress(o, now, t::MOTION_MS))).unwrap_or(0.0);
        let photo = self.photo(b);
        let grow = paint::lerp(GROW_FROM, 1.0, k);
        let c = photo.center();
        let (w, h) = (photo.width * grow, photo.height * grow);
        let dst = rect(c.x - w / 2.0, c.y - h / 2.0 + (1.0 - k) * RISE, w, h);
        let sf = self.scale_factor;

        r.with_layer(b, |r| {
            paint::fill(r, b, t::alpha(t::PHOTO_GROUND, k));
            let pictured = s.thumb.is_some() || s.preview.is_some() || s.full.is_some();
            if !pictured {
                if s.status.is_none() {
                    let phase = now.saturating_duration_since(st.opened.unwrap_or(now)).as_secs_f32() % 1.3 / 1.3;
                    paint::shimmer(r, dst, 2.0, t::alpha(t::PHOTO_MOUNT, k), t::alpha(Color::WHITE, 0.04 * k), phase);
                }
                return;
            }
            let image = |r: &mut Renderer, l: &Level| {
                r.draw_image(iced::advanced::image::Image::new(l.handle.clone()).filter_method(FilterMethod::Linear).opacity(k), dst, b);
            };
            if let Some(l) = &s.thumb {
                image(r, l);
            }
            for img in [&s.preview, &s.full].into_iter().flatten() {
                // The level nearest the size on screen, over the next coarser.
                let i = img.level_for_scale(photo.width / img.width.max(1) as f32 * sf);
                let coarse = (i + 1).min(img.levels.len() - 1);
                image(r, &img.levels[coarse]);
                if coarse != i {
                    image(r, &img.levels[i]);
                }
            }
        });

        // Marks, the caption and any status sit above the pictures.
        r.with_layer(b, |r| {
            if let Some(status) = &s.status {
                if s.full.is_none() && s.preview.is_none() {
                    paint::text(r, status, rect(dst.x, dst.center_y() - 13.0, dst.width, 26.0), t::T_TITLE, paint::REGULAR, t::alpha(t::PHOTO_INK3, k), HAlign::Center, b);
                }
            }
            draw_marks(r, s, dst, k, now, st);
            draw_caption(r, s, b, k);
        });
    }
}

impl<'a, Message: 'a> From<QuickLook<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(q: QuickLook<'a, Message>) -> Self {
        Element::new(q)
    }
}

/// The crop framing, the prune cross-out and the keep check, as the grid
/// draws them on a thumbnail.
fn draw_marks(r: &mut Renderer, s: &Shown, ir: Rectangle, k: f32, now: Instant, st: &State) {
    let pop = |at: Option<Instant>| at.map(|a| paint::out_cubic(progress(a, now, t::MOTION_MS))).unwrap_or(1.0);
    if let Some(crop) = s.crop.as_ref().filter(|c| !c.is_full() && !s.pruned) {
        let (w, h) = (ir.width, ir.height);
        let cx = ir.x + (crop.left as f32 * w).floor();
        let cy = ir.y + (crop.top as f32 * h).floor();
        let cw = (crop.width() as f32 * w).floor().max(1.0);
        let ch = (crop.height() as f32 * h).floor().max(1.0);
        let scrim = t::alpha(Color::BLACK, 0.6 * k);
        paint::fill(r, rect(ir.x, ir.y, w, cy - ir.y), scrim);
        paint::fill(r, rect(ir.x, cy + ch, w, ir.y + h - (cy + ch)), scrim);
        paint::fill(r, rect(ir.x, cy, cx - ir.x, ch), scrim);
        paint::fill(r, rect(cx + cw, cy, ir.x + w - (cx + cw), ch), scrim);
        paint::framed(r, rect(cx, cy, cw, ch), 0.0, Color::TRANSPARENT, t::alpha(Color::WHITE, 0.85 * k), 1.0);
    }
    if s.pruned {
        // Lighter than the grid's: the photo stays judgeable at this size.
        let p = pop(st.pruned_at) * k;
        paint::fill(r, ir, t::alpha(t::PHOTO_GROUND, 0.5 * p));
        let inset = (ir.width.min(ir.height) * 0.08).max(6.0);
        let (x0, y0, x1, y1) = (ir.x + inset, ir.y + inset, ir.x + ir.width - inset, ir.y + ir.height - inset);
        paint::strokes(r, &[(Point::new(x0, y0), Point::new(x1, y1)), (Point::new(x1, y0), Point::new(x0, y1))], 5.0, t::alpha(t::REJECT, 0.9 * p), true);
    }
    if s.marked {
        let p = pop(st.marked_at);
        let d = 34.0 * paint::lerp(0.6, 1.0, p);
        let centre = Point::new(ir.x + ir.width - 12.0 - 17.0, ir.y + 12.0 + 17.0);
        let a = p * k;
        paint::circle(r, centre, d / 2.0 + 2.0, t::alpha(Color::BLACK, 0.35 * a), Color::TRANSPARENT, 0.0);
        paint::circle(r, centre, d / 2.0, t::alpha(t::KEEP, a), Color::TRANSPARENT, 0.0);
        let u = d / 24.0;
        let (bx, by) = (centre.x - d / 2.0, centre.y - d / 2.0);
        let q = |x: f32, y: f32| Point::new(bx + x * u, by + y * u);
        paint::strokes(r, &[(q(7.0, 12.5), q(10.5, 16.0)), (q(10.5, 16.0), q(17.0, 8.5))], (d / 10.0).max(2.0), t::alpha(t::ON_SIGNAL, a), true);
    }
}

const HINTS: &[(&str, &str)] = &[("←  →", "Browse"), ("P", "Prune"), ("M", "Keep"), ("Enter", "Open"), ("Space", "Close")];
const READ_ONLY_HINTS: &[(&str, &str)] = &[("←  →", "Browse"), ("Enter", "Open"), ("Space", "Close")];

/// Name, file types, place in the grid, capture time and marks on the left;
/// the keys on the right, while there is room.
fn draw_caption(r: &mut Renderer, s: &Shown, b: Rectangle, k: f32) {
    let strip = rect(b.x + MARGIN, b.y + b.height - CAPTION_H, b.width - 2.0 * MARGIN, CAPTION_H);
    if strip.width <= 0.0 {
        return;
    }
    let ink = |c: Color| t::alpha(c, c.a * k);
    let right = strip.x + strip.width;
    let mut x = strip.x;
    let put = |r: &mut Renderer, x: &mut f32, text: &str, size: f32, font: iced::Font, color: Color, gap: f32| {
        if text.is_empty() || *x >= right {
            return;
        }
        let w = paint::text_width(text, size, font).min(right - *x);
        paint::text(r, text, rect(*x, strip.y, w, strip.height), size, font, ink(color), HAlign::Left, rect(*x, strip.y, w + 1.0, strip.height));
        *x += w + gap;
    };
    let name_max = (strip.width * 0.45).max(80.0);
    let name = paint::elide_start(&s.name, t::T_BODY, paint::SEMIBOLD, name_max);
    put(r, &mut x, &name, t::T_BODY, paint::SEMIBOLD, t::PHOTO_INK, t::S3);
    for (label, tone) in &s.types {
        put(r, &mut x, label, t::T_SMALL, paint::SEMIBOLD, *tone, t::S2);
    }
    x += t::S1;
    if let Some((i, n)) = s.position {
        put(r, &mut x, &format!("{} of {}", crate::util::thousands(i as u64), crate::util::thousands(n as u64)), t::T_SMALL, paint::REGULAR, t::PHOTO_INK3, t::S3);
    }
    put(r, &mut x, &s.when, t::T_SMALL, paint::REGULAR, t::PHOTO_INK3, t::S4);
    if let Some((i, n)) = s.burst {
        put(r, &mut x, &format!("Burst frame {i} of {n}"), t::T_SMALL, paint::REGULAR, t::PHOTO_INK2, t::S4);
    }
    if let Some((words, plate)) = &s.hint {
        // The plate's glyph, then the words, as the grid's plate says it.
        let g = 13.0;
        if let Some(plate) = plate.filter(|_| x + g < right) {
            let at = rect(x.round(), (strip.center_y() - g / 2.0).round(), g, g);
            let handle = super::icons::handle(super::hint_plate::glyph(plate), g, super::hint_plate::tone(plate));
            r.draw_image(iced::advanced::image::Image::new(handle).filter_method(FilterMethod::Linear).opacity(k), at, b);
            x += g + 5.0;
        }
        let tone = if *plate == Some(crate::model::hints::Plate::Accidental) { t::WARN } else { t::PHOTO_INK2 };
        put(r, &mut x, words, t::T_SMALL, paint::MEDIUM, tone, t::S4);
    }
    if s.marked {
        put(r, &mut x, "Kept", t::T_SMALL, paint::SEMIBOLD, t::KEEP, t::S3);
    }
    if s.pruned {
        put(r, &mut x, "Marked to prune", t::T_SMALL, paint::SEMIBOLD, t::REJECT, t::S3);
    }

    // The keys, right-aligned, if they fit after the facts.
    let hints = if s.read_only { READ_ONLY_HINTS } else { HINTS };
    let pieces: Vec<(f32, f32)> = hints
        .iter()
        .map(|(key, what)| (paint::text_width(key, t::T_SMALL, paint::SEMIBOLD), paint::text_width(what, t::T_SMALL, paint::REGULAR)))
        .collect();
    let total: f32 = pieces.iter().map(|(a, b)| a + 6.0 + b).sum::<f32>() + t::S4 * (hints.len() as f32 - 1.0);
    let mut hx = right - total;
    if hx < x + t::S5 {
        return;
    }
    for ((key, what), (kw, ww)) in hints.iter().zip(pieces) {
        paint::text(r, key, rect(hx, strip.y, kw, strip.height), t::T_SMALL, paint::SEMIBOLD, ink(t::PHOTO_INK2), HAlign::Left, b);
        hx += kw + 6.0;
        paint::text(r, what, rect(hx, strip.y, ww, strip.height), t::T_SMALL, paint::REGULAR, ink(t::PHOTO_INK3), HAlign::Left, b);
        hx += ww + t::S4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rectangle {
        rect(100.0, 50.0, 1000.0, 700.0)
    }

    #[test]
    fn photo_fits_the_area_and_is_centred() {
        let area = photo_area(bounds());
        // Landscape 3:2 in a wide area: limited by height.
        let p = photo_rect(bounds(), 6000.0, 4000.0);
        assert!(p.height <= area.height + 0.5 && (p.height - area.height).abs() <= 1.0);
        assert!((p.width / p.height - 1.5).abs() < 0.01);
        assert!((p.center_x() - area.center_x()).abs() <= 1.0);
        assert!((p.center_y() - area.center_y()).abs() <= 1.0);
        // Portrait: limited by height too, narrow.
        let q = photo_rect(bounds(), 4000.0, 6000.0);
        assert!(q.width < q.height && q.x > area.x);
        // Very wide panorama: limited by width.
        let pano = photo_rect(bounds(), 12000.0, 2000.0);
        assert!((pano.width - area.width).abs() <= 1.0 && pano.height < area.height);
    }

    #[test]
    fn photo_leaves_room_for_the_caption() {
        let p = photo_rect(bounds(), 4000.0, 6000.0);
        let b = bounds();
        assert!(p.y + p.height <= b.y + b.height - CAPTION_H + 0.5);
        assert!(p.y >= b.y + MARGIN - 0.5);
    }

    #[test]
    fn degenerate_sizes_give_an_empty_rect() {
        assert_eq!(photo_rect(bounds(), 0.0, 100.0).width, 0.0);
        assert_eq!(photo_rect(rect(0.0, 0.0, 20.0, 20.0), 300.0, 200.0).width, 0.0);
    }
}
