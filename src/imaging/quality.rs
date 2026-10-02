//! Per-shot image measurements for the quality checks: how sharp a frame
//! is and where, how it is exposed, and perceptual hashes.  Pure functions
//! on a luma plane, except [`load`], which reads the camera's embedded
//! preview the way the thumbnailer does.  The decisions made from these
//! (soft, accidental, sharpest, moments) are in `model::hints`.
//!
//! The plane is the camera's ~1616-px embedded preview, analysed as stored
//! (sensor orientation): sharpness and exposure don't care which way up a
//! photo is.  The AF point is turned back into sensor terms, and the hashes
//! are taken as the photo is shown, so a copy saved upright still matches.
//!
//! **Sharpness is local**: the mean squared Laplacian in each tile of a
//! 12×8 grid.  At f/1.4–2.8 most of a good frame is meant to be soft, so a
//! global score calls portraits blurry; the sharpest tiles are where the
//! plane of focus is.  Tiles with blown highlights (lights, glints, bokeh
//! discs) are set aside for that: a clipped disc has steep edges however
//! far out of focus it is, and on the quality plan's shoot A they made frames shot
//! inside the lens's minimum focus distance look as sharp as good night
//! shots.  [`Features::local`] averages the three sharpest remaining tiles.
//!
//! Techniques are the standard published ones (variance of the Laplacian,
//! Pech-Pacheco et al. 2000; dHash and pHash, Hacker Factor 2011 / Zauner
//! 2010; Shannon entropy), written here; cleanvision was the inspiration.

use std::path::Path;

use super::decode::{self, Pixels};
use super::libraw::{Raw, Thumbnail};
use crate::exif;
use crate::model::record::FileType;

/// The measurements' version.  Bump it when any of them, or the plane they
/// are taken on, changes: cached results of another version are measured
/// again rather than misread.
pub const ALGO: u32 = 1;

/// Tiles across the long and short side of the plane.
pub const GRID_LONG: usize = 12;
pub const GRID_SHORT: usize = 8;
pub const TILES: usize = GRID_LONG * GRID_SHORT;

/// Luma at or above this is a blown highlight (a light, a specular glint,
/// a bokeh disc): its edges are steep whether or not it is in focus.
pub const CLIPPED: u8 = 250;
/// A tile with more blown highlights than this (in 255ths, ~0.8 %) doesn't
/// count towards [`Features::local`].  Catchlights in an eye are far less.
pub const CLIPPED_TILE: u8 = 2;

/// The AF-point window's side, as a fraction of the plane's long edge.
const FOCUS_WINDOW: f32 = 0.125;

/// Where the analysed pixels came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// A JPEG's MPF preview (the camera's ~1616-px rendering).
    Mpf = 1,
    /// A RAW's embedded JPEG preview.
    RawPreview = 2,
    /// A JPEG's own image, decoded at reduced size (no preview inside).
    Primary = 3,
}

impl Source {
    pub fn from_u8(v: u8) -> Option<Source> {
        match v {
            1 => Some(Source::Mpf),
            2 => Some(Source::RawPreview),
            3 => Some(Source::Primary),
            _ => None,
        }
    }
}

/// A luma plane, row-major, one byte per pixel.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub width: usize,
    pub height: usize,
    pub data: &'a [u8],
}

impl<'a> Plane<'a> {
    pub fn new(width: usize, height: usize, data: &'a [u8]) -> Plane<'a> {
        assert_eq!(data.len(), width * height, "plane size");
        Plane { width, height, data }
    }

    pub fn of(px: &'a Pixels) -> Plane<'a> {
        assert_eq!(px.channels, 1, "a luma plane");
        Plane::new(px.width as usize, px.height as usize, &px.data)
    }
}

/// What one shot's pixels say.  Sharpness values are mean squared 4-neighbour
/// Laplacians in 8-bit luma units, on the ~1616-px plane.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    pub source: Source,
    /// The analysed plane, as stored (sensor orientation).
    pub width: u16,
    pub height: u16,
    /// EXIF orientation of the photo (the plane is turned by it to show).
    pub orientation: u8,
    /// Per tile, in stored orientation, row-major: [`GRID_LONG`] across a
    /// landscape plane, [`GRID_SHORT`] across a portrait one.  Log-scaled,
    /// see [`tile_value`].
    pub tiles: [u16; TILES],
    /// Per tile, the share of pixels at or next to a blown highlight
    /// ([`CLIPPED`]) in 255ths, rounded up (any at all is at least 1).
    /// Those pixels add nothing to the tile's sharpness.
    pub clipped: [u8; TILES],
    /// Over the whole frame.
    pub global: f32,
    /// Around the camera's AF point, when the camera said where that was:
    /// the sharpest quarter of a window an eighth of the long edge wide.
    pub focus: Option<f32>,
    /// Shannon entropy of the luma histogram, 0–8 bits.
    pub entropy: f32,
    /// Luma percentiles: 1st, 5th, 50th, 95th and 99th.
    pub levels: [u8; 5],
    pub mean: f32,
    /// 64-bit difference and DCT hashes, as the photo is shown.
    pub dhash: u64,
    pub phash: u64,
}

/// A tile's sharpness from its stored log-scaled value.
pub fn tile_value(v: u16) -> f32 {
    (v as f32 / TILE_SCALE).exp2() - 1.0
}

fn tile_code(e: f64) -> u16 {
    ((1.0 + e).log2() * TILE_SCALE as f64).round().clamp(0.0, u16::MAX as f64) as u16
}

/// log2(1 + e) × this fits the largest possible mean (1020²) in a u16.
const TILE_SCALE: f32 = 3000.0;

impl Features {
    /// The tile grid's (columns, rows) as stored.
    pub fn grid(&self) -> (usize, usize) {
        grid_for(self.width as usize, self.height as usize)
    }

