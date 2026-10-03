//! The helper process: `photo-pipeline --ml-worker --device gpu|cpu
//! [--text-only] [--face-model <id>]` (plan §4.3).  The same binary,
//! started by [`super::client::Client`], so there is nothing extra to
//! install.  It loads the ONNX Runtime sessions on the chosen device, runs
//! the self-check on the card, reads previews itself (the app sends paths
//! and gets vectors back), batches them, and exits after a few idle
//! minutes so its memory goes back to the system.  A driver fault kills
//! the helper, not the app.
//!
//! **Protocol:** one JSON object per line on stdin (requests) and stdout
//! (replies).  The first reply is [`Reply::Ready`], [`Reply::CardFailed`]
//! or [`Reply::Failed`].  Replies carry their request's id and may come
//! out of order: a text query is answered between two image chunks of a
//! long batch, so searching stays quick while indexing runs.
//!
//! **The text tower always runs on the processor** (~14 ms a query), so
//! searching what is indexed works whatever the card does; with
//! `--text-only` nothing touches the card at all.
//!
//! **Faces** (phase 3): with `--face-model`, the helper also loads YuNet
//! and the chosen recognition model on the same device.  A batch that
//! asks for faces gets, per photo, the faces found on the detector's
//! canvas, cut from the full preview, aligned, recognised and tiled, in
//! the same read as the search vector (`super::face`, `super::prep`).

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use half::f16;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::{Tensor, TensorElementType, ValueType};
use serde::{Deserialize, Serialize};

use super::client::{Embedding, FaceAsk, FaceFound, FileError, ImageResult, Want};
use super::face::{self, ALIGNED, CANVAS_LEN, DET_SIZE, Norm};
use super::models::{self, ModelId};
use super::prep::{self, FacePrep, INPUT_SIZE, Prepared};
use super::{CardFailure, Device, SelfCheck};
use crate::imaging::decode::{self, Pixels};
use crate::model::record::FileType;

/// The flag that makes the binary a helper.
pub const FLAG: &str = "--ml-worker";
/// Minutes without a request after which the helper exits.
pub const IDLE_MINUTES: u64 = 3;
/// Photos per inference call on each device: small on the processor so a
/// text query waits at most a fraction of a second between chunks.
pub const CHUNK_GPU: usize = 32;
pub const CHUNK_CPU: usize = 8;
/// Canvases per detector call (a float canvas is 8 MB).
pub const DETECT_BATCH: usize = 8;
/// Aligned crops per recognition call.
pub const FACE_BATCH: usize = 32;
/// Threads reading and decoding previews (the share is bandwidth-bound;
/// `analysis` uses 4 for the same reason).
pub const READERS: usize = 6;
/// Tokens per query (SigLIP2's context length).
pub const CONTEXT: usize = 64;
/// A face's tile for the page: a square around the box, this wide, JPEG.
pub const TILE_PX: u32 = 128;
const TILE_QUALITY: u8 = 80;
const PAD: i64 = 0;
const EOS: i64 = 1;

/// What the app asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    /// Embed these photos; the reply holds one result per path, in order.
    /// `want` says whether the search vector, the faces, or both.
    EmbedImages { id: u64, files: Vec<(PathBuf, FileType)>, want: Want },
    /// Encode query texts on the processor.
    EncodeText { id: u64, texts: Vec<String> },
    /// Exit now.
    Quit,
}

/// What the helper answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// Models loaded; on the card, the self-check passed.  `face_model` is
    /// the recognition model loaded, if any, with its vector width.
    Ready { device: Device, name: Option<String>, self_check: SelfCheck, dim: usize, face_model: Option<ModelId>, face_dim: usize },
    /// The card was asked for and can't be used; the helper exits.
    CardFailed { failure: CardFailure },
    /// The models couldn't be loaded (missing, damaged); the helper exits.
    Failed { message: String },
    Images { id: u64, results: Vec<ImageResult> },
    Texts { id: u64, vectors: Vec<Vec<f32>> },
    /// A request that couldn't be served (asked a text-only helper for
    /// images, or a model error), with the request's id.
    Error { id: u64, message: String },
    /// Nothing asked for [`IDLE_MINUTES`]: exiting on purpose.
    Idle,
}

/// The helper's main.  `args` are the arguments after [`FLAG`]:
/// `--device gpu|cpu`, `--text-only`, `--face-model <id>`, `--models
/// <dir>` (the search model's folder; tests) and `--models-root <dir>`
/// (the registry's folder; tests).  Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let device = match arg("--device").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let text_only = args.iter().any(|a| a == "--text-only");
    let face_model = arg("--face-model").and_then(|s| serde_json::from_value::<ModelId>(serde_json::Value::String(s)).ok()).filter(|m| m.is_face_model());
    let root = arg("--models-root").map(PathBuf::from).or_else(models::models_dir);
    let dir = arg("--models").map(PathBuf::from).or_else(|| root.as_ref().map(|r| r.join(ModelId::Siglip2.dir_name())));
    lower_priority();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let (Some(dir), Some(root)) = (dir, root) else {
        send(&mut out, &Reply::Failed { message: "no home folder for the models".into() });
        return 1;
    };
    let fake = if device == Device::Gpu && !text_only { super::fake_card_failure() } else { None };
    if let Some(f) = fake {
        send(&mut out, &Reply::CardFailed { failure: f });
        return 2;
    }
    let loaded = Models::load(&dir, &root, device, text_only, face_model);
    let models = match loaded {
        Ok(m) => m,
        Err(Load::Card(f)) => {
            send(&mut out, &Reply::CardFailed { failure: f });
            return 2;
        }
        Err(Load::Other(why)) => {
            send(&mut out, &Reply::Failed { message: why });
            return 1;
        }
    };
    send(
        &mut out,
        &Reply::Ready {
            device: models.device,
            name: models.name.clone(),
            self_check: models.self_check.clone(),
            dim: models.dim,
            face_model: models.face.as_ref().map(|f| f.id),
            face_dim: models.face.as_ref().map_or(0, |f| f.dim),
        },
    );
    serve(models, std::io::BufReader::new(std::io::stdin()), out, Duration::from_secs(IDLE_MINUTES * 60))
}

