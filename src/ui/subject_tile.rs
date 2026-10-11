//! The pieces people, fursuits and pets are drawn with: a crop in its
//! shape (round for a person's face; a rounded square for a fursuit's head
//! or a pet, so ears and horns fit), a group's tile (the crop, its name or
//! "Unnamed", how many photos), the review's stamps (the keep check, the
//! grease-pencil cross), and a whole photo with the face outlined.
//!
//! Everything sits on the photos' neutral greys; the kind is told by the
//! tab and the shape, never by colour.  Crops come decoded from the page's
//! cache; until one arrives its place is a quiet grey shape.

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{self, Cursor};
use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{Space, button, column, container, image, stack, text};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Event, Length, Padding, Point, Rectangle, Size, Theme};

use super::icons::{self, Glyph};
use super::paint::{self, Renderer, rect};
use super::theme as t;

/// A crop's outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A person's face.
    Round,
    /// A fursuit's head or a pet: room for ears.
    Rounded,
}

impl Shape {
    pub fn radius(self, size: f32) -> f32 {
        match self {
            Shape::Round => size / 2.0,
            Shape::Rounded => (size * 0.16).round(),
        }
    }
}

/// A crop `size` square in its shape, or its grey place while it decodes.
pub fn crop<'a, M: 'a>(handle: Option<Handle>, size: f32, shape: Shape) -> Element<'a, M> {
    let r = shape.radius(size);
    match handle {
        Some(h) => image(h).width(size).height(size).content_fit(ContentFit::Cover).filter_method(FilterMethod::Linear).border_radius(r).into(),
        None => container(Space::new())
            .width(size)
            .height(size)
            .style(move |_| container::Style { background: Some(Background::Color(t::PHOTO_TRAY)), border: Border { radius: r.into(), ..Border::default() }, ..Default::default() })
            .into(),
    }
}

/// A group's tile: its crop, its name (or "Unnamed", quieter), and how
/// many photos.  `picked` rings it in the accent (picked to merge or hide,
/// or the keyboard is on it).
pub struct Tile<'a, M> {
    pub crop: Element<'a, M>,
    pub name: Option<String>,
    pub count: String,
    pub picked: bool,
    /// Small: the folded-away groups and the hidden ones (no name line
    /// unless one is given).
    pub small: bool,
    /// A glyph in the corner (the hidden ones' crossed-out eye).
    pub badge: Option<Glyph>,
    pub on: M,
    /// The glyph's own click, when it has one (the split mark: the
    /// group editor).
    pub badge_on: Option<M>,
}

/// The tile's width: a large one, a small one.
pub const TILE_W: f32 = 136.0;
pub const SMALL_W: f32 = 96.0;
pub const TILE_CROP: f32 = 100.0;
pub const SMALL_CROP: f32 = 64.0;

impl<'a, M: Clone + 'a> From<Tile<'a, M>> for Element<'a, M> {
    fn from(tile: Tile<'a, M>) -> Self {
        let (w, pad) = if tile.small { (SMALL_W, Padding::from([8.0, 6.0])) } else { (TILE_W, Padding { top: 12.0, bottom: 10.0, left: 8.0, right: 8.0 }) };
        let mut col = column![tile.crop].align_x(Alignment::Center).spacing(if tile.small { 5 } else { 7 }).width(Length::Fill);
        match (&tile.name, tile.small) {
            (Some(n), _) => col = col.push(text(n.clone()).size(if tile.small { t::T_SMALL } else { t::T_BODY }).font(paint::MEDIUM).color(t::PHOTO_INK).wrapping(text::Wrapping::None)),
            (None, false) => col = col.push(text("Unnamed").size(t::T_BODY).color(t::PHOTO_INK3)),
            (None, true) => {}
        }
        col = col.push(text(tile.count).size(t::T_SMALL).color(t::PHOTO_INK3));
        let picked = tile.picked;
        let b = button(col).width(w).padding(pad).on_press(tile.on).style(move |_t: &Theme, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if hot { t::PHOTO_RAISED } else { t::PHOTO_MOUNT })),
                border: Border { color: if picked { t::SIGNAL } else { Color::TRANSPARENT }, width: if picked { 2.0 } else { 0.0 }, radius: t::R_CELL.into() },
                text_color: t::PHOTO_INK,
                ..button::Style::default()
            }
        });
        match (tile.badge, tile.badge_on) {
            (Some(g), Some(on)) => {
                let mark = iced::widget::mouse_area(container(icons::view(g, 14.0, t::PHOTO_INK3)).padding(3)).on_press(on).interaction(iced::mouse::Interaction::Pointer);
                stack![b, container(mark).width(w).align_x(iced::alignment::Horizontal::Right).padding(Padding { top: 5.0, right: 5.0, ..Padding::ZERO })].into()
            }
            (Some(g), None) => stack![b, container(icons::view(g, 14.0, t::PHOTO_INK3)).width(w).align_x(iced::alignment::Horizontal::Right).padding(Padding { top: 8.0, right: 8.0, ..Padding::ZERO })].into(),
            (None, _) => b.into(),
        }
    }
}

