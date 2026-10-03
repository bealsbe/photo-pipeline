//! People's faces (phase 3, plan §5.2 step 5, §6.5, §7a.5): what the
//! engine keeps about faces beside the search index, and the *Subjects*
//! page's state built from it and from the user's file.
//!
//! - The **cache** (recomputable, under the library's index folder, files
//!   readable by the user alone): the detections YuNet found in each
//!   indexed photo, the face vectors of the chosen model, and a small
//!   tile of each face for the page.  A face-model change keeps the
//!   detections and recomputes the vectors alone.
//! - The **user's file** (`crate::model::subjects::SubjectsFile`): names,
//!   merges, splits, hides and "not this person", pointing at detections.
//! - [`People`]: both together on the engine's thread, with the clusters
//!   derived from them, the undo stack and the snapshot the page shows.
//!
//! Nothing here is written into photos, sidecars or exports (§10).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::imaging::decode;
use crate::model::subjects::{self, Face, FaceRef, Framing, Group, GroupId, Kind, Refused, Subject, SubjectId, Subjects, SubjectsFile, UNDO_DEPTH};

/// The people state the engine's thread owns.
pub struct People {
    /// Where the user's file lives (`subjects::default_path`, or a test's).
    pub file_path: Option<PathBuf>,
    pub file: SubjectsFile,
    /// Earlier states of the file with what changed them, newest last.
    undo: Vec<(SubjectsFile, String)>,
    /// The snapshot last built.
    pub snapshot: Arc<Subjects>,
    generation: u64,
}

impl People {
    /// Load the user's file (none is empty).
    pub fn load(file_path: Option<PathBuf>) -> People {
        let file = file_path.as_deref().map(SubjectsFile::load).unwrap_or_default();
        People { file_path, file, undo: Vec::new(), snapshot: Arc::new(Subjects::default()), generation: 1 }
    }

