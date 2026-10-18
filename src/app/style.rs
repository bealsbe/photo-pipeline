//! Widget styles, all built from the tokens in `ui::theme`.
//!
//! Buttons come in a few roles rather than one-off colours: `ghost` (icon
//! and toolbar actions), `secondary`, `primary` (the one main action in a
//! view, filled with the accent), `danger`, `segment` (one option of a
//! segmented control) and `toggle` (an on/off filter).

use iced::widget::{button, checkbox, container, radio, scrollable, slider, text_input};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

use crate::ui::theme::{self as t, alpha};

pub fn theme() -> Theme {
    Theme::custom(
        "Photo Pipeline",
        iced::theme::Palette {
            background: t::GROUND,
            text: t::INK,
            primary: t::SIGNAL,
            success: t::KEEP,
            warning: t::WARN,
            danger: t::REJECT,
        },
    )
}

pub fn border(color: Color, width: f32, radius: f32) -> Border {
    Border { color, width, radius: radius.into() }
}

/// A button look: colours for each state.
#[derive(Clone, Copy)]
pub struct Look {
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub hover_bg: Color,
    pub hover_fg: Color,
    pub hover_border: Color,
    pub pressed_bg: Option<Color>,
    pub disabled_bg: Color,
    pub disabled_fg: Color,
    pub disabled_border: Color,
    pub radius: f32,
    pub width: f32,
}

impl Look {
    pub fn style(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_t, status| {
            let (bg, fg, b) = match status {
                button::Status::Active => (self.bg, self.fg, self.border),
                button::Status::Hovered => (self.hover_bg, self.hover_fg, self.hover_border),
                button::Status::Pressed => (self.pressed_bg.unwrap_or(self.hover_bg), self.hover_fg, self.hover_border),
                button::Status::Disabled => (self.disabled_bg, self.disabled_fg, self.disabled_border),
            };
            button::Style {
                background: (bg.a > 0.0).then_some(Background::Color(bg)),
                text_color: fg,
                border: border(b, self.width, self.radius),
                shadow: Shadow::default(),
                snap: true,
            }
        }
    }

    /// The same look with a different corner radius.
    pub fn radius(self, radius: f32) -> Look {
        Look { radius, ..self }
    }
}

/// Transparent until hovered: icon buttons and actions inside bars.
pub fn ghost() -> Look {
    Look {
        bg: Color::TRANSPARENT,
        fg: t::INK2,
        border: Color::TRANSPARENT,
        hover_bg: t::RAISED,
        hover_fg: t::INK,
        hover_border: Color::TRANSPARENT,
        pressed_bg: Some(t::RAISED_HI),
        disabled_bg: Color::TRANSPARENT,
        disabled_fg: t::INK4,
        disabled_border: Color::TRANSPARENT,
        radius: t::R_CONTROL,
        width: 0.0,
    }
}

/// A resting control: raised fill, hairline border.
pub fn secondary() -> Look {
    Look {
        bg: t::RAISED,
        fg: t::INK,
        border: t::LINE_HI,
        hover_bg: t::RAISED_HI,
        hover_fg: t::INK,
        hover_border: t::LINE_HI,
        pressed_bg: Some(t::RAISED),
        disabled_bg: Color::TRANSPARENT,
        disabled_fg: t::INK4,
        disabled_border: t::LINE,
        radius: t::R_CONTROL,
        width: 1.0,
    }
}

/// The main action of a view.
pub fn primary() -> Look {
    let hover = t::SIGNAL_HI;
    Look {
        bg: t::SIGNAL,
        fg: t::ON_SIGNAL,
        border: Color::TRANSPARENT,
        hover_bg: hover,
        hover_fg: t::ON_SIGNAL,
        hover_border: Color::TRANSPARENT,
        pressed_bg: Some(t::SIGNAL),
        disabled_bg: t::RAISED,
        disabled_fg: t::INK4,
        disabled_border: Color::TRANSPARENT,
        radius: t::R_CONTROL,
        width: 0.0,
    }
}

/// A destructive action (move to Trash).
pub fn danger() -> Look {
    let hover = Color { r: 0.97, g: 0.66, b: 0.76, a: 1.0 };
    Look {
        bg: t::REJECT,
        fg: t::ON_SIGNAL,
        hover_bg: hover,
        hover_fg: t::ON_SIGNAL,
        pressed_bg: Some(t::REJECT),
        ..primary()
    }
}

/// One option of a segmented control (sort key, Month / Year, crop ratio).
pub fn segment(active: bool) -> Look {
    if active {
        Look {
            bg: t::RAISED_HI,
            fg: t::INK,
            border: Color::TRANSPARENT,
            hover_bg: t::RAISED_HI,
            hover_fg: t::INK,
            ..ghost()
        }
    } else {
        Look { hover_bg: t::RAISED, ..ghost() }
    }
}

/// An on/off filter.  On, it takes `tone` (the meaning of the filter).
pub fn toggle(on: bool, tone: Color) -> Look {
    if on {
        Look {
            bg: alpha(tone, 0.16),
            fg: tone,
            border: alpha(tone, 0.55),
            hover_bg: alpha(tone, 0.24),
            hover_fg: tone,
            hover_border: alpha(tone, 0.75),
            width: 1.0,
            ..ghost()
        }
    } else {
        Look { border: t::LINE, hover_border: t::LINE_HI, width: 1.0, ..ghost() }
    }
}

