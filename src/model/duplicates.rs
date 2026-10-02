//! Duplicate files, and which copy to suggest pruning.
//!
//! Two tiers:
//!
//! - **Exact**: identical files.  Only files of one type and one size can
//!   be, and only with the same capture time, sequence number and camera
//!   in their headers, all of which the scan already knows.  Only files
//!   sharing all of those with another are read at all: their first 64 KB
//!   (EXIF, with the shutter count), then, where those match, 64 KB from
//!   the middle and the end (`analysis` reads; the grouping is here).  On
//!   shoot A of the image-quality plan, 9 224 of 9 547 RAWs and 40 of
//!   9 536 JPGs share their size with another file, but no two of those
//!   JPGs share a capture.  Whole files are not compared: on its day D (a
//!   day exported twice) every file has a copy of its size, and comparing
//!   them whole would read 340 GB from the share instead of 2.6 GB.  A RAW
//!   and its JPG are never duplicates: different types never share a bucket.
//! - **Same capture**: one frame under two files that differ in bytes (a
//!   re-saved or re-tagged JPG): the same camera, capture time to the
//!   millisecond and press sequence number, of one type, and pHashes at
//!   most [`SAME_CAPTURE_HASH`] bits apart.  Worked out per shot from
//!   metadata and the analysis, at no extra I/O.
//!
//! In each group one copy is kept and the others are *suggested* for
//! pruning (decision 7b): a copy-like name first ("copy", "(1)", a "_1"
//! added to another copy's name); then, between exports into the library
//! (`YYYYMMDD_HHMM_NNNN`), the later export, which has the higher number;
//! otherwise the newer file.  Nothing is marked: the suggestion is shown,
//! and the bulk action asks first.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;

use super::collection::{Collection, RecordId};
use super::record::{FileType, PhotoRecord};

/// pHash bits two files of one capture may differ by (re-encoding moves a
/// few; another frame of a burst moves ~8 in the median).
pub const SAME_CAPTURE_HASH: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Identical files: one type and size, the same bytes at the start,
    /// middle and end.
    Exact,
    /// One capture in files that differ (shot representatives).
    SameCapture,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    pub kind: Kind,
    /// Every copy, the kept one first.
    pub ids: Vec<RecordId>,
    /// The copy to keep.
    pub keep: RecordId,
    /// The copies suggested for pruning.
    pub prune: Vec<RecordId>,
}

// ── finding identical files (the engine reads, this decides) ──────────────

/// Indices of files that could be identical: same type and size, in
/// buckets of two or more (each bucket in input order).
pub fn size_buckets(files: &[(FileType, u64)]) -> Vec<Vec<usize>> {
    let mut by: HashMap<(FileType, u64), Vec<usize>> = HashMap::new();
    for (i, &(t, size)) in files.iter().enumerate() {
        if size > 0 {
            by.entry((t, size)).or_default().push(i);
        }
    }
    let mut out: Vec<Vec<usize>> = by.into_values().filter(|b| b.len() >= 2).collect();
    out.sort();
    out
}

/// Split a bucket by a digest: the groups of two or more with equal
/// digests.  Files with no digest (unreadable) are left out.
pub fn split_by<D: Eq + Hash>(bucket: &[usize], digest: impl Fn(usize) -> Option<D>) -> Vec<Vec<usize>> {
    let mut by: HashMap<D, Vec<usize>> = HashMap::new();
    for &i in bucket {
        if let Some(d) = digest(i) {
            by.entry(d).or_default().push(i);
        }
    }
    let mut out: Vec<Vec<usize>> = by.into_values().filter(|g| g.len() >= 2).collect();
    out.sort();
    out
}

// ── which copy to keep ──────────────────────────────────────────────────────

