//! Splitting a fursuit group that holds more than one suit (stage 3), on
//! a fursuit's page:
//!
//! - **Least like the rest first.**  *Heads* can show the heads farthest
//!   from the group first, the few farthest marked: where its strays are
//!   (`search::splitting`, measured on the labels).  Unnamed groups open
//!   in that order, named ones clearest first; either switches.
//! - **Picking a run**: Shift+click picks every head between the last one
//!   clicked and this one, in the order shown.
//! - **Split into a new group**, in the pick strip: the group in two
//!   columns, the picked heads on *Splits off*, to adjust and confirm; or
//!   drag the picked heads onto a group in the tray that shows while
//!   dragging (or *A new group*, at once).
//! - **Two heads in one photo**: a group holding both holds at least two
//!   fursuits.  A strip says so with the two heads, and *Propose a split…*
//!   shows the group split around them in the same two columns, the two
//!   heads pinned one on each side.
//! - **Arranging a split** (`Arrange`): heads dragged between *Stays* and
//!   *Splits off* (or picked, Shift+click a run, and sent with ← / →), each
//!   side least like the rest first; *Swap sides*; Ctrl+Z takes back the
//!   last change.  Nothing changes in the library until *Split off*, which
//!   makes the final arrangement as one move (one undo step); Esc or
//!   *Cancel* leaves everything as it was.
//!
//! Nothing moves without the user's click or drop; a split is the existing
//! move (one undo step).  Fursuits only; heads the viewer's labels keep
//! out are in none of it (the engine leaves them out).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::widget::{Space, column, container, pin, row, text};
use iced::{Alignment, Background, Element, Length, Padding, Point, Task};

use super::columns::{self as cols, ColMsg, Columns, Look};
use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, GroupId, Kind};
use super::style;
use super::subjects::{self as subj, m as sm};
use super::subjects_person::{Person, Show};
use super::{App, Message};
use crate::model::decisions::{At, Via};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme::{self as t, alpha};
use crate::util;

pub fn m(msg: Msg) -> Message {
    sm(subj::Msg::Split(msg))
}

/// The arrangement's two columns: their crops load as they come on screen.
const SECTIONS: [subj::Section; 2] = [subj::Section::Column(0), subj::Section::Column(1)];
/// The flag strip's crops.
const FLAG: f32 = 36.0;
/// The drop tray's group crops, and how many groups it offers.
const TRAY: f32 = 36.0;
const TRAY_N: usize = 8;

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

/// A change to an arrangement, for Ctrl+Z.
#[derive(Debug, Clone)]
enum Change {
    /// The sides before heads were moved.
    Moved([Vec<FaceRef>; 2]),
    Swapped,
}

/// A split being arranged: what stays (side 0) and what splits off into a
/// new group (side 1).  Nothing changes in the library until it is
/// confirmed, as one move.
#[derive(Debug, Clone)]
pub struct Arrange {
    pub sides: [Vec<FaceRef>; 2],
    /// Where each head started (how many were moved by hand).
    start: [HashSet<FaceRef>; 2],
    /// Each side's order, least like the rest of that side first.
    pub rank: [HashMap<FaceRef, usize>; 2],
    rank_seq: [u64; 2],
    /// The picks, the drag; the proposal's two heads from one photo pinned.
    pub cols: Columns,
    /// What Ctrl+Z takes back: the sides before a move, or a swap.
    history: Vec<Change>,
    /// Opened from picked heads (*Split into a new group*), not proposed.
    pub from_pick: bool,
}

impl Arrange {
    /// `stays` and `leaves`, with `pins` (the two heads from one photo, one
    /// on each side) never moving.
    pub fn new(stays: Vec<FaceRef>, leaves: Vec<FaceRef>, pins: Option<(FaceRef, FaceRef)>) -> Arrange {
        let mut cols = Columns::new(false);
        if let Some((a, b)) = pins {
            cols.pinned.extend([a, b]);
        }
        let start = [stays.iter().copied().collect(), leaves.iter().copied().collect()];
        Arrange { sides: [stays, leaves], start, rank: [HashMap::new(), HashMap::new()], rank_seq: [0, 0], cols, history: Vec::new(), from_pick: pins.is_none() }
    }