fn send(out: &mut impl Write, reply: &Reply) {
    if let Ok(line) = serde_json::to_string(reply) {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// `nice` and the idle I/O class for the whole helper, so indexing never
/// competes with culling.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on this process; failures are harmless.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        // ioprio_set(IOPRIO_WHO_PROCESS, self, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

/// Why the models couldn't be loaded.
pub enum Load {
    Card(CardFailure),
    Other(String),
}

/// A face recognition session.
pub struct FaceModel {
    pub id: ModelId,
    pub dim: usize,
    session: Session,
    input: String,
    norm: Norm,
    /// Crops per call: [`FACE_BATCH`], or what a fixed input shape allows.
    batch: usize,
}

/// The loaded sessions.
pub struct Models {
    pub device: Device,
    pub name: Option<String>,
    pub self_check: SelfCheck,
    pub dim: usize,
    vision: Option<Session>,
    text: Session,
    tokenizer: tokenizers::Tokenizer,
    /// YuNet, with its declared shape made symbolic, when faces are on.
    detector: Option<Session>,
    face: Option<FaceModel>,
}

/// Image `i`'s block of a detector output flattened over the batch: `c`
/// cells of `k` values each.
fn block(v: &[f32], i: usize, c: usize, k: usize) -> &[f32] {
    v.get(i * c * k..(i + 1) * c * k).unwrap_or(&[])
}

/// The recognition model's input convention.
fn norm_of(id: ModelId) -> Norm {
    match id {
        ModelId::SFace => Norm::Raw,
        _ => Norm::Centred,
    }
}

impl Models {
    /// Load the search model's towers from `dir`: the image tower on
    /// `device` (none with `text_only`), the text tower on the processor,
    /// and run the self-check when the card is used.  With `face_model`,
    /// the detector and the recognition model from `root` too, checked
    /// the same way.
    pub fn load(dir: &Path, root: &Path, device: Device, text_only: bool, face_model: Option<ModelId>) -> Result<Models, Load> {
        let info = models::info(ModelId::Siglip2);
        let path = |i: usize| dir.join(info.files[i].file);
        let tokenizer = tokenizers::Tokenizer::from_file(path(2)).map_err(|e| Load::Other(format!("tokenizer: {e}")))?;
        let text = cpu_session(&path(1), cpu_threads()).map_err(|e| Load::Other(format!("text model: {e}")))?;
        let dim = text.outputs().iter().find_map(|o| match o.dtype() {
            ValueType::Tensor { shape, .. } if shape.len() == 2 => Some(shape[1].max(0) as usize),
            _ => None,
        });
        let mut out = Models { device, name: None, self_check: SelfCheck::NotRun, dim: dim.unwrap_or(0), vision: None, text, tokenizer, detector: None, face: None };
        if text_only {
            return Ok(out);
        }
        let vision = match device {
            Device::Cpu => cpu_session(&path(0), cpu_threads()).map_err(|e| Load::Other(format!("image model: {e}")))?,
            Device::Gpu => {
                // Which card, before Dawn is asked for one (ml::gpu).
                let adapter = super::gpu::pin().map_err(Load::Card)?;
                out.name = Some(adapter.name.clone());
                let mut gpu = gpu_session(&path(0)).map_err(|e| Load::Card(card_failure(&e)))?;
                let mut cpu = cpu_session(&path(0), cpu_threads()).map_err(|e| Load::Other(format!("image model: {e}")))?;
                let (cosine, max_diff) = self_check(&mut gpu, &mut cpu).map_err(|e| Load::Card(card_failure(&e)))?;
                eprintln!("ml-worker: self-check cosine {cosine:.6}, largest difference {max_diff:.2e}");
                if !super::self_check_passes(cosine, max_diff) {
                    out.self_check = SelfCheck::Failed { cosine, max_diff };
                    return Err(Load::Card(CardFailure::SelfCheckFailed { cosine, max_diff }));
                }
                out.self_check = SelfCheck::Passed { cosine, max_diff };
                gpu
            }
        };
        if out.dim == 0 {
            out.dim = output_dim(&vision).unwrap_or(0);
        }
        out.vision = Some(vision);
        if let Some(id) = face_model {
            out.load_faces(root, id)?;
        }
        Ok(out)
    }

    /// The detector and the recognition model, on the chosen device,
    /// each checked against the processor on the card.
    fn load_faces(&mut self, root: &Path, id: ModelId) -> Result<(), Load> {
        let det_path = root.join(ModelId::Yunet.dir_name()).join(models::info(ModelId::Yunet).files[0].file);
        let det_bytes = std::fs::read(&det_path).map_err(|e| Load::Other(format!("face detector: {e}")))?;
        let det_bytes = super::onnx_patch::make_dims_symbolic(&det_bytes, &[0, 2, 3]).ok_or_else(|| Load::Other("face detector: not a readable ONNX file".into()))?;
        let rec_path = root.join(id.dir_name()).join(models::info(id).files[0].file);
        let (detector, session) = match self.device {
            Device::Cpu => (
                cpu_session_from(&det_bytes, cpu_threads()).map_err(|e| Load::Other(format!("face detector: {e}")))?,
                cpu_session(&rec_path, cpu_threads()).map_err(|e| Load::Other(format!("face model: {e}")))?,
            ),
            Device::Gpu => {
                let mut det_gpu = gpu_session_from(&det_bytes).map_err(|e| Load::Card(card_failure(&e)))?;
                let mut det_cpu = cpu_session_from(&det_bytes, cpu_threads()).map_err(|e| Load::Other(format!("face detector: {e}")))?;
                let det_input: Vec<f32> = (0..CANVAS_LEN).map(|i| ((i * 31) % 251) as f32).collect();
                let (c, d) = check_session(&mut det_gpu, &mut det_cpu, &[DET_SIZE, DET_SIZE], &det_input).map_err(|e| Load::Card(card_failure(&e)))?;
                eprintln!("ml-worker: face detector self-check cosine {c:.6}, largest difference {d:.2e}");
                if !super::self_check_passes(c, d) {
                    return Err(Load::Card(CardFailure::SelfCheckFailed { cosine: c, max_diff: d }));
                }
                let mut rec_gpu = gpu_session(&rec_path).map_err(|e| Load::Card(card_failure(&e)))?;
                let mut rec_cpu = cpu_session(&rec_path, cpu_threads()).map_err(|e| Load::Other(format!("face model: {e}")))?;
                let n = (3 * ALIGNED * ALIGNED) as usize;
                let rec_input: Vec<f32> = (0..n).map(|i| (((i * 7) % 255) as f32 / 127.5) - 1.0).collect();
                let (c, d) = check_session(&mut rec_gpu, &mut rec_cpu, &[ALIGNED, ALIGNED], &rec_input).map_err(|e| Load::Card(card_failure(&e)))?;
                eprintln!("ml-worker: face model self-check cosine {c:.6}, largest difference {d:.2e}");
                if !super::self_check_passes(c, d) {
                    return Err(Load::Card(CardFailure::SelfCheckFailed { cosine: c, max_diff: d }));
                }
                (det_gpu, rec_gpu)
            }
        };
        let first = session.inputs().first().ok_or_else(|| Load::Other("face model: no input".into()))?;
        let input = first.name().to_string();
        // SFace's file declares a batch of one; the others are free.
        let batch = match first.dtype() {
            ValueType::Tensor { shape, .. } if shape.first().is_some_and(|&n| n > 0) => shape[0] as usize,
            _ => FACE_BATCH,
        };
        let dim = output_dim(&session).unwrap_or(0);
        self.detector = Some(detector);
        self.face = Some(FaceModel { id, dim, session, input, norm: norm_of(id), batch });
        Ok(())
    }

    /// Embed prepared inputs: unit vectors, one per input.
    pub fn embed(&mut self, inputs: &[&[f32]]) -> ort::Result<Vec<Vec<f32>>> {
        let session = self.vision.as_mut().ok_or_else(|| ort::Error::new("this helper runs the text tower only"))?;
        let n = inputs.len();
        let per = (3 * INPUT_SIZE * INPUT_SIZE) as usize;
        let shape = vec![n as i64, 3, INPUT_SIZE as i64, INPUT_SIZE as i64];
        let name = session.inputs()[0].name().to_string();
        let half_input = matches!(session.inputs()[0].dtype(), ValueType::Tensor { ty: TensorElementType::Float16, .. });
        let outputs = if half_input {
            let data: Vec<f16> = inputs.iter().flat_map(|x| x.iter().map(|&v| f16::from_f32(v))).collect();
            debug_assert_eq!(data.len(), n * per);
            session.run(ort::inputs![name.as_str() => Tensor::from_array((shape, data))?])?
        } else {
            let data: Vec<f32> = inputs.iter().flat_map(|x| x.iter().copied()).collect();
            session.run(ort::inputs![name.as_str() => Tensor::from_array((shape, data))?])?
        };
        let (rows, dim) = pooled(&outputs, n)?;
        Ok(rows.chunks_exact(dim).map(normalised).collect())
    }

    /// Encode texts: canonicalised, tokenised with EOS, padded to the
    /// context length, through the text tower; unit vectors.
    pub fn encode(&mut self, texts: &[String]) -> ort::Result<Vec<Vec<f32>>> {
        let n = texts.len();
        let mut ids = Vec::with_capacity(n * CONTEXT);
        for t in texts {
            ids.extend(tokenize(&self.tokenizer, t).map_err(|e| ort::Error::new(format!("tokenizer: {e}")))?);
        }
        let names: Vec<String> = self.text.inputs().iter().map(|i| i.name().to_string()).collect();
        let shape = vec![n as i64, CONTEXT as i64];
        let mut inputs: Vec<(String, ort::session::SessionInputValue<'_>)> = Vec::new();
        for name in &names {
            let v = match name.as_str() {
                "attention_mask" => Tensor::from_array((shape.clone(), vec![1i64; n * CONTEXT]))?,
                "position_ids" => Tensor::from_array((shape.clone(), (0..n).flat_map(|_| 0..CONTEXT as i64).collect::<Vec<i64>>()))?,
                _ => Tensor::from_array((shape.clone(), ids.clone()))?,
            };
            inputs.push((name.clone(), v.into()));
        }
        let outputs = self.text.run(inputs)?;
        let (rows, dim) = pooled(&outputs, n)?;
        Ok(rows.chunks_exact(dim).map(normalised).collect())
    }

    /// Run the detector over canvases (in batches of [`DETECT_BATCH`]):
    /// the faces found on each, best first.
    pub fn detect(&mut self, canvases: &[&face::Canvas]) -> ort::Result<Vec<Vec<face::Detected>>> {
        let session = self.detector.as_mut().ok_or_else(|| ort::Error::new("this helper has no face detector"))?;
        let name = session.inputs()[0].name().to_string();
        let mut out = Vec::with_capacity(canvases.len());
        for batch in canvases.chunks(DETECT_BATCH) {
            let n = batch.len();
            // Always a full batch (black canvases at the end), so the
            // card sees one tensor shape and compiles its kernels once.
            let mut data: Vec<f32> = batch.iter().flat_map(|c| c.planes.iter().map(|&b| b as f32)).collect();
            data.resize(DETECT_BATCH * CANVAS_LEN, 0.0);
            let shape = vec![DETECT_BATCH as i64, 3, DET_SIZE as i64, DET_SIZE as i64];
            let outputs = session.run(ort::inputs![name.as_str() => Tensor::from_array((shape, data))?])?;
            // Each output is flattened over the batch: image i's cells are
            // the i-th block of `cells(stride)`.
            let get = |key: &str| -> ort::Result<Vec<f32>> {
                let v = outputs.get(key).ok_or_else(|| ort::Error::new(format!("the face detector has no output {key}")))?;
                let (_, d) = v.try_extract_tensor::<f32>()?;
                Ok(d.to_vec())
            };
            let mut per_stride = Vec::new();
            for s in face::STRIDES {
                per_stride.push((s, get(&format!("cls_{s}"))?, get(&format!("obj_{s}"))?, get(&format!("bbox_{s}"))?, get(&format!("kps_{s}"))?));
            }
            for i in 0..n {
                let levels: Vec<face::Level<'_>> = per_stride
                    .iter()
                    .map(|(s, cls, obj, bbox, kps)| {
                        let c = face::cells(*s);
                        face::Level { stride: *s, cls: block(cls, i, c, 1), obj: block(obj, i, c, 1), bbox: block(bbox, i, c, 4), kps: block(kps, i, c, 10) }
                    })
                    .collect();
                out.push(face::decode_levels(&levels));
            }
        }
        Ok(out)
    }

    /// Recognise aligned crops (in batches of [`FACE_BATCH`]): unit
    /// vectors, one per crop.
    pub fn recognise(&mut self, crops: &[&Pixels]) -> ort::Result<Vec<Vec<f32>>> {
        let fm = self.face.as_mut().ok_or_else(|| ort::Error::new("this helper has no face model"))?;
        let mut out = Vec::with_capacity(crops.len());
        let size = fm.batch.max(1);
        let per = (3 * ALIGNED * ALIGNED) as usize;
        for batch in crops.chunks(size) {
            let n = batch.len();
            // A full batch every time (blank crops at the end): one shape
            // for the card.
            let mut data: Vec<f32> = batch.iter().flat_map(|c| face::face_input(c, fm.norm)).collect();
            data.resize(size * per, 0.0);
            let shape = vec![size as i64, 3, ALIGNED as i64, ALIGNED as i64];
            let outputs = fm.session.run(ort::inputs![fm.input.as_str() => Tensor::from_array((shape, data))?])?;
            let (rows, dim) = pooled(&outputs, size)?;
            out.extend(rows.chunks_exact(dim).take(n).map(normalised));
        }
        Ok(out)
    }

    fn wants_faces(&self) -> bool {
        self.detector.is_some() && self.face.is_some()
    }
}

/// open_clip's `canonicalize` cleaning, which the model's text side was
/// evaluated with: underscores to spaces, ASCII punctuation removed,
/// lower case, whitespace collapsed.
pub fn canonicalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        let c = if c == '_' { ' ' } else { c };
        if c.is_ascii_punctuation() {
            continue;
        }
        if c.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
            continue;
        }
        for l in c.to_lowercase() {
            out.push(l);
        }
        space = false;
    }
    out.trim_end().to_string()
}

