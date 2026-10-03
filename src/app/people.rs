//! People, fursuits and pets as the pages see them: the shapes the
//! subjects API answers with, the face models the download dialog offers,
//! and, behind `PP_SEARCH_FAKE`, a stand-in that makes people up.
//!
//! - **The shapes** are the ones asked of the backend (groups with a cover
//!   face and counts, faces with the same day, capture and path fields as a
//!   search hit, one review suggestion per burst).  Until the backend's own
//!   lands they live here, and the pages import them from here only, so the
//!   switch is one place.
//! - **Three kinds, never mixed.**  Every group carries its kind; a merge, a
//!   move or a suggestion across kinds is refused here as the backend
//!   refuses it, and the pages only ever show one kind at a time.
//! - **The stand-in** reads the library index the calendar holds (no photo,
//!   no file written) and gives each shot with a "face" a made-up person:
//!   most are grouped with their own, some are held back as suggestions and
//!   a few land in the wrong group, so naming, the review and splitting all
//!   have work to do.  Its faces are grey silhouettes drawn here, never a
//!   real face.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime};

use crate::ml::Device;
use crate::model::library_index::{self, LibraryIndex};
use crate::model::record::FileType;
use crate::search::index::Key;
use crate::util;

// ── the shapes ──────────────────────────────────────────────────────────────

/// What a group is of.  Each kind has its own tab, groups, names and
/// suggestions; nothing ever crosses from one to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Kind {
    #[default]
    Person,
    Fursuit,
    Pet,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Person, Kind::Fursuit, Kind::Pet];

    /// The tab's name.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Person => "People",
            Kind::Fursuit => "Fursuits",
            Kind::Pet => "Pets",
        }
    }

    /// One of them: "person", "fursuit", "pet".
    pub fn one(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Fursuit => "fursuit",
            Kind::Pet => "pet",
        }
    }

    /// What is shown of one in a crop: "faces", "heads".
    pub fn crops(self) -> &'static str {
        match self {
            Kind::Person => "Faces",
            Kind::Fursuit => "Heads",
            Kind::Pet => "Pets",
        }
    }

    /// Saved in the settings as a number.
    pub fn index(self) -> u8 {
        match self {
            Kind::Person => 0,
            Kind::Fursuit => 1,
            Kind::Pet => 2,
        }
    }

    pub fn from_index(i: u8) -> Kind {
        match i {
            1 => Kind::Fursuit,
            2 => Kind::Pet,
            _ => Kind::Person,
        }
    }
}

/// A named subject: stable for as long as it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubjectId(pub u64);

/// A group on a tab: a named subject, or an unnamed group of faces found
/// alike (valid for one `Subjects::generation`; a stale one is refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupId {
    Subject(SubjectId),
    Cluster(u64),
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupId::Subject(s) => write!(f, "s{}", s.0),
            GroupId::Cluster(c) => write!(f, "c{c}"),
        }
    }
}

/// One detection: the shot's content key and the box, as fractions of the
/// preview (x, y, width, height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceRef {
    pub key: Key,
    pub bbox: [f32; 4],
}

impl FaceRef {
    /// A key for maps and sets (the box rounded to 1/1000, as stored).
    pub fn id(&self) -> (Key, [u16; 4]) {
        (self.key, self.bbox.map(|v| (v.clamp(0.0, 1.0) * 1000.0).round() as u16))
    }
}

/// A face in a photo, with what the results grid needs to show the photo
/// (its files, day, capture time and burst, as a search hit has them).
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub face: FaceRef,
    pub jpg: Option<PathBuf>,
    pub raw: Option<PathBuf>,
    pub day: NaiveDate,
    pub capture: NaiveDateTime,
    pub burst: Option<u64>,
    /// How sure the match is and how large and sharp the face is, 0–1:
    /// a subject's page and its faces come clearest first.
    pub clarity: f32,
    /// Confirmed by the user (named with the group, or answered Yes).
    pub confirmed: bool,
}

/// One group as a tab lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: GroupId,
    pub kind: Kind,
    pub name: Option<String>,
    pub hidden: bool,
    /// Photos (shots) with this subject.
    pub photos: usize,
    /// Days they span.
    pub days: usize,
    /// The face its tile shows.
    pub cover: Face,
    /// Photos that may show it, waiting for Y or N (named subjects).
    pub suggestions: usize,
}

/// Every group of every kind (the pages show one kind at a time).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Subjects {
    pub generation: u64,
    pub groups: Vec<Group>,
    pub faces_found: usize,
}

impl Subjects {
    pub fn get(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The named subjects of `kind` (hidden ones too).
    pub fn named(&self, kind: Kind) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.kind == kind && g.name.is_some())
    }

    /// The subject of `kind` called `name` (any case), if there is one.
    pub fn by_name(&self, kind: Kind, name: &str) -> Option<&Group> {
        let n = name.trim().to_lowercase();
        self.named(kind).find(|g| g.name.as_deref().is_some_and(|x| x.to_lowercase() == n))
    }
}

/// How a crop is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framing {
    /// A square round the face, with a margin: tiles, chips, the review.
    Face,
    /// The whole photo fitted in the square, for "In the photo" (the page
    /// draws the box).
    Photo,
}

/// A faces pass running on its own (not the first index's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacesPassKind {
    /// Finding faces in a library indexed before faces were looked for.
    CatchUp,
    /// The face model changed: the faces found are recognised again.
    NewModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacesPass {
    pub kind: FacesPassKind,
    pub done: usize,
    pub total: usize,
}

