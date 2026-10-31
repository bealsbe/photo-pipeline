//! Per-folder sidecar files, in exactly the Python app's formats so marks
//! made in either app carry over.
//!
//! | File                          | Content                                   |
//! |-------------------------------|-------------------------------------------|
//! | `.photo_pipeline.json`        | `{"pruned": [rel paths]}`                 |
//! | `.photo-pipeline-export.json` | `{"marked": [rel paths]}`                 |
//! | `.photo-pipeline-pairs.json`  | `{"paired_stems": [{parent, stem}]}`      |
//! | `.photo-pipeline-crops.json`  | `{"crops": {rel: {left, top, …}}}`        |
//!
//! Every write goes through [`atomic_write_text`] (`ops::fsutil`'s atomic
//! write: temp file, fsync, rename, directory fsync).  A crash leaves
//! either the old sidecar or the new one, never a torn file.
//!
//! The app saves through a [`Saver`] (`model::saver`), which writes on its
//! own thread: on a network share one save takes 10–300 ms, which held up
//! every P or M.  It keeps a save that failed and writes it again later.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::crop::Crop;
use crate::util::{py_json, relative_or_absolute, resolve_stored};

pub const PRUNE_FILE: &str = ".photo_pipeline.json";
pub const EXPORT_FILE: &str = ".photo-pipeline-export.json";
pub const CROPS_FILE: &str = ".photo-pipeline-crops.json";

/// Write `content` to `path` atomically, replacing what was there (a
/// sidecar is always the newest marks).
///
/// [`fsutil::atomic_write`](crate::ops::fsutil::atomic_write): the temp
/// sibling is fsynced before the rename and the directory after it, so the
/// new contents survive a power cut once this returns.
pub fn atomic_write_text(path: &Path, content: &str) -> std::io::Result<()> {
    crate::ops::fsutil::atomic_write(path, content.as_bytes(), true)
}

#[cfg(test)]
fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

#[cfg(test)]
fn read_path_list(folder: &Path, file: &str, key: &str) -> Option<HashSet<PathBuf>> {
    let data = read_json(&folder.join(file))?;
    path_list_from(folder, &data, key)
}

fn parse_path_list(folder: &Path, text: &str, key: &str) -> Option<HashSet<PathBuf>> {
    let data: Value = serde_json::from_str(text).ok()?;
    path_list_from(folder, &data, key)
}

fn path_list_from(folder: &Path, data: &Value, key: &str) -> Option<HashSet<PathBuf>> {
    let list = data.get(key)?.as_array()?;
    let mut out = HashSet::new();
    for v in list {
        // Python's Path(rel) would raise on a non-string and drop the lot.
        let s = v.as_str()?;
        out.insert(resolve_stored(folder, s));
    }
    Some(out)
}

#[cfg(test)]
fn write_path_list<'a>(
    folder: &Path,
    file: &str,
    key: &str,
    paths: impl Iterator<Item = &'a Path>,
) -> std::io::Result<()> {
    atomic_write_text(&folder.join(file), &path_list_text(folder, key, paths))
}

fn path_list_text<'a>(folder: &Path, key: &str, paths: impl Iterator<Item = &'a Path>) -> String {
    let list: Vec<Value> = paths.map(|p| Value::String(relative_or_absolute(p, folder))).collect();
    let mut m = Map::new();
    m.insert(key.into(), Value::Array(list));
    py_json(&Value::Object(m))
}

/// The prune sidecar's text for `paths`.
pub fn prune_text<'a>(folder: &Path, paths: impl Iterator<Item = &'a Path>) -> String {
    path_list_text(folder, "pruned", paths)
}

pub fn export_text<'a>(folder: &Path, paths: impl Iterator<Item = &'a Path>) -> String {
    path_list_text(folder, "marked", paths)
}

/// Prune marks from a sidecar's text.
pub fn parse_prune_marks(folder: &Path, text: &str) -> Option<HashSet<PathBuf>> {
    parse_path_list(folder, text, "pruned")
}

pub fn parse_export_marks(folder: &Path, text: &str) -> Option<HashSet<PathBuf>> {
    parse_path_list(folder, text, "marked")
}

/// The prune marks on disk (the app reads through `model::saver`, which
/// knows what is still waiting to be written; the quality evaluation and
/// the tests read the file itself).
#[cfg(test)]
pub fn read_prune_marks(folder: &Path) -> Option<HashSet<PathBuf>> {
    read_path_list(folder, PRUNE_FILE, "pruned")
}

