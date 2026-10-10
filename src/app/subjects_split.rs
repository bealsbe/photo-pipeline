//! Splitting a fursuit group that holds more than one suit (stage 3), on
//! a fursuit's page:
//!
//! - **Least like the rest first.**  *Heads* can show the heads farthest
//!   from the group first, the few farthest marked: where its strays are
//!   (`search::splitting`, measured on the labels).  Unnamed groups open
//!   in that order, named ones clearest first; either switches.
//! - **Picking a run**: Shift+click picks every head between the last one
//!   clicked and this one, in the order shown.
//! - **Split into a new group**, in the pick strip; or drag the picked
//!   heads onto a group in the tray that shows while dragging (or *A new
//!   group*).
//! - **Two heads in one photo**: a group holding both holds at least two
//!   fursuits.  A strip says so with the two heads, and *Propose a split…*
//!   shows the group split around them, for the user to confirm.
//!
//! Nothing moves without the user's click or drop; a split is the existing
//! move (one undo step).  Fursuits only; heads the viewer's labels keep
//! out are in none of it (the engine leaves them out).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::widget::{column, container, pin, row, scrollable, text};
use iced::{Alignment, Background, Element, Length, Padding, Point, Task};

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

/// The proposal dialog's crops, and how many of each side it shows.
const PROPOSE: f32 = 56.0;
const PROPOSE_N: usize = 18;
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

/// The proposal dialog: which side leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// The side with the pair's second head leaves (else the first's).
    pub second_leaves: bool,
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
    if p.check.as_ref().is_some_and(|c| c.split.is_none()) {
        p.proposal = None;
    }
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
            let faces = std::mem::take(&mut p.picked);
            split_off(app, faces, Via::Hand(At::Heads))
        }
        Msg::Propose => {
            if let Some(p) = app.subjects.person.as_mut().filter(|p| p.check.as_ref().is_some_and(|c| c.split.is_some())) {
                // The smaller side leaves, to start.
                let second = p.check.as_ref().and_then(|c| c.split.as_ref()).is_some_and(|(a, b)| b.len() <= a.len());
                p.proposal = Some(Proposal { second_leaves: second });
            }
            subj::refresh_crops(app)
        }
        Msg::ProposeSwap => {
            if let Some(pr) = app.subjects.person.as_mut().and_then(|p| p.proposal.as_mut()) {
                pr.second_leaves = !pr.second_leaves;
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
            let Some(pr) = p.proposal.take() else { return Task::none() };
            let Some((a, b)) = p.check.as_ref().and_then(|c| c.split.clone()) else { return Task::none() };
            let leaving = if pr.second_leaves { b } else { a };
            split_off(app, leaving, Via::Proposal)
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

/// Esc while dragging heads or proposing: put it away.
pub fn escape(app: &mut App) -> bool {
    let Some(p) = app.subjects.person.as_mut() else { return false };
    p.head_drag.take().is_some() || p.proposal.take().is_some()
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
        if p.proposal.is_some() {
            if let Some((a, b)) = &c.split {
                v.extend(a.iter().take(PROPOSE_N).chain(b.iter().take(PROPOSE_N)).filter_map(|f| faces.get(f)).map(|f| ((*f).clone(), subj::px(app, PROPOSE), Framing::Face)));
            }
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

/// The proposed split, for the user to confirm: what stays, what leaves
/// into a new group.
pub fn proposal_dialog(app: &App) -> Option<Element<'_, Message>> {
    let s = &app.subjects;
    let p = s.person.as_ref()?;
    let pr = p.proposal?;
    let (a, b) = p.check.as_ref()?.split.as_ref()?;
    let (stay, leave) = if pr.second_leaves { (a, b) } else { (b, a) };
    let faces: HashMap<FaceRef, &Face> = p.faces.iter().flat_map(|f| f.iter()).map(|f| (f.face, f)).collect();
    let shape = subj::shape(Kind::Fursuit);
    let side = |title: String, heads: &[FaceRef], tone: iced::Color| -> Element<'_, Message> {
        let mut grid = row![].spacing(6);
        for f in heads.iter().take(PROPOSE_N) {
            let handle = faces.get(f).and_then(|x| s.crops.get(&x.face, subj::px(app, PROPOSE), Framing::Face));
            grid = grid.push(tile::crop(handle, PROPOSE, shape));
        }
        let more = heads.len().saturating_sub(PROPOSE_N);
        let mut col = column![text(title).size(t::T_BODY).font(paint::MEDIUM).color(tone), container(grid.wrap().vertical_spacing(6)).width(Length::Fill)].spacing(8).width(Length::Fill);
        if more > 0 {
            col = col.push(kit::caption(format!("and {} more", util::thousands(more as u64))));
        }
        container(col).padding(10).width(Length::Fill).style(|_| container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(t::LINE, 1.0, t::R_CELL), ..Default::default() }).into()
    };
    let name = subj::label(s, &p.group);
    let stays = side(format!("Stay in {} ({})", if name == "the unnamed group" { "this group".to_string() } else { name.clone() }, util::plural(stay.len() as u64, "head")), stay, t::INK);
    let leaves = side(format!("Split off into a new group ({})", util::plural(leave.len() as u64, "head")), leave, t::INK);
    let body = scrollable(column![stays, leaves].spacing(t::S2)).style(style::scroll).height(Length::Shrink);
    let content = column![
        super::dialogs::header("Split this group in two?", None),
        kit::secondary("Two of its heads are in one photo, so they are two fursuits. Each head goes with the one of the two it looks more like. Check it: a head on the wrong side can be moved later, from Heads."),
        container(body).max_height(app.window_size_height() * 0.55),
        kit::caption("Ctrl+Z undoes it. The new group shows on the Fursuits tab, to name or merge."),
        super::dialogs::footer(
            vec![kit::btn("Swap sides", style::ghost(), Some(m(Msg::ProposeSwap))).into()],
            vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::ProposeCancel))).into(), kit::btn(format!("Split off {}", util::plural(leave.len() as u64, "head")), style::primary(), Some(m(Msg::ProposeGo))).into()],
        ),
    ]
    .spacing(t::S3);
    Some(super::dialogs::backdrop(super::dialogs::frame_fit(content.into(), 640f32.min(app.window_size_width() - 48.0))))
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
    format!("split=[order={:?} check={} picked={} proposal={:?} drag={:?}]", p.order, c.unwrap_or_else(|| "none".into()), p.picked.len(), p.proposal, p.head_drag.as_ref().map(|d| (d.faces.len(), d.over.clone())))
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
