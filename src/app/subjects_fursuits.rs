//! The Fursuits tab's own states, and the head step wherever it shows
//! (decision 11, §7a.4–7a.5).  Groups, naming, the review, merging,
//! splitting and hiding are the subjects pages' own, on this kind's data;
//! this module supplies what only fursuits have:
//!
//! - **The catch-up offer.**  A library indexed before heads were looked
//!   for (or with scans skipped) says what finding them costs, on the
//!   device chosen, and starts only when asked.
//! - **Asking first on the processor, every time.**  A head scan that would
//!   start (the first index's step, an export's photos, carrying on after a
//!   stop) waits with its cost: a strip under the folder bar in any view,
//!   and a card on the tab, with *Find fursuit heads* and *Skip this time*.
//! - **Progress** on the tab and in the status bar, with × to stop.
//! - **A failed card** says what failed and offers to try the card again
//!   or open Preferences; never the processor.
//! - **Heads to update** (fursuit matching v2): heads found before fursuits
//!   were told apart by the search model need its vectors once.  The tab
//!   and Preferences say what that costs; it runs only when asked.

use iced::widget::{Space, button, column, container, progress_bar, row, scrollable, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Task};

use super::fursuits::{self, HeadsStatus, Offer, Step, Why};
use super::kit::{self, tip};
use super::people::Kind;
use super::style;
use super::subjects::{self as subj};
use super::{App, Message, View};
use crate::ml::Device;
use crate::search::estimate;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme::{self as t, alpha};
use crate::util;

pub fn m(msg: Msg) -> Message {
    Message::Fursuits(msg)
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Look for heads in the fursuit photos not looked at yet (the tab's
    /// or Preferences' offer, which states the cost): downloads first when
    /// the head models aren't in place.
    FindHeads,
    /// The answer to a scan waiting on the processor.
    Start,
    Skip,
    /// × in the status bar: stop the scan (what was found is kept).
    Stop,
    TryCard,
    OpenPrefs,
    IndexLibrary,
    /// Give the heads found before v2 the search model's vectors (the
    /// offer states the cost, so the click is the answer).
    UpdateHeads,
}

// ── state ───────────────────────────────────────────────────────────────────

/// What the Fursuits tab's body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NoLibrary,
    Reading,
    /// Nothing indexed: heads are found with the index.
    NotIndexed,
    /// *Find fursuit heads* is off.
    Off,
    /// Indexed, no head looked for yet: the offer with its cost.
    CatchUp,
    /// Heads found before v2, none with the search model's vectors yet, so
    /// nothing is grouped: the offer to update them, with its cost.
    Vectors,
    /// A scan waits for start or skip (the processor), nothing found yet.
    Asking,
    /// Looking (the first index, an export's photos, the catch-up), nothing
    /// grouped yet.
    Looking,
    /// The card failed, nothing grouped yet.
    Failed,
    /// Every fursuit photo looked at, no head found.
    NoHeads,
    Groups,
}

pub fn state(app: &App) -> State {
    let h = &app.search.status.heads;
    if app.library.is_none() {
        return State::NoLibrary;
    }
    if app.calendar.index.is_none() {
        return State::Reading;
    }
    if subj::has_groups(app, Kind::Fursuit) {
        return State::Groups;
    }
    let st = &app.search.status;
    if !h.on {
        return State::Off;
    }
    if h.vectors_missing > 0 && matches!(h.step, Step::Idle) {
        return State::Vectors;
    }
    match h.step {
        Step::Asking(_) => return State::Asking,
        Step::Scanning { .. } => return State::Looking,
        Step::Failed(_) => return State::Failed,
        Step::Idle => {}
    }
    if st.indexed_shots == 0 && st.pass.is_none() {
        return State::NotIndexed;
    }
    // The first index runs the head step after search: it is coming.
    if st.pass.is_some() {
        return State::Looking;
    }
    if h.catch_up_wanted() {
        return State::CatchUp;
    }
    State::NoHeads
}

