//! Modal dialogs (Export, Copy Marked Photos, Sort, Prune Review and the
//! confirm / info boxes) and the Keyboard Shortcuts window.
//!
//! Dialogs are in-window overlays over a dimmed backdrop.  Every file
//! operation runs on a worker thread and reports progress over the bus, so
//! the UI never blocks — even for a large sort on a network share.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::NaiveDateTime;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, checkbox, column, container, opaque, progress_bar, radio, row, scrollable, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Task};

use super::kit;
use super::style;
use super::{App, FolderTarget, Message};
use crate::bus::{BgEvent, SortResult};
use crate::model::collection::RecordId;
use crate::model::record::FileType;
use crate::ops::edit_copy::{self, CopyReport, EditCopyPlan, MarkedFiles};
use crate::ops::library::{self, ExportReport, LibraryPlan, Mode, Resolution};
use crate::ops::separate::{SeparationPlan, Strategy};
use crate::ops::trash;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme as t;
use crate::ui::symbols;
use crate::util::{self, canonical_parent, plural, s};

pub struct Modal {
    pub kind: Kind,
    pub confirm: Option<Confirm>,
}

pub enum Kind {
    Info { title: String, text: String },
    Export(Box<Export>),
    EditCopy(Box<EditCopy>),
    Sort(Box<Sort>),
    Prune(Box<Prune>),
}

pub struct Confirm {
    pub title: String,
    pub text: String,
    pub ok: &'static str,
    pub cancel: &'static str,
    pub action: Msg,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Close,
    ConfirmOk,
    ConfirmCancel,
    // export
    ExMode(Mode),
    ExPruned(bool),
    ExMarkedOnly(bool),
    ExRename(bool),
    ExDateRange(bool),
    ExFrom(String),
    ExTo(String),
    ExBulk(Resolution),
    ExClearMarks,
    ExClearMarksConfirmed,
    ExChangeLibrary,
    ExGo,
    ExOpenLibrary,
    // edit copy
    EcSubfolder(bool),
    EcRaws(bool),
    EcJpgs(bool),
    EcChange,
    EcGo,
    EcStop,
    // sort
    SortStrategy(Strategy),
    SortGo,
    SortConfirmed,
    // prune
    PruneTrash,
    PruneTrashConfirmed,
    PruneUnmark,
    PruneUnmarkConfirmed,
}

// ── export ──────────────────────────────────────────────────────────────────

pub struct Export {
    working: Option<PathBuf>,
    library: Option<PathBuf>,
    mode: Mode,
    include_pruned: bool,
    marked_only: bool,
    has_marked: bool,
    rename: bool,
    date_range: bool,
    date_min: NaiveDateTime,
    date_max: NaiveDateTime,
    from_text: String,
    to_text: String,
    date_from: NaiveDateTime,
    date_to: NaiveDateTime,
    dates: HashMap<RecordId, NaiveDateTime>,
    plan: Option<LibraryPlan>,
    bulk: Option<Resolution>,
    running: Option<(usize, usize)>,
    done: Option<Arc<ExportReport>>,
}

const DT_FMT: &str = "%Y-%m-%d  %H:%M";

fn parse_dt(s: &str) -> Option<NaiveDateTime> {
    let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
    NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M").ok()
}

pub fn open_export(app: &mut App) -> Task<Message> {
    if app.coll.is_empty() {
        return info(app, "No working folder", "Open a folder of photos first (Ctrl+O).");
    }
    let mut dates = HashMap::new();
    for (id, r) in app.coll.iter_ids() {
        let pair = app.coll.find_pair(id).and_then(|p| app.coll.get(p));
        dates.insert(id, library::best_datetime(r, pair));
    }
    let now = chrono::Local::now().naive_local();
    let min = dates.values().min().copied().unwrap_or(now);
    let max = dates.values().max().copied().unwrap_or(now);
    let (min, max) = (trim_minute(min), trim_minute(max));
    let has_marked = app.coll.iter().any(|r| r.is_marked);
    let mut ex = Export {
        working: app.folder.clone(),
        library: app.library.clone(),
        mode: Mode::Copy,
        include_pruned: false,
        marked_only: has_marked,
        has_marked,
        rename: true,
        date_range: false,
        date_min: min,
        date_max: max,
        from_text: min.format(DT_FMT).to_string(),
        to_text: max.format(DT_FMT).to_string(),
        date_from: min,
        date_to: max,
        dates,
        plan: None,
        bulk: None,
        running: None,
        done: None,
    };
    rebuild_export(app, &mut ex);
    app.modal = Some(Modal { kind: Kind::Export(Box::new(ex)), confirm: None });
    Task::none()
}

fn trim_minute(dt: NaiveDateTime) -> NaiveDateTime {
    use chrono::Timelike;
    dt.with_second(0).and_then(|d| d.with_nanosecond(0)).unwrap_or(dt)
}

fn export_ids(app: &App, ex: &Export) -> Vec<RecordId> {
    app.coll
        .iter_ids()
        .filter(|(_, r)| ex.include_pruned || !r.is_pruned)
        .filter(|(_, r)| !ex.marked_only || r.is_marked)
        .filter(|(id, _)| {
            if !ex.date_range {
                return true;
            }
            let d = ex.dates.get(id).copied().unwrap_or_default();
            // Include the whole final minute.
            d >= ex.date_from && d <= ex.date_to + chrono::Duration::seconds(59) + chrono::Duration::microseconds(999_999)
        })
        .map(|(id, _)| id)
        .collect()
}

fn rebuild_export(app: &App, ex: &mut Export) {
    ex.bulk = None;
    ex.plan = ex.library.as_ref().map(|lib| LibraryPlan::new(&export_ids(app, ex), &app.coll, lib, ex.rename));
}

pub fn export_library_changed(app: &mut App, path: PathBuf) {
    let Some(Modal { kind: Kind::Export(ex), .. }) = app.modal.take() else { return };
    let mut ex = ex;
    ex.library = Some(path);
    rebuild_export(app, &mut ex);
    app.modal = Some(Modal { kind: Kind::Export(ex), confirm: None });
}

pub fn on_export_progress(app: &mut App, done: usize, total: usize) {
    if let Some(Modal { kind: Kind::Export(ex), .. }) = &mut app.modal {
        ex.running = Some((done, total));
    }
}

pub fn on_export_done(app: &mut App, report: Arc<ExportReport>) -> Task<Message> {
    if let Some(Modal { kind: Kind::Export(ex), .. }) = &mut app.modal {
        ex.running = None;
        ex.done = Some(report.clone());
    }
    if !report.succeeded.is_empty() {
        app.calendar.mark_stale();
    }
    let mut tasks = Vec::new();
    // A move leaves the working folder: drop those records (and their marks).
    if report.mode == Mode::Move && !report.succeeded.is_empty() {
        let moved: Vec<PathBuf> = report.succeeded.iter().map(|o| o.src.clone()).collect();
        app.remove_records(&moved);
        tasks.push(app.save_prune_marks());
        tasks.push(app.save_export_marks());
        tasks.push(app.save_crops());
    }
    let n = report.succeeded.len();
    let mut msg = format!("{n} file{} exported to library", s(n));
    if !report.failed.is_empty() {
        msg.push_str(&format!("  ({} failed)", report.failed.len()));
    }
    tasks.push(app.show_status(msg, 0));
    Task::batch(tasks)
}

// ── edit copy ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EcGoAction {
    None,
    /// No destination yet: ask for one.
    ChooseFolder,
    Copy,
    /// Copied, or everything was already there.
    OpenFolder,
}

pub struct EditCopy {
    sel: MarkedFiles,
    raws: bool,
    jpgs: bool,
    working: Option<PathBuf>,
    base: Option<PathBuf>,
    subfolder: bool,
    plan: Option<EditCopyPlan>,
    running: Option<(usize, usize, u64, u64)>,
    stopping: bool,
    cancel: Arc<AtomicBool>,
    done: Option<Arc<CopyReport>>,
}

impl EditCopy {
    /// What the primary button does.
    fn go(&self) -> EcGoAction {
        let plan = self.plan.as_ref();
        if self.running.is_some() {
            EcGoAction::None
        } else if self.done.is_some() || plan.is_some_and(|p| p.to_copy_count() == 0 && p.existing_count() > 0) {
            EcGoAction::OpenFolder
        } else if self.base.is_none() && !self.sel.files.is_empty() {
            EcGoAction::ChooseFolder
        } else if plan.is_some_and(|p| p.to_copy_count() > 0) {
            EcGoAction::Copy
        } else {
            EcGoAction::None
        }
    }

    /// The primary button (Copy / Choose folder / Open folder) is clickable.
    fn go_enabled(&self) -> bool {
        self.go() != EcGoAction::None
    }

    /// For the dialog's subtitle: "The RAW", "The JPG" or "The RAW and JPG".
    fn which(&self) -> &'static str {
        match (self.raws, self.jpgs) {
            (true, false) => "The RAW",
            (false, true) => "The JPG",
            _ => "The RAW and JPG",
        }
    }

    /// What is being copied, for counts: "RAW file", "JPG file" or "file".
    fn noun(&self) -> &'static str {
        match (self.raws, self.jpgs) {
            (true, false) => "RAW file",
            (false, true) => "JPG file",
            _ => "file",
        }
    }

    /// The choice of RAWs and JPGs changed: pick the files again and plan.
    fn reselect(&mut self, coll: &crate::model::collection::Collection) {
        self.sel = edit_copy::select_marked(coll, self.raws, self.jpgs);
        self.plan = self.dest_dir().map(|d| EditCopyPlan::new(&self.sel.files, coll, &d));
    }

    fn dest_dir(&self) -> Option<PathBuf> {
        let base = self.base.clone()?;
        match (&self.working, self.subfolder) {
            (Some(w), true) => Some(base.join(util::file_name(w))),
            _ => Some(base),
        }
    }
}

