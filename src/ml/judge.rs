//! PROVISIONAL (UI side, until the backend's lands): the fursuit
//! close-call judge's choice and status, with the names agreed with the
//! backend.  The judge is off by default, runs on the card only, and only
//! suggests: nothing is named, merged or marked by it.  Fursuits only.

use serde::{Deserialize, Serialize};

use crate::ml::CardFailure;
use crate::ml::models::ModelId;
use crate::model::subjects::GroupId;

/// Preferences ▸ Fursuits ▸ *Close-call judge*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeChoice {
    /// Close calls are left out, as before the judge.
    #[default]
    Off,
    /// Gemma 4 26B-A4B (unsloth's UD-Q4_K_M GGUF and its vision projector).
    Gemma4,
}

impl JudgeChoice {
    /// Every choice, in the list's order (Off first).
    pub const ALL: [JudgeChoice; 2] = [JudgeChoice::Off, JudgeChoice::Gemma4];

    pub fn label(self) -> &'static str {
        match self {
            JudgeChoice::Off => "Off",
            JudgeChoice::Gemma4 => "Gemma 4",
        }
    }

    /// The model it runs, if any.
    pub fn model(self) -> Option<ModelId> {
        match self {
            JudgeChoice::Off => None,
            JudgeChoice::Gemma4 => Some(ModelId::Gemma4),
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            JudgeChoice::Off => "",
            JudgeChoice::Gemma4 => "about 18 GB, about 1 s per close call",
        }
    }
}

/// What the judge is doing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum JudgeState {
    /// Nothing asked of it (loaded or not).
    #[default]
    Idle,
    /// Close calls wait: a pass or the head step is using the card.
    WaitingForCard,
    /// Putting the model on the card.
    Loading,
    /// Judging `group`'s close calls: `done` of `total`.
    Judging { group: GroupId, done: usize, total: usize },
    /// The card failed: the judge is stopped and says why.
    Failed(CardFailure),
    /// Not the card: a model file missing or damaged, the helper not
    /// starting; in plain words.
    Broken(String),
}

/// The judge, as Preferences, the review and the status bar show it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JudgeStatus {
    /// The judge the engine runs with (Preferences' choice).
    pub choice: JudgeChoice,
    /// Every judge whose files are in place, whatever is chosen.
    pub downloaded: Vec<JudgeChoice>,
    /// Its model is on the card now.
    pub loaded: bool,
    pub state: JudgeState,
}

impl JudgeStatus {
    /// The chosen judge's files are in place.
    pub fn installed(&self) -> bool {
        self.choice != JudgeChoice::Off && self.downloaded.contains(&self.choice)
    }
}
