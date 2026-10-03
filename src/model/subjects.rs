//! People, fursuits and pets as the app knows them (plan §5.1, §6.5,
//! §7.2 and §7a.5): the types the *Subjects* page shows, the user's own
//! file of names and decisions, and the pure rules for grouping faces,
//! ordering a subject's photos clearest first and offering suggestions.
//! Nothing here does I/O except [`SubjectsFile::load`] and
//! [`SubjectsFile::save`], which the engine calls on its own thread.
//!
//! **Two layers.**  Detections and their vectors are recomputable cache
//! (`search::faces`); *groups* are clusters recomputed from them whenever
//! they change.  The user's work — names, merges, splits, hides and "not
//! this person" answers — points at detections, not at group numbers, and
//! lives in `~/.local/share/photo-pipeline/subjects.json`, kept apart from
//! the caches and never cleared with them.  A face-model change or a
//! rebuild keeps every one of them (§6.5).
//!
//! **Kinds never mix** (decision 12, UI 3).  Each [`Subject`] and each
//! group has a [`Kind`]; a merge, a move, a suggestion or a query across
//! kinds is refused here ([`Refused::MixedKinds`]), and a test holds it.
//! Phase 3 has people only; fursuits and pets exist in the types and have
//! no detections yet.
//!
//! **Privacy** (§10): the file is written readable by the user alone
//! (0600 in a 0700 folder); names never leave the app.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, NaiveDateTime};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::search::index::Key;

/// The file's format version.
pub const FILE_VERSION: u32 = 1;
/// Undo steps the engine keeps for the subjects file.
pub const UNDO_DEPTH: usize = 50;

/// What kind of subject: people come from the face step, fursuits from
/// the head step (phase 4), pets from their own step (phase 5).  Never
/// combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Person,
    Fursuit,
    Pet,
}

impl Kind {
    /// The word in copy: "person", "fursuit", "pet".
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Fursuit => "fursuit",
            Kind::Pet => "pet",
        }
    }

    /// The tab's title.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Person => "People",
            Kind::Fursuit => "Fursuits",
            Kind::Pet => "Pets",
        }
    }
}

/// A named (or otherwise touched) subject's id: random, stable across
/// sessions, regroups and model changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubjectId(pub String);

impl SubjectId {
    /// A fresh id: 32 hex digits from the clock, the process and a counter.
    pub fn fresh() -> SubjectId {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut h = Md5::new();
        h.update(now.to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        h.update(n.to_le_bytes());
        SubjectId(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
}

impl fmt::Display for SubjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the page calls a group: a subject the user has touched (named,
/// hidden, moved faces into), or an automatic cluster of faces nobody has
/// touched.  A cluster id is valid for one [`Subjects::generation`]: the
/// engine refuses a stale one rather than misapplying it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupId {
    Subject(SubjectId),
    Cluster(u64),
}

/// One detection: a photo (by content key) and a box in it, as fractions
/// 0..1 of the oriented preview (`[x, y, width, height]`), rounded to
/// 1/1000 so the same face found again is the same detection (§6.5).
/// Equal and hashed by key and the rounded box.
#[derive(Debug, Clone, Copy)]
pub struct FaceRef {
    pub key: Key,
    pub bbox: [f32; 4],
}

impl FaceRef {
    /// A reference with the box rounded to thousandths.
    pub fn new(key: Key, bbox: [f32; 4]) -> FaceRef {
        FaceRef { key, bbox: bbox.map(|v| (v * 1000.0).round() / 1000.0) }
    }

    /// The box in thousandths, the identity the file keeps.
    pub fn thousandths(&self) -> [u16; 4] {
        self.bbox.map(|v| (v * 1000.0).round().clamp(0.0, 65535.0) as u16)
    }

    /// Centre of the box.
    pub fn centre(&self) -> (f32, f32) {
        (self.bbox[0] + self.bbox[2] / 2.0, self.bbox[1] + self.bbox[3] / 2.0)
    }

    /// The share of the smaller box the two boxes cover together: how
    /// much two detections overlap (for matching a box found again by a
    /// changed detector).
    pub fn overlap(&self, other: &FaceRef) -> f32 {
        if self.key != other.key {
            return 0.0;
        }
        let (a, b) = (self.bbox, other.bbox);
        let x0 = a[0].max(b[0]);
        let y0 = a[1].max(b[1]);
        let x1 = (a[0] + a[2]).min(b[0] + b[2]);
        let y1 = (a[1] + a[3]).min(b[1] + b[3]);
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let smaller = (a[2] * a[3]).min(b[2] * b[3]).max(1e-9);
        inter / smaller
    }
}

impl PartialEq for FaceRef {
    fn eq(&self, other: &FaceRef) -> bool {
        self.key == other.key && self.thousandths() == other.thousandths()
    }
}

impl Eq for FaceRef {}

impl Hash for FaceRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
        self.thousandths().hash(state);
    }
}

/// The file's spelling: `{"key": "<32 hex>", "box": [x, y, w, h]}`.
#[derive(Serialize, Deserialize)]
struct FaceRefJson {
    key: String,
    #[serde(rename = "box")]
    bbox: [f32; 4],
}

impl Serialize for FaceRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        FaceRefJson { key: self.key.to_string(), bbox: self.bbox }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for FaceRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<FaceRef, D::Error> {
        let j = FaceRefJson::deserialize(d)?;
        let key = parse_key(&j.key).ok_or_else(|| serde::de::Error::custom("a key is 32 hex digits"))?;
        Ok(FaceRef::new(key, j.bbox))
    }
}

