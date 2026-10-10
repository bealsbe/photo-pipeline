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

/// The file's format version as written when it holds one of the
/// optional lists added after the first ([`SubjectsFile::not_same`], and
/// the viewer's [`SubjectsFile::added`] and [`SubjectsFile::removed`]):
/// every such list is `#[serde(default)]`, and a file without any is
/// written as [`FILE_VERSION_PLAIN`], exactly as before, so an older app
/// still reads it ([`SubjectsFile::written_version`]).
pub const FILE_VERSION: u32 = 2;
/// The first format: subjects only.
pub const FILE_VERSION_PLAIN: u32 = 1;
/// Heads kept per side of a "not the same" answer: the clearest of each
/// group, enough to recognise it after a regroup.
pub const NOT_SAME_HEADS: usize = 24;
/// A detection matches a removed box when they overlap at least this
/// much (intersection over union): a box found again a thousandth off,
/// after a rebuild, is still the one the user removed.
pub const REMOVED_IOU: f32 = 0.6;
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

    /// Intersection over union of two boxes of one photo (0 for two
    /// photos).
    pub fn iou(&self, other: &FaceRef) -> f32 {
        if self.key != other.key {
            return 0.0;
        }
        iou_boxes(&self.bbox, &other.bbox)
    }
}

/// Intersection over union of two `[x, y, w, h]` boxes.
pub fn iou_boxes(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
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
    /// The bursts those photos fall in, a photo on its own counting as
    /// one ([`count_bursts`]): what the tabs sort and fold groups by, so
    /// a long burst does not make a group look established.
    pub bursts: usize,
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
    /// Fursuit groups holding two heads of one photo, so at least two
    /// fursuits (`search::splitting`).  Never a person's.
    pub seen_together: HashSet<GroupId>,
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

/// Where a labelled box's name goes (the viewer's labels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameTo {
    /// Join this subject (of the box's own kind; another is refused).
    Subject(SubjectId),
    /// This name: the subject of the kind called so, else a new one.
    New(String),
    /// Out of the subject it was in, and not suggested for it again.
    Unnamed,
}

/// A change made on a box in the viewer's labels.  Each is one undo step.
#[derive(Debug, Clone, PartialEq)]
pub enum LabelEdit {
    /// Name the box (or move it to "Unnamed").
    Name { face: FaceRef, kind: Kind, to: NameTo },
    /// "This is a fursuit, not a face" (or the reverse): out of `from`
    /// entirely, into `to`, named there if `name` says so.
    Kind { face: FaceRef, from: Kind, to: Kind, name: Option<NameTo> },
    /// A box the detector missed, drawn by the user.
    Add { face: FaceRef, kind: Kind, name: Option<NameTo> },
    /// "Not a face" / "Not a fursuit head".
    Remove { face: FaceRef, kind: Kind },
}

impl LabelEdit {
    /// The box the edit is about.
    pub fn face(&self) -> FaceRef {
        match self {
            LabelEdit::Name { face, .. } | LabelEdit::Kind { face, .. } | LabelEdit::Add { face, .. } | LabelEdit::Remove { face, .. } => *face,
        }
    }
}

/// One box of the viewer's labels: a face or a fursuit head found in the
/// photo, or drawn by the user, with its name.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelBox {
    pub face: FaceRef,
    pub kind: Kind,
    /// The subject it is a confirmed member of, and that subject's name
    /// (`None`: "Unnamed").
    pub subject: Option<SubjectId>,
    pub name: Option<String>,
    /// The user drew it.
    pub drawn: bool,
    /// Drawn, and not matched yet (no vector): it waits for the card.
    pub pending: bool,
}

/// Why drawn boxes wait to be matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingWhy {
    /// On the graphics card now.
    Matching,
    /// The processor is chosen: drawn boxes are matched on the card only.
    ProcessorChosen,
    /// The card failed or isn't there.
    NoCard,
    /// The face model isn't downloaded.
    NoFaceModel,
    /// The search model isn't downloaded.
    NoSearchModel,
}

/// The viewer's labels for one photo (`Engine::photo_labels`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhotoLabels {
    /// The photo's content key in the index; `None`: the photo isn't in
    /// the library (or not indexed yet), so nothing was looked for.
    pub key: Option<Key>,
    /// People's faces first, then fursuit heads; never one box as both.
    pub boxes: Vec<LabelBox>,
    /// Why the pending boxes wait, when some do.
    pub pending: Option<PendingWhy>,
}

/// Which photo the viewer shows, for its labels: its files (the engine
/// finds its key: from the library's paths, else by reading the file's
/// head on a worker), or a key already known.
#[derive(Debug, Clone, PartialEq)]
pub enum LabelTarget {
    Files { jpg: Option<PathBuf>, raw: Option<PathBuf> },
    Key(Key),
}

/// What a box is called in copy: "face", "fursuit head", "pet".
pub fn box_noun(kind: Kind) -> &'static str {
    match kind {
        Kind::Person => "face",
        Kind::Fursuit => "fursuit head",
        Kind::Pet => "pet",
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

/// "Not the same": the user's answer that two groups of one kind show
/// different subjects (a merge suggestion turned down).  Kept by the
/// groups' heads, never their numbers, so it holds through a regroup: a
/// pair of groups is told apart while one holds a head of `a` and the
/// other a head of `b`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotSame {
    pub kind: Kind,
    pub a: Vec<FaceRef>,
    pub b: Vec<FaceRef>,
}

/// The user's subjects file: everything that is theirs and not
/// recomputable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectsFile {
    pub version: u32,
    pub subjects: Vec<Subject>,
    /// Boxes the user drew in the viewer, of either kind (an optional
    /// list, version 2).  Their vectors are cache, made again after a
    /// rebuild.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<Labelled>,
    /// Detections the user removed ("Not a face", "Not a fursuit head";
    /// an optional list, version 2): never shown, grouped or suggested
    /// as that kind again, however often the photo is looked at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<Labelled>,
    /// Groups the user said are not the same (v2; absent in a v1 file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_same: Vec<NotSame>,
}

impl Default for SubjectsFile {
    fn default() -> Self {
        SubjectsFile { version: FILE_VERSION_PLAIN, subjects: Vec::new(), added: Vec::new(), removed: Vec::new(), not_same: Vec::new() }
    }
}

/// A box the user labelled in the viewer: drawn (a face or head the
/// detector missed) or removed.  Spelled `{"kind": "person", "key":
/// "<32 hex>", "box": [x, y, w, h]}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Labelled {
    pub kind: Kind,
    pub face: FaceRef,
}