    /// Heads moved to the other side (not swapped).
    pub fn apply(&mut self, mv: &cols::Move) -> bool {
        let cols::Target::Side(to) = mv.to else { return false };
        let to = to.min(1);
        let faces: Vec<FaceRef> = mv.faces.iter().filter(|f| !self.cols.pinned.contains(f) && self.sides[1 - to].contains(f)).copied().collect();
        if faces.is_empty() {
            return false;
        }
        self.history.push(Change::Moved(self.sides.clone()));
        self.sides[1 - to].retain(|f| !faces.contains(f));
        // At the top of their new side until its order comes back.
        let mut v = faces;
        v.extend(self.sides[to].drain(..));
        self.sides[to] = v;
        true
    }

    /// *Swap sides*: what stays splits off and the other way round.
    pub fn swap(&mut self) {
        self.history.push(Change::Swapped);
        self.swap_sides();
    }

    fn swap_sides(&mut self) {
        self.sides.swap(0, 1);
        self.start.swap(0, 1);
        self.rank.swap(0, 1);
        self.rank_seq.swap(0, 1);
        self.cols.picked.swap(0, 1);
        self.cols.drag = None;
    }

    /// Ctrl+Z: the last change taken back.
    pub fn take_back(&mut self) -> bool {
        match self.history.pop() {
            None => return false,
            Some(Change::Swapped) => self.swap_sides(),
            Some(Change::Moved(sides)) => self.sides = sides,
        }
        self.cols.picked = [Vec::new(), Vec::new()];
        self.cols.drag = None;
        true
    }

    /// Heads on another side than they started on.
    pub fn moved(&self) -> usize {
        (0..2).map(|s| self.sides[s].iter().filter(|f| !self.start[s].contains(f)).count()).sum()
    }

    /// A side in the order shown: its pinned head first, then least like
    /// the rest of it first (heads whose order isn't in yet, just moved
    /// there, before the rest).
    pub fn ordered(&self, side: usize) -> Vec<FaceRef> {
        let mut v = self.sides[side].clone();
        v.sort_by_key(|f| if self.cols.pinned.contains(f) { 0 } else { self.rank[side].get(f).map_or(1, |r| r + 2) });
        v
    }

    /// What *Split off* moves: the side splitting off, of the heads still
    /// in the group; `None` when either side would be empty.
    pub fn confirm(&self, in_group: impl Fn(&FaceRef) -> bool) -> Option<Vec<FaceRef>> {
        let stays = self.sides[0].iter().filter(|f| in_group(f)).count();
        let leaves: Vec<FaceRef> = self.sides[1].iter().filter(|f| in_group(f)).copied().collect();
        (stays > 0 && !leaves.is_empty()).then_some(leaves)
    }
}

