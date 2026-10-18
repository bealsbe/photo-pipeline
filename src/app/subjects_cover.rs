//! The group photo: for a named group (a person or a fursuit, the same
//! code, never the two mixed), the face or head that stands for it on its
//! tile, its page's header, the search field's completions and chips, the
//! merge dialog and the side-by-side reviews (`Group::cover`, wherever it
//! is shown).  An unnamed group keeps the automatic pick (the clearest)
//! and offers no choice.
//!
//! - **On a group's page** (*Faces* / *Heads*): pick one, then *Set as
//!   group photo* in the pick strip; picked on the photo it already is,
//!   the strip offers *Use the automatic photo* instead.  ⋯ has *Change
//!   group photo…* (to *Faces* / *Heads*) and, while one is chosen, *Use
//!   the automatic photo*.  Each is one undo step (Ctrl+Z, the status
//!   bar's *Undo*), recorded as `cover`; the tile follows at once, nothing
//!   regroups.
//! - **In the group editor**: the same in its pick strip, for a column of
//!   an existing named group (or one being named there).  Like everything
//!   in the editor it is part of the arrangement: Ctrl+Z there takes it
//!   back, and it changes nothing until *Apply*, which makes it part of the
//!   same undo step (`GroupEdit::covers`).
//! - **Shown quietly**: the face that is the group photo now wears a small
//!   photo mark in its corner (named groups only), and its tooltip says
//!   whether it was chosen or picked automatically.
//! - **Kept** in `subjects.json` (`Subject::cover`, a field of the first
//!   format, so the file's version doesn't change).  It outlives regroups
//!   and rebuilds, and goes back to the automatic pick when its face
//!   leaves the group, is removed, or isn't found again; a merge keeps the
//!   kept name's photo.  No key of its own: G and P are taken (bursts,
//!   prune), and a click in the strip is as quick.
//!
//! No I/O here: the change goes to the engine, and the picture comes from
//! the crops the page already holds.

use iced::Element;
use iced::Task;

use super::kit::{self, tip};
use super::people::{FaceRef, GroupId, Kind};
use super::style;
use super::subjects::{self as subj, m as sm};
use super::subjects_editor::{Editor, Origin};
use super::subjects_person::{Person, Show};
use super::{App, Message};
use crate::model::decisions::At;
use crate::ui::icons::Glyph;
use crate::ui::theme as t;

/// The mark the group photo wears.
pub const MARK: Glyph = Glyph::Photos;

pub fn m(msg: Msg) -> Message {
    sm(subj::Msg::Cover(msg))
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A group's page: the face picked as its photo.
    SetPicked,
    /// A group's page: the automatic photo again.
    Automatic,
    /// ⋯ ▸ *Change group photo…*: to *Faces* / *Heads*, to pick one.
    Choose,
    /// The editor: the face picked as its column's group photo (`true`),
    /// or the automatic one again (`false`), in the arrangement.
    Editor(bool),
}

/// The photo a group's page shows for it now, and whether the user chose
/// it (else it is the automatic pick).  Named groups only.
fn page_cover(app: &App, p: &Person) -> Option<(FaceRef, bool)> {
    let g = app.subjects.group(&p.group).filter(|g| g.name.is_some())?;
    Some((g.cover.face, app.subjects.all.chosen_covers.contains(&g.id)))
}

/// Where a choice on a page was made, for the record.
fn at(kind: Kind) -> At {
    if kind == Kind::Fursuit { At::Heads } else { At::Faces }
}

