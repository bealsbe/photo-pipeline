//! Which days of the library hold photos.
//!
//! Dates come from the path first — the library is laid out by date
//! (`2025/Mar-2025/12-Mar-2025/RAW/…`), so a file's day is already in its
//! folder name.  In priority order:
//!
//! 1. an ancestor folder named `DD-Mon-YYYY` (the day folder);
//! 2. a renamed stem `YYYYMMDD_HHMM_NNNN` (old month-level libraries);
//! 3. EXIF capture time, then mtime — anything else dropped into the tree.
//!
//! The index is built from a [`Tree`] of the library's folders, which
//! `library_cache` keeps between runs and refreshes from disk.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use parking_lot::Mutex;
use rayon::prelude::*;

use super::library_cache::{Store, Tree, folder_path, read_capture_times};
use super::record::{FileType, MTime, PhotoRecord};
use crate::util::{self, canonical_parent};

pub const MONTH_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
pub const MONTH_NAME: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];

/// Number of shading steps in the year heat map (0 = no photos).
pub const HEAT_LEVELS: usize = 4;

const MIN_YEAR: i32 = 1990;

/// A file found in the library: path, type and the stat fields a record needs.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub file_type: FileType,
    pub size: u64,
    pub mtime: MTime,
}

impl FileEntry {
    /// A record without its capture time — fields match the scanner's, so the
    /// disk thumbnail cache hits the same entries.
    pub fn record(&self) -> PhotoRecord {
        PhotoRecord::new(self.path.clone(), self.file_type, self.size, self.mtime, None)
    }
}

fn plausible(d: NaiveDate) -> bool {
    d.year() >= MIN_YEAR && d.year() <= Local::now().year() + 1
}

/// Parse a `12-Mar-2025` folder name.
pub fn date_from_day_folder(name: &str) -> Option<NaiveDate> {
    let mut parts = name.splitn(3, '-');
    let (d, m, y) = (parts.next()?, parts.next()?, parts.next()?);
    if d.len() != 2 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if y.len() != 4 || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if m.is_empty() || !m.chars().all(char::is_alphabetic) {
        return None;
    }
    let ml = m.to_lowercase();
    let month = MONTH_ABBR.iter().position(|a| a.to_lowercase() == ml)? as u32 + 1;
    let date = NaiveDate::from_ymd_opt(y.parse().ok()?, month, d.parse().ok()?)?;
    plausible(date).then_some(date)
}

/// Parse the date out of a `20250312_1430_0007` stem.
pub fn date_from_renamed_stem(stem: &str) -> Option<NaiveDate> {
    let b = stem.as_bytes();
    if b.len() < 15 || b[8] != b'_' || b[13] != b'_' {
        return None;
    }
    if !b[..8].iter().all(u8::is_ascii_digit) || !b[9..13].iter().all(u8::is_ascii_digit) {
        return None;
    }
    // At least one sequence digit after the second underscore.
    if !b.get(14).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let date = NaiveDate::parse_from_str(&stem[..8], "%Y%m%d").ok()?;
    plausible(date).then_some(date)
}

/// The capture time a renamed stem records, to the minute: the export named
/// the file after it.
pub fn time_from_renamed_stem(stem: &str) -> Option<NaiveDateTime> {
    let date = date_from_renamed_stem(stem)?;
    date.and_hms_opt(stem[9..11].parse().ok()?, stem[11..13].parse().ok()?, 0)
}

/// The date of the innermost day folder on a relative folder path.
fn folder_day(rel: &Path) -> Option<NaiveDate> {
    rel.components().filter_map(|c| date_from_day_folder(&c.as_os_str().to_string_lossy())).last()
}

/// Every library file shot on one calendar day.
#[derive(Debug, Clone)]
pub struct LibraryDay {
    pub date: NaiveDate,
    pub files: Vec<FileEntry>,
    pub shot_count: usize,
    pub raw_count: usize,
    pub jpg_count: usize,
    pub folder: Option<PathBuf>,
    /// Shots (RAW+JPG sharing stem and canonical parent), sorted by name.
    shots: Vec<Vec<usize>>,
}