    pub fn p1(&self) -> u8 {
        self.levels[0]
    }
    pub fn p50(&self) -> u8 {
        self.levels[2]
    }
    pub fn p99(&self) -> u8 {
        self.levels[4]
    }

    /// The sharpest tile, highlights and all.
    pub fn tile_max(&self) -> f32 {
        self.tiles.iter().max().map_or(0.0, |&t| tile_value(t))
    }

    /// The sharpest area: the mean of the three sharpest tiles without
    /// blown highlights (0 when every tile has them).
    pub fn local(&self) -> f32 {
        let mut v: Vec<u16> = (0..TILES).filter(|&i| self.clipped[i] <= CLIPPED_TILE).map(|i| self.tiles[i]).collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_unstable_by(|a, b| b.cmp(a));
        let n = v.len().min(3);
        v[..n].iter().map(|&t| tile_value(t)).sum::<f32>() / n as f32
    }

    /// What frames of one burst are ranked by: the geometric mean of the
    /// whole frame and the sharpest area.  Camera shake softens all of a
    /// frame and missed focus the subject's area; either sinks it.  On 74
    /// bursts where the user kept some frames and not others, this put a
    /// kept frame first in 62 % (chance: 29 %), and in the top two in 81 %.
    pub fn score(&self) -> f32 {
        (self.global.max(0.01) * self.local().max(0.01)).sqrt()
    }

