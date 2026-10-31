# Photo Pipeline

### AI disclosure

> The code and documentation in this project were written by an AI assistant
> (Claude Code) from plain-language direction.

## Overview

Photo Pipeline is a desktop app for sorting through a photo shoot. You go
through the photos with the keyboard, keep the good ones, and file them into
a library by date. It handles RAW and JPG pairs as one photo. It is written
in Rust with [iced](https://iced.rs) 0.14.

## What it does

**Culling.** Open a folder and the photos appear in a grid. Press P to prune
a photo, M to mark it for export, and U to clear a mark. Space shows a quick
look. Enter opens the full viewer with zoom, crop and EXIF. C puts up to four
photos side by side. Everything can be undone.

**Bursts.** Frames shot in one burst are grouped. You can fold a burst into
one stack to keep the grid short.

**Hints.** An optional analysis pass looks for soft, accidental and
duplicate frames. It marks nothing itself. It shows a small hint on each
photo, and the Prune menu can act on those hints after showing you the
list.

**Filing.** Export copies or moves photos into the library, in folders by
date, with RAW and JPG kept apart. Review sends pruned files to the Trash.
Sort splits a folder into RAW and JPG. Copy for editing copies marked RAWs
with a RawTherapee profile next to each one. The calendar (Ctrl+3) shows
the library by day.

**Search.** Ctrl+F searches the library by what is in the photos, using a
model that runs on your own machine. You can type words, dates and
`@name`. Faces are found and grouped so you can name people. Fursuit heads
are grouped the same way, on their own tab, and the two are never mixed.
Models are downloaded only after you say yes, and they run on the graphics
card. If the card fails, the work stops and tells you.

## Keyboard

Press F1 in the app for the full list. The main keys:

| Keys | Action |
|---|---|
| Ctrl+O | Open a folder |
| Ctrl+2 / Ctrl+3 / Ctrl+4 | Grid / calendar / people and fursuits |
| Ctrl+F | Search the library |
| Ctrl+E | Export to the library |
| Ctrl+Shift+E | Copy marked photos for editing |
| Ctrl+R | Review pruned files, then send them to the Trash |
| Ctrl+U | Show only photos still to review |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| P or Delete | Prune |
| M | Mark for export |
| U | Clear the mark |
| Space / Enter / C | Quick look / viewer / compare |
| G | Stack or unstack a burst |
| ← → | Previous / next photo |
| F / 1 / + − | Fit / actual size / zoom in and out |
| Y / N | Yes / no, when confirming a match |

## Build and run

You need Linux, Rust 1.85 or newer, a C++ compiler, pkg-config, CMake,
glslc, git, and the zlib, libjpeg-turbo and Vulkan headers. `build.sh`
tells you if anything is missing.

```sh
./build.sh                  # build; the first build takes a few minutes
./run.sh                    # start the app
./install.sh                # add a launcher and icon
scripts/build-appimage.sh   # make a portable AppImage in dist/
```

`cargo test --release` runs the tests. They need no photos, models or
network.

## Where data lives

| Where | What |
|---|---|
| next to the photos | small hidden `.json` files with your marks and crops |
| `~/.config/photo-pipeline/` | settings |
| `~/.local/share/photo-pipeline/` | names, answers and corrections |
| `~/.cache/photo_pipeline/` | thumbnails, indexes, face data and models; safe to delete |

## Licence

MIT. LibRaw is included under LGPL 2.1 or CDDL 1.0. The Barlow fonts are
under the SIL Open Font License. Downloaded models have their own licences.
