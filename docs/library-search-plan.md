# Library search and subjects for Photo Pipeline — plan

*A feasibility study turned into a plan.  The study was written on
2026-10-01; its experiments ran in a scratch folder (kept, with its
contact sheets and numbers; see the appendix) and only read from the
photo archive.  The user took the decisions below the same day.  Nothing
in the app has changed yet.*

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

## Decisions (2026-10-01)

The user answered the study's open questions: 14 on the plan, 11 on the
interface (§7a), and three follow-ups their answers raised.  Where a later
answer changed an earlier one, the later one is recorded.  Every section
below describes the chosen path only; what was weighed and dropped, and
what waits for later, follow the table.

| # | Question | Decision | Why |
|---|---|---|---|
| 1 | Scope and order | **Search, then people, then fursuits, then pets**, each usable on its own. | Search is solid and cheap; people are feasible; fursuits and pets each need their own evaluation first. |
| 2 | Search model | **SigLIP2 ViT-B/16**, the same for everyone.  Phase 0 confirms it on the labelled set. | Best of three here: 7, 4 and 7 of its top 10 for camera, car and food against CLIP's 6, 3 and 4; it sees the whole frame. |
| 3 | Graphics card | **Where models run is a Preferences choice:** *Graphics card* (the default: WebGPU over Vulkan, used once a self-check against the processor passes) or *Processor*.  If the card is chosen and fails (the self-check, no usable card, a driver crash), heavy work stops (indexing, faces, fursuit heads) and the app says what failed.  It never moves that work to the processor by itself; the processor runs it only when the user picks it.  Searching what is indexed keeps working.  (This replaces an interim "pause and offer the processor" answer, and settles UI question 9.) | Identical results, 5–7× faster; the user doesn't want the processor loaded for hours unasked. |
| 4 | Where the models run | **A helper process**: the same binary with a flag, exiting when idle. | A driver fault kills the helper, not the app with marks still queued; memory goes back to the system. |
| 5 | When indexing runs | **The first full index when asked; after that, each export in the background.** | About 20 GB from the share once should be the user's call; after that it is minutes per day. |
| 6 | Where search lives | **A page of its own**: a side-rail entry and Ctrl+F. | Chosen over a field in the calendar's header. |
| 7 | Result order | **Best first only**: one list in score order, no grouping by day, each photo's date shown under it. | The user's choice; "when" stays visible on every tile. |
| 8 | Hidden tags | **Helpers only**: completions, a ranking boost, the *Things* list and choosing photos for the fursuit-head step; never shown on photos. | Precise (24 of 24 fireworks) but they name only the main subject; search found twice as many cameras. |
| 9 | Finding people | **On by default**, during the first index.  A Preferences switch turns it off; *Forget all* wipes it; groups under the minimum fold away. | The user's choice; the privacy safeguards stay (§10). |
| 10 | Face model | **The user's choice**, asked in the download dialog: AuraFace (pre-selected), InsightFace `buffalo_l` (Immich's; non-commercial research licence, downloaded from its publisher, not tested here) or SFace.  YuNet finds faces for all three.  Changeable in Preferences; names and confirmed photos survive a change. | This is a personal, non-commercial project; AuraFace is the one tested well here and free for any use. |
| 11 | Fursuits | **Semi-automatic, scanned during indexing**: heads are found on fursuit-tagged photos as a step of indexing; the user names a fursuit from one group and confirms suggestions with Y and N.  With *Processor* chosen, a head scan doesn't start on its own: the app shows its cost (about 15 h for the library, over an hour a day) and asks to start or skip it, each time (the user, 2026-10-01). | Automatic groups mixed suits in 7 of the 16 largest; "more like this" from a named one was right 32 times in 34. |
| 12 | Pets | **A phase of their own (5), after fursuits, never combined with fursuits or people**: their own step, groups, names and tab.  The phase starts by gathering and labelling pet photos. | A hard requirement from the user; the samples hold one real dog, so there is nothing to test on yet. |
| 12b | Animal words in search | **Fursuits left out by default.**  *Show fursuits too* adds them as a section of their own (UI 3); "dog fursuit" finds suits on purpose. | To the models here, "dog" means canine fursuits; the contrast puts the real dog first. |
| 13 | Where models come from | **The publisher, or a maintained ONNX export, pinned by SHA-256, after a dialog with size and licence.**  Our own export on this repository's releases only where nobody maintains one.  InsightFace only from its publisher.  The user states this is a personal, non-commercial project (§11). | Nothing redistributed that needn't be; every file checked. |
| 14 | Working folders | **Library only for now.** | Content keys make folder search cheap later (deferred). |
| UI 1 | Where people, fursuits and pets live | **Their own side-rail entry, *Subjects*, next to Search**, with *People \| Fursuits \| Pets* tabs (Ctrl+4).  This replaces an earlier "inside Search" answer. | The user confirmed it when asked about the conflict. |
| UI 2 | Pointing search out the first time | **Quietly**: the rail, the View menu, Ctrl+F, and the page explains itself. | No dot, tip or status line. |
| UI 3 | Fursuits after *Show fursuits too* | **A section of their own**, after the real animals. | Pets and fursuits never share a list, anywhere. |
| UI 4 | Loosening results | **A Preferences slider**, *How close matches must be*, with a small reset button; the default (set in phase 0) is what works for most people: good matches.  Nothing on the search page. | Keeps the page quiet; the slider was the user's choice over three steps. |
| UI 5 | Names in the query | **Chips, typed with @.** | Names with spaces stay whole; the chip shows whose photos these are. |
| UI 6 | Bursts | **Stacked in results like the grid's, collapsed by default and expandable; one review answer covers a whole burst.** | A burst nearly always shows one pose. |
| UI 7 | Yes and No in the review | **The keep and prune colours**, check and cross. | Reviewing reads like culling. |
| UI 8 | Esc on the search page | **Closes the completions; never leaves the page.**  The review's own Esc still stops the review. | Back is the header button or the rail. |
| UI 9 | Heavy work when the card fails | **See 3:** an error, no processor offer. | — |
| UI 10 | The kind's name | **Fursuits**, everywhere: tab, pages, completions, "Same fursuit?".  Its glyphs have a furry look. | The user's word for them. |
| UI 11 | Recent searches | **The last 10**, shown when the field is empty; *Forget them* in Preferences. | — |
| UI 12 | A subject's page before words are typed | **Clearest first.** | Doubtful matches sink to the bottom, where they are easy to spot. |

### Considered and not chosen

- **Scope:** all phases at once (the least certain part would hold up
  search); search only.
- **Search model:** CLIP ViT-B/32 (Immich's default: ~9 min first index
  on the CPU and ~300 MB, but it sees only the centre square and ranks
  more empty frames); PE-Core B/16; both, chosen in Preferences.
- **Processor:** CPU only; the card off unless switched on; MIGraphX
  (ONNX Runtime built against ROCm); any automatic move to the processor,
  including the interim "pause and offer it".
- **Runtimes:** candle (no AMD GPU), burn (every model is generated Rust
  to check), tract (CPU only).
- **Where models run:** worker threads in the app.
- **Indexing:** only ever when asked; everything automatic, the first
  index included.
- **Where search and subjects live:** a field in the calendar's header;
  people, fursuits and pets inside Search (the earlier answer), or in
  both places; the study's first *Subjects* view, every kind on one page.
- **Results:** by date with a cut-off; by date with a Date | Best switch;
  *Show weaker matches* at the end, or a Stricter–Looser slider.
- **Tags:** none; tags shown on photos.
- **People:** off until switched on.
- **Face model:** one fixed model; InsightFace ruled out entirely (the
  study's first position, before the user said the project is personal
  and non-commercial).
- **Fursuits:** heads found only on request, per day or range; fully
  automatic groups like people's; the name "Characters".
- **Pets:** riding on the fursuit head step or the face pipeline (rejected
  outright: pets are never combined with fursuits or people); search only,
  with no pet recognition.
- **Animal words:** results as found with an offer to leave fursuits out;
  fursuits mixed into the real animals' list.
- **Model sources:** our own copies of every model; files the user places
  by hand.
- **Interface details:** a dot, a tip or a status line pointing search
  out; plain `@name` text; one answer per frame and bursts shown frame by
  frame; neutral Yes and No; Esc going back or clearing the query; no
  recent searches; a subject's page newest first.
- **Detectors:** Grounding DINO (called muzzles faces, boxed bodies as
  heads), OWL-ViT B/32 (faster, messier boxes), the YOLO family (no
  fursuit class; Ultralytics' models are AGPL-3.0).

### Deferred

- **Search in working folders** (decision 14): the content keys carry an
  indexed folder's work over to the library after export, so this stays
  cheap to add.  P and M would work on results there.
- **Multilingual queries** and **OCR** of signs and badges.
- **Names as keywords on export**: its own decision if it is ever wanted;
  until then names never leave the app.
- **A choice of search model**: SigLIP2 is the same for everyone.

---

## 1. Summary

**Search first, then people, then fursuits, then pets, each usable on its
own.**

- **Free-text search works on the embedded previews.**  A joint
  image–text model (CLIP family) found cameras, cars, food, fireworks and
  fursuits by plain words.  SigLIP2 ViT-B/16, the chosen model, was the
  best of three tried: 7 of its top 10 for "camera" held a camera, 4 for
  "car" (the sample has few cars), 7 for "food", 24 of 24 for "fursuit".
  Indexing the whole library reads each shot's preview once (about
  0.35 MB, 6–7 minutes for the library over the share) and costs about
  4 ms a shot on this machine's graphics card (32–47 ms on the CPU).  A
  query takes ~14 ms to encode and ~3 ms to compare against every shot:
  no vector database is needed.  Results come best first.
- **This library needs one twist: "dog" finds fursuits.**  Both CLIP and
  SigLIP2 answer "dog" and "bird" with canine and avian fursuits; the
  sample holds one real dog.  Contrasting the query with "fursuit" fixes
  it: the real dog comes first (p = 1.00), a dog statue second, and
  everything else drops away.  So an animal word leaves fursuits out by
  default, with *Show fursuits too* to add them as a section of their own.
