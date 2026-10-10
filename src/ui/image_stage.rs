//! The viewer's image area: fit / zoom / pan, the push-slide between photos,
//! the crop overlay, and touch swipe / pinch.
//!
//! The app declares what to show (a display key, the decoded image, how to
//! transition, zoom commands, the crop rect); this widget owns the view
//! transform and every animation, so panning and zooming never round-trip
//! through the app.
//!
//! Slides are drawn from state rather than screenshots: the outgoing frame
//! is the previous image at its last transform (or an in-flight slide, so
//! rapid key presses chain smoothly), the incoming one is the new image
//! fitted.  Images are drawn from a mip level close to their on-screen size,
//! with the next coarser level underneath so a level still uploading to the
//! GPU never leaves a hole.
//!
//! On a burst the stage can show the burst's sheet instead of one photo
//! (`ui::burst_sheet`): every frame as a tile.  The sheet takes part in
//! slides like a photo does, and going between the sheet and a frame zooms:
//! the frame grows from its tile to the stage, or shrinks back onto it from
//! wherever it was zoomed and panned to.  A zoom that turns round part-way
//! starts from the frame on screen.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse as amouse};
use iced::mouse::{self, Cursor, ScrollDelta};
use iced::widget::image::FilterMethod;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Vector, touch, window};

use super::burst_sheet::{self, Sheet};
use super::paint::{self, HAlign, Renderer, rect};
use super::theme as t;
use crate::imaging::loader::FullImage;
use crate::model::crop::Crop;
use crate::model::crop_math::{self, Region};

const MIN_SCALE: f32 = 0.02;
const MAX_SCALE: f32 = 32.0;
const STEP: f32 = 1.2;
const SLIDE_MS: f32 = 320.0;
const ZOOM_MS: f32 = 260.0;
const GRAB: f32 = 14.0;
const SB: f32 = 7.0;
/// Wheel travel (pixels; a notch is 120) that changes level between a
/// burst's sheet and one frame.
const LEVEL_WHEEL: f32 = 100.0;
/// Wheel events this soon after a change of level are swallowed, so a
/// flick's tail doesn't carry on into the next level.
const LEVEL_COOLDOWN_MS: f32 = 380.0;
/// A pause this long starts the wheel travel afresh.
const WHEEL_IDLE_MS: f32 = 400.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transition {
    /// Show the new image in place.
    Cut,
    /// Push-slide: +1 forward (old exits left), -1 backward.
    Slide(i32),
    /// A touch swipe already animated this change; keep the running slide.
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewCmd {
    None,
    Fit,
    Actual,
    /// 100 % centred on the camera's AF point, where the stage knows it
    /// (the compare view's burst review); elsewhere the same as `Actual`.
    Focus,
    ZoomIn,
    ZoomOut,
}

#[derive(Debug, Clone)]
pub struct CropProps {
    pub crop: Crop,
    /// Aspect constraint in normalised units (see `crop_math`).
    pub ratio: Option<f64>,
}

/// The viewer's labels over the photo: boxes (fractions of the image, as
/// the detections are kept), the one selected, and a box being drawn.
/// While they are shown a drag on the photo draws a box and a click
/// picks one; the wheel still pans and zooms.
#[derive(Debug, Clone, Default)]
pub struct LabelProps {
    pub boxes: Vec<StageBox>,
    pub selected: Option<usize>,
    /// A drawn box waiting for its kind, and the colour it would take.
    pub draft: Option<([f32; 4], Color)>,
}

/// One label box as the stage draws it.
#[derive(Debug, Clone)]
pub struct StageBox {
    /// `[x, y, w, h]`, fractions of the image.
    pub bbox: [f32; 4],
    pub color: Color,
    /// Its tag: the name, or "Unnamed".
    pub tag: String,
    /// Named: a solid tag; unnamed: a hollow one.
    pub named: bool,
    /// Drawn and not matched yet: a dashed outline.
    pub dashed: bool,
}

#[derive(Debug, Clone)]
pub enum StageEvent {
    Zoom(f32),
    Crop(Crop),
    Swipe(i32),
    Viewport(Size),
    Sheet(SheetEvent),
    /// A label box was clicked (or the photo, away from every box).
    LabelPick(Option<usize>),
    /// A box was drawn on the photo (fractions of the image).
    LabelDraw([f32; 4]),
}

/// What the burst sheet reports.
#[derive(Debug, Clone, PartialEq)]
pub enum SheetEvent {
    /// A tile was clicked, or the wheel zoomed in over it: show that frame.
    Open(usize),
    /// Ctrl+click: pick the frame to compare, or put it back.
    Pick(usize),
    /// The wheel zoomed out past fit on a frame: back to the sheet.
    Back,
    /// Tiles with no thumbnail and none on its way.
    Thumbs(Vec<usize>),
}

pub struct ImageStage<'a, Message> {
    key: u64,
    image: Option<Arc<FullImage>>,
    status: Option<String>,
    transition: Transition,
    cmd: (u64, ViewCmd),
    crop: Option<CropProps>,
    neighbors: [Option<Arc<FullImage>>; 2],
    scale_factor: f32,
    /// The burst the photo belongs to, as a sheet (shown or not).
    sheet: Option<Sheet>,
    /// The viewer's labels, while shown.
    labels: Option<LabelProps>,
    on: Box<dyn Fn(StageEvent) -> Message + 'a>,
}

impl<'a, Message> ImageStage<'a, Message> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: u64,
        image: Option<Arc<FullImage>>,
        status: Option<String>,
        transition: Transition,
        cmd: (u64, ViewCmd),
        crop: Option<CropProps>,
        neighbors: [Option<Arc<FullImage>>; 2],
        scale_factor: f32,
        on: impl Fn(StageEvent) -> Message + 'a,
    ) -> Self {
        Self { key, image, status, transition, cmd, crop, neighbors, scale_factor, sheet: None, labels: None, on: Box::new(on) }
    }

    /// The viewer's labels over the photo (`None`: not shown).
    pub fn labels(mut self, labels: Option<LabelProps>) -> Self {
        self.labels = labels;
        self
    }

    /// The photo is a frame of a burst: its sheet, on screen when
    /// `sheet.shown`, and what zooming out goes back to otherwise.
    pub fn sheet(mut self, sheet: Option<Sheet>) -> Self {
        self.sheet = sheet;
        self
    }
}

/// The image on screen and how it is placed.
#[derive(Clone)]
struct Live {
    img: Arc<FullImage>,
    /// When true the transform is recomputed from the viewport (fit mode).
    fit: bool,
    scale: f32,
    /// Image origin in viewport coordinates.
    off: Vector,
}

/// A frozen frame used as the outgoing side of a slide.
#[derive(Clone)]
enum Snap {
    Blank,
    Image { img: Arc<FullImage>, scale: f32, off: Vector },
    /// A burst's sheet, at rest.
    Sheet(Sheet),
    Slide { old: Box<Snap>, new: Box<Snap>, dir: i32, p: f32 },
}

/// The zoom between a sheet tile and the whole frame: the frame's photo
/// moving from one rectangle (stage coordinates) to another while the
/// sheet's opacity goes from one value to another (see
/// `burst_sheet::zoom_at`).
#[derive(Clone, Copy)]
struct SheetZoom {
    tile: usize,
    from: (Rectangle, f32),
    to: (Rectangle, f32),
    start: Instant,
}

impl SheetZoom {
    fn t(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / burst_sheet::ZOOM_MS).clamp(0.0, 1.0)
    }

    /// The photo's rectangle and the sheet's opacity now.
    fn at(&self, now: Instant) -> (Rectangle, f32) {
        burst_sheet::zoom_at(self.from, self.to, paint::out_cubic(self.t(now)))
    }
}

impl Snap {
    fn depth(&self) -> usize {
        match self {
            Snap::Slide { old, .. } => 1 + old.depth(),
            _ => 0,
        }
    }
}

#[derive(Clone)]
struct SlideAnim {
    old: Snap,
    dir: i32,
    from: f32,
    to: f32,
    start: Instant,
    ms: f32,
    /// A cancelled swipe restores this image when done.
    restore: Option<Live>,
}

#[derive(Clone, Copy)]
struct ZoomAnim {
    s0: f32,
    s1: f32,
    c0: Point,
    c1: Point,
    start: Instant,
    to_fit: bool,
}

struct Swipe {
    origin: Point,
    t0: Instant,
    dx: f32,
    active: bool,
    dir: i32,
    old: Option<Live>,
}

