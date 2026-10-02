//! Persisted session state (the Python app's QSettings keys).
//!
//! Stored as JSON in the config directory.  On first run the Python app's
//! `~/.config/PhotoPipeline/PhotoPipeline.conf` is imported, so the last
//! working folder, library, filters and calendar position carry over.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Serialises settings writes (see [`Settings::save_in_background`]).
static SAVE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
/// Bumped by every save; a background save writes only if still the newest.
static SAVE_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub view_index: u32,
    /// Preferences ▸ Show pruned photos: pruned photos stay in All and
    /// Marked.  The key is the one the filter bar's "Show pruned" saved
    /// (and the Python app's), so older settings carry over.
    pub filter_pruned: bool,
    pub filter_marked_only: bool,
    /// Rust only: the "To review" filter.
    pub filter_to_review: bool,
    /// Rust only: the "Pruned" filter.
    pub filter_pruned_only: bool,
    pub sort_key: String,
    pub sort_asc: bool,
    pub filter_date_from: String,
    pub filter_date_to: String,
    pub last_folder: String,
    pub library_folder: String,
    pub folder_bar_visible: bool,
    pub folder_bar_collapsed: bool,
    pub calendar_mode: String,
    pub calendar_year: i32,
    pub calendar_month: u32,
    pub edit_copy_folder: String,
    pub edit_copy_subfolder: bool,
    pub window_width: f32,
    pub window_height: f32,
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    pub window_maximized: bool,
    pub thumb_size: u32,
    /// File names under the grid's photos.
    pub show_file_names: bool,
    /// The grid shows each burst folded into one stack (View ▸ Stack all
    /// bursts); off, every frame shows.
    pub stack_bursts: bool,
    /// Viewer: a P / M key press that sets a mark moves on to the next photo.
    pub viewer_advance: bool,
    /// Viewer: the filmstrip along the bottom of the photo.
    pub viewer_filmstrip: bool,
    /// Viewer: arriving at a burst shows all its frames (the burst sheet)
    /// before any one of them.
    pub viewer_burst_sheet: bool,
    /// The viewer window's last un-maximised size (0 × 0: never resized, so
    /// it opens at a share of the screen).
    pub viewer_width: f32,
    pub viewer_height: f32,
    /// Its position, where the platform reports one (not on Wayland).
    pub viewer_x: Option<f32>,
    pub viewer_y: Option<f32>,
    pub viewer_maximized: bool,
    /// Show the menus in the KDE global menu when the panel has one.  Read
    /// at start; `PP_NO_GLOBAL_MENU` in the environment overrides it.
    pub global_menu: bool,

    // Library search (docs/library-search-plan.md §7a.8).
    /// Preferences ▸ Search ▸ *Run models on*: the graphics card (the
    /// default) or the processor.  Never changed by the app itself.
    pub search_device: crate::ml::Device,
    /// Preferences ▸ Search ▸ *How close matches must be*: a value in
    /// `search::CLOSENESS_MIN..=CLOSENESS_MAX`; higher is stricter; the
    /// reset button sets `search::CLOSENESS_DEFAULT`.
    pub search_closeness: f32,
    /// Preferences ▸ Search ▸ *Index new exports* (on): after the first
    /// index, each export is added to search on its own.
    pub search_index_exports: bool,
    /// The last [`RECENT_SEARCHES`] queries, newest first (§7a.3).
    /// *Forget them* in Preferences clears it.
    pub search_recent: Vec<String>,
}

/// How many recent searches are kept.

#[allow(dead_code)] // used by the search page (the UI branch)
pub const RECENT_SEARCHES: usize = 10;

impl Default for Settings {
    fn default() -> Self {
        Self {
            view_index: 1,
            filter_pruned: false,
            filter_marked_only: false,
            filter_to_review: false,
            filter_pruned_only: false,
            sort_key: "date".into(),
            sort_asc: true,
            filter_date_from: String::new(),
            filter_date_to: String::new(),
            last_folder: String::new(),
            library_folder: String::new(),
            folder_bar_visible: true,
            folder_bar_collapsed: false,
            calendar_mode: String::new(),
            calendar_year: 0,
            calendar_month: 0,
            edit_copy_folder: String::new(),
            edit_copy_subfolder: true,
            window_width: 1200.0,
            window_height: 780.0,
            window_x: None,
            window_y: None,
            window_maximized: false,
            thumb_size: 180,
            show_file_names: true,
            stack_bursts: false,
            viewer_advance: false,
            viewer_filmstrip: true,
            viewer_burst_sheet: true,
            viewer_width: 0.0,
            viewer_height: 0.0,
            viewer_x: None,
            viewer_y: None,
            viewer_maximized: false,
            global_menu: true,
            search_device: crate::ml::Device::Gpu,
            search_closeness: crate::search::CLOSENESS_DEFAULT,
            search_index_exports: true,
            search_recent: Vec::new(),
        }
    }
}


