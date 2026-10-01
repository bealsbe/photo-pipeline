//! The file side of undo: moving files back to where they were (or forward
//! again, for a redo) and taking trashed files back out of the Trash.
//!
//! Both run on a worker thread and go through [`fsutil`]: nothing is ever
//! replaced (`RENAME_NOREPLACE`), and a move across drives copies and
//! verifies the file before it removes the source.  A file that can't go back
//! is left where it is and reported with the reason.
//!
//! Finding a trashed file again: the `trash` crate doesn't say where it put
//! a file, so the Trash folders it could have used (the home Trash, and
//! `.Trash/$uid` or `.Trash-$uid` at the top of the file's own drive) are
//! searched for an entry with the file's original path and a deletion time
//! inside the window the trash operation ran in.  Only those folders are read,
//! and only entries named after the file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::fsutil;
use crate::model::collection::RecordId;
use crate::util;

/// Move `src` → `dest` for record `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOp {
    pub id: RecordId,
    pub src: PathBuf,
    pub dest: PathBuf,
}

/// A file that was left alone: (file name, why).
pub type Failure = (String, String);

#[derive(Debug, Default)]
pub struct MoveReport {
    pub moved: Vec<MoveOp>,
    pub failed: Vec<Failure>,
    /// Empty folders removed afterwards.
    pub removed_dirs: Vec<PathBuf>,
}

/// The folder a path is in, as the user knows it: "shoot", or "shoot/RAW"
/// for a Sort subfolder.
fn folder_of(path: &Path) -> String {
    let parent = path.parent().unwrap_or(Path::new(""));
    let Some(name) = parent.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return parent.to_string_lossy().into_owned();
    };
    let upper = name.to_uppercase();
    match parent.parent().and_then(|g| g.file_name()) {
        Some(g) if upper == "RAW" || upper == "JPG" => format!("{}/{name}", g.to_string_lossy()),
        _ => name,
    }
}

/// Move each file, never replacing anything.  Afterwards each folder in
/// `cleanup` is removed if it is empty (the RAW/ and JPG/ folders a Sort
/// made); a folder with anything left in it stays.
pub fn execute_moves(ops: &[MoveOp], cleanup: &[PathBuf]) -> MoveReport {
    let mut report = MoveReport::default();
    for op in ops {
        match move_one(op) {
            Ok(()) => report.moved.push(op.clone()),
            Err(why) => report.failed.push((util::file_name(&op.src), why)),
        }
    }
    for dir in cleanup {
        // Only ever succeeds on an empty folder.
        if fs::remove_dir(dir).is_ok() {
            fsutil::sync_parent(dir);
            report.removed_dirs.push(dir.clone());
        }
    }
    report
}

fn move_one(op: &MoveOp) -> Result<(), String> {
    let Ok(meta) = fs::metadata(&op.src) else {
        return Err(format!("it is no longer in {}", folder_of(&op.src)));
    };
    let taken = || {
        format!(
            "{} already has a file named {}, so this one was left in {}",
            folder_of(&op.dest),
            util::file_name(&op.dest),
            folder_of(&op.src)
        )
    };
    if fsutil::lexists(&op.dest) {
        return Err(taken());
    }
    fsutil::ensure_parent(&op.dest).map_err(|e| format!("could not create {}: {e}", folder_of(&op.dest)))?;
    match fsutil::safe_move(&op.src, &op.dest, &meta, false) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(taken()),
        Err(e) => Err(e.to_string()),
    }
}

// ── the Trash ───────────────────────────────────────────────────────────────

/// A trashed file to put back at `path`.
#[derive(Debug, Clone)]
pub struct TrashTarget {
    pub id: RecordId,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    /// (record, where it is now, its metadata there).
    pub restored: Vec<(RecordId, PathBuf, Option<fs::Metadata>)>,
    pub failed: Vec<Failure>,
}

/// The Trash records deletion times to the second, in local time: allow this
/// much either side of the trash operation's window.
const SLACK_S: i64 = 2;

const NOT_IN_TRASH: &str = "it is not in the Trash any more (it may have been restored already, or the Trash emptied)";

