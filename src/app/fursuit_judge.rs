//! The close-call judge as the pages show it (fursuit matching v2): what
//! it is doing while *Find more* and the "Same fursuit?" review wait on
//! it, and what it left out.
//!
//! - **A calm line, not a dialog.**  While close calls are checked, the
//!   review says so over its strip ("Checking 23 close calls…", with how
//!   far and ×), and the status bar does in any view; a judge waiting for
//!   the card or loading says that instead.
//! - **Suggestions as they land.**  What the judge offers joins the end of
//!   the review's list, nearest first, so nothing already on screen moves;
//!   each still needs the user's Y or N.
//! - **Left out, but countable.**  What it judged a different fursuit is
//!   counted quietly ("14 checked and left out") and can be shown: they
//!   join the end of the list, marked as left out, to answer like any
//!   other.
//! - **A failing card is an error**, with *Try the card again* and
//!   *Preferences*, never an offer of the processor.
//!
//! Nothing here names, merges or marks; it never touches people or pets.

use std::collections::HashSet;
use std::sync::Arc;

use iced::widget::{Space, button, column, container, progress_bar, row, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Task};

use super::kit::{self, tip};
use super::people::{Face, FaceRef, GroupId, Kind};
use super::style;
use super::subjects::{self as subj};
use super::{App, Message};
use crate::ml::judge::JudgeState;
use crate::ui::icons::{self, Glyph};
use crate::ui::paint;
use crate::ui::theme::{self as t, alpha};
use crate::util;

pub fn m(msg: Msg) -> Message {
    Message::Judge(msg)
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// × on the judge's line: stop checking (what was checked is kept).
    Stop,
    /// Show the close calls the judge left out, at the end of the review.
    ShowLeftOut,
    TryCard,
    OpenPrefs,
}

/// One request's close calls, as the judge's verdicts arrive.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub seq: u64,
    pub group: Option<GroupId>,
    /// Offered so far (entries).
    pub offered: usize,
    /// Left out so far, one entry per burst, nearest first.
    pub left_out: Vec<Vec<Face>>,
    pub done: usize,
    pub total: usize,
    /// × was pressed, or the judge failed: the rest wasn't checked.
    pub stopped: bool,
    /// The judge failed during it.
    pub dropped: bool,
}

impl Run {
    pub fn finished(&self) -> bool {
        self.stopped || self.done >= self.total
    }
}

/// A fursuit's close calls can be judged: a named fursuit, with a judge
/// chosen and in place.
pub fn judging(app: &App, group: &GroupId) -> bool {
    // Off, not downloaded, the processor chosen, or stopped: the request is
    // answered as with the judge off, and nothing waits for verdicts.
    active(&app.search.status) && app.subjects.group(group).is_some_and(|g| g.kind == Kind::Fursuit && g.name.is_some())
}

/// A request now would be judged: a judge chosen, in place and not
/// stopped, and the card chosen (the judge runs on the card only).
/// Otherwise the engine answers as with the judge off, and sends no
/// verdicts.
pub fn active(st: &crate::search::Status) -> bool {
    st.judge.usable() && st.device.chosen == crate::ml::Device::Gpu
}

/// The run for `group`, if one is under way or finished.
pub fn run_for<'a>(app: &'a App, group: &GroupId) -> Option<&'a Run> {
    app.subjects.judge.as_ref().filter(|r| r.group.as_ref() == Some(group))
}

// ── events ──────────────────────────────────────────────────────────────────