struct State {
    key: u64,
    live: Option<Live>,
    slide: Option<SlideAnim>,
    pending_old: Option<Snap>,
    zoom: Option<ZoomAnim>,
    cmd_seq: u64,
    viewport: Size,
    pan: Option<(Point, Vector)>,
    crop_drag: Option<(Region, (f64, f64), Crop)>,
    hover_region: Region,
    last_click: Option<amouse::Click>,
    swipe: Option<Swipe>,
    fingers: HashMap<touch::Finger, Point>,
    pinch: Option<(f32, f32, Point)>,
    last_zoom_pub: i32,
    sb_drag: Option<(bool, f32)>,
    reported_vp: Size,
    /// Direction for a slide deferred until the image has loaded.
    pending_dir: i32,
    /// The burst sheet as last given (the outgoing side of a slide).
    sheet: Option<Sheet>,
    sheet_zoom: Option<SheetZoom>,
    /// Where the outline is sliding from (a mount, stage coordinates).
    ring_from: Option<(Rectangle, Instant)>,
    /// The tile under the pointer and since when; the one before it and
    /// when it was left (hover cross-fades).
    tile_hover: Option<(usize, Instant)>,
    tile_hover_prev: Option<(usize, Instant)>,
    /// The wheel going between the sheet and a frame.
    level: LevelWheel,
    /// A press on the photo while labels show: where (stage and image
    /// fractions) and where the pointer is now (image fractions).  A
    /// short one is a click; a longer one draws a box.
    label_drag: Option<(Point, (f64, f64), (f64, f64))>,
    /// The label box under the pointer.
    label_hover: Option<usize>,
}

/// The wheel's part in changing level between a burst's sheet and one of
/// its frames: enough travel one way changes level; a pause starts the
/// count afresh; and just after a change (however it was made) the wheel
/// is swallowed, so a flick's tail doesn't carry on into the next level.
#[derive(Debug, Clone, Copy, Default)]
struct LevelWheel {
    acc: f32,
    at: Option<Instant>,
    changed: Option<Instant>,
}

impl LevelWheel {
    fn ms(since: Option<Instant>, now: Instant) -> f32 {
        since.map_or(f32::MAX, |s| now.saturating_duration_since(s).as_secs_f32() * 1000.0)
    }

    /// The level just changed.
    fn changed(&mut self, now: Instant) {
        self.changed = Some(now);
        self.acc = 0.0;
    }

    /// Still settling after a change: the wheel does nothing.
    fn settling(&self, now: Instant) -> bool {
        Self::ms(self.changed, now) < LEVEL_COOLDOWN_MS
    }

    /// Wheel travel of `amount` (pixels) towards the other level, or (≤ 0)
    /// away from it.  True when it is time to change.
    fn turn(&mut self, amount: f32, now: Instant) -> bool {
        if Self::ms(self.at, now) > WHEEL_IDLE_MS || amount <= 0.0 {
            self.acc = 0.0;
        }
        self.at = Some(now);
        if amount <= 0.0 || self.settling(now) {
            self.acc = 0.0;
            return false;
        }
        self.acc += amount;
        if self.acc >= LEVEL_WHEEL {
            self.changed(now);
            return true;
        }
        false
    }
}

fn fit_scale(img: &FullImage, vp: Size) -> f32 {
    if img.width == 0 || img.height == 0 || vp.width <= 0.0 || vp.height <= 0.0 {
        return 1.0;
    }
    (vp.width / img.width as f32).min(vp.height / img.height as f32)
}

/// Keep the image centred when smaller than the viewport, and edge-to-edge
/// otherwise (a QGraphicsView's scroll range).
fn clamp_off(img: &FullImage, scale: f32, off: Vector, vp: Size) -> Vector {
    let (w, h) = (img.width as f32 * scale, img.height as f32 * scale);
    let x = if w <= vp.width { (vp.width - w) / 2.0 } else { off.x.clamp(vp.width - w, 0.0) };
    let y = if h <= vp.height { (vp.height - h) / 2.0 } else { off.y.clamp(vp.height - h, 0.0) };
    Vector::new(x, y)
}

impl Live {
    fn new(img: Arc<FullImage>) -> Live {
        Live { img, fit: true, scale: 1.0, off: Vector::ZERO }
    }

    fn transform(&self, vp: Size) -> (f32, Vector) {
        if self.fit {
            let s = fit_scale(&self.img, vp);
            (s, clamp_off(&self.img, s, Vector::ZERO, vp))
        } else {
            (self.scale, self.off)
        }
    }

    fn snap(&self, vp: Size) -> Snap {
        let (scale, off) = self.transform(vp);
        Snap::Image { img: self.img.clone(), scale, off }
    }

}

impl State {
    /// What shows when no slide runs: the sheet (or a zoom mostly showing
    /// it), else the photo.
    fn live_snap(&self, now: Instant) -> Snap {
        if let Some(sheet) = &self.sheet {
            match self.sheet_zoom.map(|z| z.at(now)) {
                Some((_, alpha)) if alpha > 0.5 => return Snap::Sheet(sheet.clone()),
                // Mostly the photo: as far as it has grown.
                Some((photo, _)) => {
                    return match &self.live {
                        Some(l) => Snap::Image { img: l.img.clone(), scale: photo.width / l.img.width.max(1) as f32, off: Vector::new(photo.x, photo.y) },
                        None => Snap::Blank,
                    };
                }
                None if sheet.shown => return Snap::Sheet(sheet.clone()),
                None => {}
            }
        }
        self.live.as_ref().map(|l| l.snap(self.viewport)).unwrap_or(Snap::Blank)
    }

    /// The sheet is on screen, or a zoom to or from it is running.
    fn sheet_visible(&self) -> bool {
        self.sheet_zoom.is_some() || self.sheet.as_ref().is_some_and(|s| s.shown)
    }

    /// The tiles' places on the stage (stage coordinates).
    fn sheet_cells(&self) -> Vec<burst_sheet::Cell> {
        match &self.sheet {
            Some(s) => burst_sheet::layout(&s.aspects(), Rectangle::with_size(self.viewport)),
            None => Vec::new(),
        }
    }

    /// The photo's rectangle on the stage now (stage coordinates).
    fn photo_rect(&self, now: Instant) -> Option<Rectangle> {
        let l = self.live.as_ref()?;
        let (s, off) = self.transform_at(l, now);
        Some(rect(off.x, off.y, l.img.width as f32 * s, l.img.height as f32 * s))
    }

    /// Zoom from the sheet into the outlined frame (`into`), or back out
    /// onto its tile, starting from whatever is on screen.
    fn start_sheet_zoom(&mut self, sheet: &Sheet, into: bool, now: Instant) {
        let vp = self.viewport;
        let cells = burst_sheet::layout(&sheet.aspects(), Rectangle::with_size(vp));
        let (Some(cell), Some(tile)) = (cells.get(sheet.outline), sheet.tiles.get(sheet.outline)) else {
            self.sheet_zoom = None;
            return;
        };
        if vp.width <= 0.0 || vp.height <= 0.0 {
            self.sheet_zoom = None;
            return;
        }
        let aspect = self.live.as_ref().map_or(tile.aspect, |l| l.img.width.max(1) as f32 / l.img.height.max(1) as f32);
        let fit = burst_sheet::fit_rect(aspect, Rectangle::with_size(vp));
        // Mid-slide, the zoom starts from where the slide has got to.
        let dx = self.slide.as_ref().map_or(0.0, |s| vp.width * (1.0 - slide_progress(s, now)) * s.dir as f32);
        let slid = |r: Rectangle| rect(r.x + dx, r.y, r.width, r.height);
        let from = match self.sheet_zoom {
            Some(z) => z.at(now),
            None if into => (slid(cell.photo), 1.0),
            None => (slid(self.photo_rect(now).unwrap_or(fit)), 0.0),
        };
        let to = if into { (fit, 0.0) } else { (cell.photo, 1.0) };
        // The frame comes in fitted; a slide or a zoom of the photo stops.
        self.slide = None;
        self.pending_old = None;
        self.zoom = None;
        self.pan = None;
        if into {
            if let Some(l) = self.live.as_mut() {
                l.fit = true;
            }
        }
        self.sheet_zoom = Some(SheetZoom { tile: sheet.outline, from, to, start: now });
        self.level.changed(now);
    }

    /// The outline's mount now (stage coordinates), sliding between tiles.
    fn ring_rect(&self, cells: &[burst_sheet::Cell], outline: usize, now: Instant) -> Option<Rectangle> {
        let to = cells.get(outline)?.mount;
        Some(match self.ring_from {
            Some((from, since)) => {
                let t = (now.saturating_duration_since(since).as_secs_f32() * 1000.0 / t::MOTION_MS).clamp(0.0, 1.0);
                paint::lerp_rect(from, to, paint::out_cubic(t))
            }
            None => to,
        })
    }

    fn tile_hover_amount(&self, i: usize, now: Instant) -> f32 {
        let k = |since: Instant| (now.saturating_duration_since(since).as_secs_f32() * 1000.0 / t::MOTION_FAST_MS).clamp(0.0, 1.0);
        match (self.tile_hover, self.tile_hover_prev) {
            (Some((h, since)), _) if h == i => paint::out_cubic(k(since)),
            (_, Some((p, left))) if p == i => 1.0 - paint::out_cubic(k(left)),
            _ => 0.0,
        }
    }

