//! The named groups as they are now, exported beside the record of
//! corrections (`groups.json`): the strongest labels the user curates
//! (same within a named group, different across named groups of a kind,
//! a rejected head different from its group), with the viewer's drawn
//! and removed boxes, the "not the same" answers and the dissolved
//! groups.
//!
//! Built from the user's subjects file on the engine's thread (or the
//! stand-in's groups), written by the record's thread with the export.
//! No names and no subject ids: each subject has the code the exported
//! log gives it ([`decisions::Codes`]), so the two files agree.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::decisions::{self, Codes, Entry, Head, Lib, Pair, PairSet};
use super::subjects::{FaceRef, Kind, SubjectsFile};

/// The file's name in an export.
pub const FILE_NAME: &str = "groups.json";
/// Its format version (`v`).
pub const SCHEMA: u32 = 1;

/// One named subject as the engine knows it (names included: codes are
/// given at export time, with the log's).
#[derive(Debug, Clone, PartialEq)]
pub struct Named {
    pub kind: Kind,
    pub sid: String,
    pub name: String,
    pub members: Vec<FaceRef>,
    pub rejected: Vec<FaceRef>,
    /// Photos, bursts and days as the tab counts the group.
    pub counts: Option<(usize, usize, usize)>,
}

/// What an export needs of the groups, taken on the engine's thread.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub named: Vec<Named>,
    pub added: Vec<(Kind, FaceRef)>,
    pub removed: Vec<(Kind, FaceRef)>,
    pub not_same: Vec<(Kind, Vec<FaceRef>, Vec<FaceRef>)>,
    /// Groups dissolved, each by all its heads.
    pub dissolved: Vec<(Kind, Vec<FaceRef>)>,
    /// Each kind's library state and the cut-offs (and, for fursuits,
    /// the grouping choice) in force.
    pub libs: Vec<(Kind, Lib)>,
}

impl Snapshot {
    /// The named subjects and the labels of the user's file, with the
    /// counts `counts` gives for a subject id and each kind's `libs`.
    pub fn of(file: &SubjectsFile, counts: impl Fn(&str) -> Option<(usize, usize, usize)>, libs: Vec<(Kind, Lib)>) -> Snapshot {
        let named = file
            .subjects
            .iter()
            .filter_map(|s| {
                let name = s.name.clone()?;
                Some(Named { kind: s.kind, sid: s.id.0.clone(), name, members: s.members.clone(), rejected: s.rejected.clone(), counts: counts(&s.id.0) })
            })
            .collect();
        Snapshot {
            named,
            added: file.added.iter().map(|l| (l.kind, l.face)).collect(),
            removed: file.removed.iter().map(|l| (l.kind, l.face)).collect(),
            not_same: file.not_same.iter().map(|n| (n.kind, n.a.clone(), n.b.clone())).collect(),
            dissolved: file.dissolved.iter().map(|d| (d.kind, d.heads.clone())).collect(),
            libs,
        }
    }
}

/// A kind's header: its library state, flattened (`heads`, `white`,
/// `model`, `grouping`, `cuts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindHeader {
    pub kind: Kind,
    #[serde(flatten)]
    pub lib: Lib,
}

/// A named group as exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// The subject's code (`fursuit-12`), the one the log uses.
    pub code: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photos: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bursts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<usize>,
    /// Its members: confirmed, named into it, moved into it, answered Yes.
    pub heads: Vec<Head>,
    /// "Not this one": heads answered No for it.
    #[serde(default)]
    pub rejected: Vec<Head>,
}

/// A labelled box (drawn or removed in the viewer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Boxed {
    pub kind: Kind,
    pub head: Head,
}

/// Two groups the user said are not the same, by their heads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotSame {
    pub kind: Kind,
    pub a: Vec<Head>,
    pub b: Vec<Head>,
}

/// A group the user dissolved (not one subject), by all its heads.  No
/// pair is labelled from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dissolved {
    pub kind: Kind,
    pub heads: Vec<Head>,
}

/// `groups.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupsFile {
    pub v: u32,
    /// When exported, UTC.
    pub t: String,
    pub app: String,
    pub kinds: Vec<KindHeader>,
    pub groups: Vec<Group>,
    #[serde(default)]
    pub added: Vec<Boxed>,
    #[serde(default)]
    pub removed: Vec<Boxed>,
    #[serde(default)]
    pub not_same: Vec<NotSame>,
    #[serde(default)]
    pub dissolved: Vec<Dissolved>,
}

