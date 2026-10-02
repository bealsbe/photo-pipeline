//! Search results as the page holds them: the records of a query's hits
//! (built on a worker from the library index the calendar already holds,
//! reading no file), their tiles in one best-first list with each burst
//! stacked at the rank of its best frame, and the recent searches.  Pure,
//! so it is tested without a window.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime};

use crate::model::bursts::RunMark;
use crate::model::collection::{Collection, RecordId};
use crate::model::library_index::FileEntry;
use crate::search::Hit;
use crate::ui::grid::Section;

/// The results' one section has no day: this stands in for its date (the
/// grid keys a section's collapse by date).
pub const RESULTS_DAY: NaiveDate = NaiveDate::MIN;

/// One result shot.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The record its tile shows: the JPG of a pair, else the file.
    pub id: RecordId,
    pub score: f32,
    pub when: NaiveDateTime,
    pub day: NaiveDate,
    pub burst: Option<u64>,
}

/// A query's results.
pub struct Results {
    pub seq: u64,
    /// The words searched for, as typed.
    pub words: String,
    /// Every hit's files (both halves of a pair), paired.
    pub coll: Arc<Collection>,
    /// Best first.
    pub found: Vec<Found>,
    /// Hits whose files the library index doesn't hold (moved since).
    pub missing: usize,
}

impl std::fmt::Debug for Results {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Results {{ seq: {}, words: {:?}, found: {}, missing: {} }}", self.seq, self.words, self.found.len(), self.missing)
    }
}

impl Results {
    /// The day of the photo `id` shows (either half of a pair).
    pub fn day_of(&self, id: RecordId) -> Option<NaiveDate> {
        let tile = |x: RecordId| self.found.iter().find(|f| f.id == x).map(|f| f.day);
        tile(id).or_else(|| self.coll.find_pair(id).and_then(tile))
    }
}

/// Build the records for `hits`: each file is looked up in the library
/// index by its day and path (`lookup`), so nothing is read from disk.  The
/// capture time is the engine's, to the sub-second.
pub fn build(seq: u64, words: String, hits: &[Hit], lookup: impl Fn(NaiveDate, &Path) -> Option<FileEntry>) -> Results {
    let mut coll = Collection::new();
    let mut found = Vec::with_capacity(hits.len());
    let mut missing = 0;
    for h in hits {
        let mut add = |path: Option<&Path>| -> Option<RecordId> {
            let entry = lookup(h.day, path?)?;
            let mut r = entry.record();
            r.capture_time = Some(h.capture);
            Some(coll.add(r))
        };
        let jpg = add(h.jpg.as_deref());
        let raw = add(h.raw.as_deref());
        match jpg.or(raw) {
            Some(id) => found.push(Found { id, score: h.score, when: h.capture, day: h.day, burst: h.burst }),
            None => missing += 1,
        }
    }
    coll.build_pairs();
    Results { seq, words, coll: Arc::new(coll), found, missing }
}

/// What the grid shows of the results.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub sections: Vec<Section>,
    pub runs: Vec<Vec<Option<RunMark>>>,
    /// The tiles in order (a stack is one).
    pub tiles: Vec<RecordId>,
    /// Every frame, stacks opened in place: what the viewer steps through.
    pub visible: Vec<RecordId>,
    /// Each burst's frames in shooting order, by its group number (the
    /// grid's burst id).
    pub groups: Vec<Vec<RecordId>>,
    /// The burst key of each group.
    pub keys: Vec<Option<u64>>,
}

impl Layout {
    /// The group `id` (a frame or a cover) is in.
    pub fn group_of(&self, id: RecordId) -> Option<usize> {
        self.groups.iter().position(|g| g.contains(&id))
    }

    /// The tile showing `id`: itself, or the stack it is folded into.
    pub fn tile_of(&self, id: RecordId) -> Option<RecordId> {
        if self.tiles.contains(&id) {
            return Some(id);
        }
        let g = self.group_of(id)?;
        self.groups[g].iter().copied().find(|x| self.tiles.contains(x))
    }

