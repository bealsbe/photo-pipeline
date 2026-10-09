//! Icons, drawn from paths on a 24-unit grid with one stroke weight, round
//! caps and round joins, so the whole set reads as one family.  Each is
//! rasterised on first use at twice its display size (crisp on HiDPI and
//! when scaled) in the colour asked for, and cached.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::widget::image::Handle;
use iced::{Color, Element};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Every icon in the app, named for what it means.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Glyph {
    Menu,
    /// Open a working folder.
    FolderOpen,
    /// A folder on disk (show in folder, the working-folder slot).
    Folder,
    /// The library.
    Library,
    /// Export into the library.
    Export,
    /// Copy photos out for editing.
    EditCopy,
    /// Review pruned files (the trash).
    Trash,
    /// Sort into RAW / JPG subfolders.
    Sort,
    Grid,
    Calendar,
    /// Select mode.
    Select,
    /// Keep (mark for export).
    Keep,
    /// Prune (reject).
    Prune,
    Keyboard,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    /// Show the other half of a RAW+JPG pair.
    Swap,
    Crop,
    /// Flip a crop ratio between portrait and landscape.
    Orientation,
    ZoomIn,
    ZoomOut,
    Fit,
    Info,
    Refresh,
    Close,
    Check,
    ArrowUp,
    ArrowDown,
    Warning,
    Scissors,
    Skip,
    Photos,
    /// Undo: an arrow turning back.
    Undo,
    Redo,
    /// Compare photos side by side.
    Compare,
    /// The viewer's filmstrip.
    Filmstrip,
    /// Full screen.
    FullScreen,
    /// On a network share: a drive on a line.
    Network,
    /// Preferences: three sliders.
    Settings,
    /// Analyse this folder: a camera's focus-area brackets round a point.
    Analyse,
    /// Hints (suggestions, hollow so they never read as decisions):
    /// the sharpest frame of a burst, a crisp ring on its point …
    HintSharp,
    /// … looks out of focus, the same ring broken up …
    HintSoft,
    /// … an accidental frame, an empty frame …
    HintAccident,
    /// … a copy of another file, a frame on top of another.
    HintCopy,
    /// Search the library: a magnifier.
    Search,
    /// The graphics card search runs on: a card with two fans.
    GraphicsCard,
    /// A clock: what is done so far, or a wait.
    Clock,
    /// A word left out of a search: a ring with a bar across.
    LeaveOut,
    /// People and fursuits.
    Subjects,
    /// A person: a head on shoulders.
    Person,
    /// A fursuit: a head with full ears and a muzzle.
    Fursuit,
    /// Merge groups: two lines joining.
    Merge,
    /// Move out of a group: one line parting.
    Split,
    /// Hide: an eye crossed out.
    Hide,
    /// Name or rename: a pencil.
    Pencil,
    /// More actions: three dots.
    More,
    /// A new group, or add: a plus.
    Plus,
    /// The close-call judge: two heads weighed on one beam.
    CloseCall,
}

/// An icon as a handle, `px` logical pixels square, in `color`.
pub fn handle(glyph: Glyph, px: f32, color: Color) -> Handle {
    static CACHE: OnceLock<Mutex<HashMap<(Glyph, u32, [u8; 4]), Handle>>> = OnceLock::new();
    let size = ((px * 2.0).round() as u32).max(8);
    let rgba = color.into_rgba8();
    let key = (glyph, size, rgba);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(h) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return h;
    }
    let px_data = render(glyph, size, rgba);
    let h = Handle::from_rgba(size, size, px_data);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, h.clone());
    }
    h
}

/// An icon as a widget.
pub fn view<'a, M: 'a>(glyph: Glyph, px: f32, color: Color) -> Element<'a, M> {
    iced::widget::image(handle(glyph, px, color))
        .width(px)
        .height(px)
        .filter_method(iced::widget::image::FilterMethod::Linear)
        .into()
}

/// The window icon: the app icon `install.sh` installs, at 64 px.  Only X11
/// uses it.  On Wayland a window can't carry its own icon here; KWin and the
/// taskbar find the installed one through the app id (`app::APP_ID`).
pub fn window_icon() -> Option<iced::window::Icon> {
    const PNG: &[u8] = include_bytes!("../../assets/icons/64/io.github.bealsbe.PhotoPipeline.png");
    let px = crate::imaging::decode::decode_png(PNG).filter(|p| p.channels == 4)?;
    iced::window::icon::from_rgba(px.data, px.width, px.height).ok()
}

// ── drawing ─────────────────────────────────────────────────────────────────

