//! The search page's link to the search engine (`crate::search::Engine`).
//!
//! The page talks to the engine through [`Link`], which has the engine's
//! calls and nothing else; the engine answers on the bus
//! (`BgEvent::SearchStatus`, `SearchResults`, `ModelDownload`).
//!
//! With `PP_SEARCH_FAKE` set, a stand-in takes the engine's place for
//! tests and screenshots: it answers queries with made-up hits from the
//! library index the calendar already holds (scored by a hash of the words
//! and the file name, so a query always finds the same photos), downloads
//! and indexes on a timer, and can be put in any state the page must show
//! (`search fake …` in a test script).  It sends the same events as the
//! engine, so the page can't tell them apart.  It reads no photo and
//! writes no file but its own record of corrections
//! (`decisions-stand-in.jsonl`, never the real record's file: run it with
//! a throwaway `HOME`).  `PP_SEARCH_FAKE=indexed` starts it with the library
//! indexed.  It answers phase 2's questions too: made-up Things (the
//! vocabulary's objects and scenes, with counts that grow with the index),
//! the query's dates and `-word`s applied to its hits (dates alone answer
//! with every shot from them, oldest first), and, for an animal word, a
//! share of the hits handed back as fursuits left out.
//!
//! Phase 3 adds people (`crate::model::subjects`): the groups of faces
//! found, a group's faces and suggestions, naming, merging, moving faces,
//! answers, the faces switch and model, and a search within a person's
//! photos.  The stand-in answers them too (`super::people::FakePeople`),
//! with grey silhouettes for faces, never a real one.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as Span, NaiveDateTime};
use parking_lot::Mutex;

use super::fursuits::{self, FakeFursuits};
use super::people::{Face, FaceRef, FakePeople, Framing, GroupId, Kind, Subjects};
use crate::model::decisions::{self, Act, Entry, How, Recorder, Via};
use crate::model::subjects::{LabelEdit, LabelTarget};
use crate::bus::{BgEvent, Bus};
use crate::ml::download::DownloadState;
use crate::ml::models::{self, ModelId};
use crate::ml::judge::{JudgeChoice, JudgeState, JudgeStatus};
use crate::ml::{CardFailure, Device, DeviceStatus, SelfCheck};
use crate::model::library_index::{self, LibraryIndex};
use crate::model::record::FileType;
use crate::search::index::Key;
use crate::search::{Engine, FacesStatus, Hit, PassKind, PassProgress, Status, Stopped, Thing, vocab};
use crate::util;

pub enum Link {
    /// Shared with the crop workers (`Cropper`).
    Engine(Arc<Engine>),
    Fake(Fake),
}

impl Link {
    /// The engine, or the stand-in when `PP_SEARCH_FAKE` is set.
    pub fn new(bus: Bus, device: Device, index_exports: bool, find_faces: bool, face_model: crate::ml::models::ModelId, find_heads: bool) -> Link {
        match std::env::var("PP_SEARCH_FAKE") {
            Ok(v) => Link::Fake(Fake::new(bus, device, index_exports, v == "indexed", find_faces, face_model, find_heads)),
            Err(_) => Link::Engine(Arc::new(Engine::new(bus, device, index_exports, find_faces, face_model, find_heads))),
        }
    }

    /// The record of the user's corrections (`model::decisions`): the
    /// engine's, beside the subjects file; the stand-in's, in a file of
    /// its own.
    pub fn decisions(&self) -> Recorder {
        match self {
            Link::Engine(e) => e.decisions(),
            Link::Fake(f) => f.decisions.clone(),
        }
    }

    pub fn status(&self) -> Status {
        match self {
            Link::Engine(e) => e.status(),
            Link::Fake(f) => f.state.lock().status.clone(),
        }
    }

    /// *Things in your photos* as last derived (`BgEvent::SearchThings`
    /// carries each change).
    pub fn things(&self) -> Arc<Vec<Thing>> {
        match self {
            Link::Engine(e) => e.things(),
            Link::Fake(f) => f.state.lock().things.clone(),
        }
    }

    pub fn on_library(&self, index: Arc<LibraryIndex>) {
        match self {
            Link::Engine(e) => e.on_library(index),
            Link::Fake(f) => f.on_library(index),
        }
    }

    /// The first index, with the models it needs: search's, and the face
    /// detector and model when faces are found in the same pass
    /// (`models::models_for`).
    pub fn start_first_index(&self, models: &[ModelId]) {
        match self {
            Link::Engine(e) => e.start_first_index(models),
            Link::Fake(f) => f.start_first_index(models),
        }
    }

    pub fn stop_indexing(&self) {
        match self {
            Link::Engine(e) => e.stop_indexing(),
            Link::Fake(f) => f.change(|s| {
                s.status.pass = None;
                s.status.stopped = Some(Stopped::Cancelled);
            }),
        }
    }

    pub fn cancel_downloads(&self) {
        match self {
            Link::Engine(e) => e.cancel_downloads(),
            Link::Fake(f) => f.cancel_downloads(),
        }
    }

    pub fn try_card_again(&self) {
        match self {
            Link::Engine(e) => e.try_card_again(),
            Link::Fake(f) => f.try_card_again(),
        }
    }

    pub fn set_device(&self, d: Device) {
        match self {
            Link::Engine(e) => e.set_device(d),
            Link::Fake(f) => f.change(|s| {
                s.status.device = DeviceStatus { chosen: d, name: (d == Device::Gpu).then(|| FAKE_CARD.to_string()), self_check: SelfCheck::NotRun };
                // The processor needs no card: a card failure no longer
                // holds the work up.
                if d == Device::Cpu && matches!(s.status.stopped, Some(Stopped::Card(_))) {
                    s.status.stopped = None;
                    s.resume();
                }
                // A head scan the card stopped: on the processor, it asks
                // first (the user chose the processor; the scan didn't).
                if d == Device::Cpu && matches!(s.status.heads.step, fursuits::Step::Failed(_)) {
                    s.status.heads.step = fursuits::Step::Idle;
                    s.start_heads(fursuits::Why::Resume);
                }
            }),
        }
    }

    pub fn set_index_exports(&self, on: bool) {
        match self {
            Link::Engine(e) => e.set_index_exports(on),
            Link::Fake(f) => f.change(|s| s.status.index_exports = on),
        }
    }

    pub fn clear_index(&self) {
        match self {
            Link::Engine(e) => e.clear_index(),
            Link::Fake(f) => f.change(|s| {
                s.status.indexed_shots = 0;
                s.status.first_index_asked = false;
                s.status.pass = None;
                s.status.stopped = None;
            }),
        }
    }

    pub fn rebuild_index(&self) {
        match self {
            Link::Engine(e) => e.rebuild_index(),
            Link::Fake(f) => f.change(|s| {
                s.status.indexed_shots = 0;
                s.status.pass = Some(PassProgress { kind: PassKind::Rebuild, done: 0, total: s.status.library_shots, oldest_day: None });
            }),
        }
    }

    pub fn delete_models(&self) {
        match self {
            Link::Engine(e) => e.delete_models(),
            Link::Fake(f) => f.change(|s| {
                s.status.models_installed = false;
                s.status.pass = None;
            }),
        }
    }

    /// A search within the photos of `subjects` (all of them together): no
    /// words, clearest first; words, best first among those photos.
    pub fn query_in(&self, seq: u64, text: &str, closeness: f32, subjects: &[GroupId]) {
        match self {
            Link::Engine(e) => e.query(seq, text, closeness, subjects),
            Link::Fake(f) => f.query(seq, text, closeness, subjects),
        }
    }

    // ── people ──────────────────────────────────────────────────────────────

    /// Every group of every kind, as last reported (a cheap clone).
    pub fn subjects(&self) -> Arc<Subjects> {
        match self {
            Link::Engine(e) => e.subjects(),
            Link::Fake(f) => f.state.lock().subjects.clone(),
        }
    }

    /// Name a group (a cluster becomes a subject), or rename a subject.
    pub fn name_group(&self, id: &GroupId, name: &str) {
        match self {
            Link::Engine(e) => match id {
                GroupId::Subject(s) => e.rename(s.clone(), name.to_string()),
                GroupId::Cluster(_) => e.name_group(id.clone(), name.to_string()),
            },
            Link::Fake(f) => {
                let before = f.before(|s| Act::Name { head: None, name: Some(name.to_string()), group: Some(s.side(id, &[])), from: None }, id);
                let done = f.edit(id, |p, _| p.name_group(id, name).map(|_| ()), |z, _| z.name_group(id, name).map(|_| ()));
                f.record(done, before, Via::Hand(decisions::At::Name), None);
            }
        }
    }

    /// Merge `others` into `into` (one kind only; refused otherwise).
    /// `via`: how the page came to it, for the decision record.
    pub fn merge(&self, into: &GroupId, others: &[GroupId], via: Via) {
        match self {
            Link::Engine(e) => e.merge(into.clone(), others.to_vec(), via),
            Link::Fake(f) => {
                let before = f.before(
                    |s| {
                        let groups = std::iter::once(into).chain(others.iter().filter(|o| *o != into)).map(|g| s.side(g, &[])).collect();
                        Act::Merge { groups }
                    },
                    into,
                );
                let done = f.edit(
                    into,
                    |p, subjects| p.merge(into, others, |g: &GroupId| subjects.group(g).map(|x| x.kind)),
                    |z, subjects| z.merge(into, others, |g: &GroupId| subjects.group(g).map(|x| x.kind)),
                );
                f.record(done, before, via, None);
            }
        }
    }

    pub fn set_hidden(&self, id: &GroupId, hidden: bool) {
        match self {
            Link::Engine(e) => e.set_hidden(id.clone(), hidden),
            Link::Fake(f) => {
                f.edit(
                    id,
                    |p, _| {
                        p.set_hidden(id, hidden);
                        Ok(())
                    },
                    |z, _| {
                        z.set_hidden(id, hidden);
                        Ok(())
                    },
                );
            }
        }
    }

    /// Forget a named subject: the name and what was confirmed.
    pub fn forget(&self, id: &GroupId) {
        match self {
            Link::Engine(e) => {
                if let GroupId::Subject(s) = id {
                    e.forget(s.clone());
                }
            }
            Link::Fake(f) => {
                f.edit(
                    id,
                    |p, _| {
                        p.forget(id);
                        Ok(())
                    },
                    |z, _| {
                        z.forget(id);
                        Ok(())
                    },
                );
            }
        }
    }

