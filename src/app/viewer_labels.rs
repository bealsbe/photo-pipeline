//! The viewer's labels: faces and fursuit heads on the photo in view,
//! named, renamed, moved to the other kind, drawn where the finder
//! missed one, or removed.
//!
//! - **L** (or *Labels* in the top bar) shows a hollow box round every
//!   face (teal) and fursuit head (peach) found in the photo, each with a
//!   tag: the person's or fursuit's name, solid, or "Unnamed", hollow.
//!   The boxes are drawn by the stage itself (`ui::image_stage`), so they
//!   follow zoom and pan for free and cost a few quads a frame.
//! - **A bar** under the top bar (the crop bar's place) does the editing:
//!   with nothing picked it says how to start; a picked box takes a name
//!   (a name the kind has, offered as you type, or a new one), *Unnamed*,
//!   *It's a fursuit* (or *It's a person*) and *Not a face* (or *Not a
//!   fursuit head*); a box drawn on the photo asks *Person | Fursuit* and
//!   an optional name, then *Add*.
//! - **Every change goes to the engine** (`search_link`), which keeps it
//!   in the user's subjects file (never the photo or a sidecar), one undo
//!   step each: Ctrl+Z here takes back the last, as on the subjects
//!   pages.  Kinds never mix: a name list holds the box's own kind only,
//!   and the engine refuses a mix.
//! - **Marks are untouched.**  Nothing here prunes or marks; it works the
//!   same in a read-only viewer (search results, the library).
//! - **Which photos.**  Faces and heads are found in the library only; a
//!   working-folder photo identical to a library copy has its labels, any
//!   other says so.
//!
//! Behind `PP_SEARCH_FAKE` the stand-in ([`FakeLabels`]) answers with the
//! made-up people's and fursuits' boxes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::keyboard::{Key, key::Named};
use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Alignment, Color, Element, Length, Padding, Task};

use super::kit;
use super::style;
use super::{App, Message};
use crate::model::record::FileType;
use crate::model::subjects::{FaceRef, GroupId, Kind, LabelBox, LabelEdit, LabelTarget, NameTo, PendingWhy, PhotoLabels, SubjectId, Subjects, box_noun};
use crate::search::index::Key as ContentKey;
use crate::ui::icons::{self, Glyph};
use crate::ui::image_stage::{LabelProps, StageBox};
use crate::ui::paint;
use crate::ui::theme as t;

/// The name field's id (focused when a box is picked).
pub const NAME_FIELD: &str = "label-name";
/// Names offered under the field at most.
const OFFERED: usize = 5;

