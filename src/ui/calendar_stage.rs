//! The calendar's painted pages (year and month) and the transitions
//! between pages.
//!
//! A year is 365 squares and a month up to 42 mosaic tiles, so each page is
//! one painted widget.  Hover highlights (110 ms) and thumbnail fade-ins
//! (200 ms) live here; page transitions are driven by the app and passed in
//! as a [`Scene`], so a day page (a grid) can take part too.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate};
use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::Clipboard;
use iced::advanced::Shell;
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Transformation, Vector, touch, window};

use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::thumbs::ThumbCache;
use crate::model::library_index::{FileEntry, HEAT_LEVELS, LibraryIndex, MONTH_ABBR, MONTH_NAME};
use crate::util::{plural, thousands};

const FADE_MS: f32 = 200.0;
const HOVER_MS: f32 = 110.0;
const WHEEL_STEP: f32 = 120.0;
const WHEEL_COOLDOWN: Duration = Duration::from_millis(160);
const HEAT_ALPHA: [u8; HEAT_LEVELS + 1] = [0, 70, 125, 185, 245];

fn c_bg() -> Color {
    t::GROUND
}

/// First day of the week as 0 = Monday … 6 = Sunday, from the locale.
pub fn first_weekday() -> u32 {
    let lang = std::env::var("LC_TIME")
        .or_else(|_| std::env::var("LC_ALL"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let region = lang.split(['.', '@']).next().unwrap_or("").split('_').nth(1).unwrap_or("").to_uppercase();
    const SUNDAY_FIRST: &[&str] = &[
        "AG", "AS", "BD", "BR", "BS", "BT", "BW", "BZ", "CA", "CO", "DM", "DO", "ET", "GT", "GU", "HK", "HN",
        "ID", "IL", "IN", "JM", "JP", "KE", "KH", "KR", "LA", "MH", "MM", "MO", "MT", "MX", "MZ", "NI", "NP",
        "PA", "PE", "PH", "PK", "PR", "PT", "PY", "SA", "SG", "SV", "TH", "TT", "TW", "UM", "US", "VE", "VI",
        "WS", "YE", "ZA", "ZW",
    ];
    if SUNDAY_FIRST.contains(&region.as_str()) || region.is_empty() && lang.is_empty() { 6 } else { 0 }
}

const DAY_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAY_NARROW: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

/// Day-of-month labels, so a day cell's number costs no allocation per frame.
const DAY_LABELS: [&str; 32] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24",
    "25", "26", "27", "28", "29", "30", "31",
];

fn day_label(d: NaiveDate) -> &'static str {
    DAY_LABELS[d.day() as usize]
}

/// Python's `calendar.Calendar(first).monthdatescalendar(y, m)`: full weeks.
pub fn month_weeks(year: i32, month: u32, first: u32) -> Vec<[NaiveDate; 7]> {
    let Some(start) = NaiveDate::from_ymd_opt(year, month, 1) else { return Vec::new() };
    let wd = start.weekday().num_days_from_monday();
    let back = (wd + 7 - first) % 7;
    let mut d = start - chrono::Duration::days(back as i64);
    let (ny, nm) = crate::model::library_index::month_shift(year, month, 1);
    let end = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap_or(start);
    let mut weeks = Vec::new();
    while d < end {
        let mut w = [d; 7];
        for (i, slot) in w.iter_mut().enumerate() {
            *slot = d + chrono::Duration::days(i as i64);
        }
        weeks.push(w);
        d += chrono::Duration::days(7);
    }
    weeks
}

// ── layouts (also used by the app for zoom rects) ───────────────────────────

pub struct MonthLayout {
    pub cells: Vec<(NaiveDate, Rectangle)>,
}

pub const MONTH_HEAD: f32 = 26.0;
const MONTH_GAP: f32 = 6.0;
const MONTH_NUM_H: f32 = 24.0;

pub fn month_layout(size: Size, year: i32, month: u32) -> MonthLayout {
    let weeks = month_weeks(year, month, first_weekday());
    let mut cells = Vec::new();
    if weeks.is_empty() {
        return MonthLayout { cells };
    }
    let r = rect(14.0, 6.0, size.width - 28.0, size.height - 20.0);
    let rows = weeks.len() as f32;
    let g = MONTH_GAP;
    let cw = (r.width - g * 6.0) / 7.0;
    let ch = (r.height - MONTH_HEAD - g * (rows - 1.0)) / rows;
    for (row, week) in weeks.iter().enumerate() {
        for (col, d) in week.iter().enumerate() {
            let x = r.x + col as f32 * (cw + g);
            let y = r.y + MONTH_HEAD + row as f32 * (ch + g);
            cells.push((*d, rect(x, y, cw, ch)));
        }
    }
    MonthLayout { cells }
}

impl MonthLayout {
    pub fn cell_rect(&self, d: NaiveDate) -> Rectangle {
        self.cells.iter().find(|(cd, _)| *cd == d).map(|(_, r)| *r).unwrap_or(Rectangle::default())
    }
}

const TITLE_H: f32 = 26.0;
const WEEK_H: f32 = 16.0;

pub struct YearLayout {
    /// (month, box, title rect)
    pub months: Vec<(u32, Rectangle, Rectangle)>,
    pub squares: Vec<(NaiveDate, Rectangle)>,
    pub sq: f32,
}