    /// Forget every face found and every name given; fursuits (and pets)
    /// are not touched.
    pub fn forget_all_people(&self) {
        match self {
            Link::Engine(e) => e.forget_kind(Kind::Person),
            Link::Fake(f) => f.change(|s| {
                s.people.forget_all();
                s.labels.forget_kind(Kind::Person);
                s.edits.retain(|k| k.0 != Some(Kind::Person));
                s.answered.retain(|_, (_, k)| *k != Kind::Person);
                f.decisions.forget_kind(Kind::Person);
                s.status.faces.looked_at = 0;
                if s.status.pass.as_ref().is_some_and(|p| p.kind == PassKind::Faces) {
                    s.status.pass = None;
                }
                s.refresh_people();
            }),
        }
    }

    /// A group's faces, clearest first (`BgEvent::SubjectFaces`).
    pub fn faces(&self, seq: u64, id: &GroupId) {
        match self {
            Link::Engine(e) => e.faces(seq, id.clone()),
            Link::Fake(f) => {
                let faces = {
                    let s = f.state.lock();
                    Arc::new(if s.fursuits.owns(id) { s.fursuits.faces(id) } else { s.people.faces(id) })
                };
                let _ = f.bus.unbounded_send(BgEvent::SubjectFaces { seq, group: id.clone(), faces });
            }
        }
    }

    /// Move faces out of `from`: to `to`, or a new group of their own.
    pub fn move_faces(&self, from: &GroupId, faces: &[FaceRef], to: Option<&GroupId>, via: Via) {
        match self {
            Link::Engine(e) => e.move_faces(from.clone(), faces.to_vec(), to.cloned(), via),
            Link::Fake(f) => {
                let before = f.before(
                    |s| {
                        let heads = decisions::heads(faces.iter().take(decisions::MOVED_HEADS).copied());
                        match to {
                            None => Act::Split { from: s.side(from, &[]), heads, moved: faces.len() },
                            Some(t) => Act::Move { from: s.side(from, &[]), to: s.side(t, &[]), heads, moved: faces.len() },
                        }
                    },
                    from,
                );
                let done = f.edit(from, |p, _| p.move_faces(from, faces, to), |z, _| z.move_faces(from, faces, to));
                f.record(done, before, via, None);
            }
        }
    }

    /// Yes, No (not this one: out, and never suggested again), or take the
    /// answer back (`None`).
    pub fn answer(&self, id: &GroupId, faces: &[FaceRef], yes: Option<bool>, via: Via) {
        match self {
            Link::Engine(e) => e.answer(id.clone(), faces.to_vec(), yes, via),
            Link::Fake(f) => {
                let before = yes.and_then(|y| f.before(|s| Act::Answer { yes: y, group: s.side(id, &[]), heads: decisions::heads(faces.iter().copied()) }, id));
                let done = f.edit(
                    id,
                    |p, _| {
                        p.answer(id, faces, yes);
                        Ok(())
                    },
                    |z, _| {
                        z.answer(id, faces, yes);
                        Ok(())
                    },
                );
                match yes {
                    Some(_) => f.record(done, before, via, Some(faces)),
                    // Taking an answer back is an undo of its entry.
                    None => {
                        let undone = f.state.lock().answered.remove(&fake_sorted(faces));
                        f.record_undo(undone, done.is_some());
                    }
                }
            }
        }
    }

    /// What the review offers for a group (`BgEvent::Suggestions`).
    pub fn suggestions(&self, seq: u64, id: &GroupId) {
        match self {
            Link::Engine(e) => e.suggestions(seq, id.clone()),
            Link::Fake(f) => {
                let list = {
                    let mut s = f.state.lock();
                    // The judge counts only while it can run.
                    s.fursuits.judge_on = super::fursuit_judge::active(&s.status);
                    Arc::new(if s.fursuits.owns(id) { s.fursuits.suggestions(id) } else { s.people.suggestions(id) })
                };
                let _ = f.bus.unbounded_send(BgEvent::Suggestions { seq, group: id.clone(), list });
                f.start_judging(seq, id);
            }
        }
    }

    /// *Find more*: look again for `id` from everything confirmed, then
    /// send what the review offers.  Strict for fursuits (the next few
    /// under the cut-off; nothing new means nothing more is close enough);
    /// for people the same as `suggestions`.
    pub fn find_more(&self, seq: u64, id: &GroupId) {
        match self {
            Link::Engine(e) => e.find_more(seq, id.clone()),
            Link::Fake(f) => {
                {
                    let mut s = f.state.lock();
                    if s.fursuits.owns(id) {
                        s.fursuits.find_more(id);
                        s.refresh_people();
                    }
                }
                f.publish_people();
                self.suggestions(seq, id);
            }
        }
    }

    /// The viewer's labels for a photo (`BgEvent::PhotoLabels`).  The
    /// stand-in finds library photos by path only (its keys are made up),
    /// so a working folder's photo is never in its library.
    pub fn photo_labels(&self, seq: u64, target: LabelTarget) {
        match self {
            Link::Engine(e) => e.photo_labels(seq, target),
            Link::Fake(f) => {
                let labels = {
                    let s = f.state.lock();
                    let key = match &target {
                        LabelTarget::Key(k) => Some(*k).filter(|k| s.people.has_shot(k)),
                        LabelTarget::Files { jpg, raw } => jpg.iter().chain(raw.iter()).find_map(|p| s.people.key_of_path(p)),
                    };
                    // Nothing indexed yet: nothing looked for.
                    let key = key.filter(|_| s.status.indexed_shots > 0);
                    match key {
                        Some(k) => s.labels.labels(k, s.fake_label_base(&k), s.drawn_pending()),
                        None => crate::model::subjects::PhotoLabels::default(),
                    }
                };
                let _ = f.bus.unbounded_send(BgEvent::PhotoLabels { seq, labels: Arc::new(labels) });
            }
        }
    }

    /// A change on a box of the viewer's labels (one undo step).
    pub fn label(&self, edit: LabelEdit) {
        match self {
            Link::Engine(e) => e.label(edit),
            Link::Fake(f) => {
                let mut done = None;
                f.change(|s| {
                    let key = edit.face().key;
                    let base = s.fake_label_base(&key);
                    let pending = s.drawn_pending().is_some();
                    let subjects = s.subjects.clone();
                    match s.labels.edit(&edit, base, &subjects, pending) {
                        Ok(what) => {
                            eprintln!("[labels] {what}");
                            s.edits.push((None, None));
                            done = Some(fake_label_act(&edit, &subjects));
                        }
                        Err(e) => eprintln!("[labels] refused: {e}"),
                    }
                });
                if let Some((kind, act)) = done.filter(|_| f.decisions.is_on()) {
                    f.record(Some(kind), Some((kind, act)), Via::Hand(decisions::At::Viewer), None);
                }
            }
        }
    }

    /// A fursuit group's split check (`BgEvent::GroupCheck`): its heads
    /// farthest from the rest first, heads seen in one photo, a proposed
    /// split.  Nothing moves.
    pub fn group_check(&self, seq: u64, id: &GroupId) {
        match self {
            Link::Engine(e) => e.group_check(seq, id.clone()),
            Link::Fake(f) => {
                let (far, pairs, split) = f.state.lock().fursuits.check(id).unwrap_or_default();
                let _ = f.bus.unbounded_send(BgEvent::GroupCheck { seq, group: id.clone(), far: Arc::new(far), pairs: Arc::new(pairs), split: split.map(Arc::new) });
            }
        }
    }

    /// *Possibly the same fursuit*: pairs of fursuit groups to merge or
    /// tell apart (`BgEvent::MergeSuggestions`, then with `judge` and a
    /// judge that can run, `BgEvent::MergeJudged`).  `focus`: one group's
    /// pairs (its page's *Similar groups*).
    pub fn merge_suggestions(&self, seq: u64, focus: Option<&GroupId>, min_photos: usize, judge: bool) {
        match self {
            Link::Engine(e) => e.merge_suggestions(seq, focus.cloned(), min_photos, judge),
            Link::Fake(f) => f.merge_suggestions(seq, focus, min_photos, judge),
        }
    }

    /// "Not the same": the two groups are different fursuits; never
    /// offered together again (one undo step).
    pub fn not_same(&self, a: &GroupId, b: &GroupId, a_heads: &[FaceRef], b_heads: &[FaceRef], via: Via) {
        match self {
            Link::Engine(e) => e.not_same(a.clone(), b.clone(), a_heads.to_vec(), b_heads.to_vec(), via),
            Link::Fake(f) => {
                let before = f.before(|s| Act::NotSame { groups: vec![s.side(a, a_heads), s.side(b, b_heads)] }, a);
                let done = f.edit(a, |_, _| Err("people have no merge suggestions here".into()), |z, _| z.not_same(a, b));
                f.record(done, before, via, None);
            }
        }
    }

    /// Take back the last edit to people (`BgEvent::SubjectsUndone`, then
    /// a fresh `Subjects`).
    pub fn undo_subjects(&self) {
        match self {
            Link::Engine(e) => e.undo(),
            Link::Fake(f) => {
                let mut undone = None;
                let what = {
                    let mut s = f.state.lock();
                    // The newest edit, whichever kind it changed.
                    let (kind, recorded) = s.edits.pop().unwrap_or((Some(Kind::Person), None));
                    let what = match kind {
                        Some(Kind::Fursuit) => s.fursuits.undo(),
                        // A change made in the viewer's labels.
                        None => s.labels.undo(),
                        _ => s.people.undo(),
                    };
                    s.refresh_people();
                    if what.is_some() {
                        undone = recorded;
                    }
                    what
                };
                f.record_undo(undone, false);
                if let Some(w) = what {
                    let _ = f.bus.unbounded_send(BgEvent::SubjectsUndone(w));
                }
                f.publish_people();
            }
        }
    }

    /// Preferences ▸ *Find people's faces*.
    pub fn set_find_faces(&self, on: bool) {
        match self {
            Link::Engine(e) => e.set_find_faces(on),
            Link::Fake(f) => f.change(|s| {
                s.status.faces.on = on;
                if !on && s.status.pass.as_ref().is_some_and(|p| p.kind == PassKind::Faces) {
                    s.status.pass = None;
                }
            }),
        }
    }

    /// Another face model: the faces found are recognised again with it
    /// (names, confirmed photos and answers are kept).
    pub fn set_face_model(&self, model: ModelId) {
        match self {
            Link::Engine(e) => e.set_face_model(model),
            Link::Fake(f) => f.change(|s| {
                let changed = s.status.faces.model != Some(model);
                s.status.faces.model = Some(model);
                s.status.faces.installed = true;
                if changed && s.status.faces.looked_at > 0 {
                    let total = s.people.photos_with_faces().max(1);
                    s.status.faces.unrecognised = total;
                    s.status.pass = Some(PassProgress { kind: PassKind::Faces, done: 0, total, oldest_day: None });
                }
            }),
        }
    }