/// The judge's verdicts on a request's close calls: the review takes the
/// offered ones at the end of its list (and the left-out ones too, once
/// shown); the run counts them; the end is said once.
pub fn on_judged(app: &mut App, seq: u64, group: GroupId, offered: Arc<Vec<Vec<Face>>>, left_out: Arc<Vec<Vec<Face>>>, done: usize, total: usize) -> Task<Message> {
    // Fursuits only: a verdict about anything else is ignored.
    if app.subjects.group(&group).is_some_and(|g| g.kind != Kind::Fursuit) {
        return Task::none();
    }
    let run = app.subjects.judge.get_or_insert_with(Run::default);
    if run.seq != seq || run.group.as_ref() != Some(&group) {
        *run = Run { seq, group: Some(group.clone()), ..Run::default() };
    }
    if run.stopped {
        return Task::none();
    }
    run.offered += offered.len();
    run.left_out.extend(left_out.iter().cloned());
    run.done = done.max(run.done);
    run.total = total;
    let finished = run.done >= run.total;
    let (n_offered, n_left, n_total) = (run.offered, run.left_out.len(), run.total);

    let mut in_review = false;
    if let Some(r) = app.subjects.review.as_mut().filter(|r| r.group == group && r.seq == seq) {
        in_review = true;
        r.take(&offered, false);
        if r.left_out_shown {
            r.take(&left_out, true);
        }
        r.judge_done = finished;
        // Nothing sure and nothing offered, and the judge is done: there
        // is nothing to check.
        if finished && r.len() == 0 && n_left == 0 {
            app.subjects.review = None;
            let name = subj::label(&app.subjects, &group);
            return app.show_status(format!("No photos wait to be checked against {name} now."), 5000);
        }
    }
    let crops = subj::refresh_crops(app);
    if !finished || n_total == 0 || in_review {
        return crops;
    }
    // Asked from the fursuit's page (Find more): say what it came to.
    let name = subj::label(&app.subjects, &group);
    let text = format!(
        "Checked {} for {name}: {} may show {name}, {} left out.",
        util::plural(n_total as u64, "close call"),
        util::thousands(n_offered as u64),
        util::thousands(n_left as u64)
    );
    Task::batch([crops, app.show_status(text, 7000)])
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Stop => {
            app.search.link.stop_judging();
            if let Some(r) = app.subjects.judge.as_mut() {
                r.stopped = true;
            }
            if let Some(r) = app.subjects.review.as_mut() {
                r.judge_done = true;
            }
            app.show_status("Stopped checking close calls. Those checked are kept.", 5000)
        }
        Msg::ShowLeftOut => {
            let Some(r) = app.subjects.review.as_mut() else { return Task::none() };
            let Some(run) = app.subjects.judge.as_ref().filter(|x| x.group.as_ref() == Some(&r.group)) else { return Task::none() };
            r.left_out_shown = true;
            let first = r.len();
            let left = run.left_out.clone();
            r.take(&left, true);
            // Straight to the first of them (asked for, so the place moves).
            if r.len() > first {
                r.at = first;
            }
            subj::refresh_crops(app)
        }
        Msg::TryCard => {
            app.search.link.try_card_again();
            Task::none()
        }
        Msg::OpenPrefs => super::preferences::open_at(app, super::preferences::Control::Judge(app.settings.fursuit_judge)),
    }
}

// ── words ───────────────────────────────────────────────────────────────────

/// "14 checked and left out".
pub fn left_out_words(n: usize) -> String {
    format!("{} checked and left out", util::thousands(n as u64))
}

/// What holds the card, for "waiting for the card".
fn card_holder(app: &App) -> &'static str {
    let st = &app.search.status;
    if st.heads.scanning() {
        "the fursuit-head step is using it"
    } else if st.pass.is_some() {
        "indexing is using it"
    } else {
        "another step is using it"
    }
}

