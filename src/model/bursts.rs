//! Bursts: the frames of one shutter press, found from header metadata
//! alone, so they are known as soon as a folder is scanned.
//!
//! A shot (a RAW+JPG pair counts once) continues the press of the shot
//! before it, in capture order within its folder and camera, when:
//!
//! 1. **Both carry a press sequence number** (Sony's maker-note
//!    `SequenceNumber`, 1 for a press's first frame): the number goes up and
//!    isn't a first frame, within a second per number (generous: in-press
//!    gaps on an A7 V peak under one second).  A frame gone from the middle
//!    (trashed) doesn't split the press.  This is exact.
//! 2. **Else both have sub-second times**: they are at most twice the
//!    camera's usual frame interval apart (the median gap under a second in
//!    that folder; [`PRESS_GAP_MS`] when there are too few to tell).  On a
//!    10 fps A7 V day with the sequence numbers ignored, this rebuilt 96 %
//!    of 1 731 presses exactly; a fixed half second, 65 %.
//! 3. **Else (whole-second times only)**: they are in the same or the next
//!    second and their file numbers are consecutive (DSC01234, DSC01235).
//!
//! Shots with no EXIF capture time are never in a burst.  Merging presses
//! of the same scene into "moments" needs the pixels and comes later; the
//! data here is per press.
//!
//! The grid shows each burst as a strip of its frames, or folded into one
//! stack ([`Bursts::tiles`], [`Stacking`]).  Folding never finds bursts
//! again: they are recomputed only when files come, go or pair up.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::NaiveDateTime;

use super::collection::{Collection, RecordId};
use super::record::{FileType, PhotoRecord, ShotMeta};

/// Without sequence numbers, and too few close frames to learn the
/// camera's frame rate from, frames further apart than this are different
/// presses.  Continuous shooting at 3 fps leaves 0.33 s between frames.
pub const PRESS_GAP_MS: i64 = 500;
/// The learnt limit stays within these (10+ fps down to 3 fps bodies).
const PRESS_GAP_RANGE: (i64, i64) = (150, 700);
/// Close gaps needed before the frame interval is trusted.
const MIN_GAPS: usize = 5;
/// With sequence numbers: the most time allowed per step in the number.
const SEQUENCE_STEP_MS: i64 = 1000;
const NONE: u32 = u32::MAX;

pub type BurstId = u32;

/// Where a grid tile sits in a run of frames from one burst (consecutive in
/// display order).  Only runs of two or more frames are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunMark {
    pub burst: BurstId,
    /// The run's first and last tile (both, for a stack).
    pub first: bool,
    pub last: bool,
    /// Frames in this run (as tiles, or inside the stack), and frames in
    /// the whole burst (more when a filter hides some).
    pub shown: u16,
    pub total: u16,
    /// The run folded into this one tile, its cover: the first frame shown.
    pub stacked: bool,
    /// For a stack: how many of the burst's frames (all of them, whatever
    /// the filter shows) are marked to keep and to prune.
    pub kept: u16,
    pub pruned: u16,
}

/// Which bursts the grid shows stacked: all or none (View ▸ Stack all
/// bursts, saved in the settings), with single bursts opened or folded
/// against that for the session (G, or the burst's tab).
#[derive(Debug, Clone, Default)]
pub struct Stacking {
    all: bool,
    /// Every frame of each burst toggled against `all`.  Kept by frame
    /// rather than by burst number, so the choice survives the bursts being
    /// found again (a trash, a Sort) and any one frame going.
    flipped: HashSet<RecordId>,
}

impl Stacking {
    pub fn new(all: bool) -> Stacking {
        Stacking { all, flipped: HashSet::new() }
    }

    /// Bursts are stacked unless opened one by one.
    pub fn all(&self) -> bool {
        self.all
    }

    /// Stack every burst, or open every one; single choices are dropped.
    pub fn set_all(&mut self, all: bool) {
        self.all = all;
        self.flipped.clear();
    }

    /// A new folder: the single choices were for the old one.
    pub fn forget(&mut self) {
        self.flipped.clear();
    }

    /// The bursts toggled against `all`, resolved once for a refresh.
    pub fn flipped(&self, bursts: &Bursts) -> HashSet<BurstId> {
        self.flipped.iter().filter_map(|&id| bursts.of(id)).collect()
    }

