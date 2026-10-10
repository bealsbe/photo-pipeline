//! "Same person?": the review, where naming feels like culling.  One
//! suggestion at a time, large, between what the person looks like (their
//! cover and confirmed faces) and the whole photo with the face outlined.
//! **Y** and **N** sit where P and M would be, in the keep and prune
//! colours; the answer is stamped on the crop the way marks are (the keep
//! check, the grease-pencil cross) and, with *Advance after marking* on
//! (shown here as *Auto-advance*), the next suggestion comes after the
//! same short beat as in the viewer.
//!
//! - **One answer covers a whole burst** (UI 6): a suggestion is a press,
//!   its frames answered together.
//! - **No is "not this person"**: the photo leaves them (when it was in)
//!   and is never suggested again.
//! - ← → (A D) step, Enter opens the photo in the viewer, Ctrl+Z takes the
//!   last answer back, Esc stops; the status bar says what changed.
//! - An unnamed group is checked the same way: its own faces, "Same person
//!   as this group?".

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, row, scrollable, stack, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{self, Face, FaceRef, Framing, GroupId, Kind, KindWords};
use super::style;
use super::subjects::{self as subj, m as sm};
use super::viewer_advance::BEAT_MS;
use super::{App, Message};
use crate::model::collection::Collection;
use crate::model::decisions::Via;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile::{self as tile, PhotoBox};
use crate::ui::theme::{self as t, alpha};
use crate::util;

fn rm(msg: Msg) -> Message {
    sm(subj::Msg::Review(msg))
}

const BIG: f32 = 300.0;
const PHOTO_W: f32 = 340.0;
const REF: f32 = 150.0;
const REF_SMALL: f32 = 44.0;
const STRIP: f32 = 54.0;
/// Suggestions shown in the strip at a time.
const STRIP_N: usize = 18;

pub struct Review {
    pub group: GroupId,
    pub kind: Kind,
    /// Checking an unnamed group's own faces.
    pub checking: bool,
    /// One entry per press, its frames best first.
    pub list: Option<Arc<Vec<Vec<Face>>>>,
    pub seq: u64,
    /// The person's confirmed faces, for the left column.
    pub refs: Vec<Face>,
    refs_seq: u64,
    pub at: usize,
    pub answers: Vec<Option<bool>>,
    /// Answers in the order given, for Ctrl+Z.
    order: Vec<usize>,
    beat: u64,
    /// The answer that started the last move, and when (the stamp).
    stamp: Option<(bool, Instant)>,
    /// The close-call judge (fursuits): entries it offered, and those it
    /// left out that the user asked to see, by their first frame.
    pub judged_faces: HashSet<FaceRef>,
    pub left_out_faces: HashSet<FaceRef>,
    /// *Show them* was pressed: what it leaves out joins the list too.
    pub left_out_shown: bool,
    /// The judge has nothing more to add to this list.
    pub judge_done: bool,
}

impl Review {
    fn new(group: GroupId, kind: Kind, checking: bool, seq: u64, refs_seq: u64) -> Review {
        Review {
            group,
            kind,
            checking,
            list: None,
            seq,
            refs: Vec::new(),
            refs_seq,
            at: 0,
            answers: Vec::new(),
            order: Vec::new(),
            beat: 0,
            stamp: None,
            judged_faces: HashSet::new(),
            left_out_faces: HashSet::new(),
            left_out_shown: false,
            judge_done: true,
        }
    }

    /// How an entry (its frames) came to be offered, for the decision
    /// record: by distance, by the judge, or left out by the judge and
    /// shown on request; an unnamed group's own heads are a check.
    pub fn via(&self, faces: &[FaceRef]) -> Via {
        let first = |set: &HashSet<FaceRef>| faces.iter().any(|f| set.contains(f));
        let judge = if first(&self.judged_faces) {
            Some(true)
        } else if first(&self.left_out_faces) {
            Some(false)
        } else {
            None
        };
        Via::Review { judge, check: self.checking }
    }

    /// Entries the judge sent join the end of the list (nothing on screen
    /// moves), each once; `left_out` marks those it judged different.
    pub fn take(&mut self, entries: &[Vec<Face>], left_out: bool) {
        for f in self.append(entries) {
            if left_out {
                self.left_out_faces.insert(f);
            } else {
                self.judged_faces.insert(f);
            }
        }
    }

    /// Entries not listed yet join the end, each once; their first heads.
    pub fn append(&mut self, entries: &[Vec<Face>]) -> Vec<FaceRef> {
        let have = super::fursuit_judge::firsts(self.list.as_deref().map_or(&[][..], |l| &l[..]));
        let new: Vec<Vec<Face>> = entries.iter().filter(|e| e.first().is_some_and(|f| !have.contains(&f.face))).cloned().collect();
        if new.is_empty() && self.list.is_some() {
            return Vec::new();
        }
        let firsts = new.iter().map(|e| e[0].face).collect();
        let mut list: Vec<Vec<Face>> = self.list.as_deref().cloned().unwrap_or_default();
        list.extend(new);
        self.answers.resize(list.len(), None);
        self.list = Some(Arc::new(list));
        firsts
    }

