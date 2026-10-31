//! Network shares: where the working folder and the library live, and what
//! happens when a share goes away.
//!
//! - **Where.**  Each folder is probed once when it's chosen
//!   (`ops::netfs::probe`, on a worker with a timeout, since `statfs` on a
//!   dead mount can block).  The folder bar marks one on a network share.
//! - **Gone.**  When sidecar saves fail (the saver keeps them, and a copy
//!   on this computer), or the working folder can't be scanned or its
//!   photos loaded, a banner names the share and offers Try again.  While
//!   there's trouble, saves are retried and the folder probed every 5 s,
//!   doubling to a minute.  When they get through, the banner goes and the
//!   status bar says so.
//! - **Quitting** with marks not saved waits a few seconds for them, then
//!   says plainly where they are: kept on this computer, saved to the
//!   folder at the next start.
//!
//! Nothing here runs per frame: retries are timer tasks, and the banner and
//! the folder-bar mark are built from state the events above keep.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard::{Key, key::Named};
use iced::widget::{Space, column, container, opaque, row, text};
use iced::{Alignment, Color, Element, Length, Padding, Task};

use super::{App, Message, kit, style};
use crate::bus::BgEvent;
use crate::model::saver::{Report, SaveError, Unsaved};
use crate::model::sidecar::Kind;
use crate::ops::netfs::{self, Failure, Probe, Reach};
use crate::ui::icons::{self, Glyph};
use crate::ui::motion::Appear;
use crate::ui::paint;
use crate::ui::theme::{self as t, alpha};
use crate::util;

/// How long a probe may take before the folder counts as not answering.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// Quitting waits this long for unsaved marks before asking.
const QUIT_WAIT: Duration = Duration::from_secs(6);
const QUIT_POLL: Duration = Duration::from_millis(250);
/// Failed thumbnails or images probe the folder at most this often.
const LOAD_PROBE_EVERY: Duration = Duration::from_secs(10);
/// A save still being written after this long is stuck on a hung mount.
const STUCK_AFTER: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub enum Msg {
    TryAgain,
    Dismiss,
    RetryTick(u64),
    QuitCheck(u64),
    QuitAnyway,
    KeepOpen,
    WatchSaves,
}

/// What the working folder failed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    Scan,
    Load,
}

#[derive(Debug, Clone)]
struct Trouble {
    folder: PathBuf,
    what: Failed,
    /// The error, when the failure gave one (a scan does, a load doesn't).
    error: Option<SaveError>,
}

pub enum QuitPrompt {
    /// Waiting (briefly) for the last saves.
    Saving { since: Instant },
    /// They didn't make it.  `kept`: copies kept on this computer, or why not.
    Unsaved { kept: Result<usize, String> },
}

#[derive(Default)]
struct Place {
    probe: Option<Probe>,
    probed_at: Option<Instant>,
    /// The probe last asked for, and whether it's still out.
    seq: u64,
    probing: bool,
}

#[derive(Default)]
pub struct Net {
    places: HashMap<PathBuf, Place>,
    /// From the saver's last report.
    unsaved: Vec<Unsaved>,
    /// The working folder failed to scan or load; cleared when it answers.
    trouble: Option<Trouble>,
    /// A failed load asked for this probe: trouble if it fails too.
    load_check: Option<PathBuf>,
    last_load_probe: Option<Instant>,
    dismissed: bool,
    /// Try again is under way (the button says so).
    trying: bool,
    retry_seq: u64,
    retry_scheduled: bool,
    backoff: Duration,
    probe_seq: u64,
    pub quit: Option<QuitPrompt>,
    quit_seq: u64,
    /// The user chose to quit without waiting for saves.
    gave_up: bool,
    /// When the status bar last said marks were saved again.
    saved_note_at: Option<Instant>,
    /// A save to this folder has been stuck for a while (hung mount).
    stuck: Option<PathBuf>,
    /// A check for stuck saves is scheduled.
    watching: bool,
}

impl Net {
    fn has_trouble(&self) -> bool {
        !self.unsaved.is_empty() || self.trouble.is_some() || self.stuck.is_some()
    }