/// Token ids for one text: canonicalised, encoded (the tokenizer appends
/// EOS), truncated with EOS kept last, padded to [`CONTEXT`].
pub fn tokenize(tok: &tokenizers::Tokenizer, text: &str) -> Result<Vec<i64>, String> {
    let enc = tok.encode(canonicalize(text), true).map_err(|e| e.to_string())?;
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
    if ids.last() != Some(&EOS) {
        ids.push(EOS);
    }
    if ids.len() > CONTEXT {
        ids.truncate(CONTEXT - 1);
        ids.push(EOS);
    }
    ids.resize(CONTEXT, PAD);
    Ok(ids)
}

fn cpu_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8)
}

fn cpu_session(path: &Path, threads: usize) -> ort::Result<Session> {
    Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?.with_intra_threads(threads)?.commit_from_file(path)
}

fn cpu_session_from(bytes: &[u8], threads: usize) -> ort::Result<Session> {
    Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?.with_intra_threads(threads)?.commit_from_memory(bytes)
}

fn gpu_builder() -> ort::Result<ort::session::builder::SessionBuilder> {
    use ort::ep::ArbitrarilyConfigurableExecutionProvider;
    // The device is pinned by ml::gpu; the power preference is a second
    // steer for drivers without Mesa's device-select layer.
    let ep = ort::ep::WebGPU::default().with_arbitrary_config("ep.webgpuexecutionprovider.powerPreference", "high-performance");
    Ok(Session::builder()?.with_execution_providers([ep.build().error_on_failure()])?.with_optimization_level(GraphOptimizationLevel::Level3)?)
}