    /// The sharpest unclipped tile's centre, as fractions of the photo as
    /// shown (where to look to judge focus).
    pub fn sharpest_spot(&self) -> Option<(f32, f32)> {
        let (cols, rows) = self.grid();
        let i = (0..TILES).filter(|&i| self.clipped[i] <= CLIPPED_TILE).max_by_key(|&i| self.tiles[i])?;
        let (u, v) = (((i % cols) as f32 + 0.5) / cols as f32, ((i / cols) as f32 + 0.5) / rows as f32);
        Some(exif::orient_point(u, v, self.orientation as u16))
    }
}

fn grid_for(w: usize, h: usize) -> (usize, usize) {
    if w >= h { (GRID_LONG, GRID_SHORT) } else { (GRID_SHORT, GRID_LONG) }
}

/// Measure a plane.  `focus` is the AF point as fractions of the photo as
/// shown (`ShotMeta::focus_point`).
pub fn measure(p: Plane<'_>, orientation: u8, focus: Option<(f32, f32)>, source: Source) -> Features {
    let (cols, rows) = grid_for(p.width, p.height);
    let lap = laplacian_tiles(p, cols, rows);
    let mut tiles = [0u16; TILES];
    let mut clipped = [0u8; TILES];
    for i in 0..TILES {
        let n = lap.counts[i].max(1) as f64;
        tiles[i] = tile_code(lap.sums[i] as f64 / n);
        clipped[i] = (lap.clipped[i] as f64 * 255.0 / n).ceil().min(255.0) as u8;
    }
    let global = if lap.count > 0 { (lap.total as f64 / lap.count as f64) as f32 } else { 0.0 };
    let focus = focus.map(|(x, y)| {
        let (sx, sy) = to_stored(x, y, orientation);
        focus_window(p, sx, sy)
    });
    let pass = scan_pass(p, orientation);
    let (entropy, levels, mean) = histogram_stats(&pass.hist);
    Features {
        source,
        width: p.width.min(u16::MAX as usize) as u16,
        height: p.height.min(u16::MAX as usize) as u16,
        orientation,
        tiles,
        clipped,
        global,
        focus,
        entropy,
        levels,
        mean,
        dhash: dhash(&pass.grid9x8),
        phash: phash(&pass.grid32),
    }
}

// ── sharpness ───────────────────────────────────────────────────────────────

struct LapTiles {
    sums: [u64; TILES],
    clipped: [u32; TILES],
    counts: [u32; TILES],
    total: u64,
    count: u64,
}

/// Sum of squared 4-neighbour Laplacians per tile, over interior pixels,
/// and how many of each tile's pixels touch blown highlights.
fn laplacian_tiles(p: Plane<'_>, cols: usize, rows: usize) -> LapTiles {
    let mut out = LapTiles { sums: [0; TILES], clipped: [0; TILES], counts: [0; TILES], total: 0, count: 0 };
    let (w, h) = (p.width, p.height);
    if w < 3 || h < 3 {
        return out;
    }
    let xs: Vec<usize> = (0..=cols).map(|c| (c * w / cols).clamp(1, w - 1)).collect();
    for y in 1..h - 1 {
        let ty = (y * rows / h).min(rows - 1);
        let up = &p.data[(y - 1) * w..y * w];
        let mid = &p.data[y * w..(y + 1) * w];
        let down = &p.data[(y + 1) * w..(y + 2) * w];
        for c in 0..cols {
            let (x0, x1) = (xs[c], xs[c + 1]);
            if x1 <= x0 {
                continue;
            }
            let (s, lit) = row_energy(up, mid, down, x0, x1);
            let t = ty * cols + c;
            out.sums[t] += s;
            out.clipped[t] += lit;
            out.counts[t] += (x1 - x0) as u32;
            out.total += s;
            out.count += (x1 - x0) as u64;
        }
    }
    out
}

/// (Σ (4c − l − r − u − d)², pixels whose stencil touches a blown
/// highlight) over `x0..x1` of one row (1 ≤ x0, x1 ≤ w − 1).  Those pixels
/// add nothing to the sum: the edge of a light is steep however far out of
/// focus it is.
#[inline]
fn row_energy(up: &[u8], mid: &[u8], down: &[u8], x0: usize, x1: usize) -> (u64, u32) {
    let c = &mid[x0..x1];
    let l = &mid[x0 - 1..x1 - 1];
    let r = &mid[x0 + 1..x1 + 1];
    let u = &up[x0..x1];
    let d = &down[x0..x1];
    // Each term is at most 1020², so a u32 holds a run of 4 000 of them;
    // segments are a tile wide.
    debug_assert!(x1 - x0 <= 4000);
    let (mut s, mut n): (u32, u32) = (0, 0);
    for ((((&c, &l), &r), &u), &d) in c.iter().zip(l).zip(r).zip(u).zip(d) {
        let lit = c.max(l).max(r).max(u).max(d) >= CLIPPED;
        let v = 4 * c as i32 - l as i32 - r as i32 - u as i32 - d as i32;
        s = s.wrapping_add(if lit { 0 } else { (v * v) as u32 });
        n += lit as u32;
    }
    (s as u64, n)
}

/// The AF point (fractions of the photo as shown) in the stored plane.
pub fn to_stored(x: f32, y: f32, orientation: u8) -> (f32, f32) {
    // orient_point maps stored → shown; 6 and 8 are each other's inverse,
    // every other orientation is its own.
    let inverse = match orientation {
        6 => 8,
        8 => 6,
        o => o,
    };
    exif::orient_point(x, y, inverse as u16)
}

/// The sharpest quarter (2×2 blocks) of the window around (`x`, `y`)
/// (fractions of the stored plane).
fn focus_window(p: Plane<'_>, x: f32, y: f32) -> f32 {
    let (w, h) = (p.width, p.height);
    if w < 8 || h < 8 {
        return 0.0;
    }
    let side = ((w.max(h) as f32 * FOCUS_WINDOW) as usize).clamp(4, w.min(h) - 2) & !1;
    let cx = (x.clamp(0.0, 1.0) * w as f32) as usize;
    let cy = (y.clamp(0.0, 1.0) * h as f32) as usize;
    let x0 = cx.saturating_sub(side / 2).clamp(1, w - 1 - side);
    let y0 = cy.saturating_sub(side / 2).clamp(1, h - 1 - side);
    let half = side / 2;
    let mut best = 0.0f32;
    for (bx, by) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (xa, ya) = (x0 + bx * half, y0 + by * half);
        let mut s = 0u64;
        for yy in ya..ya + half {
            s += row_energy(&p.data[(yy - 1) * w..yy * w], &p.data[yy * w..(yy + 1) * w], &p.data[(yy + 1) * w..(yy + 2) * w], xa, xa + half).0;
        }
        best = best.max((s as f64 / (half * half) as f64) as f32);
    }
    best
}

// ── exposure and hashes (one pass) ──────────────────────────────────────────

