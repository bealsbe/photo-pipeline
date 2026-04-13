# Disclaimer

This is vibe coded and super super buggy. Please don't trust this with real photos, or at the very least make a backup.


# Photo Pipeline

A dark-themed, keyboard-driven photo culling and organisation app for photographers. Browse RAW+JPG shoots, mark files for removal, link paired files, sort into a clean folder structure, and export to a standardised library. Built with PySide6.

## Getting started

```bash
pip install -r requirements.txt
python main.py
```

## Opening a folder

- **Import** (`Ctrl+I`) — open a fresh shoot folder. RAW and JPG files are automatically separated into `RAW/` and `JPG/` subfolders and pairs are auto-detected by filename stem.
- **Open** (`Ctrl+O`) — open an already-organised working directory. Pair marks are loaded from the sidecar file if one exists.

## Workflow

1. **Open or Import** a folder.
2. **Browse** in grid view (`Ctrl+2`) or list view (`Ctrl+1`). Photos are grouped by shoot date.
3. **Filter** using the filter bar (RAW / JPG / Paired / Unpaired toggles) and sort controls.
4. **Select** thumbnails — click to highlight one, or enter Select mode (`S`) to multi-select with checkboxes.
5. **Prune** — mark files to discard with the Prune button or `P` / `Delete`. Pruned files are shown with a red overlay and an ✕.
6. **Review** pruned files (`Ctrl+R`) — confirm to send to the OS Trash (reversible via the recycle bin).
7. **Sort** (`Ctrl+Shift+S`) — move files in the current folder into `RAW/` and `JPG/` subfolders.
8. **Pair** (`Ctrl+Shift+P`) — review and confirm RAW+JPG stem-matched pairs. Pair relationships are written to a sidecar file (`.photo-pipeline-pairs.json`) and survive across sessions and Sort operations.

## UI overview

| Area | Description |
|------|-------------|
| Toolbar | Import, Open, Prune Review, Sort, Pair, List/Grid toggle, Select mode, zoom slider |
| Filter bar | Toggle visibility of RAW / JPG / Paired / Unpaired; sort by name or date |
| Grid view | Thumbnails grouped by shoot date, lazy-loaded with fade-in. Paired files show RAW+JPG badges. |
| List view | Flat table with filename, type, date, paired/pruned status |
| Select bar | Appears at the bottom of the grid in Select mode; shows count + Done button |
| Image viewer | Double-click any thumbnail to open full-screen viewer with pan/zoom |

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+I` | Import folder (auto-separate RAW/JPG) |
| `Ctrl+O` | Open existing working folder |
| `Ctrl+R` | Review pruned files → Trash |
| `Ctrl+Shift+S` | Sort files into RAW/ and JPG/ subfolders |
| `Ctrl+Shift+P` | Open Pair dialog |
| `Ctrl+1` / `Ctrl+2` | Switch to list / grid view |
| `S` | Toggle Select mode (checkboxes) |
| `P` / `Delete` | Toggle prune mark on selected image(s) |
| `+` / `-` | Zoom thumbnails in / out |
| `?` | Show all keyboard shortcuts |
| `Escape` | Exit viewer / exit select mode |

## Supported file types

**RAW:** `.cr2` `.cr3` `.nef` `.nrw` `.arw` `.srw` `.orf` `.rw2` `.raf` `.dng` `.pef` `.x3f` `.3fr` `.mef` `.rwl` `.erf`

**JPG:** `.jpg` `.jpeg`

## Sidecar files

Photo Pipeline writes two hidden files alongside your photos:

| File | Purpose |
|------|---------|
| `.photo_pipeline.json` | Prune marks |
| `.photo-pipeline-pairs.json` | Confirmed RAW+JPG pair relationships |

These are safe to delete — doing so clears all marks for that folder.

## Requirements

- Python 3.10+
- PySide6, rawpy, numpy, send2trash, Pillow
