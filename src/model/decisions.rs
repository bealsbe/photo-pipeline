//! The record of the user's corrections to people and fursuits: an
//! append-only JSON-lines file of every grouping decision (merges, "not
//! the same", splits and moves, dissolved groups, review answers, the
//! viewer's label edits, and undo), kept so that later research can learn from them
//! (re-tuning the cut-offs, training a small adapter).
//!
//! - **User data, not cache.**  `~/.local/share/photo-pipeline/
//!   decisions.jsonl`, beside `subjects.json`: *Clear* and *Rebuild* keep
//!   it; *Forget all* of a kind takes that kind's entries out; Preferences
//!   ▸ Storage deletes it on request.  Readable by the user alone (0600
//!   in a 0700 folder).  Nothing here is ever written into photos,
//!   sidecars or the repository.
//! - **Each entry stands alone** ([`Entry`]): the heads involved as
//!   content keys and boxes in thousandths (never group numbers, which
//!   last one regroup), how the decision was reached (by hand, a distance
//!   suggestion with its distance, or a judge's verdict), and the
//!   library's state at the time (head count, whitening, model, the
//!   cut-offs in force).  An earlier label set was lost because it held
//!   only ids into a temporary table: this one needs nothing else to be
//!   read.
//! - **Append only.**  Undo appends an `undo` entry naming the one it
//!   takes back; nothing is rewritten except by *Forget all* of a kind
//!   and *Delete the record*, which the user asks for.
//! - **Never on the UI thread.**  [`Recorder`] owns a thread of its own:
//!   each entry is appended there with one `write` and an `fsync`, in the
//!   order sent.  The engine builds entries on its thread; the stand-in
//!   (`PP_SEARCH_FAKE`) builds them from its made-up groups, in a file of
//!   its own so it never mixes with the real record.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::subjects::{FaceRef, Kind, private_dir, private_file};
use crate::search::index::Key;

/// The entries' format version (`v`).  Readers skip entries of a
/// version they don't know.
pub const SCHEMA: u32 = 1;
/// The record's file, beside `subjects.json`.
pub const FILE_NAME: &str = "decisions.jsonl";
/// The stand-in's record (`PP_SEARCH_FAKE`): its own file, so made-up
/// heads never join the real record.
pub const STAND_IN_FILE_NAME: &str = "decisions-stand-in.jsonl";
/// Heads kept per group of a merge, a "not the same" or a move: the
/// clearest, with the group's full count beside them.  16 × 16 = 256
/// labelled pairs per merge is plenty to learn from, and keeps a merge
/// near 2 KB.
pub const SIDE_HEADS: usize = 16;
/// Heads kept of the ones moved, answered or split off (they are what the
/// decision is about; moves of more are rare and counted in full).
pub const MOVED_HEADS: usize = 200;
/// Past this size Preferences says the record is large.  Nothing is
/// rotated or dropped: see the plan's "As built" section.
pub const LARGE_BYTES: u64 = 64 * 1024 * 1024;

/// The record's path for the app: beside the user's subjects file.
pub fn default_path() -> Option<PathBuf> {
    Some(super::subjects::default_path()?.with_file_name(FILE_NAME))
}

// ── the entries ───────────────────────────────────────────────────────────

/// A head: a detection by content key and box, written compactly as
/// `["<32 hex>", x, y, w, h]` with the box in thousandths of the oriented
/// preview (the identity `subjects.json` uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Head(pub FaceRef);

impl Serialize for Head {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let [x, y, w, h] = self.0.thousandths();
        (self.0.key.to_string(), x, y, w, h).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Head {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Head, D::Error> {
        let (hex, x, y, w, h): (String, u16, u16, u16, u16) = Deserialize::deserialize(d)?;
        let key = parse_key(&hex).ok_or_else(|| serde::de::Error::custom("a key is 32 hex digits"))?;
        Ok(Head(FaceRef::new(key, [x, y, w, h].map(|v| v as f32 / 1000.0))))
    }
}

fn parse_key(hex: &str) -> Option<Key> {
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(Key(out))
}

pub fn heads(faces: impl IntoIterator<Item = FaceRef>) -> Vec<Head> {
    faces.into_iter().map(Head).collect()
}

/// One group as a decision saw it: how many heads it had, its name and
/// subject id (the user's, from `subjects.json`) when it had them, its
/// distance to the first group of the entry when that could be measured
/// (average linkage, in the space and model of [`Lib`]), and its clearest
/// heads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Side {
    pub n: usize,
    /// Its photos, bursts (a photo on its own counting as one) and days,
    /// as the tab showed the group, when it was on the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photos: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bursts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f32>,
    pub heads: Vec<Head>,
}

impl Side {
    /// A group of `n` heads, `clearest` first (the first [`SIDE_HEADS`]
    /// are kept, each once).
    pub fn new(n: usize, name: Option<String>, sid: Option<String>, clearest: impl IntoIterator<Item = FaceRef>) -> Side {
        let mut seen = HashSet::new();
        let heads = clearest.into_iter().filter(|f| seen.insert(*f)).take(SIDE_HEADS).map(Head).collect();
        Side { n, name, sid, d: None, heads, ..Side::default() }
    }

    /// With the counts the tab showed for the group.
    pub fn counted(self, group: Option<&super::subjects::Group>) -> Side {
        match group {
            Some(g) => Side { photos: Some(g.photos), bursts: Some(g.bursts), days: Some(g.days), ..self },
            None => self,
        }
    }
}

/// What was decided.  Serialised flat into the entry, under `act`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum Act {
    /// Groups merged into the first (kept).  Every pair across two of
    /// them is labelled the same.
    Merge { groups: Vec<Side> },
    /// Two groups are not the same subject (a merge suggestion turned
    /// down, or × on *Similar groups*).  Every pair across is different.
    NotSame { groups: Vec<Side> },
    /// Heads split out of a group into a new one of their own: they and
    /// the rest of `from` are different.
    Split {
        from: Side,
        heads: Vec<Head>,
        moved: usize,
    },
    /// Heads moved out of `from` into `to`: the same as `to`, different
    /// from the rest of `from`.
    Move {
        from: Side,
        to: Side,
        heads: Vec<Head>,
        moved: usize,
    },
    /// A review answer (or "Not this person" on a group's *Faces*): the
    /// heads (one press, its frames) are, or are not, `group`.
    Answer { yes: bool, group: Side, heads: Vec<Head> },
    /// A name given: to a head in the viewer (`head`; `name` absent is
    /// *Unnamed*, out of `from`), or to a group (`group`).  A head named
    /// into an existing subject has that subject as `group`.
    Name {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<Head>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<Side>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Side>,
    },
    /// "It's a fursuit" / "It's a person": the box is of kind `to` from
    /// now on (the entry's `kind` is the one it left), named `name` there.
    KindChange {
        head: Head,
        to: Kind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A box the finder missed, drawn by the user.
    Draw {
        head: Head,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// "Not a face" / "Not a fursuit head".
    Remove { head: Head },
    /// "Not fursuit heads" / "Not faces" on whole groups or picked heads:
    /// `groups` as they were (each target once), the `heads` removed (at
    /// most [`MOVED_HEADS`]) and how many.
    RemoveMany { groups: Vec<Side>, heads: Vec<Head>, removed: usize },
    /// The group editor's *Apply* (one undo step): `groups` as they were
    /// (each group heads left or went to, once), and where heads went,
    /// one [`Out`] per group they left and where they went, so each reads
    /// as a `move` / `split` / `remove_many` does.
    Edit { groups: Vec<Side>, out: Vec<Out> },
    /// A group dissolved: not one subject, only lookalikes.  `group` as
    /// it was (its name and subject id when it had them: `named`, and the
    /// name went), its `heads` (at most [`MOVED_HEADS`]) and how many
    /// (`dissolved`), and `spread`, the heads' mean cosine distance to
    /// their mean in [`Lib`]'s space.  No pair is labelled: some of the
    /// heads may still be the same subject, only not all of them.
    Dissolve {
        group: Side,
        heads: Vec<Head>,
        dissolved: usize,
        named: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spread: Option<f32>,
    },
    /// The entry `undoes` was taken back (Ctrl+Z, Z in a review).  An undo
    /// can itself be undone, which puts the first back.  `of` is the act
    /// it took back, for reading.
    Undo { undoes: String, of: String },
}

impl Act {
    pub fn name(&self) -> &'static str {
        match self {
            Act::Merge { .. } => "merge",
            Act::NotSame { .. } => "not_same",
            Act::Split { .. } => "split",
            Act::Move { .. } => "move",
            Act::Answer { .. } => "answer",
            Act::Name { .. } => "name",
            Act::KindChange { .. } => "kind_change",
            Act::Draw { .. } => "draw",
            Act::Remove { .. } => "remove",
            Act::RemoveMany { .. } => "remove_many",
            Act::Edit { .. } => "edit",
            Act::Dissolve { .. } => "dissolve",
            Act::Undo { .. } => "undo",
        }
    }
}