    fn probe_of(&self, path: &Path) -> Option<&Probe> {
        self.places.get(path).and_then(|p| p.probe.as_ref())
    }

    fn answers(&self, path: &Path) -> bool {
        self.probe_of(path).is_some_and(|p| p.reach.is_ok())
    }

    /// Folders whose marks are waiting to be saved (shown in Preferences).
    pub fn unsaved_folders(&self) -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = self.unsaved.iter().map(|u| u.folder.clone()).collect();
        folders.dedup();
        folders
    }

    /// One line for the test driver.
    pub fn debug_state(&self) -> String {
        let places: Vec<String> = self
            .places
            .iter()
            .map(|(p, pl)| format!("{}={:?}", p.display(), pl.probe.as_ref().map(|pr| (pr.share.as_ref().map(|s| s.name()), &pr.reach))))
            .collect();
        let unsaved: Vec<String> = self
            .unsaved
            .iter()
            .map(|u| format!("{}:{}:{}:kept={}", u.folder.display(), u.kind.label(), u.error.message, u.kept_locally))
            .collect();
        format!(
            "places=[{}] unsaved=[{}] trouble={:?} stuck={:?} dismissed={} retry_scheduled={} backoff={:?} quit={}",
            places.join(", "),
            unsaved.join(", "),
            self.trouble.as_ref().map(|t| (&t.folder, t.what)),
            self.stuck,
            self.dismissed,
            self.retry_scheduled,
            self.backoff,
            match &self.quit {
                None => "none".to_string(),
                Some(QuitPrompt::Saving { .. }) => "saving".to_string(),
                Some(QuitPrompt::Unsaved { kept }) => format!("unsaved {kept:?}"),
            }
        )
    }
}

fn quoted(folder: &Path) -> String {
    format!("“{}”", util::file_name(folder))
}

/// An io::Error message without the "(os error N)" tail.
fn plain(message: &str) -> &str {
    match message.rfind(" (os error ") {
        Some(i) => &message[..i],
        None => message,
    }
}

/// The errno in an io::Error message ("… (os error 2)").
fn errno_in(message: &str) -> Option<i32> {
    let tail = &message[message.rfind("(os error ")? + 10..];
    tail.trim_end_matches(')').parse().ok()
}

// ── probing ─────────────────────────────────────────────────────────────────

/// Find out (on a worker) where `path` lives and whether it answers.
pub fn probe(app: &mut App, path: &Path) {
    if app.net.places.get(path).is_some_and(|p| p.probing) {
        return;
    }
    app.net.probe_seq += 1;
    let seq = app.net.probe_seq;
    let place = app.net.places.entry(path.to_path_buf()).or_default();
    place.seq = seq;
    place.probing = true;
    let (bus, p) = (app.bus(), path.to_path_buf());
    let spawned = std::thread::Builder::new().name("netfs".into()).spawn(move || {
        let probe = netfs::probe(&p, PROBE_TIMEOUT);
        let _ = bus.unbounded_send(BgEvent::NetProbe { path: p, seq, probe: Arc::new(probe) });
    });
    if spawned.is_err() {
        if let Some(place) = app.net.places.get_mut(path) {
            place.probing = false;
        }
    }
}

/// The working folder (`working`) or the library is now `path`.
pub fn folder_changed(app: &mut App, path: &Path, working: bool) -> Task<Message> {
    let had = app.net.has_trouble();
    if working {
        app.net.trouble = None;
        app.net.load_check = None;
    }
    probe(app, path);
    after_change(app, had)
}

