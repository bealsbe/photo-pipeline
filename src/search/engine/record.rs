//! The decision record's entries, built on the engine's thread
//! (`model::decisions`): each grouping decision with the heads it was
//! about, measured before the change (so a merge's groups are as the user
//! saw them), how it was reached, and the library's state.  Written by
//! the record's own thread; nothing here when the record is off.

use std::collections::HashMap;

use super::Worker;
use crate::model::decisions::{self, Act, Cuts, Entry, How, Lib, Side, Via, round4};
use crate::model::subjects::{EditPlan, FaceRef, GroupId, Kind, LabelEdit, NameTo, PlanDest};
use crate::search::heads;
use crate::search::merging;
use crate::search::step::KindStep;

/// What an edit's entry needs from before the edit.
pub(super) struct Before {
    kind: Kind,
    act: Act,
    /// A review answer's group and heads, for the nearest-member distance.
    answered: Option<(Vec<FaceRef>, Vec<FaceRef>)>,
}

/// Rows of `faces` in a kind's vectors (those it has).
fn rows_of(step: &dyn KindStep, faces: &[FaceRef]) -> Vec<usize> {
    let want: std::collections::HashSet<&FaceRef> = faces.iter().collect();
    step.refs().iter().enumerate().filter(|(_, r)| want.contains(r)).map(|(i, _)| i).collect()
}

/// Average linkage (`1 − mean_a · mean_b`) between two groups, in the
/// step's space.
fn linkage(step: &dyn KindStep, a: &[FaceRef], b: &[FaceRef]) -> Option<f32> {
    let (ra, rb) = (rows_of(step, a), rows_of(step, b));
    if ra.is_empty() || rb.is_empty() || step.rows().dim() == 0 {
        return None;
    }
    let m = merging::means(&step.rows(), &[&ra, &rb]);
    Some(round4(1.0 - m[0].iter().zip(&m[1]).map(|(x, y)| x * y).sum::<f32>()))
}

/// The nearest pair between `faces` and `members` (cosine distance), as
/// the review's close calls measure it: (distance, the face, the member).
fn nearest(step: &dyn KindStep, members: &[FaceRef], faces: &[FaceRef]) -> Option<(f32, FaceRef, FaceRef)> {
    let index: HashMap<&FaceRef, usize> = step.refs().iter().enumerate().map(|(i, r)| (r, i)).collect();
    let rows = step.rows();
    let mut best: Option<(f32, FaceRef, FaceRef)> = None;
    for f in faces {
        let Some(&fi) = index.get(f) else { continue };
        let v = rows.row_vec(fi);
        for m in members.iter().filter(|m| !faces.contains(m)) {
            let Some(&mi) = index.get(m) else { continue };
            let d = 1.0 - rows.dot(mi, &v);
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, *f, *m));
            }
        }
    }
    best.map(|(d, f, m)| (round4(d), f, m))
}

impl Worker {
    /// The kind's grouping step, its vectors in memory.
    fn record_step(&mut self, kind: Kind) -> Option<&dyn KindStep> {
        match kind {
            Kind::Person => self.people.store.as_ref().map(|s| s as &dyn KindStep),
            Kind::Fursuit => self.fursuits.step().map(|s| s as &dyn KindStep),
            Kind::Pet => None,
        }
    }

    fn clarity(&self, kind: Kind, r: &FaceRef) -> f32 {
        match kind {
            Kind::Person => self.people.store.as_ref().map_or(0.0, |s| s.clarity(r)),
            Kind::Fursuit => self.fursuits.store.as_ref().map_or(0.0, |s| s.clarity(r)),
            Kind::Pet => 0.0,
        }
    }

    /// Heads clearest first.
    fn clearest(&self, kind: Kind, mut faces: Vec<FaceRef>) -> Vec<FaceRef> {
        let c: HashMap<FaceRef, f32> = faces.iter().map(|f| (*f, self.clarity(kind, f))).collect();
        faces.sort_by(|a, b| c[b].total_cmp(&c[a]).then((a.key.0, a.thousandths()).cmp(&(b.key.0, b.thousandths()))));
        faces
    }

