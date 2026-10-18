//! Library export: date-based destinations, optional renaming, conflict
//! resolution, and execution.
//!
//! Layout: `root/2025/Mar-2025/12-Mar-2025/RAW/…`.
//!
//! Renaming (`rename = true`) names files `YYYYMMDD_HHMM_NNNN.ext` from the
//! shot time.  The sequence counts per calendar day in capture order and
//! carries on from the highest number already in the library for that day
//! (day folder or the older month-level folders).  Both halves of a pair
//! share one number and one datetime.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, Timelike};

use super::crop_export::{remove_xmp_crop, render_cropped_jpeg, write_xmp_crop};
use super::{Progress, fsutil};
use crate::model::collection::{Collection, RecordId};
use crate::model::crop::Crop;
use crate::model::library_index::MONTH_ABBR;
use crate::model::record::{FileType, PhotoRecord};
use crate::util;

const MIN_YEAR: i32 = 1990;

fn valid(dt: &NaiveDateTime) -> bool {
    dt.year() >= MIN_YEAR && dt.year() <= Local::now().year() + 1
}

/// Most reliable shot datetime for a record, pair-aware:
/// JPG capture time → RAW capture time → RAW mtime → the record's mtime.
/// Implausible dates (pre-1990, far future) count as absent.
pub fn best_datetime(record: &PhotoRecord, pair: Option<&PhotoRecord>) -> NaiveDateTime {
    let jpg = if record.file_type == FileType::Jpg { Some(record) } else { pair };
    let raw = if record.file_type == FileType::Raw { Some(record) } else { pair };
    if let Some(t) = jpg.and_then(|r| r.capture_time).filter(valid) {
        return t;
    }
    if let Some(t) = raw.and_then(|r| r.capture_time).filter(valid) {
        return t;
    }
    if let Some(r) = raw {
        if valid(&r.modified.local) {
            return r.modified.local;
        }
    }
    record.modified.local
}

pub fn month_folder(d: NaiveDate) -> String {
    format!("{}-{}", MONTH_ABBR[d.month0() as usize], d.year())
}

pub fn day_folder(d: NaiveDate) -> String {
    format!("{:02}-{}", d.day(), month_folder(d))
}

pub fn month_dir(root: &Path, d: NaiveDate) -> PathBuf {
    root.join(d.year().to_string()).join(month_folder(d))
}

pub fn day_dir(root: &Path, d: NaiveDate) -> PathBuf {
    month_dir(root, d).join(day_folder(d))
}

/// `20250312_1430_0007.CR3`
pub fn library_name(dt: NaiveDateTime, seq: u32, suffix: &str) -> String {
    format!("{}_{:02}{:02}_{:04}{}", dt.format("%Y%m%d"), dt.hour(), dt.minute(), seq, suffix)
}

/// `(day, seq)` from a renamed stem; anchored, so `…_0007_1` is not seq 7.
fn parse_seq(stem: &str) -> Option<(String, u32)> {
    let b = stem.as_bytes();
    if b.len() < 15 || b[8] != b'_' || b[13] != b'_' {
        return None;
    }
    if !b[..8].iter().all(u8::is_ascii_digit) || !b[9..13].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let tail = &stem[14..];
    if tail.is_empty() || !tail.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((stem[..8].to_string(), tail.parse().ok()?))
}

/// Highest sequence already used per day in `folder`'s RAW/ and JPG/.
fn existing_sequences(folder: &Path) -> HashMap<String, u32> {
    let mut highest: HashMap<String, u32> = HashMap::new();
    for sub in ["JPG", "RAW"] {
        let Ok(rd) = fs::read_dir(folder.join(sub)) else { continue };
        for e in rd.flatten() {
            let stem = util::stem(Path::new(&e.file_name()));
            if let Some((day, seq)) = parse_seq(&stem) {
                let h = highest.entry(day).or_insert(0);
                *h = (*h).max(seq);
            }
        }
    }
    highest
}