/// Faces, as Preferences and the People tab show them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FacesStatus {
    /// Preferences ▸ *Find people's faces*.
    pub on: bool,
    /// The face model chosen, once one was.
    pub model: Option<FaceModel>,
    /// Its files (and the face finder's) are in place.
    pub model_installed: bool,
    /// Indexed shots searched for faces so far.
    pub looked_at: usize,
    /// Faces found, and people named.
    pub found: usize,
    pub named: usize,
    pub pass: Option<FacesPass>,
}

impl FacesStatus {
    /// The library was indexed before faces were looked for: the People tab
    /// offers to find them (and says what it costs) instead of running it.
    pub fn catch_up_wanted(&self, indexed: usize) -> bool {
        self.on && indexed > 0 && self.looked_at == 0 && self.pass.is_none()
    }
}

/// What the stand-in (or the engine) says on the bus.
#[derive(Debug, Clone)]
pub enum Event {
    Subjects(Arc<Subjects>),
    Faces { seq: u64, group: GroupId, faces: Arc<Vec<Face>> },
    /// One entry per burst, its frames best first; one answer covers it.
    Suggestions { seq: u64, group: GroupId, list: Arc<Vec<Vec<Face>>> },
    Status(FacesStatus),
    /// The last edit was taken back: what it was.
    Undone(String),
}

// ── the face models (the registry's rows) ───────────────────────────────────

/// A face recognition model the user can pick (decision 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceModel {
    AuraFace,
    InsightFace,
    SFace,
}

/// A row of the download dialog's face-model choice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceModelInfo {
    pub id: FaceModel,
    pub name: &'static str,
    /// One line on how it did.
    pub what: &'static str,
    pub licence: &'static str,
    /// The licence in plain words.
    pub licence_plain: &'static str,
    pub publisher: &'static str,
    pub source_url: &'static str,
    /// Downloaded, and kept once unpacked.
    pub download: u64,
    pub kept: u64,
    /// Offered first, and chosen until the user picks another.
    pub recommended: bool,
    /// Its licence restricts use (said in the warning tone, never chosen
    /// for the user).
    pub restricted: bool,
    /// Recognising one face, in milliseconds: the card, the processor.
    pub ms_gpu: f64,
    pub ms_cpu: f64,
}

/// The face finder every model shares (YuNet), so changing the model keeps
/// the faces and their names.
pub const FINDER: (&str, &str, &str, u64) = ("YuNet", "MIT", "OpenCV", 232_589);

/// The models offered, in the dialog's order (recommended first).
pub const FACE_MODELS: &[FaceModelInfo] = &[
    FaceModelInfo {
        id: FaceModel::AuraFace,
        name: "AuraFace",
        what: "The most accurate in tests here: no mixed-up groups.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "fal",
        source_url: "https://huggingface.co/fal/AuraFace-v1",
        download: 261_000_000,
        kept: 261_000_000,
        recommended: true,
        restricted: false,
        ms_gpu: 9.0,
        ms_cpu: 30.0,
    },
    FaceModelInfo {
        id: FaceModel::SFace,
        name: "SFace",
        what: "The smallest and fastest. Mixed people up more often in tests: 2 of 8 groups held two people.",
        licence: "Apache-2.0",
        licence_plain: "free for any use",
        publisher: "OpenCV",
        source_url: "https://github.com/opencv/opencv_zoo",
        download: 37_000_000,
        kept: 37_000_000,
        recommended: false,
        restricted: false,
        ms_gpu: 2.0,
        ms_cpu: 3.5,
    },
    FaceModelInfo {
        id: FaceModel::InsightFace,
        name: "InsightFace",
        what: "What Immich uses. The best published scores; not tested here.",
        licence: "Non-commercial research only",
        licence_plain: "it may be used only for non-commercial research, and that binds you if you pick it",
        publisher: "InsightFace",
        source_url: "https://github.com/deepinsight/insightface/releases",
        download: 288_000_000,
        kept: 174_000_000,
        recommended: false,
        restricted: true,
        ms_gpu: 5.0,
        ms_cpu: 15.0,
    },
];

pub fn face_model(id: FaceModel) -> &'static FaceModelInfo {
    FACE_MODELS.iter().find(|m| m.id == id).expect("every FaceModel is in FACE_MODELS")
}

/// The model chosen for the user until they pick another.
pub fn recommended() -> FaceModel {
    FACE_MODELS.iter().find(|m| m.recommended).map_or(FaceModel::AuraFace, |m| m.id)
}

/// About how many faces a library of `shots` holds (the study: 371 faces of
/// 40 px or more in 1 010 photos).
pub fn faces_in(shots: usize) -> usize {
    shots * 3 / 10
}

/// Seconds to find and recognise the faces in `shots` already indexed (the
/// catch-up): every preview is read again, then each face recognised.
pub fn catch_up_seconds(shots: usize, device: Device, model: FaceModel) -> f64 {
    let m = face_model(model);
    let per_face = if device == Device::Gpu { m.ms_gpu } else { m.ms_cpu };
    crate::search::estimate::seconds(shots, device) + faces_in(shots) as f64 * per_face / 1000.0
}

/// Seconds to recognise the faces found again with another model: only
/// the photos with faces (about a quarter) are read.
pub fn new_model_seconds(shots: usize, device: Device, model: FaceModel) -> f64 {
    let m = face_model(model);
    let per_face = if device == Device::Gpu { m.ms_gpu } else { m.ms_cpu };
    crate::search::estimate::seconds(shots / 4, device) + faces_in(shots) as f64 * per_face / 1000.0
}