/// The overlay's state in the viewer.
#[derive(Debug, Default)]
pub struct LabelsUi {
    pub on: bool,
    /// The request the answer must match.
    seq: u64,
    /// The record the labels are for.
    record: Option<crate::model::collection::RecordId>,
    /// The engine's answer; `None` while it is on its way.
    pub labels: Option<Arc<PhotoLabels>>,
    /// The picked box, by its detection and kind (it survives a refresh).
    picked: Option<(FaceRef, Kind)>,
    /// A box drawn on the photo, waiting for its kind (and name).
    draft: Option<[f32; 4]>,
    /// The kind a drawn box takes (the last one chosen).
    draft_kind: Kind,
    /// The name field.
    name: String,
    /// What the last change did, or what Ctrl+Z took back.
    note: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LMsg {
    Toggle,
    Name(String),
    /// Enter in the name field.
    Submit,
    /// A name offered under the field.
    Pick(SubjectId),
    Unnamed,
    /// It's the other kind.
    SwapKind,
    Remove,
    /// Back to no box picked.
    Deselect,
    DraftKind(Kind),
    Add,
    Cancel,
}

fn lmsg(m: LMsg) -> Message {
    Message::Viewer(super::viewer::Msg::Labels(m))
}

/// A kind's colour on the photo.
pub fn color(kind: Kind) -> Color {
    match kind {
        Kind::Fursuit => t::LABEL_FURSUIT,
        _ => t::LABEL_PERSON,
    }
}

/// The word for a box of `kind`, capitalised for a label: "Face".
fn noun_cap(kind: Kind) -> String {
    let n = box_noun(kind);
    let mut c = n.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn other(kind: Kind) -> Kind {
    if kind == Kind::Fursuit { Kind::Person } else { Kind::Fursuit }
}

impl LabelsUi {
    fn boxes(&self) -> &[LabelBox] {
        self.labels.as_ref().map_or(&[], |l| l.boxes.as_slice())
    }

    fn picked_index(&self) -> Option<usize> {
        let (face, kind) = self.picked?;
        self.boxes().iter().position(|b| b.face == face && b.kind == kind)
    }

    fn picked_box(&self) -> Option<&LabelBox> {
        self.picked_index().map(|i| &self.boxes()[i])
    }

}

/// The stage's part: the boxes, the picked one, the draft.
pub fn stage_props(ui: &LabelsUi) -> Option<LabelProps> {
    if !ui.on {
        return None;
    }
    let boxes = ui
        .boxes()
        .iter()
        .map(|b| StageBox { bbox: b.face.bbox, color: color(b.kind), tag: b.name.clone().unwrap_or_else(|| "Unnamed".into()), named: b.name.is_some(), dashed: b.pending })
        .collect();
    Some(LabelProps { boxes, selected: ui.picked_index(), draft: ui.draft.map(|d| (d, color(ui.draft_kind))) })
}

// ── asking ──────────────────────────────────────────────────────────────────

/// The files of the shot in view: (JPG, RAW).
fn shot_files(app: &App) -> Option<(crate::model::collection::RecordId, Option<PathBuf>, Option<PathBuf>)> {
    let v = app.viewer.as_ref()?;
    let c = super::viewer::coll(app, v);
    let id = v.record_id()?;
    let rec = c.get(id)?;
    let pair = v.pair.and_then(|p| c.get(p));
    let (mut jpg, mut raw) = (None, None);
    for r in std::iter::once(rec).chain(pair) {
        match r.file_type {
            FileType::Jpg => jpg = jpg.or(Some(r.path.clone())),
            FileType::Raw => raw = raw.or(Some(r.path.clone())),
        }
    }
    Some((id, jpg, raw))
}

/// Ask for the labels of the photo in view.  A photo already answered
/// for is asked by its key (no lookup); a new one by its files.
pub fn request(app: &mut App) {
    let Some((id, jpg, raw)) = shot_files(app) else { return };
    let Some(v) = app.viewer.as_mut() else { return };
    let ui = &mut v.labels;
    if !ui.on {
        return;
    }
    let same = ui.record == Some(id);
    let target = match ui.labels.as_ref().and_then(|l| l.key).filter(|_| same) {
        Some(k) => LabelTarget::Key(k),
        None => LabelTarget::Files { jpg, raw },
    };
    if !same {
        ui.labels = None;
        ui.picked = None;
        ui.draft = None;
        ui.name.clear();
        ui.note = None;
    }
    ui.record = Some(id);
    ui.seq += 1;
    let seq = ui.seq;
    app.search.link.photo_labels(seq, target);
}

/// The photo in view changed (or was shown again): its labels, if shown.
pub fn on_photo(app: &mut App) {
    if app.viewer.as_ref().is_some_and(|v| v.labels.on) {
        request(app);
    }
}

/// The engine answered.
pub fn on_labels(app: &mut App, seq: u64, labels: Arc<PhotoLabels>) {
    let Some(v) = app.viewer.as_mut() else { return };
    let ui = &mut v.labels;
    if seq != ui.seq {
        return;
    }
    ui.labels = Some(labels);
    if ui.picked_index().is_none() {
        ui.picked = None;
    }
}

/// The subjects changed (a name given here or on a page, a regroup, an
/// undo): the names on the boxes follow.
pub fn on_subjects(app: &mut App) {
    if app.viewer.as_ref().is_some_and(|v| v.labels.on && v.labels.labels.is_some()) {
        request(app);
    }
}

/// Ctrl+Z took a subjects change back: say so in the bar.
pub fn on_undone(app: &mut App, what: &str) {
    if let Some(v) = app.viewer.as_mut().filter(|v| v.labels.on) {
        v.labels.note = Some(format!("Undone: {}.", what.trim_end_matches('.')));
        request(app);
    }
}

// ── changing ────────────────────────────────────────────────────────────────

/// Show or hide the labels.  Not over a burst's sheet, and the crop tool
/// gives way.
pub fn toggle(app: &mut App) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    if super::viewer_burst::on_sheet(v) {
        return Task::none();
    }
    let on = !v.labels.on;
    v.labels = LabelsUi { on, draft_kind: v.labels.draft_kind, ..LabelsUi::default() };
    if on {
        super::viewer::leave_crop(v);
        request(app);
    }
    Task::none()
}

/// Send a change, then ask again for the photo's labels.
fn send(app: &mut App, edit: LabelEdit) {
    app.search.link.label(edit);
    request(app);
}

/// The subject a typed name means: one of the kind's with that name
/// (any case), else a new one.
fn name_to(all: &Subjects, kind: Kind, typed: &str) -> Option<NameTo> {
    let n = typed.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        return None;
    }
    Some(match super::people::by_name(all, kind, &n).map(|g| g.id.clone()) {
        Some(GroupId::Subject(id)) => NameTo::Subject(id),
        _ => NameTo::New(n),
    })
}

