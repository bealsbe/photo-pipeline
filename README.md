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
- **Search index (Rust only):** `~/.cache/photo_pipeline/search/<library>/`:
  one vector per shot of the library (f16) with its content key and
  capture info, and the path map that tells which files are already
  indexed.  Safe to delete (search then needs the library indexed again).
- **Downloaded models (Rust only):** `~/.cache/photo_pipeline/models/`, the
  search model's files, checked against pinned SHA-256s (see "Models").
  Safe to delete (they are downloaded again when asked).
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
  search.rs            the search page: field, start page, results, Open day
  search_results.rs    results from hits (no I/O), burst stacks, the fursuits' list,
                       completions, recent searches
  search_setup.rs      the download dialog, indexing in the status bar, banners
  search_link.rs       the page's link to the search engine (and its test stand-in)
  dialogs.rs           Export, Edit Copy, Sort, Prune Review, shortcuts
  quality.rs           the analysis as the app holds it (runs on request)
  hints_ui.rs          its Hints control and menu, progress, banner
  bulk.rs              the bulk actions from its hints, and their dialog
  commands.rs          every command, its shortcut, icon and menu placement
  kit.rs, style.rs     shared view pieces and widget styles
  global_menu.rs       the KDE global menu (dbusmenu + KWin appmenu)
  testing.rs           PP_SCRIPT test driver (see below)
