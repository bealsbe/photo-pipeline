//! The helper process: `photo-pipeline --ml-worker --device gpu|cpu
//! [--text-only]` (plan §4.3).  The same binary, started by
//! [`super::client::Client`], so there is nothing extra to install.  It
//! loads the ONNX Runtime sessions on the chosen device, runs the
//! self-check on the card, reads previews itself (the app sends paths
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

use super::client::{Embedding, FileError, ImageResult};
use super::models::{self, ModelId};
use super::prep::{self, INPUT_SIZE, Prepared};
use super::{CardFailure, Device, SelfCheck};
use crate::model::record::FileType;

/// The flag that makes the binary a helper.
pub const FLAG: &str = "--ml-worker";
/// Minutes without a request after which the helper exits.
pub const IDLE_MINUTES: u64 = 3;
/// Photos per inference call on each device: small on the processor so a
/// text query waits at most a fraction of a second between chunks.
pub const CHUNK_GPU: usize = 32;
pub const CHUNK_CPU: usize = 8;
/// Threads reading and decoding previews (the share is bandwidth-bound;
/// `analysis` uses 4 for the same reason).
pub const READERS: usize = 6;
/// Tokens per query (SigLIP2's context length).
pub const CONTEXT: usize = 64;
const PAD: i64 = 0;
const EOS: i64 = 1;

/// What the app asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    /// Embed these photos; the reply holds one result per path, in order.
    EmbedImages { id: u64, files: Vec<(PathBuf, FileType)> },
    /// Encode query texts on the processor.
    EncodeText { id: u64, texts: Vec<String> },
    /// Exit now.
    Quit,
}

/// What the helper answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// Models loaded; on the card, the self-check passed.
    Ready { device: Device, name: Option<String>, self_check: SelfCheck, dim: usize },
    /// The card was asked for and can't be used; the helper exits.
    CardFailed(CardFailure),
    /// The models couldn't be loaded (missing, damaged); the helper exits.
    Failed(String),
    Images { id: u64, results: Vec<ImageResult> },
    Texts { id: u64, vectors: Vec<Vec<f32>> },
    /// A request that couldn't be served (asked a text-only helper for
    /// images, or a model error), with the request's id.
    Error { id: u64, message: String },
    /// Nothing asked for [`IDLE_MINUTES`]: exiting on purpose.
    Idle,
}

/// The helper's main.  `args` are the arguments after [`FLAG`]:
/// `--device gpu|cpu`, `--text-only`, `--models <dir>` (tests; the
/// registry's folder otherwise).  Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let device = match arg("--device").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let text_only = args.iter().any(|a| a == "--text-only");
    let dir = arg("--models").map(PathBuf::from).or_else(|| models::model_dir(ModelId::Siglip2));
    lower_priority();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let Some(dir) = dir else {
        send(&mut out, &Reply::Failed("no home folder for the models".into()));
        return 1;
    };
    let fake = if device == Device::Gpu && !text_only { super::fake_card_failure() } else { None };
    if let Some(f) = fake {
        send(&mut out, &Reply::CardFailed(f));
        return 2;
    }
    let loaded = Models::load(&dir, device, text_only);
    let models = match loaded {
        Ok(m) => m,
        Err(Load::Card(f)) => {
            send(&mut out, &Reply::CardFailed(f));
            return 2;
        }
        Err(Load::Other(why)) => {
            send(&mut out, &Reply::Failed(why));
            return 1;
        }
    };
    send(&mut out, &Reply::Ready { device: models.device, name: models.name.clone(), self_check: models.self_check.clone(), dim: models.dim });
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

/// The loaded sessions.
pub struct Models {
    pub device: Device,
    pub name: Option<String>,
    pub self_check: SelfCheck,
    pub dim: usize,
    vision: Option<Session>,
    text: Session,
    tokenizer: tokenizers::Tokenizer,
}

