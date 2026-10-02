//! Library search by what photos show (plan §5–§7): an index of one
//! vector per shot, built from the previews by the ML helper, and queries
//! answered from memory, best first.
//!
//! - [`engine`]: the one object the app holds.  Every call is a message to
//!   the engine's own thread; nothing here does file I/O on the caller's
//!   thread.  State comes back as [`Status`] snapshots
//!   (`BgEvent::SearchStatus`) and results as `BgEvent::SearchResults`.
//! - [`index`]: content keys that survive renames and moves, the path map,
//!   capture info and the vectors, under `~/.cache/photo_pipeline/search/`.
//! - [`pass`]: the indexing pass (the first full index when asked, then
//!   new exports on their own), cancellable, yielding to thumbnails.
//! - [`query`]: text → vector, scoring, the closeness cut-off, hits.
//! - [`vocab`]: words search treats specially (animal words, phase 2's
//!   tags later).
//! - [`estimate`]: the numbers the invitation and the dialog quote.
//!
//! Search is library only (decision 14).  The index and the models live in
//! the user's cache, never in the library; nothing is written to photos
//! or sidecars.

// See src/ml/mod.rs: unused until the search page lands.
#![allow(dead_code, unused_imports)]

pub mod engine;
pub mod estimate;
#[cfg(test)]
mod eval;
pub mod index;
pub mod pass;
pub mod query;
pub mod vocab;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub use engine::{Engine, Storage};
pub use query::{CLOSENESS_DEFAULT, CLOSENESS_MAX, CLOSENESS_MIN, Hit};

use crate::ml::{CardFailure, DeviceStatus};

/// What kind of pass is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassKind {
    /// The whole library, asked for from the search page.
    FirstIndex,
    /// Files new since the last pass (after an export), on their own.
    NewExports,
    /// Preferences ▸ Storage ▸ Rebuild: everything again, the old index
    /// answering until the new one is complete.
    Rebuild,
}

/// A running pass, as the status bar shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassProgress {
    pub kind: PassKind,
    /// Shots embedded, mapped or failed so far, of `total`.
    pub done: usize,
    pub total: usize,
    /// Newest days are indexed first: the oldest day reached so far, for
    /// "Searching the 18,200 photos indexed so far".
    pub oldest_day: Option<NaiveDate>,
}

/// Why indexing is stopped (the banner, §7a.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Stopped {
    /// The card was chosen and failed: nothing moves to the processor.
    Card(CardFailure),
    /// The library stopped answering; indexing carries on when it is back.
    LibraryAway { share: String, message: String },
    /// The helper couldn't be started or broke (a model file missing or
    /// damaged, the binary not runnable), in plain words.
    Helper { message: String },
    /// The user pressed ×.
    Cancelled,
}

/// A cheap snapshot of everything the views show about search.  Sent as
/// `BgEvent::SearchStatus` whenever any of it changes, and readable at any
/// time through [`Engine::status`] (a clone, no I/O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Status {
    /// Every file of the search model is in place.
    pub models_installed: bool,
    /// Shots in the library (from its index), and shots search has.
    pub library_shots: usize,
    pub indexed_shots: usize,
    /// The first index was asked for at some point (so new exports are
    /// indexed on their own from then on).
    pub first_index_asked: bool,
    pub pass: Option<PassProgress>,
    pub device: DeviceStatus,
    pub stopped: Option<Stopped>,
    /// New exports waiting because indexing is stopped (the card failed).
    pub waiting: usize,
    /// Times the helper stopped on the processor and was started again;
    /// the status bar notes each increase.
    pub helper_restarts: u32,
    /// Preferences ▸ *Index new exports*.
    pub index_exports: bool,
}

impl Status {
    /// The search page's state in §7a's words.
    pub fn is_indexing(&self) -> bool {
        self.pass.is_some()
    }

    /// Nothing indexed yet and no pass asked for: show the invitation.
    pub fn not_indexed(&self) -> bool {
        self.indexed_shots == 0 && !self.first_index_asked && self.pass.is_none()
    }
}