pub fn update(app: &mut App, msg: LMsg) -> Task<Message> {
    let all = app.subjects.all.clone();
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let ui = &mut v.labels;
    match msg {
        LMsg::Toggle => return toggle(app),
        LMsg::Name(s) => ui.name = s,
        LMsg::Deselect => {
            ui.picked = None;
            ui.name.clear();
            return unfocus();
        }
        LMsg::DraftKind(k) => ui.draft_kind = k,
        LMsg::Cancel => {
            ui.draft = None;
            ui.name.clear();
            return unfocus();
        }
        LMsg::Add | LMsg::Submit if ui.draft.is_some() => {
            let (Some(d), Some(key)) = (ui.draft, ui.labels.as_ref().and_then(|l| l.key)) else { return Task::none() };
            let kind = ui.draft_kind;
            let face = FaceRef::new(key, d);
            let name = name_to(&all, kind, &ui.name);
            ui.note = Some(format!("Added a {}{}.", box_noun(kind), name_words(&all, &name)));
            ui.draft = None;
            ui.name.clear();
            ui.picked = None;
            send(app, LabelEdit::Add { face, kind, name });
            return unfocus();
        }
        LMsg::Add => {}
        LMsg::Submit => {
            let Some(b) = ui.picked_box().cloned() else { return Task::none() };
            let Some(to) = name_to(&all, b.kind, &ui.name) else { return Task::none() };
            if let NameTo::Subject(id) = &to {
                if b.subject.as_ref() == Some(id) {
                    ui.name.clear();
                    return Task::none();
                }
            }
            ui.note = Some(format!("Named this {}{}.", box_noun(b.kind), name_words(&all, &Some(to.clone()))));
            ui.name.clear();
            send(app, LabelEdit::Name { face: b.face, kind: b.kind, to });
            return unfocus();
        }
        LMsg::Pick(id) => {
            let Some(b) = ui.picked_box().cloned() else { return Task::none() };
            ui.name.clear();
            if b.subject.as_ref() == Some(&id) {
                return Task::none();
            }
            let to = NameTo::Subject(id);
            ui.note = Some(format!("Named this {}{}.", box_noun(b.kind), name_words(&all, &Some(to.clone()))));
            send(app, LabelEdit::Name { face: b.face, kind: b.kind, to });
            return unfocus();
        }
        LMsg::Unnamed => {
            let Some(b) = ui.picked_box().cloned() else { return Task::none() };
            if b.subject.is_none() {
                return Task::none();
            }
            ui.note = Some(format!("Moved this {} to Unnamed.", box_noun(b.kind)));
            send(app, LabelEdit::Name { face: b.face, kind: b.kind, to: NameTo::Unnamed });
        }
        LMsg::SwapKind => {
            let Some(b) = ui.picked_box().cloned() else { return Task::none() };
            let to = other(b.kind);
            let name = name_to(&all, to, &ui.name);
            ui.note = Some(format!("Changed a {} to a {}{}.", box_noun(b.kind), box_noun(to), name_words(&all, &name)));
            ui.picked = Some((b.face, to));
            ui.name.clear();
            send(app, LabelEdit::Kind { face: b.face, from: b.kind, to, name });
        }
        LMsg::Remove => {
            let Some(b) = ui.picked_box().cloned() else { return Task::none() };
            ui.note = Some(format!("Removed a {}. Ctrl+Z brings it back.", box_noun(b.kind)));
            ui.picked = None;
            ui.name.clear();
            send(app, LabelEdit::Remove { face: b.face, kind: b.kind });
            return unfocus();
        }
    }
    Task::none()
}

/// ", Someone" for a name, nothing for none.
fn name_words(all: &Subjects, name: &Option<NameTo>) -> String {
    match name {
        Some(NameTo::Subject(id)) => all.group(&GroupId::Subject(id.clone())).and_then(|g| g.name.clone()).map(|n| format!(" as {n}")).unwrap_or_default(),
        Some(NameTo::New(n)) => format!(" as {n}"),
        _ => String::new(),
    }
}

/// The stage reported a click on a box (or beside every box).
pub fn on_pick(app: &mut App, i: Option<usize>) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let ui = &mut v.labels;
    if !ui.on {
        return Task::none();
    }
    ui.draft = None;
    ui.name.clear();
    match i.and_then(|i| ui.boxes().get(i)).map(|b| (b.face, b.kind)) {
        Some(p) => {
            ui.picked = Some(p);
            focus_name()
        }
        None => {
            ui.picked = None;
            unfocus()
        }
    }
}

/// A box was drawn on the photo.
pub fn on_draw(app: &mut App, b: [f32; 4]) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let ui = &mut v.labels;
    // Only on a photo whose faces were looked for (a library photo).
    if !ui.on || ui.labels.as_ref().is_none_or(|l| l.key.is_none()) {
        return Task::none();
    }
    ui.picked = None;
    ui.name.clear();
    ui.draft = Some(b);
    focus_name()
}

/// Keys while the labels show.  `Some` when the key was theirs: Esc
/// steps back (a field, a drawn box, the labels), Enter takes the field,
/// and while a field is open every other plain key is the field's.
pub fn on_key(app: &mut App, key: &Key, ch: Option<&str>) -> Option<Task<Message>> {
    let v = app.viewer.as_ref()?;
    let ui = &v.labels;
    let shown = ui.on && !super::viewer_burst::on_sheet(v);
    let action = key_action(shown, ui.picked_index().is_some(), ui.draft.is_some(), ui.name.is_empty(), key, ch);
    match action {
        KeyAction::Pass => None,
        KeyAction::Swallow => Some(Task::none()),
        KeyAction::Cancel => Some(update(app, LMsg::Cancel)),
        KeyAction::Deselect => Some(update(app, LMsg::Deselect)),
        KeyAction::Hide => Some(toggle(app)),
        KeyAction::Submit => Some(update(app, LMsg::Submit)),
        KeyAction::Remove => Some(update(app, LMsg::Remove)),
    }
}

/// What a key does while the labels may show (pure, so it can be tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Not the labels' key: the viewer has it.
    Pass,
    /// The labels' (or the name field's): nothing else sees it.
    Swallow,
    Cancel,
    Deselect,
    Hide,
    Submit,
    /// Delete or Backspace on the picked box: "Not a face".
    Remove,
}

