//! Metadata for the viewer's EXIF side panel.
//!
//! Same field set, labels and formatting as the Python app:
//!
//! 1. the full EXIF reader (Pillow's role) for JPEGs and TIFF-based RAWs;
//! 2. for RAWs, a minimal IFD walker fills whatever that missed;
//! 3. when showing a RAW whose JPG pair exists, the JPG fills any missing
//!    camera / exposure / lens fields;
//! 4. `exiftool`, run separately, fills lens data and remaining gaps.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{Context, Field, In, Tag, Value};

use crate::exif::{self as tiff, Endian, FileSource, Source};
use crate::model::record::FileType;
use crate::util::fmt_g;

/// Ordered sections: (title, [(label, value)]).
pub type Sections = Vec<(String, Vec<(String, String)>)>;

type Flat = Vec<(String, String)>;

fn set_default(flat: &mut Flat, k: &str, v: String) {
    if !flat.iter().any(|(key, _)| key == k) {
        flat.push((k.to_string(), v));
    }
}

fn has(flat: &Flat, k: &str) -> bool {
    flat.iter().any(|(key, _)| key == k)
}

const CAMERA_KEYS: [&str; 3] = ["Make", "Model", "Software"];
const EXPOSURE_KEYS: [&str; 12] = [
    "Date", "ISO", "Aperture", "Shutter", "EV Comp", "Exp Program", "Exp Mode", "Metering",
    "Flash", "White Balance", "Scene", "Subject Dist",
];
const LENS_KEYS: [&str; 5] = ["Lens Make", "Lens Model", "Lens Spec", "Focal Length", "35mm Equiv"];
const SNEAKY_KEYS: [&str; 20] = [
    "Make", "Model", "Software", "Date", "ISO", "Aperture", "Shutter", "EV Comp", "Exp Program",
    "Exp Mode", "Metering", "Flash", "White Balance", "Scene", "Subject Dist", "Focal Length",
    "35mm Equiv", "Lens Make", "Lens Model", "Lens Spec",
];

pub fn sections_from_flat(flat: &Flat) -> Sections {
    let get = |k: &str| flat.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let mut out = Sections::new();
    let groups: [(&str, &[&str]); 4] =
        [("CAMERA", &CAMERA_KEYS), ("EXPOSURE", &EXPOSURE_KEYS), ("LENS", &LENS_KEYS), ("GPS", &["GPS"])];
    for (name, keys) in groups {
        let rows: Vec<(String, String)> =
            keys.iter().filter_map(|k| get(k).map(|v| (k.to_string(), v))).collect();
        if !rows.is_empty() {
            out.push((name.to_string(), rows));
        }
    }
    out
}

pub fn flatten(sections: &Sections) -> Flat {
    sections.iter().flat_map(|(_, rows)| rows.iter().cloned()).collect()
}

/// Read the panel's sections for `path` (and its JPG pair, if any).
pub fn read_sections(path: &Path, file_type: FileType, jpg_pair: Option<&Path>) -> Sections {
    let mut flat = read_full(path).unwrap_or_default();
    if file_type == FileType::Raw {
        for (k, v) in read_tiff_walker(path) {
            set_default(&mut flat, &k, v);
        }
    }
    if let Some(jpg) = jpg_pair {
        if SNEAKY_KEYS.iter().any(|k| !has(&flat, k)) {
            if let Some(jflat) = read_full(jpg) {
                for (k, v) in jflat {
                    if SNEAKY_KEYS.contains(&k.as_str()) {
                        set_default(&mut flat, &k, v);
                    }
                }
            }
        }
    }
    sections_from_flat(&flat)
}

// ── full EXIF reader (Pillow's role) ────────────────────────────────────────

fn ifd0<'a>(ex: &'a exif::Exif, n: u16) -> Option<&'a Field> {
    ex.get_field(Tag(Context::Tiff, n), In::PRIMARY)
}

fn gps<'a>(ex: &'a exif::Exif, n: u16) -> Option<&'a Field> {
    ex.get_field(Tag(Context::Gps, n), In::PRIMARY)
}

