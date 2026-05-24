# Photo Pipeline — Export & Library Organisation Spec

## Overview

Two distinct workflows are being added to the existing prune/pair/sort toolset:

| Workflow | Starting point | Goal |
|----------|---------------|------|
| **1. Auto-sort** | A messy existing directory of photos | Reorganise into the standard `Year/Mon-Year/RAW/JPG` library structure |
| **2. Export** | An active working folder being culled | Send the keepers (and optionally pruned files) into the existing library structure |

Neither workflow deletes anything automatically. File operations are always move or copy — the user chooses.

---

## Standard Library Folder Structure

```
Library Root/
  2024/
    Nov-2024/
      RAW/
        IMG_0001.cr3
      JPG/
        IMG_0001.jpg
    Dec-2024/
      RAW/
      JPG/
  2025/
    Jan-2025/
      RAW/
      JPG/
```

- Year folder: `YYYY` (e.g. `2025`)
- Month folder: `Mon-YYYY` (e.g. `Jan-2025`, `Feb-2025`)
- Within each month: `RAW/` and `JPG/` subfolders
- Date is derived from the file's EXIF shot date, falling back to the file's last-modified date

---

## UI Changes

### Import button removed

The existing **Import** button (`Ctrl+I`) is removed. Its two responsibilities are split:

- **Setting the working folder** → handled by the renamed **Open** button (now "Open Working Folder", `Ctrl+O`). Open simply scans and loads — no file moves.
- **Auto-separating RAW/JPG into subfolders** → the user does this manually with the existing **Sort** button (`Ctrl+Shift+S`) if they want it.

This simplifies the mental model: there is one way to set the working folder. What you do with the files from there is up to you.

### Folder Bar (new, below toolbar)

A persistent two-path bar showing both active folders:

```
[📂 Working: ~/Photos/Shoot-Jan-2025]       [📁 Library: ~/Photos/Library]
```

- Clicking either path opens a folder picker
- If no library folder is set, the library slot shows `Set library folder…` in muted text
- Both paths persist across sessions via `QSettings`
- The library folder is global (not per-working-folder)
- Show/hide via a toolbar toggle button or `Ctrl+B`; visibility persists in `QSettings`; defaults to visible

### Toolbar changes summary

| Change | Detail |
|--------|--------|
| Import button | **Removed** |
| Open button | Kept, renamed "Open Working Folder" conceptually; behaviour unchanged (scan only) |
| Sort button | Kept; still sorts flat RAW/JPG within working folder — now the explicit step for what Import used to do automatically |
| Auto-sort *(new)* | `Ctrl+Shift+A` — reorganise any folder into the library structure |
| Export *(new)* | `Ctrl+E` — export working folder to the library |
| Folder bar toggle *(new)* | `Ctrl+B` — show/hide the folder bar |

---

## Workflow 1: Auto-sort

Entry point: toolbar **Auto-sort** button or menu item.

### Step 1 — Source + Destination

A dialog asks:
- **Source folder**: defaults to the current working folder; user can change via folder picker
- **Destination (library) folder**: defaults to the configured library folder; user can change
- **Operation**: Move or Copy radio buttons
  - Move — files leave the source
  - Copy — files remain in the source; copies land in the library

### Step 2 — Preview Dialog

Shows a tree-style preview of what will be created:

```
Library: ~/Photos/Library

  2025/Jan-2025/
    RAW/   3 files          IMG_0001.cr3  IMG_0002.cr3  IMG_0003.cr3
    JPG/   3 files          IMG_0001.jpg  IMG_0002.jpg  IMG_0003.jpg

  2024/Dec-2024/
    RAW/   12 files
    JPG/   10 files

  ⚠  4 conflicts — files already exist at destination
```

Stats chips at the top:
- `N files to move/copy`
- `N conflicts` (if any, shown in amber)
- `N unpaired RAW` / `N unpaired JPG` (if any)

### Step 3 — Conflict Resolution

If conflicts exist, a **Conflicts** section expands inside the preview dialog listing each one.
See [Conflict Dialog](#conflict-dialog) below.

### Step 4 — Execute

Progress bar. On completion: summary of succeeded / failed / skipped.

---

## Workflow 2: Export

Entry point: toolbar **Export** button or `Ctrl+E`.

The export operates on the **current working folder** and sends files to the **library folder**.

### Step 1 — If no library folder is set

A folder picker is shown first. Once chosen, it is saved and the export dialog opens.

### Step 2 — Export Dialog

Header row shows:
- Working folder path
- Library folder path (with a small "change" link)

**Pruned files toggle:**

```
[ ] Include pruned files in export
```

Unchecked by default. When unchecked, pruned files are greyed out / excluded from the preview. When checked, they are included and shown with a faint red tint and ✕ badge so the user knows they're marked.

**Operation toggle:**
```
(•) Copy — keep originals in working folder
( ) Move — remove originals after export
```

**Preview tree** (same format as Auto-sort, filtered by the pruned toggle):

```
  2025/Jan-2025/
    RAW/   24 files
    JPG/   24 files
    
  ⚠  2 conflicts
```

**Conflict section** if any — see below.

**Buttons:**
- `Export N files` (primary, blue) — disabled until library folder is set
- `Cancel`

### Step 3 — Execute

Progress bar. On completion: summary + option to "Open library folder".

---

## Conflict Dialog

Used by both workflows. Triggered when any destination file already exists.

### Appearance

A scrollable list of conflicting files. Each row shows:

```
  IMG_0001.cr3    ~/Library/2025/Jan-2025/RAW/IMG_0001.cr3   [Overwrite ▾]
  IMG_0002.jpg    ~/Library/2025/Jan-2025/JPG/IMG_0002.jpg   [Skip     ▾]
```

Each row has a per-file dropdown: **Overwrite** / **Skip** / **Rename** (auto-appends `_1`, `_2`, etc. until unique).

At the top, bulk controls:

```
Apply to all:  [Overwrite]  [Skip]  [Rename]
```

Clicking a bulk button sets all unresolved rows to that choice. Rows with an explicit per-file choice keep their value unless the user clicks bulk again.

A `[ ] Remember this choice for this operation` checkbox applies the bulk default to any new conflicts found mid-operation (e.g. if a file appears between preview and execute).

### Rename preview

When **Rename** is chosen for a file, the row shows the proposed new name in muted text:

```
  IMG_0001.cr3    → IMG_0001_1.cr3   [Rename ▾]
```

---

## Non-goals (out of scope for this spec)

- Undo / undo history for move operations (the OS Trash handles prune; export/sort moves are not yet undoable)
- Watching the library folder for external changes
- Duplicate detection beyond filename + destination path
- EXIF editing
- Uploading or cloud sync

---

## Decided

- **Folder bar** — user can show/hide it via a toolbar toggle button (or `Ctrl+B`). Visibility persists across sessions via `QSettings`. Defaults to visible.
- **Auto-sort recursion** — scans recursively into all subdirectories of the source folder.
- **Empty folders after Move** — silently removed after all files have been moved out.
