//! People's faces (phase 3, plan §5.2 step 5, §6.5, §7a.5): what the
//! engine keeps about faces beside the search index, and the *Subjects*
//! page's state built from it and from the user's file.
//!
//! - The **cache** ([`FaceStore`], recomputable, under the library's index
//!   folder, files readable by the user alone): the detections YuNet found
//!   in each indexed photo (`faces.bin`), the face vectors of the chosen
//!   model (`facevecs-<model>.bin`) and a small tile of each face for the
//!   page (`tiles.bin`).  A face-model change keeps the detections and
//!   the tiles and recomputes the vectors alone.
//! - The **user's file** (`crate::model::subjects::SubjectsFile`): names,
//!   merges, splits, hides and "not this person", pointing at detections.
//! - [`People`]: both together on the engine's thread, with the clusters
//!   derived from them, the undo stack and the snapshot the page shows.
//!
//! Nothing here is written into photos, sidecars or exports (§10).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use super::index::{Key, blocks, framed};
use super::query::{f16_to_f32, f32_to_f16};
use crate::imaging::decode;
use crate::ml::client::{FaceFound, FaceSpot};
use crate::ml::face;
use crate::ml::models::{self, ModelId};
use crate::model::binfile::{Reader, Writer};
use super::step::{self, Grouped, KindStep, RegroupJob, Regrouped};
use crate::model::subjects::{self, EditDest, EditPlan, EditTarget, Face, FaceRef, Framing, Group, GroupEdit, GroupId, Kind, LabelEdit, NameTo, PlanDest, Refused, Subject, SubjectId, Subjects, SubjectsFile, UNDO_DEPTH, box_noun, private_dir, private_file};
use crate::ops::fsutil;

const FACES_MAGIC: &[u8; 8] = b"PPFACED\0";
const FACES_VERSION: u32 = 1;
const VECS_MAGIC: &[u8; 8] = b"PPFACEV\0";
const VECS_VERSION: u32 = 1;
const TILE_MAGIC: u32 = 0x5050_5449;
/// The detector and the canvas rule the detections were made with; a
/// change means the photos are looked at again.
pub const DETECTOR_VERSION: u32 = 1;
/// Dropped entries beyond this share get the files rewritten on load.
const COMPACT_SHARE: f64 = 0.1;

/// A face as the detector found it, kept per photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub bbox: [f32; 4],
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
    pub size_px: f32,
    pub sharpness: f32,
}

impl Detection {
    pub fn face_ref(&self, key: Key) -> FaceRef {
        FaceRef::new(key, self.bbox)
    }

    /// How clear a view this is, before the grouping adds how sure the
    /// match is (UI 12).
    pub fn clarity(&self) -> f32 {
        face::clarity(self.size_px, self.sharpness, self.score)
    }

    pub fn spot(&self) -> FaceSpot {
        FaceSpot { bbox: self.bbox, landmarks: self.landmarks, score: self.score }
    }
}

/// The face vectors of one model, in memory as on disk: f16 bits (a
/// regroup works on an f32 copy of its own).
#[derive(Default)]
pub struct FaceVectors {
    pub dim: usize,
    pub refs: Vec<FaceRef>,
    pub by_ref: HashMap<FaceRef, usize>,
    /// `refs.len() × dim`, row-major, unit rows, as f16 bits.
    pub data: Vec<u16>,
}

impl FaceVectors {
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    /// Row `i` as f32.
    pub fn row(&self, i: usize) -> Vec<f32> {
        self.data[i * self.dim..(i + 1) * self.dim].iter().map(|&h| f16_to_f32(h)).collect()
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
            self.data.extend(v.iter().map(|&x| f32_to_f16(x)));
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

/// Where each face's tile lies in `tiles.bin`, for reading one at a time
/// from any thread (`Engine::crop`).
#[derive(Default)]
pub struct TileIndex {
    path: PathBuf,
    offsets: HashMap<FaceRef, (u64, u32)>,
}

impl TileIndex {
    /// A face's tile (JPEG bytes), read from the file.
    pub fn read(&self, r: &FaceRef) -> Option<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        let &(offset, len) = self.offsets.get(r)?;
        let f = fs::File::open(&self.path).ok()?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact_at(&mut buf, offset).ok()?;
        Some(buf)
    }
}

/// The face cache of one library: detections per photo, vectors per face
/// for the chosen model, tiles.
pub struct FaceStore {
    dir: PathBuf,
    model: ModelId,
    /// Photos looked at, with the faces found in each (maybe none).
    pub scanned: HashMap<Key, Vec<Detection>>,
    /// Faces the user drew in the viewer (the detector missed them), with
    /// the landmarks the helper found for them: cache, like the rest; the
    /// user's own list of them is in the subjects file.
    pub drawn: HashMap<Key, Vec<Detection>>,
    pub vectors: FaceVectors,
    tiles: Arc<Mutex<TileIndex>>,
}

impl FaceStore {
    fn faces_file(dir: &Path) -> PathBuf {
        dir.join("faces.bin")
    }

    fn drawn_file(dir: &Path) -> PathBuf {
        dir.join("faces-drawn.bin")
    }

    fn vectors_file(dir: &Path, model: ModelId) -> PathBuf {
        dir.join(format!("facevecs-{}.bin", model.dir_name()))
    }

    fn tiles_file(dir: &Path) -> PathBuf {
        dir.join("tiles.bin")
    }

    /// Load the cache in `dir` for `model` (empty where there is nothing,
    /// or files of another detector or model).
    pub fn load(dir: &Path, model: ModelId) -> FaceStore {
        let mut store = FaceStore { dir: dir.to_path_buf(), model, scanned: HashMap::new(), drawn: HashMap::new(), vectors: FaceVectors::default(), tiles: Arc::new(Mutex::new(TileIndex { path: Self::tiles_file(dir), offsets: HashMap::new() })) };
        store.scanned = Self::read_detections(&Self::faces_file(dir));
        store.drawn = Self::read_detections(&Self::drawn_file(dir));
        store.load_vectors();
        store.load_tiles();
        store
    }

    /// A detections file (`faces.bin`, or the drawn faces'), by photo;
    /// later blocks for a photo replace earlier ones.
    fn read_detections(path: &Path) -> HashMap<Key, Vec<Detection>> {
        let mut out = HashMap::new();
        if let Ok(data) = fs::read(path) {
            let bs = blocks(&data);
            if bs.first().is_some_and(|h| Self::read_faces_header(h)) {
                for b in &bs[1..] {
                    let Some(entries) = Self::read_faces_block(b) else { break };
                    for (k, d) in entries {
                        out.insert(k, d);
                    }
                }
            }
        }
        out
    }

    fn load_vectors(&mut self) {
        self.vectors = FaceVectors::default();
        let Ok(data) = fs::read(Self::vectors_file(&self.dir, self.model)) else { return };
        let bs = blocks(&data);
        let Some(dim) = bs.first().and_then(|h| Self::read_vectors_header(h, self.model)) else { return };
        self.vectors.dim = dim;
        for b in &bs[1..] {
            let Some(rows) = FaceVectors::read_block(b, dim) else { break };
            self.vectors.push(&rows);
        }
    }

    fn load_tiles(&mut self) {
        let mut index = self.tiles.lock();
        index.offsets.clear();
        let Ok(data) = fs::read(&index.path) else { return };
        let mut i = 0usize;
        while i + 32 <= data.len() {
            let magic = u32::from_le_bytes(data[i..i + 4].try_into().expect("4 bytes"));
            if magic != TILE_MAGIC {
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

    fn faces_header() -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(FACES_MAGIC);
        w.u32(FACES_VERSION);
        w.u32(DETECTOR_VERSION);
        w.seal()
    }

    fn read_faces_header(data: &[u8]) -> bool {
        let Some(mut r) = Reader::open(data) else { return false };
        r.take(8) == Some(FACES_MAGIC) && r.u32() == Some(FACES_VERSION) && r.u32() == Some(DETECTOR_VERSION) && r.done()
    }

    fn faces_block(entries: &[(Key, Vec<Detection>)]) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u32(entries.len() as u32);
        for (k, dets) in entries {
            w.0.extend_from_slice(&k.0);
            w.u16(dets.len() as u16);
            for d in dets {
                for v in d.bbox {
                    w.u32(v.to_bits());
                }
                for lm in d.landmarks {
                    w.u32(lm[0].to_bits());
                    w.u32(lm[1].to_bits());
                }
                w.u32(d.score.to_bits());
                w.u32(d.size_px.to_bits());
                w.u32(d.sharpness.to_bits());
            }
        }
        w.seal()
    }

    fn read_faces_block(data: &[u8]) -> Option<Vec<(Key, Vec<Detection>)>> {
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
                let mut landmarks = [[0f32; 2]; 5];
                for lm in &mut landmarks {
                    *lm = [f(&mut r)?, f(&mut r)?];
                }
                dets.push(Detection { bbox, landmarks, score: f(&mut r)?, size_px: f(&mut r)?, sharpness: f(&mut r)? });
            }
            out.push((key, dets));
        }
        r.done().then_some(out)
    }

    fn vectors_header(model: ModelId, dim: usize) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(VECS_MAGIC);
        w.u32(VECS_VERSION);
        w.bytes(model.dir_name().as_bytes());
        w.bytes(models::info(model).weights_sha256().as_bytes());
        w.u32(dim as u32);
        w.seal()
    }

    fn read_vectors_header(data: &[u8], model: ModelId) -> Option<usize> {
        let mut r = Reader::open(data)?;
        if r.take(8)? != VECS_MAGIC || r.u32()? != VECS_VERSION {
            return None;
        }
        if r.bytes()? != model.dir_name().as_bytes() || r.bytes()? != models::info(model).weights_sha256().as_bytes() {
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

    /// Switch to another model's vectors (the detections and tiles stay).
    pub fn set_model(&mut self, model: ModelId) {
        if model != self.model {
            self.model = model;
            self.load_vectors();
        }
    }

    /// Record a photo looked at, with the faces found (maybe none): the
    /// detections, their vectors and their tiles.
    pub fn add(&mut self, key: Key, faces: &[FaceFound]) -> io::Result<()> {
        let dets: Vec<Detection> = faces.iter().map(|f| Detection { bbox: f.bbox, landmarks: f.landmarks, score: f.score, size_px: f.size_px, sharpness: f.sharpness }).collect();
        Self::append(&Self::faces_file(&self.dir), &Self::faces_header(), &Self::faces_block(&[(key, dets.clone())]))?;
        self.scanned.insert(key, dets);
        self.add_vectors(key, faces)?;
        self.add_tiles(key, faces)
    }

    /// Record faces the user drew (the helper's landmarks, vector and
    /// tile for each): kept beside the detections, never mixed with what
    /// the detector found, so looking at the photo again keeps them.
    pub fn add_drawn(&mut self, key: Key, faces: &[FaceFound]) -> io::Result<()> {
        if faces.is_empty() {
            return Ok(());
        }
        let mut dets = self.drawn.get(&key).cloned().unwrap_or_default();
        for f in faces {
            let r = FaceRef::new(key, f.bbox);
            dets.retain(|d| d.face_ref(key) != r);
            dets.push(Detection { bbox: f.bbox, landmarks: f.landmarks, score: f.score, size_px: f.size_px, sharpness: f.sharpness });
        }
        Self::append(&Self::drawn_file(&self.dir), &Self::faces_header(), &Self::faces_block(&[(key, dets.clone())]))?;
        self.drawn.insert(key, dets);
        self.add_vectors(key, faces)?;
        self.add_tiles(key, faces)
    }

    /// The user drew this face (as against the detector finding it).
    pub fn is_drawn(&self, r: &FaceRef) -> bool {
        self.drawn.get(&r.key).is_some_and(|d| d.iter().any(|d| d.face_ref(r.key) == *r))
    }

    /// The face has a vector under the current model.
    pub fn has_vector(&self, r: &FaceRef) -> bool {
        self.vectors.by_ref.contains_key(r)
    }

    /// Record faces' vectors (a model change: the detections are known).
    pub fn add_vectors(&mut self, key: Key, faces: &[FaceFound]) -> io::Result<()> {
        let rows: Vec<(FaceRef, Vec<f32>)> = faces.iter().filter(|f| !f.vector.is_empty()).map(|f| (FaceRef::new(key, f.bbox), f.vector.clone())).filter(|(r, _)| !self.vectors.by_ref.contains_key(r)).collect();
        if rows.is_empty() {
            return Ok(());
        }
        let dim = rows[0].1.len();
        Self::append(&Self::vectors_file(&self.dir, self.model), &Self::vectors_header(self.model, dim), &FaceVectors::block(&rows))?;
        self.vectors.push(&rows);
        Ok(())
    }

    fn add_tiles(&mut self, key: Key, faces: &[FaceFound]) -> io::Result<()> {
        let mut index = self.tiles.lock();
        let new: Vec<(FaceRef, &[u8])> = faces.iter().filter(|f| !f.tile.is_empty()).map(|f| (FaceRef::new(key, f.bbox), f.tile.as_slice())).filter(|(r, _)| !index.offsets.contains_key(r)).collect();
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
        let mut buf = Vec::new();
        for (r, jpeg) in new {
            buf.clear();
            buf.extend_from_slice(&TILE_MAGIC.to_le_bytes());
            buf.extend_from_slice(&r.key.0);
            for t in r.thousandths() {
                buf.extend_from_slice(&t.to_le_bytes());
            }
            buf.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
            buf.extend_from_slice(jpeg);
            f.write_all(&buf)?;
            index.offsets.insert(r, (at + 32, jpeg.len() as u32));
            at += buf.len() as u64;
        }
        f.sync_data()
    }

    /// Photos whose faces lack vectors under the current model, with the
    /// faces to recognise again (a model change).
    pub fn needing_vectors(&self) -> Vec<(Key, Vec<FaceSpot>)> {
        // Drawn faces too: they have their landmarks, so a model change
        // recognises them again with the rest.
        let mut by_key: HashMap<Key, Vec<FaceSpot>> = HashMap::new();
        for (k, dets) in self.scanned.iter().chain(self.drawn.iter()) {
            for d in dets.iter().filter(|d| !self.vectors.by_ref.contains_key(&d.face_ref(*k))) {
                by_key.entry(*k).or_default().push(d.spot());
            }
        }
        let mut out: Vec<(Key, Vec<FaceSpot>)> = by_key.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The faces the detector found in a photo (not the drawn ones).
    pub fn faces_of(&self, key: &Key) -> &[Detection] {
        self.scanned.get(key).map_or(&[], Vec::as_slice)
    }

    /// The faces the user drew in a photo.
    pub fn drawn_of(&self, key: &Key) -> &[Detection] {
        self.drawn.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn detection(&self, r: &FaceRef) -> Option<&Detection> {
        self.faces_of(&r.key).iter().chain(self.drawn_of(&r.key)).find(|d| d.face_ref(r.key) == *r)
    }

    /// Photos looked at.
    pub fn looked_at(&self) -> usize {
        self.scanned.len()
    }

    /// Faces found.
    pub fn found(&self) -> usize {
        self.scanned.values().map(Vec::len).sum()
    }

    /// Faces without a vector under the current model.
    pub fn unrecognised(&self) -> usize {
        self.scanned.iter().flat_map(|(k, d)| d.iter().map(move |d| d.face_ref(*k))).filter(|r| !self.vectors.by_ref.contains_key(r)).count()
    }

    /// Every face, found or drawn, as (ref, detection).
    pub fn all(&self) -> impl Iterator<Item = (FaceRef, &Detection)> {
        self.scanned.iter().chain(self.drawn.iter()).flat_map(|(k, dets)| dets.iter().map(move |d| (d.face_ref(*k), d)))
    }

    /// The tile index, shared with `Engine::crop` on other threads.
    pub fn tiles(&self) -> Arc<Mutex<TileIndex>> {
        self.tiles.clone()
    }

    /// Keep only the photos in `live` (the library's), rewriting the files
    /// when many went.
    pub fn retain(&mut self, live: &HashSet<Key>) {
        let before = self.scanned.len();
        self.scanned.retain(|k, _| live.contains(k));
        let drawn = self.drawn.len();
        self.drawn.retain(|k, _| live.contains(k));
        let dropped = before - self.scanned.len();
        if (dropped > 0 && dropped as f64 > COMPACT_SHARE * before as f64) || self.drawn.len() < drawn {
            if let Err(e) = self.rewrite() {
                eprintln!("faces: couldn't compact the face cache: {e}");
            }
        }
    }

    /// Rewrite the three files from memory.
    fn rewrite(&mut self) -> io::Result<()> {
        let dir = self.dir.clone();
        fs::create_dir_all(&dir)?;
        private_dir(&dir);
        // Detections.
        let mut out = framed(&Self::faces_header());
        let entries: Vec<(Key, Vec<Detection>)> = self.scanned.iter().map(|(k, d)| (*k, d.clone())).collect();
        for chunk in entries.chunks(1024) {
            out.extend(framed(&Self::faces_block(chunk)));
        }
        fsutil::atomic_write(&Self::faces_file(&dir), &out, true)?;
        private_file(&Self::faces_file(&dir));
        // The drawn faces.
        let drawn: Vec<(Key, Vec<Detection>)> = self.drawn.iter().map(|(k, d)| (*k, d.clone())).collect();
        if drawn.is_empty() {
            let _ = fs::remove_file(Self::drawn_file(&dir));
        } else {
            let mut out = framed(&Self::faces_header());
            for chunk in drawn.chunks(1024) {
                out.extend(framed(&Self::faces_block(chunk)));
            }
            fsutil::atomic_write(&Self::drawn_file(&dir), &out, true)?;
            private_file(&Self::drawn_file(&dir));
        }
        // Vectors of the live faces.
        let live: HashSet<FaceRef> = self.all().map(|(r, _)| r).collect();
        let rows: Vec<(FaceRef, Vec<f32>)> = (0..self.vectors.len()).filter(|&i| live.contains(&self.vectors.refs[i])).map(|i| (self.vectors.refs[i], self.vectors.row(i))).collect();
        let vfile = Self::vectors_file(&dir, self.model);
        if rows.is_empty() {
            let _ = fs::remove_file(&vfile);
        } else {
            let mut out = framed(&Self::vectors_header(self.model, self.vectors.dim));
            for chunk in rows.chunks(1024) {
                out.extend(framed(&FaceVectors::block(chunk)));
            }
            fsutil::atomic_write(&vfile, &out, true)?;
            private_file(&vfile);
        }
        self.vectors = FaceVectors::default();
        self.vectors.push(&rows);
        // Tiles of the live faces.
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
            buf.extend_from_slice(&TILE_MAGIC.to_le_bytes());
            buf.extend_from_slice(&r.key.0);
            for t in r.thousandths() {
                buf.extend_from_slice(&t.to_le_bytes());
            }
            buf.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
            buf.extend_from_slice(jpeg);
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
        let _ = fs::remove_file(Self::faces_file(&self.dir));
        let _ = fs::remove_file(Self::drawn_file(&self.dir));
        let _ = fs::remove_file(Self::tiles_file(&self.dir));
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("facevecs-") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        self.scanned.clear();
        self.drawn.clear();
        self.vectors = FaceVectors::default();
        self.tiles.lock().offsets.clear();
    }

    /// Bytes the face files in an index folder take (the Storage row's
    /// *People*; a stat walk, on a worker).
    pub fn bytes_in(dir: &Path) -> u64 {
        let mut n = 0;
        for p in [Self::faces_file(dir), Self::drawn_file(dir), Self::tiles_file(dir)] {
            n += fs::metadata(p).map_or(0, |m| m.len());
        }
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("facevecs-") {
                    n += e.metadata().map_or(0, |m| m.len());
                }
            }
        }
        n
    }
}

impl KindStep for FaceStore {
    fn kind(&self) -> Kind {
        Kind::Person
    }

    fn refs(&self) -> &[FaceRef] {
        &self.vectors.refs
    }

    fn rows(&self) -> subjects::Rows<'_> {
        subjects::Rows::F16 { dim: self.vectors.dim, data: &self.vectors.data }
    }

    fn clarity(&self, r: &FaceRef) -> f32 {
        self.detection(r).map_or(0.0, Detection::clarity)
    }

    fn cutoffs(&self) -> subjects::Cutoffs {
        subjects::cutoff_for(self.model)
    }

    fn is_added(&self, r: &FaceRef) -> bool {
        self.is_drawn(r)
    }
}

