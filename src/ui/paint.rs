//! Drawing helpers shared by the custom widgets: rounded quads, text with
//! alignment, measured / elided text, anti-aliased line strokes, and the
//! easing curves the Python app used.

use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::renderer::{Quad, Renderer as _};
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::alignment::Vertical;
use iced::widget::canvas::{self, LineCap, Path, Stroke};
use iced::{Border, Color, Font, Pixels, Point, Rectangle, Size, Vector, border, font};

pub type Renderer = iced::Renderer;

pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

/// The accent orange at an 8-bit alpha, as the Python code wrote it.
pub fn accent(a: u8) -> Color {
    rgba8(0xff, 0x6d, 0x00, a)
}

pub fn with_alpha(c: Color, factor: f32) -> Color {
    Color { a: c.a * factor.clamp(0.0, 1.0), ..c }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color { r: lerp(a.r, b.r, t), g: lerp(a.g, b.g, t), b: lerp(a.b, b.b, t), a: lerp(a.a, b.a, t) }
}

// ── easing ──────────────────────────────────────────────────────────────────

pub fn out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t + 1.0
}

pub fn out_quint(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t * t * t + 1.0
}

pub fn out_expo(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t) }
}

pub fn out_quad(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * (2.0 - t)
}

// ── fonts ───────────────────────────────────────────────────────────────────

pub const REGULAR: Font = Font::DEFAULT;
pub const MEDIUM: Font = Font { weight: font::Weight::Medium, ..Font::DEFAULT };
pub const SEMIBOLD: Font = Font { weight: font::Weight::Semibold, ..Font::DEFAULT };
pub const BOLD: Font = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
pub const MONO: Font = Font::MONOSPACE;

// ── primitives ──────────────────────────────────────────────────────────────

pub fn fill(r: &mut Renderer, rect: Rectangle, color: Color) {
    if color.a <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    r.fill_quad(Quad { bounds: rect, border: Border::default(), ..Quad::default() }, color);
}

/// A soft highlight band sweeping across `rect` (loading placeholders).
/// `phase` runs 0 → 1; the band enters at the left and leaves at the right.
pub fn shimmer(r: &mut Renderer, rect: Rectangle, radius: f32, base: Color, highlight: Color, phase: f32) {
    use iced::gradient::Linear;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let centre = -0.3 + 1.6 * phase.clamp(0.0, 1.0);
    let mut g = Linear::new(iced::Radians(std::f32::consts::FRAC_PI_2 + 0.35));
    let mut stops: Vec<(f32, Color)> = vec![(0.0, base)];
    for (off, c) in [(centre - 0.22, base), (centre, highlight), (centre + 0.22, base)] {
        if off > 0.0 && off < 1.0 {
            stops.push((off, c));
        }
    }
    stops.push((1.0, base));
    for (off, c) in stops {
        g = g.add_stop(off, c);
    }
    r.fill_quad(
        Quad { bounds: rect, border: border::rounded(radius), ..Quad::default() },
        iced::Background::Gradient(iced::Gradient::Linear(g)),
    );
}

pub fn rounded(r: &mut Renderer, rect: Rectangle, radius: f32, color: Color) {
    if color.a <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    r.fill_quad(Quad { bounds: rect, border: border::rounded(radius), ..Quad::default() }, color);
}

/// A rounded rect with a border (drawn inside the bounds).
pub fn framed(r: &mut Renderer, rect: Rectangle, radius: f32, fill: Color, stroke: Color, width: f32) {
    if rect.width <= 0.0 || rect.height <= 0.0 || (fill.a <= 0.0 && stroke.a <= 0.0) {
        return;
    }
    r.fill_quad(
        Quad {
            bounds: rect,
            border: Border { color: stroke, width, radius: radius.into() },
            ..Quad::default()
        },
        fill,
    );
}

pub fn circle(r: &mut Renderer, center: Point, radius: f32, fill: Color, stroke: Color, width: f32) {
    let rect = Rectangle::new(Point::new(center.x - radius, center.y - radius), Size::new(radius * 2.0, radius * 2.0));
    framed(r, rect, radius, fill, stroke, width);
}

