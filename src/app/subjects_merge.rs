//! "Possibly the same fursuit" (fursuit merging, stage 2): pairs of
//! fursuit groups that may be one suit split apart, to merge or tell
//! apart with one key each.
//!
//! - **On the Fursuits tab**, a row of pairs, nearest first, over the
//!   groups; a click opens the review at that pair.
//! - **The review**, in the style of "Same fursuit?": the two groups side
//!   by side (their covers large, a few more heads under them), **Y**
//!   merges them (the named one keeps its name; two named ones ask which
//!   name to keep), **N** says they are not the same (remembered by their
//!   heads, so the pair isn't offered again after a regroup), ← → step,
//!   **Z** (or Ctrl+Z) takes the last answer back, Esc stops.  Each answer
//!   is one undo step.
//! - **With a close-call judge on** (card only), the judge compares the
//!   close pairs, three head pairs each: the pairs it thinks are one suit
//!   join the end of the list as they come; those it doesn't are counted
//!   and can be shown.  With it off, the distance alone decides.
//! - **On a fursuit's page**, *Similar groups*: its nearest pairs with
//!   *Merge* and ×.
//!
//! Nothing merges without the user's click or key; fursuits only.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, row, scrollable, stack, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{Face, FaceRef, Framing, GroupId, Kind};
use super::style;
use super::subjects::{self as subj, MergeAsk, Page};
use super::viewer_advance::BEAT_MS;
use super::{App, Message, View};
use crate::model::decisions::{At, Via};
use crate::search::merging::{self, GroupPair};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme::{self as t, alpha};
use crate::util;

pub fn m(msg: Msg) -> Message {
    Message::Subjects(subj::Msg::Merges(msg))
}

/// The review's crops: the covers, the heads under them, the strip.
const BIG: f32 = 220.0;
const SMALL: f32 = 52.0;
const STRIP: f32 = 40.0;
const STRIP_N: usize = 12;
/// The tab's pair cards.
const CARD: f32 = 64.0;
const CARDS: usize = 6;
/// A group page's *Similar groups*.
const SIMILAR: f32 = 32.0;
const SIMILAR_N: usize = 4;

#[derive(Debug, Clone)]
pub enum Msg {
    /// Open the review (at the tab's pair `i`).
    Review(Option<usize>),
    Answer(bool),
    Step(i32),
    At(usize),
    Beat(u64),
    TakeBack,
    Done,
    /// Show the pairs the judge left out, at the end of the list.
    ShowLeftOut,
    /// Compare the next pairs past the cap.
    CheckMore,
    /// *Similar groups* on a fursuit's page: merge with pair `i`, or not
    /// the same.
    SimilarMerge(usize),
    SimilarNotSame(usize),
}

/// The lists the engine last sent: the tab's, and a group page's.
#[derive(Debug, Default)]
pub struct Lists {
    pub tab: Option<Arc<Vec<GroupPair>>>,
    tab_seq: u64,
    pub similar: Option<(GroupId, Arc<Vec<GroupPair>>)>,
    similar_seq: u64,
}

/// An answer in the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Merged,
    NotSame,
}

/// The judge's comparisons for the review, as they land.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub offered: usize,
    pub left_out: Vec<GroupPair>,
    pub done: usize,
    pub total: usize,
    pub beyond: usize,
    /// × was pressed, or the judge failed: the rest wasn't compared.
    pub stopped: bool,
    pub dropped: bool,
}

impl Run {
    pub fn finished(&self) -> bool {
        self.stopped || self.done >= self.total
    }
}

pub struct MergeReview {
    pub seq: u64,
    /// Asked with the judge (it can run).
    pub judging: bool,
    pub list: Vec<GroupPair>,
    pub loaded: bool,
    pub at: usize,
    pub answers: Vec<Option<Answer>>,
    /// Answers in the order given, for Z.
    order: Vec<usize>,
    beat: u64,
    stamp: Option<(Answer, Instant)>,
    pub run: Option<Run>,
    /// Pairs the judge brought in, and the left-out ones shown.
    pub judged: HashSet<(GroupId, GroupId)>,
    pub left_out_keys: HashSet<(GroupId, GroupId)>,
    pub left_out_shown: bool,
    /// Asked again after a merge changed the groups.
    refreshing: bool,
    /// Two named groups: the merge dialog asks which name to keep for
    /// this pair.
    pending_merge: Option<usize>,
}

