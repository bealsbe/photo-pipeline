//! The per-kind extension point (plan §5.1, §6.5): what a kind's step
//! (faces for people; heads for fursuits in phase 4; pets' own step in
//! phase 5) supplies, and the grouping the engine runs over it, the same
//! for every kind and never across kinds.
//!
//! A [`KindStep`] owns a kind's detections and vectors (its own cache
//! files) and answers for them: the refs, the vectors as rows, each
//! detection's visual clarity, and the model's cut-off.  Everything
//! above that — clusters, incremental assignment, the regroup on a
//! worker, suggestions one per burst, the clearest-first order — is
//! [`Grouped`] and the functions here, written once.  The user's file
//! (`SubjectsFile`) is shared by every kind and held by `faces::People`;
//! its operations check kinds themselves.

use std::collections::{HashMap, HashSet};

use crate::model::subjects::{self, Cluster, Cutoffs, FaceRef, Kind, Linkage, Removed, Rows, SubjectsFile};

/// What a kind's step gives the grouping.
pub trait KindStep: Send {
    fn kind(&self) -> Kind;
    /// Every detection with a vector, in row order (stable between
    /// regroups: rows are appended; a rewrite starts the rows over).
    fn refs(&self) -> &[FaceRef];
    /// The vectors, one unit row per ref, as f32 or f16 bits.
    fn rows(&self) -> Rows<'_>;
    /// How clear a view a detection is on its own (size, sharpness,
    /// score), 0..1.
    fn clarity(&self, r: &FaceRef) -> f32;
    /// The model's cut-offs (cosine distances) for one person, suit or
    /// pet: for the automatic groups, for suggestions, for *Find more*.
    fn cutoffs(&self) -> Cutoffs;
    /// How the automatic groups are formed; people's grouping unless the
    /// kind's labels say otherwise.
    fn linkage(&self) -> Linkage {
        Linkage::PEOPLE
    }
    /// A box the user drew (the viewer's labels), as against one the
    /// step found: it takes part only while the user's file lists it.
    fn is_added(&self, r: &FaceRef) -> bool {
        let _ = r;
        false
    }
    /// A detection too small and blurred to tell anyone apart (a
    /// fursuit head's blob floor, `heads::HeadDetection::is_blob`): kept
    /// in the cache, but out of the groups, suggestions and labels unless
    /// the user confirmed it.  No floor for people.
    fn below_floor(&self, r: &FaceRef) -> bool {
        let _ = r;
        false
    }
}

/// The rows kept out of a kind's grouping and suggestions: detections
/// the user removed ("Not a face"), boxes drawn once whose drawing was
/// taken back (their vectors stay in the cache), and detections under
/// the kind's floor ([`KindStep::below_floor`]) that the user hasn't
/// confirmed into a subject.
pub struct Exclusion {
    removed: Removed,
    added: HashSet<FaceRef>,
    confirmed: HashSet<FaceRef>,
}

impl Exclusion {
    pub fn new(file: &SubjectsFile, kind: Kind) -> Exclusion {
        Exclusion { removed: file.removed_index(kind), added: file.added_of(kind).copied().collect(), confirmed: file.confirmed(kind).into_keys().collect() }
    }

    /// `r` (a row of `step`) is kept out.
    pub fn excludes(&self, step: &dyn KindStep, r: &FaceRef) -> bool {
        self.removed.covers(r) || (step.is_added(r) && !self.added.contains(r)) || (step.below_floor(r) && !self.confirmed.contains(r))
    }

    /// Per row of `step`: kept out or not.
    pub fn rows(&self, step: &dyn KindStep) -> Vec<bool> {
        step.refs().iter().map(|r| self.excludes(step, r)).collect()
    }
}

/// What a regroup works on: a copy of the step's data, so the worker
/// thread owns it.
pub struct RegroupJob {
    pub kind: Kind,
    pub generation: u64,
    pub refs: Vec<FaceRef>,
    pub dim: usize,
    pub data: Vec<f32>,
    pub clarity: Vec<f32>,
    pub cutoffs: Cutoffs,
    pub linkage: Linkage,
    pub file: SubjectsFile,
    /// Per row: kept out by the user's labels ([`Exclusion`]).
    pub excluded: Vec<bool>,
}