fn text(f: Option<&Field>) -> Option<String> {
    match &f?.value {
        Value::Ascii(parts) => {
            let s: Vec<String> = parts.iter().map(|p| tiff::ascii(p)).collect();
            Some(s.join("").trim().to_string())
        }
        _ => None,
    }
}

/// A single numeric value (None for NaN / multi-valued / non-numeric).
fn num(f: Option<&Field>) -> Option<f64> {
    let v = match &f?.value {
        Value::Rational(r) if r.len() == 1 => rational(r[0].num as f64, r[0].denom as f64),
        Value::SRational(r) if r.len() == 1 => rational(r[0].num as f64, r[0].denom as f64),
        Value::Short(s) if s.len() == 1 => s[0] as f64,
        Value::Long(s) if s.len() == 1 => s[0] as f64,
        Value::SShort(s) if s.len() == 1 => s[0] as f64,
        Value::SLong(s) if s.len() == 1 => s[0] as f64,
        Value::Byte(s) if s.len() == 1 => s[0] as f64,
        _ => return None,
    };
    (!v.is_nan()).then_some(v)
}

fn rational(n: f64, d: f64) -> f64 {
    if d == 0.0 { f64::NAN } else { n / d }
}

fn nums(f: Option<&Field>) -> Vec<f64> {
    match f.map(|f| &f.value) {
        Some(Value::Rational(r)) => r.iter().map(|x| rational(x.num as f64, x.denom as f64)).collect(),
        Some(Value::SRational(r)) => r.iter().map(|x| rational(x.num as f64, x.denom as f64)).collect(),
        Some(Value::Short(s)) => s.iter().map(|&x| x as f64).collect(),
        Some(Value::Long(s)) => s.iter().map(|&x| x as f64).collect(),
        _ => Vec::new(),
    }
}

/// Python truthiness of a tag value for the `a or b` fallbacks.
fn truthy(f: Option<&Field>) -> bool {
    match f {
        None => false,
        Some(f) => match num(Some(f)) {
            Some(v) => v != 0.0,
            None => true,
        },
    }
}

/// `ifd.get(tag) or exif.get(tag)`
fn either<'a>(ex: &'a exif::Exif, n: u16) -> Option<&'a Field> {
    let e = ex.get_field(Tag(Context::Exif, n), In::PRIMARY);
    if truthy(e) { e } else { ifd0(ex, n) }
}

fn exp_program(v: u32) -> Option<&'static str> {
    Some(match v {
        0 => "Not defined",
        1 => "Manual",
        2 => "Program",
        3 => "Aperture priority",
        4 => "Shutter priority",
        5 => "Creative",
        6 => "Action",
        7 => "Portrait",
        8 => "Landscape",
        _ => return None,
    })
}

fn metering(v: u32) -> Option<&'static str> {
    Some(match v {
        0 => "Unknown",
        1 => "Average",
        2 => "Center-weighted",
        3 => "Spot",
        4 => "Multi-spot",
        5 => "Multi-segment",
        6 => "Partial",
        255 => "Other",
        _ => return None,
    })
}

fn flash_code(v: u32) -> Option<&'static str> {
    Some(match v {
        0x00 => "No flash",
        0x01 => "Flash fired",
        0x05 => "Flash, no rtn",
        0x07 => "Flash, strobe",
        0x08 | 0x10 => "Off",
        0x18 => "Auto – no flash",
        0x19 => "Auto – flash",
        0x1d => "Auto – no rtn",
        0x1f => "Auto – strobe",
        0x20 | 0x30 => "No function",
        0x41 => "Red-eye",
        0x45 => "Red-eye, no rtn",
        0x47 => "Red-eye, strobe",
        0x49 => "Auto, red-eye",
        _ => return None,
    })
}

pub fn shutter_text(et: f64) -> String {
    if et > 0.0 && et < 1.0 {
        format!("1/{} s", (1.0 / et).round_ties_even() as i64)
    } else {
        format!("{} s", fmt_g(et))
    }
}

fn strip_make(make: &str, model: &str) -> String {
    if !make.is_empty() && model.to_uppercase().starts_with(&make.to_uppercase()) {
        model.chars().skip(make.chars().count()).collect::<String>().trim().to_string()
    } else {
        model.to_string()
    }
}