fn parse_key(hex: &str) -> Option<Key> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(Key(out))
}

/// A face as the page shows it: the detection, and the shot it is in with
/// the same fields as `search::Hit` (paths, day, capture, burst), so the
/// grid builds records from the library index without reading files.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub face: FaceRef,
    pub jpg: Option<PathBuf>,
    pub raw: Option<PathBuf>,
    pub day: NaiveDate,
    pub capture: NaiveDateTime,
    /// Frames of one shutter press share a value (as the grid finds
    /// bursts); `None` for a frame on its own.
    pub burst: Option<u64>,
    /// How clear a view this is (UI 12): how sure the match to its group
    /// is, how large the face is and how sharp.  Higher is clearer; a
    /// subject's photos come in descending order of it, so doubtful
    /// members sink to the bottom.
    pub clarity: f32,
    /// A confirmed member of its subject (named into it, moved into it,
    /// or answered Yes), as against an automatic cluster member.
    pub confirmed: bool,
}

/// A group on the *Subjects* page: a subject or an automatic cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: GroupId,
    pub kind: Kind,
    /// The user's name; `None` for a cluster and for a subject hidden or
    /// edited before being named.
    pub name: Option<String>,
    pub hidden: bool,
    /// Photos (shots) it appears in.
    pub photos: usize,
    /// Days it appears on.
    pub days: usize,
    /// Its clearest face: what its tile shows.
    pub cover: Face,
    /// Faces waiting for review ("may show this person").  Zero for a
    /// cluster.
    pub suggestions: usize,
}

/// Everything the *Subjects* page shows, of every kind (a tab shows one
/// kind).  Sent as `BgEvent::Subjects` whenever it changes and readable
/// through `Engine::subjects` at any time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Subjects {
    /// Moves on whenever the clusters are recomputed; a `GroupId::Cluster`
    /// is valid for the generation it came with.
    pub generation: u64,
    /// Named subjects first (most photos first), then clusters, most
    /// photos first.  Clusters under the page's minimum fold under
    /// *Smaller groups*; the page decides by `photos` and the setting.
    pub groups: Vec<Group>,
    /// Faces found in the library (people), named or not.
    pub faces_found: usize,
    /// What Ctrl+Z would take back ("Merged 2 groups into Someone"), if
    /// anything.
    pub undoable: Option<String>,
}

impl Subjects {
    pub fn group(&self, id: &GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| &g.id == id)
    }

    /// Named subjects of `kind`, shown or hidden.
    pub fn named(&self, kind: Kind) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.kind == kind && g.name.is_some())
    }
}

/// How `Engine::crop` frames a face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Phase 3 UI: chosen by the Subjects page; constructed when it lands.
pub enum Framing {
    /// A square around the box with a margin: tiles and the review's crop.
    Face,
    /// The whole preview fitted in the square: the review's "In the
    /// photo" view (the page draws the box itself).
    Photo,
}

