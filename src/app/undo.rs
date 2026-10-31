//! Undo and redo (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y) for the working folder.
//!
//! What can be undone:
//!
//! * **Prune and export marks, and crops**, however they were made: P / M
//!   in the grid (one photo, a selection, Ctrl+A), the viewer's
//!   buttons and keys, Review's "Unmark all" and Export's "Clear all marks".
//! * **Sort.**  Undo moves the files back out of RAW/ and JPG/ on a worker
//!   (then removes those folders if they are left empty); redo sorts them
//!   again.
//! * **Prune Review's move to the Trash.**  Undo takes exactly the files that
//!   operation trashed back out of the Trash, on a worker, and the photos
//!   return with the marks and crops they had.
//!
//! File moves go through `ops::fsutil` and never replace anything: a file
//! whose old place is taken now stays where it is and is reported.
//!
//! Mark changes are found by comparing what a change touched against a
//! baseline of every record's marks as of the last step, so any code that
//! ends in `refresh_prune` / `refresh_mark` / `refresh_crop` is recorded
//! without knowing about undo.  Steps name records by `RecordId`, which Sort
//! keeps; records the Trash gives back get new ids and the history is
//! rewritten to match.
//!
//! The history belongs to one working-folder session (opening a folder
//! clears it) and keeps the last [`CAP`] steps.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::Task;

use super::commands::Cmd;
use super::{App, Message, dialogs, viewer};
use crate::bus::BgEvent;
use crate::model::collection::{Collection, RecordId};
use crate::model::crop::Crop;
use crate::model::record::{MTime, PhotoRecord};
use crate::ops::trash::{self, TrashResult};
use crate::ops::undo::{self as fs_undo, Failure, MoveOp, MoveReport, RestoreReport, TrashTarget};
use crate::util::{self, plural};

/// Steps kept.
pub const CAP: usize = 100;

// ── steps ───────────────────────────────────────────────────────────────────

/// The undoable state of one record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Marks {
    pub pruned: bool,
    pub marked: bool,
    pub crop: Option<Crop>,
}

impl Marks {
    pub fn of(r: &PhotoRecord) -> Marks {
        Marks { pruned: r.is_pruned, marked: r.is_marked, crop: r.crop.clone() }
    }

    fn cropped(&self) -> bool {
        self.crop.as_ref().is_some_and(|c| !c.is_full())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Prune,
    Mark,
    Crop,
}

impl Field {
    fn differs(self, a: &Marks, b: &Marks) -> bool {
        match self {
            Field::Prune => a.pruned != b.pruned,
            Field::Mark => a.marked != b.marked,
            Field::Crop => a.crop != b.crop,
        }
    }