pub fn open_edit_copy(app: &mut App) -> Task<Message> {
    if app.coll.is_empty() {
        return info(app, "No working folder", "Open a folder of photos first (Ctrl+O).");
    }
    let (raws, jpgs) = (app.settings.edit_copy_raws, app.settings.edit_copy_jpgs);
    let (raws, jpgs) = if raws || jpgs { (raws, jpgs) } else { (true, false) };
    let sel = edit_copy::select_marked(&app.coll, raws, jpgs);
    if sel.files.is_empty() && sel.missing.is_empty() {
        return info(
            app,
            "Nothing marked",
            "No shots are marked.\n\nSelect photos and press M (or Keep in the viewer) to mark the ones you want to edit.",
        );
    }
    let last = PathBuf::from(&app.settings.edit_copy_folder);
    let base = (!app.settings.edit_copy_folder.is_empty() && last.is_dir()).then_some(last);
    let mut ec = EditCopy {
        sel,
        raws,
        jpgs,
        working: app.folder.clone(),
        base,
        subfolder: app.settings.edit_copy_subfolder && app.folder.is_some(),
        plan: None,
        running: None,
        stopping: false,
        cancel: Arc::new(AtomicBool::new(false)),
        done: None,
    };
    ec.plan = ec.dest_dir().map(|d| EditCopyPlan::new(&ec.sel.files, &app.coll, &d));
    app.modal = Some(Modal { kind: Kind::EditCopy(Box::new(ec)), confirm: None });
    Task::none()
}

pub fn edit_copy_base(app: &App) -> Option<PathBuf> {
    match &app.modal {
        Some(Modal { kind: Kind::EditCopy(ec), .. }) => ec.base.clone(),
        _ => None,
    }
}

pub fn edit_copy_folder_changed(app: &mut App, path: PathBuf) {
    let Some(Modal { kind: Kind::EditCopy(ec), .. }) = &mut app.modal else { return };
    ec.base = Some(path.clone());
    ec.plan = ec.dest_dir().map(|d| EditCopyPlan::new(&ec.sel.files, &app.coll, &d));
    app.settings.edit_copy_folder = path.to_string_lossy().into_owned();
    app.settings.edit_copy_subfolder = ec.subfolder;
    let _ = app.settings.save();
}

/// Preferences changed the destination or the subfolder option: an open
/// dialog that hasn't started copying follows it.
pub fn edit_copy_settings_changed(app: &mut App) {
    let s = &app.settings;
    let base = (!s.edit_copy_folder.is_empty()).then(|| PathBuf::from(&s.edit_copy_folder));
    let subfolder = s.edit_copy_subfolder;
    let Some(Modal { kind: Kind::EditCopy(ec), .. }) = &mut app.modal else { return };
    if ec.running.is_some() || ec.done.is_some() {
        return;
    }
    if base.is_some() {
        ec.base = base;
    }
    ec.subfolder = subfolder && ec.working.is_some();
    ec.plan = ec.dest_dir().map(|d| EditCopyPlan::new(&ec.sel.files, &app.coll, &d));
}

pub fn on_copy_progress(app: &mut App, f: usize, ft: usize, b: u64, bt: u64) {
    if let Some(Modal { kind: Kind::EditCopy(ec), .. }) = &mut app.modal {
        ec.running = Some((f, ft, b, bt));
    }
}

pub fn on_copy_done(app: &mut App, report: Arc<CopyReport>) -> Task<Message> {
    let mut noun = "file";
    if let Some(Modal { kind: Kind::EditCopy(ec), .. }) = &mut app.modal {
        ec.running = None;
        ec.stopping = false;
        ec.done = Some(report.clone());
        noun = ec.noun();
    }
    let n = report.copied.len();
    let mut msg = format!("{n} {noun}{} copied for editing", s(n));
    if !report.failed.is_empty() {
        msg.push_str(&format!("  ({} failed)", report.failed.len()));
    }
    app.show_status(msg, 0)
}

// ── sort / pair / prune ─────────────────────────────────────────────────────

pub struct Sort {
    plan: SeparationPlan,
    running: bool,
}

pub struct Prune {
    ids: Vec<RecordId>,
    remote: bool,
    running: bool,
}

pub fn open_sort(app: &mut App) -> Task<Message> {
    if app.coll.is_empty() {
        return info(app, "No working folder", "Open a folder of photos first (Ctrl+O).");
    }
    let plan = SeparationPlan::new(&app.coll);
    app.modal = Some(Modal { kind: Kind::Sort(Box::new(Sort { plan, running: false })), confirm: None });
    Task::none()
}

pub fn open_review(app: &mut App) -> Task<Message> {
    let ids: Vec<RecordId> = app.coll.iter_ids().filter(|(_, r)| r.is_pruned).map(|(id, _)| id).collect();
    if ids.is_empty() {
        return info(app, "Nothing to review", "No files are marked to prune. Mark photos with P in the grid or the viewer.");
    }
    let paths: Vec<PathBuf> = ids.iter().filter_map(|&i| app.coll.get(i)).map(|r| r.path.clone()).collect();
    let remote = trash::has_remote_files(&paths);
    app.modal = Some(Modal { kind: Kind::Prune(Box::new(Prune { ids, remote, running: false })), confirm: None });
    Task::none()
}

pub fn on_trashed(app: &mut App, result: Arc<trash::TrashResult>) -> Task<Message> {
    app.modal = None;
    let mut tasks = Vec::new();
    if !result.succeeded.is_empty() {
        // Close the viewer if it is showing a file that is now gone.
        let showing_gone = app.viewer.as_ref().is_some_and(|v| {
            !v.read_only && v.record(app).is_some_and(|r| result.succeeded.contains(&r.path))
        });
        if showing_gone {
            tasks.push(super::viewer::close(app));
        }
        let step = super::undo::record_trash(app, &result);
        app.remove_records(&result.succeeded);
        tasks.push(app.save_prune_marks());
        let n = result.succeeded.len();
        let text = format!("Moved {n} file{} to the Trash", s(n));
        tasks.push(match step {
            Some(serial) => app.show_status_action(text, "Undo", Message::UndoStep(serial)),
            None => app.show_status(text, 0),
        });
    }
    if !result.failed.is_empty() {
        let details: Vec<String> = result.failed.iter().map(|(p, m)| format!("  • {}: {m}", util::file_name(p))).collect();
        let n = result.failed.len();
        tasks.push(info(app, "Some files could not be moved to the Trash", &format!("{n} file{} could not be moved:\n\n{}", s(n), details.join("\n"))));
    }
    Task::batch(tasks)
}

fn on_sorted(app: &mut App, result: Arc<SortResult>) -> Task<Message> {
    let (ok, failed) = (&result.0, &result.1);
    let moves = super::undo::sort_moves(app, ok);
    let mut moved = 0;
    for (id, new_path) in ok {
        if app.coll.get(*id).is_some_and(|r| &r.path != new_path) {
            moved += 1;
            app.coll.update_path(*id, new_path.clone());
        }
    }
    // Pairs key on the canonical parent (RAW/ and JPG/ stripped), so every
    // pair re-forms across the new subfolders.
    app.coll.build_pairs();
    app.refresh_views();
    let step = super::undo::record_sort(app, moves);
    // Path-keyed sidecars must be rewritten or every mark is lost.
    let mut tasks = vec![app.save_prune_marks(), app.save_export_marks(), app.save_crops()];
    let pairs = app.coll.stats().paired / 2;
    let mut msg = format!("Sorted {moved} file{}, {pairs} RAW+JPG pair{}", s(moved), s(pairs));
    if !failed.is_empty() {
        msg.push_str(&format!("  ({} failed)", failed.len()));
    }
    tasks.push(match step {
        Some(serial) => app.show_status_action(msg, "Undo", Message::UndoStep(serial)),
        None => app.show_status(msg, 0),
    });
    app.modal = None;
    if !failed.is_empty() {
        let details: Vec<String> = failed.iter().map(|(_, name, m)| format!("  • {name}: {m}")).collect();
        let n = failed.len();
        tasks.push(info(app, "Some files could not be moved", &format!("{n} file{} failed:\n\n{}", s(n), details.join("\n"))));
    }
    Task::batch(tasks)
}

