//! "Advance after marking" (a setting, off by default): in the viewer, a P
//! or M key press that *sets* a mark moves on to the next photo after a
//! short beat, so each decision costs one key.  The beat lets the mark show
//! first: the toolbar toggle lights and the filmstrip cell is marked.  A
//! stamp over the photo says what was done while the next one slides in.
//! Clearing a mark stays put, and so does the last photo.

use std::time::{Duration, Instant};

use iced::Task;

use super::viewer::{self, Msg, Viewer};
use super::{App, Message};
use crate::model::collection::RecordId;
use crate::ui::icons::Glyph;
use crate::ui::stamp::Stamp;
use crate::ui::theme as t;

/// How long a new mark shows before the next photo slides in.
pub const BEAT_MS: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Prune,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    seq: u64,
    record: RecordId,
    mark: Mark,
}

/// One viewer's auto-advance state.
#[derive(Debug, Default)]
pub struct Flow {
    /// A move to the next photo waiting out the beat.
    pending: Option<Pending>,
    seq: u64,
    /// The last mark that started a move, and when (for the stamp).
    stamp: Option<(Mark, Instant)>,
}

impl Flow {
    /// A key press toggled `mark` on `record`.  Returns the beat to wait
    /// for, if the viewer should move on after it.
    fn marked(&mut self, record: RecordId, mark: Mark, set: bool, advance: bool, last: bool, now: Instant) -> Option<u64> {
        if !set {
            // Cleared: stay, and drop a move this record's mark had started.
            if self.pending.is_some_and(|p| p.record == record) {
                self.pending = None;
                self.stamp = None;
            }
            return None;
        }
        if !advance || last {
            return None;
        }
        self.seq += 1;
        self.pending = Some(Pending { seq: self.seq, record, mark });
        self.stamp = Some((mark, now));
        Some(self.seq)
    }

    /// Beat `seq` is over.  Returns true to move on: the move is still the
    /// latest, the viewer is still on its photo and the mark is still set.
    fn beat(&mut self, seq: u64, current: Option<RecordId>, still_set: impl Fn(RecordId, Mark) -> bool) -> bool {
        let Some(p) = self.pending.filter(|p| p.seq == seq) else { return false };
        self.pending = None;
        current == Some(p.record) && still_set(p.record, p.mark)
    }

