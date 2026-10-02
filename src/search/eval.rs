//! Phase 0: the evaluation harness for library search (ignored by
//! default; plan §8).  It runs the real helper on a folder of previews,
//! ranks a fixed set of queries, writes the candidates a labelling page
//! shows, and, once labels exist, scores precision, recall, the prompt
//! templates and the closeness cut-off.
//!
//! ```text
//! PP_MODEL_DIR=<models> PP_EVAL_PREVIEWS=<folder of JPG previews> \
//! PP_EVAL_OUT=<scratch folder> [PP_EVAL_DEVICE=gpu|cpu] [PP_EVAL_LABELS=<labels.json>] \
//!     cargo test --release -- --ignored --nocapture search::eval::
//! ```
//!
//! - `PP_EVAL_PREVIEWS`: any tree of JPEGs (the study's `prev/<set>/*.jpg`).
//! - `PP_EVAL_OUT/vectors.bin`: the embeddings, cached between runs.
//! - `PP_EVAL_OUT/candidates.json`: per query, the top [`CANDIDATES`]
//!   with scores, for the labelling page.
//! - `PP_EVAL_LABELS`: `{ "<query>": ["<relative preview path>", …] }`,
//!   the photos the user ticked as right.
//!
//! Nothing here touches a library; previews are read from the folder
//! given.  The queries are generic words.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ml::Device;
use crate::ml::client::{Client, Embedder};
use crate::model::record::FileType;
use crate::search::query::{self, CLOSENESS_MAX, CLOSENESS_MIN};

/// Candidates per query the page shows.
pub const CANDIDATES: usize = 50;

/// The queries: things an event, home, travel and nature library holds.
pub const QUERIES: &[&str] = &[
    "camera", "car", "food", "fireworks", "fursuit", "dog", "bird", "cat", "cake", "crowd", "selfie", "night sky", "sunset",
    "snow", "beach", "flowers", "tree", "building", "castle", "bridge", "train", "airplane", "boat", "bicycle", "laptop",
    "phone", "drink", "hotel lobby", "dance floor", "parking lot", "guitar", "swimming pool", "mountain", "hat", "balloon",
    "christmas tree",
];

/// Prompt templates compared on the labelled set; the first is the
/// app's ([`query::prompt`]).
pub const TEMPLATES: &[&str] = &["a photo of a {}.", "{}", "a photo of {}", "a picture of {}", "a photo of a {}"];

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn need(name: &str) -> String {
    env(name).unwrap_or_else(|| panic!("{name} must be set (see src/search/eval.rs)"))
}

fn previews(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if FileType::from_path(&p) == Some(FileType::Jpg) {
                out.push(p);
            }
        }
    }
    walk(root, &mut out);
    out.sort();
    out
}

/// The vectors for every preview: from the cache file, else embedded
/// through the helper and cached.
fn vectors(previews: &[PathBuf], root: &Path, out: &Path, client: &dyn Fn() -> Client) -> HashMap<PathBuf, Vec<f32>> {
    let cache = out.join("vectors.bin");
    let mut have: HashMap<PathBuf, Vec<f32>> = HashMap::new();
    if let Ok(data) = std::fs::read(&cache) {
        let mut r = crate::model::binfile::Reader::open(&data).expect("vectors.bin is sound");
        let n = r.count(8).unwrap();
        for _ in 0..n {
            let p = r.path().unwrap();
            let dim = r.u32().unwrap() as usize;
            let v: Vec<f32> = (0..dim).map(|_| f32::from_bits(r.u32().unwrap())).collect();
            have.insert(p, v);
        }
        eprintln!("vectors: {} cached", have.len());
    }
    let todo: Vec<(PathBuf, FileType)> = previews.iter().filter(|p| !have.contains_key(p.strip_prefix(root).unwrap())).map(|p| (p.clone(), FileType::Jpg)).collect();
    if !todo.is_empty() {
        let c = client();
        let (name, check) = c.device_report();
        eprintln!("embedding {} previews ({name:?}, self-check {check:?})", todo.len());
        let t = Instant::now();
        let mut failed = 0;
        for chunk in todo.chunks(64) {
            let results = c.embed_images(chunk).expect("the helper answers");
            for r in results {
                match r.result {
                    Ok(e) => {
                        have.insert(r.path.strip_prefix(root).unwrap().to_path_buf(), e.vector);
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("  {}: {}", r.path.display(), e.message);
                    }
                }
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("embedded {} in {:.0} ms ({:.1} ms each), {failed} failed", todo.len(), ms, ms / todo.len().max(1) as f64);
        c.shutdown();
        let mut w = crate::model::binfile::Writer(Vec::new());
        w.u32(have.len() as u32);
        let mut keys: Vec<&PathBuf> = have.keys().collect();
        keys.sort();
        for k in keys {
            w.path(k);
            let v = &have[k];
            w.u32(v.len() as u32);
            for x in v {
                w.u32(x.to_bits());
            }
        }
        std::fs::create_dir_all(out).unwrap();
        std::fs::write(&cache, w.seal()).unwrap();
    }
    have
}

struct Setup {
    root: PathBuf,
    out: PathBuf,
    /// Relative preview paths, sorted, and their vectors in the same order.
    paths: Vec<PathBuf>,
    vectors: Vec<Vec<f32>>,
    client: Client,
}

fn setup() -> Setup {
    let root = PathBuf::from(need("PP_EVAL_PREVIEWS"));
    let out = PathBuf::from(need("PP_EVAL_OUT"));
    need("PP_MODEL_DIR");
    let device = match env("PP_EVAL_DEVICE").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let files = previews(&root);
    assert!(files.len() >= 10, "{} holds {} JPEGs; the sample should hold hundreds", root.display(), files.len());
    let start = move || Client::start(device).unwrap_or_else(|e| panic!("the helper couldn't start on {}: {e:?}", device.label()));
    let have = vectors(&files, &root, &out, &start);
    let mut paths: Vec<PathBuf> = have.keys().cloned().collect();
    paths.sort();
    let vectors = paths.iter().map(|p| have[p].clone()).collect();
    // Text encoding needs a helper too (its text tower runs on the CPU).
    let client = Client::start_text_only().expect("a text-only helper");
    Setup { root, out, paths, vectors, client }
}

impl Setup {
    fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.client.encode_text(texts).expect("texts encode")
    }

    /// Cosine of every preview against `q`.
    fn scores(&self, q: &[f32]) -> Vec<f32> {
        self.vectors.iter().map(|v| v.iter().zip(q).map(|(a, b)| a * b).sum()).collect()
    }

    fn ranked(&self, scores: &[f32]) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..scores.len()).collect();
        idx.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        idx
    }
}

