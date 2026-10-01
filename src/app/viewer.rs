//! The image viewer window (Python `ImageViewer`).
//!
//! A separate, non-modal window.  Navigation slides between photos; the
//! crop tool, prune / keep marks and the pair toggle work on the record in
//! view.  Library photos (from the calendar) open read-only.  On a burst
//! the viewer shows the burst's sheet first (see `viewer_burst`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Space, button, column, container, image as iimage, row, scrollable, stack, text, text_input};
use iced::{Alignment, Element, Length, Padding, Size, Task, window};

use super::kit;
use super::style;
use super::viewer_advance::{self, Mark};
use super::viewer_burst::{self, Land, SheetUi};
use super::viewer_filmstrip;
use super::viewer_window::{self, WinMsg};
use super::{App, Message};
use crate::bus::BgEvent;
use crate::exif_panel::{self, Sections};
use crate::imaging::loader::FullImage;
use crate::model::bursts::BurstId;
use crate::model::collection::{Collection, RecordId};
use crate::model::crop::{self, AspectRatio, Crop, FREEFORM, ORIGINAL};
use crate::model::crop_math;
use crate::model::record::{FileType, PhotoRecord};
use crate::ui::filmstrip::FilmEvent;
use crate::ui::icons::{self, Glyph};
use crate::ui::image_stage::{CropProps, ImageStage, StageEvent, Transition, ViewCmd};
use crate::ui::paint;
use crate::ui::theme as t;
use crate::util;

#[derive(Clone)]
pub enum Source {
    Working,
    Library(Arc<Collection>),
}

pub struct Viewer {
    pub id: window::Id,
    pub read_only: bool,
    pub source: Source,
    pub items: Vec<RecordId>,
    pub index: usize,
    pub pair: Option<RecordId>,
    pub show_pair: bool,
    key: u64,
    /// When the photo on screen was asked for (profiling).
    shown_at: std::time::Instant,
    transition: Transition,
    cmd: (u64, ViewCmd),
    pub(super) zoom: f32,
    viewport: Size,
    info: bool,
    exif_cache: HashMap<(PathBuf, Option<PathBuf>), Sections>,
    exif_pending: Option<(PathBuf, Option<PathBuf>)>,
    et_pending: Option<PathBuf>,
    status: Option<String>,
    pub(super) image: Option<Arc<FullImage>>,
    pub(super) crop_mode: bool,
    crop_pending: bool,
    crop: Crop,
    ratio_sel: AspectRatio,
    portrait: bool,
    spin_w: String,
    spin_h: String,
    /// Advance after marking: a move waiting out its beat, and the stamp.
    pub(super) flow: viewer_advance::Flow,
    /// Full screen, width, placement (see `viewer_window`).
    pub(super) win: viewer_window::WinState,
    /// Each item's burst (see `viewer_burst`).
    pub(super) burst_of: Vec<Option<BurstId>>,
    /// The burst sheet: on screen or not, the burst open, frames picked.
    pub(super) sheet: SheetUi,
}

/// The status while a photo decodes.
const LOADING: &str = "Loading…";

#[derive(Debug, Clone)]
pub enum Msg {
    Prev,
    Next,
    TogglePair,
    Fit,
    Actual,
    ZoomIn,
    ZoomOut,
    TogglePrune,
    ToggleMark,
    ShowInFolder,
    ToggleInfo,
    ToggleCrop,
    CropApply,
    CropCancel,
    CropReset,
    Aspect(AspectRatio),
    Flip,
    SpinW(String),
    SpinH(String),
    CustomSet,
    Stage(StageEvent),
    Allocated(PathBuf, Result<iimage::Allocation, iimage::Error>),
    /// The beat after a mark is over (advance after marking).
    Advance(u64),
    ToggleAdvance,
    ToggleFilmstrip,
    ToggleFullscreen,
    Film(FilmEvent),
    Window(WinMsg),
    /// "All frames": back out to the burst's sheet.
    ToSheet,
    /// The sheet's chevrons: out of the burst, backwards or forwards.
    Leave(i32),
    /// The sheet's Compare button.
    CompareFrames,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Viewer")
    }
}

fn coll<'a>(app: &'a App, v: &'a Viewer) -> &'a Collection {
    match &v.source {
        Source::Working => &app.coll,
        Source::Library(c) => c,
    }
}

impl Viewer {
    pub fn record<'a>(&'a self, app: &'a App) -> Option<&'a PhotoRecord> {
        coll(app, self).get(*self.items.get(self.index)?)
    }

    pub(super) fn record_id(&self) -> Option<RecordId> {
        self.items.get(self.index).copied()
    }

    /// The photo area's size (a guess until the stage reports it).
    pub(super) fn stage_size(&self) -> Size {
        if self.viewport.width > 0.0 && self.viewport.height > 0.0 { self.viewport } else { Size::new(1100.0, 650.0) }
    }

    /// The record on screen — the pair file in pair view.
    pub fn display_id(&self) -> Option<RecordId> {
        if self.show_pair { self.pair.or(self.record_id()) } else { self.record_id() }
    }

    pub fn title(&self, app: &App) -> String {
        match self.display_id().and_then(|id| coll(app, self).get(id)) {
            Some(r) => r.filename(),
            None => "Viewer".into(),
        }
    }
}

// ── opening / navigation ────────────────────────────────────────────────────

/// Open the viewer on `items[index]`: on a burst's frame, its sheet (with
/// "Show a burst's frames first" on).
pub fn open(app: &mut App, source: Source, items: Vec<RecordId>, index: usize, read_only: bool) -> Task<Message> {
    open_as(app, source, items, index, read_only, Land::Auto)
}