    /// A group as the record keeps it: its count, name, subject id and
    /// clearest heads (`first`: heads to put first, as a page showed
    /// them), with its members.
    fn record_side(&self, id: &GroupId, first: &[FaceRef]) -> Option<(Kind, Side, Vec<FaceRef>)> {
        let (kind, members) = self.people.members(id).ok()?;
        let subject = self.people.subject_of(id);
        let (name, sid) = (subject.and_then(|s| s.name.clone()), subject.map(|s| s.id.0.clone()));
        let shown = first.iter().filter(|f| members.contains(f)).copied();
        let order: Vec<FaceRef> = shown.chain(self.clearest(kind, members.clone())).collect();
        let snapshot = self.shared.subjects.lock().clone();
        Some((kind, Side::new(members.len(), name, sid, order).counted(snapshot.group(id)), members))
    }

    /// The library's state for `kind`: its heads, the space, the model
    /// and the cut-offs in force now (whatever setting chose them).
    fn record_lib(&self, kind: Kind) -> Option<Lib> {
        match kind {
            Kind::Person => {
                let store = self.people.store.as_ref()?;
                let c = store.cutoffs();
                Some(Lib { heads: store.found(), white: false, model: self.face_model.dir_name().into(), grouping: None, cuts: Cuts { group: c.group, suggest: c.suggest, strict: c.strict, ..Cuts::default() } })
            }
            Kind::Fursuit => {
                // The grouping choice's cuts, from where the grouping and
                // the merge suggestions read them; the review's own.
                let g = self.fursuits.cuts()?;
                let store = self.fursuits.store.as_ref()?;
                let c = store.cutoffs();
                let (yes, no) = store.band();
                Some(Lib {
                    heads: store.found(),
                    white: g.whitened,
                    model: heads::VECTOR_MODEL.dir_name().into(),
                    grouping: Some(g.grouping),
                    cuts: Cuts {
                        group: g.group,
                        suggest: c.suggest,
                        strict: c.strict,
                        sure_yes: Some(yes),
                        sure_no: Some(no),
                        merge_suggest: Some(g.merge_suggest),
                        merge_sure: Some(g.merge_sure),
                        merge_far: Some(g.merge_far),
                    },
                })
            }
            Kind::Pet => None,
        }
    }

    /// The judge chosen now: its model folder and yes threshold.
    fn judge_words(&self) -> Option<(String, f32)> {
        self.judge_choice.model().map(|m| (m.dir_name().to_string(), self.judge_choice.yes_cut()))
    }

    /// How a decision was reached: the page's account, with what the
    /// engine measures for a review answer (the nearest confirmed member
    /// and, for the judge's, its P(yes) on that pair).
    fn record_how(&mut self, kind: Kind, via: Via, answered: Option<&(Vec<FaceRef>, Vec<FaceRef>)>) -> How {
        let mut h = How::of(via, self.judge_words());
        if let Via::Review { judge, .. } = via {
            let near = answered.and_then(|(members, faces)| self.record_step(kind).and_then(|s| nearest(s, members, faces)));
            h.d = near.map(|n| n.0);
            h.pair = near.map(|(_, f, m)| [decisions::Head(f), decisions::Head(m)]);
            if judge.is_some() {
                h.p = near.and_then(|(_, f, m)| {
                    let (_, _, cache) = self.verdicts.as_ref().filter(|(c, _, _)| *c == self.judge_choice)?;
                    cache.get(f, m).map(|v| round4(v.p()))
                });
            }
        }
        h
    }

    /// Write the entry; the newest undo step remembers it, so its undo
    /// can name it.
    pub(super) fn record(&mut self, before: Option<Before>, via: Via) {
        let Some(Before { kind, act, answered }) = before else { return };
        let how = self.record_how(kind, via, answered.as_ref());
        let lib = self.record_lib(kind);
        let entry = Entry::new(kind, act, Some(how), lib);
        if let Some(id) = self.decisions.record(entry) {
            self.people.mark_recorded(id.clone(), kind);
            if let Some((_, faces)) = answered {
                self.answered.insert(sorted(faces), (id, kind));
            }
        }
    }

    /// An undo (Ctrl+Z, or an answer taken back in a review): an entry
    /// naming the one it takes back.  The undo step it makes, if any,
    /// remembers it in turn.
    pub(super) fn record_undo(&mut self, undone: Option<(String, Kind)>, step: bool) {
        let Some((id, kind)) = undone else { return };
        self.answered.retain(|_, (a, _)| *a != id);
        if let Some(id) = self.decisions.record_undo((id, kind)) {
            if step {
                self.people.mark_recorded(id, kind);
            }
        }
    }

