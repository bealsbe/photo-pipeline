//! Placeholder art for the fursuits stand-in: grey cartoon fursuit heads,
//! drawn in code (the approved mockup's `charHead`), never a real photo.
//!
//! Each made-up suit has a [`Look`]: its ears (fox, cat, bear, rabbit,
//! dragon horns, dog), a marking (a mask, a blaze, spots or none) and three
//! greys.  A head crop frames the head square, a little off-centre and
//! tilted each time; a whole made-up photo puts the suit, body and raised
//! paws included, where its box says.  Greys only, so nothing here can be
//! mistaken for a photo of anyone.

use std::hash::{Hash, Hasher};

use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, LineCap, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode, Stroke, Transform};

/// The shape of a suit's ears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ears {
    Fox,
    Cat,
    Bear,
    Rabbit,
    Dragon,
    Dog,
}

/// A marking on the face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Mask,
    Blaze,
    Spots,
}

/// One made-up suit's look, in greys (0–255).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub ears: Ears,
    pub mark: Mark,
    pub fur: u8,
    pub muzzle: u8,
    pub dark: u8,
}

/// The stand-in's suits.  Look-alikes on purpose (0, 6 and 7 are all
/// masked foxes in close greys), so groups mix and the review has wrong
/// suggestions to turn down, as the study found with real suits.
pub const LOOKS: [Look; 12] = [
    Look { ears: Ears::Fox, mark: Mark::Mask, fur: 178, muzzle: 228, dark: 62 },
    Look { ears: Ears::Bear, mark: Mark::Spots, fur: 108, muzzle: 168, dark: 58 },
    Look { ears: Ears::Rabbit, mark: Mark::Blaze, fur: 204, muzzle: 236, dark: 90 },
    Look { ears: Ears::Dragon, mark: Mark::None, fur: 86, muzzle: 136, dark: 40 },
    Look { ears: Ears::Dog, mark: Mark::Mask, fur: 146, muzzle: 210, dark: 64 },
    Look { ears: Ears::Cat, mark: Mark::Blaze, fur: 66, muzzle: 150, dark: 30 },
    Look { ears: Ears::Fox, mark: Mark::Spots, fur: 132, muzzle: 200, dark: 52 },
    Look { ears: Ears::Fox, mark: Mark::Mask, fur: 150, muzzle: 214, dark: 48 },
    Look { ears: Ears::Cat, mark: Mark::None, fur: 190, muzzle: 232, dark: 70 },
    Look { ears: Ears::Bear, mark: Mark::Blaze, fur: 74, muzzle: 120, dark: 36 },
    Look { ears: Ears::Rabbit, mark: Mark::Spots, fur: 120, muzzle: 190, dark: 44 },
    Look { ears: Ears::Dog, mark: Mark::None, fur: 210, muzzle: 240, dark: 96 },
];

/// The look of made-up suit `suit`.
pub fn look(suit: usize) -> Look {
    LOOKS[suit % LOOKS.len()]
}

fn unit(x: &impl Hash) -> f32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    (h.finish() % 1_000_000) as f32 / 1_000_000.0
}

fn grey(v: f32) -> Color {
    let v = v.clamp(0.0, 255.0) as u8;
    Color::from_rgba8(v, v, v, 255)
}

/// Fills and strokes on one pixmap, under one transform.
struct Brush<'a> {
    pm: &'a mut Pixmap,
    tr: Transform,
}

impl Brush<'_> {
    fn fill(&mut self, pb: PathBuilder, v: f32) {
        self.fill_tr(pb, v, Transform::identity());
    }

    /// Fill with an extra transform applied first (a rotated ear).
    fn fill_tr(&mut self, pb: PathBuilder, v: f32, local: Transform) {
        if let Some(p) = pb.finish() {
            let mut paint = Paint::default();
            paint.anti_alias = true;
            paint.set_color(grey(v));
            self.pm.fill_path(&p, &paint, FillRule::Winding, self.tr.pre_concat(local), None);
        }
    }

    fn oval(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, v: f32) {
        self.oval_rot(cx, cy, rx, ry, 0.0, v);
    }

    fn oval_rot(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, deg: f32, v: f32) {
        let mut pb = PathBuilder::new();
        if let Some(r) = Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0) {
            pb.push_oval(r);
        }
        self.fill_tr(pb, v, Transform::from_rotate_at(deg, cx, cy));
    }

    fn poly(&mut self, pts: &[(f32, f32)], v: f32) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        self.fill(pb, v);
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, v: f32) {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
        if let Some(p) = pb.finish() {
            let mut paint = Paint::default();
            paint.anti_alias = true;
            paint.set_color(grey(v));
            let stroke = Stroke { width, line_cap: LineCap::Round, ..Stroke::default() };
            self.pm.stroke_path(&p, &paint, &stroke, self.tr, None);
        }
    }
}

