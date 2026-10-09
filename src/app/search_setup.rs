//! Search's setup and its background work as the user sees it: the
//! download dialog (asked once, before the first index), indexing in the
//! status bar and its steps, and the banner when indexing stops.
//!
//! - **Ask once.**  The dialog lists each model with its size and licence
//!   in plain words, what indexing reads and how long it takes, then shows
//!   the download, and a checksum or connection failure.  Its rows come
//!   from the model registry, so later phases add rows, not dialogs.
//!   Faces are found in the same pass, on unless turned off, with the face
//!   model the user picks (AuraFace chosen until then; InsightFace's
//!   licence said plainly, in the warning tone).  From the People tab the
//!   same dialog asks for the faces alone.
//! - **Then stay quiet.**  The first index is a line in the status bar with
//!   × to stop (click it for the steps); after an export, adding the new
//!   photos is a quieter line with no ×.
//! - **Say what failed; never work around it.**  When the graphics card
//!   fails, a banner says what failed, offers to try the card again and
//!   the way to Preferences, and nothing moves to the processor.

use iced::keyboard::{Key, key::Named};
use iced::widget::{Space, button, column, container, mouse_area, opaque, progress_bar, row, space, stack, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Task};

use super::fursuits;
use super::download_speed as speed;
use super::kit::{self, tip};
use super::search::{Dialog, DialogMsg, Download, Msg, Stage, search_models};
use super::style;
use super::{App, Message, dialogs};
use crate::ml::download::DownloadState;
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device};
use crate::search::{PassKind, PassProgress, Status, Stopped, estimate};
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme::{self as t, alpha};
use crate::util;

fn m(msg: Msg) -> Message {
    Message::Search(msg)
}

fn d(msg: DialogMsg) -> Message {
    Message::Search(Msg::Dialog(msg))
}

/// The download ended badly (the dialog says how).
pub fn failed(s: &DownloadState) -> bool {
    matches!(s, DownloadState::Mismatch { .. } | DownloadState::Offline | DownloadState::Failed(_))
}

pub fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A size known exactly, in the dialog's round figures ("785 MB").
pub fn size(n: u64) -> String {
    estimate::describe_bytes(n).trim_start_matches("about ").to_string()
}

/// A download's progress in the same units as the sizes the dialog
/// quotes: "312 of 785 MB", "0.4 of 1.2 GB".
pub fn progress_bytes(done: u64, total: u64) -> String {
    if total >= 1_000_000_000 {
        format!("{:.1} of {:.1} GB", done as f64 / 1e9, total as f64 / 1e9)
    } else {
        format!("{} of {} MB", (done as f64 / 1e6).round() as u64, (total as f64 / 1e6).round() as u64)
    }
}

/// What indexing this library costs, in words.
pub struct Costs {
    /// "about 20 GB from the library on the share"
    pub read: String,
    /// "About 7 minutes with the graphics card"
    pub takes: String,
    /// The models still to download: "785 MB"
    pub download: String,
    /// "about 94 MB"
    pub kept: String,
}

pub fn costs(app: &App) -> Costs {
    let st = &app.search.status;
    let shots = st.library_shots;
    let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
    let device = st.device.chosen;
    let time = estimate::describe_seconds(estimate::seconds(shots, device));
    let to_get = if st.models_installed { 0 } else { models::bytes_for(&search_models()) };
    Costs {
        read: format!("{} from the library{}", estimate::describe_bytes(estimate::bytes_read(shots)), if on_share { " on the share" } else { "" }),
        takes: capital(&format!("{time} {} {}", if device == Device::Gpu { "with" } else { "on" }, device.phrase())),
        download: size(to_get),
        kept: estimate::describe_bytes(estimate::bytes_kept(shots)),
    }
}

// ── what is still to download ───────────────────────────────────────────────

/// `model`'s files (the detector's and its own) are in place, as the
/// status knows: it says so only for the face model the engine has
/// chosen, so another one counts as missing.
pub fn face_ready(st: &Status, model: ModelId) -> bool {
    st.faces.installed && st.faces.model == Some(model)
}

/// The models still to fetch, as the status knows them: the search model
/// unless it is in place (when `search` asks for it), and the detector
/// with `face` unless that same model's set is.  The dialog's sizes,
/// its "Downloaded" labels and the rows it shows all come from here.
pub fn missing_models(st: &Status, search: bool, face: Option<ModelId>) -> Vec<ModelId> {
    let mut out = Vec::new();
    if search && !st.models_installed {
        out.extend(search_models());
    }
    if let Some(f) = face.filter(|&f| !face_ready(st, f)) {
        out.extend(models::face_set(f));
    }
    out
}

/// Rows for `ids`, waiting until the engine reports on each.
fn waiting_rows(ids: &[ModelId]) -> Vec<Download> {
    ids.iter().map(|&id| Download { model: id, done: 0, total: models::info(id).bytes(), state: DownloadState::Waiting }).collect()
}

/// What *Download and index* does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// Everything is in place: the dialog closes and indexing starts.
    Index,
    /// The rows to show, waiting, until the engine reports on each: only
    /// the models it will fetch, so none waits for a report that never
    /// comes.
    Download(Vec<Download>),
}

pub fn begin_first_index(st: &Status, face: Option<ModelId>) -> Begin {
    let missing = missing_models(st, true, face);
    if missing.is_empty() {
        return Begin::Index;
    }
    Begin::Download(missing.into_iter().map(|id| Download { model: id, done: 0, total: models::info(id).bytes(), state: DownloadState::Waiting }).collect())
}

/// A download reported.  A new download after the last ones finished
/// starts a fresh list, so the status bar's total is this download's.
pub fn record_download(rows: &mut Vec<Download>, d: Download) {
    if !d.state.is_final() && rows.iter().all(|x| x.state.is_final()) {
        rows.clear();
    }
    match rows.iter_mut().find(|x| x.model == d.model) {
        Some(x) => *x = d,
        None => rows.push(d),
    }
}

/// Rows still waiting for a model the status now says is in place are
/// done: the engine reports only what it fetches, and a row the dialog
/// listed from an older status (another face model chosen, say) would
/// otherwise wait for ever.
pub fn settle_downloads(rows: &mut [Download], st: &Status) {
    for x in rows.iter_mut().filter(|x| x.state == DownloadState::Waiting) {
        let in_place = match x.model {
            ModelId::Siglip2 => st.models_installed,
            m if m == models::FACE_DETECTOR => st.faces.installed,
            m => face_ready(st, m),
        };
        if in_place {
            x.done = x.total;
            x.state = DownloadState::Checked;
        }
    }
}

// ── the download dialog ─────────────────────────────────────────────────────

