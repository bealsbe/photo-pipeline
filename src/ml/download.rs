//! Downloading models (plan §4.4): on a worker thread, with progress as
//! [`BgEvent::ModelDownload`], each file to a temporary name beside its
//! destination, its SHA-256 checked against the registry, then renamed
//! into place.  A mismatch deletes the file and says so; nothing that
//! didn't match is ever used.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::models::{self, ModelFile, ModelId};
use crate::bus::BgEvent;
use crate::ops::fsutil;

/// Progress is reported at most this often per file.
const REPORT_EVERY: Duration = Duration::from_millis(200);
const CHUNK: usize = 256 * 1024;

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
    /// A handle over a flag the caller's own thread watches.
    pub fn from_flag(cancel: Arc<AtomicBool>) -> DownloadHandle {
        DownloadHandle { cancel }
    }

    /// Stop after the current chunk; partial files are removed and each
    /// model still pending reports [`DownloadState::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Download every model in `ids` that isn't installed, one after another,
/// on the calling thread (the engine runs it on a thread of its own and
/// watches a [`DownloadHandle`]'s flag).  `emit` gets each progress event;
/// files already in place report `Checked` at once.  Returns the ids that
/// ended `Checked`.
pub fn execute(ids: &[ModelId], cancel: &AtomicBool, emit: &mut dyn FnMut(BgEvent)) -> Vec<ModelId> {
    let mut ok = Vec::new();
    for (i, &id) in ids.iter().enumerate() {
        let total = models::info(id).bytes();
        if i > 0 {
            emit(BgEvent::ModelDownload { model: id, done: 0, total, state: DownloadState::Waiting });
        }
        let state = download_model(id, cancel, &mut |done, state| emit(BgEvent::ModelDownload { model: id, done, total, state }));
        if state == DownloadState::Checked {
            ok.push(id);
        }
        emit(BgEvent::ModelDownload { model: id, done: if state == DownloadState::Checked { total } else { 0 }, total, state });
    }
    ok
}

/// One model's files, in order; `report(done bytes, state)` as it goes.
fn download_model(id: ModelId, cancel: &AtomicBool, report: &mut dyn FnMut(u64, DownloadState)) -> DownloadState {
    let info = models::info(id);
    let Some(dir) = models::model_dir(id) else { return DownloadState::Failed("no home folder for the models".into()) };
    let mut done = 0u64;
    for f in info.files {
        if cancel.load(Ordering::Relaxed) {
            return DownloadState::Cancelled;
        }
        let dest = dir.join(f.file);
        if std::fs::metadata(&dest).is_ok_and(|m| m.len() == f.bytes) {
            done += f.bytes;
            report(done, DownloadState::Downloading);
            continue;
        }
        let base = done;
        let state = fetch(f, &dest, cancel, &mut |step| match step {
            Step::Bytes(n) => report(base + n, DownloadState::Downloading),
            Step::Checking => report(base + f.bytes, DownloadState::Checking),
        });
        if state != DownloadState::Checked {
            return state;
        }
        done += f.bytes;
    }
    DownloadState::Checked
}

/// How far one file's fetch has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Bytes received so far.
    Bytes(u64),
    /// Every byte is here; the digest is being checked.
    Checking,
}

