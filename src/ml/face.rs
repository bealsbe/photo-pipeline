//! Faces in the helper (plan §3.3, §5.2): YuNet's boxes and five
//! landmarks decoded from its raw outputs, the quality gate, the ArcFace
//! alignment every recognition model expects, and each face model's
//! input.  Pure arithmetic on pixels and tensors; the sessions themselves
//! live in [`super::worker`].
//!
//! **YuNet** (OpenCV Zoo, 2023 March) is an anchor-free detector over
//! three strides (8, 16, 32).  For each cell it gives a class score, an
//! objectness, a box (centre offset and log size, in strides) and five
//! landmarks (right eye, left eye, nose tip, right and left mouth
//! corner).  The score is the geometric mean of the two; boxes are kept
//! above [`MIN_SCORE`] (the study's quality gate, Immich's reason too),
//! merged by non-maximum suppression at [`NMS_IOU`], and only faces of at
//! least [`MIN_FACE_PX`] on the full preview are recognised.
//!
//! The detector runs on a fixed square canvas ([`DET_SIZE`]): the preview
//! is scaled to fit (about half size for a 1 616-px preview, which kept
//! 29 of 30 faces in the study) and padded with black, so every photo has
//! the same tensor shape and a chunk can be batched on the card.

use crate::imaging::decode::{self, Pixels};

/// The detector's canvas edge (a multiple of 32).
pub const DET_SIZE: u32 = 832;
/// Faces below this YuNet score are ignored (the study: 0.8 let blurred
/// faces of different people share groups; 0.9 didn't).
pub const MIN_SCORE: f32 = 0.9;
/// Boxes overlapping more than this with a better one are dropped.
pub const NMS_IOU: f32 = 0.3;
/// Smaller faces (longer box side, in the full preview's pixels) are not
/// worth recognising.
pub const MIN_FACE_PX: f32 = 40.0;
/// Every recognition model's input: a 112×112 aligned crop.
pub const ALIGNED: u32 = 112;
/// Faces kept per photo at most (a crowd shot).
pub const MAX_FACES: usize = 64;

/// The ArcFace landmark template the models were trained to (OpenCV's
/// `alignCrop`, InsightFace's `arcface_dst`): right eye, left eye, nose,
/// right mouth corner, left mouth corner, in a 112×112 crop.
pub const TEMPLATE: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// A face as YuNet found it, in the detector canvas's pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    /// `[x, y, width, height]`.
    pub bbox: [f32; 4],
    /// Five landmarks, `[x, y]` each, in the template's order.
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
}

/// The photo on the detector's canvas: scaled to fit and padded.
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas {
    /// `3 × DET_SIZE × DET_SIZE` bytes, CHW, BGR, 0–255 (what YuNet was
    /// trained on: OpenCV's blob of a BGR `Mat` with no scaling); kept as
    /// bytes until a batch is made, since a float canvas is 8 MB.
    pub planes: Vec<u8>,
    /// Canvas pixels per preview pixel.
    pub scale: f32,
}

/// Floats per canvas.
pub const CANVAS_LEN: usize = 3 * (DET_SIZE * DET_SIZE) as usize;

/// Put an oriented RGB (or grey) preview on the canvas.
pub fn canvas(px: &Pixels) -> Canvas {
    let scale = (DET_SIZE as f32 / px.width.max(px.height).max(1) as f32).min(1.0);
    let (w, h) = (((px.width as f32 * scale).round() as u32).clamp(1, DET_SIZE), ((px.height as f32 * scale).round() as u32).clamp(1, DET_SIZE));
    let small = decode::resize_to(px, w, h);
    let n = (DET_SIZE * DET_SIZE) as usize;
    let mut planes = vec![0u8; 3 * n];
    let c = small.channels as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = &small.data[(y * w as usize + x) * c..][..c];
            let (r, g, b) = if c >= 3 { (p[0], p[1], p[2]) } else { (p[0], p[0], p[0]) };
            let i = y * DET_SIZE as usize + x;
            planes[i] = b;
            planes[n + i] = g;
            planes[2 * n + i] = r;
        }
    }
    Canvas { planes, scale: w as f32 / px.width.max(1) as f32 }
}

/// Cells per image at each stride, in the order the outputs are read
/// (8, 16, 32).
pub const STRIDES: [u32; 3] = [8, 16, 32];

/// Cells per image at a stride.
pub fn cells(stride: u32) -> usize {
    ((DET_SIZE / stride) * (DET_SIZE / stride)) as usize
}