pub fn on_probe(app: &mut App, path: PathBuf, seq: u64, probe: Arc<Probe>) -> Task<Message> {
    let had = app.net.has_trouble();
    let Some(place) = app.net.places.get_mut(&path) else { return Task::none() };
    if place.seq == seq {
        place.probing = false;
    }
    let was_bad = place.probe.as_ref().is_some_and(|p| !p.reach.is_ok());
    place.probe = Some((*probe).clone());
    place.probed_at = Some(Instant::now());
    app.net.trying = false;
    let ok = probe.reach.is_ok();
    let mut tasks = Vec::new();
    if app.net.load_check.as_ref() == Some(&path) {
        app.net.load_check = None;
        if !ok && app.folder.as_ref() == Some(&path) && app.net.trouble.is_none() {
            app.net.trouble = Some(Trouble { folder: path.clone(), what: Failed::Load, error: None });
        }
    }
    // Only a folder seen away and now answering counts as back: one that
    // answers while its scan fails is failing for another reason, and
    // reopening it on every probe would loop.
    if ok && was_bad {
        if let Some(t) = app.net.trouble.clone().filter(|t| t.folder == path) {
            app.net.trouble = None;
            tasks.push(back(app, &t.folder, t.what));
        }
        if app.net.unsaved.iter().any(|u| u.folder == path) {
            app.saver.retry();
        }
    }
    tasks.push(after_change(app, had));
    Task::batch(tasks)
}

/// The working folder answers again after `what` failed.
fn back(app: &mut App, folder: &Path, what: Failed) -> Task<Message> {
    let name = quoted(folder);
    match what {
        Failed::Scan => {
            let t = app.start_scan(folder.to_path_buf());
            Task::batch([t, app.show_status(format!("{name} can be reached again. Opening it…"), 0)])
        }
        Failed::Load => {
            if app.thumbs.forget_failed() > 0 {
                app.refresh_views();
            }
            // "Saved again" says more; don't cover it.
            let saving = app.net.unsaved.iter().any(|u| u.folder == folder);
            if saving || app.net.saved_note_at.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
                return Task::none();
            }
            app.show_status(format!("{name} can be reached again."), 4000)
        }
    }
}

// ── trouble reports ─────────────────────────────────────────────────────────

/// The working folder couldn't be scanned, or its marks read (`message`).
pub fn on_scan_error(app: &mut App, message: &str) -> Task<Message> {
    let Some(folder) = app.folder.clone() else { return Task::none() };
    let had = app.net.has_trouble();
    let error = SaveError { errno: errno_in(message), message: message.to_string() };
    app.net.trouble = Some(Trouble { folder: folder.clone(), what: Failed::Scan, error: Some(error) });
    probe(app, &folder);
    let status = app.show_status(format!("Couldn't open {}: {}", quoted(&folder), plain(message)), 0);
    Task::batch([status, after_change(app, had)])
}

/// A thumbnail or the viewer's image failed: check that the folder is still
/// there (at most every few seconds).
pub fn on_load_failed(app: &mut App) {
    let Some(folder) = app.folder.clone() else { return };
    if app.net.trouble.is_some() || app.net.last_load_probe.is_some_and(|t| t.elapsed() < LOAD_PROBE_EVERY) {
        return;
    }
    app.net.last_load_probe = Some(Instant::now());
    app.net.load_check = Some(folder.clone());
    probe(app, &folder);
}

/// The calendar couldn't read the library (`ok` false), or read it.
pub fn on_library_checked(app: &mut App, ok: bool) {
    let Some(lib) = app.library.clone() else { return };
    let known_bad = app.net.probe_of(&lib).is_some_and(|p| !p.reach.is_ok());
    if ok == known_bad {
        probe(app, &lib);
    }
}

