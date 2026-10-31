//! Fursuit heads: OWLv2
//! finds them in a photo by the words "a fursuit head", and the search
//! model's image tower (SigLIP2) turns each head's crop into a vector that
//! tells one suit from another once whitened (`crate::search::whiten`).
//! This module is the helper's side: the detector session, the
//! preprocessing, the box decoding and the crops.  What is kept and how
//! suits are grouped is `crate::search::heads`.
//!
//! **Measured**: on the research previews the ONNX graph gives the same
//! heads with scores within 0.02 of the PyTorch run.  **The head vector**
//! (v2) is the installed SigLIP2 B/16 tower on the same crop the research
//! harness used (the box widened 12 % each side, square, black beyond the
//! edge), through the search step's own input (squashed to 256, mean and
//! std 0.5): whitened over the library's heads it beat DINOv2 on every
//! time tier of the user's 791 labelled pairs (AUC 0.92–0.97).  DINOv2 is
//! not run any more.
//!
//! **Kinds never mix.**  A [`Head`] is a fursuit head and nothing else:
//! it never passes through the face pipeline, and its vector is kept in
//! its own store of its own width ([`DIM`]), apart from the search index.

use std::path::{Path, PathBuf};

use half::f16;
use ort::session::Session;
use ort::value::{Tensor, TensorElementType, ValueType};
use serde::{Deserialize, Serialize};

use super::client::FileError;
use super::models::{self, ModelId};
use super::worker::{Load, card_failure, cpu_session, cpu_threads, gpu_session};
use super::{CardFailure, Device, SelfCheck};
use crate::imaging::decode::{self, Pixels};
use crate::imaging::libraw::{Raw, Thumbnail};
use crate::model::record::FileType;
use crate::search::index::{KEY_HEAD_BYTES, Key};
use crate::search::query::normalised;

/// OWLv2's input edge: the preview is fitted into this square.
pub const OWL_INPUT: u32 = 960;
/// The words OWLv2 is asked for.
pub const PROMPT: &str = "a fursuit head";
/// OWLv2's text context (CLIP's, padded).
pub const TOKENS: usize = 16;
/// A head's lowest score (the research sheets were made at this).
pub const SCORE_MIN: f32 = 0.3;
/// A large head needs less certainty: a box at least [`BIG_SIDE`] of the
/// preview's long edge is kept from this score.  Round 2 (2026-10-17): a
/// quarter of the library's fursuit photos had no head at 0.3, and most
/// of them showed one large, clear head (white and pale suits, stage
/// light, heads filling the frame) scoring 0.2–0.3: on 400 such photos
/// the best box was at least 0.2 in 77 % and at least 0.13 of the long
/// edge in 90 % (median 0.30); 85–90 % of the boxes in the band were
/// heads by eye.  The band's small boxes are backs, paws and shadows,
/// so the size condition keeps most of the gain (63 % of those photos)
/// for half the extra boxes on photos that already had heads.
pub const BIG_SCORE_MIN: f32 = 0.2;
pub const BIG_SIDE: f32 = 0.15;
/// Boxes overlapping more than this (intersection over union) are one head.
pub const NMS_OVERLAP: f32 = 0.4;
/// A head's box must be at least this share of the preview's long edge
/// (~100 px of a 1616-px preview): smaller crops don't tell suits apart.
pub const MIN_SIDE: f32 = 0.06;
/// The crop around a head: a square, this much wider than the box on each
/// side.
pub const CROP_MARGIN: f32 = 0.12;
/// Decode the preview so its long edge is at least this (OWLv2 sees 960 px;
/// a 1616-px MPF preview is decoded whole).
pub const PREVIEW_EDGE: u32 = 960;
/// The head vector: the search model's pooled image vector (SigLIP2 B/16,
/// 768 wide), unit length, before whitening.
pub const DIM: usize = 768;
/// Photos per OWLv2 call: one, on either device.  Measured on the card
/// (WebGPU): 168 ms a photo one at a time against 852 ms each in fours.
pub const CHUNK_GPU: usize = 1;
pub const CHUNK_CPU: usize = 1;
/// Crops per image-tower call.
pub const EMBED_CHUNK: usize = 32;

const CLIP_MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const CLIP_STD: [f32; 3] = [0.268_629_54, 0.261_302_58, 0.275_777_11];
/// OWLv2 pads the square with mid-grey (0.5 before normalising).
const PAD_GREY: f32 = 0.5;
const PAD_TOKEN: i64 = 0;

/// One fursuit head found in a photo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Head {
    /// `[x, y, width, height]` as fractions of the oriented preview.
    pub bbox: [f32; 4],
    /// OWLv2's score for "a fursuit head" (sigmoid).
    pub score: f32,
    /// The box's longer side in preview pixels.
    pub size: u32,
    /// How sharp the crop is (mean edge strength, 0..1): part of clarity.
    pub sharpness: f32,
    /// The search model's unit vector of the crop, [`DIM`] long (empty if
    /// the crop couldn't be embedded).
    pub vector: Vec<f32>,
    /// A `worker::TILE_PX` square JPEG around the head, for the page
    /// (empty when only the vector was asked for: [`KnownHeads`]).
    pub tile: Vec<u8>,
}

/// A photo whose heads are already known (their boxes), for the vectors
/// alone: the catch-up of libraries scanned before v2's vectors, which
/// must keep every head's identity (its key and box) as the user's names
/// point at it.  `key` is checked against the file as read: a photo that
/// changed is reported, not embedded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownHeads {
    pub path: PathBuf,
    pub file_type: FileType,
    pub key: Key,
    /// `[x, y, width, height]` as fractions of the oriented preview.
    pub boxes: Vec<[f32; 4]>,
}

/// A photo's heads, with the key the helper read for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoHeads {
    pub key: Key,
    pub heads: Vec<Head>,
}

/// One photo's result from a heads batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadsResult {
    pub path: PathBuf,
    pub result: Result<PhotoHeads, FileError>,
}