    /// Answered Yes (photos added) and No (taken out, not suggested again),
    /// counted by photo: a press counts all its frames.
    pub fn counts(&self) -> (usize, usize) {
        let Some(list) = &self.list else { return (0, 0) };
        let mut c = (0, 0);
        for (i, a) in self.answers.iter().enumerate() {
            let n = list.get(i).map_or(0, Vec::len);
            match a {
                Some(true) => c.0 += n,
                Some(false) => c.1 += n,
                None => {}
            }
        }
        c
    }

    pub fn len(&self) -> usize {
        self.list.as_ref().map_or(0, |l| l.len())
    }

    /// The first unanswered suggestion after `i`, or the end.
    pub fn next_open(&self, i: usize) -> usize {
        (i + 1..self.len()).find(|&j| self.answers[j].is_none()).unwrap_or(self.len())
    }

    /// Every suggestion answered: the end page.
    pub fn at_end(&self) -> bool {
        self.list.is_some() && self.at >= self.len()
    }

    /// Answer the suggestion shown.  Returns the faces it covers (every
    /// frame of its press) and the answer they had before.
    pub fn answer(&mut self, yes: bool) -> Option<(Vec<FaceRef>, Option<bool>)> {
        let faces: Vec<FaceRef> = self.list.as_ref()?.get(self.at)?.iter().map(|f| f.face).collect();
        let before = self.answers[self.at].replace(yes);
        self.order.retain(|&i| i != self.at);
        self.order.push(self.at);
        Some((faces, before))
    }

    /// Take the last answer back: its place and faces.
    pub fn take_back(&mut self) -> Option<(usize, Vec<FaceRef>)> {
        let i = self.order.pop()?;
        self.answers[i] = None;
        self.at = i;
        let faces = self.list.as_ref()?.get(i)?.iter().map(|f| f.face).collect();
        Some((i, faces))
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Answer(bool),
    Step(i32),
    At(usize),
    /// The beat after an answer is over: move on if still on it.
    Beat(u64),
    Advance,
    Open,
    FindMore,
    Done,
}

/// Start reviewing `group`: its suggestions (a named person) or its own
/// faces (an unnamed group).
pub fn start(app: &mut App, group: &GroupId) -> Task<Message> {
    let Some(g) = app.subjects.group(group).cloned() else { return Task::none() };
    let checking = g.name.is_none();
    // With a judge on, close calls may still be offered.
    if !checking && g.suggestions == 0 && !super::fursuit_judge::judging(app, group) {
        return app.show_status(format!("No photos wait to be checked against {} now.", g.name.unwrap_or_default()), 5000);
    }
    let (seq, refs_seq) = (app.subjects.next_seq(), app.subjects.next_seq());
    let mut r = Review::new(group.clone(), g.kind, checking, seq, refs_seq);
    r.judge_done = !super::fursuit_judge::judging(app, group);
    app.subjects.review = Some(r);
    app.quicklook = None;
    app.search.link.suggestions(seq, group);
    app.search.link.faces(refs_seq, group);
    Task::none()
}

pub fn on_suggestions(app: &mut App, seq: u64, group: GroupId, list: Arc<Vec<Vec<Face>>>) -> Task<Message> {
    let judging = super::fursuit_judge::keeps_review_open(app, &group);
    let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
    if r.seq != seq || r.group != group {
        return Task::none();
    }
    // With the judge on, a list already shown keeps its place: a new ask
    // (Find more, the next close calls) adds what is new at the end.
    if judging && !r.checking && r.list.as_ref().is_some_and(|l| !l.is_empty()) {
        r.judge_done = false;
        r.append(&list);
        return subj::refresh_crops(app);
    }
    // With the judge on, an empty first answer isn't the end: its close
    // calls come next (and its first verdicts say if there are none).
    if judging && !r.checking {
        r.judge_done = false;
        r.judged_faces.clear();
        r.left_out_faces.clear();
        r.answers = vec![None; list.len()];
        r.order.clear();
        r.at = 0;
        r.list = Some(list);
        return subj::refresh_crops(app);
    }
    if list.is_empty() {
        let first = r.list.is_none();
        let name = subj::label(&app.subjects, &group);
        if first {
            app.subjects.review = None;
            return app.show_status(format!("No photos wait to be checked against {name} now."), 5000);
        }
        return app.show_status(format!("Nothing more like {name} for now. New exports bring new suggestions."), 6000);
    }
    r.answers = vec![None; list.len()];
    r.order.clear();
    r.at = 0;
    r.list = Some(list);
    subj::refresh_crops(app)
}

/// The person's faces arrived (the left column's confirmed ones).
pub fn on_faces(app: &mut App, seq: u64, faces: &[Face]) -> bool {
    let Some(r) = app.subjects.review.as_mut() else { return false };
    if r.refs_seq != seq {
        return false;
    }
    r.refs = faces.iter().filter(|f| f.confirmed || r.checking).take(7).cloned().collect();
    true
}

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let s = &app.subjects;
    let Some(r) = s.review.as_ref() else { return Vec::new() };
    let mut v = Vec::new();
    if let Some(g) = s.group(&r.group) {
        v.push((g.cover.clone(), subj::px(app, REF), Framing::Face));
    }
    v.extend(r.refs.iter().skip(1).take(6).map(|f| (f.clone(), subj::px(app, REF_SMALL), Framing::Face)));
    if let Some(list) = &r.list {
        if let Some(cur) = list.get(r.at).and_then(|e| e.first()) {
            v.push((cur.clone(), subj::px(app, BIG), Framing::Face));
            v.push((cur.clone(), subj::px(app, PHOTO_W), Framing::Photo));
        }
        // The next one too, so it is there when the beat is over.
        if let Some(next) = list.get(r.at + 1).and_then(|e| e.first()) {
            v.push((next.clone(), subj::px(app, BIG), Framing::Face));
            v.push((next.clone(), subj::px(app, PHOTO_W), Framing::Photo));
        }
        let first = strip_start(r);
        v.extend(list.iter().skip(first).take(STRIP_N).filter_map(|e| e.first()).map(|f| (f.clone(), subj::px(app, STRIP), Framing::Face)));
    }
    v
}