/// The judge's state in words for a line: (glyph tone, title, detail).
fn state_words(app: &App, run: Option<&Run>) -> Option<(iced::Color, String, String)> {
    let j = &app.search.status.judge;
    let total = run.map_or(0, |r| r.total);
    let calls = util::plural(total as u64, "close call");
    Some(match &j.state {
        JudgeState::WaitingForCard => (t::PHOTO_INK3, format!("{} wait for the graphics card", super::search_setup::capital(&calls)), format!("They're checked as soon as it's free: {}.", card_holder(app))),
        JudgeState::Loading => {
            let secs = crate::search::estimate::describe_seconds(super::fursuits::JUDGE_LOAD_SECONDS);
            (t::PHOTO_INK3, format!("Getting the close-call judge ready for {calls}"), format!("Putting it on the graphics card takes {secs} the first time."))
        }
        JudgeState::Judging { done, total, .. } => (
            t::PHOTO_INK2,
            format!("Checking {}…", util::plural(*total as u64, "close call")),
            format!("{} of {total}. Those the judge thinks are this fursuit join the end of the list.", util::thousands(*done as u64)),
        ),
        JudgeState::Failed(f) => {
            let (title, detail) = failure_words(f);
            (t::WARN, title, detail)
        }
        JudgeState::Broken(why) => (t::WARN, "The close-call judge couldn't run".to_string(), why.clone()),
        JudgeState::Idle => return None,
    })
}

/// A failing card, for the review's line: what stopped, then why, that
/// the suggestions are kept, and that nothing moves to the processor.
pub fn failure_words(f: &crate::ml::CardFailure) -> (String, String) {
    (
        f.title().replacen("Indexing stopped", "Checking close calls stopped", 1),
        format!("{} The suggestions here are kept. Nothing is moved to the processor.", f.detail().trim()),
    )
}

/// A judge's choice in Preferences, in plain words: its speed, where it
/// runs, its size.  `None` for Off.
pub fn choice_words(c: crate::ml::judge::JudgeChoice) -> Option<String> {
    let mi = crate::ml::models::info(c.model()?);
    Some(format!(
        "{} a close call, on the graphics card only. {}, downloaded once when you choose it.",
        super::search_setup::capital(&super::fursuits::call_seconds_words(super::fursuits::JUDGE_SECONDS_PER_CALL)),
        super::search_setup::capital(&crate::search::estimate::describe_bytes(mi.bytes()))
    ))
}

// ── the review's line ───────────────────────────────────────────────────────

fn bar<'a>(frac: f32, w: f32, ground: iced::Color) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0))
        .length(w)
        .girth(3)
        .style(move |_| progress_bar::Style { background: Background::Color(ground), bar: Background::Color(t::PHOTO_INK3), border: Border { radius: 2.0.into(), ..Border::default() } })
        .into()
}

fn stop_btn<'a>(ink: iced::Color) -> Element<'a, Message> {
    tip(
        button(container(icons::view(Glyph::Close, 11.0, ink)).center(Length::Fill)).width(20).height(20).padding(0).style(style::ghost().radius(4.0).style()).on_press(m(Msg::Stop)),
        "Stop checking close calls. Those checked are kept.",
        false,
    )
}

fn link_btn<'a>(label: impl Into<String>, msg: Message) -> Element<'a, Message> {
    button(text(label.into()).size(t::T_SMALL).font(paint::MEDIUM)).padding(Padding::from([1.0, 0.0])).style(style::link().style()).on_press(msg).into()
}