/// Put each target back from the Trash, matching its entry by original path
/// and a deletion time inside `window` (unix seconds).
pub fn restore_from_trash(targets: &[TrashTarget], window: (i64, i64)) -> RestoreReport {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut bins: Vec<PathBuf> = Vec::new();
        for t in targets {
            for b in freedesktop::bins_for(&t.path) {
                if !bins.contains(&b) {
                    bins.push(b);
                }
            }
        }
        freedesktop::restore_in(&bins, targets, window)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = window;
        RestoreReport {
            restored: Vec::new(),
            failed: targets
                .iter()
                .map(|t| (util::file_name(&t.path), "putting files back from the Trash isn't supported on this system; restore them from the Trash yourself".into()))
                .collect(),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub mod freedesktop {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use chrono::{Local, NaiveDateTime, TimeZone};

    use super::{NOT_IN_TRASH, RestoreReport, SLACK_S, TrashTarget, folder_of};
    use crate::ops::fsutil;
    use crate::util;

    /// The home Trash, as the `trash` crate finds it.
    fn home_trash() -> Option<PathBuf> {
        if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(d).join("Trash"));
        }
        std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local/share/Trash"))
    }

    /// `path` with its folder resolved (symlinks, `..`), as the `trash`
    /// crate records it.
    fn canonical(path: &Path) -> Option<PathBuf> {
        Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?))
    }

    /// `\040`-style escapes in /proc/self/mounts.
    fn unescape_mount(s: &str) -> PathBuf {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
                out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        PathBuf::from(OsString::from_vec(out))
    }

    /// The mount point a path lives under (the longest one that contains it).
    fn topdir(path: &Path) -> Option<PathBuf> {
        let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
        mounts
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1))
            .map(unescape_mount)
            .filter(|m| path.starts_with(m))
            .max_by_key(|m| m.components().count())
    }

    /// Every Trash folder a file at `path` could have been moved into.
    pub fn bins_for(path: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(h) = home_trash() {
            out.push(h);
        }
        let real = canonical(path).unwrap_or_else(|| path.to_path_buf());
        if let Some(top) = topdir(&real) {
            // SAFETY: getuid has no preconditions and cannot fail.
            let uid = unsafe { libc::getuid() };
            out.push(top.join(".Trash").join(uid.to_string()));
            out.push(top.join(format!(".Trash-{uid}")));
        }
        out
    }

    /// `%20` → space, byte for byte (the Trash's `Path=` is URI-encoded).
    fn decode_uri(s: &str) -> PathBuf {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        PathBuf::from(OsString::from_vec(out))
    }

    /// For `<name>.trashinfo` → 1, `<name>.<n>.trashinfo` → n; else None.
    fn appendage(info_name: &OsStr, name: &OsStr) -> Option<u64> {
        let rest = info_name.as_bytes().strip_prefix(name.as_bytes())?.strip_suffix(b".trashinfo")?;
        if rest.is_empty() {
            return Some(1);
        }
        let digits = rest.strip_prefix(b".")?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    /// The drive root a per-drive Trash folder belongs to (relative `Path=`
    /// entries are relative to it).  None for the home Trash.
    fn bin_topdir(bin: &Path) -> Option<&Path> {
        let name = bin.file_name()?.to_string_lossy();
        if name.starts_with(".Trash-") {
            return bin.parent();
        }
        let parent = bin.parent()?;
        (parent.file_name()? == ".Trash").then(|| parent.parent()).flatten()
    }

    struct Info {
        path: PathBuf,
        /// The deletion time as unix seconds (both readings across a DST
        /// change).
        deleted: Vec<i64>,
    }

    fn read_info(file: &Path, bin: &Path) -> Option<Info> {
        let text = fs::read_to_string(file).ok()?;
        let (mut path, mut deleted) = (None, Vec::new());
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "Path" => {
                    let p = decode_uri(value.trim());
                    path = Some(if p.is_relative() { bin_topdir(bin)?.join(p) } else { p });
                }
                "DeletionDate" => {
                    let local = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%dT%H:%M:%S").ok()?;
                    let r = Local.from_local_datetime(&local);
                    deleted = [r.earliest(), r.latest()].into_iter().flatten().map(|t| t.timestamp()).collect();
                }
                _ => {}
            }
        }
        Some(Info { path: path?, deleted })
    }

    /// [`super::restore_from_trash`] over the given Trash folders.
    pub fn restore_in(bins: &[PathBuf], targets: &[TrashTarget], window: (i64, i64)) -> RestoreReport {
        let mut report = RestoreReport::default();
        // One listing per Trash folder, however many files are restored.
        let listings: Vec<(&PathBuf, Vec<OsString>)> = bins
            .iter()
            .filter_map(|b| {
                let rd = fs::read_dir(b.join("info")).ok()?;
                Some((b, rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect()))
            })
            .collect();
        let (lo, hi) = (window.0 - SLACK_S, window.1 + SLACK_S);
        for t in targets {
            let name = util::file_name(&t.path);
            let Some(file_name) = t.path.file_name() else {
                report.failed.push((name, NOT_IN_TRASH.into()));
                continue;
            };
            let real = canonical(&t.path);
            // The newest matching entry: (deleted, appendage, folder, info name).
            let mut best: Option<(i64, u64, &PathBuf, &OsString)> = None;
            for (bin, names) in &listings {
                for n in names {
                    let Some(app) = appendage(n, file_name) else { continue };
                    let Some(info) = read_info(&bin.join("info").join(n), bin) else { continue };
                    if info.path != t.path && Some(&info.path) != real.as_ref() {
                        continue;
                    }
                    let Some(&when) = info.deleted.iter().filter(|&&d| d >= lo && d <= hi).max() else { continue };
                    if best.is_none_or(|b| (when, app) > (b.0, b.1)) {
                        best = Some((when, app, bin, n));
                    }
                }
            }
            let Some((_, _, bin, info_name)) = best else {
                report.failed.push((name, NOT_IN_TRASH.into()));
                continue;
            };
            match restore_one(bin, info_name, &t.path) {
                Ok(()) => report.restored.push((t.id, t.path.clone(), fs::metadata(&t.path).ok())),
                Err(why) => report.failed.push((name, why)),
            }
        }
        report
    }

    fn restore_one(bin: &Path, info_name: &OsStr, dest: &Path) -> Result<(), String> {
        let info = bin.join("info").join(info_name);
        let in_trash = info_name.as_bytes().strip_suffix(b".trashinfo").map(OsStr::from_bytes).unwrap_or(info_name);
        let src = bin.join("files").join(in_trash);
        let meta = match fs::symlink_metadata(&src) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Err("the Trash holds something other than a file under that name, so it was left there".into()),
            Err(_) => return Err(NOT_IN_TRASH.into()),
        };
        let taken = || format!("{} already has a file named {}, so the trashed one was left in the Trash", folder_of(dest), util::file_name(dest));
        if fsutil::lexists(dest) {
            return Err(taken());
        }
        fsutil::ensure_parent(dest).map_err(|e| format!("could not recreate {}: {e}", folder_of(dest)))?;
        match fsutil::safe_move(&src, dest, &meta, false) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(taken()),
            Err(e) => return Err(e.to_string()),
        }
        // The file is back; its Trash entry goes.  If that fails the Trash
        // shows a stale entry, but nothing is lost.
        if let Err(e) = fs::remove_file(&info) {
            eprintln!("undo: restored {} but could not remove {}: {e}", dest.display(), info.display());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) mod tests {
        use super::*;

        pub fn encode(path: &Path) -> String {
            let mut out = String::new();
            for b in path.as_os_str().as_bytes() {
                match b {
                    b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
                    c if c.is_ascii_alphanumeric() => out.push(*c as char),
                    c => out.push_str(&format!("%{c:02X}")),
                }
            }
            out
        }

        /// Put `content` in a fake Trash folder as `name_in_trash`, as if
        /// `original` had been trashed at `when`.
        pub fn fake_trash(bin: &Path, name_in_trash: &str, original: &Path, when: NaiveDateTime, content: &[u8]) {
            fs::create_dir_all(bin.join("files")).unwrap();
            fs::create_dir_all(bin.join("info")).unwrap();
            fs::write(bin.join("files").join(name_in_trash), content).unwrap();
            let info = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", encode(original), when.format("%Y-%m-%dT%H:%M:%S"));
            fs::write(bin.join("info").join(format!("{name_in_trash}.trashinfo")), info).unwrap();
        }

        #[test]
        fn names_and_escapes() {
            assert_eq!(appendage(OsStr::new("a.jpg.trashinfo"), OsStr::new("a.jpg")), Some(1));
            assert_eq!(appendage(OsStr::new("a.jpg.12.trashinfo"), OsStr::new("a.jpg")), Some(12));
            assert_eq!(appendage(OsStr::new("a.jpg.x.trashinfo"), OsStr::new("a.jpg")), None);
            assert_eq!(appendage(OsStr::new("ba.jpg.trashinfo"), OsStr::new("a.jpg")), None);
            assert_eq!(decode_uri("/a%20b/c%25.jpg"), PathBuf::from("/a b/c%.jpg"));
            assert_eq!(unescape_mount("/mnt/my\\040disk"), PathBuf::from("/mnt/my disk"));
            assert_eq!(bin_topdir(Path::new("/mnt/x/.Trash-1000")), Some(Path::new("/mnt/x")));
            assert_eq!(bin_topdir(Path::new("/mnt/x/.Trash/1000")), Some(Path::new("/mnt/x")));
            assert_eq!(bin_topdir(Path::new("/home/u/.local/share/Trash")), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-undo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn moves_back_without_overwriting_and_tidies_empty_folders() {
        let d = dir("moves");
        let (raw_dir, jpg_dir) = (d.join("RAW"), d.join("JPG"));
        fs::create_dir_all(&raw_dir).unwrap();
        fs::create_dir_all(&jpg_dir).unwrap();
        fs::write(raw_dir.join("a.arw"), b"raw a").unwrap();
        fs::write(jpg_dir.join("a.jpg"), b"jpg a").unwrap();
        fs::write(jpg_dir.join("b.jpg"), b"jpg b").unwrap();
        // Something new took b.jpg's old place: it must not be replaced.
        fs::write(d.join("b.jpg"), b"precious").unwrap();
        let ops = vec![
            MoveOp { id: 1, src: raw_dir.join("a.arw"), dest: d.join("a.arw") },
            MoveOp { id: 2, src: jpg_dir.join("a.jpg"), dest: d.join("a.jpg") },
            MoveOp { id: 3, src: jpg_dir.join("b.jpg"), dest: d.join("b.jpg") },
            MoveOp { id: 4, src: jpg_dir.join("gone.jpg"), dest: d.join("gone.jpg") },
        ];
        let r = execute_moves(&ops, &[raw_dir.clone(), jpg_dir.clone()]);
        assert_eq!(r.moved.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(r.failed.len(), 2);
        assert!(r.failed[0].1.contains("already has a file named b.jpg"), "{:?}", r.failed);
        assert!(r.failed[1].1.contains("no longer in"), "{:?}", r.failed);
        assert_eq!(fs::read(d.join("a.arw")).unwrap(), b"raw a");
        assert_eq!(fs::read(d.join("b.jpg")).unwrap(), b"precious");
        assert_eq!(fs::read(jpg_dir.join("b.jpg")).unwrap(), b"jpg b");
        // RAW/ is empty and goes; JPG/ still holds b.jpg and stays.
        assert!(!raw_dir.exists());
        assert!(jpg_dir.is_dir());
        assert_eq!(r.removed_dirs, vec![raw_dir.clone()]);
        // Moving forward again (a redo) recreates the folder.
        let redo = vec![MoveOp { id: 1, src: d.join("a.arw"), dest: raw_dir.join("a.arw") }];
        let r = execute_moves(&redo, &[]);
        assert_eq!(r.moved.len(), 1);
        assert_eq!(fs::read(raw_dir.join("a.arw")).unwrap(), b"raw a");
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn restores_the_entry_from_this_operation_only() {
        use chrono::{Duration, Local};
        use freedesktop::tests::fake_trash;
        let d = dir("restore");
        let shoot = d.join("my shoot");
        fs::create_dir_all(&shoot).unwrap();
        let bin = d.join("Trash");
        let now = Local::now().naive_local();
        let window = (Local::now().timestamp(), Local::now().timestamp());
        let a = shoot.join("a.jpg");
        // An older trashing of a file with the same path, and this one.
        fake_trash(&bin, "a.jpg", &a, now - Duration::hours(3), b"old a");
        fake_trash(&bin, "a.jpg.2", &a, now, b"new a");
        // b.jpg was trashed, but something new is at its place now.
        let b = shoot.join("b.jpg");
        fake_trash(&bin, "b.jpg", &b, now, b"trashed b");
        fs::write(&b, b"precious").unwrap();
        // c.jpg was trashed long before this operation: not ours.
        let c = shoot.join("c.jpg");
        fake_trash(&bin, "c.jpg", &c, now - Duration::days(2), b"old c");
        let targets = vec![
            TrashTarget { id: 1, path: a.clone() },
            TrashTarget { id: 2, path: b.clone() },
            TrashTarget { id: 3, path: c.clone() },
        ];
        let r = freedesktop::restore_in(&[bin.clone(), d.join("no-such-trash")], &targets, window);
        assert_eq!(r.restored.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1]);
        assert!(r.restored[0].2.is_some());
        assert_eq!(fs::read(&a).unwrap(), b"new a");
        assert!(!bin.join("info/a.jpg.2.trashinfo").exists());
        assert!(!bin.join("files/a.jpg.2").exists());
        // The older entry is untouched.
        assert_eq!(fs::read(bin.join("files/a.jpg")).unwrap(), b"old a");
        assert!(bin.join("info/a.jpg.trashinfo").exists());
        // b: refused, both copies intact.
        assert_eq!(fs::read(&b).unwrap(), b"precious");
        assert_eq!(fs::read(bin.join("files/b.jpg")).unwrap(), b"trashed b");
        assert!(r.failed.iter().any(|(n, why)| n == "b.jpg" && why.contains("already has a file named b.jpg")), "{:?}", r.failed);
        // c: not found in this operation's window.
        assert!(!c.exists());
        assert!(r.failed.iter().any(|(n, why)| n == "c.jpg" && why.contains("not in the Trash")), "{:?}", r.failed);
        let _ = fs::remove_dir_all(&d);
    }
}