/// Heads that went one way in an edit: from `groups[from]` (absent: in
/// no group) to `to`; the heads (at most [`MOVED_HEADS`]) and how many.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Out {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<usize>,
    pub to: To,
    pub heads: Vec<Head>,
    pub moved: usize,
}

/// Where heads went in an edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "snake_case")]
pub enum To {
    /// An existing group: `groups[group]`.
    Group { group: usize },
    /// A new group the edit made (`new`: its number among them).
    New {
        new: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// "Not fursuit heads" / "Not faces".
    Removed,
    /// "Take out of this group" / "Not this person": not the group they
    /// left (`from`), every pair against its heads labelled different.
    TakenOut,
}

/// Where in the app a decision was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum At {
    /// A tile dragged onto another.
    Drag,
    /// Groups picked (S, clicks) and merged (M).
    #[default]
    Pick,
    /// A group page's ⋯ ▸ *Merge into…*.
    Menu,
    /// A group's name field (naming, or a typed name that merges).
    Name,
    /// A fursuit's *Heads*: split off, moved, dragged to a group.
    Heads,
    /// Two groups side by side: heads moved between them.
    SideBySide,
    /// A person's *Faces*: *Move to…*, *Not this person*.
    Faces,
    /// The viewer's labels.
    Viewer,
    /// "Possibly the same fursuit": the pairs of groups.
    MergeReview,
    /// A fursuit page's *Similar groups*.
    Similar,
    /// The "Same person?" / "Same fursuit?" review.
    Review,
    /// The group editor (Edit group, and what opens it).
    Editor,
}

/// How a decision came to be offered, as the page knows it; the engine
/// adds what it measures ([`How`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Via {
    /// By hand: the user picked the groups or heads.
    Hand(At),
    /// A pair of groups the app offered: its average-linkage distance as
    /// shown, whether the distance alone offered it, and the judge's
    /// majority when it compared them.
    Pair { at: At, distance: f32, sure: bool, verdict: Option<crate::search::merging::GroupVerdict> },
    /// A suggestion in the "Same …?" review: offered by distance
    /// (`judge: None`), by the judge (`Some(true)`), or left out by the
    /// judge and shown on request (`Some(false)`); `check`: an unnamed
    /// group's own heads ("Check this group").
    Review { judge: Option<bool>, check: bool },
    /// The group editor's *Apply*: opened from a proposed split
    /// (`proposal`) or not, `edited` heads moved by hand in it.
    Editor { proposal: bool, edited: u32 },
    #[default]
    Unknown,
}

/// How the decision was reached, as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct How {
    /// `hand`, `distance`, `judge`, `proposal` (a split proposed around
    /// two heads of one photo) or `check` (an unnamed group's own heads).
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<At>,
    /// The distance that offered it (cosine distance in [`Lib`]'s space:
    /// average linkage for pairs of groups, nearest confirmed member for
    /// review answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f32>,
    /// A pair offered by the distance alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure: Option<bool>,
    /// The judge's model folder and its yes threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_cut: Option<f32>,
    /// The judge's P(yes) for a head pair (the mean of both orders).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f32>,
    /// A pair of groups: the judge's head pairs that said yes, no, or
    /// couldn't be looked at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unseen: Option<u8>,
    /// The judge offered it (`true`), or left it out and the user asked
    /// to see it (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered: Option<bool>,
    /// The head pair the distance and P(yes) are of: the answered frame
    /// and its nearest confirmed member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair: Option<[Head; 2]>,
    /// A split arranged in two columns before it was confirmed (a
    /// proposal, or picked heads): how many heads the user moved between
    /// the sides (0: as it was offered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<u32>,
}

impl How {
    pub fn hand(at: At) -> How {
        How { by: "hand".into(), at: Some(at), ..How::default() }
    }

    /// The page's account on its own, with the judge chosen (its model
    /// folder and yes threshold); the engine adds what it measures for a
    /// review answer (the nearest member, P(yes)).
    pub fn of(via: Via, judge: Option<(String, f32)>) -> How {
        let (name, cut) = judge.map_or((None, None), |(n, c)| (Some(n), Some(c)));
        match via {
            Via::Hand(at) => How::hand(at),
            Via::Unknown => How { by: "hand".into(), ..How::default() },
            Via::Editor { proposal: true, edited } => How { by: "proposal".into(), at: Some(At::Editor), edited: Some(edited), ..How::default() },
            Via::Editor { proposal: false, edited } => How { edited: Some(edited), ..How::hand(At::Editor) },
            Via::Pair { at, distance, sure, verdict: Some(v) } => {
                How { by: "judge".into(), at: Some(at), d: Some(round4(distance)), sure: Some(sure), judge: name, judge_cut: cut, yes: Some(v.yes), no: Some(v.no), unseen: Some(v.unjudged), ..How::default() }
            }
            Via::Pair { at, distance, sure, verdict: None } => How { by: "distance".into(), at: Some(at), d: Some(round4(distance)), sure: Some(sure), ..How::default() },
            Via::Review { judge: by_judge, check } => {
                let by = if check {
                    "check"
                } else if by_judge.is_some() {
                    "judge"
                } else {
                    "distance"
                };
                let judged = by_judge.is_some();
                How { by: by.into(), at: Some(At::Review), offered: by_judge, judge: name.filter(|_| judged), judge_cut: cut.filter(|_| judged), ..How::default() }
            }
        }
    }
}

/// The cut-offs in force (cosine distances in [`Lib`]'s space).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Cuts {
    /// The automatic groups' cut (for fursuits, the grouping choice's).
    pub group: f32,
    /// A head offered for a subject in the review, and *Find more*'s.
    pub suggest: f32,
    pub strict: f32,
    /// The review's judge band (fursuits): offered at once under
    /// `sure_yes`, asked of the judge up to `sure_no`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_yes: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_no: Option<f32>,
    /// Pairs of groups ("Possibly the same fursuit"), by the grouping
    /// choice: offered by distance under `merge_suggest`; with the judge,
    /// offered at once under `merge_sure` and compared up to `merge_far`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_suggest: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_sure: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_far: Option<f32>,
}

/// The library's state when the decision was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Lib {
    /// Heads (or faces) of this kind with vectors.
    pub heads: usize,
    /// The head vectors were whitened (fursuits; never for faces).
    pub white: bool,
    /// The vectors' model folder (`siglip2-b16-256`, `auraface-v1`, …).
    pub model: String,
    /// Preferences ▸ Fursuits ▸ *Grouping* in force (`tighter`,
    /// `balanced`, `looser`); fursuits only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<crate::search::heads::Grouping>,
    pub cuts: Cuts,
}

/// One line of the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u32,
    /// Unique within the record (16 hex digits); `undo` entries name it.
    pub id: String,
    /// When, in UTC (RFC 3339, milliseconds).
    pub t: String,
    /// The app's version.
    pub app: String,
    pub kind: Kind,
    #[serde(flatten)]
    pub act: Act,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<How>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<Lib>,
}

