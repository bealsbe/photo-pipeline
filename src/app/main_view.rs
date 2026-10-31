//! The main window: menu bar, side rail, folder bar, filter bar, the active
//! view and the status bar, plus overlays (open menu, date picker, dialogs).

use chrono::{Datelike, Local, NaiveDate};
use iced::keyboard::{Key, key::Named};
use iced::widget::{Space, button, column, container, mouse_area, opaque, pin, row, slider, space, stack, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Point, Task};

use super::commands::{Cmd, MENUS};
use super::kit::{self, tip};
use super::style::{self, Look};
use super::{App, FilterMsg, FolderTarget, Message, Show, View, calendar, dialogs, hints_ui, network, search, search_setup};
use crate::model::filter::{FilterState, SortKey};
use crate::model::library_index::{MONTH_ABBR, MONTH_NAME};
use crate::ui::grid::{Grid, GridData, Restack};
use crate::ui::icons::{self, Glyph};
use crate::ui::motion::{ActivityBar, Appear, SmoothBar};
use crate::ui::paint;
use crate::ui::side_menu::{Entry, Item, RAIL_W, SideMenu};
use crate::ui::theme::{self as t, alpha};
use crate::util;

const MENU_BAR_H: f32 = 30.0;
const MENU_PAD: f32 = 10.0;
/// Minimum dropdown width.
const MENU_W: f32 = 240.0;
const ITEM_H: f32 = 30.0;
const FOLDER_H: f32 = 44.0;
const FOLDER_COLLAPSED_H: f32 = 30.0;
const FILTER_H: f32 = 44.0;
const STATUS_H: f32 = 28.0;

// Fixed widths on the filter bar's right side, so the date popup can be
// anchored under its field.
pub(super) const BAR_PAD: f32 = 12.0;
pub(super) const GAP: f32 = 8.0;
const DATE_W: f32 = 108.0;
const DASH_W: f32 = 10.0;
const RESET_W: f32 = 60.0;
const SLIDER_W: f32 = 96.0;
const ZOOM_W: f32 = 14.0 + 6.0 + SLIDER_W + 6.0 + 14.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let keyboard_ok =
        app.modal.is_none() && app.menu.is_none() && app.date_popup.is_none() && app.bulk.is_none() && !app.quality.menu && app.search.dialog.is_none() && !app.search.steps;

    // ── content column ──────────────────────────────────────────────────
    let mut col = column![].width(Length::Fill).height(Length::Fill);
    if app.folder_bar_visible {
        col = col.push(folder_bar(app));
    }
    if let Some(b) = network::banner(app) {
        col = col.push(b);
    }
    if let Some(b) = hints_ui::banner(app) {
        col = col.push(b);
    }
    if let Some(b) = search_setup::banner(app) {
        col = col.push(b);
    }
    if let Some(b) = super::subjects_fursuits::ask_strip(app) {
        col = col.push(b);
    }
    if !app.view.is_library() {
        col = col.push(filter_bar(app));
    }
    if let Some(b) = hints_ui::copies_bar(app) {
        col = col.push(b);
    }
    let content: Element<'_, Message> = match app.view {
        View::Grid if folder_empty(app) => folder_empty_page(app),
        View::Grid if hints_ui::filtered_empty(app) => hints_ui::filtered_empty_page(app),
        View::Grid if pile_done(app) => pile_done_page(app),
        View::Grid if none_shown(app) => none_shown_page(app),
        View::Grid => {
            let data = GridData {
                sections: &app.derived.sections,
                coll: &app.coll,
                thumbs: &app.thumbs,
                selection: &app.grid.selection,
                collapsed: &app.grid.collapsed,
                select_mode: app.grid.select_mode,
                scale_factor: app.scale_factor,
                show_names: app.show_names,
                bursts: &app.derived.burst_runs,
                moments: &app.derived.moment_links,
                hints: (!app.quality.hints.is_empty()).then_some(&app.quality.hints),
                restack: Some(Restack {
                    seq: app.grid.restack_seq,
                    sections: &app.derived.before,
                    bursts: &app.derived.before_runs,
                    focus: app.derived.restack_focus,
                }),
                label: crate::ui::grid::CellLabel::Name,
                footer: None,
                links: &[],
            };
            Grid::new(data, app.thumb_size, Message::Grid)
                .keyboard(keyboard_ok && !app.side_expanded && app.quicklook.is_none())
                .initial_scroll(app.grid.scroll)
                .scroll_seq(app.grid.scroll_seq)
                .layout_key(app.derived.key)
                .into()
        }
        View::Calendar => calendar::view(app, keyboard_ok),
        View::Search => search::view(app, keyboard_ok),
        View::Subjects => super::subjects::view(app, keyboard_ok),
    };
    // Background work shows as a sweep along the content's top edge.
    let busy = app.scanning()
        || match app.view {
            View::Calendar => app.calendar.is_indexing() || app.calendar.thumbs.pending() > 0,
            // The first index runs along the top of the search page (an
            // export's is quiet: the status bar only).
            View::Search => {
                app.search.pending.is_some()
                    || app.search.pane.thumbs.pending() > 0
                    || app.search.status.pass.as_ref().is_some_and(|p| matches!(p.kind, crate::search::PassKind::FirstIndex | crate::search::PassKind::Rebuild) && app.search.status.stopped.is_none())
            }
            // Looking for faces, or a person's photos on their way.
            View::Subjects => {
                let st = &app.search.status;
                let faces_pass = st.stopped.is_none() && st.pass.as_ref().is_some_and(|p| p.kind == crate::search::PassKind::Faces || (st.faces.on && p.kind == crate::search::PassKind::FirstIndex));
                faces_pass || app.subjects.person.as_ref().is_some_and(|p| p.pending.is_some() || p.pane.thumbs.pending() > 0)
            }
            _ => app.thumbs.pending() > 0 || app.quality.analysing(),
        };
    // Quick look covers the grid (an empty layer keeps the stack's shape).
    let quick_look = super::quicklook::view(app).filter(|_| matches!(app.view, View::Grid | View::Search | View::Subjects)).unwrap_or_else(|| space().into());
    // The Hints menu opens over the content, under its control.
    let hints_menu = hints_ui::menu_layer(app).filter(|_| !app.view.is_library()).unwrap_or_else(|| space().into());
    col = col.push(stack![content, quick_look, ActivityBar::new(busy), hints_menu].width(Length::Fill).height(Length::Fill));

    let central = stack![
        row![Space::new().width(RAIL_W).height(Length::Fill), col],
        side_menu(app),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    // With the Plasma global menu showing our menus, the in-window bar goes.
    let mut base = column![].width(Length::Fill).height(Length::Fill);
    if !app.global_menu_on() {
        base = base.push(menu_bar(app)).push(kit::hairline());
    }
    let base = base.push(central).push(status_bar(app));
    let base = container(base).style(style::fill(t::GROUND)).width(Length::Fill).height(Length::Fill);

    let mut layers: Vec<Element<'_, Message>> = vec![base.into()];
    if let Some(m) = app.menu {
        layers.push(menu_catcher());
        layers.push(menu_dropdown(app, m));
    }
    if let Some(p) = &app.date_popup {
        layers.push(
            opaque(mouse_area(container(space()).width(Length::Fill).height(Length::Fill)).on_press(Message::Filter(FilterMsg::CloseDatePopup)))
                .into(),
        );
        layers.push(date_popup(app, p));
    }
    if let Some(modal) = &app.modal {
        layers.push(dialogs::view(app, modal));
    }
    if let Some(dialog) = app.main_id.and_then(|id| super::bulk::overlay(app, id)) {
        layers.push(dialog);
    }
    if let Some(dialog) = super::subjects::overlay(app) {
        layers.push(dialog);
    }
    if let Some(steps) = search_setup::steps_layer(app) {
        layers.push(steps);
    }
    if let Some(dialog) = search_setup::dialog(app) {
        layers.push(dialog);
    }
    if let Some(prompt) = network::quit_overlay(app) {
        layers.push(prompt);
    }
    stack(layers).width(Length::Fill).height(Length::Fill).into()
}

