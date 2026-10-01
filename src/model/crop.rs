//! Crop rectangle model.
//!
//! A crop is stored in normalised coordinates (0–1) relative to the
//! orientation-corrected image, which is what makes one crop valid for both
//! halves of a RAW+JPG pair even though their pixel dimensions differ.

use serde_json::{Map, Value};

use crate::util::{fmt_g, round6};

/// Smallest crop allowed, as a fraction of the image edge.
pub const MIN_EDGE: f64 = 0.02;

/// A named width:height constraint.  `w`/`h` are `None` for freeform.
#[derive(Debug, Clone, PartialEq)]
pub struct AspectRatio {
    pub label: String,
    pub w: Option<f64>,
    pub h: Option<f64>,
}

impl AspectRatio {
    pub fn named(label: &str) -> Self {
        Self { label: label.into(), w: None, h: None }
    }

    pub fn fixed(label: &str, w: f64, h: f64) -> Self {
        Self { label: label.into(), w: Some(w), h: Some(h) }
    }

    pub fn is_free(&self) -> bool {
        self.w.is_none() || self.h.is_none()
    }

    /// width / height, or `None` when freeform.
    pub fn value(&self) -> Option<f64> {
        Some(self.w? / self.h?)
    }

    /// Swap orientation — 3:2 landscape becomes 2:3 portrait.
    pub fn flipped(&self) -> Self {
        match (self.w, self.h) {
            (Some(w), Some(h)) => Self {
                label: format!("{}:{}", fmt_g(h), fmt_g(w)),
                w: Some(h),
                h: Some(w),
            },
            _ => self.clone(),
        }
    }

    pub fn custom(w: f64, h: f64) -> Self {
        Self { label: format!("{}:{}", fmt_g(w), fmt_g(h)), w: Some(w), h: Some(h) }
    }
}

pub const FREEFORM: &str = "Freeform";
pub const ORIGINAL: &str = "Original";

/// Ordered as they appear in the crop bar.
pub fn presets() -> Vec<AspectRatio> {
    vec![
        AspectRatio::named(FREEFORM),
        AspectRatio::named(ORIGINAL),
        AspectRatio::fixed("1:1", 1.0, 1.0),
        AspectRatio::fixed("3:2", 3.0, 2.0),
        AspectRatio::fixed("4:3", 4.0, 3.0),
        AspectRatio::fixed("5:4", 5.0, 4.0),
        AspectRatio::fixed("7:5", 7.0, 5.0),
        AspectRatio::fixed("16:9", 16.0, 9.0),
        AspectRatio::fixed("16:10", 16.0, 10.0),
        AspectRatio::fixed("65:24", 65.0, 24.0), // XPan / panoramic
    ]
}

/// Resolve a stored label back to a ratio: presets, their flipped variants,
/// or any `w:h` custom string round-tripped through the sidecar.
pub fn ratio_from_label(label: &str) -> Option<AspectRatio> {
    if label.is_empty() {
        return None;
    }
    let all = presets();
    if let Some(p) = all.iter().find(|p| p.label == label) {
        return Some(p.clone());
    }
    for p in &all {
        if !p.is_free() && p.flipped().label == label {
            return Some(p.flipped());
        }
    }
    let (w, h) = label.split_once(':')?;
    let (w, h): (f64, f64) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0.0 && h > 0.0).then(|| AspectRatio::custom(w, h))
}

/// Normalised crop rect: fractions in [0, 1], left < right, top < bottom.
#[derive(Debug, Clone, PartialEq)]
pub struct Crop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// Ratio in force when the crop was made.
    pub aspect_label: Option<String>,
}

impl Crop {
    /// Clamps, orders and enforces the minimum edge.
    pub fn new(left: f64, top: f64, right: f64, bottom: f64, aspect: Option<String>) -> Self {
        let c = |v: f64| v.clamp(0.0, 1.0);
        let (mut l, mut t, mut r, mut b) = (c(left), c(top), c(right), c(bottom));
        if r < l {
            std::mem::swap(&mut l, &mut r);
        }
        if b < t {
            std::mem::swap(&mut t, &mut b);
        }
        if r - l < MIN_EDGE {
            r = r.max(l + MIN_EDGE);
        }
        if b - t < MIN_EDGE {
            b = b.max(t + MIN_EDGE);
        }
        Self { left: l, top: t, right: r, bottom: b, aspect_label: aspect.filter(|s| !s.is_empty()) }
    }

