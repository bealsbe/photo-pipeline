//! What the analysis suggests, per shot and per burst: "looks out of
//! focus", "accidental frame", the sharpest frame of each moment, moments
//! (presses of one scene merged), and duplicates.  Pure decisions over the
//! collection, its presses (`Bursts`) and the measurements
//! (`imaging::quality::Features`); nothing here marks anything.
//!
//! A shot is a file, or the two halves of a RAW+JPG pair; its JPG stands
//! for it (as in `Bursts`), and a hint applies to both halves.
//!
//! Thresholds are for the ~1616-px embedded preview and were set on an A7 V
//! convention day (see `docs/image-quality-plan.md`, "Phase 2 engine as
//! built").  Other bodies use them too (decision 9: best effort).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::bursts::Bursts;
use super::collection::{Collection, RecordId};
use super::duplicates::{self, DuplicateGroup, Kind as DupKind};
use super::record::{FileType, PhotoRecord};
use crate::imaging::quality::{self, Features};
use crate::util::canonical_parent;

/// "Looks out of focus" whatever else is in the burst: the sharpest area
/// ([`Features::local`]) below this.  On 1 087 frames from three shoots
/// it flagged 7, all clearly bad (motion blur at arm's length, a subject
/// inside the lens's minimum focus distance, two lens-to-wall accidents);
/// good night shots start at 21.
pub const SOFT_LOCAL: f32 = 15.0;
/// "Looks out of focus" next to its own press: at most this share of the
/// [`Features::score`] of the press's best frame that looks like it (pHash
/// within [`LOOK_ALIKE`] bits; frames where the subject turned or the
/// camera panned aren't comparable) …
pub const SOFT_RELATIVE: f32 = 0.35;
/// … unless its sharpest area is crisp anyway.
pub const SOFT_RELATIVE_CAP: f32 = 60.0;
/// pHash bits within which two frames show the same thing.  In-press
/// neighbours differ by a median of 8; different presses by ~28.
pub const LOOK_ALIKE: u32 = 16;
/// Presses merge into a moment when the next starts within this of the
/// last one's end …
pub const MOMENT_GAP_MS: i64 = 2000;
/// … and the frames where they meet (the last three of one, the first
/// three of the next) look alike: pHash within this.
pub const MOMENT_HASH: u32 = 16;

/// Accidental frames: almost black (a lens cap, a pocket) …
pub const BLACK_P99: u8 = 24;
/// … almost white (pointed at a light) …
pub const BLANK_P1: u8 = 235;
/// … nearly one tone (the 1st and 99th percentiles this close: a wall, a
/// sleeve, the inside of a bag) with nothing sharp …
pub const FLAT_SPAN: u8 = 40;
/// … dark and with nothing sharp in it (night shots that worked keep
/// detail: 60+ even at a median luma of 4) …
pub const DARK_P50: u8 = 12;
pub const DARK_LOCAL: f32 = 12.0;
/// … or nothing sharp anywhere at any exposure (a blurred wall).
pub const FEATURELESS_LOCAL: f32 = 5.0;

/// Why a frame looks out of focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Soft {
    /// Nothing in the frame is sharp.
    Absolute,
    /// Far softer than the best frame of its press that shows the same
    /// thing ([`ShotHint::best_alike`]).
    Relative,
}

/// Why a frame looks accidental.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accident {
    Black,
    Blank,
    /// Nearly one tone, with nothing sharp in it.
    Flat,
    /// Dark, with nothing sharp in it.
    Dark,
    /// Nothing sharp anywhere.
    Featureless,
}

impl Accident {
    pub fn label(self) -> &'static str {
        match self {
            Accident::Black => "almost black",
            Accident::Blank => "almost white",
            Accident::Flat => "almost one tone",
            Accident::Dark => "dark, with nothing sharp",
            Accident::Featureless => "nothing in focus anywhere",
        }
    }
}

/// What the analysis says about one shot.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotHint {
    /// [`Features::score`]: what frames of a moment are ranked by.
    pub score: f32,
    /// [`Features::local`]: the sharpest area.
    pub local: f32,
    /// `score` over the best look-alike frame's in its press (1 when it is
    /// that frame, or is a single shot).
    pub relative: f32,
    /// The frame `relative` is measured against, when another.
    pub best_alike: Option<RecordId>,
    pub soft: Option<Soft>,
    pub accidental: Option<Accident>,
    /// The sharpest frame of its press (a burst of two or more).  Per press
    /// rather than per moment: across presses the framing changes, and
    /// that moves the score as much as focus.  On 74 bursts the user kept
    /// some frames of, the press's sharpest was kept in 62 % (chance 29 %),
    /// the moment's in 54 %.
    pub sharpest: bool,
    /// (1-based place, frames ranked) in its press by `score`.
    pub rank: Option<(u16, u16)>,
}

impl ShotHint {
    /// A possible problem: soft or accidental.
    pub fn problem(&self) -> bool {
        self.soft.is_some() || self.accidental.is_some()
    }

    /// Looks out of focus and isn't an accident (an accidental frame is
    /// soft too, but is shown and counted as accidental).
    pub fn soft_only(&self) -> bool {
        self.soft.is_some() && self.accidental.is_none()
    }
}

/// The one hint a photo's plate shows in the grid (the most pressing, when
/// several apply).  Suggestions only: the plate goes once the photo is
/// pruned or marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Plate {
    Accidental,
    Soft,
    /// A copy the duplicate rule suggests pruning.
    Copy,
    /// The sharpest frame of its burst.
    Sharpest,
}

/// The filter bar's hints choice: only photos with this hint.  A RAW+JPG
/// pair passes when either half does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintFilter {
    /// Looks out of focus, accidental, or a copy suggested for pruning.
    Problems,
    Soft,
    Accidental,
    /// Every file with a copy: the one to keep and the copies.
    Duplicates,
    Sharpest,
}

impl HintFilter {
    pub const ALL: [HintFilter; 5] = [HintFilter::Problems, HintFilter::Soft, HintFilter::Accidental, HintFilter::Duplicates, HintFilter::Sharpest];

    pub fn index(self) -> usize {
        HintFilter::ALL.iter().position(|&f| f == self).unwrap_or(0)
    }

    /// Does `id` (either half of a pair) have this hint?
    pub fn accepts(self, h: &Hints, id: RecordId) -> bool {
        let hint = h.get(id);
        match self {
            HintFilter::Problems => hint.is_some_and(ShotHint::problem) || h.suggested_copy(id),
            HintFilter::Soft => hint.is_some_and(ShotHint::soft_only),
            HintFilter::Accidental => hint.is_some_and(|x| x.accidental.is_some()),
            HintFilter::Duplicates => h.duplicate_of(id).is_some(),
            HintFilter::Sharpest => hint.is_some_and(|x| x.sharpest),
        }
    }
}

pub type MomentId = u32;

/// Presses of one scene (decision 3): consecutive presses merged when they
/// follow within [`MOMENT_GAP_MS`] and look alike.  One press of two or
/// more frames is a moment too, so after an analysis every burst has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    /// Frames in capture order, one record per shot.
    pub frames: Vec<RecordId>,
    /// Where each press starts in `frames` (the first is 0).
    pub presses: Vec<usize>,
    /// Measured frames by score, best first, across its presses (for
    /// choosing which frames to show first; see [`ShotHint::sharpest`]).
    pub ranking: Vec<RecordId>,
}

