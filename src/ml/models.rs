//! The model registry: every model the app can download, pinned by URL and
//! SHA-256, with its size, licence and publisher, and where
//! downloaded files live.
//!
//! Search: SigLIP2 ViT-B/16 at 256 px, the checkpoint the feasibility
//! study measured, as the ONNX export Hugging Face's
//! `onnx-community` maintains for transformers.js.  The image tower is the
//! fp32 file (372 MB): measured here, the fp16 one gives wrong answers on
//! WebGPU (cosine 0.61 against the processor; the self-check refuses it)
//! and runs ten times slower on the processor.  The text tower is fp16
//! (565 MB against 1 130 MB): its answers equal fp32's (cosine 1.000) and
//! a query takes ~50 ms on the processor against ~30.
//!
//! Faces: YuNet finds them for every choice,
//! and the user picks one of three recognition models in the download
//! dialog — AuraFace (pre-selected), InsightFace's `buffalo_l`
//! recognition model (non-commercial research licence; downloaded only
//! from InsightFace's own release, never re-hosted), or SFace.  The
//! choice is a setting (`Settings::face_model`); the rows are data, so the
//! dialog offers them from here ([`face_models`]).
//!
//! Files are kept in `~/.cache/photo_pipeline/models/<model id>/`: they
//! are re-downloadable, so a cache, shown and cleared in Preferences ▸
//! Storage.  Nothing here is ever written into the library.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A model the app knows how to download and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelId {
    /// SigLIP2 ViT-B/16 (256 px): the search model, image and text towers
    /// and the tokenizer.
    Siglip2,
    /// YuNet (OpenCV Zoo, 2023 March): finds faces and their five
    /// landmarks, for every face model.
    Yunet,
    /// AuraFace v1 (fal, ResNet-100): face recognition, pre-selected.
    AuraFace,
    /// InsightFace `buffalo_l`'s recognition model (`w600k_r50.onnx`,
    /// ResNet-50 on WebFace600K): what Immich uses.  Non-commercial
    /// research licence; from InsightFace's release only.
    InsightFace,
    /// SFace (OpenCV Zoo, 2021 December): the smallest and fastest.
    SFace,
    /// OWLv2 B/16 (ensemble checkpoint): finds fursuit heads by the words
    /// "a fursuit head".  The image tower, the text
    /// tower and the box head in one graph, with its CLIP tokenizer.
    Owlv2,
    /// Gemma 4 26B-A4B-it (Unsloth's UD-Q4_K_M GGUF with its vision
    /// projector): the fursuit close-call judge (v2), run by the judge
    /// helper on the card alone.
    Gemma4,
    /// Qwen3-VL-Reranker-8B (mradermacher's Q8_0 GGUF with its fp16
    /// vision projector): the other close-call judge (v2), listed first.
    Qwen3VlReranker,
}

impl ModelId {
    /// The folder name under the models directory.
    pub fn dir_name(self) -> &'static str {
        match self {
            ModelId::Siglip2 => "siglip2-b16-256",
            ModelId::Yunet => "yunet-2023mar",
            ModelId::AuraFace => "auraface-v1",
            ModelId::InsightFace => "insightface-buffalo-l",
            ModelId::SFace => "sface-2021dec",
            ModelId::Owlv2 => "owlv2-b16-ensemble",
            ModelId::Gemma4 => "gemma4-26b-a4b-it-ud-q4km",
            ModelId::Qwen3VlReranker => "qwen3-vl-reranker-8b-q8",
        }
    }

    /// One of the face-recognition choices.
    pub fn is_face_model(self) -> bool {
        matches!(self, ModelId::AuraFace | ModelId::InsightFace | ModelId::SFace)
    }

    /// One of the fursuit head step's models: the detector.
    /// The head vectors come from the search model (v2), which is always
    /// in place.
    pub fn is_fursuit_model(self) -> bool {
        matches!(self, ModelId::Owlv2)
    }

    /// One of the close-call judge choices (v2).
    pub fn is_judge_model(self) -> bool {
        matches!(self, ModelId::Gemma4 | ModelId::Qwen3VlReranker)
    }
}

