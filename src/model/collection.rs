//! In-memory collection of records for the current session.
//!
//! Records live in a slab addressed by [`RecordId`]; ids stay valid for the
//! life of the collection even as other records are removed.  Insertion
//! order is preserved for iteration (it is the scan order, which the "no
//! sort" paths rely on).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::record::{FileType, PhotoRecord};
use crate::util::canonical_parent;

pub type RecordId = u32;

/// (canonical parent, lowercase stem) — the identity of a shot.
pub type PairKey = (PathBuf, String);

/// A pair key as persisted in the sidecar: (absolute parent string, stem).

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub raw: usize,
    pub jpg: usize,
    /// Individual records that are paired (both sides counted).
    pub paired: usize,
    pub unpaired: usize,
    pub pruned: usize,
    pub marked: usize,
}

#[derive(Debug, Default, Clone)]
pub struct Collection {
    slots: Vec<Option<PhotoRecord>>,
    order: Vec<RecordId>,
    by_path: HashMap<PathBuf, RecordId>,
    pair_index: HashMap<PairKey, Vec<RecordId>>,
    /// Per slot: the RAW/JPG partner, as `build_pairs` last found them.
    pair_of: Vec<Option<RecordId>>,
    /// Records came, went or moved since `build_pairs`: `pair_of` is stale.
    pairs_dirty: bool,
    stats: Stats,
    /// Bumped whenever records come, go, move or pair up (not for marks),
    /// so what is derived from those (bursts) knows when to look again.
    generation: u64,
}

pub fn pair_key(path: &Path) -> PairKey {
    (canonical_parent(path), crate::util::stem(path).to_lowercase())
}

impl Collection {
    pub fn new() -> Self {
        Self::default()
    }

    // ── mutation ────────────────────────────────────────────────────────

    pub fn add(&mut self, record: PhotoRecord) -> RecordId {
        if let Some(&existing) = self.by_path.get(&record.path) {
            // Same file reported twice — keep one record, like a path-keyed dict.
            self.remove(existing);
        }
        let id = self.slots.len() as RecordId;
        let s = &mut self.stats;
        s.total += 1;
        match record.file_type {
            FileType::Raw => s.raw += 1,
            FileType::Jpg => s.jpg += 1,
        }
        if record.is_paired() {
            s.paired += 1;
        } else {
            s.unpaired += 1;
        }
        if record.is_pruned {
            s.pruned += 1;
        }
        if record.is_marked {
            s.marked += 1;
        }
        self.pair_index.entry(pair_key(&record.path)).or_default().push(id);
        self.by_path.insert(record.path.clone(), id);
        self.slots.push(Some(record));
        self.pairs_dirty = true;
        self.order.push(id);
        self.generation += 1;
        id
    }

    pub fn clear(&mut self) {
        let generation = self.generation + 1;
        *self = Self::default();
        self.generation = generation;
    }