impl Entry {
    /// A new entry, stamped now with a fresh id.
    pub fn new(kind: Kind, act: Act, how: Option<How>, lib: Option<Lib>) -> Entry {
        Entry { v: SCHEMA, id: fresh_id(), t: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(), app: env!("CARGO_PKG_VERSION").to_string(), kind, act, how, lib }
    }

    /// Whether *Forget all* of `kind` takes it: its own kind, or a kind
    /// change into it.
    pub fn about(&self, kind: Kind) -> bool {
        self.kind == kind || matches!(&self.act, Act::KindChange { to, .. } if *to == kind)
    }

    /// The JSON line, newline included.
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// 16 hex digits from the clock, the process and a counter.
fn fresh_id() -> String {
    super::subjects::SubjectId::fresh().0[..16].to_string()
}

/// Round a distance or probability for the record (4 decimals: well
/// within the measurements' noise, and short).
pub fn round4(x: f32) -> f32 {
    (x * 10_000.0).round() / 10_000.0
}

// ── reading ───────────────────────────────────────────────────────────────

/// The entries of a record's text, and how many lines couldn't be read
/// (a partial last line after a crash, an entry of a newer version).
pub fn parse(text: &str) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Entry>(line) {
            Ok(e) if e.v <= SCHEMA => out.push(e),
            _ => bad += 1,
        }
    }
    (out, bad)
}

/// Read a record (none is empty).
pub fn read(path: &Path) -> io::Result<(Vec<Entry>, usize)> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), 0)),
        Err(e) => Err(e),
    }
}

/// The entries in force: every decision not taken back, in order (undo
/// entries themselves left out).  An undo of an undo puts the first back.
pub fn effective(entries: &[Entry]) -> Vec<&Entry> {
    let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut on: HashMap<&str, bool> = HashMap::new();
    fn set<'a>(id: &'a str, value: bool, by_id: &HashMap<&'a str, &'a Entry>, on: &mut HashMap<&'a str, bool>, depth: usize) {
        on.insert(id, value);
        if depth > 64 {
            return;
        }
        if let Some(Entry { act: Act::Undo { undoes, .. }, .. }) = by_id.get(id) {
            set(undoes.as_str(), !value, by_id, on, depth + 1);
        }
    }
    for e in entries {
        set(e.id.as_str(), true, &by_id, &mut on, 0);
    }
    entries.iter().filter(|e| !matches!(e.act, Act::Undo { .. }) && on.get(e.id.as_str()).copied().unwrap_or(true)).collect()
}

/// A labelled pair of heads rebuilt from the record: the same subject or
/// not, by which decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub kind: Kind,
    pub a: FaceRef,
    pub b: FaceRef,
    pub same: bool,
    /// The entry it comes from.
    pub id: String,
}

/// The labelled pairs a record holds, from the decisions in force: a
/// merge makes every pair across its groups the same; "not the same"
/// every pair across different; a move the moved heads the same as `to`
/// and different from the rest of `from` (a split: different from the
/// rest); a Yes the heads the same as the group, a No different; a name
/// in the viewer the head the same as the subject joined, *Unnamed*
/// different from the one left.  Later decisions win over earlier ones
/// for the same pair.
pub fn pairs(entries: &[Entry]) -> Vec<Pair> {
    let mut set = PairSet::default();
    put_pairs(entries, &mut set);
    set.into_pairs()
}

/// Labelled pairs, each pair of heads once: a later label for a pair
/// replaces an earlier one.
#[derive(Default)]
pub struct PairSet {
    out: Vec<Pair>,
    at: HashMap<(FaceRef, FaceRef), usize>,
}

impl PairSet {
    pub fn put(&mut self, kind: Kind, a: FaceRef, b: FaceRef, same: bool, id: &str) {
        if a == b {
            return;
        }
        let key = if (a.key.0, a.thousandths()) <= (b.key.0, b.thousandths()) { (a, b) } else { (b, a) };
        let p = Pair { kind, a: key.0, b: key.1, same, id: id.to_string() };
        match self.at.get(&key) {
            Some(&i) => self.out[i] = p,
            None => {
                self.at.insert(key, self.out.len());
                self.out.push(p);
            }
        }
    }

    pub fn into_pairs(self) -> Vec<Pair> {
        self.out
    }
}

/// The record's pairs ([`pairs`]) into `set`.
pub fn put_pairs(entries: &[Entry], set: &mut PairSet) {
    let cross = |a: &[Head], b: &[Head], same: bool, kind: Kind, id: &str, set: &mut PairSet| {
        for x in a {
            for y in b {
                set.put(kind, x.0, y.0, same, id);
            }
        }
    };
    let put = set;
    for e in effective(entries) {
        let (k, id) = (e.kind, e.id.as_str());
        match &e.act {
            Act::Merge { groups } => {
                for i in 0..groups.len() {
                    for j in i + 1..groups.len() {
                        cross(&groups[i].heads, &groups[j].heads, true, k, id, put);
                    }
                }
            }
            Act::NotSame { groups } => {
                if let [a, b] = &groups[..] {
                    cross(&a.heads, &b.heads, false, k, id, put);
                }
            }
            Act::Split { from, heads, .. } => {
                let rest: Vec<Head> = from.heads.iter().filter(|h| !heads.contains(h)).copied().collect();
                cross(heads, &rest, false, k, id, put);
            }
            Act::Move { from, to, heads, .. } => {
                let rest: Vec<Head> = from.heads.iter().filter(|h| !heads.contains(h)).copied().collect();
                cross(heads, &rest, false, k, id, put);
                cross(heads, &to.heads, true, k, id, put);
            }
            Act::Answer { yes, group, heads } => cross(heads, &group.heads, *yes, k, id, put),
            Act::Name { head: Some(h), name, group, from } => {
                if let (Some(_), Some(g)) = (name, group) {
                    cross(std::slice::from_ref(h), &g.heads, true, k, id, put);
                }
                if let (None, Some(f)) = (name, from) {
                    cross(std::slice::from_ref(h), &f.heads, false, k, id, put);
                }
            }
            _ => {}
        }
    }
}

/// Bytes and entries (lines) of a record, for Preferences (on a worker).
pub fn usage(path: &Path) -> (u64, usize) {
    match fs::read(path) {
        Ok(bytes) => (bytes.len() as u64, bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count()),
        Err(_) => (0, 0),
    }
}

// ── writing ───────────────────────────────────────────────────────────────

/// Append one line: opened for appending (created 0600), a partial last
/// line closed first so the new one stands alone, one `write`, `fsync`.
fn append(path: &Path, line: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
            private_dir(dir);
        }
    }
    let mut opts = OpenOptions::new();
    opts.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    let len = f.metadata()?.len();
    let mut buf = String::with_capacity(line.len() + 1);
    if len > 0 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut last = [0u8; 1];
            if f.read_at(&mut last, len - 1).is_ok() && last[0] != b'\n' {
                buf.push('\n');
            }
        }
    }
    buf.push_str(line);
    f.write_all(buf.as_bytes())?;
    f.sync_data()?;
    Ok(())
}

/// Keep only the entries `keep` wants (whole file, atomically; an undo of
/// an entry taken out goes too).  Lines that can't be read go as well:
/// they can't be told apart by kind.  Nothing left: the file goes.
fn rewrite(path: &Path, keep: impl Fn(&Entry) -> bool) -> io::Result<usize> {
    let (entries, _) = read(path)?;
    let mut gone: HashSet<String> = HashSet::new();
    let mut text = String::new();
    for e in &entries {
        let undoes_gone = matches!(&e.act, Act::Undo { undoes, .. } if gone.contains(undoes));
        if keep(e) && !undoes_gone {
            text.push_str(&e.line());
        } else {
            gone.insert(e.id.clone());
        }
    }
    if text.is_empty() {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    } else {
        crate::ops::fsutil::atomic_write(path, text.as_bytes(), true)?;
        private_file(path);
    }
    Ok(gone.len())
}