/// Write the candidates the labelling page shows, and the score spread
/// of each query.
#[test]
#[ignore = "phase 0: needs PP_MODEL_DIR, PP_EVAL_PREVIEWS and PP_EVAL_OUT"]
fn candidates() {
    let s = setup();
    let prompts: Vec<String> = QUERIES.iter().map(|q| query::prompt(q)).collect();
    let t = Instant::now();
    let qv = s.encode(&prompts);
    eprintln!("{} queries encoded in {:.0} ms", prompts.len(), t.elapsed().as_secs_f64() * 1000.0);
    let fursuit = s.encode(&[query::prompt("fursuit")]).swap_remove(0);
    let fursuit_scores = s.scores(&fursuit);
    let mut doc = serde_json::Map::new();
    let mut queries = serde_json::Map::new();
    for (q, v) in QUERIES.iter().zip(&qv) {
        let scores = s.scores(v);
        let (median, mad) = query::median_and_mad(&scores);
        let ranked = s.ranked(&scores);
        let items: Vec<serde_json::Value> = ranked
            .iter()
            .take(CANDIDATES)
            .enumerate()
            .map(|(rank, &i)| {
                serde_json::json!({
                    "path": s.paths[i].to_string_lossy(),
                    "score": scores[i],
                    "z": (scores[i] - median) / (1.4826 * mad).max(1e-4),
                    "rank": rank + 1,
                    "fursuit_beats": fursuit_scores[i] > scores[i],
                })
            })
            .collect();
        let above: BTreeMap<String, usize> = [2.0f32, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0]
            .iter()
            .map(|&z| (format!("{z}"), scores.iter().filter(|&&x| x >= median + z * 1.4826 * mad).count()))
            .collect();
        eprintln!("{q:>16}: top {:.3}, median {median:.3}, MAD {mad:.4}; above z: {above:?}", scores[ranked[0]]);
        queries.insert(q.to_string(), serde_json::json!({ "median": median, "mad": mad, "candidates": items, "animal": crate::search::vocab::is_animal_query(q) }));
    }
    doc.insert("previews".into(), serde_json::json!(s.paths.len()));
    doc.insert("queries".into(), serde_json::Value::Object(queries));
    std::fs::create_dir_all(&s.out).unwrap();
    let path = s.out.join("candidates.json");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    eprintln!("wrote {} ({} previews under {})", path.display(), s.paths.len(), s.root.display());
    s.client.shutdown();
}

