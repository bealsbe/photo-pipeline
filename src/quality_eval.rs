//! Evaluation of the quality checks on a real folder (ignored by default):
//!
//! ```text
//! PP_EVAL_DIR=<folder> cargo test --release -- --ignored --nocapture quality_eval::eval
//! ```
//!
//! Runs the analysis pass exactly as the app does (same jobs, threads and
//! decisions) and prints what it found and what it cost.  When the folder
//! has prune and keep sidecars, it scores the hints against them: a pruned
//! shot (P) is taken as soft or accidental, a kept one (M) as the best of
//! its burst.  This stands in for hand-labelling (Phase 0).
//!
//! Read-only for the folder.  `PP_EVAL_CACHE=<file>` uses (and updates) a
//! cache file there, and runs the pass twice, cold then warm; without it
//! nothing is written anywhere.  `PP_EVAL_THREADS` (default
//! `analysis::THREADS`); `PP_EVAL_CSV=<file>` writes one row per shot.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;

use crate::analysis::{self, Finished, Mode, Request};
use crate::bus::{self, BgEvent};
use crate::imaging::quality::Features;
use crate::model::bursts::Bursts;
use crate::model::collection::{Collection, RecordId};
use crate::model::hints::{Hints, Soft};
use crate::model::record::PhotoRecord;
use crate::model::sidecar;
use crate::scan;

pub fn scan_folder(dir: &Path) -> Vec<PhotoRecord> {
    let (bus, rx) = bus::channel();
    let _scan = scan::start(1, dir.to_path_buf(), bus, false);
    let mut stream = Box::pin(bus::stream(&rx));
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
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
    })
}

/// Bytes this process has read through read(2) so far (/proc/self/io).
fn bytes_read() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("rchar: ").and_then(|v| v.trim().parse().ok())))
        .unwrap_or(0)
}

struct Pass {
    features: HashMap<PathBuf, Features>,
    duplicates: Vec<Vec<PathBuf>>,
    fin: Finished,
    bytes: u64,
}

fn pass(coll: &Collection, root: &Path, cache: Option<PathBuf>, threads: usize) -> Pass {
    let files = analysis::jobs(coll, &[]);
    let req = Request { run: 1, root: root.to_path_buf(), files, mode: Mode::Full, threads, cache };
    let mut features = HashMap::new();
    let mut duplicates = Vec::new();
    let before = bytes_read();
    let fin = analysis::execute(req, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |b| {
        for (p, f) in &b.features {
            features.insert(p.clone(), f.as_ref().clone());
        }
        if let Some(d) = &b.duplicates {
            duplicates = d.clone();
        }
    });
    Pass { features, duplicates, fin, bytes: bytes_read() - before }
}

fn pct(n: usize, of: usize) -> String {
    if of == 0 { "-".into() } else { format!("{:.0} %", 100.0 * n as f64 / of as f64) }
}