/// The tab header's caption.
pub fn caption(app: &App) -> String {
    if app.search.status.heads.regrouping {
        return format!("Grouping the fursuits again ({}). Names and answers stay.", app.search.status.heads.grouping.label());
    }
    let s = &app.subjects;
    let a = subj::arrange(&s.all, Kind::Fursuit, app.settings.people_min_photos());
    let looking = app.search.status.heads.scanning();
    // Updating the heads found before v2 is not looking for new ones.
    if let Step::Scanning { why: Why::Vectors, .. } = app.search.status.heads.step {
        return match (a.named.len(), a.to_name.len()) {
            (0, 0) => "Fursuit heads are being updated".to_string(),
            (0, _) => "Fursuit heads are still being updated. Name the fursuits you want to find.".to_string(),
            (n, k) => format!("{n} named, {} to name", util::plural(k as u64, "group")),
        };
    }
    match (a.named.len(), a.to_name.len()) {
        (0, 0) if looking => "Fursuit heads are being found".to_string(),
        (0, 0) => String::new(),
        (0, _) if looking => "Fursuit heads are still being found. Name the fursuits you want to find.".to_string(),
        (0, _) => "Name the fursuits you want to find.".to_string(),
        (n, k) => format!("{n} named, {} to name", util::plural(k as u64, "group")),
    }
}

// ── words ───────────────────────────────────────────────────────────────────

/// "with the graphics card" / "on the processor".
fn on_device(d: Device) -> String {
    format!("{} {}", if d == Device::Gpu { "with" } else { "on" }, d.phrase())
}

/// "412 new photos with fursuits", "the 42,180 photos with fursuits".
fn photos_phrase(o: Offer) -> String {
    let n = util::thousands(o.photos as u64);
    let photos = if o.photos == 1 { "photo" } else { "photos" };
    match o.why {
        Why::NewExports => format!("the {n} new {photos} with fursuits"),
        Why::Resume => format!("the {n} {photos} with fursuits still to look at"),
        Why::FirstIndex | Why::CatchUp | Why::Vectors => format!("the {n} {photos} with fursuits"),
    }
}

/// The processor's question: a title and what it costs.
pub fn ask_words(o: Offer) -> (String, String) {
    let cost = fursuits::describe_cost(o.photos, Device::Cpu);
    let title = match o.why {
        Why::Resume => format!("Carry on finding fursuit heads in {}?", photos_phrase(o)),
        _ => format!("Find fursuit heads in {}?", photos_phrase(o)),
    };
    let before = match o.why {
        Why::FirstIndex => "The library is indexed for search. ",
        Why::NewExports => "Search already has them. ",
        Why::CatchUp | Why::Resume | Why::Vectors => "",
    };
    (
        title,
        format!("{before}This takes {cost}, and you can keep working. Skipped photos wait on the Fursuits tab, to look at later."),
    )
}

// ── the tab ─────────────────────────────────────────────────────────────────

/// The Fursuits tab's body.
pub fn body(app: &App) -> Element<'_, Message> {
    let h = &app.search.status.heads;
    match state(app) {
        State::NoLibrary => subj::message(
            Glyph::Library,
            "Choose your library folder",
            "Fursuits are found in the library, the folder Export files photos into by date.".into(),
            Some(("Choose library folder…", Message::PickFolder(super::FolderTarget::Library))),
            None,
        ),
        State::Reading => subj::message(Glyph::Fursuit, "Reading the library…", String::new(), None, None),
        State::NotIndexed => subj::message(
            Glyph::Fursuit,
            "Fursuits are found when the library is indexed",
            "Indexing the library for search can look for fursuit heads too, on the photos with fursuits, so you can name the fursuits you want to find and search for them. Nothing is named for you.".into(),
            Some(("Index the library…", m(Msg::IndexLibrary))),
            None,
        ),
        State::Off => subj::message(
            Glyph::Fursuit,
            "Finding fursuit heads is off",
            "Turn it on in Preferences ▸ Fursuits to look for the fursuits in your photos.".into(),
            Some(("Open Preferences", m(Msg::OpenPrefs))),
            None,
        ),
        State::CatchUp => catch_up(app),
        State::Vectors => update_page(app),
        State::Asking | State::Looking | State::Failed => {
            let note = match state(app) {
                State::Asking => "Nothing is looked at until you say so.",
                State::Failed => "What was found so far is kept.",
                _ => "Groups appear here as they are found, newest days first. You can keep culling meanwhile.",
            };
            let mut col = column![].width(Length::Fill);
            if let Some(card) = status_card(app) {
                col = col.push(card);
            }
            let (title, note) = match h.step {
                Step::Scanning { why: Why::Vectors, .. } => ("Updating the fursuit heads", "Groups appear here as their heads are updated, newest days first. You can keep culling meanwhile."),
                _ if h.scanning() => ("Looking for fursuit heads", note),
                _ => ("No fursuit heads yet", note),
            };
            col = col.push(subj::later(Glyph::Fursuit, title, note));
            container(col).width(Length::Fill).height(Length::Fill).style(style::fill(t::PHOTO_GROUND)).into()
        }
        State::NoHeads => subj::message(
            Glyph::Fursuit,
            "No fursuit heads found yet",
            format!(
                "None of the {} photos with fursuits showed a head the detector could find. New exports are looked at as they are added.",
                util::thousands(h.tagged as u64)
            ),
            None,
            None,
        ),
        State::Groups => subj::groups(app),
    }
}