// ── menu bar ────────────────────────────────────────────────────────────────

fn title_width(title: &str) -> f32 {
    paint::text_width(title, t::T_BODY, paint::REGULAR) + 2.0 * MENU_PAD
}

fn menu_bar(app: &App) -> Element<'_, Message> {
    let mut r = row![].height(MENU_BAR_H).align_y(Alignment::Center).spacing(2);
    for (i, (title, _, _)) in MENUS.iter().enumerate() {
        let open = app.menu == Some(i);
        let b = button(container(text(*title).size(t::T_BODY)).center(Length::Fill))
            .height(MENU_BAR_H - 6.0)
            .width(title_width(title))
            .padding(0)
            .style(move |_t, status| {
                let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: (open || hot).then(|| Background::Color(if open { t::RAISED_HI } else { t::RAISED })),
                    text_color: if open || hot { t::INK } else { t::INK2 },
                    border: Border { radius: t::R_CONTROL.into(), ..Border::default() },
                    ..button::Style::default()
                }
            })
            .on_press(Message::MenuOpen(if open { None } else { Some(i) }));
        let el: Element<'_, Message> = if app.menu.is_some() && !open {
            mouse_area(b).on_enter(Message::MenuOpen(Some(i))).into()
        } else {
            b.into()
        };
        r = r.push(el);
    }
    container(r)
        .width(Length::Fill)
        .padding(Padding { left: 6.0, ..Padding::ZERO })
        .style(style::fill(t::MOUNT))
        .into()
}

fn menu_catcher<'a>() -> Element<'a, Message> {
    mouse_area(container(space()).width(Length::Fill).height(Length::Fill)).on_press(Message::MenuOpen(None)).into()
}

pub fn cmd_checked(app: &App, cmd: Cmd) -> Option<bool> {
    match cmd {
        Cmd::GridView => Some(app.view == View::Grid),
        Cmd::CalendarView => Some(app.view == View::Calendar),
        Cmd::Search => Some(app.view == View::Search),
        Cmd::Subjects => Some(app.view == View::Subjects),
        Cmd::FolderBar => Some(app.folder_bar_visible),
        Cmd::FileNames => Some(app.show_names),
        Cmd::SideMenu => Some(app.side_expanded),
        Cmd::SelectMode => Some(app.grid.select_mode),
        Cmd::ToReview => Some(app.filter.to_review),
        Cmd::QuickLook => Some(app.quicklook.is_some()),
        Cmd::AdvanceAfterMark => Some(app.settings.viewer_advance),
        Cmd::StackBursts if app.view == View::Search => Some(app.search.pane.all_stacked()),
        Cmd::StackBursts if app.view == View::Subjects => app.subjects.pane().map(|p| p.all_stacked()),
        Cmd::StackBursts => Some(app.stacking.all()),
        Cmd::ProblemsOnly => Some(app.filter.hint == Some(crate::model::hints::HintFilter::Problems)),
        _ => None,
    }
}