    #[cfg(test)]
    pub fn is_stacked(&self, bursts: &Bursts, b: BurstId) -> bool {
        self.all != bursts.frames(b).iter().any(|f| self.flipped.contains(f))
    }

    /// Fold burst `b` into a stack, or open it.
    pub fn toggle(&mut self, bursts: &Bursts, b: BurstId) {
        let frames = bursts.frames(b);
        if frames.iter().any(|f| self.flipped.contains(f)) {
            for f in frames {
                self.flipped.remove(f);
            }
        } else {
            self.flipped.extend(frames.iter().copied());
        }
    }
}

/// The folder's bursts: each a press of two or more shots.
#[derive(Debug, Clone, Default)]
pub struct Bursts {
    /// The collection generation these were found for.
    generation: Option<u64>,
    /// Per record id: (burst, frame index), or `NONE`.
    at: Vec<(u32, u32)>,
    /// Each burst's frames in capture order, one record per shot (the JPG
    /// of a pair).
    frames: Vec<Vec<RecordId>>,
}

/// One shot: a file, or the halves of a RAW+JPG pair.
struct Shot<'a> {
    /// The record that stands for the shot: its JPG when it has one.
    rep: RecordId,
    members: Vec<RecordId>,
    time: NaiveDateTime,
    /// Capture time in milliseconds (with sub-seconds when known).
    millis: i64,
    meta: ShotMeta,
    name: &'a str,
}

impl Bursts {
    /// Find the bursts in `coll`.
    pub fn find(coll: &Collection) -> Bursts {
        let _t = crate::profile::span("bursts", "find");
        let mut shots: Vec<Shot<'_>> = Vec::new();
        let mut by_key: HashMap<(&Path, &str), usize> = HashMap::new();
        for (id, r) in coll.iter_ids() {
            let slot = match &r.pair_stem {
                Some(stem) => *by_key.entry((shot_parent(&r.path), stem.as_str())).or_insert_with(|| {
                    shots.push(Shot::new(id, r));
                    shots.len() - 1
                }),
                None => {
                    shots.push(Shot::new(id, r));
                    shots.len() - 1
                }
            };
            shots[slot].add(id, r, coll);
        }
        // Presses are looked for within one folder and one camera.
        let mut groups: HashMap<(&Path, u32), Vec<usize>> = HashMap::new();
        for (i, s) in shots.iter().enumerate() {
            if s.millis != i64::MIN {
                let r = coll.get(s.rep).expect("live record");
                groups.entry((shot_parent(&r.path), s.meta.camera)).or_default().push(i);
            }
        }
        let mut out = Bursts { generation: Some(coll.generation()), ..Bursts::default() };
        let max_id = coll.ids().iter().copied().max().map_or(0, |m| m as usize + 1);
        out.at = vec![(NONE, NONE); max_id];
        let mut groups: Vec<Vec<usize>> = groups.into_values().collect();
        // Bursts numbered in time order (only for a stable order).
        groups.sort_by_key(|g| g.iter().map(|&i| shots[i].millis).min());
        for mut g in groups {
            g.sort_by(|&a, &b| shots[a].order_key().cmp(&shots[b].order_key()));
            let limit = press_gap(&g.iter().map(|&i| &shots[i]).collect::<Vec<_>>());
            let mut start = 0;
            for k in 1..=g.len() {
                if k < g.len() && continues(&shots[g[k - 1]], &shots[g[k]], limit) {
                    continue;
                }
                // Copies of one frame don't make a burst on their own.
                let press: Vec<&Shot<'_>> = g[start..k].iter().map(|&i| &shots[i]).collect();
                if distinct_frames(&press) >= 2 {
                    out.add(press.into_iter());
                }
                start = k;
            }
        }
        out
    }

