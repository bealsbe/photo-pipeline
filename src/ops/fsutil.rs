//! File-safety primitives shared by every operation that writes photos.
//!
//! Guarantees, for every RAW and JPG:
//!
//! * **Copies are atomic.**  Data goes to a `<dest>.~tmp` sibling, is
//!   fsynced and size-verified, and only then renamed into place.  A failure
//!   at any point leaves the destination exactly as it was.
//! * **Nothing is overwritten by accident.**  Unless the caller explicitly
//!   asked to overwrite, the final rename is `RENAME_NOREPLACE` (Linux), so a
//!   file that appears at the destination between planning and execution is
//!   reported, not replaced.
//! * **A source is only deleted once its copy is durable.**  Cross-device
//!   moves fsync the new file and its directory before unlinking the source.
//! * **Timestamps are kept** (atime + mtime), as `shutil.copy2` + `utime` did.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use filetime::FileTime;

/// `<dest>.~tmp` — the sibling a copy is written to before the rename.
pub fn tmp_sibling(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".~tmp");
    PathBuf::from(s)
}

/// fsync a path's parent directory so a rename or create is durable.
/// Best effort: some filesystems refuse to open directories for sync.
pub fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
        if let Ok(f) = File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Does anything (including a dangling symlink) exist at `path`?
pub fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Rename `src` → `dest`, failing with `AlreadyExists` instead of replacing
/// an existing destination.
pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        match renameat_with(CWD, src, CWD, dest, RenameFlags::NOREPLACE) {
            Ok(()) => return Ok(()),
            Err(e) if e == rustix::io::Errno::EXIST => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"));
            }
            // Filesystem or kernel without RENAME_NOREPLACE: fall through.
            Err(e) if e == rustix::io::Errno::INVAL || e == rustix::io::Errno::NOSYS => {}
            Err(e) => return Err(io::Error::from_raw_os_error(e.raw_os_error())),
        }
    }
    // Portable fallback: a hard link fails atomically if dest exists.
    match fs::hard_link(src, dest) {
        Ok(()) => {
            fs::remove_file(src)?;
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"))
        }
        Err(_) => {
            // No hard links either (FAT, some network shares): check, then
            // rename.  This is the only path with a check-then-act window.
            if lexists(dest) {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"));
            }
            fs::rename(src, dest)
        }
    }
}

/// Rename, replacing `dest` if `overwrite`, otherwise refusing to.
pub fn rename(src: &Path, dest: &Path, overwrite: bool) -> io::Result<()> {
    if overwrite { fs::rename(src, dest) } else { rename_noreplace(src, dest) }
}

fn set_times_from(path: &Path, meta: &fs::Metadata) {
    let atime = FileTime::from_last_access_time(meta);
    let mtime = FileTime::from_last_modification_time(meta);
    let _ = filetime::set_file_times(path, atime, mtime);
}

/// Restore a source's atime + mtime on `path` (best effort).
pub fn copy_times(path: &Path, src_meta: &fs::Metadata) {
    set_times_from(path, src_meta);
}