/// `shown`: the labels are up; `picked`, `draft`: a box picked, a box
/// drawn; `name_empty`: nothing typed in the name field.  While the
/// labels show, Delete and Backspace are theirs, always: they remove the
/// picked box (when nothing is typed: else they edit the name), and never
/// reach the viewer's prune key.
pub fn key_action(shown: bool, picked: bool, draft: bool, name_empty: bool, key: &Key, ch: Option<&str>) -> KeyAction {
    if !shown {
        return KeyAction::Pass;
    }
    let editing = picked || draft;
    // Keys a text field uses; the window's own (F11) still work.
    let typing = matches!(key, Key::Character(_))
        || matches!(key, Key::Named(Named::ArrowLeft | Named::ArrowRight | Named::ArrowUp | Named::ArrowDown | Named::Home | Named::End | Named::PageUp | Named::PageDown | Named::Space));
    match (key, ch) {
        (Key::Named(Named::Escape), _) if draft => KeyAction::Cancel,
        (Key::Named(Named::Escape), _) if picked => KeyAction::Deselect,
        (Key::Named(Named::Escape), _) => KeyAction::Hide,
        (Key::Named(Named::Delete | Named::Backspace), _) if picked && !draft && name_empty => KeyAction::Remove,
        (Key::Named(Named::Delete | Named::Backspace), _) => KeyAction::Swallow,
        (Key::Named(Named::Enter), _) if editing => KeyAction::Submit,
        (_, Some("l")) if !editing => KeyAction::Hide,
        _ if editing && typing => KeyAction::Swallow,
        _ => KeyAction::Pass,
    }
}

/// The × on the picked box: "Not a face" (or "Not a fursuit head").
pub fn on_remove_box(app: &mut App, i: usize) -> Task<Message> {
    if !pick_index(app, i) {
        return Task::none();
    }
    update(app, LMsg::Remove)
}

/// The × on a box's name tag: back to "Unnamed"; the box stays.
pub fn on_unname_box(app: &mut App, i: usize) -> Task<Message> {
    if !pick_index(app, i) {
        return Task::none();
    }
    update(app, LMsg::Unnamed)
}

/// Pick box `i` (no field focus): false when there is no such box.
fn pick_index(app: &mut App, i: usize) -> bool {
    let Some(v) = app.viewer.as_mut() else { return false };
    let ui = &mut v.labels;
    let Some(b) = ui.boxes().get(i).map(|b| (b.face, b.kind)) else { return false };
    ui.draft = None;
    ui.name.clear();
    ui.picked = Some(b);
    true
}

fn focus_name() -> Task<Message> {
    iced::widget::operation::focus::<Message>(NAME_FIELD)
}

fn unfocus() -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus::<()>()).discard()
}

// ── the bar ─────────────────────────────────────────────────────────────────

/// "2 faces and a fursuit head".
fn counts(boxes: &[LabelBox]) -> String {
    let n = |k: Kind| boxes.iter().filter(|b| b.kind == k).count();
    let part = |c: usize, one: &str, many: &str| match c {
        0 => None,
        1 => Some(format!("a {one}")),
        c => Some(format!("{c} {many}")),
    };
    let parts: Vec<String> = [part(n(Kind::Person), "face", "faces"), part(n(Kind::Fursuit), "fursuit head", "fursuit heads")].into_iter().flatten().collect();
    match parts.as_slice() {
        [] => "No faces or fursuit heads found".to_string(),
        [a] => format!("{}{} found", a[..1].to_uppercase(), &a[1..]),
        [a, b] => format!("{}{} and {b} found", a[..1].to_uppercase(), &a[1..]),
        _ => unreachable!(),
    }
}

/// Why drawn boxes wait, in words.
fn pending_words(why: PendingWhy) -> &'static str {
    match why {
        PendingWhy::Matching => "Matching the boxes you drew on the graphics card…",
        PendingWhy::ProcessorChosen => "Boxes you draw are matched on the graphics card: they will be once it is chosen in Preferences.",
        PendingWhy::NoCard => "The graphics card isn't available: boxes you drew are kept and will be matched when it is.",
        PendingWhy::NoFaceModel => "Faces you draw are matched once the face model is downloaded (Preferences ▸ People).",
        PendingWhy::NoSearchModel => "Boxes you draw are matched once search's model is downloaded.",
    }
}

/// A small swatch of a kind's colour: the box's outline in miniature.
fn swatch<'a>(kind: Kind, named: bool) -> Element<'a, Message> {
    let c = color(kind);
    container(Space::new().width(10).height(10))
        .style(move |_t| container::Style { background: named.then_some(iced::Background::Color(c)), border: style::border(c, 2.0, 2.0), ..container::Style::default() })
        .into()
}

fn bar<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    column![container(content).padding(Padding::from([0.0, t::S3])).height(48).center_y(48).width(Length::Fill).style(style::fill(t::MOUNT)), kit::hairline()].into()
}

/// The bar under the top bar while the labels show.
pub fn view<'a>(app: &'a App, ui: &'a LabelsUi) -> Element<'a, Message> {
    let done = kit::tip(kit::btn("Done", style::secondary(), Some(lmsg(LMsg::Toggle))), "Hide the labels  (L or Esc)", true);
    let lead = icons::view(Glyph::Label, 16.0, t::INK2);
    let Some(labels) = &ui.labels else {
        return bar(row![lead, kit::secondary("Looking for this photo's faces and fursuits…"), Space::new().width(Length::Fill), done].spacing(t::S2).align_y(Alignment::Center).into());
    };
    if labels.key.is_none() {
        return bar(
            row![lead, kit::secondary("No faces or fursuits found yet: this photo isn't in your library."), Space::new().width(Length::Fill), done]
                .spacing(t::S2)
                .align_y(Alignment::Center)
                .into(),
        );
    }
    let all = &app.subjects.all;
    if let Some(d) = ui.draft {
        let _ = d;
        return bar(draft_row(all, ui));
    }
    if let Some(b) = ui.picked_box() {
        return bar(picked_row(all, ui, b));
    }
    // What happened last (or how to start), then why drawn boxes wait.
    let hint = match &ui.note {
        Some(n) => n.clone(),
        None => format!("{}.  Click a box to name it (Delete or its × removes it), or drag on the photo to add one the finder missed.", counts(&labels.boxes)),
    };
    let mut r = row![lead, kit::secondary(hint)].spacing(t::S2).align_y(Alignment::Center);
    if let Some(why) = labels.pending {
        r = r.push(icons::view(Glyph::Clock, 14.0, t::INK3)).push(kit::caption(pending_words(why)));
    } else if ui.note.is_some() {
        r = r.push(kit::caption(counts(&labels.boxes)));
    }
    bar(r.push(Space::new().width(Length::Fill)).push(done).into())
}