/// What a regroup gives back.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Regrouped {
    pub kind: Kind,
    pub generation: u64,
    /// Rows the job saw: a step with more rows since has new faces to
    /// assign on top.
    pub rows: usize,
    pub clusters: Vec<Cluster>,
    /// Per row: how sure its membership is (of its cluster, or of the
    /// subject it is confirmed to), 0..1.
    pub sureness: Vec<f32>,
    /// Suggestions waiting per subject (rows within the cut-off of its
    /// members, not confirmed anywhere, not rejected for it).
    pub suggestion_counts: HashMap<subjects::SubjectId, usize>,
}

impl Default for Kind {
    fn default() -> Self {
        Kind::Person
    }
}

/// The grouping state of one kind on the engine's thread.
#[derive(Debug, Clone, Default)]
pub struct Grouped {
    pub generation: u64,
    pub rows: usize,
    pub clusters: Vec<Cluster>,
    /// Each cluster's id (`subjects::cluster_id` of its clearest member).
    pub ids: Vec<u64>,
    pub sureness: Vec<f32>,
    pub suggestion_counts: HashMap<subjects::SubjectId, usize>,
}

impl Grouped {
    /// The cluster with this id, for the current generation.
    pub fn cluster(&self, id: u64) -> Option<&Cluster> {
        self.ids.iter().position(|&i| i == id).map(|i| &self.clusters[i])
    }

    /// Take a regroup's result.
    pub fn apply(&mut self, r: Regrouped, refs: &[FaceRef], clarity: &dyn Fn(&FaceRef) -> f32) {
        self.generation = r.generation;
        self.rows = r.rows;
        self.clusters = r.clusters;
        self.sureness = r.sureness;
        self.suggestion_counts = r.suggestion_counts;
        self.refresh_ids(refs, clarity);
    }

    /// Ids follow the clearest member of each cluster.
    pub fn refresh_ids(&mut self, refs: &[FaceRef], clarity: &dyn Fn(&FaceRef) -> f32) {
        self.ids = self
            .clusters
            .iter()
            .map(|c| {
                let cover = c.members.iter().filter_map(|&m| refs.get(m)).max_by(|a, b| clarity(a).total_cmp(&clarity(b)));
                cover.map_or(0, subjects::cluster_id)
            })
            .collect();
    }

    /// Rows that arrived since the last regroup join the nearest cluster
    /// within the cut-off or start one (the pass); confirmed rows stay
    /// out.  Sureness for them is 1 until the next regroup.
    pub fn assign_new(&mut self, step: &dyn KindStep, file: &SubjectsFile) {
        let refs = step.refs();
        if refs.len() <= self.rows {
            return;
        }
        let confirmed = file.confirmed(step.kind());
        let out = Exclusion::new(file, step.kind());
        let new: Vec<usize> = (self.rows..refs.len()).filter(|&i| !confirmed.contains_key(&refs[i]) && !out.excludes(step, &refs[i])).collect();
        let rows = step.rows();
        subjects::assign_with(&mut self.clusters, &rows, &new, step.cutoffs().group, step.linkage());
        self.sureness.resize(refs.len(), 1.0);
        self.rows = refs.len();
        self.refresh_ids(refs, &|r| step.clarity(r));
    }

    /// The clearest-first clarity of a row: its own clarity and how sure
    /// its membership is, half each (UI 12).
    pub fn clarity_of(&self, row: usize, visual: f32) -> f32 {
        let sure = self.sureness.get(row).copied().unwrap_or(1.0);
        0.5 * visual + 0.5 * sure
    }
}

/// A copy of the step's data for a regroup on a worker.
pub fn job(step: &dyn KindStep, file: &SubjectsFile, generation: u64) -> RegroupJob {
    let rows = step.rows();
    RegroupJob {
        kind: step.kind(),
        generation,
        refs: step.refs().to_vec(),
        dim: rows.dim(),
        data: rows.to_f32(),
        clarity: step.refs().iter().map(|r| step.clarity(r)).collect(),
        cutoffs: step.cutoffs(),
        linkage: step.linkage(),
        file: file.clone(),
        excluded: Exclusion::new(file, step.kind()).rows(step),
    }
}