pub fn dialog_update(app: &mut App, msg: DialogMsg) -> Task<Message> {
    let fallback = (false, app.settings.find_faces, super::subjects::face_model(app));
    let s = &mut app.search;
    match msg {
        DialogMsg::Faces => {
            if let Some(d) = s.dialog.as_mut().filter(|d| !d.faces_only) {
                d.faces = !d.faces;
            }
            Task::none()
        }
        DialogMsg::FaceModel(m) => {
            if let Some(d) = s.dialog.as_mut().filter(|d| d.faces) {
                d.face_model = m;
            }
            Task::none()
        }
        DialogMsg::Fursuits => {
            if let Some(d) = s.dialog.as_mut().filter(|d| !d.fursuits_only) {
                d.fursuits = !d.fursuits;
            }
            Task::none()
        }
        DialogMsg::Download | DialogMsg::Retry if s.dialog.as_ref().and_then(|d| d.judge).is_some() || judge_downloads(&s.downloads) => {
            // The close-call judge alone: chosen now, used once in place.
            let Some(choice) = s.dialog.as_ref().and_then(|d| d.judge).or(Some(app.settings.fursuit_judge).filter(|c| c.model().is_some())) else { return Task::none() };
            if app.settings.fursuit_judge != choice {
                app.settings.fursuit_judge = choice;
                app.settings.save_in_background();
            }
            let s = &mut app.search;
            s.link.set_judge(choice);
            if s.status.judge.downloaded.contains(&choice) {
                s.dialog = None;
                return app.show_status(format!("{} checks close calls from now on.", choice.label()), 6000);
            }
            if let Some(model) = choice.model() {
                s.downloads = waiting_rows(&[model]);
            }
            match s.dialog.as_mut() {
                Some(d) => d.stage = Stage::Progress,
                None => s.dialog = Some(Dialog { stage: Stage::Progress, ..Dialog::judge(choice, &app.settings) }),
            }
            app.search.link.download_judge(choice);
            Task::none()
        }
        DialogMsg::Download | DialogMsg::Retry => {
            let heads = s.dialog.as_ref().map(|d| (d.fursuits, d.fursuits_only));
            let (fursuits, fursuits_only) = heads.unwrap_or((app.settings.find_heads, false));
            if app.settings.find_heads != (fursuits || fursuits_only) {
                app.settings.find_heads = fursuits || fursuits_only;
                app.settings.save_in_background();
            }
            if fursuits_only {
                // The library is indexed: the head models (their download
                // first, only those missing), then the fursuit photos
                // looked at for heads.
                let s = &mut app.search;
                s.dialog = None;
                let missing = fursuits::missing_models(&s.status);
                if !missing.is_empty() {
                    s.downloads = waiting_rows(&missing);
                }
                s.link.set_find_heads(true);
                s.link.find_heads();
                return app.show_status("Finding fursuit heads in the library. Groups appear on the Fursuits tab as they are found.", 8000);
            }
            app.search.link.set_find_heads(fursuits);
            let s = &mut app.search;
            let ask = s.dialog.as_ref().map(|d| (d.faces_only, d.faces, d.face_model));
            let (faces_only, faces, model) = ask.unwrap_or(fallback);
            // The face model picked is the one Preferences shows from now.
            if app.settings.face_model != model || app.settings.find_faces != (faces || faces_only) {
                app.settings.face_model = model;
                app.settings.find_faces = faces || faces_only;
                app.settings.save_in_background();
            }
            if faces_only {
                // The library is indexed: find faces in it, with the
                // model picked (its download first, then the catch-up).
                let s = &mut app.search;
                s.dialog = None;
                s.link.set_find_faces(true);
                s.link.set_face_model(model);
                s.link.find_faces();
                return app.show_status(format!("Finding faces in the library with {}. Groups appear on the People tab as they are found.", models::info(model).name), 8000);
            }
            let face = faces.then_some(model);
            let s = &mut app.search;
            let mut ids = models::models_for(face);
            // The *Fursuits* pair asked for means heads on (the engine reads
            // it from the list); only the missing ones are rows.
            let heads_missing = if fursuits {
                ids.extend(models::FURSUIT_MODELS);
                fursuits::missing_models(&s.status)
            } else {
                Vec::new()
            };
            s.link.set_find_faces(faces);
            if face.is_some() {
                s.link.set_face_model(model);
            }
            let begin = match (begin_first_index(&s.status, face), heads_missing.is_empty()) {
                (Begin::Index, true) => Begin::Index,
                (Begin::Index, false) => Begin::Download(waiting_rows(&heads_missing)),
                (Begin::Download(mut rows), _) => {
                    rows.extend(waiting_rows(&heads_missing));
                    Begin::Download(rows)
                }
            };
            match begin {
                Begin::Index => {
                    s.dialog = None;
                    s.link.start_first_index(&ids);
                    let faces_too = if face.is_some() { " Faces are found in the same pass." } else { "" };
                    app.show_status(format!("Indexing the library. Search works on the newest days within a minute.{faces_too}"), 6000)
                }
                Begin::Download(rows) => {
                    // The rows show at once, waiting, until the engine
                    // reports.
                    s.downloads = rows;
                    if let Some(d) = s.dialog.as_mut() {
                        d.stage = Stage::Progress;
                    }
                    s.link.start_first_index(&ids);
                    Task::none()
                }
            }
        }
        DialogMsg::Show => {
            let d = Dialog::new(Stage::Progress, &app.settings);
            app.search.dialog = Some(d);
            Task::none()
        }
        DialogMsg::Cancel => {
            let was = s.dialog.take().map(|x| x.stage);
            if was == Some(Stage::Ask) {
                return app.show_status("Nothing was downloaded or indexed.", 5000);
            }
            Task::none()
        }
        DialogMsg::Close => {
            s.dialog = None;
            Task::none()
        }
        DialogMsg::Sums => {
            if let Some(dl) = s.dialog.as_mut() {
                dl.sums = !dl.sums;
            }
            Task::none()
        }
        DialogMsg::StopDownload => {
            s.dialog = None;
            s.link.cancel_downloads();
            let judge = judge_downloads(&s.downloads);
            app.show_status(if judge { "Stopped the download." } else { "Stopped the download. Nothing was indexed." }, 5000)
        }
    }
}

/// Keys while the dialog is open: Esc cancels (or closes, once it is
/// downloading: that carries on), Enter does the main action.
pub fn dialog_key(app: &mut App, key: &Key) -> Task<Message> {
    let Some(dl) = app.search.dialog.as_ref() else { return Task::none() };
    let failure = app.search.downloads.iter().any(|x| failed(&x.state));
    let msg = match (key, dl.stage) {
        (Key::Named(Named::Escape), Stage::Ask) => DialogMsg::Cancel,
        (Key::Named(Named::Escape), Stage::Progress) => DialogMsg::Close,
        (Key::Named(Named::Enter), Stage::Ask) => DialogMsg::Download,
        (Key::Named(Named::Enter), Stage::Progress) if failure => DialogMsg::Retry,
        (Key::Named(Named::Enter), Stage::Progress) => DialogMsg::Close,
        _ => return Task::none(),
    };
    dialog_update(app, msg)
}

pub fn dialog(app: &App) -> Option<Element<'_, Message>> {
    let dl = app.search.dialog.as_ref()?;
    let w = (app.window_size_width() - 48.0).min(700.0);
    let body = match dl.stage {
        Stage::Ask if dl.judge.is_some() => judge_ask_view(app, dl),
        Stage::Ask => ask_view(app, dl),
        Stage::Progress => progress_view(app),
    };
    Some(dialogs::backdrop(dialogs::frame_fit(body, w)))
}

/// A model's row: name, size, what it does, its licence in plain words
/// and where it comes from.
fn model_row<'a>(mi: &models::ModelInfo, ready: bool) -> Element<'a, Message> {
    let top = row![
        text(mi.name).size(t::T_BODY).font(paint::MEDIUM).color(t::INK),
        Space::new().width(Length::Fill),
        text(if ready { "Downloaded".to_string() } else { size(mi.bytes()) }).size(t::T_SMALL).color(t::INK2),
    ]
    .align_y(Alignment::Center);
    let col = column![
        top,
        text(mi.what).size(t::T_SMALL).color(t::INK2),
        text(format!("Licence: {}, {}. From {}.", mi.licence, mi.licence_plain, mi.publisher)).size(t::T_SMALL).color(t::INK3),
    ]
    .spacing(2);
    container(col).padding(Padding::from([7.0, 12.0])).width(Length::Fill).into()
}

fn group<'a>(rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![];
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(kit::hairline());
        }
        col = col.push(r);
    }
    container(col).width(Length::Fill).style(style::group()).into()
}

fn fact<'a>(k: &'a str, v: String, sub: Option<&'a str>) -> Element<'a, Message> {
    let mut words = column![text(v).size(t::T_BODY).color(t::INK)].spacing(2).width(Length::Fill);
    if let Some(sub) = sub {
        words = words.push(text(sub).size(t::T_SMALL).color(t::INK3));
    }
    row![text(k).size(t::T_SMALL).color(t::INK3).width(84), words].spacing(t::S3).padding(Padding::from([8.0, 12.0])).into()
}

/// A radio's dot.
fn radio_dot<'a>(on: bool, enabled: bool) -> Element<'a, Message> {
    let edge = if on && enabled { t::SIGNAL } else { t::LINE_HI };
    let inner: Element<'a, Message> = if on {
        container(Space::new())
            .width(8)
            .height(8)
            .style(move |_| iced::widget::container::Style { background: Some(Background::Color(if enabled { t::SIGNAL } else { t::INK4 })), border: Border { radius: 4.0.into(), ..Border::default() }, ..Default::default() })
            .into()
    } else {
        Space::new().into()
    };
    container(inner).width(16).height(16).center(16).style(move |_| iced::widget::container::Style { background: Some(Background::Color(t::DEEP)), border: style::border(edge, 1.0, 8.0), ..Default::default() }).into()
}