/// The library was indexed before heads were looked for: what finding them
/// costs, on the device chosen, and the way to start it.  Nothing runs
/// until asked.
fn catch_up(app: &App) -> Element<'_, Message> {
    let h = &app.search.status.heads;
    let st = &app.search.status;
    let device = st.device.chosen;
    let photos = h.unscanned();
    let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
    let mut facts = vec![
        subj::fact("Looks at", format!("The {} photos the index says show fursuits", util::thousands(photos as u64)), None),
        subj::fact(
            "Reads",
            format!("Each of their previews again: {} from the library{}", estimate::describe_bytes(fursuits::head_bytes_read(photos)), if on_share { " on the share" } else { "" }),
            None,
        ),
        subj::fact(
            "Takes",
            super::search_setup::capital(&fursuits::describe_cost(photos, device)),
            Some(match device {
                Device::Gpu => "You can keep culling, and stop it from the status bar. Groups appear as they are found, newest days first.".into(),
                Device::Cpu => "The processor is chosen in Preferences ▸ Search. You can keep culling, and stop it from the status bar.".into(),
            }),
        ),
    ];
    if !h.models_installed {
        facts.push(subj::fact(
            "Downloads",
            format!("{}, once: {}", fursuits::models_name(), super::search_setup::size(crate::ml::models::bytes_for(&fursuits::missing_models(&app.search.status)))),
            Some("The next step lists them with their licences.".into()),
        ));
    }
    facts.push(subj::fact("Keeps", "The heads found and the names you give, on this computer, readable only by you.".into(), None));
    let mut group = column![];
    for (i, f) in facts.into_iter().enumerate() {
        if i > 0 {
            group = group.push(kit::hairline());
        }
        group = group.push(f);
    }
    let go = if h.models_installed { "Find fursuit heads" } else { "Find fursuit heads…" };
    let action = row![kit::btn(go, style::primary(), Some(m(Msg::FindHeads))), kit::caption(format!("{} with fursuits", util::plural(photos as u64, "photo")))]
        .spacing(t::S3)
        .align_y(Alignment::Center);
    let lede = if h.skipped > 0 && h.scanned > 0 {
        "Some photos with fursuits weren't looked at for heads: their scan was skipped. Finding them groups the heads alike, so you can name the fursuits you want to find. Nothing is named for you."
    } else {
        "The library was indexed before fursuit heads were looked for. Finding them groups the heads alike, so you can name the fursuits you want to find and search for them. Nothing is named for you."
    };
    let col = column![
        row![icons::view(Glyph::Fursuit, 26.0, t::INK), text("Find the fursuits in your photos").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK)].spacing(t::S3).align_y(Alignment::Center),
        text(lede).size(t::T_BODY).color(t::INK2),
        container(group).width(Length::Fill).style(style::group()),
        text(if device == Device::Cpu {
            "After this, each export's photos with fursuits are offered as they are added, with what looking at them costs on the processor."
        } else {
            "After this, heads in new exports are found as they are added. Finding fursuit heads can be turned off in Preferences ▸ Fursuits."
        })
        .size(t::T_SMALL)
        .color(t::INK3),
        action,
    ]
    .spacing(14)
    .max_width(600);
    container(scrollable(container(col).center_x(Length::Fill).padding(Padding { top: 40.0, bottom: t::S5, left: t::S5, right: t::S5 })).style(style::scroll))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::GROUND))
        .into()
}

