//! Fursuits as the pages see them (decision 11): the head step's state and
//! costs (`crate::search::heads`, re-exported here), the head models' row
//! for the download dialog, and, behind `PP_SEARCH_FAKE`, a stand-in that
//! makes fursuits up.
//!
//! Groups, naming, the review, merging, splitting and hiding are the
//! subjects pages' own, on this kind's data; nothing here ever mixes
//! fursuits with people.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use chrono::NaiveDate;

pub use crate::search::heads::*;

use super::people::{Face, FaceRef, Group, GroupId, Kind, Shot, SubjectId, shots_of};
use crate::ml::Device;
use crate::ml::models::{self, ModelId};
use crate::model::library_index::LibraryIndex;
use crate::search::Status;
use crate::search::index::Key;
use crate::util;

/// The head step's models (the registry's *Fursuits* pair), named as the
/// pages say them ("OWLv2"; v2 dropped DINOv2).
pub fn models_name() -> String {
    let names: Vec<&str> = models::fursuit_models().map(|m| m.name.split(',').next().unwrap_or(m.name).trim()).collect();
    names.join(" and ")
}

/// The head models still to download: both, unless the status says they
/// are in place.  The dialog's sizes and its rows come from here, so no
/// row waits for a download that never comes.
pub fn missing_models(st: &Status) -> Vec<ModelId> {
    if st.heads.models_installed { Vec::new() } else { models::FURSUIT_MODELS.to_vec() }
}

/// The library's fursuit photos, for "for this library" costs: those the
/// index tags, or before anything is indexed, the study's share (three
/// shots in four).
pub fn library_photos(h: &HeadsStatus, shots: usize) -> usize {
    if h.tagged > 0 { h.tagged } else { shots * 3 / 4 }
}

/// Seconds to give `photos` photos' known heads the search model's
/// vectors (their previews read again), as the engine measures it.
pub fn vector_seconds(photos: usize, device: Device) -> f64 {
    crate::search::estimate::head_vector_seconds(photos, device)
}

/// Bytes read from the library to do so.
pub fn vector_bytes_read(photos: usize) -> u64 {
    crate::search::estimate::head_vector_bytes_read(photos)
}

/// "about 7 minutes with the graphics card".
pub fn describe_vector_cost(photos: usize, device: Device) -> String {
    let time = crate::search::estimate::describe_seconds(vector_seconds(photos, device));
    format!("{time} {} {}", if device == Device::Gpu { "with" } else { "on" }, device.phrase())
}

/// "about 1 second", "about 2 seconds": a close call's time (the general
/// `describe_seconds` says "under a minute" for anything this short).
pub fn call_seconds_words(secs: f64) -> String {
    let n = secs.round().max(1.0) as u64;
    format!("about {}", util::plural(n, "second"))
}

/// Seconds `choice` takes to go on the card, and a close call's (both
/// orders of the pair, as that judge asks), as the engine measures them.
pub fn judge_load_seconds(choice: crate::ml::judge::JudgeChoice) -> f64 {
    crate::search::estimate::judge_load_seconds(choice)
}

pub fn judge_call_seconds(choice: crate::ml::judge::JudgeChoice) -> f64 {
    crate::search::estimate::judge_seconds_per_pair(choice)
}

// ── the stand-in ────────────────────────────────────────────────────────────
//
// Made-up fursuits on the library's shots (the same shots and keys as the
// people's stand-in, so a photo of a person and a fursuit is one photo),
// drawn as grey cartoon heads (`fursuit_art`).  Heads are only on photos
// "tagged" as fursuits; suits come in look-alike pairs, so automatic groups
// mix a little, as the study found; named fursuits get their suggestions a
// few at a time, and *Find more* (strict) lets the next few through,
// look-alikes included.

/// Made-up suits (one per look in `fursuit_art::LOOKS`).
const SUITS: usize = 12;
/// Fursuit cluster ids start here, clear of the people's.
pub const ID_BASE: u64 = 1 << 40;
/// Suggestions a named fursuit gets at first, and each *Find more* adds.
const FIRST_BATCH: usize = 8;
const MORE_BATCH: usize = 6;

fn unit(x: &impl Hash) -> f64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    (h.finish() % 1_000_000) as f64 / 1_000_000.0
}

fn hash64(x: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    h.finish()
}

/// A few suits in many photos, then a tail.
fn weight(suit: usize) -> f64 {
    [22.0, 12.0, 8.0, 5.0, 3.5, 2.6, 2.0, 1.6, 0.9, 0.7, 0.5, 0.4][suit % SUITS]
}

/// The suit most often mistaken for `suit` (same ears, close greys).
pub fn lookalike(suit: usize) -> usize {
    match suit % SUITS {
        0 => 7,
        7 => 0,
        6 => 0,
        1 => 9,
        9 => 1,
        2 => 10,
        10 => 2,
        4 => 11,
        11 => 4,
        5 => 8,
        8 => 5,
        s => s,
    }
}

/// The press a shot belongs to, as a seed (a shot of its own if none).
fn press_seed(s: &Shot) -> u64 {
    s.burst.map_or_else(|| hash64(&s.key), |b| hash64(&(b, "press")))
}

/// The index tags this shot as showing fursuits (a press all or none).
fn tagged(s: &Shot) -> bool {
    unit(&(press_seed(s), "fursuit")) < 0.62
}

fn pick_suit(seed: u64, salt: &str) -> usize {
    let total: f64 = (0..SUITS).map(weight).sum();
    let mut x = unit(&(seed, salt)) * total;
    for s in 0..SUITS {
        x -= weight(s);
        if x <= 0.0 {
            return s;
        }
    }
    SUITS - 1
}

/// The heads on a tagged shot: its press's suit, and sometimes a second
/// suit beside it.  Boxes as fractions of the photo, a little different
/// each frame.
fn heads_of(s: &Shot) -> Vec<(usize, FaceRef)> {
    let seed = press_seed(s);
    let jit = |k: &str| (unit(&(s.key, k)) as f32 - 0.5) * 0.02;
    let main = pick_suit(seed, "suit");
    let w = 0.16 + 0.08 * unit(&(seed, "w")) as f32;
    let x = 0.28 + 0.3 * unit(&(seed, "x")) as f32 + jit("x");
    let mut out = vec![(main, FaceRef::new(s.key, [x, 0.16 + jit("y"), w, w * 1.6]))];
    if unit(&(seed, "two")) < 0.25 {
        let other = (main + 1 + (unit(&(seed, "other")) * (SUITS - 1) as f64) as usize) % SUITS;
        let w2 = w * 0.75;
        out.push((other, FaceRef::new(s.key, [(x + w + 0.08).min(0.95 - w2), 0.26 + jit("y2"), w2, w2 * 1.6])));
    }
    out
}