#[test]
#[ignore]
fn eval() {
    let Ok(dir) = std::env::var("PP_EVAL_DIR") else { return };
    let dir = PathBuf::from(dir);
    let threads = std::env::var("PP_EVAL_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(analysis::THREADS);
    let cache = std::env::var_os("PP_EVAL_CACHE").map(PathBuf::from);
    let t = Instant::now();
    let mut coll = Collection::new();
    for r in scan_folder(&dir) {
        coll.add(r);
    }
    coll.build_pairs();
    let scan_ms = t.elapsed().as_secs_f64() * 1000.0;
    // The user's decisions, read-only.
    let pruned = sidecar::read_prune_marks(&dir).unwrap_or_default();
    let marked = sidecar::read_export_marks(&dir).unwrap_or_default();
    let ids: Vec<RecordId> = coll.ids().to_vec();
    for id in ids {
        let r = coll.get_mut(id).unwrap();
        r.is_pruned = pruned.contains(&r.path);
        r.is_marked = marked.contains(&r.path);
    }
    let bursts = Bursts::find(&coll);
    println!("{}: {} files, scanned in {scan_ms:.0} ms; {} bursts of {} frames", dir.display(), coll.len(), bursts.len(), bursts.frame_count());

    let cold = pass(&coll, &dir, cache.clone(), threads);
    let shots = cold.fin.measured + cold.fin.cached + cold.fin.failed;
    let secs = cold.fin.elapsed.as_secs_f64();
    println!(
        "pass ({threads} threads): {} shots in {:.2} s = {:.2} ms/shot wall; {} measured, {} cached, {} failed; {:.2} MB read ({:.0} KB/shot); duplicate check read {:.1} MB in {:.2} s",
        shots,
        secs,
        secs * 1000.0 / shots.max(1) as f64,
        cold.fin.measured,
        cold.fin.cached,
        cold.fin.failed,
        cold.bytes as f64 / 1e6,
        cold.bytes as f64 / 1e3 / shots.max(1) as f64,
        cold.fin.hashed_bytes as f64 / 1e6,
        cold.fin.hashing.as_secs_f64(),
    );
    if cache.is_some() {
        let warm = pass(&coll, &dir, cache.clone(), threads);
        println!("warm pass: {:.0} ms, {} cached, {} measured", warm.fin.elapsed.as_secs_f64() * 1000.0, warm.fin.cached, warm.fin.measured);
    }

    let t = Instant::now();
    let hints = Hints::derive(&coll, &bursts, |_, r| cold.features.get(&r.path), &cold.duplicates, 1);
    let derive_ms = t.elapsed().as_secs_f64() * 1000.0;
    let s = hints.summary();
    let (mut abs, mut rel) = (0, 0);
    let mut accidents: HashMap<&str, usize> = HashMap::new();
    let mut flagged: Vec<(String, String)> = Vec::new();
    for (id, r) in coll.iter_ids() {
        if hints.rep(id) != Some(id) {
            continue;
        }
        let Some(h) = hints.get(id) else { continue };
        match h.soft {
            Some(Soft::Absolute) => abs += 1,
            Some(Soft::Relative) => rel += 1,
            None => {}
        }
        if let Some(a) = h.accidental {
            *accidents.entry(a.label()).or_default() += 1;
        }
        if h.problem() {
            let why = match (h.soft, h.accidental) {
                (_, Some(a)) => format!("accidental ({})", a.label()),
                (Some(Soft::Absolute), _) => "soft: nothing sharp".into(),
                _ => format!("soft: {:.2} of {}", h.relative, h.best_alike.and_then(|b| coll.get(b)).map(|b| b.filename()).unwrap_or_default()),
            };
            flagged.push((r.filename(), format!("{why}; local {:.1} score {:.1}", h.local, h.score)));
        }
    }
    flagged.sort();
    println!(
        "hints in {derive_ms:.1} ms: {} measured; soft {} ({abs} nothing sharp, {rel} far softer than a look-alike); accidental {} {accidents:?}",
        s.measured, s.soft, s.accidental
    );
    println!(
        "moments {} ({} merged from more than one press; {} presses of 2+ before merging); duplicate groups {} ({} copies to prune)",
        s.moments,
        s.merged,
        bursts.len(),
        s.duplicate_groups,
        s.duplicate_copies
    );
    for (n, why) in &flagged {
        println!("  flagged {n}: {why}");
    }
    for d in hints.duplicates().iter().take(20) {
        let n = |id: RecordId| coll.get(id).map(|r| r.filename()).unwrap_or_default();
        println!("  duplicate {:?}: keep {}, prune {:?}", d.kind, n(d.keep), d.prune.iter().map(|&i| n(i)).collect::<Vec<_>>());
    }

    // Agreement with the user's marks.
    let decided = |id: RecordId, f: fn(&PhotoRecord) -> bool| {
        coll.get(id).is_some_and(f) || coll.find_pair(id).and_then(|p| coll.get(p)).is_some_and(f)
    };
    if !pruned.is_empty() || !marked.is_empty() {
        let reps: Vec<RecordId> = coll.ids().iter().copied().filter(|&id| hints.rep(id) == Some(id) && hints.get(id).is_some()).collect();
        let p: Vec<RecordId> = reps.iter().copied().filter(|&id| decided(id, |r| r.is_pruned)).collect();
        let m: HashSet<RecordId> = reps.iter().copied().filter(|&id| decided(id, |r| r.is_marked)).collect();
        let fl: Vec<RecordId> = reps.iter().copied().filter(|&id| hints.get(id).is_some_and(|h| h.problem())).collect();
        let fl_p = fl.iter().filter(|id| p.contains(id)).count();
        let fl_m = fl.iter().filter(|id| m.contains(id)).count();
        println!(
            "marks: {} shots pruned, {} kept.  Flagged {}: {} pruned by the user, {} kept by the user ({} of flags), {} undecided; pruned shots flagged: {}",
            p.len(),
            m.len(),
            fl.len(),
            fl_p,
            fl_m,
            pct(fl_m, fl.len()),
            fl.len() - fl_p - fl_m,
            pct(fl_p, p.len())
        );
        // Sharpest-in-group against the kept frames, per press and moment.
        for (label, groups) in [
            ("presses", (0..bursts.len() as u32).map(|b| bursts.frames(b).to_vec()).collect::<Vec<_>>()),
            ("moments", hints.moments().iter().map(|mo| mo.frames.clone()).collect()),
        ] {
            let (mut n, mut t1, mut t2, mut chance) = (0, 0, 0, 0.0);
            for g in groups {
                let g: Vec<RecordId> = g.into_iter().filter(|&id| hints.get(id).is_some()).collect();
                let kept = g.iter().filter(|id| m.contains(id)).count();
                if g.len() < 2 || kept == 0 || kept == g.len() {
                    continue;
                }
                let ranked = hints.ranked(&g);
                n += 1;
                t1 += m.contains(&ranked[0]) as usize;
                t2 += (m.contains(&ranked[0]) || m.contains(&ranked[1])) as usize;
                chance += kept as f64 / g.len() as f64;
            }
            println!(
                "{label} with kept and unkept frames: {n}; the sharpest is kept in {} (chance {}), top two {}",
                pct(t1, n),
                if n > 0 { format!("{:.0} %", 100.0 * chance / n as f64) } else { "-".into() },
                pct(t2, n)
            );
        }
    }

    if let Ok(out) = std::env::var("PP_EVAL_CSV") {
        let mut f = std::fs::File::create(&out).unwrap();
        writeln!(f, "name,ms,burst,moment,pruned,marked,orient,global,local,score,rel,soft,accidental,sharpest,rank,focus,entropy,p1,p50,p99,phash,tiles,ctiles,by_levels,lit").unwrap();
        for (id, r) in coll.iter_ids() {
            if hints.rep(id) != Some(id) {
                continue;
            }
            let (Some(h), Some(ft)) = (hints.get(id), cold.features.get(&r.path)) else { continue };
            let ms = r.capture_time.map_or(0, |c| c.and_utc().timestamp_millis() + r.shot.subsec_ms.unwrap_or(0) as i64);
            writeln!(
                f,
                "{},{ms},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{},{},{},{},{:.3},{:.3},{},{},{},{:016x},{},{},{},{}",
                r.filename(),
                bursts.of(id).map_or(-1, |b| b as i64),
                hints.moment_of(id).map_or(-1, |m| m as i64),
                decided(id, |r| r.is_pruned) as u8,
                decided(id, |r| r.is_marked) as u8,
                ft.orientation,
                ft.global,
                h.local,
                h.score,
                h.relative,
                h.soft.map_or("", |s| if s == Soft::Absolute { "abs" } else { "rel" }),
                h.accidental.map_or(String::new(), |a| a.label().replace(',', ";")),
                h.sharpest as u8,
                h.rank.map_or(0, |r| r.0),
                ft.focus.unwrap_or(-1.0),
                ft.entropy,
                ft.p1(),
                ft.p50(),
                ft.p99(),
                ft.phash,
                ft.tiles.iter().map(|&t| format!("{:.1}", crate::imaging::quality::tile_value(t))).collect::<Vec<_>>().join(" "),
                ft.clipped.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" "),
                crate::model::hints::accident_by_levels(ft).map_or(String::new(), |a| a.label().replace(',', ";")),
                crate::model::hints::lit_tiles(ft),
            )
            .unwrap();
        }
        println!("wrote {out}");
    }
}

