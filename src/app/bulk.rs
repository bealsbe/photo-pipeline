//! The analysis' bulk actions: *Keep this, prune the rest*,
//! *Prune all soft frames…*, *Prune accidental frames…* and *Prune
//! duplicate copies…*.
//!
//! Every one is explicit and confirmed: a dialog says what will change,
//! with the counts and the frames themselves (their thumbnails, and why
//! each is there), and nothing is marked until the user says so.  Then it
//! is marked through `refresh_prune` / `refresh_mark` like any P or M, as
//! **one undo step**.  Only frames still to review are touched; nothing
//! goes to the Trash before Prune Review.
//!
//! This is the one place frames the user can't see on their own can be
//! marked (folded into a stack, or off screen): the dialog shows them
//! first.  The dialog opens in the window the action came from: the main
//! window, or the viewer (*Keep this, prune the rest* on a burst's sheet).

use iced::widget::{column, container, image, row, text};
use iced::{Alignment, ContentFit, Element, Length, Task, window};

use super::kit;
use super::style;
use super::{App, Message, View, dialogs, undo, viewer_burst};
use crate::imaging::thumbs::PRIORITY_VISIBLE;
use crate::model::collection::{Collection, RecordId};
use crate::model::hints::Plan;
use crate::ui::hint_plate;
use crate::ui::icons;
use crate::ui::paint;
use crate::ui::theme::{self as t};
use crate::util::{self, plural};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep the chosen frame, prune the rest of its burst.
    KeepThis,
    Soft,
    Accidental,
    Copies,
}

/// A bulk action waiting for the user's yes.
#[derive(Debug, Clone)]
pub struct Confirm {
    /// The window it was asked from, which shows the dialog.
    pub window: window::Id,
    pub action: Action,
    pub plan: Plan,
    /// *Keep this*: the frame kept, and the burst's frames in order.
    pub chosen: Option<RecordId>,
    pub frames: Vec<RecordId>,
    /// Shots the action looked through (what was shown).
    pub scope: usize,
    /// The grid was narrowed by a filter (the scope says so).
    pub filtered: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Confirm,
    Cancel,
}

/// What a confirmed plan changes, checked again when it is applied: a
/// frame decided on meanwhile is left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub prune: Vec<RecordId>,
    /// The frame kept loses a prune mark it had.
    pub unprune: Vec<RecordId>,
    pub mark: Vec<RecordId>,
}

/// Neither pruned nor marked (either half of a pair).
fn to_review(coll: &Collection, id: RecordId) -> bool {
    let decided = |id: RecordId| coll.get(id).is_some_and(|r| r.is_pruned || r.is_marked);
    coll.get(id).is_some() && !decided(id) && !coll.find_pair(id).is_some_and(decided)
}

pub fn changes(coll: &Collection, plan: &Plan, chosen: Option<RecordId>) -> Changes {
    Changes {
        prune: plan.prune.iter().copied().filter(|&id| to_review(coll, id)).collect(),
        unprune: chosen.filter(|&c| coll.get(c).is_some_and(|r| r.is_pruned)).into_iter().collect(),
        mark: plan.mark.iter().copied().filter(|&id| coll.get(id).is_some_and(|r| !r.is_marked)).collect(),
    }
}

// ── opening ─────────────────────────────────────────────────────────────────

/// The hints are there to act on, or why not.
fn ready(app: &App) -> Result<(), &'static str> {
    if app.coll.is_empty() || app.scanning() {
        return Err("Open a folder of photos first (Ctrl+O).");
    }
    if app.quality.analysing() {
        return Err("The folder is being analysed. Act on its hints once that's done, or stop it first.");
    }
    if app.quality.hints.coverage().0 == 0 {
        return Err("Analyse this folder first: the hints come from that (Prune ▸ Analyse this folder).");
    }
    Ok(())
}

/// A bulk action from the main window's menu.
pub fn open(app: &mut App, action: Action) -> Task<Message> {
    let Some(window) = app.main_id else { return Task::none() };
    if let Err(why) = ready(app) {
        return app.show_status(why, 6000);
    }
    if app.view.is_library() {
        return Task::none();
    }
    if action == Action::KeepThis {
        return open_keep_main(app, window);
    }
    // What is shown, frames folded into stacks included.
    let scope = app.derived.visible.clone();
    let h = &app.quality.hints;
    let plan = match action {
        Action::Soft => h.prune_soft(&app.coll, &scope),
        Action::Accidental => h.prune_accidental(&app.coll, &scope),
        _ => h.prune_duplicates(&app.coll, &scope),
    };
    if plan.prune.is_empty() {
        let what = match action {
            Action::Soft => "Nothing to prune: no photo shown that's still to review looks out of focus or accidental.",
            Action::Accidental => "Nothing to prune: no photo shown that's still to review looks accidental.",
            _ => "Nothing to prune: no photo shown that's still to review is a copy of another.",
        };
        return app.show_status(what, 6000);
    }
    show(app, Confirm { window, action, plan, chosen: None, frames: Vec::new(), scope: scope.len(), filtered: filtered(app) })
}

