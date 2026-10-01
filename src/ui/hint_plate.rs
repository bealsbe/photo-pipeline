//! The analysis' hints as the photo views show them: one small plate on a
//! photo's lower left corner (the grid, the burst sheet's tiles), a short
//! phrase (quick look, the viewer, compare), and the plain-word reasons
//! for tooltips and the bulk actions' dialogs.
//!
//! Hints are suggestions, so they are drawn hollow and neutral: a dark
//! plate with a light outline glyph, never the solid green and red of a
//! decision.  Only an accidental frame's glyph takes a colour (the
//! caution yellow).  A photo shows its plate until it is pruned or marked.
//!
//! Everything here reads [`Hints`] by lookup: nothing is worked out per
//! frame.

use iced::advanced::image::Renderer as _;
use iced::widget::image::FilterMethod;
use iced::{Color, Rectangle};

use super::icons::{self, Glyph};
use super::paint::{self, Renderer, rect};
use super::theme as t;
use crate::model::collection::{Collection, RecordId};
use crate::model::duplicates::{self, Kind as DupKind};
use crate::model::hints::{Accident, Hints, Plate, ShotHint, Soft};
use crate::model::record::PhotoRecord;

/// The plate's glyph.
pub fn glyph(p: Plate) -> Glyph {
    match p {
        Plate::Accidental => Glyph::HintAccident,
        Plate::Soft => Glyph::HintSoft,
        Plate::Copy => Glyph::HintCopy,
        Plate::Sharpest => Glyph::HintSharp,
    }
}

/// The glyph's colour on a photo: neutral, but for an accident.
pub fn tone(p: Plate) -> Color {
    match p {
        Plate::Accidental => t::WARN,
        _ => t::PHOTO_INK,
    }
}

/// Smaller than this, a photo has no room for a plate.
const MIN_PHOTO: f32 = 56.0;

/// The plate's place on a photo `ir` of a `ts`-pixel tile: its lower left
/// corner, inset like the keep check on the upper right.
pub fn plate_rect(ir: Rectangle, ts: f32) -> Rectangle {
    let h = if ts >= 200.0 { 22.0 } else { 20.0 };
    rect(ir.x + 6.0, ir.y + ir.height - h - 6.0, h, h)
}

/// Draw `plate` on the photo `ir`.  Call it in a layer above the images.
pub fn paint_plate(r: &mut Renderer, ir: Rectangle, ts: f32, plate: Plate, op: f32) {
    if ir.width < MIN_PHOTO || ir.height < MIN_PHOTO || op <= 0.0 {
        return;
    }
    let b = plate_rect(ir, ts);
    paint::rounded(r, b, b.height / 2.0, t::alpha(Color::BLACK, 0.55 * op));
    let g = (b.height - 5.0).round();
    let at = rect((b.center_x() - g / 2.0).round(), (b.center_y() - g / 2.0).round(), g, g);
    // Opacity on the image, not in the colour: icon handles are cached by
    // colour, and fades would fill the cache.
    r.draw_image(iced::advanced::image::Image::new(icons::handle(glyph(plate), g, tone(plate))).filter_method(FilterMethod::Linear).opacity(op), at, b);
}

/// "1st", "2nd", "3rd", "4th"… "11th", "12th", "13th", "21st".
pub fn ordinal(n: u16) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// How much softer a frame is than the best like it: "3×" (from 1/relative,
/// at least 2, as the cut-off is about a third).
fn softer(relative: f32) -> String {
    let k = if relative > 0.0 { (1.0 / relative).round() } else { 9.0 };
    format!("{}×", k.clamp(2.0, 99.0) as u32)
}

fn stem_of(coll: &Collection, id: RecordId) -> String {
    coll.get(id).map(|r| r.stem()).unwrap_or_default()
}

/// Plain words for an accident: what the frame looks like.
pub fn accident_words(a: Accident) -> &'static str {
    match a {
        Accident::Black => "almost black, as if the lens cap was on",
        Accident::Blank => "almost white",
        Accident::Flat => "almost one tone, like a wall or a sleeve",
        Accident::Dark => "dark, with nothing sharp in it",
        Accident::Featureless => "nothing in it is in focus",
    }
}