    /// The burst mark of the tile `id`.
    #[cfg(test)]
    pub fn run_of(&self, id: RecordId) -> Option<RunMark> {
        let i = self.tiles.iter().position(|&x| x == id)?;
        self.runs.first()?.get(i).copied().flatten()
    }

    /// The frames of the burst whose tile is `id`, in shooting order, when
    /// it is a burst.
    pub fn frames_of(&self, id: RecordId) -> Option<&[RecordId]> {
        let g = self.group_of(id)?;
        (self.groups[g].len() > 1).then(|| self.groups[g].as_slice())
    }
}

/// Lay `found` out best first.  The frames of one burst gather at the
/// place of its best frame: one stacked tile (that frame, its cover) when
/// `stacked` says so for its key, else every frame in shooting order.
pub fn layout(found: &[Found], label: &str, stacked: impl Fn(u64) -> bool) -> Layout {
    // Groups in the order of their best frame.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut keys: Vec<Option<u64>> = Vec::new();
    let mut by_key: HashMap<u64, usize> = HashMap::new();
    for (i, f) in found.iter().enumerate() {
        match f.burst {
            Some(k) => match by_key.get(&k) {
                Some(&g) => groups[g].push(i),
                None => {
                    by_key.insert(k, groups.len());
                    groups.push(vec![i]);
                    keys.push(Some(k));
                }
            },
            None => {
                groups.push(vec![i]);
                keys.push(None);
            }
        }
    }
    let mut tiles = Vec::with_capacity(found.len());
    let mut runs = Vec::with_capacity(found.len());
    let mut visible = Vec::with_capacity(found.len());
    let mut frames_by_group = Vec::with_capacity(groups.len());
    for (g, members) in groups.iter().enumerate() {
        let cover = found[members[0]].id;
        let mut frames: Vec<&Found> = members.iter().map(|&i| &found[i]).collect();
        frames.sort_by(|a, b| a.when.cmp(&b.when).then(a.id.cmp(&b.id)));
        let ids: Vec<RecordId> = frames.iter().map(|f| f.id).collect();
        visible.extend_from_slice(&ids);
        let n = ids.len();
        if n < 2 {
            tiles.push(cover);
            runs.push(None);
        } else {
            let mark = |first: bool, last: bool, stacked: bool| RunMark {
                burst: g as u32,
                first,
                last,
                shown: n.min(u16::MAX as usize) as u16,
                total: n.min(u16::MAX as usize) as u16,
                stacked,
                kept: 0,
                pruned: 0,
            };
            if keys[g].is_some_and(&stacked) {
                tiles.push(cover);
                runs.push(Some(mark(true, true, true)));
            } else {
                for (k, &id) in ids.iter().enumerate() {
                    tiles.push(id);
                    runs.push(Some(mark(k == 0, k + 1 == n, false)));
                }
            }
        }
        frames_by_group.push(ids);
    }
    let section = Section { date: RESULTS_DAY, label: label.to_string(), photos: found.len(), ids: tiles.clone() };
    Layout { sections: vec![section], runs: vec![runs], tiles, visible, groups: frames_by_group, keys }
}

/// The selection once the tiles changed (a burst stacked or opened, new
/// results): each photo still a tile stays, one folded into a stack hands
/// it to the stack, the rest go.
pub fn regather(layout: &Layout, selection: &HashSet<RecordId>, anchor: Option<RecordId>) -> (HashSet<RecordId>, Option<RecordId>) {
    let sel = selection.iter().filter_map(|&id| layout.tile_of(id)).collect();
    (sel, anchor.and_then(|a| layout.tile_of(a)))
}