    fn set_tile_hover(&mut self, i: Option<usize>, now: Instant) {
        if self.tile_hover.map(|h| h.0) == i {
            return;
        }
        if let Some((old, _)) = self.tile_hover {
            self.tile_hover_prev = Some((old, now));
        }
        self.tile_hover = i.map(|i| (i, now));
    }

    fn current_snap(&self, now: Instant) -> Snap {
        let live = || self.live_snap(now);
        if let Some(s) = &self.slide {
            let p = slide_progress(s, now);
            let mut old = s.old.clone();
            if old.depth() >= 2 {
                // Keep chains shallow: collapse to whichever side dominates.
                if let Snap::Slide { old: o, new: n, p: q, .. } = old {
                    old = if q > 0.5 { *n } else { *o };
                }
            }
            return Snap::Slide { old: Box::new(old), new: Box::new(live()), dir: s.dir, p };
        }
        if let Some(old) = &self.pending_old {
            return old.clone();
        }
        live()
    }

    fn scale(&self, now: Instant) -> f32 {
        match &self.live {
            Some(l) => self.transform_at(l, now).0,
            None => 1.0,
        }
    }

    /// The live transform, following any zoom animation.
    fn transform_at(&self, l: &Live, now: Instant) -> (f32, Vector) {
        let vp = self.viewport;
        if let Some(z) = self.zoom {
            let t = (now.saturating_duration_since(z.start).as_secs_f32() * 1000.0 / ZOOM_MS).clamp(0.0, 1.0);
            let e = paint::out_cubic(t);
            let s = paint::lerp(z.s0, z.s1, e);
            let c = Point::new(paint::lerp(z.c0.x, z.c1.x, e), paint::lerp(z.c0.y, z.c1.y, e));
            let off = Vector::new(vp.width / 2.0 - c.x * s, vp.height / 2.0 - c.y * s);
            return (s, clamp_off(&l.img, s, off, vp));
        }
        l.transform(vp)
    }

    fn start_zoom(&mut self, target: f32, anchor: Option<Point>, to_fit: bool, now: Instant) {
        let vp = self.viewport;
        let Some(l) = self.live.clone() else { return };
        let (s0, off0) = self.transform_at(&l, now);
        if (s0 - target).abs() < 0.001 {
            if to_fit {
                self.zoom = None;
                if let Some(l) = self.live.as_mut() {
                    l.fit = true;
                }
            }
            return;
        }
        let center = Point::new(vp.width / 2.0, vp.height / 2.0);
        let c0 = Point::new((center.x - off0.x) / s0, (center.y - off0.y) / s0);
        let c1 = if to_fit {
            Point::new(l.img.width as f32 / 2.0, l.img.height as f32 / 2.0)
        } else {
            let a = anchor.unwrap_or(center);
            let ia = Point::new((a.x - off0.x) / s0, (a.y - off0.y) / s0);
            Point::new(ia.x - (a.x - center.x) / target, ia.y - (a.y - center.y) / target)
        };
        if let Some(l) = self.live.as_mut() {
            l.fit = false;
            l.scale = s0;
            l.off = off0;
        }
        self.zoom = Some(ZoomAnim { s0, s1: target, c0, c1, start: now, to_fit });
    }

    fn finish_zoom(&mut self, now: Instant) {
        if let (Some(z), Some(l)) = (self.zoom, self.live.clone()) {
            let t = now.saturating_duration_since(z.start).as_secs_f32() * 1000.0 / ZOOM_MS;
            if t >= 1.0 {
                let (s, off) = self.transform_at(&l, now);
                self.zoom = None;
                if let Some(l) = self.live.as_mut() {
                    let fit = fit_scale(&l.img, self.viewport);
                    if z.to_fit || (s - fit).abs() < 0.005 {
                        l.fit = true;
                    } else {
                        l.fit = false;
                        l.scale = s;
                        l.off = off;
                    }
                }
            }
        }
    }

