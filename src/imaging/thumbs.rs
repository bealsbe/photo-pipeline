//! Background thumbnail generation.
//!
//! Decode strategy per file type (unchanged from the Python app):
//!
//! JPG — 1. the camera's embedded MPF preview (≈1616×1080), rotated by the
//!          *primary* image's EXIF orientation;
//!       2. otherwise the primary image with DCT-domain downscaling.
//!
//! RAW — 1. LibRaw's embedded preview (JPEG or bitmap), rotated by the
//!          preview's own EXIF orientation (LibRaw's flip when it has none);
//!       2. a half-size demosaic when there is no preview.
//!
//! Results are cached on disk (JPEG q92, shared with the Python app — same
//! directory, same key) and in memory in a byte-budgeted LRU of GPU-ready
//! handles.  Each thumbnail also carries a half- and quarter-size level so
//! small tiles are drawn from a close resolution rather than minified 4×.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use iced::widget::image::Handle;
use md5::{Digest, Md5};

use super::decode::{self, Pixels};
use super::libraw::{Raw, Thumbnail};
use super::pool::PriorityPool;
use crate::bus::{Bus, BgEvent};
use crate::exif;
use crate::model::record::{FileType, PhotoRecord};
use crate::util::py_float_repr;

pub const GENERATION_SIZE: u32 = 300;
const JPEG_QUALITY: u8 = 92;

pub const PRIORITY_VISIBLE: i32 = 10;
pub const PRIORITY_PREFETCH: i32 = 2;

/// Which generator a job belongs to (the grid and the calendar keep
/// separate caches but share the worker threads).
pub type OwnerId = u32;
pub const OWNER_MAIN: OwnerId = 1;
pub const OWNER_CALENDAR: OwnerId = 2;
pub const OWNER_SEARCH: OwnerId = 3;
/// A person's photos (People, fursuits and pets).
pub const OWNER_PERSON: OwnerId = 4;

/// One mip level of a thumbnail, ready for the GPU.
#[derive(Clone, Debug)]
pub struct Level {
    pub handle: Handle,
    pub width: u32,
    pub height: u32,
}

/// A decoded thumbnail: the full generation size plus smaller levels.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub levels: Vec<Level>,
    pub bytes: usize,
    pub arrived: Instant,
    /// `false` for the placeholder drawn when every decoder failed.
    pub ok: bool,
}

impl Thumb {
    pub fn width(&self) -> u32 {
        self.levels[0].width
    }

    pub fn height(&self) -> u32 {
        self.levels[0].height
    }

    /// The smallest level at least `target_px` on its long edge.
    pub fn level_for(&self, target_px: f32) -> &Level {
        let mut best = &self.levels[0];
        for l in &self.levels[1..] {
            if (l.width.max(l.height) as f32) >= target_px * 0.95 {
                best = l;
            }
        }
        best
    }

    fn from_pixels(px: Pixels, ok: bool) -> Thumb {
        let mut levels = Vec::with_capacity(3);
        let mut bytes = 0;
        let mut cur = px;
        for i in 0..3 {
            let next = if i < 2 && cur.width.max(cur.height) >= 64 { Some(decode::half(&cur)) } else { None };
            let (w, h) = (cur.width, cur.height);
            let rgba = decode::to_rgba(cur);
            bytes += rgba.len();
            levels.push(Level { handle: Handle::from_rgba(w, h, rgba), width: w, height: h });
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        Thumb { levels, bytes, arrived: Instant::now(), ok }
    }
}

// ── disk cache ──────────────────────────────────────────────────────────────

/// Persistent thumbnail cache: `~/.cache/photo_pipeline/thumbs/300/<md5>.jpg`,
/// keyed on `"{path}:{mtime}:{size}:{thumb_size}"` exactly as the Python app
/// keys it, so the two share entries.  Legacy `.png` entries are read too.
pub struct DiskCache {
    dir: PathBuf,
    size: u32,
}

impl DiskCache {
    pub fn new(size: u32) -> Self {
        let base = dirs::home_dir().unwrap_or_default().join(".cache").join("photo_pipeline").join("thumbs");
        let dir = base.join(size.to_string());
        let _ = fs::create_dir_all(&dir);
        Self { dir, size }
    }