/// Download one file to a temporary name beside `dest`, check its digest,
/// and rename it into place.
pub fn fetch(f: &ModelFile, dest: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(Step)) -> DownloadState {
    if let Err(e) = fsutil::ensure_parent(dest) {
        return DownloadState::Failed(format!("can't make the models folder: {e}"));
    }
    let tmp = partial_path(dest);
    let _ = std::fs::remove_file(&tmp);
    let outcome = (|| -> Result<DownloadState, DownloadState> {
        let response = ureq::get(f.url).call().map_err(|e| classify(&e))?;
        if response.status() != 200 {
            return Err(DownloadState::Failed(format!("the publisher answered HTTP {}", response.status())));
        }
        let mut body = response.into_body();
        let mut reader = body.as_reader();
        let mut file = std::fs::File::create(&tmp).map_err(|e| DownloadState::Failed(format!("can't write {}: {e}", tmp.display())))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut got = 0u64;
        let mut last = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadState::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| if got == 0 { DownloadState::Offline } else { DownloadState::Failed(format!("the download broke off: {e}")) })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| DownloadState::Failed(format!("can't write {}: {e}", tmp.display())))?;
            got += n as u64;
            if last.elapsed() >= REPORT_EVERY {
                report(Step::Bytes(got));
                last = Instant::now();
            }
        }
        report(Step::Bytes(got));
        report(Step::Checking);
        file.sync_all().map_err(|e| DownloadState::Failed(format!("can't write {}: {e}", tmp.display())))?;
        drop(file);
        if got != f.bytes {
            return Err(DownloadState::Failed(format!("{} arrived with {got} bytes, not {}", f.file, f.bytes)));
        }
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if digest != f.sha256 {
            return Err(DownloadState::Mismatch { expected: f.sha256.to_string(), got: digest });
        }
        let _ = std::fs::remove_file(dest);
        fsutil::rename_noreplace(&tmp, dest).map_err(|e| DownloadState::Failed(format!("can't put {} in place: {e}", f.file)))?;
        fsutil::sync_parent(dest);
        Ok(DownloadState::Checked)
    })();
    match outcome {
        Ok(state) => state,
        Err(state) => {
            let _ = std::fs::remove_file(&tmp);
            state
        }
    }
}

/// What a request error means for the dialog.
fn classify(e: &ureq::Error) -> DownloadState {
    match e {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) | ureq::Error::Io(_) => DownloadState::Offline,
        ureq::Error::StatusCode(code) => DownloadState::Failed(format!("the publisher answered HTTP {code}")),
        other => DownloadState::Failed(other.to_string()),
    }
}

/// The temporary name a file is downloaded under, beside its destination.
pub fn partial_path(dest: &Path) -> PathBuf {
    fsutil::tmp_sibling(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

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

    /// Serve `body` once over plain HTTP on a local port; returns the URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        });
        format!("http://127.0.0.1:{port}/file.bin")
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn a_fetched_file_is_checked_before_it_is_put_in_place() {
        let d = std::env::temp_dir().join(format!("pp-download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 241) as u8).collect();
        let digest: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
        let good = ModelFile { file: "good.bin", url: leak(serve_once(body.clone())), sha256: leak(digest.clone()), bytes: body.len() as u64, archive: None };
        let mut seen = Vec::new();
        let state = fetch(&good, &d.join("good.bin"), &AtomicBool::new(false), &mut |s| seen.push(s));
        assert_eq!(state, DownloadState::Checked);
        assert_eq!(std::fs::read(d.join("good.bin")).unwrap(), body);
        assert_eq!(seen.last(), Some(&Step::Checking));
        assert!(seen.contains(&Step::Bytes(body.len() as u64)));
        assert!(!partial_path(&d.join("good.bin")).exists());
        // The wrong digest: deleted, nothing installed.
        let bad = ModelFile { file: "bad.bin", url: leak(serve_once(body.clone())), sha256: leak("0".repeat(64)), bytes: body.len() as u64, archive: None };
        let state = fetch(&bad, &d.join("bad.bin"), &AtomicBool::new(false), &mut |_| {});
        assert!(matches!(state, DownloadState::Mismatch { ref expected, ref got } if expected == &"0".repeat(64) && got == &digest), "{state:?}");
        assert!(!d.join("bad.bin").exists() && !partial_path(&d.join("bad.bin")).exists());
        // The wrong size.
        let short = ModelFile { file: "short.bin", url: leak(serve_once(body.clone())), sha256: leak(digest.clone()), bytes: 10, archive: None };
        assert!(matches!(fetch(&short, &d.join("short.bin"), &AtomicBool::new(false), &mut |_| {}), DownloadState::Failed(_)));
        // Nobody listening: offline.
        let gone = ModelFile { file: "gone.bin", url: "http://127.0.0.1:9/file.bin", sha256: leak(digest), bytes: 1, archive: None };
        assert_eq!(fetch(&gone, &d.join("gone.bin"), &AtomicBool::new(false), &mut |_| {}), DownloadState::Offline);
        let _ = std::fs::remove_dir_all(&d);
    }
}