/// What became of a head when the stand-in grouped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    Own,
    /// Held back: a suggestion once the suit is named.
    Held,
    /// Grouped with its look-alike by mistake.
    Wrong(usize),
}

/// Suits the stand-in splits into two groups (two "events"), as the
/// tight real grouping splits a suit seen under different lights.
fn split_suit(suit: usize) -> bool {
    matches!(suit % SUITS, 0 | 2 | 3 | 5 | 7)
}

/// Which of its suit's groups a head falls in: 1 for about a third of the
/// presses of a split suit, else 0.
fn part_of(s: &Shot, suit: usize, grouping: Grouping) -> u8 {
    // Tighter splits more suits, more often; Looser joins the parts.
    let (share, any) = match grouping {
        Grouping::Tighter => (0.45, suit % 2 == 1),
        Grouping::Balanced => (0.35, false),
        Grouping::Looser => (0.0, false),
    };
    ((split_suit(suit) || any) && unit(&(press_seed(s), "part")) < share) as u8
}

fn placed(face: &FaceRef, suit: usize) -> Placed {
    let u = unit(&(face, "placed"));
    if suit <= 3 && u < 0.3 {
        Placed::Held
    } else if u > 0.9 && lookalike(suit) != suit {
        Placed::Wrong(lookalike(suit))
    } else {
        Placed::Own
    }
}

#[derive(Debug, Clone)]
struct FakeGroup {
    id: GroupId,
    name: Option<String>,
    hidden: bool,
    members: Vec<FaceRef>,
    confirmed: HashSet<FaceRef>,
    rejected: HashSet<FaceRef>,
    /// The suit most of it shows.
    suit: usize,
    /// Which of the suit's groups (a split suit has two).
    part: u8,
    /// *Find more* rounds asked so far.
    rounds: usize,
    /// The judge's verdicts on close calls, by the press's first head.
    judged: HashMap<FaceRef, bool>,
}

/// The stand-in's fursuits: groups of heads made up from the library,
/// changed by the pages' calls (the same calls as the people's).
#[derive(Debug, Clone, Default)]
pub struct FakeFursuits {
    shots: Vec<Shot>,
    by_key: HashMap<Key, usize>,
    /// The tagged shots, newest first, as indexes into `shots`.
    tagged: Vec<usize>,
    groups: Vec<FakeGroup>,
    next_id: u64,
    generation: u64,
    /// Tagged shots looked at for heads so far, newest first.
    pub scanned: usize,
    undo: Vec<(Vec<FakeGroup>, Vec<(Vec<FaceRef>, Vec<FaceRef>)>, String)>,
    /// "Not the same" answers, by the heads of both groups (as the real
    /// file keeps them).
    told_apart: Vec<(Vec<FaceRef>, Vec<FaceRef>)>,
    /// Name the two largest suits once heads are found (a library already
    /// worked on, for screenshots).
    pub name_first: bool,
    /// A close-call judge is chosen and in place.
    pub judge_on: bool,
    /// Nothing is sure: every candidate is a close call (`judge-nosure`).
    pub no_sure: bool,
    /// Preferences ▸ Fursuits ▸ *Grouping*: how often a suit is split.
    pub grouping: Grouping,
    /// Made-up groups of their own beside the suits (`fursuits-many n`),
    /// for a tab of many tiles: covers only, not backed by photos.
    pub many: usize,
}

const NAMES: [&str; 2] = ["Fursuit 1", "Fursuit 2"];

impl FakeFursuits {
    pub fn new(name_first: bool) -> FakeFursuits {
        FakeFursuits { name_first, ..FakeFursuits::default() }
    }

    pub fn on_library(&mut self, index: &LibraryIndex) {
        self.set_shots(shots_of(index));
    }

    fn set_shots(&mut self, shots: Vec<Shot>) {
        self.shots = shots;
        self.by_key = self.shots.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
        self.tagged = self.shots.iter().enumerate().filter(|(_, s)| tagged(s)).map(|(i, _)| i).collect();
        self.scanned = self.scanned.min(self.tagged.len());
    }

    /// Tagged shots among the newest `indexed` shots of the library.
    pub fn tagged_in(&self, indexed: usize) -> usize {
        self.tagged.iter().take_while(|&&i| i < indexed).count()
    }

    /// Heads were looked for in the newest `n` tagged shots: group afresh,
    /// keeping the user's names and answers.
    pub fn scan(&mut self, n: usize) {
        self.scanned = n.min(self.tagged.len());
        self.regroup();
    }

    pub fn forget_all(&mut self) {
        self.groups.clear();
        self.undo.clear();
        self.told_apart.clear();
        self.scanned = 0;
        self.generation += 1;
    }