/// Why an operation on the subjects was refused.  Nothing is changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Two kinds in one operation: never (decision 12).
    MixedKinds,
    /// No such subject.
    NoSuchSubject(SubjectId),
    /// A cluster id from an earlier generation.
    StaleGroup,
    /// Nothing to do (no faces, the same group twice).
    Nothing,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::MixedKinds => f.write_str("people, fursuits and pets are never combined"),
            Refused::NoSuchSubject(id) => write!(f, "no subject {id}"),
            Refused::StaleGroup => f.write_str("that group has changed; look again"),
            Refused::Nothing => f.write_str("nothing to do"),
        }
    }
}

/// One subject in the user's file (§6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    pub id: SubjectId,
    /// The user's name; never written to photos.
    #[serde(default)]
    pub name: Option<String>,
    pub kind: Kind,
    #[serde(default)]
    pub hidden: bool,
    /// The face its tile shows, when the user chose one; else the clearest
    /// member.
    #[serde(default)]
    pub cover: Option<FaceRef>,
    /// Confirmed members: named into it, moved into it, answered Yes.
    #[serde(default)]
    pub members: Vec<FaceRef>,
    /// "Not this person": never suggested for it again.
    #[serde(default)]
    pub rejected: Vec<FaceRef>,
}

/// The user's subjects file: everything that is theirs and not
/// recomputable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectsFile {
    pub version: u32,
    pub subjects: Vec<Subject>,
}

impl Default for SubjectsFile {
    fn default() -> Self {
        SubjectsFile { version: FILE_VERSION, subjects: Vec::new() }
    }
}

/// `~/.local/share/photo-pipeline/subjects.json`, next to `unsaved/`.
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("photo-pipeline").join("subjects.json"))
}

