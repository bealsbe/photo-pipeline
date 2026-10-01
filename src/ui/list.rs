//! The list view: one row per shot (a RAW+JPG pair shares a row), four
//! columns, virtualised like the grid.
//!
//! Selection lives in the app (as the primary record of each row); this
//! widget owns scrolling, hover and tooltips, and turns clicks and arrow
//! keys into selection events.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::image::Renderer as _;
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse as amouse};
use iced::keyboard::{self, key::Named};
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Element, Event, Length, Point, Rectangle, Size, window};

use super::icons::{self, Glyph};
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::model::collection::{Collection, RecordId};
use crate::model::filter::ListRow;
use crate::util;

pub const ROW_H: f32 = 30.0;
const HEADER_H: f32 = 32.0;
const SCROLLBAR_W: f32 = 10.0;
/// Marks: keep / prune, then crop.
const STATUS_W: f32 = 52.0;
const TYPE_W: f32 = 100.0;
const SIZE_W: f32 = 96.0;
const MOD_W: f32 = 156.0;
const PAD_X: f32 = 12.0;
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);

#[derive(Debug, Clone)]
pub enum ListEvent {
    /// Select `id` (the row's primary record) with these modifiers.
    Click { id: RecordId, modifiers: keyboard::Modifiers },
    Activate(RecordId),
    SelectAll,
    Scrolled(f32),
}

#[derive(Clone, Copy)]
pub struct ListData<'a> {
    pub rows: &'a [ListRow],
    pub coll: &'a Collection,
    pub selection: &'a HashSet<RecordId>,
    pub current: Option<RecordId>,
}

pub struct List<'a, Message> {
    data: ListData<'a>,
    keyboard: bool,
    initial_scroll: f32,
    on: Box<dyn Fn(ListEvent) -> Message + 'a>,
}

impl<'a, Message> List<'a, Message> {
    pub fn new(data: ListData<'a>, on: impl Fn(ListEvent) -> Message + 'a) -> Self {
        Self { data, keyboard: true, initial_scroll: 0.0, on: Box::new(on) }
    }

    pub fn keyboard(mut self, on: bool) -> Self {
        self.keyboard = on;
        self
    }

    pub fn initial_scroll(mut self, y: f32) -> Self {
        self.initial_scroll = y;
        self
    }
}

#[derive(Clone, Copy)]
struct Anim {
    from: f32,
    to: f32,
    start: Instant,
}

struct State {
    scroll: f32,
    anim: Option<Anim>,
    hover_row: Option<usize>,
    hover_bar: bool,
    cursor: Point,
    hover_since: Instant,
    last_click: Option<amouse::Click>,
    drag_bar: Option<f32>,
    reported: f32,
    init: bool,
}

