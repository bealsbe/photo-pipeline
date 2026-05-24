# Photo Pipeline — Claude Context

## What this app is

A PySide6 desktop app for culling and organising RAW+JPG photo shoots. Dark-themed, keyboard-driven. The user browses a folder of photos, marks files for deletion (prune), links RAW+JPG pairs by filename stem, sorts files into subfolders, and eventually exports to a standardised date-based library structure.

## Architecture

```
main.py                         entry point
app/
  main_window.py                QMainWindow — toolbar, menus, central widget, all top-level wiring
  models/
    photo_record.py             PhotoRecord dataclass — one object per file on disk
    collection.py               PhotoCollection — list of records, pair detection, filter/sort
    sidecar.py                  Read/write .photo-pipeline-pairs.json pair keys
  ops/
    separate.py                 SeparationPlan + MoveOp — file-move logic for Sort
    trash.py                    send2trash wrapper for Prune Review
  scanning/
    scanner.py                  ScanWorker (QThread) — recursive directory scan
  thumbnails/
    generator.py                ThumbnailGenerator — background pixmap generation, in-memory LRU
    disk_cache.py               Persistent thumbnail cache on disk
    image_loader.py             rawpy + Pillow image loading
  ui/
    grouped_grid.py             GroupedGridView (QScrollArea) — main grid widget
                                  _DateSection, _DateHeader, _SectionGrid (QListView), _SelectionBar
    thumbnail_grid.py           ThumbnailDelegate, PhotoGridModel, ThumbnailGridView
    file_list.py                FileListWidget — list view (QTreeView)
    filter_bar.py               FilterBar — RAW/JPG/Paired/Unpaired toggles + sort controls
    proxy.py                    PhotoFilterProxy (QSortFilterProxyModel)
    viewer.py                   ImageViewer — full-screen pan/zoom
    import_dialog.py            ImportDialog — progress UI for import scan + separation
    pair_dialog.py              PairDialog — preview + confirm RAW+JPG stem matches
    separate_dialog.py          SeparateDialog — preview + execute Sort (RAW/ JPG/ subfolders)
    prune_review.py             PruneReviewDialog — confirm prune marks → Trash
    shortcuts_dialog.py         KeyboardShortcutsDialog
    icons.py                    SVG icon helpers
```

## Key data flow

1. User opens/imports a folder → `ScanWorker` emits batches of `PhotoRecord` → `PhotoCollection` → `GroupedGridView.reset_records()`
2. `PhotoCollection.build_pairs(pinned_keys=..., auto_detect=False)` links RAW+JPG pairs by canonical stem. Import uses `auto_detect=True`. Manual pairs loaded via `sidecar.read_paired_keys()`.
3. Filter bar drives `PhotoFilterProxy` which feeds both grid and list views.
4. Grid is grouped by `shot_time.date()`. Each date section is a `_DateSection` containing a `_SectionGrid` (QListView in IconMode).
5. Thumbnails are generated lazily by `ThumbnailGenerator` (worker thread), written to disk cache, faded in via `OPACITY_ROLE`.

## Pairing model

- A **pair** is two files (one RAW, one JPG) sharing the same lowercase filename stem and the same canonical parent directory.
- **Canonical parent** strips one level if the immediate parent folder is named `RAW` or `JPG`, so pairing survives Sort operations.
- Pairs are persisted to `.photo-pipeline-pairs.json` as `(relative_parent, stem)` tuples.
- `build_pairs(pinned_keys=None, auto_detect=False)` — pass `pinned_keys` from sidecar for normal open; pass `auto_detect=True` for import.

## Sort vs Pair

| Feature | What it does |
|---------|-------------|
| **Sort** (`Ctrl+Shift+S`) | Physically moves files into `RAW/` and `JPG/` subfolders in the current folder |
| **Pair** (`Ctrl+Shift+P`) | Records stem-matched relationships to the sidecar. No files are moved. |

These are independent. Sort does not create pairs. Pair does not move files.

## Select mode

Toggle with `S` or the toolbar Select button. In select mode:
- Thumbnails show circle checkboxes (orange filled + ✓ when selected, faint outline when not)
- Clicking a thumbnail toggles its selection without clearing others
- A selection bar appears at the bottom showing the count and a Done button
- Done exits select mode and clears selection

Outside select mode, single-click highlights one item (standard browse selection).

## Sidecar files

| File | Location | Content |
|------|----------|---------|
| `.photo_pipeline.json` | Scanned folder root | Prune marks (path → bool) |
| `.photo-pipeline-pairs.json` | Scanned folder root | Pair keys `[{parent, stem}]` |

## Coding conventions

- PySide6 / Qt6 only (no PyQt5, no Qt5 compat shims)
- `QItemSelectionModel.Select` / `.Deselect` — class-level enums, not instance attributes
- `QGraphicsOpacityEffect` for opacity animation on non-top-level widgets (not `setWindowOpacity`)
- Selection bar is a child of `GroupedGridView` (the QScrollArea), NOT the viewport — viewport children are painted over by the content widget
- Cross-section selection: `_on_section_selection_changed` uses `sender()` to identify source; deselects all other sections when not in select mode
- Always call `viewport().update()` after `clearSelection()` to ensure the repaint fires

## Pending / known issues

- Selection highlight sometimes lingers until hover (viewport repaint timing) — partially mitigated, under investigation
- No threading is used for file operations (Sort, Trash); large operations block the UI briefly