/// Open the viewer, landing as `land` says on a burst's frame.
pub fn open_as(app: &mut App, source: Source, items: Vec<RecordId>, index: usize, read_only: bool, land: Land) -> Task<Message> {
    crate::profile::mark("open to first frame");
    if items.is_empty() {
        return Task::none();
    }
    let burst_of = viewer_burst::burst_of(&app.bursts, &source, &items);
    let pref = app.settings.viewer_burst_sheet;
    // A compare showing in the viewer's window hands it back first.
    super::compare::release(app);
    let index = index.min(items.len() - 1);
    let mut tasks = Vec::new();
    // A culling viewer and a library viewer differ in their controls: swap.
    if app.viewer.as_ref().is_some_and(|v| v.read_only != read_only) {
        tasks.push(close(app));
    }
    if let Some(v) = app.viewer.as_mut() {
        exit_crop(v, false);
        v.source = source;
        v.items = items;
        v.index = index;
        v.burst_of = burst_of;
        // Opening counts as arriving from outside any burst.
        viewer_burst::arrive(v, index, land, None, pref);
        let id = v.id;
        tasks.push(load_current(app, Transition::Cut));
        tasks.push(window::gain_focus(id));
        return Task::batch(tasks);
    }
    // As the window was last left (size, maximised, and position where
    // the platform reports one).
    let settings = viewer_window::open_settings(&app.settings, app.monitor);
    let win = viewer_window::WinState::new(&settings);
    let (id, open) = window::open(settings);
    app.viewer = Some(Viewer {
        id,
        read_only,
        source,
        items,
        index,
        pair: None,
        show_pair: false,
        key: 0,
        shown_at: std::time::Instant::now(),
        transition: Transition::Cut,
        cmd: (0, ViewCmd::None),
        zoom: 1.0,
        viewport: Size::ZERO,
        info: true,
        exif_cache: HashMap::new(),
        exif_pending: None,
        et_pending: None,
        status: None,
        image: None,
        crop_mode: false,
        crop_pending: false,
        crop: Crop::full(None),
        ratio_sel: AspectRatio::named(FREEFORM),
        portrait: false,
        spin_w: "3".into(),
        spin_h: "2".into(),
        flow: viewer_advance::Flow::default(),
        win,
        burst_of,
        sheet: SheetUi::default(),
    });
    if let Some(v) = app.viewer.as_mut() {
        viewer_burst::arrive(v, index, land, None, pref);
    }
    tasks.push(open.map(Message::WindowOpened));
    tasks.push(load_current(app, Transition::Cut));
    Task::batch(tasks)
}

fn display_paths(app: &App, v: &Viewer) -> Option<(PathBuf, FileType)> {
    let r = coll(app, v).get(v.display_id()?)?;
    Some((r.path.clone(), r.file_type))
}

fn load_current(app: &mut App, transition: Transition) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if v.record_id().is_none() {
        return Task::none();
    }
    v.key += 1;
    v.shown_at = std::time::Instant::now();
    v.transition = transition;
    show_record(app)
}

/// Show the record at the viewer's index (its image, pair and details)
/// without a transition of its own: the burst sheet moves its outline so.
pub(super) fn show_record(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    exit_crop(v, false);
    v.crop_pending = false;
    v.show_pair = false;
    v.flow.cancel();
    v.shown_at = std::time::Instant::now();
    let Some(id) = v.record_id() else { return Task::none() };
    let c = match &v.source {
        Source::Working => &app.coll,
        Source::Library(c) => c.as_ref(),
    };
    v.pair = c.find_pair(id);
    let (path, ft) = match c.get(id) {
        Some(r) => (r.path.clone(), r.file_type),
        None => return Task::none(),
    };
    show_path(app, &path, ft);
    preload_adjacent(app);
    populate_exif(app);
    viewer_burst::want_previews(app);
    allocate_current(app)
}

fn show_path(app: &mut App, path: &std::path::Path, ft: FileType) {
    // The full image, else its embedded preview while the full one decodes.
    let cached = app.loader.get(path);
    if !app.loader.contains(path) {
        app.loader.request(path, ft);
    }
    let Some(v) = app.viewer.as_mut() else { return };
    match cached {
        Some(img) => {
            crate::profile::record(("viewer", "already loaded"), v.shown_at.elapsed());
            v.image = Some(img);
            v.status = None;
        }
        None => {
            v.image = None;
            v.status = Some(LOADING.into());
        }
    }
}

fn preload_adjacent(app: &mut App) {
    let Some(v) = app.viewer.as_ref() else { return };
    let c = coll(app, v);
    let items: Vec<(PathBuf, FileType)> = v
        .items
        .iter()
        .map(|&id| c.get(id).map(|r| (r.path.clone(), r.file_type)).unwrap_or((PathBuf::new(), FileType::Jpg)))
        .collect();
    let index = v.index;
    app.loader.clear_queue();
    // Keep the current image's load if it is still in flight.
    if let Some((p, t)) = items.get(index) {
        if !app.loader.contains(p) {
            app.loader.request(p, *t);
        }
    }
    app.loader.preload_range(&items, index);
}

pub(super) fn navigate(app: &mut App, to: usize, dir: i32) -> Task<Message> {
    let pref = app.settings.viewer_burst_sheet;
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if v.crop_mode || to >= v.items.len() || to == v.index {
        return Task::none();
    }
    let from = viewer_burst::run_key(v);
    viewer_burst::arrive(v, to, Land::Auto, from, pref);
    v.index = to;
    load_current(app, Transition::Slide(dir))
}

/// Back from the compare view that borrowed the window: as it was, the
/// sheet's outline on the frame compare had in focus.
pub fn resume(app: &mut App, focus: Option<RecordId>) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if let (Some(run), Some(id)) = (viewer_burst::run(v).filter(|_| v.sheet.on), focus) {
        if let Some(i) = run.clone().find(|&i| v.items[i] == id) {
            v.index = i;
        }
    }
    load_current(app, Transition::Cut)
}

pub fn close(app: &mut App) -> Task<Message> {
    let Some(mut v) = app.viewer.take() else { return Task::none() };
    // Closing with the crop tool open commits the crop.
    let changed = exit_crop(&mut v, true);
    let id = v.id;
    let mut tasks = vec![window::close(id)];
    if let Some(rid) = changed {
        if let (Source::Working, Some(crop)) = (&v.source, Some(v.crop.clone())) {
            let value = if crop.is_full() { None } else { Some(crop) };
            if let Some(r) = app.coll.get_mut(rid) {
                r.crop = value;
            }
            tasks.push(app.refresh_crop(&[rid]));
        }
    }
    app.loader.clear_queue();
    viewer_window::remember(app);
    Task::batch(tasks)
}

// ── crop ────────────────────────────────────────────────────────────────────

fn resolve_ratio(v: &Viewer, ratio: &AspectRatio) -> Option<f64> {
    let img = v.image.as_ref()?;
    let pixel = if ratio.label == ORIGINAL {
        Some(img.width as f64 / img.height.max(1) as f64)
    } else {
        ratio.value()
    };
    crop_math::normalised_ratio(pixel, img.width, img.height)
}

fn enter_crop(app: &mut App) {
    let Some(v) = app.viewer.as_mut() else { return };
    if v.crop_mode || v.read_only {
        return;
    }
    if v.image.is_none() {
        // Still decoding: arm the tool, it opens when the pixels arrive.
        v.crop_pending = true;
        return;
    }
    let existing = v
        .display_id()
        .and_then(|id| match &v.source {
            Source::Working => app.coll.get(id),
            Source::Library(c) => c.get(id),
        })
        .and_then(|r| r.crop.clone())
        .unwrap_or_else(|| Crop::full(None));
    v.crop_pending = false;
    v.crop_mode = true;
    // Restore the ratio this crop was made with, without reshaping it.
    let ratio = existing.aspect_label.as_deref().and_then(crop::ratio_from_label).unwrap_or(AspectRatio::named(FREEFORM));
    select_aspect(v, ratio);
    v.crop = existing;
}