impl MergeReview {
    fn new(seq: u64, judging: bool) -> MergeReview {
        MergeReview {
            seq,
            judging,
            list: Vec::new(),
            loaded: false,
            at: 0,
            answers: Vec::new(),
            order: Vec::new(),
            beat: 0,
            stamp: None,
            run: judging.then(Run::default),
            judged: HashSet::new(),
            left_out_keys: HashSet::new(),
            left_out_shown: false,
            refreshing: false,
            pending_merge: None,
        }
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn at_end(&self) -> bool {
        self.loaded && self.at >= self.len()
    }

    fn next_open(&self, i: usize) -> usize {
        (i + 1..self.len()).find(|&j| self.answers[j].is_none()).unwrap_or(self.len())
    }

    /// Pairs not listed yet join the end, each once.
    pub fn append(&mut self, pairs: &[GroupPair]) -> Vec<(GroupId, GroupId)> {
        let have: HashSet<(GroupId, GroupId)> = self.list.iter().map(GroupPair::key).collect();
        let mut added = Vec::new();
        for p in pairs {
            let k = p.key();
            if !have.contains(&k) && !added.contains(&k) {
                self.list.push(p.clone());
                self.answers.push(None);
                added.push(k);
            }
        }
        added
    }

    /// Merged and not the same, counted by pair.
    pub fn counts(&self) -> (usize, usize) {
        let merged = self.answers.iter().filter(|a| **a == Some(Answer::Merged)).count();
        let not = self.answers.iter().filter(|a| **a == Some(Answer::NotSame)).count();
        (merged, not)
    }

    /// After the groups changed (a merge): the answered pairs stay where
    /// they are; the rest is the fresh list, without pairs listed already.
    fn refreshed(&mut self, fresh: &[GroupPair]) {
        let current = self.list.get(self.at).map(GroupPair::key).filter(|_| self.answers.get(self.at).copied().flatten().is_some());
        let kept: Vec<(GroupPair, Option<Answer>)> = self.list.iter().cloned().zip(self.answers.iter().copied()).filter(|(_, a)| a.is_some()).collect();
        let old_order: Vec<(GroupId, GroupId)> = self.order.iter().filter_map(|&i| self.list.get(i).map(GroupPair::key)).collect();
        self.list = kept.iter().map(|(p, _)| p.clone()).collect();
        self.answers = kept.iter().map(|(_, a)| *a).collect();
        self.order = old_order.iter().filter_map(|k| self.list.iter().position(|p| &p.key() == k)).collect();
        let first_new = self.list.len();
        self.append(fresh);
        self.at = current.and_then(|k| self.list.iter().position(|p| p.key() == k)).unwrap_or(first_new);
        self.beat += 1;
    }
}

// ── asking ──────────────────────────────────────────────────────────────────

/// Ask for the tab's pairs when the Fursuits tab shows its groups.
pub fn ask_tab(app: &mut App) {
    let s = &app.subjects;
    if app.view != View::Subjects || s.tab != Kind::Fursuit || s.page() != Page::Overview || super::subjects_fursuits::state(app) != super::subjects_fursuits::State::Groups {
        return;
    }
    let seq = app.subjects.next_seq();
    app.subjects.lists.tab_seq = seq;
    let min = app.settings.people_min_photos();
    app.search.link.merge_suggestions(seq, None, min, false);
}

/// Ask for a fursuit page's *Similar groups*.
pub fn ask_similar(app: &mut App) {
    let Some(p) = app.subjects.person.as_ref().filter(|p| p.kind == Kind::Fursuit) else { return };
    if app.subjects.review.is_some() {
        return;
    }
    let group = p.group.clone();
    let seq = app.subjects.next_seq();
    app.subjects.lists.similar_seq = seq;
    let min = app.settings.people_min_photos();
    app.search.link.merge_suggestions(seq, Some(&group), min, false);
}

/// The groups changed: the lists are asked again; the review asks again
/// when a pair still to answer names a group that went (merged).
pub fn on_subjects(app: &mut App) {
    ask_tab(app);
    if app.subjects.page() == Page::Person {
        ask_similar(app);
    }
    let all = app.subjects.all.clone();
    let Some(r) = app.subjects.merges.as_mut() else { return };
    let stale = r.list.iter().zip(&r.answers).any(|(p, a)| a.is_none() && (all.group(&p.a).is_none() || all.group(&p.b).is_none()));
    if stale && !r.refreshing && r.loaded {
        refresh(app);
    }
}

fn refresh(app: &mut App) {
    let seq = app.subjects.next_seq();
    let min = app.settings.people_min_photos();
    let Some(r) = app.subjects.merges.as_mut() else { return };
    r.refreshing = true;
    r.seq = seq;
    let judging = r.judging && super::fursuit_judge::active(&app.search.status);
    r.judging = judging;
    if judging {
        r.run = Some(Run::default());
    }
    app.search.link.merge_suggestions(seq, None, min, judging);
}

pub fn on_merge_suggestions(app: &mut App, seq: u64, focus: Option<GroupId>, list: Arc<Vec<GroupPair>>, judging: bool) -> Task<Message> {
    let lists = &mut app.subjects.lists;
    if seq == lists.tab_seq && focus.is_none() {
        lists.tab = Some(list.clone());
    }
    if seq == lists.similar_seq {
        if let Some(f) = focus.clone() {
            lists.similar = Some((f, list.clone()));
        }
    }
    if let Some(r) = app.subjects.merges.as_mut().filter(|r| r.seq == seq) {
        if r.refreshing {
            r.refreshing = false;
            r.refreshed(&list);
        } else {
            r.loaded = true;
            r.append(&list);
        }
        r.judging = judging;
        if !judging && r.list.is_empty() {
            app.subjects.merges = None;
            return app.show_status("No groups look like the same fursuit now.", 5000);
        }
    }
    subj::refresh_crops(app)
}

/// The judge's verdicts on the review's pairs: those it offers join the
/// end; those it doesn't are counted (and join too, marked, once shown).
pub fn on_merge_judged(app: &mut App, seq: u64, offered: Arc<Vec<GroupPair>>, left_out: Arc<Vec<GroupPair>>, done: usize, total: usize, beyond: usize) -> Task<Message> {
    let Some(r) = app.subjects.merges.as_mut().filter(|r| r.seq == seq) else { return Task::none() };
    let run = r.run.get_or_insert_with(Run::default);
    if run.stopped {
        return Task::none();
    }
    run.offered += offered.len();
    run.left_out.extend(left_out.iter().cloned());
    run.done = done.max(run.done);
    run.total = total;
    run.beyond = beyond;
    let finished = run.finished();
    let nothing = run.left_out.is_empty() && run.beyond == 0;
    for k in r.append(&offered) {
        r.judged.insert(k);
    }
    if r.left_out_shown {
        for k in r.append(&left_out) {
            r.left_out_keys.insert(k);
        }
    }
    if finished && r.list.is_empty() && nothing {
        app.subjects.merges = None;
        return app.show_status("No groups look like the same fursuit now: the judge thinks the close ones are different.", 6000);
    }
    subj::refresh_crops(app)
}

/// × on the judge's line.
pub fn stopped(app: &mut App) {
    if let Some(run) = app.subjects.merges.as_mut().and_then(|r| r.run.as_mut()) {
        run.stopped = true;
    }
}

/// The status changed: a judge that failed drops the comparisons in hand.
pub fn on_status(app: &mut App) {
    use crate::ml::judge::JudgeState;
    if matches!(app.search.status.judge.state, JudgeState::Failed(_) | JudgeState::Broken(_)) {
        if let Some(run) = app.subjects.merges.as_mut().and_then(|r| r.run.as_mut()).filter(|r| !r.finished()) {
            run.stopped = true;
            run.dropped = true;
        }
    }
}

/// The judge is comparing pairs for the review now.
pub fn comparing(app: &App) -> bool {
    app.subjects.merges.as_ref().and_then(|r| r.run.as_ref()).is_some_and(|r| !r.finished())
}

// ── crops ───────────────────────────────────────────────────────────────────

pub fn wanted_tab(app: &App) -> Vec<(Face, u32, Framing)> {
    let Some(list) = &app.subjects.lists.tab else { return Vec::new() };
    let px = subj::px(app, CARD);
    list.iter().take(CARDS).flat_map(|p| [p.a_heads.first(), p.b_heads.first()]).flatten().map(|f| (f.clone(), px, Framing::Face)).collect()
}

pub fn wanted_similar(app: &App) -> Vec<(Face, u32, Framing)> {
    let Some((g, list)) = &app.subjects.lists.similar else { return Vec::new() };
    let px = subj::px(app, SIMILAR);
    list.iter().take(SIMILAR_N).filter_map(|p| if &p.a == g { p.b_heads.first() } else { p.a_heads.first() }).map(|f| (f.clone(), px, Framing::Face)).collect()
}

pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let Some(r) = app.subjects.merges.as_ref() else { return Vec::new() };
    let (big, small, strip) = (subj::px(app, BIG), subj::px(app, SMALL), subj::px(app, STRIP));
    let mut v = Vec::new();
    for (i, p) in r.list.iter().enumerate().skip(r.at).take(2) {
        for heads in [&p.a_heads, &p.b_heads] {
            if let Some(c) = heads.first() {
                v.push((c.clone(), big, Framing::Face));
            }
            if i == r.at {
                v.extend(heads.iter().skip(1).take(3).map(|f| (f.clone(), small, Framing::Face)));
            }
        }
    }
    let first = strip_start(r);
    v.extend(r.list.iter().skip(first).take(STRIP_N).flat_map(|p| [p.a_heads.first(), p.b_heads.first()]).flatten().map(|f| (f.clone(), strip, Framing::Face)));
    v
}