fn strip_start(r: &Review) -> usize {
    r.at.saturating_sub(8).min(r.len().saturating_sub(STRIP_N))
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Answer(yes) => answer(app, yes),
        Msg::Step(d) => {
            let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
            let max = r.len().saturating_sub(1);
            r.at = (r.at as i64 + d as i64).clamp(0, max as i64) as usize;
            r.beat += 1;
            subj::refresh_crops(app)
        }
        Msg::At(i) => {
            let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
            r.at = i.min(r.len());
            r.beat += 1;
            subj::refresh_crops(app)
        }
        Msg::Beat(seq) => {
            let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
            if r.beat != seq || r.answers.get(r.at).copied().flatten().is_none() {
                return Task::none();
            }
            r.at = r.next_open(r.at);
            subj::refresh_crops(app)
        }
        Msg::Advance => {
            let on = !app.settings.viewer_advance;
            app.settings.viewer_advance = on;
            app.settings.save_in_background();
            app.show_status(if on { "Auto-advance is on: Y or N moves on to the next suggestion." } else { "Auto-advance is off: ← → move on." }, 4000)
        }
        Msg::Open => open_photo(app),
        Msg::FindMore => {
            let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
            let seq = app.subjects.seq + 1;
            app.subjects.seq = seq;
            let group = r.group.clone();
            if let Some(r) = app.subjects.review.as_mut() {
                r.seq = seq;
            }
            app.search.link.find_more(seq, &group);
            app.show_status("Looking again, from everything confirmed so far.", 4000)
        }
        Msg::Done => done(app),
    }
}

fn answer(app: &mut App, yes: bool) -> Task<Message> {
    let advance = app.settings.viewer_advance;
    let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
    if r.at_end() {
        return Task::none();
    }
    let group = r.group.clone();
    let Some((faces, before)) = r.answer(yes) else { return Task::none() };
    if before == Some(yes) {
        return Task::none();
    }
    let via = r.via(&faces);
    // A changed answer takes the old one back first.
    if before.is_some() {
        app.search.link.answer(&group, &faces, None, via);
    }
    app.search.link.answer(&group, &faces, Some(yes), via);
    let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
    r.beat += 1;
    if !advance {
        return Task::none();
    }
    r.stamp = Some((yes, Instant::now()));
    subj::after(BEAT_MS, rm(Msg::Beat(r.beat)))
}

/// Ctrl+Z in the review: the last answer, taken back.
pub fn take_back(app: &mut App) -> Task<Message> {
    let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
    let group = r.group.clone();
    let Some((_, faces)) = r.take_back() else {
        return app.show_status("No answer to take back.", 3000);
    };
    r.beat += 1;
    let via = r.via(&faces);
    app.search.link.answer(&group, &faces, None, via);
    let t = app.show_status("Took the last answer back.", 3000);
    Task::batch([t, subj::refresh_crops(app)])
}

