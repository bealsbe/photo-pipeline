//! People and fursuits (Ctrl+4): a page of their own on the side
//! rail, beside Search, with a tab for each kind.  People work in this
//! phase.  Pets are on hold: no tab, though the model keeps the kind.
//!
//! - **The People tab** lists the groups of faces found while indexing:
//!   the named first, then the groups to name, largest first, each a face
//!   crop with its name (or "Unnamed") and how many photos.  Groups under
//!   the smallest size to show (5 photos by default) fold away under
//!   *Smaller groups*, so an event's strangers stay out of sight; hidden
//!   ones wait under *Hidden*.  Nothing is ever named for the user.
//! - **Picking several** (S, or Ctrl+click) gives a strip with *Merge…*
//!   and *Hide*; merging asks which name to keep.  Ctrl+Z takes back the
//!   last change to people.
//! - **A person's page** (`subjects_person`) is a search for them: their
//!   photos clearest first in the results grid, words narrowing them, the
//!   band to name, rename, review and split them.
//! - **The review** (`subjects_review`), "Same person?", one suggestion at
//!   a time with Y and N in the keep and prune colours.
//! - **Before faces were looked for**, the tab says what finding them in
//!   the indexed library costs and runs it only when asked.
//!
//! Kinds never mix: a tab shows one kind; merges, moves and suggestions
//! only ever involve groups of that kind.  Library pages take no marks.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::image::Handle;
use iced::widget::{Space, button, column, container, opaque, row, scrollable, stack, text};
use iced::{Alignment, Background, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{self, Face, FaceRef, Framing, Group, GroupId, KINDS, Kind, KindWords, Subjects as Found};
use super::results_pane::Pane;
use super::style;
use super::subjects_person::{self as person, Person};
use super::subjects_review::{self as review, Review};
use super::{App, Message, View};
use crate::ml::Device;
use crate::ml::models::ModelId;
use crate::search::PassKind;
use crate::search::estimate;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile::{self as tile, Shape, Tile};
use crate::ui::theme as t;
use crate::util;

pub const HEADER_H: f32 = 60.0;
/// The face crops kept decoded, by bytes.
const CROP_CACHE_BYTES: usize = 48 * 1024 * 1024;
/// Crops drawn on one worker at a time, in a batch.
const CROP_BATCH: usize = 48;

pub fn m(msg: Msg) -> Message {
    Message::Subjects(msg)
}

// ── state ───────────────────────────────────────────────────────────────────

/// What the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The tab: its groups.
    Overview,
    /// A person's photos (or faces).
    Person,
    /// "Same person?"
    Review,
    /// "Same fursuit?" for pairs of groups: merge suggestions.
    Merges,
}

/// Merging picked groups: which name to keep.
#[derive(Debug, Clone)]
pub struct MergeAsk {
    pub groups: Vec<GroupId>,
    pub keep: GroupId,
}

impl MergeAsk {
    /// Merge `groups` (as `all` knows them): the named one keeps its
    /// name, so the first named is kept by default; with none named,
    /// `prefer` (the tile dropped onto), else the first.
    pub fn new(all: &Found, groups: Vec<GroupId>, prefer: Option<&GroupId>) -> Option<MergeAsk> {
        if groups.len() < 2 {
            return None;
        }
        let named = |id: &GroupId| all.group(id).is_some_and(|g| g.name.is_some());
        let keep = prefer.filter(|p| named(p)).or_else(|| groups.iter().find(|id| named(id))).or(prefer).unwrap_or(&groups[0]).clone();
        Some(MergeAsk { groups, keep })
    }

    /// More than one of the groups has a name: the dialog asks which to
    /// keep (otherwise the one name there is stays).
    pub fn asks_name(&self, all: &Found) -> bool {
        self.groups.iter().filter(|id| all.group(id).is_some_and(|g| g.name.is_some())).count() > 1
    }
}

/// A group tile being dragged onto another (to merge them), and the
/// tile the pointer is over.
#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub from: GroupId,
    pub over: Option<GroupId>,
}

pub struct Subjects {
    /// The tab shown (saved).
    pub tab: Kind,
    /// Every group of every kind, as last reported.
    pub all: Arc<Found>,
    /// Groups picked on the tab (S or Ctrl+click), in the order picked.
    pub picked: Vec<GroupId>,
    pub select_mode: bool,
    /// The keyboard's place among the tiles.
    pub focus: Option<GroupId>,
    pub small_open: bool,
    pub hidden_open: bool,
    pub merge: Option<MergeAsk>,
    /// A tile dragged onto another, while the button is down.
    pub drag: Option<Drag>,
    /// "Forget this person?" waiting for an answer.
    pub forget: Option<GroupId>,
    pub person: Option<Person>,
    pub review: Option<Review>,
    /// The close-call judge's latest run (fursuits).
    pub judge: Option<super::fursuit_judge::Run>,
    /// Pairs of fursuit groups that may be one suit (stage 2): the tab's
    /// and a group page's lists, and their review.
    pub lists: super::subjects_merge::Lists,
    pub merges: Option<super::subjects_merge::MergeReview>,
    pub crops: Crops,
    pub(super) seq: u64,
}

impl Subjects {
    pub fn new(app_tab: u8, link: &super::search_link::Link) -> Subjects {
        Subjects {
            tab: people::kind_from_index(app_tab),
            all: link.subjects(),
            picked: Vec::new(),
            select_mode: false,
            focus: None,
            small_open: false,
            hidden_open: false,
            merge: None,
            drag: None,
            forget: None,
            person: None,
            review: None,
            judge: None,
            lists: Default::default(),
            merges: None,
            crops: Crops::new(CROP_CACHE_BYTES),
            seq: 0,
        }
    }

    pub fn page(&self) -> Page {
        if self.merges.is_some() {
            Page::Merges
        } else if self.review.is_some() {
            Page::Review
        } else if self.person.is_some() {
            Page::Person
        } else {
            Page::Overview
        }
    }

    /// The person's photos, while their page is open.
    pub fn pane(&self) -> Option<&Pane> {
        self.person.as_ref().filter(|_| self.review.is_none()).map(|p| &p.pane)
    }

    pub fn pane_mut(&mut self) -> Option<&mut Pane> {
        if self.review.is_some() {
            return None;
        }
        self.person.as_mut().map(|p| &mut p.pane)
    }

    pub fn group(&self, id: &GroupId) -> Option<&Group> {
        self.all.group(id)
    }

    pub(super) fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// The groups of one kind as its tab lays them out.
#[derive(Debug, Default)]
pub struct Arranged<'a> {
    /// Named, most photos first.
    pub named: Vec<&'a Group>,
    /// Unnamed, at least the smallest size to show, largest first.
    pub to_name: Vec<&'a Group>,
    /// Unnamed and smaller: folded away under *Smaller groups*.
    pub small: Vec<&'a Group>,
    /// Hidden, named or not.
    pub hidden: Vec<&'a Group>,
}

/// Lay out `kind`'s groups: everything else is left out, so a tab never
/// shows another kind.
pub fn arrange(all: &Found, kind: Kind, smallest: usize) -> Arranged<'_> {
    let mut a = Arranged::default();
    for g in all.groups.iter().filter(|g| g.kind == kind) {
        if g.hidden {
            a.hidden.push(g);
        } else if g.name.is_some() {
            a.named.push(g);
        } else if g.photos >= smallest {
            a.to_name.push(g);
        } else {
            a.small.push(g);
        }
    }
    let by_size = |x: &&Group, y: &&Group| y.photos.cmp(&x.photos).then(x.cover.capture.cmp(&y.cover.capture));
    a.named.sort_by(by_size);
    a.to_name.sort_by(by_size);
    a.small.sort_by(by_size);
    a
}

impl Arranged<'_> {
    /// The tiles the arrow keys step through, in the page's order.
    pub fn order(&self, small_open: bool, hidden_open: bool) -> Vec<GroupId> {
        let mut v: Vec<GroupId> = self.named.iter().chain(&self.to_name).map(|g| g.id.clone()).collect();
        if small_open {
            v.extend(self.small.iter().map(|g| g.id.clone()));
        }
        if hidden_open {
            v.extend(self.hidden.iter().map(|g| g.id.clone()));
        }
        v
    }
}

// ── crops: decoded off the UI thread, kept by bytes ────────────────────────

/// A crop as cached: the face, its size in pixels, and whether it is the
/// whole photo (else the face, framed).
pub type CropKey = (FaceRef, u32, bool);

/// Decoded crops, the least recently wanted dropped first past the cap.
pub struct Crops {
    map: HashMap<CropKey, (Option<Handle>, (u32, u32), usize, u64)>,
    bytes: usize,
    cap: usize,
    pending: HashSet<CropKey>,
    tick: u64,
}

/// The sizes crops are drawn at, in pixels: a wanted size takes the next
/// one up, so a few sizes serve every place.
const BUCKETS: [u32; 5] = [64, 128, 256, 384, 640];

pub fn bucket(px: f32) -> u32 {
    let px = px.ceil() as u32;
    BUCKETS.iter().copied().find(|&b| b >= px).unwrap_or(BUCKETS[BUCKETS.len() - 1])
}

impl Crops {
    pub fn new(cap: usize) -> Crops {
        Crops { map: HashMap::new(), bytes: 0, cap, pending: HashSet::new(), tick: 0 }
    }