/// A short phrase for the photo's hint, as quick look, the viewer and
/// compare put it beside the file name, with its plate: "Looks out of
/// focus", "Sharpest of 8"; or, with no plate, a frame's place for
/// sharpness, "3rd sharpest of 8".  `None` without a hint or a rank.
pub fn phrase(h: &Hints, id: RecordId) -> Option<(String, Option<Plate>)> {
    let x = h.get(id);
    match h.plate(id) {
        Some(Plate::Accidental) => Some(("Looks accidental".into(), Some(Plate::Accidental))),
        Some(Plate::Soft) => Some(("Looks out of focus".into(), Some(Plate::Soft))),
        Some(Plate::Copy) => Some(("A copy".into(), Some(Plate::Copy))),
        Some(Plate::Sharpest) => x.and_then(|x| x.rank).map(|(_, n)| (format!("Sharpest of {n}"), Some(Plate::Sharpest))),
        None => {
            let (k, n) = x?.rank?;
            (k > 1).then(|| (format!("{} sharpest of {n}", ordinal(k)), None))
        }
    }
}

/// The reason in a few words, for a tile in a bulk action's dialog:
/// "4× softer", "almost black", "copy of DSC01234".
pub fn reason(h: &Hints, coll: &Collection, id: RecordId) -> Option<String> {
    let x = h.get(id);
    if let Some(a) = x.and_then(|x| x.accidental) {
        let short = match a {
            Accident::Black => "almost black",
            Accident::Blank => "almost white",
            Accident::Flat => "one tone",
            Accident::Dark => "dark, nothing sharp",
            Accident::Featureless => "nothing in focus",
        };
        return Some(short.into());
    }
    if let Some(x) = x.filter(|x| x.soft.is_some()) {
        return Some(match x.soft {
            Some(Soft::Relative) => format!("{} softer", softer(x.relative)),
            _ => "nothing sharp".into(),
        });
    }
    let (g, prune) = h.duplicate_of(id)?;
    prune.then(|| format!("copy of {}", stem_of(coll, g.keep)))
}

/// Why the photo has its hints, in plain words, one line each (the grid's
/// tooltip).  Empty without any.
pub fn explain(h: &Hints, coll: &Collection, id: RecordId) -> Vec<String> {
    let mut out = Vec::new();
    let x = h.get(id);
    if let Some(a) = x.and_then(|x| x.accidental) {
        out.push(format!("Looks accidental: {}.", accident_words(a)));
    } else if let Some(x) = x.filter(|x| x.soft.is_some()) {
        out.push(soft_line(coll, x));
    }
    if let Some((g, prune)) = h.duplicate_of(id) {
        let recs: Vec<&PhotoRecord> = g.ids.iter().filter_map(|&i| coll.get(i)).collect();
        let keep = coll.get(g.keep);
        let what = if g.kind == DupKind::Exact { "An identical copy of" } else { "The same shot as" };
        match (prune, keep, coll.get(id)) {
            (true, Some(k), Some(me)) => {
                out.push(format!("{what} {}.", k.filename()));
                out.push(format!("Suggested for pruning: {}.", duplicates::why(&recs, k, me).words()));
            }
            (false, Some(_), _) => {
                let copies: Vec<String> = g.prune.iter().filter_map(|&c| coll.get(c)).map(|r| r.filename()).collect();
                let list = match copies.len() {
                    0 => String::new(),
                    1 => copies[0].clone(),
                    n => format!("{} and {} more", copies[0], n - 1),
                };
                out.push(format!("Has a copy: {list}. This one is the one to keep."));
            }
            _ => {}
        }
    }
    if let Some((k, n)) = x.and_then(|x| x.rank) {
        if k == 1 {
            out.push(format!("The sharpest of this burst's {n} frames."));
        } else if x.is_some_and(|x| x.soft.is_none()) {
            out.push(format!("The {} sharpest of this burst's {n} frames.", ordinal(k)));
        }
    }
    if let Some(m) = h.joined_moment(id).and_then(|m| h.moment(m)) {
        out.push(format!("One of {} bursts of the same scene, {} frames in all.", m.presses.len(), m.frames.len()));
    }
    out
}