pub fn info(app: &mut App, title: &str, text: &str) -> Task<Message> {
    app.modal = Some(Modal { kind: Kind::Info { title: title.into(), text: text.into() }, confirm: None });
    Task::none()
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    if let Msg::ConfirmCancel = msg {
        if let Some(m) = &mut app.modal {
            m.confirm = None;
        }
        return Task::none();
    }
    if let Msg::ConfirmOk = msg {
        let action = app.modal.as_mut().and_then(|m| m.confirm.take()).map(|c| c.action);
        return match action {
            Some(a) => update(app, a),
            None => Task::none(),
        };
    }
    if let Msg::Close = msg {
        // A running copy stops first; the dialog closes when it reports back.
        if let Some(Modal { kind: Kind::EditCopy(ec), .. }) = &mut app.modal {
            if ec.running.is_some() {
                ec.cancel.store(true, Ordering::Relaxed);
                ec.stopping = true;
                return Task::none();
            }
        }
        if let Some(Modal { kind: Kind::Export(ex), .. }) = &app.modal {
            if ex.running.is_some() {
                return Task::none();
            }
        }
        if let Some(Modal { kind: Kind::Sort(so), .. }) = &app.modal {
            if so.running {
                return Task::none();
            }
        }
        app.modal = None;
        return Task::none();
    }

    let bus = app.bus();
    let Some(modal) = app.modal.take() else { return Task::none() };
    let Modal { kind, confirm } = modal;
    let mut task = Task::none();
    let kind = match kind {
        Kind::Export(mut ex) => {
            match msg {
                Msg::ExMode(m) => ex.mode = m,
                Msg::ExPruned(v) => {
                    ex.include_pruned = v;
                    rebuild_export(app, &mut ex);
                }
                Msg::ExMarkedOnly(v) => {
                    ex.marked_only = v;
                    rebuild_export(app, &mut ex);
                }
                Msg::ExRename(v) => {
                    ex.rename = v;
                    rebuild_export(app, &mut ex);
                }
                Msg::ExDateRange(v) => {
                    ex.date_range = v;
                    rebuild_export(app, &mut ex);
                }
                Msg::ExFrom(t) => {
                    ex.from_text = t;
                    if let Some(d) = parse_dt(&ex.from_text) {
                        ex.date_from = d.clamp(ex.date_min, ex.date_max);
                        if ex.date_from > ex.date_to {
                            ex.date_to = ex.date_from;
                            ex.to_text = ex.date_to.format(DT_FMT).to_string();
                        }
                        if ex.date_range {
                            rebuild_export(app, &mut ex);
                        }
                    }
                }
                Msg::ExTo(t) => {
                    ex.to_text = t;
                    if let Some(d) = parse_dt(&ex.to_text) {
                        ex.date_to = d.clamp(ex.date_min, ex.date_max);
                        if ex.date_to < ex.date_from {
                            ex.date_from = ex.date_to;
                            ex.from_text = ex.date_from.format(DT_FMT).to_string();
                        }
                        if ex.date_range {
                            rebuild_export(app, &mut ex);
                        }
                    }
                }
                Msg::ExBulk(r) => {
                    if let Some(p) = &mut ex.plan {
                        p.apply_bulk_resolution(r);
                        ex.bulk = Some(r);
                    }
                }
                Msg::ExClearMarks => {
                    let n = app.coll.iter().filter(|r| r.is_marked).count();
                    if n > 0 {
                        app.modal = Some(Modal {
                            kind: Kind::Export(ex),
                            confirm: Some(Confirm {
                                title: "Clear Marks".into(),
                                text: format!("Clear the export mark from {n} file{}?", s(n)),
                                ok: "Yes",
                                cancel: "No",
                                action: Msg::ExClearMarksConfirmed,
                            }),
                        });
                        return Task::none();
                    }
                }
                Msg::ExClearMarksConfirmed => {
                    let ids: Vec<RecordId> = app.coll.iter_ids().filter(|(_, r)| r.is_marked).map(|(id, _)| id).collect();
                    for &id in &ids {
                        if let Some(r) = app.coll.get_mut(id) {
                            r.is_marked = false;
                        }
                    }
                    task = app.refresh_mark(&ids);
                    ex.marked_only = false;
                    ex.has_marked = false;
                    rebuild_export(app, &mut ex);
                }
                Msg::ExChangeLibrary => {
                    app.modal = Some(Modal { kind: Kind::Export(ex), confirm });
                    return app.pick_folder(FolderTarget::ExportLibrary);
                }
                Msg::ExGo => {
                    if let (Some(plan), None) = (ex.plan.clone(), &ex.done) {
                        if plan.total() > 0 && ex.running.is_none() {
                            ex.running = Some((0, plan.ops.iter().filter(|o| o.resolution != Resolution::Skip).count()));
                            let mode = ex.mode;
                            let source_root = ex.working.clone();
                            let bus = bus.clone();
                            std::thread::spawn(move || {
                                let pbus = bus.clone();
                                let cancel = AtomicBool::new(false);
                                let report = plan.execute(mode, source_root.as_deref(), &cancel, &move |d, t| {
                                    let _ = pbus.unbounded_send(BgEvent::ExportProgress { done: d, total: t });
                                });
                                let _ = bus.unbounded_send(BgEvent::ExportDone(Arc::new(report)));
                            });
                        }
                    }
                }
                Msg::ExOpenLibrary => {
                    if let Some(l) = &ex.library {
                        open_folder(l);
                    }
                }
                _ => {}
            }
            Kind::Export(ex)
        }
        Kind::EditCopy(mut ec) => {
            match msg {
                Msg::EcSubfolder(v) => {
                    ec.subfolder = v;
                    ec.plan = ec.dest_dir().map(|d| EditCopyPlan::new(&ec.sel.files, &app.coll, &d));
                    if let Some(b) = &ec.base {
                        app.settings.edit_copy_folder = b.to_string_lossy().into_owned();
                        app.settings.edit_copy_subfolder = v;
                        let _ = app.settings.save();
                    }
                }
                Msg::EcRaws(v) => {
                    ec.raws = v;
                    ec.reselect(&app.coll);
                    app.settings.edit_copy_raws = v;
                    let _ = app.settings.save();
                }
                Msg::EcJpgs(v) => {
                    ec.jpgs = v;
                    ec.reselect(&app.coll);
                    app.settings.edit_copy_jpgs = v;
                    let _ = app.settings.save();
                }
                Msg::EcChange => {
                    app.modal = Some(Modal { kind: Kind::EditCopy(ec), confirm });
                    return app.pick_folder(FolderTarget::EditCopy);
                }
                Msg::EcGo => match ec.go() {
                    EcGoAction::None => {}
                    EcGoAction::OpenFolder => {
                        if let Some(d) = ec.dest_dir() {
                            open_folder(&d);
                        }
                    }
                    EcGoAction::ChooseFolder => {
                        app.modal = Some(Modal { kind: Kind::EditCopy(ec), confirm });
                        return app.pick_folder(FolderTarget::EditCopy);
                    }
                    EcGoAction::Copy => {
                        if let Some(dest) = ec.dest_dir() {
                            // Re-plan: the folder may have changed while the dialog sat open.
                            let plan = EditCopyPlan::new(&ec.sel.files, &app.coll, &dest);
                            if plan.to_copy_count() > 0 {
                                ec.running = Some((0, plan.to_copy_count(), 0, plan.total_bytes()));
                                ec.cancel = Arc::new(AtomicBool::new(false));
                                let cancel = ec.cancel.clone();
                                let run_plan = plan.clone();
                                let bus = bus.clone();
                                std::thread::spawn(move || {
                                    let pbus = bus.clone();
                                    let report = run_plan.execute(&cancel, &move |f, ft, b, bt| {
                                        let _ = pbus.unbounded_send(BgEvent::CopyProgress { files: f, files_total: ft, bytes: b, bytes_total: bt });
                                    });
                                    let _ = bus.unbounded_send(BgEvent::CopyDone(Arc::new(report)));
                                });
                            }
                            ec.plan = Some(plan);
                        }
                    }
                },
                Msg::EcStop => {
                    ec.cancel.store(true, Ordering::Relaxed);
                    ec.stopping = true;
                }
                _ => {}
            }
            Kind::EditCopy(ec)
        }
        Kind::Sort(mut so) => {
            match msg {
                Msg::SortStrategy(st) => so.plan.set_strategy(st),
                Msg::SortGo => {
                    let n = so.plan.move_count();
                    app.modal = Some(Modal {
                        kind: Kind::Sort(so),
                        confirm: Some(Confirm {
                            title: "Sort Files".into(),
                            text: format!(
                                "Move {n} file{} into RAW/ and JPG/ subfolders?\n\nYou can undo this with Ctrl+Z.",
                                s(n)
                            ),
                            ok: "OK",
                            cancel: "Cancel",
                            action: Msg::SortConfirmed,
                        }),
                    });
                    return Task::none();
                }
                Msg::SortConfirmed => {
                    so.running = true;
                    let plan_ops = SeparationPlan { ops: so.plan.ops.clone(), strategy: so.plan.strategy };
                    let bus = bus.clone();
                    std::thread::spawn(move || {
                        let result = plan_ops.execute();
                        let _ = bus.unbounded_send(BgEvent::Sorted(Arc::new(result)));
                    });
                }
                _ => {}
            }
            Kind::Sort(so)
        }
        Kind::Prune(mut pr) => {
            match msg {
                Msg::PruneTrash => {
                    let n = pr.ids.len();
                    app.modal = Some(Modal {
                        kind: Kind::Prune(pr),
                        confirm: Some(Confirm {
                            title: "Move to Trash".into(),
                            text: format!("Move {n} file{} to the system Trash?\n\nYou can undo this with Ctrl+Z, or restore them from the Trash later.", s(n)),
                            ok: "OK",
                            cancel: "Cancel",
                            action: Msg::PruneTrashConfirmed,
                        }),
                    });
                    return Task::none();
                }
                Msg::PruneTrashConfirmed => {
                    pr.running = true;
                    let paths: Vec<PathBuf> = pr.ids.iter().filter_map(|&i| app.coll.get(i)).map(|r| r.path.clone()).collect();
                    let bus = bus.clone();
                    std::thread::spawn(move || {
                        let result = trash::trash_files(&paths);
                        let _ = bus.unbounded_send(BgEvent::Trashed(Arc::new(result)));
                    });
                }
                Msg::PruneUnmark => {
                    let n = pr.ids.len();
                    app.modal = Some(Modal {
                        kind: Kind::Prune(pr),
                        confirm: Some(Confirm {
                            title: "Unmark All".into(),
                            text: format!("Remove the prune mark from all {n} file{}?", s(n)),
                            ok: "Yes",
                            cancel: "No",
                            action: Msg::PruneUnmarkConfirmed,
                        }),
                    });
                    return Task::none();
                }
                Msg::PruneUnmarkConfirmed => {
                    let ids: Vec<RecordId> = app.coll.iter_ids().filter(|(_, r)| r.is_pruned).map(|(id, _)| id).collect();
                    for &id in &ids {
                        if let Some(r) = app.coll.get_mut(id) {
                            r.is_pruned = false;
                        }
                    }
                    app.modal = None;
                    return app.refresh_prune(&ids);
                }
                _ => {}
            }
            Kind::Prune(pr)
        }
        other => other,
    };
    app.modal = Some(Modal { kind, confirm });
    task
}

