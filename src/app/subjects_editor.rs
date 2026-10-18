//! The group editor: one place to sort a group's faces (or fursuit heads),
//! people and fursuits alike, never the two mixed.
//!
//! - **Columns.**  The group being edited (*Stays*), any other group opened
//!   beside it (side by side, or from the panel), and any number of new
//!   groups (*+ New group*, or a drop on it).  Each least like the rest
//!   first, the few farthest marked; each scrolls, and only the tiles on
//!   screen load their crops.
//! - **The panel.**  The kind's other groups: named ones nearest first (a
//!   search box filters by name), then the largest unnamed.  A drop on one
//!   sends heads there (its count shows the change); *Open* shows it as a
//!   column.  Under them: *Take out of this group* (people: *Not this
//!   person*), and *Not fursuit heads* (*Not faces*).
//! - **Moving.**  Drag a face (the picked ones with it); Shift+click picks
//!   a run; ← / → to the column beside, N a new group, R take out, Delete
//!   not heads.  Ctrl+Z takes back the last step here.
//! - **Two heads of one photo never share a column** (nor a group in the
//!   panel holding the other): such a drop is refused with a short note.
//! - **Names.**  A new column, and the group being edited, are named in
//!   their headers; a name that is taken offers *Merge into …* instead.
//! - **Nothing changes in the library until *Apply***, which makes the
//!   whole arrangement ONE undo step on the engine (`GroupEdit`, by head,
//!   so it applies to the groups as they are then, a regroup or not).
//!   *Cancel* or Esc leaves everything as it was.
//!
//! What opens it (each with its preset): *Edit group* (E) on a group's
//! page or a picked tile; side by side (C with two picked, a pair in "Same
//! fursuit?"); the picked heads' *Split into a new group* and *Move to…*.
//!
//! The group's suggestions live here, and only here: the tab and the
//! group's page show the group, not what might be done to it.
//!
//! **A suggested split.**  A fursuit group holding two heads of one photo
//! holds two fursuits: the header offers *Suggested split* (the two heads,
//! small).  It is offered, never applied: a click lays it out as a new
//! column (the smaller side around the pair, the pair apart), taking only
//! heads still in *Stays*, so nothing the user arranged is undone; Ctrl+Z
//! takes it back.  Applied, it is recorded `by: "proposal"`.
//!
//! **Likely the same fursuit.**  At the top of the panel, the groups the
//! engine pairs with this one (`search::merging`, asked as a fursuit
//! page's *Similar groups* were: by distance, under the cut-offs in force,
//! "not the same" left out), nearest first.  Each is a drop target like
//! the panel's other rows; a click on it opens it as a column.  *Merge*
//! merges the two at once, as one undo step on the engine (not part of the
//! arrangement), recorded as a suggested pair (`Via::Pair`, `At::Similar`,
//! with its distance); two named ask which name to keep; the editor then
//! shows the group kept.  × says they are different ("not the same", one
//! undo step).  The third button opens "Same fursuit?" at that pair, and
//! Esc there comes back here.  *Merge* and the review wait while an
//! arrangement isn't applied: a merge is a change of its own.  Ctrl+Z here
//! takes back the arrangement's steps first, then the merges and "not the
//! same" made here.  Fursuits only: kinds never mix.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Background, Element, Length, Padding, Task};

use super::columns::{self as cols, ColMsg, Columns, Look, Target};
use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, Group, GroupId, Kind, KindWords, Subjects as Found};
use super::style;
use super::subjects::{self as subj, MergeAsk, m as sm};
use super::{App, Message};
use crate::model::decisions::{At, Via};
use crate::search::merging::GroupPair;
use crate::model::subjects::{EditDest, EditTarget, GroupEdit};
use crate::search::index::Key as PhotoKey;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme::{self as t, alpha};
use crate::util;

pub fn m(msg: Msg) -> Message {
    sm(subj::Msg::Editor(msg))
}

fn wrap(c: ColMsg) -> Message {
    m(Msg::Cols(c))
}

/// The panel's width, and its covers.
const PANEL_W: f32 = 300.0;
const COVER: f32 = 34.0;
/// Unnamed groups the panel offers (the largest).
const UNNAMED_N: usize = 12;
/// Named groups whose heads are read when the panel opens, with the
/// unnamed ones shown (for the photo pair check; others are read when a
/// drag first goes over them, or a move to them waits for it).
const READ_N: usize = 12;
/// The suggested split's two heads, in the header.
const SUGGEST: f32 = 22.0;
/// A column at its narrowest.
const COL_MIN: f32 = 250.0;
/// Heads of an automatic group remembered to find it at *Apply*.
const ANCHORS: usize = 8;
/// *Likely the same fursuit*: the nearest few shown (the rest are in the
/// review).
const PAIRS_N: usize = 5;

/// A change made to the library from the editor (not part of the
/// arrangement), for Ctrl+Z here.
#[derive(Debug, Clone, PartialEq)]
pub enum LibStep {
    /// A pair merged: the group edited before, to come back to (and its
    /// cover, should its id change back).
    Merged { back_to: GroupId, cover: FaceRef },
    /// "Not the same" on a pair.
    NotSame,
}

/// A change sent to the engine that the editor follows: once the groups
/// change, it shows the group it is on now.
#[derive(Debug, Clone, PartialEq)]
pub struct Follow {
    /// The group to show: a merge's kept one, or (an undo) the one edited
    /// before.
    pub want: GroupId,
    /// A merge has landed once this group is gone (`None`: an undo, which
    /// lands with the next change).
    pub gone: Option<GroupId>,
    /// The covers the group shown may have, should its id change (a merge
    /// of two unnamed groups makes a new one, its cover the clearer).
    pub covers: Vec<FaceRef>,
    /// What Ctrl+Z takes back afterwards.
    pub step: Option<LibStep>,
}

/// Where a followed change has left the group.
#[derive(Debug, Clone, PartialEq)]
pub enum Followed {
    Waiting,
    At(GroupId),
    /// Nowhere to be found (changed again meanwhile).
    Lost,
}

impl Follow {
    /// Where the change has left the group, as `all` has the groups now.
    pub fn resolve(&self, all: &Found, kind: Kind) -> Followed {
        if self.gone.as_ref().is_some_and(|g| all.group(g).is_some()) {
            return Followed::Waiting;
        }
        if all.group(&self.want).is_some_and(|g| g.kind == kind) {
            return Followed::At(self.want.clone());
        }
        // Its id changed: the group of the kind whose cover is one of the
        // covers (a named one first, then the largest).
        all.groups.iter().filter(|g| g.kind == kind && self.covers.contains(&g.cover.face)).max_by_key(|g| (g.name.is_some(), g.photos)).map_or(Followed::Lost, |g| Followed::At(g.id.clone()))
    }
}

/// What *Merge* on a pair does.
#[derive(Debug, Clone, PartialEq)]
pub enum MergePlan {
    /// Both named: the merge dialog asks which name to keep.
    Ask,
    /// One merge, one undo step, recorded as the suggested pair it was.
    Merge { into: GroupId, gone: GroupId, via: Via },
}

/// The group edited is kept, unless only the other is named (as a
/// fursuit page's *Similar groups* did); a named one keeps its name.
pub fn merge_plan(home: &Group, other: &Group, pair: &GroupPair) -> MergePlan {
    let via = super::subjects_merge::via(pair, At::Similar);
    match (home.name.is_some(), other.name.is_some()) {
        (true, true) => MergePlan::Ask,
        (false, true) => MergePlan::Merge { into: other.id.clone(), gone: home.id.clone(), via },
        _ => MergePlan::Merge { into: home.id.clone(), gone: other.id.clone(), via },
    }
}

/// What opens the editor, and how it starts.
#[derive(Debug, Clone, PartialEq)]
pub enum Preset {
    /// The group alone.
    Plain,
    /// Another group beside it (side by side).
    With(GroupId),
    /// These heads in a new column (picked heads split off).
    Split(Vec<FaceRef>),
    /// These heads picked in *Stays*, the panel ready for them (*Move to…*).
    Pick(Vec<FaceRef>),
}

/// Where a column came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    /// An existing group: its id when it opened, and its name then.
    Existing { id: GroupId, was: Option<String> },
    /// A new group.
    New,
}

#[derive(Debug, Clone)]
pub struct Col {
    pub origin: Origin,
    pub heads: Vec<FaceRef>,
    /// The name typed in its header.
    pub name: String,
    pub rank: HashMap<FaceRef, usize>,
    /// Its faces arrived (an existing group's).
    pub loaded: bool,
}

/// One step's arrangement, for Ctrl+Z.
#[derive(Debug, Clone)]
struct Arrangement {
    cols: Vec<Col>,
    sent: Vec<(GroupId, Vec<FaceRef>)>,
    removed: Vec<FaceRef>,
    taken: Vec<FaceRef>,
    start: HashMap<FaceRef, Place>,
}

/// A split the engine suggests for a group holding two heads of one
/// photo: the pair, and the side to split off (the smaller of the two
/// around them, holding `pair.1`).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub pair: (FaceRef, FaceRef),
    pub off: Vec<FaceRef>,
}

impl Suggestion {
    /// From the engine's split check: none unless two of the group's heads
    /// are in one photo and it proposes a split around them.
    pub fn from_check(pairs: &[(FaceRef, FaceRef)], split: Option<&(Vec<FaceRef>, Vec<FaceRef>)>) -> Option<Suggestion> {
        let &(a, b) = pairs.first()?;
        let (with_a, with_b) = split?;
        // The smaller side splits off, its pair head with it.
        Some(if with_b.len() <= with_a.len() { Suggestion { pair: (a, b), off: with_b.clone() } } else { Suggestion { pair: (b, a), off: with_a.clone() } })
    }
}

pub struct Editor {
    pub kind: Kind,
    pub cols: Vec<Col>,
    pub ui: Columns,
    /// Heads dropped on a group in the panel.
    pub sent: Vec<(GroupId, Vec<FaceRef>)>,
    pub removed: Vec<FaceRef>,
    pub taken: Vec<FaceRef>,
    history: Vec<Arrangement>,
    /// Every face read, by head.
    pub faces: HashMap<FaceRef, Face>,
    /// The group each head read was in when it was read.
    origin: HashMap<FaceRef, GroupId>,
    /// Where each head was once the preset was laid out (heads moved by
    /// hand are those elsewhere now).
    start: HashMap<FaceRef, Place>,
    preset: Option<Preset>,
    /// A suggested split (a fursuit group holding two heads of one photo).
    pub suggestion: Option<Suggestion>,
    check_seq: u64,
    /// The panel: every group of the kind with its distance.
    pub near: Vec<(GroupId, f32)>,
    near_seq: u64,
    pub search: String,
    /// Photos of groups in the panel (for the photo pair check).
    photos_of: HashMap<GroupId, HashSet<PhotoKey>>,
    /// Faces asked for: (seq → the group, and the column it fills).
    asked: HashMap<u64, (GroupId, Option<usize>)>,
    /// Orders asked for: seq → column.
    ordering: HashMap<u64, usize>,
    pub note: Option<String>,
    /// A move to a panel group whose heads are still being read (made
    /// when they arrive, if the photo pair check allows).
    pending: Option<cols::Move>,
    /// A named group *Apply* found gone.
    pub gone: Option<GroupId>,
    apply_seq: u64,
    /// *Likely the same fursuit*: the engine's pairs with this group.
    pub pairs: Option<Arc<Vec<GroupPair>>>,
    pairs_seq: u64,
    /// A merge (or its undo) sent, waiting for the groups to change.
    pub follow: Option<Follow>,
    /// Merges and "not the same" made here, newest last.
    pub lib_steps: Vec<LibStep>,
}

