//! The record of the user's corrections to people and fursuits: an
//! append-only JSON-lines file of every grouping decision (merges, "not
//! the same", splits and moves, review answers, the viewer's label
//! edits, and undo), kept so that later research can learn from them
//! (re-tuning the cut-offs, training a small adapter).
//!
//! - **User data, not cache.**  `~/.local/share/photo-pipeline/
//!   decisions.jsonl`, beside `subjects.json`: *Clear* and *Rebuild* keep
//!   it; *Forget all* of a kind takes that kind's entries out; Preferences
//!   ▸ Fursuits deletes it on request.  Readable by the user alone (0600
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
        Side { n, name, sid, d: None, heads }
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
            Act::Undo { .. } => "undo",
        }
    }
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
    /// The split proposed around two heads seen in one photo.
    Proposal,
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
            Via::Proposal => How { by: "proposal".into(), at: Some(At::Heads), ..How::default() },
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
    pub group: f32,
    pub suggest: f32,
    pub strict: f32,
    /// The judge's band (fursuits): offered at once under `sure_yes`,
    /// asked of the judge up to `sure_no`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_yes: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_no: Option<f32>,
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
    let mut out: Vec<Pair> = Vec::new();
    let mut at: HashMap<(FaceRef, FaceRef), usize> = HashMap::new();
    let mut put = |kind: Kind, a: FaceRef, b: FaceRef, same: bool, id: &str| {
        if a == b {
            return;
        }
        let key = if (a.key.0, a.thousandths()) <= (b.key.0, b.thousandths()) { (a, b) } else { (b, a) };
        let p = Pair { kind, a: key.0, b: key.1, same, id: id.to_string() };
        match at.get(&key) {
            Some(&i) => out[i] = p,
            None => {
                at.insert(key, out.len());
                out.push(p);
            }
        }
    };
    let cross = |a: &[Head], b: &[Head], same: bool, kind: Kind, id: &str, put: &mut dyn FnMut(Kind, FaceRef, FaceRef, bool, &str)| {
        for x in a {
            for y in b {
                put(kind, x.0, y.0, same, id);
            }
        }
    };
    for e in effective(entries) {
        let (k, id) = (e.kind, e.id.as_str());
        match &e.act {
            Act::Merge { groups } => {
                for i in 0..groups.len() {
                    for j in i + 1..groups.len() {
                        cross(&groups[i].heads, &groups[j].heads, true, k, id, &mut put);
                    }
                }
            }
            Act::NotSame { groups } => {
                if let [a, b] = &groups[..] {
                    cross(&a.heads, &b.heads, false, k, id, &mut put);
                }
            }
            Act::Split { from, heads, .. } => {
                let rest: Vec<Head> = from.heads.iter().filter(|h| !heads.contains(h)).copied().collect();
                cross(heads, &rest, false, k, id, &mut put);
            }
            Act::Move { from, to, heads, .. } => {
                let rest: Vec<Head> = from.heads.iter().filter(|h| !heads.contains(h)).copied().collect();
                cross(heads, &rest, false, k, id, &mut put);
                cross(heads, &to.heads, true, k, id, &mut put);
            }
            Act::Answer { yes, group, heads } => cross(heads, &group.heads, *yes, k, id, &mut put),
            Act::Name { head: Some(h), name, group, from } => {
                if let (Some(_), Some(g)) = (name, group) {
                    cross(std::slice::from_ref(h), &g.heads, true, k, id, &mut put);
                }
                if let (None, Some(f)) = (name, from) {
                    cross(std::slice::from_ref(h), &f.heads, false, k, id, &mut put);
                }
            }
            _ => {}
        }
    }
    out
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

/// Copy the record and a README of its format into a new folder in
/// `into` (`photo-pipeline-corrections-<date>`, numbered if taken; never
/// overwriting).  Reads nothing else and writes nowhere else.
fn export(path: &Path, into: &Path) -> io::Result<(PathBuf, usize)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if bytes.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the record is empty"));
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
    let copy = dir.join(FILE_NAME);
    crate::ops::fsutil::atomic_write(&copy, &bytes, false)?;
    private_file(&copy);
    crate::ops::fsutil::atomic_write(&dir.join("README.md"), README.as_bytes(), false)?;
    let entries = bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count();
    Ok((dir, entries))
}

