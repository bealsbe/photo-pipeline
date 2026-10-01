//! Copy the RAW of every marked shot into one flat folder to edit from.
//!
//! Not an archive step: no date tree, no renaming, no JPGs, originals never
//! touched.
//!
//! * A marked RAW is taken as is.
//! * A marked JPG contributes its RAW — the confirmed pair, or else a RAW
//!   with the same stem in the same canonical folder.
//! * A marked JPG with no RAW is reported, not copied.
//! * Pruned shots are left out (copying something meant for deletion is the
//!   worse mistake).
//!
//! The plan never overwrites: an identical file already present (same size,
//! mtime within 2 s for exFAT) is skipped, and a name taken by a different
//! file — or by an earlier file in the same batch — gets `_1`, `_2`….

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::NaiveDateTime;

use super::crop_export::write_xmp_crop;
use super::fsutil;
use crate::model::collection::{Collection, RecordId};
use crate::model::crop::Crop;
use crate::model::record::FileType;
use crate::util::{self, canonical_parent};

const MTIME_SLACK: f64 = 2.0;

#[derive(Debug, Clone, Default)]
pub struct MarkedRaws {
    pub raws: Vec<RecordId>,
    /// RAWs found by name for a marked, un-paired JPG.
    pub matched_by_name: usize,
    /// Marked JPGs with no RAW at all.
    pub jpg_only: Vec<RecordId>,
    /// Marked shots skipped because they are pruned.
    pub pruned: usize,
}

pub fn select_marked_raws(coll: &Collection) -> MarkedRaws {
    let mut out = MarkedRaws::default();
    let mut seen: HashSet<RecordId> = HashSet::new();
    let mut pruned_shots: HashSet<(PathBuf, String)> = HashSet::new();

    for (id, r) in coll.iter_ids() {
        if !r.is_marked {
            continue;
        }
        if r.file_type == FileType::Raw {
            if r.is_pruned {
                pruned_shots.insert((canonical_parent(&r.path), r.stem_lower()));
                continue;
            }
            if seen.insert(id) {
                out.raws.push(id);
            }
            continue;
        }
        // A marked JPG — find the RAW of the same shot.
        let mut raw = coll.find_pair(id).filter(|&p| coll.get(p).is_some_and(|x| x.file_type == FileType::Raw));
        let mut by_name = false;
        if raw.is_none() {
            raw = coll
                .stem_siblings(id)
                .into_iter()
                .find(|&s| coll.get(s).is_some_and(|x| x.file_type == FileType::Raw));
            by_name = raw.is_some();
        }
        let Some(raw_id) = raw else {
            if r.is_pruned {
                pruned_shots.insert((canonical_parent(&r.path), r.stem_lower()));
            } else {
                out.jpg_only.push(id);
            }
            continue;
        };
        let raw_rec = coll.get(raw_id).expect("live");
        if r.is_pruned || raw_rec.is_pruned {
            pruned_shots.insert((canonical_parent(&raw_rec.path), raw_rec.stem_lower()));
            continue;
        }
        if by_name && !seen.contains(&raw_id) {
            out.matched_by_name += 1;
        }
        if seen.insert(raw_id) {
            out.raws.push(raw_id);
        }
    }
    out.pruned = pruned_shots.len();
    out
}

#[derive(Debug, Clone)]
pub struct CopyOp {
    pub id: RecordId,
    pub src: PathBuf,
    pub filename: String,
    pub file_size: u64,
    pub mtime: f64,
    pub shot_time: NaiveDateTime,
    pub crop: Option<Crop>,
    pub dest: PathBuf,
    /// An identical copy is already there — nothing to do.
    pub exists: bool,
}

impl CopyOp {
    pub fn renamed(&self) -> bool {
        util::file_name(&self.dest) != self.filename
    }

    pub fn is_cropped(&self) -> bool {
        self.crop.as_ref().is_some_and(|c| !c.is_full())
    }
}

fn same_file(size: u64, mtime: f64, target: &Path) -> bool {
    let Ok(meta) = fs::metadata(target) else { return false };
    let (s, ns) = crate::model::record::unix_mtime(&meta);
    let t = s as f64 + ns as f64 * 1e-9;
    meta.len() == size && (t - mtime).abs() <= MTIME_SLACK
}

