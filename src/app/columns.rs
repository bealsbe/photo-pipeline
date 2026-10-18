//! Columns of faces (or fursuit heads) to move between, by drag or key:
//! the picks, the drag and the tiles of the group editor
//! (`subjects_editor`), where nothing changes until *Apply*.
//!
//! - A click picks a face (or puts it back); Shift+click picks the run from
//!   the last one clicked, in the order shown.  Picks live in one column.
//! - A face dragged (the picked ones with it when it is one of them) onto
//!   another column, *New group*, a group in the panel, *Take out* or
//!   *Not heads*.
//! - ← / → send the picked faces to the previous / next column; N to a new
//!   group; R takes them out of the group; Delete: not heads at all.
//!
//! The owner keeps the faces, the order and what a move may do (two heads
//! of one photo never share a column); this keeps the picks and the drag,
//! and hands back a [`Move`] for the owner to make.

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, column, container, row, scrollable, stack, text};
use iced::{Alignment, Background, Element, Length, Padding};

use super::kit::tip;
use super::people::{Face, FaceRef, Framing, GroupId, Kind};
use super::style;
use super::subjects as subj;
use super::{App, Message};
use crate::ui::icons::Glyph;
use crate::ui::paint;
use crate::ui::subject_tile::{self as tile, Tile};
use crate::ui::theme::{self as t, alpha};

/// Where a drag or a key sends the picked faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A column, by its place.
    Col(usize),
    /// A new column of its own.
    New,
    /// A group in the panel (not shown as a column).
    Group(GroupId),
    /// "Take out of this group" / "Not this person".
    TakenOut,
    /// "Not fursuit heads" / "Not faces".
    Removed,
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
    /// ← (false) / → (true): the picked faces to the previous / next column.
    Step(bool),
    DragStart(usize, FaceRef),
    DragEnter(Target),
    DragLeave(Target),
    DragDrop,
    Nothing,
}

/// The picks and the drag.
#[derive(Debug, Clone, Default)]
pub struct Columns {
    pub picked: Vec<FaceRef>,
    /// The column the picks are in.
    pub picked_in: Option<usize>,
    anchor: Option<FaceRef>,
    pub drag: Option<Drag>,
}

impl Columns {
    /// A message, with each column's faces in the order shown and how many
    /// columns there are; a move to make, if any.  `shift`: a click picks a
    /// run.
    pub fn update(&mut self, msg: ColMsg, shift: bool, order: impl Fn(usize) -> Vec<FaceRef>, columns: usize) -> Option<Move> {
        match msg {
            ColMsg::Pick(col, f) => {
                self.pick(col, f, shift, &order(col));
                None
            }
            ColMsg::MoveTo(to) => {
                let from = self.picked_in?;
                if to == Target::Col(from) {
                    return None;
                }
                self.take(from, self.picked.clone(), to)
            }
            ColMsg::Step(forward) => {
                let from = self.picked_in?;
                let to = if forward { from + 1 } else { from.checked_sub(1)? };
                if to >= columns {
                    return None;
                }
                self.take(from, self.picked.clone(), Target::Col(to))
            }
            ColMsg::DragStart(col, f) => {
                let faces = if self.picked_in == Some(col) && self.picked.contains(&f) { self.picked.clone() } else { vec![f] };
                self.drag = Some(Drag { from: col, faces, over: None });
                None
            }
            ColMsg::DragEnter(to) => {
                if let Some(d) = self.drag.as_mut() {
                    d.over = Some(to);
                }
                None
            }
            ColMsg::DragLeave(to) => {
                if let Some(d) = self.drag.as_mut().filter(|d| d.over.as_ref() == Some(&to)) {
                    d.over = None;
                }
                None
            }
            ColMsg::DragDrop => {
                let Drag { from, faces, over } = self.drag.take()?;
                let to = over?;
                if to == Target::Col(from) {
                    return None;
                }
                self.take(from, faces, to)
            }
            ColMsg::Nothing => None,
        }
    }

