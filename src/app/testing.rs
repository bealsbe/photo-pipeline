//! A tiny scripted driver for verifying the UI headlessly-ish: steps run on
//! a timer, screenshots come from iced (only this app's windows).
//!
//! Enabled with `PP_SCRIPT=<file>`.  One command per line:
//!
//! ```text
//! wait 1500
//! open /path/to/folder        # scan a working folder
//! library /path/to/library
//! cmd GridView                # any commands::Cmd variant name
//! key ctrl+2                  # a key press in the main window
//! vkey right                  # a key press in the viewer window
//! ckey tab                    # a key press in the compare window
//! cfocus 2                    # click the 3rd pane of the compare view
//! sheet open 3                # click the 4th tile of the viewer's burst sheet
//! sheet pick 3                # Ctrl+click it (pick it to compare)
//! sheet back                  # the wheel zooming out past fit on a frame
//! sdump                       # print the viewer's burst sheet state
//! bursts                      # print the working folder's bursts
//! analyse [cached|cancel]     # start the quality analysis (or show the
//!                             # cache only, or stop it)
//! quality [all|plans]         # print its state, flagged frames, moments
//!                             # and duplicates (all: every shot; plans:
//!                             # what each bulk action would mark, keep-this
//!                             # on the grid's selected photo)
//! tiles                       # print the grid's tiles, stacks and selection
//! tab 3                       # click the burst tab on the 4th grid tile
//! day 0                       # click the grid's 1st day header (collapse it)
//! film 5                      # click the 6th photo in the viewer's filmstrip
//! vresize 1300 800            # resize the viewer window
//! vmax on|off                 # maximise the viewer window, or restore it
//! vmove 200 120               # move the viewer window (not on Wayland)
//! vpos                        # print where the viewer window is (X11)
//! vdump                       # print the viewer's state and its settings
//! click 3 [ctrl|shift]        # grid click on the 4th tile (a stack is one)
//! lclick 3                    # list click on the 4th row
//! activate 0                  # open the viewer on a grid tile
//! aspect 1:1                  # pick a crop ratio in the viewer
//! filter pruned|marked|all|date|date-from|date-close
//! filter show-all|to-review|show-marked   # the All · To review · Marked choice
//! progress                    # print review progress and the filter
//! dlg export-go               # press a dialog button (see `dialog_msg`)
//! gridscroll 20000 3000       # scroll the grid to y=20000 px over 3 s
//! zoom 240                    # set the grid's thumbnail size
//! calday 2026-07-24           # open a day in the calendar
//! calactivate 0               # open the viewer on a tile of the day page
//! ql close|activate|dump      # quick look: click outside / double-click
//!                             # the photo (the widget's events), or print it
//! caldump                     # print the calendar state
//! cmd Undo                    # also Redo (or key ctrl+z / vkey ctrl+z)
//! statusaction                # press the status bar's button (Undo)
//! marks                       # print each record's path, marks and crop
//! history                     # print the undo and redo stacks
//! sh ls -R /tmp/x             # run a shell command (set up a test's files)
//! shot main out.png           # screenshot: main | viewer | compare | shortcuts
//! cdump                       # print the compare view's state
//! netdump                     # print network-share state and unsaved saves
//! nettry                      # press the banner's Try again
//! netdismiss                  # close the banner
//! netquit keep|quit           # answer the quit prompt
//! rename /from /to            # move a folder (simulate a share going away)
//! prefs open|close|dump       # the Preferences window; dump prints its state
//! prefs advance               # press a control (see `pref_control`)
//! prefs thumbsize 240         # drag the thumbnail-size slider there
//! prefs confirm|cancel        # answer a Clear confirmation
//! prefs resize 540 640        # resize the Preferences window
//! prefs library /path         # as if the folder picker returned /path
//! prefs editcopy /path        #   (library / edit-copy destination)
//! pkey tab                    # a key press in the Preferences window
//! quit
//! ```

use std::time::{Duration, Instant};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Task, window};

use super::commands::Cmd;
use super::{App, Message};