/// A tick box with its label, for *Find people's faces*.
fn tick<'a>(on: bool, label: &'a str, msg: Message) -> Element<'a, Message> {
    let mark: Element<'a, Message> = if on { icons::view(Glyph::Check, 12.0, t::ON_SIGNAL) } else { Space::new().into() };
    let bx = container(mark)
        .width(16)
        .height(16)
        .center(16)
        .style(move |_| iced::widget::container::Style { background: Some(Background::Color(if on { t::SIGNAL } else { t::DEEP })), border: style::border(if on { iced::Color::TRANSPARENT } else { t::LINE_HI }, 1.0, 3.0), ..Default::default() });
    button(row![bx, kit::heading(label)].spacing(8).align_y(Alignment::Center)).padding(0).style(|_t, _s| button::Style::default()).on_press(msg).into()
}

/// One face model the user can pick: its name, size, how it did and how
/// long it takes here, its licence in plain words and where it comes from
/// (a restrictive licence in the warning tone).
fn face_row<'a>(app: &App, mi: &models::ModelInfo, on: bool, enabled: bool, installed: bool, catch_up: bool) -> Element<'a, Message> {
    let st = &app.search.status;
    let shots = st.library_shots.max(st.indexed_shots);
    let device = st.device.chosen;
    let secs = if catch_up { estimate::face_catch_up_seconds(shots, device, mi.id) } else { estimate::face_seconds(shots, device, mi.id) };
    let speed = format!("{} for this library {} {}.", capital(&estimate::describe_seconds(secs)), if device == Device::Gpu { "with" } else { "on" }, device.phrase());
    let ink = |c: iced::Color| if enabled { c } else { alpha(c, 0.45) };
    let mut top = row![text(mi.name).size(t::T_BODY).font(paint::MEDIUM).color(ink(t::INK))].spacing(8).align_y(Alignment::Center);
    if mi.recommended {
        top = top.push(text("Recommended").size(t::T_SMALL).color(ink(t::SIGNAL)));
    }
    top = top.push(Space::new().width(Length::Fill));
    top = top.push(text(if installed { "Downloaded".to_string() } else { size(mi.bytes()) }).size(t::T_SMALL).color(ink(t::INK2)));
    let licence = format!("Licence: {}: {}. From {}, its publisher.", capital(mi.licence), mi.licence_plain, mi.publisher);
    let what = mi.what.split_whitespace().collect::<Vec<_>>().join(" ");
    let col = column![
        top,
        text(format!("{what} {speed}")).size(t::T_SMALL).color(ink(t::INK2)),
        text(licence).size(t::T_SMALL).color(ink(if mi.restricted { t::WARN } else { t::INK3 })),
    ]
    .spacing(2)
    .width(Length::Fill);
    let r = row![radio_dot(on, enabled), col].spacing(t::S3).align_y(Alignment::Start);
    let id = mi.id;
    button(r)
        .width(Length::Fill)
        .padding(Padding::from([7.0, 12.0]))
        .style(move |_t, status| {
            let hot = enabled && matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style { background: (on && enabled || hot).then_some(Background::Color(t::RAISED)), text_color: t::INK, border: Border { radius: t::R_CONTROL.into(), ..Border::default() }, ..button::Style::default() }
        })
        .on_press_maybe(enabled.then_some(d(DialogMsg::FaceModel(id))))
        .into()
}

