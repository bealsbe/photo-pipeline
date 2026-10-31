//! Shared building blocks for the views: text roles, buttons, tooltips and
//! rules, all on the design tokens.

use iced::widget::{Button, Space, Text, button, column, container, row, text, tooltip};
use iced::{Alignment, Color, Element, Length, Padding};

use super::Message;
use super::style::{self, Look};
use crate::ui::icons::{self, Glyph};
use crate::ui::motion::Glow;
use crate::ui::paint;
use crate::ui::theme as t;

// ── text ────────────────────────────────────────────────────────────────────

pub fn secondary<'a>(s: impl text::IntoFragment<'a>) -> Text<'a> {
    text(s).size(t::T_BODY).color(t::INK2)
}

pub fn caption<'a>(s: impl text::IntoFragment<'a>) -> Text<'a> {
    text(s).size(t::T_SMALL).color(t::INK3)
}

pub fn title<'a>(s: impl text::IntoFragment<'a>) -> Text<'a> {
    text(s).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::INK)
}

/// A small line in the second ink: a note under a control, a detail.
pub fn note<'a>(s: impl text::IntoFragment<'a>) -> Text<'a> {
    text(s).size(t::T_SMALL).color(t::INK2)
}

/// A page with nothing to show yet: a title and a line under it, set high
/// on the photos' grey.
pub fn quiet<'a>(title: String, body: Option<String>) -> Element<'a, Message> {
    let centred = iced::alignment::Horizontal::Center;
    let mut col = column![Space::new().height(Length::FillPortion(2)), text(title).size(t::T_TITLE).font(paint::SEMIBOLD).color(t::PHOTO_INK).align_x(centred)];
    if let Some(body) = body {
        col = col.push(text(body).size(t::T_BODY).color(t::PHOTO_INK2).align_x(centred));
    }
    col = col.push(Space::new().height(Length::FillPortion(5)));
    container(col.spacing(t::S2).align_x(Alignment::Center).max_width(480))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .style(style::fill(t::PHOTO_GROUND))
        .into()
}

/// A group heading inside a panel or dialog.
pub fn heading<'a>(s: impl text::IntoFragment<'a>) -> Text<'a> {
    text(s).size(t::T_SMALL).font(paint::SEMIBOLD).color(t::INK2)
}

// ── rules ───────────────────────────────────────────────────────────────────

pub fn hairline<'a>() -> Element<'a, Message> {
    container(Space::new().width(Length::Fill).height(1)).style(style::fill(t::LINE)).into()
}

pub fn vrule<'a>(height: f32) -> Element<'a, Message> {
    container(Space::new().width(1).height(height)).style(style::fill(t::LINE)).into()
}

// ── tooltips ────────────────────────────────────────────────────────────────

pub fn tip<'a>(content: impl Into<Element<'a, Message>>, label: impl Into<String>, below: bool) -> Element<'a, Message> {
    let bubble = container(text(label.into()).size(t::T_SMALL).color(t::INK)).padding(Padding::from([5.0, 8.0])).style(style::tip);
    let pos = if below { tooltip::Position::Bottom } else { tooltip::Position::Top };
    tooltip(content, bubble, pos).gap(4).into()
}

// ── buttons ─────────────────────────────────────────────────────────────────
//
// Each comes wrapped in a `Glow`: a soft ring in the button's tone fades in
// under the pointer.  The glow's colour follows the look (the accent, or a
// filled button's own fill), and a button without an action never glows.

/// The glow's colour for `look`: a toggle's own tone (keep green, prune
/// red), a filled button's fill (primary, danger), else the accent.
fn glow_tone(look: &Look) -> Color {
    if [t::KEEP, t::REJECT, t::SIGNAL, t::WARN].contains(&look.fg) {
        look.fg
    } else if look.bg == t::SIGNAL {
        t::SIGNAL_HI
    } else if look.bg == t::REJECT {
        t::REJECT
    } else {
        t::SIGNAL
    }
}

fn glow<'a>(b: Button<'a, Message>, look: &Look, enabled: bool) -> Glow<'a, Message> {
    Glow::new(b, enabled).tone(glow_tone(look)).radius(look.radius)
}