/// Everything the panel shows, read with a full EXIF parser.
pub fn read_full(path: &Path) -> Option<Flat> {
    let f = File::open(path).ok()?;
    let ex = exif::Reader::new().read_from_container(&mut BufReader::new(f)).ok()?;
    let mut out: Flat = Vec::new();
    let mut put = |k: &str, v: String| out.push((k.to_string(), v));

    let make = text(ifd0(&ex, 271)).unwrap_or_default();
    let model = strip_make(&make, &text(ifd0(&ex, 272)).unwrap_or_default());
    if !make.is_empty() {
        put("Make", make.clone());
    }
    if !model.is_empty() {
        put("Model", model);
    }
    if let Some(sw) = text(ifd0(&ex, 305)).filter(|s| !s.is_empty()) {
        put("Software", sw);
    }

    let dt = [36867u16, 36868]
        .iter()
        .find_map(|&t| text(ex.get_field(Tag(Context::Exif, t), In::PRIMARY)).filter(|s| !s.is_empty()))
        .or_else(|| text(ifd0(&ex, 306)).filter(|s| !s.is_empty()));
    if let Some(dt) = dt {
        let chars: Vec<char> = dt.trim().chars().collect();
        let d: String = chars.iter().take(10).collect::<String>().replace(':', "-");
        let t: String = chars.iter().skip(11).take(5).collect();
        put("Date", format!("{d}  {t}"));
    }

    if let Some(iso) = num(either(&ex, 34855)).filter(|&v| v != 0.0) {
        put("ISO", format!("{}", iso as i64));
    }
    if let Some(fnum) = num(either(&ex, 33437)) {
        put("Aperture", format!("f/{}", fmt_g(fnum)));
    }
    if let Some(et) = num(either(&ex, 33434)) {
        put("Shutter", shutter_text(et));
    }
    if let Some(ev) = num(ex.get_field(Tag(Context::Exif, 37380), In::PRIMARY)) {
        put("EV Comp", format!("{:+.1} EV", ev));
    }
    if let Some(p) = num(ex.get_field(Tag(Context::Exif, 34850), In::PRIMARY)).and_then(|v| exp_program(v as u32)) {
        put("Exp Program", p.into());
    }
    if let Some(m) = num(ex.get_field(Tag(Context::Exif, 41986), In::PRIMARY)).and_then(|v| match v as u32 {
        0 => Some("Auto"),
        1 => Some("Manual"),
        2 => Some("Auto bracket"),
        _ => None,
    }) {
        put("Exp Mode", m.into());
    }
    if let Some(m) = num(either(&ex, 37383)).and_then(|v| metering(v as u32)) {
        put("Metering", m.into());
    }
    if let Some(fl) = num(either(&ex, 37385)) {
        let fi = fl as u32;
        put("Flash", flash_code(fi).unwrap_or(if fi & 1 != 0 { "Flash fired" } else { "No flash" }).into());
    }
    if let Some(wb) = num(ex.get_field(Tag(Context::Exif, 41987), In::PRIMARY)).and_then(|v| match v as u32 {
        0 => Some("Auto"),
        1 => Some("Manual"),
        _ => None,
    }) {
        put("White Balance", wb.into());
    }
    if let Some(sc) = num(ex.get_field(Tag(Context::Exif, 41990), In::PRIMARY)).and_then(|v| match v as u32 {
        0 => Some("Standard"),
        1 => Some("Landscape"),
        2 => Some("Portrait"),
        3 => Some("Night"),
        _ => None,
    }) {
        put("Scene", sc.into());
    }
    if let Some(fl) = num(either(&ex, 37386)) {
        put("Focal Length", format!("{} mm", fmt_g(fl)));
    }
    if let Some(fl35) = num(ex.get_field(Tag(Context::Exif, 41989), In::PRIMARY)).filter(|&v| v != 0.0) {
        put("35mm Equiv", format!("{} mm", fl35 as i64));
    }
    if let Some(sd) = num(ex.get_field(Tag(Context::Exif, 37382), In::PRIMARY)).filter(|&v| v > 0.0) {
        let s = if sd >= 1000.0 {
            format!("{:.1} km", sd / 1000.0)
        } else if sd >= 1.0 {
            format!("{sd:.2} m")
        } else {
            format!("{:.0} cm", sd * 100.0)
        };
        put("Subject Dist", s);
    }

    let lmake = text(ex.get_field(Tag(Context::Exif, 42035), In::PRIMARY)).unwrap_or_default();
    let lmodel = strip_make(&lmake, &text(ex.get_field(Tag(Context::Exif, 42036), In::PRIMARY)).unwrap_or_default());
    if !lmake.is_empty() {
        put("Lens Make", lmake);
    }
    if !lmodel.is_empty() {
        put("Lens Model", lmodel);
    }
    let spec = nums(ex.get_field(Tag(Context::Exif, 42034), In::PRIMARY));
    if spec.len() >= 3 && !spec[0].is_nan() && !spec[1].is_nan() {
        let fl = if spec[0] == spec[1] {
            format!("{} mm", fmt_g(spec[0]))
        } else {
            format!("{}–{} mm", fmt_g(spec[0]), fmt_g(spec[1]))
        };
        let fnp = if !spec[2].is_nan() && spec[2] != 0.0 { format!("  f/{}", fmt_g(spec[2])) } else { String::new() };
        put("Lens Spec", fl + &fnp);
    }

    let lat = nums(gps(&ex, 2));
    let lon = nums(gps(&ex, 4));
    if !lat.is_empty() && !lon.is_empty() {
        let dms = |v: &[f64]| v.iter().enumerate().map(|(i, x)| x / 60f64.powi(i as i32)).sum::<f64>();
        let lat_ref = text(gps(&ex, 1)).unwrap_or_else(|| "N".into());
        let lon_ref = text(gps(&ex, 3)).unwrap_or_else(|| "E".into());
        let lf = dms(&lat) * if lat_ref == "N" { 1.0 } else { -1.0 };
        let lo = dms(&lon) * if lon_ref == "E" { 1.0 } else { -1.0 };
        if !lf.is_nan() && !lo.is_nan() {
            put("GPS", format!("{lf:.5}, {lo:.5}"));
        }
    }
    Some(out)
}

