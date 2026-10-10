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
//! analyse-wait 30000          # wait (up to 30 s) for the pass to end
//! scan-wait 60000             # wait (up to 60 s) for the folder's scan
//! quality [all|plans]         # print its state, flagged frames, moments
//!                             # and duplicates (all: every shot; plans:
//!                             # what each bulk action would mark, keep-this
//!                             # on the grid's selected photo)
//! hints problems|soft|accidental|duplicates|sharpest|off
//!                             # the filter bar's hints filter
//! hintmenu open|close         # the Hints menu
//! hintdump                    # print the hints filter, its counts, the
//!                             # status line and the banner's state
//! bulk soft|accidental|copies|keep   # a bulk action from the Prune menu
//!                             # (opens its confirmation)
//! vbulk keep                  # Keep this, prune the rest in the viewer
//! bulk confirm|cancel         # answer it
//! bulkdump                    # print the confirmation waiting, if any
//! links                       # print the grid's moment splices
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
//! activate 0                  # open the viewer on a grid tile
//! aspect 1:1                  # pick a crop ratio in the viewer
//! filter marked|all|date|date-from|date-close   # (all: the Reset button)
//! filter show-all|to-review|show-marked|show-pruned
//!                             # the All · To review · Marked · Pruned choice
//! filter pruned               # Preferences ▸ Show pruned photos, on or off
//!                             # (the filter bar's old Show pruned chip)
//! progress                    # print review progress and the filter
//! shown                       # print the grid's tiles and marks
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
//! prefs advance               # press a control (see `prefs_step`)
//! prefs thumbsize 240         # drag the thumbnail-size slider there
//! prefs confirm|cancel        # answer a Clear confirmation
//! prefs resize 540 640        # resize the Preferences window
//! prefs library /path         # as if the folder picker returned /path
//! prefs editcopy /path        #   (library / edit-copy destination)
//! pkey tab                    # a key press in the Preferences window
//! prefs search-gpu|search-cpu|checkcard|closeness-reset|exports|forget
//! prefs search-rebuild|search-clear|models-delete|models-open
//! prefs find-faces|keep-faces|forget-faces|face-model|forget-people
//! prefs smallest 3            # Smallest group to show: 3, 5 or 10
//! prefs rail-subjects         # Show People and fursuits in the sidebar, on or off
//!                             #   (show-people: Preferences at that row)
//! prefs facepick 1|facego|facecancel   # the face model dialog
//!                             # Preferences ▸ Search and its Storage rows
//! prefs closeness 4.5         # set how close matches must be (the slider)
//! prefs show-search           # open Preferences at its Search section
//! prefs show-fursuits|show-storage   # ... at Fursuits' last row, Storage's
//!                             # models row
//! search open                 # the search page (Ctrl+F, the rail)
//! search type red car         # type into the field (replacing it)
//! search enter|down|up|tab    # keys in the field (Enter searches, or
//!                             # takes the completion ↑ ↓ chose; Tab
//!                             # takes it, or the first)
//! search take 2               # click the 3rd completion under the field
//! search complete             # print what the completions offer
//! search things               # print the start page's Things (and what
//!                             # the engine sent that isn't offered)
//! search thing 0              # click the start page's 1st Thing
//! search fursuits show|hide   # Show fursuits too (the end note's link) /
//!                             # Hide fursuits (their heading's link)
//! search run red car          # type and search
//! search esc|clear|back       # Esc on the page, the field's ×, ‹ back
//! search key right|left|space|enter|g|shift+g   # keys on the results
//! search click 3 [ctrl]       # click the 4th result tile
//! search activate 3           # double-click it (the viewer, read-only)
//! search tab 3                # click the burst tab on the 4th tile
//! search scroll 20000 3000    # scroll the results to y=20000 px over 3 s
//! search openday              # Open day ›
//! search index                # Index the library… (the download dialog)
//! search dlg download|cancel|close|sums|retry|stop|show
//!                             # the download dialog's buttons
//! search stop|steps           # the status bar's × / its progress
//! search trycard|tryagain|prefs|hidebanner   # the banner's buttons
//! search fake indexing 300    # set the stand-in engine (PP_SEARCH_FAKE):
//!                             # notindexed, indexed, indexing [n],
//!                             # export [n], pause, resume, cardcrash,
//!                             # selfcheck, nocard, away [share],
//!                             # restarted, waiting [n], mismatch, offline,
//!                             # models (all in place, nothing indexed),
//!                             # search-model (only search's in place),
//!                             # download-rate 38 (MB a second by the
//!                             # clock; pause stalls the download)
//! search wait 5000            # wait (up to 5 s) for the query in flight
//! search dump                 # print the page's state
//! search fake people          # the stand-in's people: people (two named),
//!                             # people-first (none named), faces-catchup
//!                             # (indexed before faces), faces-off,
//!                             # faces-running [n]
//! search fake fursuits        # the stand-in's fursuits: fursuits (two
//!                             # named), fursuits-first (none named),
//!                             # heads-catchup (indexed before heads),
//!                             # heads-off, heads-running [n],
//!                             # heads-asking [photos] [first|export|resume],
//!                             # heads-skipped [n], heads-cardcrash
//! fursuits start|skip|stop|findheads|trycard|prefs|index
//!                             # the head step's buttons: the processor's
//!                             # question, ×, the catch-up offer, the card
//! fursuits dump               # print the head step and the Fursuits tab
//! fursuits updateheads        # the offer to update heads found before v2
//! judge stop|showleftout|trycard|prefs   # the close-call judge's line
//! judge dump                  # print the judge's state and its run
//! prefs judge-off|judge-qwen|judge-gemma4|judge-download|update-heads
//! prefs judge-open|judge-delete|show-judge-storage [qwen|gemma4]
//!                             # Preferences ▸ Fursuits' judge, Storage's row
//! search fake judge-on [qwen|gemma|both]|judge-off|judge-notinstalled|judge-wait|judge-free|
//!             judge-cardcrash|judge-broken|judge-nosure|heads-vectors [n|all]
//!                             # the stand-in's judge and heads to update
//! search dlg fursuits         # the dialog's Find fursuit heads box
//! prefs find-heads|find-heads-now|forget-fursuits
//!                             # Preferences ▸ Fursuits
//! search dlg faces|face 1     # the dialog's Find people's faces box, the
//!                             # 2nd face model (registry order)
//! subjects open               # People and fursuits (Ctrl+4)
//! subjects tab person|fursuit  # pets are on hold: `tab pet` says there is no such tab
//! subjects click 2 [ctrl]     # the 3rd group tile in the tab's order
//!                             # (opens it; ctrl picks it)
//! subjects small|hidden       # open or fold Smaller groups / Hidden
//! subjects select             # S: pick several
//! subjects merge|mergego|mergecancel|keep 1   # the merge dialog
//! subjects drag 3 0|dragto 3 0|dragdrop   # drag the 4th tile onto the 1st
//!                             # (and let go; dragto holds it there)
//! subjects hide|unhide 0      # hide the picked; show a hidden one again
//! subjects undo               # Ctrl+Z on the page
//! subjects forgetgo|forgetcancel
//! subjects findfaces          # the catch-up offer's button
//! subjects name Person C      # type in a group's name field
//! subjects nameenter|take 1|rename
//! subjects words camera       # type beside the chip, then Enter
//! subjects photos|faces       # Photos | Faces on a subject's page
//! subjects facepick 3         # pick the 4th face
//! subjects moveto new|2       # Move to… a new group / the 3rd group listed
//! subjects notthis            # "Not Person A"
//! subjects menu more|merge|move|close
//! subjects mergeinto 0        # ⋯ ▸ Merge into… the 1st listed
//! subjects hidethis|forget    # ⋯ ▸ Hide / Forget this person…
//! subjects pclick 2|pactivate 2   # a photo tile on a subject's page
//! subjects openday|back
//! subjects review             # Y: start the review
//! subjects y|n|next|prev|at 5|done|findmore|advance|open
//! subjects pfindmore          # Find more on a fursuit's band
//! subjects wait 3000          # wait for the subject's query in flight
//! subjects dump               # print the page's state
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
    /// The search page's results rather than the grid.
    search: bool,
}

