//! The app's side of the close-call judge's helper (`--judge-worker`,
//! [`super::judge_worker`]): start it when a close call needs it, send it
//! pairs over its pipes, and notice when it leaves (idle, or a crash).
//! Everything goes through the [`Judge`] trait, so the engine is tested
//! with a stand-in ([`Fake`], `PP_JUDGE_FAKE`) and the card rule is
//! checked without a card.
//!
//! **Card only.**  The helper loads the model on the card or not at all;
//! its refusals come back as [`JudgeError::Card`], and so does a helper
//! that dies twice in a row.  Nothing here ever starts a helper on the
//! processor: there is no such helper.
//!
//! **Memory.**  The helper exits on its own after
//! [`super::judge::IDLE_SECS`] without a question, and the card's memory
//! goes back with the process; [`Judge::unload`] makes it go now (a pass
//! wants the card).  [`Judge::loaded`] says whether a helper holds it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use super::CardFailure;
use super::judge::{JudgeChoice, PairAsk, PairResult, Reply, Request, Verdict};

/// How long the helper may take to load (18 GB from a cold disk).
const START_TIMEOUT: Duration = Duration::from_secs(300);
/// How long one pair may take (both orders; about 2.3 s measured).
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the judge couldn't answer.
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeError {
    /// The card can't run the judge (no card, too little memory for every
    /// layer, a driver that keeps failing): it stops and says so.
    Card(CardFailure),
    /// Anything else (a model file missing or damaged, the binary not
    /// runnable), in plain words.
    Broken(String),
}

/// Something that judges close calls: the real helper or a stand-in.
pub trait Judge: Send + Sync {
    /// Judge pairs (loading the model first if it isn't): one result
    /// per pair, in order.
    fn judge(&self, pairs: &[PairAsk]) -> Result<Vec<PairResult>, JudgeError>;
    /// A helper holds the model on the card right now.
    fn loaded(&self) -> bool;
    /// Let the model go now (the card's memory with it).
    fn unload(&self);
}

/// Makes the judge for a choice: the real helper, or the stand-in when
/// `PP_JUDGE_FAKE` is set; tests pass their own.
pub type JudgeFactory = Box<dyn Fn(JudgeChoice) -> Arc<dyn Judge> + Send>;

/// The app's factory: `PP_JUDGE_FAKE=<mode>` makes every judge a
/// [`Fake`], else the real helper.
pub fn factory() -> JudgeFactory {
    Box::new(|choice| match Fake::from_env() {
        Some(f) => Arc::new(f) as Arc<dyn Judge>,
        None => Arc::new(Client::new(choice)) as Arc<dyn Judge>,
    })
}

// ---- the real helper ---------------------------------------------------

struct Process {
    child: Child,
    stdin: std::process::ChildStdin,
    pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>>,
    /// Set when its stdout closes: `Some(true)` after its farewell
    /// (idle), `Some(false)` a crash.
    ended: Arc<Mutex<Option<bool>>>,
}

impl Process {
    fn spawn(choice: JudgeChoice) -> Result<(Process, Reply), JudgeError> {
        let model = choice.model().ok_or_else(|| JudgeError::Broken("no judge is chosen".into()))?;
        let dir = super::models::model_dir(model).ok_or_else(|| JudgeError::Broken("no home folder for the models".into()))?;
        let exe = helper_exe().map_err(|e| JudgeError::Broken(format!("can't find the app's own binary: {e}")))?;
        let mut cmd = Command::new(&exe);
        cmd.arg(super::judge_worker::FLAG).arg("--model-dir").arg(std::env::var_os("PP_JUDGE_MODEL_DIR").map(PathBuf::from).unwrap_or(dir));
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| JudgeError::Broken(format!("can't start the close-call judge: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>> = Arc::new(Mutex::new(HashMap::new()));
        let ended: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let (first_tx, first_rx) = crossbeam_channel::bounded::<Reply>(1);
        {
            let (pending, ended) = (pending.clone(), ended.clone());
            std::thread::Builder::new()
                .name("judge-client-reader".into())
                .spawn(move || {
                    let mut first = Some(first_tx);
                    let mut idle = false;
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let Ok(reply) = serde_json::from_str::<Reply>(&line) else {
                            eprintln!("judge: unreadable reply: {}", line.chars().take(120).collect::<String>());
                            continue;
                        };
                        if let Some(tx) = first.take() {
                            let _ = tx.send(reply);
                            continue;
                        }
                        match &reply {
                            Reply::Judged { id, .. } | Reply::Error { id, .. } => {
                                if let Some(tx) = pending.lock().remove(id) {
                                    let _ = tx.send(reply);
                                }
                            }
                            Reply::Idle => idle = true,
                            _ => {}
                        }
                    }
                    *ended.lock() = Some(idle);
                    pending.lock().clear();
                })
                .expect("spawn judge-client-reader");
        }
        let proc = Process { child, stdin, pending, ended };
        match first_rx.recv_timeout(START_TIMEOUT) {
            Ok(r) => Ok((proc, r)),
            Err(_) => {
                let mut p = proc;
                let crashed = p.ended.lock().is_some();
                let _ = p.child.kill();
                let _ = p.child.wait();
                if crashed {
                    // It died while loading: the driver took it down.
                    Err(JudgeError::Card(CardFailure::Crashed))
                } else {
                    Err(JudgeError::Broken("the close-call judge didn't load in time".into()))
                }
            }
        }
    }

    fn alive(&self) -> bool {
        self.ended.lock().is_none()
    }

    fn stop(&mut self) {
        let _ = writeln!(self.stdin, "{}", serde_json::to_string(&Request::Quit).unwrap_or_default());
        let _ = self.stdin.flush();
        // The process exits and the driver frees its memory; wait for it
        // so "unloaded" is true when this returns.
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// The binary to start: the app itself (see `ml::client::helper_exe`).
fn helper_exe() -> std::io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PP_HELPER_EXE") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    if exe.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "deps") {
        if let Some(app) = exe.parent().and_then(std::path::Path::parent).map(|d| d.join("photo-pipeline")) {
            if app.is_file() {
                return Ok(app);
            }
        }
    }
    Ok(exe)
}

/// The judge helper as a [`Judge`].
pub struct Client {
    choice: JudgeChoice,
    proc: Mutex<Option<Process>>,
    /// Crashes in a row; the second is a card failure.
    crashes: AtomicUsize,
    next_id: AtomicU64,
}

impl Client {
    pub fn new(choice: JudgeChoice) -> Client {
        Client { choice, proc: Mutex::new(None), crashes: AtomicUsize::new(0), next_id: AtomicU64::new(1) }
    }