/// The people state the engine's thread owns.
pub struct People {
    /// Where the user's file lives (`subjects::default_path`, or a test's).
    pub file_path: Option<PathBuf>,
    pub file: SubjectsFile,
    /// The face cache, once the library's index folder is known.
    pub store: Option<FaceStore>,
    /// The people's clusters and sureness, from the last regroup and the
    /// faces assigned since.
    pub grouped: Grouped,
    /// Other kinds' automatic clusters (fursuits, phase 4), by cluster
    /// id, as their steps last published them: so an operation on such a
    /// group finds its members and its kind here, and the kinds check
    /// refuses a mix.  Never people's rows.
    pub other_clusters: HashMap<u64, (Kind, Vec<FaceRef>)>,
    /// Earlier states of the file with what changed them, newest last,
    /// and the decision record's entry for the change (its id and kind),
    /// when one was written.
    undo: Vec<(SubjectsFile, String, Option<(String, Kind)>)>,
    /// The snapshot last built.
    pub snapshot: Arc<Subjects>,
    /// Moves on with every regroup: cluster ids are valid within one.
    pub generation: u64,
}

impl People {
    /// Load the user's file (none is empty).
    pub fn load(file_path: Option<PathBuf>) -> People {
        let file = file_path.as_deref().map(SubjectsFile::load).unwrap_or_default();
        People { file_path, file, store: None, grouped: Grouped::default(), other_clusters: HashMap::new(), undo: Vec::new(), snapshot: Arc::new(Subjects::default()), generation: 1 }
    }

    /// A regroup's input for the worker, stamped with the next generation.
    pub fn regroup_job(&mut self) -> Option<RegroupJob> {
        let store = self.store.as_ref()?;
        if store.vectors.len() == 0 {
            return None;
        }
        self.generation += 1;
        Some(step::job(store, &self.file, self.generation))
    }

    /// Take a regroup's result (an older generation's is dropped), then
    /// assign the faces that arrived meanwhile.
    pub fn apply_regroup(&mut self, r: Regrouped) -> bool {
        if r.generation != self.generation || r.kind != Kind::Person {
            return false;
        }
        let Some(store) = &self.store else { return false };
        self.grouped.apply(r, store.refs(), &|f| store.clarity(f));
        self.grouped.assign_new(store, &self.file);
        true
    }

    /// Faces that arrived since the last regroup join the clusters.
    pub fn assign_new(&mut self) {
        if let Some(store) = &self.store {
            self.grouped.assign_new(store, &self.file);
        }
    }

    /// The grouping starts over (the vectors were rebuilt).
    pub fn reset_grouping(&mut self) {
        self.grouped = Grouped::default();
        self.generation += 1;
    }