#[derive(Serialize, Deserialize)]
struct LabelledJson {
    kind: Kind,
    #[serde(flatten)]
    face: FaceRefJson,
}

impl Serialize for Labelled {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        LabelledJson { kind: self.kind, face: FaceRefJson { key: self.face.key.to_string(), bbox: self.face.bbox } }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Labelled {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Labelled, D::Error> {
        let j = LabelledJson::deserialize(d)?;
        let key = parse_key(&j.face.key).ok_or_else(|| serde::de::Error::custom("a key is 32 hex digits"))?;
        Ok(Labelled { kind: j.kind, face: FaceRef::new(key, j.face.bbox) })
    }
}

/// The removed boxes of one kind, by photo, for matching detections
/// quickly ([`SubjectsFile::removed_index`]).
#[derive(Debug, Clone, Default)]
pub struct Removed {
    by_key: HashMap<Key, Vec<[f32; 4]>>,
}

impl Removed {
    /// The user removed this detection (or one it overlaps closely).
    pub fn covers(&self, r: &FaceRef) -> bool {
        self.by_key.get(&r.key).is_some_and(|boxes| boxes.iter().any(|b| iou_boxes(b, &r.bbox) >= REMOVED_IOU))
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
        let mut out = self.clone();
        out.version = self.written_version();
        let text = serde_json::to_string_pretty(&out).map_err(io::Error::other)?;
        crate::ops::fsutil::atomic_write(path, text.as_bytes(), true)?;
        private_file(path);
        Ok(())
    }

    /// The version the file is written as: 2 while any optional list
    /// (`not_same`, `added`, `removed`) holds something, else 1 (a v1
    /// file saves back unchanged).  The one place the rule lives.
    pub fn written_version(&self) -> u32 {
        if self.not_same.is_empty() && self.added.is_empty() && self.removed.is_empty() { FILE_VERSION_PLAIN } else { FILE_VERSION }
    }

    /// Nothing of the user's is kept (the file can go): no subject and
    /// no optional list.
    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty() && self.written_version() == FILE_VERSION_PLAIN
    }

    /// *Forget all* of a kind: its subjects, its "not the same" answers
    /// and its labels from the viewer go; the other kinds' stay.
    pub fn forget_kind(&mut self, kind: Kind) {
        self.subjects.retain(|s| s.kind != kind);
        self.not_same.retain(|n| n.kind != kind);
        self.added.retain(|l| l.kind != kind);
        self.removed.retain(|l| l.kind != kind);
    }

    /// "Not the same": two groups of `kind`, by their heads (clearest
    /// first; the first [`NOT_SAME_HEADS`] of each are kept).
    pub fn add_not_same(&mut self, kind: Kind, a: Vec<FaceRef>, b: Vec<FaceRef>) -> Result<(), Refused> {
        let (a, b): (Vec<FaceRef>, Vec<FaceRef>) = (dedup(a).into_iter().take(NOT_SAME_HEADS).collect(), dedup(b).into_iter().take(NOT_SAME_HEADS).collect());
        if a.is_empty() || b.is_empty() {
            return Err(Refused::Nothing);
        }
        self.not_same.push(NotSame { kind, a, b });
        Ok(())
    }