fn filtered(app: &App) -> bool {
    let f = &app.filter;
    f.date_on || f.marked_only || f.to_review || f.pruned_only || f.hint.is_some()
}

/// *Keep this, prune the rest* on the grid's (or quick look's) selected
/// photo: its burst as the grid shows it.
fn open_keep_main(app: &mut App, window: window::Id) -> Task<Message> {
    let target = match (&app.quicklook, app.view) {
        (Some(q), _) => Some(q.id),
        (None, View::Grid) => app.grid_focus(),
        _ => None,
    };
    let Some(id) = target else {
        return app.show_status("Select the frame to keep first.", 5000);
    };
    if app.view == View::Grid && app.quicklook.is_none() && app.is_stack(id) {
        return app.show_status("A stacked burst can't be marked. Open it (B or Enter) and choose the frame to keep there.", 6000);
    }
    // The shot's own record as the grid shows it (a pair's JPG).
    let id = app.coll.find_pair(id).filter(|&p| app.derived.visible.contains(&p)).unwrap_or(id);
    let of: Vec<Option<u32>> = app.derived.visible.iter().map(|&v| app.bursts.of(v)).collect();
    let at = app.derived.visible.iter().position(|&v| v == id);
    let frames = match at.and_then(|i| viewer_burst::run_at(&of, i)) {
        Some(run) => app.derived.visible[run].to_vec(),
        // The hints filter hides the rest of its burst (showing only the
        // sharpest frame of each, say): the whole burst, which the dialog
        // shows before anything is marked.
        None if app.filter.hint.is_some() => app.bursts.of(id).map(|b| app.bursts.frames(b).to_vec()).unwrap_or_default(),
        None => Vec::new(),
    };
    if frames.len() < 2 {
        let name = app.coll.get(id).map(|r| r.stem()).unwrap_or_default();
        return app.show_status(format!("{name} isn't part of a burst."), 4000);
    }
    keep(app, window, id, frames)
}

/// *Keep this, prune the rest* on the viewer's frame (on its burst's
/// sheet, the outlined one).
pub fn open_keep_viewer(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_ref().filter(|v| !v.read_only) else { return Task::none() };
    let window = v.id;
    let (Some(id), Some(run)) = (v.record_id(), viewer_burst::run(v)) else {
        return app.show_status("Keep this, prune the rest works on a frame of a burst.", 4000);
    };
    let frames = v.items[run].to_vec();
    if let Err(why) = ready(app) {
        if let Some(v) = app.viewer.as_mut() {
            v.sheet.note = Some(why.to_string());
        }
        return Task::none();
    }
    keep(app, window, id, frames)
}

fn keep(app: &mut App, window: window::Id, chosen: RecordId, frames: Vec<RecordId>) -> Task<Message> {
    let plan = app.quality.hints.keep_this_prune_rest(&app.coll, &frames, chosen);
    let unprune = app.coll.get(chosen).is_some_and(|r| r.is_pruned);
    if plan.prune.is_empty() && plan.mark.is_empty() && !unprune {
        return app.show_status("Nothing to change: this frame is kept and the others are decided already.", 5000);
    }
    let scope = frames.len();
    show(app, Confirm { window, action: Action::KeepThis, plan, chosen: Some(chosen), frames, scope, filtered: false })
}

/// Ask: the thumbnails the dialog shows are wanted first.
fn show(app: &mut App, c: Confirm) -> Task<Message> {
    for id in shown_ids(&c) {
        if let Some(r) = app.coll.get(id) {
            if app.thumbs.get(&r.path).is_none() {
                app.thumbs.request(&app.workers, r, PRIORITY_VISIBLE);
            }
        }
    }
    app.quality.menu = false;
    app.bulk = Some(c);
    Task::none()
}

// ── answering ───────────────────────────────────────────────────────────────

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Cancel => {
            app.bulk = None;
            Task::none()
        }
        Msg::Confirm => apply(app),
    }
}

pub fn on_key(app: &mut App, key: &iced::keyboard::Key) -> Task<Message> {
    use iced::keyboard::{Key, key::Named};
    match key {
        Key::Named(Named::Escape) => update(app, Msg::Cancel),
        Key::Named(Named::Enter) => update(app, Msg::Confirm),
        _ => Task::none(),
    }
}

