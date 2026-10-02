# Library search and subjects for Photo Pipeline — feasibility and plan

*Investigation only: nothing in the app was changed. Written 2026-10-01.
Experiments ran in a scratch folder (kept, with its contact sheets and
numbers; see the appendix) and only read from the photo archive.*

---

**Evaluation samples.**  The measurements below come from the user's own
library, named once here:

- **Sample L:** 1 010 JPGs spread over every day folder of the library
  (at least 4 per folder, more for big days).  Mostly event photos, plus
  home, travel, nature and night shots.
- **Sample E1:** an event day, 369 consecutive frames: many fursuits,
  attendees, some cameras.
- **Sample E2:** another event day (the library's largest), 400 frames
  spread through the day.

Search was tested on all three together (1 779 photos).  The library
itself: 110 162 photo files (53 480 JPG, 56 682 RAW) in 56 710 shots:
53 452 RAW+JPG pairs, 3 230 RAW-only shots (older bodies), 28 JPG-only.

## 1. Summary and recommendation

**Go for search; go step by step for subjects.**

- **Free-text search works on the embedded previews, today, on the CPU.**
  A joint image–text model (CLIP family) found cameras, cars, food,
  fireworks and fursuits by plain words.  SigLIP2 ViT-B/16 was the best of
  three models tried: 7 of its top 10 for "camera" held a camera, 4 for
  "car" (the sample has few cars), 7 for "food", 24 of 24 for "fursuit".
  Indexing the whole library reads each shot's preview once (about
  0.35 MB, 6–7 minutes for the library over the share) and costs 5–47 ms of
  CPU per shot depending on the model, or about 1–4 ms on this machine's
  GPU.  A query takes ~14 ms to encode and ~3 ms to compare against every
  shot: no vector database is needed.
- **This library needs one twist: "dog" finds fursuits.**  Both CLIP and
  SigLIP2 answer "dog" and "bird" with canine and avian fursuits; the
  sample holds one real dog.  Contrasting the query with "fursuit" fixes
  it: the real dog comes first (p = 1.00), a dog statue second, and
  everything else drops away.  The query syntax gets a minus (`dog
  -fursuit`), and the results page offers it when a query looks like an
  animal.
- **Hidden tags are worth having, but not as the search.**  Tags derived
  from a fixed vocabulary are precise (24 of 24 "fireworks", 3 of 3
  "camera") but name only a photo's main subject: search found more than
  twice as many cameras.  They cost nothing extra: they are computed from
  the stored embeddings in milliseconds.  Use them for suggestions, a
  ranking boost, the "Things" row and to choose which photos the head
  detector runs on; never as labels on photos.
- **People: feasible with licence-clean models.**  Immich's face models
  (InsightFace) are for non-commercial research only, so they can't be
  the default in this public, MIT-licensed app.  YuNet (MIT) detects faces
  in 3 ms per preview at half size; AuraFace (Apache-2.0, an ArcFace
  ResNet-100) grouped the sample's faces with no visibly mixed group of
  three or more, once blurry faces were left out.  SFace (Apache-2.0) is
  8× cheaper and mixed two of eight groups.
- **Fursuits: promising, semi-automatic, GPU-bound.**  No face model sees a
  fursuit.  An open-vocabulary detector (OWLv2, Apache-2.0) asked for "a
  fursuit head" boxed most heads; DINOv2 (Apache-2.0) embeddings of the
  head crops grouped the same suit across bursts: 32 of 34 neighbours
  closer than a strict cut-off were the same character.  But automatic
  grouping alone left 7 of the 16 largest groups mixed, and the detector
  costs 1.3 s per photo on the CPU (15 hours for the library's fursuit
  photos) against 0.12 s on the GPU.  So: the user names a character from
  one group, then confirms suggestions with Y and N, the way P and M cull.
- **Pets** ride on the character machinery later; for now search covers
  them ("dog -fursuit").
- **Runtime: `ort` (ONNX Runtime) from Rust.**  Measured from a scratch
  crate: CPU results equal to Python's, +23 MB to the binary.  Its
  prebuilt WebGPU provider runs on this AMD card through Vulkan (no ROCm
  needed): CLIP 1.2 ms, SigLIP2 4.4 ms, DINOv2 4.2 ms per image, with
  outputs identical to the CPU's.  It is marked experimental, so it runs
  in a helper process the app starts, with the CPU as the fallback.

**What first:** phase 0 builds a small labelled set and an evaluation
harness (about 1 day).  Phase 1 delivers search in the library calendar:
the index, the model download, Ctrl+F, results as the calendar's
read-only grid grouped by day.  Phase 2 adds hidden tags and the query
syntax.  Phase 3 is people and the Subjects view.  Phase 4 is
characters, after its own evaluation.  §13 lists the decisions to make.

---

## 2. What Immich does, and what it leaves out

Immich (the self-hosted photo server the user pointed to) runs its machine
learning in a **separate Python service** (`immich-machine-learning`, a
container) that serves ONNX Runtime models downloaded from Hugging Face
and unloads them after an idle time (`MACHINE_LEARNING_MODEL_TTL`).  The
main server stores vectors in Postgres with the VectorChord extension.

| Feature | Immich | What it means here |
|---|---|---|
| Smart search | CLIP image and text towers; default `ViT-B-32__openai`; its model table lists SigLIP2 models (up to `ViT-SO400M-16-SigLIP2-384`) as higher quality, and multilingual ones | The same idea fits: one embedding per shot, a text embedding per query.  The model choice is decision 2. |
| Faces | InsightFace `buffalo_l` (SCRFD detection + ArcFace ResNet-50 recognition); defaults: min detection score 0.7, max recognition distance 0.5, at least 3 faces to make a person; DBSCAN-like clustering, incremental per upload plus a nightly pass for leftovers; name, merge, hide, change the cover | The workflow carries over.  The models don't: InsightFace's model zoo says "ALL models are available for non-commercial research purposes only." |
| Search filters | People, CLIP text, file name, folder, tags, camera, dates, OCR, places | Dates and subject names are the useful ones here (§7.2). |
| Pets | **Not supported.** Maintainers: no good free model and no training data; "describe your pet in the search" instead.  One of the most requested features. | The fursuit pipeline below is the same problem; this app can try it because its subjects are few and the user confirms each group. |
| GPU | CUDA, ROCm (now MIGraphX, an image needing 35 GiB, compiling models on first use), OpenVINO, ARM NN, RKNN | A desktop app can't ask for that.  ONNX Runtime's WebGPU provider (§4.2) reaches this AMD card without ROCm. |

**What Immich deliberately doesn't do, and why it matters:** no pets (no
model), no tags shown from CLIP (search only), no hand-made vocabulary.
Its ML is a server; ours must stay inside a desktop app that is culling at
the same time, over a network share.

---

## 3. What the experiments showed

All in `<scratch>/agents/mlsearch/` (appendix).  Previews were read as
the app reads them: the head with read-ahead off, then the MPF preview
(~1616 px).  Cold, over the share, 8 threads: **6.7–6.9 ms per file wall,
0.34–0.6 MB per file**.  50 of the 1 010 library JPGs (older bodies) have no
MPF preview and were decoded whole at a DCT scale.

### 3.1 Free-text search

Three models, each image embedded once from its preview, each query as
"a photo of a *query*.".  Precision at 10 by eye (a hit shows the thing
clearly; the sample holds only about 7 cameras and 5 cars):