    /// Record the state before an edit, so [`People::undo`] can restore
    /// it; `what` is the undo's description ("Named a group Someone").
    fn checkpoint(&mut self, what: impl Into<String>) {
        self.undo.push((self.file.clone(), what.into(), None));
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// Take the last edit back; returns what was undone.
    #[cfg(test)]
    pub fn undo(&mut self) -> Option<String> {
        self.undo_recorded().map(|(what, _)| what)
    }

    /// [`People::undo`], with the decision record's entry for the change
    /// taken back (its id and kind), if one was written.
    pub fn undo_recorded(&mut self) -> Option<(String, Option<(String, Kind)>)> {
        let (file, what, recorded) = self.undo.pop()?;
        self.file = file;
        self.save();
        Some((what, recorded))
    }

    /// The decision record's entry for the last change (the newest undo
    /// step), so its undo can name it.
    pub fn mark_recorded(&mut self, id: String, kind: Kind) {
        if let Some(last) = self.undo.last_mut() {
            last.2 = Some((id, kind));
        }
    }

    /// Write the file (whole, atomically, private).
    pub fn save(&self) {
        if let Some(p) = &self.file_path {
            if let Err(e) = self.file.save(p) {
                eprintln!("subjects: couldn't save {}: {e}", p.display());
            }
        }
    }

    /// The subject behind a group id; a cluster is not a subject.
    pub fn subject_of(&self, id: &GroupId) -> Option<&Subject> {
        match id {
            GroupId::Subject(s) => self.file.get(s),
            GroupId::Cluster(_) => None,
        }
    }

    /// The kind of a group, by its id.
    fn kind_of(&self, id: &GroupId) -> Option<Kind> {
        match id {
            GroupId::Subject(s) => self.file.get(s).map(|s| s.kind),
            GroupId::Cluster(c) => match self.other_clusters.get(c) {
                Some((kind, _)) => Some(*kind),
                None => self.cluster_members(id).map(|_| Kind::Person),
            },
        }
    }

    /// Replace the clusters published for another kind (its step's
    /// current generation); people's own clusters are never put here.
    pub fn set_other_clusters(&mut self, kind: Kind, clusters: Vec<(u64, Vec<FaceRef>)>) {
        debug_assert!(kind != Kind::Person);
        self.other_clusters.retain(|_, (k, _)| *k != kind);
        for (id, members) in clusters {
            self.other_clusters.insert(id, (kind, members));
        }
    }

    /// A cluster's faces for the current generation (none: stale or
    /// unknown).
    fn cluster_members(&self, id: &GroupId) -> Option<Vec<FaceRef>> {
        let GroupId::Cluster(id) = id else { return None };
        if let Some((_, members)) = self.other_clusters.get(id) {
            return Some(members.clone());
        }
        let store = self.store.as_ref()?;
        let c = self.grouped.cluster(*id)?;
        Some(c.members.iter().filter_map(|&m| store.refs().get(m).copied()).collect())
    }

    /// A group's confirmed or clustered faces.
    pub fn members(&self, id: &GroupId) -> Result<(Kind, Vec<FaceRef>), Refused> {
        match id {
            GroupId::Subject(s) => {
                let s = self.file.get(s).ok_or_else(|| Refused::NoSuchSubject(s.clone()))?;
                Ok((s.kind, s.members.clone()))
            }
            // A cluster's kind is whichever step published it (people's
            // own, or another kind's through `other_clusters`).
            GroupId::Cluster(_) => match (self.kind_of(id), self.cluster_members(id)) {
                (Some(kind), Some(m)) => Ok((kind, m)),
                _ => Err(Refused::StaleGroup),
            },
        }
    }

    /// A person's group's faces farthest from the group first (the two
    /// groups side by side order both kinds so), without the faces the
    /// viewer's labels keep out.  `None` for anything but a person's group.
    pub fn far_order(&self, id: &GroupId) -> Option<Vec<(FaceRef, f32)>> {
        let (kind, members) = self.members(id).ok()?;
        if kind != Kind::Person {
            return None;
        }
        let store = self.store.as_ref()?;
        let out = step::Exclusion::new(&self.file, Kind::Person);
        let refs = store.refs();
        let by_ref: HashMap<FaceRef, usize> = refs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let rows: Vec<usize> = members.iter().filter(|f| !out.excludes(store, f)).filter_map(|f| by_ref.get(f).copied()).collect();
        Some(crate::search::splitting::far_order(&store.rows(), &rows).into_iter().map(|(r, d)| (refs[r], d)).collect())
    }

    /// The group of `kind` holding a face or head now: the subject it is
    /// confirmed to, else the automatic group it sits in.  (A move into an
    /// automatic group makes a subject of it, with a new id: the two groups
    /// side by side follow their heads.)
    pub fn group_of(&self, kind: Kind, f: &FaceRef) -> Option<GroupId> {
        if let Some(s) = self.file.member_of(kind, f) {
            return Some(GroupId::Subject(s.id.clone()));
        }
        if let Some((id, _)) = self.other_clusters.iter().find(|(_, (k, m))| *k == kind && m.contains(f)) {
            return Some(GroupId::Cluster(*id));
        }
        if kind != Kind::Person {
            return None;
        }
        let store = self.store.as_ref()?;
        let row = store.refs().iter().position(|r| r == f)?;
        let i = self.grouped.clusters.iter().position(|c| c.members.contains(&row))?;
        self.grouped.ids.get(i).map(|&c| GroupId::Cluster(c))
    }

    /// Name a group: a subject is renamed; a cluster becomes a subject
    /// with the cluster's faces as its members.
    pub fn name_group(&mut self, id: &GroupId, name: String) -> Result<SubjectId, Refused> {
        let (kind, members) = self.members(id)?;
        match id {
            GroupId::Subject(s) => {
                self.checkpoint(match self.file.get(s).and_then(|s| s.name.clone()) {
                    Some(old) => format!("Renamed {old} to {name}"),
                    None => format!("Named a group {name}"),
                });
                self.file.rename(s, name)?;
                self.save();
                Ok(s.clone())
            }
            GroupId::Cluster(_) => {
                self.checkpoint(format!("Named a group {name}"));
                let id = self.file.create(kind, Some(name), members);
                self.save();
                Ok(id)
            }
        }
    }

    /// Merge groups into one (all of one kind, else refused): clusters
    /// become members of `into`, which becomes a subject if it was a
    /// cluster.
    pub fn merge(&mut self, into: &GroupId, others: &[GroupId]) -> Result<SubjectId, Refused> {
        let kind = self.kind_of(into).ok_or(Refused::StaleGroup)?;
        let others: Vec<&GroupId> = others.iter().filter(|o| *o != into).collect();
        if others.is_empty() {
            return Err(Refused::Nothing);
        }
        for o in &others {
            if self.kind_of(o).ok_or(Refused::StaleGroup)? != kind {
                return Err(Refused::MixedKinds);
            }
        }
        let target_name = self.subject_of(into).and_then(|s| s.name.clone()).or_else(|| others.iter().find_map(|o| self.subject_of(o).and_then(|s| s.name.clone())));
        self.checkpoint(match &target_name {
            Some(n) => format!("Merged {} groups into {n}", others.len() + 1),
            None => format!("Merged {} groups", others.len() + 1),
        });
        let target = match into {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => {
                let (_, members) = self.members(into)?;
                self.file.create(kind, None, members)
            }
        };
        for o in others {
            match o {
                GroupId::Subject(s) => self.file.merge(&target, s)?,
                GroupId::Cluster(_) => {
                    let (_, members) = self.members(o)?;
                    let _ = self.file.add_members(&target, members);
                }
            }
        }
        self.save();
        Ok(target)
    }

    /// "Not the same": two groups of one kind (else refused) that a merge
    /// suggestion offered are different subjects.  Kept by their heads
    /// (`a_heads`, `b_heads`: clearest first, as the suggestion showed
    /// them; their members when empty), so the pair isn't offered again
    /// after a regroup.  One undo step; nothing is merged or moved.
    pub fn not_same(&mut self, a: &GroupId, b: &GroupId, a_heads: Vec<FaceRef>, b_heads: Vec<FaceRef>) -> Result<(), Refused> {
        if a == b {
            return Err(Refused::Nothing);
        }
        let (ka, ma) = self.members(a)?;
        let (kb, mb) = self.members(b)?;
        if ka != kb {
            return Err(Refused::MixedKinds);
        }
        // The heads shown first, then the rest of the members.
        let side = |shown: Vec<FaceRef>, members: Vec<FaceRef>| -> Vec<FaceRef> { shown.into_iter().filter(|f| members.contains(f)).chain(members.iter().copied()).collect() };
        let (sa, sb) = (side(a_heads, ma), side(b_heads, mb));
        let label = |id: &GroupId| self.subject_of(id).and_then(|s| s.name.clone());
        self.checkpoint(match (label(a), label(b)) {
            (Some(x), Some(y)) => format!("Said {x} and {y} are not the same"),
            (Some(x), None) | (None, Some(x)) => format!("Said a group is not {x}"),
            (None, None) => "Said two groups are not the same".to_string(),
        });
        if let Err(e) = self.file.add_not_same(ka, sa, sb) {
            self.undo.pop();
            return Err(e);
        }
        self.save();
        Ok(())
    }

    /// Dissolve a group: it isn't one person or one fursuit, only
    /// lookalikes.  Its heads (those not removed in the viewer) leave it:
    /// a subject is forgotten (its name, confirmed heads and answers go),
    /// and the heads are kept apart from each other in the automatic
    /// groups from then on (`SubjectsFile::dissolved`), through every
    /// regroup.  Each stays a detection of its kind, free to be grouped
    /// with others, suggested for a group or found by *Find more*.  One
    /// undo step, which puts the group back as it was.  The words for
    /// the status line, and the heads.
    pub fn dissolve(&mut self, id: &GroupId) -> Result<(String, Vec<FaceRef>), Refused> {
        let (kind, members) = self.members(id)?;
        let removed = self.file.removed_index(kind);
        let heads: Vec<FaceRef> = members.into_iter().filter(|h| !removed.covers(h)).collect();
        let subject = matches!(id, GroupId::Subject(_));
        if heads.len() < 2 && !subject {
            return Err(Refused::Nothing);
        }
        let what = match self.subject_of(id).and_then(|s| s.name.clone()) {
            Some(n) => format!("Dissolved {n}"),
            None => format!("Dissolved a group of {} {}", heads.len(), plural(heads.len(), box_noun(kind))),
        };
        self.checkpoint(what.clone());
        if let GroupId::Subject(s) = id {
            if let Err(e) = self.file.forget(s) {
                self.undo.pop();
                return Err(e);
            }
        }
        if heads.len() >= 2 {
            self.file.add_dissolved(kind, heads.clone())?;
        }
        self.save();
        Ok((what, heads))
    }

    /// *Reset unnamed groups* (Preferences ▸ Fursuits): every unnamed
    /// subject of `kind` goes ([`SubjectsFile::reset_unnamed`]), so the
    /// heads it pinned together are grouped afresh by the next regroup.
    /// Named subjects, the other kinds and every answer stay.  One undo
    /// step, which puts the file back exactly.  The words for the status
    /// line, and the subjects taken.
    pub fn reset_unnamed(&mut self, kind: Kind) -> Result<(String, Vec<Subject>), Refused> {
        let u = self.file.unnamed(kind);
        if u.groups == 0 {
            return Err(Refused::Nothing);
        }
        let what = format!("Reset {} unnamed {} {}", u.groups, kind.noun(), plural(u.groups, "group"));
        self.checkpoint(what.clone());
        let gone = self.file.reset_unnamed(kind);
        self.save();
        Ok((what, gone))
    }

    /// The automatic groups of one dissolved head on its own, with their
    /// kind and size (1): what a dissolve mostly leaves.  The tab doesn't
    /// show them as groups; it counts them.  A dissolved head that joins
    /// other heads is in an ordinary group, shown as any other.
    pub fn loose_groups(&self) -> Vec<(GroupId, Kind, usize)> {
        let mut out = Vec::new();
        if self.file.dissolved.is_empty() {
            return out;
        }
        let alone = |d: &HashSet<FaceRef>, m: &[FaceRef]| matches!(m, [one] if d.contains(one));
        let people = self.file.dissolved_heads(Kind::Person);
        if let (false, Some(store)) = (people.is_empty(), &self.store) {
            let refs = store.refs();
            for (c, &id) in self.grouped.clusters.iter().zip(&self.grouped.ids) {
                let m: Vec<FaceRef> = c.members.iter().filter_map(|&r| refs.get(r).copied()).collect();
                if alone(&people, &m) {
                    out.push((GroupId::Cluster(id), Kind::Person, m.len()));
                }
            }
        }
        let suits = self.file.dissolved_heads(Kind::Fursuit);
        if !suits.is_empty() {
            for (id, (k, m)) in &self.other_clusters {
                if *k == Kind::Fursuit && alone(&suits, m) {
                    out.push((GroupId::Cluster(*id), Kind::Fursuit, m.len()));
                }
            }
        }
        out
    }

    /// Hide or show a group (a hidden cluster becomes an unnamed subject,
    /// so the hide outlives a regroup).
    pub fn set_hidden(&mut self, id: &GroupId, hidden: bool) -> Result<SubjectId, Refused> {
        let (kind, members) = self.members(id)?;
        let name = self.subject_of(id).and_then(|s| s.name.clone());
        self.checkpoint(match (hidden, name) {
            (true, Some(n)) => format!("Hid {n}"),
            (true, None) => "Hid a group".to_string(),
            (false, Some(n)) => format!("Showed {n}"),
            (false, None) => "Showed a group".to_string(),
        });
        let sid = match id {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => self.file.create(kind, None, members),
        };
        self.file.set_hidden(&sid, hidden)?;
        self.save();
        Ok(sid)
    }

    /// The group photo of a named group: one of its faces (`None`: the
    /// automatic pick again).  One undo step; refused for an unnamed
    /// group, a face it doesn't hold, or no change.  Nothing regroups.
    pub fn set_cover(&mut self, id: &SubjectId, face: Option<FaceRef>) -> Result<String, Refused> {
        let name = self.file.get(id).ok_or_else(|| Refused::NoSuchSubject(id.clone()))?.name.clone().unwrap_or_default();
        let what = subjects::cover_words(&name, face.is_some());
        self.checkpoint(what.clone());
        if let Err(e) = self.file.set_cover(id, face) {
            self.undo.pop();
            return Err(e);
        }
        self.save();
        Ok(what)
    }

    /// Forget a subject: its faces become unnamed again.
    pub fn forget(&mut self, id: &SubjectId) -> Result<(), Refused> {
        let name = self.file.get(id).and_then(|s| s.name.clone());
        self.checkpoint(match name {
            Some(n) => format!("Forgot {n}"),
            None => "Forgot a group".to_string(),
        });
        self.file.forget(id)?;
        self.save();
        Ok(())
    }

    /// Move faces out of a group: to another group of the same kind, or
    /// to a new unnamed subject (`None`).  From a cluster, the faces are
    /// confirmed into the destination; the rest of the cluster stays
    /// where the grouping puts it.
    #[cfg(test)]
    pub fn move_faces(&mut self, from: &GroupId, faces: Vec<FaceRef>, to: Option<&GroupId>) -> Result<SubjectId, Refused> {
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        let kind = self.kind_of(from).ok_or(Refused::StaleGroup)?;
        if let Some(t) = to {
            if self.kind_of(t).ok_or(Refused::StaleGroup)? != kind {
                return Err(Refused::MixedKinds);
            }
            if t == from {
                return Err(Refused::Nothing);
            }
        }
        let n = faces.len();
        let to_name = to.and_then(|t| self.subject_of(t)).and_then(|s| s.name.clone());
        self.checkpoint(match to_name {
            Some(name) => format!("Moved {n} {} to {name}", plural(n, "face")),
            None => format!("Moved {n} {} to a new group", plural(n, "face")),
        });
        let target = match to {
            Some(GroupId::Subject(s)) => s.clone(),
            Some(c @ GroupId::Cluster(_)) => {
                let (_, members) = self.members(c)?;
                self.file.create(kind, None, members)
            }
            None => self.file.create(kind, None, Vec::new()),
        };
        self.file.add_members(&target, faces)?;
        self.save();
        Ok(target)
    }

    /// The group editor's edit, resolved against the groups as they are
    /// now: each head's group (`group_of`), each target's (a subject by
    /// its id; an automatic group by the heads it holds, as `locate`
    /// finds them).  Heads the viewer's labels removed, and moves to where
    /// a head already is, drop out.  Refused only when a subject it sends
    /// heads to (or renames) is gone, or is of another kind; an automatic
    /// target that is gone becomes a new group of its own.
    pub fn plan_edit(&self, e: &GroupEdit) -> Result<EditPlan, Refused> {
        let kind = e.kind;
        let removed = self.file.removed_index(kind);
        let mut new_names = e.new_names.clone();
        let mut gone = 0;
        let mut targets: Vec<PlanDest> = Vec::with_capacity(e.targets.len());
        for t in &e.targets {
            targets.push(match t {
                EditTarget::Subject(s) => {
                    let sub = self.file.get(s).ok_or_else(|| Refused::NoSuchSubject(s.clone()))?;
                    if sub.kind != kind {
                        return Err(Refused::MixedKinds);
                    }
                    PlanDest::Group(GroupId::Subject(s.clone()))
                }
                EditTarget::ClusterOf(heads) => {
                    let mut count: HashMap<GroupId, usize> = HashMap::new();
                    for g in heads.iter().filter_map(|h| self.group_of(kind, h)) {
                        *count.entry(g).or_default() += 1;
                    }
                    let mut best: Vec<(GroupId, usize)> = count.into_iter().collect();
                    best.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| format!("{:?}", a.0).cmp(&format!("{:?}", b.0))));
                    match best.into_iter().next() {
                        Some((g, _)) => PlanDest::Group(g),
                        None => {
                            gone += 1;
                            new_names.push(None);
                            PlanDest::New(new_names.len() - 1)
                        }
                    }
                }
            });
        }
        let mut moves = Vec::with_capacity(e.moves.len());
        let mut seen: HashSet<FaceRef> = HashSet::new();
        for (head, dest) in &e.moves {
            if removed.covers(head) || !seen.insert(*head) {
                continue;
            }
            let to = match dest {
                EditDest::Group(i) => targets.get(*i).cloned().ok_or(Refused::Nothing)?,
                EditDest::New(i) if *i < e.new_names.len() => PlanDest::New(*i),
                EditDest::New(_) => return Err(Refused::Nothing),
                EditDest::Removed => PlanDest::Removed,
                EditDest::TakenOut => PlanDest::TakenOut,
            };
            let from = self.group_of(kind, head);
            // Taken out of no group: nothing to do.
            if to == PlanDest::TakenOut && from.is_none() {
                continue;
            }
            if let (PlanDest::Group(t), Some(f)) = (&to, &from) {
                if t == f {
                    continue;
                }
            }
            moves.push((*head, from, to));
        }
        let renames = e
            .renames
            .iter()
            .filter_map(|(i, name)| match targets.get(*i) {
                Some(PlanDest::Group(g)) => Some((g.clone(), name.clone())),
                _ => None,
            })
            .collect();
        let covers = e
            .covers
            .iter()
            .filter_map(|(i, face)| match targets.get(*i) {
                Some(PlanDest::Group(g)) => Some((g.clone(), *face)),
                _ => None,
            })
            .collect();
        Ok(EditPlan { kind, new_names, moves, renames, covers, gone })
    }

    /// The group editor's *Apply*, planned (`plan_edit`): every move, the
    /// new groups, the removals and the names, as ONE undo step.  An
    /// automatic group that receives heads (or a name) becomes a subject
    /// first, as a move makes it; a subject left with nothing is gone (as
    /// "Not heads" leaves it), and so is a new group nothing went to.
    /// Refused with nothing changed (and no undo step) when any part is.
    /// Returns the words (the status line's and the undo's) and the new
    /// groups made.
    pub fn apply_edit(&mut self, plan: &EditPlan) -> Result<(String, Vec<SubjectId>), Refused> {
        if plan.moves.is_empty() && plan.renames.is_empty() && plan.covers.is_empty() {
            return Err(Refused::Nothing);
        }
        let words = self.edit_words(plan);
        let before = self.file.clone();
        self.checkpoint(words.clone());
        match self.apply_plan(plan) {
            // Nothing changed (a group photo the group no longer holds):
            // no undo step.
            Ok(_) if self.file == before => {
                self.undo.pop();
                Err(Refused::Nothing)
            }
            Ok(made) => {
                self.save();
                Ok((words, made))
            }
            Err(e) => {
                self.file = before;
                self.undo.pop();
                Err(e)
            }
        }
    }

    fn apply_plan(&mut self, plan: &EditPlan) -> Result<Vec<SubjectId>, Refused> {
        let kind = plan.kind;
        // Every existing group that receives heads or a name, as a
        // subject: automatic ones made into subjects first, before any
        // head moves (their members as the grouping holds them now).
        let mut as_subject: HashMap<GroupId, SubjectId> = HashMap::new();
        // (A group heads are taken out of, too: the rejection is kept on
        // a subject, so it outlives a regroup.)
        let wanted = plan.moves.iter().filter_map(|(_, from, to)| match to {
            PlanDest::Group(g) => Some(g.clone()),
            PlanDest::TakenOut => from.clone(),
            _ => None,
        });
        for g in wanted.chain(plan.renames.iter().map(|(g, _)| g.clone())) {
            if as_subject.contains_key(&g) {
                continue;
            }
            let (k, members) = self.members(&g)?;
            if k != kind {
                return Err(Refused::MixedKinds);
            }
            let sid = match &g {
                GroupId::Subject(s) => s.clone(),
                GroupId::Cluster(_) => self.file.create(kind, None, members),
            };
            as_subject.insert(g, sid);
        }
        let made: Vec<SubjectId> = plan.new_names.iter().map(|n| self.file.create(kind, n.clone(), Vec::new())).collect();
        // The heads, by where they go (in the editor's order).
        let mut by_dest: Vec<(PlanDest, Vec<FaceRef>)> = Vec::new();
        for (head, _, to) in &plan.moves {
            match by_dest.iter_mut().find(|(d, _)| d == to) {
                Some((_, v)) => v.push(*head),
                None => by_dest.push((to.clone(), vec![*head])),
            }
        }
        // Taken out: by the group each leaves.
        let mut taken: Vec<(GroupId, Vec<FaceRef>)> = Vec::new();
        for (head, from, to) in &plan.moves {
            if let (PlanDest::TakenOut, Some(g)) = (to, from) {
                match taken.iter_mut().find(|(x, _)| x == g) {
                    Some((_, v)) => v.push(*head),
                    None => taken.push((g.clone(), vec![*head])),
                }
            }
        }
        for (g, heads) in &taken {
            let _ = self.file.reject(&as_subject[g], heads.clone());
        }
        for (to, heads) in &by_dest {
            match to {
                PlanDest::TakenOut => {}
                PlanDest::Group(g) => {
                    let _ = self.file.add_members(&as_subject[g], heads.clone());
                }
                PlanDest::New(i) => {
                    let _ = self.file.add_members(&made[*i], heads.clone());
                }
                PlanDest::Removed => {
                    for h in heads {
                        let _ = self.file.remove_box(kind, *h);
                    }
                }
            }
        }
        for (g, name) in &plan.renames {
            self.file.rename(&as_subject[g], name.clone())?;
        }
        // Group photos last, on the groups as the moves left them: a
        // named group still holding the face (else nothing, as on its
        // page).
        for (g, face) in &plan.covers {
            let sid = match (as_subject.get(g), g) {
                (Some(s), _) | (None, GroupId::Subject(s)) => s.clone(),
                (None, GroupId::Cluster(_)) => continue,
            };
            let _ = self.file.set_cover(&sid, *face);
        }
        // A subject left with nothing goes (its name with it); so does a
        // new group nothing went to.
        let touched: HashSet<SubjectId> = plan
            .moves
            .iter()
            .filter_map(|(_, from, _)| match from {
                Some(GroupId::Subject(s)) => Some(s.clone()),
                _ => None,
            })
            .chain(as_subject.values().cloned())
            .chain(made.iter().cloned())
            .collect();
        for s in touched {
            if self.file.get(&s).is_some_and(|x| x.members.is_empty()) {
                let _ = self.file.forget(&s);
            }
        }
        Ok(made.into_iter().filter(|s| self.file.get(s).is_some()).collect())
    }

    /// An edit in words: "Split into 3 groups, 4 heads to Kiba, 2
    /// removed".
    pub fn edit_words(&self, plan: &EditPlan) -> String {
        subjects::plan_words(plan, |g| self.subject_of(g).and_then(|s| s.name.clone()))
    }

    /// A review answer for `group`: `Some(true)` confirms the faces as
    /// members, `Some(false)` rejects them ("not this person"), `None`
    /// takes an earlier answer back (the faces leave both lists).  A
    /// cluster answered becomes a subject.
    pub fn answer(&mut self, group: &GroupId, faces: Vec<FaceRef>, yes: Option<bool>) -> Result<SubjectId, Refused> {
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        let (kind, members) = self.members(group)?;
        let n = faces.len();
        let name = self.subject_of(group).and_then(|s| s.name.clone()).unwrap_or_else(|| "the group".to_string());
        self.checkpoint(match yes {
            Some(true) => format!("Added {n} {} to {name}", plural(n, "photo")),
            Some(false) => format!("Marked {n} {} not {name}", plural(n, "photo")),
            None => format!("Took back {n} {} for {name}", plural(n, "answer")),
        });
        let sid = match group {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => self.file.create(kind, None, members),
        };
        match yes {
            Some(true) => {
                self.file.add_members(&sid, faces)?;
            }
            Some(false) => {
                self.file.reject(&sid, faces)?;
            }
            None => {
                self.file.remove_members(&sid, &faces)?;
                self.file.unreject(&sid, &faces)?;
            }
        }
        self.save();
        Ok(sid)
    }

    /// A change made on a box in the viewer's labels: one undo step, the
    /// file saved; refused (and nothing changed, no undo step) when it
    /// would mix kinds or changes nothing.  Returns what it did, in the
    /// words the undo uses ("Named a face Someone").
    pub fn label(&mut self, edit: &LabelEdit) -> Result<String, Refused> {
        let before = self.file.clone();
        match apply_label(&mut self.file, edit) {
            Ok(what) => {
                self.undo.push((before, what.clone(), None));
                if self.undo.len() > UNDO_DEPTH {
                    self.undo.remove(0);
                }
                self.save();
                Ok(what)
            }
            Err(e) => {
                self.file = before;
                Err(e)
            }
        }
    }

    /// "Not fursuit heads" (or "Not faces") on whole groups, or on heads
    /// picked in one: every box goes as the viewer's "Not a fursuit head"
    /// does (the `removed` list, so a rebuild never brings it back), in
    /// one undo step.  Each target is a group of `kind` (another kind is
    /// refused, nothing changed) with the boxes to remove (`None`: all its
    /// members; picked boxes not in it are ignored).  A subject left with
    /// no member goes too.  Returns the words and the boxes removed (one
    /// decision, for the record of the user's corrections).
    pub fn remove_boxes(&mut self, kind: Kind, targets: &[(GroupId, Option<Vec<FaceRef>>)]) -> Result<(String, Vec<FaceRef>), Refused> {
        let mut boxes: Vec<FaceRef> = Vec::new();
        let mut subjects: Vec<SubjectId> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (id, picked) in targets {
            let (k, members) = self.members(id)?;
            if k != kind {
                return Err(Refused::MixedKinds);
            }
            let take: Vec<FaceRef> = match picked {
                Some(p) => members.iter().filter(|m| p.contains(m)).copied().collect(),
                None => members,
            };
            for f in take {
                if !boxes.contains(&f) {
                    boxes.push(f);
                }
            }
            if let GroupId::Subject(s) = id {
                subjects.push(s.clone());
                if let Some(n) = self.file.get(s).and_then(|s| s.name.clone()) {
                    names.push(n);
                }
            }
        }
        if boxes.is_empty() {
            return Err(Refused::Nothing);
        }
        let n = boxes.len();
        // Named in the words only when it is one named group.
        if targets.len() != 1 {
            names.clear();
        }
        let count = if n == 1 { format!("a {}", box_noun(kind)) } else { format!("{n} {}", plural(n, box_noun(kind))) };
        let what = match names.as_slice() {
            [one] => format!("Removed {count} from {one}"),
            _ => format!("Removed {count}"),
        };
        self.checkpoint(what.clone());
        for f in &boxes {
            // Already removed (or never there) is fine: the rest go.
            let _ = self.file.remove_box(kind, *f);
        }
        // A subject with nothing left is gone too (its name with it).
        for s in subjects {
            if self.file.get(&s).is_some_and(|x| x.members.is_empty()) {
                let _ = self.file.forget(&s);
            }
        }
        self.save();
        Ok((what, boxes))
    }

    /// "Not a fursuit head" (or "Not a face") in a review: these boxes
    /// (one per frame of the press answered) go to the removed list, as
    /// the viewer's removal does, in one undo step.  Returns the words and
    /// the memberships the boxes lost (subject, box), so a review's take
    /// back ([`People::restore_faces`]) can put them back.
    pub fn remove_faces(&mut self, kind: Kind, faces: &[FaceRef]) -> Result<(String, Taken), Refused> {
        let faces: Vec<FaceRef> = faces.iter().fold(Vec::new(), |mut v, f| {
            if !v.contains(f) {
                v.push(*f);
            }
            v
        });
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        let lost: Vec<(SubjectId, FaceRef)> = faces.iter().filter_map(|f| self.file.member_of(kind, f).map(|s| (s.id.clone(), *f))).collect();
        // A box the user drew just goes (it isn't remembered as removed):
        // kept here, so taking the X back draws it again.
        let drawn: Vec<FaceRef> = faces.iter().filter(|f| self.file.added.contains(&subjects::Labelled { kind, face: **f })).copied().collect();
        let n = faces.len();
        let what = if n == 1 { format!("Marked a {} not a {}", "photo", box_noun(kind)) } else { format!("Marked {n} frames not a {}", box_noun(kind)) };
        self.checkpoint(what.clone());
        for f in &faces {
            let _ = self.file.remove_box(kind, *f);
        }
        self.save();
        Ok((what, Taken { lost, drawn }))
    }

    /// A review's "Not a fursuit head" taken back (Z): the boxes leave the
    /// removed list (drawn ones are drawn again) and rejoin the subjects
    /// they were in; one step (the review takes answers back the same way).
    pub fn restore_faces(&mut self, kind: Kind, faces: &[FaceRef], taken: &Taken) -> Result<String, Refused> {
        let gone: Vec<subjects::Labelled> = self.file.removed.iter().filter(|l| l.kind == kind && faces.contains(&l.face)).copied().collect();
        let redraw: Vec<subjects::Labelled> = taken.drawn.iter().filter(|f| faces.contains(f)).map(|&face| subjects::Labelled { kind, face }).filter(|l| !self.file.added.contains(l)).collect();
        if gone.is_empty() && redraw.is_empty() {
            return Err(Refused::Nothing);
        }
        let what = format!("Took back {} not a {}", util_plural(faces.len(), "photo"), box_noun(kind));
        self.checkpoint(what.clone());
        self.file.removed.retain(|l| !gone.contains(l));
        self.file.added.extend(redraw);
        for (s, f) in &taken.lost {
            if self.file.get(s).is_some_and(|s| s.kind == kind) {
                let _ = self.file.add_members(s, vec![*f]);
            }
        }
        self.save();
        Ok(what)
    }

    /// *Forget all people* (or fursuits, or pets): that kind's subjects
    /// go from the user's file, and for people the face cache too; the
    /// other kinds' names, answers and caches stay untouched.  Beyond
    /// undo's reach, like [`People::forget_all`].
    pub fn forget_kind(&mut self, kind: Kind) {
        // Its names, its "not the same" answers and its labels from the
        // viewer go together.
        self.file.forget_kind(kind);
        self.undo.clear();
        if self.file.is_empty() {
            if let Some(p) = &self.file_path {
                let _ = std::fs::remove_file(p);
            }
        } else {
            self.save();
        }
        if kind == Kind::Person {
            if let Some(s) = &mut self.store {
                s.forget_all();
            }
        }
    }

    /// Build the page's snapshot: subjects (named first, most photos
    /// first), then clusters.  `resolve` turns a detection into a
    /// [`Face`] with its shot (the engine's resolver); detections of
    /// photos no longer in the library resolve to nothing and are not
    /// counted.
    #[cfg(test)]
    pub fn build_snapshot(&self, resolve: &dyn Fn(&FaceRef) -> Option<Face>) -> Arc<Subjects> {
        self.build_snapshot_found(resolve, &|_, _| true)
    }

    /// [`People::build_snapshot`], a chosen group photo shown only while
    /// `found` (kind, box) says its detection is still there (a rebuild
    /// may not find it again); else the automatic pick.
    pub fn build_snapshot_found(&self, resolve: &dyn Fn(&FaceRef) -> Option<Face>, found: &dyn Fn(Kind, &FaceRef) -> bool) -> Arc<Subjects> {
        let mut groups: Vec<Group> = Vec::new();
        let group_of = |id: GroupId, kind: Kind, name: Option<String>, hidden: bool, cover: Option<FaceRef>, members: &[FaceRef], suggestions: usize| -> Option<Group> {
            let faces: Vec<Face> = members.iter().filter_map(resolve).collect();
            // A chosen photo counts only on a named group, while it holds it.
            let cover = subjects::cover_of(cover.filter(|_| name.is_some()), &faces, |c| found(kind, c))?;
            let photos = faces.iter().map(|f| f.face.key).collect::<HashSet<_>>().len();
            let days = subjects::count_days(&faces);
            Some(Group { id, kind, name, hidden, photos, bursts: subjects::count_bursts(&faces), days, cover, suggestions })
        };
        // The groups showing the photo the user chose (not the automatic one).
        let mut chosen_covers = HashSet::new();
        for s in &self.file.subjects {
            let suggestions = self.grouped.suggestion_counts.get(&s.id).copied().unwrap_or(0);
            let Some(g) = group_of(GroupId::Subject(s.id.clone()), s.kind, s.name.clone(), s.hidden, s.cover, &s.members, suggestions) else { continue };
            if s.cover == Some(g.cover.face) {
                chosen_covers.insert(g.id.clone());
            }
            groups.push(g);
        }
        if let Some(store) = &self.store {
            let refs = store.refs();
            for (c, &id) in self.grouped.clusters.iter().zip(&self.grouped.ids) {
                let members: Vec<FaceRef> = c.members.iter().filter_map(|&m| refs.get(m).copied()).collect();
                groups.extend(group_of(GroupId::Cluster(id), Kind::Person, None, false, None, &members, 0));
            }
        }
        groups.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(matches!(b.id, GroupId::Subject(_)).cmp(&matches!(a.id, GroupId::Subject(_)))).then(b.photos.cmp(&a.photos)).then_with(|| a.name.cmp(&b.name)));
        let undoable = self.undo.last().map(|(_, what, _)| what.clone());
        let faces_found = self.store.as_ref().map_or(0, FaceStore::found);
        Arc::new(Subjects { generation: self.generation, groups, faces_found, undoable, seen_together: HashSet::new(), chosen_covers, loose: HashMap::new() })
    }

    /// Suggestions for review (UI 6): for a subject, the faces within
    /// the cut-off (the strict one with `strict`) of its confirmed
    /// members that are confirmed nowhere and not rejected for it, nearest
    /// first, one entry per burst; for a cluster, its own members to
    /// check, surest first, one entry per burst.  Never another kind's.
    pub fn suggestions(&self, id: &GroupId, resolve: &dyn Fn(&FaceRef) -> Option<Face>, strict: bool) -> Result<Vec<Vec<Face>>, Refused> {
        let Some(store) = &self.store else { return Ok(Vec::new()) };
        let refs = store.refs();
        let rows = store.rows();
        let by_ref: HashMap<FaceRef, usize> = refs.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let ordered: Vec<(usize, f32)> = match id {
            GroupId::Subject(sid) => {
                let s = self.file.get(sid).ok_or_else(|| Refused::NoSuchSubject(sid.clone()))?;
                if s.kind != Kind::Person {
                    return Ok(Vec::new());
                }
                let members: Vec<usize> = s.members.iter().filter_map(|r| by_ref.get(r).copied()).collect();
                let confirmed = self.file.confirmed(Kind::Person);
                let out = step::Exclusion::new(&self.file, Kind::Person);
                let pool: Vec<usize> = (0..refs.len()).filter(|&i| !confirmed.contains_key(&refs[i]) && !s.rejected.contains(&refs[i]) && !out.excludes(store, &refs[i])).collect();
                let cutoffs = store.cutoffs();
                let cutoff = if strict { cutoffs.strict } else { cutoffs.suggest };
                subjects::candidates(&rows, &members, &pool, cutoff)
            }
            GroupId::Cluster(cid) => {
                let c = self.grouped.cluster(*cid).ok_or(Refused::StaleGroup)?;
                let mut v: Vec<(usize, f32)> = c.members.iter().map(|&m| (m, 1.0 - rows.dot(m, &c.centroid))).collect();
                v.sort_by(|a, b| a.1.total_cmp(&b.1));
                v
            }
        };
        let faces: Vec<(Face, Option<u64>)> = ordered.iter().filter_map(|&(i, _)| resolve(refs.get(i)?)).map(|f| {
            let b = f.burst;
            (f, b)
        }).collect();
        Ok(subjects::per_burst(faces))
    }

    /// Keep a snapshot built with [`People::build_snapshot`].
    pub fn set_snapshot(&mut self, snapshot: Arc<Subjects>) {
        self.snapshot = snapshot;
    }

    /// A group's faces, clearest first.
    pub fn faces(&self, id: &GroupId, resolve: &dyn Fn(&FaceRef) -> Option<Face>) -> Result<Vec<Face>, Refused> {
        let (_, members) = self.members(id)?;
        let mut out: Vec<Face> = members.iter().filter_map(resolve).collect();
        out.sort_by(|a, b| b.clarity.total_cmp(&a.clarity));
        Ok(out)
    }

    /// Named subjects of `kind` called `name`.
    pub fn named(&self, kind: Kind, name: &str) -> Vec<GroupId> {
        self.file.named(kind, name).into_iter().map(|s| GroupId::Subject(s.id.clone())).collect()
    }

    /// The photos (keys) a group's faces are in.
    pub fn photos_of(&self, id: &GroupId) -> Result<HashSet<Key>, Refused> {
        let (_, members) = self.members(id)?;
        Ok(members.iter().map(|f| f.key).collect())
    }

    /// A face's clarity (UI 12): how clear a view it is and how sure its
    /// membership is; 0 when unknown.
    pub fn clarity(&self, r: &FaceRef) -> f32 {
        let Some(store) = &self.store else { return 0.0 };
        let visual = store.clarity(r);
        match store.vectors.by_ref.get(r) {
            Some(&row) => self.grouped.clarity_of(row, visual),
            None => 0.5 * visual,
        }
    }

    /// The clarity of the clearest face in each of `keys` (UI 12's order
    /// for a subject's page).
    pub fn clarity_by_photo(&self, keys: &HashSet<Key>) -> HashMap<Key, f32> {
        keys.iter()
            .map(|k| {
                let best = self.store.as_ref().map_or(0.0, |s| s.faces_of(k).iter().chain(s.drawn_of(k)).map(|d| self.clarity(&d.face_ref(*k))).fold(0.0, f32::max));
                (*k, best)
            })
            .collect()
    }
}

