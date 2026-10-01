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
//!
//! The frames of a burst share one strip, a lighter mount joined across
//! the gutters, with a tab on its first frame giving the count.  A strip
//! never leaves the columns: where it wraps it ends square at the last
//! column and carries on square from the first, so every row starts at the
//! same edge.  A stacked burst is one tile, its cover, with the edges of
//! the frames under it showing below; its tab adds how many of them are
//! kept and pruned.  Clicking a tab stacks or opens the burst, and the
//! tiles slide to their new places.

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
use crate::model::bursts::{BurstId, RunMark};
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
/// Gutter between mounts in a row.
const GAP: f32 = 8.0;
/// Gutter between rows: wider than [`GAP`], so a row reads as a strip and
/// a burst's tab and a stack's edges have room above and below a mount.
const ROW_GAP: f32 = 14.0;
/// A burst's tab: how far it rises above the strip, its height (down to
/// the photo's top edge, never over it), and its inner padding.
const TAB_RISE: f32 = 7.0;
const TAB_H: f32 = TAB_RISE + PAD;
const TAB_PAD: f32 = 5.0;
/// The tab's chevron and mark glyphs.
const TAB_GLYPH: f32 = 9.0;
/// The edges of the frames under a stack, showing below its mount: each
/// edge's inset from the mount's sides and its height.
const STACK_EDGES: [(f32, f32); 2] = [(5.0, 3.0), (10.0, 3.0)];
/// Mid-slide, how far (in cells) a tile changing rows drifts as it fades
/// out of one row and into the next: enough to show which way the row
/// moved, too little to cross other tiles.
const DRIFT: f32 = 0.3;
pub const HEADER_H: f32 = 48.0;
const TOP_MARGIN: f32 = 4.0;
const BOTTOM_MARGIN: f32 = 16.0;
const LEFT: f32 = 16.0;
const SCROLLBAR_W: f32 = 10.0;
const PREFETCH_SETTLE: Duration = Duration::from_millis(80);
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);
pub const MIN_SIZE: f32 = 120.0;
pub const MAX_SIZE: f32 = 480.0;


/// One date group.
#[derive(Debug, Clone)]
pub struct Section {
    pub date: NaiveDate,
    pub label: String,
    /// The tiles: each photo, or a stacked burst's cover.
    pub ids: Vec<RecordId>,
    /// Photos in the day, frames inside stacks included (the header's
    /// count).
    pub photos: usize,
}

/// What the grid reports to the app.
#[derive(Debug, Clone)]
pub enum GridEvent {
    Click { id: RecordId, modifiers: keyboard::Modifiers },
    Activate(RecordId),
    ClearSelection,
    ToggleCollapse(NaiveDate),
    /// A burst's tab was clicked (on this tile): stack it, or open it.
    ToggleBurst(RecordId),
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
    /// Burst strips and stacks, per section and tile (empty: none).
    pub bursts: &'a [Vec<Option<RunMark>>],
    /// The grid before bursts were last stacked or opened, to animate from.
    pub restack: Option<Restack<'a>>,
}

/// What the grid looked like before bursts were stacked or opened.  A new
/// `seq` starts the animation: tiles slide from their old places to their
/// new ones, folded frames slide into their stack and fade, opened ones
/// slide out of it.
#[derive(Clone, Copy)]
pub struct Restack<'a> {
    pub seq: u64,
    pub sections: &'a [Section],
    pub bursts: &'a [Vec<Option<RunMark>>],
    /// The tile to hold still on screen, before and after (a frame folded
    /// into a stack is the stack).  Without it, the tile in the middle of
    /// the view holds still.
    pub focus: Option<(RecordId, RecordId)>,
}

/// The stride of one cell: its mount plus the gutters.  `label_h` is the
/// name strip's height: [`LABEL_H`], 0 with names hidden, or in between
/// while that animates.
pub fn cell_size(ts: f32, label_h: f32) -> Size {
    Size::new(ts + PAD * 2.0 + GAP, ts + PAD * 2.0 + label_h + ROW_GAP)
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
    rect(cell.x, cell.y, cell.width - GAP, cell.height - ROW_GAP)
}

/// A burst frame's piece of its strip: its mount, joined across the gutter
/// to the next frame in the same row, and the corner radius on each side
/// (left, right).  Only the burst's real ends are rounded; where it wraps,
/// the piece stops square at the column edge, so a strip never reaches
/// into the margins and rows stay aligned.  `col`: the cell's column of
/// `cols`.
fn strip_piece(cell: Rectangle, run: RunMark, col: usize, cols: usize) -> (Rectangle, f32, f32) {
    let mount = mount_rect(cell);
    let joins_next = !run.last && col + 1 < cols;
    let width = if joins_next { cell.width } else { mount.width };
    let left = if run.first { t::R_CELL } else { 0.0 };
    let right = if run.last { t::R_CELL } else { 0.0 };
    (rect(mount.x, mount.y, width, mount.height), left, right)
}

/// The count on a burst's tab: "11", or "5 of 11" when a filter hides
/// some of its frames.
fn tab_count(run: RunMark) -> String {
    if run.shown < run.total { format!("{} of {}", run.shown, run.total) } else { run.total.to_string() }
}

/// Width of a short label in the tab's type, cached: tabs repeat a handful
/// of counts, and measuring text shapes it.
fn badge_width(label: &str) -> f32 {
    thread_local! {
        static CACHE: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(&w) = c.borrow().get(label) {
            return w;
        }
        let w = paint::text_width(label, t::T_SMALL, paint::MEDIUM).ceil();
        let mut c = c.borrow_mut();
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(label.to_string(), w);
        w
    })
}