/// Results from worker threads that finish a dialog.
impl App {
    pub fn on_sorted_event(&mut self, result: Arc<SortResult>) -> Task<Message> {
        on_sorted(self, result)
    }
}

pub fn on_key(app: &mut App, key: &Key, _mods: keyboard::Modifiers) -> Task<Message> {
    let has_confirm = app.modal.as_ref().is_some_and(|m| m.confirm.is_some());
    match key {
        Key::Named(Named::Escape) => {
            if has_confirm {
                update(app, Msg::ConfirmCancel)
            } else {
                update(app, Msg::Close)
            }
        }
        Key::Named(Named::Enter) if has_confirm => update(app, Msg::ConfirmOk),
        Key::Named(Named::Enter) => match &app.modal {
            Some(Modal { kind: Kind::Info { .. }, .. }) => update(app, Msg::Close),
            // The only default button in the Python dialogs: Copy (or Choose
            // folder / Open folder).
            Some(Modal { kind: Kind::EditCopy(ec), .. }) if ec.go_enabled() => update(app, Msg::EcGo),
            _ => Task::none(),
        },
        _ => Task::none(),
    }
}

pub(super) fn open_folder(path: &std::path::Path) {
    let p = path.to_path_buf();
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("explorer").arg(&p).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&p).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&p).spawn();
    });
}

// ── view helpers ────────────────────────────────────────────────────────────

fn d(m: Msg) -> Message {
    Message::Dialog(m)
}

fn dm(m: Msg) -> Option<Message> {
    Some(d(m))
}

/// A small tinted summary label; ✂ ⚠ inside are drawn as icons.
fn chip<'a>(label: String, tone: Color) -> Element<'a, Message> {
    container(symbols::line(&label, t::T_SMALL, paint::MEDIUM, tone))
        .padding(Padding::from([4.0, 10.0]))
        .style(style::chip(tone))
        .into()
}

fn check<'a>(label: &'a str, value: bool, tone: Color, on: Option<fn(bool) -> Msg>) -> Element<'a, Message> {
    let mut c = checkbox(value).label(label).size(16).text_size(t::T_BODY).spacing(t::S2).style(style::check(tone));
    if let Some(f) = on {
        c = c.on_toggle(move |v| d(f(v)));
    }
    c.into()
}

/// A scrolling list inside a recessed, bordered well.
fn list_box<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(scrollable(container(content).padding(Padding::from([4.0, 0.0]))).style(style::scroll).width(Length::Fill).height(Length::Fill))
        .padding(Padding::from([2.0, 4.0]))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::group())
        .into()
}

/// Buttons along the bottom: extras on the left, then the secondary and
/// the primary action on the right.
pub(super) fn footer<'a>(left: Vec<Element<'a, Message>>, right: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut r = row![].spacing(t::S2).align_y(Alignment::Center);
    for e in left {
        r = r.push(e);
    }
    r = r.push(Space::new().width(Length::Fill));
    for e in right {
        r = r.push(e);
    }
    r.into()
}

fn frame<'a>(content: Element<'a, Message>, w: f32, h: f32) -> Element<'a, Message> {
    container(content).width(Length::Fixed(w)).height(Length::Fixed(h)).padding(t::S5).style(style::floating()).into()
}

/// A dialog whose height follows its content.
pub(super) fn frame_fit<'a>(content: Element<'a, Message>, w: f32) -> Element<'a, Message> {
    container(content).width(Length::Fixed(w)).padding(t::S5).style(style::floating()).into()
}

/// Dims the window and lifts the dialog into place.
pub(super) fn backdrop<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    opaque(crate::ui::motion::Appear::new(container(content).center(Length::Fill).padding(t::S5)).scrim(t::SCRIM).rise(14.0)).into()
}

pub(super) fn header<'a>(title: impl text::IntoFragment<'a>, subtitle: Option<String>) -> Element<'a, Message> {
    let mut c = column![kit::title(title)].spacing(t::S1);
    if let Some(sub) = subtitle.filter(|s| !s.is_empty()) {
        c = c.push(kit::secondary(sub));
    }
    c.into()
}

pub fn view<'a>(app: &'a App, modal: &'a Modal) -> Element<'a, Message> {
    let (w, h) = (app.window_size_width() - 48.0, app.window_size_height() - 48.0);
    let body = match &modal.kind {
        Kind::Info { title, text: body } => {
            let content = column![
                header(title.clone(), None),
                kit::secondary(body.clone()),
                Space::new().height(t::S2),
                footer(vec![], vec![kit::btn("OK", style::primary(), dm(Msg::Close)).into()]),
            ]
            .spacing(t::S3);
            frame_fit(content.into(), 440f32.min(w))
        }
        Kind::Export(ex) => export_view(app, ex, w, h),
        Kind::EditCopy(ec) => edit_copy_view(app, ec, w, h),
        Kind::Sort(so) => sort_view(app, so, w, h),
        Kind::Prune(pr) => prune_view(app, pr, w, h),
    };
    let mut layers: Vec<Element<'a, Message>> = vec![backdrop(body)];
    if let Some(c) = &modal.confirm {
        let content = column![
            header(c.title.clone(), None),
            kit::secondary(c.text.clone()),
            Space::new().height(t::S2),
            footer(
                vec![],
                vec![kit::btn(c.cancel, style::secondary(), dm(Msg::ConfirmCancel)).into(), kit::btn(c.ok, style::primary(), dm(Msg::ConfirmOk)).into()],
            ),
        ]
        .spacing(t::S3);
        layers.push(backdrop(frame_fit(content.into(), 440f32.min(w))));
    }
    iced::widget::stack(layers).width(Length::Fill).height(Length::Fill).into()
}

impl App {
    pub fn window_size_height(&self) -> f32 {
        self.window_size.height
    }
}

// ── export view ─────────────────────────────────────────────────────────────

/// One folder line: icon, what it is, where, and an optional Change link.
fn path_row<'a>(glyph: Glyph, label: &'a str, path: Option<&PathBuf>, link: Option<Msg>) -> Element<'a, Message> {
    let path_text: Element<'a, Message> = match path {
        Some(p) => text(util::elide_path(p, 64)).size(t::T_BODY).color(t::INK).width(Length::Fill).into(),
        None => text(if link.is_some() { "Not set" } else { "—" }).size(t::T_BODY).color(t::INK3).width(Length::Fill).into(),
    };
    let mut r = row![icons::view(glyph, 16.0, t::INK2), kit::caption(label).width(104), path_text].spacing(t::S2).align_y(Alignment::Center);
    if let Some(m) = link {
        r = r.push(
            button(text(if path.is_some() { "Change" } else { "Choose…" }).size(t::T_SMALL).font(paint::MEDIUM))
                .padding(Padding::from([2.0, 4.0]))
                .style(style::link().style())
                .on_press(d(m)),
        );
    }
    r.height(t::CONTROL_H).into()
}

fn group_box<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content).padding(Padding::from([t::S2, t::S3])).width(Length::Fill).style(style::group()).into()
}

/// A large two-line choice (Copy / Move).
fn mode_btn<'a>(title: &'a str, sub: &'a str, selected: bool, m: Mode, enabled: bool) -> Element<'a, Message> {
    let content = column![
        text(title).size(t::T_BODY).font(paint::SEMIBOLD).color(if selected { t::INK } else { t::INK2 }),
        text(sub).size(t::T_SMALL).color(t::INK3),
    ]
    .spacing(2)
    .width(Length::Fill);
    button(content)
        .width(Length::Fill)
        .padding(Padding::from([t::S2, t::S3]))
        .style(move |_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if selected { t::RAISED } else if hot { t::MOUNT } else { t::GROUND })),
                text_color: t::INK,
                border: Border {
                    color: if selected {
                        t::SIGNAL
                    } else if hot {
                        t::LINE_HI
                    } else {
                        t::LINE
                    },
                    width: if selected { 1.5 } else { 1.0 },
                    radius: t::R_CONTROL.into(),
                },
                ..button::Style::default()
            }
        })
        .on_press_maybe(enabled.then(|| d(Msg::ExMode(m))))
        .into()
}

fn date_input<'a>(value: &'a str, enabled: bool, f: fn(String) -> Msg) -> Element<'a, Message> {
    text_input("YYYY-MM-DD  HH:MM", value)
        .on_input_maybe(enabled.then_some(move |v| d(f(v))))
        .width(160)
        .size(t::T_SMALL)
        .padding(Padding::from([5.0, 8.0]))
        .style(style::input())
        .into()
}

