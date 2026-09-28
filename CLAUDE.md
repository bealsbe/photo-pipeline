# Photo Pipeline — Claude Context

## What this app is

A PySide6 desktop app for culling and organising RAW+JPG photo shoots. Dark-themed, keyboard-driven. The user browses a folder of photos, marks files for deletion (prune), links RAW+JPG pairs by filename stem, sorts files into subfolders, and eventually exports to a standardised date-based library structure.

## Architecture

```
main.py                         entry point
app/
  main_window.py                QMainWindow — actions, menus, central widget, all top-level wiring
  exif.py                       Container metadata with no pixel decode — capture time,
                                  orientation, embedded MPF/EXIF previews (TIFF + JPEG)
  models/
    photo_record.py             PhotoRecord dataclass — one object per file on disk
    collection.py               PhotoCollection — list of records, pair detection, filter/sort
    sidecar.py                  Read/write .photo-pipeline-pairs.json pair keys
    crop.py                     Crop (normalised rect) + AspectRatio presets
    crop_sidecar.py             Read/write .photo-pipeline-crops.json
    library_index.py            LibraryIndex / LibraryDay — which library days hold photos
                                  (dates parsed from folder names; no Qt)
  ops/
    separate.py                 SeparationPlan + MoveOp — file-move logic for Sort
    trash.py                    send2trash wrapper for Prune Review
    crop_export.py              Cropped-JPEG re-encode + XMP sidecar writing for RAW
    edit_copy.py                select_marked_raws + EditCopyPlan — marked RAWs → one flat
                                  folder to edit from (never overwrites)
  scanning/
    scanner.py                  ScanWorker (QThread) — recursive scan, threaded EXIF reads,
                                  emits records in batches; build_records() is shared
    library_indexer.py          LibraryIndexWorker + DayRecordsWorker (QThreads) for the calendar
  thumbnails/
    generator.py                ThumbnailGenerator — background pixmap generation, byte-budgeted LRU
    disk_cache.py               Persistent thumbnail cache on disk (JPEG)
    image_loader.py             rawpy + Qt full-resolution loading for the viewer
  ui/
    grouped_grid.py             GroupedGridView (QScrollArea) — main grid widget
                                  _DateSection, _DateHeader, _SectionGrid (QListView), _SelectionBar
    thumbnail_grid.py           ThumbnailDelegate, PhotoGridModel, ThumbnailGridView
    file_list.py                FileListWidget — list view (QTreeView)
    filter_bar.py               FilterBar — RAW/JPG/Paired/Unpaired toggles + sort controls
    proxy.py                    PhotoFilterProxy (QSortFilterProxyModel)
    viewer.py                   ImageViewer — full-screen pan/zoom (read_only for library photos)
    calendar_view.py            CalendarView — library browser: year / month / day pages,
                                  custom-painted, with slide + zoom transitions
    crop_overlay.py             CropOverlay — interactive crop rect over the image view
    crop_bar.py                 CropBar — aspect-ratio presets, custom ratio, apply/cancel
    import_dialog.py            ImportDialog — progress UI for import scan + separation
    pair_dialog.py              PairDialog — preview + confirm RAW+JPG stem matches
    separate_dialog.py          SeparateDialog — preview + execute Sort (RAW/ JPG/ subfolders)
    prune_review.py             PruneReviewDialog — confirm prune marks → Trash
    edit_copy_dialog.py         EditCopyDialog — preview + threaded, cancellable RAW copy
    shortcuts_dialog.py         KeyboardShortcutsDialog
    side_menu.py                SideMenu — left icon rail; ☰ slides it open over the content
                                  to show command names + shortcuts
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

## Renaming at export

The export dialog's "Rename to date, time and sequence" option (on by default)
names files `YYYYMMDD_HHMM_NNNN.ext` via `LibraryPlan(rename=True)`.

- The sequence counts per calendar day in capture order, and continues from
  the highest `NNNN` already in the library for that day.  So a
  re-export of the same photos makes new copies rather than conflicts.
- Both halves of a RAW+JPG pair share one number and one shot datetime.

The library layout is `2025/Mar-2025/12-Mar-2025/RAW/…`.  The sequence
lookup checks both the day folder and the older month-level `RAW/`/`JPG/`
folders, so numbering carries on across libraries exported before day
folders were added.

## Copy marked RAWs for editing

`Ctrl+Shift+E` (Prune menu / side menu) copies the **RAW** of every marked shot
into one flat folder to edit from.  There's no date tree, no renaming and no
JPGs, and the originals are never touched.  It's separate from the library
export.

- A marked JPG contributes its RAW: the confirmed pair, or else a RAW with the
  same stem in the same canonical folder (`PhotoCollection.stem_siblings`).
  Marks only propagate across *confirmed* pairs, so without the name match a
  shot marked on its JPG in an un-paired folder would be dropped.  Marked
  JPG-only shots are reported, and marked+pruned shots are skipped.
- **Never overwrites.**  A name taken by a different file, or by an earlier file
  in the same batch, gets `_1`, `_2`….  An identical copy (same size, and mtime
  within 2 s for exFAT) found at the name *or any suffixed name* is skipped, so
  re-running after marking more shots copies only the new ones.
- Cropped RAWs get the `.xmp` crop sidecar, as in the library export.
- The destination and "subfolder named after the shoot" option persist in
  QSettings (`edit_copy/*`).

## Library calendar

`Ctrl+3` (or the toolbar calendar button) shows the **library** folder by date —
not the working folder.  Year view = 12 mini-months shaded by shot count
(quantile heat levels); month view = day cells with a 1-photo or 2×2 mosaic;
clicking a day opens its photos in a read-only `GroupedGridView`.

- **Dates come from the path**, not EXIF: the `DD-Mon-YYYY` day folder, then a
  renamed `YYYYMMDD_HHMM_NNNN` stem (old month-level libraries), then EXIF /
  mtime as a last resort.  Month names are parsed with `calendar.month_abbr`,
  the same table `library.day_folder()` writes with.  Indexing is a walk plus
  one stat per file on `LibraryIndexWorker`.
- In the day view a record with no EXIF time takes the folder's date (keeping
  its mtime's time-of-day), so the grid never files it under the copy date.
- The calendar owns a **separate `ThumbnailGenerator`**: the main one is
  `clear()`ed on every working-folder scan.  They share the disk cache, and
  preview records are built from the index's `stat` exactly as `ScanWorker`
  does so the cache keys match.
- Library photos are **read-only**: `GroupedGridView(read_only=True)` ignores
  P/M, and `ImageViewer(read_only=True)` hides Prune/Keep/Crop.
  `MainWindow._open_viewer` swaps the viewer when the read-only flag differs.
  "Open as working folder" scans the day's folder in the normal workflow.
- Month/year pages are single painted widgets; hover and fade-in repaint only
  the affected cell via `update(rect)`.  Every page change runs through
  `_Transition`, which animates between grabbed frames.  Input that arrives
  mid-animation grabs the overlay's current frame, so it chains smoothly.
- The index is rebuilt lazily: on first show, after an export
  (`mark_stale()`), on library change, or with F5.

## Sidecar files

| File | Location | Content |
|------|----------|---------|
| `.photo_pipeline.json` | Scanned folder root | Prune marks (path → bool) |
| `.photo-pipeline-pairs.json` | Scanned folder root | Pair keys `[{parent, stem}]` |
| `.photo-pipeline-export.json` | Scanned folder root | Marked-for-export paths |
| `.photo-pipeline-crops.json` | Scanned folder root | Crop rects (path → normalised rect + ratio label) |

## Side menu

The left rail is `SideMenu`, not a QToolBar.  Collapsed it is 48 px of icons;
☰ (or `Ctrl+\`) slides it open to show each command's name and shortcut.
Choosing a command, clicking the dimmed content, Escape or ☰ closes it.

- The panel **floats over** the content: only `placeholder()` (a fixed 48 px
  slot) is in the layout.  Pushing the content aside would re-flow the grid on
  every animation frame.  The panel must be `raise_()`d after the content
  column is built, since siblings stack in creation order.
- **Each shortcut is bound exactly once.**  `_build_actions()` creates one
  QAction per command and both the menu bar and the side menu use it.  When
  the menu and toolbar each had their own copy, Qt treated the shortcut as
  ambiguous and fired neither (Ctrl+1/2/3, Ctrl+O … were dead).  Don't add a
  second QAction with the same shortcut anywhere in the window.
- Buttons follow their QAction via `setDefaultAction()`.  `add_action(..., hint=)`
  shows a key that is handled elsewhere (P / M live in the grid).

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

## Performance at scale

A shoot is thousands of files, so the hot paths are built around four rules.
Breaking any of them is what made the app slow before.

**Never decode a full-resolution frame to make a small one.**  A 42 MP camera
JPEG costs ~65 ms to decode even with libjpeg's DCT downscaling, against
~4 ms for the 1616×1080 MPF preview the camera already embedded in the same
file.  `app/exif.py` extracts it; RAW gets the same treatment through
`rawpy.extract_thumb`.  The preview is stored in sensor orientation, so it
must be rotated by the *primary* image's EXIF Orientation — never trust the
preview stream's own.

**The filter proxy must not sort.**  `lessThan` in Python is n·log n trips
across the C++/Python boundary; it cost 2.0 s per model at 10 000 records and
the app has two models.  Ordering lives in the source models
(`set_sort` / `records_in_order`), and `PhotoFilterProxy` only filters.  For
the same reason, bulk reads go through `visible_records()` rather than
walking `rowCount()` and pulling `Qt.UserRole`.

**Per-file work in the scanner is threaded and batched.**  Capture-time reads
are pure I/O latency, so `ScanWorker` runs them across a pool and emits
records in batches of 250 rather than one signal per file.

**Caches are bounded by bytes, not entries.**  A pixmap ranges from 240 KB (a
thumbnail) to 169 MB (a 42 MP frame), so an entry count is either wasteful or
useless.  The viewer additionally trims its preload radius to what the budget
can hold, so it never decodes a neighbour it is about to evict.

Rough costs at 10 000 files, warm cache, on an NVMe SSD: scan ~0.7 s on the
worker thread, ~60 ms of main-thread work after it, JPG thumbnails ~0.6 ms
each across 8 workers.

## Pending / known issues

- Selection highlight sometimes lingers until hover (viewport repaint timing) — partially mitigated, under investigation
- No threading is used for file operations (Sort, Trash); large operations block the UI briefly
- `ThumbnailGridView` in `ui/thumbnail_grid.py` is dead code — only
  `GroupedGridView` is instantiated.  `PhotoGridModel` and `ThumbnailDelegate`
  in the same module *are* live; the view class and its `_prefetch_nearby`
  are not (the grouped grid has its own)
- Thumbnails still re-decode when the generation size changes, since the disk
  cache is keyed by thumb size.  `_GENERATION_SIZE` is fixed at 300 so this
  only bites if that constant is edited
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