/// The fursuit head step's models (v2): OWLv2 finds the heads;
/// their vectors come from the search model, so nothing else is needed.
/// DINOv2 (the earlier head vectors) is no longer listed: a folder left from
/// then is counted and removed with the rest by Preferences ▸ Storage.
pub const FURSUIT_MODELS: [ModelId; 1] = [ModelId::Owlv2];

/// The face detector every face model uses.
pub const FACE_DETECTOR: ModelId = ModelId::Yunet;
/// The face model pre-selected in the dialog (a comparison may move it).
pub const FACE_MODEL_DEFAULT: ModelId = ModelId::AuraFace;

/// A file packed inside the downloaded archive (InsightFace ships a zip
/// of five models; the app keeps one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Archive {
    /// Lower-case hex SHA-256 of the archive as downloaded.
    pub sha256: &'static str,
    /// The archive's size: what the download reports.
    pub bytes: u64,
}

/// One file of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// The name it is kept under (and, inside an archive, the member).
    pub file: &'static str,
    /// What is downloaded: the file itself, or the archive holding it.
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the file kept.
    pub sha256: &'static str,
    /// The file kept, in bytes.
    pub bytes: u64,
    /// Set when the URL is an archive: its own digest and size are
    /// checked first, then the member's.
    pub archive: Option<Archive>,
}

impl ModelFile {
    /// Bytes that come down the wire for this file.
    pub fn download_bytes(&self) -> u64 {
        self.archive.map_or(self.bytes, |a| a.bytes)
    }
}

/// A row of the download dialog, and what the helper loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: ModelId,
    /// The step it serves: "Search", "Faces", later "Fursuits".
    pub role: &'static str,
    /// Its name as the dialog shows it.
    pub name: &'static str,
    /// One line on what it does.
    pub what: &'static str,
    /// The SPDX licence id, or the licence's own words.
    pub licence: &'static str,
    /// The licence in plain words ("free for any use").
    pub licence_plain: &'static str,
    pub publisher: &'static str,
    /// Where the files come from, for *Show sources and checksums*.
    pub source_url: &'static str,
    /// The step can't run without it (search is always used).
    pub required: bool,
    pub files: &'static [ModelFile],
    /// Version of the preprocessing that goes with these weights; part of
    /// the index's identity (a change means a new index file).
    pub preprocess_version: u32,
    /// The radio group this row belongs to (`"face"` for the three face
    /// models: the dialog offers one of them); `None` for a row of its
    /// own.
    pub choice: Option<&'static str>,
    /// Pre-selected in its group ("Recommended").
    pub recommended: bool,
    /// Its licence binds the user (non-commercial research only): shown in
    /// the warning tone, never pre-selected.
    pub restricted: bool,
}

impl ModelInfo {
    /// Bytes downloaded for this model (archives counted whole).
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(ModelFile::download_bytes).sum()
    }

    /// Bytes kept once installed (the dialog's "174 MB is kept").
    pub fn kept_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    /// The SHA-256 of the main weights file, for the index header.
    pub fn weights_sha256(&self) -> &'static str {
        self.files[0].sha256
    }
}

const SIGLIP2_BASE: &str = "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/";

/// The search model's files: the image tower (fp32), the text tower
/// (fp16), and the Gemma tokenizer (`tokenizers` JSON).  Pinned on
/// 2026-10-01.
pub const SIGLIP2_FILES: &[ModelFile] = &[
    ModelFile {
        file: "vision_model.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/onnx/vision_model.onnx",
        sha256: "f5cb16728a704703f05516ded628397e11dbca4de2eb5db04b0c0bcee988aa7a",
        bytes: 371_992_072,
        archive: None,
    },
    ModelFile {
        file: "text_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/onnx/text_model_fp16.onnx",
        sha256: "80954edffdc689599e5d5bc6a1738380bc9e8139a18e5c8892485f248b6b4890",
        bytes: 564_862_230,
        archive: None,
    },
    ModelFile {
        file: "tokenizer.json",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/tokenizer.json",
        sha256: "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322",
        bytes: 34_363_039,
        archive: None,
    },
];

const OPENCV_ZOO: &str = "https://github.com/opencv/opencv_zoo/tree/main/models/";

