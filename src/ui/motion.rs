//! Small self-animating widgets.  They ask for their own redraws while they
//! move, so animating never rebuilds the app's view.
//!
//! * [`Appear`] — content settles into place (and an optional scrim fades in)
//!   the first time it is shown: dialogs, menus, popups.
//! * [`ActivityBar`] — a thin sweep along the top edge while background work
//!   runs (scanning, reading the library, making thumbnails).

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{Quad, Renderer as _};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay};
use iced::mouse::{self, Cursor};
use iced::{Color, Element, Event, Length, Rectangle, Size, Vector, window};

use super::paint::{self, Renderer, rect};
use super::theme as t;

// ── Appear ──────────────────────────────────────────────────────────────────

pub struct Appear<'a, Message> {
    content: Element<'a, Message, iced::Theme, Renderer>,
    /// Starting vertical offset: positive rises into place, negative drops.
    rise: f32,
    scrim: Option<Color>,
}

struct AppearState {
    start: Option<Instant>,
}

impl<'a, Message> Appear<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message, iced::Theme, Renderer>>) -> Self {
        Self { content: content.into(), rise: 10.0, scrim: None }
    }

    pub fn rise(mut self, px: f32) -> Self {
        self.rise = px;
        self
    }

    /// Fill the whole area with `color`, fading in with the content.
    pub fn scrim(mut self, color: Color) -> Self {
        self.scrim = Some(color);
        self
    }
}