    /// Find faces in the library indexed before faces were looked for
    /// (only when asked: it reads every photo again), with the face model
    /// set by [`Link::set_face_model`].
    pub fn find_faces(&self) {
        match self {
            Link::Engine(e) => e.find_faces(),
            Link::Fake(f) => f.change(|s| {
                s.status.faces.on = true;
                s.status.faces.installed = true;
                let total = s.status.indexed_shots;
                s.status.faces.unrecognised = 0;
                s.status.pass = Some(PassProgress { kind: PassKind::Faces, done: s.people.looked_at, total, oldest_day: None });
            }),
        }
    }

    // ── fursuit heads (decision 11) ──

    /// Preferences ▸ *Find fursuit heads* (and the download dialog's tick).
    pub fn set_find_heads(&self, on: bool) {
        match self {
            Link::Engine(e) => e.set_find_heads(on),
            Link::Fake(f) => f.change(|s| {
                s.status.heads.on = on;
                if !on {
                    s.status.heads.step = fursuits::Step::Idle;
                }
            }),
        }
    }

    /// Look for heads in the fursuit photos not looked at yet (asked from
    /// the Fursuits tab or Preferences, after the cost was shown): the head
    /// models first if missing, then the scan, on either device.
    pub fn find_heads(&self) {
        match self {
            Link::Engine(e) => e.find_heads(),
            Link::Fake(f) => f.change(|s| {
                let h = &mut s.status.heads;
                h.on = true;
                h.models_installed = true;
                h.skipped = 0;
                let total = h.unscanned();
                h.step = fursuits::Step::Scanning { why: fursuits::Why::CatchUp, done: 0, total };
            }),
        }
    }

    /// The answer to a head scan waiting on the processor: start it, or
    /// skip it (its photos wait for a later catch-up).
    pub fn answer_heads(&self, start: bool) {
        match self {
            Link::Engine(e) => e.answer_heads(start),
            Link::Fake(f) => f.change(|s| {
                let h = &mut s.status.heads;
                let fursuits::Step::Asking(o) = h.step else { return };
                h.step = if start {
                    fursuits::Step::Scanning { why: o.why, done: 0, total: o.photos }
                } else {
                    h.skipped += o.photos;
                    fursuits::Step::Idle
                };
            }),
        }
    }

    /// × on a head scan: stop it, keeping what was found.
    pub fn stop_heads(&self) {
        match self {
            Link::Engine(e) => e.stop_heads(),
            Link::Fake(f) => f.change(|s| {
                if s.status.heads.scanning() {
                    s.status.heads.step = fursuits::Step::Idle;
                }
            }),
        }
    }

    /// Forget every fursuit head found and every fursuit name given;
    /// people are not touched.
    pub fn forget_all_fursuits(&self) {
        match self {
            Link::Engine(e) => e.forget_kind(Kind::Fursuit),
            Link::Fake(f) => f.change(|s| {
                s.fursuits.forget_all();
                s.labels.forget_kind(Kind::Fursuit);
                s.edits.retain(|k| k.0 != Some(Kind::Fursuit));
                s.answered.retain(|_, (_, k)| *k != Kind::Fursuit);
                f.decisions.forget_kind(Kind::Fursuit);
                s.status.heads.step = fursuits::Step::Idle;
                s.status.heads.skipped = 0;
                s.refresh_people();
            }),
        }
    }

    // ── the close-call judge (fursuit matching v2) ──

    /// Preferences ▸ Fursuits ▸ *Close-call judge*.  Off unloads it and
    /// drops a run.
    /// Preferences ▸ Fursuits ▸ *Grouping*: the fursuits are grouped again
    /// (names and answers stay).
    pub fn set_fursuit_grouping(&self, g: fursuits::Grouping) {
        match self {
            Link::Engine(e) => e.set_fursuit_grouping(g),
            Link::Fake(f) => f.change(|s| {
                s.status.heads.grouping = g;
                if s.fursuits.grouping != g {
                    s.fursuits.grouping = g;
                    s.fursuits.regroup_now();
                    s.refresh_people();
                }
            }),
        }
    }

    pub fn set_judge(&self, choice: JudgeChoice) {
        match self {
            Link::Engine(e) => e.set_judge(choice),
            Link::Fake(f) => f.change(|s| {
                s.status.judge.choice = choice;
                s.fursuits.judge_on = s.status.judge.installed();
                if !s.status.judge.installed() {
                    s.judge_run = None;
                    s.merge_run = None;
                    s.status.judge.loaded = false;
                    s.status.judge.state = JudgeState::Idle;
                }
                s.refresh_people();
            }),
        }
    }

    /// Download `choice`'s model alone (`BgEvent::ModelDownload`).
    pub fn download_judge(&self, choice: JudgeChoice) {
        match self {
            Link::Engine(e) => e.download_judge(choice),
            Link::Fake(f) => f.change(|s| {
                if let Some(m) = choice.model() {
                    s.downloads = vec![(m, 0, models::info(m).bytes(), DownloadState::Waiting)];
                }
            }),
        }
    }

    /// × on the judge's line: stop judging (what was judged is kept).
    pub fn stop_judging(&self) {
        match self {
            Link::Engine(e) => e.stop_judging(),
            Link::Fake(f) => f.change(|s| {
                s.judge_run = None;
                s.merge_run = None;
                s.status.judge.state = JudgeState::Idle;
            }),
        }
    }

    /// Delete `choice`'s model files (the judged pairs are kept).
    pub fn delete_judge(&self, choice: JudgeChoice) {
        match self {
            Link::Engine(e) => e.delete_judge(choice),
            Link::Fake(f) => f.change(|s| {
                s.status.judge.downloaded.retain(|c| *c != choice);
                if s.status.judge.choice == choice {
                    s.status.judge.choice = JudgeChoice::Off;
                    s.judge_run = None;
                    s.merge_run = None;
                    s.status.judge.state = JudgeState::Idle;
                }
                s.status.judge.loaded = false;
                s.fursuits.judge_on = false;
                s.refresh_people();
            }),
        }
    }

    /// Stage 1's catch-up: the heads found before fursuits were told apart
    /// by the search model get its vectors (asked for, never automatic).
    pub fn catch_up_head_vectors(&self) {
        match self {
            Link::Engine(e) => e.catch_up_head_vectors(),
            Link::Fake(f) => f.change(|s| {
                let total = s.status.heads.vectors_missing;
                if total > 0 && !s.status.heads.scanning() {
                    s.status.heads.step = fursuits::Step::Scanning { why: fursuits::Why::Vectors, done: 0, total };
                }
            }),
        }
    }

    /// What draws crops: on a worker, never the UI thread.
    pub fn cropper(&self) -> Cropper {
        match self {
            Link::Engine(e) => Cropper { engine: Some(e.clone()), fake: None },
            Link::Fake(f) => Cropper { engine: None, fake: Some(f.state.clone()) },
        }
    }

    /// Thumbnails waiting: the pass steps back while there are any.
    pub fn set_busy(&self, pending: usize) {
        if let Link::Engine(e) = self {
            e.set_busy(pending);
        }
    }

    pub fn shutdown(&self) {
        match self {
            Link::Engine(e) => {
                // The record's last entries first (a moment at most: one
                // local append each).
                e.flush_decisions(Duration::from_secs(2));
                e.shutdown();
            }
            Link::Fake(f) => f.decisions.flush(),
        }
    }

    /// Put the stand-in in a state for a test (`search fake …`); the real
    /// engine ignores it.
    pub fn fake(&self, what: &str) {
        match self {
            Link::Fake(f) => f.set(what),
            Link::Engine(_) => eprintln!("[search] fake {what:?} ignored: the real engine is running (set PP_SEARCH_FAKE)"),
        }
    }

    pub fn is_fake(&self) -> bool {
        matches!(self, Link::Fake(_))
    }
}

// ── the stand-in ────────────────────────────────────────────────────────────

const FAKE_CARD: &str = "Radeon graphics";
/// How often the stand-in moves downloads and indexing on.
const TICK: Duration = Duration::from_millis(250);

pub struct Fake {
    bus: Bus,
    state: Arc<Mutex<FakeState>>,
    /// The stand-in's record of corrections: `decisions-stand-in.jsonl`,
    /// never the real record.
    decisions: Recorder,
}

struct FakeState {
    status: Status,
    /// Each model's download: done, total, state.
    downloads: Vec<(ModelId, u64, u64, DownloadState)>,
    /// The next download ends this way (`search fake mismatch|offline`).
    fail_next: Option<DownloadState>,
    /// Bytes a second the downloads come at (`search fake download-rate`),
    /// by the clock; else a twelfth of the model a tick.
    download_rate: Option<u64>,
    /// When the downloads last moved on (for `download_rate`).
    stepped_at: Option<std::time::Instant>,
    /// Hold a running pass where it is (screenshots); downloads stall.
    paused: bool,
    index: Option<Arc<LibraryIndex>>,
    /// Made-up Things, once something is indexed.
    things: Arc<Vec<Thing>>,
    /// The ticker thread is running.
    ticking: bool,
    /// Indexed in full once the library's size is known.
    start_indexed: bool,
    /// Made-up people, from the library's shots (their status is
    /// `status.faces`).
    people: FakePeople,
    /// Made-up fursuits on the same shots (their status is `status.heads`).
    fursuits: FakeFursuits,
    /// Which kind each edit changed (`None`: the viewer's labels), newest
    /// last: Ctrl+Z takes back the newest, whichever kind; with the
    /// record's entry for it (its id and kind), when one was written.
    edits: Vec<(Option<Kind>, Option<(String, Kind)>)>,
    /// Review answers recorded, by their heads (an answer taken back
    /// names its entry).
    answered: HashMap<Vec<FaceRef>, (String, Kind)>,
    /// The viewer's labels as the stand-in keeps them.
    labels: super::viewer_labels::FakeLabels,
    subjects: Arc<Subjects>,
    /// The generation of the groups last sent (sent again on a change).
    sent_people: Option<u64>,
    /// The stand-in judge's run over a fursuit's close calls.
    judge_run: Option<JudgeRun>,
    /// ... and over pairs of groups that may be one suit.
    merge_run: Option<MergeRun>,
    /// Ticks the judge has been loading (it is loaded after a few).
    loading: u32,
    /// Pretend another step holds the card (`judge-wait`).
    card_held: bool,
}