pub fn load() -> Vec<String> {
    let Ok(path) = std::env::var("PP_SCRIPT") else { return Vec::new() };
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect())
        .unwrap_or_default()
}

fn parse_key(spec: &str) -> (Key, Modifiers) {
    let mut mods = Modifiers::empty();
    let parts: Vec<&str> = spec.split('+').collect();
    for m in &parts[..parts.len() - 1] {
        match m.to_lowercase().as_str() {
            "ctrl" => mods |= Modifiers::CTRL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            _ => {}
        }
    }
    let k = parts[parts.len() - 1];
    let key = match k.to_lowercase().as_str() {
        "left" => Key::Named(Named::ArrowLeft),
        "right" => Key::Named(Named::ArrowRight),
        "up" => Key::Named(Named::ArrowUp),
        "down" => Key::Named(Named::ArrowDown),
        "escape" | "esc" => Key::Named(Named::Escape),
        "enter" => Key::Named(Named::Enter),
        "space" => Key::Named(Named::Space),
        "home" => Key::Named(Named::Home),
        "end" => Key::Named(Named::End),
        "delete" => Key::Named(Named::Delete),
        "pageup" => Key::Named(Named::PageUp),
        "pagedown" => Key::Named(Named::PageDown),
        "f5" => Key::Named(Named::F5),
        "f11" => Key::Named(Named::F11),
        "backspace" => Key::Named(Named::Backspace),
        "tab" => Key::Named(Named::Tab),
        _ => Key::Character(k.into()),
    };
    (key, mods)
}

/// A scripted grid scroll: from `from` to `to` px over `ms`, a step per frame.
pub struct ScrollDrive {
    start: Instant,
    from: f32,
    to: f32,
    ms: f32,
}

/// One frame of a scripted scroll.
pub fn drive_scroll(app: &mut App) {
    let Some(d) = &app.script_scroll else { return };
    let k = (d.start.elapsed().as_secs_f32() * 1000.0 / d.ms).min(1.0);
    app.grid.scroll = d.from + (d.to - d.from) * k;
    app.grid.scroll_seq += 1;
    if k >= 1.0 {
        app.script_scroll = None;
    }
}

fn next(i: usize, delay_ms: u64) -> Task<Message> {
    Task::perform(tokio::time::sleep(Duration::from_millis(delay_ms)), move |_| Message::Script(i))
}

