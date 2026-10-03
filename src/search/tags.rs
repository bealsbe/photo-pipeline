//! Hidden tags (plan §3.2, decision 8): which vocabulary words each
//! indexed shot carries, derived from its stored vector against the
//! vocabulary's text vectors.  Helpers only: they power completions, the
//! *Things* list and the ranking boost (and later the fursuit-head step);
//! they are never shown on a photo.
//!
//! **The rule.**  A shot carries a word when the model's own match
//! probability for the word's prompt ([`super::query::match_probability`]
//! over the cosine) reaches the word's threshold ([`DEFAULT_THRESHOLD`]
//! unless the vocabulary gives one), and, for a word that must beat
//! another ([`super::vocab::Word::must_beat`]: the animals and "fursuit",
//! snow and "infrared"), when it does.  Each word is otherwise judged on
//! its own, so a camera in a fursuit photo counts as a camera; the study's
//! softmax share over the whole vocabulary let the fursuit label swallow
//! the rest.
//!
//! **Cost.**  Pure arithmetic on vectors already in memory, on the
//! engine's thread, never the UI's: rows are tagged as they are added
//! (a batch of 32 is a fraction of a millisecond) and the whole index
//! once on loading (about 0.4 s for 56 000 shots and the vocabulary).  The
//! vocabulary's text vectors are encoded once by the text tower and kept
//! beside the index (`vocab-<model>.bin`, invalidated by the vocabulary
//! and the model), so later starts need no helper for them.
//!
//! Tags are kept in memory only and follow the index's generation
//! ([`Tags::sync`]): new rows are tagged, a cleared index starts over.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::index::Vectors;
use super::query::{cosine_for_probability, prompt};
use super::vocab::{self, VOCABULARY};
use crate::ml::models::ModelInfo;
use crate::model::binfile::{Reader, Writer, fnv1a};
use crate::ops::fsutil;

/// The match probability a photo needs to carry a word, unless the word
/// says otherwise: 1 % (cosine ≥ 0.108), above the search default of
/// 0.3 %, so a tag is a plain match.  Set from the user's own labels on the
/// phase 0 sample (1 779 previews, 28 words, `eval::tag_thresholds`): at
/// 1 % the tags are right 87.6 % of the time and find 30.5 % of the
/// labelled photos; at 2 % (the earlier choice, from the agent's own
/// judgement) 88.5 % and 21.5 %, so 1 % finds two fifths more for a point
/// of precision; at 0.5 % precision falls to 76 %.  Tags are helpers (the
/// ranking boost, completions, Things), so precision comes first.
pub const DEFAULT_THRESHOLD: f32 = 0.01;

const VOCAB_MAGIC: &[u8; 8] = b"PPSRCHT\0";
const VOCAB_VERSION: u32 = 1;

/// The vocabulary's text vectors, in [`VOCABULARY`] order, unit length.
#[derive(Debug, Clone, PartialEq)]
pub struct VocabVectors {
    pub dim: usize,
    /// `VOCABULARY.len() × dim`, row-major.
    pub data: Vec<f32>,
}

impl VocabVectors {
    /// Every word's prompt, in order: what the text tower encodes.
    pub fn prompts() -> Vec<String> {
        VOCABULARY.iter().map(|w| w.prompt()).collect()
    }

    /// A digest of the prompts and the template: the cache file is only
    /// read back for the same vocabulary.
    pub fn digest() -> u64 {
        let mut s = String::new();
        for p in Self::prompts() {
            s.push_str(&p);
            s.push('\n');
        }
        s.push_str(&prompt("x"));
        fnv1a(s.as_bytes())
    }

    /// From the text tower's answers (one per prompt, in order).
    pub fn from_rows(rows: &[Vec<f32>]) -> Option<VocabVectors> {
        let dim = rows.first()?.len();
        if rows.len() != VOCABULARY.len() || dim == 0 || rows.iter().any(|r| r.len() != dim) {
            return None;
        }
        let mut data = Vec::with_capacity(rows.len() * dim);
        for r in rows {
            let n = r.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            data.extend(r.iter().map(|x| x / n));
        }
        Some(VocabVectors { dim, data })
    }

    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    fn file(dir: &Path, model: &ModelInfo) -> PathBuf {
        dir.join(format!("vocab-{}.bin", model.id.dir_name()))
    }