/// Apply a label edit to the user's file; what it did, in words.
fn apply_label(file: &mut SubjectsFile, edit: &LabelEdit) -> Result<String, Refused> {
    match edit {
        LabelEdit::Name { face, kind, to } => {
            let named = name_box(file, *kind, *face, to)?;
            Ok(match named {
                Some(n) => format!("Named a {} {n}", box_noun(*kind)),
                None => format!("Moved a {} to Unnamed", box_noun(*kind)),
            })
        }
        LabelEdit::Kind { face, from, to, name } => {
            file.change_kind(*face, *from, *to)?;
            let named = match name {
                Some(n) => name_box(file, *to, *face, n)?,
                None => None,
            };
            let what = format!("Changed a {} to a {}", box_noun(*from), box_noun(*to));
            Ok(match named {
                Some(n) => format!("{what}, {n}"),
                None => what,
            })
        }
        LabelEdit::Add { face, kind, name } => {
            file.add_box(*kind, *face)?;
            let named = match name {
                Some(n) => name_box(file, *kind, *face, n)?,
                None => None,
            };
            Ok(match named {
                Some(n) => format!("Added a {} for {n}", box_noun(*kind)),
                None => format!("Added a {}", box_noun(*kind)),
            })
        }
        LabelEdit::Remove { face, kind } => {
            file.remove_box(*kind, *face)?;
            Ok(format!("Removed a {}", box_noun(*kind)))
        }
    }
}

