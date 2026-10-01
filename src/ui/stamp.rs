//! A short-lived note over the photo ("Marked to prune"), shown when the
//! viewer moves on by itself after a mark, so the decision stays visible
//! while the next photo slides in.  It fades in, holds, fades out and asks
//! for its own redraws meanwhile; it never takes input.

use std::time::Instant;

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{self, Cursor};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Rectangle, Size, window};

use super::icons::{self, Glyph};
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;

const IN_MS: f32 = t::MOTION_FAST_MS;
const HOLD_MS: f32 = 800.0;
const OUT_MS: f32 = t::MOTION_MS * 2.0;

/// 0–1 opacity `ms` after the stamp was set; `None` once it has gone.
pub fn opacity(ms: f32) -> Option<f32> {
    if !(0.0..IN_MS + HOLD_MS + OUT_MS).contains(&ms) {
        return None;
    }
    Some(if ms < IN_MS {
        paint::out_cubic(ms / IN_MS)
    } else if ms < IN_MS + HOLD_MS {
        1.0
    } else {
        1.0 - paint::out_quad((ms - IN_MS - HOLD_MS) / OUT_MS)
    })
}

pub struct Stamp {
    since: Option<Instant>,
    glyph: Glyph,
    label: String,
    tone: Color,
}

impl Stamp {
    /// A stamp set at `since` (`None`: nothing to show).
    pub fn new(since: Option<Instant>, glyph: Glyph, label: impl Into<String>, tone: Color) -> Self {
        Self { since, glyph, label: label.into(), tone }
    }

    fn alpha(&self, now: Instant) -> Option<f32> {
        opacity(now.saturating_duration_since(self.since?).as_secs_f32() * 1000.0)
    }
}

impl<Message> Widget<Message, iced::Theme, Renderer> for Stamp {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _tree: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        _tree: &mut widget::Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            if self.alpha(Instant::now()).is_some() {
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(&self, _t: &widget::Tree, _l: Layout<'_>, _c: Cursor, _v: &Rectangle, _r: &Renderer) -> mouse::Interaction {
        // Never in the way of the photo under it.
        mouse::Interaction::None
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        r: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let Some(a) = self.alpha(Instant::now()) else { return };
        let b = layout.bounds();
        let c = |col: Color| paint::with_alpha(col, a);
        let text_w = paint::text_width(&self.label, t::T_BODY, paint::MEDIUM);
        let (h, icon) = (36.0, 18.0);
        let w = (t::S4 + icon + t::S2 + text_w + t::S4 + 2.0).ceil();
        // Low in the frame, rising a few pixels as it appears.
        let rise = (1.0 - a) * 6.0;
        let pill = rect((b.center_x() - w / 2.0).round(), (b.y + b.height - h - t::S5 + rise).round(), w, h);
        r.with_layer(b, |r| {
            paint::rounded(r, rect(pill.x, pill.y + 2.0, pill.width, pill.height), h / 2.0, c(t::alpha(Color::BLACK, 0.3)));
            paint::framed(r, pill, h / 2.0, c(t::alpha(t::PHOTO_MOUNT, 0.94)), c(t::PHOTO_LINE), 1.0);
            r.draw_image(
                iced::advanced::image::Image::new(icons::handle(self.glyph, icon, self.tone)).filter_method(FilterMethod::Linear).opacity(a),
                rect(pill.x + t::S4, pill.y + (h - icon) / 2.0, icon, icon),
                b,
            );
            let text_rect = rect(pill.x + t::S4 + icon + t::S2, pill.y, text_w + 2.0, h);
            paint::text(r, &self.label, text_rect, t::T_BODY, paint::MEDIUM, c(t::PHOTO_INK), HAlign::Left, b);
        });
    }
}

impl<'a, Message: 'a> From<Stamp> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(s: Stamp) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_in_holds_and_goes() {
        assert_eq!(opacity(-1.0), None);
        assert_eq!(opacity(0.0), Some(0.0));
        assert_eq!(opacity(IN_MS + 1.0), Some(1.0));
        assert_eq!(opacity(IN_MS + HOLD_MS - 1.0), Some(1.0));
        let out = opacity(IN_MS + HOLD_MS + OUT_MS / 2.0).unwrap();
        assert!(out > 0.0 && out < 1.0);
        assert_eq!(opacity(IN_MS + HOLD_MS + OUT_MS + 1.0), None);
    }
}