/// Where a head is in the arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Place {
    Col(usize),
    Sent(GroupId),
    Removed,
    TakenOut,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Open the editor on a group, with a preset.
    Open(GroupId, Preset),
    Cols(ColMsg),
    /// *+ New group*: an empty column.
    AddColumn,
    /// A panel group opened as a column.
    Expand(GroupId),
    /// An opened group back into the panel.
    Collapse(usize),
    Search(String),
    Name(usize, String),
    /// A taken name's offer: the column's heads into that group.
    MergeName(usize, GroupId),
    /// After a refusal: the gone group's heads into a new column.
    GoneToNew,
    /// Lay out the suggested split (a new column; nothing else moves).
    Suggest,
    /// *Likely the same fursuit*, pair `i` of `pairs`: merge them now,
    /// they are different, or the review at that pair.
    PairMerge(usize),
    PairNotSame(usize),
    PairReview(usize),
    /// Take back the last merge or "not the same" made here (the status
    /// bar's Undo after one).
    UndoLibrary,
    Apply,
    Cancel,
}

impl Editor {
    fn new(kind: Kind, home: GroupId, was: Option<String>, preset: Preset) -> Editor {
        let mut cols = vec![Col { origin: Origin::Existing { id: home, was: was.clone() }, heads: Vec::new(), name: was.unwrap_or_default(), rank: HashMap::new(), loaded: false }];
        if let Preset::Split(_) = preset {
            cols.push(Col { origin: Origin::New, heads: Vec::new(), name: String::new(), rank: HashMap::new(), loaded: true });
        }
        Editor {
            kind,
            cols,
            ui: Columns::default(),
            sent: Vec::new(),
            removed: Vec::new(),
            taken: Vec::new(),
            history: Vec::new(),
            faces: HashMap::new(),
            origin: HashMap::new(),
            start: HashMap::new(),
            preset: Some(preset),
            suggestion: None,
            check_seq: 0,
            near: Vec::new(),
            near_seq: 0,
            search: String::new(),
            photos_of: HashMap::new(),
            asked: HashMap::new(),
            ordering: HashMap::new(),
            note: None,
            pending: None,
            gone: None,
            apply_seq: 0,
            pairs: None,
            pairs_seq: 0,
            follow: None,
            lib_steps: Vec::new(),
        }
    }

    /// Anything arranged that *Apply* would change: the same test as
    /// [`Editor::to_edit`]'s moves and renames, without building them.
    pub fn changed(&self) -> bool {
        if !self.sent.is_empty() || !self.removed.is_empty() || !self.taken.is_empty() {
            return true;
        }
        self.cols.iter().any(|c| match &c.origin {
            Origin::New => !c.heads.is_empty(),
            Origin::Existing { id, was } => {
                let name = c.name.trim();
                (!name.is_empty() && Some(name) != was.as_deref()) || c.heads.iter().any(|f| self.origin.get(f).is_some_and(|o| o != id))
            }
        })
    }

    /// *Likely the same fursuit*: the pairs with this group whose other
    /// group is there now, of this kind (fursuits only), nearest first,
    /// with their place in `pairs`.
    pub fn shown_pairs<'a>(&'a self, all: &Found) -> Vec<(usize, &'a GroupId)> {
        let Some(list) = &self.pairs else { return Vec::new() };
        if self.kind != Kind::Fursuit {
            return Vec::new();
        }
        let home = self.home();
        list.iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let other = if p.a == *home { &p.b } else if p.b == *home { &p.a } else { return None };
                (other != home && all.group(other).is_some_and(|g| g.kind == self.kind)).then_some((i, other))
            })
            .collect()
    }

    /// The pairs the panel shows: the nearest few, none while a name is
    /// searched for.
    pub fn panel_pairs<'a>(&'a self, all: &Found) -> Vec<(usize, &'a GroupId)> {
        if !self.search.trim().is_empty() {
            return Vec::new();
        }
        self.shown_pairs(all).into_iter().take(PAIRS_N).collect()
    }

    /// The groups *Likely the same fursuit* shows (left out of the
    /// panel's lists under it).
    fn paired_groups(&self, all: &Found) -> HashSet<GroupId> {
        self.panel_pairs(all).into_iter().map(|(_, g)| g.clone()).collect()
    }

    pub fn home(&self) -> &GroupId {
        match &self.cols[0].origin {
            Origin::Existing { id, .. } => id,
            Origin::New => unreachable!("the first column is the group edited"),
        }
    }

    /// A column's faces in the order shown: least like the rest of it
    /// first (heads whose order isn't in yet, just moved there, first).
    pub fn ordered(&self, col: usize) -> Vec<FaceRef> {
        let Some(c) = self.cols.get(col) else { return Vec::new() };
        let mut v = c.heads.clone();
        v.sort_by_key(|f| c.rank.get(f).map_or(0, |r| r + 1));
        v
    }

    /// Where a head is now.
    pub fn place_of(&self, f: &FaceRef) -> Option<Place> {
        if let Some(i) = self.cols.iter().position(|c| c.heads.contains(f)) {
            return Some(Place::Col(i));
        }
        if let Some((g, _)) = self.sent.iter().find(|(_, v)| v.contains(f)) {
            return Some(Place::Sent(g.clone()));
        }
        if self.removed.contains(f) {
            return Some(Place::Removed);
        }
        self.taken.contains(f).then_some(Place::TakenOut)
    }

    fn snapshot(&self) -> Arrangement {
        Arrangement { cols: self.cols.clone(), sent: self.sent.clone(), removed: self.removed.clone(), taken: self.taken.clone(), start: self.start.clone() }
    }

    fn col_of_group(&self, g: &GroupId) -> Option<usize> {
        self.cols.iter().position(|c| matches!(&c.origin, Origin::Existing { id, .. } if id == g))
    }

    /// The photos already in where `to` sends heads: a column's heads, or
    /// a panel group's (what was read of it, and what was sent there).
    fn photos_at(&self, to: &Target) -> Option<HashSet<PhotoKey>> {
        match to {
            Target::Col(i) => Some(self.cols.get(*i)?.heads.iter().map(|f| f.key).collect()),
            Target::Group(g) => {
                if let Some(i) = self.col_of_group(g) {
                    return self.photos_at(&Target::Col(i));
                }
                let mut keys: HashSet<PhotoKey> = self.photos_of.get(g)?.clone();
                // Heads of it that went elsewhere in this edit don't count.
                for (f, o) in &self.origin {
                    if o == g && self.place_of(f).is_some_and(|p| p != Place::Sent(g.clone())) {
                        keys.remove(&f.key);
                    }
                }
                keys.extend(self.sent.iter().filter(|(x, _)| x == g).flat_map(|(_, v)| v.iter().map(|f| f.key)));
                Some(keys)
            }
            Target::New => Some(HashSet::new()),
            Target::TakenOut | Target::Removed => None,
        }
    }

    /// Why `faces` may not go to `to`: two heads of one photo in one column
    /// (or group).  `Err(None)`: not known yet (the group's heads are
    /// being read).
    pub fn refuse(&self, faces: &[FaceRef], to: &Target) -> Result<(), Option<String>> {
        if matches!(to, Target::TakenOut | Target::Removed) {
            return Ok(());
        }
        let Some(mut there) = self.photos_at(to) else { return Err(None) };
        // The moving heads themselves don't count against their own photo.
        let col = match to {
            Target::Col(i) => Some(*i),
            Target::Group(g) => self.col_of_group(g),
            _ => None,
        };
        for f in faces {
            if col.is_some_and(|i| self.cols[i].heads.contains(f)) {
                there.remove(&f.key);
            }
        }
        let mut moving: HashSet<PhotoKey> = HashSet::new();
        for f in faces {
            if there.contains(&f.key) || !moving.insert(f.key) {
                let noun = cols::crop_word(self.kind);
                return Err(Some(format!("Two {noun}s of one photo can't be in one group: they are two {}.", if self.kind == Kind::Fursuit { "fursuits" } else { "people" })));
            }
        }
        Ok(())
    }

    /// Make a move inside the arrangement (nothing in the library).
    pub fn apply_move(&mut self, mv: &cols::Move) -> Result<(), Option<String>> {
        self.refuse(&mv.faces, &mv.to)?;
        self.history.push(self.snapshot());
        for f in &mv.faces {
            for c in &mut self.cols {
                c.heads.retain(|x| x != f);
            }
            for (_, v) in &mut self.sent {
                v.retain(|x| x != f);
            }
            self.removed.retain(|x| x != f);
            self.taken.retain(|x| x != f);
        }
        self.sent.retain(|(_, v)| !v.is_empty());
        match &mv.to {
            Target::Col(i) => {
                let c = &mut self.cols[*i];
                let mut v = mv.faces.clone();
                v.extend(c.heads.drain(..));
                c.heads = v;
            }
            Target::New => {
                self.cols.push(Col { origin: Origin::New, heads: mv.faces.clone(), name: String::new(), rank: HashMap::new(), loaded: true });
            }
            Target::Group(g) => match self.col_of_group(g) {
                Some(i) => self.cols[i].heads.splice(0..0, mv.faces.iter().copied()).for_each(drop),
                None => match self.sent.iter_mut().find(|(x, _)| x == g) {
                    Some((_, v)) => v.extend(mv.faces.iter().copied()),
                    None => self.sent.push((g.clone(), mv.faces.clone())),
                },
            },
            Target::Removed => self.removed.extend(mv.faces.iter().copied()),
            Target::TakenOut => self.taken.extend(mv.faces.iter().copied()),
        }
        self.ui.moved(&mv.faces);
        Ok(())
    }

    /// Ctrl+Z: the last step taken back.
    pub fn take_back(&mut self) -> bool {
        let Some(a) = self.history.pop() else { return false };
        let n = a.cols.len();
        self.cols = a.cols;
        self.sent = a.sent;
        self.removed = a.removed;
        self.taken = a.taken;
        self.start = a.start;
        self.ui.renumber(|c| (c < n).then_some(c));
        true
    }

    /// The counts: (stay, move, removed, taken out), against the library.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let home = self.home().clone();
        let stay = self.cols[0].heads.len();
        let moved = self.origin.iter().filter(|(f, o)| **o == home && matches!(self.place_of(f), Some(Place::Col(i)) if i != 0) || **o == home && matches!(self.place_of(f), Some(Place::Sent(_)))).count();
        (stay, moved, self.removed.len(), self.taken.len())
    }

    /// Heads elsewhere than the preset laid them out.
    pub fn edited(&self) -> usize {
        self.start.iter().filter(|(f, p)| self.place_of(f).as_ref() != Some(*p)).count()
    }

    /// The whole arrangement as the engine's edit: each head whose place
    /// changed, by head; targets by subject id, or by their heads.
    pub fn to_edit(&self, cover_of: &dyn Fn(&GroupId) -> Option<FaceRef>) -> GroupEdit {
        let mut targets: Vec<EditTarget> = Vec::new();
        let mut index: HashMap<GroupId, usize> = HashMap::new();
        let mut target = |g: &GroupId, targets: &mut Vec<EditTarget>| -> usize {
            if let Some(&i) = index.get(g) {
                return i;
            }
            let t = match g {
                GroupId::Subject(s) => EditTarget::Subject(s.clone()),
                GroupId::Cluster(_) => {
                    let mut heads: Vec<FaceRef> = self.origin.iter().filter(|(_, o)| *o == g).map(|(f, _)| *f).take(ANCHORS).collect();
                    heads.sort_by_key(|f| f.key.0);
                    heads.extend(cover_of(g));
                    EditTarget::ClusterOf(heads)
                }
            };
            targets.push(t);
            index.insert(g.clone(), targets.len() - 1);
            targets.len() - 1
        };
        let mut moves: Vec<(FaceRef, EditDest)> = Vec::new();
        let mut new_names: Vec<Option<String>> = Vec::new();
        let mut renames: Vec<(usize, String)> = Vec::new();
        for c in &self.cols {
            match &c.origin {
                Origin::Existing { id, was } => {
                    let i = target(id, &mut targets);
                    for f in &c.heads {
                        if self.origin.get(f) != Some(id) {
                            moves.push((*f, EditDest::Group(i)));
                        }
                    }
                    let name = c.name.trim();
                    if !name.is_empty() && Some(name) != was.as_deref() {
                        renames.push((i, name.to_string()));
                    }
                }
                Origin::New => {
                    if c.heads.is_empty() {
                        continue;
                    }
                    let name = c.name.trim();
                    new_names.push((!name.is_empty()).then(|| name.to_string()));
                    let k = new_names.len() - 1;
                    moves.extend(c.heads.iter().map(|f| (*f, EditDest::New(k))));
                }
            }
        }
        for (g, heads) in &self.sent {
            let i = target(g, &mut targets);
            moves.extend(heads.iter().map(|f| (*f, EditDest::Group(i))));
        }
        moves.extend(self.removed.iter().map(|f| (*f, EditDest::Removed)));
        moves.extend(self.taken.iter().map(|f| (*f, EditDest::TakenOut)));
        GroupEdit { kind: self.kind, targets, new_names, moves, renames }
    }

    /// Lay out the preset, once the edited group's faces are in.
    fn lay_out(&mut self) {
        let Some(preset) = self.preset.take() else { return };
        let home: HashSet<FaceRef> = self.cols[0].heads.iter().copied().collect();
        match preset {
            Preset::Split(heads) => {
                let heads: Vec<FaceRef> = heads.into_iter().filter(|f| home.contains(f)).collect();
                self.cols[0].heads.retain(|f| !heads.contains(f));
                self.cols[1].heads = heads;
            }
            Preset::Pick(heads) => {
                let heads: Vec<FaceRef> = heads.into_iter().filter(|f| home.contains(f)).collect();
                self.ui.pick_all(0, heads);
            }
            Preset::Plain | Preset::With(_) => {}
        }
        for (i, c) in self.cols.iter().enumerate() {
            for f in &c.heads {
                self.start.insert(*f, Place::Col(i));
            }
        }
    }

    /// The suggested split is laid out: its pair head that splits off is
    /// in a column of its own, apart from *Stays*.
    pub fn suggestion_shown(&self) -> bool {
        self.suggestion.as_ref().is_some_and(|s| matches!(self.place_of(&s.pair.1), Some(Place::Col(i)) if i > 0 && self.cols[i].origin == Origin::New))
    }

    /// Lay out the suggested split: the side it splits off, of the heads
    /// still in *Stays*, in a new column; whatever the user arranged stays
    /// as it is.  One step (Ctrl+Z takes it back); not a move by hand.
    pub fn lay_out_suggestion(&mut self) -> bool {
        let Some(s) = self.suggestion.clone() else { return false };
        if self.suggestion_shown() {
            return false;
        }
        let off: Vec<FaceRef> = s.off.iter().filter(|f| self.cols[0].heads.contains(f)).copied().collect();
        if off.is_empty() {
            return false;
        }
        self.history.push(self.snapshot());
        self.cols[0].heads.retain(|f| !off.contains(f));
        self.cols.push(Col { origin: Origin::New, heads: off.clone(), name: String::new(), rank: HashMap::new(), loaded: true });
        let i = self.cols.len() - 1;
        for f in off {
            self.start.insert(f, Place::Col(i));
        }
        self.ui.clear();
        true
    }

    /// Heads that share their photo with another head in the edit.
    pub fn paired(&self) -> HashSet<FaceRef> {
        let mut by_photo: HashMap<PhotoKey, Vec<FaceRef>> = HashMap::new();
        for f in self.faces.keys() {
            by_photo.entry(f.key).or_default().push(*f);
        }
        by_photo.into_values().filter(|v| v.len() > 1).flatten().collect()
    }
}