impl LibraryDay {
    fn new(date: NaiveDate) -> Self {
        Self {
            date,
            files: Vec::new(),
            shot_count: 0,
            raw_count: 0,
            jpg_count: 0,
            folder: None,
            shots: Vec::new(),
        }
    }

    fn finalise(&mut self) {
        // The path breaks ties, so the order never depends on read order.
        self.files.sort_by_cached_key(|e| (util::file_name(&e.path).to_lowercase(), e.path.clone()));
        self.raw_count = self.files.iter().filter(|e| e.file_type == FileType::Raw).count();
        self.jpg_count = self.files.len() - self.raw_count;

        let mut shots: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
        let mut order: Vec<(PathBuf, String)> = Vec::new();
        for (i, e) in self.files.iter().enumerate() {
            let key = (canonical_parent(&e.path), util::stem(&e.path).to_lowercase());
            shots
                .entry(key.clone())
                .or_insert_with(|| {
                    order.push(key);
                    Vec::new()
                })
                .push(i);
        }
        let mut list: Vec<Vec<usize>> = order.into_iter().filter_map(|k| shots.remove(&k)).collect();
        list.sort_by_cached_key(|s| util::file_name(&self.files[s[0]].path).to_lowercase());
        self.shot_count = list.len();
        self.shots = list;

        // Prefer the day folder; fall back to the most common parent.
        let mut parents: Vec<(PathBuf, usize)> = Vec::new();
        for e in &self.files {
            let p = canonical_parent(&e.path);
            match parents.iter_mut().find(|(q, _)| *q == p) {
                Some((_, n)) => *n += 1,
                None => parents.push((p, 1)),
            }
        }
        let day_dir = parents
            .iter()
            .find(|(p, _)| date_from_day_folder(&util::file_name(p)) == Some(self.date))
            .map(|(p, _)| p.clone());
        self.folder = day_dir.or_else(|| {
            let mut best: Option<&(PathBuf, usize)> = None;
            for p in &parents {
                if best.is_none_or(|b| p.1 > b.1) {
                    best = Some(p);
                }
            }
            best.map(|b| b.0.clone())
        });
    }

    /// Up to `n` shots spread evenly through the day, preferring each shot's
    /// JPG (its embedded preview decodes far faster than a RAW thumbnail).
    pub fn preview_entries(&self, n: usize) -> Vec<&FileEntry> {
        let picked: Vec<&Vec<usize>> = if self.shots.len() <= n {
            self.shots.iter().collect()
        } else {
            let step = self.shots.len() as f64 / n as f64;
            (0..n).map(|i| &self.shots[(i as f64 * step + step / 2.0) as usize]).collect()
        };
        picked
            .into_iter()
            .map(|shot| {
                let jpg = shot.iter().find(|&&i| self.files[i].file_type == FileType::Jpg);
                &self.files[*jpg.unwrap_or(&shot[0])]
            })
            .collect()
    }
}

/// Days of the library that hold photos, with per-day counts.
#[derive(Debug, Clone)]
pub struct LibraryIndex {
    pub root: PathBuf,
    pub days: BTreeMap<NaiveDate, LibraryDay>,
    pub total_shots: usize,
    pub total_files: usize,
    thresholds: Vec<usize>,
}

impl LibraryIndex {
    fn new(root: PathBuf, mut days: BTreeMap<NaiveDate, LibraryDay>) -> Self {
        days.par_iter_mut().for_each(|(_, d)| d.finalise());
        let total_shots = days.values().map(|d| d.shot_count).sum();
        let total_files = days.values().map(|d| d.files.len()).sum();
        // Quantile thresholds, so one 2 000-shot wedding day does not push
        // every other day into the palest shade.
        let mut counts: Vec<usize> = days.values().map(|d| d.shot_count).collect();
        counts.sort_unstable();
        let thresholds = if counts.is_empty() {
            Vec::new()
        } else {
            (1..HEAT_LEVELS)
                .map(|q| counts[(counts.len() - 1).min(counts.len() * q / HEAT_LEVELS)])
                .collect()
        };
        Self { root, days, total_shots, total_files, thresholds }
    }