/// Does `stem` read like a copy of another name in the group?  "copy"
/// anywhere as a word ("IMG_1 copy", "Copy of IMG_1", "IMG_1 - Copy (2)"),
/// a trailing "(n)", or another member's stem plus "_n", "-n" or " n".
pub fn copy_like(stem: &str, others: &[&str]) -> bool {
    let s = stem.to_lowercase();
    if s.match_indices("copy").any(|(i, _)| !s[i + 4..].starts_with(|c: char| c.is_alphabetic())) {
        return true;
    }
    if let Some(inner) = s.trim_end().strip_suffix(')').and_then(|t| t.rsplit_once('(')).map(|(_, n)| n) {
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    others.iter().any(|o| {
        let o = o.to_lowercase();
        s.len() > o.len()
            && s.starts_with(&o)
            && s[o.len()..].strip_prefix(['_', '-', ' ']).is_some_and(|n| (1..=3).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit()))
    })
}

/// The sequence number of a library export's renamed file
/// (`20260523_0846_3356` → 3356).
pub fn export_number(stem: &str) -> Option<u32> {
    let b = stem.as_bytes();
    let ok = b.len() >= 18
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'_'
        && b[9..13].iter().all(u8::is_ascii_digit)
        && b[13] == b'_'
        && b[14..].iter().all(u8::is_ascii_digit);
    if ok { stem[14..].parse().ok() } else { None }
}

/// Which of `copies` to keep (an index): not copy-like, else the earlier
/// export (lower number, when every copy is a library export), else the
/// older file, else the first name.
pub fn keeper(copies: &[&PhotoRecord]) -> usize {
    let stems: Vec<String> = copies.iter().map(|r| r.stem()).collect();
    let all_exports = stems.iter().all(|s| export_number(s).is_some());
    let key = |i: usize| {
        let others: Vec<&str> = stems.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, s)| s.as_str()).collect();
        let copy = copy_like(&stems[i], &others);
        let export = if all_exports { export_number(&stems[i]).unwrap_or(0) } else { 0 };
        (copy, export, copies[i].modified.timestamp.to_bits(), copies[i].name_lower.clone())
    };
    (0..copies.len()).min_by(|&a, &b| {
        let (ka, kb) = (key(a), key(b));
        (ka.0, ka.1)
            .cmp(&(kb.0, kb.1))
            .then(f64::from_bits(ka.2).total_cmp(&f64::from_bits(kb.2)))
            .then(ka.3.cmp(&kb.3))
    })
    .unwrap_or(0)
}

/// A group of copies, with the keeper chosen.
pub fn group(coll: &Collection, kind: Kind, ids: &[RecordId]) -> Option<DuplicateGroup> {
    let recs: Vec<&PhotoRecord> = ids.iter().filter_map(|&id| coll.get(id)).collect();
    if recs.len() < 2 || recs.len() != ids.len() {
        return None;
    }
    let k = keeper(&recs);
    let keep = ids[k];
    let mut all = vec![keep];
    all.extend(ids.iter().copied().filter(|&id| id != keep));
    Some(DuplicateGroup { kind, prune: all[1..].to_vec(), ids: all, keep })
}

