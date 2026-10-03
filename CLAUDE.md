# Photo Pipeline — Claude context

A keyboard-driven desktop app for culling RAW+JPG photo shoots, in Rust with
iced 0.14.  The original PySide6 app is on the `python` branch; this one keeps
its keyboard map and on-disk formats (sidecars, thumbnail cache, settings
import).  `README.md` is the reference for features, layout, look, file
safety and testing — read it first.  `docs/` holds plans for future work.

## Build and test

```sh
./build.sh            # release build (first build compiles vendored LibRaw)
cargo test --release  # unit tests; must pass with zero warnings
./run.sh --rebuild    # rebuild and start
```

## Rules that matter

- **File safety first.**  Every copy/move/write of a photo goes through
  `src/ops/fsutil.rs` (temp file + fsync + `RENAME_NOREPLACE`, verified
  cross-device moves).  Never overwrite without the user choosing to; Trash
  never falls back to deleting.
- **Sidecars** (prune/export marks, crops) are written by `sidecar::Saver`
  (`src/model/saver.rs`) on its own thread: ordered, newest wins, kept and
  retried when a network share is away, `flush()` before reading marks back
  and at exit.  Never write them from the UI thread.
- **No file I/O on the UI thread.**  The user's folders and library live on a
  NAS (SMB): one open is 1.5–20 ms, one atomic write 10–300 ms.  Work goes to
  worker threads and comes back as `crate::bus::BgEvent`s.
- **Performance:** decode embedded previews, not full frames, for anything
  small; caches are bounded by bytes; per-frame work stays tiny (custom
  widgets request their own redraws).  `PP_PROFILE=1` reports slow updates,
  draws and frame gaps — check new features with it.
- **Analysis and hints** (`app/quality.rs`, `app/hints_ui.rs`,
  `ui/hint_plate.rs`, `app/bulk.rs`): a pass runs only when the user asks
  (`quality::start`); opening a folder reads only its cache
  (`start_cached`).  Hints are suggestions: hollow, neutral glyphs (`WARN`
  only for accidents), hidden on pruned or marked photos and on stacks,
  read by lookup in paint paths (`Hints::plate`).  A mark that comes from
  a hint goes only through `bulk.rs`: a dialog listing the frames, a
  recheck of what is still to review, and `undo::batch` so it is one undo
  step.  Never mark from a hint any other way, and never mark a stack.
- **Commands** live once in `src/app/commands.rs` (label, shortcut, icon, menu
  placement); a shortcut is bound exactly once.  The KDE global menu mirrors
  `MENUS`.
- **Look:** tokens in `src/ui/theme.rs` — Catppuccin chrome, neutral
  `PHOTO_*` greys around photos (never tint those).  Sentence-case copy, plain
  verbs, no ALL-CAPS labels.  Shared view pieces in `src/app/kit.rs` and
  `style.rs`; icons are drawn in code (`src/ui/icons.rs`).
- **Library search:** the page (`src/app/search*.rs`) only talks to the
  engine through `search_link` and its `BgEvent`s; the engine (`src/ml`,
  `src/search`) is separate work.  Results are read-only (no P, M or U)
  and are built from the calendar's library index, never from disk.  When
  the card fails, say so and stop: never offer or move work to the
  processor.  `PP_SEARCH_FAKE` stands in for the engine in tests.
- **Search kinds never mix** (`docs/library-search-plan.md`, decisions 8,
  12, 12b): pets are never combined with fursuits or people, anywhere.
  *Things* lists objects and scenes only (`vocab::Kind::is_subject` is
  the one gate, and the page applies it again to Things and completions,
  whatever the engine sends); a real animal's results never carry a
  fursuit (they come as `fursuits`, a separate list, shown as a section
  of its own, `FURSUITS_DAY`, after the real animals, and the viewer
  steps through one section only); hidden tags are never shown on a
  photo.  The vocabulary (`src/search/vocab.rs`) holds generic words
  only.  Where models run is the user's choice: a failing card is
  reported, never worked around (`src/ml`).  Completions are worked out
  in memory on each keystroke (`search_results::offer`), never with I/O.
