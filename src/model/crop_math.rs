//! Crop-rect geometry for the interactive crop tool, in normalised (0–1)
//! image coordinates.  Pure functions, shared by the viewer's overlay and
//! the crop bar's reset / aspect buttons.

use super::crop::{Crop, MIN_EDGE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    None,
    Inside,
    Top,
    Bottom,
    Left,
    Right,
    TL,
    TR,
    BL,
    BR,
}

impl Region {
    pub fn is_corner(self) -> bool {
        matches!(self, Region::TL | Region::TR | Region::BL | Region::BR)
    }
}

/// A pixel ratio (w/h) expressed in normalised units for an image.
pub fn normalised_ratio(pixel_ratio: Option<f64>, img_w: u32, img_h: u32) -> Option<f64> {
    let r = pixel_ratio?;
    if img_w == 0 || img_h == 0 {
        return None;
    }
    Some(r * (img_h as f64 / img_w as f64))
}

/// Largest rect of `ratio` that fits in the frame, centred on `crop`.
pub fn refit_to_aspect(crop: &Crop, ratio: Option<f64>) -> Crop {
    let Some(ratio) = ratio else { return crop.clone() };
    let cx = (crop.left + crop.right) / 2.0;
    let cy = (crop.top + crop.bottom) / 2.0;
    let mut w = crop.width();
    let mut h = w / ratio;
    if h > crop.height() {
        h = crop.height();
        w = h * ratio;
    }
    w = w.min(1.0);
    // (h is re-derived from w below, so it needs no clamp of its own)
    if w / ratio > 1.0 {
        w = ratio;
        h = 1.0;
    } else {
        h = w / ratio;
    }
    let left = (cx - w / 2.0).max(0.0).min(1.0 - w);
    let top = (cy - h / 2.0).max(0.0).min(1.0 - h);
    Crop::new(left, top, left + w, top + h, crop.aspect_label.clone())
}

/// Move without resizing; the rect stops at the frame edge.
pub fn translate(crop: &Crop, dx: f64, dy: f64) -> Crop {
    let dx = dx.max(-crop.left).min(1.0 - crop.right);
    let dy = dy.max(-crop.top).min(1.0 - crop.bottom);
    Crop::new(crop.left + dx, crop.top + dy, crop.right + dx, crop.bottom + dy, crop.aspect_label.clone())
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Resize by dragging `region` to `(ax, ay)`, honouring `ratio` if set.
pub fn resize(crop: &Crop, region: Region, ax: f64, ay: f64, ratio: Option<f64>) -> Crop {
    let (ax, ay) = (clamp01(ax), clamp01(ay));
    let (mut l, mut t, mut r, mut b) = (crop.left, crop.top, crop.right, crop.bottom);
    if matches!(region, Region::Left | Region::TL | Region::BL) {
        l = ax.min(r - MIN_EDGE);
    }
    if matches!(region, Region::Right | Region::TR | Region::BR) {
        r = ax.max(l + MIN_EDGE);
    }
    if matches!(region, Region::Top | Region::TL | Region::TR) {
        t = ay.min(b - MIN_EDGE);
    }
    if matches!(region, Region::Bottom | Region::BL | Region::BR) {
        b = ay.max(t + MIN_EDGE);
    }
    if let Some(ratio) = ratio {
        (l, t, r, b) = apply_ratio(l, t, r, b, region, ratio);
    }
    Crop::new(l, t, r, b, crop.aspect_label.clone())
}

/// Force width/height == ratio, anchored so the held handle stays under the
/// cursor.  Corners anchor the opposite corner; edges anchor the opposite
/// edge and grow the other axis symmetrically about the centre.
pub fn apply_ratio(mut l: f64, mut t: f64, mut r: f64, mut b: f64, region: Region, ratio: f64) -> (f64, f64, f64, f64) {
    let (mut w, mut h) = (r - l, b - t);
    if region.is_corner() {
        if w / ratio >= h {
            h = w / ratio;
        } else {
            w = h * ratio;
        }
        let anchor_right = matches!(region, Region::TL | Region::BL);
        let anchor_bottom = matches!(region, Region::TL | Region::TR);
        let max_w = if anchor_right { r } else { 1.0 - l };
        let max_h = if anchor_bottom { b } else { 1.0 - t };
        if w > max_w {
            w = max_w;
            h = max_w / ratio;
        }
        if h > max_h {
            h = max_h;
            w = max_h * ratio;
        }
        if anchor_right {
            l = r - w;
        } else {
            r = l + w;
        }
        if anchor_bottom {
            t = b - h;
        } else {
            b = t + h;
        }
        return (l, t, r, b);
    }
    if matches!(region, Region::Left | Region::Right) {
        h = w / ratio;
        let cy = (t + b) / 2.0;
        if h > 1.0 {
            h = 1.0;
            w = ratio;
            if region == Region::Left {
                l = r - w;
            } else {
                r = l + w;
            }
        }
        t = (cy - h / 2.0).max(0.0).min(1.0 - h);
        b = t + h;
        return (l, t, r, b);
    }
    // Top / bottom
    w = h * ratio;
    let cx = (l + r) / 2.0;
    if w > 1.0 {
        w = 1.0;
        h = 1.0 / ratio;
        if region == Region::Top {
            t = b - h;
        } else {
            b = t + h;
        }
    }
    l = (cx - w / 2.0).max(0.0).min(1.0 - w);
    r = l + w;
    (l, t, r, b)
}

/// Back to the full frame, honouring an aspect lock.
pub fn reset(crop: &Crop, ratio: Option<f64>) -> Crop {
    let full = Crop::full(crop.aspect_label.clone());
    match ratio {
        Some(_) => refit_to_aspect(&full, ratio),
        None => full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refit_keeps_ratio_inside_frame() {
        // 3:2 image, square crop → normalised ratio 2/3.
        let r = normalised_ratio(Some(1.0), 3000, 2000);
        let c = refit_to_aspect(&Crop::full(None), r);
        assert!((c.width() / c.height() - 2.0 / 3.0).abs() < 1e-9);
        assert!((c.height() - 1.0).abs() < 1e-9);
        assert!((c.left - (1.0 - 2.0 / 3.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn corner_resize_anchors_opposite_corner() {
        let c = Crop::new(0.2, 0.2, 0.6, 0.6, None);
        let out = resize(&c, Region::BR, 0.9, 0.7, Some(1.0));
        assert!((out.left - 0.2).abs() < 1e-9 && (out.top - 0.2).abs() < 1e-9);
        assert!((out.width() - out.height()).abs() < 1e-9);
    }
}