fn ask_view<'a>(app: &'a App, dl: &'a Dialog) -> Element<'a, Message> {
    let st = &app.search.status;
    let faces_st = &st.faces;
    let c = costs(app);
    let ready = st.models_installed;
    let faces = (dl.faces || dl.faces_only) && !dl.fursuits_only;
    let model = models::info(dl.face_model);
    // What *Download* fetches: the same list its rows will show, and the
    // fursuit-head models unless they are in place.
    let heads_st = &st.heads;
    let heads = dl.fursuits || dl.fursuits_only;
    let heads_bytes = if heads { models::bytes_for(&fursuits::missing_models(st)) } else { 0 };
    let total = models::bytes_for(&missing_models(st, !dl.faces_only && !dl.fursuits_only, faces.then_some(dl.face_model))) + heads_bytes;
    let title = match (dl.fursuits_only, dl.faces_only, total > 0) {
        (true, _, true) => "Download the fursuit-head models and find heads?",
        (true, _, false) => "Find fursuit heads in the library?",
        (false, true, true) => "Download a face model and find faces?",
        (false, true, false) => "Find faces in the library?",
        (false, false, true) => "Download the models and index the library?",
        (false, false, false) => "Index the library?",
    };
    let lede = if dl.fursuits_only {
        "Fursuit heads are found on this computer, with models downloaded once from their publishers. Nothing about your photos is sent anywhere."
    } else if dl.faces_only {
        "Faces are found on this computer, with a model downloaded once from its publisher. Nothing about your photos is sent anywhere."
    } else {
        "Search, faces and fursuit heads are found on this computer, with models downloaded once from their publishers. Nothing about your photos is sent anywhere."
    };
    let mut col = column![dialogs::header(title, None), kit::secondary(lede)].spacing(t::S2 + 2.0);
    // Search's rows, under their role.
    if !dl.faces_only && !dl.fursuits_only {
        let mut role = "";
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for mi in models::search_models() {
            if mi.role != role {
                if !rows.is_empty() {
                    col = col.push(group(std::mem::take(&mut rows)));
                }
                role = mi.role;
                col = col.push(row![kit::heading(mi.role), kit::caption(if mi.required { "Always used." } else { "" })].spacing(t::S2).align_y(Alignment::Center));
            }
            rows.push(model_row(mi, ready));
        }
        if !rows.is_empty() {
            col = col.push(group(rows));
        }
    }
    // Faces: on unless unticked, with the model picked.
    let finder = models::info(models::FACE_DETECTOR);
    if !dl.fursuits_only {
        let head: Element<'_, Message> = if dl.faces_only { kit::heading("Faces").into() } else { tick(dl.faces, "Find people's faces", d(DialogMsg::Faces)) };
        col = col.push(row![head, kit::caption("Groups faces alike so you can name people. Pick the model:")].spacing(t::S2).align_y(Alignment::Center));
        let rows: Vec<Element<'_, Message>> = models::face_models()
            .map(|mi| face_row(app, mi, mi.id == dl.face_model, faces, face_ready(st, mi.id), dl.faces_only))
            .collect();
        let mut list = column![].spacing(2).padding(2);
        for r in rows {
            list = list.push(r);
        }
        col = col.push(container(list).width(Length::Fill).style(style::group()));
        col = col.push(kit::caption(format!(
            "{} ({}, {}, from {}) finds the faces for whichever model you pick, so changing the model later keeps the faces and their names. Nothing is named for you.",
            finder.name,
            size(finder.bytes()),
            finder.licence,
            finder.publisher
        )));
    }
    // Fursuit heads: on unless unticked (unticked to start with on the
    // processor), one row for the two models they need.
    if !dl.faces_only {
        let note = match (dl.fursuits_only, st.device.chosen) {
            (true, _) => "Only on the photos the index says show fursuits.",
            (false, Device::Gpu) => "Scanned during indexing, after search and faces, on the photos with fursuits.",
            (false, Device::Cpu) => "Long on the processor, so each scan asks first, with its cost.",
        };
        let head: Element<'_, Message> = if dl.fursuits_only { kit::heading("Fursuit heads").into() } else { tick(dl.fursuits, "Find fursuit heads", d(DialogMsg::Fursuits)) };
        col = col.push(row![head, kit::caption(note)].spacing(t::S2).align_y(Alignment::Center));
        col = col.push(group(models::fursuit_models().map(|mi| heads_row(mi, heads, heads_st.models_installed)).collect()));
        col = col.push(kit::caption(heads_cost(app)));
    }
    let runs = match (st.device.chosen, &st.device.name) {
        (Device::Gpu, Some(name)) => format!("The graphics card ({name}), checked against the processor first"),
        (Device::Gpu, None) => "The graphics card, checked against the processor first".to_string(),
        (Device::Cpu, _) => "The processor".to_string(),
    };
    let device = st.device.chosen;
    let shots = st.library_shots.max(st.indexed_shots);
    let mut facts = Vec::new();
    if total > 0 {
        facts.push(fact("Downloads", format!("{} in all, once", size(total)), None));
    }
    let head_photos = fursuits::photos_to_look_at(heads_st, shots);
    let read = if dl.fursuits_only {
        let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
        format!(
            "The previews of the {} photos with fursuits again: {} from the library{}",
            crate::util::thousands(head_photos as u64),
            estimate::describe_bytes(fursuits::head_bytes_read(head_photos)),
            if on_share { " on the share" } else { "" }
        )
    } else {
        capital(&c.read)
    };
    facts.push(fact("Reads", read, None));
    facts.push(fact("Runs on", runs, Some("Preferences ▸ Search chooses the graphics card or the processor.")));
    let takes = if dl.fursuits_only {
        format!("{}. Groups appear on the Fursuits tab as they are found.", capital(&fursuits::describe_cost(head_photos, device)))
    } else if dl.faces_only {
        let todo = st.indexed_shots.saturating_sub(faces_st.looked_at);
        format!("{} {} {}. Groups appear on the People tab as they are found.", capital(&estimate::describe_seconds(estimate::face_catch_up_seconds(todo, device, dl.face_model))), if device == Device::Gpu { "with" } else { "on" }, device.phrase())
    } else if faces {
        let secs = estimate::seconds(shots, device) + estimate::face_seconds(shots, device, dl.face_model);
        format!("{} for search and faces {} {}. Search works on the newest days within a minute.", capital(&estimate::describe_seconds(secs)), if device == Device::Gpu { "with" } else { "on" }, device.phrase())
    } else {
        format!("{}. Search works on the newest days within a minute.", c.takes)
    };
    // Then the heads, after search (and faces); on the processor, asked.
    let takes = match (dl.fursuits && !dl.fursuits_only, device) {
        (true, Device::Gpu) => format!("{takes} Then {} for fursuit heads.", estimate::describe_seconds(fursuits::head_seconds(head_photos, device))),
        (true, Device::Cpu) => format!("{takes} Fursuit heads then take {}, and that asks first.", fursuits::describe_cost(head_photos, device)),
        _ => takes,
    };
    facts.push(fact("Takes", takes, None));
    col = col.push(group(facts));
    let sums_link = button(text(if dl.sums { "Hide sources and checksums" } else { "Show sources and checksums" }).size(t::T_SMALL))
        .padding(Padding::from([0.0, 4.0]))
        .style(style::link().style())
        .on_press(d(DialogMsg::Sums));
    let dir = models::models_dir().map(|p| tilde(&p)).unwrap_or_default();
    col = col.push(
        row![
            kit::caption(format!("Each file is checked against its SHA-256 before it's used, and deleted if it doesn't match. Saved in {dir}.")).width(Length::Fill),
            sums_link,
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center),
    );
    if dl.sums {
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        if !dl.faces_only && !dl.fursuits_only {
            for mi in models::search_models() {
                for f in mi.files {
                    rows.push(
                        column![
                            text(format!("{}: {}", mi.name, f.file)).size(t::T_SMALL).color(t::INK),
                            text(f.url).size(t::T_SMALL).color(t::INK3),
                            text(format!("SHA-256 {}", f.sha256)).size(t::T_SMALL).color(t::INK3),
                        ]
                        .spacing(1)
                        .padding(Padding::from([6.0, 12.0]))
                        .into(),
                    );
                }
            }
        }
        if faces {
            for mi in [finder, model] {
                for f in mi.files {
                    rows.push(
                        column![
                            text(format!("{}: {}", mi.name, f.file)).size(t::T_SMALL).color(t::INK),
                            text(f.url).size(t::T_SMALL).color(t::INK3),
                            text(format!("SHA-256 {}", f.sha256)).size(t::T_SMALL).color(t::INK3),
                        ]
                        .spacing(1)
                        .padding(Padding::from([6.0, 12.0]))
                        .into(),
                    );
                }
            }
        }
        if heads {
            for mi in models::fursuit_models() {
                for f in mi.files {
                    rows.push(
                        column![
                            text(format!("{}: {}", mi.name, f.file)).size(t::T_SMALL).color(t::INK),
                            text(f.url).size(t::T_SMALL).color(t::INK3),
                            text(format!("SHA-256 {}", f.sha256)).size(t::T_SMALL).color(t::INK3),
                        ]
                        .spacing(1)
                        .padding(Padding::from([6.0, 12.0]))
                        .into(),
                    );
                }
            }
        }
        col = col.push(group(rows));
    }
    let go = match (dl.fursuits_only, dl.faces_only, total > 0) {
        (true, _, true) => format!("Download {} and find heads", size(total)),
        (true, _, false) => "Find fursuit heads".to_string(),
        (false, true, true) => format!("Download {} and find faces", size(total)),
        (false, true, false) => "Find faces".to_string(),
        (false, false, true) => format!("Download {} and index", size(total)),
        (false, false, false) => "Index the library".to_string(),
    };
    col = col.push(Space::new().height(t::S1));
    col = col.push(dialogs::footer(
        vec![],
        vec![kit::btn("Cancel", style::secondary(), Some(d(DialogMsg::Cancel))).into(), kit::btn(go, style::primary(), Some(d(DialogMsg::Download))).into()],
    ));
    // A gutter, so the scroll bar never covers the rows (the fursuit row
    // makes the dialog scroll at ordinary window heights).
    iced::widget::scrollable(col.padding(Padding { right: t::S3, ..Padding::ZERO })).style(style::scroll).into()
}

/// Every download listed is a close-call judge's.
pub fn judge_downloads(rows: &[Download]) -> bool {
    !rows.is_empty() && rows.iter().all(|x| models::info(x.model).choice == Some("judge"))
}

/// Download a close-call judge: its row, what it does and costs, where it
/// runs (the card only), and that it only suggests.
fn judge_ask_view<'a>(app: &'a App, dl: &'a Dialog) -> Element<'a, Message> {
    let st = &app.search.status;
    let Some(mi) = dl.judge.and_then(|c| c.model()).map(models::info) else { return Space::new().into() };
    let mut col = column![
        dialogs::header("Download the close-call judge?", None),
        kit::secondary(
            "When Find more or the \u{201c}Same fursuit?\u{201d} review has photos too close to call, the judge looks at the two heads side by side and offers those it thinks are the same fursuit. It only suggests: you still answer each with Y or N, and nothing is named for you."
        ),
    ]
    .spacing(t::S2 + 2.0);
    col = col.push(row![kit::heading(mi.role), kit::caption("Fursuits only, never people.")].spacing(t::S2).align_y(Alignment::Center));
    col = col.push(group(vec![model_row(mi, false)]));
    let card = match &st.device.name {
        Some(name) => format!("The graphics card ({name}), all of it on the card"),
        None => "The graphics card, all of it on the card".to_string(),
    };
    let choice = dl.judge.unwrap_or_default();
    let secs = super::fursuits::call_seconds_words(super::fursuits::judge_call_seconds(choice));
    let facts = vec![
        fact("Downloads", format!("{}, once", size(mi.bytes())), None),
        fact("Runs on", card, Some("Never the processor. If it doesn't fit on the card, or the card fails, checking stops and says why.")),
        fact(
            "Takes",
            format!(
                "{} a close call ({} for 100), only while Find more or a review has some.",
                capital(&secs),
                crate::search::estimate::describe_seconds(100.0 * super::fursuits::judge_call_seconds(choice))
            ),
            Some("It waits while indexing or the fursuit-head step is using the card, and lets the card go after two quiet minutes."),
        ),
        fact("Keeps", format!("{} in the models folder. Preferences ▸ Storage deletes it.", size(mi.kept_bytes())), None),
    ];
    col = col.push(group(facts));
    let sums_link = button(text(if dl.sums { "Hide sources and checksums" } else { "Show sources and checksums" }).size(t::T_SMALL))
        .padding(Padding::from([0.0, 4.0]))
        .style(style::link().style())
        .on_press(d(DialogMsg::Sums));
    let dir = models::models_dir().map(|p| tilde(&p)).unwrap_or_default();
    col = col.push(
        row![kit::caption(format!("Each file is checked against its SHA-256 before it's used, and deleted if it doesn't match. Saved in {dir}.")).width(Length::Fill), sums_link]
            .spacing(t::S2)
            .align_y(Alignment::Center),
    );
    if dl.sums {
        let rows = mi
            .files
            .iter()
            .map(|f| {
                column![
                    text(format!("{}: {}", mi.name, f.file)).size(t::T_SMALL).color(t::INK),
                    text(f.url).size(t::T_SMALL).color(t::INK3),
                    text(format!("SHA-256 {}", f.sha256)).size(t::T_SMALL).color(t::INK3),
                ]
                .spacing(1)
                .padding(Padding::from([6.0, 12.0]))
                .into()
            })
            .collect();
        col = col.push(group(rows));
    }
    col = col.push(Space::new().height(t::S1));
    col = col.push(dialogs::footer(
        vec![],
        vec![
            kit::btn("Cancel", style::secondary(), Some(d(DialogMsg::Cancel))).into(),
            kit::btn(format!("Download {} and use it", size(mi.bytes())), style::primary(), Some(d(DialogMsg::Download))).into(),
        ],
    ));
    iced::widget::scrollable(col.padding(Padding { right: t::S3, ..Padding::ZERO })).style(style::scroll).into()
}

