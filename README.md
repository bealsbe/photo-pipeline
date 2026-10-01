# Photo Pipeline

A keyboard-driven desktop app for culling and organising RAW+JPG photo shoots,
written in Rust with [iced](https://iced.rs) 0.14.  It started as a rewrite of
the original PySide6 app, which lives on in the `python` branch.  It keeps
that app's keyboard map and on-disk formats, so either app can open a folder
the other has worked in.  It looks different on purpose (see "Look" below),
RAW+JPG pairing is automatic, and it adds culling tools of its own (see
"Working through a shoot").

## Build and run

```sh
./build.sh           # release build → target/release/photo-pipeline
./build.sh --test    # …then run the unit tests
./run.sh             # start the app (builds first if needed)
./run.sh --rebuild   # rebuild after source changes, then start
./install.sh         # once: add the launcher and app icon to the desktop
```

You need Rust 1.85 or newer, gcc/g++, pkg-config, zlib and libjpeg-turbo
headers, and libgomp.  On Fedora that means `rust cargo gcc-c++ pkgconf-pkg-config
zlib-ng-compat-devel libjpeg-turbo-devel`.  `build.sh` checks for them and
says what's missing.  The first build compiles the vendored LibRaw and takes a
few minutes.

Run `./install.sh` once to add the app to your desktop: a launcher entry and
the app icon, so the taskbar, title bars and Alt+Tab show it instead of the
generic Wayland logo.  It installs for your user only, under `~/.local/share`,
and needs no root.  The entry starts `run.sh` from this folder, so rebuilds
are picked up; run `install.sh` again if you move the folder, and
`./install.sh --uninstall` removes it.  On Wayland a window can't carry its
own icon: KWin and the taskbar look it up through the window's app id,
`io.github.bealsbe.PhotoPipeline`, which names the installed entry.  (KWin
window rules that matched the app before need the new id.)

## Shared with the Python app

- **Sidecars:** the prune, export and crop sidecars (`.photo_pipeline.json`,
  `.photo-pipeline-export.json` and `.photo-pipeline-crops.json`) are written
  byte-for-byte as Python writes them.  Pairs need no sidecar any more, so
  `.photo-pipeline-pairs.json` is neither read nor written; a folder opened in
  the Python app afterwards shows only the pairs it saved itself.
- **Thumbnail cache:** `~/.cache/photo_pipeline/thumbs/300/`, with the same
  keys, so thumbnails made by one app are reused by the other.
- **Capture times (Rust only):** `~/.cache/photo_pipeline/capture-times/`, one
  file per scanned folder, so reopening a folder (every launch reopens the
  last one) reads no file headers for files whose size and modified time are
  unchanged.  Safe to delete.
- **Library index (Rust only):** `~/.cache/photo_pipeline/library/`, one
  file per library (see "Library calendar" below).  Safe to delete.
- **Quality analysis (Rust only):** `~/.cache/photo_pipeline/quality/`, one
  file per analysed folder: sharpness, exposure and hashes per shot, and the
  duplicate check's digests, so analysing again reads only changed files.
  Safe to delete.
- **Settings:** on first run, the Python app's QSettings
  (`~/.config/PhotoPipeline/PhotoPipeline.conf`) are imported.  After that,
  settings live in `~/.config/photo-pipeline/settings.json`.

## Layout

```
src/main.rs            entry point; font setup; --dump-scan parity mode
src/app/               the application: state, update, views
  mod.rs               App, Message, routing, sidecar saving, scanning
  main_view.rs         menu bar, side rail, folder bar, filter bar, status bar
  viewer.rs            the image viewer window (crop tool, EXIF panel)
  calendar.rs          library calendar state and views
  dialogs.rs           Export, Edit Copy, Sort, Prune Review, shortcuts
  quality.rs           the analysis as the app holds it (runs on request)
  hints_ui.rs          its Hints control and menu, progress, banner
  bulk.rs              the bulk actions from its hints, and their dialog
  commands.rs          every command, its shortcut, icon and menu placement
  kit.rs, style.rs     shared view pieces and widget styles
  global_menu.rs       the KDE global menu (dbusmenu + KWin appmenu)
  testing.rs           PP_SCRIPT test driver (see below)
src/ui/                custom widgets: grid, list, image stage, burst
                       sheet, calendar stage, side menu, icons, symbols,
                       hint plates (the analysis' marks on photos)
  theme.rs             the design tokens: colours, type, spacing, motion
  motion.rs, tags.rs   entrance animation, activity bar, file-type tags
src/model/             records, collection & pairing, filters, crops,
                       sidecars, library index and its saved tree
src/ops/               file operations: export, sort, trash, edit copy,
                       crop export, and fsutil (the safe-write primitives)
src/imaging/           JPEG/RAW decoding, thumbnails, viewer loader
src/exif.rs            container metadata without decoding pixels
native/                C shims over LibRaw and libjpeg-turbo
vendor/LibRaw          LibRaw 0.22.2, the version rawpy uses
assets/fonts           Barlow, with tabular figures built in (see its README)
assets/icons           the app icon (SVG, and PNGs from 16 to 256 px)
assets/*.desktop.in    the desktop entry install.sh fills in and installs
```

## Look

The chrome (bars, menus, panels, dialogs, the list and the calendar) uses
the Catppuccin Mocha palette of the KDE desktop it was built on, with its
purple accent.  Everything around a photo being reviewed stays true neutral
grey: the grid's background and mounts, and the viewer's stage.  A tinted
surround shifts how you judge colour and exposure.

- **Purple** (the accent) marks the selection, focus, the active control and
  the main action of a dialog.
- **Green and red** are the keep and prune decisions.  A kept photo wears a
  green check on its corner.  A pruned photo is dimmed and crossed out corner
  to corner, the way a grease pencil marks a contact sheet.
- **RAW and JPG** are small luggage tags under each photo (blue and peach).

Motion is short and eased: hovered photos brighten, thumbnails fade in as they
arrive, photos still loading shimmer, dialogs and menus settle into place, and
a thin purple-to-blue sweep runs along the top of the content while the app is
scanning, reading the library or making thumbnails.  Each animation redraws
only its own widget, so none of it slows the app down.

The type is Barlow, a DIN-like face in the spirit of the lettering on lenses
and camera dials.  It's bundled, so the app looks the same on any machine.
Spacing follows one 4 px scale, and all colours, sizes, radii and timings come
from `src/ui/theme.rs`.  The icons are drawn in code (`src/ui/icons.rs`) with
one stroke weight, each named for what it does.  The app icon is a photo on
a purple tile with a green keep check (`assets/icons`); the 16 and 22 px
sizes are redrawn on the pixel grid rather than scaled down.

## KDE global menu

With a Global Menu widget in the Plasma panel, the menus appear there, and
the in-window menu bar hides; remove the widget and it comes back.  The menu
is exported on the session bus as `com.canonical.dbusmenu` (with KDE icons,
shortcuts and check marks), and the main window is linked to it through
KWin's appmenu protocol on Wayland, or the AppMenu registrar on X11.  Set
`PP_NO_GLOBAL_MENU=1` to keep the in-window menu bar regardless.

## Working through a shoot

- **Culling progress.** The status bar shows how far through the folder you
  are, for example "212 of 740 shots reviewed", with a thin bar.  A shot is
  reviewed once it's pruned or marked, and a RAW+JPG pair counts once.  The
  filter bar's **All · To review · Marked** choice narrows the grid to the
  shots still to review (Ctrl+U, or click the progress).  There, a photo leaves
  the grid as soon as you prune or mark it and the next one is selected, so P
  and M alone work through the pile.
- **Quick look.** In the grid, Space shows the selected photo large over the
  grid without opening the viewer.  Arrow keys (or A/D) step through the
  photos and the grid's selection follows; P and M mark the photo shown; Enter
  opens the viewer on it; Esc or Space closes it.  It opens on the grid's
  thumbnail, sharpens to the camera's preview within a few milliseconds, and
  to the full image once you pause.
- **Compare.** Select two to four photos and press **C** (View ▸ Compare
  selected) to see them side by side in the viewer window, as a row, a column
  or 2 × 2, whichever shows the most photo.  Zoom and pan are linked: 100 %
  (key 1, or double-click) on one photo shows the same spot on all of them,
  for checking focus across a burst.  Arrow keys or Tab move the purple
  outline; P, M and U mark the outlined photo.  Enter opens it on its own in
  the viewer, and Esc goes back.  (Like a Ctrl-click selection, picking photos
  from different days needs select mode, S.)
- **Bursts.** The frames of each shutter press are found from the headers
  the scan reads anyway (sub-second times; on Sony bodies, each frame's
  number in its press).  In the grid they share one strip, a lighter mount
  joined across the gutters, with a tab on the first frame giving the count.
  A strip that wraps stops square at the last column and carries on from
  the first, so every row starts at the same edge.  **G** (or a click on the
  tab) stacks the selected photo's burst into one photo, the edges of the
  frames under it showing below and its tab adding how many are kept and
  pruned; **Shift+G** (View ▸ Stack all bursts, or Preferences) stacks
  every burst and is remembered.  A stack is one photo to the arrow keys,
  quick look and selection.  It shows its sharpest frame on top once the
  folder has been analysed (its first until then), and it can't be marked:
  P and M only ever mark a photo you can see on its own, so open the stack
  (Enter, a double-click or B shows all its frames) or unstack it first.
  With To review on, the photo on top is one still to review.  The
  viewer's filmstrip brackets bursts too.  Arriving at a burst in the
  viewer — stepping to it from the photo before or after,
  or opening the viewer on one of its frames — shows **all its frames**
  first, laid out over the photo area with the one you came to outlined.
  The arrow keys move the outline (Home / End: first and last frame), and P,
  M and U mark the outlined frame; *Advance after marking* moves the
  outline along.  Click a frame (or Enter, +, the wheel) and it zooms up
  into the ordinary viewer, where ← → step through the burst's frames;
  Esc, − at fit, B or *All frames* zoom back out to all of them.
  Ctrl+click or Space picks frames, and **C** compares them (with none
  picked, the outlined frame and its neighbours): compare as above, with
  **1** at 100 % on each frame's own AF point, `,` and `.` changing the
  outlined photo's frame, and Esc back to the burst.  Stepping past either
  end of a burst, or PgUp / PgDn, goes on to the photo or burst before or
  after it.  **B** in the grid, the list or quick look opens the viewer on
  the selected photo's burst.  Preferences ▸ *Show a burst's frames first*
  turns the arrival view off (the viewer then steps frame by frame, and B
  still shows them all).
- **Analysing a folder.** Prune ▸ *Analyse this folder* (also on the side
  rail, and in the Hints menu) reads each photo's embedded preview once
  and looks for frames that look out of focus, accidental frames (almost
  black, white or one tone: a lens cap, a pocket, the floor), files that
  are in the folder twice, and the sharpest frame of each burst.  It only
  ever runs when you ask, in the background: the status bar shows how far
  it has got, with a × to stop it (what it checked is kept), and you can
  go on culling.  Allow about 30 seconds per 5,000 shots on a network
  share, a few seconds on a local disk.  Analysing again reads only new or
  changed photos, and an analysed folder shows its hints as soon as it
  opens.  When it ends, the status bar says what it found, with *Show
  them*.
  - **Hints are suggestions; nothing is marked.**  A photo with a hint
    wears one small plate on its lower left corner, hollow and grey so it
    never reads as a decision: a crisp ring for the sharpest frame of its
    burst, the same ring broken for a frame that looks out of focus, an
    empty frame (yellow) for an accident, two frames for a copy.  Hover
    the photo and the tooltip says why ("Looks out of focus: 4× softer
    than …0757, a frame of this burst that shows the same.").  The plate
    goes once you prune or mark the photo, and a stack shows none (its
    sharpest frame is on top).  Quick look, the viewer and compare name
    the hint beside the file name ("Sharpest of 11", "3rd sharpest of
    11"); a burst's sheet puts it beside each frame's number.
  - **Moments.**  Bursts of the same scene a moment apart (the next within
    2 seconds, and alike) are joined across the gutter by a narrow band,
    so each burst still shows where it starts.  The tooltip says how many
    bursts and frames the moment holds.
  - **The Hints filter** on the filter bar (the Hints control, with how
    many possible problems there are) shows only photos with a hint:
    possible problems (out of focus, accidental, or a copy), out of focus,
    accidental, duplicates (both copies), or the sharpest of each burst,
    each with its count.  It works with All · To review · Marked: *To
    review* and *Possible problems* make a quick pile to go through with
    P.  View ▸ *Show only possible problems* turns that one on and off.
    Before a folder is analysed, the Hints menu says what analysing does
    and how long it should take.
  - **Bulk actions** (Prune menu): *Keep this, prune the rest* (the
    selected frame; in the viewer, the frame outlined on a burst's sheet),
    *Prune all soft frames…*, *Prune accidental frames…* and *Prune
    duplicate copies…*.  Each asks first: a dialog shows how many photos
    it will mark, and the photos themselves with why each is there.  Only
    photos still to review are touched (the ones shown, frames in stacks
    included), it is one undo step, and nothing goes to the Trash before
    Review pruned files.  Of two copies, the one kept is the one whose
    name doesn't read like a copy's ("copy", "(1)"), else the earlier
    export to the library, else the older file.
  - **Compare** from a burst's sheet with nothing picked shows the
    outlined frame with the burst's two sharpest others, once analysed.
  - If the folder stops answering part-way (a network share dropping), a
    banner says how many shots were checked and kept; *Analyse again*
    carries on from there once the folder is back.
- **Viewer.** A filmstrip under the photo shows its neighbours with their
  marks and crops; click to jump, T to hide it.  With *Advance after marking*
  on (Ctrl+Shift+A, or the Auto-advance toggle), P or M marks the photo and
  moves on after a short beat; clearing a mark, or marking the last photo,
  stays put.  The viewer window reopens at the size and maximised state you
  left it in (and position, on X11 — Wayland doesn't tell apps where their
  windows are).  F11 toggles full screen.
- **Undo.** Ctrl+Z undoes and Ctrl+Shift+Z (or Ctrl+Y) redoes, from the main
  window or the viewer (Edit menu).  It covers prune and export marks and
  crops however they were made, Sort, and Prune Review's move to the Trash;
  after a Sort or a trash the status bar also offers an Undo button.  Undoing
  a Sort moves the files back on a background thread; undoing a trash takes
  exactly the files that operation trashed back out of the Trash.  Nothing is
  overwritten: a file whose old place is taken stays where it is and is
  listed.  The history lasts until you open another folder (100 steps).
- **File names.** A RAW+JPG pair wears one two-part tag, so names fit; long
  names are shortened from the start, keeping the sequence number.  Hover for
  both files' full names, sizes and date.  View ▸ Show file names hides the
  names so more photos fit (the tag moves onto the photo's corner).

## Preferences

Preferences (Ctrl+, — in the Edit menu, or at the foot of the side rail)
gathers the settings on one page: advance after marking, file names and
thumbnail size, the viewer's filmstrip, remembered size and whether a burst
shows all its frames first, the library and
edit-copy folders, and whether to use the KDE global menu (a change there takes
effect at the next start; `PP_NO_GLOBAL_MENU=1` still forces the in-window
menus).  Changes apply at once and are saved; the menus and the viewer's
toggles stay in step with the page.

The Storage section shows how much space the thumbnail, library-index,
capture-time and photo-analysis caches take, and clears any of them after
saying what that costs; they are rebuilt as you work (the analysis when you
next analyse a folder).  It also shows any marks waiting to be saved to a
folder that couldn't be reached.  Those live in
`~/.local/share/photo-pipeline/unsaved/` and are never cleared from the app:
they are your marks, not a cache.

## Library calendar

The calendar doesn't read the whole library each time it opens.

- **The index is saved.**  The library's folder tree is kept in
  `~/.cache/photo_pipeline/library/`.  The calendar shows it straight away
  and checks the disk in the background.  If the library can't be reached,
  the calendar keeps the saved index and says so in the status bar.
- **Only changed folders are re-read.**  Each time you switch to the calendar
  (at most every 5 s), it checks every folder's modified time in the
  background.  It re-reads only folders whose time moved.  Adding, removing
  or renaming a file changes its folder's time, and export writes that way.
  The calendar redraws only if something differs.  A day you have open
  reloads with the new files.
- **Refresh (F5 or ⟳) re-reads everything.**  Use it after other software
  rewrites a file in place, which doesn't change its folder's time.
- **Days open without reading every file.**  A renamed file
  (`YYYYMMDD_HHMM_NNNN`) takes its time from its name.  Other files have
  their EXIF read once, and the time is kept in the same cache.  A day needing
  more than 200 reads opens at once and settles its order as they come in.
- **Header reads skip read-ahead.**  Capture times and JPEG previews turn off
  the kernel's read-ahead.  On a network share it had been fetching 1–2.7 MB
  per file for a 64–256 KB read.

Measured on a 110 386-file library on an SMB share:

| | Before | Now |
|---|---|---|
| Calendar opening, library read | 3.5 s every session | 8 ms cache load + 35 ms check in the background |
| Checking for changes | — (full re-read) | 35 ms (253 folder stats) |
| Opening a 294-file day | 2.1 s | 0.2 ms |
| Opening a 19 072-file day | 168 s | 5 ms (+15 ms grid data, off the UI thread) |
| EXIF capture times, 19 072 files | 168 s | 8–16 s |
| JPEG thumbnail from the share | 14.8 ms, 2.7 MB | 6.5–10 ms, 0.3 MB |

The first time a library is opened, or after F5, it's still read in full,
about 3 s here.  Opening a working folder reads capture times with the same
reader, so it gains the read-ahead fix too.

## File safety

Every write goes to a temporary file (`<name>.~tmp`), is flushed to disk
(fsync), checked for size and then renamed into place.  Renames never replace
an existing file (`RENAME_NOREPLACE`), unless you choose Overwrite for a
conflict the dialog showed you.  A move across drives copies and verifies the
file before deleting the original.  After a move, empty source folders are
removed only inside the working folder.  Trash never falls back to permanent
deletion.  Timestamps and extended attributes are carried over, as
`shutil.copy2` did.

**Marks are saved in the background.**  Prune and export marks and crops are
written by a background saver, in order, so a P or M never waits for the disk
(on a network share one save took 10–300 ms).  Pending saves are finished
before a folder's marks are read back and before the app exits.

**Network shares.**  The folder bar marks a working folder or library on a
network share (SMB, NFS, sshfs…) with a small drive icon.  If the share goes
away, a banner names it and says your marks are kept; the app retries every
few seconds (or press Try again) and says when they're saved.  Marks that
couldn't be saved are also copied to `~/.local/share/photo-pipeline/unsaved/`,
so quitting or a crash loses nothing: the next start writes them to the
folder, unless the folder's marks were changed since — then the copy is set
aside in `unsaved/kept/` and the app tells you.  Quitting with unsaved marks
waits a few seconds, then says where they are.

The one destructive path is the same as in the Python app: a **Move** export
of a *cropped JPG* writes the cropped re-encode and then deletes the uncropped
original.  RAWs are never re-encoded.

## Differences from the Python app

These are deliberate:

- **Pairing is automatic.**  Every RAW and JPG with the same name in the
  same folder is a pair as soon as the folder is scanned, so the Pair command
  (Ctrl+Shift+P) and its dialog are gone.  A pair also comes apart as soon as
  one half is trashed or moved away.
- **The look** is new (see above), including sentence-case labels and clearer
  wording in the dialogs.
- **KDE global menu** support (see above); the Python app had none.
- **New culling tools** (see "Working through a shoot"): culling progress and
  the To review filter, quick look, compare, the viewer's filmstrip and
  auto-advance, undo/redo, the combined RAW+JPG tag, bursts, and analysing a
  folder (hints, the Hints filter and the bulk actions).
- **Library calendar** keeps its index between runs and re-reads only
  changed folders (see above).  In a day's grid, a renamed file's capture
  time is read from its name, to the minute, instead of from EXIF.  The order
  is the same, because export numbers each day in capture order; the names
  matched EXIF for every file checked on the author's library.
- **Speed.**
  - Scans are about 14× faster on a local disk and read far less per file on
    a network share.
  - RAW thumbnails come from the smallest embedded preview that covers
    300 px, about 9× faster for bodies that also embed a full-size JPEG.
  - The viewer shows the camera's embedded preview (~1616 px, about 3 ms)
    while the full image decodes, where Python showed "Loading…".
- **Cropped-JPG EXIF** is patched in place, not rebuilt through Pillow.
  Pillow broke Sony maker-note offsets and added IFD0
  `ImageWidth`/`ImageLength`, which exiftool flags as invalid in a JPEG.  The
  new size goes in `PixelX/YDimension` instead.
- **Python bugs fixed:**
  - File sizes use real division (Python printed "7.0 MB" for 7.5 MB).
  - List rows pair files by folder and stem, not by stem alone.
  - Moved records leave the working grid after a Move export.
- **Sort and Trash** run in the background instead of freezing the window.
- **Selection:**
  - Ctrl+A selects across all date sections.
  - The selection survives a prune.
- **Viewer:**
  - A working-folder viewer closes when that folder is rescanned.
  - Mouse-wheel panning moves by pixels.
- **RAW handling:**
  - RAW orientation falls back to LibRaw's when the embedded preview has no
    Orientation tag.
  - RAW EXIF panels show more fields.
- **Not yet supported:** trackpad pinch-to-zoom on Wayland (winit has no
  gesture events on Linux).  Ctrl+scroll zooms.

## Testing

- **Unit tests:** `cargo test --release`.
- **Speed comparison:**
  `PP_BENCH_DIR=<folder> cargo test --release -- --ignored --nocapture --exact bench::bench`
  runs the scan, thumbnail and viewer paths.  Set `PP_BENCH_SCAN_ONLY=1` for
  slow or remote folders.  `bench::bench_index` times the library calendar:
  a full read, a check, the saved index and opening days.  It never writes
  to the library; it saves the library's index cache, as the app would.
- **Scripted UI:** `PP_SCRIPT=<file>` drives the app with commands such as
  `open`, `key`, `click`, `dlg`, `calday` and `shot`; see
  `src/app/testing.rs` for the full list.  To run it headless:

  ```sh
  kwin_wayland --virtual --socket pp-test --width 1600 --height 1000 &
  WAYLAND_DISPLAY=pp-test PP_SCRIPT=script.txt ./target/release/photo-pipeline
  ```

  Use scratch copies of photos and a throwaway `HOME` for anything that
  writes.
- **Profiling:** `PP_PROFILE=1` prints any update, view rebuild or widget draw
  that takes more than a frame, plus gaps in animation, and a summary table on
  exit (`src/profile.rs`; off, it costs nothing).  A build with
  `--features profiler` also samples the CPU in-process when
  `PP_PROFILE_OUT=<file>` is set and writes folded stacks — no perf
  permissions needed.  Normal builds don't include the sampler.
