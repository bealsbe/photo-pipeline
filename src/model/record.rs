//! One physical image file.
//!
//! Each file is its own record regardless of pairing; pair relationships are
//! tracked with `pair_stem` (the lowercase stem shared by a RAW and a JPG).

use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDateTime, TimeZone};

use super::crop::Crop;
pub use crate::exif::ShotMeta;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FileType {
    Jpg,
    Raw,
}

impl FileType {
    pub fn label(self) -> &'static str {
        match self {
            FileType::Jpg => "JPG",
            FileType::Raw => "RAW",
        }
    }

    /// Classify by extension (lowercase, with the leading dot).
    pub fn from_ext(ext: &str) -> Option<FileType> {
        if RAW_EXTENSIONS.contains(&ext) {
            Some(FileType::Raw)
        } else if JPG_EXTENSIONS.contains(&ext) {
            Some(FileType::Jpg)
        } else {
            None
        }
    }

    pub fn from_path(path: &Path) -> Option<FileType> {
        FileType::from_ext(&util::suffix(path).to_lowercase())
    }
}

pub const RAW_EXTENSIONS: &[&str] = &[
    ".cr2", ".cr3", // Canon
    ".nef", ".nrw", // Nikon
    ".arw", ".srw", // Sony / Samsung
    ".orf", // Olympus
    ".rw2", // Panasonic
    ".raf", // Fujifilm
    ".dng", // Adobe DNG
    ".pef", // Pentax
    ".x3f", // Sigma
    ".3fr", // Hasselblad
    ".mef", // Mamiya
    ".rwl", // Leica
    ".erf", // Epson
];

pub const JPG_EXTENSIONS: &[&str] = &[".jpg", ".jpeg"];

/// A modification time the way Python's `datetime.fromtimestamp(st_mtime)`
/// sees it: local wall-clock time, rounded to the microsecond.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MTime {
    pub local: NaiveDateTime,
    /// `datetime.timestamp()` of the above — the value the disk thumbnail
    /// cache keys on, so both apps share cache entries.
    pub timestamp: f64,
}

impl MTime {
    pub fn from_unix(secs: i64, nanos: u32) -> MTime {
        // CPython: st_mtime = sec + nsec * 1e-9, then fromtimestamp() rounds
        // the fraction to microseconds, half-to-even.
        let d = secs as f64 + nanos as f64 * 1e-9;
        let mut int = d.trunc();
        let mut frac = ((d - int) * 1e6).round_ties_even();
        if frac >= 1e6 {
            frac -= 1e6;
            int += 1.0;
        } else if frac < 0.0 {
            frac += 1e6;
            int -= 1.0;
        }
        let secs = int as i64;
        let us = frac as u32;
        let local = Local
            .timestamp_opt(secs, us * 1000)
            .earliest()
            .map(|dt| dt.naive_local())
            .unwrap_or_default();
        MTime {
            local,
            timestamp: secs as f64 + us as f64 / 1e6,
        }
    }

    pub fn from_metadata(meta: &std::fs::Metadata) -> MTime {
        let (secs, nanos) = unix_mtime(meta);
        MTime::from_unix(secs, nanos)
    }
}

pub fn unix_mtime(meta: &std::fs::Metadata) -> (i64, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.mtime(), meta.mtime_nsec() as u32)
    }
    #[cfg(not(unix))]
    {
        match meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
            Some(d) => (d.as_secs() as i64, d.subsec_nanos()),
            None => (0, 0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PhotoRecord {
    pub path: PathBuf,
    pub file_type: FileType,
    pub file_size: u64,
    pub modified: MTime,
    /// EXIF DateTimeOriginal; `None` = unknown.
    pub capture_time: Option<NaiveDateTime>,

    pub is_pruned: bool,
    pub is_marked: bool,
    pub pair_stem: Option<String>,
    pub crop: Option<Crop>,
    /// Sub-second time, press sequence number and AF point, from the same
    /// header read as the capture time (not part of the Python app's data).
    pub shot: ShotMeta,

    /// Cached `filename.lower()` — the Name sort key.
    pub name_lower: String,
}

impl PhotoRecord {
    pub fn new(
        path: PathBuf,
        file_type: FileType,
        file_size: u64,
        modified: MTime,
        capture_time: Option<NaiveDateTime>,
    ) -> Self {
        let name_lower = util::file_name(&path).to_lowercase();
        Self {
            path,
            file_type,
            file_size,
            modified,
            capture_time,
            is_pruned: false,
            is_marked: false,
            pair_stem: None,
            crop: None,
            shot: ShotMeta::default(),
            name_lower,
        }
    }

    /// Best available date: EXIF capture time, falling back to mtime.
    pub fn shot_time(&self) -> NaiveDateTime {
        self.capture_time.unwrap_or(self.modified.local)
    }

    pub fn filename(&self) -> String {
        util::file_name(&self.path)
    }

    pub fn stem(&self) -> String {
        util::stem(&self.path)
    }

    pub fn stem_lower(&self) -> String {
        self.stem().to_lowercase()
    }

    pub fn is_paired(&self) -> bool {
        self.pair_stem.is_some()
    }

    pub fn is_cropped(&self) -> bool {
        self.crop.as_ref().is_some_and(|c| !c.is_full())
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.name_lower = util::file_name(&path).to_lowercase();
        self.path = path;
    }
}
