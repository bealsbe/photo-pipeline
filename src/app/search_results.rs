//! Search results as the page holds them: the records of a query's hits
//! (built on a worker from the library index the calendar already holds,
//! reading no file), their tiles in one best-first list with each burst
//! stacked at the rank of its best frame (and the fursuits an animal word
//! left out in a list of their own, when shown), the completions offered
//! while typing, and the recent searches.  Pure, so it is tested without
//! a window.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime};

use crate::model::bursts::RunMark;
use crate::model::collection::{Collection, RecordId};
use crate::model::library_index::FileEntry;
use crate::search::parse::DateRange;
use crate::search::{Hit, Parsed, Thing, parse};
use crate::ui::grid::Section;

/// The results' sections have no day: these stand in for their dates (the
/// grid keys a section's collapse, and its header's link, by date).
pub const RESULTS_DAY: NaiveDate = NaiveDate::MIN;
/// The fursuits an animal word left out, when shown: a list of their own,
/// after the real animals, never mixed into theirs.
pub const FURSUITS_DAY: NaiveDate = NaiveDate::MAX;
/// Burst keys in the fursuits' list are kept apart from the main list's.
const FURSUITS_SALT: u64 = 0x5eed_f005_0000_0000;

/// One result shot.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The record its tile shows: the JPG of a pair, else the file.
    pub id: RecordId,
    pub score: f32,
    pub when: NaiveDateTime,
    pub day: NaiveDate,
    pub burst: Option<u64>,
}

/// A query's results.
pub struct Results {
    pub seq: u64,
    /// The words searched for, as typed.
    pub words: String,
    /// Every hit's files (both halves of a pair), paired; the fursuits'
    /// too.
    pub coll: Arc<Collection>,
    /// Best first.
    pub found: Vec<Found>,
    /// The fursuits an animal word left out (decision 12b), best first:
    /// never in `found`, shown only as a list of their own.
    pub fursuits: Vec<Found>,
    /// The animal word that left them out, singular ("dog").
    pub animal: Option<String>,
    /// What the engine understood of the words, and applied.
    pub understood: Parsed,
    /// Hits whose files the library index doesn't hold (moved since).
    pub missing: usize,
}

impl std::fmt::Debug for Results {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Results {{ seq: {}, words: {:?}, found: {}, fursuits: {}, animal: {:?}, missing: {} }}",
            self.seq,
            self.words,
            self.found.len(),
            self.fursuits.len(),
            self.animal,
            self.missing
        )
    }
}

impl Results {
    /// The day of the photo `id` shows (either half of a pair).
    pub fn day_of(&self, id: RecordId) -> Option<NaiveDate> {
        let tile = |x: RecordId| self.found.iter().chain(&self.fursuits).find(|f| f.id == x).map(|f| f.day);
        tile(id).or_else(|| self.coll.find_pair(id).and_then(tile))
    }

    /// What the results look like, in words, for the end note and the empty
    /// page: the words searched for, without the dates and words left out
    /// (which the page states on their own).
    pub fn looks_like(&self) -> &str {
        let w = self.understood.words.trim();
        if w.is_empty() { self.words.trim() } else { w }
    }
}

/// What came with the hits (the stand-in's `Extra`, or the engine's
/// answer).
#[derive(Default)]
pub struct Answer<'a> {
    pub fursuits: &'a [Hit],
    pub animal: Option<String>,
    pub understood: Parsed,
}

/// Build the records for `hits` (and the fursuits left out): each file is
/// looked up in the library index by its day and path (`lookup`), so
/// nothing is read from disk.  The capture time is the engine's, to the
/// sub-second.
pub fn build(seq: u64, words: String, hits: &[Hit], answer: Answer<'_>, lookup: impl Fn(NaiveDate, &Path) -> Option<FileEntry>) -> Results {
    let mut coll = Collection::new();
    let mut missing = 0;
    let mut take = |hits: &[Hit]| -> Vec<Found> {
        let mut found = Vec::with_capacity(hits.len());
        for h in hits {
            let mut add = |path: Option<&Path>| -> Option<RecordId> {
                let entry = lookup(h.day, path?)?;
                let mut r = entry.record();
                r.capture_time = Some(h.capture);
                Some(coll.add(r))
            };
            let jpg = add(h.jpg.as_deref());
            let raw = add(h.raw.as_deref());
            match jpg.or(raw) {
                Some(id) => found.push(Found { id, score: h.score, when: h.capture, day: h.day, burst: h.burst }),
                None => missing += 1,
            }
        }
        found
    };
    let found = take(hits);
    let fursuits = take(answer.fursuits);
    coll.build_pairs();
    let animal = answer.animal.filter(|_| !fursuits.is_empty());
    Results { seq, words, coll: Arc::new(coll), found, fursuits, animal, understood: answer.understood, missing }
}

/// What the grid shows of the results.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub sections: Vec<Section>,
    pub runs: Vec<Vec<Option<RunMark>>>,
    /// The tiles in order (a stack is one), every section's.
    pub tiles: Vec<RecordId>,
    /// Per section, every frame, stacks opened in place: what the viewer
    /// steps through (one section's, never the next one's).
    pub visible: Vec<Vec<RecordId>>,
    /// Each burst's frames in shooting order, by its group number (the
    /// grid's burst id).
    pub groups: Vec<Vec<RecordId>>,
    /// The burst key of each group.
    pub keys: Vec<Option<u64>>,
}