/// One stride's raw outputs.
pub struct Level<'a> {
    pub stride: u32,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
    pub kps: &'a [f32],
}

/// Decode the three levels' outputs for one `DET_SIZE` canvas: scored,
/// gated, merged, best first.
pub fn decode_levels(levels: &[Level<'_>]) -> Vec<Detected> {
    let mut found = Vec::new();
    for l in levels {
        let cols = (DET_SIZE / l.stride) as usize;
        let rows = cols;
        for idx in 0..rows * cols {
            let (Some(&c), Some(&o)) = (l.cls.get(idx), l.obj.get(idx)) else { break };
            let score = (c.clamp(0.0, 1.0) * o.clamp(0.0, 1.0)).sqrt();
            if score < MIN_SCORE {
                continue;
            }
            let (Some(b), Some(k)) = (l.bbox.get(idx * 4..idx * 4 + 4), l.kps.get(idx * 10..idx * 10 + 10)) else { break };
            let (r, col) = ((idx / cols) as f32, (idx % cols) as f32);
            let s = l.stride as f32;
            let cx = (col + b[0]) * s;
            let cy = (r + b[1]) * s;
            let w = b[2].exp() * s;
            let h = b[3].exp() * s;
            let mut landmarks = [[0f32; 2]; 5];
            for (i, lm) in landmarks.iter_mut().enumerate() {
                *lm = [(col + k[2 * i]) * s, (r + k[2 * i + 1]) * s];
            }
            found.push(Detected { bbox: [cx - w / 2.0, cy - h / 2.0, w, h], landmarks, score });
        }
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    nms(found)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Non-maximum suppression over boxes sorted best first.
pub fn nms(sorted: Vec<Detected>) -> Vec<Detected> {
    let mut kept: Vec<Detected> = Vec::new();
    for d in sorted {
        if kept.len() >= MAX_FACES {
            break;
        }
        if kept.iter().all(|k| iou(&k.bbox, &d.bbox) <= NMS_IOU) {
            kept.push(d);
        }
    }
    kept
}

/// A face's place in the oriented preview, as fractions 0..1, from a
/// canvas detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// `[x, y, width, height]`, fractions of the preview.
    pub bbox: [f32; 4],
    /// Landmarks as fractions of the preview.
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
    /// The longer box side in full-preview pixels.
    pub size_px: f32,
}

/// Map canvas detections back onto the preview (`width`×`height`,
/// oriented), dropping faces under [`MIN_FACE_PX`].
pub fn place(found: &[Detected], scale: f32, width: u32, height: u32) -> Vec<Placed> {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    found
        .iter()
        .filter_map(|d| {
            let bw = d.bbox[2] / scale;
            let bh = d.bbox[3] / scale;
            let size_px = bw.max(bh);
            if size_px < MIN_FACE_PX {
                return None;
            }
            let x = (d.bbox[0] / scale / w).clamp(0.0, 1.0);
            let y = (d.bbox[1] / scale / h).clamp(0.0, 1.0);
            let bbox = [x, y, (bw / w).min(1.0 - x), (bh / h).min(1.0 - y)];
            let landmarks = d.landmarks.map(|[lx, ly]| [lx / scale / w, ly / scale / h]);
            Some(Placed { bbox, landmarks, score: d.score, size_px })
        })
        .collect()
}

/// The similarity transform (scale, rotation, translation) that best maps
/// `src` points onto `dst` (Umeyama, without reflection), as the 2×3
/// matrix `[[a, -b, tx], [b, a, ty]]`.
pub fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = src.len() as f32;
    let mean = |p: &[[f32; 2]; 5]| [p.iter().map(|q| q[0]).sum::<f32>() / n, p.iter().map(|q| q[1]).sum::<f32>() / n];
    let (ms, md) = (mean(src), mean(dst));
    // Cross terms of the centred points.
    let (mut sxx, mut sxy, mut syx, mut syy, mut var) = (0f32, 0f32, 0f32, 0f32, 0f32);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy) = (s[0] - ms[0], s[1] - ms[1]);
        let (dx, dy) = (d[0] - md[0], d[1] - md[1]);
        sxx += dx * sx;
        sxy += dx * sy;
        syx += dy * sx;
        syy += dy * sy;
        var += sx * sx + sy * sy;
    }
    // For a 2-D similarity the optimal rotation is atan2 of the
    // antisymmetric part; the scale follows from the traces.
    let a_num = sxx + syy;
    let b_num = syx - sxy;
    let scale = if var > 0.0 { (a_num * a_num + b_num * b_num).sqrt() / var } else { 1.0 };
    let theta = b_num.atan2(a_num);
    let (a, b) = (scale * theta.cos(), scale * theta.sin());
    let tx = md[0] - (a * ms[0] - b * ms[1]);
    let ty = md[1] - (b * ms[0] + a * ms[1]);
    [[a, -b, tx], [b, a, ty]]
}