pub fn step(app: &mut App, i: usize) -> Task<Message> {
    let Some(line) = app.script.get(i).cloned() else { return Task::none() };
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim().to_string();
    eprintln!("[script] {line}");
    let t = match cmd {
        "wait" => return next(i + 1, arg.parse().unwrap_or(500)),
        "open" => app.start_scan(arg.into()),
        "library" => {
            app.set_library(arg.into());
            Task::none()
        }
        "cmd" => {
            let c = match arg.as_str() {
                "ListView" => Some(Cmd::ListView),
                "GridView" => Some(Cmd::GridView),
                "CalendarView" => Some(Cmd::CalendarView),
                "SelectMode" => Some(Cmd::SelectMode),
                "QuickLook" => Some(Cmd::QuickLook),
                "Review" => Some(Cmd::Review),
                "Sort" => Some(Cmd::Sort),
                "Export" => Some(Cmd::Export),
                "EditCopy" => Some(Cmd::EditCopy),
                "Shortcuts" => Some(Cmd::Shortcuts),
                "SideMenu" => Some(Cmd::SideMenu),
                "FolderBar" => Some(Cmd::FolderBar),
                "FileNames" => Some(Cmd::FileNames),
                "PruneSelected" => Some(Cmd::PruneSelected),
                "MarkSelected" => Some(Cmd::MarkSelected),
                "Undo" => Some(Cmd::Undo),
                "Redo" => Some(Cmd::Redo),
                "ToReview" => Some(Cmd::ToReview),
                "Compare" => Some(Cmd::Compare),
                "ReviewBurst" => Some(Cmd::ReviewBurst),
                "StackBurst" => Some(Cmd::StackBurst),
                "StackBursts" => Some(Cmd::StackBursts),
                "AdvanceAfterMark" => Some(Cmd::AdvanceAfterMark),
                _ => None,
            };
            match c {
                Some(c) => app.update(Message::Cmd(c)),
                None => Task::none(),
            }
        }
        "pkey" => {
            let (key, mods) = parse_key(&arg);
            match app.prefs.window {
                Some(id) => app.on_key_public(id, key, mods),
                None => Task::none(),
            }
        }
        "prefs" => prefs_step(app, &arg),
        "key" | "vkey" => {
            let (key, mods) = parse_key(&arg);
            let id = if cmd == "key" { app.main_id } else { app.viewer.as_ref().map(|v| v.id) };
            match id {
                Some(id) => app.on_key_public(id, key, mods),
                None => Task::none(),
            }
        }
        "ckey" => {
            let (key, mods) = parse_key(&arg);
            match app.compare.as_ref().map(|c| c.id()) {
                Some(id) => app.on_key_public(id, key, mods),
                None => Task::none(),
            }
        }
        "cfocus" => {
            let ev = crate::ui::compare_stage::CompareEvent::Focus(arg.parse().unwrap_or(0));
            app.update(Message::Compare(super::compare::Msg::Stage(ev)))
        }
        "lclick" => match app.derived.list_rows.get(arg.parse().unwrap_or(0)).map(|r| r.primary()) {
            Some(id) => app.update(Message::List(crate::ui::list::ListEvent::Click { id, modifiers: Modifiers::empty() })),
            None => Task::none(),
        },
        "sheet" => {
            use crate::ui::image_stage::{SheetEvent, StageEvent};
            let mut a = arg.split_whitespace();
            let what = a.next().unwrap_or("");
            let n: usize = a.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let ev = match what {
                "open" => Some(SheetEvent::Open(n)),
                "pick" => Some(SheetEvent::Pick(n)),
                "back" => Some(SheetEvent::Back),
                _ => None,
            };
            match ev {
                Some(ev) => app.update(Message::Viewer(super::viewer::Msg::Stage(StageEvent::Sheet(ev)))),
                None => Task::none(),
            }
        }
        "sdump" => {
            match app.viewer.as_ref() {
                Some(v) => eprintln!("[sdump] {}", super::viewer_burst::debug_state(app, v)),
                None => eprintln!("[sdump] no viewer (compare open: {})", app.compare.is_some()),
            }
            Task::none()
        }
        "tab" => match app.derived.tiles.get(arg.parse().unwrap_or(0)).copied() {
            Some(id) => app.update(Message::Grid(crate::ui::grid::GridEvent::ToggleBurst(id))),
            None => Task::none(),
        },
        "day" => match app.derived.sections.get(arg.parse().unwrap_or(0)).map(|s| s.date) {
            Some(d) => app.update(Message::Grid(crate::ui::grid::GridEvent::ToggleCollapse(d))),
            None => Task::none(),
        },
        "tiles" => {
            let name = |id: crate::model::collection::RecordId| app.coll.get(id).map(|r| r.stem()).unwrap_or_default();
            let mut stacks = Vec::new();
            for (sec, marks) in app.derived.sections.iter().zip(&app.derived.burst_runs) {
                for (k, (&id, m)) in sec.ids.iter().zip(marks).enumerate() {
                    if let Some(m) = m.filter(|m| m.stacked) {
                        stacks.push(format!("{k}:{} {}/{} kept {} pruned {}", name(id), m.shown, m.total, m.kept, m.pruned));
                    }
                }
            }
            let sel: Vec<String> = app.derived.tiles.iter().filter(|id| app.grid.selection.contains(id)).map(|&id| name(id)).collect();
            eprintln!(
                "[tiles] tiles={} visible={} days={} stacked_all={} stacks={} {:?} selection={sel:?} anchor={:?} scroll={}",
                app.derived.tiles.len(),
                app.derived.visible.len(),
                app.derived.days.iter().map(|d| d.photos).sum::<usize>(),
                app.stacking.all(),
                stacks.len(),
                stacks.iter().take(8).collect::<Vec<_>>(),
                app.grid.anchor.map(name),
                app.grid.scroll,
            );
            Task::none()
        }
        "bursts" => {
            let b = &app.bursts;
            let lens: Vec<usize> = (0..b.len() as u32).map(|i| b.frames(i).len()).collect();
            eprintln!("[bursts] {} bursts holding {} frames: {lens:?}", b.len(), b.frame_count());
            Task::none()
        }
        "analyse" => match arg.as_str() {
            "cached" => super::quality::start_cached(app),
            "cancel" => {
                super::quality::cancel(app);
                Task::none()
            }
            _ => super::quality::start(app),
        },
        "quality" if arg == "plans" => {
            quality_plans(app);
            Task::none()
        }
        "quality" => {
            quality_dump(app, arg == "all");
            Task::none()
        }
        "cdump" => {
            match app.compare.as_ref() {
                Some(c) => {
                    let marks: Vec<String> = c
                        .items
                        .iter()
                        .filter_map(|&id| app.coll.get(id))
                        .map(|r| format!("{}{}{}", r.filename(), if r.is_marked { " kept" } else { "" }, if r.is_pruned { " pruned" } else { "" }))
                        .collect();
                    eprintln!("[cdump] focus={} items={marks:?} viewer_open={} {}", c.focus, app.viewer.is_some(), c.debug_state());
                }
                None => eprintln!("[cdump] no compare; viewer_open={}", app.viewer.is_some()),
            }
            Task::none()
        }
        "click" | "activate" => {
            let mut a = arg.split_whitespace();
            let idx: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let mods = match a.next() {
                Some("ctrl") => Modifiers::CTRL,
                Some("shift") => Modifiers::SHIFT,
                _ => Modifiers::empty(),
            };
            match app.derived.tiles.get(idx).copied() {
                Some(id) if cmd == "click" => app.update(Message::Grid(crate::ui::grid::GridEvent::Click { id, modifiers: mods })),
                Some(id) => app.update(Message::Grid(crate::ui::grid::GridEvent::Activate(id))),
                None => Task::none(),
            }
        }
        "filter" => {
            let m = match arg.as_str() {
                "pruned" => Some(super::FilterMsg::TogglePruned),
                "marked" => Some(super::FilterMsg::ToggleMarkedOnly),
                "all" => Some(super::FilterMsg::ResetAll),
                "date" => Some(super::FilterMsg::ToggleDate),
                "date-from" => Some(super::FilterMsg::OpenDate(true)),
                "date-close" => Some(super::FilterMsg::CloseDatePopup),
                "show-all" => Some(super::FilterMsg::Show(super::Show::All)),
                "to-review" => Some(super::FilterMsg::Show(super::Show::ToReview)),
                "show-marked" => Some(super::FilterMsg::Show(super::Show::Marked)),
                _ => None,
            };
            match m {
                Some(m) => app.update(Message::Filter(m)),
                None => Task::none(),
            }
        }
        "dlg" => match dialog_msg(&arg) {
            Some(m) => app.update(Message::Dialog(m)),
            None => {
                eprintln!("[script] unknown dialog action {arg}");
                Task::none()
            }
        },
        "aspect" => match crate::model::crop::ratio_from_label(&arg) {
            Some(r) => app.update(Message::Viewer(super::viewer::Msg::Aspect(r))),
            None => Task::none(),
        },
        "shot" => {
            let mut a = arg.splitn(2, ' ');
            let which = a.next().unwrap_or("main");
            let path = a.next().unwrap_or("shot.png").to_string();
            let id = match which {
                "viewer" => app.viewer.as_ref().map(|v| v.id),
                "compare" => app.compare.as_ref().map(|c| c.id()),
                "shortcuts" => app.shortcuts,
                "prefs" => app.prefs.window,
                _ => app.main_id,
            };
            if let Some(id) = id {
                return window::screenshot(id).map(move |s| Message::ScriptShot(path.clone(), s)).chain(next(i + 1, 50));
            }
            Task::none()
        }
        "dump" => {
            eprintln!(
                "[dump] folder={:?} records={} sections={} visible={} rows={} status={:?} thumbs={} scanning={} view={:?} win={:?}",
                app.folder,
                app.coll.len(),
                app.derived.sections.len(),
                app.derived.visible.len(),
                app.derived.list_rows.len(),
                app.status.text,
                app.thumbs.len(),
                app.scanning,
                app.view,
                app.window_size_width(),
            );
            Task::none()
        }
        "calday" => match chrono::NaiveDate::parse_from_str(&arg, "%Y-%m-%d") {
            Ok(d) => {
                let ev = crate::ui::calendar_stage::StageEvent::DayActivated(d, iced::Rectangle::new(iced::Point::ORIGIN, iced::Size::new(100.0, 100.0)));
                app.update(Message::Calendar(super::calendar::Msg::Stage(ev)))
            }
            Err(_) => Task::none(),
        },
        "calactivate" => {
            let n: usize = arg.parse().unwrap_or(0);
            let id = app.calendar.day.and_then(|d| app.calendar.day_data(d)).and_then(|data| data.ids.get(n).copied());
            match id {
                Some(id) => app.update(Message::Calendar(super::calendar::Msg::Grid(crate::ui::grid::GridEvent::Activate(id)))),
                None => Task::none(),
            }
        }
        "gridscroll" => {
            let mut a = arg.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(0.0));
            let (to, ms) = (a.next().unwrap_or(0.0), a.next().unwrap_or(1000.0).max(1.0));
            app.script_scroll = Some(ScrollDrive { start: Instant::now(), from: app.grid.scroll, to, ms });
            Task::none()
        }
        "progress" => {
            let p = app.derived.progress;
            let sel: Vec<String> = app.grid.selection.iter().filter_map(|&id| app.coll.get(id)).map(|r| r.filename()).collect();
            eprintln!(
                "[progress] shots={} reviewed={} to_review={} show={:?} show_pruned={} tiles={} selection={:?}",
                p.shots,
                p.reviewed,
                p.to_review(),
                app.filter.show(),
                app.filter.show_pruned,
                app.derived.visible.len(),
                sel,
            );
            Task::none()
        }
        "ql" => match arg.as_str() {
            "close" | "activate" => {
                use crate::ui::quick_look::QuickLookEvent;
                let ev = if arg == "close" { QuickLookEvent::Close } else { QuickLookEvent::Activate };
                app.update(Message::QuickLook(super::quicklook::Msg::Widget(ev)))
            }
            _ => {
                eprintln!("[ql] {}", super::quicklook::debug_state(app));
                Task::none()
            }
        },
        "zoom" => app.update(Message::Filter(super::FilterMsg::Zoom(arg.parse().unwrap_or(180.0)))),
        "film" => {
            let i: usize = arg.parse().unwrap_or(0);
            app.update(Message::Viewer(super::viewer::Msg::Film(crate::ui::filmstrip::FilmEvent::Jump(i))))
        }
        "vresize" => {
            let mut a = arg.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(800.0));
            let size = iced::Size::new(a.next().unwrap_or(800.0), a.next().unwrap_or(600.0));
            // The viewer's window, also while the compare view has it.
            match app.viewer.as_ref().map(|v| v.id).or(app.compare.as_ref().map(|c| c.id())) {
                Some(id) => window::resize(id, size),
                None => Task::none(),
            }
        }
        "vmove" => {
            let mut a = arg.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(0.0));
            let at = iced::Point::new(a.next().unwrap_or(0.0), a.next().unwrap_or(0.0));
            match app.viewer.as_ref() {
                Some(v) => window::move_to(v.id, at),
                None => Task::none(),
            }
        }
        "vpos" => match app.viewer.as_ref() {
            Some(v) => window::position(v.id).map(|p| {
                eprintln!("[vpos] {p:?}");
                Message::Noop
            }),
            None => Task::none(),
        },
        "vmax" => match app.viewer.as_ref().map(|v| v.id).or(app.compare.as_ref().map(|c| c.id())) {
            Some(id) => window::maximize(id, arg != "off"),
            None => Task::none(),
        },
        "vdump" => {
            let s = &app.settings;
            match app.viewer.as_ref() {
                Some(v) => {
                    let rec = v.record(app);
                    eprintln!(
                        "[vdump] index={} of {} file={:?} pruned={:?} marked={:?} fullscreen={} advancing={}",
                        v.index,
                        v.items.len(),
                        rec.map(|r| r.filename()),
                        rec.map(|r| r.is_pruned),
                        rec.map(|r| r.is_marked),
                        v.win.fullscreen,
                        v.flow.waiting(),
                    );
                }
                None => eprintln!("[vdump] no viewer"),
            }
            eprintln!(
                "[vdump] settings advance={} filmstrip={} size={}x{} pos={:?},{:?} maximized={}",
                s.viewer_advance, s.viewer_filmstrip, s.viewer_width, s.viewer_height, s.viewer_x, s.viewer_y, s.viewer_maximized
            );
            Task::none()
        }
        "caldump" => {
            eprintln!("[caldump] {}", app.calendar.debug_state());
            Task::none()
        }
        "statusaction" => match app.status.action.clone() {
            Some((label, msg)) => {
                eprintln!("[script] pressing status action {label:?}");
                app.update(msg)
            }
            None => {
                eprintln!("[script] no status action");
                Task::none()
            }
        },
        "marks" => {
            let root = app.folder.clone().unwrap_or_default();
            let mut rows: Vec<String> = app
                .coll
                .iter()
                .map(|r| {
                    let rel = r.path.strip_prefix(&root).unwrap_or(&r.path).display().to_string();
                    let crop = r.crop.as_ref().filter(|c| !c.is_full()).map(|c| format!(" crop={:.3},{:.3},{:.3},{:.3}", c.left, c.top, c.right, c.bottom)).unwrap_or_default();
                    format!("{rel}{}{}{crop}", if r.is_pruned { " PRUNED" } else { "" }, if r.is_marked { " MARKED" } else { "" })
                })
                .collect();
            rows.sort();
            eprintln!("[marks] {} records, status={:?}", rows.len(), app.status.text);
            for r in rows {
                eprintln!("[marks]   {r}");
            }
            Task::none()
        }
        "sh" => {
            match std::process::Command::new("sh").arg("-c").arg(&arg).output() {
                Ok(out) => {
                    for l in String::from_utf8_lossy(&out.stdout).lines().chain(String::from_utf8_lossy(&out.stderr).lines()) {
                        eprintln!("[sh] {l}");
                    }
                }
                Err(e) => eprintln!("[sh] failed: {e}"),
            }
            Task::none()
        }
        "history" => {
            use super::undo::Dir;
            eprintln!("[history] undo={:?} redo={:?}", app.history.labels(Dir::Undo), app.history.labels(Dir::Redo));
            Task::none()
        }
        "netdump" => {
            eprintln!("[netdump] {} status={:?}", app.net.debug_state(), app.status.text);
            Task::none()
        }
        "nettry" => app.update(Message::Net(super::network::Msg::TryAgain)),
        "netdismiss" => app.update(Message::Net(super::network::Msg::Dismiss)),
        "netquit" => app.update(Message::Net(if arg == "quit" { super::network::Msg::QuitAnyway } else { super::network::Msg::KeepOpen })),
        "rename" => {
            let mut a = arg.split_whitespace();
            if let (Some(from), Some(to)) = (a.next(), a.next()) {
                match std::fs::rename(from, to) {
                    Ok(()) => eprintln!("[script] renamed {from} -> {to}"),
                    Err(e) => eprintln!("[script] rename {from} -> {to} failed: {e}"),
                }
            }
            Task::none()
        }
        // Steps go on: quitting may stop to ask about unsaved marks.
        "quit" => app.update(Message::Cmd(Cmd::Quit)),
        _ => Task::none(),
    };
    Task::batch([t, next(i + 1, 50)])
}