/// YuNet, from the OpenCV Zoo (an LFS file: the media URL).  Pinned on
/// 2026-10-02; the digest is the one the feasibility study measured with.
pub const YUNET_FILES: &[ModelFile] = &[ModelFile {
    file: "face_detection_yunet_2023mar.onnx",
    url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
    sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    bytes: 232_589,
    archive: None,
}];

/// AuraFace v1's recognition model alone (its repository also carries
/// InsightFace's detectors, which are not taken).
pub const AURAFACE_FILES: &[ModelFile] = &[ModelFile {
    file: "glintr100.onnx",
    url: "https://huggingface.co/fal/AuraFace-v1/resolve/main/glintr100.onnx",
    sha256: "a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60",
    bytes: 260_694_151,
    archive: None,
}];

/// InsightFace's `buffalo_l.zip` from its own v0.7 release; the app keeps
/// the recognition model and deletes the rest of the archive.
pub const INSIGHTFACE_FILES: &[ModelFile] = &[ModelFile {
    file: "w600k_r50.onnx",
    url: "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip",
    sha256: "4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43",
    bytes: 174_383_860,
    archive: Some(Archive { sha256: "80ffe37d8a5940d59a7384c201a2a38d4741f2f3c51eef46ebb28218a7b0ca2f", bytes: 288_621_354 }),
}];

/// SFace, from the OpenCV Zoo.
pub const SFACE_FILES: &[ModelFile] = &[ModelFile {
    file: "face_recognition_sface_2021dec.onnx",
    url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
    sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
    bytes: 38_696_353,
    archive: None,
}];

const OWLV2_BASE: &str = "https://huggingface.co/Xenova/owlv2-base-patch16-ensemble/resolve/main/";
const GEMMA4_BASE: &str = "https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/resolve/main/";

/// OWLv2 B/16 ensemble as the transformers.js export Hugging Face's staff
/// maintain of Google's `owlv2-base-patch16-ensemble` (Apache-2.0): the
/// fp32 graph (measured here against the reference PyTorch boxes: the same
/// heads, scores within 0.02) and its CLIP tokenizer.  Pinned 2026-10-02.
pub const OWLV2_FILES: &[ModelFile] = &[
    ModelFile {
        file: "model.onnx",
        url: "https://huggingface.co/Xenova/owlv2-base-patch16-ensemble/resolve/main/onnx/model.onnx",
        sha256: "a58f0efe148481660a0b1e4843b59cf7af52c8d2965afb6abc98a252c63552d2",
        bytes: 614_400_346,
        archive: None,
    },
    ModelFile {
        file: "tokenizer.json",
        url: "https://huggingface.co/Xenova/owlv2-base-patch16-ensemble/resolve/main/tokenizer.json",
        sha256: "bbce3185c27ac4daf41ea2dfc864785bdcd1a42815e7646324af20edd671a578",
        bytes: 2_224_254,
        archive: None,
    },
];

/// Gemma 4 26B-A4B-it as Unsloth's dynamic Q4_K_M GGUF with its fp16
/// vision projector (Apache-2.0): the research's pick for the fursuit
/// close-call judge (recall at 95 % precision 106/110, 81/84, 102/102,
/// 63/63 per time tier on 791 labelled pairs).  Pinned 2026-10-08 by the
/// digests the research measured on its own download.
pub const GEMMA4_FILES: &[ModelFile] = &[
    ModelFile {
        file: "gemma-4-26B-A4B-it-UD-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/resolve/main/gemma-4-26B-A4B-it-UD-Q4_K_M.gguf",
        sha256: "f2c28b3dc4776931ac6f879e11f203dec637ea0f14267a86ec8f6165f63f293f",
        bytes: 16_947_541_728,
        archive: None,
    },
    ModelFile {
        file: "mmproj-F16.gguf",
        url: "https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/resolve/main/mmproj-F16.gguf",
        sha256: "418a6d8723067cd712235facbbc5cba6c8fbbd413fc1292d2aace5a027d5a42f",
        bytes: 1_193_058_784,
        archive: None,
    },
];

const QWEN3VL_RR_BASE: &str = "https://huggingface.co/mradermacher/Qwen3-VL-Reranker-8B-GGUF";

