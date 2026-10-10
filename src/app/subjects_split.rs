//! Splitting a fursuit group that holds more than one suit (stage 3), on
//! a fursuit's page:
//!
//! - **Least like the rest first.**  *Heads* can show the heads farthest
//!   from the group first, the few farthest marked: where its strays are
//!   (`search::splitting`, measured on the labels).  Unnamed groups open
//!   in that order, named ones clearest first; either switches.
//! - **Picking a run**: Shift+click picks every head between the last one
//!   clicked and this one, in the order shown.
//! - **Two heads in one photo**: a group holding both holds at least two
//!   fursuits.  A strip says so with the two heads, and *Propose a split…*
//!   opens the group editor with the group split around them.
//!
//! Moving heads is the group editor's (`subjects_editor`): *Split into a
//! new group* opens it with the picked heads in a new column.  Fursuits
//! only; heads the viewer's labels keep out are in none of it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::widget::{row, text, container};
use iced::{Alignment, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, GroupId, Kind};
use super::style;
use super::subjects::{self as subj, m as sm};
use super::subjects_editor::Preset;
use super::subjects_person::Person;
use super::{App, Message};
use crate::ui::icons::{self, Glyph};
use crate::ui::subject_tile as tile;
use crate::ui::theme as t;
use crate::util;

pub fn m(msg: Msg) -> Message {
    sm(subj::Msg::Split(msg))
}

/// The flag strip's crops.
const FLAG: f32 = 36.0;

/// How *Heads* is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Farthest from the group first (its strays first).
    LeastAlike,
    /// Clearest first (the least sure sink to the end).
    Clearest,
}

/// The engine's split check for the page's group.
#[derive(Debug, Clone, Default)]
pub struct Check {
    /// Each head's place from the far end (0: the farthest).
    pub rank: HashMap<FaceRef, usize>,
    /// The farthest few, marked.
    pub marked: HashSet<FaceRef>,
    /// Heads seen in one photo with another head of the group.
    pub pairs: Vec<(FaceRef, FaceRef)>,
    /// The split proposed around the first pair: (with its first head,
    /// with its second).
    pub split: Option<(Vec<FaceRef>, Vec<FaceRef>)>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Order(Order),
    /// The picked heads (people: faces) split off: the editor, with them
    /// in a new column.
    SplitPicked,
    /// The split proposed around two heads of one photo: the editor.
    Propose,
}

// ── asking ──────────────────────────────────────────────────────────────────

/// Ask for the page's split check (a fursuit's page only).
pub fn ask(app: &mut App) {
    let seq = app.subjects.next_seq();
    let Some(p) = app.subjects.person.as_mut().filter(|p| p.kind == Kind::Fursuit) else { return };
    p.check_seq = seq;
    let group = p.group.clone();
    app.search.link.group_check(seq, &group);
}

pub fn on_check(app: &mut App, seq: u64, group: GroupId, far: Arc<Vec<(FaceRef, f32)>>, pairs: Arc<Vec<(FaceRef, FaceRef)>>, split: Option<Arc<(Vec<FaceRef>, Vec<FaceRef>)>>) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if p.check_seq != seq || p.group != group {
        return Task::none();
    }
    let n = far.len();
    let rank: HashMap<FaceRef, usize> = far.iter().enumerate().map(|(i, (f, _))| (*f, i)).collect();
    let marked = far.iter().take(crate::search::splitting::marked(n)).map(|(f, _)| *f).collect();
    p.check = Some(Check { rank, marked, pairs: (*pairs).clone(), split: split.map(|s| (*s).clone()) });
    // The flag strip and the proposal show heads: have them at hand.
    let want_faces = !pairs.is_empty() && p.faces.is_none();
    let group = p.group.clone();
    let t = if want_faces { super::subjects_person::ask_faces(app, group) } else { Task::none() };
    Task::batch([t, subj::refresh_crops(app)])
}

/// The groups changed: a fursuit page's heads and check follow (a split or
/// a move may leave its photo count as it was, when a photo held two of its
/// heads).
pub fn on_subjects(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_ref().filter(|p| p.kind == Kind::Fursuit && app.subjects.review.is_none()) else { return Task::none() };
    let (group, faces) = (p.group.clone(), p.faces.is_some());
    ask(app);
    if faces { super::subjects_person::ask_faces(app, group) } else { Task::none() }
}