/// The analysis' state and suggestions, for scripts.
fn quality_dump(app: &App, all: bool) {
    let q = &app.quality;
    let name = |id: crate::model::collection::RecordId| app.coll.get(id).map(|r| r.filename()).unwrap_or_default();
    eprintln!("[quality] running={} progress={:?} status={:?}", q.running(), q.progress, q.status_text());
    eprintln!("[quality] last={:?}", q.last);
    eprintln!("[quality] summary={:?} failed={}", q.summary(), q.failed.len());
    let h = &q.hints;
    for (m, moment) in h.moments().iter().enumerate() {
        let best = moment.ranking.first().map(|&id| name(id)).unwrap_or_default();
        eprintln!("[quality] moment {m}: {} frames in {} presses, sharpest {best}", moment.frames.len(), moment.presses.len());
    }
    let mut shown: Vec<(String, String)> = Vec::new();
    for (id, r) in app.coll.iter_ids() {
        if h.rep(id) != Some(id) {
            continue;
        }
        let Some(x) = h.get(id) else {
            if all {
                shown.push((r.filename(), "not measured".into()));
            }
            continue;
        };
        if all || x.problem() || x.sharpest {
            shown.push((
                r.filename(),
                format!(
                    "score={:.1} local={:.1} rel={:.2} soft={:?} accidental={:?} sharpest={} rank={:?} moment={:?} source={:?}",
                    x.score,
                    x.local,
                    x.relative,
                    x.soft,
                    x.accidental.map(|a| a.label()),
                    x.sharpest,
                    x.rank,
                    h.moment_of(id),
                    q.features(&r.path).map(|f| f.source),
                ),
            ));
        }
    }
    shown.sort();
    for (n, t) in shown {
        eprintln!("[quality]   {n} {t}");
    }
    for d in h.duplicates() {
        let prune: Vec<String> = d.prune.iter().map(|&id| name(id)).collect();
        eprintln!("[quality] duplicate {:?}: keep {} prune {prune:?}", d.kind, name(d.keep));
    }
}