    /// The cached vectors, if the file is this vocabulary's and model's.
    pub fn load(dir: &Path, model: &ModelInfo) -> Option<VocabVectors> {
        let data = fs::read(Self::file(dir, model)).ok()?;
        let mut r = Reader::open(&data)?;
        if r.take(8)? != VOCAB_MAGIC || r.u32()? != VOCAB_VERSION {
            return None;
        }
        if r.bytes()? != model.weights_sha256().as_bytes() || r.u64()? != Self::digest() {
            return None;
        }
        let dim = r.u32()? as usize;
        let n = r.count(dim.checked_mul(4)?)?;
        if n != VOCABULARY.len() || dim == 0 {
            return None;
        }
        let mut out = Vec::with_capacity(n * dim);
        for _ in 0..n * dim {
            out.push(f32::from_bits(r.u32()?));
        }
        r.done().then_some(VocabVectors { dim, data: out })
    }

    /// Write the cache (whole, atomically).
    pub fn save(&self, dir: &Path, model: &ModelInfo) -> io::Result<()> {
        let mut w = Writer(Vec::with_capacity(64 + self.data.len() * 4));
        w.0.extend_from_slice(VOCAB_MAGIC);
        w.u32(VOCAB_VERSION);
        w.bytes(model.weights_sha256().as_bytes());
        w.u64(Self::digest());
        w.u32(self.dim as u32);
        w.u32(VOCABULARY.len() as u32);
        for &x in &self.data {
            w.u32(x.to_bits());
        }
        fs::create_dir_all(dir)?;
        fsutil::atomic_write(&Self::file(dir, model), &w.seal(), true)
    }
}

/// The tags of every row of an index, by row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    /// The index generation these rows belong to.
    pub generation: u64,
    /// Row `i`'s words are `words[offsets[i]..offsets[i + 1]]`.
    offsets: Vec<u32>,
    words: Vec<u16>,
}

impl Tags {
    /// Rows tagged so far.
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Row `i`'s words, as vocabulary indices in order.
    pub fn of(&self, i: usize) -> &[u16] {
        match (self.offsets.get(i), self.offsets.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.words[a as usize..b as usize],
            _ => &[],
        }
    }

    /// Row `i` carries vocabulary word `w`.
    pub fn has(&self, i: usize, w: usize) -> bool {
        self.of(i).binary_search(&(w as u16)).is_ok()
    }

    /// Bring the tags up to the index: new rows are tagged; a generation
    /// that lost rows (a clear, a rebuild) starts over.  Returns whether
    /// anything changed.
    pub fn sync(&mut self, vectors: &Vectors, generation: u64, vocab: &VocabVectors) -> bool {
        if self.generation == generation && self.len() == vectors.len() {
            return false;
        }
        if self.len() > vectors.len() || vocab.dim != vectors.dim {
            self.offsets.clear();
            self.words.clear();
        }
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        let from = self.len();
        self.generation = generation;
        if vocab.dim != vectors.dim {
            return from != vectors.len();
        }
        let cosines: Vec<f32> = VOCABULARY.iter().map(|w| cosine_for_probability(w.threshold.unwrap_or(DEFAULT_THRESHOLD))).collect();
        let targets: Vec<Option<usize>> = VOCABULARY.iter().map(|w| w.must_beat_index()).collect();
        let mut row = vec![0f32; vectors.dim];
        let mut dots = vec![0f32; VOCABULARY.len()];
        for i in from..vectors.len() {
            for (x, &h) in row.iter_mut().zip(&vectors.data[i * vectors.dim..(i + 1) * vectors.dim]) {
                *x = super::query::f16_to_f32(h);
            }
            for (j, d) in dots.iter_mut().enumerate() {
                *d = vocab.row(j).iter().zip(&row).map(|(a, b)| a * b).sum();
            }
            for (j, &c) in cosines.iter().enumerate() {
                if dots[j] >= c && targets[j].is_none_or(|t| dots[j] > dots[t]) {
                    self.words.push(j as u16);
                }
            }
            self.offsets.push(self.words.len() as u32);
        }
        from != vectors.len()
    }

    /// Photos carrying each vocabulary word, counting rows `keep` admits
    /// (the accidental and the unmapped left out).
    pub fn counts(&self, keep: impl Fn(usize) -> bool) -> Vec<usize> {
        let mut out = vec![0usize; VOCABULARY.len()];
        for i in 0..self.len() {
            if keep(i) {
                for &w in self.of(i) {
                    out[w as usize] += 1;
                }
            }
        }
        out
    }

