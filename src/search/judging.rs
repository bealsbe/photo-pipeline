//! The close-call judge as the engine uses it (fursuit matching v2): the
//! verdicts kept per pair, the runner thread that asks the judge helper,
//! and the plan of one review's close calls.
//!
//! **Fursuits only, suggestions only.**  The judge sees two fursuit heads
//! at a time and says whether they are the same suit; what it says
//! reorders and filters what the review and *Find more* offer.  Nothing
//! is named, merged or marked by it: the user still answers every
//! suggestion.  It is never asked about a face or a pet.
//!
//! **The band.**  A candidate nearer to the subject than
//! [`SURE_YES`] (whitened distance) is offered at once; one beyond
//! [`SURE_NO`] isn't considered; one in between is a close call, judged
//! in the background, nearest first: offered when the judge says yes,
//! left out (and counted) when it says no.
//!
//! **Kept per pair.**  Verdicts are cache beside the index
//! (`judged-<model>.bin`, 0600), keyed by both heads (content key and box
//! in thousandths, in either order) and the judge's model, so asking
//! again is free and a refit or regroup loses nothing.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};

use super::index::{Key, blocks, framed};
use crate::ml::judge::{Crop, JudgeChoice, PairAsk, PairResult, Verdict};
use crate::ml::judge_client::{Judge, JudgeError, JudgeFactory};
use crate::ml::models;
use crate::model::binfile::{Reader, Writer};
use crate::model::subjects::{FaceRef, private_dir, private_file};

/// Under this whitened distance to the subject a candidate is offered
/// without asking the judge (the research's band, chosen on even days).
pub const SURE_YES: f32 = 0.639;
/// Beyond this a candidate is not considered at all.
pub const SURE_NO: f32 = 0.786;

const MAGIC: &[u8; 8] = b"PPJUDGD\0";
const VERSION: u32 = 1;

/// Two heads, in a fixed order (a pair asked either way round is one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairKey(pub FaceRef, pub FaceRef);

impl PairKey {
    pub fn new(a: FaceRef, b: FaceRef) -> PairKey {
        let ka = (a.key, a.thousandths());
        let kb = (b.key, b.thousandths());
        if ka <= kb { PairKey(a, b) } else { PairKey(b, a) }
    }
}

/// The verdicts of one judge model for one library.
pub struct PairCache {
    path: Option<PathBuf>,
    map: HashMap<PairKey, Verdict>,
}

impl PairCache {
    fn file(dir: &Path, choice: JudgeChoice) -> Option<PathBuf> {
        Some(dir.join(format!("judged-{}.bin", choice.model()?.dir_name())))
    }

    /// Every verdict file a judge may keep in an index folder.
    pub fn files(dir: &Path) -> Vec<PathBuf> {
        JudgeChoice::ALL.iter().filter_map(|&c| Self::file(dir, c)).collect()
    }

    /// Bytes the verdicts take in an index folder (Storage's `judged_bytes`).
    pub fn bytes_in(dir: &Path) -> u64 {
        Self::files(dir).iter().map(|p| fs::metadata(p).map_or(0, |m| m.len())).sum()
    }

    /// Remove every judge's verdicts from an index folder (Clear,
    /// Rebuild, forgetting the fursuits).
    pub fn forget_in(dir: &Path) {
        for p in Self::files(dir) {
            let _ = fs::remove_file(p);
        }
    }

    /// No folder: verdicts kept in memory only.
    pub fn none() -> PairCache {
        PairCache { path: None, map: HashMap::new() }
    }

    /// The verdicts `choice` gave in this index folder.
    pub fn load(dir: &Path, choice: JudgeChoice) -> PairCache {
        let Some(path) = Self::file(dir, choice) else { return Self::none() };
        let mut map = HashMap::new();
        if let Ok(data) = fs::read(&path) {
            let bs = blocks(&data);
            if bs.first().is_some_and(|h| read_header(h, choice)) {
                for b in &bs[1..] {
                    let Some(rows) = read_block(b) else { break };
                    map.extend(rows);
                }
            }
        }
        PairCache { path: Some(path), map }
    }