/// A detection before its crop: the box (fractions of the preview, `[x,
/// y, w, h]`) and the score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Det {
    pub bbox: [f32; 4],
    pub score: f32,
}

/// Something that embeds head crops: the search model's image tower in
/// the helper (`worker::Models::embed_crops`), a double in tests.  Unit
/// vectors, one per crop, [`DIM`] long.
pub type Embed<'a> = dyn FnMut(&[Pixels]) -> ort::Result<Vec<Vec<f32>>> + 'a;

/// The detector session.
pub struct HeadModels {
    pub self_check: SelfCheck,
    owl: Session,
    ids: Vec<i64>,
    mask: Vec<i64>,
}

impl HeadModels {
    /// Load OWLv2 from its model folder on `device`.  On the card its
    /// answers are checked against the processor's on a fixed input (the
    /// same tolerance as search's self-check); a failure is a
    /// [`CardFailure`].  The head vectors come from the search model's
    /// tower, loaded and checked by the helper already.
    pub fn load(owl_dir: &Path, device: Device) -> Result<HeadModels, Load> {
        let owl_info = models::info(ModelId::Owlv2);
        let owl_path = owl_dir.join(owl_info.files[0].file);
        let tokenizer = tokenizers::Tokenizer::from_file(owl_dir.join(owl_info.files[1].file)).map_err(|e| Load::Other(format!("OWLv2 tokenizer: {e}")))?;
        let (ids, mask) = prompt_tokens(&tokenizer).map_err(Load::Other)?;
        let threads = cpu_threads();
        let mut out = match device {
            Device::Cpu => HeadModels { self_check: SelfCheck::NotRun, owl: cpu_session(&owl_path, threads).map_err(|e| Load::Other(format!("OWLv2: {e}")))?, ids, mask },
            Device::Gpu => {
                let owl = gpu_session(&owl_path).map_err(|e| Load::Card(card_failure(&e)))?;
                HeadModels { self_check: SelfCheck::NotRun, owl, ids, mask }
            }
        };
        if device == Device::Gpu {
            let mut cpu = HeadModels { self_check: SelfCheck::NotRun, owl: cpu_session(&owl_path, threads).map_err(|e| Load::Other(format!("OWLv2: {e}")))?, ids: out.ids.clone(), mask: out.mask.clone() };
            let (cosine, max_diff) = out.self_check_against(&mut cpu).map_err(|e| Load::Card(card_failure(&e)))?;
            eprintln!("ml-worker: heads self-check cosine {cosine:.6}, largest difference {max_diff:.2e}");
            if !super::self_check_passes(cosine, max_diff) {
                return Err(Load::Card(CardFailure::SelfCheckFailed { cosine, max_diff }));
            }
            out.self_check = SelfCheck::Passed { cosine, max_diff };
        }
        Ok(out)
    }

    /// A fixed input through both: the cosine and the largest difference
    /// over OWLv2's score vector.
    fn self_check_against(&mut self, other: &mut HeadModels) -> ort::Result<(f32, f32)> {
        let n = (OWL_INPUT * OWL_INPUT) as usize;
        let data: Vec<u8> = (0..3 * n).map(|i| ((i * 7 + (i / n) * 13) % 251) as u8).collect();
        let px = Pixels::new(OWL_INPUT, OWL_INPUT, 3, data);
        let a = self.scores(&[&px])?;
        let b = other.scores(&[&px])?;
        Ok(compare(&a[0], &b[0]))
    }