    /// A move is waiting (the prune banner holds back meanwhile, so the
    /// layout doesn't jump twice in 150 ms).
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// The user moved on (or the setting went off): forget a waiting move.
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

fn is_set(app: &App, id: RecordId, mark: Mark) -> bool {
    app.coll.get(id).is_some_and(|r| match mark {
        Mark::Prune => r.is_pruned,
        Mark::Keep => r.is_marked,
    })
}

/// P (or Delete) and M in the viewer.
pub fn mark_key(app: &mut App, mark: Mark) -> Task<Message> {
    let toggle = viewer::update(
        app,
        match mark {
            Mark::Prune => Msg::TogglePrune,
            Mark::Keep => Msg::ToggleMark,
        },
    );
    let Some(v) = app.viewer.as_ref() else { return toggle };
    let Some(id) = v.record_id().filter(|_| !v.read_only) else { return toggle };
    let set = is_set(app, id, mark);
    let last = v.index + 1 >= v.items.len();
    let advance = app.settings.viewer_advance;
    let Some(v) = app.viewer.as_mut() else { return toggle };
    match v.flow.marked(id, mark, set, advance, last, Instant::now()) {
        Some(seq) => {
            let beat = Task::perform(tokio::time::sleep(Duration::from_millis(BEAT_MS)), move |_| Message::Viewer(Msg::Advance(seq)));
            Task::batch([toggle, beat])
        }
        None => toggle,
    }
}

/// The beat is over: move on, unless something changed meanwhile.
pub fn on_beat(app: &mut App, seq: u64) -> Task<Message> {
    let Some(v) = app.viewer.as_mut() else { return Task::none() };
    let (index, current) = (v.index, v.record_id());
    let mut flow = std::mem::take(&mut v.flow);
    let go = flow.beat(seq, current, |id, mark| is_set(app, id, mark));
    if let Some(v) = app.viewer.as_mut() {
        v.flow = flow;
    }
    if go { viewer::navigate(app, index + 1, 1) } else { Task::none() }
}

/// The setting: the command (Ctrl+Shift+A, in either window) or the
/// viewer's toggle.
pub fn toggle(app: &mut App) -> Task<Message> {
    let on = !app.settings.viewer_advance;
    app.settings.viewer_advance = on;
    if let Some(v) = app.viewer.as_mut() {
        v.flow.cancel();
    }
    app.settings.save_in_background();
    let text = if on {
        "Advance after marking is on: P or M in the viewer moves on to the next photo"
    } else {
        "Advance after marking is off: the viewer stays on a photo you mark"
    };
    app.show_status(text, 5000)
}

/// What the stamp over the photo shows.
pub fn stamp(v: &Viewer) -> Stamp {
    match v.flow.stamp {
        Some((Mark::Prune, at)) => Stamp::new(Some(at), Glyph::Prune, "Marked to prune", t::REJECT),
        Some((Mark::Keep, at)) => Stamp::new(Some(at), Glyph::Keep, "Marked for export", t::KEEP),
        None => Stamp::new(None, Glyph::Keep, "", t::KEEP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always(_: RecordId, _: Mark) -> bool {
        true
    }

    #[test]
    fn setting_a_mark_moves_on_after_the_beat() {
        let mut f = Flow::default();
        let now = Instant::now();
        let seq = f.marked(7, Mark::Prune, true, true, false, now).expect("a beat");
        assert!(f.waiting());
        assert_eq!(f.stamp.map(|s| s.0), Some(Mark::Prune));
        assert!(f.beat(seq, Some(7), always));
        assert!(!f.waiting());
        // A beat only fires once.
        assert!(!f.beat(seq, Some(7), always));
    }

    #[test]
    fn off_last_photo_or_clearing_stays_put() {
        let now = Instant::now();
        let mut f = Flow::default();
        assert_eq!(f.marked(1, Mark::Keep, true, false, false, now), None, "setting off");
        assert_eq!(f.marked(1, Mark::Keep, true, true, true, now), None, "last photo");
        assert_eq!(f.marked(1, Mark::Keep, false, true, false, now), None, "cleared");
        assert!(!f.waiting());
    }

    #[test]
    fn clearing_during_the_beat_cancels_the_move() {
        let now = Instant::now();
        let mut f = Flow::default();
        let seq = f.marked(3, Mark::Prune, true, true, false, now).expect("a beat");
        // P again: the mark is cleared before the beat ends.
        assert_eq!(f.marked(3, Mark::Prune, false, true, false, now), None);
        assert!(f.stamp.is_none());
        assert!(!f.beat(seq, Some(3), always));
    }

    #[test]
    fn navigating_or_a_newer_mark_supersedes_a_beat() {
        let now = Instant::now();
        let mut f = Flow::default();
        let first = f.marked(3, Mark::Keep, true, true, false, now).expect("a beat");
        let second = f.marked(3, Mark::Prune, true, true, false, now).expect("a beat");
        assert!(!f.beat(first, Some(3), always), "superseded");
        assert!(f.beat(second, Some(3), always));

        let seq = f.marked(4, Mark::Keep, true, true, false, now).expect("a beat");
        f.cancel();
        assert!(!f.beat(seq, Some(4), always), "cancelled by navigation");

        let seq = f.marked(5, Mark::Keep, true, true, false, now).expect("a beat");
        assert!(!f.beat(seq, Some(6), always), "already on another photo");
    }

    #[test]
    fn a_mark_cleared_elsewhere_stays_put() {
        let now = Instant::now();
        let mut f = Flow::default();
        let seq = f.marked(9, Mark::Prune, true, true, false, now).expect("a beat");
        assert!(!f.beat(seq, Some(9), |_, _| false));
    }
}