/// A 1-px style line drawn as a thin quad (axis-aligned only).
pub fn hline(r: &mut Renderer, x0: f32, x1: f32, y: f32, width: f32, color: Color) {
    fill(r, Rectangle::new(Point::new(x0.min(x1), y), Size::new((x1 - x0).abs(), width)), color);
}

pub fn vline(r: &mut Renderer, x: f32, y0: f32, y1: f32, width: f32, color: Color) {
    fill(r, Rectangle::new(Point::new(x, y0.min(y1)), Size::new(width, (y1 - y0).abs())), color);
}

/// Anti-aliased strokes through `segments` (any angle).
pub fn strokes(r: &mut Renderer, segments: &[(Point, Point)], width: f32, color: Color, round: bool) {
    if segments.is_empty() || color.a <= 0.0 {
        return;
    }
    let (mut min, mut max) = (Point::new(f32::MAX, f32::MAX), Point::new(f32::MIN, f32::MIN));
    for (a, b) in segments {
        for p in [a, b] {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
    }
    let pad = width + 2.0;
    let origin = Point::new((min.x - pad).floor(), (min.y - pad).floor());
    let size = Size::new(max.x - origin.x + pad * 2.0, max.y - origin.y + pad * 2.0);
    let mut frame = canvas::Frame::new(r, size);
    let path = Path::new(|b| {
        for (a, c) in segments {
            b.move_to(Point::new(a.x - origin.x, a.y - origin.y));
            b.line_to(Point::new(c.x - origin.x, c.y - origin.y));
        }
    });
    let stroke = Stroke::default()
        .with_color(color)
        .with_width(width)
        .with_line_cap(if round { LineCap::Round } else { LineCap::Butt });
    frame.stroke(&path, stroke);
    let geometry = frame.into_geometry();
    r.with_translation(Vector::new(origin.x, origin.y), |r| r.draw_geometry(geometry));
}

// ── text ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Draw `content` inside `rect`, aligned, clipped to `clip`.
#[allow(clippy::too_many_arguments)]
pub fn text(
    r: &mut Renderer,
    content: &str,
    rect: Rectangle,
    size: f32,
    font: Font,
    color: Color,
    align: HAlign,
    clip: Rectangle,
) {
    if content.is_empty() || color.a <= 0.0 {
        return;
    }
    let (x, align_x) = match align {
        HAlign::Left => (rect.x, text::Alignment::Left),
        HAlign::Center => (rect.center_x(), text::Alignment::Center),
        HAlign::Right => (rect.x + rect.width, text::Alignment::Right),
    };
    r.fill_text(
        iced::advanced::Text {
            content: content.to_string(),
            bounds: Size::new(f32::INFINITY, rect.height.max(size * 1.4)),
            size: Pixels(size),
            line_height: text::LineHeight::Relative(1.25),
            font,
            align_x,
            align_y: Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        },
        Point::new(x, rect.center_y()),
        color,
        clip,
    );
}

/// Multi-line text (explicit `\n`), top-left anchored.
///
/// Drawn a line at a time, each with finite bounds: as one block (or with
/// an infinite height) iced drew only the start of it, which left tooltips
/// showing a single character or line.
pub fn text_block(r: &mut Renderer, content: &str, origin: Point, size: f32, font: Font, color: Color, clip: Rectangle) {
    let line_h = size * 1.3;
    for (i, line) in content.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        r.fill_text(
            iced::advanced::Text {
                content: line.to_string(),
                bounds: Size::new(f32::INFINITY, line_h.ceil() + 1.0),
                size: Pixels(size),
                line_height: text::LineHeight::Relative(1.3),
                font,
                align_x: text::Alignment::Left,
                align_y: Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            },
            Point::new(origin.x, origin.y + i as f32 * line_h),
            color,
            clip,
        );
    }
}

/// Width and height of `content` laid out on one line (or several, split on
/// `\n`).
pub fn measure(content: &str, size: f32, font: Font) -> Size {
    let p = <Renderer as text::Renderer>::Paragraph::with_text(iced::advanced::Text {
        content,
        bounds: Size::new(f32::INFINITY, f32::INFINITY),
        size: Pixels(size),
        line_height: text::LineHeight::Relative(1.3),
        font,
        align_x: text::Alignment::Left,
        align_y: Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    });
    p.min_bounds()
}