/// Qwen3-VL-Reranker-8B as mradermacher's static Q8_0 GGUF with its fp16
/// vision projector (Apache-2.0), pinned to the repository's commit
/// a0e8b54: tensor for tensor the research's own conversion of Qwen's
/// weights with the vendored llama.cpp, and within 0.034 of PyTorch's
/// scores on 52 pairs (r 0.9995).  Pinned 2026-10-09.
pub const QWEN3VL_RR_FILES: &[ModelFile] = &[
    ModelFile {
        file: "Qwen3-VL-Reranker-8B.Q8_0.gguf",
        url: "https://huggingface.co/mradermacher/Qwen3-VL-Reranker-8B-GGUF/resolve/a0e8b54c5ac2f0bd9c3e1a2c4eb497c1b98908ed/Qwen3-VL-Reranker-8B.Q8_0.gguf",
        sha256: "275e1f566cdc3355ce99d1b9a764737b7829b06566f356d9a6b5fdcf701c2779",
        bytes: 8_709_520_352,
        archive: None,
    },
    ModelFile {
        file: "Qwen3-VL-Reranker-8B.mmproj-f16.gguf",
        url: "https://huggingface.co/mradermacher/Qwen3-VL-Reranker-8B-GGUF/resolve/a0e8b54c5ac2f0bd9c3e1a2c4eb497c1b98908ed/Qwen3-VL-Reranker-8B.mmproj-f16.gguf",
        sha256: "15cd9bd4882dae771344f0ac204fce07de91b47c1438ada3861dfc817403c31e",
        bytes: 1_159_030_336,
        archive: None,
    },
];

/// Every model, in the dialog's order: search, the face detector, the
/// face choices (the recommended one first), the fursuit head detector,
/// then the close-call judge choices (v2).
pub const ALL: &[ModelInfo] = &[
    ModelInfo {
        id: ModelId::Siglip2,
        role: "Search",
        name: "SigLIP 2, base size",
        what: "Finds photos by what they show.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "Google",
        source_url: SIGLIP2_BASE,
        required: true,
        files: SIGLIP2_FILES,
        preprocess_version: 1,
        choice: None,
        recommended: false,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::Yunet,
        role: "Faces",
        name: "YuNet",
        what: "Finds faces, for whichever face model is chosen.",
        licence: "MIT",
        licence_plain: "free for any use",
        publisher: "OpenCV",
        source_url: OPENCV_ZOO,
        required: false,
        files: YUNET_FILES,
        preprocess_version: 1,
        choice: None,
        recommended: false,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::AuraFace,
        role: "Faces",
        name: "AuraFace",
        what: "The most accurate in tests here; the slowest.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "fal",
        source_url: "https://huggingface.co/fal/AuraFace-v1",
        required: false,
        files: AURAFACE_FILES,
        preprocess_version: 1,
        choice: Some("face"),
        recommended: true,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::SFace,
        role: "Faces",
        name: "SFace",
        what: "The smallest and fastest; mixed people up more often in tests.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "OpenCV",
        source_url: OPENCV_ZOO,
        required: false,
        files: SFACE_FILES,
        preprocess_version: 1,
        choice: Some("face"),
        recommended: false,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::InsightFace,
        role: "Faces",
        name: "InsightFace, what Immich uses",
        what: "Best published scores.  From InsightFace's own release; 174 MB of its 289 MB archive is kept.",
        licence: "non-commercial research only",
        licence_plain: "for non-commercial research purposes only, and it binds you if you pick it",
        publisher: "InsightFace",
        source_url: "https://github.com/deepinsight/insightface/releases/tag/v0.7",
        required: false,
        files: INSIGHTFACE_FILES,
        preprocess_version: 1,
        choice: Some("face"),
        recommended: false,
        restricted: true,
    },
    ModelInfo {
        id: ModelId::Owlv2,
        role: "Fursuits",
        name: "OWLv2, base size",
        what: "Finds fursuit heads in the photos tagged as fursuits.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "Google",
        source_url: OWLV2_BASE,
        required: false,
        files: OWLV2_FILES,
        preprocess_version: 1,
        choice: None,
        recommended: false,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::Qwen3VlReranker,
        role: "Close-call judge",
        name: "Qwen3-VL-Reranker, 8B",
        what: "Looks at two fursuit heads side by side and says whether they are the same fursuit.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "Alibaba's Qwen team, converted by mradermacher",
        source_url: QWEN3VL_RR_BASE,
        required: false,
        files: QWEN3VL_RR_FILES,
        preprocess_version: 1,
        choice: Some("judge"),
        recommended: false,
        restricted: false,
    },
    ModelInfo {
        id: ModelId::Gemma4,
        role: "Close-call judge",
        name: "Gemma 4, 26B-A4B",
        what: "Looks at two fursuit heads side by side and says whether they are the same fursuit.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "Google, quantised by Unsloth",
        source_url: GEMMA4_BASE,
        required: false,
        files: GEMMA4_FILES,
        preprocess_version: 1,
        choice: Some("judge"),
        recommended: false,
        restricted: false,
    },
];

