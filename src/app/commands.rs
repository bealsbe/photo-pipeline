//! One entry per command, shared by the menu bar, the side menu and the
//! keyboard — the Python app's "each shortcut is bound exactly once" rule.

use iced::keyboard::{Key, Modifiers, key::Named};

use crate::ui::icons::Glyph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmd {
    Open,
    Export,
    EditCopy,
    Review,
    Sort,
    GridView,
    CalendarView,
    /// Ctrl+F: the search page, its field ready to type into.
    Search,
    /// Ctrl+4: people and fursuits, on the tab last used.
    Subjects,
    SelectMode,
    QuickLook,
    PruneSelected,
    MarkSelected,
    Undo,
    Redo,
    Quit,
    Shortcuts,
    SideMenu,
    FolderBar,
    ToReview,
    FileNames,
    Compare,
    ReviewBurst,
    /// G: fold the focused photo's burst into a stack, or open it.
    StackBurst,
    /// Shift+G: every burst stacked, or every one open (saved).
    StackBursts,
    AdvanceAfterMark,
    Preferences,
    /// Analyse the working folder (only ever when asked: decision 2).
    Analyse,
    /// Keep the selected frame, prune the rest of its burst still to
    /// review (asks first).
    KeepThisPruneRest,
    /// Prune every photo shown that looks out of focus or accidental
    /// (asks first).
    PruneSoft,
    /// Prune every photo shown that looks accidental (asks first).
    PruneAccidental,
    /// Prune every copy the duplicate rule suggests (asks first).
    PruneCopies,
    /// The hints filter on "Possible problems", or off.
    ProblemsOnly,
    /// Shift+← in the viewer: back to this burst's first frame, else the
    /// burst (or photo) before.  Viewer only.
    PrevBurst,
    /// Shift+→ in the viewer: on to the next burst's first frame, else
    /// the next photo.  Viewer only.
    NextBurst,
    /// L in the viewer: show the faces and fursuit heads found in the
    /// photo, to name, change, add or remove.  Viewer only.
    LabelFaces,
}