// ── minimal TIFF walker for RAW containers ──────────────────────────────────

fn read_tiff_walker(path: &Path) -> Flat {
    let mut out: Flat = Vec::new();
    let Ok(mut f) = File::open(path) else { return out };
    let mut src = FileSource(&mut f);
    let hdr = src.read_at(0, 8);
    if hdr.len() < 8 {
        return out;
    }
    let end = match &hdr[..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return out,
    };
    let ifd0 = end.u32(&hdr[4..8]);
    let read_str = |src: &mut FileSource, off: u32, cnt: u32| tiff::ascii(&src.read_at(off as u64, cnt as usize)).trim().to_string();
    let read_rat = |src: &mut FileSource, off: u32, signed: bool| -> Option<f64> {
        let b = src.read_at(off as u64, 8);
        if b.len() < 8 {
            return None;
        }
        let (n, d) = if signed {
            (end.u32(&b[0..4]) as i32 as f64, end.u32(&b[4..8]) as i32 as f64)
        } else {
            (end.u32(&b[0..4]) as f64, end.u32(&b[4..8]) as f64)
        };
        (d != 0.0).then(|| n / d)
    };

    let mut exif_ptr = 0;
    for (tag, _t, cnt, val) in tiff::ifd_entries(&mut src, end, ifd0) {
        match tag {
            0x8769 => exif_ptr = val,
            271 if cnt > 4 => out.push(("Make".into(), read_str(&mut src, val, cnt))),
            272 if cnt > 4 => out.push(("Model".into(), read_str(&mut src, val, cnt))),
            305 if cnt > 4 => out.push(("Software".into(), read_str(&mut src, val, cnt))),
            _ => {}
        }
    }
    if exif_ptr == 0 {
        return out;
    }
    let mut raw: std::collections::HashMap<&str, f64> = Default::default();
    for (tag, _t, _cnt, val) in tiff::ifd_entries(&mut src, end, exif_ptr) {
        let short = end.short_value(val) as f64;
        match tag {
            33434 => raw.extend(read_rat(&mut src, val, false).map(|v| ("_et", v))),
            33437 => raw.extend(read_rat(&mut src, val, false).map(|v| ("_fn", v))),
            34855 => {
                raw.insert("ISO", short);
            }
            37380 => raw.extend(read_rat(&mut src, val, true).map(|v| ("_ev", v))),
            37383 => {
                raw.insert("_mm", short);
            }
            37385 => {
                raw.insert("_fl", short);
            }
            37386 => raw.extend(read_rat(&mut src, val, false).map(|v| ("Focal Length", v))),
            41987 => {
                raw.insert("_wb", short);
            }
            41989 => {
                raw.insert("35mm Equiv", short);
            }
            41986 => {
                raw.insert("_em", short);
            }
            41985 => {
                raw.insert("_ep", short);
            }
            41990 => {
                raw.insert("_sc", short);
            }
            _ => {}
        }
    }

    // Normalise Make/Model
    let make = out.iter().find(|(k, _)| k == "Make").map(|(_, v)| v.trim().to_string()).unwrap_or_default();
    if let Some(m) = out.iter_mut().find(|(k, _)| k == "Model") {
        m.1 = strip_make(&make, m.1.trim());
    }
    let mut put = |k: &str, v: String| out.push((k.to_string(), v));
    if let Some(iso) = raw.get("ISO") {
        put("ISO", format!("{}", *iso as i64));
    }
    if let Some(&et) = raw.get("_et").filter(|&&v| v > 0.0) {
        put("Shutter", shutter_text(et));
    }
    if let Some(&fnum) = raw.get("_fn").filter(|&&v| v > 0.0) {
        put("Aperture", format!("f/{}", fmt_g(fnum)));
    }
    if let Some(&fl) = raw.get("Focal Length").filter(|&&v| v > 0.0) {
        put("Focal Length", format!("{} mm", fmt_g(fl)));
    }
    if let Some(&fl35) = raw.get("35mm Equiv").filter(|&&v| v > 0.0) {
        put("35mm Equiv", format!("{} mm", fl35 as i64));
    }
    if let Some(&ev) = raw.get("_ev") {
        let s = fmt_g(ev);
        put("EV Comp", format!("{}{} EV", if ev >= 0.0 && !s.starts_with('-') { "+" } else { "" }, s));
    }
    let label = |v: f64, table: &[(u32, &str)]| -> String {
        table.iter().find(|(k, _)| *k == v as u32).map(|(_, s)| s.to_string()).unwrap_or_else(|| (v as u32).to_string())
    };
    if let Some(&mm) = raw.get("_mm") {
        put(
            "Metering",
            label(mm, &[(0, "Unknown"), (1, "Average"), (2, "Center-weighted"), (3, "Spot"), (4, "Multi-spot"), (5, "Multi-segment"), (6, "Partial")]),
        );
    }
    if let Some(&fl) = raw.get("_fl") {
        let table = [
            (0, "Off"), (1, "Fired"), (5, "Fired (no return)"), (7, "Fired (return)"), (16, "Off (flash)"),
            (24, "Off (auto)"), (25, "Fired (auto)"), (29, "Fired (auto, no return)"),
            (31, "Fired (auto, return)"), (32, "No flash"), (65, "Fired (red-eye)"),
            (71, "Fired (red-eye, no return)"),
        ];
        let v = fl as u32;
        put("Flash", table.iter().find(|(k, _)| *k == v).map(|(_, s)| s.to_string()).unwrap_or(format!("0x{v:02x}")));
    }
    if let Some(&wb) = raw.get("_wb") {
        put("White Balance", label(wb, &[(0, "Auto"), (1, "Manual")]));
    }
    if let Some(&em) = raw.get("_em") {
        put("Exp Mode", label(em, &[(0, "Auto"), (1, "Manual"), (2, "Auto bracket")]));
    }
    if let Some(&ep) = raw.get("_ep") {
        put(
            "Exp Program",
            label(ep, &[(1, "Manual"), (2, "Program"), (3, "Aperture priority"), (4, "Shutter priority"), (5, "Creative"), (6, "Action"), (7, "Portrait"), (8, "Landscape")]),
        );
    }
    if let Some(&sc) = raw.get("_sc") {
        put("Scene", label(sc, &[(0, "Standard"), (1, "Landscape"), (2, "Portrait"), (3, "Night")]));
    }
    out
}