struct Pass {
    hist: [u32; 256],
    /// Block means as the photo is shown: 32×32 for pHash, 9 wide × 8
    /// tall for dHash.
    grid32: [f32; 1024],
    grid9x8: [f32; 72],
}

/// Block means of the plane on a grid of `gw`×`gh` cells laid over the
/// photo *as shown*: each stored pixel lands in the cell where
/// `orientation` puts it.  For orientations 1–4 a pixel's shown column
/// depends only on its stored column; for 5–8 on its stored row; so each
/// row is summed in runs along one axis and added in at the other.
struct Binner {
    /// Runs of stored columns falling in one bin: (bin, first, end).
    runs: Vec<(usize, usize, usize)>,
    /// Per stored row: its bin along that axis.
    row_bin: Vec<u16>,
    /// Each x bin's and y bin's cell stride.
    x_stride: usize,
    y_stride: usize,
    sums: Vec<u64>,
    counts: Vec<u32>,
    cells: usize,
}

impl Binner {
    fn new(w: usize, h: usize, gw: usize, gh: usize, orientation: u8) -> Binner {
        let swap = orientation >= 5;
        let shown = |u: f32, v: f32| exif::orient_point(u, v, orientation as u16);
        let cell = |f: f32, n: usize| ((f * n as f32) as usize).min(n - 1);
        // Shown cells: column c, row r at index r * gw + c.
        let (x_stride, y_stride) = if swap { (gw, 1) } else { (1, gw) };
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        for x in 0..w {
            let (sx, sy) = shown((x as f32 + 0.5) / w as f32, 0.5);
            let b = if swap { cell(sy, gh) } else { cell(sx, gw) };
            match runs.last_mut() {
                Some(r) if r.0 == b => r.2 = x + 1,
                _ => runs.push((b, x, x + 1)),
            }
        }
        let row_bin = (0..h)
            .map(|y| {
                let (sx, sy) = shown(0.5, (y as f32 + 0.5) / h as f32);
                (if swap { cell(sx, gw) } else { cell(sy, gh) }) as u16
            })
            .collect();
        Binner { runs, row_bin, x_stride, y_stride, sums: vec![0; gw * gh], counts: vec![0; gw * gh], cells: gw * gh }
    }

    fn add_row(&mut self, y: usize, data: &[u8]) {
        let base = self.row_bin[y] as usize * self.y_stride;
        for &(b, x0, x1) in &self.runs {
            let i = base + b * self.x_stride;
            self.sums[i] += data[x0..x1].iter().map(|&v| v as u32).sum::<u32>() as u64;
            self.counts[i] += (x1 - x0) as u32;
        }
    }

    fn means<const N: usize>(&self) -> [f32; N] {
        debug_assert_eq!(N, self.cells);
        let mut out = [0f32; N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = if self.counts[i] > 0 { self.sums[i] as f32 / self.counts[i] as f32 } else { 0.0 };
        }
        out
    }
}

/// One pass over the plane: block means for the hashes, and the histogram
/// (of every other row, plenty for percentiles).
fn scan_pass(p: Plane<'_>, orientation: u8) -> Pass {
    let (w, h) = (p.width, p.height);
    let mut hist = [0u32; 256];
    let mut b32 = Binner::new(w, h, 32, 32, orientation);
    let mut b98 = Binner::new(w, h, 9, 8, orientation);
    let every = if h >= 64 { 2 } else { 1 };
    for y in 0..h {
        let row = &p.data[y * w..(y + 1) * w];
        if y % every == 0 {
            for &v in row {
                hist[v as usize] += 1;
            }
        }
        b32.add_row(y, row);
        b98.add_row(y, row);
    }
    Pass { hist, grid32: b32.means(), grid9x8: b98.means() }
}

/// (entropy in bits, percentiles 1/5/50/95/99, mean) of a histogram.
fn histogram_stats(hist: &[u32; 256]) -> (f32, [u8; 5], f32) {
    let total: u64 = hist.iter().map(|&c| c as u64).sum();
    if total == 0 {
        return (0.0, [0; 5], 0.0);
    }
    let mut entropy = 0.0f64;
    let mut sum = 0u64;
    for (v, &c) in hist.iter().enumerate() {
        if c > 0 {
            let p = c as f64 / total as f64;
            entropy -= p * p.log2();
            sum += v as u64 * c as u64;
        }
    }
    let mut levels = [0u8; 5];
    for (slot, q) in levels.iter_mut().zip([0.01, 0.05, 0.5, 0.95, 0.99]) {
        let target = (q * total as f64).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (v, &c) in hist.iter().enumerate() {
            acc += c as u64;
            if acc >= target {
                *slot = v as u8;
                break;
            }
        }
    }
    (entropy as f32, levels, (sum as f64 / total as f64) as f32)
}

