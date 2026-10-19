//! RawTherapee profiles for RAWs copied for editing.
//!
//! RawTherapee keeps a photo's develop settings beside it in
//! `<file name>.pp3` (`IMG_0042.ARW` → `IMG_0042.ARW.pp3`) and opens a RAW
//! with that profile.  Copy marked photos for editing can put one beside
//! each RAW it places, as a starting point: the built-in Base profile, a
//! `.pp3` the user chose, or none.
//!
//! * **Never overwritten.**  A `.pp3` already beside a RAW may hold edits
//!   made in RawTherapee, so it is kept as it is; the write itself is
//!   no-replace ([`fsutil::atomic_write`]), so one appearing meanwhile is
//!   kept too.
//! * **Read once.**  A custom profile is read and checked once per run, on
//!   the worker ([`load`]); each RAW gets a copy of those bytes.
//! * **The crop goes in.**  RawTherapee reads its `.pp3`, not the `.xmp`
//!   sidecar that carries the crop for Lightroom, so a cropped photo's
//!   profile gets the crop in its `[Crop]` section ([`with_crop`]); every
//!   other line stays byte for byte.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fsutil;
use crate::imaging::libraw;
use crate::model::crop::Crop;
use crate::util;

/// The built-in default profile (RawTherapee 5.13), carried in the binary.
pub const BASE: &str = include_str!("../../assets/rawtherapee/Base.pp3");
/// What the dialog calls it.
pub const BASE_NAME: &str = "Base";
/// A profile larger than this is refused (RawTherapee's are ~40 KB).
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// RawTherapee's default `[RAW Bayer] Border`: pixels it trims from each
/// edge of the sensor's area.
const DEFAULT_BORDER: u32 = 4;

/// Which profile the edit copy gives each RAW (a saved setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    /// The built-in [`BASE`].
    #[default]
    Base,
    /// A `.pp3` the user chose.
    Custom,
    /// No profile.
    None,
}

/// The profile for one run, as the worker is handed it: a path only, read
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileChoice {
    None,
    Base,
    Custom(PathBuf),
}

impl ProfileChoice {
    pub fn is_none(&self) -> bool {
        *self == ProfileChoice::None
    }
}

/// A profile read and checked, ready to write beside each RAW.
#[derive(Debug, Clone)]
pub struct Profile {
    pub text: String,
    /// Its crop can't be set: its coarse transformation turns or flips the
    /// frame, so a crop worked out for the upright photo would land wrong.
    turns_frame: bool,
    /// Its `[RAW Bayer] Border`.
    border: u32,
}

impl Profile {
    fn new(text: String) -> Self {
        let coarse = |key: &str| ini_value(&text, "Coarse Transformation", key).map(str::trim);
        let turns_frame = coarse("Rotate").is_some_and(|v| v != "0")
            || coarse("HorizontalFlip").is_some_and(|v| v == "true")
            || coarse("VerticalFlip").is_some_and(|v| v == "true");
        let border = ini_value(&text, "RAW Bayer", "Border").and_then(|v| v.trim().parse().ok()).unwrap_or(DEFAULT_BORDER);
        Profile { text, turns_frame, border }
    }
}

/// Check a profile's bytes: text, under [`MAX_BYTES`], with a `[Version]`
/// section.  The error says why not, to follow "<name> can't be used: ".
pub fn validate(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("it's {}, and a profile is under 2 MB", util::fmt_bytes(bytes.len() as u64)));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't a text file".to_string())?;
    if text.contains('\0') {
        return Err("it isn't a text file".into());
    }
    if !text.lines().any(|l| l.trim_start_matches('\u{feff}').trim() == "[Version]") {
        return Err("it isn't a RawTherapee profile (it has no [Version] section)".into());
    }
    Ok(text.to_string())
}

/// Read and check a custom profile (file I/O: a worker's job).
pub fn read_custom(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| io_reason(&e))?;
    if !meta.is_file() {
        return Err("it isn't a file".into());
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("it's {}, and a profile is under 2 MB", util::fmt_bytes(meta.len())));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path).and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes)).map_err(|e| io_reason(&e))?;
    validate(&bytes)
}

fn io_reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "it's no longer there".into(),
        io::ErrorKind::PermissionDenied => "it can't be read (permission denied)".into(),
        _ => format!("it can't be read ({e})"),
    }
}