/// Write the prune sidecar directly (the app saves through `model::saver`).
#[cfg(test)]
pub fn write_prune_marks<'a>(folder: &Path, paths: impl Iterator<Item = &'a Path>) -> std::io::Result<()> {
    write_path_list(folder, PRUNE_FILE, "pruned", paths)
}

/// The export marks on disk (see [`read_prune_marks`]).
#[cfg(test)]
pub fn read_export_marks(folder: &Path) -> Option<HashSet<PathBuf>> {
    read_path_list(folder, EXPORT_FILE, "marked")
}

/// Persisted crops as {absolute path: Crop}.  Malformed entries are skipped.
#[cfg(test)]
pub fn read_crops(folder: &Path) -> HashMap<PathBuf, Crop> {
    match read_json(&folder.join(CROPS_FILE)) {
        Some(data) => crops_from(folder, &data),
        None => HashMap::new(),
    }
}

/// Crops from a sidecar's text.
pub fn parse_crops(folder: &Path, text: &str) -> HashMap<PathBuf, Crop> {
    match serde_json::from_str::<Value>(text) {
        Ok(data) => crops_from(folder, &data),
        Err(_) => HashMap::new(),
    }
}

fn crops_from(folder: &Path, data: &Value) -> HashMap<PathBuf, Crop> {
    let mut out = HashMap::new();
    let Some(crops) = data.get("crops").and_then(|v| v.as_object()) else {
        return out;
    };
    for (rel, entry) in crops {
        if let Some(crop) = Crop::from_json(entry) {
            out.insert(resolve_stored(folder, rel), crop);
        }
    }
    out
}

/// Persist crops directly (the app saves through `model::saver` with
/// [`crops_text`]).  Full-frame crops are dropped, and an empty set removes
/// the sidecar rather than leaving an empty one behind.
#[cfg(test)]
pub fn write_crops<'a>(folder: &Path, crops: impl Iterator<Item = (&'a Path, &'a Crop)>) -> std::io::Result<()> {
    let target = folder.join(CROPS_FILE);
    let Some(text) = crops_text(folder, crops) else {
        match fs::remove_file(&target) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    };
    atomic_write_text(&target, &text)
}

/// The crops sidecar's text, or `None` when there are no crops (the
/// sidecar is removed rather than left empty).
pub fn crops_text<'a>(folder: &Path, crops: impl Iterator<Item = (&'a Path, &'a Crop)>) -> Option<String> {
    let mut entries: Vec<(String, Value)> = crops
        .filter(|(_, c)| !c.is_full())
        .map(|(p, c)| (relative_or_absolute(p, folder), c.to_json()))
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut m = Map::new();
    for (k, v) in entries {
        m.insert(k, v);
    }
    Some(py_json(&json!({ "crops": Value::Object(m) })))
}

// ── background saving ───────────────────────────────────────────────────────

/// Which sidecar a save writes (for coalescing and error messages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Prune,
    Export,
    Crops,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Prune => "prune marks",
            Kind::Export => "export marks",
            Kind::Crops => "crops",
        }
    }
}

pub use super::saver::Saver;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-sidecar-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn prune_marks_round_trip_in_python_format() {
        let d = tmpdir("prune");
        let a = d.join("RAW/IMG_1.ARW");
        write_prune_marks(&d, [a.as_path()].into_iter()).unwrap();
        let text = fs::read_to_string(d.join(PRUNE_FILE)).unwrap();
        assert_eq!(text, "{\n  \"pruned\": [\n    \"RAW/IMG_1.ARW\"\n  ]\n}");
        assert!(read_prune_marks(&d).unwrap().contains(&a));
        assert!(!d.join(".photo_pipeline.json.~tmp").exists());
    }

    #[test]
    fn crops_sidecar_removed_when_empty() {
        let d = tmpdir("crops");
        let p = d.join("a.jpg");
        let c = Crop::new(0.1, 0.1, 0.9, 0.9, Some("1:1".into()));
        write_crops(&d, [(p.as_path(), &c)].into_iter()).unwrap();
        assert_eq!(read_crops(&d).get(&p), Some(&c));
        write_crops(&d, std::iter::empty()).unwrap();
        assert!(!d.join(CROPS_FILE).exists());
    }
}
