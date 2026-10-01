//! The viewer window's size, position and maximised state, kept across
//! opens and restarts (settings.json), and its full-screen mode (F11).
//!
//! Wayland doesn't tell an app where its windows are, nor let it place
//! them, so there only the size and the maximised state carry over and the
//! compositor places the window.  On X11 the position comes back too.

use std::time::{Duration, Instant};

use iced::window::{self, Mode};
use iced::{Point, Size, Task, Vector};

use super::viewer::Msg;
use super::{App, Message};
use crate::settings::Settings;

pub const MIN_SIZE: Size = Size::new(560.0, 380.0);

#[derive(Debug, Clone)]
pub enum WinMsg {
    /// The window changed size.  Its mode and maximised state are asked
    /// for afterwards, so a maximised or full-screen size isn't kept as the
    /// size to come back to.
    Resized { size: Size, mode: Mode, maximized: bool },
    /// The window moved (X11 only).
    Moved { at: Point, mode: Mode, maximized: bool },
}

/// One viewer window's state.
#[derive(Debug)]
pub struct WinState {
    /// Full screen (F11).
    pub fullscreen: bool,
    /// The window's width (narrow windows get a more compact top bar).
    pub width: f32,
    placement: Placement,
}

impl WinState {
    pub fn new(settings: &window::Settings) -> Self {
        let asked = match settings.position {
            window::Position::Specific(p) => Some(p),
            _ => None,
        };
        Self { fullscreen: false, width: settings.size.width, placement: Placement::new(asked, Instant::now()) }
    }
}

/// Turns reported window positions into ones that put the window back in
/// the same place.  Where the window manager doesn't publish its frame
/// sizes, X11 windows are placed by their frame but report their client
/// area, so each reopen would creep down by a title bar.  The first report
/// just after opening at a known spot shows that offset.
#[derive(Debug)]
struct Placement {
    asked: Option<Point>,
    since: Instant,
    offset: Option<Vector>,
}

/// How long after opening a report still counts as the initial placement.
const SETTLE: Duration = Duration::from_millis(1500);
/// A larger difference is the window manager moving the window, not a frame.
const MAX_FRAME: f32 = 64.0;

impl Placement {
    fn new(asked: Option<Point>, now: Instant) -> Self {
        Self { asked, since: now, offset: None }
    }

    fn to_store(&mut self, reported: Point, now: Instant) -> Point {
        let (asked, since) = (self.asked, self.since);
        let offset = *self.offset.get_or_insert_with(|| match asked {
            Some(a) if now.saturating_duration_since(since) < SETTLE => {
                let d = Vector::new(reported.x - a.x, reported.y - a.y);
                if d.x.abs() <= MAX_FRAME && d.y.abs() <= MAX_FRAME { d } else { Vector::ZERO }
            }
            _ => Vector::ZERO,
        });
        Point::new(reported.x - offset.x, reported.y - offset.y)
    }
}

/// How to open the viewer window: as it was last left.
pub fn open_settings(s: &Settings, monitor: Option<Size>) -> window::Settings {
    let position = match (s.viewer_x, s.viewer_y) {
        (Some(x), Some(y)) => window::Position::Specific(Point::new(x, y)),
        _ => window::Position::Centered,
    };
    window::Settings {
        size: initial_size(s, monitor),
        position,
        maximized: s.viewer_maximized,
        min_size: Some(MIN_SIZE),
        ..super::window_settings()
    }
}

/// The remembered size, kept within the screen; until there is one,
/// 58 % × 64 % of the screen (with a floor for when the monitor size is
/// unknown or reported oddly, as on XWayland).
pub fn initial_size(s: &Settings, monitor: Option<Size>) -> Size {
    let monitor = monitor.filter(|m| m.width >= MIN_SIZE.width && m.height >= MIN_SIZE.height);
    if s.viewer_width < MIN_SIZE.width || s.viewer_height < MIN_SIZE.height {
        return match monitor {
            Some(m) if m.width >= 1280.0 => Size::new((m.width * 0.58).round(), (m.height * 0.64).round()),
            _ => Size::new(1120.0, 720.0),
        };
    }
    match monitor {
        Some(m) => Size::new(s.viewer_width.min(m.width), s.viewer_height.min(m.height)),
        None => Size::new(s.viewer_width, s.viewer_height),
    }
}

/// A resize or move of the viewer window.
pub fn on_event(app: &mut App, ev: window::Event) -> Task<Message> {
    let Some(v) = window_viewer(app) else { return Task::none() };
    let id = v.id;
    match ev {
        window::Event::Resized(size) => {
            v.win.width = size.width;
            with_state(id, move |mode, maximized| WinMsg::Resized { size, mode, maximized })
        }
        window::Event::Moved(at) => with_state(id, move |mode, maximized| WinMsg::Moved { at, mode, maximized }),
        _ => Task::none(),
    }
}

/// The viewer that owns the window: on screen, or lent to the compare view.
fn window_viewer(app: &mut App) -> Option<&mut super::viewer::Viewer> {
    match (app.viewer.as_mut(), app.compare.as_mut()) {
        (Some(v), _) => Some(v),
        (None, Some(c)) => Some(c.viewer_mut()),
        (None, None) => None,
    }
}

