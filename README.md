# Photo Pipeline

### AI disclosure

The code and documentation in this project were written by an AI assistant
(Claude Code) from plain-language direction.

Photo Pipeline is a keyboard-driven desktop app for culling RAW+JPG photo
shoots and filing them into a dated library. It is written in Rust with
[iced](https://iced.rs) 0.14. It began as a rewrite of an earlier PySide6 app,
which lives on in the `python` branch, and it keeps that app's keyboard map
and on-disk formats, so either one can open a folder the other has worked in.

## What it does

You open a working folder and go through it in a thumbnail grid. P (or
Delete) marks a photo to prune, M marks it for export, and U clears a prune
mark. A RAW and a JPG with the same name in the same folder are treated as
one shot, so a mark covers both. The status bar shows how many shots you
have reviewed, and the filter bar narrows the grid to all, to review, marked
or pruned photos. With "To review" on, each photo leaves the grid once it is
marked, so P and M alone carry you through a shoot.

Space opens a quick look over the grid, Enter opens the full viewer with a
filmstrip, zoom, a crop tool and an EXIF panel, and C compares two to four
photos side by side with linked zoom and pan. With "Advance after marking"
on, the viewer moves to the next photo after each mark. Undo and redo cover
marks, crops, Sort and moves to the Trash, up to 100 steps per folder.

Frames from one press of the shutter are recognised as a burst from data the
scan already reads. Bursts share a strip in the grid and can be folded into
a single stack with G (Shift+G for all of them). A stack is never marked as a
whole; you open it and mark the frames you can see. In the viewer, arriving
at a burst shows all its frames at once so you can pick from them, compare
them, or zoom into one.

Prune > Analyse this folder runs a background pass over the folder's embedded
previews. It looks for frames that seem out of focus, accidental frames
(almost black, white or a single tone), files that are in the folder twice,
and the sharpest frame of each burst. The results are hints: a small hollow
plate on the photo with a tooltip explaining it, and a Hints filter to show
only those photos. Nothing is marked by the analysis. The bulk actions in the
Prune menu (keep this frame and prune the rest, prune soft frames, prune
accidental frames, prune duplicate copies) each show the photos they would
touch and ask first, act only on photos still to review, and undo as one step.
The pass runs only when you ask for it, and analysing again reads only new or
changed files.

When a folder is done, Export copies or moves its photos (or only those
marked for export) into the library, filed by capture date as
`2025/Mar-2025/12-Mar-2025/` with RAW and JPG subfolders, and optionally
renamed to `YYYYMMDD_HHMM_NNNN`. Review pruned files sends the
pruned ones to the system Trash, and Sort splits a folder into RAW and JPG
subfolders. Copy marked photos for editing copies the RAWs, the JPGs or both
into one folder. Each RAW can get a RawTherapee profile beside it: the bundled
`Base.pp3`, a `.pp3` of your own, or none. A crop is carried into the
profile's `[Crop]` section and into an `.xmp` sidecar, and a profile already
next to a RAW is never replaced.

The library calendar (Ctrl+3) shows the library by capture date, a month or a
year at a time. Clicking a day opens its folder as the working folder, and
Alt+click shows the day's photos read-only first. The folder tree is cached,
so the calendar opens at once and re-reads only folders whose modification
time changed; F5 re-reads everything.

### Library search

Ctrl+F searches the library by what the photos show, using the SigLIP 2
model on your own machine. You can type plain words (`red car at night`),
quoted phrases, `-word` to leave something out, dates such as `jun 2024` or
`2023-2024`, and `@name` for someone you have named. The field offers
completions from the things the index has found and from your recent
searches. Results are read-only and appear in a grid with bursts stacked;
Open day shows a result's day in the calendar.

The first index is started from the search page after a dialog that lists
each model to download with its size and licence. Indexing runs in the
background, newest days first, and new exports are added as they arrive.
Models run in a helper process (`photo-pipeline --ml-worker`) on the
graphics card by default, after a self-check against the processor. If the
card fails, the work stops and a banner says what went wrong. The processor
is used only if you choose it in Preferences. Shots are keyed by a digest of
their size and first 64 KB, so renaming or moving a file keeps its place in
the index. Nothing is written into the photos or the library.

### People and fursuits

With face finding on (the default), indexing also detects faces and groups
them. The People and fursuits page (Ctrl+4) lists the groups so you can name
them, merge or split them, and review suggested photos one at a time with Y
and N. Fursuit heads are found and grouped the same way on a separate tab.
People and fursuits are always kept apart: no merge, move or suggestion
crosses the two. Nothing is named or merged without your answer. In the
viewer, L shows the faces and heads found in a photo so you can name, add or
remove them.

An optional close-call judge (Qwen3-VL-Reranker or Gemma 4, off by default)
can look at fursuit pairs the vectors cannot settle. It runs through
llama.cpp in its own helper process, on the graphics card only, and only
reorders and filters suggestions.

Your names and answers are kept in `subjects.json`; detections, vectors and
face tiles are cache files beside the search index, readable only by you.
Nothing about faces is written into photos or sidecars.

### Models

All models are downloaded only after you confirm, and each file is checked
against a pinned SHA-256 (`src/ml/models.rs`) before use.

| Model | Used for | Size | Licence |
|---|---|---|---|
| SigLIP 2 ViT-B/16, 256 px | search, fursuit matching | 971 MB | Apache-2.0 |
| YuNet (OpenCV) | face detection | 0.2 MB | MIT |
| AuraFace v1 (default) | face recognition | 261 MB | Apache-2.0 |
| SFace (OpenCV) | face recognition | 39 MB | Apache-2.0 |
| InsightFace `buffalo_l` | face recognition | 174 MB | non-commercial research only |
| OWLv2 B/16 | finding fursuit heads | 616 MB | Apache-2.0 |
| Qwen3-VL-Reranker 8B | close-call judge (optional) | 9.9 GB | Apache-2.0 |
| Gemma 4 26B-A4B | close-call judge (optional) | 18.1 GB | Apache-2.0 |

## Keyboard

The full list is under Help > Keyboard shortcuts (F1 or ?). These are the
ones used most.

| Keys | Where | Action |
|---|---|---|
| Ctrl+O | main window | Open a working folder |
| Ctrl+2 / Ctrl+3 | main window | Grid / library calendar |
| Ctrl+F / Ctrl+4 | main window | Search the library / people and fursuits |
| Ctrl+E | main window | Export to the library |
| Ctrl+Shift+E | main window | Copy marked photos for editing |
| Ctrl+R | main window | Review pruned files, then move them to the Trash |
| Ctrl+Shift+S | main window | Sort into RAW and JPG folders |
| Ctrl+U | main window | Show only photos to review, or all |
| Ctrl+Z / Ctrl+Shift+Z or Ctrl+Y | anywhere | Undo / redo |
| Ctrl+Shift+A | anywhere | Advance after marking, on or off |
| Ctrl+, | main window | Preferences |
| Ctrl+\\ / Ctrl+B | main window | Menu labels / folder bar |
| Ctrl+Q | main window | Quit |
| P or Delete / M / U | grid, quick look, viewer, compare | Prune / mark for export / clear prune |
| S | grid | Select mode |
| Space | grid | Quick look |
| C | grid | Compare the selected photos |
| B | grid, viewer | Every frame of the photo's burst |
| G / Shift+G | grid, search | Stack or expand a burst / all bursts |
| ← → or A D | viewer, quick look | Previous / next photo |
| Shift+← → | viewer | Back to this burst's start / on to the next burst |
| PgUp / PgDn | viewer | Before or after the current burst |
| F / 1 / + − | viewer, compare | Fit / actual size / zoom |
| C | viewer | Crop (Enter applies, Esc discards, R resets) |
| T / F11 / L | viewer | Filmstrip / full screen / face and fursuit labels |
| Ctrl+= / Ctrl+− | burst frames | Bigger / smaller frames |
| ← → / [ ] / T | calendar | Previous or next month / day with photos / today |
| Y / N / X | reviews | Same / not the same / not a face or head at all |

## Requirements

Linux with Rust 1.85 or newer, gcc and g++, pkg-config, CMake, glslc, git,
the zlib, libjpeg-turbo and Vulkan headers, and libgomp. On Fedora:

```sh
sudo dnf install rust cargo gcc-c++ pkgconf-pkg-config zlib-ng-compat-devel \
    libjpeg-turbo-devel cmake glslc git vulkan-headers vulkan-loader-devel
```

`build.sh` checks for each of these and lists what is missing. Search and
face finding use a Vulkan graphics card by default, and the close-call judge
runs only on one.

## Build, run and install

```sh
./build.sh            # release build, target/release/photo-pipeline
./build.sh --test     # build, then run the unit tests
./run.sh              # start the app, building first if needed
./run.sh --rebuild    # rebuild, then start
./install.sh          # add a launcher and icon for your user
```

The first build compiles the vendored LibRaw and llama.cpp, fetches the
pinned llama.cpp submodules, and downloads ONNX Runtime (about 120 MB) into
`~/.cache/ort.pyke.io`, so it takes several minutes. `build.sh` then checks
that the binary and both helper processes start on their own.

`install.sh` adds a desktop entry and the app icon under `~/.local/share`
without root. The entry starts `run.sh` from the checkout, so rebuilds are
picked up; run it again if you move the folder, and `./install.sh
--uninstall` removes it. On Wayland the taskbar finds the icon through the
window's app id, which is why the entry is needed.

To take the app to another machine, `scripts/build-appimage.sh` packages the
release build as `dist/PhotoPipeline-<version>-<commit>-x86_64.AppImage`. It
needs [appimagetool](https://github.com/AppImage/appimagetool) on `PATH` or
in `APPIMAGETOOL`. The AppImage carries the app, its WebGPU library, libjpeg
and libgomp; glibc and the graphics stack come from the target system, so it
runs on a distribution at least as new as the one that built it.

With a Global Menu widget in the KDE Plasma panel, the menus move there and
the in-window menu bar hides. Set `PP_NO_GLOBAL_MENU=1` to keep the menus in
the window.

## File safety

Every copy or write goes to a `<name>.~tmp` file beside the destination, is
fsynced and checked for size, and is then renamed into place with
`RENAME_NOREPLACE`, so an existing file is never replaced unless you chose
Overwrite for a conflict the dialog showed you. A move across drives copies
the file, makes it durable, and only then deletes the original. Timestamps
are kept. The Trash never falls back to permanent deletion; if a volume has
no Trash, the file stays where it is and is listed.

Prune marks, export marks and crops are written to sidecars by a saver on its
own thread, in order, so marking never waits on the disk. If a network share
goes away, the marks are kept, copied to
`~/.local/share/photo-pipeline/unsaved/`, and written again when the share
returns or at the next start. If the folder's sidecar changed in the
meantime, the folder's version wins and the copy is set aside in
`unsaved/kept/`.

The one destructive path matches the Python app: a Move export of a cropped
JPG writes the cropped re-encode and then deletes the uncropped original.
RAWs are never re-encoded.

## Where data lives

| Location | What |
|---|---|
| beside the photos | `.photo_pipeline.json` (prune), `.photo-pipeline-export.json` (export), `.photo-pipeline-crops.json` (crops) |
| `~/.config/photo-pipeline/settings.json` | settings; the Python app's settings are imported on first run |
| `~/.local/share/photo-pipeline/` | `subjects.json` with your names and answers, `decisions.jsonl` with a record of your corrections, unsaved marks |
| `~/.cache/photo_pipeline/` | thumbnails (shared with the Python app), capture times, the library index, analysis results, the search index and face data, downloaded models |

Everything under `~/.cache/photo_pipeline/` can be deleted and is rebuilt as
needed; Preferences > Storage shows the size of each cache and clears them.
The files under `~/.local/share/photo-pipeline/` are yours and are never
cleared as a cache.

## Differences from the Python app

RAW+JPG pairing is automatic, so the Pair command and its sidecar are gone.
There is no list view; hovering a thumbnail shows both file names, sizes and
the date. The app adds quick look, compare, bursts and stacks, the analysis
and its hints, undo and redo, a library calendar that keeps its index, search,
and KDE global menu support. Cropped-JPG EXIF is patched in place rather than
rebuilt, which keeps Sony maker notes intact. Sort and Trash run in the
background. Trackpad pinch-to-zoom does not work on Wayland yet; Ctrl+scroll
zooms instead.

## Testing

`cargo test --release` runs the unit tests. They need no photos: the header
bytes they parse are in `testdata/`. Tests that need real camera files or the
search models are ignored by default. `PP_FIXTURE_DIR` points the preview
tests at a small folder of camera files, `PP_MODEL_DIR` points the model
tests at the SigLIP 2 files, and `PP_BENCH_DIR` runs the speed benchmarks in
`src/bench.rs`. None of the default tests download anything or touch the real
cache.

`PP_SCRIPT=<file>` drives the app from a script of commands such as `open`,
`key`, `click` and `shot` (the full list is at the top of
`src/app/testing.rs`). To run it headless under a virtual compositor:

```sh
kwin_wayland --virtual --socket pp-test --width 1600 --height 1000 &
WAYLAND_DISPLAY=pp-test PP_SCRIPT=script.txt ./target/release/photo-pipeline
```

Use scratch copies of photos and a throwaway `HOME` for anything that writes.
`PP_SEARCH_FAKE=1` (or `=indexed`) replaces the search engine with a stand-in
that downloads and reads nothing, and `PP_ML_FAKE_CARD=no-card|self-check|crash`
makes the graphics card fail without touching a device.

`PP_PROFILE=1` prints any update, view rebuild or draw that takes longer than
a frame, plus gaps in animation, with a summary on exit. A build with
`--features profiler` also samples the CPU when `PP_PROFILE_OUT=<file>` is
set.

## Repository layout

```
src/main.rs      entry point; also starts the helper processes
src/app/         application state, update and views; commands.rs holds
                 every command, its shortcut and menu placement
src/ui/          custom widgets, icons drawn in code, theme tokens
src/model/       records, pairing, bursts, filters, sidecars, the saver,
                 the library index, people and fursuit records
src/ops/         export, sort, trash, copy for editing, RawTherapee
                 profiles, and fsutil.rs (the safe-write primitives)
src/imaging/     JPEG and RAW decoding, thumbnails, the viewer's loader
src/ml/          model registry, downloads, the helper processes
src/search/      the search engine, index files, faces and grouping
native/          C shims over LibRaw and libjpeg-turbo
vendor/          LibRaw, llama.cpp and SPIRV-Headers
assets/          Barlow fonts, the app icon, Base.pp3, the desktop entry
testdata/        camera header excerpts for the unit tests
scripts/         build-appimage.sh
```

## Licence

MIT, as declared in `Cargo.toml`. Vendored code keeps its own licence: LibRaw
is under LGPL 2.1 or CDDL 1.0, and the Barlow fonts under the SIL Open Font
License. Downloaded models carry the licences listed above.
