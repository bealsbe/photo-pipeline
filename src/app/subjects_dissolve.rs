//! Dissolve a group: it isn't one person or one fursuit, only lookalikes
//! the grouping put together.  People and fursuits alike, never mixed.
//!
//! - **What it does.**  The group's heads leave it and are never put in
//!   one automatic group together again, through every regroup (the
//!   user's `dissolved` list, kept by heads).  Each stays a face or a
//!   fursuit head: nothing is removed, and it can still be suggested for
//!   another group, found by *Find more*, or sent to a group by hand.  A
//!   named group's name goes, with its confirmed heads and answers.
//! - **Asked first** when the group is named (the dialog says the name
//!   goes), or when the editor holds an arrangement not applied yet (it
//!   isn't applied).  Otherwise it happens at once, with *Undo* in the
//!   status bar.  Enter confirms, Esc stops.
//! - **One undo step** (Ctrl+Z, or the status bar's *Undo*) puts the
//!   group back as it was.
//! - **Where:** *Dissolve group* in the group editor's header, and ⋯ on a
//!   group's page.
//! - **No I/O here**: the heads shown come from the editor, the page, or
//!   the engine (`Link::faces`); the change goes to the engine, which
//!   records it (`dissolve` in the record of corrections).

use std::sync::Arc;

use iced::widget::{Space, column, container, row, text};
use iced::{Alignment, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{Face, Framing, GroupId, Kind};
use super::subjects_remove::{count, nouns};
use super::style;
use super::subjects::{self as subj, m, px, shape};
use super::{App, Message};
use crate::model::decisions::At;
use crate::ui::icons::Glyph;
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme as t;
use crate::util;

/// Heads shown in the confirmation, and their size.
const SHOWN: usize = 8;
const CROP: f32 = 48.0;

/// Where the dissolve was asked from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// The group editor's header.
    Editor,
    /// ⋯ on a group's page.
    Page,
}

impl From {
    fn at(self) -> At {
        match self {
            From::Editor => At::Editor,
            From::Page => At::Menu,
        }
    }
}

/// A dissolve waiting for its answer.
#[derive(Debug, Clone)]
pub struct Ask {
    pub group: GroupId,
    pub kind: Kind,
    pub name: Option<String>,
    /// The group's heads, clearest first (as they arrive, when asked for).
    pub faces: Vec<Face>,
    /// Its heads asked for and not answered yet.
    pub waiting: Option<u64>,
    /// The editor holds moves not applied (they won't be).
    pub arranged: bool,
    pub from: From,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Ask(From),
    Go,
    Cancel,
}

/// The action's name, the same on the button, in the menu and in the
/// dialog: "Dissolve group".
pub const ACTION: &str = "Dissolve group";

/// What the button's tip says, by kind.
pub fn about(kind: Kind) -> String {
    let one = if kind == Kind::Fursuit { "one fursuit" } else { "one person" };
    format!("Not {one}, only lookalikes: break the group up. Its {} stay, free for other groups, and are never grouped together again", nouns(kind))
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Ask(from) => ask(app, from),
        Msg::Cancel => {
            app.subjects.dissolve = None;
            Task::none()
        }
        Msg::Go => go(app),
    }
}

/// Ask (or, for an unnamed group with nothing arranged, dissolve now).
fn ask(app: &mut App, from: From) -> Task<Message> {
    let s = &app.subjects;
    let (group, kind, faces, arranged) = match from {
        From::Editor => {
            let Some(e) = s.editor.as_ref() else { return Task::none() };
            let home = e.home().clone();
            let mut faces: Vec<Face> = e.faces.values().filter(|f| e.origin_of(&f.face) == Some(&home)).cloned().collect();
            faces.sort_by(|x, y| y.clarity.total_cmp(&x.clarity));
            let loaded = e.cols.first().is_some_and(|c| c.loaded);
            (home, e.kind, loaded.then_some(faces), e.has_changes())
        }
        From::Page => {
            let Some(p) = s.person.as_ref() else { return Task::none() };
            let faces = p.faces.as_ref().map(|f| {
                let mut v: Vec<Face> = f.iter().cloned().collect();
                v.sort_by(|x, y| y.clarity.total_cmp(&x.clarity));
                v
            });
            (p.group.clone(), p.kind, faces, false)
        }
    };
    let Some(g) = s.group(&group) else { return Task::none() };
    if g.kind != kind {
        return Task::none();
    }
    let name = g.name.clone();
    if let Some(p) = app.subjects.person.as_mut() {
        p.menu = None;
    }
    let mut a = Ask { group: group.clone(), kind, name, faces: faces.clone().unwrap_or_default(), waiting: None, arranged, from };
    if a.name.is_none() && !a.arranged {
        app.subjects.dissolve = Some(a);
        return go(app);
    }
    if faces.is_none() {
        let seq = app.subjects.next_seq();
        a.waiting = Some(seq);
        app.search.link.faces(seq, &group);
    }
    app.subjects.dissolve = Some(a);
    subj::refresh_crops(app)
}

/// The group's heads arrived: the dialog's, if it asked (true).
pub fn on_faces(app: &mut App, seq: u64, faces: &Arc<Vec<Face>>) -> bool {
    let Some(a) = app.subjects.dissolve.as_mut() else { return false };
    if a.waiting != Some(seq) {
        return false;
    }
    a.waiting = None;
    a.faces = faces.iter().cloned().collect();
    a.faces.sort_by(|x, y| y.clarity.total_cmp(&x.clarity));
    true
}