/// One list of the results: its photos, best first, and its heading.
pub struct Part<'a> {
    pub found: &'a [Found],
    pub date: NaiveDate,
    pub label: String,
}

impl Layout {
    /// The frames the viewer steps through from `id`: its own section's.
    pub fn visible_with(&self, id: RecordId) -> &[RecordId] {
        self.visible.iter().find(|v| v.contains(&id)).or(self.visible.first()).map_or(&[], Vec::as_slice)
    }

    /// The group `id` (a frame or a cover) is in.
    pub fn group_of(&self, id: RecordId) -> Option<usize> {
        self.groups.iter().position(|g| g.contains(&id))
    }

    /// The tile showing `id`: itself, or the stack it is folded into.
    pub fn tile_of(&self, id: RecordId) -> Option<RecordId> {
        if self.tiles.contains(&id) {
            return Some(id);
        }
        let g = self.group_of(id)?;
        self.groups[g].iter().copied().find(|x| self.tiles.contains(x))
    }

    /// The burst mark of the tile `id`.
    #[cfg(test)]
    pub fn run_of(&self, id: RecordId) -> Option<RunMark> {
        self.sections.iter().zip(&self.runs).find_map(|(s, runs)| runs.get(s.ids.iter().position(|&x| x == id)?).copied()).flatten()
    }

    /// The frames of the burst whose tile is `id`, in shooting order, when
    /// it is a burst.
    pub fn frames_of(&self, id: RecordId) -> Option<&[RecordId]> {
        let g = self.group_of(id)?;
        (self.groups[g].len() > 1).then(|| self.groups[g].as_slice())
    }
}

/// The results' lists: the best matches, and the fursuits left out after
/// them when `fursuits` is on (a section of their own, never mixed in).
pub fn parts(r: &Results, fursuits: bool) -> Vec<Part<'_>> {
    let mut parts = vec![Part { found: &r.found, date: RESULTS_DAY, label: results_heading(&r.understood).into() }];
    if fursuits && !r.fursuits.is_empty() {
        parts.push(Part { found: &r.fursuits, date: FURSUITS_DAY, label: fursuits_heading(r.animal.as_deref()) });
    }
    parts
}

/// The fursuits' heading: "Fursuits that look like dogs".
pub fn fursuits_heading(animal: Option<&str>) -> String {
    match animal {
        Some(a) => format!("Fursuits that look like {}", plural_word(a)),
        None => "Fursuits".into(),
    }
}

/// The line under the results while the fursuits are left out: "12
/// fursuits that look like dogs are left out."
pub fn fursuits_left_out(n: usize, animal: Option<&str>) -> String {
    let like = animal.map(|a| format!(" that look{} like {}", if n == 1 { "s" } else { "" }, plural_word(a))).unwrap_or_default();
    format!("{}{like} {} left out.", crate::util::plural(n as u64, "fursuit"), if n == 1 { "is" } else { "are" })
}

/// "dog" → "dogs", "puppy" → "puppies", "fox" → "foxes"; a word already
/// plural stays.
fn plural_word(w: &str) -> String {
    if w.ends_with('s') && !w.ends_with("ss") {
        w.to_string()
    } else if w.ends_with('y') && !w.ends_with("ey") {
        format!("{}ies", &w[..w.len() - 1])
    } else if w.ends_with('x') || w.ends_with("sh") || w.ends_with("ch") || w.ends_with("ss") {
        format!("{w}es")
    } else {
        format!("{w}s")
    }
}