/// Esc, Done or back: stop, and say what changed.
fn done(app: &mut App) -> Task<Message> {
    let Some(r) = app.subjects.review.take() else { return Task::none() };
    let (yes, no) = r.counts();
    let name = subj::label(&app.subjects, &r.group);
    let text = match (yes, no, r.checking) {
        (0, 0, _) => String::new(),
        (y, n, false) => {
            let mut s = format!("Added {} to {name}.", util::plural(y as u64, "photo"));
            if n > 0 {
                s.push_str(&format!(" {} won't be suggested again.", util::plural(n as u64, "photo")));
            }
            if y == 0 {
                s = format!("{} won't be suggested for {name} again.", util::plural(n as u64, "photo"));
            }
            s
        }
        (y, n, true) => format!("Confirmed {} in the group; took {} out.", util::plural(y as u64, "photo"), util::thousands(n as u64)),
    };
    let t = super::subjects_person::after_change(app);
    let refresh = subj::refresh_crops(app);
    if text.is_empty() {
        return Task::batch([t, refresh]);
    }
    let s = app.show_status(text, 8000);
    Task::batch([t, refresh, s])
}

/// Enter: the suggestion's photo in the viewer (read-only).
fn open_photo(app: &mut App) -> Task<Message> {
    let Some(r) = app.subjects.review.as_ref() else { return Task::none() };
    let Some(f) = r.list.as_ref().and_then(|l| l.get(r.at)).and_then(|e| e.first()).cloned() else { return Task::none() };
    let Some(index) = app.calendar.index.clone() else { return Task::none() };
    // The photo's records from the library index: no file is read.
    let Some(day) = index.get(f.day) else { return Task::none() };
    let mut coll = Collection::new();
    let mut first = None;
    for p in [f.jpg.as_ref(), f.raw.as_ref()].into_iter().flatten() {
        if let Some(e) = day.files.iter().find(|e| &e.path == p) {
            let mut rec = e.record();
            rec.capture_time = Some(f.capture);
            let id = coll.add(rec);
            first.get_or_insert(id);
        }
    }
    coll.build_pairs();
    let Some(id) = first else { return app.show_status("That photo isn't in the library index any more.", 5000) };
    super::viewer::open(app, super::viewer::Source::Library(Arc::new(coll)), vec![id], 0, true)
}

// ── keys ────────────────────────────────────────────────────────────────────

pub fn on_key(app: &mut App, key: &Key, mods: keyboard::Modifiers) -> Task<Message> {
    if mods.control() || mods.alt() {
        return Task::none();
    }
    let c = match key {
        Key::Character(c) => Some(c.to_lowercase()),
        _ => None,
    };
    match (key, c.as_deref()) {
        (_, Some("y")) => update(app, Msg::Answer(true)),
        (_, Some("n")) => update(app, Msg::Answer(false)),
        (Key::Named(Named::ArrowRight), _) | (_, Some("d")) => update(app, Msg::Step(1)),
        (Key::Named(Named::ArrowLeft), _) | (_, Some("a")) => update(app, Msg::Step(-1)),
        (Key::Named(Named::Enter), _) => update(app, Msg::Open),
        _ => Task::none(),
    }
}

// ── view ────────────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let Some(r) = s.review.as_ref() else { return Space::new().into() };
    let body: Element<'_, Message> = match &r.list {
        None => quiet("Finding the photos to check…".into(), String::new()),
        Some(l) if l.is_empty() => waiting_page(app, r),
        Some(_) if r.at_end() => end_page(app, r),
        Some(list) => {
            let main = row![reference(app, r), suggestion(app, r, list), photo(app, r, list)].spacing(40).align_y(Alignment::Center);
            let centre = container(main).width(Length::Fill).height(Length::Fill).center(Length::Fill).padding(Padding { top: 10.0, left: 24.0, right: 24.0, bottom: 0.0 });
            let stamp = r.stamp.map(|(yes, at)| {
                let name = subj::label(s, &r.group);
                let (g, label, tone) = if yes { (Glyph::Keep, format!("Added to {name}"), t::KEEP) } else { (Glyph::Prune, format!("Not {name}"), t::REJECT) };
                crate::ui::stamp::Stamp::new(Some(at), g, label, tone)
            });
            let centre: Element<'_, Message> = match stamp {
                Some(st) => stack![centre, st].into(),
                None => centre.into(),
            };
            let mut col = column![centre];
            if let Some(line) = super::fursuit_judge::review_line(app, &r.group, r.seq, r.left_out_shown) {
                col = col.push(line);
            }
            col.push(strip(app, r, list)).push(footer(app, r)).into()
        }
    };
    column![header(app, r), kit::hairline(), container(body).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND))].width(Length::Fill).height(Length::Fill).into()
}