    fn heads_scanned(&self) -> impl Iterator<Item = (usize, FaceRef)> + '_ {
        self.tagged[..self.scanned].iter().flat_map(|&i| heads_of(&self.shots[i]))
    }

    /// The scanned heads with the group part each falls in.
    fn heads_parted(&self) -> impl Iterator<Item = (usize, u8, FaceRef)> + '_ {
        let grouping = self.grouping;
        self.tagged[..self.scanned].iter().flat_map(move |&i| {
            let s = &self.shots[i];
            let heads = heads_of(s);
            // A crowded press now and then: the second suit's head lands in
            // the first's group, as two heads of one photo do (stage 3).
            let crowded = heads.len() > 1 && unit(&(press_seed(s), "crowd")) < 0.12;
            let main = heads[0].0;
            heads.into_iter().enumerate().map(move |(k, (suit, f))| {
                let grouped = if crowded && k > 0 { main } else { suit };
                (grouped, part_of(s, grouped, grouping), f)
            })
        })
    }

    /// The grouping choice changed: group afresh (names and answers stay).
    pub fn regroup_now(&mut self) {
        // The automatic groups start over; the named and those the user
        // made keep their heads.
        self.groups.retain(|g| g.name.is_some() || is_made(&g.id));
        self.regroup();
    }

    fn regroup(&mut self) {
        let mut members: Vec<Vec<FaceRef>> = vec![Vec::new(); SUITS * 2];
        for (suit, part, f) in self.heads_parted() {
            match placed(&f, suit) {
                Placed::Own => members[suit * 2 + part as usize].push(f),
                Placed::Wrong(q) => members[q * 2].push(f),
                Placed::Held => {}
            }
        }
        let old = std::mem::take(&mut self.groups);
        // A head already in a group (merged, moved, answered) stays there.
        let claimed: HashSet<FaceRef> = old.iter().flat_map(|g| g.members.iter().copied()).collect();
        for (slot, heads) in members.into_iter().enumerate() {
            let (suit, part) = (slot / 2, (slot % 2) as u8);
            let heads: Vec<FaceRef> = heads.into_iter().filter(|f| !claimed.contains(f)).collect();
            if let Some(mut g) = old.iter().find(|g| g.suit == suit && g.part == part && !is_made(&g.id)).cloned() {
                for f in heads {
                    if !g.rejected.contains(&f) {
                        g.members.push(f);
                    }
                }
                self.groups.push(g);
                continue;
            }
            if heads.is_empty() {
                continue;
            }
            let named = self.name_first && suit < NAMES.len() && part == 0;
            let id = if named { self.fresh_subject() } else { GroupId::Cluster(ID_BASE + suit as u64 + 100 * part as u64 + 1000 * (self.generation + 1)) };
            let confirmed = if named { heads.iter().copied().collect() } else { HashSet::new() };
            self.groups.push(FakeGroup { id, name: named.then(|| NAMES[suit].to_string()), hidden: false, members: heads, confirmed, rejected: HashSet::new(), suit, part, rounds: 0, judged: HashMap::new() });
        }
        // Groups the user made by moving heads out stay.
        for g in old {
            if is_made(&g.id) && !self.groups.iter().any(|x| x.id == g.id) {
                self.groups.push(g);
            }
        }
        self.generation += 1;
    }

    fn fresh_subject(&mut self) -> GroupId {
        self.next_id += 1;
        GroupId::Subject(SubjectId(format!("fursuit{:06}", self.next_id)))
    }

    /// Is this group one of the fursuits'?
    pub fn owns(&self, id: &GroupId) -> bool {
        self.groups.iter().any(|g| &g.id == id)
    }

    fn face_of(&self, f: FaceRef, confirmed: bool) -> Option<Face> {
        let s = &self.shots[*self.by_key.get(&f.key)?];
        let clarity = (0.3 + 0.7 * unit(&(f, "clear"))) as f32;
        Some(Face { face: f, jpg: s.jpg.clone(), raw: s.raw.clone(), day: s.day, capture: s.capture, burst: s.burst, clarity, confirmed })
    }

    fn group(&self, id: &GroupId) -> Option<&FakeGroup> {
        self.groups.iter().find(|g| &g.id == id)
    }

    fn group_mut(&mut self, id: &GroupId) -> Option<&mut FakeGroup> {
        self.groups.iter_mut().find(|g| &g.id == id)
    }

    /// Every fursuit group, as the Fursuits tab lists them.
    pub fn groups(&self) -> Vec<Group> {
        let mut out = Vec::new();
        for g in &self.groups {
            let faces: Vec<Face> = g.members.iter().filter_map(|&f| self.face_of(f, g.confirmed.contains(&f))).collect();
            let Some(cover) = faces.iter().max_by(|a, b| a.clarity.total_cmp(&b.clarity)).cloned() else { continue };
            let photos: HashSet<Key> = faces.iter().map(|f| f.face.key).collect();
            let days = crate::model::subjects::count_days(&faces);
            let suggestions = if g.name.is_some() { self.suggestions_of(g).len() } else { 0 };
            out.push(Group { id: g.id.clone(), kind: Kind::Fursuit, name: g.name.clone(), hidden: g.hidden, photos: photos.len(), bursts: crate::model::subjects::count_bursts(&faces), days, cover, suggestions });
        }
        out.extend((0..self.many).map(many_group));
        out
    }

    /// Heads found so far.
    pub fn found(&self) -> usize {
        self.heads_scanned().count()
    }

    pub fn named(&self) -> usize {
        self.groups.iter().filter(|g| g.name.is_some()).count()
    }

    /// A group's heads, clearest first.
    pub fn faces(&self, id: &GroupId) -> Vec<Face> {
        let Some(g) = self.group(id) else { return Vec::new() };
        let mut v: Vec<Face> = g.members.iter().filter_map(|&f| self.face_of(f, g.confirmed.contains(&f))).collect();
        v.sort_by(|a, b| b.clarity.total_cmp(&a.clarity).then(a.capture.cmp(&b.capture)));
        v
    }

    /// The shot keys of a group's photos.
    pub fn members(&self, id: &GroupId) -> HashSet<Key> {
        self.group(id).map(|g| g.members.iter().map(|f| f.key).collect()).unwrap_or_default()
    }

    /// The heads that may show `g`'s suit, one entry per press, nearest
    /// first, with whether each is the suit's own (else a look-alike):
    /// its own held-back heads, then look-alikes; never a member or one
    /// answered No.
    fn candidates_of(&self, g: &FakeGroup) -> Vec<(bool, Vec<Face>)> {
        let have: HashSet<FaceRef> = g.members.iter().copied().collect();
        let mut by_press: BTreeMap<(u64, u64), (bool, Vec<Face>)> = BTreeMap::new();
        for &i in &self.tagged[..self.scanned] {
            let s = &self.shots[i];
            for (suit, f) in heads_of(s) {
                if have.contains(&f) || g.rejected.contains(&f) {
                    continue;
                }
                let own = suit == g.suit && placed(&f, suit) == Placed::Held;
                // With a judge, more look-alikes reach the close calls.
                let alike = suit == lookalike(g.suit) && suit != g.suit && unit(&(f, g.suit, "alike")) < if self.judge_on { 0.4 } else { 0.12 };
                if !(own || alike) {
                    continue;
                }
                let press = s.burst.unwrap_or_else(|| hash64(&s.key));
                // Look-alikes rank after the suit's own.
                // (With a judge, the close calls mix, nearest first.)
                let rank = (unit(&(press, g.suit, "rank")) * 1e6) as u64 + if alike && !self.judge_on { 1_000_000 } else { 0 };
                if let Some(face) = self.face_of(f, false) {
                    let e = by_press.entry((rank, press)).or_insert((own, Vec::new()));
                    e.1.push(face);
                }
            }
        }
        by_press
            .into_values()
            .map(|(own, mut v)| {
                v.sort_by(|a, b| b.clarity.total_cmp(&a.clarity));
                (own, v)
            })
            .collect()
    }

    /// What the review offers `g`, one entry per press, strictest first.
    /// Without the judge: its own held-back heads, then (from the first
    /// *Find more*) look-alikes, only as many as the rounds asked let
    /// through.  With the judge: the sure ones (its own, as many as the
    /// rounds let through), then the close calls it judged the same suit.
    fn suggestions_of(&self, g: &FakeGroup) -> Vec<Vec<Face>> {
        let limit = FIRST_BATCH + MORE_BATCH * g.rounds;
        let all = self.candidates_of(g);
        if !self.judge_on {
            return all.into_iter().filter(|(own, _)| *own || g.rounds > 0).map(|(_, v)| v).take(limit).collect();
        }
        let (sure, band) = self.split(g, all);
        let yes = band.into_iter().filter(|e| g.judged.get(&e[0].face) == Some(&true));
        sure.into_iter().chain(yes).collect()
    }

    /// The sure suggestions and the close calls (the judge's band).
    fn split(&self, g: &FakeGroup, all: Vec<(bool, Vec<Face>)>) -> (Vec<Vec<Face>>, Vec<Vec<Face>>) {
        let limit = if self.no_sure { 0 } else { FIRST_BATCH + MORE_BATCH * g.rounds };
        let (mut sure, mut band) = (Vec::new(), Vec::new());
        for (own, v) in all {
            if own && sure.len() < limit {
                sure.push(v);
            } else {
                band.push(v);
            }
        }
        (sure, band)
    }

    /// The judge's work for a named fursuit: the close calls it judged
    /// different (left out), and those not judged yet, nearest first.
    pub fn close_calls(&self, id: &GroupId) -> (Vec<Vec<Face>>, Vec<Vec<Face>>) {
        let Some(g) = self.group(id).filter(|g| g.name.is_some()) else { return (Vec::new(), Vec::new()) };
        let (_, band) = self.split(g, self.candidates_of(g));
        let left_out = band.iter().filter(|e| g.judged.get(&e[0].face) == Some(&false)).cloned().collect();
        let todo = band.into_iter().filter(|e| !g.judged.contains_key(&e[0].face)).collect();
        (left_out, todo)
    }

    /// The stand-in judge's verdict on a close call: the suit's own heads
    /// are the same fursuit, look-alikes not, with a few mistakes.
    pub fn judge(&mut self, id: &GroupId, entry: &[Face]) -> bool {
        let Some(first) = entry.first().map(|f| f.face) else { return false };
        let own = self.group(id).is_some_and(|g| self.look_of(&first) == Some(g.suit));
        let wrong = unit(&(first, "judge-wrong")) < 0.06;
        let yes = own != wrong;
        if let Some(g) = self.group_mut(id) {
            g.judged.insert(first, yes);
        }
        self.generation += 1;
        yes
    }

    /// What the review offers: a named fursuit's suggestions, or an
    /// unnamed group's own heads to check, one entry per press.
    pub fn suggestions(&self, id: &GroupId) -> Vec<Vec<Face>> {
        let Some(g) = self.group(id) else { return Vec::new() };
        if g.name.is_some() {
            return self.suggestions_of(g);
        }
        let mut by_press: BTreeMap<u64, Vec<Face>> = BTreeMap::new();
        for f in self.faces(id) {
            by_press.entry(f.burst.unwrap_or_else(|| hash64(&f.face.key))).or_default().push(f);
        }
        let mut v: Vec<Vec<Face>> = by_press.into_values().collect();
        // "Check this group": the least like the rest first (stage 3).
        v.sort_by(|a, b| self.far_of(g, &b[0].face).total_cmp(&self.far_of(g, &a[0].face)));
        v
    }

    /// The stand-in's distance of a head from its group's mean: the
    /// group's own suit near (0.25 to 0.45), anything else far (0.7 to 0.9).
    fn far_of(&self, g: &FakeGroup, f: &FaceRef) -> f32 {
        let u = unit(&(f, "far")) as f32;
        if self.look_of(f) == Some(g.suit) { 0.25 + 0.2 * u } else { 0.7 + 0.2 * u }
    }

    /// The group holding a head, if any.
    pub fn group_of(&self, f: &FaceRef) -> Option<GroupId> {
        self.groups.iter().find(|g| g.members.contains(f)).map(|g| g.id.clone())
    }

    /// The groups holding two heads of one photo (stage 3's flag).
    pub fn seen_together(&self) -> HashSet<GroupId> {
        self.groups.iter().filter(|g| !crate::search::splitting::photo_pairs(&g.members).is_empty()).map(|g| g.id.clone()).collect()
    }

    /// A group's split check, as the engine sends it: heads farthest from
    /// the rest first, heads seen in one photo, and a split proposed around
    /// the first such pair (each head with the seed of its own look).
    #[allow(clippy::type_complexity)]
    pub fn check(&self, id: &GroupId) -> Option<(Vec<(FaceRef, f32)>, Vec<(FaceRef, FaceRef)>, Option<(Vec<FaceRef>, Vec<FaceRef>)>)> {
        let g = self.group(id)?;
        let mut far: Vec<(FaceRef, f32)> = g.members.iter().map(|f| (*f, self.far_of(g, f))).collect();
        far.sort_by(|a, b| b.1.total_cmp(&a.1));
        let pairs = crate::search::splitting::photo_pairs(&g.members);
        let split = pairs.first().map(|&(a, b)| {
            let lb = self.look_of(&b);
            let (with_b, with_a): (Vec<FaceRef>, Vec<FaceRef>) = g.members.iter().partition(|f| **f == b || (**f != a && self.look_of(f) == lb && lb != self.look_of(&a)));
            (with_a, with_b)
        });
        Some((far, pairs, split))
    }

    /// *Find more*, strict: let the next few through.  How many new
    /// suggestions it found (0: nothing more is close enough).
    pub fn find_more(&mut self, id: &GroupId) -> usize {
        let Some(before) = self.group(id).map(|g| self.suggestions_of(g).len()) else { return 0 };
        if let Some(g) = self.group_mut(id) {
            g.rounds += 1;
        }
        let after = self.group(id).map_or(0, |g| self.suggestions_of(g).len());
        if after == before {
            // Nothing new: the round isn't spent.
            if let Some(g) = self.group_mut(id) {
                g.rounds -= 1;
            }
        }
        after.saturating_sub(before)
    }

    fn remember(&mut self, what: impl Into<String>) {
        self.undo.push((self.groups.clone(), self.told_apart.clone(), what.into()));
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
        self.generation += 1;
    }

    /// Edits that can be taken back.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn undo(&mut self) -> Option<String> {
        let (groups, told, what) = self.undo.pop()?;
        self.groups = groups;
        self.told_apart = told;
        self.generation += 1;
        Some(what)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn label(&self, id: &GroupId) -> String {
        self.group(id).and_then(|g| g.name.clone()).unwrap_or_else(|| "an unnamed group".into())
    }

    pub fn name_group(&mut self, id: &GroupId, name: &str) -> Result<GroupId, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("A name can't be empty.".into());
        }
        if self.groups.iter().any(|g| &g.id != id && g.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&name))) {
            return Err(format!("There's already a fursuit called {name}."));
        }
        let Some(was) = self.group(id).map(|g| g.name.clone()) else { return Err("That group changed: look again.".into()) };
        self.remember(match was {
            Some(w) => format!("Renamed {w} to {name}"),
            None => format!("Named a group {name}"),
        });
        let new_id = match id {
            GroupId::Subject(_) => id.clone(),
            GroupId::Cluster(_) => self.fresh_subject(),
        };
        let g = self.group_mut(id).expect("checked");
        g.id = new_id.clone();
        g.name = Some(name);
        let all: Vec<FaceRef> = g.members.clone();
        g.confirmed.extend(all);
        Ok(new_id)
    }

    /// Merge `others` into `into`: fursuits only.
    pub fn merge(&mut self, into: &GroupId, others: &[GroupId], kinds: impl Fn(&GroupId) -> Option<Kind>) -> Result<(), String> {
        if kinds(into) != Some(Kind::Fursuit) || others.iter().any(|o| kinds(o) != Some(Kind::Fursuit)) {
            return Err("Only groups of one kind can be merged.".into());
        }
        if self.group(into).is_none() {
            return Err("That group changed: look again.".into());
        }
        let n = others.iter().filter(|o| *o != into).count();
        let label = self.label(into);
        self.remember(format!("Merged {} into {label}", util::plural(n as u64 + 1, "group")));
        let mut moved: Vec<FakeGroup> = Vec::new();
        self.groups.retain(|g| {
            if &g.id != into && others.contains(&g.id) {
                moved.push(g.clone());
                false
            } else {
                true
            }
        });
        let g = self.group_mut(into).expect("checked");
        for m in moved {
            for f in m.members {
                if !g.members.contains(&f) {
                    g.members.push(f);
                }
            }
            g.confirmed.extend(m.confirmed);
            g.rejected.extend(m.rejected);
            if g.name.is_none() {
                g.name = m.name;
            }
        }
        Ok(())
    }

    pub fn set_hidden(&mut self, id: &GroupId, hidden: bool) {
        if self.group(id).is_none() {
            return;
        }
        let label = self.label(id);
        self.remember(if hidden { format!("Hid {label}") } else { format!("Showed {label} again") });
        if let Some(g) = self.group_mut(id) {
            g.hidden = hidden;
        }
    }

    pub fn forget(&mut self, id: &GroupId) {
        let label = self.label(id);
        self.remember(format!("Forgot {label}"));
        let generation = self.generation;
        if let Some(g) = self.group_mut(id) {
            g.name = None;
            g.confirmed.clear();
            g.rejected.clear();
            g.hidden = false;
            g.rounds = 0;
            g.judged.clear();
            g.id = GroupId::Cluster(ID_BASE + g.suit as u64 + 100 * g.part as u64 + 1000 * (generation + 1));
        }
    }

    /// "Not fursuit heads": the groups' heads (all, or the picked ones)
    /// out of them and never back, as the engine's removal; one undo step.
    pub fn remove_boxes(&mut self, targets: &[(GroupId, Option<Vec<FaceRef>>)]) -> Result<String, String> {
        if targets.iter().any(|(id, _)| self.group(id).is_none()) {
            return Err("That group changed: look again.".into());
        }
        let mut n = 0;
        let mut names = Vec::new();
        for (id, picked) in targets {
            let g = self.group(id).expect("checked");
            n += match picked {
                Some(p) => g.members.iter().filter(|f| p.contains(f)).count(),
                None => g.members.len(),
            };
            names.extend(g.name.clone());
        }
        if n == 0 {
            return Err("nothing to remove".into());
        }
        if targets.len() != 1 {
            names.clear();
        }
        let count = if n == 1 { "a fursuit head".to_string() } else { format!("{n} fursuit heads") };
        let what = match names.as_slice() {
            [one] => format!("Removed {count} from {one}"),
            _ => format!("Removed {count}"),
        };
        self.remember(what.clone());
        for (id, picked) in targets {
            let g = self.group_mut(id).expect("checked");
            let gone: Vec<FaceRef> = g.members.iter().copied().filter(|f| picked.as_ref().is_none_or(|p| p.contains(f))).collect();
            g.members.retain(|f| !gone.contains(f));
            g.confirmed.retain(|f| !gone.contains(f));
            g.rejected.extend(gone);
            if g.members.is_empty() {
                g.name = None;
            }
        }
        Ok(what)
    }

    /// Move heads out of `from`: into `to` (a fursuit), or a new group.
    pub fn move_faces(&mut self, from: &GroupId, faces: &[FaceRef], to: Option<&GroupId>) -> Result<(), String> {
        if self.group(from).is_none() || to.is_some_and(|t| self.group(t).is_none()) {
            return Err("That group changed: look again.".into());
        }
        let ids: HashSet<FaceRef> = faces.iter().copied().collect();
        let what = match to {
            Some(t) => format!("Moved {} to {}", util::plural(ids.len() as u64, "head"), self.label(t)),
            None => format!("Moved {} to a new group", util::plural(ids.len() as u64, "head")),
        };
        self.remember(what);
        let suit = self.group(from).map_or(0, |g| g.suit);
        if let Some(g) = self.group_mut(from) {
            g.members.retain(|f| !ids.contains(f));
            g.confirmed.retain(|k| !ids.contains(k));
        }
        match to {
            Some(t) => {
                let g = self.group_mut(t).expect("checked");
                for f in faces {
                    if !g.members.contains(f) {
                        g.members.push(*f);
                    }
                    g.rejected.remove(f);
                    g.confirmed.insert(*f);
                }
            }
            None => {
                self.next_id += 1;
                let id = GroupId::Cluster(made_id(self.next_id));
                let s = faces.first().and_then(|f| self.look_of(f)).unwrap_or(suit);
                self.groups.push(FakeGroup { id, name: None, hidden: false, members: faces.to_vec(), confirmed: HashSet::new(), rejected: HashSet::new(), suit: s, part: 0, rounds: 0, judged: HashMap::new() });
            }
        }
        Ok(())
    }

    /// Yes (add the heads), No (not this fursuit: out, never suggested
    /// again) or None (take the answer back).
    pub fn answer(&mut self, id: &GroupId, faces: &[FaceRef], yes: Option<bool>) {
        if self.group(id).is_none() {
            return;
        }
        let label = self.label(id);
        let n = faces.len() as u64;
        self.remember(match yes {
            Some(true) => format!("Added {} to {label}", util::plural(n, "photo")),
            Some(false) => format!("Marked {} as not {label}", util::plural(n, "photo")),
            None => format!("Took back an answer for {label}"),
        });
        let Some(g) = self.group_mut(id) else { return };
        for f in faces {
            match yes {
                Some(true) => {
                    if !g.members.contains(f) {
                        g.members.push(*f);
                    }
                    g.confirmed.insert(*f);
                    g.rejected.remove(f);
                }
                Some(false) => {
                    g.members.retain(|x| x != f);
                    g.confirmed.remove(f);
                    g.rejected.insert(*f);
                }
                None => {
                    if !g.rejected.remove(f) {
                        g.members.retain(|x| x != f);
                        g.confirmed.remove(f);
                    }
                }
            }
        }
    }

    // ── merge suggestions (stage 2) ──

    /// The stand-in's distance between two groups, in the whitened scale:
    /// the two groups of a split suit (or a suit and a group of its heads
    /// moved out) lie 0.60–0.69 apart, under the suggest cut, a few under
    /// sure-yes; a suit and its look-alike 0.70–0.78 (the judge's band);
    /// anything else too far to ask about.
    fn pair_distance(&self, a: &FakeGroup, b: &FakeGroup) -> Option<f32> {
        let seed = (a.suit.min(b.suit), a.suit.max(b.suit), a.part.min(b.part), a.part.max(b.part));
        let u = unit(&(seed, "pair")) as f32;
        if a.suit == b.suit {
            Some(0.60 + 0.09 * u)
        } else if lookalike(a.suit) == b.suit {
            Some(0.70 + 0.08 * u)
        } else {
            None
        }
    }

    /// Pairs of groups that may be one suit, nearest first, as the engine
    /// offers them: with `judge`, those under sure-yes (offered at once)
    /// and the close ones up to sure-no (for the judge); without, those
    /// under the suggest cut.  Small unnamed groups and hidden ones are
    /// left out; pairs the user said are different never come back.
    pub fn merge_pairs(&self, min_photos: usize, focus: Option<&GroupId>, judge: bool) -> (Vec<crate::search::merging::GroupPair>, Vec<crate::search::merging::GroupPair>) {
        use crate::search::merging::GroupPair;
        let shown: Vec<&FakeGroup> = self.groups.iter().filter(|g| !g.hidden && (g.name.is_some() || g.members.iter().map(|f| f.key).collect::<HashSet<_>>().len() >= min_photos)).collect();
        let mut near: Vec<(f32, usize, usize)> = Vec::new();
        for i in 0..shown.len() {
            for j in i + 1..shown.len() {
                let (a, b) = (shown[i], shown[j]);
                if focus.is_some_and(|f| *f != a.id && *f != b.id) {
                    continue;
                }
                let Some(d) = self.pair_distance(a, b) else { continue };
                let cut = if judge { crate::search::judging::SURE_NO } else { crate::search::heads::CUTOFFS.suggest };
                if d > cut || self.said_apart(&a.members, &b.members) {
                    continue;
                }
                near.push((d, i, j));
            }
        }
        near.sort_by(|x, y| x.0.total_cmp(&y.0));
        let heads = |g: &FakeGroup| std::sync::Arc::new(self.faces(&g.id).into_iter().take(4).collect::<Vec<_>>());
        let (mut sure, mut close) = (Vec::new(), Vec::new());
        for (d, i, j) in near {
            let is_sure = !judge || d < crate::search::judging::SURE_YES;
            let p = GroupPair { a: shown[i].id.clone(), b: shown[j].id.clone(), distance: d, a_heads: heads(shown[i]), b_heads: heads(shown[j]), sure: is_sure, verdict: None };
            if is_sure { sure.push(p) } else { close.push(p) }
        }
        (sure, close)
    }

    fn said_apart(&self, a: &[FaceRef], b: &[FaceRef]) -> bool {
        self.told_apart.iter().any(|(x, y)| (a.iter().any(|f| x.contains(f)) && b.iter().any(|f| y.contains(f))) || (a.iter().any(|f| y.contains(f)) && b.iter().any(|f| x.contains(f))))
    }

    /// "Not the same": kept by both groups' heads; one undo step.
    pub fn not_same(&mut self, a: &GroupId, b: &GroupId) -> Result<(), String> {
        let (Some(x), Some(y)) = (self.group(a).map(|g| g.members.clone()), self.group(b).map(|g| g.members.clone())) else { return Err("That group changed: look again.".into()) };
        if a == b {
            return Err("The same group twice.".into());
        }
        self.remember("Said two groups are not the same");
        self.told_apart.push((x, y));
        Ok(())
    }

    /// The stand-in judge comparing two groups on three head pairs: the
    /// same suit says yes (now and then one pair says no), a look-alike
    /// no; now and then it is wrong.
    pub fn judge_pair(&self, p: &crate::search::merging::GroupPair) -> crate::search::merging::GroupVerdict {
        let same = match (self.group(&p.a), self.group(&p.b)) {
            (Some(a), Some(b)) => a.suit == b.suit,
            _ => false,
        };
        let u = unit(&(p.a_heads.first().map(|f| f.face), p.b_heads.first().map(|f| f.face), "judge-pair"));
        let same = same != (u < 0.06);
        let one_off = (u * 10.0).fract() < 0.3;
        match (same, one_off) {
            (true, false) => crate::search::merging::GroupVerdict { yes: 3, no: 0, unjudged: 0 },
            (true, true) => crate::search::merging::GroupVerdict { yes: 2, no: 1, unjudged: 0 },
            (false, false) => crate::search::merging::GroupVerdict { yes: 0, no: 3, unjudged: 0 },
            (false, true) => crate::search::merging::GroupVerdict { yes: 1, no: 1, unjudged: 1 },
        }
    }

    /// The suit a detection shows, if it is one of the stand-in's heads
    /// (for its grey drawing); `None` for anything else (a face).
    pub fn look_of(&self, face: &FaceRef) -> Option<usize> {
        if let Some(i) = many_index(&face.key).filter(|&i| i < self.many) {
            return Some(i % SUITS);
        }
        let s = &self.shots[*self.by_key.get(&face.key)?];
        if !tagged(s) {
            return None;
        }
        heads_of(s).into_iter().find(|(_, f)| f == face).map(|(suit, _)| suit)
    }

    /// The heads the stand-in found on a shot (once it was scanned), each
    /// with its group and that group's name, if it is in one.
    pub fn found_on(&self, key: &Key) -> Vec<(FaceRef, Option<(GroupId, Option<String>)>)> {
        let Some(&i) = self.by_key.get(key) else { return Vec::new() };
        if !self.tagged[..self.scanned].contains(&i) {
            return Vec::new();
        }
        heads_of(&self.shots[i])
            .into_iter()
            .map(|(_, f)| (f, self.groups.iter().find(|g| g.members.contains(&f)).map(|g| (g.id.clone(), g.name.clone()))))
            .collect()
    }
}