pub fn cmd_enabled(app: &App, cmd: Cmd) -> bool {
    if matches!(cmd, Cmd::Undo | Cmd::Redo) {
        return super::undo::cmd_enabled(app, cmd);
    }
    if cmd == Cmd::Compare {
        return super::compare::can_open(app);
    }
    if cmd == Cmd::ReviewBurst {
        return super::viewer_burst::can_open(app);
    }
    if cmd == Cmd::QuickLook {
        return match app.view {
            View::Grid => !app.derived.visible.is_empty(),
            View::Search => !app.search.pane.layout.tiles.is_empty(),
            View::Subjects => app.subjects.pane().is_some_and(|p| !p.layout.tiles.is_empty()),
            _ => false,
        };
    }
    if cmd == Cmd::StackBurst {
        return match app.view {
            View::Grid => app.focused_burst().is_some(),
            View::Search => app.search.pane.focused_burst(),
            View::Subjects => app.subjects.pane().is_some_and(|p| p.focused_burst()),
            _ => false,
        };
    }
    if cmd == Cmd::Analyse {
        return !app.scanning() && !app.coll.is_empty() && !app.quality.analysing();
    }
    let analysed = || !app.quality.analysing() && app.quality.hints.coverage().0 > 0;
    if matches!(cmd, Cmd::PruneSoft | Cmd::PruneAccidental | Cmd::PruneCopies | Cmd::ProblemsOnly) {
        return analysed() && !app.view.is_library();
    }
    if cmd == Cmd::KeepThisPruneRest {
        return analysed() && super::viewer_burst::can_open(app);
    }
    // Fursuit groups to compare, and no group editor to put away first.
    if cmd == Cmd::FursuitMerges {
        return super::subjects::has_groups(app, super::people::Kind::Fursuit) && !(app.view == View::Subjects && app.subjects.editor.is_some());
    }
    if cmd == Cmd::SelectMode && app.view == View::Subjects {
        return app.subjects.page() == super::subjects::Page::Overview && super::subjects::has_groups(app, app.subjects.tab);
    }
    !(app.view.is_library() && matches!(cmd, Cmd::SelectMode | Cmd::PruneSelected | Cmd::MarkSelected | Cmd::ToReview))
}

/// Wide enough for the longest label plus its shortcut, as Qt sizes menus.
fn menu_width(m: usize) -> f32 {
    let widest = MENUS[m]
        .2
        .iter()
        .flatten()
        .map(|cmd| {
            paint::text_width(cmd.label(), t::T_BODY, paint::REGULAR) + 32.0 + paint::text_width(cmd.shortcut(), t::T_SMALL, paint::REGULAR)
        })
        .fold(0.0, f32::max);
    // panel padding 4+4, item padding 8+8, check 16, gap 8, icon 16, gap 10
    (widest + 8.0 + 16.0 + 16.0 + 8.0 + 16.0 + 10.0).ceil().max(MENU_W)
}

fn menu_dropdown(app: &App, m: usize) -> Element<'_, Message> {
    let x: f32 = 6.0 + MENUS[..m].iter().map(|(t, _, _)| title_width(t) + 2.0).sum::<f32>();
    let mut items = column![].width(menu_width(m)).padding(4);
    let mut idx = 0usize;
    for entry in MENUS[m].2.iter() {
        match entry {
            None => {
                items = items.push(container(kit::hairline()).padding(Padding::from([4.0, 8.0])));
            }
            Some(cmd) => {
                let cmd = *cmd;
                let hovered = app.menu_hover == Some(idx);
                let enabled = cmd_enabled(app, cmd);
                let fg = if enabled { t::INK } else { t::INK4 };
                let check: Element<'_, Message> = match cmd_checked(app, cmd) {
                    Some(true) => icons::view(Glyph::Check, 14.0, if enabled { t::SIGNAL } else { t::INK4 }),
                    _ => Space::new().width(14).into(),
                };
                let icon: Element<'_, Message> = match cmd.icon() {
                    Some(g) => icons::view(g, 16.0, if enabled { t::INK2 } else { t::INK4 }),
                    None => Space::new().width(16).into(),
                };
                let line = row![
                    check,
                    Space::new().width(8),
                    icon,
                    Space::new().width(10),
                    text(cmd.label()).size(t::T_BODY).color(fg),
                    Space::new().width(Length::Fill),
                    text(cmd.shortcut()).size(t::T_SMALL).color(if enabled { t::INK3 } else { t::INK4 }),
                ]
                .align_y(Alignment::Center)
                .height(ITEM_H);
                let i = idx;
                let b = button(line)
                    .padding(Padding::from([0.0, 8.0]))
                    .width(Length::Fill)
                    .style(move |_t, status| {
                        let hot = enabled && (hovered || matches!(status, button::Status::Hovered | button::Status::Pressed));
                        button::Style {
                            background: hot.then(|| Background::Color(t::RAISED_HI)),
                            text_color: fg,
                            border: Border { radius: t::R_CONTROL.into(), ..Border::default() },
                            ..button::Style::default()
                        }
                    })
                    .on_press_maybe(enabled.then_some(Message::Cmd(cmd)));
                items = items.push(mouse_area(b).on_enter(Message::MenuHover(Some(i))));
                idx += 1;
            }
        }
    }
    let panel = Appear::new(container(items).style(style::floating())).rise(-6.0);
    pin(opaque(panel)).position(Point::new(x, MENU_BAR_H + 2.0)).width(Length::Fill).height(Length::Fill).into()
}