    /// A running helper, started (and loaded) if need be.
    fn ensure(&self, guard: &mut Option<Process>) -> Result<(), JudgeError> {
        if guard.as_ref().is_some_and(Process::alive) {
            return Ok(());
        }
        if let Some(old) = guard.take() {
            if *old.ended.lock() == Some(false) {
                // It died: twice in a row is the card failing.
                if self.crashes.fetch_add(1, Ordering::Relaxed) + 1 >= 2 {
                    return Err(JudgeError::Card(CardFailure::Crashed));
                }
            }
        }
        let started = std::time::Instant::now();
        match Process::spawn(self.choice)? {
            (p, Reply::Ready { device, layers, load_ms }) => {
                eprintln!("judge: {layers} layers on {device}, loaded in {load_ms} ms ({:.1} s with the start)", started.elapsed().as_secs_f64());
                *guard = Some(p);
                Ok(())
            }
            (_, Reply::CardFailed { failure }) => Err(JudgeError::Card(failure)),
            (_, Reply::Failed { message }) => Err(JudgeError::Broken(message)),
            (_, other) => Err(JudgeError::Broken(format!("unexpected first reply from the close-call judge: {other:?}"))),
        }
    }
}

impl Judge for Client {
    fn judge(&self, pairs: &[PairAsk]) -> Result<Vec<PairResult>, JudgeError> {
        for _attempt in 0..2 {
            let rx: Receiver<Reply> = {
                let mut guard = self.proc.lock();
                self.ensure(&mut guard)?;
                let p = guard.as_mut().expect("a helper");
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = crossbeam_channel::bounded(1);
                p.pending.lock().insert(id, tx);
                let line = serde_json::to_string(&Request::Judge { id, pairs: pairs.to_vec() }).map_err(|e| JudgeError::Broken(e.to_string()))?;
                if writeln!(p.stdin, "{line}").and_then(|_| p.stdin.flush()).is_err() {
                    // Gone between the check and the write: start again.
                    continue;
                }
                rx
            };
            match rx.recv_timeout(PAIR_TIMEOUT * pairs.len().max(1) as u32) {
                Ok(Reply::Judged { results, .. }) => {
                    self.crashes.store(0, Ordering::Relaxed);
                    return Ok(results);
                }
                Ok(Reply::Error { message, .. }) => return Err(JudgeError::Broken(message)),
                Ok(other) => return Err(JudgeError::Broken(format!("unexpected reply {other:?}"))),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => return Err(JudgeError::Broken("the close-call judge didn't answer in time".into())),
                // Gone mid-question (idle or a crash): the next round
                // applies the restart rule.
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => continue,
            }
        }
        let mut guard = self.proc.lock();
        self.ensure(&mut guard)?;
        Err(JudgeError::Broken("the close-call judge keeps stopping".into()))
    }

    fn loaded(&self) -> bool {
        self.proc.lock().as_ref().is_some_and(Process::alive)
    }