    /// Set this field of `r` from `m` (the others are left alone).
    fn copy(self, m: &Marks, r: &mut PhotoRecord) {
        match self {
            Field::Prune => r.is_pruned = m.pruned,
            Field::Mark => r.is_marked = m.marked,
            Field::Crop => r.crop = m.crop.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub id: RecordId,
    pub before: Marks,
    pub after: Marks,
}

/// A file Sort moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub id: RecordId,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A file moved to the Trash, and its record as it was.
#[derive(Debug, Clone)]
pub struct Trashed {
    pub id: RecordId,
    pub record: PhotoRecord,
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// Mark changes made by one action: one part for a press, several for
    /// a confirmed bulk action (prune some frames and keep one), undone
    /// and redone together.
    Batch { parts: Vec<(Field, Vec<Change>)> },
    Sort { moves: Vec<Moved> },
    /// `window`: when the trash operation ran (unix seconds).
    Trash { files: Vec<Trashed>, window: (i64, i64) },
}

impl Kind {
    fn remap(&mut self, map: &HashMap<RecordId, RecordId>) {
        let fix = |id: &mut RecordId| {
            if let Some(&n) = map.get(id) {
                *id = n;
            }
        };
        match self {
            Kind::Batch { parts } => parts.iter_mut().flat_map(|(_, c)| c.iter_mut()).for_each(|c| fix(&mut c.id)),
            Kind::Sort { moves } => moves.iter_mut().for_each(|m| fix(&mut m.id)),
            Kind::Trash { files, .. } => files.iter_mut().for_each(|f| fix(&mut f.id)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    /// Identifies the step (the status bar's Undo button names it).
    pub serial: u64,
    /// What it did, as a phrase: "prune 3 photos".
    pub label: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Undo,
    Redo,
}

impl Dir {
    fn verb(self) -> &'static str {
        match self {
            Dir::Undo => "undo",
            Dir::Redo => "redo",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Dir::Undo => "Undid",
            Dir::Redo => "Redid",
        }
    }

    fn ing(self) -> &'static str {
        match self {
            Dir::Undo => "Undoing",
            Dir::Redo => "Redoing",
        }
    }
}

// ── the history ─────────────────────────────────────────────────────────────

/// A step running on a worker.
struct Pending {
    dir: Dir,
    step: Step,
    /// `History::epoch` when it started.
    epoch: u64,
    /// Files left out before the worker started.
    skipped: Vec<Failure>,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Each record's marks as of the last step (records with none are left
    /// out).
    baseline: HashMap<RecordId, Marks>,
    serial: u64,
    /// Bumped when the history is cleared, so a worker's late result for
    /// another folder is dropped.
    session: u64,
    /// Bumped by every new step.
    epoch: u64,
    pending: Option<Pending>,
    /// While one action's changes are gathered into a single step (see
    /// [`batch`]).
    gathering: Option<Vec<(Field, Vec<Change>)>>,
}

fn trim(v: &mut Vec<Step>) {
    if v.len() > CAP {
        let n = v.len() - CAP;
        v.drain(..n);
    }
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new session: nothing to undo or redo.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.baseline.clear();
        self.pending = None;
        self.session += 1;
    }

    /// Take every record's marks as the baseline (after a scan loads them).
    pub fn reset_baseline(&mut self, coll: &Collection) {
        self.baseline = coll.iter_ids().map(|(id, r)| (id, Marks::of(r))).filter(|(_, m)| *m != Marks::default()).collect();
    }

    fn set_baseline(&mut self, id: RecordId, m: Marks) {
        if m == Marks::default() {
            self.baseline.remove(&id);
        } else {
            self.baseline.insert(id, m);
        }
    }

    /// How `field` changed on `ids` since the baseline, which moves on.
    pub fn diff(&mut self, coll: &Collection, field: Field, ids: &[RecordId]) -> Vec<Change> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for &id in ids {
            if !seen.insert(id) {
                continue;
            }
            let Some(r) = coll.get(id) else { continue };
            let before = self.baseline.get(&id).cloned().unwrap_or_default();
            let after = Marks::of(r);
            if field.differs(&before, &after) {
                out.push(Change { id, before, after: after.clone() });
            }
            self.set_baseline(id, after);
        }
        out
    }

    /// Start gathering mark changes into one step.
    fn gather(&mut self) {
        self.gathering = Some(Vec::new());
    }

    /// Stop gathering: the changes gathered become one step named `label`
    /// (none if nothing changed).  Returns its serial.
    fn gathered(&mut self, label: String) -> Option<u64> {
        let parts = self.gathering.take()?;
        if parts.is_empty() {
            return None;
        }
        Some(self.push(label, Kind::Batch { parts }))
    }

    /// Put the marks of a marks step back (`Undo`: as they were before
    /// it) or on again (`Redo`), moving the baseline with them.  Returns
    /// how many records it reached and the fields whose sidecars change.
    fn restore(&mut self, coll: &mut Collection, kind: &Kind, dir: Dir) -> (usize, Vec<Field>) {
        let parts: Vec<(Field, &[Change])> = match kind {
            Kind::Batch { parts } => parts.iter().map(|(f, c)| (*f, c.as_slice())).collect(),
            _ => return (0, Vec::new()),
        };
        // Undo takes the parts back in reverse, redo replays them in order.
        let order: Vec<usize> = if dir == Dir::Undo { (0..parts.len()).rev().collect() } else { (0..parts.len()).collect() };
        let mut applied = 0;
        let mut fields: Vec<Field> = Vec::new();
        for i in order {
            let (field, changes) = parts[i];
            if !fields.contains(&field) {
                fields.push(field);
            }
            for c in changes {
                let target = if dir == Dir::Undo { &c.before } else { &c.after };
                if let Some(r) = coll.get_mut(c.id) {
                    field.copy(target, r);
                    self.set_baseline(c.id, Marks::of(r));
                    applied += 1;
                }
            }
        }
        (applied, fields)
    }

    /// Record a new step; the redo stack goes.  Returns its serial.
    pub fn push(&mut self, label: String, kind: Kind) -> u64 {
        self.serial += 1;
        self.epoch += 1;
        self.redo.clear();
        self.undo.push(Step { serial: self.serial, label, kind });
        trim(&mut self.undo);
        self.serial
    }

    fn take(&mut self, dir: Dir) -> Option<Step> {
        match dir {
            Dir::Undo => self.undo.pop(),
            Dir::Redo => self.redo.pop(),
        }
    }

    /// A step done in direction `dir` goes onto the other stack.  An undo
    /// that finishes after a new change was made can't be redone (the new
    /// change cleared the redo stack).
    fn finish(&mut self, dir: Dir, step: Step, started: u64) {
        match dir {
            Dir::Undo if started == self.epoch => {
                self.redo.push(step);
                trim(&mut self.redo);
            }
            Dir::Undo => {}
            Dir::Redo => {
                self.undo.push(step);
                trim(&mut self.undo);
            }
        }
    }

    fn remap(&mut self, map: &HashMap<RecordId, RecordId>) {
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            step.kind.remap(map);
        }
    }

    pub fn can(&self, dir: Dir) -> bool {
        self.pending.is_none()
            && match dir {
                Dir::Undo => !self.undo.is_empty(),
                Dir::Redo => !self.redo.is_empty(),
            }
    }

