# Image-quality checks for Photo Pipeline (Rust) — feasibility and plan

*Investigation only: nothing in the app was changed. Written 2026-09-30.
Experiments ran in a scratch folder (since deleted; see the appendix) and only
read from `/mnt/nas`.*

---

## Decisions (2026-09-30)

The user answered the open questions in §10.  Where an answer changes the
plan, the change is noted here; the sections below are otherwise unchanged.

| # | Question | Decision | What it changes |
|---|---|---|---|
| 1 | Licence | **Write our own.**  Re-implement the published techniques in Rust; credit cleanvision as inspiration. | Route (c). No code ported. |
| 2 | When to run | **Only when asked.**  An "Analyse this folder" command starts the pass. | The background pass in phase 2 becomes on demand. Bursts from capture times and sequence numbers still show as soon as a folder opens, because they cost nothing. |
| 3 | What is a burst | **Presses, then moments.**  Presses show at once; after an analysis, presses of the same scene merge into a moment with the presses marked inside. | As proposed. Moments exist only after an analysis. |
| 4 | Focus hints | **Both:** a "looks out of focus" mark below a strict cut-off, and a "sharpest" marker per burst. | As proposed. Keep the cut-off conservative, and watch low-light frames. |
| 5 | Bulk actions | **All three:** *Keep this, prune the rest*; *Prune accidental frames…*; *Prune all soft frames…*. | Each asks first, lists what it will mark, and is one undo step. Nothing goes to the Trash before Prune Review. |
| 6 | Stacking | **Bracket, stack on demand.**  Every frame is visible with a bracket and count; a View toggle and a key collapse bursts into stacks. | Stacking moves from a later phase into scope, as a toggle and not the default. |
| 7a | Duplicates: where | **Both:** working folders and the library. | Exact duplicates stay in phase 2. The library side is reached by opening a day as a working folder (see 8). |
| 7b | Which copy to suggest | **Rule of thumb:** a copy-like name first ("copy", "(1)", a "_1" suffix); in the library, the later export (higher sequence number); otherwise the newer file. | The suggestion is shown, never applied without confirmation. |
| 8 | Library calendar | **Neither:** no calendar hints, and no "already in library" warning in Export for now. | Drop the library-calendar phase and the Export check. |
| 9 | Older bodies | **Best effort.**  Use the fallbacks (whole-second times plus file order, sharpest area instead of the focus point). | No dedicated tuning for the A7R II or the Canon T6. |

**Labelling (phase 0) is optional.**  None of the checks learn from data;
labels only measure them and set cut-offs.  Lighter options than labelling
800 frames: use past prune and keep decisions from culled folders' sidecars as
a read-only test set, or start from the cut-offs that worked in the
experiment and adjust them in use.

## 1. Summary and recommendation

**Go, but don't use cleanvision itself.** Re-implement the ideas in Rust, with
changes for photography. cleanvision's checks were tuned for ML training sets.
On a real 320-frame sample from the user's 19-Sep-2026 shoot (Sony ILCE-7M5,
8 fps bursts), its defaults flagged **0 blurry, 0 low-information and
0 duplicate frames**. It found only **5 near-duplicate pairs**. The sample has
91 shutter presses, 67 of them multi-frame, and at least 7 frames that are
clearly out of focus or accidental (checked by eye). cleanvision ranked two of
those 7 among the *sharpest* frames in the sample.

The good news:

- **Bursts can be found exactly, almost for free, from metadata.** Sub-second
  capture times (standard EXIF) and Sony's per-press `SequenceNumber`
  (maker-note tag `0xb04a`) recovered all 91 presses in the sample. That needs
  no pixel work. It reads header bytes the scan already reads.
- **Sharpness ranking inside a burst works on the camera's embedded preview**
  (1616×1080, already extracted by the app). In Rust it costs ≈ 5 ms of CPU per
  shot. A *local* (tile-based) sharpness measure picked out every visibly bad
  frame in the sample. Its conservative absolute threshold flagged 7 frames,
  and all 7 were genuinely out of focus or accidental.
- **Exact duplicates are cheap.** Files are bucketed by size, which the scan
  already knows, and only same-size candidates are hashed.

**What first:** Phase 0 builds a small labelled set and a harness to score
against it (about 1 day). Phase 1 delivers **bursts from metadata and a "Review
burst" mode in the compare view**: a strip of every frame, plus zoom to the
camera's focus point. That is the user's second ask, it is the most reliable
signal, and it needs no pixel analysis. The pixel pass follows in Phase 2: focus
hints, the sharpest frame in each burst, and exact duplicates.

**Licence:** cleanvision is Apache-2.0. Re-implementing these standard,
published techniques independently carries no obligations. The user should
confirm that route (§3).

---

## 2. How cleanvision computes each check

Source: cleanvision **0.3.7** (PyPI, released 2026-01-06; last commit on GitHub
2026-01-08; 37 open issues; lightly maintained). Line numbers refer to the
installed wheel.

### 2.1 Loading (all checks)

`dataset/fsspec_dataset.py:55-59` calls `Image.open(f).copy()`, which is a
**full-resolution decode** with **no EXIF orientation applied**. The duplicate
and property managers each load every image, so each file is decoded twice.
Only Pillow formats are read (`utils/constants.py:22-38`: jpg, png, tiff, webp,
gif, jp2), so **RAW files are ignored**.

### 2.2 Per-check details

