//! Symbols the text renderer cannot draw.
//!
//! ✂ ⚠ ⏭ have emoji presentations, so font fallback picks the colour-emoji
//! font for them — and that font (COLRv1) renders as nothing.  They are
//! drawn as tinted icons instead and laid out inline with the text around
//! them, so call sites keep their Python wording.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::widget::image::Handle;
use iced::widget::{Space, image, row, text};
use iced::{Alignment, Color, Element, Font};

use super::{icons, paint};

const SYMBOLS: [char; 3] = ['✂', '⚠', '⏭'];
/// Emoji presentation selector; Python's strings sometimes carry it.
const VS16: char = '\u{fe0f}';

pub fn has_symbol(s: &str) -> bool {
    s.chars().any(|c| SYMBOLS.contains(&c))
}

/// A cached icon for `ch` in `color`, rasterised at twice `px` so it stays
/// crisp on HiDPI screens.
pub fn handle(ch: char, color: Color, px: f32) -> Option<Handle> {
    static CACHE: OnceLock<Mutex<HashMap<(char, [u8; 4], u32), Handle>>> = OnceLock::new();
    let rgba = color.into_rgba8();
    let size = ((px * 2.0).round() as u32).max(8);
    let key = (ch, rgba, size);
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(h) = cache.get(&key) {
        return Some(h.clone());
    }
    let glyph = match ch {
        '✂' => icons::Glyph::Scissors,
        '⚠' => icons::Glyph::Warning,
        '⏭' => icons::Glyph::Skip,
        _ => return None,
    };
    let h = icons::handle(glyph, size as f32 / 2.0, color);
    cache.insert(key, h.clone());
    Some(h)
}

/// Width a symbol occupies in a line of `size`/`font` text: one cell for
/// monospace (keeps columns aligned), about an em otherwise.
pub fn advance(size: f32, font: Font) -> f32 {
    if font == paint::MONO {
        paint::text_width("MM", size, font) - paint::text_width("M", size, font)
    } else {
        size * 1.05
    }
}

fn space_width(size: f32, font: Font) -> f32 {
    (paint::text_width("a a", size, font) - paint::text_width("aa", size, font)).max(size * 0.2)
}

/// `s` as a single-line text element with any undrawable symbols replaced
/// by icons.  Without such symbols this is just `text(s)`.
pub fn line<'a, M: 'a>(s: &str, size: f32, font: Font, color: Color) -> Element<'a, M> {
    let s: String = s.chars().filter(|&c| c != VS16).collect();
    if !has_symbol(&s) {
        return text(s).size(size).font(font).color(color).into();
    }
    let sp = space_width(size, font);
    let adv = advance(size, font);
    let icon_px = (size * 1.05).round();
    // Split into runs of text and single symbols.
    let mut parts: Vec<Result<String, char>> = Vec::new();
    for c in s.chars() {
        if SYMBOLS.contains(&c) {
            parts.push(Err(c));
        } else if let Some(Ok(t)) = parts.last_mut() {
            t.push(c);
        } else {
            parts.push(Ok(c.to_string()));
        }
    }
    let mut out = row![].align_y(Alignment::Center);
    for (i, part) in parts.iter().enumerate() {
        match part {
            Ok(t) => {
                // Spaces at a symbol boundary don't survive as text (layout
                // trims them); keep them as fixed gaps instead.
                let lead = if i > 0 { t.len() - t.trim_start_matches(' ').len() } else { 0 };
                let trail = if i + 1 < parts.len() { t.len() - t.trim_end_matches(' ').len() } else { 0 };
                if lead + trail >= t.len() {
                    out = out.push(Space::new().width(sp * t.len() as f32));
                    continue;
                }
                if lead > 0 {
                    out = out.push(Space::new().width(sp * lead as f32));
                }
                out = out.push(text(t[lead..t.len() - trail].to_string()).size(size).font(font).color(color));
                if trail > 0 {
                    out = out.push(Space::new().width(sp * trail as f32));
                }
            }
            Err(c) => {
                let icon: Element<'a, M> = match handle(*c, color, icon_px) {
                    Some(h) => image(h).width(icon_px).height(icon_px).into(),
                    None => Space::new().width(icon_px).into(),
                };
                let w = adv.max(icon_px);
                out = out.push(iced::widget::container(icon).center_x(w));
            }
        }
    }
    out.into()
}