    /// Where each head of `kind` stands in the "not the same" answers:
    /// (answer, side 0 or 1) for every answer naming it.
    pub fn not_same_index(&self, kind: Kind) -> HashMap<FaceRef, Vec<(usize, u8)>> {
        let mut out: HashMap<FaceRef, Vec<(usize, u8)>> = HashMap::new();
        for (i, ns) in self.not_same.iter().enumerate().filter(|(_, n)| n.kind == kind) {
            for f in &ns.a {
                out.entry(*f).or_default().push((i, 0));
            }
            for f in &ns.b {
                out.entry(*f).or_default().push((i, 1));
            }
        }
        out
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
    pub fn confirmed(&self, kind: Kind) -> HashMap<FaceRef, SubjectId> {
        let mut out = HashMap::new();
        for s in self.subjects.iter().filter(|s| s.kind == kind) {
            for m in &s.members {
                out.insert(*m, s.id.clone());
            }
        }
        out
    }

    /// Subjects named, of `kind`.
    pub fn named_count(&self, kind: Kind) -> usize {
        self.subjects.iter().filter(|s| s.kind == kind && s.name.is_some()).count()
    }

    // ---- the viewer's labels (version 2) ----

    /// The removed boxes of `kind`, indexed for [`Removed::covers`].
    pub fn removed_index(&self, kind: Kind) -> Removed {
        let mut by_key: HashMap<Key, Vec<[f32; 4]>> = HashMap::new();
        for l in self.removed.iter().filter(|l| l.kind == kind) {
            by_key.entry(l.face.key).or_default().push(l.face.bbox);
        }
        Removed { by_key }
    }

    /// The user drew this box, as `kind`.
    pub fn is_added(&self, kind: Kind, face: &FaceRef) -> bool {
        self.added.contains(&Labelled { kind, face: *face })
    }

    /// The boxes drawn as `kind`.
    pub fn added_of(&self, kind: Kind) -> impl Iterator<Item = &FaceRef> {
        self.added.iter().filter(move |l| l.kind == kind).map(|l| &l.face)
    }

    /// A box the detector missed, drawn as `kind`.  Refused (nothing)
    /// when the user drew it already; a removal of the same box as this
    /// kind is taken back.
    pub fn add_box(&mut self, kind: Kind, face: FaceRef) -> Result<(), Refused> {
        let l = Labelled { kind, face };
        let removed = self.removed.len();
        self.removed.retain(|r| !(r.kind == kind && r.face.iou(&face) >= REMOVED_IOU));
        if self.added.contains(&l) {
            return if self.removed.len() < removed { Ok(()) } else { Err(Refused::Nothing) };
        }
        self.added.push(l);
        Ok(())
    }

    /// "Not a face" (or "Not a fursuit head"): the box leaves every
    /// subject of `kind` (members, cover and answers) and is never shown
    /// or grouped as `kind` again.  A box the user drew just goes; a
    /// detection is remembered as removed, so a rebuild doesn't bring it
    /// back.  The other kinds know nothing of it.
    pub fn remove_box(&mut self, kind: Kind, face: FaceRef) -> Result<(), Refused> {
        for s in self.subjects.iter_mut().filter(|s| s.kind == kind) {
            s.members.retain(|m| *m != face);
            s.rejected.retain(|m| *m != face);
            if s.cover == Some(face) {
                s.cover = None;
            }
        }
        let l = Labelled { kind, face };
        if self.added.contains(&l) {
            self.added.retain(|a| *a != l);
            return Ok(());
        }
        if self.removed_index(kind).covers(&face) {
            return Err(Refused::Nothing);
        }
        self.removed.push(l);
        Ok(())
    }

    /// "It's the other kind": the box leaves `from` entirely (as
    /// [`SubjectsFile::remove_box`]) and is drawn as `to`.  Never a mix:
    /// no subject of one kind ever holds it as the other.
    pub fn change_kind(&mut self, face: FaceRef, from: Kind, to: Kind) -> Result<(), Refused> {
        if from == to {
            return Err(Refused::Nothing);
        }
        match self.remove_box(from, face) {
            Ok(()) | Err(Refused::Nothing) => {}
            Err(e) => return Err(e),
        }
        match self.add_box(to, face) {
            Ok(()) | Err(Refused::Nothing) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Name a box: it joins `id`, a subject of its own kind (another
    /// kind is refused and nothing changes).
    pub fn name_box(&mut self, kind: Kind, face: FaceRef, id: &SubjectId) -> Result<(), Refused> {
        let s = self.get(id).ok_or_else(|| Refused::NoSuchSubject(id.clone()))?;
        if s.kind != kind {
            return Err(Refused::MixedKinds);
        }
        if s.members.contains(&face) {
            return Err(Refused::Nothing);
        }
        self.add_members(id, vec![face]).map(|_| ())
    }

    /// "Unnamed": the box leaves the subject of `kind` it was in and
    /// isn't suggested for it again.
    pub fn unname_box(&mut self, kind: Kind, face: FaceRef) -> Result<SubjectId, Refused> {
        let id = self.member_of(kind, &face).map(|s| s.id.clone()).ok_or(Refused::Nothing)?;
        self.reject(&id, vec![face])?;
        Ok(id)
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

// ---- Grouping (pure) ------------------------------------------------------

/// A kind's cut-offs, as cosine distances (1 − cosine).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cutoffs {
    /// Within this of a group's prototype a face belongs to it (the
    /// automatic groups).
    pub group: f32,
    /// Within this of a subject's confirmed members a face is offered
    /// for review.
    pub suggest: f32,
    /// *Find more*: the same, stricter.
    pub strict: f32,
}

/// The face cut-offs per model: the study's (AuraFace 0.6, SFace 0.5;
/// InsightFace as Immich's 0.5), to be moved by phase 0's comparison;
/// *Find more* at three quarters of it.  Fursuit heads have their own,
/// in whitened distance (`search::heads::CUTOFFS`, v2).
pub fn cutoff_for(model: crate::ml::models::ModelId) -> Cutoffs {
    use crate::ml::models::ModelId;
    let c = match model {
        ModelId::AuraFace => 0.6,
        ModelId::SFace => 0.5,
        _ => 0.5,
    };
    Cutoffs { group: c, suggest: c, strict: c * STRICT_SHARE }
}

/// *Find more*'s stricter cut-off: a share of the model's.
pub const STRICT_SHARE: f32 = 0.75;

/// Vectors the grouping reads: `dim` values per row, unit length, as
/// f32 or as f16 bits (a kind with wide vectors keeps them as f16 in
/// memory; the dot products decode on the fly).
#[derive(Clone, Copy)]
pub enum Rows<'a> {
    F32 { dim: usize, data: &'a [f32] },
    F16 { dim: usize, data: &'a [u16] },
}

impl Rows<'_> {
    pub fn dim(&self) -> usize {
        match self {
            Rows::F32 { dim, .. } | Rows::F16 { dim, .. } => *dim,
        }
    }

    pub fn len(&self) -> usize {
        let dim = self.dim();
        if dim == 0 {
            return 0;
        }
        match self {
            Rows::F32 { data, .. } => data.len() / dim,
            Rows::F16 { data, .. } => data.len() / dim,
        }
    }

    /// Row `i` as f32.
    pub fn row_vec(&self, i: usize) -> Vec<f32> {
        let dim = self.dim();
        match self {
            Rows::F32 { data, .. } => data[i * dim..(i + 1) * dim].to_vec(),
            Rows::F16 { data, .. } => data[i * dim..(i + 1) * dim].iter().map(|&h| crate::search::query::f16_to_f32(h)).collect(),
        }
    }

    /// The dot product of row `i` with `v`.
    pub fn dot(&self, i: usize, v: &[f32]) -> f32 {
        let dim = self.dim();
        match self {
            Rows::F32 { data, .. } => dot(&data[i * dim..(i + 1) * dim], v),
            Rows::F16 { data, .. } => data[i * dim..(i + 1) * dim].iter().zip(v).map(|(&h, y)| crate::search::query::f16_to_f32(h) * y).sum(),
        }
    }

    /// Add row `i` into `sum`.
    pub fn add_into(&self, i: usize, sum: &mut [f32]) {
        let dim = self.dim();
        match self {
            Rows::F32 { data, .. } => {
                for (s, x) in sum.iter_mut().zip(&data[i * dim..(i + 1) * dim]) {
                    *s += x;
                }
            }
            Rows::F16 { data, .. } => {
                for (s, &h) in sum.iter_mut().zip(&data[i * dim..(i + 1) * dim]) {
                    *s += crate::search::query::f16_to_f32(h);
                }
            }
        }
    }

    /// Every row as f32, row-major (a copy for a worker).
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            Rows::F32 { data, .. } => data.to_vec(),
            Rows::F16 { data, .. } => data.iter().map(|&h| crate::search::query::f16_to_f32(h)).collect(),
        }
    }
}

/// An automatic group of rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Row indices, in the order they joined (clearest first).
    pub members: Vec<usize>,
    /// The unit mean of the members' vectors.
    pub centroid: Vec<f32>,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = dot(&v, &v).sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// How a kind's rows are grouped into the automatic groups.  People use
/// [`Linkage::Centroid`] with the merge pass, as the first grouping did;
/// a kind whose heads vary more under the lights (fursuits, phase 4)
/// chooses what its labels support.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Linkage {
    /// A row joins the nearest cluster whose centroid lies within the
    /// cut; with `merge`, clusters whose centroids lie within the cut are
    /// merged afterwards, until none do.  `seed_cap`: a row also has to
    /// lie within this of the cluster's first (clearest) member, which
    /// stops a centroid drifting away from what it started as.
    Centroid { merge: bool, seed_cap: Option<f32> },
    /// A row joins the cluster whose members lie on average within the
    /// cut of it, the closest such; no merge pass.  Fursuits' grouping.
    Average,
    /// As [`Linkage::Average`], but every member must lie within the cut.
    #[allow(dead_code)] // Scored in the phase 4 tune (`heads.rs` tests); no kind uses it.
    Complete,
}

impl Linkage {
    /// The first grouping, people's: nearest centroid with the merge pass.
    pub const PEOPLE: Linkage = Linkage::Centroid { merge: true, seed_cap: None };
}

impl Default for Linkage {
    fn default() -> Linkage {
        Linkage::PEOPLE
    }
}

/// Group rows: each row, in `order` (clearest first), joins a cluster as
/// `linkage` says or starts one (under [`Linkage::PEOPLE`]: the nearest
/// centroid within `cutoff`, then clusters whose centroids lie within
/// `cutoff` of each other are merged, until none do).  Rows outside
/// `order` (confirmed to a subject) take no part.  Pure.
pub fn cluster_with(rows: &Rows<'_>, order: &[usize], cutoff: f32, linkage: Linkage) -> Vec<Cluster> {
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut sums: Vec<Vec<f32>> = Vec::new();
    for &i in order {
        if i >= rows.len() {
            continue;
        }
        let v = rows.row_vec(i);
        match join(&clusters, rows, &v, cutoff, linkage) {
            Some(k) => {
                clusters[k].members.push(i);
                for (s, x) in sums[k].iter_mut().zip(&v) {
                    *s += x;
                }
                clusters[k].centroid = unit(sums[k].clone());
            }
            None => {
                clusters.push(Cluster { members: vec![i], centroid: v.clone() });
                sums.push(v);
            }
        }
    }
    if let Linkage::Centroid { merge: true, .. } = linkage {
        merge_close(&mut clusters, rows, cutoff);
    }
    clusters.sort_by(|a, b| b.members.len().cmp(&a.members.len()).then_with(|| a.members.first().cmp(&b.members.first())));
    clusters
}

/// The cluster `v` joins under `linkage`, if any.  One pass over the
/// centroids (no copies); the member-based linkages then look only at
/// the clusters whose centroid lies within the cut (an exact bound).
fn join(clusters: &[Cluster], rows: &Rows<'_>, v: &[f32], cutoff: f32, linkage: Linkage) -> Option<usize> {
    use rayon::prelude::*;
    let centroid_distance = |c: &Cluster| 1.0 - dot(&c.centroid, v);
    match linkage {
        Linkage::Centroid { seed_cap, .. } => {
            let nearest = if clusters.len() < 256 {
                clusters.iter().enumerate().map(|(k, c)| (k, centroid_distance(c))).min_by(|a, b| a.1.total_cmp(&b.1))
            } else {
                clusters.par_iter().enumerate().map(|(k, c)| (k, centroid_distance(c))).min_by(|a, b| a.1.total_cmp(&b.1))
            };
            let (k, d) = nearest?;
            if d > cutoff {
                return None;
            }
            match seed_cap {
                Some(cap) => {
                    let seed = *clusters[k].members.first()?;
                    (1.0 - rows.dot(seed, v) <= cap).then_some(k)
                }
                None => Some(k),
            }
        }
        Linkage::Average | Linkage::Complete => {
            // For unit vectors the mean distance to the members is
            // 1 − v·(sum/n) ≥ 1 − v·centroid (the centroid is the unit
            // sum, and ‖sum‖/n ≤ 1), and the largest distance is at least
            // the mean: so a cluster whose centroid lies beyond the cut
            // can't qualify, and the shortlist is exact.
            let near: Vec<usize> = if clusters.len() < 256 {
                clusters.iter().enumerate().filter(|(_, c)| centroid_distance(c) <= cutoff).map(|(k, _)| k).collect()
            } else {
                clusters.par_iter().enumerate().filter(|(_, c)| centroid_distance(c) <= cutoff).map(|(k, _)| k).collect()
            };
            let mut best: Option<(usize, f32)> = None;
            for k in near {
                let c = &clusters[k];
                let ds = c.members.iter().filter(|&&m| m < rows.len()).map(|&m| 1.0 - rows.dot(m, v));
                let score = match linkage {
                    Linkage::Average => {
                        let (n, sum) = ds.fold((0usize, 0f32), |(n, s), d| (n + 1, s + d));
                        if n == 0 { continue } else { sum / n as f32 }
                    }
                    _ => ds.fold(f32::MIN, f32::max),
                };
                if score <= cutoff && best.is_none_or(|(_, b)| score < b) {
                    best = Some((k, score));
                }
            }
            best.map(|(k, _)| k)
        }
    }
}

/// Merge clusters whose centroids lie within `cutoff`, repeatedly.
fn merge_close(clusters: &mut Vec<Cluster>, rows: &Rows<'_>, cutoff: f32) {
    for _ in 0..4 {
        let n = clusters.len();
        if n < 2 {
            return;
        }
        // Union-find over close pairs.
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while p[r] != r {
                r = p[r];
            }
            let mut c = i;
            while p[c] != r {
                let next = p[c];
                p[c] = r;
                c = next;
            }
            r
        }
        let mut merged = false;
        for a in 0..n {
            for b in a + 1..n {
                if 1.0 - dot(&clusters[a].centroid, &clusters[b].centroid) <= cutoff {
                    let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                    if ra != rb {
                        parent[rb.max(ra)] = ra.min(rb);
                        merged = true;
                    }
                }
            }
        }
        if !merged {
            return;
        }
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            groups.entry(r).or_default().extend(clusters[i].members.iter().copied());
        }
        *clusters = groups
            .into_values()
            .map(|members| {
                let mut sum = vec![0f32; rows.dim()];
                for &m in &members {
                    rows.add_into(m, &mut sum);
                }
                Cluster { members, centroid: unit(sum) }
            })
            .collect();
    }
}

