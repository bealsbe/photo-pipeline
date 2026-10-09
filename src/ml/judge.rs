//! The fursuit close-call judge (v2, `docs/library-search-plan.md` "As
//! built — v2"): a vision language model that looks at two head crops
//! the whitened vectors can't tell apart and says whether they show the
//! same suit.  This module is the choice and the state the pages read;
//! the model itself runs in the judge helper (`judge_worker`), a process
//! of its own on the graphics card alone, spoken to through
//! [`judge_client`].
//!
//! **The rules.**  Fursuits only: nothing here ever sees a face or a pet.
//! Suggestions only: the judge reorders and filters what the review and
//! *Find more* offer; nothing is named, merged or marked by it.  Card only:
//! every layer is on the card, or the judge stops and says so; nothing
//! moves to the processor.  It loads on demand, never while the search
//! helper's pass or head step is using the card, and leaves after two
//! idle minutes so its memory goes back.
//!
//! [`judge_worker`]: super::judge_worker
//! [`judge_client`]: super::judge_client

use serde::{Deserialize, Serialize};

use super::CardFailure;
use super::models::ModelId;
use crate::model::subjects::GroupId;

/// Preferences ▸ Fursuits ▸ *Close-call judge*: a choice list, so a second
/// judge (the user may pick Qwen3-VL-Reranker after its comparison) slots
/// in as a variant with its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JudgeChoice {
    /// No judge: the review and *Find more* use the vector cut-offs alone.
    #[default]
    Off,
    /// Gemma 4 26B-A4B-it, the research's pick.
    Gemma4,
}

impl JudgeChoice {
    /// Every choice, in the list's order.
    pub const ALL: [JudgeChoice; 2] = [JudgeChoice::Off, JudgeChoice::Gemma4];

    /// The list's label.
    pub fn label(self) -> &'static str {
        match self {
            JudgeChoice::Off => "Off",
            JudgeChoice::Gemma4 => "Gemma 4",
        }
    }

    /// The line under the label.
    pub fn description(self) -> &'static str {
        match self {
            JudgeChoice::Off => "Close calls are left to the vector cut-offs.",
            JudgeChoice::Gemma4 => "About 18 GB to download; about 2 s a close call on the graphics card.",
        }
    }

    /// The model the choice runs, if any.
    pub fn model(self) -> Option<ModelId> {
        match self {
            JudgeChoice::Off => None,
            JudgeChoice::Gemma4 => Some(ModelId::Gemma4),
        }
    }

    /// The choice that runs `model`, if any.
    pub fn for_model(model: ModelId) -> Option<JudgeChoice> {
        JudgeChoice::ALL.iter().copied().find(|c| c.model() == Some(model))
    }
}

/// Where the judge is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum JudgeState {
    /// Nothing to judge (loaded or not: see [`JudgeStatus::loaded`]).
    #[default]
    Idle,
    /// Close calls wait: the search helper's pass or head step holds the
    /// card; the judge loads when it is done.
    WaitingForCard,
    /// The model is loading onto the card (about ten seconds warm).
    Loading,
    /// Judging a group's close calls, nearest first: `done` of `total`.
    Judging { group: GroupId, done: usize, total: usize },
    /// The card failed, or the model doesn't fit on it: the judge is
    /// stopped and says why; nothing moves to the processor.
    Failed(CardFailure),
    /// The helper couldn't run for another reason (a model file missing
    /// or damaged, the binary not runnable), in plain words.
    Broken(String),
}

/// The judge as Preferences, the review and the status bar show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct JudgeStatus {
    /// Preferences ▸ Fursuits ▸ *Close-call judge*.
    pub choice: JudgeChoice,
    /// The judges whose files are in place, whatever is chosen.
    pub downloaded: Vec<JudgeChoice>,
    /// The chosen judge's model is on the card right now.
    pub loaded: bool,
    pub state: JudgeState,
}

impl JudgeStatus {
    /// The chosen judge's files are in place.
    pub fn installed(&self) -> bool {
        self.choice != JudgeChoice::Off && self.downloaded.contains(&self.choice)
    }

    /// A judge is chosen and ready to be used.
    pub fn usable(&self) -> bool {
        self.installed() && !matches!(self.state, JudgeState::Failed(_) | JudgeState::Broken(_))
    }

    pub fn judging(&self) -> bool {
        matches!(self.state, JudgeState::Judging { .. } | JudgeState::Loading | JudgeState::WaitingForCard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_list_starts_off_and_names_its_model() {
        assert_eq!(JudgeChoice::default(), JudgeChoice::Off, "off by default: nothing downloads or loads unasked");
        assert_eq!(JudgeChoice::ALL[0], JudgeChoice::Off);
        assert_eq!(JudgeChoice::Off.model(), None);
        assert_eq!(JudgeChoice::Gemma4.model(), Some(ModelId::Gemma4));
        assert_eq!(JudgeChoice::for_model(ModelId::Gemma4), Some(JudgeChoice::Gemma4));
        assert_eq!(JudgeChoice::for_model(ModelId::Owlv2), None);
        assert_eq!(serde_json::to_string(&JudgeChoice::Gemma4).unwrap(), "\"gemma4\"");
        assert_eq!(serde_json::from_str::<JudgeChoice>("\"off\"").unwrap(), JudgeChoice::Off);
        assert!(JudgeChoice::ALL.iter().all(|c| !c.label().is_empty() && !c.description().is_empty()));
        assert!(JudgeChoice::ALL.iter().all(|c| c.model().is_none_or(|m| m.is_judge_model())));
    }

    #[test]
    fn the_status_says_when_the_chosen_judge_is_in_place() {
        let mut s = JudgeStatus::default();
        assert!(!s.installed() && !s.usable() && !s.judging());
        s.downloaded = vec![JudgeChoice::Gemma4];
        assert!(!s.installed(), "downloaded but Off is not installed");
        s.choice = JudgeChoice::Gemma4;
        assert!(s.installed() && s.usable());
        s.state = JudgeState::Failed(CardFailure::NoCard);
        assert!(s.installed() && !s.usable());
        s.state = JudgeState::WaitingForCard;
        assert!(s.judging());
        s.state = JudgeState::Judging { group: GroupId::Cluster(1), done: 2, total: 5 };
        assert!(s.judging());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<JudgeStatus>(&json).unwrap(), s);
    }
}