/// Heads being dragged, and the tray target under the pointer (`Some(None)`:
/// *A new group*).
#[derive(Debug, Clone, PartialEq)]
pub struct HeadDrag {
    pub faces: Vec<FaceRef>,
    pub over: Option<Option<GroupId>>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Order(Order),
    /// Split the picked heads into a new group.
    SplitPicked,
    Propose,
    ProposeSwap,
    ProposeGo,
    ProposeCancel,
    /// The two columns of a split being arranged.
    Arrange(ColMsg),
    DragStart(FaceRef),
    DragEnter(Option<GroupId>),
    DragLeave(Option<GroupId>),
    DragDrop,
    /// A drag area's message with nothing to do.
    Nothing,
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
            let Some(p) = app.subjects.person.as_mut().filter(|p| p.kind == Kind::Fursuit) else { return Task::none() };
            // The group in two columns, the picked heads splitting off.
            let picked = std::mem::take(&mut p.picked);
            p.anchor = None;
            if picked.is_empty() {
                return Task::none();
            }
            let stays: Vec<FaceRef> = in_group(p).into_iter().filter(|f| !picked.contains(f)).collect();
            p.proposal = Some(Arrange::new(stays, picked, None));
            ask_order(app);
            subj::refresh_crops(app)
        }
        Msg::Propose => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let Some(c) = p.check.as_ref() else { return Task::none() };
            let (Some((a, b)), Some(&(pa, pb))) = (c.split.clone(), c.pairs.first()) else { return Task::none() };
            // The smaller side splits off, to start.
            let (stays, leaves, pins) = if b.len() <= a.len() { (a, b, (pa, pb)) } else { (b, a, (pb, pa)) };
            p.proposal = Some(Arrange::new(stays, leaves, Some(pins)));
            ask_order(app);
            subj::refresh_crops(app)
        }
        Msg::ProposeSwap => {
            if let Some(a) = app.subjects.person.as_mut().and_then(|p| p.proposal.as_mut()) {
                a.swap();
            }
            Task::none()
        }
        Msg::ProposeCancel => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.proposal = None;
            }
            Task::none()
        }
        Msg::ProposeGo => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let Some(a) = p.proposal.as_ref() else { return Task::none() };
            let held: HashSet<FaceRef> = in_group(p).into_iter().collect();
            let Some(leaving) = a.confirm(|f| held.contains(f)) else { return Task::none() };
            let (moved, from_pick) = (a.moved(), a.from_pick);
            p.proposal = None;
            let via = if from_pick { Via::Arranged { edited: moved as u32 } } else { Via::Proposal { edited: moved as u32 } };
            let t = split_off(app, leaving.clone(), via);
            if moved == 0 {
                return t;
            }
            // Say what the hand changed (the status line's words).
            let what = if from_pick { "after moving" } else { "the proposal, after moving" };
            app.show_status_action(format!("Split {} into a new group ({what} {} by hand).", util::plural(leaving.len() as u64, "head"), util::plural(moved as u64, "head")), "Undo", sm(subj::Msg::Undo))
        }
        Msg::Arrange(c) => {
            let shift = crate::ui::modifiers::get().shift();
            let Some(a) = app.subjects.person.as_mut().and_then(|p| p.proposal.as_mut()) else { return Task::none() };
            let orders = [a.ordered(0), a.ordered(1)];
            let Some(mv) = a.cols.update(c, shift, |s| orders[s].clone()) else { return Task::none() };
            if a.apply(&mv) {
                ask_order(app);
            }
            Task::none()
        }
        Msg::DragStart(f) => {
            let Some(p) = app.subjects.person.as_mut().filter(|p| p.show == Show::Faces && p.kind == Kind::Fursuit) else { return Task::none() };
            // The picked heads when the dragged one is among them, else it
            // alone.
            let faces = if p.picked.contains(&f) { p.picked.clone() } else { vec![f] };
            p.head_drag = Some(HeadDrag { faces, over: None });
            subj::refresh_crops(app)
        }
        Msg::DragEnter(target) => {
            if let Some(d) = app.subjects.person.as_mut().and_then(|p| p.head_drag.as_mut()) {
                d.over = Some(target);
            }
            Task::none()
        }
        Msg::DragLeave(target) => {
            if let Some(d) = app.subjects.person.as_mut().and_then(|p| p.head_drag.as_mut()).filter(|d| d.over.as_ref() == Some(&target)) {
                d.over = None;
            }
            Task::none()
        }
        Msg::DragDrop => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let Some(HeadDrag { faces, over: Some(to) }) = p.head_drag.take() else { return Task::none() };
            match to {
                None => split_off(app, faces, Via::Hand(At::Heads)),
                Some(to) => move_heads(app, faces, to),
            }
        }
        Msg::Nothing => Task::none(),
    }
}