fn stem(app: &App, id: RecordId) -> String {
    app.coll.get(id).map(|r| r.stem()).unwrap_or_default()
}

/// "5 soft frames", "1 duplicate copy", "3 duplicate copies".
pub fn things(n: usize, one: &str) -> String {
    let many = match one.strip_suffix('y') {
        Some(stem) => format!("{stem}ies"),
        None => format!("{one}s"),
    };
    format!("{} {}", util::thousands(n as u64), if n == 1 { one } else { &many })
}

/// "prune 5 soft frames" → "Pruned 5 soft frames"; "keep X and prune …"
/// → "Kept X and pruned …".
pub fn past(what: &str) -> String {
    let s = what.replacen("keep ", "Kept ", 1).replace(" prune ", " pruned ");
    match s.strip_prefix("prune ") {
        Some(rest) => format!("Pruned {rest}"),
        None => s,
    }
}

fn apply(app: &mut App) -> Task<Message> {
    let Some(c) = app.bulk.take() else { return Task::none() };
    let ch = changes(&app.coll, &c.plan, c.chosen);
    let n = ch.prune.len();
    // The action keeps its name: the menu's "Prune all soft frames…", the
    // dialog's "Prune 5 soft frames?", then "Pruned 5 soft frames" and,
    // in the history, "prune 5 soft frames".
    let what = match c.action {
        Action::KeepThis => {
            let name = c.chosen.map(|id| stem(app, id)).unwrap_or_default();
            if n == 0 { format!("keep {name}") } else { format!("keep {name} and prune the rest of its burst ({})", plural(n as u64, "frame")) }
        }
        Action::Soft => format!("prune {}", things(n, "soft frame")),
        Action::Accidental => format!("prune {}", things(n, "accidental frame")),
        Action::Copies => format!("prune {}", things(n, "duplicate copy")),
    };
    let label = what.clone();
    let done = format!("{}.", past(&what));
    let (task, serial) = undo::batch(app, label, |app| {
        let mut tasks = Vec::new();
        for &id in &ch.prune {
            if let Some(r) = app.coll.get_mut(id) {
                r.is_pruned = true;
            }
        }
        for &id in &ch.unprune {
            if let Some(r) = app.coll.get_mut(id) {
                r.is_pruned = false;
            }
        }
        let touched: Vec<RecordId> = ch.prune.iter().chain(&ch.unprune).copied().collect();
        if !touched.is_empty() {
            tasks.push(app.refresh_prune(&touched));
        }
        for &id in &ch.mark {
            if let Some(r) = app.coll.get_mut(id) {
                r.is_marked = true;
            }
        }
        if !ch.mark.is_empty() {
            tasks.push(app.refresh_mark(&ch.mark));
        }
        Task::batch(tasks)
    });
    let status = match serial {
        Some(s) => app.show_status_action(format!("{done} Nothing goes to the Trash until you review pruned files."), "Undo", Message::UndoStep(s)),
        None => app.show_status("Nothing changed: those frames were decided on meanwhile.", 5000),
    };
    Task::batch([task, status])
}

// ── the dialog ──────────────────────────────────────────────────────────────

/// Tiles in the dialog at most; the rest are counted.
const SHOWN: usize = 10;
const SHOWN_KEEP: usize = 15;
const TILE_W: f32 = 104.0;
const TILE_H: f32 = 70.0;

/// Tiles per row: as few rows of up to five as hold them, as even as
/// they can be (11 frames: 4, 4 and 3).
fn per_row(n: usize) -> usize {
    let rows = n.max(1).div_ceil(5);
    n.max(1).div_ceil(rows)
}

fn shown_ids(c: &Confirm) -> Vec<RecordId> {
    match c.action {
        Action::KeepThis => c.frames.iter().copied().take(SHOWN_KEEP).collect(),
        _ => c.plan.prune.iter().copied().take(SHOWN).collect(),
    }
}