/// The profile to write, read once for a run.  `Ok(None)`: no profile.
/// The error names the file: "Portrait.pp3 can't be used: it's no longer
/// there".
pub fn load(choice: &ProfileChoice) -> Result<Option<Profile>, String> {
    match choice {
        ProfileChoice::None => Ok(None),
        ProfileChoice::Base => Ok(Some(Profile::new(BASE.to_string()))),
        ProfileChoice::Custom(path) => {
            read_custom(path).map(|t| Some(Profile::new(t))).map_err(|why| format!("{} can't be used: {why}", util::file_name(path)))
        }
    }
}

/// `IMG_0042.ARW` → `IMG_0042.ARW.pp3`.
pub fn sidecar_path(raw: &Path) -> PathBuf {
    let mut s = raw.as_os_str().to_owned();
    s.push(".pp3");
    PathBuf::from(s)
}

/// What [`write_beside`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Written; `crop`: with the photo's crop in it.
    Written { crop: bool },
    /// A profile was already there, and is kept.
    Kept,
}

/// Put `profile` beside `raw` as `<raw>.pp3`, never replacing one that is
/// there.  A crop that isn't the full frame goes into its `[Crop]` when
/// the RAW's frame can be read and the profile doesn't turn the frame.
pub fn write_beside(raw: &Path, profile: &Profile, crop: Option<&Crop>) -> Result<Outcome, String> {
    let dest = sidecar_path(raw);
    if fsutil::lexists(&dest) {
        return Ok(Outcome::Kept);
    }
    let cropped = crop.filter(|c| !c.is_full()).filter(|_| !profile.turns_frame).and_then(|c| {
        let geometry = raw_geometry(raw, profile.border)?;
        with_crop(&profile.text, geometry.rect(c))
    });
    let with = cropped.is_some();
    let text = cropped.as_deref().unwrap_or(&profile.text);
    match fsutil::atomic_write(&dest, text.as_bytes(), false) {
        Ok(()) => Ok(Outcome::Written { crop: with }),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Outcome::Kept),
        Err(e) => Err(e.to_string()),
    }
}

// ── the crop, in RawTherapee's pixels ───────────────────────────────────────

/// RawTherapee's picture of a RAW, upright: its frame (`width` × `height`,
/// what it shows uncropped) and where in it the camera's own frame lies —
/// the frame a crop in this app is a fraction of (the embedded preview's
/// and the camera JPG's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// The camera's frame: left, top, width, height (left and top may be a
    /// pixel or two outside RawTherapee's frame).
    pub camera: (f64, f64, f64, f64),
}