fn select_aspect(v: &mut Viewer, ratio: AspectRatio) {
    if let (Some(w), Some(h)) = (ratio.w, ratio.h) {
        if !ratio.is_free() {
            v.portrait = h > w;
            v.spin_w = (w.round() as i64).to_string();
            v.spin_h = (h.round() as i64).to_string();
        }
    }
    v.ratio_sel = ratio;
}

/// Leave crop mode.  Returns the record whose crop should be applied.
fn exit_crop(v: &mut Viewer, apply: bool) -> Option<RecordId> {
    v.crop_pending = false;
    if !v.crop_mode {
        return None;
    }
    v.crop_mode = false;
    if apply { v.display_id() } else { None }
}

fn apply_crop_exit(app: &mut App, apply: bool) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let target = exit_crop(v, apply);
    let crop = v.crop.clone();
    let working = matches!(v.source, Source::Working);
    match target {
        Some(rid) if working => {
            if let Some(r) = app.coll.get_mut(rid) {
                r.crop = if crop.is_full() { None } else { Some(crop) };
            }
            app.refresh_crop(&[rid])
        }
        _ => Task::none(),
    }
}

fn oriented(v: &Viewer, preset: &AspectRatio) -> AspectRatio {
    if preset.is_free() || !v.portrait { preset.clone() } else { preset.flipped() }
}

// ── EXIF ────────────────────────────────────────────────────────────────────

fn populate_exif(app: &mut App) {
    let bus = app.bus();
    let Some(v) = app.viewer.as_ref() else { return };
    let c = coll(app, v);
    let Some(rid) = v.record_id() else { return };
    let Some(rec) = c.get(rid) else { return };
    let jpg_pair = v.pair.and_then(|p| c.get(p)).filter(|p| p.file_type == FileType::Jpg).map(|p| p.path.clone());
    let key = (rec.path.clone(), jpg_pair.clone());
    let (path, ft) = (rec.path.clone(), rec.file_type);
    let cached = v.exif_cache.get(&key).cloned();
    let v = app.viewer.as_mut().expect("viewer");
    match cached {
        Some(sections) => maybe_exiftool(v, &path, ft, &sections, bus),
        None => {
            v.exif_pending = Some(key.clone());
            std::thread::spawn(move || {
                let sections = exif_panel::read_sections(&key.0, ft, key.1.as_deref());
                let _ = bus.unbounded_send(BgEvent::Exif { key, sections });
            });
        }
    }
}

fn maybe_exiftool(v: &mut Viewer, path: &std::path::Path, ft: FileType, sections: &Sections, bus: crate::bus::Bus) {
    if ft == FileType::Raw || !exif_panel::has_lens(sections) {
        v.et_pending = Some(path.to_path_buf());
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let flat = exif_panel::run_exiftool(&path);
            let _ = bus.unbounded_send(BgEvent::ExifTool { path, flat });
        });
    }
}

pub fn on_exif(app: &mut App, key: (PathBuf, Option<PathBuf>), sections: Sections) {
    let bus = app.bus();
    let Some(v) = app.viewer.as_mut() else { return };
    v.exif_cache.insert(key.clone(), sections.clone());
    if v.exif_pending.as_ref() != Some(&key) {
        return;
    }
    v.exif_pending = None;
    let ft = if FileType::from_path(&key.0) == Some(FileType::Raw) { FileType::Raw } else { FileType::Jpg };
    maybe_exiftool(v, &key.0, ft, &sections, bus);
}

pub fn on_exiftool(app: &mut App, path: PathBuf, flat: Vec<(String, String)>) {
    let Some(v) = app.viewer.as_mut() else { return };
    if v.et_pending.as_ref() != Some(&path) || flat.is_empty() {
        return;
    }
    v.et_pending = None;
    let key = v.exif_cache.keys().find(|k| k.0 == path).cloned().unwrap_or((path, None));
    let existing = v.exif_cache.get(&key).cloned().unwrap_or_default();
    let merged = exif_panel::merge(&existing, &flat);
    if !merged.is_empty() {
        v.exif_cache.insert(key, merged);
    }
}

// ── loading results ─────────────────────────────────────────────────────────

pub fn on_loaded(app: &mut App, path: PathBuf, result: Result<Arc<FullImage>, String>) -> Task<Message> {
    match &result {
        Ok(img) => app.loader.deliver(&path, img),
        Err(_) => app.loader.failed(&path),
    }
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let expected = display_paths(app, v).map(|(p, _)| p);
    let is_current = expected.as_ref() == Some(&path);
    let v = app.viewer.as_mut().expect("viewer");
    match result {
        Ok(img) => {
            let task = allocate(v.viewport, app.scale_factor, &path, &img);
            if is_current {
                crate::profile::record(("viewer", "full image on screen"), v.shown_at.elapsed());
                v.image = Some(img);
                v.status = None;
                if v.crop_pending {
                    enter_crop(app);
                }
            }
            task
        }
        Err(msg) => {
            if is_current {
                v.image = None;
                v.status = Some(format!("Could not load image\n{msg}"));
            }
            Task::none()
        }
    }
}

/// The embedded preview arrived: show it if the image is still missing.
pub fn on_preview(app: &mut App, path: PathBuf, image: Option<Arc<FullImage>>) -> Task<Message> {
    if !app.loader.deliver_preview(&path, image.as_ref()) {
        return Task::none();
    }
    let (Some(img), Some(v)) = (image, app.viewer.as_ref()) else { return Task::none() };
    let is_current = display_paths(app, v).is_some_and(|(p, _)| p == path);
    let v = app.viewer.as_mut().expect("viewer");
    let task = allocate(v.viewport, app.scale_factor, &path, &img);
    if is_current && v.image.is_none() {
        crate::profile::record(("viewer", "preview on screen"), v.shown_at.elapsed());
        v.image = Some(img);
        v.status = None;
        if v.crop_pending {
            enter_crop(app);
        }
    }
    task
}