pub fn text_width(content: &str, size: f32, font: Font) -> f32 {
    measure(content, size, font).width
}

/// Shorten a file name from its start: `20260724_0108_0001` →
/// `…0108_0001`.  The end of a name (time, sequence number, an `-Edit`
/// suffix) is what tells a shoot's photos apart; the start (date, camera
/// prefix) is shared by all of them and the grid's date header says it
/// anyway.
pub fn elide_start(content: &str, size: f32, font: Font, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    elide_start_with(content, |s| text_width(s, size, font) <= max_width)
}

/// [`elide_start`] with the width test passed in.  Prefers to cut where a
/// part of the name begins (after `_`, `-`, `.` or a space, or where letters
/// turn to digits), so no number is shown half cut off: `…0108_0001`, not
/// `…724_0108_0001`.  Within a single part that is too long, it cuts
/// anywhere.
pub fn elide_start_with(content: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(content) {
        return content.to_string();
    }
    let chars: Vec<char> = content.chars().collect();
    let tail = |i: usize| -> String { std::iter::once('…').chain(chars[i..].iter().copied()).collect() };
    // Where each part of the name starts, longest tail first.
    let starts = (1..chars.len()).filter(|&i| {
        let (a, b) = (chars[i - 1], chars[i]);
        b.is_alphanumeric() && (!a.is_alphanumeric() || (a.is_alphabetic() && b.is_numeric()))
    });
    for i in starts {
        if fits(&tail(i)) {
            return tail(i);
        }
    }
    // Not even the last part fits: cut inside it, keeping all that fits.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let keep = (lo + hi).div_ceil(2);
        if fits(&tail(chars.len() - keep)) {
            lo = keep;
        } else {
            hi = keep - 1;
        }
    }
    if lo > 0 {
        tail(chars.len() - lo)
    } else if fits("…") {
        "…".to_string()
    } else {
        String::new()
    }
}

/// Qt's `ElideRight`.
pub fn elide_right(content: &str, size: f32, font: Font, max_width: f32) -> String {
    if text_width(content, size, font) <= max_width {
        return content.to_string();
    }
    let chars: Vec<char> = content.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        if text_width(&s, size, font) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().chain(std::iter::once(&'…')).collect()
}

/// Rectangle helpers.
pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
    Rectangle::new(Point::new(x, y), Size::new(w.max(0.0), h.max(0.0)))
}

pub fn inset(r: Rectangle, d: f32) -> Rectangle {
    rect(r.x + d, r.y + d, r.width - 2.0 * d, r.height - 2.0 * d)
}

pub fn lerp_rect(a: Rectangle, b: Rectangle, t: f32) -> Rectangle {
    rect(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.width, b.width, t), lerp(a.height, b.height, t))
}

#[cfg(test)]
mod tests {
    use super::elide_start_with;

    /// Every character one unit wide; `n` units fit.
    fn width(n: usize) -> impl Fn(&str) -> bool {
        move |s: &str| s.chars().count() <= n
    }

    #[test]
    fn elide_start_keeps_the_end_and_cuts_between_parts() {
        let name = "20260724_0108_0001";
        assert_eq!(elide_start_with(name, width(18)), name);
        // One short: the whole date goes, never half of it.
        assert_eq!(elide_start_with(name, width(17)), "…0108_0001");
        assert_eq!(elide_start_with(name, width(10)), "…0108_0001");
        assert_eq!(elide_start_with(name, width(9)), "…0001");
        // The last part alone is too long: cut inside it.
        assert_eq!(elide_start_with(name, width(4)), "…001");
        assert_eq!(elide_start_with(name, width(1)), "…");
        assert_eq!(elide_start_with(name, width(0)), "");
    }

    #[test]
    fn elide_start_splits_letters_from_digits_and_keeps_suffixes() {
        assert_eq!(elide_start_with("DSC09876", width(7)), "…09876");
        assert_eq!(elide_start_with("IMG_20260724_123456", width(17)), "…20260724_123456");
        assert_eq!(elide_start_with("IMG_20260724_123456", width(15)), "…123456");
        assert_eq!(elide_start_with("20260724_0108_0001-Edit", width(16)), "…0108_0001-Edit");
        assert_eq!(elide_start_with("_DSC1234", width(7)), "…1234");
    }
}