    pub fn key(face: &FaceRef, px: u32, framing: Framing) -> CropKey {
        (*face, px, framing == Framing::Photo)
    }

    /// The crop, if decoded (`None` too when the photo couldn't be read).
    pub fn get(&self, face: &FaceRef, px: u32, framing: Framing) -> Option<Handle> {
        self.map.get(&Crops::key(face, px, framing)).and_then(|e| e.0.clone())
    }

    /// The decoded size of a crop (a photo's aspect).
    pub fn size(&self, face: &FaceRef, px: u32, framing: Framing) -> Option<(u32, u32)> {
        self.map.get(&Crops::key(face, px, framing)).map(|e| e.1)
    }

    /// Of `wanted`, the ones not decoded nor on their way; they are marked
    /// on their way.  The ones decoded count as just used.
    pub fn missing(&mut self, wanted: impl IntoIterator<Item = (Face, u32, Framing)>) -> Vec<(CropKey, Face)> {
        let mut out = Vec::new();
        for (f, px, fr) in wanted {
            let k = Crops::key(&f.face, px, fr);
            if let Some(e) = self.map.get_mut(&k) {
                self.tick += 1;
                e.3 = self.tick;
                continue;
            }
            if self.pending.insert(k) {
                out.push((k, f));
            }
        }
        out
    }

    /// Crops arrived.
    pub fn put(&mut self, batch: Vec<(CropKey, Option<(u32, u32, Vec<u8>)>)>) {
        for (k, px) in batch {
            self.pending.remove(&k);
            self.tick += 1;
            let (handle, size, bytes) = match px {
                Some((w, h, rgba)) => {
                    let n = rgba.len();
                    (Some(Handle::from_rgba(w, h, rgba)), (w, h), n)
                }
                None => (None, (0, 0), 0),
            };
            if let Some(old) = self.map.insert(k, (handle, size, bytes, self.tick)) {
                self.bytes -= old.2;
            }
            self.bytes += bytes;
        }
        while self.bytes > self.cap {
            let Some((&k, _)) = self.map.iter().min_by_key(|(_, e)| e.3) else { break };
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.2;
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Forget every crop (the faces were forgotten).
    pub fn clear(&mut self) {
        self.map.clear();
        self.pending.clear();
        self.bytes = 0;
    }
}

/// Draw the crops `wanted` that aren't decoded yet, on workers.
pub fn fetch(app: &mut App, wanted: Vec<(Face, u32, Framing)>) -> Task<Message> {
    let missing = app.subjects.crops.missing(wanted);
    if missing.is_empty() {
        return Task::none();
    }
    let cropper = app.search.link.cropper();
    let tasks = missing.chunks(CROP_BATCH).map(|chunk| {
        let keys: Vec<CropKey> = chunk.iter().map(|(k, _)| *k).collect();
        let (cropper, chunk) = (cropper.clone(), chunk.to_vec());
        let work = move || {
            // A few threads: on the share each crop is a preview read.
            let mut out: Vec<(CropKey, Option<(u32, u32, Vec<u8>)>)> = Vec::with_capacity(chunk.len());
            std::thread::scope(|s| {
                let handles: Vec<_> = chunk.chunks(chunk.len().div_ceil(4).max(1)).map(|part| s.spawn(|| part.iter().map(|(k, f)| (*k, cropper.crop(f, k.1, if k.2 { Framing::Photo } else { Framing::Face }))).collect::<Vec<_>>())).collect();
                for h in handles {
                    out.extend(h.join().unwrap_or_default());
                }
            });
            out
        };
        Task::perform(super::preferences::on_worker("crops", work), move |r| m(Msg::Cropped(r.unwrap_or_else(|| keys.iter().map(|&k| (k, None)).collect()))))
    });
    Task::batch(tasks)
}

/// The crop size for `logical` pixels on this screen.
pub fn px(app: &App, logical: f32) -> u32 {
    bucket(logical * app.scale_factor.max(1.0))
}

/// The crops the page shows now.
fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let v = match s.page() {
        Page::Overview => {
            let a = arrange(&s.all, s.tab, app.settings.people_min_photos());
            let (big, small) = (px(app, tile::TILE_CROP), px(app, tile::SMALL_CROP));
            let mut v: Vec<(Face, u32, Framing)> = a.named.iter().chain(&a.to_name).map(|g| (g.cover.clone(), big, Framing::Face)).collect();
            if s.small_open {
                v.extend(a.small.iter().map(|g| (g.cover.clone(), small, Framing::Face)));
            }
            if s.hidden_open {
                v.extend(a.hidden.iter().map(|g| (g.cover.clone(), small, Framing::Face)));
            }
            if s.tab == Kind::Fursuit {
                v.extend(super::subjects_merge::wanted_tab(app));
            }
            v
        }
        Page::Person => {
            let mut v = person::wanted(app);
            v.extend(super::subjects_merge::wanted_similar(app));
            v
        }
        Page::Review => review::wanted(app),
        Page::Merges => super::subjects_merge::wanted(app),
    };
    // The merge dialog, over any page.
    let mut v = v;
    if let Some(mg) = &s.merge {
        let size = if mg.groups.len() == 2 { PAIR_CROP } else { 40.0 };
        v.extend(mg.groups.iter().filter_map(|id| s.group(id)).map(|g| (g.cover.clone(), px(app, size), Framing::Face)));
    }
    v
}

/// Fetch what the page shows now and isn't decoded.
pub fn refresh_crops(app: &mut App) -> Task<Message> {
    let w = wanted(app);
    fetch(app, w)
}

// ── messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Msg {
    Tab(Kind),
    /// A group's tile: open its page (Ctrl+click or select mode: pick it).
    Tile(GroupId),
    /// Pick a group, or put it back (as Ctrl+click does).
    Pick(GroupId),
    SelectMode,
    ClearPicks,
    Small(bool),
    Hidden(bool),
    Unhide(GroupId),
    MergeAsk,
    /// A tile's drag: it started, the pointer entered or left a tile, the
    /// button came up (the dialog asks before anything merges).
    DragStart(GroupId),
    DragEnter(GroupId),
    DragLeave(GroupId),
    DragDrop,
    MergeKeep(GroupId),
    MergeGo,
    MergeCancel,
    HidePicked,
    Cropped(Vec<(CropKey, Option<(u32, u32, Vec<u8>)>)>),
    /// The catch-up: find faces in the library indexed before.
    FindFaces,
    OpenPrefs,
    IndexLibrary,
    /// Take back the last change to people (Ctrl+Z, the status bar).
    Undo,
    ForgetCancel,
    ForgetGo,
    Person(person::Msg),
    Review(review::Msg),
    Merges(super::subjects_merge::Msg),
}

// ── getting there ───────────────────────────────────────────────────────────

/// Ctrl+4, the rail or the View menu: the page, on the tab last used.
pub fn open(app: &mut App) -> Task<Message> {
    if app.view == View::Subjects {
        // Again from a person's page or the review: back to the tab.
        if app.subjects.page() != Page::Overview {
            return to_overview(app);
        }
        return Task::none();
    }
    app.switch_view(View::Subjects)
}

/// The view is switching to the page.
pub fn arriving(app: &mut App) -> Task<Message> {
    app.calendar.want_index();
    super::subjects_merge::ask_tab(app);
    let mut tasks = vec![refresh_crops(app)];
    // A person's page left for Open day comes back as it was; an empty one
    // asks again.
    if app.subjects.person.as_ref().is_some_and(|p| p.pane.results.is_none() && p.pending.is_none()) {
        tasks.push(person::search(app, false));
    }
    Task::batch(tasks)
}

/// Back to the tab's groups.
pub fn to_overview(app: &mut App) -> Task<Message> {
    let s = &mut app.subjects;
    let from = s.person.as_ref().map(|p| p.group.clone());
    s.review = None;
    s.person = None;
    s.merges = None;
    if from.is_some() {
        s.focus = from;
    }
    app.quicklook = None;
    super::subjects_merge::ask_tab(app);
    refresh_crops(app)
}

/// The viewer on one of a person's photos (read-only).
pub fn open_viewer(app: &mut App, id: crate::model::collection::RecordId) -> Task<Message> {
    person::open_viewer(app, id)
}

// ── events ──────────────────────────────────────────────────────────────────

/// Ctrl+Z took a change to people back: say what (a fresh `Subjects`
/// follows).
pub fn on_undone(app: &mut App, what: String) -> Task<Message> {
    let t = app.show_status(format!("Undone: {}.", what.trim_end_matches('.')), 6000);
    Task::batch([t, person::after_change(app)])
}

/// A group's faces arrived: the review's reference faces, or a subject's
/// page's split view.
pub fn on_faces(app: &mut App, seq: u64, group: GroupId, faces: Arc<Vec<Face>>) -> Task<Message> {
    if review::on_faces(app, seq, &faces) {
        return refresh_crops(app);
    }
    person::on_faces(app, seq, group, faces)
}

pub fn on_suggestions(app: &mut App, seq: u64, group: GroupId, list: Arc<Vec<Vec<Face>>>) -> Task<Message> {
    let more = person::on_more(app, seq, &group, &list);
    Task::batch([more, review::on_suggestions(app, seq, group, list)])
}

