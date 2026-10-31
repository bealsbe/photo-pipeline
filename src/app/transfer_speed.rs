//! A copy's speed and time left (export to the library, edit copies),
//! worked out on the UI thread from the progress events it already gets
//! (`ops::Progress`, one after each finished file).  Memory only, a few
//! dozen numbers: no I/O, nothing per frame.
//!
//! The same rules as the model download's speed (`download_speed`):
//! smoothed over its windows, "starting…" until [`SETTLE`] seconds of
//! bytes have gone over, a stalled state instead of "0 KB/s".  Two things
//! are its own:
//!
//! - **A file at a time.**  Progress arrives when a file finishes, so a
//!   long file looks like silence.  A stall is called only once the file
//!   under way has had the time it should take at the steady rate, and
//!   [`STALL`] more.
//! - **Renames aren't transfers.**  A move within one device is instant,
//!   so a run that moves files without carrying bytes shows no speed at
//!   all (`reading` is `None`), rather than a made-up figure.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::download_speed::{self, Reading};
use crate::ops::Progress;
use crate::search::estimate;

/// No file finished for this long past the one under way's expected time:
/// stalled.
pub const STALL: Duration = download_speed::STALL;

/// The speed of one copy run.
#[derive(Debug, Default)]
pub struct Speed {
    /// (when, bytes transferred then), oldest first, back to [`download_speed::STEADY`]
    /// and one before it.
    samples: VecDeque<(Instant, u64)>,
    /// When the run started, or the last file finished: the stall clock.
    active_at: Option<Instant>,
    /// The figure shown, and when it was worked out.
    shown: Option<(Instant, f64, f64)>,
    /// The last progress seen.
    last: Progress,
}

impl Speed {
    /// The run begins now, with nothing done.
    pub fn start(now: Instant) -> Self {
        Speed { samples: VecDeque::from([(now, 0)]), active_at: Some(now), ..Speed::default() }
    }

    /// A file finished (`p` is the run so far).
    pub fn observe(&mut self, p: Progress, now: Instant) {
        self.active_at = Some(now);
        if p.transferred > self.last.transferred {
            download_speed::record(&mut self.samples, &mut self.shown, now, p.transferred);
        }
        self.last = p;
    }

    /// The speed now; `None` for a run of renames (nothing to read).
    pub fn reading(&self, now: Instant) -> Option<Reading> {
        if self.last.files > 0 && self.last.transferred == 0 {
            return None;
        }
        let since = self.active_at.map_or(Duration::ZERO, |t| now.duration_since(t));
        let steady = self.shown.map(|(_, _, s)| s);
        let expected = steady.filter(|s| *s > 0.0).map_or(Duration::ZERO, |s| Duration::from_secs_f64(self.last.next as f64 / s));
        if since >= STALL + expected {
            return Some(Reading::Stalled);
        }
        Some(match self.shown {
            Some((_, now, steady)) => Reading::Rate { now, steady },
            None => Reading::Starting,
        })
    }
}