/// What each bulk action would mark (nothing is marked).
fn quality_plans(app: &App) {
    let h = &app.quality.hints;
    let names = |ids: &[crate::model::collection::RecordId]| -> Vec<String> { ids.iter().filter_map(|&id| app.coll.get(id)).map(|r| r.filename()).collect() };
    let all = app.coll.ids().to_vec();
    eprintln!("[quality] prune soft: {:?}", names(&h.prune_soft(&app.coll, &all).prune));
    eprintln!("[quality] prune accidental: {:?}", names(&h.prune_accidental(&app.coll, &all).prune));
    eprintln!("[quality] prune duplicates: {:?}", names(&h.prune_duplicates(&app.coll, &all).prune));
    for &id in app.derived.visible.iter().filter(|id| app.grid.selection.contains(id)).take(1) {
        let Some(m) = h.moment_of(id).and_then(|m| h.moment(m)) else {
            eprintln!("[quality] {} is in no moment", names(&[id]).join(""));
            continue;
        };
        let plan = h.keep_this_prune_rest(&app.coll, &m.frames, id);
        eprintln!("[quality] keep this: mark {:?} prune {:?}", names(&plan.mark), names(&plan.prune));
        eprintln!("[quality] review burst panes: {:?}", names(&h.ranked(&m.frames)[..m.frames.len().min(4)]));
        if let Some((g, prune)) = h.duplicate_of(id) {
            eprintln!("[quality] duplicate of {:?}, suggested for pruning: {prune}", names(&g.ids));
        }
    }
}

