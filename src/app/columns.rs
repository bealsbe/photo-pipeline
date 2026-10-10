//! Two columns of faces (or fursuit heads) to move between, by drag or
//! key: the one piece behind two groups side by side (`subjects_pair`,
//! where a move is the engine's) and a split being arranged
//! (`subjects_split`, where nothing moves until it is confirmed).
//!
//! - A click picks a face (or puts it back); Shift+click picks the run from
//!   the last one clicked, in the order shown.  Picks live on one side.
//! - A face dragged (the picked ones with it when it is one of them) onto
//!   the other column, or onto *New group* where the owner offers one.
//! - ← / → send the picked faces to that side; N to a new group.
//! - **Pinned** faces never move: a click or a drag on one does nothing,
//!   and a move never carries one (a split's two heads from one photo).
//!
//! The owner keeps the faces and the order; this keeps the picks and the
//! drag, and hands back a [`Move`] for the owner to make.

use std::collections::HashSet;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, column, container, row, scrollable, stack, text};
use iced::{Alignment, Background, Element, Length, Padding};

use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, Kind, KindWords};
use super::style;
use super::subjects as subj;
use super::{App, Message};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile::{self as tile, Tile};
use crate::ui::theme::{self as t, alpha};
use crate::util;

/// Where a drag or a key sends the picked faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Side(usize),
    New,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub from: usize,
    pub faces: Vec<FaceRef>,
    pub over: Option<Target>,
}

/// Faces to move: what the owner does with a drop or a key.
#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub from: usize,
    pub faces: Vec<FaceRef>,
    pub to: Target,
}

#[derive(Debug, Clone)]
pub enum ColMsg {
    Pick(usize, FaceRef),
    MoveTo(Target),
    DragStart(usize, FaceRef),
    DragEnter(Target),
    DragLeave(Target),
    DragDrop,
    Nothing,
}

/// The picks and the drag of two columns.
#[derive(Debug, Clone, Default)]
pub struct Columns {
    pub picked: [Vec<FaceRef>; 2],
    anchor: [Option<FaceRef>; 2],
    pub drag: Option<Drag>,
    /// Faces that never move.
    pub pinned: HashSet<FaceRef>,
    /// Whether *New group* is offered (N, the drop target).
    pub new_group: bool,
}

impl Columns {
    pub fn new(new_group: bool) -> Columns {
        Columns { new_group, ..Columns::default() }
    }

    /// A message, with each side's faces in the order shown; a move to
    /// make, if any.  `shift`: a click picks a run.
    pub fn update(&mut self, msg: ColMsg, shift: bool, order: impl Fn(usize) -> Vec<FaceRef>) -> Option<Move> {
        match msg {
            ColMsg::Pick(side, f) => {
                self.pick(side, f, shift, &order(side));
                None
            }
            ColMsg::MoveTo(to) => {
                if to == Target::New && !self.new_group {
                    return None;
                }
                let from = match to {
                    Target::Side(s) => 1 - s.min(1),
                    Target::New => usize::from(self.picked[0].is_empty()),
                };
                self.take(from, self.picked[from].clone(), to)
            }
            ColMsg::DragStart(side, f) => {
                if self.pinned.contains(&f) {
                    return None;
                }
                let faces = if self.picked[side].contains(&f) { self.picked[side].clone() } else { vec![f] };
                self.drag = Some(Drag { from: side, faces, over: None });
                None
            }
            ColMsg::DragEnter(to) => {
                if let Some(d) = self.drag.as_mut() {
                    d.over = Some(to);
                }
                None
            }
            ColMsg::DragLeave(to) => {
                if let Some(d) = self.drag.as_mut().filter(|d| d.over == Some(to)) {
                    d.over = None;
                }
                None
            }
            ColMsg::DragDrop => {
                let Drag { from, faces, over } = self.drag.take()?;
                let to = over?;
                if to == Target::Side(from) || (to == Target::New && !self.new_group) {
                    return None;
                }
                self.take(from, faces, to)
            }
            ColMsg::Nothing => None,
        }
    }

    /// A click picks or puts back; with Shift, every face from the last one
    /// clicked on that side to this one, in `order`.  Pinned faces are
    /// never picked.
    pub fn pick(&mut self, side: usize, f: FaceRef, shift: bool, order: &[FaceRef]) {
        let side = side.min(1);
        if self.pinned.contains(&f) {
            return;
        }
        self.picked[1 - side].clear();
        self.anchor[1 - side] = None;
        if shift {
            if let Some(run) = self.anchor[side].and_then(|a| super::subjects_split::run(order, a, f)) {
                for x in run.into_iter().filter(|x| !self.pinned.contains(x)) {
                    if !self.picked[side].contains(&x) {
                        self.picked[side].push(x);
                    }
                }
                self.anchor[side] = Some(f);
                return;
            }
        }
        match self.picked[side].iter().position(|x| *x == f) {
            Some(i) => {
                self.picked[side].remove(i);
            }
            None => self.picked[side].push(f),
        }
        self.anchor[side] = Some(f);
    }