/// Where a shot's time goes, one thread: reading, decoding, measuring.
#[test]
#[ignore]
fn costs() {
    let Ok(dir) = std::env::var("PP_EVAL_DIR") else { return };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg"))).collect();
    files.sort();
    files.truncate(200);
    let (mut read, mut luma, mut rgb, mut meas) = (0.0, 0.0, 0.0, 0.0);
    for p in &files {
        let t = Instant::now();
        let Some((data, head)) = crate::exif::jpeg_preview_bytes(p, 1000) else { continue };
        read += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let px = crate::imaging::decode::decode_jpeg_luma(&data, 1).unwrap();
        luma += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let _ = crate::imaging::decode::decode_jpeg_full(&data).unwrap();
        rgb += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let f = crate::imaging::quality::measure(crate::imaging::quality::Plane::of(&px), crate::exif::orientation_from_jpeg_head(&head), Some((0.5, 0.5)), crate::imaging::quality::Source::Mpf);
        meas += t.elapsed().as_secs_f64();
        std::hint::black_box(f);
    }
    let n = files.len() as f64 / 1000.0;
    println!("{} previews: read {:.2} ms, luma decode {:.2} ms (RGB {:.2} ms), measure {:.2} ms per shot", files.len(), read / n, luma / n, rgb / n, meas / n);
}