/// The stand-in judge's work for one request.
struct JudgeRun {
    seq: u64,
    group: GroupId,
    todo: Vec<Vec<Face>>,
    done: usize,
    total: usize,
    beyond: usize,
}

/// The stand-in judge comparing pairs of groups for one request.
struct MergeRun {
    seq: u64,
    todo: Vec<crate::search::merging::GroupPair>,
    done: usize,
    total: usize,
    beyond: usize,
}

/// A judged event waiting to be sent.
struct JudgeEvent {
    seq: u64,
    group: GroupId,
    offered: Vec<Vec<Face>>,
    left_out: Vec<Vec<Face>>,
    done: usize,
    total: usize,
    beyond: usize,
}

impl FakeState {
    /// Carry on with what was stopped.
    fn resume(&mut self) {
        let s = &mut self.status;
        if s.pass.is_none() && s.first_index_asked && s.indexed_shots < s.library_shots {
            s.pass = Some(PassProgress { kind: PassKind::FirstIndex, done: s.indexed_shots, total: s.library_shots, oldest_day: None });
        }
        if s.pass.is_none() && s.waiting > 0 {
            s.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: s.waiting, oldest_day: None });
            s.waiting = 0;
        }
    }

    fn busy(&self) -> bool {
        let heads = self.status.heads.scanning() && self.status.stopped.is_none();
        let judging = self.judge_run.is_some() || self.merge_run.is_some();
        !self.paused && (self.downloads.iter().any(|d| !d.3.is_final()) || (self.status.pass.is_some() && self.status.stopped.is_none()) || heads || judging)
    }

    /// Something else holds the card: a pass, the head step (or a test
    /// says so).
    fn card_in_use(&self) -> bool {
        self.card_held || (self.status.stopped.is_none() && (self.status.pass.is_some() || self.status.heads.scanning()))
    }

    /// One tick of the judge: wait for the card, load, then judge one
    /// close call.  The verdict, if one was reached.
    fn step_judge(&mut self) -> Option<JudgeEvent> {
        self.judge_run.as_ref()?;
        if self.card_in_use() {
            self.status.judge.state = JudgeState::WaitingForCard;
            return None;
        }
        if !self.status.judge.loaded {
            self.status.judge.state = JudgeState::Loading;
            self.loading += 1;
            if self.loading >= 4 {
                self.status.judge.loaded = true;
                self.loading = 0;
            }
            return None;
        }
        let run = self.judge_run.as_mut()?;
        let entry = (!run.todo.is_empty()).then(|| run.todo.remove(0));
        let (seq, group, total, beyond) = (run.seq, run.group.clone(), run.total, run.beyond);
        let mut ev = JudgeEvent { seq, group: group.clone(), offered: Vec::new(), left_out: Vec::new(), done: 0, total, beyond };
        if let Some(e) = entry {
            if self.fursuits.judge(&group, &e) {
                ev.offered.push(e);
            } else {
                ev.left_out.push(e);
            }
        }
        let run = self.judge_run.as_mut()?;
        run.done = (run.done + 1).min(run.total);
        ev.done = run.done;
        self.status.judge.state = JudgeState::Judging { group: group.clone(), done: run.done, total: run.total };
        if run.done >= run.total {
            self.judge_run = None;
            self.status.judge.state = JudgeState::Idle;
        }
        self.refresh_people();
        Some(ev)
    }

    /// The boxes the stand-in found on a shot: its person's face and its
    /// fursuit heads, with their groups' names.
    fn fake_label_base(&self, key: &Key) -> Vec<crate::model::subjects::LabelBox> {
        use crate::model::subjects::LabelBox;
        let subject = |g: &GroupId| match g {
            GroupId::Subject(s) => Some(s.clone()),
            GroupId::Cluster(_) => None,
        };
        let mut out = Vec::new();
        if let Some((face, g, name)) = self.people.found_on(key) {
            out.push(LabelBox { face, kind: Kind::Person, subject: name.as_ref().and(subject(&g)), name, drawn: false, pending: false });
        }
        for (face, g) in self.fursuits.found_on(key) {
            let (sid, name) = match g {
                Some((g, name)) => (name.as_ref().and(subject(&g)), name),
                None => (None, None),
            };
            out.push(LabelBox { face, kind: Kind::Fursuit, subject: sid, name, drawn: false, pending: false });
        }
        out
    }

    /// Why drawn boxes would wait here: the processor chosen, or the card
    /// failed (`PP_ML_FAKE_CARD`).
    fn drawn_pending(&self) -> Option<crate::model::subjects::PendingWhy> {
        use crate::model::subjects::PendingWhy;
        if self.status.device.chosen != Device::Gpu {
            Some(PendingWhy::ProcessorChosen)
        } else if matches!(self.status.stopped, Some(Stopped::Card(_))) {
            Some(PendingWhy::NoCard)
        } else {
            None
        }
    }

    /// One tick of the judge comparing pairs of groups: wait for the card,
    /// load, then compare one pair.  The verdict as it would be sent.
    fn step_merge_judge(&mut self) -> Option<BgEvent> {
        self.merge_run.as_ref()?;
        if self.card_in_use() {
            self.status.judge.state = JudgeState::WaitingForCard;
            return None;
        }
        if !self.status.judge.loaded {
            self.status.judge.state = JudgeState::Loading;
            self.loading += 1;
            if self.loading >= 4 {
                self.status.judge.loaded = true;
                self.loading = 0;
            }
            return None;
        }
        let run = self.merge_run.as_mut()?;
        let mut pair = (!run.todo.is_empty()).then(|| run.todo.remove(0))?;
        let v = self.fursuits.judge_pair(&pair);
        pair.verdict = Some(v);
        let run = self.merge_run.as_mut()?;
        run.done = (run.done + 1).min(run.total);
        let (offered, left_out) = if v.same() { (vec![pair], Vec::new()) } else { (Vec::new(), vec![pair]) };
        let ev = BgEvent::MergeJudged { seq: run.seq, offered: Arc::new(offered), left_out: Arc::new(left_out), done: run.done, total: run.total, beyond: run.beyond };
        self.status.judge.state = JudgeState::Comparing { done: run.done, total: run.total };
        if run.done >= run.total {
            self.merge_run = None;
            self.status.judge.state = JudgeState::Idle;
        }
        Some(ev)
    }

    /// The groups and the faces counts, after the people changed.
    fn refresh_people(&mut self) {
        let mut snap = self.people.snapshot();
        let f = &mut self.status.faces;
        f.found = snap.faces_found;
        f.named = self.people.named();
        f.looked_at = self.people.looked_at;
        // The fursuits' groups ride along, each carrying its kind.
        snap.groups.extend(self.fursuits.groups());
        snap.seen_together = self.fursuits.seen_together();
        snap.groups.sort_by(|a, b| (b.name.is_some(), b.photos).cmp(&(a.name.is_some(), a.photos)).then(a.cover.capture.cmp(&b.cover.capture)));
        snap.generation += self.fursuits.generation();
        self.refresh_heads();
        if *self.subjects != snap {
            self.subjects = Arc::new(snap);
        }
    }

    /// The head step's counts, from the fursuits stand-in and the index.
    fn refresh_heads(&mut self) {
        let h = &mut self.status.heads;
        h.tagged = self.fursuits.tagged_in(self.status.indexed_shots);
        h.scanned = self.fursuits.scanned.min(h.tagged);
        h.found = self.fursuits.found();
        h.named = self.fursuits.named();
        // Every head the stand-in finds has the search model's vectors.
        h.vectors = h.found;
        h.whitened = h.vectors >= crate::search::whiten::MIN_HEADS;
    }

    /// A head scan would start: on the card it does; on the processor it
    /// waits for the user's answer, with its cost (decision 11).
    fn start_heads(&mut self, why: fursuits::Why) {
        self.refresh_heads();
        let h = &mut self.status.heads;
        let photos = h.unscanned().saturating_sub(h.skipped);
        if !h.on || photos == 0 {
            return;
        }
        h.step = if fursuits::starts_unasked(self.status.device.chosen) {
            fursuits::Step::Scanning { why, done: 0, total: photos }
        } else {
            fursuits::Step::Asking(fursuits::Offer { photos, why })
        };
    }

    /// One tick of a head scan; false when none runs.
    fn step_heads(&mut self) -> bool {
        let fursuits::Step::Scanning { why, done, total } = self.status.heads.step else { return false };
        if why == fursuits::Why::Vectors {
            // The known heads get the search model's vectors.
            let done = (done + (total / 30).max(1)).min(total);
            self.status.heads.vectors_missing = total - done;
            self.status.heads.step = if done >= total { fursuits::Step::Idle } else { fursuits::Step::Scanning { why, done, total } };
            if done >= total && self.fursuits.groups().is_empty() {
                let n = self.fursuits.tagged_in(self.status.indexed_shots);
                self.fursuits.scan(n);
            }
            self.refresh_people();
            return true;
        }
        let base = self.fursuits.scanned.saturating_sub(done);
        let done = (done + (total / 30).max(1)).min(total);
        self.fursuits.scan(base + done);
        self.status.heads.step = if done >= total { fursuits::Step::Idle } else { fursuits::Step::Scanning { why, done, total } };
        self.refresh_people();
        true
    }
}

impl Fake {
    fn send_judged(&self, e: JudgeEvent) {
        let _ = self.bus.unbounded_send(BgEvent::Judged { seq: e.seq, group: e.group, offered: Arc::new(e.offered), left_out: Arc::new(e.left_out), done: e.done, total: e.total, beyond: e.beyond });
    }

    /// Merge suggestions, as the engine sends them: by distance alone, or
    /// with an active judge, the sure ones at once and the close ones
    /// compared a tick at a time (the cap in pairs of groups: three head
    /// pairs each).
    fn merge_suggestions(&self, seq: u64, focus: Option<&GroupId>, min_photos: usize, judge: bool) {
        let (first, judged) = {
            let mut s = self.state.lock();
            let active = judge && super::fursuit_judge::active(&s.status);
            let (sure, mut close) = s.fursuits.merge_pairs(min_photos, focus, active);
            if !active {
                (BgEvent::MergeSuggestions { seq, focus: focus.cloned(), list: Arc::new(sure), judging: false }, None)
            } else {
                let (now, beyond) = crate::search::merging::batch(close.len());
                close.truncate(now);
                let total = close.len();
                s.judge_run = None;
                s.merge_run = (total > 0).then(|| MergeRun { seq, todo: close, done: 0, total, beyond });
                if total > 0 {
                    s.status.judge.state = if s.card_in_use() {
                        JudgeState::WaitingForCard
                    } else if s.status.judge.loaded {
                        JudgeState::Comparing { done: 0, total }
                    } else {
                        JudgeState::Loading
                    };
                }
                let first = BgEvent::MergeJudged { seq, offered: Arc::new(Vec::new()), left_out: Arc::new(Vec::new()), done: 0, total, beyond };
                (BgEvent::MergeSuggestions { seq, focus: focus.cloned(), list: Arc::new(sure), judging: true }, Some(first))
            }
        };
        let _ = self.bus.unbounded_send(first);
        if let Some(j) = judged {
            let _ = self.bus.unbounded_send(j);
        }
        self.change(|_| {});
    }