/// The order a page opens *Heads* in: least alike first for an unnamed
/// group (where strays are found), clearest first for a named one.
pub fn default_order(named: bool) -> Order {
    if named { Order::Clearest } else { Order::LeastAlike }
}

/// The page's heads in the order shown.
pub fn ordered(p: &Person) -> Vec<&Face> {
    let Some(faces) = &p.faces else { return Vec::new() };
    let mut v: Vec<&Face> = faces.iter().collect();
    if let (Order::LeastAlike, Some(c)) = (p.order, &p.check) {
        // Heads the check doesn't know (arrived since) after the rest, in
        // clearest order.
        v.sort_by_key(|f| c.rank.get(&f.face).copied().unwrap_or(usize::MAX));
    }
    v
}

/// A click on a head: picks it (or puts it back); with Shift, every head
/// from the last one clicked to this one, in the order shown.
pub fn pick(app: &mut App, f: FaceRef, shift: bool) {
    let Some(p) = app.subjects.person.as_mut() else { return };
    p.menu = None;
    if shift {
        let order: Vec<FaceRef> = ordered(p).iter().map(|x| x.face).collect();
        if let Some(run) = p.anchor.and_then(|a| run(&order, a, f)) {
            for x in run {
                if !p.picked.contains(&x) {
                    p.picked.push(x);
                }
            }
            p.anchor = Some(f);
            return;
        }
    }
    match p.picked.iter().position(|x| *x == f) {
        Some(i) => {
            p.picked.remove(i);
        }
        None => p.picked.push(f),
    }
    p.anchor = Some(f);
}

/// The heads from `anchor` to `f`, both included, in the order shown
/// (either way round); `None` when one isn't shown.
pub fn run(order: &[FaceRef], anchor: FaceRef, f: FaceRef) -> Option<Vec<FaceRef>> {
    let (i, j) = (order.iter().position(|x| *x == anchor)?, order.iter().position(|x| *x == f)?);
    Some(order[i.min(j)..=i.max(j)].to_vec())
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Order(o) => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.order = o;
            }
            subj::refresh_crops(app)
        }
        Msg::SplitPicked => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let picked = std::mem::take(&mut p.picked);
            p.anchor = None;
            if picked.is_empty() {
                return Task::none();
            }
            let group = p.group.clone();
            super::subjects_editor::open(app, group, Preset::Split(picked))
        }
        Msg::Propose => {
            let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
            let Some((a, b)) = p.check.as_ref().and_then(|c| c.split.clone()) else { return Task::none() };
            // The smaller side splits off, to start.
            let off = if b.len() <= a.len() { b } else { a };
            let group = p.group.clone();
            super::subjects_editor::open(app, group, Preset::Proposal(off))
        }
    }
}

// ── crops ───────────────────────────────────────────────────────────────────

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(p) = s.person.as_ref() else { return Vec::new() };
    let faces: HashMap<FaceRef, &Face> = p.faces.iter().flat_map(|f| f.iter()).map(|f| (f.face, f)).collect();
    let Some((a, b)) = p.check.as_ref().and_then(|c| c.pairs.first()) else { return Vec::new() };
    [a, b].into_iter().filter_map(|f| faces.get(f)).map(|f| ((*f).clone(), subj::px(app, FLAG), Framing::Face)).collect()
}

// ── views ───────────────────────────────────────────────────────────────────

/// Over the heads (fursuits): their order, switchable, and what it means.
pub fn order_bar<'a>(p: &'a Person, n: usize) -> Element<'a, Message> {
    let seg = kit::segmented(vec![
        kit::segment("Least like the rest", p.order == Order::LeastAlike, None, m(Msg::Order(Order::LeastAlike))),
        kit::segment("Clearest first", p.order == Order::Clearest, None, m(Msg::Order(Order::Clearest))),
    ]);
    let about = match p.order {
        Order::LeastAlike => format!("{}, least like the rest first: a head that isn't this fursuit is usually among the first. Shift+click picks a run.", util::plural(n as u64, "head")),
        Order::Clearest => format!("{}, clearest first. Shift+click picks a run.", util::plural(n as u64, "head")),
    };
    container(row![seg, text(about).size(t::T_SMALL).color(t::PHOTO_INK2)].spacing(t::S3).align_y(Alignment::Center)).padding(Padding { left: 16.0, right: 16.0, top: 10.0, bottom: 8.0 }).into()
}