    pub fn get(&self, a: FaceRef, b: FaceRef) -> Option<Verdict> {
        self.map.get(&PairKey::new(a, b)).copied()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Keep verdicts (appended to the file, private).
    pub fn add(&mut self, rows: &[(PairKey, Verdict)], choice: JudgeChoice) -> io::Result<()> {
        for (k, v) in rows {
            self.map.insert(*k, *v);
        }
        let Some(path) = &self.path else { return Ok(()) };
        if rows.is_empty() {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            private_dir(dir);
        }
        let fresh = fs::metadata(path).map_or(true, |m| m.len() == 0);
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
        private_file(path);
        if fresh {
            f.write_all(&framed(&header(choice)))?;
        }
        f.write_all(&framed(&block(rows)))?;
        f.sync_data()
    }
}

fn header(choice: JudgeChoice) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    let (dir, sha) = match choice.model() {
        Some(m) => (m.dir_name(), models::info(m).weights_sha256()),
        None => ("", ""),
    };
    w.bytes(dir.as_bytes());
    w.bytes(sha.as_bytes());
    w.u32(crate::ml::judge::IMAGE_TOKENS as u32);
    w.seal()
}

fn read_header(data: &[u8], choice: JudgeChoice) -> bool {
    let Some(mut r) = Reader::open(data) else { return false };
    let Some(m) = choice.model() else { return false };
    r.take(8) == Some(MAGIC)
        && r.u32() == Some(VERSION)
        && r.bytes() == Some(m.dir_name().as_bytes())
        && r.bytes() == Some(models::info(m).weights_sha256().as_bytes())
        && r.u32() == Some(crate::ml::judge::IMAGE_TOKENS as u32)
        && r.done()
}

fn put_ref(w: &mut Writer, r: &FaceRef) {
    w.0.extend_from_slice(&r.key.0);
    for t in r.thousandths() {
        w.u16(t);
    }
}

fn get_ref(r: &mut Reader<'_>) -> Option<FaceRef> {
    let key = Key(r.take(16)?.try_into().ok()?);
    let mut t = [0u16; 4];
    for x in &mut t {
        *x = r.u16()?;
    }
    Some(FaceRef::new(key, t.map(|v| v as f32 / 1000.0)))
}

fn block(rows: &[(PairKey, Verdict)]) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(8 + rows.len() * 56));
    w.u32(rows.len() as u32);
    for (k, v) in rows {
        put_ref(&mut w, &k.0);
        put_ref(&mut w, &k.1);
        w.u32(v.p_ab.to_bits());
        w.u32(v.p_ba.to_bits());
    }
    w.seal()
}

fn read_block(data: &[u8]) -> Option<Vec<(PairKey, Verdict)>> {
    let mut r = Reader::open(data)?;
    let n = r.count(56)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let a = get_ref(&mut r)?;
        let b = get_ref(&mut r)?;
        let p_ab = f32::from_bits(r.u32()?);
        let p_ba = f32::from_bits(r.u32()?);
        out.push((PairKey::new(a, b), Verdict { p_ab, p_ba }));
    }
    r.done().then_some(out)
}

// ---- the runner ----------------------------------------------------------

/// One close call to judge, for the request `token`.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub token: u64,
    pub key: PairKey,
    pub ask: PairAsk,
}

/// What the runner reports back to the engine's thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The model is being loaded onto the card (the first close call
    /// after it was let go).
    Loading,
    /// A close call judged (or one of its photos unreadable: `Err`).
    Judged { token: u64, key: PairKey, result: Result<Verdict, String> },
    /// The card failed or the helper can't run: the runner stops; tasks
    /// still queued are dropped.
    Failed(JudgeError),
    /// The queue ran dry.
    Idle,
}

enum Msg {
    Tasks(Vec<Task>),
    /// Drop queued tasks of every token but this one (0: all).
    Keep(u64),
    /// Let the model go now (a pass wants the card).
    Unload,
    Quit,
}

