//! The analysis in the main window: the Hints control on the filter bar
//! and its menu (the hints filter, and what analysing does before it has
//! run), the pass's progress in the status bar with a way to stop it, the
//! summary when it ends, and the banner when it stopped because the folder
//! went away.
//!
//! Analysing only ever starts when asked (decision 2): from the menu, the
//! side rail, or the Hints menu.  What an earlier pass measured is shown as
//! soon as a folder opens (from the cache, reading no photo).

use iced::widget::{Space, button, column, container, mouse_area, opaque, pin, progress_bar, row, space, stack, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Point, Task};

use super::kit::{self, tip};
use super::style::{self, Look};
use super::{App, FilterMsg, Message, Show, quality};
use crate::analysis::Mode;
use crate::model::hints::HintFilter;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme::{self as t, alpha};
use crate::util;

#[derive(Debug, Clone)]
pub enum Msg {
    /// Open or close the Hints menu.
    Menu(bool),
    /// Analyse this folder (the Hints menu's button, the banner's).
    Analyse,
    /// Stop the pass (what it measured is kept).
    Stop,
    /// Close the "analysing stopped" banner.
    HideBanner,
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Menu(open) => {
            app.quality.menu = open;
            Task::none()
        }
        Msg::Analyse => {
            app.quality.menu = false;
            app.run_cmd(super::commands::Cmd::Analyse)
        }
        Msg::Stop => {
            quality::cancel(app);
            Task::none()
        }
        Msg::HideBanner => {
            app.quality.banner_hidden = true;
            Task::none()
        }
    }
}

/// Start analysing, or say why not (the Analyse command).
pub fn analyse(app: &mut App) -> Task<Message> {
    app.quality.menu = false;
    if app.scanning {
        return app.show_status("The folder is still being read. Analyse it once the scan is done.", 5000);
    }
    if app.folder.is_none() || app.coll.is_empty() {
        return app.show_status("Open a folder of photos first (Ctrl+O).", 5000);
    }
    if app.quality.analysing() {
        return Task::none();
    }
    quality::start(app)
}

// ── words ───────────────────────────────────────────────────────────────────

pub fn filter_label(f: HintFilter) -> &'static str {
    match f {
        HintFilter::Problems => "Possible problems",
        HintFilter::Soft => "Out of focus",
        HintFilter::Accidental => "Accidental",
        HintFilter::Duplicates => "Duplicates",
        HintFilter::Sharpest => "Sharpest of each burst",
    }
}

fn filter_about(f: HintFilter) -> &'static str {
    match f {
        HintFilter::Problems => "Out of focus, accidental, or a copy",
        HintFilter::Soft => "Nothing sharp, or much softer than its burst",
        HintFilter::Accidental => "Almost black, white or one tone",
        HintFilter::Duplicates => "Files in the folder twice, both copies",
        HintFilter::Sharpest => "The sharpest frame of every burst",
    }
}

fn filter_glyph(f: HintFilter) -> Glyph {
    match f {
        HintFilter::Problems => Glyph::Analyse,
        HintFilter::Soft => Glyph::HintSoft,
        HintFilter::Accidental => Glyph::HintAccident,
        HintFilter::Duplicates => Glyph::HintCopy,
        HintFilter::Sharpest => Glyph::HintSharp,
    }
}

/// How long analysing `shots` should take: about 6 ms a shot from a
/// network share (bound by reading ~0.4 MB each), about 1.2 ms from a
/// local disk (bound by decoding, on four threads).
pub fn estimate(shots: usize, on_share: bool) -> String {
    let secs = shots as f32 * if on_share { 0.006 } else { 0.0012 };
    if secs < 8.0 {
        "a few seconds".into()
    } else if secs < 50.0 {
        format!("about {} seconds", ((secs / 5.0).round() * 5.0) as u32)
    } else if secs < 90.0 {
        "about a minute".into()
    } else {
        format!("about {} minutes", (secs / 60.0).round() as u32)
    }
}