/// The dialog, for the window `id` (if the action is waiting there).
pub fn overlay(app: &App, id: window::Id) -> Option<Element<'_, Message>> {
    let c = app.bulk.as_ref().filter(|c| c.window == id)?;
    let n = c.plan.prune.len();
    let frames = plural(n as u64, "frame");
    let (title, body, ok) = match c.action {
        Action::KeepThis => {
            let name = c.chosen.map(|id| stem(app, id)).unwrap_or_default();
            let left = c.frames.len().saturating_sub(n + 1);
            let mut body = if n == 0 {
                format!("{name} is marked to keep. The burst's other frames are decided already.")
            } else {
                format!("{name} is marked to keep, and the {} of its burst you haven't decided on are marked to prune.", plural(n as u64, "other frame"))
            };
            if left > 0 && n > 0 {
                body.push_str(&format!(" {} already pruned or kept {} left as {}.", util::thousands(left as u64), if left == 1 { "is" } else { "are" }, if left == 1 { "it is" } else { "they are" }));
            }
            (format!("Keep {name} and prune the rest of its burst?"), body, if n == 0 { format!("Keep {name}") } else { format!("Keep 1, prune {n}") })
        }
        Action::Soft => (
            format!("Prune {}?", things(n, "soft frame")),
            "Each looks out of focus or accidental. They are marked to prune, as P marks a photo.".to_string(),
            format!("Prune {frames}"),
        ),
        Action::Accidental => (
            format!("Prune {}?", things(n, "accidental frame")),
            "Each is almost black, white or one tone, with nothing sharp in it. They are marked to prune, as P marks a photo.".to_string(),
            format!("Prune {frames}"),
        ),
        Action::Copies => (
            format!("Prune {}?", things(n, "duplicate copy")),
            "Each is a file that is in this folder twice. The other copy stays: the one whose name doesn't read like a copy's, else the earlier export to the library, else the older file.".to_string(),
            format!("Prune {}", things(n, "copy")),
        ),
    };
    let scope = if c.action == Action::KeepThis {
        "Frames".to_string()
    } else if c.filtered {
        format!("From the {} the filters show; frames", plural(c.scope as u64, "shot"))
    } else {
        format!("From all {} in this folder; frames", plural(c.scope as u64, "shot"))
    };
    let foot = format!("{scope} you've already pruned or kept are left alone. Nothing goes to the Trash until you review pruned files, and Ctrl+Z undoes it all in one step.");
    let ids = shown_ids(c);
    let total = if c.action == Action::KeepThis { c.frames.len() } else { n };
    let mut tiles = column![].spacing(t::S3).align_x(Alignment::Center).width(Length::Fill);
    for chunk in ids.chunks(per_row(ids.len())) {
        let mut r = row![].spacing(t::S2);
        for &id in chunk {
            r = r.push(tile(app, c, id));
        }
        tiles = tiles.push(r);
    }
    if total > ids.len() {
        tiles = tiles.push(text(format!("and {} more", util::thousands((total - ids.len()) as u64))).size(t::T_SMALL).color(t::PHOTO_INK3));
    }
    let well = container(tiles).padding(t::S3).width(Length::Fill).style(|_| container::Style {
        background: Some(iced::Background::Color(t::PHOTO_GROUND)),
        border: iced::Border { color: t::LINE, width: 1.0, radius: t::R_CONTROL.into() },
        ..Default::default()
    });
    let content = column![
        dialogs::header(title, None),
        kit::secondary(body),
        well,
        kit::caption(foot),
        dialogs::footer(
            vec![],
            vec![
                kit::btn("Cancel", style::secondary(), Some(Message::Bulk(Msg::Cancel))).into(),
                kit::btn(ok, style::danger(), Some(Message::Bulk(Msg::Confirm))).into(),
            ],
        ),
    ]
    .spacing(t::S3);
    let w = (5.0 * TILE_W + 4.0 * t::S2 + 2.0 * t::S3 + 2.0 * t::S5 + 2.0).ceil();
    Some(dialogs::backdrop(dialogs::frame_fit(content.into(), w)))
}