/// Esc while dragging heads or arranging a split: a drag, the picks,
/// then the arrangement (nothing changed).
pub fn escape(app: &mut App) -> bool {
    let Some(p) = app.subjects.person.as_mut() else { return false };
    if p.head_drag.take().is_some() {
        return true;
    }
    match p.proposal.as_mut() {
        Some(a) => {
            if !a.cols.escape() {
                p.proposal = None;
            }
            true
        }
        None => false,
    }
}

/// Keys while a split is arranged: ← / → send the picked heads to that
/// side, Enter splits.  Whether the arrangement took the key.
pub fn arrange_key(app: &mut App, key: &iced::keyboard::Key, mods: iced::keyboard::Modifiers) -> Option<Task<Message>> {
    app.subjects.person.as_ref()?.proposal.as_ref()?;
    if let Some(c) = cols::key_msg(key, mods, false) {
        return Some(update(app, Msg::Arrange(c)));
    }
    if matches!(key, iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter)) && !mods.control() && !mods.alt() {
        return Some(update(app, Msg::ProposeGo));
    }
    Some(Task::none())
}

/// Ctrl+Z while a split is arranged: its last change taken back (the
/// library's undo waits until it is closed).  Whether one was open.
pub fn take_back(app: &mut App) -> Option<Task<Message>> {
    let a = app.subjects.person.as_mut()?.proposal.as_mut()?;
    if a.take_back() {
        ask_order(app);
    }
    Some(Task::none())
}

/// The heads of the page's group a split arranges: those the check saw
/// (the viewer's removed ones are in none), in the page's order.
fn in_group(p: &Person) -> Vec<FaceRef> {
    let faces = p.faces.iter().flat_map(|f| f.iter()).map(|f| f.face);
    match p.check.as_ref().filter(|c| !c.rank.is_empty()) {
        Some(c) => faces.filter(|f| c.rank.contains_key(f)).collect(),
        None => faces.collect(),
    }
}

/// Each side's order, least like the rest of that side first.
fn ask_order(app: &mut App) {
    let seqs = [app.subjects.next_seq(), app.subjects.next_seq()];
    let Some(a) = app.subjects.person.as_mut().and_then(|p| p.proposal.as_mut()) else { return };
    a.rank_seq = seqs;
    let sides = a.sides.clone();
    for (side, heads) in sides.iter().enumerate() {
        app.search.link.heads_order(seqs[side], Kind::Fursuit, heads);
    }
}

/// A side's order came back.
pub fn on_heads_order(app: &mut App, seq: u64, far: &[(FaceRef, f32)]) -> Task<Message> {
    let Some(a) = app.subjects.person.as_mut().and_then(|p| p.proposal.as_mut()) else { return Task::none() };
    let Some(side) = (0..2).find(|&s| a.rank_seq[s] == seq) else { return Task::none() };
    a.rank[side] = far.iter().enumerate().map(|(i, (f, _))| (*f, i)).collect();
    subj::refresh_crops(app)
}

/// Heads out of the page's group into a new group of their own: the
/// existing move, one undo step.
fn split_off(app: &mut App, faces: Vec<FaceRef>, via: Via) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if faces.is_empty() || p.kind != Kind::Fursuit {
        return Task::none();
    }
    p.picked.retain(|f| !faces.contains(f));
    p.anchor = None;
    let from = p.group.clone();
    let n = faces.len() as u64;
    app.search.link.move_faces(&from, &faces, None, via);
    app.show_status_action(format!("Split {} into a new group, to name or merge.", util::plural(n, "head")), "Undo", sm(subj::Msg::Undo))
}

