//! Two groups side by side (people or fursuits), to move faces or heads
//! between them by drag or key: the user, splitting groups, "should be able
//! to drag faces between one and another".
//!
//! - **Opened** from the tab (two groups picked, then C), a group's page
//!   (⋯ ▸ *Side by side with…*, or *Side by side* on a similar group), or a
//!   pair in "Same fursuit?" (C).
//! - **Two columns**, each a group's faces least like the rest first (the
//!   few farthest from the group marked), so strays come first.
//! - **Picking**: a click picks (or puts back), Shift+click picks a run.
//! - **Moving**: drag a face (the picked ones with it) onto the other
//!   column or onto *New group*; or ← / → move the picked ones to that side,
//!   N into a new group.  Each move is the existing move, one undo step;
//!   names stay with their groups.  Esc puts the picks away, then leaves.
//! - **Never across kinds**: both groups are of one kind, and a move to
//!   another kind is refused.  Faces the viewer's labels keep out are not
//!   shown (the engine leaves them out).
//!
//! A move into an automatic group makes a subject of it (a new id); each
//! side follows its faces (`Engine::locate`).

use std::collections::HashMap;
use std::sync::Arc;

use iced::keyboard::{self, Key};
use iced::widget::{Space, column, container, row, text};
use iced::{Alignment, Element, Length, Padding, Task};

use super::columns::{self as cols, ColMsg, Columns, Look, Target};
use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, GroupId, Kind, KindWords};
use super::style;
use super::subjects::{self as subj, m as sm};
use super::{App, Message};
use crate::ui::icons::Glyph;
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme as t;
use crate::util;

pub fn m(msg: Msg) -> Message {
    sm(subj::Msg::Pair(msg))
}

/// Each column's crops: only those on screen (or near) load.
const SECTIONS: [subj::Section; 2] = [subj::Section::Column(0), subj::Section::Column(1)];

fn look(p: &PairView) -> Look<'_> {
    Look { kind: p.kind, cols: &p.cols, wrap, sections: SECTIONS }
}

/// Faces shown per column, at most (more wait for *Show more*).
const SHOWN: usize = 240;
/// Faces each side remembers to find its group again after a move.
const ANCHORS: usize = 6;
/// Times a side looks for its faces before giving up (an undo's
/// automatic groups come back only when the worker's regroup lands).
const TRIES: u8 = 4;

pub struct PairView {
    pub kind: Kind,
    pub ids: [GroupId; 2],
    pub faces: [Option<Arc<Vec<Face>>>; 2],
    faces_seq: [u64; 2],
    /// Each face's place from the far end of its group.
    pub rank: [HashMap<FaceRef, usize>; 2],
    rank_seq: [u64; 2],
    /// The picks and the drag.
    pub cols: Columns,
    /// A few faces of each side, to find its group again.
    anchors: [Vec<FaceRef>; 2],
    locate_seq: [u64; 2],
    /// Looks for each side's faces that found no group yet.
    tries: [u8; 2],
}

impl PairView {
    fn new(kind: Kind, a: GroupId, b: GroupId) -> PairView {
        PairView {
            kind,
            ids: [a, b],
            faces: [None, None],
            faces_seq: [0, 0],
            rank: [HashMap::new(), HashMap::new()],
            rank_seq: [0, 0],
            cols: Columns::new(true),
            anchors: [Vec::new(), Vec::new()],
            locate_seq: [0, 0],
            tries: [0, 0],
        }
    }

