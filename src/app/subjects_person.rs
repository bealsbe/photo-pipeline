//! A person's page: a search for them.  Their chip sits in the field, so
//! words typed beside it rank their photos best first; with no words the
//! photos come clearest first (decision UI 12), in the results grid with
//! each photo's date under it and bursts stacked, read-only (no P or M).
//! *Open day ›* shows the selected photo's day in the calendar.
//!
//! A band under the header holds the person: their name (click or F2
//! renames; an unnamed group opens with its name field ready, and a name
//! that already exists offers to merge into it first), how many photos and
//! suggestions, *Photos | Faces*, the review (Y), and ⋯ for merging,
//! hiding and forgetting.  *Faces* shows every face, clearest first, to
//! pick wrong ones and move them to someone else or out ("Not Person A"),
//! or, on a named group, one to be its group photo (`subjects_cover`).

use std::sync::Arc;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, pin, row, scrollable, stack, text, text_input};
use iced::{Alignment, Background, Border, Element, Length, Padding, Point, Task};

use super::kit::{self, tip};
use super::people::{self, Face, FaceRef, Framing, Group, GroupId, Kind};
use super::results_pane::{Ask, Pane};
use super::search::Reply;
use super::search_results::Results;
use super::style;
use super::subjects::{self as subj, m as sm};
use super::{App, Message, View};
use crate::imaging::thumbs::OWNER_PERSON;
use crate::model::collection::RecordId;
use crate::model::decisions::{At, Via};
use crate::ui::grid::GridEvent;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::side_menu::RAIL_W;
use crate::ui::subject_tile::{self as tile, Tile};
use crate::ui::theme::{self as t, alpha};
use crate::util;

/// A person's page's queries are told apart from the search page's by
/// their numbers.
pub const SEQ_BASE: u64 = 1 << 40;
const PANE_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// The words field's and the name field's widget ids.
pub const FIELD: &str = "person-field";
pub const NAME_FIELD: &str = "person-name";
const FIELD_H: f32 = 36.0;
const BAND_H: f32 = 64.0;
/// Faces shown at first in *Faces*, then this many more at a time.
const FACES_PAGE: usize = 300;

fn pm(msg: Msg) -> Message {
    sm(subj::Msg::Person(msg))
}

/// *Photos | Faces*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Photos,
    Faces,
}

/// A menu open on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    /// ⋯ in the band.
    More,
    /// ⋯ ▸ Merge into…
    MergeInto,
}

pub struct Person {
    pub group: GroupId,
    pub kind: Kind,
    /// The words beside the chip.
    pub words: String,
    pub pane: Pane,
    /// The query in flight, and whether it only refreshes the one shown.
    pub pending: Option<(u64, bool)>,
    /// The photo count the shown search was run at (it runs again when
    /// answers change it).
    ran_photos: usize,
    pub show: Show,
    pub faces: Option<Arc<Vec<Face>>>,
    faces_seq: u64,
    pub faces_shown: usize,
    pub picked: Vec<FaceRef>,
    pub menu: Option<Menu>,
    /// The name field: naming an unnamed group, or renaming.
    pub naming: Option<String>,
    pub name_hi: usize,
    /// A name just given: the page follows its group to it.
    follow: Option<String>,
    /// A group just merged into: the page follows there.
    merged_into: Option<GroupId>,
    /// *Find more* asked (strict, fursuits): its request, and how many
    /// suggestions waited before, to say what it found.
    pub more: Option<(u64, usize)>,
    /// Splitting a fursuit group (stage 3): how *Heads* is ordered, the
    /// engine's check, the last head clicked (Shift+click picks a run).
    pub order: super::subjects_split::Order,
    pub check: Option<super::subjects_split::Check>,
    pub check_seq: u64,
    pub anchor: Option<FaceRef>,
}

impl Person {
    fn new(group: GroupId, kind: Kind) -> Person {
        Person {
            group,
            kind,
            words: String::new(),
            pane: Pane::new(OWNER_PERSON, PANE_CACHE_BYTES),
            pending: None,
            ran_photos: 0,
            show: Show::Photos,
            faces: None,
            faces_seq: 0,
            faces_shown: FACES_PAGE,
            picked: Vec::new(),
            menu: None,
            naming: None,
            name_hi: 0,
            follow: None,
            merged_into: None,
            more: None,
            order: super::subjects_split::Order::Clearest,
            check: None,
            check_seq: 0,
            anchor: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Words(String),
    Submit,
    /// The field's ×: the words go, the person stays.
    ClearWords,
    /// The chip's ×, or the back button: back to the tab.
    Back,
    OpenDay,
    Grid(GridEvent),
    Built(u64, Option<Arc<Results>>),
    Show(Show),
    FacePick(FaceRef),
    FacesClear,
    FacesMore,
    Menu(Option<Menu>),
    /// The group editor: the group alone, or (picked faces) with them
    /// picked, the groups beside to send them to.
    Edit,
    /// "Not Person A": the picked faces out, never suggested again.
    NotThis,
    Rename,
    NameInput(String),
    NameTake(usize),
    NameSubmit,
    Hide,
    MergeInto(GroupId),
    Forget,
    Review,
    /// *Find more* (fursuits: strict, a few at a time).
    FindMore,
}

// ── naming ──────────────────────────────────────────────────────────────────

/// What Enter does with a name typed: merge into someone of that name
/// (offered first: it is almost always what was meant), or give the name.
#[derive(Debug, Clone, PartialEq)]
pub enum NameOption {
    MergeInto { id: GroupId, name: String, photos: usize },
    Name(String),
}

/// The choices for `typed` as the name of `group` (of `kind`): people of
/// that kind whose name starts with it (any word), the exact one first,
/// then the name itself unless someone already has it.  Never another
/// kind's names.
pub fn name_options(all: &super::people::Subjects, kind: Kind, group: &GroupId, typed: &str) -> Vec<NameOption> {
    let typed = typed.split_whitespace().collect::<Vec<_>>().join(" ");
    if typed.is_empty() {
        return Vec::new();
    }
    let low = typed.to_lowercase();
    let mut merges: Vec<&Group> = all
        .named(kind)
        .filter(|g| &g.id != group)
        .filter(|g| g.name.as_deref().is_some_and(|n| {
            let n = n.to_lowercase();
            n.starts_with(&low) || n.contains(&format!(" {low}"))
        }))
        .collect();
    merges.sort_by_key(|g| (g.name.as_deref().map(str::to_lowercase) != Some(low.clone()), std::cmp::Reverse(g.photos)));
    let taken = merges.iter().any(|g| g.name.as_deref().is_some_and(|n| n.to_lowercase() == low));
    let mut out: Vec<NameOption> = merges.into_iter().take(4).map(|g| NameOption::MergeInto { id: g.id.clone(), name: g.name.clone().unwrap_or_default(), photos: g.photos }).collect();
    if !taken {
        out.push(NameOption::Name(typed));
    }
    out
}

// ── opening, searching ──────────────────────────────────────────────────────

/// Open `id`'s page; `rename` opens its name field (F2).  An unnamed group
/// always opens with it ready.
pub fn open(app: &mut App, id: &GroupId, rename: bool) -> Task<Message> {
    let Some(g) = app.subjects.group(id).cloned() else { return Task::none() };
    let mut p = Person::new(id.clone(), g.kind);
    p.pane.thumb_size = app.search.pane.thumb_size;
    p.order = super::subjects_split::default_order(g.name.is_some());
    if g.name.is_none() || rename {
        p.naming = Some(g.name.clone().unwrap_or_default());
    }
    let naming = p.naming.is_some();
    app.subjects.person = Some(p);
    app.subjects.review = None;
    app.subjects.merges = None;
    app.quicklook = None;
    super::subjects_split::ask(app);
    let mut tasks = vec![search(app, false), subj::refresh_crops(app)];
    if naming {
        tasks.push(focus_name());
    }
    Task::batch(tasks)
}

/// The page's group is being merged into `into` (kept): the page follows
/// it there once the groups change.
pub(super) fn follow(app: &mut App, into: &GroupId) {
    if let Some(p) = app.subjects.person.as_mut() {
        p.merged_into = Some(into.clone());
    }
}

fn focus_name() -> Task<Message> {
    iced::widget::operation::focus::<Message>(NAME_FIELD).chain(iced::widget::operation::select_all(NAME_FIELD))
}

fn unfocus() -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus::<()>()).discard()
}