/// Add rows to existing clusters as they arrive (the pass): each joins
/// a cluster as `linkage` says or starts one.  The full [`cluster_with`]
/// runs later to tidy up.
pub fn assign_with(clusters: &mut Vec<Cluster>, rows: &Rows<'_>, new: &[usize], cutoff: f32, linkage: Linkage) {
    for &i in new {
        if i >= rows.len() {
            continue;
        }
        let v = rows.row_vec(i);
        match join(clusters, rows, &v, cutoff, linkage) {
            Some(k) => {
                let c = &mut clusters[k];
                let n = c.members.len() as f32;
                let mut sum: Vec<f32> = c.centroid.iter().map(|x| x * n).collect();
                for (s, x) in sum.iter_mut().zip(&v) {
                    *s += x;
                }
                c.members.push(i);
                c.centroid = unit(sum);
            }
            _ => clusters.push(Cluster { members: vec![i], centroid: v }),
        }
    }
}

/// How sure a face's membership is, 0..1: 1 at the prototype, 0 at the
/// cut-off and beyond.
pub fn sureness(distance: f32, cutoff: f32) -> f32 {
    (1.0 - distance / cutoff.max(1e-6)).clamp(0.0, 1.0)
}

/// The smallest distance from `v` to any of `members`' rows (a subject's
/// prototypes are its confirmed faces).
pub fn distance_to_members(rows: &Rows<'_>, members: &[usize], v: &[f32]) -> Option<f32> {
    members.iter().filter(|&&m| m < rows.len()).map(|&m| 1.0 - rows.dot(m, v)).min_by(f32::total_cmp)
}