/// dHash: each of 8 rows of 9 block means, one bit per neighbour pair
/// (set where the left is brighter).
fn dhash(g: &[f32; 72]) -> u64 {
    let mut h = 0u64;
    for r in 0..8 {
        for c in 0..8 {
            h = (h << 1) | (g[r * 9 + c] > g[r * 9 + c + 1]) as u64;
        }
    }
    h
}

/// pHash: the 2-D DCT-II of the 32×32 block means; the 8×8 lowest
/// frequencies (DC included), one bit each, set above their median.
fn phash(g: &[f32; 1024]) -> u64 {
    // cos((2x + 1) u π / 64) for u < 8.
    let mut basis = [[0f32; 32]; 8];
    for (u, row) in basis.iter_mut().enumerate() {
        for (x, b) in row.iter_mut().enumerate() {
            *b = ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 64.0).cos();
        }
    }
    // Rows first: 32 rows × 8 frequencies, then columns.
    let mut rows = [[0f32; 8]; 32];
    for y in 0..32 {
        for u in 0..8 {
            rows[y][u] = (0..32).map(|x| g[y * 32 + x] * basis[u][x]).sum();
        }
    }
    let mut coeffs = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            coeffs[v * 8 + u] = (0..32).map(|y| rows[y][u] * basis[v][y]).sum();
        }
    }
    let mut sorted = coeffs;
    sorted.sort_unstable_by(|a, b| a.total_cmp(b));
    let median = (sorted[31] + sorted[32]) / 2.0;
    coeffs.iter().fold(0u64, |h, &c| (h << 1) | (c > median) as u64)
}

/// Bits that differ between two hashes.
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ── reading (the only I/O here) ─────────────────────────────────────────────

/// A JPEG's own image is decoded at the largest DCT reduction that keeps
/// its long edge at least this, to compare with the ~1616-px previews.
const PRIMARY_MIN_EDGE: u32 = 1200;
/// Smaller embedded previews are not worth measuring.
const PREVIEW_MIN_EDGE: u32 = 1000;

/// A shot's analysed plane: luma, as stored, with the photo's orientation.
pub struct Loaded {
    pub plane: Pixels,
    pub orientation: u8,
    pub source: Source,
}

/// Read and decode the plane to measure for `path`: a JPEG's MPF preview
/// (else its own image at reduced size), a RAW's smallest embedded JPEG of
/// at least 1000 px.  Never a full-resolution decode, and never a RAW
/// demosaic: a RAW with no usable preview is not measured.
pub fn load(path: &Path, file_type: FileType) -> Result<Loaded, String> {
    match file_type {
        FileType::Jpg => {
            if let Some((data, head)) = exif::jpeg_preview_bytes(path, PREVIEW_MIN_EDGE) {
                if decode::jpeg_dimensions(&data).is_some_and(|(w, h)| w.max(h) >= PREVIEW_MIN_EDGE) {
                    let plane = decode::decode_jpeg_luma(&data, 1)?;
                    // The preview is stored in sensor orientation; the
                    // primary image's tag says how the photo is shown.
                    return Ok(Loaded { plane, orientation: exif::orientation_from_jpeg_head(&head), source: Source::Mpf });
                }
            }
            let data = std::fs::read(path).map_err(|e| e.to_string())?;
            let (w, h) = decode::jpeg_dimensions(&data).ok_or("not a readable JPEG")?;
            let plane = decode::decode_jpeg_luma(&data, reduction(w.max(h)))?;
            Ok(Loaded { plane, orientation: exif::orientation_from_jpeg_head(&data), source: Source::Primary })
        }
        FileType::Raw => {
            let mut raw = Raw::open(path)?;
            let flip = raw.exif_orientation();
            match raw.thumbnail_at_least(PREVIEW_MIN_EDGE)? {
                Thumbnail::Jpeg(bytes) => {
                    let (w, h) = decode::jpeg_dimensions(&bytes).ok_or("unreadable RAW preview")?;
                    let plane = decode::decode_jpeg_luma(&bytes, reduction(w.max(h)))?;
                    // As for the thumbnail: the preview's own tag, else the
                    // RAW's orientation.
                    let orientation = super::thumbs::embedded_orientation(&bytes, flip);
                    Ok(Loaded { plane, orientation, source: Source::RawPreview })
                }
                Thumbnail::Bitmap { .. } => Err("RAW preview is not a JPEG".into()),
            }
        }
    }
}