/// Search the person's photos with the words in the field; `refresh`
/// keeps the place (their photos changed).
pub fn search(app: &mut App, refresh: bool) -> Task<Message> {
    let closeness = app.settings.closeness();
    let seq = SEQ_BASE + app.subjects.next_seq();
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    p.pending = Some((seq, refresh));
    let photos = app.subjects.all.group(&p.group).map_or(0, |g| g.photos);
    p.ran_photos = photos;
    let words = p.words.split_whitespace().collect::<Vec<_>>().join(" ");
    let group = p.group.clone();
    app.search.link.query_in(seq, &words, closeness, &[group]);
    Task::none()
}

/// A query's answer, if it is the person page's.
pub fn on_results(app: &mut App, seq: u64, reply: Reply) -> Task<Message> {
    let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
    if p.pending.map(|x| x.0) != Some(seq) {
        return Task::none();
    }
    let Some(index) = app.calendar.index.clone() else {
        return Task::done(pm(Msg::Built(seq, None)));
    };
    let who = vec![subj::label(&app.subjects, &p.group)];
    let words = p.words.clone();
    let work = move || {
        let mut r = super::search::build(seq, words, &reply, &index);
        r.who = who;
        Arc::new(r)
    };
    Task::perform(super::preferences::on_worker("person-results", work), move |r| pm(Msg::Built(seq, r)))
}

/// The person's photos changed (answers, a move, an undo), or the faces:
/// show them as they are now.
pub fn after_change(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
    let (group, faces) = (p.group.clone(), p.show == Show::Faces || p.faces.is_some());
    let mut tasks = vec![search(app, true)];
    if faces {
        tasks.push(ask_faces(app, group));
    }
    super::subjects_split::ask(app);
    Task::batch(tasks)
}

pub(super) fn ask_faces(app: &mut App, group: GroupId) -> Task<Message> {
    let seq = app.subjects.next_seq();
    if let Some(p) = app.subjects.person.as_mut() {
        p.faces_seq = seq;
    }
    app.search.link.faces(seq, &group);
    Task::none()
}

pub fn on_faces(app: &mut App, seq: u64, group: GroupId, faces: Arc<Vec<Face>>) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if p.faces_seq != seq || p.group != group {
        return Task::none();
    }
    // Picks of faces no longer here go.
    p.picked.retain(|f| faces.iter().any(|x| x.face == *f));
    p.faces = Some(faces);
    subj::refresh_crops(app)
}

/// Where an unnamed group went when its id changed: the unnamed group of
/// the kind whose cover (the head its id follows) is one the page knew
/// was in it; of several (a split), the largest.  Never another kind's,
/// never a named one.
pub fn follow_cluster(all: &people::Subjects, kind: Kind, known: &std::collections::HashSet<FaceRef>) -> Option<GroupId> {
    all.groups.iter().filter(|g| g.kind == kind && g.name.is_none() && matches!(g.id, GroupId::Cluster(_)) && known.contains(&g.cover.face)).max_by_key(|g| g.photos).map(|g| g.id.clone())
}

/// The groups changed: follow this page's group (named, merged, or an
/// unnamed one whose cover changed), or go back to the tab if it went;
/// show its photos again when they changed.
pub fn on_subjects(app: &mut App) -> Task<Message> {
    let s = &mut app.subjects;
    let Some(p) = s.person.as_mut() else { return Task::none() };
    if s.all.group(&p.group).is_none() {
        // An unnamed group's id follows its clearest head (its cover): a
        // head taken out or removed (X in its check, Not a fursuit head)
        // that was the cover gives it a new id.  The page follows it to
        // the group whose cover is one of the heads it knew were there.
        let known = || {
            let mut k: std::collections::HashSet<FaceRef> = p.faces.iter().flat_map(|f| f.iter().map(|f| f.face)).collect();
            if let Some(r) = s.review.as_ref().filter(|r| r.group == p.group) {
                k.extend(r.list.iter().flat_map(|l| l.iter().flatten().map(|f| f.face)));
                k.extend(r.refs.iter().map(|f| f.face));
            }
            k
        };
        let next = p
            .follow
            .as_deref()
            .and_then(|n| people::by_name(&s.all, p.kind, n))
            .map(|g| g.id.clone())
            .or(p.merged_into.clone().filter(|id| s.all.group(id).is_some()))
            .or_else(|| matches!(p.group, GroupId::Cluster(_)).then(|| follow_cluster(&s.all, p.kind, &known())).flatten());
        match next {
            Some(id) => {
                p.group = id.clone();
                p.follow = None;
                p.merged_into = None;
                if let Some(r) = s.review.as_mut() {
                    r.group = id;
                }
            }
            None => {
                // Heads marked not heads in its review stay removed: say
                // so, and how to bring them back.
                let x = s.review.as_ref().map_or(0, |r| r.not_heads.len());
                let t = subj::to_overview(app);
                let words = if x > 0 {
                    "That group changed while it was open: here are the groups as they are now. What you marked not a head stays removed; Ctrl+Z brings it back."
                } else {
                    "That group changed while it was open: here are the groups as they are now."
                };
                let s = if x > 0 { app.show_status_action(words, "Undo", sm(subj::Msg::Undo)) } else { app.show_status(words, 6000) };
                return Task::batch([t, s]);
            }
        }
        return after_change(app);
    }
    let photos = s.all.group(&p.group).map_or(0, |g| g.photos);
    if photos != p.ran_photos && p.pending.is_none() {
        return after_change(app);
    }
    Task::none()
}

fn on_built(app: &mut App, seq: u64, built: Option<Arc<Results>>) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    let Some((want, refresh)) = p.pending else { return Task::none() };
    if want != seq {
        return Task::none();
    }
    p.pending = None;
    let Some(r) = built else { return Task::none() };
    p.pane.set_results(r, refresh, &app.workers);
    super::quicklook::follow(app)
}

/// The viewer on one of the person's photos, read-only, stepping through
/// them in the page's order.
pub fn open_viewer(app: &mut App, id: RecordId) -> Task<Message> {
    let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
    let Some(r) = p.pane.results.clone() else { return Task::none() };
    let (items, index) = p.pane.viewer_items(id);
    super::viewer::open(app, super::viewer::Source::Library(r.coll.clone()), items, index, true)
}

