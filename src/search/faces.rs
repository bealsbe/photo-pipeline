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
use crate::model::subjects::{self, Face, FaceRef, Framing, Group, GroupId, Kind, Refused, Subject, SubjectId, Subjects, SubjectsFile, UNDO_DEPTH, private_dir, private_file};
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
    pub vectors: FaceVectors,
    tiles: Arc<Mutex<TileIndex>>,
}

impl FaceStore {
    fn faces_file(dir: &Path) -> PathBuf {
        dir.join("faces.bin")
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
        let mut store = FaceStore { dir: dir.to_path_buf(), model, scanned: HashMap::new(), vectors: FaceVectors::default(), tiles: Arc::new(Mutex::new(TileIndex { path: Self::tiles_file(dir), offsets: HashMap::new() })) };
        if let Ok(data) = fs::read(Self::faces_file(dir)) {
            let bs = blocks(&data);
            if bs.first().is_some_and(|h| Self::read_faces_header(h)) {
                for b in &bs[1..] {
                    let Some(entries) = Self::read_faces_block(b) else { break };
                    for (k, d) in entries {
                        store.scanned.insert(k, d);
                    }
                }
            }
        }
        store.load_vectors();
        store.load_tiles();
        store
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
        let mut out: Vec<(Key, Vec<FaceSpot>)> = self
            .scanned
            .iter()
            .filter_map(|(k, dets)| {
                let missing: Vec<FaceSpot> = dets.iter().filter(|d| !self.vectors.by_ref.contains_key(&d.face_ref(*k))).map(Detection::spot).collect();
                (!missing.is_empty()).then(|| (*k, missing))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn faces_of(&self, key: &Key) -> &[Detection] {
        self.scanned.get(key).map_or(&[], Vec::as_slice)
    }

    pub fn detection(&self, r: &FaceRef) -> Option<&Detection> {
        self.faces_of(&r.key).iter().find(|d| d.face_ref(r.key) == *r)
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

    /// Every face, as (ref, detection).
    pub fn all(&self) -> impl Iterator<Item = (FaceRef, &Detection)> {
        self.scanned.iter().flat_map(|(k, dets)| dets.iter().map(move |d| (d.face_ref(*k), d)))
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
        let dropped = before - self.scanned.len();
        if dropped > 0 && dropped as f64 > COMPACT_SHARE * before as f64 {
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
        let _ = fs::remove_file(Self::tiles_file(&self.dir));
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("facevecs-") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        self.scanned.clear();
        self.vectors = FaceVectors::default();
        self.tiles.lock().offsets.clear();
    }

    /// Bytes the face files in an index folder take (the Storage row's
    /// *People*; a stat walk, on a worker).
    pub fn bytes_in(dir: &Path) -> u64 {
        let mut n = 0;
        for p in [Self::faces_file(dir), Self::tiles_file(dir)] {
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
    /// Earlier states of the file with what changed them, newest last.
    undo: Vec<(SubjectsFile, String)>,
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
        self.undo.push((self.file.clone(), what.into()));
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// Take the last edit back; returns what was undone.
    pub fn undo(&mut self) -> Option<String> {
        let (file, what) = self.undo.pop()?;
        self.file = file;
        self.save();
        Some(what)
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
    fn members(&self, id: &GroupId) -> Result<(Kind, Vec<FaceRef>), Refused> {
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

    /// *Forget all people* (or fursuits, or pets): that kind's subjects
    /// go from the user's file, and for people the face cache too; the
    /// other kinds' names, answers and caches stay untouched.  Beyond
    /// undo's reach, like [`People::forget_all`].
    pub fn forget_kind(&mut self, kind: Kind) {
        self.file.subjects.retain(|s| s.kind != kind);
        self.undo.clear();
        if self.file.subjects.is_empty() {
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
    pub fn build_snapshot(&self, resolve: &dyn Fn(&FaceRef) -> Option<Face>) -> Arc<Subjects> {
        let mut groups: Vec<Group> = Vec::new();
        let group_of = |id: GroupId, kind: Kind, name: Option<String>, hidden: bool, cover: Option<FaceRef>, members: &[FaceRef], suggestions: usize| -> Option<Group> {
            let faces: Vec<Face> = members.iter().filter_map(resolve).collect();
            let cover = cover.and_then(|c| resolve(&c)).or_else(|| faces.iter().cloned().max_by(|a, b| a.clarity.total_cmp(&b.clarity)))?;
            let photos = faces.iter().map(|f| f.face.key).collect::<HashSet<_>>().len();
            let days = faces.iter().map(|f| f.day).collect::<HashSet<_>>().len();
            Some(Group { id, kind, name, hidden, photos, days, cover, suggestions })
        };
        for s in &self.file.subjects {
            let suggestions = self.grouped.suggestion_counts.get(&s.id).copied().unwrap_or(0);
            groups.extend(group_of(GroupId::Subject(s.id.clone()), s.kind, s.name.clone(), s.hidden, s.cover, &s.members, suggestions));
        }
        if let Some(store) = &self.store {
            let refs = store.refs();
            for (c, &id) in self.grouped.clusters.iter().zip(&self.grouped.ids) {
                let members: Vec<FaceRef> = c.members.iter().filter_map(|&m| refs.get(m).copied()).collect();
                groups.extend(group_of(GroupId::Cluster(id), Kind::Person, None, false, None, &members, 0));
            }
        }
        groups.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(matches!(b.id, GroupId::Subject(_)).cmp(&matches!(a.id, GroupId::Subject(_)))).then(b.photos.cmp(&a.photos)).then_with(|| a.name.cmp(&b.name)));
        let undoable = self.undo.last().map(|(_, what)| what.clone());
        let faces_found = self.store.as_ref().map_or(0, FaceStore::found);
        Arc::new(Subjects { generation: self.generation, groups, faces_found, undoable })
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
                let pool: Vec<usize> = (0..refs.len()).filter(|&i| !confirmed.contains_key(&refs[i]) && !s.rejected.contains(&refs[i])).collect();
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
                let best = self.store.as_ref().map_or(0.0, |s| s.faces_of(k).iter().map(|d| self.clarity(&d.face_ref(*k))).fold(0.0, f32::max));
                (*k, best)
            })
            .collect()
    }
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