fn strip_start(r: &MergeReview) -> usize {
    r.at.saturating_sub(5).min(r.len().saturating_sub(STRIP_N))
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Review(at) => start(app, at),
        Msg::Answer(yes) => answer(app, yes),
        Msg::Step(d) => {
            let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
            let max = r.len().saturating_sub(1);
            r.at = (r.at as i64 + d as i64).clamp(0, max as i64) as usize;
            r.beat += 1;
            subj::refresh_crops(app)
        }
        Msg::At(i) => {
            let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
            r.at = i.min(r.len());
            r.beat += 1;
            subj::refresh_crops(app)
        }
        Msg::Beat(b) => {
            let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
            if r.beat != b || r.answers.get(r.at).copied().flatten().is_none() {
                return Task::none();
            }
            r.at = r.next_open(r.at);
            subj::refresh_crops(app)
        }
        Msg::TakeBack => take_back(app),
        Msg::Done => done(app),
        Msg::ShowLeftOut => {
            let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
            let Some(left) = r.run.as_ref().map(|x| x.left_out.clone()) else { return Task::none() };
            r.left_out_shown = true;
            let first = r.len();
            for k in r.append(&left) {
                r.left_out_keys.insert(k);
            }
            if r.len() > first {
                r.at = first;
            }
            subj::refresh_crops(app)
        }
        Msg::CheckMore => {
            if app.subjects.merges.is_none() {
                return Task::none();
            }
            refresh(app);
            app.show_status(format!("Comparing the next {} possible merges, nearest first.", merging::MAX_PAIRS), 4000)
        }
        Msg::SimilarMerge(i) => similar_merge(app, i),
        Msg::SimilarNotSame(i) => similar_not_same(app, i),
    }
}

/// Open the review: the tab's pairs with the judge's on top when it can
/// run, starting at pair `at`.
fn start(app: &mut App, at: Option<usize>) -> Task<Message> {
    let judging = super::fursuit_judge::active(&app.search.status);
    let seq = app.subjects.next_seq();
    let mut r = MergeReview::new(seq, judging);
    // The tab's list at once (it is what was clicked); the engine's answer
    // adds what is new.  With the judge, only the pair clicked: the rest
    // are the judge's to offer or leave out.
    if let Some(list) = app.subjects.lists.tab.clone() {
        if judging {
            if let Some(p) = at.and_then(|i| list.get(i)) {
                r.append(std::slice::from_ref(p));
            }
        } else {
            r.append(&list);
            r.loaded = !list.is_empty();
            r.at = at.unwrap_or(0).min(r.len().saturating_sub(1));
        }
    }
    app.subjects.merges = Some(r);
    app.quicklook = None;
    let min = app.settings.people_min_photos();
    app.search.link.merge_suggestions(seq, None, min, judging);
    subj::refresh_crops(app)
}

/// The two groups of a pair, as they are now (`None`: one went).
fn groups_of<'a>(app: &'a App, p: &GroupPair) -> Option<(&'a super::people::Group, &'a super::people::Group)> {
    Some((app.subjects.group(&p.a)?, app.subjects.group(&p.b)?))
}

/// Which group a merge keeps: the named one; of two unnamed, the one with
/// more photos.  `None`: both named (the dialog asks).
fn into_of(a: &super::people::Group, b: &super::people::Group) -> Option<GroupId> {
    match (a.name.is_some(), b.name.is_some()) {
        (true, true) => None,
        (true, false) => Some(a.id.clone()),
        (false, true) => Some(b.id.clone()),
        (false, false) => Some(if b.photos > a.photos { b.id.clone() } else { a.id.clone() }),
    }
}

/// A pair as the decision record has it: offered by its distance, or by
/// the judge's majority.
fn via(p: &GroupPair, at: At) -> Via {
    Via::Pair { at, distance: p.distance, sure: p.sure, verdict: p.verdict }
}

fn answer(app: &mut App, yes: bool) -> Task<Message> {
    let advance = app.settings.viewer_advance;
    let Some(r) = app.subjects.merges.as_ref() else { return Task::none() };
    if r.at_end() {
        return Task::none();
    }
    let i = r.at;
    let Some(p) = r.list.get(i).cloned() else { return Task::none() };
    if r.answers[i].is_some() {
        return app.show_status("Already answered: Z takes it back.", 3000);
    }
    let Some((a, b)) = groups_of(app, &p) else {
        refresh(app);
        return app.show_status("One of these groups changed: here are the pairs as they are now.", 5000);
    };
    if a.kind != Kind::Fursuit || b.kind != Kind::Fursuit {
        return Task::none();
    }
    let what = if yes {
        match into_of(a, b) {
            Some(into) => {
                let other = if into == p.a { p.b.clone() } else { p.a.clone() };
                app.search.link.merge(&into, &[other], via(&p, At::MergeReview));
                Answer::Merged
            }
            None => {
                // Two names: the one dialog asks which to keep.
                let all = app.subjects.all.clone();
                app.subjects.merge = MergeAsk::new(&all, vec![p.a.clone(), p.b.clone()], Some(&p.a)).map(|m| MergeAsk { via: via(&p, At::MergeReview), ..m });
                if let Some(r) = app.subjects.merges.as_mut() {
                    r.pending_merge = Some(i);
                }
                return subj::refresh_crops(app);
            }
        }
    } else {
        let heads = |h: &[Face]| h.iter().map(|f| f.face).collect::<Vec<FaceRef>>();
        app.search.link.not_same(&p.a, &p.b, &heads(&p.a_heads), &heads(&p.b_heads), via(&p, At::MergeReview));
        Answer::NotSame
    };
    record(app, i, what, advance)
}

/// An answer given: stamped, and on to the next after the beat.
fn record(app: &mut App, i: usize, a: Answer, advance: bool) -> Task<Message> {
    let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
    r.answers[i] = Some(a);
    r.order.retain(|&x| x != i);
    r.order.push(i);
    r.beat += 1;
    if !advance {
        return Task::none();
    }
    r.stamp = Some((a, Instant::now()));
    subj::after(BEAT_MS, m(Msg::Beat(r.beat)))
}

/// The merge dialog merged a pair the review asked about (two named).
pub fn merged_by_dialog(app: &mut App) -> Task<Message> {
    let advance = app.settings.viewer_advance;
    let Some(i) = app.subjects.merges.as_mut().and_then(|r| r.pending_merge.take()) else { return Task::none() };
    record(app, i, Answer::Merged, advance)
}