/// Open day ›: the selected photo's day in the calendar, back to here.
fn open_day(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
    let Some((id, r)) = p.pane.focus().zip(p.pane.results.clone()) else { return Task::none() };
    let (Some(day), Some(path)) = (r.day_of(id), r.coll.get(id).map(|x| x.path.clone())) else { return Task::none() };
    let name = subj::label(&app.subjects, &p.group);
    let label = if app.subjects.group(&p.group).is_some_and(|g| g.name.is_some()) { name } else { "Unnamed".to_string() };
    let t = app.switch_view(View::Calendar);
    app.calendar.open_from(day, path, View::Subjects, label);
    let status = app.calendar.status_text();
    Task::batch([t, app.show_status(status, 0)])
}

/// The crops the page shows.
pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(p) = s.person.as_ref() else { return Vec::new() };
    let mut v = Vec::new();
    if let Some(g) = s.group(&p.group) {
        v.push((g.cover.clone(), subj::px(app, 44.0), Framing::Face));
    }
    if p.show == Show::Faces {
        if let Some(faces) = &p.faces {
            let small = subj::px(app, tile::SMALL_CROP);
            // Those on screen or a screen away, as the page last drew them.
            v.extend(faces.iter().take(p.faces_shown).filter(|f| s.crops.is_seen(subj::Section::Faces, &subj::Crops::key(&f.face, small, Framing::Face))).map(|f| (f.clone(), small, Framing::Face)));
        }
    }
    if matches!(p.menu, Some(Menu::MergeInto)) || p.naming.is_some() {
        for g in s.all.groups.iter().filter(|g| g.kind == p.kind && g.id != p.group).take(40) {
            v.push((g.cover.clone(), subj::px(app, 22.0), Framing::Face));
        }
    }
    v
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Words(w) => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.words = w;
            }
            Task::none()
        }
        Msg::Submit => {
            let t = search(app, false);
            Task::batch([t, unfocus()])
        }
        Msg::ClearWords => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.words.clear();
            }
            search(app, false)
        }
        Msg::Back => subj::to_overview(app),
        Msg::OpenDay => open_day(app),
        Msg::Grid(ev) => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            p.menu = None;
            match p.pane.on_grid(ev, &app.workers) {
                Ask::Nothing | Ask::Fursuits(_) => Task::none(),
                Ask::Viewer(id) => open_viewer(app, id),
                Ask::Burst(id) => toggle_burst(app, id),
            }
        }
        Msg::Built(seq, r) => on_built(app, seq, r),
        Msg::Show(show) => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            if p.show == show {
                return Task::none();
            }
            p.show = show;
            p.picked.clear();
            p.menu = None;
            app.quicklook = None;
            let group = p.group.clone();
            if show == Show::Faces {
                let t = ask_faces(app, group);
                super::subjects_split::ask(app);
                return Task::batch([t, subj::refresh_crops(app)]);
            }
            Task::none()
        }
        Msg::FacePick(f) => {
            // Shift+click picks a run, in the order shown.
            super::subjects_split::pick(app, f, crate::ui::modifiers::get().shift());
            Task::none()
        }
        Msg::FacesClear => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.picked.clear();
                p.menu = None;
            }
            Task::none()
        }
        Msg::FacesMore => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.faces_shown += FACES_PAGE;
            }
            subj::refresh_crops(app)
        }
        Msg::Menu(menu) => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.menu = menu;
            }
            subj::refresh_crops(app)
        }
        Msg::Edit => edit_group(app),
        Msg::NotThis => not_this(app),
        Msg::Rename => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let name = app.subjects.all.group(&p.group).and_then(|g| g.name.clone()).unwrap_or_default();
            p.naming = Some(name);
            p.name_hi = 0;
            p.menu = None;
            Task::batch([focus_name(), subj::refresh_crops(app)])
        }
        Msg::NameInput(text) => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.naming = Some(text);
                p.name_hi = 0;
            }
            subj::refresh_crops(app)
        }
        Msg::NameTake(i) => take_name(app, i),
        Msg::NameSubmit => {
            let i = app.subjects.person.as_ref().map_or(0, |p| p.name_hi);
            take_name(app, i)
        }
        Msg::Hide => {
            let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
            let (id, named) = (p.group.clone(), app.subjects.group(&p.group).is_some_and(|g| g.name.is_some()));
            let name = subj::label(&app.subjects, &id);
            app.search.link.set_hidden(&id, true);
            let t = subj::to_overview(app);
            let what = if named { format!("{name} is hidden. Their photos still come up when you search for them by name.") } else { "The group is hidden. Find it under Hidden.".to_string() };
            Task::batch([t, app.show_status_action(what, "Undo", sm(subj::Msg::Undo))])
        }
        Msg::MergeInto(target) => {
            let target_kind = app.subjects.group(&target).map(|g| g.kind);
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            p.menu = None;
            let from = p.group.clone();
            if target_kind != Some(p.kind) {
                return app.show_status("Only one kind can be merged.", 5000);
            }
            p.merged_into = Some(target.clone());
            let (a, b) = (subj::label(&app.subjects, &from), subj::label(&app.subjects, &target));
            app.search.link.merge(&target, &[from], Via::Hand(At::Menu));
            let a = if a == "the unnamed group" { "The group".to_string() } else { a };
            app.show_status_action(format!("{a} was merged into {b}."), "Undo", sm(subj::Msg::Undo))
        }
        Msg::Forget => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            p.menu = None;
            app.subjects.forget = Some(p.group.clone());
            Task::none()
        }
        Msg::Review => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            p.menu = None;
            p.naming = None;
            let group = p.group.clone();
            super::subjects_review::start(app, &group)
        }
        Msg::FindMore => {
            let seq = app.subjects.next_seq();
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let group = p.group.clone();
            let before = app.subjects.all.group(&group).map_or(0, |g| g.suggestions);
            if let Some(p) = app.subjects.person.as_mut() {
                p.more = Some((seq, before));
            }
            app.search.link.find_more(seq, &group);
            app.show_status("Looking again, from everything confirmed so far.", 4000)
        }
    }
}

/// What *Find more* found (the suggestions it answers with).
pub fn on_more(app: &mut App, seq: u64, group: &GroupId, list: &[Vec<Face>]) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    let Some((_, before)) = p.more.filter(|(s, _)| *s == seq && &p.group == group) else { return Task::none() };
    p.more = None;
    let name = subj::label(&app.subjects, group);
    let judging = super::fursuit_judge::judging(app, group);
    let text = match list.len().saturating_sub(before) {
        0 if judging => format!("Nothing more is sure to show {name}. Checking the close calls; those the judge offers join the suggestions."),
        0 => format!("Nothing more looks close enough to {name} yet. New exports bring new suggestions."),
        1 => format!("Found 1 more photo that may show {name}. Review it with Y."),
        n => format!("Found {n} more photos that may show {name}. Review them with Y."),
    };
    app.show_status(text, 6000)
}

fn toggle_burst(app: &mut App, id: RecordId) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if let Err(name) = p.pane.toggle_burst(id) {
        return app.show_status(format!("{name} isn't part of a burst."), 4000);
    }
    super::quicklook::follow(app)
}

/// G on the page.
pub fn toggle_focused(app: &mut App) -> Task<Message> {
    match app.subjects.pane().and_then(Pane::focus) {
        Some(id) => toggle_burst(app, id),
        None => app.show_status("Select a photo to stack its burst.", 4000),
    }
}