/// Keyboard handling while a menu is open.
pub fn menu_key(app: &mut App, m: usize, key: &Key) -> Task<Message> {
    let cmds: Vec<Cmd> = MENUS[m].2.iter().flatten().copied().collect();
    match key {
        Key::Named(Named::Escape) => {
            app.menu = None;
        }
        Key::Named(Named::ArrowDown) => {
            app.menu_hover = Some(app.menu_hover.map(|h| (h + 1) % cmds.len()).unwrap_or(0));
        }
        Key::Named(Named::ArrowUp) => {
            app.menu_hover = Some(app.menu_hover.map(|h| (h + cmds.len() - 1) % cmds.len()).unwrap_or(cmds.len() - 1));
        }
        Key::Named(Named::ArrowRight) => {
            app.menu = Some((m + 1) % MENUS.len());
            app.menu_hover = None;
        }
        Key::Named(Named::ArrowLeft) => {
            app.menu = Some((m + MENUS.len() - 1) % MENUS.len());
            app.menu_hover = None;
        }
        Key::Named(Named::Enter) | Key::Named(Named::Space) => {
            if let Some(cmd) = app.menu_hover.and_then(|h| cmds.get(h)).copied() {
                app.menu = None;
                if cmd_enabled(app, cmd) {
                    return app.update(Message::Cmd(cmd));
                }
            }
        }
        _ => {}
    }
    Task::none()
}

// ── side menu ───────────────────────────────────────────────────────────────

fn side_menu(app: &App) -> Element<'_, Message> {
    let mut entries = Vec::new();
    // The rail's commands, then a spacer and those pinned to its foot.
    let spacer = super::commands::SIDE.len();
    for (i, e) in super::commands::SIDE.iter().chain(&[None]).chain(super::commands::SIDE_BOTTOM).enumerate() {
        match e {
            None if i == spacer => entries.push(Entry::Spacer),
            None => entries.push(Entry::Separator),
            // Preferences ▸ People can hide this button; the shortcut and the
            // View menu stay.  Nothing else moves: it sits between Search
            // and a separator.
            Some(Cmd::Subjects) if !app.settings.rail_subjects => {}
            Some(cmd) => {
                let hint = match cmd {
                    Cmd::PruneSelected => "P".to_string(),
                    Cmd::MarkSelected => "M".to_string(),
                    _ => cmd.shortcut().to_string(),
                };
                let tooltip = if hint.is_empty() { cmd.tooltip().to_string() } else { format!("{}  ({hint})", cmd.tooltip()) };
                entries.push(Entry::Item(Item {
                    cmd: *cmd,
                    icon: cmd.icon(),
                    label: cmd.short_label(),
                    hint,
                    tooltip,
                    enabled: cmd_enabled(app, *cmd),
                    checked: cmd_checked(app, *cmd).unwrap_or(false),
                }));
            }
        }
    }
    SideMenu::new(entries, app.side_expanded, Message::Cmd, Message::SideBurger, Message::SideCollapse).into()
}

// ── folder bar ──────────────────────────────────────────────────────────────

/// One folder field: icon, what it is, where it points, and a mark when
/// it's on a network share.  Clicking picks a new folder.
fn folder_slot<'a>(
    glyph: Glyph,
    caption: &'a str,
    path: Option<&std::path::Path>,
    on: Message,
    badge: Option<(Glyph, Color, String)>,
) -> Element<'a, Message> {
    let path_el: Element<'a, Message> = match path {
        Some(p) => text(util::elide_path(p, 60)).size(t::T_BODY).color(t::INK).into(),
        None => text("Choose a folder").size(t::T_BODY).color(t::SIGNAL).into(),
    };
    let mut inner = row![icons::view(glyph, 16.0, t::INK2), kit::caption(caption), path_el]
        .spacing(t::S2)
        .align_y(Alignment::Center);
    let note = badge.map(|(g, tone, note)| {
        inner = std::mem::replace(&mut inner, row![]).push(icons::view(g, 14.0, tone));
        note
    });
    let b = button(container(inner).center_y(Length::Fill))
        .padding(Padding::from([0.0, t::S3]))
        .height(t::CONTROL_H + 2.0)
        .width(Length::Fill)
        .style(|_t, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if hot { t::RAISED } else { t::GROUND })),
                text_color: t::INK,
                border: Border { color: if hot { t::LINE_HI } else { t::LINE }, width: 1.0, radius: t::R_CONTROL.into() },
                ..button::Style::default()
            }
        })
        .on_press(on);
    match (path, note) {
        (Some(p), Some(note)) => tip(b, format!("{}  —  click to change\n{note}", p.display()), true),
        (Some(p), None) => tip(b, format!("{}  —  click to change", p.display()), true),
        (None, _) => b.into(),
    }
}

fn collapse_button<'a>(collapsed: bool) -> Element<'a, Message> {
    let b = kit::icon_btn(
        if collapsed { Glyph::ChevronDown } else { Glyph::ChevronUp },
        t::INK3,
        style::ghost(),
        Some(Message::FolderBarCollapse),
    );
    tip(b, if collapsed { "Show folders  (Ctrl+B)" } else { "Hide folders  (Ctrl+B)" }, true)
}

