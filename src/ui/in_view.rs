//! Which tiles of a wrapped row are on screen, or near it: for pages that
//! draw their tiles' pictures only as they come into view.
//!
//! [`InView`] wraps a `row![…].wrap()` (one layout child per tile) and is
//! given each tile's key in the same order.  On each frame it works out
//! the tiles within one screen above or below the scrollable's viewport,
//! and publishes their keys when that list changes: a scroll that stays
//! within a row publishes nothing.  The work per frame is a pass over the
//! tiles' rectangles.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{self, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::mouse::{self, Cursor};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, window};

use super::paint::Renderer;

/// How far beyond the viewport a tile still counts, in viewport heights.
pub const MARGIN_SCREENS: f32 = 1.0;

pub struct InView<'a, K, Message> {
    content: Element<'a, Message, Theme, Renderer>,
    keys: Vec<K>,
    on_change: Box<dyn Fn(Vec<K>) -> Message + 'a>,
}

impl<'a, K, Message> InView<'a, K, Message> {
    /// `content` is a wrapped row whose children are the tiles of `keys`.
    pub fn new(content: impl Into<Element<'a, Message, Theme, Renderer>>, keys: Vec<K>, on_change: impl Fn(Vec<K>) -> Message + 'a) -> Self {
        InView { content: content.into(), keys, on_change: Box::new(on_change) }
    }
}

/// The keys last published.
struct State<K> {
    last: Option<Vec<K>>,
}

/// The indexes of the rectangles within `margin` of `viewport` (above or
/// below), in order.
pub fn near(tiles: impl Iterator<Item = Rectangle>, viewport: Rectangle, margin: f32) -> Vec<usize> {
    let (top, bottom) = (viewport.y - margin, viewport.y + viewport.height + margin);
    tiles.enumerate().filter(|(_, b)| b.y + b.height >= top && b.y <= bottom).map(|(i, _)| i).collect()
}

impl<K, Message> Widget<Message, Theme, Renderer> for InView<'_, K, Message>
where
    K: Clone + PartialEq + 'static,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<K>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<K> { last: None })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
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

    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>, cursor: Cursor, renderer: &Renderer, clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, Message>, viewport: &Rectangle) {
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            let state = tree.state.downcast_mut::<State<K>>();
            let near = near(layout.children().map(|c| c.bounds()), *viewport, viewport.height * MARGIN_SCREENS);
            let keys: Vec<K> = near.into_iter().filter_map(|i| self.keys.get(i).cloned()).collect();
            if state.last.as_ref() != Some(&keys) {
                state.last = Some(keys.clone());
                shell.publish((self.on_change)(keys));
            }
        }
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle) {
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn widget::Operation) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, layout: Layout<'b>, renderer: &Renderer, viewport: &Rectangle, translation: Vector) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, K, Message> From<InView<'a, K, Message>> for Element<'a, Message, Theme, Renderer>
where
    K: Clone + PartialEq + 'static,
    Message: 'a,
{
    fn from(w: InView<'a, K, Message>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(i: usize, cols: usize) -> Rectangle {
        let (w, h, gap) = (136.0, 170.0, 8.0);
        Rectangle { x: (i % cols) as f32 * (w + gap), y: (i / cols) as f32 * (h + gap), width: w, height: h }
    }

    #[test]
    fn near_takes_the_rows_on_screen_and_a_screen_either_side() {
        let tiles: Vec<Rectangle> = (0..800).map(|i| tile(i, 10)).collect();
        // Scrolled to row 40 of 80, a screen of about five rows.
        let row = 178.0;
        let vp = Rectangle { x: 0.0, y: 40.0 * row, width: 1440.0, height: 5.0 * row };
        let on = near(tiles.iter().copied(), vp, 0.0);
        assert_eq!(on.first(), Some(&400), "the row above ends in the gap over the top");
        assert!(on.iter().all(|&i| (40..=45).contains(&(i / 10))));
        let with_margin = near(tiles.iter().copied(), vp, vp.height);
        assert!(with_margin.len() > on.len() * 2, "a screen above and below too");
        assert!(with_margin.len() <= 17 * 10, "never the whole list: {}", with_margin.len());
        assert!(!with_margin.contains(&0) && !with_margin.contains(&799));
        // In order, without gaps: one band of rows.
        assert!(with_margin.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn near_is_empty_far_away() {
        let tiles: Vec<Rectangle> = (0..20).map(|i| tile(i, 10)).collect();
        let vp = Rectangle { x: 0.0, y: 10_000.0, width: 1440.0, height: 900.0 };
        assert!(near(tiles.into_iter(), vp, 900.0).is_empty());
    }
}