fn gpu_session(path: &Path) -> ort::Result<Session> {
    gpu_builder()?.commit_from_file(path)
}

fn gpu_session_from(bytes: &[u8]) -> ort::Result<Session> {
    gpu_builder()?.commit_from_memory(bytes)
}

/// An error creating or running the card's session, as a card failure.
fn card_failure(e: &ort::Error) -> CardFailure {
    let text = e.to_string();
    eprintln!("ml-worker: graphics card: {text}");
    let lower = text.to_lowercase();
    if lower.contains("adapter") || lower.contains("no device") || lower.contains("vulkan") || lower.contains("webgpu") {
        CardFailure::NoCard
    } else {
        CardFailure::Failed(text)
    }
}

/// Fixed inputs through both sessions: (smallest cosine between the two
/// answers, largest difference in any component).
fn self_check(gpu: &mut Session, cpu: &mut Session) -> ort::Result<(f32, f32)> {
    let per = (3 * INPUT_SIZE * INPUT_SIZE) as usize;
    let n = 4;
    let inputs: Vec<Vec<f32>> = (0..n).map(|k| (0..per).map(|i| (((i * 7 + k * 13) % 255) as f32 / 255.0 - 0.5) * 2.0).collect()).collect();
    let refs: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
    let a = run_raw(gpu, &refs)?;
    let b = run_raw(cpu, &refs)?;
    Ok(agreement(&a, &b))
}