/// A head marked in *Heads*: the badge and what it means.
pub fn badge(p: &Person, f: &FaceRef) -> Option<(Glyph, &'static str)> {
    let c = p.check.as_ref()?;
    if c.pairs.iter().any(|(a, b)| a == f || b == f) {
        return Some((Glyph::Split, "In one photo with another head of this group: one of the two is another fursuit"));
    }
    (p.order == Order::LeastAlike && c.marked.contains(f)).then_some((Glyph::Split, "Among the least like the rest"))
}

/// Under the band of a group holding two heads of one photo: it holds at
/// least two fursuits; the two heads; *Propose a split…*.
pub fn flag_strip<'a>(app: &'a App, p: &'a Person) -> Option<Element<'a, Message>> {
    let c = p.check.as_ref().filter(|c| !c.pairs.is_empty())?;
    let s = &app.subjects;
    let faces: HashMap<FaceRef, &Face> = p.faces.iter().flat_map(|f| f.iter()).map(|f| (f.face, f)).collect();
    let shape = subj::shape(Kind::Fursuit);
    let (a, b) = c.pairs[0];
    let crop = |f: FaceRef| -> Element<'a, Message> {
        let handle = faces.get(&f).and_then(|x| s.crops.get(&x.face, subj::px(app, FLAG), Framing::Face));
        tile::crop(handle, FLAG, shape)
    };
    let n = c.pairs.len();
    let words = if n == 1 {
        "Two of these heads are in one photo, so this group holds at least two fursuits.".to_string()
    } else {
        format!("{n} pairs of these heads are in one photo, so this group holds at least two fursuits.")
    };
    let mut r = row![icons::view(Glyph::Split, 16.0, t::INK2), crop(a), crop(b), text(words).size(t::T_SMALL).color(t::INK2).width(Length::Fill)].spacing(t::S2).align_y(Alignment::Center);
    if c.split.is_some() {
        r = r.push(tip(kit::btn("Propose a split…", style::secondary(), Some(m(Msg::Propose))), "The group split around these two heads, for you to check and confirm", true));
    }
    Some(container(r.padding(Padding::from([6.0, 12.0]))).width(Length::Fill).style(style::fill(t::MOUNT)).into())
}

pub fn debug_state(p: &Person) -> String {
    let c = p.check.as_ref().map(|c| format!("rank={} marked={} pairs={} split={:?}", c.rank.len(), c.marked.len(), c.pairs.len(), c.split.as_ref().map(|(a, b)| (a.len(), b.len()))));
    format!("split=[order={:?} check={} picked={}]", p.order, c.unwrap_or_else(|| "none".into()), p.picked.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unnamed_groups_open_least_alike_first_and_named_clearest_first() {
        assert_eq!(default_order(false), Order::LeastAlike);
        assert_eq!(default_order(true), Order::Clearest);
    }

    /// Shift+click picks the run between the last head clicked and this
    /// one, in the order shown, either way round.
    #[test]
    fn a_shift_click_picks_the_run_in_the_order_shown() {
        let f = |n: u8| FaceRef::new(crate::search::index::Key([n; 16]), [0.1, 0.1, 0.2, 0.2]);
        let order = [f(5), f(2), f(9), f(1), f(7)];
        assert_eq!(run(&order, f(2), f(1)), Some(vec![f(2), f(9), f(1)]));
        assert_eq!(run(&order, f(7), f(9)), Some(vec![f(9), f(1), f(7)]), "backwards too");
        assert_eq!(run(&order, f(5), f(5)), Some(vec![f(5)]));
        assert_eq!(run(&order, f(3), f(5)), None, "a head not shown");
    }
}