/// The search status changed: say when a faces pass of its own ended.
pub fn on_status(app: &mut App, old: &crate::search::Status) -> Task<Message> {
    let now = &app.search.status;
    let was_faces = old.pass.as_ref().is_some_and(|p| p.kind == PassKind::Faces);
    let is_faces = now.pass.as_ref().is_some_and(|p| p.kind == PassKind::Faces);
    if was_faces && !is_faces && now.stopped.is_none() {
        let text = if old.faces.unrecognised > 0 {
            "The faces were recognised again with the new model. Names were kept.".to_string()
        } else {
            format!("Found {} in the library.", util::plural(now.faces.found as u64, "face"))
        };
        return app.show_status(text, 8000);
    }
    Task::none()
}

pub fn on_subjects(app: &mut App, all: Arc<Found>) -> Task<Message> {
    if all.generation == app.subjects.all.generation && *all == *app.subjects.all {
        return Task::none();
    }
    if all.groups.is_empty() && !app.subjects.all.groups.is_empty() {
        app.subjects.crops.clear();
    }
    let s = &mut app.subjects;
    s.all = all;
    // Picks and the focus of groups that went (merged, or regrouped).
    let all = s.all.clone();
    s.picked.retain(|id| all.group(id).is_some());
    s.focus = s.focus.take().filter(|id| all.group(id).is_some());
    if let Some(mg) = &s.merge {
        if mg.groups.iter().any(|id| all.group(id).is_none()) {
            s.merge = None;
        }
    }
    if s.drag.as_ref().is_some_and(|d| all.group(&d.from).is_none()) {
        s.drag = None;
    }
    if let Some(d) = s.drag.as_mut().filter(|d| d.over.as_ref().is_some_and(|o| all.group(o).is_none())) {
        d.over = None;
    }
    let t = person::on_subjects(app);
    super::subjects_merge::on_subjects(app);
    Task::batch([t, refresh_crops(app)])
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Tab(k) => {
            let s = &mut app.subjects;
            // Pets are on hold: no tab for them, so none to switch to.
            if s.tab != k && KINDS.contains(&k) {
                s.tab = k;
                s.picked.clear();
                s.focus = None;
                s.select_mode = false;
                app.settings.subjects_tab = k.index();
                app.settings.save_in_background();
            }
            to_overview(app)
        }
        Msg::Tile(id) => {
            let ctrl = crate::ui::modifiers::get().control();
            let s = &mut app.subjects;
            if ctrl || s.select_mode {
                toggle_pick(s, id);
                return Task::none();
            }
            s.focus = Some(id.clone());
            person::open(app, &id, false)
        }
        Msg::Pick(id) => {
            toggle_pick(&mut app.subjects, id);
            Task::none()
        }
        Msg::SelectMode => {
            select_mode(app);
            Task::none()
        }
        Msg::ClearPicks => {
            let s = &mut app.subjects;
            s.picked.clear();
            s.select_mode = false;
            Task::none()
        }
        Msg::Small(open) => {
            app.subjects.small_open = open;
            refresh_crops(app)
        }
        Msg::Hidden(open) => {
            app.subjects.hidden_open = open;
            refresh_crops(app)
        }
        Msg::Unhide(id) => {
            let name = label(&app.subjects, &id);
            app.search.link.set_hidden(&id, false);
            app.show_status(format!("{name} is back on the tab."), 5000)
        }
        Msg::MergeAsk => {
            let s = &mut app.subjects;
            let groups: Vec<GroupId> = s.picked.iter().filter(|id| s.all.group(id).is_some_and(|g| g.kind == s.tab)).cloned().collect();
            // The named one keeps its name by default.
            let Some(ask) = MergeAsk::new(&s.all, groups, None) else { return Task::none() };
            s.merge = Some(ask);
            refresh_crops(app)
        }
        Msg::DragStart(id) => {
            let s = &mut app.subjects;
            if s.page() == Page::Overview && s.merge.is_none() && s.group(&id).is_some_and(|g| g.kind == s.tab && !g.hidden) {
                s.drag = Some(Drag { from: id, over: None });
            }
            Task::none()
        }
        Msg::DragEnter(id) => {
            if let Some(d) = app.subjects.drag.as_mut() {
                d.over = (d.from != id).then_some(id);
            }
            Task::none()
        }
        Msg::DragLeave(id) => {
            if let Some(d) = app.subjects.drag.as_mut().filter(|d| d.over.as_ref() == Some(&id)) {
                d.over = None;
            }
            Task::none()
        }
        Msg::DragDrop => {
            let s = &mut app.subjects;
            let Some(Drag { from, over: Some(to) }) = s.drag.take() else { return Task::none() };
            // One kind only, the tab's; never a hidden group.
            let ok = |id: &GroupId| s.all.group(id).is_some_and(|g| g.kind == s.tab && !g.hidden);
            if !ok(&from) || !ok(&to) {
                return Task::none();
            }
            // The tile dropped onto is kept, unless only the dragged one
            // has a name.
            s.merge = MergeAsk::new(&s.all, vec![to.clone(), from], Some(&to));
            s.focus = Some(to);
            refresh_crops(app)
        }
        Msg::MergeKeep(id) => {
            if let Some(mg) = app.subjects.merge.as_mut() {
                mg.keep = id;
            }
            Task::none()
        }
        Msg::MergeCancel => {
            app.subjects.merge = None;
            super::subjects_merge::dialog_cancelled(app);
            Task::none()
        }
        Msg::MergeGo => {
            let s = &mut app.subjects;
            let Some(mg) = s.merge.take() else { return Task::none() };
            // One kind only: the tab's.
            if mg.groups.iter().any(|id| s.all.group(id).is_none_or(|g| g.kind != s.tab)) {
                return app.show_status("Only groups of one kind can be merged.", 5000);
            }
            let others: Vec<GroupId> = mg.groups.iter().filter(|id| **id != mg.keep).cloned().collect();
            let name = label(s, &mg.keep);
            s.picked.clear();
            s.select_mode = false;
            s.focus = Some(mg.keep.clone());
            // A fursuit's page merged away follows the group kept.
            if app.subjects.person.as_ref().is_some_and(|p| p.group != mg.keep && mg.groups.contains(&p.group)) {
                person::follow(app, &mg.keep);
            }
            app.search.link.merge(&mg.keep, &others);
            let review = super::subjects_merge::merged_by_dialog(app);
            let said = app.show_status_action(format!("Merged {} into {name}.", util::plural(others.len() as u64 + 1, "group")), "Undo", m(Msg::Undo));
            Task::batch([review, said])
        }
        Msg::HidePicked => {
            let s = &mut app.subjects;
            let ids: Vec<GroupId> = std::mem::take(&mut s.picked);
            s.select_mode = false;
            for id in &ids {
                app.search.link.set_hidden(id, true);
            }
            let text = if ids.len() == 1 {
                match app.subjects.group(&ids[0]).and_then(|g| g.name.clone()) {
                    Some(n) => format!("{n} is hidden. Their photos still come up when you search for them by name."),
                    None => "The group is hidden. Find it under Hidden.".to_string(),
                }
            } else {
                format!("{} hidden. Find them under Hidden.", util::plural(ids.len() as u64, "group"))
            };
            app.show_status_action(text, "Undo", m(Msg::Undo))
        }
        Msg::Cropped(batch) => {
            app.subjects.crops.put(batch);
            Task::none()
        }
        Msg::FindFaces => find_faces(app),
        Msg::OpenPrefs => super::preferences::open_at(app, super::preferences::Control::FindFaces),
        Msg::IndexLibrary => {
            let t = super::search::open(app);
            Task::batch([t, Task::done(Message::Search(super::search::Msg::IndexLibrary))])
        }
        Msg::Undo => undo(app),
        Msg::ForgetCancel => {
            app.subjects.forget = None;
            Task::none()
        }
        Msg::ForgetGo => {
            let Some(id) = app.subjects.forget.take() else { return Task::none() };
            let name = label(&app.subjects, &id);
            app.search.link.forget(&id);
            let t = to_overview(app);
            let s = app.show_status_action(format!("Forgot {name}: the name and the photos you confirmed. Its faces went back to the groups to name."), "Undo", m(Msg::Undo));
            Task::batch([t, s])
        }
        Msg::Person(msg) => person::update(app, msg),
        Msg::Review(msg) => review::update(app, msg),
        Msg::Merges(msg) => super::subjects_merge::update(app, msg),
    }
}

fn toggle_pick(s: &mut Subjects, id: GroupId) {
    match s.picked.iter().position(|x| *x == id) {
        Some(i) => {
            s.picked.remove(i);
        }
        None => s.picked.push(id.clone()),
    }
    s.focus = Some(id);
}

/// S on the tab: pick several (again: stop, keeping nothing picked).
pub fn select_mode(app: &mut App) {
    let s = &mut app.subjects;
    let tab = s.tab;
    if s.page() != Page::Overview || !has_groups(app, tab) {
        return;
    }
    let s = &mut app.subjects;
    s.select_mode = !s.select_mode;
    if !s.select_mode {
        s.picked.clear();
    } else if let Some(f) = s.focus.clone().filter(|f| !s.picked.contains(f)) {
        s.picked.push(f);
    }
}

/// Ctrl+Z on the page: the last change to people.
pub fn undo(app: &mut App) -> Task<Message> {
    if app.subjects.page() == Page::Review {
        return review::take_back(app);
    }
    if app.subjects.page() == Page::Merges {
        return super::subjects_merge::take_back(app);
    }
    app.search.link.undo_subjects();
    Task::none()
}