- **Hidden tags help search and never label photos.**  Tags derived from
  a fixed vocabulary are precise (24 of 24 "fireworks", 3 of 3 "camera")
  but name only a photo's main subject: search found more than twice as
  many cameras.  They cost nothing extra (computed from the stored
  embeddings in milliseconds) and drive completions, a ranking boost, the
  *Things* list and the choice of photos for the fursuit-head step.
- **People: on by default, with the face model the user picks.**  YuNet
  (MIT) finds faces in 3 ms per preview at half size.  AuraFace
  (Apache-2.0, an ArcFace ResNet-100), pre-selected, grouped the sample's
  faces with no visibly mixed group of three or more once blurry faces
  were left out; SFace (Apache-2.0) is 8× cheaper and mixed two of eight
  groups.  Immich's InsightFace `buffalo_l` has the best published scores
  but a non-commercial research licence and wasn't run here; since this is
  a personal, non-commercial project it is offered, never pre-selected,
  and downloaded only from its publisher.
- **Fursuits: semi-automatic, scanned during indexing, on the graphics
  card.**  No face model sees a fursuit.  An open-vocabulary detector
  (OWLv2, Apache-2.0) asked for "a fursuit head" boxed most heads; DINOv2
  (Apache-2.0) embeddings of the head crops grouped the same suit across
  bursts: 32 of 34 neighbours closer than a strict cut-off were the same
  suit.  But automatic grouping alone left 7 of the 16 largest groups
  mixed, so the user names a fursuit from one group and confirms
  suggestions with Y and N, the way P and M cull.  The head step costs
  0.12 s a photo on the graphics card (about 1.4 h for the library's
  fursuit photos, then ~7 min an event day) against 1.3 s on the CPU
  (about 15 h).
- **Pets: a phase of their own, never combined with fursuits or people.**
  It starts by gathering pet photos to test on (the samples hold one real
  dog).  Until it ships, search finds pets by word.
- **Runtime: `ort` (ONNX Runtime) in a helper process, on the graphics
  card.**  Measured from a scratch crate: CPU results equal to Python's,
  +23 MB to the binary.  Its prebuilt WebGPU provider runs on this AMD
  card through Vulkan (no ROCm needed): CLIP 1.2 ms, SigLIP2 4.4 ms,
  DINOv2 4.2 ms per image, with outputs identical to the CPU's.  It is
  marked experimental, so it runs in a helper process the app starts.  If
  the card fails, the app says what failed and stops; the processor is
  used only when chosen in Preferences.

**Where it shows:** Search is a page of its own on the side rail
(Ctrl+F).  People, fursuits and pets share a second rail entry,
*Subjects* (Ctrl+4), one tab per kind, never mixed.

**What first:** phase 0 builds a small labelled set and an evaluation
harness (about 1.5–2 days).  Phase 1 delivers search: the index, the
model download, the search page, results best first.  Phase 2 adds
hidden tags and the query syntax.  Phase 3 is people and the *Subjects*
page.  Phase 4 is fursuits, after its own evaluation.  Phase 5 is pets,
after gathering photos and its own evaluation (§8).

---

## 2. What Immich does, and what it leaves out

Immich (the self-hosted photo server the user pointed to) runs its machine
learning in a **separate Python service** (`immich-machine-learning`, a
container) that serves ONNX Runtime models downloaded from Hugging Face
and unloads them after an idle time (`MACHINE_LEARNING_MODEL_TTL`).  The
main server stores vectors in Postgres with the VectorChord extension.

| Feature | Immich | What it means here |
|---|---|---|
| Smart search | CLIP image and text towers; default `ViT-B-32__openai`; its model table lists SigLIP2 models (up to `ViT-SO400M-16-SigLIP2-384`) as higher quality, and multilingual ones | The same idea: one embedding per shot, a text embedding per query, with SigLIP2 ViT-B/16 (decision 2). |
| Faces | InsightFace `buffalo_l` (SCRFD detection + ArcFace ResNet-50 recognition); defaults: min detection score 0.7, max recognition distance 0.5, at least 3 faces to make a person; DBSCAN-like clustering, incremental per upload plus a nightly pass for leftovers; name, merge, hide, change the cover | The workflow carries over.  InsightFace's model zoo says its models are "available for non-commercial research purposes only", so here its recognition model is one of three choices, never the default (decision 10). |
| Search filters | People, CLIP text, file name, folder, tags, camera, dates, OCR, places | Dates and subject names are the useful ones here (§7.2). |
| Pets | **Not supported.** Maintainers: no good free model and no training data; "describe your pet in the search" instead.  One of the most requested features. | Pets get a phase of their own here (phase 5), kept apart from fursuits and people.  It can work because a household's pets are few and the user confirms each group. |
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

| Query | CLIP ViT-B/32 (OpenAI; Immich's default) | SigLIP2 ViT-B/16-256 (chosen) | PE-Core B/16 (Meta) |
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
  same pass), and cut results off by score (§7.2).
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

| | SFace (OpenCV Zoo, Apache-2.0) | AuraFace v1 (Apache-2.0, ResNet-100) | InsightFace `buffalo_l` recognition (ResNet-50, WebFace600K) |
|---|---|---|---|
| Size, CPU cost | 37 MB, 3–4 ms per face | 261 MB, 30 ms per face | 174 MB; ~15 ms per face estimated (half AuraFace's work), not measured |
| No quality gate (score ≥ 0.8), cut-off 0.55 | 4 of the 12 largest groups were blurry faces of different people | — | not run |
| Score ≥ 0.9, cut-off 0.5 / 0.6 | 8 groups of ≥ 3 faces (42 faces); 2 mix two people | 7 groups (38 faces); none visibly mixed | not run |
| Published accuracy | 99.40 % (OpenCV Zoo's own evaluation) | LFW 99.65 %, CFP-FP 95.2 %, AgeDB 96.1 % | LFW 99.83 %, CFP-FP 99.33 %, AgeDB-30 98.23 % (InsightFace's model zoo) |

Both models run put young children in one group of 12 that may hold two
children.  A quality gate (detection score, face size, blur) matters more
than the model: Immich's 0.7 minimum score exists for the same reason.
YuNet costs 15.7 ms per full preview and **3.2 ms at half size**, which
kept 29 of 30 faces of 40 px or more.  InsightFace's published scores are
the best of the three, notably on profile faces (CFP-FP) and age gaps
(AgeDB), but they are benchmark scores, not this library; phase 0 runs it
on the labelled people set beside the other two.

### 3.4 Fursuits: finding heads

No face detector is trained on costume heads (YuNet found a few muzzles by
accident).  Three open-vocabulary detectors, asked for "a fursuit head",
"a human face", "a dog", "a cat", "a camera", on sample E1:

| Detector | Licence | GPU time (PyTorch, batch 1) | What it did |
|---|---|---|---|
| OWLv2 B/16 (chosen) | Apache-2.0 | 242 ms fp32, 120 ms fp16, + 32 ms preprocessing; CPU 1 305 ms | Boxed most heads at score ≥ 0.3; missed some profiles and very close heads.  Few human faces. |
| Grounding DINO tiny | Apache-2.0 | ~337 ms with preprocessing | Called fursuit muzzles "human face"; boxed whole bodies as heads. |
| OWL-ViT B/32 | Apache-2.0 | ~61 ms with preprocessing | Five times faster; overlapping body, head and face boxes need more filtering. |

OWLv2 gave 1 239 head crops (at least 6 % of the long edge, ~100 px) from
653 of the 769 event-day photos.  YOLO-family detectors would be faster
but have no "fursuit head" class (training one is out of scope), and
Ultralytics' models are AGPL-3.0.  Sheets: `sheets/det_*`.

### 3.5 Fursuits: grouping the same suit

Head crops embedded with DINOv2-B (CLS and mean patch token) and with
SigLIP2's image tower, then average-linkage clustering by cosine distance.
Sheets: `sheets/subj_*` (one row per group, E/D = which day).

| Embedding, cut-off | Groups of ≥ 3 | Crops in them | Of the 16 largest groups |
|---|---|---|---|
| DINOv2-B, 0.25 | 93 | 64 % | 9 one suit each (one joined across both days), 7 mixed |
| DINOv2-B, 0.35 | 49 | 85 % | one group of 377 mixed crops: too loose |
| SigLIP2, 0.15 | 78 | 81 % | 7 one suit, 2 nearly (8–9 of 10), 7 mixed |

SigLIP2 places every fursuit head close together ("a fursuit head");
DINOv2 separates individual suits better.  Mixed groups were small,
blurred, back-of-head or partly hidden heads, and two suits often
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
rarely hold the same suit on both days (one suit was joined across them);
a suit with a different outfit, a partial suit, or a head seen from
behind will often fail.  This is why fursuits stay semi-automatic, and
why phase 4 starts with its own labelled evaluation.

### 3.6 Pets

One real dog in 1 779 photos.  Search with the fursuit contrast finds it.
There is nothing to measure pet detection or grouping on, so phase 5
starts by gathering and labelling pet photos, and chooses its own
detector and embedding from that evaluation (§8).

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
these inputs, is ~1 ms.  OWLv2 on WebGPU is not measured yet (phase 4).

**The whole library, first time** (56 710 shots; reading and inference
overlap, so the slower one decides):

| What | Processor (4 threads) | Graphics card |
|---|---|---|
| Reading previews (bandwidth-bound) | ~6–7 min | ~6–7 min |
| Search, SigLIP2 B/16 | ~45 min (30 min on 8 threads) | ~7 min (reading) |
| Faces: YuNet + AuraFace (8–21 k faces) | +7–14 min | ~+3 min (estimate) |
| Faces: YuNet + SFace | +4 min | +3 min |
| Fursuit heads: OWLv2 on the ~42 k fursuit photos | ~15 h | ~1.4 h (ROCm fp16) |
| Fursuit heads: DINOv2-B on ~67 k head crops | ~35 min | ~5 min |
| For comparison, search with CLIP B/32 (not chosen) | ~9 min | ~7 min (reading) |

**Afterwards, per exported 5 000-shot day:** ~35 s of reading, then
~35 s of search on the graphics card (~4 min on the processor), and for
an event day ~7 min of fursuit heads on the card.

---

## 4. Running it from Rust

### 4.1 Runtimes

| Runtime | State (2026) | GPU on this machine | Fit |
|---|---|---|---|
| **`ort`** (ONNX Runtime 1.28) | 2.0.0-rc.13, MIT OR Apache-2.0, the de facto Rust binding to the runtime Immich uses from Python | Prebuilt **WebGPU** (Vulkan, experimental, measured above); MIGraphX needs ONNX Runtime built from source against ROCm; the ROCm provider was removed upstream in 1.23 | **Chosen.**  Any model with an ONNX export; CPU numbers equal Python's. |
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

### 4.2 The graphics card

The models run on ONNX Runtime's **WebGPU provider** (prebuilt, Vulkan
through Dawn): 5–7× faster than 8 CPU threads for these models, identical
results, no ROCm (decision 3).