pub fn on_saver_report(app: &mut App, report: Arc<Report>) -> Task<Message> {
    let had = app.net.has_trouble();
    let before = std::mem::replace(&mut app.net.unsaved, report.unsaved.clone());
    app.net.trying = false;
    if app.net.stuck.is_some() {
        app.net.stuck = app.saver.stuck(STUCK_AFTER);
    }
    let mut tasks = Vec::new();

    // Check (and name) the share for folders whose saves fail, and refresh
    // folders seen away whose saves now get through.  Not more than every
    // few seconds: a save that fails while the folder answers (permission
    // denied) would otherwise probe on every try.
    let recent = |net: &Net, f: &Path| net.places.get(f).and_then(|p| p.probed_at).is_some_and(|t| t.elapsed() < FIRST_RETRY);
    let newly = |f: &Path| !before.iter().any(|u| u.folder == f);
    let mut check: Vec<PathBuf> = app.net.unsaved.iter().map(|u| u.folder.clone()).filter(|f| newly(f) || !recent(&app.net, f)).collect();
    check.extend(report.saved.iter().map(|(f, _)| f.clone()).filter(|f| !app.net.answers(f)));
    check.sort();
    check.dedup();
    for f in check {
        probe(app, &f);
    }

    if !report.saved.is_empty() {
        let mut folders: Vec<&PathBuf> = report.saved.iter().map(|(f, _)| f).collect();
        folders.dedup();
        let names: Vec<String> = folders.iter().map(|f| quoted(f)).collect();
        let earlier = report.saved.iter().all(|(f, k)| before.iter().any(|u| &u.folder == f && u.kind == *k && u.from_earlier));
        let text = if earlier {
            format!("Marks kept from the last session are saved to {}.", names.join(", "))
        } else {
            format!("Saved again: marks for {} are in the folder.", names.join(", "))
        };
        tasks.push(app.show_status(text, 6000));
        app.net.saved_note_at = Some(Instant::now());
    }
    if !report.set_aside.is_empty() {
        let (folder, _, kept) = &report.set_aside[0];
        let body = format!(
            "Marks for {} were kept on this computer while the folder couldn't be reached. They weren't written, because the folder's marks were changed since (by another program or computer), and those are kept.\n\nThe unsaved copy is at {}.",
            quoted(folder),
            kept.display()
        );
        if app.modal.is_none() {
            tasks.push(super::dialogs::info(app, "Older marks not applied", &body));
        } else {
            tasks.push(app.show_status(format!("Older marks for {} not applied; a copy is at {}", quoted(folder), kept.display()), 0));
        }
    }
    if matches!(app.net.quit, Some(QuitPrompt::Saving { .. })) {
        tasks.push(quit_check(app));
    }
    tasks.push(after_change(app, had));
    Task::batch(tasks)
}

/// A save was queued: make sure a check for stuck saves is coming.  Only a
/// failure reports back, and on a hung mount that can take minutes.
pub fn after_save(app: &mut App) -> Task<Message> {
    if app.net.watching {
        return Task::none();
    }
    app.net.watching = true;
    Task::perform(tokio::time::sleep(STUCK_AFTER), |_| Message::Net(Msg::WatchSaves))
}

fn watch_saves(app: &mut App) -> Task<Message> {
    app.net.watching = false;
    let had = app.net.has_trouble();
    app.net.stuck = app.saver.stuck(STUCK_AFTER);
    if let Some(f) = app.net.stuck.clone() {
        probe(app, &f);
    }
    let mut tasks = vec![after_change(app, had)];
    // Keep watching while something is being written.
    if app.saver.stuck(Duration::ZERO).is_some() {
        tasks.push(after_save(app));
    }
    Task::batch(tasks)
}

// ── retrying ────────────────────────────────────────────────────────────────

/// Start or stop retrying as trouble starts or ends.
fn after_change(app: &mut App, had: bool) -> Task<Message> {
    let now = app.net.has_trouble();
    if now && !had {
        app.net.dismissed = false;
        app.net.backoff = FIRST_RETRY;
    }
    if !now {
        if app.net.retry_scheduled {
            app.net.retry_scheduled = false;
            app.net.retry_seq += 1;
        }
        app.net.trying = false;
        return Task::none();
    }
    if app.net.retry_scheduled { Task::none() } else { schedule(app) }
}

fn schedule(app: &mut App) -> Task<Message> {
    app.net.retry_seq += 1;
    app.net.retry_scheduled = true;
    let seq = app.net.retry_seq;
    let wait = app.net.backoff.max(FIRST_RETRY);
    Task::perform(tokio::time::sleep(wait), move |_| Message::Net(Msg::RetryTick(seq)))
}