/// The snapshot as exported: codes for names, from `codes` (already
/// holding the log's, so a subject has one code in both files).
pub fn export(snapshot: &Snapshot, codes: &mut Codes) -> GroupsFile {
    let heads = |v: &[FaceRef]| v.iter().copied().map(Head).collect::<Vec<_>>();
    let mut groups: Vec<Group> = snapshot
        .named
        .iter()
        .map(|n| {
            let code = codes.code(n.kind, Some(&n.sid), Some(&n.name)).unwrap_or_default();
            let (photos, bursts, days) = match n.counts {
                Some((p, b, d)) => (Some(p), Some(b), Some(d)),
                None => (None, None, None),
            };
            Group { code, kind: n.kind, photos, bursts, days, heads: heads(&n.members), rejected: heads(&n.rejected) }
        })
        .collect();
    groups.sort_by(|a, b| (a.kind as u8, code_number(&a.code)).cmp(&(b.kind as u8, code_number(&b.code))));
    let boxed = |v: &[(Kind, FaceRef)]| v.iter().map(|(k, f)| Boxed { kind: *k, head: Head(*f) }).collect::<Vec<_>>();
    GroupsFile {
        v: SCHEMA,
        t: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        app: env!("CARGO_PKG_VERSION").to_string(),
        kinds: snapshot.libs.iter().map(|(kind, lib)| KindHeader { kind: *kind, lib: lib.clone() }).collect(),
        groups,
        added: boxed(&snapshot.added),
        removed: boxed(&snapshot.removed),
        not_same: snapshot.not_same.iter().map(|(k, a, b)| NotSame { kind: *k, a: heads(a), b: heads(b) }).collect(),
        dissolved: snapshot.dissolved.iter().map(|(k, v)| Dissolved { kind: *k, heads: heads(v) }).collect(),
    }
}

fn code_number(code: &str) -> usize {
    code.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX)
}

/// The labelled pairs the groups hold: every pair within a named group
/// the same; every pair across two named groups of one kind different; a
/// rejected head different from its group's members; "not the same"
/// different across.  Each pair's `id` is `groups:<code>` (or
/// `groups:<code>/<code>` across).
///
/// Across groups grows with the square of the named heads: `across:
/// false` leaves it out.
pub fn group_pairs(file: &GroupsFile, across: bool, set: &mut PairSet) {
    for g in &file.groups {
        let id = format!("groups:{}", g.code);
        for (i, a) in g.heads.iter().enumerate() {
            for b in &g.heads[i + 1..] {
                set.put(g.kind, a.0, b.0, true, &id);
            }
            for r in &g.rejected {
                set.put(g.kind, a.0, r.0, false, &id);
            }
        }
    }
    if across {
        for (i, g) in file.groups.iter().enumerate() {
            for h in file.groups[i + 1..].iter().filter(|h| h.kind == g.kind) {
                let id = format!("groups:{}/{}", g.code, h.code);
                for a in &g.heads {
                    for b in &h.heads {
                        set.put(g.kind, a.0, b.0, false, &id);
                    }
                }
            }
        }
    }
    for (i, n) in file.not_same.iter().enumerate() {
        let id = format!("groups:not_same/{i}");
        for a in &n.a {
            for b in &n.b {
                set.put(n.kind, a.0, b.0, false, &id);
            }
        }
    }
}

/// The pairs of an export: the log's decisions in force first, then the
/// groups as they are now, which win for a pair both hold (they are the
/// later state).
pub fn pairs_with_groups(entries: &[Entry], groups: Option<&GroupsFile>, across: bool) -> Vec<Pair> {
    let mut set = PairSet::default();
    decisions::put_pairs(entries, &mut set);
    if let Some(g) = groups {
        group_pairs(g, across, &mut set);
    }
    set.into_pairs()
}

