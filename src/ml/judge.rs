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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::CardFailure;
use super::client::FileError;
use super::models::ModelId;
use crate::imaging::decode::{self, Pixels};
use crate::model::record::FileType;
use crate::model::subjects::GroupId;
use crate::search::index::{KEY_HEAD_BYTES, Key};

/// Preferences ▸ Fursuits ▸ *Close-call judge*: a choice list.  Each judge
/// runs through the same helper and runtime with its own prompt, image
/// budget, scoring and threshold (below), all as the research measured
/// them; only one is on the card at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JudgeChoice {
    /// No judge: the review and *Find more* use the vector cut-offs alone.
    #[default]
    Off,
    /// Qwen3-VL-Reranker-8B (Q8_0): the user's pick to list first
    /// (2026-10-09): as right as Gemma on both test sets, twice as fast,
    /// half the memory.
    Qwen3VlReranker,
    /// Gemma 4 26B-A4B-it, the research's first pick.
    Gemma4,
}

impl JudgeChoice {
    /// Every choice, in the list's order.
    pub const ALL: [JudgeChoice; 3] = [JudgeChoice::Off, JudgeChoice::Qwen3VlReranker, JudgeChoice::Gemma4];

    /// The list's label.
    pub fn label(self) -> &'static str {
        match self {
            JudgeChoice::Off => "Off",
            JudgeChoice::Qwen3VlReranker => "Qwen3-VL-Reranker",
            JudgeChoice::Gemma4 => "Gemma 4",
        }
    }

    /// The line under the label.
    pub fn description(self) -> &'static str {
        match self {
            JudgeChoice::Off => "Close calls are left to the vector cut-offs.",
            JudgeChoice::Qwen3VlReranker => "About 10 GB to download; about 1 s a close call on the graphics card.",
            JudgeChoice::Gemma4 => "About 18 GB to download; about 2 s a close call on the graphics card.",
        }
    }

    /// The model the choice runs, if any.
    pub fn model(self) -> Option<ModelId> {
        match self {
            JudgeChoice::Off => None,
            JudgeChoice::Qwen3VlReranker => Some(ModelId::Qwen3VlReranker),
            JudgeChoice::Gemma4 => Some(ModelId::Gemma4),
        }
    }

    /// Image tokens per head crop, the minimum and the maximum: the
    /// research's pick per judge.
    pub fn image_tokens(self) -> i32 {
        match self {
            JudgeChoice::Qwen3VlReranker => QWEN_IMAGE_TOKENS,
            _ => IMAGE_TOKENS,
        }
    }

    /// A pair at or above this mean P(yes) is the same suit.
    pub fn yes_cut(self) -> f32 {
        match self {
            JudgeChoice::Qwen3VlReranker => QWEN_P_YES_CUT,
            _ => P_YES_CUT,
        }
    }

    /// Candidate answer tokens read (with their log-probabilities).
    pub fn top_tokens(self) -> usize {
        match self {
            JudgeChoice::Qwen3VlReranker => QWEN_TOP_TOKENS,
            _ => TOP_TOKENS,
        }
    }

    /// The whole prompt, with `marker` where each image goes (query then
    /// candidate).
    pub fn prompt(self, marker: &str) -> String {
        match self {
            JudgeChoice::Qwen3VlReranker => qwen_prompt(marker),
            _ => prompt(marker),
        }
    }

    /// P(yes) from the first answer token's candidates.
    pub fn score(self, top: &[(String, f32)]) -> f32 {
        match self {
            JudgeChoice::Qwen3VlReranker => p_rerank(top),
            _ => p_yes(top),
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

    /// The judge is used for the next close calls: chosen, in place,
    /// not failed, and the graphics card is the chosen device (card
    /// only: with the processor chosen, close calls use the vector
    /// cut-offs as with *Off*).
    pub fn active(&self, device: super::Device) -> bool {
        self.usable() && device == super::Device::Gpu
    }

    pub fn judging(&self) -> bool {
        matches!(self.state, JudgeState::Judging { .. } | JudgeState::Loading | JudgeState::WaitingForCard)
    }
}

// ---- the research's pick, as the helper runs it ----------------------

/// The question, word for word as the research asked it (prompt 0).
pub const QUESTION: &str = "These are two photos of fursuit heads. Is this the same fursuit (the same costume or character), regardless of lighting, pose or photo? Answer yes or no.";
/// Image tokens per head crop (the research's pick: 560, both minimum
/// and maximum).
pub const IMAGE_TOKENS: i32 = 560;
/// The context, the logical and the physical batch, as the research's
/// server ran (`-c 4096 -b 2048 -ub 1024`): the physical batch must hold
/// an image's tokens whole (Gemma attends over an image non-causally).
pub const N_CTX: i32 = 4096;
pub const N_BATCH: i32 = 2048;
pub const N_UBATCH: i32 = 1024;
/// Most likely first tokens read for the answer.
pub const TOP_TOKENS: usize = 10;
/// A judged pair at or above this mean P(yes) is the same suit (the
/// research's threshold, chosen on even days).
pub const P_YES_CUT: f32 = 0.6;
/// The head crop the judge sees: the app's square (the box widened by
/// `heads::CROP_MARGIN` on each side, black beyond the photo's edge),
/// cut from the photo at a size at least this, resized to it, JPEG at
/// [`CROP_QUALITY`], as the research's `extract_crops.py` made them.
pub const CROP_PX: u32 = 448;
pub const CROP_QUALITY: u8 = 92;
/// Minutes without a question after which the helper exits and the
/// card's memory goes back.
pub const IDLE_SECS: u64 = 120;

/// Qwen3-VL-Reranker's instruction (the research's instruct 0).
pub const QWEN_INSTRUCT: &str = "Is the candidate the same fursuit (same costume or character) as the query, regardless of lighting, pose or photo?";
/// Its system line, from the official reranker code.
pub const QWEN_SYSTEM: &str = "Judge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".";
/// Image tokens per head crop for Qwen (the research's pick: 280; 560
/// was no better and three times slower).
pub const QWEN_IMAGE_TOKENS: i32 = 280;
/// Candidates read for Qwen's answer: "yes" and "no" are found among
/// them (a missing one counts as likely as the least likely read).
pub const QWEN_TOP_TOKENS: usize = 100;
/// Qwen says "same suit" at P ≥ 0.45 (mean of both orders).  A narrow
/// good zone: 0.40–0.50 held on the relabel's 791 pairs, its odd-day
/// hold-out and the confirmation round, with a cliff above 0.55.  It
/// must be derived again if the prompt, the crop or the image budget
/// change.
pub const QWEN_P_YES_CUT: f32 = 0.45;

/// Qwen3-VL-Reranker's official chat template, as the research rendered
/// it for llama.cpp (no BOS: Qwen's vocabulary adds none; libmtmd puts the
/// vision start and end tokens around each image itself).
pub fn qwen_prompt(marker: &str) -> String {
    format!("<|im_start|>system\n{QWEN_SYSTEM}<|im_end|>\n<|im_start|>user\n<Instruct>: {QWEN_INSTRUCT}<Query>:{marker}\n<Document>:{marker}<|im_end|>\n<|im_start|>assistant\n")
}

/// The reranker's score as its official code computes it:
/// sigmoid(logit "yes" − logit "no") at the last position, which equals
/// P(yes) / (P(yes) + P(no)) from the softmax.  Read from the candidates
/// as the research did: the tokens "yes" and "no" exactly; one not among
/// them counts as likely as the least likely read.
pub fn p_rerank(top: &[(String, f32)]) -> f32 {
    let floor = top.iter().map(|t| t.1).fold(f32::INFINITY, f32::min);
    if !floor.is_finite() {
        return 0.5;
    }
    let find = |w: &str| top.iter().find(|t| t.0 == w).map_or(floor, |t| t.1);
    let (ly, ln) = (find("yes"), find("no"));
    (1.0 / (1.0 + ((ln - ly) as f64).exp())) as f32
}

/// Gemma 4's chat template with thinking off, as llama-server renders
/// one user turn of two images and the question (BOS is added by the
/// tokenizer, as the server does after stripping it from the template).
pub fn prompt(marker: &str) -> String {
    format!("<|turn>user\n{marker}{marker}{QUESTION}<turn|>\n<|turn>model\n<|channel>thought\n<channel|>")
}

/// P(yes) from the first answer token's most likely candidates (their
/// text and log-probability), as the research scored it: the
/// probability of tokens reading "yes" (trimmed, lower-cased, leading
/// `*` dropped) over that of "yes" and "no" together; 0.5 when neither
/// is among them.
pub fn p_yes(top: &[(String, f32)]) -> f32 {
    let (mut yes, mut no) = (0f64, 0f64);
    for (piece, lp) in top {
        let t = piece.trim().to_lowercase();
        let t = t.trim_start_matches('*');
        if t.starts_with("yes") {
            yes += (*lp as f64).exp();
        } else if t.starts_with("no") {
            no += (*lp as f64).exp();
        }
    }
    if yes + no > 0.0 { (yes / (yes + no)) as f32 } else { 0.5 }
}

/// One head as the judge is shown it: where its photo is, the content
/// key it had when the head was found (a photo that changed since is
/// refused, never judged), and its box as a fraction of the oriented
/// preview.  `File` is a crop already cut (the research's crops, for
/// checking the helper against the research's own scores).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Crop {
    Head { path: PathBuf, file_type: FileType, key: Key, bbox: [f32; 4] },
    File(PathBuf),
}

