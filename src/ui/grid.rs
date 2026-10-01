//! Thumbnail grid grouped by shot date — one virtualised widget.
//!
//! Only the rows on screen are drawn, so 10 000 records cost the same as 100.
//! The app owns the data (sections, selection, collapsed dates, the target
//! thumbnail size); this widget owns everything that moves every frame:
//! scroll position and its easing, the zoom animation, section collapse
//! animations, hover, tooltips, the select-mode pill's fade and touch
//! kinetics.  It reports discrete events (clicks, activation, which
//! thumbnails are on screen) back to the app.
//!
//! Each photo sits in a "mount" — a raised frame with the file name below —
//! like slides on a light box.  Decisions are drawn on the photo itself: a
//! pruned frame is dimmed and crossed out corner to corner, as a grease
//! pencil marks a contact sheet; a kept frame wears a green check on its
//! corner; crops scrim the margins they discard.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse as amouse};
use iced::keyboard::{self, key::Named};
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, touch, window};

use super::icons::{self, Glyph};
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::thumbs::ThumbCache;
use crate::model::bursts::RunMark;
use crate::model::collection::{Collection, RecordId};
use crate::model::record::{FileType, PhotoRecord};
use crate::util;

/// Space between a mount's edge and its photo.
pub const PAD: f32 = 6.0;
/// The file-name strip under the photo (none when names are hidden).
pub const LABEL_H: f32 = 24.0;
/// Between the file name and its tag (whose point adds a little more).
const NAME_GAP: f32 = 4.0;
/// The tag's inset from the photo's corner when names are hidden.
const CORNER_INSET: f32 = 4.0;
/// Gutter between mounts.
const GAP: f32 = 8.0;
pub const HEADER_H: f32 = 48.0;
const TOP_MARGIN: f32 = 4.0;
const BOTTOM_MARGIN: f32 = 16.0;
const LEFT: f32 = 16.0;
const SCROLLBAR_W: f32 = 10.0;
const PREFETCH_SETTLE: Duration = Duration::from_millis(80);
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);
pub const MIN_SIZE: f32 = 120.0;
pub const MAX_SIZE: f32 = 480.0;
/// A burst's bracket under its mounts: colour, weight, how far it sits
/// outside them and how high its ends rise.
const BRACKET: Color = t::PHOTO_INK3;
const BRACKET_W: f32 = 1.0;
const BRACKET_DROP: f32 = 3.0;
const BRACKET_RISE: f32 = 14.0;


/// One date group.
#[derive(Debug, Clone)]
pub struct Section {
    pub date: NaiveDate,
    pub label: String,
    pub ids: Vec<RecordId>,
}

/// What the grid reports to the app.
#[derive(Debug, Clone)]
pub enum GridEvent {
    Click { id: RecordId, modifiers: keyboard::Modifiers },
    Activate(RecordId),
    ClearSelection,
    ToggleCollapse(NaiveDate),
    Chip(NaiveDate),
    ZoomStep(i32),
    PinchTo(f32),
    /// Thumbnails wanted: `visible` on screen now, `nearby` one screen either
    /// side.  `settle` means scrolling stopped — reprioritise the queue.
    Thumbs { visible: Vec<RecordId>, nearby: Vec<RecordId>, settle: bool },
    Scrolled(f32),
    SelectDone,
}

/// The data a grid paints; borrowed from the app.
#[derive(Clone, Copy)]
pub struct GridData<'a> {
    pub sections: &'a [Section],
    pub coll: &'a Collection,
    pub thumbs: &'a ThumbCache,
    pub selection: &'a HashSet<RecordId>,
    pub collapsed: &'a HashSet<NaiveDate>,
    pub select_mode: bool,
    pub scale_factor: f32,
    /// File names under the photos (View ▸ Show file names).
    pub show_names: bool,
    /// Burst brackets, per section and tile (empty: none).
    pub bursts: &'a [Vec<Option<RunMark>>],
}

/// The stride of one cell: its mount plus the gutter.  `label_h` is the
/// name strip's height: [`LABEL_H`], 0 with names hidden, or in between
/// while that animates.
pub fn cell_size(ts: f32, label_h: f32) -> Size {
    Size::new(ts + PAD * 2.0 + GAP, ts + PAD * 2.0 + label_h + GAP)
}

/// The label strip under a photo `ts` wide: (name width, tag x), from the
/// strip's left edge.  The tag sits flush right; the name has the rest.
fn label_layout(ts: f32, tag_w: f32) -> (f32, f32) {
    let tag_x = ts - tag_w;
    ((tag_x - NAME_GAP).max(0.0), tag_x)
}

/// With names hidden the tag moves onto the mount, in the photo's
/// bottom-right corner: (tag, plate).  The plate is a patch of mount behind
/// the tag, so it reads over a photo that reaches the corner (a landscape
/// photo leaves the corner empty, and the plate vanishes into the mount).
fn corner_tag(thumb: Rectangle, tag_w: f32) -> (Rectangle, Rectangle) {
    let right = thumb.x + thumb.width;
    let bottom = thumb.y + thumb.height;
    let tag = rect(right - CORNER_INSET - tag_w, bottom - CORNER_INSET - super::tags::TAG_H, tag_w, super::tags::TAG_H);
    let plate = rect(tag.x - CORNER_INSET + 1.0, tag.y - CORNER_INSET + 1.0, right - (tag.x - CORNER_INSET + 1.0), bottom - (tag.y - CORNER_INSET + 1.0));
    (tag, plate)
}

/// The mount inside a cell's stride.
fn mount_rect(cell: Rectangle) -> Rectangle {
    rect(cell.x, cell.y, cell.width - GAP, cell.height - GAP)
}

fn cols_for(width: f32, ts: f32) -> usize {
    let cw = cell_size(ts, 0.0).width;
    ((width - LEFT).max(cw) / cw).floor().max(1.0) as usize
}

fn grid_height(n: usize, cols: usize, row_h: f32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    n.div_ceil(cols) as f32 * row_h
}

/// Geometry of one frame of the grid.
struct Metrics {
    ts: f32,
    cols: usize,
    cell: Size,
    /// The name strip's height (see [`cell_size`]).
    label_h: f32,
    content_w: f32,
    /// (top, openness) per section, in content coordinates.
    tops: Vec<(f32, f32)>,
    total: f32,
}

impl Metrics {
    fn new(sections: &[Section], width: f32, ts: f32, label_h: f32, openness: &dyn Fn(NaiveDate) -> f32) -> Metrics {
        let content_w = (width - SCROLLBAR_W).max(1.0);
        let cols = cols_for(content_w, ts);
        let cell = cell_size(ts, label_h);
        let mut y = TOP_MARGIN;
        let mut tops = Vec::with_capacity(sections.len());
        for s in sections {
            let o = openness(s.date);
            tops.push((y, o));
            y += HEADER_H + grid_height(s.ids.len(), cols, cell.height) * o;
        }
        Metrics { ts, cols, cell, label_h, content_w, tops, total: y + BOTTOM_MARGIN }
    }

    fn cell_rect(&self, section: usize, index: usize) -> Rectangle {
        let (top, _) = self.tops[section];
        let row = index / self.cols;
        let col = index % self.cols;
        rect(LEFT + col as f32 * self.cell.width, top + HEADER_H + row as f32 * self.cell.height, self.cell.width, self.cell.height)
    }

    /// (section, index) of the cell at a content-space point.
    fn hit_cell(&self, sections: &[Section], p: Point) -> Option<(usize, usize)> {
        let s = self.section_at(p.y)?;
        let (top, o) = self.tops[s];
        let gy = p.y - top - HEADER_H;
        if gy < 0.0 || o < 0.999 {
            return None;
        }
        let row = (gy / self.cell.height).floor() as usize;
        let col_f = (p.x - LEFT) / self.cell.width;
        if col_f < 0.0 {
            return None;
        }
        let col = col_f.floor() as usize;
        if col >= self.cols {
            return None;
        }
        let idx = row * self.cols + col;
        (idx < sections[s].ids.len()).then_some((s, idx))
    }

    fn section_at(&self, y: f32) -> Option<usize> {
        let i = self.tops.partition_point(|(top, _)| *top <= y);
        (i > 0).then(|| i - 1)
    }