fn folder_bar(app: &App) -> Element<'_, Message> {
    if app.folder_bar_collapsed {
        let summary = match &app.folder {
            Some(p) => util::file_name(p),
            None => "No working folder".to_string(),
        };
        let badge: Element<'_, Message> = match app.folder.as_deref().and_then(|p| network::badge(app, p, false)) {
            Some((g, tone, note)) => tip(icons::view(g, 12.0, tone), note, true),
            None => Space::new().width(0).into(),
        };
        let inner = row![
            icons::view(Glyph::Folder, 14.0, t::INK3),
            kit::caption(summary),
            badge,
            Space::new().width(Length::Fill),
            collapse_button(true),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)
        .padding(Padding { left: BAR_PAD, right: 4.0, top: 0.0, bottom: 0.0 });
        let bar = mouse_area(container(inner).height(FOLDER_COLLAPSED_H).center_y(FOLDER_COLLAPSED_H).width(Length::Fill).style(style::fill(t::MOUNT)))
            .on_press(Message::FolderBarCollapse);
        return column![bar, kit::hairline()].into();
    }
    let inner = row![
        folder_slot(
            Glyph::Folder,
            "Working folder",
            app.folder.as_deref(),
            Message::PickFolder(FolderTarget::Working),
            app.folder.as_deref().and_then(|p| network::badge(app, p, false)),
        ),
        folder_slot(
            Glyph::Library,
            "Library",
            app.library.as_deref(),
            Message::PickFolder(FolderTarget::Library),
            app.library.as_deref().and_then(|p| network::badge(app, p, true)),
        ),
        collapse_button(false),
    ]
    .spacing(t::S2)
    .align_y(Alignment::Center)
    .padding(Padding { left: BAR_PAD, right: 4.0, top: 0.0, bottom: 0.0 });
    column![container(inner).height(FOLDER_H).center_y(FOLDER_H).width(Length::Fill).style(style::fill(t::MOUNT)), kit::hairline()].into()
}

// ── filter bar ──────────────────────────────────────────────────────────────

pub fn fmt_filter_date(d: NaiveDate) -> String {
    format!("{} {}, {}", MONTH_ABBR[d.month0() as usize], d.day(), d.year())
}

/// A small round swatch in a filter's colour.
fn tone_dot<'a>(color: Color) -> Element<'a, Message> {
    container(Space::new().width(6).height(6))
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border { radius: 3.0.into(), ..Border::default() },
            ..Default::default()
        })
        .into()
}

/// The filter bar's Show choice: label, filter, colour (the dot and the
/// chosen segment's tint) and tooltip.
const SHOW_OPTIONS: [(&str, Show, Option<Color>, Option<&str>); 4] = [
    ("All", Show::All, None, None),
    ("To review", Show::ToReview, Some(t::SIGNAL), Some("Show only photos not yet pruned or marked  (Ctrl+U)")),
    ("Marked", Show::Marked, Some(t::KEEP), Some("Show only photos marked for export")),
    ("Pruned", Show::Pruned, Some(t::REJECT), Some("Show only photos marked to prune")),
];

/// The Show choice's width as [`show_choice`] lays it out (the Hints
/// control's menu opens under the control after it).
pub(super) fn show_choice_width() -> f32 {
    let label = |s: &str| paint::text_width(s, t::T_SMALL, paint::MEDIUM);
    let segments: f32 = SHOW_OPTIONS
        .iter()
        .map(|(l, _, tone, _)| 2.0 * (t::S3 - 2.0) + if tone.is_some() { 6.0 + 6.0 } else { 0.0 } + label(l))
        .sum();
    // kit::segmented: 2 px padding round, 2 px between segments.
    2.0 * 2.0 + segments + 2.0 * (SHOW_OPTIONS.len() as f32 - 1.0)
}

/// All · To review · Marked · Pruned.  One choice rather than toggles,
/// because "not yet pruned or marked" can't hold with either of the last
/// two.  The chosen filter takes its colour: green for marked, red for
/// pruned, as on the photos and in the status bar.
fn show_choice<'a>(show: Show) -> Element<'a, Message> {
    let items = SHOW_OPTIONS
        .into_iter()
        .map(|(label, choice, tone, hint)| {
            let active = show == choice;
            let mut content = row![].spacing(6).align_y(Alignment::Center);
            if let Some(tone) = tone {
                content = content.push(tone_dot(if active { tone } else { alpha(tone, 0.55) }));
            }
            content = content.push(text(label).size(t::T_SMALL).font(paint::MEDIUM));
            let look = match tone {
                Some(tone) if active => Look {
                    bg: alpha(tone, 0.16),
                    fg: tone,
                    hover_bg: alpha(tone, 0.24),
                    hover_fg: tone,
                    ..style::segment(true)
                },
                _ => style::segment(active),
            };
            let b = button(container(content).center_y(Length::Fill))
                .height(t::CONTROL_SM - 4.0)
                .padding(Padding::from([0.0, t::S3 - 2.0]))
                .style(look.radius(3.0).style())
                .on_press(Message::Filter(FilterMsg::Show(choice)));
            match hint {
                Some(h) => tip(b, h, true),
                None => b.into(),
            }
        })
        .collect();
    kit::segmented(items)
}