// ── opening ─────────────────────────────────────────────────────────────────

/// Open the editor on `group` with `preset`.  Kinds never mix: a second
/// group of another kind is refused.
pub fn open(app: &mut App, group: GroupId, preset: Preset) -> Task<Message> {
    let Some(g) = app.subjects.group(&group) else { return Task::none() };
    let (kind, was) = (g.kind, g.name.clone());
    if let Preset::With(other) = &preset {
        if app.subjects.group(other).is_none_or(|o| o.kind != kind) {
            return app.show_status("People and fursuits are never shown together.", 5000);
        }
        if *other == group {
            return Task::none();
        }
    }
    let mut ed = Editor::new(kind, group.clone(), was, preset.clone());
    if let Preset::With(other) = &preset {
        let was = app.subjects.group(other).and_then(|g| g.name.clone());
        ed.cols.push(Col { origin: Origin::Existing { id: other.clone(), was: was.clone() }, heads: Vec::new(), name: was.unwrap_or_default(), rank: HashMap::new(), loaded: false });
    }
    app.subjects.editor = Some(ed);
    app.quicklook = None;
    // A fursuit group's split check: a suggested split, if two of its
    // heads are in one photo.
    if kind == Kind::Fursuit {
        let seq = app.subjects.next_seq();
        if let Some(e) = app.subjects.editor.as_mut() {
            e.check_seq = seq;
        }
        app.search.link.group_check(seq, &group);
        ask_pairs(app);
    }
    let n = app.subjects.editor.as_ref().map_or(0, |e| e.cols.len());
    for i in 0..n {
        if let Some(Origin::Existing { id, .. }) = app.subjects.editor.as_ref().map(|e| e.cols[i].origin.clone()) {
            read(app, id, Some(i));
        }
    }
    subj::refresh_crops(app)
}

/// E: the group on its page, or the one tile picked on the tab.
pub fn edit_key(app: &mut App) -> Task<Message> {
    match app.subjects.page() {
        subj::Page::Person => {
            let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
            let (group, picked) = (p.group.clone(), p.picked.clone());
            open(app, group, if picked.is_empty() { Preset::Plain } else { Preset::Pick(picked) })
        }
        subj::Page::Overview => {
            let s = &app.subjects;
            let picked: Vec<GroupId> = s.picked.iter().filter(|id| s.group(id).is_some_and(|g| g.kind == s.tab)).cloned().collect();
            match (picked.as_slice(), s.focus.clone()) {
                ([one], _) => open(app, one.clone(), Preset::Plain),
                ([a, b], _) => open(app, a.clone(), Preset::With(b.clone())),
                ([], Some(f)) if !s.select_mode => open(app, f, Preset::Plain),
                _ => app.show_status("Pick a group (S, then click it), then E edits it.", 4000),
            }
        }
        _ => Task::none(),
    }
}

/// C: two groups side by side in the editor (the two picked on the tab,
/// the pair in "Same fursuit?"), or on a group's page the editor with its
/// panel, to open the other from.
pub fn compare_key(app: &mut App) -> Task<Message> {
    match app.subjects.page() {
        subj::Page::Overview => {
            let s = &app.subjects;
            let picked: Vec<GroupId> = s.picked.iter().filter(|id| s.group(id).is_some_and(|g| g.kind == s.tab)).cloned().collect();
            match picked.as_slice() {
                [a, b] => open(app, a.clone(), Preset::With(b.clone())),
                _ => app.show_status("Pick two groups (S, then click them), then C shows them side by side.", 4000),
            }
        }
        subj::Page::Merges => {
            // An X waiting for its 1 or 2 is let go: C is the answer.
            super::subjects_merge::cancel_x(app);
            match super::subjects_merge::current_pair(app) {
                Some((a, b)) => open(app, a, Preset::With(b)),
                None => Task::none(),
            }
        }
        subj::Page::Person => edit_key(app),
        _ => Task::none(),
    }
}

fn read(app: &mut App, group: GroupId, col: Option<usize>) {
    let seq = app.subjects.next_seq();
    let Some(e) = app.subjects.editor.as_mut() else { return };
    e.asked.insert(seq, (group.clone(), col));
    app.search.link.faces(seq, &group);
}

fn ask_order(app: &mut App, col: usize) {
    let seq = app.subjects.next_seq();
    let Some(e) = app.subjects.editor.as_mut() else { return };
    let Some(c) = e.cols.get(col) else { return };
    if c.heads.is_empty() {
        return;
    }
    e.ordering.retain(|_, c| *c != col);
    e.ordering.insert(seq, col);
    let (kind, heads) = (e.kind, c.heads.clone());
    app.search.link.heads_order(seq, kind, &heads);
}

/// *Likely the same fursuit*: the engine's pairs with the group edited,
/// asked as a fursuit page's *Similar groups* were (by distance; the
/// review is where the judge compares).  Worked out on the engine.
fn ask_pairs(app: &mut App) {
    let seq = app.subjects.next_seq();
    let min = app.settings.people_min_photos();
    let Some(e) = app.subjects.editor.as_mut().filter(|e| e.kind == Kind::Fursuit && e.follow.is_none()) else { return };
    e.pairs_seq = seq;
    let home = e.home().clone();
    app.search.link.merge_suggestions(seq, Some(&home), min, false);
}

/// The engine's pairs, if they are the editor's.
pub fn on_pairs(app: &mut App, seq: u64, list: &Arc<Vec<GroupPair>>) -> bool {
    let Some(e) = app.subjects.editor.as_mut().filter(|e| e.pairs_seq == seq && seq != 0) else { return false };
    e.pairs = Some(list.clone());
    true
}

/// *Merge* (or the merge dialog's) on a pair with the group edited: the
/// editor follows the group kept.  Whether the editor made it.
pub fn merging(app: &mut App, keep: &GroupId, groups: &[GroupId]) -> bool {
    let s = &app.subjects;
    let Some(e) = s.editor.as_ref() else { return false };
    let home = e.home().clone();
    let gone = groups.iter().find(|g| *g != keep).cloned();
    let (Some(gone), true) = (gone, groups.contains(&home) && groups.len() == 2) else { return false };
    let covers: Vec<FaceRef> = groups.iter().filter_map(|g| s.group(g)).map(|g| g.cover.face).collect();
    let Some(cover) = s.group(&home).map(|g| g.cover.face) else { return false };
    let e = app.subjects.editor.as_mut().expect("the editor");
    e.follow = Some(Follow { want: keep.clone(), gone: Some(gone), covers, step: Some(LibStep::Merged { back_to: home, cover }) });
    e.note = None;
    true
}

