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
| UI 1 | Where people, fursuits and pets live | **Their own side-rail entry, *Subjects*, next to Search**, with *People \| Fursuits* tabs (Ctrl+4); *Pets* was a third tab until pets were put on hold (no tab shows now; the model keeps the kind).  This replaces an earlier "inside Search" answer. | The user confirmed it when asked about the conflict. |
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
  - `Cmd::Subjects`: "People and fursuits", Ctrl+4, a glyph of a
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

- **Pets are on hold** (the user's call): no Pets tab, no route to one; a
  saved tab of 2 opens People.  The model, search and engine keep the
  kind, so the tab can return.
- **One rail entry, two tabs** (UI 1).  *Subjects* opens on the tab last
  used; *People | Fursuits* sit in its header.  A tab shows one kind
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

### Closeness (§7.2, UI 4) — the probability rule, default provisional

The plan's relative cut (a score above the query's own spread, in robust
standard deviations) fails for things most of the library shows: on the
1 779-preview sample "fursuit" (74 % of it) gets **no** hits at any
setting, "crowd" one, "dog" none.  SigLIP2 was trained with a sigmoid
pairing loss whose scale and bias are part of its weights (e^4.7265 =
112.9 and −16.77); σ(scale·cosine + bias) is the model's own absolute
match probability and is comparable between queries.  **The app uses
it** (`query::cut_by_probability`; the user decided so on 2026-10-02,
before the labels): the slider is a floor on that probability, higher
stricter, over 0.03 %–30 % (cosine 0.077–0.141), **default 0.3 %**
(cosine ≥ 0.098).  From the sample: the background sits below 1e-4 and
the dark, empty frames near 1e-3, so both stay out; the plain hits sit
between 3e-3 and 0.98 ("fursuit" top 0.98 and median 0.70, so it comes
back whole; "camera" top 0.21; "car" and "food" tops 0.013 and 0.004,
their one clear hit each).  At 1 % "food" and "cake" would have nothing;
at 0.1 % the dark frames join "camera".  The user's labels fine-tune the
default; the relative rule lives on only in the harness, for comparison.
A closeness saved by the earlier build (2–6) is outside the new range and
reads as the default.

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

## As built — phase 2 (backend): tags and syntax

*Written as the backend landed (2026-10-02); the search page's side is
the UI round's.*

### The vocabulary (§3.2, decision 8)

`src/search/vocab.rs`: about 200 generic words, each with a kind
(object, scene, person, fursuit, animal), a plural (what lists show and
completions insert) and a flag for the words fursuits are taken for.
`Kind::is_subject` (person, fursuit, animal) is the one gate that keeps
*Things* to objects and scenes; the user's rule that people, fursuits and
pets never appear in it, and are never combined, is held by tests in
`vocab.rs`, `tags.rs` and `engine.rs`.  Multi-word entries ("night sky",
"dance floor") need their words in a row and hide the shorter entries
they contain.  Plurals go through the vocabulary's own forms, then
plain English rules.

### The user's labels (phase 0, 2026-10-02)

The user labelled the phase 0 sample: 36 words, 28 with photos, 331 right
photos in all (anything clearly visible counted, background included).

- **Search's closeness default is confirmed.**  Swept over the slider's
  range, mean F1 peaks at the default (match probability ≥ ~0.3 %,
  cosine ≥ 0.097): precision 0.70, recall 0.47.  Stricter is more often
  right but finds far less (1 %: 0.88 / 0.31); looser lets junk in.
  Unchanged.
- **The prompt template stays.**  Five phrasings span a mean average
  precision of 0.62–0.67; "a photo of a {}." scores 0.663, within noise of
  the best (0.667).
- **The tag floor drops from 2 % to 1 %.**  On the user's labels: 1 %
  tags are right 87.6 % of the time and find 30.5 % of the labelled photos,
  against 88.5 % and 21.5 % at 2 %; 0.5 % falls to 76 % right.  Tags are
  helpers, so precision first, but 1 % finds two fifths more for a point.
- **Strong and weak words.**  Fireworks, fursuit, dance floor and building
  are excellent (P@10 1.0); sunset, night sky and phone found nothing
  right at the default.
- **Infrared photos.**  Some of the sample was taken with an
  infrared-converted camera, which turns foliage white and skies strange:
  the model reads those frames as snow and sunsets, the source of most of
  the false "snow" and "sunset" hits.  Not a threshold problem.  Options
  for later: an "infrared" word in the vocabulary (so such photos can be
  searched for, and "snow"/"sunset" made to beat it, as animal words beat
  "fursuit"), or a per-camera flag in Preferences that marks a body as
  infrared.

### Hidden tags: the rule and its evidence

- **Rule.**  A photo carries a word when SigLIP2's own match probability
  for the word's prompt (the query template) reaches the word's floor,
  **1 % by default** (cosine ≥ 0.108; `tags::DEFAULT_THRESHOLD`, a per-word
  override is possible; 2 % before the user's labels, see below), and, for a word fursuits are taken for, when the
  word also beats "fursuit".  Each word is judged on its own: the study's
  softmax share let the fursuit label swallow the rest.
- **Evidence** (`eval::tag_thresholds`, the phase 0 sample of 1 779
  previews, 29 labelled words).  The absolute floor the search uses as
  its strictest (30 %) is far too strict for tags: "fireworks" tops at
  0.31 and "camera" at 0.21, so almost nothing is tagged.  Sweep (mean
  precision over the tagged photos of the labelled words / share of the
  labelled photos found): 0.5 % 0.74 / 0.44; 1 % 0.87 / 0.35; **2 % 0.94 /
  0.27**; 3 % 0.94 / 0.23; 5 % 0.97 / 0.16; 10 % 0.97 / 0.08.  At 2 % every
  tagged photo of 27 of the 29 words is right (selfie 7 of 8, crowd 3 of
  4); the study's counts come back: camera 3 of 3 right, fireworks 19 of
  19 (26 labelled), mountain 5 of 5, castle 4 of 4; by eye on contact
  sheets, stage 25 of 25 and concert 18 plausible.  (The "fursuit" row's
  precision is an artefact: only its top 50 were labelled.)
- **The contrast at derivation.**  "stuffed animal" tagged 148 previews
  at 2 %, 139 of them fursuits by the model's own fursuit probability;
  the nine left after "beats fursuit" are real plush toys (by eye).  So
  the fursuit-lookalike flag now means, for any kind, "tagged only where
  the word beats fursuit", and is set on the animals, "stuffed animal"
  and "cosplay" (986 tagged, 965 fursuits).  The same rule makes the
  "dog" tag mean real dogs, which the ranking boost and phase 5 want;
  the "fursuit" tag itself is unaffected (phase 4's head step reads it).
- **Cost.**  The vocabulary's text vectors are encoded once (~2.7 s for
  the prompts on the processor) and kept beside the index
  (`vocab-<model>.bin`, keyed by the vocabulary's digest and the model's
  weights), so later starts need no helper.  Tagging is in memory on the
  engine's thread: 1 779 × 195 words in 95 ms, so the library's 56 710
  shots take about 3 s on loading (once per start; rows are tagged as
  batches arrive during a pass).  Tags are kept in memory only and
  follow the index's generation (`Index::generation`: new rows tagged,
  a cleared index started over).

