//! Queries (plan §7.2): the text's vector against every shot's, the
//! closeness cut-off, accidental frames left out, best first.  Pure apart
//! from the text encoder, so it is unit-tested on synthetic vectors.
//!
//! **Closeness.**  Raw cosine scores aren't comparable between queries
//! (a vague word scores everything a little), so a hit must stand out
//! from the query's own spread: its score at least `closeness` robust
//! standard deviations above the median of all scores (median absolute
//! deviation scaled to a normal's).  Preferences ▸ Search ▸ *How close
//! matches must be* is a slider over [`CLOSENESS_MIN`]..=[`CLOSENESS_MAX`]
//! with a reset to [`CLOSENESS_DEFAULT`].  *Provisional* until phase 0's
//! labelled set fixes the range and the default.

use std::path::PathBuf;

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use super::index::Key;

/// The loosest setting: more results, more misses among them.
pub const CLOSENESS_MIN: f32 = 2.0;
/// The strictest: only what stands far out.
pub const CLOSENESS_MAX: f32 = 6.0;
/// Good matches, what works for most people (provisional).
pub const CLOSENESS_DEFAULT: f32 = 3.5;
/// Never more results than this, however loose the setting.
pub const MAX_HITS: usize = 2_000;

/// One result, best first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Stable for the session (and across sessions: it is the key's first
    /// 64 bits).
    pub id: u64,
    pub key: Key,
    /// Cosine similarity of the shot to the query.
    pub score: f32,
    pub day: NaiveDate,
    /// With sub-seconds where the camera recorded them; the file's
    /// modified time when it has no capture time.
    pub capture: NaiveDateTime,
    /// Frames of one shutter press share a value (found as the grid finds
    /// bursts); `None` for a frame on its own.
    pub burst: Option<u64>,
    /// The shot's files, absolute, as the library index lists them, so the
    /// UI builds records with no I/O.
    pub jpg: Option<PathBuf>,
    pub raw: Option<PathBuf>,
}

/// The prompt a plain query is wrapped in before encoding (the template
/// the study measured; phase 0 confirms it).
pub fn prompt(text: &str) -> String {
    format!("a photo of a {}.", text.trim())
}

/// Scores above the closeness cut-off, as (row, score), best first and at
/// most [`MAX_HITS`].  `scores` holds every row's cosine; rows in `skip`
/// (accidental frames) never count.  Pure.
pub fn cut(scores: &[f32], skip: impl Fn(usize) -> bool, closeness: f32) -> Vec<(usize, f32)> {
    if scores.is_empty() {
        return Vec::new();
    }
    let (median, mad) = median_and_mad(scores);
    // 1.4826 × MAD estimates a normal's standard deviation.
    let spread = (1.4826 * mad).max(1e-4);
    let threshold = median + closeness.clamp(CLOSENESS_MIN, CLOSENESS_MAX) * spread;
    let mut hits: Vec<(usize, f32)> = scores.iter().copied().enumerate().filter(|&(i, s)| s >= threshold && !skip(i)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.truncate(MAX_HITS);
    hits
}

// The absolute (probability) cut below is the alternative phase 0 weighs
// against `cut`; until the labels decide, only the evaluation calls it.
// Whichever loses is removed then, and these allows with it.

/// SigLIP2's trained pairing calibration: the model was trained with a
/// sigmoid loss, so `σ(SCALE · cosine + BIAS)` is its own estimate of the
/// chance that a text and an image belong together, comparable between
/// queries.  Read from `google/siglip2-base-patch16-256`'s weights
/// (`logit_scale` = 4.7265 → e^4.7265, `logit_bias` = −16.7718).
#[cfg_attr(not(test), allow(dead_code))]
pub const SIGMOID_SCALE: f32 = 112.901_18;
#[cfg_attr(not(test), allow(dead_code))]
pub const SIGMOID_BIAS: f32 = -16.771_803;

/// The model's own probability that a shot matches the query.
#[cfg_attr(not(test), allow(dead_code))]
pub fn match_probability(cosine: f32) -> f32 {
    1.0 / (1.0 + (-(SIGMOID_SCALE * cosine + SIGMOID_BIAS)).exp())
}

/// The cosine at which [`match_probability`] reaches `p`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn cosine_for_probability(p: f32) -> f32 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    ((p / (1.0 - p)).ln() - SIGMOID_BIAS) / SIGMOID_SCALE
}