    /// OWLv2 over previews: per photo, the sigmoid score of every anchor
    /// and the raw boxes, for [`decode`].
    fn run_owl(&mut self, imgs: &[&Pixels]) -> ort::Result<(Vec<f32>, Vec<f32>, usize)> {
        let n = imgs.len();
        let per = (3 * OWL_INPUT * OWL_INPUT) as usize;
        let mut pixels = Vec::with_capacity(n * per);
        for px in imgs {
            pixels.extend(owl_input(px));
        }
        // The export takes one text row per image in the batch (HF's
        // `[batch × queries, tokens]`): the same prompt, repeated.
        let ids: Vec<i64> = self.ids.iter().copied().cycle().take(n * TOKENS).collect();
        let mask: Vec<i64> = self.mask.iter().copied().cycle().take(n * TOKENS).collect();
        let names: Vec<String> = self.owl.inputs().iter().map(|i| i.name().to_string()).collect();
        let mut inputs: Vec<(String, ort::session::SessionInputValue<'_>)> = Vec::new();
        for name in &names {
            let v = match name.as_str() {
                "input_ids" => Tensor::from_array((vec![n as i64, TOKENS as i64], ids.clone()))?.into(),
                "attention_mask" => Tensor::from_array((vec![n as i64, TOKENS as i64], mask.clone()))?.into(),
                _ => Tensor::from_array((vec![n as i64, 3, OWL_INPUT as i64, OWL_INPUT as i64], std::mem::take(&mut pixels)))?.into(),
            };
            inputs.push((name.clone(), v));
        }
        let outputs = self.owl.run(inputs)?;
        let (lshape, logits) = extract(&outputs, "logits")?;
        let (bshape, boxes) = extract(&outputs, "pred_boxes")?;
        let anchors = *lshape.get(1).unwrap_or(&0) as usize;
        if anchors == 0 || lshape.len() != 3 || bshape.len() != 3 || bshape[1] as usize != anchors || logits.len() != n * anchors * lshape[2] as usize {
            return Err(ort::Error::new(format!("unexpected OWLv2 output shapes {lshape:?} {bshape:?}")));
        }
        // The first (only) text query's logit per anchor, as a probability.
        let q = lshape[2] as usize;
        let scores: Vec<f32> = logits.chunks_exact(q).map(|c| sigmoid(c[0])).collect();
        Ok((scores, boxes, anchors))
    }

    /// Per photo, the score vector alone (the self-check).
    fn scores(&mut self, imgs: &[&Pixels]) -> ort::Result<Vec<Vec<f32>>> {
        let (scores, _, anchors) = self.run_owl(imgs)?;
        Ok(scores.chunks_exact(anchors).map(<[f32]>::to_vec).collect())
    }

    /// The heads OWLv2 finds in each preview: boxes above [`SCORE_MIN`],
    /// one per head ([`NMS_OVERLAP`]), at least [`MIN_SIDE`] of the edge.
    pub fn detect(&mut self, imgs: &[&Pixels]) -> ort::Result<Vec<Vec<Det>>> {
        let (scores, boxes, anchors) = self.run_owl(imgs)?;
        Ok(imgs
            .iter()
            .enumerate()
            .map(|(i, px)| {
                let s = &scores[i * anchors..(i + 1) * anchors];
                let b = &boxes[i * anchors * 4..(i + 1) * anchors * 4];
                decode(s, b, px.width, px.height)
            })
            .collect())
    }

    /// Everything for a batch of previews: the heads with their crops'
    /// vectors (through `embed`, the search model's tower), per photo.
    pub fn find(&mut self, imgs: &[&Pixels], embed: &mut Embed<'_>) -> ort::Result<Vec<Vec<Head>>> {
        let dets = self.detect(imgs)?;
        let mut crops = Vec::new();
        for (px, ds) in imgs.iter().zip(&dets) {
            for d in ds {
                crops.push(crop_square(px, d.bbox, CROP_MARGIN));
            }
        }
        let vectors = if crops.is_empty() { Vec::new() } else { embed(&crops)? };
        if vectors.len() != crops.len() {
            return Err(ort::Error::new(format!("{} crops embedded as {} vectors", crops.len(), vectors.len())));
        }
        let mut vi = vectors.into_iter();
        let mut ci = crops.iter();
        Ok(imgs
            .iter()
            .zip(dets)
            .map(|(px, ds)| {
                ds.into_iter()
                    .filter_map(|d| {
                        let (crop, vector) = (ci.next()?, vi.next()?);
                        let size = (d.bbox[2] * px.width as f32).max(d.bbox[3] * px.height as f32).round() as u32;
                        Some(Head { bbox: d.bbox, score: d.score, size, sharpness: sharpness(crop), vector, tile: super::worker::tile(px, &d.bbox) })
                    })
                    .collect()
            })
            .collect())
    }
}

/// The prompt's token ids, padded to [`TOKENS`], and the attention mask.
fn prompt_tokens(tok: &tokenizers::Tokenizer) -> Result<(Vec<i64>, Vec<i64>), String> {
    let enc = tok.encode(PROMPT, true).map_err(|e| e.to_string())?;
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
    if ids.len() > TOKENS {
        return Err(format!("the prompt is {} tokens, over {TOKENS}", ids.len()));
    }
    let n = ids.len();
    ids.resize(TOKENS, PAD_TOKEN);
    let mask = (0..TOKENS).map(|i| if i < n { 1 } else { 0 }).collect();
    Ok((ids, mask))
}

/// A named f32 output's shape and data.
fn extract(outputs: &ort::session::SessionOutputs<'_>, name: &str) -> ort::Result<(Vec<i64>, Vec<f32>)> {
    let v = outputs.get(name).ok_or_else(|| ort::Error::new(format!("the model has no {name} output")))?;
    match v.dtype() {
        ValueType::Tensor { ty: TensorElementType::Float16, .. } => {
            let (s, d) = v.try_extract_tensor::<f16>()?;
            Ok((s.to_vec(), d.iter().map(|h| h.to_f32()).collect()))
        }
        _ => {
            let (s, d) = v.try_extract_tensor::<f32>()?;
            Ok((s.to_vec(), d.to_vec()))
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// (cosine, largest difference) between two vectors.
fn compare(a: &[f32], b: &[f32]) -> (f32, f32) {
    let (a, b) = (normalised(a), normalised(b));
    let cos: f32 = a.iter().zip(&b).map(|(p, q)| p * q).sum();
    let diff = a.iter().zip(&b).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
    (cos, diff)
}

/// RGB (or grey) pixels of `px` at (x, y), as 0..1 per channel.
fn rgb_at(px: &Pixels, x: u32, y: u32) -> [f32; 3] {
    let c = px.channels as usize;
    let i = (y as usize * px.width as usize + x as usize) * c;
    let p = &px.data[i..i + c];
    if c >= 3 { [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0] } else { [p[0] as f32 / 255.0; 3] }
}

/// OWLv2's input: the preview fitted into a [`OWL_INPUT`] square at the
/// top left (long edge scaled to the square), the rest mid-grey, CLIP
/// normalisation, CHW.
pub fn owl_input(px: &Pixels) -> Vec<f32> {
    let n = (OWL_INPUT * OWL_INPUT) as usize;
    let long = px.width.max(px.height).max(1) as f32;
    let (w, h) = (((px.width as f32 / long) * OWL_INPUT as f32).round().clamp(1.0, OWL_INPUT as f32) as u32, ((px.height as f32 / long) * OWL_INPUT as f32).round().clamp(1.0, OWL_INPUT as f32) as u32);
    let fitted = decode::resize_to(px, w, h);
    let mut out = vec![0f32; 3 * n];
    for ch in 0..3 {
        let grey = (PAD_GREY - CLIP_MEAN[ch]) / CLIP_STD[ch];
        out[ch * n..(ch + 1) * n].fill(grey);
    }
    for y in 0..h {
        for x in 0..w {
            let p = rgb_at(&fitted, x, y);
            let i = (y * OWL_INPUT + x) as usize;
            for ch in 0..3 {
                out[ch * n + i] = (p[ch] - CLIP_MEAN[ch]) / CLIP_STD[ch];
            }
        }
    }
    out
}

/// OWLv2's anchors into heads: `scores` per anchor, `boxes` as `(cx, cy,
/// w, h)` fractions of the padded square whose side is the preview's long
/// edge; out come boxes as `[x, y, w, h]` fractions of the `w`×`h`
/// preview, clamped to it, above [`SCORE_MIN`] (or above
/// [`BIG_SCORE_MIN`] when at least [`BIG_SIDE`] of the long edge), one
/// per head, at least [`MIN_SIDE`] of an edge, best first.
pub fn decode(scores: &[f32], boxes: &[f32], w: u32, h: u32) -> Vec<Det> {
    let side = w.max(h).max(1) as f32;
    let (sx, sy) = (side / w.max(1) as f32, side / h.max(1) as f32);
    let longer = |d: &Det| (d.bbox[2] * w as f32).max(d.bbox[3] * h as f32);
    let mut dets: Vec<Det> = scores
        .iter()
        .zip(boxes.chunks_exact(4))
        .filter(|(s, _)| **s >= BIG_SCORE_MIN)
        .map(|(&score, b)| {
            let x0 = ((b[0] - b[2] / 2.0) * sx).clamp(0.0, 1.0);
            let y0 = ((b[1] - b[3] / 2.0) * sy).clamp(0.0, 1.0);
            let x1 = ((b[0] + b[2] / 2.0) * sx).clamp(0.0, 1.0);
            let y1 = ((b[1] + b[3] / 2.0) * sy).clamp(0.0, 1.0);
            Det { bbox: [x0, y0, x1 - x0, y1 - y0], score }
        })
        .filter(|d| d.bbox[2] > 0.0 && d.bbox[3] > 0.0)
        // A box in the band earns its place by size, before NMS, so a
        // small uncertain box never suppresses a head.
        .filter(|d| d.score >= SCORE_MIN || longer(d) >= BIG_SIDE * side)
        .collect();
    dets = nms(dets, NMS_OVERLAP);
    // The size gate is in pixels of the long edge, whichever way the box
    // lies (a fraction of the short edge is fewer pixels).
    let floor = MIN_SIDE * side;
    dets.retain(|d| longer(d) >= floor);
    dets
}

/// Intersection over union of two `[x, y, w, h]` boxes.
pub fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// Keep the best of overlapping boxes; best first.
pub fn nms(mut dets: Vec<Det>, overlap: f32) -> Vec<Det> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Det> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| iou(k.bbox, d.bbox) < overlap) {
            keep.push(d);
        }
    }
    keep
}

/// The square crop around a box: centred on it, its side the box's longer
/// side widened by `margin` on each side; what falls outside the preview
/// is black (as the research crops were), so a head at the edge keeps its
/// scale.
pub fn crop_square(px: &Pixels, bbox: [f32; 4], margin: f32) -> Pixels {
    let (w, h) = (px.width as f32, px.height as f32);
    let side = ((bbox[2] * w).max(bbox[3] * h) * (1.0 + 2.0 * margin)).round().max(2.0) as u32;
    let cx = (bbox[0] + bbox[2] / 2.0) * w;
    let cy = (bbox[1] + bbox[3] / 2.0) * h;
    let x0 = (cx - side as f32 / 2.0).round() as i64;
    let y0 = (cy - side as f32 / 2.0).round() as i64;
    let c = px.channels as usize;
    let mut out = vec![0u8; side as usize * side as usize * c];
    let stride = px.width as usize * c;
    for row in 0..side as i64 {
        let sy = y0 + row;
        if sy < 0 || sy >= px.height as i64 {
            continue;
        }
        let from = x0.max(0);
        let to = (x0 + side as i64).min(px.width as i64);
        if to <= from {
            continue;
        }
        let src = sy as usize * stride + from as usize * c;
        let len = (to - from) as usize * c;
        let dst = row as usize * side as usize * c + (from - x0) as usize * c;
        out[dst..dst + len].copy_from_slice(&px.data[src..src + len]);
    }
    Pixels::new(side, side, px.channels, out)
}

/// The vectors of heads already known in a preview (`boxes` as the
/// detector left them): the same crop as [`HeadModels::find`] makes, through
/// `embed`.  Score and tile are left empty: the store keeps the detection
/// it has and takes the vector alone.
pub fn embed_known(px: &Pixels, boxes: &[[f32; 4]], embed: &mut Embed<'_>) -> ort::Result<Vec<Head>> {
    let crops: Vec<Pixels> = boxes.iter().map(|&b| crop_square(px, b, CROP_MARGIN)).collect();
    let vectors = if crops.is_empty() { Vec::new() } else { embed(&crops)? };
    if vectors.len() != crops.len() {
        return Err(ort::Error::new(format!("{} crops embedded as {} vectors", crops.len(), vectors.len())));
    }
    Ok(boxes
        .iter()
        .zip(&crops)
        .zip(vectors)
        .map(|((&bbox, crop), vector)| {
            let size = (bbox[2] * px.width as f32).max(bbox[3] * px.height as f32).round() as u32;
            Head { bbox, score: 0.0, size, sharpness: sharpness(crop), vector, tile: Vec::new() }
        })
        .collect())
}

/// How sharp a crop is: the mean absolute Laplacian of its luma at 64 px,
/// 0..1.  Blurred and distant heads score low.
pub fn sharpness(crop: &Pixels) -> f32 {
    let small = decode::resize_to(crop, 64, 64);
    let luma = decode::to_luma(&small);
    let (w, h) = (luma.width as usize, luma.height as usize);
    if w < 3 || h < 3 {
        return 0.0;
    }
    let at = |x: usize, y: usize| luma.data[y * w + x] as f32;
    let mut sum = 0.0f32;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            sum += (4.0 * at(x, y) - at(x - 1, y) - at(x + 1, y) - at(x, y - 1) - at(x, y + 1)).abs();
        }
    }
    (sum / ((w - 2) * (h - 2)) as f32 / 255.0).clamp(0.0, 1.0)
}