#[allow(dead_code)] // used by the search page (the UI branch)
impl Settings {
    /// Note a query just run: it moves to the front of the recent list
    /// (once, however often it is run), and the list keeps its last
    /// [`RECENT_SEARCHES`].  Blank queries aren't kept.
    pub fn remember_search(&mut self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        self.search_recent.retain(|r| r != q);
        self.search_recent.insert(0, q.to_string());
        self.search_recent.truncate(RECENT_SEARCHES);
    }

    /// Preferences ▸ *Forget them*.
    pub fn forget_searches(&mut self) {
        self.search_recent.clear();
    }

    /// The closeness, kept within the slider's range (an old or edited
    /// settings file can't push it outside).
    pub fn closeness(&self) -> f32 {
        if self.search_closeness.is_finite() {
            self.search_closeness.clamp(crate::search::CLOSENESS_MIN, crate::search::CLOSENESS_MAX)
        } else {
            crate::search::CLOSENESS_DEFAULT
        }
    }
}

/// `PP_NO_GLOBAL_MENU` is set: the menus stay in the window whatever the
/// setting says.
pub fn global_menu_forced_off() -> bool {
    std::env::var_os("PP_NO_GLOBAL_MENU").is_some()
}

/// Where the settings are kept.
pub fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("photo-pipeline").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        if let Some(p) = path() {
            if let Ok(text) = fs::read_to_string(&p) {
                if let Ok(s) = serde_json::from_str(&text) {
                    return s;
                }
            }
        }
        Self::import_qsettings().unwrap_or_default()
    }

    /// Whether to offer the menus to the KDE global menu, given whether the
    /// environment forces it off.
    pub fn global_menu_wanted(&self, forced_off: bool) -> bool {
        self.global_menu && !forced_off
    }

    pub fn save(&self) -> std::io::Result<()> {
        // Supersedes any background save still waiting, and waits for one
        // being written, so the two never interleave.
        SAVE_GEN.fetch_add(1, Ordering::SeqCst);
        let _writing = SAVE_LOCK.lock();
        self.write()
    }

    /// Save on a background thread, so the UI thread never waits on the
    /// disk.  Saves land in order; one overtaken by a newer save is skipped.
    pub fn save_in_background(&self) {
        let generation = SAVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let s = self.clone();
        let spawned = std::thread::Builder::new().name("settings-save".into()).spawn(move || {
            let _writing = SAVE_LOCK.lock();
            if SAVE_GEN.load(Ordering::SeqCst) == generation {
                if let Err(e) = s.write() {
                    eprintln!("settings: could not save: {e}");
                }
            }
        });
        if spawned.is_err() {
            let _ = self.save();
        }
    }

    fn write(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        crate::model::sidecar::atomic_write_text(&p, &text)
    }

    /// Read the Python app's QSettings INI, if there is one.
    fn import_qsettings() -> Option<Settings> {
        let p = dirs::config_dir()?.join("PhotoPipeline").join("PhotoPipeline.conf");
        let text = fs::read_to_string(p).ok()?;
        Some(Self::from_qsettings(&text))
    }

    /// The settings a QSettings INI holds, over the defaults.  Its
    /// `view_index` is kept as saved: the app opens the grid for the
    /// Python app's list (`app::View::from_index`).
    pub(crate) fn from_qsettings(text: &str) -> Settings {
        let mut s = Settings::default();
        let mut section = String::from("General");
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_string();
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let key = if section == "General" { k.trim().to_string() } else { format!("{section}/{}", k.trim()) };
            let v = unquote(v.trim());
            let b = v == "true";
            match key.as_str() {
                "view_index" => s.view_index = v.parse().unwrap_or(1),
                "filter/pruned" => s.filter_pruned = b,
                "filter/marked_only" => s.filter_marked_only = b,
                "sort/key" => s.sort_key = v,
                "sort/asc" => s.sort_asc = b,
                "filter/date_from" => s.filter_date_from = v,
                "filter/date_to" => s.filter_date_to = v,
                "last_folder" => s.last_folder = v,
                "library_folder" => s.library_folder = v,
                "folder_bar_visible" => s.folder_bar_visible = b,
                "folder_bar_collapsed" => s.folder_bar_collapsed = b,
                "calendar/mode" => s.calendar_mode = v,
                "calendar/year" => s.calendar_year = v.parse().unwrap_or(0),
                "calendar/month" => s.calendar_month = v.parse().unwrap_or(0),
                "edit_copy/folder" => s.edit_copy_folder = v,
                "edit_copy/subfolder" => s.edit_copy_subfolder = b,
                _ => {}
            }
        }
        s
    }
}