/// The names of `kind` offered for what is typed: those that start with
/// it first, then those that hold it; most photos first; the box's own
/// name left out.
fn offered<'a>(all: &'a Subjects, kind: Kind, typed: &str, own: Option<&SubjectId>) -> Vec<(&'a SubjectId, &'a str)> {
    let q = typed.trim().to_lowercase();
    let mut v: Vec<(u8, usize, &SubjectId, &str)> = all
        .named(kind)
        .filter_map(|g| {
            let GroupId::Subject(id) = &g.id else { return None };
            let name = g.name.as_deref()?;
            if Some(id) == own {
                return None;
            }
            let l = name.to_lowercase();
            let rank = if q.is_empty() || l.starts_with(&q) { 0 } else if l.contains(&q) { 1 } else { return None };
            Some((rank, g.photos, id, name))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.3.cmp(b.3)));
    let mut seen: HashMap<String, ()> = HashMap::new();
    v.into_iter().filter(|x| seen.insert(x.3.to_lowercase(), ()).is_none()).take(OFFERED).map(|x| (x.2, x.3)).collect()
}

fn name_field<'a>(ui: &'a LabelsUi, kind: Kind) -> Element<'a, Message> {
    let placeholder = if kind == Kind::Fursuit { "Name this fursuit" } else { "Name this person" };
    text_input(placeholder, &ui.name)
        .id(NAME_FIELD)
        .on_input(|s| lmsg(LMsg::Name(s)))
        .on_submit(lmsg(LMsg::Submit))
        .width(200)
        .size(t::T_BODY)
        .padding(Padding::from([5.0, 8.0]))
        .style(style::input())
        .into()
}

/// A chip for a name offered: click to give it.
fn name_chip<'a>(name: &'a str, msg: Message) -> Element<'a, Message> {
    button(container(text(name).size(t::T_SMALL).font(paint::MEDIUM)).center_y(Length::Fill))
        .height(t::CONTROL_SM)
        .padding(Padding::from([0.0, t::S2]))
        .style(style::secondary().radius(t::CONTROL_SM / 2.0).style())
        .on_press(msg)
        .into()
}

fn picked_row<'a>(all: &'a Subjects, ui: &'a LabelsUi, b: &'a LabelBox) -> Element<'a, Message> {
    let who = match &b.name {
        Some(n) => n.clone(),
        None => "Unnamed".into(),
    };
    let mut r = row![swatch(b.kind, b.name.is_some()), text(format!("{}: {who}", noun_cap(b.kind))).size(t::T_BODY).font(paint::SEMIBOLD).color(t::INK), name_field(ui, b.kind)]
        .spacing(t::S2)
        .align_y(Alignment::Center);
    for (id, name) in offered(all, b.kind, &ui.name, b.subject.as_ref()) {
        r = r.push(name_chip(name, lmsg(LMsg::Pick(id.clone()))));
    }
    r = r.push(Space::new().width(Length::Fill));
    if b.pending {
        let waiting = row![icons::view(Glyph::Clock, 14.0, t::INK3), kit::caption("Not matched yet")].spacing(t::S1).align_y(Alignment::Center);
        r = r.push(kit::tip(waiting, "Drawn, and not matched yet: it is kept, and matched on the graphics card when it is available", true));
    }
    if b.name.is_some() {
        r = r.push(kit::tip(kit::btn("Unnamed", style::ghost(), Some(lmsg(LMsg::Unnamed))), format!("Take this {} out of {who}; it won't be suggested for them again  (or the × on its name)", box_noun(b.kind)), true));
    }
    let swap = if b.kind == Kind::Fursuit { "It's a person" } else { "It's a fursuit" };
    r = r.push(kit::tip(
        kit::btn(swap, style::ghost(), Some(lmsg(LMsg::SwapKind))),
        format!("This is a {}, not a {}: it moves to {} and leaves {}", box_noun(other(b.kind)), box_noun(b.kind), other(b.kind).plural(), b.kind.plural()),
        true,
    ));
    r = r.push(kit::tip(
        kit::btn(format!("Not a {}", box_noun(b.kind)), style::secondary(), Some(lmsg(LMsg::Remove))),
        format!("Remove this box: it isn't a {}, and won't come back when the library is looked at again  (Delete)", box_noun(b.kind)),
        true,
    ));
    r = r.push(kit::tip(kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(lmsg(LMsg::Deselect))), "Done with this box  (Esc)", true));
    r.into()
}

