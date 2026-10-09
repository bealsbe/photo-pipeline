//! The fursuit-head step as the pages see it (decision 11, plan §5.2 and
//! §7a.4–7a.5): on or off, how far it has got, what it waits for, and
//! what it costs on each device.
//!
//! - **Semi-automatic, scanned during indexing.**  Heads are looked for on
//!   the photos the index tags as fursuits, as a step after search (and
//!   faces); the user names a fursuit from one group and confirms
//!   suggestions with Y and N.
//! - **On the processor it asks, every time.**  A head scan is long there
//!   (about 15 hours for the library, over an hour an event day), so with
//!   *Processor* chosen no scan starts on its own: each one waits as an
//!   [`Offer`] with its cost until the user starts or skips it.  On the
//!   graphics card it starts unasked.
//! - **A card that fails says so.**  The step stops ([`Step::Failed`]) and
//!   the banner says what failed; nothing moves to the processor.
//!
//! Three parts: the **status** the pages read (`Status::heads`); the
//! **head cache** beside the index (detections per photo, DINOv2 vectors
//! per head, a tile per head; files readable by the user alone, never in
//! the library); and the **scan** itself, a runner over the helper's
//! `find_heads` with the pass's cancel and card rules, driven by
//! [`Fursuits`], the state the engine's thread owns.  Grouping, naming,
//! suggestions and the review are the generic subjects machinery over
//! this kind's rows (`search::step`); nothing here is shared with faces:
//! its own files, its own refs, its own vectors of its own width.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::index::{Key, blocks, framed};
use super::query::{f16_to_f32, f32_to_f16};
use super::tags::Tags;
use super::{Stopped, estimate};
use crate::ml::client::{Embedder, FileError, HelperError};
use crate::ml::heads::{Head, PhotoHeads};
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device};
use crate::model::binfile::{Reader, Writer};
use crate::model::library_index::LibraryIndex;
use crate::model::record::FileType;
use crate::model::subjects::{self, Cutoffs, Face, FaceRef, GroupId, Kind, Refused, Rows, SubjectsFile, private_dir, private_file};
use crate::ops::fsutil;
use super::step::{self, Grouped, KindStep, RegroupJob, Regrouped};

/// Seconds per fursuit photo for the head step (OWLv2 to find heads, then
/// DINOv2 on each head): the study's 0.12 s on the card (about 1.4 hours
/// for the library's ~42 k fursuit photos) and 1.3 s on the processor
/// (about 15 hours), plan §3.7.
/// Fursuit photos in a busy event day (sample E2's day held ~3 500).
pub const EVENT_DAY_PHOTOS: usize = 3_500;

/// Seconds to look for heads in `photos` fursuit photos on `device`.
pub fn head_seconds(photos: usize, device: Device) -> f64 {
    estimate::head_catch_up_seconds(photos, device)
}

/// Bytes read from the library to look at `photos` again (each preview).
pub fn head_bytes_read(photos: usize) -> u64 {
    estimate::bytes_read(photos)
}

/// "about 1.4 hours with the graphics card", "about 15 hours on the
/// processor".
pub fn describe_cost(photos: usize, device: Device) -> String {
    let time = estimate::describe_seconds(head_seconds(photos, device));
    format!("{time} {} {}", if device == Device::Gpu { "with" } else { "on" }, device.phrase())
}

/// Why a head scan would start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The first index reached its fursuit-head step.
    FirstIndex,
    /// An export added photos with fursuits.
    NewExports,
    /// The library was indexed before heads were looked for, or scans
    /// were skipped (asked from the Fursuits tab or Preferences).
    CatchUp,
    /// Carrying on after a stop (the library came back, the card works).
    Resume,
    /// Heads found before fursuits were told apart by the search model
    /// get its vectors: the known boxes, re-read (asked for, never
    /// automatic).  PROVISIONAL until the backend's stage 1 lands.
    Vectors,
}

/// A head scan waiting for the user's answer (the processor is chosen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// Fursuit photos it would look at.
    pub photos: usize,
    pub why: Why,
}

/// Where the head step is.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum Step {
    /// Nothing running and nothing waiting.
    #[default]
    Idle,
    /// A scan would start, and waits for start or skip (the processor).
    Asking(Offer),
    /// Looking for heads: `done` of `total` fursuit photos.
    Scanning { why: Why, done: usize, total: usize },
    /// The card failed: the step is stopped, and says why.
    Failed(CardFailure),
}

/// Fursuits, as the tab, Preferences and the status bar show them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HeadsStatus {
    /// Preferences ▸ *Find fursuit heads*.
    pub on: bool,
    /// OWLv2 and DINOv2 are in place.
    pub models_installed: bool,
    /// Indexed photos the index tags as fursuits, and how many of them were
    /// looked at for heads.
    pub tagged: usize,
    pub scanned: usize,
    /// Photos of scans the user skipped, waiting for a later catch-up.
    pub skipped: usize,
    /// Heads found, and fursuits named.
    pub found: usize,
    pub named: usize,
    pub step: Step,
    /// Photos with heads but no search-model head vectors yet (found
    /// before fursuit matching v2): the catch-up offers them, with its
    /// cost.  PROVISIONAL until the backend's stage 1 lands.
    pub vectors_missing: usize,
}

impl HeadsStatus {
    /// Fursuit photos not looked at for heads (indexed before, or skipped):
    /// the tab and Preferences offer them, with their cost.
    pub fn unscanned(&self) -> usize {
        self.tagged.saturating_sub(self.scanned)
    }

    /// The tab offers to look (and says what it costs) instead of running.
    pub fn catch_up_wanted(&self) -> bool {
        self.on && self.unscanned() > 0 && matches!(self.step, Step::Idle)
    }

    pub fn scanning(&self) -> bool {
        matches!(self.step, Step::Scanning { .. })
    }

    pub fn asking(&self) -> Option<Offer> {
        match self.step {
            Step::Asking(o) => Some(o),
            _ => None,
        }
    }
}

/// The fursuit photos a head scan would look at: those indexed and not
/// looked at yet; before anything is indexed, the study's share of the
/// library (about three shots in four showed fursuits).
pub fn photos_to_look_at(h: &HeadsStatus, shots: usize) -> usize {
    if h.tagged > 0 { h.unscanned() } else { shots * 3 / 4 }
}

/// Whether a head scan may start without asking: only on the graphics
/// card.  On the processor every scan asks first, with its cost (decision
/// 11, "each time").
pub fn starts_unasked(device: Device) -> bool {
    device == Device::Gpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_costs_match_the_measurements() {
        // The library's fursuit photos, with the helper's own graphs
        // measured (plan, "As built — phase 4"): about 3.5 hours on the
        // card, about two days on the processor; an event day a quarter
        // of an hour on the card, over four hours on the processor.
        let n = 42_180;
        let gpu = head_seconds(n, Device::Gpu) / 3600.0;
        let cpu = head_seconds(n, Device::Cpu) / 3600.0;
        assert!((3.0..4.0).contains(&gpu), "{gpu} h on the card");
        assert!((50.0..55.0).contains(&cpu), "{cpu} h on the processor");
        assert!((10.0 * 60.0..20.0 * 60.0).contains(&head_seconds(EVENT_DAY_PHOTOS, Device::Gpu)), "a quarter of an hour an event day on the card");
        assert!(head_seconds(EVENT_DAY_PHOTOS, Device::Cpu) > 4.0 * 3600.0, "over four hours a day on the processor");
        assert!(describe_cost(n, Device::Cpu).ends_with(" on the processor"), "{}", describe_cost(n, Device::Cpu));
        assert_eq!(describe_cost(n, Device::Gpu), format!("{} with the graphics card", estimate::describe_seconds(head_seconds(n, Device::Gpu))));
        assert_eq!(estimate::describe_bytes(head_bytes_read(n)), "about 15 GB");
        // Before indexing, three shots in four.
        assert_eq!(photos_to_look_at(&HeadsStatus::default(), 56_710), 42_532);
    }

    #[test]
    fn only_the_card_starts_a_scan_unasked() {
        assert!(starts_unasked(Device::Gpu));
        assert!(!starts_unasked(Device::Cpu), "the processor asks every time");
    }

    #[test]
    fn unscanned_photos_are_offered_only_when_nothing_runs_or_waits() {
        let mut s = HeadsStatus { on: true, tagged: 400, scanned: 100, ..HeadsStatus::default() };
        assert_eq!(s.unscanned(), 300);
        assert_eq!(photos_to_look_at(&s, 10_000), 300);
        assert!(s.catch_up_wanted());
        s.step = Step::Scanning { why: Why::CatchUp, done: 10, total: 300 };
        assert!(!s.catch_up_wanted() && s.scanning());
        s.step = Step::Asking(Offer { photos: 300, why: Why::NewExports });
        assert!(!s.catch_up_wanted());
        assert_eq!(s.asking(), Some(Offer { photos: 300, why: Why::NewExports }));
        s.step = Step::Failed(CardFailure::Crashed);
        assert!(!s.catch_up_wanted() && !s.scanning());
        s.step = Step::Idle;
        s.on = false;
        assert!(!s.catch_up_wanted(), "off: nothing offered");
    }
}

// ---- the head cache ---------------------------------------------------

const HEADS_MAGIC: &[u8; 8] = b"PPHEADD\0";
const HEADS_VERSION: u32 = 1;
const HVECS_MAGIC: &[u8; 8] = b"PPHEADV\0";
const HVECS_VERSION: u32 = 1;
const HTILE_MAGIC: u32 = 0x5050_4854;
/// OWLv2 and the decode rule the detections were made with (the prompt,
/// the score floor, the overlap and size gates); a change means the
/// photos are looked at again.
pub const DETECTOR_VERSION: u32 = 1;
/// The vector model: one, not a choice (decision 11).
pub const VECTOR_MODEL: ModelId = ModelId::Dinov2;
/// Dropped entries beyond this share get the files rewritten on load.
const COMPACT_SHARE: f64 = 0.1;
/// The word in the vocabulary that gates the step: a photo is looked at
/// for heads only when the index tags it with it (decision 8, 11).
pub const GATE_WORD: &str = "fursuit";

// The cut-offs on DINOv2 cosine distance between two heads live in
// `subjects::cutoff_for(ModelId::Dinov2)`, measured on the user's 258
// pairwise answers (plan, "As built — phase 4"): groups and *Find more*
// at 0.14, suggestions for the review at 0.17, nothing beyond.

/// A head as the detector found it, kept per photo.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadDetection {
    pub bbox: [f32; 4],
    pub score: f32,
    pub size_px: f32,
    pub sharpness: f32,
}

impl HeadDetection {
    pub fn head_ref(&self, key: Key) -> FaceRef {
        FaceRef::new(key, self.bbox)
    }

    /// How clear a view this is, before the grouping adds how sure the
    /// match is (UI 12): size (a 100-px head is the floor, 400 px plenty),
    /// sharpness and the detector's score.
    pub fn clarity(&self) -> f32 {
        let size = ((self.size_px - 100.0) / 300.0).clamp(0.0, 1.0);
        let sharp = (self.sharpness * 4.0).clamp(0.0, 1.0);
        let score = ((self.score - 0.3) / 0.4).clamp(0.0, 1.0);
        0.5 * size + 0.3 * sharp + 0.2 * score
    }
}

/// The head vectors in memory: unit rows of the vector model's width,
/// f32 for the grouping (f16 on disk).
#[derive(Default)]
pub struct HeadVectors {
    pub dim: usize,
    pub refs: Vec<FaceRef>,
    pub by_ref: HashMap<FaceRef, usize>,
    /// `refs.len() × dim`.
    pub data: Vec<f32>,
}

impl HeadVectors {
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    fn push(&mut self, rows: &[(FaceRef, Vec<f32>)]) -> usize {
        let mut added = 0;
        for (r, v) in rows {
            if v.is_empty() || self.by_ref.contains_key(r) {
                continue;
            }
            if self.dim == 0 {
                self.dim = v.len();
            }
            if v.len() != self.dim {
                continue;
            }
            self.by_ref.insert(*r, self.refs.len());
            self.refs.push(*r);
            self.data.extend_from_slice(v);
            added += 1;
        }
        added
    }

    fn block(rows: &[(FaceRef, Vec<f32>)]) -> Vec<u8> {
        let dim = rows.first().map_or(0, |(_, v)| v.len());
        let mut w = Writer(Vec::with_capacity(16 + rows.len() * (24 + dim * 2)));
        w.u32(rows.len() as u32);
        for (r, v) in rows {
            w.0.extend_from_slice(&r.key.0);
            for t in r.thousandths() {
                w.u16(t);
            }
            for &x in v {
                w.u16(f32_to_f16(x));
            }
        }
        w.seal()
    }

