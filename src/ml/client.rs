//! The app's side of the helper process (plan §4.3): start it, send it
//! work over its pipes, turn its replies into results, and apply the
//! restart rule.  Everything that runs a model goes through the
//! [`Embedder`] trait, so the search pass is tested with a fake and the
//! device rule (decision 3) is checked without a card.
//!
//! **Restart rule.**  If the helper stops while running on the card, it
//! is started once more on the card; a second stop counts as the card
//! failing ([`CardFailure::Crashed`]).  On the processor it is restarted
//! and the caller is told ([`HelperError::Restarted`]) so the status bar
//! can say so.  A helper that left on purpose after idling is started
//! again quietly.  Once the card has failed, image work is refused until
//! a new client is made (*Try the card again*); queries still work,
//! through a text-only helper that never touches the card.  The client
//! never starts a processor helper for image work unless the processor
//! is the chosen device.  Face work (phase 3) is image work: it follows
//! the same path and the same rule.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::NaiveDateTime;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::heads::{HeadsResult, KnownHeads};
use super::models::ModelId;
use super::worker::{self, Reply, Request};
use super::{CardFailure, Device, SelfCheck};
use crate::model::record::{FileType, ShotMeta};
use crate::search::index::Key;

/// How long to wait for the helper to load its models (the text tower
/// is 565 MB; a cold disk is slow).
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one request may take (a batch of 32 over a sleepy share).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Successful requests after which an earlier crash is forgotten.
const FORGIVE_AFTER: u32 = 50;

/// A face as the helper found it in a photo (phase 3): its place on the
/// oriented preview, what the detector thought of it, how clear it is,
/// the recognition model's vector and a small tile for the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceFound {
    /// `[x, y, width, height]` as fractions of the oriented preview.
    pub bbox: [f32; 4],
    /// Five landmarks as fractions of the preview (right eye, left eye,
    /// nose, right and left mouth corner).
    pub landmarks: [[f32; 2]; 5],
    /// YuNet's score (at least the quality gate).
    pub score: f32,
    /// The longer box side in full-preview pixels.
    pub size_px: f32,
    /// Variance of the aligned crop's Laplacian.
    pub sharpness: f32,
    /// Unit length, the face model's width; empty if recognition failed.
    pub vector: Vec<f32>,
    /// A `worker::TILE_PX` square JPEG around the face.
    pub tile: Vec<u8>,
}

/// A face already found, for recognising it again with another model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceSpot {
    pub bbox: [f32; 4],
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
}

/// The known faces of one file in a batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownFaces {
    pub path: PathBuf,
    pub faces: Vec<FaceSpot>,
}

/// Faces the user drew in one file (the viewer's labels): boxes as
/// fractions of the oriented preview, with no landmarks yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawnFaces {
    pub path: PathBuf,
    pub boxes: Vec<[f32; 4]>,
}

/// What face work a batch asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "faces", rename_all = "snake_case")]
pub enum FaceAsk {
    /// No faces.
    #[default]
    None,
    /// Find the faces and recognise them.
    Detect,
    /// Recognise these faces again (a face-model change): no detection.
    Known { files: Vec<KnownFaces> },
    /// Faces the user drew: their landmarks looked for inside each box
    /// (the template fitted to it when none is found), then recognised.
    /// The box stays the user's.
    Drawn { files: Vec<DrawnFaces> },
}

/// What a batch of photos should yield.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Want {
    /// The search vector.
    pub search: bool,
    #[serde(flatten)]
    pub faces: FaceAsk,
}

impl Want {
    /// Search alone.
    pub fn search() -> Want {
        Want { search: true, faces: FaceAsk::None }
    }

    /// Search and faces in one read.
    pub fn with_faces() -> Want {
        Want { search: true, faces: FaceAsk::Detect }
    }

    /// Faces alone (the catch-up over photos already indexed).
    pub fn faces_only() -> Want {
        Want { search: false, faces: FaceAsk::Detect }
    }

    /// Known faces recognised again (a face-model change).
    pub fn known(known: Vec<KnownFaces>) -> Want {
        Want { search: false, faces: FaceAsk::Known { files: known } }
    }