/// Shift+G on the page.
pub fn toggle_all(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.pane_mut() else { return Task::none() };
    let text = p.toggle_all();
    let t = app.show_status(text, 4000);
    Task::batch([t, super::quicklook::follow(app)])
}

fn take_name(app: &mut App, i: usize) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    let typed = p.naming.clone().unwrap_or_default();
    let options = name_options(&app.subjects.all, p.kind, &p.group, &typed);
    let Some(choice) = options.get(i).or(options.first()).cloned() else {
        // Nothing typed: an unnamed group keeps its field; a rename stops.
        if app.subjects.all.group(&p.group).is_some_and(|g| g.name.is_some()) {
            p.naming = None;
        }
        return Task::none();
    };
    let current = app.subjects.all.group(&p.group).and_then(|g| g.name.clone());
    p.naming = None;
    let id = p.group.clone();
    match choice {
        NameOption::Name(n) if current.as_deref() == Some(n.as_str()) => Task::none(),
        NameOption::Name(n) => {
            p.follow = Some(n.clone());
            app.search.link.name_group(&id, &n);
            let what = match current {
                Some(old) => format!("Renamed {old} to {n}."),
                None => format!("Named this group {n}. Their photos come up when you search for @{n}."),
            };
            Task::batch([unfocus(), app.show_status_action(what, "Undo", sm(subj::Msg::Undo))])
        }
        NameOption::MergeInto { id: target, name, .. } => {
            p.merged_into = Some(target.clone());
            app.search.link.merge(&target, &[id], Via::Hand(At::Name));
            Task::batch([unfocus(), app.show_status_action(format!("Merged this group into {name}."), "Undo", sm(subj::Msg::Undo))])
        }
    }
}

/// The group editor on the page's group (the picked faces picked).
fn edit_group(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    p.menu = None;
    let picked = std::mem::take(&mut p.picked);
    p.anchor = None;
    let group = p.group.clone();
    let preset = if picked.is_empty() { super::subjects_editor::Preset::Plain } else { super::subjects_editor::Preset::Pick(picked) };
    super::subjects_editor::open(app, group, preset)
}

fn not_this(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    p.menu = None;
    if p.picked.is_empty() {
        return Task::none();
    }
    let faces = std::mem::take(&mut p.picked);
    let group = p.group.clone();
    let n = faces.len() as u64;
    let at = if p.kind == Kind::Fursuit { At::Heads } else { At::Faces };
    app.search.link.answer(&group, &faces, Some(false), Via::Hand(at));
    let name = subj::label(&app.subjects, &group);
    app.show_status_action(format!("Took {} out of {name}. They won't be suggested again.", util::plural(n, "photo")), "Undo", sm(subj::Msg::Undo))
}

// ── keys ────────────────────────────────────────────────────────────────────

/// Esc: a menu, then the name field, then the picked faces, then back to
/// the tab.
pub fn escape(app: &mut App) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if p.menu.take().is_some() {
        return Task::none();
    }
    if p.naming.is_some() && app.subjects.all.group(&p.group).is_some_and(|g| g.name.is_some()) {
        p.naming = None;
        return unfocus();
    }
    if !p.picked.is_empty() {
        p.picked.clear();
        return Task::none();
    }
    subj::to_overview(app)
}

pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
    if mods.control() || mods.alt() {
        return Task::none();
    }
    // E: the group editor (with the picked faces picked).  A focused name
    // field takes its own keys, so an E here was not typed into it.
    if matches!(key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("e")) && !mods.shift() {
        return edit_group(app);
    }
    // The name's options: ↑ ↓ choose, Tab takes.
    if let Some(typed) = p.naming.clone() {
        let n = name_options(&app.subjects.all, p.kind, &p.group, &typed).len();
        match key {
            Key::Named(Named::ArrowDown) if n > 0 => {
                p.name_hi = (p.name_hi + 1) % n;
                return Task::none();
            }
            Key::Named(Named::ArrowUp) if n > 0 => {
                p.name_hi = (p.name_hi + n - 1) % n;
                return Task::none();
            }
            Key::Named(Named::Tab) if n > 0 => return take_name(app, app.subjects.person.as_ref().map_or(0, |p| p.name_hi)),
            _ => {}
        }
    }
    let photos = p.show == Show::Photos;
    match key {
        Key::Named(Named::ArrowLeft | Named::ArrowRight) if photos => {
            p.pane.step(*key == Key::Named(Named::ArrowRight));
            Task::none()
        }
        Key::Named(Named::Enter) if photos => match p.pane.focus() {
            Some(id) => open_viewer(app, id),
            None => Task::none(),
        },
        Key::Named(Named::F2) => update(app, Msg::Rename),
        Key::Character(c) if c.as_str().eq_ignore_ascii_case("y") && !mods.shift() => update(app, Msg::Review),
        _ => Task::none(),
    }
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App, keyboard_ok: bool) -> Element<'_, Message> {
    let s = &app.subjects;
    let Some(p) = s.person.as_ref() else { return Space::new().into() };
    // The group and its photos: no suggestion rows (a merge, a split),
    // which the group editor offers.
    let mut col = column![].width(Length::Fill).height(Length::Fill);
    for part in parts(p) {
        col = match part {
            Part::Header => col.push(header(app, p)).push(kit::hairline()),
            Part::Band => col.push(band(app, p)).push(kit::hairline()),
            Part::PickStrip => col.push(faces_strip(app, p)),
            Part::Body => col,
        };
    }
    let body: Element<'_, Message> = match p.show {
        Show::Faces => faces_view(app, p),
        Show::Photos => match &p.pane.results {
            None => container(Space::new()).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into(),
            Some(r) if r.found.is_empty() => {
                let (title, line) = super::search_results::nothing_found(r, None);
                kit::quiet(title, Some(line))
            }
            Some(_) => {
                let keyboard = keyboard_ok && app.quicklook.is_none() && p.menu.is_none() && p.naming.is_none();
                p.pane.view(app.scale_factor, keyboard, true, &[], |e| pm(Msg::Grid(e)))
            }
        },
    };
    col = col.push(body);
    let mut layers: Vec<Element<'_, Message>> = vec![col.into()];
    if let Some(list) = name_list(app, p) {
        layers.push(list);
    }
    if let Some(menu) = menu_layer(app, p) {
        layers.push(subj::catcher(pm(Msg::Menu(None))));
        layers.push(menu);
    }
    subj::layered(layers)
}

/// What a group's page is made of, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Header,
    /// Who it is: name, counts, *Photos | Faces*, the review, *Edit group*.
    Band,
    /// The picked faces' actions (on *Faces*).
    PickStrip,
    /// The photos, or the faces.
    Body,
}

/// The page's parts.  Never a suggestion row (similar groups to merge, a
/// split): the page shows the group, and those are offered in the group
/// editor (*Edit group*).
pub fn parts(p: &Person) -> Vec<Part> {
    let mut v = vec![Part::Header, Part::Band];
    if p.show == Show::Faces && !p.picked.is_empty() {
        v.push(Part::PickStrip);
    }
    v.push(Part::Body);
    v
}

/// Dialogs the person's page puts over everything (none of its own:
/// forgetting asks through the page's dialog; a split is the editor's).
pub fn overlay(_app: &App) -> Option<Element<'_, Message>> {
    None
}