impl SubjectsFile {
    /// Read the file; none, or an unreadable one, is empty (an unreadable
    /// one is kept aside as `subjects.json.unreadable` rather than
    /// overwritten).
    pub fn load(path: &Path) -> SubjectsFile {
        let Ok(text) = std::fs::read_to_string(path) else { return SubjectsFile::default() };
        match serde_json::from_str::<SubjectsFile>(&text) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("subjects: {} can't be read ({e}); keeping it aside", path.display());
                let mut aside = path.as_os_str().to_owned();
                aside.push(".unreadable");
                let _ = crate::ops::fsutil::rename_noreplace(path, Path::new(&aside));
                SubjectsFile::default()
            }
        }
    }

    /// Write the file whole and atomically, readable by the user alone.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            private_dir(dir);
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::ops::fsutil::atomic_write(path, text.as_bytes(), true)?;
        private_file(path);
        Ok(())
    }

    pub fn get(&self, id: &SubjectId) -> Option<&Subject> {
        self.subjects.iter().find(|s| &s.id == id)
    }

    fn get_mut(&mut self, id: &SubjectId) -> Result<&mut Subject, Refused> {
        self.subjects.iter_mut().find(|s| &s.id == id).ok_or_else(|| Refused::NoSuchSubject(id.clone()))
    }

    /// The subject of `kind` a face is a confirmed member of.
    pub fn member_of(&self, kind: Kind, face: &FaceRef) -> Option<&Subject> {
        self.subjects.iter().find(|s| s.kind == kind && s.members.contains(face))
    }

    /// Subjects named `name` (case and surrounding space aside) of `kind`.
    pub fn named(&self, kind: Kind, name: &str) -> Vec<&Subject> {
        let want = name.trim().to_lowercase();
        self.subjects.iter().filter(|s| s.kind == kind && s.name.as_deref().is_some_and(|n| n.trim().to_lowercase() == want)).collect()
    }

    /// A new subject of `kind` with these confirmed members (taken from
    /// any other subject of the kind that held them).
    pub fn create(&mut self, kind: Kind, name: Option<String>, members: Vec<FaceRef>) -> SubjectId {
        let id = SubjectId::fresh();
        self.subjects.push(Subject { id: id.clone(), name: clean_name(name), kind, hidden: false, cover: None, members: Vec::new(), rejected: Vec::new() });
        let _ = self.add_members(&id, members);
        id
    }

    pub fn rename(&mut self, id: &SubjectId, name: String) -> Result<(), Refused> {
        self.get_mut(id)?.name = clean_name(Some(name));
        Ok(())
    }

    pub fn set_hidden(&mut self, id: &SubjectId, hidden: bool) -> Result<(), Refused> {
        self.get_mut(id)?.hidden = hidden;
        Ok(())
    }

    #[allow(dead_code)] // Phase 3: *Change the cover* (the page's menu), wired with the face cache.
    pub fn set_cover(&mut self, id: &SubjectId, cover: Option<FaceRef>) -> Result<(), Refused> {
        self.get_mut(id)?.cover = cover;
        Ok(())
    }

    /// Forget a subject: its members become unnamed faces again.
    pub fn forget(&mut self, id: &SubjectId) -> Result<Subject, Refused> {
        let i = self.subjects.iter().position(|s| &s.id == id).ok_or_else(|| Refused::NoSuchSubject(id.clone()))?;
        Ok(self.subjects.remove(i))
    }

    /// Confirm faces as members of `id`: each leaves any other subject of
    /// the kind it was in, and is no longer rejected for `id`.
    pub fn add_members(&mut self, id: &SubjectId, faces: Vec<FaceRef>) -> Result<usize, Refused> {
        let kind = self.get(id).ok_or_else(|| Refused::NoSuchSubject(id.clone()))?.kind;
        let faces: Vec<FaceRef> = dedup(faces);
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        for s in self.subjects.iter_mut().filter(|s| s.kind == kind && &s.id != id) {
            s.members.retain(|m| !faces.contains(m));
            if s.cover.is_some_and(|c| faces.contains(&c)) {
                s.cover = None;
            }
        }
        let s = self.get_mut(id)?;
        s.rejected.retain(|r| !faces.contains(r));
        let mut added = 0;
        for f in faces {
            if !s.members.contains(&f) {
                s.members.push(f);
                added += 1;
            }
        }
        Ok(added)
    }

    /// Take faces out of `id`'s members (a split that moves them nowhere).
    pub fn remove_members(&mut self, id: &SubjectId, faces: &[FaceRef]) -> Result<usize, Refused> {
        let s = self.get_mut(id)?;
        let before = s.members.len();
        s.members.retain(|m| !faces.contains(m));
        if s.cover.is_some_and(|c| faces.contains(&c)) {
            s.cover = None;
        }
        Ok(before - s.members.len())
    }

    /// "Not this person": the faces leave `id`'s members and are never
    /// suggested for it again.
    pub fn reject(&mut self, id: &SubjectId, faces: Vec<FaceRef>) -> Result<usize, Refused> {
        let faces = dedup(faces);
        if faces.is_empty() {
            return Err(Refused::Nothing);
        }
        self.remove_members(id, &faces)?;
        let s = self.get_mut(id)?;
        let mut added = 0;
        for f in faces {
            if !s.rejected.contains(&f) {
                s.rejected.push(f);
                added += 1;
            }
        }
        Ok(added)
    }

    /// Take a rejection back (the faces may be suggested again).
    pub fn unreject(&mut self, id: &SubjectId, faces: &[FaceRef]) -> Result<usize, Refused> {
        let s = self.get_mut(id)?;
        let before = s.rejected.len();
        s.rejected.retain(|r| !faces.contains(r));
        Ok(before - s.rejected.len())
    }

    /// Merge `other` into `into` (the same kind): members and rejections
    /// move over, `other` goes.  `into` keeps its name, or takes `other`'s
    /// when it has none.
    pub fn merge(&mut self, into: &SubjectId, other: &SubjectId) -> Result<(), Refused> {
        if into == other {
            return Err(Refused::Nothing);
        }
        let (a, b) = (self.get(into).ok_or_else(|| Refused::NoSuchSubject(into.clone()))?, self.get(other).ok_or_else(|| Refused::NoSuchSubject(other.clone()))?);
        if a.kind != b.kind {
            return Err(Refused::MixedKinds);
        }
        let gone = self.forget(other)?;
        let s = self.get_mut(into)?;
        if s.name.is_none() {
            s.name = gone.name;
        }
        if s.cover.is_none() {
            s.cover = gone.cover;
        }
        for f in gone.members {
            if !s.members.contains(&f) {
                s.members.push(f);
            }
        }
        for f in gone.rejected {
            if !s.rejected.contains(&f) && !s.members.contains(&f) {
                s.rejected.push(f);
            }
        }
        Ok(())
    }

    /// Faces confirmed to any subject of `kind`, with their subject.
    #[allow(dead_code)] // Phase 3: the grouping's must-links, next commit.
    pub fn confirmed(&self, kind: Kind) -> HashMap<FaceRef, SubjectId> {
        let mut out = HashMap::new();
        for s in self.subjects.iter().filter(|s| s.kind == kind) {
            for m in &s.members {
                out.insert(*m, s.id.clone());
            }
        }
        out
    }

    /// Every face the file mentions (for keeping cache rows alive).
    #[allow(dead_code)] // Phase 3: the face cache's retention, next commit.
    pub fn faces(&self) -> HashSet<FaceRef> {
        self.subjects.iter().flat_map(|s| s.members.iter().chain(s.rejected.iter()).chain(s.cover.iter())).copied().collect()
    }

    /// Subjects named, of `kind`.
    pub fn named_count(&self, kind: Kind) -> usize {
        self.subjects.iter().filter(|s| s.kind == kind && s.name.is_some()).count()
    }
}