    fn read_block(data: &[u8], dim: usize) -> Option<Vec<(FaceRef, Vec<f32>)>> {
        let mut r = Reader::open(data)?;
        let n = r.count(16 + 8 + dim * 2)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let key = Key(r.take(16)?.try_into().ok()?);
            let mut t = [0u16; 4];
            for x in &mut t {
                *x = r.u16()?;
            }
            let bbox = t.map(|v| v as f32 / 1000.0);
            let mut v = Vec::with_capacity(dim);
            for _ in 0..dim {
                v.push(f16_to_f32(r.u16()?));
            }
            out.push((FaceRef::new(key, bbox), v));
        }
        r.done().then_some(out)
    }
}

/// Where each head's tile lies in `headtiles.bin`, for reading one at a
/// time from any thread (`Engine::crop`).
#[derive(Default)]
pub struct HeadTiles {
    path: PathBuf,
    offsets: HashMap<FaceRef, (u64, u32)>,
}

impl HeadTiles {
    /// A head's tile (JPEG bytes), read from the file.
    pub fn read(&self, r: &FaceRef) -> Option<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        let &(offset, len) = self.offsets.get(r)?;
        let f = fs::File::open(&self.path).ok()?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact_at(&mut buf, offset).ok()?;
        Some(buf)
    }

    fn entry(r: &FaceRef, jpeg: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32 + jpeg.len());
        buf.extend_from_slice(&HTILE_MAGIC.to_le_bytes());
        buf.extend_from_slice(&r.key.0);
        for t in r.thousandths() {
            buf.extend_from_slice(&t.to_le_bytes());
        }
        buf.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
        buf.extend_from_slice(jpeg);
        buf
    }
}

/// The head cache of one library: detections per photo looked at, a
/// vector per head, a tile per head.  The vectors (1 536 wide, f32 in
/// memory: 405 MB measured for the ~68 000 heads a library of 56 000
/// shots is expected to hold) are read only when the fursuit work asks
/// for them ([`HeadStore::vectors`]), never when the library opens, and
/// are let go again once that work is quiet ([`Fursuits::release_if_idle`];
/// the user's choice: full size, freed after use).
pub struct HeadStore {
    dir: PathBuf,
    /// Photos looked at, with the heads found in each (maybe none).
    pub scanned: HashMap<Key, Vec<HeadDetection>>,
    /// In memory only once asked for ([`HeadStore::vectors`]).
    pub(crate) vectors: Option<HeadVectors>,
    tiles: Arc<Mutex<HeadTiles>>,
}

impl HeadStore {
    fn heads_file(dir: &Path) -> PathBuf {
        dir.join("heads.bin")
    }

    fn vectors_file(dir: &Path) -> PathBuf {
        dir.join(format!("headvecs-{}.bin", VECTOR_MODEL.dir_name()))
    }

    fn tiles_file(dir: &Path) -> PathBuf {
        dir.join("headtiles.bin")
    }

    /// Load the cache in `dir` (empty where there is nothing, or files of
    /// another detector rule or model).
    pub fn load(dir: &Path) -> HeadStore {
        let mut store = HeadStore { dir: dir.to_path_buf(), scanned: HashMap::new(), vectors: None, tiles: Arc::new(Mutex::new(HeadTiles { path: Self::tiles_file(dir), offsets: HashMap::new() })) };
        if let Ok(data) = fs::read(Self::heads_file(dir)) {
            let bs = blocks(&data);
            if bs.first().is_some_and(|h| Self::read_heads_header(h)) {
                for b in &bs[1..] {
                    let Some(entries) = Self::read_heads_block(b) else { break };
                    for (k, d) in entries {
                        store.scanned.insert(k, d);
                    }
                }
            }
        }
        store.load_tiles();
        store
    }

    /// The vectors, read from the file the first time they are asked for
    /// (the memory is built only when the fursuit work needs it).
    pub fn vectors(&mut self) -> &HeadVectors {
        if self.vectors.is_none() {
            self.vectors = Some(Self::read_vectors(&self.dir));
        }
        self.vectors.as_ref().expect("just loaded")
    }

    /// Whether the vectors are in memory.
    pub fn vectors_loaded(&self) -> bool {
        self.vectors.is_some()
    }

    /// Let the vectors go (read again from the file when next asked for,
    /// about 0.3 s at library scale).
    pub fn release_vectors(&mut self) {
        self.vectors = None;
    }

    fn read_vectors(dir: &Path) -> HeadVectors {
        let mut v = HeadVectors::default();
        let Ok(data) = fs::read(Self::vectors_file(dir)) else { return v };
        let bs = blocks(&data);
        let Some(dim) = bs.first().and_then(|h| Self::read_vectors_header(h)) else { return v };
        v.dim = dim;
        for b in &bs[1..] {
            let Some(rows) = HeadVectors::read_block(b, dim) else { break };
            v.push(&rows);
        }
        v
    }

    fn load_tiles(&mut self) {
        let mut index = self.tiles.lock();
        index.offsets.clear();
        let Ok(data) = fs::read(&index.path) else { return };
        let mut i = 0usize;
        while i + 32 <= data.len() {
            let magic = u32::from_le_bytes(data[i..i + 4].try_into().expect("4 bytes"));
            if magic != HTILE_MAGIC {
                break;
            }
            let key = Key(data[i + 4..i + 20].try_into().expect("16 bytes"));
            let mut t = [0u16; 4];
            for (j, x) in t.iter_mut().enumerate() {
                *x = u16::from_le_bytes(data[i + 20 + 2 * j..i + 22 + 2 * j].try_into().expect("2 bytes"));
            }
            let len = u32::from_le_bytes(data[i + 28..i + 32].try_into().expect("4 bytes"));
            let start = i + 32;
            let Some(end) = start.checked_add(len as usize).filter(|&e| e <= data.len()) else { break };
            index.offsets.insert(FaceRef::new(key, t.map(|v| v as f32 / 1000.0)), (start as u64, len));
            i = end;
        }
    }

    fn heads_header() -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(HEADS_MAGIC);
        w.u32(HEADS_VERSION);
        w.u32(DETECTOR_VERSION);
        w.seal()
    }

    fn read_heads_header(data: &[u8]) -> bool {
        let Some(mut r) = Reader::open(data) else { return false };
        r.take(8) == Some(HEADS_MAGIC) && r.u32() == Some(HEADS_VERSION) && r.u32() == Some(DETECTOR_VERSION) && r.done()
    }

    fn heads_block(entries: &[(Key, Vec<HeadDetection>)]) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u32(entries.len() as u32);
        for (k, dets) in entries {
            w.0.extend_from_slice(&k.0);
            w.u16(dets.len() as u16);
            for d in dets {
                for v in d.bbox {
                    w.u32(v.to_bits());
                }
                w.u32(d.score.to_bits());
                w.u32(d.size_px.to_bits());
                w.u32(d.sharpness.to_bits());
            }
        }
        w.seal()
    }

    fn read_heads_block(data: &[u8]) -> Option<Vec<(Key, Vec<HeadDetection>)>> {
        let mut r = Reader::open(data)?;
        let n = r.count(18)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let key = Key(r.take(16)?.try_into().ok()?);
            let m = r.u16()? as usize;
            let mut dets = Vec::with_capacity(m);
            for _ in 0..m {
                let f = |r: &mut Reader<'_>| r.u32().map(f32::from_bits);
                let bbox = [f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?];
                dets.push(HeadDetection { bbox, score: f(&mut r)?, size_px: f(&mut r)?, sharpness: f(&mut r)? });
            }
            out.push((key, dets));
        }
        r.done().then_some(out)
    }

    fn vectors_header(dim: usize) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(HVECS_MAGIC);
        w.u32(HVECS_VERSION);
        w.bytes(VECTOR_MODEL.dir_name().as_bytes());
        w.bytes(models::info(VECTOR_MODEL).weights_sha256().as_bytes());
        w.u32(dim as u32);
        w.seal()
    }

    fn read_vectors_header(data: &[u8]) -> Option<usize> {
        let mut r = Reader::open(data)?;
        if r.take(8)? != HVECS_MAGIC || r.u32()? != HVECS_VERSION {
            return None;
        }
        if r.bytes()? != VECTOR_MODEL.dir_name().as_bytes() || r.bytes()? != models::info(VECTOR_MODEL).weights_sha256().as_bytes() {
            return None;
        }
        let dim = r.u32()? as usize;
        (r.done() && dim > 0).then_some(dim)
    }

    /// Append framed blocks to a file (made private), writing `header`
    /// first when the file is new or empty.
    fn append(path: &Path, header: &[u8], block: &[u8]) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            private_dir(dir);
        }
        let fresh = fs::metadata(path).map_or(true, |m| m.len() == 0);
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
        private_file(path);
        if fresh {
            f.write_all(&framed(header))?;
        }
        f.write_all(&framed(block))?;
        f.sync_data()
    }

    /// Record a photo looked at, with the heads found (maybe none): the
    /// detections, their vectors and their tiles.
    pub fn add(&mut self, key: Key, heads: &[Head]) -> io::Result<()> {
        let dets: Vec<HeadDetection> = heads.iter().map(|h| HeadDetection { bbox: h.bbox, score: h.score, size_px: h.size as f32, sharpness: h.sharpness }).collect();
        Self::append(&Self::heads_file(&self.dir), &Self::heads_header(), &Self::heads_block(&[(key, dets.clone())]))?;
        self.scanned.insert(key, dets);
        // New rows go to the file; into memory too only while the
        // vectors are loaded (else they are read with the rest later).
        let rows: Vec<(FaceRef, Vec<f32>)> = heads.iter().filter(|h| !h.vector.is_empty()).map(|h| (FaceRef::new(key, h.bbox), h.vector.clone())).filter(|(r, _)| !self.vectors.as_ref().is_some_and(|v| v.by_ref.contains_key(r))).collect();
        if !rows.is_empty() {
            let dim = rows[0].1.len();
            Self::append(&Self::vectors_file(&self.dir), &Self::vectors_header(dim), &HeadVectors::block(&rows))?;
            if let Some(v) = &mut self.vectors {
                v.push(&rows);
            }
        }
        self.add_tiles(key, heads)
    }

    fn add_tiles(&mut self, key: Key, heads: &[Head]) -> io::Result<()> {
        let mut index = self.tiles.lock();
        let new: Vec<(FaceRef, &[u8])> = heads.iter().filter(|h| !h.tile.is_empty()).map(|h| (FaceRef::new(key, h.bbox), h.tile.as_slice())).filter(|(r, _)| !index.offsets.contains_key(r)).collect();
        if new.is_empty() {
            return Ok(());
        }
        if let Some(dir) = index.path.parent() {
            fs::create_dir_all(dir)?;
            private_dir(dir);
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&index.path)?;
        private_file(&index.path);
        let mut at = f.metadata()?.len();
        for (r, jpeg) in new {
            let buf = HeadTiles::entry(&r, jpeg);
            f.write_all(&buf)?;
            index.offsets.insert(r, (at + 32, jpeg.len() as u32));
            at += buf.len() as u64;
        }
        f.sync_data()
    }

    pub fn heads_of(&self, key: &Key) -> &[HeadDetection] {
        self.scanned.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn detection(&self, r: &FaceRef) -> Option<&HeadDetection> {
        self.heads_of(&r.key).iter().find(|d| d.head_ref(r.key) == *r)
    }

    /// Photos looked at.
    pub fn looked_at(&self) -> usize {
        self.scanned.len()
    }

    /// Heads found.
    pub fn found(&self) -> usize {
        self.scanned.values().map(Vec::len).sum()
    }

    /// Every head, as (ref, detection).
    pub fn all(&self) -> impl Iterator<Item = (FaceRef, &HeadDetection)> {
        self.scanned.iter().flat_map(|(k, dets)| dets.iter().map(move |d| (d.head_ref(*k), d)))
    }

    /// The tile index, shared with `Engine::crop` on other threads.
    pub fn tiles(&self) -> Arc<Mutex<HeadTiles>> {
        self.tiles.clone()
    }

    /// Keep only the photos in `live` (the library's), rewriting the files
    /// when many went.
    pub fn retain(&mut self, live: &HashSet<Key>) {
        let before = self.scanned.len();
        self.scanned.retain(|k, _| live.contains(k));
        let dropped = before - self.scanned.len();
        if dropped > 0 && dropped as f64 > COMPACT_SHARE * before as f64 {
            if let Err(e) = self.rewrite() {
                eprintln!("heads: couldn't compact the head cache: {e}");
            }
        }
    }

    /// Rewrite the three files from memory.
    fn rewrite(&mut self) -> io::Result<()> {
        let dir = self.dir.clone();
        fs::create_dir_all(&dir)?;
        private_dir(&dir);
        let mut out = framed(&Self::heads_header());
        let entries: Vec<(Key, Vec<HeadDetection>)> = self.scanned.iter().map(|(k, d)| (*k, d.clone())).collect();
        for chunk in entries.chunks(1024) {
            out.extend(framed(&Self::heads_block(chunk)));
        }
        fsutil::atomic_write(&Self::heads_file(&dir), &out, true)?;
        private_file(&Self::heads_file(&dir));
        let live: HashSet<FaceRef> = self.all().map(|(r, _)| r).collect();
        let was_loaded = self.vectors.is_some();
        let all = self.vectors.take().unwrap_or_else(|| Self::read_vectors(&dir));
        let rows: Vec<(FaceRef, Vec<f32>)> = (0..all.len()).filter(|&i| live.contains(&all.refs[i])).map(|i| (all.refs[i], all.row(i).to_vec())).collect();
        let vfile = Self::vectors_file(&dir);
        if rows.is_empty() {
            let _ = fs::remove_file(&vfile);
        } else {
            let mut out = framed(&Self::vectors_header(all.dim));
            for chunk in rows.chunks(1024) {
                out.extend(framed(&HeadVectors::block(chunk)));
            }
            fsutil::atomic_write(&vfile, &out, true)?;
            private_file(&vfile);
        }
        if was_loaded {
            let mut v = HeadVectors::default();
            v.push(&rows);
            self.vectors = Some(v);
        }
        let (path, kept): (PathBuf, Vec<(FaceRef, Vec<u8>)>) = {
            let index = self.tiles.lock();
            let mut kept = Vec::new();
            for r in index.offsets.keys().filter(|r| live.contains(r)) {
                if let Some(bytes) = index.read(r) {
                    kept.push((*r, bytes));
                }
            }
            (index.path.clone(), kept)
        };
        let mut buf = Vec::new();
        let mut offsets = HashMap::new();
        for (r, jpeg) in &kept {
            let at = buf.len() as u64;
            buf.extend(HeadTiles::entry(r, jpeg));
            offsets.insert(*r, (at + 32, jpeg.len() as u32));
        }
        if buf.is_empty() {
            let _ = fs::remove_file(&path);
        } else {
            fsutil::atomic_write(&path, &buf, true)?;
            private_file(&path);
        }
        self.tiles.lock().offsets = offsets;
        Ok(())
    }

    /// *Forget all* and *Clear*: every file goes.
    pub fn forget_all(&mut self) {
        for p in [Self::heads_file(&self.dir), Self::vectors_file(&self.dir), Self::tiles_file(&self.dir)] {
            let _ = fs::remove_file(p);
        }
        self.scanned.clear();
        self.vectors = None;
        self.tiles.lock().offsets.clear();
    }

    /// Bytes the head files in an index folder take (the Storage row's
    /// *Fursuits*; a stat walk, on a worker).
    pub fn bytes_in(dir: &Path) -> u64 {
        [Self::heads_file(dir), Self::vectors_file(dir), Self::tiles_file(dir)].iter().map(|p| fs::metadata(p).map_or(0, |m| m.len())).sum()
    }
}