fn filter_bar(app: &App) -> Element<'_, Message> {
    let f = &app.filter;
    // Whether All and Marked keep pruned photos is a preference (Show
    // pruned photos); the Pruned choice shows them either way.
    let mut r = row![
        show_choice(f.show()),
        hints_ui::chip(app),
        Space::new().width(Length::Fill),
    ]
    .spacing(GAP)
    .align_y(Alignment::Center);

    let dir = if f.sort_asc { Glyph::ArrowUp } else { Glyph::ArrowDown };
    let sorts = [("Date", SortKey::Date), ("Name", SortKey::Name), ("Size", SortKey::Size)]
        .into_iter()
        .map(|(label, key)| {
            let active = f.sort_key == key;
            kit::segment(label, active, active.then_some(dir), Message::Filter(FilterMsg::Sort(key)))
        })
        .collect();
    r = r.push(tip(kit::segmented(sorts), "Sort by; click again to reverse", true));
    r = r.push(kit::vrule(20.0));

    let date_toggle = kit::icon_btn(
        Glyph::Calendar,
        if f.date_on { t::SIGNAL } else { t::INK2 },
        if f.date_on { style::toggle(true, t::SIGNAL) } else { style::ghost() },
        Some(Message::Filter(FilterMsg::ToggleDate)),
    );
    r = r.push(tip(date_toggle, "Filter by date range", true));
    if f.date_on {
        let field = |d: NaiveDate, from: bool| {
            let open = app.date_popup.as_ref().is_some_and(|p| p.from == from);
            button(container(text(fmt_filter_date(d)).size(t::T_SMALL)).center(Length::Fill))
                .width(DATE_W)
                .height(t::CONTROL_SM)
                .padding(0)
                .style(move |_t, status| {
                    let hot = open || matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: Some(Background::Color(t::GROUND)),
                        text_color: if hot { t::INK } else { t::INK2 },
                        border: Border { color: if open { t::SIGNAL } else if hot { t::INK3 } else { t::LINE_HI }, width: 1.0, radius: t::R_CONTROL.into() },
                        ..button::Style::default()
                    }
                })
                .on_press(Message::Filter(FilterMsg::OpenDate(from)))
        };
        r = r.push(field(f.date_from, true));
        r = r.push(container(kit::caption("–")).width(DASH_W).center_x(DASH_W));
        r = r.push(field(f.date_to, false));
        r = r.push(tip(
            kit::icon_btn(Glyph::Close, t::INK3, style::ghost(), Some(Message::Filter(FilterMsg::ClearDates))),
            "Clear date range",
            true,
        ));
    }
    let reset = button(container(text("Reset").size(t::T_SMALL).font(paint::MEDIUM)).center(Length::Fill))
        .width(RESET_W)
        .height(t::CONTROL_SM)
        .padding(0)
        .style(style::ghost().style())
        .on_press(Message::Filter(FilterMsg::ResetAll));
    r = r.push(tip(reset, "Clear every filter", true));
    r = r.push(kit::vrule(20.0));

    let zoom_enabled = app.view == View::Grid;
    // Disabled outside the grid, as in Qt: dimmed, and drags change nothing.
    let sl = slider(120.0f32..=480.0, app.thumb_size, move |v| {
        if zoom_enabled { Message::Filter(FilterMsg::Zoom(v)) } else { Message::Noop }
    })
    .step(20.0f32)
    .width(SLIDER_W)
    .style(style::zoom_slider(zoom_enabled));
    let tone = if zoom_enabled { t::INK3 } else { t::INK4 };
    let zoom = row![icons::view(Glyph::Grid, 12.0, tone), sl, icons::view(Glyph::Photos, 14.0, tone)]
        .spacing(6)
        .align_y(Alignment::Center)
        .width(ZOOM_W);
    let zoom: Element<'_, Message> = if zoom_enabled { tip(zoom, "Thumbnail size  (Ctrl+scroll)", true) } else { zoom.into() };
    r = r.push(zoom);

    let bar = container(r.padding(Padding::from([0.0, BAR_PAD])))
        .height(FILTER_H)
        .center_y(FILTER_H)
        .width(Length::Fill)
        .style(style::fill(t::MOUNT));
    column![bar, kit::hairline()].into()
}

/// The date picker popup, anchored under its field.
fn date_popup<'a>(app: &'a App, p: &'a super::DatePopup) -> Element<'a, Message> {
    let right = app.window_size_width() - BAR_PAD;
    let to_x = right - ZOOM_W - GAP - 1.0 - GAP - RESET_W - GAP - t::CONTROL_H - GAP - DATE_W;
    let x = if p.from { to_x - GAP - DASH_W - GAP - DATE_W } else { to_x };
    let folder_h = if app.folder_bar_visible { (if app.folder_bar_collapsed { FOLDER_COLLAPSED_H } else { FOLDER_H }) + 1.0 } else { 0.0 };
    let menu_h = if app.global_menu_on() { 0.0 } else { MENU_BAR_H + 1.0 };
    let y = menu_h + folder_h + FILTER_H - 6.0;
    let x = x.max(RAIL_W + 4.0).min(app.window_size_width() - 260.0);

    let first = crate::ui::calendar_stage::first_weekday();
    let weeks = crate::ui::calendar_stage::month_weeks(p.year, p.month, first);
    let current = if p.from { app.filter.date_from } else { app.filter.date_to };
    let today = Local::now().date_naive();
    let nav = |g: Glyph, delta: i32, label: &'static str| {
        tip(kit::icon_btn(g, t::INK2, style::ghost(), Some(Message::Filter(FilterMsg::DatePopupMonth(delta)))), label, false)
    };
    let header = row![
        nav(Glyph::ChevronLeft, -1, "Previous month"),
        text(format!("{} {}", MONTH_NAME[(p.month - 1) as usize], p.year))
            .size(t::T_BODY)
            .font(paint::SEMIBOLD)
            .color(t::INK)
            .width(Length::Fill)
            .center(),
        nav(Glyph::ChevronRight, 1, "Next month"),
    ]
    .align_y(Alignment::Center);
    let mut grid = column![header, Space::new().height(4)].spacing(2);
    let names = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    let mut head = row![].spacing(2);
    for i in 0..7 {
        head = head.push(kit::caption(names[(first as usize + i) % 7]).width(32).center());
    }
    grid = grid.push(head);
    for week in weeks {
        let mut r = row![].spacing(2);
        for d in week {
            let in_month = d.month() == p.month;
            let sel = d == current;
            let is_today = d == today;
            let fg = if sel {
                t::ON_SIGNAL
            } else if in_month {
                t::INK
            } else {
                t::INK4
            };
            r = r.push(
                button(container(text(d.day().to_string()).size(t::T_SMALL)).center(Length::Fill))
                    .width(32)
                    .height(28)
                    .padding(0)
                    .style(move |_t, status| {
                        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
                        button::Style {
                            background: if sel {
                                Some(Background::Color(t::SIGNAL))
                            } else if hot {
                                Some(Background::Color(t::RAISED_HI))
                            } else {
                                None
                            },
                            text_color: fg,
                            border: Border { color: if is_today && !sel { t::INK3 } else { Color::TRANSPARENT }, width: 1.0, radius: t::R_CONTROL.into() },
                            ..button::Style::default()
                        }
                    })
                    .on_press(Message::Filter(FilterMsg::PickDate(p.from, d))),
            );
        }
        grid = grid.push(r);
    }
    let panel = Appear::new(container(grid.padding(t::S2)).style(style::floating())).rise(-6.0);
    pin(opaque(panel)).position(Point::new(x, y)).width(Length::Fill).height(Length::Fill).into()
}