/// The registry entry for `id`.
pub fn info(id: ModelId) -> &'static ModelInfo {
    ALL.iter().find(|m| m.id == id).expect("every ModelId is in ALL")
}

/// The models search needs.
pub fn search_models() -> &'static [ModelInfo] {
    &ALL[..1]
}

/// The face-recognition choices, the recommended one first (the dialog's
/// radio group).
pub fn face_models() -> impl Iterator<Item = &'static ModelInfo> {
    ALL.iter().filter(|m| m.choice == Some("face"))
}

/// The models to download for a face choice: the detector and the model.
pub fn face_set(face_model: ModelId) -> [ModelId; 2] {
    [FACE_DETECTOR, face_model]
}

/// Every model a first index needs: search, plus faces when a face model
/// is chosen.
pub fn models_for(face_model: Option<ModelId>) -> Vec<ModelId> {
    let mut out = vec![ModelId::Siglip2];
    if let Some(f) = face_model {
        out.extend(face_set(f));
    }
    out
}

/// The face model among `ids`, if one.
pub fn face_model_in(ids: &[ModelId]) -> Option<ModelId> {
    ids.iter().copied().find(|id| id.is_face_model())
}

/// The fursuit head step's rows (the dialog's *Fursuits* row).
pub fn fursuit_models() -> impl Iterator<Item = &'static ModelInfo> {
    ALL.iter().filter(|m| m.id.is_fursuit_model())
}

/// The dialog asked for fursuit heads: the step's models are among `ids`.
pub fn fursuits_in(ids: &[ModelId]) -> bool {
    FURSUIT_MODELS.iter().all(|m| ids.contains(m))
}

/// The close-call judge choices (v2; Preferences ▸ Fursuits ▸ *Close-call
/// judge*), in the list's order.
pub fn judge_models() -> impl Iterator<Item = &'static ModelInfo> {
    ALL.iter().filter(|m| m.choice == Some("judge"))
}

/// Preferences ▸ Storage ▸ Downloaded models ▸ Delete: every model
/// folder in `dir` goes (folders of models no longer used too, such as
/// the earlier `dinov2-base`), except the close-call judges', which have a
/// row of their own.
pub fn delete_all_but_judges(dir: &Path) {
    let keep: Vec<&str> = judge_models().map(|m| m.id.dir_name()).collect();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        if keep.iter().any(|k| name.to_str() == Some(*k)) {
            continue;
        }
        let p = e.path();
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

/// `~/.cache/photo_pipeline/models/`
pub fn models_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cache").join("photo_pipeline").join("models"))
}

/// Where `id`'s files live once downloaded.
pub fn model_dir(id: ModelId) -> Option<PathBuf> {
    Some(models_dir()?.join(id.dir_name()))
}

/// The path of one of `id`'s files.
pub fn file_path(id: ModelId, file: &ModelFile) -> Option<PathBuf> {
    Some(model_dir(id)?.join(file.file))
}

/// Every file of `id` is in place with its expected size.  Stats the
/// files: call it off the UI thread.  (The digest was checked when the
/// file was renamed into place; a file of the wrong size is not one of
/// ours.)
pub fn installed(id: ModelId) -> bool {
    let m = info(id);
    m.files.iter().all(|f| file_path(id, f).and_then(|p| std::fs::metadata(p).ok()).is_some_and(|md| md.len() == f.bytes))
}