    /// Scale about a viewport point (instant).
    fn scale_about(&mut self, factor: f32, anchor: Point, now: Instant) {
        let vp = self.viewport;
        self.zoom = None;
        let Some(l) = self.live.clone() else { return };
        let (s, off) = self.transform_at(&l, now);
        let ns = (s * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ia = Point::new((anchor.x - off.x) / s, (anchor.y - off.y) / s);
        let noff = Vector::new(anchor.x - ia.x * ns, anchor.y - ia.y * ns);
        if let Some(l) = self.live.as_mut() {
            let fit = fit_scale(&l.img, vp);
            l.fit = (ns - fit).abs() < 1e-4;
            l.scale = ns;
            l.off = clamp_off(&l.img, ns, noff, vp);
        }
    }

    fn fit_mode(&self) -> bool {
        self.zoom.is_none() && self.live.as_ref().is_some_and(|l| l.fit)
    }
}

fn slide_progress(s: &SlideAnim, now: Instant) -> f32 {
    let t = (now.saturating_duration_since(s.start).as_secs_f32() * 1000.0 / s.ms.max(1.0)).clamp(0.0, 1.0);
    paint::lerp(s.from, s.to, paint::out_cubic(t))
}

fn slide_done(s: &SlideAnim, now: Instant) -> bool {
    now.saturating_duration_since(s.start).as_secs_f32() * 1000.0 >= s.ms
}

impl<'a, Message> Widget<Message, iced::Theme, Renderer> for ImageStage<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            key: self.key,
            live: self.image.clone().map(Live::new),
            slide: None,
            pending_old: None,
            zoom: None,
            cmd_seq: self.cmd.0,
            viewport: Size::ZERO,
            pan: None,
            crop_drag: None,
            hover_region: Region::None,
            last_click: None,
            swipe: None,
            fingers: HashMap::new(),
            pinch: None,
            last_zoom_pub: -1,
            sb_drag: None,
            reported_vp: Size::ZERO,
            pending_dir: 1,
            sheet: self.sheet.clone(),
            sheet_zoom: None,
            ring_from: None,
            tile_hover: None,
            tile_hover_prev: None,
            level: LevelWheel::default(),
            label_drag: None,
            label_hover: None,
        })
    }

    fn diff(&self, tree: &mut widget::Tree) {
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        let shown = self.sheet.as_ref().is_some_and(|s| s.shown);
        let same_burst = self.sheet.as_ref().map(|s| s.key) == st.sheet.as_ref().map(|s| s.key);
        if !same_burst {
            st.tile_hover = None;
            st.tile_hover_prev = None;
            st.ring_from = None;
        } else if let (Some(new), Some(old)) = (&self.sheet, &st.sheet) {
            // The outline slides to the next tile.
            if new.shown && old.shown && new.outline != old.outline && self.key == st.key {
                let cells = st.sheet_cells();
                st.ring_from = st.ring_rect(&cells, old.outline, now).map(|r| (r, now));
            }
        }
        if self.key != st.key {
            st.key = self.key;
            st.zoom = None;
            st.pan = None;
            st.crop_drag = None;
            st.label_drag = None;
            st.label_hover = None;
            st.sheet_zoom = None;
            st.ring_from = None;
            match self.transition {
                // Onto a sheet: it needs no image, so it slides in at once.
                Transition::Slide(dir) if shown => {
                    let old = st.current_snap(now);
                    st.live = self.image.clone().map(Live::new);
                    st.pending_old = None;
                    st.slide = Some(SlideAnim { old, dir, from: 0.0, to: 1.0, start: now, ms: SLIDE_MS, restore: None });
                }
                Transition::Slide(dir) => {
                    let old = st.current_snap(now);
                    st.slide = None;
                    match &self.image {
                        Some(img) => {
                            st.live = Some(Live::new(img.clone()));
                            st.pending_old = None;
                            st.slide = Some(SlideAnim { old, dir, from: 0.0, to: 1.0, start: now, ms: SLIDE_MS, restore: None });
                        }
                        None => {
                            st.live = None;
                            if st.pending_old.is_none() {
                                st.pending_old = Some(old);
                            }
                            st.slide = None;
                            st.pending_dir = dir;
                        }
                    }
                }
                Transition::Keep => {
                    if let Some(img) = &self.image {
                        if !st.live.as_ref().is_some_and(|l| Arc::ptr_eq(&l.img, img)) {
                            st.live = Some(Live::new(img.clone()));
                        }
                    }
                    st.pending_old = None;
                }
                Transition::Cut => {
                    st.slide = None;
                    st.pending_old = None;
                    st.live = self.image.clone().map(Live::new);
                }
            }
        } else if shown {
            let was = st.sheet.as_ref().is_some_and(|s| s.shown);
            if !was && same_burst {
                // Back out to the sheet, from the frame as it is now.
                if let Some(sheet) = &self.sheet {
                    st.start_sheet_zoom(sheet, false, now);
                }
            }
            // On the sheet the image is the outlined frame's, ready for a
            // zoom into it.
            let same = match (&self.image, &st.live) {
                (Some(a), Some(l)) => Arc::ptr_eq(a, &l.img),
                (None, None) => true,
                _ => false,
            };
            if !same {
                st.live = self.image.clone().map(Live::new);
            }
        } else {
            if st.sheet.as_ref().is_some_and(|s| s.shown) && same_burst {
                // Into the outlined frame, which a click may have just moved
                // the outline to: its image, or none until it comes (the
                // tile's picture stands in).
                let same = match (&self.image, &st.live) {
                    (Some(a), Some(l)) => Arc::ptr_eq(a, &l.img),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    st.live = self.image.clone().map(Live::new);
                }
                if let Some(sheet) = &self.sheet {
                    st.start_sheet_zoom(sheet, true, now);
                }
            }
            match (&self.image, &st.live) {
                (Some(img), None) => {
                    st.live = Some(Live::new(img.clone()));
                    if let Some(old) = st.pending_old.take() {
                        let dir = st.pending_dir;
                        st.slide = Some(SlideAnim { old, dir, from: 0.0, to: 1.0, start: now, ms: SLIDE_MS, restore: None });
                    }
                }
                (Some(img), Some(l)) if !Arc::ptr_eq(img, &l.img) => {
                    // The full image replacing its preview: same frame, so
                    // keep the zoom and pan the user may already have set.
                    let same_frame = l.img.width == img.width && l.img.height == img.height;
                    let mut live = Live::new(img.clone());
                    if same_frame {
                        (live.fit, live.scale, live.off) = (l.fit, l.scale, l.off);
                    }
                    st.live = Some(live);
                }
                (None, Some(_)) if self.status.is_some() => {
                    st.live = None;
                    st.slide = None;
                    st.pending_old = None;
                }
                _ => {}
            }
        }
        st.sheet = self.sheet.clone();
        if self.cmd.0 != st.cmd_seq && st.sheet_visible() {
            // Commands are for a photo; the sheet has none.
            st.cmd_seq = self.cmd.0;
        }
        if self.cmd.0 != st.cmd_seq {
            st.cmd_seq = self.cmd.0;
            let vp = st.viewport;
            let center = Point::new(vp.width / 2.0, vp.height / 2.0);
            match self.cmd.1 {
                ViewCmd::Fit => {
                    if let Some(l) = st.live.clone() {
                        let f = fit_scale(&l.img, vp);
                        st.start_zoom(f, None, true, now);
                    }
                }
                ViewCmd::Actual | ViewCmd::Focus => st.start_zoom(1.0, None, false, now),
                ViewCmd::ZoomIn => st.scale_about(STEP, center, now),
                ViewCmd::ZoomOut => st.scale_about(1.0 / STEP, center, now),
                ViewCmd::None => {}
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _t: &mut widget::Tree, _r: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        crate::ui::modifiers::observe(event);
        let b = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        let vp = b.size();
        if st.viewport != vp {
            // Keep the centre steady across a resize when zoomed.
            if let Some(l) = st.live.as_mut() {
                if !l.fit && st.viewport.width > 0.0 {
                    let c = Point::new((st.viewport.width / 2.0 - l.off.x) / l.scale, (st.viewport.height / 2.0 - l.off.y) / l.scale);
                    l.off = clamp_off(&l.img, l.scale, Vector::new(vp.width / 2.0 - c.x * l.scale, vp.height / 2.0 - c.y * l.scale), vp);
                }
            }
            st.viewport = vp;
            // A zoom to or from the sheet lands where the new size puts it
            // (zooming into a pruned frame brings the viewer's banner).
            if let (Some(z), Some(sheet)) = (st.sheet_zoom, &self.sheet) {
                let area = Rectangle::with_size(vp);
                let landing = if z.to.1 > 0.5 {
                    burst_sheet::layout(&sheet.aspects(), area).get(z.tile).map(|c| c.photo)
                } else {
                    let aspect = match &st.live {
                        Some(l) => l.img.width.max(1) as f32 / l.img.height.max(1) as f32,
                        None => sheet.tiles.get(z.tile).map_or(burst_sheet::DEFAULT_ASPECT, |t| t.aspect),
                    };
                    Some(burst_sheet::fit_rect(aspect, area))
                };
                if let (Some(r), Some(z)) = (landing, st.sheet_zoom.as_mut()) {
                    z.to.0 = r;
                }
            }
        }
        st.finish_zoom(now);
        let local = |p: Point| Point::new(p.x - b.x, p.y - b.y);

        // The sheet, or a zoom to or from it, takes the pointer.
        if st.sheet_visible() && !matches!(event, Event::Window(_)) {
            if let Some(sheet) = &self.sheet {
                sheet_input(st, sheet, event, b, cursor, now, shell, &self.on);
            }
            return;
        }

        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                crate::profile::frame("image", *at);
                if let Some(s) = &st.slide {
                    if slide_done(s, now) {
                        let s = st.slide.take().expect("slide");
                        if let Some(restore) = s.restore {
                            st.live = Some(restore);
                        }
                    } else {
                        shell.request_redraw();
                    }
                }
                if st.zoom.is_some() {
                    shell.request_redraw();
                }
                if let Some(z) = st.sheet_zoom {
                    if z.t(now) >= 1.0 {
                        st.sheet_zoom = None;
                    } else {
                        shell.request_redraw();
                    }
                }
                if let Some((_, since)) = st.ring_from {
                    if now.saturating_duration_since(since).as_secs_f32() * 1000.0 >= t::MOTION_MS {
                        st.ring_from = None;
                    } else {
                        shell.request_redraw();
                    }
                }
                if let Some(sheet) = self.sheet.as_ref().filter(|_| st.sheet_visible()) {
                    let fading = sheet.tiles.iter().any(|tile| tile.thumb.as_ref().is_some_and(|th| burst_sheet::arrival(th.arrived, now) < 1.0));
                    let hovering = [st.tile_hover, st.tile_hover_prev]
                        .iter()
                        .flatten()
                        .any(|(_, since)| now.saturating_duration_since(*since).as_secs_f32() * 1000.0 < t::MOTION_FAST_MS);
                    if fading || hovering {
                        shell.request_redraw();
                    }
                    let wanted: Vec<usize> = sheet.tiles.iter().enumerate().filter(|(_, tile)| tile.wanted).map(|(i, _)| i).collect();
                    if !wanted.is_empty() {
                        shell.publish((self.on)(StageEvent::Sheet(SheetEvent::Thumbs(wanted))));
                    }
                }
                let pct = (st.scale(now) * 1000.0).round() as i32;
                if pct != st.last_zoom_pub && st.live.is_some() {
                    st.last_zoom_pub = pct;
                    shell.publish((self.on)(StageEvent::Zoom(st.scale(now))));
                }
                if st.reported_vp != vp {
                    st.reported_vp = vp;
                    shell.publish((self.on)(StageEvent::Viewport(vp)));
                }
            }

            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(p) = cursor.position_in(b) else { return };
                // A frame of a burst: zooming out at fit goes back to the
                // sheet (and a flick's tail after a change of level is
                // swallowed rather than zooming the photo).
                let burst = self.sheet.is_some();
                let out = match delta {
                    ScrollDelta::Lines { y, .. } => -y * 120.0,
                    ScrollDelta::Pixels { y, .. } => -y,
                };
                if burst {
                    let settling = st.level.settling(now);
                    let at_fit = st.live.is_none() || st.fit_mode();
                    if settling || (at_fit && out > 0.0) {
                        shell.capture_event();
                        if !settling && st.level.turn(out, now) {
                            shell.publish((self.on)(StageEvent::Sheet(SheetEvent::Back)));
                        }
                        return;
                    }
                }
                if st.live.is_none() {
                    return;
                }
                let ctrl = crate::ui::modifiers::get().control();
                if st.fit_mode() || ctrl || st.zoom.is_some() {
                    let factor = match delta {
                        ScrollDelta::Pixels { y, .. } if *y != 0.0 => 1.0 + y * 0.002,
                        ScrollDelta::Lines { y, .. } if *y != 0.0 => {
                            if *y > 0.0 { STEP } else { 1.0 / STEP }
                        }
                        _ => return,
                    };
                    // On the way out, a burst's frame stops at fit: one
                    // more turn goes to the sheet.
                    if let Some(l) = st.live.clone().filter(|_| burst && factor < 1.0) {
                        let (s, _) = st.transform_at(&l, now);
                        if s * factor <= fit_scale(&l.img, vp) * 1.0001 {
                            st.zoom = None;
                            if let Some(l) = st.live.as_mut() {
                                l.fit = true;
                            }
                            st.level.changed(now);
                            shell.capture_event();
                            shell.request_redraw();
                            return;
                        }
                    }
                    st.scale_about(factor, p, now);
                } else if let Some(l) = st.live.as_mut() {
                    let shift = crate::ui::modifiers::get().shift();
                    let (dx, dy) = match delta {
                        ScrollDelta::Lines { x, y } => (x * 60.0, y * 60.0),
                        ScrollDelta::Pixels { x, y } => (*x, *y),
                    };
                    let (dx, dy) = if shift { (dy, dx) } else { (dx, dy) };
                    l.off = clamp_off(&l.img, l.scale, Vector::new(l.off.x + dx, l.off.y + dy), vp);
                }
                shell.capture_event();
                shell.request_redraw();
            }

            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(abs) = cursor.position_in(b).and(cursor.position()) else { return };
                let p = local(abs);
                shell.capture_event();
                // Scrollbars
                if let Some(l) = st.live.clone() {
                    let (s, off) = st.transform_at(&l, now);
                    let (w, h) = (l.img.width as f32 * s, l.img.height as f32 * s);
                    if h > vp.height && p.x >= vp.width - SB {
                        let (y0, th) = sb_thumb(vp.height, h, -off.y);
                        st.sb_drag = Some((true, if p.y >= y0 && p.y <= y0 + th { p.y - y0 } else { th / 2.0 }));
                        return;
                    }
                    if w > vp.width && p.y >= vp.height - SB {
                        let (x0, tw) = sb_thumb(vp.width, w, -off.x);
                        st.sb_drag = Some((false, if p.x >= x0 && p.x <= x0 + tw { p.x - x0 } else { tw / 2.0 }));
                        return;
                    }
                }
                let click = amouse::Click::new(abs, mouse::Button::Left, st.last_click);
                st.last_click = Some(click);
                let double = click.kind() == amouse::click::Kind::Double;

                // Labels: a press picks a box or starts drawing one.
                if let (Some(_), Some(l)) = (&self.labels, st.live.clone()) {
                    let ip = norm_point(st, &l, p, now);
                    st.label_drag = Some((p, ip, ip));
                    return;
                }
                if let (Some(cp), Some(l)) = (&self.crop, st.live.clone()) {
                    if double {
                        shell.publish((self.on)(StageEvent::Crop(crop_math::reset(&cp.crop, cp.ratio))));
                        return;
                    }
                    let region = crop_region(st, &l, &cp.crop, p, now);
                    let ip = norm_point(st, &l, p, now);
                    let (region, start) = if region == Region::None {
                        let (x, y) = (ip.0.clamp(0.0, 1.0), ip.1.clamp(0.0, 1.0));
                        let c = Crop::new(x, y, x + crate::model::crop::MIN_EDGE, y + crate::model::crop::MIN_EDGE, cp.crop.aspect_label.clone());
                        shell.publish((self.on)(StageEvent::Crop(c.clone())));
                        (Region::BR, c)
                    } else {
                        (region, cp.crop.clone())
                    };
                    st.crop_drag = Some((region, ip, start));
                    return;
                }
                if double {
                    if st.fit_mode() {
                        st.start_zoom(1.0, Some(p), false, now);
                    } else if let Some(l) = st.live.clone() {
                        let f = fit_scale(&l.img, vp);
                        st.start_zoom(f, None, true, now);
                    }
                    shell.request_redraw();
                    return;
                }
                if let Some(l) = st.live.clone() {
                    let (s, off) = st.transform_at(&l, now);
                    if l.img.width as f32 * s > vp.width + 0.5 || l.img.height as f32 * s > vp.height + 0.5 {
                        st.zoom = None;
                        if let Some(lm) = st.live.as_mut() {
                            lm.fit = false;
                            lm.scale = s;
                            lm.off = off;
                        }
                        st.pan = Some((p, off));
                    }
                }
            }

            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(abs) = cursor.position() else { return };
                let p = local(abs);
                if let Some((vertical, grab)) = st.sb_drag {
                    if let Some(l) = st.live.as_mut() {
                        let (w, h) = (l.img.width as f32 * l.scale, l.img.height as f32 * l.scale);
                        if vertical {
                            let (_, th) = sb_thumb(vp.height, h, -l.off.y);
                            let frac = ((p.y - grab) / (vp.height - th).max(1.0)).clamp(0.0, 1.0);
                            l.off.y = -(frac * (h - vp.height));
                        } else {
                            let (_, tw) = sb_thumb(vp.width, w, -l.off.x);
                            let frac = ((p.x - grab) / (vp.width - tw).max(1.0)).clamp(0.0, 1.0);
                            l.off.x = -(frac * (w - vp.width));
                        }
                        l.fit = false;
                    }
                    shell.request_redraw();
                    return;
                }
                if let Some((region, start_pt, start)) = st.crop_drag.clone() {
                    if let (Some(cp), Some(l)) = (&self.crop, st.live.clone()) {
                        let now_pt = norm_point(st, &l, p, now);
                        let c = if region == Region::Inside {
                            crop_math::translate(&start, now_pt.0 - start_pt.0, now_pt.1 - start_pt.1)
                        } else {
                            crop_math::resize(&start, region, now_pt.0, now_pt.1, cp.ratio)
                        };
                        shell.publish((self.on)(StageEvent::Crop(c)));
                    }
                    return;
                }
                if let Some((p0, ip0, _)) = st.label_drag {
                    if let Some(l) = st.live.clone() {
                        let ip = norm_point(st, &l, p, now);
                        st.label_drag = Some((p0, ip0, ip));
                        shell.request_redraw();
                    }
                    return;
                }
                if let (Some(lp), Some(l)) = (&self.labels, st.live.clone()) {
                    let (s, off) = st.transform_at(&l, now);
                    let over = label_hit(&l, s, off, lp, p, vp.width);
                    if over != st.label_hover {
                        st.label_hover = over;
                        shell.request_redraw();
                    }
                }
                if let Some((origin, off0)) = st.pan {
                    if let Some(l) = st.live.as_mut() {
                        l.off = clamp_off(&l.img, l.scale, off0 + (p - origin), vp);
                    }
                    shell.request_redraw();
                    return;
                }
                if let (Some(cp), Some(l)) = (&self.crop, st.live.clone()) {
                    st.hover_region = crop_region(st, &l, &cp.crop, p, now);
                }
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.pan = None;
                st.crop_drag = None;
                st.sb_drag = None;
                if let (Some((p0, a, z)), Some(lp), Some(l)) = (st.label_drag.take(), &self.labels, st.live.clone()) {
                    let p = cursor.position().map(local).unwrap_or(p0);
                    if (p.x - p0.x).abs() < LABEL_CLICK && (p.y - p0.y).abs() < LABEL_CLICK {
                        let (s, off) = st.transform_at(&l, now);
                        shell.publish((self.on)(StageEvent::LabelPick(label_hit(&l, s, off, lp, p0, vp.width))));
                    } else if let Some(b) = drawn_box(a, z) {
                        shell.publish((self.on)(StageEvent::LabelDraw(b)));
                    }
                    shell.request_redraw();
                }
            }

            Event::Touch(te) => {
                handle_touch(st, te, b, &self.neighbors, now, shell, &self.on);
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let Some(p) = cursor.position_in(b) else {
            return mouse::Interaction::None;
        };
        if st.sheet_visible() {
            let over = st.sheet_zoom.is_none() && burst_sheet::hit(&st.sheet_cells(), p).is_some();
            return if over { mouse::Interaction::Pointer } else { mouse::Interaction::Idle };
        }
        if self.labels.is_some() && st.live.is_some() {
            return if st.label_drag.is_none() && st.label_hover.is_some() { mouse::Interaction::Pointer } else { mouse::Interaction::Crosshair };
        }
        if self.crop.is_some() {
            let region = st.crop_drag.as_ref().map(|d| d.0).unwrap_or(st.hover_region);
            return match region {
                Region::Inside => mouse::Interaction::Move,
                Region::Top | Region::Bottom => mouse::Interaction::ResizingVertically,
                Region::Left | Region::Right => mouse::Interaction::ResizingHorizontally,
                Region::TL | Region::BR => mouse::Interaction::ResizingDiagonallyDown,
                Region::TR | Region::BL => mouse::Interaction::ResizingDiagonallyUp,
                Region::None => mouse::Interaction::Crosshair,
            };
        }
        if st.pan.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some(l) = &st.live {
            let (s, _) = st.transform_at(l, Instant::now());
            if l.img.width as f32 * s > b.width + 0.5 || l.img.height as f32 * s > b.height + 0.5 {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::Idle
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        r: &mut Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let _t = crate::profile::span("draw", "image");
        crate::profile::since("viewer", "open to first frame");
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let now = Instant::now();
        let sf = self.scale_factor;
        let sheet_up = st.sheet_visible() && st.slide.is_none() && st.pending_old.is_none();
        r.with_layer(b, |r| {
            paint::fill(r, b, t::PHOTO_GROUND);
            if let Some(s) = &st.slide {
                let p = slide_progress(s, now);
                let w = b.width;
                draw_snap(r, b, &s.old, Vector::new(-w * p * s.dir as f32, 0.0), sf);
                draw_snap(r, b, &st.live_snap(now), Vector::new(w * (1.0 - p) * s.dir as f32, 0.0), sf);
            } else if let Some(old) = &st.pending_old {
                draw_snap(r, b, old, Vector::ZERO, sf);
            } else if let (Some(z), Some(sheet)) = (st.sheet_zoom, &self.sheet) {
                draw_sheet_zoom(r, b, st, sheet, z, sf, now);
            } else if let Some(sheet) = self.sheet.as_ref().filter(|s| s.shown) {
                let cells = burst_sheet::layout(&sheet.aspects(), b);
                let ring = st.ring_rect(&st.sheet_cells(), sheet.outline, now).map(|r| shifted(r, b));
                let hover = |i: usize| st.tile_hover_amount(i, now);
                let look = burst_sheet::Look { alpha: 1.0, zoom: None, hover: &hover, ring, skip: None, scale_factor: sf, now };
                burst_sheet::paint(r, b, sheet, &cells, &look);
            } else if let Some(l) = &st.live {
                let (s, off) = st.transform_at(l, now);
                draw_image(r, b, &l.img, s, off, sf);
            } else if let Some(tile) = self.sheet.as_ref().and_then(|s| s.tiles.get(s.outline)) {
                // A frame still loading shows its tile's picture until the
                // photo comes.
                burst_sheet::draw_tile_photo(r, b, tile, burst_sheet::fit_rect(tile.aspect, b), 1.0, sf, now);
            }
        });
        // Everything that sits over the picture needs its own layer: images
        // draw above every quad in theirs.
        r.with_layer(b, |r| {
            if sheet_up {
                return;
            }
            if let Some(l) = st.live.as_ref().filter(|_| st.slide.is_none() && st.pending_old.is_none()) {
                let (s, off) = st.transform_at(l, now);
                // Scrollbars when the image overflows.
                let (w, h) = (l.img.width as f32 * s, l.img.height as f32 * s);
                if h > b.height + 0.5 {
                    let (y, th) = sb_thumb(b.height, h, -off.y);
                    let col = if st.sb_drag.is_some_and(|d| d.0) { t::PHOTO_INK2 } else { t::PHOTO_INK4 };
                    paint::rounded(r, rect(b.x + b.width - SB, b.y + y, SB, th), 3.0, col);
                }
                if w > b.width + 0.5 {
                    let (x, tw) = sb_thumb(b.width, w, -off.x);
                    let col = if st.sb_drag.is_some_and(|d| !d.0) { t::PHOTO_INK2 } else { t::PHOTO_INK4 };
                    paint::rounded(r, rect(b.x + x, b.y + b.height - SB, tw, SB), 3.0, col);
                }
                if let Some(cp) = &self.crop {
                    draw_crop(r, b, l, s, off, &cp.crop);
                }
                if let Some(lp) = &self.labels {
                    let _t = crate::profile::span("draw", "labels");
                    let drag = st.label_drag.and_then(|(p0, a, z)| drawn_box(a, z).filter(|_| st.live.is_some()).map(|bx| (bx, p0)));
                    draw_labels(r, b, l, s, off, lp, st.label_hover, drag.map(|d| d.0));
                }
            }
            if let Some(status) = &self.status {
                if st.slide.is_none() {
                    if st.pending_old.is_some() {
                        paint::fill(r, b, t::alpha(t::PHOTO_GROUND, 0.45));
                    }
                    let lines: Vec<&str> = status.lines().collect();
                    let lh = 26.0;
                    let top = b.center_y() - lh * lines.len() as f32 / 2.0;
                    for (i, line) in lines.iter().enumerate() {
                        paint::text(r, line, rect(b.x, top + i as f32 * lh, b.width, lh), t::T_TITLE, paint::REGULAR, t::PHOTO_INK3, HAlign::Center, b);
                    }
                }
            }
        });
    }
}

fn sb_thumb(view: f32, content: f32, pos: f32) -> (f32, f32) {
    let len = (view / content * view).max(24.0);
    let max = (content - view).max(1.0);
    ((pos / max).clamp(0.0, 1.0) * (view - len), len)
}

/// `r` (stage coordinates) on screen, in `b`.
fn shifted(r: Rectangle, b: Rectangle) -> Rectangle {
    rect(r.x + b.x, r.y + b.y, r.width, r.height)
}

/// The zoom between a tile and the whole frame: the sheet carried along
/// and fading, the frame's photo over it, its marks fading with the sheet.
fn draw_sheet_zoom(r: &mut Renderer, b: Rectangle, st: &State, sheet: &Sheet, z: SheetZoom, sf: f32, now: Instant) {
    let cells = burst_sheet::layout(&sheet.aspects(), b);
    let (Some(cell), Some(tile)) = (cells.get(z.tile), sheet.tiles.get(z.tile)) else { return };
    let (photo, alpha) = z.at(now);
    let photo = shifted(photo, b);
    let ring = cells.get(sheet.outline).map(|c| c.mount);
    let none = |_: usize| 0.0;
    let look = burst_sheet::Look { alpha, zoom: Some((cell.photo, photo)), hover: &none, ring, skip: Some(z.tile), scale_factor: sf, now };
    burst_sheet::paint(r, b, sheet, &cells, &look);
    r.with_layer(b, |r| match &st.live {
        Some(l) => {
            let scale = photo.width / l.img.width.max(1) as f32;
            draw_image(r, b, &l.img, scale, Vector::new(photo.x - b.x, photo.y - b.y), sf);
        }
        None => burst_sheet::draw_tile_photo(r, b, tile, photo, 1.0, sf, now),
    });
    if alpha > 0.002 {
        r.with_layer(b, |r| burst_sheet::paint_tile_marks(r, b, tile, photo, sheet.picking, alpha));
    }
}

/// Pointer input while the sheet is up: hover, click to open a frame,
/// Ctrl+click to pick one, the wheel to zoom into the frame under it.
/// Nothing reacts while a zoom runs (a double-click's second click lands
/// on the zoom).
#[allow(clippy::too_many_arguments)]
fn sheet_input<Message>(
    st: &mut State,
    sheet: &Sheet,
    event: &Event,
    b: Rectangle,
    cursor: Cursor,
    now: Instant,
    shell: &mut Shell<'_, Message>,
    on: &dyn Fn(StageEvent) -> Message,
) {
    let zooming = st.sheet_zoom.is_some();
    let over = |st: &State| if zooming { None } else { cursor.position_in(b).and_then(|p| burst_sheet::hit(&st.sheet_cells(), p)) };
    match event {
        Event::Mouse(mouse::Event::CursorMoved { .. }) => {
            let hit = over(st);
            if hit != st.tile_hover.map(|h| h.0) {
                st.set_tile_hover(hit, now);
                shell.request_redraw();
            }
        }
        Event::Mouse(mouse::Event::CursorLeft) => {
            if st.tile_hover.is_some() {
                st.set_tile_hover(None, now);
                shell.request_redraw();
            }
        }
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
            if cursor.position_in(b).is_none() {
                return;
            }
            shell.capture_event();
            if let Some(i) = over(st) {
                let pick = crate::ui::modifiers::get().control();
                shell.publish(on(StageEvent::Sheet(if pick { SheetEvent::Pick(i) } else { SheetEvent::Open(i) })));
            }
        }
        Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
            if cursor.position_in(b).is_none() {
                return;
            }
            shell.capture_event();
            let up = match delta {
                ScrollDelta::Lines { y, .. } => y * 120.0,
                ScrollDelta::Pixels { y, .. } => *y,
            };
            if zooming {
                return;
            }
            if st.level.turn(up, now) {
                let i = over(st).unwrap_or(sheet.outline);
                shell.publish(on(StageEvent::Sheet(SheetEvent::Open(i))));
            }
        }
        Event::Touch(_) => shell.capture_event(),
        _ => {}
    }
}