/// Counts for a status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub shots: usize,
    pub measured: usize,
    pub soft: usize,
    /// Soft and not accidental (what the hints count as out of focus).
    pub soft_only: usize,
    pub accidental: usize,
    /// Bursts whose sharpest frame is known.
    pub sharpest: usize,
    pub moments: usize,
    /// Moments of more than one press.
    pub merged: usize,
    pub duplicate_groups: usize,
    /// Files suggested for pruning as copies.
    pub duplicate_copies: usize,
}

/// Records to mark, as a bulk action would (decision 5).  Prunes are shot
/// representatives (a pair's JPG); marks applied through
/// `App::refresh_prune` / `refresh_mark` reach the other half of a pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub mark: Vec<RecordId>,
    pub prune: Vec<RecordId>,
}

const NONE: u32 = u32::MAX;

/// The analysis' suggestions for one state of the collection.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    generation: Option<u64>,
    revision: u64,
    /// Per record id: its shot, or NONE.
    shot_of: Vec<u32>,
    /// Per shot: its representative record.
    reps: Vec<RecordId>,
    /// Per shot: hint (None when not measured).
    hints: Vec<Option<ShotHint>>,
    /// Per shot: moment, or NONE.
    moment_of: Vec<u32>,
    moments: Vec<Moment>,
    duplicates: Vec<DuplicateGroup>,
    /// Per record id: (duplicate group, suggested for pruning), or NONE.
    dup_of: Vec<(u32, bool)>,
}

/// One shot while deriving.
struct Shot<'a> {
    rep: RecordId,
    members: Vec<RecordId>,
    /// Capture time in ms (with sub-seconds), when known.
    millis: Option<i64>,
    sequence: Option<u16>,
    exact_capture: bool,
    camera: u32,
    folder: PathBuf,
    features: Option<&'a Features>,
    file_type: FileType,
}