/// One close call: two heads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairAsk {
    pub a: Crop,
    pub b: Crop,
}

/// The judge's answer for a pair, asked both ways round.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub p_ab: f32,
    pub p_ba: f32,
}

impl Verdict {
    /// The mean P(yes) of both orders.
    pub fn p(&self) -> f32 {
        (self.p_ab + self.p_ba) / 2.0
    }

    /// The same suit, by `choice`'s threshold.
    pub fn same(&self, choice: JudgeChoice) -> bool {
        self.p() >= choice.yes_cut()
    }
}

/// A pair's result: a verdict, or why one of its photos couldn't be
/// shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairResult {
    pub result: Result<Verdict, FileError>,
}

/// What the app asks the judge helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    /// Judge these pairs; the reply holds one result per pair, in order.
    Judge { id: u64, pairs: Vec<PairAsk> },
    Quit,
}

/// What the judge helper answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// Loaded, every layer on the card: the device's name, the layers
    /// offloaded (all of them), and how long the load took.
    Ready { device: String, layers: i32, load_ms: u64 },
    /// The card can't run it (no card, too little memory for every
    /// layer, a driver failure); the helper exits.  Nothing moves to the
    /// processor.
    CardFailed { failure: CardFailure },
    /// The model files are missing or damaged, or the helper can't run;
    /// it exits.
    Failed { message: String },
    Judged { id: u64, results: Vec<PairResult> },
    Error { id: u64, message: String },
    /// Nothing asked for [`IDLE_SECS`]: exiting on purpose, the card's
    /// memory with it.
    Idle,
}