/// Upload the level the viewer will draw at fit size (and the next coarser)
/// ahead of time, so a slide never shows an empty frame.
fn allocate(viewport: Size, sf: f32, path: &std::path::Path, img: &Arc<FullImage>) -> Task<Message> {
    let vp = if viewport.width > 0.0 { viewport } else { Size::new(1100.0, 650.0) };
    let fit = (vp.width / img.width.max(1) as f32).min(vp.height / img.height.max(1) as f32);
    let i = img.level_for_scale(fit * sf);
    let mut tasks = Vec::new();
    for idx in [i, (i + 1).min(img.levels.len() - 1)] {
        let handle = img.levels[idx].handle.clone();
        let p = path.to_path_buf();
        tasks.push(iimage::allocate(handle).map(move |r| Message::Viewer(Msg::Allocated(p.clone(), r))));
        if i == img.levels.len() - 1 {
            break;
        }
    }
    Task::batch(tasks)
}

fn allocate_current(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let Some((path, _)) = display_paths(app, v) else { return Task::none() };
    match &v.image {
        Some(img) => allocate(v.viewport, app.scale_factor, &path, img),
        None => Task::none(),
    }
}

// ── update ──────────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    // The window's own reports count while the compare view borrows it.
    if let Msg::Window(m) = msg {
        viewer_window::update(app, m);
        return Task::none();
    }
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let sheet = viewer_burst::on_sheet(v);
    match msg {
        Msg::Prev => {
            let i = v.index;
            if i > 0 {
                return navigate(app, i - 1, -1);
            }
        }
        Msg::Next => {
            let i = v.index;
            return navigate(app, i + 1, 1);
        }
        Msg::TogglePair => {
            if v.pair.is_none() || v.crop_mode || sheet {
                return Task::none();
            }
            v.show_pair = !v.show_pair;
            v.key += 1;
            v.transition = Transition::Cut;
            if let Some((p, t)) = display_paths(app, app.viewer.as_ref().expect("viewer")) {
                show_path(app, &p, t);
            }
            return allocate_current(app);
        }
        // On the sheet, zooming in opens the outlined frame.
        Msg::Actual | Msg::ZoomIn if sheet => {
            let at = v.index;
            return viewer_burst::zoom_in(app, at);
        }
        Msg::Fit if sheet => {}
        Msg::Fit => v.cmd = (v.cmd.0 + 1, ViewCmd::Fit),
        Msg::Actual => v.cmd = (v.cmd.0 + 1, ViewCmd::Actual),
        Msg::ZoomIn => v.cmd = (v.cmd.0 + 1, ViewCmd::ZoomIn),
        Msg::ZoomOut => {
            // A burst's frame zooms out to its sheet.
            if let Some(t) = viewer_burst::zoom_out(app) {
                return t;
            }
            if let Some(v) = app.viewer.as_mut() {
                v.cmd = (v.cmd.0 + 1, ViewCmd::ZoomOut);
            }
        }
        Msg::TogglePrune => {
            if v.read_only {
                return Task::none();
            }
            let Some(id) = v.record_id() else { return Task::none() };
            if let Some(r) = app.coll.get_mut(id) {
                r.is_pruned = !r.is_pruned;
            }
            return app.refresh_prune(&[id]);
        }
        Msg::ToggleMark => {
            if v.read_only {
                return Task::none();
            }
            let Some(id) = v.record_id() else { return Task::none() };
            if let Some(r) = app.coll.get_mut(id) {
                r.is_marked = !r.is_marked;
            }
            return app.refresh_mark(&[id]);
        }
        Msg::ShowInFolder => {
            let path = app.viewer.as_ref().and_then(|v| v.record(app)).map(|r| r.path.clone());
            if let Some(p) = path {
                show_in_folder(&p);
            }
        }
        Msg::ToggleInfo => v.info = !v.info,
        Msg::ToggleCrop => {
            if v.crop_mode {
                return apply_crop_exit(app, true);
            }
            if !sheet {
                enter_crop(app);
            }
        }
        Msg::CropApply => return apply_crop_exit(app, true),
        Msg::CropCancel => return apply_crop_exit(app, false),
        Msg::CropReset => {
            let r = resolve_ratio(v, &v.ratio_sel.clone());
            v.crop = crop_math::reset(&v.crop, r);
        }
        Msg::Aspect(ratio) => {
            if !v.crop_mode {
                return Task::none();
            }
            select_aspect(v, ratio.clone());
            // Stamp the label so it survives to the sidecar.
            let label = (ratio.label != FREEFORM).then(|| ratio.label.clone());
            v.crop = Crop::new(v.crop.left, v.crop.top, v.crop.right, v.crop.bottom, label);
            if let Some(r) = resolve_ratio(v, &ratio) {
                v.crop = crop_math::refit_to_aspect(&v.crop, Some(r));
            }
        }
        Msg::Flip => {
            if v.ratio_sel.is_free() {
                return Task::none();
            }
            v.portrait = !v.portrait;
            let flipped = v.ratio_sel.flipped();
            return update(app, Msg::Aspect(flipped));
        }
        Msg::SpinW(s) => v.spin_w = s.chars().filter(|c| c.is_ascii_digit()).take(3).collect(),
        Msg::SpinH(s) => v.spin_h = s.chars().filter(|c| c.is_ascii_digit()).take(3).collect(),
        Msg::CustomSet => {
            let w: f64 = v.spin_w.parse().unwrap_or(0.0);
            let h: f64 = v.spin_h.parse().unwrap_or(0.0);
            if !(1.0..=999.0).contains(&w) || !(1.0..=999.0).contains(&h) {
                return Task::none();
            }
            v.portrait = h > w;
            return update(app, Msg::Aspect(AspectRatio::custom(w, h)));
        }
        Msg::Stage(ev) => match ev {
            StageEvent::Zoom(z) => v.zoom = z,
            StageEvent::Crop(c) => {
                if v.crop_mode {
                    v.crop = c;
                }
            }
            StageEvent::Swipe(dir) => {
                let to = v.index as i64 + dir as i64;
                if to >= 0 && (to as usize) < v.items.len() {
                    // The swipe has already shown the photo: it stays.
                    let from = viewer_burst::run_key(v);
                    viewer_burst::arrive(v, to as usize, Land::Frame, from, false);
                    v.index = to as usize;
                    return load_current(app, Transition::Keep);
                }
            }
            StageEvent::Viewport(s) => {
                v.viewport = s;
                viewer_burst::want_previews(app);
                return allocate_current(app);
            }
            StageEvent::Sheet(ev) => return viewer_burst::on_stage(app, ev),
        },
        Msg::Allocated(path, Ok(allocation)) => app.loader.hold_allocation(&path, allocation),
        Msg::Allocated(_, Err(_)) => {}
        Msg::Advance(seq) => return viewer_advance::on_beat(app, seq),
        Msg::ToggleAdvance => return viewer_advance::toggle(app),
        Msg::ToggleFilmstrip => viewer_filmstrip::toggle(app),
        Msg::ToggleFullscreen => return viewer_window::toggle_fullscreen(app),
        Msg::Film(ev) => return viewer_filmstrip::on_event(app, ev),
        Msg::Window(_) => {}
        Msg::ToSheet => return viewer_burst::to_sheet(app),
        Msg::Leave(dir) => return viewer_burst::leave(app, dir),
        Msg::CompareFrames => return viewer_burst::compare(app),
    }
    Task::none()
}