    /// Labels, oldest first (for the test driver).
    pub fn labels(&self, dir: Dir) -> Vec<&str> {
        match dir {
            Dir::Undo => &self.undo,
            Dir::Redo => &self.redo,
        }
        .iter()
        .map(|s| s.label.as_str())
        .collect()
    }
}

// ── planning (no I/O) ───────────────────────────────────────────────────────

/// The moves that undo (or redo) a Sort, for records still where the Sort
/// left them (`path_of` gives a record's current path); the rest are
/// reported.
pub fn plan_moves(moves: &[Moved], dir: Dir, path_of: impl Fn(RecordId) -> Option<PathBuf>) -> (Vec<MoveOp>, Vec<Failure>) {
    let mut ops = Vec::new();
    let mut skipped = Vec::new();
    for m in moves {
        let (src, dest) = match dir {
            Dir::Undo => (&m.to, &m.from),
            Dir::Redo => (&m.from, &m.to),
        };
        match path_of(m.id) {
            Some(p) if p == *src => ops.push(MoveOp { id: m.id, src: src.clone(), dest: dest.clone() }),
            Some(p) => skipped.push((util::file_name(&p), format!("it has moved to {} since", util::parent_name(&p)))),
            None => skipped.push((util::file_name(src), "it is no longer in the working folder".into())),
        }
    }
    (ops, skipped)
}

/// The RAW/ and JPG/ folders an undone Sort moves files out of: removed
/// afterwards if they are left empty.  Only folders inside the working
/// folder are considered.
pub fn cleanup_dirs(ops: &[MoveOp], root: Option<&Path>) -> Vec<PathBuf> {
    let Some(root) = root else { return Vec::new() };
    let mut out: Vec<PathBuf> = Vec::new();
    for op in ops {
        let Some(dir) = op.src.parent() else { continue };
        let name = util::file_name(dir).to_uppercase();
        if (name == "RAW" || name == "JPG") && dir.starts_with(root) && dir != root && !out.iter().any(|d| d == dir) {
            out.push(dir.to_path_buf());
        }
    }
    out.sort();
    out
}

/// Distinct shots (a RAW+JPG pair counts once).
fn count_shots(coll: &Collection, ids: impl Iterator<Item = RecordId>) -> usize {
    ids.map(|id| coll.find_pair(id).map_or(id, |p| p.min(id))).collect::<HashSet<_>>().len()
}

pub fn marks_label(coll: &Collection, field: Field, changes: &[Change]) -> String {
    let shots = count_shots(coll, changes.iter().map(|c| c.id));
    let photos = plural(shots as u64, "photo");
    let all = |f: fn(&Marks) -> bool, after: bool| changes.iter().all(|c| f(if after { &c.after } else { &c.before }));
    match field {
        Field::Prune if all(|m| m.pruned, true) => format!("prune {photos}"),
        Field::Prune if all(|m| !m.pruned, true) => format!("remove the prune mark from {photos}"),
        Field::Prune => format!("change prune marks on {photos}"),
        Field::Mark if all(|m| m.marked, true) => format!("mark {photos} for export"),
        Field::Mark if all(|m| !m.marked, true) => format!("remove the export mark from {photos}"),
        Field::Mark => format!("change export marks on {photos}"),
        Field::Crop => {
            let what = match changes.first().and_then(|c| coll.get(c.id)) {
                Some(r) if shots == 1 => r.stem(),
                _ => photos,
            };
            if all(Marks::cropped, true) && all(|m| !m.cropped(), false) {
                format!("crop {what}")
            } else if all(|m| !m.cropped(), true) {
                format!("clear the crop on {what}")
            } else {
                format!("change the crop on {what}")
            }
        }
    }
}

fn sort_label(n: usize) -> String {
    format!("sort {} into RAW and JPG folders", plural(n as u64, "file"))
}

fn trash_label(n: usize) -> String {
    format!("move {} to the Trash", plural(n as u64, "file"))
}

// ── recording ───────────────────────────────────────────────────────────────

/// After `field` changed on `ids` (pairs included): record a step if
/// anything differs from the baseline.
pub fn note(app: &mut App, field: Field, ids: &[RecordId]) {
    let changes = app.history.diff(&app.coll, field, ids);
    if changes.is_empty() {
        return;
    }
    if let Some(parts) = &mut app.history.gathering {
        parts.push((field, changes));
        return;
    }
    let label = marks_label(&app.coll, field, &changes);
    app.history.push(label, Kind::Batch { parts: vec![(field, changes)] });
}

/// Run `f` (which ends in `refresh_prune` / `refresh_mark`, once or more)
/// as one step named `label`: one Ctrl+Z undoes all of it.
pub fn batch(app: &mut App, label: String, f: impl FnOnce(&mut App) -> Task<Message>) -> (Task<Message>, Option<u64>) {
    app.history.gather();
    let t = f(app);
    let serial = app.history.gathered(label);
    (t, serial)
}

/// The moves in a Sort's result, read before the records are updated.
pub fn sort_moves(app: &App, ok: &[(RecordId, PathBuf)]) -> Vec<Moved> {
    ok.iter()
        .filter_map(|(id, to)| {
            let r = app.coll.get(*id)?;
            (r.path != *to).then(|| Moved { id: *id, from: r.path.clone(), to: to.clone() })
        })
        .collect()
}

/// Record a Sort.  Returns the step's serial.
pub fn record_sort(app: &mut App, moves: Vec<Moved>) -> Option<u64> {
    if moves.is_empty() {
        return None;
    }
    let label = sort_label(moves.len());
    Some(app.history.push(label, Kind::Sort { moves }))
}

fn trashed_files(app: &App, result: &TrashResult) -> Vec<Trashed> {
    let missing: HashSet<&PathBuf> = result.missing.iter().collect();
    result
        .succeeded
        .iter()
        .filter(|p| !missing.contains(p))
        .filter_map(|p| {
            let id = app.coll.id_of(p)?;
            Some(Trashed { id, record: app.coll.get(id)?.clone() })
        })
        .collect()
}

/// Record a move to the Trash, before its records are removed.  Returns the
/// step's serial.
pub fn record_trash(app: &mut App, result: &TrashResult) -> Option<u64> {
    let files = trashed_files(app, result);
    if files.is_empty() {
        return None;
    }
    let label = trash_label(files.len());
    Some(app.history.push(label, Kind::Trash { files, window: (result.started, result.finished) }))
}

// ── running steps ───────────────────────────────────────────────────────────

/// A worker's result.
pub struct Done {
    session: u64,
    outcome: Outcome,
}

enum Outcome {
    Moved(MoveReport),
    Restored(RestoreReport),
    Trashed(TrashResult),
}

pub fn cmd_enabled(app: &App, cmd: Cmd) -> bool {
    let dir = if cmd == Cmd::Redo { Dir::Redo } else { Dir::Undo };
    app.modal.is_none() && app.history.can(dir)
}

pub fn undo(app: &mut App) -> Task<Message> {
    run(app, Dir::Undo)
}

pub fn redo(app: &mut App) -> Task<Message> {
    run(app, Dir::Redo)
}

/// The status bar's Undo button: undo step `serial` if it is still the
/// latest.
pub fn undo_step(app: &mut App, serial: u64) -> Task<Message> {
    if app.history.undo.last().map(|s| s.serial) == Some(serial) {
        return run(app, Dir::Undo);
    }
    if app.history.pending.as_ref().is_some_and(|p| p.step.serial == serial) {
        return Task::none();
    }
    let text = if app.history.redo.iter().any(|s| s.serial == serial) {
        "That has already been undone."
    } else {
        "Newer changes came after that one. Undo them first with Ctrl+Z."
    };
    app.show_status(text, 6000)
}

fn run(app: &mut App, dir: Dir) -> Task<Message> {
    if app.modal.is_some() {
        return Task::none();
    }
    if let Some(p) = &app.history.pending {
        let text = format!("Still {} “{}”. Try again when it's done.", p.dir.ing().to_lowercase(), p.step.label);
        return app.show_status(text, 4000);
    }
    let Some(step) = app.history.take(dir) else {
        return app.show_status(if dir == Dir::Undo { "Nothing to undo" } else { "Nothing to redo" }, 3000);
    };
    match (&step.kind, dir) {
        (Kind::Batch { .. }, _) => apply_marks(app, dir, step),
        (Kind::Sort { .. }, _) => start_moves(app, dir, step),
        (Kind::Trash { .. }, Dir::Undo) => start_restore(app, step),
        (Kind::Trash { .. }, Dir::Redo) => start_retrash(app, step),
    }
}

fn apply_marks(app: &mut App, dir: Dir, step: Step) -> Task<Message> {
    let (applied, fields) = app.history.restore(&mut app.coll, &step.kind, dir);
    if applied == 0 {
        let text = format!("Couldn't {} “{}”: those files are no longer in the working folder.", dir.verb(), step.label);
        return app.show_status(text, 0);
    }
    app.coll.recount_flags();
    app.refresh_views();
    let saves: Vec<Task<Message>> = fields
        .iter()
        .map(|f| match f {
            Field::Prune => app.save_prune_marks(),
            Field::Mark => app.save_export_marks(),
            Field::Crop => app.save_crops(),
        })
        .collect();
    let text = format!("{}: {}", dir.past(), step.label);
    let epoch = app.history.epoch;
    app.history.finish(dir, step, epoch);
    Task::batch(saves.into_iter().chain([app.show_status(text, 0)]))
}

/// Spawn `work` for step `step` running in direction `dir`.
fn spawn(app: &mut App, dir: Dir, step: Step, skipped: Vec<Failure>, work: impl FnOnce() -> Outcome + Send + 'static) -> Task<Message> {
    let text = format!("{}: {}…", dir.ing(), step.label);
    let session = app.history.session;
    let epoch = app.history.epoch;
    app.history.pending = Some(Pending { dir, step, epoch, skipped });
    let bus = app.bus();
    std::thread::spawn(move || {
        let outcome = work();
        let _ = bus.unbounded_send(BgEvent::Undo(Arc::new(Done { session, outcome })));
    });
    app.show_status(text, 0)
}

/// Nothing could even be started: say why.
fn nothing_to_do(app: &mut App, dir: Dir, step: &Step, skipped: &[Failure]) -> Task<Message> {
    let t = app.show_status(format!("Couldn't {} “{}”: none of its files are where it left them.", dir.verb(), step.label), 0);
    Task::batch([t, report(app, dir, is_trash(step), skipped)])
}

fn is_trash(step: &Step) -> bool {
    matches!(step.kind, Kind::Trash { .. })
}

/// (dialog title, what couldn't be done to the files).
fn failure_words(dir: Dir, trash: bool) -> (&'static str, &'static str) {
    match (trash, dir) {
        (true, Dir::Undo) => ("Some files couldn't be restored", "restored from the Trash"),
        (true, Dir::Redo) => ("Some files couldn't be moved to the Trash", "moved to the Trash"),
        (false, Dir::Undo) => ("Some files couldn't be moved back", "moved back"),
        (false, Dir::Redo) => ("Some files couldn't be sorted again", "sorted again"),
    }
}

/// List the files that were left alone, in a dialog.
fn report(app: &mut App, dir: Dir, trash: bool, failed: &[Failure]) -> Task<Message> {
    if failed.is_empty() || app.modal.is_some() {
        return Task::none();
    }
    let n = failed.len();
    let (title, what) = failure_words(dir, trash);
    let mut lines: Vec<String> = failed.iter().take(30).map(|(name, why)| format!("  • {name}: {why}")).collect();
    if n > 30 {
        lines.push(format!("  and {} more", n - 30));
    }
    let text = format!("{} couldn't be {what}. Nothing was overwritten.\n\n{}", plural(n as u64, "file"), lines.join("\n"));
    dialogs::info(app, title, &text)
}

fn start_moves(app: &mut App, dir: Dir, step: Step) -> Task<Message> {
    let Kind::Sort { moves } = &step.kind else { return Task::none() };
    let (ops, skipped) = plan_moves(moves, dir, |id| app.coll.get(id).map(|r| r.path.clone()));
    if ops.is_empty() {
        return nothing_to_do(app, dir, &step, &skipped);
    }
    let cleanup = if dir == Dir::Undo { cleanup_dirs(&ops, app.folder.as_deref()) } else { Vec::new() };
    spawn(app, dir, step, skipped, move || Outcome::Moved(fs_undo::execute_moves(&ops, &cleanup)))
}

fn start_restore(app: &mut App, step: Step) -> Task<Message> {
    let Kind::Trash { files, window } = &step.kind else { return Task::none() };
    let mut skipped = Vec::new();
    let mut targets = Vec::new();
    for f in files {
        if app.coll.id_of(&f.record.path).is_some() {
            skipped.push((f.record.filename(), "a file with that name is in the working folder again, so it was left in the Trash".into()));
        } else {
            targets.push(TrashTarget { id: f.id, path: f.record.path.clone() });
        }
    }
    if targets.is_empty() {
        return nothing_to_do(app, Dir::Undo, &step, &skipped);
    }
    let window = *window;
    spawn(app, Dir::Undo, step, skipped, move || Outcome::Restored(fs_undo::restore_from_trash(&targets, window)))
}

fn start_retrash(app: &mut App, step: Step) -> Task<Message> {
    let Kind::Trash { files, .. } = &step.kind else { return Task::none() };
    let mut skipped = Vec::new();
    let mut paths = Vec::new();
    for f in files {
        match app.coll.get(f.id) {
            Some(r) => paths.push(r.path.clone()),
            None => skipped.push((f.record.filename(), "it is no longer in the working folder".into())),
        }
    }
    if paths.is_empty() {
        return nothing_to_do(app, Dir::Redo, &step, &skipped);
    }
    spawn(app, Dir::Redo, step, skipped, move || Outcome::Trashed(trash::trash_files(&paths)))
}

/// A worker finished a step.
pub fn on_done(app: &mut App, done: Arc<Done>) -> Task<Message> {
    if done.session != app.history.session {
        return Task::none();
    }
    let Some(p) = app.history.pending.take() else { return Task::none() };
    match &done.outcome {
        Outcome::Moved(r) => finish_moves(app, p, r),
        Outcome::Restored(r) => finish_restore(app, p, r),
        Outcome::Trashed(r) => finish_retrash(app, p, r),
    }
}

/// "Undid: sort 21 files into RAW and JPG folders", and how many files
/// were left alone.
fn result_text(dir: Dir, trash: bool, label: &str, done: usize, left: usize) -> String {
    let mut text = if done > 0 { format!("{}: {label}", dir.past()) } else { format!("Couldn't {} anything", dir.verb()) };
    if left > 0 {
        text.push_str(&format!(". {} couldn't be {}.", plural(left as u64, "file"), failure_words(dir, trash).1));
    }
    text
}

/// Rewrite the path-keyed sidecars and refresh, as after a Sort or a trash.
fn files_changed(app: &mut App) -> Vec<Task<Message>> {
    app.coll.build_pairs();
    app.coll.recount_flags();
    app.refresh_views();
    vec![app.save_prune_marks(), app.save_export_marks(), app.save_crops()]
}

fn finish_moves(app: &mut App, p: Pending, r: &MoveReport) -> Task<Message> {
    let Pending { dir, step, epoch, mut skipped } = p;
    let mut tasks = Vec::new();
    let mut moved: HashSet<RecordId> = HashSet::new();
    for op in &r.moved {
        if app.coll.get(op.id).is_some_and(|rec| rec.path == op.src) {
            app.coll.update_path(op.id, op.dest.clone());
        }
        moved.insert(op.id);
    }
    if !moved.is_empty() {
        tasks.extend(files_changed(app));
    }
    skipped.extend(r.failed.iter().cloned());
    let Step { serial, kind, .. } = step;
    let Kind::Sort { moves } = kind else { return Task::batch(tasks) };
    let kept: Vec<Moved> = moves.into_iter().filter(|m| moved.contains(&m.id)).collect();
    let n = kept.len();
    let label = sort_label(n);
    let text = result_text(dir, false, &label, n, skipped.len());
    if n > 0 {
        app.history.finish(dir, Step { serial, label, kind: Kind::Sort { moves: kept } }, epoch);
    }
    tasks.push(if dir == Dir::Redo && n > 0 {
        app.show_status_action(text, "Undo", Message::UndoStep(serial))
    } else {
        app.show_status(text, 0)
    });
    tasks.push(report(app, dir, false, &skipped));
    Task::batch(tasks)
}

fn finish_restore(app: &mut App, p: Pending, r: &RestoreReport) -> Task<Message> {
    let Pending { step, epoch, mut skipped, .. } = p;
    let Kind::Trash { files, .. } = &step.kind else { return Task::none() };
    let by_id: HashMap<RecordId, &Trashed> = files.iter().map(|f| (f.id, f)).collect();
    let mut map = HashMap::new();
    let mut back = Vec::new();
    for (old, path, meta) in &r.restored {
        let Some(t) = by_id.get(old) else { continue };
        let mut rec = t.record.clone();
        rec.set_path(path.clone());
        if let Some(m) = meta {
            rec.file_size = m.len();
            rec.modified = MTime::from_metadata(m);
        }
        rec.pair_stem = None;
        let marks = Marks::of(&rec);
        let id = app.coll.add(rec.clone());
        app.history.set_baseline(id, marks);
        map.insert(*old, id);
        back.push(Trashed { id, record: rec });
    }
    app.history.remap(&map);
    let mut tasks = if back.is_empty() { Vec::new() } else { files_changed(app) };
    skipped.extend(r.failed.iter().cloned());
    let label = trash_label(back.len());
    let text = result_text(Dir::Undo, true, &label, back.len(), skipped.len());
    if !back.is_empty() {
        app.history.finish(Dir::Undo, Step { serial: step.serial, label, kind: Kind::Trash { files: back, window: (0, 0) } }, epoch);
    }
    tasks.push(app.show_status(text, 0));
    tasks.push(report(app, Dir::Undo, true, &skipped));
    Task::batch(tasks)
}

fn finish_retrash(app: &mut App, p: Pending, result: &TrashResult) -> Task<Message> {
    let Pending { step, epoch, mut skipped, .. } = p;
    let mut tasks = Vec::new();
    // Close the viewer if it shows a file that is now gone (as Review does).
    let showing_gone =
        app.viewer.as_ref().is_some_and(|v| !v.read_only && v.record(app).is_some_and(|r| result.succeeded.contains(&r.path)));
    if showing_gone {
        tasks.push(viewer::close(app));
    }
    let files = trashed_files(app, result);
    let n = files.len();
    let label = trash_label(n);
    if n > 0 {
        let kind = Kind::Trash { files, window: (result.started, result.finished) };
        app.history.finish(Dir::Redo, Step { serial: step.serial, label: label.clone(), kind }, epoch);
    }
    if !result.succeeded.is_empty() {
        app.remove_records(&result.succeeded);
        tasks.extend(files_changed(app));
    }
    skipped.extend(result.failed.iter().map(|(p, why)| (util::file_name(p), why.clone())));
    let text = result_text(Dir::Redo, true, &label, n, skipped.len());
    tasks.push(if n > 0 { app.show_status_action(text, "Undo", Message::UndoStep(step.serial)) } else { app.show_status(text, 0) });
    tasks.push(report(app, Dir::Redo, true, &skipped));
    Task::batch(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::FileType;

    fn rec(path: &str) -> PhotoRecord {
        let p = PathBuf::from(path);
        let ft = FileType::from_path(&p).unwrap();
        PhotoRecord::new(p, ft, 10, MTime::from_unix(1_700_000_000, 0), None)
    }

    fn shoot() -> (Collection, RecordId, RecordId, RecordId) {
        let mut c = Collection::new();
        let raw = c.add(rec("/s/IMG_1.ARW"));
        let jpg = c.add(rec("/s/IMG_1.JPG"));
        let lone = c.add(rec("/s/IMG_2.JPG"));
        c.build_pairs();
        (c, raw, jpg, lone)
    }

    #[test]
    fn diff_records_only_what_changed_since_the_baseline() {
        let (mut c, raw, jpg, lone) = shoot();
        let mut h = History::new();
        h.reset_baseline(&c);
        // P on a pair (both halves) and on a photo that was already pruned.
        c.get_mut(lone).unwrap().is_pruned = true;
        h.reset_baseline(&c);
        c.get_mut(raw).unwrap().is_pruned = true;
        c.get_mut(jpg).unwrap().is_pruned = true;
        let changes = h.diff(&c, Field::Prune, &[raw, jpg, lone, raw]);
        assert_eq!(changes.iter().map(|c| c.id).collect::<Vec<_>>(), vec![raw, jpg]);
        assert_eq!(marks_label(&c, Field::Prune, &changes), "prune 1 photo");
        // Nothing new since: no change.
        assert!(h.diff(&c, Field::Prune, &[raw, jpg]).is_empty());
        // Unpruning all three is one step over two shots.
        for id in [raw, jpg, lone] {
            c.get_mut(id).unwrap().is_pruned = false;
        }
        let changes = h.diff(&c, Field::Prune, &[raw, jpg, lone]);
        assert_eq!(changes.len(), 3);
        assert_eq!(marks_label(&c, Field::Prune, &changes), "remove the prune mark from 2 photos");
        // A prune diff ignores marks made outside it.
        c.get_mut(lone).unwrap().is_marked = true;
        assert!(h.diff(&c, Field::Prune, &[lone]).is_empty());
    }

    #[test]
    fn a_bulk_action_is_one_step_and_undoes_whole() {
        // Keep IMG_1 (both halves), prune IMG_2: two fields, one step.
        let (mut c, raw, jpg, lone) = shoot();
        let mut h = History::new();
        h.reset_baseline(&c);
        h.gather();
        c.get_mut(lone).unwrap().is_pruned = true;
        let pruned = h.diff(&c, Field::Prune, &[lone]);
        h.gathering.as_mut().unwrap().push((Field::Prune, pruned));
        c.get_mut(jpg).unwrap().is_marked = true;
        c.get_mut(raw).unwrap().is_marked = true;
        let kept = h.diff(&c, Field::Mark, &[jpg, raw]);
        h.gathering.as_mut().unwrap().push((Field::Mark, kept));
        assert!(h.gathered("keep IMG_1 and prune the rest of its burst".into()).is_some());
        assert_eq!(h.labels(Dir::Undo), vec!["keep IMG_1 and prune the rest of its burst"]);
        // One undo takes both back.
        let step = h.take(Dir::Undo).unwrap();
        let (n, fields) = h.restore(&mut c, &step.kind, Dir::Undo);
        assert_eq!((n, fields), (3, vec![Field::Mark, Field::Prune]));
        assert!(c.iter().all(|r| !r.is_pruned && !r.is_marked));
        // Redo puts both on again.
        let (n, _) = h.restore(&mut c, &step.kind, Dir::Redo);
        assert_eq!(n, 3);
        assert!(c.get(lone).unwrap().is_pruned && c.get(jpg).unwrap().is_marked && c.get(raw).unwrap().is_marked);
        // The baseline followed: nothing new to record.
        assert!(h.diff(&c, Field::Mark, &[jpg, raw]).is_empty());
        // Gathering nothing records nothing.
        h.gather();
        assert_eq!(h.gathered("nothing".into()), None);
        assert!(h.gathering.is_none());
    }

    #[test]
    fn crop_labels_name_a_single_shot() {
        let (mut c, raw, jpg, _) = shoot();
        let mut h = History::new();
        h.reset_baseline(&c);
        let crop = Crop::new(0.1, 0.1, 0.9, 0.9, Some("1:1".into()));
        c.get_mut(raw).unwrap().crop = Some(crop.clone());
        c.get_mut(jpg).unwrap().crop = Some(crop);
        let changes = h.diff(&c, Field::Crop, &[jpg, raw]);
        assert_eq!(marks_label(&c, Field::Crop, &changes), "crop IMG_1");
        c.get_mut(raw).unwrap().crop = None;
        c.get_mut(jpg).unwrap().crop = None;
        let changes = h.diff(&c, Field::Crop, &[jpg, raw]);
        assert_eq!(marks_label(&c, Field::Crop, &changes), "clear the crop on IMG_1");
    }

    fn marks_step(h: &mut History, n: u64) -> u64 {
        h.push(format!("step {n}"), Kind::Batch { parts: vec![(Field::Prune, Vec::new())] })
    }

    #[test]
    fn stack_order_redo_clearing_and_cap() {
        let mut h = History::new();
        assert!(!h.can(Dir::Undo) && !h.can(Dir::Redo));
        for n in 0..3 {
            marks_step(&mut h, n);
        }
        // Undo two, redo one: LIFO both ways.
        let s = h.take(Dir::Undo).unwrap();
        assert_eq!(s.label, "step 2");
        let e = h.epoch;
        h.finish(Dir::Undo, s, e);
        let s = h.take(Dir::Undo).unwrap();
        assert_eq!(s.label, "step 1");
        h.finish(Dir::Undo, s, e);
        assert_eq!(h.labels(Dir::Redo), vec!["step 2", "step 1"]);
        let s = h.take(Dir::Redo).unwrap();
        assert_eq!(s.label, "step 1");
        h.finish(Dir::Redo, s, e);
        assert_eq!(h.labels(Dir::Undo), vec!["step 0", "step 1"]);
        assert_eq!(h.labels(Dir::Redo), vec!["step 2"]);
        // A new step clears redo.
        marks_step(&mut h, 3);
        assert!(!h.can(Dir::Redo));
        // An undo that finishes after a newer step can't be redone.
        let s = h.take(Dir::Undo).unwrap();
        let started = h.epoch;
        marks_step(&mut h, 4);
        h.finish(Dir::Undo, s, started);
        assert!(!h.can(Dir::Redo));
        // Capped, oldest dropped.
        for n in 0..(CAP as u64 + 20) {
            marks_step(&mut h, 100 + n);
        }
        assert_eq!(h.undo.len(), CAP);
        assert_eq!(h.undo.last().unwrap().label, format!("step {}", 100 + CAP as u64 + 19));
        // Clearing starts a new session.
        let session = h.session;
        h.clear();
        assert!(!h.can(Dir::Undo));
        assert_eq!(h.session, session + 1);
    }

    #[test]
    fn remap_follows_records_back_from_the_trash() {
        let mut h = History::new();
        h.push(
            "x".into(),
            Kind::Batch { parts: vec![(Field::Prune, vec![Change { id: 7, before: Marks::default(), after: Marks::default() }])] },
        );
        h.push("y".into(), Kind::Sort { moves: vec![Moved { id: 7, from: "/a".into(), to: "/b".into() }, Moved { id: 8, from: "/c".into(), to: "/d".into() }] });
        h.remap(&HashMap::from([(7, 70)]));
        match (&h.undo[0].kind, &h.undo[1].kind) {
            (Kind::Batch { parts }, Kind::Sort { moves }) => {
                assert_eq!(parts[0].1[0].id, 70);
                assert_eq!((moves[0].id, moves[1].id), (70, 8));
            }
            _ => panic!("kinds"),
        }
    }

    #[test]
    fn sort_reversal_moves_only_files_still_where_sort_left_them() {
        let (mut c, raw, jpg, lone) = shoot();
        let moves = vec![
            Moved { id: raw, from: "/s/IMG_1.ARW".into(), to: "/s/RAW/IMG_1.ARW".into() },
            Moved { id: jpg, from: "/s/IMG_1.JPG".into(), to: "/s/JPG/IMG_1.JPG".into() },
            Moved { id: lone, from: "/s/IMG_2.JPG".into(), to: "/s/JPG/IMG_2.JPG".into() },
        ];
        // As the Sort left them, except the lone JPG, since moved elsewhere,
        // and a record that is gone.
        c.update_path(raw, "/s/RAW/IMG_1.ARW".into());
        c.update_path(jpg, "/s/JPG/IMG_1.JPG".into());
        c.update_path(lone, "/s/picked/IMG_2.JPG".into());
        let mut with_gone = moves.clone();
        with_gone.push(Moved { id: 99, from: "/s/x.jpg".into(), to: "/s/JPG/x.jpg".into() });
        let path_of = |id| c.get(id).map(|r| r.path.clone());
        let (ops, skipped) = plan_moves(&with_gone, Dir::Undo, path_of);
        assert_eq!(
            ops,
            vec![
                MoveOp { id: raw, src: "/s/RAW/IMG_1.ARW".into(), dest: "/s/IMG_1.ARW".into() },
                MoveOp { id: jpg, src: "/s/JPG/IMG_1.JPG".into(), dest: "/s/IMG_1.JPG".into() },
            ]
        );
        assert_eq!(skipped.len(), 2);
        assert!(skipped[0].1.contains("moved to picked"), "{skipped:?}");
        assert!(skipped[1].1.contains("no longer in the working folder"), "{skipped:?}");
        // Folders to tidy: RAW/ and JPG/ inside the working folder only.
        assert_eq!(cleanup_dirs(&ops, Some(Path::new("/s"))), vec![PathBuf::from("/s/JPG"), PathBuf::from("/s/RAW")]);
        assert!(cleanup_dirs(&ops, Some(Path::new("/elsewhere"))).is_empty());
        assert!(cleanup_dirs(&ops, None).is_empty());
        // Redo goes the other way, from where the undo put them.
        c.update_path(raw, "/s/IMG_1.ARW".into());
        let path_of = |id| c.get(id).map(|r| r.path.clone());
        let (ops, skipped) = plan_moves(&moves[..1], Dir::Redo, path_of);
        assert_eq!(ops, vec![MoveOp { id: raw, src: "/s/IMG_1.ARW".into(), dest: "/s/RAW/IMG_1.ARW".into() }]);
        assert!(skipped.is_empty());
    }
}