/// Lay the parts out, each best first, one section each.  The frames of
/// one burst gather at the place of its best frame: one stacked tile (that
/// frame, its cover) when `stacked` says so for its key, else every frame
/// in shooting order.  A burst never spans two sections.
pub fn layout(parts: &[Part<'_>], stacked: impl Fn(u64) -> bool) -> Layout {
    let mut out = Layout::default();
    for part in parts {
        let found = part.found;
        let salt = if part.date == FURSUITS_DAY { FURSUITS_SALT } else { 0 };
        // Groups in the order of their best frame.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut keys: Vec<Option<u64>> = Vec::new();
        let mut by_key: HashMap<u64, usize> = HashMap::new();
        for (i, f) in found.iter().enumerate() {
            match f.burst.map(|k| k ^ salt) {
                Some(k) => match by_key.get(&k) {
                    Some(&g) => groups[g].push(i),
                    None => {
                        by_key.insert(k, groups.len());
                        groups.push(vec![i]);
                        keys.push(Some(k));
                    }
                },
                None => {
                    groups.push(vec![i]);
                    keys.push(None);
                }
            }
        }
        let first_tile = out.tiles.len();
        let mut runs = Vec::with_capacity(found.len());
        let mut visible = Vec::with_capacity(found.len());
        for (g, members) in groups.iter().enumerate() {
            let burst = out.groups.len() as u32;
            let cover = found[members[0]].id;
            let mut frames: Vec<&Found> = members.iter().map(|&i| &found[i]).collect();
            frames.sort_by(|a, b| a.when.cmp(&b.when).then(a.id.cmp(&b.id)));
            let ids: Vec<RecordId> = frames.iter().map(|f| f.id).collect();
            visible.extend_from_slice(&ids);
            let n = ids.len();
            if n < 2 {
                out.tiles.push(cover);
                runs.push(None);
            } else {
                let mark = |first: bool, last: bool, stacked: bool| RunMark {
                    burst,
                    first,
                    last,
                    shown: n.min(u16::MAX as usize) as u16,
                    total: n.min(u16::MAX as usize) as u16,
                    stacked,
                    kept: 0,
                    pruned: 0,
                };
                if keys[g].is_some_and(&stacked) {
                    out.tiles.push(cover);
                    runs.push(Some(mark(true, true, true)));
                } else {
                    for (k, &id) in ids.iter().enumerate() {
                        out.tiles.push(id);
                        runs.push(Some(mark(k == 0, k + 1 == n, false)));
                    }
                }
            }
            out.groups.push(ids);
            out.keys.push(keys[g]);
        }
        let ids = out.tiles[first_tile..].to_vec();
        out.sections.push(Section { date: part.date, label: part.label.clone(), photos: found.len(), ids });
        out.runs.push(runs);
        out.visible.push(visible);
    }
    out
}

/// The selection once the tiles changed (a burst stacked or opened, new
/// results): each photo still a tile stays, one folded into a stack hands
/// it to the stack, the rest go.
pub fn regather(layout: &Layout, selection: &HashSet<RecordId>, anchor: Option<RecordId>) -> (HashSet<RecordId>, Option<RecordId>) {
    let sel = selection.iter().filter_map(|&id| layout.tile_of(id)).collect();
    (sel, anchor.and_then(|a| layout.tile_of(a)))
}

/// The tile an arrow key moves to: the next or previous one, the first
/// when nothing is focused; it stays at either end.
pub fn step(tiles: &[RecordId], anchor: Option<RecordId>, forward: bool) -> Option<RecordId> {
    match anchor.and_then(|a| tiles.iter().position(|&x| x == a)) {
        None => tiles.first().copied(),
        Some(i) if forward => tiles.get((i + 1).min(tiles.len() - 1)).copied(),
        Some(i) => tiles.get(i.saturating_sub(1)).copied(),
    }
}

// ── recent searches ─────────────────────────────────────────────────────────
//
// Kept in the settings (`Settings::remember_search`, the last 10, newest
// first; Preferences ▸ Search ▸ Forget them clears them).

/// The recent searches offered while `typed` is in the field: those with a
/// word starting with it (all of them for an empty field), newest first,
/// leaving out the one that is exactly what is typed.
pub fn completions<'a>(recent: &'a [String], typed: &str) -> Vec<&'a str> {
    let t = typed.trim().to_lowercase();
    recent
        .iter()
        .map(String::as_str)
        .filter(|r| {
            let l = r.to_lowercase();
            l != t && (t.is_empty() || l.starts_with(&t) || l.contains(&format!(" {t}")))
        })
        .collect()
}

// ── completions ─────────────────────────────────────────────────────────────
//
// While typing, the list under the field offers things search found (never
// people, fursuits or animals), months and years with photos, and recent
// searches.  Everything comes from memory: the engine's vocabulary, the
// calendar's index, the settings.

/// One completion under the field.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// A thing search found, with how many photos show it; `minus` puts it
    /// in as a word to leave out.
    Thing { word: String, photos: usize, minus: bool },
    /// A month or a year with photos: its name ("June 2024"), what it puts
    /// in the field ("jun 2024"), how many days have photos, and how many
    /// of the words being typed it replaces (2 for "jun 20").
    Date { label: String, text: String, days: usize, replace: usize },
    /// A recent search, run as it is.
    Recent(String),
}

impl Pick {
    /// The heading it sits under.
    pub fn group(&self) -> &'static str {
        match self {
            Pick::Thing { minus: false, .. } => "Things",
            Pick::Thing { minus: true, .. } => "Leave out",
            Pick::Date { .. } => "Dates",
            Pick::Recent(_) => "Recent searches",
        }
    }
}

/// What the list under the field offers for what is typed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Offer {
    pub picks: Vec<Pick>,
    /// The word being typed (lower case, without a leading `-`): set
    /// heavier in each thing and date.
    pub stem: String,
    /// The word being typed starts with `@`: names come later.
    pub names_later: bool,
}

/// At most this many of each group while typing.
const MAX_THINGS: usize = 5;
const MAX_DATES: usize = 4;
const MAX_RECENT: usize = 3;

/// The things worth offering: objects and scenes only (never people,
/// fursuits or animals, whatever the engine sends), most photos first.
pub fn offered(things: &[Thing]) -> impl Iterator<Item = &Thing> {
    things.iter().filter(|t| !t.kind.is_subject() && t.photos > 0)
}