/// One fixed input (`height × width`, three planes) through two sessions
/// of the same model, every output compared: (cosine over all the
/// outputs, largest difference).  For the face sessions on the card.
pub fn check_session(a: &mut Session, b: &mut Session, hw: &[u32; 2], input: &[f32]) -> ort::Result<(f32, f32)> {
    let run = |s: &mut Session| -> ort::Result<Vec<f32>> {
        let name = s.inputs()[0].name().to_string();
        let shape = vec![1i64, 3, hw[0] as i64, hw[1] as i64];
        let outputs = s.run(ort::inputs![name.as_str() => Tensor::from_array((shape, input.to_vec()))?])?;
        let mut all = Vec::new();
        for (_, v) in outputs.iter() {
            if let Ok((_, d)) = v.try_extract_tensor::<f32>() {
                all.extend_from_slice(d);
            }
        }
        Ok(all)
    };
    let x = run(a)?;
    let y = run(b)?;
    Ok(agreement(&[x], &[y]))
}

/// The smallest cosine and the largest component difference between
/// paired answers, each normalised.
fn agreement(a: &[Vec<f32>], b: &[Vec<f32>]) -> (f32, f32) {
    let mut min_cos = 1.0f32;
    let mut max_diff = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (normalised(x), normalised(y));
        let cos: f32 = x.iter().zip(&y).map(|(p, q)| p * q).sum();
        min_cos = min_cos.min(cos);
        for (p, q) in x.iter().zip(&y) {
            max_diff = max_diff.max((p - q).abs());
        }
    }
    (min_cos, max_diff)
}

fn run_raw(session: &mut Session, inputs: &[&[f32]]) -> ort::Result<Vec<Vec<f32>>> {
    let n = inputs.len();
    let shape = vec![n as i64, 3, INPUT_SIZE as i64, INPUT_SIZE as i64];
    let name = session.inputs()[0].name().to_string();
    let half_input = matches!(session.inputs()[0].dtype(), ValueType::Tensor { ty: TensorElementType::Float16, .. });
    let outputs = if half_input {
        let data: Vec<f16> = inputs.iter().flat_map(|x| x.iter().map(|&v| f16::from_f32(v))).collect();
        session.run(ort::inputs![name.as_str() => Tensor::from_array((shape, data))?])?
    } else {
        let data: Vec<f32> = inputs.iter().flat_map(|x| x.iter().copied()).collect();
        session.run(ort::inputs![name.as_str() => Tensor::from_array((shape, data))?])?
    };
    let (rows, dim) = pooled(&outputs, n)?;
    Ok(rows.chunks_exact(dim).map(<[f32]>::to_vec).collect())
}

/// The pooled embedding among a session's outputs (`pooler_output`, else
/// the first 2-D output), as f32 with its width.
fn pooled(outputs: &ort::session::SessionOutputs<'_>, n: usize) -> ort::Result<(Vec<f32>, usize)> {
    let name = if outputs.get("pooler_output").is_some() {
        Some("pooler_output".to_string())
    } else {
        outputs.iter().find_map(|(k, v)| match v.dtype() {
            ValueType::Tensor { shape, .. } if shape.len() == 2 => Some(k.to_string()),
            _ => None,
        })
    };
    let v = name.and_then(|n| outputs.get(n)).ok_or_else(|| ort::Error::new("the model has no pooled output"))?;
    let (shape, data): (Vec<i64>, Vec<f32>) = match v.dtype() {
        ValueType::Tensor { ty: TensorElementType::Float16, .. } => {
            let (s, d) = v.try_extract_tensor::<f16>()?;
            (s.to_vec(), d.iter().map(|h| h.to_f32()).collect())
        }
        _ => {
            let (s, d) = v.try_extract_tensor::<f32>()?;
            (s.to_vec(), d.to_vec())
        }
    };
    let dim = *shape.last().unwrap_or(&0) as usize;
    if dim == 0 || data.len() != n * dim {
        return Err(ort::Error::new(format!("unexpected output shape {shape:?} for {n} inputs")));
    }
    Ok((data, dim))
}