/// Identical groups (as paths, from the analysis) as record groups.
/// Paths no longer in the collection are dropped.
pub fn exact_groups(coll: &Collection, paths: &[Vec<PathBuf>]) -> Vec<DuplicateGroup> {
    paths
        .iter()
        .filter_map(|g| {
            let ids: Vec<RecordId> = g.iter().filter_map(|p| coll.id_of(p)).collect();
            group(coll, Kind::Exact, &ids)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;

    fn rec(path: &str, mtime: i64) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        PhotoRecord::new(p, ft, 10, MTime::from_unix(mtime, 0), None)
    }

    fn kept(names: &[(&str, i64)]) -> String {
        let recs: Vec<PhotoRecord> = names.iter().map(|(n, t)| rec(&format!("/s/{n}"), *t)).collect();
        let refs: Vec<&PhotoRecord> = recs.iter().collect();
        recs[keeper(&refs)].filename()
    }

    #[test]
    fn copy_like_names() {
        for s in ["IMG_1 copy", "IMG_1 Copy", "Copy of IMG_1", "IMG_1 - Copy (2)", "IMG_1 copy 2", "IMG_1(1)", "IMG_1 (12)"] {
            assert!(copy_like(s, &["IMG_1"]), "{s}");
        }
        for s in ["IMG_1", "copyright", "DSC_0001", "20260523_0846_3356", "IMG_1 (a)"] {
            assert!(!copy_like(s, &["IMG_2"]), "{s}");
        }
        // "_1" only counts as an added suffix: DSC_0001 isn't a copy of DSC.
        assert!(copy_like("DSC01234_1", &["DSC01234"]));
        assert!(copy_like("DSC01234-2", &["DSC01234"]));
        assert!(!copy_like("DSC01234_1", &["DSC01235"]));
        assert!(!copy_like("DSC_0001", &["DSC"]), "four digits is a camera number, not a copy suffix");
        assert!(!copy_like("DSC01234", &["DSC01234_1"]));
    }

    #[test]
    fn export_numbers() {
        assert_eq!(export_number("20260523_0846_3356"), Some(3356));
        assert_eq!(export_number("20260523_0846_0001"), Some(1));
        assert_eq!(export_number("20260523_0846_12345"), Some(12345));
        assert_eq!(export_number("DSC01234"), None);
        assert_eq!(export_number("20260523_0846_3356 copy"), None);
    }

    #[test]
    fn which_copy_is_kept() {
        // A copy-like name loses, whatever the dates.
        assert_eq!(kept(&[("IMG_1 copy.JPG", 100), ("IMG_1.JPG", 200)]), "IMG_1.JPG");
        assert_eq!(kept(&[("DSC01234_1.JPG", 100), ("DSC01234.JPG", 200)]), "DSC01234.JPG");
        assert_eq!(kept(&[("IMG_1 (1).JPG", 100), ("IMG_1.JPG", 200), ("IMG_1 (2).JPG", 50)]), "IMG_1.JPG");
        // Two library exports: the earlier one (lower number), even when
        // its file is newer.
        assert_eq!(kept(&[("20260523_0846_3356.JPG", 100), ("20260523_0846_0001.JPG", 200)]), "20260523_0846_0001.JPG");
        // Otherwise the older file.
        assert_eq!(kept(&[("b.JPG", 200), ("a.JPG", 100)]), "a.JPG");
        // Same age: the first name, so the choice is stable.
        assert_eq!(kept(&[("b.JPG", 100), ("a.JPG", 100)]), "a.JPG");
        // An export and a camera name: no export rule; the older one.
        assert_eq!(kept(&[("20260523_0846_0001.JPG", 200), ("DSC01234.JPG", 100)]), "DSC01234.JPG");
    }

    #[test]
    fn buckets_only_hold_same_type_and_size() {
        let files = [
            (FileType::Jpg, 100),
            (FileType::Raw, 100), // a RAW the size of a JPG: never compared
            (FileType::Jpg, 100),
            (FileType::Jpg, 101),
            (FileType::Raw, 500),
            (FileType::Raw, 500),
            (FileType::Raw, 500),
            (FileType::Jpg, 0),
            (FileType::Jpg, 0),
        ];
        assert_eq!(size_buckets(&files), vec![vec![0, 2], vec![4, 5, 6]]);
        let digest = |i: usize| match i {
            4 | 6 => Some(1u64),
            5 => Some(2),
            _ => None,
        };
        assert_eq!(split_by(&[4, 5, 6], digest), vec![vec![4, 6]]);
        assert!(split_by(&[0, 2], digest).is_empty(), "unreadable files are left out");
    }

    #[test]
    fn groups_map_paths_to_records_and_drop_missing_ones() {
        let mut c = Collection::new();
        let a = c.add(rec("/s/20260523_0846_0001.JPG", 100));
        let b = c.add(rec("/s/20260523_0846_3356.JPG", 100));
        c.add(rec("/s/x.JPG", 100));
        let groups = exact_groups(
            &c,
            &[
                vec!["/s/20260523_0846_3356.JPG".into(), "/s/20260523_0846_0001.JPG".into()],
                vec!["/s/x.JPG".into(), "/s/gone.JPG".into()],
            ],
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], DuplicateGroup { kind: Kind::Exact, ids: vec![a, b], keep: a, prune: vec![b] });
    }
}