impl Hints {
    /// Work out the suggestions.  `features` gives a record's measurements
    /// (asked for each shot's representative); `exact` lists identical
    /// files as the analysis found them.  `revision` identifies the results
    /// these come from (see [`Hints::is_current`]).
    pub fn derive<'a>(
        coll: &Collection,
        bursts: &Bursts,
        features: impl Fn(RecordId, &PhotoRecord) -> Option<&'a Features>,
        exact: &[Vec<PathBuf>],
        revision: u64,
    ) -> Hints {
        let _t = crate::profile::span("hints", "derive");
        let max_id = coll.ids().iter().copied().max().map_or(0, |m| m as usize + 1);
        let mut out = Hints {
            generation: Some(coll.generation()),
            revision,
            shot_of: vec![NONE; max_id],
            dup_of: vec![(NONE, false); max_id],
            ..Hints::default()
        };
        // Shots: a pair's JPG stands for both halves.
        let mut shots: Vec<Shot<'_>> = Vec::new();
        for (id, r) in coll.iter_ids() {
            let pair = coll.find_pair(id);
            if r.file_type == FileType::Raw && pair.is_some() {
                continue;
            }
            let other = pair.and_then(|p| coll.get(p));
            let time = r.capture_time.or(other.and_then(|o| o.capture_time));
            let meta = if r.shot.subsec_ms.is_none() && r.shot.sequence.is_none() { other.map_or(r.shot, |o| o.shot) } else { r.shot };
            let millis = time.map(|t| t.and_utc().timestamp_millis() + meta.subsec_ms.unwrap_or(0) as i64);
            let mut members = vec![id];
            members.extend(pair);
            for &m in &members {
                if let Some(slot) = out.shot_of.get_mut(m as usize) {
                    *slot = shots.len() as u32;
                }
            }
            shots.push(Shot {
                rep: id,
                members,
                millis,
                sequence: meta.sequence,
                exact_capture: meta.subsec_ms.is_some() || meta.sequence.is_some(),
                camera: meta.camera,
                folder: canonical_parent(&r.path),
                features: features(id, r),
                file_type: r.file_type,
            });
        }
        out.reps = shots.iter().map(|s| s.rep).collect();
        out.moment_of = vec![NONE; shots.len()];
        out.duplicates = duplicate_groups(coll, &shots, exact);
        let mut suggested: HashSet<RecordId> = HashSet::new();
        for (g, d) in out.duplicates.iter().enumerate() {
            for &id in &d.ids {
                let prune = d.prune.contains(&id);
                if prune {
                    suggested.insert(id);
                }
                // A suggestion for a shot covers both halves of its pair.
                let s = out.shot_of.get(id as usize).copied().unwrap_or(NONE);
                let reach: Vec<RecordId> = if d.kind == DupKind::SameCapture && s != NONE { shots[s as usize].members.clone() } else { vec![id] };
                for m in reach {
                    if let Some(slot) = out.dup_of.get_mut(m as usize) {
                        *slot = (g as u32, prune || slot.1);
                    }
                }
            }
        }
        // Per-frame hints from the measurements alone.
        out.hints = shots
            .iter()
            .map(|s| {
                s.features.map(|f| ShotHint {
                    score: f.score(),
                    local: f.local(),
                    relative: 1.0,
                    best_alike: None,
                    soft: (f.local() < SOFT_LOCAL).then_some(Soft::Absolute),
                    accidental: accident(f),
                    sharpest: false,
                    rank: None,
                })
            })
            .collect();
        out.moments = moments(&shots, bursts);
        // Ranking: by score; a copy suggested for pruning after the one
        // kept; then capture order.
        let order = |hints: &[Option<ShotHint>], a: usize, b: usize| {
            let (ha, hb) = (hints[a].as_ref().expect("measured"), hints[b].as_ref().expect("measured"));
            hb.score
                .total_cmp(&ha.score)
                .then(suggested.contains(&shots[a].rep).cmp(&suggested.contains(&shots[b].rep)))
                .then(a.cmp(&b))
        };
        for (m, moment) in out.moments.iter_mut().enumerate() {
            let idx: Vec<usize> = moment.frames.iter().map(|&id| out.shot_of[id as usize] as usize).collect();
            for &i in &idx {
                out.moment_of[i] = m as u32;
            }
            let mut ranked: Vec<usize> = idx.iter().copied().filter(|&i| out.hints[i].is_some()).collect();
            ranked.sort_by(|&a, &b| order(&out.hints, a, b));
            moment.ranking = ranked.iter().map(|&i| shots[i].rep).collect();
            // Within each press: the framing is the same frame to frame, so
            // a lower score means softer, not other content.  Across presses
            // the subject moves, turns or fills more of the frame, which
            // moves the score as much as focus does.
            for (k, &start) in moment.presses.iter().enumerate() {
                let end = moment.presses.get(k + 1).copied().unwrap_or(idx.len());
                rank_press(&mut out.hints, &shots, &idx[start..end], &order);
            }
        }
        out
    }

    /// Worked out for this state of the collection and these results.
    pub fn is_current(&self, coll: &Collection, revision: u64) -> bool {
        self.generation == Some(coll.generation()) && self.revision == revision
    }

    /// The collection generation these were worked out for.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Nothing worked out (no results, or none yet).
    pub fn is_empty(&self) -> bool {
        self.reps.is_empty()
    }

    fn shot(&self, id: RecordId) -> Option<usize> {
        self.shot_of.get(id as usize).copied().filter(|&s| s != NONE).map(|s| s as usize)
    }

    /// The hint for `id`'s shot (either half of a pair), once measured.
    pub fn get(&self, id: RecordId) -> Option<&ShotHint> {
        self.hints.get(self.shot(id)?)?.as_ref()
    }

    /// The record standing for `id`'s shot (the JPG of a pair).
    pub fn rep(&self, id: RecordId) -> Option<RecordId> {
        self.reps.get(self.shot(id)?).copied()
    }

    pub fn moment_of(&self, id: RecordId) -> Option<MomentId> {
        self.moment_of.get(self.shot(id)?).copied().filter(|&m| m != NONE)
    }

    pub fn moment(&self, m: MomentId) -> Option<&Moment> {
        self.moments.get(m as usize)
    }

    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    /// `ids` by score, best first; frames not measured keep their order
    /// after the rest (Review burst's initial panes: the top four).
    pub fn ranked(&self, ids: &[RecordId]) -> Vec<RecordId> {
        let mut v: Vec<(usize, RecordId, Option<f32>)> = ids.iter().enumerate().map(|(i, &id)| (i, id, self.get(id).map(|h| h.score))).collect();
        v.sort_by(|a, b| match (a.2, b.2) {
            (Some(x), Some(y)) => y.total_cmp(&x).then(a.0.cmp(&b.0)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        });
        v.into_iter().map(|x| x.1).collect()
    }

    pub fn duplicates(&self) -> &[DuplicateGroup] {
        &self.duplicates
    }

    /// `id`'s duplicate group, and whether it is a copy suggested for
    /// pruning.  A same-capture group covers both halves of a pair.
    pub fn duplicate_of(&self, id: RecordId) -> Option<(&DuplicateGroup, bool)> {
        let &(g, prune) = self.dup_of.get(id as usize)?;
        (g != NONE).then(|| (&self.duplicates[g as usize], prune))
    }

    /// `id` is a copy the duplicate rule suggests pruning.
    pub fn suggested_copy(&self, id: RecordId) -> bool {
        self.duplicate_of(id).is_some_and(|(_, prune)| prune)
    }

    /// What `id`'s plate shows: accidental first, then soft, a copy, the
    /// sharpest of its burst.  The caller hides it on decided photos.
    pub fn plate(&self, id: RecordId) -> Option<Plate> {
        let h = self.get(id);
        if h.is_some_and(|h| h.accidental.is_some()) {
            Some(Plate::Accidental)
        } else if h.is_some_and(|h| h.soft.is_some()) {
            Some(Plate::Soft)
        } else if self.suggested_copy(id) {
            Some(Plate::Copy)
        } else if h.is_some_and(|h| h.sharpest) {
            Some(Plate::Sharpest)
        } else {
            None
        }
    }

    /// `id`'s moment, when it joins presses of one scene (two or more).
    pub fn joined_moment(&self, id: RecordId) -> Option<MomentId> {
        self.moment_of(id).filter(|&m| self.moments.get(m as usize).is_some_and(|m| m.presses.len() > 1))
    }

    /// Shots whose own file is a copy suggested for pruning.
    pub fn copies(&self) -> usize {
        self.reps.iter().filter(|&&r| self.suggested_copy(r)).count()
    }

    /// Shots measured and shots in all (how far an analysis got).
    pub fn coverage(&self) -> (usize, usize) {
        (self.hints.iter().flatten().count(), self.reps.len())
    }

    pub fn summary(&self) -> Summary {
        let measured: Vec<&ShotHint> = self.hints.iter().flatten().collect();
        Summary {
            shots: self.reps.len(),
            measured: measured.len(),
            soft: measured.iter().filter(|h| h.soft.is_some()).count(),
            soft_only: measured.iter().filter(|h| h.soft_only()).count(),
            accidental: measured.iter().filter(|h| h.accidental.is_some()).count(),
            sharpest: measured.iter().filter(|h| h.sharpest).count(),
            moments: self.moments.len(),
            merged: self.moments.iter().filter(|m| m.presses.len() > 1).count(),
            duplicate_groups: self.duplicates.len(),
            duplicate_copies: self.duplicates.iter().map(|d| d.prune.len()).sum(),
        }
    }

    // ── bulk actions (decision 5): what each would mark ──────────────────

    /// *Keep this, prune the rest*: `chosen` is marked to keep, and every
    /// frame of `frames` (a burst or moment) still to review is pruned.
    /// Frames already pruned or marked are left as they are.
    pub fn keep_this_prune_rest(&self, coll: &Collection, frames: &[RecordId], chosen: RecordId) -> Plan {
        let chosen_shot = self.shot(chosen);
        let mark = coll.get(chosen).filter(|r| !r.is_marked).map(|_| vec![chosen]).unwrap_or_default();
        let prune = frames
            .iter()
            .copied()
            .filter(|&id| id != chosen && (chosen_shot.is_none() || self.shot(id) != chosen_shot))
            .filter(|&id| to_review(coll, id))
            .collect();
        Plan { mark, prune }
    }

    /// *Prune accidental frames…*: shots flagged accidental, still to
    /// review, among `scope` (any half of a pair stands for its shot).
    pub fn prune_accidental(&self, coll: &Collection, scope: &[RecordId]) -> Plan {
        self.prune_where(coll, scope, |h| h.accidental.is_some())
    }

    /// *Prune all soft frames…*: shots that look out of focus (accidental
    /// ones included), still to review, among `scope`.
    pub fn prune_soft(&self, coll: &Collection, scope: &[RecordId]) -> Plan {
        self.prune_where(coll, scope, |h| h.soft.is_some() || h.accidental.is_some())
    }

    /// Shots the duplicate rule suggests pruning, still to review, among
    /// `scope`: those whose every file is a copy to prune.  (A prune
    /// reaches both halves of a pair, so a copied JPG whose RAW is the only
    /// one of its kind is left alone.)
    pub fn prune_duplicates(&self, coll: &Collection, scope: &[RecordId]) -> Plan {
        let mut prune: Vec<RecordId> = Vec::new();
        let mut seen = HashSet::new();
        for &id in scope {
            let Some(s) = self.shot(id) else { continue };
            if !seen.insert(s) {
                continue;
            }
            let rep = self.reps[s];
            let members = std::iter::once(rep).chain(coll.find_pair(rep));
            let all_copies = members.into_iter().all(|m| self.duplicate_of(m).is_some_and(|(_, prune)| prune));
            if all_copies && to_review(coll, rep) {
                prune.push(rep);
            }
        }
        Plan { mark: Vec::new(), prune }
    }

    fn prune_where(&self, coll: &Collection, scope: &[RecordId], want: impl Fn(&ShotHint) -> bool) -> Plan {
        let mut seen = HashSet::new();
        let mut prune = Vec::new();
        for &id in scope {
            let Some(s) = self.shot(id) else { continue };
            if !seen.insert(s) {
                continue;
            }
            let rep = self.reps[s];
            if self.hints[s].as_ref().is_some_and(&want) && to_review(coll, rep) {
                prune.push(rep);
            }
        }
        Plan { mark: Vec::new(), prune }
    }
}

