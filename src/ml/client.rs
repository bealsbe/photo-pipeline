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
//! can say so.  The client never starts a processor helper for image work
//! unless the processor is the chosen device.

use std::path::PathBuf;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use super::{CardFailure, Device, SelfCheck};
use crate::model::record::{FileType, ShotMeta};
use crate::search::index::Key;

/// What the helper found in one photo: its content key and capture info
/// (from the header it read for the preview) and the model's vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedding {
    pub key: Key,
    pub capture: Option<NaiveDateTime>,
    pub meta: ShotMeta,
    /// The frame looks accidental (a lens cap, a pocket, the floor): kept
    /// in the index but left out of results.
    pub accidental: bool,
    /// Unit-length, `dim` long.
    pub vector: Vec<f32>,
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
pub trait Embedder: Send {
    /// The device this embedder runs image work on.
    fn device(&self) -> Device;
    /// The adapter's name and the self-check, once known.
    fn device_report(&self) -> (Option<String>, SelfCheck);
    /// Embed a batch of photos (a pair's JPG, else the file).
    fn embed_images(&mut self, files: &[(PathBuf, FileType)]) -> Result<Vec<ImageResult>, HelperError>;
    /// Encode query texts (always on the processor, whatever the device).
    fn encode_text(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError>;
}

/// Why a helper couldn't be started.
#[derive(Debug, Clone, PartialEq)]
pub enum StartError {
    /// The card was chosen and can't be used.
    Card(CardFailure),
    /// The models aren't in place, the binary couldn't be run, …
    Other(String),
}

/// Makes an [`Embedder`] for a device: the real helper in the app, a
/// double in tests.  The engine calls it with the chosen device only.
pub type EmbedderFactory = Box<dyn Fn(Device) -> Result<Box<dyn Embedder>, StartError> + Send>;

/// The helper process as an [`Embedder`].
pub struct Client {
    device: Device,
    name: Option<String>,
    self_check: SelfCheck,
}

impl Client {
    /// Start a helper on `device` and wait for it to load the models and,
    /// on the card, pass the self-check.  `PP_ML_FAKE_CARD` makes the card
    /// fail for tests ([`super::fake_card_failure`]).
    pub fn start(device: Device) -> Result<Client, StartError> {
        if device == Device::Gpu {
            if let Some(f) = super::fake_card_failure() {
                return Err(StartError::Card(f));
            }
        }
        // Phase 1 fills this in: spawn `photo-pipeline --ml-worker`, read
        // its ready message (adapter name, self-check).
        Err(StartError::Other("the search helper isn't built yet".into()))
    }

    /// The factory the app uses.
    pub fn factory() -> EmbedderFactory {
        Box::new(|device| Client::start(device).map(|c| Box::new(c) as Box<dyn Embedder>))
    }
}

impl Embedder for Client {
    fn device(&self) -> Device {
        self.device
    }

    fn device_report(&self) -> (Option<String>, SelfCheck) {
        (self.name.clone(), self.self_check.clone())
    }

    fn embed_images(&mut self, _files: &[(PathBuf, FileType)]) -> Result<Vec<ImageResult>, HelperError> {
        Err(HelperError::Broken("the search helper isn't built yet".into()))
    }

    fn encode_text(&mut self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
        Err(HelperError::Broken("the search helper isn't built yet".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_card_failure_stops_the_card_helper_before_it_starts() {
        // (The variable is read at call time, so set it for this test only.)
        temp_env("PP_ML_FAKE_CARD", "self-check", || {
            assert!(matches!(Client::start(Device::Gpu), Err(StartError::Card(CardFailure::SelfCheckFailed { .. }))));
            assert!(super::super::fake_card_failure().is_some());
        });
        temp_env("PP_ML_FAKE_CARD", "no-card", || {
            assert_eq!(Client::start(Device::Gpu).err(), Some(StartError::Card(CardFailure::NoCard)));
        });
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