fn export_view<'a>(app: &'a App, ex: &'a Export, w: f32, h: f32) -> Element<'a, Message> {
    let running = ex.running.is_some();
    let locked = running || ex.done.is_some();
    let paths = group_box(
        column![
            path_row(Glyph::Folder, "Working folder", ex.working.as_ref(), None),
            path_row(Glyph::Library, "Library", ex.library.as_ref(), (!locked).then_some(Msg::ExChangeLibrary)),
        ]
        .into(),
    );

    let rename_example = match (&ex.plan, ex.rename) {
        (Some(p), true) => p.ops.first().map(|o| format!("for example {}", util::file_name(&o.dest))).unwrap_or_default(),
        _ => String::new(),
    };
    let lock = |f: fn(bool) -> Msg| (!locked).then_some(f);
    let options = column![
        row![
            mode_btn("Copy", "Keep the originals in the working folder", ex.mode == Mode::Copy, Mode::Copy, !locked),
            mode_btn("Move", "Remove the originals once exported", ex.mode == Mode::Move, Mode::Move, !locked),
        ]
        .spacing(t::S2),
        Space::new().height(t::S1),
        row![
            check("Only files marked for export", ex.marked_only, t::KEEP, (ex.has_marked && !locked).then_some(Msg::ExMarkedOnly as fn(bool) -> Msg)),
            Space::new().width(Length::Fill),
            button(text("Clear all marks").size(t::T_SMALL).font(paint::MEDIUM))
                .padding(Padding::from([2.0, 4.0]))
                .style(style::link().style())
                .on_press_maybe((ex.has_marked && !locked).then_some(d(Msg::ExClearMarks))),
        ]
        .align_y(Alignment::Center),
        check("Include pruned files", ex.include_pruned, t::SIGNAL, lock(Msg::ExPruned)),
        row![check("Rename to date, time and sequence", ex.rename, t::SIGNAL, lock(Msg::ExRename)), kit::caption(rename_example)]
            .spacing(t::S2)
            .align_y(Alignment::Center),
        row![
            check("Only photos taken between", ex.date_range, t::SIGNAL, lock(Msg::ExDateRange)),
            date_input(&ex.from_text, ex.date_range && !locked, Msg::ExFrom),
            kit::caption("and"),
            date_input(&ex.to_text, ex.date_range && !locked, Msg::ExTo),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center),
    ]
    .spacing(t::S3);

    // What will happen
    let mut chips = row![].spacing(t::S2).align_y(Alignment::Center);
    if let Some(plan) = &ex.plan {
        let ids = export_ids(app, ex);
        let unp_raw = ids.iter().filter_map(|&i| app.coll.get(i)).filter(|r| r.file_type == FileType::Raw && !r.is_paired()).count();
        let unp_jpg = ids.iter().filter_map(|&i| app.coll.get(i)).filter(|r| r.file_type == FileType::Jpg && !r.is_paired()).count();
        let n_pruned = if ex.include_pruned { ids.iter().filter_map(|&i| app.coll.get(i)).filter(|r| r.is_pruned).count() } else { 0 };
        let n = plan.total();
        chips = chips.push(chip(format!("{} to {}", plural(n as u64, "file"), ex.mode.verb()), t::INK));
        let c = plan.conflict_count();
        if c > 0 {
            chips = chips.push(chip(format!("⚠ {}", plural(c as u64, "conflict")), t::WARN));
        }
        if unp_raw > 0 {
            chips = chips.push(chip(format!("{unp_raw} RAW without a JPG"), t::RAW));
        }
        if unp_jpg > 0 {
            chips = chips.push(chip(format!("{unp_jpg} JPG without a RAW"), t::JPG));
        }
        if n_pruned > 0 {
            chips = chips.push(chip(format!("{n_pruned} pruned"), t::REJECT));
        }
        let cr = plan.cropped_count();
        if cr > 0 {
            chips = chips.push(kit::tip(
                chip(format!("✂ {cr} cropped"), t::SIGNAL),
                "Cropped JPGs are re-encoded with the crop applied. Cropped RAWs are copied untouched, with an .xmp sidecar holding the crop for Lightroom and Camera Raw.",
                false,
            ));
        }
    }

    // Preview / summary
    let preview: Element<'a, Message> = if let Some(report) = &ex.done {
        let (ok, fail) = (report.succeeded.len(), report.failed.len());
        let verb = if report.mode == Mode::Move { "moved" } else { "exported" };
        let tone = if fail > 0 { t::WARN } else { t::KEEP };
        let mut col = column![row![
            icons::view(if fail > 0 { Glyph::Warning } else { Glyph::Check }, 18.0, tone),
            text(format!("{} {verb}", plural(ok as u64, "file"))).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK),
            kit::secondary(if fail > 0 { format!("{fail} failed") } else { String::new() }),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)]
        .spacing(t::S1);
        for (op, err) in report.failed.iter().take(10) {
            col = col.push(text(format!("{} — {err}", op.filename)).size(t::T_SMALL).color(t::REJECT));
        }
        if fail > 10 {
            col = col.push(kit::caption(format!("and {} more", fail - 10)));
        }
        col.padding(t::S3).into()
    } else if let Some(plan) = &ex.plan {
        let groups = plan.grouped();
        if groups.is_empty() {
            container(kit::secondary("Nothing to export with these options.")).padding(t::S3).into()
        } else {
            let mut col = column![
                row![kit::caption("Destination").width(Length::Fill), kit::caption("Files").width(48).align_x(iced::alignment::Horizontal::Right), Space::new().width(150)]
                    .padding(Padding::from([t::S1, t::S3])),
                kit::hairline(),
            ]
            .spacing(2);
            for g in groups {
                let has_pruned = ex.include_pruned && g.pruned_count > 0;
                let type_color = if g.type_label == "RAW" { t::RAW } else { t::JPG };
                let sep = || text("/").size(t::T_BODY).color(t::INK4);
                let path = row![
                    text(g.year.to_string()).size(t::T_BODY).color(t::INK),
                    sep(),
                    text(g.month_key.clone()).size(t::T_BODY).color(t::INK2),
                    sep(),
                    text(g.day_key.clone()).size(t::T_BODY).color(t::INK2),
                    sep(),
                    text(g.type_label).size(t::T_BODY).font(paint::MEDIUM).color(type_color),
                ]
                .spacing(6)
                .width(Length::Fill);
                let flag = |glyph: Glyph, n: usize, tone: Color| -> Element<'a, Message> {
                    if n == 0 {
                        return Space::new().width(50).into();
                    }
                    row![icons::view(glyph, 14.0, tone), text(n.to_string()).size(t::T_SMALL).font(paint::MEDIUM).color(tone)]
                        .spacing(4)
                        .align_y(Alignment::Center)
                        .width(50)
                        .into()
                };
                let r = row![
                    path,
                    text(g.file_count.to_string()).size(t::T_BODY).color(t::INK).width(48).align_x(iced::alignment::Horizontal::Right),
                    Space::new().width(t::S3),
                    flag(Glyph::Warning, g.conflict_count, t::WARN),
                    flag(Glyph::Scissors, g.cropped_count, t::SIGNAL),
                    flag(Glyph::Prune, if has_pruned { g.pruned_count } else { 0 }, t::REJECT),
                ]
                .align_y(Alignment::Center)
                .height(30)
                .padding(Padding::from([0.0, t::S3]));
                col = col.push(r);
            }
            col.into()
        }
    } else {
        container(kit::secondary("Choose a library folder to see where the photos will go.")).padding(t::S3).into()
    };
    let preview_box = container(scrollable(preview).style(style::scroll).height(Length::Fill)).height(Length::Fill).width(Length::Fill).style(style::group());

    let mut col = column![
        header(if ex.mode == Mode::Move { "Move to library" } else { "Export to library" }, Some("Photos are filed by the day they were taken.".into())),
        paths,
        options,
        chips,
        preview_box
    ]
    .spacing(t::S4);

    // Conflicts
    if let (Some(plan), false) = (&ex.plan, locked) {
        let n = plan.conflict_count();
        if n > 0 {
            let seg = |label: &'static str, r: Resolution| kit::segment(label, ex.bulk == Some(r), None, d(Msg::ExBulk(r)));
            let bar = row![
                icons::view(Glyph::Warning, 16.0, t::WARN),
                text(format!("{} already {} at the destination", plural(n as u64, "file"), if n == 1 { "exists" } else { "exist" }))
                    .size(t::T_BODY)
                    .color(t::INK),
                Space::new().width(Length::Fill),
                kit::caption("For all:"),
                kit::segmented(vec![seg("Skip", Resolution::Skip), seg("Overwrite", Resolution::Overwrite), seg("Rename", Resolution::Rename)]),
            ]
            .spacing(t::S2)
            .align_y(Alignment::Center);
            col = col.push(container(bar).padding(Padding::from([t::S2, t::S3])).width(Length::Fill).style(|_| container::Style {
                background: Some(Background::Color(t::alpha(t::WARN, 0.08))),
                border: Border { color: t::alpha(t::WARN, 0.35), width: 1.0, radius: t::R_CONTROL.into() },
                ..Default::default()
            }));
        }
    }
    if let Some((done, total)) = ex.running {
        col = col.push(
            column![
                progress_bar(0.0..=total.max(1) as f32, done as f32).girth(6).style(style::progress),
                kit::caption(if done == 0 { "Starting…".to_string() } else { format!("{done} of {total} files") }),
            ]
            .spacing(t::S1),
        );
    }
    let go: Element<'a, Message> = if ex.done.is_some() {
        kit::btn("Open library folder", style::primary(), ex.library.as_ref().map(|_| d(Msg::ExOpenLibrary))).into()
    } else {
        let (label, msg) = match &ex.plan {
            Some(p) if p.total() > 0 => {
                let verb = if ex.mode == Mode::Move { "Move" } else { "Export" };
                (format!("{verb} {}", plural(p.total() as u64, "file")), (!running).then_some(Msg::ExGo))
            }
            _ => ("Export".into(), None),
        };
        kit::btn(label, style::primary(), msg.map(d)).into()
    };
    let cancel_label = if ex.done.is_some() { "Close" } else { "Cancel" };
    col = col.push(footer(vec![], vec![kit::btn(cancel_label, style::secondary(), (!running).then(|| d(Msg::Close))).into(), go]));
    frame(col.into(), 780f32.min(w), 760f32.min(h))
}

// ── edit copy view ──────────────────────────────────────────────────────────

