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

use super::kit::{self, tip};
use super::search::{Dialog, DialogMsg, Download, Msg, Stage, search_models};
use super::style;
use super::{App, Message, dialogs};
use crate::ml::download::DownloadState;
use crate::ml::models::{self, ModelId};
use crate::ml::{CardFailure, Device};
use crate::search::{PassKind, Stopped, estimate};
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
        DialogMsg::Download | DialogMsg::Retry => {
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
            let ids = models::models_for(face);
            s.link.set_find_faces(faces);
            if face.is_some() {
                s.link.set_face_model(model);
            }
            if s.status.models_installed && face.is_none() {
                s.dialog = None;
                s.link.start_first_index(&ids);
                return app.show_status("Indexing the library. Search works on the newest days within a minute.", 6000);
            }
            // The rows show at once, waiting, until the engine reports.
            s.downloads = ids.iter().map(|&id| Download { model: id, done: 0, total: models::info(id).bytes(), state: DownloadState::Waiting }).collect();
            if let Some(d) = s.dialog.as_mut() {
                d.stage = Stage::Progress;
            }
            s.link.start_first_index(&ids);
            Task::none()
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
            app.show_status("Stopped the download. Nothing was indexed.", 5000)
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
    let faces = dl.faces || dl.faces_only;
    let model = models::info(dl.face_model);
    // The face models are fetched unless the ones in place are the ones
    // picked.
    let face_ready = faces_st.installed && faces_st.model == Some(dl.face_model);
    let face_bytes = if faces && !face_ready { models::bytes_for(&models::face_set(dl.face_model)) } else { 0 };
    let search_bytes = if ready || dl.faces_only { 0 } else { models::bytes_for(&search_models()) };
    let total = search_bytes + face_bytes;
    let title = match (dl.faces_only, total > 0) {
        (true, true) => "Download a face model and find faces?",
        (true, false) => "Find faces in the library?",
        (false, true) => "Download the models and index the library?",
        (false, false) => "Index the library?",
    };
    let lede = if dl.faces_only {
        "Faces are found on this computer, with a model downloaded once from its publisher. Nothing about your photos is sent anywhere."
    } else {
        "Search and face finding run on this computer, with models downloaded once from their publishers. Nothing about your photos is sent anywhere."
    };
    let mut col = column![dialogs::header(title, None), kit::secondary(lede)].spacing(t::S2 + 2.0);
    // Search's rows, under their role.
    if !dl.faces_only {
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
    let head: Element<'_, Message> = if dl.faces_only { kit::heading("Faces").into() } else { tick(dl.faces, "Find people's faces", d(DialogMsg::Faces)) };
    col = col.push(row![head, kit::caption("Groups faces alike so you can name people. Pick the model:")].spacing(t::S2).align_y(Alignment::Center));
    let rows: Vec<Element<'_, Message>> = models::face_models()
        .map(|mi| face_row(app, mi, mi.id == dl.face_model, faces, faces_st.installed && faces_st.model == Some(mi.id), dl.faces_only))
        .collect();
    let mut list = column![].spacing(2).padding(2);
    for r in rows {
        list = list.push(r);
    }
    col = col.push(container(list).width(Length::Fill).style(style::group()));
    let finder = models::info(models::FACE_DETECTOR);
    col = col.push(kit::caption(format!(
        "{} ({}, {}, from {}) finds the faces for whichever model you pick, so changing the model later keeps the faces and their names. Nothing is named for you.",
        finder.name,
        size(finder.bytes()),
        finder.licence,
        finder.publisher
    )));
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
    facts.push(fact("Reads", capital(&c.read), None));
    facts.push(fact("Runs on", runs, Some("Preferences ▸ Search chooses the graphics card or the processor.")));
    let takes = if dl.faces_only {
        let todo = st.indexed_shots.saturating_sub(faces_st.looked_at);
        format!("{} {} {}. Groups appear on the People tab as they are found.", capital(&estimate::describe_seconds(estimate::face_catch_up_seconds(todo, device, dl.face_model))), if device == Device::Gpu { "with" } else { "on" }, device.phrase())
    } else if faces {
        let secs = estimate::seconds(shots, device) + estimate::face_seconds(shots, device, dl.face_model);
        format!("{} for search and faces {} {}. Search works on the newest days within a minute.", capital(&estimate::describe_seconds(secs)), if device == Device::Gpu { "with" } else { "on" }, device.phrase())
    } else {
        format!("{}. Search works on the newest days within a minute.", c.takes)
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
        if !dl.faces_only {
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
        col = col.push(group(rows));
    }
    let go = match (dl.faces_only, total > 0) {
        (true, true) => format!("Download {} and find faces", size(total)),
        (true, false) => "Find faces".to_string(),
        (false, true) => format!("Download {} and index", size(total)),
        (false, false) => "Index the library".to_string(),
    };
    col = col.push(Space::new().height(t::S1));
    col = col.push(dialogs::footer(
        vec![],
        vec![kit::btn("Cancel", style::secondary(), Some(d(DialogMsg::Cancel))).into(), kit::btn(go, style::primary(), Some(d(DialogMsg::Download))).into()],
    ));
    iced::widget::scrollable(col).style(style::scroll).into()
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
                    "{name} downloaded ({}), but the file isn't the one Photo Pipeline expects, so it was deleted and nothing was installed. This happens when a download is cut short or changed on the way. Nothing was indexed.",
                    size(x.total)
                ),
                Some((expected, got)),
            ),
            DownloadState::Offline => {
                ("Couldn't reach the model's publisher".to_string(), "Nothing was downloaded or indexed. Try again when you're online.".to_string(), None)
            }
            DownloadState::Failed(why) => ("The download didn't finish".to_string(), format!("{} Nothing was indexed.", capital(why.trim_end_matches('.')) + "."), None),
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
    let (title, body) = match (done, several) {
        (true, false) => ("Search model ready", "Downloaded and checked. Indexing is running; the status bar shows how far it has got."),
        (true, true) => ("Models ready", "Downloaded and checked. Indexing is running; the status bar shows how far it has got."),
        (false, false) => ("Downloading the search model", "Indexing starts as soon as the search model is in place. You can close this: the download carries on, with its progress in the status bar."),
        (false, true) => ("Downloading the models", "Indexing starts as soon as the models are in place. You can close this: the downloads carry on, with their progress in the status bar."),
    };
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
                _ => text(progress_bytes(x.done, x.total)).size(t::T_SMALL).color(t::INK2).into(),
            };
            row![
                text(format!("{role}: {name}")).size(t::T_BODY).color(t::INK).width(190),
                bar(frac, None),
                container(state).width(120).align_x(iced::alignment::Horizontal::Right),
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
    column![dialogs::header(title, None), kit::secondary(body), group(rows), Space::new().height(t::S1), dialogs::footer(vec![], buttons)].spacing(t::S3).into()
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
        let content = row![
            words(if s.downloads.len() > 1 { "Downloading the models".into() } else { "Downloading the search model".into() }),
            text(progress_bytes(done, total)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
            bar(frac, Some(56.0)),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center);
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
        let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
        let of = format!("{} of {}", util::thousands(p.done as u64), util::thousands(p.total as u64));
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

    #[test]
    fn exact_sizes_drop_the_about() {
        assert_eq!(size(785_356_945), "785 MB");
        assert_eq!(size(971_000_000), "971 MB");
        assert!(!size(models::bytes_for(&[ModelId::Siglip2])).starts_with("about"));
        assert_eq!(capital("about 7 minutes with the graphics card"), "About 7 minutes with the graphics card");
    }
}