/// The made-up groups' keys: a marker, then the number.
const MANY_MARK: [u8; 8] = *b"manysuit";

fn many_index(key: &Key) -> Option<usize> {
    (key.0[..8] == MANY_MARK).then(|| u64::from_le_bytes(key.0[8..].try_into().expect("8 bytes")) as usize)
}

/// One of `fursuits-many`'s groups: a cover on a made-up file, a size
/// above the smallest shown, falling with its number.
fn many_group(i: usize) -> Group {
    let mut k = [0u8; 16];
    k[..8].copy_from_slice(&MANY_MARK);
    k[8..].copy_from_slice(&(i as u64).to_le_bytes());
    let day = NaiveDate::from_ymd_opt(2026, 7, 24).expect("a date");
    let face = FaceRef::new(Key(k), [0.3, 0.16, 0.2, 0.32]);
    let cover = Face {
        face,
        jpg: Some(format!("/mnt/nas/library/2026/07/24/suit-{i:04}.jpg").into()),
        raw: None,
        day,
        capture: day.and_hms_opt(12, 0, 0).expect("a time"),
        burst: None,
        clarity: 0.9,
        confirmed: false,
    };
    Group { id: GroupId::Cluster(ID_BASE + 2_000_000_000 + i as u64), kind: Kind::Fursuit, name: None, hidden: false, photos: 400usize.saturating_sub(i / 3).max(5), bursts: 400usize.saturating_sub(i / 3).max(5) / 3, days: 1, cover, suggestions: 0 }
}

