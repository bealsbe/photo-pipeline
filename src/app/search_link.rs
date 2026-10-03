//! The search page's link to the search engine (`crate::search::Engine`).
//!
//! The page talks to the engine through [`Link`], which has the engine's
//! calls and nothing else; the engine answers on the bus
//! (`BgEvent::SearchStatus`, `SearchResults`, `ModelDownload`).
//!
//! With `PP_SEARCH_FAKE` set, a stand-in takes the engine's place for
//! tests and screenshots: it answers queries with made-up hits from the
//! library index the calendar already holds (scored by a hash of the words
//! and the file name, so a query always finds the same photos), downloads
//! and indexes on a timer, and can be put in any state the page must show
//! (`search fake …` in a test script).  It sends the same events as the
//! engine, so the page can't tell them apart.  It reads no photo and
//! writes no file.  `PP_SEARCH_FAKE=indexed` starts it with the library
//! indexed.  It answers phase 2's questions too: made-up Things (the
//! vocabulary's objects and scenes, with counts that grow with the index),
//! the query's dates and `-word`s applied to its hits (dates alone answer
//! with every shot from them, oldest first), and, for an animal word, a
//! share of the hits handed back as fursuits left out.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as Span, NaiveDateTime};
use parking_lot::Mutex;

use crate::bus::{BgEvent, Bus};
use crate::ml::download::DownloadState;
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device, DeviceStatus, SelfCheck};
use crate::model::library_index::{self, LibraryIndex};
use crate::model::record::FileType;
use crate::search::index::Key;
use crate::search::{Engine, Hit, PassKind, PassProgress, Status, Stopped, Thing, vocab};
use crate::util;

pub enum Link {
    Engine(Engine),
    Fake(Fake),
}

impl Link {
    /// The engine, or the stand-in when `PP_SEARCH_FAKE` is set.
    pub fn new(bus: Bus, device: Device, index_exports: bool) -> Link {
        match std::env::var("PP_SEARCH_FAKE") {
            Ok(v) => Link::Fake(Fake::new(bus, device, index_exports, v == "indexed")),
            Err(_) => Link::Engine(Engine::new(bus, device, index_exports)),
        }
    }

    pub fn status(&self) -> Status {
        match self {
            Link::Engine(e) => e.status(),
            Link::Fake(f) => f.state.lock().status.clone(),
        }
    }

    /// *Things in your photos* as last derived (`BgEvent::SearchThings`
    /// carries each change).
    pub fn things(&self) -> Arc<Vec<Thing>> {
        match self {
            Link::Engine(e) => e.things(),
            Link::Fake(f) => f.state.lock().things.clone(),
        }
    }

    pub fn on_library(&self, index: Arc<LibraryIndex>) {
        match self {
            Link::Engine(e) => e.on_library(index),
            Link::Fake(f) => f.on_library(index),
        }
    }

    pub fn start_first_index(&self, models: &[ModelId]) {
        match self {
            Link::Engine(e) => e.start_first_index(models),
            Link::Fake(f) => f.start_first_index(models),
        }
    }

    pub fn stop_indexing(&self) {
        match self {
            Link::Engine(e) => e.stop_indexing(),
            Link::Fake(f) => f.change(|s| {
                s.status.pass = None;
                s.status.stopped = Some(Stopped::Cancelled);
            }),
        }
    }

    pub fn cancel_downloads(&self) {
        match self {
            Link::Engine(e) => e.cancel_downloads(),
            Link::Fake(f) => f.cancel_downloads(),
        }
    }

    pub fn try_card_again(&self) {
        match self {
            Link::Engine(e) => e.try_card_again(),
            Link::Fake(f) => f.try_card_again(),
        }
    }