impl Cmd {
    /// Menu label, in sentence case; an ellipsis means a dialog follows.
    pub fn label(self) -> &'static str {
        match self {
            Cmd::Open => "Open working folder…",
            Cmd::Export => "Export to library…",
            Cmd::EditCopy => "Copy marked photos for editing…",
            Cmd::Review => "Review pruned files…",
            Cmd::Sort => "Sort into RAW and JPG folders…",
            Cmd::GridView => "Grid",
            Cmd::CalendarView => "Library calendar",
            Cmd::Search => "Search the library",
            Cmd::Subjects => "People and fursuits",
            Cmd::SelectMode => "Select mode",
            Cmd::QuickLook => "Quick look",
            Cmd::PruneSelected => "Prune selected",
            Cmd::MarkSelected => "Mark selected for export",
            Cmd::Undo => "Undo",
            Cmd::Redo => "Redo",
            Cmd::Quit => "Quit",
            Cmd::Shortcuts => "Keyboard shortcuts",
            Cmd::SideMenu => "Show menu labels",
            Cmd::FolderBar => "Show folder bar",
            Cmd::ToReview => "Show only photos to review",
            Cmd::FileNames => "Show file names",
            Cmd::Compare => "Compare selected",
            Cmd::ReviewBurst => "Review burst",
            Cmd::StackBurst => "Stack or expand burst",
            Cmd::StackBursts => "Stack all bursts",
            Cmd::AdvanceAfterMark => "Advance after marking",
            Cmd::Preferences => "Preferences…",
            Cmd::Analyse => "Analyse this folder",
            Cmd::KeepThisPruneRest => "Keep this, prune the rest…",
            Cmd::PruneSoft => "Prune all soft frames…",
            Cmd::PruneAccidental => "Prune accidental frames…",
            Cmd::PruneCopies => "Prune duplicate copies…",
            Cmd::ProblemsOnly => "Show only possible problems",
            Cmd::PrevBurst => "Previous burst",
            Cmd::NextBurst => "Next burst",
            Cmd::LabelFaces => "Label faces and fursuits",
        }
    }

    /// The side menu's label: `iconText()` — no trailing ellipsis.
    pub fn short_label(self) -> &'static str {
        let l = self.label();
        l.strip_suffix('…').unwrap_or(l)
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Cmd::Open => "Ctrl+O",
            Cmd::Export => "Ctrl+E",
            Cmd::EditCopy => "Ctrl+Shift+E",
            Cmd::Review => "Ctrl+R",
            Cmd::Sort => "Ctrl+Shift+S",
            Cmd::GridView => "Ctrl+2",
            Cmd::CalendarView => "Ctrl+3",
            Cmd::Search => "Ctrl+F",
            Cmd::Subjects => "Ctrl+4",
            Cmd::SelectMode => "S",
            Cmd::QuickLook => "Space",
            Cmd::PruneSelected | Cmd::MarkSelected => "",
            Cmd::Undo => "Ctrl+Z",
            Cmd::Redo => "Ctrl+Shift+Z",
            Cmd::Quit => "Ctrl+Q",
            Cmd::Shortcuts => "?",
            Cmd::SideMenu => "Ctrl+\\",
            Cmd::FolderBar => "Ctrl+B",
            Cmd::ToReview => "Ctrl+U",
            Cmd::FileNames => "",
            Cmd::Compare => "C",
            Cmd::ReviewBurst => "B",
            Cmd::StackBurst => "G",
            Cmd::StackBursts => "Shift+G",
            Cmd::AdvanceAfterMark => "Ctrl+Shift+A",
            Cmd::Preferences => "Ctrl+,",
            Cmd::Analyse | Cmd::KeepThisPruneRest | Cmd::PruneSoft | Cmd::PruneAccidental | Cmd::PruneCopies | Cmd::ProblemsOnly => "",
            Cmd::PrevBurst => "Shift+←",
            Cmd::NextBurst => "Shift+→",
            Cmd::LabelFaces => "L",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Cmd::Open => "Open a working folder",
            Cmd::Export => "Copy or move photos into the library, filed by date",
            Cmd::EditCopy => "Copy the RAWs, the JPGs or both of every marked shot into one folder to edit from",
            Cmd::Review => "Review pruned files, then move them to the Trash",
            Cmd::Sort => "Move files into RAW and JPG subfolders",
            Cmd::GridView => "Show files as a grid",
            Cmd::CalendarView => "Browse the library by date",
            Cmd::Search => "Find photos in the library by what they show",
            Cmd::Subjects => "Name the people found in your photos, then find them",
            Cmd::SelectMode => "Select several photos with single clicks",
            Cmd::QuickLook => "See the selected photo large, over the grid",
            Cmd::PruneSelected => "Mark the selected photos to prune",
            Cmd::MarkSelected => "Mark the selected photos for export",
            Cmd::Undo => "Undo the last change to marks, crops, Sort or the Trash",
            Cmd::Redo => "Redo what was undone",
            Cmd::ToReview => "Show only photos not yet pruned or marked",
            Cmd::FileNames => "Show each photo's file name under it",
            Cmd::Compare => "Compare 2 to 4 selected photos side by side",
            Cmd::ReviewBurst => "See every frame of the selected photo's burst in the viewer, to look closer at or compare",
            Cmd::StackBurst => "Fold the selected photo's burst into one stack in the grid, or show all its frames",
            Cmd::StackBursts => "Show every burst in the grid as one stack; off, every frame shows",
            Cmd::AdvanceAfterMark => "In the viewer, move on to the next photo when P or M marks one",
            Cmd::Preferences => "Settings for culling, the grid, the viewer, folders and storage",
            Cmd::Analyse => "Look for photos that are out of focus or accidental, for copies, and for the sharpest frame of each burst. Nothing is marked",
            Cmd::KeepThisPruneRest => "Keep the selected frame and prune the other frames of its burst you haven't decided on (asks first)",
            Cmd::PruneSoft => "Prune every photo shown that looks out of focus or accidental (asks first)",
            Cmd::PruneAccidental => "Prune every photo shown that looks accidental (asks first)",
            Cmd::PruneCopies => "Prune the copies of files that are in the folder twice (asks first)",
            Cmd::ProblemsOnly => "Show only photos that look out of focus or accidental, and copies",
            Cmd::PrevBurst => "In the viewer, back to the first frame of this burst, or of the burst before",
            Cmd::NextBurst => "In the viewer, on to the first frame of the next burst",
            Cmd::LabelFaces => "In the viewer, show the faces and fursuit heads found in the photo: name them, add one that was missed, or remove one",
            _ => "",
        }
    }

    pub fn icon(self) -> Option<Glyph> {
        Some(match self {
            Cmd::Open => Glyph::FolderOpen,
            Cmd::Export => Glyph::Export,
            Cmd::EditCopy => Glyph::EditCopy,
            Cmd::Review => Glyph::Trash,
            Cmd::Sort => Glyph::Sort,
            Cmd::GridView => Glyph::Grid,
            Cmd::CalendarView => Glyph::Calendar,
            Cmd::Search => Glyph::Search,
            Cmd::Subjects => Glyph::Subjects,
            Cmd::SelectMode => Glyph::Select,
            Cmd::QuickLook => Glyph::ZoomIn,
            Cmd::PruneSelected => Glyph::Prune,
            Cmd::MarkSelected => Glyph::Keep,
            Cmd::Undo => Glyph::Undo,
            Cmd::Redo => Glyph::Redo,
            Cmd::Shortcuts => Glyph::Keyboard,
            Cmd::Compare => Glyph::Compare,
            Cmd::AdvanceAfterMark => Glyph::Skip,
            Cmd::Preferences => Glyph::Settings,
            Cmd::Analyse => Glyph::Analyse,
            Cmd::LabelFaces => Glyph::Label,
            _ => return None,
        })
    }

    /// Commands that act in the viewer window only: the main window lets
    /// their keys through to its own use of them (Shift+arrows extend the
    /// grid's selection).
    pub fn viewer_only(self) -> bool {
        matches!(self, Cmd::PrevBurst | Cmd::NextBurst | Cmd::LabelFaces)
    }

    /// Match a key press against the command shortcuts.
    pub fn from_key(key: &Key, mods: Modifiers) -> Option<Cmd> {
        let ctrl = mods.control();
        let shift = mods.shift();
        let alt = mods.alt();
        if alt {
            return None;
        }
        match key {
            Key::Character(c) => {
                let c = c.as_str();
                let lower = c.to_lowercase();
                match (ctrl, shift, lower.as_str()) {
                    (true, false, "o") => Some(Cmd::Open),
                    (true, false, "e") => Some(Cmd::Export),
                    (true, true, "e") => Some(Cmd::EditCopy),
                    (true, false, "r") => Some(Cmd::Review),
                    (true, true, "s") => Some(Cmd::Sort),
                    (true, _, "2") | (true, _, "@") => Some(Cmd::GridView),
                    (true, _, "3") | (true, _, "#") => Some(Cmd::CalendarView),
                    (true, _, "4") | (true, _, "$") => Some(Cmd::Subjects),
                    (true, false, "q") => Some(Cmd::Quit),
                    (true, _, "\\") | (true, _, "|") => Some(Cmd::SideMenu),
                    (true, false, "b") => Some(Cmd::FolderBar),
                    (true, false, "z") => Some(Cmd::Undo),
                    (true, true, "z") | (true, false, "y") => Some(Cmd::Redo),
                    (true, false, "u") => Some(Cmd::ToReview),
                    (true, true, "a") => Some(Cmd::AdvanceAfterMark),
                    (true, false, ",") => Some(Cmd::Preferences),
                    (true, false, "f") => Some(Cmd::Search),
                    (false, false, "s") => Some(Cmd::SelectMode),
                    (false, false, "c") => Some(Cmd::Compare),
                    (false, false, "b") => Some(Cmd::ReviewBurst),
                    (false, false, "g") => Some(Cmd::StackBurst),
                    (false, true, "g") => Some(Cmd::StackBursts),
                    (false, true, "a") => Some(Cmd::PrevBurst),
                    (false, true, "d") => Some(Cmd::NextBurst),
                    (false, false, "l") => Some(Cmd::LabelFaces),
                    (false, _, "?") => Some(Cmd::Shortcuts),
                    _ => None,
                }
            }
            Key::Named(Named::F1) if !ctrl => Some(Cmd::Shortcuts),
            Key::Named(Named::Space) if !ctrl => Some(Cmd::QuickLook),
            Key::Named(Named::ArrowLeft) if shift && !ctrl => Some(Cmd::PrevBurst),
            Key::Named(Named::ArrowRight) if shift && !ctrl => Some(Cmd::NextBurst),
            _ => None,
        }
    }
}