    pub fn len(&self) -> usize {
        self.days.len()
    }

    pub fn is_empty(&self) -> bool {
        self.days.is_empty()
    }

    pub fn get(&self, d: NaiveDate) -> Option<&LibraryDay> {
        self.days.get(&d)
    }

    pub fn years(&self) -> Vec<i32> {
        let mut y: Vec<i32> = self.days.keys().map(|d| d.year()).collect();
        y.dedup();
        y
    }

    pub fn days_in_month(&self, year: i32, month: u32) -> impl Iterator<Item = &LibraryDay> {
        let start = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or_default();
        let (ny, nm) = month_shift(year, month, 1);
        let end = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap_or(NaiveDate::MAX);
        self.days.range(start..end).map(|(_, d)| d)
    }

    pub fn month_shot_count(&self, year: i32, month: u32) -> usize {
        self.days_in_month(year, month).map(|d| d.shot_count).sum()
    }

    pub fn year_shot_count(&self, year: i32) -> usize {
        self.days.values().filter(|d| d.date.year() == year).map(|d| d.shot_count).sum()
    }

    pub fn year_day_count(&self, year: i32) -> usize {
        self.days.keys().filter(|d| d.year() == year).count()
    }

    /// Nearest day with photos after (+1) or before (-1) `d`.
    pub fn adjacent_day(&self, d: NaiveDate, direction: i32) -> Option<NaiveDate> {
        if direction > 0 {
            self.days.range(d.succ_opt()?..).next().map(|(k, _)| *k)
        } else {
            self.days.range(..d).next_back().map(|(k, _)| *k)
        }
    }

    /// The day with photos closest to `d` (either side; earlier wins ties).
    pub fn nearest_day(&self, d: NaiveDate) -> Option<NaiveDate> {
        if self.days.contains_key(&d) {
            return Some(d);
        }
        let after = self.adjacent_day(d, 1);
        let before = self.adjacent_day(d, -1);
        match (before, after) {
            (Some(b), Some(a)) => Some(if (d - b) <= (a - d) { b } else { a }),
            (b, a) => b.or(a),
        }
    }

    pub fn latest_day(&self) -> Option<NaiveDate> {
        self.days.keys().next_back().copied()
    }

    /// 0 for no photos, then 1..=HEAT_LEVELS by quantile of shot count.
    pub fn heat_level(&self, day: Option<&LibraryDay>) -> usize {
        match day {
            None => 0,
            Some(d) if d.shot_count == 0 => 0,
            Some(d) => 1 + self.thresholds.partition_point(|&t| t < d.shot_count),
        }
    }
}

pub fn month_shift(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let m = year * 12 + (month as i32 - 1) + delta;
    (m.div_euclid(12), (m.rem_euclid(12) + 1) as u32)
}