/// Invert a similarity matrix.
fn invert(m: &[[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let (a, b) = (m[0][0], m[1][0]);
    let det = a * a + b * b;
    let (ia, ib) = if det > 0.0 { (a / det, -b / det) } else { (1.0, 0.0) };
    let tx = -(ia * m[0][2] - ib * m[1][2]);
    let ty = -(ib * m[0][2] + ia * m[1][2]);
    [[ia, -ib, tx], [ib, ia, ty]]
}

/// The aligned 112×112 RGB crop of a face: the landmarks (in preview
/// pixels) mapped onto the template, sampled bilinearly from the
/// oriented preview.  Outside the preview is black.
pub fn align(px: &Pixels, landmarks: &[[f32; 2]; 5]) -> Pixels {
    let m = similarity(landmarks, &TEMPLATE);
    let inv = invert(&m);
    let (w, h, c) = (px.width as i32, px.height as i32, px.channels as usize);
    let mut out = vec![0u8; (ALIGNED * ALIGNED * 3) as usize];
    for oy in 0..ALIGNED {
        for ox in 0..ALIGNED {
            let (fx, fy) = (ox as f32, oy as f32);
            let sx = inv[0][0] * fx + inv[0][1] * fy + inv[0][2];
            let sy = inv[1][0] * fx + inv[1][1] * fy + inv[1][2];
            let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
            if x0 < -1 || y0 < -1 || x0 >= w || y0 >= h {
                continue;
            }
            let (ax, ay) = (sx - x0 as f32, sy - y0 as f32);
            let sample = |x: i32, y: i32, ch: usize| -> f32 {
                if x < 0 || y < 0 || x >= w || y >= h {
                    return 0.0;
                }
                let i = (y as usize * w as usize + x as usize) * c;
                px.data[i + if c >= 3 { ch } else { 0 }] as f32
            };
            let o = ((oy * ALIGNED + ox) * 3) as usize;
            for ch in 0..3 {
                let v = sample(x0, y0, ch) * (1.0 - ax) * (1.0 - ay) + sample(x0 + 1, y0, ch) * ax * (1.0 - ay) + sample(x0, y0 + 1, ch) * (1.0 - ax) * ay + sample(x0 + 1, y0 + 1, ch) * ax * ay;
                out[o + ch] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Pixels::new(ALIGNED, ALIGNED, 3, out)
}

/// How a recognition model wants its crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Norm {
    /// `(v − 127.5) / 127.5`, RGB (AuraFace, InsightFace).
    Centred,
    /// 0–255 as is, RGB (SFace, as OpenCV feeds it).
    Raw,
}

/// An aligned crop as a model's CHW input.
pub fn face_input(crop: &Pixels, norm: Norm) -> Vec<f32> {
    let n = (ALIGNED * ALIGNED) as usize;
    let c = crop.channels as usize;
    let mut out = vec![0f32; 3 * n];
    for (i, p) in crop.data.chunks_exact(c).enumerate() {
        for ch in 0..3 {
            let v = p[if c >= 3 { ch } else { 0 }] as f32;
            out[ch * n + i] = match norm {
                Norm::Centred => (v - 127.5) / 127.5,
                Norm::Raw => v,
            };
        }
    }
    out
}

/// How sharp an aligned crop is: the variance of its Laplacian on the
/// luma (the study's measure), for clearest-first ordering.
pub fn sharpness(crop: &Pixels) -> f32 {
    let (w, h, c) = (crop.width as usize, crop.height as usize, crop.channels as usize);
    let luma = |x: usize, y: usize| -> f32 {
        let p = &crop.data[(y * w + x) * c..][..c];
        if c >= 3 { 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32 } else { p[0] as f32 }
    };
    let mut sum = 0f64;
    let mut sum2 = 0f64;
    let mut n = 0usize;
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let l = 4.0 * luma(x, y) - luma(x - 1, y) - luma(x + 1, y) - luma(x, y - 1) - luma(x, y + 1);
            sum += l as f64;
            sum2 += (l * l) as f64;
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let mean = sum / n as f64;
    (sum2 / n as f64 - mean * mean).max(0.0) as f32
}

/// A face's clarity (UI 12): how large and how sharp it is, on a 0..1
/// scale that saturates for a sharp face of 200 px or more.  The grouping
/// adds how sure its match is.
pub fn clarity(size_px: f32, sharpness: f32, score: f32) -> f32 {
    let size = (size_px / 200.0).clamp(0.0, 1.0);
    let sharp = (sharpness / 400.0).clamp(0.0, 1.0);
    (0.5 * size + 0.3 * sharp + 0.2 * score.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canvas_scales_to_fit_and_is_bgr_0_to_255() {
        let mut data = vec![0u8; 1600 * 1000 * 3];
        data[0..3].copy_from_slice(&[255, 0, 0]); // a red pixel top left
        let px = Pixels::new(1600, 1000, 3, data);
        let c = canvas(&px);
        assert!((c.scale - 0.52).abs() < 0.01, "{}", c.scale);
        let n = (DET_SIZE * DET_SIZE) as usize;
        assert_eq!(c.planes.len(), CANVAS_LEN);
        assert!(c.planes[2 * n] > 60 && c.planes[0] < 10, "red lands in the R plane: {} {}", c.planes[2 * n], c.planes[0]);
        // Below the fitted height the canvas is black padding.
        assert_eq!(c.planes[(DET_SIZE as usize - 1) * DET_SIZE as usize], 0);
        assert_eq!(cells(8) + cells(16) + cells(32), 10816 + 2704 + 676);
        // A small photo is not scaled up.
        assert_eq!(canvas(&Pixels::new(400, 300, 3, vec![9; 400 * 300 * 3])).scale, 1.0);
    }

    #[test]
    fn decoding_places_boxes_by_cell_and_stride_and_merges_overlaps() {
        let cols = (DET_SIZE / 32) as usize;
        let n = cols * cols;
        let mut cls = vec![0f32; n];
        let mut obj = vec![0f32; n];
        let mut bbox = vec![0f32; 4 * n];
        let kps = vec![0.5f32; 10 * n];
        // Cell (row 3, col 5): a 64 × 64 face centred on the cell.
        let idx = 3 * cols + 5;
        cls[idx] = 0.95;
        obj[idx] = 0.95;
        bbox[idx * 4] = 0.5;
        bbox[idx * 4 + 1] = 0.5;
        bbox[idx * 4 + 2] = 2f32.ln();
        bbox[idx * 4 + 3] = 2f32.ln();
        // Its neighbour: the same face again, a little weaker: merged.
        let j = idx + 1;
        cls[j] = 0.9;
        obj[j] = 0.92;
        bbox[j * 4] = -0.5;
        bbox[j * 4 + 1] = 0.5;
        bbox[j * 4 + 2] = 2f32.ln();
        bbox[j * 4 + 3] = 2f32.ln();
        // A weak one elsewhere: gated out.
        cls[0] = 0.5;
        obj[0] = 0.5;
        let found = decode_levels(&[Level { stride: 32, cls: &cls, obj: &obj, bbox: &bbox, kps: &kps }]);
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert!((f.bbox[0] - (5.5 * 32.0 - 32.0)).abs() < 1e-3 && (f.bbox[1] - (3.5 * 32.0 - 32.0)).abs() < 1e-3);
        assert!((f.bbox[2] - 64.0).abs() < 1e-3 && (f.score - 0.95).abs() < 1e-3);
        assert!((f.landmarks[0][0] - 5.5 * 32.0).abs() < 1e-3);
        // Back on a 1600 × 1000 preview at the canvas scale.
        let placed = place(&found, 0.52, 1600, 1000);
        assert_eq!(placed.len(), 1);
        assert!((placed[0].size_px - 64.0 / 0.52).abs() < 0.1);
        assert!((placed[0].bbox[2] - 64.0 / 0.52 / 1600.0).abs() < 1e-4);
        assert!(placed[0].bbox.iter().all(|v| (0.0..=1.0).contains(v)));
        // Too small on the preview: dropped.
        assert!(place(&found, 2.0, 1600, 1000).is_empty());
    }

    #[test]
    fn the_similarity_maps_the_template_onto_itself_and_a_scaled_rotated_copy_back() {
        let m = similarity(&TEMPLATE, &TEMPLATE);
        assert!((m[0][0] - 1.0).abs() < 1e-4 && m[1][0].abs() < 1e-4 && m[0][2].abs() < 1e-3 && m[1][2].abs() < 1e-3, "{m:?}");
        // Landmarks of a face twice the size, turned 30°, moved to (500, 300).
        let (s, t) = (2.0f32, 30f32.to_radians());
        let moved = TEMPLATE.map(|[x, y]| [s * (t.cos() * x - t.sin() * y) + 500.0, s * (t.sin() * x + t.cos() * y) + 300.0]);
        let m = similarity(&moved, &TEMPLATE);
        for (p, q) in moved.iter().zip(&TEMPLATE) {
            let x = m[0][0] * p[0] + m[0][1] * p[1] + m[0][2];
            let y = m[1][0] * p[0] + m[1][1] * p[1] + m[1][2];
            assert!((x - q[0]).abs() < 0.05 && (y - q[1]).abs() < 0.05, "{p:?} -> {x},{y} not {q:?}");
        }
        let inv = invert(&m);
        let x = inv[0][0] * TEMPLATE[2][0] + inv[0][1] * TEMPLATE[2][1] + inv[0][2];
        assert!((x - moved[2][0]).abs() < 0.05);
    }

    #[test]
    fn alignment_samples_the_face_and_inputs_are_normalised() {
        // A 400 × 400 picture with a bright square where the eyes sit when
        // the template is scaled ×2 at the origin.
        let mut data = vec![20u8; 400 * 400 * 3];
        for y in 95..115 {
            for x in 70..155 {
                let i = (y * 400 + x) * 3;
                data[i..i + 3].copy_from_slice(&[240, 240, 240]);
            }
        }
        let px = Pixels::new(400, 400, 3, data);
        let lm = TEMPLATE.map(|[x, y]| [2.0 * x, 2.0 * y]);
        let crop = align(&px, &lm);
        assert_eq!((crop.width, crop.height, crop.channels), (ALIGNED, ALIGNED, 3));
        let at = |x: u32, y: u32| crop.data[((y * ALIGNED + x) * 3) as usize];
        assert!(at(38, 51) > 200 && at(73, 51) > 200, "the eyes land bright: {} {}", at(38, 51), at(73, 51));
        assert!(at(56, 100) < 40, "the chin stays dark: {}", at(56, 100));
        let centred = face_input(&crop, Norm::Centred);
        assert_eq!(centred.len(), 3 * (ALIGNED * ALIGNED) as usize);
        assert!(centred.iter().all(|v| (-1.0..=1.0).contains(v)));
        let raw = face_input(&crop, Norm::Raw);
        assert!(raw.iter().any(|&v| v > 200.0));
        // Sharpness: an edge beats a flat field; clarity grows with size.
        assert!(sharpness(&crop) > sharpness(&Pixels::new(ALIGNED, ALIGNED, 3, vec![100; (ALIGNED * ALIGNED * 3) as usize])));
        assert!(clarity(200.0, 400.0, 1.0) > clarity(40.0, 400.0, 1.0));
        assert!(clarity(40.0, 0.0, 0.9) > 0.0 && clarity(1000.0, 1e6, 1.0) <= 1.0);
        // Landmarks off the picture: a black crop, no panic.
        let off = align(&px, &TEMPLATE.map(|[x, y]| [x + 5000.0, y + 5000.0]));
        assert!(off.data.iter().all(|&v| v == 0));
    }

    /// Print each face model's inputs and outputs: `PP_MODEL_ROOT=<models
    /// dir>` (the registry's folder layout).
    #[test]
    #[ignore = "needs PP_MODEL_ROOT with the face models"]
    fn print_face_model_io() {
        use ort::session::Session;
        let root = std::path::PathBuf::from(std::env::var("PP_MODEL_ROOT").expect("PP_MODEL_ROOT"));
        for id in [crate::ml::models::ModelId::Yunet, crate::ml::models::ModelId::AuraFace, crate::ml::models::ModelId::SFace, crate::ml::models::ModelId::InsightFace] {
            let info = crate::ml::models::info(id);
            let path = root.join(id.dir_name()).join(info.files[0].file);
            let s = Session::builder().unwrap().commit_from_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            eprintln!("== {id:?}");
            for i in s.inputs() {
                eprintln!("  in  {} {:?}", i.name(), i.dtype());
            }
            for o in s.outputs() {
                eprintln!("  out {} {:?}", o.name(), o.dtype());
            }
        }
    }
}