/// "5 look out of focus, 1 looks accidental and 3 are copies", or `None`
/// when nothing was found.
pub fn findings(soft: usize, accidental: usize, copies: usize) -> Option<String> {
    let verb = |n: usize, one: &str, many: &str| format!("{} {}", util::thousands(n as u64), if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if soft > 0 {
        parts.push(verb(soft, "looks out of focus", "look out of focus"));
    }
    if accidental > 0 {
        parts.push(verb(accidental, "looks accidental", "look accidental"));
    }
    if copies > 0 {
        parts.push(verb(copies, "is a copy", "are copies"));
    }
    match parts.len() {
        0 => None,
        1 => Some(parts.remove(0)),
        n => {
            let last = parts.pop().unwrap_or_default();
            Some(format!("{} and {last}", parts[..n - 1].join(", ")))
        }
    }
}

/// The status line when a pass the user asked for ends: what it found, or
/// where it stopped.
pub fn ended_text(app: &App) -> String {
    let q = &app.quality;
    let (measured, shots) = q.hints.coverage();
    let Some(fin) = &q.last else { return String::new() };
    if fin.error.is_some() {
        return format!("Analysing stopped at {} of {} shots: the folder stopped answering.", util::thousands(measured as u64), util::thousands(shots as u64));
    }
    if measured < shots {
        return format!(
            "Stopped analysing at {} of {} shots. What was checked is kept: analysing again carries on from there.",
            util::thousands(measured as u64),
            util::thousands(shots as u64)
        );
    }
    let s = q.summary();
    match findings(s.soft_only, s.accidental, q.hints.copies()) {
        Some(f) => format!("Analysed {}: {f}.", util::plural(shots as u64, "shot")),
        None => format!("Analysed {}: none look out of focus or accidental.", util::plural(shots as u64, "shot")),
    }
}

/// A pass the user asked for has ended: say so, with a way to see what it
/// found.  (Showing cached results on opening a folder says nothing.)
pub fn on_ended(app: &mut App, mode: Mode) -> Task<Message> {
    if mode != Mode::Full {
        return Task::none();
    }
    let text = ended_text(app);
    let s = app.quality.summary();
    let found = s.soft_only + s.accidental + app.quality.hints.copies();
    let complete = app.quality.last.as_ref().is_some_and(|f| f.error.is_none());
    if complete && found > 0 && app.filter.hint != Some(HintFilter::Problems) {
        app.show_status_action(text, "Show them", Message::Filter(FilterMsg::Hint(Some(HintFilter::Problems))))
    } else {
        app.show_status(text, 0)
    }
}

// ── the status bar ──────────────────────────────────────────────────────────

const BAR_W: f32 = 56.0;

/// While a pass runs: "Analysing 120 of 369", a thin bar, and a stop
/// button, on the status bar's right.
pub fn status_slot(app: &App) -> Option<Element<'_, Message>> {
    if !app.quality.analysing() {
        return None;
    }
    let (done, total) = app.quality.progress.unwrap_or((0, 0));
    let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
    let bar = progress_bar(0.0..=1.0, frac).length(BAR_W).girth(4).style(|_| progress_bar::Style {
        background: Background::Color(t::RAISED),
        bar: Background::Color(t::FLOW),
        border: Border { radius: 2.0.into(), ..Border::default() },
    });
    let stop = button(container(icons::view(Glyph::Close, 11.0, t::INK2)).center(Length::Fill))
        .width(20)
        .height(20)
        .padding(0)
        .style(style::ghost().radius(4.0).style())
        .on_press(Message::Hints(Msg::Stop));
    let words = row![
        text("Analysing").size(t::T_SMALL).color(t::INK3),
        text(format!("{} of {}", util::thousands(done as u64), util::thousands(total as u64))).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
    ]
    .spacing(4);
    let slot = row![words, bar, tip(stop, "Stop analysing. What is checked so far is kept.", false)].spacing(t::S2).align_y(Alignment::Center);
    Some(row![slot, Space::new().width(t::S3), kit::vrule(12.0), Space::new().width(t::S3)].align_y(Alignment::Center).into())
}

// ── the filter bar's Hints control ──────────────────────────────────────────

/// Where the Hints control starts on the filter bar, from the bar's left
/// edge: after All · To review · Marked and Show pruned (their widths as
/// the filter bar builds them), so the menu opens under it.
pub fn chip_x() -> f32 {
    let w = |s: &str| paint::text_width(s, t::T_SMALL, paint::MEDIUM);
    let seg_pad = 2.0 * (t::S3 - 2.0);
    let dot = 6.0 + 6.0;
    let segmented = 2.0 * 2.0 + (seg_pad + w("All")) + (seg_pad + dot + w("To review")) + (seg_pad + dot + w("Marked")) + 2.0 * 2.0;
    let pruned = 2.0 * 10.0 + dot + w("Show pruned");
    super::main_view::BAR_PAD + segmented + super::main_view::GAP + pruned + super::main_view::GAP
}