/// Read a photo's preview for the head step: the same preview the search
/// step reads (the JPG's MPF preview, else its own image at a reduced
/// size; a RAW-only shot's embedded preview), decoded so its long edge is
/// at least [`PREVIEW_EDGE`] where it allows, oriented as shown; with the
/// content key from the same header.
pub fn read_preview(path: &Path, file_type: FileType) -> Result<(Key, Pixels), FileError> {
    let size = std::fs::metadata(path).map_err(|e| FileError::io(&e))?.len();
    match file_type {
        FileType::Jpg => {
            if let Some((data, head)) = crate::exif::jpeg_preview_bytes(path, PREVIEW_EDGE / 2) {
                let key = Key::of(size, &head);
                let orientation = crate::exif::orientation_from_jpeg_head(&head);
                return decode_oriented(&data, orientation).map(|px| (key, px));
            }
            let data = std::fs::read(path).map_err(|e| FileError::io(&e))?;
            let key = Key::of(size, &data);
            let orientation = crate::exif::orientation_from_jpeg_head(&data);
            decode_oriented(&data, orientation).map(|px| (key, px))
        }
        FileType::Raw => {
            let head = {
                use std::io::Read;
                let mut f = crate::exif::open_for_header(path).map_err(|e| FileError::io(&e))?;
                let mut buf = Vec::with_capacity(KEY_HEAD_BYTES);
                f.by_ref().take(KEY_HEAD_BYTES as u64).read_to_end(&mut buf).map_err(|e| FileError::io(&e))?;
                buf
            };
            let key = Key::of(size, &head);
            let mut raw = Raw::open(path).map_err(|e| FileError::other(format!("RAW: {e}")))?;
            let flip = raw.exif_orientation();
            match raw.thumbnail_at_least(PREVIEW_EDGE / 2).map_err(|e| FileError::other(format!("RAW preview: {e}")))? {
                Thumbnail::Jpeg(bytes) => {
                    let orientation = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
                    decode_oriented(&bytes, orientation).map(|px| (key, px))
                }
                Thumbnail::Bitmap { width, height, colors, data } => {
                    let px = crate::imaging::thumbs::bitmap(width, height, colors, data).ok_or_else(|| FileError::other("RAW preview is not readable"))?;
                    Ok((key, decode::apply_orientation(px, flip)))
                }
            }
        }
    }
}

