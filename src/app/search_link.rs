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
//! indexed.  It also answers phase 2's questions: the things it "found"
//! (a made-up vocabulary with counts that grow with the index, people,
//! fursuits and animals among them so the page's filter is exercised),
//! dates and `-word` applied to its hits, and, for an animal word, a share
//! of the hits handed back as fursuits left out.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as Span, NaiveDateTime};

use api::{Parsed, Thing, ThingKind};
use parking_lot::Mutex;

use crate::bus::{BgEvent, Bus};
use crate::ml::download::DownloadState;
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device, DeviceStatus, SelfCheck};
use crate::model::library_index::{self, LibraryIndex};
use crate::model::record::FileType;
use crate::search::index::Key;
use crate::search::{Engine, Hit, PassKind, PassProgress, Status, Stopped};
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

    /// The things search found in the library, most photos first (a clone
    /// of what the engine holds; no I/O).
    pub fn things(&self) -> Arc<Vec<Thing>> {
        match self {
            Link::Engine(_) => Arc::new(Vec::new()),
            Link::Fake(f) => f.things(),
        }
    }

    /// What came with query `seq`'s hits besides them: the fursuits an
    /// animal word left out, and what was understood of the words.
    pub fn take_extra(&self, seq: u64) -> Option<Extra> {
        match self {
            Link::Engine(_) => None,
            Link::Fake(f) => f.extras.lock().remove(&seq),
        }
    }
}

/// What a query's answer carries besides its hits.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    /// The fursuits an animal word left out (decision 12b), best first,
    /// never among the hits.
    pub fursuits: Arc<Vec<Hit>>,
    /// The animal word that left them out, singular ("dog").
    pub animal: Option<String>,
    /// What the engine understood of the words, and applied.
    pub understood: Arc<Parsed>,
}

/// The engine's phase 2 vocabulary and query syntax, as the page uses them.
/// Stand-ins with the shapes asked of the engine (things, the parser, a
/// date range's label) until it publishes its own.
pub mod api {
    use chrono::{Datelike, NaiveDate};

    use crate::model::library_index::{MONTH_ABBR, MONTH_NAME};