/// The tile an arrow key moves to: the next or previous one, the first
/// when nothing is focused; it stays at either end.
pub fn step(tiles: &[RecordId], anchor: Option<RecordId>, forward: bool) -> Option<RecordId> {
    match anchor.and_then(|a| tiles.iter().position(|&x| x == a)) {
        None => tiles.first().copied(),
        Some(i) if forward => tiles.get((i + 1).min(tiles.len() - 1)).copied(),
        Some(i) => tiles.get(i.saturating_sub(1)).copied(),
    }
}

// ── recent searches ─────────────────────────────────────────────────────────
//
// Kept in the settings (`Settings::remember_search`, the last 10, newest
// first; Preferences ▸ Search ▸ Forget them clears them).

/// The recent searches offered while `typed` is in the field: those with a
/// word starting with it (all of them for an empty field), newest first,
/// leaving out the one that is exactly what is typed.
pub fn completions<'a>(recent: &'a [String], typed: &str) -> Vec<&'a str> {
    let t = typed.trim().to_lowercase();
    recent
        .iter()
        .map(String::as_str)
        .filter(|r| {
            let l = r.to_lowercase();
            l != t && (t.is_empty() || l.starts_with(&t) || l.contains(&format!(" {t}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::{FileType, MTime};
    use std::path::PathBuf;

    fn when(min: u32, ms: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 6, 8).unwrap().and_hms_opt(14, min, 0).unwrap() + chrono::Duration::milliseconds(ms)
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 6, 8).unwrap()
    }

    fn f(id: RecordId, score: f32, w: NaiveDateTime, burst: Option<u64>) -> Found {
        Found { id, score, when: w, day: day(), burst }
    }

    #[test]
    fn a_burst_stacks_at_the_rank_of_its_best_frame() {
        // Best first: 10 (burst 7), 20, 11 (burst 7), 30, 12 (burst 7).
        let found = vec![
            f(10, 0.9, when(1, 300), Some(7)),
            f(20, 0.8, when(5, 0), None),
            f(11, 0.7, when(1, 100), Some(7)),
            f(30, 0.6, when(9, 0), Some(9)),
            f(12, 0.5, when(1, 200), Some(7)),
        ];
        let l = layout(&found, "Best matches first", |_| true);
        assert_eq!(l.tiles, [10, 20, 30], "the stack's cover is its best frame; burst 9 has one frame and is no stack");
        let mark = l.run_of(10).expect("a stack");
        assert!(mark.stacked && mark.first && mark.last);
        assert_eq!((mark.shown, mark.total), (3, 3));
        assert_eq!(l.run_of(30), None);
        // The viewer steps through every frame, each burst in shooting order.
        assert_eq!(l.visible, [11, 12, 10, 20, 30]);
        assert_eq!(l.sections.len(), 1);
        assert_eq!(l.sections[0].photos, 5);
        assert_eq!(l.sections[0].label, "Best matches first");
    }

    #[test]
    fn an_open_burst_shows_its_frames_in_shooting_order_in_its_place() {
        let found = vec![f(10, 0.9, when(1, 300), Some(7)), f(20, 0.8, when(5, 0), None), f(11, 0.7, when(1, 100), Some(7))];
        let l = layout(&found, "", |_| false);
        assert_eq!(l.tiles, [11, 10, 20]);
        let (a, b) = (l.run_of(11).unwrap(), l.run_of(10).unwrap());
        assert!(a.first && !a.last && !a.stacked);
        assert!(!b.first && b.last);
        assert_eq!(l.run_of(20), None);
        assert_eq!(l.frames_of(10), Some(&[11, 10][..]));
        assert_eq!(l.frames_of(20), None);
    }

    #[test]
    fn stacking_hands_the_selection_to_the_stack() {
        let found = vec![f(10, 0.9, when(1, 300), Some(7)), f(20, 0.8, when(5, 0), None), f(11, 0.7, when(1, 100), Some(7))];
        let stacked = layout(&found, "", |_| true);
        let (sel, anchor) = regather(&stacked, &[11, 20].into(), Some(11));
        assert_eq!(sel, [10, 20].into());
        assert_eq!(anchor, Some(10));
        assert_eq!(stacked.tile_of(11), Some(10));
        assert_eq!(stacked.tile_of(99), None);
    }

    #[test]
    fn arrows_step_by_tile_and_stop_at_the_ends() {
        let tiles = [10, 20, 30];
        assert_eq!(step(&tiles, None, true), Some(10));
        assert_eq!(step(&tiles, Some(10), true), Some(20));
        assert_eq!(step(&tiles, Some(30), true), Some(30));
        assert_eq!(step(&tiles, Some(10), false), Some(10));
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn records_come_from_the_library_index_and_pair_up() {
        let entry = |p: &str, ft: FileType| FileEntry { path: PathBuf::from(p), file_type: ft, size: 10, mtime: MTime::from_unix(1_700_000_000, 0) };
        let index: HashMap<PathBuf, FileEntry> = [
            entry("/lib/08-Jun-2024/a.JPG", FileType::Jpg),
            entry("/lib/08-Jun-2024/a.ARW", FileType::Raw),
            entry("/lib/08-Jun-2024/b.ARW", FileType::Raw),
        ]
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
        let hit = |jpg: Option<&str>, raw: Option<&str>, score: f32| Hit {
            id: 0,
            key: crate::search::index::Key([0; 16]),
            score,
            tagged: false,
            day: day(),
            capture: when(1, 250),
            burst: None,
            jpg: jpg.map(PathBuf::from),
            raw: raw.map(PathBuf::from),
        };
        let hits = [
            hit(Some("/lib/08-Jun-2024/a.JPG"), Some("/lib/08-Jun-2024/a.ARW"), 0.9),
            hit(None, Some("/lib/08-Jun-2024/b.ARW"), 0.8),
            hit(Some("/lib/gone.JPG"), None, 0.7),
        ];
        let r = build(3, "cameras".into(), &hits, |d, p| (d == day()).then(|| index.get(p).cloned()).flatten());
        assert_eq!((r.seq, r.found.len(), r.missing), (3, 2, 1));
        let a = r.coll.get(r.found[0].id).unwrap();
        assert_eq!(a.file_type, FileType::Jpg, "a pair's tile is its JPG");
        assert!(a.is_paired());
        assert_eq!(a.capture_time, Some(when(1, 250)), "the engine's capture time, to the sub-second");
        assert_eq!(r.coll.get(r.found[1].id).unwrap().file_type, FileType::Raw);
        assert_eq!(r.day_of(r.coll.find_pair(r.found[0].id).unwrap()), Some(day()), "either half finds its day");
    }

    #[test]
    fn the_list_offers_the_last_ten_searches_newest_first_until_forgotten() {
        let mut s = crate::settings::Settings::default();
        for i in 0..12 {
            s.remember_search(&format!("query {i}"));
        }
        let offered = completions(&s.search_recent, "");
        assert_eq!(offered.len(), 10, "the last 10");
        assert_eq!((offered[0], offered[9]), ("query 11", "query 2"), "newest first");
        s.remember_search("query 5");
        assert_eq!(completions(&s.search_recent, "qu")[0], "query 5", "run again, it comes first");
        assert_eq!(completions(&s.search_recent, "").len(), 10);
        // Preferences ▸ Search ▸ Forget them.
        s.forget_searches();
        assert!(completions(&s.search_recent, "").is_empty());
    }

    #[test]
    fn completions_match_the_start_of_a_word() {
        let recent: Vec<String> = ["red car at night", "cameras", "fireworks", "Cars"].iter().map(|s| s.to_string()).collect();
        assert_eq!(completions(&recent, "car"), ["red car at night", "Cars"]);
        assert_eq!(completions(&recent, "ca"), ["red car at night", "cameras", "Cars"]);
        assert_eq!(completions(&recent, "cars"), Vec::<&str>::new(), "what is typed exactly isn't offered again");
        assert_eq!(completions(&recent, "ight"), Vec::<&str>::new(), "not from the middle of a word");
        assert_eq!(completions(&recent, "").len(), 4);
    }
}