/// The DCT reduction (1, 2, 4 or 8) that keeps `long_edge` at least
/// [`PRIMARY_MIN_EDGE`] (1 when it is already smaller).
fn reduction(long_edge: u32) -> u32 {
    [8, 4, 2].into_iter().find(|&d| long_edge / d >= PRIMARY_MIN_EDGE).unwrap_or(1)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A deterministic pseudo-random generator for test images.
    pub struct Rng(pub u32);
    impl Rng {
        pub fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
        /// Roughly normal, mean 0, sd 1.
        pub fn gauss(&mut self) -> f32 {
            (0..12).map(|_| (self.next() % 10_000) as f32 / 10_000.0).sum::<f32>() - 6.0
        }
    }

    pub struct Img {
        pub w: usize,
        pub h: usize,
        pub d: Vec<u8>,
    }

    impl Img {
        pub fn plane(&self) -> Plane<'_> {
            Plane::new(self.w, self.h, &self.d)
        }
        pub fn from_fn(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Img {
            let mut d = Vec::with_capacity(w * h);
            for y in 0..h {
                for x in 0..w {
                    d.push(f(x, y).round().clamp(0.0, 255.0) as u8);
                }
            }
            Img { w, h, d }
        }
        /// Box blur of radius `r` (separable, edges clamped).
        pub fn blur(&self, r: usize) -> Img {
            if r == 0 {
                return Img { w: self.w, h: self.h, d: self.d.clone() };
            }
            let (w, h) = (self.w, self.h);
            let mut tmp = vec![0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
                    tmp[y * w + x] = (a..=b).map(|i| self.d[y * w + i] as f32).sum::<f32>() / (b - a + 1) as f32;
                }
            }
            let mut d = vec![0u8; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
                    d[y * w + x] = ((a..=b).map(|i| tmp[i * w + x]).sum::<f32>() / (b - a + 1) as f32).round() as u8;
                }
            }
            Img { w, h, d }
        }
        /// An underexposed copy: every level scaled by `k`.
        pub fn darker(&self, k: f32) -> Img {
            Img { w: self.w, h: self.h, d: self.d.iter().map(|&v| (v as f32 * k).round() as u8).collect() }
        }
        pub fn noisy(&self, sigma: f32, seed: u32) -> Img {
            let mut rng = Rng(seed);
            Img { w: self.w, h: self.h, d: self.d.iter().map(|&v| (v as f32 + sigma * rng.gauss()).round().clamp(0.0, 255.0) as u8).collect() }
        }
        /// Copy `src`'s pixels inside the rectangle onto this image.
        pub fn paste(&mut self, src: &Img, x0: usize, y0: usize, x1: usize, y1: usize) {
            for y in y0..y1 {
                for x in x0..x1 {
                    self.d[y * self.w + x] = src.d[y * self.w + x];
                }
            }
        }
    }

    /// Detailed texture: random-sized blocks of random grey, plus fine
    /// lines, so every tile holds edges.
    pub fn texture(w: usize, h: usize, seed: u32) -> Img {
        let mut rng = Rng(seed);
        let cells: Vec<f32> = (0..64 * 64).map(|_| (rng.next() % 200) as f32 + 28.0).collect();
        Img::from_fn(w, h, |x, y| {
            let base = cells[(y / 7 % 64) * 64 + (x / 5 % 64)];
            let line = if (x + 2 * y) % 13 == 0 { -40.0 } else { 0.0 };
            base + line
        })
    }

    pub fn feats(img: &Img) -> Features {
        measure(img.plane(), 1, None, Source::Mpf)
    }

    #[test]
    fn sharpness_falls_with_blur() {
        let sharp = texture(240, 160, 7);
        let mut last = (f32::INFINITY, f32::INFINITY);
        for r in [0, 1, 2, 4, 8] {
            let f = feats(&sharp.blur(r));
            assert!(f.tile_max() < last.0 && f.score() < last.1, "radius {r}");
            assert!(f.global <= f.tile_max() && f.local() <= f.tile_max());
            last = (f.tile_max(), f.score());
        }
        // A checkerboard is sharp; its blurred copy is far softer.
        let check = Img::from_fn(240, 160, |x, y| if (x / 4 + y / 4) % 2 == 0 { 40.0 } else { 210.0 });
        assert!(feats(&check).local() > 20.0 * feats(&check.blur(3)).local());
    }

    #[test]
    fn shallow_depth_of_field_is_still_sharp() {
        // A blurred frame with one sharp subject region: the global score
        // sinks, the sharpest area doesn't.
        let sharp = texture(240, 160, 11);
        let mut dof = sharp.blur(6);
        dof.paste(&sharp, 100, 60, 140, 100);
        let (all, soft, f) = (feats(&sharp), feats(&sharp.blur(6)), feats(&dof));
        assert!(f.global < all.global * 0.25, "global {} vs {}", f.global, all.global);
        assert!(f.tile_max() > all.tile_max() * 0.5);
        assert!(f.local() > soft.local() * 5.0, "local {} vs {}", f.local(), soft.local());
        // The ranking score sits between the two.
        assert!(f.score() > soft.score() * 2.0 && f.score() < all.score());
    }

    #[test]
    fn blown_highlights_dont_count_as_sharp() {
        // A soft frame with a few blown discs (lights, bokeh): their edges
        // are steep, but the sharpest area stays soft.
        let soft = texture(240, 160, 4).blur(5).darker(0.6);
        let mut lit = Img { w: soft.w, h: soft.h, d: soft.d.clone() };
        for (cx, cy) in [(30.0f32, 30.0f32), (200.0, 40.0), (120.0, 130.0)] {
            lit = Img::from_fn(240, 160, |x, y| {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if d < 9.0 { 255.0 } else { lit.d[y * 240 + x] as f32 }
            });
        }
        let (a, b) = (feats(&soft), feats(&lit));
        assert!(b.tile_max() < 1.5 * a.tile_max(), "tile max {} vs {}", b.tile_max(), a.tile_max());
        assert!(b.local() < 1.5 * a.local(), "local {} vs {}", b.local(), a.local());
        assert!(b.global < 1.5 * a.global, "global {} vs {}", b.global, a.global);
        assert!(b.clipped.iter().filter(|&&c| c > CLIPPED_TILE).count() >= 3);
        // A sharp frame with the same lights is still sharp.
        let sharp = texture(240, 160, 4).darker(0.6);
        assert!(feats(&sharp).local() > 5.0 * b.local());
        // Nothing but a highlight's edge: no sharp area at all.
        let white = Img::from_fn(64, 48, |x, _| if x < 32 { 255.0 } else { 0.0 });
        assert_eq!((feats(&white).local(), feats(&white).global), (0.0, 0.0));
    }

    #[test]
    fn the_focus_window_follows_the_af_point_through_orientation() {
        let sharp = texture(240, 160, 3);
        let mut img = sharp.blur(6);
        // Sharp patch near the stored plane's top-left.
        img.paste(&sharp, 20, 10, 60, 50);
        let at = |x, y, o| measure(img.plane(), o, Some((x, y)), Source::Mpf).focus.unwrap();
        let soft = at(0.75, 0.75, 1);
        assert!(at(40.0 / 240.0, 30.0 / 160.0, 1) > 10.0 * soft);
        // Shown with orientation 6 (rotated 90° clockwise) the stored
        // top-left is the shown top-right.
        let (sx, sy) = exif::orient_point(40.0 / 240.0, 30.0 / 160.0, 6);
        assert!(sx > 0.5 && sy < 0.5);
        assert!(at(sx, sy, 6) > 10.0 * soft);
        assert!(at(sx, sy, 1) < 2.0 * soft);
        for o in 1..=8u8 {
            let (x, y) = exif::orient_point(0.2, 0.7, o as u16);
            let (bx, by) = to_stored(x, y, o);
            assert!((bx - 0.2).abs() < 1e-6 && (by - 0.7).abs() < 1e-6, "orientation {o}");
        }
        // The sharpest spot is reported as shown.
        let f = measure(img.plane(), 6, None, Source::Mpf);
        let (x, y) = f.sharpest_spot().unwrap();
        assert!(x > 0.5 && y < 0.5, "{x} {y}");
    }

    #[test]
    fn exposure_and_entropy() {
        let flat = Img::from_fn(64, 48, |_, _| 128.0);
        let f = feats(&flat);
        assert_eq!(f.entropy, 0.0);
        assert_eq!(f.levels, [128; 5]);
        assert_eq!((f.tile_max(), f.global), (0.0, 0.0));
        let two = Img::from_fn(64, 48, |x, _| if x < 32 { 10.0 } else { 240.0 });
        let f = feats(&two);
        assert!((f.entropy - 1.0).abs() < 1e-6);
        assert_eq!((f.p1(), f.p50(), f.p99()), (10, 10, 240));
        assert!((f.mean - 125.0).abs() < 1e-3);
        let black = Img::from_fn(64, 48, |x, y| ((x * 7 + y * 3) % 5) as f32);
        let f = feats(&black);
        assert!(f.p99() <= 4 && f.entropy < 2.5);
    }

    #[test]
    fn hashes_are_stable_and_tell_scenes_apart() {
        let a = texture(320, 214, 21);
        let fa = feats(&a);
        // Small changes: noise, slight blur, darker exposure.
        for v in [a.noisy(4.0, 1), a.blur(1), a.darker(0.7)] {
            let f = feats(&v);
            assert!(distance(fa.phash, f.phash) <= 6, "phash {}", distance(fa.phash, f.phash));
            assert!(distance(fa.dhash, f.dhash) <= 10, "dhash {}", distance(fa.dhash, f.dhash));
        }
        // Another scene.
        let fb = feats(&texture(320, 214, 22));
        assert!(distance(fa.phash, fb.phash) >= 20, "{}", distance(fa.phash, fb.phash));
        // A left-to-right gradient: no block is brighter than its right
        // neighbour.  (Its pHash is noise-led: almost every coefficient but
        // the lowest is near zero, so flat frames can't be told apart by it.)
        let g = Img::from_fn(320, 214, |x, y| (x as f32 * 0.6 + y as f32 * 0.2).min(255.0));
        assert_eq!(feats(&g).dhash, 0);
        assert_eq!(distance(0b1011, 0b0110), 3);
    }

    #[test]
    fn hashes_are_taken_as_the_photo_is_shown() {
        // A photo stored sideways and the same photo saved upright hash
        // alike, for every orientation.
        let upright = texture(214, 320, 5);
        let a = measure(upright.plane(), 1, None, Source::Mpf);
        for o in 2..=8u8 {
            // Stored so that `o` shows it upright: stored pixel (x, y) shows
            // at orient_point((x, y)), so sample the upright image there.
            let swap = o >= 5;
            let (w, h) = if swap { (upright.h, upright.w) } else { (upright.w, upright.h) };
            let stored = Img::from_fn(w, h, |x, y| {
                let (u, v) = exif::orient_point((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32, o as u16);
                let (ux, uy) = ((u * upright.w as f32) as usize, (v * upright.h as f32) as usize);
                upright.d[uy.min(upright.h - 1) * upright.w + ux.min(upright.w - 1)] as f32
            });
            let b = measure(stored.plane(), o, None, Source::Mpf);
            assert!(distance(a.phash, b.phash) <= 2, "orientation {o}: {}", distance(a.phash, b.phash));
            assert!(distance(a.dhash, b.dhash) <= 4, "orientation {o}: {}", distance(a.dhash, b.dhash));
            // Read as if not turned, they differ.
            let c = measure(stored.plane(), 1, None, Source::Mpf);
            assert!(distance(a.phash, c.phash) > 10, "orientation {o}");
        }
    }

    #[test]
    fn tile_values_round_trip() {
        for e in [0.0f64, 0.5, 3.0, 20.0, 1234.5, 1_040_400.0] {
            let back = tile_value(tile_code(e)) as f64;
            assert!((back - e).abs() <= e * 0.001 + 0.001, "{e} -> {back}");
        }
        let f = feats(&texture(240, 160, 1));
        assert_eq!(f.grid(), (12, 8));
        let tall = measure(Img::from_fn(100, 150, |x, _| x as f32).plane(), 1, None, Source::Mpf);
        assert_eq!(tall.grid(), (8, 12));
    }

    #[test]
    fn tiny_planes_measure_without_panicking() {
        for (w, h) in [(1, 1), (2, 5), (3, 3), (9, 2), (16, 16)] {
            let img = Img::from_fn(w, h, |x, y| ((x * 31 + y * 17) % 256) as f32);
            for o in 1..=8 {
                let f = measure(img.plane(), o, Some((0.5, 0.5)), Source::Mpf);
                assert!(f.score().is_finite() && f.focus.is_some_and(f32::is_finite));
            }
        }
    }

    #[test]
    fn reductions_keep_the_long_edge_comparable() {
        assert_eq!(reduction(1616), 1);
        assert_eq!(reduction(2400), 2);
        assert_eq!(reduction(7008), 4);
        assert_eq!(reduction(6000), 4);
        assert_eq!(reduction(800), 1);
    }

    /// Real files from `PP_FIXTURE_DIR` (a small folder of camera files):
    /// a JPEG's MPF preview, and every RAW's preview.  Runs only when asked
    /// for, and then fails without a JPEG with an MPF preview or a RAW.
    #[test]
    #[ignore = "needs PP_FIXTURE_DIR (see README, Testing)"]
    fn loads_embedded_previews() {
        let path = crate::exif::fixture_mpf_jpeg();
        let jpg = load(&path, FileType::Jpg).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!((jpg.source, jpg.plane.channels), (Source::Mpf, 1), "{}", path.display());
        assert!(jpg.plane.width.max(jpg.plane.height) >= 1000, "{}", path.display());
        let raws = crate::util::fixture_files(FileType::Raw);
        assert!(!raws.is_empty(), "PP_FIXTURE_DIR has no RAW files: add one of each kind the app reads (see README, Testing)");
        for path in raws {
            let raw = path.display();
            let l = load(&path, FileType::Raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(l.source, Source::RawPreview);
            assert!(l.plane.width.max(l.plane.height) >= 1000, "{raw}");
            assert!(l.plane.width.max(l.plane.height) <= 2400, "{raw}: {}x{}", l.plane.width, l.plane.height);
        }
    }
}