/// A crop rectangle in RawTherapee's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Geometry {
    /// Work out RawTherapee's frame from LibRaw's reading of the header.
    ///
    /// RawTherapee shows the sensor's area less `border` pixels on each
    /// side; LibRaw's visible area is that area (both come from dcraw).
    /// The camera's frame sits at its default-crop origin less the border;
    /// a file that gives no origin has it centred.  RawTherapee's frame
    /// ends as far past the camera's on the right and bottom as it starts
    /// before it, or at LibRaw's edge if that is nearer.  Checked against
    /// `rawtherapee-cli` renders of a Sony A7 V, a Sony A7R II and a Canon
    /// EOS Rebel T6 (camera JPG located in the render to the pixel).  When
    /// LibRaw's visible area is wider than RawTherapee's (a camera whose
    /// masked edge LibRaw doesn't trim) the camera frame still sits at its
    /// origin less the border; RawTherapee clamps a crop to its frame.
    pub fn from_frame(f: &libraw::Frame, orientation: u8, border: u32) -> Option<Geometry> {
        let (mut fw, mut fh) = (f.width.checked_sub(2 * border)?, f.height.checked_sub(2 * border)?);
        if fw == 0 || fh == 0 {
            return None;
        }
        let (ox, oy, cw, ch) = match f.crop {
            Some((cl, ct, cw, ch)) if cl > 0 || ct > 0 => (
                cl as f64 - f.left_margin as f64 - border as f64,
                ct as f64 - f.top_margin as f64 - border as f64,
                cw as f64,
                ch as f64,
            ),
            Some((_, _, cw, ch)) => ((fw as f64 - cw as f64) / 2.0, (fh as f64 - ch as f64) / 2.0, cw as f64, ch as f64),
            None => (0.0, 0.0, fw as f64, fh as f64),
        };
        // RawTherapee trims the masked edge LibRaw may leave in (the A7 V's
        // visible area is 7168 wide, RawTherapee's 7024): its frame reaches
        // no further past the camera's on the far side than on the near.
        let fit = |frame: u32, origin: f64, size: f64| {
            if origin >= 0.0 { frame.min((size + 2.0 * origin).round() as u32) } else { frame }
        };
        fw = fit(fw, ox, cw);
        fh = fit(fh, oy, ch);
        let (fwf, fhf) = (fw as f64, fh as f64);
        // Turn the sensor's picture upright as RawTherapee does (EXIF
        // orientation: 3 half a turn, 6 a quarter clockwise, 8 anticlockwise).
        Some(match orientation {
            3 => Geometry { width: fw, height: fh, camera: (fwf - ox - cw, fhf - oy - ch, cw, ch) },
            6 => Geometry { width: fh, height: fw, camera: (fhf - oy - ch, ox, ch, cw) },
            8 => Geometry { width: fh, height: fw, camera: (oy, fwf - ox - cw, ch, cw) },
            _ => Geometry { width: fw, height: fh, camera: (ox, oy, cw, ch) },
        })
    }

    /// `crop` (fractions of the upright camera frame) in RawTherapee's
    /// pixels, inside its frame.
    pub fn rect(&self, crop: &Crop) -> Rect {
        let (cx, cy, cw, ch) = self.camera;
        let span = |a: f64, b: f64, origin: f64, size: f64, limit: u32| {
            let lo = (origin + a * size).round().clamp(0.0, (limit - 1) as f64) as u32;
            let hi = (origin + b * size).round().clamp(0.0, limit as f64) as u32;
            (lo, hi.max(lo + 1) - lo)
        };
        let (x, w) = span(crop.left, crop.right, cx, cw, self.width);
        let (y, h) = span(crop.top, crop.bottom, cy, ch, self.height);
        Rect { x, y, w, h }
    }
}

/// Open the RAW's header and work out its geometry.  `None` when LibRaw
/// can't read it.
fn raw_geometry(raw: &Path, border: u32) -> Option<Geometry> {
    let mut r = libraw::Raw::open(raw).ok()?;
    let orientation = r.exif_orientation();
    Geometry::from_frame(&r.frame(), orientation, border)
}

/// The keys of `[Crop]` this sets.  `FixedRatio` goes off so RawTherapee's
/// crop tool doesn't pull the crop to the image's ratio when it is moved.
fn crop_keys(r: Rect) -> [(&'static str, String); 6] {
    [
        ("Enabled", "true".into()),
        ("X", r.x.to_string()),
        ("Y", r.y.to_string()),
        ("W", r.w.to_string()),
        ("H", r.h.to_string()),
        ("FixedRatio", "false".into()),
    ]
}

/// `text` with its `[Crop]` set to `r`.  Only those keys' lines change;
/// one missing is added at the end of the section, and a profile with no
/// `[Crop]` gets one at its end.  Every other byte is kept.
pub fn with_crop(text: &str, r: Rect) -> Option<String> {
    let keys = crop_keys(r);
    let mut seen = [false; 6];
    let mut out: Vec<String> = Vec::new();
    let mut section: Option<String> = None;
    let mut crop_end: Option<usize> = None; // after the section's last key
    let mut newline = "\n";
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix("\r\n") {
            Some(b) => (b, "\r\n"),
            None => match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            },
        };
        if end == "\r\n" {
            newline = "\r\n";
        }
        if let Some(name) = section_name(body) {
            section = Some(name.to_string());
            out.push(line.to_string());
            if name == "Crop" {
                crop_end = Some(out.len());
            }
            continue;
        }
        if section.as_deref() == Some("Crop") {
            if let Some((k, _)) = body.split_once('=') {
                if let Some(i) = keys.iter().position(|(key, _)| *key == k.trim()) {
                    seen[i] = true;
                    out.push(format!("{}={}{end}", keys[i].0, keys[i].1));
                    crop_end = Some(out.len());
                    continue;
                }
                crop_end = Some(out.len() + 1);
            }
        }
        out.push(line.to_string());
    }
    let missing: Vec<String> = keys.iter().zip(seen).filter(|(_, s)| !s).map(|((k, v), _)| format!("{k}={v}{newline}")).collect();
    match crop_end {
        Some(at) => {
            // The line before may lack a newline (the file's last line).
            if !missing.is_empty() && at > 0 && !out[at - 1].ends_with('\n') {
                out[at - 1].push_str(newline);
            }
            out.splice(at..at, missing);
        }
        None => {
            if out.last().is_some_and(|l| !l.ends_with('\n')) {
                out.push(newline.to_string());
            }
            out.push(format!("{newline}[Crop]{newline}"));
            out.extend(missing);
        }
    }
    Some(out.concat())
}