/// An answer drawn on a crop as marks are drawn on a photo: Yes, the keep
/// check on its corner; No, the grease-pencil cross with the crop dimmed.
pub fn stamped<'a, M: 'a>(content: Element<'a, M>, answer: Option<bool>, size: f32, shape: Shape) -> Element<'a, M> {
    match answer {
        None => content,
        Some(yes) => stack![content, Element::new(Mark { yes, shape })].width(size).height(size).into(),
    }
}

/// The overlay of [`stamped`].
struct Mark {
    yes: bool,
    shape: Shape,
}

impl<M> Widget<M, Theme, Renderer> for Mark {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _tree: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn mouse_interaction(&self, _t: &widget::Tree, _l: Layout<'_>, _c: Cursor, _v: &Rectangle, _r: &Renderer) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn draw(&self, _tree: &widget::Tree, r: &mut Renderer, _theme: &Theme, _style: &iced::advanced::renderer::Style, layout: Layout<'_>, _cursor: Cursor, _viewport: &Rectangle) {
        let b = layout.bounds();
        let s = b.width.min(b.height);
        // A wide margin so the badge may sit over the corner.
        let room = Rectangle { x: b.x - s * 0.2, y: b.y - s * 0.2, width: b.width + s * 0.4, height: b.height + s * 0.4 };
        r.with_layer(room, |r| {
            if self.yes {
                // The keep check on the upper right corner.
                let d = (s * 0.32).clamp(16.0, 44.0);
                let inset = match self.shape {
                    Shape::Round => s * 0.08,
                    Shape::Rounded => -d * 0.25,
                };
                let c = Point::new(b.x + b.width - inset - d / 2.0, b.y + inset + d / 2.0);
                paint::circle(r, Point::new(c.x, c.y + 1.5), d / 2.0, t::alpha(Color::BLACK, 0.35), Color::TRANSPARENT, 0.0);
                paint::circle(r, c, d / 2.0, t::KEEP, Color::TRANSPARENT, 0.0);
                let g = d * 0.62;
                r.draw_image(iced::advanced::image::Image::new(icons::handle(Glyph::Check, g, t::ON_SIGNAL)).filter_method(FilterMethod::Linear), rect(c.x - g / 2.0, c.y - g / 2.0, g, g), room);
            } else {
                // Dimmed, and crossed corner to corner as a grease pencil
                // crosses a contact sheet.
                paint::rounded(r, b, self.shape.radius(s), t::alpha(t::PHOTO_GROUND, 0.45));
                let m = s * 0.14;
                let (a, z) = (Point::new(b.x + m, b.y + m), Point::new(b.x + b.width - m, b.y + b.height - m));
                let (c, d) = (Point::new(b.x + b.width - m, b.y + m), Point::new(b.x + m, b.y + b.height - m));
                let w = (s * 0.035).clamp(2.0, 5.0);
                paint::strokes(r, &[(a, z), (c, d)], w, t::REJECT, true);
            }
        });
    }
}

/// A whole photo fitted in its box, with one face's box outlined: a light
/// line on a dark halo, never the accent (which means selection).
pub struct PhotoBox {
    pub handle: Option<Handle>,
    /// The photo's size in pixels (its aspect).
    pub size: (u32, u32),
    /// The face, as fractions of the photo.
    pub bbox: [f32; 4],
    pub width: f32,
    pub height: f32,
}

impl PhotoBox {
    /// Where the photo sits in `b`, fitted whole.
    fn fit(&self, b: Rectangle) -> Rectangle {
        let (w, h) = (self.size.0.max(1) as f32, self.size.1.max(1) as f32);
        let k = (b.width / w).min(b.height / h);
        let (fw, fh) = (w * k, h * k);
        rect((b.x + (b.width - fw) / 2.0).round(), (b.y + (b.height - fh) / 2.0).round(), fw.round(), fh.round())
    }
}

impl<M> Widget<M, Theme, Renderer> for PhotoBox {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fixed(self.height))
    }

    fn layout(&mut self, _tree: &mut widget::Tree, _r: &Renderer, _limits: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::new(self.width, self.height))
    }

    fn update(&mut self, _tree: &mut widget::Tree, _event: &Event, _layout: Layout<'_>, _cursor: Cursor, _renderer: &Renderer, _clipboard: &mut dyn Clipboard, _shell: &mut Shell<'_, M>, _viewport: &Rectangle) {}

    fn draw(&self, _tree: &widget::Tree, r: &mut Renderer, _theme: &Theme, _style: &iced::advanced::renderer::Style, layout: Layout<'_>, _cursor: Cursor, _viewport: &Rectangle) {
        let b = layout.bounds();
        let Some(h) = &self.handle else {
            paint::rounded(r, b, 2.0, t::PHOTO_TRAY);
            return;
        };
        let ir = self.fit(b);
        r.draw_image(iced::advanced::image::Image::new(h.clone()).filter_method(FilterMethod::Linear), ir, b);
        let [x, y, w, hh] = self.bbox;
        let fb = rect(ir.x + x * ir.width, ir.y + y * ir.height, w * ir.width, hh * ir.height);
        r.with_layer(b, |r| {
            let halo = Rectangle { x: fb.x - 1.0, y: fb.y - 1.0, width: fb.width + 2.0, height: fb.height + 2.0 };
            paint::framed(r, halo, 4.0, Color::TRANSPARENT, t::alpha(Color::BLACK, 0.55), 3.5);
            paint::framed(r, fb, 3.0, Color::TRANSPARENT, t::alpha(t::PHOTO_INK, 0.95), 1.6);
        });
    }
}

impl<'a, M: 'a> From<PhotoBox> for Element<'a, M> {
    fn from(p: PhotoBox) -> Self {
        Element::new(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_is_round_and_a_head_a_rounded_square() {
        assert_eq!(Shape::Round.radius(100.0), 50.0);
        assert!(Shape::Rounded.radius(100.0) < 20.0);
    }

    #[test]
    fn the_photo_is_fitted_whole_and_centred() {
        let p = PhotoBox { handle: None, size: (300, 200), bbox: [0.0; 4], width: 340.0, height: 340.0 };
        let r = p.fit(rect(0.0, 0.0, 340.0, 340.0));
        assert_eq!((r.width, r.height), (340.0, 227.0));
        assert_eq!(r.y, 57.0);
    }
}