enum Job {
    Append(String),
    ForgetKind(Kind),
    DeleteAll,
    Export(PathBuf, Sender<io::Result<(PathBuf, usize)>>),
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
    /// What each entry recorded this session was (its act), so an undo
    /// can say what it took back without reading the file.
    acts: Arc<Mutex<HashMap<String, &'static str>>>,
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
        let id = entry.id.clone();
        let act = entry.act.name();
        self.tx.send(Job::Append(entry.line())).ok()?;
        self.acts.lock().insert(id.clone(), act);
        Some(id)
    }

    /// An undo of the entry `undone` (its id and kind), naming it and
    /// what it was; nothing while off.  Returns the undo's own id.
    pub fn record_undo(&self, undone: (String, Kind)) -> Option<String> {
        let (id, kind) = undone;
        let of = self.acts.lock().get(&id).copied().unwrap_or("unknown");
        self.record(Entry::new(kind, Act::Undo { undoes: id, of: of.to_string() }, None, None))
    }

    /// *Forget all* of a kind: its entries leave the record.
    pub fn forget_kind(&self, kind: Kind) {
        let _ = self.tx.send(Job::ForgetKind(kind));
    }

    /// *Delete the record…*: the file goes.
    pub fn delete_all(&self) {
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

    /// Copy the record and its README into a new folder in `into`; the
    /// folder made and the entries copied.  **Blocks**: call it on a
    /// worker.
    pub fn export(&self, into: &Path) -> io::Result<(PathBuf, usize)> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx.send(Job::Export(into.to_path_buf(), tx)).map_err(|_| io::Error::other("the record's writer stopped"))?;
        rx.recv().map_err(|_| io::Error::other("the record's writer stopped"))?
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
            Job::Export(into, reply) => {
                let r = match &path {
                    Some(p) => export(p, &into),
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
It is the user's own data: it names people and fursuits as the user named
them, and points at photos by content key.  Keep it private.

## Reading it

- One entry a line.  A line that doesn't parse (a write cut short) is
  skipped; so is an entry whose `v` is newer than the reader knows.
- Entries are never rewritten.  An `undo` entry takes back the entry whose
  `id` it names; an undo of an undo puts that entry back.
- A head is `["<key>", x, y, w, h]`: the photo's content key (32 hex
  digits: MD5 of the file size, as 8 little-endian bytes, then its first
  64 KB) and the box in thousandths of the oriented embedded preview.
- Groups are recorded by their heads, never by group numbers.  A group
  (`Side`) is `{"n": heads in it, "name", "sid" (the subject id in
  subjects.json), "d" (average-linkage cosine distance to the entry's
  first group, in `lib`'s space), "heads": its clearest heads, at most 16}`.

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
| `lib` | `{"heads": with vectors, "white": whitened, "model": the vectors' model, "cuts": {"group", "suggest", "strict", "sure_yes", "sure_no"}}`: the cut-offs in force. |

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
| `undo` | `undoes` (an id), `of` (its act) | takes that entry's pairs back |

## How

`{"by": "hand" | "distance" | "judge" | "proposal" | "check", "at": where}`, with:

- `distance`: `d` (average linkage for a pair of groups; for a review
  answer, the nearest confirmed member), `sure`, `pair` (the head pair).
- `judge`: `judge` (the model) and `judge_cut` (its yes threshold); for a
  pair of groups `yes`, `no`, `unseen` (head pairs; the majority decided),
  for a review answer `p` (P(yes), the mean of both orders) and
  `offered` (`false`: the judge left it out and the user asked to see it).
- `at`: `drag`, `pick`, `menu`, `name`, `heads`, `faces`, `viewer`,
  `merge-review`, `similar`, `review`.
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
        Lib { heads: 9000, white: true, model: "siglip2-b16-256".into(), cuts: Cuts { group: 0.69, suggest: 0.711, strict: 0.671, sure_yes: Some(0.639), sure_no: Some(0.786) } }
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
            Act::Undo { undoes: "0123456789abcdef".into(), of: "merge".into() },
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
        let (dir, n) = r.export(&out).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read(dir.join(FILE_NAME)).unwrap(), fs::read(&p).unwrap());
        assert!(fs::read_to_string(dir.join("README.md")).unwrap().contains("not_same"));
        let (dir2, _) = r.export(&out).unwrap();
        assert_ne!(dir, dir2, "never over an earlier export");
        r.forget_kind(Kind::Person);
        r.flush();
        assert!(!p.exists(), "nothing left: the file goes");
        let _ = fs::remove_dir_all(&d);
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
        let full = |n: u8| Side { n: 40, name: Some("Some name".into()), sid: Some("0123456789abcdef0123456789abcdef".into()), d: Some(0.0841), heads: (0..SIDE_HEADS as u16).map(|i| Head(head(n, i * 37))).collect() };
        let how = How { by: "judge".into(), at: Some(At::MergeReview), d: Some(0.0912), judge: Some("qwen3-vl-reranker-8b".into()), judge_cut: Some(0.5), yes: Some(2), no: Some(1), unseen: Some(0), ..How::default() };
        let merge = Entry::new(Kind::Fursuit, Act::Merge { groups: vec![full(1), full(2)] }, Some(how), Some(lib())).line().len();
        let review = How { by: "judge".into(), at: Some(At::Review), d: Some(0.104), judge: Some("qwen3-vl-reranker-8b".into()), judge_cut: Some(0.5), p: Some(0.8312), offered: Some(true), pair: Some([Head(head(9, 0)), Head(head(1, 0))]), ..How::default() };
        let answer = Entry::new(Kind::Fursuit, Act::Answer { yes: true, group: full(1), heads: heads([head(9, 0), head(9, 1)]) }, Some(review), Some(lib())).line().len();
        let label = Entry::new(Kind::Fursuit, Act::Remove { head: Head(head(1, 0)) }, Some(How::hand(At::Viewer)), Some(lib())).line().len();
        eprintln!("bytes: merge {merge}, answer {answer}, label {label}");
        assert!(merge < 2600 && answer < 1800 && label < 400, "merge {merge}, answer {answer}, label {label}");
    }
}