fn output_dim(session: &Session) -> Option<usize> {
    session.outputs().iter().find_map(|o| match o.dtype() {
        ValueType::Tensor { shape, .. } if shape.len() == 2 => Some(shape[1].max(0) as usize),
        _ => None,
    })
}

fn normalised(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 && n.is_finite() { v.iter().map(|x| x / n).collect() } else { v.to_vec() }
}

/// A face's tile for the page: a square around the box (60 % wider than
/// its longer side), `TILE_PX` wide, as JPEG bytes.
pub fn tile(full: &Pixels, bbox: &[f32; 4]) -> Vec<u8> {
    let (w, h) = (full.width as f32, full.height as f32);
    let side = (bbox[2] * w).max(bbox[3] * h) * 1.6;
    let side = side.min(w).min(h).max(2.0);
    let (cx, cy) = ((bbox[0] + bbox[2] / 2.0) * w, (bbox[1] + bbox[3] / 2.0) * h);
    let x = (cx - side / 2.0).clamp(0.0, w - side);
    let y = (cy - side / 2.0).clamp(0.0, h - side);
    let cut = decode::crop(full, x as u32, y as u32, (side as u32).max(1), (side as u32).max(1));
    let small = decode::resize_to(&cut, TILE_PX, TILE_PX);
    let rgb = decode::to_rgb(&small);
    let mut out = Vec::with_capacity(6 * 1024);
    if jpeg_encoder::Encoder::new(&mut out, TILE_QUALITY).encode(&rgb, TILE_PX as u16, TILE_PX as u16, jpeg_encoder::ColorType::Rgb).is_err() {
        out.clear();
    }
    out
}

/// A face cut from the full preview and measured, before recognition.
struct Cut {
    /// Index of its photo in the chunk.
    photo: usize,
    placed: face::Placed,
    crop: Pixels,
    sharpness: f32,
    tile: Vec<u8>,
}

/// Cut, align and tile the faces placed on each photo of a chunk
/// (`faces[i]` for photo `i`), decoding each full preview once on the
/// reader pool.  Photos whose preview can't be decoded again lose their
/// faces.
fn cut_faces(pool: &rayon::ThreadPool, preps: &[&FacePrep], placed: &[Vec<face::Placed>]) -> Vec<Cut> {
    use rayon::prelude::*;
    let mut cuts: Vec<Vec<Cut>> = pool.install(|| {
        preps
            .par_iter()
            .zip(placed.par_iter())
            .enumerate()
            .map(|(i, (prep, faces))| {
                if faces.is_empty() {
                    return Vec::new();
                }
                let Some(full) = prep.full_pixels() else { return Vec::new() };
                let (w, h) = (full.width as f32, full.height as f32);
                faces
                    .iter()
                    .map(|p| {
                        let lm = p.landmarks.map(|[x, y]| [x * w, y * h]);
                        let crop = face::align(&full, &lm);
                        let sharpness = face::sharpness(&crop);
                        Cut { photo: i, placed: *p, crop, sharpness, tile: tile(&full, &p.bbox) }
                    })
                    .collect()
            })
            .collect()
    });
    cuts.drain(..).flatten().collect()
}