pub fn year_layout(size: Size, year: i32) -> YearLayout {
    let (w, h) = (size.width, size.height - 30.0);
    let mut out = YearLayout { months: Vec::new(), squares: Vec::new(), sq: 8.0 };
    if w <= 0.0 || h <= 0.0 {
        return out;
    }
    let mut best: Option<(f32, usize, usize)> = None;
    for (cols, rows) in [(6usize, 2usize), (4, 3), (3, 4), (2, 6)] {
        let bw = (w - 28.0 - (cols as f32 - 1.0) * 22.0) / cols as f32;
        let bh = (h - 12.0 - (rows as f32 - 1.0) * 16.0) / rows as f32;
        let sq = (bw / 7.0).min((bh - TITLE_H - WEEK_H) / 6.0);
        if best.is_none_or(|b| sq > b.0) {
            best = Some((sq, cols, rows));
        }
    }
    let (sq, cols, rows) = best.expect("layouts");
    let sq = sq.max(4.0);
    let block_w = sq * 7.0;
    let block_h = TITLE_H + WEEK_H + sq * 6.0;
    let x0 = (w - (block_w * cols as f32 + (cols as f32 - 1.0) * 22.0)) / 2.0;
    let y0 = 10.0 + ((h - 12.0 - (block_h * rows as f32 + (rows as f32 - 1.0) * 16.0)) / 2.0).max(0.0);
    let first = first_weekday();
    for i in 0..12u32 {
        let month = i + 1;
        let (col, row) = ((i as usize) % cols, (i as usize) / cols);
        let x = x0 + col as f32 * (block_w + 22.0);
        let y = y0 + row as f32 * (block_h + 16.0);
        out.months.push((month, rect(x - 6.0, y - 4.0, block_w + 12.0, block_h + 8.0), rect(x, y, block_w, TITLE_H - 4.0)));
        for (r_i, week) in month_weeks(year, month, first).iter().enumerate() {
            for (c_i, d) in week.iter().enumerate() {
                if d.month() != month {
                    continue;
                }
                out.squares.push((*d, rect(x + c_i as f32 * sq, y + TITLE_H + WEEK_H + r_i as f32 * sq, sq, sq)));
            }
        }
    }
    out.sq = sq;
    out
}

impl YearLayout {
    pub fn month_rect(&self, month: u32) -> Rectangle {
        self.months.iter().find(|m| m.0 == month).map(|m| m.1).unwrap_or_default()
    }

    pub fn day_rect(&self, d: NaiveDate) -> Rectangle {
        self.squares.iter().find(|s| s.0 == d).map(|s| s.1).unwrap_or_default()
    }

    pub fn month_at(&self, p: Point) -> Option<u32> {
        self.months.iter().find(|m| m.1.contains(p)).map(|m| m.0)
    }
}

// ── scene description ───────────────────────────────────────────────────────

/// A page the stage can paint.
#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    Year(i32),
    Month(i32, u32),
    /// A day grid, painted by the caller-supplied closure.
    Day(NaiveDate),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Slide(i32),
    Zoom { rect: Rectangle, zoom_in: bool },
}

/// What to paint: a page, or a transition between two (possibly nested)
/// scenes at progress `t` (already eased).
#[derive(Debug, Clone)]
pub enum Scene {
    Page(Page),
    Transition { old: Box<Scene>, new: Box<Scene>, kind: Kind, t: f32 },
}

#[derive(Debug, Clone)]
pub enum StageEvent {
    Step(i32),
    Zoom(i32, Point),
    /// A click on a day with photos: open its folder.
    DayActivated(NaiveDate, Rectangle),
    /// Alt+click on one: show its photos, without opening the folder.
    DayBrowse(NaiveDate, Rectangle),
    MonthActivated(u32, Rectangle),
    MonthJump(i32, u32),
    JumpNearest,
    /// Thumbnails the painted page is missing.
    Need(Vec<FileEntry>),
    /// The day under the pointer on a month page changed.
    Hover(Option<NaiveDate>),
    Resized(Size),
}