/// The chip in the field: the person's crop and name, × to leave.
fn chip<'a>(app: &'a App, g: Option<&'a Group>, kind: Kind) -> (Element<'a, Message>, f32) {
    let name = g.and_then(|g| g.name.clone()).unwrap_or_else(|| "Unnamed".into());
    let label = format!("@{name}");
    let avatar = tile::crop(g.and_then(|g| app.subjects.crops.get(&g.cover.face, subj::px(app, 44.0), Framing::Face)), 18.0, subj::shape(kind));
    let x = button(container(icons::view(Glyph::Close, 11.0, t::INK3)).center(Length::Fill)).width(18).height(18).padding(0).style(style::ghost().radius(9.0).style()).on_press(pm(Msg::Back));
    let w = 3.0 + 18.0 + 6.0 + paint::text_width(&label, t::T_BODY, paint::MEDIUM).ceil() + 4.0 + 18.0 + 3.0;
    let c = container(row![avatar, text(label).size(t::T_BODY).font(paint::MEDIUM).color(t::INK), tip(x, "Back to the groups", true)].spacing(6).align_y(Alignment::Center))
        .padding(Padding { left: 3.0, right: 3.0, top: 0.0, bottom: 0.0 })
        .height(24)
        .center_y(24)
        .style(|_| container::Style { background: Some(Background::Color(t::RAISED)), border: style::border(t::LINE_HI, 1.0, 12.0), ..Default::default() });
    (c.into(), w)
}

fn field_width(app: &App) -> f32 {
    (app.window_size_width() - RAIL_W - 420.0).clamp(300.0, 560.0)
}

fn header<'a>(app: &'a App, p: &'a Person) -> Element<'a, Message> {
    let s = &app.subjects;
    let g = s.group(&p.group);
    let back = tip(kit::icon_text_btn(Glyph::ChevronLeft, p.kind.plural(), t::INK2, style::secondary(), Some(pm(Msg::Back))), format!("Back to the {}  (Esc)", p.kind.plural().to_lowercase()), true);
    // The field: the magnifier, the chip, then the words.
    let w = field_width(app);
    let (chip, chip_w) = chip(app, g, p.kind);
    let input = text_input("Add words to narrow it", &p.words)
        .id(FIELD)
        .on_input(|v| pm(Msg::Words(v)))
        .on_submit(pm(Msg::Submit))
        .size(t::T_TITLE)
        .font(paint::MEDIUM)
        .padding(Padding { top: 7.0, bottom: 7.0, left: 36.0 + chip_w + 6.0, right: 36.0 })
        .width(w)
        .style(super::search::field_style);
    let clear: Element<'a, Message> = if p.words.is_empty() {
        Space::new().into()
    } else {
        tip(
            button(container(icons::view(Glyph::Close, 14.0, t::INK3)).center(Length::Fill)).width(24).height(24).padding(0).style(style::ghost().radius(4.0).style()).on_press(pm(Msg::ClearWords)),
            "Clear the words",
            true,
        )
    };
    let over = row![icons::view(Glyph::Search, 18.0, t::INK3), Space::new().width(8), chip, Space::new().width(Length::Fill), clear]
        .align_y(Alignment::Center)
        .padding(Padding { left: 11.0, right: 6.0, top: 0.0, bottom: 0.0 })
        .width(w)
        .height(FIELD_H);
    let field = stack![container(input).height(FIELD_H).center_y(FIELD_H), over];
    let caption = match (&p.pane.results, p.pending) {
        (_, Some((_, false))) => "Searching…".to_string(),
        (Some(r), _) if p.show == Show::Photos => super::search_results::count_caption(&r.understood, &r.who, r.found.len(), false),
        _ => String::new(),
    };
    let mut r = row![back, Space::new().width(t::S2), field, container(kit::caption(caption)).padding(Padding { left: t::S3, ..Padding::ZERO }), Space::new().width(Length::Fill)].align_y(Alignment::Center);
    if p.show == Show::Photos {
        let day = p.pane.focused_day();
        let tone = if day.is_some() { t::INK } else { t::INK4 };
        let label = row![text("Open day").size(t::T_BODY).font(paint::MEDIUM), icons::view(Glyph::ChevronRight, 15.0, tone)].spacing(t::S1).align_y(Alignment::Center);
        let b = button(container(label).center_y(Length::Fill))
            .height(t::CONTROL_H)
            .padding(Padding { left: t::S4, right: t::S2, top: 0.0, bottom: 0.0 })
            .style(style::secondary().style())
            .on_press_maybe(day.map(|_| pm(Msg::OpenDay)));
        let hint = match day {
            Some(d) => format!("Show {} in the calendar, with this photo selected", super::search::long_date(d)),
            None => "Select a photo to open its day".into(),
        };
        r = r.push(tip(b, hint, true));
    }
    container(r.padding(Padding::from([0.0, t::S3]))).height(subj::HEADER_H).center_y(subj::HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// The band under the header: who this is, and what can be done.
fn band<'a>(app: &'a App, p: &'a Person) -> Element<'a, Message> {
    let s = &app.subjects;
    let Some(g) = s.group(&p.group) else { return Space::new().into() };
    let one = p.kind.noun();
    let crop = tile::crop(s.crops.get(&g.cover.face, subj::px(app, 44.0), Framing::Face), 44.0, subj::shape(p.kind));
    let counts = format!("{} over {}.", util::plural(g.photos as u64, "photo"), util::plural(g.days as u64, "day"));
    let mut r = row![crop].spacing(14).align_y(Alignment::Center);
    let more = tip(
        kit::icon_btn(Glyph::More, t::INK2, style::ghost(), Some(pm(Msg::Menu(if p.menu == Some(Menu::More) { None } else { Some(Menu::More) })))),
        "Merge, hide or forget",
        true,
    );
    match &p.naming {
        Some(typed) => {
            let placeholder = format!("Name this {one}");
            let input = text_input(&placeholder, typed)
                .id(NAME_FIELD)
                .on_input(|v| pm(Msg::NameInput(v)))
                .on_submit(pm(Msg::NameSubmit))
                .size(14)
                .font(paint::MEDIUM)
                .padding(Padding::from([5.0, 9.0]))
                .width(380)
                .style(style::input());
            let about = if g.name.is_some() {
                format!("{counts} Enter renames; Esc keeps the name.")
            } else {
                format!("{counts} Grouped by look, so it may hold more than one {one}: check it before naming, or after.")
            };
            r = r.push(column![input, kit::note(about)].spacing(4).width(Length::Fill));
            if g.name.is_none() {
                let label = row![text("Check this group").size(t::T_BODY).font(paint::MEDIUM), quiet_keycap("Y")].spacing(8).align_y(Alignment::Center);
                let b = button(container(label).center_y(Length::Fill)).height(t::CONTROL_H).padding(Padding::from([0.0, t::S4])).style(style::secondary().style()).on_press(pm(Msg::Review));
                r = r.push(tip(b, format!("One face at a time: Y if it's the same {one}, N if not"), true));
            }
        }
        None => {
            let name = button(row![kit::title(g.name.clone().unwrap_or_default()), icons::view(Glyph::Pencil, 14.0, t::INK3)].spacing(8).align_y(Alignment::Center))
                .padding(0)
                .style(|_t, _s| button::Style::default())
                .on_press(pm(Msg::Rename));
            let more_line = match g.suggestions {
                0 => String::new(),
                1 => format!(" 1 more photo may show this {one}."),
                n => format!(" {} more photos may show this {one}.", util::thousands(n as u64)),
            };
            let left_out = super::fursuit_judge::band_words(app, &p.group).unwrap_or_default();
            r = r.push(column![tip(name, "Rename  (F2)", true), kit::note(format!("{counts}{more_line}{left_out}"))].spacing(2).width(Length::Fill));
        }
    }
    let seg = kit::segmented(vec![
        kit::segment("Photos", p.show == Show::Photos, None, pm(Msg::Show(Show::Photos))),
        kit::segment(people::crops(p.kind), p.show == Show::Faces, None, pm(Msg::Show(Show::Faces))),
    ]);
    r = r.push(seg);
    // Fursuits are matched strictly: more come a few at a time, asked for.
    if g.name.is_some() && p.kind == Kind::Fursuit {
        r = r.push(tip(kit::btn("Find more", style::secondary(), Some(pm(Msg::FindMore))), "Look again from every photo confirmed. Strict, so a few at a time, each to check with Y or N", true));
    }
    if g.name.is_some() {
        let review: Element<'a, Message> = if g.suggestions > 0 {
            let label = row![text(format!("Review {}", util::plural(g.suggestions as u64, "suggestion"))).size(t::T_BODY).font(paint::MEDIUM), keycap("Y")].spacing(8).align_y(Alignment::Center);
            tip(button(container(label).center_y(Length::Fill)).height(t::CONTROL_H).padding(Padding::from([0.0, t::S4])).style(style::primary().style()).on_press(pm(Msg::Review)), format!("One at a time: Y if it's this {one}, N if not"), true)
        } else if super::fursuit_judge::judging(app, &p.group) {
            // Nothing sure now, but the judge may find some among the
            // close calls.
            let label = row![text("Check close calls").size(t::T_BODY).font(paint::MEDIUM), quiet_keycap("Y")].spacing(8).align_y(Alignment::Center);
            tip(button(container(label).center_y(Length::Fill)).height(t::CONTROL_H).padding(Padding::from([0.0, t::S4])).style(style::secondary().style()).on_press(pm(Msg::Review)), format!("The judge checks the photos that may show this {one}; you answer Y or N for those it offers"), true)
        } else {
            tip(kit::btn("No suggestions now", style::secondary(), None), "New exports bring new suggestions", true)
        };
        r = r.push(review);
    }
    r = r.push(tip(kit::icon_text_btn(Glyph::Pencil, "Edit group", t::INK, style::secondary(), Some(pm(Msg::Edit))), "Sort its faces into groups: split, move, take out  (E)", true));
    r = r.push(more);
    container(r.padding(Padding::from([10.0, 12.0]))).height(BAND_H).center_y(BAND_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// A key on a secondary button.
fn quiet_keycap<'a>(k: &'a str) -> Element<'a, Message> {
    container(text(k).size(11).font(paint::MEDIUM).color(t::INK2)).padding(Padding::from([0.0, 4.0])).style(|_| container::Style { border: style::border(t::LINE_HI, 1.0, 4.0), ..Default::default() }).into()
}