    fn unload(&self) {
        if let Some(mut p) = self.proc.lock().take() {
            p.stop();
        }
    }
}

// ---- the stand-in ------------------------------------------------------

/// How the stand-in answers (`PP_JUDGE_FAKE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeMode {
    /// Every pair the same suit.
    Yes,
    /// Every pair different.
    No,
    /// Yes, no, yes, … in the order asked.
    Alternate,
    /// Yes, a second and a half a pair, after a five-second "load".
    Slow,
    /// The card fails at the load.
    CardFail,
    /// The helper can't run (a missing file).
    Broken,
}

impl FakeMode {
    pub fn parse(s: &str) -> Option<FakeMode> {
        Some(match s {
            "yes" => FakeMode::Yes,
            "no" => FakeMode::No,
            "alternate" => FakeMode::Alternate,
            "slow" => FakeMode::Slow,
            "card-fail" => FakeMode::CardFail,
            "broken" => FakeMode::Broken,
            _ => return None,
        })
    }
}

/// A judge that never touches a card or a file: for the pages and the
/// engine's tests.
pub struct Fake {
    pub mode: FakeMode,
    loaded: AtomicBool,
    asked: AtomicUsize,
    /// Pairs judged, for tests.
    pub pairs: Mutex<Vec<PairAsk>>,
}

impl Fake {
    pub fn new(mode: FakeMode) -> Fake {
        Fake { mode, loaded: AtomicBool::new(false), asked: AtomicUsize::new(0), pairs: Mutex::new(Vec::new()) }
    }

    pub fn from_env() -> Option<Fake> {
        FakeMode::parse(&std::env::var("PP_JUDGE_FAKE").ok()?).map(Fake::new)
    }
}

impl Judge for Fake {
    fn judge(&self, pairs: &[PairAsk]) -> Result<Vec<PairResult>, JudgeError> {
        match self.mode {
            FakeMode::CardFail => return Err(JudgeError::Card(CardFailure::Failed("the stand-in judge's card failed (PP_JUDGE_FAKE=card-fail)".into()))),
            FakeMode::Broken => return Err(JudgeError::Broken("the stand-in judge can't run (PP_JUDGE_FAKE=broken)".into())),
            _ => {}
        }
        if !self.loaded.swap(true, Ordering::Relaxed) && self.mode == FakeMode::Slow {
            std::thread::sleep(Duration::from_secs(5));
        }
        self.pairs.lock().extend(pairs.iter().cloned());
        Ok(pairs
            .iter()
            .map(|_| {
                let n = self.asked.fetch_add(1, Ordering::Relaxed);
                let yes = match self.mode {
                    FakeMode::Yes | FakeMode::Slow => true,
                    FakeMode::No => false,
                    _ => n % 2 == 0,
                };
                if self.mode == FakeMode::Slow {
                    std::thread::sleep(Duration::from_millis(1500));
                }
                let p = if yes { 0.9 } else { 0.1 };
                PairResult { result: Ok(Verdict { p_ab: p, p_ba: p }) }
            })
            .collect())
    }

    fn loaded(&self) -> bool {
        self.loaded.load(Ordering::Relaxed)
    }

    fn unload(&self) {
        self.loaded.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::judge::Crop;

    fn pair() -> PairAsk {
        PairAsk { a: Crop::File(PathBuf::from("/x/1.jpg")), b: Crop::File(PathBuf::from("/x/2.jpg")) }
    }

    #[test]
    fn the_stand_in_answers_as_told_and_never_touches_a_card() {
        let f = Fake::new(FakeMode::Alternate);
        assert!(!f.loaded());
        let r = f.judge(&[pair(), pair(), pair()]).unwrap();
        let same: Vec<bool> = r.iter().map(|x| x.result.as_ref().unwrap().same()).collect();
        assert_eq!(same, [true, false, true]);
        assert!(f.loaded());
        f.unload();
        assert!(!f.loaded());
        assert!(matches!(Fake::new(FakeMode::CardFail).judge(&[pair()]), Err(JudgeError::Card(_))));
        assert!(matches!(Fake::new(FakeMode::Broken).judge(&[pair()]), Err(JudgeError::Broken(_))));
        assert!(Fake::new(FakeMode::No).judge(&[pair()]).unwrap().iter().all(|x| !x.result.as_ref().unwrap().same()));
        for m in ["yes", "no", "alternate", "slow", "card-fail", "broken"] {
            assert!(FakeMode::parse(m).is_some(), "{m}");
        }
        assert_eq!(FakeMode::parse("maybe"), None);
    }

    /// The real helper with its model missing says so plainly: nothing
    /// is loaded, nothing touches the card.  Needs the app binary beside
    /// the tests (`cargo build --release` first).
    #[test]
    #[ignore = "starts the app binary as the judge helper"]
    fn a_missing_model_is_reported_not_worked_around() {
        crate::ml::client::tests::temp_env("PP_JUDGE_MODEL_DIR", "/nonexistent", || {
            let c = Client::new(JudgeChoice::Gemma4);
            assert!(matches!(c.judge(&[pair()]), Err(JudgeError::Broken(m)) if m.contains("missing")));
            assert!(!c.loaded());
        });
    }
}