/// Menu bar layout: (title, mnemonic, entries).  `None` is a separator.
pub const MENUS: &[(&str, char, &[Option<Cmd>])] = &[
    ("File", 'f', &[Some(Cmd::Open), None, Some(Cmd::Quit)]),
    ("Edit", 'e', &[Some(Cmd::Undo), Some(Cmd::Redo), None, Some(Cmd::Preferences)]),
    (
        "Prune",
        'p',
        &[
            Some(Cmd::Analyse),
            None,
            Some(Cmd::KeepThisPruneRest),
            Some(Cmd::PruneSoft),
            Some(Cmd::PruneAccidental),
            Some(Cmd::PruneCopies),
            None,
            Some(Cmd::Review),
            Some(Cmd::Sort),
            None,
            Some(Cmd::Export),
            Some(Cmd::EditCopy),
            None,
            Some(Cmd::AdvanceAfterMark),
        ],
    ),
    ("Help", 'h', &[Some(Cmd::Shortcuts)]),
    (
        "View",
        'v',
        &[
            Some(Cmd::GridView),
            Some(Cmd::CalendarView),
            Some(Cmd::Search),
            Some(Cmd::Subjects),
            None,
            Some(Cmd::ToReview),
            Some(Cmd::ProblemsOnly),
            Some(Cmd::QuickLook),
            Some(Cmd::Compare),
            Some(Cmd::ReviewBurst),
            Some(Cmd::StackBurst),
            Some(Cmd::StackBursts),
            None,
            Some(Cmd::SideMenu),
            Some(Cmd::FolderBar),
            Some(Cmd::FileNames),
        ],
    ),
];