    /// The move of `faces` from `from`, without the pinned ones; the
    /// picks of that side put away.
    fn take(&mut self, from: usize, faces: Vec<FaceRef>, to: Target) -> Option<Move> {
        let faces: Vec<FaceRef> = faces.into_iter().filter(|f| !self.pinned.contains(f)).collect();
        if faces.is_empty() {
            return None;
        }
        self.picked[from].retain(|f| !faces.contains(f));
        self.anchor[from] = None;
        Some(Move { from, faces, to })
    }

    /// Esc: a drag, then the picks.  Whether there was one.
    pub fn escape(&mut self) -> bool {
        if self.drag.take().is_some() {
            return true;
        }
        if self.picked.iter().any(|p| !p.is_empty()) {
            self.picked = [Vec::new(), Vec::new()];
            self.anchor = [None, None];
            return true;
        }
        false
    }

    /// Picks of faces no longer on their side are put away.
    pub fn keep_only(&mut self, side: usize, on_side: impl Fn(&FaceRef) -> bool) {
        self.picked[side].retain(|f| on_side(f));
        if self.anchor[side].is_some_and(|a| !on_side(&a)) {
            self.anchor[side] = None;
        }
    }

    pub fn clear(&mut self, side: usize) {
        self.picked[side].clear();
        self.anchor[side] = None;
    }

    pub fn picked_side(&self) -> Option<usize> {
        (0..2).find(|&s| !self.picked[s].is_empty())
    }
}

/// ← / → (and N when offered): the message, if the key is one.
pub fn key_msg(key: &Key, mods: keyboard::Modifiers, new_group: bool) -> Option<ColMsg> {
    if mods.control() || mods.alt() {
        return None;
    }
    match key {
        Key::Named(Named::ArrowLeft) => Some(ColMsg::MoveTo(Target::Side(0))),
        Key::Named(Named::ArrowRight) => Some(ColMsg::MoveTo(Target::Side(1))),
        Key::Character(c) if new_group && c.as_str().eq_ignore_ascii_case("n") => Some(ColMsg::MoveTo(Target::New)),
        _ => None,
    }
}

// ── views ───────────────────────────────────────────────────────────────────

/// One crop's word: "head" for fursuits, "face" for people.
pub fn crop_word(kind: Kind) -> &'static str {
    if kind == Kind::Fursuit { "head" } else { "face" }
}

/// What the views need from their owner.
pub struct Look<'a> {
    pub kind: Kind,
    pub cols: &'a Columns,
    /// The owner's message for a column message.
    pub wrap: fn(ColMsg) -> Message,
    /// The crops loaded for what is on screen: each side's section.
    pub sections: [subj::Section; 2],
}

/// One side's faces as tiles: the first `marked` are the least like the
/// rest; pinned ones carry the pin.  Only the tiles on screen (or near)
/// load their crops (`InView`).
pub fn grid<'a>(app: &'a App, look: &Look<'a>, side: usize, order: &[&'a Face], marked: usize) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(look.kind);
    let small = subj::px(app, tile::SMALL_CROP);
    let dragging = look.cols.drag.is_some();
    let wrap = look.wrap;
    let mut r = row![].spacing(8);
    let mut keys = Vec::with_capacity(order.len());
    for (i, f) in order.iter().enumerate() {
        keys.push(subj::Crops::key(&f.face, small, Framing::Face));
        let crop = tile::crop(s.crops.get(&f.face, small, Framing::Face), tile::SMALL_CROP, shape);
        let pinned = look.cols.pinned.contains(&f.face);
        let badge = if pinned { Some(Glyph::Pin) } else { (i < marked).then_some(Glyph::Split) };
        let el: Element<'a, Message> = Tile { crop, name: None, count: if pinned { "Pinned".to_string() } else { f.day.format("%b %-d, %Y").to_string() }, picked: look.cols.picked[side].contains(&f.face), small: true, badge, on: wrap(ColMsg::Pick(side, f.face)) }.into();
        let el: Element<'a, Message> = if pinned {
            el
        } else {
            let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::DragStart(side, f.face)), enter: wrap(ColMsg::Nothing), leave: wrap(ColMsg::Nothing), drop: wrap(ColMsg::DragDrop) };
            crate::ui::drag::DragArea::new(el, messages, dragging).into()
        };
        let words = if pinned {
            format!("{}. In one photo with the other side's pinned {}: the two stay apart", super::search::long_date(f.day), crop_word(look.kind))
        } else {
            format!("{}{}", super::search::long_date(f.day), if i < marked { ". Among the least like the rest" } else { "" })
        };
        r = r.push(tip(el, words, true));
    }
    let section = look.sections[side.min(1)];
    crate::ui::in_view::InView::new(r.wrap().vertical_spacing(8), keys, move |keys| subj::m(subj::Msg::InView(section, keys))).into()
}