/// QSettings quotes values containing special characters and escapes
/// backslashes; undo both.
fn unquote(v: &str) -> String {
    let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_preference() {
        let s = Settings::default();
        assert!(!s.viewer_advance);
        assert!(!s.stack_bursts, "bursts open by default");
        assert!(s.show_file_names && s.viewer_filmstrip && s.edit_copy_subfolder && s.global_menu);
        assert_eq!(s.thumb_size, 180);
        assert!(s.library_folder.is_empty() && s.edit_copy_folder.is_empty());
        // An older settings.json, written before the global-menu setting.
        let old: Settings = serde_json::from_str(r#"{"show_file_names": false, "viewer_advance": true}"#).expect("parses");
        assert!(old.global_menu);
        assert!(!old.show_file_names && old.viewer_advance);
    }

    #[test]
    fn preferences_round_trip_through_json() {
        let s = Settings {
            viewer_advance: true,
            show_file_names: false,
            thumb_size: 260,
            viewer_filmstrip: false,
            viewer_width: 0.0,
            viewer_height: 0.0,
            library_folder: "/mnt/library".into(),
            edit_copy_folder: "/home/me/RAW edits".into(),
            edit_copy_subfolder: false,
            global_menu: false,
            stack_bursts: true,
            ..Settings::default()
        };
        let text = serde_json::to_string_pretty(&s).expect("serialises");
        let back: Settings = serde_json::from_str(&text).expect("parses");
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&s).unwrap());
        assert!(text.contains("\"global_menu\": false"));
    }

    #[test]
    fn show_pruned_keeps_its_key() {
        // A settings file from before the Pruned filter, "Show pruned" on.
        let old: Settings = serde_json::from_str(r#"{"filter_pruned": true, "filter_marked_only": false}"#).expect("parses");
        assert!(old.filter_pruned && !old.filter_pruned_only);
        let text = serde_json::to_string(&Settings { filter_pruned: true, filter_pruned_only: true, ..Settings::default() }).unwrap();
        assert!(text.contains(r#""filter_pruned":true"#) && text.contains(r#""filter_pruned_only":true"#));
        // The Python app's "Show pruned" becomes the preference.
        let py = Settings::from_qsettings("[filter]\npruned=true\nmarked_only=false\n");
        assert!(py.filter_pruned && !py.filter_marked_only && !py.filter_pruned_only);
        assert!(!Settings::from_qsettings("[filter]\npruned=false\n").filter_pruned);
    }

    #[test]
    fn search_settings_default_to_the_card_and_keep_ten_recent_searches() {
        let mut s = Settings::default();
        assert_eq!(s.search_device, crate::ml::Device::Gpu);
        assert!(s.search_index_exports);
        assert_eq!(s.closeness(), crate::search::CLOSENESS_DEFAULT);
        for i in 0..12 {
            s.remember_search(&format!("query {i}"));
        }
        s.remember_search("  ");
        s.remember_search("query 5");
        assert_eq!(s.search_recent.len(), RECENT_SEARCHES);
        assert_eq!(s.search_recent[0], "query 5", "a repeat moves to the front");
        assert_eq!(s.search_recent.iter().filter(|q| *q == "query 5").count(), 1);
        assert!(!s.search_recent.iter().any(|q| q == "query 0" || q == "query 1"));
        s.forget_searches();
        assert!(s.search_recent.is_empty());
        // A settings file from before search: its own values stay, the
        // search fields take their defaults.
        let old: Settings = serde_json::from_str(r#"{"thumb_size": 200, "library_folder": "/mnt/nas/lib", "viewer_advance": true, "stack_bursts": true}"#).expect("parses");
        assert_eq!((old.thumb_size, old.library_folder.as_str(), old.viewer_advance, old.stack_bursts), (200, "/mnt/nas/lib", true, true));
        assert_eq!(old.search_device, crate::ml::Device::Gpu);
        assert!(old.search_index_exports && old.search_recent.is_empty());
        assert_eq!(old.closeness(), crate::search::CLOSENESS_DEFAULT);
        // The Python app's QSettings import has none of them either.
        let py = Settings::from_qsettings("[General]\nlibrary_folder=/mnt/nas/lib\nview_index=2\n");
        assert_eq!((py.library_folder.as_str(), py.view_index), ("/mnt/nas/lib", 2));
        assert_eq!(py.search_device, crate::ml::Device::Gpu);
        assert!(py.search_index_exports && py.search_recent.is_empty());
        let odd = Settings { search_closeness: 99.0, ..Settings::default() };
        assert_eq!(odd.closeness(), crate::search::CLOSENESS_MAX);
        let text = serde_json::to_string(&Settings { search_device: crate::ml::Device::Cpu, ..Settings::default() }).unwrap();
        assert!(text.contains(r#""search_device":"cpu""#));
    }

    #[test]
    fn the_environment_forces_the_global_menu_off() {
        let on = Settings::default();
        let off = Settings { global_menu: false, ..Settings::default() };
        assert!(on.global_menu_wanted(false));
        assert!(!on.global_menu_wanted(true), "PP_NO_GLOBAL_MENU wins over the setting");
        assert!(!off.global_menu_wanted(false));
        assert!(!off.global_menu_wanted(true));
    }
}