/// Name a box as `to` says; the name it now has (`None`: Unnamed).
fn name_box(file: &mut SubjectsFile, kind: Kind, face: FaceRef, to: &NameTo) -> Result<Option<String>, Refused> {
    match to {
        NameTo::Subject(id) => {
            file.name_box(kind, face, id)?;
            Ok(Some(file.get(id).and_then(|s| s.name.clone()).unwrap_or_else(|| "a group".to_string())))
        }
        NameTo::New(name) => {
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            if name.is_empty() {
                return Err(Refused::Nothing);
            }
            // A name the kind has already is that subject.
            match file.named(kind, &name).first().map(|s| s.id.clone()) {
                Some(id) => {
                    file.name_box(kind, face, &id)?;
                    Ok(Some(file.get(&id).and_then(|s| s.name.clone()).unwrap_or(name)))
                }
                None => {
                    file.create(kind, Some(name.clone()), vec![face]);
                    Ok(Some(name))
                }
            }
        }
        NameTo::Unnamed => {
            file.unname_box(kind, face)?;
            Ok(None)
        }
    }
}

/// What a review's X took ([`People::remove_faces`]), so Z can put it
/// back ([`People::restore_faces`]): the subjects the boxes were members
/// of, and the boxes the user had drawn (they leave `added` outright).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taken {
    pub lost: Vec<(SubjectId, FaceRef)>,
    pub drawn: Vec<FaceRef>,
}

