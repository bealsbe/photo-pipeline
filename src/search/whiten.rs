//! PCA whitening of the fursuit head vectors (fursuit matching v2,
//! `docs/library-search-plan.md` "As built — v2").
//!
//! The search model's image tower gives each head crop a 768-wide unit
//! vector.  Raw, those vectors spend most of their length on what every
//! fursuit photo shares (a costume, a crowd, the lights); whitened over
//! the library's own heads, the directions that tell one suit from
//! another weigh as much as the common ones.  The research
//! (`fursuit-id/HANDOFF.md`, 791 labelled pairs in four time tiers) found
//! this the best of everything it tried short of a vision language
//! model: AUC 0.97/0.97/0.96/0.92 per tier, against DINOv2's
//! 0.82/0.85/0.87/0.87.
//!
//! **The recipe, exactly as the research's `rerank.py whiten --k 256
//! --power 0.5`:** the unit vectors' mean μ and covariance C (over n,
//! not n − 1), C's eigen-decomposition, the top [`K`] eigenvectors each
//! scaled by `1 / (λ^0.25 + 1e-6)` as the rows of P; a head's whitened
//! vector is `unit((v − μ) Pᵀ)`, and distances are cosine distances
//! between those.  Unsupervised: nothing the user named is used, so a
//! refit never needs the user's answers, and names and answers survive
//! it (they point at detections, `FaceRef`s, never at rows).
//!
//! **It is maths, not a model:** fitted on the processor, on a worker
//! thread, in about a second at library scale; kept as cache beside the
//! index (0600), refitted when a head pass or the catch-up ends and when
//! the heads grow by a fifth (`super::heads::Fursuits`).

use std::io;
use std::path::Path;

use crate::model::binfile::{Reader, Writer};
use crate::model::subjects::{private_dir, private_file};
use crate::ops::fsutil;

/// Components kept.
pub const K: usize = 256;
/// Each component is scaled by `λ^(−POWER / 2)`: half whitening.
pub const POWER: f64 = 0.5;
/// Added to `λ^(POWER/2)` before dividing, as the research did.
const EPS: f64 = 1e-6;
/// Fewer heads than this aren't fitted: [`K`] components need several
/// times as many samples to be estimated at all.  Below it the fursuit
/// step has no rows to group (a library this small has a day or two of
/// fursuit photos at most).
pub const MIN_HEADS: usize = 2 * K;
/// A refit is due once the heads with vectors grow past this share of
/// those the fit saw (plan: "when the head count grows by more than
/// 20 %").
pub const REFIT_GROWTH: f64 = 1.2;

const MAGIC: &[u8; 8] = b"PPHWHIT\0";
const VERSION: u32 = 1;

/// A fitted whitening: the mean and the scaled projection.
#[derive(Debug, Clone, PartialEq)]
pub struct Whitening {
    /// The raw vectors' width.
    pub dim: usize,
    /// The whitened vectors' width (`min(K, dim)`).
    pub k: usize,
    /// Heads it was fitted on.
    pub fitted_on: usize,
    mean: Vec<f32>,
    /// `k × dim`, row-major: each row an eigenvector already divided by
    /// `λ^(POWER/2) + EPS`.
    proj: Vec<f32>,
}

impl Whitening {
    /// Fit on `data` (`n × dim`, row-major; each row is made unit length
    /// first, as the research did).  `None` when there are fewer than two
    /// rows or the width is zero.  Keeps `min(k, dim)` components.
    pub fn fit(data: &[f32], dim: usize, k: usize) -> Option<Whitening> {
        if dim == 0 || data.len() < 2 * dim || data.len() % dim != 0 {
            return None;
        }
        let n = data.len() / dim;
        let k = k.min(dim);
        let unit: Vec<f64> = data.chunks_exact(dim).flat_map(|r| {
            let norm = r.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt().max(1e-6);
            r.iter().map(move |&x| x as f64 / norm)
        }).collect();
        let mut mean = vec![0f64; dim];
        for r in unit.chunks_exact(dim) {
            for (m, x) in mean.iter_mut().zip(r) {
                *m += x;
            }
        }
        mean.iter_mut().for_each(|m| *m /= n as f64);
        let cov = covariance(&unit, &mean, dim);
        let (values, vectors) = symmetric_eigen(cov, dim);
        // Largest first.
        let mut order: Vec<usize> = (0..dim).collect();
        order.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
        let mut proj = Vec::with_capacity(k * dim);
        for &c in order.iter().take(k) {
            let scale = 1.0 / (values[c].max(0.0).powf(POWER / 2.0) + EPS);
            proj.extend((0..dim).map(|i| (vectors[i * dim + c] * scale) as f32));
        }
        Some(Whitening { dim, k, fitted_on: n, mean: mean.iter().map(|&m| m as f32).collect(), proj })
    }