/// Every layer on the card, or a card failure saying how many weren't.
/// llama.cpp reports `offloaded/layers`; anything short of all of them
/// (or no report at all) is refused: the judge never runs partly on the
/// processor.
pub fn check_offload(offloaded: i32, layers: i32) -> Result<(), CardFailure> {
    if layers > 0 && offloaded == layers {
        return Ok(());
    }
    Err(CardFailure::Failed(if layers > 0 {
        format!("the close-call judge needs all {layers} of its layers on the graphics card, and only {offloaded} fit")
    } else {
        "the close-call judge couldn't confirm that every layer is on the graphics card".into()
    }))
}

/// A head's crop for the judge, as JPEG bytes: read from its photo (the
/// JPG decoded at the largest reduction that keeps the crop at least
/// [`CROP_PX`] wide; a RAW's largest embedded preview), oriented, cut as
/// the head step cuts (`heads::crop_square`), resized to [`CROP_PX`] and
/// encoded at [`CROP_QUALITY`].  A photo whose content key isn't `key`
/// any more is refused.
pub fn head_crop(path: &Path, file_type: FileType, key: &Key, bbox: [f32; 4]) -> Result<Vec<u8>, FileError> {
    let size = std::fs::metadata(path).map_err(|e| FileError::io(&e))?.len();
    let (jpeg, orientation, found_key) = match file_type {
        FileType::Jpg => {
            let data = std::fs::read(path).map_err(|e| FileError::io(&e))?;
            let k = Key::of(size, &data);
            let o = crate::exif::orientation_from_jpeg_head(&data[..data.len().min(256 * 1024)]);
            (data, o, k)
        }
        FileType::Raw => {
            let head = {
                use std::io::Read;
                let mut f = crate::exif::open_for_header(path).map_err(|e| FileError::io(&e))?;
                let mut buf = Vec::with_capacity(KEY_HEAD_BYTES);
                f.by_ref().take(KEY_HEAD_BYTES as u64).read_to_end(&mut buf).map_err(|e| FileError::io(&e))?;
                buf
            };
            let k = Key::of(size, &head);
            let mut raw = crate::imaging::libraw::Raw::open(path).map_err(|e| FileError::other(format!("RAW: {e}")))?;
            let flip = raw.exif_orientation();
            match raw.thumbnail().map_err(|e| FileError::other(format!("RAW preview: {e}")))? {
                crate::imaging::libraw::Thumbnail::Jpeg(bytes) => {
                    let o = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
                    (bytes, o, k)
                }
                crate::imaging::libraw::Thumbnail::Bitmap { width, height, colors, data } => {
                    if k != *key {
                        return Err(FileError::other("the photo changed since its heads were found"));
                    }
                    let px = crate::imaging::thumbs::bitmap(width, height, colors, data).ok_or_else(|| FileError::other("RAW preview is not readable"))?;
                    return encode_crop(&decode::apply_orientation(px, flip), bbox);
                }
            }
        }
    };
    if found_key != *key {
        return Err(FileError::other("the photo changed since its heads were found"));
    }
    let (w, h) = decode::jpeg_dimensions(&jpeg).ok_or_else(|| FileError::other("not a readable JPEG"))?;
    let (ow, oh) = if orientation >= 5 { (h, w) } else { (w, h) };
    let side = (bbox[2] * ow as f32).max(bbox[3] * oh as f32) * (1.0 + 2.0 * super::heads::CROP_MARGIN);
    let denom = [8u32, 4, 2].into_iter().find(|&d| side / d as f32 >= CROP_PX as f32).unwrap_or(1);
    let px = decode::decode_jpeg_full_scaled(&jpeg, denom).map_err(FileError::other)?;
    encode_crop(&decode::apply_orientation(px, orientation), bbox)
}

