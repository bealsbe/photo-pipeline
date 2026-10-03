//! The query syntax (plan §7.2), parsed into what the engine applies and
//! the page shows.  Pure, cheap, callable on the UI thread.
//!
//! | Write | Means |
//! |---|---|
//! | `red car at night` | photos that look like the words |
//! | `"a red car at night"` | the words exactly as the prompt, no template |
//! | `fireworks -crowd` | like "fireworks", and more like it than like "crowd" |
//! | `@Name` | a person, fursuit or pet's chip: phase 3; recognised, not searched |
//! | `2024`, `jun 2024`, `apr–jun 2024`, `2023–2024` | only those dates (`-` and `..` work as the dash) |
//!
//! Everything is combined with AND; two date terms narrow each other.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

/// A span of days, both ends inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DateRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
const SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

impl DateRange {
    /// A whole year.
    pub fn year(y: i32) -> Option<DateRange> {
        Some(DateRange { from: NaiveDate::from_ymd_opt(y, 1, 1)?, to: NaiveDate::from_ymd_opt(y, 12, 31)? })
    }

    /// A whole month.
    pub fn month(y: i32, m: u32) -> Option<DateRange> {
        Some(DateRange { from: NaiveDate::from_ymd_opt(y, m, 1)?, to: month_end(y, m)? })
    }

    /// Months `a`..=`b` of a year.
    pub fn months(y: i32, a: u32, b: u32) -> Option<DateRange> {
        Some(DateRange { from: NaiveDate::from_ymd_opt(y, a.min(b), 1)?, to: month_end(y, a.max(b))? })
    }

    /// Years `a`..=`b`.
    pub fn years(a: i32, b: i32) -> Option<DateRange> {
        Some(DateRange { from: NaiveDate::from_ymd_opt(a.min(b), 1, 1)?, to: NaiveDate::from_ymd_opt(a.max(b), 12, 31)? })
    }

    pub fn contains(&self, day: NaiveDate) -> bool {
        self.from <= day && day <= self.to
    }

    /// The days both ranges share (empty when they don't overlap: `from`
    /// after `to`, which [`contains`](Self::contains) answers false for).
    pub fn intersect(&self, other: &DateRange) -> DateRange {
        DateRange { from: self.from.max(other.from), to: self.to.min(other.to) }
    }

    pub fn is_empty(&self) -> bool {
        self.from > self.to
    }

    /// The range as the page shows it: "Jun 2024", "2024", "Apr–Jun 2024",
    /// "2023–2024", "Nov 2024–Feb 2025", or the days when it isn't whole
    /// months ("12 Jun 2024–3 Jul 2024").
    pub fn label(&self) -> String {
        if self.is_empty() {
            return "no dates".into();
        }
        let (f, t) = (self.from, self.to);
        let whole = f.day() == 1 && Some(t) == month_end(t.year(), t.month());
        if !whole {
            let d = |x: NaiveDate| format!("{} {} {}", x.day(), SHORT[x.month0() as usize], x.year());
            return if f == t { d(f) } else { format!("{}–{}", d(f), d(t)) };
        }
        let m = |x: NaiveDate| format!("{} {}", SHORT[x.month0() as usize], x.year());
        if f.year() == t.year() {
            if f.month() == 1 && t.month() == 12 {
                format!("{}", f.year())
            } else if f.month() == t.month() {
                m(f)
            } else {
                format!("{}–{} {}", SHORT[f.month0() as usize], SHORT[t.month0() as usize], f.year())
            }
        } else if f.month() == 1 && t.month() == 12 {
            format!("{}–{}", f.year(), t.year())
        } else {
            format!("{}–{}", m(f), m(t))
        }
    }

    /// The range as a query writes it ("jun 2024", "2024", "apr–jun
    /// 2024", "2023–2024"), when it is whole months; else `None`.
    pub fn text(&self) -> Option<String> {
        let (f, t) = (self.from, self.to);
        if self.is_empty() || f.day() != 1 || Some(t) != month_end(t.year(), t.month()) {
            return None;
        }
        let short = |x: NaiveDate| SHORT[x.month0() as usize].to_lowercase();
        Some(if f.year() == t.year() {
            if f.month() == 1 && t.month() == 12 {
                format!("{}", f.year())
            } else if f.month() == t.month() {
                date_text(f.year(), Some(f.month()))
            } else {
                format!("{}–{} {}", short(f), short(t), f.year())
            }
        } else if f.month() == 1 && t.month() == 12 {
            format!("{}–{}", f.year(), t.year())
        } else {
            return None;
        })
    }
}