src/ui/                custom widgets: grid, image stage, burst sheet,
                       calendar stage, side menu, icons, symbols, hint
                       plates (the analysis' marks on photos)
  theme.rs             the design tokens: colours, type, spacing, motion
  motion.rs, tags.rs   entrance animation, activity bar, file-type tags
src/model/             records, collection & pairing, filters, crops,
                       sidecars, library index and its saved tree
src/ops/               file operations: export, sort, trash, edit copy,
                       crop export, and fsutil (the safe-write primitives)
src/imaging/           JPEG/RAW decoding, thumbnails, viewer loader
src/ml/                the search model: registry, download, the helper
                       process (--ml-worker), its client, preview preparation
src/search/            library search: the engine the app holds, the index
                       files, the indexing pass, queries, estimates
src/exif.rs            container metadata without decoding pixels
native/                C shims over LibRaw and libjpeg-turbo
vendor/LibRaw          LibRaw 0.22.2, the version rawpy uses
assets/fonts           Barlow, with tabular figures built in (see its README)
assets/icons           the app icon (SVG, and PNGs from 16 to 256 px)
assets/*.desktop.in    the desktop entry install.sh fills in and installs
testdata/              camera header excerpts for the unit tests (see its README)
scripts/               check-private.sh (see "Testing")
```

## Look

The chrome (bars, menus, panels, dialogs and the calendar) uses
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
  filter bar's **All · To review · Marked · Pruned** choice narrows the
  grid: To review to the shots still to review (Ctrl+U, or click the
  progress), Marked to those marked for export, Pruned to those marked to
  prune.  A photo leaves as soon as its mark no longer fits (under To review,
  once you prune or mark it; under Pruned, once P un-prunes it) and the next
  one is selected, so P and M alone work through the pile.  Pruned shows
  every pruned photo, whatever Preferences ▸ *Show pruned photos* says; that
  setting (off at first) decides whether All and Marked keep pruned photos,
  crossed out, or drop them as you prune (and when one drops out, the next
  is selected there too).
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
  after it.  **B** in the grid or quick look opens the viewer on
  the selected photo's burst.  Preferences ▸ *Show a burst's frames first*
  turns the arrival view off (the viewer then steps frame by frame, and B
  still shows them all).
- **Analysing a folder.** Prune ▸ *Analyse this folder* (also on the side
  rail, and in the Hints menu) reads each photo's embedded preview once
  and looks for frames that look out of focus, accidental frames (almost
  black, white or one tone: a lens cap, a pocket, the floor; a lit subject
  against a dark background never counts as one), files that
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
    accidental, duplicates (only the extra copies: the copy to keep of
    each isn't shown), or the sharpest of each burst, each with its count.
    With Duplicates on, a strip under the filter bar offers *Prune these
    copies…*, which prunes every extra copy shown after one confirmation.
    It works with All · To review · Marked · Pruned: *To review* and
    *Possible problems* make a quick pile to go through with P.  View ▸
    *Show only possible problems* turns that one on and off.
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
gathers the settings on one page: advance after marking, whether All and
Marked show pruned photos (the filter bar's old *Show pruned*), file names
and thumbnail size, the viewer's filmstrip, remembered size and whether a
burst shows all its frames first, the library and
edit-copy folders, search (see "Library search" below), and whether to use
the KDE global menu (a change there takes effect at the next start;
`PP_NO_GLOBAL_MENU=1` still forces the in-window menus).  Changes apply at
once and are saved; the menus and the viewer's toggles stay in step with
the page.

The Storage section shows how much space the thumbnail, library-index,
capture-time and photo-analysis caches take, and clears any of them after
saying what that costs; they are rebuilt as you work (the analysis when you
next analyse a folder).  The search index can be rebuilt or cleared, and the
downloaded model deleted, each after a confirmation that says what it costs.
It also shows any marks waiting to be saved to a
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

## Library search

**Search the library** (Ctrl+F, on the side rail under the calendar, or in
the View menu) finds photos in the library by what they show: type a few
words, such as *red car at night*, and press Enter.  Everything runs on this
computer; nothing about your photos is sent anywhere.  The plan behind it is
`docs/library-search-plan.md`.

- **A page of its own.**  It keeps its query while you work elsewhere, and
  its back button names where it returns ("‹ Grid", "‹ Jul 2026").  Ctrl+F
  brings it back with the field ready to type into (its text selected).  Esc
  closes the list under the field and never leaves the page.
- **The field.**  While you type, a list under it offers *Things* search
  found in your photos (with how many photos show each), *Dates* (months
  and years with photos, with how many days), and your recent searches
  (the last 10) that have a word starting with what you typed.  ↑ ↓ choose
  one; Enter or Tab takes it (Tab with none chosen takes the first): a
  thing or a date goes into the field in place of the word you were
  typing, ready for the next, and a recent search runs.  Enter alone
  searches what you typed and moves to the results.  The list's footer
  shows the syntax: `-word` and dates.  Preferences ▸ Search ▸ *Forget
  them* clears the recent searches.
- **What you can write.**  Plain words (*red car at night*) find photos
  that look like them.  Everything else is optional and combines with
  AND: `"a red car at night"` in quotes sends the words exactly as typed;
  `-crowd` keeps only photos more like your words than like *crowd*
  (*fireworks -crowd*); a date narrows to those days — `2024`, `jun 2024`,
  `apr–jun 2024` or `2023–2024` (`-` and `..` work as the dash; full month
  names too).  Dates alone (`jun 2026`, or `jun 2026 -fursuit`: all of
  June but what a search for *fursuit* would find) show every photo from
  them, in the order they were taken rather than best first ("312 photos
  from Jun 2026"), accidental frames left out.  `@Name` is for a person,
  fursuit or pet you have named, which comes in a later phase: for now
  the strip says so and the other words are searched; a name or a
  `-word` on its own searches nothing, and the page says why.
- **What was understood.**  A quiet strip over the results says what was
  applied besides the words ("Only Jun 2024", "Every photo from Jun
  2026", "Leaving out crowd"), so you can see why they look as they do.
- **Things in your photos.**  The start page lists the things the index
  knows the library holds (*cameras 96*, *fireworks 24*, *food 21*), most
  photos first; a click searches one.  The list holds objects and scenes
  only: people, fursuits and pets never appear in it, or in the list
  under the field; they get pages of their own later.  Photos that
  plainly show the thing you searched for come first, then the rest by
  similarity.  The start page also shows your recent searches and a few
  ways to search.
- **Results, best first.**  One list in the grid of a library day, on the
  same grey, with each photo's date under it (hover for the date and time
  to the second, and where the file is).  A burst is one stacked tile at
  the rank of its best frame, as in the grid: G or its tab shows its frames
  in shooting order, Shift+G every burst (for these results only).  ← →
  move, Space is quick look, Enter or a double-click opens the viewer.
  Results are read-only, like the calendar's days: P, M and U do nothing
  here.
- **Animal words.**  Here, "dog" also looks like the canine fursuits, so a
  search for a real animal (dog, cat, fox, bird…) leaves fursuits out:
  a quiet line at the end says how many ("26 fursuits that look like dogs
  are left out") with *Show fursuits too*.  Shown, they are a list of their
  own after the real animals, "Fursuits that look like dogs", never mixed
  into theirs; *Hide fursuits* on its heading puts them away again.  The
  viewer steps through one list at a time.  "dog fursuit" or "fursuits"
  asks for suits.
- **Open day ›** shows the selected photo's day in the calendar with the
  photo selected; that page's back button reads "‹ Search" (as does Esc)
  and returns to the same results.
- **Before the first index**, the page says what indexing costs (what is
  read from the library, how long it takes, what is downloaded and kept)
  and offers *Index the library…*.  A dialog asks once: it lists each model
  with its size, its licence in plain words and its publisher, with the
  sources and SHA-256 checksums a click away, then shows the download.  A
  file that doesn't match its checksum is deleted and the dialog says so.
- **Indexing** runs in the background, newest days first: the status bar
  shows how far it has got, with × to stop (what is indexed is kept) and a
  click for its steps.  A search while it runs looks at the photos indexed
  so far, under a strip that says so, and fills in as it goes.  After an
  export, the new photos are added on their own, as a quiet line in the
  status bar (Preferences ▸ Search ▸ *Index new exports*).
- **When something fails**, a banner under the folder bar says what, in any
  view: the graphics card stopped responding, gave different answers from
  the processor, or isn't there; the library stopped answering; or the
  search helper couldn't run.  It offers *Try the card again* and the way
  to Preferences.  Nothing ever moves to the processor by itself: the
  processor is used only when chosen in Preferences ▸ Search ▸ *Run models
  on*.  Searching what is already indexed keeps working.
- **Preferences ▸ Search:** where models run (the graphics card, checked
  against the processor first, or the processor, with what each costs),
  *How close matches must be* (a slider from closer to looser, with a reset
  to the default, good matches), *Index new exports*, the search model, and
  the recent searches.
- **Pointed out quietly:** only the rail, the View menu, Ctrl+F and the page
  explaining itself.

### How it works

- **The first index is asked for**, from the search page; after that each
  export is added on its own (Preferences ▸ Search ▸ *Index new exports*).
  Indexing reads each shot's embedded preview once (about 0.35 MB a shot),
  newest days first, in the background, stepping back while the grid waits
  for thumbnails; × stops it and what is done is kept.
- **Where models run** is Preferences ▸ Search ▸ *Run models on*: the
  graphics card (the default; WebGPU over Vulkan, used only after a
  self-check against the processor on fixed inputs agrees) or the
  processor.  If the card is chosen and fails (no usable card, the
  self-check, a driver crash), indexing stops and a banner says what
  failed.  **Nothing is ever moved to the processor by itself**; choose it
  in Preferences if you want it.  Searching what is already indexed keeps
  working either way: the text side always runs on the processor.
- **The helper process.**  Models run in `photo-pipeline --ml-worker`, the
  same binary started by the app, at low priority, exiting after a few idle
  minutes.  A driver fault takes the helper down, not the app.
- **Which card.**  The helper lists the Vulkan adapters itself and takes a
  discrete card first, else an integrated one; a software adapter
  (llvmpipe, SwiftShader) is never used, since that would be processor work
  in disguise, and counts as no card.  The choice is pinned through Mesa's
  device-select layer so the runtime can't wander to another device, and
  the status names the adapter actually used.  `PP_GPU=<part of a name>`
  picks a particular hardware adapter for testing (no setting for it yet).
- **Keys survive renames.**  A shot is identified by a digest of its file's
  size and first 64 KB, so Sort, export and copies keep its vector; a
  cropped JPG re-encoded by a Move export is indexed again.
- **How close matches must be** (Preferences ▸ Search) is a floor on the
  model's own match probability (SigLIP 2's trained calibration, so it
  means the same for every query): looser shows weaker matches, stricter
  only the plainest; the default (0.3 %) keeps empty and dark frames out
  while a thing most of the library shows comes back whole.  A setting
  saved by an earlier build on another scale falls back to the default.
- **Hidden tags.**  A vocabulary of a couple of hundred generic words
  (`src/search/vocab.rs`: objects, scenes, people, fursuits, animals) is
  encoded once by the text tower and kept beside the index; each indexed
  photo is then tagged, from its stored vector alone, with the words whose
  match probability reaches 2 % (a plain match; the search default is
  0.3 %).  Words fursuits are taken for — the animals, a stuffed animal —
  count only where the word beats *fursuit*.  Tags are never shown on a
  photo: they make *Things in your photos*, the completions and the
  ranking boost (a photo tagged with what you typed comes first).
  Tagging is arithmetic on vectors already in memory, on the engine's
  thread, as photos are indexed.
- **`-word` and animal words** are the same measured contrast: a photo
  counts only where its similarity to your words beats its similarity to
  the word left out (or to *fursuit*).  Dates go by the day the shot was
  taken, as the calendar files it.
- **Nothing is written into the library**, no sidecar, no XMP; the index,
  the vocabulary's vectors and the models live in
  `~/.cache/photo_pipeline/` (Preferences ▸ Storage shows and clears
  them).  Queries read no photos.

### Models

Downloaded once, after a dialog that names each file, its size and licence;
every file is checked against its SHA-256 before use and deleted if it
doesn't match (`src/ml/models.rs` holds the pins).  Phase 1 uses one model:

| Model | Files | Size | Licence | Source |
|---|---|---|---|---|
| SigLIP 2 ViT-B/16, 256 px (Google) | image tower (fp32), text tower (fp16), tokenizer | 971 MB | Apache-2.0 | Hugging Face `onnx-community/siglip2-base-patch16-256-ONNX`, the transformers.js export of `google/siglip2-base-patch16-256` |

The image tower is the fp32 file on purpose: the fp16 export gives wrong
answers on WebGPU (the self-check refuses it) and is ten times slower on the
processor.  The fp16 text tower's answers equal fp32's.

ONNX Runtime comes through the `ort` crate (pinned release candidate); the
first build downloads it (~120 MB) into `~/.cache/ort.pyke.io` and puts
`libwebgpu_dawn.so` beside the binary.

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
- **No list view.**  The grid is the one way to see a working folder (Grid
  is still Ctrl+2 and the calendar Ctrl+3; Ctrl+1 does nothing).  Hovering
  a photo gives what the list's columns did: both file names, sizes and
  the modified date.  Settings that last showed the list, saved here or
  imported from the Python app, open in the grid.
- **New culling tools** (see "Working through a shoot"): culling progress and
  the To review and Pruned filters, quick look, compare, the viewer's
  filmstrip and auto-advance, undo/redo, the combined RAW+JPG tag, bursts,
  and analysing a folder (hints, the Hints filter and the bulk actions).
  *Show pruned* moved from the filter bar to Preferences, under the same
  saved setting.
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

- **Unit tests:** `cargo test --release`.  They need no photos: the header
  bytes they parse are in `testdata/` (see its README).
- **Tests on real camera files** are ignored unless asked for.  Point
  `PP_FIXTURE_DIR` at a small folder of camera files and run:

  ```sh
  PP_FIXTURE_DIR=<folder> cargo test --release -- --ignored \
      previews_are_found_whatever_the_index_offset loads_embedded_previews
  ```

  The folder should hold an A7 V JPEG (its MPF preview is what's tested),
  plus one RAW of each family the app reads: an A7 V ARW, an older Sony ARW
  (A7R II) and a Canon CR2.  Every RAW in it must give an embedded preview
  of 1000–2400 px.  The files are only read, so symlinks to originals are
  fine.  Asked for without the variable, or with a folder lacking an MPF
  JPEG or any RAW, the tests fail and say what's missing.
- **Speed comparison:**
  `PP_BENCH_DIR=<folder> cargo test --release -- --ignored --nocapture --exact bench::bench`
  runs the scan, thumbnail and viewer paths.  Set `PP_BENCH_SCAN_ONLY=1` for
  slow or remote folders.  `bench::bench_index` times the library calendar:
  a full read, a check, the saved index and opening days.  It never writes
  to the library; it saves the library's index cache, as the app would.
- **Search:** the engine, the pass, the index files, downloads and the
  device rule are unit-tested with a fake helper (no models, no photos).
  Tests that need the models are ignored and take `PP_MODEL_DIR=<folder
  with the SigLIP2 files>`; the phase 0 harness (`src/search/eval.rs`) also
  takes `PP_EVAL_PREVIEWS` and `PP_EVAL_OUT`, and `PP_EVAL_LABELS` to score
  hand labels; phase 2's `tag_thresholds` sweeps the hidden tags' floor
  against those labels, and `library_queries` indexes a scratch library
  (`PP_EVAL_LIBRARY`) and prints the example queries with their hit
  counts, the fursuits left out and the Things list (`PP_EVAL_QUERIES`
  overrides the queries, `;`-separated).  `bench::bench_search_index` indexes a library through the
  real helper (`PP_BENCH_LIBRARY`, `PP_MODEL_DIR`, `PP_BENCH_DEVICE`) into a
  temporary folder and times queries.  `PP_ML_FAKE_CARD=no-card|self-check|crash`
  makes the graphics card fail without touching a device, for UI tests of
  the banner.  Never point these at the real cache: downloads go wherever
  `HOME` says, so use a throwaway one.
- **Scripted UI:** `PP_SCRIPT=<file>` drives the app with commands such as
  `open`, `key`, `click`, `dlg`, `calday` and `shot`; see
  `src/app/testing.rs` for the full list.  To run it headless:

  ```sh
  kwin_wayland --virtual --socket pp-test --width 1600 --height 1000 &
  WAYLAND_DISPLAY=pp-test PP_SCRIPT=script.txt ./target/release/photo-pipeline
  ```

  Use scratch copies of photos and a throwaway `HOME` for anything that
  writes.  `PP_SEARCH_FAKE=1` (or `=indexed`, to start indexed) puts a
  stand-in in the search engine's place: made-up hits from the library
  index (with the query's dates and `-word`s applied, and a share of an
  animal word's hits handed back as fursuits left out), made-up Things,
  downloads and indexing on a timer, and every failure the page shows on
  request (`search fake …`).  It downloads, reads and writes nothing.
  `search complete`, `search things` and `search fursuits show|hide`
  drive and print the completions, the start page's Things and the
  fursuits' list.
- **Profiling:** `PP_PROFILE=1` prints any update, view rebuild or widget draw
  that takes more than a frame, plus gaps in animation, and a summary table on
  exit (`src/profile.rs`; off, it costs nothing).  A build with
  `--features profiler` also samples the CPU in-process when
  `PP_PROFILE_OUT=<file>` is set and writes folded stacks — no perf
  permissions needed.  Normal builds don't include the sampler.
- **Privacy check:** `scripts/check-private.sh` scans the tracked files for
  things that belong to one machine rather than the project: private
  (RFC 1918) addresses, home and `/mnt` paths other than the placeholders
  the tests use, agent scratch paths, e-mail addresses other than
  `example.*` and `noreply`, non-zero file IDs (XMP document IDs, EXIF
  ImageUniqueID) in `testdata/`, photos or screenshots outside `assets/`,
  and tracked `*.local.md` notes.  It prints `file:line` for each hit and
  exits non-zero.  Run it before pushing (`--cached` checks what is
  staged), or make it a pre-commit hook, which checks the staged files:

  ```sh
  ln -s ../../scripts/check-private.sh .git/hooks/pre-commit
  ```

  Tests and docs use placeholders instead: `192.0.2.x` (RFC 5737),
  `example.com`, `/mnt/nas`, `/home/me`.