/// Decode a JPEG at the largest DCT reduction that keeps its long edge at
/// least [`PREVIEW_EDGE`], and orient it.
fn decode_oriented(jpeg: &[u8], orientation: u8) -> Result<Pixels, FileError> {
    let (w, h) = decode::jpeg_dimensions(jpeg).ok_or_else(|| FileError::other("not a readable JPEG"))?;
    let denom = [8, 4, 2].into_iter().find(|&d| w.max(h) / d >= PREVIEW_EDGE).unwrap_or(1);
    let px = decode::decode_jpeg_full_scaled(jpeg, denom).map_err(FileError::other)?;
    Ok(decode::apply_orientation(px, orientation))
}

/// The helper's heads loop over one request: previews are read and
/// decoded on `pool`'s threads ahead of inference; photos go through the
/// detector `chunk` at a time and their crops through `embed`; one result
/// per path, in order.
pub fn serve(models: &mut HeadModels, embed: &mut Embed<'_>, pool: &rayon::ThreadPool, files: &[(PathBuf, FileType)], chunk: usize) -> Vec<HeadsResult> {
    use std::sync::mpsc;
    let mut results: Vec<Option<HeadsResult>> = vec![None; files.len()];
    let (ptx, prx) = mpsc::sync_channel::<(usize, Result<(Key, Pixels), FileError>)>(chunk.max(1) * 2);
    let paths: Vec<(PathBuf, FileType)> = files.to_vec();
    pool.spawn(move || {
        use rayon::prelude::*;
        paths.par_iter().enumerate().for_each(|(i, (p, t))| {
            let _ = ptx.send((i, read_preview(p, *t)));
        });
    });
    let mut ready: Vec<(usize, Key, Pixels)> = Vec::with_capacity(chunk);
    let mut flush = |ready: &mut Vec<(usize, Key, Pixels)>, results: &mut Vec<Option<HeadsResult>>, models: &mut HeadModels| {
        if ready.is_empty() {
            return;
        }
        let imgs: Vec<&Pixels> = ready.iter().map(|(_, _, p)| p).collect();
        match models.find(&imgs, embed) {
            Ok(found) => {
                for ((i, key, _), heads) in ready.drain(..).zip(found) {
                    results[i] = Some(HeadsResult { path: files[i].0.clone(), result: Ok(PhotoHeads { key, heads }) });
                }
            }
            Err(e) => {
                for (i, _, _) in ready.drain(..) {
                    results[i] = Some(HeadsResult { path: files[i].0.clone(), result: Err(FileError::other(format!("model: {e}"))) });
                }
            }
        }
    };
    for (i, r) in prx {
        match r {
            Ok((key, px)) => ready.push((i, key, px)),
            Err(e) => results[i] = Some(HeadsResult { path: files[i].0.clone(), result: Err(e) }),
        }
        if ready.len() >= chunk.max(1) {
            flush(&mut ready, &mut results, models);
        }
    }
    flush(&mut ready, &mut results, models);
    results
        .into_iter()
        .zip(files)
        .map(|(r, (p, _))| r.unwrap_or_else(|| HeadsResult { path: p.clone(), result: Err(FileError::other("not prepared")) }))
        .collect()
}