    /// Record the state before an edit, so [`People::undo`] can restore
    /// it; `what` is the undo's description ("Named a group Someone").
    fn checkpoint(&mut self, what: impl Into<String>) {
        self.undo.push((self.file.clone(), what.into()));
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// Take the last edit back; returns what was undone.
    pub fn undo(&mut self) -> Option<String> {
        let (file, what) = self.undo.pop()?;
        self.file = file;
        self.save();
        Some(what)
    }

    /// Write the file (whole, atomically, private).
    pub fn save(&self) {
        if let Some(p) = &self.file_path {
            if let Err(e) = self.file.save(p) {
                eprintln!("subjects: couldn't save {}: {e}", p.display());
            }
        }
    }

    /// The subject behind a group id; a cluster is not a subject.
    pub fn subject_of(&self, id: &GroupId) -> Option<&Subject> {
        match id {
            GroupId::Subject(s) => self.file.get(s),
            GroupId::Cluster(_) => None,
        }
    }

    /// The kind of a group, by its id.
    fn kind_of(&self, id: &GroupId) -> Option<Kind> {
        match id {
            GroupId::Subject(s) => self.file.get(s).map(|s| s.kind),
            GroupId::Cluster(_) => self.cluster_members(id).map(|_| Kind::Person),
        }
    }

    /// A cluster's faces for the current generation (none: stale or
    /// unknown).  Clusters arrive with the grouping (a later commit).
    fn cluster_members(&self, _id: &GroupId) -> Option<Vec<FaceRef>> {
        None
    }

    /// A group's confirmed or clustered faces.
    fn members(&self, id: &GroupId) -> Result<(Kind, Vec<FaceRef>), Refused> {
        match id {
            GroupId::Subject(s) => {
                let s = self.file.get(s).ok_or_else(|| Refused::NoSuchSubject(s.clone()))?;
                Ok((s.kind, s.members.clone()))
            }
            GroupId::Cluster(_) => self.cluster_members(id).map(|m| (Kind::Person, m)).ok_or(Refused::StaleGroup),
        }
    }

    /// Name a group: a subject is renamed; a cluster becomes a subject
    /// with the cluster's faces as its members.
    pub fn name_group(&mut self, id: &GroupId, name: String) -> Result<SubjectId, Refused> {
        let (kind, members) = self.members(id)?;
        match id {
            GroupId::Subject(s) => {
                self.checkpoint(match self.file.get(s).and_then(|s| s.name.clone()) {
                    Some(old) => format!("Renamed {old} to {name}"),
                    None => format!("Named a group {name}"),
                });
                self.file.rename(s, name)?;
                self.save();
                Ok(s.clone())
            }
            GroupId::Cluster(_) => {
                self.checkpoint(format!("Named a group {name}"));
                let id = self.file.create(kind, Some(name), members);
                self.save();
                Ok(id)
            }
        }
    }

    /// Merge groups into one (all of one kind, else refused): clusters
    /// become members of `into`, which becomes a subject if it was a
    /// cluster.
    pub fn merge(&mut self, into: &GroupId, others: &[GroupId]) -> Result<SubjectId, Refused> {
        let kind = self.kind_of(into).ok_or(Refused::StaleGroup)?;
        let others: Vec<&GroupId> = others.iter().filter(|o| *o != into).collect();
        if others.is_empty() {
            return Err(Refused::Nothing);
        }
        for o in &others {
            if self.kind_of(o).ok_or(Refused::StaleGroup)? != kind {
                return Err(Refused::MixedKinds);
            }
        }
        let target_name = self.subject_of(into).and_then(|s| s.name.clone()).or_else(|| others.iter().find_map(|o| self.subject_of(o).and_then(|s| s.name.clone())));
        self.checkpoint(match &target_name {
            Some(n) => format!("Merged {} groups into {n}", others.len() + 1),
            None => format!("Merged {} groups", others.len() + 1),
        });
        let target = match into {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => {
                let (_, members) = self.members(into)?;
                self.file.create(kind, None, members)
            }
        };
        for o in others {
            match o {
                GroupId::Subject(s) => self.file.merge(&target, s)?,
                GroupId::Cluster(_) => {
                    let (_, members) = self.members(o)?;
                    let _ = self.file.add_members(&target, members);
                }
            }
        }
        self.save();
        Ok(target)
    }

    /// Hide or show a group (a hidden cluster becomes an unnamed subject,
    /// so the hide outlives a regroup).
    pub fn set_hidden(&mut self, id: &GroupId, hidden: bool) -> Result<SubjectId, Refused> {
        let (kind, members) = self.members(id)?;
        let name = self.subject_of(id).and_then(|s| s.name.clone());
        self.checkpoint(match (hidden, name) {
            (true, Some(n)) => format!("Hid {n}"),
            (true, None) => "Hid a group".to_string(),
            (false, Some(n)) => format!("Showed {n}"),
            (false, None) => "Showed a group".to_string(),
        });
        let sid = match id {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => self.file.create(kind, None, members),
        };
        self.file.set_hidden(&sid, hidden)?;
        self.save();
        Ok(sid)
    }

    /// Forget a subject: its faces become unnamed again.
    pub fn forget(&mut self, id: &SubjectId) -> Result<(), Refused> {
        let name = self.file.get(id).and_then(|s| s.name.clone());
        self.checkpoint(match name {
            Some(n) => format!("Forgot {n}"),
            None => "Forgot a group".to_string(),
        });
        self.file.forget(id)?;
        self.save();
        Ok(())
    }

    /// Move faces out of a group: to another group of the same kind, or
    /// to a new unnamed subject (`None`).  From a cluster, the faces are
    /// confirmed into the destination; the rest of the cluster stays
    /// where the grouping puts it.
    pub fn move_faces(&mut self, from: &GroupId, faces: Vec<FaceRef>, to: Option<&GroupId>) -> Result<SubjectId, Refused> {
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        let kind = self.kind_of(from).ok_or(Refused::StaleGroup)?;
        if let Some(t) = to {
            if self.kind_of(t).ok_or(Refused::StaleGroup)? != kind {
                return Err(Refused::MixedKinds);
            }
            if t == from {
                return Err(Refused::Nothing);
            }
        }
        let n = faces.len();
        let to_name = to.and_then(|t| self.subject_of(t)).and_then(|s| s.name.clone());
        self.checkpoint(match to_name {
            Some(name) => format!("Moved {n} {} to {name}", plural(n, "face")),
            None => format!("Moved {n} {} to a new group", plural(n, "face")),
        });
        let target = match to {
            Some(GroupId::Subject(s)) => s.clone(),
            Some(c @ GroupId::Cluster(_)) => {
                let (_, members) = self.members(c)?;
                self.file.create(kind, None, members)
            }
            None => self.file.create(kind, None, Vec::new()),
        };
        self.file.add_members(&target, faces)?;
        self.save();
        Ok(target)
    }

    /// A review answer for `group`: `Some(true)` confirms the faces as
    /// members, `Some(false)` rejects them ("not this person"), `None`
    /// takes an earlier answer back (the faces leave both lists).  A
    /// cluster answered becomes a subject.
    pub fn answer(&mut self, group: &GroupId, faces: Vec<FaceRef>, yes: Option<bool>) -> Result<SubjectId, Refused> {
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        let (kind, members) = self.members(group)?;
        let n = faces.len();
        let name = self.subject_of(group).and_then(|s| s.name.clone()).unwrap_or_else(|| "the group".to_string());
        self.checkpoint(match yes {
            Some(true) => format!("Added {n} {} to {name}", plural(n, "photo")),
            Some(false) => format!("Marked {n} {} not {name}", plural(n, "photo")),
            None => format!("Took back {n} {} for {name}", plural(n, "answer")),
        });
        let sid = match group {
            GroupId::Subject(s) => s.clone(),
            GroupId::Cluster(_) => self.file.create(kind, None, members),
        };
        match yes {
            Some(true) => {
                self.file.add_members(&sid, faces)?;
            }
            Some(false) => {
                self.file.reject(&sid, faces)?;
            }
            None => {
                self.file.remove_members(&sid, &faces)?;
                self.file.unreject(&sid, &faces)?;
            }
        }
        self.save();
        Ok(sid)
    }

    /// *Forget all*: the user's file goes (the caller deletes the cache).
    pub fn forget_all(&mut self) {
        self.file = SubjectsFile::default();
        self.undo.clear();
        if let Some(p) = &self.file_path {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Build the page's snapshot: subjects (named first, most photos
    /// first), then clusters.  `resolve` turns a detection into a
    /// [`Face`] with its shot (the engine's resolver); detections of
    /// photos no longer in the library resolve to nothing and are not
    /// counted.
    pub fn build_snapshot(&self, resolve: &dyn Fn(&FaceRef) -> Option<Face>) -> Arc<Subjects> {
        let mut groups: Vec<Group> = Vec::new();
        for s in &self.file.subjects {
            let faces: Vec<Face> = s.members.iter().filter_map(resolve).collect();
            let Some(cover) = s.cover.and_then(|c| resolve(&c)).or_else(|| faces.iter().cloned().max_by(|a, b| a.clarity.total_cmp(&b.clarity))) else { continue };
            let photos = faces.iter().map(|f| f.face.key).collect::<std::collections::HashSet<_>>().len();
            let days = faces.iter().map(|f| f.day).collect::<std::collections::HashSet<_>>().len();
            groups.push(Group { id: GroupId::Subject(s.id.clone()), kind: s.kind, name: s.name.clone(), hidden: s.hidden, photos, days, cover, suggestions: 0 });
        }
        groups.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(b.photos.cmp(&a.photos)).then_with(|| a.name.cmp(&b.name)));
        let undoable = self.undo.last().map(|(_, what)| what.clone());
        Arc::new(Subjects { generation: self.generation, groups, faces_found: 0, undoable })
    }

    /// Keep a snapshot built with [`People::build_snapshot`].
    pub fn set_snapshot(&mut self, snapshot: Arc<Subjects>) {
        self.snapshot = snapshot;
    }

    /// A group's faces, clearest first.
    pub fn faces(&self, id: &GroupId, resolve: &dyn Fn(&FaceRef) -> Option<Face>) -> Result<Vec<Face>, Refused> {
        let (_, members) = self.members(id)?;
        let mut out: Vec<Face> = members.iter().filter_map(resolve).collect();
        out.sort_by(|a, b| b.clarity.total_cmp(&a.clarity));
        Ok(out)
    }

    /// Named subjects of `kind` called `name`.
    pub fn named(&self, kind: Kind, name: &str) -> Vec<GroupId> {
        self.file.named(kind, name).into_iter().map(|s| GroupId::Subject(s.id.clone())).collect()
    }

    /// The photos (keys) a group's faces are in.
    pub fn photos_of(&self, id: &GroupId) -> Result<std::collections::HashSet<crate::search::index::Key>, Refused> {
        let (_, members) = self.members(id)?;
        Ok(members.iter().map(|f| f.key).collect())
    }

    /// The clarity of the clearest face in each of `keys` (UI 12's order
    /// for a subject's page).  Clarity comes with the face cache (a later
    /// commit); until then every photo is equally clear.
    pub fn clarity_by_photo(&self, keys: &std::collections::HashSet<crate::search::index::Key>) -> std::collections::HashMap<crate::search::index::Key, f32> {
        keys.iter().map(|k| (*k, 0.0)).collect()
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

/// A face's pixels from its photo's own preview, for `Engine::crop` when
/// no tile is cached: reads the file (call on a worker), decodes the
/// preview, orients it, and cuts the face's square (a margin around the
/// box) or fits the whole preview, as `px`×`px` RGBA.
pub fn crop_from_photo(face: &Face, px: u32, framing: Framing) -> Option<(u32, u32, Vec<u8>)> {
    let px = px.clamp(8, 2048);
    let path = face.jpg.as_deref().or(face.raw.as_deref())?;
    let oriented = preview_pixels(path, face.jpg.is_some(), px.max(400))?;
    let out = match framing {
        Framing::Photo => decode::fit(oriented, px),
        Framing::Face => {
            let (w, h) = (oriented.width as f32, oriented.height as f32);
            let b = face.face.bbox;
            // A square around the box, 60 % wider than its longer side.
            let side = (b[2] * w).max(b[3] * h) * 1.6;
            let (cx, cy) = ((b[0] + b[2] / 2.0) * w, (b[1] + b[3] / 2.0) * h);
            let side = side.min(w).min(h).max(2.0);
            let x = (cx - side / 2.0).clamp(0.0, w - side);
            let y = (cy - side / 2.0).clamp(0.0, h - side);
            let cut = decode::crop(&oriented, x as u32, y as u32, (side as u32).max(1), (side as u32).max(1));
            decode::resize_to(&cut, px, px)
        }
    };
    let (w, h) = (out.width, out.height);
    Some((w, h, decode::to_rgba(out)))
}

/// The photo's preview, decoded and oriented, at least `min_edge` on its
/// shorter side where the preview allows.
fn preview_pixels(path: &Path, is_jpg: bool, min_edge: u32) -> Option<decode::Pixels> {
    use crate::imaging::libraw::{Raw, Thumbnail};
    if is_jpg {
        let (data, head) = crate::exif::jpeg_preview_bytes(path, min_edge).or_else(|| {
            let all = std::fs::read(path).ok()?;
            let head = all[..all.len().min(256 * 1024)].to_vec();
            Some((all, head))
        })?;
        let orientation = crate::exif::orientation_from_jpeg_head(&head);
        let (w, h) = decode::jpeg_dimensions(&data)?;
        let denom = [8, 4, 2].into_iter().find(|&d| w.min(h) / d >= min_edge).unwrap_or(1);
        let px = decode::decode_jpeg_full_scaled(&data, denom).ok()?;
        Some(decode::apply_orientation(px, orientation))
    } else {
        let mut raw = Raw::open(path).ok()?;
        let flip = raw.exif_orientation();
        match raw.thumbnail_at_least(min_edge).ok()? {
            Thumbnail::Jpeg(bytes) => {
                let orientation = crate::imaging::thumbs::embedded_orientation(&bytes, flip);
                let px = decode::decode_jpeg_full_scaled(&bytes, 1).ok()?;
                Some(decode::apply_orientation(px, orientation))
            }
            Thumbnail::Bitmap { width, height, colors, data } => {
                let px = crate::imaging::thumbs::bitmap(width, height, colors, data)?;
                Some(decode::apply_orientation(px, flip))
            }
        }
    }
}

/// The default subjects file path (`subjects::default_path`), for the
/// engine's configuration.
pub fn default_file_path() -> Option<PathBuf> {
    subjects::default_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::Key;
    use chrono::NaiveDate;

    fn face(n: u8) -> FaceRef {
        FaceRef::new(Key([n; 16]), [0.1, 0.1, 0.2, 0.2])
    }

    fn resolve(f: &FaceRef) -> Option<Face> {
        let day = NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        Some(Face { face: *f, jpg: Some(PathBuf::from(format!("/mnt/nas/lib/{}.JPG", f.key.0[0]))), raw: None, day, capture: day.and_hms_opt(12, 0, 0).unwrap(), burst: None, clarity: f.key.0[0] as f32, confirmed: true })
    }

    #[test]
    fn edits_are_undone_newest_first_and_the_snapshot_follows_the_file() {
        let d = std::env::temp_dir().join(format!("pp-people-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut p = People::load(Some(path.clone()));
        let a = p.move_faces(&GroupId::Cluster(1), vec![face(1)], None);
        assert_eq!(a, Err(Refused::StaleGroup), "no clusters yet: a cluster id is stale");
        let a = GroupId::Subject(p.file.create(Kind::Person, None, vec![face(1), face(2)]));
        p.name_group(&a, "Someone".into()).unwrap();
        assert!(path.is_file());
        let b = GroupId::Subject(p.file.create(Kind::Person, None, vec![face(3)]));
        p.merge(&a, std::slice::from_ref(&b)).unwrap();
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.groups.len(), 1);
        assert_eq!((snap.groups[0].photos, snap.groups[0].name.as_deref()), (3, Some("Someone")));
        assert_eq!(snap.groups[0].cover.face, face(3), "the clearest face is the cover");
        assert_eq!(snap.undoable.as_deref(), Some("Merged 2 groups into Someone"));
        assert_eq!(p.undo().as_deref(), Some("Merged 2 groups into Someone"));
        assert_eq!(p.file.subjects.len(), 2);
        assert_eq!(p.undo().as_deref(), Some("Named a group Someone"));
        assert!(p.file.subjects[0].name.is_none());
        assert_eq!(p.undo(), None);
        // Answers, moves, hides and forgets each leave an undo step.
        p.answer(&a, vec![face(4)], Some(true)).unwrap();
        p.answer(&a, vec![face(5)], Some(false)).unwrap();
        assert_eq!(p.faces(&a, &resolve).unwrap().iter().map(|f| f.face.key.0[0]).collect::<Vec<_>>(), [4, 2, 1], "clearest first");
        p.answer(&a, vec![face(5)], None).unwrap();
        assert!(p.subject_of(&a).unwrap().rejected.is_empty());
        let moved = p.move_faces(&a, vec![face(4)], None).unwrap();
        assert_eq!(p.subject_of(&GroupId::Subject(moved.clone())).unwrap().members, [face(4)]);
        p.set_hidden(&GroupId::Subject(moved.clone()), true).unwrap();
        p.forget(&moved).unwrap();
        assert_eq!(p.undo().as_deref(), Some("Forgot a group"));
        assert_eq!(p.undo().as_deref(), Some("Hid a group"));
        assert_eq!(p.undo().as_deref(), Some("Moved 1 face to a new group"));
        assert_eq!(p.subject_of(&a).unwrap().members.len(), 3);
        assert_eq!(p.photos_of(&a).unwrap().len(), 3);
        assert_eq!(p.answer(&a, vec![], Some(true)), Err(Refused::Nothing));
        p.forget_all();
        assert!(!path.exists() && p.file.subjects.is_empty() && p.undo().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Decision 12 through the people state: merging or moving across
    /// kinds is refused and leaves no undo step.
    #[test]
    fn people_fursuits_and_pets_never_merge() {
        let mut p = People::load(None);
        let person = GroupId::Subject(p.file.create(Kind::Person, Some("Someone".into()), vec![face(1)]));
        let suit = GroupId::Subject(p.file.create(Kind::Fursuit, Some("Suit".into()), vec![face(2)]));
        let pet = GroupId::Subject(p.file.create(Kind::Pet, Some("Pet".into()), vec![face(3)]));
        assert_eq!(p.merge(&person, &[suit.clone()]), Err(Refused::MixedKinds));
        assert_eq!(p.merge(&pet, &[person.clone(), suit.clone()]), Err(Refused::MixedKinds));
        assert_eq!(p.move_faces(&person, vec![face(1)], Some(&suit)), Err(Refused::MixedKinds));
        assert_eq!(p.move_faces(&suit, vec![face(2)], Some(&pet)), Err(Refused::MixedKinds));
        assert!(p.undo().is_none(), "a refusal leaves nothing to undo");
        assert_eq!(p.file.subjects.len(), 3);
        assert_eq!(p.named(Kind::Person, "someone"), [person.clone()]);
        assert!(p.named(Kind::Fursuit, "someone").is_empty(), "a name is of one kind");
        let snap = p.build_snapshot(&resolve);
        assert_eq!(snap.groups.iter().map(|g| g.kind).collect::<std::collections::HashSet<_>>().len(), 3);
    }
}