/// Side menu layout.
pub const SIDE: &[Option<Cmd>] = &[
    Some(Cmd::Open),
    Some(Cmd::Export),
    Some(Cmd::EditCopy),
    None,
    Some(Cmd::Review),
    Some(Cmd::Analyse),
    None,
    Some(Cmd::GridView),
    Some(Cmd::CalendarView),
    Some(Cmd::Search),
    Some(Cmd::Subjects),
    None,
    Some(Cmd::SelectMode),
    Some(Cmd::PruneSelected),
    Some(Cmd::MarkSelected),
    Some(Cmd::Compare),
];

/// Pinned to the bottom of the side menu.
pub const SIDE_BOTTOM: &[Option<Cmd>] = &[Some(Cmd::Preferences)];

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: Named) -> Key {
        Key::Named(name)
    }

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn shift_arrows_and_shift_a_d_skip_bursts() {
        let shift = Modifiers::SHIFT;
        assert_eq!(Cmd::from_key(&key(Named::ArrowLeft), shift), Some(Cmd::PrevBurst));
        assert_eq!(Cmd::from_key(&key(Named::ArrowRight), shift), Some(Cmd::NextBurst));
        // The layout sends the capital with Shift held.
        assert_eq!(Cmd::from_key(&ch("A"), shift), Some(Cmd::PrevBurst));
        assert_eq!(Cmd::from_key(&ch("D"), shift), Some(Cmd::NextBurst));
        assert!(Cmd::PrevBurst.viewer_only() && Cmd::NextBurst.viewer_only());
        assert!(!Cmd::StackBursts.viewer_only());
    }

    #[test]
    fn the_burst_keys_take_nothing_else() {
        let none = Modifiers::empty();
        // Plain arrows and A / D stay the viewer's own steps.
        for k in [key(Named::ArrowLeft), key(Named::ArrowRight), ch("a"), ch("d")] {
            assert_eq!(Cmd::from_key(&k, none), None);
        }
        // Ctrl+Shift+A is still advance after marking; Ctrl or Alt with an
        // arrow is nothing.
        assert_eq!(Cmd::from_key(&ch("A"), Modifiers::CTRL | Modifiers::SHIFT), Some(Cmd::AdvanceAfterMark));
        assert_eq!(Cmd::from_key(&key(Named::ArrowRight), Modifiers::CTRL | Modifiers::SHIFT), None);
        assert_eq!(Cmd::from_key(&key(Named::ArrowRight), Modifiers::ALT | Modifiers::SHIFT), None);
        assert_eq!(Cmd::from_key(&ch("G"), Modifiers::SHIFT), Some(Cmd::StackBursts));
    }

    /// L labels faces and fursuits in the viewer, and nothing else has it:
    /// not with Shift or Ctrl, not in the main window.
    #[test]
    fn l_labels_faces_in_the_viewer_only() {
        assert_eq!(Cmd::from_key(&ch("l"), Modifiers::empty()), Some(Cmd::LabelFaces));
        assert_eq!(Cmd::from_key(&ch("L"), Modifiers::SHIFT), None);
        assert_eq!(Cmd::from_key(&ch("l"), Modifiers::CTRL), None);
        assert!(Cmd::LabelFaces.viewer_only());
        assert_eq!((Cmd::LabelFaces.label(), Cmd::LabelFaces.shortcut()), ("Label faces and fursuits", "L"));
        // Bound once: no other command answers to L.
        let all = [Cmd::Open, Cmd::SelectMode, Cmd::Compare, Cmd::ReviewBurst, Cmd::StackBurst, Cmd::Shortcuts, Cmd::QuickLook];
        assert!(all.iter().all(|c| c.shortcut() != "L"));
        assert!(MENUS.iter().all(|(_, _, items)| !items.contains(&Some(Cmd::LabelFaces))), "the viewer has no menus");
    }

    #[test]
    fn the_burst_commands_are_named_in_sentence_case() {
        assert_eq!(Cmd::PrevBurst.label(), "Previous burst");
        assert_eq!(Cmd::NextBurst.label(), "Next burst");
        assert_eq!(Cmd::PrevBurst.shortcut(), "Shift+←");
        assert_eq!(Cmd::NextBurst.shortcut(), "Shift+→");
        // Not in the main window's menus: the viewer has none.
        assert!(MENUS.iter().all(|(_, _, items)| !items.contains(&Some(Cmd::PrevBurst)) && !items.contains(&Some(Cmd::NextBurst))));
    }
}