/// A group's name for a sentence: "Person A", or "the unnamed group".
pub fn label(s: &Subjects, id: &GroupId) -> String {
    s.group(id).and_then(|g| g.name.clone()).unwrap_or_else(|| "the unnamed group".into())
}

/// The face model faces are found with: the one in use, else the one
/// chosen in Preferences (the recommended one until then).
pub fn face_model(app: &App) -> ModelId {
    app.search.status.faces.model.unwrap_or_else(|| app.settings.face_model())
}

/// Find faces in the library indexed before faces were looked for: when
/// the models are in place it runs now (the page stated the cost); else
/// the download dialog asks first, with only the faces' rows.
fn find_faces(app: &mut App) -> Task<Message> {
    if !app.search.status.faces.installed {
        let d = super::search::Dialog { faces_only: true, faces: true, ..super::search::Dialog::new(super::search::Stage::Ask, &app.settings) };
        app.search.dialog = Some(d);
        return Task::none();
    }
    app.search.link.find_faces();
    app.show_status("Finding faces in the library. Groups appear as they are found, newest days first.", 6000)
}

// ── keys ────────────────────────────────────────────────────────────────────

/// Esc on the page: a dialog or menu first, then picks, then the page
/// below (the review stops; a person's page goes back to the tab).
pub fn escape(app: &mut App) -> Task<Message> {
    let s = &mut app.subjects;
    if s.drag.take().is_some() || s.forget.take().is_some() {
        return Task::none();
    }
    if s.merge.take().is_some() {
        super::subjects_merge::dialog_cancelled(app);
        return Task::none();
    }
    let s = &mut app.subjects;
    match s.page() {
        Page::Review => review::update(app, review::Msg::Done),
        Page::Merges => super::subjects_merge::update(app, super::subjects_merge::Msg::Done),
        Page::Person => person::escape(app),
        Page::Overview => {
            if !s.picked.is_empty() || s.select_mode {
                s.picked.clear();
                s.select_mode = false;
            }
            Task::none()
        }
    }
}

/// Keys on the page that no command took.
pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if app.subjects.merge.is_some() {
        return match key {
            Key::Named(Named::Enter) => update(app, Msg::MergeGo),
            _ => Task::none(),
        };
    }
    if app.subjects.forget.is_some() {
        return match key {
            Key::Named(Named::Enter) => update(app, Msg::ForgetGo),
            _ => Task::none(),
        };
    }
    match app.subjects.page() {
        Page::Review => review::on_key(app, key, mods),
        Page::Merges => super::subjects_merge::on_key(app, key, mods),
        Page::Person => person::on_key(app, key, mods),
        Page::Overview => overview_key(app, key, mods),
    }
}

fn overview_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if mods.control() || mods.alt() || !has_groups(app, app.subjects.tab) {
        return Task::none();
    }
    let s = &app.subjects;
    let order = arrange(&s.all, s.tab, app.settings.people_min_photos()).order(s.small_open, s.hidden_open);
    let at = s.focus.as_ref().and_then(|f| order.iter().position(|x| x == f));
    match key {
        Key::Named(dir @ (Named::ArrowLeft | Named::ArrowRight)) => {
            let next = match (at, *dir == Named::ArrowRight) {
                (None, _) => 0,
                (Some(i), true) => (i + 1).min(order.len().saturating_sub(1)),
                (Some(i), false) => i.saturating_sub(1),
            };
            if let Some(id) = order.get(next) {
                let s = &mut app.subjects;
                s.focus = Some(id.clone());
                if !s.select_mode {
                    s.picked = vec![id.clone()];
                }
            }
            Task::none()
        }
        Key::Named(Named::Enter) => match s.focus.clone() {
            Some(id) => person::open(app, &id, false),
            None => Task::none(),
        },
        Key::Named(Named::F2) => match s.focus.clone() {
            Some(id) => person::open(app, &id, true),
            None => Task::none(),
        },
        // M: merge the groups picked (the one confirmation asks first).
        Key::Character(c) if c.as_str().eq_ignore_ascii_case("m") && !mods.shift() => {
            if s.picked.len() >= 2 {
                return update(app, Msg::MergeAsk);
            }
            if !s.picked.is_empty() || s.select_mode {
                return app.show_status("Pick two or more groups to merge them.", 3000);
            }
            Task::none()
        }
        // Y on the Fursuits tab: review the pairs that may be one suit.
        Key::Character(c) if c.as_str().eq_ignore_ascii_case("y") && !mods.shift() && s.tab == Kind::Fursuit && !s.select_mode => super::subjects_merge::tab_key(app).unwrap_or_else(Task::none),
        Key::Named(Named::Space) if s.select_mode => {
            if let Some(id) = s.focus.clone() {
                toggle_pick(&mut app.subjects, id);
            }
            Task::none()
        }
        _ => Task::none(),
    }
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    match app.subjects.page() {
        Page::Review => review::view(app),
        Page::Merges => super::subjects_merge::view(app),
        Page::Person => person::view(app, keyboard_ok),
        Page::Overview => overview(app),
    }
}

/// Dialogs over the page: merging, forgetting a person.
pub fn overlay(app: &App) -> Option<Element<'_, Message>> {
    if app.view != View::Subjects {
        return None;
    }
    let s = &app.subjects;
    if let Some(mg) = &s.merge {
        return Some(merge_dialog(app, mg));
    }
    if let Some(id) = &s.forget {
        return Some(forget_dialog(app, id));
    }
    person::overlay(app)
}

/// The glyph of a kind's tab.
pub fn kind_glyph(k: Kind) -> Glyph {
    match k {
        Kind::Person | Kind::Pet => Glyph::Person,
        Kind::Fursuit => Glyph::Fursuit,
    }
}

pub fn shape(k: Kind) -> Shape {
    if k == Kind::Person { Shape::Round } else { Shape::Rounded }
}

/// The tabs: People | Fursuits, each with its glyph.
fn tabs(app: &App) -> Element<'_, Message> {
    let current = app.subjects.tab;
    let seg = |k: Kind| -> Element<'_, Message> {
        let on = k == current;
        let fg = if on { t::INK } else { t::INK2 };
        let content = row![icons::view(kind_glyph(k), 15.0, fg), text(k.title()).size(t::T_BODY).font(paint::MEDIUM)].spacing(6).align_y(Alignment::Center);
        button(container(content).center_y(Length::Fill))
            .height(t::CONTROL_H - 4.0)
            .padding(Padding { left: 9.0, right: 12.0, top: 0.0, bottom: 0.0 })
            .style(style::segment(on).radius(4.0).style())
            .on_press(m(Msg::Tab(k)))
            .into()
    };
    kit::segmented(KINDS.iter().map(|&k| seg(k)).collect())
}

fn header(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let kind = s.tab;
    let caption = match kind {
        Kind::Person => {
            let a = arrange(&s.all, kind, app.settings.people_min_photos());
            if !app.search.status.faces.on && has_groups(app, kind) {
                // Kept from before it was turned off: say why no more come.
                "Finding faces is off, so new photos aren't looked at. Preferences ▸ People turns it on.".to_string()
            } else if a.named.is_empty() && !a.to_name.is_empty() {
                "Faces were found while indexing. Name the people you want to find.".to_string()
            } else if a.named.is_empty() && a.to_name.is_empty() {
                String::new()
            } else {
                format!("{} named, {} to name", a.named.len(), util::plural(a.to_name.len() as u64, "group"))
            }
        }
        Kind::Fursuit => super::subjects_fursuits::caption(app),
        Kind::Pet => String::new(),
    };
    let mut title = column![text(kind.title()).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK)].spacing(1);
    if !caption.is_empty() {
        title = title.push(kit::caption(caption));
    }
    let mut r = row![container(title).padding(Padding { left: t::S2, ..Padding::ZERO }), Space::new().width(Length::Fill), tabs(app)].align_y(Alignment::Center).spacing(t::S2);
    // Always there (quiet without groups), so the tabs never move.
    let can = has_groups(app, kind) && s.page() == Page::Overview;
    let look = if s.select_mode { style::toggle(true, t::SIGNAL) } else { style::ghost() };
    let b = kit::icon_text_btn(Glyph::Select, "Select", if s.select_mode { t::SIGNAL } else { t::INK2 }, look, can.then(|| m(Msg::SelectMode)));
    r = r.push(tip(b, "Pick several groups, to merge or hide them  (S, or Ctrl+click)", true));
    container(r.padding(Padding::from([0.0, t::S3]))).height(HEADER_H).center_y(HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// The tab of `kind` has groups to show (its picking and keys work).
pub fn has_groups(app: &App, kind: Kind) -> bool {
    app.subjects.all.groups.iter().any(|g| g.kind == kind)
}

/// What a tab's body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NoLibrary,
    Reading,
    /// Nothing indexed yet: faces are found with the first index.
    NotIndexed,
    /// Finding faces is off.
    Off,
    /// Indexed before faces were looked for: the catch-up's offer.
    CatchUp,
    /// Looking (the first index, or the catch-up), nothing found yet.
    Looking,
    /// Looked everywhere, found no one.
    NoFaces,
    Groups,
}