#[derive(Debug, Clone)]
pub struct EditCopyPlan {
    pub ops: Vec<CopyOp>,
}

impl EditCopyPlan {
    /// Building touches the disk read-only.
    pub fn new(raws: &[RecordId], coll: &Collection, dest_dir: &Path) -> Self {
        let mut records: Vec<_> = raws.iter().filter_map(|&id| coll.get(id).map(|r| (id, r))).collect();
        // Capture order, so `_1` suffixes go to the later shot.
        records.sort_by(|a, b| a.1.shot_time().cmp(&b.1.shot_time()).then_with(|| a.1.name_lower.cmp(&b.1.name_lower)));
        let mut claimed: HashSet<String> = HashSet::new(); // lower-cased: exFAT/NTFS
        let mut ops = Vec::with_capacity(records.len());
        for (id, r) in records {
            let filename = r.filename();
            let target = dest_dir.join(&filename);
            let (stem, suffix) = (util::stem(&target), util::suffix(&target));
            let mut dest = target.clone();
            let mut n = 0;
            let mut exists = false;
            loop {
                let key = util::file_name(&dest).to_lowercase();
                if !claimed.contains(&key) {
                    if !fsutil::lexists(&dest) {
                        break;
                    }
                    if same_file(r.file_size, r.modified.timestamp, &dest) {
                        exists = true;
                        break;
                    }
                }
                n += 1;
                dest = dest_dir.join(format!("{stem}_{n}{suffix}"));
            }
            claimed.insert(util::file_name(&dest).to_lowercase());
            ops.push(CopyOp {
                id,
                src: r.path.clone(),
                filename,
                file_size: r.file_size,
                mtime: r.modified.timestamp,
                shot_time: r.shot_time(),
                crop: r.crop.clone(),
                dest,
                exists,
            });
        }
        Self { ops }
    }

    pub fn to_copy(&self) -> impl Iterator<Item = &CopyOp> {
        self.ops.iter().filter(|o| !o.exists)
    }

    pub fn to_copy_count(&self) -> usize {
        self.to_copy().count()
    }

    pub fn existing_count(&self) -> usize {
        self.ops.iter().filter(|o| o.exists).count()
    }

    pub fn renamed_count(&self) -> usize {
        self.to_copy().filter(|o| o.renamed()).count()
    }

    pub fn cropped_count(&self) -> usize {
        self.to_copy().filter(|o| o.is_cropped()).count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.to_copy().map(|o| o.file_size).sum()
    }

    /// Copy every op that isn't already there.  A cancel stops between
    /// files; whatever was copied stays.
    pub fn execute(&self, cancel: &AtomicBool, progress: &dyn Fn(usize, usize, u64, u64)) -> CopyReport {
        let todo: Vec<&CopyOp> = self.to_copy().collect();
        let total_bytes: u64 = todo.iter().map(|o| o.file_size).sum();
        let mut done_bytes = 0;
        let mut report = CopyReport::default();
        for (n, op) in todo.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                return report;
            }
            match copy_one(op) {
                Ok(()) => report.copied.push((*op).clone()),
                Err(e) => report.failed.push(((*op).clone(), e)),
            }
            done_bytes += op.file_size;
            progress(n + 1, todo.len(), done_bytes, total_bytes);
        }
        report
    }
}

fn copy_one(op: &CopyOp) -> Result<(), String> {
    let meta = fs::metadata(&op.src).map_err(|_| format!("source file no longer exists: {}", op.filename))?;
    // Re-check: something may have appeared since the plan was built.  This
    // copy never overwrites.
    if fsutil::lexists(&op.dest) {
        return Err(format!("{} appeared in the folder since the preview", util::file_name(&op.dest)));
    }
    fsutil::ensure_parent(&op.dest).map_err(|e| e.to_string())?;
    fsutil::safe_copy(&op.src, &op.dest, &meta, false).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} appeared in the folder since the preview", util::file_name(&op.dest))
        } else {
            e.to_string()
        }
    })?;
    fsutil::copy_times(&op.dest, &meta);
    if let Some(crop) = op.crop.as_ref().filter(|c| !c.is_full()) {
        write_xmp_crop(&op.dest, crop)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct CopyReport {
    pub copied: Vec<CopyOp>,
    pub failed: Vec<(CopyOp, String)>,
    pub cancelled: bool,
}