/// Confirmed (or not asked): the engine dissolves it; the editor and the
/// group's page close, since the group is gone.
fn go(app: &mut App) -> Task<Message> {
    let Some(a) = app.subjects.dissolve.take() else { return Task::none() };
    let n = app.subjects.group(&a.group).map(|_| a.faces.len()).filter(|&n| n > 0).or_else(|| app.subjects.group(&a.group).map(|g| g.photos)).unwrap_or(0);
    app.search.link.dissolve(&a.group, a.from.at());
    app.subjects.editor = None;
    let on_page = app.subjects.person.as_ref().is_some_and(|p| p.group == a.group);
    let t = if on_page { subj::to_overview(app) } else { subj::refresh_crops(app) };
    let what = match &a.name {
        Some(name) => format!("Dissolved {name}: its {} are free for other groups, never grouped together again.", nouns(a.kind)),
        None => format!("Dissolved the group: {} free for other groups, never grouped together again.", count(a.kind, n)),
    };
    let s = app.show_status_action(what, "Undo", m(subj::Msg::Undo));
    Task::batch([t, s])
}

/// The crops the dialog shows.
pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let Some(a) = &app.subjects.dissolve else { return Vec::new() };
    a.faces.iter().take(SHOWN).map(|f| (f.clone(), px(app, CROP), Framing::Face)).collect()
}

/// The editor header's button.
pub fn editor_button<'a>(kind: Kind, loaded: bool) -> Element<'a, Message> {
    let b = kit::icon_text_btn(Glyph::Dissolve, ACTION, t::INK2, style::ghost(), loaded.then(|| m(subj::Msg::Dissolve(Msg::Ask(From::Editor)))));
    tip(b, about(kind), true).into()
}

/// The confirmation: named groups (the name goes), or an arrangement
/// the dissolve would drop.
pub fn dialog(app: &App) -> Option<Element<'_, Message>> {
    let a = app.subjects.dissolve.as_ref()?;
    let dm = |msg: Msg| m(subj::Msg::Dissolve(msg));
    let counting = a.waiting.is_some();
    let n = a.faces.len();
    let what = nouns(a.kind);
    let one = if a.kind == Kind::Fursuit { "one fursuit" } else { "one person" };
    let title = match &a.name {
        Some(name) => format!("Dissolve {name}?"),
        None => "Dissolve this group?".to_string(),
    };
    let says = if counting {
        format!("Counting the {what}…")
    } else {
        format!(
            "Its {} don't show {one}. They leave the group and are never grouped together again. Each stays a {}: it can still be suggested for another group, found with Find more, or added to one by hand.",
            count(a.kind, n).trim_start_matches("a "),
            what.trim_end_matches('s')
        )
    };
    let mut col = column![super::dialogs::header(title, None), kit::secondary(says)].spacing(t::S3);
    if let Some(name) = &a.name {
        col = col.push(text(format!("The name {name} goes too, with the {what} you confirmed as {name} and your answers.")).size(t::T_BODY).font(paint::MEDIUM).color(t::WARN));
    }
    if a.arranged {
        col = col.push(text("What you arranged in the editor isn't applied.").size(t::T_BODY).color(t::INK2));
    }
    if n > 0 {
        let mut heads = row![].spacing(t::S2).align_y(Alignment::Center);
        for f in a.faces.iter().take(SHOWN) {
            heads = heads.push(tile::crop(app.subjects.crops.get(&f.face, px(app, CROP), Framing::Face), CROP, shape(a.kind)));
        }
        if n > SHOWN {
            heads = heads.push(kit::caption(format!("and {} more", util::thousands((n - SHOWN) as u64))));
        }
        col = col.push(container(heads).width(Length::Fill).padding(Padding::from([t::S2, t::S2])).style(style::group()));
    }
    col = col.push(kit::caption("Ctrl+Z undoes it. Your photos are not changed."));
    col = col.push(Space::new().height(t::S1));
    let go_label = match &a.name {
        Some(name) => format!("Dissolve {name}"),
        None => ACTION.to_string(),
    };
    let go_style = if a.name.is_some() { style::danger() } else { style::primary() };
    col = col.push(super::dialogs::footer(vec![], vec![kit::btn("Cancel", style::secondary(), Some(dm(Msg::Cancel))).into(), kit::btn(go_label, go_style, (!counting).then(|| dm(Msg::Go))).into()]));
    Some(super::dialogs::backdrop(super::dialogs::frame_fit(col.into(), 540f32.min(app.window_size_width() - 48.0))))
}

/// The dialog's state, for the test driver's dumps.
pub fn debug_state(app: &App) -> String {
    match &app.subjects.dissolve {
        None => "none".into(),
        Some(a) => format!("kind={} name={:?} heads={} waiting={} arranged={} from={:?}", a.kind.noun(), a.name, a.faces.len(), a.waiting.is_some(), a.arranged, a.from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_follow_the_kind_and_the_count() {
        assert_eq!(count(Kind::Fursuit, 1), "a fursuit head");
        assert_eq!(count(Kind::Fursuit, 1200), "1,200 fursuit heads");
        assert_eq!(count(Kind::Person, 2), "2 faces");
        assert!(about(Kind::Fursuit).starts_with("Not one fursuit") && about(Kind::Person).contains("faces"));
    }
}