fn ask_near(app: &mut App) {
    let seq = app.subjects.next_seq();
    let Some(e) = app.subjects.editor.as_mut() else { return };
    e.near_seq = seq;
    let (kind, heads) = (e.kind, e.cols[0].heads.clone());
    app.search.link.near_groups(seq, kind, &heads);
}

// ── events ──────────────────────────────────────────────────────────────────

/// A group's faces arrived: a column's, or a panel group's (its photos).
pub fn on_faces(app: &mut App, seq: u64, faces: &Arc<Vec<Face>>) -> bool {
    let Some(e) = app.subjects.editor.as_mut() else { return false };
    let Some((group, col)) = e.asked.remove(&seq) else { return false };
    e.photos_of.insert(group.clone(), faces.iter().map(|f| f.face.key).collect());
    for f in faces.iter() {
        e.faces.entry(f.face).or_insert_with(|| f.clone());
        e.origin.entry(f.face).or_insert_with(|| group.clone());
    }
    let mut order = Vec::new();
    if let Some(i) = col {
        // Heads placed elsewhere in this edit stay where they are; heads
        // sent to this group before it opened join it.
        let placed: HashSet<FaceRef> = faces.iter().map(|f| f.face).filter(|f| e.place_of(f).is_some()).collect();
        let mut heads: Vec<FaceRef> = faces.iter().map(|f| f.face).filter(|f| !placed.contains(f)).collect();
        if let Some(k) = e.sent.iter().position(|(g, _)| *g == group) {
            let (_, v) = e.sent.remove(k);
            heads.splice(0..0, v);
        }
        if let Some(c) = e.cols.get_mut(i) {
            c.heads = heads;
            c.loaded = true;
        }
        if i == 0 {
            e.lay_out();
        } else if let Some(c) = e.cols.get(i) {
            // Where its heads start (heads moved by hand are those elsewhere
            // later); the edited group's are laid out with its preset.
            for f in c.heads.clone() {
                e.start.entry(f).or_insert(Place::Col(i));
            }
        }
        order = (0..e.cols.len()).collect();
    }
    // A move waiting for this group's heads: made now (or refused).
    if col.is_none() && e.pending.as_ref().is_some_and(|mv| mv.to == Target::Group(group.clone())) {
        let mv = e.pending.take().expect("checked");
        match e.apply_move(&mv) {
            Ok(()) => {
                e.note = None;
                order = (0..e.cols.len()).collect();
            }
            Err(why) => e.note = why,
        }
    }
    let home_first = col == Some(0);
    for i in order {
        ask_order(app, i);
    }
    if home_first {
        ask_near(app);
    }
    true
}

/// The edited group's split check arrived: a suggested split, or none.
pub fn on_check(app: &mut App, seq: u64, pairs: &[(FaceRef, FaceRef)], split: Option<&(Vec<FaceRef>, Vec<FaceRef>)>) -> bool {
    let Some(e) = app.subjects.editor.as_mut() else { return false };
    if e.check_seq != seq {
        return false;
    }
    e.suggestion = Suggestion::from_check(pairs, split);
    true
}

/// A column's order arrived.
pub fn on_heads_order(app: &mut App, seq: u64, far: &[(FaceRef, f32)]) -> bool {
    let Some(e) = app.subjects.editor.as_mut() else { return false };
    let Some(col) = e.ordering.remove(&seq) else { return false };
    if let Some(c) = e.cols.get_mut(col) {
        c.rank = far.iter().enumerate().map(|(i, (f, _))| (*f, i)).collect();
    }
    true
}

/// The panel's groups arrived, nearest first: the nearest named ones'
/// heads are read (for the photo pair check).
pub fn on_near(app: &mut App, seq: u64, list: &Arc<Vec<(GroupId, f32)>>) -> bool {
    let Some(e) = app.subjects.editor.as_mut() else { return false };
    if e.near_seq != seq {
        return false;
    }
    e.near = (**list).clone();
    let shown: Vec<GroupId> = app.subjects.editor.as_ref().map(|e| e.paired_groups(&app.subjects.all).into_iter().chain(panel_groups(&app.subjects, e).into_iter().take(READ_N)).chain(unnamed_groups(&app.subjects, e)).collect()).unwrap_or_default();
    for g in shown {
        read(app, g, None);
    }
    true
}

/// *Apply* went through, or was refused.
pub fn on_applied(app: &mut App, seq: u64, words: &str) -> Task<Message> {
    let Some(e) = app.subjects.editor.as_ref() else { return Task::none() };
    if e.apply_seq != seq {
        return Task::none();
    }
    let home_cluster = matches!(e.home(), GroupId::Cluster(_));
    app.subjects.editor = None;
    let t = if home_cluster && app.subjects.person.is_some() { subj::to_overview(app) } else { subj::refresh_crops(app) };
    let s = app.show_status_action(format!("{words}."), "Undo", sm(subj::Msg::Undo));
    Task::batch([t, s])
}

pub fn on_refused(app: &mut App, seq: u64, why: &str, gone: Option<crate::model::subjects::SubjectId>) -> Task<Message> {
    let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
    if e.apply_seq != seq {
        return Task::none();
    }
    e.apply_seq = 0;
    match gone {
        Some(s) => {
            let g = GroupId::Subject(s);
            let name = e.cols.iter().find_map(|c| match &c.origin {
                Origin::Existing { id, was } if *id == g => was.clone(),
                _ => None,
            });
            e.note = Some(format!("{} is gone (forgotten or merged meanwhile). Nothing was changed: send its {}s to a new group, or elsewhere, then apply again.", name.unwrap_or_else(|| "A group you sent heads to".into()), cols::crop_word(e.kind)));
            e.gone = Some(g);
        }
        None => e.note = Some(format!("Nothing was changed: {why}.")),
    }
    Task::none()
}

/// The groups changed while the editor is open (a regroup, a scan): the
/// arrangement stays; a column whose group changed says so, quietly.  A
/// merge made here (or its undo) has the editor show the group it is on
/// now; the pairs are asked again.
pub fn on_subjects(app: &mut App) -> Task<Message> {
    let all = app.subjects.all.clone();
    let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
    if let Some(f) = e.follow.clone() {
        return match f.resolve(&all, e.kind) {
            Followed::Waiting => Task::none(),
            Followed::At(g) => {
                let mut steps = std::mem::take(&mut e.lib_steps);
                steps.extend(f.step);
                // A group's page under the editor follows too.
                if app.subjects.person.as_ref().is_some_and(|p| all.group(&p.group).is_none()) {
                    super::subjects_person::follow(app, &g);
                }
                let t = open(app, g, Preset::Plain);
                if let Some(e) = app.subjects.editor.as_mut() {
                    e.lib_steps = steps;
                }
                t
            }
            Followed::Lost => {
                app.subjects.editor = None;
                app.show_status("That group changed meanwhile: here are the groups as they are now.", 6000)
            }
        };
    }
    let changed = e.cols.iter().any(|c| matches!(&c.origin, Origin::Existing { id, .. } if all.group(id).is_none()));
    if changed && e.note.is_none() {
        e.note = Some(format!("The groups were sorted again meanwhile. Your arrangement stays: Apply sends each {} where you put it.", cols::crop_word(e.kind)));
    }
    ask_pairs(app);
    Task::none()
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    if let Msg::Open(g, preset) = msg {
        return open(app, g, preset);
    }
    if let Msg::UndoLibrary = msg {
        return undo_library(app);
    }
    let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
    match msg {
        Msg::Open(..) | Msg::UndoLibrary => Task::none(),
        Msg::PairMerge(i) => pair_merge(app, i),
        Msg::PairNotSame(i) => pair_not_same(app, i),
        Msg::PairReview(i) => pair_review(app, i),
        Msg::Cols(c) => {
            // A drag going over a panel group not read yet: read it now.
            if let ColMsg::DragEnter(Target::Group(g)) = &c {
                if !e.photos_of.contains_key(g) && !e.asked.values().any(|(x, _)| x == g) {
                    let g = g.clone();
                    let t = update_cols(app, c);
                    read(app, g, None);
                    return t;
                }
            }
            update_cols(app, c)
        }
        Msg::AddColumn => {
            e.history.push(e.snapshot());
            e.cols.push(Col { origin: Origin::New, heads: Vec::new(), name: String::new(), rank: HashMap::new(), loaded: true });
            Task::none()
        }
        Msg::Expand(g) => {
            let (shown, kind) = (e.col_of_group(&g).is_some(), e.kind);
            if shown || app.subjects.group(&g).is_none_or(|x| x.kind != kind) {
                return Task::none();
            }
            let was = app.subjects.group(&g).and_then(|x| x.name.clone());
            let e = app.subjects.editor.as_mut().expect("editor");
            e.history.push(e.snapshot());
            e.cols.push(Col { origin: Origin::Existing { id: g.clone(), was: was.clone() }, heads: Vec::new(), name: was.unwrap_or_default(), rank: HashMap::new(), loaded: false });
            let i = e.cols.len() - 1;
            read(app, g, Some(i));
            subj::refresh_crops(app)
        }
        Msg::Collapse(i) => {
            let Some(Origin::Existing { id, .. }) = e.cols.get(i).map(|c| c.origin.clone()) else { return Task::none() };
            if i == 0 {
                return Task::none();
            }
            e.history.push(e.snapshot());
            let c = e.cols.remove(i);
            // Heads brought into it stay sent there; its own stay put.
            let brought: Vec<FaceRef> = c.heads.iter().filter(|f| e.origin.get(f) != Some(&id)).copied().collect();
            if !brought.is_empty() {
                e.sent.push((id.clone(), brought));
            }
            for f in c.heads.iter().filter(|f| e.origin.get(f) == Some(&id)) {
                // Not shown, not moved: forgotten by the arrangement.
                e.start.remove(f);
            }
            e.ui.renumber(|c| if c == i { None } else if c > i { Some(c - 1) } else { Some(c) });
            Task::none()
        }
        Msg::Search(s) => {
            e.search = s;
            subj::refresh_crops(app)
        }
        Msg::Name(i, name) => {
            if let Some(c) = e.cols.get_mut(i) {
                c.name = name;
            }
            Task::none()
        }
        Msg::MergeName(i, g) => {
            let Some(c) = e.cols.get(i) else { return Task::none() };
            let mv = cols::Move { from: i, faces: c.heads.clone(), to: Target::Group(g) };
            let was = match &c.origin {
                Origin::Existing { was, .. } => was.clone().unwrap_or_default(),
                Origin::New => String::new(),
            };
            let new = c.origin == Origin::New;
            if mv.faces.is_empty() {
                e.cols[i].name = was;
                return Task::none();
            }
            match e.apply_move(&mv) {
                Ok(()) => {
                    e.cols[i].name = was;
                    if new {
                        e.cols.remove(i);
                        e.ui.renumber(|c| if c == i { None } else if c > i { Some(c - 1) } else { Some(c) });
                    }
                    reorder(app)
                }
                Err(why) => {
                    e.note = why;
                    Task::none()
                }
            }
        }
        Msg::GoneToNew => {
            let Some(g) = e.gone.take() else { return Task::none() };
            e.note = None;
            e.history.push(e.snapshot());
            let mut heads: Vec<FaceRef> = e.sent.iter().filter(|(x, _)| *x == g).flat_map(|(_, v)| v.iter().copied()).collect();
            e.sent.retain(|(x, _)| *x != g);
            if let Some(i) = e.col_of_group(&g) {
                let c = e.cols.remove(i);
                heads.extend(c.heads);
                e.ui.renumber(|c| if c == i { None } else if c > i { Some(c - 1) } else { Some(c) });
            }
            if !heads.is_empty() {
                e.cols.push(Col { origin: Origin::New, heads, name: String::new(), rank: HashMap::new(), loaded: true });
            }
            reorder(app)
        }
        Msg::Suggest => {
            if e.lay_out_suggestion() {
                e.note = None;
                return reorder(app);
            }
            Task::none()
        }
        Msg::Apply => apply(app),
        Msg::Cancel => close(app),
    }
}