impl<Message> Widget<Message, iced::Theme, Renderer> for Appear<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<AppearState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(AppearState { start: None })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let st = tree.state.downcast_mut::<AppearState>();
            let start = *st.start.get_or_insert(*now);
            if paint::progress(start, *now, t::MOTION_MS) < 1.0 {
                shell.request_redraw();
            }
        }
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<AppearState>();
        let k = st.start.map(|s| paint::out_cubic(paint::progress(s, Instant::now(), t::MOTION_MS))).unwrap_or(0.0);
        if let Some(scrim) = self.scrim {
            paint::fill(renderer, layout.bounds(), Color { a: scrim.a * k, ..scrim });
        }
        let dy = (1.0 - k) * self.rise;
        renderer.with_translation(Vector::new(0.0, dy), |r| {
            self.content.as_widget().draw(&tree.children[0], r, theme, style, layout, cursor, viewport);
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<Appear<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(a: Appear<'a, Message>) -> Self {
        Element::new(a)
    }
}

// ── ActivityBar ─────────────────────────────────────────────────────────────

/// Fills its area but draws only a 2 px band along the top, and takes no
/// input — lay it over content in a stack.  Work that finishes within
/// `DELAY` never shows it, so quick checks don't flicker.
pub struct ActivityBar {
    active: bool,
}

struct BarState {
    active: bool,
    /// When the bar last switched on or off (it fades both ways).
    changed: Instant,
    /// How visible it was at that moment: a fade starts from there.
    from: f32,
    start: Instant,
}

impl BarState {
    fn level(&self, now: Instant) -> f32 {
        if self.active {
            // Already partly visible (switched back on mid-fade): no delay.
            let delay = if self.from > 0.0 { Duration::ZERO } else { DELAY };
            self.from + (1.0 - self.from) * paint::progress(self.changed + delay, now, t::MOTION_MS)
        } else {
            self.from * (1.0 - paint::progress(self.changed, now, t::MOTION_MS))
        }
    }
}

impl ActivityBar {
    pub fn new(active: bool) -> Self {
        Self { active }
    }
}

const BAR_H: f32 = 2.0;
const SWEEP_S: f32 = 1.15;
const DELAY: Duration = Duration::from_millis(250);

impl<Message> Widget<Message, iced::Theme, Renderer> for ActivityBar {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<BarState>()
    }

    fn state(&self) -> tree::State {
        let now = Instant::now();
        // Starts settled, so a bar that is idle at startup doesn't fade out.
        let from = if self.active { 1.0 } else { 0.0 };
        tree::State::new(BarState { active: self.active, changed: now - Duration::from_secs(1), from, start: now })
    }

    fn diff(&self, tree: &mut Tree) {
        let st = tree.state.downcast_mut::<BarState>();
        if st.active != self.active {
            let now = Instant::now();
            st.from = st.level(now);
            st.active = self.active;
            st.changed = now;
            if self.active && st.from == 0.0 {
                st.start = now + DELAY;
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let st = tree.state.downcast_ref::<BarState>();
            if st.active || st.level(*now) > 0.0 {
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(&self, _t: &Tree, _l: Layout<'_>, _c: Cursor, _v: &Rectangle, _r: &Renderer) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<BarState>();
        let now = Instant::now();
        let level = st.level(now);
        if level <= 0.0 {
            return;
        }
        let b = layout.bounds();
        let band = rect(b.x, b.y, b.width, BAR_H);
        // A purple-to-blue comet runs left to right, easing at each end.
        let phase = (now.saturating_duration_since(st.start).as_secs_f32() % SWEEP_S) / SWEEP_S;
        let x = paint::lerp(-0.4, 1.0, paint::out_quad(phase));
        let w = 0.4;
        let (x0, x1) = ((b.x + x * b.width).max(b.x), (b.x + (x + w) * b.width).min(b.x + b.width));
        if x1 <= x0 {
            return;
        }
        let head = rect(x0, band.y, x1 - x0, BAR_H);
        let gradient = iced::gradient::Linear::new(iced::Radians(std::f32::consts::FRAC_PI_2))
            .add_stop(0.0, t::alpha(t::SIGNAL, 0.0))
            .add_stop(0.55, t::alpha(t::SIGNAL, level))
            .add_stop(1.0, t::alpha(t::FLOW, level));
        r.fill_quad(Quad { bounds: head, ..Quad::default() }, iced::Background::Gradient(iced::Gradient::Linear(gradient)));
    }
}

impl<'a, Message: 'a> From<ActivityBar> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(b: ActivityBar) -> Self {
        Element::new(b)
    }
}

// ── SmoothBar ───────────────────────────────────────────────────────────────

/// A determinate progress bar that glides to each new value instead of
/// jumping: from where it stands it eases to the value over [`GLIDE_MS`],
/// never ahead of it (nothing is extrapolated past what is done; a value
/// that falls, a new run, is taken at once).  It asks for frames only
/// while catching up, then goes idle.  A drop-in for `progress_bar` on
/// 0–1: `length`, `girth`, the ground and bar colours, a 2 px radius.
pub struct SmoothBar {
    value: f32,
    length: Length,
    girth: f32,
    ground: Color,
    bar: Color,
    radius: f32,
}

struct SmoothState {
    from: f32,
    to: f32,
    changed: Instant,
}

/// How long the bar takes to reach a new value.
const GLIDE_MS: f32 = 200.0;

impl SmoothState {
    fn level(&self, now: Instant) -> f32 {
        self.from + (self.to - self.from) * paint::out_cubic(paint::progress(self.changed, now, GLIDE_MS))
    }

    fn settled(&self, now: Instant) -> bool {
        paint::progress(self.changed, now, GLIDE_MS) >= 1.0
    }
}

impl SmoothBar {
    /// `value` is the share done, 0–1.
    pub fn new(value: f32) -> Self {
        Self { value: value.clamp(0.0, 1.0), length: Length::Fill, girth: 4.0, ground: t::RAISED, bar: t::SIGNAL, radius: 2.0 }
    }

    pub fn length(mut self, length: impl Into<Length>) -> Self {
        self.length = length.into();
        self
    }

    pub fn girth(mut self, girth: f32) -> Self {
        self.girth = girth;
        self
    }

    /// The track and the filled part.
    pub fn colors(mut self, ground: Color, bar: Color) -> Self {
        self.ground = ground;
        self.bar = bar;
        self
    }
}

impl<Message> Widget<Message, iced::Theme, Renderer> for SmoothBar {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SmoothState>()
    }

    fn state(&self) -> tree::State {
        // Starts settled on its value: nothing glides at first sight.
        tree::State::new(SmoothState { from: self.value, to: self.value, changed: Instant::now() - Duration::from_secs(1) })
    }

    fn diff(&self, tree: &mut Tree) {
        let st = tree.state.downcast_mut::<SmoothState>();
        if (self.value - st.to).abs() < 1e-4 {
            return;
        }
        let now = Instant::now();
        if self.value < st.to {
            // Back to less (a new run): no glide backwards.
            *st = SmoothState { from: self.value, to: self.value, changed: now - Duration::from_secs(1) };
        } else {
            *st = SmoothState { from: st.level(now), to: self.value, changed: now };
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.length, Length::Fixed(self.girth))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::atomic(limits, self.length, Length::Fixed(self.girth))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let st = tree.state.downcast_ref::<SmoothState>();
            if !st.settled(*now) {
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(&self, _t: &Tree, _l: Layout<'_>, _c: Cursor, _v: &Rectangle, _r: &Renderer) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<SmoothState>();
        let b = layout.bounds();
        paint::rounded(r, b, self.radius, self.ground);
        let w = (b.width * st.level(Instant::now()).clamp(0.0, 1.0)).round();
        if w > 0.0 {
            paint::rounded(r, rect(b.x, b.y, w, b.height), self.radius, self.bar);
        }
    }
}

impl<'a, Message: 'a> From<SmoothBar> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(b: SmoothBar) -> Self {
        Element::new(b)
    }
}
// ── Glow ────────────────────────────────────────────────────────────────────

/// A button with a soft glow that fades in under the pointer and out again
/// (over [`GLOW_MS`]): a 2 px ring in the button's tone just outside its
/// edge, drawn beneath it.  The button itself is untouched: clicks, focus,
/// Tab order, its disabled look and tooltips are its own.  A disabled
/// button never glows.  Frames are asked for only during the fade.
///
/// Until it becomes an element it still takes the button's `width`,
/// `height` and `padding`, so the kit's callers read as before.
pub struct Glow<'a, Message> {
    button: iced::widget::Button<'a, Message>,
    enabled: bool,
    tone: Color,
    radius: f32,
}

/// How long the glow takes to come and go.
const GLOW_MS: f32 = 150.0;

/// `PP_GLOW_ALL=1`: every enabled button glows as if under the pointer,
/// for screenshots from the scripted driver (the headless compositor has
/// no pointer).  Read once.
fn glow_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| std::env::var_os("PP_GLOW_ALL").is_some_and(|v| v == "1"))
}