- **A choice in Preferences.**  *Run models on*: *Graphics card* (the
  default) or *Processor*.  Each choice says what indexing this library
  costs on it (§7a.8).
- **Checked before use.**  A quick probe for a usable Vulkan adapter runs
  before the download dialog.  Once the search model is in place, and
  again whenever the app, the driver or a model changes, a self-check runs
  a few fixed inputs through each model on both the card and the
  processor and compares them (measured: cosine 1.000000, largest
  difference 2.7e-5; the tolerance is set in phase 0).
- **A failure is reported, never worked around.**  If the card is chosen
  and the probe or the self-check fails, or the driver brings the helper
  down, heavy work (indexing, faces, fursuit heads) stops where it is,
  keeping what is done, and a banner says what failed (§7a.4).  Nothing
  moves to the processor unless the user chooses *Processor*.
- **Queries are light.**  Encoding a query's text takes ~14 ms on the
  processor, so the helper always runs the text tower there: searching
  what is indexed works whatever the card does.
- **Experimental provider.**  `ort` marks WebGPU experimental and a driver
  fault could crash the process running it; hence the helper process
  (4.3).

Not chosen: MIGraphX (AMD's ONNX Runtime provider) needs ONNX Runtime
built from source against the distribution's ROCm; Immich's ROCm image is
35 GiB and compiles models on first use.

### 4.3 The helper process

Immich needs a service because it is a server.  A desktop app doesn't,
but a helper earns its keep (decision 4): `photo-pipeline --ml-worker`,
the same binary started with a flag, so there is nothing extra to
install.

- **A GPU driver fault** kills the helper, not the app with marks still
  queued for the saver; the app says so (4.2).
- **Memory** (models ~0.3–0.8 GB for search and faces, ~1 GB more while
  fursuit heads run) is returned to the system when the helper exits
  after a few idle minutes.
- **Priority:** `nice` and the idle I/O class for the whole helper.
- **Cancel:** cooperative, or kill.
- **Restarts:** if the helper stops while running on the card, it is
  started once more on the card; a second stop counts as the card
  failing (4.2).  On the processor it is restarted and the status bar
  says so.
- **Cost:** a pipe protocol (length-prefixed messages) and a reader thread
  turning replies into `BgEvent`s, ~300 lines.

The helper reads previews itself (the app sends paths and receives
embeddings), so image bytes don't cross the pipe.

### 4.4 Shipping models

Decision 13:

- **Never in the repository.**  Downloaded once, after the dialog
  (§7a.7) that names each model, its size and its licence.
- **Pinned:** each model has a URL, a size and a SHA-256 in
  `src/ml/models.rs`; a download goes to a temporary file, is checked, then
  renamed into `~/.cache/photo_pipeline/models/` (re-downloadable, so a
  cache).  A mismatch deletes it and says so.
- **Sources:** the publisher, or a maintained ONNX export on Hugging Face
  (Immich's `immich-app/…` for the CLIP family; community exports made
  for transformers.js for others).  A model nobody maintains as ONNX
  (OWLv2 is the likely one) is exported by us and attached to this
  repository's releases with its licence and NOTICE beside it.
- **InsightFace** comes only from InsightFace's own release
  (`buffalo_l.zip`, 289 MB, five models); the app keeps the 174 MB
  recognition model (`w600k_r50.onnx`) and deletes the rest.  It is never
  bundled with the app or re-hosted by this repository (§11).
- **Sizes** (fp32 ONNX as measured; fp16 about half, to check in phase 0):
  SigLIP2 B/16 image 372 MB + text 1 130 MB (its 256 k-token vocabulary),
  ~750 MB in fp16; YuNet 0.2 MB; AuraFace 261 MB; SFace 39 MB; InsightFace
  recognition 174 MB; OWLv2 ~600 MB; DINOv2-B 347 MB, -S 88 MB.

---

## 5. Architecture in this codebase

### 5.1 New modules

| File | Role |
|---|---|
| `src/ml/models.rs` | The model registry: id, version, URLs, SHA-256, size, licence, input size and preprocessing; the three face-model choices. |
| `src/ml/download.rs` | Download on a worker thread with progress (`BgEvent::ModelDownload`), check, rename into place; unpack InsightFace's archive and keep its one file. |
| `src/ml/worker.rs` | The helper process's main loop: `ort` sessions on the chosen device, the probe and self-check, batching, idle exit. |
| `src/ml/client.rs` | Starts the helper, the pipe protocol, the restart rule (4.3), card failures reported as `BgEvent`s. |
| `src/ml/prep.rs` | Preview → model input: DCT-scaled decode (`imaging/decode.rs`), resize (`fast_image_resize`, already a dependency), normalise; RAW-only shots through `Raw::thumbnail_at_least`. |
| `src/search/index.rs` | Content keys, the path map, capture info, the vector file (§6). |
| `src/search/pass.rs` | The indexing pass, modelled on `analysis.rs`: its steps, jobs, cancel, yielding to visible thumbnails, network errors through `ops::netfs::classify_error`. |
| `src/search/query.rs` | Parsing (§7.2), text encoding, scoring, the closeness cut-off, tags, animal words, burst stacks.  Pure apart from the text encoder; unit-tested. |
| `src/search/vocab.rs` | The hidden-tag vocabulary (generic words only) and the animal words that leave fursuits out. |
| `src/model/subjects.rs` | Detections, grouping per kind (pure, testable), suggestions, clearest-first order, the user's subjects file (§6.5); refuses anything that crosses kinds. |
| `src/app/search.rs`, `src/app/subjects.rs` | App state and views: the search page, completions and results; the *Subjects* page with its tabs, a subject's page, the review. |
| `src/app/commands.rs` | `Cmd::Search` (Ctrl+F) and `Cmd::Subjects` (Ctrl+4), in `SIDE` and `MENUS`. |
| `src/ui/subject_tile.rs` | Paints a subject tile (crop, name, count) for a grid of subjects. |

### 5.2 Data flow and threading

1. **Library changes.**  The calendar's index job already sends
   `BgEvent::IndexReady { done: true }` after a check, including the one
   `mark_stale()` triggers after an export.  `search::on_library(index)`
   diffs the library's files (path, size, mtime) against the path map, on
   a worker: unchanged files cost nothing; new or changed ones become jobs.
2. **The pass.**  The first full index starts only when asked; after
   that each export's new files are queued on their own (decision 5).
   Per shot, the JPG if paired, else the RAW.  Jobs are ordered newest
   day first and sent to the helper as paths.  The helper reads previews
   with the app's own code (`exif::jpeg_preview_bytes`, LibRaw) on 8
   reader threads (the share is bandwidth-bound), decodes on the reader
   and runs batches of 16–32, in two steps:
   - **search and faces**, one read of each preview: the SigLIP2
     embedding and, with *Find people's faces* on, YuNet and the chosen
     face model;
   - **fursuit heads** (phase 4), on the photos whose hidden tags say
     fursuit: OWLv2 and DINOv2, reading those previews again (about
     15 GB for the library, spread over the step's ~1.4 h).

   The app pauses the job stream while visible thumbnails are queued, as
   the analysis pass yields.  Results come back as `BgEvent::Search { run,
   batch }`; the UI thread only updates counts, and the index is written
   by the pass's own thread.  If the card fails, the pass stops and keeps
   what is done (4.2).
3. **Catching up.**  A step switched on later, or shipped after the
   library was indexed (faces in phase 3, fursuit heads in phase 4, pets
   in phase 5), runs over the photos already indexed when the user asks,
   from its tab or Preferences, with its cost shown first, like the first
   index.  New exports include it from then on.
4. **A query** never touches the share: text → embedding on the
   processor (~14 ms), scores against the vectors in memory (~3 ms), the
   closeness cut-off, the tag boost, the animal-word contrast, burst
   stacks, best first.  Runs off the UI thread; the results appear when
   it returns.
5. **Subjects:** detections and their embeddings go into the subjects
   cache, per kind; grouping is recomputed on a worker when they change.

---

## 6. Index design

### 6.1 Where it lives

- Recomputable data: `~/.cache/photo_pipeline/search/` (embeddings,
  detections, tags, capture info, crops for tiles).  Shown and cleared in
  Preferences ▸ Storage like the other caches.
- The user's own data (names, merges, "not this one"):
  `~/.local/share/photo-pipeline/subjects.json`, next to `unsaved/`.  Never
  cleared with the caches.
- Recent searches (the last 10) are kept with the app's settings, not in
  a cache; *Forget them* in Preferences clears them.
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
shots), so a pair is one result.  From the same head the pass keeps each
shot's capture time, sub-seconds and sequence number (`exif::read_shot`),
so results stack bursts the way the grid does (`model/bursts.rs`).

### 6.3 Incremental updates

- **New or changed files** (after an export, `mark_stale()` → check →
  diff): head read → key → if known, map only; else queue.
- **Deleted files:** their path entries go when their folder is re-read.
  Vectors with no path left are dropped when the file is compacted (at
  most once a session), so a file moved out and back costs nothing.
- **A new model or preprocessing:** every vector file records its model
  id, weights SHA-256 and a preprocessing version; a mismatch means a new
  file built beside the old, which keeps answering queries until the new
  one is complete.  A change of face model is this case, for the face
  vectors only (§6.5).
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
| Search vectors, SigLIP2 (768-d f16) | 87 MB |
| Keys, path map and capture info | ~7 MB |
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
      "kind": "person",               // person | fursuit | pet
      "hidden": false,
      "cover": { "key": "…", "box": [0.412, 0.188, 0.506, 0.331] },
      "members":  [ { "key": "…", "box": [ … ] } ],   // confirmed
      "rejected": [ { "key": "…", "box": [ … ] } ]    // "not this one"
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

- **Three kinds, never mixed.**  People come from the face step,
  fursuits from the head step, pets from their own step (phase 5).  Each
  kind has its own detections, vectors, cut-offs and groups.  A merge, a
  move, a suggestion or a completion never crosses kinds:
  `model/subjects.rs` refuses it, and a unit test holds it.
- **Changing the face model keeps the user's work.**  Members, rejections
  and covers point at detections, and YuNet finds the same faces whichever
  recognition model is chosen.  Only the face vectors, the unnamed groups
  and the suggestions are rebuilt: the photos with faces (about a quarter
  of the library, ~4.5 GB) are read again and their 8–21 k faces
  recognised, a few minutes on the graphics card, 5–15 on the processor.
  The old vectors keep answering until the new ones are complete.

---

## 7. Search and subjects in the app

*This section sets the rules the screens follow; §7a draws the screens.*

### 7.1 Design direction

The new pages are library pages, so they wear the calendar's chrome and
keep photos on neutral grey:

- **Colour:** no new tokens.  Header and panels `MOUNT`, fields `RAISED`,
  text `INK`, captions `INK3`; results and subject crops sit on
  `PHOTO_GROUND` / `PHOTO_MOUNT`, never tinted.  `SIGNAL` purple only for
  focus, the selection and the main action; `KEEP` and `REJECT` only for
  the review's Yes and No, as in culling.
- **Type:** Barlow throughout.  Page titles `T_HEAD` semibold (the query
  itself is the title of the search page), subject names `T_BODY` medium,
  counts `T_SMALL` in `INK3`.
- **Layout:** the calendar's 60 px header; content on the 4 px scale.
- **The one memorable thing:** reviewing a subject feels like culling.
  Suggestions come one at a time, large, with **Y** and **N** where P and
  M would be, and advance after each answer.  Everything else stays quiet.
- **Hidden means hidden:** no tag chips on photos, no badges for subjects.

### 7.2 Queries and ranking

**Query syntax** (plain, all optional, combined with AND):

| Write | Means |
|---|---|
| `red car at night` | photos that look like the words |
| `"a red car at night"` | the words exactly as the prompt |
| `fireworks -crowd` | like "fireworks", and more like it than like "crowd" |
| `@` and a name | a chip: photos of that person, fursuit or pet; words beside it rank them |
| `2024`, `jun 2024`, `apr–jun 2024` | only those dates |

- **Best first** (decision 7): one list in score order, no grouping by
  day.  Each photo's date sits under it (§7a.3).
- **Closeness** (UI 4): a result needs a score above a threshold relative
  to the query's own spread (scores aren't comparable between queries).
  Phase 0 sets the range and the default per model (the default: good
  matches, what works for most people); Preferences ▸ Search, *How close
  matches must be*, is a slider over that range with a small reset button
  back to the default.  The search page has no control for it.