fn update_cols(app: &mut App, c: ColMsg) -> Task<Message> {
    let shift = crate::ui::modifiers::get().shift();
    let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
    let orders: Vec<Vec<FaceRef>> = (0..e.cols.len()).map(|i| e.ordered(i)).collect();
    let Some(mv) = e.ui.update(c, shift, |i| orders.get(i).cloned().unwrap_or_default(), orders.len()) else { return Task::none() };
    match e.apply_move(&mv) {
        Ok(()) => {
            e.note = None;
            reorder(app)
        }
        Err(Some(why)) => {
            e.note = Some(why);
            Task::none()
        }
        Err(None) => {
            // Its heads are being read: the move is made when they arrive.
            let Target::Group(g) = mv.to.clone() else { return Task::none() };
            e.note = Some(format!("Reading that group's {}s first…", cols::crop_word(e.kind)));
            let asked = e.asked.values().any(|(x, _)| *x == g);
            e.pending = Some(mv);
            if !asked {
                read(app, g, None);
            }
            Task::none()
        }
    }
}

/// After a step: every column's order again (cheap, on the engine).
fn reorder(app: &mut App) -> Task<Message> {
    let n = app.subjects.editor.as_ref().map_or(0, |e| e.cols.len());
    for i in 0..n {
        ask_order(app, i);
    }
    subj::refresh_crops(app)
}

// ── *Likely the same fursuit* ───────────────────────────────────────────────

/// Pair `i`'s other group and the pair, while nothing waits on a merge.
fn pair_at(app: &App, i: usize) -> Option<(GroupId, GroupPair)> {
    let e = app.subjects.editor.as_ref().filter(|e| e.follow.is_none())?;
    let (_, other) = e.shown_pairs(&app.subjects.all).into_iter().find(|(j, _)| *j == i)?;
    Some((other.clone(), e.pairs.as_ref()?.get(i)?.clone()))
}

/// The arrangement isn't applied: a merge (or the review) waits for it.
fn wait_for_apply(app: &mut App) -> bool {
    let Some(e) = app.subjects.editor.as_mut().filter(|e| e.changed()) else { return false };
    e.note = Some("Apply or cancel your changes first: a merge is made at once, as a change of its own.".into());
    true
}

/// *Merge*: the two at once, one undo step, recorded as the suggested
/// pair; two named ask which name to keep.
fn pair_merge(app: &mut App, i: usize) -> Task<Message> {
    let Some((other, pair)) = pair_at(app, i) else { return Task::none() };
    if wait_for_apply(app) {
        return Task::none();
    }
    let Some(e) = app.subjects.editor.as_ref() else { return Task::none() };
    let home = e.home().clone();
    let (Some(mine), Some(theirs)) = (app.subjects.group(&home).cloned(), app.subjects.group(&other).cloned()) else {
        return app.show_status("That group changed: look again.", 4000);
    };
    if mine.kind != Kind::Fursuit || theirs.kind != Kind::Fursuit {
        return Task::none();
    }
    match merge_plan(&mine, &theirs, &pair) {
        MergePlan::Ask => {
            let all = app.subjects.all.clone();
            app.subjects.merge = MergeAsk::new(&all, vec![home.clone(), other], Some(&home)).map(|m| MergeAsk { via: super::subjects_merge::via(&pair, At::Similar), ..m });
            subj::refresh_crops(app)
        }
        MergePlan::Merge { into, gone, via } => {
            merging(app, &into, &[into.clone(), gone.clone()]);
            app.search.link.merge(&into, &[gone], via);
            let name = mine.name.or(theirs.name).unwrap_or_else(|| "one group".into());
            app.show_status_action(format!("Merged the two groups into {name}."), "Undo", m(Msg::UndoLibrary))
        }
    }
}

/// ×: not the same fursuit (one undo step); the pair goes at once.
fn pair_not_same(app: &mut App, i: usize) -> Task<Message> {
    let Some((_, pair)) = pair_at(app, i) else { return Task::none() };
    let heads = |h: &[Face]| h.iter().map(|f| f.face).collect::<Vec<FaceRef>>();
    app.search.link.not_same(&pair.a, &pair.b, &heads(&pair.a_heads), &heads(&pair.b_heads), super::subjects_merge::via(&pair, At::Similar));
    if let Some(e) = app.subjects.editor.as_mut() {
        e.lib_steps.push(LibStep::NotSame);
        if let Some(list) = &e.pairs {
            e.pairs = Some(Arc::new(list.iter().filter(|p| p.key() != pair.key()).cloned().collect()));
        }
    }
    app.show_status_action("Not the same fursuit: they won't be suggested together again.", "Undo", m(Msg::UndoLibrary))
}

/// "Same fursuit?" at pair `i` (the editor closes; Esc there opens it
/// again on this group).
fn pair_review(app: &mut App, i: usize) -> Task<Message> {
    let Some((_, pair)) = pair_at(app, i) else { return Task::none() };
    if wait_for_apply(app) {
        return Task::none();
    }
    let Some(e) = app.subjects.editor.take() else { return Task::none() };
    super::subjects_merge::start_at(app, pair, e.home().clone())
}

/// Take back the newest merge or "not the same" made here (Ctrl+Z once
/// the arrangement's steps are gone, or the status bar's Undo).  With
/// none, or no editor, the library's last change.
fn undo_library(app: &mut App) -> Task<Message> {
    let Some(e) = app.subjects.editor.as_mut() else {
        app.search.link.undo_subjects();
        return Task::none();
    };
    if e.follow.is_some() {
        return app.show_status("One moment: the last change is still being made.", 3000);
    }
    match e.lib_steps.last().cloned() {
        None => {
            app.search.link.undo_subjects();
            Task::none()
        }
        Some(LibStep::NotSame) => {
            e.lib_steps.pop();
            app.search.link.undo_subjects();
            Task::none()
        }
        Some(LibStep::Merged { back_to, cover }) => {
            // Undoing it shows the group edited before: an arrangement
            // here would go with it.
            if e.changed() {
                e.note = Some("Apply or cancel your changes first, then undo the merge.".into());
                return Task::none();
            }
            e.lib_steps.pop();
            e.follow = Some(Follow { want: back_to, gone: None, covers: vec![cover], step: None });
            app.search.link.undo_subjects();
            Task::none()
        }
    }
}

fn apply(app: &mut App) -> Task<Message> {
    let s = &app.subjects;
    let Some(e) = s.editor.as_ref() else { return Task::none() };
    if e.apply_seq != 0 {
        return Task::none();
    }
    let edit = e.to_edit(&|g| s.group(g).map(|x| x.cover.face));
    if edit.moves.is_empty() && edit.renames.is_empty() {
        return close(app);
    }
    let via = Via::Editor { proposal: e.suggestion_shown(), edited: e.edited() as u32 };
    let seq = app.subjects.next_seq();
    if let Some(e) = app.subjects.editor.as_mut() {
        e.apply_seq = seq;
        e.note = None;
    }
    app.search.link.apply_edit(seq, edit, via);
    Task::none()
}

fn close(app: &mut App) -> Task<Message> {
    app.subjects.editor = None;
    subj::refresh_crops(app)
}

/// Ctrl+Z while the editor is open: its arrangement's last step, else the
/// last merge or "not the same" made here (other changes to the library
/// wait).  Whether it was open.
pub fn take_back(app: &mut App) -> Option<Task<Message>> {
    let e = app.subjects.editor.as_mut()?;
    if e.take_back() {
        e.note = None;
        return Some(reorder(app));
    }
    if !e.lib_steps.is_empty() {
        return Some(undo_library(app));
    }
    Some(Task::none())
}

/// Esc: a drag, the picks, a note, then *Cancel*.
pub fn escape(app: &mut App) -> Task<Message> {
    let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
    if e.ui.escape() {
        return Task::none();
    }
    if e.note.take().is_some() {
        return Task::none();
    }
    close(app)
}

pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if let Some(c) = cols::key_msg(key, mods) {
        return update(app, Msg::Cols(c));
    }
    match key {
        Key::Named(Named::Enter) if !mods.control() && !mods.alt() => update(app, Msg::Apply),
        _ => Task::none(),
    }
}

// ── crops ───────────────────────────────────────────────────────────────────

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(e) = s.editor.as_ref() else { return Vec::new() };
    let small = subj::px(app, tile::SMALL_CROP);
    let mut v = Vec::new();
    for i in 0..e.cols.len() {
        let section = cols::section(i);
        v.extend(e.ordered(i).iter().filter_map(|f| e.faces.get(f)).filter(|f| s.crops.is_seen(section, &subj::Crops::key(&f.face, small, Framing::Face))).cloned().map(|f| (f, small, Framing::Face)));
    }
    if let Some(sg) = &e.suggestion {
        let px = subj::px(app, SUGGEST);
        v.extend([sg.pair.0, sg.pair.1].iter().filter_map(|f| e.faces.get(f)).cloned().map(|f| (f, px, Framing::Face)));
    }
    let cover = subj::px(app, COVER);
    let paired = e.panel_pairs(&s.all).into_iter().map(|(_, g)| g.clone());
    v.extend(paired.chain(panel_groups(s, e)).chain(unnamed_groups(s, e)).filter_map(|g| s.group(&g)).map(|g| (g.cover.clone(), cover, Framing::Face)));
    for c in &e.cols {
        if let Origin::Existing { id, .. } = &c.origin {
            if let Some(g) = s.group(id) {
                v.push((g.cover.clone(), cover, Framing::Face));
            }
        }
    }
    v
}

// ── the panel ───────────────────────────────────────────────────────────────

/// Named groups of the kind, nearest first (by name when no distance is
/// known yet), filtered by the search box; never one shown as a column,
/// nor one *Likely the same fursuit* shows over them (unless searched for).
fn panel_groups(s: &subj::Subjects, e: &Editor) -> Vec<GroupId> {
    let want = e.search.trim().to_lowercase();
    let paired = e.paired_groups(&s.all);
    let rank: HashMap<&GroupId, usize> = e.near.iter().enumerate().map(|(i, (g, _))| (g, i)).collect();
    let mut v: Vec<&super::people::Group> = s.all.groups.iter().filter(|g| g.kind == e.kind && g.name.is_some() && !g.hidden && e.col_of_group(&g.id).is_none() && !paired.contains(&g.id)).filter(|g| want.is_empty() || g.name.as_deref().is_some_and(|n| n.to_lowercase().contains(&want))).collect();
    v.sort_by_key(|g| (rank.get(&g.id).copied().unwrap_or(usize::MAX), g.name.clone()));
    v.into_iter().map(|g| g.id.clone()).collect()
}