/// Bucket every file of `tree` by day.  Files whose path gives no date get
/// their EXIF capture time, from the store's cache or read now (and cached).
/// `None` if cancelled part-way.
pub fn index_from_tree(root: &Path, tree: &Tree, store: &Mutex<Store>, cancelled: &AtomicBool) -> Option<LibraryIndex> {
    let placed: Vec<(Option<NaiveDate>, FileEntry)> = tree
        .folders
        .par_iter()
        .flat_map_iter(|(rel, folder)| {
            let day = folder_day(rel);
            let dir = folder_path(root, rel);
            folder.files.iter().map(move |f| {
                let fe = FileEntry {
                    path: dir.join(&f.name),
                    file_type: f.file_type,
                    size: f.size,
                    mtime: MTime::from_unix(f.mtime.0, f.mtime.1),
                };
                (day.or_else(|| date_from_renamed_stem(&util::stem(&fe.path))), fe)
            })
        })
        .collect();

    let mut days: BTreeMap<NaiveDate, LibraryDay> = BTreeMap::new();
    let mut unresolved: Vec<FileEntry> = Vec::new();
    for (d, fe) in placed {
        match d {
            Some(d) => days.entry(d).or_insert_with(|| LibraryDay::new(d)).files.push(fe),
            None => unresolved.push(fe),
        }
    }

    if !unresolved.is_empty() {
        let known: Vec<Option<Option<NaiveDateTime>>> = {
            let st = store.lock();
            unresolved.iter().map(|fe| st.time(fe)).collect()
        };
        let to_read: Vec<usize> = (0..unresolved.len()).filter(|&i| known[i].is_none()).collect();
        let read = read_capture_times(&to_read.iter().map(|&i| &unresolved[i]).collect::<Vec<_>>());
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        {
            let mut st = store.lock();
            for (&i, t) in to_read.iter().zip(&read) {
                st.set_time(&unresolved[i], *t);
            }
        }
        let mut read = read.into_iter();
        for (fe, k) in unresolved.into_iter().zip(known) {
            let ct = match k {
                Some(t) => t,
                None => read.next().flatten(),
            };
            let d = match ct {
                Some(t) if plausible(t.date()) => t.date(),
                _ => fe.mtime.local.date(),
            };
            days.entry(d).or_insert_with(|| LibraryDay::new(d)).files.push(fe);
        }
    }
    Some(LibraryIndex::new(root.to_path_buf(), days))
}

/// Records for one day's files, touching the files only where it must.  A
/// capture time comes from the store when it was read before, else from a
/// renamed stem.  Returns the records and the indices of the files that
/// still need an EXIF read.
pub fn day_records(files: &[FileEntry], store: &Store) -> (Vec<PhotoRecord>, Vec<usize>) {
    let mut missing = Vec::new();
    let records = files
        .iter()
        .enumerate()
        .map(|(i, fe)| {
            let ct = store.time(fe).unwrap_or_else(|| {
                let t = time_from_renamed_stem(&util::stem(&fe.path));
                if t.is_none() {
                    missing.push(i);
                }
                t
            });
            PhotoRecord::new(fe.path.clone(), fe.file_type, fe.size, fe.mtime, ct)
        })
        .collect();
    (records, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_day_folders_and_stems() {
        assert_eq!(date_from_day_folder("12-Mar-2025"), NaiveDate::from_ymd_opt(2025, 3, 12));
        assert_eq!(date_from_day_folder("12-mar-2025"), NaiveDate::from_ymd_opt(2025, 3, 12));
        assert_eq!(date_from_day_folder("1-Mar-2025"), None);
        assert_eq!(date_from_day_folder("31-Feb-2025"), None);
        assert_eq!(date_from_day_folder("12-Mar-1980"), None);
        assert_eq!(date_from_renamed_stem("20250312_1430_0007"), NaiveDate::from_ymd_opt(2025, 3, 12));
        assert_eq!(date_from_renamed_stem("20250312_1430_0007_1"), NaiveDate::from_ymd_opt(2025, 3, 12));
        assert_eq!(date_from_renamed_stem("IMG_0001"), None);
        let t = time_from_renamed_stem("20250312_1430_0007").unwrap();
        assert_eq!(t.format("%Y-%m-%d %H:%M:%S").to_string(), "2025-03-12 14:30:00");
        assert_eq!(time_from_renamed_stem("20250312_2475_0007"), None);
        assert_eq!(folder_day(Path::new("2025/Mar-2025/12-Mar-2025/RAW")), NaiveDate::from_ymd_opt(2025, 3, 12));
        assert_eq!(folder_day(Path::new("2025/Mar-2025")), None);
    }

    #[test]
    fn month_shift_wraps() {
        assert_eq!(month_shift(2025, 1, -1), (2024, 12));
        assert_eq!(month_shift(2025, 12, 1), (2026, 1));
    }
}
