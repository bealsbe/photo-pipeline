//! A model download's speed and time left, worked out on the UI thread
//! from the progress events it already gets (`BgEvent::ModelDownload`:
//! the bytes fetched over the network, and when each event arrived).
//! Memory only, a few dozen numbers: no I/O, nothing per frame.
//!
//! - **Smoothed.**  The speed is the bytes over the last [`WINDOW`]
//!   seconds; the time left uses a longer [`STEADY`] window, so its
//!   minutes don't jump with each burst.  The figure shown changes at most
//!   once a [`REFRESH`].
//! - **Honest at the edges.**  Until [`SETTLE`] seconds of bytes have
//!   arrived (at the start, after a model is checked, after a stall) there
//!   is no figure, only "starting…".  With no byte for [`STALL`] seconds
//!   it says the download is waiting for the network, never "0 KB/s".
//! - **Network bytes only.**  A file already in place counts in the
//!   progress, never in the speed (`fetched`, not `done`).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::ml::models::ModelId;
use crate::search::estimate;

/// The speed shown is the average over this long.
pub const WINDOW: Duration = Duration::from_secs(5);
/// The time left is worked out from the average over this long.
pub const STEADY: Duration = Duration::from_secs(30);
/// Bytes over at least this long before any figure shows.
pub const SETTLE: Duration = Duration::from_secs(3);
/// No byte for this long: waiting for the network.
pub const STALL: Duration = Duration::from_secs(6);
/// The figure shown changes at most this often.
pub const REFRESH: Duration = Duration::from_secs(1);

/// What the download's speed is, as the UI says it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    /// Not enough bytes yet for a figure.
    Starting,
    /// No byte for [`STALL`] or longer.
    Stalled,
    /// Bytes a second: `now` over [`WINDOW`], `steady` over [`STEADY`]
    /// (for the time left).
    Rate { now: f64, steady: f64 },
}

/// The speed of the downloads under way, as one stream (they run one
/// after another).
#[derive(Debug, Default)]
pub struct Speed {
    /// A download set is under way (else the next event starts afresh).
    active: bool,
    /// Each model's `fetched` as last reported.
    seen: Vec<(ModelId, u64)>,
    /// The model whose bytes are coming down, until it is checked.
    current: Option<ModelId>,
    /// Network bytes since the set began.
    bytes: u64,
    /// (when, `bytes` then), oldest first: the samples since the last
    /// start, back to [`STEADY`] and one before it.
    samples: VecDeque<(Instant, u64)>,
    /// The last byte, or the last (re)start: the stall clock.
    active_at: Option<Instant>,
    /// The figure shown, and when it was worked out.
    shown: Option<(Instant, f64, f64)>,
}

impl Speed {
    /// A progress event: `model` has `fetched` network bytes, and is
    /// downloading (`receiving`) or not (waiting, checking).
    pub fn observe(&mut self, model: ModelId, fetched: u64, receiving: bool, now: Instant) {
        if !self.active {
            *self = Speed { active: true, active_at: Some(now), ..Speed::default() };
        }
        let last = match self.seen.iter_mut().find(|(m, _)| *m == model) {
            Some((_, f)) => std::mem::replace(f, fetched),
            None => {
                self.seen.push((model, fetched));
                0
            }
        };
        // Less than before (a retry from the start): a new baseline.
        let delta = fetched.saturating_sub(last);
        if !receiving {
            // The model coming down is being checked: the clock starts
            // again with the next file, so the pause doesn't read as slow.
            // (Other models' rows, waiting or done, change nothing.)
            if self.current == Some(model) {
                self.current = None;
                self.restart(now);
            }
            return;
        }
        if self.current != Some(model) {
            // The next model (or file): a new connection, a new start.
            self.current = Some(model);
            self.restart(now);
        }
        if delta == 0 {
            return;
        }
        if self.active_at.is_some_and(|t| now.duration_since(t) >= STALL) {
            self.restart(now);
        }
        self.bytes += delta;
        self.active_at = Some(now);
        if self.samples.is_empty() {
            // The first bytes came over an unknown time: they are the
            // baseline, not counted.
            self.samples.push_back((now, self.bytes));
            return;
        }
        self.samples.push_back((now, self.bytes));
        while self.samples.len() > 2 && self.samples[1].0 + STEADY <= now {
            self.samples.pop_front();
        }
        if self.shown.is_none_or(|(at, _, _)| now.duration_since(at) >= REFRESH) {
            if let (Some(fast), Some(steady)) = (self.rate_over(WINDOW), self.rate_over(STEADY)) {
                self.shown = Some((now, fast, steady));
            }
        }
    }

    /// Every download of the set has ended: the next event starts afresh.
    pub fn finish(&mut self) {
        self.active = false;
    }

    /// The speed now, for a download that is receiving.
    pub fn reading(&self, now: Instant) -> Reading {
        if self.active_at.is_none_or(|t| now.duration_since(t) >= STALL) {
            return Reading::Stalled;
        }
        match self.shown {
            Some((_, now, steady)) => Reading::Rate { now, steady },
            None => Reading::Starting,
        }
    }