/// The merge dialog was put away.
pub fn dialog_cancelled(app: &mut App) {
    if let Some(r) = app.subjects.merges.as_mut() {
        r.pending_merge = None;
    }
}

/// Z (or Ctrl+Z) in the review: the last answer taken back (the merge
/// undone, or "not the same" forgotten).
pub fn take_back(app: &mut App) -> Task<Message> {
    let Some(r) = app.subjects.merges.as_mut() else { return Task::none() };
    let Some(i) = r.order.pop() else { return app.show_status("No answer to take back.", 3000) };
    r.answers[i] = None;
    r.at = i;
    r.beat += 1;
    r.stamp = None;
    app.search.link.undo_subjects();
    let t = app.show_status("Took the last answer back.", 3000);
    Task::batch([t, subj::refresh_crops(app)])
}

fn done(app: &mut App) -> Task<Message> {
    let Some(r) = app.subjects.merges.take() else { return Task::none() };
    let (merged, not) = r.counts();
    let refresh = subj::refresh_crops(app);
    ask_tab(app);
    let text = match (merged, not) {
        (0, 0) => return refresh,
        (m, 0) => format!("Merged {}.", util::plural(m as u64, "pair")),
        (0, n) => format!("{} won't be suggested again.", util::plural(n as u64, "pair")),
        (m, n) => format!("Merged {}; {} won't be suggested again.", util::plural(m as u64, "pair"), util::thousands(n as u64)),
    };
    Task::batch([refresh, app.show_status(text, 6000)])
}

// ── *Similar groups* on a fursuit's page ────────────────────────────────────

fn similar_pair(app: &App, i: usize) -> Option<(GroupId, GroupPair)> {
    let p = app.subjects.person.as_ref()?;
    let (g, list) = app.subjects.lists.similar.as_ref()?;
    if *g != p.group {
        return None;
    }
    Some((g.clone(), list.get(i)?.clone()))
}

fn similar_merge(app: &mut App, i: usize) -> Task<Message> {
    let Some((me, pair)) = similar_pair(app, i) else { return Task::none() };
    let other_id = if pair.a == me { pair.b.clone() } else { pair.a.clone() };
    let (Some(mine), Some(other)) = (app.subjects.group(&me).cloned(), app.subjects.group(&other_id).cloned()) else {
        return app.show_status("That group changed: look again.", 4000);
    };
    if mine.kind != Kind::Fursuit || other.kind != Kind::Fursuit {
        return Task::none();
    }
    match (mine.name.is_some(), other.name.is_some()) {
        (true, true) => {
            let all = app.subjects.all.clone();
            app.subjects.merge = MergeAsk::new(&all, vec![me.clone(), other_id], Some(&me)).map(|m| MergeAsk { via: via(&pair, At::Similar), ..m });
            subj::refresh_crops(app)
        }
        (named_me, named_other) => {
            // The page's group is kept, unless only the other is named.
            let into = if named_other && !named_me { other_id.clone() } else { me.clone() };
            let gone = if into == me { other_id } else { me.clone() };
            if into != me {
                super::subjects_person::follow(app, &into);
            }
            app.search.link.merge(&into, &[gone], via(&pair, At::Similar));
            let name = mine.name.or(other.name).unwrap_or_else(|| "one group".into());
            app.show_status_action(format!("Merged the two groups into {name}."), "Undo", subj::m(subj::Msg::Undo))
        }
    }
}

fn similar_not_same(app: &mut App, i: usize) -> Task<Message> {
    let Some((_, pair)) = similar_pair(app, i) else { return Task::none() };
    let heads = |h: &[Face]| h.iter().map(|f| f.face).collect::<Vec<FaceRef>>();
    app.search.link.not_same(&pair.a, &pair.b, &heads(&pair.a_heads), &heads(&pair.b_heads), via(&pair, At::Similar));
    app.show_status_action("Not the same fursuit: that group won't be suggested here again.", "Undo", subj::m(subj::Msg::Undo))
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
        (_, Some("z")) => update(app, Msg::TakeBack),
        (Key::Named(Named::ArrowRight), _) | (_, Some("d")) => update(app, Msg::Step(1)),
        (Key::Named(Named::ArrowLeft), _) | (_, Some("a")) => update(app, Msg::Step(-1)),
        _ => Task::none(),
    }
}

// ── the tab's row ───────────────────────────────────────────────────────────

fn name_or_unnamed(app: &App, id: &GroupId) -> (String, bool) {
    match app.subjects.group(id).and_then(|g| g.name.clone()) {
        Some(n) => (n, true),
        None => ("Unnamed".to_string(), false),
    }
}

/// On the Fursuits tab, over the groups: the nearest pairs that may be one
/// suit, a click to review each; nothing when there are none.
pub fn tab_section(app: &App) -> Option<Element<'_, Message>> {
    let list = app.subjects.lists.tab.as_ref().filter(|l| !l.is_empty())?;
    let s = &app.subjects;
    let shape = subj::shape(Kind::Fursuit);
    let mut cards = row![].spacing(8);
    for (i, p) in list.iter().take(CARDS).enumerate() {
        let (Some(fa), Some(fb)) = (p.a_heads.first(), p.b_heads.first()) else { continue };
        let side = |f: &Face, id: &GroupId| -> Element<'_, Message> {
            let (name, named) = name_or_unnamed(app, id);
            let photos = s.group(id).map_or(0, |g| g.photos);
            column![
                tile::crop(s.crops.get(&f.face, subj::px(app, CARD), Framing::Face), CARD, shape),
                text(name).size(t::T_SMALL).font(paint::MEDIUM).color(if named { t::PHOTO_INK } else { t::PHOTO_INK3 }).wrapping(text::Wrapping::None),
                text(util::plural(photos as u64, "photo")).size(11).color(t::PHOTO_INK3),
            ]
            .spacing(3)
            .width(CARD + 4.0)
            .align_x(Alignment::Center)
            .into()
        };
        let body = row![side(fa, &p.a), container(icons::view(Glyph::Merge, 13.0, t::PHOTO_INK3)).height(CARD).center_y(CARD), side(fb, &p.b)].spacing(4).align_y(Alignment::Start);
        let b = button(body).padding(Padding { top: 10.0, bottom: 8.0, left: 8.0, right: 8.0 }).on_press(m(Msg::Review(Some(i)))).style(|_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style { background: Some(Background::Color(if hot { t::PHOTO_RAISED } else { t::PHOTO_MOUNT })), text_color: t::PHOTO_INK, border: Border { radius: t::R_CELL.into(), ..Border::default() }, ..button::Style::default() }
        });
        cards = cards.push(tip(b, "Check these two: Y merges them, N says they're different", true));
    }
    let n = list.len();
    let judge = &app.search.status.judge;
    let active = super::fursuit_judge::active(&app.search.status);
    let label = if n > CARDS { format!("Review all {n}") } else { "Review them".to_string() };
    let go = button(container(row![text(label).size(t::T_BODY).font(paint::MEDIUM), keycap_quiet("Y")].spacing(8).align_y(Alignment::Center)).center_y(Length::Fill))
        .height(t::CONTROL_H)
        .padding(Padding::from([0.0, t::S4]))
        .style(style::secondary().style())
        .on_press(m(Msg::Review(None)));
    let about = if active {
        format!("{} compares the close ones as you review, on the graphics card.", judge.choice.label())
    } else {
        "Groups are kept tight, so one suit can sit in several. Nothing merges until you say.".to_string()
    };
    let side = column![tip(go, "One pair at a time: Y merges, N says they're different", true), text(about).size(t::T_SMALL).color(t::PHOTO_INK3).width(220)].spacing(8);
    let r = row![cards, Space::new().width(t::S3), side].align_y(Alignment::Center);
    let note = format!("{}, nearest first", util::plural(n as u64, "pair"));
    Some(column![subj::heading("Possibly the same fursuit", note), container(scrollable(r).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0).scroller_width(0)))).padding(Padding { left: 16.0, right: 16.0, top: 0.0, bottom: 10.0 })].into())
}