fn clean_name(name: Option<String>) -> Option<String> {
    name.map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|n| !n.is_empty())
}

fn dedup(faces: Vec<FaceRef>) -> Vec<FaceRef> {
    let mut seen = HashSet::new();
    faces.into_iter().filter(|f| seen.insert(*f)).collect()
}

/// `chmod 600`: biometric-like data is the user's alone (§10).
pub fn private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// `chmod 700` on a folder of such files.
pub fn private_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(n: u8, x: f32) -> FaceRef {
        FaceRef::new(Key([n; 16]), [x, 0.2, 0.1, 0.15])
    }

    #[test]
    fn face_refs_are_the_key_and_the_box_in_thousandths() {
        let a = FaceRef::new(Key([1; 16]), [0.41234, 0.18849, 0.5061, 0.3309]);
        let b = FaceRef::new(Key([1; 16]), [0.4121, 0.1884, 0.506, 0.331]);
        assert_eq!(a, b, "the same face found again, within rounding");
        assert_eq!(a.thousandths(), [412, 188, 506, 331]);
        assert_ne!(a, FaceRef::new(Key([2; 16]), a.bbox), "another photo");
        assert_ne!(a, FaceRef::new(Key([1; 16]), [0.5, 0.1885, 0.506, 0.331]));
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        // JSON as §6.5 spells it, and back.
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.starts_with(r#"{"key":"01010101"#) && json.contains(r#""box":[0.412,0.188,0.506,0.331]"#), "{json}");
        assert_eq!(serde_json::from_str::<FaceRef>(&json).unwrap(), a);
        assert!(serde_json::from_str::<FaceRef>(r#"{"key":"zz","box":[0,0,1,1]}"#).is_err());
        assert!((a.overlap(&b) - 1.0).abs() < 0.05);
        assert_eq!(a.overlap(&FaceRef::new(Key([1; 16]), [0.0, 0.0, 0.1, 0.1])), 0.0);
        assert!(a.overlap(&FaceRef::new(Key([1; 16]), [0.6, 0.3, 0.3, 0.3])) > 0.3);
        assert_eq!(SubjectId::fresh().0.len(), 32);
        assert_ne!(SubjectId::fresh(), SubjectId::fresh());
    }

    #[test]
    fn the_file_round_trips_and_is_private() {
        let d = std::env::temp_dir().join(format!("pp-subjects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("data").join("subjects.json");
        let mut f = SubjectsFile::load(&path);
        assert!(f.subjects.is_empty());
        let id = f.create(Kind::Person, Some("  Some   One ".into()), vec![face(1, 0.1), face(2, 0.1), face(1, 0.1)]);
        f.reject(&id, vec![face(3, 0.3)]).unwrap();
        f.save(&path).unwrap();
        let back = SubjectsFile::load(&path);
        assert_eq!(back, f);
        assert_eq!(back.subjects[0].name.as_deref(), Some("Some One"));
        assert_eq!(back.subjects[0].members, [face(1, 0.1), face(2, 0.1)], "deduplicated, in order");
        assert_eq!(back.version, FILE_VERSION);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(d.join("data")).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // An unreadable file is kept aside, not overwritten.
        std::fs::write(&path, "{ not json").unwrap();
        assert!(SubjectsFile::load(&path).subjects.is_empty());
        assert!(!path.exists() && d.join("data").join("subjects.json.unreadable").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn members_belong_to_one_subject_and_answers_move_them() {
        let mut f = SubjectsFile::default();
        let a = f.create(Kind::Person, Some("A".into()), vec![face(1, 0.1), face(2, 0.1)]);
        let b = f.create(Kind::Person, Some("B".into()), vec![face(3, 0.1)]);
        // Confirming a face to B takes it from A.
        f.add_members(&b, vec![face(2, 0.1)]).unwrap();
        assert_eq!(f.get(&a).unwrap().members, [face(1, 0.1)]);
        assert_eq!(f.member_of(Kind::Person, &face(2, 0.1)).unwrap().id, b);
        assert!(f.member_of(Kind::Fursuit, &face(2, 0.1)).is_none(), "another kind knows nothing of it");
        // No: out of the members, never suggested again; Yes undoes it.
        f.reject(&b, vec![face(2, 0.1)]).unwrap();
        assert!(f.get(&b).unwrap().members == [face(3, 0.1)] && f.get(&b).unwrap().rejected == [face(2, 0.1)]);
        f.add_members(&b, vec![face(2, 0.1)]).unwrap();
        assert!(f.get(&b).unwrap().rejected.is_empty());
        assert_eq!(f.add_members(&b, vec![]), Err(Refused::Nothing));
        assert_eq!(f.unreject(&b, &[face(9, 0.1)]).unwrap(), 0);
        // Move (a confirmation elsewhere), merge, forget.
        f.add_members(&a, vec![face(3, 0.1)]).unwrap();
        assert_eq!(f.get(&a).unwrap().members, [face(1, 0.1), face(3, 0.1)]);
        assert_eq!(f.get(&b).unwrap().members, [face(2, 0.1)]);
        f.merge(&a, &b).unwrap();
        assert!(f.get(&b).is_none());
        assert_eq!(f.get(&a).unwrap().members, [face(1, 0.1), face(3, 0.1), face(2, 0.1)]);
        assert_eq!(f.named(Kind::Person, " a ").len(), 1);
        assert_eq!(f.named_count(Kind::Person), 1);
        assert_eq!(f.confirmed(Kind::Person).len(), 3);
        assert_eq!(f.faces().len(), 3);
        f.rename(&a, "A renamed".into()).unwrap();
        f.set_hidden(&a, true).unwrap();
        f.set_cover(&a, Some(face(3, 0.1))).unwrap();
        f.remove_members(&a, &[face(3, 0.1)]).unwrap();
        assert!(f.get(&a).unwrap().cover.is_none(), "a cover that leaves is forgotten");
        assert!(f.get(&a).unwrap().hidden && f.get(&a).unwrap().name.as_deref() == Some("A renamed"));
        f.forget(&a).unwrap();
        assert!(f.subjects.is_empty());
        assert_eq!(f.rename(&a, "x".into()), Err(Refused::NoSuchSubject(a.clone())));
        assert!(matches!(f.forget(&a), Err(Refused::NoSuchSubject(_))));
    }

    /// Decision 12: people, fursuits and pets are never combined, by
    /// merge or by move; the file refuses it and changes nothing.
    #[test]
    fn kinds_never_mix_in_the_file() {
        let mut f = SubjectsFile::default();
        let person = f.create(Kind::Person, Some("Someone".into()), vec![face(1, 0.1)]);
        let suit = f.create(Kind::Fursuit, Some("Suit".into()), vec![face(2, 0.1)]);
        let pet = f.create(Kind::Pet, Some("Pet".into()), vec![face(3, 0.1)]);
        let before = f.clone();
        for (a, b) in [(&person, &suit), (&suit, &person), (&person, &pet), (&pet, &suit)] {
            assert_eq!(f.merge(a, b), Err(Refused::MixedKinds));
        }
        assert_eq!(f, before, "a refusal changes nothing");
        // Confirming a face to a pet never takes it from a person: kinds
        // have their own detections.
        f.add_members(&pet, vec![face(1, 0.1)]).unwrap();
        assert_eq!(f.get(&person).unwrap().members, [face(1, 0.1)]);
        assert_eq!(Refused::MixedKinds.to_string(), "people, fursuits and pets are never combined");
        assert_eq!(Kind::Person.noun(), "person");
        assert_eq!(Kind::Fursuit.plural(), "Fursuits");
    }
}