/// The step the generic grouping runs over (`search::step`): refs and
/// rows are the vectors **in memory**, so a caller builds them first
/// ([`HeadStore::vectors`]); before that the step is empty, never a
/// read behind the grouping's back.  Rows are f32, full width (the
/// user's decision, 2026-10-03).
impl KindStep for HeadStore {
    fn kind(&self) -> Kind {
        Kind::Fursuit
    }

    fn refs(&self) -> &[FaceRef] {
        self.vectors.as_ref().map_or(&[], |v| v.refs.as_slice())
    }

    fn rows(&self) -> Rows<'_> {
        match &self.vectors {
            Some(v) => Rows::F32 { dim: v.dim, data: &v.data },
            None => Rows::F32 { dim: 0, data: &[] },
        }
    }

    fn clarity(&self, r: &FaceRef) -> f32 {
        self.detection(r).map_or(0.0, HeadDetection::clarity)
    }

    fn cutoffs(&self) -> Cutoffs {
        subjects::cutoff_for(VECTOR_MODEL)
    }

    /// Tight groups, no joining step (the user's choice, 2026-10-08,
    /// from the relabel): a head joins a group only when it lies within
    /// the cut of the group's members on average, and groups are never
    /// merged by their centroids.  People's nearest-centroid-with-merge
    /// chained this library's heads into one group of 4 265 (39 % of
    /// them); average linkage at 0.17 kept 95 of 359 labelled same pairs
    /// together with 7 of 432 different pairs, a judged group in eight
    /// mixed, and regroups the library's heads in under two seconds.
    /// *Find more* (0.14) and the review (0.17) join a suit across time.
    fn linkage(&self) -> subjects::Linkage {
        subjects::Linkage::Average
    }
}

// ---- the scan ---------------------------------------------------------

/// Photos per request to the helper.
pub const BATCH: usize = 16;
/// Batches are handed over at least this often while work goes on.
const EMIT_MS: u64 = 700;
/// This many failed reads in a row make the pass check the library.
const FAILURE_RUN: usize = 8;

/// One photo to look at: the shot's representative file (its JPG, else
/// the RAW) and its index key.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadJob {
    pub path: PathBuf,
    pub file_type: FileType,
    pub key: Key,
}

/// Part of a scan's results.
#[derive(Default)]
pub struct HeadBatch {
    pub found: Vec<(HeadJob, PhotoHeads)>,
    pub failed: Vec<(HeadJob, FileError)>,
    pub done: usize,
    pub total: usize,
    pub restarted: bool,
    pub finished: Option<HeadsFinished>,
}

/// How a scan ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeadsFinished {
    /// Every job was tried (none means stopped early).
    pub complete: bool,
    pub stopped: Option<Stopped>,
    pub looked_at: usize,
    pub failed: usize,
    pub elapsed: Duration,
}

/// A running scan.
pub struct ScanHandle {
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicUsize>,
}

impl ScanHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Thumbnails the grid is waiting for: the scan steps back while any
    /// are.
    pub fn set_busy(&self, pending: usize) {
        self.busy.store(pending, Ordering::Relaxed);
    }
}

/// Run a scan on its own thread, handing batches to `emit`.
pub fn start(root: PathBuf, jobs: Vec<HeadJob>, embedder: Arc<dyn Embedder>, emit: impl FnMut(HeadBatch) + Send + 'static) -> ScanHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicUsize::new(0));
    let (c, b) = (cancel.clone(), busy.clone());
    let mut emit = emit;
    std::thread::Builder::new()
        .name("heads-scan".into())
        .spawn(move || {
            execute(&root, &jobs, embedder.as_ref(), &c, &b, &mut emit);
        })
        .expect("spawn heads-scan");
    ScanHandle { cancel, busy }
}

/// The scan itself, on the calling thread.  The card rule is the pass's:
/// a card failure stops it and is reported; a helper restarted on the
/// processor is sent the chunk again; a run of unreadable files makes it
/// check whether the library is away.
pub fn execute(root: &Path, jobs: &[HeadJob], embedder: &dyn Embedder, cancel: &AtomicBool, busy: &AtomicUsize, emit: &mut dyn FnMut(HeadBatch)) -> HeadsFinished {
    let started = Instant::now();
    let mut fin = HeadsFinished::default();
    let mut batch = HeadBatch { total: jobs.len(), ..HeadBatch::default() };
    let mut last = Instant::now();
    let mut failures = 0usize;
    let mut stopped: Option<Stopped> = None;
    let mut i = 0;
    while i < jobs.len() {
        if cancel.load(Ordering::Relaxed) {
            stopped = Some(Stopped::Cancelled);
            break;
        }
        for _ in 0..10 {
            if busy.load(Ordering::Relaxed) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let chunk = &jobs[i..(i + BATCH).min(jobs.len())];
        let files: Vec<(PathBuf, FileType)> = chunk.iter().map(|j| (j.path.clone(), j.file_type)).collect();
        let results = match embedder.find_heads(&files) {
            Ok(r) => r,
            Err(HelperError::Restarted) => {
                batch.restarted = true;
                continue;
            }
            Err(HelperError::Card(f)) => {
                stopped = Some(Stopped::Card(f));
                break;
            }
            Err(HelperError::Broken(why)) => {
                stopped = Some(Stopped::Helper { message: format!("the search helper stopped: {why}") });
                break;
            }
        };
        for (job, r) in chunk.iter().zip(results) {
            match r.result {
                Ok(p) => {
                    failures = 0;
                    fin.looked_at += 1;
                    batch.found.push((job.clone(), p));
                }
                Err(e) => {
                    failures += 1;
                    fin.failed += 1;
                    batch.failed.push((job.clone(), e));
                    if failures >= FAILURE_RUN && stopped.is_none() {
                        if let Err(err) = std::fs::metadata(root) {
                            let why = crate::ops::netfs::describe_errno(err.raw_os_error()).unwrap_or("it can't be read");
                            stopped = Some(Stopped::LibraryAway { share: root.display().to_string(), message: why.to_string() });
                        }
                        failures = 0;
                    }
                }
            }
            batch.done += 1;
        }
        i += chunk.len();
        if stopped.is_some() {
            break;
        }
        if batch.found.len() + batch.failed.len() >= BATCH || last.elapsed() >= Duration::from_millis(EMIT_MS) {
            let done = batch.done;
            emit(std::mem::replace(&mut batch, HeadBatch { total: jobs.len(), done, ..HeadBatch::default() }));
            last = Instant::now();
        }
    }
    fin.complete = stopped.is_none();
    fin.stopped = stopped;
    fin.elapsed = started.elapsed();
    batch.finished = Some(fin.clone());
    emit(batch);
    fin
}

// ---- the step's state -------------------------------------------------

/// What [`Fursuits::consider`] decided about photos that want a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Start now (the card; or the user said so).
    Start,
    /// Wait for the user: the processor is chosen (decision 11).
    Ask(Offer),
    /// Nothing to do: off, nothing to look at, or something runs already.
    Nothing,
}

/// The fursuit step's state on the engine's thread: the cache, the gate,
/// what waits for the user, and the status the pages read.
pub struct Fursuits {
    pub store: Option<HeadStore>,
    pub status: HeadsStatus,
    /// When fursuit work last needed the vectors (a regroup, suggestions,
    /// new heads joining the clusters); none since they were last built.
    last_use: Option<Instant>,
    /// The fursuits' clusters and sureness, from the last regroup and
    /// the heads assigned since (this kind's rows only).
    pub grouped: Grouped,
    generation: u64,
    /// Photos the user skipped a scan of (this session): offered again
    /// only from the tab, as a catch-up.
    skipped: HashSet<Key>,
    /// The jobs behind `Step::Asking`, until the user answers.
    waiting: Vec<HeadJob>,
    /// The vocabulary index of [`GATE_WORD`].
    gate: Option<usize>,
}

impl Default for Fursuits {
    fn default() -> Fursuits {
        Fursuits { store: None, status: HeadsStatus::default(), last_use: None, grouped: Grouped::default(), generation: 1, skipped: HashSet::new(), waiting: Vec::new(), gate: super::vocab::find(GATE_WORD) }
    }
}

impl Fursuits {
    /// The step as the engine starts it: the preference's switch, no
    /// cache yet.
    pub fn new(on: bool) -> Fursuits {
        Fursuits { status: HeadsStatus { on, ..HeadsStatus::default() }, ..Fursuits::default() }
    }

    /// Open the cache in `dir` (the library's index folder).
    pub fn open(&mut self, dir: &Path) {
        self.store = Some(HeadStore::load(dir));
        self.refresh();
    }

    pub fn close(&mut self) {
        self.store = None;
        self.grouped = Grouped::default();
        self.waiting.clear();
        self.status.step = Step::Idle;
        self.refresh();
    }

    // ---- grouping (the generic machinery over this kind's rows) ----

    /// The store with its vectors in memory: built now if they weren't
    /// (only the fursuit work asks for this; opening the library doesn't).
    pub fn step(&mut self) -> Option<&HeadStore> {
        self.load_vectors();
        self.store.as_ref()
    }

