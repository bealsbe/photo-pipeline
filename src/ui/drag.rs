//! Dragging one tile onto another (the subjects pages' merge by drag).
//!
//! [`DragArea`] wraps a tile and leaves its clicks alone: a press becomes a
//! drag only once the pointer has moved [`THRESHOLD`] pixels.  The tile
//! being dragged draws its crop under the pointer itself (no message per
//! move, so the app's view isn't rebuilt while the pointer travels); the
//! page hears only where a drag starts, which tile the pointer enters or
//! leaves while dragging, and the drop.  A release over the dragged tile's
//! own place cancels quietly and never counts as a click.

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay};
use iced::mouse::{self, Cursor};
use iced::widget::image::{FilterMethod, Handle};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

use super::paint::{self, Renderer, rect};
use super::theme as t;

/// Pixels the pointer moves with the button down before a press is a drag.
pub const THRESHOLD: f32 = 6.0;
/// The crop drawn under the pointer while dragging.
const GHOST: f32 = 64.0;

/// What a drag area tells the page.
pub struct DragMessages<M> {
    /// The pointer moved far enough with the button down on this tile.
    pub start: M,
    /// While something is dragged, the pointer came over this tile.
    pub enter: M,
    /// ... and left it.
    pub leave: M,
    /// The button came up after this tile was dragged.
    pub drop: M,
}

pub struct DragArea<'a, M> {
    content: Element<'a, M, iced::Theme, Renderer>,
    messages: DragMessages<M>,
    /// A drag is under way somewhere on the page (enter and leave are only
    /// said then).
    dragging: bool,
    /// The tile's crop, drawn under the pointer while it is dragged.
    ghost: Option<Handle>,
    radius: f32,
}

impl<'a, M> DragArea<'a, M> {
    pub fn new(content: impl Into<Element<'a, M, iced::Theme, Renderer>>, messages: DragMessages<M>, dragging: bool) -> Self {
        DragArea { content: content.into(), messages, dragging, ghost: None, radius: 0.0 }
    }

    /// The crop to draw under the pointer, and its corner radius at
    /// [`GHOST`] pixels.
    pub fn ghost(mut self, handle: Option<Handle>, radius: f32) -> Self {
        self.ghost = handle;
        self.radius = radius;
        self
    }
}

#[derive(Debug, Default)]
struct State {
    /// Where the button went down on this tile (content coordinates).
    pressed: Option<Point>,
    /// This tile is being dragged.
    dragging: bool,
    /// The pointer is over this tile while something is dragged.
    inside: bool,
    /// Window position minus content position: scrolling moves one and
    /// not the other, and the pointer may leave the scrolled area.
    offset: Vector,
    /// The pointer, in window coordinates.
    at: Option<Point>,
}

/// What a tile says to the page after a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    Start,
    Enter,
    Leave,
}

impl State {
    /// The button went down (`over`: where, when on this tile).
    fn press(&mut self, over: Option<Point>) {
        self.dragging = false;
        self.pressed = over;
    }

    /// The pointer moved to `window` (`content`: the same point in the
    /// tile's coordinates, when the page can say; `over`: it is on this
    /// tile); `page_dragging`: a drag is under way on the page.
    fn moved(&mut self, window: Point, content: Option<Point>, over: bool, page_dragging: bool) -> Vec<Said> {
        let mut said = Vec::new();
        if let Some(p) = content {
            self.offset = window - p;
        }
        self.at = Some(window);
        if let (Some(from), false) = (self.pressed, self.dragging) {
            if from.distance(window - self.offset) >= THRESHOLD {
                self.dragging = true;
                said.push(Said::Start);
            }
        }
        if page_dragging && over != self.inside {
            self.inside = over;
            said.push(if over { Said::Enter } else { Said::Leave });
        } else if !page_dragging {
            self.inside = false;
        }
        said
    }

    /// The button came up: whether this tile was being dragged (a drop,
    /// never a click).
    fn release(&mut self) -> bool {
        self.pressed = None;
        self.inside = false;
        std::mem::take(&mut self.dragging)
    }
}

