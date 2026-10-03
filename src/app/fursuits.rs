//! Fursuits as the pages see them (decision 11): the fursuit-head step's
//! state, what it costs, and when it must ask first.
//!
//! - **Semi-automatic, scanned during indexing.**  Heads are looked for on
//!   the photos the index tags as fursuits, as a step after search (and
//!   faces); the user names a fursuit from one group and confirms
//!   suggestions with Y and N.  Groups, naming, the review and the rest
//!   are the subjects pages' own, on this kind's data.
//! - **On the processor it asks, every time.**  A head scan is long there
//!   (about 15 hours for this library, over an hour an event day), so with
//!   *Processor* chosen no scan starts on its own: each one waits as an
//!   [`Offer`] with its cost until the user starts or skips it.
//! - **A card that fails says so.**  The step stops and the banner says
//!   what failed; nothing moves to the processor, and nothing here offers it.
//!
//! The shapes here stand in for the backend's until its fursuit API lands;
//! the pages import them from here only, so the switch is one place.

use crate::ml::{CardFailure, Device};
use crate::search::estimate;

/// Seconds per fursuit photo for the head step (OWLv2 to find heads, then
/// DINOv2 on each head): the study's 0.12 s on the card (about 1.4 hours
/// for the library's ~42 k fursuit photos) and 1.3 s on the processor
/// (about 15 hours).
pub const HEAD_SECONDS_GPU: f64 = 0.12;
pub const HEAD_SECONDS_CPU: f64 = 1.3;
/// Fursuit photos in a busy event day (sample E2's day held ~3 500).
pub const EVENT_DAY_PHOTOS: usize = 3_500;
/// The head models together (OWLv2 and DINOv2), until the registry's rows
/// say otherwise.
pub const HEAD_MODELS_BYTES: u64 = 950_000_000;

/// Seconds to look for heads in `photos` fursuit photos on `device`.
pub fn head_seconds(photos: usize, device: Device) -> f64 {
    photos as f64
        * match device {
            Device::Gpu => HEAD_SECONDS_GPU,
            Device::Cpu => HEAD_SECONDS_CPU,
        }
}

/// Bytes read from the library to look at `photos` again (each preview).
pub fn head_bytes_read(photos: usize) -> u64 {
    estimate::bytes_read(photos)
}

/// "about 1.4 hours with the graphics card", "about 15 hours on the
/// processor".
pub fn describe_cost(photos: usize, device: Device) -> String {
    let time = estimate::describe_seconds(head_seconds(photos, device));
    format!("{time} {} {}", if device == Device::Gpu { "with" } else { "on" }, device.phrase())
}

/// Why a head scan would start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The first index reached its fursuit-head step.
    FirstIndex,
    /// An export added photos with fursuits.
    NewExports,
    /// The library was indexed before heads were looked for (asked from
    /// the Fursuits tab or Preferences).
    CatchUp,
    /// Carrying on after a stop (the library came back, the card works).
    Resume,
}

/// A head scan waiting for the user's answer (the processor is chosen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub photos: usize,
    pub why: Why,
}

/// Where the head step is.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Step {
    /// Nothing running and nothing waiting.
    #[default]
    Idle,
    /// A scan would start, and waits for start or skip (the processor).
    Asking(Offer),
    /// Looking for heads: `done` of `total` fursuit photos.
    Scanning { why: Why, done: usize, total: usize },
    /// The card failed: the step is stopped, and says why.
    Failed(CardFailure),
}

/// Fursuits, as the tab, Preferences and the status bar show them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeadsStatus {
    /// Preferences ▸ *Find fursuit heads*.
    pub on: bool,
    /// OWLv2 and DINOv2 are in place.
    pub models_installed: bool,
    /// Indexed photos the index tags as fursuits, and how many of them were
    /// looked at for heads.
    pub tagged: usize,
    pub scanned: usize,
    /// Skipped scans' photos, waiting for a later catch-up.
    pub skipped: usize,
    /// Heads found, and fursuits named.
    pub found: usize,
    pub named: usize,
    pub step: Step,
}

impl HeadsStatus {
    /// Fursuit photos not looked at for heads (indexed before, or skipped):
    /// the tab and Preferences offer them, with their cost.
    pub fn unscanned(&self) -> usize {
        self.tagged.saturating_sub(self.scanned)
    }

    /// The tab offers to look (and says what it costs) instead of running.
    pub fn catch_up_wanted(&self) -> bool {
        self.on && self.unscanned() > 0 && matches!(self.step, Step::Idle)
    }

    pub fn scanning(&self) -> bool {
        matches!(self.step, Step::Scanning { .. })
    }

    pub fn asking(&self) -> Option<Offer> {
        match self.step {
            Step::Asking(o) => Some(o),
            _ => None,
        }
    }
}

/// Whether a head scan of `photos` may start without asking: only on the
/// graphics card.  On the processor every scan asks first, with its cost
/// (decision 11, "each time").
pub fn starts_unasked(device: Device) -> bool {
    device == Device::Gpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_costs_match_the_plan() {
        // The library's fursuit photos: ~1.4 hours on the card, ~15 on the
        // processor; an event day minutes on the card, over an hour on the
        // processor.
        let n = 42_180;
        assert_eq!(estimate::describe_seconds(head_seconds(n, Device::Gpu)), "about 1.4 hours");
        assert_eq!(estimate::describe_seconds(head_seconds(n, Device::Cpu)), "about 15 hours");
        assert_eq!(estimate::describe_seconds(head_seconds(EVENT_DAY_PHOTOS, Device::Gpu)), "about 7 minutes");
        assert!(head_seconds(EVENT_DAY_PHOTOS, Device::Cpu) > 3600.0, "over an hour a day on the processor");
        assert_eq!(describe_cost(n, Device::Cpu), "about 15 hours on the processor");
        assert_eq!(describe_cost(n, Device::Gpu), "about 1.4 hours with the graphics card");
        assert_eq!(estimate::describe_bytes(head_bytes_read(n)), "about 15 GB");
    }

    #[test]
    fn only_the_card_starts_a_scan_unasked() {
        assert!(starts_unasked(Device::Gpu));
        assert!(!starts_unasked(Device::Cpu), "the processor asks every time");
    }

    #[test]
    fn unscanned_photos_are_offered_only_when_nothing_runs_or_waits() {
        let mut s = HeadsStatus { on: true, tagged: 400, scanned: 100, ..HeadsStatus::default() };
        assert_eq!(s.unscanned(), 300);
        assert!(s.catch_up_wanted());
        s.step = Step::Scanning { why: Why::CatchUp, done: 10, total: 300 };
        assert!(!s.catch_up_wanted() && s.scanning());
        s.step = Step::Asking(Offer { photos: 300, why: Why::NewExports });
        assert!(!s.catch_up_wanted());
        assert_eq!(s.asking(), Some(Offer { photos: 300, why: Why::NewExports }));
        s.step = Step::Idle;
        s.on = false;
        assert!(!s.catch_up_wanted(), "off: nothing offered");
    }
}