/// A cartoon fursuit head centred at (`x`, `cy`), `s` its half width: full
/// ears with a lighter inside, cheek ruffs, big eyes and a two-lobed muzzle
/// with a nose (the mockup's `charHead`, point for point).
fn head(b: &mut Brush<'_>, x: f32, cy: f32, s: f32, l: Look) {
    let (fur, muz, dark) = (l.fur as f32, l.muzzle as f32, l.dark as f32);
    let inner = (fur + muz) / 2.0 + 12.0;
    let p = |dx: f32, dy: f32| (x + dx * s, cy + dy * s);
    let quad_shape = |pts: [(f32, f32); 5]| {
        // M a Q b c Q d e Z
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0, pts[0].1);
        pb.quad_to(pts[1].0, pts[1].1, pts[2].0, pts[2].1);
        pb.quad_to(pts[3].0, pts[3].1, pts[4].0, pts[4].1);
        pb.close();
        pb
    };
    for d in [-1.0f32, 1.0] {
        match l.ears {
            Ears::Fox | Ears::Cat => {
                let (t, w) = if l.ears == Ears::Fox { (1.55, 1.0) } else { (1.2, 1.04) };
                b.fill(quad_shape([p(d * 0.9, -0.1), p(d * w, -t * 0.72), p(d * 0.68, -t), p(d * 0.36, -t * 0.66), p(d * 0.08, -0.66)]), fur);
                b.fill(quad_shape([p(d * 0.74, -0.36), p(d * 0.8, -t * 0.66), p(d * 0.64, -t * 0.82), p(d * 0.44, -t * 0.6), p(d * 0.3, -0.62)]), inner);
                if l.ears == Ears::Fox {
                    b.fill(quad_shape([p(d * 0.78, -1.28), p(d * 0.74, -1.45), p(d * 0.68, -t), p(d * 0.58, -1.42), p(d * 0.52, -1.3)]), dark);
                }
            }
            Ears::Bear => {
                b.oval(x + d * s * 0.66, cy - s * 0.66, s * 0.36, s * 0.36, fur);
                b.oval(x + d * s * 0.66, cy - s * 0.66, s * 0.2, s * 0.2, inner);
            }
            Ears::Rabbit => {
                b.oval_rot(x + d * s * 0.34, cy - s * 1.36, s * 0.23, s * 0.66, d * 9.0, fur);
                b.oval_rot(x + d * s * 0.35, cy - s * 1.32, s * 0.11, s * 0.48, d * 9.0, inner);
            }
            Ears::Dragon => {
                b.fill(quad_shape([p(d * 0.45, -0.6), p(d * 0.95, -1.2), p(d * 1.05, -1.55), p(d * 0.6, -1.15), p(d * 0.2, -0.72)]), fur + 70.0);
                b.poly(&[p(d * 0.82, -0.3), p(d * 1.12, -0.5), p(d * 0.96, -0.18), p(d * 1.16, -0.12), p(d * 0.86, 0.02)], dark);
            }
            Ears::Dog => b.oval_rot(x + d * s * 0.86, cy + s * 0.08, s * 0.3, s * 0.62, -d * 18.0, dark),
        }
    }
    // Cheek ruffs, then the head and its tuft.
    for d in [-1.0f32, 1.0] {
        b.poly(&[p(d * 0.7, -0.12), p(d * 1.12, 0.1), p(d * 0.92, 0.24), p(d * 1.1, 0.44), p(d * 0.84, 0.5), p(d * 0.96, 0.7), p(d * 0.6, 0.62)], fur);
    }
    b.oval(x, cy, s * 0.9, s * 0.82, fur);
    b.poly(&[p(-0.24, -0.76), p(-0.12, -0.98), p(-0.02, -0.8), p(0.1, -1.0), p(0.24, -0.76)], fur);
    match l.mark {
        Mark::Mask => {
            let mut pb = PathBuilder::new();
            let (w, h) = (s * 1.6, s * 0.38);
            let (x0, y0, r) = (x - s * 0.8, cy - s * 0.34, s * 0.19);
            pb.move_to(x0 + r, y0);
            pb.line_to(x0 + w - r, y0);
            pb.quad_to(x0 + w, y0, x0 + w, y0 + r);
            pb.quad_to(x0 + w, y0 + h, x0 + w - r, y0 + h);
            pb.line_to(x0 + r, y0 + h);
            pb.quad_to(x0, y0 + h, x0, y0 + r);
            pb.quad_to(x0, y0, x0 + r, y0);
            pb.close();
            b.fill(pb, dark);
        }
        Mark::Blaze => b.oval(x, cy - s * 0.32, s * 0.16, s * 0.44, muz),
        Mark::Spots => {
            b.oval(x - s * 0.5, cy - s * 0.46, s * 0.12, s * 0.12, dark);
            b.oval(x + s * 0.54, cy - s * 0.38, s * 0.09, s * 0.09, dark);
            b.oval(x + s * 0.32, cy - s * 0.62, s * 0.07, s * 0.07, dark);
        }
        Mark::None => {}
    }
    // Big eyes, then the muzzle: two lobes, a chin, a nose and a mouth.
    for d in [-1.0f32, 1.0] {
        b.oval(x + d * s * 0.34, cy - s * 0.14, s * 0.15, s * 0.17, 245.0);
        b.oval(x + d * s * 0.31, cy - s * 0.1, s * 0.09, s * 0.09, 20.0);
        b.oval(x + d * s * 0.28, cy - s * 0.15, s * 0.03, s * 0.03, 245.0);
    }
    b.oval(x, cy + s * 0.58, s * 0.22, s * 0.14, muz);
    for d in [-1.0f32, 1.0] {
        b.oval(x + d * s * 0.2, cy + s * 0.4, s * 0.28, s * 0.28, muz);
    }
    let mut nose = PathBuilder::new();
    let (a, c1, e, c2, f, c3) = (p(-0.16, 0.2), p(0.0, 0.12), p(0.16, 0.2), p(0.08, 0.34), p(0.0, 0.36), p(-0.08, 0.34));
    nose.move_to(a.0, a.1);
    nose.quad_to(c1.0, c1.1, e.0, e.1);
    nose.quad_to(c2.0, c2.1, f.0, f.1);
    nose.quad_to(c3.0, c3.1, a.0, a.1);
    nose.close();
    b.fill(nose, 25.0);
    b.line(p(0.0, 0.36), p(0.0, 0.48), (s * 0.04).max(0.6), 45.0);
}

