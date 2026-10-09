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
//! - [`parse`]: the query syntax (quotes, `-word`, dates, `@name`), pure.
//! - [`vocab`]: the vocabulary: generic words with a kind (object, scene,
//!   person, fursuit, animal), their plural and the animal words fursuits
//!   are taken for.
//! - [`tags`]: hidden tags derived from the stored vectors against the
//!   vocabulary; *Things* and the ranking boost come from them.
//! - [`estimate`]: the numbers the invitation and the dialog quote.
//! - [`faces`]: people's faces (phase 3): the detections and face
//!   vectors cache beside the index, the catch-up, and the grouping into
//!   the *Subjects* page's groups (`crate::model::subjects` holds the
//!   pure rules and the user's file).
//!
//! Search is library only (decision 14).  The index and the models live in
//! the user's cache, never in the library; nothing is written to photos
//! or sidecars.

pub mod engine;
pub mod estimate;
#[cfg(test)]
mod eval;
pub mod faces;
pub mod heads;
pub mod index;
pub mod parse;
pub mod pass;
pub mod query;
pub mod step;
pub mod tags;
pub mod vocab;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub use engine::{Engine, Storage};
pub use parse::{Parsed, parse};
pub use query::{CLOSENESS_DEFAULT, CLOSENESS_MAX, CLOSENESS_MIN, Hit};
pub use vocab::Kind;

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
    /// Faces over photos already indexed: the catch-up the People tab
    /// asks for, or the face vectors again after a face-model change
    /// (phase 3).  Reads previews, writes the faces cache, never the
    /// search index.
    Faces,
}

/// Where finding people's faces stands (phase 3, decision 9).  Part of
/// [`Status`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FacesStatus {
    /// Preferences ▸ People ▸ *Find people's faces* (on by default).
    pub on: bool,
    /// The chosen face model (Preferences ▸ People ▸ *Face model*).
    pub model: Option<crate::ml::models::ModelId>,
    /// The detector's and the chosen model's files are in place.
    pub installed: bool,
    /// Indexed shots that have been looked at for faces (with the
    /// detector; whichever model recognised them).  Below
    /// `indexed_shots` means a catch-up is on offer.
    pub looked_at: usize,
    /// Shots whose faces still want vectors from the chosen model (after
    /// a model change); zero otherwise.
    pub unrecognised: usize,
    /// Faces found (score and size above the quality gate).
    pub found: usize,
    /// People named.
    pub named: usize,
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
    /// Shots whose file the helper couldn't read (empty, damaged, not a
    /// photo): skipped until the file changes or the index is rebuilt.
    pub unreadable: usize,
    /// Times the helper stopped on the processor and was started again;
    /// the status bar notes each increase.
    pub helper_restarts: u32,
    /// Preferences ▸ *Index new exports*.
    pub index_exports: bool,
    /// People's faces (phase 3).
    pub faces: FacesStatus,
    /// The fursuit-head step (phase 4): on or off, how far, what it waits
    /// for.
    pub heads: heads::HeadsStatus,
    /// The fursuit close-call judge (fursuit matching v2).
    pub judge: crate::ml::judge::JudgeStatus,
}

/// One entry of *Things in your photos*: a vocabulary word with how many
/// indexed photos carry it as a hidden tag.  The engine's list holds
/// objects and scenes only (people, fursuits and pets never appear; they
/// get pages of their own), most photos first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thing {
    /// The word as the list shows it and a query writes it: the plural
    /// ("cameras", "fireworks", "food").
    pub word: String,
    pub photos: usize,
    /// Never a subject kind in the engine's list; a second guard for the
    /// views.
    pub kind: Kind,
}

/// The Things offered while `typed` is in the field: those whose word
/// (singular or plural) starts with the last word typed, in the list's
/// order.  Pure, for the UI thread; nothing for an empty field.
pub fn complete_things(things: &[Thing], typed: &str) -> Vec<Thing> {
    let Some(last) = typed.split_whitespace().last().map(str::to_lowercase) else { return Vec::new() };
    things
        .iter()
        .filter(|t| {
            let w = t.word.to_lowercase();
            w.starts_with(&last) || vocab::find(&t.word).is_some_and(|i| vocab::word(i).word.starts_with(&last))
        })
        .cloned()
        .collect()
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