/// The largest unnamed groups (not when searching by name), but those
/// *Likely the same fursuit* shows.
fn unnamed_groups(s: &subj::Subjects, e: &Editor) -> Vec<GroupId> {
    if !e.search.trim().is_empty() {
        return Vec::new();
    }
    let paired = e.paired_groups(&s.all);
    let mut v: Vec<&super::people::Group> = s.all.groups.iter().filter(|g| g.kind == e.kind && g.name.is_none() && !g.hidden && e.col_of_group(&g.id).is_none() && !paired.contains(&g.id)).collect();
    v.sort_by(|a, b| b.photos.cmp(&a.photos));
    v.into_iter().take(UNNAMED_N).map(|g| g.id.clone()).collect()
}

/// The panel's groups in the order shown (the test driver's names for
/// them): named, then unnamed.
pub fn panel_ids(app: &App, e: &Editor) -> Vec<GroupId> {
    let s = &app.subjects;
    panel_groups(s, e).into_iter().chain(unnamed_groups(s, e)).collect()
}

/// The group a taken name in column `i` offers to merge into.
pub fn name_offer(app: &App, e: &Editor, i: usize) -> Option<GroupId> {
    taken_name(&app.subjects, e, i).map(|g| g.id.clone())
}

/// A taken name: the named group of the kind called so, if it isn't the
/// column's own.
fn taken_name<'a>(s: &'a subj::Subjects, e: &Editor, col: usize) -> Option<&'a super::people::Group> {
    let c = e.cols.get(col)?;
    let name = c.name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    let own = match &c.origin {
        Origin::Existing { id, .. } => Some(id),
        Origin::New => None,
    };
    s.all.groups.iter().find(|g| g.kind == e.kind && Some(&g.id) != own && g.name.as_deref().is_some_and(|n| n.to_lowercase() == name))
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let Some(e) = s.editor.as_ref() else { return Space::new().into() };
    let look = Look { kind: e.kind, cols: &e.ui, wrap };
    let (stay, moved, removed, taken) = e.counts();
    let title = match s.group(e.home()).and_then(|g| g.name.clone()) {
        Some(n) => format!("Sorting {n}"),
        None => format!("Sorting an unnamed group of {}", util::plural(s.group(e.home()).map_or(0, |g| g.photos) as u64, "photo")),
    };
    let mut counter = format!("{stay} stay · {moved} move · {removed} removed");
    if taken > 0 {
        counter.push_str(&format!(" · {taken} taken out"));
    }
    let busy = e.apply_seq != 0;
    let header = row![
        tip(kit::icon_text_btn(Glyph::ChevronLeft, "Cancel", t::INK2, style::secondary(), Some(m(Msg::Cancel))), "Leave without changing anything  (Esc)", true),
        column![text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK), kit::caption("Nothing changes until you apply. Ctrl+Z here takes back a step.")].spacing(1).padding(Padding { left: t::S3, ..Padding::ZERO }),
        Space::new().width(Length::Fill),
        suggestion(app, e),
        text(counter).size(t::T_BODY).font(paint::MEDIUM).color(t::INK2),
        tip(kit::btn(if busy { "Applying…" } else { "Apply" }, style::primary(), (!busy).then(|| m(Msg::Apply))), "Make these changes, as one undo step  (Enter)", true),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center);
    let header = container(header.padding(Padding::from([0.0, t::S3]))).height(subj::HEADER_H).center_y(subj::HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT));
    let mut page = column![header, kit::hairline()];
    if let Some(strip) = pick_strip(e) {
        page = page.push(strip);
    }
    if let Some(note) = &e.note {
        let mut r = row![icons::view(Glyph::Info, 15.0, t::INK2), text(note.clone()).size(t::T_SMALL).color(t::INK2).width(Length::Fill)].spacing(8).align_y(Alignment::Center);
        if e.gone.is_some() {
            r = r.push(kit::btn("Send to a new group", style::secondary(), Some(m(Msg::GoneToNew))));
        }
        page = page.push(container(r).padding(Padding::from([6.0, 16.0])).width(Length::Fill).style(style::fill(t::RAISED)));
    }
    // The columns: as wide as fits, never narrower than COL_MIN (then they
    // scroll sideways).
    let avail = app.window_size_width() - crate::ui::side_menu::RAIL_W - PANEL_W - 2.0;
    let n = e.cols.len() as f32 + 0.5;
    let w = (avail / n).max(COL_MIN);
    let paired = e.paired();
    let mut strip = row![].height(Length::Fill);
    for i in 0..e.cols.len() {
        if i > 0 {
            strip = strip.push(container(Space::new().width(1).height(Length::Fill)).style(style::fill(t::PHOTO_LINE)));
        }
        strip = strip.push(column_view(app, e, &look, i, w, &|f| paired.contains(f)));
    }
    strip = strip.push(container(Space::new().width(1).height(Length::Fill)).style(style::fill(t::PHOTO_LINE)));
    strip = strip.push(new_column(e, &look, (w * 0.5).max(150.0)));
    let columns: Element<'_, Message> = if w * n > avail + 1.0 {
        scrollable(strip).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())).style(style::scroll).width(Length::Fill).height(Length::Fill).into()
    } else {
        strip.into()
    };
    let body = row![container(columns).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)), container(Space::new().width(1).height(Length::Fill)).style(style::fill(t::LINE)), panel(app, e, &look)];
    page = page.push(body.height(Length::Fill));
    let crops = cols::crop_word(e.kind);
    let r_word = if e.kind == Kind::Fursuit { "Take out of group" } else { "Not this person" };
    let keys = cols::keys(&[("Shift+click", "Pick a run"), ("← →", "To the column beside"), ("N", "New group"), ("R", r_word), ("Delete", if crops == "head" { "Not heads" } else { "Not faces" }), ("Ctrl+Z", "Take back"), ("Enter", "Apply"), ("Esc", "Cancel")]);
    page = page.push(container(keys).padding(Padding::from([7.0, 20.0])).width(Length::Fill).style(style::fill(t::PHOTO_GROUND)));
    page.width(Length::Fill).height(Length::Fill).into()
}

/// *Suggested split*, in the header, for a group holding two heads of one
/// photo: the two, small, and a click lays the split out.  Nothing when
/// there is none, or once it is laid out.
fn suggestion<'a>(app: &'a App, e: &'a Editor) -> Element<'a, Message> {
    let Some(sg) = &e.suggestion else { return Space::new().into() };
    if e.suggestion_shown() || !e.cols[0].loaded {
        return Space::new().into();
    }
    let s = &app.subjects;
    let px = subj::px(app, SUGGEST);
    let shape = subj::shape(e.kind);
    let thumb = |f: &FaceRef| tile::crop(e.faces.get(f).and_then(|x| s.crops.get(&x.face, px, Framing::Face)), SUGGEST, shape);
    let inner = row![icons::view(Glyph::Split, 14.0, t::INK2), thumb(&sg.pair.0), thumb(&sg.pair.1), text("Suggested split").size(t::T_SMALL).font(paint::MEDIUM).color(t::INK)].spacing(6).align_y(Alignment::Center);
    let b = button(inner).padding(Padding::from([3.0, 8.0])).style(style::secondary().style()).on_press(m(Msg::Suggest));
    tip(b, format!("Two of its heads are in one photo. Lay out a split around them ({} {}s in a new column); nothing else moves", sg.off.len(), cols::crop_word(e.kind)), true)
}

fn column_view<'a>(app: &'a App, e: &'a Editor, look: &Look<'a>, i: usize, w: f32, paired: &dyn Fn(&FaceRef) -> bool) -> Element<'a, Message> {
    let s = &app.subjects;
    let c = &e.cols[i];
    let order: Vec<&Face> = e.ordered(i).iter().filter_map(|f| e.faces.get(f)).collect();
    let marked = if c.rank.is_empty() { 0 } else { crate::search::splitting::marked(order.len()) };
    let crops = cols::crop_word(e.kind);
    let (label, placeholder, can_close) = match &c.origin {
        Origin::Existing { .. } if i == 0 => (format!("Stays ({})", order.len()), "Name this group (optional)".to_string(), false),
        Origin::Existing { was, .. } => (format!("{} ({})", was.clone().unwrap_or_else(|| "Unnamed group".into()), order.len()), "Name it (optional)".to_string(), true),
        Origin::New => {
            let k = e.cols[..i].iter().filter(|x| x.origin == Origin::New).count() + 1;
            (format!("New group {k} ({})", order.len()), "Name it (optional)".to_string(), false)
        }
    };
    let mut top = row![text(label).size(t::T_BODY).font(paint::SEMIBOLD).color(t::PHOTO_INK).width(Length::Fill)].align_y(Alignment::Center).spacing(6);
    if can_close {
        top = top.push(tip(kit::icon_btn(Glyph::Close, t::PHOTO_INK3, style::ghost(), Some(m(Msg::Collapse(i)))).width(22).height(22).padding(0), "Back into the list (what you moved here stays sent to it)", true));
    }
    let input = text_input(&placeholder, &c.name).on_input(move |v| m(Msg::Name(i, v))).size(t::T_SMALL).padding(Padding::from([4.0, 8.0])).style(style::input());
    let mut head = column![top, input].spacing(6);
    if let Some(g) = taken_name(s, e, i) {
        let name = g.name.clone().unwrap_or_default();
        head = head.push(row![text(format!("{name} already exists.")).size(t::T_SMALL).color(t::PHOTO_INK2), kit::btn(format!("Merge into {name}"), style::secondary(), Some(m(Msg::MergeName(i, g.id.clone()))))].spacing(8).align_y(Alignment::Center));
    }
    head = head.push(text(if c.rank.is_empty() { String::new() } else { "Least like the rest first".to_string() }).size(t::T_SMALL).color(t::PHOTO_INK3));
    let tiles = cols::grid(app, look, i, &order, marked, paired);
    let foot: Option<Element<'a, Message>> = if !c.loaded {
        Some(text("Reading…").size(t::T_SMALL).color(t::PHOTO_INK3).into())
    } else if order.is_empty() {
        Some(text(format!("Drag {crops}s here.")).size(t::T_SMALL).color(t::PHOTO_INK3).into())
    } else {
        None
    };
    cols::column_box(look, i, head.into(), tiles, foot, Length::Fixed(w))
}

/// *+ New group*: a click makes an empty column; a drop makes one with the
/// heads dropped.
fn new_column<'a>(e: &'a Editor, look: &Look<'a>, w: f32) -> Element<'a, Message> {
    let dragging = e.ui.drag.is_some();
    let words = if dragging { "Drop here for a new group" } else { "New group" };
    let inner = column![icons::view(Glyph::Plus, 20.0, t::PHOTO_INK2), text(words).size(t::T_SMALL).font(paint::MEDIUM).color(t::PHOTO_INK2)].spacing(6).align_x(Alignment::Center);
    let b = button(container(inner).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).center_y(Length::Fill)).on_press(m(Msg::AddColumn)).padding(12).width(Length::Fill).height(Length::Fill).style(|_t, status| button::Style {
        background: Some(Background::Color(if matches!(status, button::Status::Hovered) { t::PHOTO_RAISED } else { Default::default() })),
        text_color: t::PHOTO_INK,
        ..button::Style::default()
    });
    container(cols::drop_target(look, Target::New, b.into())).width(Length::Fixed(w)).height(Length::Fill).into()
}