fn crop_word(kind: Kind) -> &'static str {
    if kind == Kind::Fursuit { "head" } else { "face" }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::SetPicked | Msg::Automatic => {
            let Some(p) = app.subjects.person.as_mut() else { return Task::none() };
            let face = match msg {
                Msg::SetPicked => match p.picked[..] {
                    [one] => Some(one),
                    _ => return Task::none(),
                },
                _ => None,
            };
            let (group, kind) = (p.group.clone(), p.kind);
            p.picked.clear();
            p.menu = None;
            let name = subj::label(&app.subjects, &group);
            if !matches!(group, GroupId::Subject(_)) || app.subjects.group(&group).is_none_or(|g| g.name.is_none()) {
                return Task::none();
            }
            app.search.link.set_cover(&group, face, at(kind));
            let words = if face.is_some() { format!("{name}'s group photo is set.") } else { format!("{name}'s group photo is picked automatically again: the clearest.") };
            app.show_status_action(words, "Undo", sm(subj::Msg::Undo))
        }
        Msg::Choose => {
            let kind = app.subjects.person.as_ref().map_or(Kind::Person, |p| p.kind);
            let t = super::subjects_person::update(app, super::subjects_person::Msg::Show(Show::Faces));
            if let Some(p) = app.subjects.person.as_mut() {
                p.menu = None;
            }
            let s = app.show_status(format!("Pick a {}, then Set as group photo.", crop_word(kind)), 6000);
            Task::batch([t, s])
        }
        Msg::Editor(set) => {
            let Some(e) = app.subjects.editor.as_mut() else { return Task::none() };
            let Some(col) = e.ui.picked_in else { return Task::none() };
            let face = match (set, &e.ui.picked[..]) {
                (true, [one]) => Some(*one),
                (true, _) => return Task::none(),
                (false, _) => None,
            };
            e.set_cover(col, face);
            Task::none()
        }
    }
}

// ── a group's page ──────────────────────────────────────────────────────────

/// The pick strip's action with one face picked on a named group's page:
/// *Set as group photo*, or *Use the automatic photo* on the one chosen.
pub fn strip_action<'a>(app: &'a App, p: &'a Person) -> Option<Element<'a, Message>> {
    let [one] = p.picked[..] else { return None };
    let (cover, chosen) = page_cover(app, p)?;
    Some(if one == cover && chosen {
        tip(kit::icon_text_btn(MARK, "Use the automatic photo", t::INK, style::secondary(), Some(m(Msg::Automatic))), "Show the clearest photo for this group again (one undo step)", true)
    } else {
        tip(kit::icon_text_btn(MARK, "Set as group photo", t::INK, style::secondary(), Some(m(Msg::SetPicked))), format!("Show this {} for the group: its tile, its page, the search field (one undo step)", crop_word(p.kind)), true)
    })
}

/// ⋯'s rows on a named group's page: (glyph, words, message).
pub fn menu_rows(app: &App, p: &Person) -> Vec<(Glyph, &'static str, Message)> {
    let Some((_, chosen)) = page_cover(app, p) else { return Vec::new() };
    let mut v = vec![(MARK, "Change group photo…", m(Msg::Choose))];
    if chosen {
        v.push((MARK, "Use the automatic photo", m(Msg::Automatic)));
    }
    v
}

/// The mark on a face of *Faces* / *Heads*: the group photo now (named
/// groups only), with its words.
pub fn badge(app: &App, p: &Person, f: &FaceRef) -> Option<(Glyph, &'static str)> {
    let (cover, chosen) = page_cover(app, p)?;
    (cover == *f).then_some((MARK, if chosen { "The group photo" } else { "The group photo, picked automatically: the clearest" }))
}

// ── the group editor ────────────────────────────────────────────────────────

/// Whether column `col` stands for a named group whose photo can be
/// chosen: an existing group, named, or being named here.  (A new group
/// gets its photo on its page once it exists.)
fn editor_named(e: &Editor, col: usize) -> Option<&GroupId> {
    let c = e.cols.get(col)?;
    match &c.origin {
        Origin::Existing { id, was } if was.is_some() || !c.name.trim().is_empty() => Some(id),
        _ => None,
    }
}