- **Ranking:** photos carrying the query word as a hidden tag first
  (precise), then the rest by similarity.  Accidental frames left out.
- **Animal words** (12b): when the vocabulary marks a word as a real
  animal that fursuits are taken for (dog, cat, bird, fox, wolf…) and the
  query doesn't name fursuits, a photo counts only where the word beats
  "fursuit" (the measured contrast).  The fursuits left out are counted;
  *Show fursuits too* shows them as a section of their own after the real
  animals, never in their list.  "dog fursuit" or "fursuit" asks for suits.
- **Bursts** (UI 6): frames of one burst (presses, found as the grid finds
  them from the capture info) stack into one result at the rank of their
  best frame, which is the cover.  Expanded, the frames follow it in
  shooting order.
- **A subject's page** (UI 12): with no words typed, its photos come
  clearest first: by how sure the match is and how large and sharp the
  face or head is, so doubtful members sink to the bottom, where wrong
  ones are easy to spot and move out.  With words, best first by the
  words.

### 7.3 Pairs, RAW-only shots and the library

- One result per shot; the pair's tag shows both halves.
- RAW-only shots use the RAW's embedded preview: the smallest of at least
  300 px for search, at least 1 000 px for faces and heads.
- **Library only** (decision 14): results are read-only, like the
  calendar's days; P, M and U do nothing there.  Searching a working
  folder is deferred.

---

## 7a. Interface design

*The screens were built as interactive mockups in the app's own tokens
(Barlow, Catppuccin chrome, neutral `PHOTO_*` greys, made-up data and grey
placeholder photos) and reviewed with the user.  This section records
them, updated to the decisions at the top.  Names, dates, counts and file
names in the wireframes are made up.*

### 7a.1 Principles

- **Search is a place.**  A page of its own on the side rail, like List,
  Grid and the calendar: it keeps its query while you work elsewhere, and
  its back button names where it returns.
- **So are subjects.**  People, fursuits and pets share one rail entry,
  *Subjects*, next to Search, with a tab per kind.
- **Results are a ranked list.**  Best first, in the calendar's read-only
  grid, so quick look, the viewer, the RAW+JPG tag, one result per pair
  and burst stacks work as they do there.  Each photo's date sits under
  it.
- **Words before widgets.**  One field.  The syntax (`-word`, `@name`,
  dates) is taught by the completions and their footer, not by filter
  panels.  How close matches must be lives in Preferences.
- **Hidden means hidden.**  No tags or badges on photos; the vocabulary
  shows only as completions and the *Things* list.
- **Ask once, then stay quiet.**  One dialog before the first index lists
  every download, size, licence and the time it takes.  After that,
  indexing is a status-bar line.
- **Say what failed; never work around it.**  A failure stops the work
  and says why in the app's existing banner.  Nothing moves to the
  processor unless it is chosen in Preferences.
- **Three kinds, never mixed.**  People, fursuits and pets each have their
  own tab, groups, suggestions and completions heading.
- **Reviewing feels like culling** (the one memorable screen).  One
  suggestion at a time, large, with Y and N where P and M would be,
  stamped and advanced the way marks are.
- **Strangers stay out of sight.**  Faces are on by default, so nothing is
  named for you, groups under the minimum fold away, unnamed groups are
  never offered as completions, and everything found can be forgotten in
  one place.

### 7a.2 Getting there and back

- **Rail and menu.**  Two new commands, placed in `SIDE` under *Library
  calendar* and in the View menu after it, so the KDE global menu and the
  shortcuts dialog follow:
  - `Cmd::Search`: "Search the library", Ctrl+F, a magnifier glyph;
  - `Cmd::Subjects`: "People, fursuits and pets", Ctrl+4, a glyph of a
    person's head beside a fursuit's.
- **Pointed out quietly** (UI 2): the rail, the View menu and Ctrl+F; the
  page explains itself.  No dot, tip or status line.
- **Ctrl+F** from anywhere in the main window opens Search with the field
  focused and its text selected, so typing replaces the last query.
- **Back.**  The header's first button names where it returns ("‹ Grid",
  "‹ Jun 2024"), like the day page's "‹ Jul 2026".  **Esc never leaves the
  search page** (UI 8): it closes the completions when they are open and
  otherwise does nothing; the back button and the rail leave.  P, M and the
  rail's marking commands do nothing on the search page, as on the
  calendar's days.
- **Open day.**  *Open day ›* in the header acts on the selected result:
  the calendar's page for that day with the photo selected.  Its back
  button then reads "‹ Search" and returns to the same results and scroll
  position.

### 7a.3 The search page