fn prefs_step(app: &mut App, arg: &str) -> Task<Message> {
    use super::preferences::{Control, Msg};
    use super::preferences_storage::Cache;
    let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
    let rest = rest.trim();
    let control = match what {
        "advance" => Some(Control::Advance),
        "names" => Some(Control::FileNames),
        "stack" => Some(Control::StackBursts),
        "filmstrip" => Some(Control::Filmstrip),
        "burstsheet" => Some(Control::BurstSheet),
        "viewersize" => Some(Control::ViewerSize),
        "librarybutton" => Some(Control::Library),
        "editcopybutton" => Some(Control::EditCopyFolder),
        "subfolder" => Some(Control::EditCopySubfolder),
        "globalmenu" => Some(Control::GlobalMenu),
        "clear-thumbs" => Some(Control::Clear(Cache::Thumbs)),
        "clear-library" => Some(Control::Clear(Cache::Library)),
        "clear-capture" => Some(Control::Clear(Cache::CaptureTimes)),
        "close-button" => Some(Control::Close),
        _ => None,
    };
    if let Some(c) = control {
        return app.update(Message::Prefs(Msg::Press(c)));
    }
    match what {
        "open" => app.update(Message::Cmd(Cmd::Preferences)),
        "close" => match app.prefs.window {
            Some(id) => app.update(Message::CloseRequested(id)),
            None => Task::none(),
        },
        "thumbsize" => {
            let t = app.update(Message::Prefs(Msg::ThumbSize(rest.parse().unwrap_or(180.0))));
            Task::batch([t, app.update(Message::Prefs(Msg::ThumbSizeDone))])
        }
        "resize" => {
            let mut a = rest.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(600.0));
            let size = iced::Size::new(a.next().unwrap_or(600.0), a.next().unwrap_or(600.0));
            match app.prefs.window {
                Some(id) => window::resize(id, size),
                None => Task::none(),
            }
        }
        "confirm" => app.update(Message::Prefs(Msg::ClearConfirmed)),
        "cancel" => app.update(Message::Prefs(Msg::ConfirmCancel)),
        "library" => app.update(Message::FolderPicked(super::FolderTarget::Library, Some(rest.into()))),
        "editcopy" => app.update(Message::FolderPicked(super::FolderTarget::EditCopySetting, Some(rest.into()))),
        "dump" => {
            let s = &app.settings;
            eprintln!("[prefs] {}", app.prefs.debug_state());
            eprintln!(
                "[prefs] settings advance={} names={} thumb={} filmstrip={} burst_sheet={} viewer={}x{} max={} library={:?} edit_copy={:?} subfolder={} global_menu={}",
                s.viewer_advance,
                s.show_file_names,
                s.thumb_size,
                s.viewer_filmstrip,
                s.viewer_burst_sheet,
                s.viewer_width,
                s.viewer_height,
                s.viewer_maximized,
                s.library_folder,
                s.edit_copy_folder,
                s.edit_copy_subfolder,
                s.global_menu,
            );
            eprintln!(
                "[prefs] app show_names={} thumb_size={} library={:?} global_menu_served={} global_menu_on={} unsaved={:?}",
                app.show_names,
                app.thumb_size,
                app.library,
                app.global_menu.is_some(),
                app.global_menu_on(),
                app.net.unsaved_folders(),
            );
            Task::none()
        }
        _ => {
            eprintln!("[script] unknown prefs action {arg}");
            Task::none()
        }
    }
}