fn draw_snap(r: &mut Renderer, b: Rectangle, snap: &Snap, shift: Vector, sf: f32) {
    match snap {
        Snap::Blank => {}
        Snap::Image { img, scale, off } => draw_image(r, b, img, *scale, *off + shift, sf),
        Snap::Sheet(sheet) => {
            let area = rect(b.x + shift.x, b.y + shift.y, b.width, b.height);
            let cells = burst_sheet::layout(&sheet.aspects(), area);
            let ring = cells.get(sheet.outline).map(|c| c.mount);
            let none = |_: usize| 0.0;
            let look = burst_sheet::Look { alpha: 1.0, zoom: None, hover: &none, ring, skip: None, scale_factor: sf, now: Instant::now() };
            burst_sheet::paint(r, b, sheet, &cells, &look);
        }
        Snap::Slide { old, new, dir, p } => {
            let w = b.width;
            draw_snap(r, b, old, shift + Vector::new(-w * p * *dir as f32, 0.0), sf);
            draw_snap(r, b, new, shift + Vector::new(w * (1.0 - p) * *dir as f32, 0.0), sf);
        }
    }
}

fn draw_image(r: &mut Renderer, b: Rectangle, img: &FullImage, scale: f32, off: Vector, sf: f32) {
    let dst = rect(b.x + off.x, b.y + off.y, img.width as f32 * scale, img.height as f32 * scale);
    if dst.intersection(&b).is_none() {
        return;
    }
    let i = img.level_for_scale(scale * sf);
    let coarse = (i + 1).min(img.levels.len() - 1);
    for idx in [coarse, i] {
        let l = &img.levels[idx];
        r.draw_image(iced::advanced::image::Image::new(l.handle.clone()).filter_method(FilterMethod::Linear), dst, b);
        if coarse == i {
            break;
        }
    }
}