impl<M: Clone> Widget<M, iced::Theme, Renderer> for DragArea<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => st.press(cursor.position_over(bounds)),
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                for said in st.moved(*position, cursor.position(), cursor.is_over(bounds), self.dragging) {
                    shell.publish(match said {
                        Said::Start => self.messages.start.clone(),
                        Said::Enter => self.messages.enter.clone(),
                        Said::Leave => self.messages.leave.clone(),
                    });
                }
                if st.dragging {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if st.release() {
                    shell.publish(self.messages.drop.clone());
                    shell.request_redraw();
                    // The tile's own button lets go without a click.
                    self.content.as_widget_mut().update(&mut tree.children[0], event, layout, Cursor::Unavailable, renderer, clipboard, shell, viewport);
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        if st.dragging || (self.dragging && cursor.is_over(layout.bounds())) {
            return mouse::Interaction::Grabbing;
        }
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &iced::Theme, style: &iced::advanced::renderer::Style, layout: Layout<'_>, cursor: Cursor, viewport: &Rectangle) {
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        let st = tree.state.downcast_ref::<State>();
        let (true, Some(at)) = (st.dragging, st.at) else { return };
        // Under the pointer, a little below and right of it, in its own
        // layer so the page's scrolling doesn't clip it.
        let here = at - st.offset;
        let g = rect(here.x + 10.0, here.y + 8.0, GHOST, GHOST);
        let room = rect(g.x - 8.0, g.y - 8.0, GHOST + 16.0, GHOST + 16.0);
        renderer.with_layer(room, |r| {
            paint::rounded(r, rect(g.x - 3.0, g.y - 1.0, GHOST + 6.0, GHOST + 6.0), self.radius + 3.0, t::alpha(t::PHOTO_GROUND, 0.55));
            match &self.ghost {
                Some(h) => {
                    let mut img = iced::advanced::image::Image::new(h.clone()).filter_method(FilterMethod::Linear).opacity(0.92_f32);
                    img.border_radius = self.radius.into();
                    r.draw_image(img, g, room);
                }
                None => paint::rounded(r, g, self.radius, t::PHOTO_TRAY),
            }
            paint::framed(r, g, self.radius, iced::Color::TRANSPARENT, t::SIGNAL, 2.0);
        });
    }

    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, layout: Layout<'b>, renderer: &Renderer, viewport: &Rectangle, translation: Vector) -> Option<overlay::Element<'b, M, iced::Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, M: Clone + 'a> From<DragArea<'a, M>> for Element<'a, M, iced::Theme, Renderer> {
    fn from(d: DragArea<'a, M>) -> Self {
        Element::new(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    /// A click stays a click: a press and release without moving says
    /// nothing, and a wobble under the threshold doesn't start a drag.
    #[test]
    fn a_click_is_never_a_drag() {
        let mut s = State::default();
        s.press(Some(p(10.0, 10.0)));
        assert!(s.moved(p(13.0, 12.0), Some(p(13.0, 12.0)), true, false).is_empty(), "under the threshold");
        assert!(!s.release(), "no drop: the button's own click stands");
        assert!(s.moved(p(80.0, 80.0), Some(p(80.0, 80.0)), false, false).is_empty(), "moving after the release starts nothing");
        // A press elsewhere (not on this tile) never drags it.
        s.press(None);
        assert!(s.moved(p(200.0, 80.0), Some(p(200.0, 80.0)), false, false).is_empty());
    }

    /// Past the threshold the pressed tile starts a drag once; tiles the
    /// pointer then crosses say enter and leave; the release drops, said
    /// by the dragged tile alone.
    #[test]
    fn a_drag_starts_once_crosses_tiles_and_drops() {
        let (mut from, mut onto) = (State::default(), State::default());
        from.press(Some(p(10.0, 10.0)));
        onto.press(None);
        assert_eq!(from.moved(p(20.0, 10.0), Some(p(20.0, 10.0)), true, false), [Said::Start]);
        assert_eq!(from.moved(p(30.0, 10.0), Some(p(30.0, 10.0)), true, true), [Said::Enter], "the page now drags; the source is under the pointer");
        assert!(onto.moved(p(30.0, 10.0), Some(p(30.0, 10.0)), false, true).is_empty());
        assert_eq!(from.moved(p(200.0, 10.0), Some(p(200.0, 10.0)), false, true), [Said::Leave]);
        assert_eq!(onto.moved(p(200.0, 10.0), Some(p(200.0, 10.0)), true, true), [Said::Enter]);
        assert!(onto.moved(p(205.0, 12.0), Some(p(205.0, 12.0)), true, true).is_empty(), "said once while inside");
        assert!(!onto.release(), "the target never drops");
        assert!(from.release(), "the dragged tile drops");
        assert!(!from.release(), "once");
    }

    /// Scrolling: the pointer's window position and the tile's own
    /// coordinates differ by the scroll; the threshold is measured in the
    /// tile's, and a pointer outside the scrolled area still moves the
    /// drag on.
    #[test]
    fn the_threshold_counts_in_the_tiles_own_coordinates() {
        let mut s = State::default();
        // Scrolled by 500: window y 100 is content y 600.
        s.press(Some(p(10.0, 600.0)));
        assert!(s.moved(p(12.0, 100.0), Some(p(12.0, 600.0)), true, false).is_empty());
        assert_eq!(s.moved(p(12.0, 110.0), None, false, false), [Said::Start], "outside the scrolled area, the last offset holds");
    }
}