| Query | CLIP ViT-B/32 (OpenAI; Immich's default) | SigLIP2 ViT-B/16-256 | PE-Core B/16 (Meta) |
|---|---|---|---|
| camera | 6 | **7** | 6 |
| car | 3 | **4** | 2 |
| food | 4 | **7** | — |
| fursuit | 10 (24 of 24) | 10 (24 of 24) | — |
| dog (real dogs) | 1 | 1 | — |
| bird (real birds) | 2–3 | 2–3 | — |

- **What the model sees matters.**  SigLIP2 and PE squash the 3:2
  preview to a square; CLIP B/32 crops the centre square, losing a third
  of the frame.  SigLIP2 found cars parked behind people; the other two
  did not.
- **Empty frames pollute weak queries.**  Lens-cap, black and grey frames
  sit near every text prompt: 6 of CLIP B/32's top 16 for "camera" were
  near-empty, 5 of PE's, 3 of SigLIP2's.  Fix: leave out frames the
  analysis calls accidental (its luma test is cheap and can run in the
  same pass), and cut results off by score (§7.3).
- **"dog" means canine fursuits in this library.**  Both models' top 24
  for "dog" were fursuits except one real dog; "bird" the same with avian
  fursuits.  Rewording ("a real dog, a pet animal") didn't help.  Two
  things did:
  - a softmax over a few labels {dog, fursuit, person, bird, cat}: the
    real dog p = 1.00, a dog statue 0.95, nothing else above 0.5 (2–4
    photos in all, by model);
  - `dog -fursuit`: rank by "dog" only where "dog" beats "fursuit":
    the real dog first, the statue third.

  Contact sheets: `sheets/q_*`, `d_*`, `neg_*`.
- **Thumbnails as input.**  Embeddings from the app's 300-px disk
  thumbnails agree with the preview's (cosine median 0.94–0.97), and 17–23
  of each query's top 24 are the same.  Close, not equal: index from one
  source only (the preview), so results don't depend on what the user
  happened to view.

### 3.2 Hidden tags against search

A 91-label vocabulary (people, costumes, animals, things, places, kinds of
photo), a softmax over it per photo, tags = labels with a share ≥ 0.2
(at most three).  Computed from the stored embeddings: 2–10 ms for 1 779
photos, so the library's tags take well under a second and can be
recomputed whenever the vocabulary changes.

| Tag | Photos tagged | Right by eye | Search, top 10 |
|---|---|---|---|
| fireworks | 24 | 24 | — |
| camera | 3 | 3 | 7 |
| car | 1–3 | — | 3–4 |
| fursuit or person in a fursuit | 74 % of sample L | — | — |

Tags are **precise but name the main subject**: a person holding a camera
is "person" first.  Search ranks every photo, so it finds the camera in
the corner.  Hence decision 8: tags suggest and boost, search finds.

### 3.3 People

YuNet (OpenCV Zoo, MIT) on sample L: 371 faces of at least 40 px in 233 of
1 010 photos at score ≥ 0.8; 145 at score ≥ 0.9.  Faces aligned by its
five landmarks (the ArcFace template), embedded, grouped by average-linkage
clustering.  Contact sheets: `sheets/faces_*`, `faces2_*`.

| | SFace (OpenCV Zoo, Apache-2.0) | AuraFace v1 (Apache-2.0, ResNet-100) |
|---|---|---|
| Size, CPU cost | 37 MB, 3–4 ms per face | 261 MB, 30 ms per face |
| No quality gate (score ≥ 0.8), cut-off 0.55 | 4 of the 12 largest groups were blurry faces of different people | — |
| Score ≥ 0.9, cut-off 0.5 / 0.6 | 8 groups of ≥ 3 faces (42 faces); 2 mix two people | 7 groups (38 faces); none visibly mixed |
| Published accuracy | 99.40 % (OpenCV Zoo's own evaluation) | LFW 99.65 %, CFP-FP 95.2 %, AgeDB 96.1 % |

Both put young children in one group of 12 that may hold two children.  A
quality gate (detection score, face size, blur) matters more than the
model: Immich's 0.7 minimum score exists for the same reason.  YuNet costs
15.7 ms per full preview and **3.2 ms at half size**, which kept 29 of 30
faces of 40 px or more.

### 3.4 Fursuits: finding heads

No face detector is trained on costume heads (YuNet found a few muzzles by
accident).  Three open-vocabulary detectors, asked for "a fursuit head",
"a human face", "a dog", "a cat", "a camera", on sample E1:

| Detector | Licence | GPU time (PyTorch, batch 1) | What it did |
|---|---|---|---|
| OWLv2 B/16 | Apache-2.0 | 242 ms fp32, 120 ms fp16, + 32 ms preprocessing; CPU 1 305 ms | Boxed most heads at score ≥ 0.3; missed some profiles and very close heads.  Few human faces. |
| Grounding DINO tiny | Apache-2.0 | ~337 ms with preprocessing | Called fursuit muzzles "human face"; boxed whole bodies as heads. |
| OWL-ViT B/32 | Apache-2.0 | ~61 ms with preprocessing | Five times faster; overlapping body, head and face boxes need more filtering. |

OWLv2 gave 1 239 head crops (at least 6 % of the long edge, ~100 px) from
653 of the 769 event-day photos.  YOLO-family detectors would be faster
but have no "fursuit head" class (training one is out of scope), and
Ultralytics' models are AGPL-3.0.  Sheets: `sheets/det_*`.

### 3.5 Fursuits: grouping the same character

Head crops embedded with DINOv2-B (CLS and mean patch token) and with
SigLIP2's image tower, then average-linkage clustering by cosine distance.
Sheets: `sheets/subj_*` (one row per group, E/D = which day).

| Embedding, cut-off | Groups of ≥ 3 | Crops in them | Of the 16 largest groups |
|---|---|---|---|
| DINOv2-B, 0.25 | 93 | 64 % | 9 one character each (one joined across both days), 7 mixed |
| DINOv2-B, 0.35 | 49 | 85 % | one group of 377 mixed crops: too loose |
| SigLIP2, 0.15 | 78 | 81 % | 7 one character, 2 nearly (8–9 of 10), 7 mixed |

SigLIP2 places every fursuit head close together ("a fursuit head");
DINOv2 separates individual suits better.  Mixed groups were small,
blurred, back-of-head or partly hidden heads, and two characters often
photographed together.

**"More like this"** is the test that matches the UX (name one, find the
rest).  From 8 seed heads, the nearest crops taken at least 2 minutes
away (so not the seed's own burst), DINOv2-B (`sheets/more_*`):

- **32 of 34 neighbours closer than 0.20 were the same suit**: 12 of 12
  and 12 of 12 for two seeds, 6 of 6 and 2 of 2 for two more (their suits
  were photographed again 6 and 3 times, so most were found);
- 4 seeds' suits appear in one burst only.  For 3 of them nothing came
  closer than 0.24, so a cut-off says "no more" correctly; the fourth had
  the two wrong neighbours, at 0.17 and 0.19 (similar dark suits);
- beyond about 0.22 most neighbours were other suits.

**Uncertain:** matching across days and costume changes.  The samples
rarely hold the same suit on both days (one character was joined across
them); a suit with a different outfit, a partial suit, or a head seen from
behind will often fail.  This is why characters stay semi-automatic, and
why phase 4 starts with its own labelled evaluation.

### 3.6 Pets

One real dog in 1 779 photos.  Search with the fursuit contrast finds it;
detection and grouping would work as for fursuits (OWLv2 "a dog", "a
cat"; DINOv2 crops), but there is too little to test here.  Decision 12.

### 3.7 Throughput

**This machine:** AMD Ryzen 7 9800X3D (8 cores, AVX-512), 60 GB RAM; AMD
Radeon RX 7900 XTX (Navi 31, gfx1100, 24 GB) plus the CPU's integrated
GPU; Mesa RADV (Vulkan 1.4); ROCm 7.1 runtime, MIGraphX 7.1 and MIOpen
from the distribution; no NVIDIA.

Per image, from scratch Rust crates on `ort` 2.0.0-rc.13 (ONNX Runtime
1.28, prebuilt static library), batch 16:

| Model | CPU, 8 threads | CPU, 4 threads | GPU (WebGPU on Vulkan) |
|---|---|---|---|
| CLIP ViT-B/32, 224 px | 5.4 ms | 9.0 ms | 1.2 ms |
| SigLIP2 ViT-B/16, 256 px | 32 ms | 47 ms (Python ORT) | 4.4 ms |
| DINOv2-B, 224 px | 31 ms | — | 4.2 ms |
| DINOv2-S, 224 px | 8.2 ms | — | — |
| Text query: CLIP B/32, SigLIP2 | 7.4 ms, 13.7 ms (4 threads) | | |
| YuNet (OpenCV), half preview | 3.2 ms | | |
| SFace, AuraFace, per face | 3–4 ms, 30 ms | | |
| OWLv2 B/16 (PyTorch) | 1 305 ms | | 120 ms fp16 (ROCm) |

The WebGPU outputs equal the CPU's (cosine 1.000000, largest difference
2.7e-5).  Decoding a preview at ¼ scale with libjpeg-turbo, enough for
these inputs, is ~1 ms.

**The whole library, first time** (56 710 shots; reading and inference
overlap, so the slower one decides):

| What | CPU (4 threads) | GPU |
|---|---|---|
| Reading previews (bandwidth-bound) | ~6–7 min | ~6–7 min |
| Search, CLIP B/32 | ~9 min | ~7 min (reading) |
| Search, SigLIP2 B/16 | ~45 min (30 min on 8 threads) | ~7 min (reading) |
| People: YuNet + AuraFace (8–21 k faces) | +7–14 min | ~+3 min (estimate) |
| People: YuNet + SFace | +4 min | +3 min |
| Characters: OWLv2 on the ~42 k fursuit photos | ~15 h | ~1.4 h (ROCm fp16) |
| Characters: DINOv2-B on ~67 k head crops | ~35 min | ~5 min |

**Afterwards, per exported 5 000-shot day:** ~35 s of reading, then
~45 s (CLIP B/32, CPU), ~4 min (SigLIP2, CPU) or ~35 s (either, GPU).

---

## 4. Running it from Rust

### 4.1 Runtimes

| Runtime | State (2026) | GPU on this machine | Fit |
|---|---|---|---|
| **`ort`** (ONNX Runtime 1.28) | 2.0.0-rc.13, MIT OR Apache-2.0, the de facto Rust binding to the runtime Immich uses from Python | Prebuilt **WebGPU** (Vulkan, experimental, measured above); MIGraphX needs ONNX Runtime built from source against ROCm; the ROCm provider was removed upstream in 1.23 | **Recommended.**  Any model with an ONNX export; CPU numbers equal Python's. |
| `candle` (Hugging Face) | Pure-Rust, CUDA and Metal | ROCm only in unmerged pull requests | CLIP and DINOv2 exist as candle code, but no AMD GPU. |
| `burn` | 0.21; wgpu backend (Vulkan, Metal, DX12); `burn-onnx` turns ONNX into Rust code | Yes, through Vulkan | Pure Rust and attractive, but every model is code to generate and check; a bigger lift per model. |
| `tract` (Sonos) | Pure-Rust, CPU only | No | Simple build; not measured; transformer speed unknown. |

**Build cost of `ort`:** the build downloads a static ONNX Runtime (105 MB
CPU, 122 MB with WebGPU) into `~/.cache/ort.pyke.io` once; a release binary
that only runs a model is 28 MB (23 MB stripped), so the app grows by
about 23 MB.  The WebGPU build adds a 12 MB `libwebgpu_dawn.so` beside the
binary.  It needs `features = ["download-binaries", "tls-rustls", …]` (the
build fails without a TLS feature).  `ort` is still a release candidate;
pin the exact version.

### 4.2 GPU options

1. **CPU only.**  Mature; SigLIP2's first index ~45 min on 4 threads.
2. **WebGPU provider** (prebuilt, Vulkan through Dawn): 5–7× faster than 8
   CPU threads for these models, identical results, no ROCm.  Marked
   experimental by `ort`; a driver fault could crash the process running
   it.  Hence the helper process (4.3).
3. **MIGraphX** (AMD's ONNX Runtime provider): build ONNX Runtime from
   source against the distribution's ROCm; Immich's ROCm image is 35 GiB
   and compiles models on first use.  Not worth it for a desktop app.

### 4.3 In-process threads or a helper process

Immich needs a service because it is a server.  A desktop app doesn't, but
a helper still earns its keep:

| | Worker threads in the app | Helper process (`photo-pipeline --ml-worker`) |
|---|---|---|
| Plumbing | Least: a pool like `analysis.rs`, `BgEvent`s | A pipe protocol (length-prefixed messages), a reader thread turning replies into `BgEvent`s, ~300 lines |
| A GPU driver fault | Takes the app down, with marks still queued for the saver | Kills the helper; the app says so and carries on on the CPU |
| Memory (models ~0.3–0.8 GB) | Held until the app frees the sessions | Returned to the system when the helper exits after a few idle minutes |
| Priority | Thread niceness | `nice` and idle I/O class for the whole helper |
| Cancel | Cooperative | Cooperative, or kill |

The helper is the same binary started with a flag, so there is nothing
extra to install.  It reads previews itself (the app sends paths and
receives embeddings), so image bytes don't cross the pipe.  Decision 4.

### 4.4 Shipping models

- **Never in the repository.**  Downloaded on first use, after a dialog
  naming the model, its size and licence: "Search needs a 750 MB model.
  Download it now?"
- **Pinned:** each model has a URL, a size and a SHA-256 in
  `src/ml/models.rs`; a download goes to a temporary file, is checked, then
  renamed into `~/.cache/photo_pipeline/models/` (re-downloadable, so a
  cache).  A mismatch deletes it and says so.
- **Sources:** where the publisher has no ONNX file, the choice is a
  maintained export on Hugging Face (Immich's `immich-app/…` for the CLIP
  models; community exports made for transformers.js for others) or our
  own exports attached to this repository's releases with the model's
  licence and NOTICE beside them.  Decision 13.
- **Sizes** (fp32 ONNX as measured; fp16 about half, to check in phase 0):
  CLIP B/32 image 352 MB + text 254 MB; SigLIP2 B/16 image 372 MB + text
  1 130 MB (its 256 k-token vocabulary); DINOv2-B 347 MB, -S 88 MB; YuNet
  0.2 MB; SFace 39 MB; AuraFace 261 MB; OWLv2 ~600 MB.

---

## 5. Architecture in this codebase

### 5.1 New modules (suggested)

| File | Role |
|---|---|
| `src/ml/models.rs` | The model registry: id, version, URLs, SHA-256, size, licence, input size and preprocessing. |
| `src/ml/download.rs` | Download on a worker thread with progress (`BgEvent::ModelDownload`), check, rename into place. |
| `src/ml/worker.rs` | The helper process's main loop: `ort` sessions (WebGPU, else CPU), batching, idle exit. |
| `src/ml/client.rs` | Starts the helper, the pipe protocol, restarts on CPU after a crash. |
| `src/ml/prep.rs` | Preview → model input: DCT-scaled decode (`imaging/decode.rs`), resize (`fast_image_resize`, already a dependency), normalise; RAW-only shots through `Raw::thumbnail_at_least`. |
| `src/search/index.rs` | Content keys, the path map, the vector file (§6). |
| `src/search/pass.rs` | The indexing pass, modelled on `analysis.rs`: jobs, cancel, yielding to visible thumbnails, network errors through `ops::netfs::classify_error`. |
| `src/search/query.rs` | Parsing (§7.2), text encoding, scoring, the cut-off, tags and contrast.  Pure apart from the text encoder; unit-tested. |
| `src/search/vocab.rs` | The hidden-tag vocabulary (generic words only) and its confusers (dog ↔ fursuit). |
| `src/model/subjects.rs` | Detections, grouping (pure, testable), suggestions, the user's subjects file (§6.5). |
| `src/app/search.rs`, `src/app/subjects.rs` | App state and views: the search field, results page, Subjects page, subject page, suggestion review. |
| `src/ui/subject_tile.rs` | Paints a subject tile (crop, name, count) for a grid of subjects. |

### 5.2 Data flow and threading

1. **Library changes.**  The calendar's index job already sends
   `BgEvent::IndexReady { done: true }` after a check, including the one
   `mark_stale()` triggers after an export.  `search::on_library(index)`
   diffs the library's files (path, size, mtime) against the path map, on
   a worker: unchanged files cost nothing; new or changed ones become jobs.
2. **The pass** (when decision 5 allows): per shot, the JPG if paired,
   else the RAW.  Jobs ordered newest day first and sent to the helper as
   paths.  The helper reads previews with the app's own code
   (`exif::jpeg_preview_bytes`, LibRaw) on 8 reader threads (the share is
   bandwidth-bound), decodes on the reader and runs batches of 16–32.  The
   app pauses the job stream while visible thumbnails are queued, as the
   analysis pass yields.  Results come back as `BgEvent::Search { run,
   batch }`; the UI thread only updates counts, and the index is written
   by the pass's own thread.
3. **A query** never touches the share: text → embedding (helper or a
   small CPU session, ~14 ms), scores against the vectors in memory
   (~3 ms), filters, cut-off, grouping by day.  Runs off the UI thread;
   the results page appears when it returns.
4. **Subjects** (phases 3–4) are a second pass over the same previews
   (or folded into the first when switched on): detections and their
   embeddings into the subjects cache, grouping recomputed on a worker
   when it changes.

---

## 6. Index design

### 6.1 Where it lives

- Recomputable data: `~/.cache/photo_pipeline/search/` (embeddings,
  detections, tags, crops for tiles).  Shown and cleared in Preferences ▸
  Storage like the other caches.
- The user's own data (names, merges, "not this person"):
  `~/.local/share/photo-pipeline/subjects.json`, next to `unsaved/`.  Never
  cleared with the caches.
- **Nothing in the library or a working folder**, no sidecar, no XMP.

### 6.2 Keys: file identity that survives renames

Export renames files (`YYYYMMDD_HHMM_NNNN`) and moves them, so a path is
no key.  Neither is (size, mtime): uncompressed RAWs share their size
(9 224 of 9 547 RAWs in one shoot did), and FAT-formatted cards keep
2-second times.

**Key = a 128-bit digest of the file's size and first 64 KB.**  Those bytes
hold the EXIF with the capture time, sub-seconds and the maker note's
frame counters, so different frames never share them, and the pass reads
them anyway to find the preview.  It survives Sort, export (copy or move,
renamed or not) and copies to other disks.  Byte-identical copies (the
days exported twice) share one entry.  A cropped JPG re-encoded by a Move
export is new bytes and is indexed again.

The **path map** remembers (library-relative path, size, mtime) → key, so
a check needs no reads for files that haven't changed; a renamed file costs
one 64 KB head read and no inference.

Shots: an embedding is stored for the file analysed (the JPG of a pair);
the RAW maps to the same shot through the day's pairing (`LibraryDay`
shots), so a pair is one result.

### 6.3 Incremental updates

- **New or changed files** (after an export, `mark_stale()` → check →
  diff): head read → key → if known, map only; else queue.
- **Deleted files:** their path entries go when their folder is re-read.
  Vectors with no path left are dropped when the file is compacted (at
  most once a session), so a file moved out and back costs nothing.
- **A new model or preprocessing:** every vector file records its model
  id, weights SHA-256 and a preprocessing version; a mismatch means a new
  file built beside the old, which keeps answering queries until the new
  one is complete.
- **The share away:** search keeps working from the index (results draw
  from the disk thumbnail cache when it has them); opening a photo waits
  for the share, as everywhere else.

### 6.4 Storage and query cost

Flat files on `model/binfile.rs`, as the other caches: a header (magic,
version, model id, dimension), then rows of key and f16 vector, appended as
batches arrive; the small path map written whole through
`fsutil::atomic_write`.

| Data | Size for this library |
|---|---|
| Search vectors, SigLIP2 (768-d f16) | 87 MB (CLIP B/32 512-d: 58 MB) |
| Keys and path map | ~6 MB |
| Face vectors (8–21 k faces, 512-d) | 8–22 MB |
| Head vectors (~67 k crops, DINOv2-B CLS 768-d) | ~100 MB |
| Tile crops (128 px JPEG, covers and suggestions only) | a few MB |

**Query:** brute force.  56 710 × 768 in f32: 3.0 ms with the top 200;
110 162 × 768: 5.5 ms.  Held in memory as f32 (174 MB) only while
searching, or as i8 (44 MB) if phase 0 shows no loss.  No SQLite, no HNSW:
both add a dependency and an index to keep in step for no gain below a
million rows.

### 6.5 The user's subjects

Groups are recomputed whenever detections change, so the user's decisions
can't point at a group number.  They point at detections and keep
prototypes:

```jsonc
{
  "version": 1,
  "subjects": [
    {
      "id": "6f1c…",                  // random, stable
      "name": "…",                    // the user's, never written to photos
      "kind": "person",               // person | character | pet
      "hidden": false,
      "cover": { "key": "…", "box": [0.412, 0.188, 0.506, 0.331] },
      "members":  [ { "key": "…", "box": [ … ] } ],   // confirmed
      "rejected": [ { "key": "…", "box": [ … ] } ]    // "not this person"
    }
  ]
}
```

A detection is (content key, box rounded to 1/1000).  If the detector
changes, old boxes are matched to new ones by overlap.  A subject's photos
are its confirmed members plus suggestions the user accepted; unconfirmed
detections near its members (under the cut-off) are offered as
suggestions, never added silently.  Merging moves members; splitting
moves them back or rejects them.  Saved on a worker with
`fsutil::atomic_write`.

---

## 7. Search and subjects in the app

*§7a, after this section, records the interface design made once the
first decisions in §13 were taken.  Where the two differ, §7a wins:
search has a page of its own, and people, characters and pets are three
separate kinds.*

### 7.1 Design direction

The new pages are library pages, so they wear the calendar's chrome and
keep photos on neutral grey:

- **Colour:** no new tokens.  Header and panels `MOUNT`, fields `RAISED`,
  text `INK`, captions `INK3`; results and subject crops sit on
  `PHOTO_GROUND` / `PHOTO_MOUNT`, never tinted.  `SIGNAL` purple only for
  focus, the selection and the main action.
- **Type:** Barlow throughout.  Page titles `T_HEAD` semibold (the query
  itself is the title of a results page), subject names `T_BODY` medium,
  counts `T_SMALL` in `INK3`.
- **Layout:** the calendar's 60 px header; content on the 4 px scale.
- **The one memorable thing:** reviewing a subject feels like culling.
  Suggestions come one at a time, large, with **Y** and **N** where P and
  M would be, and advance after each answer.  Everything else stays quiet.
- **Hidden means hidden:** no tag chips on photos, no badges for subjects.

### 7.2 Where search lives

- **Ctrl+F** (free today) and *View ▸ Search the library*: switch to the
  library calendar and focus its search field.  Esc clears the field and
  returns to the page you were on.
- The field sits in the calendar header, left of Month | Year:

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ ›  June 2024                      ⌕ Search the library       Month│Year  ⟳ │
│      1 204 photos over 6 days                                                │
├──────────────────────────────────────────────────────────────────────────────┤
│                         (calendar as today)                                  │
```

- Typing offers completions under the field: subject names first
  ("@…  People, 312 photos"), then vocabulary words with counts
  ("fireworks  312 photos"), so the hidden tags show what's findable
  without labelling any photo.

**Query syntax** (plain, all optional, combined with AND):

| Write | Means |
|---|---|
| `red car at night` | photos that look like the words |
| `"a red car at night"` | the words exactly as the prompt |
| `dog -fursuit` | like "dog", and more like it than like "fursuit" |
| `@name` | photos of that subject; `@name camera` ranks them by "camera" |
| `2024`, `jun 2024`, `apr–jun 2024` | only those dates |

### 7.3 Results

The calendar's read-only day grid (`ui::grid::Grid` with day `Section`s),
so everything a day page does works: Space for quick look, Enter for the
viewer, the RAW+JPG tag, one pair = one result.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Calendar   camera                 ⌕ camera                 ×  By date│Best │
│              212 photos over 14 days                                         │
├──────────────────────────────────────────────────────────────────────────────┤
│  Saturday, June 8, 2024   18                                                 │
│  ▢ ▢ ▢ ▢ ▢ ▢ ▢ ▢                                                             │
│  ▢ ▢ ▢ ▢ ▢ ▢ ▢ ▢ ▢ ▢                                                         │
│  Saturday, April 13, 2024   6                                                │
│  ▢ ▢ ▢ ▢ ▢ ▢                                                                 │
│                          Show weaker matches                                 │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **By date** (newest day first, capture order inside) is the default;
  **Best first** is one section in score order.  Decision 7.
- **Cut-off:** a result needs a score above a threshold set per model in
  phase 0, relative to the query's own spread (scores aren't comparable
  between queries).  *Show weaker matches* lowers it a step at a time.
- **Ranking:** photos carrying the query word as a hidden tag first
  (precise), then the rest by similarity.  Accidental frames left out.
- **Animal words:** when the vocabulary marks the word as confusable
  (dog, cat, bird, fox, wolf, dragon…), a line under the title offers it:
  *Many of these are fursuits.* with the button *Leave out fursuits*,
  which adds `-fursuit`.
- **Empty and waiting states**, in the app's voice:
  - not indexed: *Search needs an index of the library. Photo Pipeline
    reads each photo's preview once and keeps what it learns on this
    computer. About 8 minutes for this library; you can keep working.*
    Button: **Index the library**.
  - indexing: results so far, and the status bar's *Indexing the
    library: 12 400 of 56 710 shots* with ×.
  - nothing found: *Nothing in the library looks like "zebra". Try
    another word, or fewer words.*

### 7.4 The Subjects view

*View ▸ Subjects* (Ctrl+4, beside List, Grid and Library calendar), also a
button in the calendar header.  Before any subject is found, the page says
what finding them does, what it costs and that it stays on this computer,
with **Find people and characters**.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Subjects                            ⌕ Search the library        Calendar  ⟳ │
│ 12 people and 31 characters                                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ People                                                                       │
│   ( )     ( )     ( )     ( )     ( )     ( )                                │
│  Name    Name   Unnamed Unnamed Unnamed Unnamed                              │
│  312     140      58      23      12       9                                 │
│                                                                              │
│ Characters                                                                   │
│  ┌──┐    ┌──┐    ┌──┐    ┌──┐    ┌──┐                                        │
│  └──┘    └──┘    └──┘    └──┘    └──┘                                        │
│  Name   Unnamed Unnamed Unnamed Unnamed                                      │
│  96       41      30      22      14                                         │
│                                                                              │
│ Things   fireworks 312   stage 280   cars 41   food 37   dogs 3   …          │
│                                                                              │
│ › Smaller groups (230)        › Hidden (7)                                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

- People are round crops of the face; characters rounded squares, so ears
  and horns fit.  Crops sit on `PHOTO_MOUNT`.  The kind is told by the
  section and the shape, not by colour.
- Named subjects first, then unnamed groups by size.  Groups below a
  minimum (5 photos, decision 9) wait under *Smaller groups*, which keeps
  an event's strangers out of the way.
- *Things* lists the most common hidden tags as plain text; a click
  searches for the word.
- Keys: arrows move, Enter opens, F2 renames, Ctrl-click or S selects
  several (for Merge).

### 7.5 A subject's page

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Subjects  ( )  Name                      Find more   Merge into…   Hide    │
│                  312 photos over 9 days                                      │
├──────────────────────────────────────────────────────────────────────────────┤
│  Is this Name?   [ large crop ]   ← photo context →     Yes (Y)   No (N)     │
│  24 suggestions left                                                         │
├──────────────────────────────────────────────────────────────────────────────┤
│  Saturday, June 8, 2024   40           (the day grid, as search results)     │
│  ▢ ▢ ▢ ▢ ▢ ▢ ▢ ▢                                                             │
```

- Click the name (or F2) to name or rename; an unnamed group reads "Name
  this person" / "Name this character".
- **Suggestions** (Y, N, → to skip, Esc to stop) show the crop large and
  the photo around it.  Y adds it to the subject; N records "not this
  person" so it is never offered again.  Advance follows the viewer's
  *Advance after marking* setting.
- **Find more** searches for more suggestions from everything confirmed
  so far, with the strict cut-off.
- **Merge into…** asks which subject; **Split**: switch the grid to
  *Faces* (or *Heads*), pick crops, then *Move to…* or *Not this person*.
- **Hide** moves the subject under *Hidden*; its photos stay findable by
  name.
- **Show where** outlines the subject's box on each photo in the grid
  (off by default).
- Copy keeps one name per action: *Merge into…* → "Merged into Name";
  *Hide* → "Hidden. Show hidden subjects".

### 7.6 Pairs, RAW-only shots and culling

- One result per shot; the pair's tag shows both halves.
- RAW-only shots use the RAW's embedded preview: the smallest of at least
  300 px for search, at least 1 000 px for faces and heads.
- **Culling (optional, decision 14):** the same index keys by content, so
  a working folder could be searched from the filter bar ("Find in this
  folder") and its work carries over to the library after export.  P and
  M work on results there; in the library they stay read-only, as today.

### 7.7 Preferences

- **Search:** *Index new photos after export* (decision 5), *Use the
  graphics card* (decision 3; shows which device), the model in use.
- **People and characters:** *Find people* (decision 9), *Find
  characters*, the minimum group size.
- **Storage** (§10): the search index; people and characters, with
  *Forget all…*; downloaded models.

---

## 7a. Interface design

*Designed after the first decisions were taken.  The screens were built
as interactive mockups in the app's own tokens (Barlow, Catppuccin
chrome, neutral `PHOTO_*` greys, made-up data and grey placeholder
photos) and reviewed separately.  This section records them in words and
wireframes.*

### 7a.1 What the design follows

Decisions from §13 as taken, some changed from the recommendations:

- **Order** (1): search, then people, then characters, then pets, each
  usable on its own.  Pets are a phase of their own after characters.
- **Search has a page of its own** (6 (b)): a side-rail entry and Ctrl+F,
  not a field in the calendar header.
- **Library only** (14 (a)): results are read-only, like the calendar's
  days.
- **Indexing** (5 (b)): the first full index starts only when asked;
  after that each export is indexed in the background, with a line in the
  status bar and no dialog.
- **Search model** (2 (b)): SigLIP2, about 750 MB, the same for everyone.
- **Hidden tags** (8 (b)): completions, ranking and a *Things* list only.
- **Graphics card** (3 (b)): used after a self-check against the CPU,
  with a switch in Preferences.  If it fails (the self-check, no usable
  card, a driver crash), heavy work pauses and a notice offers *Continue
  on the CPU* (slower, lowest priority) or *Not now*.  Searching what is
  indexed keeps working.
- **Helper process** (4 (b)): if it dies, the app says so and carries on.
- **Faces are found during the first index, on by default** (9, now
  (b)), with a switch to turn it off and *Forget all*.
- **The face model is the user's choice** (10), asked in the download
  dialog: AuraFace (pre-selected), SFace, or InsightFace, whose
  non-commercial research licence is stated in plain words and which is
  downloaded from its publisher.  Changing it later finds faces again.
- **Characters** (11 (a), scanned automatically): heads are found during
  indexing, on fursuit photos only and on the GPU only (about 1.4 h the
  first time, then about 7 minutes per new event day).  The user names a
  character from one group and confirms suggestions with Y and N.
- **Pets** (12): their own kind, scan, groups and page, **never combined
  with fursuits or people**.  A search for an animal word leaves fursuits
  out by default, with a quiet *Show fursuits too*; "dog fursuit" or
  "fursuit" finds suits on purpose.

### 7a.2 Principles

- **Search is a place.**  A page of its own, like List, Grid and the
  calendar: it keeps its query while you work elsewhere, and its back
  button names where it returns.  (Decision 6; the rail is how this app
  moves between places.)
- **Results look like days.**  The calendar's read-only day grid, so
  quick look, the viewer, the RAW+JPG tag and one result per pair work
  as they do there.  No new grid, and nothing new to learn.
- **Words before widgets.**  One field.  The syntax (`-word`, `@name`,
  dates) is taught by the completions and their footer, not by filter
  panels.
- **Hidden means hidden.**  No tags or badges on photos; the vocabulary
  shows only as completions and the *Things* list.
- **Ask once, then stay quiet.**  One dialog before the first index lists
  every download, size, licence and the time it takes.  After that,
  indexing is a status-bar line.  A failure pauses the work and says why
  in the app's existing banner.
- **Three kinds, never mixed.**  People, characters and pets each have
  their own page, groups, suggestions and completions heading.
- **Reviewing feels like culling** (the one memorable screen).  One
  suggestion at a time, large, with Y and N where P and M would be,
  stamped and advanced the way marks are.
- **Strangers stay out of sight.**  Faces are on by default, so nothing is
  named for you, groups under the minimum fold away, unnamed groups are
  never offered as completions, and everything found can be forgotten in
  one place.

### 7a.3 Getting there and back

- **Rail and menu.**  One new command, `Cmd::Search` ("Search the
  library", Ctrl+F, a magnifier glyph), placed in `SIDE` under *Library
  calendar* and in the View menu after it, so the KDE global menu and the
  shortcuts dialog follow.  It is the only new rail entry: people,
  characters and pets live inside Search (open question 1).
- **Ctrl+F** from anywhere in the main window opens the page with the
  field focused and its text selected, so typing replaces the last query.
- **Back.**  The header's first button names where it returns ("‹ Grid",
  "‹ Jun 2024"), like the day page's "‹ Jul 2026".  Esc does the same once
  completions are closed.  P, M and the rail's marking commands do
  nothing on the search page, as on the calendar's days.
- **Open day.**  Each day heading in the results ends with *Open day ›*:
  the calendar's page for that day with the photo selected.  Its back
  button then reads "‹ Search" and returns to the same results and
  scroll position.

### 7a.4 The search page

**Header** (60 px, `MOUNT`): the back button, a larger field (36 px, the
query in `T_TITLE` medium: the query is the page's title), the count,
and the order switch.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Grid │ ⌕ cameras                      × │ 46 photos over 11 days  Date│Best│
├──────────────────────────────────────────────────────────────────────────────┤
```

**Start page** (nothing typed): what can be found, on `PHOTO_GROUND`.

```
│ Recent searches ─────────────────────────────────────────────── Forget them  │
│   fireworks   @Character 1   dog   cameras jun 2024                          │
│ People ─────────────────────────────────────────────────────── All people ›  │
│   ( )  ( )  ( 4 )                       named first, then "groups to name"   │
│ Characters ─────────────────────────────────────────────── All characters ›  │
│   ▢   ▢   ▢   [ 5 ]                                                          │
│ Pets ─────────────────────────────────────────────────────────── All pets ›  │
│   ▢   [ 2 ]                                                                  │
│ Things in your photos ───────────────────────────────────────────────────────│
│   fursuits 412   people 380   cameras 96   fireworks 24   food 21   …        │
│ Ways to search ──────────────────────────────────────────────────────────────│
│   red car at night    photos that look like the words                        │
│   fireworks -crowd    leave a word out                                       │
│   @Character 1        a person, character or pet you named                   │
│   fireworks 2024      only those dates (jun 2024, apr–jun 2024)              │
```

Each kind's row appears with its phase; a word or a crop starts a search.

**Completions** (the Hints menu's floating list: rows with a title, a
detail and a count, and a footer):

```
           ┌──────────────────────────────────────────────────────┐
           │ ⌕ f                                                  │
           ├──────────────────────────────────────────────────────┤
           │ Things                                               │
           │  ⌕  fursuits                              412 photos │
           │  ⌕  fireworks                              24 photos │
           │  ⌕  food                                   21 photos │
           │ Dates                                                │
           │  ▦  February 2024                1 day with photos   │
           ├──────────────────────────────────────────────────────┤
           │ -word leaves a word out    @ finds a person,         │
           │                              character or pet        │
           └──────────────────────────────────────────────────────┘
```

- Empty field: recent searches.  `@`: named subjects only, under
  *People*, *Characters* and *Pets* headings, never one list; unnamed
  groups are never offered.  `-`: things to leave out.
- ↑ ↓ choose; Enter or Tab takes the choice; Enter with none chosen
  searches what was typed.  A name becomes a chip in the field (with its
  crop), so names with spaces stay whole; Backspace in an empty field
  removes the last chip.

**Results** (`ui::grid::Grid` with day `Section`s, newest day first):

```
│ ⌄ Tuesday, December 31, 2024   1 photo ────────────────────────── Open day › │
│   ▢                                                                          │
│ ⌄ Saturday, October 26, 2024   5 photos ───────────────────────── Open day › │
│   ▢ ▢ ▢ ▢ ▢                                                                  │
│                  5 more photos look a little like "cameras".                 │
│                           [ Show weaker matches ]                            │
```

- **Best first** is one section, "Best matches first", in score order;
  the choice is remembered.
- **Cut-off** as in §7.3; *Show weaker matches* lowers it one step and
  says how many the step adds.  At the end: "That's everything that
  looks like …".
- **Animal words.**  Fursuits are left out by default; a quiet line at
  the end of the results says how many, with *Show fursuits too*.  Shown,
  they get a section of their own after the real animals ("Fursuits that
  look like dogs, kept apart from the real dogs", with *Hide fursuits*),
  never mixed into the days.  This replaces §7.3's *Leave out fursuits*
  strip.
- **Nothing found:** "Nothing in the library looks like "zebra".  Try
  another word, or fewer words.  Search goes by what a photo shows, not
  by file names or folder names."
- Keys as on a day page: ← → move, Space quick look (read-only: no P, M
  or U), Enter opens the viewer read-only, Esc goes back.

### 7a.5 Indexing, as the user sees it

**Not indexed yet.**  The search page's body invites the first index and
says what it costs, in the "Not analysed yet" pattern of the Hints menu:

```
│        ⌕  Search the library by what's in your photos                        │
│        Type what you're looking for … In the same pass it finds faces, so    │
│        you can name people and search for them.                              │
│        ┌──────────────────────────────────────────────────────────────┐      │
│        │ Reads      Each photo's preview once: about 20 GB from the   │      │
│        │            library on the share                              │      │
│        │ Takes      About 10 minutes with the graphics card, up to an │      │
│        │            hour on the processor (search: 7 or 45; faces: 3  │      │
│        │            more or 15).  Search works within a minute.       │      │
│        │ Downloads  Search 750 MB, faces 37–261 MB (you choose)       │      │
│        │ Keeps      About 110 MB, on this computer, only you can read │      │
│        └──────────────────────────────────────────────────────────────┘      │
│        After this, each export is added on its own.  Faces are never named   │
│        for you; finding them can be turned off in Preferences.               │
│        [ Index the library… ]                                                │
```

**The first index.**  The activity line runs along the top of the
content, and the status bar shows progress with ×, as Analyse does.
Clicking the progress opens the steps:

```
                ┌──────────────────────────────────────────────────────┐
                │ Indexing the library                                 │
                │ ⌕ Search and faces                         ▓▓▓▓░░░░  │
                │   18,200 of 56,710 photos, newest first.             │
                │   About 6 minutes left.                              │
                │ ⊡ Fursuit heads                            ░░░░░░░░  │
                │   Next, on the 42,180 photos with fursuits.          │
                │   About 1 h 25 min with the graphics card.           │
                ├──────────────────────────────────────────────────────┤
                │ Search works on whatever is done.  Fursuit heads are │
                │ found only with the graphics card.                   │
                └──────────────────────────────────────────────────────┘
 Indexing: search and faces 18,200 of 56,710 ▓▓░░ ×                (status bar)
```

Searching while it runs shows results so far, under a strip: "Searching
the 18,200 photos indexed so far, newest first.  Results fill in as
indexing reaches older days."  New days fade in as they arrive.

**After an export** (quiet): no dialog and no activity line, only the
status bar: "Exported 412 photos to the library." then "Adding them to
search 120 of 412", "Finding fursuit heads in them 80 of 310", and
finally "Search has the 412 new photos."

**The library stops answering:** the analysis banner's pattern
(`WARN` strip): "Indexing stopped: the library on nas stopped answering.
18,200 of 56,710 photos were indexed and are kept, and search works on
those.  Indexing carries on from there when the library is back."
*Try again*, ×.

**The graphics card stops working.**  A banner under the folder bar, in
whichever view is open (like the network banner), since indexing runs
while the user culls:

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ▣ Indexing paused: the graphics card stopped responding                      │
│   Its driver stopped the search helper.  Search still works on the 18,200    │
│   photos indexed so far.  Indexing can carry on with the processor instead:  │
│   slower (about 35 minutes left), and it gives way to anything else you do.  │
│                                         [ Continue on the CPU ]   Not now    │
└──────────────────────────────────────────────────────────────────────────────┘
```

- *Self-check failed:* "the graphics card gave wrong answers … the
  results didn't match, so the card isn't used."
- *No usable card:* "Indexing is waiting: there's no graphics card it can
  use … up to an hour for this library."
- *During the head scan:* heads are GPU-only, so the offer is *Try the
  card again* or *Not now* ("on the processor it would take about 15
  hours"; open question 9).  Search and faces are unaffected.
- *Not now* leaves the work paused; the status bar keeps "Indexing
  paused" with the same offer as a link.
- *The helper stops for another reason:* it is restarted and the status
  bar says "The search helper stopped and was started again.  Indexing
  carried on."  No banner.

### 7a.6 People, characters and pets

- **Three kinds, three pages.**  One page per kind, switched by
  *People | Characters | Pets* tabs in its header and reached from the
  kind's row on the search start page.  A page shows one kind only, so a
  merge, a move or a suggestion can never cross kinds; completions list
  each kind under its own heading.
- **Shapes:** people are round crops of the face; characters and pets
  rounded squares, so ears and horns fit.  Crops sit on `PHOTO_MOUNT`;
  the kind is told by the page and the shape, never by colour.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Search │ Characters                     People│Characters│Pets    ☐ Select │
│          │ 3 named, 5 groups to name                                         │
├──────────────────────────────────────────────────────────────────────────────┤
│ Named  3 ────────────────────────────────────────────────────────────────────│
│   ┌──────┐     ┌──────┐     ┌──────┐                                         │
│   └──────┘     └──────┘     └──────┘                                         │
│  Character 1  Character 2  Character 3                                       │
│   96 photos    54 photos    31 photos                                        │
│ Groups to name  5, largest first ────────────────────────────────────────────│
│   ┌──────┐     ┌──────┐   …                                                  │
│  Name this    Name this                                                      │
│  character    character                                                      │
│ › Smaller groups 230     › Hidden 1                                          │
```

- **Right after the first index** the People page holds only groups to
  name (none named yet) and *Smaller groups*; the Characters page shows
  the head scan's progress, with groups appearing as they are found.
- **A subject's page is a search for it**: the subject is a chip in the
  field (words added narrow it), and a band under the header holds the
  subject.

```
│ ‹ Characters │ ⌕ [▢ Character 1 ×] Add words      │ 96 photos      Date│Best │
├──────────────────────────────────────────────────────────────────────────────┤
│ ▢ Character 1 ✎         Photos│Heads   Find more  [Review 24 suggestions Y] ⋯│
│   96 photos over 3 days.  24 more photos may show this character.            │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⌄ Saturday, October 26, 2024   22 photos ──────────────────────── Open day › │
```

- **Naming.**  An unnamed group opens with "Name this character" focused
  in the band.  Typing a name that already exists offers *Merge into
  Character 1* first and *Name it "…"* second; Enter takes the highlighted
  one.  *Check this group (Y)* reviews the group's own members ("Same
  character as this group?") before or after naming.  F2 or a click on
  the name renames.
- **Merging.**  Ctrl+click or S picks several; a strip says "2 selected"
  with *Merge…*, *Hide* and ×.  The dialog asks which name to keep (the
  named one by default) and says Ctrl+Z undoes it.
- **Splitting.**  *Photos | Heads* (or *Faces*) shows every crop by day;
  picked crops go *Move to…* another subject of the same kind or a new
  one, or *Not Character 1*, which also stops them being suggested.
- **Hiding** takes a subject off its page and out of completions; its
  photos still come up when searched by name.  The status bar says so
  with *Undo*; *Hidden* lists them with *Show*.
- **⋯** holds *Merge into…*, *Change the cover*, *Show where* (outlines
  the subject on each photo), *Hide* and *Forget this character…*.
- **Pets** (phase 5): the same page pattern.  Animals are found only on
  photos the index marks as animals and not as fursuits.  Until the
  phase ships, the Pets tab says "Pets are coming later" and that search
  finds animals by word meanwhile.

### 7a.7 "Same character?": the review

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Character 1 │ Same character?          ▓▓▓░░░░░░  Auto-advance ●   [Done]  │
│               │ 7 of 24 suggestions. 5 added, 1 not.                         │
├──────────────────────────────────────────────────────────────────────────────┤
│  Character 1, from          ┌────────────────┐       In the photo            │
│  96 photos                  │                │       ┌───────────────────┐   │
│  ┌────────┐                 │  the suggested │       │    ┌────┐         │   │
│  │ cover  │                 │  crop, 300 px  │       │    │box │         │   │
│  └────────┘                 │                │       │    └────┘         │   │
│  ▫ ▫ ▫   ▫ ▫ ▫              └────────────────┘       └───────────────────┘   │
│                           [ ✓ Yes  Y ] [ ✕ No  N ]   20240608_1609_2755      │
│                   A burst of 4 frames: the answer covers them all.           │
├──────────────────────────────────────────────────────────────────────────────┤
│ ▫✓ ▫✓ ▫✓ ▫✕ ▫✓ ▫✓ [▫] ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫                                  │
│ Answers go to Character 1 at once.    Y Yes  N No  ← → Step  Enter  Esc Done │
└──────────────────────────────────────────────────────────────────────────────┘
```

- Left: what the subject looks like (its cover and confirmed crops).
  Centre: the suggestion, large.  Right: the whole photo with the head's
  box outlined (light line, dark halo; never the accent, which means
  selection).
- **Y** adds it, **N** records "not this character" so it is never offered
  again.  The answer is stamped as marks are: a green check on the
  crop's corner (`KEEP`), or a grease-pencil cross with the crop dimmed
  (`REJECT`).  Buttons use `style::toggle` in those tones.
- With *Advance after marking* on (the same setting, shown as
  *Auto-advance*), it moves on after the same short beat; ← → (or A D)
  step; Enter opens the photo in the viewer; Ctrl+Z takes the last
  answer back; Esc stops and the status bar says "Added 5 photos to
  Character 1.  1 photo won't be suggested again."
- The strip along the bottom is the viewer's filmstrip pattern: answered
  suggestions carry their stamp, the current one the accent frame; click
  to go back to one.
- **One suggestion per burst**; its answer covers every frame (open
  question 6).
- **The end:** "All 24 answered.  18 photos added to Character 1.  6
  won't be suggested again."  *Find more* (strict, a few at a time) and
  *Back to Character 1*.
- The same screen serves people ("Same person?") and, later, pets.

### 7a.8 First run and the download dialog

**Pointing search out once** (open question 2): (a) quiet: the rail, the
View menu, Ctrl+F, and the page explains itself; (b) a dot on the rail's
magnifier until it is first opened; (c) a one-time tip beside the rail
("New: search the library … Got it"); (d) a status-bar line after the
next export ("Exported 412 photos.  Set up search to find photos by what
they show").

**The download dialog** opens from *Index the library…*:

```
┌────────────────────────────────────────────────────────────────────┐
│ Download the models and index the library?                         │
│ Search and face finding run on this computer, with models          │
│ downloaded once from their publishers.  Nothing about your photos  │
│ is sent anywhere.                                                  │
│ Search  Always used.                                               │
│ │ SigLIP 2, base size                                       750 MB │
│ │ Finds photos by what they show.                                  │
│ │ Licence: Apache-2.0, free for any use.  From Google.             │
│ ☑ Find people's faces   Pick the model:                            │
│ │ ◉ AuraFace  Recommended                                   261 MB │
│ │   The most accurate in tests.  Slowest: about 3 minutes with the │
│ │   graphics card, 15 on the processor.  Apache-2.0.               │
│ │ ○ SFace                                                    37 MB │
│ │   The smallest and fastest.  Mixes people up more often.         │
│ │   Apache-2.0.                                                    │
│ │ ○ InsightFace                                       about 190 MB │
│ │   What Immich uses.  Licence: non-commercial research only, so   │
│ │   not for any commercial use.  From InsightFace.                 │
│ ☑ Find fursuit characters   Graphics card only.                    │
│ │ OWLv2 and DINOv2                                          950 MB │
│ │   About 1.4 hours for this library, then 7 minutes a new day.    │
│ Downloads  1.96 GB in all, once                                    │
│ Reads      About 20 GB from the library on the share               │
│ Takes      About 10 minutes with the graphics card, up to an hour  │
│            on the processor, then about 1.4 hours for heads        │
│ Each file is checked against its SHA-256 before it's used, and     │
│ deleted if it doesn't match.        Show sources and checksums     │
│                         [ Cancel ]  [ Download 1.96 GB and index ] │
└────────────────────────────────────────────────────────────────────┘
```

- Search is fixed; faces and fursuit heads are on, each with a box to
  turn it off; totals and times update with each choice.  InsightFace is
  never pre-selected, and its licence line is in `WARN`.
- *Show sources and checksums* lists each file's source and SHA-256.
- The face download includes YuNet (0.2 MB, MIT) for AuraFace and SFace;
  InsightFace brings its own detector.  InsightFace's size and speed
  are estimates to confirm in phase 0.
- **Downloading:** one row per model with a bar, then "Checked".
  Indexing starts as soon as the search model is in place; the dialog
  can be closed and downloads carry on, with progress in the status bar.
- **A checksum mismatch:** "The face model didn't match its checksum.
  AuraFace downloaded, but it isn't the file Photo Pipeline expects, so it
  was deleted and nothing was installed …  Search and fursuit heads are
  fine; indexing goes ahead without faces."  Expected and received
  digests; *Not now*, *Try again*.
- **Offline:** "Couldn't reach the model's publisher.  Nothing was
  downloaded or indexed.  Try again when you're online."

### 7a.9 Preferences

New sections in the existing Preferences window, in its card-and-row
pattern:

- **Search:** *Index new exports* (on); *Use the graphics card* (on),
  with the last self-check's result ("Checked against the processor:
  same results") and *Check again*; *Search model* (SigLIP 2, shown as
  fixed); *Recent searches* with *Forget them*.
- **People:** *Find people's faces* (on).  Turning it off offers, inline:
  "No new faces will be looked for.  Forget the 8,412 faces already
  found, and the 2 names you gave?"  *Keep them* / *Forget them*.  *Face
  model* with *Change…* (the same chooser, saying how long finding faces
  again takes and that names are kept).  *Smallest group to show*: 3, 5
  or 10 photos.
- **Characters:** *Find fursuit characters* (on; graphics card only).
- **Pets:** *Find pets*, "Coming later" until phase 5.
- **Storage**, new rows: *Search index* (92 MB, 56,710 photos;
  *Rebuild…*, *Clear…*); *People, characters and pets* ("not a cache: it
  asks first"; *Forget all…*); *Downloaded models* (1.96 GB; *Open*,
  *Delete…*).  Each action confirms in the window, with what it costs:
  "Rebuild the search index?  Every photo in the library is read again:
  about 20 GB, about 7 minutes with the graphics card.  Search keeps
  using the current index until the new one is ready."

### 7a.10 New keys

| Key | Where | Does |
|---|---|---|
| Ctrl+F | Main window, anywhere | Opens Search, field focused, its text selected |
| ↑ ↓, Enter, Tab | Search field | Choose and take a completion; Enter alone searches |
| Backspace | Empty search field | Removes the last name chip |
| Esc | Search page | Closes completions, else goes back where you came from |
| ← →, Space, Enter | Results | As on a day page: move, quick look, viewer (read-only) |
| F2 | A kind's page; a subject's page | Name or rename |
| S, Ctrl+click | A kind's page | Pick several, to merge or hide |
| Y | A subject's page | Start reviewing its suggestions |
| Y, N | Review | Yes, same; no, not this one |
| ← → (A, D), Enter, Esc | Review | Step, open the photo, stop |
| Ctrl+Z | Review | Take the last answer back |

Checked against `src/app/commands.rs` and the view key handlers:

- **Ctrl+F** is free: `Cmd::from_key` binds no Ctrl+F.  Plain F (fit)
  belongs to the viewer and compare, in their own window, and is not
  touched.
- **Y and N** are unbound without modifiers everywhere.  Ctrl+Y stays
  Redo; the review checks for no modifier.
- **F2** is unbound.
- **S** is the existing *Select mode* command (today disabled only in
  the calendar view), enabled on the kinds' pages with its usual meaning.
- Arrows, A and D, Space, Enter and Esc keep the meanings they have in
  quick look and the viewer.
- §7.4's Ctrl+4 for a Subjects view is dropped: the kinds live inside
  Search.  No new single-letter command is added to `Cmd`.

### 7a.11 What it reuses

| Existing piece | Reused for | Change needed |
|---|---|---|
| `ui::grid::Grid` with day `Section`s | Results; a subject's photos | Best first needs a section without a date (`Section` is keyed by `NaiveDate`): a label-only section or a sentinel date |
| The calendar's day page | *Open day* | Its back button learns a "Search" origin |
| Quick look, read-only viewer (`read_only`) | Space and Enter on results | None beyond the read-only path the calendar uses |
| Hints menu (`hints_ui.rs`): floating rows with title, detail, count and a footer | Completions; the indexing steps; the "Not indexed yet" invitation (its "Not analysed yet" body) | A row variant with a crop for names |
| Analysis progress in the status bar, with × | Indexing progress | Clickable, to open the steps |
| The analysis and network banners (`WARN` strip) | Library away; the graphics-card notice | None |
| The duplicates strip (`copies_bar`) | "2 selected" strips for merge, hide and heads | None |
| The bulk-action confirmation (`bulk.rs`): count, the photos, why | Merge (crops and counts), Forget all, Rebuild, Clear | None |
| `commands.rs` `SIDE` and `MENUS` | `Cmd::Search` | One entry; the global menu and shortcuts follow |
| `kit::segmented`, `kit::btn`, `kit::tip`, `style::input`, `style::toggle` | Date│Best, People│Characters│Pets, buttons, the field, Yes and No | A 36 px field size |
| Viewer filmstrip (`ui::filmstrip`) and the keep check and grease-pencil cross | The review's strip and stamps | Square crops instead of thumbnails |
| *Advance after marking* (`viewer_advance.rs`) and `ui::stamp` | The review's advance and its beat | None |
| `undo.rs` | Answers, merges, hides, names | A subjects entry kind; it outlives a folder change |
| Preferences Storage rows (`preferences_storage.rs`) | Search index, people, models | Three rows, measured on the worker |

New pieces: `ui/subject_tile.rs` (§5.1: crop, name, count in a mount;
round or rounded square) and glyphs drawn in `ui/icons.rs` with the same
stroke: *Search* (magnifier), *Head* (a character's head), *Merge*,
*Split*, *Hide* (eye crossed out) and *Graphics card*.

### 7a.12 States

| State | Where | What the user sees |
|---|---|---|
| Nothing typed | Search page | The start page (§7a.4) |
| No completion | Completions | "Press Enter to search for "…"" |
| No name matches `@…` | Completions | "No one by that name yet" |
| Nothing found | Results | "Nothing in the library looks like "…"" with what to try |
| Weaker matches left | End of results | "N more photos look a little like …" and *Show weaker matches* |
| Animal word | End of results | "N fursuits that look like dogs are left out" and *Show fursuits too* |
| Not indexed | Search page | The invitation with its costs |
| First index running | Status bar, steps, strip on results | Progress with ×; results so far |
| After an export | Status bar only | Quiet progress, then "Search has the N new photos" |
| Library away | Banner | Kept, paused, *Try again* |
| Graphics card failed | Banner in any view | Why, *Continue on the CPU* or *Not now* (heads: *Try the card again*) |
| Helper restarted | Status bar | One line; work carries on |
| Download mismatch, offline | Dialog | Which file, what still works, *Try again* |
| Faces off | People page | "Finding faces is off.  Turn it on in Preferences." |
| Heads still scanning | Characters page | The scan's progress; groups as found |
| Pets before phase 5 | Pets page | "Pets are coming later" |

### 7a.13 Open UI questions

Each says what the mockups do now; the recommendation is the mockups'
choice.

1. **Where do people, characters and pets live?**  (a) Inside Search: a
   row each on the start page and a page each with kind tabs; (b) a
   side-rail entry of their own with the same tabs; (c) both.
   *Recommendation: (a).*  Search is where photos are found; one new rail
   entry; the rows show what can be searched for.
2. **How is search pointed out the first time?**  (a) Quietly; (b) a dot
   on the rail's magnifier until first opened; (c) a one-time tip beside
   the rail; (d) a status-bar line after the next export.
   *Recommendation: (b).*  Visible, silent, gone after one look.
3. **Where do fursuits go after *Show fursuits too*?**  (a) A section of
   their own after the real animals; (b) mixed into the days.
   *Recommendation: (a)*: pets and fursuits never share a list.
4. **How are results loosened?**  (a) *Show weaker matches* at the end,
   one step at a time; (b) a Stricter–Looser slider in the header.
   *Recommendation: (a).*
5. **How does a name sit in the query?**  (a) A chip, typed with `@`;
   (b) plain `@name` text.  *Recommendation: (a)*: names with spaces stay
   whole and the chip shows whose photos these are.
6. **Does one answer cover a burst?**  (a) Yes, one suggestion per burst;
   (b) one answer per frame.  *Recommendation: (a)*: a burst nearly
   always shows one pose; a frame that differs can be moved out later.
7. **How do Yes and No look?**  (a) The keep and prune colours, check and
   cross; (b) neutral, the accent only on the current suggestion.
   *Recommendation: (a)*: it reads like culling.
8. **What does Esc do on the search page?**  (a) Closes completions,
   else goes back and keeps the query; (b) clears the query first.
   *Recommendation: (a)*, as on the calendar's day pages.
9. **What does the head scan do when the graphics card fails?**  (a)
   Waits for the card: *Try the card again* or *Not now*; (b) offers the
   CPU anyway ("about 15 hours").  *Recommendation: (a).*
10. **What is the fursuit kind called?**  (a) Characters; (b) Fursuits.
    *Recommendation: (a)*: the suit is the costume, the character is who
    you search for.
11. **Are recent searches kept?**  (a) The last 10, shown when the field
    is empty, with *Forget them* in Preferences; (b) none.
    *Recommendation: (a).*

---

## 8. Phased rollout

Rough effort in focused working days for someone who knows the codebase.

| Phase | Content | Effort |
|---|---|---|
| **0. Evaluation** | A labelled set from scratch copies: ~30 queries with their right answers on a 2 000-photo sample; ~10 people named on sample L; ~15 characters across two event days.  An ignored test that reports precision at 10/50, recall, and group purity.  Fixes the cut-offs, prompt template, fp16/i8 storage and the model choice. | 1–1.5 d + ~1–2 h of the user's labelling |
| **1. Search** | `ml` (registry, download with checks, helper process, `ort` CPU + WebGPU with self-check), `search/index` (keys, path map, vectors), the pass (library, incremental after export, cancel, network banner), the calendar's search field, Ctrl+F, results grid by day, cut-off, empty states, Storage rows, bench and scripted UI tests, `PP_PROFILE` check. | 6–8 d |
| **2. Tags and syntax** | The vocabulary and derived tags, completions, ranking boost, confusers and `-word`, `@name` and dates, *Things*. | 2 d |
| **3. People** | YuNet + recognition pass (gated), grouping (incremental assignment, full regroup on a worker), Subjects view, subject page, naming, suggestions with Y/N, merge, split, hide, *Forget all*. | 6–8 d |
| **4. Characters** | OWLv2 export to ONNX and its cost on WebGPU, head pass (fursuit-tagged photos only, per day or range, GPU), DINOv2 crops, strict "find more", the same review UI. Starts with its own evaluation. | 4–6 d |
| **5. Optional** | Pets (same as characters), working-folder search, multilingual queries, OCR of signs and badges. | 3+ d |

---

## 9. Testing

- **Unit tests (no photos, no models):** the query parser; the cut-off on
  synthetic score lists; key stability (a rename, a move, a copy keep the
  key; a changed byte doesn't); path-map diffs; vector file round-trip,
  damage rejection, model-mismatch handling; grouping and suggestion rules
  on synthetic vectors; subjects file round-trip and merge/split.
- **Model tests (ignored, need `PP_MODEL_DIR`):** each model's output on
  a few synthetic images against stored reference vectors (catches a bad
  export or a provider bug), and WebGPU against CPU.
- **Evaluation (phase 0 set, ignored):** precision and recall per query,
  group purity, run before changing a model or threshold.
- **Bench:** `bench_search_index` on a scratch copy and on the share
  (reads only), CIFS bytes, CPU and GPU.
- **Scripted UI** (`PP_SCRIPT`, headless KWin, scratch `HOME` with a tiny
  model set): `key ctrl+f`, type a query, `shot`, Enter, Esc; the Subjects
  page; Y/N review.
- **Profiling:** `PP_PROFILE=1` while results stream in and while the
  pass runs during culling.

---

## 10. Privacy

- **All local.**  Embeddings, detections, crops and names stay on this
  computer.  The only network use is the one-time model download, which
  sends nothing about the photos.
- **Face and head embeddings are biometric-like.**  Stored in files only
  the user can read (0600, folders 0700); never written to sidecars, XMP,
  exports, the library or a working folder; never synced or exported.
- **Easy to wipe.**  Preferences ▸ Storage gains two rows in its
  existing pattern (a name, one sentence, the size, an action):
  - *Search index*: "What each photo looks like to search, so a search
    reads no photos."  92 MB, **Clear**, like the other caches.
  - *People and characters*: "The faces and heads found in your photos,
    and the names you gave them."  31 MB, 14 named, **Forget all…**.  Like
    the unsaved-marks row, it is not a cache, so it asks first: "Forget
    all people and characters? This deletes the faces and heads found in
    your photos and the names you gave them. Your photos are not changed."
    (**Forget all**, **Cancel**).

  Each subject has *Forget this person*.  Turning *Find people* off offers
  to delete what was found.
- **Strangers.**  Event days are full of people who never asked to be
  recognised.  People finding is opt-in (decision 9); unnamed groups below
  the minimum stay out of sight; unnamed detections are cache and go with
  *Clear*.
- **Nothing personal in the public repo.**  The vocabulary is generic
  words; tests use synthetic vectors; no photos, crops, names or vectors
  are committed.  `scripts/check-private.sh` could also refuse tracked
  `subjects.json`, `*.npy` and vector files.
- **Names stay in the app.**  They are never written into photo files; a
  later "write names as keywords on export" would be its own decision.

---

## 11. Licences

| Model or code | Role | Licence | Download by default? |
|---|---|---|---|
| CLIP ViT-B/32 (OpenAI) | search (fast) | MIT | yes |
| SigLIP2 ViT-B/16 (Google) | search (recommended) | Apache-2.0 | yes |
| PE-Core B/16 (Meta) | search (not chosen) | Apache-2.0 | yes |
| MobileCLIP / MobileCLIP2 (Apple) | — | `apple-amlr` (research use) | **no** |
| DINOv2 (Meta) | head grouping | Apache-2.0 | yes |
| OWLv2, OWL-ViT (Google) | head detection | Apache-2.0 | yes |
| Grounding DINO (IDEA) | — | Apache-2.0 | yes (not chosen) |
| YOLO, YOLO-World (Ultralytics) | — | AGPL-3.0 | **no** |
| YuNet (OpenCV Zoo) | face detection | MIT | yes |
| SFace (OpenCV Zoo) | face recognition | Apache-2.0 | yes |
| AuraFace v1 (fal) | face recognition | Apache-2.0 (its repo also carries InsightFace detectors; take only `glintr100.onnx`) | yes |
| InsightFace `buffalo_l`, `antelopev2` (Immich's) | — | "non-commercial research purposes only" | **no** |
| ONNX Runtime | runtime | MIT | — |
| `ort` crate | binding | MIT OR Apache-2.0 | — |
| Dawn (WebGPU) | GPU | BSD-3-Clause | — |

Downloading weights from their publisher at run time redistributes
nothing; hosting them (decision 13) means shipping each licence and
NOTICE beside them.  The README gains a *Models* section listing each
one, its source and licence, and the app's About lists those downloaded.
*Not legal advice*: the training data behind the weights (WebLI, LAION,
WIT) carries its own terms, which the model licences don't settle.

Sources: Immich docs (facial recognition, searching, ML hardware
acceleration, system settings) and discussions on pet recognition;
InsightFace model zoo README; ONNX Runtime provider docs (ROCm removed in
1.23); `ort` docs (execution providers, linking); candle and burn
repositories; the model cards on Hugging Face and OpenCV Zoo.

---

## 12. Risks

- **Domain mismatch.**  General models think "dog" is a fursuit here and
  rank empty frames high.  Mitigations measured above: contrast, the
  accidental filter, a relative cut-off.  Phase 0 checks them on more
  queries.
- **Character grouping is the uncertain part.**  Half the large automatic
  groups were mixed; cross-day and costume-change matching is barely
  tested.  Mitigation: semi-automatic by design, strict cut-off, its own
  evaluation before building the UI on it.
- **Compute.**  SigLIP2 on the CPU is ~45 min for the first index; OWLv2
  on the CPU is impractical (~15 h).  The fast path is an experimental
  provider.  Mitigations: helper process, CPU fallback, characters only on
  request per day or range.
- **NAS load.**  ~20 GB read for the first index (about 0.35 MB a shot).
  Read-ahead off, visible thumbnails first, newest days first, stoppable.
- **Build and size.**  +23 MB binary, a ~105–122 MB download at the first
  build, a release-candidate crate.  Pin it; `build.sh` says when it
  needs the network.
- **Memory.**  The SigLIP2 text tower is large (1.1 GB fp32); fp16 or an
  int8 vocabulary table, and the helper exits when idle.
- **Privacy.**  §10; opt-in for people.
- **UI weight.**  Two new pages and one field.  No badges, no tags on
  photos, nothing in the culling grid unless decision 14 says so.

---

## 13. Decisions for the user

1. **Scope and order.**
   (a) Search, then people, then characters, each usable on its own;
   (b) all three at once; (c) search only for now.
   *Recommendation: (a).*  Search is solid and cheap; people are
   feasible; characters need their own evaluation.
2. **Search model.**
   (a) CLIP ViT-B/32 (Immich's default, MIT): ~9 min first index on the
   CPU, ~300 MB, weaker on things at the frame's edges, more empty frames;
   (b) SigLIP2 ViT-B/16 (Apache-2.0): best here, ~45 min on 4 CPU threads
   or ~7 min with the GPU, ~750 MB; (c) both, chosen in Preferences.
   *Recommendation: (b)*, unless phase 0 shows (a) close enough.
3. **Graphics card.**
   (a) CPU only; (b) WebGPU when a self-check against the CPU passes, with
   a Preferences switch and the CPU as fallback; (c) WebGPU off unless
   switched on; (d) MIGraphX (custom ONNX Runtime build).
   *Recommendation: (b).*  Identical results measured, 5–7× faster.
4. **Where the models run.**
   (a) Worker threads in the app; (b) a helper process (the same binary
   with a flag) that exits when idle.
   *Recommendation: (b)*, for the GPU's sake and to give memory back.
5. **When indexing runs.**
   (a) Only when asked (like *Analyse this folder*); (b) the first full
   index when asked, then new exports automatically in the background;
   (c) everything automatically.
   *Recommendation: (b).*
6. **Where search lives.**
   (a) In the library calendar's header, Ctrl+F from anywhere; (b) a
   search page of its own; (c) (a) plus working folders (decision 14).
   *Recommendation: (a).*
7. **Result order.**
   (a) By date with a relevance cut-off and *Show weaker matches*;
   (b) best first; (c) by date by default, with a By date | Best switch.
   *Recommendation: (c).*
8. **Hidden tags.**
   (a) None, embeddings only; (b) derived tags for completions, ranking
   boost, *Things* and the head detector's choice, never shown on photos;
   (c) tags visible on photos.
   *Recommendation: (b).*
9. **Finding people.**
   (a) Off until switched on, groups under 5 photos kept out of sight;
   (b) on by default.
   *Recommendation: (a).*  Event days are full of strangers.
10. **Face recognition model.**
    (a) AuraFace (Apache-2.0, 261 MB, 30 ms a face, no visibly mixed
    group in the test); (b) SFace (Apache-2.0, 37 MB, 4 ms, 2 of 8 groups
    mixed).  InsightFace (Immich's) is ruled out by its licence.
    *Recommendation: (a).*
11. **Characters (fursuits).**
    (a) Semi-automatic: heads found per day or range on request (GPU
    advised), the user names a group and confirms suggestions with Y/N;
    (b) automatic groups like people; (c) not now, search only.
    *Recommendation: (a)*, after phase 4's evaluation.
12. **Pets.**
    (a) Search only for now (`dog -fursuit`); (b) the character machinery
    with "dog" and "cat".
    *Recommendation: (a).*  One real dog in the sample.
13. **Where models come from.**
    (a) Downloaded on first use from the publisher or a maintained ONNX
    export on Hugging Face (Immich's for CLIP models), pinned by SHA-256,
    after a dialog with size and licence; (b) our own exports on this
    repository's releases; (c) files the user places by hand.
    *Recommendation: (a)*, with (b) only for a model no maintained source
    publishes as ONNX.
14. **Working folders.**
    (a) Library only for now; (b) also search the working folder from the
    filter bar.
    *Recommendation: (a)*, (b) in phase 5: the content keys make it cheap
    later.

---

## Appendix — scratch artefacts (kept)

Under `<scratch>/agents/mlsearch/`, with a README holding every number
above.  Large model files, the Python environment, download caches and
build outputs were deleted; the scripts rebuild them (the README says
how).  Contact sheets show people's faces and stay out of the repository.

| Path | What it is |
|---|---|
| `README.md` | The numbers, the samples, the models with SHA-256, how to re-run |
| `files.tsv`, `lib.txt`, `event.txt`, `day.txt`, `sample.py` | The read-only library listing and the three samples |
| `extract.py`, `prev/` | Read-only MPF preview extraction, and the previews |
| `clip_embed.py`, `query.py`, `disamb.py`, `negate.py`, `tags.py`, `thumb_vs_preview.py` | Search, contrast, tags and input tests |
| `detect.py`, `viz_det.py`, `owl_speed.py` | Head detection and its cost |
| `subjects.py`, `subjects_lib.py`, `retrieval.py` | Head grouping and "more like this" |
| `faces.py`, `faces2.py`, `yunet_speed.py` | People |
| `export_onnx.py`, `bruteforce.py`, `compare_dump.py`, `rsort/`, `rsgpu/` | ONNX export, CPU timings, search cost; the Rust `ort` crates (CPU and WebGPU) |
| `out/` | Embeddings (`*.npy`), detections, groups, tags, query rankings |
| `sheets/` | Contact sheets: `q_*` queries, `d_*`/`neg_*` contrast, `tag_*`/`tags_*` tags, `det_*` detectors, `subj_*` groups, `more_*` "more like this", `faces*` people |