/// What to offer while `typed` is in the field, from the things found, the
/// days with photos and the recent searches.  The last word is the one
/// being typed; a space after it means none is.
pub fn offer<'a>(typed: &str, things: &[Thing], days: impl IntoIterator<Item = &'a NaiveDate>, recent: &[String]) -> Offer {
    let mut o = Offer::default();
    if typed.trim().is_empty() {
        o.picks = recent.iter().map(|r| Pick::Recent(r.clone())).collect();
        return o;
    }
    let tokens: Vec<&str> = typed.split_whitespace().collect();
    let open = !typed.ends_with(char::is_whitespace);
    let last = if open { tokens.last().copied().unwrap_or("") } else { "" };
    if last.starts_with('@') {
        o.names_later = true;
        return o;
    }
    let minus = last.starts_with('-');
    let stem = last.trim_start_matches('-').to_lowercase();
    // Things already in the query (a word, or the words of "parking
    // lots", left out or not) aren't offered again.
    let before = &tokens[..tokens.len() - usize::from(open && !tokens.is_empty())];
    let said: Vec<String> = before.iter().map(|w| w.trim_start_matches('-').trim_matches('"').to_lowercase()).collect();
    let said = format!(" {} ", said.join(" "));
    let fresh = |t: &Thing| !t.kind.is_subject() && t.photos > 0 && !said.contains(&format!(" {} ", t.word.to_lowercase()));
    if minus && stem.is_empty() {
        // "-": the things most photos show, to leave out.
        o.picks.extend(things.iter().filter(|t| fresh(t)).take(MAX_THINGS).map(|t| Pick::Thing { word: t.word.clone(), photos: t.photos, minus }));
    } else if !stem.is_empty() {
        let found = crate::search::complete_things(things, &stem);
        o.picks.extend(found.into_iter().filter(|t| fresh(t)).take(MAX_THINGS).map(|t| Pick::Thing { word: t.word, photos: t.photos, minus }));
    }
    if open && !minus && !stem.is_empty() {
        // "jun 20" offers June of the years starting so: both words go.
        let numeric = stem.bytes().all(|b| b.is_ascii_digit());
        let after_month = numeric && before.last().and_then(|w| parse::month_of(w)).is_some();
        let replace = if after_month { 2 } else { 1 };
        let hints = parse::date_completions(days, &tokens[tokens.len().saturating_sub(2)..].join(" "));
        o.picks.extend(hints.into_iter().take(MAX_DATES).map(|h| Pick::Date { label: h.label, text: h.text, days: h.days, replace }));
    }
    let n = if o.picks.is_empty() { recent.len() } else { MAX_RECENT };
    o.picks.extend(completions(recent, typed).into_iter().take(n).map(|r| Pick::Recent(r.to_string())));
    o.stem = stem;
    o
}

/// The field's text once `pick` is taken: a thing or a date replaces the
/// word (or words) being typed and leaves a space for the next; a recent
/// search replaces everything.
pub fn apply(typed: &str, pick: &Pick) -> String {
    let (text, replace) = match pick {
        Pick::Recent(r) => return r.clone(),
        // Two words left out stay together: -"parking lots".
        Pick::Thing { word, minus: true, .. } if word.contains(' ') => (format!("-\"{word}\""), 1),
        Pick::Thing { word, minus, .. } => (if *minus { format!("-{word}") } else { word.clone() }, 1),
        Pick::Date { text, replace, .. } => (text.clone(), *replace),
    };
    let tokens: Vec<&str> = typed.split_whitespace().collect();
    let open = !typed.ends_with(char::is_whitespace);
    let keep = if open { tokens.len().saturating_sub(replace) } else { tokens.len() };
    let mut out: Vec<&str> = tokens[..keep].to_vec();
    out.push(&text);
    format!("{} ", out.join(" "))
}

/// What the engine applied besides the words, in a few plain words each:
/// "Only Jun 2024" (or "Every photo from Jun 2024" when there are no
/// words), "Leaving out crowd and stage".
pub fn understood_lines(p: &Parsed) -> (Option<String>, Option<String>) {
    let dates = match every_photo(p) {
        Some(d) => Some(format!("Every photo from {}", d.label())),
        None => p.dates.map(|d| format!("Only {}", d.label())),
    };
    let leave = (!p.exclude.is_empty()).then(|| format!("Leaving out {}", and_list(&p.exclude)));
    (dates, leave)
}

/// Dates and no words ("jun 2026", "jun 2026 -crowd"): the engine answers
/// with every photo from those dates in the order they were taken, not by
/// how well they match.  The dates, when so.
pub fn every_photo(p: &Parsed) -> Option<DateRange> {
    p.dates.filter(|_| p.words.trim().is_empty())
}

/// The results' heading: by match, or in the order taken for dates alone.
pub fn results_heading(p: &Parsed) -> &'static str {
    if every_photo(p).is_some() { "In the order they were taken" } else { "Best matches first" }
}

/// The count beside the field: "46 photos", or "312 photos from Jun 2026"
/// for dates alone; `so_far` while indexing still reaches older days.
pub fn count_caption(p: &Parsed, n: usize, so_far: bool) -> String {
    let mut s = crate::util::plural(n as u64, "photo");
    if let Some(d) = every_photo(p) {
        s = format!("{s} from {}", d.label());
    }
    if so_far {
        s.push_str(" so far");
    }
    s
}