    pub fn digest(&self, path: &Path, mtime: f64, file_size: u64) -> String {
        let posix = path.to_string_lossy().replace('\\', "/");
        let key = format!("{posix}:{}:{file_size}:{}", py_float_repr(mtime), self.size);
        let mut h = Md5::new();
        h.update(key.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn get(&self, digest: &str) -> Option<Pixels> {
        let jpg = self.dir.join(format!("{digest}.jpg"));
        if let Ok(data) = fs::read(&jpg) {
            if let Ok(px) = decode::decode_jpeg_full(&data) {
                return Some(px);
            }
        }
        let png = self.dir.join(format!("{digest}.png"));
        let data = fs::read(png).ok()?;
        decode::decode_png(&data)
    }

    pub fn put(&self, digest: &str, px: &Pixels) {
        let target = self.dir.join(format!("{digest}.jpg"));
        if target.exists() || self.dir.join(format!("{digest}.png")).exists() {
            return;
        }
        let mut out = Vec::new();
        let rgb = decode::to_rgb(px);
        let enc = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
        if enc.encode(&rgb, px.width as u16, px.height as u16, jpeg_encoder::ColorType::Rgb).is_err() {
            return;
        }
        // Unique temp name: two workers may race on the same key.
        let tmp = self.dir.join(format!("{digest}.{:016x}.tmp", unique()));
        if fs::write(&tmp, &out).is_ok() && fs::rename(&tmp, &target).is_ok() {
            return;
        }
        let _ = fs::remove_file(&tmp);
    }
}

fn unique() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.finish()
}

// ── decoding ────────────────────────────────────────────────────────────────

/// Decode one thumbnail, fitting within `size`×`size`.  `None` when every
/// strategy failed.
pub fn generate(path: &Path, file_type: FileType, size: u32) -> Option<Pixels> {
    match file_type {
        FileType::Jpg => jpeg_preview(path, size).or_else(|| jpeg_primary(path, size)),
        FileType::Raw => raw_thumbnail(path, size),
    }
}

fn jpeg_preview(path: &Path, size: u32) -> Option<Pixels> {
    let (data, head) = exif::jpeg_preview_bytes(path, size)?;
    let (w, h) = decode::jpeg_dimensions(&data)?;
    // Reject a preview that would have to be upscaled.
    if w.max(h) < size {
        return None;
    }
    let px = decode::decode_jpeg_scaled(&data, size, size).ok()?;
    // The preview is stored in sensor orientation; its own EXIF is not to be
    // trusted, so rotate by the primary image's orientation.
    Some(decode::apply_orientation(px, exif::orientation_from_jpeg_head(&head)))
}

fn jpeg_primary(path: &Path, size: u32) -> Option<Pixels> {
    let data = fs::read(path).ok()?;
    let px = decode::decode_jpeg_scaled(&data, size, size).ok()?;
    Some(decode::apply_orientation(px, exif::orientation_from_jpeg_head(&data)))
}

fn raw_thumbnail(path: &Path, size: u32) -> Option<Pixels> {
    let mut raw = Raw::open(path).ok()?;
    let flip = raw.exif_orientation();
    // The smallest embedded preview that covers the thumbnail: bodies that
    // also embed a full-size JPEG would otherwise decode 30+ MP for 300 px.
    if let Ok(Thumbnail::Jpeg(bytes)) = raw.thumbnail_at_least(size) {
        if let Some(px) = decode::jpeg_dimensions(&bytes)
            .filter(|&(w, h)| w.max(h) >= size)
            .and_then(|_| decode::decode_jpeg_scaled(&bytes, size, size).ok())
        {
            return Some(decode::apply_orientation(px, embedded_orientation(&bytes, flip)));
        }
    }
    // As rawpy's extract_thumb(): the default (largest) preview.  Reopened,
    // since choosing a preview above changed LibRaw's thumbnail state.
    let mut raw = Raw::open(path).ok()?;
    match raw.thumbnail() {
        Ok(Thumbnail::Jpeg(bytes)) => {
            if let Ok(px) = decode::decode_jpeg_scaled(&bytes, size, size) {
                return Some(decode::apply_orientation(px, embedded_orientation(&bytes, flip)));
            }
        }
        Ok(Thumbnail::Bitmap { width, height, colors, data }) => {
            if let Some(px) = bitmap(width, height, colors, data) {
                return Some(decode::fit(px, size));
            }
        }
        Err(_) => {}
    }
    // No usable embedded preview: a half-size demosaic.
    let mut raw = Raw::open(path).ok()?;
    let rgb = raw.process(true).ok()?;
    Some(decode::fit(Pixels::new(rgb.width, rgb.height, 3, rgb.data), size))
}

/// Orientation for an embedded RAW preview: its own EXIF tag (what Qt's
/// auto-transform applied), else the RAW's orientation.
pub fn embedded_orientation(jpeg: &[u8], raw_flip: u8) -> u8 {
    let has_tag = exif::exif_tiff(jpeg).is_some_and(|(tiff, end, ifd0, _)| {
        exif::ifd_entries(&mut exif::BytesSource(tiff), end, ifd0).iter().any(|e| e.0 == 0x0112)
    });
    if has_tag { exif::orientation_from_jpeg_head(jpeg) } else { raw_flip }
}

pub fn bitmap(width: u32, height: u32, colors: u32, data: Vec<u8>) -> Option<Pixels> {
    let need = width as usize * height as usize * colors as usize;
    if data.len() < need {
        return None;
    }
    match colors {
        1 => Some(Pixels::new(width, height, 1, data[..need].to_vec())),
        3 => Some(Pixels::new(width, height, 3, data[..need].to_vec())),
        4 => Some(Pixels::new(width, height, 4, data[..need].to_vec())),
        _ => None,
    }
}

/// Solid placeholder for files no decoder could read.
fn placeholder(file_type: FileType, size: u32) -> Pixels {
    let c = match file_type {
        FileType::Raw => [22u8, 36, 60],
        FileType::Jpg => [42, 42, 42],
    };
    let data = c.iter().copied().cycle().take(size as usize * size as usize * 3).collect();
    Pixels::new(size, size, 3, data)
}

// ── generator ───────────────────────────────────────────────────────────────

pub struct Job {
    owner: OwnerId,
    path: PathBuf,
    file_type: FileType,
    mtime: f64,
    file_size: u64,
}

/// Worker pool shared by every thumbnail generator.
pub struct ThumbWorkers {
    pool: PriorityPool<Job>,
}

impl ThumbWorkers {
    pub fn new(bus: Bus) -> Self {
        let disk = Arc::new(DiskCache::new(GENERATION_SIZE));
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(4, 8);
        let pool = PriorityPool::new("thumb", n, move |job: Job| {
            let digest = disk.digest(&job.path, job.mtime, job.file_size);
            let thumb = if let Some(px) = disk.get(&digest) {
                Thumb::from_pixels(px, true)
            } else {
                match generate(&job.path, job.file_type, GENERATION_SIZE) {
                    Some(px) => {
                        disk.put(&digest, &px);
                        Thumb::from_pixels(px, true)
                    }
                    None => Thumb::from_pixels(placeholder(job.file_type, GENERATION_SIZE), false),
                }
            };
            let _ = bus.unbounded_send(BgEvent::Thumb { owner: job.owner, path: job.path, thumb: Arc::new(thumb) });
        });
        Self { pool }
    }
}

/// Byte-bounded LRU of decoded thumbnails for one owner.
pub struct ThumbCache {
    owner: OwnerId,
    max_bytes: usize,
    map: HashMap<PathBuf, Arc<Thumb>>,
    order: VecDeque<PathBuf>,
    bytes: usize,
    queued: HashSet<PathBuf>,
}

impl ThumbCache {
    pub fn new(owner: OwnerId, max_bytes: usize) -> Self {
        Self {
            owner,
            max_bytes,
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            queued: HashSet::new(),
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Arc<Thumb>> {
        self.map.get(path)
    }

    /// Queue a thumbnail unless it is cached or already queued.
    pub fn request(&mut self, workers: &ThumbWorkers, record: &PhotoRecord, priority: i32) {
        if self.map.contains_key(&record.path) || self.queued.contains(&record.path) {
            return;
        }
        self.queued.insert(record.path.clone());
        workers.pool.submit(
            self.owner,
            priority,
            Job {
                owner: self.owner,
                path: record.path.clone(),
                file_type: record.file_type,
                mtime: record.modified.timestamp,
                file_size: record.file_size,
            },
        );
    }

    /// Drop queued-but-not-started jobs; running ones still land.
    pub fn clear_queue(&mut self, workers: &ThumbWorkers) {
        workers.pool.clear(self.owner);
        self.queued.clear();
    }

    pub fn clear(&mut self, workers: &ThumbWorkers) {
        self.clear_queue(workers);
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// A worker finished.  Returns true if the thumbnail was stored.
    pub fn deliver(&mut self, path: PathBuf, thumb: Arc<Thumb>) -> bool {
        self.queued.remove(&path);
        if let Some(old) = self.map.remove(&path) {
            self.bytes -= old.bytes;
            self.order.retain(|p| p != &path);
        }
        self.bytes += thumb.bytes;
        self.map.insert(path.clone(), thumb);
        self.order.push_back(path);
        // Always keep the most recent entry, however large.
        while self.bytes > self.max_bytes && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front() {
                if let Some(t) = self.map.remove(&old) {
                    self.bytes -= t.bytes;
                }
            }
        }
        true
    }

    /// Drop the placeholders of thumbnails that failed, so they are asked
    /// for again (after a share that was away comes back).
    pub fn forget_failed(&mut self) -> usize {
        let failed: Vec<PathBuf> = self.map.iter().filter(|(_, t)| !t.ok).map(|(p, _)| p.clone()).collect();
        for p in &failed {
            if let Some(t) = self.map.remove(p) {
                self.bytes -= t.bytes;
            }
        }
        if !failed.is_empty() {
            self.order.retain(|p| self.map.contains_key(p));
        }
        failed.len()
    }

    /// Mark an entry as recently used.
    pub fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            if pos + 1 != self.order.len() {
                let p = self.order.remove(pos).expect("in range");
                self.order.push_back(p);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Thumbnails asked for and not yet delivered.
    pub fn pending(&self) -> usize {
        self.queued.len()
    }

    /// Whether `path` is asked for and not yet delivered.  Clearing the
    /// queue forgets it, so a caller that still wants it can ask again.
    pub fn is_queued(&self, path: &Path) -> bool {
        self.queued.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_python() {
        // hashlib.md5(b"/tmp/a.jpg:1700000000.25:1234:300").hexdigest()
        let dc = DiskCache { dir: PathBuf::from("/tmp"), size: 300 };
        assert_eq!(dc.digest(Path::new("/tmp/a.jpg"), 1_700_000_000.25, 1234), "5b5d3eccecdf560552d5e0cc5eec3fca");
    }
}