/// The runner thread: owns the judge, asks it one pair at a time,
/// nearest first as queued, and reports each verdict.  Its queue is
/// replaced by the newest request's; [`Runner::unload`] lets the model
/// go (the card's memory with it).
pub struct Runner {
    tx: Sender<Msg>,
    judge: Arc<dyn Judge>,
    /// Bumped on every `keep`, so a task being judged when its request
    /// was dropped isn't reported for it.
    current: Arc<AtomicU64>,
}

impl Runner {
    /// Start a runner for `choice` (`factory` makes the judge), reporting
    /// through `events`.
    pub fn start(choice: JudgeChoice, factory: &JudgeFactory, events: impl Fn(Event) + Send + 'static) -> Runner {
        let judge = factory(choice);
        let (tx, rx) = crossbeam_channel::unbounded::<Msg>();
        let current = Arc::new(AtomicU64::new(0));
        let (j, cur) = (judge.clone(), current.clone());
        std::thread::Builder::new()
            .name("judge-runner".into())
            .spawn(move || run(j, rx, events, cur))
            .expect("spawn judge-runner");
        Runner { tx, judge, current }
    }

    /// Queue close calls (behind those already queued for the same
    /// request; another request's are dropped first with [`Runner::keep`]).
    pub fn ask(&self, tasks: Vec<Task>) {
        let _ = self.tx.send(Msg::Tasks(tasks));
    }

    /// Only `token`'s close calls stay queued (0: none).
    pub fn keep(&self, token: u64) {
        self.current.store(token, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Keep(token));
    }

    /// Let the model go now.
    pub fn unload(&self) {
        let _ = self.tx.send(Msg::Unload);
    }

    /// A helper holds the model on the card.
    pub fn loaded(&self) -> bool {
        self.judge.loaded()
    }

    pub fn judge(&self) -> &Arc<dyn Judge> {
        &self.judge
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
    }
}

fn run(judge: Arc<dyn Judge>, rx: Receiver<Msg>, events: impl Fn(Event), current: Arc<AtomicU64>) {
    let mut queue: std::collections::VecDeque<Task> = std::collections::VecDeque::new();
    let mut failed = false;
    loop {
        // Take everything waiting; block only when there's nothing to do.
        let msg = if queue.is_empty() {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        } else {
            rx.try_recv().ok()
        };
        if let Some(m) = msg {
            match m {
                Msg::Tasks(t) => {
                    if !failed {
                        queue.extend(t);
                    }
                }
                Msg::Keep(token) => queue.retain(|t| t.token == token && token != 0),
                Msg::Unload => {
                    queue.clear();
                    judge.unload();
                }
                Msg::Quit => {
                    judge.unload();
                    break;
                }
            }
            continue;
        }
        let Some(task) = queue.pop_front() else { continue };
        if task.token != current.load(Ordering::Relaxed) {
            continue;
        }
        if !judge.loaded() {
            events(Event::Loading);
        }
        match judge.judge(std::slice::from_ref(&task.ask)) {
            Ok(mut results) => {
                let r: Result<Verdict, String> = match results.pop() {
                    Some(PairResult { result }) => result.map_err(|e| e.message),
                    None => Err("no answer".into()),
                };
                events(Event::Judged { token: task.token, key: task.key, result: r });
                if queue.is_empty() {
                    events(Event::Idle);
                }
            }
            Err(e) => {
                queue.clear();
                failed = matches!(e, JudgeError::Card(_));
                judge.unload();
                events(Event::Failed(e));
            }
        }
    }
}