/// The request loop: previews are read and prepared on [`READERS`]
/// threads ahead of inference; text requests are answered between image
/// chunks; nothing for `idle` exits.
pub fn serve(mut models: Models, input: impl BufRead + Send + 'static, mut out: impl Write, idle: Duration) -> i32 {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("ml-worker-stdin".into())
        .spawn(move || {
            for line in input.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Request>(&line) {
                    Ok(req) => {
                        if tx.send(req).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("ml-worker: bad request: {e}"),
                }
            }
        })
        .expect("spawn stdin reader");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(READERS).thread_name(|i| format!("ml-reader-{i}")).build().expect("reader pool");
    let chunk = match models.device {
        Device::Gpu => CHUNK_GPU,
        Device::Cpu => CHUNK_CPU,
    };
    let mut queue: VecDeque<Request> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv_timeout(idle) {
                Ok(r) => queue.push_back(r),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    send(&mut out, &Reply::Idle);
                    return 0;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
            }
        }
        while let Ok(r) = rx.try_recv() {
            queue.push_back(r);
        }
        // Text first: a query shouldn't wait behind a batch of photos.
        let next = queue.iter().position(|r| matches!(r, Request::EncodeText { .. })).unwrap_or(0);
        let Some(req) = queue.remove(next) else { continue };
        match req {
            Request::Quit => return 0,
            Request::EncodeText { id, texts } => {
                let started = Instant::now();
                match models.encode(&texts) {
                    Ok(vectors) => send(&mut out, &Reply::Texts { id, vectors }),
                    Err(e) => send(&mut out, &Reply::Error { id, message: e.to_string() }),
                }
                if std::env::var_os("PP_PROFILE").is_some() {
                    eprintln!("ml-worker: {} texts in {:.1} ms", texts.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Request::EmbedImages { id, files, want } => {
                if models.vision.is_none() {
                    send(&mut out, &Reply::Error { id, message: "this helper runs the text tower only".into() });
                    continue;
                }
                let faces_asked = want.faces != FaceAsk::None;
                if faces_asked && !models.wants_faces() {
                    send(&mut out, &Reply::Error { id, message: "this helper has no face model loaded".into() });
                    continue;
                }
                let started = Instant::now();
                let mut results: Vec<Option<ImageResult>> = vec![None; files.len()];
                let (ptx, prx) = mpsc::sync_channel::<(usize, Result<Prepared, FileError>)>(chunk);
                let paths: Vec<(PathBuf, FileType)> = files.clone();
                pool.spawn(move || {
                    use rayon::prelude::*;
                    paths.par_iter().enumerate().for_each(|(i, (p, t))| {
                        let _ = ptx.send((i, prep::prepare(p, *t, faces_asked)));
                    });
                });
                let mut ready: Vec<(usize, Prepared)> = Vec::with_capacity(chunk);
                let mut faces_found = 0usize;
                let flush = |ready: &mut Vec<(usize, Prepared)>, results: &mut Vec<Option<ImageResult>>, models: &mut Models, faces_found: &mut usize| {
                    if ready.is_empty() {
                        return;
                    }
                    let vectors = if want.search {
                        let inputs: Vec<&[f32]> = ready.iter().map(|(_, p)| p.input.as_slice()).collect();
                        match models.embed(&inputs) {
                            Ok(v) => v,
                            Err(e) => {
                                for (i, _) in ready.drain(..) {
                                    results[i] = Some(ImageResult { path: files[i].0.clone(), result: Err(FileError::other(format!("model: {e}"))) });
                                }
                                return;
                            }
                        }
                    } else {
                        vec![Vec::new(); ready.len()]
                    };
                    let faces: Vec<Vec<FaceFound>> = match &want.faces {
                        FaceAsk::None => vec![Vec::new(); ready.len()],
                        ask => match faces_in(models, &pool, ready, ask, &files) {
                            Ok(f) => f,
                            Err(e) => {
                                for (i, _) in ready.drain(..) {
                                    results[i] = Some(ImageResult { path: files[i].0.clone(), result: Err(FileError::other(format!("face model: {e}"))) });
                                }
                                return;
                            }
                        },
                    };
                    for (((i, p), v), f) in ready.drain(..).zip(vectors).zip(faces) {
                        *faces_found += f.len();
                        let e = Embedding { key: p.key, capture: p.capture, meta: p.meta, accidental: p.accidental, vector: v, faces: f };
                        results[i] = Some(ImageResult { path: files[i].0.clone(), result: Ok(e) });
                    }
                };
                for (i, r) in prx {
                    match r {
                        Ok(p) => ready.push((i, p)),
                        Err(e) => results[i] = Some(ImageResult { path: files[i].0.clone(), result: Err(e) }),
                    }
                    if ready.len() >= chunk {
                        flush(&mut ready, &mut results, &mut models, &mut faces_found);
                        // A query waiting?  Answer it between chunks.
                        while let Ok(r) = rx.try_recv() {
                            queue.push_back(r);
                        }
                        let texts: Vec<Request> = {
                            let mut t = Vec::new();
                            let mut rest = VecDeque::new();
                            for r in queue.drain(..) {
                                if matches!(r, Request::EncodeText { .. }) { t.push(r) } else { rest.push_back(r) }
                            }
                            queue = rest;
                            t
                        };
                        for t in texts {
                            if let Request::EncodeText { id, texts } = t {
                                match models.encode(&texts) {
                                    Ok(vectors) => send(&mut out, &Reply::Texts { id, vectors }),
                                    Err(e) => send(&mut out, &Reply::Error { id, message: e.to_string() }),
                                }
                            }
                        }
                    }
                }
                flush(&mut ready, &mut results, &mut models, &mut faces_found);
                let results: Vec<ImageResult> = results
                    .into_iter()
                    .zip(&files)
                    .map(|(r, (p, _))| r.unwrap_or_else(|| ImageResult { path: p.clone(), result: Err(FileError::other("not prepared")) }))
                    .collect();
                if std::env::var_os("PP_PROFILE").is_some() {
                    eprintln!("ml-worker: {} photos ({faces_found} faces) in {:.0} ms", results.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
                send(&mut out, &Reply::Images { id, results });
            }
        }
    }
}

/// The faces of a chunk: detected on the canvases (or taken as known
/// landmarks for a model change), cut from the full previews, recognised
/// and tiled.  One list per photo of `ready`, in its order.
fn faces_in(models: &mut Models, pool: &rayon::ThreadPool, ready: &[(usize, Prepared)], ask: &FaceAsk, files: &[(PathBuf, FileType)]) -> ort::Result<Vec<Vec<FaceFound>>> {
    let n = ready.len();
    let preps: Vec<&FacePrep> = ready.iter().map(|(_, p)| p.faces.as_ref()).collect::<Option<Vec<_>>>().unwrap_or_default();
    if preps.len() != n {
        return Err(ort::Error::new("a photo was prepared without its face canvas"));
    }
    let placed: Vec<Vec<face::Placed>> = match ask {
        FaceAsk::Detect => {
            let canvases: Vec<&face::Canvas> = preps.iter().map(|p| &p.canvas).collect();
            let detected = models.detect(&canvases)?;
            detected.iter().zip(&preps).map(|(d, p)| face::place(d, p.canvas.scale, p.width, p.height)).collect()
        }
        FaceAsk::Known { files: known } => ready
            .iter()
            .zip(&preps)
            .map(|((i, _), p)| {
                let path = &files[*i].0;
                known
                    .iter()
                    .find(|k| &k.path == path)
                    .map(|k| {
                        k.faces
                            .iter()
                            .map(|f| face::Placed { bbox: f.bbox, landmarks: f.landmarks, score: f.score, size_px: (f.bbox[2] * p.width as f32).max(f.bbox[3] * p.height as f32) })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect(),
        FaceAsk::None => vec![Vec::new(); n],
    };
    let cuts = cut_faces(pool, &preps, &placed);
    let crops: Vec<&Pixels> = cuts.iter().map(|c| &c.crop).collect();
    let vectors = if crops.is_empty() { Vec::new() } else { models.recognise(&crops)? };
    let mut out: Vec<Vec<FaceFound>> = vec![Vec::new(); n];
    for (cut, vector) in cuts.into_iter().zip(vectors) {
        out[cut.photo].push(FaceFound { bbox: cut.placed.bbox, landmarks: cut.placed.landmarks, score: cut.placed.score, size_px: cut.placed.size_px, sharpness: cut.sharpness, vector, tile: cut.tile });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_round_trip_as_one_line_each() {
        let req = Request::EmbedImages { id: 7, files: vec![(PathBuf::from("/x/a.JPG"), FileType::Jpg)], want: Want { search: true, faces: FaceAsk::Detect } };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
        let rep = Reply::Ready { device: Device::Gpu, name: Some("Test GPU".into()), self_check: SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 }, dim: 768, face_model: Some(ModelId::AuraFace), face_dim: 512 };
        let line = serde_json::to_string(&rep).unwrap();
        assert!(line.starts_with(r#"{"reply":"ready""#));
        assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), rep);
        // Every reply the helper can give serialises (internally tagged
        // enums refuse newtype variants, which once silenced the failures).
        for rep in [
            Reply::CardFailed { failure: CardFailure::NoCard },
            Reply::CardFailed { failure: CardFailure::SelfCheckFailed { cosine: 0.5, max_diff: 0.2 } },
            Reply::CardFailed { failure: CardFailure::Failed("x".into()) },
            Reply::Failed { message: "no models".into() },
            Reply::Error { id: 3, message: "m".into() },
            Reply::Idle,
            Reply::Texts { id: 1, vectors: vec![vec![0.5, 0.5]] },
        ] {
            let line = serde_json::to_string(&rep).unwrap_or_else(|e| panic!("{rep:?}: {e}"));
            assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), rep);
        }
        // A known-faces request names the files and their landmarks.
        let known = FaceAsk::Known { files: vec![super::super::client::KnownFaces { path: PathBuf::from("/x/a.JPG"), faces: vec![super::super::client::FaceSpot { bbox: [0.1, 0.1, 0.2, 0.2], landmarks: [[0.15, 0.15]; 5], score: 0.95 }] }] };
        let req = Request::EmbedImages { id: 8, files: vec![(PathBuf::from("/x/a.JPG"), FileType::Jpg)], want: Want { search: false, faces: known } };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&req).unwrap()).unwrap(), req);
    }

    #[test]
    fn canonicalize_matches_open_clip() {
        assert_eq!(canonicalize("A photo of a Red_Car, at night!"), "a photo of a red car at night");
        assert_eq!(canonicalize("  Fireworks   -crowd  "), "fireworks crowd");
        assert_eq!(canonicalize("Café"), "café");
        assert_eq!(canonicalize(""), "");
    }

    #[test]
    fn a_tile_is_a_small_jpeg_around_the_box() {
        let px = Pixels::new(800, 600, 3, vec![120; 800 * 600 * 3]);
        let t = tile(&px, &[0.4, 0.3, 0.1, 0.15]);
        assert!(t.len() > 200 && t.starts_with(&[0xff, 0xd8]), "{} bytes", t.len());
        assert_eq!(decode::jpeg_dimensions(&t), Some((TILE_PX, TILE_PX)));
        // A box at the edge still gives a full tile.
        assert_eq!(decode::jpeg_dimensions(&tile(&px, &[0.95, 0.9, 0.1, 0.15])), Some((TILE_PX, TILE_PX)));
        assert_eq!(norm_of(ModelId::SFace), Norm::Raw);
        assert_eq!(norm_of(ModelId::AuraFace), Norm::Centred);
        assert_eq!(norm_of(ModelId::InsightFace), Norm::Centred);
    }

    /// Needs the tokenizer file: `PP_MODEL_DIR=<dir with tokenizer.json>`.
    #[test]
    #[ignore = "needs PP_MODEL_DIR with the SigLIP2 tokenizer.json"]
    fn the_tokenizer_pads_to_the_context_with_eos_last() {
        let dir = std::env::var("PP_MODEL_DIR").expect("PP_MODEL_DIR must point at the model folder");
        let tok = tokenizers::Tokenizer::from_file(Path::new(&dir).join("tokenizer.json")).expect("tokenizer.json loads");
        let ids = tokenize(&tok, "a photo of a camera.").unwrap();
        assert_eq!(ids.len(), CONTEXT);
        let n = ids.iter().rposition(|&i| i != PAD).unwrap();
        assert_eq!(ids[n], EOS, "EOS ends the text");
        assert!(n >= 4 && n < 12, "{ids:?}");
        assert!(ids[..n].iter().all(|&i| i > 1));
        let long = tokenize(&tok, &"word ".repeat(100)).unwrap();
        assert_eq!(long.len(), CONTEXT);
        assert_eq!(long[CONTEXT - 1], EOS, "truncation keeps EOS last");
    }
}
