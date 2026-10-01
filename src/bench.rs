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