fn keycap_quiet<'a>(k: &'a str) -> Element<'a, Message> {
    container(text(k).size(11).font(paint::MEDIUM).color(t::INK2)).padding(Padding::from([0.0, 4.0])).style(|_| container::Style { border: style::border(t::LINE_HI, 1.0, 4.0), ..Default::default() }).into()
}

/// Y on the tab with pairs to review.
pub fn tab_key(app: &mut App) -> Option<Task<Message>> {
    app.subjects.lists.tab.as_ref().filter(|l| !l.is_empty())?;
    Some(update(app, Msg::Review(None)))
}

// ── *Similar groups* ────────────────────────────────────────────────────────

/// Under a fursuit's band: the groups nearest it, each with *Merge* and ×.
pub fn similar_row<'a>(app: &'a App, group: &GroupId) -> Option<Element<'a, Message>> {
    let s = &app.subjects;
    let (g, list) = s.lists.similar.as_ref()?;
    if g != group || list.is_empty() {
        return None;
    }
    let shape = subj::shape(Kind::Fursuit);
    let mut r = row![text("Similar groups").size(t::T_SMALL).font(paint::MEDIUM).color(t::INK2)].spacing(t::S3).align_y(Alignment::Center);
    for (i, p) in list.iter().enumerate().take(SIMILAR_N) {
        let (other, heads) = if &p.a == group { (&p.b, &p.b_heads) } else { (&p.a, &p.a_heads) };
        let Some(f) = heads.first() else { continue };
        let (name, named) = name_or_unnamed(app, other);
        let photos = s.group(other).map_or(0, |g| g.photos);
        let crop = tile::crop(s.crops.get(&f.face, subj::px(app, SIMILAR), Framing::Face), SIMILAR, shape);
        let words = column![
            text(name).size(t::T_SMALL).font(paint::MEDIUM).color(if named { t::INK } else { t::INK2 }).wrapping(text::Wrapping::None),
            text(util::plural(photos as u64, "photo")).size(11).color(t::INK3)
        ]
        .spacing(0);
        let merge = tip(kit::btn("Merge", style::secondary(), Some(m(Msg::SimilarMerge(i)))).height(26).padding(Padding::from([0.0, 10.0])), "Merge it with this fursuit (Ctrl+Z undoes it)", true);
        let not = tip(kit::icon_btn(Glyph::Close, t::INK3, style::ghost(), Some(m(Msg::SimilarNotSame(i)))).width(24).height(24).padding(0), "Not the same fursuit: don't suggest it here again", true);
        let chip = container(row![crop, words, merge, not].spacing(8).align_y(Alignment::Center))
            .padding(Padding { left: 4.0, right: 4.0, top: 3.0, bottom: 3.0 })
            .style(|_| container::Style { background: Some(Background::Color(t::RAISED)), border: style::border(t::LINE_HI, 1.0, t::R_CONTROL), ..Default::default() });
        r = r.push(chip);
    }
    if list.len() > SIMILAR_N {
        r = r.push(text(format!("{} more on the Fursuits tab", list.len() - SIMILAR_N)).size(t::T_SMALL).color(t::INK3));
    }
    Some(container(r.padding(Padding::from([7.0, 12.0]))).width(Length::Fill).style(style::fill(t::MOUNT)).into())
}

// ── the review ──────────────────────────────────────────────────────────────

pub fn view(app: &App) -> Element<'_, Message> {
    let s = &app.subjects;
    let Some(r) = s.merges.as_ref() else { return Space::new().into() };
    let body: Element<'_, Message> = if !r.loaded && r.list.is_empty() && !judge_running(app, r) {
        quiet("Finding pairs of groups to check…".into())
    } else if r.list.is_empty() || (r.at_end() && r.len() == 0) {
        waiting_page(app, r)
    } else if r.at_end() {
        end_page(app, r)
    } else {
        let p = &r.list[r.at];
        let main = row![side(app, r, &p.a, &p.a_heads), container(icons::view(Glyph::Merge, 22.0, t::PHOTO_INK3)).height(BIG).center_y(BIG), side(app, r, &p.b, &p.b_heads)].spacing(28).align_y(Alignment::Start);
        let answer = r.answers.get(r.at).copied().flatten();
        let col = column![main, buttons(answer), origin(r, p)].spacing(16).align_x(Alignment::Center);
        let centre = container(col).width(Length::Fill).height(Length::Fill).center(Length::Fill).padding(Padding { top: 10.0, left: 24.0, right: 24.0, bottom: 0.0 });
        let stamp = r.stamp.map(|(a, at)| {
            let (g, label, tone) = match a {
                Answer::Merged => (Glyph::Keep, "Merged".to_string(), t::KEEP),
                Answer::NotSame => (Glyph::Prune, "Not the same".to_string(), t::REJECT),
            };
            crate::ui::stamp::Stamp::new(Some(at), g, label, tone)
        });
        let centre: Element<'_, Message> = match stamp {
            Some(st) => stack![centre, st].into(),
            None => centre.into(),
        };
        let mut col = column![centre];
        if let Some(line) = judge_line(app, r) {
            col = col.push(line);
        }
        col.push(strip(app, r)).push(footer()).into()
    };
    column![header(app, r), kit::hairline(), container(body).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND))].width(Length::Fill).height(Length::Fill).into()
}

fn judge_running(app: &App, r: &MergeReview) -> bool {
    r.judging && r.run.as_ref().is_some_and(|x| !x.finished()) && super::fursuit_judge::active(&app.search.status)
}

fn quiet<'a>(title: String) -> Element<'a, Message> {
    let col = column![Space::new().height(Length::FillPortion(2)), text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK), Space::new().height(Length::FillPortion(5))].align_x(Alignment::Center);
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