    /// A fursuit's suggestions were asked for with a judge on: judge its
    /// close calls, nearest first; those judged before count at once.
    fn start_judging(&self, seq: u64, id: &GroupId) {
        let first = {
            let mut s = self.state.lock();
            // Only an active judge (chosen, in place, the card, not
            // stopped) is asked; otherwise the answer is as with it off.
            if !s.fursuits.owns(id) || !super::fursuit_judge::active(&s.status) || s.subjects.group(id).is_none_or(|g| g.name.is_none()) {
                return;
            }
            s.merge_run = None;
            let (left_out, mut todo) = s.fursuits.close_calls(id);
            // As the engine: the nearest close calls only, the rest left
            // for the next request.
            let beyond = todo.len().saturating_sub(crate::search::judging::MAX_CLOSE_CALLS);
            todo.truncate(crate::search::judging::MAX_CLOSE_CALLS);
            let total = todo.len();
            s.judge_run = (total > 0).then(|| JudgeRun { seq, group: id.clone(), todo, done: 0, total, beyond });
            if total > 0 {
                s.status.judge.state = if s.card_in_use() {
                    JudgeState::WaitingForCard
                } else if s.status.judge.loaded {
                    JudgeState::Judging { group: id.clone(), done: 0, total }
                } else {
                    JudgeState::Loading
                };
            }
            JudgeEvent { seq, group: id.clone(), offered: Vec::new(), left_out, done: 0, total, beyond }
        };
        self.send_judged(first);
        self.change(|_| {});
    }

    fn new(bus: Bus, device: Device, index_exports: bool, indexed: bool, find_faces: bool, face_model: ModelId, find_heads: bool) -> Fake {
        let mut status = Status {
            index_exports,
            device: DeviceStatus { chosen: device, name: (device == Device::Gpu).then(|| FAKE_CARD.to_string()), self_check: SelfCheck::NotRun },
            ..Status::default()
        };
        if indexed {
            status.models_installed = true;
            status.first_index_asked = true;
            status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
        }
        // A failing card for tests, as the engine reads it.
        if let Some(f) = crate::ml::fake_card_failure() {
            if device == Device::Gpu {
                if f == CardFailure::NoCard {
                    status.device.name = None;
                }
                status.stopped = Some(Stopped::Card(f));
            }
        }
        status.faces = FacesStatus { on: find_faces, model: Some(face_model), installed: indexed, ..FacesStatus::default() };
        let people = FakePeople::new(true);
        status.heads = fursuits::HeadsStatus { on: find_heads, models_installed: indexed, ..fursuits::HeadsStatus::default() };
        let state = FakeState {
            status,
            downloads: Vec::new(),
            fail_next: None,
            download_rate: None,
            stepped_at: None,
            paused: false,
            index: None,
            things: Arc::new(Vec::new()),
            ticking: false,
            start_indexed: indexed,
            people,
            fursuits: FakeFursuits::new(true),
            edits: Vec::new(),
            answered: HashMap::new(),
            labels: Default::default(),
            subjects: Arc::new(Subjects::default()),
            sent_people: None,
            judge_run: None,
            merge_run: None,
            loading: 0,
            card_held: false,
        };
        let decisions = Recorder::new(decisions::default_path().map(|p| p.with_file_name(decisions::STAND_IN_FILE_NAME)));
        let fake = Fake { bus, state: Arc::new(Mutex::new(state)), decisions };
        fake.publish(&fake.state.lock());
        fake.publish_people();
        fake
    }

    /// The groups and the faces status, as they are now.
    fn publish_people(&self) {
        let mut s = self.state.lock();
        s.sent_people = None;
        self.publish_people_if_changed(&mut s);
        self.keep_ticking(&mut s);
    }

    /// Change a group (an edit the page asked for) in the stand-in of its
    /// kind, then say so; a refusal is printed (the page checks first, as
    /// the engine does).  The kind changed, when an undo step was made.
    fn edit(
        &self,
        id: &GroupId,
        people: impl FnOnce(&mut FakePeople, &Subjects) -> Result<(), String>,
        fursuits: impl FnOnce(&mut FakeFursuits, &Subjects) -> Result<(), String>,
    ) -> Option<Kind> {
        // The counts in the status (named, found) change with the groups.
        let mut stepped = None;
        self.change(|s| {
            let snap = s.subjects.clone();
            let fursuit = s.fursuits.owns(id);
            let before = s.fursuits.undo_depth();
            let done = if fursuit { fursuits(&mut s.fursuits, &snap) } else { people(&mut s.people, &snap) };
            match done {
                Err(e) => eprintln!("[people] refused: {e}"),
                Ok(()) if fursuit && s.fursuits.undo_depth() > before => stepped = Some(Kind::Fursuit),
                Ok(()) if !fursuit => stepped = Some(Kind::Person),
                Ok(()) => {}
            }
            if let Some(k) = stepped {
                s.edits.push((Some(k), None));
            }
            s.refresh_people();
        });
        stepped
    }

    /// A decision's entry, measured before the edit (nothing while the
    /// record is off): what `act` makes of the stand-in's groups.
    fn before(&self, act: impl FnOnce(&FakeState) -> Act, id: &GroupId) -> Option<(Kind, Act)> {
        if !self.decisions.is_on() {
            return None;
        }
        let s = self.state.lock();
        let kind = if s.fursuits.owns(id) { Kind::Fursuit } else { Kind::Person };
        Some((kind, act(&s)))
    }

    /// Write a decision made (`done`: the edit's undo step was made), and
    /// tie it to that step so its undo can name it.
    fn record(&self, done: Option<Kind>, before: Option<(Kind, Act)>, via: Via, answered: Option<&[FaceRef]>) {
        let (Some(_), Some((kind, act))) = (done, before) else { return };
        let mut s = self.state.lock();
        let chosen = s.status.judge.choice;
        let judge = chosen.model().map(|m| (m.dir_name().to_string(), chosen.yes_cut()));
        let entry = Entry::new(kind, act, Some(How::of(via, judge)), Some(s.lib(kind)));
        if let Some(id) = self.decisions.record(entry) {
            if let Some(last) = s.edits.last_mut() {
                last.1 = Some((id.clone(), kind));
            }
            if let Some(faces) = answered {
                s.answered.insert(fake_sorted(faces), (id, kind));
            }
        }
    }

    /// An undo's entry, naming the one it takes back.  `step`: it made an
    /// undo step of its own (an answer taken back), which remembers it.
    fn record_undo(&self, undone: Option<(String, Kind)>, step: bool) {
        let Some((id, kind)) = undone else { return };
        if let Some(id) = self.decisions.record_undo((id, kind)) {
            if step {
                if let Some(last) = self.state.lock().edits.last_mut() {
                    last.1 = Some((id, kind));
                }
            }
        }
    }

    fn publish(&self, s: &FakeState) {
        let _ = self.bus.unbounded_send(BgEvent::SearchStatus(Arc::new(s.status.clone())));
    }