| Check | How it is computed | Default rule | Source |
|---|---|---|---|
| **exact_duplicates** | MD5 of the *decoded pixel array* (`np.asarray(image).tobytes()`), not of the file bytes. Images with identical MD5 form a set. | Any set of ≥ 2 | `duplicate_issue_manager.py:26-28, 75-79, 259-264` |
| **near_duplicates** | `imagehash.phash(hash_size=8)`: grey → 32×32 Lanczos → 2-D DCT-II (scipy, unnormalised) → top-left 8×8 incl. DC → bit = coefficient > median → 64-bit hash. Images are grouped by **identical hash string (Hamming distance 0)**, and exact-duplicate sets are removed. Score = 1/set size. | Any set of ≥ 2 | `duplicate_issue_manager.py:35-36, 240-256`; `imagehash/__init__.py:261-281` |
| **blurry** | Resize so the long edge is **64 px** (Pillow default BICUBIC) → grey "L" → `FIND_EDGES` 3×3 kernel (−1…8…−1) → σ_edges = std of the edge image. σ_hist = std of the 256-bin grey histogram *counts*. Score = min(1, (1−e^(−0.01·σ_edges)) + (1−e^(−0.01·σ_hist) if that term > 0.18 else 0)). | Blurry if score < 0.29, i.e. σ_edges < ≈34 when the histogram term is 0 | `image_property.py:236-293`; `constants.py:20` (`MAX_RESOLUTION_FOR_BLURRY_DETECTION = 64`); defaults `image_property_issue_manager.py:66-70` |
| **low_information** | Pillow `Image.entropy()` on the full image. For RGB this is log₂ entropy over the 768 concatenated R, G, B histogram bins (max 9.58; verified against a manual computation). Score = min(1, 0.1·H). | Low if H < 3 bits, i.e. close to a solid colour | `image_property.py:199-233`; defaults `:62-65` |
| dark | Per-pixel brightness √(0.241R²+0.691G²+0.068B²)/255; score = 99th percentile | < 0.32 | `image_property.py:91-164`, `:59` |
| light | 1 − 5th percentile of the same brightness | < 0.05 | same, `:60` |
| grayscale | Pillow mode == "L". A black-and-white JPEG stored as RGB is not caught. | — | `image_property.py:296-341` |
| odd_aspect_ratio | min(w/h, h/w) | < 0.35 | `:167-196`, `:61` |
| odd_size | √(w·h) outside a 3×IQR band of the dataset | score < 0.5 | `:344-405`, `:72` |

### 2.3 Known limitations

- Open issue #120, "Improve blurry issue type": uniformly coloured images,
  high-resolution images and odd-aspect images get "undesirably low blurry
  scores".
- Issue #63 left a TODO open: the near-duplicate score should reflect "how
  similar they are". Today it is all or nothing.
- The blur score's histogram term pushes **dark or flat frames towards "sharp"**.
  That is why the near-black accidental frame in our sample scored 0.725, one of
  the sharpest in the set.
- Blur measured at 64 px can't see focus on a 33 MP frame.

For this app, dark, light, grayscale, odd_aspect_ratio and odd_size add little.
One camera's output is uniformly 3:2 and the same size, and dark or bright
frames are usually intentional at events and night shoots. At most, exposure
percentiles could appear in the EXIF panel. They should not become badges.

---

## 3. Licence findings and the decision needed

| Item | Licence |
|---|---|
| cleanvision | **Apache-2.0** (repo `LICENSE`; no `NOTICE` file at the repo root as of today) |
| imagehash (its pHash) | BSD-2-Clause, © Johannes Buchner |
| Other runtime dependencies | Pillow (MIT-CMU), numpy / pandas / scipy (BSD-3), PyWavelets (MIT), fsspec (BSD-3), tqdm (MPL-2.0/MIT), tabulate (MIT), matplotlib (PSF-style) |
| This app | `Cargo.toml`: `license = "MIT"` |

**(a) Calling cleanvision as a Python subprocess.** There is no licence problem:
it is a separate program. If the app only runs a copy the user installed, there
are no obligations. If it *bundles* one, it must ship the licence texts. The
practical problems rule this out:

- It needs a Python environment of about 350 MB (numpy, scipy, pandas,
  matplotlib).
- It ignores ARW files.
- It decodes full-size frames: **970 ms of CPU per 33 MP JPG** (measured).
- It reads every file twice. That is about 150 GB over the share for 10 000 JPGs.
- Its output is not suited to photography (§4).

**(b) Porting its code.** This is allowed. Apache-2.0 §4 requires:

- a copy of the licence,
- the original copyright and attribution notices kept,
- modified files marked as changed,
- the NOTICE file carried over, if one exists (none does).

Ported files stay Apache-2.0, so the distribution becomes "MIT AND Apache-2.0".
The two are compatible in one binary. Apache-2.0 also adds a patent grant with a
termination clause. There is little to gain: the relevant code is about 40 lines
of arithmetic, and we want different arithmetic anyway.

**(c) Re-implementing the published techniques independently (recommended).**
Every technique involved is standard and published elsewhere:

- variance of the Laplacian (Pech-Pacheco et al., 2000)
- Shannon entropy
- aHash / dHash / pHash (Hacker Factor blog, 2011; Zauner, 2010)
- content hashing

Algorithms and threshold values are not copyrightable expression. We would
calibrate our own thresholds anyway. This route has no obligations; a line in
the README crediting cleanvision as inspiration is a courtesy.