/// The whole suit: a body, two raised paws with their pads, and the head.
fn suit(b: &mut Brush<'_>, x: f32, cy: f32, s: f32, l: Look) {
    let (fur, muz) = (l.fur as f32, l.muzzle as f32);
    let pad = (fur + muz) / 2.0 + 12.0;
    b.oval(x, cy + s * 1.75, s * 1.15, s * 1.0, fur - 18.0);
    for d in [-1.0f32, 1.0] {
        let (px, py) = (x + d * s * 1.08, cy + s * 1.22);
        b.oval(px, py, s * 0.3, s * 0.3, fur - 8.0);
        b.oval(px, py + s * 0.06, s * 0.13, s * 0.1, pad);
        for (bx, by) in [(-0.15, -0.12), (0.0, -0.18), (0.15, -0.12)] {
            b.oval(px + bx * s, py + by * s, s * 0.05, s * 0.05, pad);
        }
    }
    head(b, x, cy, s, l);
}

fn background(pm: &mut Pixmap, top: f32, bottom: f32, diagonal: bool) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let mut paint = Paint::default();
    let end = if diagonal { Point::from_xy(w, h) } else { Point::from_xy(0.0, h) };
    if let Some(sh) = LinearGradient::new(Point::from_xy(0.0, 0.0), end, vec![GradientStop::new(0.0, grey(top)), GradientStop::new(1.0, grey(bottom))], SpreadMode::Pad, Transform::identity()) {
        paint.shader = sh;
    }
    if let Some(r) = Rect::from_xywh(0.0, 0.0, w, h) {
        pm.fill_rect(r, &paint, Transform::identity(), None);
    }
}

fn opaque(pm: Pixmap) -> Vec<u8> {
    let mut px = pm.take();
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    px
}

