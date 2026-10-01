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

fn progress(start: Instant, now: Instant, ms: f32) -> f32 {
    (now.saturating_duration_since(start).as_secs_f32() * 1000.0 / ms).clamp(0.0, 1.0)
}

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
            if progress(start, *now, t::MOTION_MS) < 1.0 {
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
        let k = st.start.map(|s| paint::out_cubic(progress(s, Instant::now(), t::MOTION_MS))).unwrap_or(0.0);
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
            self.from + (1.0 - self.from) * progress(self.changed + delay, now, t::MOTION_MS)
        } else {
            self.from * (1.0 - progress(self.changed, now, t::MOTION_MS))
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