/// Try failed saves again and check the folders in trouble.
fn retry_now(app: &mut App) {
    app.saver.retry();
    let mut folders: Vec<PathBuf> = app.net.unsaved.iter().map(|u| u.folder.clone()).collect();
    if let Some(t) = &app.net.trouble {
        folders.push(t.folder.clone());
    }
    folders.extend(app.net.stuck.clone());
    // The library too, if it was seen away (its mark in the folder bar).
    folders.extend(app.library.clone().filter(|l| app.net.probe_of(l).is_some_and(|p| !p.reach.is_ok())));
    folders.sort();
    folders.dedup();
    for f in folders {
        probe(app, &f);
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::TryAgain => {
            app.net.trying = true;
            app.net.backoff = FIRST_RETRY;
            // A folder that answers but failed to open: open it again.
            if let Some(t) = app.net.trouble.clone().filter(|t| t.what == Failed::Scan && app.net.answers(&t.folder)) {
                return app.start_scan(t.folder);
            }
            retry_now(app);
            schedule(app)
        }
        Msg::Dismiss => {
            app.net.dismissed = true;
            Task::none()
        }
        Msg::RetryTick(seq) => {
            if seq != app.net.retry_seq {
                return Task::none();
            }
            app.net.retry_scheduled = false;
            if !app.net.has_trouble() {
                return Task::none();
            }
            retry_now(app);
            app.net.backoff = (app.net.backoff * 2).clamp(FIRST_RETRY, MAX_RETRY);
            schedule(app)
        }
        Msg::QuitCheck(seq) => {
            if seq != app.net.quit_seq {
                return Task::none();
            }
            quit_check(app)
        }
        Msg::QuitAnyway => {
            let _ = app.saver.secure();
            app.net.quit = None;
            app.net.gave_up = true;
            app.quit_now()
        }
        Msg::WatchSaves => watch_saves(app),
        Msg::KeepOpen => {
            app.net.quit = None;
            app.net.dismissed = false;
            if app.net.has_trouble() && !app.net.retry_scheduled { schedule(app) } else { Task::none() }
        }
    }
}

// ── quitting ────────────────────────────────────────────────────────────────

/// On quit: `Some(task)` holds it while marks aren't saved yet.
pub fn hold_quit(app: &mut App) -> Option<Task<Message>> {
    match app.net.quit {
        // Asked again while waiting: don't make them wait, ask now.
        Some(QuitPrompt::Saving { .. }) => {
            let kept = app.saver.secure();
            app.net.unsaved = app.saver.unsaved();
            app.net.quit = Some(QuitPrompt::Unsaved { kept });
            return Some(Task::none());
        }
        Some(QuitPrompt::Unsaved { .. }) => return Some(Task::none()),
        None => {}
    }
    if !app.saver.pending() {
        return None;
    }
    // The last P or M is usually written in a moment.
    if app.saver.flush_for(Duration::from_millis(400)) && !app.saver.pending() {
        return None;
    }
    app.saver.retry();
    app.net.quit = Some(QuitPrompt::Saving { since: Instant::now() });
    Some(quit_tick(app))
}

fn quit_tick(app: &mut App) -> Task<Message> {
    app.net.quit_seq += 1;
    let seq = app.net.quit_seq;
    Task::perform(tokio::time::sleep(QUIT_POLL), move |_| Message::Net(Msg::QuitCheck(seq)))
}

fn quit_check(app: &mut App) -> Task<Message> {
    let Some(QuitPrompt::Saving { since }) = &app.net.quit else { return Task::none() };
    if !app.saver.pending() {
        app.net.quit = None;
        return app.quit_now();
    }
    if since.elapsed() >= QUIT_WAIT {
        let kept = app.saver.secure();
        app.net.unsaved = app.saver.unsaved();
        app.net.quit = Some(QuitPrompt::Unsaved { kept });
        return Task::none();
    }
    quit_tick(app)
}

/// The app is going: wait (bounded) for saves, unless the user already
/// chose not to, and keep whatever isn't saved on this computer.
pub fn on_exit(app: &App) {
    if !app.net.gave_up {
        app.saver.flush();
    }
    if app.saver.pending() {
        let _ = app.saver.secure();
    }
}

/// Keys while the quit prompt is up.
pub fn on_key(app: &mut App, key: &Key) -> Task<Message> {
    match (key, &app.net.quit) {
        (Key::Named(Named::Escape), _) => update(app, Msg::KeepOpen),
        (Key::Named(Named::Enter), Some(QuitPrompt::Unsaved { kept: Ok(_) })) => update(app, Msg::QuitAnyway),
        (Key::Named(Named::Enter), Some(QuitPrompt::Unsaved { kept: Err(_) })) => update(app, Msg::KeepOpen),
        _ => Task::none(),
    }
}

