//! Where a folder lives (a local disk or a network share) and whether it can
//! be reached right now.
//!
//! `statfs` names the filesystem type, but on a dead network mount it (like
//! any call that touches the folder) can block for a long time: a hard CIFS
//! mount waits for the server, and an automount that can't mount waits for
//! its timeout.  So [`probe`] runs those calls on a helper thread and gives
//! up after a timeout.  Call it from a worker, never the UI thread.
//!
//! The mount table (`/proc/self/mountinfo`) and `/etc/fstab` name the share
//! (`//nas/share`) and still say where it would be when it isn't mounted.
//! Reading them never waits on the share itself.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A network filesystem that a folder is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// "SMB", "NFS", "SSHFS"…
    pub kind: &'static str,
    /// What is mounted, e.g. `//nas/share` or `nas:/export/photos`.
    pub source: String,
    pub mount_point: PathBuf,
    /// Mounted now.  `false` when only fstab or an idle automount names it.
    pub mounted: bool,
}

impl Share {
    /// How the interface names the share: its source, else its mount point.
    pub fn name(&self) -> String {
        let s = self.source.trim();
        if s.is_empty() || s == "none" || s == "systemd-1" {
            self.mount_point.to_string_lossy().into_owned()
        } else {
            s.to_string()
        }
    }
}

/// Whether a folder answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// It answered.
    Ok,
    /// It isn't there, though the disk or share it would be on is.
    Missing,
    /// The share isn't mounted, or answered with a network error.
    Unreachable(String),
    /// No answer within the timeout.
    NotResponding,
    /// Some other error, such as permission denied.
    Failed(String),
}

impl Reach {
    pub fn is_ok(&self) -> bool {
        matches!(self, Reach::Ok)
    }

    /// The share or disk itself is away (not just the folder).
    pub fn is_away(&self) -> bool {
        matches!(self, Reach::Unreachable(_) | Reach::NotResponding)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// `Some` when the folder is on a network filesystem.
    pub share: Option<Share>,
    pub reach: Reach,
}

// ── classification ──────────────────────────────────────────────────────────

/// `statfs` f_type of FUSE filesystems; which one it is comes from the
/// mount table (`fuse.sshfs`…).
pub const FUSE_MAGIC: u32 = 0x6573_5546;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Magic {
    Network(&'static str),
    /// FUSE: network or not depending on the program behind it.
    Fuse,
    Local,
}

/// Classify a `statfs` f_type (the low 32 bits).
pub fn classify_magic(f_type: u32) -> Magic {
    match f_type {
        0xFF53_4D42 | 0xFE53_4D42 | 0x0000_517B => Magic::Network("SMB"),
        0x0000_6969 => Magic::Network("NFS"),
        0x5346_414F | 0x6B41_4653 => Magic::Network("AFS"),
        0x7375_7245 => Magic::Network("Coda"),
        0x0000_564C => Magic::Network("NCP"),
        0x0102_1997 => Magic::Network("9P"),
        0x00C3_6400 => Magic::Network("Ceph"),
        0x0BD0_0BD0 => Magic::Network("Lustre"),
        FUSE_MAGIC => Magic::Fuse,
        _ => Magic::Local,
    }
}

/// The share kind for a mount-table filesystem type, `None` when local.
pub fn network_fstype(fstype: &str) -> Option<&'static str> {
    let t = fstype.to_ascii_lowercase();
    let t = t.strip_prefix("fuse.").or_else(|| t.strip_prefix("fuseblk.")).unwrap_or(&t);
    Some(match t {
        "cifs" | "smb3" | "smbfs" | "smb" | "smbnetfs" => "SMB",
        "nfs" | "nfs4" => "NFS",
        "sshfs" => "SSHFS",
        "afs" | "openafs" => "AFS",
        "9p" => "9P",
        "ceph" | "ceph-fuse" => "Ceph",
        "glusterfs" => "GlusterFS",
        "lustre" => "Lustre",
        "coda" => "Coda",
        "ncpfs" => "NCP",
        "davfs" | "davfs2" | "webdavfs" => "WebDAV",
        "curlftpfs" => "FTP",
        "rclone" => "rclone",
        "s3fs" | "goofys" | "gcsfuse" | "juicefs" => "cloud storage",
        "gvfsd-fuse" => "GVfs",
        "kio-fuse" => "KIO",
        _ => return None,
    })
}

/// What a failed file operation says about where the file lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The share or disk can't be reached: the connection dropped, the
    /// server is down or stopped answering, or I/O failed.
    Unreachable,
    /// No such file or folder.  When the target is the folder itself (a
    /// sidecar in it, the folder being scanned) the folder is gone: moved,
    /// renamed, or on a share that isn't mounted.
    Missing,
    /// Anything else: permission denied, disk full, read-only…
    Other,
}

