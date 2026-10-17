//! Pictures for a burst sheet's tiles once they outgrow their thumbnails
//! (`ui::burst_sheet`, made bigger with Ctrl+wheel or Ctrl+=).
//!
//! Each is the camera's embedded preview, decoded straight down to the
//! tile's size class (`EDGES`) with the JPEG's DCT scaling: never the full
//! image, and never bigger than the preview itself.  A file without an
//! embedded preview keeps its thumbnail.
//!
//! Only the tiles on screen and near it are asked for (the sheet reports
//! them), on their own worker threads, the ones on screen first; asking
//! again drops what was queued and not started.  The cache is bounded by
//! bytes and never lets go of a tile on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::decode::{self, Pixels};
use super::libraw::{Raw, Thumbnail};
use super::loader::FullImage;
use super::pool::PriorityPool;
use super::thumbs::{GENERATION_SIZE, embedded_orientation};
use crate::bus::{BgEvent, Bus};
use crate::exif;
use crate::model::record::FileType;

/// The sizes pictures are made at (long edge, device pixels).
pub const EDGES: [u32; 3] = [640, 1024, 1600];
/// Pictures kept, at most (about two dozen of the largest).
const BUDGET: usize = 192 * 1024 * 1024;
const OWNER: u32 = 0;
const PRIO_SHOWN: i32 = 10;
const PRIO_NEAR: i32 = 2;

/// The size class for a tile whose photo is `px` device pixels on its long
/// edge: `None` while the thumbnail is close enough.
pub fn edge_for(px: u32) -> Option<u32> {
    if px as f32 <= GENERATION_SIZE as f32 * 1.15 {
        return None;
    }
    Some(EDGES.iter().copied().find(|&e| e as f32 * 1.1 >= px as f32).unwrap_or(EDGES[EDGES.len() - 1]))
}

/// The embedded preview of `path`, decoded to fit `edge` × `edge` (no
/// bigger than it is), the right way up.
pub fn make(path: &Path, file_type: FileType, edge: u32) -> Option<Pixels> {
    let (bytes, orientation) = match file_type {
        FileType::Jpg => {
            let (data, head) = exif::jpeg_preview_bytes(path, 0)?;
            (data, exif::orientation_from_jpeg_head(&head))
        }
        FileType::Raw => {
            let mut raw = Raw::open(path).ok()?;
            let flip = raw.exif_orientation();
            let data = match raw.thumbnail_at_least(edge) {
                Ok(Thumbnail::Jpeg(d)) => d,
                // None that big: the largest there is (reopened, as
                // choosing one changed LibRaw's state).
                _ => match Raw::open(path).ok()?.thumbnail() {
                    Ok(Thumbnail::Jpeg(d)) => d,
                    _ => return None,
                },
            };
            let orientation = embedded_orientation(&data, flip);
            (data, orientation)
        }
    };
    let (w, h) = decode::jpeg_dimensions(&bytes)?;
    // A preview no bigger than the thumbnail adds nothing.
    if w.max(h) <= GENERATION_SIZE {
        return None;
    }
    let fit = edge.min(w.max(h));
    let px = decode::decode_jpeg_scaled(&bytes, fit, fit).ok()?;
    Some(decode::apply_orientation(px, orientation))
}

struct Job {
    path: PathBuf,
    file_type: FileType,
    edge: u32,
}

struct Entry {
    image: Arc<FullImage>,
    edge: u32,
    used: u64,
}

pub struct TilePreviews {
    pool: PriorityPool<Job>,
    entries: HashMap<PathBuf, Entry>,
    bytes: usize,
    /// Asked for and not back yet, at what size.
    asked: HashMap<PathBuf, u32>,
    /// On screen: never let go.
    shown: HashSet<PathBuf>,
    clock: u64,
}

impl TilePreviews {
    pub fn new(bus: Bus) -> Self {
        let pool = PriorityPool::new("tiles", 2, move |job: Job| {
            let image = make(&job.path, job.file_type, job.edge).map(|px| Arc::new(FullImage::single(px)));
            let _ = bus.unbounded_send(BgEvent::TilePreview { path: job.path, edge: job.edge, image });
        });
        Self { pool, entries: HashMap::new(), bytes: 0, asked: HashMap::new(), shown: HashSet::new(), clock: 0 }
    }

    /// The picture held for `path`, if any.
    pub fn get(&self, path: &Path) -> Option<&Arc<FullImage>> {
        self.entries.get(path).map(|e| &e.image)
    }