fn highest_sequence(root: &Path, d: NaiveDate, cache: &mut HashMap<PathBuf, HashMap<String, u32>>) -> u32 {
    let key = d.format("%Y%m%d").to_string();
    let mut best = 0;
    for folder in [day_dir(root, d), month_dir(root, d)] {
        let seqs = cache.entry(folder.clone()).or_insert_with(|| existing_sequences(&folder));
        best = best.max(seqs.get(&key).copied().unwrap_or(0));
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Skip,
    Overwrite,
    Rename,
}

/// `dest` with `_1`, `_2`… appended to the stem, first one that is free.
pub fn next_free(dest: &Path) -> PathBuf {
    let (stem, suffix) = (util::stem(dest), util::suffix(dest));
    let parent = dest.parent().unwrap_or(Path::new(""));
    (1..)
        .map(|n| parent.join(format!("{stem}_{n}{suffix}")))
        .find(|c| !fsutil::lexists(c))
        .expect("a free name exists")
}

/// One file operation within a plan.  Carries a snapshot of everything the
/// executor needs, so it can run on a worker thread.
#[derive(Debug, Clone)]
pub struct PlannedOp {
    pub id: RecordId,
    pub src: PathBuf,
    pub filename: String,
    pub file_type: FileType,
    pub is_pruned: bool,
    pub crop: Option<Crop>,
    pub dest: PathBuf,
    pub date: NaiveDate,
    pub conflict: bool,
    pub resolution: Resolution,
    pub final_dest: PathBuf,
    /// The source's size as the record knows it (for progress; not a stat).
    pub file_size: u64,
}

impl PlannedOp {
    pub fn is_cropped(&self) -> bool {
        self.crop.as_ref().is_some_and(|c| !c.is_full())
    }

    pub fn set_resolution(&mut self, r: Resolution) {
        self.resolution = r;
        self.final_dest = if r == Resolution::Rename { next_free(&self.dest) } else { self.dest.clone() };
    }

    /// Replace an existing file only when the user chose to for a known
    /// conflict.  Everything else is written no-clobber.
    fn may_overwrite(&self) -> bool {
        self.conflict && self.resolution == Resolution::Overwrite
    }
}

/// One year / month / day / type bucket of the preview.
#[derive(Debug, Clone)]
pub struct OpGroup {
    pub year: i32,
    pub month: u32,
    pub month_key: String,
    pub day: u32,
    pub day_key: String,
    pub type_label: &'static str,
    pub file_count: usize,
    pub conflict_count: usize,
    pub pruned_count: usize,
    pub cropped_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

impl Mode {
    pub fn verb(self) -> &'static str {
        match self {
            Mode::Copy => "copy",
            Mode::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LibraryPlan {
    pub ops: Vec<PlannedOp>,
}

impl LibraryPlan {
    /// Plan `ids` (already filtered by the caller) into `root`.
    pub fn new(ids: &[RecordId], coll: &Collection, root: &Path, rename: bool) -> Self {
        let included: HashSet<RecordId> = ids.iter().copied().collect();
        let mut seen: HashSet<RecordId> = HashSet::new();
        let mut shots: Vec<(NaiveDateTime, Vec<RecordId>)> = Vec::new();
        for &id in ids {
            if seen.contains(&id) {
                continue;
            }
            let Some(record) = coll.get(id) else { continue };
            let pair_id = coll.find_pair(id);
            let pair = pair_id.and_then(|p| coll.get(p));
            let mut members = vec![id];
            if let Some(p) = pair_id {
                if included.contains(&p) && !seen.contains(&p) {
                    members.push(p);
                }
            }
            seen.extend(members.iter().copied());
            shots.push((best_datetime(record, pair), members));
        }

        let mut names: HashMap<RecordId, String> = HashMap::new();
        if rename {
            shots.sort_by(|a, b| {
                let min_name = |s: &Vec<RecordId>| {
                    s.iter().filter_map(|&m| coll.get(m)).map(|r| r.name_lower.clone()).min().unwrap_or_default()
                };
                a.0.cmp(&b.0).then_with(|| min_name(&a.1).cmp(&min_name(&b.1)))
            });
            let mut counters: HashMap<String, u32> = HashMap::new();
            let mut existing: HashMap<PathBuf, HashMap<String, u32>> = HashMap::new();
            for (dt, members) in &shots {
                let day = dt.format("%Y%m%d").to_string();
                let counter = counters
                    .entry(day)
                    .or_insert_with(|| highest_sequence(root, dt.date(), &mut existing));
                *counter += 1;
                for &m in members {
                    if let Some(r) = coll.get(m) {
                        names.insert(m, library_name(*dt, *counter, &util::suffix(&r.path)));
                    }
                }
            }
        }

        let mut ops = Vec::new();
        for (dt, members) in &shots {
            let d = dt.date();
            for &m in members {
                let Some(r) = coll.get(m) else { continue };
                let name = names.get(&m).cloned().unwrap_or_else(|| r.filename());
                let dest = day_dir(root, d).join(r.file_type.label()).join(name);
                let conflict = dest.exists();
                let mut op = PlannedOp {
                    id: m,
                    src: r.path.clone(),
                    filename: r.filename(),
                    file_type: r.file_type,
                    is_pruned: r.is_pruned,
                    crop: r.crop.clone(),
                    final_dest: dest.clone(),
                    dest,
                    date: d,
                    conflict,
                    resolution: Resolution::Skip,
                    file_size: r.file_size,
                };
                // Clean ops are ready to go; conflicts wait for the user.
                if !conflict {
                    op.set_resolution(Resolution::Overwrite);
                }
                ops.push(op);
            }
        }
        Self { ops }
    }

    pub fn total(&self) -> usize {
        self.ops.len()
    }

    pub fn conflict_count(&self) -> usize {
        self.ops.iter().filter(|o| o.conflict).count()
    }

    /// The ops that will run (not skipped).
    fn active(&self) -> impl Iterator<Item = &PlannedOp> {
        self.ops.iter().filter(|o| o.resolution != Resolution::Skip)
    }

    pub fn active_count(&self) -> usize {
        self.active().count()
    }

    /// The bytes of the files that will run, as the records know them.
    pub fn active_bytes(&self) -> u64 {
        self.active().map(|o| o.file_size).sum()
    }

    pub fn cropped_count(&self) -> usize {
        self.ops.iter().filter(|o| o.is_cropped()).count()
    }

    pub fn apply_bulk_resolution(&mut self, r: Resolution) {
        for op in self.ops.iter_mut().filter(|o| o.conflict) {
            op.set_resolution(r);
        }
    }

    /// Ops bucketed by (year, month, day, type), oldest first.
    pub fn grouped(&self) -> Vec<OpGroup> {
        let mut map: HashMap<(i32, u32, u32, &'static str), OpGroup> = HashMap::new();
        for op in &self.ops {
            let d = op.date;
            let key = (d.year(), d.month(), d.day(), op.file_type.label());
            let g = map.entry(key).or_insert_with(|| OpGroup {
                year: d.year(),
                month: d.month(),
                month_key: month_folder(d),
                day: d.day(),
                day_key: day_folder(d),
                type_label: op.file_type.label(),
                file_count: 0,
                conflict_count: 0,
                pruned_count: 0,
                cropped_count: 0,
            });
            g.file_count += 1;
            g.conflict_count += op.conflict as usize;
            g.pruned_count += op.is_pruned as usize;
            g.cropped_count += op.is_cropped() as usize;
        }
        let mut groups: Vec<OpGroup> = map.into_values().collect();
        groups.sort_by(|a, b| {
            (a.year, a.month, a.day, a.type_label).cmp(&(b.year, b.month, b.day, b.type_label))
        });
        groups
    }

    /// Execute every non-skipped op.
    ///
    /// `source_root` bounds the empty-folder cleanup after a move; without it
    /// no folders are removed.  `progress` is told after each file, with
    /// the bytes that went over (none for a same-device move).
    pub fn execute(
        &self,
        mode: Mode,
        source_root: Option<&Path>,
        cancel: &AtomicBool,
        progress: &dyn Fn(Progress),
    ) -> ExportReport {
        let active: Vec<&PlannedOp> = self.active().collect();
        let mut p = Progress { files_total: active.len(), bytes_total: active.iter().map(|o| o.file_size).sum(), ..Progress::default() };
        let mut report = ExportReport { mode, ..Default::default() };

        for (n, op) in active.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            match execute_one(op, mode) {
                Ok(moved) => {
                    report.succeeded.push(OpOutcome::of(op));
                    if moved == fsutil::Moved::Copied {
                        p.transferred += op.file_size;
                    }
                }
                Err(e) => report.failed.push((OpOutcome::of(op), e)),
            }
            p.files = n + 1;
            p.bytes += op.file_size;
            p.next = active.get(n + 1).map_or(0, |o| o.file_size);
            progress(p);
        }

        if mode == Mode::Move && !report.succeeded.is_empty() {
            if let Some(bound) = source_root {
                remove_empty_dirs(report.succeeded.iter().map(|o| o.src.as_path()), bound);
            }
        }
        report
    }
}

/// Runs one op; says whether its bytes were copied or only renamed.
fn execute_one(op: &PlannedOp, mode: Mode) -> Result<fsutil::Moved, String> {
    let dest = &op.final_dest;
    let overwrite = op.may_overwrite();
    if !overwrite && fsutil::lexists(dest) {
        return Err(format!("destination appeared after plan: {}", util::file_name(dest)));
    }
    fsutil::ensure_parent(dest).map_err(|e| e.to_string())?;
    let meta = fs::metadata(&op.src).map_err(|_| format!("source file no longer exists: {}", op.filename))?;
    let appeared = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("destination appeared after plan: {}", util::file_name(dest))
        } else {
            e.to_string()
        }
    };

    if op.is_cropped() {
        let crop = op.crop.as_ref().expect("cropped");
        match op.file_type {
            // A cropped JPG is a different file from its source.
            FileType::Jpg => render_cropped_jpeg(&op.src, dest, crop, overwrite)?,
            // RAW: copy the bits untouched, describe the crop beside them.
            FileType::Raw => {
                fsutil::safe_copy(&op.src, dest, &meta, overwrite).map_err(appeared)?;
                write_xmp_crop(dest, crop)?;
            }
        }
        fsutil::copy_times(dest, &meta);
        if mode == Mode::Move {
            // The new file is already fsynced; now drop the original.
            fs::remove_file(&op.src).map_err(|e| format!("exported, but the original could not be removed: {e}"))?;
            fsutil::sync_parent(&op.src);
        }
        return Ok(fsutil::Moved::Copied);
    }

    let moved = match mode {
        Mode::Copy => {
            fsutil::safe_copy(&op.src, dest, &meta, overwrite).map_err(appeared)?;
            fsutil::Moved::Copied
        }
        Mode::Move => fsutil::safe_move(&op.src, dest, &meta, overwrite).map_err(appeared)?,
    };
    fsutil::copy_times(dest, &meta);
    // A deliberate overwrite of a RAW must not inherit a crop the earlier
    // export left in its sidecar.
    if op.file_type == FileType::Raw && overwrite {
        remove_xmp_crop(dest);
    }
    Ok(moved)
}

/// Remove source folders emptied by a move, bottom-up, never at or above
/// `bound` and never outside it.
fn remove_empty_dirs<'a>(sources: impl Iterator<Item = &'a Path>, bound: &Path) {
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for src in sources {
        let mut p = src.parent();
        while let Some(d) = p {
            if d == bound || !d.starts_with(bound) {
                break;
            }
            dirs.insert(d.to_path_buf());
            p = d.parent();
        }
    }
    let mut ordered: Vec<PathBuf> = dirs.into_iter().collect();
    ordered.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in ordered {
        let _ = fs::remove_dir(&d); // only succeeds when empty
    }
}

#[derive(Debug, Clone)]
pub struct OpOutcome {
    pub id: RecordId,
    pub filename: String,
    pub src: PathBuf,
    pub dest: PathBuf,
}

impl OpOutcome {
    fn of(op: &PlannedOp) -> Self {
        Self { id: op.id, filename: op.filename.clone(), src: op.src.clone(), dest: op.final_dest.clone() }
    }
}

#[derive(Debug, Clone)]
pub struct ExportReport {
    pub mode: Mode,
    pub succeeded: Vec<OpOutcome>,
    pub failed: Vec<(OpOutcome, String)>,
}

impl Default for ExportReport {
    fn default() -> Self {
        Self { mode: Mode::Copy, succeeded: Vec::new(), failed: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-lib-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn rec(path: &Path, ct: &str) -> PhotoRecord {
        fs::write(path, b"data").unwrap();
        let ft = FileType::from_path(path).unwrap();
        let t = NaiveDateTime::parse_from_str(ct, "%Y-%m-%d %H:%M:%S").unwrap();
        PhotoRecord::new(path.to_path_buf(), ft, 4, MTime::from_unix(1_700_000_000, 0), Some(t))
    }

    #[test]
    fn names_and_folders() {
        let d = NaiveDate::from_ymd_opt(2025, 3, 2).unwrap();
        assert_eq!(month_folder(d), "Mar-2025");
        assert_eq!(day_folder(d), "02-Mar-2025");
        let dt = d.and_hms_opt(14, 30, 0).unwrap();
        assert_eq!(library_name(dt, 7, ".CR3"), "20250302_1430_0007.CR3");
        assert_eq!(parse_seq("20250302_1430_0007"), Some(("20250302".into(), 7)));
        assert_eq!(parse_seq("20250302_1430_0007_1"), None);
    }

    #[test]
    fn rename_shares_numbers_and_continues_sequence() {
        let src = tmp("src");
        let lib = tmp("lib");
        let mut c = Collection::new();
        let a_raw = c.add(rec(&src.join("A.ARW"), "2025-03-02 10:00:00"));
        let a_jpg = c.add(rec(&src.join("A.JPG"), "2025-03-02 10:00:00"));
        let b = c.add(rec(&src.join("B.JPG"), "2025-03-02 09:00:00"));
        c.build_pairs();
        // An earlier export already used sequence 4 that day.
        let d = NaiveDate::from_ymd_opt(2025, 3, 2).unwrap();
        fs::create_dir_all(day_dir(&lib, d).join("JPG")).unwrap();
        fs::write(day_dir(&lib, d).join("JPG/20250302_0800_0004.JPG"), b"x").unwrap();

        let plan = LibraryPlan::new(&[a_raw, a_jpg, b], &c, &lib, true);
        let name = |id| util::file_name(&plan.ops.iter().find(|o| o.id == id).unwrap().dest);
        assert_eq!(name(b), "20250302_0900_0005.JPG");
        assert_eq!(name(a_raw), "20250302_1000_0006.ARW");
        assert_eq!(name(a_jpg), "20250302_1000_0006.JPG");

        let report = plan.execute(Mode::Move, Some(&src), &AtomicBool::new(false), &|_| {});
        assert_eq!(report.succeeded.len(), 3, "{:?}", report.failed);
        assert!(day_dir(&lib, d).join("RAW/20250302_1000_0006.ARW").exists());
        assert!(!src.join("A.ARW").exists());
        assert!(src.exists(), "source root itself is kept");
    }

    #[test]
    fn appeared_destination_is_not_overwritten() {
        let src = tmp("src2");
        let lib = tmp("lib2");
        let mut c = Collection::new();
        let a = c.add(rec(&src.join("A.JPG"), "2025-03-02 10:00:00"));
        let plan = LibraryPlan::new(&[a], &c, &lib, false);
        let dest = plan.ops[0].final_dest.clone();
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, b"someone else's").unwrap();
        let report = plan.execute(Mode::Copy, None, &AtomicBool::new(false), &|_| {});
        assert_eq!(report.failed.len(), 1);
        assert_eq!(fs::read(&dest).unwrap(), b"someone else's");
    }

    #[test]
    fn progress_carries_the_bytes_of_each_finished_file() {
        let src = tmp("src3");
        let lib = tmp("lib3");
        let mut c = Collection::new();
        let a = c.add(rec(&src.join("A.ARW"), "2025-03-02 10:00:00"));
        let b = c.add(rec(&src.join("B.JPG"), "2025-03-02 10:01:00"));
        let plan = LibraryPlan::new(&[a, b], &c, &lib, false);
        assert_eq!(plan.active_count(), 2);
        assert_eq!(plan.active_bytes(), 8, "the records' sizes, not a stat");

        let seen = std::sync::Mutex::new(Vec::new());
        let report = plan.execute(Mode::Copy, None, &AtomicBool::new(false), &|p| seen.lock().unwrap().push(p));
        assert_eq!(report.succeeded.len(), 2, "{:?}", report.failed);
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], Progress { files: 1, files_total: 2, bytes: 4, bytes_total: 8, transferred: 4, next: 4 });
        assert_eq!(seen[1], Progress { files: 2, files_total: 2, bytes: 8, bytes_total: 8, transferred: 8, next: 0 });
    }

    #[test]
    fn a_move_on_one_device_transfers_no_bytes() {
        let src = tmp("src4");
        let lib = tmp("lib4");
        let mut c = Collection::new();
        let a = c.add(rec(&src.join("A.JPG"), "2025-03-02 10:00:00"));
        let plan = LibraryPlan::new(&[a], &c, &lib, false);
        let seen = std::sync::Mutex::new(Vec::new());
        let report = plan.execute(Mode::Move, Some(&src), &AtomicBool::new(false), &|p| seen.lock().unwrap().push(p));
        assert_eq!(report.succeeded.len(), 1, "{:?}", report.failed);
        let seen = seen.into_inner().unwrap();
        // The file counts as done, but a rename is instant: no speed can be
        // read from it.
        assert_eq!(seen, vec![Progress { files: 1, files_total: 1, bytes: 4, bytes_total: 4, transferred: 0, next: 0 }]);
    }
}
