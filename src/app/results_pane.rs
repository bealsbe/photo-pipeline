//! A list of library photos on the photos' grey, read-only: the search
//! page's results and a person's photos share it.  It holds the results,
//! their tiles (bursts stacked at the rank of their best frame, opened with
//! G or a tab, Shift+G every one), the selection, the scroll and the
//! thumbnails, and draws them in the grid of a library day with each
//! photo's date under it.  What needs the app (the viewer, quick look, the
//! status bar) its owner does, from what [`Pane::on_grid`] answers.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::NaiveDate;
use iced::Element;

use super::search_results::{self as res, Layout, Results};
use crate::imaging::thumbs::{OwnerId, PRIORITY_PREFETCH, PRIORITY_VISIBLE, ThumbCache, ThumbWorkers};
use crate::model::bursts::RunMark;
use crate::model::collection::RecordId;
use crate::ui::grid::{self, CellLabel, Grid, GridData, GridEvent, Restack, Section};

/// What a grid event asks of the pane's owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Nothing,
    /// Open the viewer on this photo.
    Viewer(RecordId),
    /// Stack or open this photo's burst (a tab was clicked).
    Burst(RecordId),
    /// *Show fursuits too* (true) or *Hide fursuits* (false).
    Fursuits(bool),
}

pub struct Pane {
    pub results: Option<Arc<Results>>,
    /// The fursuits an animal word left out are shown, as their own list.
    pub fursuits_shown: bool,
    pub layout: Layout,
    /// Bursts stacked unless toggled (G, a tab); Shift+G flips them all.
    stack_all: bool,
    flipped: HashSet<u64>,
    pub selection: HashSet<RecordId>,
    pub anchor: Option<RecordId>,
    pub collapsed: HashSet<NaiveDate>,
    pub scroll: f32,
    pub scroll_seq: u64,
    pub reveal: (u64, Option<RecordId>),
    pub key: u64,
    before: (Vec<Section>, Vec<Vec<Option<RunMark>>>),
    restack_seq: u64,
    restack_focus: Option<(RecordId, RecordId)>,
    pub thumbs: ThumbCache,
    pub thumb_size: f32,
    /// The line after the last result, and a quieter one under it.
    pub end_line: String,
    end_quiet: String,
    /// "12 fursuits that look like dogs are left out.", while they are.
    pub left_out_line: String,
}

impl Pane {
    pub fn new(owner: OwnerId, cache_bytes: usize) -> Pane {
        Pane {
            results: None,
            fursuits_shown: false,
            layout: Layout::default(),
            stack_all: true,
            flipped: HashSet::new(),
            selection: HashSet::new(),
            anchor: None,
            collapsed: HashSet::new(),
            scroll: 0.0,
            scroll_seq: 0,
            reveal: (0, None),
            key: 0,
            before: (Vec::new(), Vec::new()),
            restack_seq: 0,
            restack_focus: None,
            thumbs: ThumbCache::new(owner, cache_bytes),
            thumb_size: 180.0,
            end_line: String::new(),
            end_quiet: String::new(),
            left_out_line: String::new(),
        }
    }

    /// No results: an empty page.
    pub fn clear(&mut self) {
        self.results = None;
        self.layout = Layout::default();
        self.selection.clear();
        self.anchor = None;
    }

    /// The focused result: the one last clicked or stepped to, while
    /// selected, else a lone selected one.
    pub fn focus(&self) -> Option<RecordId> {
        let sel = &self.selection;
        self.anchor.filter(|a| sel.contains(a)).or_else(|| if sel.len() == 1 { sel.iter().next().copied() } else { None })
    }

    /// Bursts are stacked (Shift+G's state on the page).
    pub fn all_stacked(&self) -> bool {
        self.stack_all
    }

    /// The focused result is in a burst (stacked or open).
    pub fn focused_burst(&self) -> bool {
        self.focus().is_some_and(|id| self.layout.frames_of(id).is_some())
    }

    fn stacked(&self, key: u64) -> bool {
        self.stack_all != self.flipped.contains(&key)
    }

    /// Photos found (frames inside stacks too), the fursuits left out not
    /// counted.
    pub fn photos(&self) -> usize {
        self.results.as_ref().map_or(0, |r| r.found.len())
    }

    /// The day of the focused result.
    pub fn focused_day(&self) -> Option<NaiveDate> {
        self.focus().and_then(|id| self.results.as_ref()?.day_of(id))
    }