/// A key on a primary button: a quiet outline in the button's ink.
fn keycap<'a>(k: &'a str) -> Element<'a, Message> {
    container(text(k).size(11).font(paint::MEDIUM).color(t::ON_SIGNAL)).padding(Padding::from([0.0, 4.0])).style(|_| container::Style { border: style::border(alpha(t::ON_SIGNAL, 0.55), 1.0, 4.0), ..Default::default() }).into()
}

/// The name's options, under the name field.
fn name_list<'a>(app: &'a App, p: &'a Person) -> Option<Element<'a, Message>> {
    let typed = p.naming.as_deref()?;
    let options = name_options(&app.subjects.all, p.kind, &p.group, typed);
    if options.is_empty() {
        return None;
    }
    let one = p.kind.noun();
    let mut list = column![].spacing(2);
    for (i, o) in options.iter().enumerate() {
        let hot = i == p.name_hi;
        let (lead, title, detail, tag): (Element<'a, Message>, Element<'a, Message>, String, &str) = match o {
            NameOption::MergeInto { id, name, photos } => {
                let g = app.subjects.group(id);
                let avatar = tile::crop(g.and_then(|g| app.subjects.crops.get(&g.cover.face, subj::px(app, 22.0), Framing::Face)), 22.0, subj::shape(p.kind));
                (
                    avatar,
                    row![text("Merge into ").size(t::T_BODY).color(t::INK), text(name.clone()).size(t::T_BODY).font(paint::SEMIBOLD).color(t::INK)].into(),
                    format!("Already a {one}, {}", util::plural(*photos as u64, "photo")),
                    "Merge",
                )
            }
            NameOption::Name(n) => {
                let is_rename = app.subjects.group(&p.group).is_some_and(|g| g.name.is_some());
                (
                    icons::view(Glyph::Pencil, 15.0, t::INK3),
                    text(if is_rename { format!("Rename to “{n}”") } else { format!("Name it “{n}”") }).size(t::T_BODY).color(t::INK).into(),
                    if is_rename { format!("The same {one}, a new name") } else { format!("A new {one}") },
                    "New",
                )
            }
        };
        let line = row![
            container(lead).width(22).center_x(22),
            column![title, kit::caption(detail)].spacing(1).width(Length::Fill),
            kit::caption(tag)
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        list = list.push(
            button(line)
                .padding(Padding::from([5.0, 10.0]))
                .width(Length::Fill)
                .style(move |_t, status| {
                    let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style { background: (hot || hover).then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..button::Style::default() }
                })
                .on_press(pm(Msg::NameTake(i))),
        );
    }
    let panel = crate::ui::motion::Appear::new(container(list).width(380).padding(t::S1).style(style::floating())).rise(-4.0);
    let x = 12.0 + 44.0 + 14.0;
    let y = subj::HEADER_H + 1.0 + 10.0 + 30.0 + 4.0;
    Some(pin(iced::widget::opaque(panel)).position(Point::new(x, y)).width(Length::Fill).height(Length::Fill).into())
}

/// A floating menu of rows.
fn menu_panel<'a>(heading: Option<String>, rows: Vec<Element<'a, Message>>, w: f32) -> Element<'a, Message> {
    let mut col = column![].spacing(2);
    if let Some(h) = heading {
        col = col.push(container(kit::heading(h)).padding(Padding { top: t::S2, bottom: 3.0, left: 10.0, right: 10.0 }));
    }
    for r in rows {
        col = col.push(r);
    }
    iced::widget::opaque(crate::ui::motion::Appear::new(container(col).width(w).padding(t::S1).style(style::floating())).rise(-4.0)).into()
}

fn menu_row<'a>(lead: Element<'a, Message>, label: String, trail: String, on: Option<Message>) -> Element<'a, Message> {
    let enabled = on.is_some();
    let line = row![
        container(lead).width(22).center_x(22),
        text(label).size(t::T_BODY).color(if enabled { t::INK } else { t::INK4 }).width(Length::Fill),
        kit::caption(trail)
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    button(line)
        .padding(Padding::from([6.0, 10.0]))
        .width(Length::Fill)
        .style(|_t, status| {
            let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style { background: hover.then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..button::Style::default() }
        })
        .on_press_maybe(on)
        .into()
}