Crates, if wanted:

- `image_hasher` 3.1.1: MIT OR Apache-2.0, updated Feb 2026, ~400k recent
  downloads. It depends on `image` 0.25, which is already in `Cargo.lock` via
  iced, plus `rustdct`, `transpose` and `base64`.
- `img_hash`: unmaintained since 2021. Avoid.

Hand-rolling dHash and pHash takes about 80 lines with no dependencies
(benchmarked below at 0.4 ms). It also keeps the hash definition stable under
our own cache version.

*Not legal advice.* **Decision for the user:** confirm route (c). The answer to
"might the app be distributed?" matters only if they choose (b).

---

## 4. What the experiment showed

### 4.1 Sample

- 320 JPGs, read-only, from `…/19-Sep-2026/JPG`, a convention day with heavy
  continuous shooting. Five windows: 10:42 (120 frames of non-stop 8 fps
  bursts), 13:50, 15:12–15:13, 19:42 (evening, low light), plus scattered
  singles.
- For each file the script read the first 256 KB and then the MPF preview, with
  `fadvise(RANDOM)`, as `exif::jpeg_preview_bytes` does.
- Serial Python took **32 ms and 457 KB per file**. The app's measured
  equivalent is 6.5 ms and 0.46 MB per file (README).
- `exiftool` decoded the saved header bytes.

### 4.2 Metadata

- Every frame on this body carries:
  - `SubSecTimeOriginal` (EXIF `0x9291`)
  - Sony maker-note `ReleaseMode` (`0xb049`, "Continuous" on *every* frame: the
    user always shoots continuous, so this alone means nothing)
  - `SequenceNumber` (`0xb04a`, resets to 1 at each press)
  - `FocusLocation` (`0x2027`: sensor width, height, x, y of the AF point)
  - `FocusFrameSize` (`0x2037`)

  All of these are plain, unencrypted IFD entries; `exiftool -H` showed the tag
  IDs. The ARW in the fixture has the same tags, and so does an older ILCE-7RM2
  ARW. The 7RM2 has no SubSec, and its FocusLocation was simply the image
  centre.
- **Presses:** 91. Lengths: 24×1, 18×2, 17×3, 8×4, 8×5, 4×6, 4×7, 2×8, 1×9,
  2×11, 1×12, 2×13.
- **Timing:**
  - In-press frame gaps: median 0.125 s (8 fps), max 0.166 s.
  - The smallest gap *between* presses is 0.25 s.
  - Grouping by time alone with a gap ≤ 0.2 s reproduces the 91 presses
    exactly.
  - With ≤ 0.5 s, 14 groups span two or more presses.

### 4.3 cleanvision defaults on the 1616-px previews

- 4.9 s wall time for 320 images on 8 processes (15 ms per image).
- Results: **blurry 0, low_information 0, dark 0, light 0, exact_duplicates 0,
  near_duplicates 5 pairs** (all 5 are consecutive frames within one press).
- Blurry scores ranged from 0.31 to 0.79, against a threshold of 0.29.
- Low-information scores ranged from 0.58 to 0.94, against a threshold of 0.30.
- On the 10 full-size 33 MP fixture JPGs, it took 970 ms of CPU per image with
  a single process.

### 4.4 Perceptual hashes (64-bit pHash, Hamming distance)

| Pair type | min | p25 | median | p75 | p90 | max |
|---|---|---|---|---|---|---|
| Consecutive frames in one press (229 pairs) | 0 | 6 | 8 | 12 | 20 | 40 |
| Last frame → first frame of the next press | 6 | 18.5 | 28 | 32 | 34 | 40 |
| Random pairs in the sample | 10 | 27.5 | 30 | 34 | 36 | 46 |

- Exact hash equality, which is cleanvision's rule, caught **5 of 229
  in-press neighbours (2 %)**.
- 34 of 229 in-press neighbours differ by more than 16, because the subject
  walks or the camera pans.
- dHash behaved the same (median 8 in-press, 27 across presses).
- Conclusion: use time and sequence first, and use hashes only to merge
  look-alike neighbouring presses. Merging presses ≤ 2 s apart with
  pHash ≤ 16 gives 91 → 77 groups, the largest 22 frames. Merging with
  pHash ≤ 22 gives 66 groups, the largest 37.

### 4.5 Sharpness on the preview luma (1616×1080)

- **Global Laplacian variance** ranges from 1.1 to 232 across the sample, a
  200× spread driven mostly by content (fur versus floor, background blur, ISO).
  A global absolute threshold won't work.
- **Max tile energy** (8×6 grid, mean squared Laplacian per tile): an absolute
  cut-off of 20 flagged **7 frames, and all 7 are genuinely bad**:
  - a near-black accidental frame
  - a frame blurred by motion while shooting at arm's length
  - five frames where the subject was inside the lens's minimum focus distance

  Between 20 and 40, soft frames mix with acceptable low-light night shots.
  Those would be false positives.
- **Relative to the press median** (< 0.5× the median max-tile): 17 frames. Most
  are visibly soft. A few are explained by the AF point moving onto a different
  texture: in frames 0782–0788 the tracking box moved from the eye to the hair,
  and the focus-window score halved.
- cleanvision's blur score ranked those 7 bad frames at positions
  **1, 4, 9, 13, 149, 315 and 317 of 320** (1 = blurriest). The black frame
  came out as one of the *sharpest*.