/// A click held with Alt: on a day, show its photos rather than open it.
fn with_alt(ev: StageEvent, alt: bool) -> StageEvent {
    match ev {
        StageEvent::DayActivated(d, r) if alt => StageEvent::DayBrowse(d, r),
        ev => ev,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HoverKey {
    Date(NaiveDate),
    Month(u32),
    Pill,
}

pub struct CalendarStage<'a, Message> {
    scene: Scene,
    index: Option<&'a Arc<LibraryIndex>>,
    thumbs: &'a ThumbCache,
    day_painter: Option<Box<dyn Fn(&mut Renderer, Rectangle, NaiveDate, f32) + 'a>>,
    scale_factor: f32,
    on: Box<dyn Fn(StageEvent) -> Message + 'a>,
}

impl<'a, Message> CalendarStage<'a, Message> {
    pub fn new(
        scene: Scene,
        index: Option<&'a Arc<LibraryIndex>>,
        thumbs: &'a ThumbCache,
        scale_factor: f32,
        on: impl Fn(StageEvent) -> Message + 'a,
    ) -> Self {
        Self { scene, index, thumbs, day_painter: None, scale_factor, on: Box::new(on) }
    }

    /// How to paint a day page inside a transition: (renderer, bounds at the
    /// origin, date, opacity).
    pub fn day_painter(mut self, f: impl Fn(&mut Renderer, Rectangle, NaiveDate, f32) + 'a) -> Self {
        self.day_painter = Some(Box::new(f));
        self
    }
}

#[derive(Clone, Copy)]
struct HoverVal {
    v: f32,
    target: f32,
}

struct State {
    hover: HashMap<HoverKey, HoverVal>,
    hover_key: Option<HoverKey>,
    last_tick: Instant,
    pressed: Option<HoverKey>,
    wheel_acc: f32,
    wheel_last: Instant,
    size: Size,
    reported: Size,
    page_key: Option<Page>,
    fingers: HashMap<touch::Finger, Point>,
    pinch: Option<(f32, bool)>,
    resizing_until: Option<Instant>,
    /// Thumbnails already requested for this page (don't ask every frame).
    asked: std::collections::HashSet<std::path::PathBuf>,
}

fn is_live_page(scene: &Scene) -> Option<&Page> {
    match scene {
        Scene::Page(p) => Some(p),
        _ => None,
    }
}

impl<'a, Message> CalendarStage<'a, Message> {
    fn hit(&self, size: Size, p: Point) -> Option<HoverKey> {
        let page = is_live_page(&self.scene)?;
        let index = self.index?;
        match page {
            Page::Month(y, m) => {
                if let Some(pill) = month_pill(index, *y, *m, size) {
                    if pill.contains(p) {
                        return Some(HoverKey::Pill);
                    }
                }
                month_layout(size, *y, *m).cells.iter().find(|(_, r)| r.contains(p)).map(|(d, _)| HoverKey::Date(*d))
            }
            Page::Year(y) => {
                if let Some(pill) = year_pill(index, *y, size) {
                    if pill.contains(p) {
                        return Some(HoverKey::Pill);
                    }
                }
                let lay = year_layout(size, *y);
                if let Some((d, _)) = lay.squares.iter().find(|(_, r)| r.contains(p)) {
                    return Some(HoverKey::Date(*d));
                }
                lay.months.iter().find(|(_, _, t)| t.contains(p)).map(|(m, _, _)| HoverKey::Month(*m))
            }
            Page::Day(_) => None,
        }
    }

    fn clickable(&self, key: HoverKey) -> bool {
        let Some(Page::Month(_, m)) = is_live_page(&self.scene) else { return true };
        match key {
            HoverKey::Date(d) => self.index.is_some_and(|ix| ix.get(d).is_some()) || d.month() != *m,
            _ => true,
        }
    }
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for CalendarStage<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            hover: HashMap::new(),
            hover_key: None,
            last_tick: Instant::now(),
            pressed: None,
            wheel_acc: 0.0,
            wheel_last: Instant::now() - Duration::from_secs(1),
            size: Size::ZERO,
            reported: Size::ZERO,
            page_key: None,
            fingers: HashMap::new(),
            pinch: None,
            resizing_until: None,
            asked: std::collections::HashSet::new(),
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let key = is_live_page(&self.scene).cloned();
        if key != st.page_key {
            // A new page: forget hover and fades (like set_month / set_year).
            st.page_key = key;
            st.hover.clear();
            st.hover_key = None;
            st.asked.clear();
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
        let size = b.size();
        let now = Instant::now();
        let st = tree.state.downcast_mut::<State>();
        if st.size != size {
            if st.size != Size::ZERO {
                st.resizing_until = Some(now + Duration::from_millis(140));
            }
            st.size = size;
        }
        let local = |p: Point| Point::new(p.x - b.x, p.y - b.y);

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("calendar", *at);
                // Hover animation step.
                let dt = now.saturating_duration_since(st.last_tick).as_secs_f32() * 1000.0 / HOVER_MS;
                st.last_tick = now;
                let mut moving = false;
                st.hover.retain(|_, h| {
                    if (h.v - h.target).abs() > 1e-4 {
                        h.v = if h.target > h.v { (h.v + dt).min(h.target) } else { (h.v - dt).max(h.target) };
                        moving = true;
                    }
                    !(h.v == 0.0 && h.target == 0.0)
                });
                if moving {
                    shell.request_redraw();
                }
                if st.reported != size {
                    st.reported = size;
                    shell.publish((self.on)(StageEvent::Resized(size)));
                }
                if let Some(until) = st.resizing_until {
                    if now >= until {
                        st.resizing_until = None;
                    } else {
                        shell.request_redraw_at(window::RedrawRequest::At(until));
                    }
                }
                // Missing thumbnails and running fades.
                let (mut need, fading) = self.scan_thumbs(size, now, st.hover_key);
                need.retain(|fe| st.asked.insert(fe.path.clone()));
                if !need.is_empty() {
                    shell.publish((self.on)(StageEvent::Need(need)));
                }
                if fading {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let key = cursor.position_in(b).and_then(|_| cursor.position()).and_then(|p| self.hit(size, local(p)));
                if key != st.hover_key {
                    let day = |k: Option<HoverKey>| match k {
                        Some(HoverKey::Date(d)) => Some(d),
                        _ => None,
                    };
                    if day(key) != day(st.hover_key) {
                        shell.publish((self.on)(StageEvent::Hover(day(key))));
                    }
                    if let Some(old) = st.hover_key {
                        st.hover.entry(old).or_insert(HoverVal { v: 0.0, target: 0.0 }).target = 0.0;
                    }
                    if let Some(new) = key {
                        st.hover.entry(new).or_insert(HoverVal { v: 0.0, target: 0.0 }).target = 1.0;
                    }
                    st.hover_key = key;
                    st.last_tick = now;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if let Some(old) = st.hover_key.take() {
                    st.hover.entry(old).or_insert(HoverVal { v: 0.0, target: 0.0 }).target = 0.0;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_in(b).and(cursor.position()) else { return };
                st.pressed = self.hit(size, local(p));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let pressed = st.pressed.take();
                let Some(p) = cursor.position_in(b).and(cursor.position()) else { return };
                let key = self.hit(size, local(p));
                if key.is_none() || key != pressed {
                    return;
                }
                let (Some(page), Some(index)) = (is_live_page(&self.scene), self.index) else { return };
                let ev = match (page, key.expect("some")) {
                    (_, HoverKey::Pill) => Some(StageEvent::JumpNearest),
                    (Page::Month(_, m), HoverKey::Date(d)) => {
                        if d.month() != *m {
                            Some(StageEvent::MonthJump(d.year(), d.month()))
                        } else if index.get(d).is_some() {
                            Some(StageEvent::DayActivated(d, month_layout(size, d.year(), d.month()).cell_rect(d)))
                        } else {
                            None
                        }
                    }
                    (Page::Year(y), HoverKey::Date(d)) => {
                        let lay = year_layout(size, *y);
                        if index.get(d).is_some() {
                            Some(StageEvent::DayActivated(d, lay.day_rect(d)))
                        } else {
                            Some(StageEvent::MonthActivated(d.month(), lay.month_rect(d.month())))
                        }
                    }
                    (Page::Year(y), HoverKey::Month(m)) => Some(StageEvent::MonthActivated(m, year_layout(size, *y).month_rect(m))),
                    _ => None,
                };
                if let Some(ev) = ev.map(|ev| with_alt(ev, crate::ui::modifiers::get().alt())) {
                    shell.publish((self.on)(ev));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(p) = cursor.position_in(b) else { return };
                shell.capture_event();
                let (dx, dy) = match delta {
                    ScrollDelta::Lines { x, y } => (x * WHEEL_STEP, y * WHEEL_STEP),
                    ScrollDelta::Pixels { x, y } => (*x, *y),
                };
                if crate::ui::modifiers::get().control() {
                    if dy != 0.0 {
                        shell.publish((self.on)(StageEvent::Zoom(if dy > 0.0 { 1 } else { -1 }, p)));
                    }
                    return;
                }
                let d = if dy.abs() >= dx.abs() { dy } else { dx };
                if d == 0.0 {
                    return;
                }
                if (st.wheel_acc > 0.0) != (d > 0.0) {
                    st.wheel_acc = 0.0;
                }
                st.wheel_acc += d;
                if st.wheel_acc.abs() >= WHEEL_STEP && now.saturating_duration_since(st.wheel_last) >= WHEEL_COOLDOWN {
                    let step = if st.wheel_acc > 0.0 { -1 } else { 1 };
                    st.wheel_acc = 0.0;
                    st.wheel_last = now;
                    shell.publish((self.on)(StageEvent::Step(step)));
                }
            }
            Event::Touch(te) => match te {
                touch::Event::FingerPressed { id, position } if b.contains(*position) => {
                    st.fingers.insert(*id, *position);
                    if st.fingers.len() == 2 {
                        let pts: Vec<Point> = st.fingers.values().copied().collect();
                        st.pinch = Some((pts[0].distance(pts[1]).max(1.0), false));
                    }
                    shell.capture_event();
                }
                touch::Event::FingerMoved { id, position } => {
                    if st.fingers.contains_key(id) {
                        st.fingers.insert(*id, *position);
                        if let Some((d0, fired)) = st.pinch {
                            let pts: Vec<Point> = st.fingers.values().copied().collect();
                            if pts.len() >= 2 && !fired {
                                let s = pts[0].distance(pts[1]) / d0;
                                let c = local(Point::new((pts[0].x + pts[1].x) / 2.0, (pts[0].y + pts[1].y) / 2.0));
                                if s > 1.25 {
                                    st.pinch = Some((d0, true));
                                    shell.publish((self.on)(StageEvent::Zoom(1, c)));
                                } else if s < 0.8 {
                                    st.pinch = Some((d0, true));
                                    shell.publish((self.on)(StageEvent::Zoom(-1, c)));
                                }
                            }
                        }
                        shell.capture_event();
                    }
                }
                touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. } => {
                    st.fingers.remove(id);
                    if st.fingers.len() < 2 {
                        st.pinch = None;
                    }
                }
                _ => {}
            },
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
        match st.hover_key {
            Some(k) if self.clickable(k) => mouse::Interaction::Pointer,
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
        let _t = crate::profile::span("draw", "calendar");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let ctx = Ctx {
            index: self.index,
            thumbs: self.thumbs,
            hover: &st.hover,
            hover_key: st.hover_key,
            now,
            sf: self.scale_factor,
            day_painter: self.day_painter.as_deref(),
        };
        r.with_layer(b, |r| {
            paint::fill(r, b, c_bg());
            r.with_translation(Vector::new(b.x, b.y), |r| {
                let size = b.size();
                draw_scene(r, size, rect(0.0, 0.0, size.width, size.height), &self.scene, 1.0, &ctx, true);
            });
        });
    }
}

impl<'a, Message> CalendarStage<'a, Message> {
    /// Thumbnails the live page needs but doesn't have, and whether any
    /// drawn thumbnail is still fading in.
    fn scan_thumbs(&self, size: Size, now: Instant, hover: Option<HoverKey>) -> (Vec<FileEntry>, bool) {
        let mut need = Vec::new();
        let mut fading = false;
        let Some(index) = self.index else { return (need, false) };
        let mut visit = |fe: &FileEntry| match self.thumbs.get(&fe.path) {
            Some(t) => {
                if !paint::done(t.arrived, now, FADE_MS) {
                    fading = true;
                }
            }
            None => need.push(fe.clone()),
        };
        let mut pages = Vec::new();
        collect_pages(&self.scene, &mut pages);
        for page in pages {
            match page {
                Page::Month(y, m) => {
                    for (d, cell) in month_layout(size, y, m).cells {
                        if d.month() != m {
                            continue;
                        }
                        if let Some(day) = index.get(d) {
                            let area = rect(cell.x + 5.0, cell.y + MONTH_NUM_H + 4.0, cell.width - 10.0, cell.height - MONTH_NUM_H - 9.0);
                            if area.height >= 22.0 && area.width >= 22.0 {
                                let n = if day.shot_count >= 4 && area.width >= 56.0 && area.height >= 44.0 { 4 } else { 1 };
                                for fe in day.preview_entries(n) {
                                    visit(fe);
                                }
                            }
                        }
                    }
                }
                Page::Year(_) => {
                    if let Some(HoverKey::Date(d)) = hover {
                        if let Some(day) = index.get(d) {
                            if let Some(fe) = day.preview_entries(1).first() {
                                visit(fe);
                            }
                        }
                    }
                }
                Page::Day(_) => {}
            }
        }
        (need, fading)
    }
}

fn collect_pages(scene: &Scene, out: &mut Vec<Page>) {
    match scene {
        Scene::Page(p) => out.push(p.clone()),
        Scene::Transition { old, new, .. } => {
            collect_pages(old, out);
            collect_pages(new, out);
        }
    }
}

struct Ctx<'c> {
    index: Option<&'c Arc<LibraryIndex>>,
    thumbs: &'c ThumbCache,
    hover: &'c HashMap<HoverKey, HoverVal>,
    hover_key: Option<HoverKey>,
    now: Instant,
    sf: f32,
    day_painter: Option<&'c dyn Fn(&mut Renderer, Rectangle, NaiveDate, f32)>,
}

impl Ctx<'_> {
    fn h(&self, key: HoverKey, live: bool) -> f32 {
        if !live {
            return 0.0;
        }
        self.hover.get(&key).map(|h| h.v).unwrap_or(0.0)
    }

    fn fade(&self, arrived: Instant) -> f32 {
        paint::out_quad(paint::progress(arrived, self.now, FADE_MS))
    }
}

/// Paint a scene into a page of `size` at the origin, at opacity `alpha`.
/// `vis` is the on-screen part of the stage in this scene's coordinates:
/// every page is drawn in its own layer (so a page above covers the images
/// of the page below), and layers don't inherit their parent's clip.
fn draw_scene(r: &mut Renderer, size: Size, vis: Rectangle, scene: &Scene, alpha: f32, ctx: &Ctx<'_>, live: bool) {
    match scene {
        Scene::Page(p) => {
            let full = rect(0.0, 0.0, size.width, size.height);
            if let Some(clip) = full.intersection(&vis) {
                r.with_layer(clip, |r| draw_page(r, size, p, alpha, ctx, live));
            }
        }
        Scene::Transition { old, new, kind, t } => {
            let full = rect(0.0, 0.0, size.width, size.height);
            let t = *t;
            match kind {
                Kind::Slide(dir) => {
                    let off = -(*dir as f32) * size.width * t;
                    let shifted = |dx: f32| rect(vis.x - dx, vis.y, vis.width, vis.height);
                    r.with_translation(Vector::new(off, 0.0), |r| draw_scene(r, size, shifted(off), old, alpha, ctx, false));
                    let off2 = off + *dir as f32 * size.width;
                    r.with_translation(Vector::new(off2, 0.0), |r| {
                        draw_scene(r, size, shifted(off2), new, alpha, ctx, false)
                    });
                }
                Kind::Zoom { rect: rr, zoom_in } => {
                    let rr = fit_aspect(*rr, size);
                    let k = size.width / rr.width.max(1.0);
                    let magnified = rect(-rr.x * k, -rr.y * k, size.width * k, size.height * k);
                    let draw_in = |r: &mut Renderer, target: Rectangle, sc: &Scene, a: f32| {
                        let s = (target.width / size.width.max(1.0)).max(1e-4);
                        let local = rect((vis.x - target.x) / s, (vis.y - target.y) / s, vis.width / s, vis.height / s);
                        r.with_transformation(Transformation::translate(target.x, target.y) * Transformation::scale(s), |r| {
                            draw_scene(r, size, local, sc, a, ctx, false);
                        });
                    };
                    if *zoom_in {
                        draw_in(r, paint::lerp_rect(full, magnified, t), old, alpha * (1.0 - t));
                        draw_in(r, paint::lerp_rect(rr, full, t), new, alpha * (t * 1.4).min(1.0));
                    } else {
                        draw_in(r, paint::lerp_rect(magnified, full, t), new, alpha * (0.25 + t).min(1.0));
                        draw_in(r, paint::lerp_rect(full, rr, t), old, alpha * (1.0 - t * 1.3).max(0.0));
                    }
                }
            }
        }
    }
}

/// Grow `r` about its centre to the page's aspect ratio (contain).
fn fit_aspect(r: Rectangle, size: Size) -> Rectangle {
    let (w, h) = (size.width.max(1.0), size.height.max(1.0));
    if r.width <= 0.0 || r.height <= 0.0 {
        return rect(w * 0.45, h * 0.45, w * 0.1, h * 0.1);
    }
    let s = (r.width / w).max(r.height / h);
    let (nw, nh) = (w * s, h * s);
    rect(r.center_x() - nw / 2.0, r.center_y() - nh / 2.0, nw, nh)
}

fn draw_page(r: &mut Renderer, size: Size, page: &Page, alpha: f32, ctx: &Ctx<'_>, live: bool) {
    let full = rect(0.0, 0.0, size.width, size.height);
    paint::fill(r, full, paint::with_alpha(c_bg(), alpha));
    match page {
        Page::Month(y, m) => paint_month(r, size, *y, *m, alpha, ctx, live),
        Page::Year(y) => paint_year(r, size, *y, alpha, ctx, live),
        Page::Day(d) => {
            if let Some(f) = ctx.day_painter {
                f(r, full, *d, alpha);
            }
        }
    }
}

fn draw_tile(r: &mut Renderer, fe: &FileEntry, tile: Rectangle, radius: f32, alpha: f32, ctx: &Ctx<'_>) {
    let a = |c: Color, k: f32| paint::with_alpha(c, k);
    let empty = t::RAISED;
    match ctx.thumbs.get(&fe.path) {
        None => paint::rounded(r, tile, radius, a(empty, alpha)),
        Some(t) => {
            let f = ctx.fade(t.arrived);
            if f < 1.0 {
                paint::rounded(r, tile, radius, a(empty, alpha));
            }
            let (tw, th) = (t.width() as f32, t.height() as f32);
            let s = (tile.width / tw).max(tile.height / th);
            let (w, h) = (tw * s, th * s);
            let cover = rect(tile.center_x() - w / 2.0, tile.center_y() - h / 2.0, w, h);
            let level = t.level_for(w.max(h) * ctx.sf);
            let mut img = iced::advanced::image::Image::new(level.handle.clone()).filter_method(FilterMethod::Linear).opacity(alpha * f);
            img.border_radius = radius.into();
            r.draw_image(img, cover, tile);
        }
    }
}

fn month_pill(index: &LibraryIndex, year: i32, month: u32, size: Size) -> Option<Rectangle> {
    if index.is_empty() || index.days_in_month(year, month).next().is_some() {
        return None;
    }
    let text = format!("No photos in {} {}  —  go to the nearest  ›", MONTH_NAME[(month - 1) as usize], year);
    let w = paint::text_width(&text, 12.0, paint::SEMIBOLD) + 36.0;
    Some(rect((size.width - w) / 2.0, size.height - 64.0, w, 34.0))
}

fn year_pill(index: &LibraryIndex, year: i32, size: Size) -> Option<Rectangle> {
    if index.is_empty() || index.years().contains(&year) {
        return None;
    }
    let text = format!("No photos in {year}  —  go to the nearest  ›");
    let w = paint::text_width(&text, 12.0, paint::SEMIBOLD) + 36.0;
    Some(rect((size.width - w) / 2.0, (size.height - 34.0) / 2.0, w, 34.0))
}

fn paint_pill(r: &mut Renderer, pill: Rectangle, text: &str, h: f32, alpha: f32, fill_a: u8, clip: Rectangle) {
    let a = |c: Color| paint::with_alpha(c, alpha);
    let _ = fill_a;
    paint::framed(r, pill, 17.0, a(t::FLOAT), a(paint::lerp_color(t::LINE_HI, t::SIGNAL, h)), 1.0);
    paint::text(r, text, pill, t::T_BODY, paint::MEDIUM, a(paint::lerp_color(t::INK2, t::SIGNAL, h)), HAlign::Center, clip);
}

fn paint_month(r: &mut Renderer, size: Size, year: i32, month: u32, alpha: f32, ctx: &Ctx<'_>, live: bool) {
    let a = |c: Color| paint::with_alpha(c, alpha);
    let clip = rect(0.0, 0.0, size.width, size.height);
    let lay = month_layout(size, year, month);
    let first = first_weekday();
    for (i, (_, cell)) in lay.cells.iter().take(7).enumerate() {
        let hr = rect(cell.x, cell.y - MONTH_HEAD, cell.width, MONTH_HEAD - 4.0);
        let name = DAY_SHORT[((first as usize) + i) % 7];
        paint::text(r, name, hr, t::T_SMALL, paint::MEDIUM, a(t::INK3), HAlign::Center, clip);
    }
    let today = Local::now().date_naive();
    for (d, cell) in &lay.cells {
        let in_month = d.month() == month;
        let day = ctx.index.and_then(|ix| ix.get(*d));
        let h = ctx.h(HoverKey::Date(*d), live);
        let base = if in_month { t::MOUNT } else { t::GROUND };
        let fill = if in_month || day.is_some() { paint::lerp_color(base, t::RAISED, h) } else { base };
        paint::rounded(r, *cell, 8.0, a(fill));

        let num = rect(cell.x + 6.0, cell.y + 4.0, 24.0, 20.0);
        let font = if in_month { paint::BOLD } else { paint::REGULAR };
        let color = if *d == today {
            paint::circle(r, Point::new(num.center_x(), num.center_y()), 10.5, a(t::SIGNAL), Color::TRANSPARENT, 0.0);
            t::ON_SIGNAL
        } else if in_month {
            t::INK
        } else {
            t::INK4
        };
        paint::text(r, day_label(*d), num, 12.0, font, a(color), HAlign::Center, clip);

        match day {
            Some(_) if !in_month => {
                paint::circle(r, Point::new(cell.x + cell.width - 12.0, cell.y + 14.0), 3.0, a(t::alpha(t::SIGNAL, 0.45)), Color::TRANSPARENT, 0.0);
            }
            Some(day) => {
                let text = thousands(day.shot_count as u64);
                let tw = paint::text_width(&text, 10.0, paint::SEMIBOLD) + 12.0;
                let badge = rect(cell.x + cell.width - tw - 5.0, cell.y + 6.0, tw, 16.0);
                paint::rounded(r, badge, 8.0, a(paint::lerp_color(t::RAISED, t::alpha(t::SIGNAL, 0.22), h)));
                paint::text(r, &text, badge, 11.0, paint::MEDIUM, a(paint::lerp_color(t::INK2, t::SIGNAL, h)), HAlign::Center, clip);
                let area = rect(cell.x + 5.0, cell.y + MONTH_NUM_H + 4.0, cell.width - 10.0, cell.height - MONTH_NUM_H - 9.0);
                if area.height >= 22.0 && area.width >= 22.0 {
                    let n = if day.shot_count >= 4 && area.width >= 56.0 && area.height >= 44.0 { 4 } else { 1 };
                    let recs = day.preview_entries(n);
                    let tiles: Vec<Rectangle> = if n == 1 {
                        vec![area]
                    } else {
                        let g = 2.0;
                        let (tw2, th2) = ((area.width - g) / 2.0, (area.height - g) / 2.0);
                        (0..4)
                            .map(|i| rect(area.x + (i % 2) as f32 * (tw2 + g), area.y + (i / 2) as f32 * (th2 + g), tw2, th2))
                            .collect()
                    };
                    for (fe, tile) in recs.iter().zip(tiles) {
                        draw_tile(r, fe, tile, 5.0, alpha, ctx);
                    }
                } else if let Some(ix) = ctx.index {
                    let level = ix.heat_level(Some(day));
                    paint::rounded(r, rect(cell.x + 6.0, cell.y + cell.height - 7.0, cell.width - 12.0, 3.0), 1.5, a(paint::accent(HEAT_ALPHA[level])));
                }
            }
            None => {}
        }
        if h > 0.0 && (in_month || day.is_some()) {
            let col = if day.is_some() { t::alpha(t::SIGNAL, 0.6 * h) } else { t::alpha(t::INK3, 0.5 * h) };
            paint::framed(r, paint::inset(*cell, 0.6), 8.0, Color::TRANSPARENT, a(col), 1.2);
        }
    }
    if let Some(ix) = ctx.index {
        if let Some(pill) = month_pill(ix, year, month, size) {
            let text = format!("No photos in {} {}  —  go to the nearest  ›", MONTH_NAME[(month - 1) as usize], year);
            paint_pill(r, pill, &text, ctx.h(HoverKey::Pill, live), alpha, 235, clip);
        }
    }
}

fn paint_year(r: &mut Renderer, size: Size, year: i32, alpha: f32, ctx: &Ctx<'_>, live: bool) {
    let a = |c: Color| paint::with_alpha(c, alpha);
    let clip = rect(0.0, 0.0, size.width, size.height);
    let lay = year_layout(size, year);
    let sq = lay.sq;
    let today = Local::now().date_naive();
    let inset = (sq * 0.09).max(1.0);
    let radius = (sq * 0.22).max(1.5);
    let show_nums = sq >= 20.0;
    let first = first_weekday();
    for (month, _bx, title) in &lay.months {
        let h = ctx.h(HoverKey::Month(*month), live);
        let count = ctx.index.map(|ix| ix.month_shot_count(year, *month)).unwrap_or(0);
        let base = if count > 0 { t::INK } else { t::INK3 };
        paint::text(r, MONTH_NAME[(*month - 1) as usize], *title, 14.0, paint::SEMIBOLD, a(paint::lerp_color(base, t::SIGNAL, h)), HAlign::Left, clip);
        if count > 0 {
            paint::text(r, &thousands(count as u64), *title, 11.0, paint::MEDIUM, a(t::INK3), HAlign::Right, clip);
        }
        if h > 0.0 {
            let y = title.y + title.height + 1.0;
            paint::hline(r, title.x, title.x + title.width * h, y, 1.2, a(t::alpha(t::SIGNAL, 0.7 * h)));
        }
        let wy = title.y + title.height + 4.0;
        for i in 0..7 {
            let name = DAY_NARROW[((first as usize) + i) % 7];
            paint::text(r, name, rect(title.x + i as f32 * sq, wy, sq, WEEK_H - 4.0), 10.0, paint::MEDIUM, a(t::INK3), HAlign::Center, clip);
        }
    }
    let num_px = (sq * 0.42).floor().clamp(8.0, 12.0);
    for (d, sr) in &lay.squares {
        let day = ctx.index.and_then(|ix| ix.get(*d));
        let level = ctx.index.map(|ix| ix.heat_level(day)).unwrap_or(0);
        let mut cell = paint::inset(*sr, inset);
        let h = ctx.h(HoverKey::Date(*d), live);
        if h > 0.0 && day.is_some() {
            let g = h * inset * 1.4;
            cell = rect(cell.x - g, cell.y - g, cell.width + 2.0 * g, cell.height + 2.0 * g);
        }
        let fill = if level > 0 { paint::accent(HEAT_ALPHA[level]) } else { t::RAISED };
        paint::rounded(r, cell, radius, a(fill));
        if *d == today {
            paint::framed(r, rect(cell.x - 1.0, cell.y - 1.0, cell.width + 2.0, cell.height + 2.0), radius + 1.0, Color::TRANSPARENT, a(t::SIGNAL), 1.4);
        }
        if h > 0.0 && day.is_some() {
            paint::framed(r, cell, radius, Color::TRANSPARENT, a(paint::rgba8(255, 255, 255, (h * 200.0) as u8)), 1.2);
        }
        if show_nums {
            let col = if level >= 3 {
                t::ON_SIGNAL
            } else if level > 0 {
                t::INK
            } else {
                t::INK3
            };
            paint::text(r, day_label(*d), cell, num_px, paint::SEMIBOLD, a(col), HAlign::Center, clip);
        }
    }
    // Legend
    let s = 11.0;
    let y = size.height - 22.0;
    let x = size.width - 20.0 - (s + 3.0) * (HEAT_LEVELS as f32 + 1.0) - 70.0;
    paint::text(r, "Fewer", rect(x - 48.0, y - 3.0, 44.0, s + 6.0), 11.0, paint::REGULAR, a(t::INK3), HAlign::Right, clip);
    for lvl in 0..=HEAT_LEVELS {
        let fill = if lvl > 0 { paint::accent(HEAT_ALPHA[lvl]) } else { t::RAISED };
        paint::rounded(r, rect(x + lvl as f32 * (s + 3.0), y, s, s), 2.5, a(fill));
    }
    paint::text(r, "More", rect(x + (HEAT_LEVELS as f32 + 1.0) * (s + 3.0) + 4.0, y - 3.0, 60.0, s + 6.0), 11.0, paint::REGULAR, a(t::INK3), HAlign::Left, clip);

    if let Some(ix) = ctx.index {
        if let Some(pill) = year_pill(ix, year, size) {
            let text = format!("No photos in {year}  —  go to the nearest  ›");
            paint_pill(r, pill, &text, ctx.h(HoverKey::Pill, live), alpha, 240, clip);
        }
    }

    // Hover card for the hovered day.
    if !live {
        return;
    }
    let Some(HoverKey::Date(d)) = ctx.hover_key else { return };
    let Some(ix) = ctx.index else { return };
    let Some(day) = ix.get(d) else { return };
    let h = ctx.h(HoverKey::Date(d), live);
    let sqr = lay.day_rect(d);
    let (cw, ch) = (200.0, 176.0);
    let mut x = sqr.center_x() - cw / 2.0;
    let mut y = sqr.y + sqr.height + 8.0;
    if y + ch > size.height - 4.0 {
        y = sqr.y - ch - 8.0;
    }
    x = x.clamp(6.0, (size.width - cw - 6.0).max(6.0));
    let card = rect(x, y, cw, ch);
    let ca = alpha * h;
    let b = |c: Color| paint::with_alpha(c, ca);
    paint::rounded(r, rect(card.x, card.y + 6.0, card.width, card.height), 10.0, b(t::alpha(Color::BLACK, 0.35)));
    paint::framed(r, card, 10.0, b(t::FLOAT), b(t::LINE_HI), 1.0);
    let thumb = rect(x + 8.0, y + 8.0, cw - 16.0, ch - 56.0);
    if let Some(fe) = day.preview_entries(1).first() {
        draw_tile(r, fe, thumb, 6.0, ca, ctx);
    }
    const DOW: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let title = format!("{}, {} {}", DOW[d.weekday().num_days_from_monday() as usize], MONTH_ABBR[d.month0() as usize], d.day());
    paint::text(r, &title, rect(x + 10.0, y + ch - 44.0, cw - 20.0, 20.0), t::T_BODY, paint::SEMIBOLD, b(t::INK), HAlign::Left, clip);
    paint::text(r, &plural(day.shot_count as u64, "photo"), rect(x + 10.0, y + ch - 24.0, cw - 20.0, 18.0), t::T_SMALL, paint::REGULAR, b(t::INK3), HAlign::Left, clip);
    let action = if day.folders.len() > 1 { "Choose a folder  ›" } else { "Open  ›" };
    paint::text(r, action, rect(x + 10.0, y + ch - 24.0, cw - 20.0, 18.0), t::T_SMALL, paint::MEDIUM, b(t::SIGNAL), HAlign::Right, clip);
}

impl<'a, Message: 'a> From<CalendarStage<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(s: CalendarStage<'a, Message>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_click_on_a_day_shows_its_photos_instead_of_opening_it() {
        let d = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let r = rect(10.0, 20.0, 100.0, 80.0);
        assert!(matches!(with_alt(StageEvent::DayActivated(d, r), false), StageEvent::DayActivated(x, _) if x == d));
        assert!(matches!(with_alt(StageEvent::DayActivated(d, r), true), StageEvent::DayBrowse(x, _) if x == d));
        // Alt changes nothing else.
        assert!(matches!(with_alt(StageEvent::MonthActivated(3, r), true), StageEvent::MonthActivated(3, _)));
    }
}