    pub fn full(aspect: Option<String>) -> Self {
        Crop::new(0.0, 0.0, 1.0, 1.0, aspect)
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }

    /// True when the crop covers the whole frame.
    pub fn is_full(&self) -> bool {
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-4;
        close(self.left, 0.0) && close(self.top, 0.0) && close(self.right, 1.0) && close(self.bottom, 1.0)
    }

    /// Integer pixel box (left, top, right, bottom), at least 1×1 and inside
    /// the image.  Origin and size are rounded independently so an exact
    /// ratio survives.
    pub fn to_pixels(&self, img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
        let (iw, ih) = (img_w as i64, img_h as i64);
        let rnd = |v: f64| py_round(v);
        let l = rnd(self.left * iw as f64).min(iw - 1).max(0);
        let t = rnd(self.top * ih as f64).min(ih - 1).max(0);
        let w = rnd(self.width() * iw as f64).max(1);
        let h = rnd(self.height() * ih as f64).max(1);
        let r = (l + w).min(iw);
        let b = (t + h).min(ih);
        let l = (r - w).max(0);
        let t = (b - h).max(0);
        (l as u32, t as u32, r as u32, b as u32)
    }

    pub fn pixel_size(&self, img_w: u32, img_h: u32) -> (u32, u32) {
        let (l, t, r, b) = self.to_pixels(img_w, img_h);
        (r - l, b - t)
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("left".into(), round6(self.left).into());
        m.insert("top".into(), round6(self.top).into());
        m.insert("right".into(), round6(self.right).into());
        m.insert("bottom".into(), round6(self.bottom).into());
        if let Some(a) = &self.aspect_label {
            m.insert("aspect".into(), a.clone().into());
        }
        Value::Object(m)
    }

    /// Parse a sidecar entry; `None` if malformed.
    pub fn from_json(v: &Value) -> Option<Crop> {
        let o = v.as_object()?;
        let num = |k: &str| -> Option<f64> {
            match o.get(k)? {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse().ok(),
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                _ => None,
            }
        };
        let aspect = match o.get("aspect") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        Some(Crop::new(num("left")?, num("top")?, num("right")?, num("bottom")?, aspect))
    }
}

/// Python 3's `round()`: half-to-even.
pub fn py_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipped_labels() {
        assert_eq!(AspectRatio::fixed("3:2", 3.0, 2.0).flipped().label, "2:3");
        assert_eq!(ratio_from_label("9:16").unwrap().value(), Some(9.0 / 16.0));
        assert_eq!(ratio_from_label("7:3").unwrap().label, "7:3");
        assert!(ratio_from_label("nope").is_none());
    }

    #[test]
    fn construction_normalises() {
        let c = Crop::new(0.9, 0.8, 0.1, 0.2, None);
        assert_eq!((c.left, c.top, c.right, c.bottom), (0.1, 0.2, 0.9, 0.8));
        let tiny = Crop::new(0.5, 0.5, 0.5, 0.5, None);
        assert!((tiny.width() - MIN_EDGE).abs() < 1e-12);
    }

    #[test]
    fn pixels_keep_exact_ratio() {
        // A 16:9 crop of a 6000x4000 frame.
        let c = Crop::new(0.0, 0.0625, 1.0, 0.0625 + 0.84375, None);
        let (w, h) = c.pixel_size(6000, 4000);
        assert_eq!((w, h), (6000, 3375));
        let far = Crop::new(0.5, 0.5, 1.0, 1.0, None);
        assert_eq!(far.to_pixels(101, 101), (50, 50, 100, 100));
    }

    #[test]
    fn json_round_trip() {
        let c = Crop::new(0.08, 0.0, 0.92, 1.0, Some("1:1".into()));
        let back = Crop::from_json(&c.to_json()).unwrap();
        assert_eq!(back, c);
        assert_eq!(
            serde_json::to_string(&c.to_json()).unwrap(),
            r#"{"left":0.08,"top":0.0,"right":0.92,"bottom":1.0,"aspect":"1:1"}"#
        );
    }
}