### 4.6 Costs (this machine, 8 cores)

| Step | Measured |
|---|---|
| Preview decode, libjpeg-turbo, RGB 1616 | 3.0 ms |
| Preview decode, **luma only**, 1616 | 2.0 ms (½ scale: 1.8 ms; Huffman decoding dominates) |
| Rust, scalar, unoptimised: Laplacian variance + 8×6 tiles @ 1616 | 2.1 ms (0.55 ms @ 808) |
| Rust: dHash + pHash (box-resample from 1616 + 8×8 DCT) | 0.41 ms |
| Rust: 256-bin luma entropy @ 1616 | 0.50 ms |

---

## 5. Photography pitfalls and the adaptations proposed

| cleanvision idea | Where it misleads on real photos | Proposed variant |
|---|---|---|
| Global blur score | **Shallow depth of field is the point** of event and portrait work at f/1.4–2.8. Most of the frame is meant to be soft, so a global score calls good portraits blurry. At 64 px it can't see focus at all. High-ISO noise reads as "sharp". | **Local sharpness**: per-tile Laplacian energy on the preview luma. Use the *max / 90th-percentile tile*, since the in-focus plane is the sharpest area. Add a **focus-point window** from Sony `FocusLocation`. **Rank within a burst** (relative to the burst's best) rather than by absolute threshold. Keep an absolute "looks out of focus" hint only below a conservative, per-camera-calibrated threshold. A light pre-blur or the ½-scale plane damps ISO noise. |
| (same) | **Intentional blur**: panning, slow shutter, ICM, dreamy shots. Motion blur and defocus look alike to one number. | Hints only, never actions. Phase 4: a gradient structure-tensor anisotropy test in the sharpest tiles labels "motion blur" versus "out of focus". EXIF priors help: exposure time against focal length. |
| (same) | The 1616-px preview is ¼ linear resolution. **Subtle misfocus** (eyelashes against the iris at f/1.4) is invisible at that size. | Phase 4, on demand for burst candidates only: decode a full-resolution crop around the AF point (`jpeg_crop_scanline` / `jpeg_skip_scanlines` in libjpeg-turbo) when the user opens Review burst. |
| Focus point | `FocusLocation` is where the camera *tried* to focus. With AF-C tracking the box moves (0782–0788). Older bodies in Wide/Zone report the centre (7RM2 sample). | Use it as a weighted hint next to the max tile. Treat an exact-centre location as weak. Compare windows only within a burst. |
| Low information (entropy < 3 bits over 768 RGB bins) | Effectively never fires on photos. The black accidental frame had 5.7 bits. Real minimal, night, fog or silhouette shots have low entropy *and are good*. | Replace with **"accidental frame"**: (luma entropy low, *or* p99 luma very dark) **and** no sharp tile anywhere. Lens cap, pocket and ground shots match; night skylines with sharp lights don't. |
| Near duplicates = identical pHash | Catches 2 % of burst neighbours. Different scenes in front of the same backdrop can be only 10 bits apart. | **Bursts from metadata first** (SequenceNumber resets, sub-second gaps, ShutterCount continuity on Sony). Then merge neighbouring presses into a "moment" when ≤ 2 s apart **and** pHash ≤ 16. Hashes never split a press. |
| Exact duplicates = MD5 of decoded pixels | Costs a full decode. Misses "same photo, different file": renamed copies, edited metadata, re-encoded JPGs, and re-exports into the library (export *renames* and never overwrites, so re-exporting makes new numbered copies). | Tier 1 **byte-identical**: bucket by (file type, size), then a content hash of candidates only. Tier 2 **same capture**: equal (Model, DateTimeOriginal, SubSec, type) plus pHash ≤ 2. |
| RAW+JPG | A RAW and its JPG are the same capture, not duplicates. | Analyse **once per shot**: the JPG's preview, or for RAW-only shots the RAW's ≥ 1000-px preview via `Raw::thumbnail_at_least`. Results apply to both halves, as marks do (`App::propagate`). Never compare across file types. |
| Bracketing / HDR | Exposure brackets look like near duplicates. pHash is fairly insensitive to brightness. | Group them, but don't call the darker frames "worse". Show "bracket" when exposure compensation varies within a press. |
| Orientation | The preview is stored in sensor orientation. | Metrics are rotation-invariant. Store the orientation; rotate the focus point; hash in sensor orientation, which is consistent within a burst. |

---

## 6. Architecture in this codebase

### 6.1 Two kinds of per-shot data

**ShotMeta** comes from header bytes and is cheap. Fields: SubSec ms, Sony
SequenceNumber / ReleaseMode, FocusLocation (normalised) and FocusFrameSize,
Model id, orientation, and optionally ExposureTime / ISO / FNumber /
FocalLength for priors.

- Read it in the **scan**, from the head bytes `exif::read_capture_time`
  already reads: 72 KB for JPG, which covers APP1 including the maker note;
  64 KB for RAW, read in place via `HeadSource` beyond that.
- Store it next to the capture time in the per-folder capture cache
  (`model/capture_cache.rs`): bump `VERSION` and add fields to `TimeEntry`.
- Keep it **outside** `PhotoRecord`'s dumped fields, so `--dump-scan` parity
  with Python (`main.rs:22`) is unchanged. Don't fold SubSec into
  `capture_time` for the same reason.