/// The Hints control: the filter's name when one is on, else "Hints" with
/// how many possible problems there are.
pub fn chip(app: &App) -> Element<'_, Message> {
    let f = app.filter.hint;
    let open = app.quality.menu;
    let (measured, _) = app.quality.hints.coverage();
    let tone = t::WARN;
    let on = f.is_some();
    let fg = if on { tone } else { t::INK2 };
    let glyph = f.map(filter_glyph).unwrap_or(Glyph::Analyse);
    let mut content = row![icons::view(glyph, 13.0, fg), text(f.map(filter_label).unwrap_or("Hints")).size(t::T_SMALL).font(paint::MEDIUM)]
        .spacing(6)
        .align_y(Alignment::Center);
    let problems = app.derived.hint_counts[HintFilter::Problems.index()];
    if !on && measured > 0 && problems > 0 {
        content = content.push(text(util::thousands(problems as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK));
    }
    content = content.push(icons::view(if open { Glyph::ChevronUp } else { Glyph::ChevronDown }, 10.0, if on { tone } else { t::INK3 }));
    let look = if on {
        style::toggle(true, tone)
    } else if open {
        Look { bg: t::RAISED, ..style::toggle(false, tone) }
    } else {
        style::toggle(false, tone)
    };
    let b = button(container(content).center_y(Length::Fill))
        .height(t::CONTROL_SM)
        .padding(Padding::from([0.0, 10.0]))
        .style(look.style())
        .on_press(Message::Hints(Msg::Menu(!open)));
    let hint = if on {
        format!("Showing only: {}. Click to change.", filter_label(f.unwrap_or(HintFilter::Problems)).to_lowercase())
    } else if measured == 0 {
        "Find photos that look out of focus or accidental, copies, and the sharpest of each burst".to_string()
    } else {
        "Show only photos with a hint".to_string()
    };
    if open { b.into() } else { tip(b, hint, true) }
}

/// The Hints menu, under its control, over the grid; a click outside
/// closes it.
pub fn menu_layer(app: &App) -> Option<Element<'_, Message>> {
    if !app.quality.menu {
        return None;
    }
    let catcher = opaque(mouse_area(container(space()).width(Length::Fill).height(Length::Fill)).on_press(Message::Hints(Msg::Menu(false))));
    let panel = crate::ui::motion::Appear::new(container(menu(app)).width(MENU_W).padding(t::S1).style(style::floating())).rise(-6.0);
    let x = (chip_x() - t::S1).max(t::S2);
    Some(stack![catcher, pin(opaque(panel)).position(Point::new(x, 2.0)).width(Length::Fill).height(Length::Fill)].into())
}

const MENU_W: f32 = 340.0;

fn menu(app: &App) -> Element<'_, Message> {
    let q = &app.quality;
    let (measured, shots) = q.hints.coverage();
    let running = q.analysing();
    let shots_here = app.derived.progress.shots.max(shots);
    if measured == 0 && !running {
        return empty_state(app, shots_here);
    }
    let mut col = column![].spacing(2);
    col = col.push(option_row(app, None));
    for f in HintFilter::ALL {
        col = col.push(option_row(app, Some(f)));
    }
    col = col.push(container(kit::hairline()).padding(Padding::from([t::S1, t::S2])));
    // Where the analysis is, and what to do next.
    let (line, action): (String, Element<'_, Message>) = if running {
        let (done, total) = q.progress.unwrap_or((0, 0));
        (
            format!("Analysing {} of {} shots…", util::thousands(done as u64), util::thousands(total as u64)),
            kit::btn("Stop", style::secondary(), Some(Message::Hints(Msg::Stop))).into(),
        )
    } else if measured < shots {
        (
            format!("{} of {} shots analysed.", util::thousands(measured as u64), util::thousands(shots as u64)),
            kit::btn("Analyse the rest", style::primary(), Some(Message::Hints(Msg::Analyse))).into(),
        )
    } else {
        (
            format!("All {} analysed. Nothing is marked: hints only suggest.", util::plural(shots as u64, "shot")),
            tip(kit::btn("Analyse again", style::ghost(), Some(Message::Hints(Msg::Analyse))), "Reads only photos that are new or changed since", true),
        )
    };
    let foot = row![text(line).size(t::T_SMALL).color(t::INK3).width(Length::Fill), action].spacing(t::S2).align_y(Alignment::Center);
    col = col.push(container(foot).padding(Padding { top: 0.0, bottom: t::S1, left: t::S2, right: t::S1 }));
    col.into()
}

