//! The close-call judge's helper process: `photo-pipeline --judge-worker
//! --model-dir <dir>` (fursuit matching v2).  The same binary, started by
//! [`super::judge_client`], a process of its own beside the search helper
//! (`--ml-worker`): a driver fault takes it down, never the app, and when
//! it exits after [`IDLE_SECS`] without a question the card's memory goes
//! back with it.
//!
//! It runs Gemma 4 through llama.cpp and libmtmd, vendored at a pinned
//! commit and built with Vulkan (`build.rs`, `native/judge_shim.cpp`).
//! **Card only:** the helper picks the card as the search helper does
//! (`ml::gpu`: discrete, else integrated, never a software one), hands
//! llama.cpp that device alone, asks for every layer on it and refuses
//! anything less ([`check_offload`]); the vision projector runs on the
//! same device.  No card, too little memory, or a failed load is a
//! [`Reply::CardFailed`], and the helper exits: nothing is ever run on
//! the processor instead.
//!
//! **Protocol:** one JSON object per line on stdin and stdout, as the
//! search helper's.  The first reply is [`Reply::Ready`],
//! [`Reply::CardFailed`] or [`Reply::Failed`].
//!
//! **Scoring** is the research's: each pair is asked both ways round (A
//! then B, B then A), each a fresh prompt; P(yes) comes from the first
//! answer token's log-probabilities ([`p_yes`]), and the verdict is the
//! mean of the two.

use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_int};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::client::FileError;
use super::judge::{self, Crop, IDLE_SECS, PairAsk, PairResult, Reply, Request, Verdict, check_offload, p_yes};
use super::models::{self, ModelId};
use super::CardFailure;

/// The flag that makes the binary the judge helper.
pub const FLAG: &str = "--judge-worker";
/// Crops kept between questions (a subject's member is asked against
/// many candidates).
const CROP_CACHE: usize = 64;

#[repr(C)]
struct RawJudge {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn pp_judge_init(echo: c_int);
    fn pp_judge_devices(out: *mut c_char, len: usize) -> c_int;
    fn pp_judge_load(
        model: *const c_char,
        mmproj: *const c_char,
        device: *const c_char,
        n_ctx: c_int,
        n_batch: c_int,
        n_ubatch: c_int,
        image_tokens: c_int,
        threads: c_int,
        offloaded: *mut c_int,
        layers: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> *mut RawJudge;
    fn pp_judge_marker() -> *const c_char;
    fn pp_judge_ask(
        j: *mut RawJudge,
        prompt: *const c_char,
        a: *const u8,
        a_len: usize,
        b: *const u8,
        b_len: usize,
        top: c_int,
        ids: *mut c_int,
        logprobs: *mut f32,
        pieces: *mut c_char,
        pieces_len: usize,
        n_prompt: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn pp_judge_free(j: *mut RawJudge);
    fn pp_judge_is_software(description: *const c_char) -> c_int;
}

fn c_text(buf: &[u8]) -> String {
    CStr::from_bytes_until_nul(buf).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A graphics device as llama.cpp lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// llama.cpp's name ("Vulkan0").
    pub name: String,
    /// The driver's description ("AMD Radeon RX 7900 XTX (RADV NAVI31)").
    pub description: String,
    pub free: u64,
    pub total: u64,
}

/// The devices llama.cpp sees (after `ml::gpu::pin` narrowed Vulkan to
/// the chosen card), software rasterizers left out.
fn devices() -> Vec<Device> {
    let mut buf = vec![0u8; 8192];
    // SAFETY: the buffer is valid for its length; the shim writes a
    // NUL-terminated string within it.
    unsafe { pp_judge_devices(buf.as_mut_ptr() as *mut c_char, buf.len()) };
    c_text(&buf)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 5 {
                return None;
            }
            let desc = CString::new(f[1]).ok()?;
            // SAFETY: a valid NUL-terminated string.
            if unsafe { pp_judge_is_software(desc.as_ptr()) } != 0 {
                return None;
            }
            Some(Device { name: f[0].into(), description: f[1].into(), free: f[3].parse().unwrap_or(0), total: f[4].parse().unwrap_or(0) })
        })
        .collect()
}

/// The device among `devices` that is the adapter `ml::gpu` chose: its
/// name in the description, else the only one there is.
pub fn pick<'a>(devices: &'a [Device], adapter: &str) -> Option<&'a Device> {
    let a = adapter.to_lowercase();
    devices.iter().find(|d| !a.is_empty() && (d.description.to_lowercase().contains(&a) || a.contains(&d.description.to_lowercase()))).or(if devices.len() == 1 { devices.first() } else { None })
}