fn quiet<'a>(title: String, body: String) -> Element<'a, Message> {
    let col = column![
        Space::new().height(Length::FillPortion(2)),
        text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(body).size(t::T_BODY).color(t::PHOTO_INK2),
        Space::new().height(Length::FillPortion(5)),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center);
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

fn header<'a>(app: &'a App, r: &'a Review) -> Element<'a, Message> {
    let s = &app.subjects;
    let name = s.group(&r.group).and_then(|g| g.name.clone());
    let one = r.kind.one();
    let back = tip(kit::icon_text_btn(Glyph::ChevronLeft, name.clone().unwrap_or_else(|| "Unnamed group".into()), t::INK2, style::secondary(), Some(rm(Msg::Done))), "Stop the review  (Esc)", true);
    let title = if r.checking { format!("Same {one} as this group?") } else { format!("Same {one}?") };
    let n = r.len();
    let (yes, no) = r.counts();
    let caption = if r.list.is_none() {
        String::new()
    } else if r.checking {
        format!("{} of {} to check. {yes} confirmed, {no} taken out.", (r.at + 1).min(n), util::plural(n as u64, "photo"))
    } else {
        format!("{} of {}. {yes} added, {no} not.", (r.at + 1).min(n), util::plural(n as u64, "suggestion"))
    };
    let answered = r.answers.iter().filter(|a| a.is_some()).count();
    let frac = if n == 0 { 0.0 } else { answered as f32 / n as f32 };
    let bar = iced::widget::progress_bar(0.0..=1.0, frac).length(160).girth(4).style(|_| iced::widget::progress_bar::Style {
        background: Background::Color(t::RAISED),
        bar: Background::Color(t::SIGNAL),
        border: Border { radius: 2.0.into(), ..Border::default() },
    });
    let on = app.settings.viewer_advance;
    let advance = tip(
        kit::icon_text_btn(Glyph::Skip, "Auto-advance", if on { t::SIGNAL } else { t::INK2 }, style::toggle(on, t::SIGNAL), Some(rm(Msg::Advance))),
        "Move on after each answer, as Advance after marking does in the viewer  (Ctrl+Shift+A)",
        true,
    );
    let r = row![
        back,
        column![text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK), kit::caption(caption)].spacing(1).padding(Padding { left: t::S3, ..Padding::ZERO }),
        Space::new().width(Length::Fill),
        bar,
        Space::new().width(t::S4),
        advance,
        kit::btn("Done", style::secondary(), Some(rm(Msg::Done))),
    ]
    .spacing(t::S2)
    .align_y(Alignment::Center);
    container(r.padding(Padding::from([0.0, t::S3]))).height(subj::HEADER_H).center_y(subj::HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// What the person looks like: their cover and a few confirmed faces.
fn reference<'a>(app: &'a App, r: &'a Review) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(r.kind);
    let Some(g) = s.group(&r.group) else { return Space::new().into() };
    let caption = match &g.name {
        Some(n) => format!("{n}, from {}", util::plural(g.photos as u64, "photo")),
        None => "This group".to_string(),
    };
    let cover = tile::crop(s.crops.get(&g.cover.face, subj::px(app, REF), Framing::Face), REF, shape);
    let mut grid = row![].spacing(8);
    for f in r.refs.iter().filter(|f| f.face != g.cover.face).take(6) {
        grid = grid.push(tile::crop(s.crops.get(&f.face, subj::px(app, REF_SMALL), Framing::Face), REF_SMALL, shape));
    }
    column![text(caption).size(t::T_SMALL).color(t::PHOTO_INK3), cover, container(grid.wrap().vertical_spacing(8)).width(REF_SMALL * 3.0 + 16.0)].spacing(8).width(186).into()
}

/// The suggestion, large, with Y and N under it.
fn suggestion<'a>(app: &'a App, r: &'a Review, list: &'a [Vec<Face>]) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(r.kind);
    let entry = &list[r.at];
    let Some(f) = entry.first() else { return Space::new().into() };
    let ans = r.answers.get(r.at).copied().flatten();
    let crop = tile::crop(s.crops.get(&f.face, subj::px(app, BIG), Framing::Face), BIG, shape);
    let crop = tile::stamped(crop, ans, BIG, shape);
    let answer_btn = |yes: bool| -> Element<'a, Message> {
        let (glyph, label, key, tone) = if yes { (Glyph::Check, "Yes", "Y", t::KEEP) } else { (Glyph::Close, "No", "N", t::REJECT) };
        let chosen = ans == Some(yes);
        let content = row![
            icons::view(glyph, 16.0, tone),
            text(label).size(t::T_BODY).font(paint::MEDIUM),
            container(text(key).size(11).font(paint::MEDIUM).color(alpha(tone, 0.8))).padding(Padding::from([0.0, 4.0])).style(move |_| container::Style { border: style::border(alpha(tone, 0.6), 1.0, 4.0), ..Default::default() })
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        // Chosen, it fills with its tone, as a mark lights its toggle.
        let look = style::toggle(true, tone);
        let look = if chosen {
            style::Look { bg: alpha(tone, 0.3), hover_bg: alpha(tone, 0.36), border: tone, hover_border: tone, ..look }
        } else {
            style::Look { fg: tone, hover_fg: tone, border: alpha(tone, 0.45), hover_border: alpha(tone, 0.7), bg: alpha(tone, 0.08), hover_bg: alpha(tone, 0.16), ..look }
        };
        button(container(content).center(Length::Fill)).width(150).height(38).padding(0).style(look.style()).on_press(rm(Msg::Answer(yes))).into()
    };
    let one = r.kind.one();
    let hint_yes = if r.checking { format!("Yes: the same {one} as this group  (Y)") } else { format!("Yes: add it to them  (Y)") };
    let hint_no = format!("No: not this {one}; never suggested again  (N)");
    let mut burst = if entry.len() > 1 { format!("A burst of {} frames: one answer covers them all.", entry.len()) } else { String::new() };
    if let Some(o) = super::fursuit_judge::origin_words(r, &f.face) {
        burst = if burst.is_empty() { o.to_string() } else { format!("{o} {burst}") };
    }
    column![crop, row![tip(answer_btn(true), hint_yes, false), tip(answer_btn(false), hint_no, false)].spacing(10), text(burst).size(t::T_SMALL).color(t::PHOTO_INK2).height(16)]
        .spacing(14)
        .align_x(Alignment::Center)
        .into()
}