fn norm_point(st: &State, l: &Live, p: Point, now: Instant) -> (f64, f64) {
    let (s, off) = st.transform_at(l, now);
    let ix = (p.x - off.x) / s;
    let iy = (p.y - off.y) / s;
    (ix as f64 / l.img.width.max(1) as f64, iy as f64 / l.img.height.max(1) as f64)
}

fn crop_widget_rect(l: &Live, s: f32, off: Vector, c: &Crop) -> Rectangle {
    let (w, h) = (l.img.width as f32 * s, l.img.height as f32 * s);
    let x0 = off.x + c.left as f32 * w;
    let y0 = off.y + c.top as f32 * h;
    let x1 = off.x + c.right as f32 * w;
    let y1 = off.y + c.bottom as f32 * h;
    rect(x0.round(), y0.round(), (x1 - x0).round(), (y1 - y0).round())
}

fn crop_region(st: &State, l: &Live, c: &Crop, p: Point, now: Instant) -> Region {
    let (s, off) = st.transform_at(l, now);
    let r = crop_widget_rect(l, s, off, c);
    let (x, y) = (p.x, p.y);
    let (left, right, top, bottom) = (r.x, r.x + r.width, r.y, r.y + r.height);
    let near_l = (x - left).abs() <= GRAB;
    let near_r = (x - right).abs() <= GRAB;
    let near_t = (y - top).abs() <= GRAB;
    let near_b = (y - bottom).abs() <= GRAB;
    let in_x = x >= left - GRAB && x <= right + GRAB;
    let in_y = y >= top - GRAB && y <= bottom + GRAB;
    if near_l && near_t {
        Region::TL
    } else if near_r && near_t {
        Region::TR
    } else if near_l && near_b {
        Region::BL
    } else if near_r && near_b {
        Region::BR
    } else if near_t && in_x {
        Region::Top
    } else if near_b && in_x {
        Region::Bottom
    } else if near_l && in_y {
        Region::Left
    } else if near_r && in_y {
        Region::Right
    } else if r.contains(p) {
        Region::Inside
    } else {
        Region::None
    }
}

