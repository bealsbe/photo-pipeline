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

pub struct SeparationPlan {
    pub ops: Vec<MoveOp>,
    pub strategy: Strategy,
}

impl SeparationPlan {
    pub fn new(coll: &Collection) -> Self {
        let mut ops = Vec::with_capacity(coll.len());
        for (id, r) in coll.iter_ids() {
            let sub = r.file_type.label();
            let filename = r.filename();
            if util::parent_name(&r.path).to_uppercase() == sub {
                ops.push(MoveOp {
                    id,
                    src: r.path.clone(),
                    file_type: r.file_type,
                    filename,
                    dest: r.path.clone(),
                    final_dest: r.path.clone(),
                    conflict: false,
                    skipped: false,
                });
                continue;
            }
            let dest = r.path.parent().unwrap_or(Path::new("")).join(sub).join(&filename);
            let conflict = fsutil::lexists(&dest);
            let final_dest = if conflict { free_path(&dest) } else { dest.clone() };
            ops.push(MoveOp {
                id,
                src: r.path.clone(),
                file_type: r.file_type,
                filename,
                dest,
                final_dest,
                conflict,
                skipped: false,
            });
        }
        Self { ops, strategy: Strategy::Rename }
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
                    op.final_dest = free_path(&op.dest);
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