/// One frame of a scripted scroll.
pub fn drive_scroll(app: &mut App) {
    let Some(d) = &app.script_scroll else { return };
    let k = (d.start.elapsed().as_secs_f32() * 1000.0 / d.ms).min(1.0);
    let y = d.from + (d.to - d.from) * k;
    if d.search {
        app.search.pane.scroll = y;
        app.search.pane.scroll_seq += 1;
    } else {
        app.grid.scroll = y;
        app.grid.scroll_seq += 1;
    }
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
                "PrevBurst" => Some(Cmd::PrevBurst),
                "NextBurst" => Some(Cmd::NextBurst),
                "Analyse" => Some(Cmd::Analyse),
                "KeepThisPruneRest" => Some(Cmd::KeepThisPruneRest),
                "PruneSoft" => Some(Cmd::PruneSoft),
                "PruneAccidental" => Some(Cmd::PruneAccidental),
                "PruneCopies" => Some(Cmd::PruneCopies),
                "ProblemsOnly" => Some(Cmd::ProblemsOnly),
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
        "search" => {
            let (t, wait) = search_step(app, i, &arg);
            if let Some(t) = wait {
                return t;
            }
            t
        }
        "fursuits" => {
            use super::subjects_fursuits::{Msg as F, debug_state};
            let m = |f: F| Message::Fursuits(f);
            match arg.trim() {
                "start" => app.update(m(F::Start)),
                "skip" => app.update(m(F::Skip)),
                "stop" => app.update(m(F::Stop)),
                "findheads" => app.update(m(F::FindHeads)),
                "trycard" => app.update(m(F::TryCard)),
                "prefs" => app.update(m(F::OpenPrefs)),
                "index" => app.update(m(F::IndexLibrary)),
                "updateheads" => app.update(m(F::UpdateHeads)),
                "dump" => {
                    eprintln!("[fursuits] {}", debug_state(app));
                    Task::none()
                }
                other => {
                    eprintln!("[script] unknown fursuits action {other}");
                    Task::none()
                }
            }
        }
        "judge" => {
            use super::fursuit_judge::{Msg as J, debug_state};
            let m = |j: J| Message::Judge(j);
            match arg.trim() {
                "stop" => app.update(m(J::Stop)),
                "showleftout" => app.update(m(J::ShowLeftOut)),
                "trycard" => app.update(m(J::TryCard)),
                "prefs" => app.update(m(J::OpenPrefs)),
                "dump" => {
                    eprintln!("[judge] {}", debug_state(app));
                    Task::none()
                }
                other => {
                    eprintln!("[script] unknown judge action {other}");
                    Task::none()
                }
            }
        }
        "subjects" => {
            let (t, wait) = subjects_step(app, i, &arg);
            if let Some(t) = wait {
                return t;
            }
            t
        }
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
        "scan-wait" => {
            let left: u64 = arg.parse().unwrap_or(60_000);
            if app.scanning && left > 0 {
                app.script[i] = format!("scan-wait {}", left.saturating_sub(200));
                return Task::perform(tokio::time::sleep(Duration::from_millis(200)), move |_| Message::Script(i));
            }
            eprintln!("[analysis] scan done: scanning={} records={}", app.scanning, app.coll.len());
            Task::none()
        }
        "analyse-wait" => {
            // Poll until the pass ends (or the time is up).
            let left: u64 = arg.parse().unwrap_or(30_000);
            if app.quality.running() && left > 0 {
                app.script[i] = format!("analyse-wait {}", left.saturating_sub(200));
                return Task::perform(tokio::time::sleep(Duration::from_millis(200)), move |_| Message::Script(i));
            }
            eprintln!("[analysis] ended: running={} last={:?}", app.quality.running(), app.quality.last.as_ref().map(|f| (f.complete, f.error.clone(), f.measured, f.cached)));
            Task::none()
        }
        "hints" => {
            use crate::model::hints::HintFilter;
            let f = match arg.as_str() {
                "problems" => Some(HintFilter::Problems),
                "soft" => Some(HintFilter::Soft),
                "accidental" => Some(HintFilter::Accidental),
                "duplicates" => Some(HintFilter::Duplicates),
                "sharpest" => Some(HintFilter::Sharpest),
                _ => None,
            };
            app.update(Message::Filter(super::FilterMsg::Hint(f)))
        }
        "hintmenu" => app.update(Message::Hints(super::hints_ui::Msg::Menu(arg != "close"))),
        "hintdump" => {
            let q = &app.quality;
            eprintln!(
                "[hints] filter={:?} counts={:?} coverage={:?} analysing={} menu={} visible={} tiles={} status={:?} action={:?} banner={}",
                app.filter.hint,
                app.derived.hint_counts,
                q.hints.coverage(),
                q.analysing(),
                q.menu,
                app.derived.visible.len(),
                app.derived.tiles.len(),
                app.status.text,
                app.status.action.as_ref().map(|a| a.0.clone()),
                super::hints_ui::banner(app).is_some(),
            );
            Task::none()
        }
        "links" => {
            let name = |id: crate::model::collection::RecordId| app.coll.get(id).map(|r| r.stem()).unwrap_or_default();
            let mut pairs = Vec::new();
            for (sec, links) in app.derived.sections.iter().zip(&app.derived.moment_links) {
                for (k, &l) in links.iter().enumerate() {
                    if l {
                        pairs.push(format!("{}>{}", name(sec.ids[k]), name(sec.ids[k + 1])));
                    }
                }
            }
            eprintln!("[links] {} splices: {:?}", pairs.len(), pairs);
            Task::none()
        }
        "bulk" | "vbulk" => {
            use super::bulk::{Msg, open_keep_viewer};
            match (cmd, arg.as_str()) {
                ("vbulk", _) => open_keep_viewer(app),
                (_, "confirm") => app.update(Message::Bulk(Msg::Confirm)),
                (_, "cancel") => app.update(Message::Bulk(Msg::Cancel)),
                (_, "soft") => app.update(Message::Cmd(Cmd::PruneSoft)),
                (_, "accidental") => app.update(Message::Cmd(Cmd::PruneAccidental)),
                (_, "copies") => app.update(Message::Cmd(Cmd::PruneCopies)),
                (_, "keep") => app.update(Message::Cmd(Cmd::KeepThisPruneRest)),
                _ => Task::none(),
            }
        }
        "bulkdump" => {
            let names = |ids: &[crate::model::collection::RecordId]| -> Vec<String> { ids.iter().filter_map(|&id| app.coll.get(id)).map(|r| r.filename()).collect() };
            match &app.bulk {
                Some(b) => eprintln!(
                    "[bulk] {:?} in {} window: mark {:?} prune {:?} chosen {:?} scope {}",
                    b.action,
                    if Some(b.window) == app.main_id { "main" } else { "viewer" },
                    names(&b.plan.mark),
                    names(&b.plan.prune),
                    b.chosen.map(|c| names(&[c])),
                    b.scope
                ),
                None => eprintln!("[bulk] none waiting; status={:?}", app.status.text),
            }
            Task::none()
        }
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
        // Show pruned photos is a preference now; scripts that toggled the
        // chip keep working.
        "filter" if arg == "pruned" => app.toggle_show_pruned(),
        "filter" => {
            let m = match arg.as_str() {
                "marked" => Some(super::FilterMsg::ToggleMarkedOnly),
                "all" => Some(super::FilterMsg::ResetAll),
                "date" => Some(super::FilterMsg::ToggleDate),
                "date-from" => Some(super::FilterMsg::OpenDate(true)),
                "date-close" => Some(super::FilterMsg::CloseDatePopup),
                "show-all" => Some(super::FilterMsg::Show(super::Show::All)),
                "to-review" => Some(super::FilterMsg::Show(super::Show::ToReview)),
                "show-marked" => Some(super::FilterMsg::Show(super::Show::Marked)),
                "show-pruned" => Some(super::FilterMsg::Show(super::Show::Pruned)),
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
                "[dump] folder={:?} records={} sections={} visible={} status={:?} thumbs={} scanning={} view={:?} win={:?}",
                app.folder,
                app.coll.len(),
                app.derived.sections.len(),
                app.derived.visible.len(),
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
            app.script_scroll = Some(ScrollDrive { start: Instant::now(), from: app.grid.scroll, to, ms, search: false });
            Task::none()
        }
        "shown" => {
            // The grid's tiles with their marks.
            let ids = &app.derived.tiles;
            let names: Vec<String> = ids
                .iter()
                .filter_map(|&id| app.coll.get(id))
                .map(|r| format!("{}{}{}", r.stem(), if r.is_pruned { " P" } else { "" }, if r.is_marked { " M" } else { "" }))
                .collect();
            let unpruned = ids.iter().filter(|&&id| app.coll.get(id).is_some_and(|r| !r.is_pruned)).count();
            let selected: Vec<String> = app.grid.selection.iter().filter_map(|&id| app.coll.get(id)).map(|r| r.stem()).collect();
            eprintln!(
                "[shown] view={:?} show={:?} n={} not_pruned={unpruned} selected={selected:?} {:?}",
                app.view,
                app.filter.show(),
                names.len(),
                names.iter().take(40).collect::<Vec<_>>()
            );
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

/// `qwen` or (anything else) `gemma4`: which judge a step means.
fn judge_arg(rest: &str) -> crate::ml::judge::JudgeChoice {
    if rest.trim().starts_with("qwen") { crate::ml::judge::JudgeChoice::Qwen3VlReranker } else { crate::ml::judge::JudgeChoice::Gemma4 }
}

fn prefs_step(app: &mut App, arg: &str) -> Task<Message> {
    use super::preferences::{Control, Msg};
    use super::preferences_storage::Cache;
    let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
    let rest = rest.trim();
    let control = match what {
        "advance" => Some(Control::Advance),
        "showpruned" => Some(Control::ShowPruned),
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
        "clear-quality" => Some(Control::Clear(Cache::Quality)),
        "find-heads" => Some(Control::FindHeads),
        "find-heads-now" => Some(Control::FindHeadsNow),
        "forget-fursuits" => Some(Control::ForgetFursuits),
        "judge-off" => Some(Control::Judge(crate::ml::judge::JudgeChoice::Off)),
        "judge-gemma4" => Some(Control::Judge(crate::ml::judge::JudgeChoice::Gemma4)),
        "judge-qwen" => Some(Control::Judge(crate::ml::judge::JudgeChoice::Qwen3VlReranker)),
        "judge-download" => Some(Control::JudgeDownload),
        "update-heads" => Some(Control::UpdateHeads),
        "judge-open" => Some(Control::JudgeOpen(judge_arg(rest))),
        "judge-delete" => Some(Control::JudgeDelete(judge_arg(rest))),
        "search-gpu" => Some(Control::SearchDevice(crate::ml::Device::Gpu)),
        "search-cpu" => Some(Control::SearchDevice(crate::ml::Device::Cpu)),
        "checkcard" => Some(Control::CheckCard),
        "closeness-reset" => Some(Control::ClosenessReset),
        "exports" => Some(Control::IndexExports),
        "forget" => Some(Control::ForgetSearches),
        "search-rebuild" => Some(Control::SearchRebuild),
        "search-clear" => Some(Control::SearchClear),
        "models-delete" => Some(Control::ModelsDelete),
        "models-open" => Some(Control::ModelsOpen),
        "find-faces" => Some(Control::FindFaces),
        "keep-faces" => Some(Control::KeepFaces),
        "forget-faces" => Some(Control::ForgetFaces),
        "face-model" => Some(Control::FaceModel),
        "forget-people" => Some(Control::ForgetPeople),
        "rail-subjects" => Some(Control::RailSubjects),
        "smallest" => Some(Control::SmallestGroup(rest.parse().unwrap_or(5))),
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
        "facepick" => match crate::ml::models::face_models().nth(rest.parse().unwrap_or(0)) {
            Some(mi) => app.update(Message::Prefs(Msg::PickFaceModel(mi.id))),
            None => Task::none(),
        },
        "facego" => app.update(Message::Prefs(Msg::ChangeFaceModel(true))),
        "facecancel" => app.update(Message::Prefs(Msg::ChangeFaceModel(false))),
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
        "closeness" => {
            // As the slider sets it: by its place on the track.
            let v: f32 = rest.parse().unwrap_or(crate::search::CLOSENESS_DEFAULT);
            let t = app.update(Message::Prefs(Msg::Closeness(super::preferences::closeness_to_track(v))));
            Task::batch([t, app.update(Message::Prefs(Msg::ClosenessDone))])
        }
        "show-search" => super::preferences::open_at(app, Control::SearchDevice(app.settings.search_device)),
        "show-people" => super::preferences::open_at(app, Control::RailSubjects),
        "show-fursuits" => super::preferences::open_at(app, Control::ForgetFursuits),
        "show-judge" => super::preferences::open_at(app, Control::Judge(crate::ml::judge::JudgeChoice::Gemma4)),
        "show-judge-storage" => super::preferences::open_at(app, Control::JudgeDelete(judge_arg(rest))),
        "show-storage" => super::preferences::open_at(app, Control::ModelsDelete),
        "cancel" => app.update(Message::Prefs(Msg::ConfirmCancel)),
        "library" => app.update(Message::FolderPicked(super::FolderTarget::Library, Some(rest.into()))),
        "editcopy" => app.update(Message::FolderPicked(super::FolderTarget::EditCopySetting, Some(rest.into()))),
        "dump" => {
            let s = &app.settings;
            eprintln!("[prefs] {}", app.prefs.debug_state());
            eprintln!(
                "[prefs] search device={:?} closeness={} index_exports={} recent={:?}",
                s.search_device, s.search_closeness, s.search_index_exports, s.search_recent
            );
            eprintln!(
                "[prefs] settings advance={} show_pruned={} names={} thumb={} filmstrip={} burst_sheet={} viewer={}x{} max={} library={:?} edit_copy={:?} subfolder={} global_menu={}",
                s.viewer_advance,
                s.filter_pruned,
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

/// One `search …` step.  Returns the task, and for `search wait` the
/// polling task that replaces the script's own next step.
fn search_step(app: &mut App, i: usize, arg: &str) -> (Task<Message>, Option<Task<Message>>) {
    use super::search::{DialogMsg, Msg};
    use crate::ui::grid::GridEvent;
    let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
    let rest = rest.trim();
    let s = |m: Msg| Message::Search(m);
    let tile = |app: &App, n: &str| app.search.pane.layout.tiles.get(n.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0)).copied();
    let main_key = |app: &mut App, spec: &str| {
        let (key, mods) = parse_key(spec);
        match app.main_id {
            Some(id) => app.on_key_public(id, key, mods),
            None => Task::none(),
        }
    };
    let t = match what {
        "open" => app.update(Message::Cmd(Cmd::Search)),
        "type" => app.update(s(Msg::Input(rest.to_string()))),
        "run" => {
            let t = app.update(s(Msg::Input(rest.to_string())));
            Task::batch([t, app.update(s(Msg::Submit))])
        }
        "enter" => app.update(s(Msg::Submit)),
        "down" | "up" => main_key(app, what),
        "tab" if rest.is_empty() => main_key(app, "tab"),
        "esc" => main_key(app, "escape"),
        "key" => main_key(app, rest),
        "clear" => app.update(s(Msg::Clear)),
        "back" => app.update(s(Msg::Back)),
        "click" => match tile(app, rest) {
            Some(id) => {
                let modifiers = if rest.ends_with("ctrl") { Modifiers::CTRL } else { Modifiers::empty() };
                app.update(s(Msg::Grid(GridEvent::Click { id, modifiers })))
            }
            None => Task::none(),
        },
        "activate" => match tile(app, rest) {
            Some(id) => app.update(s(Msg::Grid(GridEvent::Activate(id)))),
            None => Task::none(),
        },
        "tab" if !rest.is_empty() => match tile(app, rest) {
            Some(id) => app.update(s(Msg::Grid(GridEvent::ToggleBurst(id)))),
            None => Task::none(),
        },
        "openday" => app.update(s(Msg::OpenDay)),
        "take" => app.update(s(Msg::Take(rest.parse().unwrap_or(0)))),
        "complete" => {
            eprintln!("[search] complete {}", super::search::debug_offer(app));
            Task::none()
        }
        "things" => {
            eprintln!("[search] things {}", super::search::debug_things(app));
            Task::none()
        }
        "thing" => {
            let word = super::search_results::offered(&app.search.things).nth(rest.parse().unwrap_or(0)).map(|t| t.word.clone());
            match word {
                Some(word) => app.update(s(Msg::Run(word))),
                None => Task::none(),
            }
        }
        "fursuits" => match rest {
            "show" => app.update(s(Msg::Grid(GridEvent::FooterLink))),
            "hide" => app.update(s(Msg::Grid(GridEvent::SectionLink(super::search_results::FURSUITS_DAY)))),
            _ => Task::none(),
        },
        "index" => app.update(s(Msg::IndexLibrary)),
        "dlg" => {
            let m = match rest {
                "faces" => Some(DialogMsg::Faces),
                "fursuits" => Some(DialogMsg::Fursuits),
                r if r.starts_with("face ") => r[5..].trim().parse::<usize>().ok().and_then(|k| crate::ml::models::face_models().nth(k)).map(|mi| DialogMsg::FaceModel(mi.id)),
                "download" => Some(DialogMsg::Download),
                "cancel" => Some(DialogMsg::Cancel),
                "close" => Some(DialogMsg::Close),
                "sums" => Some(DialogMsg::Sums),
                "retry" => Some(DialogMsg::Retry),
                "stop" => Some(DialogMsg::StopDownload),
                "show" => Some(DialogMsg::Show),
                _ => None,
            };
            match m {
                Some(m) => app.update(s(Msg::Dialog(m))),
                None => Task::none(),
            }
        }
        "stop" => app.update(s(Msg::Stop)),
        "steps" => app.update(s(Msg::Steps(true))),
        "trycard" => app.update(s(Msg::TryCard)),
        "tryagain" => app.update(s(Msg::TryAgain)),
        "prefs" => app.update(s(Msg::OpenPrefs)),
        "hidebanner" => app.update(s(Msg::HideBanner)),
        "fake" => {
            app.search.link.fake(rest);
            Task::none()
        }
        "scroll" => {
            let mut a = rest.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(0.0));
            let (to, ms) = (a.next().unwrap_or(0.0), a.next().unwrap_or(1000.0).max(1.0));
            app.script_scroll = Some(ScrollDrive { start: Instant::now(), from: app.search.pane.scroll, to, ms, search: true });
            Task::none()
        }
        "wait" => {
            let left: u64 = rest.parse().unwrap_or(5_000);
            if app.search.pending.is_some() && left > 0 {
                app.script[i] = format!("search wait {}", left.saturating_sub(100));
                return (Task::none(), Some(Task::perform(tokio::time::sleep(Duration::from_millis(100)), move |_| Message::Script(i))));
            }
            eprintln!("[search] wait done: pending={:?}", app.search.pending);
            Task::none()
        }
        "dump" => {
            eprintln!("[search] {}", super::search::debug_state(app));
            Task::none()
        }
        _ => {
            eprintln!("[script] unknown search action {arg}");
            Task::none()
        }
    };
    (t, None)
}

fn subjects_step(app: &mut App, i: usize, arg: &str) -> (Task<Message>, Option<Task<Message>>) {
    use super::people::Kind;
    use super::subjects::{self, Msg};
    use super::subjects_person::{Menu, Msg as P, Show};
    use super::subjects_review::Msg as R;
    use crate::ui::grid::GridEvent;
    let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
    let rest = rest.trim();
    let n: usize = rest.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let s = |m: Msg| Message::Subjects(m);
    let p = |m: P| Message::Subjects(Msg::Person(m));
    let r = |m: R| Message::Subjects(Msg::Review(m));
    // The tab's groups in the page's order (what the arrow keys step).
    let order = |app: &App| {
        let st = &app.subjects;
        subjects::arrange(&st.all, st.tab, app.settings.people_min_photos()).order(st.small_open, st.hidden_open)
    };
    let others = |app: &App| -> Vec<super::people::GroupId> {
        let st = &app.subjects;
        let Some(pp) = st.person.as_ref() else { return Vec::new() };
        let a = subjects::arrange(&st.all, pp.kind, app.settings.people_min_photos());
        a.named.iter().chain(a.to_name.iter().take(6)).map(|g| g.id.clone()).filter(|id| *id != pp.group).collect()
    };
    let photo = |app: &App, n: usize| app.subjects.pane().and_then(|pp| pp.layout.tiles.get(n).copied());
    let t = match what {
        "open" => app.update(Message::Cmd(Cmd::Subjects)),
        "tab" => {
            let k = match rest {
                "fursuit" | "fursuits" => Some(Kind::Fursuit),
                "pet" | "pets" => None,
                _ => Some(Kind::Person),
            };
            match k {
                Some(k) => app.update(s(Msg::Tab(k))),
                None => {
                    println!("subjects tab: there is no pets tab (pets are on hold); use person or fursuit");
                    Task::none()
                }
            }
        }
        "click" => match order(app).get(n).cloned() {
            Some(id) if rest.ends_with("ctrl") => app.update(s(Msg::Pick(id))),
            Some(id) => app.update(s(Msg::Tile(id))),
            None => Task::none(),
        },
        "small" => {
            let open = !app.subjects.small_open;
            app.update(s(Msg::Small(open)))
        }
        "hidden" => {
            let open = !app.subjects.hidden_open;
            app.update(s(Msg::Hidden(open)))
        }
        "select" => app.update(s(Msg::SelectMode)),
        "merge" => app.update(s(Msg::MergeAsk)),
        "mergego" => app.update(s(Msg::MergeGo)),
        "mergecancel" => app.update(s(Msg::MergeCancel)),
        // A drag as the tiles report it: `drag 3 0` drags the 4th tile onto
        // the 1st and lets go; `dragto 3 0` holds it there (a screenshot);
        // `dragdrop` lets go.
        "drag" | "dragto" => {
            let ids = order(app);
            let mut nums = rest.split_whitespace().filter_map(|x| x.parse::<usize>().ok());
            let (a, b) = (nums.next().and_then(|i| ids.get(i).cloned()), nums.next().and_then(|i| ids.get(i).cloned()));
            match (a, b) {
                (Some(a), Some(b)) => {
                    let mut t = vec![app.update(s(Msg::DragStart(a.clone()))), app.update(s(Msg::DragEnter(a.clone()))), app.update(s(Msg::DragLeave(a))), app.update(s(Msg::DragEnter(b)))];
                    if what == "drag" {
                        t.push(app.update(s(Msg::DragDrop)));
                    }
                    Task::batch(t)
                }
                _ => {
                    println!("subjects drag: no such tiles ({rest})");
                    Task::none()
                }
            }
        }
        "dragdrop" => app.update(s(Msg::DragDrop)),
        "keep" => match app.subjects.merge.as_ref().and_then(|mg| mg.groups.get(n).cloned()) {
            Some(id) => app.update(s(Msg::MergeKeep(id))),
            None => Task::none(),
        },
        "hide" => app.update(s(Msg::HidePicked)),
        "unhide" => {
            let st = &app.subjects;
            let id = subjects::arrange(&st.all, st.tab, app.settings.people_min_photos()).hidden.get(n).map(|g| g.id.clone());
            match id {
                Some(id) => app.update(s(Msg::Unhide(id))),
                None => Task::none(),
            }
        }
        "undo" => app.update(s(Msg::Undo)),
        "forgetgo" => app.update(s(Msg::ForgetGo)),
        "forgetcancel" => app.update(s(Msg::ForgetCancel)),
        "findfaces" => app.update(s(Msg::FindFaces)),
        "name" => app.update(p(P::NameInput(rest.to_string()))),
        "nameenter" => app.update(p(P::NameSubmit)),
        "take" => app.update(p(P::NameTake(n))),
        "rename" => app.update(p(P::Rename)),
        "words" => {
            let t = app.update(p(P::Words(rest.to_string())));
            Task::batch([t, app.update(p(P::Submit))])
        }
        "photos" => app.update(p(P::Show(Show::Photos))),
        "faces" => app.update(p(P::Show(Show::Faces))),
        "facepick" => {
            let f = app.subjects.person.as_ref().and_then(|pp| pp.faces.as_ref()).and_then(|f| f.get(n)).map(|f| f.face);
            match f {
                Some(f) => app.update(p(P::FacePick(f))),
                None => Task::none(),
            }
        }
        "moveto" if rest == "new" => app.update(p(P::MoveTo(None))),
        "moveto" => match others(app).get(n).cloned() {
            Some(id) => app.update(p(P::MoveTo(Some(id)))),
            None => Task::none(),
        },
        "notthis" => app.update(p(P::NotThis)),
        "menu" => {
            let m = match rest {
                "more" => Some(Menu::More),
                "merge" => Some(Menu::MergeInto),
                "move" => Some(Menu::MoveTo),
                _ => None,
            };
            app.update(p(P::Menu(m)))
        }
        "mergeinto" => match others(app).get(n).cloned() {
            Some(id) => app.update(p(P::MergeInto(id))),
            None => Task::none(),
        },
        "hidethis" => app.update(p(P::Hide)),
        "forget" => app.update(p(P::Forget)),
        "pclick" => match photo(app, n) {
            Some(id) => app.update(p(P::Grid(GridEvent::Click { id, modifiers: Modifiers::empty() }))),
            None => Task::none(),
        },
        "pactivate" => match photo(app, n) {
            Some(id) => app.update(p(P::Grid(GridEvent::Activate(id)))),
            None => Task::none(),
        },
        "openday" => app.update(p(P::OpenDay)),
        "back" => app.update(p(P::Back)),
        "review" => app.update(p(P::Review)),
        "y" => app.update(r(R::Answer(true))),
        "n" => app.update(r(R::Answer(false))),
        "next" => app.update(r(R::Step(1))),
        "prev" => app.update(r(R::Step(-1))),
        "at" => app.update(r(R::At(n))),
        "done" => app.update(r(R::Done)),
        "findmore" => app.update(r(R::FindMore)),
        "pfindmore" => app.update(p(P::FindMore)),
        "advance" => app.update(r(R::Advance)),
        "photo" => app.update(r(R::Open)),
        "wait" => {
            let left: u64 = rest.parse().unwrap_or(3_000);
            let pending = app.subjects.person.as_ref().is_some_and(|pp| pp.pending.is_some());
            if pending && left > 0 {
                app.script[i] = format!("subjects wait {}", left.saturating_sub(100));
                return (Task::none(), Some(Task::perform(tokio::time::sleep(Duration::from_millis(100)), move |_| Message::Script(i))));
            }
            Task::none()
        }
        "dump" => {
            eprintln!("[subjects] {} {}", subjects::debug_state(app), super::subjects_fursuits::debug_state(app));
            Task::none()
        }
        _ => {
            eprintln!("[script] unknown subjects action {arg}");
            Task::none()
        }
    };
    (t, None)
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
