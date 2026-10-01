//! The left navigation rail, expandable like a hamburger menu.
//!
//! Collapsed it is a 48 px strip of icons; ☰ slides it open to show each
//! command's name and shortcut.  The panel floats over the content (only a
//! fixed 48 px slot is in the layout) so opening it never re-flows the grid,
//! and a scrim dims the content behind it.  This widget is a full-size stack
//! layer that only claims events inside the rail — or everywhere while open.

use std::time::{Duration, Instant};

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{self, Cursor};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, window};

use super::icons::{self, Glyph};
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;

pub const RAIL_W: f32 = 48.0;
const MARGIN: f32 = 6.0;
const BTN_H: f32 = 36.0;
const ROW_GAP: f32 = 2.0;
const SEP_H: f32 = 13.0;
const ICON: f32 = 18.0;
const TEXT_X: f32 = 44.0;
const ANIM_MS: f32 = 220.0;
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);

pub struct Item<C> {
    pub cmd: C,
    pub icon: Option<Glyph>,
    pub label: &'static str,
    pub hint: String,
    pub tooltip: String,
    pub enabled: bool,
    pub checked: bool,
}

pub enum Entry<C> {
    Item(Item<C>),
    Separator,
    /// Pushes the entries after it to the foot of the rail, room allowing.
    Spacer,
}

pub struct SideMenu<'a, C, Message> {
    entries: Vec<Entry<C>>,
    expanded: bool,
    on_cmd: Box<dyn Fn(C) -> Message + 'a>,
    on_burger: Message,
    on_collapse: Message,
}

impl<'a, C: Copy, Message: Clone> SideMenu<'a, C, Message> {
    pub fn new(
        entries: Vec<Entry<C>>,
        expanded: bool,
        on_cmd: impl Fn(C) -> Message + 'a,
        on_burger: Message,
        on_collapse: Message,
    ) -> Self {
        Self { entries, expanded, on_cmd: Box::new(on_cmd), on_burger, on_collapse }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Burger,
    Item(usize),
    Scrim,
}

struct State {
    t: f32,
    from: f32,
    to: f32,
    start: Instant,
    ms: f32,
    hover: Hit,
    pressed: Hit,
    hover_since: Instant,
    cursor: Point,
    open_w: Option<f32>,
}

impl State {
    fn progress(&self, now: Instant) -> f32 {
        let k = (now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.ms.max(1.0)).clamp(0.0, 1.0);
        paint::lerp(self.from, self.to, paint::out_cubic(k))
    }

    fn animating(&self, now: Instant) -> bool {
        (self.progress(now) - self.to).abs() > 1e-3
    }
}

impl<'a, C: Copy, Message: Clone> SideMenu<'a, C, Message> {
    fn expanded_width(&self, st: &State) -> f32 {
        if let Some(w) = st.open_w {
            return w;
        }
        let mut need: f32 = 0.0;
        for e in &self.entries {
            if let Entry::Item(it) = e {
                let mut w = TEXT_X + paint::text_width(it.label, t::T_BODY, paint::SEMIBOLD) + 16.0;
                if !it.hint.is_empty() {
                    w += paint::text_width(&it.hint, t::T_SMALL, paint::REGULAR) + 20.0;
                }
                need = need.max(w);
            }
        }
        (need + 2.0 * MARGIN).clamp(220.0, 320.0)
    }