/// Classify an errno (`None`: an error without one).
pub fn classify_errno(errno: Option<i32>) -> Failure {
    let Some(e) = errno else { return Failure::Other };
    match e {
        libc::ENOENT | libc::ENOTDIR => Failure::Missing,
        libc::ENOTCONN
        | libc::EHOSTDOWN
        | libc::EHOSTUNREACH
        | libc::ETIMEDOUT
        | libc::EIO
        | libc::ENETDOWN
        | libc::ENETUNREACH
        | libc::ENETRESET
        | libc::ECONNABORTED
        | libc::ECONNRESET
        | libc::ECONNREFUSED
        | libc::ESHUTDOWN
        | libc::ESTALE
        | libc::EREMOTEIO
        | libc::ENOLINK
        | libc::ECOMM
        | libc::ENODEV
        | libc::ENXIO => Failure::Unreachable,
        _ => Failure::Other,
    }
}

pub fn classify_error(e: &io::Error) -> Failure {
    match e.raw_os_error() {
        Some(n) => classify_errno(Some(n)),
        None => match e.kind() {
            io::ErrorKind::NotFound => Failure::Missing,
            io::ErrorKind::TimedOut | io::ErrorKind::NotConnected | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
                Failure::Unreachable
            }
            _ => Failure::Other,
        },
    }
}

/// A short, plain reason for an error that means the share is away.
pub fn describe_errno(errno: Option<i32>) -> Option<&'static str> {
    Some(match errno? {
        libc::ENOTCONN | libc::ECONNRESET | libc::ECONNABORTED | libc::ESHUTDOWN | libc::ENOLINK | libc::ECOMM => {
            "the connection was lost"
        }
        libc::EHOSTDOWN | libc::EHOSTUNREACH | libc::ENETDOWN | libc::ENETUNREACH | libc::ECONNREFUSED => {
            "the server can't be reached"
        }
        libc::ETIMEDOUT => "the server stopped answering",
        libc::ESTALE => "the server lost track of the folder",
        libc::ENODEV | libc::ENXIO => "the share isn't mounted",
        libc::EIO | libc::EREMOTEIO => "reading or writing failed",
        _ => return None,
    })
}

// ── mount tables ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub mount_point: PathBuf,
    pub fstype: String,
    pub source: String,
}