/// Where the grid splices one press to the next of the same moment: for
/// each tile, whether it joins the tile after it.  `moment` gives a tile's
/// moment when that joins several presses ([`Hints::joined_moment`]);
/// frames of one strip are joined already and aren't spliced.
pub fn moment_links(tiles: &[RecordId], runs: &[Option<super::bursts::RunMark>], moment: impl Fn(RecordId) -> Option<MomentId>) -> Vec<bool> {
    let ms: Vec<Option<MomentId>> = tiles.iter().map(|&id| moment(id)).collect();
    (0..tiles.len())
        .map(|i| {
            let Some(next) = ms.get(i + 1) else { return false };
            let same_strip = match (runs.get(i).copied().flatten(), runs.get(i + 1).copied().flatten()) {
                (Some(a), Some(b)) => a.burst == b.burst && !a.last,
                _ => false,
            };
            ms[i].is_some() && ms[i] == *next && !same_strip
        })
        .collect()
}

/// Rank one press's frames, mark its sharpest, and flag frames far softer
/// than the best frame of the press that looks like them.
fn rank_press(hints: &mut [Option<ShotHint>], shots: &[Shot<'_>], press: &[usize], order: &dyn Fn(&[Option<ShotHint>], usize, usize) -> std::cmp::Ordering) {
    let mut ranked: Vec<usize> = press.iter().copied().filter(|&i| hints[i].is_some()).collect();
    if ranked.len() < 2 {
        return;
    }
    ranked.sort_by(|&a, &b| order(hints, a, b));
    let n = ranked.len().min(u16::MAX as usize) as u16;
    for (place, &i) in ranked.iter().enumerate() {
        let h = hints[i].as_mut().expect("measured");
        h.rank = Some(((place + 1).min(u16::MAX as usize) as u16, n));
        h.sharpest = place == 0;
    }
    for &i in &ranked {
        let fi = shots[i].features.expect("measured");
        // `ranked` is best first: the first frame that looks alike.
        let best = ranked
            .iter()
            .copied()
            .find(|&j| quality::distance(fi.phash, shots[j].features.expect("measured").phash) <= LOOK_ALIKE)
            .unwrap_or(i);
        let best_score = hints[best].as_ref().expect("measured").score;
        let h = hints[i].as_mut().expect("measured");
        h.relative = if best_score > 0.0 { (h.score / best_score).min(1.0) } else { 1.0 };
        h.best_alike = (best != i).then_some(shots[best].rep);
        if h.soft.is_none() && h.relative <= SOFT_RELATIVE && h.local < SOFT_RELATIVE_CAP {
            h.soft = Some(Soft::Relative);
        }
    }
}

/// Neither pruned nor marked (either half of a pair).
fn to_review(coll: &Collection, id: RecordId) -> bool {
    let decided = |id: RecordId| coll.get(id).is_some_and(|r| r.is_pruned || r.is_marked);
    coll.get(id).is_some() && !decided(id) && !coll.find_pair(id).is_some_and(decided)
}

fn accident(f: &Features) -> Option<Accident> {
    let local = f.local();
    if f.p99() <= BLACK_P99 {
        Some(Accident::Black)
    } else if f.p1() >= BLANK_P1 {
        Some(Accident::Blank)
    } else if f.p99().saturating_sub(f.p1()) <= FLAT_SPAN && local < SOFT_LOCAL {
        Some(Accident::Flat)
    } else if f.p50() <= DARK_P50 && local < DARK_LOCAL {
        Some(Accident::Dark)
    } else if local < FEATURELESS_LOCAL {
        Some(Accident::Featureless)
    } else {
        None
    }
}

/// Presses in capture order within each folder and camera, merged into
/// moments.  Shots with no capture time are in none.
fn moments(shots: &[Shot<'_>], bursts: &Bursts) -> Vec<Moment> {
    let mut groups: HashMap<(&PathBuf, u32), Vec<usize>> = HashMap::new();
    for (i, s) in shots.iter().enumerate() {
        if s.millis.is_some() {
            groups.entry((&s.folder, s.camera)).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = groups.into_values().collect();
    for g in &mut groups {
        g.sort_by_key(|&i| (shots[i].millis, i));
    }
    groups.sort_by_key(|g| shots[g[0]].millis);
    let mut out = Vec::new();
    for g in groups {
        // Presses: runs of one burst; every other shot is a press of one.
        let mut presses: Vec<Vec<usize>> = Vec::new();
        for &i in &g {
            let b = bursts.of(shots[i].rep);
            let same = b.is_some() && presses.last().is_some_and(|p| bursts.of(shots[p[p.len() - 1]].rep) == b);
            if same {
                presses.last_mut().expect("a press").push(i);
            } else {
                presses.push(vec![i]);
            }
        }
        let mut current: Vec<Vec<usize>> = Vec::new();
        let flush = |current: &mut Vec<Vec<usize>>, out: &mut Vec<Moment>| {
            let frames: Vec<usize> = current.iter().flatten().copied().collect();
            if two_captures(shots, &frames) {
                let mut starts = Vec::new();
                let mut at = 0;
                for p in current.iter() {
                    starts.push(at);
                    at += p.len();
                }
                out.push(Moment { frames: frames.iter().map(|&i| shots[i].rep).collect(), presses: starts, ranking: Vec::new() });
            }
            current.clear();
        };
        for p in presses {
            let merge = current.last().is_some_and(|last| look_alike_presses(shots, last, &p));
            if !merge {
                flush(&mut current, &mut out);
            }
            current.push(p);
        }
        flush(&mut current, &mut out);
    }
    out
}

/// Does press `b` (just after `a`) show the same scene?  It starts within
/// [`MOMENT_GAP_MS`] of `a`'s end, and the frames where they meet look
/// alike.  Unmeasured frames never merge.
fn look_alike_presses(shots: &[Shot<'_>], a: &[usize], b: &[usize]) -> bool {
    let (Some(end), Some(start)) = (shots[a[a.len() - 1]].millis, shots[b[0]].millis) else { return false };
    if start - end > MOMENT_GAP_MS {
        return false;
    }
    let tail = &a[a.len().saturating_sub(3)..];
    let head = &b[..b.len().min(3)];
    tail.iter().any(|&x| {
        head.iter().any(|&y| match (shots[x].features, shots[y].features) {
            (Some(fx), Some(fy)) => quality::distance(fx.phash, fy.phash) <= MOMENT_HASH,
            _ => false,
        })
    })
}

/// The same capture: one frame in two files (a day exported twice).  Only
/// trusted with sub-seconds or a sequence number.
fn same_capture(a: &Shot<'_>, b: &Shot<'_>) -> bool {
    a.millis.is_some() && a.millis == b.millis && a.sequence == b.sequence && a.camera == b.camera && a.exact_capture && b.exact_capture
}

/// At least two different captures among `frames` (copies count once).
fn two_captures(shots: &[Shot<'_>], frames: &[usize]) -> bool {
    frames.iter().any(|&i| !same_capture(&shots[frames[0]], &shots[i]))
}

/// Identical files, then same-capture shots not already covered.
fn duplicate_groups(coll: &Collection, shots: &[Shot<'_>], exact: &[Vec<PathBuf>]) -> Vec<DuplicateGroup> {
    let mut out = duplicates::exact_groups(coll, exact);
    let in_exact: HashMap<RecordId, usize> = out.iter().enumerate().flat_map(|(g, d)| d.ids.iter().map(move |&id| (id, g))).collect();
    let mut by_capture: HashMap<(i64, Option<u16>, u32, FileType), Vec<usize>> = HashMap::new();
    for (i, s) in shots.iter().enumerate() {
        if let (Some(ms), true, true) = (s.millis, s.exact_capture, s.camera != 0) {
            by_capture.entry((ms, s.sequence, s.camera, s.file_type)).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = by_capture.into_values().filter(|g| g.len() >= 2).collect();
    groups.sort();
    for g in groups {
        // Only frames that look the same (measured), so two bodies of one
        // model firing in the same millisecond aren't called copies.
        let Some(first) = g.iter().find_map(|&i| shots[i].features) else { continue };
        let alike: Vec<usize> = g
            .iter()
            .copied()
            .filter(|&i| shots[i].features.is_some_and(|f| quality::distance(f.phash, first.phash) <= duplicates::SAME_CAPTURE_HASH))
            .collect();
        if alike.len() < 2 {
            continue;
        }
        // Copies already found identical are reported once.
        let groups_hit: HashSet<Option<usize>> = alike.iter().map(|&i| in_exact.get(&shots[i].rep).copied()).collect();
        if groups_hit.len() == 1 && !groups_hit.contains(&None) {
            continue;
        }
        let ids: Vec<RecordId> = alike.iter().map(|&i| shots[i].rep).collect();
        if let Some(d) = duplicates::group(coll, DupKind::SameCapture, &ids) {
            out.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::quality::tests::{Img, feats, texture};
    use crate::model::record::{MTime, ShotMeta};
    use chrono::NaiveDateTime;
    use std::path::Path;

    /// A record shot `ms` milliseconds past 10:00:00, Sony sequence `seq`.
    fn rec(path: &str, ms: i64, seq: Option<u16>) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        let base = NaiveDateTime::parse_from_str("2026-09-19 10:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let t = base + chrono::Duration::seconds(ms / 1000);
        let mut r = PhotoRecord::new(p, ft, 1000, MTime::from_unix(1_700_000_000, 0), Some(t));
        r.shot = ShotMeta { subsec_ms: Some((ms % 1000) as u16), sequence: seq, focus: None, camera: 7 };
        r
    }

    /// Features for a scene `seed` blurred by `blur`.
    fn scene(seed: u32, blur: usize) -> Features {
        feats(&texture(192, 128, seed).blur(blur))
    }

    struct Shoot {
        coll: Collection,
        bursts: Bursts,
        feats: HashMap<PathBuf, Features>,
    }

    impl Shoot {
        fn new(recs: Vec<(PhotoRecord, Option<Features>)>) -> Shoot {
            let mut coll = Collection::new();
            let mut feats = HashMap::new();
            for (r, f) in recs {
                if let Some(f) = f {
                    feats.insert(r.path.clone(), f);
                }
                coll.add(r);
            }
            coll.build_pairs();
            let bursts = Bursts::find(&coll);
            Shoot { coll, bursts, feats }
        }
        fn hints(&self, exact: &[Vec<PathBuf>]) -> Hints {
            Hints::derive(&self.coll, &self.bursts, |_, r| self.feats.get(&r.path), exact, 1)
        }
        fn id(&self, p: &str) -> RecordId {
            self.coll.id_of(Path::new(p)).unwrap()
        }
    }

    #[test]
    fn the_sharpest_frame_and_the_soft_ones() {
        // One press of four frames of one scene: two sharp, one a little
        // soft, one far softer.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 1))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 0))),
            (rec("/s/a3.JPG", 200, Some(3)), Some(scene(1, 2))),
            (rec("/s/a4.JPG", 300, Some(4)), Some(scene(1, 6))),
        ]);
        let h = s.hints(&[]);
        let get = |n: &str| h.get(s.id(&format!("/s/{n}.JPG"))).unwrap().clone();
        assert!(get("a2").sharpest && !get("a1").sharpest);
        assert_eq!(get("a2").rank, Some((1, 4)));
        assert_eq!(get("a4").rank, Some((4, 4)));
        assert_eq!(get("a2").relative, 1.0);
        assert!(get("a4").relative <= SOFT_RELATIVE, "{}", get("a4").relative);
        assert_eq!(get("a4").soft, Some(Soft::Relative));
        assert_eq!(get("a4").best_alike, Some(s.id("/s/a2.JPG")));
        assert_eq!(get("a1").soft, None);
        let m = h.moment_of(s.id("/s/a1.JPG")).unwrap();
        assert_eq!(h.moment(m).unwrap().ranking[0], s.id("/s/a2.JPG"));
        assert_eq!(h.ranked(&[s.id("/s/a4.JPG"), s.id("/s/a1.JPG"), s.id("/s/a2.JPG")])[0], s.id("/s/a2.JPG"));
        assert_eq!(h.summary().soft, 1);
        assert!(h.is_current(&s.coll, 1) && !h.is_current(&s.coll, 2));
    }

    #[test]
    fn softness_and_the_sharpest_are_judged_within_a_press() {
        // Two presses of one scene, merged into a moment.  The second is
        // softer throughout (as when the subject comes closer and fills the
        // frame): its frames aren't flagged for that, and it has its own
        // sharpest frame.  The moment's ranking still spans both.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 1))),
            (rec("/s/b1.JPG", 600, Some(1)), Some(scene(1, 4))),
            (rec("/s/b2.JPG", 700, Some(2)), Some(scene(1, 3))),
        ]);
        let h = s.hints(&[]);
        let id = |n: &str| s.id(&format!("/s/{n}.JPG"));
        assert_eq!(h.moment_of(id("a1")), h.moment_of(id("b2")), "one moment");
        let b2 = h.get(id("b2")).unwrap();
        assert!(b2.sharpest && h.get(id("a1")).unwrap().sharpest);
        assert_eq!((b2.relative, b2.best_alike, b2.soft), (1.0, None, None));
        assert_eq!(h.get(id("b1")).unwrap().rank, Some((2, 2)));
        assert_ne!(h.get(id("b1")).unwrap().soft, Some(Soft::Relative));
        let m = h.moment(h.moment_of(id("a1")).unwrap()).unwrap();
        assert_eq!(m.ranking.first(), Some(&id("a1")));
        assert_eq!(m.ranking.len(), 4);
    }

    #[test]
    fn a_soft_frame_is_only_compared_with_frames_that_look_like_it() {
        // The camera turned to another scene mid-press: that frame is
        // softer than the first scene's best but isn't flagged, because
        // there is nothing like it to compare with.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 0))),
            (rec("/s/a3.JPG", 200, Some(3)), Some(scene(9, 5))),
        ]);
        let h = s.hints(&[]);
        let a3 = h.get(s.id("/s/a3.JPG")).unwrap();
        assert_eq!((a3.relative, a3.best_alike, a3.soft), (1.0, None, None));
    }

    #[test]
    fn nothing_sharp_is_soft_anywhere_and_dark_featureless_frames_are_accidents() {
        let soft_single = feats(&texture(192, 128, 3).blur(9));
        assert!(soft_single.local() < SOFT_LOCAL);
        let black = feats(&Img::from_fn(192, 128, |x, y| ((x + y) % 7) as f32));
        let white = feats(&Img::from_fn(192, 128, |x, _| 245.0 + (x % 5) as f32));
        // A dim, even wall: a narrow span of tones, soft.
        let wall = feats(&texture(192, 128, 5).blur(8).darker(0.15));
        assert!(wall.p99() - wall.p1() <= FLAT_SPAN, "{:?}", wall.levels);
        // Dark and blurred, with a soft lamp in a corner.
        let mut dark = texture(192, 128, 3).blur(6).darker(0.05);
        dark.paste(&Img::from_fn(192, 128, |_, _| 120.0).blur(0), 0, 0, 40, 30);
        let dark_soft = feats(&dark.blur(8));
        let dark_levels = dark_soft.levels;
        // A dark night shot that worked: a lit, sharp subject on black.
        let mut night = texture(192, 128, 3).darker(0.03);
        night.paste(&texture(192, 128, 3), 70, 40, 120, 80);
        let dark_sharp = feats(&night);
        let s = Shoot::new(vec![
            (rec("/s/soft.JPG", 0, Some(0)), Some(soft_single)),
            (rec("/s/black.JPG", 10_000, Some(0)), Some(black)),
            (rec("/s/white.JPG", 20_000, Some(0)), Some(white)),
            (rec("/s/wall.JPG", 25_000, Some(0)), Some(wall)),
            (rec("/s/dark.JPG", 30_000, Some(0)), Some(dark_soft)),
            (rec("/s/night.JPG", 40_000, Some(0)), Some(dark_sharp.clone())),
            (rec("/s/notyet.JPG", 50_000, Some(0)), None),
        ]);
        assert!(dark_sharp.p50() <= DARK_P50, "the night shot is dark: {}", dark_sharp.p50());
        let h = s.hints(&[]);
        let get = |n: &str| h.get(s.id(&format!("/s/{n}.JPG"))).cloned();
        assert_eq!(get("soft").unwrap().soft, Some(Soft::Absolute));
        assert_eq!(get("black").unwrap().accidental, Some(Accident::Black));
        assert_eq!(get("white").unwrap().accidental, Some(Accident::Blank));
        assert_eq!(get("wall").unwrap().accidental, Some(Accident::Flat));
        let dark = get("dark").unwrap();
        assert_eq!(dark.accidental, Some(Accident::Dark), "local {} levels {:?}", dark.local, dark_levels);
        let night = get("night").unwrap();
        assert_eq!((night.accidental, night.soft), (None, None), "local {}", night.local);
        assert_eq!(get("notyet"), None);
        // Singles have no moment and no rank.
        assert_eq!(h.moment_of(s.id("/s/soft.JPG")), None);
        assert_eq!(get("soft").unwrap().rank, None);
        let sum = h.summary();
        assert_eq!((sum.shots, sum.measured, sum.accidental), (7, 6, 4));
    }

    #[test]
    fn presses_of_one_scene_merge_into_a_moment() {
        // Press a (scene 1), press b of the same scene 1.5 s later, press c
        // of scene 1 but 5 s later, press d of another scene right after.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 1))),
            (rec("/s/b1.JPG", 1600, Some(1)), Some(scene(1, 1))),
            (rec("/s/b2.JPG", 1700, Some(2)), Some(scene(1, 0))),
            (rec("/s/single.JPG", 2500, Some(0)), Some(scene(1, 0))),
            (rec("/s/c1.JPG", 7500, Some(1)), Some(scene(1, 0))),
            (rec("/s/c2.JPG", 7600, Some(2)), Some(scene(1, 0))),
            (rec("/s/d1.JPG", 7900, Some(1)), Some(scene(5, 0))),
            (rec("/s/d2.JPG", 8000, Some(2)), Some(scene(5, 0))),
        ]);
        let h = s.hints(&[]);
        let m = |n: &str| h.moment_of(s.id(&format!("/s/{n}.JPG")));
        assert_eq!(m("a1"), m("b2"));
        assert_eq!(m("a1"), m("single"), "a single shot can join the moment");
        assert_ne!(m("a1"), m("c1"));
        assert_ne!(m("c1"), m("d1"));
        let first = h.moment(m("a1").unwrap()).unwrap();
        assert_eq!(first.presses, vec![0, 2, 4]);
        assert_eq!(first.frames.len(), 5);
        assert_eq!(h.summary().moments, 3);
        assert_eq!(h.summary().merged, 1);
        // Unmeasured presses don't merge (and a lone single is no moment).
        let s2 = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), None),
            (rec("/s/a2.JPG", 100, Some(2)), None),
            (rec("/s/b1.JPG", 600, Some(1)), None),
            (rec("/s/b2.JPG", 700, Some(2)), None),
            (rec("/s/x.JPG", 9000, Some(0)), Some(scene(1, 0))),
        ]);
        let h2 = s2.hints(&[]);
        assert_eq!(h2.moments().len(), 2);
        assert_eq!(h2.moment_of(s2.id("/s/x.JPG")), None);
    }

    #[test]
    fn moments_stay_within_a_folder_and_camera_and_a_pair_is_one_frame() {
        let mut other = rec("/s/b1.JPG", 300, Some(1));
        other.shot.camera = 8;
        let s = Shoot::new(vec![
            (rec("/s/RAW/a1.ARW", 0, Some(1)), None),
            (rec("/s/JPG/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/RAW/a2.ARW", 100, Some(2)), None),
            (rec("/s/JPG/a2.JPG", 100, Some(2)), Some(scene(1, 8))),
            (other, Some(scene(1, 0))),
            (rec("/s/b2.JPG", 400, Some(2)), Some(scene(1, 0))),
            (rec("/t/c1.JPG", 300, Some(1)), Some(scene(1, 0))),
            (rec("/t/c2.JPG", 400, Some(2)), Some(scene(1, 0))),
        ]);
        let h = s.hints(&[]);
        let raw = s.id("/s/RAW/a1.ARW");
        let jpg = s.id("/s/JPG/a1.JPG");
        assert_eq!(h.rep(raw), Some(jpg));
        assert_eq!(h.get(raw), h.get(jpg));
        // b2 follows on the same body (b1, from another, doesn't break it).
        let m = h.moment(h.moment_of(raw).unwrap()).unwrap();
        assert_eq!(m.frames, vec![jpg, s.id("/s/JPG/a2.JPG"), s.id("/s/b2.JPG")]);
        assert_eq!(m.presses, vec![0, 2]);
        assert_eq!(h.moment_of(s.id("/s/b1.JPG")), None, "another body's single frame");
        assert_ne!(h.moment_of(s.id("/t/c1.JPG")), h.moment_of(jpg));
        // The soft half-burst frame: both halves flagged.
        assert!(h.get(s.id("/s/RAW/a2.ARW")).unwrap().soft.is_some());
    }

    #[test]
    fn exported_twice_copies_are_duplicates_not_moments() {
        // A single shot and its identical copy, and a two-frame press
        // exported twice (interleaved in capture order).
        let s = Shoot::new(vec![
            (rec("/s/20260523_0846_0001.JPG", 0, Some(0)), Some(scene(1, 0))),
            (rec("/s/20260523_0846_3356.JPG", 0, Some(0)), Some(scene(1, 0))),
            (rec("/s/20260523_0846_0002.JPG", 5000, Some(1)), Some(scene(2, 0))),
            (rec("/s/20260523_0846_3357.JPG", 5000, Some(1)), Some(scene(2, 0))),
            (rec("/s/20260523_0846_0003.JPG", 5100, Some(2)), Some(scene(2, 3))),
            (rec("/s/20260523_0846_3358.JPG", 5100, Some(2)), Some(scene(2, 3))),
        ]);
        let exact: Vec<Vec<PathBuf>> = [(1, 3356), (2, 3357), (3, 3358)]
            .iter()
            .map(|(a, b)| vec![format!("/s/20260523_0846_{b:04}.JPG").into(), format!("/s/20260523_0846_{a:04}.JPG").into()])
            .collect();
        let h = s.hints(&exact);
        // Copies alone make no moment; the press does, with its copies.
        assert_eq!(h.moment_of(s.id("/s/20260523_0846_0001.JPG")), None);
        let m = h.moment(h.moment_of(s.id("/s/20260523_0846_0002.JPG")).unwrap()).unwrap();
        assert_eq!(m.frames.len(), 4);
        // Copies tie on score: the kept copy ranks first and is the sharpest.
        let sharpest = s.id("/s/20260523_0846_0002.JPG");
        assert_eq!(m.ranking[0], sharpest);
        assert!(h.get(sharpest).unwrap().sharpest);
        // Three exact groups, no same-capture repeats; the later export is
        // the one to prune.
        assert_eq!(h.duplicates().len(), 3);
        assert!(h.duplicates().iter().all(|d| d.kind == DupKind::Exact));
        let (g, prune) = h.duplicate_of(s.id("/s/20260523_0846_3356.JPG")).unwrap();
        assert!(prune);
        assert_eq!(g.keep, s.id("/s/20260523_0846_0001.JPG"));
        assert_eq!(h.duplicate_of(g.keep).map(|x| x.1), Some(false));
        let all: Vec<RecordId> = s.coll.ids().to_vec();
        let plan = h.prune_duplicates(&s.coll, &all);
        assert_eq!(plan.prune.len(), 3);
        assert_eq!(h.summary().duplicate_copies, 3);
    }

    #[test]
    fn a_copy_is_only_offered_for_pruning_with_all_of_its_shot() {
        // b.JPG is a copy of a.JPG, but b's RAW is its own: pruning b would
        // prune that RAW too, so b isn't in the bulk list.  c (a copy of
        // a, with no RAW) is.
        let s = Shoot::new(vec![
            (rec("/s/a.JPG", 0, Some(0)), Some(scene(1, 0))),
            (rec("/s/b.JPG", 5000, Some(0)), Some(scene(1, 0))),
            (rec("/s/b.ARW", 5000, Some(0)), None),
            (rec("/s/c.JPG", 9000, Some(0)), Some(scene(1, 0))),
        ]);
        let exact = vec![vec![PathBuf::from("/s/a.JPG"), PathBuf::from("/s/b.JPG"), PathBuf::from("/s/c.JPG")]];
        let h = s.hints(&exact);
        let d = &h.duplicates()[0];
        assert_eq!(d.keep, s.id("/s/a.JPG"));
        assert_eq!(h.duplicate_of(s.id("/s/b.JPG")).map(|x| x.1), Some(true));
        let all: Vec<RecordId> = s.coll.ids().to_vec();
        assert_eq!(h.prune_duplicates(&s.coll, &all).prune, vec![s.id("/s/c.JPG")]);
    }

    #[test]
    fn same_capture_copies_that_differ_in_bytes() {
        // A re-saved JPG ("copy") of a paired frame: same capture, slightly
        // different pixels.  Its RAW twin is not involved; the RAW and its
        // own JPG are never duplicates.
        let f = scene(1, 0);
        let resaved = feats(&texture(192, 128, 1).noisy(2.0, 4));
        assert!(quality::distance(f.phash, resaved.phash) <= duplicates::SAME_CAPTURE_HASH);
        let s = Shoot::new(vec![
            (rec("/s/DSC01.ARW", 0, Some(0)), None),
            (rec("/s/DSC01.JPG", 0, Some(0)), Some(f.clone())),
            (rec("/s/DSC01 copy.JPG", 0, Some(0)), Some(resaved)),
            // Another body of the same model in the same millisecond, of
            // another scene: not a copy.
            (rec("/s/OTHER.JPG", 0, Some(0)), Some(scene(7, 0))),
        ]);
        let h = s.hints(&[]);
        assert_eq!(h.duplicates().len(), 1);
        let d = &h.duplicates()[0];
        assert_eq!(d.kind, DupKind::SameCapture);
        assert_eq!(d.keep, s.id("/s/DSC01.JPG"));
        assert_eq!(d.prune, vec![s.id("/s/DSC01 copy.JPG")]);
        // The kept shot's RAW is in the group's shot, not suggested.
        assert_eq!(h.duplicate_of(s.id("/s/DSC01.ARW")).map(|x| x.1), Some(false));
        assert_eq!(h.duplicate_of(s.id("/s/OTHER.JPG")), None);
    }

    #[test]
    fn bulk_actions_list_only_frames_still_to_review() {
        let mut s = Shoot::new(vec![
            (rec("/s/RAW/a1.ARW", 0, Some(1)), None),
            (rec("/s/JPG/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 6))),
            (rec("/s/a3.JPG", 200, Some(3)), Some(scene(1, 7))),
            (rec("/s/a4.JPG", 300, Some(4)), Some(scene(1, 1))),
            (rec("/s/black.JPG", 9000, Some(0)), Some(feats(&Img::from_fn(192, 128, |_, _| 3.0)))),
        ]);
        // The user already pruned a3 and marked a4.
        let (a3, a4) = (s.id("/s/a3.JPG"), s.id("/s/a4.JPG"));
        s.coll.get_mut(a3).unwrap().is_pruned = true;
        s.coll.get_mut(a4).unwrap().is_marked = true;
        let h = s.hints(&[]);
        let (a1, a2) = (s.id("/s/JPG/a1.JPG"), s.id("/s/a2.JPG"));
        let frames = h.moment(h.moment_of(a1).unwrap()).unwrap().frames.clone();
        // Keep a1 (chosen by its RAW half): prune only a2.
        let plan = h.keep_this_prune_rest(&s.coll, &frames, s.id("/s/RAW/a1.ARW"));
        assert_eq!(plan, Plan { mark: vec![s.id("/s/RAW/a1.ARW")], prune: vec![a2] });
        // Keeping the already-marked a4 marks nothing new.
        assert_eq!(h.keep_this_prune_rest(&s.coll, &frames, a4).mark, Vec::<RecordId>::new());
        let all: Vec<RecordId> = s.coll.ids().to_vec();
        assert_eq!(h.prune_accidental(&s.coll, &all).prune, vec![s.id("/s/black.JPG")]);
        // Soft: a2 (a3 is soft too, but already pruned) and the black frame.
        let soft = h.prune_soft(&s.coll, &all);
        assert_eq!(soft.prune, vec![a2, s.id("/s/black.JPG")]);
        // Scoped to what is shown.
        assert_eq!(h.prune_soft(&s.coll, &[a1, a2]).prune, vec![a2]);
        assert!(soft.mark.is_empty());
    }

    #[test]
    fn hints_follow_a_sort_and_a_trash() {
        let mut s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a1.ARW", 0, Some(1)), None),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 6))),
        ]);
        let h = s.hints(&[]);
        assert!(h.get(s.id("/s/a2.JPG")).unwrap().soft.is_some());
        // Sort: files move into JPG/ and RAW/; measurements follow by
        // canonical key in the app, here by path.
        for (from, to) in [("/s/a1.JPG", "/s/JPG/a1.JPG"), ("/s/a1.ARW", "/s/RAW/a1.ARW"), ("/s/a2.JPG", "/s/JPG/a2.JPG")] {
            let id = s.id(from);
            s.coll.update_path(id, to.into());
            if let Some(f) = s.feats.remove(Path::new(from)) {
                s.feats.insert(to.into(), f);
            }
        }
        s.coll.build_pairs();
        assert!(!h.is_current(&s.coll, 1));
        s.bursts = Bursts::find(&s.coll);
        let h = s.hints(&[]);
        assert!(h.get(s.id("/s/JPG/a2.JPG")).unwrap().soft.is_some());
        assert!(h.get(s.id("/s/RAW/a1.ARW")).unwrap().sharpest);
        // Trash a2: a burst of one is no moment.
        let a2 = s.id("/s/JPG/a2.JPG");
        s.coll.remove(a2);
        s.coll.build_pairs();
        s.bursts = Bursts::find(&s.coll);
        let h = s.hints(&[]);
        assert_eq!(h.moments().len(), 0);
        assert_eq!(h.get(a2), None);
        assert!(!h.get(s.id("/s/JPG/a1.JPG")).unwrap().sharpest);
    }

    #[test]
    fn one_plate_per_photo_the_most_pressing_first() {
        // A press with a sharp frame, a soft one and a black one; a single
        // shot with an identical copy.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 6))),
            (rec("/s/a3.JPG", 200, Some(3)), Some(feats(&Img::from_fn(192, 128, |_, _| 3.0)))),
            (rec("/s/a4.JPG", 300, Some(4)), Some(scene(1, 1))),
            (rec("/s/b.JPG", 9000, Some(0)), Some(scene(2, 0))),
            (rec("/s/b copy.JPG", 9000, Some(0)), Some(scene(2, 0))),
        ]);
        let h = s.hints(&[vec![PathBuf::from("/s/b.JPG"), PathBuf::from("/s/b copy.JPG")]]);
        let plate = |n: &str| h.plate(s.id(&format!("/s/{n}.JPG")));
        assert_eq!(plate("a1"), Some(Plate::Sharpest));
        assert_eq!(plate("a2"), Some(Plate::Soft));
        // Black is soft too, but shows (and counts) as accidental.
        assert_eq!(plate("a3"), Some(Plate::Accidental));
        assert!(h.get(s.id("/s/a3.JPG")).unwrap().soft.is_some() && !h.get(s.id("/s/a3.JPG")).unwrap().soft_only());
        assert_eq!(plate("a4"), None);
        assert_eq!(plate("b copy"), Some(Plate::Copy));
        assert_eq!(plate("b"), None, "the copy to keep has no plate");
        let sum = h.summary();
        assert_eq!((sum.soft, sum.soft_only, sum.accidental), (2, 1, 1));
        assert_eq!(h.coverage(), (6, 6));
    }

    #[test]
    fn the_hints_filter_takes_a_pair_by_either_half() {
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a1.ARW", 0, Some(1)), None),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 6))),
            (rec("/s/a2.ARW", 100, Some(2)), None),
            (rec("/s/black.JPG", 5000, Some(0)), Some(feats(&Img::from_fn(192, 128, |_, _| 3.0)))),
            (rec("/s/c.JPG", 9000, Some(0)), Some(scene(2, 0))),
            (rec("/s/c (1).JPG", 9000, Some(0)), Some(scene(2, 0))),
            (rec("/s/new.JPG", 20_000, Some(0)), None),
        ]);
        let h = s.hints(&[vec![PathBuf::from("/s/c.JPG"), PathBuf::from("/s/c (1).JPG")]]);
        let pass = |f: HintFilter| -> Vec<String> {
            let mut v: Vec<String> = s.coll.iter_ids().filter(|&(id, _)| f.accepts(&h, id)).map(|(_, r)| r.filename()).collect();
            v.sort();
            v
        };
        assert_eq!(pass(HintFilter::Soft), vec!["a2.ARW", "a2.JPG"]);
        assert_eq!(pass(HintFilter::Accidental), vec!["black.JPG"]);
        assert_eq!(pass(HintFilter::Problems), vec!["a2.ARW", "a2.JPG", "black.JPG", "c (1).JPG"]);
        assert_eq!(pass(HintFilter::Duplicates), vec!["c (1).JPG", "c.JPG"]);
        assert_eq!(pass(HintFilter::Sharpest), vec!["a1.ARW", "a1.JPG"]);
        // Not measured yet: no hint at all.
        assert!(HintFilter::ALL.iter().all(|f| !f.accepts(&h, s.id("/s/new.JPG"))));
        assert_eq!(HintFilter::ALL.iter().map(|f| f.index()).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn presses_of_one_moment_are_spliced_in_the_grid() {
        use super::super::bursts::RunMark;
        // Press a (2 frames) and single s make a moment; press c (2 frames)
        // is a moment of its own; d is a single in none.
        let s = Shoot::new(vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 1))),
            (rec("/s/s.JPG", 900, Some(0)), Some(scene(1, 0))),
            (rec("/s/c1.JPG", 9000, Some(1)), Some(scene(3, 0))),
            (rec("/s/c2.JPG", 9100, Some(2)), Some(scene(3, 0))),
            (rec("/s/d.JPG", 20_000, Some(0)), Some(scene(4, 0))),
        ]);
        let h = s.hints(&[]);
        let ids: Vec<RecordId> = ["a1", "a2", "s", "c1", "c2", "d"].iter().map(|n| s.id(&format!("/s/{n}.JPG"))).collect();
        assert!(h.joined_moment(ids[0]).is_some() && h.joined_moment(ids[0]) == h.joined_moment(ids[2]));
        assert_eq!(h.joined_moment(ids[3]), None, "one press is a burst, not a joined moment");
        let (tiles, runs) = s.bursts.tiles(&ids, |_| false, |r| r[0], &s.coll);
        // a1–a2 are one strip; a2 splices to s; nothing else.
        assert_eq!(moment_links(&tiles, &runs, |id| h.joined_moment(id)), vec![false, true, false, false, false, false]);
        // Stacked, press a is one tile, spliced to s.
        let (tiles, runs) = s.bursts.tiles(&ids, |_| true, |r| r[0], &s.coll);
        assert_eq!(tiles.len(), 4);
        assert_eq!(moment_links(&tiles, &runs, |id| h.joined_moment(id)), vec![true, false, false, false]);
        let _: Option<RunMark> = runs[0];
    }
}