    /// (entry index, rect) for each button, top to bottom.
    fn rows(&self, bounds: Rectangle, width: f32) -> (Rectangle, Vec<(usize, Rectangle)>, Vec<f32>) {
        let bw = width - 2.0 * MARGIN;
        let mut y = bounds.y + 6.0;
        let burger = rect(bounds.x + MARGIN, y, bw, BTN_H);
        y += BTN_H + ROW_GAP;
        let mut seps = vec![y + SEP_H / 2.0];
        y += SEP_H + ROW_GAP;
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                Entry::Item(_) => {
                    out.push((i, rect(bounds.x + MARGIN, y, bw, BTN_H)));
                    y += BTN_H + ROW_GAP;
                }
                Entry::Separator => {
                    seps.push(y + SEP_H / 2.0);
                    y += SEP_H + ROW_GAP;
                }
                Entry::Spacer => {
                    let rest: f32 = self.entries[i + 1..]
                        .iter()
                        .map(|e| match e {
                            Entry::Item(_) => BTN_H + ROW_GAP,
                            Entry::Separator => SEP_H + ROW_GAP,
                            Entry::Spacer => 0.0,
                        })
                        .sum();
                    y = y.max(bounds.y + bounds.height - 6.0 - rest + ROW_GAP);
                }
            }
        }
        (burger, out, seps)
    }

    fn hit(&self, bounds: Rectangle, width: f32, t: f32, p: Point) -> Hit {
        let panel = rect(bounds.x, bounds.y, width, bounds.height);
        if !panel.contains(p) {
            return if t > 0.0 { Hit::Scrim } else { Hit::None };
        }
        let (burger, rows, _) = self.rows(bounds, width);
        if burger.contains(p) {
            return Hit::Burger;
        }
        for (i, r) in rows {
            if r.contains(p) {
                return Hit::Item(i);
            }
        }
        Hit::None
    }
}