/// The whole photo, with the face outlined.
fn photo<'a>(app: &'a App, r: &'a Review, list: &'a [Vec<Face>]) -> Element<'a, Message> {
    let s = &app.subjects;
    let entry = &list[r.at];
    let Some(f) = entry.first() else { return Space::new().into() };
    let px = subj::px(app, PHOTO_W);
    let handle = s.crops.get(&f.face, px, Framing::Photo);
    let size = s.crops.size(&f.face, px, Framing::Photo).filter(|z| z.0 > 0).unwrap_or((3, 2));
    let h = (PHOTO_W * size.1 as f32 / size.0 as f32).min(PHOTO_W).round();
    let pb: Element<'a, Message> = PhotoBox { handle, size, bbox: f.face.bbox, width: PHOTO_W, height: h }.into();
    let mount = container(pb).padding(5).style(|_| container::Style { background: Some(Background::Color(t::PHOTO_MOUNT)), border: Border { radius: t::R_CELL.into(), ..Border::default() }, ..Default::default() });
    let caption = if entry.len() > 1 { format!("In the photo: the first of {} frames", entry.len()) } else { "In the photo".to_string() };
    let name = f.jpg.as_deref().or(f.raw.as_deref()).map(util::stem).unwrap_or_default();
    let when = format!("{}, {}", super::search::long_date(f.day), f.capture.format("%H:%M:%S"));
    let open = button(text("Open the photo  Enter").size(t::T_SMALL).font(paint::MEDIUM)).padding(Padding::from([2.0, 0.0])).style(style::link().style()).on_press(rm(Msg::Open));
    column![text(caption).size(t::T_SMALL).color(t::PHOTO_INK3), mount, text(name).size(t::T_SMALL).color(t::PHOTO_INK2), text(when).size(t::T_SMALL).color(t::PHOTO_INK3), open]
        .spacing(6)
        .width(PHOTO_W + 10.0)
        .into()
}

/// The suggestions along the bottom, each with its answer stamped, the one
/// shown ringed; a click goes back to one.
fn strip<'a>(app: &'a App, r: &'a Review, list: &'a [Vec<Face>]) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(r.kind);
    let first = strip_start(r);
    let mut cells = row![].spacing(8);
    for (i, entry) in list.iter().enumerate().skip(first).take(STRIP_N) {
        let Some(f) = entry.first() else { continue };
        let crop = tile::crop(s.crops.get(&f.face, subj::px(app, STRIP), Framing::Face), STRIP, shape);
        let mut cell: Element<'a, Message> = tile::stamped(crop, r.answers.get(i).copied().flatten(), STRIP, shape);
        if entry.len() > 1 {
            let badge = container(text(entry.len().to_string()).size(10).font(paint::SEMIBOLD).color(t::PHOTO_INK2))
                .padding(Padding::from([0.0, 4.0]))
                .style(|_| container::Style { background: Some(Background::Color(alpha(t::PHOTO_GROUND, 0.85))), border: Border { radius: 3.0.into(), ..Border::default() }, ..Default::default() });
            cell = stack![cell, container(badge).width(STRIP).height(STRIP).align_bottom(STRIP).padding(Padding { left: 3.0, bottom: 3.0, ..Padding::ZERO })].into();
        }
        let current = i == r.at;
        let b = button(cell).padding(2).style(move |_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: None,
                border: Border { color: if current { t::SIGNAL } else if hot { t::PHOTO_LINE } else { iced::Color::TRANSPARENT }, width: 2.0, radius: (shape.radius(STRIP) + 2.0).into() },
                ..button::Style::default()
            }
        });
        cells = cells.push(b.on_press(rm(Msg::At(i))));
    }
    if let Some(tail) = super::fursuit_judge::strip_tail(app, &r.group, r.seq, STRIP, shape) {
        cells = cells.push(tail);
    }
    let row = scrollable(container(cells).padding(Padding::from([10.0, 16.0]))).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0).scroller_width(0)));
    column![container(Space::new().width(Length::Fill).height(1)).style(style::fill(t::PHOTO_LINE)), row].into()
}

