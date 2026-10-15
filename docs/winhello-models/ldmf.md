# Windows Hello Face: `LDMF` detector (FaceDetectorResources 101/102) and the ORT helper detectors (110–113)

Research / interoperability write-up. The models are Microsoft proprietary; converted outputs live only in the
git-ignored `models/windows_hello/converted/ldmf/`. linux_hello does not ship or use any of this.
Code: `tools/winhello_models/ldmf/` (README there). Author: agent `conv-ldmf`.
Binaries analysed (Windows 11 build 26300, public PDBs): `FaceTrackerInternal.dll`
(sha256 `934c618f…ebd32e`), `FaceProcessorCore.dll` (sha256 `d9736a9d…b6c1be`). All addresses below are VAs
with image base `0x180000000`.

## 1. Result in one paragraph

`MODEL_FILES_101.ldmf` (infrared) and `MODEL_FILES_102.ldmf` ("low end") are **not CNNs**. They are
Microsoft's *JDA* — joint cascade face detection and alignment (boosted pixel-difference regression/classification
trees, 5 stages, 31 resp. 27 landmarks) — plus an *oriented-HoG post filter* (OpenCV-SIFT descriptors at every
landmark, linear classifier). Two parts of the model are **not in the file but compiled into
FaceTrackerInternal.dll code**: the whole first cascade stage (248 resp. 81 depth-5 trees, comment in 102's header:
*"cascade model (without the first stage)"*) and a 400-fern pre-filter. They were recovered by symbolically
executing the Hex-Rays pseudocode. A numpy reimplementation of the full detector (`ldmf/jda.py`) is
**bit-identical to the DLL's own code executed under Unicorn** on every tested window (fern decisions, integer
cascade scores, all 31 landmark coordinates, post-filter scores for 101), finds exactly one face on each of the
owner's full-face IR frames (landmark error vs SCRFD 0.03–0.04 inter-ocular) and all 6 faces in the stranger
photo with no false positives. `MODEL_FILES_110..113.bin` are XOR-obfuscated ONNX-Runtime (`.ort`) RetinaFace /
img2pose-style detectors used by `FaceProcessorCore`'s `RetinaEngine`; they load directly in ONNX Runtime after
decryption, and their pre/post-processing is reimplemented in `ldmf/retina.py`.

## 2. Inventory