/// One of the fursuit-head models' rows: its name, size, what it does, the
/// licence in plain words and the publisher.  Dimmed while unticked.
fn heads_row<'a>(mi: &models::ModelInfo, on: bool, installed: bool) -> Element<'a, Message> {
    let ink = |c: iced::Color| if on { c } else { alpha(c, 0.45) };
    let top = row![
        text(mi.name).size(t::T_BODY).font(paint::MEDIUM).color(ink(t::INK)),
        Space::new().width(Length::Fill),
        text(if installed { "Downloaded".to_string() } else { size(mi.bytes()) }).size(t::T_SMALL).color(ink(t::INK2)),
    ]
    .align_y(Alignment::Center);
    let col = column![
        top,
        text(mi.what).size(t::T_SMALL).color(ink(t::INK2)),
        text(format!("Licence: {}, {}. From {}.", mi.licence, mi.licence_plain, mi.publisher)).size(t::T_SMALL).color(ink(t::INK3)),
    ]
    .spacing(2);
    container(col).padding(Padding::from([7.0, 12.0])).width(Length::Fill).into()
}

/// What looking for heads costs here, under the head models' rows: the
/// library's fursuit photos, then an event day.
fn heads_cost(app: &App) -> String {
    let st = &app.search.status;
    let device = st.device.chosen;
    let photos = fursuits::library_photos(&st.heads, st.library_shots.max(st.indexed_shots));
    let day = estimate::describe_seconds(fursuits::head_seconds(fursuits::EVENT_DAY_PHOTOS, device));
    match device {
        Device::Gpu => format!("{} for this library, then {day} a new event day.", capital(&fursuits::describe_cost(photos, device))),
        Device::Cpu => format!("{} for this library, {day} an event day.", capital(&fursuits::describe_cost(photos, device))),
    }
}

fn progress_view(app: &App) -> Element<'_, Message> {
    let s = &app.search;
    let name_of = |id: ModelId| {
        let mi = models::info(id);
        (mi.role, mi.name)
    };
    if let Some(x) = s.downloads.iter().find(|x| failed(&x.state)) {
        let (role, name) = name_of(x.model);
        let (title, body, digests): (String, String, Option<(&str, &str)>) = match &x.state {
            DownloadState::Mismatch { expected, got } => (
                format!("The {} model didn't match its checksum", role.to_lowercase()),
                format!(
                    "{name} downloaded ({}), but the file isn't the one Photo Pipeline expects, so it was deleted and nothing was installed. This happens when a download is cut short or changed on the way.{}",
                    size(x.total),
                    if judge_downloads(&s.downloads) { "" } else { " Nothing was indexed." }
                ),
                Some((expected, got)),
            ),
            DownloadState::Offline => {
                let what = if judge_downloads(&s.downloads) { "Nothing was downloaded." } else { "Nothing was downloaded or indexed." };
                ("Couldn't reach the model's publisher".to_string(), format!("{what} Try again when you're online."), None)
            }
            DownloadState::Failed(why) => ("The download didn't finish".to_string(), format!("{} Nothing was {}.", capital(why.trim_end_matches('.')) + ".", if judge_downloads(&s.downloads) { "installed" } else { "indexed" }), None),
            _ => (String::new(), String::new(), None),
        };
        let mut words = column![kit::title(title), kit::secondary(body)].spacing(t::S2).width(Length::Fill);
        if let Some((e, g)) = digests {
            words = words.push(group(vec![fact("Expected", e.to_string(), None), fact("Got", g.to_string(), None)]));
        }
        let col = column![
            row![icons::view(Glyph::Warning, 20.0, t::WARN), words].spacing(t::S3),
            Space::new().height(t::S1),
            dialogs::footer(
                vec![],
                vec![kit::btn("Not now", style::secondary(), Some(d(DialogMsg::Close))).into(), kit::btn("Try again", style::primary(), Some(d(DialogMsg::Retry))).into()],
            ),
        ]
        .spacing(t::S3);
        return col.into();
    }
    let done = !s.downloads.is_empty() && s.downloads.iter().all(|x| x.state == DownloadState::Checked);
    let several = s.downloads.len() > 1;
    let judge = judge_downloads(&s.downloads);
    let (title, body) = match (done, several) {
        (true, _) if judge => ("Close-call judge ready", "Downloaded and checked. Find more and the \u{201c}Same fursuit?\u{201d} review use it from now on."),
        (false, _) if judge => ("Downloading the close-call judge", "It is used as soon as it's in place. You can close this: the download carries on, with its progress in the status bar."),
        (true, false) => ("Search model ready", "Downloaded and checked. Indexing is running; the status bar shows how far it has got."),
        (true, true) => ("Models ready", "Downloaded and checked. Indexing is running; the status bar shows how far it has got."),
        (false, false) => ("Downloading the search model", "Indexing starts as soon as the search model is in place. You can close this: the download carries on, with its progress in the status bar."),
        (false, true) => ("Downloading the models", "Indexing starts as soon as the models are in place. You can close this: the downloads carry on, with their progress in the status bar."),
    };
    let reading = s.speed_now();
    let rows = s
        .downloads
        .iter()
        .map(|x| {
            let (role, name) = name_of(x.model);
            let frac = if x.total == 0 { 0.0 } else { x.done as f32 / x.total as f32 };
            let state: Element<'_, Message> = match &x.state {
                DownloadState::Checked => row![icons::view(Glyph::Check, 14.0, t::KEEP), text("Checked").size(t::T_SMALL).color(t::KEEP)].spacing(4).align_y(Alignment::Center).into(),
                DownloadState::Checking => text("Checking…").size(t::T_SMALL).color(t::INK2).into(),
                DownloadState::Waiting => text("Waiting").size(t::T_SMALL).color(t::INK3).into(),
                DownloadState::Cancelled => text("Stopped").size(t::T_SMALL).color(t::INK3).into(),
                DownloadState::Downloading => text(speed::with_speed(progress_bytes(x.done, x.total), reading)).size(t::T_SMALL).color(t::INK2).into(),
                _ => text(progress_bytes(x.done, x.total)).size(t::T_SMALL).color(t::INK2).into(),
            };
            row![
                text(format!("{role}: {name}")).size(t::T_BODY).color(t::INK).width(190),
                bar(frac, None),
                container(state).width(170).align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(t::S3)
            .align_y(Alignment::Center)
            .padding(Padding::from([10.0, 12.0]))
            .into()
        })
        .collect();
    let buttons: Vec<Element<'_, Message>> = if done || !s.downloading() {
        vec![kit::btn("Close", style::primary(), Some(d(DialogMsg::Close))).into()]
    } else {
        vec![
            kit::btn("Stop the download", style::secondary(), Some(d(DialogMsg::StopDownload))).into(),
            kit::btn("Close", style::secondary(), Some(d(DialogMsg::Close))).into(),
        ]
    };
    // How long is left (or that it is starting, or waits for the network),
    // left of the buttons.
    let left: Vec<Element<'_, Message>> = reading.map(|r| speed_note(r, remaining(&s.downloads), true)).into_iter().collect();
    column![dialogs::header(title, None), kit::secondary(body), group(rows), Space::new().height(t::S1), dialogs::footer(left, buttons)].spacing(t::S3).into()
}