/// Read an export's folder: the log and the groups (either may be
/// missing).  **Blocks**: tests and the script driver.
pub fn read_export(dir: &Path) -> io::Result<(Vec<Entry>, Option<GroupsFile>)> {
    let (entries, _) = decisions::read(&dir.join(decisions::FILE_NAME))?;
    let groups = match std::fs::read_to_string(dir.join(FILE_NAME)) {
        Ok(text) => Some(serde_json::from_str(&text).map_err(io::Error::other)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok((entries, groups))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::decisions::{Act, Cuts, Recorder, Side};
    use crate::search::index::Key;

    fn head(n: u8, i: u16) -> FaceRef {
        FaceRef::new(Key([n; 16]), [i as f32 / 1000.0, 0.1, 0.05, 0.08])
    }

    fn lib() -> Lib {
        let g = crate::search::heads::GroupingCuts::of(crate::search::heads::Grouping::Looser, true);
        Lib { heads: 120, white: true, model: "siglip2-b16-256".into(), grouping: Some(g.grouping), cuts: Cuts { group: g.group, suggest: 0.711, strict: 0.671, sure_yes: Some(0.639), sure_no: Some(0.786), merge_suggest: Some(g.merge_suggest), merge_sure: Some(g.merge_sure), merge_far: Some(g.merge_far) } }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            named: vec![
                Named { kind: Kind::Fursuit, sid: "sid-sparky".into(), name: "Sparky Dragon".into(), members: vec![head(1, 0), head(1, 10), head(1, 20)], rejected: vec![head(9, 0)], counts: Some((3, 2, 1)) },
                Named { kind: Kind::Fursuit, sid: "sid-mossy".into(), name: "Mossy Wolf".into(), members: vec![head(2, 0), head(2, 10)], rejected: vec![], counts: Some((2, 2, 2)) },
                Named { kind: Kind::Person, sid: "sid-jane".into(), name: "Jane Example".into(), members: vec![head(3, 0), head(3, 10)], rejected: vec![], counts: None },
            ],
            added: vec![(Kind::Fursuit, head(4, 0))],
            removed: vec![(Kind::Person, head(5, 0))],
            not_same: vec![(Kind::Fursuit, vec![head(1, 0)], vec![head(6, 0)])],
            dissolved: vec![(Kind::Fursuit, vec![head(1, 30), head(8, 0)])],
            libs: vec![(Kind::Fursuit, lib())],
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pp-group-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The export's two files agree on codes, hold no name or subject id,
    /// carry the header, the groups with their counts and rejections, and
    /// the viewer's boxes; the pairs come back from both.
    #[test]
    fn the_groups_go_out_beside_the_log_with_its_codes_and_no_names() {
        let d = tmp("both");
        let r = Recorder::new(Some(d.join(decisions::FILE_NAME)));
        // The log knows Mossy Wolf (by id and name) before Sparky.
        let mossy = Side { sid: Some("sid-mossy".into()), name: Some("Mossy Wolf".into()), ..Side::new(2, None, None, [head(2, 0), head(2, 10)]) };
        r.record(Entry::new(Kind::Fursuit, Act::Merge { groups: vec![mossy, Side::new(1, None, None, [head(7, 0)])] }, None, None));
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (dir, n) = r.export(&out, Some(snapshot())).unwrap();
        assert_eq!(n, 1);
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        let log = std::fs::read_to_string(dir.join(decisions::FILE_NAME)).unwrap();
        for secret in ["Sparky", "Mossy", "Jane", "sid-"] {
            assert!(!text.contains(secret) && !log.contains(secret), "{secret} exported");
        }
        let (entries, groups) = read_export(&dir).unwrap();
        let g = groups.expect("groups.json");
        assert_eq!((g.v, g.app.as_str()), (SCHEMA, env!("CARGO_PKG_VERSION")));
        assert_eq!(g.kinds[0].lib, lib(), "the header: the choice, cut-offs, whitening, head count");
        assert!(text.contains(r#""grouping":"looser""#));
        let codes: Vec<(&str, Kind, usize)> = g.groups.iter().map(|x| (x.code.as_str(), x.kind, x.heads.len())).collect();
        assert_eq!(codes, [("person-1", Kind::Person, 2), ("fursuit-1", Kind::Fursuit, 2), ("fursuit-2", Kind::Fursuit, 3)], "the log's code for Mossy Wolf; a fresh one for Sparky");
        assert!(matches!(&entries[0].act, Act::Merge { groups } if groups[0].name.as_deref() == Some("fursuit-1")), "the same code in the log");
        let sparky = &g.groups[2];
        assert_eq!((sparky.photos, sparky.bursts, sparky.days, sparky.rejected.len()), (Some(3), Some(2), Some(1), 1));
        assert_eq!((g.added.len(), g.removed.len(), g.not_same.len(), g.dissolved.len()), (1, 1, 1, 1));
        assert_eq!((g.dissolved[0].kind, g.dissolved[0].heads.len()), (Kind::Fursuit, 2), "a dissolved group by its heads (it labels no pair)");
        // Pairs from both files.
        let ps = pairs_with_groups(&entries, Some(&g), true);
        let find = |a: FaceRef, b: FaceRef| ps.iter().find(|p| (p.a == a && p.b == b) || (p.a == b && p.b == a)).map(|p| (p.same, p.id.clone()));
        assert_eq!(find(head(1, 0), head(1, 20)), Some((true, "groups:fursuit-2".into())), "within a group");
        assert_eq!(find(head(1, 10), head(9, 0)).map(|x| x.0), Some(false), "rejected");
        assert_eq!(find(head(1, 0), head(2, 10)).map(|x| x.0), Some(false), "across named groups");
        assert_eq!(find(head(2, 0), head(7, 0)).map(|x| x.0), Some(true), "the log's merge");
        assert_eq!(find(head(1, 0), head(6, 0)).map(|x| x.0), Some(false), "not the same");
        assert_eq!(find(head(1, 0), head(3, 0)), None, "never across kinds");
        // 3 + 1 within/rejected (fursuit-2), 1 (fursuit-1), 1 (person-1),
        // 6 across, 2 from the log, 1 not the same.
        assert_eq!(ps.len(), 3 + 3 + 1 + 1 + 6 + 2 + 1);
        assert_eq!(pairs_with_groups(&entries, Some(&g), false).len(), ps.len() - 6);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A record left empty still exports the groups, and nothing at all
    /// is refused.
    #[test]
    fn the_groups_go_out_with_an_empty_record_and_nothing_is_refused() {
        let d = tmp("empty");
        let r = Recorder::new(Some(d.join(decisions::FILE_NAME)));
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(r.export(&out, None).is_err(), "nothing to export");
        assert!(r.export(&out, Some(Snapshot::default())).is_err(), "no group, no label: nothing either");
        let (dir, n) = r.export(&out, Some(snapshot())).unwrap();
        assert_eq!(n, 0);
        assert!(dir.join(FILE_NAME).exists() && !dir.join(decisions::FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