/// A text button of standard height.
pub fn btn<'a>(label: impl Into<String>, look: Look, on: Option<Message>) -> Glow<'a, Message> {
    let enabled = on.is_some();
    let b = button(container(text(label.into()).size(t::T_BODY).font(paint::MEDIUM)).center_y(Length::Fill))
        .height(t::CONTROL_H)
        .padding(Padding::from([0.0, t::S4]))
        .style(look.style())
        .on_press_maybe(on);
    glow(b, &look, enabled)
}

/// A square icon-only button.
pub fn icon_btn<'a>(glyph: Glyph, color: Color, look: Look, on: Option<Message>) -> Glow<'a, Message> {
    let enabled = on.is_some();
    let c = if enabled { color } else { t::INK4 };
    let b = button(container(icons::view(glyph, 16.0, c)).center(Length::Fill))
        .width(t::CONTROL_H)
        .height(t::CONTROL_H)
        .padding(0)
        .style(look.style())
        .on_press_maybe(on);
    glow(b, &look, enabled)
}

/// Icon then label.
pub fn icon_text_btn<'a>(glyph: Glyph, label: impl Into<String>, color: Color, look: Look, on: Option<Message>) -> Glow<'a, Message> {
    let enabled = on.is_some();
    let c = if enabled { color } else { t::INK4 };
    let content = row![icons::view(glyph, 16.0, c), text(label.into()).size(t::T_BODY).font(paint::MEDIUM)]
        .spacing(t::S2)
        .align_y(Alignment::Center);
    let b = button(container(content).center_y(Length::Fill))
        .height(t::CONTROL_H)
        .padding(Padding::from([0.0, t::S3]))
        .style(look.style())
        .on_press_maybe(on);
    glow(b, &look, enabled)
}

/// A link: small medium text, no frame.  `pad` is [vertical, horizontal];
/// `on` None draws it dim.
pub fn link_btn<'a>(label: impl Into<String>, pad: [f32; 2], on: Option<Message>) -> Element<'a, Message> {
    button(text(label.into()).size(t::T_SMALL).font(paint::MEDIUM)).padding(Padding::from(pad)).style(style::link().style()).on_press_maybe(on).into()
}

/// The 20 px × that stops a job from the status bar, with its tip.
pub fn stop_btn<'a>(ink: Color, hint: impl Into<String>, on: Message) -> Element<'a, Message> {
    let b = button(container(icons::view(Glyph::Close, 11.0, ink)).center(Length::Fill)).width(20).height(20).padding(0).style(style::ghost().radius(4.0).style()).on_press(on);
    tip(b, hint, false)
}

/// Options of a segmented control, joined on one recessed track.
pub fn segmented<'a>(items: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut r = row![].spacing(2).align_y(Alignment::Center);
    for it in items {
        r = r.push(it);
    }
    container(r).padding(2).style(style::group()).into()
}

/// One segment: text, optionally with a trailing icon.
pub fn segment<'a>(label: impl Into<String>, active: bool, trailing: Option<Glyph>, on: Message) -> Element<'a, Message> {
    segment_padded(label, active, trailing, Some(on), t::S3)
}

/// A segment that may be off (`on` is `None`): drawn dim, not pressable.
pub fn segment_maybe<'a>(label: impl Into<String>, active: bool, on: Option<Message>) -> Element<'a, Message> {
    segment_padded(label, active, None, on, t::S3)
}

/// A segment with tighter sides, for long rows of short options.
pub fn segment_compact<'a>(label: impl Into<String>, active: bool, on: Message) -> Element<'a, Message> {
    segment_padded(label, active, None, Some(on), t::S2)
}

fn segment_padded<'a>(label: impl Into<String>, active: bool, trailing: Option<Glyph>, on: Option<Message>, pad: f32) -> Element<'a, Message> {
    // Off, the button's style dims the text; the icon follows it.
    let fg = if on.is_none() {
        t::INK4
    } else if active {
        t::INK
    } else {
        t::INK2
    };
    let mut content = row![text(label.into()).size(t::T_SMALL).font(paint::MEDIUM)].spacing(t::S1).align_y(Alignment::Center);
    if let Some(g) = trailing {
        content = content.push(icons::view(g, 12.0, fg));
    }
    button(container(content).center_y(Length::Fill))
        .height(t::CONTROL_SM - 4.0)
        .padding(Padding::from([0.0, pad]))
        .style(style::segment(active).radius(3.0).style())
        .on_press_maybe(on)
        .into()
}
