//! "Not fursuit heads" (and, for people, "Not faces") on whole groups:
//! every head of a group, of several picked groups, or of the heads
//! picked on a group's page, removed as the viewer's "Not a fursuit
//! head" removes one (the user's `removed` list), so they are never
//! grouped, suggested or found again, whatever is looked at again.
//!
//! - **Asked first, every time.**  A confirmation says how many heads go
//!   and shows a few of them; a named group is named, and its name goes
//!   with its last head.  Enter confirms, Esc stops.
//! - **One undo step** for the whole removal (Ctrl+Z, or the status
//!   bar's *Undo*).
//! - **Kinds never mix**: the groups are the tab's kind, and the engine
//!   refuses another.
//! - **No I/O here**: the heads to show come from the engine
//!   (`Link::faces`, answered on the bus), and the change goes to it.

use std::sync::Arc;

use iced::widget::{Space, column, container, row, text};
use iced::{Alignment, Element, Length, Padding, Task};

use super::kit;
use super::people::{Face, FaceRef, Framing, GroupId, Kind, KindWords};
use super::style;
use super::subjects::{self as subj, m, px, shape};
use super::{App, Message};
use crate::ui::paint;
use crate::ui::subject_tile as tile;
use crate::ui::theme as t;
use crate::util;

/// Heads shown in the confirmation.
const SHOWN: usize = 8;
/// Their size.
const CROP: f32 = 48.0;

/// A removal waiting for its answer.
#[derive(Debug, Clone)]
pub struct Ask {
    pub kind: Kind,
    /// Each group, with the heads picked in it (`None`: all of them).
    pub targets: Vec<(GroupId, Option<Vec<FaceRef>>)>,
    /// The groups' heads asked for and not answered yet.
    pub waiting: Vec<u64>,
    /// The heads that would go, as they arrive (clearest first once all
    /// are in).
    pub faces: Vec<Face>,
    /// Where it was asked from.
    pub from: From,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// A group's page, the whole group (⋯ ▸ Not fursuit heads…).
    Page,
    /// The tab's picked groups.
    Tab,
    /// The heads picked on a group's page.
    Picked,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// ⋯ on a group's page: the whole group.
    AskPage,
    /// The tab's picked groups.
    AskTab,
    /// The heads picked on a group's page.
    AskPicked,
    Go,
    Cancel,
}

/// "fursuit heads", "faces".
pub fn nouns(kind: Kind) -> &'static str {
    match kind {
        Kind::Fursuit => "fursuit heads",
        _ => "faces",
    }
}

/// The action's name: "Not fursuit heads", "Not faces".
pub fn action(kind: Kind) -> String {
    format!("Not {}", nouns(kind))
}

/// The heads of `n` in words: "a fursuit head", "23 fursuit heads".
fn count(kind: Kind, n: usize) -> String {
    match (kind, n) {
        (Kind::Fursuit, 1) => "a fursuit head".to_string(),
        (_, 1) => "a face".to_string(),
        _ => format!("{n} {}", nouns(kind)),
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::AskPage => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            p.menu = None;
            let (group, kind) = (p.group.clone(), p.kind);
            ask(app, kind, vec![(group, None)], From::Page)
        }
        Msg::AskTab => {
            let s = &app.subjects;
            let kind = s.tab;
            let groups: Vec<(GroupId, Option<Vec<FaceRef>>)> = s.picked.iter().filter(|id| s.group(id).is_some_and(|g| g.kind == kind)).map(|id| (id.clone(), None)).collect();
            if groups.is_empty() {
                return Task::none();
            }
            ask(app, kind, groups, From::Tab)
        }
        Msg::AskPicked => {
            let Some(p) = app.subjects.person.as_ref() else { return Task::none() };
            if p.picked.is_empty() {
                return Task::none();
            }
            let (group, kind, picked) = (p.group.clone(), p.kind, p.picked.clone());
            let faces: Vec<Face> = p.faces.as_ref().map(|f| f.iter().filter(|x| picked.contains(&x.face)).cloned().collect()).unwrap_or_default();
            app.subjects.remove = Some(Ask { kind, targets: vec![(group, Some(picked))], waiting: Vec::new(), faces, from: From::Picked });
            subj::refresh_crops(app)
        }
        Msg::Cancel => {
            app.subjects.remove = None;
            Task::none()
        }
        Msg::Go => go(app),
    }
}

/// Ask about whole groups: their heads are asked for (a group's page
/// that has them already lends them), and the dialog counts as they come.
fn ask(app: &mut App, kind: Kind, targets: Vec<(GroupId, Option<Vec<FaceRef>>)>, from: From) -> Task<Message> {
    let mut a = Ask { kind, targets: targets.clone(), waiting: Vec::new(), faces: Vec::new(), from };
    for (id, _) in &targets {
        let lent = app.subjects.person.as_ref().filter(|p| &p.group == id).and_then(|p| p.faces.clone());
        match lent {
            Some(f) => a.faces.extend(f.iter().cloned()),
            None => {
                let seq = app.subjects.next_seq();
                a.waiting.push(seq);
                app.search.link.faces(seq, id);
            }
        }
    }
    app.subjects.remove = Some(a);
    subj::refresh_crops(app)
}

/// A group's heads arrived: the dialog's, if it asked (true).
pub fn on_faces(app: &mut App, seq: u64, faces: &Arc<Vec<Face>>) -> bool {
    let Some(a) = app.subjects.remove.as_mut() else { return false };
    let Some(i) = a.waiting.iter().position(|&s| s == seq) else { return false };
    a.waiting.remove(i);
    for f in faces.iter() {
        if !a.faces.iter().any(|x| x.face == f.face) {
            a.faces.push(f.clone());
        }
    }
    if a.waiting.is_empty() {
        a.faces.sort_by(|x, y| y.clarity.total_cmp(&x.clarity));
    }
    true
}