/// Bytes still to come down, over every row.
pub fn remaining(rows: &[Download]) -> u64 {
    rows.iter().map(|x| x.total.saturating_sub(x.done)).sum()
}

/// The time left, "starting…" or "waiting for the network", in the
/// progress's quiet small type (the dialog's starts with a capital).
fn speed_note<'a>(r: speed::Reading, remaining: u64, sentence: bool) -> Element<'a, Message> {
    let words = speed::note(r, remaining);
    let tone = if r == speed::Reading::Stalled { t::INK2 } else { t::INK3 };
    text(if sentence { capital(&words) } else { words }).size(t::T_SMALL).color(tone).into()
}

/// A thin progress bar.  `width` None fills the row.
fn bar<'a>(frac: f32, width: Option<f32>) -> Element<'a, Message> {
    let b = progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0)).girth(4).style(|_| progress_bar::Style {
        background: Background::Color(t::RAISED),
        bar: Background::Color(t::FLOW),
        border: Border { radius: 2.0.into(), ..Border::default() },
    });
    match width {
        Some(w) => b.length(w).into(),
        None => b.into(),
    }
}

// ── the status bar ──────────────────────────────────────────────────────────

/// Downloads and indexing on the status bar's right, in any view: the
/// first index with × to stop and a click for its steps; new exports
/// quietly; a stop with the way back.
pub fn status_slot(app: &App) -> Option<Element<'_, Message>> {
    let s = &app.search;
    let st = &s.status;
    let words = |w: String| text(w).size(t::T_SMALL).color(t::INK3);
    let stop = |hint: &'static str| {
        tip(
            button(container(icons::view(Glyph::Close, 11.0, t::INK2)).center(Length::Fill))
                .width(20)
                .height(20)
                .padding(0)
                .style(style::ghost().radius(4.0).style())
                .on_press(m(Msg::Stop)),
            hint,
            false,
        )
    };
    let link = |label: &'static str, msg: Msg| button(text(label).size(t::T_SMALL).font(paint::MEDIUM)).padding(Padding::from([2.0, 6.0])).style(style::link().style()).on_press(m(msg));
    let slot: Element<'_, Message> = if s.downloading() {
        let (done, total) = s.downloads.iter().fold((0u64, 0u64), |(a, b), x| (a + x.done, b + x.total));
        let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        let reading = s.speed_now();
        let mut content = row![
            words(if judge_downloads(&s.downloads) {
                "Downloading the close-call judge".into()
            } else if s.downloads.len() > 1 {
                "Downloading the models".into()
            } else {
                "Downloading the search model".into()
            }),
            text(speed::with_speed(progress_bytes(done, total), reading)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
            bar(frac, Some(56.0)),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center);
        if let Some(r) = reading {
            content = content.push(speed_note(r, remaining(&s.downloads), false));
        }
        let b = button(content).padding(Padding::from([0.0, 6.0])).style(style::ghost().style()).on_press(d(DialogMsg::Show));
        row![tip(b, "Show the download", false), stop("Stop the download. Nothing is indexed until it's done.")].spacing(t::S1).align_y(Alignment::Center).into()
    } else if let Some(why) = &st.stopped {
        match why {
            Stopped::LibraryAway { .. } if st.indexed_shots < st.library_shots => {
                words(format!("Indexing paused: {} of {}", util::thousands(st.indexed_shots as u64), util::thousands(st.library_shots as u64))).into()
            }
            Stopped::LibraryAway { .. } => words("Indexing paused: the library is away".into()).into(),
            // Before the first index is asked for, the search page says it.
            Stopped::Card(CardFailure::NoCard) if !st.first_index_asked && st.indexed_shots == 0 => return None,
            Stopped::Card(f) => {
                let mut r = row![text("Indexing stopped: graphics card").size(t::T_SMALL).color(t::WARN)].spacing(t::S2).align_y(Alignment::Center);
                if st.waiting > 0 {
                    r = r.push(words(format!("{} new {} wait for it", util::thousands(st.waiting as u64), if st.waiting == 1 { "photo" } else { "photos" })));
                }
                if s.banner_hidden {
                    r = r.push(link(retry_label(f), Msg::TryCard));
                    r = r.push(link("Preferences", Msg::OpenPrefs));
                }
                r.into()
            }
            Stopped::Helper { .. } => {
                let mut r = row![text("Indexing stopped: the search helper couldn't run").size(t::T_SMALL).color(t::WARN)].spacing(t::S2).align_y(Alignment::Center);
                if s.banner_hidden {
                    r = r.push(link("Try again", Msg::TryCard));
                }
                r.into()
            }
            Stopped::Cancelled => return None,
        }
    } else if let Some(p) = &st.pass {
        // New exports count their own photos; the rest, the library's.
        let (done, total) = if p.kind == PassKind::NewExports { new_photos(p, s.pass_start) } else { (p.done, p.total) };
        let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        let of = format!("{} of {}", util::thousands(done as u64), util::thousands(total as u64));
        match p.kind {
            PassKind::NewExports => {
                // Quiet: no ×, a grey bar.
                let b = progress_bar(0.0..=1.0, frac).length(48).girth(3).style(|_| progress_bar::Style {
                    background: Background::Color(t::RAISED),
                    bar: Background::Color(t::INK3),
                    border: Border { radius: 2.0.into(), ..Border::default() },
                });
                row![words("Adding new photos to search".into()), text(of).size(t::T_SMALL).color(t::INK2), b].spacing(t::S2).align_y(Alignment::Center).into()
            }
            kind => {
                let label = match kind {
                    PassKind::Rebuild => "Rebuilding the search index",
                    PassKind::Faces if st.faces.unrecognised > 0 => "Recognising faces again",
                    PassKind::Faces => "Finding faces",
                    _ => "Indexing",
                };
                let content =
                    row![words(label.into()), text(of).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK), bar(frac, Some(56.0))].spacing(t::S2).align_y(Alignment::Center);
                let b = button(content).padding(Padding::from([0.0, 6.0])).style(style::ghost().style()).on_press(m(Msg::Steps(!s.steps)));
                row![tip(b, "Show the steps", false), stop("Stop indexing. What is indexed so far is kept, and search works on it.")].spacing(t::S1).align_y(Alignment::Center).into()
            }
        }
    } else if st.waiting > 0 {
        words(format!("{} new {} wait to be indexed", util::thousands(st.waiting as u64), if st.waiting == 1 { "photo" } else { "photos" })).into()
    } else {
        return None;
    };
    Some(row![slot, Space::new().width(t::S3), kit::vrule(12.0), Space::new().width(t::S3)].align_y(Alignment::Center).into())
}

/// What retrying the card is called, by what failed.  Every one tries the
/// card again; none offers the processor.
pub fn retry_label(f: &CardFailure) -> &'static str {
    match f {
        CardFailure::SelfCheckFailed { .. } => "Check the card again",
        CardFailure::NoCard => "Look for the card again",
        CardFailure::Crashed | CardFailure::Failed(_) => "Try the card again",
    }
}

/// Where the running pass began.  The engine counts every pass against the
/// whole library (`done` starts at the shots already indexed), so a pass of
/// new exports is measured from here: its own photos on the quiet line, and
/// what search gained when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassStart {
    pub kind: PassKind,
    /// The pass's `done` when it first showed.
    pub done: usize,
    /// Shots indexed then.
    pub indexed: usize,
}

/// The start of the pass `st` shows: kept while that pass runs, taken
/// afresh when another begins, none when none runs.
pub fn pass_start(was: Option<PassStart>, st: &Status) -> Option<PassStart> {
    let p = st.pass.as_ref()?;
    match was {
        Some(s) if s.kind == p.kind && s.done <= p.done => Some(s),
        _ => Some(PassStart { kind: p.kind, done: p.done, indexed: st.indexed_shots }),
    }
}

/// A pass of new exports in its own photos: (done, of).
pub fn new_photos(p: &PassProgress, start: Option<PassStart>) -> (usize, usize) {
    let from = start.filter(|s| s.kind == p.kind).map_or(0, |s| s.done.min(p.done));
    (p.done - from, p.total.saturating_sub(from))
}

