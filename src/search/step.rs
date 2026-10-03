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

use crate::model::subjects::{self, Cluster, FaceRef, Kind, Rows, SubjectsFile};

/// What a kind's step gives the grouping.
pub trait KindStep: Send {
    fn kind(&self) -> Kind;
    /// Every detection with a vector, in row order (stable between
    /// regroups: rows are appended; a rewrite starts the rows over).
    fn refs(&self) -> &[FaceRef];
    /// The vectors, one unit row per ref.
    fn rows(&self) -> Rows<'_>;
    /// How clear a view a detection is on its own (size, sharpness,
    /// score), 0..1.
    fn clarity(&self, r: &FaceRef) -> f32;
    /// The model's cut-off (cosine distance) for one person, suit or pet.
    fn cutoff(&self) -> f32;
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
    pub cutoff: f32,
    pub file: SubjectsFile,
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
        let new: Vec<usize> = (self.rows..refs.len()).filter(|&i| !confirmed.contains_key(&refs[i])).collect();
        let rows = step.rows();
        subjects::assign(&mut self.clusters, &rows, &new, step.cutoff());
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
        dim: rows.dim,
        data: rows.data.to_vec(),
        clarity: step.refs().iter().map(|r| step.clarity(r)).collect(),
        cutoff: step.cutoff(),
        file: file.clone(),
    }
}

/// The regroup itself (pure; run on a worker): clusters over the rows
/// not confirmed to a subject, clearest first; sureness per row;
/// suggestion counts per subject of the kind.
pub fn regroup(j: &RegroupJob) -> Regrouped {
    let rows = Rows { dim: j.dim, data: &j.data };
    let n = rows.len().min(j.refs.len());
    let confirmed = j.file.confirmed(j.kind);
    let mut order: Vec<usize> = (0..n).filter(|&i| !confirmed.contains_key(&j.refs[i])).collect();
    order.sort_by(|&a, &b| j.clarity[b].total_cmp(&j.clarity[a]).then(a.cmp(&b)));
    let clusters = subjects::cluster(&rows, &order, j.cutoff);
    let mut sureness = vec![1.0f32; n];
    for c in &clusters {
        for &m in &c.members {
            let d = 1.0 - c.centroid.iter().zip(rows.row(m)).map(|(a, b)| a * b).sum::<f32>();
            sureness[m] = subjects::sureness(d, j.cutoff);
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
            if let Some(d) = subjects::distance_to_members(&rows, &others, rows.row(m)) {
                sureness[m] = subjects::sureness(d, j.cutoff);
            }
        }
        if members.is_empty() {
            continue;
        }
        let rejected: HashSet<usize> = s.rejected.iter().filter_map(|r| by_ref.get(r).copied()).collect();
        let open: Vec<usize> = pool.iter().copied().filter(|i| !rejected.contains(i)).collect();
        let count = subjects::candidates(&rows, &members, &open, j.cutoff).len();
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
            Rows { dim: 8, data: &self.data }
        }
        fn clarity(&self, r: &FaceRef) -> f32 {
            r.bbox[2]
        }
        fn cutoff(&self) -> f32 {
            0.5
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
