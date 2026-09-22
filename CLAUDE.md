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
    crop.py                     Crop (normalised rect) + AspectRatio presets
    crop_sidecar.py             Read/write .photo-pipeline-crops.json
  ops/
    separate.py                 SeparationPlan + MoveOp — file-move logic for Sort
    trash.py                    send2trash wrapper for Prune Review
    crop_export.py              Cropped-JPEG re-encode + XMP sidecar writing for RAW
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
    crop_overlay.py             CropOverlay — interactive crop rect over the image view
    crop_bar.py                 CropBar — aspect-ratio presets, custom ratio, apply/cancel
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

## Cropping

Press **C** in the viewer (or the Crop toolbar button) to open the crop tool.

- The rect is stored **normalised (0–1)** against the *orientation-corrected*
  image, so one crop is valid for both halves of a RAW+JPG pair even though
  their pixel dimensions differ.
- Ratios: Freeform, Original, 1:1, 3:2, 4:3, 5:4, 7:5, 16:9, 16:10, 65:24,
  a ⇅ portrait/landscape flip, and a custom `w:h` entry.
- A crop propagates to the record's pair via `_refresh_crop` in main_window,
  exactly as prune and keep marks do.
- Crop mode owns the keyboard: Enter/C apply, Escape cancels, R resets.
  Navigation is disabled while the rect is open.
- Cropped files are visible without opening them: the grid delegate draws the
  crop rect over the thumbnail with the discarded margins scrimmed, and the
  list prefixes the filename with ✂.  The export dialog shows a `✂ n cropped`
  chip plus a per-destination count.

### Crops at export

**Nothing is ever written to the source files** — a crop is metadata until export.

| Type | What export does |
|------|------------------|
| JPG | Re-encoded with the crop applied to the pixels (q95, 4:4:4). EXIF + ICC preserved; Orientation reset to 1 because the crop is computed against the already-rotated image. |
| RAW | Copied **byte-for-byte** plus an `.xmp` sidecar carrying the same rect in Camera Raw's `crs:Crop*` fields. Lightroom / ACR / Bridge open it already cropped; anything ignoring XMP sees a pristine RAW. |

RAW pixels cannot be cropped in place — the file holds undemosaiced sensor data
in a proprietary container, so re-encoding would destroy it.  The XMP sidecar is
the standard answer and is what every RAW developer does.

An existing `.xmp` at the destination is **merged**, not replaced, so develop
settings written by other software survive a re-export.  A stale crop is only
cleared when the user explicitly overwrites a conflicting destination.

## Sidecar files

| File | Location | Content |
|------|----------|---------|
| `.photo_pipeline.json` | Scanned folder root | Prune marks (path → bool) |
| `.photo-pipeline-pairs.json` | Scanned folder root | Pair keys `[{parent, stem}]` |
| `.photo-pipeline-export.json` | Scanned folder root | Marked-for-export paths |
| `.photo-pipeline-crops.json` | Scanned folder root | Crop rects (path → normalised rect + ratio label) |

## Coding conventions

- PySide6 / Qt6 only (no PyQt5, no Qt5 compat shims)
- `QItemSelectionModel.Select` / `.Deselect` — class-level enums, not instance attributes
- `QGraphicsOpacityEffect` for opacity animation on non-top-level widgets (not `setWindowOpacity`)
- Selection bar is a child of `GroupedGridView` (the QScrollArea), NOT the viewport — viewport children are painted over by the content widget
- `CropOverlay` *is* a child of the graphics-view viewport (it must share the
  viewport coordinate system); it maps through `view.mapToScene`/`mapFromScene`
  so pan and zoom come for free
- Widget stylesheets that set a border must be scoped by object name
  (`#cropBar { … }`) — an unscoped rule is inherited by every child label
- Cross-section selection: `_on_section_selection_changed` uses `sender()` to identify source; deselects all other sections when not in select mode
- Always call `viewport().update()` after `clearSelection()` to ensure the repaint fires

## Path-keyed sidecars and Sort

`.photo_pipeline.json`, `.photo-pipeline-export.json` and
`.photo-pipeline-crops.json` are all keyed by **file path**, so a Sort — which
moves every file into `RAW/` or `JPG/` — invalidates them.  `_on_sorted` must
re-save all three after `update_path`, or every mark is silently dropped the
next time the folder is opened.

The pair sidecar is immune: its keys use the canonical parent, which
deliberately strips the `RAW`/`JPG` level.

## Pending / known issues

- Selection highlight sometimes lingers until hover (viewport repaint timing) — partially mitigated, under investigation
- No threading is used for file operations (Sort, Trash); large operations block the UI briefly
- Cropped JPG export re-encodes on the calling thread inside `LibraryPlan.execute`;
  that runs in a worker for the export dialog, but a large cropped batch is
  meaningfully slower than a plain copy
- `ExportDialog._refresh_stats` is called from two places and the second
  overwrites the first — every stat must be passed at both call sites
- PySide6 6.11 / Python 3.14 intermittently aborts inside
  `PySide::destroyQCoreApplication` at interpreter shutdown, after `app.exec()`
  returns.  Pre-existing and unrelated to app code; ~40% of headless runs
- There is no automated test suite; the crop work was verified with ad-hoc
  scripts driving MainWindow/ImageViewer under `QT_QPA_PLATFORM=offscreen`