fn pick_strip(e: &Editor) -> Option<Element<'_, Message>> {
    let col = e.ui.picked_in?;
    let crops = cols::crop_word(e.kind);
    let n = e.ui.picked.len();
    let mut r = row![icons::view(Glyph::Select, 16.0, t::SIGNAL), text(format!("{} picked", util::plural(n as u64, crops))).size(t::T_BODY).color(t::INK).width(Length::Fill)].spacing(t::S2).align_y(Alignment::Center);
    if col > 0 {
        r = r.push(tip(kit::icon_text_btn(Glyph::ChevronLeft, "Left", t::INK, style::secondary(), Some(wrap(ColMsg::Step(false)))), "To the column on the left  (←)", true));
    }
    if col + 1 < e.cols.len() {
        r = r.push(tip(kit::icon_text_btn(Glyph::ChevronRight, "Right", t::INK, style::secondary(), Some(wrap(ColMsg::Step(true)))), "To the column on the right  (→)", true));
    }
    r = r.push(tip(kit::icon_text_btn(Glyph::Plus, "New group", t::INK, style::secondary(), Some(wrap(ColMsg::MoveTo(Target::New)))), "Into a group of their own  (N)", true));
    let (take, take_tip) = if e.kind == Kind::Fursuit { ("Take out", "Out of this group, and never back into it  (R)") } else { ("Not this person", "Out of this group, and never back into it  (R)") };
    r = r.push(tip(kit::icon_text_btn(Glyph::LeaveOut, take, t::INK, style::secondary(), Some(wrap(ColMsg::MoveTo(Target::TakenOut)))), take_tip, true));
    r = r.push(tip(kit::icon_text_btn(Glyph::Close, if crops == "head" { "Not heads" } else { "Not faces" }, t::REJECT, style::secondary(), Some(wrap(ColMsg::MoveTo(Target::Removed)))), "Not a fursuit head (or face) at all: the box goes  (Delete)", true));
    Some(column![container(r.padding(Padding { top: 4.0, bottom: 4.0, left: 12.0, right: 12.0 })).width(Length::Fill).style(style::fill(alpha(t::SIGNAL, 0.08))), container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::SIGNAL, 0.3)))].into())
}

fn panel<'a>(app: &'a App, e: &'a Editor, look: &Look<'a>) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(e.kind);
    let cover_px = subj::px(app, COVER);
    let search = text_input(&format!("Find a {}", e.kind.one()), &e.search).on_input(|v| m(Msg::Search(v))).size(t::T_SMALL).padding(Padding::from([5.0, 8.0])).style(style::input());
    let row_of = |id: GroupId| -> Element<'a, Message> {
        let g = s.group(&id);
        let crop = tile::crop(g.and_then(|g| s.crops.get(&g.cover.face, cover_px, Framing::Face)), COVER, shape);
        let name = g.and_then(|g| g.name.clone()).unwrap_or_else(|| "Unnamed".into());
        let photos = g.map_or(0, |g| g.photos);
        let sent = e.sent.iter().filter(|(x, _)| *x == id).map(|(_, v)| v.len()).sum::<usize>();
        let mut line = row![text(util::thousands(photos as u64)).size(t::T_SMALL).color(t::INK3)].spacing(6);
        if sent > 0 {
            line = line.push(text(format!("+{sent}")).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::SIGNAL));
        }
        let words = column![text(name).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK).wrapping(text::Wrapping::None), line].spacing(1).width(Length::Fill);
        let open = tip(kit::icon_btn(Glyph::ChevronRight, t::INK3, style::ghost(), Some(m(Msg::Expand(id.clone())))).width(24).height(24).padding(0), "Open it as a column", true);
        let r = row![crop, words, open].spacing(8).align_y(Alignment::Center).padding(Padding::from([4.0, 8.0]));
        cols::drop_target(look, Target::Group(id), r.into())
    };
    let mut list = column![].spacing(2);
    if let Some(pairs) = pairs_section(app, e, look) {
        list = list.push(pairs);
    }
    let named = panel_groups(s, e);
    if !named.is_empty() {
        list = list.push(container(text(format!("Named {}", e.kind.title().to_lowercase())).size(t::T_SMALL).color(t::INK3)).padding(Padding { left: 8.0, top: 4.0, bottom: 2.0, ..Padding::ZERO }));
    }
    for id in named {
        list = list.push(row_of(id));
    }
    let unnamed = unnamed_groups(s, e);
    if !unnamed.is_empty() {
        list = list.push(container(text("The largest unnamed").size(t::T_SMALL).color(t::INK3)).padding(Padding { left: 8.0, top: 10.0, bottom: 2.0, ..Padding::ZERO }));
    }
    for id in unnamed {
        list = list.push(row_of(id));
    }
    let (_, _, removed, taken) = e.counts();
    let crops = cols::crop_word(e.kind);
    let special = |target: Target, glyph: Glyph, title: &'a str, about: String, n: usize, tone: iced::Color| -> Element<'a, Message> {
        let mut words = column![text(title).size(t::T_SMALL).font(paint::MEDIUM).color(tone), text(about).size(t::T_SMALL).color(t::INK3)].spacing(1).width(Length::Fill);
        if n > 0 {
            words = words.push(text(format!("{n} so far")).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::SIGNAL));
        }
        let r = row![icons::view(glyph, 16.0, tone), words].spacing(10).align_y(Alignment::Center).padding(Padding::from([8.0, 10.0]));
        let boxed = container(r).width(Length::Fill).style(|_| container::Style { border: style::border(t::LINE_HI, 1.0, t::R_CONTROL), ..Default::default() });
        cols::drop_target(look, target, boxed.into())
    };
    let take_title = if e.kind == Kind::Fursuit { "Take out of this group" } else { "Not this person" };
    let take = special(Target::TakenOut, Glyph::LeaveOut, take_title, "Free for other groups, never back in this one  (R)".to_string(), taken, t::INK);
    let gone_title = if e.kind == Kind::Fursuit { "Not fursuit heads" } else { "Not faces" };
    let gone = special(Target::Removed, Glyph::Close, gone_title, format!("Not a {crops} at all: the boxes go  (Delete)"), removed, t::REJECT);
    let col = column![search, scrollable(list).style(style::scroll).height(Length::Fill), take, gone].spacing(8).padding(10);
    container(col).width(PANEL_W).height(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// *Likely the same fursuit*, over the panel's lists: the groups the
/// engine pairs with this one, nearest first.  Each row is a drop target
/// (heads dropped go there, as on any row), its cover and name open it as
/// a column, and three quiet controls: *Merge*, the review at that pair,
/// × not the same.  Offered only: nothing merges without the click.
fn pairs_section<'a>(app: &'a App, e: &'a Editor, look: &Look<'a>) -> Option<Element<'a, Message>> {
    let s = &app.subjects;
    let shown = e.panel_pairs(&s.all);
    if shown.is_empty() {
        return None;
    }
    let shape = subj::shape(e.kind);
    let cover_px = subj::px(app, COVER);
    let free = e.follow.is_none();
    let can = free && !e.changed();
    let mut rows = column![].spacing(0);
    for (i, other) in shown {
        let g = s.group(other);
        let crop = tile::crop(g.and_then(|g| s.crops.get(&g.cover.face, cover_px, Framing::Face)), COVER, shape);
        let name = g.and_then(|g| g.name.clone());
        let named = name.is_some();
        let photos = g.map_or(0, |g| g.photos);
        let sent = e.sent.iter().filter(|(x, _)| x == other).map(|(_, v)| v.len()).sum::<usize>();
        let mut line = row![text(util::plural(photos as u64, "photo")).size(t::T_SMALL).color(t::INK3)].spacing(6);
        if sent > 0 {
            line = line.push(text(format!("+{sent}")).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::SIGNAL));
        }
        let words = column![text(name.unwrap_or_else(|| "Unnamed".into())).size(t::T_SMALL).font(paint::MEDIUM).color(if named { t::INK } else { t::INK2 }).wrapping(text::Wrapping::None), line].spacing(1).width(Length::Fill);
        let open = button(row![crop, words].spacing(8).align_y(Alignment::Center)).padding(0).width(Length::Fill).style(|_t, _s| button::Style::default()).on_press(m(Msg::Expand(other.clone())));
        let open = tip(open, "Open it as a column, to move heads between the two", true);
        let merge_label = text("Merge").size(t::T_SMALL).font(paint::MEDIUM);
        let merge = button(container(merge_label).center_y(Length::Fill)).height(24).padding(Padding::from([0.0, 9.0])).style(style::secondary().style()).on_press_maybe(can.then(|| m(Msg::PairMerge(i))));
        let merge_tip = if can { "Merge it into this group now, as one step (Ctrl+Z takes it back)" } else { "Apply or cancel your changes first: a merge is made at once, as a change of its own" };
        let review = kit::icon_btn(Glyph::Compare, t::INK2, style::ghost(), can.then(|| m(Msg::PairReview(i)))).width(24).height(24).padding(0);
        let not = kit::icon_btn(Glyph::Close, t::INK3, style::ghost(), free.then(|| m(Msg::PairNotSame(i)))).width(24).height(24).padding(0);
        let r = row![
            open,
            tip(merge, merge_tip, true),
            tip(review, "Check the two side by side, large: Y merges, N says they're different", true),
            tip(not, "Not the same fursuit: don't suggest them together again", true)
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .padding(Padding::from([4.0, 8.0]));
        rows = rows.push(cols::drop_target(look, Target::Group(other.clone()), r.into()));
    }
    let all = e.shown_pairs(&s.all);
    if let Some(&(next, _)) = all.get(PAIRS_N) {
        let label = text(format!("{} more: review them", all.len() - PAIRS_N)).size(t::T_SMALL).color(if can { t::INK2 } else { t::INK4 });
        let b = button(label).padding(Padding::from([4.0, 8.0])).style(style::ghost().style()).on_press_maybe(can.then(|| m(Msg::PairReview(next))));
        rows = rows.push(b);
    }
    let head = row![icons::view(Glyph::Merge, 13.0, t::INK3), text("Likely the same fursuit").size(t::T_SMALL).color(t::INK3)].spacing(6).align_y(Alignment::Center);
    let boxed = container(rows).width(Length::Fill).style(|_| container::Style { border: style::border(t::LINE_HI, 1.0, t::R_CONTROL), ..Default::default() });
    Some(column![container(head).padding(Padding { left: 8.0, top: 4.0, bottom: 2.0, right: 0.0 }), boxed, Space::new().height(6)].spacing(4).into())
}