/// What finding faces adds to the first index, for the dialog's *Takes*.
pub fn face_seconds(shots: usize, device: Device, model: FaceModel) -> f64 {
    let m = face_model(model);
    let per_face = if device == Device::Gpu { m.ms_gpu } else { m.ms_cpu };
    // The face finder on each preview already read for search, then each
    // face.
    shots as f64 * 0.0032 + faces_in(shots) as f64 * per_face / 1000.0
}

// ── the stand-in ────────────────────────────────────────────────────────────

/// A shot of the library, as the stand-in sees it.
#[derive(Debug, Clone)]
struct Shot {
    key: Key,
    jpg: Option<PathBuf>,
    raw: Option<PathBuf>,
    day: NaiveDate,
    capture: NaiveDateTime,
    burst: Option<u64>,
}

/// A number in 0..1 from a hash of `x`.
fn unit(x: &impl Hash) -> f64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    (h.finish() % 1_000_000) as f64 / 1_000_000.0
}

fn hash64(x: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut h);
    h.finish()
}

/// The library's shots, newest day first, with made-up keys and the
/// presses found as the stand-in's search finds them.
fn shots_of(index: &LibraryIndex) -> Vec<Shot> {
    let mut out = Vec::new();
    let mut press = 0u64;
    for (date, day) in index.days.iter().rev() {
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
        let mut last: Option<(NaiveDateTime, u32)> = None;
        let mut run = 0;
        let start = out.len();
        for (stem, (jpg, raw, when)) in shots {
            let num: Option<u32> = stem.rsplit('_').next().and_then(|n| n.parse().ok());
            if matches!((last, num), (Some((t, n0)), Some(n1)) if t == when && n1 == n0 + 1 && run < 8) {
                run += 1;
            } else {
                press += 1;
                run = 1;
            }
            last = num.map(|n| (when, n));
            let (a, b) = (hash64(&(&stem, date, 1u8)), hash64(&(&stem, date, 2u8)));
            let mut k = [0u8; 16];
            k[..8].copy_from_slice(&a.to_le_bytes());
            k[8..].copy_from_slice(&b.to_le_bytes());
            let capture = when + chrono::Duration::milliseconds(num.unwrap_or(0) as i64 % 1000 * 3);
            out.push(Shot { key: Key(k), jpg, raw, day: *date, capture, burst: Some(press) });
        }
        // A press of one frame is no burst.
        let mut sizes: HashMap<u64, usize> = HashMap::new();
        for s in &out[start..] {
            *sizes.entry(s.burst.unwrap_or(0)).or_default() += 1;
        }
        for s in &mut out[start..] {
            if s.burst.is_some_and(|b| sizes.get(&b) == Some(&1)) {
                s.burst = None;
            }
        }
    }
    out
}

/// How many made-up people the stand-in's library holds, and the share of
/// faces each has: a few people in many photos, then a long tail of
/// strangers in one or two.
const PEOPLE: usize = 34;

fn weight(person: usize) -> f64 {
    match person {
        0 => 30.0,
        1 => 14.0,
        2 => 6.0,
        3 => 2.6,
        4 => 1.3,
        5 => 1.0,
        6 => 0.8,
        _ => 0.12 + 0.2 * unit(&(person, "tail")),
    }
}

/// Who a shot shows, if anyone: one made-up person per shot, the frames of
/// a press showing the same one.  Deterministic, so a crop drawn on a
/// worker shows the same person the stand-in grouped.
fn true_person(shot_key: &Key, burst: Option<u64>) -> Option<usize> {
    let seed = burst.map_or_else(|| hash64(shot_key), |b| hash64(&(b, "press")));
    if unit(&(seed, "face")) > 0.45 {
        return None;
    }
    let total: f64 = (0..PEOPLE).map(weight).sum();
    let mut x = unit(&(seed, "who")) * total;
    for p in 0..PEOPLE {
        x -= weight(p);
        if x <= 0.0 {
            return Some(p);
        }
    }
    Some(PEOPLE - 1)
}

/// What became of a face when the stand-in grouped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// In its own person's group.
    Own,
    /// Held back: offered as a suggestion.
    Held,
    /// Grouped with someone else by mistake.
    Wrong(usize),
}

fn placed(shot: &Key, person: usize) -> Placed {
    let u = unit(&(shot, "placed"));
    if person <= 2 && u < 0.28 {
        Placed::Held
    } else if person <= 1 && u > 0.965 {
        Placed::Wrong((person + 1) % 3)
    } else {
        Placed::Own
    }
}

/// One group's state in the stand-in.
#[derive(Debug, Clone)]
struct FakeGroup {
    id: GroupId,
    name: Option<String>,
    hidden: bool,
    /// Its faces, by shot key.
    members: Vec<Key>,
    /// Confirmed members.
    confirmed: HashSet<Key>,
    /// "Not this person": never suggested again.
    rejected: HashSet<Key>,
    /// The person most of it shows (its look, and whose held-back faces
    /// are its suggestions).
    person: usize,
}

/// The stand-in's people: groups made up from the library, changed by the
/// page's calls.
#[derive(Debug, Clone, Default)]
pub struct FakePeople {
    shots: Vec<Shot>,
    by_key: HashMap<Key, usize>,
    groups: Vec<FakeGroup>,
    next_id: u64,
    generation: u64,
    /// Shots looked at for faces so far, newest first.
    pub looked_at: usize,
    /// Edits to take back, newest last: the state before, and what it was.
    undo: Vec<(Vec<FakeGroup>, String)>,
    /// Name the two largest people once faces are found.
    pub name_first: bool,
}

