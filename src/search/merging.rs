//! "Possibly the same fursuit": groups that may be one suit split apart,
//! offered to merge, nearest first (fursuit merging, stage 2).
//!
//! **Why groups split.**  Fursuit groups are tight by design (average
//! linkage at the step's group cut, no joining step): looser grouping
//! mixed suits.  On the user's labels, about half the same-suit pairs sit
//! in different groups.  This module finds the pairs of groups worth
//! asking about; the user merges or says "not the same", one key each.
//!
//! **The distance between two groups** is average linkage, the mean of
//! every cross pair's cosine distance, in the space and with the cut-offs
//! the library's step uses right now (raw vectors below
//! `whiten::MIN_HEADS` heads, whitened from there).  The step's rows are
//! unit length, so the mean of the cross dot products is the dot product
//! of the two groups' mean rows: `1 − mean_a · mean_b`, exact, and the
//! cost is groups², not heads².  It is the linkage the groups are built
//! with, so a pair under the cut is two groups the step would have joined
//! had it seen them in another order.  On the labels (whitened, groups of
//! at least 5 photos) the suggest cut offers 46 of the 103 split pairs
//! with 46 labelled same and 2 different; the closest pair of heads
//! offered as many but with 163 different, so it is not used.
//!
//! **The judges compare groups** when one is on (card only, one at a
//! time, the cap): a pair under sure-yes whose covers are both clear
//! enough is offered at once; every other pair up to sure-no is a close
//! call, judged on up to three head pairs ([`questions`]) and offered when
//! most of them say the same suit ([`GroupVerdict::same`]).  A head pair
//! the judge couldn't look at counts as a no, so there are no ties and an
//! unseen pair is never offered.  With the judge off (or unable to run)
//! the suggest cut alone decides.
//!
//! Suggestions only: nothing is merged, named or marked here.  Fursuits
//! only: the groups come from the fursuit step and nothing else.

use std::sync::Arc;

use crate::model::subjects::{Face, FaceRef, GroupId, Rows};

/// Head pairs asked of the judge for one pair of groups, at most.
pub const QUESTIONS: usize = 3;

/// One group as the suggestions see it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: GroupId,
    /// Its rows, the cover first, then clearest first.
    pub rows: Vec<usize>,
    /// The heads of `rows`, in the same order.
    pub heads: Vec<FaceRef>,
}

/// What the judge made of a pair of groups: its head pairs' answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupVerdict {
    pub yes: u8,
    pub no: u8,
    /// Head pairs it couldn't look at (a photo gone, an unreadable file):
    /// counted as no.
    pub unjudged: u8,
}

impl GroupVerdict {
    /// The same suit: most of the head pairs asked say so; a tie, or an
    /// unjudged pair tipping it, is no.
    pub fn same(&self) -> bool {
        let asked = self.yes + self.no + self.unjudged;
        asked > 0 && self.yes * 2 > asked
    }

    pub fn asked(&self) -> u8 {
        self.yes + self.no + self.unjudged
    }
}

/// A pair of groups that may be one fursuit, as the pages show it.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupPair {
    pub a: GroupId,
    pub b: GroupId,
    /// Average linkage in the step's space (raw or whitened).
    pub distance: f32,
    /// A few heads of each, the cover first.
    pub a_heads: Arc<Vec<Face>>,
    pub b_heads: Arc<Vec<Face>>,
    /// Offered by the distance alone (under the suggest cut with the
    /// judge off; under sure-yes with both covers clear with it on).
    pub sure: bool,
    /// The judge's answer, when it compared them.
    pub verdict: Option<GroupVerdict>,
}

impl GroupPair {
    /// The pair as one key, whichever way round.
    pub fn key(&self) -> (GroupId, GroupId) {
        order(self.a.clone(), self.b.clone())
    }
}