// ── wording ─────────────────────────────────────────────────────────────────

/// "Prune marks", "Prune and export marks", "Crops"…
fn what_phrase(kinds: &[Kind]) -> String {
    let marks: Vec<&str> = kinds
        .iter()
        .filter_map(|k| match k {
            Kind::Prune => Some("prune"),
            Kind::Export => Some("export"),
            _ => None,
        })
        .collect();
    let crops = kinds.contains(&Kind::Crops);
    let others = kinds.iter().any(|k| !matches!(k, Kind::Prune | Kind::Export | Kind::Crops));
    let s = match (marks.len(), crops) {
        (0, true) if !others => "Crops".to_string(),
        (0, _) => "Marks".to_string(),
        (_, false) if !others => format!("{} marks", marks.join(" and ")),
        _ => format!("{} marks and crops", marks.join(", ")),
    };
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Why things fail in `folder`, as far as is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// The share or disk is away.
    Away,
    /// The folder isn't where it was.
    Missing,
    /// It's there, but this failed anyway.
    Other,
}

/// The headline for trouble in `folder`, why, and when things will resume.
/// `opening`: the folder failed to open (else a save failed).
fn cause(app: &App, folder: &Path, error: Option<&SaveError>, opening: bool) -> (String, Cause, &'static str) {
    let path = util::elide_path(folder, 64);
    let probe = app.net.probe_of(folder);
    let share = probe.and_then(|p| p.share.as_ref());
    let reach = probe.map(|p| &p.reach);
    let away = |what: String| (format!("Can't reach {what}"), Cause::Away, "when it's back");
    match (share, reach) {
        (Some(s), Some(r)) if r.is_away() || (*r == Reach::Missing && !s.mounted) => away(s.name()),
        (Some(s), Some(Reach::Missing)) => (format!("Can't find {} on {}", quoted(folder), s.name()), Cause::Missing, "when the folder is back"),
        (None, Some(r)) if r.is_away() => away(path),
        (None, Some(Reach::Missing)) => (format!("Can't find the folder {path}"), Cause::Missing, "when the folder is back"),
        (_, Some(Reach::Failed(m))) => (format!("Can't open {path}: {}", plain(m)), Cause::Other, "once it can be opened"),
        _ => match error.map(|e| (netfs::classify_errno(e.errno), e)) {
            Some((Failure::Unreachable, _)) => away(share.map(|s| s.name()).unwrap_or(path)),
            Some((Failure::Missing, _)) => (format!("Can't find the folder {path}"), Cause::Missing, "when the folder is back"),
            Some((Failure::Other, e)) if opening => (format!("Can't open {}: {}", quoted(folder), plain(&e.message)), Cause::Other, "once it can be opened"),
            Some((Failure::Other, e)) => (format!("Can't save in {}: {}", quoted(folder), plain(&e.message)), Cause::Other, "once it can be written"),
            None => (format!("Can't open {path}"), Cause::Other, "when it's back"),
        },
    }
}