/// The names the stand-in gives (made up, never anyone's).
const NAMES: [&str; 2] = ["Person A", "Person B"];

impl FakePeople {
    /// No one yet; `name_first` names the two largest people once faces
    /// are found (screenshots of a library already worked on).
    pub fn new(name_first: bool) -> FakePeople {
        FakePeople { name_first, ..FakePeople::default() }
    }

    /// The library changed: take its shots (newest first).
    pub fn on_library(&mut self, index: &LibraryIndex) {
        self.set_shots(shots_of(index));
    }

    fn set_shots(&mut self, shots: Vec<Shot>) {
        self.shots = shots;
        self.by_key = self.shots.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
        self.looked_at = self.looked_at.min(self.shots.len());
    }

    #[cfg(test)]
    pub fn shots(&self) -> usize {
        self.shots.len()
    }

    /// Faces were looked for in the newest `n` shots: group them afresh
    /// (the user's names and answers are kept where their people still
    /// show).
    pub fn look_at(&mut self, n: usize) {
        self.looked_at = n.min(self.shots.len());
        self.regroup();
    }

    /// Forget everything found and named.
    pub fn forget_all(&mut self) {
        self.groups.clear();
        self.undo.clear();
        self.looked_at = 0;
        self.generation += 1;
    }

    fn regroup(&mut self) {
        let mut members: Vec<Vec<Key>> = vec![Vec::new(); PEOPLE];
        for s in &self.shots[..self.looked_at] {
            let Some(p) = true_person(&s.key, s.burst) else { continue };
            match placed(&s.key, p) {
                Placed::Own => members[p].push(s.key),
                Placed::Wrong(q) => members[q].push(s.key),
                Placed::Held => {}
            }
        }
        let old = std::mem::take(&mut self.groups);
        for (p, keys) in members.into_iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            // A person the user already has keeps their group (and name).
            if let Some(mut g) = old.iter().find(|g| g.person == p).cloned() {
                let have: HashSet<Key> = g.members.iter().copied().collect();
                for k in keys {
                    if !have.contains(&k) && !g.rejected.contains(&k) {
                        g.members.push(k);
                    }
                }
                self.groups.push(g);
                continue;
            }
            let named = self.name_first && p < NAMES.len();
            let id = if named {
                self.next_id += 1;
                GroupId::Subject(SubjectId(self.next_id))
            } else {
                GroupId::Cluster(p as u64 + 1000 * (self.generation + 1))
            };
            let confirmed = if named { keys.iter().copied().collect() } else { HashSet::new() };
            self.groups.push(FakeGroup { id, name: named.then(|| NAMES[p].to_string()), hidden: false, members: keys, confirmed, rejected: HashSet::new(), person: p });
        }
        self.generation += 1;
    }

    fn face_of(&self, key: Key, confirmed: bool) -> Option<Face> {
        let s = &self.shots[*self.by_key.get(&key)?];
        let u = unit(&(key, "box"));
        let size = 0.12 + 0.1 * unit(&(key, "size")) as f32;
        let bbox = [0.2 + 0.5 * u as f32, 0.12 + 0.15 * unit(&(key, "y")) as f32, size, size * 1.3];
        // The wrong ones and the held-back ones come out less clear.
        let clarity = (0.35 + 0.65 * unit(&(key, "clear"))) as f32;
        Some(Face { face: FaceRef { key, bbox }, jpg: s.jpg.clone(), raw: s.raw.clone(), day: s.day, capture: s.capture, burst: s.burst, clarity, confirmed })
    }

    fn group(&self, id: GroupId) -> Option<&FakeGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut FakeGroup> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// Every group, as the tabs list them.
    pub fn snapshot(&self) -> Subjects {
        let mut groups = Vec::new();
        for g in &self.groups {
            let faces: Vec<Face> = g.members.iter().filter_map(|&k| self.face_of(k, g.confirmed.contains(&k))).collect();
            let Some(cover) = faces.iter().max_by(|a, b| a.clarity.total_cmp(&b.clarity)).cloned() else { continue };
            let days: HashSet<NaiveDate> = faces.iter().map(|f| f.day).collect();
            let suggestions = if g.name.is_some() { self.suggestions_of(g).len() } else { 0 };
            groups.push(Group { id: g.id, kind: Kind::Person, name: g.name.clone(), hidden: g.hidden, photos: faces.len(), days: days.len(), cover, suggestions });
        }
        groups.sort_by(|a, b| b.photos.cmp(&a.photos).then(a.cover.capture.cmp(&b.cover.capture)));
        let faces_found = self.groups.iter().map(|g| g.members.len()).sum::<usize>()
            + self.shots[..self.looked_at].iter().filter(|s| true_person(&s.key, s.burst).is_some_and(|p| placed(&s.key, p) == Placed::Held)).count();
        Subjects { generation: self.generation, groups, faces_found }
    }

    pub fn named(&self) -> usize {
        self.groups.iter().filter(|g| g.name.is_some()).count()
    }

    /// A group's faces, clearest first.
    pub fn faces(&self, id: GroupId) -> Vec<Face> {
        let Some(g) = self.group(id) else { return Vec::new() };
        let mut v: Vec<Face> = g.members.iter().filter_map(|&k| self.face_of(k, g.confirmed.contains(&k))).collect();
        v.sort_by(|a, b| b.clarity.total_cmp(&a.clarity).then(a.capture.cmp(&b.capture)));
        v
    }

    /// The shot keys of a group's photos.
    pub fn members(&self, id: GroupId) -> HashSet<Key> {
        self.group(id).map(|g| g.members.iter().copied().collect()).unwrap_or_default()
    }

    /// The faces that may show `g`'s person, one entry per press: the ones
    /// held back, then a few look-alikes; never a member or one answered
    /// No.
    fn suggestions_of(&self, g: &FakeGroup) -> Vec<Vec<Face>> {
        let have: HashSet<Key> = g.members.iter().copied().collect();
        let mut by_press: BTreeMap<(u64, u64), Vec<Face>> = BTreeMap::new();
        for s in &self.shots[..self.looked_at] {
            if have.contains(&s.key) || g.rejected.contains(&s.key) {
                continue;
            }
            let Some(p) = true_person(&s.key, s.burst) else { continue };
            let held = p == g.person && placed(&s.key, p) == Placed::Held;
            let lookalike = p != g.person && p > 2 && unit(&(s.key, g.person, "alike")) < 0.08;
            if !(held || lookalike) {
                continue;
            }
            let press = s.burst.unwrap_or_else(|| hash64(&s.key));
            let rank = (unit(&(press, g.person, "rank")) * 1e6) as u64 + if lookalike { 1_000_000 } else { 0 };
            if let Some(f) = self.face_of(s.key, false) {
                by_press.entry((rank, press)).or_default().push(f);
            }
        }
        by_press
            .into_values()
            .map(|mut v| {
                v.sort_by(|a, b| b.clarity.total_cmp(&a.clarity));
                v
            })
            .take(24)
            .collect()
    }

    /// What the review offers for `id`: a named subject's suggestions, or
    /// an unnamed group's own faces to check, one entry per press.
    pub fn suggestions(&self, id: GroupId) -> Vec<Vec<Face>> {
        let Some(g) = self.group(id) else { return Vec::new() };
        if g.name.is_some() {
            return self.suggestions_of(g);
        }
        let mut by_press: BTreeMap<u64, Vec<Face>> = BTreeMap::new();
        for f in self.faces(id) {
            by_press.entry(f.burst.unwrap_or_else(|| hash64(&f.face.key))).or_default().push(f);
        }
        let mut v: Vec<Vec<Face>> = by_press.into_values().collect();
        v.sort_by(|a, b| a[0].clarity.total_cmp(&b[0].clarity));
        v
    }

    fn remember(&mut self, what: impl Into<String>) {
        self.undo.push((self.groups.clone(), what.into()));
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
        self.generation += 1;
    }

    /// Take back the last edit; what it was.
    pub fn undo(&mut self) -> Option<String> {
        let (groups, what) = self.undo.pop()?;
        self.groups = groups;
        self.generation += 1;
        Some(what)
    }

    fn label(&self, id: GroupId) -> String {
        self.group(id).and_then(|g| g.name.clone()).unwrap_or_else(|| "an unnamed group".into())
    }

    pub fn name_group(&mut self, id: GroupId, name: &str) -> Result<GroupId, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("A name can't be empty.".into());
        }
        if self.groups.iter().any(|g| g.id != id && g.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&name))) {
            return Err(format!("There's already someone called {name}."));
        }
        if self.group(id).is_none() {
            return Err("That group changed: look again.".into());
        }
        let was = self.group(id).and_then(|g| g.name.clone());
        self.remember(match was {
            Some(w) => format!("Renamed {w} to {name}"),
            None => format!("Named a group {name}"),
        });
        self.next_id += 1;
        let new_id = match id {
            GroupId::Subject(s) => GroupId::Subject(s),
            GroupId::Cluster(_) => GroupId::Subject(SubjectId(self.next_id)),
        };
        let g = self.group_mut(id).expect("checked");
        g.id = new_id;
        g.name = Some(name);
        // Naming a group confirms what it holds.
        let all: Vec<Key> = g.members.clone();
        g.confirmed.extend(all);
        Ok(new_id)
    }

    /// Merge `others` into `into`; every group must be one kind (all people
    /// here: the stand-in has no fursuits or pets to mix in).
    pub fn merge(&mut self, into: GroupId, others: &[GroupId], kinds: impl Fn(GroupId) -> Option<Kind>) -> Result<(), String> {
        let k = kinds(into).ok_or("That group changed: look again.")?;
        if others.iter().any(|&o| kinds(o) != Some(k)) {
            return Err("Only groups of one kind can be merged.".into());
        }
        let n = others.iter().filter(|&&o| o != into).count();
        let label = self.label(into);
        self.remember(format!("Merged {} into {label}", util::plural(n as u64 + 1, "group")));
        let mut moved: Vec<FakeGroup> = Vec::new();
        self.groups.retain(|g| {
            if g.id != into && others.contains(&g.id) {
                moved.push(g.clone());
                false
            } else {
                true
            }
        });
        let g = self.group_mut(into).expect("checked");
        for m in moved {
            for k in m.members {
                if !g.members.contains(&k) {
                    g.members.push(k);
                }
            }
            g.confirmed.extend(m.confirmed);
            g.rejected.extend(m.rejected);
            if g.name.is_none() {
                g.name = m.name;
            }
        }
        Ok(())
    }

    pub fn set_hidden(&mut self, id: GroupId, hidden: bool) {
        if self.group(id).is_none() {
            return;
        }
        let label = self.label(id);
        self.remember(if hidden { format!("Hid {label}") } else { format!("Showed {label} again") });
        if let Some(g) = self.group_mut(id) {
            g.hidden = hidden;
        }
    }

    /// Forget a named subject: its name, and what was confirmed.  Its faces
    /// go back to an unnamed group.
    pub fn forget(&mut self, id: GroupId) {
        let label = self.label(id);
        self.remember(format!("Forgot {label}"));
        let generation = self.generation;
        if let Some(g) = self.group_mut(id) {
            g.name = None;
            g.confirmed.clear();
            g.rejected.clear();
            g.hidden = false;
            g.id = GroupId::Cluster(g.person as u64 + 1000 * (generation + 1));
        }
    }

    /// Move faces out of `from`: into `to`, or a new unnamed group.
    pub fn move_faces(&mut self, from: GroupId, faces: &[FaceRef], to: Option<GroupId>) -> Result<(), String> {
        if self.group(from).is_none() || to.is_some_and(|t| self.group(t).is_none()) {
            return Err("That group changed: look again.".into());
        }
        let keys: HashSet<Key> = faces.iter().map(|f| f.key).collect();
        let what = match to {
            Some(t) => format!("Moved {} to {}", util::plural(keys.len() as u64, "face"), self.label(t)),
            None => format!("Moved {} to a new group", util::plural(keys.len() as u64, "face")),
        };
        self.remember(what);
        let person = self.group(from).map_or(0, |g| g.person);
        if let Some(g) = self.group_mut(from) {
            g.members.retain(|k| !keys.contains(k));
            g.confirmed.retain(|k| !keys.contains(k));
        }
        match to {
            Some(t) => {
                let g = self.group_mut(t).expect("checked");
                for &k in &keys {
                    if !g.members.contains(&k) {
                        g.members.push(k);
                    }
                    g.rejected.remove(&k);
                    g.confirmed.insert(k);
                }
            }
            None => {
                self.next_id += 1;
                let id = GroupId::Cluster(1_000_000 + self.next_id);
                // Its look: whoever the first face shows.
                let p = keys.iter().next().and_then(|k| self.by_key.get(k)).and_then(|&i| true_person(&self.shots[i].key, self.shots[i].burst)).unwrap_or(person);
                self.groups.push(FakeGroup { id, name: None, hidden: false, members: keys.iter().copied().collect(), confirmed: HashSet::new(), rejected: HashSet::new(), person: p });
            }
        }
        Ok(())
    }

    /// Yes (add the faces), No (not this person: out, and never suggested
    /// again) or None (take the answer back).
    pub fn answer(&mut self, id: GroupId, faces: &[FaceRef], yes: Option<bool>) {
        let Some(label) = self.group(id).map(|_| self.label(id)) else { return };
        let keys: Vec<Key> = faces.iter().map(|f| f.key).collect();
        let n = keys.len() as u64;
        self.remember(match yes {
            Some(true) => format!("Added {} to {label}", util::plural(n, "photo")),
            Some(false) => format!("Marked {} as not {label}", util::plural(n, "photo")),
            None => format!("Took back an answer for {label}"),
        });
        let Some(g) = self.group_mut(id) else { return };
        for k in keys {
            match yes {
                Some(true) => {
                    if !g.members.contains(&k) {
                        g.members.push(k);
                    }
                    g.confirmed.insert(k);
                    g.rejected.remove(&k);
                }
                Some(false) => {
                    g.members.retain(|x| *x != k);
                    g.confirmed.remove(&k);
                    g.rejected.insert(k);
                }
                None => {
                    if g.rejected.remove(&k) {
                        // It was a member before it was answered No when
                        // it shows this person (checking a group).
                    } else {
                        g.members.retain(|x| *x != k);
                        g.confirmed.remove(&k);
                    }
                }
            }
        }
    }

    /// The person a face shows, for its silhouette.
    pub fn look_of(&self, face: &FaceRef) -> usize {
        self.by_key.get(&face.key).and_then(|&i| true_person(&self.shots[i].key, self.shots[i].burst)).unwrap_or((hash64(&face.key) % PEOPLE as u64) as usize)
    }
}