/// The canonical query text for a month or a year, as completions insert
/// it: "jun 2024", "2024".
pub fn date_text(year: i32, month: Option<u32>) -> String {
    match month {
        Some(m) if (1..=12).contains(&m) => format!("{} {year}", SHORT[m as usize - 1].to_lowercase()),
        _ => format!("{year}"),
    }
}

/// The month's name for a label: "June".
pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    NAMES[(month.clamp(1, 12) - 1) as usize]
}

fn month_end(y: i32, m: u32) -> Option<NaiveDate> {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()
}

/// The month a token names (three letters or more of its name, or
/// "sept"), 1–12.
pub fn month_of(token: &str) -> Option<u32> {
    let t = token.to_lowercase();
    if t.len() < 3 {
        return None;
    }
    if t == "sept" {
        return Some(9);
    }
    let matches: Vec<u32> = MONTHS.iter().enumerate().filter(|(_, m)| m.starts_with(&t)).map(|(i, _)| i as u32 + 1).collect();
    match matches.as_slice() {
        [m] => Some(*m),
        _ => None,
    }
}

fn year_of(token: &str) -> Option<i32> {
    if token.len() != 4 || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse::<i32>().ok().filter(|y| (1900..=2100).contains(y))
}

/// A token split at a range dash: "apr–jun", "2023..2024", "apr-jun".
fn split_range(token: &str) -> Option<(&str, &str)> {
    for sep in ["–", "—", "..", "-"] {
        if let Some((a, b)) = token.split_once(sep) {
            if !a.is_empty() && !b.is_empty() {
                return Some((a, b));
            }
        }
    }
    None
}

/// What a query means.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Parsed {
    /// The text searched for, as typed (without the exclusions, dates
    /// and names).  Empty: nothing is searched.
    pub words: String,
    /// `words` was quoted: it is the prompt exactly, not wrapped in the
    /// template.
    pub exact: bool,
    /// `-word`s: a photo must be more like `words` than like each.
    pub exclude: Vec<String>,
    /// Only shots on these days (two date terms narrow each other).
    pub dates: Option<DateRange>,
    /// `@Name`s, as typed without the `@`.  Phase 3's chips; phase 2
    /// recognises them and searches nothing for them.
    pub names: Vec<String>,
}

