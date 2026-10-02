//! The model registry: every model the app can download, pinned by URL and
//! SHA-256, with its size, licence and publisher (decision 13), and where
//! downloaded files live.
//!
//! Phase 1 lists search alone: SigLIP2 ViT-B/16 at 256 px, the checkpoint
//! the feasibility study measured, as the ONNX export Hugging Face's
//! `onnx-community` maintains for transformers.js (fp16 weights: 751 MB
//! for both towers, against 1.5 GB in fp32).  Face and fursuit models join
//! in their phases.
//!
//! Files are kept in `~/.cache/photo_pipeline/models/<model id>/`: they
//! are re-downloadable, so a cache, shown and cleared in Preferences ▸
//! Storage.  Nothing here is ever written into the library.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A model the app knows how to download and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelId {
    /// SigLIP2 ViT-B/16 (256 px): the search model, image and text towers
    /// and the tokenizer.
    Siglip2,
}

impl ModelId {
    /// The folder name under the models directory.
    pub fn dir_name(self) -> &'static str {
        match self {
            ModelId::Siglip2 => "siglip2-b16-256",
        }
    }
}

/// One file of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// The name it is kept under.
    pub file: &'static str,
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the whole file.
    pub sha256: &'static str,
    pub bytes: u64,
}

/// A row of the download dialog (§7a.7), and what the helper loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: ModelId,
    /// The step it serves: "Search", later "Faces", "Fursuits".
    pub role: &'static str,
    /// Its name as the dialog shows it.
    pub name: &'static str,
    /// One line on what it does.
    pub what: &'static str,
    /// The SPDX licence id.
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
    /// the index's identity (a change means a new index file, §6.3).
    pub preprocess_version: u32,
}

impl ModelInfo {
    /// Every file's size together.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    /// The SHA-256 of the main weights file, for the index header.
    pub fn weights_sha256(&self) -> &'static str {
        self.files[0].sha256
    }
}

const SIGLIP2_BASE: &str = "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/";

/// The search model's files: the image tower, the text tower (both fp16),
/// and the Gemma tokenizer (`tokenizers` JSON).  Pinned on 2026-10-01.
pub const SIGLIP2_FILES: &[ModelFile] = &[
    ModelFile {
        file: "vision_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/onnx/vision_model_fp16.onnx",
        sha256: "fe9ad8020a6d3d98d394c9be8f07064066135fc2f87ec11692de0b677c0ac4db",
        bytes: 186_131_676,
    },
    ModelFile {
        file: "text_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/onnx/text_model_fp16.onnx",
        sha256: "80954edffdc689599e5d5bc6a1738380bc9e8139a18e5c8892485f248b6b4890",
        bytes: 564_862_230,
    },
    ModelFile {
        file: "tokenizer.json",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/main/tokenizer.json",
        sha256: "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322",
        bytes: 34_363_039,
    },
];

/// Every model, in the dialog's order.
pub const ALL: &[ModelInfo] = &[ModelInfo {
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
}];

/// The registry entry for `id`.
pub fn info(id: ModelId) -> &'static ModelInfo {
    ALL.iter().find(|m| m.id == id).expect("every ModelId is in ALL")
}

/// The models search needs.
pub fn search_models() -> &'static [ModelInfo] {
    ALL
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

/// The ids among `ids` whose files are not all in place (stats files).
pub fn missing(ids: &[ModelId]) -> Vec<ModelId> {
    ids.iter().copied().filter(|&id| !installed(id)).collect()
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
                assert!(f.url.ends_with(f.file), "{}: the URL names the file", f.file);
                assert!(f.bytes > 0);
            }
            assert!(!m.licence.is_empty() && !m.licence_plain.is_empty() && !m.publisher.is_empty());
        }
        // The plan's number: about 750 MB for search in fp16.
        let search_mb = info(ModelId::Siglip2).bytes() / 1_000_000;
        assert!((700..800).contains(&search_mb), "{search_mb} MB");
        assert_eq!(bytes_for(&[ModelId::Siglip2]), info(ModelId::Siglip2).bytes());
        assert_eq!(serde_json::to_string(&ModelId::Siglip2).unwrap(), "\"siglip2\"");
    }

    #[test]
    fn models_live_in_the_cache_never_the_library() {
        let dir = models_dir().expect("a home");
        assert!(dir.ends_with(".cache/photo_pipeline/models"));
        assert!(model_dir(ModelId::Siglip2).unwrap().starts_with(&dir));
    }
}