fn move_heads(app: &mut App, faces: Vec<FaceRef>, to: GroupId) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if faces.is_empty() || to == p.group || app.subjects.all.group(&to).is_none_or(|g| g.kind != Kind::Fursuit) {
        return Task::none();
    }
    let p = app.subjects.person.as_mut().expect("person");
    p.picked.retain(|f| !faces.contains(f));
    p.anchor = None;
    let from = p.group.clone();
    let n = faces.len() as u64;
    app.search.link.move_faces(&from, &faces, Some(&to), Via::Hand(At::Heads));
    let whom = subj::label(&app.subjects, &to);
    app.show_status_action(format!("Moved {} to {whom}.", util::plural(n, "head")), "Undo", sm(subj::Msg::Undo))
}

// ── crops ───────────────────────────────────────────────────────────────────

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(p) = s.person.as_ref() else { return Vec::new() };
    let mut v = Vec::new();
    let faces: HashMap<FaceRef, &Face> = p.faces.iter().flat_map(|f| f.iter()).map(|f| (f.face, f)).collect();
    if let Some(c) = &p.check {
        if let Some((a, b)) = c.pairs.first() {
            v.extend([a, b].into_iter().filter_map(|f| faces.get(f)).map(|f| ((*f).clone(), subj::px(app, FLAG), Framing::Face)));
        }
    }
    if let Some(a) = &p.proposal {
        let small = subj::px(app, tile::SMALL_CROP);
        for side in 0..2 {
            let shown = a.ordered(side).into_iter().filter_map(|f| faces.get(&f).copied());
            v.extend(shown.filter(|f| s.crops.is_seen(SECTIONS[side], &subj::Crops::key(&f.face, small, Framing::Face))).map(|f| (f.clone(), small, Framing::Face)));
        }
    }
    if p.head_drag.is_some() {
        v.extend(tray_groups(app, p).into_iter().filter_map(|id| s.group(&id)).map(|g| (g.cover.clone(), subj::px(app, TRAY), Framing::Face)));
    }
    v
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

fn wrap(c: ColMsg) -> Message {
    m(Msg::Arrange(c))
}