    /// Made-up Things for the start page: the vocabulary's objects and
    /// scenes with counts from a hash of each word, scaled to what is
    /// indexed; sent whenever the indexed count changes.
    fn publish_things(&self, s: &mut FakeState) {
        let indexed = s.status.indexed_shots;
        let mut things: Vec<Thing> = vocab::things()
            .filter_map(|(_, w)| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                w.word.hash(&mut h);
                let share = (h.finish() % 1000) as f64 / 1000.0;
                let photos = ((share.powi(4) * 0.12 * indexed as f64).round() as usize).min(indexed);
                (photos > 0).then(|| Thing { word: w.plural.to_string(), photos, kind: w.kind })
            })
            .collect();
        things.sort_by(|a, b| b.photos.cmp(&a.photos).then_with(|| a.word.cmp(&b.word)));
        if things != *s.things {
            s.things = Arc::new(things);
            let _ = self.bus.unbounded_send(BgEvent::SearchThings(s.things.clone()));
        }
    }

    fn publish_downloads(&self, s: &FakeState) {
        for (model, done, total, state) in &s.downloads {
            let _ = self.bus.unbounded_send(BgEvent::ModelDownload { model: *model, done: *done, fetched: *done, total: *total, state: state.clone() });
        }
    }

    /// Change the state, say so, and keep the clock running while there is
    /// work.
    fn change(&self, f: impl FnOnce(&mut FakeState)) {
        let mut s = self.state.lock();
        f(&mut s);
        self.publish(&s);
        self.publish_downloads(&s);
        self.publish_things(&mut s);
        self.publish_people_if_changed(&mut s);
        self.keep_ticking(&mut s);
    }

    /// The groups, when they changed since sent (the faces status goes
    /// with the search status).
    fn publish_people_if_changed(&self, s: &mut FakeState) {
        if s.sent_people == Some(s.subjects.generation) {
            return;
        }
        s.sent_people = Some(s.subjects.generation);
        let _ = self.bus.unbounded_send(BgEvent::Subjects(s.subjects.clone()));
    }

    fn keep_ticking(&self, s: &mut FakeState) {
        if s.ticking || !s.busy() {
            return;
        }
        s.ticking = true;
        let (state, bus) = (self.state.clone(), self.bus.clone());
        let me = Fake { bus, state, decisions: self.decisions.clone() };
        std::thread::Builder::new().name("search-fake".into()).spawn(move || me.run()).ok();
    }

    fn run(self) {
        loop {
            std::thread::sleep(TICK);
            let mut s = self.state.lock();
            if !s.busy() {
                s.ticking = false;
                return;
            }
            step(&mut s);
            if let Some(e) = s.step_judge() {
                self.send_judged(e);
            }
            if let Some(e) = s.step_merge_judge() {
                let _ = self.bus.unbounded_send(e);
            }
            self.publish(&s);
            self.publish_downloads(&s);
            self.publish_things(&mut s);
            self.publish_people_if_changed(&mut s);
        }
    }

    fn on_library(&self, index: Arc<LibraryIndex>) {
        self.change(|s| {
            let shots = index.total_shots;
            let had = s.status.library_shots;
            s.index = Some(index);
            s.status.library_shots = shots;
            if let Some(ix) = s.index.clone() {
                s.people.on_library(&ix);
                s.fursuits.on_library(&ix);
            }
            if s.start_indexed {
                s.start_indexed = false;
                s.status.indexed_shots = shots;
                s.people.look_at(shots);
                let tagged = s.fursuits.tagged_in(shots);
                s.fursuits.scan(tagged);
            }
            s.status.indexed_shots = s.status.indexed_shots.min(shots);
            s.refresh_people();
            // An export added photos: index them quietly (decision 5).
            let grew = shots.saturating_sub(had);
            if had > 0 && grew > 0 && s.status.first_index_asked && s.status.index_exports && s.status.pass.is_none() {
                if s.status.stopped.is_some() {
                    s.status.waiting += grew;
                } else {
                    s.status.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: grew, oldest_day: None });
                }
            }
        });
    }

    fn start_first_index(&self, ids: &[ModelId]) {
        let faces = models::face_model_in(ids);
        self.change(|s| {
            // As the engine: only what isn't in place is fetched (and
            // reported); with everything here, indexing starts at once.
            let begin = super::search_setup::begin_first_index(&s.status, faces);
            s.status.faces.on = faces.is_some();
            if let Some(f) = faces {
                s.status.faces.installed = super::search_setup::face_ready(&s.status, f);
                s.status.faces.model = Some(f);
            }
            s.status.first_index_asked = true;
            s.status.stopped = None;
            match begin {
                super::search_setup::Begin::Index => s.resume(),
                super::search_setup::Begin::Download(rows) => s.downloads = rows.into_iter().map(|d| (d.model, 0, d.total, DownloadState::Waiting)).collect(),
            }
        });
    }

    fn cancel_downloads(&self) {
        self.change(|s| {
            for d in &mut s.downloads {
                if !d.3.is_final() {
                    d.3 = DownloadState::Cancelled;
                }
            }
        });
    }

    fn try_card_again(&self) {
        self.change(|s| {
            if matches!(s.status.stopped, Some(Stopped::Card(_)) | Some(Stopped::LibraryAway { .. }) | Some(Stopped::Cancelled)) {
                s.status.stopped = None;
            }
            if s.status.device.chosen == Device::Gpu {
                s.status.device.name = Some(FAKE_CARD.into());
                s.status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
            }
            s.resume();
            // A head scan the card stopped carries on (on the processor it
            // asks first).
            if matches!(s.status.heads.step, fursuits::Step::Failed(_)) && s.status.stopped.is_none() {
                s.status.heads.step = fursuits::Step::Idle;
                s.start_heads(fursuits::Why::Resume);
            }
        });
    }

    fn set(&self, what: &str) {
        let (word, arg) = what.split_once(' ').unwrap_or((what, ""));
        let n: usize = arg.trim().parse().unwrap_or(0);
        self.change(|st| {
            let s = &mut st.status;
            match word {
                // Nothing indexed; `models` with every model already in
                // place, `search-model` with only search's.
                "notindexed" | "models" | "search-model" => {
                    s.models_installed = word != "notindexed";
                    s.faces.installed = word == "models";
                    s.indexed_shots = 0;
                    s.first_index_asked = false;
                    s.pass = None;
                    s.stopped = None;
                    s.waiting = 0;
                    st.downloads.clear();
                }
                "indexed" => {
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.indexed_shots = s.library_shots;
                    s.pass = None;
                    s.stopped = None;
                    s.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
                }
                "indexing" => {
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.stopped = None;
                    s.indexed_shots = if n > 0 { n.min(s.library_shots) } else { s.library_shots / 3 };
                    s.pass = Some(PassProgress { kind: PassKind::FirstIndex, done: s.indexed_shots, total: s.library_shots, oldest_day: None });
                }
                "export" => {
                    let n = n.max(1);
                    s.models_installed = true;
                    s.first_index_asked = true;
                    s.indexed_shots = s.library_shots.saturating_sub(n);
                    s.pass = Some(PassProgress { kind: PassKind::NewExports, done: 0, total: n, oldest_day: None });
                }
                "pause" => {
                    st.paused = true;
                    st.stepped_at = None;
                }
                "resume" => st.paused = false,
                // Downloads at this many MB a second (0: a twelfth of the
                // model a tick, as before).
                "download-rate" => st.download_rate = arg.trim().parse::<f64>().ok().filter(|&r| r > 0.0).map(|r| (r * 1e6) as u64),
                "cardcrash" => s.stopped = Some(Stopped::Card(CardFailure::Crashed)),
                "selfcheck" => {
                    s.device.self_check = SelfCheck::Failed { cosine: 0.42, max_diff: 0.9 };
                    s.stopped = Some(Stopped::Card(CardFailure::SelfCheckFailed { cosine: 0.42, max_diff: 0.9 }));
                }
                "nocard" => {
                    s.device.name = None;
                    s.stopped = Some(Stopped::Card(CardFailure::NoCard));
                }
                "away" => {
                    let share = if arg.trim().is_empty() { "nas".to_string() } else { arg.trim().to_string() };
                    s.stopped = Some(Stopped::LibraryAway { share, message: "No route to host (os error 113)".into() });
                }
                "restarted" => s.helper_restarts += 1,
                "waiting" => s.waiting = n.max(1),
                "mismatch" => {
                    st.fail_next = Some(DownloadState::Mismatch {
                        expected: models::info(ModelId::Siglip2).weights_sha256().to_string(),
                        got: "7e410b9d3c2a8f6155a012c8f0a2be4c19d3e0a77f6b2c5d81e9a04b3f6c2d10".into(),
                    })
                }
                "offline" => st.fail_next = Some(DownloadState::Offline),
                // People: right after the first index (none named), the
                // library indexed before faces were looked for, faces
                // off, a catch-up running, or found with two named.
                "people-first" | "people" => {
                    let indexed = s.indexed_shots;
                    s.faces = FacesStatus { on: true, model: Some(models::FACE_MODEL_DEFAULT), installed: true, ..FacesStatus::default() };
                    st.people.forget_all();
                    st.people.name_first = word == "people";
                    st.people.look_at(indexed);
                    st.refresh_people();
                }
                "faces-catchup" => {
                    s.faces = FacesStatus { on: true, model: None, installed: false, ..FacesStatus::default() };
                    st.people.forget_all();
                    st.people.name_first = false;
                    st.refresh_people();
                }
                "faces-off" => {
                    s.faces.on = false;
                    if s.pass.as_ref().is_some_and(|p| p.kind == PassKind::Faces) {
                        s.pass = None;
                    }
                }
                "faces-running" => {
                    let total = s.indexed_shots;
                    let done = if n > 0 { n.min(total) } else { total / 4 };
                    s.faces = FacesStatus { on: true, model: Some(models::FACE_MODEL_DEFAULT), installed: true, ..FacesStatus::default() };
                    s.pass = Some(PassProgress { kind: PassKind::Faces, done, total, oldest_day: None });
                    st.people.forget_all();
                    st.people.name_first = false;
                    st.people.look_at(done);
                    st.refresh_people();
                }
                // Fursuits: heads found with two named, found with none
                // named, the library indexed before heads were looked for,
                // finding them off, a scan running, a scan waiting on the
                // processor's answer, scans skipped, the card failing.
                // `fursuits-many [n]`: as `fursuits-first`, with `n` more
                // made-up groups (800 by default), for a tab of many tiles.
                "fursuits" | "fursuits-first" | "fursuits-many" => {
                    let indexed = s.indexed_shots;
                    s.heads = fursuits::HeadsStatus { on: true, models_installed: true, ..fursuits::HeadsStatus::default() };
                    st.fursuits.forget_all();
                    st.fursuits.name_first = word == "fursuits";
                    st.fursuits.many = if word == "fursuits-many" { if n > 0 { n } else { 800 } } else { 0 };
                    let tagged = st.fursuits.tagged_in(indexed);
                    st.fursuits.scan(tagged);
                    st.refresh_people();
                }
                "heads-catchup" => {
                    s.heads = fursuits::HeadsStatus { on: true, models_installed: false, ..fursuits::HeadsStatus::default() };
                    st.fursuits.forget_all();
                    st.refresh_people();
                }
                "heads-off" => {
                    s.heads.on = false;
                    s.heads.step = fursuits::Step::Idle;
                }
                "heads-running" => {
                    let total = st.fursuits.tagged_in(s.indexed_shots);
                    let done = if n > 0 { n.min(total) } else { total / 4 };
                    s.heads = fursuits::HeadsStatus { on: true, models_installed: true, step: fursuits::Step::Scanning { why: fursuits::Why::CatchUp, done, total }, ..fursuits::HeadsStatus::default() };
                    st.fursuits.forget_all();
                    st.fursuits.name_first = false;
                    st.fursuits.scan(done);
                    st.refresh_people();
                }
                "heads-asking" => {
                    // heads-asking [photos] [first|export|resume]
                    let mut words = arg.split_whitespace();
                    let photos = words.next().and_then(|w| w.parse().ok()).unwrap_or(412);
                    let why = match words.next() {
                        Some("first") => fursuits::Why::FirstIndex,
                        Some("resume") => fursuits::Why::Resume,
                        _ => fursuits::Why::NewExports,
                    };
                    // The photos asked about are tagged and not looked at
                    // yet, as the engine's are, so skipping leaves them
                    // offered on the tab.
                    let tagged = st.fursuits.tagged_in(s.indexed_shots);
                    let photos = photos.min(tagged);
                    s.heads.on = true;
                    s.heads.models_installed = true;
                    s.heads.skipped = 0;
                    s.heads.step = fursuits::Step::Asking(fursuits::Offer { photos, why });
                    st.fursuits.scan(tagged - photos);
                    st.refresh_people();
                }
                "heads-skipped" => {
                    let tagged = st.fursuits.tagged_in(s.indexed_shots);
                    let skip = if n > 0 { n.min(tagged) } else { tagged / 5 };
                    s.heads.on = true;
                    s.heads.models_installed = true;
                    s.heads.skipped = skip;
                    s.heads.step = fursuits::Step::Idle;
                    st.fursuits.scan(tagged - skip);
                    st.refresh_people();
                }
                // The close-call judge: chosen and in place, off, chosen
                // but not downloaded, the card held by another step (or
                // let go), a card failure, a broken helper.
                "judge-on" | "judge-off" | "judge-notinstalled" => {
                    // `judge-on [qwen|gemma|both]`: which is chosen and in place.
                    let qwen = arg.trim().starts_with("qwen");
                    let picked = if qwen { JudgeChoice::Qwen3VlReranker } else { JudgeChoice::Gemma4 };
                    let choice = if word == "judge-off" { JudgeChoice::Off } else { picked };
                    let downloaded = match (word, arg.trim()) {
                        (_, "both") => vec![JudgeChoice::Qwen3VlReranker, JudgeChoice::Gemma4],
                        ("judge-on", _) => vec![picked],
                        _ => Vec::new(),
                    };
                    s.judge = JudgeStatus { choice, downloaded, loaded: false, state: JudgeState::Idle };
                    st.fursuits.judge_on = s.judge.installed();
                    st.judge_run = None;
                    st.merge_run = None;
                    st.refresh_people();
                    st.sent_people = None;
                }
                "judge-wait" => st.card_held = true,
                "judge-nosure" => {
                    st.fursuits.no_sure = true;
                    st.refresh_people();
                    // The counts changed with no edit: send them again.
                    st.sent_people = None;
                }
                "judge-free" => st.card_held = false,
                "judge-cardcrash" => {
                    st.judge_run = None;
                    st.merge_run = None;
                    s.judge.loaded = false;
                    s.judge.state = JudgeState::Failed(CardFailure::Crashed);
                }
                "judge-broken" => {
                    st.judge_run = None;
                    st.merge_run = None;
                    s.judge.loaded = false;
                    s.judge.state = JudgeState::Broken("The judge's model file is damaged. Delete it in Preferences ▸ Storage and download it again.".into());
                }
                // Heads found before fursuit matching v2, without the
                // search model's head vectors.
                "heads-vectors" => {
                    let tagged = st.fursuits.tagged_in(s.indexed_shots);
                    s.heads.on = true;
                    s.heads.models_installed = true;
                    s.heads.vectors_missing = if n > 0 { n } else { tagged * 3 / 5 };
                    s.heads.step = fursuits::Step::Idle;
                    // `heads-vectors all`: none has them yet, so nothing
                    // is grouped (a library from before v2).
                    if arg.trim() == "all" {
                        s.heads.vectors_missing = tagged * 3 / 5;
                        st.fursuits.forget_all();
                        st.refresh_people();
                        st.status.heads.found = tagged;
                    }
                }
                "heads-cardcrash" => {
                    s.stopped = Some(Stopped::Card(CardFailure::Crashed));
                    s.heads.step = fursuits::Step::Failed(CardFailure::Crashed);
                }
                _ => eprintln!("[search] unknown fake state {what:?}"),
            }
        });
    }

    fn query(&self, seq: u64, text: &str, closeness: f32, subjects: &[GroupId]) {
        // `@name`s written in the text narrow to whom they name, as the
        // engine's do; a name that names nobody narrows to nothing.
        let names = crate::search::parse(text).names;
        if !subjects.is_empty() || !names.is_empty() {
            let all = self.state.lock().subjects.clone();
            let mut within: Vec<GroupId> = subjects.to_vec();
            let mut unknown = false;
            for n in &names {
                match crate::app::search::NAMED_KINDS.iter().find_map(|&k| super::people::by_name(&all, k, n)) {
                    Some(g) => within.push(g.id.clone()),
                    None => unknown = true,
                }
            }
            if unknown {
                let indexed = self.state.lock().status.indexed_shots;
                let _ = self.bus.unbounded_send(BgEvent::SearchResults {
                    seq,
                    hits: Arc::new(Vec::new()),
                    indexed,
                    error: None,
                    understood: Arc::new(crate::search::parse(text)),
                    fursuits: Arc::new(Vec::new()),
                    animal: None,
                    subjects: Arc::new(within),
                    also_fursuits: Arc::new(Vec::new()),
                });
                return;
            }
            return self.query_people(seq, text, &within);
        }
        let (index, indexed) = {
            let s = self.state.lock();
            (s.index.clone(), s.status.indexed_shots)
        };
        let (bus, words) = (self.bus.clone(), text.to_string());
        std::thread::Builder::new()
            .name("search-fake-query".into())
            .spawn(move || {
                let parsed = crate::search::parse(&words);
                let animal = vocab::animal_in(&parsed.words).map(|w| w.word.to_string());
                // Dates alone: every shot from them, in the order taken.
                let every = parsed.is_empty() && parsed.dates.is_some();
                let words = if every { "everything" } else { parsed.words.as_str() };
                let mut hits = index.map(|ix| made_up_hits(&ix, words, closeness, indexed)).unwrap_or_default();
                if parsed.is_empty() && !every {
                    hits.clear();
                }
                if every {
                    hits.sort_by(|a, b| a.capture.cmp(&b.capture));
                }
                if let Some(d) = parsed.dates {
                    hits.retain(|h| d.contains(h.day));
                }
                // Each word left out takes about a third of the hits away.
                hits.retain(|h| parsed.exclude.iter().all(|x| unit(&(x, h.id)) > 0.33));
                // The fursuit contrast: every third hit of an animal query
                // is a fursuit left out.
                let mut fursuits = Vec::new();
                if animal.is_some() {
                    let (f, h): (Vec<(usize, Hit)>, Vec<(usize, Hit)>) = hits.into_iter().enumerate().partition(|(i, _)| i % 3 == 2);
                    fursuits = f.into_iter().map(|(_, h)| h).collect();
                    hits = h.into_iter().map(|(_, h)| h).collect();
                }
                let _ = bus.unbounded_send(BgEvent::SearchResults {
                    seq,
                    hits: Arc::new(hits),
                    indexed,
                    error: None,
                    understood: Arc::new(parsed),
                    fursuits: Arc::new(fursuits),
                    animal,
                    subjects: Arc::new(Vec::new()),
                    also_fursuits: Arc::new(Vec::new()),
                });
            })
            .ok();
    }
}