/// The loaded model.
struct Loaded {
    raw: *mut RawJudge,
    prompt: CString,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        // SAFETY: `raw` came from pp_judge_load and is freed once.
        unsafe { pp_judge_free(self.raw) };
    }
}

impl Loaded {
    /// One question about two encoded images: P(yes).
    fn ask(&self, a: &[u8], b: &[u8]) -> Result<(f32, i32), String> {
        let top = judge::TOP_TOKENS;
        let mut ids = vec![0 as c_int; top];
        let mut lps = vec![0f32; top];
        let mut pieces = vec![0u8; 4096];
        let mut err = vec![0u8; 1024];
        let mut n_prompt: c_int = 0;
        // SAFETY: every pointer is valid for the length passed; the shim
        // writes within them.
        let rc = unsafe {
            pp_judge_ask(
                self.raw,
                self.prompt.as_ptr(),
                a.as_ptr(),
                a.len(),
                b.as_ptr(),
                b.len(),
                top as c_int,
                ids.as_mut_ptr(),
                lps.as_mut_ptr(),
                pieces.as_mut_ptr() as *mut c_char,
                pieces.len(),
                &mut n_prompt,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc != 0 {
            return Err(c_text(&err));
        }
        let text = c_text(&pieces);
        let top: Vec<(String, f32)> = text.split('\n').zip(lps).map(|(p, lp)| (p.to_string(), lp)).collect();
        if std::env::var_os("PP_JUDGE_TRACE").is_some() {
            eprintln!("judge-worker: {n_prompt} prompt tokens; top {top:?}");
        }
        Ok((p_yes(&top), n_prompt))
    }
}

/// The helper's main.  `args` are those after [`FLAG`]: `--model-dir
/// <dir>` (the model's folder; the registry's by default).  Returns the
/// exit code.
pub fn main(args: &[String]) -> i32 {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let dir = arg("--model-dir").map(PathBuf::from).or_else(|| models::model_dir(ModelId::Gemma4));
    let Some(dir) = dir else {
        send(&mut out, &Reply::Failed { message: "no home folder for the models".into() });
        return 1;
    };
    let info = models::info(ModelId::Gemma4);
    let (model, mmproj) = (dir.join(info.files[0].file), dir.join(info.files[1].file));
    for f in [&model, &mmproj] {
        if !f.is_file() {
            send(&mut out, &Reply::Failed { message: format!("the close-call judge's model file {} is missing", f.file_name().unwrap_or_default().to_string_lossy()) });
            return 1;
        }
    }
    if let Some(f) = fake_card_failure() {
        send(&mut out, &Reply::CardFailed { failure: f });
        return 2;
    }
    // The card the search helper would use, and Vulkan narrowed to it.
    let adapter = match super::gpu::pin() {
        Ok(a) => a,
        Err(f) => {
            send(&mut out, &Reply::CardFailed { failure: f });
            return 2;
        }
    };
    let echo = std::env::var_os("PP_JUDGE_VERBOSE").is_some();
    // SAFETY: called once, before any other shim call.
    unsafe { pp_judge_init(echo as c_int) };
    let listed = devices();
    let Some(device) = pick(&listed, &adapter.name).cloned() else {
        eprintln!("judge-worker: llama.cpp sees {listed:?}; the chosen card is {}", adapter.name);
        send(&mut out, &Reply::CardFailed { failure: CardFailure::NoCard });
        return 2;
    };
    let started = Instant::now();
    let loaded = match load(&model, &mmproj, &device) {
        Ok(l) => l,
        Err(f) => {
            send(&mut out, &Reply::CardFailed { failure: f });
            return 2;
        }
    };
    let load_ms = started.elapsed().as_millis() as u64;
    eprintln!("judge-worker: loaded on {} ({}) in {load_ms} ms", device.description, device.name);
    send(&mut out, &Reply::Ready { device: device.description.clone(), layers: loaded.1, load_ms });
    serve(&loaded.0, std::io::BufReader::new(std::io::stdin()), out, Duration::from_secs(IDLE_SECS))
}

/// Load on `device`, refusing anything short of every layer there.
fn load(model: &Path, mmproj: &Path, device: &Device) -> Result<(Loaded, i32), CardFailure> {
    let c = |p: &Path| CString::new(p.as_os_str().as_encoded_bytes()).map_err(|_| CardFailure::Failed("a model path holds a NUL".into()));
    let (m, x, d) = (c(model)?, c(mmproj)?, CString::new(device.name.clone()).map_err(|_| CardFailure::NoCard)?);
    let (mut offloaded, mut layers): (c_int, c_int) = (-1, -1);
    let mut err = vec![0u8; 2048];
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8) as c_int;
    // SAFETY: valid NUL-terminated strings and out-pointers; the error
    // buffer is valid for its length.
    let raw = unsafe {
        pp_judge_load(
            m.as_ptr(),
            x.as_ptr(),
            d.as_ptr(),
            judge::N_CTX,
            judge::N_BATCH,
            judge::N_UBATCH,
            judge::IMAGE_TOKENS,
            threads,
            &mut offloaded,
            &mut layers,
            err.as_mut_ptr() as *mut c_char,
            err.len(),
        )
    };
    if raw.is_null() {
        let why = c_text(&err);
        eprintln!("judge-worker: {why} (offloaded {offloaded}/{layers})");
        return Err(CardFailure::Failed(format!("The close-call judge couldn't be loaded on the graphics card ({why}).")));
    }
    // SAFETY: the shim returns a static string.
    let marker = unsafe { CStr::from_ptr(pp_judge_marker()) }.to_string_lossy().into_owned();
    let loaded = Loaded { raw, prompt: CString::new(judge::prompt(&marker)).expect("no NUL in the prompt") };
    // `PP_JUDGE_FAKE_OFFLOAD=<n>` pretends only n layers fit (tests of
    // the refusal on a real card).
    let offloaded = std::env::var("PP_JUDGE_FAKE_OFFLOAD").ok().and_then(|s| s.parse().ok()).unwrap_or(offloaded);
    check_offload(offloaded, layers)?;
    Ok((loaded, layers))
}

/// `PP_JUDGE_FAKE_CARD`, for tests: the card fails in the named way
/// before anything touches a device (`no-card`, `no-fit`, `crash`).
fn fake_card_failure() -> Option<CardFailure> {
    match std::env::var("PP_JUDGE_FAKE_CARD").ok()?.as_str() {
        "no-card" => Some(CardFailure::NoCard),
        "no-fit" => Some(check_offload(20, 31).err()?),
        "crash" => std::process::abort(),
        _ => None,
    }
}

fn send(out: &mut impl Write, reply: &Reply) {
    if let Ok(line) = serde_json::to_string(reply) {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// The request loop: pairs are judged in order; nothing for `idle`
/// exits (the model and the card's memory with the process).
fn serve(model: &Loaded, input: impl BufRead + Send + 'static, mut out: impl Write, idle: Duration) -> i32 {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("judge-worker-stdin".into())
        .spawn(move || {
            for line in input.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Request>(&line) {
                    Ok(r) => {
                        if tx.send(r).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("judge-worker: bad request: {e}"),
                }
            }
        })
        .expect("spawn stdin reader");
    let mut crops = CropCache::default();
    loop {
        let req = match rx.recv_timeout(idle) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                send(&mut out, &Reply::Idle);
                return 0;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
        };
        match req {
            Request::Quit => return 0,
            Request::Judge { id, pairs } => {
                let started = Instant::now();
                let results: Vec<PairResult> = pairs.iter().map(|p| PairResult { result: judge_pair(model, &mut crops, p) }).collect();
                if std::env::var_os("PP_PROFILE").is_some() {
                    eprintln!("judge-worker: {} pairs in {:.0} ms", pairs.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
                send(&mut out, &Reply::Judged { id, results });
            }
        }
    }
}

/// Both orders of one pair.
fn judge_pair(model: &Loaded, crops: &mut CropCache, pair: &PairAsk) -> Result<Verdict, FileError> {
    let a = crops.get(&pair.a)?;
    let b = crops.get(&pair.b)?;
    let (p_ab, _) = model.ask(&a, &b).map_err(|e| FileError::other(format!("judge: {e}")))?;
    let (p_ba, _) = model.ask(&b, &a).map_err(|e| FileError::other(format!("judge: {e}")))?;
    Ok(Verdict { p_ab, p_ba })
}

/// Crops recently cut, by what they show.
#[derive(Default)]
struct CropCache {
    map: HashMap<String, std::sync::Arc<Vec<u8>>>,
    order: VecDeque<String>,
}

impl CropCache {
    fn get(&mut self, crop: &Crop) -> Result<std::sync::Arc<Vec<u8>>, FileError> {
        let id = serde_json::to_string(crop).unwrap_or_default();
        if let Some(c) = self.map.get(&id) {
            return Ok(c.clone());
        }
        let bytes = match crop {
            Crop::Head { path, file_type, key, bbox } => judge::head_crop(path, *file_type, key, *bbox)?,
            Crop::File(p) => std::fs::read(p).map_err(|e| FileError::io(&e))?,
        };
        let c = std::sync::Arc::new(bytes);
        self.map.insert(id.clone(), c.clone());
        self.order.push_back(id);
        while self.order.len() > CROP_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, desc: &str) -> Device {
        Device { name: name.into(), description: desc.into(), free: 0, total: 0 }
    }

    #[test]
    fn the_judge_uses_the_card_the_search_helper_chose() {
        let list = vec![dev("Vulkan0", "AMD Radeon RX 7900 XTX (RADV NAVI31)"), dev("Vulkan1", "AMD Ryzen 7 (RADV RAPHAEL_MENDOCINO)")];
        assert_eq!(pick(&list, "AMD Radeon RX 7900 XTX (RADV NAVI31)").unwrap().name, "Vulkan0");
        assert_eq!(pick(&list, "AMD Ryzen 7 (RADV RAPHAEL_MENDOCINO)").unwrap().name, "Vulkan1");
        assert_eq!(pick(&list, "NVIDIA something"), None, "two cards and neither is the chosen one: none");
        assert_eq!(pick(&list[..1], "a name the driver spells differently").unwrap().name, "Vulkan0", "the only card there is");
        assert_eq!(pick(&[], "AMD"), None, "no card");
    }

    /// The real model on the research's own crops: P(yes) against the
    /// research server's for the same pairs (`judge_gemma_b560_head_p0.json`),
    /// the prompt's length (1 204 tokens with the answer, as the server
    /// logged), the load time and the seconds a pair.  `PP_JUDGE_MODEL_DIR`
    /// holds the two GGUF files, `PP_JUDGE_CROPS` the research's
    /// `crops/head/<row>.jpg`, `PP_JUDGE_REF` its results JSON,
    /// `PP_JUDGE_PAIRS` how many (20).  Run under the card lock.
    #[test]
    #[ignore = "needs the model (PP_JUDGE_MODEL_DIR), the research's crops and results (PP_JUDGE_CROPS, PP_JUDGE_REF) and the card"]
    fn the_real_judge_matches_the_research_server() {
        let dir = PathBuf::from(std::env::var("PP_JUDGE_MODEL_DIR").expect("PP_JUDGE_MODEL_DIR"));
        let crops = PathBuf::from(std::env::var("PP_JUDGE_CROPS").expect("PP_JUDGE_CROPS"));
        let reference: serde_json::Value = serde_json::from_slice(&std::fs::read(std::env::var("PP_JUDGE_REF").expect("PP_JUDGE_REF")).unwrap()).unwrap();
        let n: usize = std::env::var("PP_JUDGE_PAIRS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        let adapter = super::super::gpu::pin().expect("a card");
        // SAFETY: once, first.
        unsafe { pp_judge_init(std::env::var_os("PP_JUDGE_VERBOSE").is_some() as c_int) };
        let listed = devices();
        let device = pick(&listed, &adapter.name).expect("the card in llama.cpp's list").clone();
        let info = models::info(ModelId::Gemma4);
        let t = Instant::now();
        let (model, layers) = load(&dir.join(info.files[0].file), &dir.join(info.files[1].file), &device).expect("loaded on the card");
        eprintln!("loaded {layers} layers on {} in {:.1} s", device.description, t.elapsed().as_secs_f64());
        let mut diffs = Vec::new();
        let mut agree = 0;
        let t = Instant::now();
        let results = reference["results"].as_array().unwrap();
        for r in results.iter().take(n) {
            let (a, b) = (r["a"].as_u64().unwrap(), r["b"].as_u64().unwrap());
            let ja = std::fs::read(crops.join(format!("{a}.jpg"))).unwrap();
            let jb = std::fs::read(crops.join(format!("{b}.jpg"))).unwrap();
            let (p_ab, n_ab) = model.ask(&ja, &jb).unwrap();
            let (p_ba, _) = model.ask(&jb, &ja).unwrap();
            let (r_ab, r_ba) = (r["p_ab"].as_f64().unwrap() as f32, r["p_ba"].as_f64().unwrap() as f32);
            let mine = (p_ab + p_ba) / 2.0;
            let theirs = (r_ab + r_ba) / 2.0;
            diffs.push((mine - theirs).abs());
            agree += ((mine >= judge::P_YES_CUT) == (theirs >= judge::P_YES_CUT)) as usize;
            eprintln!("pair {a}-{b}: mine {p_ab:.4}/{p_ba:.4}, research {r_ab:.4}/{r_ba:.4}; {n_ab} prompt tokens; same {}", r["same"]);
        }
        let secs = t.elapsed().as_secs_f64() / diffs.len().max(1) as f64;
        diffs.sort_by(f32::total_cmp);
        eprintln!(
            "{} pairs: {secs:.2} s a pair (both orders); |P − P_research| median {:.4}, max {:.4}; verdicts agree {agree}/{}",
            diffs.len(),
            diffs[diffs.len() / 2],
            diffs.last().unwrap(),
            diffs.len()
        );
        assert!(agree as f64 >= 0.9 * diffs.len() as f64);
    }
}