    /// Build the vector memory now, if the store is open, and note the use.
    fn load_vectors(&mut self) {
        if let Some(store) = self.store.as_mut() {
            store.vectors();
            self.last_use = Some(Instant::now());
        }
    }

    /// Fursuit work has been quiet this long: the built vectors (and
    /// with them the memory) are let go, to be read again on the next
    /// request.  The user's choice (2026-10-03): full size, freed after
    /// use.
    pub const VECTORS_IDLE: Duration = Duration::from_secs(120);

    /// Let the vectors go when nothing fursuit-shaped has needed them for
    /// [`Fursuits::VECTORS_IDLE`] and no scan is running (a scan's batches
    /// keep joining the clusters).  Returns whether they were released.
    pub fn release_if_idle(&mut self, now: Instant) -> bool {
        let Some(store) = self.store.as_mut() else { return false };
        if !store.vectors_loaded() || self.status.scanning() {
            return false;
        }
        let idle = self.last_use.map_or(true, |t| now.duration_since(t) >= Self::VECTORS_IDLE);
        if idle {
            store.release_vectors();
            self.last_use = None;
        }
        idle
    }

    /// Whether the vectors are in memory right now.
    #[cfg(test)]
    pub fn vectors_resident(&self) -> bool {
        self.store.as_ref().is_some_and(HeadStore::vectors_loaded)
    }

    /// A regroup's input for the worker, stamped with the next
    /// generation; `file` is the user's subjects file (every kind; the
    /// job counts fursuits only).
    pub fn regroup_job(&mut self, file: &SubjectsFile) -> Option<RegroupJob> {
        self.load_vectors();
        let store = self.store.as_ref()?;
        if store.refs().is_empty() {
            return None;
        }
        let generation = self.generation + 1;
        self.generation = generation;
        Some(step::job(store, file, generation))
    }

    /// Take a regroup's result (an older generation's, or another
    /// kind's, is dropped), then assign the heads that arrived meanwhile.
    pub fn apply_regroup(&mut self, r: Regrouped, file: &SubjectsFile) -> bool {
        if r.generation != self.generation || r.kind != Kind::Fursuit {
            return false;
        }
        self.load_vectors();
        let Some(store) = self.store.as_ref() else { return false };
        self.grouped.apply(r, store.refs(), &|f| store.clarity(f));
        self.grouped.assign_new(store, file);
        true
    }

    /// Heads that arrived since the last regroup join the clusters.
    pub fn assign_new(&mut self, file: &SubjectsFile) {
        self.load_vectors();
        if let Some(store) = self.store.as_ref() {
            self.grouped.assign_new(store, file);
        }
    }

    /// The grouping starts over (the cache was emptied).
    pub fn reset_grouping(&mut self) {
        self.grouped = Grouped::default();
        self.generation += 1;
    }

    /// The automatic groups for the *Fursuits* tab: each cluster's id
    /// and members, in cluster order (the snapshot builder makes
    /// `Group`s of them, kind Fursuit).
    pub fn clusters(&self) -> Vec<(u64, Vec<FaceRef>)> {
        let Some(store) = &self.store else { return Vec::new() };
        let refs = store.refs();
        self.grouped.clusters.iter().zip(&self.grouped.ids).map(|(c, &id)| (id, c.members.iter().filter_map(|&m| refs.get(m).copied()).collect())).collect()
    }

    /// Suggestions waiting for a fursuit subject, from the last regroup.
    pub fn suggestion_count(&self, id: &subjects::SubjectId) -> usize {
        self.grouped.suggestion_counts.get(id).copied().unwrap_or(0)
    }

    /// Suggestions for review (UI 6): for a fursuit subject, the heads
    /// within the cut-off (the strict one with `strict`: *Find more*) of
    /// its confirmed members that are confirmed nowhere and not rejected
    /// for it, nearest first, one entry per burst; for a cluster, its own
    /// members to check.  A subject of another kind gets nothing: never
    /// a face among heads.  The vectors must be in memory ([`Fursuits::step`]
    /// builds them); before that the answer is empty.
    pub fn suggestions(&self, file: &SubjectsFile, id: &GroupId, resolve: &dyn Fn(&FaceRef) -> Option<Face>, strict: bool) -> Result<Vec<Vec<Face>>, Refused> {
        let Some(store) = self.store.as_ref() else { return Ok(Vec::new()) };
        let grouped = &self.grouped;
        let refs = store.refs();
        let rows = store.rows();
        let by_ref: HashMap<FaceRef, usize> = refs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let ordered: Vec<(usize, f32)> = match id {
            GroupId::Subject(sid) => {
                let s = file.get(sid).ok_or_else(|| Refused::NoSuchSubject(sid.clone()))?;
                if s.kind != Kind::Fursuit {
                    return Ok(Vec::new());
                }
                let members: Vec<usize> = s.members.iter().filter_map(|r| by_ref.get(r).copied()).collect();
                let confirmed = file.confirmed(Kind::Fursuit);
                let pool: Vec<usize> = (0..refs.len()).filter(|&i| !confirmed.contains_key(&refs[i]) && !s.rejected.contains(&refs[i])).collect();
                let cutoffs = store.cutoffs();
                subjects::candidates(&rows, &members, &pool, if strict { cutoffs.strict } else { cutoffs.suggest })
            }
            GroupId::Cluster(cid) => {
                let c = grouped.cluster(*cid).ok_or(Refused::StaleGroup)?;
                let mut v: Vec<(usize, f32)> = c.members.iter().map(|&m| (m, 1.0 - rows.dot(m, &c.centroid))).collect();
                v.sort_by(|a, b| a.1.total_cmp(&b.1));
                v
            }
        };
        let heads: Vec<(Face, Option<u64>)> = ordered
            .iter()
            .filter_map(|&(i, _)| resolve(refs.get(i)?))
            .map(|f| {
                let b = f.burst;
                (f, b)
            })
            .collect();
        Ok(subjects::per_burst(heads))
    }

    /// Whether a group is this kind's: a fursuit subject, or one of the
    /// current fursuit clusters.
    pub fn is_fursuit_group(&self, file: &SubjectsFile, id: &GroupId) -> bool {
        match id {
            GroupId::Subject(s) => file.get(s).is_some_and(|s| s.kind == Kind::Fursuit),
            GroupId::Cluster(c) => self.grouped.cluster(*c).is_some(),
        }
    }

    /// Add this kind's part to the *Subjects* snapshot the people state
    /// built: the fursuit subjects' suggestion counts, and the fursuit
    /// clusters as groups to name (kind Fursuit); then the order again.
    pub fn extend_snapshot(&self, s: &mut subjects::Subjects, resolve: &dyn Fn(&FaceRef) -> Option<Face>) {
        for g in &mut s.groups {
            if let (Kind::Fursuit, GroupId::Subject(id)) = (g.kind, &g.id) {
                g.suggestions = self.suggestion_count(id);
            }
        }
        for (id, members) in self.clusters() {
            let heads: Vec<Face> = members.iter().filter_map(resolve).collect();
            let Some(cover) = heads.iter().cloned().max_by(|a, b| a.clarity.total_cmp(&b.clarity)) else { continue };
            let photos = heads.iter().map(|f| f.face.key).collect::<HashSet<_>>().len();
            let days = heads.iter().map(|f| f.day).collect::<HashSet<_>>().len();
            s.groups.push(subjects::Group { id: GroupId::Cluster(id), kind: Kind::Fursuit, name: None, hidden: false, photos, days, cover, suggestions: 0 });
        }
        s.groups.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(matches!(b.id, GroupId::Subject(_)).cmp(&matches!(a.id, GroupId::Subject(_)))).then(b.photos.cmp(&a.photos)).then_with(|| a.name.cmp(&b.name)));
    }

    /// A head's clarity (UI 12): how clear a view it is and, once the
    /// vectors are in memory, how sure its membership is.
    pub fn clarity_with_sureness(&self, r: &FaceRef) -> f32 {
        let Some(store) = &self.store else { return 0.0 };
        let visual = store.clarity(r);
        match store.vectors.as_ref().and_then(|v| v.by_ref.get(r)) {
            Some(&row) => self.grouped.clarity_of(row, visual),
            None => 0.5 * visual,
        }
    }

    /// The photos to look at: every indexed shot the tags call a fursuit
    /// and the cache hasn't seen, newest day first; `catch_up` includes
    /// the ones skipped earlier this session.
    pub fn plan(&self, lib: &LibraryIndex, index: &super::index::Index, tags: &Tags, catch_up: bool) -> Vec<HeadJob> {
        let Some(w) = self.gate else { return Vec::new() };
        let mut jobs = Vec::new();
        let mut tagged = 0usize;
        for day in lib.days.values().rev() {
            for shot in day.shots() {
                let rep = shot.iter().find(|&&i| day.files[i].file_type == FileType::Jpg).or(shot.first());
                let Some(&i) = rep else { continue };
                let fe = &day.files[i];
                let rel = super::pass::rel_path(&lib.root, &fe.path);
                let Some(key) = index.paths.key_for(&rel, fe.size, &fe.mtime) else { continue };
                let Some(&row) = index.vectors.by_key.get(&key) else { continue };
                if !tags.has(row, w) {
                    continue;
                }
                tagged += 1;
                if self.store.as_ref().is_some_and(|s| s.scanned.contains_key(&key)) {
                    continue;
                }
                if !catch_up && self.skipped.contains(&key) {
                    continue;
                }
                jobs.push(HeadJob { path: fe.path.clone(), file_type: fe.file_type, key });
            }
        }
        let _ = tagged;
        jobs
    }

    /// Count the indexed photos the tags call fursuits (for the status).
    pub fn count_tagged(&mut self, tags: &Tags) {
        let tagged = match self.gate {
            Some(w) => (0..tags.len()).filter(|&i| tags.has(i, w)).count(),
            None => 0,
        };
        self.status.tagged = tagged;
    }

    /// Photos want a scan: start it, or hold it for the user's answer.
    /// On the card a scan starts unasked; on the processor every scan
    /// waits as an [`Offer`] with its cost, however it came about, except
    /// a catch-up the user just asked for from the tab (`asked`), which
    /// the tab has already put the cost to.
    pub fn consider(&mut self, jobs: Vec<HeadJob>, why: Why, device: Device, asked: bool) -> Decision {
        if !self.status.on || jobs.is_empty() || !matches!(self.status.step, Step::Idle) || self.store.is_none() {
            return Decision::Nothing;
        }
        if starts_unasked(device) || asked {
            self.status.step = Step::Scanning { why, done: 0, total: jobs.len() };
            self.waiting.clear();
            return Decision::Start;
        }
        let offer = Offer { photos: jobs.len(), why };
        self.waiting = jobs;
        self.status.step = Step::Asking(offer);
        Decision::Ask(offer)
    }

    /// The user answered the question: the jobs to start (`Some`), or
    /// none when skipped (the photos are remembered as skipped and
    /// offered again from the tab).
    pub fn answer(&mut self, start: bool) -> Option<(Vec<HeadJob>, Why)> {
        let Step::Asking(offer) = self.status.step.clone() else { return None };
        let jobs = std::mem::take(&mut self.waiting);
        if start {
            self.status.step = Step::Scanning { why: offer.why, done: 0, total: jobs.len() };
            Some((jobs, offer.why))
        } else {
            self.skipped.extend(jobs.iter().map(|j| j.key));
            self.status.skipped = self.skipped.len();
            self.status.step = Step::Idle;
            None
        }
    }

    /// A batch from the scan: the cache grows, the counts follow.  Returns
    /// whether the scan is over.
    pub fn absorb(&mut self, batch: &HeadBatch) -> bool {
        if let Some(store) = &mut self.store {
            for (job, p) in &batch.found {
                self.skipped.remove(&job.key);
                if let Err(e) = store.add(p.key, &p.heads) {
                    eprintln!("heads: couldn't write the head cache: {e}");
                }
            }
            self.status.skipped = self.skipped.len();
        }
        if let Step::Scanning { why, .. } = self.status.step {
            self.status.step = Step::Scanning { why, done: batch.done, total: batch.total };
        }
        if let Some(fin) = &batch.finished {
            self.status.step = match &fin.stopped {
                Some(Stopped::Card(f)) => Step::Failed(f.clone()),
                _ => Step::Idle,
            };
        }
        self.refresh();
        batch.finished.is_some()
    }

    /// The scan was cancelled or dropped before it finished.
    pub fn stop(&mut self) {
        self.waiting.clear();
        self.status.step = Step::Idle;
        self.refresh();
    }

    /// The card works again (or was chosen again): a failed step is idle,
    /// ready to be offered.
    pub fn card_back(&mut self) {
        if matches!(self.status.step, Step::Failed(_)) {
            self.status.step = Step::Idle;
        }
    }

    /// The card failed elsewhere (the search pass): the step stops too.
    pub fn card_failed(&mut self, f: CardFailure) {
        self.waiting.clear();
        self.status.step = Step::Failed(f);
        self.refresh();
    }

    /// *Forget all*: the cache goes.
    pub fn forget_all(&mut self) {
        if let Some(store) = &mut self.store {
            store.forget_all();
        }
        self.skipped.clear();
        self.status.skipped = 0;
        self.refresh();
    }

    /// Bring the counts up to the cache.
    pub fn refresh(&mut self) {
        let (scanned, found) = self.store.as_ref().map_or((0, 0), |s| (s.looked_at(), s.found()));
        self.status.scanned = scanned;
        self.status.found = found;
    }

}