/// The empty page's title and line, when nothing came back; `so_far` is
/// how many photos search has while the first index still runs.
pub fn nothing_found(r: &Results, so_far: Option<usize>) -> (String, String) {
    let p = &r.understood;
    let words = r.looks_like();
    let n = so_far.map(|n| crate::util::thousands(n as u64));
    match (every_photo(p), p.dates, n) {
        (Some(d), _, Some(n)) => (format!("None of the {n} photos indexed so far are from {}", d.label()), "Results fill in as indexing reaches older days.".into()),
        (Some(d), _, None) if !p.exclude.is_empty() => {
            (format!("No photos from {} without {}", d.label(), and_list(&p.exclude)), "Try other dates, or leave fewer words out.".into())
        }
        (Some(d), _, None) => (format!("No photos from {}", d.label()), "The library has none from those dates. Try other dates.".into()),
        (None, _, Some(n)) => (format!("Nothing in the {n} photos indexed so far looks like “{words}”"), "Results fill in as indexing reaches older days. Or try another word.".into()),
        (None, Some(d), None) => (format!("Nothing from {} looks like “{words}”", d.label()), "Try other dates, another word, or fewer words.".into()),
        (None, None, None) => (
            format!("Nothing in the library looks like “{words}”"),
            "Try another word, or fewer words. Search goes by what a photo shows, not by file names or folder names.".into(),
        ),
    }
}

/// The line after the last result, and a quieter one under it.
pub fn end_lines(r: &Results) -> (String, &'static str) {
    const CLOSENESS: &str = "Preferences ▸ Search sets how close a match must be.";
    let p = &r.understood;
    match (every_photo(p), p.dates) {
        (Some(d), _) => (format!("That's every photo from {}.", d.label()), "Accidental frames, such as empty or dark ones, are left out."),
        (None, Some(d)) => (format!("That's everything from {} that looks like “{}”.", d.label(), r.looks_like()), CLOSENESS),
        (None, None) => (format!("That's everything that looks like “{}”.", r.looks_like()), CLOSENESS),
    }
}