fn footer<'a>(app: &'a App, r: &'a Review) -> Element<'a, Message> {
    let name = subj::label(&app.subjects, &r.group);
    let note = if r.checking { "Answers go to this group at once, and Ctrl+Z takes one back. A burst takes one answer.".to_string() } else { format!("Answers go to {name} at once, and Ctrl+Z takes one back. A burst takes one answer.") };
    let key = |k: &'a str, what: &'a str| row![text(k).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::PHOTO_INK), text(what).size(t::T_SMALL).color(t::PHOTO_INK3)].spacing(4);
    let keys = row![key("Y", "Yes"), key("N", "No"), key("← →", "Step"), key("Enter", "Open photo"), key("Esc", "Done")].spacing(16);
    container(row![text(note).size(t::T_SMALL).color(t::PHOTO_INK3).width(Length::Fill), keys].align_y(Alignment::Center))
        .padding(Padding::from([7.0, 20.0]))
        .width(Length::Fill)
        .into()
}

/// Nothing sure to offer yet, while the judge checks the close calls (or
/// after it left them all out).
fn waiting_page<'a>(app: &'a App, r: &'a Review) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(r.kind);
    let name = subj::label(s, &r.group);
    let cover = s.group(&r.group).map(|g| tile::crop(s.crops.get(&g.cover.face, subj::px(app, REF), Framing::Face), 72.0, shape)).unwrap_or_else(|| Space::new().into());
    let (title, body) = if r.judge_done {
        (format!("Nothing more like {name} for now"), "The judge left out every close call it checked. You can still look at them.".to_string())
    } else {
        (format!("Checking close calls for {name}"), "No photo is close enough to offer straight away. Those the judge thinks are this fursuit appear here as they come, for your Y or N.".to_string())
    };
    let mut col = column![
        Space::new().height(Length::FillPortion(2)),
        cover,
        text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(body).size(t::T_BODY).color(t::PHOTO_INK2),
        Space::new().height(t::S1),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(560);
    if let Some(line) = super::fursuit_judge::review_line(app, &r.group, r.seq, r.left_out_shown) {
        col = col.push(container(line).max_width(620));
    }
    col = col.push(kit::btn(format!("Back to {name}"), style::secondary(), Some(rm(Msg::Done))));
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

/// Every suggestion answered: what changed, and the next steps.
fn end_page<'a>(app: &'a App, r: &'a Review) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(r.kind);
    let n = r.len();
    let (yes, no) = r.counts();
    let name = s.group(&r.group).and_then(|g| g.name.clone());
    let who = name.clone().unwrap_or_else(|| "this group".into());
    let cover = s.group(&r.group).map(|g| tile::crop(s.crops.get(&g.cover.face, subj::px(app, REF), Framing::Face), 72.0, shape)).unwrap_or_else(|| Space::new().into());
    let line = if r.checking {
        format!("{} confirmed in {who}. {} taken out.", util::plural(yes as u64, "photo"), util::plural(no as u64, "photo"))
    } else {
        format!("{} added to {who}. {} won't be suggested again.", util::plural(yes as u64, "photo"), util::plural(no as u64, "photo"))
    };
    let back = kit::btn(format!("Back to {}", name.unwrap_or_else(|| "the group".into())), style::primary(), Some(rm(Msg::Done)));
    let mut buttons = row![].spacing(8);
    if !r.checking {
        buttons = buttons.push(kit::btn("Find more", style::secondary(), Some(rm(Msg::FindMore))));
    }
    buttons = buttons.push(back);
    let mut col = column![
        Space::new().height(Length::FillPortion(2)),
        cover,
        text(format!("All {n} answered")).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(line).size(t::T_BODY).color(t::PHOTO_INK2),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(520);
    // Close calls still being checked land here, and the review carries on.
    if !r.judge_done {
        col = col.push(text("Close calls are still being checked. Those the judge thinks are this fursuit appear here as they come.").size(t::T_BODY).color(t::PHOTO_INK2));
    }
    col = col.push(Space::new().height(t::S1));
    col = col.push(buttons);
    if let Some(line) = super::fursuit_judge::review_line(app, &r.group, r.seq, r.left_out_shown) {
        col = col.push(container(line).max_width(620));
    }
    if !r.checking {
        col = col.push(text("Find more looks again, using everything you confirmed. It's strict, so expect a few at a time.").size(t::T_SMALL).color(t::PHOTO_INK3));
    }
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

pub fn debug_state(app: &App) -> String {
    let Some(r) = app.subjects.review.as_ref() else { return "review=none".into() };
    let (yes, no) = r.counts();
    let entries: Vec<usize> = r.list.as_ref().map(|l| l.iter().map(Vec::len).collect()).unwrap_or_default();
    format!(
        "review=[group={} checking={} list={:?} at={} answers={:?} yes={yes} no={no} end={} refs={} advance={} judged={} left_out={} shown={} judge_done={}]",
        people::short(&r.group),
        r.checking,
        entries,
        r.at,
        r.answers.iter().map(|a| match a {
            Some(true) => 'y',
            Some(false) => 'n',
            None => '.',
        }).collect::<String>(),
        r.at_end(),
        r.refs.len(),
        app.settings.viewer_advance,
        r.judged_faces.len(),
        r.left_out_faces.len(),
        r.left_out_shown,
        r.judge_done,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key;
    use chrono::NaiveDate;

    fn face(n: u8, burst: Option<u64>) -> Face {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        Face { face: FaceRef { key: Key([n; 16]), bbox: [0.4, 0.2, 0.1, 0.1] }, jpg: None, raw: None, day, capture: day.and_hms_opt(10, 0, n as u32).unwrap(), burst, clarity: 0.5, confirmed: false }
    }

    fn review() -> Review {
        // Three suggestions: a single frame, a burst of three, a single.
        let list = vec![vec![face(1, None)], vec![face(2, Some(7)), face(3, Some(7)), face(4, Some(7))], vec![face(5, None)]];
        let mut r = Review::new(GroupId::Cluster(1), Kind::Person, false, 1, 2);
        r.answers = vec![None; list.len()];
        r.list = Some(Arc::new(list));
        r
    }

    #[test]
    fn one_answer_covers_a_whole_burst() {
        let mut r = review();
        r.at = 1;
        let (faces, before) = r.answer(true).unwrap();
        assert_eq!(faces.len(), 3, "every frame of the press");
        assert_eq!(before, None);
        assert_eq!(r.counts(), (3, 0), "counted by photo");
        // The next open one, then the end.
        assert_eq!(r.next_open(1), 2);
        r.at = 2;
        r.answer(false);
        assert_eq!(r.counts(), (3, 1));
        assert_eq!(r.next_open(0), 3, "0 is open but behind: the end comes after the last");
        r.at = 3;
        assert!(r.at_end());
    }

    /// The judge's verdicts join the end of the list, each once, so
    /// nothing on screen moves and every answer stays where it was.
    #[test]
    fn judged_suggestions_join_the_end_and_answers_stay_put() {
        let mut r = review();
        r.answer(true);
        let before: Vec<FaceRef> = r.list.as_ref().unwrap().iter().map(|e| e[0].face).collect();
        r.take(&[vec![face(9, None)], vec![face(1, None)]], false);
        let after: Vec<FaceRef> = r.list.as_ref().unwrap().iter().map(|e| e[0].face).collect();
        assert_eq!(&after[..before.len()], &before[..], "nothing on screen moves");
        assert_eq!(after.len(), before.len() + 1, "an entry already listed isn't added twice");
        assert_eq!(r.answers, [Some(true), None, None, None]);
        assert!(r.judged_faces.contains(&face(9, None).face) && r.left_out_faces.is_empty());
        r.take(&[vec![face(10, None)]], true);
        assert!(r.left_out_faces.contains(&face(10, None).face), "shown left-out ones are marked");
        assert_eq!(r.len(), 5);
        // At the end, a new arrival is the next to answer.
        r.at = 5;
        assert!(r.at_end());
        r.take(&[vec![face(11, None)]], false);
        assert!(!r.at_end());
        // An empty first answer stays open for the judge's.
        let mut e = Review::new(GroupId::Cluster(1), Kind::Fursuit, false, 1, 2);
        e.take(&[], false);
        assert_eq!(e.len(), 0);
        assert!(e.list.is_some());
    }

    #[test]
    fn ctrl_z_takes_back_the_last_answer_and_goes_to_it() {
        let mut r = review();
        r.answer(true);
        r.at = 2;
        r.answer(false);
        let (i, faces) = r.take_back().unwrap();
        assert_eq!((i, r.at, faces.len()), (2, 2, 1));
        assert_eq!(r.answers, [Some(true), None, None]);
        // Answering one again moves it to the end of the order.
        r.at = 0;
        let (_, before) = r.answer(false).unwrap();
        assert_eq!(before, Some(true), "the old answer, to take back first");
        assert_eq!(r.take_back().unwrap().0, 0);
        assert!(r.take_back().is_none());
    }
}