/// A text-only link ("Change", "Clear").
pub fn link() -> Look {
    Look { fg: t::SIGNAL, hover_fg: t::INK, hover_bg: Color::TRANSPARENT, pressed_bg: None, ..ghost() }
}

// ── containers ──────────────────────────────────────────────────────────────

pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style { background: Some(Background::Color(color)), ..Default::default() }
}

/// A bordered group inside a dialog or panel.
pub fn group() -> impl Fn(&Theme) -> container::Style {
    |_| container::Style {
        background: Some(Background::Color(t::DEEP)),
        border: border(t::LINE, 1.0, t::R_CONTROL),
        ..Default::default()
    }
}

/// Dialogs, menus and popups: they float above everything.
pub fn floating() -> impl Fn(&Theme) -> container::Style {
    |_| container::Style {
        background: Some(Background::Color(t::FLOAT)),
        border: border(t::LINE_HI, 1.0, t::R_PANEL),
        shadow: Shadow { color: Color { a: 0.45, ..Color::BLACK }, offset: Vector::new(0.0, 8.0), blur_radius: 28.0 },
        ..Default::default()
    }
}

/// A small tinted label (status and summary chips).
pub fn chip(tone: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(alpha(tone, 0.12))),
        border: border(Color::TRANSPARENT, 0.0, 3.0),
        text_color: Some(tone),
        ..Default::default()
    }
}

/// Tooltip bubble.
pub fn tip(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(t::RAISED_HI)),
        border: border(t::LINE_HI, 1.0, t::R_CONTROL),
        text_color: Some(t::INK),
        shadow: Shadow { color: Color { a: 0.35, ..Color::BLACK }, offset: Vector::new(0.0, 3.0), blur_radius: 10.0 },
        ..Default::default()
    }
}

// ── inputs ──────────────────────────────────────────────────────────────────

pub fn check(tone: Color) -> impl Fn(&Theme, checkbox::Status) -> checkbox::Style {
    move |_t, status| {
        let (hovered, checked, disabled) = match status {
            checkbox::Status::Active { is_checked } => (false, is_checked, false),
            checkbox::Status::Hovered { is_checked } => (true, is_checked, false),
            checkbox::Status::Disabled { is_checked } => (false, is_checked, true),
        };
        let (bg, b) = if checked {
            (if disabled { t::RAISED_HI } else { tone }, Color::TRANSPARENT)
        } else {
            (t::DEEP, if hovered { t::INK3 } else { t::LINE_HI })
        };
        checkbox::Style {
            background: Background::Color(bg),
            icon_color: if disabled { t::INK3 } else { t::ON_SIGNAL },
            border: border(b, 1.0, 3.0),
            text_color: Some(if disabled { t::INK4 } else { t::INK }),
        }
    }
}

pub fn input() -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    |_t, status| {
        let focused = matches!(status, text_input::Status::Focused { .. });
        let hovered = matches!(status, text_input::Status::Hovered);
        text_input::Style {
            background: Background::Color(t::DEEP),
            border: border(
                if focused {
                    t::SIGNAL
                } else if hovered {
                    t::INK3
                } else {
                    t::LINE_HI
                },
                1.0,
                t::R_CONTROL,
            ),
            icon: t::INK2,
            placeholder: t::INK3,
            value: t::INK,
            selection: alpha(t::SIGNAL, 0.35),
        }
    }
}

pub fn zoom_slider(enabled: bool) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_t, status| {
        let active = matches!(status, slider::Status::Hovered | slider::Status::Dragged);
        let (done, rest, knob) = if enabled {
            (t::INK2, t::LINE_HI, if active { Color::WHITE } else { t::INK })
        } else {
            (t::LINE_HI, t::LINE, t::INK4)
        };
        slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(done), Background::Color(rest)),
                width: 2.0,
                border: border(Color::TRANSPARENT, 0.0, 1.0),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 6.0 },
                background: Background::Color(knob),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        }
    }
}

pub fn radio_style(_t: &Theme, status: radio::Status) -> radio::Style {
    let (hovered, selected) = match status {
        radio::Status::Active { is_selected } => (false, is_selected),
        radio::Status::Hovered { is_selected } => (true, is_selected),
    };
    radio::Style {
        background: Background::Color(t::DEEP),
        dot_color: t::SIGNAL,
        border_width: 1.0,
        border_color: if selected {
            t::SIGNAL
        } else if hovered {
            t::INK3
        } else {
            t::LINE_HI
        },
        text_color: Some(t::INK),
    }
}

pub fn scroll(_t: &Theme, status: scrollable::Status) -> scrollable::Style {
    let active = matches!(status, scrollable::Status::Hovered { is_vertical_scrollbar_hovered: true, .. })
        || matches!(status, scrollable::Status::Dragged { is_vertical_scrollbar_dragged: true, .. });
    let rail = scrollable::Rail {
        background: None,
        border: border(Color::TRANSPARENT, 0.0, 0.0),
        scroller: scrollable::Scroller {
            background: Background::Color(if active { t::INK3 } else { t::LINE_HI }),
            border: border(Color::TRANSPARENT, 0.0, 3.0),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(t::FLOAT),
            border: border(t::LINE_HI, 1.0, 8.0),
            shadow: Shadow::default(),
            icon: t::INK,
        },
    }
}