/// Bytes the dialog's chosen rows download together.
pub fn bytes_for(ids: &[ModelId]) -> u64 {
    ids.iter().map(|&id| info(id).bytes()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_pinned_and_consistent() {
        for m in ALL {
            assert!(!m.files.is_empty());
            assert_eq!(info(m.id).id, m.id);
            for f in m.files {
                assert_eq!(f.sha256.len(), 64, "{}: a full SHA-256", f.file);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                assert!(f.url.starts_with("https://"), "{}: pinned https URL", f.file);
                match f.archive {
                    None => assert!(f.url.ends_with(f.file), "{}: the URL names the file", f.file),
                    Some(a) => {
                        assert!(f.url.ends_with(".zip"), "{}: an archive", f.file);
                        assert!(a.sha256.len() == 64 && a.bytes > f.bytes);
                    }
                }
                assert!(f.bytes > 0);
            }
            assert!(!m.licence.is_empty() && !m.licence_plain.is_empty() && !m.publisher.is_empty());
        }
        // About 970 MB for search: the fp32 image tower, the fp16 text tower.
        let search_mb = info(ModelId::Siglip2).bytes() / 1_000_000;
        assert!((900..1_050).contains(&search_mb), "{search_mb} MB");
        assert_eq!(bytes_for(&[ModelId::Siglip2]), info(ModelId::Siglip2).bytes());
        assert_eq!(serde_json::to_string(&ModelId::Siglip2).unwrap(), "\"siglip2\"");
        assert_eq!(serde_json::to_string(&ModelId::AuraFace).unwrap(), "\"aura-face\"");
        assert_eq!(search_models().len(), 1);
    }

    /// Three face choices, AuraFace recommended, InsightFace
    /// restricted and never recommended, from its publisher's release.
    #[test]
    fn the_face_models_are_a_choice_with_insightface_restricted() {
        let faces: Vec<&ModelInfo> = face_models().collect();
        assert_eq!(faces.iter().map(|m| m.id).collect::<Vec<_>>(), [ModelId::AuraFace, ModelId::SFace, ModelId::InsightFace]);
        assert_eq!(faces.iter().filter(|m| m.recommended).map(|m| m.id).collect::<Vec<_>>(), [FACE_MODEL_DEFAULT]);
        assert!(faces.iter().all(|m| m.choice == Some("face") && m.role == "Faces" && m.id.is_face_model() && !m.required));
        let insight = info(ModelId::InsightFace);
        assert!(insight.restricted && !insight.recommended);
        assert!(insight.files[0].url.starts_with("https://github.com/deepinsight/insightface/releases/"), "from InsightFace's own release only");
        assert_eq!(insight.files[0].archive.map(|a| a.bytes / 1_000_000), Some(288));
        assert_eq!((insight.bytes() / 1_000_000, insight.kept_bytes() / 1_000_000), (288, 174));
        assert!(insight.licence_plain.contains("non-commercial"));
        assert!(!FACE_DETECTOR.is_face_model() && info(FACE_DETECTOR).licence == "MIT");
        assert!(faces.iter().filter(|m| !m.restricted).all(|m| m.licence == "Apache-2.0"));
        assert_eq!(models_for(None), [ModelId::Siglip2]);
        assert_eq!(models_for(Some(ModelId::SFace)), [ModelId::Siglip2, ModelId::Yunet, ModelId::SFace]);
        assert_eq!(face_model_in(&models_for(Some(ModelId::SFace))), Some(ModelId::SFace));
        assert_eq!(face_model_in(&[ModelId::Siglip2, ModelId::Yunet]), None);
        // The dialog's sizes: YuNet is tiny, SFace 39 MB, AuraFace 261 MB.
        assert_eq!(info(ModelId::Yunet).bytes() / 1_000, 232);
        assert_eq!(info(ModelId::SFace).bytes() / 1_000_000, 38);
        assert_eq!(info(ModelId::AuraFace).bytes() / 1_000_000, 260);
    }

    /// The fursuit head step is one row
    /// (OWLv2, Apache-2.0, a maintained ONNX export, pinned); the head
    /// vectors come from the search model, so DINOv2 is gone from the
    /// registry.
    #[test]
    fn the_fursuit_step_is_the_pinned_detector_alone() {
        let rows: Vec<&ModelInfo> = fursuit_models().collect();
        assert_eq!(rows.iter().map(|m| m.id).collect::<Vec<_>>(), FURSUIT_MODELS);
        assert!(rows.iter().all(|m| m.role == "Fursuits" && m.licence == "Apache-2.0" && !m.required && !m.restricted && m.choice.is_none()));
        assert!(rows.iter().all(|m| !m.id.is_face_model() && !m.id.is_judge_model()), "never a face model or the judge");
        assert!(ALL.iter().filter(|m| !m.id.is_fursuit_model()).all(|m| m.role != "Fursuits"));
        assert!(ALL.iter().all(|m| !m.id.dir_name().contains("dinov2")), "DINOv2 is not downloaded any more");
        assert_eq!(info(ModelId::Owlv2).bytes() / 1_000_000, 616, "the graph and its tokenizer");
        assert_eq!(bytes_for(&FURSUIT_MODELS) / 1_000_000, 616);
        assert!(fursuits_in(&[ModelId::Siglip2, ModelId::Owlv2]));
        assert!(!fursuits_in(&[ModelId::Siglip2]), "no detector is no step");
        assert_eq!(serde_json::to_string(&ModelId::Owlv2).unwrap(), "\"owlv2\"");
        assert_eq!(models_for(None), [ModelId::Siglip2], "search alone, as before");
    }

    /// The close-call judge (v2): a choice list of its own, Apache-2.0,
    /// pinned by the digests the research measured, never required, and
    /// never a fursuit step or face model.
    #[test]
    fn the_judge_is_a_pinned_choice_of_its_own() {
        let rows: Vec<&ModelInfo> = judge_models().collect();
        assert_eq!(rows.iter().map(|m| m.id).collect::<Vec<_>>(), [ModelId::Qwen3VlReranker, ModelId::Gemma4], "Qwen listed first");
        let q = info(ModelId::Qwen3VlReranker);
        assert!(q.role == "Close-call judge" && q.licence == "Apache-2.0" && !q.required && q.choice == Some("judge") && q.id.is_judge_model());
        assert_eq!((q.files[0].bytes, q.files[1].bytes), (8_709_520_352, 1_159_030_336));
        assert_eq!(q.weights_sha256(), "275e1f566cdc3355ce99d1b9a764737b7829b06566f356d9a6b5fdcf701c2779");
        assert!(q.files.iter().all(|f| f.url.starts_with(QWEN3VL_RR_BASE) && f.url.contains("/resolve/a0e8b54c5ac2f0bd9c3e1a2c4eb497c1b98908ed/")), "pinned to the commit");
        assert_eq!(serde_json::to_string(&ModelId::Qwen3VlReranker).unwrap(), "\"qwen3-vl-reranker\"");
        let g = info(ModelId::Gemma4);
        assert!(g.role == "Close-call judge" && g.licence == "Apache-2.0" && !g.required && !g.restricted && g.choice == Some("judge"));
        assert!(g.id.is_judge_model() && !g.id.is_fursuit_model() && !g.id.is_face_model());
        assert_eq!(g.files.len(), 2, "the weights and the vision projector");
        assert_eq!(g.bytes() / 1_000_000_000, 18, "about 18 GB");
        assert_eq!(g.files[0].bytes / 1_000_000, 16_947);
        assert_eq!(g.files[1].bytes / 1_000_000, 1_193);
        assert!(g.files.iter().all(|f| f.url.starts_with(GEMMA4_BASE) && f.file.ends_with(".gguf")));
        assert_eq!(g.weights_sha256(), "f2c28b3dc4776931ac6f879e11f203dec637ea0f14267a86ec8f6165f63f293f");
        assert_eq!(serde_json::to_string(&ModelId::Gemma4).unwrap(), "\"gemma4\"");
        assert!(!FURSUIT_MODELS.contains(&ModelId::Gemma4) && !fursuits_in(&[ModelId::Gemma4]));
    }

    #[test]
    fn models_live_in_the_cache_never_the_library() {
        let dir = models_dir().expect("a home");
        assert!(dir.ends_with(".cache/photo_pipeline/models"));
        assert!(model_dir(ModelId::Siglip2).unwrap().starts_with(&dir));
        assert!(model_dir(ModelId::InsightFace).unwrap().starts_with(&dir));
    }
}