/// A split being arranged, in two columns: what stays, what splits off
/// into a new group.  Nothing changes until *Split off*.
pub fn proposal_dialog(app: &App) -> Option<Element<'_, Message>> {
    let s = &app.subjects;
    let p = s.person.as_ref()?;
    let a = p.proposal.as_ref()?;
    let faces: HashMap<FaceRef, &Face> = p.faces.iter().flat_map(|f| f.iter()).map(|f| (f.face, f)).collect();
    let look = Look { kind: Kind::Fursuit, cols: &a.cols, wrap, sections: SECTIONS };
    let name = subj::label(s, &p.group);
    let here = if name == "the unnamed group" { "this group".to_string() } else { name };
    let n = [0, 1].map(|side| a.sides[side].iter().filter(|f| faces.contains_key(f)).count());
    let column_of = |side: usize| -> Element<'_, Message> {
        let order: Vec<&Face> = a.ordered(side).into_iter().filter_map(|f| faces.get(&f).copied()).collect();
        let marked = if a.rank[side].is_empty() { 0 } else { crate::search::splitting::marked(order.len()) };
        let title = if side == 0 { format!("Stays in {here}") } else { "Splits off into a new group".to_string() };
        let head = column![
            text(title).size(t::T_BODY).font(paint::SEMIBOLD).color(t::PHOTO_INK),
            text(format!("{}, least like the rest first", util::plural(n[side] as u64, "head"))).size(t::T_SMALL).color(t::PHOTO_INK3)
        ]
        .spacing(2);
        let tiles = cols::grid(app, &look, side, &order, marked);
        let foot: Option<Element<'_, Message>> = order.is_empty().then(|| text("Drag heads here.").size(t::T_SMALL).color(t::PHOTO_INK3).into());
        cols::side(&look, side, head.into(), tiles, foot)
    };
    let columns = container(cols::pair(column_of(0), column_of(1)))
        .height(Length::Fixed((app.window_size_height() * 0.56).max(260.0)))
        .width(Length::Fill)
        .style(|_| container::Style { background: Some(Background::Color(t::PHOTO_GROUND)), border: style::border(t::LINE, 1.0, t::R_CELL), ..Default::default() });
    let (title, about) = if a.from_pick {
        ("Split off the picked heads?", "Your picked heads are on the right. Drag heads across, or pick them and press ← or →, until each side is one fursuit. Nothing changes until you split.")
    } else {
        ("Split this group in two?", "Two of its heads are in one photo, so they are two fursuits. Each head starts beside the one of the two it looks more like. Drag heads across, or pick them and press ← or →. Nothing changes until you split.")
    };
    let mut content = column![super::dialogs::header(title, None), kit::secondary(about)].spacing(t::S3);
    if !a.cols.pinned.is_empty() {
        // The two, as the flag strip shows them, in their sides' order.
        let pins = [0, 1].map(|side| a.sides[side].iter().find(|f| a.cols.pinned.contains(f)).and_then(|f| faces.get(f)).map(|x| tile::crop(s.crops.get(&x.face, subj::px(app, FLAG), Framing::Face), FLAG * 0.75, subj::shape(Kind::Fursuit))));
        let mut r = row![icons::view(Glyph::Pin, 16.0, t::INK2)].spacing(8).align_y(Alignment::Center);
        for c in pins.into_iter().flatten() {
            r = r.push(c);
        }
        content = content.push(r.push(text("These two are in one photo, so they are pinned: one stays, one splits off, and neither moves.").size(t::T_SMALL).color(t::INK2)));
    }
    let strip = cols::pick_strip(&look, ["Move to Stays".to_string(), "Move to Splits off".to_string()], None);
    let mut body = column![].spacing(0);
    if let Some(strip) = strip {
        body = body.push(strip);
    }
    content = content.push(body.push(columns));
    let counter = text(format!("{} stay · {} split off", util::thousands(n[0] as u64), util::thousands(n[1] as u64))).size(t::T_BODY).font(paint::MEDIUM).color(t::INK);
    let moved = a.moved();
    let mut status = row![counter].spacing(t::S3).align_y(Alignment::Center);
    if moved > 0 {
        status = status.push(kit::caption(format!("{} moved by hand", util::plural(moved as u64, "head"))));
    }
    status = status.push(Space::new().width(Length::Fill)).push(cols::keys(&[("Shift+click", "Pick a run"), ("← →", "Send the picked over"), ("Ctrl+Z", "Take back"), ("Enter", "Split off"), ("Esc", "Cancel")]));
    content = content.push(status);
    let go = (n[0] > 0 && n[1] > 0).then(|| m(Msg::ProposeGo));
    content = content.push(super::dialogs::footer(
        vec![kit::btn("Swap sides", style::ghost(), Some(m(Msg::ProposeSwap))).into()],
        vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::ProposeCancel))).into(), kit::btn(format!("Split off {}", util::plural(n[1] as u64, "head")), style::primary(), go).into()],
    ));
    content = content.push(kit::caption("Ctrl+Z on the page undoes the split. The new group shows on the Fursuits tab, to name or merge."));
    Some(super::dialogs::backdrop(super::dialogs::frame_fit(content.into(), 1180f32.min(app.window_size_width() - 48.0))))
}

/// The groups the drop tray offers (the test driver's names for them).
pub fn tray_ids(app: &App, p: &Person) -> Vec<GroupId> {
    tray_groups(app, p)
}