/// The regroup itself (pure; run on a worker): clusters over the rows
/// not confirmed to a subject, clearest first; sureness per row;
/// suggestion counts per subject of the kind.
pub fn regroup(j: &RegroupJob) -> Regrouped {
    let rows = Rows::F32 { dim: j.dim, data: &j.data };
    let n = rows.len().min(j.refs.len());
    let confirmed = j.file.confirmed(j.kind);
    let out = |i: usize| j.excluded.get(i).copied().unwrap_or(false);
    let mut order: Vec<usize> = (0..n).filter(|&i| !confirmed.contains_key(&j.refs[i]) && !out(i)).collect();
    order.sort_by(|&a, &b| j.clarity[b].total_cmp(&j.clarity[a]).then(a.cmp(&b)));
    let clusters = subjects::cluster_with(&rows, &order, j.cutoffs.group, j.linkage);
    let mut sureness = vec![1.0f32; n];
    for c in &clusters {
        for &m in &c.members {
            let d = 1.0 - rows.dot(m, &c.centroid);
            sureness[m] = subjects::sureness(d, j.cutoffs.group);
        }
    }
    // Confirmed rows: how sure against the subject's other members.
    let by_ref: HashMap<FaceRef, usize> = j.refs.iter().take(n).enumerate().map(|(i, r)| (*r, i)).collect();
    let mut suggestion_counts = HashMap::new();
    let pool: Vec<usize> = order.clone();
    for s in j.file.subjects.iter().filter(|s| s.kind == j.kind) {
        let members: Vec<usize> = s.members.iter().filter_map(|r| by_ref.get(r).copied()).collect();
        for &m in &members {
            let others: Vec<usize> = members.iter().copied().filter(|&o| o != m).collect();
            if let Some(d) = subjects::distance_to_members(&rows, &others, &rows.row_vec(m)) {
                sureness[m] = subjects::sureness(d, j.cutoffs.suggest);
            }
        }
        if members.is_empty() {
            continue;
        }
        let rejected: HashSet<usize> = s.rejected.iter().filter_map(|r| by_ref.get(r).copied()).collect();
        let open: Vec<usize> = pool.iter().copied().filter(|i| !rejected.contains(i)).collect();
        let count = subjects::candidates(&rows, &members, &open, j.cutoffs.suggest).len();
        suggestion_counts.insert(s.id.clone(), count);
    }
    Regrouped { kind: j.kind, generation: j.generation, rows: n, clusters, sureness, suggestion_counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key;

    /// A step over synthetic rows: two people of four faces, one loner.
    struct Toy {
        refs: Vec<FaceRef>,
        data: Vec<f32>,
    }

    impl Toy {
        fn new() -> Toy {
            let dim = 8;
            let mut data = Vec::new();
            let mut refs = Vec::new();
            for i in 0..9u8 {
                let mut v = vec![0f32; dim];
                let axis = if i < 4 { 0 } else if i < 8 { 1 } else { 5 };
                v[axis] = 1.0;
                v[2 + (i % 2) as usize] += 0.15 * (i as f32 + 1.0) / 9.0;
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                data.extend(v.iter().map(|x| x / n));
                refs.push(FaceRef::new(Key([i + 1; 16]), [0.1, 0.1, 0.1 + i as f32 * 0.02, 0.2]));
            }
            Toy { refs, data }
        }
    }

    impl KindStep for Toy {
        fn kind(&self) -> Kind {
            Kind::Person
        }
        fn refs(&self) -> &[FaceRef] {
            &self.refs
        }
        fn rows(&self) -> Rows<'_> {
            Rows::F32 { dim: 8, data: &self.data }
        }
        fn clarity(&self, r: &FaceRef) -> f32 {
            r.bbox[2]
        }
        fn cutoffs(&self) -> Cutoffs {
            Cutoffs { group: 0.5, suggest: 0.5, strict: 0.375 }
        }
    }

    #[test]
    fn a_regroup_clusters_the_unconfirmed_and_counts_suggestions_per_subject() {
        let toy = Toy::new();
        let mut file = SubjectsFile::default();
        // Person A confirmed with two of its four faces.
        let a = file.create(Kind::Person, Some("A".into()), vec![toy.refs[0], toy.refs[1]]);
        file.reject(&a, vec![toy.refs[3]]).unwrap();
        let j = job(&toy, &file, 3);
        assert_eq!((j.refs.len(), j.dim, j.data.len()), (9, 8, 72));
        let r = regroup(&j);
        assert_eq!((r.kind, r.generation, r.rows), (Kind::Person, 3, 9));
        let sizes: Vec<usize> = r.clusters.iter().map(|c| c.members.len()).collect();
        assert_eq!(sizes, [4, 2, 1], "person B whole, A's two loose faces, the loner: {sizes:?}");
        assert!(r.clusters.iter().all(|c| !c.members.contains(&0) && !c.members.contains(&1)), "confirmed faces take no part");
        assert_eq!(r.suggestion_counts[&a], 1, "face 2 is offered; face 3 is rejected");
        assert!(r.sureness.iter().all(|&s| (0.0..=1.0).contains(&s)));
        assert!(r.sureness[0] > 0.9 && r.sureness[8] == 1.0, "a loner is its own group: {:?}", r.sureness);
        // Applied: ids follow the clearest member; a stale id is unknown.
        let mut g = Grouped::default();
        g.apply(r, &toy.refs, &|r| toy.clarity(r));
        assert_eq!(g.ids.len(), 3);
        let b_id = g.ids[0];
        assert_eq!(g.cluster(b_id).unwrap().members.len(), 4);
        assert!(g.cluster(12345).is_none());
        assert!((g.clarity_of(8, 0.5) - 0.75).abs() < 1e-6);
        // New rows arriving join the nearest cluster.
        let mut more = Toy::new();
        let mut v = vec![0f32; 8];
        v[1] = 1.0;
        more.data.extend(v);
        more.refs.push(FaceRef::new(Key([99; 16]), [0.1, 0.1, 0.5, 0.2]));
        g.assign_new(&more, &file);
        assert_eq!(g.rows, 10);
        assert_eq!(g.cluster(g.ids[0]).map(|c| c.members.len()), Some(5), "{:?}", g.clusters);
        g.assign_new(&more, &file);
        assert_eq!(g.rows, 10, "nothing new twice");
    }

    /// The viewer's labels in the grouping: with none, a regroup is what
    /// it was (people's grouping unchanged); a removed face (or one found
    /// again a thousandth off) takes no part, nor does it count as a
    /// suggestion; a removal of a fursuit head says nothing about a face.
    #[test]
    fn removed_faces_take_no_part_and_no_labels_change_nothing() {
        let toy = Toy::new();
        let mut file = SubjectsFile::default();
        let a = file.create(Kind::Person, Some("A".into()), vec![toy.refs[0], toy.refs[1]]);
        let plain = regroup(&job(&toy, &file, 1));
        assert!(job(&toy, &file, 1).excluded.iter().all(|x| !x), "no labels: nothing kept out");
        // A fursuit removal on the same box changes nothing for people.
        file.remove_box(Kind::Fursuit, toy.refs[2]).unwrap();
        assert_eq!(regroup(&job(&toy, &file, 1)).clusters, plain.clusters);
        // Face 2 (A's suggestion) and face 5 (in B's group) removed; face
        // 5 slightly off, as a rebuild would find it.
        file.remove_box(Kind::Person, toy.refs[2]).unwrap();
        let off = FaceRef::new(toy.refs[5].key, [toy.refs[5].bbox[0] + 0.001, toy.refs[5].bbox[1], toy.refs[5].bbox[2], toy.refs[5].bbox[3]]);
        file.remove_box(Kind::Person, off).unwrap();
        let j = job(&toy, &file, 2);
        assert_eq!(j.excluded.iter().filter(|x| **x).count(), 2);
        let r = regroup(&j);
        assert!(r.clusters.iter().all(|c| !c.members.contains(&2) && !c.members.contains(&5)), "{:?}", r.clusters);
        assert_eq!(r.clusters.iter().map(|c| c.members.len()).sum::<usize>(), 9 - 2 - 2, "the two confirmed and the two removed out");
        assert_eq!((plain.suggestion_counts[&a], r.suggestion_counts[&a]), (2, 1), "a removed face is never a suggestion (face 3 still is)");
        // New rows arriving: a removed one stays out of the clusters too.
        let mut g = Grouped::default();
        g.apply(regroup(&job(&toy, &SubjectsFile::default(), 3)), &toy.refs, &|r| toy.clarity(r));
        let mut more = Toy::new();
        let mut v = vec![0f32; 8];
        v[1] = 1.0;
        more.data.extend(v);
        let late = FaceRef::new(Key([77; 16]), [0.1, 0.1, 0.5, 0.2]);
        more.refs.push(late);
        file.remove_box(Kind::Person, late).unwrap();
        let before: usize = g.clusters.iter().map(|c| c.members.len()).sum();
        g.assign_new(&more, &file);
        assert_eq!(g.clusters.iter().map(|c| c.members.len()).sum::<usize>(), before, "the removed newcomer joins nothing");
    }

    /// A kind's floor (round 2): rows under it take no part in a regroup
    /// or in the suggestions, unless confirmed into a subject; a kind
    /// without a floor (people) is as before.
    #[test]
    fn rows_under_the_floor_take_no_part_unless_confirmed() {
        struct Floored(Toy);
        impl KindStep for Floored {
            fn kind(&self) -> Kind {
                Kind::Fursuit
            }
            fn refs(&self) -> &[FaceRef] {
                &self.0.refs
            }
            fn rows(&self) -> Rows<'_> {
                self.0.rows()
            }
            fn clarity(&self, r: &FaceRef) -> f32 {
                self.0.clarity(r)
            }
            fn cutoffs(&self) -> Cutoffs {
                self.0.cutoffs()
            }
            fn below_floor(&self, r: &FaceRef) -> bool {
                r.bbox[2] < 0.13 // rows 0 and 1 (widths 0.10, 0.12)
            }
        }
        let step = Floored(Toy::new());
        let file = SubjectsFile::default();
        let j = job(&step, &file, 1);
        assert_eq!(j.excluded.iter().filter(|x| **x).count(), 2, "{:?}", j.excluded);
        let r = regroup(&j);
        assert!(r.clusters.iter().all(|c| !c.members.contains(&0) && !c.members.contains(&1)), "{:?}", r.clusters);
        assert_eq!(r.clusters.iter().map(|c| c.members.len()).sum::<usize>(), 7);
        // Confirmed, row 0 is a member (and so not clustered either way);
        // row 1 stays out; a person's step has no floor.
        let mut file = SubjectsFile::default();
        file.create(Kind::Fursuit, Some("A".into()), vec![step.refs()[0]]);
        let out = Exclusion::new(&file, Kind::Fursuit);
        assert!(!out.excludes(&step, &step.refs()[0]) && out.excludes(&step, &step.refs()[1]));
        assert!(job(&Toy::new(), &file, 1).excluded.iter().all(|x| !x), "people: no floor");
    }

    /// Kinds never mix in the grouping: a job for one kind counts only
    /// that kind's subjects, and a fursuit named like a person is nobody
    /// here.
    #[test]
    fn a_regroup_sees_its_own_kind_only() {
        let toy = Toy::new();
        let mut file = SubjectsFile::default();
        let suit = file.create(Kind::Fursuit, Some("Someone".into()), vec![toy.refs[0], toy.refs[1]]);
        let r = regroup(&job(&toy, &file, 1));
        assert!(!r.suggestion_counts.contains_key(&suit), "a fursuit subject is not counted for people");
        assert_eq!(r.clusters.iter().map(|c| c.members.len()).sum::<usize>(), 9, "its faces are not confirmed for people");
    }
}
