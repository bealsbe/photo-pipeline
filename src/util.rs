//! Small formatting helpers that reproduce Python's output exactly where the
//! text is user-visible or has to match something the Python app wrote
//! (the disk-cache key, sidecar JSON).

use std::path::{Component, Path, PathBuf};

/// Python's `format(x, "g")`: six significant digits, trailing zeros
/// stripped, scientific notation below 1e-4 or from 1e6.
pub fn fmt_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.5e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if x < 0.0 { "-" } else { "" };
    if !(-4..6).contains(&exp) {
        let m = strip_zeros(mant);
        let es = if exp < 0 { '-' } else { '+' };
        format!("{sign}{m}e{es}{:02}", exp.abs())
    } else {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{:.*}", decimals, x.abs());
        format!("{sign}{}", strip_zeros(&s))
    }
}

fn strip_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// Python's `repr(float)`: shortest round-trip digits, always with a `.`
/// (for the magnitudes that occur here — timestamps).
pub fn py_float_repr(x: f64) -> String {
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// Python's `round(x, 6)` — round-half-even on the exact binary value.
pub fn round6(x: f64) -> f64 {
    format!("{x:.6}").parse().unwrap_or(x)
}

/// `f"{n:,}"`
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `"1,234 photos"` / `"1 photo"`
pub fn plural(n: u64, word: &str) -> String {
    format!("{} {word}{}", thousands(n), if n == 1 { "" } else { "s" })
}

/// `"s"` unless n == 1.
pub fn s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

const THIN: char = '\u{202f}';

/// File size with one decimal: `"7.9 MB"` (narrow no-break space).
pub fn fmt_size(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 {
            return format!("{v:.1}{THIN}{unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.1}{THIN}TB")
}

/// File size rounded to a whole number — the grid tooltip's format.
pub fn fmt_size_whole(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 {
            return format!("{v:.0}{THIN}{unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.0}{THIN}TB")
}

/// The edit-copy dialog's byte format: `"812 KB"`, `"1.4 GB"`.
pub fn fmt_bytes(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 || unit == "GB" {
            return if unit == "B" || unit == "KB" {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
        v /= 1024.0;
    }
    format!("{v:.1} TB")
}

/// Shorten a path from the left: `"…/Photos/Shoot"`.
pub fn elide_path(path: &Path, max_len: usize) -> String {
    let s = path.to_string_lossy();
    let n = s.chars().count();
    if n <= max_len {
        return s.into_owned();
    }
    let keep: String = s.chars().skip(n - (max_len - 1)).collect();
    format!("…{keep}")
}

/// Normalise `.` components and repeated separators the way pathlib does
/// (`..` is kept — pathlib does not resolve it either).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `str(path.relative_to(root))`, or the absolute path when it is outside.
pub fn relative_or_absolute(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// `root / rel` for a stored sidecar path (absolute entries are kept).
pub fn resolve_stored(root: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&root.join(p))
    }
}

/// Serialise like Python's `json.dumps(value, indent=2)` — same layout and
/// `ensure_ascii` escaping, so sidecars stay byte-compatible.
pub fn py_json(value: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
    let mut out = String::with_capacity(pretty.len());
    for c in pretty.chars() {
        if (c as u32) < 0x80 {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{:04x}", unit));
            }
        }
    }
    out
}

/// `Path.stem` — the final component without its last suffix.
pub fn stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    py_stem(&name).to_string()
}

/// `Path.suffix`, e.g. `".CR3"` (empty when there is none).
pub fn suffix(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = py_stem(&name);
    name[stem.len()..].to_string()
}

/// pathlib's rule: the suffix starts at the last dot, unless that dot is the
/// first character or the name ends in a dot.
fn py_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[..i],
        _ => name,
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Name of the parent folder, e.g. `"RAW"`.
pub fn parent_name(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Strip one `RAW/` or `JPG/` level so a sorted pair still shares a parent.
pub fn canonical_parent(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    let name = parent_name(path).to_uppercase();
    if name == "RAW" || name == "JPG" {
        parent.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

/// Real camera files for the ignored tests that read them: the photos of
/// one type in `PP_FIXTURE_DIR`, sorted by name.  Panics, saying what to
/// do, when the variable is unset or the folder can't be read, so a run
/// that asks for those tests never passes without checking anything.  The
/// files are only read.
#[cfg(test)]
pub fn fixture_files(file_type: crate::model::record::FileType) -> Vec<PathBuf> {
    let dir = std::env::var_os("PP_FIXTURE_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("PP_FIXTURE_DIR is not set: point it at a folder of camera files (see README, Testing)"));
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("PP_FIXTURE_DIR={}: can't read the folder: {e} (see README, Testing)", dir.display()));
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| crate::model::record::FileType::from_path(p) == Some(file_type))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_format_matches_python() {
        assert_eq!(fmt_g(3.0), "3");
        assert_eq!(fmt_g(16.0), "16");
        assert_eq!(fmt_g(2.8), "2.8");
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(1.0 / 3.0), "0.333333");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(0.00001), "1e-05");
        assert_eq!(fmt_g(-0.7), "-0.7");
        assert_eq!(fmt_g(100000.0), "100000");
        assert_eq!(fmt_g(35.0), "35");
    }

    #[test]
    fn float_repr() {
        assert_eq!(py_float_repr(1721811234.0), "1721811234.0");
        assert_eq!(py_float_repr(1721811234.567891), "1721811234.567891");
    }

    #[test]
    fn stems_follow_pathlib() {
        assert_eq!(stem(Path::new("/a/IMG_1.CR3")), "IMG_1");
        assert_eq!(suffix(Path::new("/a/IMG_1.CR3")), ".CR3");
        assert_eq!(stem(Path::new("/a/.hidden")), ".hidden");
        assert_eq!(suffix(Path::new("/a/a.tar.gz")), ".gz");
    }

    #[test]
    fn canonical_parent_strips_type_folder() {
        assert_eq!(canonical_parent(Path::new("/s/RAW/a.arw")), Path::new("/s"));
        assert_eq!(canonical_parent(Path::new("/s/jpg/a.jpg")), Path::new("/s"));
        assert_eq!(canonical_parent(Path::new("/s/x/a.jpg")), Path::new("/s/x"));
    }

    #[test]
    fn json_escapes_non_ascii() {
        let v = serde_json::json!({"pruned": ["café.jpg"]});
        assert_eq!(py_json(&v), "{\n  \"pruned\": [\n    \"caf\\u00e9.jpg\"\n  ]\n}");
    }

    #[test]
    fn thousands_sep() {
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }
}