// ── status bar ──────────────────────────────────────────────────────────────

/// "12 RAW" — a figure and what it counts, optionally led by a coloured dot.
fn count<'a>(n: usize, label: &'a str, tone: Color, dot: bool) -> Element<'a, Message> {
    let mut r = row![].spacing(5).align_y(Alignment::Center);
    if dot {
        let c = if n > 0 { tone } else { t::INK4 };
        r = r.push(container(Space::new().width(6).height(6)).style(move |_| container::Style {
            background: Some(Background::Color(c)),
            border: Border { radius: 3.0.into(), ..Border::default() },
            ..Default::default()
        }));
    }
    r = r.push(text(util::thousands(n as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(if dot { t::INK } else { tone }));
    r = r.push(kit::caption(label));
    r.into()
}

fn status_bar(app: &App) -> Element<'_, Message> {
    let s = app.coll.stats();
    let counts = row![
        count(s.total, "files", t::INK, false),
        count(s.raw, "RAW", t::RAW, false),
        count(s.jpg, "JPG", t::JPG, false),
        count(s.paired / 2, if s.paired / 2 == 1 { "pair" } else { "pairs" }, t::INK2, false),
        kit::vrule(12.0),
        count(s.pruned, "pruned", t::REJECT, true),
        count(s.marked, "marked", t::KEEP, true),
    ]
    .spacing(t::S4)
    .align_y(Alignment::Center);
    let mut message = row![crate::ui::symbols::line(&app.status.text, t::T_SMALL, paint::REGULAR, t::INK2)].spacing(t::S2).align_y(Alignment::Center);
    if let Some((label, msg)) = &app.status.action {
        message = message.push(
            kit::link_btn(label.clone(), [2.0, 6.0], Some(msg.clone())),
        );
    }
    let mut r = row![message, Space::new().width(Length::Fill)].align_y(Alignment::Center).padding(Padding::from([0.0, BAR_PAD]));
    if let Some(slot) = hints_ui::status_slot(app) {
        r = r.push(slot);
    }
    if let Some(slot) = super::subjects_fursuits::status_slot(app) {
        r = r.push(slot);
    }
    if let Some(slot) = super::fursuit_judge::status_slot(app) {
        r = r.push(slot);
    }
    if let Some(slot) = search_setup::status_slot(app) {
        r = r.push(slot);
    }
    // The working folder's counts, once there is one (a row of zeros says
    // nothing).
    if s.total > 0 {
        r = r.push(counts);
    }
    if let Some(p) = review_progress(app) {
        r = r.push(Space::new().width(t::S3)).push(kit::vrule(12.0)).push(Space::new().width(t::S2)).push(p);
    }
    column![kit::hairline(), container(r).height(STATUS_H).center_y(STATUS_H).width(Length::Fill).style(style::fill(t::MOUNT))].into()
}

/// "To review" is showing and nothing is left in it.
fn pile_done(app: &App) -> bool {
    app.filter.to_review && app.derived.visible.is_empty() && !app.scanning() && !app.coll.is_empty()
}

/// The end of the pile: say so, rather than show an empty grid.
fn pile_done_page(app: &App) -> Element<'_, Message> {
    let left = app.derived.progress.to_review();
    // Shots left outside the date range: offer those rather than everything.
    let (title, body, action, hint) = if app.filter.date_on && left > 0 {
        (
            "Nothing to review in these dates",
            format!("{} outside them {} still to review.", util::plural(left as u64, "shot"), if left == 1 { "is" } else { "are" }),
            ("Show all dates", FilterMsg::ToggleDate),
            Some("Turn off the date filter"),
        )
    } else {
        (
            "Nothing left to review",
            "Every shot in this folder is pruned or marked.".to_string(),
            ("Show all photos", FilterMsg::Show(Show::All)),
            Some("Ctrl+U"),
        )
    };
    empty_page(app, Glyph::Check, title, body, action, hint)
}

/// Marked or Pruned is showing and nothing is in it.
fn none_shown(app: &App) -> bool {
    matches!(app.filter.show(), Show::Marked | Show::Pruned) && app.derived.visible.is_empty() && !app.scanning() && !app.coll.is_empty()
}