/// What the status bar says when a pass ends of itself.  `None` for a pass
/// of new exports that added nothing (files that can't be read): nothing
/// to say, and nothing for the results to show again.
pub fn pass_ended(p: &PassProgress, start: Option<PassStart>, st: &Status) -> Option<String> {
    match p.kind {
        PassKind::NewExports => {
            let added = start.map_or(0, |s| st.indexed_shots.saturating_sub(s.indexed));
            (added > 0).then(|| format!("Search has the {} new {}.", util::thousands(added as u64), if added == 1 { "photo" } else { "photos" }))
        }
        _ if st.indexed_shots >= st.library_shots => Some(format!("Indexed {}. Search has the whole library.", util::plural(st.library_shots as u64, "photo"))),
        _ => Some(format!("Indexed {} of {} photos.", util::thousands(st.indexed_shots as u64), util::thousands(st.library_shots as u64))),
    }
}

/// The indexing steps, over the status bar's right (a click on the
/// progress).  Phase 1 has one: search.
pub fn steps_layer(app: &App) -> Option<Element<'_, Message>> {
    let s = &app.search;
    let p = s.status.pass.as_ref().filter(|p| p.kind != PassKind::NewExports && s.steps)?;
    let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
    let left = estimate::describe_seconds(estimate::seconds(p.total.saturating_sub(p.done), s.status.device.chosen));
    let detail = format!("{} of {} photos, newest first. {} left.", util::thousands(p.done as u64), util::thousands(p.total as u64), capital(&left));
    // The first index finds faces in the same pass; a faces pass of its
    // own is the one step.
    let (glyph, what) = match p.kind {
        PassKind::Faces => (Glyph::Person, if s.status.faces.unrecognised > 0 { "Faces, with the new model" } else { "Faces" }),
        PassKind::FirstIndex if s.status.faces.on => (Glyph::Search, "Search and faces"),
        _ => (Glyph::Search, "Search"),
    };
    let step = row![
        icons::view(glyph, 16.0, t::INK3),
        column![text(what).size(t::T_BODY).color(t::INK), text(detail).size(t::T_SMALL).color(t::INK3)].spacing(1).width(Length::Fill),
        bar(frac, Some(80.0)),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center)
    .padding(Padding::from([6.0, 10.0]));
    let panel = column![
        container(kit::heading("Indexing the library")).padding(Padding { top: t::S2, bottom: 2.0, left: 10.0, right: 10.0 }),
        step,
        container(kit::hairline()).padding(Padding::from([2.0, 0.0])),
        container(text("Search works on whatever is done.").size(t::T_SMALL).color(t::INK3)).padding(Padding { top: 4.0, bottom: t::S2, left: 10.0, right: 10.0 }),
    ];
    let panel = crate::ui::motion::Appear::new(container(panel).width(380).padding(t::S1).style(style::floating())).rise(6.0);
    let catcher = opaque(mouse_area(container(space()).width(Length::Fill).height(Length::Fill)).on_press(m(Msg::Steps(false))));
    let placed = container(opaque(panel))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding { right: 372.0, bottom: 34.0, top: 0.0, left: 0.0 });
    Some(stack![catcher, placed].width(Length::Fill).height(Length::Fill).into())
}

// ── the banner ──────────────────────────────────────────────────────────────

/// Indexing is stopped in a way the banner explains.  Before the first
/// index is asked for, a missing card is the search page's to say.
pub fn banner_shown(app: &App) -> bool {
    let s = &app.search;
    let st = &s.status;
    match &st.stopped {
        _ if s.banner_hidden => false,
        Some(Stopped::Card(CardFailure::NoCard)) => st.first_index_asked || st.indexed_shots > 0,
        Some(Stopped::Card(_) | Stopped::LibraryAway { .. } | Stopped::Helper { .. }) => true,
        _ => false,
    }
}

/// Under the folder bar, in whichever view is open, while indexing is
/// stopped: what failed, what still works, and what to do.
pub fn banner(app: &App) -> Option<Element<'_, Message>> {
    if !banner_shown(app) {
        return None;
    }
    let st = &app.search.status;
    let n = util::thousands(st.indexed_shots as u64);
    let (glyph, head, detail, buttons): (Glyph, String, String, Vec<Element<'_, Message>>) = match st.stopped.as_ref()? {
        Stopped::LibraryAway { share, .. } => (
            Glyph::Warning,
            format!("Indexing stopped: the library on {share} stopped answering"),
            format!(
                "{} of {} photos were indexed and are kept, and search works on those. Indexing carries on from there when the library is back.",
                n,
                util::thousands(st.library_shots as u64)
            ),
            vec![kit::btn("Try again", style::secondary(), Some(m(Msg::TryAgain))).into()],
        ),
        Stopped::Card(f) => {
            let works = if st.indexed_shots > 0 { format!(" Search still works on the {n} photos indexed so far.") } else { String::new() };
            let waiting = if st.waiting > 0 { format!(" {} new photos wait for it.", util::thousands(st.waiting as u64)) } else { String::new() };
            (
                Glyph::GraphicsCard,
                f.title().to_string(),
                format!(
                    "{}{works}{waiting} Nothing is moved to the processor: to index on it instead, choose it in Preferences ▸ Search.",
                    f.detail().trim()
                ),
                vec![
                    kit::btn(retry_label(f), style::secondary(), Some(m(Msg::TryCard))).into(),
                    kit::btn("Preferences", style::ghost(), Some(m(Msg::OpenPrefs))).into(),
                ],
            )
        }
        Stopped::Helper { message } => {
            let works = if st.indexed_shots > 0 { format!(" Search still works on the {n} photos indexed so far.") } else { String::new() };
            (
                Glyph::Warning,
                "Indexing stopped: the search helper couldn't run".to_string(),
                format!("{}{works}", capital(message.trim())),
                vec![kit::btn("Try again", style::secondary(), Some(m(Msg::TryCard))).into()],
            )
        }
        Stopped::Cancelled => return None,
    };
    let words = column![text(head).size(t::T_BODY).font(paint::MEDIUM).color(t::INK), text(detail).size(t::T_SMALL).color(t::INK2)].spacing(2).width(Length::Fill);
    let dismiss = tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(m(Msg::HideBanner))), "Hide this message (the status bar keeps the way back)", true);
    let mut content =
        row![icons::view(glyph, 18.0, t::WARN), words].spacing(t::S3).align_y(Alignment::Center).padding(Padding { top: t::S2, bottom: t::S2, left: 12.0, right: 4.0 });
    for b in buttons {
        content = content.push(b);
    }
    content = content.push(dismiss);
    let strip = container(content).width(Length::Fill).style(style::fill(alpha(t::WARN, 0.09)));
    Some(column![strip, container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::WARN, 0.35)))].into())
}