impl<'a, C: Copy, Message: Clone> Widget<Message, iced::Theme, Renderer> for SideMenu<'a, C, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        let v = if self.expanded { 1.0 } else { 0.0 };
        tree::State::new(State {
            t: v,
            from: v,
            to: v,
            start: Instant::now(),
            ms: ANIM_MS,
            hover: Hit::None,
            pressed: Hit::None,
            hover_since: Instant::now(),
            cursor: Point::ORIGIN,
            open_w: None,
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let target = if self.expanded { 1.0 } else { 0.0 };
        if (st.to - target).abs() > 1e-3 {
            let now = Instant::now();
            let cur = st.progress(now);
            st.from = cur;
            st.to = target;
            st.start = now;
            // Keep the pace constant when reversing mid-slide.
            st.ms = (ANIM_MS * (target - cur).abs()).max(60.0);
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
        let bounds = layout.bounds();
        let now = Instant::now();
        let t = {
            let st = tree.state.downcast_mut::<State>();
            st.t = st.progress(now);
            st.t
        };
        let width_open = {
            let st = tree.state.downcast_ref::<State>();
            self.expanded_width(st)
        };
        let st = tree.state.downcast_mut::<State>();
        st.open_w = Some(width_open);
        let width = if t > 0.0 { (RAIL_W + (width_open - RAIL_W) * t).round() } else { RAIL_W };

        match event {
            Event::Window(window::Event::RedrawRequested(_)) => {
                if st.animating(now) {
                    shell.request_redraw();
                }
                if matches!(st.hover, Hit::Item(_) | Hit::Burger) && t <= 0.5 {
                    let at = st.hover_since + TOOLTIP_DELAY;
                    if now < at {
                        shell.request_redraw_at(window::RedrawRequest::At(at));
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let h = match cursor.position() {
                    Some(p) if bounds.contains(p) => self.hit(bounds, width, t, p),
                    _ => Hit::None,
                };
                if let Some(p) = cursor.position() {
                    if p != st.cursor {
                        st.cursor = p;
                        if matches!(h, Hit::Item(_) | Hit::Burger) {
                            st.hover_since = now;
                        }
                    }
                }
                if h != st.hover {
                    st.hover = h;
                    st.hover_since = now;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                st.hover = Hit::None;
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position() else { return };
                if !bounds.contains(p) {
                    return;
                }
                let h = self.hit(bounds, width, t, p);
                match h {
                    Hit::None => {
                        if p.x < bounds.x + width {
                            shell.capture_event();
                        }
                    }
                    Hit::Scrim => {
                        shell.publish(self.on_collapse.clone());
                        shell.capture_event();
                    }
                    _ => {
                        st.pressed = h;
                        shell.capture_event();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let pressed = std::mem::replace(&mut st.pressed, Hit::None);
                let Some(p) = cursor.position() else { return };
                let h = self.hit(bounds, width, t, p);
                if pressed != Hit::None && h == pressed {
                    match h {
                        Hit::Burger => shell.publish(self.on_burger.clone()),
                        Hit::Item(i) => {
                            if let Some(Entry::Item(it)) = self.entries.get(i) {
                                if it.enabled {
                                    shell.publish((self.on_cmd)(it.cmd));
                                }
                            }
                        }
                        _ => {}
                    }
                    shell.capture_event();
                }
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                if let Some(p) = cursor.position() {
                    if bounds.contains(p) && (t > 0.0 || p.x < bounds.x + RAIL_W) {
                        shell.capture_event();
                    }
                }
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
        let Some(p) = cursor.position() else { return mouse::Interaction::None };
        if !b.contains(p) {
            return mouse::Interaction::None;
        }
        let width = if st.t > 0.0 { RAIL_W + (st.open_w.unwrap_or(RAIL_W) - RAIL_W) * st.t } else { RAIL_W };
        if st.t <= 0.0 && p.x >= b.x + RAIL_W {
            return mouse::Interaction::None;
        }
        match st.hover {
            Hit::Burger => mouse::Interaction::Pointer,
            Hit::Item(i) => match self.entries.get(i) {
                Some(Entry::Item(it)) if it.enabled => mouse::Interaction::Pointer,
                _ => mouse::Interaction::Idle,
            },
            _ if p.x < b.x + width => mouse::Interaction::Idle,
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
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let t = st.progress(now);
        let open_w = st.open_w.unwrap_or_else(|| self.expanded_width(st));
        let width = if t > 0.0 { (RAIL_W + (open_w - RAIL_W) * t).round() } else { RAIL_W };

        r.with_layer(b, |r| {
            if t > 0.0 {
                paint::fill(r, b, t::alpha(Color::BLACK, 0.5 * t));
            }
            let panel = rect(b.x, b.y, width, b.height);
            paint::fill(r, panel, t::MOUNT);
            paint::vline(r, b.x + width - 1.0, b.y, b.y + b.height, 1.0, t::LINE);

            let (burger, rows, seps) = self.rows(b, width);
            r.with_layer(panel, |r| {
                // Burger: ☰ morphs into ←
                let bh = st.hover == Hit::Burger;
                let bp = st.pressed == Hit::Burger;
                if bh || bp {
                    paint::rounded(r, burger, t::R_CONTROL, if bp { t::RAISED_HI } else { t::RAISED });
                }
                let cx = burger.x + (RAIL_W - 2.0 * MARGIN) / 2.0;
                let cy = burger.center_y();
                let lerp = |a: Point, c: Point| Point::new(a.x + (c.x - a.x) * t, a.y + (c.y - a.y) * t);
                let lines = [
                    (lerp(Point::new(cx - 7.0, cy - 5.0), Point::new(cx - 6.0, cy)), lerp(Point::new(cx + 7.0, cy - 5.0), Point::new(cx - 0.5, cy - 6.0))),
                    (lerp(Point::new(cx - 7.0, cy), Point::new(cx - 6.0, cy)), lerp(Point::new(cx + 7.0, cy), Point::new(cx + 7.0, cy))),
                    (lerp(Point::new(cx - 7.0, cy + 5.0), Point::new(cx - 6.0, cy)), lerp(Point::new(cx + 7.0, cy + 5.0), Point::new(cx - 0.5, cy + 6.0))),
                ];
                let col = if bh || t > 0.0 { t::INK } else { t::INK2 };
                paint::strokes(r, &lines, 1.6, col, true);
                let fade = ((t - 0.35) / 0.65).max(0.0);
                if t > 0.0 {
                    let tr = rect(burger.x + TEXT_X, burger.y, burger.width - TEXT_X - 6.0, BTN_H);
                    paint::text(r, "Photo Pipeline", tr, 14.0, paint::SEMIBOLD, paint::with_alpha(t::INK, fade), HAlign::Left, panel);
                }
                for y in &seps {
                    paint::hline(r, b.x + MARGIN + 4.0, b.x + width - MARGIN - 4.0, y.floor(), 1.0, t::LINE);
                }
                for (i, br) in &rows {
                    let Some(Entry::Item(it)) = self.entries.get(*i) else { continue };
                    let hovered = st.hover == Hit::Item(*i);
                    let down = st.pressed == Hit::Item(*i);
                    if down && it.enabled {
                        paint::rounded(r, *br, t::R_CONTROL, t::RAISED_HI);
                    } else if it.checked || (hovered && it.enabled) {
                        paint::rounded(r, *br, t::R_CONTROL, t::RAISED);
                    }
                    if it.checked {
                        // The active view: an accent bar on the rail's edge.
                        paint::rounded(r, rect(b.x + 1.0, br.y + 8.0, 3.0, BTN_H - 16.0), 1.5, t::SIGNAL);
                    }
                    if let Some(glyph) = it.icon {
                        let x = br.x + (RAIL_W - 2.0 * MARGIN - ICON) / 2.0;
                        let y = br.y + (BTN_H - ICON) / 2.0;
                        let color = if !it.enabled {
                            t::INK4
                        } else if it.checked {
                            t::SIGNAL
                        } else if hovered {
                            t::INK
                        } else {
                            t::INK2
                        };
                        r.draw_image(
                            iced::advanced::image::Image::new(icons::handle(glyph, ICON, color)).filter_method(FilterMethod::Linear),
                            rect(x, y, ICON, ICON),
                            panel,
                        );
                    }
                    if t <= 0.0 {
                        continue;
                    }
                    let mut hint_w = 0.0;
                    if !it.hint.is_empty() {
                        hint_w = paint::text_width(&it.hint, t::T_SMALL, paint::REGULAR) + 12.0;
                        let col = if it.enabled { t::INK3 } else { t::INK4 };
                        paint::text(r, &it.hint, rect(br.x + br.width - hint_w - 8.0, br.y, hint_w, BTN_H), t::T_SMALL, paint::REGULAR, paint::with_alpha(col, fade), HAlign::Right, panel);
                    }
                    let col = if it.enabled { t::INK } else { t::INK4 };
                    let room = (br.width - TEXT_X - hint_w - 8.0).max(0.0);
                    let clip = rect(br.x + TEXT_X, br.y, room, BTN_H).intersection(&panel).unwrap_or_default();
                    let font = if it.checked { paint::SEMIBOLD } else { paint::REGULAR };
                    paint::text(r, it.label, rect(br.x + TEXT_X, br.y, 400.0, BTN_H), t::T_BODY, font, paint::with_alpha(col, fade), HAlign::Left, clip);
                }
            });

            // Tooltip while collapsed
            if t <= 0.5 && now >= st.hover_since + TOOLTIP_DELAY {
                let tip = match st.hover {
                    Hit::Burger => Some("Show menu labels  (Ctrl+\\)"),
                    Hit::Item(i) => match self.entries.get(i) {
                        Some(Entry::Item(it)) => Some(it.tooltip.as_str()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(tip) = tip.filter(|s| !s.is_empty()) {
                    super::grid::draw_tooltip(r, b, st.cursor, tip);
                }
            }
        });
    }
}

impl<'a, C: Copy + 'a, Message: Clone + 'a> From<SideMenu<'a, C, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(m: SideMenu<'a, C, Message>) -> Self {
        Element::new(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spacer_pins_what_follows_to_the_foot() {
        let item = |cmd: u8| Entry::Item(Item { cmd, icon: None, label: "x", hint: String::new(), tooltip: String::new(), enabled: true, checked: false });
        let m: SideMenu<'_, u8, ()> = SideMenu::new(vec![item(1), Entry::Separator, item(2), Entry::Spacer, item(3)], false, |_| (), (), ());
        let (_, rows, _) = m.rows(rect(0.0, 0.0, RAIL_W, 800.0), RAIL_W);
        let last = rows[2].1;
        assert_eq!(last.y + last.height, 800.0 - 6.0);
        // A rail too short for the gap: it follows straight on, never overlapping.
        let (_, rows, _) = m.rows(rect(0.0, 0.0, RAIL_W, 100.0), RAIL_W);
        assert_eq!(rows[2].1.y, rows[1].1.y + BTN_H + ROW_GAP);
    }
}