    fn add<'s, 'a: 's>(&mut self, press: impl Iterator<Item = &'s Shot<'a>>) {
        let b = self.frames.len() as u32;
        let mut frames = Vec::new();
        for (f, shot) in press.enumerate() {
            for &m in &shot.members {
                if let Some(slot) = self.at.get_mut(m as usize) {
                    *slot = (b, f as u32);
                }
            }
            frames.push(shot.rep);
        }
        self.frames.push(frames);
    }

    /// Found for this state of the collection.
    pub fn is_current(&self, coll: &Collection) -> bool {
        self.generation == Some(coll.generation())
    }

    /// The burst `id` belongs to (either half of a pair).
    pub fn of(&self, id: RecordId) -> Option<BurstId> {
        self.at.get(id as usize).map(|a| a.0).filter(|&b| b != NONE)
    }

    /// `id`'s frame index within its burst.
    pub fn frame_index(&self, id: RecordId) -> Option<usize> {
        self.at.get(id as usize).filter(|a| a.0 != NONE).map(|a| a.1 as usize)
    }

    /// A burst's frames in capture order, one record per shot.
    pub fn frames(&self, b: BurstId) -> &[RecordId] {
        self.frames.get(b as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How many bursts there are.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frames in bursts.
    pub fn frame_count(&self) -> usize {
        self.frames.iter().map(Vec::len).sum()
    }

    /// The grid's tiles for one day's photos in display order, and their
    /// marks: each run of two or more frames from one burst stays a strip
    /// of tiles, or folds into one tile when `stacked(burst)`.  The stack's
    /// cover is the run's first frame shown — with To review on, the first
    /// still to review.  (The sharpest frame, once the analysis knows it.)
    pub fn tiles(
        &self,
        ids: &[RecordId],
        stacked: impl Fn(BurstId) -> bool,
        cover: impl Fn(&[RecordId]) -> RecordId,
        coll: &Collection,
    ) -> (Vec<RecordId>, Vec<Option<RunMark>>) {
        let mut runs: Vec<(usize, usize, BurstId)> = Vec::new();
        self.each_run(ids, |b, i, j| runs.push((i, j, b)));
        if runs.is_empty() {
            return (ids.to_vec(), vec![None; ids.len()]);
        }
        let mut tiles = Vec::with_capacity(ids.len());
        let mut marks = Vec::with_capacity(ids.len());
        let mut at = 0;
        for (i, j, b) in runs {
            tiles.extend_from_slice(&ids[at..i]);
            marks.resize(tiles.len(), None);
            let (shown, total) = self.counts(b, j - i);
            if stacked(b) {
                let (mut kept, mut pruned) = (0u16, 0u16);
                for r in self.frames(b).iter().filter_map(|&f| coll.get(f)) {
                    kept = kept.saturating_add(r.is_marked as u16);
                    pruned = pruned.saturating_add(r.is_pruned as u16);
                }
                tiles.push(cover(&ids[i..j]));
                marks.push(Some(RunMark { burst: b, first: true, last: true, shown, total, stacked: true, kept, pruned }));
            } else {
                for (k, &id) in ids.iter().enumerate().take(j).skip(i) {
                    tiles.push(id);
                    marks.push(Some(RunMark { burst: b, first: k == i, last: k == j - 1, shown, total, stacked: false, kept: 0, pruned: 0 }));
                }
            }
            at = j;
        }
        tiles.extend_from_slice(&ids[at..]);
        marks.resize(tiles.len(), None);
        (tiles, marks)
    }

    /// Calls `f(burst, start, end)` for each run of two or more consecutive
    /// ids from one burst.
    fn each_run(&self, ids: &[RecordId], mut f: impl FnMut(BurstId, usize, usize)) {
        if self.frames.is_empty() {
            return;
        }
        let mut i = 0;
        while i < ids.len() {
            let Some(b) = self.of(ids[i]) else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            while j < ids.len() && self.of(ids[j]) == Some(b) {
                j += 1;
            }
            if j - i >= 2 {
                f(b, i, j);
            }
            i = j;
        }
    }

    /// (frames in a run of `len`, frames in burst `b`), for a [`RunMark`].
    fn counts(&self, b: BurstId, len: usize) -> (u16, u16) {
        let clamp = |n: usize| n.min(u16::MAX as usize) as u16;
        (clamp(len), clamp(self.frames(b).len()))
    }
}

impl<'a> Shot<'a> {
    fn new(id: RecordId, r: &'a PhotoRecord) -> Shot<'a> {
        let (time, millis) = match r.capture_time {
            Some(t) => (t, t.and_utc().timestamp_millis() + r.shot.subsec_ms.unwrap_or(0) as i64),
            None => (NaiveDateTime::default(), i64::MIN),
        };
        Shot { rep: id, members: Vec::new(), time, millis, meta: r.shot, name: &r.name_lower }
    }

    /// Another file of the same shot.  The JPG stands for the shot; the
    /// metadata is the first file's, with gaps filled from the others.
    fn add(&mut self, id: RecordId, r: &'a PhotoRecord, coll: &Collection) {
        if !self.members.is_empty() {
            let rep_is_jpg = coll.get(self.rep).is_some_and(|x| x.file_type == FileType::Jpg);
            if r.file_type == FileType::Jpg && !rep_is_jpg {
                let keep = (self.millis, self.time, self.meta);
                *self = Shot { members: std::mem::take(&mut self.members), ..Shot::new(id, r) };
                if self.millis == i64::MIN {
                    (self.millis, self.time) = (keep.0, keep.1);
                }
                self.meta = fill(self.meta, keep.2);
            } else {
                if self.millis == i64::MIN {
                    let other = Shot::new(id, r);
                    (self.millis, self.time) = (other.millis, other.time);
                }
                self.meta = fill(self.meta, r.shot);
            }
        }
        self.members.push(id);
    }

    /// Capture order: time, then file name (frames in one second on a body
    /// without sub-second times).
    fn order_key(&self) -> (i64, &str) {
        (self.millis, self.name)
    }
}

/// `a` with the fields it lacks taken from `b`.
fn fill(a: ShotMeta, b: ShotMeta) -> ShotMeta {
    ShotMeta {
        subsec_ms: a.subsec_ms.or(b.subsec_ms),
        sequence: a.sequence.or(b.sequence),
        focus: a.focus.or(b.focus),
        camera: if a.camera != 0 { a.camera } else { b.camera },
    }
}

/// The widest gap between frames of one press when only sub-second times
/// tell: twice the camera's usual frame interval, learnt from the gaps
/// under a second between shots in capture order.
fn press_gap(shots: &[&Shot<'_>]) -> i64 {
    let mut close: Vec<i64> = shots
        .windows(2)
        .filter(|w| w[0].meta.subsec_ms.is_some() && w[1].meta.subsec_ms.is_some())
        .map(|w| w[1].millis - w[0].millis)
        .filter(|g| (1..1000).contains(g))
        .collect();
    if close.len() < MIN_GAPS {
        return PRESS_GAP_MS;
    }
    close.sort_unstable();
    (2 * close[close.len() / 2]).clamp(PRESS_GAP_RANGE.0, PRESS_GAP_RANGE.1)
}

/// Does `b` (later) belong to the same press as `a`?  `limit`: see
/// [`press_gap`].
fn continues(a: &Shot<'_>, b: &Shot<'_>, limit: i64) -> bool {
    // A copy of the same frame (a day exported twice) belongs with it.
    if same_capture(a, b) {
        return true;
    }
    let gap = b.millis - a.millis;
    if gap < 0 {
        return false;
    }
    match (a.meta.sequence, b.meta.sequence) {
        (Some(sa), Some(sb)) => sb >= 2 && sb > sa && gap <= SEQUENCE_STEP_MS * (sb - sa) as i64 + SEQUENCE_STEP_MS,
        _ if a.meta.subsec_ms.is_some() && b.meta.subsec_ms.is_some() => gap <= limit,
        _ => gap <= 1000 && file_number(a.name).zip(file_number(b.name)).is_some_and(|(x, y)| y == x + 1),
    }
}

/// Two files of one frame: the same capture to the millisecond and the same
/// place in its press.  Only trusted with sub-seconds or a sequence number;
/// whole-second times can't tell frames of one second apart.
fn same_capture(a: &Shot<'_>, b: &Shot<'_>) -> bool {
    a.millis == b.millis
        && a.meta.sequence == b.meta.sequence
        && a.meta.camera == b.meta.camera
        && (a.meta.sequence.is_some() || a.meta.subsec_ms.is_some())
}

/// Frames in a press, counting copies of one frame once.
fn distinct_frames(press: &[&Shot<'_>]) -> usize {
    1 + press.windows(2).filter(|w| !same_capture(w[0], w[1])).count()
}

/// The number a camera ends its file names with ("dsc01234.arw" → 1234).
fn file_number(name: &str) -> Option<u64> {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 18 {
        return None;
    }
    stem[stem.len() - digits..].parse().ok()
}

/// The folder a shot belongs to: the parent, without a `RAW`/`JPG` level
/// (so pairs and bursts survive a Sort).
fn shot_parent(path: &Path) -> &Path {
    let empty = Path::new("");
    let parent = path.parent().unwrap_or(empty);
    match parent.file_name().map(|n| n.to_string_lossy()) {
        Some(n) if n.eq_ignore_ascii_case("RAW") || n.eq_ignore_ascii_case("JPG") => parent.parent().unwrap_or(empty),
        _ => parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;
    use std::path::PathBuf;

    /// A record shot at second `sec` past 10:00:00, with `ms` and Sony
    /// sequence `seq` when given.
    fn rec(path: &str, sec: u32, ms: Option<u16>, seq: Option<u16>) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        let t = NaiveDateTime::parse_from_str(&format!("2026-09-19 10:00:{sec:02}"), "%Y-%m-%d %H:%M:%S").unwrap();
        let mut r = PhotoRecord::new(p, ft, 10, MTime::from_unix(1_700_000_000, 0), Some(t));
        r.shot = ShotMeta { subsec_ms: ms, sequence: seq, focus: None, camera: 1 };
        r
    }

    fn names(c: &Collection, b: &Bursts) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> =
            (0..b.len() as u32).map(|i| b.frames(i).iter().map(|&id| c.get(id).unwrap().stem()).collect()).collect();
        out.sort();
        out
    }

    fn find(recs: Vec<PhotoRecord>) -> (Collection, Bursts) {
        let mut c = Collection::new();
        for r in recs {
            c.add(r);
        }
        c.build_pairs();
        let b = Bursts::find(&c);
        (c, b)
    }

    #[test]
    fn sequence_numbers_split_presses_exactly() {
        // Two presses 250 ms apart (closer than some in-press gaps), then a
        // single shot: the sequence number decides, not the time.
        let (c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/a3.JPG", 0, Some(340), Some(3)),
            rec("/s/b1.JPG", 0, Some(590), Some(1)),
            rec("/s/b2.JPG", 0, Some(690), Some(2)),
            rec("/s/c.JPG", 5, Some(0), Some(0)),
        ]);
        assert_eq!(names(&c, &b), [vec!["a1", "a2", "a3"], vec!["b1", "b2"]]);
        let c_id = c.id_of(Path::new("/s/c.JPG")).unwrap();
        assert_eq!(b.of(c_id), None);
        let a3 = c.id_of(Path::new("/s/a3.JPG")).unwrap();
        assert_eq!(b.frame_index(a3), Some(2));
        assert!(b.is_current(&c));
    }

    #[test]
    fn a_folder_exported_twice_keeps_its_presses_whole() {
        // Every frame twice under two names (as on 23-May-2026): in capture
        // order the copies interleave, 0001, 3356, 0002, 3357 …  They must
        // not split the press into pairs.
        let mut recs = Vec::new();
        for (i, ms) in [(1u16, 784u16), (2, 117), (3, 450), (4, 783)] {
            let sec = if i == 1 { 54 } else { 55 };
            recs.push(rec(&format!("/s/x_{:04}.JPG", i), sec, Some(ms), Some(i)));
            recs.push(rec(&format!("/s/x_{:04}.JPG", 3355 + i as u32), sec, Some(ms), Some(i)));
        }
        // A single frame, copied twice: copies alone don't make a burst.
        recs.push(rec("/s/y_0014.JPG", 59, Some(118), Some(1)));
        recs.push(rec("/s/y_3369.JPG", 59, Some(118), Some(1)));
        let (c, b) = find(recs);
        assert_eq!(
            names(&c, &b),
            [vec!["x_0001", "x_3356", "x_0002", "x_3357", "x_0003", "x_3358", "x_0004", "x_3359"]]
        );
        assert_eq!(b.of(c.id_of(Path::new("/s/y_0014.JPG")).unwrap()), None);
        assert_eq!(b.of(c.id_of(Path::new("/s/y_3369.JPG")).unwrap()), None);
    }

    #[test]
    fn a_frame_gone_from_the_middle_keeps_the_press_whole() {
        let (c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            // a3 and a4 trashed
            rec("/s/a5.JPG", 0, Some(400), Some(5)),
            // A press whose first frames are gone, too soon after a5 to be
            // its continuation by number alone? 6 > 5, so time decides: 4 s.
            rec("/s/b3.JPG", 4, Some(500), Some(6)),
            rec("/s/b4.JPG", 4, Some(600), Some(7)),
        ]);
        assert_eq!(names(&c, &b), [vec!["a1", "a2", "a5"], vec!["b3", "b4"]]);
    }

    #[test]
    fn sub_second_times_group_without_sequence_numbers() {
        // A Canon-style body: sub-seconds, no maker-note sequence.  3 fps.
        let (c, b) = find(vec![
            rec("/s/IMG_0001.JPG", 0, Some(10), None),
            rec("/s/IMG_0002.JPG", 0, Some(340), None),
            rec("/s/IMG_0003.JPG", 0, Some(670), None),
            rec("/s/IMG_0004.JPG", 1, Some(400), None),
            rec("/s/IMG_0005.JPG", 1, Some(700), None),
            rec("/s/IMG_0006.JPG", 9, Some(0), None),
        ]);
        assert_eq!(names(&c, &b), [vec!["IMG_0001", "IMG_0002", "IMG_0003"], vec!["IMG_0004", "IMG_0005"]]);
    }

    #[test]
    fn the_gap_limit_follows_the_cameras_frame_rate() {
        // 10 fps, no sequence numbers: presses 300 ms apart stay apart,
        // which a fixed half second would merge.
        let mut recs = Vec::new();
        for (p, start) in [(0u32, 0u32), (1, 700), (2, 1400)] {
            for f in 0..4u32 {
                let ms = start + f * 100;
                recs.push(rec(&format!("/s/p{p}f{f}.JPG"), ms / 1000, Some((ms % 1000) as u16), None));
            }
        }
        let (c, b) = find(recs);
        assert_eq!(b.len(), 3, "{:?}", names(&c, &b));
        assert!(names(&c, &b).iter().all(|p| p.len() == 4));
    }

    #[test]
    fn whole_second_bodies_need_consecutive_file_numbers() {
        // An A7R II-style body: whole seconds only.  Frames in one second
        // are put in file order, wherever the scan found them.
        let (c, b) = find(vec![
            rec("/s/DSC00012.ARW", 1, None, None),
            rec("/s/DSC00010.ARW", 0, None, None),
            rec("/s/DSC00011.ARW", 0, None, None),
            // Next second but not the next number: another press.
            rec("/s/DSC00020.ARW", 2, None, None),
            rec("/s/DSC00021.ARW", 5, None, None),
            rec("/s/notes.ARW", 5, None, None),
        ]);
        assert_eq!(names(&c, &b), [vec!["DSC00010", "DSC00011", "DSC00012"]]);
        let frames = b.frames(0);
        assert_eq!(c.get(frames[0]).unwrap().stem(), "DSC00010");
    }

    #[test]
    fn a_pair_is_one_frame_and_either_half_finds_the_burst() {
        let mut raw2 = rec("/s/RAW/x2.ARW", 0, None, None);
        // The RAW's metadata fills what the JPG lacks.
        raw2.shot.sequence = Some(2);
        let (c, b) = find(vec![
            rec("/s/RAW/x1.ARW", 0, Some(0), Some(1)),
            rec("/s/JPG/x1.JPG", 0, Some(0), Some(1)),
            raw2,
            rec("/s/JPG/x2.JPG", 0, Some(120), None),
            rec("/s/x3.JPG", 0, Some(240), Some(3)),
        ]);
        assert_eq!(b.len(), 1);
        let frames = b.frames(0);
        assert_eq!(frames.len(), 3);
        // Frames are the JPGs.
        assert!(frames.iter().all(|&id| c.get(id).unwrap().file_type == FileType::Jpg));
        let raw1 = c.id_of(Path::new("/s/RAW/x1.ARW")).unwrap();
        assert_eq!((b.of(raw1), b.frame_index(raw1)), (Some(0), Some(0)));
    }

    #[test]
    fn different_cameras_and_folders_never_share_a_burst() {
        let mut other = rec("/s/b2.JPG", 0, Some(100), Some(2));
        other.shot.camera = 2;
        let (c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            other,
            rec("/t/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/a3.JPG", 0, Some(150), Some(2)),
        ]);
        assert_eq!(names(&c, &b), [vec!["a1", "a3"]]);
    }

    #[test]
    fn no_capture_time_no_burst() {
        let mut a = rec("/s/a1.JPG", 0, Some(0), Some(1));
        a.capture_time = None;
        let (_, b) = find(vec![a, rec("/s/a2.JPG", 0, Some(100), Some(2))]);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn bursts_survive_a_sort_and_follow_removals() {
        let (mut c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a1.ARW", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/a2.ARW", 0, Some(100), Some(2)),
        ]);
        assert_eq!(b.frame_count(), 2);
        for (from, to) in [("/s/a1.JPG", "/s/JPG/a1.JPG"), ("/s/a1.ARW", "/s/RAW/a1.ARW"), ("/s/a2.JPG", "/s/JPG/a2.JPG")] {
            let id = c.id_of(Path::new(from)).unwrap();
            c.update_path(id, to.into());
        }
        c.build_pairs();
        assert!(!b.is_current(&c), "a move changes the collection");
        let sorted = Bursts::find(&c);
        assert_eq!(names(&c, &sorted), [vec!["a1", "a2"]]);
        // Trash one shot: a burst of one is no burst.
        for p in ["/s/JPG/a2.JPG", "/s/a2.ARW"] {
            let id = c.id_of(Path::new(p)).unwrap();
            c.remove(id);
        }
        c.build_pairs();
        assert_eq!(Bursts::find(&c).len(), 0);
    }

    /// Marks for tiles shown in this order, every burst open.
    fn open_runs(b: &Bursts, c: &Collection, ids: &[RecordId]) -> Vec<Option<RunMark>> {
        let (tiles, marks) = b.tiles(ids, |_| false, |run| run[0], c);
        assert_eq!(tiles, ids, "nothing folds when nothing is stacked");
        marks
    }

    #[test]
    fn runs_mark_consecutive_tiles_of_one_burst() {
        let (c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/a3.JPG", 0, Some(200), Some(3)),
            rec("/s/x.JPG", 3, Some(0), Some(1)),
            rec("/s/b1.JPG", 6, Some(0), Some(1)),
            rec("/s/b2.JPG", 6, Some(100), Some(2)),
        ]);
        let id = |n: &str| c.id_of(Path::new(&format!("/s/{n}.JPG"))).unwrap();
        // All shown, in order.
        let order = [id("a1"), id("a2"), id("a3"), id("x"), id("b1"), id("b2")];
        let runs = open_runs(&b, &c, &order);
        let a = b.of(id("a1")).unwrap();
        let open = |first, last, shown, total| Some(RunMark { burst: a, first, last, shown, total, stacked: false, kept: 0, pruned: 0 });
        assert_eq!(runs[0], open(true, false, 3, 3));
        assert_eq!(runs[1].map(|r| (r.first, r.last)), Some((false, false)));
        assert_eq!(runs[2].map(|r| (r.first, r.last)), Some((false, true)));
        assert_eq!(runs[3], None);
        assert_eq!(runs[4].map(|r| (r.first, r.shown)), Some((true, 2)));
        assert_ne!(runs[4].map(|r| r.burst), Some(a));
        // A filter hid a2: the run shows two of three.
        let runs = open_runs(&b, &c, &[id("a1"), id("a3"), id("x")]);
        assert_eq!(runs[0], open(true, false, 2, 3));
        // Split by another photo (a name sort): lone tiles get no mark.
        let runs = open_runs(&b, &c, &[id("a1"), id("x"), id("a2"), id("a3")]);
        assert_eq!(runs[0], None);
        assert_eq!(runs[2].map(|r| r.shown), Some(2));
    }

    #[test]
    fn tiles_fold_stacked_runs_into_their_first_frame() {
        let (mut c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/a3.JPG", 0, Some(200), Some(3)),
            rec("/s/x.JPG", 3, Some(0), Some(1)),
            rec("/s/b1.JPG", 6, Some(0), Some(1)),
            rec("/s/b2.JPG", 6, Some(100), Some(2)),
        ]);
        let id = |c: &Collection, n: &str| c.id_of(Path::new(&format!("/s/{n}.JPG"))).unwrap();
        let order: Vec<RecordId> = ["a1", "a2", "a3", "x", "b1", "b2"].iter().map(|n| id(&c, n)).collect();
        let (ba, bb) = (b.of(order[0]).unwrap(), b.of(order[4]).unwrap());
        // Nothing stacked: the tiles are the photos, each run a strip.
        let (tiles, marks) = b.tiles(&order, |_| false, |run| run[0], &c);
        assert_eq!(tiles, order);
        assert_eq!(marks.iter().filter(|m| m.is_some_and(|m| !m.stacked)).count(), 5);
        // Burst a stacked: one tile, a1, standing for three frames.
        c.get_mut(order[1]).unwrap().is_pruned = true;
        c.get_mut(order[2]).unwrap().is_marked = true;
        let (tiles, marks) = b.tiles(&order, |x| x == ba, |run| run[0], &c);
        assert_eq!(tiles, [order[0], order[3], order[4], order[5]]);
        assert_eq!(marks[0], Some(RunMark { burst: ba, first: true, last: true, shown: 3, total: 3, stacked: true, kept: 1, pruned: 1 }));
        assert_eq!(marks[1], None);
        assert_eq!(marks[2].map(|m| (m.burst, m.first, m.stacked)), Some((bb, true, false)));
        // Both stacked; a filter hid a1, so a2 is the cover of "2 of 3", and
        // the marks still count the whole burst.
        let shown = [order[1], order[2], order[3], order[4], order[5]];
        let (tiles, marks) = b.tiles(&shown, |_| true, |run| run[0], &c);
        assert_eq!(tiles, [order[1], order[3], order[4]]);
        assert_eq!(marks[0].map(|m| (m.shown, m.total, m.kept, m.pruned)), Some((2, 3, 1, 1)));
        assert_eq!(marks[2].map(|m| (m.shown, m.stacked)), Some((2, true)));
        // The cover is chosen from the frames shown (the sharpest, once
        // analysed): here the last of a's, the marks unchanged.
        let (tiles, marks) = b.tiles(&order, |x| x == ba, |run| *run.last().unwrap(), &c);
        assert_eq!(tiles, [order[2], order[3], order[4], order[5]]);
        assert_eq!(marks[0].map(|m| (m.shown, m.stacked)), Some((3, true)));
        // One frame left of a burst isn't a run, stacked or not.
        let (tiles, marks) = b.tiles(&[order[0], order[3]], |_| true, |run| run[0], &c);
        assert_eq!(tiles, [order[0], order[3]]);
        assert_eq!(marks, [None, None]);
    }

    #[test]
    fn stacking_follows_the_default_and_single_choices() {
        let (mut c, b) = find(vec![
            rec("/s/a1.JPG", 0, Some(0), Some(1)),
            rec("/s/a2.JPG", 0, Some(100), Some(2)),
            rec("/s/b1.JPG", 6, Some(0), Some(1)),
            rec("/s/b2.JPG", 6, Some(100), Some(2)),
            rec("/s/b3.JPG", 6, Some(200), Some(3)),
        ]);
        let burst = |c: &Collection, b: &Bursts, n: &str| b.of(c.id_of(Path::new(&format!("/s/{n}.JPG"))).unwrap()).unwrap();
        let (ba, bb) = (burst(&c, &b, "a1"), burst(&c, &b, "b1"));
        // Open by default; one burst folded, then opened again.
        let mut s = Stacking::new(false);
        assert!(!s.is_stacked(&b, ba) && !s.is_stacked(&b, bb));
        s.toggle(&b, bb);
        assert!(!s.is_stacked(&b, ba) && s.is_stacked(&b, bb));
        assert_eq!(s.flipped(&b), [bb].into());
        s.toggle(&b, bb);
        assert!(!s.is_stacked(&b, bb));
        // Stack all: every burst folds and single choices are dropped; one
        // opened against it stays open while the rest stay stacked.
        s.toggle(&b, ba);
        s.set_all(true);
        assert!(s.is_stacked(&b, ba) && s.is_stacked(&b, bb));
        s.toggle(&b, ba);
        assert!(!s.is_stacked(&b, ba) && s.is_stacked(&b, bb));
        // The bursts found again after a frame is trashed (new numbers):
        // the choice carries over, by frame.
        let b1 = c.id_of(Path::new("/s/b1.JPG")).unwrap();
        c.remove(b1);
        c.build_pairs();
        let again = Bursts::find(&c);
        let (ba2, bb2) = (burst(&c, &again, "a1"), burst(&c, &again, "b2"));
        assert!(!s.is_stacked(&again, ba2) && s.is_stacked(&again, bb2));
        // A new folder forgets the single choices, not the default.
        s.forget();
        assert!(s.all() && s.is_stacked(&again, ba2));
    }

    #[test]
    fn file_numbers() {
        assert_eq!(file_number("dsc01234.arw"), Some(1234));
        assert_eq!(file_number("20260919_1042_0817.jpg"), Some(817));
        assert_eq!(file_number("img_0001 copy.jpg"), None);
        assert_eq!(file_number("noext"), None);
    }
}