impl Fake {
    /// A search within the photos of `subjects` (every one of them): no
    /// words, clearest first; words, the made-up best first among them.
    fn query_people(&self, seq: u64, text: &str, subjects: &[GroupId]) {
        let (faces, indexed) = {
            let s = self.state.lock();
            let mut keep: Option<std::collections::HashSet<Key>> = None;
            for g in subjects {
                let m = if s.fursuits.owns(g) { s.fursuits.members(g) } else { s.people.members(g) };
                keep = Some(match keep {
                    None => m,
                    Some(k) => k.intersection(&m).copied().collect(),
                });
            }
            let keep = keep.unwrap_or_default();
            let first = &subjects[0];
            let faces = if s.fursuits.owns(first) { s.fursuits.faces(first) } else { s.people.faces(first) };
            // One hit per photo, at its clearest face (a photo can hold two
            // heads of one fursuit group).
            let mut seen = std::collections::HashSet::new();
            let faces: Vec<Face> = faces.into_iter().filter(|f| keep.contains(&f.face.key) && seen.insert(f.face.key)).collect();
            (faces, s.status.indexed_shots)
        };
        let parsed = crate::search::parse(text);
        let words = parsed.words.trim().to_lowercase();
        let mut hits: Vec<Hit> = faces
            .iter()
            .filter_map(|f| {
                let id = u64::from_le_bytes(f.face.key.0[..8].try_into().unwrap_or_default());
                let score = if words.is_empty() { f.clarity } else { unit(&(&words, id)) as f32 };
                (words.is_empty() || score > 0.45).then(|| Hit {
                    id,
                    key: f.face.key,
                    score,
                    tagged: false,
                    day: f.day,
                    capture: f.capture,
                    burst: f.burst,
                    jpg: f.jpg.clone(),
                    raw: f.raw.clone(),
                })
            })
            .collect();
        if let Some(d) = parsed.dates {
            hits.retain(|h| d.contains(h.day));
        }
        hits.retain(|h| parsed.exclude.iter().all(|x| unit(&(x, h.id)) > 0.33));
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.capture.cmp(&b.capture)));
        let _ = self.bus.unbounded_send(BgEvent::SearchResults {
            seq,
            hits: Arc::new(hits),
            indexed,
            error: None,
            understood: Arc::new(parsed),
            fursuits: Arc::new(Vec::new()),
            animal: None,
            subjects: Arc::new(subjects.to_vec()),
            also_fursuits: Arc::new(Vec::new()),
        });
    }
}

/// Draws faces and photos for the pages, on a worker: the engine's crops
/// of the previews, or the stand-in's grey silhouettes.
#[derive(Clone)]
pub struct Cropper {
    engine: Option<Arc<Engine>>,
    fake: Option<Arc<Mutex<FakeState>>>,
}

impl Cropper {
    /// `face` framed as asked, fitted in `px` × `px`: (width, height,
    /// RGBA).  Blocking: call it on a worker.
    pub fn crop(&self, face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
        if let Some(fake) = &self.fake {
            let (suit, person) = {
                let s = fake.lock();
                (s.fursuits.look_of(&face.face), s.people.look_of(&face.face))
            };
            // A fursuit's head is a grey cartoon head; a person's face a
            // grey silhouette.  Never a photo.
            let mut h = std::collections::hash_map::DefaultHasher::new();
            face.face.hash(&mut h);
            let seed = h.finish();
            return Some(match (suit, framing) {
                (Some(suit), Framing::Face) => super::fursuit_art::head_crop(suit, seed, px),
                (Some(suit), Framing::Photo) => super::fursuit_art::photo(suit, seed, face.face.bbox, px),
                (None, _) => super::people::silhouette(person, &face.face, px, framing),
            });
        }
        self.engine.as_ref()?.crop(face, px, framing)
    }
}

/// A hash of `x` as a number in 0..1.
fn unit(x: &impl Hash) -> f64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    (h.finish() % 10_000) as f64 / 10_000.0
}