/// `~/…` for a path in the home folder.
fn tilde(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(std::path::Path::to_path_buf)) {
        Some(rel) => format!("~/{}", rel.display()),
        None => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_is_retried_never_worked_around() {
        // Every way the card can fail is retried on the card; none offers
        // the processor.
        for f in [CardFailure::Crashed, CardFailure::SelfCheckFailed { cosine: 0.4, max_diff: 0.9 }, CardFailure::NoCard, CardFailure::Failed("x".into())] {
            let label = retry_label(&f).to_lowercase();
            assert!(label.contains("card"), "{label}");
            assert!(!label.contains("processor") && !label.contains("cpu"), "{label}");
        }
    }

    fn status(pass: Option<(PassKind, usize, usize)>, indexed: usize, library: usize) -> Status {
        Status {
            pass: pass.map(|(kind, done, total)| PassProgress { kind, done, total, oldest_day: None }),
            indexed_shots: indexed,
            library_shots: library,
            first_index_asked: true,
            ..Status::default()
        }
    }

    /// Feed snapshots in order, as `on_status` does: the start is followed
    /// and each pass that ends gives its note (or none).
    fn notes(snapshots: &[Status]) -> Vec<Option<String>> {
        let (mut start, mut old): (Option<PassStart>, Option<&Status>) = (None, None);
        let mut out = Vec::new();
        for st in snapshots {
            let was = start;
            start = pass_start(was, st);
            if let (Some(p), None) = (old.and_then(|o| o.pass.as_ref()), &st.pass) {
                out.push(pass_ended(p, was, st));
            }
            old = Some(st);
        }
        out
    }

    #[test]
    fn a_pass_of_new_exports_counts_only_its_own_photos() {
        // The engine counts against the library: 44,878 already indexed,
        // 59 to go.  The quiet line says 0 of 59, then 22 of 59.
        let first = status(Some((PassKind::NewExports, 44_878, 44_937)), 44_792, 44_937);
        let start = pass_start(None, &first);
        assert_eq!(new_photos(first.pass.as_ref().unwrap(), start), (0, 59));
        let later = status(Some((PassKind::NewExports, 44_900, 44_937)), 44_800, 44_937);
        let start = pass_start(start, &later);
        assert_eq!(start.map(|s| (s.done, s.indexed)), Some((44_878, 44_792)), "kept while the pass runs");
        assert_eq!(new_photos(later.pass.as_ref().unwrap(), start), (22, 59));
        // Unknown start (the stand-in counts from 0): as given.
        assert_eq!(new_photos(later.pass.as_ref().unwrap(), None), (44_900, 44_937));
    }

    #[test]
    fn a_pass_that_adds_nothing_says_nothing_and_one_that_adds_says_how_many() {
        let lib = 127;
        // Files that can't be read, tried again and again: no note, ever
        // (it used to say "Search has the 127 new photos." each time).
        let mut stuck = Vec::new();
        for _ in 0..3 {
            stuck.push(status(Some((PassKind::NewExports, 126, lib)), 126, lib));
            stuck.push(status(None, 126, lib));
        }
        assert_eq!(notes(&stuck), vec![None, None, None]);
        // A real export: what search gained, not the library's size.
        let added = [status(Some((PassKind::NewExports, 124, lib)), 124, lib), status(Some((PassKind::NewExports, 126, lib)), 126, lib), status(None, 127, lib)];
        assert_eq!(notes(&added), vec![Some("Search has the 3 new photos.".to_string())]);
        let one = [status(Some((PassKind::NewExports, 126, lib)), 126, lib), status(None, 127, lib)];
        assert_eq!(notes(&one), vec![Some("Search has the 1 new photo.".to_string())]);
        // The first index still says where it got to.
        let first = [status(Some((PassKind::FirstIndex, 0, lib)), 0, lib), status(None, 127, lib)];
        assert_eq!(notes(&first), vec![Some("Indexed 127 photos. Search has the whole library.".to_string())]);
    }

    #[test]
    fn exact_sizes_drop_the_about() {
        assert_eq!(size(785_356_945), "785 MB");
        assert_eq!(size(971_000_000), "971 MB");
        assert!(!size(models::bytes_for(&[ModelId::Siglip2])).starts_with("about"));
        assert_eq!(capital("about 7 minutes with the graphics card"), "About 7 minutes with the graphics card");
    }

    /// Nothing indexed; `search` and `faces` say which models are in
    /// place (the faces' for AuraFace, the engine's chosen model).
    fn models_here(search: bool, faces: bool) -> Status {
        let mut st = status(None, 0, 634);
        st.first_index_asked = false;
        st.models_installed = search;
        st.faces = crate::search::FacesStatus { on: true, model: Some(ModelId::AuraFace), installed: faces, ..Default::default() };
        st
    }

    fn rows(b: &Begin) -> Vec<ModelId> {
        match b {
            Begin::Index => Vec::new(),
            Begin::Download(rows) => rows.iter().map(|d| d.model).collect(),
        }
    }

    #[test]
    fn with_every_model_in_place_the_dialog_closes_and_indexing_starts() {
        // The do-not-break pass: all three models here, faces ticked.  The
        // rows used to list them all, waiting for reports that never came.
        let st = models_here(true, true);
        assert_eq!(begin_first_index(&st, Some(ModelId::AuraFace)), Begin::Index);
        assert_eq!(begin_first_index(&st, None), Begin::Index);
        assert!(missing_models(&st, true, Some(ModelId::AuraFace)).is_empty(), "the dialog quotes nothing to download");
    }

    #[test]
    fn with_search_in_place_only_the_face_rows_show_and_the_total_is_theirs() {
        // The upgrade: search's model here, the face models not.
        let st = models_here(true, false);
        let begin = begin_first_index(&st, Some(ModelId::AuraFace));
        assert_eq!(rows(&begin), vec![models::FACE_DETECTOR, ModelId::AuraFace]);
        let Begin::Download(list) = begin else { unreachable!() };
        let total: u64 = list.iter().map(|d| d.total).sum();
        assert_eq!(total, models::bytes_for(&models::face_set(ModelId::AuraFace)));
        assert!(list.iter().all(|d| d.state == DownloadState::Waiting && d.done == 0));
        // The dialog's quoted total is the same list's.
        assert_eq!(models::bytes_for(&missing_models(&st, true, Some(ModelId::AuraFace))), total);
        // Faces unticked: nothing to fetch, indexing starts.
        assert_eq!(begin_first_index(&st, None), Begin::Index);
        // Nothing here: all three.
        let none = models_here(false, false);
        assert_eq!(rows(&begin_first_index(&none, Some(ModelId::AuraFace))), vec![ModelId::Siglip2, models::FACE_DETECTOR, ModelId::AuraFace]);
        assert_eq!(rows(&begin_first_index(&none, None)), vec![ModelId::Siglip2]);
    }

    #[test]
    fn another_face_model_than_the_one_in_place_is_fetched() {
        // AuraFace's set is in place; SFace picked: its set is listed (the
        // status can't tell the detector apart), and the catch-up's
        // faces-only total leaves search out.
        let st = models_here(true, true);
        assert!(face_ready(&st, ModelId::AuraFace) && !face_ready(&st, ModelId::SFace));
        assert_eq!(missing_models(&st, false, Some(ModelId::SFace)), models::face_set(ModelId::SFace).to_vec());
        assert!(missing_models(&models_here(false, true), false, Some(ModelId::AuraFace)).is_empty());
    }

    #[test]
    fn a_row_whose_model_is_in_place_stops_waiting() {
        // Listed from an older status: the detector's row waits while
        // SFace downloads; once the status has SFace's set in place, it
        // is done and nothing is downloading any more.
        let mut st = models_here(true, false);
        let Begin::Download(mut list) = begin_first_index(&st, Some(ModelId::SFace)) else { panic!("rows expected") };
        record_download(&mut list, Download { model: ModelId::SFace, done: 39, total: 39, state: DownloadState::Checked });
        settle_downloads(&mut list, &st);
        assert_eq!(list[0].state, DownloadState::Waiting, "not in place yet");
        st.faces.model = Some(ModelId::SFace);
        st.faces.installed = true;
        settle_downloads(&mut list, &st);
        assert!(list.iter().all(|d| d.state == DownloadState::Checked && d.done == d.total), "{list:?}");
        // A row that is downloading is left to its own reports.
        let mut busy = vec![Download { model: ModelId::Siglip2, done: 5, total: 9, state: DownloadState::Downloading }];
        settle_downloads(&mut busy, &models_here(true, true));
        assert_eq!(busy[0].state, DownloadState::Downloading);
    }

    #[test]
    fn a_new_download_after_finished_ones_starts_a_fresh_list() {
        // The first index's rows, all done; then a face model change
        // fetches SFace: the status bar's total is SFace's alone.
        let mut list = vec![
            Download { model: ModelId::Siglip2, done: 9, total: 9, state: DownloadState::Checked },
            Download { model: ModelId::AuraFace, done: 7, total: 7, state: DownloadState::Checked },
        ];
        record_download(&mut list, Download { model: ModelId::SFace, done: 0, total: 4, state: DownloadState::Downloading });
        assert_eq!(list.iter().map(|d| d.model).collect::<Vec<_>>(), vec![ModelId::SFace]);
        // While it runs, later reports update its row.
        record_download(&mut list, Download { model: ModelId::SFace, done: 4, total: 4, state: DownloadState::Checked });
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, DownloadState::Checked);
        // A queued row joins a running list rather than replacing it.
        let mut running = vec![Download { model: ModelId::Yunet, done: 1, total: 2, state: DownloadState::Downloading }];
        record_download(&mut running, Download { model: ModelId::AuraFace, done: 0, total: 7, state: DownloadState::Waiting });
        assert_eq!(running.len(), 2);
    }
}
