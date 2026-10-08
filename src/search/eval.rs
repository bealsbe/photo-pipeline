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
            let results = c.embed_images(chunk, &crate::ml::client::Want::search()).expect("the helper answers");
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
    let start = move || Client::start(crate::ml::client::Spec::images(device, None)).unwrap_or_else(|e| panic!("the helper couldn't start on {}: {e:?}", device.label()));
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
    eprintln!("relative sweep (z above the median in robust SDs, the rule the app doesn't use): mean precision / recall / F1 over the labelled queries, mean hits");
    let mut best = (0.0f64, 0.0f32);
    let mut z = 2.0f32;
    while z <= 6.0 + 1e-6 {
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
    eprintln!("best mean F1 {:.3} at z {:.2}", best.0, best.1);

    // The app's rule: the model's own match probability.
    eprintln!(
        "probability sweep (the app's rule; slider {CLOSENESS_MIN}–{CLOSENESS_MAX}, default {}): mean precision / recall / F1, mean hits",
        query::CLOSENESS_DEFAULT
    );
    let mut best_p = (0.0f64, 0.0f32);
    for &p in &[0.0003f32, 0.0005, 0.001, 0.002, 0.003, 0.005, 0.01, 0.02, 0.03, 0.05, 0.1, 0.2, 0.3] {
        let (mut ps, mut rs, mut fs, mut ns) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (q, scores) in &per_query {
            let rel = &relevant[q];
            let hits: Vec<usize> = query::cut_by_probability(scores, |_| false, p).into_iter().map(|(i, _)| i).collect();
            let tp = hits.iter().filter(|i| rel.contains(i)).count() as f64;
            let pr = if hits.is_empty() { 1.0 } else { tp / hits.len() as f64 };
            let r = tp / rel.len() as f64;
            ps.push(pr);
            rs.push(r);
            fs.push(if pr + r > 0.0 { 2.0 * pr * r / (pr + r) } else { 0.0 });
            ns.push(hits.len() as f64);
        }
        let f = mean(&fs);
        eprintln!("  p ≥ {p:>4.2} (cos ≥ {:.3}): P {:.3}  R {:.3}  F1 {f:.3}  hits {:.1}", query::cosine_for_probability(p), mean(&ps), mean(&rs), mean(&ns));
        if f > best_p.0 {
            best_p = (f, p);
        }
    }
    eprintln!("best mean F1 {:.3} at p ≥ {:.2}", best_p.0, best_p.1);
    // Per query, how the two rules fare at their best settings.
    eprintln!("{:>16} {:>9} {:>9} {:>9} {:>9}", "query", "z hits", "z right", "p hits", "p right");
    for (q, scores) in &per_query {
        let rel = &relevant[q];
        let z: Vec<usize> = query::cut(scores, |_| false, best.1).into_iter().map(|(i, _)| i).collect();
        let p: Vec<usize> = query::cut_by_probability(scores, |_| false, best_p.1).into_iter().map(|(i, _)| i).collect();
        eprintln!("{q:>16} {:>9} {:>9} {:>9} {:>9}", z.len(), z.iter().filter(|i| rel.contains(i)).count(), p.len(), p.iter().filter(|i| rel.contains(i)).count());
    }
    s.client.shutdown();
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

// ── phase 2: the hidden tags and the query syntax ───────────────────────────

/// The tag thresholds against the labels: for each vocabulary word, how
/// many previews carry it at each probability floor, and for the words
/// the user labelled, how many of those are right (precision) and how
/// many of the right ones are found (recall within the labelled pool).
/// Needs the `candidates` run (the cached vectors) and `PP_EVAL_LABELS`.
#[test]
#[ignore = "phase 2: needs the candidates run and PP_EVAL_LABELS"]
fn tag_thresholds() {
    use crate::search::tags::{DEFAULT_THRESHOLD, VocabVectors};
    use crate::search::vocab::{self, VOCABULARY};
    let s = setup();
    let labels: HashMap<String, Vec<String>> = serde_json::from_str(&std::fs::read_to_string(need("PP_EVAL_LABELS")).expect("labels file")).expect("labels JSON");
    let index_of: HashMap<&Path, usize> = s.paths.iter().enumerate().map(|(i, p)| (p.as_path(), i)).collect();
    let t = Instant::now();
    let rows = s.encode(&VocabVectors::prompts());
    let vocab = VocabVectors::from_rows(&rows).expect("every word encoded");
    eprintln!("{} vocabulary words encoded in {:.0} ms", VOCABULARY.len(), t.elapsed().as_secs_f64() * 1000.0);
    let floors = [0.005f32, 0.01, 0.02, 0.03, 0.05, 0.1, 0.3];
    // probs[j][i]: the match probability of preview i for word j.
    let t = Instant::now();
    let probs: Vec<Vec<f32>> = (0..VOCABULARY.len()).map(|j| s.scores(vocab.row(j)).into_iter().map(query::match_probability).collect()).collect();
    eprintln!("{} previews × {} words in {:.1} ms", s.paths.len(), VOCABULARY.len(), t.elapsed().as_secs_f64() * 1000.0);
    eprintln!("{:>18} {:>7}  {}", "word", "top", floors.iter().map(|f| format!("{f:>6}")).collect::<Vec<_>>().join(" "));
    for (j, w) in VOCABULARY.iter().enumerate() {
        let p = &probs[j];
        let top = p.iter().copied().fold(0f32, f32::max);
        let counts: Vec<String> = floors.iter().map(|&f| format!("{:>6}", p.iter().filter(|&&x| x >= f).count())).collect();
        eprintln!("{:>18} {top:>7.3}  {}", w.word, counts.join(" "));
    }
    eprintln!("\nlabelled words (tagged / right / of labelled; precision, recall) at each floor; default {DEFAULT_THRESHOLD}");
    let mut rows_out: Vec<(String, Vec<(usize, usize, usize)>)> = Vec::new();
    for (q, right) in &labels {
        let Some(j) = vocab::find(q) else { continue };
        let rel: HashSet<usize> = right.iter().filter_map(|p| index_of.get(Path::new(p)).copied()).collect();
        if rel.is_empty() {
            continue;
        }
        let per: Vec<(usize, usize, usize)> = floors
            .iter()
            .map(|&f| {
                let tagged: Vec<usize> = (0..s.paths.len()).filter(|&i| probs[j][i] >= f).collect();
                (tagged.len(), tagged.iter().filter(|i| rel.contains(i)).count(), rel.len())
            })
            .collect();
        rows_out.push((q.clone(), per));
    }
    rows_out.sort();
    for (q, per) in &rows_out {
        let cells: Vec<String> = per.iter().map(|(n, r, of)| format!("{n:>3}/{r:>3}/{of:>3}")).collect();
        eprintln!("{q:>18}  {}", cells.join("  "));
    }
    for (k, f) in floors.iter().enumerate() {
        let (mut ps, mut rs) = (Vec::new(), Vec::new());
        for (_, per) in &rows_out {
            let (n, r, of) = per[k];
            ps.push(if n == 0 { 1.0 } else { r as f64 / n as f64 });
            rs.push(r as f64 / of as f64);
        }
        eprintln!("  floor {f:<5}: mean precision {:.3}, mean recall {:.3}", mean(&ps), mean(&rs));
    }
    // At the floor under study (`PP_EVAL_TAG_FLOOR`, else the default):
    // every word with photos, how many of them "fursuit" beats, and the
    // Things list as the page would show it; the top photos per word go
    // to `PP_EVAL_OUT/tags.json` for contact sheets.
    let floor: f32 = env("PP_EVAL_TAG_FLOOR").and_then(|f| f.parse().ok()).unwrap_or(DEFAULT_THRESHOLD);
    let fursuit = vocab::find("fursuit").unwrap();
    eprintln!("\nat floor {floor}: tagged, of which fursuit beats the word (* = a Thing)");
    let mut doc = serde_json::Map::new();
    let mut things: Vec<(usize, &str)> = Vec::new();
    for (j, w) in VOCABULARY.iter().enumerate() {
        let f = w.threshold.unwrap_or(floor);
        let mut tagged: Vec<usize> = (0..s.paths.len()).filter(|&i| probs[j][i] >= f).collect();
        if tagged.is_empty() {
            continue;
        }
        tagged.sort_by(|&a, &b| probs[j][b].total_cmp(&probs[j][a]));
        let beaten = tagged.iter().filter(|&&i| probs[fursuit][i] > probs[j][i]).count();
        let thing = !w.kind.is_subject();
        eprintln!("{:>18}{} {:>4} tagged, {beaten:>4} look more like fursuits", w.word, if thing { "*" } else { " " }, tagged.len());
        if thing {
            things.push((tagged.len(), w.plural));
        }
        let items: Vec<serde_json::Value> = tagged.iter().take(48).map(|&i| serde_json::json!([s.paths[i].to_string_lossy(), probs[j][i], probs[fursuit][i]])).collect();
        doc.insert(w.word.to_string(), serde_json::Value::Array(items));
    }
    things.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    eprintln!("things by the floor alone: {}", things.iter().map(|(n, w)| format!("{w} {n}")).collect::<Vec<_>>().join(", "));
    let any: usize = (0..s.paths.len()).filter(|&i| vocab::things().any(|(j, w)| probs[j][i] >= w.threshold.unwrap_or(floor))).count();
    eprintln!("previews carrying any Thing: {any} of {}", s.paths.len());
    // The app's own derivation (the contrast included) at the default
    // floor, through `Tags::sync` on the sample as f16 rows.
    let mut vectors = crate::search::index::Vectors { dim: vocab.dim, ..Default::default() };
    for (i, v) in s.vectors.iter().enumerate() {
        let key = crate::search::index::Key::of(i as u64, s.paths[i].as_os_str().as_encoded_bytes());
        vectors.by_key.insert(key, i);
        vectors.records.push(crate::search::index::Record { key, capture: None, meta: Default::default(), accidental: false });
        vectors.data.extend(v.iter().map(|&x| query::f32_to_f16(x)));
    }
    let mut tags = crate::search::tags::Tags::default();
    let t = Instant::now();
    tags.sync(&vectors, 1, &vocab);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let list = tags.things(|_| true);
    eprintln!("things as the app derives them (default floor, the fursuit contrast; {ms:.1} ms): {}", list.iter().map(|t| format!("{} {}", t.word, t.photos)).collect::<Vec<_>>().join(", "));
    let counts = tags.counts(|_| true);
    for w in ["fursuit", "cosplay", "dog", "stuffed animal", "person"] {
        eprintln!("  {w}: {}", counts[vocab::find(w).unwrap()]);
    }
    std::fs::write(s.out.join("tags.json"), serde_json::to_string(&doc).unwrap()).unwrap();
    s.client.shutdown();
}

/// The engine on a scratch library (symlinks into the archive are fine:
/// it only reads): the first index on the card, then the example
/// queries of the phase 2 round with their hit counts, the fursuits
/// left out and the Things list.  The index goes in `PP_EVAL_OUT/index`.
///
/// ```text
/// PP_MODEL_DIR=<models> PP_EVAL_LIBRARY=<library root> PP_EVAL_OUT=<scratch> \
///     cargo test --release -- --ignored --nocapture --exact search::eval::library_queries
/// ```
#[test]
/// Phase 3's check on a scratch library of symlinks (read only): the first
/// index with faces on the chosen device, the clusters that follow, naming
/// the largest, `@name` queries clearest first, suggestions, a tile, undo
/// and *Forget all*.  Prints counts only, never names or paths of people.
///
/// ```sh
/// PP_MODEL_DIR=<siglip2 dir> PP_MODEL_ROOT=<models root> PP_EVAL_LIBRARY=<root> \
///   PP_EVAL_OUT=<scratch> [PP_EVAL_DEVICE=gpu|cpu] [PP_EVAL_FACE_MODEL=aura-face|s-face|insight-face] \
///   cargo test --release people_library -- --ignored --nocapture
/// ```
#[ignore = "phase 3: needs PP_MODEL_DIR, PP_MODEL_ROOT, PP_EVAL_LIBRARY and PP_EVAL_OUT"]
fn people_library() {
    use crate::bus::BgEvent;
    use crate::ml::models::{self, ModelId};
    use crate::model::library_cache::{self, Store};
    use crate::model::library_index;
    use crate::model::subjects::{Framing, GroupId, Kind};
    use crate::search::engine::{Config, Engine};
    use std::sync::Arc;
    use std::time::Duration;
    let root = PathBuf::from(need("PP_EVAL_LIBRARY"));
    let out = PathBuf::from(need("PP_EVAL_OUT"));
    need("PP_MODEL_DIR");
    need("PP_MODEL_ROOT");
    let device = match env("PP_EVAL_DEVICE").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let face_model = env("PP_EVAL_FACE_MODEL").and_then(|s| serde_json::from_value::<ModelId>(serde_json::Value::String(s)).ok()).unwrap_or(models::FACE_MODEL_DEFAULT);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let scanned = library_cache::scan(&root, None, true, &cancelled, &|_| {}).unwrap().unwrap();
    let store = parking_lot::Mutex::new(Store::new(root.clone()));
    let lib = Arc::new(library_index::index_from_tree(&root, &scanned.tree, &store, &cancelled).unwrap());
    eprintln!("library: {} shots in {} days", lib.total_shots, lib.len());
    let (bus, rx) = crate::bus::channel();
    let subjects_path = out.join("subjects.json");
    let _ = std::fs::remove_file(&subjects_path);
    let config = Config { factory: Client::factory(), installed: Box::new(|_| true), index_dir: Some(out.join("index")), subjects_path: Some(subjects_path.clone()), downloader: crate::search::engine::no_downloads() };
    let engine = Engine::with_config(bus, device, true, true, face_model, false, config);
    engine.on_library(lib.clone());
    let t = Instant::now();
    engine.start_first_index(&models::models_for(Some(face_model)));
    let deadline = Instant::now() + Duration::from_secs(3600);
    let status = loop {
        let s = engine.status();
        if s.stopped.is_some() || (s.first_index_asked && s.pass.is_none() && s.indexed_shots > 0) {
            break s;
        }
        assert!(Instant::now() < deadline, "the index didn't finish: {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.stopped.is_none(), "{:?}", status.stopped);
    eprintln!(
        "indexed {} shots in {:.1} s on {:?}; faces: on={} model={:?} looked_at={} found={} unrecognised={}",
        status.indexed_shots,
        t.elapsed().as_secs_f64(),
        status.device.name,
        status.faces.on,
        status.faces.model,
        status.faces.looked_at,
        status.faces.found,
        status.faces.unrecognised
    );
    assert_eq!(status.faces.looked_at, status.indexed_shots, "every indexed shot was looked at for faces");
    let mut stream: std::pin::Pin<Box<dyn iced::futures::Stream<Item = Vec<BgEvent>>>> = Box::pin(crate::bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    // The regroup after the pass: wait for a snapshot with clusters.
    let wait_subjects = |stream: &mut std::pin::Pin<Box<dyn iced::futures::Stream<Item = Vec<BgEvent>>>>, pred: &dyn Fn(&crate::model::subjects::Subjects) -> bool| -> Arc<crate::model::subjects::Subjects> {
        rt.block_on(async {
            use iced::futures::StreamExt;
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut latest = engine.subjects();
            while !pred(&latest) && Instant::now() < deadline {
                let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await else { continue };
                for ev in batch {
                    if let BgEvent::Subjects(s) = ev {
                        latest = s;
                    }
                }
            }
            latest
        })
    };
    let snap = wait_subjects(&mut stream, &|s| s.groups.iter().any(|g| matches!(g.id, GroupId::Cluster(_))) || status.faces.found == 0);
    let clusters: Vec<&crate::model::subjects::Group> = snap.groups.iter().filter(|g| matches!(g.id, GroupId::Cluster(_))).collect();
    let sizes: Vec<usize> = clusters.iter().map(|g| g.photos).collect();
    eprintln!("groups: {} clusters over {} faces; photos per cluster (largest first): {:?}; of >= 5 photos: {}", clusters.len(), snap.faces_found, &sizes[..sizes.len().min(20)], sizes.iter().filter(|&&n| n >= 5).count());
    assert!(clusters.iter().all(|g| g.kind == Kind::Person && g.name.is_none()));
    let Some(largest) = clusters.first() else {
        engine.shutdown();
        return;
    };
    // Its faces, clearest first; a tile from the cache; suggestions.
    engine.faces(1, largest.id.clone());
    engine.suggestions(2, largest.id.clone());
    let (faces, suggestions) = rt.block_on(async {
        use iced::futures::StreamExt;
        let (mut f, mut s) = (None, None);
        let deadline = Instant::now() + Duration::from_secs(30);
        while (f.is_none() || s.is_none()) && Instant::now() < deadline {
            let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await else { continue };
            for ev in batch {
                match ev {
                    BgEvent::SubjectFaces { seq: 1, faces, .. } => f = Some(faces),
                    BgEvent::Suggestions { seq: 2, list, .. } => s = Some(list),
                    _ => {}
                }
            }
        }
        (f.expect("faces"), s.expect("suggestions"))
    });
    eprintln!("largest cluster: {} faces, clarity {:.2}..{:.2}, {} bursts to check", faces.len(), faces.first().map_or(0.0, |f| f.clarity), faces.last().map_or(0.0, |f| f.clarity), suggestions.len());
    assert!(faces.windows(2).all(|w| w[0].clarity >= w[1].clarity), "clearest first");
    let t = Instant::now();
    let tile = engine.crop(&largest.cover, 128, Framing::Face).expect("a tile");
    let whole = engine.crop(&largest.cover, 300, Framing::Photo).expect("the photo");
    eprintln!("crop: tile {}x{} and photo {}x{} in {:.0} ms", tile.0, tile.1, whole.0, whole.1, t.elapsed().as_secs_f64() * 1000.0);
    // Name it, then search for it.
    engine.name_group(largest.id.clone(), "Someone".into());
    let snap = wait_subjects(&mut stream, &|s| s.groups.iter().any(|g| g.name.as_deref() == Some("Someone")));
    let named = snap.groups.iter().find(|g| g.name.as_deref() == Some("Someone")).expect("named");
    eprintln!("named: {} photos on {} days, {} suggestions, undoable {:?}", named.photos, named.days, named.suggestions, snap.undoable);
    assert!(snap.undoable.is_some());
    for (seq, q) in [(10u64, "@Someone"), (11, "@Someone camera"), (12, "@Nobody"), (13, "camera")] {
        let t = Instant::now();
        engine.query(seq, q, crate::search::CLOSENESS_DEFAULT, &[]);
        let got = rt.block_on(async {
            use iced::futures::StreamExt;
            loop {
                let Some(batch) = stream.next().await else { return None };
                for ev in batch {
                    if let BgEvent::SearchResults { seq: s, hits, error, subjects, .. } = ev {
                        if s == seq {
                            return Some((hits, error, subjects));
                        }
                    }
                }
            }
        });
        let (hits, error, subjects) = got.expect("an answer");
        eprintln!("{q:>16}: {} hits, {} subjects applied, error {error:?}; {:.0} ms; first scores {:?}", hits.len(), subjects.len(), t.elapsed().as_secs_f64() * 1000.0, hits.iter().take(3).map(|h| h.score).collect::<Vec<_>>());
        match q {
            "@Someone" => {
                assert_eq!(hits.len(), named.photos);
                assert!(hits.windows(2).all(|w| w[0].score >= w[1].score), "clearest first");
                assert_eq!(subjects.len(), 1);
            }
            "@Nobody" => assert!(hits.is_empty() && subjects.is_empty()),
            _ => {}
        }
    }
    // Undo the naming; forget all.
    engine.undo();
    let snap = wait_subjects(&mut stream, &|s| !s.groups.iter().any(|g| g.name.is_some()));
    assert!(snap.groups.iter().all(|g| g.name.is_none()));
    // Every kind forgotten: the file goes when no subject remains.
    engine.forget_kind(crate::model::subjects::Kind::Person);
    engine.forget_kind(crate::model::subjects::Kind::Fursuit);
    let snap = wait_subjects(&mut stream, &|s| s.faces_found == 0);
    eprintln!("after forget all: {} groups, {} faces, subjects file exists {}", snap.groups.len(), snap.faces_found, subjects_path.exists());
    assert!(snap.groups.is_empty() && !subjects_path.exists());
    engine.shutdown();
    std::thread::sleep(Duration::from_millis(200));
}

/// Phase 4's check on a scratch library of symlinks (read only): the
/// first index with the head step on the chosen device (the card scans
/// at once; the processor asks, and the test answers yes), the fursuit
/// clusters that follow, a tile, *Find more* on the largest, naming it
/// and an `@name` query; the resident size at each stage.  Prints counts
/// and times only.
///
/// ```sh
/// PP_MODEL_DIR=<siglip2 dir> PP_MODEL_ROOT=<models root> PP_EVAL_LIBRARY=<root> \
///   PP_EVAL_OUT=<scratch> [PP_EVAL_DEVICE=gpu|cpu] \
///   cargo test --release fursuits_library -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs the models, a scratch library and PP_EVAL_* (see the doc comment)"]
fn fursuits_library() {
    use crate::bus::BgEvent;
    use crate::ml::models::ModelId;
    use crate::model::library_cache::{self, Store};
    use crate::model::library_index;
    use crate::model::subjects::{Framing, GroupId, Kind};
    use crate::search::engine::{Config, Engine};
    use crate::search::heads::Step;
    use std::sync::Arc;
    use std::time::Duration;
    fn rss_mb() -> f64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
    }
    let root = PathBuf::from(need("PP_EVAL_LIBRARY"));
    let out = PathBuf::from(need("PP_EVAL_OUT"));
    need("PP_MODEL_DIR");
    need("PP_MODEL_ROOT");
    let device = match env("PP_EVAL_DEVICE").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let scanned = library_cache::scan(&root, None, true, &cancelled, &|_| {}).unwrap().unwrap();
    let store = parking_lot::Mutex::new(Store::new(root.clone()));
    let lib = Arc::new(library_index::index_from_tree(&root, &scanned.tree, &store, &cancelled).unwrap());
    eprintln!("library: {} shots in {} days; resident {:.0} MB", lib.total_shots, lib.len(), rss_mb());
    let (bus, rx) = crate::bus::channel();
    let subjects_path = out.join("subjects.json");
    let _ = std::fs::remove_file(&subjects_path);
    let config = Config { factory: Client::factory(), installed: Box::new(|_| true), index_dir: Some(out.join("index")), subjects_path: Some(subjects_path.clone()), downloader: crate::search::engine::no_downloads() };
    let engine = Engine::with_config(bus, device, true, false, crate::ml::models::FACE_MODEL_DEFAULT, true, config);
    engine.on_library(lib.clone());
    let t = Instant::now();
    engine.start_first_index(&[ModelId::Siglip2, ModelId::Owlv2, ModelId::Dinov2]);
    let deadline = Instant::now() + Duration::from_secs(3600);
    let status = loop {
        let s = engine.status();
        if s.stopped.is_some() || (s.first_index_asked && s.pass.is_none() && s.indexed_shots > 0) {
            break s;
        }
        assert!(Instant::now() < deadline, "the index didn't finish: {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.stopped.is_none(), "{:?}", status.stopped);
    let index_s = t.elapsed().as_secs_f64();
    eprintln!("indexed {} shots in {index_s:.1} s on {:?}; heads: on={} installed={} tagged={} step={:?}; resident {:.0} MB", status.indexed_shots, status.device.name, status.heads.on, status.heads.models_installed, status.heads.tagged, status.heads.step, rss_mb());
    // The step: the card scans at once; the processor asks (decision 11).
    let s = loop {
        let s = engine.status();
        if !matches!(s.heads.step, Step::Idle) || s.heads.tagged == 0 {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Step::Asking(offer) = s.heads.step {
        assert_eq!(device, Device::Cpu, "only the processor asks");
        eprintln!("asked: {} photos ({:?}); answering yes", offer.photos, offer.why);
        engine.answer_heads(true);
    } else {
        assert_eq!(device, Device::Gpu, "the card starts unasked");
    }
    let t = Instant::now();
    let status = loop {
        let s = engine.status();
        if s.stopped.is_some() || (matches!(s.heads.step, Step::Idle | Step::Failed(_)) && s.heads.scanned > 0) || s.heads.tagged == 0 {
            break s;
        }
        assert!(Instant::now() < deadline, "the scan didn't finish: {s:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let scan_s = t.elapsed().as_secs_f64();
    assert!(matches!(status.heads.step, Step::Idle), "{:?} {:?}", status.heads.step, status.stopped);
    eprintln!("scanned {} of {} fursuit photos in {scan_s:.1} s ({:.2} s a photo): {} heads; resident {:.0} MB", status.heads.scanned, status.heads.tagged, scan_s / status.heads.scanned.max(1) as f64, status.heads.found, rss_mb());
    assert_eq!(status.heads.scanned, status.heads.tagged, "every tagged photo looked at");
    let mut stream: std::pin::Pin<Box<dyn iced::futures::Stream<Item = Vec<BgEvent>>>> = Box::pin(crate::bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let wait_subjects = |stream: &mut std::pin::Pin<Box<dyn iced::futures::Stream<Item = Vec<BgEvent>>>>, pred: &dyn Fn(&crate::model::subjects::Subjects) -> bool| -> Arc<crate::model::subjects::Subjects> {
        rt.block_on(async {
            use iced::futures::StreamExt;
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut latest = engine.subjects();
            while !pred(&latest) && Instant::now() < deadline {
                let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await else { continue };
                for ev in batch {
                    if let BgEvent::Subjects(s) = ev {
                        latest = s;
                    }
                }
            }
            latest
        })
    };
    // The full regroup after the scan replaces the clusters the pass
    // assigned (new ids): let it settle before reading the groups, else
    // a name goes to a stale id and is refused.
    std::thread::sleep(Duration::from_secs(3));
    let snap = wait_subjects(&mut stream, &|s| s.groups.iter().any(|g| g.kind == Kind::Fursuit) || status.heads.found == 0);
    let clusters: Vec<&crate::model::subjects::Group> = snap.groups.iter().filter(|g| g.kind == Kind::Fursuit && matches!(g.id, GroupId::Cluster(_))).collect();
    let sizes: Vec<usize> = clusters.iter().map(|g| g.photos).collect();
    eprintln!("groups: {} fursuit clusters; photos per cluster (largest first): {:?}; of >= 3 photos: {}; resident {:.0} MB", clusters.len(), &sizes[..sizes.len().min(20)], sizes.iter().filter(|&&n| n >= 3).count(), rss_mb());
    assert!(snap.groups.iter().all(|g| g.kind != Kind::Person), "no people here: faces were off");
    let Some(largest) = clusters.first() else {
        engine.shutdown();
        return;
    };
    engine.faces(1, largest.id.clone());
    engine.find_more(2, largest.id.clone());
    engine.suggestions(3, largest.id.clone());
    let (heads, more, check) = rt.block_on(async {
        use iced::futures::StreamExt;
        let (mut f, mut m, mut c) = (None, None, None);
        let deadline = Instant::now() + Duration::from_secs(30);
        while (f.is_none() || m.is_none() || c.is_none()) && Instant::now() < deadline {
            let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await else { continue };
            for ev in batch {
                match ev {
                    BgEvent::SubjectFaces { seq: 1, faces, .. } => f = Some(faces),
                    BgEvent::Suggestions { seq: 2, list, .. } => m = Some(list),
                    BgEvent::Suggestions { seq: 3, list, .. } => c = Some(list),
                    _ => {}
                }
            }
        }
        (f.expect("heads"), m.expect("find more"), c.expect("check"))
    });
    eprintln!("largest cluster: {} heads, clarity {:.2}..{:.2}; check this group: {} bursts; find more (strict): {} bursts", heads.len(), heads.first().map_or(0.0, |f| f.clarity), heads.last().map_or(0.0, |f| f.clarity), check.len(), more.len());
    assert!(heads.windows(2).all(|w| w[0].clarity >= w[1].clarity), "clearest first");
    let t = Instant::now();
    let tile = engine.crop(&largest.cover, 128, Framing::Face).expect("a tile");
    let whole = engine.crop(&largest.cover, 300, Framing::Photo).expect("the photo");
    eprintln!("crop: tile {}x{} and photo {}x{} in {:.0} ms", tile.0, tile.1, whole.0, whole.1, t.elapsed().as_secs_f64() * 1000.0);
    engine.name_group(largest.id.clone(), "Suitone".into());
    let snap = wait_subjects(&mut stream, &|s| s.groups.iter().any(|g| g.name.as_deref() == Some("Suitone")));
    let named = snap.groups.iter().find(|g| g.name.as_deref() == Some("Suitone")).expect("named");
    assert_eq!(named.kind, Kind::Fursuit);
    eprintln!("named: {} photos on {} days, {} suggestions, undoable {:?}", named.photos, named.days, named.suggestions, snap.undoable);
    let t = Instant::now();
    engine.query(10, "@Suitone", crate::search::CLOSENESS_DEFAULT, &[]);
    let hits = rt.block_on(async {
        use iced::futures::StreamExt;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "no answer");
            let Ok(Some(batch)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await else { continue };
            for ev in batch {
                if let BgEvent::SearchResults { seq: 10, hits, also_fursuits, .. } = ev {
                    assert!(also_fursuits.is_empty());
                    return hits;
                }
            }
        }
    });
    eprintln!("@name: {} photos in {:.0} ms; resident {:.0} MB", hits.len(), t.elapsed().as_secs_f64() * 1000.0, rss_mb());
    assert_eq!(hits.len(), named.photos);
    // Every kind forgotten: the file goes when no subject remains.
    engine.forget_kind(crate::model::subjects::Kind::Person);
    engine.forget_kind(crate::model::subjects::Kind::Fursuit);
    std::thread::sleep(Duration::from_millis(500));
    engine.shutdown();
    assert!(!subjects_path.exists(), "forget all removes the file");
    eprintln!("done; resident {:.0} MB", rss_mb());
}

#[test]
#[ignore = "phase 2: needs PP_MODEL_DIR, PP_EVAL_LIBRARY and PP_EVAL_OUT"]
fn library_queries() {
    use crate::bus::BgEvent;
    use crate::model::library_cache::{self, Store};
    use crate::model::library_index;
    use crate::search::engine::{Config, Engine};
    use std::sync::Arc;
    use std::time::Duration;
    let root = PathBuf::from(need("PP_EVAL_LIBRARY"));
    let out = PathBuf::from(need("PP_EVAL_OUT"));
    need("PP_MODEL_DIR");
    let device = match env("PP_EVAL_DEVICE").as_deref() {
        Some("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let scanned = library_cache::scan(&root, None, true, &cancelled, &|_| {}).unwrap().unwrap();
    let store = parking_lot::Mutex::new(Store::new(root.clone()));
    let lib = Arc::new(library_index::index_from_tree(&root, &scanned.tree, &store, &cancelled).unwrap());
    eprintln!("library: {} shots in {} days", lib.total_shots, lib.len());
    let (bus, rx) = crate::bus::channel();
    let config = Config { factory: Client::factory(), installed: Box::new(|_| true), index_dir: Some(out.join("index")), subjects_path: None, downloader: crate::search::engine::no_downloads() };
    let engine = Engine::with_config(bus, device, true, false, crate::ml::models::FACE_MODEL_DEFAULT, false, config);
    engine.on_library(lib.clone());
    let t = Instant::now();
    engine.start_first_index(&[crate::ml::models::ModelId::Siglip2]);
    let deadline = Instant::now() + Duration::from_secs(3600);
    let status = loop {
        let s = engine.status();
        if s.stopped.is_some() || (s.first_index_asked && s.pass.is_none() && s.indexed_shots > 0) {
            break s;
        }
        assert!(Instant::now() < deadline, "the index didn't finish: {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.stopped.is_none(), "{:?}", status.stopped);
    eprintln!("indexed {} shots in {:.1} s on {:?} (self-check {:?})", status.indexed_shots, t.elapsed().as_secs_f64(), status.device.name, status.device.self_check);
    let mut stream = Box::pin(crate::bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    // Things arrive once the tags are derived.
    let things = rt.block_on(async {
        use iced::futures::StreamExt;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut latest = engine.things();
        while latest.is_empty() && Instant::now() < deadline {
            let Some(batch) = stream.next().await else { break };
            for ev in batch {
                if let BgEvent::SearchThings(t) = ev {
                    latest = t;
                }
            }
        }
        latest
    });
    eprintln!("things ({}): {}", things.len(), things.iter().map(|t| format!("{} {}", t.word, t.photos)).collect::<Vec<_>>().join(", "));
    assert!(things.iter().all(|t| !t.kind.is_subject()), "Things holds a subject kind: {things:?}");
    let queries: Vec<String> = env("PP_EVAL_QUERIES").map(|q| q.split(';').map(|s| s.trim().to_string()).collect()).unwrap_or_else(|| {
        ["dog", "dog fursuit", "fursuit", "cat", "bird", "fireworks", "fireworks -crowd", "camera", "camera 2026", "cameras", "car", "food", "\"a photo of a camera.\"", "@Someone", "@Someone camera", "crowd -fursuit", "2026", "stage", "sunset jun 2026"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    for (seq, q) in queries.iter().enumerate() {
        let t = Instant::now();
        engine.query(seq as u64, q, crate::search::CLOSENESS_DEFAULT, &[]);
        let got = rt.block_on(async {
            use iced::futures::StreamExt;
            loop {
                let Some(batch) = stream.next().await else { return None };
                for ev in batch {
                    if let BgEvent::SearchResults { seq: s, hits, error, understood, fursuits, animal, .. } = ev {
                        if s as usize == seq {
                            return Some((hits, error, understood, fursuits, animal));
                        }
                    }
                }
            }
        });
        let Some((hits, error, understood, fursuits, animal)) = got else { panic!("no answer for {q:?}") };
        let tagged = hits.iter().filter(|h| h.tagged).count();
        let days: BTreeMap<String, usize> = hits.iter().fold(BTreeMap::new(), |mut m, h| {
            *m.entry(h.day.format("%Y-%m").to_string()).or_default() += 1;
            m
        });
        let top = hits.iter().chain(fursuits.iter()).map(|h| h.score).fold(f32::MIN, f32::max);
        let plain = hits.iter().filter(|h| query::match_probability(h.score) >= crate::search::tags::DEFAULT_THRESHOLD).count();
        eprintln!(
            "{q:>24}: {:>4} hits ({tagged} tagged first, {plain} at the tag floor, top p {:.3}), {:>3} fursuits left out{}; {:.0} ms; understood words={:?} exact={} exclude={:?} dates={:?} names={:?}; error={error:?}; months {days:?}",
            hits.len(),
            if top > f32::MIN { query::match_probability(top) } else { 0.0 },
            fursuits.len(),
            animal.as_ref().map(|a| format!(" (animal {a})")).unwrap_or_default(),
            t.elapsed().as_secs_f64() * 1000.0,
            understood.words,
            understood.exact,
            understood.exclude,
            understood.dates.map(|d| d.label()),
            understood.names,
        );
        // `PP_EVAL_LIST=<query>`: that query's files, tagged ones marked,
        // for a look by eye.
        if env("PP_EVAL_LIST").as_deref() == Some(q.as_str()) {
            for h in hits.iter().chain(fursuits.iter()) {
                eprintln!("  {} {:.3} {}", if h.tagged { "T" } else { " " }, query::match_probability(h.score), h.jpg.as_deref().or(h.raw.as_deref()).map(|p| p.display().to_string()).unwrap_or_default());
            }
        }
        let in_hits: HashSet<u64> = hits.iter().map(|h| h.id).collect();
        assert!(fursuits.iter().all(|f| !in_hits.contains(&f.id)), "{q}: a fursuit is in both lists");
        assert!(animal.is_some() || fursuits.is_empty(), "{q}: fursuits without an animal word");
        assert!(hits.iter().take(tagged).all(|h| h.tagged) && hits.iter().skip(tagged).all(|h| !h.tagged), "{q}: tagged first");
        if let Some(d) = understood.dates {
            assert!(hits.iter().chain(fursuits.iter()).all(|h| d.contains(h.day)), "{q}: a hit outside the dates");
        }
    }
    engine.shutdown();
    std::thread::sleep(Duration::from_millis(200));
}

/// Infrared frames (the user's converted body) against the vocabulary:
/// how well candidate "infrared" prompts find them, which words they
/// trip, and "snow"/"sunset" before and after the contrast, scored
/// against the user's labels.  `PP_EVAL_MODELS` is a TSV of
/// `preview<TAB>camera model<TAB>source` (every frame whose model
/// contains `PP_EVAL_IR_MODEL`, default `ILCE-6000`, is infrared).
#[test]
#[ignore = "phase 2: needs the candidates run, PP_EVAL_LABELS and PP_EVAL_MODELS"]
fn infrared() {
    use crate::search::tags::DEFAULT_THRESHOLD;
    use crate::search::vocab::{self, VOCABULARY};
    let s = setup();
    let labels: HashMap<String, Vec<String>> = serde_json::from_str(&std::fs::read_to_string(need("PP_EVAL_LABELS")).expect("labels file")).expect("labels JSON");
    let index_of: HashMap<&Path, usize> = s.paths.iter().enumerate().map(|(i, p)| (p.as_path(), i)).collect();
    let ir_model = env("PP_EVAL_IR_MODEL").unwrap_or_else(|| "ILCE-6000".into());
    let mut ir: HashSet<usize> = HashSet::new();
    let mut unknown: HashSet<usize> = HashSet::new();
    let mut known = 0;
    for line in std::fs::read_to_string(need("PP_EVAL_MODELS")).expect("models file").lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        if let Some(&i) = index_of.get(Path::new(cols[0])) {
            if cols[1] == "?" {
                unknown.insert(i);
                continue;
            }
            known += 1;
            if cols[1].contains(&ir_model) {
                ir.insert(i);
            }
        }
    }
    let n = s.paths.len();
    eprintln!("{known} of {n} previews have a camera model ({} unknown); {} are infrared ({ir_model})", unknown.len(), ir.len());
    assert!(!ir.is_empty(), "no infrared frames in the models file");
    let default_floor = query::cosine_for_probability(query::CLOSENESS_DEFAULT);
    let tag_floor = query::cosine_for_probability(DEFAULT_THRESHOLD);
    let pr = |hits: &[usize], right: &HashSet<usize>| -> (usize, usize, f64, f64) {
        let tp = hits.iter().filter(|i| right.contains(i)).count();
        (hits.len(), tp, if hits.is_empty() { 1.0 } else { tp as f64 / hits.len() as f64 }, if right.is_empty() { 0.0 } else { tp as f64 / right.len() as f64 })
    };
    // Average precision of a ranking against a set.
    let ap = |scores: &[f32], right: &HashSet<usize>| -> f64 {
        let ranked = s.ranked(scores);
        let (mut hits, mut sum) = (0usize, 0.0f64);
        for (k, &i) in ranked.iter().enumerate() {
            if right.contains(&i) {
                hits += 1;
                sum += hits as f64 / (k + 1) as f64;
            }
        }
        if right.is_empty() { 0.0 } else { sum / right.len() as f64 }
    };

    // 1. Candidate prompts for the word.
    let candidates: Vec<String> = env("PP_EVAL_IR_PROMPTS").map(|p| p.split(';').map(|x| x.trim().to_string()).collect()).unwrap_or_else(|| {
        [
            query::prompt("infrared"),
            "an infrared photo.".to_string(),
            "infrared photography".to_string(),
            "an infrared photograph.".to_string(),
            "a photo taken with an infrared camera.".to_string(),
            "a false-colour infrared photo with white foliage.".to_string(),
            "an infrared landscape photo.".to_string(),
            "infrared".to_string(),
        ]
        .to_vec()
    });
    let qv = s.encode(&candidates);
    eprintln!("\ninfrared prompts against the {} infrared frames: hits at the search default / right / precision / recall, the same at the tag floor, AP, top p", ir.len());
    let mut best: Option<(f64, usize)> = None;
    let mut cand_scores: Vec<Vec<f32>> = Vec::new();
    for (k, (p, v)) in candidates.iter().zip(&qv).enumerate() {
        let scores = s.scores(v);
        let at_default: Vec<usize> = (0..n).filter(|&i| scores[i] >= default_floor).collect();
        let at_tag: Vec<usize> = (0..n).filter(|&i| scores[i] >= tag_floor).collect();
        let (h, r, prec, rec) = pr(&at_default, &ir);
        let (h2, r2, prec2, rec2) = pr(&at_tag, &ir);
        let a = ap(&scores, &ir);
        let top = scores.iter().copied().fold(f32::MIN, f32::max);
        eprintln!("{p:>52}: {h:>4}/{r:>4} P {prec:.2} R {rec:.2} | tag {h2:>4}/{r2:>4} P {prec2:.2} R {rec2:.2} | AP {a:.3} | top p {:.3}", query::match_probability(top));
        if best.is_none_or(|(b, _)| a > b) {
            best = Some((a, k));
        }
        cand_scores.push(scores);
    }
    let (_, best_k) = best.unwrap();
    let ir_scores = cand_scores[best_k].clone();
    eprintln!("best by AP: {:?}", candidates[best_k]);
    // The ranking's shape: where do the infrared frames sit?
    let ranked = s.ranked(&ir_scores);
    let in_top = |k: usize, set: &HashSet<usize>| ranked.iter().take(k).filter(|i| set.contains(i)).count();
    eprintln!(
        "infrared frames in the top 10/25/50/100/200: {}/{}/{}/{}/{}; frames of unknown model there: {}/{}/{}/{}/{}",
        in_top(10, &ir),
        in_top(25, &ir),
        in_top(50, &ir),
        in_top(100, &ir),
        in_top(200, &ir),
        in_top(10, &unknown),
        in_top(25, &unknown),
        in_top(50, &unknown),
        in_top(100, &unknown),
        in_top(200, &unknown)
    );
    eprintln!("the top 60 by the best prompt (I = infrared by model, ? = unknown model):");
    for &i in ranked.iter().take(60) {
        let mark = if ir.contains(&i) { "I" } else if unknown.contains(&i) { "?" } else { " " };
        eprintln!("  {mark} {:.3} {}", query::match_probability(ir_scores[i]), s.paths[i].display());
    }

    // 2. Which vocabulary words do infrared frames trip?  Hits at the
    // search default and tags at the floor, with their infrared share.
    let vocab_v = s.encode(&VOCABULARY.iter().map(|w| w.prompt()).collect::<Vec<_>>());
    eprintln!("\nwords tripped by infrared frames (hits at the search default: infrared of all; tags at the floor: infrared of all); shown where ≥ 3 infrared or ≥ 30 %");
    let mut tripped: Vec<(f64, String)> = Vec::new();
    for (w, v) in VOCABULARY.iter().zip(&vocab_v) {
        let scores = s.scores(v);
        let hits: Vec<usize> = (0..n).filter(|&i| scores[i] >= default_floor).collect();
        let tags: Vec<usize> = (0..n).filter(|&i| scores[i] >= tag_floor).collect();
        let hi = hits.iter().filter(|i| ir.contains(i)).count();
        let ti = tags.iter().filter(|i| ir.contains(i)).count();
        let share = if hits.is_empty() { 0.0 } else { hi as f64 / hits.len() as f64 };
        if hi >= 3 || (share >= 0.3 && !hits.is_empty()) {
            tripped.push((share, format!("{:>18}: hits {hi:>4} of {:>4} ({:.0} %), tags {ti:>3} of {:>3}", w.word, hits.len(), share * 100.0, tags.len())));
        }
    }
    tripped.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, line) in &tripped {
        eprintln!("{line}");
    }

    // 3. Before and after for the labelled words that trip, against the
    // user's labels: hits at the search default, right, precision,
    // recall; the wrong hits that are infrared; right ones the contrast
    // would drop.
    let words: Vec<String> = env("PP_EVAL_IR_WORDS").map(|p| p.split(';').map(|x| x.trim().to_string()).collect()).unwrap_or_else(|| ["snow", "sunset", "night sky", "tree", "forest", "mountain", "beach", "flowers", "building"].iter().map(|s| s.to_string()).collect());
    eprintln!("\n{:>12} {:>22} {:>22} {:>8} {:>8}", "word", "before hits/right/P/R", "after hits/right/P/R", "wrong IR", "right lost");
    for w in &words {
        let Some(right) = labels.get(w) else { continue };
        let right: HashSet<usize> = right.iter().filter_map(|p| index_of.get(Path::new(p)).copied()).collect();
        let v = s.encode(&[vocab::query_prompt(w)]).swap_remove(0);
        let scores = s.scores(&v);
        let before: Vec<usize> = (0..n).filter(|&i| scores[i] >= default_floor).collect();
        let after: Vec<usize> = before.iter().copied().filter(|&i| scores[i] > ir_scores[i]).collect();
        let (bh, br, bp, brc) = pr(&before, &right);
        let (ah, ar, ap_, arc) = pr(&after, &right);
        let wrong_ir = before.iter().filter(|i| !right.contains(i) && ir.contains(i)).count();
        let wrong = before.len() - br;
        let lost = before.iter().filter(|i| right.contains(i) && !after.contains(i)).count();
        eprintln!("{w:>12} {:>22} {:>22} {:>8} {:>8}   (labelled {}, of them infrared {})", format!("{bh}/{br}/{bp:.2}/{brc:.2}"), format!("{ah}/{ar}/{ap_:.2}/{arc:.2}"), format!("{wrong_ir} of {wrong}"), lost, right.len(), right.iter().filter(|i| ir.contains(i)).count());
        // The labelling page showed the top 50 by rank: the same, there.
        let top: Vec<usize> = s.ranked(&scores).into_iter().take(CANDIDATES).collect();
        let kept: Vec<usize> = s.ranked(&scores).into_iter().filter(|&i| scores[i] > ir_scores[i]).take(CANDIDATES).collect();
        let count = |v: &[usize], set: &HashSet<usize>| v.iter().filter(|i| set.contains(i)).count();
        eprintln!(
            "{:>12} top 50 by rank: right {}, infrared {}, unknown model {}; with the contrast the top 50 hold right {}, infrared {}, unknown {} (right dropped from the top 50: {})",
            "",
            count(&top, &right),
            count(&top, &ir),
            count(&top, &unknown),
            count(&kept, &right),
            count(&kept, &ir),
            count(&kept, &unknown),
            top.iter().filter(|i| right.contains(i) && !kept.contains(i)).count()
        );
    }
    s.client.shutdown();
}