/// What working the hints out costs on the UI thread for a big shoot:
/// 10 000 files (5 000 RAW+JPG shots in bursts of 8), all measured, as
/// `app::quality::refresh` does it (canonical keys and all).
#[test]
#[ignore]
fn derive_cost() {
    use crate::model::record::{FileType, MTime, ShotMeta};
    let base = chrono::NaiveDateTime::parse_from_str("2026-09-19 10:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
    let mut coll = Collection::new();
    let f = crate::imaging::quality::tests::feats(&crate::imaging::quality::tests::texture(192, 128, 1));
    let mut features: HashMap<PathBuf, std::sync::Arc<Features>> = HashMap::new();
    for i in 0..5000u32 {
        let ms = (i / 8) as i64 * 5000 + (i % 8) as i64 * 100;
        for (dir, ext, ft) in [("JPG", "JPG", FileType::Jpg), ("RAW", "ARW", FileType::Raw)] {
            let p = PathBuf::from(format!("/shoot/{dir}/DSC{i:05}.{ext}"));
            let t = base + chrono::Duration::milliseconds(ms - ms % 1000);
            let mut r = PhotoRecord::new(p.clone(), ft, 1000 + i as u64, MTime::from_unix(1_700_000_000, 0), Some(t));
            r.shot = ShotMeta { subsec_ms: Some((ms % 1000) as u16), sequence: Some((i % 8 + 1) as u16), focus: None, camera: 1 };
            if ft == FileType::Jpg {
                let mut g = f.clone();
                g.global = 10.0 + (i % 13) as f32;
                features.insert(crate::app::quality::canonical_key(&p), std::sync::Arc::new(g));
            }
            coll.add(r);
        }
    }
    coll.build_pairs();
    let bursts = Bursts::find(&coll);
    let t = Instant::now();
    let by_key: HashMap<PathBuf, &Path> = coll.iter().map(|r| (crate::app::quality::canonical_key(&r.path), r.path.as_path())).collect();
    let keys_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let hints = Hints::derive(&coll, &bursts, |_, r| features.get(&crate::app::quality::canonical_key(&r.path)).map(|f| f.as_ref()), &[], 1);
    let derive_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("{} files, {} bursts: key map {keys_ms:.1} ms, derive {derive_ms:.1} ms; {:?}", coll.len(), bursts.len(), hints.summary());
    assert_eq!(by_key.len(), coll.len());
}