/// "a photo", "3 photos".
fn util_plural(n: usize, word: &str) -> String {
    if n == 1 { format!("a {word}") } else { format!("{n} {word}s") }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

/// A face's pixels for `Engine::crop`: the cached tile when there is one
/// and the framing is the face's; else from the photo's own preview
/// (reads the file: call on a worker).  `px`×`px` RGBA, or fitted for
/// [`Framing::Photo`].
pub fn crop(tiles: Option<&TileIndex>, face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
    let px = px.clamp(8, 2048);
    if framing == Framing::Face {
        if let Some(jpeg) = tiles.and_then(|t| t.read(&face.face)) {
            if let Ok(decoded) = decode::decode_jpeg_full_scaled(&jpeg, 1) {
                let out = decode::resize_to(&decoded, px, px);
                return Some((px, px, decode::to_rgba(out)));
            }
        }
    }
    crop_from_photo(face, px, framing)
}

/// A face's pixels from its photo's own preview: decodes the preview,
/// orients it, and cuts the face's square (a margin around the box) or
/// fits the whole preview.
pub fn crop_from_photo(face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
    let path = face.jpg.as_deref().or(face.raw.as_deref())?;
    let oriented = preview_pixels(path, face.jpg.is_some(), px.max(400))?;
    let out = match framing {
        Framing::Photo => decode::fit(oriented, px),
        Framing::Face => {
            let (w, h) = (oriented.width as f32, oriented.height as f32);
            let b = face.face.bbox;
            // A square around the box, 60 % wider than its longer side.
            let side = (b[2] * w).max(b[3] * h) * 1.6;
            let (cx, cy) = ((b[0] + b[2] / 2.0) * w, (b[1] + b[3] / 2.0) * h);
            let side = side.min(w).min(h).max(2.0);
            let x = (cx - side / 2.0).clamp(0.0, w - side);
            let y = (cy - side / 2.0).clamp(0.0, h - side);
            let cut = decode::crop(&oriented, x as u32, y as u32, (side as u32).max(1), (side as u32).max(1));
            decode::resize_to(&cut, px, px)
        }
    };
    let (w, h) = (out.width, out.height);
    Some((w, h, decode::to_rgba(out)))
}

/// The photo's preview, decoded and oriented, at least `min_edge` on its
/// shorter side where the preview allows.
fn preview_pixels(path: &Path, is_jpg: bool, min_edge: u32) -> Option<decode::Pixels> {
    use crate::imaging::libraw::{Raw, Thumbnail};
    if is_jpg {
        let (data, head) = crate::exif::jpeg_preview_bytes(path, min_edge).or_else(|| {
            let all = std::fs::read(path).ok()?;
            let head = all[..all.len().min(256 * 1024)].to_vec();
            Some((all, head))
        })?;
        let orientation = crate::exif::orientation_from_jpeg_head(&head);
        let (w, h) = decode::jpeg_dimensions(&data)?;
        let denom = [8, 4, 2].into_iter().find(|&d| w.min(h) / d >= min_edge).unwrap_or(1);
        let px = decode::decode_jpeg_full_scaled(&data, denom).ok()?;
        Some(decode::apply_orientation(px, orientation))
    } else {
        let mut raw = Raw::open(path).ok()?;
        let flip = raw.exif_orientation();
        match raw.thumbnail_at_least(min_edge).ok()? {
            Thumbnail::Jpeg(bytes) => {
                let orientation = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
                let px = decode::decode_jpeg_full_scaled(&bytes, 1).ok()?;
                Some(decode::apply_orientation(px, orientation))
            }
            Thumbnail::Bitmap { width, height, colors, data } => {
                let px = crate::imaging::thumbs::bitmap(width, height, colors, data)?;
                Some(decode::apply_orientation(px, flip))
            }
        }
    }
}

/// The default subjects file path (`subjects::default_path`), for the
/// engine's configuration.
pub fn default_file_path() -> Option<PathBuf> {
    subjects::default_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn face(n: u8) -> FaceRef {
        FaceRef::new(Key([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }

    fn resolve(f: &FaceRef) -> Option<Face> {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        Some(Face { face: *f, jpg: Some(PathBuf::from(format!("/mnt/nas/lib/{}.JPG", f.key.0[0]))), raw: None, day, capture: day.and_hms_opt(12, 0, 0).unwrap(), burst: None, clarity: f.key.0[0] as f32, confirmed: true })
    }

    pub fn found(x: f32, size: f32, v: &[f32]) -> FaceFound {
        FaceFound { bbox: [x, 0.2, 0.1, 0.15], landmarks: [[x + 0.02, 0.25]; 5], score: 0.95, size_px: size, sharpness: 300.0, vector: v.to_vec(), tile: vec![0xff, 0xd8, 1, 2, 3] }
    }

    #[test]
    fn the_cache_keeps_detections_vectors_and_tiles_apart_and_private() {
        let d = std::env::temp_dir().join(format!("pp-facestore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let dir = d.join("cache");
        let mut s = FaceStore::load(&dir, ModelId::AuraFace);
        assert_eq!((s.looked_at(), s.found()), (0, 0));
        let (k1, k2, k3) = (Key([1; 16]), Key([2; 16]), Key([3; 16]));
        s.add(k1, &[found(0.1, 80.0, &[1.0, 0.0, 0.0, 0.0]), found(0.5, 40.0, &[0.0, 1.0, 0.0, 0.0])]).unwrap();
        s.add(k2, &[]).unwrap();
        assert_eq!((s.looked_at(), s.found(), s.vectors.len()), (2, 2, 2));
        assert!(s.scanned.contains_key(&k2) && !s.scanned.contains_key(&k3));
        let r = s.faces_of(&k1)[0].face_ref(k1);
        assert_eq!(s.vectors.row(s.vectors.by_ref[&r]), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(s.tiles().lock().read(&r).unwrap(), [0xff, 0xd8, 1, 2, 3]);
        assert!(s.detection(&r).unwrap().clarity() > s.detection(&s.faces_of(&k1)[1].face_ref(k1)).unwrap().clarity(), "the larger face is clearer");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["faces.bin", "facevecs-auraface-v1.bin", "tiles.bin"] {
                assert_eq!(std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
            }
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // Back from disk, the same.
        let back = FaceStore::load(&dir, ModelId::AuraFace);
        assert_eq!(back.scanned, s.scanned);
        assert_eq!(back.vectors.row(back.vectors.by_ref[&r]), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(back.tiles().lock().offsets.len(), 2);
        assert!(FaceStore::bytes_in(&dir) > 0);
        // Another model: the detections and tiles stay, the vectors are
        // wanted again; recognising again fills them without new tiles.
        let mut other = FaceStore::load(&dir, ModelId::SFace);
        assert_eq!((other.looked_at(), other.vectors.len(), other.unrecognised()), (2, 0, 2));
        let need = other.needing_vectors();
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].1.len(), 2);
        other.add_vectors(k1, &[found(0.1, 80.0, &[0.5, 0.5]), found(0.5, 40.0, &[0.5, -0.5])]).unwrap();
        assert_eq!((other.vectors.dim, other.unrecognised()), (2, 0));
        assert!(other.needing_vectors().is_empty());
        assert_eq!(other.tiles().lock().offsets.len(), 2);
        assert_eq!(FaceStore::load(&dir, ModelId::AuraFace).vectors.dim, 4, "the first model's vectors are untouched");
        // Photos that left the library go at the next load's compaction.
        let mut s = FaceStore::load(&dir, ModelId::AuraFace);
        s.retain(&[k2].into_iter().collect());
        assert_eq!((s.looked_at(), s.vectors.len(), s.tiles().lock().offsets.len()), (1, 0, 0));
        let back = FaceStore::load(&dir, ModelId::AuraFace);
        assert_eq!((back.looked_at(), back.vectors.len(), back.tiles().lock().offsets.len()), (1, 0, 0));
        // Forget all: nothing left on disk.
        let mut s = back;
        s.forget_all();
        assert_eq!(FaceStore::bytes_in(&dir), 0);
        assert!(!dir.join("faces.bin").exists() && !dir.join("tiles.bin").exists() && !dir.join("facevecs-auraface-v1.bin").exists() && !dir.join("facevecs-sface-2021dec.bin").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_torn_tail_loses_only_the_last_block() {
        let d = std::env::temp_dir().join(format!("pp-facetorn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut s = FaceStore::load(&d, ModelId::AuraFace);
        s.add(Key([1; 16]), &[found(0.1, 80.0, &[1.0, 0.0])]).unwrap();
        s.add(Key([2; 16]), &[found(0.3, 80.0, &[0.0, 1.0])]).unwrap();
        for f in ["faces.bin", "facevecs-auraface-v1.bin", "tiles.bin"] {
            let p = d.join(f);
            let data = std::fs::read(&p).unwrap();
            std::fs::write(&p, &data[..data.len() - 3]).unwrap();
        }
        let back = FaceStore::load(&d, ModelId::AuraFace);
        assert_eq!((back.looked_at(), back.vectors.len(), back.tiles().lock().offsets.len()), (1, 1, 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edits_are_undone_newest_first_and_the_snapshot_follows_the_file() {
        let d = std::env::temp_dir().join(format!("pp-people-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut p = People::load(Some(path.clone()));
        let a = p.move_faces(&GroupId::Cluster(1), vec![face(1)], None);
        assert_eq!(a, Err(Refused::StaleGroup), "no clusters yet: a cluster id is stale");
        let a = GroupId::Subject(p.file.create(Kind::Person, None, vec![face(1), face(2)]));
        p.name_group(&a, "Someone".into()).unwrap();
        assert!(path.is_file());
        let b = GroupId::Subject(p.file.create(Kind::Person, None, vec![face(3)]));
        p.merge(&a, std::slice::from_ref(&b)).unwrap();
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.groups.len(), 1);
        assert_eq!((snap.groups[0].photos, snap.groups[0].name.as_deref()), (3, Some("Someone")));
        assert_eq!(snap.groups[0].cover.face, face(3), "the clearest face is the cover");
        assert_eq!(snap.undoable.as_deref(), Some("Merged 2 groups into Someone"));
        assert_eq!(p.undo().as_deref(), Some("Merged 2 groups into Someone"));
        assert_eq!(p.file.subjects.len(), 2);
        assert_eq!(p.undo().as_deref(), Some("Named a group Someone"));
        assert!(p.file.subjects[0].name.is_none());
        assert_eq!(p.undo(), None);
        // Answers, moves, hides and forgets each leave an undo step.
        p.answer(&a, vec![face(4)], Some(true)).unwrap();
        p.answer(&a, vec![face(5)], Some(false)).unwrap();
        assert_eq!(p.faces(&a, &resolve).unwrap().iter().map(|f| f.face.key.0[0]).collect::<Vec<_>>(), [4, 2, 1], "clearest first");
        p.answer(&a, vec![face(5)], None).unwrap();
        assert!(p.subject_of(&a).unwrap().rejected.is_empty());
        let moved = p.move_faces(&a, vec![face(4)], None).unwrap();
        assert_eq!(p.subject_of(&GroupId::Subject(moved.clone())).unwrap().members, [face(4)]);
        p.set_hidden(&GroupId::Subject(moved.clone()), true).unwrap();
        p.forget(&moved).unwrap();
        assert_eq!(p.undo().as_deref(), Some("Forgot a group"));
        assert_eq!(p.undo().as_deref(), Some("Hid a group"));
        assert_eq!(p.undo().as_deref(), Some("Moved 1 face to a new group"));
        assert_eq!(p.subject_of(&a).unwrap().members.len(), 3);
        assert_eq!(p.photos_of(&a).unwrap().len(), 3);
        assert_eq!(p.answer(&a, vec![], Some(true)), Err(Refused::Nothing));
        assert_eq!(p.clarity(&face(1)), 0.0, "no cache: unknown clarity");
        // People were the only kind: forgetting them takes the file too.
        p.forget_kind(Kind::Person);
        assert!(!path.exists() && p.file.subjects.is_empty() && p.undo().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Decision 12 through the people state: merging or moving across
    /// kinds is refused and leaves no undo step.
    /// Another kind's cluster, published through `other_clusters`, keeps
    /// its kind through every operation: naming it makes a subject of
    /// that kind, merging it into a person is refused, and its members
    /// are found (phase 4 found the opposite on a real run).
    #[test]
    fn another_kinds_cluster_keeps_its_kind() {
        let mut p = People::load(None);
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![face(1), face(2)])]);
        let cluster = GroupId::Cluster(7);
        assert_eq!(p.members(&cluster), Ok((Kind::Fursuit, vec![face(1), face(2)])));
        let person = GroupId::Subject(p.file.create(Kind::Person, Some("Someone".into()), vec![face(3)]));
        assert_eq!(p.merge(&person, &[cluster.clone()]), Err(Refused::MixedKinds), "a fursuit cluster never joins a person");
        assert_eq!(p.move_faces(&cluster, vec![face(1)], Some(&person)), Err(Refused::MixedKinds));
        let id = p.name_group(&cluster, "Suit".into()).unwrap();
        let s = p.file.get(&id).unwrap();
        assert_eq!((s.kind, s.name.as_deref(), s.members.len()), (Kind::Fursuit, Some("Suit"), 2));
        assert_eq!(p.named(Kind::Fursuit, "suit"), [GroupId::Subject(id.clone())]);
        assert!(p.named(Kind::Person, "suit").is_empty());
        // A stale or unknown cluster id is refused, not taken for a person.
        p.set_other_clusters(Kind::Fursuit, Vec::new());
        assert_eq!(p.name_group(&cluster, "Again".into()), Err(Refused::StaleGroup));
        assert_eq!(p.undo().as_deref(), Some("Named a group Suit"));
    }

    /// Forgetting one kind leaves the others: Preferences ▸ People's
    /// "Forget all" must not take the fursuits' names and answers with it,
    /// nor the fursuits' the people's.
    #[test]
    fn forgetting_one_kind_leaves_the_others() {
        let d = std::env::temp_dir().join(format!("pp-forget-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut p = People::load(Some(path.clone()));
        let person = p.file.create(Kind::Person, Some("Someone".into()), vec![face(1)]);
        let suit = p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(2)]);
        p.file.reject(&suit, vec![face(3)]).unwrap();
        p.save();
        p.forget_kind(Kind::Person);
        assert!(p.file.get(&person).is_none(), "the person is gone");
        let s = p.file.get(&suit).expect("the fursuit stays");
        assert_eq!((s.name.as_deref(), s.members.len(), s.rejected.len()), (Some("Suit"), 1, 1), "with its members and answers");
        assert!(path.is_file(), "the file stays while a kind is left");
        assert_eq!(SubjectsFile::load(&path).subjects.len(), 1);
        p.forget_kind(Kind::Pet);
        assert!(p.file.get(&suit).is_some(), "another kind's forget changes nothing here");
        p.forget_kind(Kind::Fursuit);
        assert!(p.file.subjects.is_empty() && !path.exists(), "the last kind gone: no file");
        assert!(p.undo().is_none(), "beyond undo");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The viewer's labels through the people state: every change is one
    /// undo step with its words, saved to the file; a mix of kinds is
    /// refused and leaves no step; a kind change leaves the old kind
    /// entirely.
    #[test]
    fn label_edits_are_one_undo_step_each_and_never_mix_kinds() {
        let d = std::env::temp_dir().join(format!("pp-labels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut p = People::load(Some(path.clone()));
        let someone = p.file.create(Kind::Person, Some("Someone".into()), vec![face(1)]);
        let suit = p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(2)]);
        let f3 = face(3);
        // Name: a person's name on a face; a fursuit's is refused.
        assert_eq!(p.label(&LabelEdit::Name { face: f3, kind: Kind::Person, to: NameTo::Subject(suit.clone()) }), Err(Refused::MixedKinds));
        assert!(p.undo.is_empty(), "a refusal leaves no step");
        assert_eq!(p.label(&LabelEdit::Name { face: f3, kind: Kind::Person, to: NameTo::Subject(someone.clone()) }).as_deref(), Ok("Named a face Someone"));
        // A typed name the kind has is that subject; a new one is made.
        assert_eq!(p.label(&LabelEdit::Name { face: face(4), kind: Kind::Person, to: NameTo::New(" someone ".into()) }).as_deref(), Ok("Named a face Someone"));
        assert_eq!(p.file.member_of(Kind::Person, &face(4)).unwrap().id, someone);
        p.label(&LabelEdit::Name { face: face(5), kind: Kind::Person, to: NameTo::New("Other".into()) }).unwrap();
        let other = p.file.member_of(Kind::Person, &face(5)).unwrap().id.clone();
        assert_ne!(other, someone);
        // "Suit" typed for a face is a new person, never the fursuit.
        p.label(&LabelEdit::Name { face: face(6), kind: Kind::Person, to: NameTo::New("Suit".into()) }).unwrap();
        assert_eq!(p.file.member_of(Kind::Person, &face(6)).unwrap().kind, Kind::Person);
        assert_ne!(p.file.member_of(Kind::Person, &face(6)).unwrap().id, suit);
        // Unnamed.
        assert_eq!(p.label(&LabelEdit::Name { face: f3, kind: Kind::Person, to: NameTo::Unnamed }).as_deref(), Ok("Moved a face to Unnamed"));
        assert!(p.file.get(&someone).unwrap().rejected.contains(&f3));
        // Kind: the suit's head is a person's face, named Someone.
        assert_eq!(
            p.label(&LabelEdit::Kind { face: face(2), from: Kind::Fursuit, to: Kind::Person, name: Some(NameTo::Subject(someone.clone())) }).as_deref(),
            Ok("Changed a fursuit head to a face, Someone")
        );
        assert!(p.file.get(&suit).unwrap().members.is_empty() && p.file.member_of(Kind::Person, &face(2)).unwrap().id == someone);
        assert!(p.file.subjects.iter().all(|s| !(s.kind == Kind::Fursuit && s.members.contains(&face(2)))));
        // A kind change naming a subject of the old kind is refused whole.
        let before = p.file.clone();
        assert_eq!(p.label(&LabelEdit::Kind { face: face(1), from: Kind::Person, to: Kind::Fursuit, name: Some(NameTo::Subject(someone.clone())) }), Err(Refused::MixedKinds));
        assert_eq!(p.file, before, "nothing changed");
        // Add, Remove.
        let drawn = FaceRef::new(Key([9; 16]), [0.5, 0.5, 0.1, 0.1]);
        assert_eq!(p.label(&LabelEdit::Add { face: drawn, kind: Kind::Fursuit, name: Some(NameTo::Subject(suit.clone())) }).as_deref(), Ok("Added a fursuit head for Suit"));
        assert!(p.file.is_added(Kind::Fursuit, &drawn) && p.file.get(&suit).unwrap().members.contains(&drawn));
        assert_eq!(p.label(&LabelEdit::Remove { face: face(1), kind: Kind::Person }).as_deref(), Ok("Removed a face"));
        assert!(!p.file.get(&someone).unwrap().members.contains(&face(1)));
        let saved = SubjectsFile::load(&path);
        assert_eq!((&saved.subjects, &saved.added, &saved.removed, saved.version), (&p.file.subjects, &p.file.added, &p.file.removed, subjects::FILE_VERSION), "every change is saved");
        // Undo, newest first, one step each.
        for want in [
            "Removed a face",
            "Added a fursuit head for Suit",
            "Changed a fursuit head to a face, Someone",
            "Moved a face to Unnamed",
            "Named a face Suit",
            "Named a face Other",
            "Named a face Someone",
            "Named a face Someone",
        ] {
            assert_eq!(p.undo().as_deref(), Some(want));
        }
        assert!(p.undo().is_none());
        assert!(p.file.written_version() == subjects::FILE_VERSION_PLAIN && p.file.get(&suit).unwrap().members == [face(2)]);
        // Forgetting fursuits takes their labels, not the people's.
        p.label(&LabelEdit::Remove { face: face(7), kind: Kind::Person }).unwrap();
        p.label(&LabelEdit::Add { face: drawn, kind: Kind::Fursuit, name: None }).unwrap();
        p.forget_kind(Kind::Fursuit);
        assert!(p.file.added.is_empty() && p.file.removed.len() == 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Merging fursuit groups (a drop, or S and M): an unnamed group merged
    /// into a named one keeps the name, whichever way round; of two named
    /// ones the kept one's name stays and the other's goes; each merge is
    /// one undo step that brings both back as they were.
    #[test]
    fn fursuit_merges_keep_the_name_and_undo_in_one_step() {
        let mut p = People::load(None);
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![face(1), face(2)]), (8, vec![face(3)])]);
        let named = GroupId::Subject(p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(4)]));
        // The cluster into the named one.
        let into = p.merge(&named, &[GroupId::Cluster(7)]).unwrap();
        let s = p.file.get(&into).unwrap();
        assert_eq!((s.name.as_deref(), s.members.len()), (Some("Suit"), 3));
        assert_eq!(p.undo().as_deref(), Some("Merged 2 groups into Suit"));
        assert_eq!(p.file.subjects.len(), 1);
        assert_eq!(p.file.get(&into).unwrap().members, [face(4)], "the undo brings it back as it was");
        // The named one into the cluster (dropped the other way): the name
        // still stays.
        let into = p.merge(&GroupId::Cluster(8), std::slice::from_ref(&named)).unwrap();
        let s = p.file.get(&into).unwrap();
        assert_eq!((s.kind, s.name.as_deref(), s.members.len()), (Kind::Fursuit, Some("Suit"), 2));
        assert_eq!(p.file.subjects.len(), 1, "one fursuit");
        assert!(p.undo().is_some());
        // Two named: the one kept keeps its name; one step undoes it.
        let other = GroupId::Subject(p.file.create(Kind::Fursuit, Some("Other".into()), vec![face(5)]));
        let before = p.file.clone();
        let kept = p.merge(&other, std::slice::from_ref(&named)).unwrap();
        assert_eq!(p.file.get(&kept).unwrap().name.as_deref(), Some("Other"));
        assert_eq!(p.file.named_count(Kind::Fursuit), 1, "the other name goes");
        assert_eq!(p.undo().as_deref(), Some("Merged 2 groups into Other"));
        assert_eq!(p.file, before);
    }

    /// Dissolving, people and fursuits alike: a group's heads kept apart
    /// by kind (a fursuit set says nothing of faces), a named subject
    /// forgotten with its answers, the heads left detections (nothing
    /// removed), one undo step back to the file exactly; a one-head group
    /// is nothing to dissolve; a dissolved head on its own is loose.
    #[test]
    fn dissolving_keeps_heads_apart_by_kind_and_undoes_exactly() {
        let mut p = People::load(None);
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![face(1), face(2), face(3)]), (8, vec![face(4)])]);
        assert_eq!(p.dissolve(&GroupId::Cluster(8)), Err(Refused::Nothing), "one head: nothing to dissolve");
        assert_eq!(p.dissolve(&GroupId::Cluster(99)), Err(Refused::StaleGroup));
        assert!(p.undo().is_none(), "a refusal leaves nothing to undo");
        let (what, heads) = p.dissolve(&GroupId::Cluster(7)).unwrap();
        assert_eq!((what.as_str(), heads.len()), ("Dissolved a group of 3 fursuit heads", 3));
        assert_eq!(p.file.dissolved_heads(Kind::Fursuit).len(), 3);
        assert!(p.file.dissolved_heads(Kind::Person).is_empty() && p.file.removed.is_empty(), "never a face's; nothing removed");
        // After the regroup each is on its own: loose, not a tile.
        p.set_other_clusters(Kind::Fursuit, vec![(11, vec![face(1)]), (12, vec![face(2)]), (13, vec![face(3), face(4)])]);
        let loose: Vec<GroupId> = p.loose_groups().into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(loose.len(), 2, "two alone; the one that joined face 4 is an ordinary group");
        assert!(!loose.contains(&GroupId::Cluster(13)));
        assert_eq!(p.undo().as_deref(), Some("Dissolved a group of 3 fursuit heads"));
        assert!(p.file.dissolved.is_empty() && p.loose_groups().is_empty());
        // A named person: forgotten, answers and all; undo, exactly.
        let named = p.file.create(Kind::Person, Some("Someone".into()), vec![face(20), face(21)]);
        p.file.reject(&named, vec![face(22)]).unwrap();
        let before = p.file.clone();
        let (what, heads) = p.dissolve(&GroupId::Subject(named.clone())).unwrap();
        assert_eq!((what.as_str(), heads.len()), ("Dissolved Someone", 2));
        assert!(p.file.get(&named).is_none() && p.file.named_count(Kind::Person) == 0, "the name goes");
        assert_eq!(p.file.dissolved_heads(Kind::Person).len(), 2);
        assert!(p.file.dissolved_heads(Kind::Fursuit).is_empty(), "kinds never mix");
        assert!(p.undo().is_some());
        assert_eq!(p.file, before, "back exactly");
    }

    /// *Reset unnamed groups* is one undo step of its kind only: the
    /// fursuits' unnamed subjects go, people's stay; the saved file is
    /// written once, and one undo puts it back exactly (on disk too).
    /// With none to reset it is refused and leaves nothing to undo.
    #[test]
    fn resetting_unnamed_groups_is_one_undo_step_of_its_kind() {
        let d = std::env::temp_dir().join(format!("pp-faces-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut p = People::load(Some(path.clone()));
        let named = p.file.create(Kind::Fursuit, Some("Sparky Dragon".into()), vec![face(1)]);
        p.file.create(Kind::Fursuit, None, vec![face(2), face(3)]);
        p.file.create(Kind::Fursuit, None, vec![face(4)]);
        let person = p.file.create(Kind::Person, None, vec![face(5), face(6)]);
        p.file.add_dissolved(Kind::Fursuit, vec![face(7), face(8)]).unwrap();
        p.save();
        let on_disk = std::fs::read(&path).unwrap();
        let before = p.file.clone();
        let (what, gone) = p.reset_unnamed(Kind::Fursuit).unwrap();
        assert_eq!((what.as_str(), gone.len()), ("Reset 2 unnamed fursuit groups", 2));
        assert!(p.file.get(&named).is_some() && p.file.get(&person).is_some(), "named fursuits and people stay");
        assert_eq!(p.file.unnamed(Kind::Fursuit).groups, 0);
        assert_eq!(SubjectsFile::load(&path), SubjectsFile { version: p.file.written_version(), ..p.file.clone() }, "saved");
        assert_eq!(p.undo.last().map(|u| u.1.as_str()), Some("Reset 2 unnamed fursuit groups"));
        assert_eq!(p.undo().as_deref(), Some("Reset 2 unnamed fursuit groups"));
        assert_eq!(p.file, before, "back exactly");
        assert_eq!(std::fs::read(&path).unwrap(), on_disk, "byte for byte on disk");
        assert_eq!(p.reset_unnamed(Kind::Pet), Err(Refused::Nothing));
        p.file.reset_unnamed(Kind::Fursuit);
        assert_eq!(p.reset_unnamed(Kind::Fursuit), Err(Refused::Nothing));
        assert!(p.undo().is_none(), "a refusal leaves nothing to undo");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Not the same" is the user's answer: kept by the groups' heads,
    /// one undo step, never across kinds, and gone with its kind's
    /// *Forget all*.
    #[test]
    fn not_the_same_is_remembered_undone_and_kept_to_its_kind() {
        let mut p = People::load(None);
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![face(1), face(2)]), (8, vec![face(3)])]);
        let person = GroupId::Subject(p.file.create(Kind::Person, Some("Someone".into()), vec![face(9)]));
        assert_eq!(p.not_same(&GroupId::Cluster(7), &person, vec![], vec![]), Err(Refused::MixedKinds));
        assert_eq!(p.not_same(&GroupId::Cluster(7), &GroupId::Cluster(7), vec![], vec![]), Err(Refused::Nothing));
        assert!(p.undo().is_none(), "a refusal leaves nothing to undo");
        p.not_same(&GroupId::Cluster(7), &GroupId::Cluster(8), vec![face(2)], vec![face(3)]).unwrap();
        assert_eq!(p.file.not_same.len(), 1);
        assert_eq!(p.file.not_same[0].a, [face(2), face(1)], "the heads shown first, then the rest");
        assert_eq!(p.file.subjects.len(), 1, "nothing named or merged");
        // After a regroup the clusters have new numbers: still told apart.
        p.set_other_clusters(Kind::Fursuit, vec![(70, vec![face(1), face(4)]), (80, vec![face(3), face(5)])]);
        let ix = p.file.not_same_index(Kind::Fursuit);
        assert!(subjects::told_apart(&ix, &[face(1), face(4)], &[face(3), face(5)]));
        assert_eq!(p.undo().as_deref(), Some("Said two groups are not the same"));
        assert!(p.file.not_same.is_empty());
        p.not_same(&GroupId::Cluster(70), &GroupId::Cluster(80), vec![], vec![]).unwrap();
        p.forget_kind(Kind::Person);
        assert_eq!(p.file.not_same.len(), 1, "people's forget leaves the fursuits' answers");
        p.forget_kind(Kind::Fursuit);
        assert!(p.file.not_same.is_empty());
    }

    fn h(n: u8) -> FaceRef {
        FaceRef::new(Key([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }

    /// The group editor's *Apply* in one step: a new group (named), heads
    /// to a named group and to an automatic one, a removal and a rename;
    /// one undo puts every part back.  Viewer-removed heads stay out.
    #[test]
    fn an_edit_applies_as_one_step_and_one_undo_takes_it_back() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        let a = p.file.create(k, None, vec![h(1), h(2), h(3), h(4), h(5)]);
        let kiba = p.file.create(k, Some("Kiba".into()), vec![h(10)]);
        p.set_other_clusters(k, vec![(7, vec![h(20), h(21)])]);
        p.file.remove_box(k, h(5)).unwrap();
        p.file.add_members(&a, vec![h(5)]).unwrap();
        let before = p.file.clone();
        let edit = GroupEdit {
            kind: k,
            targets: vec![EditTarget::Subject(kiba.clone()), EditTarget::ClusterOf(vec![h(20), h(21)]), EditTarget::Subject(a.clone())],
            new_names: vec![Some("Rook".into()), None],
            moves: vec![(h(2), EditDest::New(0)), (h(3), EditDest::Group(0)), (h(4), EditDest::Removed), (h(1), EditDest::Group(1)), (h(5), EditDest::Group(0)), (h(10), EditDest::Group(0))],
            renames: vec![(2, "Ash".into())],
            covers: Vec::new(),
        };
        let plan = p.plan_edit(&edit).unwrap();
        assert!(!plan.moves.iter().any(|m| m.0 == h(5)), "the viewer removed it");
        assert!(!plan.moves.iter().any(|m| m.0 == h(10)), "already in Kiba");
        let undo_depth = p.undo.len();
        let (words, made) = p.apply_edit(&plan).unwrap();
        assert_eq!(p.undo.len(), undo_depth + 1, "one undo step");
        assert_eq!(words, "Split into 2 groups, 1 head to Kiba, 1 head to an unnamed group, 1 removed, named Ash");
        assert_eq!(made.len(), 1, "the unused new group is not made");
        let rook = p.file.named(k, "Rook")[0].id.clone();
        assert_eq!(p.file.get(&rook).unwrap().members, vec![h(2)]);
        assert!(p.file.get(&kiba).unwrap().members.contains(&h(3)));
        let cluster_now = p.file.member_of(k, &h(20)).expect("the automatic group became a subject");
        assert!(cluster_now.members.contains(&h(1)) && cluster_now.members.contains(&h(21)));
        assert!(p.file.removed_index(k).covers(&h(4)));
        assert_eq!(p.file.get(&a).unwrap().name.as_deref(), Some("Ash"));
        p.undo().unwrap();
        assert_eq!(p.file.subjects, before.subjects, "every part back");
        assert!(!p.file.removed_index(k).covers(&h(4)));
    }

    /// The group photo, through the people state: a named group's own
    /// face, one undo step; the snapshot shows it at once and says it was
    /// chosen; it holds through a regroup; it goes back to the clearest
    /// when its face leaves (one edit, its own undo), or its detection
    /// isn't found again; an unnamed group can't have one.
    #[test]
    fn a_group_photo_is_one_step_and_survives_a_regroup() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        let kiba = p.file.create(k, Some("Kiba".into()), vec![h(1), h(2), h(9)]);
        let id = GroupId::Subject(kiba.clone());
        let unnamed = p.file.create(k, None, vec![h(4), h(5)]);
        p.set_other_clusters(k, vec![(7, vec![h(20), h(21)])]);
        assert_eq!(p.set_cover(&unnamed, Some(h(4))), Err(Refused::Nothing), "unnamed: automatic only");
        assert_eq!(p.set_cover(&kiba, Some(h(20))), Err(Refused::Nothing), "not its own");
        assert!(p.undo.is_empty(), "a refusal leaves no undo step");
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.group(&id).unwrap().cover.face, h(9), "automatic: the clearest");
        assert!(snap.chosen_cover(&id).is_none());
        assert_eq!(p.set_cover(&kiba, Some(h(1))).unwrap(), "Set Kiba's group photo");
        let snap = p.build_snapshot(&resolve);
        assert_eq!((snap.group(&id).unwrap().cover.face, snap.chosen_cover(&id)), (h(1), Some(h(1))), "shown at once");
        assert_eq!(snap.undoable.as_deref(), Some("Set Kiba's group photo"));
        // A regroup: other clusters, a new generation; the photo stays.
        p.set_other_clusters(k, vec![(70, vec![h(20)]), (71, vec![h(21)])]);
        p.reset_grouping();
        assert_eq!(p.build_snapshot(&resolve).group(&id).unwrap().cover.face, h(1));
        // Its detection not found again (a rebuild): the clearest, not chosen.
        let gone = p.build_snapshot_found(&resolve, &|_, f| *f != h(1));
        assert_eq!((gone.group(&id).unwrap().cover.face, gone.chosen_cover(&id)), (h(9), None));
        // One undo takes it back.
        assert_eq!(p.undo().as_deref(), Some("Set Kiba's group photo"));
        assert!(p.file.get(&kiba).unwrap().cover.is_none());
        // Set again, then its face goes to another group in the editor:
        // the clearest again, and that edit's undo brings the photo back.
        p.set_cover(&kiba, Some(h(1))).unwrap();
        let edit = GroupEdit { kind: k, targets: vec![EditTarget::Subject(unnamed.clone())], moves: vec![(h(1), EditDest::Group(0))], ..GroupEdit::default() };
        p.apply_edit(&p.plan_edit(&edit).unwrap()).unwrap();
        assert!(p.file.get(&kiba).unwrap().cover.is_none());
        assert_eq!(p.build_snapshot(&resolve).group(&id).unwrap().cover.face, h(9));
        p.undo().unwrap();
        assert_eq!(p.build_snapshot(&resolve).chosen_cover(&id), Some(h(1)));
        // Back to automatic is a step of its own.
        assert_eq!(p.set_cover(&kiba, None).unwrap(), "Kiba's group photo is automatic again");
        assert_eq!(p.build_snapshot(&resolve).group(&id).unwrap().cover.face, h(9));
        // People the same way, never a fursuit's head.
        let someone = p.file.create(Kind::Person, Some("Someone".into()), vec![h(30), h(31)]);
        p.set_cover(&someone, Some(h(30))).unwrap();
        assert_eq!(p.build_snapshot(&resolve).chosen_cover(&GroupId::Subject(someone)), Some(h(30)));
    }

    /// A merge of two named groups keeps the kept name's photo, through
    /// the people state, and one undo restores both.
    #[test]
    fn a_merge_keeps_the_kept_names_photo() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        let kiba = p.file.create(k, Some("Kiba".into()), vec![h(1), h(2)]);
        let rook = p.file.create(k, Some("Rook".into()), vec![h(3), h(9)]);
        p.set_cover(&kiba, Some(h(1))).unwrap();
        p.set_cover(&rook, Some(h(3))).unwrap();
        let before = p.file.clone();
        let into = p.merge(&GroupId::Subject(kiba.clone()), &[GroupId::Subject(rook.clone())]).unwrap();
        assert_eq!(into, kiba);
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.chosen_cover(&GroupId::Subject(kiba.clone())), Some(h(1)), "Kiba kept: Kiba's photo, not the clearer one");
        p.undo().unwrap();
        assert_eq!(p.file, before);
        p.merge(&GroupId::Subject(rook.clone()), &[GroupId::Subject(kiba)]).unwrap();
        assert_eq!(p.build_snapshot(&resolve).chosen_cover(&GroupId::Subject(rook)), Some(h(3)), "Rook kept: Rook's photo");
    }

    /// The editor's *Apply* chooses group photos in the same undo step as
    /// its moves; a photo alone is a step too; a photo whose face the
    /// same edit moves away is not set (and alone, changes nothing).
    #[test]
    fn an_edit_sets_group_photos_in_its_one_step() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        let kiba = p.file.create(k, Some("Kiba".into()), vec![h(1), h(2), h(3)]);
        let rook = p.file.create(k, Some("Rook".into()), vec![h(4)]);
        let edit = GroupEdit {
            kind: k,
            targets: vec![EditTarget::Subject(kiba.clone()), EditTarget::Subject(rook.clone())],
            moves: vec![(h(3), EditDest::Group(1))],
            covers: vec![(0, Some(h(2))), (1, Some(h(3)))],
            ..GroupEdit::default()
        };
        let plan = p.plan_edit(&edit).unwrap();
        assert_eq!(plan.covers.len(), 2);
        let depth = p.undo.len();
        let (words, _) = p.apply_edit(&plan).unwrap();
        assert_eq!(p.undo.len(), depth + 1, "one step");
        assert_eq!(words, "1 head to Rook, new group photo for Kiba, new group photo for Rook");
        assert_eq!((p.file.get(&kiba).unwrap().cover, p.file.get(&rook).unwrap().cover), (Some(h(2)), Some(h(3))), "the moved head is Rook's photo");
        p.undo().unwrap();
        assert!(p.file.get(&kiba).unwrap().cover.is_none() && p.file.get(&rook).unwrap().cover.is_none());
        // A photo alone.
        let only = GroupEdit { kind: k, targets: vec![EditTarget::Subject(kiba.clone())], covers: vec![(0, Some(h(1)))], ..GroupEdit::default() };
        assert_eq!(p.apply_edit(&p.plan_edit(&only).unwrap()).unwrap().0, "Set Kiba's group photo");
        // A photo the same edit sends away: not set, and nothing else to do.
        let away = GroupEdit { kind: k, targets: vec![EditTarget::Subject(rook.clone())], covers: vec![(0, Some(h(1)))], ..GroupEdit::default() };
        let depth = p.undo.len();
        assert_eq!(p.apply_edit(&p.plan_edit(&away).unwrap()), Err(Refused::Nothing));
        assert_eq!(p.undo.len(), depth, "no undo step for nothing");
    }

    /// A review's X on a detection and on a box the user drew, then Z: the
    /// detection leaves the removed list, the drawn box is drawn again
    /// (it had left `added` outright), and both rejoin the subject they
    /// were in; each is one step.
    #[test]
    fn x_then_z_brings_back_a_detection_and_a_drawn_box() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        p.file.add_box(k, h(2)).unwrap();
        let a = p.file.create(k, Some("Ash".into()), vec![h(1), h(2), h(3)]);
        let depth = p.undo.len();
        let (_, taken) = p.remove_faces(k, &[h(1), h(2)]).unwrap();
        assert_eq!(p.undo.len(), depth + 1, "one step");
        assert_eq!(taken.drawn, [h(2)]);
        assert!(p.file.removed_index(k).covers(&h(1)) && !p.file.added.iter().any(|l| l.face == h(2)));
        assert_eq!(p.file.get(&a).unwrap().members, [h(3)]);
        p.restore_faces(k, &[h(1), h(2)], &taken).unwrap();
        assert!(!p.file.removed_index(k).covers(&h(1)));
        assert!(p.file.added.contains(&subjects::Labelled { kind: k, face: h(2) }), "drawn again");
        let mut m = p.file.get(&a).unwrap().members.clone();
        m.sort_by_key(|f| f.key.0);
        assert_eq!(m, [h(1), h(2), h(3)]);
        assert_eq!(p.restore_faces(k, &[h(1), h(2)], &taken), Err(Refused::Nothing), "twice is once");
    }

    /// Apply survives a regroup landing while the editor is open: an
    /// automatic target is found by its heads under its new id; one that
    /// is gone becomes a group of its own.  A named target that is gone is
    /// refused (nothing changed), and so is one of another kind.
    #[test]
    fn an_edit_follows_its_heads_through_a_regroup_and_refuses_a_gone_subject() {
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        let a = p.file.create(k, None, vec![h(1), h(2)]);
        p.set_other_clusters(k, vec![(7, vec![h(20), h(21)])]);
        let edit = GroupEdit { kind: k, targets: vec![EditTarget::ClusterOf(vec![h(20), h(21)])], moves: vec![(h(2), EditDest::Group(0))], ..GroupEdit::default() };
        // The regroup: the same heads, a new id.
        p.set_other_clusters(k, vec![(9, vec![h(20), h(21)])]);
        let plan = p.plan_edit(&edit).unwrap();
        assert_eq!(plan.moves[0].2, PlanDest::Group(GroupId::Cluster(9)));
        assert_eq!(plan.moves[0].1, Some(GroupId::Subject(a.clone())), "from the group it is in when it applies");
        // Gone altogether: a new group of its own.
        p.set_other_clusters(k, Vec::new());
        let plan = p.plan_edit(&edit).unwrap();
        assert_eq!((plan.gone, plan.moves[0].2.clone()), (1, PlanDest::New(0)));
        p.apply_edit(&plan).unwrap();
        assert!(p.file.member_of(k, &h(2)).is_some_and(|s| s.id != a));
        // A named target deleted meanwhile: refused, nothing changed.
        let kiba = p.file.create(k, Some("Kiba".into()), vec![h(30)]);
        let gone = GroupEdit { kind: k, targets: vec![EditTarget::Subject(kiba.clone())], moves: vec![(h(1), EditDest::Group(0))], ..GroupEdit::default() };
        p.file.forget(&kiba).unwrap();
        let file = p.file.clone();
        assert_eq!(p.plan_edit(&gone), Err(Refused::NoSuchSubject(kiba)));
        assert_eq!(p.file.subjects, file.subjects);
        // Never across kinds.
        let person = p.file.create(Kind::Person, Some("Someone".into()), vec![h(40)]);
        let mixed = GroupEdit { kind: k, targets: vec![EditTarget::Subject(person)], moves: vec![(h(1), EditDest::Group(0))], ..GroupEdit::default() };
        assert_eq!(p.plan_edit(&mixed), Err(Refused::MixedKinds));
    }

    /// "Take out of this group" in bulk, for both kinds: the heads leave
    /// the group they are in and are rejected for it (an automatic group
    /// becomes a subject, so the rejection outlives a regroup), free for
    /// any other group; one undo puts them back.
    #[test]
    fn taken_out_heads_leave_are_rejected_and_stay_free_for_others() {
        for k in [Kind::Fursuit, Kind::Person] {
            let mut p = People::load(None);
            let a = p.file.create(k, Some("Kiba".into()), vec![h(1), h(2), h(3), h(4)]);
            let take = |heads: &[u8]| GroupEdit { kind: k, moves: heads.iter().map(|&n| (h(n), EditDest::TakenOut)).collect(), ..GroupEdit::default() };
            let before = p.file.clone();
            let plan = p.plan_edit(&take(&[2, 3])).unwrap();
            assert_eq!(plan.moves.iter().map(|m| m.1.clone()).collect::<Vec<_>>(), vec![Some(GroupId::Subject(a.clone())); 2]);
            let (words, _) = p.apply_edit(&plan).unwrap();
            assert_eq!(words, "2 taken out");
            let s = p.file.get(&a).unwrap();
            assert_eq!((s.members.clone(), s.rejected.clone()), (vec![h(1), h(4)], vec![h(2), h(3)]), "{k:?}");
            assert!(p.file.member_of(k, &h(2)).is_none(), "free");
            // Free for another group.
            let rook = p.file.create(k, Some("Rook".into()), vec![h(9)]);
            let to_rook = GroupEdit { kind: k, targets: vec![EditTarget::Subject(rook.clone())], moves: vec![(h(2), EditDest::Group(0))], ..GroupEdit::default() };
            let plan = p.plan_edit(&to_rook).unwrap();
            p.apply_edit(&plan).unwrap();
            assert!(p.file.get(&rook).unwrap().members.contains(&h(2)));
            p.undo().unwrap();
            p.undo().unwrap();
            assert_eq!(p.file.subjects.iter().filter(|s| s.id != rook).cloned().collect::<Vec<_>>(), before.subjects, "{k:?}: one undo each");
        }
        // Out of an automatic group: it becomes a subject holding the
        // rejection, so a regroup can't bring the head back.
        let mut p = People::load(None);
        let k = Kind::Fursuit;
        p.set_other_clusters(k, vec![(7, vec![h(20), h(21), h(22)])]);
        let plan = p.plan_edit(&GroupEdit { kind: k, moves: vec![(h(22), EditDest::TakenOut)], ..GroupEdit::default() }).unwrap();
        p.apply_edit(&plan).unwrap();
        let s = p.file.member_of(k, &h(20)).expect("a subject now").clone();
        assert_eq!((s.members.len(), s.rejected.clone()), (2, vec![h(22)]));
        // Taken out of no group: nothing to do.
        assert_eq!(p.apply_edit(&p.plan_edit(&GroupEdit { kind: k, moves: vec![(h(50), EditDest::TakenOut)], ..GroupEdit::default() }).unwrap()), Err(Refused::Nothing));
    }

    /// Two groups side by side: a person's faces come least like the rest
    /// first (never another kind's group); and after a move into an
    /// automatic group (which makes a subject of it, a new id) the group
    /// holding a face is found again, of its own kind only.
    #[test]
    fn faces_far_first_and_a_moved_face_is_found_again() {
        let d = std::env::temp_dir().join(format!("pp-people-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut p = People::load(None);
        let mut store = FaceStore::load(&d, ModelId::AuraFace);
        for (n, v) in [(1u8, [1.0f32, 0.0]), (2, [0.99, 0.14]), (3, [0.98, -0.2]), (4, [0.0, 1.0])] {
            store.add(Key([n; 16]), &[found(0.1, 80.0, &v)]).unwrap();
        }
        p.store = Some(store);
        let f = |n: u8| p.store.as_ref().unwrap().faces_of(&Key([n; 16]))[0].face_ref(Key([n; 16]));
        let (f1, f2, f3, f4) = (f(1), f(2), f(3), f(4));
        let sid = GroupId::Subject(p.file.create(Kind::Person, None, vec![f1, f2, f3, f4]));
        let far = p.far_order(&sid).unwrap();
        assert_eq!(far[0].0, f4, "the stray first: {far:?}");
        let suit = GroupId::Subject(p.file.create(Kind::Fursuit, None, vec![FaceRef::new(Key([9; 16]), [0.1, 0.1, 0.2, 0.2])]));
        assert!(p.far_order(&suit).is_none(), "a fursuit's group is not a person's");
        // A cluster published for fursuits never answers for a person.
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![FaceRef::new(Key([8; 16]), [0.1, 0.1, 0.2, 0.2])])]);
        assert_eq!(p.group_of(Kind::Fursuit, &FaceRef::new(Key([8; 16]), [0.1, 0.1, 0.2, 0.2])), Some(GroupId::Cluster(7)));
        assert_eq!(p.group_of(Kind::Person, &FaceRef::new(Key([8; 16]), [0.1, 0.1, 0.2, 0.2])), None);
        // Moved into a new group: found there.
        let to = p.move_faces(&sid, vec![f4], None).unwrap();
        assert_eq!(p.group_of(Kind::Person, &f4), Some(GroupId::Subject(to)));
        assert_eq!(p.group_of(Kind::Person, &f1), Some(sid));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Not fursuit heads" on whole groups (or picked heads): every box
    /// goes to the removed list in one undo step; a subject left empty
    /// goes with it; a group of another kind is refused and changes
    /// nothing; picked heads not in the group are ignored.
    #[test]
    fn whole_groups_are_removed_in_one_step_and_never_across_kinds() {
        let mut p = People::load(None);
        p.set_other_clusters(Kind::Fursuit, vec![(7, vec![face(1), face(2)])]);
        let suit = p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(3), face(4)]);
        let kept = p.file.create(Kind::Fursuit, Some("Kept".into()), vec![face(5), face(6)]);
        let person = p.file.create(Kind::Person, Some("Someone".into()), vec![face(8)]);
        let before = p.file.clone();
        // A person's group among fursuits: refused whole.
        let mixed = [(GroupId::Cluster(7), None), (GroupId::Subject(person.clone()), None)];
        assert_eq!(p.remove_boxes(Kind::Fursuit, &mixed), Err(Refused::MixedKinds));
        assert_eq!(p.remove_boxes(Kind::Person, &[(GroupId::Cluster(7), None)]), Err(Refused::MixedKinds));
        assert_eq!(p.file, before, "a refusal changes nothing");
        assert!(p.undo.is_empty());
        // Two whole groups: one step.
        let (what, boxes) = p.remove_boxes(Kind::Fursuit, &[(GroupId::Cluster(7), None), (GroupId::Subject(suit.clone()), None)]).unwrap();
        assert_eq!(what, "Removed 4 fursuit heads");
        assert_eq!(boxes, [face(1), face(2), face(3), face(4)]);
        assert!(p.file.get(&suit).is_none(), "the emptied subject goes");
        let removed = p.file.removed_index(Kind::Fursuit);
        assert!(boxes.iter().all(|b| removed.covers(b)));
        assert!(!p.file.removed_index(Kind::Person).covers(&face(1)), "a fursuit removal is no face's");
        assert!(p.file.get(&person).is_some() && p.file.get(&kept).unwrap().members.len() == 2);
        // Picked heads only; one not in the group is ignored.
        let (what, boxes) = p.remove_boxes(Kind::Fursuit, &[(GroupId::Subject(kept.clone()), Some(vec![face(5), face(9)]))]).unwrap();
        assert_eq!((what.as_str(), boxes), ("Removed a fursuit head from Kept", vec![face(5)]));
        assert_eq!(p.file.get(&kept).unwrap().members, [face(6)], "the rest stay, and so does the name");
        assert_eq!(p.remove_boxes(Kind::Fursuit, &[(GroupId::Subject(kept.clone()), Some(vec![face(9)]))]), Err(Refused::Nothing));
        // People, for symmetry: a named person's faces.
        assert_eq!(p.remove_boxes(Kind::Person, &[(GroupId::Subject(person.clone()), None)]).map(|r| r.0).as_deref(), Ok("Removed a face from Someone"));
        // One undo each.
        assert_eq!(p.undo().as_deref(), Some("Removed a face from Someone"));
        assert_eq!(p.undo().as_deref(), Some("Removed a fursuit head from Kept"));
        assert_eq!(p.undo().as_deref(), Some("Removed 4 fursuit heads"));
        assert_eq!(p.file, before, "all back as it was");
    }

    #[test]
    fn people_fursuits_and_pets_never_merge() {
        let mut p = People::load(None);
        let person = GroupId::Subject(p.file.create(Kind::Person, Some("Someone".into()), vec![face(1)]));
        let suit = GroupId::Subject(p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(2)]));
        let pet = GroupId::Subject(p.file.create(Kind::Pet, Some("Pet".into()), vec![face(3)]));
        assert_eq!(p.merge(&person, &[suit.clone()]), Err(Refused::MixedKinds));
        assert_eq!(p.merge(&pet, &[person.clone(), suit.clone()]), Err(Refused::MixedKinds));
        assert_eq!(p.move_faces(&person, vec![face(1)], Some(&suit)), Err(Refused::MixedKinds));
        assert_eq!(p.move_faces(&suit, vec![face(2)], Some(&pet)), Err(Refused::MixedKinds));
        assert!(p.undo().is_none(), "a refusal leaves nothing to undo");
        assert_eq!(p.file.subjects.len(), 3);
        assert_eq!(p.named(Kind::Person, "someone"), [person.clone()]);
        assert!(p.named(Kind::Fursuit, "someone").is_empty(), "a name is of one kind");
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.groups.iter().map(|g| g.kind).collect::<HashSet<_>>().len(), 3);
    }
}