fn draw_crop(r: &mut Renderer, b: Rectangle, l: &Live, s: f32, off: Vector, c: &Crop) {
    let mut cr = crop_widget_rect(l, s, off, c);
    cr.x += b.x;
    cr.y += b.y;
    let dim = t::alpha(t::PHOTO_GROUND, 0.72);
    paint::fill(r, rect(b.x, b.y, b.width, cr.y - b.y), dim);
    paint::fill(r, rect(b.x, cr.y + cr.height, b.width, b.y + b.height - (cr.y + cr.height)), dim);
    paint::fill(r, rect(b.x, cr.y, cr.x - b.x, cr.height), dim);
    paint::fill(r, rect(cr.x + cr.width, cr.y, b.x + b.width - (cr.x + cr.width), cr.height), dim);
    let grid = t::alpha(Color::WHITE, 0.22);
    for i in 1..3 {
        let x = (cr.x + cr.width * i as f32 / 3.0).floor();
        let y = (cr.y + cr.height * i as f32 / 3.0).floor();
        paint::vline(r, x, cr.y, cr.y + cr.height, 1.0, grid);
        paint::hline(r, cr.x, cr.x + cr.width, y, 1.0, grid);
    }
    paint::framed(r, cr, 0.0, Color::TRANSPARENT, t::alpha(Color::WHITE, 0.8), 1.0);
    let orange = t::SIGNAL;
    let th = 3.0;
    let arm = 22f32.min(cr.width / 3.0).min(cr.height / 3.0).floor();
    let (x0, y0, x1, y1) = (cr.x, cr.y, cr.x + cr.width, cr.y + cr.height);
    // Corner brackets, inside the rect.
    for (cx, cy, sx, sy) in [(x0, y0, 1.0, 1.0), (x1, y0, -1.0, 1.0), (x0, y1, 1.0, -1.0), (x1, y1, -1.0, -1.0)] {
        let hx = if sx > 0.0 { cx } else { cx - arm };
        let hy = if sy > 0.0 { cy } else { cy - th };
        paint::fill(r, rect(hx, hy, arm, th), orange);
        let vx = if sx > 0.0 { cx } else { cx - th };
        let vy = if sy > 0.0 { cy } else { cy - arm };
        paint::fill(r, rect(vx, vy, th, arm), orange);
    }
    // Edge grips
    let grip = 22f32.min(cr.width / 4.0).min(cr.height / 4.0).floor();
    let (mx, my) = (cr.center_x(), cr.center_y());
    paint::fill(r, rect(mx - grip / 2.0, y0, grip, th), orange);
    paint::fill(r, rect(mx - grip / 2.0, y1 - th, grip, th), orange);
    paint::fill(r, rect(x0, my - grip / 2.0, th, grip), orange);
    paint::fill(r, rect(x1 - th, my - grip / 2.0, th, grip), orange);
}

// ── the viewer's labels ─────────────────────────────────────────────────────

/// A press that moves less than this (pixels) is a click, not a drawing.
const LABEL_CLICK: f32 = 5.0;
/// A label's tag: its height and side padding.
const TAG_H: f32 = 18.0;
const TAG_PAD: f32 = 6.0;

/// A box (fractions of the image) on the stage, local coordinates.
fn label_rect(l: &Live, s: f32, off: Vector, b: &[f32; 4]) -> Rectangle {
    let (w, h) = (l.img.width as f32 * s, l.img.height as f32 * s);
    rect(off.x + b[0] * w, off.y + b[1] * h, b[2] * w, b[3] * h)
}

/// Where a box's tag sits: above its top-left corner, or just inside it
/// when the box touches the top of the stage; pulled left when it would
/// run off the stage's right edge (`top`, `right`: the stage's).
fn tag_rect(bx: Rectangle, tag: &str, top: f32, right: f32) -> Rectangle {
    let w = (paint::text_width(tag, t::T_SMALL, paint::SEMIBOLD) + 2.0 * TAG_PAD).ceil();
    let y = if bx.y - TAG_H >= top { bx.y - TAG_H } else { bx.y };
    rect(bx.x.min(right - w), y, w, TAG_H)
}