/// The banner's two lines, when there's trouble.
fn banner_text(app: &App) -> Option<(String, String)> {
    let net = &app.net;
    if let Some(first) = net.unsaved.iter().find(|u| Some(&u.folder) == app.folder.as_ref()).or(net.unsaved.first()) {
        let folder = first.folder.clone();
        let mine: Vec<&Unsaved> = net.unsaved.iter().filter(|u| u.folder == folder).collect();
        let kinds: Vec<Kind> = mine.iter().map(|u| u.kind).collect();
        let what = what_phrase(&kinds);
        let kept = if mine.iter().all(|u| u.kept_locally) { "kept on this computer" } else { "kept in the app" };
        let (head, _, when) = cause(app, &folder, Some(&first.error), false);
        let earlier = if mine.iter().all(|u| u.from_earlier) { " from the last session" } else { "" };
        let mut detail = format!("{what} for {}{earlier} are {kept} and will be saved {when}.", quoted(&folder));
        let mut others: Vec<&PathBuf> = net.unsaved.iter().map(|u| &u.folder).filter(|f| **f != folder).collect();
        others.dedup();
        if !others.is_empty() {
            let n = others.len();
            detail.push_str(&format!(" Marks for {n} other folder{} are waiting too.", util::s(n)));
        }
        return Some((head, detail));
    }
    if let Some(folder) = &net.stuck {
        let away = net.probe_of(folder).is_some_and(|p| !p.reach.is_ok());
        let (head, _, when) = cause(app, folder, None, false);
        return Some(if away {
            (head, format!("Marks for {} are kept in the app and will be saved {when}.", quoted(folder)))
        } else {
            ("Saving is taking a long time".to_string(), format!("Marks for {} are kept in the app until the save finishes.", quoted(folder)))
        });
    }
    let t = net.trouble.as_ref()?;
    let (head, why, when) = cause(app, &t.folder, t.error.as_ref(), t.what == Failed::Scan);
    let name = quoted(&t.folder);
    let detail = match (why, t.what) {
        (Cause::Missing, _) => "It may have been moved or renamed. Open it from its new place with Ctrl+O.".to_string(),
        (Cause::Other, Failed::Scan) => "Press Try again once that's fixed.".to_string(),
        (_, Failed::Scan) => format!("{name} will open {when}."),
        (_, Failed::Load) => format!("Photos in {name} can't be loaded until it's back."),
    };
    Some((head, detail))
}

// ── views ───────────────────────────────────────────────────────────────────

/// The banner under the folder bar while saves fail or the folder is away.
pub fn banner(app: &App) -> Option<Element<'_, Message>> {
    if app.net.dismissed {
        return None;
    }
    let (head, detail) = banner_text(app)?;
    let try_again = if app.net.trying {
        kit::btn("Trying…", style::secondary(), None)
    } else {
        kit::btn("Try again", style::secondary(), Some(Message::Net(Msg::TryAgain)))
    };
    let dismiss = kit::tip(
        kit::icon_btn(Glyph::Close, t::INK2, style::ghost(), Some(Message::Net(Msg::Dismiss))),
        "Hide this message (saving keeps trying)",
        true,
    );
    let words = column![
        text(head).size(t::T_BODY).font(paint::MEDIUM).color(t::INK),
        kit::note(detail),
    ]
    .spacing(2)
    .width(Length::Fill);
    let content = row![icons::view(Glyph::Warning, 18.0, t::WARN), words, try_again, dismiss]
        .spacing(t::S3)
        .align_y(Alignment::Center)
        .padding(Padding { top: t::S2, bottom: t::S2, left: 12.0, right: 4.0 });
    let strip = container(content).width(Length::Fill).style(style::fill(alpha(t::WARN, 0.09)));
    Some(column![strip, container(Space::new().width(Length::Fill).height(1)).style(style::fill(alpha(t::WARN, 0.35)))].into())
}

/// The viewer's window (also when the compare view has it) carries the
/// banner and the quit prompt too: it may be the only window in sight.
pub fn in_window<'a>(app: &'a App, content: Element<'a, Message>) -> Element<'a, Message> {
    let body: Element<'a, Message> = match banner(app) {
        Some(b) => column![b, content].width(Length::Fill).height(Length::Fill).into(),
        None => content,
    };
    match quit_overlay(app) {
        Some(prompt) => iced::widget::stack![body, prompt].width(Length::Fill).height(Length::Fill).into(),
        None => body,
    }
}

/// The network share `path` is on, by name, once it has been probed.
pub fn share_of(app: &App, path: &Path) -> Option<String> {
    app.net.probe_of(path)?.share.as_ref().map(|s| s.name())
}

/// The folder bar's mark for `path`: an icon, its colour and a tooltip line.
pub fn badge(app: &App, path: &Path, library: bool) -> Option<(Glyph, Color, String)> {
    let probe = app.net.probe_of(path);
    let unsaved = app.net.unsaved.iter().any(|u| u.folder == path) || app.net.stuck.as_deref() == Some(path);
    let trouble = app.net.trouble.as_ref().is_some_and(|t| t.folder == path);
    let share = probe.and_then(|p| p.share.as_ref());
    let bad = probe.is_some_and(|p| !p.reach.is_ok()) || unsaved || trouble;
    match share {
        Some(s) if !bad => {
            Some((Glyph::Network, t::INK3, format!("On a network share ({}). Opening and saving take longer here.", s.name())))
        }
        Some(s) => {
            let more = if unsaved {
                " Marks are kept until it's back."
            } else if library {
                " The calendar shows the library as it was last read."
            } else {
                ""
            };
            Some((Glyph::Network, t::WARN, format!("Can't reach {} right now.{more}", s.name())))
        }
        None if unsaved => Some((Glyph::Warning, t::WARN, "Marks for this folder aren't saved yet.".to_string())),
        None => None,
    }
}