    /// A side's faces in the order shown: least like the rest first.
    pub fn ordered(&self, side: usize) -> Vec<&Face> {
        let Some(faces) = &self.faces[side] else { return Vec::new() };
        let mut v: Vec<&Face> = faces.iter().filter(|f| self.rank[side].is_empty() || self.rank[side].contains_key(&f.face)).collect();
        v.sort_by_key(|f| self.rank[side].get(&f.face).copied().unwrap_or(usize::MAX));
        v
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Show two groups side by side.
    Open(GroupId, GroupId),
    Cols(ColMsg),
    Close,
}

fn wrap(c: ColMsg) -> Message {
    m(Msg::Cols(c))
}

// ── opening ─────────────────────────────────────────────────────────────────

/// Open `a` and `b` side by side: groups of one kind only.
pub fn open(app: &mut App, a: GroupId, b: GroupId) -> Task<Message> {
    let (Some(ga), Some(gb)) = (app.subjects.group(&a), app.subjects.group(&b)) else { return Task::none() };
    if ga.kind != gb.kind {
        return app.show_status("People and fursuits are never shown together.", 5000);
    }
    if a == b {
        return Task::none();
    }
    let kind = ga.kind;
    app.subjects.pair = Some(PairView::new(kind, a, b));
    app.quicklook = None;
    for side in 0..2 {
        ask(app, side);
    }
    subj::refresh_crops(app)
}

/// C on the page: the two groups picked on the tab, the pair shown in
/// "Same fursuit?", or on a group's page the list to choose the other from.
pub fn compare_key(app: &mut App) -> Task<Message> {
    match app.subjects.page() {
        subj::Page::Overview => {
            let s = &app.subjects;
            let picked: Vec<GroupId> = s.picked.iter().filter(|id| s.group(id).is_some_and(|g| g.kind == s.tab)).cloned().collect();
            match picked.as_slice() {
                [a, b] => open(app, a.clone(), b.clone()),
                _ => app.show_status("Pick two groups (S, then click them), then C shows them side by side.", 4000),
            }
        }
        subj::Page::Merges => match super::subjects_merge::current_pair(app) {
            Some((a, b)) => open(app, a, b),
            None => Task::none(),
        },
        subj::Page::Person => super::subjects_person::open_side_by_side_menu(app),
        _ => Task::none(),
    }
}

fn ask(app: &mut App, side: usize) {
    let (seq_f, seq_r) = (app.subjects.next_seq(), app.subjects.next_seq());
    let Some(p) = app.subjects.pair.as_mut() else { return };
    p.faces_seq[side] = seq_f;
    p.rank_seq[side] = seq_r;
    let id = p.ids[side].clone();
    app.search.link.faces(seq_f, &id);
    app.search.link.group_check(seq_r, &id);
}

// ── events ──────────────────────────────────────────────────────────────────

/// A group's faces arrived: one of the sides'.
pub fn on_faces(app: &mut App, seq: u64, faces: &Arc<Vec<Face>>) -> bool {
    let Some(p) = app.subjects.pair.as_mut() else { return false };
    let Some(side) = (0..2).find(|&s| p.faces_seq[s] == seq) else { return false };
    p.cols.keep_only(side, |f| faces.iter().any(|x| x.face == *f));
    p.anchors[side] = faces.iter().take(ANCHORS).map(|f| f.face).collect();
    p.faces[side] = Some(faces.clone());
    true
}

/// A side's order arrived (the engine's check).
pub fn on_check(app: &mut App, seq: u64, far: &[(FaceRef, f32)]) -> bool {
    let Some(p) = app.subjects.pair.as_mut() else { return false };
    let Some(side) = (0..2).find(|&s| p.rank_seq[s] == seq) else { return false };
    p.rank[side] = far.iter().enumerate().map(|(i, (f, _))| (*f, i)).collect();
    true
}

/// Where each side's faces are now: a side whose group got a new id (a
/// move made a subject of it) follows them.
pub fn on_located(app: &mut App, seq: u64, groups: &[Option<GroupId>]) -> Task<Message> {
    let Some(p) = app.subjects.pair.as_mut() else { return Task::none() };
    let Some(side) = (0..2).find(|&s| p.locate_seq[s] == seq) else { return Task::none() };
    p.locate_seq[side] = 0;
    match follow(groups, &p.ids[1 - side]) {
        Follow::To(id) => {
            p.ids[side] = id;
            p.tries[side] = 0;
            ask(app, side);
            subj::refresh_crops(app)
        }
        // In no group yet: an undo's automatic groups come back when the
        // worker's regroup lands, and its publish looks again.
        Follow::Wait if p.tries[side] + 1 < TRIES => {
            p.tries[side] += 1;
            Task::none()
        }
        Follow::Wait | Follow::Gone => {
            app.subjects.pair = None;
            app.show_status("One of the two groups is gone (its faces were all moved out).", 5000)
        }
    }
}

#[derive(Debug, PartialEq)]
enum Follow {
    /// The group most of a side's remembered faces are in now.
    To(GroupId),
    /// None of them is in a group (yet).
    Wait,
    /// None left, or all in the other side's group: this group is gone.
    Gone,
}

fn follow(groups: &[Option<GroupId>], other: &GroupId) -> Follow {
    let mut count: HashMap<&GroupId, usize> = HashMap::new();
    for g in groups.iter().flatten() {
        *count.entry(g).or_default() += 1;
    }
    // Ties go to the smaller id, so the answer never depends on hashing.
    let best = count.iter().filter(|(g, _)| **g != other).max_by(|a, b| a.1.cmp(b.1).then_with(|| super::people::short(b.0).cmp(&super::people::short(a.0))));
    match best {
        Some((g, _)) => Follow::To((*g).clone()),
        None if groups.is_empty() || !count.is_empty() => Follow::Gone,
        None => Follow::Wait,
    }
}

/// The groups changed: each side is asked again; a side whose group is
/// gone looks for its faces.
pub fn on_subjects(app: &mut App) {
    let Some(p) = app.subjects.pair.as_ref() else { return };
    let all = app.subjects.all.clone();
    let (kind, gone): (Kind, Vec<(usize, Vec<FaceRef>)>) = (p.kind, (0..2).filter(|&s| all.group(&p.ids[s]).is_none()).map(|s| (s, p.anchors[s].clone())).collect());
    if let Some(p) = app.subjects.pair.as_mut() {
        // A side looking for its group shows nothing stale meanwhile.
        for (side, _) in &gone {
            p.faces[*side] = None;
            p.rank[*side].clear();
            p.cols.clear(*side);
        }
    }
    for side in 0..2 {
        if gone.iter().any(|(s, _)| *s == side) {
            continue;
        }
        ask(app, side);
    }
    for (side, heads) in gone {
        let seq = app.subjects.next_seq();
        if let Some(p) = app.subjects.pair.as_mut() {
            p.locate_seq[side] = seq;
        }
        app.search.link.locate(seq, kind, &heads);
    }
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Open(a, b) => open(app, a, b),
        Msg::Cols(c) => {
            let shift = crate::ui::modifiers::get().shift();
            let Some(p) = app.subjects.pair.as_mut() else { return Task::none() };
            let orders = [0, 1].map(|s| p.ordered(s).iter().map(|f| f.face).collect::<Vec<_>>());
            let mv = p.cols.update(c, shift, |s| orders[s].clone());
            match mv {
                Some(cols::Move { from, faces, to }) => move_faces(app, from, faces, to),
                None => Task::none(),
            }
        }
        Msg::Close => close(app),
    }
}