fn edit_copy_view<'a>(app: &'a App, ec: &'a EditCopy, w: f32, h: f32) -> Element<'a, Message> {
    let running = ec.running.is_some();
    let locked = running || ec.done.is_some();
    let shoot = ec.working.as_ref().map(|p| util::file_name(p)).unwrap_or_else(|| "shoot".into());
    let dest = ec.dest_dir();
    let card = group_box(
        column![
            path_row(Glyph::Folder, "Working folder", ec.working.as_ref(), None),
            path_row(Glyph::Folder, "Copy to", dest.as_ref(), (!locked).then_some(Msg::EcChange)),
            row![
                checkbox(ec.raws)
                    .label("RAW files")
                    .size(16)
                    .text_size(t::T_BODY)
                    .spacing(t::S2)
                    .style(style::check(t::SIGNAL))
                    .on_toggle_maybe((!locked && (ec.jpgs || !ec.raws)).then_some(|v| d(Msg::EcRaws(v)))),
                checkbox(ec.jpgs)
                    .label("JPG files")
                    .size(16)
                    .text_size(t::T_BODY)
                    .spacing(t::S2)
                    .style(style::check(t::SIGNAL))
                    .on_toggle_maybe((!locked && (ec.raws || !ec.jpgs)).then_some(|v| d(Msg::EcJpgs(v)))),
            ]
            .spacing(t::S4)
            .padding(Padding { top: t::S1, bottom: t::S1, left: 0.0, right: 0.0 }),
            container(
                checkbox(ec.subfolder)
                    .label(format!("Put them in a subfolder named “{shoot}”"))
                    .size(16)
                    .text_size(t::T_BODY)
                    .spacing(t::S2)
                    .style(style::check(t::SIGNAL))
                    .on_toggle_maybe((ec.working.is_some() && !locked).then_some(|v| d(Msg::EcSubfolder(v)))),
            )
            .padding(Padding { top: t::S1, bottom: t::S1, left: 0.0, right: 0.0 }),
        ]
        .into(),
    );

    // What will happen (once copied: what did)
    let sel = &ec.sel;
    let noun = ec.noun();
    let marked = || sel.files.iter().filter_map(|&i| app.coll.get(i));
    let (n, bytes) = match &ec.plan {
        Some(plan) => (plan.to_copy_count(), plan.total_bytes()),
        None => (sel.files.len(), marked().map(|r| r.file_size).sum()),
    };
    let (renamed, cropped) = match (&ec.done, &ec.plan) {
        (Some(report), _) => (report.copied.iter().filter(|o| o.renamed()).count(), report.copied.iter().filter(|o| o.is_cropped()).count()),
        (None, Some(plan)) => (plan.renamed_count(), plan.cropped_count()),
        (None, None) => (0, marked().filter(|r| r.is_cropped() && r.file_type == FileType::Raw).count()),
    };
    let mut chips = row![].spacing(t::S2).align_y(Alignment::Center);
    chips = chips.push(if let Some(report) = &ec.done {
        let copied: u64 = report.copied.iter().map(|o| o.file_size).sum();
        chip(format!("{} copied, {}", plural(report.copied.len() as u64, noun), util::fmt_bytes(copied)), t::RAW)
    } else if n > 0 {
        chip(format!("{} to copy, {}", plural(n as u64, noun), util::fmt_bytes(bytes)), t::RAW)
    } else {
        chip("Nothing to copy".into(), t::INK2)
    });
    if let Some(k) = ec.plan.as_ref().map(|p| p.existing_count()).filter(|&k| k > 0) {
        chips = chips.push(chip(format!("{k} already in the folder"), t::INK2));
    }
    if renamed > 0 {
        chips = chips.push(kit::tip(
            chip(format!("{renamed} renamed"), t::WARN),
            "Other files in the folder have these names, so the copies get a number added. Nothing is overwritten.",
            false,
        ));
    }
    if cropped > 0 {
        chips = chips.push(kit::tip(
            chip(format!("✂ {cropped} cropped"), t::SIGNAL),
            "Cropped RAWs are copied untouched, with an .xmp sidecar holding the crop for Lightroom and Camera Raw.",
            false,
        ));
    }
    let (missing_kind, held_kind) = if ec.raws { ("RAW", "JPG") } else { ("JPG", "RAW") };
    if sel.matched_by_name > 0 {
        chips = chips.push(kit::tip(
            chip(format!("{} matched by name", sel.matched_by_name), t::INK2),
            "Marked files whose RAW or JPG was found by its name, in the same folder.",
            false,
        ));
    }
    if !sel.missing.is_empty() {
        chips = chips.push(chip(format!("⚠ {} with no {missing_kind}", plural(sel.missing.len() as u64, held_kind)), t::WARN));
    }
    if sel.pruned > 0 {
        chips = chips.push(kit::tip(
            chip(format!("{} pruned, skipped", sel.pruned), t::REJECT),
            "Shots that are both marked and pruned are left out.",
            false,
        ));
    }

    // The files, in groups: what will be (or was) copied, what's already
    // there, and marked JPGs with no RAW.
    let file_row = |name: String, cropped: bool, note: Option<(String, Color)>, size: u64, dim: bool| -> Element<'a, Message> {
        let mut r = row![text(name).size(t::T_BODY).color(if dim { t::INK3 } else { t::INK })].spacing(t::S2).align_y(Alignment::Center);
        if cropped {
            r = r.push(icons::view(Glyph::Scissors, 14.0, t::SIGNAL));
        }
        if let Some((note, tone)) = note {
            r = r.push(text(note).size(t::T_SMALL).color(tone));
        }
        r = r.push(Space::new().width(Length::Fill));
        r = r.push(
            text(util::fmt_size(size))
                .size(t::T_SMALL)
                .color(if dim { t::INK3 } else { t::INK2 })
                .width(70)
                .align_x(iced::alignment::Horizontal::Right),
        );
        container(r).padding(Padding::from([4.0, t::S3])).width(Length::Fill).into()
    };
    let group = |label: &str, n: usize| -> Element<'a, Message> {
        container(kit::heading(format!("{label} ({n})"))).padding(Padding { top: t::S2, bottom: t::S1, left: t::S3, right: t::S3 }).into()
    };
    let renamed_note = |op: &edit_copy::CopyOp| op.renamed().then(|| (format!("saved as {} (the name is taken)", util::file_name(&op.dest)), t::WARN));
    let mut list = column![].spacing(0);
    if let Some(report) = &ec.done {
        let tone = if !report.failed.is_empty() || report.cancelled { t::WARN } else { t::KEEP };
        let mut extra = Vec::new();
        if report.cancelled {
            extra.push("stopped early".to_string());
        }
        if !report.failed.is_empty() {
            extra.push(format!("{} failed", report.failed.len()));
        }
        list = list.push(
            container(
                row![
                    icons::view(if tone == t::KEEP { Glyph::Check } else { Glyph::Warning }, 18.0, tone),
                    text(format!("{} copied", plural(report.copied.len() as u64, noun))).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK),
                    kit::secondary(extra.join(", ")),
                ]
                .spacing(t::S2)
                .align_y(Alignment::Center),
            )
            .padding(t::S3),
        );
        if !report.failed.is_empty() {
            list = list.push(group("Couldn't copy", report.failed.len()));
            for (op, err) in report.failed.iter().take(50) {
                list = list.push(file_row(op.filename.clone(), false, Some((err.clone(), t::REJECT)), op.file_size, false));
            }
            if report.failed.len() > 50 {
                list = list.push(container(kit::caption(format!("and {} more", report.failed.len() - 50))).padding(Padding::from([4.0, t::S3])));
            }
        }
        if !report.copied.is_empty() {
            list = list.push(group("Copied", report.copied.len()));
            for op in &report.copied {
                list = list.push(file_row(op.filename.clone(), op.is_cropped(), renamed_note(op), op.file_size, false));
            }
        }
        // Stopped part-way: what the copy never reached.
        if report.cancelled {
            let reached: HashSet<RecordId> = report.copied.iter().chain(report.failed.iter().map(|(op, _)| op)).map(|op| op.id).collect();
            let left: Vec<_> = ec.plan.iter().flat_map(|p| p.to_copy()).filter(|op| !reached.contains(&op.id)).collect();
            if !left.is_empty() {
                list = list.push(group("Not copied", left.len()));
                for op in left {
                    list = list.push(file_row(op.filename.clone(), false, None, op.file_size, true));
                }
            }
        }
    } else {
        if sel.files.is_empty() {
            list = list.push(container(kit::secondary(format!("No {noun}s to copy. Mark shots with M in the grid or the viewer first."))).padding(t::S3));
        } else if let Some(plan) = &ec.plan {
            let (have, new): (Vec<_>, Vec<_>) = plan.ops.iter().partition(|op| op.exists);
            if !new.is_empty() {
                list = list.push(group("To copy", new.len()));
                for op in new {
                    list = list.push(file_row(op.filename.clone(), op.is_cropped(), renamed_note(op), op.file_size, false));
                }
            }
            if !have.is_empty() {
                list = list.push(group("Already in the folder", have.len()));
                for op in have {
                    list = list.push(file_row(op.filename.clone(), false, None, op.file_size, true));
                }
            }
        } else {
            let mut raws: Vec<_> = marked().collect();
            raws.sort_by(|a, b| a.shot_time().cmp(&b.shot_time()).then_with(|| a.name_lower.cmp(&b.name_lower)));
            list = list.push(group("To copy", raws.len()));
            for r in raws {
                list = list.push(file_row(r.filename(), r.is_cropped(), None, r.file_size, false));
            }
        }
        if !sel.missing.is_empty() {
            let mut jpgs: Vec<_> = sel.missing.iter().filter_map(|&i| app.coll.get(i)).collect();
            jpgs.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
            list = list.push(group(&format!("Marked, but no {missing_kind} to copy"), jpgs.len()));
            for r in jpgs {
                list = list.push(file_row(r.filename(), false, None, r.file_size, true));
            }
        }
    }

    let mut col = column![
        header(
            "Copy marked photos for editing",
            Some(format!("{} of each marked shot {} copied into one folder to edit from. The originals stay put.", ec.which(), if ec.raws && ec.jpgs { "are" } else { "is" })),
        ),
        card,
        chips,
        list_box(list.into()),
    ]
    .spacing(t::S4);
    if let Some((f, ft, b, bt)) = ec.running {
        let label = if ec.stopping {
            "Stopping after the current file…".to_string()
        } else if f == 0 {
            "Starting…".into()
        } else {
            format!("{f} of {ft} files, {} of {}", util::fmt_bytes(b), util::fmt_bytes(bt))
        };
        col = col.push(
            column![progress_bar(0.0..=1000.0, if bt > 0 { 1000.0 * b as f32 / bt as f32 } else { 1000.0 }).girth(6).style(style::progress), kit::caption(label)]
                .spacing(t::S1),
        );
    }
    let go = ec.go();
    let go_label = match go {
        _ if running => "Copying…".to_string(),
        EcGoAction::OpenFolder => "Open folder".into(),
        EcGoAction::ChooseFolder => "Choose folder…".into(),
        EcGoAction::Copy => format!("Copy {}", plural(n as u64, noun)),
        EcGoAction::None => "Copy".into(),
    };
    // Nothing left to do: there is nothing to cancel either.
    let cancel_label = if running {
        "Stop"
    } else if ec.done.is_some() || go == EcGoAction::OpenFolder {
        "Close"
    } else {
        "Cancel"
    };
    col = col.push(footer(
        vec![],
        vec![
            kit::btn(cancel_label, style::secondary(), (!ec.stopping).then(|| d(if running { Msg::EcStop } else { Msg::Close }))).into(),
            kit::btn(go_label, style::primary(), (go != EcGoAction::None).then(|| d(Msg::EcGo))).into(),
        ],
    ));
    frame(col.into(), 700f32.min(w), 640f32.min(h))
}