/// One frame in the dialog: its thumbnail on the neutral ground, its
/// name, and what happens to it (or why it is there).
fn tile<'a>(app: &'a App, c: &Confirm, id: RecordId) -> Element<'a, Message> {
    let r = app.coll.get(id);
    let keep = c.action == Action::KeepThis;
    let chosen = Some(id) == c.chosen;
    // Keep this previews the result: the frames to prune dim, the one to
    // keep framed in green.
    let dim = keep && !chosen;
    let picture: Element<'a, Message> = match r.and_then(|r| app.thumbs.get(&r.path)).filter(|th| th.ok) {
        Some(th) => image(th.level_for(TILE_W * app.scale_factor).handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(ContentFit::Contain)
            .opacity(if dim { 0.5_f32 } else { 1.0 })
            .into(),
        None => text(r.map(|r| r.file_type.label()).unwrap_or_default()).size(t::T_SMALL).color(t::PHOTO_INK4).into(),
    };
    let edge = if keep && chosen { t::KEEP } else { iced::Color::TRANSPARENT };
    let frame = container(picture).width(TILE_W).height(TILE_H).padding(2).center(Length::Fixed(TILE_W)).style(move |_| container::Style {
        background: Some(iced::Background::Color(t::PHOTO_MOUNT)),
        border: iced::Border { color: edge, width: 2.0, radius: t::R_CELL.into() },
        ..Default::default()
    });
    let name = r.map(|r| paint::elide_start(&r.stem(), t::T_SMALL, paint::REGULAR, TILE_W)).unwrap_or_default();
    let what: Element<'a, Message> = match c.action {
        Action::KeepThis => {
            let (word, tone) = if chosen {
                ("Keep", t::KEEP)
            } else if c.plan.prune.contains(&id) {
                ("Prune", t::REJECT)
            } else if r.is_some_and(|r| r.is_marked) {
                ("Kept already", t::PHOTO_INK3)
            } else {
                ("Pruned already", t::PHOTO_INK3)
            };
            text(word).size(t::T_SMALL).font(paint::MEDIUM).color(tone).into()
        }
        _ => {
            let h = &app.quality.hints;
            let words = hint_plate::reason(h, &app.coll, id).unwrap_or_default();
            let mut line = row![].spacing(5).align_y(Alignment::Center).width(TILE_W);
            let mut room = TILE_W;
            if let Some(plate) = h.plate(id) {
                line = line.push(icons::view(hint_plate::glyph(plate), 12.0, hint_plate::tone(plate)));
                room -= 17.0;
            }
            // "copy of …0846_0001": the end of a long name is what tells.
            let words = match words.strip_prefix("copy of ") {
                Some(name) => {
                    let lead = paint::text_width("copy of ", t::T_SMALL, paint::REGULAR);
                    format!("copy of {}", paint::elide_start(name, t::T_SMALL, paint::REGULAR, (room - lead).max(0.0)))
                }
                None => paint::elide_start(&words, t::T_SMALL, paint::REGULAR, room),
            };
            line.push(text(words).size(t::T_SMALL).color(t::PHOTO_INK2)).into()
        }
    };
    column![frame, text(name).size(t::T_SMALL).color(t::PHOTO_INK2), what].spacing(3).width(TILE_W).align_x(Alignment::Start).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::{FileType, MTime, PhotoRecord};
    use std::path::PathBuf;

    fn rec(path: &str) -> PhotoRecord {
        let p = PathBuf::from(path);
        PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), 10, MTime::from_unix(1_700_000_000, 0), None)
    }

    #[test]
    fn an_action_keeps_its_name_from_the_dialog_to_the_history() {
        assert_eq!(things(1, "soft frame"), "1 soft frame");
        assert_eq!(things(5, "soft frame"), "5 soft frames");
        assert_eq!(things(3, "duplicate copy"), "3 duplicate copies");
        assert_eq!(things(1_204, "copy"), "1,204 copies");
        assert_eq!(past("prune 5 soft frames"), "Pruned 5 soft frames");
        assert_eq!(past("keep DSC0757 and prune the rest of its burst (7 frames)"), "Kept DSC0757 and pruned the rest of its burst (7 frames)");
        assert_eq!(past("keep DSC0757"), "Kept DSC0757");
        assert_eq!((per_row(3), per_row(6), per_row(7), per_row(10), per_row(11), per_row(15)), (3, 3, 4, 5, 4, 5));
    }

    #[test]
    fn applying_a_plan_rechecks_what_is_still_to_review() {
        let mut c = Collection::new();
        let a = c.add(rec("/s/a.JPG"));
        let a_raw = c.add(rec("/s/a.ARW"));
        let b = c.add(rec("/s/b.JPG"));
        let k = c.add(rec("/s/k.JPG"));
        let d = c.add(rec("/s/d.JPG"));
        c.build_pairs();
        let plan = Plan { mark: vec![k], prune: vec![a, b, d] };
        // Meanwhile: b was marked, a's RAW half pruned, and the frame to
        // keep had been pruned.
        c.get_mut(b).unwrap().is_marked = true;
        c.get_mut(a_raw).unwrap().is_pruned = true;
        c.get_mut(k).unwrap().is_pruned = true;
        let ch = changes(&c, &plan, Some(k));
        assert_eq!(ch, Changes { prune: vec![d], unprune: vec![k], mark: vec![k] });
        // Already kept: nothing to mark, nothing to clear.
        c.get_mut(k).unwrap().is_pruned = false;
        c.get_mut(k).unwrap().is_marked = true;
        assert_eq!(changes(&c, &plan, Some(k)), Changes { prune: vec![d], unprune: vec![], mark: vec![] });
        // Without a frame to keep (the prune actions).
        assert!(changes(&c, &Plan { mark: vec![], prune: vec![d] }, None).unprune.is_empty());
    }
}