/// The progress line with the speed and time left after it:
/// "12 of 40 files, 310 of 980 MB · 38 MB/s · about 2 minutes left".
/// Starting and stalled say so; a run of renames shows the progress alone.
pub fn caption(progress: String, reading: Option<Reading>, remaining: u64) -> String {
    match reading {
        None => progress,
        Some(Reading::Starting) => format!("{progress} \u{b7} starting\u{2026}"),
        Some(Reading::Stalled) => format!("{progress} \u{b7} stalled"),
        Some(r @ Reading::Rate { steady, .. }) => {
            let left = estimate::describe_seconds(remaining as f64 / steady.max(1.0));
            format!("{} \u{b7} {left} left", download_speed::with_speed(progress, Some(r)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    /// `n` files of `size` bytes finish, one every `every`, from `t`;
    /// `next` is the size announced after each.  Returns the end time.
    fn files(s: &mut Speed, p: &mut Progress, t: Instant, n: usize, size: u64, every: Duration, next: u64) -> Instant {
        let mut t = t;
        for _ in 0..n {
            t += every;
            p.files += 1;
            p.bytes += size;
            p.transferred += size;
            p.next = next;
            s.observe(*p, t);
        }
        t
    }

    fn run(total_files: usize, total_bytes: u64) -> Progress {
        Progress { files_total: total_files, bytes_total: total_bytes, ..Progress::default() }
    }

    #[test]
    fn nothing_shows_before_a_few_seconds_of_files() {
        let t0 = Instant::now();
        let mut s = Speed::start(t0);
        let mut p = run(100, 100 * 20 * MB);
        assert_eq!(s.reading(t0), Some(Reading::Starting));
        // 20 MB files every half second: 40 MB/s.  Two seconds isn't enough.
        let t = files(&mut s, &mut p, t0, 4, 20 * MB, Duration::from_millis(500), 20 * MB);
        assert_eq!(s.reading(t), Some(Reading::Starting));
        let t = files(&mut s, &mut p, t, 4, 20 * MB, Duration::from_millis(500), 20 * MB);
        let Some(Reading::Rate { now, steady }) = s.reading(t) else { panic!("{:?}", s.reading(t)) };
        assert!((now - 40e6).abs() < 1e6, "{now}");
        assert!((steady - 40e6).abs() < 1e6, "{steady}");
    }

    #[test]
    fn a_big_file_is_not_a_stall_but_silence_past_its_time_is() {
        let t0 = Instant::now();
        let mut s = Speed::start(t0);
        let mut p = run(100, 100 * 20 * MB);
        // 40 MB/s settled, then a 400 MB file is announced: 10 s expected.
        let t = files(&mut s, &mut p, t0, 12, 20 * MB, Duration::from_millis(500), 400 * MB);
        assert!(matches!(s.reading(t + Duration::from_secs(12)), Some(Reading::Rate { .. })), "{:?}", s.reading(t + Duration::from_secs(12)));
        assert_eq!(s.reading(t + Duration::from_secs(10) + STALL), Some(Reading::Stalled));
        // The same silence with a small file announced is a stall sooner.
        let mut s2 = Speed::start(t0);
        let mut p2 = run(100, 100 * 20 * MB);
        let t2 = files(&mut s2, &mut p2, t0, 12, 20 * MB, Duration::from_millis(500), 20 * MB);
        assert!(matches!(s2.reading(t2 + Duration::from_secs(5)), Some(Reading::Rate { .. })));
        assert_eq!(s2.reading(t2 + STALL + Duration::from_secs(1)), Some(Reading::Stalled));
        // A file finishing again reads on.
        let t3 = files(&mut s2, &mut p2, t2 + STALL + Duration::from_secs(1), 1, 20 * MB, Duration::ZERO, 20 * MB);
        assert!(matches!(s2.reading(t3), Some(Reading::Rate { .. })));
    }

    #[test]
    fn a_burst_of_small_files_is_smoothed_and_the_figure_holds_a_second() {
        let t0 = Instant::now();
        let mut s = Speed::start(t0);
        let mut p = run(1000, 1000 * 20 * MB);
        let t = files(&mut s, &mut p, t0, 20, 20 * MB, Duration::from_millis(500), 20 * MB);
        let Some(Reading::Rate { now: before, .. }) = s.reading(t) else { panic!() };
        // Ten 2 MB JPGs land within 200 ms: 100 MB/s for a moment.
        let t1 = files(&mut s, &mut p, t, 10, 2 * MB, Duration::from_millis(20), 20 * MB);
        let Some(Reading::Rate { now, steady }) = s.reading(t1) else { panic!() };
        assert!(now < before * 1.3, "a burst moves a 5 s average a little: {before} → {now}");
        assert!(steady <= now + 1.0, "{steady} {now}");
        // Within the same second the figure shown doesn't change.
        let shown = s.reading(t1);
        files(&mut s, &mut p, t1, 1, 50 * MB, Duration::from_millis(100), 20 * MB);
        assert_eq!(s.reading(t1 + Duration::from_millis(100)), shown);
    }

    #[test]
    fn renames_show_no_speed_at_all() {
        let t0 = Instant::now();
        let mut s = Speed::start(t0);
        let mut p = run(3, 60 * MB);
        for i in 1..=3 {
            p.files = i;
            p.bytes = i as u64 * 20 * MB;
            // transferred stays 0: same-device moves.
            s.observe(p, t0 + Duration::from_millis(i as u64 * 10));
        }
        assert_eq!(s.reading(t0 + Duration::from_millis(30)), None);
        assert_eq!(caption("3 of 3 files".into(), None, 0), "3 of 3 files");
    }

    #[test]
    fn the_caption_says_the_speed_and_the_time_left() {
        let rate = Some(Reading::Rate { now: 38e6, steady: 38e6 });
        assert_eq!(caption("12 of 40 files, 310 of 980 MB".into(), rate, 5_700_000_000), "12 of 40 files, 310 of 980 MB \u{b7} 38 MB/s \u{b7} about 3 minutes left");
        assert_eq!(caption("1 of 40 files".into(), Some(Reading::Starting), 1), "1 of 40 files \u{b7} starting\u{2026}");
        assert_eq!(caption("1 of 40 files".into(), Some(Reading::Stalled), 1), "1 of 40 files \u{b7} stalled");
    }
}