    fn restart(&mut self, now: Instant) {
        self.samples.clear();
        self.shown = None;
        self.active_at = Some(now);
    }

    /// Bytes a second over the last `w` (or all there is), once that is
    /// at least [`SETTLE`] long.
    fn rate_over(&self, w: Duration) -> Option<f64> {
        rate_over(&self.samples, w)
    }
}

/// Bytes a second over the last `w` of `samples` ((when, bytes so far),
/// oldest first; or all there is), once that is at least [`SETTLE`] long.
/// Shared with the copies' speed (`transfer_speed`).
pub(super) fn rate_over(samples: &VecDeque<(Instant, u64)>, w: Duration) -> Option<f64> {
    let &(t1, b1) = samples.back()?;
    let &(t0, b0) = samples.iter().rev().find(|(t, _)| *t + w <= t1).or(samples.front())?;
    let span = t1.duration_since(t0);
    (span >= SETTLE).then(|| (b1 - b0) as f64 / span.as_secs_f64())
}

/// A speed in the units the progress uses (decimal): "38 MB/s",
/// "4.2 MB/s", "640 KB/s" when slow, "1.1 GB/s".
pub fn rate_words(bps: f64) -> String {
    let kb = (bps / 1e3).round();
    if kb < 1000.0 {
        return format!("{} KB/s", kb.max(1.0) as u64);
    }
    let mb = bps / 1e6;
    if (mb * 10.0).round() < 100.0 {
        format!("{mb:.1} MB/s")
    } else if mb.round() < 1000.0 {
        format!("{} MB/s", mb.round() as u64)
    } else {
        format!("{:.1} GB/s", bps / 1e9)
    }
}

/// The progress with its speed after it: "4.2 of 9.9 GB · 38 MB/s"; the
/// progress alone without a figure.
pub fn with_speed(progress: String, reading: Option<Reading>) -> String {
    match reading {
        Some(Reading::Rate { now, .. }) => format!("{progress} \u{b7} {}", rate_words(now)),
        _ => progress,
    }
}