    fn header_rect(&self, s: usize) -> Rectangle {
        rect(0.0, self.tops[s].0, self.content_w, HEADER_H)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hover {
    None,
    Cell(RecordId),
    Header(usize),
    Chip(usize),
    BarDone,
    Scrollbar,
}

#[derive(Clone, Copy)]
struct Anim {
    from: f32,
    to: f32,
    start: Instant,
    ms: f32,
}

impl Anim {
    fn value(&self, now: Instant, ease: fn(f32) -> f32) -> f32 {
        let t = (now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.ms).clamp(0.0, 1.0);
        paint::lerp(self.from, self.to, ease(t))
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 >= self.ms
    }
}

struct Touch {
    fingers: HashMap<touch::Finger, Point>,
    last_y: f32,
    last_t: Instant,
    velocity: f32,
    start: Point,
    moved: bool,
    pinch_start_dist: f32,
    pinch_start_size: f32,
}

pub struct State {
    scroll: f32,
    scroll_anim: Option<Anim>,
    size: f32,
    zoom: Option<Anim>,
    last_target: f32,
    /// The name strip's height, animating when names are shown or hidden.
    label_h: f32,
    label_anim: Option<Anim>,
    last_names: bool,
    collapse: HashMap<NaiveDate, Anim>,
    last_collapsed: HashSet<NaiveDate>,
    hover: Hover,
    /// When the hovered cell was entered, and the one before it left: both
    /// fade (hover is a quick cross-fade, not a flicker).
    hover_start: Instant,
    hover_prev: Option<(RecordId, Instant)>,
    cursor: Point,
    hover_since: Instant,
    last_click: Option<amouse::Click>,
    bar: Anim,
    last_select_mode: bool,
    sent_range: Option<(usize, usize, usize, usize, u64)>,
    settle_at: Option<Instant>,
    layout_key: u64,
    elide: RefCell<HashMap<(RecordId, u32), String>>,
    viewport: Rectangle,
    drag_bar: Option<f32>,
    kinetic: Option<Anim>,
    touch: Option<Touch>,
    last_frame: Instant,
    initialised: bool,
    scroll_seq: u64,
    reported_scroll: f32,
}

impl State {
    fn new(size: f32, names: bool) -> Self {
        let now = Instant::now();
        Self {
            scroll: 0.0,
            scroll_anim: None,
            size,
            zoom: None,
            last_target: size,
            label_h: if names { LABEL_H } else { 0.0 },
            label_anim: None,
            last_names: names,
            collapse: HashMap::new(),
            last_collapsed: HashSet::new(),
            hover: Hover::None,
            hover_start: now,
            hover_prev: None,
            cursor: Point::ORIGIN,
            hover_since: now,
            last_click: None,
            bar: Anim { from: 0.0, to: 0.0, start: now, ms: 180.0 },
            last_select_mode: false,
            sent_range: None,
            settle_at: None,
            layout_key: u64::MAX,
            elide: RefCell::new(HashMap::new()),
            viewport: Rectangle::default(),
            drag_bar: None,
            kinetic: None,
            touch: None,
            last_frame: now,
            initialised: false,
            scroll_seq: 0,
            reported_scroll: 0.0,
        }
    }

    fn openness(&self, date: NaiveDate, collapsed: &HashSet<NaiveDate>, now: Instant) -> f32 {
        match self.collapse.get(&date) {
            Some(a) if !a.done(now) => a.value(now, paint::out_expo),
            _ => {
                if collapsed.contains(&date) {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }

    fn target_scroll(&self) -> f32 {
        self.scroll_anim.map(|a| a.to).unwrap_or(self.scroll)
    }

    /// 0–1: how far `id`'s mount has lit up under the cursor.
    fn hover_amount(&self, id: RecordId, now: Instant) -> f32 {
        let k = |since: Instant| (now.saturating_duration_since(since).as_secs_f32() * 1000.0 / t::MOTION_FAST_MS).clamp(0.0, 1.0);
        if self.hover == Hover::Cell(id) {
            return paint::out_cubic(k(self.hover_start));
        }
        match self.hover_prev {
            Some((prev, left)) if prev == id => 1.0 - paint::out_cubic(k(left)),
            _ => 0.0,
        }
    }

    fn hover_fading(&self, now: Instant) -> bool {
        let busy = |since: Instant| now.saturating_duration_since(since).as_secs_f32() * 1000.0 < t::MOTION_FAST_MS;
        (matches!(self.hover, Hover::Cell(_)) && busy(self.hover_start)) || self.hover_prev.is_some_and(|(_, left)| busy(left))
    }

    fn set_hover(&mut self, new: Hover, now: Instant) {
        if let Hover::Cell(old) = self.hover {
            self.hover_prev = Some((old, now));
        }
        if let Hover::Cell(_) = new {
            self.hover_start = now;
        }
        self.hover = new;
    }
}

/// When a thumbnail landed → its fade-in (0–1).
fn arrival(arrived: Instant, now: Instant) -> f32 {
    paint::out_cubic((now.saturating_duration_since(arrived).as_secs_f32() * 1000.0 / t::MOTION_MS).clamp(0.0, 1.0))
}

/// The loading sweep's position, shared by every placeholder so they move
/// together.
fn shimmer_phase(now: Instant) -> f32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    (now.saturating_duration_since(start).as_secs_f32() % 1.3) / 1.3
}

pub struct Grid<'a, Message> {
    data: GridData<'a>,
    thumb_target: f32,
    keyboard: bool,
    initial_scroll: f32,
    scroll_seq: u64,
    layout_key: u64,
    on: Box<dyn Fn(GridEvent) -> Message + 'a>,
}

impl<'a, Message> Grid<'a, Message> {
    pub fn new(data: GridData<'a>, thumb_target: f32, on: impl Fn(GridEvent) -> Message + 'a) -> Self {
        Self { data, thumb_target, keyboard: true, initial_scroll: 0.0, scroll_seq: 0, layout_key: 0, on: Box::new(on) }
    }

    /// Whether keyboard scrolling is live (off while a dialog is open).
    pub fn keyboard(mut self, on: bool) -> Self {
        self.keyboard = on;
        self
    }

    /// Scroll offset to start from when the widget is (re)created.
    pub fn initial_scroll(mut self, y: f32) -> Self {
        self.initial_scroll = y;
        self
    }

    /// Bump to force the scroll position back to `initial_scroll`.
    pub fn scroll_seq(mut self, seq: u64) -> Self {
        self.scroll_seq = seq;
        self
    }

    /// Changes whenever the sections are rebuilt.
    pub fn layout_key(mut self, key: u64) -> Self {
        self.layout_key = key;
        self
    }
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for Grid<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        let mut s = State::new(self.thumb_target, self.data.show_names);
        s.scroll = self.initial_scroll;
        s.scroll_seq = self.scroll_seq;
        s.last_collapsed = self.data.collapsed.clone();
        s.last_select_mode = self.data.select_mode;
        s.bar = Anim {
            from: if self.data.select_mode { 1.0 } else { 0.0 },
            to: if self.data.select_mode { 1.0 } else { 0.0 },
            start: Instant::now(),
            ms: 180.0,
        };
        tree::State::new(s)
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let now = Instant::now();
        let st = tree.state.downcast_mut::<State>();
        // Zoom target moved: animate to it, chasing the latest value.
        if (self.thumb_target - st.last_target).abs() > 0.01 {
            let from = st.size;
            st.zoom = Some(Anim { from, to: self.thumb_target, start: now, ms: 160.0 });
            st.last_target = self.thumb_target;
        }
        // Names shown or hidden: the strip grows or shrinks, like a zoom.
        if st.last_names != self.data.show_names {
            let to = if self.data.show_names { LABEL_H } else { 0.0 };
            st.label_anim = Some(Anim { from: st.label_h, to, start: now, ms: t::MOTION_MS });
            st.last_names = self.data.show_names;
        }
        // Sections collapsed / expanded.
        if st.last_collapsed != *self.data.collapsed {
            for d in st.last_collapsed.symmetric_difference(self.data.collapsed) {
                let target = if self.data.collapsed.contains(d) { 0.0 } else { 1.0 };
                let current = match st.collapse.get(d) {
                    Some(a) if !a.done(now) => a.value(now, paint::out_expo),
                    _ => 1.0 - target,
                };
                st.collapse.insert(*d, Anim { from: current, to: target, start: now, ms: 110.0 });
            }
            st.last_collapsed = self.data.collapsed.clone();
        }
        if st.last_select_mode != self.data.select_mode {
            let cur = st.bar.value(now, paint::out_cubic);
            st.bar = Anim { from: cur, to: if self.data.select_mode { 1.0 } else { 0.0 }, start: now, ms: 180.0 };
            st.last_select_mode = self.data.select_mode;
        }
        if st.scroll_seq != self.scroll_seq {
            st.scroll_seq = self.scroll_seq;
            st.scroll = self.initial_scroll;
            st.scroll_anim = None;
            st.kinetic = None;
            st.sent_range = None;
        }
        if st.layout_key != self.layout_key {
            st.layout_key = self.layout_key;
            st.sent_range = None;
            st.hover = Hover::None;
            if st.elide.borrow().len() > 20_000 {
                st.elide.borrow_mut().clear();
            }
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
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        st.viewport = bounds;
        let now = Instant::now();
        let data = self.data;
        let collapsed = data.collapsed;

        // Advance animations to "now" before handling input, keeping the
        // cell under the viewport centre in place while the size animates.
        let anchor = (st.zoom.is_some() || st.label_anim.is_some()).then(|| {
            let st_ref: &State = st;
            let m = Metrics::new(data.sections, bounds.width, st_ref.size, st_ref.label_h, &|d| st_ref.openness(d, collapsed, now));
            locate(&m, data.sections, st_ref.scroll + bounds.height / 2.0)
        });
        advance(st, now);
        if let Some(Some(anchor)) = anchor {
            let st_ref: &State = st;
            let m = Metrics::new(data.sections, bounds.width, st_ref.size, st_ref.label_h, &|d| st_ref.openness(d, collapsed, now));
            st.scroll = anchor_y(&m, anchor) - bounds.height / 2.0;
        }
        let metrics = {
            let st_ref: &State = st;
            Metrics::new(data.sections, bounds.width, st_ref.size, st_ref.label_h, &|d| st_ref.openness(d, collapsed, now))
        };
        let max_scroll = (metrics.total - bounds.height).max(0.0);
        if !st.initialised {
            st.initialised = true;
            st.scroll = st.scroll.clamp(0.0, max_scroll);
        }
        clamp_scroll(st, max_scroll);

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("grid", *at);
                st.last_frame = now;
                let animating = st.scroll_anim.is_some()
                    || st.zoom.is_some()
                    || st.label_anim.is_some()
                    || st.kinetic.is_some()
                    || st.collapse.values().any(|a| !a.done(now))
                    || !st.bar.done(now);
                // Thumbnails fading in, placeholders shimmering, a hover fade.
                let (on_screen, _) = collect(&metrics, data, st.scroll, bounds.height, false);
                let lively = st.hover_fading(now)
                    || on_screen.iter().filter_map(|id| data.coll.get(*id)).any(|r| match data.thumbs.get(&r.path) {
                        None => true,
                        Some(th) => arrival(th.arrived, now) < 1.0,
                    });
                if animating || lively {
                    shell.request_redraw();
                }
                // Thumbnail scheduling.
                let range = visible_range(&metrics, data.sections, st.scroll, bounds.height);
                let key = (range.0, range.1, range.2, range.3, st.layout_key);
                if st.sent_range != Some(key) {
                    st.sent_range = Some(key);
                    let (visible, _) = collect(&metrics, data, st.scroll, bounds.height, false);
                    let missing: Vec<RecordId> = visible
                        .into_iter()
                        .filter(|id| data.coll.get(*id).is_some_and(|r| data.thumbs.get(&r.path).is_none()))
                        .collect();
                    if !missing.is_empty() {
                        shell.publish((self.on)(GridEvent::Thumbs { visible: missing, nearby: Vec::new(), settle: false }));
                    }
                    st.settle_at = Some(now + PREFETCH_SETTLE);
                    shell.request_redraw_at(window::RedrawRequest::At(now + PREFETCH_SETTLE));
                }
                if let Some(at) = st.settle_at {
                    if now >= at {
                        st.settle_at = None;
                        let (visible, nearby) = collect(&metrics, data, st.scroll, bounds.height, true);
                        shell.publish((self.on)(GridEvent::Thumbs { visible, nearby, settle: true }));
                        if (st.scroll - st.reported_scroll).abs() > 0.5 {
                            st.reported_scroll = st.scroll;
                            shell.publish((self.on)(GridEvent::Scrolled(st.scroll)));
                        }
                    } else {
                        shell.request_redraw_at(window::RedrawRequest::At(at));
                    }
                }
                // Tooltip timer.
                if let Hover::Cell(_) = st.hover {
                    let at = st.hover_since + TOOLTIP_DELAY;
                    if now < at {
                        shell.request_redraw_at(window::RedrawRequest::At(at));
                    }
                }
            }

            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(_) = cursor.position_in(bounds) else { return };
                let mods = current_modifiers();
                let (dy, lines) = match delta {
                    ScrollDelta::Lines { y, .. } => (*y, true),
                    ScrollDelta::Pixels { y, .. } => (*y, false),
                };
                if mods.control() {
                    if dy != 0.0 {
                        shell.publish((self.on)(GridEvent::ZoomStep(if dy > 0.0 { 1 } else { -1 })));
                    }
                } else if lines {
                    let base = st.target_scroll();
                    smooth_scroll_to(st, base - dy * 100.0, max_scroll, now);
                } else {
                    st.scroll_anim = None;
                    st.kinetic = None;
                    st.scroll = (st.scroll - dy).clamp(0.0, max_scroll);
                }
                st.hover_since = now;
                shell.capture_event();
                shell.request_redraw();
            }

            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let pos = cursor.position();
                if let Some(grab) = st.drag_bar {
                    if let Some(p) = pos {
                        let (_, track_h, thumb_h) = scrollbar_geometry(bounds, metrics.total, st.scroll);
                        let usable = (track_h - thumb_h).max(1.0);
                        let frac = ((p.y - bounds.y - grab) / usable).clamp(0.0, 1.0);
                        st.scroll = frac * max_scroll;
                        st.scroll_anim = None;
                        shell.request_redraw();
                    }
                    return;
                }
                let new_hover = match cursor.position_in(bounds) {
                    Some(local) => hit(st, &metrics, data, bounds, local),
                    None => Hover::None,
                };
                if let Some(p) = pos {
                    if p != st.cursor {
                        st.cursor = p;
                        st.hover_since = now;
                        if matches!(st.hover, Hover::Cell(_)) {
                            shell.request_redraw();
                        }
                    }
                }
                if new_hover != st.hover {
                    st.set_hover(new_hover, now);
                    st.hover_since = now;
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::CursorLeft) => {
                if st.hover != Hover::None {
                    st.set_hover(Hover::None, now);
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(local) = cursor.position_in(bounds) else { return };
                let Some(abs) = cursor.position() else { return };
                shell.capture_event();
                st.hover_since = now;
                match hit(st, &metrics, data, bounds, local) {
                    Hover::Scrollbar => {
                        let (thumb_y, _, thumb_h) = scrollbar_geometry(bounds, metrics.total, st.scroll);
                        let y = local.y;
                        if y >= thumb_y && y <= thumb_y + thumb_h {
                            st.drag_bar = Some(y - thumb_y);
                        } else {
                            let page = if y < thumb_y { -bounds.height } else { bounds.height };
                            let base = st.target_scroll();
                            smooth_scroll_to(st, base + page, max_scroll, now);
                        }
                        shell.request_redraw();
                    }
                    Hover::BarDone => shell.publish((self.on)(GridEvent::SelectDone)),
                    Hover::Chip(s) => shell.publish((self.on)(GridEvent::Chip(data.sections[s].date))),
                    Hover::Header(s) => shell.publish((self.on)(GridEvent::ToggleCollapse(data.sections[s].date))),
                    Hover::Cell(id) => {
                        let click = amouse::Click::new(abs, mouse::Button::Left, st.last_click);
                        st.last_click = Some(click);
                        if click.kind() == amouse::click::Kind::Double && !data.select_mode {
                            shell.publish((self.on)(GridEvent::Activate(id)));
                        } else {
                            shell.publish((self.on)(GridEvent::Click { id, modifiers: current_modifiers() }));
                        }
                    }
                    Hover::None => {
                        st.last_click = None;
                        if !data.select_mode {
                            shell.publish((self.on)(GridEvent::ClearSelection));
                        }
                    }
                }
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if st.drag_bar.take().is_some() {
                    shell.request_redraw();
                }
            }

            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) if self.keyboard => {
                if modifiers.control() || modifiers.alt() {
                    return;
                }
                let base = st.target_scroll();
                let target = match key {
                    keyboard::Key::Named(Named::ArrowUp) => base - 90.0,
                    keyboard::Key::Named(Named::ArrowDown) => base + 90.0,
                    keyboard::Key::Named(Named::PageUp) => base - bounds.height,
                    keyboard::Key::Named(Named::PageDown) => base + bounds.height,
                    keyboard::Key::Named(Named::Home) => 0.0,
                    keyboard::Key::Named(Named::End) => max_scroll,
                    _ => return,
                };
                smooth_scroll_to(st, target, max_scroll, now);
                shell.capture_event();
                shell.request_redraw();
            }

            Event::Touch(te) => handle_touch(st, te, bounds, &metrics, data, max_scroll, now, shell, &self.on),

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
        if cursor.position_in(layout.bounds()).is_none() {
            return mouse::Interaction::None;
        }
        match st.hover {
            Hover::Header(_) | Hover::Chip(_) | Hover::BarDone => mouse::Interaction::Pointer,
            _ => mouse::Interaction::Idle,
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let _t = crate::profile::span("draw", "grid");
        crate::profile::milestone("first grid draw");
        let st = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let now = Instant::now();
        let hover_amt = |id: RecordId| st.hover_amount(id, now);
        let view = PaintState {
            scroll: st.scroll,
            size: st.size,
            hover: st.hover,
            hover_amt: &hover_amt,
            now,
            opacity: 1.0,
            bar: st.bar.value(now, paint::out_cubic),
            scrollbar_active: st.drag_bar.is_some(),
            openness: &|d| st.openness(d, self.data.collapsed, now),
            elide: Some(&st.elide),
            label_h: Some(st.label_h),
        };
        paint(renderer, bounds, &self.data, &view);
        // Tooltip
        if let Hover::Cell(id) = st.hover {
            if now >= st.hover_since + TOOLTIP_DELAY && st.drag_bar.is_none() {
                if let Some(r) = self.data.coll.get(id) {
                    let pair = self.data.coll.find_pair(id).and_then(|p| self.data.coll.get(p));
                    draw_tooltip(renderer, bounds, st.cursor, &tooltip_text(r, pair));
                }
            }
        }
    }
}

impl<'a, Message: 'a> From<Grid<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(g: Grid<'a, Message>) -> Self {
        Element::new(g)
    }
}

fn current_modifiers() -> keyboard::Modifiers {
    crate::ui::modifiers::get()
}

fn advance(st: &mut State, now: Instant) {
    if let Some(z) = st.zoom {
        st.size = z.value(now, paint::out_quint);
        if z.done(now) {
            st.size = z.to;
            st.zoom = None;
        }
    }
    if let Some(a) = st.label_anim {
        st.label_h = a.value(now, paint::out_cubic);
        if a.done(now) {
            st.label_h = a.to;
            st.label_anim = None;
        }
    }
    if let Some(a) = st.scroll_anim {
        st.scroll = a.value(now, paint::out_cubic);
        if a.done(now) {
            st.scroll = a.to;
            st.scroll_anim = None;
        }
    }
    if let Some(k) = st.kinetic {
        // Kinetic fling: `from` holds velocity (px/s), decaying.
        let dt = now.saturating_duration_since(st.last_frame).as_secs_f32().min(0.05);
        let v = k.from * (-(now.saturating_duration_since(k.start).as_secs_f32()) * 3.0).exp();
        st.scroll -= v * dt;
        if v.abs() < 20.0 {
            st.kinetic = None;
        }
    }
    st.collapse.retain(|_, a| !a.done(now));
    st.last_frame = now;
}

fn clamp_scroll(st: &mut State, max_scroll: f32) {
    if st.scroll > max_scroll {
        st.scroll = max_scroll;
        st.kinetic = None;
    }
    if st.scroll < 0.0 {
        st.scroll = 0.0;
        st.kinetic = None;
    }
    if let Some(a) = &mut st.scroll_anim {
        a.to = a.to.clamp(0.0, max_scroll);
    }
}

fn smooth_scroll_to(st: &mut State, target: f32, max_scroll: f32, now: Instant) {
    let end = target.clamp(0.0, max_scroll);
    let cur = st.scroll;
    st.kinetic = None;
    if (cur - end).abs() < 0.5 {
        st.scroll_anim = None;
        return;
    }
    st.scroll_anim = Some(Anim { from: cur, to: end, start: now, ms: 180.0 });
}

/// Where content-space `y` falls: (section, item index, fraction of a row),
/// or (section, usize::MAX, offset into the header).
fn locate(m: &Metrics, sections: &[Section], y: f32) -> Option<(usize, usize, f32)> {
    let s = m.section_at(y)?;
    let (top, o) = m.tops[s];
    let gy = y - top - HEADER_H;
    if gy < 0.0 || o < 0.999 || sections[s].ids.is_empty() {
        return Some((s, usize::MAX, y - top));
    }
    let row = (gy / m.cell.height).floor();
    let frac = gy / m.cell.height - row;
    let idx = (row as usize * m.cols).min(sections[s].ids.len() - 1);
    Some((s, idx, frac))
}

fn anchor_y(m: &Metrics, (s, idx, frac): (usize, usize, f32)) -> f32 {
    let (top, _) = m.tops[s];
    if idx == usize::MAX {
        return top + frac;
    }
    let row = (idx / m.cols) as f32;
    top + HEADER_H + (row + frac) * m.cell.height
}

/// First and last visible (section, index) as a cache key for scheduling.
fn visible_range(m: &Metrics, sections: &[Section], scroll: f32, h: f32) -> (usize, usize, usize, usize) {
    let first = m.section_at(scroll).unwrap_or(0);
    let last = m.section_at(scroll + h).unwrap_or(0);
    let row_of = |s: usize, y: f32| -> usize {
        if s >= sections.len() {
            return 0;
        }
        let gy = y - m.tops[s].0 - HEADER_H;
        if gy <= 0.0 { 0 } else { (gy / m.cell.height) as usize }
    };
    (first, row_of(first, scroll), last, row_of(last, scroll + h))
}

/// Records whose cells intersect the viewport (and one screen either side).
fn collect(m: &Metrics, data: GridData<'_>, scroll: f32, h: f32, with_nearby: bool) -> (Vec<RecordId>, Vec<RecordId>) {
    let band = |top: f32, bottom: f32| -> Vec<RecordId> {
        let mut out = Vec::new();
        for (s, sec) in data.sections.iter().enumerate() {
            let (stop, o) = m.tops[s];
            if o < 0.01 {
                continue;
            }
            let gtop = stop + HEADER_H;
            let gh = grid_height(sec.ids.len(), m.cols, m.cell.height) * o;
            if gtop + gh < top || gtop > bottom {
                continue;
            }
            let first_row = ((top - gtop) / m.cell.height).floor().max(0.0) as usize;
            let last_row = ((bottom - gtop) / m.cell.height).floor().max(0.0) as usize;
            let first = first_row * m.cols;
            let last = ((last_row + 1) * m.cols).min(sec.ids.len());
            if first < last {
                out.extend_from_slice(&sec.ids[first..last]);
            }
        }
        out
    };
    let visible = band(scroll, scroll + h);
    let nearby = if with_nearby {
        let on: HashSet<RecordId> = visible.iter().copied().collect();
        band(scroll - h, scroll + 2.0 * h).into_iter().filter(|id| !on.contains(id)).collect()
    } else {
        Vec::new()
    };
    (visible, nearby)
}

fn scrollbar_geometry(bounds: Rectangle, total: f32, scroll: f32) -> (f32, f32, f32) {
    let track_h = bounds.height;
    if total <= bounds.height {
        return (0.0, track_h, track_h);
    }
    let thumb_h = (bounds.height / total * track_h).max(24.0);
    let max_scroll = total - bounds.height;
    let y = (scroll / max_scroll).clamp(0.0, 1.0) * (track_h - thumb_h);
    (y, track_h, thumb_h)
}

fn bar_rect(bounds: Rectangle) -> Rectangle {
    let w = (bounds.width - 32.0).min(440.0);
    rect(bounds.x + (bounds.width - w) / 2.0, bounds.y + bounds.height - 48.0 - 16.0, w, 48.0)
}

fn done_rect(bar: Rectangle) -> Rectangle {
    let w = paint::text_width("Done", t::T_BODY, paint::SEMIBOLD) + 32.0;
    rect(bar.x + bar.width - 10.0 - w, bar.y + (bar.height - 30.0) / 2.0, w, 30.0)
}

// Section header layout: chevron, date, then the count / selection chip.
const HEADER_CHEVRON: f32 = 16.0;
const HEADER_LABEL_SIZE: f32 = t::T_TITLE;

fn header_label_x(header: Rectangle) -> f32 {
    header.x + LEFT + HEADER_CHEVRON + 6.0
}

fn chip_label(sec: &Section, sel: &HashSet<RecordId>) -> (String, usize) {
    let n_total = sec.ids.len();
    let n_sel = sec.ids.iter().filter(|id| sel.contains(id)).count();
    let label = if n_sel == 0 {
        format!("{} {}", util::thousands(n_total as u64), if n_total == 1 { "photo" } else { "photos" })
    } else if n_sel < n_total {
        format!("{n_sel} of {n_total} selected")
    } else if n_total == 1 {
        "Selected".to_string()
    } else {
        format!("All {n_total} selected")
    };
    (label, n_sel)
}

fn chip_rect(header: Rectangle, sec: &Section, sel: &HashSet<RecordId>) -> Rectangle {
    let label_w = paint::text_width(&sec.label, HEADER_LABEL_SIZE, paint::SEMIBOLD);
    let (text, _) = chip_label(sec, sel);
    let w = paint::text_width(&text, t::T_SMALL, paint::MEDIUM) + 20.0;
    let x = header_label_x(header) + label_w + 10.0;
    rect(x, header.center_y() - 11.0, w, 22.0)
}

fn hit(st: &State, m: &Metrics, data: GridData<'_>, bounds: Rectangle, local: Point) -> Hover {
    if st.bar.to > 0.5 {
        let bar = bar_rect(Rectangle::new(Point::ORIGIN, bounds.size()));
        if bar.contains(local) {
            return if done_rect(bar).contains(local) { Hover::BarDone } else { Hover::None };
        }
    }
    if local.x >= bounds.width - SCROLLBAR_W && m.total > bounds.height {
        return Hover::Scrollbar;
    }
    let p = Point::new(local.x, local.y + st.scroll);
    if let Some(s) = m.section_at(p.y) {
        let hr = m.header_rect(s);
        if hr.contains(p) {
            let chip = chip_rect(hr, &data.sections[s], data.selection);
            return if chip.contains(p) { Hover::Chip(s) } else { Hover::Header(s) };
        }
    }
    match m.hit_cell(data.sections, p) {
        Some((s, i)) => Hover::Cell(data.sections[s].ids[i]),
        None => Hover::None,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_touch<Message>(
    st: &mut State,
    te: &touch::Event,
    bounds: Rectangle,
    m: &Metrics,
    data: GridData<'_>,
    max_scroll: f32,
    now: Instant,
    shell: &mut Shell<'_, Message>,
    on: &dyn Fn(GridEvent) -> Message,
) {
    match te {
        touch::Event::FingerPressed { id, position } => {
            if !bounds.contains(*position) && st.touch.is_none() {
                return;
            }
            let t = st.touch.get_or_insert_with(|| Touch {
                fingers: HashMap::new(),
                last_y: position.y,
                last_t: now,
                velocity: 0.0,
                start: *position,
                moved: false,
                pinch_start_dist: 0.0,
                pinch_start_size: st.size,
            });
            t.fingers.insert(*id, *position);
            if t.fingers.len() == 2 {
                let pts: Vec<Point> = t.fingers.values().copied().collect();
                t.pinch_start_dist = pts[0].distance(pts[1]).max(1.0);
                t.pinch_start_size = st.size;
            }
            st.kinetic = None;
            st.scroll_anim = None;
            shell.capture_event();
        }
        touch::Event::FingerMoved { id, position } => {
            let Some(t) = st.touch.as_mut() else { return };
            t.fingers.insert(*id, *position);
            if t.fingers.len() >= 2 {
                let pts: Vec<Point> = t.fingers.values().copied().collect();
                let d = pts[0].distance(pts[1]);
                let size = (t.pinch_start_size * d / t.pinch_start_dist).clamp(MIN_SIZE, MAX_SIZE);
                t.moved = true;
                shell.publish(on(GridEvent::PinchTo(size.round())));
            } else {
                let dy = position.y - t.last_y;
                let dt = now.saturating_duration_since(t.last_t).as_secs_f32().max(0.001);
                if (position.y - t.start.y).abs() > 6.0 || (position.x - t.start.x).abs() > 6.0 {
                    t.moved = true;
                }
                t.velocity = 0.7 * t.velocity + 0.3 * (dy / dt);
                t.last_y = position.y;
                t.last_t = now;
                st.scroll = (st.scroll - dy).clamp(0.0, max_scroll);
                shell.request_redraw();
            }
            shell.capture_event();
        }
        touch::Event::FingerLifted { id, position } | touch::Event::FingerLost { id, position } => {
            let Some(t) = st.touch.as_mut() else { return };
            t.fingers.remove(id);
            if t.fingers.is_empty() {
                let t = st.touch.take().expect("touch");
                if !t.moved {
                    // A tap is a click.
                    let local = Point::new(position.x - bounds.x, position.y - bounds.y);
                    let p = Point::new(local.x, local.y + st.scroll);
                    if let Some((s, i)) = m.hit_cell(data.sections, p) {
                        let id = data.sections[s].ids[i];
                        let click = amouse::Click::new(*position, mouse::Button::Left, st.last_click);
                        st.last_click = Some(click);
                        if click.kind() == amouse::click::Kind::Double && !data.select_mode {
                            shell.publish(on(GridEvent::Activate(id)));
                        } else {
                            shell.publish(on(GridEvent::Click { id, modifiers: keyboard::Modifiers::empty() }));
                        }
                    }
                } else if t.velocity.abs() > 150.0 {
                    st.kinetic = Some(Anim { from: -t.velocity, to: 0.0, start: now, ms: 1500.0 });
                    shell.request_redraw();
                }
            }
            shell.capture_event();
        }
    }
}

// ── painting ────────────────────────────────────────────────────────────────

/// Everything that varies per frame for [`paint`].
pub struct PaintState<'s> {
    pub scroll: f32,
    pub size: f32,
    hover: Hover,
    hover_amt: &'s dyn Fn(RecordId) -> f32,
    now: Instant,
    pub opacity: f32,
    pub bar: f32,
    scrollbar_active: bool,
    pub openness: &'s dyn Fn(NaiveDate) -> f32,
    elide: Option<&'s RefCell<HashMap<(RecordId, u32), String>>>,
    /// The name strip's height mid-animation; `None` follows
    /// `GridData::show_names`.
    label_h: Option<f32>,
}

impl<'s> PaintState<'s> {
    /// A static frame (calendar transitions): no hover, no pill.
    pub fn snapshot(scroll: f32, size: f32, opacity: f32, openness: &'s dyn Fn(NaiveDate) -> f32) -> Self {
        Self {
            scroll,
            size,
            hover: Hover::None,
            hover_amt: &no_hover,
            now: Instant::now(),
            opacity,
            bar: 0.0,
            scrollbar_active: false,
            openness,
            elide: None,
            label_h: None,
        }
    }
}

fn no_hover(_: RecordId) -> f32 {
    0.0
}

pub fn paint(r: &mut Renderer, bounds: Rectangle, data: &GridData<'_>, ps: &PaintState<'_>) {
    let op = ps.opacity;
    let c = |col: Color| paint::with_alpha(col, op);
    paint::fill(r, bounds, c(t::PHOTO_GROUND));
    let label_h = ps.label_h.unwrap_or(if data.show_names { LABEL_H } else { 0.0 });
    let m = Metrics::new(data.sections, bounds.width, ps.size, label_h, ps.openness);
    let scroll = ps.scroll.clamp(0.0, (m.total - bounds.height).max(0.0));
    let top = scroll;
    let bottom = scroll + bounds.height;

    r.with_layer(bounds, |r| {
        for (s, sec) in data.sections.iter().enumerate() {
            let (stop, o) = m.tops[s];
            let gh = grid_height(sec.ids.len(), m.cols, m.cell.height) * o;
            if stop + HEADER_H + gh < top || stop > bottom {
                continue;
            }
            let origin_y = bounds.y - scroll;
            // Grid rows (clipped to the animated height).
            if o > 0.001 && !sec.ids.is_empty() {
                let clip_y = (bounds.y + stop + HEADER_H - scroll).max(bounds.y);
                let clip_bottom = (bounds.y + stop + HEADER_H + gh - scroll).min(bounds.y + bounds.height);
                let clip = rect(bounds.x, clip_y, bounds.width, clip_bottom - clip_y);
                if clip.height > 0.0 {
                    let first_row = ((top - stop - HEADER_H) / m.cell.height).floor().max(0.0) as usize;
                    let last_row = ((bottom - stop - HEADER_H) / m.cell.height).floor().max(0.0) as usize;
                    let first = first_row * m.cols;
                    let last = ((last_row + 1) * m.cols).min(sec.ids.len());
                    // Images draw above every quad in their layer, so the
                    // marks that sit on top of a thumbnail get a layer of
                    // their own.
                    let mut drawn = Vec::with_capacity(last.saturating_sub(first));
                    let runs = data.bursts.get(s).map(Vec::as_slice).unwrap_or(&[]);
                    r.with_layer(clip, |r| {
                        for i in first..last {
                            let id = sec.ids[i];
                            let Some(rec) = data.coll.get(id) else { continue };
                            let mut cell = m.cell_rect(s, i);
                            cell.x += bounds.x;
                            cell.y += origin_y;
                            let hover = (ps.hover_amt)(id);
                            let (thumb_rect, image_rect) = paint_cell(r, cell, id, rec, data, m.ts, m.label_h, hover, ps.now, op, ps.elide, clip);
                            let run = runs.get(i).copied().flatten();
                            if let Some(run) = run {
                                paint_bracket(r, cell, run, i % m.cols == 0, op);
                            }
                            drawn.push((id, rec, thumb_rect, image_rect, run));
                        }
                    });
                    r.with_layer(clip, |r| {
                        for &(id, rec, thumb_rect, image_rect, run) in &drawn {
                            paint_cell_marks(r, id, rec, data, m.ts, m.label_h, thumb_rect, image_rect, op, clip);
                            if let Some(run) = run.filter(|run| run.first) {
                                paint_burst_chip(r, run, image_rect, m.ts, data.select_mode, op, clip);
                            }
                        }
                    });
                    // Names hidden: each tag on its photo's corner, above
                    // the marks.
                    let corner = 1.0 - m.label_h / LABEL_H;
                    if corner > 0.01 {
                        r.with_layer(clip, |r| {
                            for &(id, rec, thumb_rect, image_rect, _) in &drawn {
                                let mount = paint::lerp_color(t::PHOTO_MOUNT, t::PHOTO_RAISED, (ps.hover_amt)(id));
                                paint_corner_tag(r, rec, thumb_rect, image_rect, mount, op * corner, clip);
                            }
                        });
                    }
                }
            }
            // Header
            let mut hr = m.header_rect(s);
            hr.x += bounds.x;
            hr.y += origin_y;
            paint_header(r, hr, sec, data, ps.hover, s, o < 0.5, op, bounds);
        }
    });

    r.with_layer(bounds, |r| paint_chrome(r, bounds, data, ps, &m, scroll));
}

/// Scrollbar and select-mode pill, above the cells.
fn paint_chrome(r: &mut Renderer, bounds: Rectangle, data: &GridData<'_>, ps: &PaintState<'_>, m: &Metrics, scroll: f32) {
    let c = |col: Color| paint::with_alpha(col, ps.opacity);
    let op = ps.opacity;
    // Scrollbar
    if m.total > bounds.height {
        let (y, _, h) = scrollbar_geometry(bounds, m.total, scroll);
        let (color, w) = if ps.scrollbar_active {
            (t::PHOTO_INK2, 8.0)
        } else if ps.hover == Hover::Scrollbar {
            (t::PHOTO_INK3, 8.0)
        } else {
            (t::PHOTO_INK4, 6.0)
        };
        let x = bounds.x + bounds.width - (SCROLLBAR_W + w) / 2.0;
        paint::rounded(r, rect(x, bounds.y + y + 2.0, w, (h - 4.0).max(8.0)), w / 2.0, c(color));
    }

    // Select-mode pill
    if ps.bar > 0.01 {
        paint_select_bar(r, bounds, data, ps.bar * op, ps.hover == Hover::BarDone);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_header(
    r: &mut Renderer,
    hr: Rectangle,
    sec: &Section,
    data: &GridData<'_>,
    hover: Hover,
    s: usize,
    collapsed: bool,
    op: f32,
    clip: Rectangle,
) {
    let c = |col: Color| paint::with_alpha(col, op);
    let header_hot = matches!(hover, Hover::Header(i) if i == s);
    let chevron = if collapsed { Glyph::ChevronRight } else { Glyph::ChevronDown };
    let cy = hr.center_y();
    r.draw_image(
        iced::advanced::image::Image::new(icons::handle(chevron, HEADER_CHEVRON, if header_hot { t::PHOTO_INK } else { t::PHOTO_INK3 }))
            .filter_method(FilterMethod::Linear)
            .opacity(op),
        rect(hr.x + LEFT, cy - HEADER_CHEVRON / 2.0, HEADER_CHEVRON, HEADER_CHEVRON),
        clip,
    );
    let x = header_label_x(hr);
    let label_w = paint::text_width(&sec.label, HEADER_LABEL_SIZE, paint::SEMIBOLD);
    paint::text(r, &sec.label, rect(x, hr.y, label_w + 2.0, hr.height), HEADER_LABEL_SIZE, paint::SEMIBOLD, c(t::PHOTO_INK), HAlign::Left, clip);
    let chip = chip_rect(hr, sec, data.selection);
    let (label, n_sel) = chip_label(sec, data.selection);
    let hovered = hover == Hover::Chip(s);
    let fg = if n_sel > 0 {
        t::SIGNAL
    } else if hovered {
        t::PHOTO_INK
    } else {
        t::PHOTO_INK3
    };
    if n_sel > 0 {
        paint::rounded(r, chip, 11.0, c(t::alpha(t::SIGNAL, if hovered { 0.24 } else { 0.14 })));
    } else if hovered {
        paint::rounded(r, chip, 11.0, c(t::PHOTO_RAISED));
    }
    paint::text(r, &label, chip, t::T_SMALL, paint::MEDIUM, c(fg), HAlign::Center, clip);
    let rule_x = chip.x + chip.width + 10.0;
    let rule_end = hr.x + hr.width - LEFT;
    if rule_x < rule_end {
        paint::hline(r, rule_x, rule_end, cy.round(), 1.0, c(t::PHOTO_LINE));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_cell(
    r: &mut Renderer,
    cell: Rectangle,
    id: RecordId,
    rec: &PhotoRecord,
    data: &GridData<'_>,
    ts: f32,
    label_h: f32,
    hover: f32,
    now: Instant,
    op: f32,
    elide: Option<&RefCell<HashMap<(RecordId, u32), String>>>,
    clip: Rectangle,
) -> (Rectangle, Rectangle) {
    let c = |col: Color| paint::with_alpha(col, op);
    let selected = data.selection.contains(&id);
    let mount = mount_rect(cell);
    paint::rounded(r, mount, t::R_CELL, c(paint::lerp_color(t::PHOTO_MOUNT, t::PHOTO_RAISED, hover)));

    let thumb_rect = rect(mount.x + PAD, mount.y + PAD, ts, ts);
    let thumb = data.thumbs.get(&rec.path);
    let image_rect = match thumb {
        Some(th) if th.ok => {
            let (tw, th_h) = (th.width() as f32, th.height() as f32);
            let s = (ts / tw).min(ts / th_h);
            let (w, h) = ((tw * s).round(), (th_h * s).round());
            let ir = rect(thumb_rect.x + ((ts - w) / 2.0).floor(), thumb_rect.y + ((ts - h) / 2.0).floor(), w, h);
            let level = th.level_for(w.max(h) * data.scale_factor);
            let fade = arrival(th.arrived, now);
            if fade < 1.0 {
                paint::rounded(r, thumb_rect, 2.0, c(t::alpha(t::PHOTO_GROUND, 1.0 - fade)));
            }
            r.draw_image(
                iced::advanced::image::Image::new(level.handle.clone()).filter_method(FilterMethod::Linear).opacity(op * fade),
                ir,
                clip,
            );
            ir
        }
        _ => {
            // Not decoded yet (or no decoder could read it): a quiet frame
            // with the file type.
            let failed = thumb.is_some();
            if failed {
                paint::rounded(r, thumb_rect, 2.0, c(t::PHOTO_GROUND));
            } else {
                // Still coming: a light sweep says it's on its way.
                paint::shimmer(r, thumb_rect, 2.0, c(t::PHOTO_GROUND), c(t::alpha(Color::WHITE, 0.045 * op)), shimmer_phase(now));
            }
            let label = if failed { format!("{} — no preview", rec.file_type.label()) } else { rec.file_type.label().to_string() };
            paint::text(r, &label, thumb_rect, t::T_SMALL, paint::MEDIUM, c(t::PHOTO_INK4), HAlign::Center, clip);
            thumb_rect
        }
    };

    // The strip fades as it shrinks (names being hidden).
    let shown = (label_h / LABEL_H).clamp(0.0, 1.0);
    if shown > 0.01 {
        let strip_clip = mount.intersection(&clip).unwrap_or_default();
        paint_cell_label(r, mount, id, rec, ts, thumb_rect, selected, op * shown, elide, strip_clip);
    }
    (thumb_rect, image_rect)
}

/// What sits on top of a thumbnail: crop framing, the prune cross-out, the
/// keep check, the select-mode checkbox and the selection outline (drawn in
/// a layer above the images).
#[allow(clippy::too_many_arguments)]
fn paint_cell_marks(
    r: &mut Renderer,
    id: RecordId,
    rec: &PhotoRecord,
    data: &GridData<'_>,
    ts: f32,
    label_h: f32,
    thumb_rect: Rectangle,
    image_rect: Rectangle,
    op: f32,
    _clip: Rectangle,
) {
    let c = |col: Color| paint::with_alpha(col, op);
    let selected = data.selection.contains(&id);
    let ir = image_rect;

    // Crop framing: the discarded margins are scrimmed.
    if let Some(crop) = rec.crop.as_ref().filter(|cr| !cr.is_full()) {
        if !rec.is_pruned && ir.width > 0.0 && ir.height > 0.0 {
            let (w, h) = (ir.width, ir.height);
            let cx = ir.x + (crop.left as f32 * w).floor();
            let cy = ir.y + (crop.top as f32 * h).floor();
            let cw = (crop.width() as f32 * w).floor().max(1.0);
            let ch = (crop.height() as f32 * h).floor().max(1.0);
            let scrim = c(t::alpha(Color::BLACK, 0.6));
            paint::fill(r, rect(ir.x, ir.y, w, cy - ir.y), scrim);
            paint::fill(r, rect(ir.x, cy + ch, w, ir.y + h - (cy + ch)), scrim);
            paint::fill(r, rect(ir.x, cy, cx - ir.x, ch), scrim);
            paint::fill(r, rect(cx + cw, cy, ir.x + w - (cx + cw), ch), scrim);
            paint::framed(r, rect(cx, cy, cw, ch), 0.0, Color::TRANSPARENT, c(t::alpha(Color::WHITE, 0.85)), 1.0);
        }
    }

    // Pruned: dimmed and crossed out, corner to corner.
    if rec.is_pruned {
        paint::fill(r, ir, c(t::alpha(t::PHOTO_GROUND, 0.62)));
        let inset = (ir.width.min(ir.height) * 0.1).max(4.0);
        let (x0, y0, x1, y1) = (ir.x + inset, ir.y + inset, ir.x + ir.width - inset, ir.y + ir.height - inset);
        let width = (ts / 36.0).clamp(2.5, 6.0);
        paint::strokes(
            r,
            &[(Point::new(x0, y0), Point::new(x1, y1)), (Point::new(x1, y0), Point::new(x0, y1))],
            width,
            c(t::alpha(t::REJECT, 0.92)),
            true,
        );
    }

    // Kept: a green check on the photo's corner.
    if rec.is_marked {
        let d = (ts / 7.0).floor().clamp(20.0, 30.0);
        let (bx, by) = (ir.x + ir.width - d - 6.0, ir.y + 6.0);
        let centre = Point::new(bx + d / 2.0, by + d / 2.0);
        paint::circle(r, centre, d / 2.0 + 1.5, c(t::alpha(Color::BLACK, 0.35)), Color::TRANSPARENT, 0.0);
        paint::circle(r, centre, d / 2.0, c(t::KEEP), Color::TRANSPARENT, 0.0);
        let u = d / 24.0;
        let p = |x: f32, y: f32| Point::new(bx + x * u, by + y * u);
        paint::strokes(r, &[(p(7.0, 12.5), p(10.5, 16.0)), (p(10.5, 16.0), p(17.0, 8.5))], (d / 10.0).max(2.0), c(t::ON_SIGNAL), true);
    }

    // Select-mode checkbox on the photo's other corner.
    if data.select_mode {
        let d = (ts / 8.0).floor().clamp(18.0, 26.0);
        let (bx, by) = (ir.x + 6.0, ir.y + 6.0);
        let centre = Point::new(bx + d / 2.0, by + d / 2.0);
        if selected {
            paint::circle(r, centre, d / 2.0, c(t::SIGNAL), Color::TRANSPARENT, 0.0);
            let u = d / 24.0;
            let p = |x: f32, y: f32| Point::new(bx + x * u, by + y * u);
            paint::strokes(r, &[(p(7.0, 12.5), p(10.5, 16.0)), (p(10.5, 16.0), p(17.0, 8.5))], (d / 10.0).max(1.8), c(t::ON_SIGNAL), true);
        } else {
            paint::circle(r, centre, d / 2.0, c(t::alpha(Color::BLACK, 0.4)), c(t::alpha(Color::WHITE, 0.85)), 1.5);
        }
    }

    // Selection: an accent outline around the whole mount.
    if selected {
        let mount = rect(thumb_rect.x - PAD, thumb_rect.y - PAD, ts + 2.0 * PAD, ts + 2.0 * PAD + label_h);
        paint::framed(r, mount, t::R_CELL, Color::TRANSPARENT, c(t::SIGNAL), 2.0);
    }
}

/// A burst's bracket: a rule through the gutter just under its run of
/// mounts, rising a little beside the first and the last.  Between tiles
/// it runs on across the gap; where the run wraps to the next row it runs
/// out into the gap, open, and picks up again under the next row.
fn paint_bracket(r: &mut Renderer, cell: Rectangle, run: RunMark, row_start: bool, op: f32) {
    let mount = mount_rect(cell);
    let color = paint::with_alpha(BRACKET, op);
    let bottom = mount.y + mount.height;
    let y = (bottom + BRACKET_DROP).round();
    let (left, right) = (mount.x - BRACKET_DROP, mount.x + mount.width + BRACKET_DROP - BRACKET_W);
    let x0 = if run.first {
        left
    } else if row_start {
        mount.x - GAP / 2.0
    } else {
        mount.x
    };
    let x1 = if run.last { right + BRACKET_W } else { cell.x + cell.width };
    paint::hline(r, x0, x1, y, BRACKET_W, color);
    if run.first {
        paint::vline(r, left, y - BRACKET_RISE, y + BRACKET_W, BRACKET_W, color);
    }
    if run.last {
        paint::vline(r, right, y - BRACKET_RISE, y + BRACKET_W, BRACKET_W, color);
    }
}

/// The count on a burst's first tile, top left on the photo (beside the
/// select-mode checkbox): two stacked frames and the number of frames, or
/// "5 of 13" when a filter hides some.
fn paint_burst_chip(r: &mut Renderer, run: RunMark, ir: Rectangle, ts: f32, select_mode: bool, op: f32, clip: Rectangle) {
    if ir.width < 40.0 || ir.height < 30.0 {
        return;
    }
    let c = |col: Color| paint::with_alpha(col, op);
    let label = if run.shown < run.total { format!("{} of {}", run.shown, run.total) } else { run.total.to_string() };
    let tw = paint::text_width(&label, t::T_SMALL, paint::MEDIUM).ceil();
    let (h, glyph_w) = (18.0, 11.0);
    let mut x = ir.x + 6.0;
    if select_mode {
        x += (ts / 8.0).floor().clamp(18.0, 26.0) + 4.0;
    }
    let chip = rect(x, ir.y + 6.0, 6.0 + glyph_w + 4.0 + tw + 7.0, h);
    paint::rounded(r, chip, h / 2.0, c(t::alpha(Color::BLACK, 0.6)));
    // Two frames, one behind the other.
    let (gx, gy) = (chip.x + 6.0, chip.y + 4.0);
    paint::framed(r, rect(gx + 3.0, gy, 8.0, 7.0), 1.0, Color::TRANSPARENT, c(t::PHOTO_INK2), 1.0);
    paint::framed(r, rect(gx, gy + 3.0, 8.0, 7.0), 1.0, c(t::alpha(Color::BLACK, 0.9)), c(t::PHOTO_INK), 1.0);
    let text_clip = chip.intersection(&clip).unwrap_or_default();
    paint::text(r, &label, rect(gx + glyph_w + 4.0, chip.y, tw + 2.0, h), t::T_SMALL, paint::MEDIUM, c(t::PHOTO_INK), HAlign::Left, text_clip);
}

#[allow(clippy::too_many_arguments)]
fn paint_cell_label(
    r: &mut Renderer,
    mount: Rectangle,
    id: RecordId,
    rec: &PhotoRecord,
    ts: f32,
    thumb_rect: Rectangle,
    selected: bool,
    op: f32,
    elide: Option<&RefCell<HashMap<(RecordId, u32), String>>>,
    clip: Rectangle,
) {
    let c = |col: Color| paint::with_alpha(col, op);
    // File name left, its tag right, in the strip under the photo.  The
    // name is the stem: the tag says the type, and the tooltip has the full
    // names.
    let strip = rect(thumb_rect.x, thumb_rect.y + ts + 2.0, ts, mount.y + mount.height - (thumb_rect.y + ts + 2.0));
    let kind = super::tags::Kind::of(rec);
    let (name_w, tag_x) = label_layout(ts, super::tags::kind_width(kind));
    super::tags::draw_kind(r, strip.x + tag_x, strip.center_y(), kind, op, clip);
    let color = if rec.is_pruned {
        t::PHOTO_INK3
    } else if selected {
        t::PHOTO_INK
    } else {
        t::PHOTO_INK2
    };
    let name_rect = rect(strip.x, strip.y, name_w, strip.height);
    let label_clip = name_rect.intersection(&clip).unwrap_or_default();
    let draw_name = |r: &mut Renderer, shown: &str| {
        paint::text(r, shown, name_rect, t::T_SMALL, paint::REGULAR, c(color), HAlign::Left, label_clip);
        if rec.is_pruned {
            let tw = paint::text_width(shown, t::T_SMALL, paint::REGULAR).min(name_w);
            paint::hline(r, strip.x, strip.x + tw, strip.center_y().round(), 1.0, c(t::PHOTO_INK3));
        }
    };
    match elide {
        Some(cache) => {
            let key = (id, name_w as u32);
            if !cache.borrow().contains_key(&key) {
                let shown = paint::elide_start(&rec.stem(), t::T_SMALL, paint::REGULAR, name_w);
                cache.borrow_mut().insert(key, shown);
            }
            if let Some(shown) = cache.borrow().get(&key) {
                draw_name(r, shown);
            }
        }
        None => draw_name(r, &paint::elide_start(&rec.stem(), t::T_SMALL, paint::REGULAR, name_w)),
    }
}

/// The tag on a photo's corner (names hidden), on its plate of mount where
/// the photo reaches the corner.
fn paint_corner_tag(r: &mut Renderer, rec: &PhotoRecord, thumb_rect: Rectangle, image_rect: Rectangle, mount: Color, op: f32, clip: Rectangle) {
    let kind = super::tags::Kind::of(rec);
    let (tag, plate) = corner_tag(thumb_rect, super::tags::kind_width(kind));
    if plate.intersects(&image_rect) {
        r.fill_quad(
            iced::advanced::renderer::Quad {
                bounds: plate,
                border: iced::Border { radius: iced::border::Radius::default().top_left(t::R_CONTROL), ..iced::Border::default() },
                ..Default::default()
            },
            paint::with_alpha(mount, op),
        );
    }
    super::tags::draw_kind(r, tag.x, tag.center_y(), kind, op, clip);
}

fn paint_select_bar(r: &mut Renderer, bounds: Rectangle, data: &GridData<'_>, alpha: f32, done_hover: bool) {
    let c = |col: Color| paint::with_alpha(col, alpha);
    let bar = bar_rect(bounds);
    paint::rounded(r, rect(bar.x, bar.y + 4.0, bar.width, bar.height), t::R_PANEL, c(t::alpha(Color::BLACK, 0.35)));
    paint::framed(r, bar, t::R_PANEL, c(t::FLOAT), c(t::LINE_HI), 1.0);
    let n = data.selection.len();
    let label = match n {
        0 => "Click photos to select them".to_string(),
        1 => "1 photo selected".to_string(),
        n => format!("{} photos selected", util::thousands(n as u64)),
    };
    let fg = if n > 0 { t::INK } else { t::INK2 };
    paint::text(r, &label, rect(bar.x + 16.0, bar.y, bar.width - 130.0, bar.height), t::T_BODY, paint::MEDIUM, c(fg), HAlign::Left, bounds);
    let done = done_rect(bar);
    let bg = if done_hover { t::SIGNAL_HI } else { t::SIGNAL };
    paint::rounded(r, done, t::R_CONTROL, c(bg));
    paint::text(r, "Done", done, t::T_BODY, paint::SEMIBOLD, c(t::ON_SIGNAL), HAlign::Center, bounds);
}

/// The hover text: the full file name (both, for a pair — the grid shows
/// only the stem), then size and date.
pub fn tooltip_text(r: &PhotoRecord, pair: Option<&PhotoRecord>) -> String {
    let date = |r: &PhotoRecord| r.modified.local.format("%Y-%m-%d %H:%M").to_string();
    match pair {
        Some(other) => {
            let (raw, jpg) = if r.file_type == FileType::Raw { (r, other) } else { (other, r) };
            let newest = if raw.modified.local >= jpg.modified.local { raw } else { jpg };
            [
                raw.filename(),
                jpg.filename(),
                format!("RAW {}, JPG {}", util::fmt_size_whole(raw.file_size), util::fmt_size_whole(jpg.file_size)),
                format!("Modified {}", date(newest)),
            ]
            .join("\n")
        }
        None => [r.filename(), format!("{} file, {}", r.file_type.label(), util::fmt_size_whole(r.file_size)), format!("Modified {}", date(r))].join("\n"),
    }
}

/// A tooltip bubble near `cursor`, kept inside `bounds`.
pub fn draw_tooltip(r: &mut Renderer, bounds: Rectangle, cursor: Point, content: &str) {
    let size = paint::measure(content, t::T_SMALL, paint::REGULAR);
    let (w, h) = (size.width + 16.0, size.height + 10.0);
    let mut x = cursor.x + 14.0;
    let mut y = cursor.y + 18.0;
    if x + w > bounds.x + bounds.width - 2.0 {
        x = (bounds.x + bounds.width - w - 2.0).max(bounds.x);
    }
    if y + h > bounds.y + bounds.height - 2.0 {
        y = (cursor.y - h - 6.0).max(bounds.y);
    }
    let bx = rect(x, y, w, h);
    r.with_layer(bounds, |r| {
        paint::rounded(r, rect(bx.x, bx.y + 2.0, bx.width, bx.height), t::R_CONTROL, t::alpha(Color::BLACK, 0.3));
        paint::framed(r, bx, t::R_CONTROL, t::RAISED_HI, t::LINE_HI, 1.0);
        paint::text_block(r, content, Point::new(x + 8.0, y + 5.0), t::T_SMALL, paint::REGULAR, t::INK, bx);
    });
}

/// The scroll offset that brings `id`'s mount wholly into view in a grid of
/// `size` scrolled to `scroll`, or `None` when it already is in view (or
/// isn't drawn: unknown, or its day is collapsed).  Quick look uses it to
/// keep the grid under it following the photo it shows.
pub fn reveal_scroll(
    sections: &[Section],
    collapsed: &HashSet<NaiveDate>,
    ts: f32,
    label_h: f32,
    size: Size,
    scroll: f32,
    id: RecordId,
) -> Option<f32> {
    let (s, i) = sections.iter().enumerate().find_map(|(s, sec)| sec.ids.iter().position(|&x| x == id).map(|i| (s, i)))?;
    if collapsed.contains(&sections[s].date) || size.height <= 0.0 {
        return None;
    }
    let m = Metrics::new(sections, size.width, ts, label_h, &|d| if collapsed.contains(&d) { 0.0 } else { 1.0 });
    let mount = mount_rect(m.cell_rect(s, i));
    let max_scroll = (m.total - size.height).max(0.0);
    let target = if mount.y < scroll {
        // Up: at the top, with its day's header when it is in the first row.
        if i < m.cols { m.tops[s].0 } else { mount.y - GAP }
    } else if mount.y + mount.height > scroll + size.height {
        mount.y + mount.height + GAP - size.height
    } else {
        return None;
    };
    Some(target.clamp(0.0, max_scroll))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;
    use std::path::PathBuf;

    fn rec(path: &str, size: u64, secs: i64) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        PhotoRecord::new(p, ft, size, MTime::from_unix(secs, 0), None)
    }

    #[test]
    fn label_strip_gives_the_name_all_but_the_tag() {
        // The tag sits flush right, the name takes the rest less a gap.
        let (name_w, tag_x) = label_layout(180.0, 63.0);
        assert_eq!(tag_x, 117.0);
        assert_eq!(name_w, 117.0 - NAME_GAP);
        // Never negative, however small the photo.
        assert_eq!(label_layout(40.0, 63.0).0, 0.0);
    }

    #[test]
    fn corner_tag_sits_inside_the_photo_corner_on_its_plate() {
        let thumb = rect(100.0, 50.0, 180.0, 180.0);
        let (tag, plate) = corner_tag(thumb, 63.0);
        assert_eq!(tag.x + tag.width, thumb.x + thumb.width - CORNER_INSET);
        assert_eq!(tag.y + tag.height, thumb.y + thumb.height - CORNER_INSET);
        // The plate holds the tag and runs out to the photo's corner.
        assert!(plate.x < tag.x && plate.y < tag.y);
        assert_eq!(plate.x + plate.width, thumb.x + thumb.width);
        assert_eq!(plate.y + plate.height, thumb.y + thumb.height);
        // A 3:2 landscape photo leaves the corner empty: no plate needed.
        let landscape = rect(thumb.x, thumb.y + 30.0, 180.0, 120.0);
        assert!(!plate.intersects(&landscape));
    }

    #[test]
    fn hiding_names_packs_rows_and_hit_testing_follows() {
        let mut c = Collection::new();
        let ids: Vec<RecordId> = (0..20).map(|i| c.add(rec(&format!("/s/IMG_{i}.JPG"), 1, 0))).collect();
        let sections = vec![Section { date: NaiveDate::from_ymd_opt(2026, 7, 24).unwrap(), label: String::new(), ids: ids.clone() }];
        let open = |_: NaiveDate| 1.0;
        let width = 1000.0;
        let on = Metrics::new(&sections, width, 180.0, LABEL_H, &open);
        let off = Metrics::new(&sections, width, 180.0, 0.0, &open);
        assert_eq!(on.cols, off.cols);
        assert_eq!(on.cell.height - off.cell.height, LABEL_H);
        assert_eq!(off.cell.width, off.cell.height, "with names hidden a cell is square");
        assert!(off.total < on.total);
        // The centre of every cell hits that cell, in both layouts.
        for m in [&on, &off] {
            for i in 0..ids.len() {
                let cr = m.cell_rect(0, i);
                assert_eq!(m.hit_cell(&sections, cr.center()), Some((0, i)));
            }
        }
    }

    #[test]
    fn tooltip_names_both_halves_of_a_pair() {
        let raw = rec("/s/20260724_0108_0001.ARW", 39 << 20, 200);
        let jpg = rec("/s/20260724_0108_0001.JPG", 8 << 20, 100);
        // RAW first, whichever half the tile shows.
        let (mb39, mb8) = (util::fmt_size_whole(39 << 20), util::fmt_size_whole(8 << 20));
        let expected = format!("20260724_0108_0001.ARW\n20260724_0108_0001.JPG\nRAW {mb39}, JPG {mb8}\nModified ");
        assert!(tooltip_text(&jpg, Some(&raw)).starts_with(&expected));
        assert!(tooltip_text(&raw, Some(&jpg)).starts_with(&expected));
        let single = tooltip_text(&jpg, None);
        assert!(single.starts_with(&format!("20260724_0108_0001.JPG\nJPG file, {mb8}\nModified ")), "{single}");
    }

    #[test]
    fn tag_kind_follows_pairing() {
        use super::super::tags::Kind;
        let mut raw = rec("/s/A.ARW", 1, 0);
        assert_eq!(Kind::of(&raw), Kind::Raw);
        assert_eq!(Kind::of(&rec("/s/A.JPG", 1, 0)), Kind::Jpg);
        raw.pair_stem = Some("a".into());
        assert_eq!(Kind::of(&raw), Kind::Pair);
        assert_eq!(Kind::from_halves(true, true), Some(Kind::Pair));
        assert_eq!(Kind::from_halves(false, true), Some(Kind::Jpg));
        assert_eq!(Kind::from_halves(false, false), None);
    }
}