/// Copy the record, the named groups as they are (`groups`, when given)
/// and a README of their format into a new folder in `into`
/// (`photo-pipeline-corrections-<date>`, numbered if taken; never
/// overwriting), names replaced by codes.  Reads nothing else and writes
/// nowhere else.  The entries copied.
fn export(path: &Path, into: &Path, groups: Option<&super::group_export::Snapshot>) -> io::Result<(PathBuf, usize)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let labels = groups.is_some_and(|g| !g.named.is_empty() || !g.added.is_empty() || !g.removed.is_empty() || !g.not_same.is_empty() || !g.dissolved.is_empty());
    if bytes.is_empty() && !labels {
        return Err(io::Error::new(io::ErrorKind::NotFound, "nothing is recorded and no group is named"));
    }
    let date = chrono::Local::now().format("%Y-%m-%d");
    let mut dir = into.join(format!("photo-pipeline-corrections-{date}"));
    let mut n = 2;
    loop {
        match fs::create_dir(&dir) {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => {
                dir = into.join(format!("photo-pipeline-corrections-{date}-{n}"));
                n += 1;
            }
            Err(e) => return Err(e),
        }
    }
    private_dir(&dir);
    let (mut entries, _) = parse(&String::from_utf8_lossy(&bytes));
    let mut codes = Codes::default();
    code_entries(&mut entries, &mut codes);
    if !entries.is_empty() {
        let text: String = entries.iter().map(Entry::line).collect();
        let copy = dir.join(FILE_NAME);
        crate::ops::fsutil::atomic_write(&copy, text.as_bytes(), false)?;
        private_file(&copy);
    }
    if let Some(g) = groups.filter(|_| labels) {
        // After the log, so a subject keeps the code the log gave it.
        let file = super::group_export::export(g, &mut codes);
        let text = serde_json::to_string(&file).map_err(io::Error::other)?;
        let out = dir.join(super::group_export::FILE_NAME);
        crate::ops::fsutil::atomic_write(&out, text.as_bytes(), false)?;
        private_file(&out);
    }
    crate::ops::fsutil::atomic_write(&dir.join("README.md"), README.as_bytes(), false)?;
    Ok((dir, entries.len()))
}

/// Codes for subjects (`fursuit-12`), one per subject, known by its id,
/// its name (any case) or both; shared by an export's two files.
#[derive(Default)]
pub struct Codes {
    by_sid: HashMap<String, String>,
    by_name: HashMap<(Kind, String), String>,
    next: HashMap<Kind, usize>,
}

impl Codes {
    fn fresh(&mut self, kind: Kind) -> String {
        let n = self.next.entry(kind).or_insert(0);
        *n += 1;
        format!("{}-{n}", kind.noun().replace(' ', "-"))
    }
    /// The code of a subject known by `sid`, `name` or both.
    pub fn code(&mut self, kind: Kind, sid: Option<&str>, name: Option<&str>) -> Option<String> {
        let name_key = name.map(|n| (kind, n.to_lowercase()));
        let known = sid.and_then(|s| self.by_sid.get(s)).or_else(|| name_key.as_ref().and_then(|k| self.by_name.get(k))).cloned();
        if sid.is_none() && name.is_none() {
            return None;
        }
        let code = known.unwrap_or_else(|| self.fresh(kind));
        if let Some(s) = sid {
            self.by_sid.insert(s.to_string(), code.clone());
        }
        if let Some(k) = name_key {
            self.by_name.insert(k, code.clone());
        }
        Some(code)
    }
    fn side(&mut self, kind: Kind, side: &mut Side) {
        let code = self.code(kind, side.sid.as_deref(), side.name.as_deref());
        side.name = code.clone();
        side.sid = code;
    }
    fn name(&mut self, kind: Kind, name: &mut Option<String>) {
        if let Some(n) = name.take() {
            *name = self.code(kind, None, Some(&n));
        }
    }
}

/// Replace the names and subject ids of `entries` by `c`'s codes
/// (`fursuit-12`, `person-7`): one code per subject throughout, whether an
/// entry names it by id, by name or both.  An export carries no names
/// (the user's choice); the record on this computer keeps them.
pub fn code_entries(entries: &mut [Entry], c: &mut Codes) {
    for e in entries.iter_mut() {
        let k = e.kind;
        match &mut e.act {
            Act::Merge { groups } | Act::NotSame { groups } => groups.iter_mut().for_each(|g| c.side(k, g)),
            Act::Split { from, .. } => c.side(k, from),
            Act::Edit { groups, out } => {
                groups.iter_mut().for_each(|g| c.side(k, g));
                for o in out {
                    if let To::New { name, .. } = &mut o.to {
                        c.name(k, name);
                    }
                }
            }
            Act::Move { from, to, .. } => {
                c.side(k, from);
                c.side(k, to);
            }
            Act::Answer { group, .. } => c.side(k, group),
            Act::Name { name, group, from, .. } => {
                // A head named into a subject: its name and the subject's
                // are one, so they share the code.
                if let Some(g) = group {
                    if g.name.is_none() {
                        g.name = name.clone();
                    }
                    c.side(k, g);
                    if name.is_some() {
                        *name = g.name.clone();
                    }
                } else {
                    c.name(k, name);
                }
                if let Some(f) = from {
                    c.side(k, f);
                }
            }
            Act::KindChange { to, name, .. } => {
                let to = *to;
                c.name(to, name);
            }
            Act::Draw { name, .. } => c.name(k, name),
            Act::RemoveMany { groups, .. } => groups.iter_mut().for_each(|g| c.side(k, g)),
            // A named group dissolved: its name becomes its code, `named`
            // says it had one.
            Act::Dissolve { group, .. } => c.side(k, group),
            Act::Remove { .. } | Act::Undo { .. } => {}
        }
    }
}

enum Job {
    Append(String),
    ForgetKind(Kind),
    DeleteAll,
    Export(PathBuf, Option<Box<super::group_export::Snapshot>>, Sender<io::Result<(PathBuf, usize)>>),
    Flush(Sender<()>),
}