    /// What a thing in the vocabulary is.  Only objects and scenes are ever
    /// offered (the start page, completions): never people, fursuits or
    /// animals, which have their own kinds.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ThingKind {
        Object,
        Scene,
        Person,
        Fursuit,
        Animal,
    }

    impl ThingKind {
        /// Offered on the start page and as a completion.
        pub fn offered(self) -> bool {
            matches!(self, ThingKind::Object | ThingKind::Scene)
        }
    }

    /// A word of the vocabulary and how many indexed photos carry it.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Thing {
        pub word: String,
        pub photos: usize,
        pub kind: ThingKind,
    }

    /// Days from `from` to `to`, both included.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DateRange {
        pub from: NaiveDate,
        pub to: NaiveDate,
    }

    impl DateRange {
        pub fn year(y: i32) -> Option<DateRange> {
            Some(DateRange { from: NaiveDate::from_ymd_opt(y, 1, 1)?, to: NaiveDate::from_ymd_opt(y, 12, 31)? })
        }

        /// Months `m0` to `m1` (1–12) of year `y`.
        pub fn months(y: i32, m0: u32, m1: u32) -> Option<DateRange> {
            let from = NaiveDate::from_ymd_opt(y, m0, 1)?;
            let next = if m1 == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(y, m1 + 1, 1)? };
            Some(DateRange { from, to: next.pred_opt()? })
        }

        pub fn contains(&self, d: NaiveDate) -> bool {
            self.from <= d && d <= self.to
        }

        /// "2024", "Jun 2024", "Apr–Jun 2024", "Nov 2023–Feb 2024".
        pub fn label(&self) -> String {
            let (f, t) = (self.from, self.to);
            let whole_months = f.day() == 1 && t.succ_opt().is_some_and(|n| n.day() == 1);
            let abbr = |d: NaiveDate| MONTH_ABBR[d.month0() as usize];
            if whole_months && f.year() == t.year() && f.month() == 1 && t.month() == 12 {
                f.year().to_string()
            } else if whole_months && f.year() == t.year() && f.month() == t.month() {
                format!("{} {}", abbr(f), f.year())
            } else if whole_months && f.year() == t.year() {
                format!("{}–{} {}", abbr(f), abbr(t), f.year())
            } else if whole_months {
                format!("{} {}–{} {}", abbr(f), f.year(), abbr(t), t.year())
            } else {
                format!("{} {} {}–{} {} {}", f.day(), abbr(f), f.year(), t.day(), abbr(t), t.year())
            }
        }
    }

    /// A query in parts: the words to look for, words to leave out
    /// (`-word`), the dates (`2024`, `jun 2024`, `apr–jun 2024`) and names
    /// (`@name`, which phase 2 doesn't search yet).
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct Parsed {
        pub words: String,
        pub exclude: Vec<String>,
        pub dates: Option<DateRange>,
        pub names: Vec<String>,
    }

    /// The month a word names (`jun`, `june`, `sept`), 1–12.
    pub fn month_of(word: &str) -> Option<u32> {
        let w = word.to_lowercase();
        if w.len() < 3 {
            return None;
        }
        let i = MONTH_NAME.iter().map(|m| m.to_lowercase()).position(|m| m == w || (m.starts_with(&w) && (w.len() == 3 || w == "sept")))?;
        Some(i as u32 + 1)
    }

    fn year_of(word: &str) -> Option<i32> {
        if word.len() != 4 || !word.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        word.parse().ok().filter(|y| (1900..=2100).contains(y))
    }

    /// Split a query into its parts.  A month needs a year after it; on its
    /// own it is a word.
    pub fn parse(text: &str) -> Parsed {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let mut p = Parsed::default();
        let mut words = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let tok = tokens[i];
            let next_year = tokens.get(i + 1).and_then(|n| year_of(n));
            let span = tok.split_once(['–', '-']).and_then(|(a, b)| Some((month_of(a)?, month_of(b)?)));
            if let Some(name) = tok.strip_prefix('@').filter(|n| !n.is_empty()) {
                p.names.push(name.to_string());
            } else if let Some(w) = tok.strip_prefix('-').filter(|w| !w.is_empty()) {
                p.exclude.push(w.to_lowercase());
            } else if let Some(y) = year_of(tok) {
                p.dates = DateRange::year(y);
            } else if let (Some(m), Some(y)) = (month_of(tok), next_year) {
                p.dates = DateRange::months(y, m, m);
                i += 1;
            } else if let (Some((a, b)), Some(y)) = (span, next_year) {
                p.dates = DateRange::months(y, a.min(b), a.max(b));
                i += 1;
            } else {
                words.push(tok);
            }
            i += 1;
        }
        p.words = words.join(" ");
        p
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_query_splits_into_words_exclusions_dates_and_names() {
            let p = parse("fireworks -crowd jun 2024");
            assert_eq!(p.words, "fireworks");
            assert_eq!(p.exclude, ["crowd"]);
            assert_eq!(p.dates.unwrap().label(), "Jun 2024");
            let p = parse("red car at night 2023 @Alice");
            assert_eq!(p.words, "red car at night");
            assert_eq!(p.names, ["Alice"]);
            assert_eq!(p.dates.unwrap().label(), "2023");
            assert_eq!(parse("cake apr–jun 2024").dates.unwrap().label(), "Apr–Jun 2024");
            assert_eq!(parse("june").words, "june", "a month without a year is a word");
            assert_eq!(parse("sept 2025").dates.unwrap().label(), "Sep 2025");
        }
    }
}

// ── the stand-in ────────────────────────────────────────────────────────────

const FAKE_CARD: &str = "Radeon graphics";
/// How often the stand-in moves downloads and indexing on.
const TICK: Duration = Duration::from_millis(250);