pub fn on_key(app: &mut App, key: Key, mods: keyboard::Modifiers) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    let ch = |k: &Key| match k {
        Key::Character(c) => Some(c.to_lowercase()),
        _ => None,
    };
    let c = ch(&key);
    let c = c.as_deref();
    // Undo and redo work from the viewer too; the crop tool owns the keys
    // while it is open.
    if let Some(cmd @ (super::commands::Cmd::Undo | super::commands::Cmd::Redo)) = super::commands::Cmd::from_key(&key, mods) {
        if v.crop_mode {
            return Task::none();
        }
        return app.update(Message::Cmd(cmd));
    }
    // Window-wide keys, also while cropping.
    if key == Key::Named(Named::F11) && !mods.control() && !mods.alt() {
        return viewer_window::toggle_fullscreen(app);
    }
    if super::commands::Cmd::from_key(&key, mods) == Some(super::commands::Cmd::AdvanceAfterMark) {
        return viewer_advance::toggle(app);
    }
    if super::commands::Cmd::from_key(&key, mods) == Some(super::commands::Cmd::Preferences) {
        return app.update(Message::Cmd(super::commands::Cmd::Preferences));
    }
    if mods.control() || mods.alt() {
        return Task::none();
    }
    if !v.crop_mode {
        if let Some(t) = viewer_burst::on_key(app, &key, c) {
            return t;
        }
    }
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    if v.crop_mode {
        return match (&key, c) {
            (Key::Named(Named::Enter), _) | (_, Some("c")) => update(app, Msg::CropApply),
            (Key::Named(Named::Escape), _) => update(app, Msg::CropCancel),
            (_, Some("r")) => update(app, Msg::CropReset),
            (Key::Named(Named::Space), _) | (_, Some("f")) => update(app, Msg::Fit),
            (_, Some("1")) => update(app, Msg::Actual),
            (_, Some("+")) | (_, Some("=")) => update(app, Msg::ZoomIn),
            (_, Some("-")) => update(app, Msg::ZoomOut),
            _ => Task::none(),
        };
    }
    let len = v.items.len();
    let fullscreen = v.win.fullscreen;
    match (&key, c) {
        (Key::Named(Named::ArrowLeft), _) | (_, Some("a")) => update(app, Msg::Prev),
        (Key::Named(Named::ArrowRight), _) | (_, Some("d")) => update(app, Msg::Next),
        (Key::Named(Named::Home), _) => navigate(app, 0, -1),
        (Key::Named(Named::End), _) => navigate(app, len.saturating_sub(1), 1),
        (Key::Named(Named::PageUp), _) => update(app, Msg::Prev),
        (Key::Named(Named::PageDown), _) => update(app, Msg::Next),
        (Key::Named(Named::Escape), _) if fullscreen => viewer_window::leave_fullscreen(app),
        (Key::Named(Named::Escape), _) => close(app),
        (Key::Named(Named::Space), _) | (_, Some("f")) => update(app, Msg::Fit),
        (_, Some("1")) => update(app, Msg::Actual),
        (_, Some("+")) | (_, Some("=")) => update(app, Msg::ZoomIn),
        (_, Some("-")) => update(app, Msg::ZoomOut),
        (Key::Named(Named::Delete), _) | (_, Some("p")) => viewer_advance::mark_key(app, Mark::Prune),
        (_, Some("m")) => viewer_advance::mark_key(app, Mark::Keep),
        (_, Some("t")) => {
            viewer_filmstrip::toggle(app);
            Task::none()
        }
        (_, Some("b")) if !v.read_only => viewer_burst::to_sheet(app),
        (_, Some("c")) => {
            enter_crop(app);
            Task::none()
        }
        (_, Some("u")) => {
            let Some(v) = app.viewer.as_ref() else { return Task::none() };
            if v.read_only {
                return Task::none();
            }
            match v.record_id() {
                Some(id) if app.coll.get(id).is_some_and(|r| r.is_pruned) => update(app, Msg::TogglePrune),
                _ => Task::none(),
            }
        }
        (_, Some("?")) => app.open_shortcuts(),
        _ => Task::none(),
    }
}

pub fn show_in_folder(path: &std::path::Path) {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg("/select,").arg(path).spawn();
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
        return;
    }
    #[allow(unreachable_code)]
    {
        // Linux: the FileManager1 D-Bus interface opens the folder with the
        // file selected (Dolphin, Nautilus, Nemo, Thunar all implement it).
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let ok = Command::new("dbus-send")
                .args([
                    "--session",
                    "--print-reply",
                    "--dest=org.freedesktop.FileManager1",
                    "/org/freedesktop/FileManager1",
                    "org.freedesktop.FileManager1.ShowItems",
                ])
                .arg(format!("array:string:{uri}"))
                .arg("string:")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                if let Some(parent) = path.parent() {
                    let _ = Command::new("xdg-open").arg(parent).spawn();
                }
            }
        });
    }
}

// ── view ────────────────────────────────────────────────────────────────────

/// "LENS" → "Lens", "Lens Model" → "Lens model"; short all-caps words
/// (ISO, GPS, EV) are kept.
fn sentence(s: &str) -> String {
    let mut out = Vec::new();
    for (i, w) in s.split(' ').enumerate() {
        let caps = w.len() > 1 && w.chars().all(|c| !c.is_lowercase());
        let word = if caps && w.len() <= 3 {
            w.to_string()
        } else if i == 0 {
            let lower = w.to_lowercase();
            let mut cs = lower.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        } else {
            w.to_lowercase()
        };
        out.push(word);
    }
    out.join(" ")
}

fn vmsg(m: Msg) -> Option<Message> {
    Some(Message::Viewer(m))
}

