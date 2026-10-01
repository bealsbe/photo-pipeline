//! The viewer's filmstrip (`ui::filmstrip`): which thumbnails it draws
//! from, what its clicks do, and the setting that shows it (T).
//!
//! Working-folder photos use the grid's thumbnail cache; library photos,
//! opened read-only from the calendar, use the calendar's, so the strip
//! reuses whatever either has already decoded.

use iced::{Element, Task};

use super::viewer::{self, Msg, Source, Viewer};
use super::{App, Message};
use crate::imaging::thumbs::{PRIORITY_PREFETCH, PRIORITY_VISIBLE};
use crate::ui::filmstrip::{FilmEvent, Filmstrip};

pub fn view<'a>(app: &'a App, v: &'a Viewer, interactive: bool) -> Element<'a, Message> {
    let (coll, thumbs) = match &v.source {
        Source::Working => (&app.coll, &app.thumbs),
        Source::Library(c) => (c.as_ref(), &app.calendar.thumbs),
    };
    Filmstrip::new(&v.items, coll, thumbs, v.index, app.scale_factor, |e| Message::Viewer(Msg::Film(e)))
        .interactive(interactive)
        .into()
}

pub fn on_event(app: &mut App, ev: FilmEvent) -> Task<Message> {
    let Some(v) = app.viewer.as_ref() else { return Task::none() };
    match ev {
        FilmEvent::Jump(i) => {
            let dir = if i > v.index { 1 } else { -1 };
            viewer::navigate(app, i, dir)
        }
        FilmEvent::Thumbs { visible, nearby } => {
            let wanted = visible.iter().map(|&id| (id, PRIORITY_VISIBLE)).chain(nearby.iter().map(|&id| (id, PRIORITY_PREFETCH)));
            match &v.source {
                Source::Working => {
                    for (id, priority) in wanted {
                        if let Some(r) = app.coll.get(id) {
                            app.thumbs.request(&app.workers, r, priority);
                        }
                    }
                }
                Source::Library(c) => {
                    let c = c.clone();
                    for (id, priority) in wanted {
                        if let Some(r) = c.get(id) {
                            app.calendar.thumbs.request(&app.workers, r, priority);
                        }
                    }
                }
            }
            Task::none()
        }
    }
}

/// T in the viewer, or its toggle: show or hide the strip (remembered).
pub fn toggle(app: &mut App) {
    app.settings.viewer_filmstrip = !app.settings.viewer_filmstrip;
    app.settings.save_in_background();
}