    /// A merge's groups, measured before it: the one kept first, each
    /// other with its distance to it.
    pub(super) fn before_merge(&mut self, into: &GroupId, others: &[GroupId]) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let (kind, first, members) = self.record_side(into, &[])?;
        let mut groups = vec![first];
        for o in others.iter().filter(|o| *o != into) {
            let (k, mut side, m) = self.record_side(o, &[])?;
            if k != kind {
                return None;
            }
            side.d = self.record_step(kind).and_then(|s| linkage(s, &members, &m));
            groups.push(side);
        }
        Some(Before { kind, act: Act::Merge { groups }, answered: None })
    }

    /// "Not the same": both groups, the heads shown first.
    pub(super) fn before_not_same(&mut self, a: &GroupId, b: &GroupId, a_heads: &[FaceRef], b_heads: &[FaceRef]) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let (kind, sa, ma) = self.record_side(a, a_heads)?;
        let (_, mut sb, mb) = self.record_side(b, b_heads)?;
        sb.d = self.record_step(kind).and_then(|s| linkage(s, &ma, &mb));
        Some(Before { kind, act: Act::NotSame { groups: vec![sa, sb] }, answered: None })
    }


    /// The group editor's *Apply*: every group heads left or went to, as
    /// it was, and the heads by (from, to).
    pub(super) fn before_edit(&mut self, plan: &EditPlan) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let mut ids: Vec<GroupId> = Vec::new();
        let index = |g: &GroupId, ids: &mut Vec<GroupId>| -> usize {
            match ids.iter().position(|x| x == g) {
                Some(i) => i,
                None => {
                    ids.push(g.clone());
                    ids.len() - 1
                }
            }
        };
        let mut out: Vec<(Option<usize>, decisions::To, Vec<FaceRef>)> = Vec::new();
        for (head, from, to) in &plan.moves {
            let from = from.as_ref().map(|g| index(g, &mut ids));
            let to = match to {
                PlanDest::Group(g) => decisions::To::Group { group: index(g, &mut ids) },
                PlanDest::New(i) => decisions::To::New { new: *i, name: plan.new_names.get(*i).cloned().flatten() },
                PlanDest::Removed => decisions::To::Removed,
                PlanDest::TakenOut => decisions::To::TakenOut,
            };
            match out.iter_mut().find(|(f, t, _)| *f == from && *t == to) {
                Some((_, _, v)) => v.push(*head),
                None => out.push((from, to, vec![*head])),
            }
        }
        let mut groups = Vec::with_capacity(ids.len());
        for g in &ids {
            let (_, side, _) = self.record_side(g, &[])?;
            groups.push(side);
        }
        let out = out.into_iter().map(|(from, to, heads)| decisions::Out { from, to, moved: heads.len(), heads: decisions::heads(heads.into_iter().take(decisions::MOVED_HEADS)) }).collect();
        Some(Before { kind: plan.kind, act: Act::Edit { groups, out }, answered: None })
    }

    /// A review answer (Yes or No): the group as it was, the heads.
    pub(super) fn before_answer(&mut self, group: &GroupId, faces: &[FaceRef], yes: bool) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let (kind, side, members) = self.record_side(group, &[])?;
        let heads = decisions::heads(faces.iter().take(decisions::MOVED_HEADS).copied());
        Some(Before { kind, act: Act::Answer { yes, group: side, heads }, answered: Some((members, faces.to_vec())) })
    }

    /// "Not fursuit heads" / "Not faces" on groups or picked heads: the
    /// groups as they were (the heads removed are filled in after, from
    /// what the edit resolved).
    pub(super) fn before_remove_boxes(&mut self, kind: Kind, targets: &[(GroupId, Option<Vec<FaceRef>>)]) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let mut groups = Vec::new();
        for (id, _) in targets {
            if let Some((k, side, _)) = self.record_side(id, &[]) {
                if k == kind && !groups.contains(&side) {
                    groups.push(side);
                }
            }
        }
        Some(Before { kind, act: Act::RemoveMany { groups, heads: Vec::new(), removed: 0 }, answered: None })
    }

    /// The removal's heads, once resolved.
    pub(super) fn removed_heads(before: Option<Before>, removed: &[FaceRef]) -> Option<Before> {
        let mut b = before?;
        if let Act::RemoveMany { heads, removed: n, .. } = &mut b.act {
            *heads = decisions::heads(removed.iter().take(decisions::MOVED_HEADS).copied());
            *n = removed.len();
        }
        Some(b)
    }

    /// *Export…*: the named groups as they are now go to the record's
    /// thread with the export, which answers `reply` (nothing here waits).
    pub(super) fn export_record(&mut self, into: &std::path::Path, reply: crossbeam_channel::Sender<std::io::Result<(std::path::PathBuf, usize)>>) {
        let snapshot = self.shared.subjects.lock().clone();
        let counts = |sid: &str| snapshot.group(&GroupId::Subject(crate::model::subjects::SubjectId(sid.to_string()))).map(|g| (g.photos, g.bursts, g.days));
        let libs = [Kind::Person, Kind::Fursuit].into_iter().filter_map(|k| self.record_lib(k).map(|l| (k, l))).collect();
        let groups = crate::model::group_export::Snapshot::of(&self.people.file, counts, libs);
        self.decisions.export_to(into, Some(groups), reply);
    }

    /// Naming a group (the tab, a group's page).
    pub(super) fn before_name_group(&mut self, id: &GroupId, name: &str) -> Option<Before> {
        if !self.decisions.is_on() {
            return None;
        }
        let (kind, side, _) = self.record_side(id, &[])?;
        Some(Before { kind, act: Act::Name { head: None, name: Some(name.to_string()), group: Some(side), from: None }, answered: None })
    }

    /// The subject a box is in, as a side without the box.
    fn box_side(&self, kind: Kind, face: &FaceRef) -> Option<Side> {
        let s = self.people.file.member_of(kind, face)?;
        let rest: Vec<FaceRef> = s.members.iter().filter(|m| *m != face).copied().collect();
        let snapshot = self.shared.subjects.lock().clone();
        Some(Side::new(rest.len(), s.name.clone(), Some(s.id.0.clone()), self.clearest(kind, rest)).counted(snapshot.group(&GroupId::Subject(s.id.clone()))))
    }

    /// A label edit in the viewer, before it is applied: what it changes
    /// and the subject the box leaves.
    pub(super) fn before_label(&mut self, edit: &LabelEdit) -> Option<(Kind, Option<Side>)> {
        if !self.decisions.is_on() {
            return None;
        }
        let (kind, face) = match edit {
            LabelEdit::Name { face, kind, .. } | LabelEdit::Add { face, kind, .. } | LabelEdit::Remove { face, kind } => (*kind, *face),
            LabelEdit::Kind { face, from, .. } => (*from, *face),
        };
        Some((kind, self.box_side(kind, &face)))
    }

    /// A label edit applied: its entry, the subject joined read after the
    /// change.
    pub(super) fn after_label(&mut self, edit: &LabelEdit, before: Option<(Kind, Option<Side>)>) -> Option<Before> {
        let (kind, left) = before?;
        let named = |kind: Kind, face: &FaceRef, to: &NameTo| -> Option<String> {
            match to {
                NameTo::Unnamed => None,
                _ => self.people.file.member_of(kind, face).and_then(|s| s.name.clone()),
            }
        };
        let act = match edit {
            LabelEdit::Name { face, kind, to } => {
                let name = named(*kind, face, to);
                let group = if name.is_some() { self.box_side(*kind, face) } else { None };
                Act::Name { head: Some(decisions::Head(*face)), name, group, from: if matches!(to, NameTo::Unnamed) { left } else { None } }
            }
            LabelEdit::Kind { face, to, name, .. } => Act::KindChange { head: decisions::Head(*face), to: *to, name: name.as_ref().and_then(|n| named(*to, face, n)) },
            LabelEdit::Add { face, kind, name } => Act::Draw { head: decisions::Head(*face), name: name.as_ref().and_then(|n| named(*kind, face, n)) },
            LabelEdit::Remove { face, .. } => Act::Remove { head: decisions::Head(*face) },
        };
        Some(Before { kind, act, answered: None })
    }

    /// An answer taken back in a review (`answer(None)`): the entry of the
    /// answer it takes back, if one was written.
    pub(super) fn answered_entry(&mut self, faces: &[FaceRef]) -> Option<(String, Kind)> {
        self.answered.remove(&sorted(faces.to_vec()))
    }
}

fn sorted(mut faces: Vec<FaceRef>) -> Vec<FaceRef> {
    faces.sort_by_key(|f| (f.key.0, f.thousandths()));
    faces
}