/// The other groups of this kind, as rows: the named, then the largest to
/// name.
fn other_groups<'a>(app: &'a App, p: &'a Person, on: impl Fn(GroupId) -> Message) -> Vec<Element<'a, Message>> {
    let s = &app.subjects;
    let a = subj::arrange(&s.all, p.kind, app.settings.people_min_photos());
    a.named
        .iter()
        .chain(a.to_name.iter().take(6))
        .filter(|g| g.id != p.group)
        .map(|g| {
            let avatar = tile::crop(s.crops.get(&g.cover.face, subj::px(app, 22.0), Framing::Face), 20.0, subj::shape(p.kind));
            menu_row(avatar, g.name.clone().unwrap_or_else(|| "Unnamed group".into()), util::thousands(g.photos as u64), Some(on(g.id.clone())))
        })
        .collect()
}

fn menu_layer<'a>(app: &'a App, p: &'a Person) -> Option<Element<'a, Message>> {
    let menu = p.menu?;
    let s = &app.subjects;
    let g = s.group(&p.group)?;
    let one = p.kind.noun();
    let page_w = app.window_size_width() - RAIL_W;
    let (panel, x, y, w) = match menu {
        Menu::More => {
            let mut rows = vec![
                menu_row(icons::view(Glyph::Merge, 15.0, t::INK2), "Merge into…".into(), String::new(), Some(pm(Msg::Menu(Some(Menu::MergeInto))))),
                menu_row(icons::view(Glyph::Pencil, 15.0, t::INK2), "Edit group…".into(), "E".into(), Some(pm(Msg::Edit))),
                menu_row(icons::view(Glyph::Hide, 15.0, t::INK2), "Hide".into(), String::new(), Some(pm(Msg::Hide))),
                // Named: asked first (the name goes).
                menu_row(
                    icons::view(Glyph::Dissolve, 15.0, t::INK2),
                    format!("{}{}", super::subjects_dissolve::ACTION, if g.name.is_some() { "…" } else { "" }),
                    String::new(),
                    Some(subj::m(subj::Msg::Dissolve(super::subjects_dissolve::Msg::Ask(super::subjects_dissolve::From::Page)))),
                ),
                menu_row(
                    icons::view(Glyph::Close, 15.0, t::REJECT),
                    format!("{}…", super::subjects_remove::action(p.kind)),
                    String::new(),
                    Some(subj::m(subj::Msg::Remove(super::subjects_remove::Msg::AskPage))),
                ),
            ];
            for (i, (glyph, words, on)) in super::subjects_cover::menu_rows(app, p).into_iter().enumerate() {
                rows.insert(2 + i, menu_row(icons::view(glyph, 15.0, t::INK2), words.into(), String::new(), Some(on)));
            }
            if g.name.is_some() {
                rows.push(menu_row(icons::view(Glyph::Close, 15.0, t::REJECT), format!("Forget this {one}…"), String::new(), Some(pm(Msg::Forget))));
            }
            (menu_panel(None, rows, 240.0), page_w - 240.0 - 12.0, subj::HEADER_H + BAND_H - 6.0, 240.0)
        }
        Menu::MergeInto => {
            let rows = other_groups(app, p, |id| pm(Msg::MergeInto(id)));
            let rows = if rows.is_empty() { vec![menu_row(Space::new().into(), format!("No other {} yet", p.kind.plural().to_lowercase()), String::new(), None)] } else { rows };
            (menu_panel(Some(format!("Merge {} into", g.name.clone().unwrap_or_else(|| "this group".into()))), rows, 280.0), page_w - 280.0 - 12.0, subj::HEADER_H + BAND_H - 6.0, 280.0)
        }
    };
    let _ = w;
    Some(pin(panel).position(Point::new(x.max(8.0), y)).width(Length::Fill).height(Length::Fill).into())
}

/// While faces are picked: how many, and where they go.
fn faces_strip<'a>(app: &'a App, p: &'a Person) -> Element<'a, Message> {
    let name = subj::label(&app.subjects, &p.group);
    let not = match (p.kind, name == "the unnamed group") {
        (Kind::Fursuit, _) => "Take out of group".to_string(),
        (_, true) => "Not this one".to_string(),
        (_, false) => format!("Not {name}"),
    };
    let mut r = row![
        icons::view(Glyph::Select, 16.0, t::SIGNAL),
        text(format!("{} picked", util::plural(p.picked.len() as u64, if p.kind == Kind::Fursuit { "head" } else { "face" }))).size(t::T_BODY).color(t::INK).width(Length::Fill),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center)
    .padding(Padding { top: 4.0, bottom: 4.0, left: 12.0, right: 8.0 });
    // One picked on a named group: it can be the group photo.
    if let Some(b) = super::subjects_cover::strip_action(app, p) {
        r = r.push(b);
    }
    // Split them off: the editor, with them in a new column to adjust.
    r = r.push(tip(kit::icon_text_btn(Glyph::Split, "Split into a new group", t::INK, style::secondary(), Some(super::subjects_split::m(super::subjects_split::Msg::SplitPicked))), "The group editor, these in a new column, to adjust before applying  (E for the editor as it is)", true));
    // Not heads at all (a mask, a plush, a sign): removed for good.
    let what = super::subjects_remove::nouns(p.kind);
    r = r.push(tip(
        kit::icon_text_btn(Glyph::Close, format!("{}…", super::subjects_remove::action(p.kind)), t::REJECT, style::ghost(), Some(subj::m(subj::Msg::Remove(super::subjects_remove::Msg::AskPicked)))),
        format!("These aren't {what} at all: remove them, so they are never grouped, suggested or found again (asks first)"),
        true,
    ));
    let r = r.push(row![
        tip(kit::icon_text_btn(Glyph::Split, "Move to…", t::INK, style::secondary(), Some(pm(Msg::Edit))), "The group editor, these picked: drop them on a group  (E)", true),
        tip(kit::icon_text_btn(Glyph::LeaveOut, not, t::REJECT, style::secondary(), Some(pm(Msg::NotThis))), "Take them out of this group, and never suggest them for it again (one undo step)", true),
        tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(pm(Msg::FacesClear))), "Stop picking  (Esc)", true),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center));
    column![container(r).width(Length::Fill).style(style::fill(alpha(t::SIGNAL, 0.08))), container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::SIGNAL, 0.3)))].into()
}