// ── exiftool ────────────────────────────────────────────────────────────────

const EXIFTOOL_TAGS: [&str; 20] = [
    "-Make", "-Model", "-ISO", "-ExposureTime", "-FNumber", "-FocalLength", "-FocalLengthIn35mmFormat",
    "-ExposureBiasValue", "-MeteringMode", "-Flash", "-WhiteBalance", "-ExposureProgram", "-ExposureMode",
    "-SceneCaptureType", "-LensID", "-LensModel", "-LensMake", "-LensSpec", "-Lens", "-LensType",
];

/// Run `exiftool -j -n` (5 s timeout) and format what it found.
pub fn run_exiftool(path: &Path) -> Flat {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut out: Flat = Vec::new();
    let Ok(mut child) = Command::new("exiftool")
        .args(["-j", "-n"])
        .args(EXIFTOOL_TAGS)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return out;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return out;
            }
        }
    }
    let Ok(output) = child.wait_with_output() else { return out };
    if !output.status.success() {
        return out;
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else { return out };
    let Some(d) = json.as_array().and_then(|a| a.first()) else { return out };
    let s = |k: &str| -> String {
        match d.get(k) {
            Some(serde_json::Value::String(s)) => s.trim().to_string(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    };
    let f = |k: &str| -> Option<f64> {
        match d.get(k)? {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    let truthy = |k: &str| f(k).map(|v| v != 0.0).unwrap_or_else(|| !s(k).is_empty());
    let mut put = |k: &str, v: String| out.push((k.to_string(), v));

    let make = s("Make");
    let model = strip_make(&make, &s("Model"));
    if !make.is_empty() {
        put("Make", make);
    }
    if !model.is_empty() {
        put("Model", model);
    }
    if truthy("ISO") {
        if let Some(v) = f("ISO") {
            put("ISO", format!("{}", v as i64));
        }
    }
    if truthy("ExposureTime") {
        if let Some(v) = f("ExposureTime") {
            put("Shutter", shutter_text(v));
        }
    }
    if truthy("FNumber") {
        if let Some(v) = f("FNumber") {
            put("Aperture", format!("f/{}", fmt_g(v)));
        }
    }
    if truthy("FocalLength") {
        if let Some(v) = f("FocalLength") {
            put("Focal Length", format!("{} mm", fmt_g(v)));
        }
    }
    if truthy("FocalLengthIn35mmFormat") {
        if let Some(v) = f("FocalLengthIn35mmFormat") {
            put("35mm Equiv", format!("{} mm", fmt_g(v)));
        }
    }
    if let Some(v) = f("ExposureBiasValue") {
        let g = fmt_g(v);
        put("EV Comp", format!("{}{g} EV", if v >= 0.0 && !g.starts_with('-') { "+" } else { "" }));
    }
    let mut lmodel = s("LensModel");
    if lmodel.is_empty() {
        lmodel = s("LensID");
    }
    if lmodel.is_empty() {
        lmodel = s("Lens");
    }
    let lmake = s("LensMake");
    let lmodel = strip_make(&lmake, &lmodel);
    if !lmake.is_empty() {
        put("Lens Make", lmake);
    }
    if !lmodel.is_empty() {
        put("Lens Model", lmodel.clone());
    }
    let mut spec = s("LensSpec");
    if spec.is_empty() {
        spec = s("LensType");
    }
    if !spec.is_empty() && lmodel.is_empty() {
        put("Lens Model", spec);
    }
    out
}

/// Merge exiftool results into cached sections (existing values win).
pub fn merge(existing: &Sections, extra: &Flat) -> Sections {
    let mut flat = flatten(existing);
    for (k, v) in extra {
        set_default(&mut flat, k, v.clone());
    }
    sections_from_flat(&flat)
}

pub fn has_lens(sections: &Sections) -> bool {
    sections.iter().any(|(name, rows)| {
        name == "LENS" || rows.iter().any(|(l, _)| ["Lens Make", "Lens Model", "Lens Spec"].contains(&l.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_formats() {
        assert_eq!(shutter_text(1.0 / 250.0), "1/250 s");
        assert_eq!(shutter_text(2.0), "2 s");
        assert_eq!(shutter_text(0.3), "1/3 s");
    }
}