fn draft_row<'a>(all: &'a Subjects, ui: &'a LabelsUi) -> Element<'a, Message> {
    let kinds = super::people::KINDS
        .iter()
        .map(|&k| kit::segment(if k == Kind::Fursuit { "Fursuit head" } else { "Face" }, ui.draft_kind == k, None, lmsg(LMsg::DraftKind(k))))
        .collect();
    let mut r = row![swatch(ui.draft_kind, false), text("New box").size(t::T_BODY).font(paint::SEMIBOLD).color(t::INK), kit::segmented(kinds), name_field(ui, ui.draft_kind)]
        .spacing(t::S2)
        .align_y(Alignment::Center);
    for (id, name) in offered(all, ui.draft_kind, &ui.name, None) {
        r = r.push(name_chip(name, lmsg(LMsg::Name(name.to_string()))));
        let _ = id;
    }
    r = r.push(Space::new().width(Length::Fill));
    r = r.push(kit::caption("The name is optional"));
    r = r.push(kit::tip(kit::btn("Cancel", style::secondary(), Some(lmsg(LMsg::Cancel))), "Forget this box  (Esc)", true));
    r = r.push(kit::tip(kit::btn("Add", style::primary(), Some(lmsg(LMsg::Add))), "Keep this box  (Enter)", true));
    r.into()
}

// ── the test driver ─────────────────────────────────────────────────────────

/// What clicking the `n`th name offered under the field sends (the test
/// driver's `take`): for a picked box the name; for a drawn one, the
/// name into the field.
pub fn offered_at(app: &App, n: usize) -> Option<LMsg> {
    let ui = &app.viewer.as_ref()?.labels;
    let kind = match (ui.draft, ui.picked_box()) {
        (Some(_), _) => ui.draft_kind,
        (None, Some(b)) => b.kind,
        _ => return None,
    };
    let own = ui.picked_box().and_then(|b| b.subject.as_ref());
    let (id, name) = offered(&app.subjects.all, kind, &ui.name, own).get(n).map(|x| (x.0.clone(), x.1.to_string()))?;
    Some(if ui.draft.is_some() { LMsg::Name(name) } else { LMsg::Pick(id) })
}

/// The overlay's state, for the test driver's dumps.
pub fn debug_state(app: &App) -> String {
    let Some(v) = app.viewer.as_ref() else { return "no viewer".into() };
    let ui = &v.labels;
    let boxes: Vec<String> = ui
        .boxes()
        .iter()
        .map(|b| format!("{}:{}{}{} [{:.3} {:.3} {:.3} {:.3}]", b.kind.noun(), b.name.as_deref().unwrap_or("Unnamed"), if b.drawn { " drawn" } else { "" }, if b.pending { " pending" } else { "" }, b.face.bbox[0], b.face.bbox[1], b.face.bbox[2], b.face.bbox[3]))
        .collect();
    format!(
        "on={} key={} boxes=[{}] picked={:?} draft={:?} kind={} name={:?} note={:?} pending={:?}",
        ui.on,
        ui.labels.as_ref().map_or("waiting".to_string(), |l| l.key.map_or("none".into(), |k| k.to_string()[..8].to_string())),
        boxes.join(", "),
        ui.picked_index(),
        ui.draft,
        ui.draft_kind.noun(),
        ui.name,
        ui.note,
        ui.labels.as_ref().and_then(|l| l.pending),
    )
}

// ── the stand-in ────────────────────────────────────────────────────────────

/// The viewer's labels as the stand-in keeps them: the made-up boxes of
/// a photo, changed by the bar's calls, with an undo of their own.
/// Names given here don't make groups on the made-up pages.
#[derive(Debug, Default)]
pub struct FakeLabels {
    /// Photos changed: their boxes now.
    photos: HashMap<ContentKey, Vec<LabelBox>>,
    undo: Vec<(ContentKey, Option<Vec<LabelBox>>, String)>,
    next: u64,
}

impl FakeLabels {
    /// A photo's labels: as changed, else the made-up ones.
    pub fn labels(&self, key: ContentKey, base: Vec<LabelBox>, pending: Option<PendingWhy>) -> PhotoLabels {
        let boxes = self.photos.get(&key).cloned().unwrap_or(base);
        let pending = boxes.iter().any(|b| b.pending).then_some(pending.unwrap_or(PendingWhy::Matching));
        PhotoLabels { key: Some(key), boxes, pending }
    }

    /// A change, as the engine makes it (same words), refused across
    /// kinds.
    pub fn edit(&mut self, edit: &LabelEdit, base: Vec<LabelBox>, all: &Subjects, pending: bool) -> Result<String, String> {
        let key = edit.face().key;
        let before = self.photos.get(&key).cloned();
        let mut boxes = before.clone().unwrap_or(base);
        let mut next = self.next;
        let mut name_of = |kind: Kind, to: &NameTo| -> Result<(Option<SubjectId>, Option<String>), String> {
            match to {
                NameTo::Subject(id) => {
                    let g = all.group(&GroupId::Subject(id.clone())).ok_or("no such subject")?;
                    if g.kind != kind {
                        return Err("people, fursuits and pets are never combined".into());
                    }
                    Ok((Some(id.clone()), g.name.clone()))
                }
                NameTo::New(n) => match super::people::by_name(all, kind, n) {
                    Some(g) => Ok((match &g.id {
                        GroupId::Subject(s) => Some(s.clone()),
                        GroupId::Cluster(_) => None,
                    }, g.name.clone())),
                    None => {
                        next += 1;
                        Ok((Some(SubjectId(format!("label{next:04}"))), Some(n.clone())))
                    }
                },
                NameTo::Unnamed => Ok((None, None)),
            }
        };
        let find = |boxes: &[LabelBox], face: &FaceRef, kind: Kind| boxes.iter().position(|b| b.face == *face && b.kind == kind);
        let what = match edit {
            LabelEdit::Name { face, kind, to } => {
                let i = find(&boxes, face, *kind).ok_or("no such box")?;
                let (subject, name) = name_of(*kind, to)?;
                boxes[i].subject = subject;
                boxes[i].name = name.clone();
                match name {
                    Some(n) => format!("Named a {} {n}", box_noun(*kind)),
                    None => format!("Moved a {} to Unnamed", box_noun(*kind)),
                }
            }
            LabelEdit::Kind { face, from, to, name } => {
                let i = find(&boxes, face, *from).ok_or("no such box")?;
                let (subject, n) = match name {
                    Some(t) => name_of(*to, t)?,
                    None => (None, None),
                };
                boxes[i] = LabelBox { face: *face, kind: *to, subject, name: n, drawn: true, pending };
                format!("Changed a {} to a {}", box_noun(*from), box_noun(*to))
            }
            LabelEdit::Add { face, kind, name } => {
                let (subject, n) = match name {
                    Some(t) => name_of(*kind, t)?,
                    None => (None, None),
                };
                let what = match &n {
                    Some(n) => format!("Added a {} for {n}", box_noun(*kind)),
                    None => format!("Added a {}", box_noun(*kind)),
                };
                boxes.push(LabelBox { face: *face, kind: *kind, subject, name: n, drawn: true, pending });
                what
            }
            LabelEdit::Remove { face, kind } => {
                let i = find(&boxes, face, *kind).ok_or("no such box")?;
                boxes.remove(i);
                format!("Removed a {}", box_noun(*kind))
            }
        };
        self.next = next;
        self.photos.insert(key, boxes);
        self.undo.push((key, before, what.clone()));
        Ok(what)
    }