/// Heads found before v2 and none updated yet: why they need the search
/// model's vectors, what that costs on the device chosen, and the way to
/// start it.  Nothing runs until asked.
fn update_page(app: &App) -> Element<'_, Message> {
    let h = &app.search.status.heads;
    let device = app.search.status.device.chosen;
    let n = h.vectors_missing;
    let on_share = app.library.as_deref().and_then(|l| super::network::share_of(app, l)).is_some();
    let facts = vec![
        subj::fact("Looks at", format!("The {} photos where fursuit heads were found, at the heads already found", util::thousands(n as u64)), None),
        subj::fact(
            "Reads",
            format!("Each of their previews again: {} from the library{}", estimate::describe_bytes(fursuits::vector_bytes_read(n)), if on_share { " on the share" } else { "" }),
            None,
        ),
        subj::fact(
            "Takes",
            super::search_setup::capital(&fursuits::describe_vector_cost(n, device)),
            Some("You can keep culling, and stop it from the status bar. Groups appear as their heads are updated, newest days first.".into()),
        ),
        subj::fact("Downloads", "Nothing: it uses the search model, already here.".into(), None),
        subj::fact("Keeps", "The names you gave and your answers: they point at the heads, not at the groups.".into(), None),
    ];
    let mut group = column![];
    for (i, f) in facts.into_iter().enumerate() {
        if i > 0 {
            group = group.push(kit::hairline());
        }
        group = group.push(f);
    }
    let action = row![kit::btn("Update the fursuit heads", style::primary(), Some(m(Msg::UpdateHeads))), kit::caption(format!("{} with fursuit heads", util::plural(n as u64, "photo")))]
        .spacing(t::S3)
        .align_y(Alignment::Center);
    let col = column![
        row![icons::view(Glyph::Fursuit, 26.0, t::INK), text("Update the fursuit heads").size(t::T_HEAD).font(paint::SEMIBOLD).color(t::INK)].spacing(t::S3).align_y(Alignment::Center),
        text("Fursuits are now told apart by the search model, which finds the same fursuit across days and years more often. The heads found before need it once, so their previews are read again. Until then they aren't grouped or suggested. Nothing is named for you.")
            .size(t::T_BODY)
            .color(t::INK2),
        container(group).width(Length::Fill).style(style::group()),
        action,
    ]
    .spacing(14)
    .max_width(600);
    container(scrollable(container(col).center_x(Length::Fill).padding(Padding { top: 40.0, bottom: t::S5, left: t::S5, right: t::S5 })).style(style::scroll))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::fill(t::GROUND))
        .into()
}

/// A card on the photos' grey, outlined, as the faces' progress is.
fn card<'a>(glyph: Glyph, tone: iced::Color, title: String, detail: String, right: Element<'a, Message>) -> Element<'a, Message> {
    let words = column![text(title).size(t::T_BODY).font(paint::MEDIUM).color(t::PHOTO_INK), text(detail).size(t::T_SMALL).color(t::PHOTO_INK2)].spacing(2).width(Length::Fill);
    let r = row![icons::view(glyph, 20.0, tone), words, right].spacing(14).align_y(Alignment::Center);
    let boxed = container(r)
        .padding(Padding::from([12.0, 14.0]))
        .max_width(820)
        .style(|_| container::Style { border: style::border(t::PHOTO_LINE, 1.0, t::R_CONTROL), ..Default::default() });
    container(boxed).padding(Padding { top: 16.0, left: 16.0, right: 16.0, bottom: 4.0 }).into()
}

fn bar<'a>(frac: f32, w: f32, ground: iced::Color) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0))
        .length(w)
        .girth(4)
        .style(move |_| progress_bar::Style { background: Background::Color(ground), bar: Background::Color(t::SIGNAL), border: Border { radius: 2.0.into(), ..Border::default() } })
        .into()
}