fn max_scroll(n: usize, h: f32) -> f32 {
    (n as f32 * ROW_H - (h - HEADER_H)).max(0.0)
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for List<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            scroll: self.initial_scroll,
            anim: None,
            hover_row: None,
            hover_bar: false,
            cursor: Point::ORIGIN,
            hover_since: Instant::now(),
            last_click: None,
            drag_bar: None,
            reported: self.initial_scroll,
            init: false,
        })
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
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        let n = self.data.rows.len();
        let maxs = max_scroll(n, bounds.height);
        if let Some(a) = st.anim {
            let t = (now.saturating_duration_since(a.start).as_secs_f32() / 0.18).min(1.0);
            st.scroll = paint::lerp(a.from, a.to, paint::out_cubic(t));
            if t >= 1.0 {
                st.anim = None;
            }
        }
        st.scroll = st.scroll.clamp(0.0, maxs);
        if !st.init {
            st.init = true;
        }
        let body = rect(bounds.x, bounds.y + HEADER_H, bounds.width, bounds.height - HEADER_H);
        let row_at = |st: &State, p: Point| -> Option<usize> {
            if !body.contains(p) || p.x > bounds.x + bounds.width - SCROLLBAR_W {
                return None;
            }
            let i = ((p.y - body.y + st.scroll) / ROW_H).floor() as usize;
            (i < n).then_some(i)
        };

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("list", *at);
                if st.anim.is_some() {
                    shell.request_redraw();
                } else if (st.scroll - st.reported).abs() > 0.5 {
                    st.reported = st.scroll;
                    shell.publish((self.on)(ListEvent::Scrolled(st.scroll)));
                }
                if st.hover_row.is_some() {
                    let at = st.hover_since + TOOLTIP_DELAY;
                    if now < at {
                        shell.request_redraw_at(window::RedrawRequest::At(at));
                    }
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if cursor.position_in(bounds).is_none() {
                    return;
                }
                match delta {
                    ScrollDelta::Lines { y, .. } => {
                        let base = st.anim.map(|a| a.to).unwrap_or(st.scroll);
                        let to = (base - y * ROW_H * 3.0).clamp(0.0, maxs);
                        st.anim = Some(Anim { from: st.scroll, to, start: now });
                    }
                    ScrollDelta::Pixels { y, .. } => {
                        st.anim = None;
                        st.scroll = (st.scroll - y).clamp(0.0, maxs);
                    }
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(p) = cursor.position() else { return };
                if let Some(grab) = st.drag_bar {
                    let track = body.height;
                    let thumb = scroll_thumb(n, body.height, st.scroll).1;
                    let frac = ((p.y - body.y - grab) / (track - thumb).max(1.0)).clamp(0.0, 1.0);
                    st.scroll = frac * maxs;
                    st.anim = None;
                    shell.request_redraw();
                    return;
                }
                let hr = row_at(st, p);
                let hb = cursor.position_in(body).is_some() && p.x >= bounds.x + bounds.width - SCROLLBAR_W;
                if hr != st.hover_row || hb != st.hover_bar || p != st.cursor {
                    if hr != st.hover_row || p != st.cursor {
                        st.hover_since = now;
                    }
                    st.hover_row = hr;
                    st.hover_bar = hb;
                    st.cursor = p;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                st.hover_row = None;
                st.hover_bar = false;
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_in(bounds).and(cursor.position()) else { return };
                shell.capture_event();
                st.hover_since = now;
                if p.x >= bounds.x + bounds.width - SCROLLBAR_W && n as f32 * ROW_H > body.height {
                    let (y, h) = scroll_thumb(n, body.height, st.scroll);
                    let ly = p.y - body.y;
                    if ly >= y && ly <= y + h {
                        st.drag_bar = Some(ly - y);
                    } else {
                        let page = if ly < y { -body.height } else { body.height };
                        let to = (st.scroll + page).clamp(0.0, maxs);
                        st.anim = Some(Anim { from: st.scroll, to, start: now });
                        shell.request_redraw();
                    }
                    return;
                }
                if let Some(i) = row_at(st, p) {
                    let id = self.data.rows[i].primary();
                    let click = amouse::Click::new(p, mouse::Button::Left, st.last_click);
                    st.last_click = Some(click);
                    if click.kind() == amouse::click::Kind::Double {
                        shell.publish((self.on)(ListEvent::Activate(id)));
                    } else {
                        shell.publish((self.on)(ListEvent::Click { id, modifiers: crate::ui::modifiers::get() }));
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.drag_bar = None;
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) if self.keyboard && n > 0 => {
                let cur = self.data.current.and_then(|c| self.data.rows.iter().position(|r| r.primary() == c));
                let page = ((body.height / ROW_H).floor() as usize).max(1);
                let target = match key {
                    keyboard::Key::Named(Named::ArrowUp) => cur.map(|c| c.saturating_sub(1)).unwrap_or(0),
                    keyboard::Key::Named(Named::ArrowDown) => cur.map(|c| (c + 1).min(n - 1)).unwrap_or(0),
                    keyboard::Key::Named(Named::PageUp) => cur.map(|c| c.saturating_sub(page)).unwrap_or(0),
                    keyboard::Key::Named(Named::PageDown) => cur.map(|c| (c + page).min(n - 1)).unwrap_or(0),
                    keyboard::Key::Named(Named::Home) => 0,
                    keyboard::Key::Named(Named::End) => n - 1,
                    keyboard::Key::Named(Named::Enter) => {
                        if let Some(c) = cur {
                            shell.publish((self.on)(ListEvent::Activate(self.data.rows[c].primary())));
                            shell.capture_event();
                        }
                        return;
                    }
                    keyboard::Key::Character(c) if c.as_str() == "a" && modifiers.control() => {
                        shell.publish((self.on)(ListEvent::SelectAll));
                        shell.capture_event();
                        return;
                    }
                    _ => return,
                };
                let mods = keyboard::Modifiers::SHIFT & *modifiers;
                shell.publish((self.on)(ListEvent::Click { id: self.data.rows[target].primary(), modifiers: mods }));
                // Keep the current row in view.
                let row_top = target as f32 * ROW_H;
                if row_top < st.scroll {
                    st.scroll = row_top;
                } else if row_top + ROW_H > st.scroll + body.height {
                    st.scroll = row_top + ROW_H - body.height;
                }
                st.anim = None;
                shell.capture_event();
                shell.request_redraw();
            }
            _ => {}
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
        let _t = crate::profile::span("draw", "list");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let d = &self.data;
        let n = d.rows.len();
        paint::fill(r, b, t::GROUND);
        let width = b.width - SCROLLBAR_W;
        let status_x = b.x + PAD_X;
        let name_x = status_x + STATUS_W;
        let name_w = (width - PAD_X - STATUS_W - TYPE_W - SIZE_W - MOD_W - PAD_X).max(60.0);
        let type_x = name_x + name_w;
        let size_x = type_x + TYPE_W;
        let mod_x = size_x + SIZE_W + 24.0;
        // Header
        let header = rect(b.x, b.y, b.width, HEADER_H);
        paint::fill(r, header, t::MOUNT);
        paint::hline(r, b.x, b.x + b.width, b.y + HEADER_H - 1.0, 1.0, t::LINE);
        let head = |r: &mut Renderer, label: &str, x: f32, w: f32, align: HAlign| {
            paint::text(r, label, rect(x, b.y, w, HEADER_H), t::T_SMALL, paint::MEDIUM, t::INK3, align, header);
        };
        head(r, "Name", name_x, name_w, HAlign::Left);
        head(r, "Type", type_x, TYPE_W, HAlign::Left);
        head(r, "Size", size_x, SIZE_W, HAlign::Right);
        head(r, "Modified", mod_x, MOD_W, HAlign::Left);

        let body = rect(b.x, b.y + HEADER_H, b.width, b.height - HEADER_H);
        let first = (st.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((st.scroll + body.height) / ROW_H).ceil() as usize).min(n);
        // Rows' fills, then their icons in a layer above (images draw above
        // same-layer quads), then text.
        let mut icons_to_draw: Vec<(Glyph, iced::Color, Rectangle)> = Vec::new();
        let mut tags_to_draw: Vec<(super::tags::Kind, f32)> = Vec::new();
        r.with_layer(body, |r| {
            for i in first..last {
                let row = &d.rows[i];
                let y = body.y + i as f32 * ROW_H - st.scroll;
                let rr = rect(b.x, y, width, ROW_H);
                let pid = row.primary();
                let Some(primary) = d.coll.get(pid) else { continue };
                let selected = d.selection.contains(&pid);
                if selected {
                    paint::fill(r, rr, t::alpha(t::SIGNAL, 0.16));
                    paint::fill(r, rect(b.x, y, 3.0, ROW_H), t::SIGNAL);
                } else if st.hover_row == Some(i) {
                    paint::fill(r, rr, t::RAISED);
                } else if i % 2 == 1 {
                    paint::fill(r, rr, t::alpha(iced::Color::WHITE, 0.02));
                }
                let raw = row.raw.and_then(|id| d.coll.get(id));
                let jpg = row.jpg.and_then(|id| d.coll.get(id));
                let paired = raw.is_some() && jpg.is_some();
                let display = match (paired, raw) {
                    (true, Some(raw)) => raw.stem(),
                    _ => primary.filename(),
                };
                let size = raw.map(|x| x.file_size).unwrap_or(0) + jpg.map(|x| x.file_size).unwrap_or(0);
                let modified = [raw, jpg].iter().flatten().map(|x| x.modified.local).max().unwrap_or(primary.modified.local);

                // Marks
                let icon = 16.0;
                let iy = (y + (ROW_H - icon) / 2.0).round();
                if primary.is_pruned {
                    icons_to_draw.push((Glyph::Prune, t::REJECT, rect(status_x, iy, icon, icon)));
                } else if primary.is_marked {
                    icons_to_draw.push((Glyph::Keep, t::KEEP, rect(status_x, iy, icon, icon)));
                }
                if primary.is_cropped() {
                    icons_to_draw.push((Glyph::Scissors, t::SIGNAL, rect(status_x + icon + 6.0, iy, icon, icon)));
                }

                // Name
                let name_color = if primary.is_pruned { t::INK3 } else { t::INK };
                let cell = rect(name_x, y, name_w - 12.0, ROW_H);
                let clip = cell.intersection(&body).unwrap_or_default();
                let shown = paint::elide_right(&display, t::T_BODY, paint::REGULAR, cell.width);
                paint::text(r, &shown, cell, t::T_BODY, paint::REGULAR, name_color, HAlign::Left, clip);
                if primary.is_pruned {
                    let tw = paint::text_width(&shown, t::T_BODY, paint::REGULAR).min(cell.width);
                    paint::hline(r, cell.x, cell.x + tw, (y + ROW_H / 2.0).round(), 1.0, t::INK3);
                }

                // Type: one tag (a pair's is two-part), drawn with the icons
                // (they are images).
                if let Some(kind) = super::tags::Kind::from_halves(raw.is_some(), jpg.is_some()) {
                    tags_to_draw.push((kind, y));
                }

                // Size and date
                let sclip = rect(size_x, y, SIZE_W, ROW_H).intersection(&body).unwrap_or_default();
                paint::text(r, &util::fmt_size(size), rect(size_x, y, SIZE_W, ROW_H), t::T_SMALL, paint::REGULAR, t::INK2, HAlign::Right, sclip);
                let mclip = rect(mod_x, y, MOD_W, ROW_H).intersection(&body).unwrap_or_default();
                paint::text(r, &modified.format("%Y-%m-%d %H:%M").to_string(), rect(mod_x, y, MOD_W, ROW_H), t::T_SMALL, paint::REGULAR, t::INK2, HAlign::Left, mclip);
            }
        });
        r.with_layer(body, |r| {
            for (glyph, color, ir) in &icons_to_draw {
                r.draw_image(iced::advanced::image::Image::new(icons::handle(*glyph, ir.width, *color)).filter_method(FilterMethod::Linear), *ir, body);
            }
            for (kind, y) in &tags_to_draw {
                super::tags::draw_kind(r, type_x, y + ROW_H / 2.0, *kind, 1.0, body);
            }
        });

        // Scrollbar
        if n as f32 * ROW_H > body.height {
            let (y, h) = scroll_thumb(n, body.height, st.scroll);
            let (color, w) = if st.drag_bar.is_some() {
                (t::INK2, 8.0)
            } else if st.hover_bar {
                (t::INK3, 8.0)
            } else {
                (t::LINE_HI, 6.0)
            };
            let x = b.x + b.width - (SCROLLBAR_W + w) / 2.0;
            paint::rounded(r, rect(x, body.y + y + 2.0, w, (h - 4.0).max(8.0)), w / 2.0, color);
        }

        // Tooltip on the filename column
        if let Some(i) = st.hover_row {
            let name_end = b.x + b.width - SCROLLBAR_W - PAD_X - TYPE_W - SIZE_W - MOD_W;
            if Instant::now() >= st.hover_since + TOOLTIP_DELAY && st.cursor.x < name_end && i < n {
                let row = &d.rows[i];
                let mut lines = Vec::new();
                if let Some(raw) = row.raw.and_then(|id| d.coll.get(id)) {
                    lines.push(format!("RAW: {}", raw.path.display()));
                }
                if let Some(jpg) = row.jpg.and_then(|id| d.coll.get(id)) {
                    lines.push(format!("JPG: {}", jpg.path.display()));
                }
                if !lines.is_empty() {
                    super::grid::draw_tooltip(r, b, st.cursor, &lines.join("\n"));
                }
            }
        }
    }
}

fn scroll_thumb(n: usize, h: f32, scroll: f32) -> (f32, f32) {
    let total = n as f32 * ROW_H;
    if total <= h {
        return (0.0, h);
    }
    let thumb = (h / total * h).max(24.0);
    let y = (scroll / (total - h)).clamp(0.0, 1.0) * (h - thumb);
    (y, thumb)
}

impl<'a, Message: 'a> From<List<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(l: List<'a, Message>) -> Self {
        Element::new(l)
    }
}