/// Copy `src` → `dest` atomically (see the module docs).
///
/// `src_meta` is the source's metadata taken before the copy; the copy is
/// rejected if its size differs.  With `overwrite == false` an existing
/// destination is never replaced — the call fails with `AlreadyExists`.
pub fn safe_copy(src: &Path, dest: &Path, src_meta: &fs::Metadata, overwrite: bool) -> io::Result<()> {
    let tmp = tmp_sibling(dest);
    let result = (|| {
        // A stale temp from an earlier crash is ours to discard.
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        fs::copy(src, &tmp)?;
        {
            let f = File::options().write(true).open(&tmp)?;
            f.sync_all()?;
        }
        let got = fs::metadata(&tmp)?.len();
        if got != src_meta.len() {
            return Err(io::Error::other(format!(
                "size mismatch after copy: {} ({} of {} bytes)",
                crate::util::file_name(src),
                got,
                src_meta.len()
            )));
        }
        copy_xattrs(src, &tmp);
        set_times_from(&tmp, src_meta);
        rename(&tmp, dest, overwrite)?;
        sync_parent(dest);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Extended attributes, best effort — what `shutil.copy2` did for the
/// Python app.  Attributes the destination refuses (a filesystem without
/// xattrs, a protected namespace) are skipped, as Python skips them.
fn copy_xattrs(src: &Path, dest: &Path) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        use rustix::fs::{XattrFlags, getxattr, listxattr, setxattr};
        let Ok(len) = listxattr(src, &mut [0u8; 0][..]) else { return };
        if len == 0 {
            return;
        }
        let mut names = vec![0u8; len];
        let Ok(len) = listxattr(src, &mut names[..]) else { return };
        for name in names[..len.min(names.len())].split(|&b| b == 0).filter(|n| !n.is_empty()) {
            let name = std::ffi::OsStr::from_bytes(name);
            let Ok(size) = getxattr(src, name, &mut [0u8; 0][..]) else { continue };
            let mut value = vec![0u8; size];
            let Ok(size) = getxattr(src, name, &mut value[..]) else { continue };
            let _ = setxattr(dest, name, &value[..size.min(value.len())], XattrFlags::empty());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (src, dest);
}

/// Move `src` → `dest`.  Same filesystem: one atomic rename.  Across
/// devices: [`safe_copy`], then the source is removed.
pub fn safe_move(src: &Path, dest: &Path, src_meta: &fs::Metadata, overwrite: bool) -> io::Result<()> {
    match rename(src, dest, overwrite) {
        Ok(()) => {
            sync_parent(dest);
            set_times_from(dest, src_meta);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            // Cross-device (or a share that refuses renames): copy, make it
            // durable, and only then drop the original.
            safe_copy(src, dest, src_meta, overwrite)?;
            fs::remove_file(src)?;
            sync_parent(src);
            Ok(())
        }
    }
}

/// Write bytes produced in memory (a re-encoded JPEG, an XMP packet) to
/// `dest` through the same temp + fsync + rename sequence.
pub fn atomic_write(dest: &Path, data: &[u8], overwrite: bool) -> io::Result<()> {
    use std::io::Write;
    let tmp = tmp_sibling(dest);
    let result = (|| {
        let _ = fs::remove_file(&tmp);
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        rename(&tmp, dest, overwrite)?;
        sync_parent(dest);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `mkdir -p` for a file's parent directory.
pub fn ensure_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => fs::create_dir_all(p),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn safe_copy_keeps_user_xattrs() {
        let d = std::env::temp_dir().join(format!("pp-xattr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let (src, dest) = (d.join("a.jpg"), d.join("b.jpg"));
        fs::write(&src, b"pixels").unwrap();
        // Not every filesystem (e.g. some tmpfs setups) takes user xattrs.
        if rustix::fs::setxattr(&src, "user.pp.test", b"kept", rustix::fs::XattrFlags::empty()).is_err() {
            return;
        }
        let meta = fs::metadata(&src).unwrap();
        safe_copy(&src, &dest, &meta, false).unwrap();
        let mut buf = [0u8; 16];
        let n = rustix::fs::getxattr(&dest, "user.pp.test", &mut buf[..]).unwrap();
        assert_eq!(&buf[..n], b"kept");
        let _ = fs::remove_dir_all(&d);
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-fsutil-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copy_never_clobbers_without_overwrite() {
        let d = dir("noclobber");
        let (a, b) = (d.join("a.jpg"), d.join("b.jpg"));
        fs::write(&a, b"new").unwrap();
        fs::write(&b, b"precious").unwrap();
        let meta = fs::metadata(&a).unwrap();
        let err = safe_copy(&a, &b, &meta, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"precious");
        assert!(!tmp_sibling(&b).exists());
        safe_copy(&a, &b, &meta, true).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"new");
    }

    #[test]
    fn move_keeps_mtime_and_refuses_existing() {
        let d = dir("move");
        let (a, b) = (d.join("a.arw"), d.join("sub/a.arw"));
        fs::write(&a, b"raw").unwrap();
        let old = FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&a, old).unwrap();
        let meta = fs::metadata(&a).unwrap();
        ensure_parent(&b).unwrap();
        safe_move(&a, &b, &meta, false).unwrap();
        assert!(!a.exists());
        let m = fs::metadata(&b).unwrap();
        assert_eq!(FileTime::from_last_modification_time(&m), old);

        fs::write(&a, b"other").unwrap();
        let meta = fs::metadata(&a).unwrap();
        assert!(safe_move(&a, &b, &meta, false).is_err());
        assert!(a.exists());
        assert_eq!(fs::read(&b).unwrap(), b"raw");
    }
}