    pub fn set_device(&self, d: Device) {
        match self {
            Link::Engine(e) => e.set_device(d),
            Link::Fake(f) => f.change(|s| {
                s.status.device = DeviceStatus { chosen: d, name: (d == Device::Gpu).then(|| FAKE_CARD.to_string()), self_check: SelfCheck::NotRun };
                // The processor needs no card: a card failure no longer
                // holds the work up.
                if d == Device::Cpu && matches!(s.status.stopped, Some(Stopped::Card(_))) {
                    s.status.stopped = None;
                    s.resume();
                }
            }),
        }
    }

    pub fn set_index_exports(&self, on: bool) {
        match self {
            Link::Engine(e) => e.set_index_exports(on),
            Link::Fake(f) => f.change(|s| s.status.index_exports = on),
        }
    }

    pub fn clear_index(&self) {
        match self {
            Link::Engine(e) => e.clear_index(),
            Link::Fake(f) => f.change(|s| {
                s.status.indexed_shots = 0;
                s.status.first_index_asked = false;
                s.status.pass = None;
                s.status.stopped = None;
            }),
        }
    }

    pub fn rebuild_index(&self) {
        match self {
            Link::Engine(e) => e.rebuild_index(),
            Link::Fake(f) => f.change(|s| {
                s.status.indexed_shots = 0;
                s.status.pass = Some(PassProgress { kind: PassKind::Rebuild, done: 0, total: s.status.library_shots, oldest_day: None });
            }),
        }
    }

    pub fn delete_models(&self) {
        match self {
            Link::Engine(e) => e.delete_models(),
            Link::Fake(f) => f.change(|s| {
                s.status.models_installed = false;
                s.status.pass = None;
            }),
        }
    }

    pub fn query(&self, seq: u64, text: &str, closeness: f32) {
        match self {
            Link::Engine(e) => e.query(seq, text, closeness),
            Link::Fake(f) => f.query(seq, text, closeness),
        }
    }

    /// Thumbnails waiting: the pass steps back while there are any.
    pub fn set_busy(&self, pending: usize) {
        if let Link::Engine(e) = self {
            e.set_busy(pending);
        }
    }

    pub fn shutdown(&self) {
        if let Link::Engine(e) = self {
            e.shutdown();
        }
    }

    /// Put the stand-in in a state for a test (`search fake …`); the real
    /// engine ignores it.
    pub fn fake(&self, what: &str) {
        match self {
            Link::Fake(f) => f.set(what),
            Link::Engine(_) => eprintln!("[search] fake {what:?} ignored: the real engine is running (set PP_SEARCH_FAKE)"),
        }
    }

    pub fn is_fake(&self) -> bool {
        matches!(self, Link::Fake(_))
    }
}

// ── the stand-in ────────────────────────────────────────────────────────────

const FAKE_CARD: &str = "Radeon graphics";
/// How often the stand-in moves downloads and indexing on.
const TICK: Duration = Duration::from_millis(250);

pub struct Fake {
    bus: Bus,
    state: Arc<Mutex<FakeState>>,
}

struct FakeState {
    status: Status,
    /// Each model's download: done, total, state.
    downloads: Vec<(ModelId, u64, u64, DownloadState)>,
    /// The next download ends this way (`search fake mismatch|offline`).
    fail_next: Option<DownloadState>,
    /// Hold a running pass where it is (screenshots).
    paused: bool,
    index: Option<Arc<LibraryIndex>>,
    /// Made-up Things, once something is indexed.
    things: Arc<Vec<Thing>>,
    /// The ticker thread is running.
    ticking: bool,
    /// Indexed in full once the library's size is known.
    start_indexed: bool,
}

impl FakeState {
    /// Carry on with what was stopped.
    fn resume(&mut self) {
        let s = &mut self.status;
        if s.pass.is_none() && s.first_index_asked && s.indexed_shots < s.library_shots {
            s.pass = Some(PassProgress { kind: PassKind::FirstIndex, done: s.indexed_shots, total: s.library_shots, oldest_day: None });
        }
        if s.pass.is_none() && s.waiting > 0 {
            s.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: s.waiting, oldest_day: None });
            s.waiting = 0;
        }
    }

    fn busy(&self) -> bool {
        !self.paused && (self.downloads.iter().any(|d| !d.3.is_final()) || (self.status.pass.is_some() && self.status.stopped.is_none()))
    }
}