    /// Faces the user drew, recognised.
    pub fn drawn(files: Vec<DrawnFaces>) -> Want {
        Want { search: false, faces: FaceAsk::Drawn { files } }
    }
}

/// What the helper found in one photo: its content key and capture info
/// (from the header it read for the preview), the model's vector and the
/// faces, when asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedding {
    pub key: Key,
    pub capture: Option<NaiveDateTime>,
    pub meta: ShotMeta,
    /// The frame looks accidental (a lens cap, a pocket, the floor): kept
    /// in the index but left out of results.
    pub accidental: bool,
    /// Unit-length, `dim` long; empty when the search vector wasn't asked
    /// for.
    pub vector: Vec<f32>,
    /// The faces found (or recognised again), when asked for.
    #[serde(default)]
    pub faces: Vec<FaceFound>,
}

/// Why a photo couldn't be embedded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileError {
    pub message: String,
    /// The OS error, when it was one (the pass tells a share that went
    /// away from a damaged file with `ops::netfs::classify_errno`).
    pub errno: Option<i32>,
}

impl FileError {
    pub fn io(e: &std::io::Error) -> FileError {
        FileError { message: e.to_string(), errno: e.raw_os_error() }
    }

    pub fn other(message: impl Into<String>) -> FileError {
        FileError { message: message.into(), errno: None }
    }
}

/// One photo's result from a batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageResult {
    pub path: PathBuf,
    pub result: Result<Embedding, FileError>,
}

/// Why the helper couldn't answer.
#[derive(Debug, Clone, PartialEq)]
pub enum HelperError {
    /// The graphics card failed (decision 3): the caller stops its work.
    Card(CardFailure),
    /// The helper stopped on the processor and was started again; the
    /// request should be sent again.
    Restarted,
    /// A protocol or I/O error with the helper, in plain words.
    Broken(String),
}

/// Something that runs the models: the real helper, or a test double.
/// Shared between the pass and the engine's queries, so the methods take
/// `&self`.
pub trait Embedder: Send + Sync {
    /// The adapter's name and the self-check, once known.
    fn device_report(&self) -> (Option<String>, SelfCheck);
    /// Embed a batch of photos (a pair's JPG, else the file): the search
    /// vector and the faces `want` asks for.
    fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<ImageResult>, HelperError>;
    /// Encode query texts (always on the processor, whatever the device).
    fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError>;
    /// Find fursuit heads in a batch of photos (phase 4): image work on
    /// the chosen device, under the same card rule as [`embed_images`].
    /// A helper started without the head models answers with an error.
    ///
    /// [`embed_images`]: Embedder::embed_images
    fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<HeadsResult>, HelperError> {
        let _ = files;
        Err(HelperError::Broken("this helper has no head step".into()))
    }
    /// The search model's vectors of heads already found (fursuit
    /// matching v2's catch-up): image work on the chosen device, under
    /// the same card rule; one result per photo, in order.  Any image
    /// helper serves it (the head detector isn't needed).
    fn embed_heads(&self, files: &[KnownHeads]) -> Result<Vec<HeadsResult>, HelperError> {
        let _ = files;
        Err(HelperError::Broken("this helper can't embed heads".into()))
    }
    /// Stop the helper (the app is quitting, or the device changed).
    fn shutdown(&self) {}
}

/// Why a helper couldn't be started.
#[derive(Debug, Clone, PartialEq)]
pub enum StartError {
    /// The card was chosen and can't be used.
    Card(CardFailure),
    /// The models aren't in place, the binary couldn't be run, …
    Other(String),
}

/// What kind of helper to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The device for image work.
    pub device: Device,
    /// The text tower alone, on the processor, whatever the device
    /// (queries after a card failure).
    pub text_only: bool,
    /// Load the face detector and this recognition model too.
    pub face_model: Option<ModelId>,
    /// Load the fursuit head detector too (OWLv2; phase 4).  The heads'
    /// vectors come from the search model's image tower (v2).
    pub heads: bool,
}

impl Spec {
    pub fn images(device: Device, face_model: Option<ModelId>) -> Spec {
        Spec { device, text_only: false, face_model, heads: false }
    }