    /// A click picks or puts back; with Shift, every face from the last one
    /// clicked in that column to this one, in `order`.
    pub fn pick(&mut self, col: usize, f: FaceRef, shift: bool, order: &[FaceRef]) {
        if self.picked_in != Some(col) {
            self.picked.clear();
            self.anchor = None;
            self.picked_in = Some(col);
        }
        if shift {
            if let Some(run) = self.anchor.and_then(|a| super::subjects_split::run(order, a, f)) {
                for x in run {
                    if !self.picked.contains(&x) {
                        self.picked.push(x);
                    }
                }
                self.anchor = Some(f);
                return;
            }
        }
        match self.picked.iter().position(|x| *x == f) {
            Some(i) => {
                self.picked.remove(i);
            }
            None => self.picked.push(f),
        }
        self.anchor = Some(f);
        if self.picked.is_empty() {
            self.picked_in = None;
        }
    }

    /// Pick these (a preset: the heads picked before the editor opened).
    pub fn pick_all(&mut self, col: usize, faces: Vec<FaceRef>) {
        self.picked_in = (!faces.is_empty()).then_some(col);
        self.anchor = faces.last().copied();
        self.picked = faces;
    }

    fn take(&mut self, from: usize, faces: Vec<FaceRef>, to: Target) -> Option<Move> {
        if faces.is_empty() {
            return None;
        }
        Some(Move { from, faces, to })
    }

    /// The move was made: its faces are no longer picked.
    pub fn moved(&mut self, faces: &[FaceRef]) {
        self.picked.retain(|f| !faces.contains(f));
        if self.picked.is_empty() {
            self.picked_in = None;
            self.anchor = None;
        }
    }

    /// Esc: a drag, then the picks.  Whether there was one.
    pub fn escape(&mut self) -> bool {
        if self.drag.take().is_some() {
            return true;
        }
        if !self.picked.is_empty() {
            self.clear();
            return true;
        }
        false
    }

    pub fn clear(&mut self) {
        self.picked.clear();
        self.picked_in = None;
        self.anchor = None;
    }

    /// Columns were removed or reordered: picks follow, or go.
    pub fn renumber(&mut self, map: impl Fn(usize) -> Option<usize>) {
        match self.picked_in.and_then(&map) {
            Some(c) => self.picked_in = Some(c),
            None => self.clear(),
        }
        self.drag = None;
    }
}

/// The editor's keys: ← / →, N, R, Delete.  The message, if the key is one.
pub fn key_msg(key: &Key, mods: keyboard::Modifiers) -> Option<ColMsg> {
    if mods.control() || mods.alt() {
        return None;
    }
    match key {
        Key::Named(Named::ArrowLeft) => Some(ColMsg::Step(false)),
        Key::Named(Named::ArrowRight) => Some(ColMsg::Step(true)),
        Key::Named(Named::Delete) => Some(ColMsg::MoveTo(Target::Removed)),
        Key::Character(c) if c.as_str().eq_ignore_ascii_case("n") => Some(ColMsg::MoveTo(Target::New)),
        Key::Character(c) if c.as_str().eq_ignore_ascii_case("r") => Some(ColMsg::MoveTo(Target::TakenOut)),
        _ => None,
    }
}

/// One crop's word: "head" for fursuits, "face" for people.
pub fn crop_word(kind: Kind) -> &'static str {
    if kind == Kind::Fursuit { "head" } else { "face" }
}

// ── views ───────────────────────────────────────────────────────────────────

/// What the views need from their owner.
pub struct Look<'a> {
    pub kind: Kind,
    pub cols: &'a Columns,
    /// The owner's message for a column message.
    pub wrap: fn(ColMsg) -> Message,
}

/// A column's crops: only those on screen (or near) load.
pub fn section(col: usize) -> subj::Section {
    subj::Section::Column(col.min(250) as u8)
}