/// The crop a judge is shown for a head of `path`'s photo.
pub fn crop(path: &Path, file_type: crate::model::record::FileType, r: &FaceRef) -> Crop {
    Crop::Head { path: path.to_path_buf(), file_type, key: r.key, bbox: r.bbox }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::judge_client::{Fake, FakeMode};

    fn r(n: u8, x: f32) -> FaceRef {
        FaceRef::new(Key([n; 16]), [x, 0.1, 0.2, 0.3])
    }

    #[test]
    fn a_pair_is_one_whichever_way_round() {
        assert_eq!(PairKey::new(r(1, 0.1), r(2, 0.1)), PairKey::new(r(2, 0.1), r(1, 0.1)));
        assert_ne!(PairKey::new(r(1, 0.1), r(2, 0.1)), PairKey::new(r(1, 0.2), r(2, 0.1)));
    }

    #[test]
    fn verdicts_are_kept_per_model_privately_and_survive_a_reload() {
        let d = std::env::temp_dir().join(format!("pp-judged-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let mut c = PairCache::load(&d, JudgeChoice::Gemma4);
        assert_eq!(c.len(), 0);
        let v = Verdict { p_ab: 0.8, p_ba: 0.7 };
        c.add(&[(PairKey::new(r(1, 0.1), r(2, 0.1)), v)], JudgeChoice::Gemma4).unwrap();
        c.add(&[(PairKey::new(r(3, 0.1), r(2, 0.1)), Verdict { p_ab: 0.1, p_ba: 0.2 })], JudgeChoice::Gemma4).unwrap();
        let again = PairCache::load(&d, JudgeChoice::Gemma4);
        assert_eq!(again.len(), 2);
        assert_eq!(again.get(r(2, 0.1), r(1, 0.1)), Some(v), "either order");
        let path = d.join("judged-gemma4-26b-a4b-it-ud-q4km.bin");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(PairCache::bytes_in(&d) > 0);
        assert_eq!(PairCache::load(&d, JudgeChoice::Off).len(), 0, "Off keeps nothing");
        PairCache::forget_in(&d);
        assert_eq!(PairCache::bytes_in(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    fn task(token: u64, n: u8) -> Task {
        let (a, b) = (r(n, 0.1), r(n + 100, 0.1));
        Task { token, key: PairKey::new(a, b), ask: PairAsk { a: Crop::File(PathBuf::from("/x/a.jpg")), b: Crop::File(PathBuf::from("/x/b.jpg")) } }
    }

    #[test]
    fn the_runner_judges_in_order_and_drops_a_superseded_request() {
        let fake = Arc::new(Fake::new(FakeMode::Alternate));
        let f2 = fake.clone();
        let factory: JudgeFactory = Box::new(move |_| f2.clone() as Arc<dyn Judge>);
        let (tx, rx) = crossbeam_channel::unbounded();
        let runner = Runner::start(JudgeChoice::Gemma4, &factory, move |e| drop(tx.send(e)));
        runner.keep(1);
        runner.ask(vec![task(1, 1), task(1, 2), task(1, 3)]);
        let mut got = Vec::new();
        while got.len() < 3 {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).expect("an event") {
                Event::Judged { token, key, result } => got.push((token, key, result.unwrap().same())),
                Event::Loading | Event::Idle => {}
                e => panic!("{e:?}"),
            }
        }
        assert_eq!(got.iter().map(|g| g.2).collect::<Vec<_>>(), [true, false, true]);
        assert_eq!(got[0].1, task(1, 1).key, "nearest (first queued) first");
        // A newer request: the old one's queue goes.
        runner.keep(2);
        runner.ask(vec![task(2, 9)]);
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).expect("an event") {
                Event::Judged { token, .. } => {
                    assert_eq!(token, 2);
                    break;
                }
                _ => {}
            }
        }
        runner.unload();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!runner.loaded(), "let go on request");
    }

    #[test]
    fn a_card_failure_stops_the_runner_and_is_reported() {
        let factory: JudgeFactory = Box::new(|_| Arc::new(Fake::new(FakeMode::CardFail)) as Arc<dyn Judge>);
        let (tx, rx) = crossbeam_channel::unbounded();
        let runner = Runner::start(JudgeChoice::Gemma4, &factory, move |e| drop(tx.send(e)));
        runner.keep(1);
        runner.ask(vec![task(1, 1), task(1, 2)]);
        let mut failed = 0;
        let mut judged = 0;
        while let Ok(e) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
            match e {
                Event::Failed(JudgeError::Card(_)) => failed += 1,
                Event::Judged { .. } => judged += 1,
                _ => {}
            }
        }
        assert_eq!((failed, judged), (1, 0), "one report, nothing judged, nothing elsewhere");
        // Later asks are refused until a new runner (try the card again).
        runner.ask(vec![task(1, 3)]);
        assert!(rx.recv_timeout(std::time::Duration::from_millis(300)).is_err());
    }
}