    /// The identity on `dim`-wide vectors (each made unit length): for
    /// tests that group hand-made vectors whose distances they choose.
    #[cfg(test)]
    pub fn identity(dim: usize) -> Whitening {
        let mut proj = vec![0f32; dim * dim];
        for i in 0..dim {
            proj[i * dim + i] = 1.0;
        }
        Whitening { dim, k: dim, fitted_on: 0, mean: vec![0.0; dim], proj }
    }

    /// A raw vector whitened: `unit((v − μ) Pᵀ)`, [`Whitening::k`] wide.
    /// A vector of another width gives an empty one.
    pub fn apply(&self, v: &[f32]) -> Vec<f32> {
        if v.len() != self.dim {
            return Vec::new();
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        let centred: Vec<f32> = v.iter().zip(&self.mean).map(|(x, m)| x / norm - m).collect();
        let mut z: Vec<f32> = self.proj.chunks_exact(self.dim).map(|row| row.iter().zip(&centred).map(|(p, c)| p * c).sum()).collect();
        let zn = z.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        z.iter_mut().for_each(|x| *x /= zn);
        z
    }

    /// Whiten `n × dim` rows into `n × k`, in parallel.
    pub fn apply_rows(&self, data: &[f32]) -> Vec<f32> {
        use rayon::prelude::*;
        if self.dim == 0 {
            return Vec::new();
        }
        data.par_chunks(self.dim * 64).flat_map_iter(|block| block.chunks_exact(self.dim).flat_map(|r| self.apply(r)).collect::<Vec<f32>>()).collect()
    }

    /// The file's bytes: a sealed block with the model it was fitted for
    /// (`model`, `sha`: a vector of another model or crop is never
    /// whitened with it), then the numbers.
    fn to_bytes(&self, model: &str, sha: &str, crop_version: u32) -> Vec<u8> {
        let mut w = Writer(Vec::with_capacity(64 + 4 * (self.dim + self.k * self.dim)));
        w.0.extend_from_slice(MAGIC);
        w.u32(VERSION);
        w.bytes(model.as_bytes());
        w.bytes(sha.as_bytes());
        w.u32(crop_version);
        w.u32(self.dim as u32);
        w.u32(self.k as u32);
        w.u64(self.fitted_on as u64);
        for &x in self.mean.iter().chain(&self.proj) {
            w.u32(x.to_bits());
        }
        w.seal()
    }

    fn from_bytes(data: &[u8], model: &str, sha: &str, crop_version: u32) -> Option<Whitening> {
        let mut r = Reader::open(data)?;
        if r.take(8)? != MAGIC || r.u32()? != VERSION || r.bytes()? != model.as_bytes() || r.bytes()? != sha.as_bytes() || r.u32()? != crop_version {
            return None;
        }
        let dim = r.u32()? as usize;
        let k = r.u32()? as usize;
        let fitted_on = r.u64()? as usize;
        if dim == 0 || k == 0 || k > dim {
            return None;
        }
        let mut f = |n: usize| -> Option<Vec<f32>> { (0..n).map(|_| r.u32().map(f32::from_bits)).collect() };
        let mean = f(dim)?;
        let proj = f(k * dim)?;
        r.done().then_some(Whitening { dim, k, fitted_on, mean, proj })
    }

    /// Keep it in `path` (atomically, readable by the user alone).
    pub fn save(&self, path: &Path, model: &str, sha: &str, crop_version: u32) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            private_dir(dir);
        }
        fsutil::atomic_write(path, &self.to_bytes(model, sha, crop_version), true)?;
        private_file(path);
        Ok(())
    }

    /// The whitening kept in `path`, if it is one for this model and crop.
    pub fn load(path: &Path, model: &str, sha: &str, crop_version: u32) -> Option<Whitening> {
        Whitening::from_bytes(&std::fs::read(path).ok()?, model, sha, crop_version)
    }
}

