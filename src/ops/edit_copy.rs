//! Copy the RAW, the JPG, or both, of every marked shot into one flat folder
//! to edit from.
//!
//! Not an archive step: no date tree, no renaming, originals never touched.
//!
//! * A marked file of a wanted kind is taken as is.
//! * A marked file of the other kind contributes its partner of the wanted
//!   kind — the confirmed pair, or else a file with the same stem in the same
//!   canonical folder.
//! * A marked shot with no file of a wanted kind is reported, not copied.
//! * Pruned shots are left out (copying something meant for deletion is the
//!   worse mistake).
//!
//! The plan never overwrites: an identical file already present (same size,
//! mtime within 2 s for exFAT) is skipped, and a name taken by a different
//! file — or by an earlier file in the same batch — gets `_1`, `_2`….
//!
//! Each RAW placed can get a RawTherapee profile beside it, named after its
//! final name (`IMG_1_1.ARW.pp3` after a rename), and so can a RAW already
//! there that has none; one already there is kept (see `rt_profile`).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::NaiveDateTime;

use super::crop_export::write_xmp_crop;
use super::rt_profile::{self, ProfileChoice};
use super::{Progress, fsutil};
use crate::model::collection::{Collection, RecordId};
use crate::model::crop::Crop;
use crate::model::record::FileType;
use crate::util::{self, canonical_parent};

const MTIME_SLACK: f64 = 2.0;

#[derive(Debug, Clone, Default)]
pub struct MarkedFiles {
    /// The files to copy: RAWs, JPGs or both, each once.
    pub files: Vec<RecordId>,
    /// Files found by name for a marked file of the other kind.
    pub matched_by_name: usize,
    /// Marked files whose shot has nothing of a wanted kind.
    pub missing: Vec<RecordId>,
    /// Marked shots skipped because they are pruned.
    pub pruned: usize,
}

/// The file of kind `ft` that belongs to the shot of `id`, and whether it was
/// found by name rather than by a confirmed pair.
fn partner(coll: &Collection, id: RecordId, ft: FileType) -> Option<(RecordId, bool)> {
    let of_kind = |p: &RecordId| coll.get(*p).is_some_and(|x| x.file_type == ft);
    if let Some(p) = coll.find_pair(id).filter(of_kind) {
        return Some((p, false));
    }
    coll.stem_siblings(id).into_iter().find(of_kind).map(|p| (p, true))
}