// ── sort / prune views ──────────────────────────────────────────────────────

fn sort_view<'a>(_app: &'a App, so: &'a Sort, w: f32, h: f32) -> Element<'a, Message> {
    let plan = &so.plan;
    let n_move = plan.ops.iter().filter(|o| !o.in_place()).count();
    let n_skip = plan.already_in_place_count();
    // RAW+JPG pairs, listed together.
    let mut groups: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
    for (i, op) in plan.ops.iter().enumerate() {
        groups.entry((canonical_parent(&op.src), util::stem(&op.src).to_lowercase())).or_default().push(i);
    }
    let mut keys: Vec<_> = groups.keys().cloned().collect();
    keys.sort_by(|a, b| a.1.cmp(&b.1));
    let mut shown: Vec<Vec<usize>> = Vec::new();
    let mut singles: Vec<usize> = Vec::new();
    let mut n_pairs = 0;
    for k in keys {
        let mut g = groups.remove(&k).unwrap_or_default();
        let types: HashSet<FileType> = g.iter().map(|&i| plan.ops[i].file_type).collect();
        if types.contains(&FileType::Raw) && types.contains(&FileType::Jpg) {
            n_pairs += 1;
            g.sort_by_key(|&i| if plan.ops[i].file_type == FileType::Raw { 0 } else { 1 });
            shown.push(g);
        } else {
            singles.extend(g);
        }
    }
    singles.sort_by_cached_key(|&i| plan.ops[i].filename.to_lowercase());
    shown.extend(singles.into_iter().map(|i| vec![i]));

    let mut list = column![];
    for (gi, g) in shown.iter().enumerate() {
        let is_pair = g.len() == 2;
        for &i in g {
            let op = &plan.ops[i];
            let dest = format!("{}/{}", util::parent_name(&op.final_dest), util::file_name(&op.final_dest));
            let (glyph, tone, status, dim) = if op.in_place() {
                (Glyph::Check, t::INK3, "Already sorted".to_string(), true)
            } else if op.skipped {
                (Glyph::Skip, t::INK3, "Skipped".to_string(), true)
            } else if op.conflict {
                (Glyph::Warning, t::WARN, "Renamed".to_string(), false)
            } else if is_pair {
                (Glyph::Swap, t::INK3, "Pair".to_string(), false)
            } else {
                (Glyph::ChevronRight, t::INK3, String::new(), false)
            };
            let r = row![
                container(icons::view(glyph, 14.0, tone)).width(22),
                text(op.filename.clone()).size(t::T_BODY).color(if dim { t::INK3 } else { t::INK }).width(Length::FillPortion(5)),
                text(if op.in_place() { String::new() } else { dest }).size(t::T_BODY).color(t::INK2).width(Length::FillPortion(5)),
                text(status).size(t::T_SMALL).color(tone).width(Length::FillPortion(2)),
            ]
            .align_y(Alignment::Center)
            .padding(Padding::from([4.0, t::S2]));
            // Pairs sit on alternating bands so the two halves read together.
            let band = is_pair && gi % 2 == 0;
            list = list.push(container(r).width(Length::Fill).style(move |_| container::Style {
                background: band.then(|| Background::Color(t::alpha(Color::WHITE, 0.03))),
                ..Default::default()
            }));
        }
    }

    let mut chips = row![].spacing(t::S2).align_y(Alignment::Center);
    if n_skip > 0 {
        chips = chips.push(chip(format!("{n_skip} already sorted"), t::INK2));
    }
    if n_pairs > 0 {
        chips = chips.push(chip(format!("{} stay paired", plural(n_pairs as u64, "RAW+JPG pair")), t::INK2));
    }
    let sub = if n_move > 0 {
        format!("{} will move into RAW and JPG subfolders of this folder.", plural(n_move as u64, "file"))
    } else {
        "Every file is already in its RAW or JPG subfolder.".to_string()
    };
    let mut col = column![header("Sort into RAW and JPG folders", Some(sub)), chips].spacing(t::S4);
    let n_conf = plan.conflict_count();
    if n_conf > 0 {
        col = col.push(
            column![
                row![
                    icons::view(Glyph::Warning, 16.0, t::WARN),
                    text(format!("{} already {} in the subfolder", plural(n_conf as u64, "file"), if n_conf == 1 { "exists" } else { "exist" }))
                        .size(t::T_BODY)
                        .color(t::INK)
                ]
                .spacing(t::S2)
                .align_y(Alignment::Center),
                row![
                    radio("Rename the moved file (IMG_001_1.cr3)", Strategy::Rename, Some(plan.strategy), |st| d(Msg::SortStrategy(st)))
                        .size(16)
                        .text_size(t::T_BODY)
                        .style(style::radio_style),
                    radio("Leave it where it is", Strategy::Skip, Some(plan.strategy), |st| d(Msg::SortStrategy(st)))
                        .size(16)
                        .text_size(t::T_BODY)
                        .style(style::radio_style),
                ]
                .spacing(t::S5),
            ]
            .spacing(t::S2),
        );
    }
    col = col.push(
        column![
            row![
                Space::new().width(22),
                kit::caption("File").width(Length::FillPortion(5)),
                kit::caption("Moves to").width(Length::FillPortion(5)),
                Space::new().width(Length::FillPortion(2)),
            ]
            .padding(Padding::from([0.0, t::S3])),
            list_box(list.into()),
        ]
        .spacing(t::S1)
        .height(Length::Fill),
    );
    let can_go = plan.move_count() > 0 && !so.running;
    col = col.push(footer(
        vec![],
        vec![
            kit::btn("Cancel", style::secondary(), (!so.running).then(|| d(Msg::Close))).into(),
            kit::btn(if so.running { "Sorting…".to_string() } else { format!("Sort {}", plural(n_move as u64, "file")) }, style::primary(), can_go.then(|| d(Msg::SortGo))).into(),
        ],
    ));
    frame(col.into(), 760f32.min(w), 600f32.min(h))
}