/// The photo column `col`'s group will show once applied, and whether it
/// is chosen: the editor's choice if one was made, else the group's own
/// while it stays in the column; automatic ones are the clearest left.
pub fn editor_cover(app: &App, e: &Editor, col: usize) -> Option<(FaceRef, bool)> {
    let id = editor_named(e, col)?;
    let heads = &e.cols[col].heads;
    let clearest = || heads.iter().filter_map(|f| e.faces.get(f)).max_by(|a, b| a.clarity.total_cmp(&b.clarity)).map(|f| (f.face, false));
    match e.cover_choice(id) {
        Some(Some(f)) if heads.contains(&f) => Some((f, true)),
        Some(_) => clearest(),
        None => {
            let chosen = app.subjects.all.chosen_cover(id).filter(|f| heads.contains(f));
            chosen.map(|f| (f, true)).or_else(clearest)
        }
    }
}

/// The editor's pick strip action, with one face picked in a column of a
/// named group.
pub fn editor_action<'a>(app: &'a App, e: &'a Editor) -> Option<Element<'a, Message>> {
    let col = e.ui.picked_in?;
    let [one] = e.ui.picked[..] else { return None };
    let (cover, chosen) = editor_cover(app, e, col)?;
    Some(if one == cover && chosen {
        tip(kit::icon_text_btn(MARK, "Use the automatic photo", t::INK, style::secondary(), Some(m(Msg::Editor(false)))), "The clearest photo for this group again, when you apply", true)
    } else {
        tip(kit::icon_text_btn(MARK, "Set as group photo", t::INK, style::secondary(), Some(m(Msg::Editor(true)))), format!("Show this {} for the group once you apply: its tile, its page, the search field", crop_word(e.kind)), true)
    })
}

// ── the test script (`cover …`, `app::testing`) ─────────────────────────────

pub fn script(app: &mut App, arg: &str) -> Task<Message> {
    let words: Vec<&str> = arg.split_whitespace().collect();
    match words[..] {
        ["set"] => update(app, Msg::SetPicked),
        ["auto"] => update(app, Msg::Automatic),
        ["choose"] => update(app, Msg::Choose),
        ["editor", "set"] => update(app, Msg::Editor(true)),
        ["editor", "auto"] => update(app, Msg::Editor(false)),
        ["dump"] => {
            println!("[cover] {}", debug_state(app));
            Task::none()
        }
        _ => {
            eprintln!("[script] unknown cover action {arg}");
            Task::none()
        }
    }
}

fn short_face(f: &FaceRef) -> String {
    format!("{:02x}{:02x}@{}", f.key.0[0], f.key.0[1], f.thousandths()[0])
}

/// Each named group of the tab's kind and its photo (`*`: chosen); on a
/// page, the face shown marked and its place among the faces; in the
/// editor, each named column's photo.
pub fn debug_state(app: &App) -> String {
    let s = &app.subjects;
    let groups: Vec<String> = s
        .all
        .named(s.tab)
        .map(|g| format!("{}={}{}", g.name.clone().unwrap_or_default(), short_face(&g.cover.face), if s.all.chosen_covers.contains(&g.id) { "*" } else { "" }))
        .collect();
    let page = s.person.as_ref().map(|p| {
        let at = page_cover(app, p).and_then(|(c, chosen)| {
            let i = p.faces.as_ref()?.iter().position(|f| f.face == c)?;
            Some(format!("{i}{}", if chosen { "*" } else { "" }))
        });
        format!(" page=[marked={at:?} menu={:?} strip={}]", menu_rows(app, p).iter().map(|r| r.1).collect::<Vec<_>>(), strip_action(app, p).is_some())
    });
    let editor = s.editor.as_ref().map(|e| {
        let cols: Vec<String> = (0..e.cols.len()).map(|i| editor_cover(app, e, i).map_or("-".into(), |(f, chosen)| format!("{}{}", short_face(&f), if chosen { "*" } else { "" }))).collect();
        format!(" editor=[{}] action={}", cols.join(" "), editor_action(app, e).is_some())
    });
    format!("groups={groups:?}{}{}", page.unwrap_or_default(), editor.unwrap_or_default())
}