    /// The same helper with the head models loaded (or not).
    pub fn with_heads(self, heads: bool) -> Spec {
        Spec { heads: heads && !self.text_only, ..self }
    }

    pub fn text_only() -> Spec {
        Spec { device: Device::Cpu, text_only: true, face_model: None, heads: false }
    }
}

/// Makes an [`Embedder`]: the real helper in the app, a double in tests.
/// The engine asks for image work on the chosen device only.
pub type EmbedderFactory = Box<dyn Fn(Spec) -> Result<Arc<dyn Embedder>, StartError> + Send>;

/// The binary to start as the helper: the app itself.  `PP_HELPER_EXE`
/// overrides it; a test binary (in `target/<profile>/deps/`) uses the app
/// binary beside its folder, since a test harness doesn't know the flag.
fn helper_exe() -> std::io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PP_HELPER_EXE") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    if exe.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "deps") {
        if let Some(app) = exe.parent().and_then(Path::parent).map(|d| d.join("photo-pipeline")) {
            if app.is_file() {
                return Ok(app);
            }
        }
    }
    Ok(exe)
}

/// A running helper process.
struct Process {
    child: Child,
    stdin: std::process::ChildStdin,
    /// Replies waited for, by request id.
    pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>>,
    /// Set by the reader thread when the helper's stdout closes: its
    /// farewell (`Idle`) or none (a crash).
    ended: Arc<Mutex<Option<Option<Reply>>>>,
    text_only: bool,
}

