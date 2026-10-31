//! Machine-learning models for library search (and later people, fursuits
//! and pets): where they come from, where they run, and the helper process
//! that runs them.
//!
//! - [`models`]: the registry (pinned URLs, SHA-256, sizes, licences) and
//!   where downloaded files live (`~/.cache/photo_pipeline/models/`).
//! - [`download`]: download with progress and a checksum check, renamed
//!   into place only when the digest matches.
//! - [`worker`]: the helper process (`photo-pipeline --ml-worker`), the same
//!   binary started with a flag: ONNX Runtime sessions on the chosen
//!   device, the self-check against the processor, batching, idle exit.
//! - [`client`]: starts the helper, speaks its pipe protocol, applies the
//!   restart rule, and reports card failures.
//! - [`prep`]: a photo's preview as the model's input.
//!
//! **The device rule**: where models run is the user's choice
//! ([`Device`]), the graphics card by default.  A card that fails, in any
//! way, stops the work and is reported ([`CardFailure`]); nothing here ever
//! moves work to the processor on its own.

pub mod client;
pub mod download;
pub mod face;
pub mod gpu;
pub mod heads;
pub mod judge;
pub mod judge_client;
pub mod judge_worker;
pub mod models;
pub mod onnx_patch;
pub mod prep;
pub mod unzip;
pub mod worker;

use serde::{Deserialize, Serialize};

/// Where models run: Preferences ▸ Search ▸ *Run models on*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    /// WebGPU over Vulkan, after a self-check against the processor.
    #[default]
    Gpu,
    /// The CPU, only when chosen.
    Cpu,
}

impl Device {
    /// The Preferences label.
    pub fn label(self) -> &'static str {
        match self {
            Device::Gpu => "Graphics card",
            Device::Cpu => "Processor",
        }
    }

    /// The word used mid-sentence ("about 7 minutes with the graphics card").
    pub fn phrase(self) -> &'static str {
        match self {
            Device::Gpu => "the graphics card",
            Device::Cpu => "the processor",
        }
    }
}

/// Why the graphics card can't be used.  Reported, never worked around:
/// heavy work stops where it is and the processor is used only when the
/// user chooses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CardFailure {
    /// No Vulkan adapter the runtime can use.
    NoCard,
    /// The card's answers differed from the processor's beyond the
    /// tolerance ([`SelfCheck`]).
    SelfCheckFailed { cosine: f32, max_diff: f32 },
    /// The driver brought the helper down (twice in a row while running on
    /// the card).
    Crashed,
    /// The helper couldn't be started or couldn't load a model on the card.
    Failed(String),
}

impl CardFailure {
    /// The banner's first line.
    pub fn title(&self) -> &'static str {
        match self {
            CardFailure::NoCard => "Indexing stopped: there's no graphics card Photo Pipeline can use",
            CardFailure::SelfCheckFailed { .. } => {
                "Indexing stopped: the graphics card gave different answers from the processor, so it isn't used"
            }
            CardFailure::Crashed => "Indexing stopped: the graphics card stopped responding",
            CardFailure::Failed(_) => "Indexing stopped: the graphics card couldn't be used",
        }
    }

    /// The banner's detail, after the title.
    pub fn detail(&self) -> String {
        match self {
            CardFailure::NoCard => "No Vulkan graphics card was found.".into(),
            // Plain words for the banner (the measured agreement stays in
            // the status, for Preferences and the profile log).
            CardFailure::SelfCheckFailed { .. } => "Its answers didn't match the processor's on a test, so its results can't be trusted.".into(),
            CardFailure::Crashed => "Its driver stopped the search helper.".into(),
            CardFailure::Failed(why) => why.clone(),
        }
    }
}

/// The result of running fixed inputs through a model on the card and on
/// the processor and comparing them.  Done once the model is in place, and
/// again whenever the app, the driver or the model changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum SelfCheck {
    #[default]
    NotRun,
    Passed { cosine: f32, max_diff: f32 },
    Failed { cosine: f32, max_diff: f32 },
}

/// The device as the status bar and Preferences show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DeviceStatus {
    /// What Preferences says.
    pub chosen: Device,
    /// The adapter's name once the helper has reported it ("AMD Radeon …").
    pub name: Option<String>,
    /// The last self-check of the chosen card.
    pub self_check: SelfCheck,
}

/// The closest two unit vectors may be and still count as the same answer
/// from the two devices, and the largest difference in any one component.
/// Measured on a development machine: cosine 1.000000, largest difference 2.7e-5;
/// the tolerance leaves room for other drivers and for fp16 weights.
/// *Provisional* until the fp16 models are measured.
pub const SELF_CHECK_MIN_COSINE: f32 = 0.995;
pub const SELF_CHECK_MAX_DIFF: f32 = 0.02;

/// Judge a self-check's numbers.
pub fn self_check_passes(cosine: f32, max_diff: f32) -> bool {
    cosine.is_finite() && max_diff.is_finite() && cosine >= SELF_CHECK_MIN_COSINE && max_diff <= SELF_CHECK_MAX_DIFF
}

/// `PP_ML_FAKE_CARD`, for tests: makes the graphics card fail in the named
/// way without touching a real device.  Values: `no-card`, `self-check`,
/// `crash`.  Anything else, or unset: the real card.
pub fn fake_card_failure() -> Option<CardFailure> {
    match std::env::var("PP_ML_FAKE_CARD").ok()?.as_str() {
        "no-card" => Some(CardFailure::NoCard),
        "self-check" => Some(CardFailure::SelfCheckFailed { cosine: 0.42, max_diff: 0.9 }),
        "crash" => Some(CardFailure::Crashed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_serialise_as_short_words() {
        assert_eq!(serde_json::to_string(&Device::Gpu).unwrap(), "\"gpu\"");
        assert_eq!(serde_json::from_str::<Device>("\"cpu\"").unwrap(), Device::Cpu);
        assert_eq!(Device::default(), Device::Gpu, "the graphics card is the default");
    }

    #[test]
    fn the_self_check_tolerance_accepts_the_measured_agreement() {
        assert!(self_check_passes(1.0, 2.7e-5));
        assert!(!self_check_passes(0.9, 0.0));
        assert!(!self_check_passes(1.0, 0.5));
        assert!(!self_check_passes(f32::NAN, 0.0));
    }
}