/// The body of `kind`'s tab.  Each kind's step answers for itself.
pub fn state(app: &App, kind: Kind) -> State {
    let f = &app.search.status.faces;
    if app.library.is_none() {
        return State::NoLibrary;
    }
    if app.calendar.index.is_none() {
        return State::Reading;
    }
    let st = &app.search.status;
    if has_groups(app, kind) {
        return State::Groups;
    }
    if !f.on {
        return State::Off;
    }
    if st.indexed_shots == 0 && st.pass.is_none() && !app.search.downloading() {
        return State::NotIndexed;
    }
    if app.search.downloading() {
        return State::Looking;
    }
    if st.pass.is_some() && st.stopped.is_none() {
        return State::Looking;
    }
    if f.looked_at < st.indexed_shots {
        return State::CatchUp;
    }
    State::NoFaces
}

fn overview(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let mut col = column![header(app), kit::hairline()].width(Length::Fill).height(Length::Fill);
    if s.picked.len() >= 2 || (s.select_mode && !s.picked.is_empty()) {
        col = col.push(pick_strip(app));
    }
    let body: Element<'_, Message> = match s.tab {
        Kind::Fursuit => super::subjects_fursuits::body(app),
        kind => match state(app, kind) {
            State::NoLibrary => message(Glyph::Library, "Choose your library folder", "People are found in the library, the folder Export files photos into by date.".into(), Some(("Choose library folder…", Message::PickFolder(super::FolderTarget::Library))), None),
            State::Reading => message(Glyph::Person, "Reading the library…", String::new(), None, None),
            State::NotIndexed => message(
                Glyph::Person,
                "People are found when the library is indexed",
                "Indexing the library for search finds faces in the same pass, so you can name people and search for them. Nothing is named for you.".into(),
                Some(("Index the library…", m(Msg::IndexLibrary))),
                None,
            ),
            State::Off => message(Glyph::Person, "Finding faces is off", "Turn it on in Preferences ▸ People to find the people in your photos.".into(), Some(("Open Preferences", m(Msg::OpenPrefs))), None),
            State::CatchUp => catch_up(app),
            State::Looking => looking(app),
            State::NoFaces => message(Glyph::Person, "No faces found yet", "No one was found in the photos indexed. New exports are looked at as they are added.".into(), None, None),
            State::Groups => groups(app),
        },
    };
    col.push(body).into()
}

/// A quiet centred message on the photos' grey.
pub(super) fn later<'a>(glyph: Glyph, title: &'a str, body: &'a str) -> Element<'a, Message> {
    let col = column![
        Space::new().height(Length::FillPortion(2)),
        icons::view(glyph, 30.0, t::PHOTO_INK3),
        Space::new().height(t::S1),
        text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(body).size(t::T_BODY).color(t::PHOTO_INK2).align_x(iced::alignment::Horizontal::Center),
        Space::new().height(Length::FillPortion(5)),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(470);
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
}

/// A page that explains itself on the chrome's ground, as search's do.
pub(super) fn message<'a>(glyph: Glyph, title: &'a str, body: String, primary: Option<(&'a str, Message)>, secondary: Option<(&'a str, Message)>) -> Element<'a, Message> {
    let mut col = column![].spacing(t::S2).align_x(Alignment::Center).max_width(480);
    col = col.push(Space::new().height(Length::FillPortion(3)));
    col = col.push(icons::view(glyph, 44.0, t::INK4));
    col = col.push(Space::new().height(t::S2));
    col = col.push(text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK).align_x(iced::alignment::Horizontal::Center));
    if !body.is_empty() {
        col = col.push(text(body).size(t::T_BODY).color(t::INK2).align_x(iced::alignment::Horizontal::Center));
    }
    let mut buttons = row![].spacing(t::S2);
    if let Some((label, msg)) = primary {
        buttons = buttons.push(kit::btn(label, style::primary(), Some(msg)));
    }
    if let Some((label, msg)) = secondary {
        buttons = buttons.push(kit::btn(label, style::secondary(), Some(msg)));
    }
    col = col.push(Space::new().height(t::S2));
    col = col.push(buttons);
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(t::GROUND)).into()
}

/// One fact of an invitation: what, and a quieter line under it.
pub(super) fn fact<'a>(k: &'a str, v: String, sub: Option<String>) -> Element<'a, Message> {
    let mut words = column![text(v).size(t::T_BODY).color(t::INK)].spacing(2).width(Length::Fill);
    if let Some(sub) = sub {
        words = words.push(text(sub).size(t::T_SMALL).color(t::INK3));
    }
    row![text(k).size(t::T_SMALL).color(t::INK3).width(88), words].spacing(t::S3).padding(Padding::from([10.0, 14.0])).into()
}

/// The library was indexed before faces were looked for: what finding them
/// costs, and the way to start it.  Nothing runs until asked.
fn catch_up(app: &App) -> Element<'_, Message> {
    let st = &app.search.status;
    // The photos not looked at yet: all of them the first time.
    let shots = st.indexed_shots.saturating_sub(st.faces.looked_at);
    let device = st.device.chosen;
    let model = face_model(app);
    let info = crate::ml::models::info(model);
    let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
    let takes = estimate::describe_seconds(estimate::face_catch_up_seconds(shots, device, model));
    let mut facts = vec![
        fact("Reads", format!("Each photo's preview again: {} from the library{}", estimate::describe_bytes(estimate::bytes_read(shots)), if on_share { " on the share" } else { "" }), None),
        fact(
            "Takes",
            super::search_setup::capital(&format!("{takes} {} {}", if device == Device::Gpu { "with" } else { "on" }, device.phrase())),
            Some("You can keep culling, and stop it from the status bar. Groups appear as they are found, newest days first.".into()),
        ),
    ];
    if !st.faces.installed {
        facts.push(fact(
            "Downloads",
            format!("A face model, once: {} ({}), or another you pick", super::search_setup::size(crate::ml::models::bytes_for(&crate::ml::models::face_set(model))), info.name),
            Some("The next step lists each model with its size and licence.".into()),
        ));
    }
    facts.push(fact("Keeps", "The faces found and the names you give, on this computer, readable only by you.".into(), None));
    let mut group = column![];
    for (i, f) in facts.into_iter().enumerate() {
        if i > 0 {
            group = group.push(kit::hairline());
        }
        group = group.push(f);
    }
    let go = if st.faces.installed { "Find faces" } else { "Find faces…" };
    let some = st.faces.looked_at > 0;
    let action = row![kit::btn(go, style::primary(), Some(m(Msg::FindFaces))), kit::caption(format!("{} to look at", util::plural(shots as u64, "photo")))].spacing(t::S3).align_y(Alignment::Center);
    let about = if some {
        "Some of the library was indexed while finding faces was off, or before it existed. Finding them there too groups those faces with the rest. Nothing is named for you."
    } else {
        "The library was indexed before faces were looked for. Finding them groups the faces alike, so you can name the people you want to find and search for them. Nothing is named for you."
    };
    let col = column![
        row![icons::view(Glyph::Person, 26.0, t::INK), text("Find the people in your photos").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK)].spacing(t::S3).align_y(Alignment::Center),
        text(about).size(t::T_BODY).color(t::INK2),
        container(group).width(Length::Fill).style(style::group()),
        text("After this, faces in new exports are found as they are added. Finding faces can be turned off in Preferences ▸ People.").size(t::T_SMALL).color(t::INK3),
        action,
    ]
    .spacing(14)
    .max_width(600);
    container(scrollable(container(col).center_x(Length::Fill).padding(Padding { top: 40.0, bottom: t::S5, left: t::S5, right: t::S5 })).style(style::scroll))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::GROUND))
        .into()
}

/// Faces are being looked for and none is grouped yet.
fn looking(app: &App) -> Element<'_, Message> {
    let body = if app.search.downloading() {
        later(Glyph::Person, "Downloading the models", "Faces are found as the library is indexed, which starts once the models are in place. Groups appear here as they are found.")
    } else {
        later(Glyph::Person, "Looking for faces", "Groups appear here as they are found, newest days first. You can keep culling meanwhile.")
    };
    let col = column![progress_strip(app, Kind::Person).unwrap_or_else(|| Space::new().into()), body];
    container(col).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
}

/// While a kind's step runs (faces looked for, fursuit heads scanned):
/// how far, on the photos' grey; for fursuits also what the step waits
/// for.  Never another kind's.
fn progress_strip(app: &App, kind: Kind) -> Option<Element<'_, Message>> {
    match kind {
        Kind::Person => {}
        Kind::Fursuit => return super::subjects_fursuits::status_card(app),
        Kind::Pet => return None,
    }
    let st = &app.search.status;
    let f = &st.faces;
    if st.stopped.is_some() {
        return None;
    }
    let (line, done, total) = match &st.pass {
        Some(p) if p.kind == PassKind::Faces => {
            let what = if f.unrecognised > 0 { "Recognising the faces again with the new model" } else { "Finding faces" };
            (format!("{what}: {} of {}", util::thousands(p.done as u64), util::plural(p.total as u64, "photo")), p.done, p.total)
        }
        Some(p) if f.on && p.kind == PassKind::FirstIndex => {
            (format!("Indexing finds faces as it goes: {} of {} so far", util::thousands(p.done as u64), util::plural(p.total as u64, "photo")), p.done, p.total)
        }
        _ => return None,
    };
    let left = estimate::describe_seconds(estimate::seconds(total.saturating_sub(done), st.device.chosen));
    let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
    let bar = iced::widget::progress_bar(0.0..=1.0, frac).length(120).girth(4).style(|_| iced::widget::progress_bar::Style {
        background: Background::Color(t::PHOTO_RAISED),
        bar: Background::Color(t::SIGNAL),
        border: iced::Border { radius: 2.0.into(), ..iced::Border::default() },
    });
    let words = column![
        text(line).size(t::T_BODY).font(paint::MEDIUM).color(t::PHOTO_INK),
        text(format!("{} left. Groups appear as they are found, newest days first.", super::search_setup::capital(&left))).size(t::T_SMALL).color(t::PHOTO_INK2)
    ]
    .spacing(2)
    .width(Length::Fill);
    let r = row![icons::view(Glyph::Person, 20.0, t::PHOTO_INK2), words, bar].spacing(14).align_y(Alignment::Center);
    let boxed = container(r)
        .padding(Padding::from([12.0, 14.0]))
        .max_width(760)
        .style(|_| container::Style { border: style::border(t::PHOTO_LINE, 1.0, t::R_CONTROL), ..Default::default() });
    Some(container(boxed).padding(Padding { top: 16.0, left: 16.0, right: 16.0, bottom: 4.0 }).into())
}