    /// Take the last change back.
    pub fn undo(&mut self) -> Option<String> {
        let (key, before, what) = self.undo.pop()?;
        match before {
            Some(b) => self.photos.insert(key, b),
            None => self.photos.remove(&key),
        };
        Some(what)
    }

    /// *Forget all* of a kind: its changed boxes go back.
    pub fn forget_kind(&mut self, kind: Kind) {
        for boxes in self.photos.values_mut() {
            boxes.retain(|b| b.kind != kind || !b.drawn);
            for b in boxes.iter_mut().filter(|b| b.kind == kind) {
                b.subject = None;
                b.name = None;
            }
        }
        self.undo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(n: u8, kind: Kind, name: Option<&str>) -> LabelBox {
        LabelBox { face: FaceRef::new(ContentKey([1; 16]), [0.1 * n as f32, 0.1, 0.1, 0.1]), kind, subject: None, name: name.map(String::from), drawn: false, pending: false }
    }

    /// Delete (or Backspace) on the picked box removes it; while the
    /// labels show it never reaches the viewer (so it never prunes),
    /// picked or not; with a name typed it edits the name; with the labels
    /// hidden it is the viewer's again (prune).
    #[test]
    fn delete_removes_the_picked_box_and_never_prunes_while_labels_show() {
        let del = Key::Named(Named::Delete);
        let back = Key::Named(Named::Backspace);
        assert_eq!(key_action(true, true, false, true, &del, None), KeyAction::Remove);
        assert_eq!(key_action(true, true, false, true, &back, None), KeyAction::Remove);
        assert_eq!(key_action(true, false, false, true, &del, None), KeyAction::Swallow, "nothing picked: kept from the prune key");
        assert_eq!(key_action(true, true, false, false, &back, None), KeyAction::Swallow, "a name typed: the field's");
        assert_eq!(key_action(true, false, true, true, &del, None), KeyAction::Swallow, "a box being drawn: nothing removed");
        assert_eq!(key_action(false, false, false, true, &del, None), KeyAction::Pass, "labels hidden: the viewer prunes as ever");
        // The other keys keep their places.
        let esc = Key::Named(Named::Escape);
        assert_eq!(key_action(true, true, false, true, &esc, None), KeyAction::Deselect);
        assert_eq!(key_action(true, false, false, true, &esc, None), KeyAction::Hide);
        assert_eq!(key_action(true, false, false, true, &Key::Character("p".into()), Some("p")), KeyAction::Pass, "P still prunes in a culling viewer");
        assert_eq!(key_action(true, true, false, true, &Key::Character("p".into()), Some("p")), KeyAction::Swallow, "typed into the name");
        assert_eq!(key_action(true, false, false, true, &Key::Character("l".into()), Some("l")), KeyAction::Hide);
    }

    /// The tag's × (or the bar's Unnamed) clears the name and keeps the
    /// box, one undo step.
    #[test]
    fn unnamed_clears_the_name_and_keeps_the_box() {
        use crate::model::subjects::{Face, Group};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let cover = Face { face: FaceRef::new(ContentKey([9; 16]), [0.0; 4]), jpg: None, raw: None, day, capture: day.and_hms_opt(1, 0, 0).unwrap(), burst: None, clarity: 1.0, confirmed: true };
        let id = SubjectId("p".into());
        let all = Subjects { groups: vec![Group { id: GroupId::Subject(id.clone()), kind: Kind::Person, name: Some("Someone".into()), hidden: false, photos: 3, bursts: 3, days: 1, cover, suggestions: 0 }], ..Subjects::default() };
        let key = ContentKey([1; 16]);
        let mut named = bx(1, Kind::Person, Some("Someone"));
        named.subject = Some(id);
        let base = vec![named.clone()];
        let mut f = FakeLabels::default();
        f.edit(&LabelEdit::Name { face: named.face, kind: Kind::Person, to: NameTo::Unnamed }, base.clone(), &all, false).unwrap();
        let l = f.labels(key, base.clone(), None);
        assert_eq!((l.boxes.len(), l.boxes[0].name.clone(), l.boxes[0].subject.clone()), (1, None, None), "the box stays, unnamed");
        assert_eq!(f.undo().as_deref(), Some("Moved a face to Unnamed"));
        assert_eq!(f.labels(key, base, None).boxes[0].name.as_deref(), Some("Someone"));
    }

    #[test]
    fn counts_read_as_a_sentence() {
        assert_eq!(counts(&[]), "No faces or fursuit heads found");
        assert_eq!(counts(&[bx(1, Kind::Person, None)]), "A face found");
        assert_eq!(counts(&[bx(1, Kind::Person, None), bx(2, Kind::Person, None), bx(3, Kind::Fursuit, None)]), "2 faces and a fursuit head found");
        assert_eq!(counts(&[bx(3, Kind::Fursuit, None), bx(4, Kind::Fursuit, None)]), "2 fursuit heads found");
    }

    /// The stand-in's labels: each change one undo step, the kinds kept
    /// apart (a fursuit's name never goes on a face).
    #[test]
    fn the_stand_in_changes_names_kinds_and_boxes_and_undoes_them() {
        use crate::model::subjects::{Face, Group, Subjects};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let cover = Face { face: FaceRef::new(ContentKey([9; 16]), [0.0; 4]), jpg: None, raw: None, day, capture: day.and_hms_opt(1, 0, 0).unwrap(), burst: None, clarity: 1.0, confirmed: true };
        let suit = SubjectId("suit".into());
        let all = Subjects { groups: vec![Group { id: GroupId::Subject(suit.clone()), kind: Kind::Fursuit, name: Some("Suit".into()), hidden: false, photos: 3, bursts: 3, days: 1, cover, suggestions: 0 }], ..Subjects::default() };
        let key = ContentKey([1; 16]);
        let base = vec![bx(1, Kind::Person, None), bx(3, Kind::Fursuit, None)];
        let mut f = FakeLabels::default();
        let face = base[0].face;
        assert!(f.edit(&LabelEdit::Name { face, kind: Kind::Person, to: NameTo::Subject(suit.clone()) }, base.clone(), &all, false).is_err(), "a fursuit's name on a face");
        f.edit(&LabelEdit::Name { face, kind: Kind::Person, to: NameTo::New("Someone".into()) }, base.clone(), &all, false).unwrap();
        f.edit(&LabelEdit::Kind { face, from: Kind::Person, to: Kind::Fursuit, name: Some(NameTo::Subject(suit.clone())) }, base.clone(), &all, false).unwrap();
        let l = f.labels(key, base.clone(), None);
        assert_eq!((l.boxes[0].kind, l.boxes[0].name.as_deref()), (Kind::Fursuit, Some("Suit")));
        f.edit(&LabelEdit::Add { face: FaceRef::new(key, [0.6, 0.6, 0.1, 0.1]), kind: Kind::Person, name: None }, base.clone(), &all, true).unwrap();
        assert_eq!(f.labels(key, base.clone(), Some(PendingWhy::NoCard)).pending, Some(PendingWhy::NoCard));
        f.edit(&LabelEdit::Remove { face: base[1].face, kind: Kind::Fursuit }, base.clone(), &all, false).unwrap();
        assert_eq!(f.labels(key, base.clone(), None).boxes.len(), 2);
        assert_eq!(f.undo().as_deref(), Some("Removed a fursuit head"));
        assert_eq!(f.undo().as_deref(), Some("Added a face"));
        assert_eq!(f.undo().as_deref(), Some("Changed a face to a fursuit head"));
        assert_eq!(f.undo().as_deref(), Some("Named a face Someone"));
        assert_eq!(f.labels(key, base.clone(), None).boxes, base);
        assert!(f.undo().is_none());
    }

    #[test]
    fn names_are_offered_from_the_boxs_own_kind_only() {
        use crate::model::subjects::{Face, Group};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 3, 12).unwrap();
        let cover = Face { face: FaceRef::new(ContentKey([9; 16]), [0.0; 4]), jpg: None, raw: None, day, capture: day.and_hms_opt(1, 0, 0).unwrap(), burst: None, clarity: 1.0, confirmed: true };
        let g = |id: &str, kind: Kind, name: &str, photos: usize| Group { id: GroupId::Subject(SubjectId(id.into())), kind, name: Some(name.into()), hidden: false, photos, bursts: photos, days: 1, cover: cover.clone(), suggestions: 0 };
        let all = Subjects { groups: vec![g("a", Kind::Person, "Sam", 3), g("b", Kind::Person, "Alex", 9), g("c", Kind::Fursuit, "Sammy", 40)], ..Subjects::default() };
        let names = |typed: &str, own: Option<&SubjectId>| offered(&all, Kind::Person, typed, own).into_iter().map(|x| x.1).collect::<Vec<_>>();
        assert_eq!(names("", None), ["Alex", "Sam"], "most photos first, people only");
        assert_eq!(names("sa", None), ["Sam"], "never the fursuit Sammy");
        assert_eq!(names("", Some(&SubjectId("b".into()))), ["Sam"], "not the box's own name");
        assert_eq!(names("le", None), ["Alex"], "a name that holds what is typed");
        assert!(matches!(name_to(&all, Kind::Person, " sam "), Some(NameTo::Subject(id)) if id.0 == "a"));
        assert!(matches!(name_to(&all, Kind::Fursuit, "Sam"), Some(NameTo::New(n)) if n == "Sam"), "a person's name is a new fursuit");
        assert_eq!(name_to(&all, Kind::Person, "  "), None);
    }
}