    /// The tiles on screen (`shown`) and those near them (`near`, nearest
    /// first), wanted at size class `edge`.  What was queued for any other
    /// tile is dropped; what is held already, at that size or bigger, is
    /// kept and counts as used.
    pub fn want(&mut self, shown: &[(PathBuf, FileType)], near: &[(PathBuf, FileType)], edge: u32) {
        self.pool.clear(OWNER);
        self.asked.clear();
        self.shown = shown.iter().map(|(p, _)| p.clone()).collect();
        for (list, prio) in [(shown, PRIO_SHOWN), (near, PRIO_NEAR)] {
            for (path, ft) in list {
                self.clock += 1;
                if let Some(e) = self.entries.get_mut(path) {
                    e.used = self.clock;
                    if e.edge >= edge {
                        continue;
                    }
                }
                if self.asked.get(path).is_some_and(|&a| a >= edge) {
                    continue;
                }
                self.asked.insert(path.clone(), edge);
                self.pool.submit(OWNER, prio, Job { path: path.clone(), file_type: *ft, edge });
            }
        }
        self.trim();
    }

    /// A picture came back (`None`: the file has none to give).  `false`
    /// when it isn't kept (one as big or bigger is held already).
    pub fn deliver(&mut self, path: &Path, edge: u32, image: Option<Arc<FullImage>>) -> bool {
        if self.asked.get(path) == Some(&edge) {
            self.asked.remove(path);
        }
        let Some(image) = image else { return false };
        if self.entries.get(path).is_some_and(|e| e.edge >= edge) {
            return false;
        }
        self.clock += 1;
        if let Some(old) = self.entries.insert(path.to_path_buf(), Entry { image: image.clone(), edge, used: self.clock }) {
            self.bytes -= old.image.bytes;
        }
        self.bytes += image.bytes;
        self.trim();
        true
    }

    /// Let go of everything (the viewer closed).
    pub fn clear(&mut self) {
        self.pool.clear(OWNER);
        self.asked.clear();
        self.shown.clear();
        self.entries.clear();
        self.bytes = 0;
    }

    /// Over budget: the least recently used go first, never one on screen.
    fn trim(&mut self) {
        while self.bytes > BUDGET {
            let Some(oldest) = self.entries.iter().filter(|(p, _)| !self.shown.contains(*p)).min_by_key(|(_, e)| e.used).map(|(p, _)| p.clone()) else { break };
            if let Some(e) = self.entries.remove(&oldest) {
                self.bytes -= e.image.bytes;
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn shutdown(&mut self) {
        self.pool.clear_all();
        self.asked.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(bytes: usize) -> Option<Arc<FullImage>> {
        Some(Arc::new(FullImage { levels: Vec::new(), width: 100, height: 100, bytes, preview: true }))
    }

    fn files(names: &[&str]) -> Vec<(PathBuf, FileType)> {
        names.iter().map(|n| (PathBuf::from(format!("/mnt/nas/shoot/{n}.jpg")), FileType::Jpg)).collect()
    }

    #[test]
    fn the_size_class_follows_the_tile() {
        // Thumbnail-sized tiles need nothing more.
        assert_eq!(edge_for(200), None);
        assert_eq!(edge_for(340), None);
        assert_eq!(edge_for(400), Some(640));
        assert_eq!(edge_for(700), Some(640), "a little bigger than a class still uses it");
        assert_eq!(edge_for(900), Some(1024));
        assert_eq!(edge_for(1300), Some(1600));
        assert_eq!(edge_for(3000), Some(1600), "never more than the largest");
    }

    #[test]
    fn tiles_on_screen_are_never_let_go() {
        let (bus, _rx) = crate::bus::channel();
        let mut t = TilePreviews::new(bus);
        let shown = files(&["a", "b", "c"]);
        let big = BUDGET / 2;
        t.want(&shown, &[], 1600);
        for (p, _) in &shown {
            assert!(t.deliver(p, 1600, fake(big)));
        }
        // Over budget, but all three are on screen.
        assert_eq!(t.len(), 3);
        // Scrolled on: the new ones are on screen, the old ones go (least
        // recently used first) to make room.
        let next = files(&["d", "e"]);
        t.want(&next, &[], 1600);
        for (p, _) in &next {
            t.deliver(p, 1600, fake(big));
        }
        assert!(next.iter().all(|(p, _)| t.get(p).is_some()));
        assert!(t.bytes() <= BUDGET || t.len() == next.len());
        assert!(shown.iter().filter(|(p, _)| t.get(p).is_some()).count() <= 1);
    }

    #[test]
    fn a_smaller_picture_never_replaces_a_bigger_one() {
        let (bus, _rx) = crate::bus::channel();
        let mut t = TilePreviews::new(bus);
        let f = files(&["a"]);
        t.want(&f, &[], 1600);
        assert!(t.deliver(&f[0].0, 1600, fake(1000)));
        assert!(!t.deliver(&f[0].0, 640, fake(10)));
        assert_eq!(t.bytes(), 1000);
        // No picture to give: the thumbnail stays.
        assert!(!t.deliver(Path::new("/mnt/nas/shoot/none.jpg"), 640, None));
    }
}