/// `Σ (x − μ)(x − μ)ᵀ / n` over `n × dim` rows, in f64, in parallel.
fn covariance(rows: &[f64], mean: &[f64], dim: usize) -> Vec<f64> {
    use rayon::prelude::*;
    let n = rows.len() / dim;
    let upper = rows
        .par_chunks(dim * 256)
        .fold(
            || vec![0f64; dim * dim],
            |mut acc, block| {
                let mut x = vec![0f64; dim];
                for r in block.chunks_exact(dim) {
                    for ((xi, ri), mi) in x.iter_mut().zip(r).zip(mean) {
                        *xi = ri - mi;
                    }
                    for i in 0..dim {
                        let xi = x[i];
                        let row = &mut acc[i * dim + i..(i + 1) * dim];
                        for (a, xj) in row.iter_mut().zip(&x[i..]) {
                            *a += xi * xj;
                        }
                    }
                }
                acc
            },
        )
        .reduce(|| vec![0f64; dim * dim], |mut a, b| {
            a.iter_mut().zip(&b).for_each(|(x, y)| *x += y);
            a
        });
    let mut c = upper;
    for i in 0..dim {
        for j in i..dim {
            let v = c[i * dim + j] / n as f64;
            c[i * dim + j] = v;
            c[j * dim + i] = v;
        }
    }
    c
}