/// Scores whose match probability is at least `min_probability`, as
/// (row, score), best first and at most [`MAX_HITS`]; rows in `skip`
/// never count.  The absolute alternative to [`cut`]: it also answers
/// queries most of the library matches ("fursuit" here), where nothing
/// stands out from the spread.  Pure.
#[cfg_attr(not(test), allow(dead_code))]
pub fn cut_by_probability(scores: &[f32], skip: impl Fn(usize) -> bool, min_probability: f32) -> Vec<(usize, f32)> {
    let threshold = cosine_for_probability(min_probability);
    let mut hits: Vec<(usize, f32)> = scores.iter().copied().enumerate().filter(|&(i, s)| s >= threshold && !skip(i)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.truncate(MAX_HITS);
    hits
}

/// The median and the median absolute deviation of `v`.
pub fn median_and_mad(v: &[f32]) -> (f32, f32) {
    let mut s: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return (0.0, 0.0);
    }
    s.sort_by(f32::total_cmp);
    let med = s[s.len() / 2];
    let mut d: Vec<f32> = s.iter().map(|x| (x - med).abs()).collect();
    d.sort_by(f32::total_cmp);
    (med, d[d.len() / 2])
}

/// Cosine of a unit query against f16 rows: `scores[i]` for row `i`.
pub fn score_rows(query: &[f32], rows: &[u16], dim: usize) -> Vec<f32> {
    if dim == 0 {
        return Vec::new();
    }
    rows.chunks_exact(dim).map(|row| row.iter().zip(query).map(|(&h, &q)| f16_to_f32(h) * q).sum()).collect()
}

/// IEEE half → single.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign << 31,
        0 => {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((f & 0x3ff) << 13)
        }
        31 => (sign << 31) | 0x7f80_0000 | (frac << 13),
        _ => (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

/// Single → IEEE half, round to nearest even.
pub fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if frac != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (frac | 0x80_0000) >> (1 - e);
        let rounded = (m + 0x1000 + ((m >> 13) & 1)) >> 13;
        return sign | rounded as u16;
    }
    let m = ((e as u32) << 23 | frac) + 0x1000 + ((frac >> 13) & 1);
    sign | (m >> 13) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip() {
        for v in [0.0f32, 1.0, -1.0, 0.5, 0.333_25, 65504.0, 1e-5, -3.14, 0.0123, 1.0 / 3.0] {
            let h = f32_to_f16(v);
            let back = f16_to_f32(h);
            let tol = v.abs().max(6.1e-5) / 1024.0;
            assert!((back - v).abs() <= tol, "{v} → {h:#06x} → {back}");
        }
        assert_eq!(f16_to_f32(f32_to_f16(1e-9)), 0.0);
        assert!(f16_to_f32(f32_to_f16(1e9)).is_infinite());
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
    }

    #[test]
    fn the_cut_keeps_what_stands_out_and_skips_accidents() {
        // 1 000 background scores around 0.1, three real hits, one
        // accidental frame scoring high.
        let mut scores: Vec<f32> = (0..1000).map(|i| 0.1 + ((i * 37 % 101) as f32 - 50.0) * 0.0004).collect();
        scores[10] = 0.4;
        scores[20] = 0.35;
        scores[30] = 0.3;
        scores[40] = 0.5; // the accident
        let hits = cut(&scores, |i| i == 40, CLOSENESS_DEFAULT);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [10, 20, 30]);
        assert!(hits[0].1 > hits[1].1 && hits[1].1 > hits[2].1, "best first");
        // Looser finds more, stricter fewer, and the range is clamped.
        assert!(cut(&scores, |_| false, CLOSENESS_MIN).len() >= 4);
        assert!(cut(&scores, |_| false, 100.0).len() <= cut(&scores, |_| false, CLOSENESS_MAX).len());
        assert!(cut(&[], |_| false, CLOSENESS_DEFAULT).is_empty());
        // A flat field: nothing stands out.
        assert!(cut(&vec![0.2; 50], |_| false, CLOSENESS_DEFAULT).is_empty());
    }

    #[test]
    fn the_sigmoid_calibration_is_absolute() {
        // The numbers measured on the sample: "fursuit" hits around 0.18,
        // a weak "camera" hit around 0.10, background around 0.03.
        assert!(match_probability(0.184) > 0.95);
        assert!((0.1..0.4).contains(&match_probability(0.137)));
        assert!(match_probability(0.10) < 0.02);
        assert!(match_probability(0.03) < 1e-4);
        for p in [0.05f32, 0.3, 0.5, 0.9] {
            let c = cosine_for_probability(p);
            assert!((match_probability(c) - p).abs() < 1e-4, "{p}");
        }
        let scores = [0.03, 0.20, 0.15, 0.12, 0.19];
        let hits = cut_by_probability(&scores, |i| i == 4, 0.5);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 2], "best first, the accident skipped");
        assert!(cut_by_probability(&scores, |_| false, 0.999).is_empty());
    }

    #[test]
    fn scoring_uses_f16_rows() {
        let dim = 4;
        let rows: Vec<u16> = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.6, 0.8, 0.0, 0.0]].iter().flatten().map(|&v| f32_to_f16(v)).collect();
        let s = score_rows(&[1.0, 0.0, 0.0, 0.0], &rows, dim);
        assert_eq!(s.len(), 3);
        assert!((s[0] - 1.0).abs() < 1e-3 && s[1].abs() < 1e-3 && (s[2] - 0.6).abs() < 1e-3);
        assert_eq!(prompt("  red car "), "a photo of a red car.");
    }
}