/// The box under `p` (local): the smallest whose outline or tag holds it.
fn label_hit(l: &Live, s: f32, off: Vector, lp: &LabelProps, p: Point, width: f32) -> Option<usize> {
    lp.boxes
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let r = label_rect(l, s, off, &b.bbox);
            let grown = rect(r.x - 3.0, r.y - 3.0, r.width + 6.0, r.height + 6.0);
            (grown.contains(p) || tag_rect(r, &b.tag, 0.0, width).contains(p)).then_some((i, r.width * r.height))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// A drawn box from two corners (image fractions), clamped to the image;
/// none when it is too small to be a face.
fn drawn_box(a: (f64, f64), z: (f64, f64)) -> Option<[f32; 4]> {
    let (x0, x1) = (a.0.min(z.0).clamp(0.0, 1.0), a.0.max(z.0).clamp(0.0, 1.0));
    let (y0, y1) = (a.1.min(z.1).clamp(0.0, 1.0), a.1.max(z.1).clamp(0.0, 1.0));
    (x1 - x0 >= 0.005 && y1 - y0 >= 0.005).then(|| [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32])
}

/// A rectangle's outline in dashes (a box drawn and not matched yet, or
/// one being drawn).
fn dashed(r: &mut Renderer, bx: Rectangle, width: f32, color: Color) {
    const DASH: f32 = 7.0;
    const GAP: f32 = 5.0;
    let mut segs = Vec::new();
    let mut side = |a: Point, b: Point| {
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        let mut d = 0.0;
        while d < len {
            let e = (d + DASH).min(len);
            let at = |t: f32| Point::new(a.x + (b.x - a.x) * t / len, a.y + (b.y - a.y) * t / len);
            segs.push((at(d), at(e)));
            d += DASH + GAP;
        }
    };
    let (x0, y0, x1, y1) = (bx.x, bx.y, bx.x + bx.width, bx.y + bx.height);
    side(Point::new(x0, y0), Point::new(x1, y0));
    side(Point::new(x1, y0), Point::new(x1, y1));
    side(Point::new(x1, y1), Point::new(x0, y1));
    side(Point::new(x0, y1), Point::new(x0, y0));
    paint::strokes(r, &segs, width, color, false);
}

/// The labels over the photo: each box a hollow outline in its kind's
/// colour with a tag hanging off its top edge (solid when named, hollow
/// when not), a dark keyline under both so they read on any photo; the
/// selected box heavier and the rest quieter; the box being drawn dashed.
#[allow(clippy::too_many_arguments)]
fn draw_labels(r: &mut Renderer, b: Rectangle, l: &Live, s: f32, off: Vector, lp: &LabelProps, hover: Option<usize>, drawing: Option<[f32; 4]>) {
    let keyline = t::alpha(t::PHOTO_GROUND, 0.55);
    let order: Vec<usize> = (0..lp.boxes.len()).filter(|&i| Some(i) != lp.selected).chain(lp.selected).collect();
    for i in order {
        let bx_def = &lp.boxes[i];
        let local = label_rect(l, s, off, &bx_def.bbox);
        let bx = rect(local.x + b.x, local.y + b.y, local.width, local.height);
        let selected = lp.selected == Some(i);
        let quiet = lp.selected.is_some() && !selected && hover != Some(i);
        let color = if quiet { t::alpha(bx_def.color, 0.55) } else { bx_def.color };
        let width = if selected { 3.0 } else { 2.0 };
        // The keyline: a dark hairline just outside the outline.
        paint::framed(r, rect(bx.x - 1.0, bx.y - 1.0, bx.width + 2.0, bx.height + 2.0), 0.0, Color::TRANSPARENT, keyline, 1.0);
        if bx_def.dashed {
            dashed(r, bx, width, color);
        } else {
            paint::framed(r, bx, 0.0, Color::TRANSPARENT, color, width);
        }
        let tag = tag_rect(bx, &bx_def.tag, b.y, b.x + b.width);
        if bx_def.named {
            paint::rounded(r, tag, 2.0, color);
            paint::text(r, &bx_def.tag, rect(tag.x + TAG_PAD, tag.y, tag.width, tag.height), t::T_SMALL, paint::SEMIBOLD, t::alpha(t::ON_SIGNAL, if quiet { 0.7 } else { 1.0 }), HAlign::Left, b);
        } else {
            paint::framed(r, tag, 2.0, t::alpha(t::PHOTO_GROUND, 0.82), color, 1.0);
            paint::text(r, &bx_def.tag, rect(tag.x + TAG_PAD, tag.y, tag.width, tag.height), t::T_SMALL, paint::SEMIBOLD, color, HAlign::Left, b);
        }
    }
    let draft = drawing.map(|d| (d, lp.draft.map_or(t::PHOTO_INK, |(_, c)| c))).or(lp.draft);
    if let Some((d, color)) = draft {
        let local = label_rect(l, s, off, &d);
        let bx = rect(local.x + b.x, local.y + b.y, local.width, local.height);
        paint::framed(r, rect(bx.x - 1.0, bx.y - 1.0, bx.width + 2.0, bx.height + 2.0), 0.0, Color::TRANSPARENT, keyline, 1.0);
        dashed(r, bx, 2.0, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_touch<Message>(
    st: &mut State,
    te: &touch::Event,
    b: Rectangle,
    neighbors: &[Option<Arc<FullImage>>; 2],
    now: Instant,
    shell: &mut Shell<'_, Message>,
    on: &dyn Fn(StageEvent) -> Message,
) {
    let vp = b.size();
    match te {
        touch::Event::FingerPressed { id, position } => {
            if !b.contains(*position) {
                return;
            }
            st.fingers.insert(*id, *position);
            if st.fingers.len() == 2 {
                st.swipe = None;
                let pts: Vec<Point> = st.fingers.values().copied().collect();
                let c = Point::new((pts[0].x + pts[1].x) / 2.0 - b.x, (pts[0].y + pts[1].y) / 2.0 - b.y);
                st.pinch = Some((pts[0].distance(pts[1]).max(1.0), st.scale(now), c));
            } else if st.fit_mode() && st.slide.is_none() {
                st.swipe = Some(Swipe { origin: *position, t0: now, dx: 0.0, active: false, dir: 0, old: None });
            }
            shell.capture_event();
        }
        touch::Event::FingerMoved { id, position } => {
            st.fingers.insert(*id, *position);
            if let Some((d0, s0, c)) = st.pinch {
                let pts: Vec<Point> = st.fingers.values().copied().collect();
                if pts.len() >= 2 {
                    let target = (s0 * pts[0].distance(pts[1]) / d0).clamp(MIN_SCALE, MAX_SCALE);
                    let cur = st.scale(now);
                    st.scale_about(target / cur, c, now);
                    shell.request_redraw();
                }
            } else if let Some(sw) = st.swipe.as_mut() {
                let dx = position.x - sw.origin.x;
                let dy = position.y - sw.origin.y;
                if !sw.active && dx.abs() > 8.0 && dx.abs() > dy.abs() * 1.5 {
                    let dir = if dx < 0.0 { 1 } else { -1 };
                    let target = neighbors[if dir > 0 { 1 } else { 0 }].clone();
                    {
                        sw.active = true;
                        sw.dir = dir;
                        sw.old = st.live.clone();
                        let old = st.live.as_ref().map(|l| l.snap(vp)).unwrap_or(Snap::Blank);
                        st.live = target.map(Live::new);
                        st.slide = Some(SlideAnim { old, dir, from: 0.0, to: 0.0, start: now, ms: 1.0, restore: None });
                    }
                }
                if let Some(sw) = st.swipe.as_mut() {
                    if sw.active {
                        sw.dx = dx;
                        let p = (dx.abs() / vp.width.max(1.0)).min(1.0);
                        if let Some(s) = st.slide.as_mut() {
                            s.from = p;
                            s.to = p;
                        }
                        shell.request_redraw();
                    }
                }
            }
            shell.capture_event();
        }
        touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. } => {
            st.fingers.remove(id);
            if st.fingers.len() < 2 {
                st.pinch = None;
            }
            if let Some(sw) = st.swipe.take() {
                if sw.active {
                    let dt = now.saturating_duration_since(sw.t0).as_secs_f32().max(0.001);
                    let velocity = sw.dx.abs() / dt;
                    let p = (sw.dx.abs() / vp.width.max(1.0)).min(1.0);
                    let has_target = st.live.is_some();
                    let committed = has_target && (sw.dx.abs() > vp.width * 0.35 || velocity > 500.0);
                    if let Some(s) = st.slide.as_mut() {
                        s.start = now;
                        s.from = p;
                        if committed {
                            s.to = 1.0;
                            s.ms = (220.0 * (1.0 - p)).max(60.0);
                        } else {
                            s.to = 0.0;
                            s.ms = (280.0 * p).max(80.0);
                            s.restore = sw.old.clone();
                        }
                    }
                    if committed {
                        shell.publish(on(StageEvent::Swipe(sw.dir)));
                    }
                    shell.request_redraw();
                }
            }
            shell.capture_event();
        }
    }
}

impl<'a, Message: 'a> From<ImageStage<'a, Message>> for Element<'a, Message, iced::Theme, Renderer> {
    fn from(s: ImageStage<'a, Message>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_wheel_notch_changes_level_and_a_flicks_tail_is_swallowed() {
        let t0 = Instant::now();
        let mut w = LevelWheel::default();
        // One mouse-wheel notch (120 px) is enough.
        assert!(w.turn(120.0, t0));
        // The rest of a flick right after it does nothing...
        assert!(w.settling(at(t0, 100)));
        assert!(!w.turn(120.0, at(t0, 100)));
        assert!(!w.turn(120.0, at(t0, 300)));
        // ...until it has settled.
        assert!(!w.settling(at(t0, 500)));
        assert!(w.turn(120.0, at(t0, 500)));
    }

    #[test]
    fn a_trackpad_needs_some_travel_and_a_pause_starts_again() {
        let t0 = Instant::now();
        let mut w = LevelWheel::default();
        // Small steps add up.
        assert!(!w.turn(30.0, t0));
        assert!(!w.turn(30.0, at(t0, 16)));
        assert!(!w.turn(30.0, at(t0, 32)));
        assert!(w.turn(30.0, at(t0, 48)));
        // A pause loses what had built up.
        let t1 = at(t0, 2000);
        assert!(!w.turn(60.0, t1));
        assert!(!w.turn(60.0, at(t1, WHEEL_IDLE_MS as u64 + 50)));
        // Turning the other way does too.
        let t2 = at(t0, 5000);
        assert!(!w.turn(80.0, t2));
        assert!(!w.turn(-10.0, at(t2, 10)));
        assert!(!w.turn(80.0, at(t2, 20)));
    }

    #[test]
    fn a_zoom_by_key_also_settles_the_wheel() {
        let t0 = Instant::now();
        let mut w = LevelWheel::default();
        w.changed(t0);
        assert!(!w.turn(240.0, at(t0, 50)));
    }
}