fn section_name(line: &str) -> Option<&str> {
    let t = line.trim().trim_start_matches('\u{feff}');
    t.strip_prefix('[')?.strip_suffix(']')
}

/// A key's value in a section (the first one), for reading a profile.
fn ini_value<'a>(text: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in text.lines() {
        if let Some(name) = section_name(line) {
            inside = name == section;
            continue;
        }
        if inside {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(l: f64, t: f64, r: f64, b: f64) -> Crop {
        Crop { left: l, top: t, right: r, bottom: b, aspect_label: None }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-rtprofile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_embedded_base_is_the_asset_byte_for_byte() {
        let asset = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rawtherapee/Base.pp3")).unwrap();
        assert_eq!(BASE.as_bytes(), &asset[..]);
        assert!(validate(BASE.as_bytes()).is_ok());
        let p = load(&ProfileChoice::Base).unwrap().unwrap();
        assert!(!p.turns_frame);
        assert_eq!(p.border, 4);
    }

    #[test]
    fn the_sidecar_is_the_whole_file_name_plus_pp3() {
        assert_eq!(sidecar_path(Path::new("/home/me/edits/20261012_2044_2048.ARW")), PathBuf::from("/home/me/edits/20261012_2044_2048.ARW.pp3"));
        assert_eq!(sidecar_path(Path::new("/home/me/edits/20261012_2044_2048_1.ARW")), PathBuf::from("/home/me/edits/20261012_2044_2048_1.ARW.pp3"));
    }

    #[test]
    fn invalid_or_missing_custom_profiles_are_refused() {
        let d = dir("refuse");
        let none = d.join("gone.pp3");
        let err = load(&ProfileChoice::Custom(none)).unwrap_err();
        assert_eq!(err, "gone.pp3 can't be used: it's no longer there");

        let not_pp3 = d.join("notes.pp3");
        fs::write(&not_pp3, "[General]\nColorLabel=0\n").unwrap();
        assert!(load(&ProfileChoice::Custom(not_pp3)).unwrap_err().contains("no [Version] section"));

        let binary = d.join("photo.pp3");
        fs::write(&binary, [0xFFu8, 0xD8, 0xFF, 0xE0, 0, 0x10]).unwrap();
        assert!(load(&ProfileChoice::Custom(binary)).unwrap_err().contains("isn't a text file"));

        let big = d.join("big.pp3");
        let mut text = String::from("[Version]\nAppVersion=5.13\n");
        text.push_str(&"x".repeat(MAX_BYTES as usize));
        fs::write(&big, text).unwrap();
        assert!(load(&ProfileChoice::Custom(big)).unwrap_err().contains("under 2 MB"));

        assert!(load(&ProfileChoice::Custom(d.clone())).unwrap_err().contains("isn't a file"));

        let good = d.join("Portrait.pp3");
        fs::write(&good, "[Version]\nAppVersion=5.13\nVersion=353\n").unwrap();
        assert_eq!(load(&ProfileChoice::Custom(good)).unwrap().unwrap().text, "[Version]\nAppVersion=5.13\nVersion=353\n");
        assert!(load(&ProfileChoice::None).unwrap().is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_existing_profile_is_never_overwritten() {
        let d = dir("keep");
        let raw = d.join("IMG_0001.ARW");
        fs::write(&raw, b"raw").unwrap();
        fs::write(sidecar_path(&raw), b"edited in RawTherapee").unwrap();
        let p = load(&ProfileChoice::Base).unwrap().unwrap();
        assert_eq!(write_beside(&raw, &p, None).unwrap(), Outcome::Kept);
        assert_eq!(fs::read(sidecar_path(&raw)).unwrap(), b"edited in RawTherapee");
        let fresh = d.join("IMG_0002.ARW");
        fs::write(&fresh, b"raw").unwrap();
        assert_eq!(write_beside(&fresh, &p, None).unwrap(), Outcome::Written { crop: false });
        assert_eq!(fs::read_to_string(sidecar_path(&fresh)).unwrap(), BASE);
        assert!(!fsutil::tmp_sibling(&sidecar_path(&fresh)).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_crop_on_a_file_libraw_cant_read_leaves_the_profile_as_it_is() {
        let d = dir("unreadable");
        let raw = d.join("IMG_0003.ARW");
        fs::write(&raw, b"not a raw").unwrap();
        let p = load(&ProfileChoice::Base).unwrap().unwrap();
        assert_eq!(write_beside(&raw, &p, Some(&crop(0.1, 0.1, 0.9, 0.9))).unwrap(), Outcome::Written { crop: false });
        assert_eq!(fs::read_to_string(sidecar_path(&raw)).unwrap(), BASE);
        let _ = fs::remove_dir_all(&d);
    }

    /// The lines of `a` and `b` that differ, as (a's, b's).
    fn changed<'a>(a: &'a str, b: &'a str) -> Vec<(&'a str, &'a str)> {
        a.split_inclusive('\n').zip(b.split_inclusive('\n')).filter(|(x, y)| x != y).collect()
    }

    #[test]
    fn the_crop_merge_changes_only_the_crop_keys() {
        let r = Rect { x: 708, y: 471, w: 5608, h: 3738 };
        let merged = with_crop(BASE, r).unwrap();
        assert_eq!(merged.split_inclusive('\n').count(), BASE.split_inclusive('\n').count(), "no line added or lost");
        // Every changed line is in [Crop], and says what it should.
        let crop_lines: Vec<&str> = changed(BASE, &merged).iter().map(|(_, b)| *b).collect();
        assert_eq!(crop_lines, ["Enabled=true\n", "X=708\n", "Y=471\n", "W=5608\n", "H=3738\n", "FixedRatio=false\n"]);
        assert_eq!(ini_value(&merged, "Crop", "Ratio"), Some("As Image"));
        // Enabled=false elsewhere (other sections) is untouched.
        assert_eq!(merged.matches("Enabled=false").count(), BASE.matches("Enabled=false").count() - 1);
    }

    #[test]
    fn the_crop_merge_keeps_crlf_and_fills_in_missing_keys() {
        let text = "[Version]\r\nVersion=353\r\n\r\n[Crop]\r\nEnabled=false\r\nRatio=3:2\r\n\r\n[Resize]\r\nEnabled=false\r\n";
        let merged = with_crop(text, Rect { x: 1, y: 2, w: 3, h: 4 }).unwrap();
        assert_eq!(
            merged,
            "[Version]\r\nVersion=353\r\n\r\n[Crop]\r\nEnabled=true\r\nRatio=3:2\r\nX=1\r\nY=2\r\nW=3\r\nH=4\r\nFixedRatio=false\r\n\r\n[Resize]\r\nEnabled=false\r\n"
        );
        // No [Crop] at all, and no newline at the end.
        let merged = with_crop("[Version]\nVersion=353", Rect { x: 1, y: 2, w: 3, h: 4 }).unwrap();
        assert_eq!(merged, "[Version]\nVersion=353\n\n[Crop]\nEnabled=true\nX=1\nY=2\nW=3\nH=4\nFixedRatio=false\n");
    }

    #[test]
    fn a_profile_that_turns_the_frame_gets_no_crop() {
        let turned = BASE.replace("[Coarse Transformation]\nRotate=0", "[Coarse Transformation]\nRotate=90");
        assert_ne!(turned, BASE);
        assert!(Profile::new(turned).turns_frame);
        let flipped = BASE.replace("HorizontalFlip=false", "HorizontalFlip=true");
        assert!(Profile::new(flipped).turns_frame);
    }

    /// LibRaw's readings of real files, with where `rawtherapee-cli` put
    /// the camera's frame in its render (located to the pixel).
    fn a7v() -> libraw::Frame {
        libraw::Frame { width: 7168, height: 5120, left_margin: 0, top_margin: 0, crop: Some((12, 8, 7008, 4672)) }
    }

    #[test]
    fn geometry_matches_rawtherapee_renders() {
        // Sony A7 V, landscape: RawTherapee 7024×4680, camera frame at 8,4.
        let g = Geometry::from_frame(&a7v(), 1, 4).unwrap();
        assert_eq!(g.camera, (8.0, 4.0, 7008.0, 4672.0));
        assert_eq!(g.rect(&crop(0.0, 0.0, 1.0, 1.0)), Rect { x: 8, y: 4, w: 7008, h: 4672 });
        // Portrait (EXIF 8): RawTherapee 4680×7024, camera frame at 4,8.
        let g = Geometry::from_frame(&a7v(), 8, 4).unwrap();
        assert_eq!((g.width, g.height), (4680, 7024));
        assert_eq!(g.rect(&crop(0.0, 0.0, 1.0, 1.0)), Rect { x: 4, y: 8, w: 4672, h: 7008 });
        // Canon EOS Rebel T6: RawTherapee 5194×3456, camera frame at 6,0.
        let t6 = libraw::Frame { width: 5202, height: 3464, left_margin: 142, top_margin: 52, crop: Some((152, 56, 5184, 3456)) };
        let g = Geometry::from_frame(&t6, 1, 4).unwrap();
        assert_eq!((g.width, g.height), (5194, 3456));
        assert_eq!(g.rect(&crop(0.0, 0.0, 1.0, 1.0)), Rect { x: 6, y: 0, w: 5184, h: 3456 });
        // Sony A7R II (no crop origin in the file): RawTherapee 7960×5312,
        // camera frame centred at 4,4.
        let a7r2 = libraw::Frame { width: 7968, height: 5320, left_margin: 0, top_margin: 0, crop: Some((0, 0, 7952, 5304)) };
        let g = Geometry::from_frame(&a7r2, 1, 4).unwrap();
        assert_eq!((g.width, g.height), (7960, 5312));
        assert_eq!(g.rect(&crop(0.0, 0.0, 1.0, 1.0)), Rect { x: 4, y: 4, w: 7952, h: 5304 });
    }

    #[test]
    fn a_crop_is_turned_with_the_photo() {
        // A 100×50 sensor with no border, the crop the left half of the
        // upright photo.
        let f = libraw::Frame { width: 100, height: 50, left_margin: 0, top_margin: 0, crop: None };
        let left_half = crop(0.0, 0.0, 0.5, 1.0);
        assert_eq!(Geometry::from_frame(&f, 1, 0).unwrap().rect(&left_half), Rect { x: 0, y: 0, w: 50, h: 50 });
        assert_eq!(Geometry::from_frame(&f, 3, 0).unwrap().rect(&left_half), Rect { x: 0, y: 0, w: 50, h: 50 });
        let g6 = Geometry::from_frame(&f, 6, 0).unwrap();
        assert_eq!((g6.width, g6.height), (50, 100));
        assert_eq!(g6.rect(&left_half), Rect { x: 0, y: 0, w: 25, h: 100 });
        // An off-centre camera frame moves with the turn.
        let f = libraw::Frame { width: 100, height: 50, left_margin: 0, top_margin: 0, crop: Some((10, 0, 80, 50)) };
        let all = crop(0.0, 0.0, 1.0, 1.0);
        assert_eq!(Geometry::from_frame(&f, 1, 0).unwrap().rect(&all), Rect { x: 10, y: 0, w: 80, h: 50 });
        assert_eq!(Geometry::from_frame(&f, 6, 0).unwrap().rect(&all), Rect { x: 0, y: 10, w: 50, h: 80 });
        assert_eq!(Geometry::from_frame(&f, 8, 0).unwrap().rect(&all), Rect { x: 0, y: 10, w: 50, h: 80 });
    }

    #[test]
    fn a_crop_stays_inside_the_frame() {
        // A camera frame a pixel outside RawTherapee's on the left.
        let f = libraw::Frame { width: 108, height: 58, left_margin: 0, top_margin: 0, crop: Some((3, 4, 100, 50)) };
        let g = Geometry::from_frame(&f, 1, 4).unwrap();
        assert_eq!(g.camera.0, -1.0);
        let r = g.rect(&crop(0.0, 0.0, 1.0, 1.0));
        assert_eq!(r, Rect { x: 0, y: 0, w: 99, h: 50 });
        assert!(Geometry::from_frame(&libraw::Frame { width: 8, height: 8, left_margin: 0, top_margin: 0, crop: None }, 1, 4).is_none());
    }
}
