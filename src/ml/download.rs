//! Downloading models (plan §4.4): on a worker thread, with progress as
//! [`BgEvent::ModelDownload`], each file to a temporary name beside its
//! destination, its SHA-256 checked against the registry, then renamed
//! into place.  A mismatch deletes the file and says so; nothing that
//! didn't match is ever used.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::models::{self, ModelId};
use crate::bus::{BgEvent, Bus};

/// Where a model's download stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DownloadState {
    /// Queued behind another model.
    Waiting,
    Downloading,
    /// Every byte is here; the digest is being computed.
    Checking,
    /// In place and verified.
    Checked,
    /// The digest differed: the file was deleted, nothing installed.
    Mismatch { expected: String, got: String },
    /// The publisher couldn't be reached (no network, DNS, a refused or
    /// timed-out connection) before any byte arrived.
    Offline,
    /// Anything else (an HTTP error, a full disk, a short read), in plain
    /// words.
    Failed(String),
    /// The user cancelled; partial files are removed.
    Cancelled,
}

impl DownloadState {
    /// Finished, one way or the other.
    pub fn is_final(&self) -> bool {
        !matches!(self, DownloadState::Waiting | DownloadState::Downloading | DownloadState::Checking)
    }
}

/// A running set of downloads; dropping it doesn't cancel them.
pub struct DownloadHandle {
    cancel: Arc<AtomicBool>,
}

impl DownloadHandle {
    /// Stop after the current chunk; partial files are removed and each
    /// model still pending reports [`DownloadState::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Download every model in `ids` that isn't installed, one after another,
/// reporting progress on `bus`.  Files already in place report `Checked`
/// at once.  Returns a handle to cancel with.
pub fn start(ids: Vec<ModelId>, bus: Bus) -> DownloadHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("model-download".into())
        .spawn(move || execute(&ids, &c, &mut |ev| drop(bus.unbounded_send(ev))))
        .expect("spawn model-download");
    DownloadHandle { cancel }
}

/// The downloads themselves, on the calling thread.  `emit` gets each
/// progress event.  Returns the ids that ended `Checked`.
pub fn execute(ids: &[ModelId], cancel: &AtomicBool, emit: &mut dyn FnMut(BgEvent)) -> Vec<ModelId> {
    let mut ok = Vec::new();
    for (i, &id) in ids.iter().enumerate() {
        let total = models::info(id).bytes();
        if i > 0 {
            emit(BgEvent::ModelDownload { model: id, done: 0, total, state: DownloadState::Waiting });
        }
        if cancel.load(Ordering::Relaxed) {
            emit(BgEvent::ModelDownload { model: id, done: 0, total, state: DownloadState::Cancelled });
            continue;
        }
        let state = if models::installed(id) {
            DownloadState::Checked
        } else {
            // Phase 1 fills this in: fetch, check, rename into place.
            DownloadState::Failed("downloading isn't built yet".into())
        };
        if state == DownloadState::Checked {
            ok.push(id);
        }
        emit(BgEvent::ModelDownload { model: id, done: if state == DownloadState::Checked { total } else { 0 }, total, state });
    }
    ok
}

/// The temporary name a file is downloaded under, beside its destination.
pub fn partial_path(dest: &std::path::Path) -> PathBuf {
    crate::ops::fsutil::tmp_sibling(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_states_are_final() {
        assert!(!DownloadState::Waiting.is_final());
        assert!(!DownloadState::Downloading.is_final());
        assert!(!DownloadState::Checking.is_final());
        assert!(DownloadState::Checked.is_final());
        assert!(DownloadState::Offline.is_final());
        assert!(DownloadState::Cancelled.is_final());
        assert!(DownloadState::Mismatch { expected: "a".into(), got: "b".into() }.is_final());
    }

    #[test]
    fn a_cancelled_set_reports_every_model_cancelled() {
        let cancel = AtomicBool::new(true);
        let mut got = Vec::new();
        let ok = execute(&[ModelId::Siglip2], &cancel, &mut |ev| got.push(ev));
        assert!(ok.is_empty());
        assert!(matches!(got.as_slice(), [BgEvent::ModelDownload { state: DownloadState::Cancelled, .. }]));
    }
}