/// A side's column: `head` over its tiles, scrolling; the whole of it
/// takes a drop from the other side, outlined while one is over it.
pub fn side<'a>(look: &Look<'a>, side: usize, head: Element<'a, Message>, tiles: Element<'a, Message>, foot: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let wrap = look.wrap;
    let dragging = look.cols.drag.is_some();
    let mut col = column![head, container(tiles).width(Length::Fill)].spacing(12).padding(Padding::from([14.0, 16.0]));
    if let Some(foot) = foot {
        col = col.push(foot);
    }
    let over = look.cols.drag.as_ref().is_some_and(|d| d.over == Some(Target::Side(side)) && d.from != side);
    let body: Element<'a, Message> = scrollable(col).style(style::scroll).width(Length::Fill).height(Length::Fill).into();
    let body: Element<'a, Message> = if over {
        stack![body, container(Space::new()).width(Length::Fill).height(Length::Fill).style(|_| container::Style { border: style::border(t::SIGNAL, 2.0, t::R_CELL), background: Some(Background::Color(alpha(t::SIGNAL, 0.05))), ..Default::default() })].into()
    } else {
        body
    };
    let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::Nothing), enter: wrap(ColMsg::DragEnter(Target::Side(side))), leave: wrap(ColMsg::DragLeave(Target::Side(side))), drop: wrap(ColMsg::Nothing) };
    container(crate::ui::drag::DragArea::new(body, messages, dragging).target_only()).width(Length::FillPortion(1)).height(Length::Fill).into()
}

/// The two columns with the line between them.
pub fn pair<'a>(left: Element<'a, Message>, right: Element<'a, Message>) -> Element<'a, Message> {
    row![left, container(Space::new().width(1).height(Length::Fill)).style(style::fill(t::PHOTO_LINE)), right].height(Length::Fill).into()
}

/// While faces are picked: how many, the move to the other side (named
/// `to_other`), *Split into a new group* when offered, and `close`.
pub fn pick_strip<'a>(look: &Look<'a>, to_other: [String; 2], close: Option<(Message, &'a str)>) -> Option<Element<'a, Message>> {
    let side = look.cols.picked_side()?;
    let wrap = look.wrap;
    let crops = crop_word(look.kind);
    let key = if side == 0 { "→" } else { "←" };
    let mut r = row![
        icons::view(Glyph::Select, 16.0, t::SIGNAL),
        text(format!("{} picked", util::plural(look.cols.picked[side].len() as u64, crops))).size(t::T_BODY).color(t::INK).width(Length::Fill),
        tip(kit::icon_text_btn(if side == 0 { Glyph::ChevronRight } else { Glyph::ChevronLeft }, to_other[1 - side].clone(), t::INK, style::secondary(), Some(wrap(ColMsg::MoveTo(Target::Side(1 - side))))), format!("To the other side  ({key})"), true),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center)
    .padding(Padding { top: 4.0, bottom: 4.0, left: 12.0, right: 8.0 });
    if look.cols.new_group {
        r = r.push(tip(kit::icon_text_btn(Glyph::Plus, "Split into a new group", t::INK, style::secondary(), Some(wrap(ColMsg::MoveTo(Target::New)))), "Into a group of their own, to name or merge  (N)", true));
    }
    if let Some((msg, words)) = close {
        r = r.push(tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(msg)), words, true));
    }
    Some(column![container(r).width(Length::Fill).style(style::fill(alpha(t::SIGNAL, 0.08))), container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::SIGNAL, 0.3)))].into())
}