/// Ask the window for its mode and maximised state, then report.
fn with_state(id: window::Id, report: impl Fn(Mode, bool) -> WinMsg + Clone + Send + 'static) -> Task<Message> {
    window::mode(id).then(move |mode| {
        let report = report.clone();
        window::is_maximized(id).map(move |maximized| Message::Viewer(Msg::Window(report(mode, maximized))))
    })
}

pub fn update(app: &mut App, msg: WinMsg) {
    let v = match (app.viewer.as_mut(), app.compare.as_mut()) {
        (Some(v), _) => v,
        (None, Some(c)) => c.viewer_mut(),
        (None, None) => return,
    };
    let s = &mut app.settings;
    match msg {
        WinMsg::Resized { size, mode, maximized } => {
            v.win.fullscreen = mode == Mode::Fullscreen;
            if mode == Mode::Windowed {
                s.viewer_maximized = maximized;
                if !maximized && size.width >= MIN_SIZE.width && size.height >= MIN_SIZE.height {
                    s.viewer_width = size.width;
                    s.viewer_height = size.height;
                }
            }
        }
        WinMsg::Moved { at, mode, maximized } => {
            if mode == Mode::Windowed && !maximized {
                let p = v.win.placement.to_store(at, Instant::now());
                s.viewer_x = Some(p.x);
                s.viewer_y = Some(p.y);
            }
        }
    }
}

/// F11: full screen, or back.
pub fn toggle_fullscreen(app: &App) -> Task<Message> {
    let Some(id) = app.viewer.as_ref().map(|v| v.id) else { return Task::none() };
    window::mode(id).then(move |mode| window::set_mode(id, if mode == Mode::Fullscreen { Mode::Windowed } else { Mode::Fullscreen }))
}

/// Escape in full screen.
pub fn leave_fullscreen(app: &App) -> Task<Message> {
    match app.viewer.as_ref() {
        Some(v) => window::set_mode(v.id, Mode::Windowed),
        None => Task::none(),
    }
}

/// The viewer closed: keep its geometry for next time.  (Quitting saves
/// every setting itself.)
pub fn remember(app: &App) {
    if !app.quitting {
        app.settings.save_in_background();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_open_takes_a_share_of_the_screen() {
        let s = Settings::default();
        assert_eq!(initial_size(&s, Some(Size::new(2560.0, 1440.0))), Size::new(1485.0, 922.0));
        assert_eq!(initial_size(&s, None), Size::new(1120.0, 720.0));
        assert_eq!(initial_size(&s, Some(Size::new(0.0, 0.0))), Size::new(1120.0, 720.0));
    }

    #[test]
    fn remembered_size_comes_back_within_the_screen() {
        let s = Settings { viewer_width: 1700.0, viewer_height: 900.0, ..Settings::default() };
        assert_eq!(initial_size(&s, Some(Size::new(2560.0, 1440.0))), Size::new(1700.0, 900.0));
        assert_eq!(initial_size(&s, Some(Size::new(1600.0, 1000.0))), Size::new(1600.0, 900.0));
        assert_eq!(initial_size(&s, None), Size::new(1700.0, 900.0));
        let settings = open_settings(&Settings { viewer_maximized: true, viewer_x: Some(40.0), viewer_y: Some(60.0), ..s }, None);
        assert!(settings.maximized);
        assert!(matches!(settings.position, window::Position::Specific(p) if p == Point::new(40.0, 60.0)));
        assert!(matches!(open_settings(&Settings::default(), None).position, window::Position::Centered));
    }

    #[test]
    fn settings_without_viewer_keys_load_with_defaults() {
        let s: Settings = serde_json::from_str(r#"{"thumb_size": 200}"#).expect("parses");
        assert_eq!(s.thumb_size, 200);
        assert!(!s.viewer_advance);
        assert!(s.viewer_filmstrip);
        assert_eq!((s.viewer_width, s.viewer_height, s.viewer_x, s.viewer_maximized), (0.0, 0.0, None, false));
    }

    #[test]
    fn a_frame_offset_seen_at_opening_is_taken_off() {
        let t0 = Instant::now();
        // Placed at (240, 156) by its frame; reports its client area 26 px lower.
        let mut p = Placement::new(Some(Point::new(240.0, 156.0)), t0);
        assert_eq!(p.to_store(Point::new(240.0, 182.0), t0 + Duration::from_millis(100)), Point::new(240.0, 156.0));
        // Later moves keep the same correction.
        assert_eq!(p.to_store(Point::new(500.0, 326.0), t0 + Duration::from_secs(20)), Point::new(500.0, 300.0));
    }

    #[test]
    fn no_correction_without_a_placement_to_compare() {
        let t0 = Instant::now();
        // Centred: nothing was asked for.
        let mut p = Placement::new(None, t0);
        assert_eq!(p.to_store(Point::new(300.0, 200.0), t0), Point::new(300.0, 200.0));
        // The first report came late (a user's move, not the placement).
        let mut p = Placement::new(Some(Point::new(240.0, 156.0)), t0);
        assert_eq!(p.to_store(Point::new(250.0, 170.0), t0 + Duration::from_secs(5)), Point::new(250.0, 170.0));
        // The window manager moved it far (e.g. onto the screen).
        let mut p = Placement::new(Some(Point::new(4000.0, 156.0)), t0);
        assert_eq!(p.to_store(Point::new(1200.0, 156.0), t0), Point::new(1200.0, 156.0));
    }
}