/// Over the groups (or alone): a scan's progress, the processor's question,
/// a failed card, or photos still to look at.
pub fn status_card(app: &App) -> Option<Element<'_, Message>> {
    let h = &app.search.status.heads;
    let device = app.search.status.device.chosen;
    match &h.step {
        Step::Scanning { why: Why::Vectors, done, total } => {
            let left = estimate::describe_seconds(fursuits::vector_seconds(total.saturating_sub(*done), device));
            let frac = if *total == 0 { 0.0 } else { *done as f32 / *total as f32 };
            Some(card(
                Glyph::Fursuit,
                t::PHOTO_INK2,
                format!("Updating fursuit heads: {} of {}", util::thousands(*done as u64), util::plural(*total as u64, "photo")),
                format!("{} left {}. Their groups and suggestions come back as they're updated.", super::search_setup::capital(&left), on_device(device)),
                bar(frac, 120.0, t::PHOTO_RAISED),
            ))
        }
        Step::Scanning { done, total, .. } => {
            let left = estimate::describe_seconds(fursuits::head_seconds(total.saturating_sub(*done), device));
            let frac = if *total == 0 { 0.0 } else { *done as f32 / *total as f32 };
            Some(card(
                Glyph::Fursuit,
                t::PHOTO_INK2,
                format!("Finding fursuit heads: {} of {}", util::thousands(*done as u64), util::plural(*total as u64, "photo")),
                format!("{} left {}. Groups appear as they are found, newest days first.", super::search_setup::capital(&left), on_device(device)),
                bar(frac, 120.0, t::PHOTO_RAISED),
            ))
        }
        Step::Asking(o) => {
            let (title, detail) = ask_words(*o);
            let buttons = row![
                kit::btn("Skip this time", style::secondary(), Some(m(Msg::Skip))),
                kit::btn("Find fursuit heads", style::primary(), Some(m(Msg::Start))),
            ]
            .spacing(t::S2);
            Some(card(Glyph::Fursuit, t::PHOTO_INK2, title, detail, buttons.into()))
        }
        // The banner over every view says it already; the tab says it once
        // the banner is put away.
        Step::Failed(_) if super::search_setup::banner_shown(app) => None,
        Step::Failed(f) => {
            let buttons = row![
                kit::btn(super::search_setup::retry_label(f), style::secondary(), Some(m(Msg::TryCard))),
                kit::btn("Preferences", style::ghost(), Some(m(Msg::OpenPrefs))),
            ]
            .spacing(t::S2);
            Some(card(
                Glyph::GraphicsCard,
                t::WARN,
                f.title().replacen("Indexing stopped", "Finding fursuit heads stopped", 1),
                format!("{} What was found so far is kept. Nothing is moved to the processor: to look for heads on it instead, choose it in Preferences ▸ Search.", f.detail().trim()),
                buttons.into(),
            ))
        }
        Step::Idle if h.vectors_missing > 0 => {
            let n = h.vectors_missing;
            let go = kit::btn("Update them", style::secondary(), Some(m(Msg::UpdateHeads)));
            Some(card(
                Glyph::Fursuit,
                t::PHOTO_INK3,
                format!("{} with fursuit heads to update", util::plural(n as u64, "photo")),
                format!(
                    "They were found before fursuits were told apart by the search model, so they aren't grouped or suggested yet. Updating them reads their previews again: {}.",
                    fursuits::describe_vector_cost(n, device)
                ),
                go.into(),
            ))
        }
        Step::Idle if h.on && h.unscanned() > 0 && h.scanned > 0 => {
            let n = h.unscanned();
            let go = kit::btn("Find their heads", style::secondary(), Some(m(Msg::FindHeads)));
            Some(card(
                Glyph::Fursuit,
                t::PHOTO_INK3,
                format!("{} with fursuits not looked at yet", util::plural(n as u64, "photo")),
                format!("Their scan was skipped. Looking at them takes {}.", fursuits::describe_cost(n, device)),
                go.into(),
            ))
        }
        Step::Idle => None,
    }
}

