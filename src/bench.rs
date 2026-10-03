//! Speed comparison with the Python app — ignored by default:
//!
//! ```text
//! PP_BENCH_DIR=<folder> cargo test --release -- --ignored --nocapture --exact bench::bench
//! ```
//!
//! `PP_BENCH_SCAN_ONLY=1` stops after the scan (for slow or remote folders).
//!
//! Times the production paths: a full scan (walk, stat, threaded capture
//! times, batching), thumbnail generation per file type (one thread, then
//! 8 in parallel), and a full-size viewer decode including the mip pyramid.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use rayon::prelude::*;

use crate::bus::{self, BgEvent};
use crate::imaging::{loader, thumbs};
use crate::model::record::PhotoRecord;
use crate::scan;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

#[test]
#[ignore]
fn bench() {
    let Ok(dir) = std::env::var("PP_BENCH_DIR") else { return };
    let dir = PathBuf::from(dir);

    // 1. Scan, through the same worker and bus the app uses.
    let t = Instant::now();
    let (bus, rx) = bus::channel();
    let _scan = scan::start(1, dir, bus, false);
    let mut stream = Box::pin(bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let records: Vec<PhotoRecord> = rt.block_on(async {
        use iced::futures::StreamExt;
        let mut out = Vec::new();
        while let Some(batch) = stream.next().await {
            for ev in batch {
                match ev {
                    BgEvent::ScanBatch { records, .. } => out.extend(records.iter().cloned()),
                    BgEvent::ScanComplete { .. } | BgEvent::ScanError { .. } => return out,
                    _ => {}
                }
            }
        }
        out
    });
    println!("scan            {:>6} files  {:>8.0} ms", records.len(), ms(t));
    if std::env::var("PP_BENCH_SCAN_ONLY").is_ok() {
        return;
    }

    // One record per distinct file name (the bench folder repeats a shoot).
    let mut seen = HashSet::new();
    let distinct: Vec<&PhotoRecord> = records.iter().filter(|r| seen.insert(r.filename())).collect();

    // 2. Thumbnails, one thread, median of 3 per file, grouped by extension.
    for ext in ["JPG", "ARW", "CR2"] {
        let group: Vec<&&PhotoRecord> = distinct.iter().filter(|r| r.filename().ends_with(ext)).collect();
        if group.is_empty() {
            continue;
        }
        let mut times = Vec::new();
        for r in &group {
            let mut rep = Vec::new();
            for _ in 0..3 {
                let t = Instant::now();
                let px = thumbs::generate(&r.path, r.file_type, thumbs::GENERATION_SIZE);
                rep.push(ms(t));
                assert!(px.is_some(), "no thumbnail for {}", r.path.display());
            }
            times.push(median(rep));
        }
        println!("thumb {ext}       {:>3} files  {:>8.1} ms each (median)", group.len(), median(times));
    }

    // 3. Thumbnails, 8 threads over the first 1000 records.
    let batch: Vec<&PhotoRecord> = records.iter().take(1000).collect();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
    let t = Instant::now();
    pool.install(|| {
        batch.par_iter().for_each(|r| {
            let _ = thumbs::generate(&r.path, r.file_type, thumbs::GENERATION_SIZE);
        })
    });
    let total = ms(t);
    println!("thumbs x8       {:>6} files  {:>8.0} ms  ({:.2} ms/file)", batch.len(), total, total / batch.len() as f64);

    // 4a. Viewer, first picture: the embedded preview.
    for ext in ["JPG", "ARW", "CR2"] {
        let group: Vec<&&PhotoRecord> = distinct.iter().filter(|r| r.filename().ends_with(ext)).take(4).collect();
        if group.is_empty() {
            continue;
        }
        let mut times = Vec::new();
        let mut shown = 0;
        for r in &group {
            let t = Instant::now();
            let p = loader::load_preview(&r.path, r.file_type);
            times.push(ms(t));
            shown += p.is_some() as usize;
        }
        println!("preview {ext}     {:>3} files  {:>8.1} ms each (median), {shown} with a preview", group.len(), median(times));
    }

    // 4. Viewer: full-size decode plus the mip pyramid it is shown from.
    for ext in ["JPG", "ARW", "CR2"] {
        let group: Vec<&&PhotoRecord> = distinct.iter().filter(|r| r.filename().ends_with(ext)).take(4).collect();
        if group.is_empty() {
            continue;
        }
        let mut times = Vec::new();
        for r in &group {
            let t = Instant::now();
            let px = loader::load(&r.path, r.file_type).expect("decode");
            let img = loader::FullImage::from_pixels(px);
            times.push(ms(t));
            assert!(img.width > 0);
        }
        println!("viewer {ext}      {:>3} files  {:>8.0} ms each (median)", group.len(), median(times));
    }
}


/// Writes each fixture thumbnail as raw RGB for comparison with Python's.
#[test]
#[ignore]
fn dump_thumbs() {
    let (Ok(dir), Ok(out)) = (std::env::var("PP_THUMB_SRC"), std::env::var("PP_THUMB_OUT")) else { return };
    let (dir, out) = (PathBuf::from(dir), PathBuf::from(out));
    std::fs::create_dir_all(&out).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    names.sort();
    for p in names {
        let Some(ft) = crate::model::record::FileType::from_path(&p) else { continue };
        let px = thumbs::generate(&p, ft, thumbs::GENERATION_SIZE).expect("thumb");
        let rgb = crate::imaging::decode::to_rgb(&px);
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        std::fs::write(out.join(format!("{name}.{}x{}.rgb", px.width, px.height)), rgb).unwrap();
    }
}

/// Capture-time reads, one thread, over the files listed in `PP_BENCH_LIST`
/// (one path per line) — isolates per-file latency on slow or remote disks.
#[test]
#[ignore]
fn bench_capture_times() {
    let Ok(list) = std::env::var("PP_BENCH_LIST") else { return };
    let paths: Vec<PathBuf> = std::fs::read_to_string(list).unwrap().lines().map(PathBuf::from).collect();
    let t = Instant::now();
    let mut found = 0;
    for p in &paths {
        let raw = crate::model::record::FileType::from_path(p) == Some(crate::model::record::FileType::Raw);
        found += crate::exif::read_capture_time(p, raw).is_some() as usize;
    }
    println!("capture times   {:>6} files  {:>8.0} ms  ({found} found)", paths.len(), ms(t));
}

/// The calendar's library index over `PP_BENCH_DIR` (read-only): a full
/// read, a check for changes against it, building the index, and opening
/// days (the median-sized one and the largest).
#[test]
#[ignore]
fn bench_index() {
    use crate::model::library_cache::{self, Store};
    use crate::model::library_index;
    let Ok(dir) = std::env::var("PP_BENCH_DIR") else { return };
    let dir = PathBuf::from(dir);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let t = Instant::now();
    let full = library_cache::scan(&dir, None, true, &cancelled, &|_| {}).unwrap().unwrap();
    println!("full read       {:>6} files  {:>8.0} ms  ({} folders)", full.tree.file_count(), ms(t), full.tree.folders.len());
    let t = Instant::now();
    let check = library_cache::scan(&dir, Some(&full.tree), false, &cancelled, &|_| {}).unwrap().unwrap();
    println!("check           {:>6} files  {:>8.0} ms  ({} folders re-read, changed={})", check.tree.file_count(), ms(t), check.reread, check.changed);
    let store = parking_lot::Mutex::new(Store::new(dir.clone()));
    store.lock().set_tree(std::sync::Arc::new(full.tree.clone()));
    let t = Instant::now();
    store.lock().save();
    let saved = ms(t);
    let t = Instant::now();
    let mut again = Store::new(dir.clone());
    again.load();
    let loaded = again.tree.as_deref() == Some(&full.tree);
    println!("cache           save {saved:>5.0} ms  load {:>5.0} ms  (loads back identical: {loaded})", ms(t));
    let t = Instant::now();
    let ix = library_index::index_from_tree(&dir, &full.tree, &store, &cancelled).unwrap();
    println!("index build     {:>6} files  {:>8.0} ms  ({} days)", ix.total_files, ms(t), ix.len());
    let mut days: Vec<_> = ix.days.values().collect();
    days.sort_by_key(|d| d.files.len());
    for d in [days[days.len() / 2], days[days.len() - 1]] {
        let t = Instant::now();
        let (records, missing) = library_index::day_records(&d.files, &store.lock());
        let quick = ms(t);
        let t = Instant::now();
        let exif = library_cache::read_capture_times(&d.files.iter().collect::<Vec<_>>());
        let agree = records.iter().zip(&exif).filter(|(r, e)| r.capture_time.map(|t| t.format("%Y%m%d%H%M").to_string()) == e.map(|t| t.format("%Y%m%d%H%M").to_string())).count();
        println!(
            "day {}  {:>6} files  {:>8.1} ms without reads ({} need one); reading all EXIF {:>8.0} ms; stem time = EXIF minute for {agree}",
            d.date, d.files.len(), quick, missing.len(), ms(t)
        );
        // What the calendar then does with them on the UI thread.
        let t = Instant::now();
        let mut coll = crate::model::collection::Collection::new();
        for r in records {
            coll.add(r);
        }
        coll.build_pairs();
        let state = crate::model::filter::FilterState::default();
        let sorted = crate::model::filter::visible_sorted(&coll, &state);
        let ids = crate::model::filter::dedup_pairs(&sorted, &coll);
        let groups = crate::model::filter::group_by_date(&ids, &coll, &state);
        println!("    day grid data  {:>8.1} ms  ({} tiles, {} sections)", ms(t), ids.len(), groups.len());
    }
}

/// Thumbnails for the files listed in `PP_BENCH_LIST`, 8 at a time as the
/// app makes them, with the bytes the CIFS client read meanwhile (Linux).
#[test]
#[ignore]
fn bench_thumbs_list() {
    let Ok(list) = std::env::var("PP_BENCH_LIST") else { return };
    let paths: Vec<PathBuf> = std::fs::read_to_string(list).unwrap().lines().map(PathBuf::from).collect();
    let cifs_read = || {
        std::fs::read_to_string("/proc/fs/cifs/Stats").ok().and_then(|s| {
            s.lines().find(|l| l.contains("Bytes read")).and_then(|l| l.split_whitespace().nth(2)?.parse::<u64>().ok())
        })
    };
    let before = cifs_read();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
    let t = Instant::now();
    let ok = pool.install(|| {
        paths
            .par_iter()
            .filter(|p| {
                let ft = crate::model::record::FileType::from_path(p).unwrap();
                thumbs::generate(p, ft, thumbs::GENERATION_SIZE).is_some()
            })
            .count()
    });
    let wire = match (before, cifs_read()) {
        (Some(a), Some(b)) => format!("{:.0} KB/file over the network", (b - a) as f64 / paths.len() as f64 / 1024.0),
        _ => String::new(),
    };
    println!("thumbs          {:>6} files  {:>8.1} ms each ({ok} ok)  {wire}", paths.len(), ms(t) / paths.len() as f64);
}

/// Shot metadata as the scan reads it, for every photo directly in
/// `PP_SHOT_DIR`, one tab-separated line each (name, capture time,
/// sub-second ms, sequence, AF point as fractions of the photo as shown),
/// to check against `exiftool`.
#[test]
#[ignore]
fn dump_shot_meta() {
    let Ok(dir) = std::env::var("PP_SHOT_DIR") else { return };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    paths.sort();
    let t = Instant::now();
    let mut n = 0;
    for p in &paths {
        let Some(ft) = crate::model::record::FileType::from_path(p) else { continue };
        let (time, m) = crate::exif::read_shot(p, ft == crate::model::record::FileType::Raw);
        let opt = |v: Option<u16>| v.map(|x| x.to_string()).unwrap_or_default();
        let focus = m.focus_point().map(|(x, y)| format!("{x:.4}\t{y:.4}")).unwrap_or("\t".into());
        let time = time.map(|t| t.format("%Y:%m:%d %H:%M:%S").to_string()).unwrap_or_default();
        println!("{}\t{time}\t{}\t{}\t{focus}\t{:08x}", crate::util::file_name(p), opt(m.subsec_ms), opt(m.sequence), m.camera);
        n += 1;
    }
    eprintln!("read {n} headers in {:.0} ms", ms(t));
}

/// Burst grouping over a whole shoot's metadata, from an `exiftool -n -csv`
/// export at `PP_BURST_CSV` (columns SourceFile, SubSecTimeOriginal,
/// DateTimeOriginal, SequenceNumber): presses found with all the metadata,
/// then as an older body would see them (no sequence numbers; then whole
/// seconds only), each scored against the presses the sequence numbers
/// mark.  Reads only the CSV.
#[test]
#[ignore]
fn bench_bursts() {
    use crate::model::bursts::Bursts;
    use crate::model::collection::Collection;
    use crate::model::record::{FileType, MTime, ShotMeta};
    use chrono::NaiveDateTime;
    let Ok(csv) = std::env::var("PP_BURST_CSV") else { return };
    let text = std::fs::read_to_string(csv).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split(',').collect();
    let col = |name: &str| header.iter().position(|h| *h == name).unwrap();
    let (c_file, c_sub, c_dto, c_seq) = (col("SourceFile"), col("SubSecTimeOriginal"), col("DateTimeOriginal"), col("SequenceNumber"));
    let rows: Vec<Vec<String>> = lines.map(|l| l.split(',').map(str::to_string).collect()).collect();
    let build = |keep_seq: bool, keep_subsec: bool| -> (Collection, f64) {
        let mut c = Collection::new();
        for r in &rows {
            let path = PathBuf::from("/day").join(r[c_file].trim_start_matches("./"));
            let time = NaiveDateTime::parse_from_str(&r[c_dto], "%Y:%m:%d %H:%M:%S").ok();
            let mut rec = PhotoRecord::new(path, FileType::Jpg, 1, crate::model::record::MTime::from_unix(0, 0), time);
            rec.shot = ShotMeta {
                subsec_ms: crate::exif::parse_subsec(r[c_sub].as_bytes()).filter(|_| keep_subsec),
                sequence: r[c_seq].parse().ok().filter(|_| keep_seq),
                focus: None,
                camera: 1,
            };
            c.add(rec);
        }
        c.build_pairs();
        let t = Instant::now();
        let _ = Bursts::find(&c);
        (c, ms(t))
    };
    let _ = MTime::from_unix(0, 0);
    // Every shot's press, as the names in it (singles included).
    let presses = |c: &Collection, b: &Bursts| -> HashSet<Vec<String>> {
        let mut out: HashSet<Vec<String>> =
            (0..b.len() as u32).map(|i| b.frames(i).iter().map(|&id| c.get(id).unwrap().filename()).collect()).collect();
        for (id, r) in c.iter_ids() {
            if b.of(id).is_none() {
                out.insert(vec![r.filename()]);
            }
        }
        out
    };
    let (c, t_full) = build(true, true);
    let truth = presses(&c, &Bursts::find(&c));
    let multi: Vec<&Vec<String>> = truth.iter().filter(|p| p.len() > 1).collect();
    println!(
        "{} frames: {} presses, {} of them bursts holding {} frames; grouping took {t_full:.1} ms",
        c.len(),
        truth.len(),
        multi.len(),
        multi.iter().map(|p| p.len()).sum::<usize>()
    );
    for (label, seq, sub) in [("sub-second times only", false, true), ("whole seconds + file numbers", false, false)] {
        let (c, _) = build(seq, sub);
        let got = presses(&c, &Bursts::find(&c));
        let exact = got.intersection(&truth).count();
        let spanning = got.iter().filter(|p| truth.iter().filter(|t| t.iter().any(|f| p.contains(f))).count() > 1).count();
        println!(
            "{label:>30}: {} presses, {exact} of {} exactly right ({:.1} %), {spanning} spanning two or more presses",
            got.len(),
            truth.len(),
            100.0 * exact as f64 / truth.len() as f64
        );
    }
}

/// Library search (plan §9): the first index of a library through the
/// real engine and helper, then queries.
///
/// ```text
/// PP_BENCH_LIBRARY=<library root> PP_MODEL_DIR=<models> [PP_BENCH_DEVICE=gpu|cpu] \
///     cargo test --release -- --ignored --nocapture --exact bench::bench_search_index
/// ```
///
/// Reads the library only (a scratch folder of symlinks is fine); the
/// index goes to a temporary folder that is removed at the end.  Prints
/// the time per shot (the share's bandwidth decides on the card), the
/// helper's device report, and the time of ten queries.
#[test]
#[ignore]
fn bench_search_index() {
    use crate::ml::Device;
    use crate::model::library_cache::{self, Store};
    use crate::model::library_index;
    use crate::search::engine::{Config, Engine};
    use std::sync::Arc;
    use std::time::Duration;
    let Ok(dir) = std::env::var("PP_BENCH_LIBRARY") else { return };
    assert!(std::env::var("PP_MODEL_DIR").is_ok(), "PP_MODEL_DIR must point at the model folder");
    let device = match std::env::var("PP_BENCH_DEVICE").as_deref() {
        Ok("cpu") => Device::Cpu,
        _ => Device::Gpu,
    };
    let dir = PathBuf::from(dir);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let scanned = library_cache::scan(&dir, None, true, &cancelled, &|_| {}).unwrap().unwrap();
    let store = parking_lot::Mutex::new(Store::new(dir.clone()));
    let lib = Arc::new(library_index::index_from_tree(&dir, &scanned.tree, &store, &cancelled).unwrap());
    println!("library         {:>6} shots in {} days", lib.total_shots, lib.len());
    for d in lib.days.values() {
        println!("  {}  {:>5} shots  {:>5} files  ({} JPG, {} RAW)", d.date, d.shot_count, d.files.len(), d.jpg_count, d.raw_count);
    }
    let scratch = std::env::temp_dir().join(format!("pp-bench-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let (bus, rx) = bus::channel();
    let config = Config { factory: crate::ml::client::Client::factory(), installed: Box::new(|_| true), subjects_path: None, index_dir: Some(scratch.clone()) };
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
    let elapsed = ms(t);
    println!(
        "first index     {:>6} shots  {:>8.0} ms  ({:.1} ms per shot) on {} ({:?}, self-check {:?}); stopped: {:?}",
        status.indexed_shots,
        elapsed,
        elapsed / status.indexed_shots.max(1) as f64,
        device.label(),
        status.device.name,
        status.device.self_check,
        status.stopped
    );
    // Queries: the first pays the text helper's start, the rest are warm.
    let mut stream = Box::pin(bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    // Each query at the default closeness, then at the loosest setting, to
    // show the margin; the last repeats the first for the text cache.
    let queries = ["fursuit", "people dancing", "camera", "car", "dog", "food", "fireworks", "sunset", "crowd"];
    let runs: Vec<(&str, f32)> = queries
        .iter()
        .map(|q| (*q, crate::search::CLOSENESS_DEFAULT))
        .chain(queries.iter().map(|q| (*q, crate::search::CLOSENESS_MIN)))
        .chain(std::iter::once(("fursuit", crate::search::CLOSENESS_DEFAULT)))
        .collect();
    for (seq, (q, closeness)) in runs.into_iter().enumerate() {
        let t = Instant::now();
        engine.query(seq as u64, q, closeness, &[]);
        let hits = rt.block_on(async {
            use iced::futures::StreamExt;
            loop {
                let Some(batch) = stream.next().await else { return None };
                for ev in batch {
                    if let BgEvent::SearchResults { seq: s, hits, error, .. } = ev {
                        if s as usize == seq {
                            assert!(error.is_none(), "{q}: {error:?}");
                            return Some(hits.len());
                        }
                    }
                }
            }
        });
        println!("query {q:>16} at {closeness:<7}  {:>4} hits  {:>8.1} ms", hits.unwrap_or(0), ms(t));
    }
    engine.shutdown();
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&scratch);
}