/// Candidates for a subject's review: rows in `pool` within `cutoff` of
/// any of `members`, as (row, distance), nearest first.
pub fn candidates(rows: &Rows<'_>, members: &[usize], pool: &[usize], cutoff: f32) -> Vec<(usize, f32)> {
    use rayon::prelude::*;
    let mut out: Vec<(usize, f32)> = pool
        .par_iter()
        .filter_map(|&i| {
            if i >= rows.len() {
                return None;
            }
            let d = distance_to_members(rows, members, &rows.row_vec(i))?;
            (d <= cutoff).then_some((i, d))
        })
        .collect();
    out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    out
}

/// One suggestion per burst (UI 6): `items` nearest first, each with its
/// burst (`None`: a frame on its own); frames of one burst are gathered
/// under the first (nearest) of them, keeping their order.
pub fn per_burst<T>(items: Vec<(T, Option<u64>)>) -> Vec<Vec<T>> {
    let mut out: Vec<Vec<T>> = Vec::new();
    let mut slot: HashMap<u64, usize> = HashMap::new();
    for (item, burst) in items {
        match burst {
            Some(b) => match slot.get(&b) {
                Some(&i) => out[i].push(item),
                None => {
                    slot.insert(b, out.len());
                    out.push(vec![item]);
                }
            },
            None => out.push(vec![item]),
        }
    }
    out
}

/// How many separate moments `faces` come from: photos (a photo with two
/// heads is one), with the frames of one burst gathered into one and a
/// photo in no burst counting alone.  The same burst notion as
/// [`per_burst`], so a group's count agrees with its reviews.
pub fn count_bursts(faces: &[Face]) -> usize {
    let mut seen: HashSet<Key> = HashSet::new();
    let photos: Vec<((), Option<u64>)> = faces.iter().filter(|f| seen.insert(f.face.key)).map(|f| ((), f.burst)).collect();
    per_burst(photos).len()
}

/// Whether the user said two groups are not the same: some answer in
/// `index` ([`SubjectsFile::not_same_index`]) names a head of `a` on one
/// side and a head of `b` on the other.  Groups are known by their
/// heads, so this holds through a regroup.
pub fn told_apart(index: &HashMap<FaceRef, Vec<(usize, u8)>>, a: &[FaceRef], b: &[FaceRef]) -> bool {
    if index.is_empty() {
        return false;
    }
    let sides = |m: &[FaceRef]| -> HashSet<(usize, u8)> { m.iter().filter_map(|f| index.get(f)).flatten().copied().collect() };
    let (sa, sb) = (sides(a), sides(b));
    sa.iter().any(|&(i, side)| sb.contains(&(i, 1 - side)))
}