/// Under the columns: *New group*, a drop target.
pub fn new_group_target<'a>(look: &Look<'a>) -> Element<'a, Message> {
    let wrap = look.wrap;
    let over = look.cols.drag.as_ref().is_some_and(|d| d.over == Some(Target::New));
    let words = if look.cols.drag.is_some() { "Drop here for a new group".to_string() } else { format!("New group: drop {} here to split them off", look.kind.crops().to_lowercase()) };
    let chip = container(row![icons::view(Glyph::Plus, 16.0, if over { t::SIGNAL } else { t::INK2 }), text(words).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK2)].spacing(8).align_y(Alignment::Center))
        .padding(Padding::from([8.0, 14.0]))
        .width(Length::Fill)
        .center_x(Length::Fill)
        .style(move |_| container::Style { background: Some(Background::Color(if over { alpha(t::SIGNAL, 0.14) } else { t::RAISED })), border: style::border(if over { t::SIGNAL } else { t::LINE_HI }, if over { 2.0 } else { 1.0 }, t::R_CONTROL), ..Default::default() });
    let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::Nothing), enter: wrap(ColMsg::DragEnter(Target::New)), leave: wrap(ColMsg::DragLeave(Target::New)), drop: wrap(ColMsg::Nothing) };
    container(crate::ui::drag::DragArea::new(chip, messages, look.cols.drag.is_some()).target_only()).padding(Padding::from([6.0, 16.0])).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// A row of keys and what they do.
pub fn keys<'a>(list: &[(&'a str, &'a str)]) -> Element<'a, Message> {
    let mut r = row![].spacing(16);
    for (k, what) in list {
        r = r.push(row![text(*k).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::PHOTO_INK), text(*what).size(t::T_SMALL).color(t::PHOTO_INK3)].spacing(4));
    }
    r.wrap().vertical_spacing(4).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key as K;

    fn f(n: u8) -> FaceRef {
        FaceRef::new(K([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }

    /// A drag carries the picked faces when it starts on one of them; a
    /// drop on its own side or on nothing moves nothing; picks live on one
    /// side; ← / → send them over.
    #[test]
    fn drags_and_keys_move_the_picked_faces() {
        let order = |s: usize| if s == 0 { vec![f(1), f(2), f(3), f(4)] } else { vec![f(7), f(8)] };
        let mut c = Columns::new(true);
        c.update(ColMsg::Pick(0, f(1)), false, order);
        c.update(ColMsg::Pick(0, f(3)), true, order);
        assert_eq!(c.picked[0], [f(1), f(2), f(3)], "a Shift run");
        c.update(ColMsg::DragStart(0, f(2)), false, order);
        c.update(ColMsg::DragEnter(Target::Side(0)), false, order);
        assert_eq!(c.update(ColMsg::DragDrop, false, order), None, "its own side");
        assert_eq!(c.picked[0].len(), 3, "kept");
        c.update(ColMsg::DragStart(0, f(4)), false, order);
        c.update(ColMsg::DragEnter(Target::Side(1)), false, order);
        assert_eq!(c.update(ColMsg::DragDrop, false, order), Some(Move { from: 0, faces: vec![f(4)], to: Target::Side(1) }), "not picked: it alone");
        assert_eq!(c.update(ColMsg::MoveTo(Target::Side(1)), false, order), Some(Move { from: 0, faces: vec![f(1), f(2), f(3)], to: Target::Side(1) }));
        assert!(c.picked[0].is_empty());
        c.update(ColMsg::Pick(1, f(7)), false, order);
        c.update(ColMsg::Pick(0, f(1)), false, order);
        assert!(c.picked[1].is_empty(), "picks live on one side");
        assert_eq!(c.update(ColMsg::MoveTo(Target::Side(0)), false, order), None, "← with the picks on the left: nothing");
    }

    /// Pinned faces are never picked, dragged or carried, even inside a
    /// Shift run; *New group* only where it is offered.
    #[test]
    fn pinned_faces_never_move_and_new_group_only_when_offered() {
        let order = |_: usize| vec![f(1), f(2), f(3)];
        let mut c = Columns::new(false);
        c.pinned.insert(f(2));
        c.update(ColMsg::Pick(0, f(2)), false, order);
        assert!(c.picked[0].is_empty());
        c.update(ColMsg::DragStart(0, f(2)), false, order);
        assert!(c.drag.is_none(), "a pinned face doesn't drag");
        c.update(ColMsg::Pick(0, f(1)), false, order);
        c.update(ColMsg::Pick(0, f(3)), true, order);
        assert_eq!(c.picked[0], [f(1), f(3)], "the run leaves the pin out");
        assert_eq!(c.update(ColMsg::MoveTo(Target::New), false, order), None, "no New group here");
        c.drag = Some(Drag { from: 0, faces: vec![f(1), f(2)], over: Some(Target::Side(1)) });
        assert_eq!(c.update(ColMsg::DragDrop, false, order), Some(Move { from: 0, faces: vec![f(1)], to: Target::Side(1) }), "a pin never rides along");
        assert!(c.escape(), "the picks");
        assert!(!c.escape());
    }
}