/// Over the review's strip, on the photos' grey: what the judge is doing
/// for this review, or what it came to, with *Show them* for what it left
/// out.  Nothing when no judge is on.
pub fn review_line<'a>(app: &'a App, group: &GroupId, seq: u64, left_out_shown: bool) -> Option<Element<'a, Message>> {
    if !judging(app, group) && run_for(app, group).is_none() {
        return None;
    }
    let run = run_for(app, group).filter(|r| r.seq == seq);
    let j = &app.search.status.judge;
    let ours = match &j.state {
        JudgeState::Judging { group: g, .. } => g == group,
        JudgeState::Idle => false,
        // Waiting, loading and failures are about the run in hand.
        JudgeState::Failed(_) | JudgeState::Broken(_) => live_shown(app, group, run),
        _ => run.is_some_and(|r| !r.finished()),
    };
    let mut r = row![].spacing(10).align_y(Alignment::Center);
    let live = if ours { state_words(app, run) } else { None };
    match live {
        Some((tone, title, detail)) => {
            let failed = matches!(j.state, JudgeState::Failed(_) | JudgeState::Broken(_));
            let glyph = match j.state {
                JudgeState::Failed(_) => Glyph::GraphicsCard,
                JudgeState::Broken(_) => Glyph::Warning,
                _ => Glyph::CloseCall,
            };
            r = r.push(icons::view(glyph, 16.0, tone));
            let title = text(title).size(t::T_SMALL).font(paint::MEDIUM).color(if failed { t::WARN } else { t::PHOTO_INK });
            let detail = text(detail).size(t::T_SMALL).color(t::PHOTO_INK3);
            // A failure takes two lines: what stopped, then why and what is
            // kept; the rest is one calm line.
            r = if failed { r.push(column![title, detail].spacing(1).width(Length::Fill)) } else { r.push(title).push(detail) };
            if let JudgeState::Judging { done, total, .. } = j.state {
                r = r.push(bar(if total == 0 { 0.0 } else { done as f32 / total as f32 }, 72.0, t::PHOTO_RAISED));
            }
            match &j.state {
                JudgeState::Failed(f) => {
                    r = r.push(Space::new().width(t::S1));
                    r = r.push(link_btn(super::search_setup::retry_label(f), m(Msg::TryCard)));
                    r = r.push(link_btn("Preferences", m(Msg::OpenPrefs)));
                }
                JudgeState::Broken(_) => r = r.push(link_btn("Preferences", m(Msg::OpenPrefs))),
                _ => r = r.push(stop_btn(t::PHOTO_INK2)),
            }
        }
        None => {
            // Finished (or nothing to check): what it came to.
            let Some(run) = run else { return None };
            if run.total == 0 && run.left_out.is_empty() {
                return None;
            }
            r = r.push(icons::view(Glyph::CloseCall, 16.0, t::PHOTO_INK3));
            let checked = if run.dropped {
                format!("Checked {} of {} before the judge stopped.", util::thousands(run.done as u64), util::plural(run.total as u64, "close call"))
            } else if run.stopped {
                format!("Stopped after {} of {}.", util::thousands(run.done as u64), util::plural(run.total as u64, "close call"))
            } else if run.total > 0 {
                format!("Checked {}: {} joined the list.", util::plural(run.total as u64, "close call"), util::thousands(run.offered as u64))
            } else {
                "Close calls were checked before.".to_string()
            };
            r = r.push(text(checked).size(t::T_SMALL).color(t::PHOTO_INK2));
        }
    }
    // The quiet count, and the way to see them.
    let failed = matches!(j.state, JudgeState::Failed(_) | JudgeState::Broken(_)) && live_shown(app, group, run);
    if let Some(run) = run.filter(|r| !r.left_out.is_empty()) {
        if !failed {
            r = r.push(Space::new().width(Length::Fill));
        }
        r = r.push(icons::view(Glyph::LeaveOut, 13.0, t::PHOTO_INK3));
        r = r.push(text(left_out_words(run.left_out.len())).size(t::T_SMALL).color(t::PHOTO_INK3));
        if !left_out_shown {
            r = r.push(tip(link_btn("Show them", m(Msg::ShowLeftOut)), "Add them to the end of this list, to check yourself", false));
        } else {
            r = r.push(text("shown at the end").size(t::T_SMALL).color(t::PHOTO_INK4));
        }
    }
    Some(container(r).padding(Padding { top: 6.0, bottom: 6.0, left: 18.0, right: 14.0 }).width(Length::Fill).into())
}

/// The judge's failure is shown for this run (it was under way).
fn live_shown(app: &App, group: &GroupId, run: Option<&Run>) -> bool {
    run_for(app, group).is_some() && run.is_some_and(|r| r.dropped || !r.finished())
}

/// The search status changed: a judge that failed drops the run in hand
/// (the engine does), so it is said as stopped, not as finished.
pub fn on_status(app: &mut App, _old: &crate::search::Status) -> Task<Message> {
    if matches!(app.search.status.judge.state, JudgeState::Failed(_) | JudgeState::Broken(_)) {
        if let Some(r) = app.subjects.judge.as_mut().filter(|r| !r.finished()) {
            r.stopped = true;
            r.dropped = true;
        }
        if let Some(r) = app.subjects.review.as_mut() {
            r.judge_done = true;
        }
    }
    Task::none()
}