/// One step of the stand-in's downloads, then its indexing.
fn step(s: &mut FakeState) {
    let fail = s.fail_next.clone();
    let now = std::time::Instant::now();
    let by_clock = s.download_rate.map(|r| (r as f64 * s.stepped_at.map_or(TICK, |t| now - t).as_secs_f64()) as u64);
    if let Some(d) = s.downloads.iter_mut().find(|d| !d.3.is_final()) {
        s.stepped_at = Some(now);
        match d.3 {
            DownloadState::Waiting => d.3 = DownloadState::Downloading,
            DownloadState::Downloading => {
                d.1 = (d.1 + by_clock.unwrap_or(d.2 / 12)).min(d.2);
                if d.1 >= d.2 {
                    d.3 = DownloadState::Checking;
                }
            }
            DownloadState::Checking => {
                d.3 = fail.unwrap_or(DownloadState::Checked);
            }
            _ => {}
        }
        if d.3 != DownloadState::Checked && d.3.is_final() {
            s.fail_next = None;
            return;
        }
        // The judge's model alone: it is in place, nothing else changes.
        if s.downloads.iter().all(|d| d.3 == DownloadState::Checked && JudgeChoice::for_model(d.0).is_some()) {
            for c in s.downloads.iter().filter_map(|d| JudgeChoice::for_model(d.0)).collect::<Vec<_>>() {
                if !s.status.judge.downloaded.contains(&c) {
                    s.status.judge.downloaded.push(c);
                }
            }
            s.fursuits.judge_on = s.status.judge.installed();
            s.refresh_people();
            return;
        }
        if s.downloads.iter().all(|d| d.3 == DownloadState::Checked) {
            s.status.models_installed = true;
            if s.status.faces.on {
                s.status.faces.installed = true;
            }
            // The head models come with them when finding heads is on.
            s.status.heads.models_installed |= s.status.heads.on;
            if s.status.device.chosen == Device::Gpu {
                s.status.device.self_check = SelfCheck::Passed { cosine: 1.0, max_diff: 2.7e-5 };
            }
            s.resume();
        }
        return;
    }
    // The card failed: a head scan stops and says so, never moving to the
    // processor.
    if let Some(Stopped::Card(f)) = &s.status.stopped {
        if s.status.heads.scanning() {
            s.status.heads.step = fursuits::Step::Failed(f.clone());
        }
    }
    if s.status.stopped.is_some() {
        return;
    }
    // A head scan on its own (an export's photos, the catch-up).
    if s.status.pass.is_none() && s.step_heads() {
        return;
    }
    // A faces pass on its own: the catch-up reads the photos indexed, a new
    // model only recognises the faces found.
    if let Some(p) = s.status.pass.as_mut().filter(|p| p.kind == PassKind::Faces) {
        p.done = (p.done + (p.total / 30).max(1)).min(p.total);
        let (done, total) = (p.done, p.total);
        if s.status.faces.unrecognised > 0 {
            s.status.faces.unrecognised = total - done;
        } else {
            s.people.look_at(done);
        }
        if done >= total {
            s.status.pass = None;
        }
        s.refresh_people();
        return;
    }
    let library = s.status.library_shots;
    if let Some(p) = s.status.pass.as_mut() {
        p.done = (p.done + (p.total / 40).max(1)).min(p.total);
        let (kind, done, total) = (p.kind, p.done, p.total);
        if kind != PassKind::NewExports {
            s.status.indexed_shots = done;
        }
        // Faces are found in the same pass, on the photos it reaches.
        if s.status.faces.on && s.status.faces.model.is_some() && kind == PassKind::FirstIndex {
            s.people.look_at(done);
            s.refresh_people();
        }
        if done >= total {
            if kind == PassKind::NewExports {
                s.status.indexed_shots = (s.status.indexed_shots + total).min(library);
            }
            s.status.pass = None;
            // Then the fursuit-head step, on the photos with fursuits.
            match kind {
                PassKind::NewExports => s.start_heads(fursuits::Why::NewExports),
                PassKind::FirstIndex | PassKind::Rebuild => s.start_heads(fursuits::Why::FirstIndex),
                _ => {}
            }
        }
    }
}

/// The stand-in's hits: shots from the newest `indexed` of the library,
/// scored by a hash of the words and the file name, best first.  "zebra"
/// finds nothing; "everything" finds every shot (for long lists).
fn made_up_hits(index: &LibraryIndex, words: &str, closeness: f32, indexed: usize) -> Vec<Hit> {
    let words = words.trim().to_lowercase();
    if words.is_empty() || words.contains("zebra") {
        return Vec::new();
    }
    let everything = words == "everything";
    // The share of shots that pass: about 1 % at the strictest, 6 % at the
    // default, 25 % at the loosest.
    let span = crate::search::CLOSENESS_MAX - crate::search::CLOSENESS_MIN;
    let loose = ((crate::search::CLOSENESS_MAX - closeness) / span).clamp(0.0, 1.0) as f64;
    let share = if everything { 1.0 } else { 0.01 + loose.powf(1.6) * 0.24 };
    let mut hits = Vec::new();
    let mut seen = 0usize;
    let mut next_burst = 0u64;
    for (date, day) in index.days.iter().rev() {
        if seen >= indexed {
            break;
        }
        let mut shots: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>, NaiveDateTime)> = BTreeMap::new();
        for shot in day.shots() {
            let first = &day.files[shot[0]];
            let stem = util::stem(&first.path);
            let when = library_index::time_from_renamed_stem(&stem).unwrap_or(first.mtime.local);
            let e = shots.entry(stem.to_lowercase()).or_insert((None, None, when));
            for &i in shot {
                let f = &day.files[i];
                match f.file_type {
                    FileType::Jpg => e.0 = Some(f.path.clone()),
                    FileType::Raw => e.1 = Some(f.path.clone()),
                }
            }
        }
        seen += shots.len();
        // Presses: consecutive numbers within one minute, up to 8 frames.
        let mut last: Option<(NaiveDateTime, u32)> = None;
        let mut run_len = 0;
        for (stem, (jpg, raw, when)) in shots {
            let num: Option<u32> = stem.rsplit('_').next().and_then(|n| n.parse().ok());
            if matches!((last, num), (Some((t, n0)), Some(n1)) if t == when && n1 == n0 + 1 && run_len < 8) {
                run_len += 1;
            } else {
                next_burst += 1;
                run_len = 1;
            }
            last = num.map(|n| (when, n));
            let hash = |x: &dyn Fn(&mut std::collections::hash_map::DefaultHasher)| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                x(&mut h);
                (h.finish() % 10_000) as f64 / 10_000.0
            };
            let own = hash(&|h| (&words, &stem).hash(h));
            // Frames of a press score alike, so they come up together.
            let press = hash(&|h| (&words, next_burst, date).hash(h));
            let score = 0.7 * press + 0.3 * own;
            if everything || score > 1.0 - share {
                let capture = when + Span::milliseconds(num.unwrap_or(0) as i64 % 1000 * 3);
                let id = hash(&|h| (&stem, date).hash(h)).to_bits();
                hits.push(Hit { id, key: Key([0; 16]), score: score as f32, tagged: own > 0.8, day: *date, capture, burst: Some(next_burst), jpg, raw });
            }
        }
    }
    // A press of one frame is no burst.
    let mut sizes: HashMap<u64, usize> = HashMap::new();
    for h in &hits {
        *sizes.entry(h.burst.unwrap_or(u64::MAX)).or_default() += 1;
    }
    for h in &mut hits {
        if h.burst.is_some_and(|b| sizes.get(&b) == Some(&1)) {
            h.burst = None;
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.capture.cmp(&b.capture)));
    hits.truncate(crate::search::query::MAX_HITS.max(if everything { usize::MAX } else { 0 }));
    hits
}

// ── the stand-in's decision record ─────────────────────────────────────────

impl FakeState {
    /// A group as the record keeps it (`first`: heads shown first).
    fn side(&self, id: &GroupId, first: &[FaceRef]) -> decisions::Side {
        let faces = if self.fursuits.owns(id) { self.fursuits.faces(id) } else { self.people.faces(id) };
        let group = self.subjects.group(id);
        let sid = match id {
            GroupId::Subject(s) => Some(s.0.clone()),
            GroupId::Cluster(_) => None,
        };
        let shown = first.iter().filter(|f| faces.iter().any(|x| x.face == **f)).copied();
        decisions::Side::new(faces.len(), group.and_then(|g| g.name.clone()), sid, shown.chain(faces.iter().map(|f| f.face))).counted(group)
    }

    /// The stand-in's library for the record: made-up heads, raw space,
    /// the cut-offs the engine would use.
    fn lib(&self, kind: Kind) -> decisions::Lib {
        match kind {
            Kind::Fursuit => {
                // The grouping choice as the status carries it.
                let g = self.status.heads.cuts();
                let c = if g.whitened { crate::search::heads::CUTOFFS } else { crate::search::heads::RAW_CUTOFFS };
                let band = if g.whitened { (crate::search::judging::SURE_YES, crate::search::judging::SURE_NO) } else { (crate::search::judging::RAW_SURE_YES, crate::search::judging::RAW_SURE_NO) };
                decisions::Lib {
                    heads: self.status.heads.found,
                    white: g.whitened,
                    model: "stand-in".into(),
                    grouping: Some(g.grouping),
                    cuts: decisions::Cuts {
                        group: g.group,
                        suggest: c.suggest,
                        strict: c.strict,
                        sure_yes: Some(band.0),
                        sure_no: Some(band.1),
                        merge_suggest: Some(g.merge_suggest),
                        merge_sure: Some(g.merge_sure),
                        merge_far: Some(g.merge_far),
                    },
                }
            }
            _ => {
                let c = crate::model::subjects::cutoff_for(self.status.faces.model.unwrap_or(models::FACE_MODEL_DEFAULT));
                decisions::Lib { heads: self.status.faces.found, white: false, model: "stand-in".into(), grouping: None, cuts: decisions::Cuts { group: c.group, suggest: c.suggest, strict: c.strict, ..decisions::Cuts::default() } }
            }
        }
    }
}

fn fake_sorted(faces: &[FaceRef]) -> Vec<FaceRef> {
    let mut v = faces.to_vec();
    v.sort_by_key(|f| (f.key.0, f.thousandths()));
    v
}

/// A label edit in the stand-in, as the record keeps it (the boxes'
/// subjects aren't looked up: the stand-in keeps its labels apart).
fn fake_label_act(edit: &LabelEdit, subjects: &Subjects) -> (Kind, Act) {
    use crate::model::subjects::NameTo;
    let name = |to: &NameTo| match to {
        NameTo::Subject(id) => subjects.group(&GroupId::Subject(id.clone())).and_then(|g| g.name.clone()),
        NameTo::New(n) => Some(n.clone()),
        NameTo::Unnamed => None,
    };
    let head = decisions::Head(edit.face());
    match edit {
        LabelEdit::Name { kind, to, .. } => (*kind, Act::Name { head: Some(head), name: name(to), group: None, from: None }),
        LabelEdit::Kind { from, to, name: n, .. } => (*from, Act::KindChange { head, to: *to, name: n.as_ref().and_then(name) }),
        LabelEdit::Add { kind, name: n, .. } => (*kind, Act::Draw { head, name: n.as_ref().and_then(name) }),
        LabelEdit::Remove { kind, .. } => (*kind, Act::Remove { head }),
    }
}