// ── everywhere else ─────────────────────────────────────────────────────────

/// Under the folder bar in any view while a scan waits for the user (the
/// processor is chosen): what it would look at, what it costs, start or
/// skip.  The Fursuits tab shows the same question as a card instead.
pub fn ask_strip(app: &App) -> Option<Element<'_, Message>> {
    let o = app.search.status.heads.asking()?;
    if app.view == View::Subjects && app.subjects.tab == Kind::Fursuit && app.subjects.page() == subj::Page::Overview {
        return None;
    }
    let (title, detail) = ask_words(o);
    let words = column![text(title).size(t::T_BODY).font(paint::MEDIUM).color(t::INK), text(detail).size(t::T_SMALL).color(t::INK2)].spacing(2).width(Length::Fill);
    let content = row![
        icons::view(Glyph::Fursuit, 18.0, t::INK2),
        words,
        kit::btn("Skip this time", style::secondary(), Some(m(Msg::Skip))),
        kit::btn("Find fursuit heads", style::primary(), Some(m(Msg::Start))),
    ]
    .spacing(t::S3)
    .align_y(Alignment::Center)
    .padding(Padding { top: t::S2, bottom: t::S2, left: 12.0, right: 8.0 });
    let strip = container(content).width(Length::Fill).style(style::fill(alpha(t::INK, 0.035)));
    Some(column![strip, kit::hairline()].into())
}

/// The status bar while heads are looked for on their own (an export's
/// photos, the catch-up): how far, with × to stop.  During the first index
/// the search line covers it, and its steps list heads.
pub fn status_slot(app: &App) -> Option<Element<'_, Message>> {
    let h = &app.search.status.heads;
    let Step::Scanning { why, done, total } = h.step else { return None };
    if app.search.status.pass.is_some() {
        return None;
    }
    let of = format!("{} of {}", util::thousands(done as u64), util::thousands(total as u64));
    let label = match why {
        Why::NewExports => "Finding fursuit heads in them",
        Why::Vectors => "Updating fursuit heads",
        _ => "Finding fursuit heads",
    };
    let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
    let content = row![text(label).size(t::T_SMALL).color(t::INK3), text(of).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK), bar(frac, 56.0, t::RAISED)].spacing(t::S2).align_y(Alignment::Center);
    let stop = tip(
        button(container(icons::view(Glyph::Close, 11.0, t::INK2)).center(Length::Fill)).width(20).height(20).padding(0).style(style::ghost().radius(4.0).style()).on_press(m(Msg::Stop)),
        if why == Why::Vectors { "Stop updating fursuit heads. Those updated are kept." } else { "Stop looking for fursuit heads. What was found is kept." },
        false,
    );
    Some(row![content, stop, Space::new().width(t::S3), kit::vrule(12.0), Space::new().width(t::S3)].spacing(t::S1).align_y(Alignment::Center).into())
}

// ── update ──────────────────────────────────────────────────────────────────

