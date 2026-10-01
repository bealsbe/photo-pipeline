//! Opt-in timing for finding slow spots.  With `PP_PROFILE=1` the app
//! records how long each update, view rebuild and custom-widget draw takes,
//! and the gaps between animation frames.  Anything over a frame (16 ms) is
//! printed as it happens, and a summary table goes to stderr on exit.  Off,
//! each probe costs one relaxed atomic load.
//!
//! A build with `--features profiler` also samples the CPU in-process (no
//! perf permissions needed) when `PP_PROFILE_OUT=<file>` is set, and writes
//! the samples as folded stacks (`thread;outer;…;inner count` per line).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);
const SLOW: Duration = Duration::from_millis(16);
/// A frame gap this long means at least one frame was missed (at 60 Hz).
const SLOW_GAP: Duration = Duration::from_millis(25);
/// Redraws further apart than this aren't one animation's frames.
const FRAME_RUN: Duration = Duration::from_millis(100);

pub fn init() {
    ON.store(std::env::var_os("PP_PROFILE").is_some(), Ordering::Relaxed);
    started();
}

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// When the process started (as near as `main` can tell).
pub fn started() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

#[derive(Default)]
struct Stat {
    count: u64,
    total: Duration,
    max: Duration,
    slow: u64,
}

/// A probe's name: a category ("update", "draw", "frame" …) and what in it.
pub type Name = (&'static str, &'static str);

fn stats() -> &'static Mutex<HashMap<Name, Stat>> {
    static STATS: OnceLock<Mutex<HashMap<Name, Stat>>> = OnceLock::new();
    STATS.get_or_init(Default::default)
}

pub fn record(name: Name, d: Duration) {
    if !on() {
        return;
    }
    let slow = if name.0 == "frame gap" { SLOW_GAP } else { SLOW };
    if d >= slow && name.0 != "startup" {
        eprintln!("[profile] slow {}:{}: {:.1} ms", name.0, name.1, d.as_secs_f64() * 1000.0);
    }
    if let Ok(mut s) = stats().lock() {
        let st = s.entry(name).or_default();
        st.count += 1;
        st.total += d;
        st.max = st.max.max(d);
        st.slow += (d >= slow) as u64;
    }
}

/// Times its own lifetime under `name`.
pub struct Span(Option<(Name, Instant)>);

#[inline]
pub fn span(category: &'static str, what: &'static str) -> Span {
    Span(on().then(|| ((category, what), Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((name, t)) = self.0 {
            record(name, t.elapsed());
        }
    }
}

/// A redraw of the widget behind `name` at `now`: the gap since its last
/// one is recorded while redraws come in an unbroken run (an animation).
pub fn frame(widget: &'static str, now: Instant) {
    if !on() {
        return;
    }
    static LAST: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();
    let prev = LAST.get_or_init(Default::default).lock().ok().and_then(|mut m| m.insert(widget, now));
    if let Some(gap) = prev.and_then(|p| now.checked_duration_since(p)) {
        if gap < FRAME_RUN {
            record(("frame gap", widget), gap);
        }
    }
}

fn marks() -> &'static Mutex<HashMap<&'static str, Instant>> {
    static MARKS: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();
    MARKS.get_or_init(Default::default)
}

/// Start timing `what` (see [`since`]).
pub fn mark(what: &'static str) {
    if on() {
        if let Ok(mut m) = marks().lock() {
            m.insert(what, Instant::now());
        }
    }
}

/// Record the time since [`mark`]`(what)` under `category:what`, once.
pub fn since(category: &'static str, what: &'static str) {
    if !on() {
        return;
    }
    let start = marks().lock().ok().and_then(|mut m| m.remove(what));
    if let Some(t) = start {
        record((category, what), t.elapsed());
    }
}

/// Once: the time from process start to `name` (e.g. the first frame).
pub fn milestone(name: &'static str) {
    if !on() {
        return;
    }
    static SEEN: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    if let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() {
        if !seen.contains(&name) {
            seen.push(name);
            let d = started().elapsed();
            eprintln!("[profile] {name} at {:.0} ms", d.as_secs_f64() * 1000.0);
            drop(seen);
            record(("startup", name), d);
        }
    }
}

pub fn report() {
    if !on() {
        return;
    }
    let Ok(s) = stats().lock() else { return };
    let mut rows: Vec<_> = s.iter().collect();
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    eprintln!("[profile] {:<34} {:>7} {:>10} {:>8} {:>8} {:>6}", "what", "count", "total ms", "mean ms", "max ms", "slow");
    for ((cat, what), st) in rows {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "[profile] {:<34} {:>7} {:>10.1} {:>8.2} {:>8.1} {:>6}",
            format!("{cat}:{what}"),
            st.count,
            ms(st.total),
            ms(st.total) / st.count.max(1) as f64,
            ms(st.max),
            st.slow
        );
    }
}

#[cfg(feature = "profiler")]
pub struct Sampler(Option<pprof::ProfilerGuard<'static>>);

#[cfg(feature = "profiler")]
pub fn sampler() -> Sampler {
    let on = std::env::var_os("PP_PROFILE_OUT").is_some();
    Sampler(on.then(|| pprof::ProfilerGuardBuilder::default().frequency(997).blocklist(&["libc", "libgcc", "pthread", "vdso"]).build().ok()).flatten())
}

#[cfg(feature = "profiler")]
impl Sampler {
    pub fn finish(self) {
        use std::io::Write;
        let (Some(guard), Some(out)) = (self.0, std::env::var_os("PP_PROFILE_OUT")) else { return };
        let Ok(report) = guard.report().build() else { return };
        let Ok(mut f) = std::fs::File::create(&out) else { return };
        for (frames, count) in &report.data {
            let mut line = frames.thread_name.replace(';', "_");
            // Outermost first.
            for frame in frames.frames.iter().rev() {
                for sym in frame.iter().rev() {
                    line.push(';');
                    line.push_str(&sym.name().replace(';', "_"));
                }
            }
            let _ = writeln!(f, "{line} {count}");
        }
        eprintln!("[profile] samples written to {}", std::path::Path::new(&out).display());
    }
}