struct Pen<'p> {
    pm: &'p mut Pixmap,
    u: f32,
    paint: Paint<'static>,
    stroke: Stroke,
}

impl Pen<'_> {
    fn stroke(&mut self, pb: PathBuilder) {
        if let Some(p) = pb.finish() {
            self.pm.stroke_path(&p, &self.paint, &self.stroke, Transform::identity(), None);
        }
    }

    fn fill(&mut self, pb: PathBuilder) {
        if let Some(p) = pb.finish() {
            self.pm.fill_path(&p, &self.paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    /// Polyline through 24-grid points.
    fn line(&mut self, pts: &[(f32, f32)]) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 {
                pb.move_to(x * self.u, y * self.u);
            } else {
                pb.line_to(x * self.u, y * self.u);
            }
        }
        self.stroke(pb);
    }

    fn closed(&mut self, pts: &[(f32, f32)]) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 {
                pb.move_to(x * self.u, y * self.u);
            } else {
                pb.line_to(x * self.u, y * self.u);
            }
        }
        pb.close();
        self.stroke(pb);
    }

    /// Open polyline through 24-grid points with its inner corners rounded
    /// to `r` (one stroke, so the joins don't double up).
    fn rounded(&mut self, pts: &[(f32, f32)], r: f32) {
        let u = self.u;
        let k = 0.5523;
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 * u, pts[0].1 * u);
        for i in 1..pts.len() {
            let (x, y) = pts[i];
            if i + 1 == pts.len() {
                pb.line_to(x * u, y * u);
                break;
            }
            let ((px, py), (nx, ny)) = (pts[i - 1], pts[i + 1]);
            let l0 = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            let l1 = ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
            let rr = r.min(l0 / 2.0).min(l1 / 2.0);
            let a = (x + (px - x) / l0 * rr, y + (py - y) / l0 * rr);
            let b = (x + (nx - x) / l1 * rr, y + (ny - y) / l1 * rr);
            pb.line_to(a.0 * u, a.1 * u);
            pb.cubic_to(
                (a.0 + (x - a.0) * k) * u,
                (a.1 + (y - a.1) * k) * u,
                (b.0 + (x - b.0) * k) * u,
                (b.1 + (y - b.1) * k) * u,
                b.0 * u,
                b.1 * u,
            );
        }
        self.stroke(pb);
    }

    fn rrect_path(&self, x: f32, y: f32, w: f32, h: f32, r: f32) -> PathBuilder {
        let u = self.u;
        let (x, y, w, h, r) = (x * u, y * u, w * u, h * u, r * u);
        let k = 0.5523 * r;
        let mut pb = PathBuilder::new();
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
        pb.line_to(x, y + r);
        pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
        pb.close();
        pb
    }

    fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let pb = self.rrect_path(x, y, w, h, r);
        self.stroke(pb);
    }

    fn fill_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let pb = self.rrect_path(x, y, w, h, r);
        self.fill(pb);
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        if let Some(p) = PathBuilder::from_circle(cx * self.u, cy * self.u, r * self.u) {
            self.pm.stroke_path(&p, &self.paint, &self.stroke, Transform::identity(), None);
        }
    }

    fn dot(&mut self, cx: f32, cy: f32, r: f32) {
        if let Some(p) = PathBuilder::from_circle(cx * self.u, cy * self.u, r * self.u) {
            self.pm.fill_path(&p, &self.paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    /// A path in SVG's notation on the 24 grid, stroked.
    fn path(&mut self, d: &str) {
        let pb = svg_path(d, self.u);
        self.stroke(pb);
    }

    /// Circular arc from `a0` to `a1` degrees (0 = east, clockwise on screen).
    fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
        let steps = 24;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.line(&pts);
    }
}

/// Grid units per raster pixel under which a detailed glyph draws its
/// simpler form: shown at about 18 logical pixels or less (the raster is
/// twice the display size).
const SMALL_U: f32 = 1.55;

fn render(glyph: Glyph, size: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let u = size as f32 / 24.0;
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
    paint.anti_alias = true;
    let mut stroke = Stroke::default();
    // 1.6 units, but never thinner than ~1.2 display pixels (the raster is
    // 2× the display size).
    stroke.width = (1.6 * u).max(2.4);
    stroke.line_cap = LineCap::Round;
    stroke.line_join = LineJoin::Round;
    let mut p = Pen { pm: &mut pm, u, paint, stroke };
    draw(&mut p, glyph);
    unpremultiply(pm.take())
}

fn draw(p: &mut Pen<'_>, glyph: Glyph) {
    use Glyph::*;
    match glyph {
        Menu => {
            p.line(&[(4.0, 7.0), (20.0, 7.0)]);
            p.line(&[(4.0, 12.0), (20.0, 12.0)]);
            p.line(&[(4.0, 17.0), (20.0, 17.0)]);
        }
        Folder => {
            p.closed(&[(3.0, 6.0), (9.5, 6.0), (11.5, 8.0), (21.0, 8.0), (21.0, 19.0), (3.0, 19.0)]);
        }
        FolderOpen => {
            p.line(&[(4.0, 19.0), (3.0, 19.0), (3.0, 5.5), (9.0, 5.5), (11.0, 7.5), (18.5, 7.5), (18.5, 10.5)]);
            p.closed(&[(3.0, 19.0), (6.2, 10.5), (21.5, 10.5), (18.3, 19.0)]);
        }
        Library => {
            p.rrect(3.0, 4.0, 18.0, 4.5, 1.0);
            p.line(&[(4.5, 8.5), (4.5, 20.0), (19.5, 20.0), (19.5, 8.5)]);
            p.line(&[(10.0, 12.5), (14.0, 12.5)]);
        }
        Export => {
            p.line(&[(4.0, 14.0), (4.0, 20.0), (20.0, 20.0), (20.0, 14.0)]);
            p.line(&[(12.0, 3.5), (12.0, 15.0)]);
            p.line(&[(7.5, 10.5), (12.0, 15.0), (16.5, 10.5)]);
        }
        EditCopy => {
            // A copy (the sheet behind) with a pencil: copied out to edit.
            p.rounded(&[(3.5, 14.5), (3.5, 3.5), (14.5, 3.5)], 2.5);
            p.rounded(&[(13.0, 8.5), (8.5, 8.5), (8.5, 20.5), (20.5, 20.5), (20.5, 16.0)], 2.5);
            p.closed(&[(12.5, 16.5), (15.47, 15.79), (20.7, 10.56), (18.44, 8.3), (13.21, 13.53)]);
        }
        Trash => {
            p.line(&[(4.0, 7.0), (20.0, 7.0)]);
            p.line(&[(9.5, 7.0), (9.5, 4.5), (14.5, 4.5), (14.5, 7.0)]);
            p.line(&[(6.0, 7.0), (7.0, 20.0), (17.0, 20.0), (18.0, 7.0)]);
            p.line(&[(10.0, 11.0), (10.0, 16.0)]);
            p.line(&[(14.0, 11.0), (14.0, 16.0)]);
        }
        Sort => {
            // One stream splitting into two.
            p.line(&[(3.5, 12.0), (8.5, 12.0)]);
            p.line(&[(8.5, 12.0), (13.0, 7.0), (20.0, 7.0)]);
            p.line(&[(8.5, 12.0), (13.0, 17.0), (20.0, 17.0)]);
            p.line(&[(17.0, 4.0), (20.0, 7.0), (17.0, 10.0)]);
            p.line(&[(17.0, 14.0), (20.0, 17.0), (17.0, 20.0)]);
        }
        Grid => {
            for (x, y) in [(4.0, 4.0), (13.5, 4.0), (4.0, 13.5), (13.5, 13.5)] {
                p.rrect(x, y, 6.5, 6.5, 1.5);
            }
        }
        Calendar => {
            p.rrect(3.5, 5.0, 17.0, 15.0, 2.5);
            p.line(&[(3.5, 9.5), (20.5, 9.5)]);
            p.line(&[(8.0, 3.0), (8.0, 6.5)]);
            p.line(&[(16.0, 3.0), (16.0, 6.5)]);
            for y in [13.2, 16.6] {
                for x in [8.0, 12.0, 16.0] {
                    p.dot(x, y, 1.15);
                }
            }
        }
        Select => {
            p.rrect(4.0, 4.0, 16.0, 16.0, 3.5);
            p.line(&[(8.0, 12.3), (10.8, 15.0), (16.0, 9.2)]);
        }
        Keep => {
            p.circle(12.0, 12.0, 8.5);
            p.line(&[(8.2, 12.3), (10.9, 15.0), (15.9, 9.5)]);
        }
        Prune => {
            p.circle(12.0, 12.0, 8.5);
            p.line(&[(9.0, 9.0), (15.0, 15.0)]);
            p.line(&[(15.0, 9.0), (9.0, 15.0)]);
        }
        Keyboard => {
            p.rrect(2.5, 6.0, 19.0, 12.0, 2.0);
            for y in [9.5, 12.5] {
                for x in [6.0, 9.0, 12.0, 15.0, 18.0] {
                    p.dot(x, y, 0.9);
                }
            }
            p.line(&[(8.0, 15.5), (16.0, 15.5)]);
        }
        ChevronLeft => p.line(&[(14.5, 5.5), (8.0, 12.0), (14.5, 18.5)]),
        ChevronRight => p.line(&[(9.5, 5.5), (16.0, 12.0), (9.5, 18.5)]),
        ChevronUp => p.line(&[(5.5, 14.5), (12.0, 8.0), (18.5, 14.5)]),
        ChevronDown => p.line(&[(5.5, 9.5), (12.0, 16.0), (18.5, 9.5)]),
        Swap => {
            p.line(&[(5.0, 8.0), (18.5, 8.0)]);
            p.line(&[(15.0, 4.5), (18.5, 8.0), (15.0, 11.5)]);
            p.line(&[(19.0, 16.0), (5.5, 16.0)]);
            p.line(&[(9.0, 12.5), (5.5, 16.0), (9.0, 19.5)]);
        }
        Crop => {
            p.line(&[(7.0, 3.0), (7.0, 17.0), (21.0, 17.0)]);
            p.line(&[(3.0, 7.0), (17.0, 7.0), (17.0, 21.0)]);
        }
        Orientation => {
            p.line(&[(8.0, 19.5), (8.0, 4.5)]);
            p.line(&[(4.5, 8.0), (8.0, 4.5), (11.5, 8.0)]);
            p.line(&[(16.0, 4.5), (16.0, 19.5)]);
            p.line(&[(12.5, 16.0), (16.0, 19.5), (19.5, 16.0)]);
        }
        ZoomIn | ZoomOut => {
            p.circle(10.5, 10.5, 6.5);
            p.line(&[(15.3, 15.3), (20.0, 20.0)]);
            p.line(&[(7.7, 10.5), (13.3, 10.5)]);
            if glyph == ZoomIn {
                p.line(&[(10.5, 7.7), (10.5, 13.3)]);
            }
        }
        Fit => {
            p.line(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)]);
            p.line(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)]);
            p.line(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)]);
            p.line(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)]);
        }
        Info => {
            p.circle(12.0, 12.0, 8.5);
            p.line(&[(12.0, 11.0), (12.0, 16.5)]);
            p.dot(12.0, 7.9, 1.2);
        }
        Refresh => {
            p.arc(12.0, 12.0, 7.0, -50.0, 250.0);
            // Arrowhead at the arc's start (upper right), pointing along it.
            p.line(&[(13.8, 3.6), (16.5, 6.6), (12.6, 7.6)]);
        }
        Close => {
            p.line(&[(6.5, 6.5), (17.5, 17.5)]);
            p.line(&[(17.5, 6.5), (6.5, 17.5)]);
        }
        Check => p.line(&[(5.0, 12.5), (9.8, 17.0), (19.0, 7.0)]),
        ArrowUp => {
            p.line(&[(12.0, 19.5), (12.0, 5.0)]);
            p.line(&[(6.5, 10.5), (12.0, 5.0), (17.5, 10.5)]);
        }
        ArrowDown => {
            p.line(&[(12.0, 4.5), (12.0, 19.0)]);
            p.line(&[(6.5, 13.5), (12.0, 19.0), (17.5, 13.5)]);
        }
        Warning => {
            p.closed(&[(12.0, 3.8), (21.0, 19.5), (3.0, 19.5)]);
            p.line(&[(12.0, 9.5), (12.0, 14.0)]);
            p.dot(12.0, 16.9, 1.2);
        }
        Scissors => {
            p.circle(6.0, 7.5, 3.0);
            p.circle(6.0, 16.5, 3.0);
            p.line(&[(8.6, 9.3), (20.5, 17.5)]);
            p.line(&[(8.6, 14.7), (20.5, 6.5)]);
        }
        Skip => {
            let mut pb = PathBuilder::new();
            for x in [3.5, 10.5] {
                pb.move_to(x * p.u, 6.0 * p.u);
                pb.line_to((x + 7.0) * p.u, 12.0 * p.u);
                pb.line_to(x * p.u, 18.0 * p.u);
                pb.close();
            }
            p.fill(pb);
            p.fill_rrect(18.3, 6.0, 2.2, 12.0, 0.5);
        }
        Photos => {
            p.rrect(3.0, 5.0, 18.0, 14.0, 2.5);
            p.line(&[(3.5, 16.5), (9.0, 11.0), (13.0, 15.0), (15.5, 12.5), (20.5, 17.5)]);
            p.circle(15.5, 9.0, 1.6);
        }
        Undo => {
            p.line(&[(8.5, 4.5), (4.5, 8.5), (8.5, 12.5)]);
            p.line(&[(4.5, 8.5), (14.5, 8.5)]);
            p.arc(14.5, 13.75, 5.25, -90.0, 90.0);
            p.line(&[(14.5, 19.0), (9.0, 19.0)]);
        }
        Redo => {
            p.line(&[(15.5, 4.5), (19.5, 8.5), (15.5, 12.5)]);
            p.line(&[(19.5, 8.5), (9.5, 8.5)]);
            p.arc(9.5, 13.75, 5.25, 270.0, 90.0);
            p.line(&[(9.5, 19.0), (15.0, 19.0)]);
        }
        Compare => {
            // Two frames side by side, a mountain in each.
            p.rrect(2.5, 4.0, 8.5, 16.0, 1.5);
            p.rrect(13.0, 4.0, 8.5, 16.0, 1.5);
            p.line(&[(4.5, 16.5), (6.75, 13.0), (9.0, 16.5)]);
            p.line(&[(15.0, 16.5), (17.25, 13.0), (19.5, 16.5)]);
        }
        Filmstrip => {
            // The photo, with its neighbours in a row beneath.
            p.rrect(3.5, 3.5, 17.0, 11.0, 2.0);
            for x in [3.5, 9.75, 16.0] {
                p.rrect(x, 17.0, 4.5, 3.5, 1.0);
            }
        }
        FullScreen => {
            for (cx, cy, dx, dy) in [(4.0, 4.0, 1.0, 1.0), (20.0, 4.0, -1.0, 1.0), (4.0, 20.0, 1.0, -1.0), (20.0, 20.0, -1.0, -1.0)] {
                p.line(&[(cx + 5.5 * dx, cy), (cx, cy), (cx, cy + 5.5 * dy)]);
                p.line(&[(cx, cy), (cx + 5.0 * dx, cy + 5.0 * dy)]);
            }
        }
        Network => {
            p.rrect(4.0, 3.5, 16.0, 9.0, 2.0);
            p.dot(8.0, 8.0, 1.1);
            p.line(&[(12.0, 12.5), (12.0, 17.0)]);
            p.circle(12.0, 19.3, 2.3);
            p.line(&[(3.5, 19.3), (9.7, 19.3)]);
            p.line(&[(14.3, 19.3), (20.5, 19.3)]);
        }
        Settings => {
            // Three sliders, their knobs set at different points.
            for (y, x) in [(6.0, 15.5), (12.0, 8.5), (18.0, 13.5)] {
                p.line(&[(3.5, y), (x - 2.6, y)]);
                p.line(&[(x + 2.6, y), (20.5, y)]);
                p.circle(x, y, 2.6);
            }
        }
        Analyse => {
            p.line(&[(3.5, 8.5), (3.5, 3.5), (8.5, 3.5)]);
            p.line(&[(15.5, 3.5), (20.5, 3.5), (20.5, 8.5)]);
            p.line(&[(20.5, 15.5), (20.5, 20.5), (15.5, 20.5)]);
            p.line(&[(8.5, 20.5), (3.5, 20.5), (3.5, 15.5)]);
            p.circle(12.0, 12.0, 3.6);
        }
        HintSharp => {
            p.circle(12.0, 12.0, 7.5);
            p.dot(12.0, 12.0, 2.4);
        }
        HintSoft => {
            // The sharp ring broken into six dashes, with no point in it.
            for k in 0..6 {
                let a = k as f32 * 60.0 - 90.0;
                p.arc(12.0, 12.0, 7.5, a + 12.0, a + 45.0);
            }
        }
        HintAccident => {
            p.rrect(3.5, 5.5, 17.0, 13.0, 2.0);
        }
        HintCopy => {
            p.rounded(&[(4.0, 15.5), (4.0, 4.0), (15.5, 4.0)], 2.5);
            p.rrect(8.5, 8.5, 11.5, 11.5, 2.0);
        }
        Search => {
            p.circle(10.5, 10.5, 6.5);
            p.line(&[(15.3, 15.3), (20.0, 20.0)]);
        }
        GraphicsCard => {
            p.rrect(2.5, 6.0, 19.0, 11.0, 2.0);
            p.circle(8.5, 11.5, 2.6);
            p.circle(15.5, 11.5, 2.6);
            for x in [6.0, 10.0, 14.0] {
                p.line(&[(x, 17.0), (x, 19.5)]);
            }
        }
        Clock => {
            p.circle(12.0, 12.0, 8.5);
            p.line(&[(12.0, 7.5), (12.0, 12.0), (15.0, 14.0)]);
        }
        LeaveOut => {
            p.circle(12.0, 12.0, 8.5);
            p.line(&[(8.0, 12.0), (16.0, 12.0)]);
        }
        Subjects => {
            p.circle(7.6, 7.8, 3.0);
            p.path("M2.6 19c.5-3.4 2.5-5.3 5-5.3 1.1 0 2 .3 2.8.9");
            p.circle(16.6, 9.6, 2.4);
            p.path("M12.8 19.4c.4-2.8 1.8-4.4 3.8-4.4s3.4 1.6 3.8 4.4");
        }
        Person => {
            p.circle(12.0, 8.5, 3.5);
            p.path("M5 20c.8-3.8 3.6-6 7-6s6.2 2.2 7 6");
        }
        Fursuit if p.u < SMALL_U => {
            // Small (tabs, buttons, chips): the head's outline with one
            // tuft a side, the eyes and a solid nose.  The inner-ear lines
            // and the muzzle would run together at this size.
            p.path("M5.9 10.3C4.5 7.8 3.9 5.2 4 2.9c2.6.5 4.8 1.9 6.3 3.4q1.7-.4 3.4 0c1.5-1.5 3.7-2.9 6.3-3.4.1 2.3-.5 4.9-1.9 7.4.8 1.1 1.2 2.4 1.2 3.8l1.6 1.2-1.7.4c-1.1 2.6-3.4 4.1-6.1 4.1s-5-1.5-6.1-4.1l-1.7-.4 1.6-1.2c0-1.4.4-2.7 1.2-3.8z");
            p.dot(9.2, 11.9, 1.35);
            p.dot(14.8, 11.9, 1.35);
            let mut nose = PathBuilder::new();
            nose.move_to(10.6 * p.u, 14.5 * p.u);
            nose.line_to(13.4 * p.u, 14.5 * p.u);
            nose.line_to(12.0 * p.u, 16.2 * p.u);
            nose.close();
            p.fill(nose);
        }
        Fursuit => {
            // Full ears, cheek ruffs and a muzzle with its nose.
            p.path("M5.9 10.3C4.5 7.8 3.9 5.2 4 2.9c2.6.5 4.8 1.9 6.3 3.4q1.7-.4 3.4 0c1.5-1.5 3.7-2.9 6.3-3.4.1 2.3-.5 4.9-1.9 7.4.8 1.1 1.2 2.4 1.2 3.8l1.9 1.1-2.1.5.8 1.4-1.9-.2c-1.2 2.3-3.4 3.6-6 3.6s-4.8-1.3-6-3.6l-1.9.2.8-1.4-2.1-.5 1.9-1.1c0-1.4.4-2.7 1.2-3.8z");
            p.path("M6.2 5.4c.6 1.1 1.3 2 2.2 2.7M17.8 5.4c-.6 1.1-1.3 2-2.2 2.7M10.9 14.7h2.2L12 16zM12 16c-.3.9-1 1.3-1.8 1.1M12 16c.3.9 1 1.3 1.8 1.1");
            p.dot(9.2, 11.8, 1.1);
            p.dot(14.8, 11.8, 1.1);
        }
        Merge => p.path("M6 4v4.5c0 3.5 6 4.5 6 8.5v3M18 4v4.5c0 3.5-6 4.5-6 8.5"),
        Split => p.path("M12 4v4c0 3.5-6 4.5-6 8.5V20M12 8c0 3.5 6 4.5 6 8.5V20"),
        Hide => {
            p.path("M3 12s3.5-6 9-6c1.6 0 3 .5 4.2 1.2M21 12s-3.5 6-9 6c-1.6 0-3-.5-4.2-1.2");
            p.line(&[(4.0, 4.0), (20.0, 20.0)]);
            p.path("M9.9 9.9a3 3 0 0 0 4.2 4.2");
        }
        Pencil => {
            p.closed(&[(4.0, 20.0), (8.0, 20.0), (19.0, 9.0), (15.0, 5.0), (4.0, 16.0)]);
            p.line(&[(13.0, 7.0), (17.0, 11.0)]);
        }
        More => {
            for x in [6.0, 12.0, 18.0] {
                p.dot(x, 12.0, 1.5);
            }
        }
        Plus => {
            p.line(&[(12.0, 5.0), (12.0, 19.0)]);
            p.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        CloseCall => {
            // Two heads (the fursuit tiles' rounded squares) side by side
            // on a beam with its post: a pair weighed.
            p.rrect(2.5, 4.0, 8.0, 8.0, 2.5);
            p.rrect(13.5, 4.0, 8.0, 8.0, 2.5);
            p.line(&[(4.0, 16.0), (20.0, 16.0)]);
            p.line(&[(12.0, 16.0), (12.0, 20.5)]);
            p.line(&[(8.5, 20.5), (15.5, 20.5)]);
        }
    }
}