/// Cut, resize and encode a crop from an oriented image.
fn encode_crop(px: &Pixels, bbox: [f32; 4]) -> Result<Vec<u8>, FileError> {
    let cut = super::heads::crop_square(px, bbox, super::heads::CROP_MARGIN);
    let small = decode::resize_to(&cut, CROP_PX, CROP_PX);
    let rgb = decode::to_rgb(&small);
    let mut out = Vec::with_capacity(64 * 1024);
    jpeg_encoder::Encoder::new(&mut out, CROP_QUALITY)
        .encode(&rgb, CROP_PX as u16, CROP_PX as u16, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| FileError::other(format!("crop: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt as llama-server rendered the research's request (two
    /// image parts, then the text, thinking off), BOS left to the
    /// tokenizer.
    #[test]
    fn the_prompt_is_the_researchs_rendering() {
        let p = prompt("<M>");
        assert_eq!(p, format!("<|turn>user\n<M><M>{QUESTION}<turn|>\n<|turn>model\n<|channel>thought\n<channel|>"));
        assert!(!p.starts_with("<bos>"), "the tokenizer adds BOS");
        assert_eq!(p.matches("<M>").count(), 2, "one marker per head");
    }

    #[test]
    fn p_yes_is_yes_over_yes_and_no_among_the_top_tokens() {
        let lp = |p: f64| p.ln() as f32;
        let top = vec![("yes".to_string(), lp(0.6)), (" Yes".into(), lp(0.1)), ("**No".into(), lp(0.1)), ("maybe".into(), lp(0.2))];
        assert!((p_yes(&top) - 0.875).abs() < 1e-5, "{}", p_yes(&top));
        assert_eq!(p_yes(&[("perhaps".into(), lp(0.9))]), 0.5, "neither: undecided");
        assert!((p_yes(&[("No".into(), lp(0.5))]) - 0.0).abs() < 1e-6);
        assert!((p_yes(&[(" *yes*".into(), lp(0.2))]) - 1.0).abs() < 1e-6, "trimmed, then leading stars dropped");
        assert!(Verdict { p_ab: 0.7, p_ba: 0.5 }.same(JudgeChoice::Gemma4) && !Verdict { p_ab: 0.7, p_ba: 0.4 }.same(JudgeChoice::Gemma4), "the mean against 0.6");
        assert!(Verdict { p_ab: 0.5, p_ba: 0.4 }.same(JudgeChoice::Qwen3VlReranker) && !Verdict { p_ab: 0.5, p_ba: 0.39 }.same(JudgeChoice::Qwen3VlReranker), "Qwen's own: 0.45");
    }

    /// Qwen's prompt is the official template as the research rendered
    /// it, and its score sigmoid(logit yes − logit no).
    #[test]
    fn qwen_is_asked_and_scored_as_the_research_did() {
        let p = JudgeChoice::Qwen3VlReranker.prompt("<M>");
        assert_eq!(
            p,
            format!("<|im_start|>system\n{QWEN_SYSTEM}<|im_end|>\n<|im_start|>user\n<Instruct>: {QWEN_INSTRUCT}<Query>:<M>\n<Document>:<M><|im_end|>\n<|im_start|>assistant\n")
        );
        assert_eq!(p.matches("<M>").count(), 2);
        assert_eq!((JudgeChoice::Qwen3VlReranker.image_tokens(), JudgeChoice::Gemma4.image_tokens()), (280, 560));
        assert_eq!(JudgeChoice::Gemma4.prompt("<M>"), prompt("<M>"));
        // sigmoid(ly − ln): equal logits 0.5; yes two nats ahead 0.881.
        let s = |v: &[(&str, f32)]| p_rerank(&v.iter().map(|(t, l)| (t.to_string(), *l)).collect::<Vec<_>>());
        assert!((s(&[("yes", -1.0), ("no", -1.0)]) - 0.5).abs() < 1e-6);
        assert!((s(&[("no", -3.0), ("yes", -1.0), ("x", -9.0)]) - 0.880_797).abs() < 1e-5);
        // Exact tokens only, and one outside the list counts at the floor.
        assert!((s(&[("Yes", -0.1), ("no", -0.2), ("x", -5.0)]) - (1.0 / (1.0 + (4.8f64).exp())) as f32).abs() < 1e-6);
        assert_eq!(p_rerank(&[]), 0.5);
    }

    /// Card only: anything short of every layer on the card is a card
    /// failure, never a run with some layers on the processor.
    #[test]
    fn a_partial_offload_is_refused() {
        assert_eq!(check_offload(31, 31), Ok(()));
        assert!(matches!(check_offload(30, 31), Err(CardFailure::Failed(m)) if m.contains("all 31") && m.contains("only 30")));
        assert!(matches!(check_offload(0, 31), Err(CardFailure::Failed(_))));
        assert!(matches!(check_offload(-1, -1), Err(CardFailure::Failed(_))), "no report is no proof");
    }

    #[test]
    fn requests_and_replies_round_trip_as_one_line_each() {
        let head = Crop::Head { path: PathBuf::from("/mnt/nas/a.JPG"), file_type: FileType::Jpg, key: Key([3; 16]), bbox: [0.1, 0.2, 0.3, 0.4] };
        let req = Request::Judge { id: 4, pairs: vec![PairAsk { a: head.clone(), b: Crop::File(PathBuf::from("/x/1.jpg")) }] };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n') && line.starts_with(r#"{"req":"judge""#));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
        for rep in [
            Reply::Ready { device: "Test GPU".into(), layers: 31, load_ms: 9000 },
            Reply::CardFailed { failure: CardFailure::NoCard },
            Reply::Failed { message: "no model".into() },
            Reply::Judged { id: 4, results: vec![PairResult { result: Ok(Verdict { p_ab: 0.9, p_ba: 0.8 }) }, PairResult { result: Err(FileError::other("gone")) }] },
            Reply::Error { id: 1, message: "m".into() },
            Reply::Idle,
        ] {
            let line = serde_json::to_string(&rep).unwrap();
            assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), rep);
        }
    }

    /// The crop the judge sees: square, CROP_PX wide, cut from the photo
    /// at a reduction that keeps the head at least that wide, and refused
    /// when the photo changed.
    #[test]
    fn a_head_crop_is_cut_from_the_photo_and_refused_when_it_changed() {
        let d = std::env::temp_dir().join(format!("pp-judge-crop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let img = crate::imaging::quality::tests::texture(3000, 2000, 5);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90).encode(&img.d, 3000, 2000, jpeg_encoder::ColorType::Luma).unwrap();
        let p = d.join("a.jpg");
        std::fs::write(&p, &out).unwrap();
        let key = Key::of(out.len() as u64, &out);
        let jpeg = head_crop(&p, FileType::Jpg, &key, [0.4, 0.4, 0.1, 0.1]).expect("cut");
        assert_eq!(decode::jpeg_dimensions(&jpeg), Some((CROP_PX, CROP_PX)));
        assert!(matches!(head_crop(&p, FileType::Jpg, &Key([0; 16]), [0.4, 0.4, 0.1, 0.1]), Err(e) if e.message.contains("changed")));
        assert!(matches!(head_crop(&d.join("gone.jpg"), FileType::Jpg, &key, [0.4, 0.4, 0.1, 0.1]), Err(e) if e.errno.is_some()));
        let _ = std::fs::remove_dir_all(&d);
    }

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
        assert_eq!(JudgeChoice::ALL, [JudgeChoice::Off, JudgeChoice::Qwen3VlReranker, JudgeChoice::Gemma4], "Qwen listed first, Off the default");
        assert_eq!(JudgeChoice::for_model(ModelId::Qwen3VlReranker), Some(JudgeChoice::Qwen3VlReranker));
        assert_eq!(serde_json::to_string(&JudgeChoice::Qwen3VlReranker).unwrap(), "\"qwen3_vl_reranker\"");
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