/// Under a suggestion the judge brought in: where it came from, so the
/// answer can weigh it.  `None` for a sure one.
pub fn origin_words(r: &super::subjects_review::Review, face: &FaceRef) -> Option<&'static str> {
    if r.left_out_faces.contains(face) {
        Some("A close call the judge left out.")
    } else if r.judged_faces.contains(face) {
        Some("A close call the judge thinks is this fursuit.")
    } else {
        None
    }
}

/// The hollow tile at the end of the review's strip while close calls are
/// still being checked: how many are still to come.
pub fn strip_tail<'a>(app: &'a App, group: &GroupId, seq: u64, size: f32, shape: crate::ui::subject_tile::Shape) -> Option<Element<'a, Message>> {
    let run = run_for(app, group).filter(|r| r.seq == seq && !r.finished())?;
    let left = run.total.saturating_sub(run.done);
    let radius = shape.radius(size);
    let words = column![
        icons::view(Glyph::CloseCall, 16.0, t::PHOTO_INK3),
        text(util::thousands(left as u64)).size(t::T_SMALL).font(paint::MEDIUM).color(t::PHOTO_INK2),
    ]
    .spacing(1)
    .align_x(Alignment::Center);
    let tile = container(words)
        .width(size)
        .height(size)
        .center(size)
        .style(move |_| container::Style { border: Border { color: t::PHOTO_LINE, width: 1.0, radius: radius.into() }, background: Some(Background::Color(alpha(t::PHOTO_INK, 0.02))), ..Default::default() });
    Some(tip(container(tile).padding(2), format!("{} still to check", util::plural(left as u64, "close call")), false))
}

// ── the status bar ──────────────────────────────────────────────────────────

/// The status bar while the judge works, in any view: checking (how far,
/// with ×), loading, waiting for the card, or stopped.
pub fn status_slot(app: &App) -> Option<Element<'_, Message>> {
    let j = &app.search.status.judge;
    let words = |w: String, c: iced::Color| text(w).size(t::T_SMALL).color(c);
    let name = |g: &GroupId| subj::label(&app.subjects, g);
    let run = app.subjects.judge.as_ref().filter(|r| !r.finished());
    let content: Element<'_, Message> = match &j.state {
        JudgeState::Idle => return None,
        JudgeState::Judging { group, done, total } => row![
            icons::view(Glyph::CloseCall, 13.0, t::INK3),
            words(format!("Checking close calls for {}", name(group)), t::INK3),
            text(format!("{} of {}", util::thousands(*done as u64), util::thousands(*total as u64))).size(t::T_SMALL).font(paint::MEDIUM).color(t::INK),
            status_bar(if *total == 0 { 0.0 } else { *done as f32 / *total as f32 }),
            stop_btn(t::INK2),
        ]
        .spacing(t::S2)
        .align_y(Alignment::Center)
        .into(),
        JudgeState::WaitingForCard | JudgeState::Loading => {
            run?;
            let what = if j.state == JudgeState::Loading { "Getting the close-call judge ready" } else { "Close calls wait for the graphics card" };
            row![icons::view(Glyph::CloseCall, 13.0, t::INK3), words(what.into(), t::INK3), stop_btn(t::INK2)].spacing(t::S2).align_y(Alignment::Center).into()
        }
        JudgeState::Failed(f) => tip(
            row![icons::view(Glyph::GraphicsCard, 13.0, t::WARN), words("Checking close calls stopped".into(), t::WARN)].spacing(t::S2).align_y(Alignment::Center),
            format!("{} {}", f.title().replacen("Indexing stopped: ", "", 1), f.detail()),
            false,
        ),
        JudgeState::Broken(why) => tip(row![icons::view(Glyph::Warning, 13.0, t::WARN), words("The close-call judge couldn't run".into(), t::WARN)].spacing(t::S2).align_y(Alignment::Center), why.clone(), false),
    };
    Some(row![content, Space::new().width(t::S3), kit::vrule(12.0), Space::new().width(t::S3)].align_y(Alignment::Center).into())
}