/// Eigenvalues and eigenvectors of a symmetric `n × n` matrix (row-major):
/// Householder reduction to tridiagonal form, then the implicit QL
/// method (EISPACK's tred2 and tql2, as JAMA ports them).  Returns the
/// eigenvalues ascending and the eigenvectors as the columns of an
/// `n × n` row-major matrix (`v[i * n + c]` is component `i` of vector
/// `c`).
pub fn symmetric_eigen(a: Vec<f64>, n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut v = a;
    let mut d = vec![0f64; n];
    let mut e = vec![0f64; n];
    if n == 0 {
        return (d, v);
    }
    let at = |v: &Vec<f64>, i: usize, j: usize| v[i * n + j];
    // tred2
    for j in 0..n {
        d[j] = at(&v, n - 1, j);
    }
    for i in (1..n).rev() {
        let mut scale = 0.0;
        let mut h = 0.0;
        for k in 0..i {
            scale += d[k].abs();
        }
        if scale == 0.0 {
            e[i] = d[i - 1];
            for j in 0..i {
                d[j] = at(&v, i - 1, j);
                v[i * n + j] = 0.0;
                v[j * n + i] = 0.0;
            }
        } else {
            for k in 0..i {
                d[k] /= scale;
                h += d[k] * d[k];
            }
            let mut f = d[i - 1];
            let mut g = h.sqrt();
            if f > 0.0 {
                g = -g;
            }
            e[i] = scale * g;
            h -= f * g;
            d[i - 1] = f - g;
            for j in 0..i {
                e[j] = 0.0;
            }
            for j in 0..i {
                f = d[j];
                v[j * n + i] = f;
                g = e[j] + at(&v, j, j) * f;
                for k in j + 1..i {
                    g += at(&v, k, j) * d[k];
                    e[k] += at(&v, k, j) * f;
                }
                e[j] = g;
            }
            f = 0.0;
            for j in 0..i {
                e[j] /= h;
                f += e[j] * d[j];
            }
            let hh = f / (h + h);
            for j in 0..i {
                e[j] -= hh * d[j];
            }
            for j in 0..i {
                f = d[j];
                g = e[j];
                for k in j..i {
                    v[k * n + j] -= f * e[k] + g * d[k];
                }
                d[j] = at(&v, i - 1, j);
                v[i * n + j] = 0.0;
            }
        }
        d[i] = h;
    }
    for i in 0..n - 1 {
        v[(n - 1) * n + i] = at(&v, i, i);
        v[i * n + i] = 1.0;
        let h = d[i + 1];
        if h != 0.0 {
            for k in 0..=i {
                d[k] = at(&v, k, i + 1) / h;
            }
            for j in 0..=i {
                let mut g = 0.0;
                for k in 0..=i {
                    g += at(&v, k, i + 1) * at(&v, k, j);
                }
                for k in 0..=i {
                    v[k * n + j] -= g * d[k];
                }
            }
        }
        for k in 0..=i {
            v[k * n + i + 1] = 0.0;
        }
    }
    for j in 0..n {
        d[j] = at(&v, n - 1, j);
        v[(n - 1) * n + j] = 0.0;
    }
    v[(n - 1) * n + n - 1] = 1.0;
    e[0] = 0.0;
    // tql2, on Vᵀ so that the rotations walk memory in order (each
    // rotation updates two eigenvectors: two rows of Vᵀ).
    let mut vt = vec![0f64; n * n];
    for i in 0..n {
        for j in 0..n {
            vt[j * n + i] = v[i * n + j];
        }
    }
    for i in 1..n {
        e[i - 1] = e[i];
    }
    e[n - 1] = 0.0;
    let mut f = 0.0f64;
    let mut tst1 = 0.0f64;
    let eps = f64::EPSILON;
    for l in 0..n {
        tst1 = tst1.max(d[l].abs() + e[l].abs());
        let mut m = l;
        while m < n {
            if e[m].abs() <= eps * tst1 {
                break;
            }
            m += 1;
        }
        let m = m.min(n - 1);
        if m > l {
            let mut iter = 0;
            loop {
                iter += 1;
                let mut g = d[l];
                let mut p = (d[l + 1] - g) / (2.0 * e[l]);
                let mut r = p.hypot(1.0);
                if p < 0.0 {
                    r = -r;
                }
                d[l] = e[l] / (p + r);
                d[l + 1] = e[l] * (p + r);
                let dl1 = d[l + 1];
                let mut h = g - d[l];
                for i in l + 2..n {
                    d[i] -= h;
                }
                f += h;
                p = d[m];
                let mut c = 1.0;
                let mut c2 = c;
                let mut c3 = c;
                let el1 = e[l + 1];
                let mut s = 0.0;
                let mut s2 = 0.0;
                for i in (l..m).rev() {
                    c3 = c2;
                    c2 = c;
                    s2 = s;
                    g = c * e[i];
                    h = c * p;
                    r = p.hypot(e[i]);
                    e[i + 1] = s * r;
                    s = e[i] / r;
                    c = p / r;
                    p = c * d[i] - s * g;
                    d[i + 1] = h + s * (c * g + s * d[i]);
                    let (lo, hi) = vt.split_at_mut((i + 1) * n);
                    let (a, b) = (&mut lo[i * n..], &mut hi[..n]);
                    for (vi, vi1) in a.iter_mut().zip(b.iter_mut()) {
                        let h = *vi1;
                        *vi1 = s * *vi + c * h;
                        *vi = c * *vi - s * h;
                    }
                }
                p = -s * s2 * c3 * el1 * e[l] / dl1;
                e[l] = s * p;
                d[l] = c * p;
                if e[l].abs() <= eps * tst1 || iter > 60 {
                    break;
                }
            }
        }
        d[l] += f;
        e[l] = 0.0;
    }
    // Ascending, vectors with them.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| d[a].total_cmp(&d[b]));
    let values: Vec<f64> = order.iter().map(|&i| d[i]).collect();
    let mut vectors = vec![0f64; n * n];
    for (c, &src) in order.iter().enumerate() {
        for i in 0..n {
            vectors[i * n + c] = vt[src * n + i];
        }
    }
    (values, vectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (no rand dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    #[test]
    fn the_eigen_solver_reconstructs_a_symmetric_matrix() {
        let n = 24;
        let mut g = Lcg(7);
        let b: Vec<f64> = (0..n * n).map(|_| g.next()).collect();
        // A = B Bᵀ: symmetric, positive semi-definite.
        let mut a = vec![0f64; n * n];
        for i in 0..n {
            for j in 0..n {
                a[i * n + j] = (0..n).map(|k| b[i * n + k] * b[j * n + k]).sum();
            }
        }
        let (values, v) = symmetric_eigen(a.clone(), n);
        assert!(values.windows(2).all(|w| w[0] <= w[1]), "ascending");
        assert!(values[0] > -1e-9, "positive semi-definite");
        for i in 0..n {
            for j in 0..n {
                // V Λ Vᵀ = A
                let r: f64 = (0..n).map(|c| v[i * n + c] * values[c] * v[j * n + c]).sum();
                assert!((r - a[i * n + j]).abs() < 1e-8 * values[n - 1], "A[{i},{j}]");
                // VᵀV = I
                let o: f64 = (0..n).map(|k| v[k * n + i] * v[k * n + j]).sum();
                assert!((o - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9, "orthonormal");
            }
        }
        // A diagonal matrix gives its own diagonal.
        let (vals, _) = symmetric_eigen(vec![3.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0], 3);
        assert!((vals[0] - 1.0).abs() < 1e-12 && (vals[1] - 2.0).abs() < 1e-12 && (vals[2] - 3.0).abs() < 1e-12);
    }

    /// The recipe: whitened vectors are unit length and, over the fitted
    /// rows, centred with every kept direction of equal spread up to the
    /// half power (variance ∝ λ^(1 − POWER) = √λ before the unit step).
    /// A direction every row shares stops dominating the distance.
    #[test]
    fn whitening_follows_the_research_recipe() {
        let (n, dim) = (600, 8);
        let mut g = Lcg(11);
        // Rows: a large common direction (axis 0) plus small, distinct
        // noise on the others.
        let mut data = Vec::with_capacity(n * dim);
        for _ in 0..n {
            let mut r = vec![0f32; dim];
            r[0] = 5.0 + g.next() as f32 * 0.5;
            for x in r.iter_mut().skip(1) {
                *x = g.next() as f32 * 0.3;
            }
            data.extend(r);
        }
        let w = Whitening::fit(&data, dim, 4).expect("fitted");
        assert_eq!((w.dim, w.k, w.fitted_on), (dim, 4, n));
        let z = w.apply_rows(&data);
        assert_eq!(z.len(), n * 4);
        for r in z.chunks_exact(4) {
            assert!((r.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4, "unit");
        }
        // Centred: the mean of the whitened rows is near zero against
        // their unit length.
        for c in 0..4 {
            let m: f32 = z.chunks_exact(4).map(|r| r[c]).sum::<f32>() / n as f32;
            assert!(m.abs() < 0.1, "component {c} mean {m}");
        }
        // The same as one at a time.
        assert_eq!(w.apply(&data[..dim]), z[..4].to_vec());
        // A vector of another width is refused, not misread.
        assert!(w.apply(&[1.0, 0.0]).is_empty());
        // Too little to fit.
        assert!(Whitening::fit(&data[..dim], dim, 4).is_none());
        assert!(Whitening::fit(&[], 0, 4).is_none());
    }

    #[test]
    fn a_whitening_file_round_trips_for_its_model_only() {
        let d = std::env::temp_dir().join(format!("pp-whiten-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut g = Lcg(3);
        let data: Vec<f32> = (0..64 * 6).map(|_| g.next() as f32).collect();
        let w = Whitening::fit(&data, 6, 3).unwrap();
        let p = d.join("headwhiten.bin");
        w.save(&p, "model-a", "sha-a", 1).unwrap();
        assert_eq!(Whitening::load(&p, "model-a", "sha-a", 1), Some(w.clone()));
        assert_eq!(Whitening::load(&p, "model-b", "sha-a", 1), None, "another model's whitening is never used");
        assert_eq!(Whitening::load(&p, "model-a", "sha-b", 1), None);
        assert_eq!(Whitening::load(&p, "model-a", "sha-a", 2), None, "another crop's neither");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // A damaged file is no whitening.
        let mut bytes = std::fs::read(&p).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&p, &bytes).unwrap();
        assert_eq!(Whitening::load(&p, "model-a", "sha-a", 1), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The fit's cost at the research cohort's size (10 826 heads × 768)
    /// and the library's expected one (`PP_WHITEN_ROWS`, 68 000 by
    /// default): the plan's bar is under 10 s on the processor.
    #[test]
    #[ignore = "measures the fit at library scale (PP_WHITEN_ROWS)"]
    fn the_fit_takes_seconds_at_library_scale() {
        let rows: usize = std::env::var("PP_WHITEN_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(68_000);
        let dim = 768;
        let mut g = Lcg(5);
        let data: Vec<f32> = (0..rows * dim).map(|_| g.next() as f32).collect();
        let t = std::time::Instant::now();
        let w = Whitening::fit(&data, dim, K).unwrap();
        let fit = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let z = w.apply_rows(&data);
        let apply = t.elapsed().as_secs_f64();
        eprintln!("{rows} heads × {dim}: fit {fit:.2} s, whitening every head {apply:.2} s ({} wide)", z.len() / rows);
        assert!(fit < 10.0, "{fit} s");
    }
}
