//! Queries (plan §7.2): the text's vector against every shot's, the
//! closeness cut-off, accidental frames left out, best first.  Pure apart
//! from the text encoder, so it is unit-tested on synthetic vectors.
//!
//! **Closeness.**  A hit needs the model's own match probability
//! ([`match_probability`]: SigLIP2's sigmoid over its trained scale and
//! bias, comparable between queries) of at least the closeness setting.
//! Preferences ▸ Search ▸ *How close matches must be* is a slider over
//! [`CLOSENESS_MIN`]..=[`CLOSENESS_MAX`] (higher is stricter) with a reset
//! to [`CLOSENESS_DEFAULT`].  A cut relative to the query's own spread
//! was tried first and fails for things most of the library shows
//! ("fursuit" here: nothing stands out); it stays in the evaluation
//! harness for comparison.  *Provisional* until phase 0's labelled set
//! fine-tunes the default.

use std::path::PathBuf;

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use super::index::Key;

/// The loosest setting, a match probability of 0.03 %: cosine ≥ 0.077,
/// where the sample's dark and empty frames start to come in.
pub const CLOSENESS_MIN: f32 = 0.0003;
/// The strictest, 30 %: cosine ≥ 0.141, only the plainest matches
/// (a fireworks fountain, a fursuit, a snow mountain).
pub const CLOSENESS_MAX: f32 = 0.3;
/// Good matches, what works for most people: a match probability of
/// 0.3 % (cosine ≥ 0.098), provisional.  Measured on the 1 779-preview
/// sample: the background sits below 1e-4 and the weak, dark frames near
/// 1e-3, so both stay out; the plain hits sit between 3e-3 and 0.98
/// ("fursuit" top 0.98 and median 0.70, so a thing most of the library
/// shows comes back whole; "camera" top 0.21; "car" and "food" tops 0.013
/// and 0.004, their one clear hit each), so they stay in.  At 0.01 "food"
/// and "cake" would have nothing; at 0.001 the dark frames join "camera".
pub const CLOSENESS_DEFAULT: f32 = 0.003;
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

/// The relative cut the app no longer uses, kept for the evaluation
/// harness: scores at least `z` robust standard deviations (1.4826 × the
/// median absolute deviation) above the median, as (row, score), best
/// first and at most [`MAX_HITS`]; rows in `skip` never count.  Pure.
#[cfg(test)]
pub fn cut(scores: &[f32], skip: impl Fn(usize) -> bool, z: f32) -> Vec<(usize, f32)> {
    if scores.is_empty() {
        return Vec::new();
    }
    let (median, mad) = median_and_mad(scores);
    let spread = (1.4826 * mad).max(1e-4);
    let threshold = median + z * spread;
    let mut hits: Vec<(usize, f32)> = scores.iter().copied().enumerate().filter(|&(i, s)| s >= threshold && !skip(i)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.truncate(MAX_HITS);
    hits
}

/// SigLIP2's trained pairing calibration: the model was trained with a
/// sigmoid loss, so `σ(SCALE · cosine + BIAS)` is its own estimate of the
/// chance that a text and an image belong together, comparable between
/// queries.  Read from `google/siglip2-base-patch16-256`'s weights
/// (`logit_scale` = 4.7265 → e^4.7265, `logit_bias` = −16.7718).
pub const SIGMOID_SCALE: f32 = 112.901_18;
pub const SIGMOID_BIAS: f32 = -16.771_803;

/// The model's own probability that a shot matches the query.
pub fn match_probability(cosine: f32) -> f32 {
    1.0 / (1.0 + (-(SIGMOID_SCALE * cosine + SIGMOID_BIAS)).exp())
}

/// The cosine at which [`match_probability`] reaches `p`.
pub fn cosine_for_probability(p: f32) -> f32 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    ((p / (1.0 - p)).ln() - SIGMOID_BIAS) / SIGMOID_SCALE
}

/// Scores whose match probability is at least `closeness` (clamped to the
/// slider's range), as (row, score), best first and at most [`MAX_HITS`];
/// rows in `skip` (accidental frames) never count.  Being absolute, it
/// also answers queries most of the library matches ("fursuit" here),
/// where nothing stands out from the spread.  Pure.
pub fn cut_by_probability(scores: &[f32], skip: impl Fn(usize) -> bool, closeness: f32) -> Vec<(usize, f32)> {
    let threshold = cosine_for_probability(closeness.clamp(CLOSENESS_MIN, CLOSENESS_MAX));
    let mut hits: Vec<(usize, f32)> = scores.iter().copied().enumerate().filter(|&(i, s)| s >= threshold && !skip(i)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.truncate(MAX_HITS);
    hits
}

/// The median and the median absolute deviation of `v` (the harness's
/// score spread).
#[cfg(test)]
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
        let hits = cut(&scores, |i| i == 40, 3.5);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [10, 20, 30]);
        assert!(hits[0].1 > hits[1].1 && hits[1].1 > hits[2].1, "best first");
        // Looser finds more, stricter fewer.
        assert!(cut(&scores, |_| false, 2.0).len() >= 4);
        assert!(cut(&scores, |_| false, 100.0).len() <= cut(&scores, |_| false, 6.0).len());
        assert!(cut(&[], |_| false, 3.5).is_empty());
        // A flat field: nothing stands out — which is why the app uses the
        // probability rule instead.
        assert!(cut(&vec![0.2; 50], |_| false, 3.5).is_empty());
        assert_eq!(cut_by_probability(&vec![0.2; 50], |_| false, CLOSENESS_DEFAULT).len(), 50);
    }

    #[test]
    fn the_closeness_range_is_a_probability_floor_with_higher_stricter() {
        assert!(CLOSENESS_MIN < CLOSENESS_DEFAULT && CLOSENESS_DEFAULT < CLOSENESS_MAX);
        assert!(CLOSENESS_MIN > 0.0 && CLOSENESS_MAX < 1.0);
        // The sample's landmarks: background out at every setting, a plain
        // fursuit in at every setting, the weak camera hit in by default,
        // the dark frames out by default.
        let (background, dark, camera, fursuit) = (0.03f32, 0.089, 0.137, 0.184);
        for c in [CLOSENESS_MIN, CLOSENESS_DEFAULT, CLOSENESS_MAX] {
            let hits = cut_by_probability(&[background, dark, camera, fursuit], |_| false, c);
            assert!(hits.iter().all(|h| h.0 != 0), "{c}: background");
            assert!(hits.iter().any(|h| h.0 == 3), "{c}: fursuit");
        }
        let default = cut_by_probability(&[background, dark, camera, fursuit], |_| false, CLOSENESS_DEFAULT);
        assert_eq!(default.iter().map(|h| h.0).collect::<Vec<_>>(), [3, 2]);
        assert!(cut_by_probability(&[camera], |_| false, CLOSENESS_MAX).is_empty(), "stricter drops the weak hit");
        assert_eq!(cut_by_probability(&[dark], |_| false, CLOSENESS_MIN).len(), 1, "looser admits the dark frame");
        // Out-of-range settings are clamped, not taken literally.
        assert_eq!(cut_by_probability(&[fursuit], |_| false, 5.0).len(), cut_by_probability(&[fursuit], |_| false, CLOSENESS_MAX).len());
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
        let hits = cut_by_probability(&scores, |i| i == 4, 0.3);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 2], "best first, the accident skipped");
        assert!(cut_by_probability(&[0.10, 0.03], |_| false, CLOSENESS_MAX).is_empty());
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