impl Models {
    /// Load the search model's towers from `dir`: the image tower on
    /// `device` (none with `text_only`), the text tower on the processor,
    /// and run the self-check when the card is used.
    pub fn load(dir: &Path, device: Device, text_only: bool) -> Result<Models, Load> {
        let info = models::info(ModelId::Siglip2);
        let path = |i: usize| dir.join(info.files[i].file);
        let tokenizer = tokenizers::Tokenizer::from_file(path(2)).map_err(|e| Load::Other(format!("tokenizer: {e}")))?;
        let text = cpu_session(&path(1), cpu_threads()).map_err(|e| Load::Other(format!("text model: {e}")))?;
        let dim = text.outputs().iter().find_map(|o| match o.dtype() {
            ValueType::Tensor { shape, .. } if shape.len() == 2 => Some(shape[1].max(0) as usize),
            _ => None,
        });
        let mut out = Models { device, name: None, self_check: SelfCheck::NotRun, dim: dim.unwrap_or(0), vision: None, text, tokenizer };
        if text_only {
            return Ok(out);
        }
        let vision = match device {
            Device::Cpu => cpu_session(&path(0), cpu_threads()).map_err(|e| Load::Other(format!("image model: {e}")))?,
            Device::Gpu => {
                let mut gpu = gpu_session(&path(0)).map_err(|e| Load::Card(card_failure(&e)))?;
                out.name = adapter_name();
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
        Ok(out)
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

fn gpu_session(path: &Path) -> ort::Result<Session> {
    Session::builder()?
        .with_execution_providers([ort::ep::WebGPU::default().build().error_on_failure()])?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_file(path)
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

/// The adapter's name, best effort: the first `deviceName` vulkaninfo
/// reports, within a short timeout; none without the tool.
fn adapter_name() -> Option<String> {
    let out = crate::ops::netfs::with_timeout(Duration::from_secs(3), || {
        std::process::Command::new("vulkaninfo").arg("--summary").stderr(std::process::Stdio::null()).output().ok()
    })??;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("deviceName").map(|r| r.trim_start_matches(|c: char| c == ' ' || c == '=').trim().to_string())
    })
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
    let mut min_cos = 1.0f32;
    let mut max_diff = 0.0f32;
    for (x, y) in a.iter().zip(&b) {
        let (x, y) = (normalised(x), normalised(y));
        let cos: f32 = x.iter().zip(&y).map(|(p, q)| p * q).sum();
        min_cos = min_cos.min(cos);
        for (p, q) in x.iter().zip(&y) {
            max_diff = max_diff.max((p - q).abs());
        }
    }
    Ok((min_cos, max_diff))
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
            Request::EmbedImages { id, files } => {
                if models.vision.is_none() {
                    send(&mut out, &Reply::Error { id, message: "this helper runs the text tower only".into() });
                    continue;
                }
                let started = Instant::now();
                let mut results: Vec<Option<ImageResult>> = vec![None; files.len()];
                let (ptx, prx) = mpsc::sync_channel::<(usize, Result<Prepared, FileError>)>(chunk * 2);
                let paths: Vec<(PathBuf, FileType)> = files.clone();
                pool.spawn(move || {
                    use rayon::prelude::*;
                    paths.par_iter().enumerate().for_each(|(i, (p, t))| {
                        let _ = ptx.send((i, prep::prepare(p, *t)));
                    });
                });
                let mut ready: Vec<(usize, Prepared)> = Vec::with_capacity(chunk);
                let flush = |ready: &mut Vec<(usize, Prepared)>, results: &mut Vec<Option<ImageResult>>, models: &mut Models| {
                    if ready.is_empty() {
                        return;
                    }
                    let inputs: Vec<&[f32]> = ready.iter().map(|(_, p)| p.input.as_slice()).collect();
                    match models.embed(&inputs) {
                        Ok(vectors) => {
                            for ((i, p), v) in ready.drain(..).zip(vectors) {
                                let e = Embedding { key: p.key, capture: p.capture, meta: p.meta, accidental: p.accidental, vector: v };
                                results[i] = Some(ImageResult { path: files[i].0.clone(), result: Ok(e) });
                            }
                        }
                        Err(e) => {
                            for (i, _) in ready.drain(..) {
                                results[i] = Some(ImageResult { path: files[i].0.clone(), result: Err(FileError::other(format!("model: {e}"))) });
                            }
                        }
                    }
                };
                for (i, r) in prx {
                    match r {
                        Ok(p) => ready.push((i, p)),
                        Err(e) => results[i] = Some(ImageResult { path: files[i].0.clone(), result: Err(e) }),
                    }
                    if ready.len() >= chunk {
                        flush(&mut ready, &mut results, &mut models);
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
                flush(&mut ready, &mut results, &mut models);
                let results: Vec<ImageResult> = results
                    .into_iter()
                    .zip(&files)
                    .map(|(r, (p, _))| r.unwrap_or_else(|| ImageResult { path: p.clone(), result: Err(FileError::other("not prepared")) }))
                    .collect();
                if std::env::var_os("PP_PROFILE").is_some() {
                    eprintln!("ml-worker: {} photos in {:.0} ms", results.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
                send(&mut out, &Reply::Images { id, results });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_round_trip_as_one_line_each() {
        let req = Request::EmbedImages { id: 7, files: vec![(PathBuf::from("/x/a.JPG"), FileType::Jpg)] };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
        let rep = Reply::Ready { device: Device::Gpu, name: Some("Test GPU".into()), self_check: SelfCheck::Passed { cosine: 1.0, max_diff: 0.0 }, dim: 768 };
        let line = serde_json::to_string(&rep).unwrap();
        assert!(line.starts_with(r#"{"reply":"ready""#));
        assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), rep);
    }

    #[test]
    fn canonicalize_matches_open_clip() {
        assert_eq!(canonicalize("A photo of a Red_Car, at night!"), "a photo of a red car at night");
        assert_eq!(canonicalize("  Fireworks   -crowd  "), "fireworks crowd");
        assert_eq!(canonicalize("Café"), "café");
        assert_eq!(canonicalize(""), "");
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
