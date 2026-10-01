//! Full-resolution loading for the viewer.
//!
//! RAW files show the camera's embedded JPEG (fast, camera-rendered) and fall
//! back to a full demosaic; JPGs decode at full size.  Both are rotated by
//! their EXIF orientation.
//!
//! Each image is kept as a mip pyramid (full, ½, ¼ …) so the view can draw a
//! level close to the on-screen size: minifying an 8000 px frame straight to
//! a 1400 px window on the GPU would alias badly.
//!
//! The cache is bounded by bytes with a floor of three entries (current ±1),
//! and the preload radius shrinks to what the budget can hold, so a 42 MP
//! shoot preloads ±1 rather than decoding a neighbour it is about to evict.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::image::{Allocation, Handle};

use super::decode::{self, Pixels};
use super::libraw::{Raw, Thumbnail};
use super::pool::PriorityPool;
use super::thumbs::{Level, embedded_orientation};
use crate::bus::{Bus, BgEvent};
use crate::exif;
use crate::model::record::FileType;

const CACHE_BYTES: usize = 768 * 1024 * 1024;
const CACHE_FLOOR: usize = 3;
const PRELOAD_RADIUS: usize = 2;
const PRIO_NOW: i32 = 10;
const PRIO_AHEAD: i32 = 1;
const OWNER: u32 = 0;
/// Stop halving once the long edge drops below this.
const MIN_LEVEL_EDGE: u32 = 900;
/// A stand-in preview must be at least this big to be worth showing.
const PREVIEW_MIN_EDGE: u32 = 1000;
/// Previews kept (about 7 MB each for a 1616 × 1080 preview).
const PREVIEW_CACHE: usize = 12;

#[derive(Debug)]
pub struct FullImage {
    /// Level 0 is full resolution; each next level is half the previous.
    /// A preview has one smaller level standing in for the full frame.
    pub levels: Vec<Level>,
    /// The full image's size — also for a preview, so framing, zoom and
    /// crop maths are already right before the real pixels arrive.
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub preview: bool,
}

impl FullImage {
    pub(crate) fn from_pixels(px: Pixels) -> FullImage {
        let (width, height) = (px.width, px.height);
        let mut pyramid = vec![px];
        while pyramid.last().is_some_and(|p| p.width.max(p.height) >= MIN_LEVEL_EDGE * 2) {
            let next = decode::half(pyramid.last().expect("non-empty"));
            pyramid.push(next);
        }
        let mut bytes = 0;
        let levels = pyramid
            .into_iter()
            .map(|p| {
                let (w, h) = (p.width, p.height);
                let rgba = decode::to_rgba(p);
                bytes += rgba.len();
                Level { handle: Handle::from_rgba(w, h, rgba), width: w, height: h }
            })
            .collect();
        FullImage { levels, width, height, bytes, preview: false }
    }

    /// `px` shown in place of a `width`×`height` image.
    fn preview_of(px: Pixels, width: u32, height: u32) -> FullImage {
        let (w, h) = (px.width, px.height);
        let rgba = decode::to_rgba(px);
        let bytes = rgba.len();
        let level = Level { handle: Handle::from_rgba(w, h, rgba), width: w, height: h };
        FullImage { levels: vec![level], width, height, bytes, preview: true }
    }

    /// Index of the smallest level that is still at least `scale` of full
    /// size (so the GPU never minifies by more than 2×).
    pub fn level_for_scale(&self, scale: f32) -> usize {
        let mut idx = 0;
        for (i, l) in self.levels.iter().enumerate() {
            let s = l.width as f32 / self.width as f32;
            if s + 1e-4 >= scale {
                idx = i;
            }
        }
        idx
    }
}