impl Fake {
    fn new(bus: Bus, device: Device, index_exports: bool, indexed: bool) -> Fake {
        let mut status = Status {
            index_exports,
            device: DeviceStatus { chosen: device, name: (device == Device::Gpu).then(|| FAKE_CARD.to_string()), self_check: SelfCheck::NotRun },
            ..Status::default()
        };
        if indexed {
            status.models_installed = true;
            status.first_index_asked = true;
            status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
        }
        // A failing card for tests, as the engine reads it.
        if let Some(f) = crate::ml::fake_card_failure() {
            if device == Device::Gpu {
                if f == CardFailure::NoCard {
                    status.device.name = None;
                }
                status.stopped = Some(Stopped::Card(f));
            }
        }
        let state = FakeState { status, downloads: Vec::new(), fail_next: None, paused: false, index: None, things: Arc::new(Vec::new()), ticking: false, start_indexed: indexed };
        let fake = Fake { bus, state: Arc::new(Mutex::new(state)) };
        fake.publish(&fake.state.lock());
        fake
    }

    fn publish(&self, s: &FakeState) {
        let _ = self.bus.unbounded_send(BgEvent::SearchStatus(Arc::new(s.status.clone())));
    }

    /// Made-up Things for the start page: the vocabulary's objects and
    /// scenes with counts from a hash of each word, scaled to what is
    /// indexed; sent whenever the indexed count changes.
    fn publish_things(&self, s: &mut FakeState) {
        let indexed = s.status.indexed_shots;
        let mut things: Vec<Thing> = vocab::things()
            .filter_map(|(_, w)| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                w.word.hash(&mut h);
                let share = (h.finish() % 1000) as f64 / 1000.0;
                let photos = ((share.powi(4) * 0.12 * indexed as f64).round() as usize).min(indexed);
                (photos > 0).then(|| Thing { word: w.plural.to_string(), photos, kind: w.kind })
            })
            .collect();
        things.sort_by(|a, b| b.photos.cmp(&a.photos).then_with(|| a.word.cmp(&b.word)));
        if things != *s.things {
            s.things = Arc::new(things);
            let _ = self.bus.unbounded_send(BgEvent::SearchThings(s.things.clone()));
        }
    }

    fn publish_downloads(&self, s: &FakeState) {
        for (model, done, total, state) in &s.downloads {
            let _ = self.bus.unbounded_send(BgEvent::ModelDownload { model: *model, done: *done, total: *total, state: state.clone() });
        }
    }

    /// Change the state, say so, and keep the clock running while there is
    /// work.
    fn change(&self, f: impl FnOnce(&mut FakeState)) {
        let mut s = self.state.lock();
        f(&mut s);
        self.publish(&s);
        self.publish_downloads(&s);
        self.publish_things(&mut s);
        self.keep_ticking(&mut s);
    }

    fn keep_ticking(&self, s: &mut FakeState) {
        if s.ticking || !s.busy() {
            return;
        }
        s.ticking = true;
        let (state, bus) = (self.state.clone(), self.bus.clone());
        let me = Fake { bus, state };
        std::thread::Builder::new().name("search-fake".into()).spawn(move || me.run()).ok();
    }

    fn run(self) {
        loop {
            std::thread::sleep(TICK);
            let mut s = self.state.lock();
            if !s.busy() {
                s.ticking = false;
                return;
            }
            step(&mut s);
            self.publish(&s);
            self.publish_downloads(&s);
            self.publish_things(&mut s);
        }
    }

    fn on_library(&self, index: Arc<LibraryIndex>) {
        self.change(|s| {
            let shots = index.total_shots;
            let had = s.status.library_shots;
            s.index = Some(index);
            s.status.library_shots = shots;
            if s.start_indexed {
                s.start_indexed = false;
                s.status.indexed_shots = shots;
            }
            s.status.indexed_shots = s.status.indexed_shots.min(shots);
            // An export added photos: index them quietly (decision 5).
            let grew = shots.saturating_sub(had);
            if had > 0 && grew > 0 && s.status.first_index_asked && s.status.index_exports && s.status.pass.is_none() {
                if s.status.stopped.is_some() {
                    s.status.waiting += grew;
                } else {
                    s.status.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: grew, oldest_day: None });
                }
            }
        });
    }

    fn start_first_index(&self, ids: &[ModelId]) {
        self.change(|s| {
            s.status.first_index_asked = true;
            s.status.stopped = None;
            if !s.status.models_installed {
                s.downloads = ids.iter().map(|&id| (id, 0, models::info(id).bytes(), DownloadState::Waiting)).collect();
            } else {
                s.resume();
            }
        });
    }

    fn cancel_downloads(&self) {
        self.change(|s| {
            for d in &mut s.downloads {
                if !d.3.is_final() {
                    d.3 = DownloadState::Cancelled;
                }
            }
        });
    }

    fn try_card_again(&self) {
        self.change(|s| {
            if matches!(s.status.stopped, Some(Stopped::Card(_)) | Some(Stopped::LibraryAway { .. }) | Some(Stopped::Cancelled)) {
                s.status.stopped = None;
            }
            if s.status.device.chosen == Device::Gpu {
                s.status.device.name = Some(FAKE_CARD.into());
                s.status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
            }
            s.resume();
        });
    }

    fn set(&self, what: &str) {
        let (word, arg) = what.split_once(' ').unwrap_or((what, ""));
        let n: usize = arg.trim().parse().unwrap_or(0);
        self.change(|st| {
            let s = &mut st.status;
            match word {
                "notindexed" => {
                    s.models_installed = false;
                    s.indexed_shots = 0;
                    s.first_index_asked = false;
                    s.pass = None;
                    s.stopped = None;
                    s.waiting = 0;
                    st.downloads.clear();
                }
                "indexed" => {
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.indexed_shots = s.library_shots;
                    s.pass = None;
                    s.stopped = None;
                    s.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
                }
                "indexing" => {
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.stopped = None;
                    s.indexed_shots = if n > 0 { n.min(s.library_shots) } else { s.library_shots / 3 };
                    s.pass = Some(PassProgress { kind: PassKind::FirstIndex, done: s.indexed_shots, total: s.library_shots, oldest_day: None });
                }
                "export" => {
                    let n = n.max(1);
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.indexed_shots = s.library_shots.saturating_sub(n);
                    s.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: n, oldest_day: None });
                }
                "pause" => st.paused = true,
                "resume" => st.paused = false,
                "cardcrash" => s.stopped = Some(Stopped::Card(CardFailure::Crashed)),
                "selfcheck" => {
                    s.device.self_check = SelfCheck::Failed { cosine: 0.42, max_diff: 0.9 };
                    s.stopped = Some(Stopped::Card(CardFailure::SelfCheckFailed { cosine: 0.42, max_diff: 0.9 }));
                }
                "nocard" => {
                    s.device.name = None;
                    s.stopped = Some(Stopped::Card(CardFailure::NoCard));
                }
                "away" => {
                    let share = if arg.trim().is_empty() { "nas".to_string() } else { arg.trim().to_string() };
                    s.stopped = Some(Stopped::LibraryAway { share, message: "No route to host (os error 113)".into() });
                }
                "restarted" => s.helper_restarts += 1,
                "waiting" => s.waiting = n.max(1),
                "mismatch" => {
                    st.fail_next = Some(DownloadState::Mismatch {
                        expected: models::info(ModelId::Siglip2).weights_sha256().to_string(),
                        got: "7e410b9d3c2a8f6155a012c8f0a2be4c19d3e0a77f6b2c5d81e9a04b3f6c2d10".into(),
                    })
                }
                "offline" => st.fail_next = Some(DownloadState::Offline),
                _ => eprintln!("[search] unknown fake state {what:?}"),
            }
        });
    }

    fn query(&self, seq: u64, text: &str, closeness: f32) {
        let (index, indexed) = {
            let s = self.state.lock();
            (s.index.clone(), s.status.indexed_shots)
        };
        let (bus, words) = (self.bus.clone(), text.to_string());
        std::thread::Builder::new()
            .name("search-fake-query".into())
            .spawn(move || {
                let parsed = crate::search::parse(&words);
                let animal = vocab::animal_in(&parsed.words).map(|w| w.word.to_string());
                // Dates alone: every shot from them, in the order taken.
                let every = parsed.is_empty() && parsed.dates.is_some();
                let words = if every { "everything" } else { parsed.words.as_str() };
                let mut hits = index.map(|ix| made_up_hits(&ix, words, closeness, indexed)).unwrap_or_default();
                if parsed.is_empty() && !every {
                    hits.clear();
                }
                if every {
                    hits.sort_by(|a, b| a.capture.cmp(&b.capture));
                }
                if let Some(d) = parsed.dates {
                    hits.retain(|h| d.contains(h.day));
                }
                // Each word left out takes about a third of the hits away.
                hits.retain(|h| parsed.exclude.iter().all(|x| unit(&(x, h.id)) > 0.33));
                // The fursuit contrast: every third hit of an animal query
                // is a fursuit left out.
                let mut fursuits = Vec::new();
                if animal.is_some() {
                    let (f, h): (Vec<(usize, Hit)>, Vec<(usize, Hit)>) = hits.into_iter().enumerate().partition(|(i, _)| i % 3 == 2);
                    fursuits = f.into_iter().map(|(_, h)| h).collect();
                    hits = h.into_iter().map(|(_, h)| h).collect();
                }
                let _ = bus.unbounded_send(BgEvent::SearchResults {
                    seq,
                    hits: Arc::new(hits),
                    indexed,
                    error: None,
                    understood: Arc::new(parsed),
                    fursuits: Arc::new(fursuits),
                    animal,
                });
            })
            .ok();
    }
}