/// A small SVG path reader for the 24-grid icons: M, L, H, V, C, S, Q and
/// Z, absolute or relative, and A only as a quarter-turn-free arc drawn as
/// its chord's bulge (`a r r 0 0 s dx dy`, enough for the eye's lid).
fn svg_path(d: &str, u: f32) -> PathBuilder {
    let mut pb = PathBuilder::new();
    // Tokens: commands and numbers ("-.4", ".6", "1.2.3" = 1.2 .3).
    let mut toks: Vec<String> = Vec::new();
    let mut num = String::new();
    let flush = |num: &mut String, toks: &mut Vec<String>| {
        if !num.is_empty() {
            toks.push(std::mem::take(num));
        }
    };
    for ch in d.chars() {
        match ch {
            c if c.is_ascii_alphabetic() => {
                flush(&mut num, &mut toks);
                toks.push(c.to_string());
            }
            '-' => {
                flush(&mut num, &mut toks);
                num.push('-');
            }
            '.' if num.contains('.') => {
                flush(&mut num, &mut toks);
                num.push('.');
            }
            c if c.is_ascii_digit() || c == '.' => num.push(c),
            _ => flush(&mut num, &mut toks),
        }
    }
    flush(&mut num, &mut toks);
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    // The last control point, for S.
    let mut last_c: Option<(f32, f32)> = None;
    let mut i = 0;
    let mut cmd = 'M';
    let nums = |i: &mut usize, n: usize| -> Option<Vec<f32>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(toks.get(*i)?.parse::<f32>().ok()?);
            *i += 1;
        }
        Some(v)
    };
    while i < toks.len() {
        if let Some(c) = toks[i].chars().next().filter(|c| c.is_ascii_alphabetic()) {
            cmd = c;
            i += 1;
            if c == 'z' || c == 'Z' {
                pb.close();
                (x, y) = (sx, sy);
                last_c = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                let Some(v) = nums(&mut i, 2) else { break };
                (x, y) = (ox + v[0], oy + v[1]);
                (sx, sy) = (x, y);
                pb.move_to(x * u, y * u);
                // Pairs after a move are lines.
                cmd = if rel { 'l' } else { 'L' };
                last_c = None;
            }
            'L' => {
                let Some(v) = nums(&mut i, 2) else { break };
                (x, y) = (ox + v[0], oy + v[1]);
                pb.line_to(x * u, y * u);
                last_c = None;
            }
            'H' => {
                let Some(v) = nums(&mut i, 1) else { break };
                x = if rel { x + v[0] } else { v[0] };
                pb.line_to(x * u, y * u);
                last_c = None;
            }
            'V' => {
                let Some(v) = nums(&mut i, 1) else { break };
                y = if rel { y + v[0] } else { v[0] };
                pb.line_to(x * u, y * u);
                last_c = None;
            }
            'C' => {
                let Some(v) = nums(&mut i, 6) else { break };
                let (x1, y1, x2, y2, ex, ey) = (ox + v[0], oy + v[1], ox + v[2], oy + v[3], ox + v[4], oy + v[5]);
                pb.cubic_to(x1 * u, y1 * u, x2 * u, y2 * u, ex * u, ey * u);
                last_c = Some((x2, y2));
                (x, y) = (ex, ey);
            }
            'S' => {
                let Some(v) = nums(&mut i, 4) else { break };
                let (x1, y1) = last_c.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                let (x2, y2, ex, ey) = (ox + v[0], oy + v[1], ox + v[2], oy + v[3]);
                pb.cubic_to(x1 * u, y1 * u, x2 * u, y2 * u, ex * u, ey * u);
                last_c = Some((x2, y2));
                (x, y) = (ex, ey);
            }
            'Q' => {
                let Some(v) = nums(&mut i, 4) else { break };
                let (x1, y1, ex, ey) = (ox + v[0], oy + v[1], ox + v[2], oy + v[3]);
                pb.quad_to(x1 * u, y1 * u, ex * u, ey * u);
                last_c = None;
                (x, y) = (ex, ey);
            }
            'A' => {
                // rx ry rotation large sweep x y: drawn as a quadratic
                // bulging by the radius' sagitta (small arcs only).
                let Some(v) = nums(&mut i, 7) else { break };
                let (ex, ey) = (ox + v[5], oy + v[6]);
                let (mx, my) = ((x + ex) / 2.0, (y + ey) / 2.0);
                let (dx, dy) = (ex - x, ey - y);
                let len = (dx * dx + dy * dy).sqrt().max(1e-3);
                let r = v[0].max(len / 2.0);
                let sag = r - (r * r - len * len / 4.0).max(0.0).sqrt();
                let side = if v[4] > 0.5 { 1.0 } else { -1.0 };
                let (nx, ny) = (-dy / len * side, dx / len * side);
                pb.quad_to((mx - nx * sag * 2.0) * u, (my - ny * sag * 2.0) * u, ex * u, ey * u);
                last_c = None;
                (x, y) = (ex, ey);
            }
            _ => break,
        }
    }
    pb
}