/// The prompt shown when quitting with marks not saved.
pub fn quit_overlay(app: &App) -> Option<Element<'_, Message>> {
    let prompt = app.net.quit.as_ref()?;
    let mut folders: Vec<String> = app.net.unsaved.iter().map(|u| quoted(&u.folder)).collect();
    folders.dedup();
    let which = match (folders.is_empty(), &app.folder) {
        (false, _) => folders.join(", "),
        (true, Some(f)) => quoted(f),
        (true, None) => "this folder".to_string(),
    };
    let share = app
        .net
        .unsaved
        .first()
        .and_then(|u| app.net.probe_of(&u.folder))
        .and_then(|p| p.share.as_ref())
        .map(|s| s.name())
        .unwrap_or_else(|| "The folder".to_string());
    let (title, body, buttons): (String, String, Vec<Element<'_, Message>>) = match prompt {
        QuitPrompt::Saving { .. } => (
            "Saving marks…".into(),
            format!("Waiting a few seconds for the marks for {which} to be saved."),
            vec![kit::btn("Keep the app open", style::secondary(), Some(Message::Net(Msg::KeepOpen))).into()],
        ),
        QuitPrompt::Unsaved { kept: Ok(_) } => (
            "Marks aren't saved in the folder yet".into(),
            format!(
                "{share} can't be reached, so the latest marks for {which} couldn't be saved there. They're kept on this computer and will be saved to the folder the next time Photo Pipeline starts and can reach it."
            ),
            vec![
                kit::btn("Keep the app open", style::secondary(), Some(Message::Net(Msg::KeepOpen))).into(),
                kit::btn("Quit", style::primary(), Some(Message::Net(Msg::QuitAnyway))).into(),
            ],
        ),
        QuitPrompt::Unsaved { kept: Err(e) } => (
            "Quitting now loses marks".into(),
            format!(
                "{share} can't be reached, and the latest marks for {which} couldn't be kept on this computer either ({}). Keep the app open until the folder can be reached again.",
                plain(e)
            ),
            vec![
                kit::btn("Quit without saving", style::danger(), Some(Message::Net(Msg::QuitAnyway))).into(),
                kit::btn("Keep the app open", style::primary(), Some(Message::Net(Msg::KeepOpen))).into(),
            ],
        ),
    };
    let mut foot = row![Space::new().width(Length::Fill)].spacing(t::S2).align_y(Alignment::Center);
    for b in buttons {
        foot = foot.push(b);
    }
    let content = column![
        kit::title(title),
        text(body).size(t::T_BODY).color(t::INK2),
        Space::new().height(t::S2),
        foot,
    ]
    .spacing(t::S3);
    let w = (app.window_size_width() - 48.0).min(460.0);
    let frame = container(content).width(Length::Fixed(w)).padding(t::S5).style(style::floating());
    Some(opaque(Appear::new(container(frame).center(Length::Fill).padding(t::S5)).scrim(t::SCRIM).rise(14.0)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases() {
        assert_eq!(what_phrase(&[Kind::Prune]), "Prune marks");
        assert_eq!(what_phrase(&[Kind::Export, Kind::Prune]), "Export and prune marks");
        assert_eq!(what_phrase(&[Kind::Crops]), "Crops");
        assert_eq!(what_phrase(&[Kind::Prune, Kind::Crops]), "Prune marks and crops");
        assert_eq!(plain("No such file or directory (os error 2)"), "No such file or directory");
        assert_eq!(plain("odd"), "odd");
        assert_eq!(errno_in("No such file or directory (os error 2)"), Some(2));
        assert_eq!(errno_in("odd"), None);
    }
}