fn prune_view<'a>(app: &'a App, pr: &'a Prune, w: f32, h: f32) -> Element<'a, Message> {
    let recs: Vec<_> = pr.ids.iter().filter_map(|&i| app.coll.get(i)).collect();
    let total: u64 = recs.iter().map(|r| r.file_size).sum();
    let n = recs.len();
    let mut list = column![];
    for (label, ft) in [("RAW", FileType::Raw), ("JPG", FileType::Jpg)] {
        let mut group: Vec<_> = recs.iter().filter(|r| r.file_type == ft).collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
        list = list.push(container(kit::heading(format!("{label} files ({})", group.len()))).padding(Padding { top: t::S2, bottom: t::S1, left: t::S3, right: t::S3 }));
        for r in group {
            let parent = r.path.parent().map(|p| util::elide_path(p, 40)).unwrap_or_default();
            list = list.push(
                container(
                    row![
                        text(r.filename()).size(t::T_BODY).color(t::INK).width(Length::Fill),
                        kit::caption(parent),
                        text(util::fmt_size(r.file_size)).size(t::T_SMALL).color(t::INK2).width(70).align_x(iced::alignment::Horizontal::Right),
                    ]
                    .spacing(t::S3)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([4.0, t::S3]))
                .width(Length::Fill),
            );
        }
    }
    let mut col = column![
        header("Review pruned files", Some(format!("{} marked to prune, {} in all.", plural(n as u64, "file"), util::fmt_size(total)))),
        list_box(list.into()),
        row![icons::view(Glyph::Info, 16.0, t::INK3), kit::secondary("They go to the system Trash, where you can restore them.")].spacing(t::S2).align_y(Alignment::Center),
    ]
    .spacing(t::S4);
    if pr.remote {
        col = col.push(
            row![
                icons::view(Glyph::Warning, 16.0, t::REJECT),
                text("Some files are on a network or removable drive, which may have no Trash. Any that can't be trashed are left in place and listed. Nothing is deleted outright.")
                    .size(t::T_BODY)
                    .color(t::INK),
            ]
            .spacing(t::S2)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(footer(
        vec![kit::btn("Unmark all", style::ghost(), (!pr.running).then(|| d(Msg::PruneUnmark))).into()],
        vec![
            kit::btn("Close", style::secondary(), (!pr.running).then(|| d(Msg::Close))).into(),
            kit::btn(if pr.running { "Moving…".to_string() } else { format!("Move {} to Trash", plural(n as u64, "file")) }, style::danger(), (!pr.running).then(|| d(Msg::PruneTrash))).into(),
        ],
    ));
    frame(col.into(), 640f32.min(w), 520f32.min(h))
}

// ── keyboard shortcuts window ───────────────────────────────────────────────

const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Main window",
        &[
            ("Ctrl+O", "Open a working folder"),
            ("Ctrl+R", "Review pruned files"),
            ("Ctrl+E", "Export to the library"),
            ("Ctrl+Shift+E", "Copy marked photos to a folder for editing"),
            ("Ctrl+Shift+S", "Sort into RAW and JPG folders"),
            ("Ctrl+2", "Grid view"),
            ("Ctrl+3", "Library calendar"),
            ("Ctrl+F", "Search the library"),
            ("Ctrl+Z", "Undo marks, crops, Sort or a move to the Trash"),
            ("Ctrl+Shift+Z  or  Ctrl+Y", "Redo"),
            ("Ctrl+\\", "Show or hide the menu labels"),
            ("Ctrl+B", "Show or hide the folder bar"),
            ("Ctrl+U", "Show only photos to review, or all"),
            ("Ctrl+Shift+A", "Advance after marking, on or off"),
            ("Ctrl+,", "Preferences"),
            ("S", "Select mode"),
            ("P  or  Delete", "Mark the selection to prune"),
            ("M", "Mark the selection for export"),
            ("C", "Compare the 2 to 4 selected photos"),
            ("B", "Every frame of the selected photo's burst, in the viewer"),
            ("G  or  click its tab", "Stack the selected photo's burst, or show all its frames"),
            ("Shift+G", "Stack all bursts, or show every frame"),
            ("?", "This list"),
        ],
    ),
    (
        "Search",
        &[
            ("Ctrl+F", "Go to the search field, its text selected"),
            ("↑  ↓  then  Enter  or  Tab", "Take a recent search"),
            ("Enter", "Search, and move to the results"),
            ("Esc", "Close the list of recent searches (the page stays)"),
            ("←  →", "Previous and next result"),
            ("Space", "Quick look at the selected result"),
            ("Enter  or  double-click", "Open it in the viewer (read-only)"),
            ("G  /  Shift+G", "Show a burst's frames, or stack it again; every burst"),
        ],
    ),
    (
        "People and fursuits",
        &[
            ("Ctrl+4", "People and fursuits"),
            ("←  →", "Previous and next group"),
            ("Enter  /  F2", "Open the group; name it"),
            ("S  or  Ctrl+click", "Pick several groups"),
            ("M", "Merge the groups picked (asks first)"),
            ("Drag a group", "Onto another to merge the two (asks first)"),
            ("Shift+click", "On a fursuit's Heads: pick every head from the last one clicked"),
            ("Y", "On a group's page: review its suggestions"),
            ("Y  /  N", "In the review: the same, or not"),
            ("X", "In the review: not a fursuit head (or not a face) at all"),
            ("X  then  1  /  2", "Same fursuit? for two groups: that side's big head isn't a fursuit head"),
            ("Z", "In a review: take the last answer back"),
            ("Ctrl+Z", "Take back the last change to the groups"),
        ],
    ),
    (
        "Quick look (grid)",
        &[
            ("Space", "See the selected photo large; again to close"),
            ("←  →  ↑  ↓  or  A  D", "Previous and next photo"),
            ("Home  /  End", "First and last photo"),
            ("PgUp  /  PgDn", "Previous and next day"),
            ("P  or  Delete", "Mark to prune"),
            ("M", "Mark for export"),
            ("U", "Unmark a pruned photo"),
            ("B", "Every frame of its burst, in the viewer"),
            ("Enter  or  double-click", "Open it in the viewer"),
            ("Esc  or  click outside", "Close"),
        ],
    ),
    (
        "Viewer",
        &[
            ("←  →  or  A  D", "Previous and next photo"),
            ("Shift+←  →  or  Shift+A  D", "Skip a burst: back to its first frame, or on to the next one"),
            ("Home  /  End", "First and last photo"),
            ("P  or  Delete", "Mark to prune"),
            ("M", "Mark for export"),
            ("U", "Unmark a pruned photo"),
            ("Ctrl+Z  /  Ctrl+Shift+Z", "Undo and redo"),
            ("Ctrl+Shift+A", "Advance after marking, on or off"),
            ("T", "Show or hide the filmstrip"),
            ("B", "Every frame of the photo's burst"),
            ("PgUp  /  PgDn", "Before or after the burst (else the previous or next photo)"),
            ("F11", "Full screen"),
            ("C", "Crop"),
            ("F  or  Space", "Fit to the window"),
            ("1", "Actual size"),
            ("+  and  −", "Zoom in and out"),
            ("Scroll", "Zoom"),
            ("Double-click", "Switch between fit and actual size"),
            ("Esc", "Leave full screen, or close the viewer"),
            ("L", "Label the faces and fursuit heads in the photo"),
        ],
    ),
    (
        "Labels in the viewer",
        &[
            ("L  or  Esc", "Show or hide the labels"),
            ("Click a box", "Name it, change it to the other kind, or remove it"),
            ("Delete  or  Backspace", "Remove the picked box (not a face, not a fursuit head)"),
            ("×  on the picked box", "Remove it"),
            ("×  on a name", "Back to Unnamed; the box stays"),
            ("Drag on the photo", "Add a face or fursuit head that was missed"),
            ("Enter", "Give the name typed"),
            ("Esc", "Done with the box (or the box drawn)"),
            ("Ctrl+Z", "Take back the last change to the labels"),
        ],
    ),
    (
        "A burst in the viewer",
        &[
            ("←  →  ↑  ↓", "Move the outline over the frames"),
            ("Home  /  End", "First and last frame"),
            ("Click  or  Enter  or  +", "Zoom into the frame"),
            ("Esc  or  −  or  B", "From a frame, back out to all of them"),
            ("←  →  on a frame", "The burst's previous and next frame"),
            ("PgUp  /  PgDn", "Before or after the burst"),
            ("Shift+←  /  Shift+→", "This burst's first frame or the burst before; the next burst"),
            ("P  /  M  /  U", "Mark the outlined frame"),
            ("Ctrl+click  or  Space", "Pick frames to compare"),
            ("C", "Compare the picked frames, or the outlined one and its neighbours"),
            ("Esc", "On all the frames: close the viewer"),
        ],
    ),
    (
        "Library calendar",
        &[
            ("←  →  or  PgUp  PgDn", "Previous and next month or year"),
            ("Scroll or swipe", "Previous and next month or year"),
            ("Ctrl+scroll or pinch", "Zoom between year, month and day"),
            ("−  or  Esc", "Zoom out"),
            ("+  or  Enter", "Zoom in, or open the hovered day"),
            ("T  or  Home", "Today"),
            ("[  and  ]", "Previous and next day with photos"),
            ("Esc  or  Backspace", "From a day back to its month"),
            ("F5", "Re-read the library folder"),
        ],
    ),
    (
        "Crop",
        &[
            ("Enter  or  C", "Apply the crop"),
            ("Esc", "Discard the change"),
            ("R", "Back to the whole frame"),
            ("Drag inside", "Move the crop"),
            ("Drag an edge", "Resize, keeping the ratio if one is set"),
            ("Double-click", "Back to the whole frame"),
        ],
    ),
    (
        "Compare",
        &[
            ("←  →  ↑  ↓  or  Tab", "Move between the photos"),
            ("P  or  Delete", "Mark the outlined photo to prune"),
            ("M", "Mark the outlined photo for export"),
            ("U", "Unmark a pruned photo"),
            ("F  or  Space", "Fit every photo"),
            ("1", "Every photo at actual size"),
            ("+  and  −", "Zoom in and out"),
            ("Scroll  or  drag", "Zoom and pan, all photos together"),
            ("Double-click", "Switch between fit and actual size"),
            ("Enter", "Open the outlined photo in the viewer"),
            ("Esc", "Leave compare"),
        ],
    ),
    (
        "Comparing frames of a burst (as Compare, and)",
        &[
            (",  .  or  Shift+←  →", "The outlined photo shows the previous or next frame"),
            ("PgUp  /  PgDn", "The previous or next frames"),
            ("Home  /  End", "The first or last frames"),
            ("1", "Every photo at 100 % on its own focus point"),
            ("Esc", "Back to all the frames"),
        ],
    ),
];

pub fn shortcuts_view<'a>() -> Element<'a, Message> {
    let mut col = column![kit::title("Keyboard shortcuts")].spacing(2);
    for (section, rows) in SHORTCUTS {
        col = col.push(Space::new().height(t::S4));
        col = col.push(kit::heading(*section));
        col = col.push(Space::new().height(t::S1));
        for (key, desc) in rows.iter() {
            col = col.push(
                row![
                    container(
                        container(text(*key).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK))
                            .padding(Padding::from([2.0, 7.0]))
                            .style(|_| container::Style {
                                background: Some(Background::Color(t::RAISED)),
                                border: Border { color: t::LINE_HI, width: 1.0, radius: 4.0.into() },
                                ..Default::default()
                            })
                    )
                    .width(170),
                    kit::secondary(*desc),
                ]
                .spacing(t::S3)
                .align_y(Alignment::Center)
                .padding(Padding::from([3.0, 0.0])),
            );
        }
    }
    col = col.push(Space::new().height(t::S4));
    col = col.push(footer(vec![], vec![kit::btn("Close", style::secondary(), Some(Message::ShortcutsClose)).into()]));
    container(scrollable(container(col).padding(Padding::from([t::S5, t::S5]))).style(style::scroll))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::MOUNT))
        .into()
}
