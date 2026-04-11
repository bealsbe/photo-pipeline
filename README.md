# Disclaimer

This is vibe coded and super super buggy.  Please don't trust this with real photos, or at the very least make a backup. 


# Photo Pipeline

A fast, keyboard-driven photo culling app for photographers. Browse and prune
RAW+JPG shoots before archiving. Built with PySide6.

## Getting started

```bash
pip install -r requirements.txt
python main.py
```

**Open a folder** — `Ctrl+O` to open an already-organised working directory, or
**Import** — `Ctrl+I` to import a fresh shoot folder (RAW and JPG files are
automatically separated into `RAW/` and `JPG/` subfolders).

## Workflow

1. Open or import a folder. RAW+JPG pairs are detected automatically.
2. Browse in the **grid** (`Ctrl+2`) or **list** (`Ctrl+1`) view.
3. Mark images to discard with `P` or `Delete`. Marking one file in a pair
   marks both.
4. Review marks with `Ctrl+R`, then commit to Trash (reversible via the OS
   recycle bin).

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `P` / `Delete` | Toggle prune mark on selected image(s) |
| `←` / `→` | Previous / next in viewer |
| `Home` / `End` | First / last image |
| `F` / `Space` | Fit to window |
| `1` | 100% zoom |
| `+` / `-` | Zoom in / out |
| `U` | Unmark (clear prune) |
| `Ctrl+R` | Review pruned → Trash |
| `Ctrl+Shift+S` | Separate RAW/JPG into subfolders |
| `Ctrl+1` / `Ctrl+2` | Switch to list / grid view |
| `?` | Show all keyboard shortcuts |
| `Escape` | Close viewer |

## Requirements

- Python 3.10+
- PySide6, rawpy, numpy, send2trash, Pillow
