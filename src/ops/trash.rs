//! Prune → system Trash (reversible).
//!
//! Files are trashed one at a time so a failure on one doesn't stop the rest.
//! The `trash` crate follows the freedesktop Trash spec on Linux (home trash,
//! or `$topdir/.Trash-$uid` on other volumes) and returns an error when no
//! trash is available — it never falls back to deleting permanently.

use std::path::PathBuf;

pub struct TrashResult {
    pub succeeded: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    /// Paths in `succeeded` that were already gone: nothing went to the Trash.
    pub missing: Vec<PathBuf>,
    /// When the operation started and finished (unix seconds, rounded
    /// outwards), so an undo can find these files in the Trash again.
    pub started: i64,
    pub finished: i64,
}

fn unix_now() -> std::time::Duration {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
}

pub fn trash_files(paths: &[PathBuf]) -> TrashResult {
    let started = unix_now().as_secs() as i64;
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    let mut missing = Vec::new();
    for p in paths {
        if !p.exists() {
            // Already gone — treat as success so it leaves the model.
            succeeded.push(p.clone());
            missing.push(p.clone());
            continue;
        }
        match trash::delete(p) {
            Ok(()) => succeeded.push(p.clone()),
            Err(e) => {
                let msg = match &e {
                    trash::Error::FileSystem { source, .. }
                        if source.kind() == std::io::ErrorKind::PermissionDenied =>
                    {
                        format!("Permission denied: {e}")
                    }
                    _ => e.to_string(),
                };
                failed.push((p.clone(), msg));
            }
        }
    }
    let end = unix_now();
    let finished = end.as_secs() as i64 + i64::from(end.subsec_nanos() > 0);
    TrashResult { succeeded, failed, missing, started, finished }
}

/// True if any path is on a different device than the home directory —
/// network or removable drives, where a Trash may not exist.
pub fn has_remote_files(paths: &[PathBuf]) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Some(home) = dirs::home_dir() else { return false };
        let Ok(home_dev) = std::fs::metadata(home).map(|m| m.dev()) else { return false };
        paths
            .iter()
            .any(|p| std::fs::metadata(p).map(|m| m.dev() != home_dev).unwrap_or(false))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        false
    }
}