    /// Lay the results out again (bursts stacked or opened, new results),
    /// keeping the selection.  `animate` slides the tiles to their new
    /// places, holding `focus` still (else the middle row).
    pub fn relayout(&mut self, animate: bool, focus: Option<RecordId>) {
        let Some(r) = self.results.clone() else {
            self.layout = Layout::default();
            return;
        };
        let (end, quiet) = res::end_lines(&r);
        (self.end_line, self.end_quiet) = (end, quiet.to_string());
        self.left_out_line = if r.fursuits.is_empty() { String::new() } else { res::fursuits_left_out(r.fursuits.len(), r.animal.as_deref()) };
        let new = res::layout(&res::parts(&r, self.fursuits_shown), |k| self.stacked(k));
        let old = std::mem::replace(&mut self.layout, new);
        if animate {
            self.before = (old.sections, old.runs);
            self.restack_seq += 1;
            self.restack_focus = focus.and_then(|a| Some((a, self.layout.tile_of(a)?)));
        }
        let (sel, anchor) = res::regather(&self.layout, &self.selection, self.anchor);
        self.selection = sel;
        self.anchor = anchor;
        self.key = self.key.wrapping_add(1);
    }

    /// New results.  A `refresh` (the same search run again) keeps the
    /// selection, place and open bursts; a new search starts at the top
    /// with its first result selected.
    pub fn set_results(&mut self, r: Arc<Results>, refresh: bool, workers: &ThumbWorkers) {
        // Selection by file, so a refresh (new records) keeps it.
        let old = self.results.take();
        let path_of = |id: RecordId| old.as_ref().and_then(|o| o.coll.get(id)).map(|x| x.path.clone());
        let kept: Vec<_> = self.selection.iter().filter_map(|&id| path_of(id)).collect();
        let anchor = self.anchor.and_then(path_of);
        self.results = Some(r.clone());
        self.selection = kept.iter().filter_map(|p| r.coll.id_of(p)).collect();
        self.anchor = anchor.and_then(|p| r.coll.id_of(&p));
        if !refresh {
            self.fursuits_shown = false;
            self.flipped.clear();
            self.collapsed.clear();
            self.selection.clear();
            self.anchor = None;
            self.scroll = 0.0;
            self.scroll_seq += 1;
            self.thumbs.clear_queue(workers);
        }
        self.relayout(false, None);
        if !refresh {
            if let Some(&first) = self.layout.tiles.first() {
                self.selection = [first].into();
                self.anchor = Some(first);
            }
        }
    }

    /// *Show fursuits too* or *Hide fursuits*.  False when nothing changed.
    pub fn show_fursuits(&mut self, on: bool) -> bool {
        if self.fursuits_shown == on || self.results.as_ref().is_none_or(|r| r.fursuits.is_empty()) {
            return false;
        }
        self.fursuits_shown = on;
        let focus = self.focus();
        self.relayout(false, focus);
        // Shown, their list starts where the note was: bring it into view.
        if on {
            let first = self.layout.sections.get(1).and_then(|sec| sec.ids.first().copied());
            self.reveal = (self.reveal.0 + 1, first);
        }
        true
    }

    /// A grid event: what the pane does itself, and what it asks of its
    /// owner.
    pub fn on_grid(&mut self, ev: GridEvent, workers: &ThumbWorkers) -> Ask {
        match ev {
            GridEvent::Click { id, modifiers } => {
                if modifiers.control() {
                    if !self.selection.remove(&id) {
                        self.selection.insert(id);
                    }
                } else {
                    self.selection = [id].into();
                }
                self.anchor = Some(id);
                Ask::Nothing
            }
            GridEvent::Activate(id) => Ask::Viewer(id),
            GridEvent::ClearSelection => {
                self.selection.clear();
                Ask::Nothing
            }
            GridEvent::ToggleBurst(id) => Ask::Burst(id),
            GridEvent::ToggleCollapse(d) => {
                if !self.collapsed.remove(&d) {
                    self.collapsed.insert(d);
                }
                Ask::Nothing
            }
            GridEvent::Chip(_) | GridEvent::SelectDone => Ask::Nothing,
            // *Hide fursuits* on their list's heading; *Show fursuits too*
            // in the note under the results.
            GridEvent::SectionLink(d) if d == res::FURSUITS_DAY => Ask::Fursuits(false),
            GridEvent::SectionLink(_) => Ask::Nothing,
            GridEvent::FooterLink => Ask::Fursuits(true),
            GridEvent::ZoomStep { step, width } => {
                self.thumb_size = grid::zoom_step(width, self.thumb_size, step);
                Ask::Nothing
            }
            GridEvent::PinchTo(size) => {
                self.thumb_size = size.clamp(grid::MIN_SIZE, grid::MAX_SIZE);
                Ask::Nothing
            }
            GridEvent::Thumbs { visible, nearby, settle } => {
                let Some(r) = self.results.clone() else { return Ask::Nothing };
                if settle {
                    self.thumbs.clear_queue(workers);
                }
                // What is on screen and near it is kept, so the thumbnails
                // arriving for the screens around never push it out.
                let paths: Vec<_> = visible.iter().chain(&nearby).filter_map(|id| r.coll.get(*id)).map(|rec| rec.path.clone()).collect();
                self.thumbs.hold(paths, settle);
                for (ids, prio) in [(visible, PRIORITY_VISIBLE), (nearby, PRIORITY_PREFETCH)] {
                    for id in ids {
                        if let Some(rec) = r.coll.get(id) {
                            self.thumbs.request(workers, rec, prio);
                        }
                    }
                }
                Ask::Nothing
            }
            GridEvent::Scrolled(y) => {
                self.scroll = y;
                Ask::Nothing
            }
        }
    }