fn header<'a>(app: &'a App, r: &'a MergeReview) -> Element<'a, Message> {
    let back = tip(kit::icon_text_btn(Glyph::ChevronLeft, "Fursuits", t::INK2, style::secondary(), Some(m(Msg::Done))), "Stop the review  (Esc)", true);
    let n = r.len();
    let (merged, not) = r.counts();
    let caption = if n == 0 { String::new() } else { format!("{} of {}. {merged} merged, {not} not the same.", (r.at + 1).min(n), util::plural(n as u64, "pair")) };
    let answered = r.answers.iter().filter(|a| a.is_some()).count();
    let frac = if n == 0 { 0.0 } else { answered as f32 / n as f32 };
    let bar = iced::widget::progress_bar(0.0..=1.0, frac).length(160).girth(4).style(|_| iced::widget::progress_bar::Style {
        background: Background::Color(t::RAISED),
        bar: Background::Color(t::SIGNAL),
        border: Border { radius: 2.0.into(), ..Border::default() },
    });
    let on = app.settings.viewer_advance;
    let advance = tip(
        kit::icon_text_btn(Glyph::Skip, "Auto-advance", if on { t::SIGNAL } else { t::INK2 }, style::toggle(on, t::SIGNAL), Some(subj::m(subj::Msg::Review(super::subjects_review::Msg::Advance)))),
        "Move on after each answer, as Advance after marking does in the viewer  (Ctrl+Shift+A)",
        true,
    );
    let row = row![
        back,
        column![text("Same fursuit?").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK), kit::caption(caption)].spacing(1).padding(Padding { left: t::S3, ..Padding::ZERO }),
        Space::new().width(Length::Fill),
        bar,
        Space::new().width(t::S4),
        advance,
        kit::btn("Done", style::secondary(), Some(m(Msg::Done))),
    ]
    .spacing(t::S2)
    .align_y(Alignment::Center);
    container(row.padding(Padding::from([0.0, t::S3]))).height(subj::HEADER_H).center_y(subj::HEADER_H).width(Length::Fill).style(style::fill(t::MOUNT)).into()
}

/// One group of the pair: its name and size, its cover large, a few more
/// heads under it.
fn side<'a>(app: &'a App, r: &'a MergeReview, id: &'a GroupId, heads: &'a [Face]) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(Kind::Fursuit);
    let g = s.group(id);
    let name = g.and_then(|g| g.name.clone());
    let counts = g.map(|g| format!("{} over {}", util::plural(g.photos as u64, "photo"), util::plural(g.days as u64, "day"))).unwrap_or_else(|| "Changed since: merged or regrouped".into());
    let answer = r.answers.get(r.at).copied().flatten();
    let cover = heads.first().map(|f| tile::crop(s.crops.get(&f.face, subj::px(app, BIG), Framing::Face), BIG, shape)).unwrap_or_else(|| Space::new().width(BIG).height(BIG).into());
    let cover = tile::stamped(cover, answer.map(|a| a == Answer::Merged), BIG, shape);
    let mut more = row![].spacing(8);
    for f in heads.iter().skip(1).take(3) {
        more = more.push(tile::crop(s.crops.get(&f.face, subj::px(app, SMALL), Framing::Face), SMALL, shape));
    }
    column![
        text(name.clone().unwrap_or_else(|| "Unnamed group".into())).size(t::T_TITLE).font(paint::SEMIBOLD).color(if name.is_some() { t::PHOTO_INK } else { t::PHOTO_INK2 }),
        text(counts).size(t::T_SMALL).color(t::PHOTO_INK3),
        cover,
        more,
    ]
    .spacing(8)
    .width(BIG)
    .into()
}

fn buttons<'a>(answer: Option<Answer>) -> Element<'a, Message> {
    let b = |yes: bool| -> Element<'a, Message> {
        let (glyph, label, key, tone) = if yes { (Glyph::Merge, "Merge", "Y", t::KEEP) } else { (Glyph::Close, "Not the same", "N", t::REJECT) };
        let chosen = answer == Some(if yes { Answer::Merged } else { Answer::NotSame });
        let content = row![
            icons::view(glyph, 16.0, tone),
            text(label).size(t::T_BODY).font(paint::MEDIUM),
            container(text(key).size(11).font(paint::MEDIUM).color(alpha(tone, 0.8))).padding(Padding::from([0.0, 4.0])).style(move |_| container::Style { border: style::border(alpha(tone, 0.6), 1.0, 4.0), ..Default::default() })
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let look = style::toggle(true, tone);
        let look = if chosen {
            style::Look { bg: alpha(tone, 0.3), hover_bg: alpha(tone, 0.36), border: tone, hover_border: tone, ..look }
        } else {
            style::Look { fg: tone, hover_fg: tone, border: alpha(tone, 0.45), hover_border: alpha(tone, 0.7), bg: alpha(tone, 0.08), hover_bg: alpha(tone, 0.16), ..look }
        };
        let hint = if yes { "Yes: one fursuit. Merge them; a name stays  (Y)" } else { "No: different fursuits. This pair isn't offered again  (N)" };
        tip(button(container(content).center(Length::Fill)).width(170).height(38).padding(0).style(look.style()).on_press(m(Msg::Answer(yes))), hint, false)
    };
    row![b(true), b(false)].spacing(10).into()
}

/// Under the buttons: where the pair came from, when the judge brought it.
fn origin<'a>(r: &'a MergeReview, p: &'a GroupPair) -> Element<'a, Message> {
    let k = p.key();
    let words = if r.left_out_keys.contains(&k) {
        match p.verdict {
            Some(v) if v.unjudged > 0 => format!("The judge thinks these are different fursuits ({} of {} views say so; {} couldn't be compared).", v.no, v.asked(), v.unjudged),
            Some(v) => format!("The judge thinks these are different fursuits ({} of {} views).", v.no, v.asked()),
            None => "The judge thinks these are different fursuits.".to_string(),
        }
    } else if r.judged.contains(&k) {
        match p.verdict {
            Some(v) if v.no + v.unjudged == 0 => format!("The judge thinks these are one fursuit ({} of {} views agree).", v.yes, v.asked()),
            Some(v) => format!("The judge thinks these are one fursuit ({} of {} views).", v.yes, v.asked()),
            None => "The judge thinks these are one fursuit.".to_string(),
        }
    } else {
        String::new()
    };
    text(words).size(t::T_SMALL).color(t::PHOTO_INK2).height(16).into()
}