impl Process {
    /// Start `photo-pipeline --ml-worker` and wait for its first reply.
    fn spawn(spec: Spec) -> Result<(Process, Reply), StartError> {
        let exe = helper_exe().map_err(|e| StartError::Other(format!("can't find the app's own binary: {e}")))?;
        let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
        let mut cmd = Command::new(&exe);
        cmd.arg(worker::FLAG).arg("--device").arg(if spec.device == Device::Gpu { "gpu" } else { "cpu" });
        if spec.text_only {
            cmd.arg("--text-only");
        }
        if let Some(m) = spec.face_model.filter(|_| !spec.text_only) {
            let name = serde_json::to_value(m).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            cmd.arg("--face-model").arg(name);
        }
        if spec.heads && !spec.text_only {
            cmd.arg("--heads");
        }
        if let Some(models) = std::env::var_os("PP_MODEL_DIR") {
            cmd.arg("--models").arg(models);
        }
        if let Some(root) = std::env::var_os("PP_MODEL_ROOT") {
            cmd.arg("--models-root").arg(root);
        }
        // libwebgpu_dawn.so sits beside the binary (ort's copy-dylibs); a
        // test binary lives one level down, in deps/.
        let mut ld: Vec<PathBuf> = vec![dir.clone()];
        if let Some(up) = dir.parent() {
            ld.push(up.to_path_buf());
        }
        if let Some(old) = std::env::var_os("LD_LIBRARY_PATH") {
            ld.extend(std::env::split_paths(&old));
        }
        if let Ok(joined) = std::env::join_paths(ld) {
            cmd.env("LD_LIBRARY_PATH", joined);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| StartError::Other(format!("can't start the search helper: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>> = Arc::new(Mutex::new(HashMap::new()));
        let ended: Arc<Mutex<Option<Option<Reply>>>> = Arc::new(Mutex::new(None));
        let (first_tx, first_rx) = crossbeam_channel::bounded::<Reply>(1);
        {
            let (pending, ended) = (pending.clone(), ended.clone());
            std::thread::Builder::new()
                .name("ml-client-reader".into())
                .spawn(move || {
                    let mut first = Some(first_tx);
                    let mut farewell = None;
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let Ok(reply) = serde_json::from_str::<Reply>(&line) else {
                            eprintln!("search helper: unreadable reply: {}", line.chars().take(120).collect::<String>());
                            continue;
                        };
                        if let Some(tx) = first.take() {
                            let _ = tx.send(reply);
                            continue;
                        }
                        match &reply {
                            Reply::Images { id, .. } | Reply::Texts { id, .. } | Reply::Heads { id, .. } | Reply::Error { id, .. } => {
                                if let Some(tx) = pending.lock().remove(id) {
                                    let _ = tx.send(reply);
                                }
                            }
                            Reply::Idle => farewell = Some(reply),
                            _ => {}
                        }
                    }
                    *ended.lock() = Some(farewell);
                    // Anyone still waiting learns the helper is gone.
                    pending.lock().clear();
                })
                .expect("spawn ml-client-reader");
        }
        let proc = Process { child, stdin, pending, ended, text_only: spec.text_only };
        match first_rx.recv_timeout(START_TIMEOUT) {
            Ok(reply) => Ok((proc, reply)),
            Err(_) => {
                let mut proc = proc;
                let _ = proc.child.kill();
                Err(StartError::Other("the search helper didn't start in time".into()))
            }
        }
    }

    fn alive(&self) -> bool {
        self.ended.lock().is_none()
    }

    /// Left on purpose after idling (not a crash).
    fn idled(&self) -> bool {
        matches!(&*self.ended.lock(), Some(Some(Reply::Idle)))
    }

    fn send(&mut self, req: &Request) -> Result<Receiver<Reply>, HelperError> {
        let id = match req {
            Request::EmbedImages { id, .. } | Request::EncodeText { id, .. } | Request::FindHeads { id, .. } | Request::EmbedHeads { id, .. } => *id,
            Request::Quit => 0,
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.pending.lock().insert(id, tx);
        let line = serde_json::to_string(req).map_err(|e| HelperError::Broken(e.to_string()))?;
        writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush()).map_err(|e| {
            self.pending.lock().remove(&id);
            HelperError::Broken(format!("the search helper stopped taking requests: {e}"))
        })?;
        Ok(rx)
    }

    fn stop(&mut self) {
        let _ = writeln!(self.stdin, "{}", serde_json::to_string(&Request::Quit).unwrap_or_default());
        let _ = self.stdin.flush();
        std::thread::sleep(Duration::from_millis(50));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Inner {
    proc: Option<Process>,
    /// The card failed: image work is refused; text goes to a text-only
    /// helper.
    card_failed: Option<CardFailure>,
    crashes: u32,
    since_crash: u32,
    name: Option<String>,
    self_check: SelfCheck,
}

/// The helper process as an [`Embedder`].
pub struct Client {
    spec: Spec,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
}

impl Client {
    /// Start a helper for image work on `device` (with the face models
    /// when `face_model` is given) and wait for it to load the models
    /// and, on the card, pass the self-check.  `PP_ML_FAKE_CARD` makes
    /// the card fail for tests ([`super::fake_card_failure`]).
    pub fn start(spec: Spec) -> Result<Client, StartError> {
        if spec.device == Device::Gpu {
            if let Some(f) = super::fake_card_failure() {
                return Err(StartError::Card(f));
            }
        }
        Client::spawn(Spec { text_only: false, ..spec })
    }

    /// A helper for queries only: the text tower on the processor, no
    /// image work, nothing on the card.
    pub fn start_text_only() -> Result<Client, StartError> {
        Client::spawn(Spec::text_only())
    }

    fn spawn(spec: Spec) -> Result<Client, StartError> {
        let (proc, reply) = Process::spawn(spec)?;
        match reply {
            Reply::Ready { name, self_check, .. } => Ok(Client {
                spec,
                inner: Mutex::new(Inner {
                    proc: Some(proc),
                    card_failed: if spec.text_only { Some(CardFailure::Failed("this helper runs queries only".into())) } else { None },
                    crashes: 0,
                    since_crash: 0,
                    name,
                    self_check,
                }),
                next_id: AtomicU64::new(1),
            }),
            Reply::CardFailed { failure } => Err(StartError::Card(failure)),
            Reply::Failed { message } => Err(StartError::Other(message)),
            other => Err(StartError::Other(format!("unexpected first reply from the search helper: {other:?}"))),
        }
    }

    /// The factory the app uses.
    pub fn factory() -> EmbedderFactory {
        Box::new(|spec| {
            let c = if spec.text_only { Client::start_text_only() } else { Client::start(spec) };
            c.map(|c| Arc::new(c) as Arc<dyn Embedder>)
        })
    }

    /// Make sure a helper is running for `req`'s kind of work, applying
    /// the restart rule, and send it.  Returns the reply channel.
    fn dispatch(&self, req: Request) -> Result<Receiver<Reply>, HelperError> {
        let images = matches!(req, Request::EmbedImages { .. } | Request::FindHeads { .. } | Request::EmbedHeads { .. });
        let mut inner = self.inner.lock();
        if images {
            if let Some(f) = &inner.card_failed {
                return Err(HelperError::Card(f.clone()));
            }
        }
        let mut restarted_cpu = false;
        let need_start = match &inner.proc {
            Some(p) if p.alive() && !(images && p.text_only) => false,
            Some(p) => {
                let idled = p.idled();
                let text_only = p.text_only;
                if !idled && !text_only {
                    // A crash.
                    match self.spec.device {
                        Device::Gpu => {
                            inner.crashes += 1;
                            inner.since_crash = 0;
                            if inner.crashes > 1 {
                                inner.card_failed = Some(CardFailure::Crashed);
                                inner.proc = None;
                                if images {
                                    return Err(HelperError::Card(CardFailure::Crashed));
                                }
                            }
                        }
                        Device::Cpu => restarted_cpu = true,
                    }
                }
                true
            }
            None => true,
        };
        if need_start {
            inner.proc = None;
            let text_only = inner.card_failed.is_some() || (!images && self.spec.device == Device::Gpu && inner.crashes > 1);
            let spec = if text_only { Spec::text_only() } else { self.spec };
            match Process::spawn(spec) {
                Ok((proc, Reply::Ready { name, self_check, .. })) => {
                    if !text_only {
                        inner.name = name;
                        inner.self_check = self_check;
                    }
                    inner.proc = Some(proc);
                }
                Ok((_, Reply::CardFailed { failure: f })) => {
                    inner.card_failed = Some(f.clone());
                    return Err(HelperError::Card(f));
                }
                Ok((_, Reply::Failed { message })) => return Err(HelperError::Broken(message)),
                Ok((_, other)) => return Err(HelperError::Broken(format!("unexpected reply {other:?}"))),
                Err(StartError::Card(f)) => {
                    inner.card_failed = Some(f.clone());
                    return Err(HelperError::Card(f));
                }
                Err(StartError::Other(why)) => return Err(HelperError::Broken(why)),
            }
            if restarted_cpu && images {
                return Err(HelperError::Restarted);
            }
        }
        inner.proc.as_mut().expect("a helper").send(&req)
    }

    fn wait(&self, rx: Receiver<Reply>) -> Result<Reply, HelperError> {
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(reply) => {
                let mut inner = self.inner.lock();
                inner.since_crash += 1;
                if inner.since_crash >= FORGIVE_AFTER {
                    inner.crashes = 0;
                }
                Ok(reply)
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(HelperError::Broken("the search helper didn't answer in time".into())),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                // The helper went away mid-request: the next dispatch
                // applies the restart rule.
                let inner = self.inner.lock();
                let idled = inner.proc.as_ref().is_some_and(Process::idled);
                drop(inner);
                if idled { Err(HelperError::Restarted) } else { self.after_crash() }
            }
        }
    }

    /// What a crash mid-request means for the caller.
    fn after_crash(&self) -> Result<Reply, HelperError> {
        let mut inner = self.inner.lock();
        match self.spec.device {
            Device::Gpu => {
                inner.crashes += 1;
                inner.since_crash = 0;
                if inner.crashes > 1 {
                    inner.card_failed = Some(CardFailure::Crashed);
                    inner.proc = None;
                    return Err(HelperError::Card(CardFailure::Crashed));
                }
                // Started once more on the card by the next dispatch; the
                // caller repeats the request.
                inner.proc = None;
                Err(HelperError::Restarted)
            }
            Device::Cpu => {
                inner.proc = None;
                Err(HelperError::Restarted)
            }
        }
    }
}

impl Embedder for Client {
    fn device_report(&self) -> (Option<String>, SelfCheck) {
        let inner = self.inner.lock();
        (inner.name.clone(), inner.self_check.clone())
    }

    fn embed_images(&self, files: &[(PathBuf, FileType)], want: &Want) -> Result<Vec<ImageResult>, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.dispatch(Request::EmbedImages { id, files: files.to_vec(), want: want.clone() })?;
        match self.wait(rx)? {
            Reply::Images { results, .. } => Ok(results),
            Reply::Error { message, .. } => Err(HelperError::Broken(message)),
            other => Err(HelperError::Broken(format!("unexpected reply {other:?}"))),
        }
    }

    fn encode_text(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.dispatch(Request::EncodeText { id, texts: texts.to_vec() })?;
        match self.wait(rx)? {
            Reply::Texts { vectors, .. } => Ok(vectors),
            Reply::Error { message, .. } => Err(HelperError::Broken(message)),
            other => Err(HelperError::Broken(format!("unexpected reply {other:?}"))),
        }
    }

    fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<HeadsResult>, HelperError> {
        if !self.spec.heads {
            return Err(HelperError::Broken("this helper was started without the head models".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.dispatch(Request::FindHeads { id, files: files.to_vec() })?;
        match self.wait(rx)? {
            Reply::Heads { results, .. } => Ok(results),
            Reply::Error { message, .. } => Err(HelperError::Broken(message)),
            other => Err(HelperError::Broken(format!("unexpected reply {other:?}"))),
        }
    }

    fn embed_heads(&self, files: &[KnownHeads]) -> Result<Vec<HeadsResult>, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.dispatch(Request::EmbedHeads { id, files: files.to_vec() })?;
        match self.wait(rx)? {
            Reply::Heads { results, .. } => Ok(results),
            Reply::Error { message, .. } => Err(HelperError::Broken(message)),
            other => Err(HelperError::Broken(format!("unexpected reply {other:?}"))),
        }
    }

    fn shutdown(&self) {
        if let Some(mut p) = self.inner.lock().proc.take() {
            p.stop();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn a_fake_card_failure_stops_the_card_helper_before_it_starts() {
        temp_env("PP_ML_FAKE_CARD", "self-check", || {
            assert!(matches!(Client::start(Spec::images(Device::Gpu, None)), Err(StartError::Card(CardFailure::SelfCheckFailed { .. }))));
        });
        temp_env("PP_ML_FAKE_CARD", "no-card", || {
            assert_eq!(Client::start(Spec::images(Device::Gpu, Some(ModelId::AuraFace))).err(), Some(StartError::Card(CardFailure::NoCard)));
        });
    }

    #[test]
    fn a_spec_carries_the_head_models_only_for_image_work() {
        let s = Spec::images(Device::Gpu, Some(ModelId::AuraFace)).with_heads(true);
        assert!(s.heads && !s.text_only && s.face_model == Some(ModelId::AuraFace));
        assert!(!Spec::text_only().with_heads(true).heads, "queries only: no head models");
        assert!(!Spec::images(Device::Cpu, None).heads, "off unless asked");
    }

    #[test]
    fn wants_serialise_flat() {
        let w = Want::with_faces();
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"search":true,"faces":"detect"}"#);
        assert_eq!(serde_json::from_str::<Want>(&json).unwrap(), w);
        assert_eq!(serde_json::to_string(&Want::search()).unwrap(), r#"{"search":true,"faces":"none"}"#);
        let k = Want::known(vec![KnownFaces { path: "/x.JPG".into(), faces: vec![] }]);
        assert_eq!(serde_json::from_str::<Want>(&serde_json::to_string(&k).unwrap()).unwrap(), k);
        assert!(!Want::faces_only().search);
    }

    /// Run `f` with an environment variable set, restoring it after.
    pub fn temp_env(key: &str, value: &str, f: impl FnOnce()) {
        static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _g = LOCK.lock();
        let old = std::env::var_os(key);
        // SAFETY: tests that touch the environment take LOCK, so no other
        // thread reads it while it changes.
        unsafe { std::env::set_var(key, value) };
        f();
        match old {
            // SAFETY: as above.
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}