/// A section heading on the photos' grey, as the grid draws a day's.
pub fn heading<'a>(label: &str, note: String) -> Element<'a, Message> {
    let mut r = row![text(label.to_string()).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK)].spacing(10).align_y(Alignment::Center);
    if !note.is_empty() {
        r = r.push(text(note).size(t::T_SMALL).color(t::PHOTO_INK3));
    }
    r = r.push(container(Space::new().width(Length::Fill).height(1)).width(Length::Fill).style(style::fill(t::PHOTO_LINE)));
    container(r).height(crate::ui::grid::HEADER_H).center_y(crate::ui::grid::HEADER_H).padding(Padding::from([0.0, 16.0])).width(Length::Fill).into()
}

/// A group's tile on its tab.
fn group_tile<'a>(app: &'a App, g: &'a Group, small: bool) -> Element<'a, Message> {
    let s = &app.subjects;
    let size = if small { tile::SMALL_CROP } else { tile::TILE_CROP };
    let handle = s.crops.get(&g.cover.face, px(app, size), Framing::Face);
    let crop = tile::crop(handle, size, shape(g.kind));
    let name = if small && !g.hidden { None } else { g.name.clone() };
    let count = if g.hidden { "Show".to_string() } else { util::plural(g.photos as u64, "photo") };
    let on = if g.hidden { m(Msg::Unhide(g.id.clone())) } else { m(Msg::Tile(g.id.clone())) };
    let drag = s.drag.as_ref();
    let target = drag.is_some_and(|d| d.over.as_ref() == Some(&g.id));
    let source = drag.is_some_and(|d| d.from == g.id);
    let picked = target || s.picked.contains(&g.id) || (s.picked.is_empty() && s.focus.as_ref() == Some(&g.id) && !g.hidden && app.subjects.select_mode);
    let el: Element<'a, Message> = Tile { crop, name, count, picked, small, badge: g.hidden.then_some(Glyph::Hide), on }.into();
    if g.hidden {
        return tip(el, "Show it on the tab again", true);
    }
    let width = if small { tile::SMALL_W } else { tile::TILE_W };
    // While dragged: the tile left behind is veiled; the one under the
    // pointer says what letting go does.
    let el: Element<'a, Message> = if source {
        stack![el, container(Space::new()).width(width).height(Length::Fill).style(|_| container::Style { background: Some(Background::Color(t::alpha(t::PHOTO_GROUND, 0.6))), border: iced::Border { radius: t::R_CELL.into(), ..iced::Border::default() }, ..Default::default() })].into()
    } else if target {
        let pill = container(row![icons::view(Glyph::Merge, 12.0, t::ON_SIGNAL), text("Merge here").size(t::T_SMALL).font(paint::MEDIUM).color(t::ON_SIGNAL)].spacing(4).align_y(Alignment::Center))
            .padding(Padding::from([2.0, 7.0]))
            .style(|_| container::Style { background: Some(Background::Color(t::SIGNAL)), border: iced::Border { radius: 9.0.into(), ..iced::Border::default() }, ..Default::default() });
        stack![el, container(pill).width(width).align_x(iced::alignment::Horizontal::Center).padding(Padding { top: 6.0, ..Padding::ZERO })].into()
    } else {
        el
    };
    let messages = crate::ui::drag::DragMessages { start: m(Msg::DragStart(g.id.clone())), enter: m(Msg::DragEnter(g.id.clone())), leave: m(Msg::DragLeave(g.id.clone())), drop: m(Msg::DragDrop) };
    let ghost = s.crops.get(&g.cover.face, px(app, size), Framing::Face);
    // The same widgets whether or not a drag is under way, so the dragged
    // tile keeps its place in the tree (and its drag) as the page redraws.
    let el: Element<'a, Message> = crate::ui::drag::DragArea::new(el, messages, drag.is_some()).ghost(ghost, shape(g.kind).radius(64.0)).into();
    let hint = if g.name.is_none() {
        format!("Open to name this {} (F2). Drag it onto another group to merge them.", g.kind.one())
    } else {
        format!("{}: {}. Drag onto another group to merge them.", g.name.clone().unwrap_or_default(), util::plural(g.photos as u64, "photo"))
    };
    tip(el, hint, true)
}

fn tiles<'a>(app: &'a App, groups: &[&'a Group], small: bool) -> Element<'a, Message> {
    let mut r = row![].spacing(8);
    for g in groups {
        r = r.push(group_tile(app, g, small));
    }
    container(r.wrap().vertical_spacing(8)).padding(Padding { left: 16.0, right: 16.0, top: 0.0, bottom: 10.0 }).into()
}

/// A disclosure: › Smaller groups 412.
fn disclose<'a>(label: &'a str, n: usize, open: bool, msg: Message) -> Element<'a, Message> {
    let content = row![
        icons::view(if open { Glyph::ChevronDown } else { Glyph::ChevronRight }, 14.0, t::PHOTO_INK2),
        text(label).size(t::T_BODY).font(paint::MEDIUM),
        text(util::thousands(n as u64)).size(t::T_SMALL).color(t::PHOTO_INK3)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    button(content)
        .padding(Padding::from([5.0, 8.0]))
        .style(|_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hot.then_some(Background::Color(t::PHOTO_RAISED)),
                text_color: if hot { t::PHOTO_INK } else { t::PHOTO_INK2 },
                border: iced::Border { radius: t::R_CONTROL.into(), ..iced::Border::default() },
                ..button::Style::default()
            }
        })
        .on_press(msg)
        .into()
}

pub(super) fn groups(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let a = arrange(&s.all, s.tab, app.settings.people_min_photos());
    let mut col = column![].padding(Padding { top: t::S1, bottom: t::S5, left: 0.0, right: 0.0 });
    if let Some(p) = progress_strip(app, s.tab) {
        col = col.push(p);
    }
    if s.tab == Kind::Fursuit {
        if let Some(pairs) = super::subjects_merge::tab_section(app) {
            col = col.push(pairs);
        }
    }
    if !a.named.is_empty() {
        col = col.push(heading("Named", util::thousands(a.named.len() as u64)));
        col = col.push(tiles(app, &a.named, false));
    }
    if !a.to_name.is_empty() {
        let note = format!("{}, largest first", util::thousands(a.to_name.len() as u64));
        col = col.push(heading("Groups to name", note));
        col = col.push(tiles(app, &a.to_name, false));
    }
    let mut folds = row![].spacing(6);
    let mut any = false;
    if !a.small.is_empty() {
        folds = folds.push(tip(
            disclose("Smaller groups", a.small.len(), s.small_open, m(Msg::Small(!s.small_open))),
            format!("Groups of fewer than {} photos, folded away so strangers stay out of sight. Preferences ▸ People sets the size.", app.settings.people_min_photos()),
            true,
        ));
        any = true;
    }
    if !a.hidden.is_empty() {
        folds = folds.push(disclose("Hidden", a.hidden.len(), s.hidden_open, m(Msg::Hidden(!s.hidden_open))));
        any = true;
    }
    if any {
        col = col.push(container(folds).padding(Padding { left: 10.0, right: 10.0, top: 6.0, bottom: 6.0 }));
    }
    if s.small_open && !a.small.is_empty() {
        col = col.push(tiles(app, &a.small, true));
    }
    if s.hidden_open && !a.hidden.is_empty() {
        col = col.push(container(text("Hidden: kept off this tab and out of the search field's suggestions. Their photos still come up when searched for by name.").size(t::T_SMALL).color(t::PHOTO_INK2)).padding(Padding { left: 16.0, right: 16.0, top: 8.0, bottom: 8.0 }));
        col = col.push(tiles(app, &a.hidden, true));
    }
    if a.named.is_empty() && a.to_name.is_empty() && !a.small.is_empty() && !s.small_open {
        col = col.push(container(text("Every group found so far is small. Open Smaller groups to see them.").size(t::T_SMALL).color(t::PHOTO_INK3)).padding(Padding::from([8.0, 16.0])));
    }
    container(scrollable(col).style(style::scroll).width(Length::Fill).height(Length::Fill)).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
}

