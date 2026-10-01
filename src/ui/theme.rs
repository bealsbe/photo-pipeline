//! Design tokens: every colour, size, spacing and timing value the
//! interface uses.
//!
//! Two palettes.  The chrome (bars, menus, panels, dialogs, the list and the
//! calendar) follows the desktop's Catppuccin Mocha scheme and its purple
//! accent.  Everything around a photo being reviewed (the grid's background
//! and mounts, the viewer's stage) stays true neutral grey: a tinted
//! surround shifts how colour and exposure are judged.

use iced::Color;

const fn hex(v: u32) -> Color {
    Color {
        r: ((v >> 16) & 0xff) as f32 / 255.0,
        g: ((v >> 8) & 0xff) as f32 / 255.0,
        b: (v & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// `c` at `alpha` (0–1).
pub const fn alpha(c: Color, alpha: f32) -> Color {
    Color { a: alpha, ..c }
}

// ── chrome surfaces (Catppuccin Mocha) ──────────────────────────────────────

/// Content areas that hold no photos: the list, the calendar pages (base).
pub const GROUND: Color = hex(0x1e1e2e);
/// Bars and panels: menu bar, folder and filter bars, side rail (mantle).
pub const MOUNT: Color = hex(0x181825);
/// Controls resting on a bar (surface 0).
pub const RAISED: Color = hex(0x313244);
/// A hovered or pressed control (surface 1).
pub const RAISED_HI: Color = hex(0x45475a);
/// Hairlines and control borders.
pub const LINE: Color = hex(0x2a2b3d);
/// Borders that need to be seen (inputs, dialogs).
pub const LINE_HI: Color = hex(0x45475a);
/// Menus, tooltips and dialogs float on this (base).
pub const FLOAT: Color = hex(0x1e1e2e);
/// The deepest shade (crust): recessed wells.
pub const DEEP: Color = hex(0x11111b);
/// Dims whatever is behind a dialog or the open side menu.
pub const SCRIM: Color = Color { r: 0.043, g: 0.043, b: 0.07, a: 0.62 };

// ── chrome text ─────────────────────────────────────────────────────────────

pub const INK: Color = hex(0xcdd6f4);
/// Secondary text: labels, metadata (subtext 0).
pub const INK2: Color = hex(0xa6adc8);
/// Tertiary text: hints, captions, placeholders (overlay 1).
pub const INK3: Color = hex(0x7f849c);
/// Disabled text and icons (surface 2).
pub const INK4: Color = hex(0x585b70);

// ── photo surfaces: true neutral ────────────────────────────────────────────

/// Behind the photos: the grid and the viewer stage.
pub const PHOTO_GROUND: Color = hex(0x1a1a1a);
/// The mount a thumbnail sits in.
pub const PHOTO_MOUNT: Color = hex(0x232323);
/// A hovered mount.
pub const PHOTO_RAISED: Color = hex(0x2d2d2d);
/// Rules on the photo ground.
pub const PHOTO_LINE: Color = hex(0x333333);
/// A burst's strip: one mount under all its frames, joined across the
/// gutters, a clear step lighter than a single photo's mount.
pub const PHOTO_TRAY: Color = hex(0x2c2c2c);
/// A hovered frame in a strip, and a hovered burst tab.
pub const PHOTO_TRAY_HI: Color = hex(0x363636);
pub const PHOTO_INK: Color = hex(0xe6e6e6);
pub const PHOTO_INK2: Color = hex(0xa6a6a6);
pub const PHOTO_INK3: Color = hex(0x747474);
pub const PHOTO_INK4: Color = hex(0x4c4c4c);

// ── meaning ─────────────────────────────────────────────────────────────────

/// The accent (the desktop's purple): selection, focus, the active control,
/// the primary action.
pub const SIGNAL: Color = hex(0xb875dc);
/// The accent, lighter: hover on accent fills, links.
pub const SIGNAL_HI: Color = hex(0xcb95e8);
/// Blue, the other half of the scheme: progress and activity.
pub const FLOW: Color = hex(0x89b4fa);
/// Text on a SIGNAL fill.
pub const ON_SIGNAL: Color = hex(0x11111b);
/// Marked to keep (for export).
pub const KEEP: Color = hex(0xa6e3a1);
/// Marked to prune (reject).
pub const REJECT: Color = hex(0xf38ba8);
/// Conflicts and cautions.
pub const WARN: Color = hex(0xf9e2af);
/// File types.
pub const RAW: Color = hex(0x89b4fa);
pub const JPG: Color = hex(0xfab387);

// ── type ────────────────────────────────────────────────────────────────────

/// The bundled typeface (Barlow, tabular figures frozen in).
pub const FAMILY: &str = "Barlow Pipeline";
/// Captions, metadata, badges.
pub const T_SMALL: f32 = 12.0;
/// Body text and controls.
pub const T_BODY: f32 = 13.0;
/// Section and dialog titles.
pub const T_TITLE: f32 = 16.0;
/// Page titles (calendar, viewer file name).
pub const T_HEAD: f32 = 20.0;

// ── space ───────────────────────────────────────────────────────────────────

pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S5: f32 = 24.0;

/// Standard control height (buttons, toggles, inputs).
pub const CONTROL_H: f32 = 28.0;
/// Compact controls inside dense bars.
pub const CONTROL_SM: f32 = 24.0;

/// Radii by role, not one size for everything.
pub const R_CELL: f32 = 3.0;
pub const R_CONTROL: f32 = 6.0;
pub const R_PANEL: f32 = 10.0;

// ── motion ──────────────────────────────────────────────────────────────────
//
// Short and eased out: things respond at once and settle quickly.

/// Hover and press feedback.
pub const MOTION_FAST_MS: f32 = 90.0;
/// Things appearing: dialogs, menus, thumbnails.
pub const MOTION_MS: f32 = 160.0;