    /// Changes whenever records are added, removed, moved or paired.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn remove(&mut self, id: RecordId) -> Option<PhotoRecord> {
        let record = self.slots.get_mut(id as usize)?.take()?;
        self.generation += 1;
        self.pairs_dirty = true;
        let s = &mut self.stats;
        s.total -= 1;
        match record.file_type {
            FileType::Raw => s.raw -= 1,
            FileType::Jpg => s.jpg -= 1,
        }
        if record.is_paired() {
            s.paired -= 1;
        } else {
            s.unpaired -= 1;
        }
        if record.is_pruned {
            s.pruned -= 1;
        }
        if record.is_marked {
            s.marked -= 1;
        }
        self.by_path.remove(&record.path);
        let key = pair_key(&record.path);
        if let Some(bucket) = self.pair_index.get_mut(&key) {
            bucket.retain(|&r| r != id);
            if bucket.is_empty() {
                self.pair_index.remove(&key);
            }
        }
        self.order.retain(|&r| r != id);
        Some(record)
    }

    /// Re-key a record after its file has been moved.
    pub fn update_path(&mut self, id: RecordId, new_path: PathBuf) {
        let Some(record) = self.slots.get(id as usize).and_then(|r| r.as_ref()) else {
            return;
        };
        let old_path = record.path.clone();
        let old_key = pair_key(&old_path);
        self.generation += 1;
        self.pairs_dirty = true;
        if let Some(bucket) = self.pair_index.get_mut(&old_key) {
            bucket.retain(|&r| r != id);
            if bucket.is_empty() {
                self.pair_index.remove(&old_key);
            }
        }
        self.by_path.remove(&old_path);
        self.by_path.insert(new_path.clone(), id);
        self.pair_index.entry(pair_key(&new_path)).or_default().push(id);
        if let Some(Some(record)) = self.slots.get_mut(id as usize) {
            record.set_path(new_path);
        }
    }

    /// Recount pruned / marked from ground truth after bulk flag changes.
    pub fn recount_flags(&mut self) {
        let (mut pruned, mut marked) = (0, 0);
        for r in self.iter() {
            pruned += r.is_pruned as usize;
            marked += r.is_marked as usize;
        }
        self.stats.pruned = pruned;
        self.stats.marked = marked;
    }

    // ── queries ─────────────────────────────────────────────────────────

    pub fn get(&self, id: RecordId) -> Option<&PhotoRecord> {
        self.slots.get(id as usize)?.as_ref()
    }

    pub fn get_mut(&mut self, id: RecordId) -> Option<&mut PhotoRecord> {
        self.slots.get_mut(id as usize)?.as_mut()
    }

    pub fn id_of(&self, path: &Path) -> Option<RecordId> {
        self.by_path.get(path).copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Live ids in insertion order.
    pub fn ids(&self) -> &[RecordId] {
        &self.order
    }

    pub fn iter(&self) -> impl Iterator<Item = &PhotoRecord> + '_ {
        self.order.iter().filter_map(|&id| self.get(id))
    }

    pub fn iter_ids(&self) -> impl Iterator<Item = (RecordId, &PhotoRecord)> + '_ {
        self.order.iter().filter_map(|&id| self.get(id).map(|r| (id, r)))
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    // ── pairing ─────────────────────────────────────────────────────────

    /// Pair every RAW and JPG that share a lowercase stem in the same
    /// canonical folder (the `RAW/` / `JPG/` level stripped, so pairs survive
    /// a Sort).  Pairing is automatic: there is nothing to confirm or store.
    pub fn build_pairs(&mut self) {
        let mut paired_count = 0;
        let mut updates: Vec<(RecordId, Option<String>)> = Vec::with_capacity(self.order.len());
        for ((_, stem), ids) in &self.pair_index {
            let mut has_raw = false;
            let mut has_jpg = false;
            for &id in ids {
                match self.get(id).map(|r| r.file_type) {
                    Some(FileType::Raw) => has_raw = true,
                    Some(FileType::Jpg) => has_jpg = true,
                    None => {}
                }
            }
            let value = (has_raw && has_jpg).then(|| stem.clone());
            if value.is_some() {
                paired_count += ids.len();
            }
            for &id in ids {
                updates.push((id, value.clone()));
            }
        }
        for (id, v) in updates {
            if let Some(r) = self.get_mut(id) {
                r.pair_stem = v;
            }
        }
        self.pair_of = vec![None; self.slots.len()];
        for ids in self.pair_index.values() {
            for &id in ids {
                if self.get(id).is_some_and(|r| r.is_paired()) {
                    self.pair_of[id as usize] = ids.iter().copied().find(|&o| o != id);
                }
            }
        }
        self.pairs_dirty = false;
        self.generation += 1;
        self.stats.paired = paired_count;
        self.stats.unpaired = self.stats.total - paired_count;
    }

    /// Other records sharing this record's (canonical parent, stem), paired
    /// or not — the RAW a JPG *would* pair with.
    pub fn stem_siblings(&self, id: RecordId) -> Vec<RecordId> {
        let Some(r) = self.get(id) else { return Vec::new() };
        self.pair_index
            .get(&pair_key(&r.path))
            .map(|b| b.iter().copied().filter(|&o| o != id).collect())
            .unwrap_or_default()
    }

    /// The RAW/JPG partner of a paired record.
    pub fn find_pair(&self, id: RecordId) -> Option<RecordId> {
        if !self.pairs_dirty {
            return self.pair_of.get(id as usize).copied().flatten();
        }
        let r = self.get(id)?;
        if !r.is_paired() {
            return None;
        }
        self.pair_index.get(&pair_key(&r.path))?.iter().copied().find(|&o| o != id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::MTime;

    fn rec(path: &str) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        PhotoRecord::new(p, ft, 10, MTime::from_unix(1_700_000_000, 0), None)
    }

    #[test]
    fn pairing_is_automatic_and_needs_both_types() {
        let mut c = Collection::new();
        let raw = c.add(rec("/s/RAW/IMG_1.ARW"));
        let jpg = c.add(rec("/s/JPG/img_1.jpg"));
        let lone = c.add(rec("/s/IMG_2.jpg"));
        let other_dir = c.add(rec("/t/IMG_1.jpg"));
        c.build_pairs();
        assert_eq!(c.find_pair(raw), Some(jpg));
        assert_eq!(c.find_pair(jpg), Some(raw));
        assert_eq!(c.find_pair(lone), None);
        assert_eq!(c.find_pair(other_dir), None);
        assert_eq!(c.stats().paired, 2);
        assert_eq!(c.stats().unpaired, 2);
        // Removing one half unpairs the other once pairs are rebuilt.
        c.remove(raw);
        c.build_pairs();
        assert_eq!(c.find_pair(jpg), None);
        assert_eq!(c.stats().paired, 0);
    }

    #[test]
    fn update_path_keeps_pair_across_sort() {
        let mut c = Collection::new();
        let raw = c.add(rec("/s/IMG_1.ARW"));
        let jpg = c.add(rec("/s/IMG_1.JPG"));
        c.build_pairs();
        c.update_path(raw, "/s/RAW/IMG_1.ARW".into());
        c.update_path(jpg, "/s/JPG/IMG_1.JPG".into());
        c.build_pairs();
        assert_eq!(c.find_pair(raw), Some(jpg));
        assert_eq!(c.id_of(Path::new("/s/RAW/IMG_1.ARW")), Some(raw));
    }

    #[test]
    fn remove_updates_stats() {
        let mut c = Collection::new();
        let a = c.add(rec("/s/a.jpg"));
        c.add(rec("/s/b.arw"));
        c.get_mut(a).unwrap().is_pruned = true;
        c.recount_flags();
        assert_eq!(c.stats().pruned, 1);
        c.remove(a);
        assert_eq!(c.stats().total, 1);
        assert_eq!(c.stats().pruned, 0);
        assert_eq!(c.len(), 1);
    }
}