/// Under the header while groups are picked: how many, and what to do.
fn pick_strip(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let n = s.picked.len();
    let merge = tip(kit::icon_text_btn(Glyph::Merge, "Merge…", t::INK, style::secondary(), (n >= 2).then(|| m(Msg::MergeAsk))), "Merge the picked groups into one  (M)", true);
    let hide = kit::icon_text_btn(Glyph::Hide, "Hide", t::INK2, style::ghost(), (n >= 1).then(|| m(Msg::HidePicked)));
    let close = tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(m(Msg::ClearPicks))), "Stop picking  (Esc)", true);
    let words = if n == 0 { "Click groups to pick them".to_string() } else { format!("{n} picked") };
    let r = row![icons::view(Glyph::Select, 16.0, t::SIGNAL), text(words).size(t::T_BODY).color(t::INK).width(Length::Fill), merge, hide, close]
        .spacing(t::S3)
        .align_y(Alignment::Center)
        .padding(Padding { top: 4.0, bottom: 4.0, left: 12.0, right: 8.0 });
    column![container(r).width(Length::Fill).style(style::fill(t::alpha(t::SIGNAL, 0.08))), container(Space::new().width(Length::Fill).height(1)).style(style::fill(t::alpha(t::SIGNAL, 0.3)))].into()
}

/// The crop size of the two-group merge dialog's cards.
const PAIR_CROP: f32 = 112.0;

/// Two groups to merge (a drop, or two picked): both side by side, large,
/// so the eye can check them; when both are named, a click on one keeps
/// its name.
fn merge_pair_dialog<'a>(app: &'a App, mg: &'a MergeAsk) -> Element<'a, Message> {
    let s = &app.subjects;
    let one = s.tab.one();
    let asks = mg.asks_name(&s.all);
    let card = |id: &'a GroupId| -> Element<'a, Message> {
        let Some(g) = s.group(id) else { return Space::new().into() };
        let kept = *id == mg.keep && g.name.is_some();
        let crop = tile::crop(s.crops.get(&g.cover.face, px(app, PAIR_CROP), Framing::Face), PAIR_CROP, shape(g.kind));
        let mut col = column![
            crop,
            text(g.name.clone().unwrap_or_else(|| "Unnamed group".into())).size(t::T_BODY).font(paint::MEDIUM).color(if g.name.is_some() { t::INK } else { t::INK2 }),
            text(format!("{} over {}", util::plural(g.photos as u64, "photo"), util::plural(g.days as u64, "day"))).size(t::T_SMALL).color(t::INK3),
        ]
        .spacing(6)
        .width(Length::Fill)
        .align_x(Alignment::Center);
        if asks {
            let line = if kept { "Its name stays" } else { "Keep this name instead" };
            col = col.push(text(line).size(t::T_SMALL).font(paint::MEDIUM).color(if kept { t::SIGNAL } else { t::INK3 }));
        }
        let b = button(col).width(196).padding(Padding { top: 14.0, bottom: 12.0, left: 10.0, right: 10.0 }).style(move |_t, status| {
            let hot = asks && matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if hot { t::RAISED } else { t::DEEP })),
                text_color: t::INK,
                border: style::border(if kept { t::SIGNAL } else { t::LINE }, if kept { 2.0 } else { 1.0 }, t::R_CELL),
                ..button::Style::default()
            }
        });
        if asks { b.on_press(m(Msg::MergeKeep(id.clone()))).into() } else { b.into() }
    };
    let (a, b) = (&mg.groups[0], &mg.groups[1]);
    let join = container(icons::view(Glyph::Merge, 20.0, t::INK3)).width(36).center_x(36);
    let cards = row![card(a), join, card(b)].align_y(Alignment::Center);
    let keep = s.group(&mg.keep).and_then(|g| g.name.clone());
    let says = match (&keep, asks) {
        (Some(_), true) => "Both have a name. Click the one to keep.".to_string(),
        (Some(n), false) => format!("It keeps the name {n}."),
        (None, _) => format!("Neither is named yet, so the merged {one} stays unnamed."),
    };
    let go = match &keep {
        Some(n) => format!("Merge into {n}"),
        None => "Merge".to_string(),
    };
    let content = column![
        super::dialogs::header(format!("Merge these two groups into one {one}?"), None),
        kit::secondary(format!("Their photos become one {one}, and suggestions come from all of them. {says}")),
        container(cards).width(Length::Fill).center_x(Length::Fill).padding(Padding::from([t::S2, 0.0])),
        kit::caption(format!("Ctrl+Z undoes it. A photo that doesn't belong can be moved out later, from {} on the {one}'s page.", s.tab.crops())),
        Space::new().height(t::S1),
        super::dialogs::footer(vec![], vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::MergeCancel))).into(), kit::btn(go, style::primary(), Some(m(Msg::MergeGo))).into()]),
    ]
    .spacing(t::S3);
    super::dialogs::backdrop(super::dialogs::frame_fit(content.into(), 520f32.min(app.window_size_width() - 48.0)))
}

fn merge_dialog<'a>(app: &'a App, mg: &'a MergeAsk) -> Element<'a, Message> {
    if mg.groups.len() == 2 {
        return merge_pair_dialog(app, mg);
    }
    let s = &app.subjects;
    let one = s.tab.one();
    let mut list = column![].spacing(2).padding(4);
    for id in &mg.groups {
        let Some(g) = s.group(id) else { continue };
        let on = *id == mg.keep;
        let dot = container(if on { Element::from(container(Space::new()).width(8).height(8).style(|_| container::Style { background: Some(Background::Color(t::SIGNAL)), border: iced::Border { radius: 4.0.into(), ..iced::Border::default() }, ..Default::default() })) } else { Space::new().into() })
            .width(16)
            .height(16)
            .center(16)
            .style(move |_| container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(if on { t::SIGNAL } else { t::LINE_HI }, 1.0, 8.0), ..Default::default() });
        let crop = tile::crop(s.crops.get(&g.cover.face, px(app, 40.0), Framing::Face), 40.0, shape(g.kind));
        let words = column![
            text(g.name.clone().unwrap_or_else(|| "Unnamed group".into())).size(t::T_BODY).font(paint::MEDIUM).color(if g.name.is_some() { t::INK } else { t::INK2 }),
            text(format!("{} over {}", util::plural(g.photos as u64, "photo"), util::plural(g.days as u64, "day"))).size(t::T_SMALL).color(t::INK3)
        ]
        .spacing(1);
        let r = row![dot, crop, words].spacing(12).align_y(Alignment::Center);
        list = list.push(
            button(r)
                .width(Length::Fill)
                .padding(Padding::from([8.0, 10.0]))
                .style(move |_t, status| {
                    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: (on || hot).then_some(Background::Color(t::RAISED)),
                        text_color: t::INK,
                        border: iced::Border { radius: t::R_CONTROL.into(), ..iced::Border::default() },
                        ..button::Style::default()
                    }
                })
                .on_press(m(Msg::MergeKeep(id.clone()))),
        );
    }
    let keep = s.group(&mg.keep).and_then(|g| g.name.clone());
    let go = match &keep {
        Some(n) => format!("Merge into {n}"),
        None => "Merge".to_string(),
    };
    let content = column![
        super::dialogs::header(format!("Merge {} groups into one {one}?", mg.groups.len()), None),
        kit::secondary(match (&keep, mg.asks_name(&s.all)) {
            (_, true) => format!("Their photos become one {one}, and suggestions come from all of them. Which name should it keep?"),
            (Some(n), false) => format!("Their photos become one {one}, and suggestions come from all of them. It keeps the name {n}."),
            (None, false) => format!("Their photos become one {one}, and suggestions come from all of them. None is named yet, so it stays unnamed."),
        }),
        container(list).width(Length::Fill).style(style::group()),
        kit::caption(format!("Ctrl+Z undoes it. A photo that doesn't belong can be moved out later, from {} on the {one}'s page.", s.tab.crops())),
        Space::new().height(t::S1),
        super::dialogs::footer(vec![], vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::MergeCancel))).into(), kit::btn(go, style::primary(), Some(m(Msg::MergeGo))).into()]),
    ]
    .spacing(t::S3);
    super::dialogs::backdrop(super::dialogs::frame_fit(content.into(), 500f32.min(app.window_size_width() - 48.0)))
}

fn forget_dialog<'a>(app: &'a App, id: &GroupId) -> Element<'a, Message> {
    let s = &app.subjects;
    let name = label(s, id);
    let photos = s.group(id).map_or(0, |g| g.photos);
    let content = column![
        super::dialogs::header(format!("Forget {name}?"), None),
        kit::secondary(format!(
            "This deletes the name {name} and which of the {} you confirmed are of them. Their faces go back to the groups to name. Your photos are not changed.",
            util::plural(photos as u64, "photo")
        )),
        Space::new().height(t::S2),
        super::dialogs::footer(vec![], vec![kit::btn("Cancel", style::secondary(), Some(m(Msg::ForgetCancel))).into(), kit::btn(format!("Forget {name}"), style::danger(), Some(m(Msg::ForgetGo))).into()]),
    ]
    .spacing(t::S3);
    super::dialogs::backdrop(super::dialogs::frame_fit(content.into(), 460f32.min(app.window_size_width() - 48.0)))
}

/// The page's catcher for menus: a click anywhere closes them.
pub fn catcher<'a>(msg: Message) -> Element<'a, Message> {
    opaque(iced::widget::mouse_area(container(Space::new()).width(Length::Fill).height(Length::Fill)).on_press(msg)).into()
}