    /// The *Things* list: objects and scenes with photos, most first (by
    /// name for equal counts).  Subject kinds never appear.
    pub fn things(&self, keep: impl Fn(usize) -> bool) -> Vec<super::Thing> {
        let counts = self.counts(keep);
        let mut out: Vec<super::Thing> = vocab::things().filter(|(i, _)| counts[*i] > 0).map(|(i, w)| super::Thing { word: w.plural.to_string(), photos: counts[i], kind: w.kind }).collect();
        out.sort_by(|a, b| b.photos.cmp(&a.photos).then_with(|| a.word.cmp(&b.word)));
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::ml::models::{self, ModelId};
    use crate::search::index::{Key, Record};
    use crate::search::query::{f32_to_f16, match_probability};
    use crate::search::vocab::Kind;
    use crate::model::record::ShotMeta;

    /// A vocabulary whose word `j` points along axis `j` (the dimension
    /// is the vocabulary's size).
    pub fn axis_vocab() -> VocabVectors {
        let n = VOCABULARY.len();
        let mut data = vec![0f32; n * n];
        for j in 0..n {
            data[j * n + j] = 1.0;
        }
        VocabVectors { dim: n, data }
    }

    /// Vectors whose row `i` has the given cosine with each word in
    /// `tags[i]` (0.5 is a plain match, far above any threshold) and
    /// nothing else.
    pub fn vectors_with<R: AsRef<[(usize, f32)]>>(tags: &[R]) -> Vectors {
        let n = VOCABULARY.len();
        let mut v = Vectors { dim: n, ..Vectors::default() };
        for (i, words) in tags.iter().enumerate() {
            let words = words.as_ref();
            let mut row = vec![0f32; n];
            for &(w, c) in words {
                row[w] = c;
            }
            // Fill the rest so the row is a unit vector without touching
            // any word's axis past 0.06 (probability ~1e-5).
            let spread = (1.0 - row.iter().map(|x| x * x).sum::<f32>()).max(0.0).sqrt();
            let free: Vec<usize> = (0..n).filter(|j| !words.iter().any(|(w, _)| w == j)).collect();
            let each = spread / (free.len() as f32).sqrt();
            for j in free {
                row[j] = each;
            }
            v.by_key.insert(Key([i as u8 + 1; 16]), i);
            v.records.push(Record { key: Key([i as u8 + 1; 16]), capture: None, meta: ShotMeta::default(), accidental: false });
            v.data.extend(row.iter().map(|&x| f32_to_f16(x)));
        }
        v
    }

    #[test]
    fn rows_are_tagged_by_the_probability_rule_and_follow_the_generation() {
        let camera = vocab::find("camera").unwrap();
        let fireworks = vocab::find("fireworks").unwrap();
        let fursuit = vocab::find("fursuit").unwrap();
        let dog = vocab::find("dog").unwrap();
        let plush = vocab::find("stuffed animal").unwrap();
        let snow = vocab::find("snow").unwrap();
        let sunset = vocab::find("sunset").unwrap();
        let infrared = vocab::find("infrared").unwrap();
        let vocab = axis_vocab();
        let t = cosine_for_probability(DEFAULT_THRESHOLD);
        assert!((match_probability(t) - DEFAULT_THRESHOLD).abs() < 1e-4);
        let rows: Vec<Vec<(usize, f32)>> = vec![
            vec![(camera, 0.5)],
            vec![(fireworks, 0.5), (fursuit, 0.5)],
            vec![],
            vec![(dog, 0.5)],
            // The contrast: a canine fursuit isn't a dog, a fursuit isn't a
            // plush toy, but a dog beside a weaker fursuit is a dog.
            vec![(dog, 0.3), (fursuit, 0.5)],
            vec![(plush, 0.3), (fursuit, 0.5)],
            vec![(dog, 0.5), (fursuit, 0.3)],
            // Just over the threshold is in, just under is out.
            vec![(camera, t + 0.003)],
            vec![(camera, t - 0.003)],
            // The same rule for infrared: white foliage isn't snow and a
            // strange sky isn't a sunset where the frame is infrared, but
            // real snow beside a faint infrared look is snow.
            vec![(snow, 0.3), (infrared, 0.5)],
            vec![(sunset, 0.3), (infrared, 0.5)],
            vec![(snow, 0.5), (infrared, 0.3)],
        ];
        let vecs = vectors_with(&rows);
        let mut tags = Tags::default();
        assert!(tags.sync(&vecs, 1, &vocab));
        assert_eq!(tags.len(), 12);
        assert!(!tags.has(9, snow) && tags.has(9, infrared), "{:?}", tags.of(9));
        assert!(!tags.has(10, sunset) && tags.has(10, infrared), "{:?}", tags.of(10));
        assert!(tags.has(11, snow) && tags.has(11, infrared), "{:?}", tags.of(11));
        assert_eq!(tags.of(0), [camera as u16]);
        assert_eq!(tags.of(1).len(), 2);
        assert!(tags.has(1, fireworks) && tags.has(1, fursuit) && !tags.has(1, camera));
        assert!(tags.of(2).is_empty());
        assert!(tags.has(3, dog));
        assert!(!tags.has(4, dog) && tags.has(4, fursuit), "{:?}", tags.of(4));
        assert!(!tags.has(5, plush) && tags.has(5, fursuit), "{:?}", tags.of(5));
        assert!(tags.has(6, dog) && tags.has(6, fursuit), "{:?}", tags.of(6));
        assert_eq!(tags.of(7), [camera as u16]);
        assert!(tags.of(8).is_empty());
        assert!(!tags.sync(&vecs, 1, &vocab), "nothing new");
        // The generation moved on with more rows: only the new ones are
        // tagged, the old ones kept.
        let mut more_rows = rows.clone();
        more_rows.push(vec![(camera, 0.5), (dog, 0.5)]);
        let more = vectors_with(&more_rows);
        assert!(tags.sync(&more, 2, &vocab));
        assert_eq!(tags.len(), 13);
        assert!(tags.has(12, camera) && tags.has(12, dog));
        assert!(tags.has(4, fursuit), "old rows are kept as they were");
        // Fewer rows (a clear): everything starts over.
        let fewer = vectors_with(&[vec![(fireworks, 0.5)]]);
        assert!(tags.sync(&fewer, 3, &vocab));
        assert_eq!(tags.len(), 1);
        assert!(tags.has(0, fireworks));
        let counts = tags.counts(|_| true);
        assert_eq!(counts[fireworks], 1);
        assert_eq!(counts.iter().sum::<usize>(), 1);
    }

    /// Kinds never mix: the *Things* list holds objects and scenes only,
    /// whatever the photos carry, most photos first.
    #[test]
    fn things_hold_objects_and_scenes_only_most_first() {
        let camera = vocab::find("camera").unwrap();
        let fireworks = vocab::find("fireworks").unwrap();
        let fursuit = vocab::find("fursuit").unwrap();
        let dog = vocab::find("dog").unwrap();
        let person = vocab::find("person").unwrap();
        let vocab = axis_vocab();
        let p = 0.5f32;
        let mut vecs = vectors_with(&[vec![(camera, p), (fursuit, p)], vec![(fireworks, p), (person, p), (dog, p)], vec![(fireworks, p), (dog, p)], vec![(camera, p)], vec![(camera, p), (fursuit, p)], vec![(dog, p)]]);
        vecs.records[4].accidental = true;
        let mut tags = Tags::default();
        tags.sync(&vecs, 1, &vocab);
        let things = tags.things(|i| !vecs.records[i].accidental);
        let names: Vec<(&str, usize)> = things.iter().map(|t| (t.word.as_str(), t.photos)).collect();
        assert_eq!(names, [("cameras", 2), ("fireworks", 2)], "the accidental camera isn't counted");
        for t in &things {
            assert!(!t.kind.is_subject(), "{t:?}");
            assert!(matches!(t.kind, Kind::Object | Kind::Scene));
        }
        assert!(tags.counts(|_| true)[dog] == 3 && tags.counts(|_| true)[fursuit] == 2, "subject tags exist, just not in Things");
        // The completions see the same list.
        let c = crate::search::complete_things(&things, "fireworks ca");
        assert_eq!(c.iter().map(|t| t.word.as_str()).collect::<Vec<_>>(), ["cameras"]);
        assert!(crate::search::complete_things(&things, "dog").is_empty());
        assert!(crate::search::complete_things(&things, "").is_empty());
        assert_eq!(crate::search::complete_things(&things, "F").len(), 1);
    }

    #[test]
    fn the_vocabulary_vectors_cache_round_trips_and_is_tied_to_the_vocabulary() {
        let d = crate::search::index::tests::scratch("vocab");
        let model = models::info(ModelId::Siglip2);
        assert!(VocabVectors::load(&d, model).is_none());
        let rows: Vec<Vec<f32>> = (0..VOCABULARY.len()).map(|j| (0..8).map(|k| ((j * 7 + k) % 11) as f32 - 5.0).collect()).collect();
        let v = VocabVectors::from_rows(&rows).unwrap();
        assert_eq!(v.dim, 8);
        assert!((v.row(3).iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5, "unit length");
        v.save(&d, model).unwrap();
        assert_eq!(VocabVectors::load(&d, model), Some(v.clone()));
        // Another model's weights, or a damaged file, aren't read back.
        let other = models::ModelInfo { preprocess_version: model.preprocess_version + 1, ..*model };
        assert_eq!(VocabVectors::load(&d, &other), Some(v.clone()), "preprocessing doesn't touch the text tower");
        let file = d.join(format!("vocab-{}.bin", model.id.dir_name()));
        let mut data = fs::read(&file).unwrap();
        data[40] ^= 0xff;
        fs::write(&file, &data).unwrap();
        assert!(VocabVectors::load(&d, model).is_none());
        assert!(VocabVectors::from_rows(&rows[..3]).is_none(), "every word needs a vector");
        assert_ne!(VocabVectors::digest(), 0);
        let _ = fs::remove_dir_all(&d);
    }
}