/// The record's writer: a handle (cheap to clone) to a thread of its own
/// that appends, forgets and exports in the order asked.  With no path
/// (tests) the lines are kept in memory.
#[derive(Clone)]
pub struct Recorder {
    tx: Sender<Job>,
    on: Arc<AtomicBool>,
    path: Option<PathBuf>,
    memory: Arc<Mutex<Vec<String>>>,
    /// The entries recorded this session and still in the record: what
    /// each was (its act) and the kinds it is about, so an undo can say
    /// what it took back, and is written even while off.
    acts: Arc<Mutex<HashMap<String, (&'static str, Kind, Option<Kind>)>>>,
}

impl Recorder {
    /// A recorder writing to `path` (`None`: memory only), on by default
    /// (the user asked for the record; Preferences turns it off).
    pub fn new(path: Option<PathBuf>) -> Recorder {
        let (tx, rx) = crossbeam_channel::unbounded();
        let memory = Arc::new(Mutex::new(Vec::new()));
        let (p, mem) = (path.clone(), memory.clone());
        let spawned = std::thread::Builder::new().name("decisions".into()).spawn(move || run(rx, p, mem));
        if let Err(e) = spawned {
            eprintln!("decisions: couldn't start the writer ({e}); nothing will be recorded");
        }
        Recorder { tx, on: Arc::new(AtomicBool::new(true)), path, memory, acts: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Preferences ▸ *Keep a record of my corrections*.  Off: nothing is
    /// appended (what is kept stays until deleted).
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append an entry (nothing while off).  Returns its id when sent.
    pub fn record(&self, entry: Entry) -> Option<String> {
        if !self.is_on() {
            return None;
        }
        self.append_entry(entry)
    }

    fn append_entry(&self, entry: Entry) -> Option<String> {
        let id = entry.id.clone();
        let to = match &entry.act {
            Act::KindChange { to, .. } => Some(*to),
            _ => None,
        };
        let about = (entry.act.name(), entry.kind, to);
        self.tx.send(Job::Append(entry.line())).ok()?;
        self.acts.lock().insert(id.clone(), about);
        Some(id)
    }

    /// An undo of the entry `undone` (its id and kind), naming it and
    /// what it was.  Written even while off when that entry is in the
    /// record (the user's choice: the record never shows a correction
    /// that was taken back as still in force); an entry the record
    /// doesn't hold (made while off, forgotten, deleted) gets none.
    /// Returns the undo's own id.
    pub fn record_undo(&self, undone: (String, Kind)) -> Option<String> {
        let (id, kind) = undone;
        let of = self.acts.lock().get(&id).map(|a| a.0)?;
        self.append_entry(Entry::new(kind, Act::Undo { undoes: id, of: of.to_string() }, None, None))
    }

    /// *Forget all* of a kind: its entries leave the record.
    pub fn forget_kind(&self, kind: Kind) {
        self.acts.lock().retain(|_, (_, k, to)| *k != kind && *to != Some(kind));
        let _ = self.tx.send(Job::ForgetKind(kind));
    }

    /// *Delete the record…*: the file goes.
    pub fn delete_all(&self) {
        self.acts.lock().clear();
        let _ = self.tx.send(Job::DeleteAll);
    }

    /// Wait until everything sent is written.  **Blocks**: a worker, a
    /// test or the exit only.
    pub fn flush(&self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.tx.send(Job::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }

    /// Copy the record, the named groups (`groups`) and the README into
    /// a new folder in `into`; the folder made and the entries copied.
    /// **Blocks**: call it on a worker.
    pub fn export(&self, into: &Path, groups: Option<super::group_export::Snapshot>) -> io::Result<(PathBuf, usize)> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.export_to(into, groups, tx);
        rx.recv().map_err(|_| io::Error::other("the record's writer stopped"))?
    }

    /// [`Recorder::export`] without waiting: the answer goes to `reply`
    /// (the engine hands the groups over and carries on).
    pub fn export_to(&self, into: &Path, groups: Option<super::group_export::Snapshot>, reply: Sender<io::Result<(PathBuf, usize)>>) {
        if let Err(crossbeam_channel::SendError(Job::Export(_, _, reply))) = self.tx.send(Job::Export(into.to_path_buf(), groups.map(Box::new), reply)) {
            let _ = reply.send(Err(io::Error::other("the record's writer stopped")));
        }
    }

    /// The record's entries as written (after a flush).  **Blocks** on
    /// the file for a path: tests and the script driver only (with no
    /// path, the lines kept in memory).
    pub fn entries(&self) -> Vec<Entry> {
        self.flush();
        match &self.path {
            Some(p) => read(p).map(|(e, _)| e).unwrap_or_default(),
            None => parse(&self.memory.lock().concat()).0,
        }
    }

    /// The raw text (after a flush): the script driver's `decisions print`.
    pub fn text(&self) -> String {
        self.flush();
        match &self.path {
            Some(p) => fs::read_to_string(p).unwrap_or_default(),
            None => self.memory.lock().concat(),
        }
    }
}

fn run(rx: Receiver<Job>, path: Option<PathBuf>, memory: Arc<Mutex<Vec<String>>>) {
    // Lines that couldn't be written yet (a full disk): tried again with
    // the next, in order.
    let mut waiting: Vec<String> = Vec::new();
    for job in rx {
        match job {
            Job::Append(line) => match &path {
                None => memory.lock().push(line),
                Some(p) => {
                    waiting.push(line);
                    while let Some(first) = waiting.first() {
                        match append(p, first) {
                            Ok(()) => {
                                waiting.remove(0);
                            }
                            Err(e) => {
                                eprintln!("decisions: couldn't append to {} ({e}); kept to try again", p.display());
                                break;
                            }
                        }
                    }
                }
            },
            Job::ForgetKind(kind) => match &path {
                None => {
                    let text = memory.lock().concat();
                    let (entries, _) = parse(&text);
                    let mut gone = HashSet::new();
                    let mut kept = Vec::new();
                    for e in entries {
                        let undoes_gone = matches!(&e.act, Act::Undo { undoes, .. } if gone.contains(undoes));
                        if e.about(kind) || undoes_gone {
                            gone.insert(e.id.clone());
                        } else {
                            kept.push(e.line());
                        }
                    }
                    *memory.lock() = kept;
                }
                Some(p) => {
                    waiting.retain(|l| serde_json::from_str::<Entry>(l).map_or(true, |e| !e.about(kind)));
                    if let Err(e) = rewrite(p, |e| !e.about(kind)) {
                        eprintln!("decisions: couldn't forget the {} in {} ({e})", kind.plural(), p.display());
                    }
                }
            },
            Job::DeleteAll => {
                waiting.clear();
                memory.lock().clear();
                if let Some(p) = &path {
                    if let Err(e) = fs::remove_file(p) {
                        if e.kind() != io::ErrorKind::NotFound {
                            eprintln!("decisions: couldn't delete {} ({e})", p.display());
                        }
                    }
                }
            }
            Job::Export(into, groups, reply) => {
                let r = match &path {
                    Some(p) => export(p, &into, groups.as_deref()),
                    None => Err(io::Error::other("the record is kept in memory only")),
                };
                let _ = reply.send(r);
            }
            Job::Flush(reply) => {
                let _ = reply.send(());
            }
        }
    }
}

// ── the README that goes with an export ───────────────────────────────────

/// What an export puts beside the record: its format, for whoever reads
/// it later.
pub const README: &str = r#"# Photo Pipeline: a record of grouping corrections

`decisions.jsonl` holds the corrections made by hand to the People and
Fursuits groups in Photo Pipeline, one JSON object a line, oldest first.
It is the user's own data and points at photos by content key.  Keep it
private.

**No names.**  In this export every name and subject id is replaced by a
code, `person-7` or `fursuit-12`: the same code for the same person or
fursuit throughout the file, whether an entry named it or gave its id.
The record on the user's computer keeps the names.

`groups.json` (when any group is named, or any label kept) is the named
groups as they were at the export, by the same codes (below).

## Reading it

- One entry a line.  A line that doesn't parse (a write cut short) is
  skipped; so is an entry whose `v` is newer than the reader knows.
- Entries are never rewritten.  An `undo` entry takes back the entry whose
  `id` it names; an undo of an undo puts that entry back.
- A head is `["<key>", x, y, w, h]`: the photo's content key (32 hex
  digits: MD5 of the file size, as 8 little-endian bytes, then its first
  64 KB) and the box in thousandths of the oriented embedded preview.
- Groups are recorded by their heads, never by group numbers.  A group
  (`Side`) is `{"n": heads in it, "photos", "bursts", "days" (as the tab
  counted the group), "name", "sid" (the subject id in subjects.json), "d"
  (average-linkage cosine distance to the entry's first group, in `lib`'s
  space), "heads": its clearest heads, at most 16}`.

## Every entry

| Field | Meaning |
|---|---|
| `v` | Schema version (1). |
| `id` | 16 hex digits, unique in the record. |
| `t` | When, UTC, RFC 3339 with milliseconds. |
| `app` | The app's version. |
| `kind` | `person` or `fursuit`.  Kinds never mix. |
| `act` | What was decided (below). |
| `how` | How it was reached (below).  Absent on `undo`. |
| `lib` | `{"heads": with vectors, "white": whitened, "model": the vectors' model, "grouping": "tighter" / "balanced" / "looser" (fursuits), "cuts": {"group", "suggest", "strict", "sure_yes", "sure_no", "merge_suggest", "merge_sure", "merge_far"}}`: the setting and cut-offs in force. |

## Acts

| `act` | Fields | Labelled pairs |
|---|---|---|
| `merge` | `groups`: the group kept first | every pair across groups: same |
| `not_same` | `groups`: two | every pair across: different |
| `split` | `from`, `heads` (split off), `moved` (count) | `heads` × the rest of `from`: different |
| `move` | `from`, `to`, `heads`, `moved` | `heads` × `to`: same; × the rest of `from`: different |
| `answer` | `yes`, `group`, `heads` (one press) | `heads` × `group`: same if `yes`, else different |
| `name` | `head` (viewer) or `group`; `name` (absent: Unnamed); `group` joined; `from` left | head × `group`: same; Unnamed: head × `from`: different |
| `kind_change` | `head`, `to` (the new kind), `name` | none |
| `draw` | `head`, `name` | none |
| `remove` | `head` (not a face / not a fursuit head) | none |
| `remove_many` | `groups` (as they were), `heads`, `removed` (whole groups or picked heads: not faces / not fursuit heads) | none |
| `edit` | `groups` (as they were), `out`: `[{"from": index into groups, "to": {"is": "group", "group"} / {"is": "new", "new", "name"} / {"is": "removed"} / {"is": "taken_out"}, "heads", "moved"}]` (the group editor's Apply) | each `out` read as a `move`, `split` or `remove_many` |
| `dissolve` | `group` (as it was), `heads` (all of them, at most 200), `dissolved` (count), `named` (it had a name, which went: its code is `group.name`), `spread` (the heads' mean cosine distance to their mean) | none: the group is not one subject, but some of its heads may be |
| `undo` | `undoes` (an id), `of` (its act) | takes that entry's pairs back |

## How

`{"by": "hand" | "distance" | "judge" | "proposal" | "check", "at": where}`, with:

- `distance`: `d` (average linkage for a pair of groups; for a review
  answer, the nearest confirmed member), `sure`, `pair` (the head pair).
- `judge`: `judge` (the model) and `judge_cut` (its yes threshold); for a
  pair of groups `yes`, `no`, `unseen` (head pairs; the majority decided),
  for a review answer `p` (P(yes), the mean of both orders) and
  `offered` (`false`: the judge left it out and the user asked to see it).
- `at`: `drag`, `pick`, `menu`, `name`, `heads`, `side-by-side`, `editor`,
  `faces`, `viewer`, `merge-review`, `similar`, `review`.

## groups.json: the named groups now

The log says how the groups came to be; `groups.json` is what they are at
the export, the user's curated ground truth.  Unnamed automatic groups are
left out (nobody checked them).

```
{"v": 1, "t": exported (UTC), "app": version,
 "kinds": [{"kind", "heads", "white", "model", "grouping", "cuts"}],
 "groups": [{"code", "kind", "photos", "bursts", "days",
             "heads": [heads], "rejected": [heads]}],
 "added": [{"kind", "head"}], "removed": [{"kind", "head"}],
 "not_same": [{"kind", "a": [heads], "b": [heads]}],
 "dissolved": [{"kind", "heads": [heads]}]}
```

- `kinds`: each kind's state as for the log's `lib`: head count, whether
  whitened, the vectors' model, the grouping choice and the cut-offs in
  force at the export.
- `groups`: each named person or fursuit by its code (the log's code for
  the same subject), its counts as the app showed them, its member heads
  (confirmed: named, moved or merged into it, answered Yes) and its
  `rejected` heads (answered No for it).
- `added`: boxes drawn in the viewer; `removed`: boxes the user said are
  not a face or not a fursuit head (never a match for anything).
- `not_same`: groups the user said are different, by their heads.
- `dissolved`: groups the user dissolved (no coherent match: only
  lookalikes), each by all its heads.  The heads are still faces or
  fursuit heads, never grouped together again by the app; a head may
  since have been confirmed into a named group.

### Labelled pairs from groups.json

- Within a group: every two heads are the same.
- Across two named groups of one kind: different.  (This grows with the
  square of the named heads; sample it if it is large.)
- A rejected head and each member of its group: different.
- `not_same`: every head of `a` and of `b`: different.
- `dissolved`: none.  Not all of a set's heads are one subject, but any
  two of them may be.
- Never across kinds.

Built after the log's pairs, these win for a pair both hold: the groups
are the state after every decision.  The log adds what the groups can't
say: how each decision was offered (distance, judge), what was taken back,
and decisions about groups never named.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Key {
        Key([n; 16])
    }

    fn head(n: u8, i: u16) -> FaceRef {
        FaceRef::new(key(n), [i as f32 / 1000.0, 0.1, 0.05, 0.08])
    }

    fn side(n: u8, k: usize) -> Side {
        Side::new(k, None, None, (0..k as u16).map(|i| head(n, i * 10)))
    }

    fn lib() -> Lib {
        let g = crate::search::heads::GroupingCuts::of(crate::search::heads::Grouping::Balanced, true);
        Lib {
            heads: 9000,
            white: true,
            model: "siglip2-b16-256".into(),
            grouping: Some(g.grouping),
            cuts: Cuts { group: g.group, suggest: 0.711, strict: 0.671, sure_yes: Some(0.639), sure_no: Some(0.786), merge_suggest: Some(g.merge_suggest), merge_sure: Some(g.merge_sure), merge_far: Some(g.merge_far) },
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-decisions-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A head is five values: the key and the box in thousandths, and
    /// comes back equal.
    #[test]
    fn a_head_is_written_compactly_and_read_back() {
        let h = Head(FaceRef::new(key(0xab), [0.1234, 0.5, 0.0789, 0.09]));
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(s, r#"["abababababababababababababababab",123,500,79,90]"#);
        assert_eq!(serde_json::from_str::<Head>(&s).unwrap(), h);
    }

    /// A split arranged before it was confirmed says how many heads were
    /// moved by hand: a proposal as `proposal`, picked heads as `hand` on
    /// *Heads*; other decisions carry no such field.
    #[test]
    fn an_arranged_split_records_the_heads_moved_by_hand() {
        let how = How::of(Via::Editor { proposal: true, edited: 3 }, None);
        assert_eq!((how.by.as_str(), how.at, how.edited), ("proposal", Some(At::Editor), Some(3)));
        let line = serde_json::to_string(&how).unwrap();
        assert!(line.contains(r#""edited":3"#) && line.contains(r#""at":"editor""#), "{line}");
        assert_eq!(serde_json::from_str::<How>(&line).unwrap(), how);
        let as_offered = How::of(Via::Editor { proposal: true, edited: 0 }, None);
        assert!(serde_json::to_string(&as_offered).unwrap().contains(r#""edited":0"#), "0 is kept: confirmed as offered");
        let picked = How::of(Via::Editor { proposal: false, edited: 2 }, None);
        assert_eq!((picked.by.as_str(), picked.at, picked.edited), ("hand", Some(At::Editor), Some(2)));
        // An entry written before the editor (a proposal confirmed in the
        // old dialog) still reads.
        let old: How = serde_json::from_str(r#"{"by":"proposal","at":"heads","edited":1}"#).unwrap();
        assert_eq!((old.by.as_str(), old.at, old.edited), ("proposal", Some(At::Heads), Some(1)));
        assert!(!serde_json::to_string(&How::of(Via::Hand(At::Heads), None)).unwrap().contains("edited"));
    }

    /// Every act round-trips through a line, with its kind, how and lib.
    #[test]
    fn every_act_round_trips() {
        let acts = vec![
            Act::Merge { groups: vec![side(1, 3), side(2, 2)] },
            Act::NotSame { groups: vec![side(1, 2), side(2, 2)] },
            Act::Split { from: side(1, 4), heads: heads([head(1, 0)]), moved: 1 },
            Act::Move { from: side(1, 4), to: side(2, 3), heads: heads([head(1, 0)]), moved: 1 },
            Act::Answer { yes: true, group: side(1, 3), heads: heads([head(3, 0), head(3, 10)]) },
            Act::Name { head: Some(Head(head(4, 0))), name: Some("Someone".into()), group: Some(side(1, 2)), from: None },
            Act::KindChange { head: Head(head(5, 0)), to: Kind::Person, name: None },
            Act::Draw { head: Head(head(6, 0)), name: Some("Someone".into()) },
            Act::Remove { head: Head(head(7, 0)) },
            Act::RemoveMany { groups: vec![side(8, 2)], heads: heads([head(8, 0), head(8, 10)]), removed: 2 },
            Act::Undo { undoes: "0123456789abcdef".into(), of: "merge".into() },
            Act::Dissolve { group: side(9, 3), heads: heads([head(9, 0), head(9, 10), head(9, 20)]), dissolved: 3, named: false, spread: Some(0.4123) },
            Act::Dissolve { group: side(9, 2), heads: heads([head(9, 0), head(9, 10)]), dissolved: 2, named: false, spread: None },
            Act::Edit {
                groups: vec![side(1, 5), side(2, 3)],
                out: vec![
                    Out { from: Some(0), to: To::Group { group: 1 }, heads: heads([head(1, 0)]), moved: 1 },
                    Out { from: Some(0), to: To::New { new: 0, name: Some("Someone".into()) }, heads: heads([head(1, 10)]), moved: 1 },
                    Out { from: Some(0), to: To::Removed, heads: heads([head(1, 20)]), moved: 1 },
                    Out { from: Some(0), to: To::TakenOut, heads: heads([head(1, 30)]), moved: 1 },
                    Out { from: None, to: To::New { new: 1, name: None }, heads: heads([head(9, 0)]), moved: 1 },
                ],
            },
        ];
        for act in acts {
            let e = Entry::new(Kind::Fursuit, act, Some(How::hand(At::Drag)), Some(lib()));
            let line = e.line();
            assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
            let back: Entry = serde_json::from_str(&line).unwrap();
            assert_eq!(back, e, "{line}");
            assert!(line.contains(&format!(r#""act":"{}""#, e.act.name())), "{line}");
        }
    }

    /// A dissolved group's line: the group as it was, every head, how
    /// many, whether it was named and how spread out; by hand, where; the
    /// library with the grouping choice.  It labels no pair (some of its
    /// heads may be one subject), and an export codes its name.
    #[test]
    fn a_dissolve_is_one_line_with_its_heads_and_codes_its_name() {
        let group = Side { sid: Some("aaaa".into()), name: Some("Sparky Dragon".into()), photos: Some(3), bursts: Some(2), days: Some(1), ..side(9, 3) };
        let e = Entry::new(Kind::Fursuit, Act::Dissolve { group, heads: heads([head(9, 0), head(9, 10), head(9, 20)]), dissolved: 3, named: true, spread: Some(0.3125) }, Some(How::hand(At::Editor)), Some(lib()));
        let line = e.line();
        for part in [
            r#""kind":"fursuit","act":"dissolve","group":{"n":3,"photos":3,"bursts":2,"days":1,"name":"Sparky Dragon","sid":"aaaa","heads":[["09090909090909090909090909090909",0,100,50,80],"#,
            r#""heads":[["09090909090909090909090909090909",0,100,50,80],["09090909090909090909090909090909",10,100,50,80],["09090909090909090909090909090909",20,100,50,80]],"dissolved":3,"named":true,"spread":0.3125,"how":{"by":"hand","at":"editor"}"#,
            r#""grouping":"balanced""#,
        ] {
            assert!(line.contains(part), "{part}
in {line}");
        }
        assert!(pairs(std::slice::from_ref(&e)).is_empty(), "no pair is labelled");
        let mut coded = vec![e];
        code_entries(&mut coded, &mut Codes::default());
        let text = coded[0].line();
        assert!(!text.contains("Sparky") && !text.contains("aaaa"), "{text}");
        assert!(matches!(&coded[0].act, Act::Dissolve { group, named: true, .. } if group.name.as_deref() == Some("fursuit-1") && group.sid.as_deref() == Some("fursuit-1")));
    }

    /// A partial last line (a crash mid-write) and a line from a newer
    /// version are skipped; the rest load; the next append starts on a
    /// line of its own.
    #[test]
    fn a_partial_last_line_is_tolerated() {
        let d = tmp("partial");
        let p = d.join(FILE_NAME);
        let a = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, None);
        let mut newer = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(2, 0)) }, None, None);
        newer.v = SCHEMA + 1;
        let cut = a.line();
        fs::write(&p, format!("{}{}{}", a.line(), newer.line(), &cut[..cut.len() / 2])).unwrap();
        let (entries, bad) = read(&p).unwrap();
        assert_eq!((entries.len(), bad), (1, 2));
        let b = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(3, 0)) }, None, None);
        append(&p, &b.line()).unwrap();
        let (entries, bad) = read(&p).unwrap();
        assert_eq!(entries, vec![a, b], "the new line stands alone");
        assert_eq!(bad, 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let q = d.join("new").join(FILE_NAME);
            append(&q, &entries[0].line()).unwrap();
            assert_eq!(fs::metadata(&q).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// Undo appends; the decision leaves what is in force, and an undo of
    /// the undo puts it back.
    #[test]
    fn undo_takes_an_entry_back_and_an_undo_of_it_puts_it_back() {
        let a = Entry::new(Kind::Fursuit, Act::Merge { groups: vec![side(1, 2), side(2, 2)] }, Some(How::hand(At::Pick)), None);
        let b = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(3, 0)) }, Some(How::hand(At::Viewer)), None);
        let u = Entry::new(Kind::Fursuit, Act::Undo { undoes: a.id.clone(), of: "merge".into() }, None, None);
        let all = vec![a.clone(), b.clone(), u.clone()];
        assert_eq!(effective(&all), vec![&b]);
        let uu = Entry::new(Kind::Fursuit, Act::Undo { undoes: u.id.clone(), of: "undo".into() }, None, None);
        let all = vec![a.clone(), b.clone(), u, uu];
        assert_eq!(effective(&all), vec![&a, &b]);
    }

    /// The research's use: labelled same / different pairs rebuilt from a
    /// sample record, with undone decisions left out and later ones
    /// winning.
    #[test]
    fn labelled_pairs_are_rebuilt_from_a_sample_record() {
        let (a, b, c) = (side(1, 2), side(2, 2), side(3, 1));
        let merge = Entry::new(Kind::Fursuit, Act::Merge { groups: vec![a.clone(), b.clone()] }, Some(How::hand(At::Drag)), Some(lib()));
        let not_same = Entry::new(Kind::Fursuit, Act::NotSame { groups: vec![a.clone(), c.clone()] }, Some(How::hand(At::MergeReview)), Some(lib()));
        let moved = Entry::new(Kind::Fursuit, Act::Move { from: side(4, 3), to: c.clone(), heads: heads([head(4, 0)]), moved: 1 }, Some(How::hand(At::Heads)), Some(lib()));
        let answer = Entry::new(Kind::Person, Act::Answer { yes: false, group: side(5, 1), heads: heads([head(6, 0)]) }, None, None);
        let undone = Entry::new(Kind::Fursuit, Act::Merge { groups: vec![side(7, 1), side(8, 1)] }, None, None);
        let undo = Entry::new(Kind::Fursuit, Act::Undo { undoes: undone.id.clone(), of: "merge".into() }, None, None);
        let text: String = [&merge, &not_same, &moved, &answer, &undone, &undo].iter().map(|e| e.line()).collect();
        let (entries, bad) = parse(&text);
        assert_eq!(bad, 0);
        let ps = pairs(&entries);
        let find = |x: FaceRef, y: FaceRef| ps.iter().find(|p| (p.a == x && p.b == y) || (p.a == y && p.b == x)).map(|p| p.same);
        // The merge: 2 × 2 the same.
        assert_eq!(find(head(1, 0), head(2, 10)), Some(true));
        // Not the same: 2 × 1 different.
        assert_eq!(find(head(1, 10), head(3, 0)), Some(false));
        // The move: the same as `to`, different from the rest of `from`.
        assert_eq!(find(head(4, 0), head(3, 0)), Some(true));
        assert_eq!(find(head(4, 0), head(4, 10)), Some(false));
        assert_eq!(find(head(4, 0), head(4, 20)), Some(false));
        // A No.
        assert_eq!(find(head(6, 0), head(5, 0)), Some(false));
        // Undone: no pair.
        assert_eq!(find(head(7, 0), head(8, 0)), None);
        assert_eq!(ps.len(), 4 + 2 + 3 + 1);
        assert!(ps.iter().all(|p| p.kind == Kind::Fursuit || p.id == answer.id));
    }

    /// Forgetting a kind takes its entries (and a kind change into it, and
    /// the undo of anything taken) and leaves the other kind's; nothing
    /// left removes the file.  The stand-in's file and the export never
    /// touch anything else.
    #[test]
    fn forget_kind_filters_that_kind_and_the_writer_keeps_order() {
        let d = tmp("forget");
        let p = d.join(FILE_NAME);
        let r = Recorder::new(Some(p.clone()));
        let fur = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, None);
        let person = Entry::new(Kind::Person, Act::Remove { head: Head(head(2, 0)) }, None, None);
        let into_fur = Entry::new(Kind::Person, Act::KindChange { head: Head(head(3, 0)), to: Kind::Fursuit, name: None }, None, None);
        let undo = Entry::new(Kind::Person, Act::Undo { undoes: into_fur.id.clone(), of: "kind_change".into() }, None, None);
        for e in [&fur, &person, &into_fur, &undo] {
            r.record(e.clone());
        }
        assert_eq!(r.entries().len(), 4);
        r.forget_kind(Kind::Fursuit);
        assert_eq!(r.entries(), vec![person.clone()]);
        // Exported: a new folder with the record and the README.
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let (dir, n) = r.export(&out, None).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read(dir.join(FILE_NAME)).unwrap(), fs::read(&p).unwrap(), "nothing named: copied as is");
        assert!(fs::read_to_string(dir.join("README.md")).unwrap().contains("not_same"));
        let (dir2, _) = r.export(&out, None).unwrap();
        assert_ne!(dir, dir2, "never over an earlier export");
        r.forget_kind(Kind::Person);
        r.flush();
        assert!(!p.exists(), "nothing left: the file goes");
        let _ = fs::remove_dir_all(&d);
    }

    /// An export carries no name and no subject id: each subject has one
    /// code throughout, whether named, given by id or both; the record
    /// itself keeps the names.
    #[test]
    fn an_export_has_codes_for_names() {
        let d = tmp("export-names");
        let p = d.join(FILE_NAME);
        let r = Recorder::new(Some(p.clone()));
        let named = |sid: &str, name: &str, n: u8| Side { sid: Some(sid.into()), name: Some(name.into()), ..side(n, 2) };
        let entries = vec![
            Entry::new(Kind::Fursuit, Act::Merge { groups: vec![named("aaaa", "Sparky Dragon", 1), side(2, 2)] }, None, None),
            Entry::new(Kind::Fursuit, Act::NotSame { groups: vec![named("aaaa", "Sparky Dragon", 1), named("bbbb", "Mossy Wolf", 3)] }, None, None),
            Entry::new(Kind::Fursuit, Act::Name { head: Some(Head(head(4, 0))), name: Some("mossy wolf".into()), group: None, from: None }, None, None),
            Entry::new(Kind::Person, Act::Name { head: None, name: Some("Jane Example".into()), group: Some(named("cccc", "Jane Example", 5)), from: None }, None, None),
            Entry::new(Kind::Fursuit, Act::KindChange { head: Head(head(6, 0)), to: Kind::Person, name: Some("Jane Example".into()) }, None, None),
        ];
        for e in &entries {
            r.record(e.clone());
        }
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let (dir, n) = r.export(&out, None).unwrap();
        assert_eq!(n, 5);
        let text = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        for secret in ["Sparky", "Mossy", "mossy", "Jane", "aaaa", "bbbb", "cccc"] {
            assert!(!text.contains(secret), "{secret} in the export: {text}");
        }
        let (got, bad) = parse(&text);
        assert_eq!((got.len(), bad), (5, 0));
        let code = |i: usize, g: usize| match &got[i].act {
            Act::Merge { groups } | Act::NotSame { groups } => groups[g].name.clone().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(code(0, 0), "fursuit-1");
        assert_eq!(code(1, 0), "fursuit-1", "the same subject, the same code");
        assert_eq!(code(1, 1), "fursuit-2");
        assert!(matches!(&got[0].act, Act::Merge { groups } if groups[1].name.is_none() && groups[0].sid.as_deref() == Some("fursuit-1")));
        assert!(matches!(&got[2].act, Act::Name { name: Some(n), .. } if n == "fursuit-2"), "a name typed in another case is the same subject");
        assert!(matches!(&got[3].act, Act::Name { name: Some(n), group: Some(g), .. } if n == "person-1" && g.name.as_deref() == Some("person-1")));
        assert!(matches!(&got[4].act, Act::KindChange { name: Some(n), .. } if n == "person-1"), "coded in the kind it became");
        assert!(fs::read_to_string(&p).unwrap().contains("Sparky Dragon"), "the record keeps the names");
        assert!(fs::read_to_string(dir.join("README.md")).unwrap().contains("No names"));
        let _ = fs::remove_dir_all(&d);
    }

    /// Off: nothing new is recorded, but an undo of an entry the record
    /// holds still is (so the record never shows a correction taken back
    /// as in force); an undo of one it doesn't hold isn't.
    #[test]
    fn off_still_records_undos_of_entries_in_the_record() {
        let r = Recorder::new(None);
        let kept = r.record(Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, None)).unwrap();
        r.set_on(false);
        assert_eq!(r.record(Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(2, 0)) }, None, None)), None, "nothing new while off");
        assert_eq!(r.record_undo(("0000000000000000".into(), Kind::Fursuit)), None, "not in the record: no undo");
        let undo = r.record_undo((kept.clone(), Kind::Fursuit)).expect("in the record: its undo is written");
        // Its undo can be undone in turn (Ctrl+Z, then Ctrl+Z on the undo step).
        assert!(r.record_undo((undo.clone(), Kind::Fursuit)).is_some());
        let got = r.entries();
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[1].act, Act::Undo { undoes, of } if *undoes == kept && of == "remove"));
        // Forgotten or deleted entries are no longer in the record.
        r.forget_kind(Kind::Fursuit);
        assert_eq!(r.record_undo((kept, Kind::Fursuit)), None);
    }