pub fn view<'a>(app: &'a App, v: &'a Viewer) -> Element<'a, Message> {
    let c = coll(app, v);
    let record = v.record(app);
    let display = v.display_id().and_then(|id| c.get(id));
    let n = v.items.len();
    let nav_ok = !v.crop_mode;
    let run = viewer_burst::run(v);
    let on_sheet = viewer_burst::on_sheet(v);

    // ── top bar ─────────────────────────────────────────────────────────
    let (prev, next) = match run.as_ref().filter(|_| on_sheet) {
        // On the sheet the chevrons leave the burst.
        Some(r) => (
            kit::tip(kit::icon_btn(Glyph::ChevronLeft, t::INK, style::ghost(), (r.start > 0).then_some(Message::Viewer(Msg::Leave(-1)))), "Before this burst  (PgUp)", true),
            kit::tip(kit::icon_btn(Glyph::ChevronRight, t::INK, style::ghost(), (r.end < n).then_some(Message::Viewer(Msg::Leave(1)))), "After this burst  (PgDn)", true),
        ),
        None => (
            kit::tip(
                kit::icon_btn(Glyph::ChevronLeft, t::INK, style::ghost(), if nav_ok && v.index > 0 { vmsg(Msg::Prev) } else { None }),
                "Previous  (←)",
                true,
            ),
            kit::tip(
                kit::icon_btn(Glyph::ChevronRight, t::INK, style::ghost(), if nav_ok && v.index + 1 < n { vmsg(Msg::Next) } else { None }),
                "Next  (→)",
                true,
            ),
        ),
    };
    let name = display.map(|r| r.filename()).unwrap_or_default();
    let mut top = row![prev, next, Space::new().width(t::S2)].spacing(t::S2).align_y(Alignment::Center);
    if on_sheet {
        // The burst, and the frame the outline is on.
        let title = viewer_burst::title(app, v).unwrap_or_default();
        let frame = record.and_then(|r| viewer_burst::frame_caption(app, v).map(|f| format!("{f}, {}", r.filename())));
        top = top.push(text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK));
        top = top.push(kit::caption(v.sheet.note.clone().or(frame).unwrap_or_default()));
    } else {
        top = top.push(text(name).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK));
        match viewer_burst::frame_caption(app, v) {
            Some(frame) => {
                top = top.push(kit::caption(frame));
                let tip = if viewer_burst::sheet_open(v) { "See every frame of this burst  (Esc, B or −)" } else { "See every frame of this burst  (B or −)" };
                top = top.push(kit::tip(
                    kit::icon_text_btn(Glyph::Grid, "All frames", t::INK2, style::ghost(), if nav_ok { vmsg(Msg::ToSheet) } else { None }),
                    tip,
                    true,
                ));
            }
            None => top = top.push(kit::caption(format!("{} of {}", util::thousands(v.index as u64 + 1), util::thousands(n as u64)))),
        }
    }
    top = top.push(Space::new().width(Length::Fill));

    let pair_btn: Element<'a, Message> = match (v.pair.is_some() && !on_sheet, display) {
        (true, Some(d)) => {
            let other = match d.file_type {
                FileType::Raw => "JPG",
                FileType::Jpg => "RAW",
            };
            let look = if v.show_pair { style::toggle(true, t::SIGNAL) } else { style::ghost() };
            kit::tip(
                kit::icon_text_btn(Glyph::Swap, format!("Show {other}"), if v.show_pair { t::SIGNAL } else { t::INK2 }, look, if nav_ok { vmsg(Msg::TogglePair) } else { None }),
                "Switch between the RAW and JPG of this shot",
                true,
            )
        }
        _ => Space::new().width(0).into(),
    };
    top = top.push(pair_btn);
    if on_sheet {
        let picked = v.sheet.picked.len();
        let label = if picked >= 2 { format!("Compare {picked}") } else { "Compare".to_string() };
        let tip = if picked >= 2 {
            "Compare the picked frames side by side  (C)"
        } else {
            "Compare this frame with its neighbours, or with frames you pick: Ctrl+click or Space  (C)"
        };
        top = top.push(kit::tip(kit::icon_text_btn(Glyph::Compare, label, t::INK2, style::ghost(), vmsg(Msg::CompareFrames)), tip, true));
    } else if !v.read_only {
        let cropped = display.is_some_and(|r| r.is_cropped());
        let active = v.crop_mode || cropped;
        let look = if active { style::toggle(true, t::SIGNAL) } else { style::ghost() };
        top = top.push(kit::tip(
            kit::icon_text_btn(Glyph::Crop, if cropped { "Cropped" } else { "Crop" }, if active { t::SIGNAL } else { t::INK2 }, look, vmsg(Msg::ToggleCrop)),
            "Crop this photo  (C)",
            true,
        ));
    }
    top = top.push(kit::tip(kit::icon_btn(Glyph::Folder, t::INK2, style::ghost(), vmsg(Msg::ShowInFolder)), "Show in folder", true));
    if !v.read_only {
        let marked = record.is_some_and(|r| r.is_marked);
        let pruned = record.is_some_and(|r| r.is_pruned);
        let advance = app.settings.viewer_advance;
        top = top.push(kit::vrule(20.0));
        let (fg, look) = (if advance { t::SIGNAL } else { t::INK2 }, style::toggle(advance, t::SIGNAL));
        let toggle_advance = if v.win.width >= 1000.0 {
            kit::icon_text_btn(Glyph::Skip, "Auto-advance", fg, look, vmsg(Msg::ToggleAdvance))
        } else {
            // Narrow: Keep and Prune need the room more.
            kit::icon_btn(Glyph::Skip, fg, look, vmsg(Msg::ToggleAdvance))
        };
        let tip = if advance {
            "Auto-advance is on: P or M moves on to the next photo  (Ctrl+Shift+A)"
        } else {
            "Auto-advance: move on to the next photo when P or M marks one  (Ctrl+Shift+A)"
        };
        top = top.push(kit::tip(toggle_advance, tip, true));
        top = top.push(kit::tip(
            kit::icon_text_btn(Glyph::Keep, "Keep", if marked { t::KEEP } else { t::INK2 }, style::toggle(marked, t::KEEP), vmsg(Msg::ToggleMark)),
            "Mark for export  (M)",
            true,
        ));
        top = top.push(kit::tip(
            kit::icon_text_btn(Glyph::Prune, "Prune", if pruned { t::REJECT } else { t::INK2 }, style::toggle(pruned, t::REJECT), vmsg(Msg::TogglePrune)),
            "Mark to prune  (P)",
            true,
        ));
    }
    let top_bar = container(top.padding(Padding::from([0.0, t::S2])))
        .height(48)
        .center_y(48)
        .width(Length::Fill)
        .style(style::fill(t::MOUNT));

    let mut body = column![top_bar, kit::hairline()];

    // The crop bar and the prune banner come and go in slots of their own,
    // so the photo keeps its place in the widget tree: its zoom, a slide or
    // the burst sheet's zoom carry on when either appears.
    body = body.push(if v.crop_mode { crop_bar(v) } else { Space::new().height(0).into() });
    // Held back while auto-advance is about to move on, so the layout
    // doesn't jump twice; the stamp says what happened instead.
    // (Not on the sheet: its tiles say so, and the banner coming and going
    // as the outline moves would shift every tile.)
    let mut banner_slot = column![];
    if !v.read_only && !on_sheet && record.is_some_and(|r| r.is_pruned) && !v.flow.waiting() {
        let banner = row![
            icons::view(Glyph::Prune, 16.0, t::REJECT),
            text("Marked to prune. It goes to the Trash when you review pruned files.").size(t::T_BODY).color(t::INK),
            Space::new().width(Length::Fill),
            button(container(text("Unmark").size(t::T_SMALL).font(paint::MEDIUM)).center_y(Length::Fill))
                .height(t::CONTROL_SM)
                .padding(Padding::from([0.0, t::S3]))
                .style(style::toggle(true, t::REJECT).style())
                .on_press(Message::Viewer(Msg::TogglePrune)),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)
        .padding(Padding::from([0.0, t::S3]));
        banner_slot = banner_slot.push(container(banner).height(36).center_y(36).width(Length::Fill).style(style::fill(t::alpha(t::REJECT, 0.12))));
        banner_slot = banner_slot.push(container(Space::new().width(Length::Fill).height(1)).style(style::fill(t::alpha(t::REJECT, 0.35))));
    }
    body = body.push(banner_slot);

    // ── stage + EXIF panel ──────────────────────────────────────────────
    let neighbors = [
        v.index.checked_sub(1).and_then(|i| v.items.get(i)).and_then(|&id| c.get(id)).and_then(|r| app.loader.peek(&r.path).cloned()),
        v.items.get(v.index + 1).and_then(|&id| c.get(id)).and_then(|r| app.loader.peek(&r.path).cloned()),
    ];
    let crop_props = v.crop_mode.then(|| CropProps { crop: v.crop.clone(), ratio: resolve_ratio(v, &v.ratio_sel) });
    let sheet = viewer_burst::sheet(app, v);
    // A burst's frame still loading shows its tile's picture, not the word.
    let status = if sheet.is_some() && v.image.is_none() && v.status.as_deref() == Some(LOADING) { None } else { v.status.clone() };
    let stage = ImageStage::new(
        v.key,
        v.image.clone(),
        status,
        v.transition,
        v.cmd,
        crop_props,
        neighbors,
        app.scale_factor,
        |e| Message::Viewer(Msg::Stage(e)),
    )
    .sheet(sheet);
    // The stamp floats over the photo; the filmstrip runs under it.
    let mut photo = column![stack![stage, viewer_advance::stamp(v)]].height(Length::Fill);
    if app.settings.viewer_filmstrip {
        photo = photo.push(viewer_filmstrip::view(app, v, !v.crop_mode));
    }
    let mut middle = row![photo].height(Length::Fill);
    if v.info {
        middle = middle.push(exif_panel(app, v));
    }
    body = body.push(middle);

    // ── bottom bar: zoom, what this file is, the info toggle ────────────
    // On the sheet there is no zoom to show; zooming in opens a frame.
    let zoom_label = text(if on_sheet { "All".to_string() } else { format!("{:.0}%", v.zoom * 100.0) })
        .size(t::T_SMALL)
        .font(paint::MEDIUM)
        .color(t::INK2)
        .width(44)
        .center();
    let (out_tip, in_tip) = match (on_sheet, run.is_some()) {
        (true, _) => ("Zoom out  (−)", "Zoom into the outlined frame  (+ or Enter)"),
        (false, true) => ("Zoom out; at fit, every frame of the burst  (−)", "Zoom in  (+)"),
        (false, false) => ("Zoom out  (−)", "Zoom in  (+)"),
    };
    let mut bottom = row![
        kit::tip(kit::icon_btn(Glyph::ZoomOut, t::INK2, style::ghost(), if on_sheet { None } else { vmsg(Msg::ZoomOut) }), out_tip, false),
        zoom_label,
        kit::tip(kit::icon_btn(Glyph::ZoomIn, t::INK2, style::ghost(), vmsg(Msg::ZoomIn)), in_tip, false),
        kit::tip(kit::icon_btn(Glyph::Fit, t::INK2, style::ghost(), if on_sheet { None } else { vmsg(Msg::Fit) }), "Fit to window  (F)", false),
        kit::tip(
            button(container(text("1:1").size(t::T_SMALL).font(paint::MEDIUM)).center(Length::Fill))
                .width(t::CONTROL_H + 8.0)
                .height(t::CONTROL_H)
                .padding(0)
                .style(style::ghost().style())
                .on_press(Message::Viewer(Msg::Actual)),
            "Actual size  (1)",
            false,
        ),
        kit::vrule(16.0),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    if let Some(r) = display {
        let mut facts = row![].spacing(t::S4).align_y(Alignment::Center);
        facts = facts.push(text(r.file_type.label()).size(t::T_SMALL).font(paint::MEDIUM).color(if r.file_type == FileType::Raw { t::RAW } else { t::JPG }));
        if let Some(img) = &v.image {
            facts = facts.push(kit::caption(format!("{} × {} px", img.width, img.height)));
        }
        facts = facts.push(kit::caption(util::fmt_size(r.file_size)));
        facts = facts.push(kit::caption(r.modified.local.format("%Y-%m-%d %H:%M").to_string()));
        if r.is_cropped() {
            let crop = r.crop.clone().unwrap_or_else(|| Crop::full(None));
            let label = match &v.image {
                Some(img) => {
                    let (w, h) = crop.pixel_size(img.width, img.height);
                    match &crop.aspect_label {
                        Some(a) => format!("Cropped to {w} × {h} px, {a}"),
                        None => format!("Cropped to {w} × {h} px"),
                    }
                }
                None => "Cropped".into(),
            };
            facts = facts.push(row![icons::view(Glyph::Scissors, 14.0, t::SIGNAL), text(label).size(t::T_SMALL).color(t::INK2)].spacing(6).align_y(Alignment::Center));
        }
        bottom = bottom.push(container(facts).padding(Padding::from([0.0, t::S2])));
    }
    bottom = bottom.push(Space::new().width(Length::Fill));
    if on_sheet && v.win.width >= 1180.0 {
        bottom = bottom.push(kit::caption("Click a frame to look closer  ·  Ctrl+click or Space picks frames  ·  C compares"));
        bottom = bottom.push(Space::new().width(t::S3));
    }
    let film = app.settings.viewer_filmstrip;
    bottom = bottom.push(kit::tip(
        kit::icon_text_btn(Glyph::Filmstrip, "Filmstrip", if film { t::INK } else { t::INK2 }, style::segment(film), vmsg(Msg::ToggleFilmstrip)),
        "Show or hide the filmstrip  (T)",
        false,
    ));
    bottom = bottom.push(Space::new().width(t::S1));
    bottom = bottom.push(kit::tip(
        kit::icon_text_btn(Glyph::Info, "Info", if v.info { t::INK } else { t::INK2 }, style::segment(v.info), vmsg(Msg::ToggleInfo)),
        "Show or hide photo details",
        false,
    ));
    bottom = bottom.push(Space::new().width(t::S1));
    bottom = bottom.push(kit::tip(
        kit::icon_btn(Glyph::FullScreen, if v.win.fullscreen { t::INK } else { t::INK2 }, style::segment(v.win.fullscreen), vmsg(Msg::ToggleFullscreen)),
        if v.win.fullscreen { "Leave full screen  (F11 or Esc)" } else { "Full screen  (F11)" },
        false,
    ));
    let bottom_bar = container(bottom.padding(Padding::from([0.0, t::S2]))).height(44).center_y(44).width(Length::Fill).style(style::fill(t::MOUNT));
    body = body.push(kit::hairline()).push(bottom_bar);

    container(body).width(Length::Fill).height(Length::Fill).style(style::fill(t::GROUND)).into()
}