/// Wait `ms`, then send `msg` (the review's beat).
pub fn after(ms: u64, msg: Message) -> Task<Message> {
    Task::perform(tokio::time::sleep(Duration::from_millis(ms)), move |_| msg)
}

/// A stack of layers, the first filling the page.
pub fn layered<'a>(layers: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    stack(layers).width(Length::Fill).height(Length::Fill).into()
}

// ── the test driver ─────────────────────────────────────────────────────────

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let s = &app.subjects;
    let a = arrange(&s.all, s.tab, app.settings.people_min_photos());
    let names = |v: &[&Group]| v.iter().map(|g| format!("{}:{}{}", people::short(&g.id), g.name.clone().unwrap_or_else(|| "-".into()), g.photos)).collect::<Vec<_>>();
    let kinds: HashSet<Kind> = a.named.iter().chain(&a.to_name).chain(&a.small).chain(&a.hidden).map(|g| g.kind).collect();
    format!(
        "view={:?} page={:?} tab={:?} state={:?} named={:?} to_name={:?} small={} hidden={:?} kinds_shown={kinds:?} picked={:?} select={} focus={:?} small_open={} hidden_open={} merge={:?} forget={:?} faces=[on={} model={:?} installed={} looked={} unrecognised={} found={} named={} pass={:?}] crops={} ({} KB) {} {}",
        app.view,
        s.page(),
        s.tab,
        // The Fursuits tab's states are its own.
        match s.tab {
            Kind::Fursuit => format!("{:?}", super::subjects_fursuits::state(app)),
            kind => format!("{:?}", state(app, kind)),
        },
        names(&a.named),
        names(&a.to_name).into_iter().take(8).collect::<Vec<_>>(),
        a.small.len(),
        names(&a.hidden),
        s.picked.iter().map(people::short).collect::<Vec<_>>(),
        s.select_mode,
        s.focus.as_ref().map(people::short),
        s.small_open,
        s.hidden_open,
        s.merge.as_ref().map(|mg| (mg.groups.iter().map(people::short).collect::<Vec<_>>(), people::short(&mg.keep))),
        s.forget.as_ref().map(people::short),
        app.search.status.faces.on,
        app.search.status.faces.model,
        app.search.status.faces.installed,
        app.search.status.faces.looked_at,
        app.search.status.faces.unrecognised,
        app.search.status.faces.found,
        app.search.status.faces.named,
        app.search.status.pass,
        s.crops.len(),
        s.crops.bytes() / 1024,
        person::debug_state(app),
        review::debug_state(app),
    ) + " " + &super::subjects_merge::debug_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::people::Face;
    use crate::search::index::Key;
    use chrono::NaiveDate;

    fn face(n: u8) -> Face {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        Face {
            face: FaceRef { key: Key([n; 16]), bbox: [0.4, 0.2, 0.1, 0.1] },
            jpg: None,
            raw: None,
            day,
            capture: day.and_hms_opt(10, n as u32 % 60, 0).unwrap(),
            burst: None,
            clarity: 0.5,
            confirmed: false,
        }
    }

    fn group(id: u64, kind: Kind, name: Option<&str>, photos: usize, hidden: bool) -> Group {
        Group { id: GroupId::Cluster(id), kind, name: name.map(str::to_string), hidden, photos, days: 1, cover: face(id as u8), suggestions: 0 }
    }

    fn found() -> Found {
        Found {
            generation: 1,
            groups: vec![
                group(1, Kind::Person, Some("Person A"), 3, false),
                group(2, Kind::Person, None, 58, false),
                group(3, Kind::Person, None, 4, false),
                group(4, Kind::Person, None, 12, true),
                group(5, Kind::Fursuit, Some("Fursuit 1"), 96, false),
                group(6, Kind::Pet, None, 40, false),
                group(7, Kind::Person, None, 9, false),
            ],
            faces_found: 200,
            undoable: None,
        }
    }

    #[test]
    fn a_tab_shows_one_kind_named_first_and_small_groups_folded_away() {
        let f = found();
        let a = arrange(&f, Kind::Person, 5);
        let ids = |v: &[&Group]| v.iter().map(|g| g.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&a.named), [GroupId::Cluster(1)], "a named person stays named however few photos");
        assert_eq!(ids(&a.to_name), [GroupId::Cluster(2), GroupId::Cluster(7)], "largest first");
        assert_eq!(ids(&a.small), [GroupId::Cluster(3)], "under the smallest size: folded away");
        assert_eq!(ids(&a.hidden), [GroupId::Cluster(4)]);
        for g in a.named.iter().chain(&a.to_name).chain(&a.small).chain(&a.hidden) {
            assert_eq!(g.kind, Kind::Person, "never another kind on the People tab");
        }
        // The size to show is a setting.
        assert_eq!(arrange(&f, Kind::Person, 10).to_name.len(), 1);
        assert_eq!(arrange(&f, Kind::Person, 3).small.len(), 0);
        // The arrow keys step through what shows.
        assert_eq!(a.order(false, false), [GroupId::Cluster(1), GroupId::Cluster(2), GroupId::Cluster(7)]);
        assert_eq!(a.order(true, true).len(), 5);
        // Other tabs, other kinds.
        assert_eq!(arrange(&f, Kind::Fursuit, 5).named.len(), 1);
        assert_eq!(arrange(&f, Kind::Pet, 5).to_name.len(), 1);
    }

    /// The merge dialog's name: the named group keeps its name whichever
    /// tile was dragged onto which; two named ones ask, the one dropped
    /// onto chosen to start; none named, the one dropped onto.
    #[test]
    fn a_merge_keeps_the_name_and_asks_only_between_two() {
        let mut f = found();
        f.groups.push(group(8, Kind::Fursuit, None, 12, false));
        f.groups.push(group(9, Kind::Fursuit, Some("Fursuit 2"), 30, false));
        f.groups.push(group(10, Kind::Fursuit, None, 7, false));
        let (named, other_named, a, b) = (GroupId::Cluster(5), GroupId::Cluster(9), GroupId::Cluster(8), GroupId::Cluster(10));
        // An unnamed group dropped onto a named one, and the other way.
        let ask = MergeAsk::new(&f, vec![named.clone(), a.clone()], Some(&named)).unwrap();
        assert_eq!((ask.keep.clone(), ask.asks_name(&f)), (named.clone(), false));
        let ask = MergeAsk::new(&f, vec![a.clone(), named.clone()], Some(&a)).unwrap();
        assert_eq!(ask.keep, named, "dropped onto the unnamed one, the name still stays");
        // Two named: asks; starts on the one dropped onto.
        let ask = MergeAsk::new(&f, vec![other_named.clone(), named.clone()], Some(&other_named)).unwrap();
        assert_eq!((ask.keep.clone(), ask.asks_name(&f)), (other_named, true));
        // None named: the one dropped onto.
        let ask = MergeAsk::new(&f, vec![b.clone(), a.clone()], Some(&b)).unwrap();
        assert_eq!((ask.keep.clone(), ask.asks_name(&f)), (b, false));
        // Picked (S, then M): the first named, else the first picked.
        assert_eq!(MergeAsk::new(&f, vec![a.clone(), named.clone(), GroupId::Cluster(10)], None).unwrap().keep, named);
        assert!(MergeAsk::new(&f, vec![a], None).is_none(), "one group is nothing to merge");
    }

    #[test]
    fn completions_offer_named_people_only() {
        let f = found();
        let named: Vec<&Group> = f.named(Kind::Person).filter(|g| !g.hidden).collect();
        assert_eq!(named.len(), 1);
        assert!(named.iter().all(|g| g.kind == Kind::Person && g.name.is_some()));
        assert!(people::by_name(&f, Kind::Person, "person a").is_some());
        assert!(people::by_name(&f, Kind::Person, "Fursuit 1").is_none(), "a fursuit's name is never a person's");
    }

    #[test]
    fn crops_are_kept_by_bytes_least_used_first_out() {
        let mut c = Crops::new(3 * 64 * 64 * 4);
        let all: Vec<Face> = (0..5).map(face).collect();
        let faces: Vec<FaceRef> = all.iter().map(|f| f.face).collect();
        let want: Vec<_> = all.iter().map(|f| (f.clone(), 64, Framing::Face)).collect();
        let missing = c.missing(want.clone());
        assert_eq!(missing.len(), 5);
        assert!(c.missing(want.clone()).is_empty(), "asked once while on their way");
        c.put(missing.iter().map(|(k, _)| (*k, Some((64, 64, vec![0u8; 64 * 64 * 4])))).collect());
        assert!(c.bytes() <= 3 * 64 * 64 * 4, "{}", c.bytes());
        assert_eq!(c.len(), 3);
        // The newest stay.
        assert!(c.get(&faces[4], 64, Framing::Face).is_some());
        assert!(c.get(&faces[0], 64, Framing::Face).is_none());
        // A photo that couldn't be read is remembered, not asked again.
        let gone = face(9);
        let k = c.missing([(gone.clone(), 64, Framing::Face)]);
        c.put(vec![(k[0].0, None)]);
        assert!(c.missing([(gone, 64, Framing::Face)]).is_empty());
        assert_eq!(bucket(100.0), 128);
        assert_eq!(bucket(10.0), 64);
        assert_eq!(bucket(5000.0), 640);
    }
}