/// tiny-skia stores premultiplied RGBA; iced wants straight alpha.
fn unpremultiply(mut px: Vec<u8>) -> Vec<u8> {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            p[0] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_draws_something() {
        use Glyph::*;
        for g in [
            Menu, FolderOpen, Folder, Library, Export, EditCopy, Trash, Sort, Grid, Calendar, Select, Keep, Prune,
            Keyboard, ChevronLeft, ChevronRight, ChevronUp, ChevronDown, Swap, Crop, Orientation, ZoomIn, ZoomOut, Fit,
            Info, Refresh, Close, Check, ArrowUp, ArrowDown, Warning, Scissors, Skip, Photos, Undo, Redo, Compare, Filmstrip, FullScreen, Network,
            Settings, Analyse, HintSharp, HintSoft, HintAccident, HintCopy, Search, GraphicsCard, Clock, LeaveOut, Subjects, Person, Fursuit,
            Merge, Split, Hide, Pencil, More, Plus, CloseCall,
        ] {
            let px = render(g, 32, [255, 255, 255, 255]);
            assert!(px.chunks_exact(4).any(|p| p[3] > 128), "{g:?} is blank");
        }
    }

    #[test]
    fn window_icon_is_the_bundled_app_icon() {
        assert!(window_icon().is_some());
    }

    /// The windows, `install.sh` and the desktop entry agree on the app id,
    /// and every icon size `install.sh` installs is there.
    #[test]
    fn app_id_matches_install_and_desktop_entry() {
        use crate::app::APP_ID;
        let install = include_str!("../../install.sh");
        assert!(install.contains(&format!("\napp_id={APP_ID}\n")), "install.sh app_id");
        let entry = include_str!("../../assets/io.github.bealsbe.PhotoPipeline.desktop.in");
        for key in ["Icon", "StartupWMClass"] {
            assert!(entry.contains(&format!("\n{key}={APP_ID}\n")), "desktop entry {key}");
        }
        let sizes = install.lines().find_map(|l| l.strip_prefix("sizes=(")).and_then(|l| l.strip_suffix(')')).expect("sizes");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons");
        assert!(root.join(format!("{APP_ID}.svg")).is_file());
        for s in sizes.split_whitespace() {
            assert!(root.join(s).join(format!("{APP_ID}.png")).is_file(), "{s} px icon");
        }
    }

    /// Where a glyph rendered at `size` raster pixels has ink, sampled on
    /// the 24 grid (a 3×3 neighbourhood, so a stroke's edge counts).
    fn ink(g: Glyph, size: u32) -> impl Fn(f32, f32) -> bool {
        let px = render(g, size, [255, 255, 255, 255]);
        move |x: f32, y: f32| {
            let n = size as i64;
            let (cx, cy) = ((x / 24.0 * size as f32) as i64, (y / 24.0 * size as f32) as i64);
            (cy - 1..=cy + 1).any(|yy| (cx - 1..=cx + 1).any(|xx| xx >= 0 && yy >= 0 && xx < n && yy < n && px[((yy * n + xx) * 4 + 3) as usize] > 96))
        }
    }

    #[test]
    fn the_fursuit_head_keeps_its_ears_eyes_and_nose_at_every_size() {
        // Shown at 12 px (the small form) up to 44 px (the full drawing).
        for size in [24, 30, 36, 48, 88] {
            let at = ink(Glyph::Fursuit, size);
            assert!(at(4.0, 3.2) && at(20.0, 3.2), "{size}: both ear tips");
            assert!(at(9.2, 11.8) && at(14.8, 11.8), "{size}: both eyes");
            assert!(at(12.0, 15.0), "{size}: the nose");
            assert!(!at(12.0, 2.5), "{size}: open between the ears, never a round head");
        }
        // Small, the inner-ear lines go (they run into the outline there).
        assert!(!ink(Glyph::Fursuit, 30)(7.9, 7.6), "small: no inner-ear line");
        assert!(ink(Glyph::Fursuit, 48)(7.9, 7.6), "full size: the inner-ear line");
    }
}