/// A count after a glyph in a stack's tab (kept, pruned).
fn tally(n: u16) -> Option<String> {
    (n > 0).then(|| n.to_string())
}

/// The tab on a burst's first tile: on the strip's top edge, above the
/// photo's left edge, rising into the gutter above.  Its pieces from the
/// left: a chevron (open or stacked), the count, then for a stack the
/// kept and pruned tallies.
fn tab_rect(mount: Rectangle, run: RunMark) -> Rectangle {
    let mut w = TAB_PAD + TAB_GLYPH + 3.0 + badge_width(&tab_count(run)) + TAB_PAD;
    if run.stacked {
        for n in [run.kept, run.pruned] {
            if let Some(s) = tally(n) {
                w += 7.0 + TAB_GLYPH + 2.0 + badge_width(&s);
            }
        }
    }
    // Never wider than the mount.
    rect(mount.x + PAD, mount.y - TAB_RISE, w.min(mount.width - PAD), TAB_H)
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
    /// The tab of the burst whose first tile this is.
    Tab(RecordId),
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
    /// The last [`Restack::seq`] seen, and the slide it started.
    restack_seq: u64,
    restack: Option<Slide>,
}

/// Tiles sliding to new places after bursts were stacked or opened.  All
/// rects are in the new layout's content coordinates (the old ones moved by
/// the scroll change that holds the focused tile still).
struct Slide {
    anim: Anim,
    /// Where each tile near the view was.
    from: HashMap<RecordId, Rectangle>,
    /// Where each stack was: frames opening out of it start there.
    stack_from: HashMap<BurstId, Rectangle>,
    /// Frames folded away: where they were, and their stack's new place.
    gone: Vec<(RecordId, Rectangle, Rectangle)>,
    /// Each day's old (top, grid height), so headers slide with their rows.
    days: HashMap<NaiveDate, (f32, f32)>,
}