/// Beside the progress, lower case: "about 3 minutes left",
/// "starting…", "waiting for the network".
pub fn note(reading: Reading, remaining: u64) -> String {
    match reading {
        Reading::Starting => "starting\u{2026}".into(),
        Reading::Stalled => "waiting for the network".into(),
        Reading::Rate { steady, .. } => format!("{} left", estimate::describe_seconds(remaining as f64 / steady.max(1.0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: ModelId = ModelId::Siglip2;

    /// Feed `bps` for `secs` seconds, an event every 200 ms, from `t0`
    /// with `fetched` already; returns the time and bytes at the end.
    fn feed(s: &mut Speed, t0: Instant, fetched: u64, bps: u64, secs: u64) -> (Instant, u64) {
        feed_model(s, M, t0, fetched, bps, secs)
    }

    fn feed_model(s: &mut Speed, model: ModelId, t0: Instant, fetched: u64, bps: u64, secs: u64) -> (Instant, u64) {
        let mut f = fetched;
        let mut t = t0;
        for _ in 0..secs * 5 {
            t += Duration::from_millis(200);
            f += bps / 5;
            s.observe(model, f, true, t);
        }
        (t, f)
    }

    #[test]
    fn nothing_shows_before_a_few_seconds_of_bytes() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        assert_eq!(s.reading(t0), Reading::Starting);
        let (t, _) = feed(&mut s, t0, 0, 10_000_000, 2);
        assert_eq!(s.reading(t), Reading::Starting, "two seconds isn't enough");
        let (t, _) = feed(&mut s, t, 10_000_000 * 2, 10_000_000, 2);
        assert!(matches!(s.reading(t), Reading::Rate { .. }), "{:?}", s.reading(t));
    }

    #[test]
    fn a_steady_rate_reads_as_itself() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, _) = feed(&mut s, t0, 0, 38_000_000, 20);
        let Reading::Rate { now, steady } = s.reading(t) else { panic!("{:?}", s.reading(t)) };
        assert!((now - 38e6).abs() < 0.5e6, "{now}");
        assert!((steady - 38e6).abs() < 0.5e6, "{steady}");
        assert_eq!(with_speed("4.2 of 9.9 GB".into(), Some(s.reading(t))), "4.2 of 9.9 GB \u{b7} 38 MB/s");
    }

    #[test]
    fn a_burst_is_smoothed_and_the_figure_holds_for_a_second() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, f) = feed(&mut s, t0, 0, 10_000_000, 10);
        // One event carries a second's worth at 50 MB/s.
        let t1 = t + Duration::from_millis(200);
        s.observe(M, f + 10_000_000, true, t1);
        let Reading::Rate { now, .. } = s.reading(t1) else { panic!() };
        assert!(now < 13e6, "one burst over a 5 s window moves it a little: {now}");
        // The figure shown changes at most once a second.
        let before = s.reading(t1);
        s.observe(M, f + 30_000_000, true, t1 + Duration::from_millis(200));
        assert_eq!(s.reading(t1 + Duration::from_millis(200)), before);
        // The time left uses the longer window: steadier still.
        let Reading::Rate { steady, .. } = s.reading(t1) else { panic!() };
        assert!(steady <= now, "{steady} {now}");
    }

    #[test]
    fn a_stall_says_so_and_recovery_starts_again() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, f) = feed(&mut s, t0, 0, 5_000_000, 10);
        assert!(matches!(s.reading(t + Duration::from_secs(5)), Reading::Rate { .. }));
        assert_eq!(s.reading(t + STALL), Reading::Stalled);
        assert_eq!(note(Reading::Stalled, 1), "waiting for the network");
        // Bytes again: a new start, not a figure diluted by the gap.
        let back = t + Duration::from_secs(30);
        s.observe(M, f + 1_000_000, true, back);
        assert_eq!(s.reading(back), Reading::Starting);
        let (t, _) = feed(&mut s, back, f + 1_000_000, 5_000_000, 4);
        let Reading::Rate { now, .. } = s.reading(t) else { panic!() };
        assert!((now - 5e6).abs() < 0.2e6, "{now}");
    }

    #[test]
    fn no_byte_at_all_is_a_stall_too() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        assert_eq!(s.reading(t0 + Duration::from_secs(2)), Reading::Starting);
        assert_eq!(s.reading(t0 + STALL), Reading::Stalled);
    }

    #[test]
    fn a_check_between_files_does_not_read_as_slow() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, f) = feed(&mut s, t0, 0, 2_000_000, 6);
        // Checking for 20 s: no stall, and the next model starts afresh.
        s.observe(M, f, false, t + Duration::from_secs(1));
        let t2 = t + Duration::from_secs(20);
        s.observe(ModelId::AuraFace, 0, false, t2);
        s.observe(ModelId::AuraFace, 0, true, t2);
        assert_eq!(s.reading(t2), Reading::Starting);
        let (t3, _) = feed_model(&mut s, ModelId::AuraFace, t2, 0, 2_000_000, 5);
        let Reading::Rate { now, .. } = s.reading(t3) else { panic!() };
        assert!((now - 2e6).abs() < 0.1e6, "{now}");
    }

    #[test]
    fn other_rows_reported_again_change_nothing() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, f) = feed(&mut s, t0, 0, 4_000_000, 6);
        let before = s.reading(t);
        // A model done, another waiting, reported again beside it.
        s.observe(ModelId::Yunet, 232_589, false, t);
        s.observe(ModelId::AuraFace, 0, false, t);
        assert_eq!(s.reading(t), before);
        let (t, _) = feed(&mut s, t, f, 4_000_000, 2);
        let Reading::Rate { now, .. } = s.reading(t) else { panic!("{:?}", s.reading(t)) };
        assert!((now - 4e6).abs() < 0.1e6, "{now}");
    }

    #[test]
    fn a_new_set_starts_afresh() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        s.observe(M, 0, true, t0);
        let (t, _) = feed(&mut s, t0, 0, 2_000_000, 6);
        s.finish();
        // A retry from nothing: a fresh start, its bytes counted from 0.
        let t1 = t + Duration::from_secs(60);
        s.observe(M, 0, true, t1);
        assert_eq!(s.reading(t1), Reading::Starting);
        let (t2, _) = feed(&mut s, t1, 0, 3_000_000, 5);
        let Reading::Rate { now, .. } = s.reading(t2) else { panic!() };
        assert!((now - 3e6).abs() < 0.1e6, "{now}");
    }

    #[test]
    fn speeds_are_said_in_the_progress_units() {
        assert_eq!(rate_words(38_400_000.0), "38 MB/s");
        assert_eq!(rate_words(4_240_000.0), "4.2 MB/s");
        assert_eq!(rate_words(9_960_000.0), "10 MB/s");
        assert_eq!(rate_words(1_000_000.0), "1.0 MB/s");
        assert_eq!(rate_words(999_400.0), "999 KB/s");
        assert_eq!(rate_words(999_600.0), "1.0 MB/s");
        assert_eq!(rate_words(640_000.0), "640 KB/s");
        assert_eq!(rate_words(120.0), "1 KB/s");
        assert_eq!(rate_words(1_130_000_000.0), "1.1 GB/s");
        assert_eq!(with_speed("312 of 785 MB".into(), None), "312 of 785 MB");
        assert_eq!(with_speed("312 of 785 MB".into(), Some(Reading::Starting)), "312 of 785 MB");
    }

    #[test]
    fn time_left_is_in_round_words() {
        let r = |steady: f64| Reading::Rate { now: steady, steady };
        assert_eq!(note(r(38e6), 5_700_000_000), "about 3 minutes left");
        assert_eq!(note(r(38e6), 1_000_000_000), "under a minute left");
        assert_eq!(note(r(1e6), 5_000_000_000), "about 1.4 hours left");
        assert_eq!(note(Reading::Starting, 1), "starting\u{2026}");
    }
}