/// One choice of the menu: no filter (`None`), or a hint, with how many
/// photos shown have it.
fn option_row(app: &App, f: Option<HintFilter>) -> Element<'_, Message> {
    let chosen = app.filter.hint == f;
    let count = f.map(|f| app.derived.hint_counts[f.index()]);
    let enabled = chosen || count.is_none_or(|n| n > 0);
    let fg = if enabled { t::INK } else { t::INK4 };
    let check: Element<'_, Message> = match (chosen, f) {
        (true, Some(_)) => icons::view(Glyph::Check, 14.0, t::WARN),
        (true, None) => icons::view(Glyph::Check, 14.0, t::INK2),
        _ => Space::new().width(14).into(),
    };
    let glyph: Element<'_, Message> = match f {
        Some(f) => icons::view(filter_glyph(f), 16.0, if enabled { t::INK2 } else { t::INK4 }),
        None => icons::view(Glyph::Photos, 16.0, if enabled { t::INK2 } else { t::INK4 }),
    };
    let (label, about) = match f {
        Some(f) => (filter_label(f), filter_about(f)),
        None => ("All photos", "No hints filter"),
    };
    let words = column![text(label).size(t::T_BODY).color(fg), text(about).size(t::T_SMALL).color(if enabled { t::INK3 } else { t::INK4 })].spacing(1);
    let mut line = row![check, glyph, words.width(Length::Fill)].spacing(t::S2).align_y(Alignment::Center);
    if let Some(n) = count {
        line = line.push(text(util::thousands(n as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(if enabled { t::INK2 } else { t::INK4 }));
    }
    button(line)
        .padding(Padding::from([5.0, t::S2]))
        .width(Length::Fill)
        .style(move |_t, status| {
            let hot = enabled && matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if hot {
                    Some(Background::Color(t::RAISED_HI))
                } else if chosen {
                    Some(Background::Color(if f.is_some() { alpha(t::WARN, 0.08) } else { t::RAISED }))
                } else {
                    None
                },
                text_color: fg,
                border: Border { radius: t::R_CONTROL.into(), ..Border::default() },
                ..button::Style::default()
            }
        })
        .on_press_maybe(enabled.then_some(Message::Filter(FilterMsg::Hint(f))))
        .into()
}

/// Before anything is analysed: what analysing does, what it costs, and
/// the button.
fn empty_state(app: &App, shots: usize) -> Element<'_, Message> {
    let share = app.folder.as_deref().and_then(|f| super::network::share_of(app, f));
    let time = estimate(shots, share.is_some());
    let cost = match &share {
        Some(_) => format!("For these {} on a network share, {time}. You can keep culling meanwhile.", util::plural(shots as u64, "shot")),
        None => format!("For these {}, {time}. You can keep culling meanwhile.", util::plural(shots as u64, "shot")),
    };
    let points = column![
        point(Glyph::HintSoft, "Frames that look out of focus"),
        point(Glyph::HintAccident, "Accidental frames: lens cap, pocket, ground"),
        point(Glyph::HintCopy, "Files that are in the folder twice"),
        point(Glyph::HintSharp, "The sharpest frame of each burst"),
    ]
    .spacing(6);
    let enabled = !app.scanning && !app.coll.is_empty();
    let content = column![
        text("Not analysed yet").size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK),
        text("Analysing reads each photo's preview and looks for:").size(t::T_BODY).color(t::INK2),
        points,
        text(cost).size(t::T_SMALL).color(t::INK3),
        text("Nothing is marked: hints only suggest. The bulk actions in the Prune menu always ask first.").size(t::T_SMALL).color(t::INK3),
        row![Space::new().width(Length::Fill), kit::btn("Analyse this folder", style::primary(), enabled.then_some(Message::Hints(Msg::Analyse)))],
    ]
    .spacing(t::S2);
    container(content).padding(t::S3).into()
}

fn point<'a>(g: Glyph, words: &'a str) -> Element<'a, Message> {
    row![icons::view(g, 14.0, t::INK2), text(words).size(t::T_BODY).color(t::INK)].spacing(t::S2).align_y(Alignment::Center).into()
}

// ── an empty grid ───────────────────────────────────────────────────────────

/// The hints filter is on and nothing passes it.
pub fn filtered_empty(app: &App) -> bool {
    let empty = match app.view {
        super::View::List => app.derived.list_rows.is_empty(),
        _ => app.derived.visible.is_empty(),
    };
    app.filter.hint.is_some() && empty && !app.scanning && !app.coll.is_empty()
}