/// Score the labels: precision at 10 and 50, recall within the pool,
/// the prompt templates, the closeness sweep and the fursuit contrast.
#[test]
#[ignore = "phase 0: needs the candidates run and PP_EVAL_LABELS"]
fn score_labels() {
    let s = setup();
    let labels: HashMap<String, Vec<String>> = serde_json::from_str(&std::fs::read_to_string(need("PP_EVAL_LABELS")).expect("labels file")).expect("labels JSON");
    let index_of: HashMap<&Path, usize> = s.paths.iter().enumerate().map(|(i, p)| (p.as_path(), i)).collect();
    let relevant: BTreeMap<&str, HashSet<usize>> = labels
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(q, v)| (q.as_str(), v.iter().filter_map(|p| index_of.get(Path::new(p)).copied()).collect()))
        .collect();
    assert!(!relevant.is_empty(), "no labelled queries");
    eprintln!("{} labelled queries, {} relevant photos in all", relevant.len(), relevant.values().map(HashSet::len).sum::<usize>());

    // Templates: mean average precision over the labelled pool.
    for tpl in TEMPLATES {
        let prompts: Vec<String> = relevant.keys().map(|q| tpl.replace("{}", q)).collect();
        let qv = s.encode(&prompts);
        let mut aps = Vec::new();
        let mut p10 = Vec::new();
        for ((_, rel), v) in relevant.iter().zip(&qv) {
            let scores = s.scores(v);
            let ranked = s.ranked(&scores);
            let mut hits = 0;
            let mut sum = 0.0;
            for (k, &i) in ranked.iter().take(CANDIDATES).enumerate() {
                if rel.contains(&i) {
                    hits += 1;
                    sum += hits as f64 / (k + 1) as f64;
                }
            }
            aps.push(if rel.is_empty() { 0.0 } else { sum / rel.len().min(CANDIDATES) as f64 });
            p10.push(ranked.iter().take(10).filter(|i| rel.contains(i)).count() as f64 / 10.0);
        }
        eprintln!("template {tpl:>20}: mAP {:.3}, mean P@10 {:.3}", mean(&aps), mean(&p10));
    }

    // The app's template: per query numbers and the closeness sweep.
    let prompts: Vec<String> = relevant.keys().map(|q| query::prompt(q)).collect();
    let qv = s.encode(&prompts);
    let fursuit = s.encode(&[query::prompt("fursuit")]).swap_remove(0);
    let fursuit_scores = s.scores(&fursuit);
    let mut per_query: Vec<(&str, Vec<f32>)> = Vec::new();
    eprintln!("{:>16} {:>5} {:>5} {:>8} {:>8}", "query", "P@10", "P@50", "recall50", "relevant");
    for ((q, rel), v) in relevant.iter().zip(&qv) {
        let scores = s.scores(v);
        let ranked = s.ranked(&scores);
        let p10 = ranked.iter().take(10).filter(|i| rel.contains(i)).count();
        let p50 = ranked.iter().take(50).filter(|i| rel.contains(i)).count();
        eprintln!("{q:>16} {:>5.2} {:>5.2} {:>8.2} {:>8}", p10 as f64 / 10.0, p50 as f64 / 50.0, p50 as f64 / rel.len() as f64, rel.len());
        if crate::search::vocab::is_animal_query(q) {
            let contrast: Vec<usize> = ranked.iter().copied().filter(|&i| scores[i] > fursuit_scores[i]).collect();
            let c10 = contrast.iter().take(10).filter(|i| rel.contains(i)).count();
            eprintln!("{:>16} with the fursuit contrast: P@10 {:.2} ({} photos pass the contrast)", "", c10 as f64 / 10.0, contrast.len());
        }
        per_query.push((q, scores));
    }
    eprintln!("closeness sweep (z above the median in robust SDs): mean precision / recall / F1 over the labelled queries, mean hits");
    let mut best = (0.0f64, 0.0f32);
    let mut z = CLOSENESS_MIN;
    while z <= CLOSENESS_MAX + 1e-6 {
        let (mut ps, mut rs, mut fs, mut ns) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (q, scores) in &per_query {
            let rel = &relevant[q];
            let hits: Vec<usize> = query::cut(scores, |_| false, z).into_iter().map(|(i, _)| i).collect();
            let tp = hits.iter().filter(|i| rel.contains(i)).count() as f64;
            let p = if hits.is_empty() { 1.0 } else { tp / hits.len() as f64 };
            let r = tp / rel.len() as f64;
            ps.push(p);
            rs.push(r);
            fs.push(if p + r > 0.0 { 2.0 * p * r / (p + r) } else { 0.0 });
            ns.push(hits.len() as f64);
        }
        let f = mean(&fs);
        eprintln!("  z {z:>4.2}: P {:.3}  R {:.3}  F1 {f:.3}  hits {:.1}", mean(&ps), mean(&rs), mean(&ns));
        if f > best.0 {
            best = (f, z);
        }
        z += 0.25;
    }
    eprintln!("best mean F1 {:.3} at closeness {:.2} (the default candidate; the slider's range is {CLOSENESS_MIN}–{CLOSENESS_MAX})", best.0, best.1);
    s.client.shutdown();
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}