// ── placeholder crops: grey silhouettes, never a face ───────────────────────

/// A made-up person's look, in greys: head, hair, shoulders.
struct Look {
    skin: u8,
    hair: u8,
    shirt: u8,
    style: u8,
}

fn look(person: usize) -> Look {
    let u = |s: &str| unit(&(person, s));
    Look { skin: (120.0 + 80.0 * u("skin")) as u8, hair: (30.0 + 70.0 * u("hair")) as u8, shirt: (60.0 + 90.0 * u("shirt")) as u8, style: (u("style") * 4.0) as u8 }
}

/// A placeholder crop: a grey silhouette of made-up person `person` (head,
/// hair, shoulders, no features), `px` square, or a whole made-up photo
/// with them at `bbox`.  RGBA, straight alpha.
pub fn silhouette(person: usize, face: &FaceRef, px: u32, framing: Framing) -> (u32, u32, Vec<u8>) {
    use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode, Transform};
    let (w, h) = match framing {
        Framing::Face => (px.max(8), px.max(8)),
        Framing::Photo => (px.max(12), (px * 2 / 3).max(8)),
    };
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    let grey = |v: u8| Color::from_rgba8(v, v, v, 255);
    let seed = hash64(&face.id());
    let r = |s: &str| unit(&(seed, s)) as f32;
    // The background: a soft vertical gradient.
    let top = 70.0 + 50.0 * r("top");
    let bottom = top - 30.0 - 15.0 * r("bottom");
    let mut paint = Paint::default();
    paint.anti_alias = true;
    if let Some(sh) = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(0.0, h as f32),
        vec![GradientStop::new(0.0, grey(top as u8)), GradientStop::new(1.0, grey(bottom.max(20.0) as u8))],
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        paint.shader = sh;
    }
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
        pm.fill_rect(rect, &paint, Transform::identity(), None);
    }
    let person_at = |pm: &mut Pixmap, cx: f32, base: f32, s: f32, who: usize| {
        let lk = look(who);
        let fill = |pm: &mut Pixmap, pb: PathBuilder, v: u8| {
            if let Some(p) = pb.finish() {
                let mut paint = Paint::default();
                paint.anti_alias = true;
                paint.set_color(grey(v));
                pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
        };
        let oval = |x: f32, y: f32, rx: f32, ry: f32| {
            let mut pb = PathBuilder::new();
            if let Some(r) = Rect::from_xywh(x - rx, y - ry, rx * 2.0, ry * 2.0) {
                pb.push_oval(r);
            }
            pb
        };
        // Shoulders, neck, then the head and its hair.
        fill(pm, oval(cx, base + s * 0.12, s * 1.05, s * 0.72), lk.shirt);
        let head_y = base - s * 1.18;
        let hr = s * 0.37;
        let mut neck = PathBuilder::new();
        if let Some(r) = Rect::from_xywh(cx - s * 0.15, head_y + hr * 0.6, s * 0.3, s * 0.55) {
            neck.push_rect(r);
        }
        fill(pm, neck, lk.skin.saturating_sub(22));
        if lk.style == 1 {
            fill(pm, oval(cx, head_y + hr * 0.5, hr * 1.12, hr * 1.5), lk.hair);
        }
        fill(pm, oval(cx, head_y, hr * 0.9, hr * 1.12), lk.skin);
        match lk.style {
            2 => fill(pm, oval(cx, head_y - hr * 1.2, hr * 0.42, hr * 0.42), lk.hair),
            3 => {
                let mut cap = PathBuilder::new();
                cap.move_to(cx - hr, head_y - hr * 0.25);
                cap.quad_to(cx, head_y - hr * 1.75, cx + hr, head_y - hr * 0.25);
                cap.close();
                fill(pm, cap, lk.hair);
            }
            _ => {}
        }
        let mut top = PathBuilder::new();
        top.move_to(cx - hr * 0.95, head_y - hr * 0.1);
        top.quad_to(cx, head_y - hr * 1.55, cx + hr * 0.95, head_y - hr * 0.1);
        top.quad_to(cx + hr * 0.3, head_y - hr * 0.7, cx - hr * 0.95, head_y - hr * 0.1);
        top.close();
        fill(pm, top, lk.hair);
    };
    match framing {
        Framing::Face => {
            // Framed on the head: a little off-centre and tilted each time.
            let s = w as f32 * (0.62 + 0.1 * r("scale"));
            let cx = w as f32 * (0.5 + 0.06 * (r("dx") - 0.5));
            person_at(&mut pm, cx, h as f32 * (1.12 + 0.06 * r("dy")), s, person);
        }
        Framing::Photo => {
            // A floor, someone else now and then, and the person at the box.
            let floor = h as f32 * 0.78;
            let mut pb = PathBuilder::new();
            if let Some(rr) = Rect::from_xywh(0.0, floor, w as f32, h as f32 - floor) {
                pb.push_rect(rr);
            }
            if let Some(p) = pb.finish() {
                let mut paint = Paint::default();
                paint.set_color(grey((40.0 + 25.0 * r("floor")) as u8));
                pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
            if r("other") < 0.5 {
                person_at(&mut pm, w as f32 * 0.12, h as f32 * 1.02, h as f32 * 0.24, (person + 7) % PEOPLE);
            }
            let [bx, by, bw, bh] = face.bbox;
            let s = bw * w as f32 * 1.35;
            let cx = (bx + bw / 2.0) * w as f32;
            let head_y = (by + bh / 2.0) * h as f32;
            person_at(&mut pm, cx, head_y + s * 1.18, s, person);
        }
    }
    let mut px = pm.take();
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    (w, h, px)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up shots: `days` days of `n` frames each, in presses of three,
    /// newest day first (as `shots_of` gives them).
    fn shots(days: u32, n: u32) -> Vec<Shot> {
        let mut out = Vec::new();
        for d in (0..days).rev() {
            let day = NaiveDate::from_ymd_opt(2024, 6, d + 1).unwrap();
            for i in 0..n {
                let mut k = [0u8; 16];
                k[..4].copy_from_slice(&d.to_le_bytes());
                k[4..8].copy_from_slice(&i.to_le_bytes());
                let capture = day.and_hms_opt(10, i / 3 % 60, (i % 3) * 2).unwrap();
                let jpg = Some(PathBuf::from(format!("/lib/{day}/{i:04}.JPG")));
                out.push(Shot { key: Key(k), jpg, raw: None, day, capture, burst: Some((d * 1000 + i / 3) as u64) });
            }
        }
        out
    }

    fn people(named: bool) -> FakePeople {
        let mut p = FakePeople::new(named);
        p.set_shots(shots(6, 120));
        p.look_at(p.shots());
        p
    }

    #[test]
    fn faces_group_into_a_few_people_and_many_strangers() {
        let p = people(false);
        let s = p.snapshot();
        assert!(s.groups.len() >= 10, "{}", s.groups.len());
        assert!(s.groups.iter().all(|g| g.kind == Kind::Person && g.name.is_none()));
        // Largest first, and a long tail of small ones.
        assert!(s.groups.windows(2).all(|w| w[0].photos >= w[1].photos));
        assert!(s.groups[0].photos >= 40, "{}", s.groups[0].photos);
        assert!(s.groups.iter().filter(|g| g.photos < 5).count() >= 5);
        // Every group's cover is one of its own faces.
        for g in &s.groups {
            assert!(p.faces(g.id).iter().any(|f| f.face == g.cover.face));
        }
    }

    #[test]
    fn naming_a_group_confirms_it_and_a_name_is_taken_once() {
        let mut p = people(false);
        let s = p.snapshot();
        let (a, b) = (s.groups[0].id, s.groups[1].id);
        let named = p.name_group(a, "Person A").unwrap();
        assert!(matches!(named, GroupId::Subject(_)));
        assert!(p.faces(named).iter().all(|f| f.confirmed));
        assert!(p.name_group(b, "person a").is_err(), "names are unique, any case");
        assert!(p.name_group(a, "x").is_err(), "the old cluster id is stale");
        assert_eq!(p.undo().as_deref(), Some("Named a group Person A"));
        assert!(p.snapshot().groups.iter().all(|g| g.name.is_none()));
    }

    #[test]
    fn merging_never_crosses_kinds() {
        let mut p = people(false);
        let s = p.snapshot();
        let (a, b) = (s.groups[0].id, s.groups[1].id);
        let fursuit = GroupId::Cluster(99_999);
        let kinds = |id: GroupId| if id == fursuit { Some(Kind::Fursuit) } else { s.get(id).map(|g| g.kind) };
        assert!(p.merge(a, &[fursuit], kinds).is_err(), "a person and a fursuit are never merged");
        let before = s.groups[0].photos + s.groups[1].photos;
        p.merge(a, &[b], kinds).unwrap();
        let after = p.snapshot();
        assert!(after.get(b).is_none());
        assert_eq!(after.get(a).unwrap().photos, before);
    }

    #[test]
    fn answers_add_take_out_and_come_back() {
        let mut p = people(true);
        let s = p.snapshot();
        let a = s.groups.iter().find(|g| g.name.as_deref() == Some("Person A")).unwrap().id;
        let list = p.suggestions(a);
        assert!(!list.is_empty() && list.len() <= 24);
        // One entry per press: its frames together.
        for entry in &list {
            assert!(entry.iter().all(|f| f.burst == entry[0].burst));
        }
        let first: Vec<FaceRef> = list[0].iter().map(|f| f.face).collect();
        let n = p.snapshot().get(a).unwrap().photos;
        p.answer(a, &first, Some(true));
        assert_eq!(p.snapshot().get(a).unwrap().photos, n + first.len(), "Yes covers the whole press");
        assert!(!p.suggestions(a).iter().any(|e| e[0].face == first[0]), "answered, not offered again");
        p.answer(a, &first, None);
        assert_eq!(p.snapshot().get(a).unwrap().photos, n);
        // No: never offered again, and out if it was in.
        p.answer(a, &first, Some(false));
        assert!(!p.suggestions(a).iter().any(|e| e[0].face == first[0]));
        let member = p.faces(a)[0].face;
        p.answer(a, &[member], Some(false));
        assert!(!p.faces(a).iter().any(|f| f.face == member), "not this person moves it out");
    }

    #[test]
    fn moving_faces_out_makes_a_new_group_or_joins_another() {
        let mut p = people(false);
        let s = p.snapshot();
        let (a, b) = (s.groups[0].id, s.groups[1].id);
        let two: Vec<FaceRef> = p.faces(a).iter().take(2).map(|f| f.face).collect();
        p.move_faces(a, &two, Some(b)).unwrap();
        assert!(two.iter().all(|f| p.faces(b).iter().any(|x| x.face.key == f.key)));
        assert!(!p.faces(a).iter().any(|x| two.iter().any(|f| f.key == x.face.key)));
        let groups = p.snapshot().groups.len();
        let one = vec![p.faces(b)[0].face];
        p.move_faces(b, &one, None).unwrap();
        assert_eq!(p.snapshot().groups.len(), groups + 1);
    }

    #[test]
    fn a_silhouette_is_drawn_in_greys_at_the_size_asked() {
        let f = FaceRef { key: Key([7; 16]), bbox: [0.4, 0.2, 0.15, 0.2] };
        let (w, h, px) = silhouette(3, &f, 64, Framing::Face);
        assert_eq!((w, h, px.len()), (64, 64, 64 * 64 * 4));
        assert!(px.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255), "greys only");
        let (w, h, _) = silhouette(3, &f, 300, Framing::Photo);
        assert_eq!((w, h), (300, 200));
    }

    #[test]
    fn estimates_follow_the_device_and_the_model() {
        let n = 56_710;
        let gpu = catch_up_seconds(n, Device::Gpu, FaceModel::AuraFace);
        assert!((8.0 * 60.0..14.0 * 60.0).contains(&gpu), "{gpu}");
        assert!(catch_up_seconds(n, Device::Cpu, FaceModel::AuraFace) > gpu);
        assert!(face_seconds(n, Device::Gpu, FaceModel::SFace) < face_seconds(n, Device::Gpu, FaceModel::AuraFace));
        assert_eq!(recommended(), FaceModel::AuraFace);
        assert!(FACE_MODELS.iter().filter(|m| m.restricted).all(|m| !m.recommended), "a restricted licence is never chosen for the user");
    }
}