    /// Off records nothing; on again records.
    #[test]
    fn the_switch_off_writes_nothing() {
        let d = tmp("off");
        let p = d.join(FILE_NAME);
        let r = Recorder::new(Some(p.clone()));
        r.set_on(false);
        assert_eq!(r.record(Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, None)), None);
        r.flush();
        assert!(!p.exists());
        r.set_on(true);
        assert!(r.record(Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, None)).is_some());
        assert_eq!(r.entries().len(), 1);
        r.delete_all();
        r.flush();
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    /// Sizes for the report: a merge of two full sides, a review answer
    /// with the judge, a label edit.
    #[test]
    fn entries_stay_compact() {
        let full = |n: u8| Side { n: 40, photos: Some(38), bursts: Some(21), days: Some(3), name: Some("Some name".into()), sid: Some("0123456789abcdef0123456789abcdef".into()), d: Some(0.0841), heads: (0..SIDE_HEADS as u16).map(|i| Head(head(n, i * 37))).collect() };
        let how = How { by: "judge".into(), at: Some(At::MergeReview), d: Some(0.0912), judge: Some("qwen3-vl-reranker-8b".into()), judge_cut: Some(0.5), yes: Some(2), no: Some(1), unseen: Some(0), ..How::default() };
        let merge = Entry::new(Kind::Fursuit, Act::Merge { groups: vec![full(1), full(2)] }, Some(how), Some(lib())).line().len();
        let review = How { by: "judge".into(), at: Some(At::Review), d: Some(0.104), judge: Some("qwen3-vl-reranker-8b".into()), judge_cut: Some(0.5), p: Some(0.8312), offered: Some(true), pair: Some([Head(head(9, 0)), Head(head(1, 0))]), ..How::default() };
        let answer = Entry::new(Kind::Fursuit, Act::Answer { yes: true, group: full(1), heads: heads([head(9, 0), head(9, 1)]) }, Some(review), Some(lib())).line().len();
        let label = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, Some(How::hand(At::Viewer)), Some(lib())).line().len();
        eprintln!("bytes: merge {merge}, answer {answer}, label {label}");
        assert!(merge < 2800 && answer < 1900 && label < 500, "merge {merge}, answer {answer}, label {label}");
        assert!(Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, None, Some(lib())).line().contains(r#""grouping":"balanced""#));
    }
}