- **Windows** open with `app::window_settings()`: it carries the Wayland app
  id (`app::APP_ID`, which must match `assets/<id>.desktop.in` and
  `install.sh`) and the icon.  A window without it shows the generic
  Wayland logo.
- **iced quirks:** within a layer, images draw above quads — overlays on
  images need `renderer.with_layer`; nested layers aren't clipped by parents.
- **Library search (`src/ml`, `src/search`;** plan in
  `docs/library-search-plan.md`): models run only in the helper process
  (`--ml-worker`), never in the app; the device is the user's Preferences
  choice and **a failing card stops the work and is reported, never
  worked around** (no fallback to the CPU; `search::engine` tests hold
  this).  The helper picks the adapter itself (`ml/gpu.rs`: discrete,
  else integrated, never a software one) and reports its real name.  Models are pinned by SHA-256 in `ml/models.rs`, downloaded to
  the cache and checked before use; never commit a model file.  The index
  lives in `~/.cache/photo_pipeline/search/`, never in the library, and
  nothing is written to photos or sidecars.  Keys are content digests
  (size + first 64 KB), not paths.  Tests needing models are ignored
  behind `PP_MODEL_DIR`; nothing in `cargo test` may touch the real cache
  or download anything (inject `Config { models_ready, index_dir }`).
- **People (`src/search/faces.rs`, `src/search/step.rs`,
  `src/model/subjects.rs`, `src/ml/face.rs`):** faces are found in the
  helper, in the same read as the search vector, on the chosen device
  only.  Detections, vectors and tiles are cache beside the index (files
  0600); the user's names and answers are `subjects.json` and point at
  detections (`FaceRef`: content key and box in thousandths), never at
  group numbers.  Grouping is `search::step` over a `KindStep`, the same
  code for every kind and **never across kinds** (`Refused::MixedKinds`;
  tests in `subjects.rs`, `faces.rs`, `step.rs`).  A face pass
  (`PassKind::Faces`) starts only from a command, never from a pass's
  end.  Nothing about faces is ever written into photos or sidecars, and
  no face, name or vector goes into the repository.
- **Placeholders, not this machine:** tests and docs use `192.0.2.x`
  (RFC 5737), `example.com`, `/mnt/nas`, `/home/me` and made-up file
  names — never paths, addresses, file names or terminal output copied from
  the user's machine.  New `testdata/` headers follow `testdata/README.md`.
  Run `scripts/check-private.sh` before pushing; it fails on what it finds.

## Testing safely

- Never write to the user's real photo archive.  Its location is in
  `CLAUDE.local.md` (untracked, on the user's machine); if that file is
  missing, ask before writing anywhere outside a scratch folder.  Read-only
  scans are fine.  Test file operations on scratch copies with a throwaway
  `HOME`.
- Headless UI: `kwin_wayland --virtual --socket pp-test --width 1600
  --height 1000 &`, then run with `WAYLAND_DISPLAY=pp-test PP_SCRIPT=<file>`
  (commands at the top of `src/app/testing.rs`; `shot main <png>` takes
  screenshots; `scan-wait` / `analyse-wait` wait for a scan or a pass;
  `search …` drives the search page, with `PP_SEARCH_FAKE=indexed` for a
  library already indexed; `search complete`, `search things` and
  `search fursuits show|hide` print and drive phase 2's pieces).
  Kill the compositor by PID, never `pkill` by pattern.  A script that
  needs hints analyses the folder itself (`cmd Analyse`): don't rely on a
  cache left by an earlier run.
- With a fake `HOME`, also set `CARGO_HOME=~/.cargo` for cargo, or it
  re-downloads crates.
- Search needs the model files (971 MB) in the fake home's
  `.cache/photo_pipeline/models/siglip2-b16-256/`, or `PP_MODEL_DIR`;
  `libwebgpu_dawn.so` beside the binary must be on `LD_LIBRARY_PATH` to
  run the helper by hand.  `PP_ML_FAKE_CARD` fakes a card failure.