fn crop_bar(v: &Viewer) -> Element<'_, Message> {
    let presets = crop::presets()
        .into_iter()
        .map(|preset| {
            let shown = oriented(v, &preset);
            let checked = shown.label == v.ratio_sel.label;
            kit::segment_compact(shown.label.clone(), checked, Message::Viewer(Msg::Aspect(shown)))
        })
        .collect();
    let mut r = row![kit::segmented(presets)].spacing(t::S2).align_y(Alignment::Center);
    r = r.push(kit::tip(
        kit::icon_btn(Glyph::Orientation, t::INK2, style::ghost(), (!v.ratio_sel.is_free()).then_some(Message::Viewer(Msg::Flip))),
        "Swap portrait and landscape",
        true,
    ));
    let spin = |value: &str, f: fn(String) -> Msg| {
        text_input("", value)
            .on_input(move |s| Message::Viewer(f(s)))
            .on_submit(Message::Viewer(Msg::CustomSet))
            .width(44)
            .size(t::T_SMALL)
            .padding(Padding::from([4.0, 4.0]))
            .align_x(iced::alignment::Horizontal::Center)
            .style(style::input())
    };
    r = r.push(kit::vrule(20.0));
    r = r.push(spin(&v.spin_w, Msg::SpinW));
    r = r.push(kit::caption(":"));
    r = r.push(spin(&v.spin_h, Msg::SpinH));
    r = r.push(
        button(container(text("Set").size(t::T_SMALL).font(paint::MEDIUM)).center_y(Length::Fill))
            .height(t::CONTROL_SM)
            .padding(Padding::from([0.0, t::S3]))
            .style(style::secondary().style())
            .on_press(Message::Viewer(Msg::CustomSet)),
    );
    r = r.push(Space::new().width(Length::Fill));
    let readout = match &v.image {
        Some(img) => {
            let (w, h) = v.crop.pixel_size(img.width, img.height);
            let pct = 100.0 * (w as f64 * h as f64) / (img.width as f64 * img.height as f64).max(1.0);
            format!("{w} × {h} px ({pct:.0}%)")
        }
        None => String::new(),
    };
    r = r.push(kit::caption(readout));
    r = r.push(Space::new().width(t::S2));
    r = r.push(kit::tip(kit::btn("Reset", style::ghost(), vmsg(Msg::CropReset)), "Back to the whole frame  (R)", true));
    r = r.push(kit::tip(kit::btn("Cancel", style::secondary(), vmsg(Msg::CropCancel)), "Discard changes  (Esc)", true));
    r = r.push(kit::tip(kit::btn("Apply", style::primary(), vmsg(Msg::CropApply)), "Keep this crop  (Enter)", true));
    column![
        container(r.padding(Padding::from([0.0, t::S3]))).height(48).center_y(48).width(Length::Fill).style(style::fill(t::MOUNT)),
        kit::hairline(),
    ]
    .into()
}