/// A click on a face (the test driver's): picks it, or a run with Shift.
pub fn pick(app: &mut App, side: usize, f: FaceRef, shift: bool) {
    let Some(p) = app.subjects.pair.as_mut() else { return };
    let order: Vec<FaceRef> = p.ordered(side.min(1)).iter().map(|x| x.face).collect();
    p.cols.pick(side, f, shift, &order);
}

/// Faces of side `from` to the other side or a new group: the existing
/// move, one undo step.  Never to another kind.
fn move_faces(app: &mut App, from: usize, faces: Vec<FaceRef>, target: Target) -> Task<Message> {
    let Some(p) = app.subjects.pair.as_mut() else { return Task::none() };
    if faces.is_empty() {
        return Task::none();
    }
    let source = p.ids[from].clone();
    let to = match target {
        Target::Side(s) if s != from => Some(p.ids[s].clone()),
        Target::Side(_) => return Task::none(),
        Target::New => None,
    };
    let kind = p.kind;
    if to.as_ref().is_some_and(|t| app.subjects.group(t).is_none_or(|g| g.kind != kind)) {
        return app.show_status("People and fursuits are never mixed.", 5000);
    }
    let p = app.subjects.pair.as_mut().expect("pair");
    // Both sides are found again by their faces if their ids change.
    if let Target::Side(s) = target {
        p.anchors[s].extend(faces.iter().take(ANCHORS));
    }
    p.anchors[from].retain(|f| !faces.contains(f));
    let crops = if p.kind == Kind::Fursuit { "head" } else { "face" };
    let n = faces.len() as u64;
    app.search.link.move_faces(&source, &faces, to.as_ref());
    let whom = to.as_ref().map_or_else(|| "a new group".to_string(), |t| subj::label(&app.subjects, t));
    app.show_status_action(format!("Moved {} to {whom}.", util::plural(n, crops)), "Undo", sm(subj::Msg::Undo))
}