/// A hash of `x` as a number in 0..1.
fn unit(x: &impl Hash) -> f64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    (h.finish() % 10_000) as f64 / 10_000.0
}

/// One step of the stand-in's downloads, then its indexing.
fn step(s: &mut FakeState) {
    let fail = s.fail_next.clone();
    if let Some(d) = s.downloads.iter_mut().find(|d| !d.3.is_final()) {
        match d.3 {
            DownloadState::Waiting => d.3 = DownloadState::Downloading,
            DownloadState::Downloading => {
                d.1 = (d.1 + d.2 / 12).min(d.2);
                if d.1 >= d.2 {
                    d.3 = DownloadState::Checking;
                }
            }
            DownloadState::Checking => {
                d.3 = fail.unwrap_or(DownloadState::Checked);
            }
            _ => {}
        }
        if d.3 != DownloadState::Checked && d.3.is_final() {
            s.fail_next = None;
            return;
        }
        if s.downloads.iter().all(|d| d.3 == DownloadState::Checked) {
            s.status.models_installed = true;
            if s.status.device.chosen == Device::Gpu {
                s.status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
            }
            s.resume();
        }
        return;
    }
    if s.status.stopped.is_some() {
        return;
    }
    let library = s.status.library_shots;
    if let Some(p) = s.status.pass.as_mut() {
        p.done = (p.done + (p.total / 40).max(1)).min(p.total);
        let (kind, done, total) = (p.kind, p.done, p.total);
        if kind != PassKind::NewExports {
            s.status.indexed_shots = done;
        }
        if done >= total {
            if kind == PassKind::NewExports {
                s.status.indexed_shots = (s.status.indexed_shots + total).min(library);
            }
            s.status.pass = None;
        }
    }
}