/// The groups the drop tray offers: the page's similar groups, then the
/// named, then the largest to name; never the page's own, never another
/// kind.
fn tray_groups(app: &App, p: &Person) -> Vec<GroupId> {
    let s = &app.subjects;
    let mut out: Vec<GroupId> = Vec::new();
    if let Some((g, list)) = &s.lists.similar {
        if *g == p.group {
            out.extend(list.iter().map(|x| if x.a == p.group { x.b.clone() } else { x.a.clone() }));
        }
    }
    let a = subj::arrange(&s.all, Kind::Fursuit, app.settings.people_min_photos());
    out.extend(a.named.iter().chain(a.to_name.iter()).map(|g| g.id.clone()));
    let mut seen = HashSet::new();
    out.into_iter().filter(|id| *id != p.group && s.group(id).is_some_and(|g| g.kind == Kind::Fursuit) && seen.insert(id.clone())).take(TRAY_N).collect()
}

/// While heads are dragged: a tray of groups to drop them on, and *A new
/// group*.
pub fn tray<'a>(app: &'a App, p: &'a Person) -> Option<Element<'a, Message>> {
    let d = p.head_drag.as_ref()?;
    let s = &app.subjects;
    let shape = subj::shape(Kind::Fursuit);
    let target = |id: Option<GroupId>| -> Element<'a, Message> {
        let over = d.over.as_ref() == Some(&id);
        let (lead, label): (Element<'a, Message>, String) = match &id {
            None => (icons::view(Glyph::Plus, 18.0, t::INK2), "A new group".to_string()),
            Some(g) => {
                let group = s.group(g);
                let crop = tile::crop(group.and_then(|x| s.crops.get(&x.cover.face, subj::px(app, TRAY), Framing::Face)), TRAY, shape);
                (crop, group.and_then(|x| x.name.clone()).unwrap_or_else(|| format!("Unnamed, {}", util::plural(group.map_or(0, |x| x.photos) as u64, "photo"))))
            }
        };
        let chip = container(row![lead, text(label).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK).wrapping(text::Wrapping::None)].spacing(8).align_y(Alignment::Center))
            .padding(Padding::from([4.0, 8.0]))
            .style(move |_| container::Style {
                background: Some(Background::Color(if over { alpha(t::SIGNAL, 0.18) } else { t::RAISED })),
                border: style::border(if over { t::SIGNAL } else { t::LINE_HI }, if over { 2.0 } else { 1.0 }, t::R_CONTROL),
                ..Default::default()
            });
        let messages = crate::ui::drag::DragMessages { start: m(Msg::Nothing), enter: m(Msg::DragEnter(id.clone())), leave: m(Msg::DragLeave(id.clone())), drop: m(Msg::Nothing) };
        crate::ui::drag::DragArea::new(chip, messages, true).into()
    };
    let mut r = row![text(format!("Move {} to", util::plural(d.faces.len() as u64, "head"))).size(t::T_SMALL).color(t::INK2), target(None)].spacing(8).align_y(Alignment::Center);
    for id in tray_groups(app, p) {
        r = r.push(target(Some(id)));
    }
    let panel = crate::ui::motion::Appear::new(container(r.wrap().vertical_spacing(6)).padding(Padding::from([8.0, 12.0])).max_width(app.window_size_width() - 120.0).style(style::floating())).rise(6.0);
    let y = app.window_size_height() - 190.0;
    Some(pin(iced::widget::opaque(panel)).position(Point::new(24.0, y.max(120.0))).width(Length::Fill).height(Length::Fill).into())
}

/// The drag messages of a head tile on *Heads*.
pub fn head_drag(f: FaceRef) -> crate::ui::drag::DragMessages<Message> {
    crate::ui::drag::DragMessages { start: m(Msg::DragStart(f)), enter: m(Msg::Nothing), leave: m(Msg::Nothing), drop: m(Msg::DragDrop) }
}