/// Say why the grid is empty, and how to see photos again.
pub fn filtered_empty_page(app: &App) -> Element<'_, Message> {
    let f = app.filter.hint.unwrap_or(HintFilter::Problems);
    let analysed = app.quality.hints.coverage().0 > 0;
    let (title, body) = if !analysed {
        ("Not analysed yet".to_string(), "Hints come from analysing the folder.".to_string())
    } else if app.filter.show() == Show::ToReview {
        (format!("Nothing left to review in “{}”", filter_label(f).to_lowercase()), "Every photo with this hint is pruned or marked.".to_string())
    } else {
        (format!("No photos in “{}”", filter_label(f).to_lowercase()), "None of the photos shown has this hint.".to_string())
    };
    let mut col = column![].spacing(t::S2).align_x(Alignment::Center).max_width(460);
    col = col.push(Space::new().height(Length::FillPortion(3)));
    col = col.push(icons::view(filter_glyph(f), 48.0, t::INK4));
    col = col.push(Space::new().height(t::S2));
    col = col.push(text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK));
    col = col.push(text(body).size(t::T_BODY).color(t::INK2).align_x(iced::alignment::Horizontal::Center));
    col = col.push(Space::new().height(t::S2));
    let mut buttons = row![kit::btn("Turn off the hints filter", style::secondary(), Some(Message::Filter(FilterMsg::Hint(None))))].spacing(t::S2);
    if !analysed {
        buttons = buttons.push(kit::btn("Analyse this folder", style::primary(), Some(Message::Hints(Msg::Analyse))));
    }
    col = col.push(buttons);
    col = col.push(Space::new().height(Length::FillPortion(4)));
    let ground = if app.view == super::View::Grid { t::PHOTO_GROUND } else { t::GROUND };
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(ground)).into()
}

// ── the banner ──────────────────────────────────────────────────────────────

/// When a pass stopped because the folder stopped answering: what
/// happened, what was kept, and how to carry on.
pub fn banner(app: &App) -> Option<Element<'_, Message>> {
    let q = &app.quality;
    let error = q.last.as_ref()?.error.as_ref()?;
    if q.banner_hidden || q.analysing() {
        return None;
    }
    let (measured, shots) = q.hints.coverage();
    let place = app.folder.as_deref().and_then(|f| super::network::share_of(app, f)).unwrap_or_else(|| "the folder".to_string());
    let head = format!("Analysing stopped: {place} stopped answering");
    let detail = format!(
        "{} of {} shots were checked, and those results are kept. Once it can be reached again, Analyse again carries on from there.",
        util::thousands(measured as u64),
        util::thousands(shots as u64)
    );
    let words = column![
        text(head).size(t::T_BODY).font(paint::MEDIUM).color(t::INK),
        text(detail).size(t::T_SMALL).color(t::INK2),
        text(error.clone()).size(t::T_SMALL).color(t::INK3),
    ]
    .spacing(2)
    .width(Length::Fill);
    let again = kit::btn("Analyse again", style::secondary(), Some(Message::Hints(Msg::Analyse)));
    let dismiss = tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(Message::Hints(Msg::HideBanner))), "Hide this message", true);
    let content = row![icons::view(Glyph::Analyse, 18.0, t::WARN), words, again, dismiss]
        .spacing(t::S3)
        .align_y(Alignment::Center)
        .padding(Padding { top: t::S2, bottom: t::S2, left: 12.0, right: 4.0 });
    let strip = container(content).width(Length::Fill).style(style::fill(alpha(t::WARN, 0.09)));
    Some(column![strip, container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::WARN, 0.35)))].into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_estimate_speaks_in_round_figures() {
        assert_eq!(estimate(369, true), "a few seconds");
        assert_eq!(estimate(5_000, true), "about 30 seconds");
        assert_eq!(estimate(5_000, false), "a few seconds");
        assert_eq!(estimate(12_000, true), "about a minute");
        assert_eq!(estimate(40_000, true), "about 4 minutes");
        assert_eq!(estimate(20_000, false), "about 25 seconds");
    }

    #[test]
    fn findings_read_as_a_sentence() {
        assert_eq!(findings(0, 0, 0), None);
        assert_eq!(findings(5, 0, 0).as_deref(), Some("5 look out of focus"));
        assert_eq!(findings(1, 1, 0).as_deref(), Some("1 looks out of focus and 1 looks accidental"));
        assert_eq!(findings(3, 2, 1_204).as_deref(), Some("3 look out of focus, 2 look accidental and 1,204 are copies"));
        assert_eq!(findings(0, 0, 1).as_deref(), Some("1 is a copy"));
    }

    #[test]
    fn every_filter_has_words_and_a_glyph() {
        for f in HintFilter::ALL {
            assert!(!filter_label(f).is_empty() && !filter_about(f).is_empty());
            let _ = filter_glyph(f);
            assert_eq!(filter_label(f).chars().next().map(char::is_uppercase), Some(true), "sentence case");
        }
    }
}