/// One column's faces as tiles: the first `marked` are the least like the
/// rest; `paired` ones (another head of their photo is in the edit) carry
/// the pin; `cover` (the group photo, and whether it was chosen) its mark.
/// Only the tiles on screen (or near) load their crops.
pub fn grid<'a>(app: &'a App, look: &Look<'a>, col: usize, order: &[&'a Face], marked: usize, paired: &dyn Fn(&FaceRef) -> bool, cover: Option<(FaceRef, bool)>) -> Element<'a, Message> {
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
        let pinned = paired(&f.face);
        let is_cover = cover.filter(|(c, _)| *c == f.face);
        let badge = if pinned {
            Some(Glyph::Pin)
        } else if is_cover.is_some() {
            Some(super::subjects_cover::MARK)
        } else {
            (i < marked).then_some(Glyph::Split)
        };
        let picked = look.cols.picked_in == Some(col) && look.cols.picked.contains(&f.face);
        let el: Element<'a, Message> = Tile { crop, name: None, count: f.day.format("%b %-d, %Y").to_string(), picked, small: true, badge, on: wrap(ColMsg::Pick(col, f.face)), badge_on: None }.into();
        let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::DragStart(col, f.face)), enter: wrap(ColMsg::Nothing), leave: wrap(ColMsg::Nothing), drop: wrap(ColMsg::DragDrop) };
        let el: Element<'a, Message> = crate::ui::drag::DragArea::new(el, messages, dragging).into();
        let words = if pinned {
            format!("{}. Another {} of this photo is in the edit: the two can't share a column", super::search::long_date(f.day), crop_word(look.kind))
        } else {
            let photo = match is_cover {
                Some((_, true)) => ". The group photo",
                Some((_, false)) => ". The group photo, picked automatically: the clearest",
                None => "",
            };
            format!("{}{}{}", super::search::long_date(f.day), if i < marked { ". Among the least like the rest" } else { "" }, photo)
        };
        r = r.push(tip(el, words, true));
    }
    crate::ui::in_view::InView::new(r.wrap().vertical_spacing(8), keys, move |keys| subj::m(subj::Msg::InView(section(col), keys))).into()
}

/// A column: `head` over its tiles, scrolling; the whole of it takes a
/// drop from another column, outlined while one is over it.
pub fn column_box<'a>(look: &Look<'a>, col: usize, head: Element<'a, Message>, tiles: Element<'a, Message>, foot: Option<Element<'a, Message>>, width: Length) -> Element<'a, Message> {
    let wrap = look.wrap;
    let dragging = look.cols.drag.is_some();
    let mut body = column![container(tiles).width(Length::Fill)].spacing(10).padding(Padding { top: 4.0, bottom: 14.0, left: 12.0, right: 12.0 });
    if let Some(foot) = foot {
        body = body.push(foot);
    }
    let over = look.cols.drag.as_ref().is_some_and(|d| d.over == Some(Target::Col(col)) && d.from != col);
    let scroll: Element<'a, Message> = scrollable(body).style(style::scroll).width(Length::Fill).height(Length::Fill).into();
    let whole: Element<'a, Message> = column![container(head).padding(Padding { top: 12.0, bottom: 6.0, left: 12.0, right: 12.0 }).width(Length::Fill), scroll].into();
    let whole: Element<'a, Message> = if over { outlined(whole) } else { whole };
    let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::Nothing), enter: wrap(ColMsg::DragEnter(Target::Col(col))), leave: wrap(ColMsg::DragLeave(Target::Col(col))), drop: wrap(ColMsg::Nothing) };
    container(crate::ui::drag::DragArea::new(whole, messages, dragging).target_only()).width(width).height(Length::Fill).into()
}

/// Outlined in the accent: a drop is over it.
pub fn outlined<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    stack![content, container(Space::new()).width(Length::Fill).height(Length::Fill).style(|_| container::Style { border: style::border(t::SIGNAL, 2.0, t::R_CELL), background: Some(Background::Color(alpha(t::SIGNAL, 0.05))), ..Default::default() })].into()
}

/// A drop target (a panel group, *Take out*, *Not heads*, *New group*):
/// `content`, outlined while a drag is over it.
pub fn drop_target<'a>(look: &Look<'a>, target: Target, content: Element<'a, Message>) -> Element<'a, Message> {
    let wrap = look.wrap;
    let over = look.cols.drag.as_ref().is_some_and(|d| d.over.as_ref() == Some(&target));
    let content: Element<'a, Message> = container(content)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(if over { alpha(t::SIGNAL, 0.14) } else { Default::default() })),
            border: style::border(if over { t::SIGNAL } else { iced::Color::TRANSPARENT }, if over { 2.0 } else { 0.0 }, t::R_CONTROL),
            ..Default::default()
        })
        .into();
    let messages = crate::ui::drag::DragMessages { start: wrap(ColMsg::Nothing), enter: wrap(ColMsg::DragEnter(target.clone())), leave: wrap(ColMsg::DragLeave(target)), drop: wrap(ColMsg::Nothing) };
    crate::ui::drag::DragArea::new(content, messages, look.cols.drag.is_some()).target_only().into()
}