/// Undo the octal escapes (`\040` for a space) of mountinfo and fstab.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            if v <= 255 {
                out.push(v as u8);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/proc/self/mountinfo`: `id parent dev root mount_point opts [optional…] - fstype source super_opts`.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mount_point = left.split_whitespace().nth(4)?;
            let mut r = right.split_whitespace();
            let fstype = r.next()?;
            let source = r.next().unwrap_or("");
            Some(Mount { mount_point: PathBuf::from(unescape(mount_point)), fstype: fstype.to_string(), source: unescape(source) })
        })
        .collect()
}

/// `/etc/fstab`: `source mount_point fstype options dump pass`.
pub fn parse_fstab(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut f = line.split_whitespace();
            let (source, mount_point, fstype) = (f.next()?, f.next()?, f.next()?);
            Some(Mount { mount_point: PathBuf::from(unescape(mount_point)), fstype: fstype.to_string(), source: unescape(source) })
        })
        .collect()
}

/// `path` with `.` and `..` resolved lexically (no filesystem access).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The mount that `path` is on: the longest mount point containing it and,
/// of several stacked on one point, the last (the one on top).
pub fn mount_of<'a>(mounts: &'a [Mount], path: &Path) -> Option<&'a Mount> {
    let path = lexical(path);
    let mut best: Option<&Mount> = None;
    for m in mounts {
        if path.starts_with(&m.mount_point)
            && best.is_none_or(|b| m.mount_point.components().count() >= b.mount_point.components().count())
        {
            best = Some(m);
        }
    }
    best
}

/// The share `path` is on, from the mount tables alone.
pub fn share_from_tables(mountinfo: &[Mount], fstab: &[Mount], path: &Path) -> Option<Share> {
    let current = mount_of(mountinfo, path);
    if let Some(m) = current {
        if let Some(kind) = network_fstype(&m.fstype) {
            return Some(Share { kind, source: m.source.clone(), mount_point: m.mount_point.clone(), mounted: true });
        }
    }
    // Not mounted: an idle automount point, or a plain folder where the
    // share would be.  fstab says what belongs there.
    let configured = mount_of(fstab, path)?;
    let kind = network_fstype(&configured.fstype)?;
    let deeper = current.is_none_or(|m| {
        m.fstype == "autofs" || configured.mount_point.components().count() > m.mount_point.components().count()
    });
    deeper.then(|| Share { kind, source: configured.source.clone(), mount_point: configured.mount_point.clone(), mounted: false })
}

fn read_tables() -> (Vec<Mount>, Vec<Mount>) {
    let mi = std::fs::read_to_string("/proc/self/mountinfo").map(|t| parse_mountinfo(&t)).unwrap_or_default();
    let fstab = std::fs::read_to_string("/etc/fstab").map(|t| parse_fstab(&t)).unwrap_or_default();
    (mi, fstab)
}

/// For trying the interface without a network share: `PP_TEST_SHARE=<dir>`
/// makes everything under `<dir>` look like an SMB share mounted there
/// (unmounted once `<dir>` itself is gone).
fn test_share(path: &Path) -> Option<Share> {
    let dir = PathBuf::from(std::env::var_os("PP_TEST_SHARE")?);
    if !lexical(path).starts_with(&dir) {
        return None;
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Some(Share { kind: "SMB", source: format!("//test-nas/{name}"), mounted: dir.exists(), mount_point: dir })
}

// ── probing ─────────────────────────────────────────────────────────────────

/// Helper threads still inside a filesystem call.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Beyond this many, a mount is plainly stuck: don't park more threads on it.
const MAX_IN_FLIGHT: usize = 4;

/// Run `f` on a helper thread and wait at most `timeout` for it.  `None` if
/// it didn't finish (the helper is left to finish on its own).
pub fn with_timeout<T: Send + 'static>(timeout: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    if IN_FLIGHT.load(Ordering::Relaxed) >= MAX_IN_FLIGHT {
        return None;
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("netfs-probe".into()).spawn(move || {
        let v = f();
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        let _ = tx.send(v);
    });
    if spawned.is_err() {
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        return None;
    }
    rx.recv_timeout(timeout).ok()
}

/// Where `path` lives and whether it answers.  Blocks for up to `timeout`.
pub fn probe(path: &Path, timeout: Duration) -> Probe {
    let (mi, fstab) = read_tables();
    let override_share = test_share(path);
    let tables = || override_share.clone().or_else(|| share_from_tables(&mi, &fstab, path));
    let p = path.to_path_buf();
    let answer = with_timeout(timeout, move || -> io::Result<(u32, PathBuf)> {
        let st = rustix::fs::statfs(&p).map_err(io::Error::from)?;
        // Opening it as the scan does catches what statfs doesn't
        // (permission denied, not a folder).
        std::fs::read_dir(&p)?;
        // Through any symlinks, to name the share the folder is really on.
        let real = std::fs::canonicalize(&p).unwrap_or(p);
        Ok((st.f_type as u32, real))
    });
    match answer {
        None => Probe { share: tables(), reach: Reach::NotResponding },
        Some(Ok((magic, real))) => {
            let share = override_share.clone().or_else(|| match classify_magic(magic) {
                Magic::Local => None,
                Magic::Fuse => share_from_tables(&mi, &fstab, &real),
                Magic::Network(kind) => Some(share_from_tables(&mi, &fstab, &real).unwrap_or(Share {
                    kind,
                    source: String::new(),
                    mount_point: real.clone(),
                    mounted: true,
                })),
            });
            match &override_share {
                // The test share "unmounted": as if the mount point were empty.
                Some(s) if !s.mounted => Probe { share, reach: Reach::Unreachable("the share isn't mounted".into()) },
                _ => Probe { share, reach: Reach::Ok },
            }
        }
        Some(Err(e)) => {
            let share = tables();
            let reach = match classify_error(&e) {
                Failure::Missing => match &share {
                    Some(s) if !s.mounted => Reach::Unreachable("the share isn't mounted".into()),
                    _ => Reach::Missing,
                },
                Failure::Unreachable => {
                    Reach::Unreachable(describe_errno(e.raw_os_error()).map(str::to_string).unwrap_or_else(|| e.to_string()))
                }
                Failure::Other => Reach::Failed(e.to_string()),
            };
            Probe { share, reach }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_types_classify() {
        assert_eq!(classify_magic(0xFF53_4D42), Magic::Network("SMB"));
        assert_eq!(classify_magic(0xFE53_4D42), Magic::Network("SMB"));
        assert_eq!(classify_magic(0x517B), Magic::Network("SMB"));
        assert_eq!(classify_magic(0x6969), Magic::Network("NFS"));
        assert_eq!(classify_magic(FUSE_MAGIC), Magic::Fuse);
        // ext4, btrfs, xfs, tmpfs
        for local in [0xEF53, 0x9123_683E, 0x5846_5342, 0x0102_1994] {
            assert_eq!(classify_magic(local), Magic::Local, "{local:#x}");
        }
        // A negative c_long from 32-bit statfs still matches after `as u32`.
        assert_eq!(classify_magic(-11_317_950i64 as u32), Magic::Network("SMB"));

        assert_eq!(network_fstype("cifs"), Some("SMB"));
        assert_eq!(network_fstype("smb3"), Some("SMB"));
        assert_eq!(network_fstype("nfs4"), Some("NFS"));
        assert_eq!(network_fstype("fuse.sshfs"), Some("SSHFS"));
        assert_eq!(network_fstype("fuse.rclone"), Some("rclone"));
        for local in ["ext4", "btrfs", "tmpfs", "autofs", "fuse.ntfs-3g", "fuse.gocryptfs", "fuseblk", "vfat", "overlay"] {
            assert_eq!(network_fstype(local), None, "{local}");
        }
    }

    #[test]
    fn errors_classify() {
        let e = |n| io::Error::from_raw_os_error(n);
        for n in [libc::ENOTCONN, libc::EHOSTDOWN, libc::EHOSTUNREACH, libc::ETIMEDOUT, libc::EIO, libc::ESTALE, libc::ENETUNREACH] {
            assert_eq!(classify_error(&e(n)), Failure::Unreachable, "errno {n}");
            assert!(describe_errno(Some(n)).is_some(), "errno {n}");
        }
        assert_eq!(classify_error(&e(libc::ENOENT)), Failure::Missing);
        assert_eq!(classify_error(&e(libc::ENOTDIR)), Failure::Missing);
        for n in [libc::EACCES, libc::EPERM, libc::EROFS, libc::ENOSPC, libc::EEXIST] {
            assert_eq!(classify_error(&e(n)), Failure::Other, "errno {n}");
        }
        assert_eq!(classify_errno(None), Failure::Other);
        assert_eq!(classify_error(&io::Error::new(io::ErrorKind::TimedOut, "x")), Failure::Unreachable);
        assert_eq!(classify_error(&io::Error::new(io::ErrorKind::NotFound, "x")), Failure::Missing);
    }

    const MOUNTINFO: &str = "\
22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p3 rw,ssd
41 22 0:41 / /mnt/nas rw,relatime shared:22 - autofs systemd-1 rw,fd=80,pgrp=1,timeout=600,direct
745 41 0:90 / /mnt/nas rw,relatime shared:661 - cifs //192.0.2.10/nas rw,vers=3.0,soft
50 22 0:50 / /home/me/My\\040Server rw,nosuid shared:30 - fuse.sshfs me@host:/srv rw,user_id=1000
51 22 0:51 / /mnt/usb rw shared:31 - fuseblk /dev/sdb1 rw
";

    const FSTAB: &str = "\
# comment
UUID=84d7 /  btrfs defaults 0 0
//192.0.2.10/nas  /mnt/nas  cifs  credentials=/etc/x,_netdev,x-systemd.automount  0 0
nas:/export/photos /mnt/photos nfs defaults 0 0
";

    #[test]
    fn mount_tables_name_the_share() {
        let mi = parse_mountinfo(MOUNTINFO);
        let fstab = parse_fstab(FSTAB);
        assert_eq!(mi.len(), 5);
        assert_eq!(fstab.len(), 3);

        // Mounted: the cifs mount on top of its automount point wins.
        let s = share_from_tables(&mi, &fstab, Path::new("/mnt/nas/Photography/2026")).unwrap();
        assert_eq!((s.kind, s.source.as_str(), s.mounted), ("SMB", "//192.0.2.10/nas", true));
        assert_eq!(s.mount_point, PathBuf::from("/mnt/nas"));
        assert_eq!(s.name(), "//192.0.2.10/nas");

        // Escaped spaces, FUSE subtype.
        let s = share_from_tables(&mi, &fstab, Path::new("/home/me/My Server/pics")).unwrap();
        assert_eq!((s.kind, s.source.as_str()), ("SSHFS", "me@host:/srv"));

        // Local disks and local FUSE.
        assert_eq!(share_from_tables(&mi, &fstab, Path::new("/home/me/Pictures")), None);
        assert_eq!(share_from_tables(&mi, &fstab, Path::new("/mnt/usb/DCIM")), None);
        // A sibling whose name merely starts the same is not under the mount.
        assert_eq!(share_from_tables(&mi, &fstab, Path::new("/mnt/nasish")), None);

        // Automount idle (only autofs mounted): fstab says what it is.
        let idle: Vec<Mount> = mi.iter().filter(|m| m.fstype != "cifs").cloned().collect();
        let s = share_from_tables(&idle, &fstab, Path::new("/mnt/nas/Photography")).unwrap();
        assert_eq!((s.kind, s.mounted), ("SMB", false));
        // Not mounted at all: a plain folder on / where the share goes.
        let s = share_from_tables(&mi, &fstab, Path::new("/mnt/photos/2025")).unwrap();
        assert_eq!((s.kind, s.source.as_str(), s.mounted), ("NFS", "nas:/export/photos", false));
    }

    #[test]
    fn probe_local_folders() {
        let d = std::env::temp_dir().join(format!("pp-netfs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = probe(&d, Duration::from_secs(5));
        assert_eq!(p.reach, Reach::Ok);
        let p = probe(&d.join("not-there"), Duration::from_secs(5));
        assert_eq!(p.reach, Reach::Missing);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timeout_gives_up_on_a_stuck_call() {
        let t = std::time::Instant::now();
        let r = with_timeout(Duration::from_millis(50), || std::thread::sleep(Duration::from_millis(400)));
        assert!(r.is_none());
        assert!(t.elapsed() < Duration::from_millis(300));
        assert_eq!(with_timeout(Duration::from_secs(1), || 7), Some(7));
    }

    /// Read-only check against a real mount:
    /// `PP_PROBE_PATH=/mnt/share/folder cargo test --release -- --ignored --nocapture probe_real`
    #[test]
    #[ignore]
    fn probe_real() {
        let Some(p) = std::env::var_os("PP_PROBE_PATH") else { return };
        let t = std::time::Instant::now();
        let r = probe(Path::new(&p), Duration::from_secs(5));
        eprintln!("{r:?} in {:?}", t.elapsed());
    }
}