/// "a", "a and b", "a, b and c".
pub fn and_list(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::record::{FileType, MTime};
    use crate::search::Kind;
    use std::path::PathBuf;

    fn when(min: u32, ms: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 6, 8).unwrap().and_hms_opt(14, min, 0).unwrap() + chrono::Duration::milliseconds(ms)
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 6, 8).unwrap()
    }

    fn f(id: RecordId, score: f32, w: NaiveDateTime, burst: Option<u64>) -> Found {
        Found { id, score, when: w, day: day(), burst }
    }

    fn one(found: &[Found]) -> Vec<Part<'_>> {
        vec![Part { found, date: RESULTS_DAY, label: "Best matches first".into() }]
    }

    fn results(found: Vec<Found>, fursuits: Vec<Found>) -> Results {
        Results {
            seq: 1,
            words: "dogs".into(),
            coll: Arc::new(Collection::new()),
            found,
            fursuits,
            animal: Some("dog".into()),
            understood: parse("dogs"),
            missing: 0,
        }
    }

    #[test]
    fn fursuits_left_out_are_a_list_of_their_own_after_the_real_animals() {
        // A burst (key 7) with frames among the dogs and among the fursuits.
        let dogs = vec![f(10, 0.9, when(1, 0), None), f(11, 0.8, when(2, 100), Some(7)), f(12, 0.7, when(2, 200), Some(7))];
        let suits = vec![f(20, 0.95, when(2, 300), Some(7)), f(21, 0.85, when(3, 0), Some(7)), f(22, 0.6, when(4, 0), None)];
        let r = results(dogs, suits);
        // Left out by default: one list, the dogs only.
        let hidden = layout(&parts(&r, false), |_| true);
        assert_eq!(hidden.sections.len(), 1);
        assert_eq!(hidden.tiles, [10, 11]);
        assert!(hidden.tiles.iter().all(|id| *id < 20), "no fursuit among the real animals");
        // Shown: a second section after the first, never mixed in.
        let shown = layout(&parts(&r, true), |_| true);
        assert_eq!(shown.sections.len(), 2);
        assert_eq!((shown.sections[0].date, shown.sections[1].date), (RESULTS_DAY, FURSUITS_DAY));
        assert_eq!(shown.sections[0].ids, [10, 11]);
        assert_eq!(shown.sections[1].ids, [20, 22], "the fursuits' own burst stacks in their list");
        assert_eq!(shown.sections[1].label, "Fursuits that look like dogs");
        assert_eq!(shown.sections[1].photos, 3);
        // A burst split between the lists is two stacks, one in each, with
        // keys of their own (G on one never opens the other).
        assert_eq!(shown.frames_of(11), Some(&[11, 12][..]));
        assert_eq!(shown.frames_of(20), Some(&[20, 21][..]));
        let (k0, k1) = (shown.keys[shown.group_of(11).unwrap()], shown.keys[shown.group_of(20).unwrap()]);
        assert!(k0.is_some() && k1.is_some() && k0 != k1);
        // The viewer steps through one list, never from dogs into fursuits.
        assert_eq!(shown.visible_with(12), [10, 11, 12]);
        assert_eq!(shown.visible_with(21), [20, 21, 22]);
        assert_eq!(shown.run_of(20).map(|m| m.stacked), Some(true));
    }

    #[test]
    fn the_left_out_line_and_heading_name_the_animal() {
        assert_eq!(fursuits_left_out(12, Some("dog")), "12 fursuits that look like dogs are left out.");
        assert_eq!(fursuits_left_out(1, Some("fox")), "1 fursuit that looks like foxes is left out.");
        assert_eq!(fursuits_heading(Some("puppy")), "Fursuits that look like puppies");
        assert_eq!(fursuits_heading(None), "Fursuits");
    }

    fn thing(word: &str, photos: usize, kind: Kind) -> Thing {
        Thing { word: word.into(), photos, kind }
    }

    /// Things as the engine might send them, a few subjects slipped in to
    /// show the page keeps them out on its own.
    fn vocab() -> Vec<Thing> {
        use Kind::*;
        vec![
            thing("fursuits", 900, Fursuit),
            thing("people", 800, Person),
            thing("food", 60, Object),
            thing("flowers", 40, Object),
            thing("fireworks", 24, Object),
            thing("forests", 10, Scene),
            thing("foxes", 5, Animal),
            thing("crowds", 70, Scene),
        ]
    }

    fn days() -> Vec<NaiveDate> {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        vec![d(2023, 6, 3), d(2024, 6, 1), d(2024, 6, 8), d(2024, 7, 4), d(2025, 1, 1), d(2025, 2, 9)]
    }

    const NO_DAYS: &[NaiveDate] = &[];

    fn labels(o: &Offer) -> Vec<String> {
        o.picks
            .iter()
            .map(|p| match p {
                Pick::Thing { word, photos, minus } => format!("{}{word} {photos}", if *minus { "-" } else { "" }),
                Pick::Date { label, days, .. } => format!("{label} ({days})"),
                Pick::Recent(r) => format!("recent {r}"),
            })
            .collect()
    }

    #[test]
    fn things_offered_are_objects_and_scenes_never_people_fursuits_or_animals() {
        let o = offer("fo", &vocab(), NO_DAYS, &[]);
        assert_eq!(labels(&o), ["food 60", "forests 10"], "no fursuits, no foxes");
        assert_eq!(o.stem, "fo");
        // A singular finds the plural the list shows.
        assert_eq!(labels(&offer("forest", &vocab(), NO_DAYS, &[])), ["forests 10"]);
        assert!(offer("peo", &vocab(), NO_DAYS, &[]).picks.is_empty(), "people are never a thing");
        assert!(offer("fox", &vocab(), NO_DAYS, &[]).picks.is_empty(), "nor animals");
        // The start page's list follows the same rule.
        let v = vocab();
        let shown: Vec<&str> = offered(&v).map(|t| t.word.as_str()).collect();
        assert_eq!(shown, ["food", "flowers", "fireworks", "forests", "crowds"]);
    }

    #[test]
    fn a_minus_offers_things_to_leave_out() {
        let o = offer("fireworks -", &vocab(), &days(), &[]);
        assert_eq!(labels(&o), ["-food 60", "-flowers 40", "-forests 10", "-crowds 70"], "fireworks is already in the query; no dates after a minus");
        assert_eq!(o.picks[0].group(), "Leave out");
        assert_eq!(labels(&offer("fireworks -cr", &vocab(), &days(), &[])), ["-crowds 70"]);
        assert_eq!(apply("fireworks -cr", &o.picks[3]), "fireworks -crowds ");
        // Two words left out stay together, and the parser reads them so.
        let lots = Pick::Thing { word: "parking lots".into(), photos: 3, minus: true };
        let typed = apply("cars -pa", &lots);
        assert_eq!(typed, "cars -\"parking lots\" ");
        assert_eq!(parse(&typed).exclude, ["parking lots"]);
        assert_eq!(parse(&typed).words, "cars");
        // Once in the query, a thing of two words isn't offered again.
        let things = vec![thing("parking lots", 3, Kind::Scene), thing("parks", 9, Kind::Scene)];
        assert_eq!(labels(&offer(&format!("{typed}pa"), &things, NO_DAYS, &[])), ["parks 9"]);
    }

    #[test]
    fn dates_offer_months_and_years_with_photos() {
        // Years with their days, the most first.
        assert_eq!(labels(&offer("cake 202", &[], &days(), &[])), ["2024 (3)", "2025 (2)", "2023 (1)"]);
        // A month's name: every year it has photos.
        let o = offer("cake ju", &[], &days(), &[]);
        assert_eq!(labels(&o), ["June 2024 (2)", "July 2024 (1)", "June 2023 (1)"]);
        assert_eq!(apply("cake ju", &o.picks[0]), "cake jun 2024 ");
        // A month then the start of a year: both words are replaced.
        let o = offer("cake jun 20", &[], &days(), &[]);
        assert_eq!(labels(&o), ["June 2024 (2)", "June 2023 (1)"]);
        assert_eq!(apply("cake jun 20", &o.picks[0]), "cake jun 2024 ");
        assert!(offer("cake 1999", &[], &days(), &[]).picks.is_empty(), "no photos then");
        assert!(offer("cake ju ", &[], &days(), &[]).picks.is_empty(), "a space ends the word: nothing is being typed");
    }

    #[test]
    fn recent_searches_follow_things_and_dates_and_fill_an_empty_field() {
        let recent: Vec<String> = ["fireworks 2024", "food trucks", "red car"].iter().map(|s| s.to_string()).collect();
        let o = offer("f", &vocab(), &days(), &recent);
        let groups: Vec<&str> = o.picks.iter().map(Pick::group).collect();
        assert_eq!(groups, ["Things", "Things", "Things", "Things", "Dates", "Recent searches", "Recent searches"]);
        assert_eq!(labels(&offer("", &vocab(), &days(), &recent)), ["recent fireworks 2024", "recent food trucks", "recent red car"]);
        assert_eq!(apply("f", &Pick::Recent("food trucks".into())), "food trucks");
        // After a word is taken, the space ends it: recent searches that
        // carry on from it.
        assert_eq!(labels(&offer("fireworks ", &vocab(), &days(), &recent)), ["recent fireworks 2024"]);
    }

    #[test]
    fn a_name_is_not_offered_yet() {
        let o = offer("@al", &vocab(), &days(), &["alpine".into()]);
        assert!(o.names_later && o.picks.is_empty());
    }

    #[test]
    fn what_was_understood_reads_in_plain_words() {
        let (dates, leave) = understood_lines(&parse("fireworks -crowd -stage jun 2024"));
        assert_eq!(dates.as_deref(), Some("Only Jun 2024"));
        assert_eq!(leave.as_deref(), Some("Leaving out crowd and stage"));
        assert_eq!(understood_lines(&parse("fireworks")), (None, None));
        assert_eq!(and_list(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    /// Results for `query` with `n` photos found (none of them built: only
    /// the words matter here).
    fn answered(query: &str, n: usize) -> Results {
        let found = (0..n as RecordId).map(|i| f(i, 0.5, when(1, i as i64), None)).collect();
        Results {
            seq: 1,
            words: query.into(),
            coll: Arc::new(Collection::new()),
            found,
            fursuits: Vec::new(),
            animal: None,
            understood: parse(query),
            missing: 0,
        }
    }

    #[test]
    fn dates_alone_are_every_photo_from_them_in_the_order_taken() {
        let r = answered("jun 2026", 312);
        assert_eq!(every_photo(&r.understood).map(|d| d.label()).as_deref(), Some("Jun 2026"));
        assert_eq!(understood_lines(&r.understood).0.as_deref(), Some("Every photo from Jun 2026"));
        assert_eq!(count_caption(&r.understood, 312, false), "312 photos from Jun 2026");
        assert_eq!(count_caption(&r.understood, 312, true), "312 photos from Jun 2026 so far");
        let l = layout(&parts(&r, false), |_| true);
        assert_eq!(l.sections[0].label, "In the order they were taken");
        let (end, quiet) = end_lines(&r);
        assert_eq!(end, "That's every photo from Jun 2026.");
        // Nothing says these are matches, best first.
        for line in [&l.sections[0].label, &end, quiet, &count_caption(&r.understood, 312, false)] {
            assert!(!line.to_lowercase().contains("best") && !line.to_lowercase().contains("looks like"), "{line}");
        }
        // With words left out, still every photo from the dates, but those.
        let r = answered("jun 2026 -crowds", 0);
        assert_eq!(understood_lines(&r.understood), (Some("Every photo from Jun 2026".into()), Some("Leaving out crowds".into())));
        assert_eq!(nothing_found(&r, None).0, "No photos from Jun 2026 without crowds");
        assert_eq!(nothing_found(&answered("jun 2026", 0), None).0, "No photos from Jun 2026");
        assert_eq!(nothing_found(&answered("jun 2026", 0), Some(1200)).0, "None of the 1,200 photos indexed so far are from Jun 2026");
        // Words and dates: matches, best first, within the dates.
        let r = answered("fireworks jun 2026", 4);
        assert_eq!(every_photo(&r.understood), None);
        assert_eq!(understood_lines(&r.understood).0.as_deref(), Some("Only Jun 2026"));
        assert_eq!(count_caption(&r.understood, 4, false), "4 photos");
        assert_eq!(layout(&parts(&r, false), |_| true).sections[0].label, "Best matches first");
        assert_eq!(end_lines(&r).0, "That's everything from Jun 2026 that looks like “fireworks”.");
        assert_eq!(nothing_found(&r, None).0, "Nothing from Jun 2026 looks like “fireworks”");
        // A name alone, or words to leave out alone, are no dates: nothing
        // to show every photo of.
        assert_eq!(every_photo(&parse("@Alice")), None);
        assert_eq!(every_photo(&parse("-crowds")), None);
    }

    #[test]
    fn a_burst_stacks_at_the_rank_of_its_best_frame() {
        // Best first: 10 (burst 7), 20, 11 (burst 7), 30, 12 (burst 7).
        let found = vec![
            f(10, 0.9, when(1, 300), Some(7)),
            f(20, 0.8, when(5, 0), None),
            f(11, 0.7, when(1, 100), Some(7)),
            f(30, 0.6, when(9, 0), Some(9)),
            f(12, 0.5, when(1, 200), Some(7)),
        ];
        let l = layout(&one(&found), |_| true);
        assert_eq!(l.tiles, [10, 20, 30], "the stack's cover is its best frame; burst 9 has one frame and is no stack");
        let mark = l.run_of(10).expect("a stack");
        assert!(mark.stacked && mark.first && mark.last);
        assert_eq!((mark.shown, mark.total), (3, 3));
        assert_eq!(l.run_of(30), None);
        // The viewer steps through every frame, each burst in shooting order.
        assert_eq!(l.visible[0], [11, 12, 10, 20, 30]);
        assert_eq!(l.sections.len(), 1);
        assert_eq!(l.sections[0].photos, 5);
        assert_eq!(l.sections[0].label, "Best matches first");
    }

    #[test]
    fn an_open_burst_shows_its_frames_in_shooting_order_in_its_place() {
        let found = vec![f(10, 0.9, when(1, 300), Some(7)), f(20, 0.8, when(5, 0), None), f(11, 0.7, when(1, 100), Some(7))];
        let l = layout(&one(&found), |_| false);
        assert_eq!(l.tiles, [11, 10, 20]);
        let (a, b) = (l.run_of(11).unwrap(), l.run_of(10).unwrap());
        assert!(a.first && !a.last && !a.stacked);
        assert!(!b.first && b.last);
        assert_eq!(l.run_of(20), None);
        assert_eq!(l.frames_of(10), Some(&[11, 10][..]));
        assert_eq!(l.frames_of(20), None);
    }

    #[test]
    fn stacking_hands_the_selection_to_the_stack() {
        let found = vec![f(10, 0.9, when(1, 300), Some(7)), f(20, 0.8, when(5, 0), None), f(11, 0.7, when(1, 100), Some(7))];
        let stacked = layout(&one(&found), |_| true);
        let (sel, anchor) = regather(&stacked, &[11, 20].into(), Some(11));
        assert_eq!(sel, [10, 20].into());
        assert_eq!(anchor, Some(10));
        assert_eq!(stacked.tile_of(11), Some(10));
        assert_eq!(stacked.tile_of(99), None);
    }

    #[test]
    fn arrows_step_by_tile_and_stop_at_the_ends() {
        let tiles = [10, 20, 30];
        assert_eq!(step(&tiles, None, true), Some(10));
        assert_eq!(step(&tiles, Some(10), true), Some(20));
        assert_eq!(step(&tiles, Some(30), true), Some(30));
        assert_eq!(step(&tiles, Some(10), false), Some(10));
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn records_come_from_the_library_index_and_pair_up() {
        let entry = |p: &str, ft: FileType| FileEntry { path: PathBuf::from(p), file_type: ft, size: 10, mtime: MTime::from_unix(1_700_000_000, 0) };
        let index: HashMap<PathBuf, FileEntry> = [
            entry("/lib/08-Jun-2024/a.JPG", FileType::Jpg),
            entry("/lib/08-Jun-2024/a.ARW", FileType::Raw),
            entry("/lib/08-Jun-2024/b.ARW", FileType::Raw),
        ]
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
        let hit = |jpg: Option<&str>, raw: Option<&str>, score: f32| Hit {
            id: 0,
            key: crate::search::index::Key([0; 16]),
            score,
            tagged: false,
            day: day(),
            capture: when(1, 250),
            burst: None,
            jpg: jpg.map(PathBuf::from),
            raw: raw.map(PathBuf::from),
        };
        let hits = [
            hit(Some("/lib/08-Jun-2024/a.JPG"), Some("/lib/08-Jun-2024/a.ARW"), 0.9),
            hit(None, Some("/lib/08-Jun-2024/b.ARW"), 0.8),
            hit(Some("/lib/gone.JPG"), None, 0.7),
        ];
        let r = build(3, "cameras".into(), &hits, Answer::default(), |d, p| (d == day()).then(|| index.get(p).cloned()).flatten());
        assert_eq!((r.seq, r.found.len(), r.missing), (3, 2, 1));
        let a = r.coll.get(r.found[0].id).unwrap();
        assert_eq!(a.file_type, FileType::Jpg, "a pair's tile is its JPG");
        assert!(a.is_paired());
        assert_eq!(a.capture_time, Some(when(1, 250)), "the engine's capture time, to the sub-second");
        assert_eq!(r.coll.get(r.found[1].id).unwrap().file_type, FileType::Raw);
        assert_eq!(r.day_of(r.coll.find_pair(r.found[0].id).unwrap()), Some(day()), "either half finds its day");
    }

    #[test]
    fn the_list_offers_the_last_ten_searches_newest_first_until_forgotten() {
        let mut s = crate::settings::Settings::default();
        for i in 0..12 {
            s.remember_search(&format!("query {i}"));
        }
        let offered = completions(&s.search_recent, "");
        assert_eq!(offered.len(), 10, "the last 10");
        assert_eq!((offered[0], offered[9]), ("query 11", "query 2"), "newest first");
        s.remember_search("query 5");
        assert_eq!(completions(&s.search_recent, "qu")[0], "query 5", "run again, it comes first");
        assert_eq!(completions(&s.search_recent, "").len(), 10);
        // Preferences ▸ Search ▸ Forget them.
        s.forget_searches();
        assert!(completions(&s.search_recent, "").is_empty());
    }

    #[test]
    fn completions_match_the_start_of_a_word() {
        let recent: Vec<String> = ["red car at night", "cameras", "fireworks", "Cars"].iter().map(|s| s.to_string()).collect();
        assert_eq!(completions(&recent, "car"), ["red car at night", "Cars"]);
        assert_eq!(completions(&recent, "ca"), ["red car at night", "cameras", "Cars"]);
        assert_eq!(completions(&recent, "cars"), Vec::<&str>::new(), "what is typed exactly isn't offered again");
        assert_eq!(completions(&recent, "ight"), Vec::<&str>::new(), "not from the middle of a word");
        assert_eq!(completions(&recent, "").len(), 4);
    }
}