/// Confirmed: the engine removes them; the page follows.
fn go(app: &mut App) -> Task<Message> {
    let Some(a) = app.subjects.remove.take() else { return Task::none() };
    if !a.waiting.is_empty() || a.faces.is_empty() {
        app.subjects.remove = Some(a);
        return Task::none();
    }
    app.search.link.remove_boxes(a.kind, &a.targets);
    let n = a.faces.len();
    let words = format!("Removed {}: they won't be grouped, suggested or found again.", count(a.kind, n));
    let mut tasks = Vec::new();
    match a.from {
        From::Page => tasks.push(subj::to_overview(app)),
        From::Tab => {
            app.subjects.picked.clear();
            app.subjects.select_mode = false;
        }
        From::Picked => {
            if let Some(p) = app.subjects.person.as_mut() {
                p.picked.clear();
            }
        }
    }
    tasks.push(app.show_status_action(words, "Undo", m(subj::Msg::Undo)));
    Task::batch(tasks)
}

/// The crops the dialog shows.
pub fn wanted(app: &App) -> Vec<(Face, u32, Framing)> {
    let Some(a) = &app.subjects.remove else { return Vec::new() };
    a.faces.iter().take(SHOWN).map(|f| (f.clone(), px(app, CROP), Framing::Face)).collect()
}

/// The named groups among the targets, as they are called.
fn names(app: &App, a: &Ask) -> Vec<String> {
    a.targets.iter().filter_map(|(id, _)| app.subjects.group(id).and_then(|g| g.name.clone())).collect()
}

/// The confirmation.  Firmer when a named group loses its heads: it
/// names the group, and says the name goes with its last head.
pub fn dialog(app: &App) -> Option<Element<'_, Message>> {
    let a = app.subjects.remove.as_ref()?;
    let rm = |msg: Msg| m(subj::Msg::Remove(msg));
    let counting = !a.waiting.is_empty();
    let n = a.faces.len();
    let names = names(app, a);
    let whole = a.from != From::Picked;
    let what = nouns(a.kind);
    let title = match (names.as_slice(), whole) {
        ([one], true) => format!("Remove all of {one}'s {what}?"),
        ([one], false) => format!("Remove these {what} from {one}?"),
        _ => format!("{}?", action(a.kind)),
    };
    let says = if counting {
        format!("Counting the {what}…")
    } else {
        match n {
            1 => format!("This {} will be removed: it won't be grouped, suggested or found again, even when the library is looked at again.", count(a.kind, 1).trim_start_matches("a ")),
            _ => format!("These {n} {what} will be removed: they won't be grouped, suggested or found again, even when the library is looked at again."),
        }
    };
    let firm = match (names.as_slice(), whole) {
        ([], _) | (_, false) => None,
        ([one], true) => Some(format!("{one} is named. The name {one} goes too, with the photos you confirmed of them.")),
        (many, true) => Some(format!("Named among them: {}. Their names go too.", many.join(", "))),
    };
    let mut heads = row![].spacing(t::S2).align_y(Alignment::Center);
    for f in a.faces.iter().take(SHOWN) {
        heads = heads.push(tile::crop(app.subjects.crops.get(&f.face, px(app, CROP), Framing::Face), CROP, shape(a.kind)));
    }
    if n > SHOWN {
        heads = heads.push(kit::caption(format!("and {} more", util::thousands((n - SHOWN) as u64))));
    }
    let go_label = if counting { "Remove".to_string() } else { format!("Remove {}", count(a.kind, n)) };
    let mut col = column![super::dialogs::header(title, None), kit::secondary(says)].spacing(t::S3);
    if let Some(f) = firm {
        col = col.push(text(f).size(t::T_BODY).font(paint::MEDIUM).color(t::WARN));
    }
    if n > 0 {
        col = col.push(container(heads).width(Length::Fill).padding(Padding::from([t::S2, t::S2])).style(style::group()));
    }
    col = col.push(kit::caption("Ctrl+Z undoes it. Your photos are not changed."));
    col = col.push(Space::new().height(t::S1));
    col = col.push(super::dialogs::footer(
        vec![],
        vec![
            kit::btn("Cancel", style::secondary(), Some(rm(Msg::Cancel))).into(),
            kit::btn(go_label, style::danger(), (!counting && n > 0).then(|| rm(Msg::Go))).into(),
        ],
    ));
    Some(super::dialogs::backdrop(super::dialogs::frame_fit(col.into(), 540f32.min(app.window_size_width() - 48.0))))
}

/// The dialog's state, for the test driver's dumps.
pub fn debug_state(app: &App) -> String {
    match &app.subjects.remove {
        None => "none".into(),
        Some(a) => format!(
            "kind={} targets={} waiting={} heads={} from={:?} names={:?}",
            a.kind.one(),
            a.targets.len(),
            a.waiting.len(),
            a.faces.len(),
            a.from,
            names(app, a)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_follow_the_kind_and_the_count() {
        assert_eq!(action(Kind::Fursuit), "Not fursuit heads");
        assert_eq!(action(Kind::Person), "Not faces");
        assert_eq!(count(Kind::Fursuit, 1), "a fursuit head");
        assert_eq!(count(Kind::Fursuit, 23), "23 fursuit heads");
        assert_eq!(count(Kind::Person, 2), "2 faces");
    }
}