### Infrared (the user, 2026-10-02): a word and a generalised contrast

- **The word.**  "infrared" is a vocabulary word of kind scene (so it
  may appear in *Things*) with a prompt of its own, since the template
  doesn't suit it: `Word::prompt` is `"a photo taken with an infrared
  camera."`, and a plain search for one vocabulary word uses the word's
  prompt (`vocab::query_prompt`).  Measured against the sample's frames
  from the user's converted body (28 of 1 779 by the sources' EXIF model,
  `eval::infrared`; 15 sources could no longer be found): at the search
  default it finds 16, 14 of them infrared (precision 0.88, recall 0.50),
  the top 10 all infrared, 21 of the top 50, AP 0.71.  Other phrasings:
  the template's "a photo of a infrared." 11/10 (AP 0.54), "an infrared
  photo." 10/10 (0.66), "infrared photography" 7/7 (0.65), "an infrared
  landscape photo." 9/8 (0.62), the bare "infrared" 10/8 (0.46).  The
  frames the word misses rank near zero: a6000 frames without the
  infrared look (night shots and the like), which don't trip snow or
  sunsets either.
- **The contrast.**  `Word::fursuit_lookalike` became `Word::must_beat:
  Option<&str>`: a word this one must beat for a photo to count, in tags
  and in search alike, unless the query names that word itself
  (`vocab::contrasts_in`; "snow infrared" asks for infrared snow).  The
  fursuit rule is unchanged (`must_beat: Some("fursuit")` on the animals,
  "stuffed animal" and "cosplay"; the animal split and its tests as they
  were).  `must_beat: Some("infrared")` is set on **snow, sunset, night
  sky and mountain**: photos infrared beats are simply dropped, no
  section.
- **Evidence for which words** (the user's labels; the labelling page
  showed the top 50 by rank, so that is where the false ticks were).
  Infrared frames in the top 50 before → after the contrast, right ones
  kept → kept (right lost): snow 13 → 1, 4 → 4 (0); sunset 15 → 1, 4 → 4
  (0); night sky 10 → 1, 1 → 1 (0); mountain 14 → 1, 5 → 5 (0).  At the
  search default: snow 2 hits, 1 right, the wrong one infrared → 1 hit,
  right; sunset 2 hits, both wrong and infrared → 1.  **Every wrong snow
  and sunset tick at the default was an a6000 frame.**  Not contrasted,
  with reason: tree (6 of the 22 right trees are infrared frames — a
  white tree is a tree; the contrast would drop 5 right from the top 50),
  beach (2 right dropped), building (1 right dropped), flowers (3
  infrared in the top 50, none wrong at the default), forest, field,
  park, sign, kitchen, bedroom (one to four hits each, infrared frames of
  real fields and parks).  The word alone separates the infrared look
  well; a Preferences setting naming an infrared body would catch the
  a6000 frames without that look, which only the user can decide and is
  not built.

**Open — revisit later (the user, 2026-10-02): infrared recall.**  The
"infrared" word is precise (88 % at the default; the top 10 all infrared)
but finds only half the frames from the infrared body (14 of 28; AP 0.71).
The misses rank near zero and don't look infrared, so the word catches
the *look*, not the body.  The test set is small (28 frames, 15 more
sources unresolved after renames).  Options when this is picked up:
check the 14 missed frames by eye (if they don't look infrared, the word
is doing its job); a looser floor for "infrared" alone (more found, some
ordinary photos let in); or a Preferences setting naming the infrared
body (exact for that camera, if every frame from it is infrared).

### Queries (§7.2) as applied

- `search::parse` is pure and shared with the page: words, quotes
  (`exact`), `-word`s, dates (a year, a month and year, a month range
  `apr–jun 2024` with `-`, `–`, `—` or `..`, a year range), `@name`s.
  Two date terms narrow each other; a month name alone stays a word
  ("may flowers").  A `-word` or a name alone runs nothing and answers
  empty hits without an error.
- **Dates alone browse the period** (the user, 2026-10-02): every
  indexed shot of those days, in shooting order, oldest first, with a
  neutral score of 0, no cut and no cap; accidental frames stay out and
  bursts stack as usual.  With `-word`s, a shot is left out when a search
  for the word would show it (its match probability for the word reaches
  the closeness setting), the one sensible reading of the contrast with
  no words to compare against.  `@name` beside dates browses the dates.
- **The user's other answers** (2026-10-02): a burst split between the
  real animals and the fursuits stays split per frame, because one photo
  can hold both a fursuit and a real animal; the header's count while
  the fursuits are shown is the real animals only; single letters in the
  field already offer months ("j": June, July, January…).
- One query encodes the words' prompt (or the quoted text), each
  `-word`'s prompt and, for a real-animal query, "a photo of a
  fursuit.", in one request; a photo is skipped where any `-word`'s
  score reaches the words' score, or its day is outside the dates.  The
  closeness cut applies to the words' score.  Then the animal split:
  photos "fursuit" beats go to `fursuits` (best first), never to `hits`.
  Then the boost: hits carrying every vocabulary word the query names
  as a tag come first, each group still best first (`Hit::tagged`).
- Dates filter on the day the calendar files the shot under; the
  completions' months and years come from the calendar's days (the page
  computes them; `parse::date_completions` is there if wanted).
- A vocabulary word's plural is sent to the model as its singular
  (`vocab::for_prompt`): *Things* inserts the plural, and "a photo of a
  cameras." tops at a match probability of 0.035 on the sample against
  0.21 for "a photo of a camera.".  Other words go as typed.
- *Things* is sent at most every 2 s while a pass runs, and once more
  when it ends.

### Measured (`eval::library_queries`, the phase 0 sample as a scratch library)