impl Parsed {
    /// Nothing to search: no words (only dates, exclusions or names).
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// Parse a query.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut plain: Vec<String> = Vec::new();
    let mut exact = false;
    let mut chars = text.chars().peekable();
    // Lexing: quoted runs stay whole; a leading - or @ marks the token.
    let mut tokens: Vec<(char, String)> = Vec::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mark = if c == '-' || c == '@' {
            chars.next();
            c
        } else {
            ' '
        };
        let mut word = String::new();
        let mut quoted = false;
        if chars.peek() == Some(&'"') {
            chars.next();
            quoted = true;
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                word.push(ch);
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '"' {
                    break;
                }
                word.push(ch);
                chars.next();
            }
        }
        let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
        if word.is_empty() {
            if mark != ' ' && !quoted {
                // A lone "-" or "@": plain text.
                plain.push(mark.to_string());
            }
            continue;
        }
        tokens.push((if quoted && mark == ' ' { '"' } else { mark }, word));
    }
    let mut i = 0;
    while i < tokens.len() {
        let (mark, word) = &tokens[i];
        match mark {
            '-' => out.exclude.push(word.to_lowercase()),
            '@' => out.names.push(word.clone()),
            '"' => {
                exact = true;
                plain.push(word.clone());
            }
            _ => {
                // Dates: a year, a year range, a month and a year, a month
                // range and a year.
                let next_year = tokens.get(i + 1).filter(|(m, _)| *m == ' ').and_then(|(_, w)| year_of(w));
                let range = if let Some(y) = year_of(word) {
                    DateRange::year(y)
                } else if let Some((a, b)) = split_range(word).filter(|(a, b)| year_of(a).is_some() && year_of(b).is_some()) {
                    DateRange::years(year_of(a).unwrap_or(0), year_of(b).unwrap_or(0))
                } else if let (Some(m), Some(y)) = (month_of(word), next_year) {
                    i += 1;
                    DateRange::month(y, m)
                } else if let (Some((a, b)), Some(y)) = (split_range(word), next_year) {
                    match (month_of(a), month_of(b)) {
                        (Some(a), Some(b)) => {
                            i += 1;
                            DateRange::months(y, a, b)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                match range {
                    Some(r) => out.dates = Some(out.dates.map_or(r, |d| d.intersect(&r))),
                    None => plain.push(word.clone()),
                }
            }
        }
        i += 1;
    }
    out.words = plain.join(" ");
    out.exact = exact && !out.words.is_empty();
    out
}

/// A month or a year with photos, offered while a date is typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateHint {
    /// "June 2024", "2024".
    pub label: String,
    /// What the completion inserts: `date_text`'s "jun 2024", "2024".
    pub text: String,
    pub range: DateRange,
    /// Days with photos in it.
    pub days: usize,
}

/// Date completions for the end of `typed`, from the days that have
/// photos: a digit prefix offers years, a month prefix ("j", "jun",
/// "june") offers that month in every year, "jun 20" the years
/// starting so; the ones with the most days first.  Pure.
pub fn date_completions<'a>(days: impl IntoIterator<Item = &'a NaiveDate>, typed: &str) -> Vec<DateHint> {
    let mut months: std::collections::BTreeMap<(i32, u32), usize> = std::collections::BTreeMap::new();
    for d in days {
        *months.entry((d.year(), d.month())).or_default() += 1;
    }
    let toks: Vec<&str> = typed.split_whitespace().collect();
    let last = toks.last().copied().unwrap_or("").to_lowercase();
    let before = toks.len().checked_sub(2).map(|i| toks[i].to_lowercase());
    if last.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<DateHint> = Vec::new();
    let year_hint = |y: i32| {
        let days = months.iter().filter(|((yy, _), _)| *yy == y).map(|(_, n)| n).sum::<usize>();
        DateHint { label: format!("{y}"), text: date_text(y, None), range: DateRange::year(y).unwrap_or(DateRange { from: NaiveDate::MIN, to: NaiveDate::MIN }), days }
    };
    let month_hint = |y: i32, m: u32, days: usize| DateHint {
        label: format!("{} {y}", month_name(m)),
        text: date_text(y, Some(m)),
        range: DateRange::month(y, m).unwrap_or(DateRange { from: NaiveDate::MIN, to: NaiveDate::MIN }),
        days,
    };
    if last.bytes().all(|b| b.is_ascii_digit()) {
        // "2", "20", "202", "2024": years; after a month, that month of
        // those years.
        let month = before.as_deref().and_then(month_of);
        let mut years: Vec<i32> = months.keys().map(|(y, _)| *y).collect();
        years.dedup();
        for y in years.into_iter().filter(|y| y.to_string().starts_with(&last)) {
            match month {
                Some(m) => {
                    if let Some(&n) = months.get(&(y, m)) {
                        out.push(month_hint(y, m, n));
                    }
                }
                None => out.push(year_hint(y)),
            }
        }
    } else if last.chars().all(|c| c.is_alphabetic()) && last.len() >= 1 {
        // A month prefix: every year's month of that name, a one-letter
        // prefix matching several months.
        let candidates: Vec<u32> = if last.len() >= 3 {
            month_of(&last).into_iter().collect()
        } else {
            MONTHS.iter().enumerate().filter(|(_, m)| m.starts_with(&last)).map(|(i, _)| i as u32 + 1).collect()
        };
        for (&(y, m), &n) in &months {
            if candidates.contains(&m) {
                out.push(month_hint(y, m, n));
            }
        }
    }
    out.sort_by(|a, b| b.days.cmp(&a.days).then(b.range.from.cmp(&a.range.from)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn plain_words_quotes_exclusions_and_names_are_told_apart() {
        let p = parse("red car at night");
        assert_eq!(p, Parsed { words: "red car at night".into(), ..Parsed::default() });
        assert!(!p.is_empty());
        let p = parse("\"a red car at night\"");
        assert_eq!((p.words.as_str(), p.exact), ("a red car at night", true));
        let p = parse("fireworks -crowd -\"dark sky\"");
        assert_eq!(p.words, "fireworks");
        assert_eq!(p.exclude, ["crowd", "dark sky"]);
        let p = parse("@Fursuit on a beach @\"Some Name\" ");
        assert_eq!(p.words, "on a beach");
        assert_eq!(p.names, ["Fursuit", "Some Name"]);
        let p = parse("@Someone");
        assert!(p.is_empty() && p.names == ["Someone"], "{p:?}");
        assert_eq!(parse("  -  @ ").words, "- @", "lone marks are text");
        assert_eq!(parse("").words, "");
        assert_eq!(parse("\"\"").words, "");
        assert!(!parse("\"\"").exact);
    }

    #[test]
    fn dates_are_years_months_and_ranges_and_narrow_each_other() {
        let p = parse("camera 2024");
        assert_eq!(p.words, "camera");
        assert_eq!(p.dates, DateRange::year(2024));
        assert_eq!(p.dates.unwrap().label(), "2024");
        let p = parse("fireworks jun 2024");
        assert_eq!(p.dates, DateRange::month(2024, 6));
        assert_eq!(p.dates.unwrap().label(), "Jun 2024");
        assert_eq!(parse("June 2024 fireworks").dates, DateRange::month(2024, 6));
        assert_eq!(parse("sept 2024").dates, DateRange::month(2024, 9));
        for q in ["apr–jun 2024", "apr-jun 2024", "apr..jun 2024", "April—June 2024", "jun-apr 2024"] {
            let p = parse(q);
            assert_eq!(p.dates, DateRange::months(2024, 4, 6), "{q}");
            assert_eq!(p.dates.unwrap().label(), "Apr–Jun 2024", "{q}");
            assert!(p.words.is_empty() && p.exclude.is_empty(), "{q}: {p:?}");
        }
        assert_eq!(parse("2023–2024").dates, DateRange::years(2023, 2024));
        assert_eq!(parse("2023..2024").dates.unwrap().label(), "2023–2024");
        // Two terms narrow each other; a month alone is a word ("may").
        let p = parse("2024 jun 2024 dog");
        assert_eq!(p.dates, DateRange::month(2024, 6));
        assert_eq!(p.words, "dog");
        assert_eq!(parse("2023 2024").dates.map(|r| r.is_empty()), Some(true));
        assert_eq!(parse("may flowers").words, "may flowers");
        assert_eq!(parse("jun").words, "jun");
        assert_eq!(parse("1066").words, "1066", "not a photo year");
        assert_eq!(parse("ju 2024").words, "ju", "two letters don't name a month");
        assert_eq!(parse("ju 2024").dates, DateRange::year(2024));
        assert_eq!(parse("-2024").exclude, ["2024"], "a leading dash is an exclusion");
    }

    #[test]
    fn labels_and_texts_round_trip() {
        let r = DateRange { from: d(2024, 11, 1), to: d(2025, 2, 28) };
        assert_eq!(r.label(), "Nov 2024–Feb 2025");
        assert_eq!(r.text(), None);
        let r = DateRange { from: d(2024, 6, 12), to: d(2024, 7, 3) };
        assert_eq!(r.label(), "12 Jun 2024–3 Jul 2024");
        assert_eq!(DateRange { from: d(2024, 6, 12), to: d(2024, 6, 12) }.label(), "12 Jun 2024");
        for q in ["2024", "jun 2024", "apr–jun 2024", "2023–2024"] {
            let r = parse(q).dates.unwrap();
            assert_eq!(r.text().as_deref(), Some(q));
            assert_eq!(parse(&r.text().unwrap()).dates, Some(r));
        }
        assert_eq!(date_text(2024, Some(6)), "jun 2024");
        assert_eq!(date_text(2024, None), "2024");
        assert_eq!(date_text(2024, Some(13)), "2024");
        assert_eq!(month_name(6), "June");
        assert!(DateRange::month(2024, 2).unwrap().contains(d(2024, 2, 29)));
        assert!(!DateRange::month(2024, 2).unwrap().contains(d(2024, 3, 1)));
        assert_eq!(DateRange { from: d(2024, 3, 1), to: d(2024, 1, 1) }.label(), "no dates");
    }

    #[test]
    fn date_completions_offer_years_and_months_with_their_day_counts() {
        let days = [d(2024, 6, 8), d(2024, 6, 9), d(2024, 7, 1), d(2025, 6, 3), d(2026, 1, 1)];
        let labels = |t: &str| date_completions(days.iter(), t).into_iter().map(|h| format!("{} {} {}", h.label, h.days, h.text)).collect::<Vec<_>>();
        assert_eq!(labels("2"), ["2024 3 2024", "2026 1 2026", "2025 1 2025"], "most days first, then newest");
        assert_eq!(labels("202"), labels("2"));
        assert_eq!(labels("2025"), ["2025 1 2025"]);
        assert_eq!(labels("jun"), ["June 2024 2 jun 2024", "June 2025 1 jun 2025"]);
        assert_eq!(labels("fireworks June"), ["June 2024 2 jun 2024", "June 2025 1 jun 2025"]);
        assert_eq!(labels("j"), ["June 2024 2 jun 2024", "January 2026 1 jan 2026", "June 2025 1 jun 2025", "July 2024 1 jul 2024"]);
        assert_eq!(labels("jun 20"), ["June 2024 2 jun 2024", "June 2025 1 jun 2025"]);
        assert_eq!(labels("jun 2025"), ["June 2025 1 jun 2025"]);
        assert!(labels("x").is_empty() && labels("").is_empty() && labels("3").is_empty());
        let h = &date_completions(days.iter(), "jul")[0];
        assert_eq!(h.range, DateRange::month(2024, 7).unwrap());
    }
}