fn soft_line(coll: &Collection, x: &ShotHint) -> String {
    match (x.soft, x.best_alike) {
        (Some(Soft::Relative), Some(best)) => {
            format!("Looks out of focus: {} softer than {}, a frame of this burst that shows the same.", softer(x.relative), stem_of(coll, best))
        }
        _ => "Looks out of focus: nothing in it is sharp.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals() {
        let v: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 111].iter().map(|&n| ordinal(n)).collect();
        assert_eq!(v, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st", "111th"]);
    }

    #[test]
    fn softer_reads_as_a_whole_factor() {
        assert_eq!(softer(0.35), "3×");
        assert_eq!(softer(0.21), "5×");
        assert_eq!(softer(0.6), "2×", "never less than twice");
        assert_eq!(softer(0.0), "9×");
    }

    /// A shot `ms` past 10:00 with Sony sequence `seq`.
    fn rec(path: &str, ms: i64, seq: Option<u16>) -> PhotoRecord {
        use crate::model::record::{FileType, MTime, ShotMeta};
        let p = std::path::PathBuf::from(path);
        let base = chrono::NaiveDateTime::parse_from_str("2026-09-19 10:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let mut r = PhotoRecord::new(p.clone(), FileType::from_path(&p).unwrap(), 1000, MTime::from_unix(1_700_000_000, 0), Some(base + chrono::Duration::seconds(ms / 1000)));
        r.shot = ShotMeta { subsec_ms: Some((ms % 1000) as u16), sequence: seq, focus: None, camera: 7 };
        r
    }

    #[test]
    fn hints_explain_themselves_in_plain_words() {
        use crate::imaging::quality::Features;
        use crate::imaging::quality::tests::{Img, feats, texture};
        use crate::model::bursts::Bursts;
        let scene = |seed: u32, blur: usize| feats(&texture(192, 128, seed).blur(blur));
        let shots: Vec<(PhotoRecord, Option<Features>)> = vec![
            (rec("/s/a1.JPG", 0, Some(1)), Some(scene(1, 0))),
            (rec("/s/a2.JPG", 100, Some(2)), Some(scene(1, 1))),
            (rec("/s/a3.JPG", 200, Some(3)), Some(scene(1, 6))),
            (rec("/s/black.JPG", 9000, Some(0)), Some(feats(&Img::from_fn(192, 128, |_, _| 3.0)))),
            (rec("/s/c.JPG", 20_000, Some(0)), Some(scene(4, 0))),
            (rec("/s/c copy.JPG", 20_000, Some(0)), Some(scene(4, 0))),
        ];
        let mut coll = Collection::new();
        let mut by_path = std::collections::HashMap::new();
        for (r, f) in shots {
            if let Some(f) = f {
                by_path.insert(r.path.clone(), f);
            }
            coll.add(r);
        }
        coll.build_pairs();
        let bursts = Bursts::find(&coll);
        let exact = vec![vec![std::path::PathBuf::from("/s/c.JPG"), std::path::PathBuf::from("/s/c copy.JPG")]];
        let h = Hints::derive(&coll, &bursts, |_, r| by_path.get(&r.path), &exact, 1);
        let id = |n: &str| coll.id_of(std::path::Path::new(&format!("/s/{n}.JPG"))).unwrap();
        // The sharpest: a plate, a phrase and a line.
        assert_eq!(phrase(&h, id("a1")), Some(("Sharpest of 3".to_string(), Some(Plate::Sharpest))));
        assert_eq!(explain(&h, &coll, id("a1")), vec!["The sharpest of this burst's 3 frames."]);
        // The 2nd: no plate, only its place.
        assert_eq!(phrase(&h, id("a2")), Some(("2nd sharpest of 3".to_string(), None)));
        // Soft next to its burst's best, which it names.
        let soft = explain(&h, &coll, id("a3"));
        assert!(soft[0].starts_with("Looks out of focus: ") && soft[0].contains("softer than a1"), "{soft:?}");
        assert!(reason(&h, &coll, id("a3")).unwrap().ends_with("× softer"));
        // An accident says what it looks like.
        assert_eq!(explain(&h, &coll, id("black"))[0], "Looks accidental: almost black, as if the lens cap was on.");
        assert_eq!(reason(&h, &coll, id("black")).as_deref(), Some("almost black"));
        // A copy names the file kept, and why this one is the copy.
        assert_eq!(explain(&h, &coll, id("c copy")), vec!["An identical copy of c.JPG.", "Suggested for pruning: its name reads like a copy's."]);
        assert_eq!(reason(&h, &coll, id("c copy")).as_deref(), Some("copy of c"));
        assert_eq!(explain(&h, &coll, id("c")), vec!["Has a copy: c copy.JPG. This one is the one to keep."]);
        assert_eq!(reason(&h, &coll, id("c")), None);
    }

    #[test]
    fn the_plate_sits_in_the_lower_left_corner() {
        let ir = rect(100.0, 50.0, 300.0, 200.0);
        let p = plate_rect(ir, 300.0);
        assert_eq!((p.x, p.y + p.height), (106.0, 244.0));
        assert!(plate_rect(ir, 160.0).height < p.height);
        // Accidents alone take a colour.
        assert_eq!(tone(Plate::Accidental), t::WARN);
        assert!([Plate::Soft, Plate::Copy, Plate::Sharpest].iter().all(|&p| tone(p) == t::PHOTO_INK));
    }
}