/// The helper's loop for heads already known ([`KnownHeads`]): previews
/// are read on `pool`'s threads; a photo whose key is not the one asked
/// for (the file changed) is reported, not embedded; the known boxes are
/// cropped and embedded through `embed`.  One result per entry, in order.
pub fn serve_known(embed: &mut Embed<'_>, pool: &rayon::ThreadPool, files: &[KnownHeads]) -> Vec<HeadsResult> {
    use std::sync::mpsc;
    let mut results: Vec<Option<HeadsResult>> = vec![None; files.len()];
    let (ptx, prx) = mpsc::sync_channel::<(usize, Result<(Key, Pixels), FileError>)>(2 * EMBED_CHUNK);
    let jobs: Vec<KnownHeads> = files.to_vec();
    pool.spawn(move || {
        use rayon::prelude::*;
        jobs.par_iter().enumerate().for_each(|(i, j)| {
            let _ = ptx.send((i, read_preview(&j.path, j.file_type)));
        });
    });
    for (i, r) in prx {
        let job = &files[i];
        let result = match r {
            Err(e) => Err(e),
            Ok((key, _)) if key != job.key => Err(FileError::other("the photo changed since its heads were found")),
            Ok((key, px)) => embed_known(&px, &job.boxes, embed).map(|heads| PhotoHeads { key, heads }).map_err(|e| FileError::other(format!("model: {e}"))),
        };
        results[i] = Some(HeadsResult { path: job.path.clone(), result });
    }
    results
        .into_iter()
        .zip(files)
        .map(|(r, j)| r.unwrap_or_else(|| HeadsResult { path: j.path.clone(), result: Err(FileError::other("not prepared")) }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: f32, y: f32, w: f32, h: f32, score: f32) -> Det {
        Det { bbox: [x, y, w, h], score }
    }

    #[test]
    fn boxes_decode_from_the_padded_square_into_the_preview() {
        // A 1600×1000 preview: the square's side is 1600, so a box's y
        // stretches by 1.6 and x not at all.  Anchor 0 scores high, anchor
        // 1 overlaps it (dropped by NMS), anchor 2 is tiny, anchor 3 is
        // under the score floor (large, but under the lower bar too).
        let scores = [0.9, 0.5, 0.8, 0.19];
        let boxes = [
            0.25, 0.25, 0.2, 0.2, // → x 0.15..0.35, y 0.24..0.56
            0.26, 0.25, 0.2, 0.2, // the same head
            0.7, 0.3, 0.02, 0.02, // 2 % of the edge: too small
            0.7, 0.7, 0.2, 0.2,
        ];
        let out = decode(&scores, &boxes, 1600, 1000);
        assert_eq!(out.len(), 1, "{out:?}");
        let b = out[0].bbox;
        assert!((b[0] - 0.15).abs() < 1e-5 && (b[1] - 0.24).abs() < 1e-5 && (b[2] - 0.2).abs() < 1e-5 && (b[3] - 0.32).abs() < 1e-5, "{b:?}");
        assert_eq!(out[0].score, 0.9);
        // A box hanging over the edge is clamped.
        let out = decode(&[0.9], &[0.0, 0.0, 0.4, 0.4], 1000, 1000);
        assert_eq!(out[0].bbox, [0.0, 0.0, 0.2, 0.2]);
        // Under the floor: a large box (half the edge) is kept from 0.2, a
        // small one (a tenth) is not; under 0.2 nothing is.
        assert_eq!(decode(&[0.29], &[0.5, 0.5, 0.5, 0.5], 1000, 1000).len(), 1);
        assert_eq!(decode(&[0.2], &[0.5, 0.5, 0.16, 0.16], 1000, 1000).len(), 1, "just over BIG_SIDE of the edge, at the lower bar's score");
        assert!(decode(&[0.29], &[0.5, 0.5, 0.1, 0.1], 1000, 1000).is_empty());
        assert!(decode(&[0.19], &[0.5, 0.5, 0.5, 0.5], 1000, 1000).is_empty());
        // A small uncertain box overlapping a large one never takes its
        // place in NMS: it is out before NMS runs.
        let out = decode(&[0.29, 0.25], &[0.5, 0.5, 0.1, 0.1, 0.5, 0.5, 0.5, 0.5], 1000, 1000);
        assert_eq!((out.len(), out[0].score), (1, 0.25), "{out:?}");
        // The size gate counts pixels of the long edge: on 1600×1000, a box
        // 7 % of the height tall (70 px, 4.4 % of 1600) is too small, one
        // 7 % of the width wide (112 px) is not.
        assert!(decode(&[0.9], &[0.5, 0.3, 0.02, 0.07 * 1000.0 / 1600.0], 1600, 1000).is_empty());
        assert_eq!(decode(&[0.9], &[0.5, 0.3, 0.07, 0.02], 1600, 1000).len(), 1);
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes_best_first() {
        let kept = nms(vec![det(0.0, 0.0, 0.2, 0.2, 0.5), det(0.01, 0.0, 0.2, 0.2, 0.9), det(0.5, 0.5, 0.2, 0.2, 0.7)], NMS_OVERLAP);
        assert_eq!(kept.iter().map(|d| d.score).collect::<Vec<_>>(), [0.9, 0.7]);
        assert!((iou([0.0, 0.0, 1.0, 1.0], [0.5, 0.0, 1.0, 1.0]) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(iou([0.0, 0.0, 0.1, 0.1], [0.5, 0.5, 0.1, 0.1]), 0.0);
    }

    #[test]
    fn the_owl_input_fits_the_preview_top_left_and_pads_grey() {
        // A 400×200 white image: the top half of the square is white, the
        // bottom half grey, in every channel.
        let px = Pixels::new(400, 200, 3, vec![255; 400 * 200 * 3]);
        let x = owl_input(&px);
        let n = (OWL_INPUT * OWL_INPUT) as usize;
        assert_eq!(x.len(), 3 * n);
        let white = (1.0 - CLIP_MEAN[0]) / CLIP_STD[0];
        let grey = (PAD_GREY - CLIP_MEAN[0]) / CLIP_STD[0];
        assert!((x[0] - white).abs() < 1e-5, "top left is the image");
        assert!((x[(OWL_INPUT * 479) as usize] - white).abs() < 1e-5, "row 479 is still the image (200/400 × 960 = 480 rows)");
        assert!((x[(OWL_INPUT * 480) as usize] - grey).abs() < 1e-5, "row 480 is padding");
        assert!((x[3 * n - 1] - (PAD_GREY - CLIP_MEAN[2]) / CLIP_STD[2]).abs() < 1e-5);
        // A grey (one-channel) image is repeated across the channels.
        let g = owl_input(&Pixels::new(10, 10, 1, vec![0; 100]));
        let black_r = (0.0 - CLIP_MEAN[0]) / CLIP_STD[0];
        let black_b = (0.0 - CLIP_MEAN[2]) / CLIP_STD[2];
        assert!((g[0] - black_r).abs() < 1e-5 && (g[2 * n] - black_b).abs() < 1e-5);
    }

    #[test]
    fn crops_are_square_with_a_margin_and_black_beyond_the_edge() {
        // A 100×100 image, white; a box of 20×10 at the left edge, (0, 10)
        // → side 20 × 1.24 = 25 (rounded), centred on (10, 15): x −3..22
        // (−2.5 rounds away from zero), y 3..28.
        let px = Pixels::new(100, 100, 3, vec![255; 100 * 100 * 3]);
        let c = crop_square(&px, [0.0, 0.1, 0.2, 0.1], CROP_MARGIN);
        assert_eq!((c.width, c.height, c.channels), (25, 25, 3));
        assert_eq!(&c.data[0..3], &[0, 0, 0], "outside the image on the left: black");
        assert_eq!(&c.data[2 * 3..2 * 3 + 3], &[0, 0, 0], "still outside at crop x = 2");
        assert_eq!(&c.data[3 * 3..3 * 3 + 3], &[255, 255, 255], "crop x = 3 is x = 0 of the image");
        // A box well inside stays inside: nothing black.
        let inner = crop_square(&px, [0.1, 0.1, 0.2, 0.1], CROP_MARGIN);
        assert!(inner.data.iter().all(|&v| v == 255));
        let (cos, diff) = compare(&[1.0, 0.0], &[1.0, 0.0]);
        assert!((cos - 1.0).abs() < 1e-6 && diff == 0.0);
    }

    /// v2: known heads are cut exactly as the detector's step cuts them
    /// (the same square, the same margin) and go through the embedder in
    /// one call; the vectors come back in the boxes' order; a photo
    /// without boxes asks nothing.
    #[test]
    fn known_heads_are_cut_as_found_ones_and_embedded_in_order() {
        let mut data = vec![0u8; 200 * 100 * 3];
        for (i, p) in data.chunks_exact_mut(3).enumerate() {
            p.fill(((i % 200) + (i / 200)) as u8);
        }
        let px = Pixels::new(200, 100, 3, data);
        let boxes = [[0.1, 0.2, 0.1, 0.3], [0.6, 0.1, 0.2, 0.2]];
        let mut seen: Vec<Pixels> = Vec::new();
        let mut calls = 0;
        let mut embed = |crops: &[Pixels]| -> ort::Result<Vec<Vec<f32>>> {
            calls += 1;
            seen.extend(crops.iter().cloned());
            Ok(crops.iter().map(|c| vec![c.width as f32, 0.0]).collect())
        };
        let heads = embed_known(&px, &boxes, &mut embed).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(heads.len(), 2);
        for (h, b) in heads.iter().zip(&boxes) {
            assert_eq!(h.bbox, *b);
            assert!(h.tile.is_empty(), "the store keeps the tile it has");
        }
        for (got, b) in seen.iter().zip(&boxes) {
            let want = crop_square(&px, *b, CROP_MARGIN);
            assert!(got.width == want.width && got.height == want.height && got.data == want.data, "cut as the detector's step cuts");
        }
        assert_eq!(heads[0].vector, vec![seen[0].width as f32, 0.0]);
        assert_eq!(heads[1].size, 40, "the longer side in preview pixels");
        let mut never = |_: &[Pixels]| -> ort::Result<Vec<Vec<f32>>> { panic!("nothing to embed") };
        assert!(embed_known(&px, &[], &mut never).unwrap().is_empty());
        // An embedder that loses a crop is an error, never a shifted vector.
        let mut short = |_: &[Pixels]| -> ort::Result<Vec<Vec<f32>>> { Ok(vec![vec![1.0]]) };
        assert!(embed_known(&px, &boxes, &mut short).is_err());
    }

    #[test]
    fn sharpness_tells_texture_from_flat() {
        let flat = Pixels::new(64, 64, 3, vec![128; 64 * 64 * 3]);
        assert_eq!(sharpness(&flat), 0.0);
        let mut data = vec![0u8; 64 * 64 * 3];
        for (i, p) in data.chunks_exact_mut(3).enumerate() {
            let v = if (i % 64 + i / 64) % 2 == 0 { 255 } else { 0 };
            p.fill(v);
        }
        let s = sharpness(&Pixels::new(64, 64, 3, data));
        assert!(s > 0.5, "{s}");
    }

    #[test]
    fn a_plain_jpeg_preview_is_read_oriented_with_its_key() {
        let d = std::env::temp_dir().join(format!("pp-heads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let img = crate::imaging::quality::tests::texture(1200, 800, 5);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90).encode(&img.d, 1200, 800, jpeg_encoder::ColorType::Luma).unwrap();
        std::fs::write(d.join("a.jpg"), &out).unwrap();
        let (key, px) = read_preview(&d.join("a.jpg"), FileType::Jpg).expect("read");
        assert_eq!(key, Key::of(out.len() as u64, &out));
        assert_eq!((px.width, px.height), (1200, 800), "no reduction keeps the long edge at 960");
        let big = crate::imaging::quality::tests::texture(2400, 1600, 5);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90).encode(&big.d, 2400, 1600, jpeg_encoder::ColorType::Luma).unwrap();
        std::fs::write(d.join("b.jpg"), &out).unwrap();
        let (_, px) = read_preview(&d.join("b.jpg"), FileType::Jpg).expect("read");
        assert_eq!((px.width, px.height), (1200, 800), "halved: 1200 ≥ 960");
        assert!(matches!(read_preview(&d.join("missing.jpg"), FileType::Jpg), Err(e) if e.errno.is_some()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The real models on the research previews: boxes against the
    /// reference detections and the cost per photo.  `PP_MODEL_DIR=<models
    /// folder>` (with `owlv2-b16-ensemble/` and `siglip2-b16-256/`),
    /// `PP_HEADS_PREVIEWS=<folder of JPEGs>`, `PP_HEADS_DEVICE=gpu|cpu` (cpu
    /// by default),
    /// `PP_HEADS_N=<photos>` (16).  Prints per-photo times; `PP_HEADS_REF=
    /// <det_owlv2_*.json>` compares with the reference detections.
    #[test]
    #[ignore = "needs the downloaded models and previews (PP_MODEL_DIR, PP_HEADS_PREVIEWS)"]
    fn the_real_models_find_the_studys_heads() {
        let dir = PathBuf::from(std::env::var("PP_MODEL_DIR").expect("PP_MODEL_DIR"));
        let previews = PathBuf::from(std::env::var("PP_HEADS_PREVIEWS").expect("PP_HEADS_PREVIEWS"));
        let device = if std::env::var("PP_HEADS_DEVICE").as_deref() == Ok("gpu") { Device::Gpu } else { Device::Cpu };
        let n: usize = std::env::var("PP_HEADS_N").ok().and_then(|s| s.parse().ok()).unwrap_or(16);
        if device == Device::Gpu {
            super::super::gpu::pin().ok().expect("a card");
        }
        let t = std::time::Instant::now();
        let mut m = match HeadModels::load(&dir.join(ModelId::Owlv2.dir_name()), device) {
            Ok(m) => m,
            Err(Load::Card(f)) => panic!("card: {f:?}"),
            Err(Load::Other(e)) => panic!("{e}"),
        };
        let mut search = match super::super::worker::Models::load(&dir.join(ModelId::Siglip2.dir_name()), &dir, device, false, None, false) {
            Ok(m) => m,
            Err(Load::Card(f)) => panic!("card: {f:?}"),
            Err(Load::Other(e)) => panic!("{e}"),
        };
        let mut embed = |crops: &[Pixels]| search.embed_crops(crops);
        eprintln!("loaded in {:.1} s, self-check {:?}", t.elapsed().as_secs_f64(), m.self_check);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&previews).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jpg")).collect();
        files.sort();
        files.truncate(n);
        let chunk = std::env::var("PP_HEADS_CHUNK").ok().and_then(|s| s.parse().ok()).unwrap_or(if device == Device::Gpu { CHUNK_GPU } else { CHUNK_CPU });
        let mut imgs = Vec::new();
        for f in &files {
            let data = std::fs::read(f).unwrap();
            imgs.push(decode_oriented(&data, 1).unwrap());
        }
        let refs: Option<serde_json::Value> = std::env::var("PP_HEADS_REF").ok().map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap());
        let (mut heads, mut matched, mut studied) = (0, 0, 0);
        let mut owl_ms = 0.0;
        let mut all_ms = 0.0;
        let mut prep_ms = 0.0;
        let _ = m.find(&[&imgs[0]], &mut embed);
        for img in imgs.iter().take(8) {
            let t = std::time::Instant::now();
            let _ = owl_input(img);
            prep_ms += t.elapsed().as_secs_f64() * 1000.0 / 8.0;
        }
        for (ci, chunk_imgs) in imgs.chunks(chunk).enumerate() {
            let refs_imgs: Vec<&Pixels> = chunk_imgs.iter().collect();
            let t = std::time::Instant::now();
            let dets = m.detect(&refs_imgs).unwrap();
            owl_ms += t.elapsed().as_secs_f64() * 1000.0;
            let t = std::time::Instant::now();
            let found = m.find(&refs_imgs, &mut embed).unwrap();
            all_ms += t.elapsed().as_secs_f64() * 1000.0;
            for (k, (f, hs)) in chunk_imgs.iter().zip(&found).enumerate() {
                let file = &files[ci * chunk + k];
                heads += hs.len();
                assert_eq!(hs.len(), dets[k].len());
                for h in hs {
                    assert_eq!(h.vector.len(), DIM);
                    assert!((h.vector.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-3);
                    let long = f.width.max(f.height) as f32;
                    assert!((h.score >= SCORE_MIN || (h.score >= BIG_SCORE_MIN && h.size as f32 >= BIG_SIDE * long - 1.0)) && h.size >= (MIN_SIDE * long) as u32 - 1);
                }
                if let Some(r) = &refs {
                    let name = format!("prev/{}/{}", previews.file_name().unwrap().to_string_lossy(), file.file_name().unwrap().to_string_lossy());
                    if let Some(list) = r.get(&name).and_then(|v| v.as_array()) {
                        let study: Vec<[f32; 4]> = list
                            .iter()
                            .filter(|d| d["label"] == "a fursuit head" && d["score"].as_f64().unwrap() >= SCORE_MIN as f64)
                            .map(|d| {
                                let b: Vec<f32> = d["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
                                [b[0], b[1], b[2] - b[0], b[3] - b[1]]
                            })
                            .collect();
                        studied += study.len();
                        matched += hs.iter().filter(|h| study.iter().any(|s| iou(*s, h.bbox) > 0.6)).count();
                    }
                }
            }
        }
        let per = imgs.len() as f64;
        eprintln!(
            "{} photos on {:?} (chunk {chunk}): OWLv2 {:.0} ms a photo (of which the input {prep_ms:.0} ms), with crops and their vectors {:.0} ms a photo; {heads} heads, {matched} of them match the reference's {studied}",
            imgs.len(),
            device,
            owl_ms / per,
            all_ms / per
        );
        if refs.is_some() {
            assert!(matched as f64 >= 0.85 * studied.max(1) as f64, "{matched} of {studied}");
        }
    }
}
