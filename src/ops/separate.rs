//! Sort: move each file into a `RAW/` or `JPG/` subfolder of its own parent.
//!
//! Files already inside the right subfolder are left alone.  A conflict is a
//! destination that already exists; the batch strategy either skips those
//! files or renames them `_1`, `_2`….

use std::fs;
use std::path::{Path, PathBuf};

use super::fsutil;
use crate::model::collection::{Collection, RecordId};
use crate::model::record::FileType;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Skip,
    Rename,
}

#[derive(Debug, Clone)]
pub struct MoveOp {
    pub id: RecordId,
    pub src: PathBuf,
    pub file_type: FileType,
    pub filename: String,
    /// Desired destination (may conflict).
    pub dest: PathBuf,
    /// Destination after the strategy is applied.
    pub final_dest: PathBuf,
    pub conflict: bool,
    pub skipped: bool,
    /// For a conflict: the first free `_1`, `_2`… name, found with the
    /// conflict so choosing Rename again needs no disk.
    pub renamed: PathBuf,
}

impl MoveOp {
    pub fn in_place(&self) -> bool {
        self.src == self.dest
    }
}

/// First name `stem_1.ext`, `stem_2.ext`… that does not exist.
pub fn free_path(dest: &Path) -> PathBuf {
    if !fsutil::lexists(dest) {
        return dest.to_path_buf();
    }
    let (stem, suffix) = (util::stem(dest), util::suffix(dest));
    let parent = dest.parent().unwrap_or(Path::new(""));
    (1..)
        .map(|n| parent.join(format!("{stem}_{n}{suffix}")))
        .find(|c| !fsutil::lexists(c))
        .expect("some suffix is free")
}

#[derive(Debug, Clone)]
pub struct SeparationPlan {
    pub ops: Vec<MoveOp>,
    pub strategy: Strategy,
}

impl SeparationPlan {
    /// The plan, conflicts checked on disk (see [`SeparationPlan::prepare`]
    /// and [`SeparationPlan::check_disk`]).
    pub fn new(coll: &Collection) -> Self {
        Self::prepare(coll).check_disk()
    }

    /// Where every file goes, from the collection in memory; conflicts are
    /// not known yet ([`SeparationPlan::check_disk`] finds them).
    pub fn prepare(coll: &Collection) -> Self {
        let mut ops = Vec::with_capacity(coll.len());
        for (id, r) in coll.iter_ids() {
            let sub = r.file_type.label();
            let filename = r.filename();
            let dest = if util::parent_name(&r.path).to_uppercase() == sub {
                r.path.clone()
            } else {
                r.path.parent().unwrap_or(Path::new("")).join(sub).join(&filename)
            };
            ops.push(MoveOp {
                id,
                src: r.path.clone(),
                file_type: r.file_type,
                filename,
                final_dest: dest.clone(),
                renamed: dest.clone(),
                dest,
                conflict: false,
                skipped: false,
            });
        }
        Self { ops, strategy: Strategy::Rename }
    }

    /// Find which destinations are taken, and a free name for each: a stat
    /// or more a file, so a worker's job.
    pub fn check_disk(mut self) -> Self {
        for op in self.ops.iter_mut().filter(|o| !o.in_place()) {
            op.conflict = fsutil::lexists(&op.dest);
            if op.conflict {
                op.renamed = free_path(&op.dest);
            }
        }
        let strategy = self.strategy;
        self.set_strategy(strategy);
        self
    }

    pub fn conflict_count(&self) -> usize {
        self.ops.iter().filter(|o| o.conflict).count()
    }

    pub fn already_in_place_count(&self) -> usize {
        self.ops.iter().filter(|o| o.in_place()).count()
    }

    /// Files that will actually move.
    pub fn move_count(&self) -> usize {
        self.ops.iter().filter(|o| !o.in_place() && !o.skipped).count()
    }

    pub fn set_strategy(&mut self, strategy: Strategy) {
        self.strategy = strategy;
        for op in self.ops.iter_mut().filter(|o| o.conflict) {
            match strategy {
                Strategy::Skip => {
                    op.skipped = true;
                    op.final_dest = op.src.clone();
                }
                Strategy::Rename => {
                    op.skipped = false;
                    op.final_dest = op.renamed.clone();
                }
            }
        }
    }

    /// Perform the moves.  Returns (moved or already in place: (id, new
    /// path)) and (failed: (id, filename, message)).
    ///
    /// Destinations are claimed with a no-clobber rename, so a file that
    /// appeared after the preview is reported rather than overwritten.
    pub fn execute(&self) -> (Vec<(RecordId, PathBuf)>, Vec<(RecordId, String, String)>) {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for op in &self.ops {
            if op.in_place() {
                ok.push((op.id, op.src.clone()));
                continue;
            }
            if op.skipped {
                continue;
            }
            let meta = match fs::metadata(&op.src) {
                Ok(m) => m,
                Err(_) => {
                    failed.push((op.id, op.filename.clone(), format!("source file no longer exists: {}", op.filename)));
                    continue;
                }
            };
            let result = fsutil::ensure_parent(&op.final_dest)
                .and_then(|_| fsutil::safe_move(&op.src, &op.final_dest, &meta, false));
            match result {
                Ok(_) => ok.push((op.id, op.final_dest.clone())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => failed.push((
                    op.id,
                    op.filename.clone(),
                    format!("{} appeared at the destination since the preview", util::file_name(&op.final_dest)),
                )),
                Err(e) => failed.push((op.id, op.filename.clone(), e.to_string())),
            }
        }
        (ok, failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::{MTime, PhotoRecord};

    #[test]
    fn conflicts_are_found_by_check_disk_and_a_strategy_needs_no_disk() {
        let d = std::env::temp_dir().join(format!("pp-separate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("JPG")).unwrap();
        fs::write(d.join("JPG/IMG_1.JPG"), b"taken").unwrap();
        let mut c = Collection::new();
        for name in ["IMG_1.JPG", "IMG_2.JPG", "JPG/IMG_3.JPG"] {
            let p = d.join(name);
            c.add(PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), 1, MTime::from_unix(1_700_000_000, 0), None));
        }
        // In memory: where each goes, no conflicts known yet.
        let prepared = SeparationPlan::prepare(&c);
        assert_eq!((prepared.conflict_count(), prepared.already_in_place_count(), prepared.move_count()), (0, 1, 2));
        // On disk (a worker): the taken name, and a free one for it.
        let mut plan = prepared.check_disk();
        assert_eq!(plan.conflict_count(), 1);
        let op = plan.ops.iter().find(|o| o.conflict).unwrap();
        assert_eq!(op.final_dest, d.join("JPG/IMG_1_1.JPG"));
        assert_eq!(format!("{:?}", plan.ops), format!("{:?}", SeparationPlan::new(&c).ops));
        // Toggling the strategy reads nothing: the folder can be gone.
        fs::remove_dir_all(&d).unwrap();
        plan.set_strategy(Strategy::Skip);
        assert_eq!(plan.move_count(), 1);
        plan.set_strategy(Strategy::Rename);
        assert_eq!(plan.ops.iter().find(|o| o.conflict).unwrap().final_dest, d.join("JPG/IMG_1_1.JPG"));
    }
}
