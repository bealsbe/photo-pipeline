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