/// A stable id for a cluster: its clearest member's reference, hashed.
pub fn cluster_id(cover: &FaceRef) -> u64 {
    let mut bytes = Vec::with_capacity(24);
    bytes.extend_from_slice(&cover.key.0);
    for t in cover.thousandths() {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    crate::model::binfile::fnv1a(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(n: u8, x: f32) -> FaceRef {
        FaceRef::new(Key([n; 16]), [x, 0.2, 0.1, 0.15])
    }

    fn shot(n: u8, x: f32, burst: Option<u64>) -> Face {
        let day = NaiveDate::from_ymd_opt(2024, 6, 8).unwrap();
        Face { face: face(n, x), jpg: None, raw: None, day, capture: day.and_hms_opt(12, 0, 0).unwrap(), burst, clarity: 0.5, confirmed: false }
    }

    /// A group's size in moments: a burst is one, a photo on its own is
    /// one, and a photo with two heads is still one photo.
    #[test]
    fn a_burst_counts_once_and_singles_count_each() {
        assert_eq!(count_bursts(&[]), 0);
        // Twenty frames of one burst.
        let burst: Vec<Face> = (0..20).map(|n| shot(n, 0.3, Some(7))).collect();
        assert_eq!(count_bursts(&burst), 1);
        // Five photos on their own.
        let singles: Vec<Face> = (0..5).map(|n| shot(n, 0.3, None)).collect();
        assert_eq!(count_bursts(&singles), 5);
        // Two bursts and three singles.
        let mixed: Vec<Face> = [Some(1), Some(1), Some(2), Some(2), Some(2), None, None, None].iter().enumerate().map(|(n, b)| shot(n as u8, 0.3, *b)).collect();
        assert_eq!(count_bursts(&mixed), 5);
        // Two heads in one photo are one photo, and so one moment.
        assert_eq!(count_bursts(&[shot(1, 0.2, None), shot(1, 0.7, None)]), 1);
        assert_eq!(count_bursts(&[shot(1, 0.2, Some(3)), shot(1, 0.7, Some(3)), shot(2, 0.2, Some(3))]), 1);
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
        assert_eq!(back.version, FILE_VERSION_PLAIN, "no optional list: written as the first version");
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

    /// A file from before "not the same" (v1) loads, and saves back
    /// exactly as it was; one with answers is written as v2 and reads back
    /// the same.  The answers are additive: nothing else changes shape.
    #[test]
    fn an_old_file_loads_and_saves_back_unchanged() {
        let d = std::env::temp_dir().join(format!("pp-subjects-v1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        std::fs::create_dir_all(&d).unwrap();
        let v1 = concat!(
            "{\n",
            "  \"version\": 1,\n",
            "  \"subjects\": [\n",
            "    {\n",
            "      \"id\": \"5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e\",\n",
            "      \"name\": \"Suit\",\n",
            "      \"kind\": \"fursuit\",\n",
            "      \"hidden\": false,\n",
            "      \"cover\": null,\n",
            "      \"members\": [\n",
            "        {\n",
            "          \"key\": \"01010101010101010101010101010101\",\n",
            "          \"box\": [\n",
            "            0.1,\n",
            "            0.2,\n",
            "            0.1,\n",
            "            0.15\n",
            "          ]\n",
            "        }\n",
            "      ],\n",
            "      \"rejected\": []\n",
            "    }\n",
            "  ]\n",
            "}"
        );
        std::fs::write(&path, v1).unwrap();
        let f = SubjectsFile::load(&path);
        assert_eq!((f.version, f.subjects.len(), f.not_same.len()), (1, 1, 0));
        f.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1, "saved back unchanged");
        // With an answer: v2, and back the same.
        let mut f = f;
        f.add_not_same(Kind::Fursuit, vec![face(1, 0.1), face(2, 0.1)], vec![face(3, 0.1)]).unwrap();
        f.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 2") && text.contains("\"not_same\""), "{text}");
        let back = SubjectsFile::load(&path);
        assert_eq!((back.version, back.not_same.clone()), (2, f.not_same.clone()));
        assert_eq!(back.subjects, f.subjects);
        // Taken back (undo, forget): v1 again.
        let mut back = back;
        back.not_same.clear();
        back.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Not the same" is kept by heads, so it holds after a regroup gives
    /// the groups new numbers and members: two groups are told apart while
    /// one holds a head of one side and the other a head of the other.
    /// Answers about another kind never count.
    #[test]
    fn not_the_same_holds_through_a_regroup_and_by_kind() {
        let mut f = SubjectsFile::default();
        let many: Vec<FaceRef> = (0..40).map(|i| face(100 + i, 0.1)).collect();
        f.add_not_same(Kind::Fursuit, many.clone(), vec![face(3, 0.1), face(4, 0.1)]).unwrap();
        assert_eq!(f.not_same[0].a.len(), NOT_SAME_HEADS, "the clearest heads are kept, not every one");
        assert_eq!(f.add_not_same(Kind::Fursuit, vec![], vec![face(5, 0.1)]), Err(Refused::Nothing));
        f.add_not_same(Kind::Person, vec![face(7, 0.1)], vec![face(8, 0.1)]).unwrap();
        let ix = f.not_same_index(Kind::Fursuit);
        // After a regroup: other members around the same heads.
        let x = vec![face(9, 0.1), many[3]];
        let y = vec![face(4, 0.1), face(11, 0.1)];
        assert!(told_apart(&ix, &x, &y) && told_apart(&ix, &y, &x), "either way round");
        assert!(!told_apart(&ix, &x, &[face(12, 0.1)]), "a group the answer doesn't name");
        assert!(!told_apart(&ix, &x, &[many[5]]), "two groups of the same side are not told apart by it");
        assert!(!told_apart(&f.not_same_index(Kind::Fursuit), &[face(7, 0.1)], &[face(8, 0.1)]), "a person's answer is no fursuit's");
        assert!(told_apart(&f.not_same_index(Kind::Person), &[face(7, 0.1)], &[face(8, 0.1)]));
        assert!(!f.is_empty() && f.written_version() == FILE_VERSION);
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
        f.rename(&a, "A renamed".into()).unwrap();
        f.set_hidden(&a, true).unwrap();
        f.get_mut(&a).unwrap().cover = Some(face(3, 0.1));
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

    /// A version 1 file (as phase 3 and 4 wrote it) loads as it was, with
    /// no labels, and saves back as version 1 in the same shape: nothing
    /// a label added shows in a file that was never labelled.
    #[test]
    fn an_old_file_saves_back_unchanged_and_a_label_makes_it_v2() {
        let d = std::env::temp_dir().join(format!("pp-subjects-v1-labels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("subjects.json");
        let v1 = r#"{
  "version": 1,
  "subjects": [
    {
      "id": "0123456789abcdef0123456789abcdef",
      "name": "Someone",
      "kind": "person",
      "hidden": false,
      "cover": null,
      "members": [
        {
          "key": "01010101010101010101010101010101",
          "box": [
            0.1,
            0.2,
            0.1,
            0.15
          ]
        }
      ],
      "rejected": []
    }
  ]
}"#;
        std::fs::write(&path, v1).unwrap();
        let f = SubjectsFile::load(&path);
        assert_eq!((f.version, f.subjects.len(), f.added.len(), f.removed.len()), (1, 1, 0, 0));
        assert_eq!(f.subjects[0].members, [face(1, 0.1)]);
        f.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1, "byte for byte as it was");
        // With a label it is version 2, and back again without.
        let mut g = f.clone();
        g.remove_box(Kind::Person, face(1, 0.1)).unwrap();
        g.save(&path).unwrap();
        let back = SubjectsFile::load(&path);
        assert_eq!((back.version, back.removed.len()), (FILE_VERSION, 1));
        assert!(std::fs::read_to_string(&path).unwrap().contains(r#""kind": "person""#));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// All three optional lists at once (`not_same`, `added`, `removed`):
    /// one rule writes version 2 while any holds something; each round-
    /// trips; the file is empty only with none; *Forget all* of a kind
    /// takes that kind's entries from every list and leaves the other's;
    /// with every list empty again it is written as version 1.
    #[test]
    fn the_three_optional_lists_share_one_version_rule() {
        let d = std::env::temp_dir().join(format!("pp-subjects-three-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = d.join("subjects.json");
        let mut f = SubjectsFile::default();
        assert!(f.is_empty() && f.written_version() == FILE_VERSION_PLAIN);
        // Each list alone is enough for version 2.
        for which in 0..3 {
            let mut g = SubjectsFile::default();
            match which {
                0 => g.add_not_same(Kind::Fursuit, vec![face(1, 0.1)], vec![face(2, 0.1)]).unwrap(),
                1 => g.add_box(Kind::Person, face(3, 0.1)).unwrap(),
                _ => g.remove_box(Kind::Fursuit, face(4, 0.1)).unwrap(),
            }
            assert_eq!(g.written_version(), FILE_VERSION, "list {which}");
            assert!(!g.is_empty(), "list {which} keeps the file");
        }
        f.create(Kind::Person, Some("Someone".into()), vec![face(9, 0.1)]);
        f.add_not_same(Kind::Fursuit, vec![face(1, 0.1)], vec![face(2, 0.1)]).unwrap();
        f.add_box(Kind::Fursuit, face(3, 0.1)).unwrap();
        f.add_box(Kind::Person, face(5, 0.1)).unwrap();
        f.remove_box(Kind::Person, face(4, 0.1)).unwrap();
        f.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 2") && text.contains("\"not_same\"") && text.contains("\"added\"") && text.contains("\"removed\""), "{text}");
        let back = SubjectsFile::load(&path);
        assert_eq!((&back.subjects, &back.not_same, &back.added, &back.removed), (&f.subjects, &f.not_same, &f.added, &f.removed));
        // Forget fursuits: their answer and their drawn box go; people's stay.
        let mut g = back.clone();
        g.forget_kind(Kind::Fursuit);
        assert!(g.not_same.is_empty() && g.added.iter().all(|l| l.kind == Kind::Person) && g.removed.len() == 1 && g.subjects.len() == 1);
        g.forget_kind(Kind::Person);
        assert!(g.is_empty(), "every kind forgotten: nothing left");
        // Every list emptied: written as version 1 again.
        let mut h = back;
        h.not_same.clear();
        h.added.clear();
        h.removed.clear();
        h.save(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"version\": 1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The viewer's labels in the file: drawn and removed boxes, a name,
    /// "Unnamed", a kind change; each kind keeps to itself.
    #[test]
    fn labels_add_remove_name_and_change_kind_without_mixing() {
        let mut f = SubjectsFile::default();
        let a = f.create(Kind::Person, Some("A".into()), vec![face(1, 0.1)]);
        let suit = f.create(Kind::Fursuit, Some("Suit".into()), vec![face(2, 0.1)]);
        // Remove: out of A, remembered; again is nothing.
        f.remove_box(Kind::Person, face(1, 0.1)).unwrap();
        assert!(f.get(&a).unwrap().members.is_empty());
        assert_eq!(f.remove_box(Kind::Person, face(1, 0.1)), Err(Refused::Nothing));
        let removed = f.removed_index(Kind::Person);
        assert!(removed.covers(&face(1, 0.1)));
        // Found again a thousandth off after a rebuild: still removed.
        assert!(removed.covers(&FaceRef::new(Key([1; 16]), [0.101, 0.201, 0.1, 0.15])));
        assert!(!removed.covers(&face(1, 0.5)), "another face of the photo");
        assert!(!f.removed_index(Kind::Fursuit).covers(&face(1, 0.1)), "people only");
        // Drawn: listed; drawing the removed box again takes the removal back.
        f.add_box(Kind::Person, face(3, 0.4)).unwrap();
        assert!(f.is_added(Kind::Person, &face(3, 0.4)) && !f.is_added(Kind::Fursuit, &face(3, 0.4)));
        assert_eq!(f.add_box(Kind::Person, face(3, 0.4)), Err(Refused::Nothing));
        f.add_box(Kind::Person, face(1, 0.1)).unwrap();
        assert!(!f.removed_index(Kind::Person).covers(&face(1, 0.1)));
        // Removing a drawn box just takes it out.
        f.remove_box(Kind::Person, face(1, 0.1)).unwrap();
        assert!(!f.is_added(Kind::Person, &face(1, 0.1)) && !f.removed_index(Kind::Person).covers(&face(1, 0.1)));
        // Name: same kind only; Unnamed rejects it for its subject.
        assert_eq!(f.name_box(Kind::Person, face(3, 0.4), &suit), Err(Refused::MixedKinds));
        f.name_box(Kind::Person, face(3, 0.4), &a).unwrap();
        assert_eq!(f.member_of(Kind::Person, &face(3, 0.4)).unwrap().id, a);
        assert_eq!(f.unname_box(Kind::Person, face(3, 0.4)), Ok(a.clone()));
        assert!(f.get(&a).unwrap().rejected.contains(&face(3, 0.4)) && f.member_of(Kind::Person, &face(3, 0.4)).is_none());
        assert_eq!(f.unname_box(Kind::Person, face(3, 0.4)), Err(Refused::Nothing));
        // Kind change: the suit's head is a person's face.  It leaves the
        // fursuit entirely and is a drawn person; no subject holds it as
        // both.
        f.change_kind(face(2, 0.1), Kind::Fursuit, Kind::Person).unwrap();
        assert!(f.get(&suit).unwrap().members.is_empty());
        assert!(f.removed_index(Kind::Fursuit).covers(&face(2, 0.1)) && f.is_added(Kind::Person, &face(2, 0.1)));
        assert_eq!(f.name_box(Kind::Person, face(2, 0.1), &suit), Err(Refused::MixedKinds));
        f.name_box(Kind::Person, face(2, 0.1), &a).unwrap();
        assert!(f.subjects.iter().all(|s| s.kind == Kind::Person || !s.members.contains(&face(2, 0.1))), "never in a fursuit");
        // And back: out of A, a drawn fursuit again.
        f.change_kind(face(2, 0.1), Kind::Person, Kind::Fursuit).unwrap();
        assert!(!f.is_added(Kind::Person, &face(2, 0.1)) && f.member_of(Kind::Person, &face(2, 0.1)).is_none());
        assert!(f.is_added(Kind::Fursuit, &face(2, 0.1)) && !f.removed_index(Kind::Fursuit).covers(&face(2, 0.1)));
        assert_eq!(f.change_kind(face(2, 0.1), Kind::Fursuit, Kind::Fursuit), Err(Refused::Nothing));
        // Forgetting a kind takes its labels too, and leaves the other's.
        f.remove_box(Kind::Person, face(5, 0.1)).unwrap();
        f.forget_kind(Kind::Fursuit);
        assert!(f.added.iter().all(|l| l.kind == Kind::Person) && f.removed.iter().all(|l| l.kind == Kind::Person));
        assert!(f.removed_index(Kind::Person).covers(&face(5, 0.1)) && f.get(&a).is_some());
        f.forget_kind(Kind::Person);
        assert!(f.is_empty());
        // Labels round-trip as JSON.
        let mut g = SubjectsFile::default();
        g.add_box(Kind::Fursuit, face(7, 0.3)).unwrap();
        g.remove_box(Kind::Person, face(8, 0.2)).unwrap();
        let back: SubjectsFile = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!((back.added.clone(), back.removed.clone()), (g.added.clone(), g.removed.clone()));
        assert!((iou_boxes(&[0.0, 0.0, 1.0, 1.0], &[0.0, 0.0, 0.5, 1.0]) - 0.5).abs() < 1e-6);
    }

    /// Three people's faces as unit vectors around three directions, with
    /// a little noise; `n` faces per person, then `loners` far from all.
    fn people_rows(n: usize, loners: usize) -> (Vec<f32>, usize) {
        let dim = 16;
        let mut data = Vec::new();
        let mut seed = 7u64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed % 1000) as f32 / 1000.0 - 0.5) * 0.3
        };
        for person in 0..3 {
            for _ in 0..n {
                let mut v = vec![0f32; dim];
                v[person] = 1.0;
                for x in v.iter_mut() {
                    *x += noise();
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                data.extend(v.iter().map(|x| x / norm));
            }
        }
        for k in 0..loners {
            let mut v = vec![0f32; dim];
            v[5 + k] = 1.0;
            data.extend(v);
        }
        (data, dim)
    }

    /// People's grouping is unchanged by the per-kind linkage: on fixed
    /// toy rows, nearest centroid with the merge pass gives exactly these
    /// members, in this order, as it did before the linkage existed (the
    /// face between the first two bunches joins the first, whose centroid
    /// it meets first; the bunches' centroids stay too far apart to
    /// merge).  The pin is the members themselves, not a property.
    #[test]
    fn peoples_grouping_is_pinned() {
        let dim = 3;
        let mut data = Vec::new();
        let unit = |v: [f32; 3]| {
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            [v[0] / n, v[1] / n, v[2] / n]
        };
        for v in [[1.0, 0.0, 0.0], [0.98, 0.1, 0.0], [0.0, 1.0, 0.0], [0.1, 0.98, 0.0], [0.0, 0.0, 1.0], [0.0, 0.1, 0.98], [0.7, 0.7, 0.0]] {
            data.extend_from_slice(&unit(v));
        }
        let rows = Rows::F32 { dim, data: &data };
        let order: Vec<usize> = (0..7).collect();
        let people = cluster_with(&rows, &order, 0.5, Linkage::PEOPLE);
        let members: Vec<Vec<usize>> = people.iter().map(|c| c.members.clone()).collect();
        assert_eq!(members, vec![vec![0, 1, 6], vec![2, 3], vec![4, 5]]);
        let centroid: Vec<f32> = people[0].centroid.iter().map(|x| (x * 1000.0).round() / 1000.0).collect();
        assert_eq!(centroid, vec![0.958, 0.287, 0.0]);
        // The same rows under the fursuits' linkage: the in-between face
        // still goes with the first bunch, whose members it is nearest on
        // average; no merge pass runs.
        let suits = cluster_with(&rows, &order, 0.5, Linkage::Average);
        assert_eq!(suits.iter().map(|c| c.members.clone()).collect::<Vec<_>>(), members);
    }

    #[test]
    fn clustering_finds_the_people_and_leaves_loners_alone() {
        let (data, dim) = people_rows(8, 3);
        let rows = Rows::F32 { dim, data: &data };
        assert_eq!(rows.len(), 27);
        // The same rows as f16 bits answer the same, within rounding.
        let half: Vec<u16> = data.iter().map(|&x| crate::search::query::f32_to_f16(x)).collect();
        let rows16 = Rows::F16 { dim, data: &half };
        assert_eq!(rows16.len(), 27);
        assert!((rows16.dot(3, &rows.row_vec(3)) - 1.0).abs() < 1e-2);
        assert!(rows16.row_vec(5).iter().zip(&rows.row_vec(5)).all(|(a, b)| (a - b).abs() < 1e-3));
        assert_eq!(rows16.to_f32().len(), data.len());
        let order: Vec<usize> = (0..rows.len()).collect();
        let clusters = cluster_with(&rows, &order, 0.5, Linkage::PEOPLE);
        let sizes: Vec<usize> = clusters.iter().map(|c| c.members.len()).collect();
        assert_eq!(sizes, [8, 8, 8, 1, 1, 1], "{sizes:?}");
        for c in &clusters[..3] {
            let people: HashSet<usize> = c.members.iter().map(|&m| m / 8).collect();
            assert_eq!(people.len(), 1, "one person per group: {:?}", c.members);
            assert!((dot(&c.centroid, &c.centroid) - 1.0).abs() < 1e-4, "unit centroid");
        }
        // Rows left out of the order (confirmed to a subject) take no part.
        let partial: Vec<usize> = (8..27).collect();
        let without = cluster_with(&rows, &partial, 0.5, Linkage::PEOPLE);
        assert!(without.iter().all(|c| c.members.iter().all(|&m| m >= 8)));
        // Incremental assignment puts a new face of person 0 with its group.
        let mut live = cluster_with(&rows, &partial, 0.5, Linkage::PEOPLE);
        assign_with(&mut live, &rows, &[0, 1, 2], 0.5, Linkage::PEOPLE);
        let with0: Vec<&Cluster> = live.iter().filter(|c| c.members.contains(&0)).collect();
        assert_eq!(with0.len(), 1);
        assert!(with0[0].members.contains(&1) && with0[0].members.contains(&2) && with0[0].members.len() == 3, "{:?}", with0[0].members);
        assert!(assign_is_merged_by_full_cluster(&rows));
        // Sureness and candidates.
        assert_eq!(sureness(0.0, 0.5), 1.0);
        assert_eq!(sureness(0.5, 0.5), 0.0);
        assert!((sureness(0.25, 0.5) - 0.5).abs() < 1e-6);
        let members = [0usize, 1];
        let pool: Vec<usize> = (2..27).collect();
        let cands = candidates(&rows, &members, &pool, 0.5);
        assert_eq!(cands.len(), 6, "the rest of person 0: {cands:?}");
        assert!(cands.iter().all(|&(i, d)| i < 8 && d <= 0.5));
        assert!(cands.windows(2).all(|w| w[0].1 <= w[1].1), "nearest first");
        assert!(candidates(&rows, &members, &pool, 0.5 * STRICT_SHARE).len() <= 6);
        assert_eq!(distance_to_members(&rows, &[], &rows.row_vec(0)), None);
        // One suggestion per burst, in order of arrival.
        let grouped = per_burst(vec![("a", Some(1)), ("b", None), ("c", Some(2)), ("d", Some(1)), ("e", Some(2))]);
        assert_eq!(grouped, [vec!["a", "d"], vec!["b"], vec!["c", "e"]]);
        let id = cluster_id(&FaceRef::new(Key([1; 16]), [0.1, 0.2, 0.3, 0.4]));
        assert_eq!(id, cluster_id(&FaceRef::new(Key([1; 16]), [0.1001, 0.2, 0.3, 0.4])), "stable within rounding");
        assert_ne!(id, cluster_id(&FaceRef::new(Key([2; 16]), [0.1, 0.2, 0.3, 0.4])));
        let c = cutoff_for(crate::ml::models::ModelId::AuraFace);
        assert!(c.group > cutoff_for(crate::ml::models::ModelId::SFace).group && c.strict < c.suggest && c.suggest == c.group);
    }

    /// Two clusters of one person started apart (a split order) are merged
    /// by the full clustering's merge pass.
    fn assign_is_merged_by_full_cluster(rows: &Rows<'_>) -> bool {
        let (a, b): (Vec<usize>, Vec<usize>) = (0..8).partition(|i| i % 2 == 0);
        let mut c = cluster_with(rows, &a, 0.5, Linkage::PEOPLE);
        let before = c.len();
        c.extend(cluster_with(rows, &b, 0.5, Linkage::PEOPLE));
        let order: Vec<usize> = (0..8).collect();
        let merged = cluster_with(rows, &order, 0.5, Linkage::PEOPLE);
        before == 1 && merged.len() == 1 && merged[0].members.len() == 8
    }
}
