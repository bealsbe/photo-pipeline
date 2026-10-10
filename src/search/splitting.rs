//! Splitting a fursuit group that holds more than one suit (fursuit
//! merging, stage 3): what the pages need to show the heads that look
//! least like the rest, and to propose a split the user confirms.
//!
//! Measured on the user's labels (`docs/library-search-plan.md`, "As built
//! — splitting mixed fursuit groups"): a group's strays sit at its far
//! end.  Ranked by distance to the group's mean, one head of two seen in
//! one photo (so of two suits) is among the 5 farthest in 29 of 40 such
//! pairs (whitened grouping), against 17 of 178 labelled same pairs.  A
//! fixed distance does much worse (3 of 40 at the group cut), so the
//! pages rank and the user picks.
//!
//! **Two heads in one photo** are two suits (a suit is in a photo once):
//! a group holding both holds at least two fursuits, a certain signal
//! with no model (25 of 492 groups of 5+ photos, whitened).  Boxes that
//! overlap are one head found twice, not two.
//!
//! Nothing here moves a head: the user splits, through the existing move,
//! one undo step.  Fursuits only.

use std::collections::HashMap;

use crate::model::subjects::{FaceRef, Rows};

/// Boxes overlapping more than this share of the smaller one are one
/// head found twice, not two suits.
pub const SAME_HEAD_OVERLAP: f32 = 0.05;

/// Heads marked as "least like the rest" in a group of `n`: the farthest
/// few, never most of a small group.
pub fn marked(n: usize) -> usize {
    (n / 3).min(5)
}

/// Each row's distance to the mean of `members` (cosine distance to the
/// unit mean), farthest first.
pub fn far_order(rows: &Rows<'_>, members: &[usize]) -> Vec<(usize, f32)> {
    let members: Vec<usize> = members.iter().copied().filter(|&m| m < rows.len()).collect();
    if members.is_empty() {
        return Vec::new();
    }
    let mut mean = vec![0f32; rows.dim()];
    for &m in &members {
        rows.add_into(m, &mut mean);
    }
    let norm = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        mean.iter_mut().for_each(|x| *x /= norm);
    }
    let mut out: Vec<(usize, f32)> = members.iter().map(|&m| (m, 1.0 - rows.dot(m, &mean))).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Pairs of heads of one photo (two suits), each pair once: heads on the
/// same photo whose boxes don't overlap.
pub fn photo_pairs(heads: &[FaceRef]) -> Vec<(FaceRef, FaceRef)> {
    let mut by_photo: HashMap<_, Vec<FaceRef>> = HashMap::new();
    for h in heads {
        let v = by_photo.entry(h.key).or_default();
        if !v.contains(h) {
            v.push(*h);
        }
    }
    let mut out = Vec::new();
    for v in by_photo.values() {
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                if v[i].overlap(&v[j]) < SAME_HEAD_OVERLAP {
                    out.push((v[i], v[j]));
                }
            }
        }
    }
    out.sort_by(|a, b| (a.0.key.0, a.0.thousandths(), a.1.thousandths()).cmp(&(b.0.key.0, b.0.thousandths(), b.1.thousandths())));
    out
}

/// A proposed split around two heads of different suits (rows `a`, `b`):
/// each member goes with the one it is nearer.  Returns (with `a`, with
/// `b`), each in `members`' order.
pub fn around(rows: &Rows<'_>, members: &[usize], a: usize, b: usize) -> (Vec<usize>, Vec<usize>) {
    let (va, vb) = (rows.row_vec(a), rows.row_vec(b));
    let mut with_a = Vec::new();
    let mut with_b = Vec::new();
    for &m in members.iter().filter(|&&m| m < rows.len()) {
        if m == b || (m != a && rows.dot(m, &vb) > rows.dot(m, &va)) {
            with_b.push(m);
        } else {
            with_a.push(m);
        }
    }
    (with_a, with_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key;

    fn circle(angles: &[f32]) -> Vec<f32> {
        angles.iter().flat_map(|a| [a.to_radians().cos(), a.to_radians().sin()]).collect()
    }

    /// The far end: a suit's heads around 0° and two strays far off come
    /// out farthest first; the marks are a few, never most of a group.
    #[test]
    fn strays_come_first_and_only_a_few_are_marked() {
        let data = circle(&[0.0, 4.0, -3.0, 2.0, -1.0, 5.0, 60.0, -55.0]);
        let rows = Rows::F32 { dim: 2, data: &data };
        let order = far_order(&rows, &[0, 1, 2, 3, 4, 5, 6, 7]);
        let first: Vec<usize> = order.iter().take(2).map(|x| x.0).collect();
        assert!(first.contains(&6) && first.contains(&7), "{order:?}");
        assert!(order.windows(2).all(|w| w[0].1 >= w[1].1), "farthest first");
        assert_eq!((marked(2), marked(5), marked(8), marked(13), marked(200)), (0, 1, 2, 4, 5));
        assert!(far_order(&rows, &[]).is_empty());
    }

    /// Two heads of one photo are two suits; one head boxed twice is not.
    #[test]
    fn two_heads_of_one_photo_are_two_suits() {
        let k = |n: u8| Key([n; 16]);
        let a = FaceRef::new(k(1), [0.1, 0.1, 0.2, 0.3]);
        let b = FaceRef::new(k(1), [0.6, 0.1, 0.2, 0.3]);
        let twice = FaceRef::new(k(1), [0.12, 0.11, 0.2, 0.3]);
        let other = FaceRef::new(k(2), [0.6, 0.1, 0.2, 0.3]);
        assert_eq!(photo_pairs(&[a, other]), []);
        assert_eq!(photo_pairs(&[a, twice]), [], "one head found twice");
        let p = photo_pairs(&[a, b, other, a]);
        assert_eq!(p.len(), 1);
        assert!((p[0] == (a, b)) || (p[0] == (b, a)));
    }

    /// The proposal: each head with the seed it is nearer, both seeds on
    /// their own side.
    #[test]
    fn a_proposed_split_goes_around_the_two_heads() {
        let data = circle(&[0.0, 10.0, 5.0, 90.0, 85.0, 80.0, 43.0]);
        let rows = Rows::F32 { dim: 2, data: &data };
        let (a, b) = around(&rows, &[0, 1, 2, 3, 4, 5, 6], 0, 3);
        assert_eq!(a, [0, 1, 2, 6]);
        assert_eq!(b, [3, 4, 5]);
    }
}