fn strip<'a>(app: &'a App, r: &'a MergeReview) -> Element<'a, Message> {
    let s = &app.subjects;
    let shape = subj::shape(Kind::Fursuit);
    let first = strip_start(r);
    let mut cells = row![].spacing(10);
    for (i, p) in r.list.iter().enumerate().skip(first).take(STRIP_N) {
        let ans = r.answers.get(i).copied().flatten().map(|a| a == Answer::Merged);
        let crop = |f: Option<&Face>| -> Element<'a, Message> {
            let c = tile::crop(f.and_then(|f| s.crops.get(&f.face, subj::px(app, STRIP), Framing::Face)), STRIP, shape);
            tile::stamped(c, ans, STRIP, shape)
        };
        let cell = row![crop(p.a_heads.first()), crop(p.b_heads.first())].spacing(2);
        let current = i == r.at;
        let b = button(cell).padding(2).style(move |_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: None,
                border: Border { color: if current { t::SIGNAL } else if hot { t::PHOTO_LINE } else { iced::Color::TRANSPARENT }, width: 2.0, radius: (shape.radius(STRIP) + 2.0).into() },
                ..button::Style::default()
            }
        });
        cells = cells.push(b.on_press(m(Msg::At(i))));
    }
    if let Some(run) = r.run.as_ref().filter(|x| !x.finished() && r.judging) {
        let left = run.total.saturating_sub(run.done);
        let words = column![icons::view(Glyph::CloseCall, 14.0, t::PHOTO_INK3), text(util::thousands(left as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(t::PHOTO_INK2)].spacing(1).align_x(Alignment::Center);
        let radius = shape.radius(STRIP);
        let tail = container(words).width(STRIP * 2.0 + 2.0).height(STRIP).center(STRIP).style(move |_| container::Style { border: Border { color: t::PHOTO_LINE, width: 1.0, radius: radius.into() }, background: Some(Background::Color(alpha(t::PHOTO_INK, 0.02))), ..Default::default() });
        cells = cells.push(tip(container(tail).padding(2), format!("{} still to compare", util::plural(left as u64, "pair")), false));
    }
    let row = scrollable(container(cells).padding(Padding::from([10.0, 16.0]))).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0).scroller_width(0)));
    column![container(Space::new().width(Length::Fill).height(1)).style(style::fill(t::PHOTO_LINE)), row].into()
}

fn footer<'a>() -> Element<'a, Message> {
    let note = "A merge or a no takes effect at once, and Z takes the last one back.";
    let key = |k: &'a str, what: &'a str| row![text(k).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::PHOTO_INK), text(what).size(t::T_SMALL).color(t::PHOTO_INK3)].spacing(4);
    let keys = row![key("Y", "Merge"), key("N", "Not the same"), key("← →", "Step"), key("Z", "Take back"), key("Esc", "Done")].spacing(16);
    container(row![text(note).size(t::T_SMALL).color(t::PHOTO_INK3).width(Length::Fill), keys].align_y(Alignment::Center)).padding(Padding::from([7.0, 20.0])).width(Length::Fill).into()
}

/// Over the strip: what the judge is doing for this review, or what it came
/// to, with *Show them* for the pairs it left out.
fn judge_line<'a>(app: &'a App, r: &'a MergeReview) -> Option<Element<'a, Message>> {
    use crate::ml::judge::JudgeState;
    let run = r.run.as_ref()?;
    let j = &app.search.status.judge;
    let mut line = row![].spacing(10).align_y(Alignment::Center);
    let live = !run.finished() && !matches!(j.state, JudgeState::Idle);
    let failed = matches!(j.state, JudgeState::Failed(_) | JudgeState::Broken(_));
    let shown_run = super::fursuit_judge::Run { total: run.total, done: run.done, ..Default::default() };
    match (live || (failed && run.dropped)).then(|| super::fursuit_judge::state_words(app, Some(&shown_run))).flatten() {
        Some((tone, title, detail)) => {
            line = line.push(icons::view(if failed { Glyph::GraphicsCard } else { Glyph::CloseCall }, 16.0, tone));
            let title = text(title).size(t::T_SMALL).font(paint::MEDIUM).color(if failed { t::WARN } else { t::PHOTO_INK });
            let detail = text(detail).size(t::T_SMALL).color(t::PHOTO_INK3);
            line = if failed { line.push(column![title, detail].spacing(1).width(Length::Fill)) } else { line.push(title).push(detail) };
            if let JudgeState::Comparing { done, total } = j.state {
                line = line.push(super::fursuit_judge::bar(if total == 0 { 0.0 } else { done as f32 / total as f32 }, 72.0, t::PHOTO_RAISED));
            }
            match &j.state {
                JudgeState::Failed(f) => {
                    line = line.push(super::fursuit_judge::link_btn(super::search_setup::retry_label(f), super::fursuit_judge::m(super::fursuit_judge::Msg::TryCard)));
                    line = line.push(super::fursuit_judge::link_btn("Preferences", super::fursuit_judge::m(super::fursuit_judge::Msg::OpenPrefs)));
                }
                JudgeState::Broken(_) => line = line.push(super::fursuit_judge::link_btn("Preferences", super::fursuit_judge::m(super::fursuit_judge::Msg::OpenPrefs))),
                _ => line = line.push(super::fursuit_judge::stop_btn(t::PHOTO_INK2)),
            }
        }
        None => {
            if run.total == 0 && run.left_out.is_empty() && run.beyond == 0 {
                return None;
            }
            line = line.push(icons::view(Glyph::CloseCall, 16.0, t::PHOTO_INK3));
            let said = if run.dropped {
                format!("Compared {} of {} before the judge stopped.", util::thousands(run.done as u64), util::plural(run.total as u64, "possible merge"))
            } else if run.stopped {
                format!("Stopped after {} of {}.", util::thousands(run.done as u64), util::plural(run.total as u64, "possible merge"))
            } else if run.total > 0 {
                format!("Compared {}: {} joined the list.", util::plural(run.total as u64, "possible merge"), util::thousands(run.offered as u64))
            } else {
                "The close pairs were compared before.".to_string()
            };
            line = line.push(text(said).size(t::T_SMALL).color(t::PHOTO_INK2));
            if run.beyond > 0 || run.dropped {
                let n = merging::batch(run.beyond).0;
                let label = if run.dropped { "Compare again".to_string() } else { format!("Compare the next {n}") };
                if run.beyond > 0 {
                    line = line.push(text(format!("{} more can be compared.", util::thousands(run.beyond as u64))).size(t::T_SMALL).color(t::PHOTO_INK3));
                }
                let takes = crate::search::estimate::describe_seconds(merging::compare_seconds(n.max(1), super::fursuits::judge_call_seconds(j.choice)));
                let hint = format!("Up to {takes} with the graphics card; × stops it. Pairs the judge thinks are one fursuit join the end of this list.");
                line = line.push(tip(super::fursuit_judge::link_btn(label, m(Msg::CheckMore)), super::search_setup::capital(&hint), false));
            }
        }
    }
    if !run.left_out.is_empty() {
        line = line.push(Space::new().width(Length::Fill));
        line = line.push(icons::view(Glyph::LeaveOut, 13.0, t::PHOTO_INK3));
        line = line.push(text(format!("{} compared and left out", util::thousands(run.left_out.len() as u64))).size(t::T_SMALL).color(t::PHOTO_INK3));
        if !r.left_out_shown {
            line = line.push(tip(super::fursuit_judge::link_btn("Show them", m(Msg::ShowLeftOut)), "Add them to the end of this list, to check yourself", false));
        } else {
            line = line.push(text("shown at the end").size(t::T_SMALL).color(t::PHOTO_INK4));
        }
    }
    Some(container(line).padding(Padding { top: 6.0, bottom: 6.0, left: 18.0, right: 14.0 }).width(Length::Fill).into())
}