- Sony maker note: walk EXIF IFD → `0x927C` → "SONY DSC" header + IFD (offsets
  relative to the TIFF header). Read `0x2027`, `0x2037`, `0xb049` and `0xb04a`
  with the existing bounded `ifd_entries` (`exif.rs:151`). Other makes skip
  this step.

**Features** come from pixels, cost ~5 ms per shot, and are filled in by a
background pass:

- `dhash: u64`, `phash: u64`
- sharpness: global variance, tile p90, tile max, focus-window value (or NaN),
  and the 12×8 tile grid as log-scaled `u16`s (≈ 190 B; useful for "where is
  the sharp area" and later overlays)
- luma entropy, luma p1 / p99
- the decoder used (MPF / RAW preview / none)

### 6.2 New modules (suggested)

| File | Role |
|---|---|
| `src/imaging/quality.rs` | Pure functions on a luma plane: Laplacian tiles, focus window, dHash/pHash, entropy, percentiles. No I/O. Unit-testable. |
| `native/jpeg_shim.c` + `src/imaging/decode.rs` | Add a *luma* output mode (`JCS_GRAYSCALE` from YCbCr) to `pp_jpeg_decode`: 2.0 ms versus 3.0 ms RGB. |
| `src/exif.rs` | `read_shot_meta()` alongside `read_capture_time()`. SubSec (`0x9291`) and the Sony maker-note fields. |
| `src/model/bursts.rs` | Pure grouping: presses → moments → ranking; duplicate tiers. Input: (RecordId, ShotMeta, Option<Features>, size, type). |
| `src/model/quality_cache.rs` | Per-folder Features cache (§6.4), built on `binfile.rs`. |
| `src/analysis.rs` | Background pass, modelled on `scan.rs`: handle with cancel flag, worker pool, batched `BgEvent`s. |
| `src/app/quality.rs` | App-side state: `HashMap<RecordId, Features>`, the current `Bursts`, the run id, progress, and hint and filter state. |

### 6.3 Data flow and threading

1. **Scan.** `scan.rs` → `ScanBatch` (now with ShotMeta) → `on_scan_complete`
   (`app/mod.rs:676`) → `coll.build_pairs()` → **`bursts::presses()` from
   metadata alone**. Bursts are known as soon as the folder is open (Phase 1).
2. **Analysis pass.** Started from `on_scan_complete` when enabled.
   - Jobs: one per *shot*. Use the JPG if paired, else the file, mirroring
     `filter::dedup_pairs`. Order them by the grid's display order, starting at
     the current scroll position, so visible tiles get results first.
   - Each job:
     1. quality cache hit? → done
     2. else `exif::jpeg_preview_bytes` (JPG), or LibRaw `thumbnail_at_least(1000)` (RAW-only)
     3. luma decode
     4. `quality::features`
   - **Pool:** its own `PriorityPool` (`imaging/pool.rs`) with 4 threads. It
     yields while visible thumbnails are queued (e.g. a shared `AtomicUsize`
     that `ThumbWorkers` bumps for `PRIORITY_VISIBLE` jobs), so scrolling never
     waits behind analysis on the NAS.
   - **Events:** `BgEvent::Analysis { run, batch: Arc<Vec<(PathBuf, Features)>> }`
     every 100 results or 250 ms, `BgEvent::AnalysisProgress`, and
     `BgEvent::AnalysisDone { run, complete }`. These go in `bus.rs`, delivered
     in batches like thumbnails.
   - **UI thread per batch:** insert into the map and mark groups dirty.
     Recompute `bursts` (O(n log n) over shots, a few ms at 10 000), debounced
     to ≤ 2 per second. Call `refresh_views()` only when a filter or stacking
     depends on the result; otherwise bump `derived.key` so badges repaint.
     Verify with `PP_PROFILE=1`.
3. **Optional synergy (measure first).** The analysis job already holds the
   preview bytes in memory. It could also write the 300-px thumbnail into
   `DiskCache` (+~1.7 ms), so each NAS file is read once per session instead of
   twice. Off by default.
4. **Lifecycle.**
   - `start_scan` cancels a running pass, as it does the scan.
   - **Sort** moves files into `RAW/` and `JPG/`. The cache key must survive
     that (§6.4), and the in-memory map is keyed by `RecordId`, which Sort keeps.
   - **Trash and its undo:** restored records get new ids (`app/undo.rs`
     rewrites history), so look Features up by path on restore and recompute
     bursts after any collection change.

### 6.4 Caching

- **File:** `~/.cache/photo_pipeline/quality/<md5 of folder>.bin`. It has a
  MAGIC, a format `VERSION` and an **`ALGO` version**; bumping `ALGO` (metric
  maths changed) invalidates every entry.
- **Key:** canonical relative path, with the `RAW`/`JPG` level stripped as
  `util::canonical_parent` does, plus the file name. A Sort therefore doesn't
  force re-analysis; this is the same problem the path-keyed sidecars have in
  `_on_sorted`.
- **Validity:** size + mtime bits, exactly like `TimeEntry::valid_for`
  (`capture_cache.rs:39`).
- **Saving:** unlike the capture cache, also save **after a cancelled or
  partial pass**, since each entry stands alone, and drop entries for files
  that are gone. ~250 B per shot, so ≈ 1.3 MB for 5 000 shots. Saved through
  `fsutil::atomic_write`.
- Nothing goes into the working folder. **No new sidecars.** Suggestions and
  flags are derived data, so the Python app and the shared sidecar formats are
  unaffected.

### 6.5 Cost estimates

Costs are per shot. A 10 000-file RAW+JPG shoot is about 5 000 shots.

| | Local NVMe | NAS (SMB, gigabit) |
|---|---|---|
| Bytes read | ~0.3–0.46 MB | same, over the wire |
| CPU | ~2 ms decode + ~3 ms features (scalar; ≤ 1.5 ms vectorised) | same |
| 5 000 shots, cold | ≈ 25 s CPU → **~7 s on 4 threads** | bandwidth-bound: 1.5–2.3 GB → **~15–25 s** at ~100 MB/s |
| 10 000 shots, cold | ~15 s | ~30–45 s |
| Warm (cached) | < 20 ms to load the cache, < 10 ms to group | same |
| Bursts from metadata | ~0 extra: same head bytes, cached with capture times | ~0 extra |
| Byte-duplicate check | size buckets are free; whole-file hash only for same-size candidates (usually none) | 7.5 MB / 40 MB per candidate file |

The NAS read can be trimmed by ~40 %: read 64 KB first to find the MPF index,
then the ~210 KB preview, instead of the current fixed 256 KB head. Compare
cleanvision on the originals: ~1 s of CPU per file plus two whole-file reads.

---

## 7. UX design

The design follows the README "Look":

- neutral greys around photos
- green and red only for keep and prune decisions
- purple only for selection, focus and the primary action
- one 4 px spacing scale; glyphs drawn in `ui/icons.rs` (no emoji glyphs; see
  the COLRv1 pitfall in the port notes)

### 7.1 The analysis pass

- Runs automatically after a scan when *View ▸ Check focus and bursts* is on (a
  `settings.rs` flag). Also available as `Cmd::Analyse`, "Check focus and
  bursts", in `app/commands.rs`, to re-run it.
- **Progress:**
  - The existing ActivityBar sweep: add `|| app.quality.running()` to `busy` in
    `main_view.rs:90`.
  - The status bar shows "Checking focus · 1 240 of 4 770 shots", left of the
    review progress.
  - At the end it shows "38 bursts · 7 look out of focus · 2 duplicates", with
    a **Show** action. `Status.action` already supports a button.
- Hints appear progressively, visible tiles first. The grid never waits for
  them.

### 7.2 Grid (`ui/grid.rs`)

**Hint plate.** One small plate in the photo's **bottom-left** corner, drawn in
`paint_cell_marks` (already the layer above images). Its glyph is neutral
`PHOTO_INK` on a dark plate; `WARN` (#f9e2af) is used only for "accidental".

| Glyph | Meaning |
|---|---|
| *Soft* | Feathered circle: looks out of focus |
| *Accidental* | Empty frame |
| *Duplicate* | Two offset frames |
| *Sharpest* | Hollow ring: the suggestion |

- Suggestions are **hollow** and decisions are **solid** (the green check and the
  red cross), so a suggestion never reads as a decision.
- Hints hide once a photo is pruned or marked, the same way crop framing hides
  on pruned photos.
- Each glyph has a tooltip, e.g. "Looks out of focus: its sharpest area is 4×
  softer than this burst's best (0757)".
- *View ▸ Show hints* turns them off.

**Burst bracket (Date sort only).**

- A burst is a run of contiguous tiles. A 1-px `PHOTO_INK4` bracket runs
  through the gutter under its mounts and continues across row wraps, with a
  count chip ("13") on the first tile.
- In Name or Size sort, bursts aren't contiguous, so only the chip shows.

**Stack bursts** (*View ▸ Stack bursts*, key **G**; free today).

- Each burst collapses to one tile. The representative is the suggested best,
  else the first frame still to review. The tile gets two offset mount edges
  behind it and the count chip.
- Clicking the chip expands the burst in place, reusing the section-collapse
  animation (`openness`). Enter opens Review burst.
- Implement it as a step in `refresh_views()` after `dedup_pairs`:
  `bursts::stack(&visible, &groups, &expanded)`.
- With **To review**, a fully reviewed burst disappears, and a partly reviewed
  one shows its best remaining frame. Progress still counts shots
  (`filter::progress` is untouched).

### 7.3 Filters

- Add a **Hints ▾** control to the filter bar. Its choices are *All photos*,
  *Possible problems* (soft, accidental, duplicate), *Duplicates* and
  *In bursts*, each with a count.
- It is **independent of** All · To review · Marked, so *To review + Possible
  problems* becomes a fast triage pile for P.
- `FilterState::accept` sees only the record, so pass a predicate over the
  quality map into `visible_sorted` instead of adding fields to `PhotoRecord`.

### 7.4 Review burst (`app/compare.rs`, `ui/compare_stage.rs`, `ui/filmstrip.rs`)

**Entry.**

- **B** on any frame of a burst, in the grid, the list or quick look.
- **C** with a single stack selected.
- The status action and quick look's info line.

Today, `can_open` / `open_selected` require 2–4 selected photos.

**Layout.**

- The compare stage stays as it is, with `MAX_PANES` = 4, the most that stays
  readable at 1:1.
- Below it, a **burst strip** reuses the `ui/filmstrip.rs` widget, which
  already draws marks and crops from the thumbnail caches, glides, and
  click-to-jumps.
- Each strip cell gets a thin sharpness bar (relative to the burst's best) and
  tick marks between presses.
- Bursts of 10–30 frames live in the strip, with 2–4 candidates in the panes.

**Initial panes.**

- A burst of ≤ 4 frames shows all of them.
- A larger one shows the top 4 by sharpness, in time order, with focus on the
  sharpest.
- Before the pixel pass has run (Phase 1), it shows the first 4 frames.

**Keys.**

- Arrows and Tab still move the purple outline between panes.
- **`,` / `.`** (or Shift+←/→) swap the focused pane's photo for the
  previous / next frame in the burst.
- Clicking a strip cell puts that frame into the focused pane.
- P / M / U mark it, as today.
- **`1` = 100 % at each photo's own AF point** (`FocusLocation`), with zoom
  still linked. Checking eyes across a burst becomes one key press instead of
  panning.
- Enter opens the viewer; Esc goes back.

**Explicit bulk action.** *Keep this, prune the rest* (Shift+M):

- marks the focused frame M, and every *still-unreviewed* frame of the burst P;
- never touches frames the user already decided;
- needs a combined undo step: `app/undo.rs` records one `Field` per step today,
  so add a small `Kind::Batch` so one Ctrl+Z reverts both.

**State.** Add `burst: Option<BurstView { frames, strip_scroll }>` to `Compare`.
`on_removed` already handles frames leaving.

**Option (Phase 4).** An *elimination* mode: two panes, and pruning one brings
the next frame in. This suits very long bursts.

### 7.5 Suggestions are suggestions

- Nothing is pruned, marked or moved automatically.
- Hints are never written to sidecars.
- Every bulk action is a named command with a count, and undo covers it.
- A possible later command, *Prune all accidental frames…*, should show a
  confirmation with the count. It is a question for the user (§10).

### 7.6 Quick look, viewer and list

- **Quick look** (`app/quicklook.rs` / `ui/quick_look.rs`): add an info line,
  e.g. "Burst · 4 of 13 · sharpest is 0757 · looks soft", and **B** opens
  Review burst.
- **Viewer:**
  - the EXIF panel (`exif_panel.rs`) gains a *Focus* section: relative
    sharpness, AF point and mode, and press/sequence number;
  - an overlay toggle draws the camera's AF box on the image;
  - the filmstrip shows burst brackets.
- **List:** a narrow hint-glyph column (icons, not emoji).

### 7.7 Library calendar (read-only)

- There is no automatic pass over the library (110 k files).
- **Phase 4, optional:**
  - Analyse a day when it is opened, cached per day folder.
  - Show hints and burst brackets in the read-only day grid, and allow Review
    burst read-only. P/M are already ignored in read-only views.
  - In the **Export dialog**, add an "already in the library" check by capture
    key, which catches accidental re-exports.

### 7.8 Network shares

- Never touch the UI thread. Open files through `exif::open_for_header` so
  read-ahead stays off; watch `/proc/fs/cifs/Stats` "Bytes read" in the bench.
- On read failures, classify with `ops::netfs::classify_error`. Pause the pass
  behind the existing banner (`app/network.rs`) and resume from the cache when
  the probe succeeds.
- Cap concurrency so visible thumbnails always win.

---

## 8. Phased rollout

Rough effort, in focused working days for someone who already knows the
codebase.

| Phase | Content | Effort |
|---|---|---|
| **0. Labels + harness** | Scratch copies of 3–4 shoots (~800 frames): this 19-Sep sample, an indoor/night event, an f/1.4 portrait session, something static. The user labels them **in the app itself** on the copies: P = soft/accidental, M = best of each burst. The sidecars *are* the labels. Plus an ignored test `quality::eval` that prints precision/recall and top-1/top-2 agreement. | 1 d + ~1 h of the user's labelling |
| **1. Bursts from metadata + Review burst** | `read_shot_meta` (SubSec + Sony maker note) into capture cache v2; `model/bursts.rs` presses (time + sequence); grid bracket + count chip; **B → Review burst** with strip and AF-point zoom. No pixel analysis yet. | 2.5–3 d |
| **2. Analysis pass** | Luma decode mode; `imaging/quality.rs`; `analysis.rs` + `BgEvent`s + quality cache; hint plates; *sharpest in burst* suggestion; *looks out of focus* and *accidental* (thresholds from Phase 0); byte-identical duplicates; Hints filter; status/progress. | 3–4 d |
| **3. Polish** | Stack bursts (G); pHash "moments" merging presses; same-capture duplicates + which copy to suggest pruning; *Keep this, prune the rest* + `Kind::Batch` undo; quick look/viewer/list hints; bench + scripted UI tests. | 2–3 d |
| **4. Optional** | Full-resolution AF-point crop check on demand; motion-versus-defocus label; elimination mode; library-day hints; export "already in library" check. | 3+ d |

**Note on ordering.** The user listed exact duplicates first. In a
card-to-folder workflow they are rare, though re-exports into the library can
produce them. They are cheap, so they ride along in Phase 2 rather than leading.
Ask the user whether they've actually hit duplicates (§10).

---

## 9. Testing

- **Unit tests (pure, no files):**
  - `quality.rs`:
    - a checkerboard should score sharper than its box-blurred copy, and the
      score should fall monotonically with blur radius;
    - simulated shallow depth of field (blurred image, one sharp tile): tile-max
      flags it as sharp while the global score does not;
    - entropy of a uniform image = 0, of a two-level image = 1;
    - the pHash/dHash of synthetic gradients is stable, and Hamming distance
      works.
  - `bursts.rs`: sequence resets, sub-second gaps, missing SubSec (7RM2-style),
    mixed bodies, RAW+JPG counted once, groups surviving `update_path`
    (Sort), and duplicate tiers that never pair a RAW with its JPG.
  - `quality_cache.rs`: round-trip, damage rejection, and validity on
    size/mtime, as in `capture_cache.rs` tests; a Sort keeps entries valid.
  - `exif.rs`: maker-note parsing on saved header fixtures (copy 2–3 heads from
    `/tmp/scratch/agents/cleanvision/sample/head/` into test data; ~128 KB
    each), with truncated and corrupt variants.
- **Labelled evaluation (Phase 0 set):**
  - "Looks out of focus" targets **precision ≥ 90 %**, because false flags erode
    trust; report recall.
  - "Sharpest in burst" agrees with the user's M pick (top-1 / top-2).
  - Press grouping is exact versus Sony SequenceNumber; moment grouping is
    reported per threshold.
  - Thresholds are tuned per camera model. Unknown models get relative-only
    hints.
- **Fixture integration:**
  - Use a scratch copy of `/tmp/scratch/fixture/shoot1` with a
    byte-identical copy (`cp`), a renamed copy, and a re-saved JPG.
  - Expected result: tier-1 duplicates, tier-2 duplicates, and the pair never
    flagged.
- **Bench:** extend `bench.rs` with `bench_analysis`. Time cold and warm, local
  and NAS (scan-only style), and measure CIFS bytes. It never writes to the
  library; it writes only its own cache.
- **Scripted UI** (`PP_SCRIPT`, headless kwin, scratch HOME): open the
  fixture, wait for analysis, take a `shot` of the badges, press `key b` for
  Review burst, step `.`, press `1` for AF-point zoom, `shot`, P/M, Ctrl+Z.
- **Profiling:** `PP_PROFILE=1` while results stream in for 10 000 records. No
  update or view should exceed one frame.

---

## 10. Risks and open questions for the user

### Risks

- **False "soft" flags** are the main danger: they would teach the user to
  ignore hints. Mitigations: relative-in-burst first, conservative absolute
  thresholds, hint-only, and a per-camera calibration set. Low-light shots in
  the 20–40 band were the false positives in this sample.
- **Absolute thresholds depend on camera and preview** (size, in-camera
  sharpening). They are calibrated per Model. Other bodies get relative hints
  only until they are labelled.
- **NAS load during culling.** It adds ~0.3–0.46 MB per shot once; the cache
  makes later opens free. Visible thumbnails take priority.
- **Grid clutter versus the "Look".** One plate, neutral, hidden once a photo
  is decided, and with a toggle.
- **Compare complexity.** The strip reuses `filmstrip.rs`, and the pane logic
  stays as it is.
- **Undo for combined actions** needs a small `Kind::Batch`.
- **Parity.** Nothing is written to shared sidecars, the caches are Rust-only,
  and `--dump-scan` output is unchanged.

### Questions

1. **Licence:** OK to re-implement independently, i.e. route (c), with
   cleanvision credited as inspiration?
2. **When to run:** automatically after every scan (including NAS folders, about
   15–25 s per 5 000 shots the first time), or only on command?
3. **What is a "burst"** to you: one shutter press, which is exact from
   metadata, or a "moment" of several presses of the same scene seconds apart,
   which needs a hash heuristic? Proposal: moments in the UI, with press ticks
   inside.
4. **Hints:** show "looks out of focus" on every frame, or only "sharpest in
   burst"?
5. **Bulk action:** should *Keep this, prune the rest* exist? And a
   *Prune all accidental frames…* command (with confirmation)?
6. **Stacking:** stack bursts in the grid by default, or keep the bracket only?
7. **Duplicates:** have you actually hit them? In the working folder, in the
   library from re-exports, or both? Which copy should be suggested for pruning
   (a name like "copy" or "(1)", the newer mtime, the deeper path)?
8. **Library calendar:** do you want read-only hints there, and an "already in
   library" warning in Export?
9. **Older bodies** (7RM2: no SubSec; Canon T6: SubSec but no Sony maker note):
   still important? They fall back to whole-second times plus filename
   sequence.

---

## Appendix — scratch artefacts (no longer available)

These lived under `/tmp/scratch/agents/cleanvision/` and were deleted in a
cleanup on 2026-09-30, so the numbers above can't be re-run from them.  The
list records what was used.  Rebuilding them means re-extracting previews
read-only from the 19-Sep-2026 day and re-running the steps described in §4.

| Path | What it is |
|---|---|
| `venv/` | cleanvision 0.3.7, ImageHash 4.3.2, Pillow 12.3, scipy |
| `extract.py` | Read-only MPF preview + 128 KB header extraction (320 files) from the NAS day |
| `sample/prev/`, `sample/head/`, `meta.csv` | The extracted previews and headers, and the `exiftool` metadata CSV |
| `run_cv.py` | cleanvision run; outputs `cv_prev.csv` (+ `.sets.json`) and `cv_full.csv` |
| `bursts_meta.py` | Burst analysis; outputs `bursts_meta.json` |
| `proto.py` | Hash and sharpness prototypes; outputs `proto.json` |
| `outliers.py`, `sheet.py` | Contact sheets: `sheet_0752.jpg`, `sheet_0776.jpg`, `sheet_outliers.jpg`, `sheet_low2.jpg` |
| `rsbench/` | Throwaway Rust cost benchmark on luma PGMs (`pgm/`) |