/// Clusters the user made by moving heads out: their own id range.
fn made_id(n: u64) -> u64 {
    ID_BASE + 1_000_000_000 + n
}

fn is_made(id: &GroupId) -> bool {
    matches!(id, GroupId::Cluster(c) if *c >= made_id(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up shots: `days` days of `n` frames, presses of three, newest
    /// day first (as `shots_of` gives them).
    fn shots(days: u32, n: u32) -> Vec<Shot> {
        let mut out = Vec::new();
        for d in (0..days).rev() {
            let day = NaiveDate::from_ymd_opt(2024, 6, d + 1).unwrap();
            for i in 0..n {
                let mut k = [0u8; 16];
                k[..4].copy_from_slice(&d.to_le_bytes());
                k[4..8].copy_from_slice(&i.to_le_bytes());
                let capture = day.and_hms_opt(10, i / 3 % 60, (i % 3) * 2).unwrap();
                let jpg = Some(std::path::PathBuf::from(format!("/lib/{day}/{i:04}.JPG")));
                out.push(Shot { key: Key(k), jpg, raw: None, day, capture, burst: Some((d * 1000 + i / 3) as u64) });
            }
        }
        out
    }

    fn fursuits(named: bool) -> FakeFursuits {
        let mut f = FakeFursuits::new(named);
        f.set_shots(shots(6, 150));
        let n = f.tagged.len();
        f.scan(n);
        f
    }

    fn named_one(f: &FakeFursuits) -> GroupId {
        f.groups().into_iter().find(|g| g.name.as_deref() == Some("Fursuit 1")).unwrap().id
    }

    /// The head models are listed for download only while missing, and the
    /// pages name them from the registry.
    #[test]
    fn head_models_are_rows_only_while_missing() {
        let mut st = Status::default();
        assert_eq!(missing_models(&st), models::FURSUIT_MODELS.to_vec());
        assert!(models::bytes_for(&missing_models(&st)) > 600_000_000, "the detector is about 600 MB");
        st.heads.models_installed = true;
        assert!(missing_models(&st).is_empty(), "in place: no row waits for a download that never comes");
        assert_eq!(models_name(), "OWLv2");
    }

    #[test]
    fn heads_are_only_on_fursuit_photos_and_group_by_suit() {
        let f = fursuits(false);
        assert!(f.tagged.len() > 300 && f.tagged.len() < 750, "about 60 % tagged: {}", f.tagged.len());
        let groups = f.groups();
        assert!(groups.len() >= 8, "{}", groups.len());
        assert!(groups.iter().all(|g| g.kind == Kind::Fursuit && g.name.is_none()), "fursuits only, nothing named for the user");
        for g in &groups {
            for key in f.members(&g.id) {
                assert!(tagged(&f.shots[f.by_key[&key]]), "a head on an untagged photo");
            }
            assert!(f.faces(&g.id).iter().any(|x| x.face == g.cover.face), "the cover is one of its own heads");
        }
        // Ids sit clear of the people's.
        assert!(groups.iter().all(|g| matches!(g.id, GroupId::Cluster(c) if c >= ID_BASE)));
        // Some groups hold a look-alike's head: automatic groups mix.
        let mixed = f.groups.iter().filter(|g| g.members.iter().any(|h| f.look_of(h) != Some(g.suit))).count();
        assert!(mixed >= 1, "some automatic groups are mixed");
        // The frames of a press show the press's suit.
        let s = &f.shots[f.tagged[0]];
        let same: Vec<usize> = f.tagged.iter().map(|&i| &f.shots[i]).filter(|x| x.burst == s.burst).map(|x| heads_of(x)[0].0).collect();
        assert!(same.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn find_more_is_strict_a_few_at_a_time() {
        let mut f = fursuits(true);
        let one = named_one(&f);
        let first = f.suggestions(&one);
        assert!(!first.is_empty() && first.len() <= FIRST_BATCH, "{}", first.len());
        // One entry per press: one answer covers a burst.
        for e in &first {
            assert!(e.iter().all(|x| x.burst == e[0].burst));
        }
        assert!(first.iter().flatten().all(|x| f.look_of(&x.face) == Some(0)), "strict: no look-alikes at first");
        let more = f.find_more(&one);
        assert!(more > 0 && more <= MORE_BATCH, "{more}");
        let second = f.suggestions(&one);
        assert_eq!(second.len(), first.len() + more);
        let mut last = second.len();
        for _ in 0..40 {
            if f.find_more(&one) == 0 {
                break;
            }
            let now = f.suggestions(&one).len();
            assert!(now > last);
            last = now;
        }
        assert_eq!(f.find_more(&one), 0, "eventually nothing more is close enough");
    }

    /// The stand-in judge: close calls beyond the sure ones; a verdict of
    /// yes offers one, no leaves it out (counted, never offered); nothing
    /// is added to the fursuit by a verdict.
    #[test]
    fn the_stand_in_judge_offers_and_leaves_out_but_never_adds() {
        let mut f = fursuits(true);
        f.judge_on = true;
        let one = named_one(&f);
        let sure = f.suggestions(&one);
        let (left, todo) = f.close_calls(&one);
        assert!(left.is_empty() && todo.len() > 5, "{}", todo.len());
        let members = f.faces(&one).len();
        let (mut yes, mut no) = (0, 0);
        for e in &todo {
            if f.judge(&one, e) {
                yes += 1;
            } else {
                no += 1;
            }
        }
        assert!(yes > 0 && no > 0, "some of each: {yes} {no}");
        assert_eq!(f.faces(&one).len(), members, "a verdict adds nothing");
        assert_eq!(f.suggestions(&one).len(), sure.len() + yes, "offered after the sure ones");
        assert_eq!(&f.suggestions(&one)[..sure.len()], &sure[..]);
        let (left, todo) = f.close_calls(&one);
        assert_eq!((left.len(), todo.len()), (no, 0));
        assert!(left.iter().all(|e| !f.suggestions(&one).contains(e)), "left out is never offered");
        // An unnamed group's close calls are never judged.
        let unnamed = f.groups().into_iter().find(|g| g.name.is_none()).unwrap().id;
        assert_eq!(f.close_calls(&unnamed), (Vec::new(), Vec::new()));
    }

    #[test]
    fn answers_cover_a_press_and_no_is_never_offered_again() {
        let mut f = fursuits(true);
        let one = named_one(&f);
        let list = f.suggestions(&one);
        let entry: Vec<FaceRef> = list[0].iter().map(|x| x.face).collect();
        let before = f.faces(&one).len();
        f.answer(&one, &entry, Some(true));
        assert_eq!(f.faces(&one).len(), before + entry.len(), "Yes covers every frame of the press");
        assert!(!f.suggestions(&one).iter().any(|e| e[0].face == entry[0]));
        let what = f.undo().unwrap_or_default();
        assert!(what.starts_with("Added ") && what.ends_with(" to Fursuit 1"), "{what}");
        assert_eq!(f.faces(&one).len(), before, "undone");
        f.answer(&one, &entry, Some(false));
        assert!(!f.suggestions(&one).iter().any(|e| e[0].face == entry[0]), "No: not offered again");
        f.answer(&one, &entry, None);
        assert!(f.suggestions(&one).iter().any(|e| e[0].face == entry[0]), "taken back: offered again");
    }

    /// The stand-in's merge suggestions (stage 2): split suits come back
    /// as pairs, nearest first; with the judge, the close ones are compared
    /// and a look-alike is mostly left out; "not the same" keeps a pair out
    /// through a regroup, and undo brings it back.
    #[test]
    fn the_stand_in_suggests_split_suits_and_remembers_not_the_same() {
        let mut f = fursuits(false);
        let (pairs, close) = f.merge_pairs(5, None, false);
        assert!(close.is_empty(), "without the judge, distance alone");
        assert!(pairs.len() >= 3, "{}", pairs.len());
        assert!(pairs.windows(2).all(|w| w[0].distance <= w[1].distance), "nearest first");
        let suit = |id: &GroupId| f.group(id).map(|g| g.suit);
        assert!(pairs.iter().filter(|p| suit(&p.a) == suit(&p.b)).count() >= 3, "split suits are offered");
        assert!(pairs.iter().all(|p| f.owns(&p.a) && f.owns(&p.b)), "fursuit groups only");
        // With the judge: sure ones at once, the band compared.
        let (sure, close) = f.merge_pairs(5, None, true);
        assert!(!close.is_empty() && sure.iter().all(|p| p.distance < crate::search::judging::SURE_YES));
        let same = close.iter().filter(|p| f.judge_pair(p).same()).count();
        let alike = close.iter().filter(|p| suit(&p.a) != suit(&p.b)).count();
        assert!(alike > 0 && same < close.len(), "look-alikes are compared and mostly left out");
        // Not the same: kept out, through a regroup; undo brings it back.
        let p = pairs[0].clone();
        f.not_same(&p.a, &p.b).unwrap();
        assert!(!f.merge_pairs(5, None, false).0.iter().any(|x| x.key() == p.key()));
        let n = f.tagged.len();
        f.scan(n);
        assert!(!f.merge_pairs(5, None, false).0.iter().any(|x| x.key() == p.key()), "after a regroup too");
        assert_eq!(f.undo().as_deref(), Some("Said two groups are not the same"));
        assert!(f.merge_pairs(5, None, false).0.iter().any(|x| x.key() == p.key()));
        // A group's own pairs.
        assert!(f.merge_pairs(5, Some(&p.a), false).0.iter().all(|x| x.a == p.a || x.b == p.a));
    }

    /// The stand-in's split check (stage 3): crowded presses put a second
    /// suit's head in the first's group, flagged; the check puts the heads
    /// that aren't the group's suit first and proposes a split around the
    /// two; "Check this group" goes least alike first; nothing moves.
    #[test]
    fn the_stand_in_flags_two_heads_of_one_photo_and_orders_strays_first() {
        let f = fursuits(false);
        let flagged = f.seen_together();
        assert!(!flagged.is_empty(), "some groups hold two heads of one photo");
        let id = flagged.iter().next().unwrap().clone();
        let g = f.group(&id).unwrap().clone();
        let (far, pairs, split) = f.check(&id).unwrap();
        assert_eq!(far.len(), g.members.len());
        let strays = g.members.iter().filter(|h| f.look_of(h) != Some(g.suit)).count();
        assert!(strays > 0 && far.iter().take(strays).all(|(h, _)| f.look_of(h) != Some(g.suit)), "strays first");
        assert!(!pairs.is_empty());
        let (a, b) = split.unwrap();
        assert_eq!(a.len() + b.len(), g.members.len());
        let list = f.suggestions(&id);
        assert!(list.first().is_some_and(|e| f.look_of(&e[0].face) != Some(g.suit)), "the review's first is a stray");
        assert!(f.check(&GroupId::Cluster(7)).is_none(), "no other group");
    }

    #[test]
    fn merging_and_moving_never_cross_kinds() {
        let mut f = fursuits(false);
        let g = f.groups();
        let (a, b) = (g[0].id.clone(), g[1].id.clone());
        let person = GroupId::Cluster(7);
        let kinds = |id: &GroupId| if *id == person { Some(Kind::Person) } else { g.iter().find(|x| &x.id == id).map(|x| x.kind) };
        assert!(f.merge(&a, std::slice::from_ref(&person), kinds).is_err(), "a fursuit and a person are never merged");
        assert!(f.merge(&person, std::slice::from_ref(&a), kinds).is_err());
        f.merge(&a, std::slice::from_ref(&b), kinds).unwrap();
        assert!(!f.owns(&b) && f.owns(&a));
        assert!(f.move_faces(&a, &[], Some(&person)).is_err(), "moved only to a fursuit");
        let two: Vec<FaceRef> = f.faces(&a).iter().take(2).map(|x| x.face).collect();
        let n = f.groups().len();
        f.move_faces(&a, &two, None).unwrap();
        assert_eq!(f.groups().len(), n + 1);
        // A group made by moving heads out survives a regroup.
        let tagged = f.tagged.len();
        f.scan(tagged);
        assert_eq!(f.groups().len(), n + 1);
        // Naming: one name per fursuit, any case.
        let c = f.groups()[0].id.clone();
        let named = f.name_group(&c, "Patches").unwrap();
        assert!(f.faces(&named).iter().all(|x| x.confirmed));
        let d = f.groups().iter().find(|x| x.name.is_none()).unwrap().id.clone();
        assert!(f.name_group(&d, "patches").is_err());
    }
}