pub fn debug_state(app: &App) -> String {
    let Some(e) = app.subjects.editor.as_ref() else { return "editor=none".into() };
    let cols: Vec<String> = e
        .cols
        .iter()
        .map(|c| {
            let what = match &c.origin {
                Origin::Existing { id, .. } => super::people::short(id),
                Origin::New => "new".into(),
            };
            format!("{what}:{}{}", c.heads.len(), if c.name.is_empty() { String::new() } else { format!("\"{}\"", c.name) })
        })
        .collect();
    let (stay, moved, removed, taken) = e.counts();
    format!(
        "editor=[kind={:?} cols={:?} sent={:?} picked={}@{:?} counts=({stay},{moved},{removed},{taken}) edited={} near={} suggest={} note={:?} history={} pairs={:?} changed={} follow={} lib={:?}]",
        e.kind,
        cols,
        e.sent.iter().map(|(g, v)| (super::people::short(g), v.len())).collect::<Vec<_>>(),
        e.ui.picked.len(),
        e.ui.picked_in,
        e.edited(),
        e.near.len(),
        match (&e.suggestion, e.suggestion_shown()) {
            (None, _) => "none".to_string(),
            (Some(s), false) => format!("offered:{}", s.off.len()),
            (Some(s), true) => format!("shown:{}", s.off.len()),
        },
        e.note,
        e.history.len(),
        e.shown_pairs(&app.subjects.all).iter().map(|(_, g)| super::people::short(g)).collect::<Vec<_>>(),
        e.changed(),
        e.follow.is_some(),
        e.lib_steps.iter().map(|s| if matches!(s, LibStep::NotSame) { "notsame" } else { "merged" }).collect::<Vec<_>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::subjects::SubjectId;
    use chrono::NaiveDate;

    fn h(n: u8) -> FaceRef {
        FaceRef::new(PhotoKey([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }
    /// The second head of photo `n`.
    fn h2(n: u8) -> FaceRef {
        FaceRef::new(PhotoKey([n; 16]), [0.6, 0.1, 0.2, 0.2])
    }

    fn face(f: FaceRef) -> Face {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        Face { face: f, jpg: None, raw: None, day, capture: day.and_hms_opt(10, 0, 0).unwrap(), burst: None, clarity: 0.5, confirmed: false }
    }

    /// An editor on `home` (heads read), with `preset` laid out.
    fn editor(home: GroupId, heads: &[FaceRef], preset: Preset) -> Editor {
        let mut e = Editor::new(Kind::Fursuit, home.clone(), None, preset);
        for f in heads {
            e.faces.insert(*f, face(*f));
            e.origin.insert(*f, home.clone());
        }
        e.cols[0].heads = heads.to_vec();
        e.cols[0].loaded = true;
        e.lay_out();
        e
    }

    fn mv(from: usize, faces: &[FaceRef], to: Target) -> cols::Move {
        cols::Move { from, faces: faces.to_vec(), to }
    }

    /// The presets: the picked heads split off, the picked heads picked;
    /// side by side has both groups as columns.
    #[test]
    fn each_preset_starts_as_its_task_needs() {
        let home = GroupId::Cluster(1);
        let e = editor(home.clone(), &[h(1), h(2)], Preset::Split(vec![h(2)]));
        assert_eq!((e.cols.len(), e.cols[1].heads.clone(), e.suggestion_shown()), (2, vec![h(2)], false));
        let e = editor(home.clone(), &[h(1), h(2), h(3)], Preset::Pick(vec![h(1), h(3)]));
        assert_eq!((e.cols.len(), e.ui.picked.clone(), e.ui.picked_in), (1, vec![h(1), h(3)], Some(0)));
        let mut e = Editor::new(Kind::Fursuit, home, None, Preset::With(GroupId::Cluster(2)));
        e.cols.push(Col { origin: Origin::Existing { id: GroupId::Cluster(2), was: None }, heads: vec![], name: String::new(), rank: HashMap::new(), loaded: false });
        assert_eq!(e.cols.len(), 2);
    }

    /// A suggested split: offered only for a group with two heads in one
    /// photo (and a split proposed around them); laid out, it is the old
    /// proposal's layout (the smaller side in a new column, the pair
    /// apart), not a move by hand, recorded as a proposal; with the user's
    /// arrangement already made, it adds its column and moves nothing
    /// else; Ctrl+Z takes it back.
    #[test]
    fn a_suggested_split_is_offered_then_laid_out_without_undoing_the_users_moves() {
        assert_eq!(Suggestion::from_check(&[], None), None, "not flagged: nothing offered");
        assert_eq!(Suggestion::from_check(&[(h(2), h2(2))], None), None, "no split proposed: nothing offered");
        let split = (vec![h(1), h(2), h(4)], vec![h2(2), h(3)]);
        let sg = Suggestion::from_check(&[(h(2), h2(2))], Some(&split)).unwrap();
        assert_eq!((sg.pair, sg.off.clone()), ((h(2), h2(2)), vec![h2(2), h(3)]), "the smaller side, its pair head with it");
        let home = GroupId::Cluster(1);
        // Laid out on a fresh editor: the old proposal preset's layout.
        let mut e = editor(home.clone(), &[h(1), h(2), h2(2), h(3), h(4)], Preset::Plain);
        e.suggestion = Some(sg.clone());
        assert!(!e.suggestion_shown());
        assert!(e.lay_out_suggestion());
        assert_eq!((e.cols[0].heads.clone(), e.cols[1].heads.clone()), (vec![h(1), h(2), h(4)], vec![h2(2), h(3)]));
        assert!(e.suggestion_shown() && e.edited() == 0, "laid out, not moved by hand");
        assert!(!e.lay_out_suggestion(), "once");
        assert!(matches!(e.apply_move(&mv(1, &[h2(2)], Target::Col(0))), Err(Some(_))), "the pair stays apart");
        assert!(e.take_back());
        assert!(!e.suggestion_shown() && e.cols.len() == 1 && e.edited() == 0, "Ctrl+Z takes it back");
        // The user arranged first: h(3) to Kiba, h(4) to a new column.
        let mut e = editor(home, &[h(1), h(2), h2(2), h(3), h(4)], Preset::Plain);
        let kiba = GroupId::Subject(crate::model::subjects::SubjectId("kiba".into()));
        e.photos_of.insert(kiba.clone(), HashSet::new());
        e.apply_move(&mv(0, &[h(3)], Target::Group(kiba.clone()))).unwrap();
        e.apply_move(&mv(0, &[h(4)], Target::New)).unwrap();
        e.suggestion = Some(sg);
        assert!(e.lay_out_suggestion());
        assert_eq!(e.cols.len(), 3, "its own column, after the user's");
        assert_eq!(e.cols[2].heads, vec![h2(2)], "only what was still in Stays");
        assert_eq!((e.place_of(&h(3)), e.place_of(&h(4))), (Some(Place::Sent(kiba)), Some(Place::Col(1))), "the user's moves stand");
        assert_eq!(e.edited(), 2, "the user's two moves, not the suggestion's");
    }

    /// Two heads of one photo never share a column, nor a panel group
    /// holding (or sent) the other; taking out and removing are always
    /// allowed; a group not read yet can't be judged.
    #[test]
    fn two_heads_of_one_photo_never_share_a_column_or_a_group() {
        let home = GroupId::Cluster(1);
        let mut e = editor(home, &[h(1), h(2), h2(2), h(3)], Preset::Split(vec![h2(2)]));
        assert!(matches!(e.apply_move(&mv(1, &[h2(2)], Target::Col(0))), Err(Some(_))), "back with its pair");
        assert!(matches!(e.apply_move(&mv(0, &[h(2), h(3)], Target::New)), Ok(())), "a new column of its own is fine");
        let kiba = GroupId::Subject(SubjectId("kiba".into()));
        e.photos_of.insert(kiba.clone(), [h(9).key].into_iter().collect());
        assert!(e.apply_move(&mv(1, &[h2(2)], Target::Group(kiba.clone()))).is_ok());
        assert!(matches!(e.apply_move(&mv(2, &[h(2)], Target::Group(kiba.clone()))), Err(Some(_))), "Kiba holds the other head now");
        e.photos_of.insert(kiba.clone(), [h(1).key].into_iter().collect());
        assert!(matches!(e.refuse(&[h(1)], &Target::Group(kiba)), Err(Some(_))), "Kiba's own members count");
        assert!(matches!(e.refuse(&[h(1)], &Target::Group(GroupId::Cluster(77))), Err(None)), "not read yet");
        assert!(e.refuse(&[h(1), h(2)], &Target::TakenOut).is_ok() && e.refuse(&[h(1)], &Target::Removed).is_ok());
    }

    /// The arrangement becomes one edit, by head: new columns (empty ones
    /// left out), a panel group, an opened group, removals, take-outs and
    /// a name; only heads whose place changed; automatic groups known by
    /// their heads.  Ctrl+Z takes back one step at a time.
    #[test]
    fn the_arrangement_becomes_one_edit_by_head() {
        let home = GroupId::Cluster(1);
        let mut e = editor(home.clone(), &[h(1), h(2), h(3), h(4), h(5), h(6)], Preset::Plain);
        let kiba = GroupId::Subject(SubjectId("kiba".into()));
        let other = GroupId::Cluster(2);
        e.photos_of.insert(kiba.clone(), HashSet::new());
        e.cols.push(Col { origin: Origin::Existing { id: other.clone(), was: None }, heads: vec![h(20)], name: String::new(), rank: HashMap::new(), loaded: true });
        e.origin.insert(h(20), other.clone());
        e.apply_move(&mv(0, &[h(1)], Target::New)).unwrap();
        e.cols.push(Col { origin: Origin::New, heads: vec![], name: String::new(), rank: HashMap::new(), loaded: true });
        e.cols[2].name = "Rook".into();
        e.apply_move(&mv(0, &[h(2)], Target::Group(kiba.clone()))).unwrap();
        e.apply_move(&mv(0, &[h(3)], Target::Col(1))).unwrap();
        e.apply_move(&mv(0, &[h(4)], Target::Removed)).unwrap();
        e.apply_move(&mv(0, &[h(5)], Target::TakenOut)).unwrap();
        e.cols[0].name = "Ash".into();
        assert_eq!(e.counts(), (1, 3, 1, 1));
        assert_eq!(e.edited(), 5);
        let edit = e.to_edit(&|_| None);
        assert_eq!(edit.new_names, vec![Some("Rook".to_string())], "the empty column is left out");
        let dest = |f: FaceRef| edit.moves.iter().find(|m| m.0 == f).map(|m| m.1);
        assert_eq!(dest(h(1)), Some(EditDest::New(0)));
        assert_eq!(dest(h(6)), None, "it stays: no move");
        assert_eq!(dest(h(20)), None);
        assert_eq!((dest(h(4)), dest(h(5))), (Some(EditDest::Removed), Some(EditDest::TakenOut)));
        let target = |f: FaceRef| match dest(f) {
            Some(EditDest::Group(i)) => edit.targets[i].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(target(h(2)), EditTarget::Subject(SubjectId("kiba".into())));
        assert!(matches!(target(h(3)), EditTarget::ClusterOf(heads) if heads == vec![h(20)]), "known by its heads");
        assert!(matches!(&edit.renames[..], [(i, n)] if n == "Ash" && matches!(&edit.targets[*i], EditTarget::ClusterOf(v) if v.contains(&h(6)))));
        // Ctrl+Z, step by step.
        assert!(e.take_back());
        assert!(e.taken.is_empty() && e.removed.len() == 1);
        while e.take_back() {}
        assert_eq!(e.cols[0].heads.len(), 6);
        assert_eq!(e.counts(), (6, 0, 0, 0));
    }
}