fn dialog_msg(name: &str) -> Option<super::dialogs::Msg> {
    use super::dialogs::Msg;
    use crate::ops::library::{Mode, Resolution};
    use crate::ops::separate::Strategy;
    Some(match name {
        "close" => Msg::Close,
        "confirm" => Msg::ConfirmOk,
        "cancel" => Msg::ConfirmCancel,
        "export-copy" => Msg::ExMode(Mode::Copy),
        "export-move" => Msg::ExMode(Mode::Move),
        "export-rename-off" => Msg::ExRename(false),
        "export-rename-on" => Msg::ExRename(true),
        "export-pruned" => Msg::ExPruned(true),
        "export-all" => Msg::ExMarkedOnly(false),
        "export-skip" => Msg::ExBulk(Resolution::Skip),
        "export-overwrite" => Msg::ExBulk(Resolution::Overwrite),
        "export-go" => Msg::ExGo,
        "copy-go" => Msg::EcGo,
        "sort-rename" => Msg::SortStrategy(Strategy::Rename),
        "sort-skip" => Msg::SortStrategy(Strategy::Skip),
        "sort-go" => Msg::SortGo,
        "trash" => Msg::PruneTrash,
        "unmark" => Msg::PruneUnmark,
        _ => return None,
    })
}

pub fn save_shot(path: &str, shot: &window::Screenshot) {
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[script] cannot write {path}: {e}");
            return;
        }
    };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), shot.size.width, shot.size.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut w) = enc.write_header() {
        let _ = w.write_image_data(&shot.rgba);
    }
    eprintln!("[script] saved {path} ({}x{})", shot.size.width, shot.size.height);
}