/// A head crop, `px` square: suit `suit`'s head framed on a soft grey,
/// shifted, scaled and tilted a little by `seed` (each detection differs).
/// RGBA, straight alpha.
pub fn head_crop(suit_no: usize, seed: u64, px: u32) -> (u32, u32, Vec<u8>) {
    let px = px.max(8);
    let mut pm = Pixmap::new(px, px).expect("pixmap");
    let r = |s: &str| unit(&(seed, s));
    let t = 54.0 + r("tone") * 70.0;
    background(&mut pm, t + 20.0, t - 22.0, true);
    // The mockup's 100-unit square, jittered.
    let k = px as f32 / 100.0;
    let (dx, dy) = ((r("dx") - 0.5) * 10.0, (r("dy") - 0.5) * 8.0);
    let (sc, rot) = (0.92 + r("sc") * 0.16, (r("rot") - 0.5) * 14.0);
    let tr = Transform::from_scale(k, k)
        .pre_translate(50.0 + dx, 50.0 + dy)
        .pre_concat(Transform::from_rotate(rot))
        .pre_scale(sc, sc)
        .pre_translate(-50.0, -50.0);
    let l = look(suit_no);
    let mut b = Brush { pm: &mut pm, tr };
    b.oval(50.0, 118.0, 44.0, 30.0, l.fur as f32 - 18.0);
    head(&mut b, 50.0, 60.0, 30.0, l);
    (px, px, opaque(pm))
}

/// A whole made-up photo, `px` wide (3:2): a floor, now and then a second
/// suit behind, and suit `suit` with its head at `bbox` (fractions of the
/// photo: x, y, width, height).  For "In the photo" beside the review.
pub fn photo(suit_no: usize, seed: u64, bbox: [f32; 4], px: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (px.max(12), (px * 2 / 3).max(8));
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    let r = |s: &str| unit(&(seed, s));
    let top = 84.0 + 48.0 * r("top");
    background(&mut pm, top, top - 40.0 - 20.0 * r("bottom"), false);
    let (wf, hf) = (w as f32, h as f32);
    {
        let mut b = Brush { pm: &mut pm, tr: Transform::identity() };
        b.poly(&[(0.0, hf * 0.78), (wf, hf * 0.78), (wf, hf), (0.0, hf)], 40.0 + 24.0 * r("floor"));
        if r("other") < 0.45 {
            suit(&mut b, wf * (0.8 + 0.1 * r("ox")), hf * 0.48, hf * 0.15, look(suit_no + 5));
        }
        // The head's box is the head with its ears: the suit's scale from it.
        let [bx, by, bw, bh] = bbox;
        let s = bw * wf / 2.24;
        let x = (bx + bw / 2.0) * wf;
        let cy = by * hf + bh * hf * 0.64;
        suit(&mut b, x, cy, s, look(suit_no));
    }
    (w, h, opaque(pm))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn greys_only(px: &[u8]) -> bool {
        px.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255)
    }

    #[test]
    fn heads_are_grey_at_the_size_asked_and_differ_by_suit() {
        let (w, h, a) = head_crop(0, 7, 128);
        assert_eq!((w, h, a.len()), (128, 128, 128 * 128 * 4));
        assert!(greys_only(&a), "placeholder art is greys only");
        // Something is drawn on the background: many tones.
        let tones: std::collections::HashSet<u8> = a.chunks_exact(4).map(|p| p[0]).collect();
        assert!(tones.len() > 24, "{} tones", tones.len());
        // Another suit, same framing: a different picture.
        let (_, _, b) = head_crop(1, 7, 128);
        let differ = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(x, y)| x[0].abs_diff(y[0]) > 24).count();
        assert!(differ > 128 * 128 / 10, "{differ}");
        // The same detection always draws the same.
        assert_eq!(head_crop(0, 7, 128).2, a);
    }

    #[test]
    fn a_made_up_photo_is_three_by_two_and_grey() {
        let (w, h, px) = photo(3, 11, [0.4, 0.2, 0.18, 0.3], 300);
        assert_eq!((w, h), (300, 200));
        assert!(greys_only(&px));
        // The suit's eyes (near white) sit inside its box.
        let white: Vec<(u32, u32)> = px.chunks_exact(4).enumerate().filter(|(_, p)| p[0] >= 240).map(|(i, _)| (i as u32 % w, i as u32 / w)).collect();
        assert!(white.iter().any(|&(x, y)| (120..=174).contains(&x) && (40..=100).contains(&y)), "eyes in the box");
    }
}