| file | what (status) |
|---|---|
| `MODEL_FILES_101.ldmf` | JDA, type tag `0xFACE0C06` = infrared, 31 landmarks, 5 stages × 248 trees, HoG threshold 6.3406. Loaded by `FaceDetectorFactory::InitializeModelsUsingFilePath` 0x1800367ec for `DetectionModel` 1 (`FaceTrackerInternalType` 0) — OBSERVED |
| `MODEL_FILES_102.ldmf` | JDA, type tag `0xFACE4DB5` = "low end", 27 landmarks, 5 × 81 trees, HoG threshold 5.5762. `DetectionModel` 3 (`_readOnlyCommonModel`, `FaceTrackerInternalType` 1) — OBSERVED |
| `MODEL_FILES_110.bin` / `112.bin` | HelloFaceCipher-XORed `.ort`; RetinaFace-style, outputs bbox/class/landmarks(5)/confidence/**dofs(6)**. `RetinaEngine` type 0; 112 instead of 110 when feature `IntegrateComplianceHelloModels` is on. 110 and 112 give identical outputs (max diff 1e-6) — same weights, different serialisation |
| `MODEL_FILES_111.bin` / `113.bin` | same family without `dofs`, `RetinaEngine` type 1 (strides 32/64, anchors doubled); 111 ≡ 113 numerically |

Pairing 101↔110/112 and 102↔111/113 is INFERRED from the shared `FaceTrackerInternalType` value (0/1).

## 3. LDMF byte layout (OBSERVED, every byte accounted for)

Little-endian. Header read by `JointCascadeModel::Read` 0x180001bd8 (`istream::read(this, 360)`).

| off | type | 101 | 102 | meaning |
|---|---|---|---|---|
| 0x000 | char[4] | `LDMF` | `LDMF` | magic (checked) |
| 0x004 | u32 | 360 | 360 | header size; loader accepts 16 or 360 |
| 0x008 | u32 | 101 | 101 | model id; must be 101..595; 101/102 → `JointCascadeDetectionModel` (both files say 101) |
| 0x00C | u32 | 13096096 | 3872256 | total file size |
| 0x010 | char[64] | `FaceSDK Team` | `FaceSDK Team` | author; rest 0xFE fill (101) / uninitialised writer memory incl. pointers (102) |
| 0x050 | 16 B | 0 | 0 | |
| 0x060 | u32 | 1425080885 | 1414479676 | looks like a Unix time (2015-02-27 / 2014-10-28) — INFERRED |
| 0x064 | u32 | 0 | 0 | |
| 0x068 | char[256] | `Infrared` | `This is a production jda model for low end platforms, including cascade model (without the first stage) and post filter model.` | description, 0xFE fill |

Cascade body, `JointCascadeDetectionModel::Read` 0x180002450 (offset 0x168):

| field | type | 101 | 102 | meaning (consumer) |
|---|---|---|---|---|
| tag | char[4] | `SACD` | `SACD` | |
| body size | u32 | 13079848 | 3858056 | bytes from `SACD` to the post filter |
| type tag | u32 | 0xFACE0C06 | 0xFACE4DB5 | `AssignCascadeModel` 0x18009c170 selects tree count, reference shape, Boost code |
| L | i32 | 31 | 27 | landmarks |
| n_w, w[n_w] | i32, i32[] | 39680 | 12960 | leaf weights, fixed point (score scale 2^24), `5 stages × T trees × 32 leaves`; stage-0 weights are here although stage-0 trees are not |
| n_nodes, nodes[] | i32, 44 B each | 62496 | 20412 | `SplitNode`, 63 per tree (31 split + 32 leaf), stages 1–4 only |
| n_trees, info[] | i32, 12 B each | 992 | 324 | `TreeInfo` = 4 stages × T |
| n_shapes | i32 | 39685 | 12965 | `DeltaShape` count |
| n_bytes, bytes[] | i32, u8[] | 4, `00000000` | 4, `00000000` | `vector<char>`, read, never used by the predictor |
| shapes | n_shapes × (f32[2L] + 8 B pad if L odd) | | | `DeltaShape` slots are 256 B in memory (64 floats); file rows padded to 16 B |

`SplitNode` (44 B): `i32 leaf` (−1 = split, else leaf index inside the stage) · `i32 scale` (0 full, 1 half, 2 quarter
image) · `i32 a_idx` (float index 2·landmark) · `f32 a_dx, a_dy` · `i32 b_idx` · `f32 b_dx, b_dy` · `i32 thr` ·
`i32 left, right` (child index relative to the tree's first node). Decision: `pixel(A) − pixel(B) <= thr → right`.

`TreeInfo` (12 B): `i32 leaf_end` (cumulative leaves inside the stage: 32, 64, …), `i32 n_nodes` (63),
`i32 reject_thr` (cascade score must stay `>=` this after the tree).

Shape array order: `[0]` mean shape (96×96 window coordinates); `[1 .. T·32]` stage-0 leaf deltas (window coords);
then for stages 1..4: one stage-bias delta followed by `T·32` leaf deltas (reference-frame coords).
Checked: `1 + T·32 + 4·(1 + T·32) = n_shapes`.

Post filter, `OrientedHogPostFilter::Read` 0x180001974: `char[4] "SOPD"` · `u32 size` (= 16 + 4n) ·
`f32 threshold` · `i32 n` (= 128·L) · `f32 w[n]`. Loader stops exactly at EOF for both files.

## 4. Parts that live in FaceTrackerInternal.dll

| item | location | how recovered |
|---|---|---|
| stage-0 trees (IR 248, LowEnd 81; depth 5, fixed window-relative pixel pairs on full/half/quarter images, per-tree reject thresholds) | code: `JointCascadePredictProductionInfraredModelBoost1..10` (0x180048158 …), `…LowEndModelBoost1/2` (0x18000e328, 0x18003a6b8) | symbolic execution of Hex-Rays output (§6) |
| initial cascade score | `JointCascadePredictProductionInfraredModel` 0x18000ca80: `*a11 = 126862`; LowEnd 0x18000d610: `-8221270` | read |
| fern pre-filter: 400 ferns × 8 pixel-pair tests `(A−B) > thr` on the 24×24 half-res face patch, per-fern reject thresholds | code: `FernPredict1..8` (50 ferns each, 0x180007210 …), thresholds in `__xmm@` constants | symbolic execution |
| fern weights int32[400][256], fern index LUT uint8[65536] | `0x1800AE6F0`, `0x180112750` (LUT verified = plain lane bits) | .rdata |
| fern initial score −25453782; `*a5` counter bumped after ferns 1,2,4,8,16,32,64,128,256,399,400 | `DetectInLayer`, FernPredict* | read / symbolic |
| reference shape (similarity target for stages 1–4) | IR `0x180134440` (31 pts), LowEnd `0x180134FF0` (27 pts) | .rdata |
| post-filter 5-point template ([0,1] units) | `__xmm@` 0x180122A30, 0x180122A20 + immediates `0x3f1f4b08`, `0x3f2aacb9` (0.6222, 0.6667) | .rdata/code |
| atan LUT float[128][128] (degrees of atan2(dy>>2, dx>>2)) | centred at `0x18012AB90` | .rdata |
| window 96×96, 5 stages, score scale 2^24, face 48 px inside window, scan step 8, border 24, 4 layers/octave, box factor 1.1 | `AssignCascadeModel`, `WindowSlideConfig::Initialize` 0x18009c0e4, `JointCascadeFaceDetector` ctor 0x18009bd58, `Detect` | read |

## 5. Inference algorithm (all OBSERVED unless marked)

1. **Pyramid** (`Pyramid::InitAllLayers` 0x1800040f0, `Construct` 0x180004680, `ResizeImage` 0x180003e10).
   Layers 0..3: nearest-neighbour resize of the input to `int(W·0.5^(i/4)+0.5)`, source index
   `trunc(x·(W−1)/(w−1)+0.5)`; layer i≥4: 2×2 box `(a+b+c+d+2)>>2` of layer i−4 (odd column/row: average of 2,
   odd corner: copy), until a side ≤ 3. Each layer is zero-padded by 24 px; layer→image mapping
   `x_img = x_layer·sx + ox` with `sx=(W−1)/(w−1)` (octave 0) and `s←2s, o←s/2+o` per octave.
2. **Window scan** (`DetectInLayer` 0x1800061f0). Window 96×96 at padded position (x, y), the face is the central
   48×48 (= unpadded (x, y)). Layers used: face size `trunc(48·s+0.5) >= min_face` (both axes); half image =
   layer i+4, quarter = layer i+8. Rows step 8; columns step 16, and when a window's fern counter reaches 8
   (≥ 128 ferns passed) the scan steps back 16 px and scans the next 4 windows at step 8 (window re-visits are
   real and produce duplicate detections that the merge absorbs).
3. **Ferns** on the half-res patch at padded (24 + x/2, 24 + y/2): index = Σ bit_j·2^j with
   `bit_j = (p(A_j) − p(B_j)) > thr_j`; score += W[f][index]; reject when score < thr_f.
4. **JDA cascade** (`JointCascadePredictProductionInfraredModel` 0x18000ca80). Images: full window at (x, y),
   half at (12 + x/2, 12 + y/2), quarter at (18 + x/4, 18 + y/4). Stage 0: fixed-offset trees, shape = mean +
   Σ leaf deltas. Stages 1–4: similarity transform M from current shape to the reference shape
   (`ComputeSimilarityTransformationCoeff` 0x18000df70, least squares a,b,tx,ty; `Inverse` 0x18000e1b8),
   shape' = M·shape; for each node point = shape'[idx] + (dx, dy) mapped back with M⁻¹, `trunc(v+0.5)` clamped to
   [0,95], sampled at `>>scale` in the chosen image; after the stage shape' += bias + Σ leaf deltas and
   shape = M⁻¹·shape'. Integer score accumulates leaf weights, rejection after every tree.
5. **Post filter** (`OrientedHogPostFilter::Predict` 0x1800054f0). Similarity of landmarks 0–4 to the 5-point
   template gives a, b; keypoint size = 3.5/(100·√(a²+b²)), angle = deg(atan2(b, a)) in [0,360); one SIFT
   descriptor per landmark (`CalculateOnePointDescriptor` 0x18000a860 = OpenCV `calcSIFTDescriptor` with
   cvRound for centre and radius, `cvCeil(lo)+1 .. cvFloor(hi)−1` column ranges, orientation from the atan LUT,
   saturate_cast rounding); score = w·desc; pass if score ≥ threshold. The reported confidence is this score.
6. **Box** (DetectInLayer tail): landmarks to image coordinates `(x + lx − 24)·sx + ox`; square side
   `1.1·max(x-range, y-range)` of the first ⌈L/2⌉ landmarks (the loop runs `m < L` over x,y pairs), centred on the
   mean of landmarks 0,1,3,4.
7. **Merge** (`MergeDetectionResult` 0x1800314a0): repeatedly take the best-scoring detection, mean-shift over
   (cx, cy, s) with kernel `max(0, 1 − (dc²/(w²/4) + ds²/4))` (≤5 iterations), collect detections whose
   intersection / min-area with the mode box > 0.4, output box 96·s around the mode, score = kernel-weighted
   mean, landmarks = per-coordinate median of the top 6 members.

Landmarks 0–4 are the two eye centres, nose tip and mouth corners (INFERRED from positions; matches SCRFD's 5
points within 0.03–0.04 IOD). The other 26 (IR) are jaw/brow/eye/mouth contour points (INFERRED, see
`converted/ldmf/validation_viz.png`).

Not reimplemented (by design; only affects which windows are scanned, not the per-window result):
`WindowTracker` suppression of windows already covered by a detection (`SetMultiple`/`SetVicinity`),
`GetPrioritizedLayers` ordering, `VideoFaceDetector` tracking/search areas and thread pool, and the max-face
cap. The reimplementation scans every eligible window; duplicates collapse in the merge.

Windows pipeline context (FaceProcessorCore `FaceDetectorWithHelper::DetectFaces` 0x18001efc0, OBSERVED): the
JDA tracker runs on the frame as is; only if it finds nothing and the helper may start, the frame is
histogram-equalised (`FaceProcessorUtils::HistogramEqualization` 0x18000b31c) and a second "helper" tracker runs.
Which tracker is the helper (JDA again or `RetinaEngine`) depends on feature flags — UNKNOWN.

## 6. How the code-only parts were recovered

1. `extract/ida_decompile.py` (idalib, on a copy of the PDB-named IDB) renames all `__xmm@` constants to
   `XMMC_<ea>` and decompiles `FernPredict1..8` and the 12 `…Boost` functions.
2. `extract/build_structure.py` rewrites the pseudocode into C++ (`extract/symrt.hpp`): every local becomes a
   symbolic value; image reads become pixel terms, differences linear terms; a comparison of a pixel difference
   with a constant is a *decision* answered by an oracle and logged; reading the leaf-weight array ends a tree;
   SSE fern code becomes 8 logged tests. Hex-Rays artefacts handled: `__OFSUB__` flag idioms, `(x>>31)&1` sign
   tests, `int *` locals (element vs byte arithmetic), `&a[i]` aliases, score comparisons.
3. Each Boost function is explored depth-first per tree (32 paths): 248/81 complete depth-5 trees with leaves
   numbered `32·t + path` (OBSERVED: leaf index = path bits, `<=` = 1), one reject threshold per tree. Each
   FernPredict runs once (50 ferns, weights pointer advances 1024 B per fern).
4. Re-running the pipeline from scratch reproduces `hardcoded_structure.npz` byte-for-byte.

## 7. ORT detectors 110–113 (`ldmf/retina.py`)

* Decryption: HelloFaceCipher (`out[i] = in[i] ^ K[i%17] ^ (i&0xFF)`, see `../cipher.py`), applied by
  `OnnxTester::LoadAndInitialize` 0x180040c7c; result starts with `ORTM` and loads in onnxruntime 1.31 as is.
* I/O (OBSERVED in the graphs): `input_0` float 1×1×H×W; outputs `bbox` [1,N,4], `class` [1,N,2],
  `landmarks` [1,N,10], `confidence` [1,N,2] (softmax), `dofs` [1,N,6] (110/112 only); producer pytorch.
* Pre-processing (`RetinaEngine::ProcessNextFrame` 0x1800041d0): the search area of the 8-bit frame (full frame
  unless the ROI optimisation is active) copied **as raw 0..255 floats** — no mean/std. The DLL checks
  `N == GetNumPriorBoxes` = Σ 2·⌈H/s⌉·⌈W/s⌉.
* Configuration (`RetinaEngine::Initialize` 0x18001fd30): type 0 → resource 110 (112), strides 16/32, anchor sizes
  {32, 64} / {128, 256}; type 1 → 111 (113), strides 32/64, sizes doubled; confidence threshold 0.925
  (`0x3F6CCCCD`); variances 0.1/0.2; pose mean `[-0.0915, 0.0160, -0.0223, 0.0295, 0.2317, 3.5089]`, std
  `[0.2937, 0.5891, 0.2657, 0.6261, 0.1812, 0.4003]` (img2pose-style 6-DoF, INFERRED family).
* Decode (`ProcessResultCandidates` 0x180005c3c): prior p = level offset + m + 2·(k + gw·j), anchor centre
  ((k+0.5)·s, (j+0.5)·s); cx = 0.1·bbox₀·size + ax, w = exp(0.2·bbox₂)·size; keep `0.925 < conf[p,1]`, w,h ≥ 2;
  landmarks 0.1·lm·size + anchor; candidates sorted by score and accepted greedily if they do not intersect any
  accepted box at all (`any_of` lambda), up to the requested face count; `ShouldRejectCandidate` 0x18003dd30 then
  rejects candidates with any of the 5 landmarks outside the frame (plus an orientation check from the
  landmarks that is not reimplemented).
* Pose (`DecodeRotationVec` 0x18001a8a8): dofs·std + mean → rotation vector r;
  M = K_img⁻¹·K_box·R(r) with K_box = [[W+H,0,ax],[0,W+H,ay]], K_img = [[W+H,0,W/2],[0,W+H,H/2]];
  outputs (atan2(M₁₂, M₂₂), asin(M₀₂), −atan2(M₀₁, M₀₀)) in degrees. Formula fitted to and verified against the
  function under Unicorn: max error 2.4e-5° on 200 random inputs. Which output is yaw/pitch/roll is UNKNOWN
  (first = rotation about x, second about y, third about z).

## 8. Usage

```sh
cd tools/winhello_models
.venv/bin/python -m ldmf convert --dll /path/FaceTrackerInternal.dll       # -> converted/ldmf/*.npz,*.json
.venv/bin/python -m ldmf detect --draw /tmp/out frame.png
.venv/bin/python -m ldmf validate --dll /path/FaceTrackerInternal.dll      # -> converted/ldmf/validation.json
.venv/bin/python -m cipher ../../models/windows_hello/FaceDetectorResources/MODEL_FILES_11?.bin -o ../../models/windows_hello/converted/ldmf/ort
```
`unicorn` is used only by `emu.py`/`validate --dll` (it was already installed in the shared venv; nothing else
was added). The structure extraction (`extract/`) needs IDA 9.5 idalib and g++ and only has to run once.

## 9. Validation

**Emulator cross-check** (`validate --dll`, `ldmf/emu.py`: DLL mapped at its base under Unicorn, CRT imports
stubbed, `__chkstk` patched, fake TEB/TLS for MSVC thread-safe statics; the C++ objects are filled with the
field offsets read from `AssignCascadeModel`). On every fern-passing window of watch_0014, watch_0147 and the
stranger photo (all pyramid layers) plus 40 random windows per layer:

| model | fern windows (pass flag + counter) | JDA windows (alive + int score + 2L floats) | post-filter windows | max HoG score diff |
|---|---|---|---|---|
| 101 infrared | 1685 / 0 mismatches | 203 / 0 mismatches, landmark diff 0.0 | 124 / 0 decision mismatches | 0.0 (bit-exact) |
| 102 low end | 1685 / 0 | 203 / 0, landmark diff 0.0 | 167 / 0 | 1.5e-3 (float-summation order; decisions equal) |

The descriptor function alone was also compared on random patches: identical 128-byte descriptors.

**Full-frame detection** (min face 48, merge on; SCRFD det_10g from `models/` as reference; landmark error =
mean distance of our landmarks 0–4 to SCRFD's 5 points / SCRFD inter-ocular distance):

| frame | SCRFD faces | 101 infrared | 102 low end |
|---|---|---|---|
| watch_0014 (owner, IR) | 1 | 1 face, HoG 8.44, IoU 0.64, NME 0.040 | 1 face, 6.61, IoU 0.50, NME 0.094 |
| watch_0147 | 1 | 1, 8.78, 0.65, 0.035 | 1, 7.04, 0.53, 0.095 |
| watch_0258 | 1 | 1, 8.56, 0.67, 0.031 | 1, 6.71, 0.52, 0.078 |
| watch_0435 | 1 | 1, 9.07, 0.67, 0.037 | 1, 6.96, 0.52, 0.099 |
| ir_11/13/15/17/19 (face cut at the chin) | 1 (box extends below the frame) | 0 | 0 |
| strangers photo (gray, 1280×886) | 6 | 6, no false positives, NME 0.036–0.16 | 6, NME 0.045–0.17 |

IoU is low by construction: the Microsoft box is a square ~1.1× the inner landmark span (eyes–mouth), SCRFD's
includes forehead and chin. The ir_NN frames contain only the upper half of the face; none of the Microsoft
detectors fires there (the ORT models fire with conf 0.9998 but are rejected because the mouth landmarks are
outside the frame — the same rule as `ShouldRejectCandidate`). Runtime of the numpy port: 2.5–3.8 s per 640×360
frame (101), 7 s for the 1280×886 photo; not optimised.

ORT detectors (`retina.py`, raw input; histogram-equalised input gives the same picture):

| frame | 110 (=112) | 111 (=113) |
|---|---|---|
| watch_* (4 frames) | 1 face each, conf 1.0, IoU vs SCRFD 0.91–0.92, NME 0.022–0.035, rotation ≈ (−11…−14°, 2…5°, −2…−3°) | 1 face each, conf 0.95, IoU 0.85–0.93, NME 0.022–0.042 |
| ir_NN | 0 (rejected: landmarks outside frame) | 0 |
| strangers | 6/6, IoU 0.84–0.91, NME 0.06–0.20 | 2/6 (type-1 model uses strides 32/64, i.e. meant for larger faces/frames) |

## 10. Verified vs assumed, open questions

Verified (bit-exact against the DLL): LDMF parsing; fern pre-filter; JDA stages 0–4 incl. similarity transforms;
SIFT descriptor; post-filter score/decision (101 exact, 102 within 1.5e-3); pose decode formula.
Derived from code but not executed against the DLL: pyramid construction, scan schedule, landmark→box,
merge, RetinaEngine decode/NMS. Assumed / open:
- `max_face` layer limit: the DLL bounds layers via `ImageMetadata::Initialize` 0x18000358c with arguments not
  traced; we use face size ≤ min(H, W, 2584).
- Window-tracker suppression and layer prioritisation (§5) are omitted.
- Names of the 31/27 landmarks beyond 0–4, and of the three pose angles.
- Which tracker is the "helper" in `FaceDetectorWithHelper` and the exact feature-flag gating of RetinaEngine.
- `ShouldRejectCandidate`'s orientation part (`GetFrameOrientation`) and search-area containment test.
- The meaning of header +0x60 (timestamp is a guess) and the unused 4-byte vector.

## Finding ledger

| id | finding | status | evidence |
|---|---|---|---|
| L1 | LDMF = 360-B header + SACD cascade + SOPD post filter; parses to EOF | OBSERVED | Read 0x180001bd8/0x180002450/0x180001974; `format.py` |
| L2 | stage 0 + 400 ferns are compiled code, not data | OBSERVED | Boost/FernPredict functions; header text of 102 |
| L3 | type tags 0xFACE0C06 / 0xFACE4DB5 select IR / low-end parameters | OBSERVED | AssignCascadeModel 0x18009c170 |
| L4 | 101 = DetectionModel 1 (FaceTrackerInternalType 0), 102 = DetectionModel 3 (type 1) | OBSERVED | InitializeModelsUsingFilePath 0x1800367ec, FaceTracker::Initialize 0x180035f10 |
| L5 | Python port bit-identical per window | OBSERVED | §9 emulator table |
| L6 | descriptor uses cvRound (cvtss2si) for centre/radius and output; Hex-Rays prints `(int)` | OBSERVED | asm 0x18000a87a, 0x18000a8f6, 0x18000b620 |
| L7 | fern counter counts milestones (1,2,4,…,256,399,400), dense rescan at ≥ 8 | OBSERVED | symbolic logs, DetectInLayer |
| L8 | 110–113 are XORed ORT RetinaFace-style models; 110≡112, 111≡113 | OBSERVED | cipher, onnxruntime outputs |
| L9 | RetinaEngine input = raw 0–255 floats, threshold 0.925, strides/anchors per type | OBSERVED | ProcessNextFrame, Initialize |
| L10 | 6-DoF output is img2pose-like (mean/std, K = W+H focal) | INFERRED | constants + DecodeRotationVec structure |
| L11 | pairing 101↔110/112, 102↔111/113 | INFERRED | shared type enum |