pub struct Fake {
    bus: Bus,
    state: Arc<Mutex<FakeState>>,
    /// Each answer's extras, by query, until the page takes them.
    extras: Arc<Mutex<HashMap<u64, Extra>>>,
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
        let state = FakeState { status, downloads: Vec::new(), fail_next: None, paused: false, index: None, ticking: false, start_indexed: indexed };
        let fake = Fake { bus, state: Arc::new(Mutex::new(state)), extras: Arc::default() };
        fake.publish(&fake.state.lock());
        fake
    }

    fn publish(&self, s: &FakeState) {
        let _ = self.bus.unbounded_send(BgEvent::SearchStatus(Arc::new(s.status.clone())));
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
        self.keep_ticking(&mut s);
    }

    fn keep_ticking(&self, s: &mut FakeState) {
        if s.ticking || !s.busy() {
            return;
        }
        s.ticking = true;
        let (state, bus, extras) = (self.state.clone(), self.bus.clone(), self.extras.clone());
        let me = Fake { bus, state, extras };
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
        let (bus, words, extras) = (self.bus.clone(), text.to_string(), self.extras.clone());
        std::thread::Builder::new()
            .name("search-fake-query".into())
            .spawn(move || {
                let understood = api::parse(&words);
                let hits = index.map(|ix| made_up_hits(&ix, &understood, closeness, indexed)).unwrap_or_default();
                // An animal word (decision 12b): about half of what "looks
                // like" it is fursuits here, handed back on their own.
                let animal = if crate::search::vocab::is_animal_query(&understood.words) { animal_word(&understood.words) } else { None };
                let (hits, fursuits): (Vec<Hit>, Vec<Hit>) = match animal {
                    Some(_) => hits.into_iter().partition(|h| unit(&(h.id, "fursuit")) < 0.55),
                    None => (hits, Vec::new()),
                };
                extras.lock().insert(seq, Extra { fursuits: Arc::new(fursuits), animal, understood: Arc::new(understood) });
                let _ = bus.unbounded_send(BgEvent::SearchResults { seq, hits: Arc::new(hits), indexed, error: None });
            })
            .ok();
    }

    /// The stand-in's vocabulary: made-up things with counts that grow with
    /// what is indexed, people, fursuits and animals among them.
    fn things(&self) -> Arc<Vec<Thing>> {
        let indexed = self.state.lock().status.indexed_shots;
        let mut out: Vec<Thing> = FAKE_THINGS
            .iter()
            .map(|&(word, share, kind)| Thing { word: word.to_string(), photos: (indexed as f64 * share).round() as usize, kind })
            .filter(|t| t.photos > 0)
            .collect();
        out.sort_by(|a, b| b.photos.cmp(&a.photos).then(a.word.cmp(&b.word)));
        Arc::new(out)
    }
}

/// The stand-in's vocabulary: a word, the share of photos that show it,
/// and its kind.
const FAKE_THINGS: &[(&str, f64, ThingKind)] = &[
    ("fursuits", 0.55, ThingKind::Fursuit),
    ("people", 0.40, ThingKind::Person),
    ("convention hall", 0.12, ThingKind::Scene),
    ("crowd", 0.09, ThingKind::Scene),
    ("cameras", 0.08, ThingKind::Object),
    ("night", 0.07, ThingKind::Scene),
    ("stage", 0.06, ThingKind::Scene),
    ("trees", 0.04, ThingKind::Object),
    ("buildings", 0.035, ThingKind::Object),
    ("signs", 0.03, ThingKind::Object),
    ("park", 0.03, ThingKind::Scene),
    ("badges", 0.025, ThingKind::Object),
    ("food", 0.02, ThingKind::Object),
    ("phones", 0.02, ThingKind::Object),
    ("city street", 0.02, ThingKind::Scene),
    ("cars", 0.015, ThingKind::Object),
    ("drinks", 0.012, ThingKind::Object),
    ("landscape", 0.01, ThingKind::Scene),
    ("beach", 0.008, ThingKind::Scene),
    ("sunset", 0.007, ThingKind::Scene),
    ("flowers", 0.006, ThingKind::Object),
    ("snow", 0.006, ThingKind::Scene),
    ("balloons", 0.005, ThingKind::Object),
    ("cake", 0.004, ThingKind::Object),
    ("fireworks", 0.003, ThingKind::Object),
    ("dogs", 0.002, ThingKind::Animal),
    ("cats", 0.001, ThingKind::Animal),
];

/// The query's animal word, singular ("dogs" → "dog").
fn animal_word(words: &str) -> Option<String> {
    words.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).find_map(|w| {
        let one = w.strip_suffix("ies").map(|s| format!("{s}y")).or_else(|| w.strip_suffix('s').map(str::to_string)).unwrap_or_else(|| w.clone());
        [w, one].into_iter().find(|x| crate::search::vocab::ANIMAL_WORDS.contains(&x.as_str()))
    })
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
/// scored by a hash of the words and the file name, best first, within the
/// query's dates, and without about a third of them for each word left
/// out.  "zebra" finds nothing; "everything" finds every shot (for long
/// lists).
fn made_up_hits(index: &LibraryIndex, q: &Parsed, closeness: f32, indexed: usize) -> Vec<Hit> {
    let words = q.words.trim().to_lowercase();
    if (words.is_empty() && q.dates.is_none()) || words.contains("zebra") {
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
            let wanted = q.dates.is_none_or(|d| d.contains(*date)) && q.exclude.iter().all(|x| unit(&(x, &stem)) > 0.33);
            if wanted && (everything || score > 1.0 - share) {
                let capture = when + Span::milliseconds(num.unwrap_or(0) as i64 % 1000 * 3);
                let id = hash(&|h| (&stem, date).hash(h)).to_bits();
                hits.push(Hit { id, key: Key([0; 16]), score: score as f32, day: *date, capture, burst: Some(next_burst), jpg, raw });
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