/// The stand-in's hits: shots from the newest `indexed` of the library,
/// scored by a hash of the words and the file name, best first.  "zebra"
/// finds nothing; "everything" finds every shot (for long lists).
fn made_up_hits(index: &LibraryIndex, words: &str, closeness: f32, indexed: usize) -> Vec<Hit> {
    let words = words.trim().to_lowercase();
    if words.is_empty() || words.contains("zebra") {
        return Vec::new();
    }
    let everything = words == "everything";
    // The share of shots that pass: about 1 % at the strictest, 6 % at the
    // default, 25 % at the loosest.
    let span = crate::search::CLOSENESS_MAX - crate::search::CLOSENESS_MIN;
    let loose = ((crate::search::CLOSENESS_MAX - closeness) / span).clamp(0.0, 1.0) as f64;
    let share = if everything { 1.0 } else { 0.01 + loose.powf(1.6) * 0.24 };
    let mut hits = Vec::new();
    let mut seen = 0usize;
    let mut next_burst = 0u64;
    for (date, day) in index.days.iter().rev() {
        if seen >= indexed {
            break;
        }
        let mut shots: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>, NaiveDateTime)> = BTreeMap::new();
        for shot in day.shots() {
            let first = &day.files[shot[0]];
            let stem = util::stem(&first.path);
            let when = library_index::time_from_renamed_stem(&stem).unwrap_or(first.mtime.local);
            let e = shots.entry(stem.to_lowercase()).or_insert((None, None, when));
            for &i in shot {
                let f = &day.files[i];
                match f.file_type {
                    FileType::Jpg => e.0 = Some(f.path.clone()),
                    FileType::Raw => e.1 = Some(f.path.clone()),
                }
            }
        }
        seen += shots.len();
        // Presses: consecutive numbers within one minute, up to 8 frames.
        let mut last: Option<(NaiveDateTime, u32)> = None;
        let mut run_len = 0;
        for (stem, (jpg, raw, when)) in shots {
            let num: Option<u32> = stem.rsplit('_').next().and_then(|n| n.parse().ok());
            if matches!((last, num), (Some((t, n0)), Some(n1)) if t == when && n1 == n0 + 1 && run_len < 8) {
                run_len += 1;
            } else {
                next_burst += 1;
                run_len = 1;
            }
            last = num.map(|n| (when, n));
            let hash = |x: &dyn Fn(&mut std::collections::hash_map::DefaultHasher)| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                x(&mut h);
                (h.finish() % 10_000) as f64 / 10_000.0
            };
            let own = hash(&|h| (&words, &stem).hash(h));
            // Frames of a press score alike, so they come up together.
            let press = hash(&|h| (&words, next_burst, date).hash(h));
            let score = 0.7 * press + 0.3 * own;
            if everything || score > 1.0 - share {
                let capture = when + Span::milliseconds(num.unwrap_or(0) as i64 % 1000 * 3);
                let id = hash(&|h| (&stem, date).hash(h)).to_bits();
                hits.push(Hit { id, key: Key([0; 16]), score: score as f32, tagged: own > 0.8, day: *date, capture, burst: Some(next_burst), jpg, raw });
            }
        }
    }
    // A press of one frame is no burst.
    let mut sizes: HashMap<u64, usize> = HashMap::new();
    for h in &hits {
        *sizes.entry(h.burst.unwrap_or(u64::MAX)).or_default() += 1;
    }
    for h in &mut hits {
        if h.burst.is_some_and(|b| sizes.get(&b) == Some(&1)) {
            h.burst = None;
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.capture.cmp(&b.capture)));
    hits.truncate(crate::search::query::MAX_HITS.max(if everything { usize::MAX } else { 0 }));
    hits
}