1 779 previews in three made-up days, indexed on the card in 14.5 s
(1 764 rows: the sample's duplicates share a content key).  At the
default closeness: **"dog" 1 hit (the real dog, tagged first) and 50
fursuits left out**; "fox" 1 hit and 207 left out; "cat" 2 and 7;
"bird" 3 and 3; "fireworks" 24 hits, 19 tagged first ("fireworks
-crowd" the same 24: no crowd among them); "camera" 5 hits, 3 tagged
first; "camera 2025" 5, "camera 2026" 0; "stuffed animal" 793 hits with
the 7 real plush toys first; "fursuit" 1 511 hits, 1 491 tagged first;
"@Someone" nothing, "@Someone camera" the 5 cameras; "2025" alone
nothing.  Things (38): stages 25, fireworks 19, concerts 18, parties 16,
stuffed animals 7, dance floors 6, mountains 5, televisions 5, castles
4, cameras 3, …  Queries answer in 7–9 ms with the texts cached (two or
three prompts scored), ~40 ms with the text tower.  On a 238-shot
scratch library of five archive days (symlinks, read only): 6.7 s to
index, Things of mountains 42, stuffed animals 10, forests 9, lakes 9,
televisions 4, …

### API (the page's contract)

`BgEvent::SearchResults { seq, hits, indexed, error, understood:
Arc<Parsed>, fursuits: Arc<Vec<Hit>>, animal: Option<String> }`;
`BgEvent::SearchThings(Arc<Vec<Thing>>)` and `Engine::things()`;
`search::Thing { word (the plural), photos, kind }`;
`search::complete_things`; `parse::{parse, Parsed, DateRange, date_text,
date_completions}`.

### Not built, and open

- Per-word thresholds other than the default: none needed on the
  sample; the hook is `Word::threshold`.
- A tag file on disk: tags are recomputed on loading (seconds); if the
  start ever feels slow with the full library, cache them by generation.
- "Searching the photos indexed so far" during the first index already
  carries tags for the rows done.

## As built — phase 3 (UI): people

The page follows §7a.5–7a.9; what differs, and what phase 4 builds on.

### Where it lives

- `Cmd::Subjects`, Ctrl+4, the side rail under Search and the View menu,
  labelled *People, fursuits and pets* (*Subjects* is only the code's
  name).  The page remembers its tab (`settings.subjects_tab`).
- `src/app/subjects.rs` (the tabs, the overview, merge and forget
  dialogs, the catch-up page, the crop cache), `subjects_person.rs` (a
  subject's page and band, *Faces*, naming), `subjects_review.rs` (the
  review), `people.rs` (the backend's types re-exported, the words for
  each kind, and the stand-in `FakePeople`), `results_pane.rs` (the
  results grid, shared by the search page and a subject's page),
  `src/ui/subject_tile.rs` (round and rounded crops, tiles, the Y/N
  stamps, the photo with its box outlined).  Glyphs in `ui/icons.rs`:
  Subjects, Person, Fursuit, Paw, Merge, Split, Hide, Pencil, More,
  Plus.
- **Kind-generic where it was cheap:** a tab's state comes from
  `subjects::state(app, kind)`, the words from `people::KindWords`
  (`title`, `one`, `crops`), the shape from `Kind` (round for people,
  rounded squares otherwise), `search::NAMED_KINDS` lists the kinds `@`
  completes.  Fursuits and Pets show `State::Later` until their phase
  plugs in.

### As designed, with these differences

- **Overview:** named, then *Groups to name* largest first, *Smaller
  groups* (under `people_min_photos`, folded) and *Hidden* (with
  *Show*).  An unnamed tile reads "Unnamed" (the mockup's "Name this
  person" read as a button on a grid of 20 of them); its tooltip says
  "Open to name this person (F2)".  *Select* is always in the header,
  greyed with no groups, so the tabs don't move.
- **A subject's page** queries through `search_link::query_in` with the
  subject as a chip; its sequence numbers start at `SEQ_BASE` (2^40) so
  its results never land on the search page.  Quick look and the viewer
  read the same pane (`Place::Person`); *Open day ›* returns to it
  (`calendar.back_to`).
- **⋯** holds *Merge into…*, *Hide* and *Forget this person…*.  *Change
  the cover* and *Show where* are not built: the backend has no cover
  call, and the review's outlined photo covers *Show where* for now.
- **Hiding** says so in the status bar ("Person A is hidden.  Their
  photos still come up when you search for them by name."); Ctrl+Z
  undoes it, as every change to people (the backend's `undoable` names
  it), but there is no *Undo* button in the status line.
- **The review** is as drawn: Y and N in `KEEP`/`REJECT`, the stamp on
  the crop, the beat from `viewer_advance::BEAT_MS` when *Advance after
  marking* is on, one answer per press, the strip with stamps, the end
  page with *Find more* and *Back to …*.  Checking an unnamed group's own
  faces asks "Same person as this group?" and No moves the face out.
- **The download dialog** builds the face rows from the registry
  (`models::face_models()`): radio, *Recommended*, size, speed from
  `estimate::face_seconds` on the chosen device, licence line in `WARN`
  when `restricted`.  Unticking *Find people's faces* leaves only the
  search model.  The same dialog in a faces-only form serves the
  catch-up (`Dialog::faces_only`).
- **Preferences:** a *People* section of its own after *Search*: *Find
  people's faces* (turning it off offers *Keep them* / *Forget them*
  inline), *Face model* with *Change…* (the chooser, the time from
  `estimate::face_model_change_seconds`, names and confirmed faces
  kept), *Smallest group to show* (3, 5, 10), and *People found* with
  *Forget all…*.  §7a.8 put *Forget all…* under *Storage*; it sits with
  the other people rows here, since it is not a cache (open question 2
  below).
- **`@name`:** typing `@` completes named people only, each kind under
  its own heading (`search_results::offer`, in memory); the pick becomes
  a chip (up to three; Backspace in an empty field or × removes one);
  the query goes as `query_in(text, chips)`.  A name no one has is
  `Results.unknown`: "No one is named "…"".  Recent searches store the
  chips as `@"Name"`.  "Fursuits and pets come later." sits under the
  completions.
- **Faces passes** keep apart from search's: the status bar says
  *Finding faces* / *Recognising faces again*, search's coverage strip
  and its end message ignore them, and the People tab shows its own
  progress strip.

### The stand-in

The stand-in engine (`PP_SEARCH_FAKE`) takes `search fake people`
(named and unnamed groups, smaller and hidden ones), `people-first` (none
named yet), `faces-catchup` (indexed before faces), `faces-off` and
`faces-running [n]` from a test script.  `FakePeople` keeps
the backend's shapes (`GroupId`, `FaceRef`, `Subjects`) and rules: kinds
never merge, a name is unique within its kind, No removes and never
suggests again, one undo step per change.  Its crops are grey
silhouettes drawn in code: no face, real or generated, is in the tree.

### Checked

- `cargo test --release`: 333 passed, zero warnings (people, chips,
  completions, kind separation, the stand-in's rules).
- Headless with the stand-in (`subjects …`, `search dlg faces|face N`,
  `prefs find-faces|face-model|forget-people|…` in `src/app/testing.rs`):
  every state above shot and read; `PP_PROFILE=1` over a review run
  showed no slow update (max 0.9 ms) and no slow draw.

### Open

1. **Unnamed tiles:** "Unnamed" (as built) or the mockup's "Name this
   person".
2. **Forget all:** Preferences ▸ People (as built) or Preferences ▸
   Storage (§7a.8).
3. **The catch-up pass:** `Engine::find_faces()` fetches the missing
   face models (as the faces-only dialog expects); the pass over the
   indexed library itself comes with the backend's face cache, and the
   page shows it through `PassKind::Faces` as the stand-in does.

## As built — phase 3 (backend): people

*Written as the backend landed (2026-10-03).  The page's side is above.*

### Models (§4.4, decisions 10 and 13)

- **Pinned** (`ml/models.rs`): YuNet 2023 March (OpenCV Zoo, MIT,
  0.2 MB, an LFS file fetched through GitHub's media endpoint), AuraFace
  v1 (`glintr100.onnx` alone, 261 MB, Apache-2.0, recommended), SFace
  2021 December (39 MB, Apache-2.0), InsightFace `buffalo_l`'s
  `w600k_r50.onnx` (174 MB, "non-commercial research purposes only").
  The digests are the study's.
- **InsightFace comes as an archive** from its own v0.7 release
  (`buffalo_l.zip`, 289 MB): `ModelFile.archive` pins the archive's
  digest and size; the download checks the archive, inflates the one
  member (`ml/unzip.rs`, a small reader of the format; `flate2`), checks
  the member's digest and size, renames it into place and deletes the
  archive.  Never re-hosted; never pre-selected; `restricted` marks its
  row for the dialog's warning tone.
- **YuNet's file declares a 640 × 640 input** although the network is
  fully convolutional (its reshapes are `[1, −1, k]`; OpenCV ignores the
  declared shape).  The helper makes the batch, height and width dims
  symbolic in memory after the digest check (`ml/onnx_patch.rs`, a
  protobuf walk of the few messages involved); the file on disk stays
  the publisher's.  Measured first in Python: the patched graph runs at
  832 × 832 and in batches, its outputs flattened in batch order.
- **Inputs:** the detector takes BGR 0–255 (OpenCV's blob of a BGR
  `Mat`); AuraFace and InsightFace `(v − 127.5) / 127.5` RGB; SFace RGB
  0–255 as OpenCV feeds it.  SFace's file fixes its batch at one, which
  the helper honours; the others take any batch.

### Faces in the helper (§3.3, §5.2)

- **Canvas, not full size.**  The detector runs on a fixed square canvas
  the preview is scaled to fit and padded on, so every photo has one
  tensor shape and a chunk is batched on the card (eight canvases a
  call).  Measured on 200 of the study's previews (score ≥ 0.9, ≥ 40 px
  on the full preview): a 640 canvas found 49 faces, 832 55, **1 024
  61**, 1 280 64 at twice the processor cost, the full preview 59
  (large faces score lower).  1 024 is the knee.  Over the 1 010
  previews it finds 140 faces in 119 photos; the study, at full size,
  found 145.
- **One read, two decodes.**  The search input and the accident rule keep
  their small DCT-reduced decode; the canvas comes from a second decode
  of the same bytes at ≥ 1 000 px on the long side.  Measuring accidents
  on the large decode had cost more than the detector.  Faces found are
  cut from a third decode at full preview size (only for the photos with
  faces, about a quarter), aligned to the ArcFace template by a
  similarity transform from YuNet's five landmarks (`ml/face.rs`),
  recognised, and given a 128-px JPEG tile for the page.  RAW-only shots
  take a preview of at least 1 000 px for faces (§7.3).
- **Fixed batch shapes.**  The card compiles its kernels per tensor
  shape: a chunk whose face count was new had cost 6–12 s.  Detector and
  face batches are always full (padded with black), so compilation
  happens once per helper start: 10–30 s with AuraFace.  That cost
  returns at every helper start (the idle exit after three minutes), a
  thing to keep in mind if new-export passes feel slow to begin.
- **Self-checks** run per session on the card: the detector and the face
  model are each checked against the processor on a fixed input
  (`worker::check_session`); measured cosine 0.99997–1.000000, largest
  difference ≤ 1e-6.  A failure is a `CardFailure::SelfCheckFailed`, as
  for search.
- **Protocol:** `Request::EmbedImages { files, want: Want { search,
  faces } }` with `FaceAsk::{None, Detect, Known { files }}`;
  `Embedding.faces: Vec<FaceFound>` (box and landmarks as fractions of
  the oriented preview, score, size, sharpness, vector, tile).  `Known`
  recognises stored landmarks again (a model change): no detection.  The
  helper takes `--face-model <id>` and `--models-root`; `Spec { device,
  text_only, face_model }` replaces the factory's two flags.
- **A deadlock, found by the processor timing:** faces were cut on the
  reader pool while that pool's threads were all blocked handing
  prepared photos to the inference thread (the processor's chunks of 8
  fill the channel).  Faces are cut on the global pool now.

### The cache and the user's file (§6)

- **Beside the index** (`search/faces.rs`), files 0600 in a 0700
  folder: `faces.bin` (per photo looked at, the detections, maybe none;
  sealed blocks like the vectors file; a detector version in the
  header), `facevecs-<model>.bin` (a `FaceRef` and an f16 vector per
  face, one file per model so a change keeps the old one), `tiles.bin`
  (an offset index in memory; `Engine::crop` reads one tile from any
  thread).  Rows are compacted away when more than a tenth of the photos
  left the library; a torn tail loses its last block.  *Clear* drops
  everything of the cache (unnamed detections are cache, §10); *Forget
  all* the user's file too.
- **`FaceRef`** is a photo's content key and the box in thousandths of
  the oriented preview: the identity the user's decisions point at, the
  same whichever model recognised the face.  The file is §6.5's JSON
  (`SubjectsFile`), one file for every kind; its operations check kinds
  (`Refused::MixedKinds`), confirming a face to one subject takes it
  from any other of that kind, No puts it in `rejected` and takes it out
  of the members.  Fifty undo steps are kept in memory, each the whole
  file before the edit with what the edit was ("Merged 2 groups into
  …"); `BgEvent::SubjectsUndone` says it.
- **Catch-up and model change** are `PassKind::Faces` passes planned from
  the index's keys, never from `pass::plan`, and started only by a
  command (`find_faces`, `set_face_model`), never by a pass's end: a
  pass that has nothing left starts nothing.  **The catch-up covers every
  indexed photo not yet looked at for faces, new exports indexed while
  face finding was off included** (the user, 2026-10-03; a test holds
  it).  A model change switches the vectors file, starts the helper
  again with the new model, and recognises the stored landmarks again
  for the photos whose faces lack vectors; names, members, rejections
  and covers are untouched.
- `Storage.faces_bytes` is the *People* row; the index row no longer
  counts the face files.

### Grouping (§5.1, §6.5, UI 6, UI 12)

- **Pure, in `model/subjects.rs`:** rows join the nearest cluster within
  the cut-off or start one, clearest first, then clusters whose centroids
  lie within the cut-off merge until none do; new rows are assigned
  incrementally as batches arrive and the full grouping runs on a worker
  thread when a pass ends or a decision changes (`Cmd::Regrouped`).
  Faces confirmed to a subject take no part in the clusters.  Cluster ids
  are the clearest member's `FaceRef` hashed, so an id lives as long as
  its cover does; a stale one is `Refused::StaleGroup`.
- **The per-kind step** (`search/step.rs`): a `KindStep` supplies refs,
  rows (f32 or f16 bits), each detection's visual clarity and its
  cut-offs; `step::regroup` and `Grouped` are written once for every
  kind and never see another kind's rows or subjects (tested).  Phase 4
  implements it for fursuits with its own cache and cut-offs
  (`Cutoffs { group, suggest, strict }`: its head model wants groups
  stricter than suggestions).
- **Suggestions** for a subject: faces within the cut-off of any of its
  confirmed members, confirmed nowhere, not rejected for it, nearest
  first, **one entry per burst** (the resolver's burst id); *Find more*
  is the same at the strict cut-off.  For a cluster: its own members,
  surest first, per burst.  Counts per subject come from the regroup.
- **Clearest first:** a face's clarity is half its own (size over 200 px,
  sharpness as the Laplacian's variance, the detection score) and half
  how sure its membership is (one minus its distance over the cut-off,
  to its cluster's centroid or to its subject's other members); a
  subject's page with no words orders photos by their clearest face.
- **`@name`:** resolved against the named subjects of `Kind::Person`
  (two subjects with one name count as either), narrowing the query's
  photos; a name nobody has narrows to nothing, and
  `SearchResults.subjects` carries the groups applied so the page can
  tell.

### The face-model comparison (phase 0's last item)

On the card, over the 1 010 previews of sample L, the three models see the
same 140 faces (the detector is shared), and run at the same speed within
noise, since reading and decoding dominate: **AuraFace 15.4 ms a photo,
SFace 15.9, InsightFace 17.3**.  Average-linkage groups of their vectors,
judged by eye on scratch sheets (never published):

| | AuraFace (0.6) | SFace (0.5) | InsightFace (0.5 / 0.6) |
|---|---|---|---|
| Groups of ≥ 3 faces | 7 (38 faces) | 9 (50) | 9 (57) / 9 (67) |
| Of ≥ 5 | 2 (21) | 3 (30) | 4 (40) / 4 (50) |
| The 14 largest, by eye | all one person each; no junk group | one group of 9 non-faces (foliage, blur), the rest one person each | a group of 14 / 20 non-faces, one person-group with a stranger in it |

YuNet's false detections at score ≥ 0.9 (leaves, bokeh, a hat) are the
difference: SFace and InsightFace give them similar vectors and gather
them into the largest "person"; AuraFace spreads them apart.  So
**AuraFace stays pre-selected** and `subjects::cutoff_for` keeps 0.6 for
it and 0.5 for the other two.  A later improvement: a junk filter on the
crop (variance, a second detector score) before recognition.

**The processor** (eight threads, 200 previews, 61 faces, search off):
SFace 11.8 ms a photo, InsightFace 13.3, AuraFace 15.3; so finding faces
costs about 11.5 ms a photo on the processor whichever the model (the
larger decode, the canvas, YuNet, the full decode of photos with faces)
and recognition about 12 ms a face for AuraFace, 5 for InsightFace, 1 for
SFace.  (A first timing gave 57 ms a photo for AuraFace: the processor
was running the card's padded batches of 32; it now runs what there is.)
`estimate::face_*` carries these numbers: a catch-up over the library is
about 10 minutes on the card, 11–14 on the processor, within the plan's
+7–14.  InsightFace's real size and speed, which the plan left to measure:
174 MB kept of 289 downloaded, and in the middle of the three.

### Checked on a scratch library (`eval::people_library`, read only)

634 shots in six days (an event day and phase 1's five), symlinks into the
archive, the card: the first index with faces took 50 s (the share bound;
each photo's preview decoded twice, those with faces three times); 56
faces in 35 clusters (8, 6, 4, 3, 3 photos, then pairs and singles);
naming the largest; `@Someone` answered its 8 photos clearest first in
7 ms, `@Nobody` nothing; a tile from the cache in 10 ms, the whole
preview in the same call; undo; *Forget all* removed the subjects file.
Nothing under the archive was written.

### Not built, and open

- A junk-face filter (above).
- The regroup is leader clustering with a merge pass, not the study's
  average linkage; on 20 k faces it is seconds on the worker.  Average
  linkage over the library's faces would be minutes and is not needed
  while the groups read clean.
- The helper's kernel compilation on each start (10–30 s with AuraFace);
  a persistent shader cache would be Dawn's to offer.
- *Change the cover* has no engine call yet (the cover is the clearest
  member).

## As built — phase 4 (backend): fursuits

### The evaluation (2026-10-03): pairwise "Same suit?"

Phase 4 started, as planned, with the user's own labels.  Two earlier
pages (groups to name; one head at a time with names) were dropped at the
user's request — clusters that mixed two suits about evenly could not be
named as one, and nothing here is trained on names — in favour of
**pairwise answers**: two heads from two presses of the shutter, each
shown as a tight crop and as the whole photo with the head boxed; Y same,
N different, S skip, X "not one clean head", Z undo.  Pairs were drawn
from the study's 892 head tracks (one head followed through one press;
1 239 OWLv2 crops over two event days) in bands of DINOv2 cosine
distance around the provisional cut-offs, half of them across the two
days, never within a press or within two minutes on the same day, and a
pair whose answer already followed from earlier answers (same is
transitive; different carries across a same-group) was not asked.  The
page lives outside the repository and keeps no names, dates or paths in
tracked files.

**258 pairs answered** (65 same, 193 different; 121 of them across the
days), 10 heads left out as not one clean head, none skipped; with
transitivity 673 pairs have a known answer.  34 suits were seen in two or
more presses, the largest in 29; only 2 of the 34 on both days, so the
cross-day evidence for *same* is thin (18 asked same-pairs).

P(same | distance) per band, asked pairs only (unbiased: the bands were
sampled evenly), with the figure counting implied pairs in brackets —
those are lifted by the suits photographed many times:

| Band | All | Same day | Cross-day |
|---|---|---|---|
| under 0.14 | 23/26 = 88 % [99 %] | 19/21 [99 %] | 4/5 [80 %] |
| 0.14–0.17 | 24/52 = 46 % [79 %] | 13/26 [83 %] | 11/26 [73 %] |
| 0.17–0.20 | 12/65 = 18 % [58 %] | 9/26 [58 %] | 3/39 [58 %] |
| 0.20–0.23 | 5/65 = 8 % [34 %] | 5/39 [21 %] | 0/26 [48 %] |
| 0.23–0.27 | 0/39 [5 %] | 0/14 | 0/25 |

Cumulative, asked pairs only (precision = share same; recall over the 65
asked same-pairs): under **0.14**: 23/26, recall 35 % (same day 40 %,
cross-day 22 %); under **0.17**: 47/78 = 60 %, recall 72 % (same day
32/47, cross-day 15/31); under **0.20**: 59/143 = 41 %, recall 91 %.
With implied pairs: 0.14 → 98.7 % / 50 %; 0.17 → 91 % / 73 % (same day
95 % / 87 %, cross-day 73 % / 36 %); 0.20 → 82 % / 91 % (cross-day 64 %).

**Purity** of average-linkage clusters over the crops, judged on clusters
holding two or more labelled presses with an answered pair: at 0.14, 3 of
15 mixed (20 %); 0.17, 8 of 19 (42 %); 0.20, 14 of 28 (50 %); **0.25, 14
of 25 (56 %)**, and 9 of the 11 judged among its 16 largest — the study's
"7 of 16" was, if anything, kind.

**Chosen (`search::heads::{STRICT_CUT, SUGGEST_CUT}`):** strict *Find
more* at **0.14** (nearly always right; finds about a third of the
matches, fewer across days); suggestions for the Y/N review ("may show
this fursuit") up to **0.17**; nothing offered beyond it.  The study's
provisional 0.20 is not used: between 0.17 and 0.20 only 12 of 65 pairs
were the same suit.  Groups to name come from the strict cut, and still
one judged group in five mixes suits, so *Check this group* stays on
offer before naming.  Matching across days is weak at every cut-off: the
same suit under different light is found about half the time at 0.17
and rarely at 0.14; this is the number to revisit if the user wants more
cross-day matches (a looser cut for the review only, never for *Find
more*).

**The user's decisions (2026-10-03), on these numbers:**

- *Cross-day matching:* accepted as it is.  `SUGGEST_CUT` stays 0.17
  everywhere and `STRICT_CUT` 0.14 for *Find more*; no looser cut across
  days.  A suit seen on another day mostly comes through a Y in the
  review.
- *Vector size:* **full size kept**, 1 536 wide, f32 rows for the
  grouping: **measured at that scale** (`search::heads` ignored test,
  68 000 synthetic rows × 1 536): the process grows from 40 MB with the
  store open (detections and tiles) to 445 MB once the vectors are
  built (+405 MB, read from the f16 file in 0.3 s), and to 843 MB while
  a regroup job's f32 copy is alive on the worker (+399 MB for its few
  seconds; the copy is the generic grouping's, shared with people).
  Built only when the fursuit work needs it (a regroup, the Fursuits
  tab, suggestions), never at start-up, and **freed after use** (the
  user's final choice): once no fursuit request has needed the vectors
  for `Fursuits::VECTORS_IDLE` (2 minutes) and no scan runs, the engine
  lets them go on its own tick and the process is back near the
  store-open size; the next request reads them again (0.3 s).  The file
  on disk is 218 MB.  For
  the record, the CLS half alone (768 wide) scored as well on the same
  258 answers, slightly better in places (under 0.14: 24 of 26 asked;
  under 0.17: 46 of 67 = 69 %; cross-day under 0.14: 6 of 6; cluster
  purity at 0.14: 17 % mixed), and the mean-patch half clearly worse
  (under 0.14: 27 of 60); the choice was made for the full vector
  anyway, and is one line (`ml::heads::pool_tokens`) if memory ever
  presses.

### The head step (helper side)

`ml::heads`: OWLv2 B/16 ensemble (Xenova's export of Google's weights)
asked for "a fursuit head" over the preview fitted top-left in a 960
square (grey padding, CLIP normalisation); boxes decoded from the padded
square into the preview, kept at score ≥ 0.3, one per head at IoU 0.4,
and at least 6 % of the long edge; a square crop with a 12 % margin
(black beyond the edge, as the study's); DINOv2 base over the crop
resized to 256 and centre-cut to 224, pooled as CLS and mean patch token
(1 536, unit length); a sharpness measure for clarity; a 128-px tile.
On the card both sessions are checked against the processor on fixed
inputs (cosine 1.000000 measured, largest difference 5e-5); a failure is
a card failure, never a move to the processor.  The helper takes
`--heads`, answers `Request::FindHeads` with `Reply::Heads`, and
`Embedder::find_heads` is image work under the same card rule as
embedding.  Models pinned in the registry as `Owlv2` (614 MB + 2 MB
tokenizer) and `Dinov2` (347 MB), role *Fursuits*.

### The step's state (`search::heads`)

The head cache beside the index, files readable by the user alone:
`heads.bin` (detections per photo looked at, sealed blocks with a
detector version), `headvecs-dinov2-base.bin` (ref + f16 vector, the
model's weights digest in the header), `headtiles.bin`; nothing shared
with the face cache.  A scan runner over `find_heads` with the pass's
cancel, busy and card rules (`heads::start`, `heads::execute`).
`Fursuits`: the gate (a photo is looked at when the index tags it
*fursuit* and the cache hasn't seen it), the plan over the library's
indexed shots newest day first, and decision 11's rule as code:
`consider(jobs, why, device, asked)` starts on the card, and on the
processor holds the jobs as `Step::Asking(Offer)` every time — first
index, new exports, resume — until `answer(start)`; a skipped scan's
photos are remembered and offered again from the tab as a catch-up; a
catch-up the tab asked for (its cost already shown) starts.  A card
failure ends the step `Failed`; `card_back` makes it idle again.  Costs:
`estimate::head_catch_up_seconds` (0.12 s a photo on the card, 1.3 s on
the processor, from the study; the helper's own numbers follow from the
ignored `ml::heads` test on the real models).

**A name shared across kinds** (the user, 2026-10-03: "the one you
picked"): a chip carries its `GroupId`, so a picked chip searches that
subject alone.  `@Alex` typed, when a person and a fursuit both have the
name, resolves to the person; the fursuit comes beside the results as
`SearchResults.also_fursuits: Vec<(String, GroupId)>` for the page to
offer ("also a fursuit called Alex"), and its photos are never in the
list (tested: `a_name_shared_across_kinds_never_unions_results`).

Kinds never mix: a head is a `FaceRef` of `Kind::Fursuit` subjects only;
its vectors are 1 536 wide where faces are 512; its files, requests and
replies are its own; the store ignores face files in the same folder
(tested).

### The relabel across the library (2026-10-07) and the grouping chosen

The first labels were two event days.  The user wanted every photo of a
suit gathered, "from minutes apart to years apart", so a second label
set was taken over the whole library: a read-only symlink library of
all 74 archive days with JPGs (days over 600 JPGs thinned evenly to
~600; 14 517 photos), indexed and scanned for heads by the app's own
step on the card (9 251 fursuit-tagged photos, 10 826 heads in 7 033
photos, 7 909 head tracks in 5 088 presses over 37 days), and a
pairwise page like the first with four tiers by the time between the
two heads: the same hour (never the same press, a minute apart at
least), the same day, the same week, further apart.  Pairs were drawn
from each track's nearest neighbours within the tier's time window,
mostly close calls with easy ends, spread over tracks and days.  Every
answer carries both heads by content key, box, time and press, so it
stands on its own.  **791 answers**: same/different 110/86, 84/115,
102/97, 63/134 by tier; 12 heads left out.

P(same | distance), asked pairs, the two heads' own distance:

| tier | < 0.10 | 0.10–0.14 | 0.14–0.17 | 0.17–0.20 | 0.20–0.24 | 0.24–0.30 |
|---|---|---|---|---|---|---|
| same hour | 10/10 | 27/29 | 32/44 | 23/44 | 12/39 | 4/22 |
| same day | 10/10 | 26/30 | 27/42 | 12/40 | 6/48 | 2/21 |
| same week | 9/9 | 29/30 | 34/49 | 22/44 | 5/38 | 1/19 |
| further apart | 9/10 | 23/28 | 19/44 | 8/52 | 4/36 | 0/20 |

Under 0.14 a pair is the same suit 82–97 % of the time in every tier;
beyond it the further tiers fall off sooner (0.14–0.17: 73 % the same
hour, 43 % further apart).  *Find more* at 0.14 reaches 37/110, 36/84,
38/102 and 32/63 of the labelled same pairs per tier at 37/39, 36/40,
38/39 and 32/38 right; the review at 0.17 reaches 69/110, 63/84,
72/102 and 51/63 at 69/83, 63/82, 72/88 and 51/82 right.  (The pairs
were sampled as close calls, so these recalls are of hard pairs.)

**The grouping that shipped, measured against these labels**, over all
10 826 heads: nearest centroid with the merge pass at 0.14 chained the
library into **one group of 4 265 heads (39 % of them)**; it held 327 of
359 labelled same pairs but also 273 of 432 different pairs.  The
merge pass was the cause (at 0.12 the group was 2 635 heads, at 0.10
still 695).  Without it, over the same labels (recall = same pairs in
one group; merged = different pairs in one group; mixed = 3+ groups
holding a labelled different pair / 3+ groups holding any labelled
pair): nearest centroid 0.14 gathered 152/359 with 32/432 merged, mixed
8/45; **average linkage 0.17: 95/359 (35/110, 23/84, 21/102, 16/63 by
tier), merged 7/432 (0/86, 4/115, 1/97, 2/134), mixed 6/49**; the
centroid with a seed cap 0.14/0.17: 98/359, 7/432, 6/39; complete
linkage 0.17: 47/359, 3/432.  The even and odd halves of the days give
the same ranking.  Regroup cost at 10 826 heads on one worker: the
merge pass 17.6 s and 70 MB; average linkage 1.84 s and no extra memory
once a cluster is shortlisted by an exact bound (for unit vectors the
mean distance to a cluster's members is at least the distance to its
centroid).

**The user's choice (2026-10-08): tight groups and *Find more*.**
Fursuit heads group by average linkage at 0.17 with no joining step
(`KindStep::linkage` → `Linkage::Average`; `cutoff_for(Dinov2)` is
group 0.17, suggest 0.17, strict 0.14); the incremental assign during a
pass uses the same rule, so it admits nothing a regroup would refuse;
*Find more* (0.14) and the review (0.17), nearest first, join a suit
across days and years.  People's grouping is untouched
(`Linkage::PEOPLE`, pinned member by member on toy rows).  On the night
convention day of the pass (455 heads): before, the largest group held
101 heads spanning 12 of the tight groups (and the 32-head one spanned
3); after, the largest are 31, 24 and 13 heads, each one suit on its
contact sheet.  No automatic grouping on these vectors gathers the
far-apart matches without mixing; that is what the review and *Find
more* are for.

## As built — phase 4 (UI): fursuits

The Fursuits tab and the head step as the user sees them (§7a.4–7a.8,
decision 11), on phase 3's pages and the phase 4 backend's API.

### Where it lives

- `src/app/subjects_fursuits.rs`: the tab's own states (no library, not
  indexed, off, the catch-up offer, looking, the processor's question, a
  failed card, no heads), its caption, the head step's card over the
  groups, the processor's strip under the folder bar in any view, the
  status bar's line with ×, and their messages (`Message::Fursuits`).
- `src/app/fursuits.rs`: `search::heads` re-exported, the registry's
  pair as the pages name and count it (`models_name`, `missing_models`,
  `library_photos`), and the stand-in `FakeFursuits`.
- `src/app/fursuit_art.rs`: the stand-in's grey cartoon heads (the
  approved mockup's drawing: fox, cat, bear, rabbit, dragon and dog ears;
  masks, blazes and spots; three greys), and made-up photos with a suit
  at its box.  Greys only; no photo of anyone.
- Hooks, kept narrow, in phase 3's files: the Fursuits tab's body and
  caption (`subjects::overview`, `header`), the progress strip by kind,
  *Find more* on a fursuit's band and the review's end page
  (`Link::find_more`), the dialog's fursuit tick and rows, Preferences ▸
  Fursuits and a *Fursuit heads* storage row, `NAMED_KINDS` with
  fursuits, and the test driver's `fursuits …` and `search fake
  fursuits|heads-…`.

### As designed, with these differences

- **The glyph** is the mockup's head; shown at 18 px or less (the tabs,
  buttons) it draws a simpler outline with the eyes and a solid nose,
  since the inner-ear lines and the muzzle ran into a blob.  The paw
  print stays the Pets tab's, so the two kinds never look alike (open
  question 1).
- **The processor asks every time** (decision 11): whenever a scan would
  start (after the first index, after an export, carrying on after a
  stop) the step waits as `Step::Asking`, and the page asks in a strip
  under the folder bar in any view, and as a card on the tab, with the
  photos and the cost from the measured estimates: *Find fursuit heads* /
  *Skip this time*.  Ticking *Find fursuit heads* in the first-index
  dialog is not the answer (open question 2): the dialog starts it
  unticked on the processor (§7a.4), and the scan still asks when it
  would start.  Skipped photos are offered again from the tab and
  Preferences, with their cost.
- **Find more** (strict) is a button on a named fursuit's band beside
  *Review N suggestions*, and the review's end page calls the same
  `find_more`; the status bar says how many it found, or that nothing
  more is close enough.
- **A failed card** is said once: the banner in every view, and the
  tab's card (with *Try the card again* and *Preferences*, never the
  processor) only once the banner is put away.
- **A typed `@name` both kinds share** searches the person (the user's
  decision); the page spots the fursuit of that name itself (it turns
  typed names into chips before the engine sees them) and also takes the
  engine's `also_fursuits`: the understood strip says "There's also a
  fursuit called Alex." with *Search for the fursuit instead*, which puts
  the fursuit's chip in the person's place.  Never mixed into the
  results.
- **Forget all, per kind:** Preferences ▸ People's *Forget all…* calls
  `forget_kind(Person)` and Preferences ▸ Fursuits' calls
  `forget_kind(Fursuit)`; each leaves the other kind's names and answers
  alone.  The engine's all-kinds `forget_all` had no caller left and was
  removed (Clear and Rebuild still empty both caches and keep the user's
  file, §10).
- **The download dialog** lists the registry's pair (OWLv2, DINOv2) under
  *Find fursuit heads*, a cost line, totals and sources; only the missing
  models are download rows (the same rule as phase 3's fix for faces).
  The heads-only form serves the tab's catch-up.
- **Storage:** a read-only *Fursuit heads* row with the head cache's
  size; forgetting is in Preferences ▸ Fursuits.

### The stand-in

`FakeFursuits` sits beside `FakePeople` on the same shots and keys (so a
photo of a person and a suit is one photo): heads only on shots "tagged"
fursuit, a press showing one suit, sometimes two heads a photo; suits in
look-alike pairs so automatic groups mix a little, as the study found;
named suits' suggestions a few at a time, *Find more* letting the next
few through (look-alikes after the first round), until nothing more.
Edits go to the stand-in of the group's kind, and Ctrl+Z takes back the
newest edit of either.  `search fake fursuits|fursuits-first|
heads-catchup|heads-off|heads-running [n]|heads-asking [photos]
[first|export|resume]|heads-skipped [n]|heads-cardcrash` set its states.

### Checked

- `cargo test --release`: zero warnings; tests for the stand-in's rules
  (heads only on fursuit photos, mixing, strict Find more, answers per
  press, merges and moves never across kinds), the art (greys only), the
  glyph (ears, eyes and nose at every size, open between the ears), the
  processor's question (states the cost, never offers the card), the
  head models as download rows only while missing, and a name both kinds
  share offering the fursuit beside the person.
- Headless on the stand-in: every state of the tab, a fursuit's page and
  heads, naming with *Merge into*, the review with Yes and No, the
  processor's strip and card, skipping, the catch-up offer and its
  dialog on both devices, a failed card, Preferences and Storage, and
  `@` completing people and fursuits under their own headings.
- Headless on the real engine (a throwaway home with the search and face
  models linked in, 165 photos of one convention day as a scratch
  library, the card): the dialog lists only the fursuit pair as
  downloads (963 MB), with search and AuraFace "Downloaded"; the pair
  downloads and checks, then indexing runs search, faces and the head
  step, which starts unasked on the card after the first index
  (124 photos tagged fursuit, 149 heads); the status bar shows *Finding
  fursuit heads 80 of 124* with ×; the tab shows two groups to name
  (24 and 12 photos) and 103 smaller ones; a group's page, *Heads* and
  "Same fursuit as this group?" work on real heads.  The 24-photo group
  holds more than one suit (a red cat and a black dragon among them),
  which the page's line "it may hold more than one fursuit" and the
  review are for.  `PP_PROFILE=1` over the run: the only slow updates are
  the test driver writing its screenshots; the regroup (17 ms) and the
  vocabulary (7 s) are on the engine's worker.  The run also caught the
  review's count reading "19 photo to checks" (fixed for both kinds).
- A last tour on the stand-in under `PP_PROFILE=1` (the tab, a page,
  *Heads*, the review, *Find more*, the processor's card and strip,
  skipping, a running scan, a failed card, the catch-up and its dialog,
  Preferences, Storage, `@`, a shared name): no slow update but the
  driver's screenshots (`update:Bg` at most 14 ms).  It caught, and these
  are fixed: the off and catch-up pages sent the user to Preferences ▸
  Search for the switch (it is in Preferences ▸ Fursuits); *Downloaded
  models* still said "the search model" (it holds the face and head
  models too, and Delete removes them all); the stand-in's skipped scan
  left nothing to offer.

## As built — fursuit matching v2 (UI): the close-call judge

The user-facing side of v2 (decided 2026-10-08): whitened search-model
head vectors in place of DINOv2, and Gemma 4 as an optional judge of the
close calls in *Find more* and the "Same fursuit?" review.

### Where it lives

- `src/app/fursuit_judge.rs`: the judge as the pages show it.
  - `Run`, one request's verdicts, kept in `Subjects::judge`.
  - `on_judged` takes `BgEvent::Judged`.
  - The review's line over the strip, the strip's hollow tail tile, the status bar's slot, the band's left-out count, and their messages (`Message::Judge`).
- The review (`subjects_review.rs`): the judge's offers join the END of the list (`Review::take`, deduped by each entry's first head), so nothing on screen moves and answers keep their places.
  - What the judge left out joins too, marked, when *Show them* is pressed.
  - An empty first list stays open while a judge is on (`waiting_page`), and the end page says when close calls are still coming.
- Preferences ▸ Fursuits:
  - *Close-call judge*, a radio list built from `JudgeChoice::ALL` and the registry's `choice: "judge"` rows, so another judge is a new variant and a row, not a new control.
  - A line under the list says where the chosen judge stands: not downloaded (with *Download…*), downloading, the processor chosen, on the card now, or stopped.
  - *Heads to update* with *Update them*.
  - Storage ▸ *Close-call judge*, a row of its own with *Open* and *Delete…*.
- The download dialog: `Dialog::judge(choice)` shows the judge's row and what it costs, runs on and keeps. Its progress, failure and status-bar words name the judge.
- The Fursuits tab: `State::Vectors` (a page when no head has the new vectors yet), a card over the groups otherwise, and `Why::Vectors` progress with ×.
- Glyph `CloseCall`: two rounded-square heads on a beam.

### As designed, with these differences

- **Choosing a judge not downloaded** opens the dialog in the main window (Preferences closes, as *Find their heads…* does). The choice is made when *Download* is pressed. If the download stops or fails, the choice stays, and Preferences offers *Download…* again.
- **Off is never an empty state.** With the judge off, the review and Find more are exactly as before: no line, no tile, no hint to turn it on.
  - The same holds whenever the judge can't run: not downloaded, the processor chosen, or stopped. The engine answers with the plain cut-offs and sends no `Judged`.
  - The page asks `JudgeStatus::active(device)` before waiting for verdicts.
  - Preferences says "close calls are matched by look alone" for each case.
- **Speed in words** comes from the measured time a close call takes (2.3 s a pair, both orders): "About 2 seconds a close call", and in the dialog "(about 4 minutes for 100)".
- **A failing card** is said in the review's line as two lines in the warning tone, with *Try the card again* and *Preferences*, and in the status bar. The run it stopped is told as "Checked 6 of 81 close calls before the judge stopped". The processor is never offered (`failure_words`, tested).
- **The fursuit download** is now the head finder alone; the copy names it from the registry (`fursuits::models_name`).

- **One ask, sixty close calls** (the backend's `judging::MAX_CLOSE_CALLS`). A finished run with more beyond says "N more can be checked" with *Check the next N*, which is the review's *Find more*. With the judge on, a new ask keeps the review's list and adds what is new at the end (`Review::append`), so answers and place stay.

### Checked on the real engine

All of this ran in a throwaway home: a copy of the relabel's v1 index (14,517 shots, 10,826 DINOv2-era heads), with the search and head models linked read-only. It ran through the card lock and never wrote to the library.
- *Update the fursuit heads*:
  - the page offered 7,033 photos ("about 6 minutes", 2.5 GB);
  - it ran at about 25 photos a second on the card;
  - groups appeared once whitened (160 to name by 2,000 photos);
  - × kept what was done, and the rest was offered again.
- The judge, under `PP_JUDGE_FAKE`:
  - a group named, then the review;
  - the sure suggestions came at once, and the judge's offers landed at the end of the list;
  - *Find more* started a new run;
  - the status bar and the review line followed `JudgeState`.
- With `PP_JUDGE_FAKE=card-fail`, the review's line showed the failure in the warning tone, with *Try the card again* and *Preferences* and no processor offer. The run was told as stopped.
- The real Gemma 4 judge (the integration build, its files linked read-only):
  - it loaded, then the line said "Checking 135 close calls…, about 2 minutes left";
  - the 60 new close calls took about 2 s each;
  - the review kept its place while verdicts landed; *Show them* listed the 90 left out;
  - Preferences said "Gemma 4 is on the graphics card now".
- `PP_PROFILE=1`: the only slow updates are the test driver's (`update:Script`). `bg:Judged` and `update:Subjects` take well under a frame.

### The stand-in

`FakeFursuits` judges a named fursuit's close calls:
- its own suit's held-back heads beyond the sure batch;
- look-alikes, mixed in nearest first;
- verdicts as the suit, with a few mistakes.

The fake engine runs them a tick at a time. It waits while a pass, the head step or `judge-wait` holds the card, and it takes a few ticks to load. It sends `Judged` deltas as the engine does.

`search fake judge-on|judge-off|judge-notinstalled|judge-wait|judge-free|judge-cardcrash|judge-broken|heads-vectors [n|all]` set its states.