/// Decode one image for the viewer.
pub fn load(path: &Path, file_type: FileType) -> Result<Pixels, String> {
    if file_type == FileType::Raw {
        if let Some(px) = load_raw(path) {
            return Ok(px);
        }
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let px = decode::decode_jpeg_rgba(&data).map_err(|_| "Could not decode image".to_string())?;
    Ok(decode::apply_orientation(px, exif::orientation_from_jpeg_head(&data)))
}

fn load_raw(path: &Path) -> Option<Pixels> {
    let mut raw = Raw::open(path).ok()?;
    let flip = raw.exif_orientation();
    if let Ok(Thumbnail::Jpeg(bytes)) = raw.thumbnail() {
        if let Ok(px) = decode::decode_jpeg_rgba(&bytes) {
            return Some(decode::apply_orientation(px, embedded_orientation(&bytes, flip)));
        }
    }
    // Full demosaic — no half size, viewer quality.
    let mut raw = Raw::open(path).ok()?;
    let rgb = raw.process(false).ok()?;
    Some(Pixels::new(rgb.width, rgb.height, 3, rgb.data))
}

/// The camera's embedded preview (about 1616 px), labelled with the size of
/// the image [`load`] will produce, to show while that decodes.  `None` when
/// there is no suitable preview or it frames the picture differently (a
/// letterboxed preview would jump when the real image replaces it).
pub fn load_preview(path: &Path, file_type: FileType) -> Option<FullImage> {
    let (bytes, orientation, full) = match file_type {
        FileType::Jpg => {
            let (data, head) = exif::jpeg_preview_bytes(path, PREVIEW_MIN_EDGE)?;
            let full = decode::jpeg_dimensions(&head)?;
            // As for thumbnails: rotate by the primary image's orientation.
            (data, exif::orientation_from_jpeg_head(&head), full)
        }
        FileType::Raw => {
            let mut raw = Raw::open(path).ok()?;
            let flip = raw.exif_orientation();
            let full = raw.default_thumb_size()?;
            let Ok(Thumbnail::Jpeg(data)) = raw.thumbnail_at_least(PREVIEW_MIN_EDGE) else { return None };
            let orientation = embedded_orientation(&data, flip);
            (data, orientation, full)
        }
    };
    let (pw, ph) = decode::jpeg_dimensions(&bytes)?;
    // Nothing to gain if the "preview" is the full image itself.
    if pw.max(ph) < PREVIEW_MIN_EDGE || pw as u64 * ph as u64 >= full.0 as u64 * full.1 as u64 {
        return None;
    }
    if ((pw as f64 / ph as f64) / (full.0 as f64 / full.1 as f64) - 1.0).abs() > 0.01 {
        return None;
    }
    let px = decode::apply_orientation(decode::decode_jpeg_rgba(&bytes).ok()?, orientation);
    let (w, h) = if orientation >= 5 { (full.1, full.0) } else { full };
    Some(FullImage::preview_of(px, w, h))
}

pub struct LoadJob {
    path: PathBuf,
    file_type: FileType,
    preview: bool,
}

struct Entry {
    path: PathBuf,
    image: Arc<FullImage>,
    /// GPU residency for the level the viewer draws at fit size.
    allocations: Vec<Allocation>,
}

pub struct Loader {
    pool: PriorityPool<LoadJob>,
    entries: VecDeque<Entry>,
    bytes: usize,
    loading: HashSet<PathBuf>,
    /// Stand-ins for images still decoding (dropped when the full arrives).
    previews: VecDeque<Entry>,
    previewing: HashSet<PathBuf>,
    /// Images on screen in the compare view: never evicted.
    pinned: HashSet<PathBuf>,
}

impl Loader {
    pub fn new(bus: Bus) -> Self {
        let pool = PriorityPool::new("viewer", 3, move |job: LoadJob| {
            if job.preview {
                let image = load_preview(&job.path, job.file_type).map(Arc::new);
                let _ = bus.unbounded_send(BgEvent::Preview { path: job.path, image });
                return;
            }
            let result = load(&job.path, job.file_type)
                .map(|px| Arc::new(FullImage::from_pixels(px)))
                .map_err(|e| if e.is_empty() { "Could not decode image".into() } else { e });
            let _ = bus.unbounded_send(BgEvent::Full { path: job.path, result });
        });
        Self {
            pool,
            entries: VecDeque::new(),
            bytes: 0,
            loading: HashSet::new(),
            previews: VecDeque::new(),
            previewing: HashSet::new(),
            pinned: HashSet::new(),
        }
    }

    /// Keep these images cached however full the cache gets, for as long as
    /// they are on screen together (the compare view shows up to four,
    /// which can be most of the budget).  An empty list lets them go.
    pub fn pin(&mut self, paths: Vec<PathBuf>) {
        self.pinned = paths.into_iter().collect();
    }

    /// The full image if cached, else its preview.
    pub fn get(&mut self, path: &Path) -> Option<Arc<FullImage>> {
        let Some(pos) = self.entries.iter().position(|e| e.path == path) else {
            return self.previews.iter().find(|e| e.path == path).map(|e| e.image.clone());
        };
        let e = self.entries.remove(pos).expect("in range");
        let img = e.image.clone();
        self.entries.push_back(e);
        Some(img)
    }

    /// The full image if cached, else its preview (no LRU touch).
    pub fn peek(&self, path: &Path) -> Option<&Arc<FullImage>> {
        self.entries
            .iter()
            .find(|e| e.path == path)
            .or_else(|| self.previews.iter().find(|e| e.path == path))
            .map(|e| &e.image)
    }

    /// The full image is cached.
    pub fn contains(&self, path: &Path) -> bool {
        self.entries.iter().any(|e| e.path == path)
    }

    /// Load now at high priority (no-op if cached or in flight).  The
    /// embedded preview is queued just ahead, to show while this decodes.
    pub fn request(&mut self, path: &Path, file_type: FileType) {
        self.submit(path, file_type, PRIO_NOW);
    }

    pub fn preload(&mut self, path: &Path, file_type: FileType) {
        self.submit(path, file_type, PRIO_AHEAD);
    }

    /// Only the embedded preview, now (`urgent`) or ahead.  Quick look
    /// browses on previews and asks for the full image once it settles, so
    /// full decodes don't hold up the next photo's preview.
    pub fn request_preview(&mut self, path: &Path, file_type: FileType, urgent: bool) {
        if self.contains(path) || self.previews.iter().any(|e| e.path == path) {
            return;
        }
        if self.previewing.insert(path.to_path_buf()) {
            let prio = if urgent { PRIO_NOW } else { PRIO_AHEAD } + 1;
            self.pool.submit(OWNER, prio, LoadJob { path: path.to_path_buf(), file_type, preview: true });
        }
    }

    fn submit(&mut self, path: &Path, file_type: FileType, prio: i32) {
        if self.contains(path) {
            return;
        }
        let has_preview = self.previews.iter().any(|e| e.path == path);
        if !has_preview && self.previewing.insert(path.to_path_buf()) {
            self.pool.submit(OWNER, prio + 1, LoadJob { path: path.to_path_buf(), file_type, preview: true });
        }
        if self.loading.insert(path.to_path_buf()) {
            self.pool.submit(OWNER, prio, LoadJob { path: path.to_path_buf(), file_type, preview: false });
        }
    }

    /// Preload up to ±radius around `center`, trimmed to the budget.
    pub fn preload_range(&mut self, items: &[(PathBuf, FileType)], center: usize) {
        let radius = PRELOAD_RADIUS.min(self.affordable_radius());
        let lo = center.saturating_sub(radius);
        let hi = (center + radius).min(items.len().saturating_sub(1));
        for i in lo..=hi {
            if i != center {
                if let Some((p, t)) = items.get(i) {
                    self.preload(p, *t);
                }
            }
        }
    }

    fn affordable_radius(&self) -> usize {
        let largest = self.entries.iter().map(|e| e.image.bytes).max().unwrap_or(0);
        if largest == 0 {
            return PRELOAD_RADIUS;
        }
        let capacity = CACHE_FLOOR.max(CACHE_BYTES / largest);
        ((capacity - 1) / 2).max(1)
    }

    /// Drop queued-but-not-started loads.
    pub fn clear_queue(&mut self) {
        self.pool.clear(OWNER);
        self.loading.clear();
        self.previewing.clear();
    }

    /// A preview job finished; `false` when it is no longer wanted (the
    /// full image got there first, or there was no preview).
    pub fn deliver_preview(&mut self, path: &Path, image: Option<&Arc<FullImage>>) -> bool {
        self.previewing.remove(path);
        let Some(image) = image else { return false };
        if self.contains(path) || self.previews.iter().any(|e| e.path == path) {
            return false;
        }
        self.previews.push_back(Entry { path: path.to_path_buf(), image: image.clone(), allocations: Vec::new() });
        while self.previews.len() > PREVIEW_CACHE {
            self.previews.pop_front();
        }
        true
    }

    /// A worker finished.
    pub fn deliver(&mut self, path: &Path, image: &Arc<FullImage>) {
        self.loading.remove(path);
        self.previews.retain(|e| e.path != path);
        if let Some(pos) = self.entries.iter().position(|e| e.path == path) {
            let e = self.entries.remove(pos).expect("in range");
            self.bytes -= e.image.bytes;
        }
        self.bytes += image.bytes;
        self.entries.push_back(Entry { path: path.to_path_buf(), image: image.clone(), allocations: Vec::new() });
        // Never evict below the floor: dropping a neighbour the viewer is
        // about to slide to would make navigation re-decode it.
        while self.bytes > CACHE_BYTES && self.entries.len() > CACHE_FLOOR {
            // Pinned images stay, even over budget: they are on screen.
            let Some(pos) = self.entries.iter().position(|e| !self.pinned.contains(&e.path)) else { break };
            if let Some(old) = self.entries.remove(pos) {
                self.bytes -= old.image.bytes;
            }
        }
    }

    pub fn failed(&mut self, path: &Path) {
        self.loading.remove(path);
    }

    /// Keep a GPU allocation alive for as long as the image stays cached.
    pub fn hold_allocation(&mut self, path: &Path, allocation: Allocation) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == path) {
            e.allocations.push(allocation);
        } else if let Some(e) = self.previews.iter_mut().find(|e| e.path == path) {
            e.allocations.push(allocation);
        }
    }

    pub fn shutdown(&mut self) {
        self.pool.clear_all();
        self.loading.clear();
        self.previewing.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(bytes: usize) -> Arc<FullImage> {
        Arc::new(FullImage { levels: Vec::new(), width: 100, height: 100, bytes, preview: false })
    }

    #[test]
    fn pinned_images_stay_cached_over_budget() {
        let (bus, _rx) = crate::bus::channel();
        let mut l = Loader::new(bus);
        let big = CACHE_BYTES / 3;
        let shown: Vec<PathBuf> = (0..4).map(|i| PathBuf::from(format!("/shown/{i}.jpg"))).collect();
        l.pin(shown.clone());
        l.deliver(Path::new("/other/a.jpg"), &fake(big));
        for p in &shown {
            l.deliver(p, &fake(big));
        }
        l.deliver(Path::new("/other/b.jpg"), &fake(big));
        assert!(shown.iter().all(|p| l.contains(p)), "an image on screen was evicted");
        assert!(!l.contains(Path::new("/other/a.jpg")));
        // Unpinned, the oldest go first again.
        l.pin(Vec::new());
        l.deliver(Path::new("/other/c.jpg"), &fake(big));
        assert!(!l.contains(&shown[0]));
        assert!(l.contains(Path::new("/other/c.jpg")));
    }
}
