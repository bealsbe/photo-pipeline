//! Filter + sort state for the grid, and the pipeline that turns the
//! collection into what it shows.
//!
//! As in the Python app, ordering is a plain stable sort of the source
//! records and filtering only removes rows — the grid's grouping by day
//! and one tile per RAW+JPG pair are built on top.

use std::cmp::Ordering;

use chrono::NaiveDate;

use super::collection::{Collection, RecordId};
use super::record::{FileType, PhotoRecord};
use crate::util::canonical_parent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Date,
    Name,
    Size,
}

impl SortKey {
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Date => "date",
            SortKey::Name => "name",
            SortKey::Size => "size",
        }
    }

    pub fn parse(s: &str) -> SortKey {
        match s {
            "name" => SortKey::Name,
            "size" => SortKey::Size,
            _ => SortKey::Date,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterState {
    pub show_raw: bool,
    pub show_jpg: bool,
    pub show_paired: bool,
    pub show_unpaired: bool,
    pub show_pruned: bool,
    pub show_unpruned: bool,
    pub show_marked: bool,
    pub show_unmarked: bool,
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub sort_key: SortKey,
    pub sort_asc: bool,
}

impl Default for FilterState {
    fn default() -> Self {
        Self {
            show_raw: true,
            show_jpg: true,
            show_paired: true,
            show_unpaired: true,
            show_pruned: true,
            show_unpruned: true,
            show_marked: true,
            show_unmarked: true,
            date_from: None,
            date_to: None,
            sort_key: SortKey::Date,
            sort_asc: true,
        }
    }
}

impl FilterState {
    pub fn accept(&self, r: &PhotoRecord) -> bool {
        if r.file_type == FileType::Raw && !self.show_raw {
            return false;
        }
        if r.file_type == FileType::Jpg && !self.show_jpg {
            return false;
        }
        if r.is_paired() && !self.show_paired {
            return false;
        }
        if !r.is_paired() && !self.show_unpaired {
            return false;
        }
        if r.is_pruned && !self.show_pruned {
            return false;
        }
        if !r.is_pruned && !self.show_unpruned {
            return false;
        }
        if r.is_marked && !self.show_marked {
            return false;
        }
        if !r.is_marked && !self.show_unmarked {
            return false;
        }
        if self.date_from.is_some() || self.date_to.is_some() {
            let d = r.shot_time().date();
            if self.date_from.is_some_and(|f| d < f) {
                return false;
            }
            if self.date_to.is_some_and(|t| d > t) {
                return false;
            }
        }
        true
    }

    /// Narrow to the photos still to review: neither pruned nor marked.
    pub fn only_to_review(self) -> Self {
        Self { show_pruned: false, show_marked: false, ..self }
    }

    /// Narrow to the pruned photos, whatever `show_pruned` said: marked
    /// as well or not.
    pub fn only_pruned(self) -> Self {
        Self { show_pruned: true, show_unpruned: false, ..self }
    }
}

/// How far culling has got, in shots: a shot is what the grid shows as one
/// tile, so a RAW+JPG pair counts once.  A shot is reviewed once it is
/// pruned or marked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub shots: usize,
    pub reviewed: usize,
}

impl Progress {
    pub fn to_review(&self) -> usize {
        self.shots - self.reviewed
    }

    /// Reviewed share, 0–1.
    pub fn fraction(&self) -> f32 {
        if self.shots == 0 { 0.0 } else { self.reviewed as f32 / self.shots as f32 }
    }
}

/// Count shots and how many are reviewed, ignoring every filter.
///
/// "To review" folds pairs exactly as [`dedup_pairs`] does over the To review
/// filter, so it always equals the tiles that filter shows: a pair's RAW
/// stands in for the shot only when none of its JPGs is still to review.
/// One pass with borrowed keys; it runs on every mark, and building
/// `dedup_pairs`' owned keys twice cost as much as the rest of a refresh.
pub fn progress(coll: &Collection) -> Progress {
    let to_review = |r: &PhotoRecord| !r.is_pruned && !r.is_marked;
    // Each paired JPG's shot, and whether any JPG of it is still to review.
    let mut jpgs: std::collections::HashMap<(&std::path::Path, &str), bool> = Default::default();
    for r in coll.iter() {
        if let (Some(stem), FileType::Jpg) = (&r.pair_stem, r.file_type) {
            *jpgs.entry((shot_parent(&r.path), stem.as_str())).or_default() |= to_review(r);
        }
    }
    let (mut shots, mut left) = (0, 0);
    for r in coll.iter() {
        let jpg_left = match (&r.pair_stem, r.file_type) {
            (Some(stem), FileType::Raw) => jpgs.get(&(shot_parent(&r.path), stem.as_str())).copied(),
            _ => None,
        };
        match jpg_left {
            // A RAW behind its JPG: no tile of its own, unless the JPG is
            // reviewed and the RAW isn't (halves marked apart).
            Some(jpg_left) => left += (to_review(r) && !jpg_left) as usize,
            None => {
                shots += 1;
                left += to_review(r) as usize;
            }
        }
    }
    Progress { shots, reviewed: shots.saturating_sub(left) }
}

/// [`canonical_parent`] without allocating.
fn shot_parent(path: &std::path::Path) -> &std::path::Path {
    let empty = std::path::Path::new("");
    let parent = path.parent().unwrap_or(empty);
    match parent.file_name().map(|n| n.to_string_lossy()) {
        Some(n) if n.eq_ignore_ascii_case("RAW") || n.eq_ignore_ascii_case("JPG") => parent.parent().unwrap_or(empty),
        _ => parent,
    }
}

/// Compare two records by the sort key (ascending).  Within one second,
/// dates fall back on the sub-second time and then the file name, so the
/// frames of a burst (eight or more a second) stay in shooting order.
pub fn compare(key: SortKey, a: &PhotoRecord, b: &PhotoRecord) -> Ordering {
    match key {
        SortKey::Date => a
            .shot_time()
            .cmp(&b.shot_time())
            .then_with(|| a.shot.subsec_ms.unwrap_or(0).cmp(&b.shot.subsec_ms.unwrap_or(0)))
            .then_with(|| a.name_lower.cmp(&b.name_lower)),
        SortKey::Name => a.name_lower.cmp(&b.name_lower),
        SortKey::Size => a.file_size.cmp(&b.file_size),
    }
}

/// Stable sort like Python's `list.sort(key=…, reverse=…)`: equal keys keep
/// their original relative order in both directions.
pub fn sort_ids(ids: &mut [RecordId], coll: &Collection, key: SortKey, asc: bool) {
    ids.sort_by(|&a, &b| {
        let (Some(ra), Some(rb)) = (coll.get(a), coll.get(b)) else {
            return Ordering::Equal;
        };
        let o = compare(key, ra, rb);
        if asc { o } else { o.reverse() }
    });
}

/// Every record the filter accepts, in sorted order.
pub fn visible_sorted(coll: &Collection, state: &FilterState) -> Vec<RecordId> {
    let mut ids: Vec<RecordId> = coll.ids().to_vec();
    sort_ids(&mut ids, coll, state.sort_key, state.sort_asc);
    ids.retain(|&id| coll.get(id).is_some_and(|r| state.accept(r)));
    ids
}

/// Drop the RAW of a pair when its JPG is also in the list — the grid and
/// viewer show one tile per shot.
pub fn dedup_pairs(ids: &[RecordId], coll: &Collection) -> Vec<RecordId> {
    let mut jpg_keys = std::collections::HashSet::new();
    for &id in ids {
        if let Some(r) = coll.get(id) {
            if let (Some(stem), FileType::Jpg) = (&r.pair_stem, r.file_type) {
                jpg_keys.insert((canonical_parent(&r.path), stem.clone()));
            }
        }
    }
    ids.iter()
        .copied()
        .filter(|&id| {
            let Some(r) = coll.get(id) else { return false };
            match (&r.pair_stem, r.file_type) {
                (Some(stem), FileType::Raw) => {
                    !jpg_keys.contains(&(canonical_parent(&r.path), stem.clone()))
                }
                _ => true,
            }
        })
        .collect()
}

/// A date group in the grid.
#[derive(Debug, Clone, PartialEq)]
pub struct DateGroup {
    pub date: NaiveDate,
    pub ids: Vec<RecordId>,
}

/// Group already-sorted ids by shot date.  Sections run oldest → newest,
/// or newest → oldest when sorting by date descending.
pub fn group_by_date(ids: &[RecordId], coll: &Collection, state: &FilterState) -> Vec<DateGroup> {
    let mut map: std::collections::HashMap<NaiveDate, Vec<RecordId>> = Default::default();
    for &id in ids {
        if let Some(r) = coll.get(id) {
            map.entry(r.shot_time().date()).or_default().push(id);
        }
    }
    let mut groups: Vec<DateGroup> = map.into_iter().map(|(date, ids)| DateGroup { date, ids }).collect();
    let reverse = state.sort_key == SortKey::Date && !state.sort_asc;
    groups.sort_by(|a, b| if reverse { b.date.cmp(&a.date) } else { a.date.cmp(&b.date) });
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;
    use chrono::NaiveDateTime;
    use std::path::PathBuf;

    fn rec(path: &str, day: u32) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        let t = NaiveDateTime::parse_from_str(&format!("2025-03-{day:02} 10:00:00"), "%Y-%m-%d %H:%M:%S").unwrap();
        PhotoRecord::new(p, ft, 10, MTime::from_unix(1_700_000_000, 0), Some(t))
    }

    /// Two pairs, a lone JPG and a lone RAW: four shots in six files.
    fn shoot() -> (Collection, [RecordId; 6]) {
        let mut c = Collection::new();
        let ids = [
            c.add(rec("/s/IMG_1.ARW", 1)),
            c.add(rec("/s/IMG_1.JPG", 1)),
            c.add(rec("/s/IMG_2.ARW", 1)),
            c.add(rec("/s/IMG_2.JPG", 1)),
            c.add(rec("/s/IMG_3.JPG", 2)),
            c.add(rec("/s/IMG_4.ARW", 2)),
        ];
        c.build_pairs();
        (c, ids)
    }

    fn set(c: &mut Collection, ids: &[RecordId], f: impl Fn(&mut PhotoRecord)) {
        for &id in ids {
            f(c.get_mut(id).unwrap());
        }
    }

    /// Tiles the grid would show for this filter.
    fn tiles(c: &Collection, st: &FilterState) -> usize {
        dedup_pairs(&visible_sorted(c, st), c).len()
    }

    #[test]
    fn to_review_hides_pruned_and_marked() {
        let (mut c, [a, b, d, e, ..]) = shoot();
        set(&mut c, &[a], |r| r.is_pruned = true);
        set(&mut c, &[d], |r| r.is_marked = true);
        set(&mut c, &[e], |r| {
            r.is_pruned = true;
            r.is_marked = true;
        });
        let st = FilterState::default().only_to_review();
        let accepted: Vec<bool> = [a, b, d, e].iter().map(|&id| st.accept(c.get(id).unwrap())).collect();
        assert_eq!(accepted, [false, true, false, false]);
        // "Show pruned" can't bring pruned photos back into the pile.
        let st = FilterState { show_pruned: true, ..FilterState::default() }.only_to_review();
        assert!(!st.accept(c.get(a).unwrap()));
    }

    #[test]
    fn pruned_shows_only_pruned_photos_whatever_show_pruned_says() {
        let (mut c, [raw1, jpg1, raw2, jpg2, jpg3, raw4]) = shoot();
        // Shot 1 pruned, shot 2 marked, the lone JPG pruned and marked.
        set(&mut c, &[raw1, jpg1], |r| r.is_pruned = true);
        set(&mut c, &[raw2, jpg2], |r| r.is_marked = true);
        set(&mut c, &[jpg3], |r| {
            r.is_pruned = true;
            r.is_marked = true;
        });
        for show_pruned in [false, true] {
            let st = FilterState { show_pruned, ..FilterState::default() }.only_pruned();
            let accepted: Vec<bool> = [raw1, jpg1, raw2, jpg2, jpg3, raw4].iter().map(|&id| st.accept(c.get(id).unwrap())).collect();
            assert_eq!(accepted, [true, true, false, false, true, false], "show_pruned = {show_pruned}");
            // One tile per shot: the pair as its JPG, and the lone JPG.
            let shown = dedup_pairs(&visible_sorted(&c, &st), &c);
            assert_eq!(shown, [jpg1, jpg3]);
        }
        // Un-pruning a shot takes it out at once.
        set(&mut c, &[raw1, jpg1], |r| r.is_pruned = false);
        let st = FilterState::default().only_pruned();
        assert_eq!(dedup_pairs(&visible_sorted(&c, &st), &c), [jpg3]);
        // With "Show pruned" off, the unfiltered view hides them instead.
        let all = FilterState { show_pruned: false, ..FilterState::default() };
        assert!(!all.accept(c.get(jpg3).unwrap()));
        assert!(all.accept(c.get(raw4).unwrap()));
    }

    #[test]
    fn pruned_combines_with_date_and_type_filters() {
        let (mut c, [raw1, jpg1, _, _, jpg3, raw4]) = shoot();
        set(&mut c, &[raw1, jpg1, jpg3, raw4], |r| r.is_pruned = true);
        let day2 = NaiveDate::from_ymd_opt(2025, 3, 2);
        // Day 2: the lone JPG and the lone RAW.
        let st = FilterState { date_from: day2, date_to: day2, ..FilterState::default() }.only_pruned();
        assert_eq!(dedup_pairs(&visible_sorted(&c, &st), &c), [jpg3, raw4]);
        // RAWs only: the pair's RAW, now its JPG is hidden, and the lone RAW.
        let raws = FilterState { show_jpg: false, ..FilterState::default() }.only_pruned();
        assert_eq!(tiles(&c, &raws), 2);
        // Pruned and to review can't both hold.
        let both = FilterState::default().only_pruned().only_to_review();
        assert_eq!(tiles(&c, &both), 0);
    }

    #[test]
    fn progress_counts_a_pair_once() {
        let (mut c, [raw1, jpg1, _, _, jpg3, raw4]) = shoot();
        assert_eq!(progress(&c), Progress { shots: 4, reviewed: 0 });
        // Marks propagate across a pair, so both halves carry them.
        set(&mut c, &[raw1, jpg1], |r| r.is_pruned = true);
        assert_eq!(progress(&c), Progress { shots: 4, reviewed: 1 });
        set(&mut c, &[jpg3], |r| r.is_marked = true);
        set(&mut c, &[raw4], |r| r.is_marked = true);
        let p = progress(&c);
        assert_eq!(p, Progress { shots: 4, reviewed: 3 });
        assert_eq!(p.to_review(), 1);
        assert_eq!(p.fraction(), 0.75);
        // Pruned and marked at once is still one reviewed shot.
        set(&mut c, &[jpg3], |r| r.is_pruned = true);
        assert_eq!(progress(&c).reviewed, 3);
    }

    #[test]
    fn progress_follows_pairs_across_sort_folders() {
        let mut c = Collection::new();
        c.add(rec("/s/RAW/IMG_1.ARW", 1));
        let jpg = c.add(rec("/s/JPG/IMG_1.JPG", 1));
        c.add(rec("/s/RAW/IMG_2.ARW", 1));
        c.build_pairs();
        assert_eq!(progress(&c), Progress { shots: 2, reviewed: 0 });
        set(&mut c, &[jpg], |r| r.is_marked = true);
        let raw = c.find_pair(jpg).unwrap();
        set(&mut c, &[raw], |r| r.is_marked = true);
        assert_eq!(progress(&c), Progress { shots: 2, reviewed: 1 });
    }

    #[test]
    fn frames_in_one_second_sort_in_shooting_order() {
        // Scanned in directory order, which isn't shooting order.
        let mut c = Collection::new();
        let mut add = |name: &str, ms: Option<u16>| {
            let mut r = rec(&format!("/s/{name}"), 1);
            r.shot.subsec_ms = ms;
            c.add(r)
        };
        let late = add("DSC0003.JPG", Some(900));
        let early = add("DSC0001.JPG", Some(100));
        let mid = add("DSC0002.JPG", Some(500));
        // An older body: whole seconds, so the file name decides.
        let b = add("B0002.JPG", None);
        let a = add("B0001.JPG", None);
        let up = visible_sorted(&c, &FilterState::default());
        assert_eq!(up, [a, b, early, mid, late]);
        let down = visible_sorted(&c, &FilterState { sort_asc: false, ..FilterState::default() });
        assert_eq!(down, [late, mid, early, b, a]);
    }

    #[test]
    fn empty_folder_has_no_progress() {
        let p = progress(&Collection::new());
        assert_eq!(p, Progress::default());
        assert_eq!(p.fraction(), 0.0);
    }

    #[test]
    fn to_review_count_matches_the_filtered_grid() {
        let (mut c, [raw1, jpg1, raw2, jpg2, _, raw4]) = shoot();
        let st = FilterState::default().only_to_review();
        assert_eq!(tiles(&c, &st), progress(&c).to_review());
        set(&mut c, &[raw2, jpg2], |r| r.is_marked = true);
        set(&mut c, &[raw4], |r| r.is_pruned = true);
        assert_eq!(tiles(&c, &st), 2);
        assert_eq!(progress(&c).to_review(), 2);
        // A pair whose halves disagree (marks loaded from an older sidecar):
        // the grid shows the unmarked RAW, so the shot is still to review.
        set(&mut c, &[jpg1], |r| r.is_pruned = true);
        assert!(!c.get(raw1).unwrap().is_pruned);
        assert_eq!(tiles(&c, &st), 2);
        assert_eq!(progress(&c), Progress { shots: 4, reviewed: 2 });
        // Sort order doesn't change the count.
        let desc = FilterState { sort_key: SortKey::Name, sort_asc: false, ..st.clone() };
        assert_eq!(tiles(&c, &desc), 2);
    }

    /// What the grid shows, the slow way: tiles with no filter, and tiles
    /// with the To review filter.
    fn reference(c: &Collection) -> Progress {
        let shots = tiles(c, &FilterState::default());
        let left = tiles(c, &FilterState::default().only_to_review());
        Progress { shots, reviewed: shots.saturating_sub(left) }
    }

    #[test]
    fn progress_agrees_with_the_grid_for_any_marks() {
        // Pairs at the root and across RAW/ and JPG/, a shot with two JPGs,
        // one with two RAWs, and lone files; marks set at random, so halves
        // of a pair often disagree.
        let names = [
            "/s/A.ARW", "/s/A.JPG", "/s/RAW/B.ARW", "/s/JPG/b.jpg", "/s/C.ARW", "/s/C.JPG", "/s/JPG/C.jpeg",
            "/s/D.ARW", "/s/RAW/D.ARW", "/s/D.JPG", "/s/E.JPG", "/s/F.ARW", "/t/A.JPG", "/s/raw/G.arw", "/s/G.JPG",
        ];
        let mut c = Collection::new();
        let ids: Vec<RecordId> = names.iter().map(|n| c.add(rec(n, 1))).collect();
        c.build_pairs();
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2000 {
            for &id in &ids {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let r = c.get_mut(id).unwrap();
                r.is_pruned = seed & 3 == 0;
                r.is_marked = seed & 12 == 0;
            }
            assert_eq!(progress(&c), reference(&c));
        }
        // And the shot count itself: A, B, C (two JPG tiles), D, E, F, /t/A, G.
        assert_eq!(progress(&c).shots, 9);
    }

    #[test]
    fn to_review_combines_with_date_and_type_filters() {
        let (mut c, [_, jpg1, raw2, jpg2, jpg3, raw4]) = shoot();
        set(&mut c, &[raw2, jpg2], |r| r.is_marked = true);
        let day2 = NaiveDate::from_ymd_opt(2025, 3, 2);
        // Day 2 holds the lone JPG and the lone RAW, both unreviewed.
        let st = FilterState { date_from: day2, date_to: day2, ..FilterState::default() }.only_to_review();
        let shown = dedup_pairs(&visible_sorted(&c, &st), &c);
        assert_eq!(shown.len(), 2);
        let groups = group_by_date(&shown, &c, &st);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].date, day2.unwrap());
        // Day 1 has one pair left to review, shown as its JPG.
        let day1 = NaiveDate::from_ymd_opt(2025, 3, 1);
        let st1 = FilterState { date_from: day1, date_to: day1, ..FilterState::default() }.only_to_review();
        let shown = dedup_pairs(&visible_sorted(&c, &st1), &c);
        assert_eq!(shown.len(), 1);
        assert_eq!(c.get(shown[0]).unwrap().file_type, FileType::Jpg);
        // RAWs only: the lone RAW, and the pair's RAW now its JPG is hidden.
        let raws = FilterState { show_jpg: false, ..FilterState::default() }.only_to_review();
        assert_eq!(tiles(&c, &raws), 2);
        set(&mut c, &[raw4], |r| r.is_pruned = true);
        assert_eq!(tiles(&c, &raws), 1);
        // One tile per shot still to review: the pair as its JPG, and the
        // lone JPG.
        let all_days = FilterState::default().only_to_review();
        assert_eq!(dedup_pairs(&visible_sorted(&c, &all_days), &c), [jpg1, jpg3]);
        // Marked only and to review can't both hold.
        let both = FilterState { show_unmarked: false, ..FilterState::default() }.only_to_review();
        assert_eq!(tiles(&c, &both), 0);
    }
}