/// Two group ids in a fixed order.
pub fn order(a: GroupId, b: GroupId) -> (GroupId, GroupId) {
    let k = |g: &GroupId| match g {
        GroupId::Subject(s) => (0u8, s.0.clone(), 0u64),
        GroupId::Cluster(c) => (1u8, String::new(), *c),
    };
    if k(&a) <= k(&b) { (a, b) } else { (b, a) }
}

/// Each group's mean row (the sum over its members, divided by their
/// number: not unit length, so that a dot product of two is the mean of
/// their members' dot products).
pub fn means(rows: &Rows<'_>, groups: &[&[usize]]) -> Vec<Vec<f32>> {
    let dim = rows.dim();
    groups
        .iter()
        .map(|g| {
            let mut sum = vec![0f32; dim];
            let mut n = 0usize;
            for &r in g.iter().filter(|&&r| r < rows.len()) {
                rows.add_into(r, &mut sum);
                n += 1;
            }
            if n > 0 {
                sum.iter_mut().for_each(|x| *x /= n as f32);
            }
            sum
        })
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Pairs of groups (by index) whose average linkage `1 − mean_a·mean_b`
/// is at most `cut`, nearest first; with `focus`, only the pairs with
/// that group.
pub fn pairs(means: &[Vec<f32>], cut: f32, focus: Option<usize>) -> Vec<(usize, usize, f32)> {
    use rayon::prelude::*;
    let n = means.len();
    let mut out: Vec<(usize, usize, f32)> = match focus {
        Some(f) if f < n => (0..n).filter(|&j| j != f).filter_map(|j| {
            let d = 1.0 - dot(&means[f], &means[j]);
            (d <= cut).then_some((f.min(j), f.max(j), d))
        }).collect(),
        Some(_) => Vec::new(),
        None => (0..n)
            .into_par_iter()
            .flat_map_iter(|i| {
                let mi = &means[i];
                (i + 1..n).filter_map(move |j| {
                    let d = 1.0 - dot(mi, &means[j]);
                    (d <= cut).then_some((i, j, d))
                })
            })
            .collect(),
    };
    out.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    out
}

/// The head pairs the judge is shown for two groups (rows, the cover
/// first then clearest first), at most [`QUESTIONS`], each asked once:
/// the two covers; the member of each nearest the other group's mean
/// (where they look most alike); the second clearest of each.  Several
/// views, so one odd frame (a turned head, a dark stage) doesn't decide.
pub fn questions(rows: &Rows<'_>, a: &[usize], b: &[usize], mean_a: &[f32], mean_b: &[f32]) -> Vec<(usize, usize)> {
    let (Some(&ca), Some(&cb)) = (a.first(), b.first()) else { return Vec::new() };
    let mut out = vec![(ca, cb)];
    let nearest = |side: &[usize], to: &[f32]| side.iter().copied().filter(|&r| r < rows.len()).max_by(|&x, &y| rows.dot(x, to).total_cmp(&rows.dot(y, to)));
    if let (Some(x), Some(y)) = (nearest(a, mean_b), nearest(b, mean_a)) {
        out.push((x, y));
    }
    if let (Some(&x), Some(&y)) = (a.get(1), b.get(1)) {
        out.push((x, y));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(*p));
    out.truncate(QUESTIONS);
    out
}

/// The verdict of a pair of groups from its head pairs' answers (`None`:
/// the judge couldn't look at that one).
pub fn verdict(answers: &[Option<bool>]) -> GroupVerdict {
    let mut v = GroupVerdict::default();
    for a in answers {
        match a {
            Some(true) => v.yes += 1,
            Some(false) => v.no += 1,
            None => v.unjudged += 1,
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit rows on a circle, at these angles (degrees).
    fn circle(angles: &[f32]) -> Vec<f32> {
        angles.iter().flat_map(|a| [a.to_radians().cos(), a.to_radians().sin()]).collect()
    }

    /// The group distance is exactly the mean of the cross pairs'
    /// distances (unit rows), and the pairs come nearest first, within
    /// the cut only.
    #[test]
    fn the_group_distance_is_the_mean_cross_distance_and_its_cut() {
        let data = circle(&[0.0, 10.0, 20.0, 30.0, 90.0, 95.0, 200.0]);
        let rows = Rows::F32 { dim: 2, data: &data };
        let groups: Vec<Vec<usize>> = vec![vec![0, 1], vec![2, 3], vec![4, 5], vec![6]];
        let refs: Vec<&[usize]> = groups.iter().map(|g| g.as_slice()).collect();
        let m = means(&rows, &refs);
        // Exact against the mean of the four cross distances.
        let cross = |a: &[usize], b: &[usize]| {
            let mut s = 0.0;
            for &i in a {
                for &j in b {
                    s += 1.0 - rows.dot(i, &rows.row_vec(j));
                }
            }
            s / (a.len() * b.len()) as f32
        };
        let all = pairs(&m, 2.0, None);
        assert_eq!(all.len(), 6);
        for &(i, j, d) in &all {
            assert!((d - cross(&groups[i], &groups[j])).abs() < 1e-5, "{i} {j}: {d}");
        }
        assert!(all.windows(2).all(|w| w[0].2 <= w[1].2), "nearest first");
        assert_eq!((all[0].0, all[0].1), (0, 1), "the two groups 10° to 30° apart");
        // The cut: only the near pair.
        let near = pairs(&m, cross(&groups[0], &groups[1]) + 1e-4, None);
        assert_eq!(near.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(), [(0, 1)]);
        // A group's own pairs (its page's "Similar groups").
        let mine = pairs(&m, 2.0, Some(2));
        assert_eq!(mine.len(), 3);
        assert!(mine.iter().all(|p| p.0 == 2 || p.1 == 2));
        assert!(pairs(&m, 2.0, Some(9)).is_empty());
    }

    /// The judge is shown the covers, the members where the groups look
    /// most alike, and the second clearest; each pair once.  The verdict
    /// is the majority; a tie and an unseen pair are no.
    #[test]
    fn questions_and_the_majority_verdict() {
        let data = circle(&[0.0, 40.0, 25.0, 60.0, 30.0, 90.0]);
        let rows = Rows::F32 { dim: 2, data: &data };
        // Clearest first: a is 0°, 25°, 40°; b is 60°, 30°, 90°.
        let (a, b) = (vec![0usize, 2, 1], vec![3usize, 4, 5]);
        let m = means(&rows, &[a.as_slice(), b.as_slice()]);
        let q = questions(&rows, &a, &b, &m[0], &m[1]);
        assert_eq!(q, [(0, 3), (1, 4), (2, 4)], "the covers; where they look most alike (40° and 30°); the second clearest");
        // Where the second clearest are the ones most alike: asked once.
        let a2 = vec![0usize, 1, 2];
        let m2 = means(&rows, &[a2.as_slice(), b.as_slice()]);
        assert_eq!(questions(&rows, &a2, &b, &m2[0], &m2[1]), [(0, 3), (1, 4)]);
        // A single head each: one question.
        assert_eq!(questions(&rows, &[0], &[3], &m[0], &m[1]), [(0, 3)]);
        // The majority.
        assert!(verdict(&[Some(true), Some(true), Some(false)]).same());
        assert!(!verdict(&[Some(true), Some(false), Some(false)]).same());
        assert!(!verdict(&[Some(true), Some(false)]).same(), "a tie is no");
        assert!(!verdict(&[Some(true), None]).same(), "an unseen pair counts as no");
        assert!(!verdict(&[Some(true), Some(true), None, None]).same());
        assert!(verdict(&[Some(true)]).same());
        assert!(!verdict(&[None, None, None]).same(), "nothing seen: never offered");
        assert_eq!(verdict(&[Some(true), None, Some(false)]), GroupVerdict { yes: 1, no: 1, unjudged: 1 });
        // A pair of ids is one whichever way round.
        assert_eq!(order(GroupId::Cluster(2), GroupId::Cluster(1)), order(GroupId::Cluster(1), GroupId::Cluster(2)));
    }
}