fn close(app: &mut App) -> Task<Message> {
    app.subjects.pair = None;
    subj::refresh_crops(app)
}

/// Esc: a drag, then the picks, then the view.
pub fn escape(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.pair.as_mut() else { return Task::none() };
    if p.cols.escape() {
        return Task::none();
    }
    close(app)
}

/// ← / → move the picked faces to that side; N into a new group.
pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    match cols::key_msg(key, mods, true) {
        Some(c) => update(app, Msg::Cols(c)),
        None => Task::none(),
    }
}

// ── crops ───────────────────────────────────────────────────────────────────

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(p) = s.pair.as_ref() else { return Vec::new() };
    let small = subj::px(app, tile::SMALL_CROP);
    let cover = subj::px(app, 44.0);
    let mut v = Vec::new();
    for side in 0..2 {
        if let Some(g) = s.group(&p.ids[side]) {
            v.push((g.cover.clone(), cover, Framing::Face));
        }
        let section = SECTIONS[side];
        v.extend(p.ordered(side).into_iter().take(SHOWN).filter(|f| s.crops.is_seen(section, &subj::Crops::key(&f.face, small, Framing::Face))).map(|f| (f.clone(), small, Framing::Face)));
    }
    v
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let Some(p) = s.pair.as_ref() else { return Space::new().into() };
    let crops = p.kind.crops().to_lowercase();
    let back = tip(kit::icon_text_btn(Glyph::ChevronLeft, "Back", t::INK2, style::secondary(), Some(m(Msg::Close))), "Leave the two groups  (Esc)", true);
    let header = row![
        back,
        column![
            text("Two groups side by side").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK),
            kit::caption(format!("Drag {crops} from one to the other, or pick them and press ← or →. Each move is one undo step."))
        ]
        .spacing(1)
        .padding(Padding { left: t::S3, ..Padding::ZERO }),
        Space::new().width(Length::Fill),
        kit::btn("Done", style::secondary(), Some(m(Msg::Close))),
    ]
    .spacing(t::S2)
    .align_y(Alignment::Center);
    let header = container(header.padding(Padding::from([0.0, t::S3]))).height(subj::HEADER_H).center_y(subj::HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT));
    let body = cols::pair(side_column(app, p, 0), side_column(app, p, 1));
    let look = look(p);
    let mut col = column![header, kit::hairline()];
    let to_other = [0, 1].map(|s| {
        let name = subj::label(&app.subjects, &p.ids[s]);
        format!("Move to {}", if name == "the unnamed group" { "the other group".to_string() } else { name })
    });
    if let Some(strip) = cols::pick_strip(&look, to_other, Some((m(Msg::Close), "Leave  (Esc)"))) {
        col = col.push(strip);
    }
    col = col.push(container(body).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)));
    col = col.push(cols::new_group_target(&look));
    col = col.push(container(cols::keys(&[("Click", "Pick"), ("Shift+click", "Pick a run"), ("← →", "Move the picked to that side"), ("N", "New group"), ("Ctrl+Z", "Undo"), ("Esc", "Done")])).padding(Padding::from([7.0, 20.0])).width(Length::Fill).style(style::fill(t::PHOTO_GROUND)));
    col.width(Length::Fill).height(Length::Fill).into()
}