    /// The frames the viewer steps through from `id` (its own list's), and
    /// where `id` is among them.
    pub fn viewer_items(&self, id: RecordId) -> (Vec<RecordId>, usize) {
        let items = self.layout.visible_with(id).to_vec();
        let index = items.iter().position(|&x| x == id).unwrap_or(0);
        (items, index)
    }

    /// Stack the burst of `id`, or open it.  `Err` with the photo's name
    /// when it isn't part of a burst.
    pub fn toggle_burst(&mut self, id: RecordId) -> Result<(), String> {
        let Some(g) = self.layout.group_of(id) else { return Ok(()) };
        let Some(key) = self.layout.keys[g].filter(|_| self.layout.groups[g].len() > 1) else {
            return Err(self.results.as_ref().and_then(|r| r.coll.get(id)).map(|r| r.stem()).unwrap_or_default());
        };
        if !self.flipped.remove(&key) {
            self.flipped.insert(key);
        }
        self.relayout(true, Some(id));
        Ok(())
    }

    /// Shift+G: every burst stacked, or every frame shown (for this list
    /// only).  The status line to show.
    pub fn toggle_all(&mut self) -> String {
        self.stack_all = !self.stack_all;
        self.flipped.clear();
        let anchor = self.focus();
        self.relayout(true, anchor);
        let n = self.layout.groups.iter().filter(|g| g.len() > 1).count();
        match (self.stack_all, n) {
            (_, 0) => "No bursts in these results.".to_string(),
            (true, n) => format!("{} stacked. G opens the selected one.", crate::util::plural(n as u64, "burst")),
            (false, _) => "Every frame of every burst shows.".to_string(),
        }
    }

    /// Move the selection to `id` and bring it into view.
    pub fn select(&mut self, id: RecordId) {
        self.selection = [id].into();
        self.anchor = Some(id);
        self.reveal = (self.reveal.0 + 1, Some(id));
    }

    /// ← or →: the next or previous tile.
    pub fn step(&mut self, forward: bool) {
        if let Some(next) = res::step(&self.layout.tiles, self.anchor, forward) {
            self.select(next);
        }
    }

    /// The grid, read-only, each photo's date under it.  `whole`: search
    /// sees the whole library, so the end note says this is everything.
    /// `links` puts a link on a section's heading.
    pub fn view<'a, M: 'a>(&'a self, scale_factor: f32, keyboard: bool, whole: bool, links: &'a [(NaiveDate, &'a str)], on: impl Fn(GridEvent) -> M + 'a) -> Element<'a, M> {
        let Some(r) = self.results.as_ref() else { return iced::widget::Space::new().into() };
        // The end note: the fursuits left out (with the way to show them),
        // then that this is everything, once search sees the whole library.
        let note = (!r.fursuits.is_empty() && !self.fursuits_shown).then_some((self.left_out_line.as_str(), "Show fursuits too"));
        let end = whole.then_some((self.end_line.as_str(), self.end_quiet.as_str()));
        let footer = (note.is_some() || end.is_some()).then_some(grid::Footer { note, end });
        let data = GridData {
            sections: &self.layout.sections,
            coll: &r.coll,
            thumbs: &self.thumbs,
            selection: &self.selection,
            collapsed: &self.collapsed,
            select_mode: false,
            scale_factor,
            show_names: true,
            bursts: &self.layout.runs,
            moments: &[],
            hints: None,
            restack: Some(Restack { seq: self.restack_seq, sections: &self.before.0, bursts: &self.before.1, focus: self.restack_focus }),
            label: CellLabel::Date,
            footer,
            links,
        };
        Grid::new(data, self.thumb_size, on).keyboard(keyboard).initial_scroll(self.scroll).scroll_seq(self.scroll_seq).layout_key(self.key).reveal(self.reveal.0, self.reveal.1).into()
    }
}
