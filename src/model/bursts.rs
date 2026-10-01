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
//! data here is per press, so stacking a burst into one tile can be built
//! on [`Bursts::frames`] and [`Bursts::runs`].

use std::collections::HashMap;
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

/// Where a grid tile sits in a run of tiles from one burst (consecutive in
/// display order).  Only runs of two or more tiles are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunMark {
    /// The run's first and last tile.
    pub first: bool,
    pub last: bool,
    /// Tiles in this run, and frames in the whole burst (more when a
    /// filter hides some).
    pub shown: u16,
    pub total: u16,
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
                if k - start >= 2 {
                    out.add(g[start..k].iter().map(|&i| &shots[i]));
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

    /// Marks for tiles shown in this order: each run of two or more
    /// consecutive tiles from one burst.
    pub fn runs(&self, ids: &[RecordId]) -> Vec<Option<RunMark>> {
        let of: Vec<Option<BurstId>> = ids.iter().map(|&id| self.of(id)).collect();
        let mut out = vec![None; ids.len()];
        let mut i = 0;
        while i < ids.len() {
            let Some(b) = of[i] else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            while j < ids.len() && of[j] == Some(b) {
                j += 1;
            }
            if j - i >= 2 {
                let total = self.frames(b).len().min(u16::MAX as usize) as u16;
                let shown = (j - i).min(u16::MAX as usize) as u16;
                for (k, slot) in out.iter_mut().enumerate().take(j).skip(i) {
                    *slot = Some(RunMark { first: k == i, last: k == j - 1, shown, total });
                }
            }
            i = j;
        }
        out
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
        let runs = b.runs(&order);
        assert_eq!(runs[0], Some(RunMark { first: true, last: false, shown: 3, total: 3 }));
        assert_eq!(runs[1].map(|r| (r.first, r.last)), Some((false, false)));
        assert_eq!(runs[2].map(|r| (r.first, r.last)), Some((false, true)));
        assert_eq!(runs[3], None);
        assert_eq!(runs[4].map(|r| (r.first, r.shown)), Some((true, 2)));
        // A filter hid a2: the run shows two of three.
        let runs = b.runs(&[id("a1"), id("a3"), id("x")]);
        assert_eq!(runs[0], Some(RunMark { first: true, last: false, shown: 2, total: 3 }));
        // Split by another photo (a name sort): lone tiles get no mark.
        let runs = b.runs(&[id("a1"), id("x"), id("a2"), id("a3")]);
        assert_eq!(runs[0], None);
        assert_eq!(runs[2].map(|r| r.shown), Some(2));
    }

    #[test]
    fn file_numbers() {
        assert_eq!(file_number("dsc01234.arw"), Some(1234));
        assert_eq!(file_number("20260919_1042_0817.jpg"), Some(817));
        assert_eq!(file_number("img_0001 copy.jpg"), None);
        assert_eq!(file_number("noext"), None);
    }
}