/// Every face, clearest first: the least sure come last, where a wrong one
/// is easy to spot and pick.
fn faces_view<'a>(app: &'a App, p: &'a Person) -> Element<'a, Message> {
    let s = &app.subjects;
    let Some(faces) = &p.faces else { return container(Space::new()).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into() };
    let fursuit = p.kind == Kind::Fursuit;
    let mut col = if fursuit {
        // Fursuits: least like the rest first, or clearest first.
        column![super::subjects_split::order_bar(p, faces.len())]
    } else {
        column![subj::heading(people::crops(p.kind), format!("{}, clearest first. Pick the ones that aren't them.", util::thousands(faces.len() as u64)))]
    };
    let small = subj::px(app, tile::SMALL_CROP);
    let mut r = row![].spacing(8);
    let mut keys = Vec::with_capacity(p.faces_shown.min(faces.len()));
    let shown = super::subjects_split::ordered(p);
    for f in shown.into_iter().take(p.faces_shown) {
        keys.push(subj::Crops::key(&f.face, small, Framing::Face));
        let crop = tile::crop(s.crops.get(&f.face, small, Framing::Face), tile::SMALL_CROP, subj::shape(p.kind));
        let badge = super::subjects_cover::badge(app, p, &f.face).or(if fursuit { super::subjects_split::badge(p, &f.face) } else { None });
        let el: Element<'a, Message> = Tile { crop, name: None, count: f.day.format("%b %-d, %Y").to_string(), picked: p.picked.contains(&f.face), small: true, badge: badge.map(|b| b.0), on: pm(Msg::FacePick(f.face)), badge_on: None }.into();
        let words = format!("{}{}{}", super::search::long_date(f.day), if f.confirmed { ", confirmed" } else { "" }, badge.map(|b| format!(". {}", b.1)).unwrap_or_default());
        r = r.push(tip(el, words, true));
    }
    let watched = crate::ui::in_view::InView::new(r.wrap().vertical_spacing(8), keys, |keys| subj::m(subj::Msg::InView(subj::Section::Faces, keys)));
    col = col.push(container(watched).padding(Padding { left: 16.0, right: 16.0, top: 0.0, bottom: 12.0 }));
    if faces.len() > p.faces_shown {
        let more = faces.len() - p.faces_shown;
        col = col.push(container(kit::btn(format!("Show {} more", util::thousands(more.min(FACES_PAGE) as u64)), style::secondary(), Some(pm(Msg::FacesMore)))).padding(Padding { left: 16.0, bottom: 24.0, ..Padding::ZERO }));
    }
    container(scrollable(col).style(style::scroll).width(Length::Fill).height(Length::Fill)).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
}

pub fn debug_state(app: &App) -> String {
    let split = app.subjects.person.as_ref().map(super::subjects_split::debug_state).unwrap_or_default();
    debug_state_inner(app) + " " + &split
}

fn debug_state_inner(app: &App) -> String {
    let Some(p) = app.subjects.person.as_ref() else { return "person=none".into() };
    let name = |id: RecordId| p.pane.results.as_ref().and_then(|r| r.coll.get(id)).map(|r| r.stem()).unwrap_or_else(|| "?".into());
    format!(
        "person=[group={} kind={:?} words={:?} pending={:?} photos={} tiles={} sections={:?} focus={:?} show={:?} faces={:?} picked={} menu={:?} naming={:?} hi={} options={:?}]",
        people::short(&p.group),
        p.kind,
        p.words,
        p.pending,
        p.pane.photos(),
        p.pane.layout.tiles.len(),
        p.pane.layout.sections.iter().map(|x| format!("{}:{}", x.label, x.ids.len())).collect::<Vec<_>>(),
        p.pane.focus().map(name),
        p.show,
        p.faces.as_ref().map(|f| f.len()),
        p.picked.len(),
        p.menu,
        p.naming,
        p.name_hi,
        p.naming.as_deref().map(|n| name_options(&app.subjects.all, p.kind, &p.group, n)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::people::{Face, FaceRef, Subjects};
    use crate::search::index::Key;
    use chrono::NaiveDate;

    fn g(id: u64, kind: Kind, name: Option<&str>, photos: usize) -> Group {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        let cover = Face { face: FaceRef { key: Key([id as u8; 16]), bbox: [0.0; 4] }, jpg: None, raw: None, day, capture: day.and_hms_opt(1, 0, 0).unwrap(), burst: None, clarity: 1.0, confirmed: true };
        Group { id: GroupId::Cluster(id), kind, name: name.map(str::to_string), hidden: false, photos, bursts: photos, days: 1, cover, suggestions: 0 }
    }

    fn all() -> Subjects {
        Subjects {
            generation: 1,
            groups: vec![g(1, Kind::Person, Some("Person A"), 312), g(2, Kind::Person, Some("Pat"), 20), g(3, Kind::Person, None, 58), g(4, Kind::Fursuit, Some("Patches"), 90)],
            faces_found: 400,
            undoable: None, seen_together: Default::default(),
            chosen_covers: Default::default(), loose: Default::default(),
        }
    }

    /// A group's page shows the group (its header, band and photos or
    /// faces), never a suggestion row: no *Similar groups* to merge on a
    /// fursuit's page, no split banner.  Those are the group editor's.
    #[test]
    fn a_group_page_shows_no_merge_or_split_suggestion() {
        for kind in [Kind::Fursuit, Kind::Person] {
            let mut p = Person::new(GroupId::Cluster(4), kind);
            assert_eq!(parts(&p), [Part::Header, Part::Band, Part::Body]);
            p.show = Show::Faces;
            assert_eq!(parts(&p), [Part::Header, Part::Band, Part::Body]);
            p.picked.push(FaceRef { key: Key([4; 16]), bbox: [0.0; 4] });
            assert_eq!(parts(&p), [Part::Header, Part::Band, Part::PickStrip, Part::Body], "the picked faces' actions only");
        }
    }

    /// An unnamed group whose cover was removed has a new id: the page
    /// follows it to the group whose cover it knew was there, the largest
    /// of several, never a named one or another kind's.
    #[test]
    fn an_unnamed_group_is_followed_by_the_heads_it_held() {
        let a = all();
        let key = |n: u8| FaceRef { key: Key([n; 16]), bbox: [0.0; 4] };
        let known: std::collections::HashSet<FaceRef> = [key(1), key(3), key(4), key(9)].into_iter().collect();
        assert_eq!(follow_cluster(&a, Kind::Person, &known), Some(GroupId::Cluster(3)), "the unnamed one, not Person A");
        assert!(follow_cluster(&a, Kind::Fursuit, &known).is_none(), "Patches is named");
        let mut b = a.clone();
        b.groups.push(g(9, Kind::Person, None, 80));
        assert_eq!(follow_cluster(&b, Kind::Person, &known), Some(GroupId::Cluster(9)), "split: the largest");
        assert!(follow_cluster(&a, Kind::Person, &Default::default()).is_none());
    }

    #[test]
    fn a_name_that_exists_offers_to_merge_first() {
        let a = all();
        let me = GroupId::Cluster(3);
        // "pat" starts two people's names: both offered as merges, the
        // exact one first, then the new name; never the fursuit.
        let o = name_options(&a, Kind::Person, &me, "pat");
        assert_eq!(o[0], NameOption::MergeInto { id: GroupId::Cluster(2), name: "Pat".into(), photos: 20 }, "the exact name first");
        assert!(!o.iter().any(|x| matches!(x, NameOption::MergeInto { id, .. } if *id == GroupId::Cluster(4))), "never another kind");
        assert!(!o.contains(&NameOption::Name("pat".into())), "the name is taken: no second Pat");
        let o = name_options(&a, Kind::Person, &me, "Per");
        assert_eq!(o.len(), 2);
        assert!(matches!(&o[0], NameOption::MergeInto { name, .. } if name == "Person A"));
        assert_eq!(o[1], NameOption::Name("Per".into()));
        // A word start counts too ("A" for "Person A"); a new name alone
        // when nothing matches; nothing for nothing typed.
        assert!(matches!(&name_options(&a, Kind::Person, &me, "a")[0], NameOption::MergeInto { name, .. } if name == "Person A"));
        assert_eq!(name_options(&a, Kind::Person, &me, "  Zed   Q "), [NameOption::Name("Zed Q".into())]);
        assert!(name_options(&a, Kind::Person, &me, "  ").is_empty());
        // Renaming someone never offers to merge them into themselves.
        assert_eq!(name_options(&a, Kind::Person, &GroupId::Cluster(1), "Person A"), [NameOption::Name("Person A".into())]);
    }
}