**Header** (60 px, `MOUNT`): the back button, a larger field (36 px, the
query in `T_TITLE` medium: the query is the page's title), and the count.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Grid │ ⌕ cameras                                      × │  Open day ›  46  │
├──────────────────────────────────────────────────────────────────────────────┤
```

**Start page** (nothing typed), on `PHOTO_GROUND`:

```
│ Recent searches ─────────────────────────────────────────────────────────────│
│   fireworks   @Fursuit 1   dog   cameras jun 2024                            │
│ Things in your photos ───────────────────────────────────────────────────────│
│   fursuits 412   people 380   cameras 96   fireworks 24   food 21   …        │
│ Ways to search ──────────────────────────────────────────────────────────────│
│   red car at night    photos that look like the words                        │
│   fireworks -crowd    leave a word out                                       │
│   @Fursuit 1          a person, fursuit or pet you named                     │
│   fireworks 2024      only those dates (jun 2024, apr–jun 2024)              │
```

- **Recent searches** (UI 11): the last 10; a click runs one again.
  *Forget them* is in Preferences.
- A word in *Things* starts a search for it.

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
           │                              fursuit or pet          │
           └──────────────────────────────────────────────────────┘
```

- Empty field: recent searches.  `@`: named subjects only, under
  *People*, *Fursuits* and *Pets* headings, never one list; unnamed
  groups are never offered.  `-`: things to leave out.
- ↑ ↓ choose; Enter or Tab takes the choice; Enter with none chosen
  searches what was typed and moves to the results.  A name becomes a
  chip in the field, with its crop (UI 5), so names with spaces stay
  whole; Backspace in an empty field removes the last chip.

**Results** (`ui::grid::Grid`, one section, best first):

```
│ Best matches first ──────────────────────────────────────────────────────────│
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
│  │          │  │          │  │  ▤ 4     │  │          │  │          │        │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘  └──────────┘        │
│  Oct 26, 2024  Dec 31, 2024  Oct 26, 2024  Mar 3, 2023   May 18, 2025        │
│                                                                              │
│                That's everything that looks like "cameras".                  │
```

- **Best first** (decision 7): one section, "Best matches first", in
  score order; no date headings and no order switch.  The strip under
  each tile, which shows file names in the grid, shows the photo's date;
  hovering shows the date, the time and the file name.
- **Bursts** (UI 6): a burst is one stacked tile (the third above), at the
  rank of its best frame, which is its cover.  G shows its frames in place
  or folds them back, Shift+G every burst, as in the grid.
- **The end:** "That's everything that looks like "cameras"."  How close
  a match must be is set in Preferences (UI 4); the page has no button or
  slider for it.
- **Animal words** (12b, UI 3): fursuits are left out by default; a quiet
  line at the end says how many, with *Show fursuits too*.  Shown, they
  get a section of their own after the real animals ("Fursuits that look
  like dogs", with *Hide fursuits*), never mixed into the real animals'
  list.
- **Nothing found:** "Nothing in the library looks like "zebra".  Try
  another word, or fewer words.  Search goes by what a photo shows, not
  by file names or folder names."
- **Keys** as on a day page: arrows move, Space quick look (read-only: no
  P, M or U), Enter opens the viewer read-only, G and Shift+G stack; Esc
  closes the completions and nothing else.

### 7a.4 Indexing, as the user sees it

**Not indexed yet.**  The search page's body invites the first index and
says what it costs, in the "Not analysed yet" pattern of the Hints menu:

```
│        ⌕  Search the library by what's in your photos                        │
│        Type what you're looking for … In the same pass it finds faces, so    │
│        you can name people and search for them.                              │
│        ┌──────────────────────────────────────────────────────────────┐      │
│        │ Reads      Each photo's preview once: about 20 GB from the   │      │
│        │            library on the share                              │      │
│        │ Takes      About 10 minutes on the graphics card (search 7,  │      │
│        │            faces 3), then about 1.4 hours for fursuit heads. │      │
│        │            Search works within a minute.                     │      │
│        │ Downloads  Search 750 MB, faces 37–290 MB (you choose),      │      │
│        │            fursuits 950 MB                                   │      │
│        │ Keeps      About 210 MB, on this computer, only you can read │      │
│        └──────────────────────────────────────────────────────────────┘      │
│        After this, each export is added on its own.  Faces are never named   │
│        for you; finding them can be turned off in Preferences.               │
│        [ Index the library… ]                                                │
```

- Each line appears with its phase: phase 1 offers search alone (about
  95 MB kept); faces join in phase 3, fursuits in phase 4.
- *Takes* follows *Run models on*.  With *Processor* chosen it reads
  "About an hour on the processor (search 45, faces up to 15), then about
  15 hours for fursuit heads", and *Find fursuits* starts unticked in the
  dialog.
- With *Graphics card* chosen and no card it can use, the body says so
  instead of offering the index: "There's no graphics card Photo Pipeline
  can use, so nothing is indexed.  To index on the processor instead
  (about an hour for this library), choose it in Preferences ▸ Search."
  *Open Preferences*.

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
                │   About 1 h 25 min on the graphics card.             │
                ├──────────────────────────────────────────────────────┤
                │ Search works on whatever is done.                    │
                └──────────────────────────────────────────────────────┘
 Indexing: search and faces 18,200 of 56,710 ▓▓░░ ×                (status bar)
```

Searching while it runs shows results so far, under a strip: "Searching
the 18,200 photos indexed so far, newest first.  Results fill in as
indexing reaches older days."

**After an export** (quiet): no dialog and no activity line, only the
status bar: "Exported 412 photos to the library." then "Adding them to
search 120 of 412", "Finding fursuit heads in them 80 of 310", and
finally "Search has the 412 new photos."

**The library stops answering:** the analysis banner's pattern
(`WARN` strip): "Indexing stopped: the library on nas stopped answering.
18,200 of 56,710 photos were indexed and are kept, and search works on
those.  Indexing carries on from there when the library is back."
*Try again*, ×.

**The graphics card fails** (decision 3).  A banner under the folder bar,
in whichever view is open (like the network banner), since indexing runs
while the user culls:

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ▣ Indexing stopped: the graphics card stopped responding                     │
│   Its driver stopped the search helper.  Search still works on the 18,200    │
│   photos indexed so far.  Nothing is moved to the processor: to index on     │
│   it instead, choose it in Preferences ▸ Search.                             │
│                                      [ Try the card again ]  Preferences  ×  │
└──────────────────────────────────────────────────────────────────────────────┘
```

- *Self-check failed:* "Indexing stopped: the graphics card gave
  different answers from the processor, so it isn't used."
- *No usable card:* "Indexing stopped: there's no graphics card Photo
  Pipeline can use."
- The same banner serves faces and fursuit heads; no step offers the
  processor by itself.
- × leaves the work stopped; the status bar keeps "Indexing stopped:
  graphics card" with the same actions as a link.  New exports wait,
  counted: "412 new photos wait for the graphics card."
- *The helper stops on the processor:* it is restarted and the status bar
  says "The search helper stopped and was started again.  Indexing
  carried on."  No banner.

### 7a.5 People, fursuits and pets

- **One rail entry, three tabs** (UI 1).  *Subjects* opens on the tab last
  used; *People | Fursuits | Pets* sit in its header.  A tab shows one kind
  only, so a merge, a move or a suggestion can never cross kinds;
  completions list each kind under its own heading.
- **Shapes:** people are round crops of the face; fursuits and pets
  rounded squares, so ears and horns fit.  Crops sit on `PHOTO_MOUNT`;
  the kind is told by the tab and the shape, never by colour.
- **Glyphs with a furry look** (UI 10): the fursuit glyph is a head with
  full ears and a muzzle, with a paw print where it helps at small sizes.
  The pet glyph is a different shape (a whole animal, sitting), so the
  two kinds never look alike.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Fursuits                                   People│Fursuits│Pets    ☐ Select  │
│ 3 named, 5 groups to name                                                    │
├──────────────────────────────────────────────────────────────────────────────┤
│ Named  3 ────────────────────────────────────────────────────────────────────│
│   ┌──────┐     ┌──────┐     ┌──────┐                                         │
│   └──────┘     └──────┘     └──────┘                                         │
│  Fursuit 1    Fursuit 2    Fursuit 3                                         │
│   96 photos    54 photos    31 photos                                        │
│ Groups to name  5, largest first ────────────────────────────────────────────│
│   ┌──────┐     ┌──────┐   …                                                  │
│  Name this    Name this                                                      │
│  fursuit      fursuit                                                        │
│ › Smaller groups 230     › Hidden 1                                          │
```

- **Right after the first index** the People tab holds only groups to
  name (none named yet) and *Smaller groups*; the Fursuits tab shows the
  head step's progress, with groups appearing as they are found.
- **Before a kind's step has run** on the library (it shipped later, or
  was off), its tab invites it with its cost, as the search page does:
  *Find faces in the 56,710 photos indexed*.  With faces off: "Finding
  faces is off.  Turn it on in Preferences."
- **A subject's page is a search for it**: the subject is a chip in the
  field (words added narrow it, best first), and a band under the header
  holds the subject.  With no words, its photos come clearest first
  (UI 12).

```
│ ‹ Fursuits │ ⌕ [▢ Fursuit 1 ×] Add words                   │   96 photos     │
├──────────────────────────────────────────────────────────────────────────────┤
│ ▢ Fursuit 1 ✎          Photos│Heads   Find more  [Review 24 suggestions Y] ⋯ │
│   96 photos, clearest first.  24 more photos may show this fursuit.          │
├──────────────────────────────────────────────────────────────────────────────┤
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
```

- **Naming.**  An unnamed group opens with "Name this fursuit" focused in
  the band.  Typing a name that already exists offers *Merge into
  Fursuit 1* first and *Name it "…"* second; Enter takes the highlighted
  one.  *Check this group (Y)* reviews the group's own members ("Same
  fursuit as this group?") before or after naming.  F2 or a click on the
  name renames.
- **Merging.**  Ctrl+click or S picks several; a strip says "2 selected"
  with *Merge…*, *Hide* and ×.  The dialog asks which name to keep (the
  named one by default) and says Ctrl+Z undoes it.
- **Splitting.**  *Photos | Heads* (or *Faces*) shows every crop, clearest
  first; picked crops go *Move to…* another subject of the same kind or a
  new one, or *Not Fursuit 1*, which also stops them being suggested.
- **Hiding** takes a subject off its tab and out of completions; its
  photos still come up when searched by name.  The status bar says so
  with *Undo*; *Hidden* lists them with *Show*.
- **⋯** holds *Merge into…*, *Change the cover*, *Show where* (outlines
  the subject on each photo), *Hide* and *Forget this fursuit…* (or
  person, or pet).
- **Pets** (phase 5): the same page pattern on their own data, found by
  their own step, only on photos the index tags as animals and not as
  fursuits.  Until the phase ships, the Pets tab says "Pets are coming
  later" and that search finds animals by word meanwhile.

### 7a.6 "Same fursuit?": the review

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ‹ Fursuit 1 │ Same fursuit?              ▓▓▓░░░░░░  Auto-advance ●   [Done]  │
│             │ 7 of 24 suggestions. 5 added, 1 not.                           │
├──────────────────────────────────────────────────────────────────────────────┤
│  Fursuit 1, from            ┌────────────────┐       In the photo            │
│  96 photos                  │                │       ┌───────────────────┐   │
│  ┌────────┐                 │  the suggested │       │    ┌────┐         │   │
│  │ cover  │                 │  crop, 300 px  │       │    │box │         │   │
│  └────────┘                 │                │       │    └────┘         │   │
│  ▫ ▫ ▫   ▫ ▫ ▫              └────────────────┘       └───────────────────┘   │
│                           [ ✓ Yes  Y ] [ ✕ No  N ]   20230303_1412_0815      │
│                   A burst of 4 frames: the answer covers them all.           │
├──────────────────────────────────────────────────────────────────────────────┤
│ ▫✓ ▫✓ ▫✓ ▫✕ ▫✓ ▫✓ [▫] ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫ ▫                                  │
│ Answers go to Fursuit 1 at once.      Y Yes  N No  ← → Step  Enter  Esc Done │
└──────────────────────────────────────────────────────────────────────────────┘
```

- Left: what the subject looks like (its cover and confirmed crops).
  Centre: the suggestion, large.  Right: the whole photo with the head's
  box outlined (light line, dark halo; never the accent, which means
  selection).
- **Y** adds it, **N** records "not this fursuit" so it is never offered
  again.  The answer is stamped as marks are (UI 7): a green check on the
  crop's corner (`KEEP`), or a grease-pencil cross with the crop dimmed
  (`REJECT`).  Buttons use `style::toggle` in those tones.
- With *Advance after marking* on (the same setting, shown as
  *Auto-advance*), it moves on after the same short beat; ← → (or A D)
  step; Enter opens the photo in the viewer; Ctrl+Z takes the last
  answer back; Esc stops the review and the status bar says "Added 5
  photos to Fursuit 1.  1 photo won't be suggested again."
- The strip along the bottom is the viewer's filmstrip pattern: answered
  suggestions carry their stamp, the current one the accent frame; click
  to go back to one.
- **One suggestion per burst** (UI 6); its answer covers every frame.
- **The end:** "All 24 answered.  18 photos added to Fursuit 1.  6
  won't be suggested again."  *Find more* (strict, a few at a time) and
  *Back to Fursuit 1*.
- The same screen serves people ("Same person?") and, in phase 5, pets
  ("Same pet?"), each on its own kind's data.

### 7a.7 The download dialog

It opens from *Index the library…* (search is pointed out quietly, UI 2,
so this is the first thing that asks anything):

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
│ │   The most accurate in tests here.  The slowest: about 3 minutes │
│ │   for this library on the graphics card.  Apache-2.0.            │
│ │ ○ SFace                                                    37 MB │
│ │   The smallest and fastest.  Mixed people up more often in       │
│ │   tests.  Apache-2.0.                                            │
│ │ ○ InsightFace, what Immich uses                           290 MB │
│ │   Best published scores; not tested here.  Licence: for          │
│ │   non-commercial research only, and it binds you if you pick     │
│ │   it.  From InsightFace; 174 MB is kept.                         │
│ ☑ Find fursuits   OWLv2 and DINOv2                          950 MB │
│ │   About 1.4 hours for this library, then 7 minutes a new day.    │
│ Downloads  1.96 GB in all, once                                    │
│ Reads      About 20 GB from the library on the share               │
│ Takes      About 10 minutes on the graphics card, then about 1.4   │
│            hours for fursuit heads                                 │
│ Each file is checked against its SHA-256 before it's used, and     │
│ deleted if it doesn't match.        Show sources and checksums     │
│                         [ Cancel ]  [ Download 1.96 GB and index ] │
└────────────────────────────────────────────────────────────────────┘
```

- **Rows come with their phases:** phase 1's dialog holds search only;
  faces join in phase 3, fursuits in phase 4, pets in phase 5.  A step
  that ships later asks again, from its tab, with only its own download.
- Search is fixed; faces and fursuits are on, each with a box to turn it
  off; totals and times update with each choice.  *Takes* follows *Run
  models on* (§7a.4).
- **Face model** (decision 10): AuraFace is pre-selected until phase 0
  says otherwise; InsightFace never is, and its licence line is in
  `WARN`.  YuNet (0.2 MB, MIT) finds faces for all three, so changing
  the model later keeps the faces and their names.  InsightFace's speed
  is an estimate until phase 0 measures it.
- *Show sources and checksums* lists each file's source and SHA-256.
- **Downloading:** one row per model with a bar, then "Checked".  The
  card's self-check runs as soon as the search model is in place, and
  indexing starts when it passes; the dialog can be closed and downloads
  carry on, with progress in the status bar.  If the self-check fails,
  the models are kept, nothing is indexed, and the banner (§7a.4) says
  why.
- **A checksum mismatch:** "The face model didn't match its checksum.
  AuraFace downloaded, but it isn't the file Photo Pipeline expects, so it
  was deleted and nothing was installed …  Search and fursuits are fine;
  indexing goes ahead without faces."  Expected and received digests;
  *Not now*, *Try again*.
- **Offline:** "Couldn't reach the model's publisher.  Nothing was
  downloaded or indexed.  Try again when you're online."

### 7a.8 Preferences

New sections in the existing Preferences window, in its card-and-row
pattern:

- **Search:**
  - *Run models on*: *Graphics card* (the default) or *Processor*.
    *Graphics card* names the device and the last self-check ("Checked
    against the processor: same results"), with *Check again*.
    *Processor* says what it costs: "Indexing this library: about 45
    minutes for search, up to 15 more for faces, about 15 hours for
    fursuit heads.  Each new export: about 4 minutes a day."
  - *Index new exports* (on).
  - *How close matches must be*: a slider from stricter to looser, with a
    small reset button to the default (good matches).
  - *Search model*: SigLIP 2, shown as fixed.
  - *Recent searches*, with *Forget them*.
- **People:** *Find people's faces* (on).  Turning it off offers, inline:
  "No new faces will be looked for.  Forget the 8,412 faces already
  found, and the 2 names you gave?"  *Keep them* / *Forget them*.  *Face
  model* with *Change…* (the dialog's chooser, saying how long finding
  faces again takes and that names, confirmed photos and "not this
  person" answers are kept).  *Smallest group to show*: 3, 5 or 10
  photos.
- **Fursuits:** *Find fursuits* (on).
- **Pets:** *Find pets*, "Coming later" until phase 5.
- **Storage**, new rows: *Search index* (92 MB, 56,710 photos;
  *Rebuild…*, *Clear…*); *People, fursuits and pets* ("not a cache: it
  asks first"; *Forget all…*); *Downloaded models* (1.96 GB; *Open*,
  *Delete…*).  Each action confirms in the window, with what it costs:
  "Rebuild the search index?  Every photo in the library is read again:
  about 20 GB, about 7 minutes on the graphics card.  Search keeps
  using the current index until the new one is ready."

### 7a.9 New keys

| Key | Where | Does |
|---|---|---|
| Ctrl+F | Main window, anywhere | Opens Search, field focused, its text selected |
| Ctrl+4 | Main window, anywhere | Opens *Subjects* on its last tab |
| ↑ ↓, Enter, Tab | Search field | Choose and take a completion; Enter alone searches and moves to the results |
| Backspace | Empty search field | Removes the last name chip |
| Esc | Search page | Closes the completions; never leaves the page |
| Arrows, Space, Enter | Results | As on a day page: move, quick look, viewer (read-only) |
| G, Shift+G | Results | Expand or fold a burst's stack; every burst |
| F2 | A kind's tab; a subject's page | Name or rename |
| S, Ctrl+click | A kind's tab | Pick several, to merge or hide |
| Y | A subject's page | Start reviewing its suggestions |
| Y, N | Review | Yes, same; no, not this one |
| ← → (A, D), Enter, Esc | Review | Step, open the photo, stop the review |
| Ctrl+Z | Review | Take the last answer back |

Checked against `src/app/commands.rs` and the view key handlers:

- **Ctrl+F** is free: `Cmd::from_key` binds no Ctrl+F.  Plain F (fit)
  belongs to the viewer and compare, in their own window, and is not
  touched.
- **Ctrl+4** is free: Ctrl+1–3 are List, Grid and Library calendar, and
  *Subjects* follows them as the fourth place.
- **Y and N** are unbound without modifiers everywhere.  Ctrl+Y stays
  Redo; the review checks for no modifier.
- **F2** is unbound.
- **G and Shift+G** are the existing *Stack or expand burst* and *Stack
  all bursts* commands, with their usual meaning on results.
- **S** is the existing *Select mode* command (today disabled only in
  the calendar view), enabled on the kinds' tabs with its usual meaning.
- Arrows, A and D, Space, Enter and Esc keep the meanings they have in
  quick look and the viewer, except that Esc never leaves the search
  page.  No new single-letter command is added to `Cmd`.

### 7a.10 What it reuses

| Existing piece | Reused for | Change needed |
|---|---|---|
| `ui::grid::Grid` and its `Section`s | Results; a subject's photos; the fursuits section | A section without a date (`Section` is keyed by `NaiveDate`): a label-only section or a sentinel date.  The name strip under each tile shows the date instead. |
| The grid's burst stacks (`model/bursts.rs`, *Stack or expand burst*) | Bursts in results | Presses found from the index's capture info rather than a folder's |
| The calendar's day page | *Open day* | Its back button learns a "Search" origin |
| Quick look, read-only viewer (`read_only`) | Space and Enter on results | None beyond the read-only path the calendar uses |
| Hints menu (`hints_ui.rs`): floating rows with title, detail, count and a footer | Completions; the indexing steps; the "Not indexed yet" invitation (its "Not analysed yet" body) | A row variant with a crop for names |
| Analysis progress in the status bar, with × | Indexing progress | Clickable, to open the steps |
| The analysis and network banners (`WARN` strip) | Library away; the graphics-card error | None |
| The duplicates strip (`copies_bar`) | "2 selected" strips for merge, hide and heads | None |
| The bulk-action confirmation (`bulk.rs`): count, the photos, why | Merge (crops and counts), Forget all, Rebuild, Clear | None |
| `commands.rs` `SIDE` and `MENUS` | `Cmd::Search`, `Cmd::Subjects` | Two entries; the global menu and shortcuts follow |
| `kit::segmented`, `kit::btn`, `kit::tip`, `style::input`, `style::toggle` | People│Fursuits│Pets, *Run models on*, buttons, the field, Yes and No | A 36 px field size |
| Viewer filmstrip (`ui::filmstrip`) and the keep check and grease-pencil cross | The review's strip and stamps | Square crops instead of thumbnails |
| *Advance after marking* (`viewer_advance.rs`) and `ui::stamp` | The review's advance and its beat | None |
| `undo.rs` | Answers, merges, hides, names | A subjects entry kind; it outlives a folder change |
| Preferences Storage rows (`preferences_storage.rs`) | Search index, people, models | Three rows, measured on the worker |

New pieces: `ui/subject_tile.rs` (§5.1: crop, name, count in a mount;
round or rounded square) and glyphs drawn in `ui/icons.rs` with the same
stroke: *Search* (magnifier), *Subjects* (a person's head beside a
fursuit's), *Fursuit* (full ears and a muzzle; a paw print where it
helps), *Pet* (a whole animal, sitting), *Merge*, *Split*, *Hide* (eye
crossed out) and *Graphics card*.

### 7a.11 States

| State | Where | What the user sees |
|---|---|---|
| Nothing typed | Search page | The start page (§7a.3) |
| No completion | Completions | "Press Enter to search for "…"" |
| No name matches `@…` | Completions | "No one by that name yet" |
| Nothing found | Results | "Nothing in the library looks like "…"" with what to try |
| End of results | Results | "That's everything that looks like …" |
| Animal word | End of results | "N fursuits that look like dogs are left out" and *Show fursuits too* |
| Not indexed | Search page | The invitation with its costs |
| No usable card, card chosen | Search page | Why, and *Open Preferences* to choose the processor |
| First index running | Status bar, steps, strip on results | Progress with ×; results so far |
| After an export | Status bar only | Quiet progress, then "Search has the N new photos" |
| Library away | Banner | Kept, stopped, *Try again* |
| Graphics card failed | Banner in any view | What failed; *Try the card again*, *Preferences*; nothing moves to the processor |
| Helper restarted (processor) | Status bar | One line; work carries on |
| Download mismatch, offline | Dialog | Which file, what still works, *Try again* |
| A step not yet run on the library | Its tab | The invitation with its cost |
| Faces off | People tab | "Finding faces is off.  Turn it on in Preferences." |
| Heads still scanning | Fursuits tab | The step's progress; groups as found |
| Pets before phase 5 | Pets tab | "Pets are coming later" |

---

## 8. Phased rollout

Rough effort in focused working days for someone who knows the codebase.

| Phase | Content | Effort |
|---|---|---|
| **0. Evaluation** | A labelled set from scratch copies: ~30 queries with their right answers on a 2 000-photo sample; ~10 people named on sample L.  An ignored test that reports precision at 10/50, recall and group purity.  Confirms SigLIP2 and fixes the prompt template, the three closeness steps, the animal-word list, fp16/i8 storage and the self-check's tolerance.  Runs AuraFace, InsightFace and SFace on the people set: fixes the pre-selected face model and InsightFace's real size and speed. | 1.5–2 d + ~1–2 h of the user's labelling |
| **1. Search** | `ml` (registry, download dialog with checks, helper process, `ort` WebGPU and CPU, the probe and self-check, *Run models on*, the card-failure banner, never a silent move to the processor), `search/index` (keys, path map, capture info, vectors), the pass (first index when asked, exports on their own after, cancel, network banner), `Cmd::Search` and Ctrl+F, the search page and start page, best-first results with dates and burst stacks, *Open day*, *How close matches must be*, recent searches, empty states, Storage rows, bench and scripted UI tests, `PP_PROFILE` check. | 7–9 d |
| **2. Tags and syntax** | The vocabulary and derived tags, completions, the ranking boost, `-word` and dates, animal words with fursuits left out and their own section, *Things*. | 2–3 d |
| **3. People** | YuNet and the chosen face model (the dialog's choice; *Change…* rebuilding face vectors and keeping names), faces in the pass (on by default) and the catch-up for an indexed library (asked), grouping (incremental assignment, full regroup on a worker), `Cmd::Subjects` and Ctrl+4 with the People tab, a subject's page (clearest first, `@` chips), naming, the review with Y/N (one answer per burst), merge, split, hide, *Forget all*. | 7–9 d |
| **4. Fursuits** | Starts with its own evaluation: ~15 fursuits labelled across two event days (~1 h of the user's time), purity and "more like this" against the cut-offs.  Then OWLv2 export to ONNX and its cost on WebGPU (and on the processor, for Preferences' estimate), the head step after search and faces on fursuit-tagged photos (first index, catch-up, each export), DINOv2 crops, strict *Find more*, the Fursuits tab and "Same fursuit?", the furry glyphs. | 5–7 d |
| **5. Pets** | Starts by gathering and labelling real pet photos (the user's), then evaluates animal detection and grouping on them and chooses the models.  Then the pets' own step (photos tagged as animals and not fursuits), crops, vectors, groups, the Pets tab and "Same pet?".  Shares no detections, vectors, groups or suggestions with faces or fursuits; it may reuse a model file already downloaded, never data. | 4–6 d + the user's gathering and labelling |

About 27–36 days in all.  Deferred work (above the summary) is outside
these phases.

---

## 9. Testing

- **Unit tests (no photos, no models):** the query parser and chips; the
  closeness steps on synthetic score lists; animal words and the fursuits
  section; burst stacks in a ranked list; clearest-first order; key
  stability (a rename, a move, a copy keep the key; a changed byte
  doesn't); path-map diffs; vector file round-trip, damage rejection,
  model-mismatch handling; grouping and suggestion rules on synthetic
  vectors; **kinds never crossing** (a merge, a move, a suggestion or a
  completion across kinds is refused); a face-model change keeping
  members, rejections and covers; subjects file round-trip and
  merge/split; recent searches capped at 10 and forgotten.
- **Model tests (ignored, need `PP_MODEL_DIR`):** each model's output on
  a few synthetic images against stored reference vectors (catches a bad
  export or a provider bug), and WebGPU against CPU within the
  self-check's tolerance.
- **Device rule:** with a fake failing card (a test-only switch in the
  client), the pass stops, the banner shows what failed, and no job runs
  on the processor; with *Processor* chosen, the same jobs run there.
- **Evaluation (phase 0, 4 and 5 sets, ignored):** precision and recall
  per query, group purity per kind, run before changing a model or
  threshold.
- **Bench:** `bench_search_index` on a scratch copy and on the share
  (reads only), CIFS bytes, processor and graphics card.
- **Scripted UI** (`PP_SCRIPT`, headless KWin, scratch `HOME` with a tiny
  model set): `key ctrl+f`, type a query, `shot`, Enter, Esc (the page
  stays); `key ctrl+4` and the tabs; Y/N review; the card-failure banner.
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
- **Easy to wipe.**  Preferences ▸ Storage gains rows in its existing
  pattern (a name, one sentence, the size, an action):
  - *Search index*: "What each photo looks like to search, so a search
    reads no photos."  92 MB, **Clear**, like the other caches.
  - *People, fursuits and pets*: "The faces, fursuits and pets found in
    your photos, and the names you gave them."  31 MB, 14 named,
    **Forget all…**.  Like the unsaved-marks row, it is not a cache, so it
    asks first: "Forget all people, fursuits and pets?  This deletes what
    was found in your photos and the names you gave them.  Your photos are
    not changed."  (**Forget all**, **Cancel**).

  Each subject has *Forget this person* (or fursuit, or pet).  Turning
  *Find people's faces* off offers to delete what was found.  *Recent
  searches* has *Forget them*.
- **Strangers.**  Event days are full of people who never asked to be
  recognised, and faces are found by default (decision 9).  So: nothing
  is named for you; groups under the minimum (5 photos by default; 3, 5 or
  10) fold under *Smaller groups*; unnamed groups never appear in
  completions; unnamed detections are cache and go with *Clear*; one
  switch turns finding off and offers to forget what was found.
- **Nothing personal in the public repo.**  The vocabulary is generic
  words; tests use synthetic vectors; no photos, crops, names or vectors
  are committed.  `scripts/check-private.sh` could also refuse tracked
  `subjects.json`, `*.npy` and vector files.
- **Names stay in the app.**  They are never written into photo files
  (deferred: "write names as keywords on export" would be its own
  decision).

---

## 11. Licences

**A personal, non-commercial project.**  The user states that this is a
personal project with no commercial use.  It is recorded here because it
is why InsightFace is offered at all.  InsightFace's models are "available
for non-commercial research purposes only": a personal hobby app is
non-commercial, but "research" is narrower, and whether personal use
counts is for each user to judge.  So the app never pre-selects it,
states its licence in the download dialog in plain words, downloads it
only from InsightFace's own release (never bundled with the app or
re-hosted by this repository), and anyone who runs the app and picks it
is bound by that licence themselves.  The app and its other models stay
usable by anyone under their own licences.  *Not legal advice.*

| Model or code | Role | Licence | In the app |
|---|---|---|---|
| SigLIP2 ViT-B/16 (Google) | search | Apache-2.0 | **chosen**; always downloaded |
| CLIP ViT-B/32 (OpenAI) | search | MIT | not chosen |
| PE-Core B/16 (Meta) | search | Apache-2.0 | not chosen |
| MobileCLIP / MobileCLIP2 (Apple) | — | `apple-amlr` (research use) | **no** |
| YuNet (OpenCV Zoo) | face detection, for all three face models | MIT | downloaded with faces |
| AuraFace v1 (fal) | face recognition | Apache-2.0 (its repo also carries InsightFace detectors; take only `glintr100.onnx`) | offered, pre-selected |
| SFace (OpenCV Zoo) | face recognition | Apache-2.0 | offered |
| InsightFace `buffalo_l` (Immich's) | face recognition (`w600k_r50.onnx` only) | "non-commercial research purposes only" | offered, never pre-selected; from InsightFace's release only |
| InsightFace `antelopev2` | — | the same | **no** |
| OWLv2 (Google) | fursuit head detection | Apache-2.0 | downloaded with fursuits |
| DINOv2 (Meta) | fursuit head grouping | Apache-2.0 | downloaded with fursuits |
| OWL-ViT (Google), Grounding DINO (IDEA) | — | Apache-2.0 | not chosen |
| YOLO, YOLO-World (Ultralytics) | — | AGPL-3.0 | **no** |
| Pet models | pet detection and grouping | chosen in phase 5 | — |
| ONNX Runtime | runtime | MIT | — |
| `ort` crate | binding | MIT OR Apache-2.0 | — |
| Dawn (WebGPU) | GPU | BSD-3-Clause | — |

Downloading weights from their publisher at run time redistributes
nothing; a model this repository exports and attaches to its releases
(decision 13; OWLv2 is the likely one) ships with its licence and NOTICE
beside it.  The README gains a *Models* section listing each one, its
source and licence, and the app's About lists those downloaded.  *Not
legal advice*: the training data behind the weights (WebLI, LAION, WIT,
WebFace) carries its own terms, which the model licences don't settle.

Sources: Immich docs (facial recognition, searching, ML hardware
acceleration, system settings) and discussions on pet recognition;
InsightFace's model zoo (python-package docs) and its v0.7 release; ONNX
Runtime provider docs (ROCm removed in 1.23); `ort` docs (execution
providers, linking); candle and burn repositories; the model cards on
Hugging Face and OpenCV Zoo.

---

## 12. Risks

- **Domain mismatch.**  General models think "dog" is a fursuit here and
  rank empty frames high.  Mitigations measured above: the fursuit
  contrast (now the default for animal words), the accidental filter, a
  relative cut-off.  Phase 0 checks them on more queries.
- **Fursuit grouping is the uncertain part.**  Half the large automatic
  groups were mixed; cross-day and costume-change matching is barely
  tested.  Mitigation: semi-automatic by design, strict cut-off, its own
  evaluation before building the UI on it.
- **Pets have no data yet.**  One real dog in the samples.  Phase 5
  starts by gathering photos; until then search covers pets.
- **Compute and the card.**  SigLIP2 on the processor is ~45 min for the
  first index; OWLv2 on the processor ~15 h.  The fast path is an
  experimental provider, and with no automatic move to the processor, a
  card that fails stops indexing until it works again or the user chooses
  the processor.  Mitigations: the helper process, a self-check before
  use, a banner that says exactly what failed, search working on what is
  indexed.
- **NAS load.**  ~20 GB read for the first index (about 0.35 MB a shot);
  the fursuit-head step reads ~15 GB more, spread over ~1.4 h; a catch-up
  step reads again, when asked.  Read-ahead off, visible thumbnails first,
  newest days first, stoppable.
- **Build and size.**  +23 MB binary, a ~105–122 MB download at the first
  build, a release-candidate crate.  Pin it; `build.sh` says when it
  needs the network.
- **Memory.**  The SigLIP2 text tower is large (1.1 GB fp32); fp16 or an
  int8 vocabulary table, and the helper exits when idle.  OWLv2 adds
  ~600 MB fp32 while heads run.
- **Licence.**  InsightFace's terms bind whoever picks it (§11); it is
  never the default and never re-hosted.
- **Privacy.**  §10; faces are on by default, with the safeguards there.
- **UI weight.**  Two rail entries (Search, Subjects), Ctrl+F and Ctrl+4.
  No badges, no tags on photos, nothing in the culling grid.

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

---

## As built — phases 0 and 1 (backend)

*What the code does where it differs from, or settles, the sections
above.  Written as the backend landed (2026-10-02); the search page is the
UI round's.*

### Models (§4.4, decision 13)

- **Source.**  Immich keeps no ONNX export of the checkpoint the study
  measured (SigLIP2 ViT-B/16 at 256 px; its B/16 is the 224 px
  checkpoint, fp32 only, 1.5 GB).  The app pins Hugging Face's
  `onnx-community/siglip2-base-patch16-256-ONNX`, the transformers.js
  export of `google/siglip2-base-patch16-256` (Apache-2.0), maintained by
  the Hugging Face staff: the measured checkpoint.
- **Precision, measured here.**  The fp16 *image* tower gives wrong
  answers on WebGPU (cosine 0.61 against the processor's answers; the
  self-check refuses it) and runs ten times slower on the processor
  (300 ms a photo: ONNX Runtime casts around every fp16 op on x86).  So
  the image tower is the **fp32 file (372 MB)**.  The fp16 *text* tower's
  answers equal fp32's (cosine 1.000) and a query costs ~50 ms against
  ~30 on the processor, so it stays fp16 (565 MB).  With the tokenizer
  (34 MB): **971 MB**, not the ~750 MB the dialog copy assumed.
- The fp32 image tower passes the self-check on this card: cosine
  0.999999, largest difference 8.6e-7 over four fixed inputs (the
  tolerance is cosine ≥ 0.995, difference ≤ 0.02, provisional).
  Vectors from the Rust pipeline equal the study's open_clip embeddings
  (cosine ≥ 0.9995 on 64 previews), so §3's numbers carry over.

### Which card (§4.2)

ONNX Runtime's WebGPU provider asks Dawn for an adapter and never says
which it got; this machine offers the discrete card, the processor's
integrated GPU and llvmpipe (software), and the driver's default ordering
changes with the session (the integrated one came first in a headless
run).  The helper therefore enumerates the Vulkan adapters with `wgpu`,
chooses by a fixed rule (discrete, else integrated; never a software or
virtual adapter, which is `NoCard`), pins Dawn to it through Mesa's
device-select layer (`MESA_VK_DEVICE_SELECT=vendor:device!`, set in the
helper before the session is made) and also asks for the high-performance
power preference, then reports that adapter's name (`ml/gpu.rs`, with a
unit test of the rule).  `PP_GPU=<part of a name>` picks a hardware
adapter for tests.  Measured with 96 local previews: the 7900 XTX 5.7 ms a
photo end to end (the earlier numbers were already its); the integrated
GPU 244 ms; both pass the self-check.  Without Mesa's layer (another
driver) only the power preference steers Dawn.

### The helper and the client (§4.3)

As planned: `photo-pipeline --ml-worker --device gpu|cpu`, JSON lines on
its pipes, nice 10 and the idle I/O class, exit after 3 idle minutes.
Additions: `--text-only` (the text tower alone, on the processor, for
queries after a card failure); text requests are answered between image
chunks (32 photos on the card, 8 on the processor), so a query waits at
most a fraction of a second while indexing runs; previews are read and
prepared on 6 threads ahead of inference.  Measured: 1 779 local previews
in 10.0 s on the card (5.6 ms a photo, reading and decoding included);
the helper starts in ~1.2 s.  `libwebgpu_dawn.so` sits beside the binary
and is found through a runpath (`$ORIGIN`); `build.sh` checks both.

### Costs measured on the share (`bench::bench_search_index`)

Scratch libraries of symlinks into the archive, read over SMB, the index
in a temporary folder, the card: a Sony day (126 shots, MPF previews)
runs at 32 photos per ~230 ms once going, **7.2 ms a shot**, the plan's
number; the whole bench including the helper's start and the library
diff took 17 ms a shot on so small a set.  Older bodies' JPGs have no
MPF preview, so the whole JPG is read and decoded at a DCT reduction:
on the Canon days (2017–18) chunks took ~535 ms, **~17 ms a shot**.  The
library's 3 230 RAW-only shots go through LibRaw's smallest preview of at
least 300 px.  Queries: ~33 ms with the text encoded (27 ms of it in the
text tower), 7 ms when the text is cached; the engine prints both with
`PP_PROFILE=1`.

### The index (§6)

As planned, with these details: the vectors file is append-only in sealed
blocks (a torn tail is dropped), compacted on load when more than a tenth
of its rows have no path; a file of another model, weights or
preprocessing version is not read (the index is simply built again:
"the old file answers until the new one is complete" is not built in
phase 1, nor for Rebuild, which clears first).  A renamed or copied file
costs one preview read and one inference, after which its known key is
mapped without a new row (the plan's "one head read" short cut isn't
built; renames in the library are rare).  Burst stacks come from
`Bursts::find` over the indexed shots when a query needs them.

### Closeness (§7.2, UI 4) — open until the labels arrive

The relative cut (a score above the query's own spread, in robust
standard deviations) fails for things most of the library shows: on the
1 779-preview sample "fursuit" (74 % of it) gets **no** hits at any
setting, "crowd" one, "dog" none.  SigLIP2 was trained with a sigmoid
pairing loss whose scale and bias are part of its weights (e^4.7265 =
112.9 and −16.77); σ(scale·cosine + bias) is the model's own absolute
match probability and is comparable between queries: "fursuit" top 0.98,
median 0.70; a weak "camera" hit at cosine 0.137 → 0.21; background
< 1e-4.  Both rules are implemented (`query::cut`,
`query::cut_by_probability`) and the phase 0 harness scores both on the
labelled set; the slider keeps its meaning (higher = stricter) whichever
wins, and its range and default are set then.

### Phase 0 so far

- Harness: `src/search/eval.rs` (ignored tests `candidates` and
  `score_labels`; `PP_MODEL_DIR`, `PP_EVAL_PREVIEWS`, `PP_EVAL_OUT`,
  `PP_EVAL_LABELS`).  36 generic queries, the top 50 of each as
  candidates, five prompt templates compared by mAP, P@10/P@50, recall in
  the pool, both closeness rules swept, the fursuit contrast on animal
  words.
- The labelling page is local HTML in the scratch folder (real photos;
  never published), pre-ticked by eye; the user's labels decide the
  prompt template, the closeness rule, range and default, and whether
  the animal-word contrast holds (dog: the real dogs rank 1 and 45 of 50;
  the model's "fursuit beats it" flag marks 47 of the other 48).
- Not done in this round: the face-model comparison (lower priority, no
  people set labelled yet).
