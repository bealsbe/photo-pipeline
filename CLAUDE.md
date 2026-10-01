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
- **Commands** live once in `src/app/commands.rs` (label, shortcut, icon, menu
  placement); a shortcut is bound exactly once.  The KDE global menu mirrors
  `MENUS`.
- **Look:** tokens in `src/ui/theme.rs` — Catppuccin chrome, neutral
  `PHOTO_*` greys around photos (never tint those).  Sentence-case copy, plain
  verbs, no ALL-CAPS labels.  Shared view pieces in `src/app/kit.rs` and
  `style.rs`; icons are drawn in code (`src/ui/icons.rs`).
- **iced quirks:** within a layer, images draw above quads — overlays on
  images need `renderer.with_layer`; nested layers aren't clipped by parents.

## Testing safely

- Never write to `/mnt/nas` (the user's real archive).  Read-only scans are
  fine.  Test file operations on scratch copies with a throwaway `HOME`.
- Headless UI: `kwin_wayland --virtual --socket pp-test --width 1600
  --height 1000 &`, then run with `WAYLAND_DISPLAY=pp-test PP_SCRIPT=<file>`
  (commands at the top of `src/app/testing.rs`; `shot main <png>` takes
  screenshots).  Kill the compositor by PID, never `pkill` by pattern.
- With a fake `HOME`, also set `CARGO_HOME=~/.cargo` for cargo, or it
  re-downloads crates.