impl<'a, Message: 'a> Glow<'a, Message> {
    /// `enabled`: the button has an action (only then does it glow).
    pub fn new(button: iced::widget::Button<'a, Message>, enabled: bool) -> Self {
        Self { button, enabled, tone: t::SIGNAL, radius: t::R_CONTROL }
    }

    /// The glow's colour (the accent unless the button has a tone of its own).
    pub fn tone(mut self, tone: Color) -> Self {
        self.tone = tone;
        self
    }

    /// The button's corner radius, so the ring follows it.
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.button = self.button.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.button = self.button.height(height);
        self
    }

    pub fn padding(mut self, padding: impl Into<iced::Padding>) -> Self {
        self.button = self.button.padding(padding);
        self
    }
}

struct GlowWidget<'a, Message> {
    content: Element<'a, Message, iced::Theme, Renderer>,
    enabled: bool,
    tone: Color,
    radius: f32,
}

struct GlowState {
    over: bool,
    /// How lit it was when the pointer last came or went: the fade starts there.
    from: f32,
    changed: Instant,
}

impl GlowState {
    fn level(&self, now: Instant) -> f32 {
        let k = paint::out_cubic(paint::progress(self.changed, now, GLOW_MS));
        if self.over { self.from + (1.0 - self.from) * k } else { self.from * (1.0 - k) }
    }

    fn settled(&self, now: Instant) -> bool {
        paint::progress(self.changed, now, GLOW_MS) >= 1.0
    }
}

impl<'a, Message: 'a> Widget<Message, iced::Theme, Renderer> for GlowWidget<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<GlowState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(GlowState { over: false, from: 0.0, changed: Instant::now() - Duration::from_secs(1) })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
        let st = tree.state.downcast_mut::<GlowState>();
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                if !st.settled(*now) {
                    shell.request_redraw();
                }
            }
            _ => {
                let over = self.enabled && (cursor.is_over(layout.bounds()) || glow_forced());
                if over != st.over {
                    let now = Instant::now();
                    st.from = st.level(now);
                    st.over = over;
                    st.changed = now;
                    shell.request_redraw();
                }
            }
        }
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<GlowState>();
        let k = st.level(Instant::now());
        if k > 0.005 {
            let b = layout.bounds();
            let ring = rect(b.x - 2.0, b.y - 2.0, b.width + 4.0, b.height + 4.0);
            paint::framed(renderer, ring, self.radius + 2.0, Color::TRANSPARENT, t::alpha(self.tone, 0.32 * k), 2.0);
        }
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'a> From<Glow<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(g: Glow<'a, Message>) -> Self {
        Element::new(GlowWidget { content: g.button.into(), enabled: g.enabled, tone: g.tone, radius: g.radius })
    }
}