/// A head's pixels for a tile or the review: its cached tile when there
/// is one (decoded and resized), else none (the caller falls back to the
/// photo).
pub fn tile_pixels(tiles: &HeadTiles, r: &FaceRef, px: u32) -> Option<(u32, u32, Vec<u8>)> {
    use crate::imaging::decode;
    let jpeg = tiles.read(r)?;
    let decoded = decode::decode_jpeg_full_scaled(&jpeg, 1).ok()?;
    let out = decode::resize_to(&decoded, px.clamp(8, 2048), px.clamp(8, 2048));
    let (w, h) = (out.width, out.height);
    Some((w, h, decode::to_rgba(out)))
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::ml::client::ImageResult;
    use crate::ml::heads::HeadsResult;
    use crate::ml::{SelfCheck, client::Want};
    use crate::ml::models::ModelId;

    fn key(n: u8) -> Key {
        Key([n; 16])
    }

    fn head(x: f32, size: u32, v: &[f32]) -> Head {
        Head { bbox: [x, 0.2, 0.2, 0.3], score: 0.5, size, sharpness: 0.1, vector: v.to_vec(), tile: vec![0xFF, 0xD8, 0xFF, 0xD9] }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-heads-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_head_cache_round_trips_and_is_private() {
        let d = scratch("store");
        let mut s = HeadStore::load(&d);
        assert_eq!((s.looked_at(), s.found()), (0, 0));
        s.add(key(1), &[head(0.1, 300, &[1.0, 0.0]), head(0.5, 120, &[0.0, 1.0])]).unwrap();
        s.add(key(2), &[]).unwrap();
        assert!(!s.vectors_loaded(), "writing never builds the vector memory");
        assert_eq!((s.looked_at(), s.found(), s.vectors().len()), (2, 2, 2));
        let mut again = HeadStore::load(&d);
        assert!(!again.vectors_loaded(), "opening the library reads no vectors");
        assert_eq!((again.looked_at(), again.found()), (2, 2));
        let r = again.heads_of(&key(1))[0].head_ref(key(1));
        let v = again.vectors();
        assert_eq!((v.len(), v.dim), (2, 2));
        assert_eq!(v.row(v.by_ref[&r]), &[1.0, 0.0]);
        let mut again = HeadStore::load(&d);
        again.add(key(3), &[head(0.3, 150, &[0.0, 1.0])]).unwrap();
        assert!(!again.vectors_loaded(), "a write builds no memory");
        assert_eq!(again.vectors().len(), 3, "a row written before the vectors were read is read with the rest");
        assert_eq!(again.tiles().lock().read(&r).unwrap(), vec![0xFF, 0xD8, 0xFF, 0xD9]);
        assert!(again.detection(&r).unwrap().clarity() > again.heads_of(&key(1))[1].clarity(), "the larger head is clearer");
        for f in ["heads.bin", "headvecs-dinov2-base.bin", "headtiles.bin"] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(d.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
        }
        // Only the live photos are kept; a big drop compacts the files.
        let mut s = again;
        s.retain(&HashSet::from([key(2)]));
        assert_eq!((s.looked_at(), s.found(), s.vectors().len()), (1, 0, 0));
        assert!(!d.join("headvecs-dinov2-base.bin").exists() && !d.join("headtiles.bin").exists());
        assert_eq!(HeadStore::load(&d).looked_at(), 1);
        s.forget_all();
        assert_eq!(HeadStore::bytes_in(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    /// The memory the full-width vectors take at the library's expected
    /// scale (the user's decision, 2026-10-03: f32, 1 536 wide, built only
    /// when asked for).  Writes a scratch store of `PP_HEADS_ROWS` rows
    /// (68 000 by default: ~1.6 heads in each of ~42 000 fursuit photos),
    /// reads the process's resident size before and after the vectors
    /// are built, and again with a regroup job's copy alive.  Ignored:
    /// it writes ~200 MB and takes a while.
    #[test]
    #[ignore = "measures resident memory at library scale (PP_HEADS_ROWS)"]
    fn resident_size_of_the_head_vectors_at_library_scale() {
        fn rss_mb() -> f64 {
            let s = fs::read_to_string("/proc/self/status").unwrap_or_default();
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
        }
        let rows: usize = std::env::var("PP_HEADS_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(68_000);
        let dim = crate::ml::heads::DIM;
        let d = scratch("rss");
        let t = std::time::Instant::now();
        {
            let mut s = HeadStore::load(&d);
            let mut v = vec![0f32; dim];
            for i in 0..rows {
                v.fill(0.0);
                v[i % dim] = 1.0;
                let mut k = [0u8; 16];
                k[..8].copy_from_slice(&(i as u64).to_le_bytes());
                s.add(Key(k), &[head(0.1, 200, &v)]).unwrap();
            }
        }
        eprintln!("wrote {rows} rows in {:.1} s: {} MB on disk", t.elapsed().as_secs_f64(), HeadStore::bytes_in(&d) / 1_000_000);
        let before = rss_mb();
        let mut s = HeadStore::load(&d);
        let opened = rss_mb();
        let t = std::time::Instant::now();
        let n = s.vectors().len();
        let built = rss_mb();
        let read_s = t.elapsed().as_secs_f64();
        let file = SubjectsFile::default();
        let job = step::job(&s, &file, 1);
        let with_job = rss_mb();
        eprintln!(
            "{n} rows × {dim}: resident {before:.0} MB before, {opened:.0} MB with the store open (detections and tiles), {built:.0} MB with the vectors built in {read_s:.1} s (+{:.0} MB), {with_job:.0} MB with a regroup job's copy alive (+{:.0} MB)",
            built - opened,
            with_job - built
        );
        drop(job);
        let after_job = rss_mb();
        s.release_vectors();
        let released = rss_mb();
        eprintln!("{after_job:.0} MB with the job gone, {released:.0} MB with the vectors released (freed after use)");
        assert_eq!(n, rows);
        assert!(built - opened > (rows * dim * 4) as f64 / 1_000_000.0 * 0.8, "the vectors are resident once built");
        assert!(released < opened + 60.0, "released: back near the store-open size ({released:.0} vs {opened:.0} MB)");
        let _ = fs::remove_dir_all(&d);
    }


    /// The grouping variants the tune compares, by name.
    fn linkages() -> Vec<(&'static str, f32, subjects::Linkage)> {
        use subjects::Linkage as L;
        vec![
            ("ships now: average at the group cut", subjects::cutoff_for(VECTOR_MODEL).group, L::Average),
            ("shipped before: centroid+merge 0.14", 0.14, L::PEOPLE),
            ("centroid, no merge 0.14", 0.14, L::Centroid { merge: false, seed_cap: None }),
            ("centroid+merge 0.12", 0.12, L::PEOPLE),
            ("centroid+merge 0.10", 0.10, L::PEOPLE),
            ("centroid, seed cap 0.14/0.14", 0.14, L::Centroid { merge: false, seed_cap: Some(0.14) }),
            ("centroid, seed cap 0.14/0.17", 0.14, L::Centroid { merge: false, seed_cap: Some(0.17) }),
            ("average 0.14", 0.14, L::Average),
            ("average 0.17", 0.17, L::Average),
            ("complete 0.14", 0.14, L::Complete),
            ("complete 0.17", 0.17, L::Complete),
            ("complete 0.20", 0.20, L::Complete),
        ]
    }

    /// Rows of a head store in the regroup's order (clearest first), as
    /// the engine would group them.
    fn ordered(store: &HeadStore) -> Vec<usize> {
        let refs = store.refs();
        let mut order: Vec<usize> = (0..refs.len()).collect();
        order.sort_by(|&a, &b| store.clarity(&refs[b]).total_cmp(&store.clarity(&refs[a])).then(a.cmp(&b)));
        order
    }

    /// Contact sheets of the biggest groups under each linkage, from a
    /// head cache an eval run left (`PP_EVAL_OUT/index`), for looking at
    /// by eye: `PP_SHEETS_OUT/<variant>-<n>.jpg`, one row of tiles per
    /// group, 12 tiles at most.  Prints the group sizes.  Nothing is
    /// published; the sheets stay in scratch.
    #[test]
    #[ignore = "needs a head cache from an eval run (PP_EVAL_OUT) and PP_SHEETS_OUT"]
    fn fursuit_group_sheets() {
        use crate::imaging::decode;
        let dir = PathBuf::from(std::env::var("PP_EVAL_OUT").expect("PP_EVAL_OUT")).join("index");
        let out = PathBuf::from(std::env::var("PP_SHEETS_OUT").expect("PP_SHEETS_OUT"));
        fs::create_dir_all(&out).unwrap();
        let mut store = HeadStore::load(&dir);
        store.vectors();
        let mut order = ordered(&store);
        // PP_SHEETS_DAY=<day folder name>: only that day's heads, found
        // through the index's path map.
        if let Ok(day) = std::env::var("PP_SHEETS_DAY") {
            let root = PathBuf::from(std::env::var("PP_EVAL_LIBRARY").expect("PP_EVAL_LIBRARY with PP_SHEETS_DAY"));
            let index = super::super::index::Index::load(&root, &dir, models::info(ModelId::Siglip2));
            let keys: HashSet<Key> = index.paths.entries.iter().filter(|(rel, _)| rel.to_string_lossy().contains(&day)).map(|(_, e)| e.key).collect();
            let refs = store.refs();
            order.retain(|&i| keys.contains(&refs[i].key));
            eprintln!("{} heads on {day}", order.len());
        }
        let rows = store.rows();
        let tiles = store.tiles();
        eprintln!("{} heads in {} photos", store.found(), store.looked_at());
        // The groups that ship now, to say what the other variants' big
        // groups are made of (how many of the tight groups each spans).
        let tight = subjects::cluster_with(&rows, &order, store.cutoffs().group, store.linkage());
        let mut tight_of: HashMap<usize, usize> = HashMap::new();
        for (g, c) in tight.iter().enumerate().filter(|(_, c)| c.members.len() >= 3) {
            for &m in &c.members {
                tight_of.insert(m, g);
            }
        }
        for (name, cut, linkage) in linkages() {
            let clusters = subjects::cluster_with(&rows, &order, cut, linkage);
            let sizes: Vec<usize> = clusters.iter().map(|c| c.members.len()).collect();
            let big = sizes.iter().filter(|&&n| n >= 3).count();
            let spans: Vec<String> = clusters.iter().take(4).map(|c| {
                let groups: HashSet<usize> = c.members.iter().filter_map(|m| tight_of.get(m).copied()).collect();
                format!("{} heads span {} tight 3+ groups", c.members.len(), groups.len())
            }).collect();
            eprintln!("{name}: {} groups, {big} of 3+ heads; sizes {:?}; {}", clusters.len(), &sizes[..sizes.len().min(12)], spans.join(", "));
            let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            for (gi, c) in clusters.iter().take(6).enumerate() {
                if c.members.len() < 3 {
                    break;
                }
                let mut sheet = vec![0u8; 12 * 128 * 128 * 3];
                let mut n = 0;
                for &m in c.members.iter().take(12) {
                    let Some(jpeg) = tiles.lock().read(&store.refs()[m]) else { continue };
                    let Ok(px) = decode::decode_jpeg_full_scaled(&jpeg, 1) else { continue };
                    let px = decode::resize_to(&px, 128, 128);
                    let rgb = decode::to_rgb(&px);
                    for y in 0..128 {
                        let src = &rgb[y * 128 * 3..(y + 1) * 128 * 3];
                        let dst = (y * 12 * 128 + n * 128) * 3;
                        sheet[dst..dst + 128 * 3].copy_from_slice(src);
                    }
                    n += 1;
                }
                let mut bytes = Vec::new();
                jpeg_encoder::Encoder::new(&mut bytes, 85).encode(&sheet, (12 * 128) as u16, 128, jpeg_encoder::ColorType::Rgb).unwrap();
                fs::write(out.join(format!("{slug}-{gi}-{}heads.jpg", c.members.len())), bytes).unwrap();
            }
        }
    }

    /// Score the grouping variants against the user's pairwise labels
    /// (v3 `labels.json`: tracks, crop boxes, same/different answers),
    /// over the app's own heads of the same photos (a head cache from an
    /// eval run over the two labelled days, `PP_EVAL_OUT`; the library
    /// root `PP_EVAL_LIBRARY`; the labels copy `PP_LABELS`).  A label
    /// track is matched to the app's heads of its photo by box overlap
    /// (IoU ≥ 0.5).  Per variant: groups holding two known-different
    /// tracks (mixed), known-same pairs split across groups, known-
    /// different pairs merged into one group; same-day and cross-day
    /// apart; and the group sizes.  Prints only counts.
    #[test]
    #[ignore = "needs the labels copy (PP_LABELS), a head cache (PP_EVAL_OUT) and the library (PP_EVAL_LIBRARY)"]
    fn fursuit_grouping_vs_labels() {
        use std::collections::BTreeMap;
        let labels: serde_json::Value = serde_json::from_slice(&fs::read(std::env::var("PP_LABELS").expect("PP_LABELS")).unwrap()).unwrap();
        assert_eq!(labels["version"], 3, "a v3 pairwise labels file");
        let root = PathBuf::from(std::env::var("PP_EVAL_LIBRARY").expect("PP_EVAL_LIBRARY"));
        let dir = PathBuf::from(std::env::var("PP_EVAL_OUT").expect("PP_EVAL_OUT")).join("index");
        let index = super::super::index::Index::load(&root, &dir, models::info(ModelId::Siglip2));
        // File name (the camera's) → key, through the index's path map.
        let key_by_name: HashMap<String, Key> = index.paths.entries.iter().filter_map(|(rel, e)| Some((rel.file_name()?.to_string_lossy().to_string(), e.key))).collect();
        let mut store = HeadStore::load(&dir);
        store.vectors();
        let refs = store.refs().to_vec();
        let row_of: HashMap<FaceRef, usize> = refs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        // Label crops → app rows.
        let crops = labels["crop_files"].as_array().unwrap();
        let mut row_of_crop: HashMap<u64, usize> = HashMap::new();
        let (mut matched, mut unmatched_photo, mut unmatched_box) = (0, 0, 0);
        for c in crops {
            let name = c["name"].as_str().unwrap();
            let Some(key) = key_by_name.get(name) else { unmatched_photo += 1; continue };
            let b: Vec<f32> = c["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
            let lb = [b[0], b[1], b[2] - b[0], b[3] - b[1]];
            let best = store.heads_of(key).iter().map(|d| (crate::ml::heads::iou(lb, d.bbox), d.head_ref(*key))).max_by(|a, b| a.0.total_cmp(&b.0));
            match best {
                Some((iou, r)) if iou >= 0.5 => {
                    if let Some(&row) = row_of.get(&r) {
                        row_of_crop.insert(c["i"].as_u64().unwrap(), row);
                        matched += 1;
                    }
                }
                _ => unmatched_box += 1,
            }
        }
        eprintln!("label crops: {matched} matched to the app's heads, {unmatched_box} without a matching box, {unmatched_photo} whose photo is not indexed");
        // Tracks → rows; a track is one head through one press.
        let tracks = labels["tracks"].as_array().unwrap();
        let mut rows_of_track: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut set_of_track: HashMap<u64, String> = HashMap::new();
        for t in tracks {
            let id = t["t"].as_u64().unwrap();
            set_of_track.insert(id, t["set"].as_str().unwrap().to_string());
            let rs: Vec<usize> = t["crops"].as_array().unwrap().iter().filter_map(|i| row_of_crop.get(&i.as_u64().unwrap()).copied()).collect();
            if !rs.is_empty() {
                rows_of_track.insert(id, rs);
            }
        }
        // Known pairs: asked, plus implied (same is transitive; different
        // carries across same-groups).
        let bad: HashSet<u64> = labels["bad"].as_object().map(|m| m.keys().filter_map(|k| k.parse().ok()).collect()).unwrap_or_default();
        let mut parent: HashMap<u64, u64> = HashMap::new();
        fn find(p: &mut HashMap<u64, u64>, x: u64) -> u64 {
            let mut r = x;
            while let Some(&q) = p.get(&r) {
                if q == r {
                    break;
                }
                r = q;
            }
            r
        }
        let answers: Vec<(u64, u64, bool, bool)> = labels["answers"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|a| {
                let (x, y) = (a["a"].as_u64()?, a["b"].as_u64()?);
                if bad.contains(&x) || bad.contains(&y) {
                    return None;
                }
                match a["r"].as_str()? {
                    "same" => Some((x, y, true, a["cross"].as_bool().unwrap_or(false))),
                    "diff" => Some((x, y, false, a["cross"].as_bool().unwrap_or(false))),
                    _ => None,
                }
            })
            .collect();
        for &(x, y, same, _) in &answers {
            if same {
                let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
                if rx != ry {
                    parent.insert(rx.max(ry), rx.min(ry));
                }
            }
        }
        let mut diff: HashSet<(u64, u64)> = HashSet::new();
        for &(x, y, same, _) in &answers {
            if !same {
                let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
                diff.insert((rx.min(ry), rx.max(ry)));
            }
        }
        let labelled: Vec<u64> = {
            let mut v: Vec<u64> = answers.iter().flat_map(|&(x, y, _, _)| [x, y]).filter(|t| rows_of_track.contains_key(t)).collect();
            v.sort();
            v.dedup();
            v
        };
        let mut known: Vec<(u64, u64, bool, bool)> = Vec::new();
        for (i, &x) in labelled.iter().enumerate() {
            for &y in &labelled[i + 1..] {
                let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
                let same = rx == ry;
                if !same && !diff.contains(&(rx.min(ry), rx.max(ry))) {
                    continue;
                }
                let cross = set_of_track[&x] != set_of_track[&y];
                known.push((x, y, same, cross));
            }
        }
        let n_same = known.iter().filter(|k| k.2).count();
        let n_cross = known.iter().filter(|k| k.3).count();
        eprintln!("{} labelled tracks with app heads; {} known pairs ({n_same} same, {} different; {n_cross} cross-day) from {} answers", labelled.len(), known.len(), known.len() - n_same, answers.len());
        let order = ordered(&store);
        let rows = store.rows();
        for (name, cut, linkage) in linkages() {
            let clusters = subjects::cluster_with(&rows, &order, cut, linkage);
            let mut group_of_row: HashMap<usize, usize> = HashMap::new();
            for (g, c) in clusters.iter().enumerate() {
                for &m in &c.members {
                    group_of_row.insert(m, g);
                }
            }
            // A track's group: the group most of its rows are in.
            let group_of_track: HashMap<u64, usize> = rows_of_track
                .iter()
                .filter_map(|(t, rs)| {
                    let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
                    for r in rs {
                        if let Some(&g) = group_of_row.get(r) {
                            *counts.entry(g).or_default() += 1;
                        }
                    }
                    counts.into_iter().max_by_key(|&(_, n)| n).map(|(g, _)| (*t, g))
                })
                .collect();
            let (mut split, mut merged, mut split_cross, mut merged_cross, mut same_seen, mut diff_seen) = (0, 0, 0, 0, 0, 0);
            let mut mixed_groups: HashSet<usize> = HashSet::new();
            let mut judged_groups: HashSet<usize> = HashSet::new();
            for &(x, y, same, cross) in &known {
                let (Some(&gx), Some(&gy)) = (group_of_track.get(&x), group_of_track.get(&y)) else { continue };
                if same {
                    same_seen += 1;
                    if gx != gy {
                        split += 1;
                        split_cross += cross as usize;
                    } else {
                        judged_groups.insert(gx);
                    }
                } else {
                    diff_seen += 1;
                    if gx == gy {
                        merged += 1;
                        merged_cross += cross as usize;
                        mixed_groups.insert(gx);
                    }
                }
            }
            judged_groups.extend(mixed_groups.iter().copied());
            let sizes: Vec<usize> = clusters.iter().map(|c| c.members.len()).collect();
            eprintln!(
                "{name}: {} groups ({} of 3+ heads); judged groups {}, mixed {}; same pairs split {split}/{same_seen} (cross-day {split_cross}); different pairs merged {merged}/{diff_seen} (cross-day {merged_cross}); sizes {:?}",
                clusters.len(),
                sizes.iter().filter(|&&n| n >= 3).count(),
                judged_groups.len(),
                mixed_groups.len(),
                &sizes[..sizes.len().min(10)]
            );
        }
    }

    /// Dump a head cache for the relabelling page's builder: per photo
    /// its key, library-relative path, capture time and day, and each
    /// head's box, score, size, sharpness and vector row; the f32 rows as
    /// a raw file.  `PP_EVAL_OUT/index` holds the cache and the index,
    /// `PP_EVAL_LIBRARY` the library root, `PP_DUMP_OUT` where to write.
    /// Paths are relative to the library root; nothing else about the
    /// user's photos is written.
    #[test]
    #[ignore = "needs a head cache from an eval run (PP_EVAL_OUT, PP_EVAL_LIBRARY) and PP_DUMP_OUT"]
    fn fursuit_relabel_dump() {
        let root = PathBuf::from(std::env::var("PP_EVAL_LIBRARY").expect("PP_EVAL_LIBRARY"));
        let dir = PathBuf::from(std::env::var("PP_EVAL_OUT").expect("PP_EVAL_OUT")).join("index");
        let out = PathBuf::from(std::env::var("PP_DUMP_OUT").expect("PP_DUMP_OUT"));
        fs::create_dir_all(&out).unwrap();
        let index = super::super::index::Index::load(&root, &dir, models::info(ModelId::Siglip2));
        let mut store = HeadStore::load(&dir);
        let (vrefs, vdim, vdata) = {
            let v = store.vectors();
            (v.refs.clone(), v.dim, v.data.clone())
        };
        let row_of: HashMap<FaceRef, usize> = vrefs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let mut photos = Vec::new();
        for (rel, e) in &index.paths.entries {
            let dets = store.heads_of(&e.key);
            if dets.is_empty() {
                continue;
            }
            let rec = index.vectors.by_key.get(&e.key).map(|&i| &index.vectors.records[i]);
            let capture_ms = rec.and_then(|r| r.capture.map(|c| c.and_utc().timestamp_millis() + r.meta.subsec_ms.unwrap_or(0) as i64));
            let day = rec.and_then(|r| r.capture.map(|c| c.date().to_string())).or_else(|| rel.components().nth(2).map(|c| c.as_os_str().to_string_lossy().to_string()));
            let heads: Vec<serde_json::Value> = dets
                .iter()
                .map(|d| {
                    let r = d.head_ref(e.key);
                    serde_json::json!({ "bbox": d.bbox, "thousandths": r.thousandths(), "score": d.score, "size_px": d.size_px, "sharpness": d.sharpness, "clarity": d.clarity(), "row": row_of.get(&r) })
                })
                .collect();
            photos.push(serde_json::json!({ "key": format!("{:032x}", u128::from_be_bytes(e.key.0)), "rel": rel.to_string_lossy(), "capture_ms": capture_ms, "day": day, "heads": heads }));
        }
        let rows: Vec<serde_json::Value> = vrefs.iter().map(|r| serde_json::json!({ "key": format!("{:032x}", u128::from_be_bytes(r.key.0)), "thousandths": r.thousandths() })).collect();
        let dump = serde_json::json!({ "root": root.to_string_lossy(), "dim": vdim, "photos": photos, "rows": rows });
        fs::write(out.join("heads.json"), serde_json::to_vec(&dump).unwrap()).unwrap();
        let bytes: Vec<u8> = vdata.iter().flat_map(|x| x.to_le_bytes()).collect();
        fs::write(out.join("vectors.f32"), bytes).unwrap();
        eprintln!("dumped {} photos with heads, {} rows × {}", photos.len(), vrefs.len(), vdim);
    }

    /// Score the grouping variants against the relabelling page's
    /// answers (v4: each answer carries both heads by content key and
    /// box), over the app's own heads (`PP_EVAL_OUT/index`): per tier,
    /// the distance curve, and for each variant recall (same pairs in
    /// one group), merged different pairs, and the share of 3+ groups
    /// holding a labelled different pair; Find more's strict cut and the
    /// suggestion band as the pair's own distance.  `PP_HOLDOUT=even|odd`
    /// scores only the answers whose first head's day index is even or
    /// odd (a hold-out by day).  Prints counts only.
    #[test]
    #[ignore = "needs the labels copy (PP_LABELS) and a head cache (PP_EVAL_OUT)"]
    fn fursuit_relabel_score() {
        let labels: serde_json::Value = serde_json::from_slice(&fs::read(std::env::var("PP_LABELS").expect("PP_LABELS")).unwrap()).unwrap();
        assert_eq!(labels["version"], 4, "a v4 relabel file");
        let dir = PathBuf::from(std::env::var("PP_EVAL_OUT").expect("PP_EVAL_OUT")).join("index");
        let holdout = std::env::var("PP_HOLDOUT").ok();
        let mut store = HeadStore::load(&dir);
        store.vectors();
        let refs = store.refs().to_vec();
        let row_of: HashMap<FaceRef, usize> = refs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let key_of = |hex: &str| -> Key {
            let mut k = [0u8; 16];
            for (i, b) in k.iter_mut().enumerate() {
                *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
            }
            Key(k)
        };
        // The page rounds boxes to four decimals, the store's refs to
        // thousandths: join by key and the best-overlapping head of that
        // photo (IoU >= 0.6), which is the same head.
        let exact = std::cell::Cell::new(0usize);
        let head_row = |h: &serde_json::Value| -> Option<usize> {
            let b: Vec<f32> = h["bbox"].as_array()?.iter().map(|v| v.as_f64().unwrap() as f32).collect();
            let key = key_of(h["key"].as_str()?);
            let r = FaceRef::new(key, [b[0], b[1], b[2], b[3]]);
            if let Some(&row) = row_of.get(&r) {
                exact.set(exact.get() + 1);
                return Some(row);
            }
            let best = store.heads_of(&key).iter().map(|d| (crate::ml::heads::iou([b[0], b[1], b[2], b[3]], d.bbox), d.head_ref(key))).max_by(|x, y| x.0.total_cmp(&y.0))?;
            if best.0 < 0.6 {
                return None;
            }
            row_of.get(&best.1).copied()
        };
        // Days, for the hold-out.
        let mut days: Vec<String> = labels["answers"].as_object().unwrap().values().map(|a| a["heads"]["a"]["day"].as_str().unwrap().to_string()).collect();
        days.sort();
        days.dedup();
        let day_index: HashMap<String, usize> = days.iter().enumerate().map(|(i, d)| (d.clone(), i)).collect();
        // Answers → (row a, row b, same, tier, d, held)
        let mut pairs: Vec<(usize, usize, bool, u64, f32)> = Vec::new();
        let (mut unmapped, mut skipped, mut held_out) = (0, 0, 0);
        for a in labels["answers"].as_object().unwrap().values() {
            let r = a["r"].as_str().unwrap();
            if r == "skip" {
                skipped += 1;
                continue;
            }
            let di = day_index[a["heads"]["a"]["day"].as_str().unwrap()];
            if let Some(h) = &holdout {
                let keep = if h == "even" { di % 2 == 0 } else { di % 2 == 1 };
                if !keep {
                    held_out += 1;
                    continue;
                }
            }
            match (head_row(&a["heads"]["a"]), head_row(&a["heads"]["b"])) {
                (Some(x), Some(y)) => pairs.push((x, y, r == "same", a["tier"].as_u64().unwrap(), a["d"].as_f64().unwrap() as f32)),
                _ => unmapped += 1,
            }
        }
        eprintln!("{} answers mapped to the app's heads ({} heads by exact ref, the rest by key and overlap), {unmapped} unmapped, {skipped} skipped, {held_out} held out ({:?}); {} days", pairs.len(), exact.get(), holdout, days.len());
        let rows = store.rows();
        let dist = |x: usize, y: usize| 1.0 - rows.dot(x, &rows.row_vec(y));
        let tiers = [1u64, 2, 3, 4];
        // The distance curve per tier (the pair's own head-to-head distance).
        let bands = [(0.0f32, 0.10f32), (0.10, 0.14), (0.14, 0.17), (0.17, 0.20), (0.20, 0.24), (0.24, 0.30), (0.30, 1.0)];
        eprintln!("\nP(same | distance) per tier, asked pairs, the two heads' own distance:");
        for t in tiers {
            let mut line = format!("  tier {t}:");
            for (lo, hi) in bands {
                let inb: Vec<&(usize, usize, bool, u64, f32)> = pairs.iter().filter(|p| p.3 == t && { let d = dist(p.0, p.1); d >= lo && d < hi }).collect();
                let same = inb.iter().filter(|p| p.2).count();
                line += &format!(" [{lo:.2}-{hi:.2}) {same}/{};", inb.len());
            }
            eprintln!("{line}");
        }
        // Find more (strict 0.14) and the suggestion band (0.17): what a pair's own distance gathers.
        eprintln!("\nFind more (d < 0.14) and suggestions (d < 0.17) per tier: same pairs reached / all same (recall), and the share same among the pairs under the cut (precision):");
        for t in tiers {
            let tp: Vec<&(usize, usize, bool, u64, f32)> = pairs.iter().filter(|p| p.3 == t).collect();
            let same_all = tp.iter().filter(|p| p.2).count();
            let mut line = format!("  tier {t}:");
            for cut in [0.14f32, 0.17, 0.20] {
                let under: Vec<&&(usize, usize, bool, u64, f32)> = tp.iter().filter(|p| dist(p.0, p.1) < cut).collect();
                let same_under = under.iter().filter(|p| p.2).count();
                line += &format!(" <{cut:.2}: recall {same_under}/{same_all}, precision {same_under}/{};", under.len());
            }
            eprintln!("{line}");
        }
        // The grouping variants.
        let order = ordered(&store);
        eprintln!("\nGrouping variants: per tier recall = same pairs in one group / same pairs; merged = different pairs in one group / different pairs; then 3+ groups holding a labelled different pair / 3+ groups holding any labelled pair:");
        for (name, cut, linkage) in linkages() {
            let clusters = subjects::cluster_with(&rows, &order, cut, linkage);
            let mut group_of: HashMap<usize, usize> = HashMap::new();
            for (g, c) in clusters.iter().enumerate() {
                for &m in &c.members {
                    group_of.insert(m, g);
                }
            }
            let big: HashSet<usize> = clusters.iter().enumerate().filter(|(_, c)| c.members.len() >= 3).map(|(g, _)| g).collect();
            let mut judged: HashSet<usize> = HashSet::new();
            let mut mixed: HashSet<usize> = HashSet::new();
            let mut line = format!("{name}: {} groups, {} of 3+ heads, largest {:?} |", clusters.len(), big.len(), clusters.iter().take(3).map(|c| c.members.len()).collect::<Vec<_>>());
            let (mut rs, mut rn, mut ms, mut mn) = (0, 0, 0, 0);
            for t in tiers {
                let (mut same_in, mut same_n, mut diff_in, mut diff_n) = (0, 0, 0, 0);
                for p in pairs.iter().filter(|p| p.3 == t) {
                    let (ga, gb) = (group_of.get(&p.0), group_of.get(&p.1));
                    let together = ga.is_some() && ga == gb;
                    if let (Some(&g), true) = (ga, together) {
                        if big.contains(&g) {
                            judged.insert(g);
                            if !p.2 {
                                mixed.insert(g);
                            }
                        }
                    }
                    if p.2 {
                        same_n += 1;
                        if together { same_in += 1; }
                    } else {
                        diff_n += 1;
                        if together { diff_in += 1; }
                    }
                }
                rs += same_in; rn += same_n; ms += diff_in; mn += diff_n;
                line += &format!(" t{t} recall {same_in}/{same_n} merged {diff_in}/{diff_n};");
            }
            line += &format!(" all recall {rs}/{rn} merged {ms}/{mn}; mixed 3+ groups {}/{}", mixed.len(), judged.len());
            eprintln!("{line}");
        }
    }

    /// The cost of a regroup under each candidate linkage at the
    /// cohort's size (`PP_EVAL_OUT`'s head cache, 10 826 heads): wall
    /// time of `step::regroup`'s path on one thread of work, and the
    /// resident size before and after, for the gates (about 2 s, under
    /// about 200 MB extra).
    #[test]
    #[ignore = "needs a head cache from an eval run (PP_EVAL_OUT)"]
    fn fursuit_regroup_cost() {
        fn rss_mb() -> f64 {
            let s = fs::read_to_string("/proc/self/status").unwrap_or_default();
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
        }
        let dir = PathBuf::from(std::env::var("PP_EVAL_OUT").expect("PP_EVAL_OUT")).join("index");
        let mut store = HeadStore::load(&dir);
        store.vectors();
        let order = ordered(&store);
        let rows = store.rows();
        let base = rss_mb();
        eprintln!("{} heads; resident {base:.0} MB with the vectors built", store.refs().len());
        for (name, cut, linkage) in linkages() {
            let before = rss_mb();
            let t = Instant::now();
            let clusters = subjects::cluster_with(&rows, &order, cut, linkage);
            let secs = t.elapsed().as_secs_f64();
            let after = rss_mb();
            eprintln!("{name}: {secs:.2} s, resident {before:.0} → {after:.0} MB (+{:.0}), {} groups, largest {}", after - before, clusters.len(), clusters.first().map_or(0, |c| c.members.len()));
        }
    }

    #[test]
    fn a_face_file_is_not_a_head_file() {
        // The face cache's files have other names and magics: a head
        // store in a folder with faces sees nothing of them.
        let d = scratch("apart");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("faces.bin"), b"PPFACED\0garbage").unwrap();
        fs::write(d.join("facevecs-auraface-v1.bin"), b"PPFACEV\0garbage").unwrap();
        let mut s = HeadStore::load(&d);
        assert_eq!((s.looked_at(), s.vectors().len()), (0, 0));
        assert_eq!(HeadStore::bytes_in(&d), 0, "face bytes are not head bytes");
        let _ = fs::remove_dir_all(&d);
    }

    /// A double that finds one head per photo, or fails as told.
    struct Fake {
        fail: Option<HelperError>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Embedder for Fake {
        fn device_report(&self) -> (Option<String>, SelfCheck) {
            (None, SelfCheck::NotRun)
        }
        fn embed_images(&self, _: &[(PathBuf, FileType)], _: &Want) -> Result<Vec<ImageResult>, HelperError> {
            panic!("a head scan never asks for search vectors or faces")
        }
        fn encode_text(&self, _: &[String]) -> Result<Vec<Vec<f32>>, HelperError> {
            unreachable!()
        }
        fn find_heads(&self, files: &[(PathBuf, FileType)]) -> Result<Vec<HeadsResult>, HelperError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(f) = &self.fail {
                return Err(f.clone());
            }
            Ok(files
                .iter()
                .map(|(p, _)| {
                    let n = p.file_name().unwrap().to_string_lossy().as_bytes()[0];
                    HeadsResult { path: p.clone(), result: Ok(PhotoHeads { key: key(n), heads: vec![head(0.1, 200, &[1.0, 0.0])] }) }
                })
                .collect())
        }
    }

    fn jobs(n: usize) -> Vec<HeadJob> {
        (0..n).map(|i| HeadJob { path: PathBuf::from(format!("/mnt/nas/lib/{}.JPG", (b'a' + i as u8) as char)), file_type: FileType::Jpg, key: key(b'a' + i as u8) }).collect()
    }

    #[test]
    fn a_scan_uses_find_heads_only_and_stops_on_a_card_failure() {
        let fake = Fake { fail: None, calls: AtomicUsize::new(0) };
        let mut batches = Vec::new();
        let fin = execute(Path::new("/"), &jobs(20), &fake, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |b| batches.push(b));
        assert!(fin.complete && fin.looked_at == 20 && fin.failed == 0);
        assert_eq!(fake.calls.load(Ordering::Relaxed), 2, "16 and 4");
        assert_eq!(batches.iter().map(|b| b.found.len()).sum::<usize>(), 20);
        assert_eq!(batches.last().unwrap().done, 20);
        let fake = Fake { fail: Some(HelperError::Card(CardFailure::NoCard)), calls: AtomicUsize::new(0) };
        let mut n = 0;
        let fin = execute(Path::new("/"), &jobs(3), &fake, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |_| n += 1);
        assert_eq!(fin.stopped, Some(Stopped::Card(CardFailure::NoCard)));
        assert!(!fin.complete && n == 1, "one final batch, nothing moved to the processor");
        let cancelled = AtomicBool::new(true);
        let fin = execute(Path::new("/"), &jobs(3), &Fake { fail: None, calls: AtomicUsize::new(0) }, &cancelled, &AtomicUsize::new(0), &mut |_| {});
        assert_eq!(fin.stopped, Some(Stopped::Cancelled));
    }

    #[test]
    fn the_processor_asks_every_time_and_the_card_starts_unasked() {
        let d = scratch("ask");
        let mut f = Fursuits { status: HeadsStatus { on: true, ..HeadsStatus::default() }, ..Fursuits::default() };
        f.open(&d);
        // The card: straight to scanning.
        assert_eq!(f.consider(jobs(5), Why::FirstIndex, Device::Gpu, false), Decision::Start);
        assert!(f.status.scanning());
        f.stop();
        // The processor: an offer, every time, even after an earlier yes.
        // Why::FirstIndex with `on` (the first-index dialog's "Find
        // fursuit heads" ticked) is not a go-ahead either (the user,
        // 2026-10-03): the scan asks when it would start.
        for why in [Why::FirstIndex, Why::NewExports] {
            let d = f.consider(jobs(5), why, Device::Cpu, false);
            assert_eq!(d, Decision::Ask(Offer { photos: 5, why }));
            assert_eq!(f.status.asking(), Some(Offer { photos: 5, why }));
            assert_eq!(f.consider(jobs(2), Why::NewExports, Device::Cpu, false), Decision::Nothing, "one question at a time");
            let (j, w) = f.answer(true).expect("started");
            assert_eq!((j.len(), w), (5, why));
            assert!(f.status.scanning());
            f.stop();
        }
        // Skipped: remembered, counted, and not planned again until a catch-up.
        assert!(matches!(f.consider(jobs(4), Why::NewExports, Device::Cpu, false), Decision::Ask(_)));
        assert!(f.answer(false).is_none());
        assert_eq!((f.status.skipped, f.status.step.clone()), (4, Step::Idle));
        // A catch-up the tab asked for (the cost shown there) starts on the processor too.
        assert_eq!(f.consider(jobs(4), Why::CatchUp, Device::Cpu, true), Decision::Start);
        // Off: nothing, whatever the device.
        f.stop();
        f.status.on = false;
        assert_eq!(f.consider(jobs(4), Why::CatchUp, Device::Gpu, true), Decision::Nothing);
        let _ = fs::remove_dir_all(&d);
    }

    /// The fursuit grouping has no joining step.  Two bunches of heads
    /// whose seeds lie apart (0.34) are pulled together by their later
    /// members until their centroids lie within the cut: people's
    /// grouping then merges them into one group whose ends are far
    /// apart; average linkage keeps them as two, since a head joins only
    /// what it is near on average and nothing merges groups afterwards.
    /// The pass's incremental assign under the same linkage refuses the
    /// far head too.
    #[test]
    fn two_bunches_pulled_together_stay_two_fursuit_groups() {
        use crate::model::subjects::Linkage;
        // Unit vectors in the plane at these angles (cosine distance
        // 1 − cos of the angle between): P at 0, 0, 0.3; Q at 0.85, 0.55, 0.55.
        let angles = [0.0f32, 0.0, 0.85, 0.55, 0.55, 0.3];
        let mut data = Vec::new();
        for a in angles {
            data.extend_from_slice(&[a.cos(), a.sin()]);
        }
        let rows = subjects::Rows::F32 { dim: 2, data: &data };
        let order: Vec<usize> = (0..angles.len()).collect();
        let d = |i: usize, j: usize| 1.0 - rows.dot(i, &rows.row_vec(j));
        assert!(d(0, 2) > 0.3, "the seeds are far apart: {}", d(0, 2));
        let people = subjects::cluster_with(&rows, &order, 0.17, Linkage::PEOPLE);
        assert_eq!(people.len(), 1, "people's merge pass joins the two bunches once their centroids drift within the cut: {people:?}");
        let suits = subjects::cluster_with(&rows, &order, 0.17, Linkage::Average);
        assert_eq!(suits.len(), 2, "average linkage keeps them apart: {suits:?}");
        for c in &suits {
            for &a in &c.members {
                for &b in &c.members {
                    assert!(d(a, b) < 0.2, "no fursuit group holds heads far apart");
                }
            }
        }
        // The pass's incremental assign under the same linkage: the far
        // seed starts its own group rather than drifting into P.
        let mut live = subjects::cluster_with(&rows, &order[..2], 0.17, Linkage::Average);
        subjects::assign_with(&mut live, &rows, &[2], 0.17, Linkage::Average);
        assert_eq!(live.len(), 2, "{live:?}");
        let store = HeadStore::load(&scratch("linkage"));
        assert_eq!(store.linkage(), Linkage::Average, "fursuits group by average linkage");
        assert_eq!(store.cutoffs(), subjects::cutoff_for(ModelId::Dinov2));
        assert_eq!((store.cutoffs().group, store.cutoffs().suggest, store.cutoffs().strict), (0.17, 0.17, 0.14));
    }

    /// Kinds never mix in the fursuit grouping: the step is Kind::Fursuit,
    /// a person named over the same refs counts for nothing here, a
    /// fursuit subject does; suggestions for a person's id are empty; and
    /// the vectors are built only when the grouping asks.
    #[test]
    fn a_fursuit_regroup_sees_its_own_kind_only_and_builds_memory_on_demand() {
        let d = scratch("ownkind");
        let mut f = Fursuits::default();
        f.open(&d);
        {
            let store = f.store.as_mut().unwrap();
            for (n, v) in [(1u8, [1.0f32, 0.0]), (2, [1.0, 0.0]), (3, [0.0, 1.0]), (4, [0.0, 1.0])] {
                store.add(key(n), &[head(0.1, 300, &v)]).unwrap();
            }
            assert!(!store.vectors_loaded(), "opening and writing build no vector memory");
        }
        let refs: Vec<FaceRef> = (1..=4u8).map(|n| f.store.as_ref().unwrap().heads_of(&key(n))[0].head_ref(key(n))).collect();
        let mut file = SubjectsFile::default();
        let person = file.create(Kind::Person, Some("Someone".into()), vec![refs[0], refs[1]]);
        assert!(f.regroup_job(&file).is_some() && f.store.as_ref().unwrap().vectors_loaded(), "the regroup builds the memory");
        assert_eq!(f.store.as_ref().unwrap().kind(), Kind::Fursuit);
        let job = f.regroup_job(&file).unwrap();
        assert_eq!((job.kind, job.dim, job.refs.len()), (Kind::Fursuit, 2, 4));
        assert_eq!(job.cutoffs, subjects::cutoff_for(ModelId::Dinov2));
        assert_eq!((job.cutoffs.group, job.cutoffs.suggest, job.cutoffs.strict), (0.17, 0.17, 0.14), "the measured cut-offs: groups and the review at 0.17, Find more strict at 0.14");
        let r = step::regroup(&job);
        assert!(!r.suggestion_counts.contains_key(&person), "a person is not counted among fursuits");
        assert_eq!(r.clusters.iter().map(|c| c.members.len()).sum::<usize>(), 4, "the person's refs are not confirmed here");
        assert!(f.apply_regroup(r, &file));
        assert_eq!(f.clusters().len(), 2, "two suits of two heads");
        // A regroup of the wrong kind or an old generation is dropped.
        let stale = Regrouped { kind: Kind::Person, generation: job.generation, ..Regrouped::default() };
        assert!(!f.apply_regroup(stale, &file));
        // A fursuit subject with one confirmed head gets its twin suggested; a person's id gets nothing.
        let suit = file.create(Kind::Fursuit, Some("Suit".into()), vec![refs[2]]);
        let resolve = |r: &FaceRef| Some(Face { face: *r, jpg: None, raw: None, day: chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(), capture: chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap().and_hms_opt(10, 0, 0).unwrap(), burst: None, clarity: 0.5, confirmed: false });
        f.step();
        let s = f.suggestions(&file, &GroupId::Subject(suit.clone()), &resolve, true).unwrap();
        assert_eq!(s.iter().flatten().map(|x| x.face).collect::<Vec<_>>(), [refs[3]], "the strict cut finds the identical head");
        // Suggestions come nearest first: a subject with one member and
        // three candidates at 0.02, 0.10 and 0.15 lists them in that order.
        {
            let store = f.store.as_mut().unwrap();
            // Around (−1, 0): far from every head above, so only these four count.
            let v = |t: f32| [(std::f32::consts::PI + t).cos(), (std::f32::consts::PI + t).sin()];
            for (n, t) in [(10u8, 0.0f32), (11, (1.0f32 - 0.15).acos()), (12, (1.0f32 - 0.02).acos()), (13, (1.0f32 - 0.10).acos())] {
                store.add(key(n), &[head(0.2, 300, &v(t))]).unwrap();
            }
        }
        let r10 = f.store.as_ref().unwrap().heads_of(&key(10))[0].head_ref(key(10));
        let near = file.create(Kind::Fursuit, Some("Near".into()), vec![r10]);
        f.step();
        let order: Vec<Key> = f.suggestions(&file, &GroupId::Subject(near), &resolve, false).unwrap().iter().flatten().map(|x| x.face.key).collect();
        assert_eq!(order, [key(12), key(13), key(11)], "nearest first, at 0.02, 0.10, 0.15");
        assert!(f.suggestions(&file, &GroupId::Subject(person.clone()), &resolve, false).unwrap().is_empty(), "a person's suggestions are not answered from heads");
        assert_eq!(f.suggestions(&file, &GroupId::Cluster(999), &resolve, false), Err(Refused::StaleGroup));
        assert!(f.clarity_with_sureness(&refs[0]) > 0.0);
        // Freed after use: quiet for VECTORS_IDLE → the vectors go; the
        // next request builds them again; a running scan keeps them.
        let now = std::time::Instant::now();
        assert!(f.vectors_resident());
        assert!(!f.release_if_idle(now), "just used: kept");
        assert!(f.release_if_idle(now + Fursuits::VECTORS_IDLE), "idle: released");
        assert!(!f.vectors_resident());
        assert!(f.suggestions(&file, &GroupId::Subject(suit.clone()), &resolve, true).unwrap().is_empty(), "nothing built: nothing answered until step()");
        f.step();
        assert!(f.vectors_resident(), "the next request rebuilds");
        assert_eq!(f.suggestions(&file, &GroupId::Subject(suit.clone()), &resolve, true).unwrap().iter().flatten().count(), 1);
        f.status.step = Step::Scanning { why: Why::CatchUp, done: 0, total: 1 };
        assert!(!f.release_if_idle(now + 2 * Fursuits::VECTORS_IDLE), "a scan keeps them");
        f.status.step = Step::Idle;
        f.forget_all();
        f.reset_grouping();
        assert!(f.clusters().is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_batch_grows_the_cache_and_a_card_failure_ends_the_step_failed() {
        let d = scratch("absorb");
        let mut f = Fursuits { status: HeadsStatus { on: true, ..HeadsStatus::default() }, ..Fursuits::default() };
        f.open(&d);
        assert_eq!(f.consider(jobs(3), Why::NewExports, Device::Gpu, false), Decision::Start);
        let fake = Fake { fail: None, calls: AtomicUsize::new(0) };
        let mut batches = Vec::new();
        execute(Path::new("/"), &jobs(3), &fake, &AtomicBool::new(false), &AtomicUsize::new(0), &mut |b| batches.push(b));
        let mut over = false;
        for b in &batches {
            over = f.absorb(b);
        }
        assert!(over && f.status.step == Step::Idle);
        assert_eq!((f.status.scanned, f.status.found), (3, 3));
        let mut fin = HeadBatch::default();
        fin.finished = Some(HeadsFinished { stopped: Some(Stopped::Card(CardFailure::Crashed)), ..HeadsFinished::default() });
        f.status.step = Step::Scanning { why: Why::CatchUp, done: 0, total: 1 };
        f.absorb(&fin);
        assert_eq!(f.status.step, Step::Failed(CardFailure::Crashed));
        assert_eq!(f.consider(jobs(1), Why::NewExports, Device::Gpu, false), Decision::Nothing, "failed: nothing starts");
        f.card_back();
        assert_eq!(f.status.step, Step::Idle);
        f.forget_all();
        assert_eq!((f.status.scanned, f.status.found), (0, 0));
        let _ = fs::remove_dir_all(&d);
    }
}