/// The search status changed: say when a head scan has finished (not when
/// it was stopped).
pub fn on_status(app: &mut App, old: &crate::search::Status) -> Task<Message> {
    let (was, now) = (&old.heads, &app.search.status.heads);
    if let (Step::Scanning { why: Why::Vectors, .. }, Step::Idle) = (&was.step, &now.step) {
        if now.vectors_missing == 0 {
            return app.show_status("Updated the fursuit heads: fursuits are told apart by the search model now.", 8000);
        }
        return Task::none();
    }
    if let (Step::Scanning { done, total, .. }, Step::Idle) = (&was.step, &now.step) {
        // Finished, not stopped: everything it set out to look at.
        if now.scanned >= was.scanned + total.saturating_sub(*done) {
            let text = format!("Finished looking for fursuit heads: {} found in all.", util::thousands(now.found as u64));
            return app.show_status(text, 8000);
        }
    }
    Task::none()
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::FindHeads => find_heads(app),
        Msg::Start => {
            let Some(o) = app.search.status.heads.asking() else { return Task::none() };
            app.search.link.answer_heads(true);
            app.show_status(format!("Finding fursuit heads in {}. You can keep working.", photos_phrase(o)), 6000)
        }
        Msg::Skip => {
            let Some(o) = app.search.status.heads.asking() else { return Task::none() };
            app.search.link.answer_heads(false);
            app.show_status(format!("Skipped. {} wait on the Fursuits tab, to look at later.", super::search_setup::capital(&photos_phrase(o))), 6000)
        }
        Msg::Stop => {
            let updating = matches!(app.search.status.heads.step, Step::Scanning { why: Why::Vectors, .. });
            app.search.link.stop_heads();
            app.show_status(if updating { "Stopped updating fursuit heads. Those updated are kept." } else { "Stopped looking for fursuit heads. What was found is kept." }, 5000)
        }
        Msg::TryCard => {
            app.search.link.try_card_again();
            Task::none()
        }
        Msg::OpenPrefs => super::preferences::open_at(app, super::preferences::Control::FindHeads),
        Msg::UpdateHeads => {
            let h = &app.search.status.heads;
            if h.vectors_missing == 0 || !matches!(h.step, Step::Idle) {
                return Task::none();
            }
            let n = h.vectors_missing;
            let device = app.search.status.device.chosen;
            app.search.link.catch_up_head_vectors();
            app.show_status(format!("Updating the fursuit heads in {}: {}.", util::plural(n as u64, "photo"), fursuits::describe_vector_cost(n, device)), 6000)
        }
        Msg::IndexLibrary => {
            let t = super::search::open(app);
            Task::batch([t, Task::done(Message::Search(super::search::Msg::IndexLibrary))])
        }
    }
}

/// Look for heads in the fursuit photos not looked at: when the head models
/// are in place it runs now (the offer stated the cost, so the click is the
/// answer, on either device); else the download dialog asks first, with
/// only their row.
fn find_heads(app: &mut App) -> Task<Message> {
    let h: &HeadsStatus = &app.search.status.heads;
    if !h.models_installed {
        let mut d = super::search::Dialog::new(super::search::Stage::Ask, &app.settings);
        d.fursuits_only = true;
        d.fursuits = true;
        app.search.dialog = Some(d);
        return Task::none();
    }
    let n = h.unscanned();
    app.search.link.find_heads();
    let device = app.search.status.device.chosen;
    app.show_status(format!("Finding fursuit heads in {}: {}. Groups appear as they are found.", util::plural(n as u64, "photo"), fursuits::describe_cost(n, device)), 6000)
}

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let h = &app.search.status.heads;
    format!(
        "fursuits=[state={:?} on={} installed={} tagged={} scanned={} skipped={} found={} named={} step={:?} strip={} vectors_missing={} vectors={} whitened={}]",
        state(app),
        h.on,
        h.models_installed,
        h.tagged,
        h.scanned,
        h.skipped,
        h.found,
        h.named,
        h.step,
        ask_strip(app).is_some(),
        h.vectors_missing,
        h.vectors,
        h.whitened,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_processor_question_states_the_cost_and_never_the_card() {
        for why in [Why::FirstIndex, Why::NewExports, Why::CatchUp, Why::Resume] {
            let (title, detail) = ask_words(Offer { photos: 42_180, why });
            assert!(title.ends_with('?'), "{title}");
            assert!(title.contains("42,180"), "{title}");
            // The processor's cost, as the estimates measure it (hours, for
            // the library's fursuit photos).
            let cost = fursuits::describe_cost(42_180, Device::Cpu);
            assert!(cost.contains("hours on the processor"), "{cost}");
            assert!(detail.contains(&cost), "{detail}");
            assert!(!detail.to_lowercase().contains("graphics card"), "the question never offers the card as a way round: {detail}");
        }
        let (title, detail) = ask_words(Offer { photos: 412, why: Why::NewExports });
        assert_eq!(title, "Find fursuit heads in the 412 new photos with fursuits?");
        assert!(detail.starts_with(&format!("Search already has them. This takes {}", fursuits::describe_cost(412, Device::Cpu))), "{detail}");
        let (title, _) = ask_words(Offer { photos: 1, why: Why::NewExports });
        assert_eq!(title, "Find fursuit heads in the 1 new photo with fursuits?");
    }
}