fn side_column<'a>(app: &'a App, p: &'a PairView, side: usize) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(p.kind);
    let g = s.group(&p.ids[side]);
    let cover = tile::crop(g.and_then(|g| s.crops.get(&g.cover.face, subj::px(app, 44.0), Framing::Face)), 44.0, shape);
    let name = g.and_then(|g| g.name.clone());
    let counts = g.map(|g| format!("{} over {}", util::plural(g.photos as u64, "photo"), util::plural(g.days as u64, "day"))).unwrap_or_default();
    let head = row![
        cover,
        column![
            text(name.clone().unwrap_or_else(|| "Unnamed group".into())).size(t::T_TITLE).font(paint::SEMIBOLD).color(if name.is_some() { t::PHOTO_INK } else { t::PHOTO_INK2 }),
            text(format!("{counts}. Least like the rest first.")).size(t::T_SMALL).color(t::PHOTO_INK3)
        ]
        .spacing(2)
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let order = p.ordered(side);
    let marked = crate::search::splitting::marked(order.len());
    let more = order.len().saturating_sub(SHOWN);
    let shown: Vec<&Face> = order.into_iter().take(SHOWN).collect();
    let look = look(p);
    let tiles = cols::grid(app, &look, side, &shown, marked);
    let foot: Option<Element<'a, Message>> = if more > 0 {
        Some(kit::caption(format!("and {} more", util::thousands(more as u64))).into())
    } else if p.faces[side].is_none() {
        Some(text("Reading…").size(t::T_SMALL).color(t::PHOTO_INK3).into())
    } else {
        None
    };
    cols::side(&look, side, head.into(), tiles, foot)
}

pub fn debug_state(app: &App) -> String {
    let Some(p) = app.subjects.pair.as_ref() else { return "pair=none".into() };
    format!(
        "pair=[kind={:?} ids={:?} faces={:?} ranked={:?} picked={:?} drag={:?}]",
        p.kind,
        p.ids.iter().map(super::people::short).collect::<Vec<_>>(),
        p.faces.iter().map(|f| f.as_ref().map(|v| v.len())).collect::<Vec<_>>(),
        [p.rank[0].len(), p.rank[1].len()],
        [p.cols.picked[0].len(), p.cols.picked[1].len()],
        p.cols.drag.as_ref().map(|d| (d.from, d.faces.len(), d.over))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key;
    use chrono::NaiveDate;

    fn face(n: u8) -> Face {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        Face { face: FaceRef { key: Key([n; 16]), bbox: [0.4, 0.2, 0.1, 0.1] }, jpg: None, raw: None, day, capture: day.and_hms_opt(10, 0, n as u32).unwrap(), burst: None, clarity: 0.5, confirmed: false }
    }

    /// A side follows its faces to their group; it waits while they are in
    /// none (an undo's groups come back with the regroup), and is gone when
    /// none is left or all went to the other side.
    #[test]
    fn a_side_follows_its_faces_waits_for_a_regroup_or_is_gone() {
        let (a, b, other) = (GroupId::Cluster(5), GroupId::Cluster(6), GroupId::Cluster(9));
        assert_eq!(follow(&[Some(a.clone()), Some(b.clone()), Some(b.clone()), None], &other), Follow::To(b.clone()));
        assert_eq!(follow(&[Some(a.clone()), Some(b.clone())], &other), Follow::To(a.clone()), "a tie, the same answer every time");
        assert_eq!(follow(&[Some(other.clone()), Some(a.clone())], &other), Follow::To(a));
        assert_eq!(follow(&[None, None], &other), Follow::Wait);
        assert_eq!(follow(&[Some(other.clone()), None], &other), Follow::Gone, "merged into the other side");
        assert_eq!(follow(&[], &other), Follow::Gone);
    }

    /// Each column shows its faces least like the rest first (the engine's
    /// order); faces the order doesn't hold (kept out by the viewer's
    /// labels) aren't shown.
    #[test]
    fn a_side_shows_least_alike_first_and_leaves_out_what_the_check_left_out() {
        let mut p = PairView::new(Kind::Fursuit, GroupId::Cluster(1), GroupId::Cluster(2));
        p.faces[0] = Some(Arc::new(vec![face(1), face(2), face(3)]));
        assert_eq!(p.ordered(0).len(), 3, "before the order arrives, every face");
        p.rank[0] = [(face(3).face, 0), (face(1).face, 1)].into_iter().collect();
        let shown: Vec<FaceRef> = p.ordered(0).iter().map(|f| f.face).collect();
        assert_eq!(shown, [face(3).face, face(1).face], "the farthest first; face 2 left out");
        assert!(p.ordered(1).is_empty());
    }
}