/// Nothing is marked, or pruned: say how photos get here.
fn none_shown_page(app: &App) -> Element<'_, Message> {
    let pruned = app.filter.show() == Show::Pruned;
    // Some would show without the date range: offer those.
    let undated = FilterState { date_from: None, date_to: None, ..app.filter.state() };
    let outside = app.filter.date_on && app.coll.iter().any(|r| undated.accept(r));
    let (glyph, what) = if pruned { (Glyph::Prune, "pruned") } else { (Glyph::Keep, "marked") };
    let (title, body, action, hint) = if outside {
        (
            format!("Nothing {what} in these dates"),
            format!("There are {what} photos outside them."),
            ("Show all dates", FilterMsg::ToggleDate),
            Some("Turn off the date filter"),
        )
    } else if pruned {
        (
            "Nothing pruned".to_string(),
            "Photos you prune with P show here until they go to the Trash.".to_string(),
            ("Show all photos", FilterMsg::Show(Show::All)),
            None,
        )
    } else {
        (
            "Nothing marked".to_string(),
            "Photos you mark for export with M show here.".to_string(),
            ("Show all photos", FilterMsg::Show(Show::All)),
            None,
        )
    };
    empty_page(app, glyph, title, body, action, hint)
}

/// No working folder yet, or the folder has no photos: say so, with the
/// way in, rather than show a bare grid.  Not while a scan is running.
fn folder_empty(app: &App) -> bool {
    app.coll.is_empty() && !app.scanning()
}

fn folder_empty_page(app: &App) -> Element<'_, Message> {
    let open = (Cmd::Open.label().to_string(), Message::Cmd(Cmd::Open));
    let hint = Some(Cmd::Open.shortcut());
    match &app.folder {
        None => empty_page_to(
            app,
            Glyph::FolderOpen,
            "No working folder",
            "Open a folder of photos to start culling, or pick a day in the library calendar.".to_string(),
            open,
            hint,
        ),
        Some(folder) => empty_page_to(
            app,
            Glyph::Folder,
            "No photos here",
            format!("{} has no RAW or JPG files in it, or in its folders.", util::file_name(folder)),
            open,
            hint,
        ),
    }
}

/// A filter that shows nothing: what's going on, and a way out.
fn empty_page<'a>(
    app: &App,
    glyph: Glyph,
    title: impl text::IntoFragment<'a>,
    body: String,
    action: (&'a str, FilterMsg),
    hint: Option<&'a str>,
) -> Element<'a, Message> {
    empty_page_to(app, glyph, title, body, (action.0.to_string(), Message::Filter(action.1)), hint)
}

/// [`empty_page`] with any message as its way out.
fn empty_page_to<'a>(
    app: &App,
    glyph: Glyph,
    title: impl text::IntoFragment<'a>,
    body: String,
    action: (String, Message),
    hint: Option<&'a str>,
) -> Element<'a, Message> {
    let mut col = column![].spacing(t::S2).align_x(Alignment::Center).max_width(460);
    col = col.push(Space::new().height(Length::FillPortion(3)));
    col = col.push(icons::view(glyph, 48.0, t::INK4));
    col = col.push(Space::new().height(t::S2));
    col = col.push(kit::title(title));
    col = col.push(text(body).size(t::T_BODY).color(t::INK2).align_x(iced::alignment::Horizontal::Center));
    col = col.push(Space::new().height(t::S2));
    let button = kit::btn(action.0, style::secondary(), Some(action.1));
    col = col.push(match hint {
        Some(h) => tip(button, h, true),
        None => button.into(),
    });
    col = col.push(Space::new().height(Length::FillPortion(4)));
    let ground = if app.view == View::Grid { t::PHOTO_GROUND } else { t::GROUND };
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(style::fill(ground)).into()
}

/// Width of the review progress bar in the status bar.
const PROGRESS_W: f32 = 64.0;

/// "212 of 740 shots reviewed" and a thin bar, for the working folder.  A
/// shot is a grid tile (a RAW+JPG pair counts once), so the figures can
/// differ from the file counts beside them.  Clicking switches To review.
fn review_progress(app: &App) -> Option<Element<'_, Message>> {
    let p = app.derived.progress;
    if app.view.is_library() || app.scanning() || p.shots == 0 {
        return None;
    }
    let shots = format!("shot{}", util::s(p.shots));
    let done = p.reviewed == p.shots;
    let words: Element<'_, Message> = if done {
        row![
            kit::caption("All"),
            text(util::thousands(p.shots as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
            kit::caption(format!("{shots} reviewed")),
        ]
        .spacing(4)
        .into()
    } else {
        row![
            text(util::thousands(p.reviewed as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
            kit::caption(format!("of {} {shots} reviewed", util::thousands(p.shots as u64))),
        ]
        .spacing(4)
        .into()
    };
    let bar = SmoothBar::new(p.fraction()).length(PROGRESS_W).colors(t::RAISED, t::FLOW);
    let content = row![words, bar].spacing(t::S2).align_y(Alignment::Center);
    let b = button(container(content).center_y(Length::Fill))
        .height(STATUS_H - 6.0)
        .padding(Padding::from([0.0, 6.0]))
        .style(style::ghost().style())
        .on_press(Message::Cmd(Cmd::ToReview));
    let left = p.to_review();
    let hint = if app.filter.to_review {
        "Show all photos  (Ctrl+U)".to_string()
    } else if left == 0 {
        "Every shot is pruned or marked".to_string()
    } else {
        format!("Show the {} still to review  (Ctrl+U)", util::plural(left as u64, "shot"))
    };
    Some(tip(b, hint, false))
}

impl App {
    pub fn window_size_width(&self) -> f32 {
        self.window_size.width
    }
}