impl Slide {
    /// Progress, eased (0 → 1).
    fn t(&self, now: Instant) -> f32 {
        self.anim.value(now, paint::out_cubic)
    }
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
            restack_seq: 0,
            restack: None,
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
        s.restack_seq = self.data.restack.map_or(0, |r| r.seq);
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
        if let Some(r) = self.data.restack.filter(|r| r.seq != st.restack_seq) {
            st.restack_seq = r.seq;
            start_slide(st, &self.data, r, now);
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
                    || st.restack.is_some()
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
                if let Hover::Cell(_) | Hover::Tab(_) = st.hover {
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
                        if matches!(st.hover, Hover::Cell(_) | Hover::Tab(_)) {
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
                    Hover::Tab(id) => {
                        st.last_click = None;
                        shell.publish((self.on)(GridEvent::ToggleBurst(id)));
                    }
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
            Hover::Header(_) | Hover::Chip(_) | Hover::Tab(_) | Hover::BarDone => mouse::Interaction::Pointer,
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
            slide: st.restack.as_ref(),
        };
        paint(renderer, bounds, &self.data, &view);
        // Tooltip
        if now >= st.hover_since + TOOLTIP_DELAY && st.drag_bar.is_none() {
            match st.hover {
                Hover::Cell(id) => {
                    if let Some(r) = self.data.coll.get(id) {
                        let pair = self.data.coll.find_pair(id).and_then(|p| self.data.coll.get(p));
                        draw_tooltip(renderer, bounds, st.cursor, &tooltip_text(r, pair));
                    }
                }
                Hover::Tab(id) => {
                    if let Some(run) = run_of(&self.data, id) {
                        draw_tooltip(renderer, bounds, st.cursor, &tab_tooltip(run));
                    }
                }
                _ => {}
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
    if st.restack.as_ref().is_some_and(|sl| sl.anim.done(now)) {
        st.restack = None;
    }
    st.last_frame = now;
}

/// Begin the slide after bursts were stacked or opened (`r` is the grid
/// before), moving the scroll so the focused tile — else the one in the
/// middle of the view — stays where it was on screen.  Costs one pass over
/// the tiles, once; each frame of the slide then looks tiles up by id.
fn start_slide(st: &mut State, data: &GridData<'_>, r: Restack<'_>, now: Instant) {
    st.restack = None;
    let vp = st.viewport;
    if vp.width <= 0.0 || vp.height <= 0.0 {
        return;
    }
    let (old, new) = {
        let st_ref: &State = st;
        let open = |d: NaiveDate| st_ref.openness(d, data.collapsed, now);
        (
            Metrics::new(r.sections, vp.width, st_ref.size, st_ref.label_h, &open),
            Metrics::new(data.sections, vp.width, st_ref.size, st_ref.label_h, &open),
        )
    };
    // Where every tile is now, and each stack by its burst.
    let mut home: HashMap<RecordId, (usize, usize)> = HashMap::with_capacity(data.sections.iter().map(|s| s.ids.len()).sum());
    let mut stack_home: HashMap<BurstId, (usize, usize)> = HashMap::new();
    for (s, sec) in data.sections.iter().enumerate() {
        let marks = data.bursts.get(s);
        for (i, &id) in sec.ids.iter().enumerate() {
            home.insert(id, (s, i));
            if let Some(m) = marks.and_then(|m| m.get(i).copied().flatten()).filter(|m| m.stacked) {
                stack_home.entry(m.burst).or_insert((s, i));
            }
        }
    }
    let old_mark = |s: usize, i: usize| r.bursts.get(s).and_then(|m| m.get(i).copied().flatten());
    // A tile's new place: itself, or the stack it was folded into.
    let new_home = |id: RecordId, mark: Option<RunMark>| home.get(&id).or_else(|| mark.and_then(|m| stack_home.get(&m.burst))).copied();

    // Hold the focused tile still if it was on screen, else the middle row.
    let (h, old_scroll) = (vp.height, st.scroll);
    let mut hold: Option<(f32, f32)> = None;
    if let Some((before, after)) = r.focus {
        let at = r.sections.iter().enumerate().find_map(|(s, sec)| sec.ids.iter().position(|&x| x == before).map(|i| (s, i)));
        if let (Some((s0, i0)), Some(&(s1, i1))) = (at, home.get(&after)) {
            let y0 = old.cell_rect(s0, i0).y;
            if old.tops[s0].1 > 0.999 && y0 + old.cell.height > old_scroll && y0 < old_scroll + h {
                hold = Some((y0, new.cell_rect(s1, i1).y));
            }
        }
    }
    if hold.is_none() {
        if let Some((s, idx, _)) = locate(&old, r.sections, old_scroll + h / 2.0) {
            hold = if idx == usize::MAX {
                let date = r.sections[s].date;
                data.sections.iter().position(|x| x.date == date).map(|s1| (old.tops[s].0, new.tops[s1].0))
            } else {
                new_home(r.sections[s].ids[idx], old_mark(s, idx)).map(|(s1, i1)| (old.cell_rect(s, idx).y, new.cell_rect(s1, i1).y))
            };
        }
    }
    let max_scroll = (new.total - h).max(0.0);
    let new_scroll = hold.map_or(old_scroll, |(y0, y1)| old_scroll + y1 - y0).clamp(0.0, max_scroll);
    let delta = new_scroll - old_scroll;

    // Where the tiles around the view were (a screen either side: tiles
    // slide in from off screen), moved by the scroll change.
    let mut slide = Slide {
        anim: Anim { from: 0.0, to: 1.0, start: now, ms: t::MOTION_MS },
        from: HashMap::new(),
        stack_from: HashMap::new(),
        gone: Vec::new(),
        days: HashMap::with_capacity(r.sections.len()),
    };
    let (lo, hi) = (old_scroll - h, old_scroll + 2.0 * h);
    for (s, sec) in r.sections.iter().enumerate() {
        let (top, o) = old.tops[s];
        let gh = grid_height(sec.ids.len(), old.cols, old.cell.height) * o;
        slide.days.insert(sec.date, (top + delta, gh));
        let gtop = top + HEADER_H;
        if o < 0.01 || gtop + gh < lo || gtop > hi {
            continue;
        }
        let first_row = ((lo - gtop) / old.cell.height).floor().max(0.0) as usize;
        let last_row = ((hi - gtop) / old.cell.height).floor().max(0.0) as usize;
        let first = (first_row * old.cols).min(sec.ids.len());
        let last = ((last_row + 1) * old.cols).min(sec.ids.len());
        for i in first..last {
            let id = sec.ids[i];
            let mut was = old.cell_rect(s, i);
            was.y += delta;
            let mark = old_mark(s, i);
            if let Some(m) = mark.filter(|m| m.stacked) {
                slide.stack_from.entry(m.burst).or_insert(was);
            }
            if home.contains_key(&id) {
                slide.from.insert(id, was);
            } else if let Some((s1, i1)) = new_home(id, mark) {
                slide.gone.push((id, was, new.cell_rect(s1, i1)));
            }
        }
    }
    st.scroll = new_scroll;
    st.scroll_anim = None;
    st.kinetic = None;
    st.sent_range = None;
    st.restack = Some(slide);
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

/// The header's chip: the day's photos (frames inside stacks too), or how
/// many of its tiles are selected (a stack is one).
fn chip_label(sec: &Section, sel: &HashSet<RecordId>) -> (String, usize) {
    let n_total = sec.ids.len();
    let n_sel = if sel.is_empty() { 0 } else { sec.ids.iter().filter(|id| sel.contains(id)).count() };
    let label = if n_sel == 0 {
        format!("{} {}", util::thousands(sec.photos as u64), if sec.photos == 1 { "photo" } else { "photos" })
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
    // Before the header: a first row's tabs rise into it.
    if let Some(id) = hit_tab(m, data, p) {
        return Hover::Tab(id);
    }
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

/// The burst tab at content-space point `p`, by the tile it sits on.  A tab
/// rises into the gutter above its tile, which belongs to the row above, so
/// the tile below the point is tried too.
fn hit_tab(m: &Metrics, data: GridData<'_>, p: Point) -> Option<RecordId> {
    for probe in [p, Point::new(p.x, p.y + TAB_RISE)] {
        let Some((s, i)) = m.hit_cell(data.sections, probe) else { continue };
        let run = data.bursts.get(s).and_then(|r| r.get(i).copied().flatten()).filter(|r| r.first);
        if let Some(run) = run {
            if tab_rect(mount_rect(m.cell_rect(s, i)), run).contains(p) {
                return Some(data.sections[s].ids[i]);
            }
        }
    }
    None
}

/// The burst mark of the tile showing `id`.
fn run_of(data: &GridData<'_>, id: RecordId) -> Option<RunMark> {
    data.sections.iter().zip(data.bursts).find_map(|(sec, runs)| sec.ids.iter().position(|&x| x == id).and_then(|i| runs.get(i).copied().flatten()))
}

/// The tab's hover text.
fn tab_tooltip(run: RunMark) -> String {
    let frames = if run.shown < run.total {
        format!("Showing {} of this burst's {} frames", run.shown, run.total)
    } else {
        format!("A burst of {} frames", run.total)
    };
    if run.stacked {
        let marks = match (run.kept, run.pruned) {
            (0, 0) => "none marked yet".to_string(),
            (k, 0) => format!("{k} kept"),
            (0, p) => format!("{p} pruned"),
            (k, p) => format!("{k} kept, {p} pruned"),
        };
        format!("{frames}, stacked: {marks}.\nMarks go to the photo on top.\nClick or press G to show every frame.")
    } else {
        format!("{frames}.\nClick or press G to stack it.")
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
    /// Tiles sliding after bursts were stacked or opened.
    slide: Option<&'s Slide>,
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
            slide: None,
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
    let origin_y = bounds.y - scroll;
    // Mid-slide, tiles come in from off screen: a screen either side of the
    // view is drawn.
    let slide = ps.slide.map(|sl| (sl.t(ps.now), sl));
    let reach = if slide.is_some() { bounds.height } else { 0.0 };
    let (top, bottom) = (scroll - reach, scroll + bounds.height + reach);

    r.with_layer(bounds, |r| {
        // Frames folding into a stack, fading out under everything: those on
        // the stack's row slide into it, the rest drift back where they are.
        if let Some((k, sl)) = slide {
            for &(id, from, to) in &sl.gone {
                let Some(rec) = data.coll.get(id) else { continue };
                let mut cell = if (from.y - to.y).abs() < 0.5 {
                    paint::lerp_rect(from, to, k)
                } else {
                    rect(from.x - m.cell.width * DRIFT * k, from.y, from.width, from.height)
                };
                cell.x += bounds.x;
                cell.y += origin_y;
                if cell.intersects(&bounds) {
                    paint_cell(r, cell, id, rec, data, m.ts, m.label_h, 0.0, true, false, ps.now, op * (1.0 - k), ps.elide, bounds);
                }
            }
        }
        for (s, sec) in data.sections.iter().enumerate() {
            let (new_top, o) = m.tops[s];
            let new_gh = grid_height(sec.ids.len(), m.cols, m.cell.height) * o;
            // Mid-slide, a day's header and rows move together.
            let (stop, gh) = match slide.and_then(|(k, sl)| sl.days.get(&sec.date).map(|&(t0, h0)| (k, t0, h0))) {
                Some((k, t0, h0)) => (paint::lerp(t0, new_top, k), paint::lerp(h0, new_gh, k)),
                None => (new_top, new_gh),
            };
            if stop + HEADER_H + gh < top || stop > bottom {
                continue;
            }
            // Grid rows (clipped to the animated height).
            if o > 0.001 && !sec.ids.is_empty() {
                let clip_y = (origin_y + stop + HEADER_H).max(bounds.y);
                let clip_bottom = (origin_y + stop + HEADER_H + gh).min(bounds.y + bounds.height);
                let clip = rect(bounds.x, clip_y, bounds.width, clip_bottom - clip_y);
                if clip.height > 0.0 {
                    // Mid-slide the rows sit on the day's sliding top.
                    let was = slide.and_then(|(_, sl)| sl.days.get(&sec.date)).map_or(new_top, |d| d.0);
                    let gtops = (stop + HEADER_H, was + HEADER_H);
                    let rows = Rows { bounds, origin_y, top, bottom, gtops, clip, slide };
                    paint_rows(r, data, ps, &m, s, &rows);
                }
            }
            // Header
            let mut hr = m.header_rect(s);
            hr.x += bounds.x;
            hr.y = origin_y + stop;
            paint_header(r, hr, sec, data, ps.hover, s, o < 0.5, op, bounds);
        }
    });

    r.with_layer(bounds, |r| paint_chrome(r, bounds, data, ps, &m, scroll));
}

/// Where one day's rows are painted: the widget's bounds, the content's
/// screen offset, the content-space band to draw, the rows' clip, and the
/// slide in progress (with its eased progress).
struct Rows<'s> {
    bounds: Rectangle,
    origin_y: f32,
    top: f32,
    bottom: f32,
    /// The top of the day's rows (content space): now, mid-slide, and
    /// where it was before the slide.
    gtops: (f32, f32),
    clip: Rectangle,
    slide: Option<(f32, &'s Slide)>,
}

/// Where a tile is drawn this frame, and how present it is: once, or twice
/// for a tile changing rows mid-slide.
type Places = ([(Rectangle, f32); 2], usize);

/// A tile painted this frame, for the layers drawn over it.
struct Drawn<'a> {
    id: RecordId,
    rec: &'a PhotoRecord,
    mount: Rectangle,
    thumb: Rectangle,
    image: Rectangle,
    run: Option<RunMark>,
    alpha: f32,
}

/// Day `s`'s tiles within the band: burst strips and stack edges, then the
/// mounts and photos, then (each a layer above the images) the marks, the
/// burst tabs and, with names hidden, the corner tags.
fn paint_rows(r: &mut Renderer, data: &GridData<'_>, ps: &PaintState<'_>, m: &Metrics, s: usize, rows: &Rows<'_>) {
    let sec = &data.sections[s];
    let op = ps.opacity;
    let clip = rows.clip;
    let gtop = m.tops[s].0 + HEADER_H;
    let first_row = ((rows.top - gtop) / m.cell.height).floor().max(0.0) as usize;
    let last_row = ((rows.bottom - gtop) / m.cell.height).floor().max(0.0) as usize;
    let first = (first_row * m.cols).min(sec.ids.len());
    let last = ((last_row + 1) * m.cols).min(sec.ids.len());
    if first >= last {
        return;
    }
    let runs = data.bursts.get(s).map(Vec::as_slice).unwrap_or(&[]);
    // A tile's place on screen and how present it is: its cell, or mid-slide
    // on its way there from where it was.  Only moves along a row slide;
    // a tile changing rows fades out of the old one and into the new one,
    // drifting the way the rows moved, rather than flying across the grid.
    // Frames opening out of a stack slide out of it along its row, and fade
    // in on the rows after, as does a tile from too far away.
    let (gtop, old_gtop) = rows.gtops;
    let (cw, ch) = (m.cell.width, m.cell.height);
    let place = |i: usize, id: RecordId, run: Option<RunMark>| -> Places {
        let at = |mut cr: Rectangle| {
            cr.x += rows.bounds.x;
            cr.y += rows.origin_y;
            cr
        };
        let (row, col) = (i / m.cols, i % m.cols);
        let cell = rect(LEFT + col as f32 * cw, gtop + row as f32 * ch, cw, ch);
        let none = (Rectangle::default(), 0.0);
        let Some((k, sl)) = rows.slide else { return ([(at(cell), 1.0), none], 1) };
        match sl.from.get(&id) {
            Some(&from) => {
                let old_row = ((from.y - old_gtop) / ch).round();
                if old_row == row as f32 {
                    ([(at(rect(paint::lerp(from.x, cell.x, k), cell.y, cw, ch)), 1.0), none], 1)
                } else {
                    let shift = if row as f32 > old_row { cw * DRIFT } else { -cw * DRIFT };
                    let leaving = rect(from.x + shift * k, gtop + old_row * ch, cw, ch);
                    let arriving = rect(cell.x - shift * (1.0 - k), cell.y, cw, ch);
                    ([(at(leaving), 1.0 - k), (at(arriving), k)], 2)
                }
            }
            None => {
                let stack = run.and_then(|m| sl.stack_from.get(&m.burst)).copied();
                match stack {
                    Some(f) if ((f.y - old_gtop) / ch).round() == row as f32 => ([(at(paint::lerp_rect(f, cell, k)), k), none], 1),
                    _ => ([(at(rect(cell.x + cw * DRIFT * (1.0 - k), cell.y, cw, ch)), k), none], 1),
                }
            }
        }
    };
    // Mid-slide each frame carries its own mount; the strips join up again
    // once the tiles have landed.
    let joined = rows.slide.is_none();
    let mut drawn: Vec<Drawn<'_>> = Vec::with_capacity(last - first);
    r.with_layer(clip, |r| {
        // Strips and stack edges first: the mounts sit on them.
        for i in first..last {
            let Some(run) = runs.get(i).copied().flatten() else { continue };
            let (places, n) = place(i, sec.ids[i], Some(run));
            for &(cell, alpha) in &places[..n] {
                if run.stacked {
                    // A new stack's edges grow in as its frames fold away.
                    let grow = match rows.slide {
                        Some((k, sl)) if !sl.stack_from.contains_key(&run.burst) => k,
                        _ => 1.0,
                    };
                    paint_stack_edges(r, mount_rect(cell), op * alpha * grow);
                }
                if joined {
                    let (band, left, right) = strip_piece(cell, run, i % m.cols, m.cols);
                    paint_corners(r, band, (left, right, right, left), paint::with_alpha(t::PHOTO_TRAY, op));
                }
            }
        }
        for i in first..last {
            let id = sec.ids[i];
            let Some(rec) = data.coll.get(id) else { continue };
            let run = runs.get(i).copied().flatten();
            let (places, n) = place(i, id, run);
            for &(cell, alpha) in &places[..n] {
                if !cell.intersects(&clip) || alpha <= 0.0 {
                    continue;
                }
                let hover = (ps.hover_amt)(id);
                let (thumb, image) =
                    paint_cell(r, cell, id, rec, data, m.ts, m.label_h, hover, run.is_some(), joined, ps.now, op * alpha, ps.elide, clip);
                drawn.push(Drawn { id, rec, mount: mount_rect(cell), thumb, image, run, alpha });
            }
        }
    });
    // Images draw above every quad in their layer, so the marks that sit
    // on top of a thumbnail get a layer of their own.
    r.with_layer(clip, |r| {
        for d in &drawn {
            paint_cell_marks(r, d.id, d.rec, data, m.ts, m.label_h, d.thumb, d.image, op * d.alpha, clip);
        }
    });
    // Burst tabs, over the selection outline.  They rise into the gutter
    // above their row, so past the top of the day's first row too.
    let tab_top = (clip.y - TAB_RISE).max(rows.bounds.y);
    let tab_clip = rect(clip.x, tab_top, clip.width, clip.y + clip.height - tab_top);
    r.with_layer(tab_clip, |r| {
        for d in &drawn {
            if let Some(run) = d.run.filter(|r| r.first) {
                paint_tab(r, d.mount, run, ps.hover == Hover::Tab(d.id), op * d.alpha, tab_clip);
            }
        }
    });
    // Names hidden: each tag on its photo's corner, above the marks.
    let corner = 1.0 - m.label_h / LABEL_H;
    if corner > 0.01 {
        r.with_layer(clip, |r| {
            for d in &drawn {
                let mount = mount_color(d.run.is_some(), (ps.hover_amt)(d.id));
                paint_corner_tag(r, d.rec, d.thumb, d.image, mount, op * corner * d.alpha, clip);
            }
        });
    }
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
    in_burst: bool,
    on_strip: bool,
    now: Instant,
    op: f32,
    elide: Option<&RefCell<HashMap<(RecordId, u32), String>>>,
    clip: Rectangle,
) -> (Rectangle, Rectangle) {
    let c = |col: Color| paint::with_alpha(col, op);
    let selected = data.selection.contains(&id);
    let mount = mount_rect(cell);
    // A burst frame `on_strip` sits on its strip (see `strip_piece`): its
    // own mount only shows when hovered.
    if !(in_burst && on_strip && hover <= 0.0) {
        paint::rounded(r, mount, t::R_CELL, c(mount_color(in_burst, hover)));
    }

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

/// A mount's colour: a single photo's, or a burst frame's (its strip's),
/// lit by `hover` (0–1).
fn mount_color(in_burst: bool, hover: f32) -> Color {
    if in_burst {
        paint::lerp_color(t::PHOTO_TRAY, t::PHOTO_TRAY_HI, hover)
    } else {
        paint::lerp_color(t::PHOTO_MOUNT, t::PHOTO_RAISED, hover)
    }
}

/// A filled rect with its own radius at each corner: (top left, top right,
/// bottom right, bottom left).
fn paint_corners(r: &mut Renderer, bounds: Rectangle, (tl, tr, br, bl): (f32, f32, f32, f32), color: Color) {
    if color.a <= 0.0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let radius = iced::border::Radius::default().top_left(tl).top_right(tr).bottom_right(br).bottom_left(bl);
    r.fill_quad(
        iced::advanced::renderer::Quad { bounds, border: iced::Border { radius, ..iced::Border::default() }, ..Default::default() },
        color,
    );
}

/// The edges of the frames folded under a stack, showing below its mount
/// like the cards of a squared-up deck: each narrower and darker than the
/// one above.  They sit in the row gutter, inside the columns.
fn paint_stack_edges(r: &mut Renderer, mount: Rectangle, op: f32) {
    let mut y = mount.y + mount.height;
    for (k, &(inset, h)) in STACK_EDGES.iter().enumerate() {
        let shade = paint::lerp_color(t::PHOTO_TRAY, t::PHOTO_GROUND, 0.12 + 0.3 * k as f32);
        paint_corners(r, rect(mount.x + inset, y, mount.width - 2.0 * inset, h), (0.0, 0.0, 1.5, 1.5), paint::with_alpha(shade, op));
        y += h;
    }
}

/// A burst's tab on its first tile (see [`tab_rect`]): part of the strip,
/// rounded on top like a folder's tab.  An open burst's chevron points
/// down, a stack's to the side, as the day headers' do.
fn paint_tab(r: &mut Renderer, mount: Rectangle, run: RunMark, hot: bool, op: f32, clip: Rectangle) {
    let c = |col: Color| paint::with_alpha(col, op);
    let tab = tab_rect(mount, run);
    let fill = if hot { t::PHOTO_TRAY_HI } else { t::PHOTO_TRAY };
    paint_corners(r, tab, (4.0, 4.0, 0.0, 0.0), c(fill));
    let inside = tab.intersection(&clip).unwrap_or_default();
    // The count is brighter than the file names, so where each burst
    // starts stands out in a grid full of them.
    let ink = t::PHOTO_INK;
    let cy = tab.center_y();
    let glyph = |r: &mut Renderer, g: Glyph, x: f32, color: Color| {
        r.draw_image(
            iced::advanced::image::Image::new(icons::handle(g, TAB_GLYPH, color)).filter_method(FilterMethod::Linear).opacity(op),
            rect(x, (cy - TAB_GLYPH / 2.0).round(), TAB_GLYPH, TAB_GLYPH),
            inside,
        );
    };
    let mut x = tab.x + TAB_PAD;
    glyph(r, if run.stacked { Glyph::ChevronRight } else { Glyph::ChevronDown }, x, if hot { t::PHOTO_INK } else { t::PHOTO_INK2 });
    x += TAB_GLYPH + 3.0;
    let count = tab_count(run);
    let w = badge_width(&count);
    paint::text(r, &count, rect(x, tab.y, w + 2.0, tab.height), t::T_SMALL, paint::MEDIUM, c(ink), HAlign::Left, inside);
    x += w;
    if run.stacked {
        for (n, g, color) in [(run.kept, Glyph::Check, t::KEEP), (run.pruned, Glyph::Close, t::REJECT)] {
            let Some(label) = tally(n) else { continue };
            x += 7.0;
            glyph(r, g, x, color);
            x += TAB_GLYPH + 2.0;
            let w = badge_width(&label);
            paint::text(r, &label, rect(x, tab.y, w + 2.0, tab.height), t::T_SMALL, paint::MEDIUM, c(ink), HAlign::Left, inside);
            x += w;
        }
    }
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
        if i < m.cols { m.tops[s].0 } else { mount.y - ROW_GAP }
    } else if mount.y + mount.height > scroll + size.height {
        mount.y + mount.height + ROW_GAP - size.height
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

    /// Tiles and burst marks for runs of these lengths in a row of the grid
    /// (1: a single photo; `true`: that run stacked into one tile).  Ids
    /// count every photo, so a stack skips the ids folded into it.
    fn layout(runs: &[(usize, bool)]) -> (Vec<RecordId>, Vec<Option<RunMark>>) {
        let (mut ids, mut marks, mut next) = (Vec::new(), Vec::new(), 0 as RecordId);
        for (b, &(len, stacked)) in runs.iter().enumerate() {
            let mark = |first, last| RunMark {
                burst: b as BurstId,
                first,
                last,
                shown: len as u16,
                total: len as u16,
                stacked,
                kept: 0,
                pruned: 0,
            };
            if len < 2 {
                ids.push(next);
                marks.push(None);
            } else if stacked {
                ids.push(next);
                marks.push(Some(mark(true, true)));
            } else {
                for k in 0..len {
                    ids.push(next + k as RecordId);
                    marks.push(Some(mark(k == 0, k == len - 1)));
                }
            }
            next += len as RecordId;
        }
        (ids, marks)
    }

    fn day(ids: &[RecordId], photos: usize) -> Vec<Section> {
        vec![Section { date: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(), label: String::new(), ids: ids.to_vec(), photos }]
    }

    #[test]
    fn strips_never_leave_the_columns() {
        // Singles, a burst wrapping rows, 2-frame bursts side by side, a
        // very long burst, a stack: at many widths, sizes, names on and off.
        let (ids, marks) = layout(&[(1, false), (11, false), (2, false), (2, false), (1, false), (30, false), (3, true), (2, false), (1, false)]);
        let sections = day(&ids, ids.len());
        let open = |_: NaiveDate| 1.0;
        for ts in [MIN_SIZE, 180.0, 300.0, MAX_SIZE] {
            for label_h in [LABEL_H, 0.0] {
                for width in [700.0, 1000.0, 1452.0, 2500.0] {
                    let m = Metrics::new(&sections, width, ts, label_h, &open);
                    let right_edge = LEFT + m.cols as f32 * m.cell.width - GAP;
                    for i in 0..ids.len() {
                        let (cell, col) = (m.cell_rect(0, i), i % m.cols);
                        let mount = mount_rect(cell);
                        if col == 0 {
                            assert_eq!(mount.x, LEFT, "every row's first mount starts at the same x");
                        }
                        let Some(run) = marks[i] else { continue };
                        let (band, left, right) = strip_piece(cell, run, col, m.cols);
                        let ctx = format!("tile {i} col {col}/{} ts {ts} width {width}", m.cols);
                        assert!(band.x >= LEFT && band.x + band.width <= right_edge + 0.01, "{ctx}: {band:?}");
                        assert_eq!((band.x, band.y, band.height), (mount.x, mount.y, mount.height), "{ctx}");
                        if !run.last && col + 1 < m.cols {
                            let next = mount_rect(m.cell_rect(0, i + 1));
                            assert_eq!(band.x + band.width, next.x, "{ctx}: joined across the gutter");
                        } else {
                            assert_eq!(band.width, mount.width, "{ctx}: stops at its own mount");
                        }
                        // Rounded only where the burst really starts and ends.
                        assert_eq!((left > 0.0, right > 0.0), (run.first, run.last), "{ctx}");
                    }
                }
            }
        }
    }

    #[test]
    fn stacks_are_one_tile_and_their_tabs_are_hit() {
        // A single, a 5-frame stack, an open 3-frame burst, a single.
        let (ids, marks) = layout(&[(1, false), (5, true), (3, false), (1, false), (4, true), (2, false)]);
        assert_eq!(ids, [0, 1, 6, 7, 8, 9, 10, 14, 15]);
        let sections = day(&ids, 16);
        let bursts = vec![marks.clone()];
        let (coll, thumbs, none) = (Collection::new(), ThumbCache::new(crate::imaging::thumbs::OWNER_MAIN, 1 << 20), HashSet::new());
        let collapsed = HashSet::new();
        let data = GridData {
            sections: &sections,
            coll: &coll,
            thumbs: &thumbs,
            selection: &none,
            collapsed: &collapsed,
            select_mode: false,
            scale_factor: 1.0,
            show_names: true,
            bursts: &bursts,
            restack: None,
        };
        // The header counts photos, frames in stacks too.
        assert_eq!(chip_label(&sections[0], &none).0, "16 photos");
        // Three columns, so tabs sit on the first, second and third rows.
        let m = Metrics::new(&sections, LEFT + 3.0 * cell_size(180.0, LABEL_H).width + SCROLLBAR_W + 1.0, 180.0, LABEL_H, &|_| 1.0);
        assert_eq!(m.cols, 3);
        for (i, mark) in marks.iter().enumerate() {
            let mount = mount_rect(m.cell_rect(0, i));
            assert_eq!(m.hit_cell(&sections, m.cell_rect(0, i).center()), Some((0, i)));
            match mark.filter(|r| r.first) {
                Some(run) => {
                    let tab = tab_rect(mount, run);
                    assert_eq!(hit_tab(&m, data, tab.center()), Some(ids[i]), "tile {i}");
                    // Its top is in the gutter above (the row above's cell).
                    assert_eq!(hit_tab(&m, data, Point::new(tab.x + 2.0, tab.y + 1.0)), Some(ids[i]), "tile {i}");
                    // The photo under it is not the tab.
                    assert_eq!(hit_tab(&m, data, Point::new(tab.center_x(), mount.y + PAD + 1.0)), None);
                }
                None => assert_eq!(hit_tab(&m, data, Point::new(mount.x + PAD + 4.0, mount.y + 2.0)), None, "tile {i}"),
            }
        }
        // A first-row tab rises into the day's header, and is still the tab.
        let first = tab_rect(mount_rect(m.cell_rect(0, 1)), marks[1].unwrap());
        assert!(m.header_rect(0).contains(Point::new(first.x + 2.0, first.y + 1.0)));
        assert_eq!(hit_tab(&m, data, Point::new(first.x + 2.0, first.y + 1.0)), Some(ids[1]));
        let st = State::new(180.0, true);
        let bounds = rect(0.0, 0.0, m.content_w + SCROLLBAR_W, 2000.0);
        assert_eq!(hit(&st, &m, data, bounds, Point::new(first.x + 2.0, first.y + 1.0)), Hover::Tab(ids[1]), "not the header");
        assert_eq!(hit(&st, &m, data, bounds, Point::new(first.x + 2.0, m.tops[0].0 + 4.0)), Hover::Header(0));
        // A stack's tab names the burst's frames and what's marked.
        let stack = marks[1].unwrap();
        assert!(tab_rect(mount_rect(m.cell_rect(0, 1)), RunMark { kept: 3, pruned: 1, ..stack }).width > tab_rect(mount_rect(m.cell_rect(0, 1)), stack).width);
        assert!(tab_tooltip(RunMark { kept: 3, pruned: 1, ..stack }).starts_with("A burst of 5 frames, stacked: 3 kept, 1 pruned."));
        assert!(tab_tooltip(RunMark { shown: 2, ..marks[2].unwrap() }).starts_with("Showing 2 of this burst's 3 frames."));
        assert_eq!(run_of(&data, 10), marks[6]);
    }

    #[test]
    fn the_tab_stays_off_the_photo_and_clear_of_a_stack_above() {
        let mount = rect(LEFT, 100.0, 192.0, 216.0);
        let (_, marks) = layout(&[(4, false)]);
        let tab = tab_rect(mount, marks[0].unwrap());
        assert_eq!(tab.y + tab.height, mount.y + PAD, "down to the photo's top edge, not over it");
        assert_eq!(tab.x, mount.x + PAD, "over the photo's left edge");
        assert!(tab.y < mount.y, "rising into the gutter");
        // The frames' edges under a stack in the row above, and this tab,
        // share the gutter without touching.
        let edges: f32 = STACK_EDGES.iter().map(|e| e.1).sum();
        assert!(TAB_RISE + edges < ROW_GAP);
        // Never wider than its mount, however small the photo.
        let tiny = rect(LEFT, 100.0, MIN_SIZE + 2.0 * PAD, MIN_SIZE + 2.0 * PAD);
        let busy = RunMark { shown: 999, total: 1000, stacked: true, kept: 999, pruned: 999, ..marks[0].unwrap() };
        assert!(tab_rect(tiny, busy).x + tab_rect(tiny, busy).width <= tiny.x + tiny.width);
    }

    #[test]
    fn stacking_slides_and_holds_the_focused_tile_still() {
        // Before: everything open.  After: the 11-frame burst stacked.  (Room
        // to scroll either way: singles after.)
        let runs = |stacked| [[(1, false), (11, stacked), (3, false), (1, false), (6, false), (2, false), (9, false)].as_slice(), &[(1, false); 20]].concat();
        let (open_ids, open_marks) = layout(&runs(false));
        let (ids, marks) = layout(&runs(true));
        let (before, after) = (day(&open_ids, open_ids.len()), day(&ids, open_ids.len()));
        let (before_runs, after_runs) = (vec![open_marks], vec![marks]);
        let (coll, thumbs, none, collapsed) = (Collection::new(), ThumbCache::new(crate::imaging::thumbs::OWNER_MAIN, 1 << 20), HashSet::new(), HashSet::new());
        let data = GridData {
            sections: &after,
            coll: &coll,
            thumbs: &thumbs,
            selection: &none,
            collapsed: &collapsed,
            select_mode: false,
            scale_factor: 1.0,
            show_names: true,
            bursts: &after_runs,
            restack: None,
        };
        let open = |_: NaiveDate| 1.0;
        let width = 1000.0;
        let m0 = Metrics::new(&before, width, 180.0, LABEL_H, &open);
        let m1 = Metrics::new(&after, width, 180.0, LABEL_H, &open);
        let y = |m: &Metrics, secs: &[Section], id: RecordId| m.cell_rect(0, secs[0].ids.iter().position(|&x| x == id).unwrap()).y;
        let now = Instant::now();
        // The focus, a photo after the burst (id 30, on screen): it moves up
        // in the grid but stays put on screen.
        let mut st = State::new(180.0, true);
        st.viewport = rect(0.0, 0.0, width, 700.0);
        st.scroll = y(&m0, &before, 30) - 300.0;
        let screen = y(&m0, &before, 30) - st.scroll;
        let restack = Restack { seq: 1, sections: &before, bursts: &before_runs, focus: Some((30, 30)) };
        start_slide(&mut st, &data, restack, now);
        assert!((y(&m1, &after, 30) - st.scroll - screen).abs() < 0.01, "held still");
        let slide = st.restack.as_ref().expect("sliding");
        // The folded frames within a screen of the view slide into the
        // stack (or fade where they are); those further away just go.
        let stack = m1.cell_rect(0, 1);
        let gone: Vec<RecordId> = slide.gone.iter().map(|g| g.0).collect();
        assert_eq!(gone, (8..=11).collect::<Vec<_>>(), "rows 2 and up of the old layout");
        assert!(slide.gone.iter().all(|g| g.2 == stack));
        // Tiles that stay are known by where they were, moved by the scroll.
        let delta = st.scroll - (y(&m0, &before, 30) - 300.0);
        assert_eq!(slide.from.get(&30).map(|r| r.y), Some(y(&m0, &before, 30) + delta));
        // A frame folded away while focused: the stack holds its place.
        let mut st = State::new(180.0, true);
        st.viewport = rect(0.0, 0.0, width, 700.0);
        st.scroll = y(&m0, &before, 9) - 200.0;
        let screen = y(&m0, &before, 9) - st.scroll;
        start_slide(&mut st, &data, Restack { focus: Some((9, 1)), ..restack }, now);
        assert!((y(&m1, &after, 1) - st.scroll - screen).abs() < 0.01 || st.scroll == 0.0);
        // The slide ends by itself.
        advance(&mut st, now + Duration::from_millis(t::MOTION_MS as u64 + 1));
        assert!(st.restack.is_none());
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
        let sections = day(&ids, ids.len());
        let open = |_: NaiveDate| 1.0;
        let width = 1000.0;
        let on = Metrics::new(&sections, width, 180.0, LABEL_H, &open);
        let off = Metrics::new(&sections, width, 180.0, 0.0, &open);
        assert_eq!(on.cols, off.cols);
        assert_eq!(on.cell.height - off.cell.height, LABEL_H);
        let mount = mount_rect(off.cell_rect(0, 0));
        assert_eq!(mount.width, mount.height, "with names hidden a mount is square");
        assert_eq!(off.cell.height - off.cell.width, ROW_GAP - GAP, "rows sit further apart than columns");
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