/// A row of keys and what they do.
pub fn keys<'a>(list: &[(&'a str, &'a str)]) -> Element<'a, Message> {
    let mut r = row![].spacing(16);
    for (k, what) in list {
        r = r.push(row![text(*k).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::PHOTO_INK), text(*what).size(t::T_SMALL).color(t::PHOTO_INK3)].spacing(4).align_y(Alignment::Center));
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
    /// drop on its own column or on nothing moves nothing; picks live in
    /// one column; ← / → send them to the column beside, not past the
    /// ends.
    #[test]
    fn drags_and_keys_move_the_picked_faces() {
        let order = |s: usize| match s {
            0 => vec![f(1), f(2), f(3), f(4)],
            1 => vec![f(7), f(8)],
            _ => vec![],
        };
        let mut c = Columns::default();
        c.update(ColMsg::Pick(0, f(1)), false, order, 3);
        c.update(ColMsg::Pick(0, f(3)), true, order, 3);
        assert_eq!(c.picked, [f(1), f(2), f(3)], "a Shift run");
        c.update(ColMsg::DragStart(0, f(2)), false, order, 3);
        c.update(ColMsg::DragEnter(Target::Col(0)), false, order, 3);
        assert_eq!(c.update(ColMsg::DragDrop, false, order, 3), None, "its own column");
        c.update(ColMsg::DragStart(0, f(4)), false, order, 3);
        c.update(ColMsg::DragEnter(Target::Col(2)), false, order, 3);
        assert_eq!(c.update(ColMsg::DragDrop, false, order, 3), Some(Move { from: 0, faces: vec![f(4)], to: Target::Col(2) }), "not picked: it alone");
        c.update(ColMsg::DragStart(0, f(1)), false, order, 3);
        c.update(ColMsg::DragEnter(Target::Group(GroupId::Cluster(9))), false, order, 3);
        assert_eq!(c.update(ColMsg::DragDrop, false, order, 3), Some(Move { from: 0, faces: vec![f(1), f(2), f(3)], to: Target::Group(GroupId::Cluster(9)) }), "picked: all of them");
        assert_eq!(c.update(ColMsg::Step(false), false, order, 3), None, "← from the first column: nowhere");
        assert_eq!(c.update(ColMsg::Step(true), false, order, 3), Some(Move { from: 0, faces: vec![f(1), f(2), f(3)], to: Target::Col(1) }));
        c.moved(&[f(1), f(2), f(3)]);
        assert!(c.picked.is_empty() && c.picked_in.is_none());
        c.update(ColMsg::Pick(1, f(7)), false, order, 3);
        c.update(ColMsg::Pick(0, f(1)), false, order, 3);
        assert_eq!((c.picked.clone(), c.picked_in), (vec![f(1)], Some(0)), "picks live in one column");
        assert!(c.escape(), "the picks");
        assert!(!c.escape());
    }

    /// The editor's keys: N, R and Delete send the picked faces to a new
    /// group, out of the group, or out as not heads; with Ctrl or Alt,
    /// nothing.
    #[test]
    fn the_keys_name_their_targets() {
        let k = |s: &str| Key::Character(s.into());
        let none = keyboard::Modifiers::empty();
        assert!(matches!(key_msg(&k("n"), none), Some(ColMsg::MoveTo(Target::New))));
        assert!(matches!(key_msg(&k("R"), none), Some(ColMsg::MoveTo(Target::TakenOut))));
        assert!(matches!(key_msg(&Key::Named(Named::Delete), none), Some(ColMsg::MoveTo(Target::Removed))));
        assert!(matches!(key_msg(&Key::Named(Named::ArrowRight), none), Some(ColMsg::Step(true))));
        assert!(key_msg(&k("r"), keyboard::Modifiers::CTRL).is_none());
    }
}