fn status_bar<'a>(frac: f32) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0))
        .length(56)
        .girth(4)
        .style(|_| progress_bar::Style { background: Background::Color(t::RAISED), bar: Background::Color(t::SIGNAL), border: Border { radius: 2.0.into(), ..Border::default() } })
        .into()
}

/// The fursuit's band: the quiet count of what was left out, after the
/// suggestions ("; 14 checked and left out").
pub fn band_words(app: &App, group: &GroupId) -> Option<String> {
    let run = run_for(app, group)?;
    (!run.left_out.is_empty()).then(|| format!(" {}.", super::search_setup::capital(&left_out_words(run.left_out.len()))))
}

/// The review's first list came back empty: with a judge on it isn't the
/// end, since close calls may still be offered.
pub fn keeps_review_open(app: &App, group: &GroupId) -> bool {
    judging(app, group)
}

/// One line for the test driver.
pub fn debug_state(app: &App) -> String {
    let j = &app.search.status.judge;
    let run = app.subjects.judge.as_ref().map(|r| format!("seq={} offered={} left_out={} done={}/{} stopped={}", r.seq, r.offered, r.left_out.len(), r.done, r.total, r.stopped));
    format!("judge=[choice={:?} downloaded={:?} loaded={} state={:?} run={}]", j.choice, j.downloaded, j.loaded, j.state, run.unwrap_or_else(|| "none".into()))
}

/// Faces of entries, by their first frame (what the review dedupes on).
pub fn firsts(list: &[Vec<Face>]) -> HashSet<FaceRef> {
    list.iter().filter_map(|e| e.first()).map(|f| f.face).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::CardFailure;
    use crate::ml::judge::JudgeChoice;

    #[test]
    fn the_choice_says_its_speed_size_and_where_it_runs() {
        assert_eq!(choice_words(JudgeChoice::Off), None, "Off costs nothing to say");
        let w = choice_words(JudgeChoice::Gemma4).unwrap();
        assert!(w.starts_with("About 2 seconds a close call"), "{w}");
        assert!(w.contains("About 18 GB"), "{w}");
        assert!(w.contains("graphics card only"), "{w}");
        assert!(!w.contains("processor"), "never offers the processor: {w}");
        assert_eq!(JudgeChoice::default(), JudgeChoice::Off, "off by default");
        assert_eq!(JudgeChoice::ALL[0], JudgeChoice::Off, "Off heads the list");
    }

    #[test]
    fn a_failing_card_is_an_error_never_a_way_round() {
        for f in [CardFailure::NoCard, CardFailure::Crashed, CardFailure::Failed("out of memory".into()), CardFailure::SelfCheckFailed { cosine: 0.5, max_diff: 0.9 }] {
            let (title, detail) = failure_words(&f);
            assert!(title.starts_with("Checking close calls stopped"), "{title}");
            assert!(detail.ends_with("Nothing is moved to the processor."), "{detail}");
            assert!(detail.contains("kept"), "what it found stays: {detail}");
            assert!(!detail.contains("choose it") && !detail.contains("instead"), "no offer of the processor: {detail}");
        }
        assert_eq!(left_out_words(14), "14 checked and left out");
        assert_eq!(left_out_words(1_200), "1,200 checked and left out");
    }

    #[test]
    fn a_run_is_finished_when_all_are_checked_or_it_stopped() {
        let mut r = Run { total: 3, ..Run::default() };
        assert!(!r.finished());
        r.done = 3;
        assert!(r.finished());
        let r = Run { total: 3, done: 1, stopped: true, ..Run::default() };
        assert!(r.finished(), "× ends it");
        assert!(Run::default().finished(), "nothing to check");
    }
}