/// Nothing to show yet while the judge compares the close pairs (or it
/// left them all out).
fn waiting_page<'a>(app: &'a App, r: &'a MergeReview) -> Element<'a, Message> {
    let running = judge_running(app, r);
    let (title, body) = if running {
        ("Comparing possible merges", "No pair is close enough to offer straight away. Pairs the judge thinks are one fursuit appear here as they come, for your Y or N.")
    } else {
        ("No pairs to check now", "The judge thinks the close pairs it compared are different fursuits. You can still look at them.")
    };
    let mut col = column![
        Space::new().height(Length::FillPortion(2)),
        icons::view(Glyph::Fursuit, 30.0, t::PHOTO_INK3),
        text(title).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(body).size(t::T_BODY).color(t::PHOTO_INK2),
        Space::new().height(t::S1),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(560);
    if let Some(line) = judge_line(app, r) {
        col = col.push(container(line).max_width(640));
    }
    col = col.push(kit::btn("Back to the fursuits", style::secondary(), Some(m(Msg::Done))));
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

fn end_page<'a>(app: &'a App, r: &'a MergeReview) -> Element<'a, Message> {
    let n = r.len();
    let (merged, not) = r.counts();
    let mut col = column![
        Space::new().height(Length::FillPortion(2)),
        icons::view(Glyph::Fursuit, 30.0, t::PHOTO_INK3),
        text(format!("All {} answered", util::plural(n as u64, "pair"))).size(t::T_HEAD).font(paint::SEMIBOLD).color(t::PHOTO_INK),
        text(format!("{} merged. {} won't be suggested again.", util::plural(merged as u64, "pair"), util::plural(not as u64, "pair"))).size(t::T_BODY).color(t::PHOTO_INK2),
    ]
    .spacing(t::S2)
    .align_x(Alignment::Center)
    .max_width(560);
    if judge_running(app, r) {
        col = col.push(text("Close pairs are still being compared. Those the judge thinks are one fursuit appear here as they come.").size(t::T_BODY).color(t::PHOTO_INK2));
    }
    col = col.push(Space::new().height(t::S1));
    col = col.push(kit::btn("Back to the fursuits", style::primary(), Some(m(Msg::Done))));
    if let Some(line) = judge_line(app, r) {
        col = col.push(container(line).max_width(640));
    }
    col = col.push(text("New pairs show on the Fursuits tab as groups change: after a merge, a scan, or new exports.").size(t::T_SMALL).color(t::PHOTO_INK3));
    col = col.push(Space::new().height(Length::FillPortion(4)));
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).into()
}

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let s = &app.subjects;
    let short = super::people::short;
    let tab = s.lists.tab.as_ref().map(|l| l.iter().map(|p| format!("{}+{}{}", short(&p.a), short(&p.b), if p.sure { "" } else { "?" })).collect::<Vec<_>>());
    let similar = s.lists.similar.as_ref().map(|(g, l)| (short(g), l.len()));
    let review = s.merges.as_ref().map(|r| {
        format!(
            "[list={} at={} answers={} judging={} run={:?} judged={} left_out_shown={} refreshing={}]",
            r.len(),
            r.at,
            r.answers.iter().map(|a| match a {
                Some(Answer::Merged) => 'm',
                Some(Answer::NotSame) => 'n',
                None => '.',
            }).collect::<String>(),
            r.judging,
            r.run.as_ref().map(|x| (x.offered, x.left_out.len(), x.done, x.total, x.beyond, x.stopped)),
            r.judged.len(),
            r.left_out_shown,
            r.refreshing
        )
    });
    format!("merges=[tab={tab:?} similar={similar:?} review={}]", review.unwrap_or_else(|| "none".into()))
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

    fn pair(a: u64, b: u64) -> GroupPair {
        GroupPair { a: GroupId::Cluster(a), b: GroupId::Cluster(b), distance: 0.6, a_heads: Arc::new(vec![face(a as u8)]), b_heads: Arc::new(vec![face(b as u8)]), sure: true, verdict: None }
    }

    /// The judge's pairs join the end, each once; after a merge the
    /// answered pairs stay where they were and the fresh list follows,
    /// without pairs listed already.
    #[test]
    fn pairs_join_the_end_and_a_refresh_keeps_the_answers() {
        let mut r = MergeReview::new(1, true);
        r.append(&[pair(1, 2), pair(3, 4)]);
        r.loaded = true;
        assert_eq!(r.append(&[pair(2, 1), pair(5, 6)]).len(), 1, "a pair either way round is listed once");
        assert_eq!(r.len(), 3);
        r.answers[0] = Some(Answer::Merged);
        r.order.push(0);
        r.at = 0;
        // 1 and 2 merged: the pairs with group 2 went; group 1 pairs with 7.
        r.refreshed(&[pair(1, 7), pair(5, 6)]);
        let keys: Vec<(GroupId, GroupId)> = r.list.iter().map(GroupPair::key).collect();
        assert_eq!(keys, [pair(1, 2).key(), pair(1, 7).key(), pair(5, 6).key()]);
        assert_eq!(r.answers, [Some(Answer::Merged), None, None]);
        assert_eq!((r.at, r.order.clone()), (0, vec![0]), "the answered pair stays shown");
        assert_eq!(r.counts(), (1, 0));
        assert_eq!(r.next_open(0), 1);
    }

    /// The name rule of a merge from the review: the named one is kept; of
    /// two unnamed, the larger; two named ask.
    #[test]
    fn a_merge_keeps_the_named_group() {
        let g = |id: u64, name: Option<&str>, photos: usize| super::super::people::Group { id: GroupId::Cluster(id), kind: Kind::Fursuit, name: name.map(str::to_string), hidden: false, photos, days: 1, cover: face(id as u8), suggestions: 0 };
        assert_eq!(into_of(&g(1, None, 40), &g(2, Some("Suit"), 3)), Some(GroupId::Cluster(2)));
        assert_eq!(into_of(&g(1, Some("Suit"), 3), &g(2, None, 40)), Some(GroupId::Cluster(1)));
        assert_eq!(into_of(&g(1, None, 3), &g(2, None, 40)), Some(GroupId::Cluster(2)));
        assert_eq!(into_of(&g(1, Some("A"), 3), &g(2, Some("B"), 40)), None, "two names: the dialog asks");
    }
}