pub fn select_marked(coll: &Collection, want_raws: bool, want_jpgs: bool) -> MarkedFiles {
    let mut out = MarkedFiles::default();
    let mut seen: HashSet<RecordId> = HashSet::new();
    let mut pruned_shots: HashSet<(PathBuf, String)> = HashSet::new();

    for (id, r) in coll.iter_ids() {
        if !r.is_marked {
            continue;
        }
        let mut take: Vec<(RecordId, bool)> = Vec::new();
        for (ft, want) in [(FileType::Raw, want_raws), (FileType::Jpg, want_jpgs)] {
            if !want {
                continue;
            }
            if r.file_type == ft {
                take.push((id, false));
            } else if let Some(found) = partner(coll, id, ft) {
                take.push(found);
            }
        }
        if r.is_pruned || take.iter().any(|&(p, _)| coll.get(p).is_some_and(|x| x.is_pruned)) {
            pruned_shots.insert((canonical_parent(&r.path), r.stem_lower()));
            continue;
        }
        if take.is_empty() {
            out.missing.push(id);
        }
        for (p, by_name) in take {
            if seen.insert(p) {
                out.files.push(p);
                out.matched_by_name += by_name as usize;
            }
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
    /// A RAW: the one kind that gets a crop sidecar and a profile.
    pub raw: bool,
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

/// What planning needs of each file, read from the collection in memory
/// (no disk), in capture order so `_1` suffixes go to the later shot.  The
/// ops' `dest` is only the file name until [`EditCopyPlan::build`] places
/// them.
pub fn plan_inputs(files: &[RecordId], coll: &Collection) -> Vec<CopyOp> {
    let mut records: Vec<_> = files.iter().filter_map(|&id| coll.get(id).map(|r| (id, r))).collect();
    records.sort_by(|a, b| a.1.shot_time().cmp(&b.1.shot_time()).then_with(|| a.1.name_lower.cmp(&b.1.name_lower)));
    records
        .into_iter()
        .map(|(id, r)| {
            let filename = r.filename();
            CopyOp {
                id,
                src: r.path.clone(),
                dest: PathBuf::from(&filename),
                filename,
                file_size: r.file_size,
                mtime: r.modified.timestamp,
                shot_time: r.shot_time(),
                // Only a RAW gets a crop sidecar; a JPG is copied as it is.
                crop: r.crop.clone().filter(|_| r.file_type == FileType::Raw),
                exists: false,
                raw: r.file_type == FileType::Raw,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct EditCopyPlan {
    pub ops: Vec<CopyOp>,
}

impl EditCopyPlan {
    /// Building touches the disk read-only: on a worker, not the UI thread
    /// (see [`plan_inputs`] and [`EditCopyPlan::build`]).
    pub fn new(files: &[RecordId], coll: &Collection, dest_dir: &Path) -> Self {
        Self::build(plan_inputs(files, coll), dest_dir)
    }

    /// Place `inputs` (from [`plan_inputs`]) in `dest_dir`: an identical
    /// file already there is skipped, a taken name gets `_1`, `_2`….  Reads
    /// the disk (a stat or two a file), so it runs on a worker.
    pub fn build(inputs: Vec<CopyOp>, dest_dir: &Path) -> Self {
        let mut claimed: HashSet<String> = HashSet::new(); // lower-cased: exFAT/NTFS
        let mut ops = Vec::with_capacity(inputs.len());
        for mut op in inputs {
            let target = dest_dir.join(&op.filename);
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
                    if same_file(op.file_size, op.mtime, &dest) {
                        exists = true;
                        break;
                    }
                }
                n += 1;
                dest = dest_dir.join(format!("{stem}_{n}{suffix}"));
            }
            claimed.insert(util::file_name(&dest).to_lowercase());
            op.dest = dest;
            op.exists = exists;
            ops.push(op);
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

    /// RAWs already in the folder: each still gets a profile if it has none.
    pub fn existing_raws(&self) -> impl Iterator<Item = &CopyOp> {
        self.ops.iter().filter(|o| o.exists && o.raw)
    }

    /// Any RAW in the plan, to copy or already there.
    pub fn has_raws(&self) -> bool {
        self.ops.iter().any(|o| o.raw)
    }

    /// Copy every op that isn't already there.  A cancel stops between
    /// files; whatever was copied stays.  `progress` is told after each
    /// file, with its bytes (every file here is a real copy; the profiles
    /// are a few KB and aren't counted).
    ///
    /// With a profile, it is read once, first: one that can't be used stops
    /// the run before anything is copied (`profiles.refused`).  Each RAW
    /// copied gets it beside it, then each RAW already there that has none.
    /// A profile that can't be written is reported and leaves its RAW be.
    pub fn execute(&self, profile: &ProfileChoice, cancel: &AtomicBool, progress: &dyn Fn(Progress)) -> CopyReport {
        let mut report = CopyReport::default();
        let profile = if self.has_raws() {
            match rt_profile::load(profile) {
                Ok(p) => p,
                Err(why) => {
                    report.profiles.refused = Some(why);
                    return report;
                }
            }
        } else {
            None
        };
        let todo: Vec<&CopyOp> = self.to_copy().collect();
        let mut p = Progress { files_total: todo.len(), bytes_total: todo.iter().map(|o| o.file_size).sum(), ..Progress::default() };
        for (n, op) in todo.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                return report;
            }
            match copy_one(op) {
                Ok(()) => {
                    report.copied.push((*op).clone());
                    p.transferred += op.file_size;
                    if let Some(prof) = profile.as_ref().filter(|_| op.raw) {
                        report.profiles.note(op, rt_profile::write_beside(&op.dest, prof, op.crop.as_ref()));
                    }
                }
                Err(e) => report.failed.push(((*op).clone(), e)),
            }
            p.files = n + 1;
            p.bytes += op.file_size;
            p.next = todo.get(n + 1).map_or(0, |o| o.file_size);
            progress(p);
        }
        if let Some(prof) = &profile {
            for op in self.existing_raws() {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    return report;
                }
                report.profiles.note(op, rt_profile::write_beside(&op.dest, prof, op.crop.as_ref()));
            }
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
    pub profiles: ProfileReport,
}

/// The RawTherapee profiles a run wrote beside its RAWs.
#[derive(Debug, Clone, Default)]
pub struct ProfileReport {
    /// The RAWs that got a new profile.
    pub written: Vec<PathBuf>,
    /// … of which this many carry the photo's crop.
    pub with_crop: usize,
    /// Profiles already beside a RAW, kept as they were.
    pub kept: usize,
    /// The profiles that couldn't be written: the `.pp3`'s name, and why.
    pub failed: Vec<(String, String)>,
    /// The chosen profile couldn't be used, so nothing was copied.
    pub refused: Option<String>,
}

impl ProfileReport {
    fn note(&mut self, op: &CopyOp, outcome: Result<rt_profile::Outcome, String>) {
        match outcome {
            Ok(rt_profile::Outcome::Written { crop }) => {
                self.written.push(op.dest.clone());
                self.with_crop += crop as usize;
            }
            Ok(rt_profile::Outcome::Kept) => self.kept += 1,
            Err(e) => self.failed.push((util::file_name(&rt_profile::sidecar_path(&op.dest)), e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::{MTime, PhotoRecord};

    fn add(c: &mut Collection, path: &str, marked: bool, pruned: bool) -> RecordId {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        let mut r = PhotoRecord::new(p, ft, 10, MTime::from_unix(1_700_000_000, 0), None);
        r.is_marked = marked;
        r.is_pruned = pruned;
        c.add(r)
    }

    /// Shot 1: a RAW+JPG pair.  Shot 2: a RAW alone.  Shot 3: a JPG alone.
    fn shoot(marked: [bool; 4]) -> (Collection, [RecordId; 4]) {
        let mut c = Collection::new();
        let ids = [
            add(&mut c, "/s/RAW/IMG_1.ARW", marked[0], false),
            add(&mut c, "/s/JPG/IMG_1.JPG", marked[1], false),
            add(&mut c, "/s/RAW/IMG_2.ARW", marked[2], false),
            add(&mut c, "/s/JPG/IMG_3.JPG", marked[3], false),
        ];
        c.build_pairs();
        (c, ids)
    }

    fn sorted(mut v: Vec<RecordId>) -> Vec<RecordId> {
        v.sort();
        v
    }

    #[test]
    fn raw_only_takes_the_raw_of_a_marked_raw_or_jpg_and_reports_a_lone_jpg() {
        let (c, [raw1, _, raw2, jpg3]) = shoot([false, true, true, true]);
        let sel = select_marked(&c, true, false);
        assert_eq!(sorted(sel.files), sorted(vec![raw1, raw2]));
        assert_eq!(sel.missing, vec![jpg3]);
    }

    #[test]
    fn jpg_only_takes_the_jpg_of_a_marked_raw() {
        let (c, [_, jpg1, ..]) = shoot([true, false, false, false]);
        let sel = select_marked(&c, false, true);
        assert_eq!(sel.files, vec![jpg1]);
        assert!(sel.missing.is_empty());
    }

    #[test]
    fn jpg_only_takes_a_marked_jpg_as_it_is() {
        let (c, [_, jpg1, _, jpg3]) = shoot([false, true, false, true]);
        let sel = select_marked(&c, false, true);
        assert_eq!(sorted(sel.files), sorted(vec![jpg1, jpg3]));
        assert!(sel.missing.is_empty());
    }

    #[test]
    fn both_takes_the_raw_and_the_jpg_of_each_marked_shot_once() {
        // Marking either half, or both, of shot 1 gives the same two files.
        for (a, b) in [(true, false), (false, true), (true, true)] {
            let (c, [raw1, jpg1, ..]) = shoot([a, b, false, false]);
            let sel = select_marked(&c, true, true);
            assert_eq!(sorted(sel.files), sorted(vec![raw1, jpg1]), "{a} {b}");
        }
    }

    #[test]
    fn both_copies_a_lone_file_and_reports_nothing_missing() {
        let (c, [_, _, raw2, jpg3]) = shoot([false, false, true, true]);
        let sel = select_marked(&c, true, true);
        assert_eq!(sorted(sel.files), sorted(vec![raw2, jpg3]));
        assert!(sel.missing.is_empty());
    }

    #[test]
    fn a_raw_with_no_jpg_is_reported_not_copied_when_only_jpgs_are_wanted() {
        let (c, [_, _, raw2, _]) = shoot([false, false, true, false]);
        let sel = select_marked(&c, false, true);
        assert!(sel.files.is_empty());
        assert_eq!(sel.missing, vec![raw2]);
    }

    #[test]
    fn pruned_shots_are_left_out_whichever_half_is_marked_or_pruned() {
        let mut c = Collection::new();
        add(&mut c, "/s/RAW/IMG_1.ARW", true, true);
        add(&mut c, "/s/JPG/IMG_1.JPG", false, false);
        add(&mut c, "/s/RAW/IMG_2.ARW", false, false);
        add(&mut c, "/s/JPG/IMG_2.JPG", true, true);
        c.build_pairs();
        for (raws, jpgs) in [(true, false), (false, true), (true, true)] {
            let sel = select_marked(&c, raws, jpgs);
            assert!(sel.files.is_empty(), "{raws} {jpgs}");
            assert!(sel.missing.is_empty(), "{raws} {jpgs}");
            assert_eq!(sel.pruned, 2, "{raws} {jpgs}");
        }
    }

    #[test]
    fn a_pruned_jpg_does_not_stop_its_raw_when_only_raws_are_wanted() {
        let mut c = Collection::new();
        let raw = add(&mut c, "/s/RAW/IMG_1.ARW", true, false);
        add(&mut c, "/s/JPG/IMG_1.JPG", false, true);
        c.build_pairs();
        assert_eq!(select_marked(&c, true, false).files, vec![raw]);
        assert!(select_marked(&c, false, true).files.is_empty());
    }

    #[test]
    fn the_plan_gives_both_files_of_a_shot_their_own_name_without_renaming() {
        let (c, [raw1, jpg1, ..]) = shoot([true, true, false, false]);
        let sel = select_marked(&c, true, true);
        assert_eq!(sel.files.len(), 2);
        let plan = EditCopyPlan::new(&sel.files, &c, Path::new("/nonexistent/edits"));
        assert_eq!(plan.to_copy_count(), 2);
        let mut names: Vec<_> = plan.ops.iter().map(|o| util::file_name(&o.dest)).collect();
        names.sort();
        assert_eq!(names, vec!["IMG_1.ARW", "IMG_1.JPG"]);
        assert!(plan.ops.iter().all(|o| !o.renamed()));
        let ids: HashSet<_> = plan.ops.iter().map(|o| o.id).collect();
        assert_eq!(ids, HashSet::from([raw1, jpg1]));
    }

    #[test]
    fn only_a_raw_carries_a_crop_into_the_plan() {
        let mut c = Collection::new();
        let crop = Crop { left: 0.1, top: 0.1, right: 0.9, bottom: 0.9, aspect_label: None };
        for path in ["/s/RAW/IMG_1.ARW", "/s/JPG/IMG_1.JPG"] {
            let id = add(&mut c, path, true, false);
            c.get_mut(id).unwrap().crop = Some(crop.clone());
        }
        c.build_pairs();
        let sel = select_marked(&c, true, true);
        let plan = EditCopyPlan::new(&sel.files, &c, Path::new("/nonexistent/edits"));
        assert_eq!(plan.ops.len(), 2);
        for op in &plan.ops {
            assert_eq!(op.is_cropped(), op.filename.ends_with(".ARW"), "{}", op.filename);
        }
    }

    // ── execution, with RawTherapee profiles ────────────────────────────────

    struct Scratch {
        root: PathBuf,
        src: PathBuf,
        dest: PathBuf,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!("pp-editcopy-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (src, dest) = (root.join("shoot"), root.join("edits"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        Scratch { root, src, dest }
    }

    /// A real file in the shoot, marked, as a record of the collection.
    fn put(c: &mut Collection, dir: &Path, name: &str, body: &[u8]) -> RecordId {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        let meta = fs::metadata(&p).unwrap();
        let (secs, nanos) = crate::model::record::unix_mtime(&meta);
        let mut r = PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), meta.len(), MTime::from_unix(secs, nanos), None);
        r.is_marked = true;
        c.add(r)
    }

    fn run(c: &Collection, dest: &Path, raws: bool, jpgs: bool, profile: &ProfileChoice) -> (EditCopyPlan, CopyReport) {
        let sel = select_marked(c, raws, jpgs);
        let plan = EditCopyPlan::new(&sel.files, c, dest);
        let report = plan.execute(profile, &AtomicBool::new(false), &|_| {});
        (plan, report)
    }

    fn pp3s(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".pp3"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn each_raw_copied_gets_the_profile_under_its_final_name() {
        let s = scratch("naming");
        let mut c = Collection::new();
        put(&mut c, &s.src, "20261012_2044_2048.ARW", b"raw one");
        put(&mut c, &s.src, "20261012_2044_2048.JPG", b"jpg one");
        put(&mut c, &s.src, "20261012_2045_2049.ARW", b"raw two");
        c.build_pairs();
        // A different file already holds the first RAW's name: it becomes _1.
        fs::write(s.dest.join("20261012_2044_2048.ARW"), b"someone else's").unwrap();
        let (_, report) = run(&c, &s.dest, true, true, &ProfileChoice::Base);
        assert_eq!(report.copied.len(), 3);
        assert_eq!(pp3s(&s.dest), ["20261012_2044_2048_1.ARW.pp3", "20261012_2045_2049.ARW.pp3"]);
        assert_eq!(fs::read_to_string(s.dest.join("20261012_2044_2048_1.ARW.pp3")).unwrap(), rt_profile::BASE);
        assert_eq!(report.profiles.written.len(), 2);
        assert_eq!((report.profiles.kept, report.profiles.failed.len()), (0, 0));
        // The JPG got none, and the RAW is copied as it was.
        assert_eq!(fs::read(s.dest.join("20261012_2044_2048_1.ARW")).unwrap(), b"raw one");
    }

    #[test]
    fn an_existing_profile_is_kept_and_counted() {
        let s = scratch("kept");
        let mut c = Collection::new();
        put(&mut c, &s.src, "IMG_1.ARW", b"raw");
        fs::write(s.dest.join("IMG_1.ARW.pp3"), b"[Version]\nedited in RawTherapee\n").unwrap();
        let (_, report) = run(&c, &s.dest, true, false, &ProfileChoice::Base);
        assert_eq!(report.copied.len(), 1);
        assert_eq!(fs::read(s.dest.join("IMG_1.ARW.pp3")).unwrap(), b"[Version]\nedited in RawTherapee\n");
        assert_eq!((report.profiles.written.len(), report.profiles.kept), (0, 1));
    }

    #[test]
    fn a_raw_already_there_gets_a_missing_profile_and_keeps_one_it_has() {
        let s = scratch("identical");
        let mut c = Collection::new();
        put(&mut c, &s.src, "IMG_1.ARW", b"raw one");
        put(&mut c, &s.src, "IMG_2.ARW", b"raw two");
        // Both already copied (identical: same size, same mtime) …
        for name in ["IMG_1.ARW", "IMG_2.ARW"] {
            let meta = fs::metadata(s.src.join(name)).unwrap();
            fsutil::safe_copy(&s.src.join(name), &s.dest.join(name), &meta, false).unwrap();
        }
        // … one with a profile edited since.
        fs::write(s.dest.join("IMG_2.ARW.pp3"), b"mine").unwrap();
        let (plan, report) = run(&c, &s.dest, true, false, &ProfileChoice::Base);
        assert_eq!((plan.to_copy_count(), plan.existing_count()), (0, 2));
        assert!(report.copied.is_empty());
        assert_eq!(pp3s(&s.dest), ["IMG_1.ARW.pp3", "IMG_2.ARW.pp3"]);
        assert_eq!(fs::read_to_string(s.dest.join("IMG_1.ARW.pp3")).unwrap(), rt_profile::BASE);
        assert_eq!(fs::read(s.dest.join("IMG_2.ARW.pp3")).unwrap(), b"mine");
        assert_eq!((report.profiles.written, report.profiles.kept), (vec![s.dest.join("IMG_1.ARW")], 1));
    }

    #[test]
    fn jpg_only_runs_and_no_profile_write_none() {
        let s = scratch("none");
        let mut c = Collection::new();
        put(&mut c, &s.src, "IMG_1.ARW", b"raw");
        put(&mut c, &s.src, "IMG_1.JPG", b"jpg");
        c.build_pairs();
        // JPGs only: even a custom profile that isn't there isn't read.
        let (_, report) = run(&c, &s.dest, false, true, &ProfileChoice::Custom(s.root.join("gone.pp3")));
        assert_eq!(report.copied.len(), 1);
        assert!(report.profiles.refused.is_none());
        assert!(pp3s(&s.dest).is_empty());
        // No profile: the RAW is copied bare.
        let (_, report) = run(&c, &s.dest, true, false, &ProfileChoice::None);
        assert_eq!(report.copied.len(), 1);
        assert!(pp3s(&s.dest).is_empty());
        assert!(report.profiles.written.is_empty() && report.profiles.kept == 0);
    }

    #[test]
    fn a_custom_profile_that_cant_be_used_stops_the_run_before_any_copy() {
        let s = scratch("refused");
        let mut c = Collection::new();
        put(&mut c, &s.src, "IMG_1.ARW", b"raw");
        let custom = ProfileChoice::Custom(s.root.join("Portrait.pp3"));
        let (_, report) = run(&c, &s.dest, true, false, &custom);
        assert_eq!(report.profiles.refused.as_deref(), Some("Portrait.pp3 can't be used: it's no longer there"));
        assert!(report.copied.is_empty() && report.failed.is_empty());
        assert!(!s.dest.join("IMG_1.ARW").exists());
        // Not a profile: refused the same way.
        fs::write(s.root.join("Portrait.pp3"), b"just some notes").unwrap();
        let (_, report) = run(&c, &s.dest, true, false, &custom);
        assert!(report.profiles.refused.unwrap().contains("[Version]"));
        assert!(!s.dest.join("IMG_1.ARW").exists());
        // A good one is copied beside the RAW byte for byte.
        fs::write(s.root.join("Portrait.pp3"), b"[Version]\r\nAppVersion=5.13\r\n").unwrap();
        let (_, report) = run(&c, &s.dest, true, false, &custom);
        assert_eq!(report.copied.len(), 1);
        assert_eq!(fs::read(s.dest.join("IMG_1.ARW.pp3")).unwrap(), b"[Version]\r\nAppVersion=5.13\r\n");
    }

    #[test]
    fn a_profile_that_cant_be_written_leaves_the_raw_copied() {
        let s = scratch("blocked");
        let mut c = Collection::new();
        put(&mut c, &s.src, "IMG_1.ARW", b"raw");
        // A folder squats on the profile's temp name, so its write fails.
        fs::create_dir_all(fsutil::tmp_sibling(&s.dest.join("IMG_1.ARW.pp3")).join("x")).unwrap();
        let (_, report) = run(&c, &s.dest, true, false, &ProfileChoice::Base);
        assert_eq!(report.copied.len(), 1);
        assert!(report.failed.is_empty());
        assert_eq!(fs::read(s.dest.join("IMG_1.ARW")).unwrap(), b"raw");
        assert_eq!(report.profiles.failed.len(), 1);
        assert_eq!(report.profiles.failed[0].0, "IMG_1.ARW.pp3");
        assert!(!s.dest.join("IMG_1.ARW.pp3").exists());
    }
}