fn exif_panel<'a>(app: &'a App, v: &'a Viewer) -> Element<'a, Message> {
    let c = coll(app, v);
    let mut col = column![].spacing(0).padding(Padding { left: t::S4, right: t::S3, top: t::S2, bottom: t::S4 });
    let section = |title: &str| -> Element<'a, Message> {
        container(kit::heading(sentence(title))).padding(Padding { top: t::S4, bottom: t::S2, left: 0.0, right: 0.0 }).into()
    };
    let form = |rows: Vec<(String, String)>| -> Element<'a, Message> {
        let mut f = column![].spacing(5).padding(Padding { left: 0.0, right: 0.0, top: 0.0, bottom: t::S2 });
        for (l, val) in rows {
            f = f.push(
                row![
                    text(l).size(t::T_SMALL).color(t::INK3).width(92),
                    text(val).size(t::T_SMALL).color(t::INK).width(Length::Fill),
                ]
                .spacing(t::S2),
            );
        }
        f.into()
    };
    if let Some(rid) = v.record_id() {
        if let Some(r) = c.get(rid) {
            col = col.push(section("File"));
            let mut rows = vec![
                ("Type".to_string(), r.file_type.label().to_string()),
                ("Size".to_string(), util::fmt_size(r.file_size)),
                ("Modified".to_string(), r.modified.local.format("%Y-%m-%d %H:%M").to_string()),
            ];
            if let Some(p) = v.pair.and_then(|p| c.get(p)) {
                let (raw, jpg) = if r.file_type == FileType::Raw { (r, p) } else { (p, r) };
                rows.push(("RAW".into(), raw.filename()));
                rows.push(("JPG".into(), jpg.filename()));
            }
            col = col.push(form(rows));
            let jpg_pair = v.pair.and_then(|p| c.get(p)).filter(|p| p.file_type == FileType::Jpg).map(|p| p.path.clone());
            if let Some(sections) = v.exif_cache.get(&(r.path.clone(), jpg_pair)) {
                for (name, rows) in sections {
                    col = col.push(kit::hairline());
                    col = col.push(section(name));
                    col = col.push(form(rows.iter().map(|(l, val)| (sentence(l), val.clone())).collect()));
                }
            }
        }
    }
    let scroll = scrollable(col).style(style::scroll).width(Length::Fill).height(Length::Fill);
    row![
        container(Space::new().width(1).height(Length::Fill)).style(style::fill(t::LINE)),
        container(scroll).width(279).height(Length::Fill).style(style::fill(t::MOUNT)),
    ]
    .into()
}