pub fn debug_state(p: &Person) -> String {
    let c = p.check.as_ref().map(|c| format!("rank={} marked={} pairs={} split={:?}", c.rank.len(), c.marked.len(), c.pairs.len(), c.split.as_ref().map(|(a, b)| (a.len(), b.len()))));
    let a = p.proposal.as_ref().map(|a| format!("[stay={} off={} pinned={} picked={:?} moved={} ranked={:?} from_pick={}]", a.sides[0].len(), a.sides[1].len(), a.cols.pinned.len(), [a.cols.picked[0].len(), a.cols.picked[1].len()], a.moved(), [a.rank[0].len(), a.rank[1].len()], a.from_pick));
    format!("split=[order={:?} check={} picked={} proposal={} drag={:?}]", p.order, c.unwrap_or_else(|| "none".into()), p.picked.len(), a.unwrap_or_else(|| "none".into()), p.head_drag.as_ref().map(|d| (d.faces.len(), d.over.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> FaceRef {
        FaceRef::new(crate::search::index::Key([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }

    /// The two heads from one photo stay pinned, one on each side, however
    /// heads are dragged, sent or swapped; Ctrl+Z takes back a change.
    #[test]
    fn the_pinned_heads_never_cross_and_stay_apart() {
        let (pa, pb) = (h(1), h(2));
        let mut a = Arrange::new(vec![pa, h(3), h(4)], vec![pb, h(5)], Some((pa, pb)));
        let order = |a: &Arrange| [a.ordered(0), a.ordered(1)];
        // A drag of the pinned head does nothing; a run leaves it out.
        let o = order(&a);
        assert_eq!(a.cols.update(ColMsg::DragStart(0, pa), false, |s| o[s].clone()), None);
        assert!(a.cols.drag.is_none());
        a.cols.pick(0, pa, false, &o[0]);
        a.cols.pick(0, h(4), true, &o[0]);
        assert!(!a.cols.picked[0].contains(&pa));
        // Even a move made up to carry it moves only the rest.
        assert!(a.apply(&cols::Move { from: 0, faces: vec![pa, h(3)], to: cols::Target::Side(1) }));
        assert!(a.sides[0].contains(&pa) && a.sides[1].contains(&pb) && a.sides[1].contains(&h(3)));
        assert!(!a.apply(&cols::Move { from: 1, faces: vec![pb], to: cols::Target::Side(0) }), "the other pin neither");
        assert_eq!(a.moved(), 1);
        a.swap();
        assert!(a.sides[1].contains(&pa) && a.sides[0].contains(&pb), "a swap keeps them apart");
        assert_eq!(a.moved(), 1, "a swap is no move by hand");
        assert!(a.take_back());
        assert!(a.sides[0].contains(&pa) && a.sides[1].contains(&pb));
        assert!(a.take_back());
        assert_eq!(a.sides, [vec![pa, h(3), h(4)], vec![pb, h(5)]], "back as proposed");
        assert_eq!(a.moved(), 0);
        assert!(!a.take_back());
    }

    /// *Split off* moves the final side splitting off, once (heads that
    /// left the group meanwhile left out); never with a side empty.
    /// Cancelling is dropping the arrangement: nothing was asked of the
    /// library while arranging (`apply` only changes the two lists).
    #[test]
    fn confirm_moves_the_final_arrangement_once_and_never_an_empty_side() {
        let mut a = Arrange::new(vec![h(3), h(4), h(6)], vec![h(5)], None);
        assert!(a.from_pick);
        a.apply(&cols::Move { from: 0, faces: vec![h(4), h(6)], to: cols::Target::Side(1) });
        assert_eq!(a.confirm(|_| true), Some(vec![h(4), h(6), h(5)]));
        assert_eq!(a.confirm(|f| *f != h(6)), Some(vec![h(4), h(5)]), "a head gone from the group meanwhile");
        a.apply(&cols::Move { from: 0, faces: vec![h(3)], to: cols::Target::Side(1) });
        assert_eq!(a.confirm(|_| true), None, "nothing would stay");
        a.apply(&cols::Move { from: 1, faces: vec![h(3), h(4), h(5), h(6)], to: cols::Target::Side(0) });
        assert_eq!(a.confirm(|_| true), None, "nothing would split off");
        assert_eq!(a.moved(), 1, "h(5) is the one on another side than it started");
    }

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
