# Windows Hello legacy IR landmark aligner (LBF) and head pose — format and converter

Research / interoperability notes. The converted files stay local and git-ignored
(`models/windows_hello/converted/lbf/`). linux_hello has a Rust port (see "Rust port") that reads the original
resources; it is an API and a diagnostic, not a frame gate.

Evidence tags: **OBSERVED** = seen in code (function + address cited, FaceProcessorCore.dll
10.0.26100.9444 with public PDB unless noted), **INFERRED** = reasoning, **UNKNOWN** = not resolved.

## Summary

| Resource (facerecognitionsensoradapterresources.dll, type `MODEL_FILES`) | What it is | Loader |
|---|---|---|
| 102 (`MODEL_FILES_102.cefa`, 7,349,136 B) | **LbfAligner database**: 31-point IR landmark aligner (5-stage LBF cascade) + face/non-face classifier | `LbfAligner::Aligner::LoadDatabase` 0x180017d80 |
| 104 (33,140 B) | `PITCH_KNN`: 91 pitch class centroids (−45…45°) in a 90-d LDA space | `LDARegressor::LoadKNN` 0x180018810 |
| 105 (2,598,600 B) | `PITCH_LDA`: mean + 90 × 7139 LDA basis over an LBP feature | `LDARegressor::LoadLDA` 0x180018ad8 |
| 106 (58,580 B) | `YAW_KNN`: 121 yaw class centroids (−60…60°) in a 120-d LDA space | `LoadKNN` |
| 107 (3,455,280 B) | `YAW_LDA`: mean + 120 × 7139 LDA basis | `LoadLDA` |

**Correction to earlier notes** (README/manifest, `windows-hello-re.md` inventory): 104/106 are *not*
"landmark tables" and 105/107 are *not* LBF float tables. OBSERVED in
`FaceProcessor::InitializeLegacyFaceAlignmentAndFaceOrientationIfNeeded` (0x18003a954): resource 102 goes to
`CreateFaceAlignment` (→ `LbfAligner::Aligner`), and 104/105/106/107 go to `CreateIrFaceOrientation`; the error
strings name them `…GetInputStreamForResourcePITCH_KNN`, `…PITCH_LDA`, `…YAW_KNN`, `…YAW_LDA` in that order.
The `.cefa` extension came from the first bytes `CE FA` = little-endian magic `0xFACE`.
Also: the F13 third anchor group is **mouth** points (3, 4, 23–26), not nose/mouth (see "Landmarks → crops").

What converts and how well it is verified:

* Both formats parse exactly to end of file. The full aligner, the head-pose estimator and the
  engine's 128×128 CNN input crop are reimplemented in numpy and are **bit-exact** against Microsoft's own
  x64 code run under emulation (Unicorn) on all 9 owner IR frames.
* The head-pose LDA + nearest-centroid stage is also exported as ONNX (`headpose_lda_knn.onnx`) and gives
  identical angles in ONNX Runtime. The LBF cascade is a pixel-sampling tree ensemble, not a neural
  net; it is kept as numpy arrays + JSON + the Python implementation (no ONNX graph).
* On the owner's IR frames, started from the SCRFD box, the 5 main landmarks are within **2.1 px mean
  (NME 0.037 of inter-ocular distance)** of SCRFD's 5 keypoints; the classifier accepts all 9 faces and rejects
  all 46 background boxes tried.

## Package and usage

Code: `tools/winhello_models/lbf/` (run with the shared venv; `PYTHONPATH=tools/winhello_models` or `cd` there).

| File | Content |
|---|---|
| `format.py` | parsers: `parse_lbf()`, `parse_lda()`, `parse_knn()`, `load_lda_regressor()` |
| `aligner.py` | LbfAligner: `align(model, img, rect=… / prev=…)` → (31×2 points, classifier score) |
| `pose.py` | IrFaceOrientation: `head_pose(img, points, yaw, pitch[, prior_yaw, prior_pitch])` |
| `crop.py` | engine 128×128 liveness crop: `engine_crop_128(gray, pts5)` |
| `emu.py` | validation only: maps `FaceProcessorCore.dll` / `FaceRecognitionEngineAdapter.dll` into Unicorn and calls `Aligner::Predict`, `IrFaceOrientation::ComputeOrientation`, ENG `sub_180015F68` |
| `scrfd.py` | minimal SCRFD (`models/det_10g.onnx`) decoder used to make start boxes |
| `__main__.py` | CLI |

```
cd tools/winhello_models
.venv/bin/python -m lbf convert                 # -> models/windows_hello/converted/lbf/*
.venv/bin/python -m lbf align IMG.png [--box L T R B] [--track N] [--overlay out.png]
.venv/bin/python -m lbf validate [--no-emu]     # owner IR frames, overlays, validation.json
```

Outputs in `models/windows_hello/converted/lbf/`: `lbf_aligner.npz` + `lbf_aligner.json` (mean shape,
per-stage node arrays `stageN_nodes` (310×31 records), int16 leaf weights `stageN_weights_i16` + scale, classifier
weights), `headpose_lda_knn.npz/.json/.onnx`, `overlay_*.png`, `engine_crops_128.png`,
`overlay_strangers_rgb_gray.png`, `validation.json`.

Extra dependency: `unicorn==2.1.4` was installed into the shared venv
(`uv pip install --python tools/winhello_models/.venv/bin/python unicorn`); only `emu.py` needs it.
`emu.py` also needs the original DLLs under `~/SiteResearch/windows_hello/re/bin/`.

## Format: MODEL_FILES 102 (LbfAligner database)

Little-endian. OBSERVED in `Aligner::LoadDatabase` 0x180017d80, `Stage::Init` 0x180018c30, `Tree::Init`
0x180018d84, `Regressor::Init` 0x180017bc8, `Classifier::Initialize` 0x180043194.

| Offset | Type | Field | Value in this file |
|---|---|---|---|
| 0 | u16 | magic, must be `0xFACE` | `CE FA` |
| 2 | u8 | version, must be 10 | 10 |
| 3 | 7 bytes | read but unused (part of the 10-byte header read) | `00 02 00 90 23 70 00` (UNKNOWN meaning) |
| 10 | u32 | `num_points` N (Aligner+8; context stores ≤ 31 points, `UpdateFacePoints` checks 2N ≤ 62) | 31 |
| 14 | u32 | `num_stages` S (Aligner+32) | 5 |
| 18 | f32[2N] | mean shape (x, y) in the unit face box (Aligner+16) | see `lbf_aligner.json` |
| … | Stage × S | cascade stages | |
| … | u32 | classifier flag, must be 1 if present (otherwise error) | 1 |
| … | Regressor | classifier regressor (1 column), evaluated on the **last stage's trees** | rows 9916, float32 |

Stage (64-byte object):

| Type | Field | Value |
|---|---|---|
| u32 | T = number of trees | 310 |
| u32 | K = trees per landmark (object stores N_stage = T / K at +4) | 10 |
| Tree × T | `i32 node_count`, then `node_count` × 24-byte nodes | 31 nodes (depth-5 binary trees, 32 leaves) |
| Regressor | leaf regression matrix | |

Node (24 bytes):

| Offset | Type | Field |
|---|---|---|
| 0 | f32 x1, y1 | first pixel offset, in mean-shape (unit box) units, relative to the tree's landmark |
| 8 | f32 x2, y2 | second pixel offset |
| 16 | f32 | threshold |
| 20 | u32 | children: low 16 bits = child if `threshold >= feature`, high 16 bits otherwise; in each half bit 15 = leaf flag, bits 0–14 = node index (internal) or **global leaf row** (leaf) |

Leaf rows are numbered globally across the stage (tree t uses rows 32t…32t+31; a few trees are pruned, so the
last stages have 9916–9920 rows); the regressor has one extra last row = bias.

Regressor:

| Type | Field |
|---|---|
| u8 | compressed flag (Regressor+0) |
| u32 | rows R (+28) |
| u32 | cols C (+32) — 62 (= 2N) for stages, 1 for the classifier |
| f32 | scale (+24), only if compressed |
| i16[R·C] or f32[R·C] | weights (compressed: value = int16 × scale) |

Stage regressors are compressed int16 (scales 4.6e-7 … 6.8e-8); the classifier is float32.
Total: 18 + 248 + 5 stages + 4 + 9 + 9916·4 = 7,349,136 bytes = file size.

## Algorithm: LbfAligner (OBSERVED)

`Aligner::Predict` 0x180010cb0 (called by `ComputeAlignment` 0x180010be0 for detection, `TrackFace` 0x180042fe0
for tracking). Input: 8-bit luminance **full lit frame** (`FaceProcessor`+56, `LuminanceImage` {+8 w, +12 h,
+16 stride, +24 data}) and a `FaceDetectionResult` RECT.

1. Initial shape.
   * Detection: `ComputeSimilarityTransformCoefficient` 0x180011c10 fits a least-squares similarity from the unit
     square (0,0),(1,0),(0,1),(1,1) (Aligner+96, set in `LoadDatabase`) to the rect corners
     (left,top),(right−1,top),(left,bottom−1),(right−1,bottom−1); `AffineTransform` 0x180011b8c maps the mean shape.
   * Tracking (`a6 = true`): `ComputeDifferencialSimilarityTransformationCoefficient` 0x180011534 maps the mean
     shape onto the previous points.
2. For each stage: `M = ComputeDifferencialSimilarityTransformationCoefficient(mean, shape)`: centre both
   shapes, scale the current shape per axis to the mean's x/y spread, fit a similarity, keep only its rotation,
   then apply separate x and y scales and a translation. Only M's 2×2 part is used below.
3. Every tree t works on landmark ⌊t/K⌋ = (lx, ly). At each node, pixel i = (⌊M00·xi + lx + M01·yi + 0.5⌋,
   ⌊M10·xi + ly + M11·yi + 0.5⌋) (C truncation, float32); a pixel outside the image reads 0.
   Feature = (I1 − I2) / (I1 + I2 + FLT_MIN). Go to the low child if `threshold >= feature`.
4. `Regressor::Predict` 0x1800114c0 (→ `PredictCompressed` 0x180011ed0): sum the reached leaf rows (int16 × scale)
   + bias row → Δ (62 values, mean-shape units). Shape += M₂ₓ₂ · Δ per point.
5. After the last stage, `Classifier::Predict` 0x18001073c re-runs the last stage's trees on the final shape and
   sums the 1-column classifier rows; score ≤ 0 → flag set (`AlignerContext`+260 → `FaceAlignmentResult`+252,
   i.e. FPO+276). `TrackFace` returns E_FAIL on that flag; `ProcessTrackedFaces` (0x180014db4) sets
   `FaceProcessor`+288 when the flag is clear (INFERRED: "a confirmed face exists").

`FaceProcessor::ProcessTrackedFaces` runs this aligner only on the legacy path. RetinaEngine (ONNX detector with a
`landmarks` output, `IsRetinaFaceAlignmentEnabled`) is the alternative: `RetinaEngine::ComputeAlignment`
0x18003d270 copies its **5 points into slots 0–4** of the same 31-slot result and fills slots 5–30 from them
(see "Which aligner runs"). So slots 0–4 = RetinaFace 5-point convention (OBSERVED copy; semantic INFERRED).

### Which aligner runs, and where landmarks 5–30 come from on the RetinaEngine path

OBSERVED (FaceProcessorCore, pseudocode and disassembly in `~/SiteResearch/windows_hello/re/conv/landmarks/`):

* `FaceProcessor` holds one `IFaceAlignment` (this+11, its shared_ptr control block at +12) and one
  `IFaceOrientation` (+14/+15). `ProcessTrackedFaces` 0x180014db4 calls, per tracked face, context `SetParameter`
  (vtable +8), then **one** `ComputeAlignment` (vtable +16 of this+11), then `AlignerContext::GetResult`
  (`…InfraredAlignerContextGetResult`). There is no second alignment pass.
* `InfraredFaceProcessor::Initialize` 0x18002e120 creates the `RetinaEngine` (feature `OnnxInfraredFaceDetector`)
  and, when `StaticAdapterInfo::IsRetinaFaceAlignmentEnabled` 0x18002f72c is true, installs the engine's
  `IFaceAlignment` sub-object (RetinaEngine+8) at this+11; when the byte at `StaticAdapterInfo`+2938 is set, its
  `IFaceOrientation` (RetinaEngine+16) at this+14. `IsRetinaFaceAlignmentEnabled` returns 1 whenever feature
  `IntegrateComplianceHelloModels` is on, else the byte at `StaticAdapterInfo`+2937 (0 in the constructor
  0x18002f4a8, `dword_180067988 = 0x10000`, which also sets +2938 = 1).
* **Which path a stock install runs (OBSERVED, F64).** Nothing writes +2937: the `StaticAdapterInfo` setters
  that exist cover the neighbouring bytes only — `SetRotateFrameToDetectFaceEnabled` +2936,
  `SetRetinaSearchAreaOptimizationEnabled` +2939, `SetAverageAggregationEnabled` +2940,
  `SetFaceOcclusionDetectorEnabled` +2942, `SetInfraredROIControlImprovementEnabled` +2943 (FaceRecognitionSensorAdapterVsm
  0x18002fd84, 0x18002fd24, 0x18002fa80, 0x18002fb3c, 0x18002fc64; the plain sensor adapter writes +2939…+2945 by the
  same offsets, 0x18002e214…0x18002e4b8, and +2941/+2945 directly), and the literal offset 2937 appears in no pseudocode dump of
  FaceProcessor, FaceProcessorCore or the three sensor adapters except the two copies of the getter (FaceProcessorCore
  0x18002f72c, FaceProcessor 0x18007b864). No registry or WinBio string names it. So the byte stays 0 and the
  decision is the feature flag alone. The flag is a WIL velocity feature, id **60805723** (0x39FD25B), compiled
  **enabled by default**: `FeatureImpl<…IntegrateComplianceHelloModels>::GetCurrentFeatureEnabledState` 0x18002c3dc
  treats `GetFeatureEnabledState` = 0 (no override) as enabled (`v8 = 64` on the default branch; compare
  `ImproveFaceQualityFilter` 0x180030d54, id 61462116, whose default branch leaves the bit clear = disabled by
  default), and requires `TestLoc03` (id 58989177, 0x18002c5e4, itself enabled by default). INFERRED: a stock
  26100.9444 install runs the **RetinaEngine alignment path** (slots 5–30 copied from the five Retina points, eye
  corners at ±0.1875·IOD) and never the LBF aligner, unless the OS feature store
  (`HKLM\SYSTEM\CurrentControlSet\Control\FeatureManagement\Overrides`, feature 60805723) turns the flag off,
  which is a per-machine state that static analysis cannot see.
* `InitializeLegacyFaceAlignmentAndFaceOrientationIfNeeded` 0x18003a954 loads MODEL_FILES 102 and creates the
  `LbfAligner` **only if this+11 is still null** (`if (this+11 != 0) goto LABEL_69`), and `IrFaceOrientation` only
  if this+14 is null. So with Retina alignment the LBF database is never loaded and the aligner never runs.
* `RetinaEngine::ComputeAlignment` 0x18003d270 takes the cached candidate of the context's tracking id
  (`FindCachedResultCandidate`), requires exactly 5 landmarks (40 bytes), clamps them into the frame rect and
  writes them to slots 0–4 of the 62-float result; then, with `IntegrateComplianceHelloModels` on:
  * `d = (p1 − p0) · 0.75 · 0.5 · 0.5 = 0.1875 · (p1 − p0)` per axis; p7 = p0 − d, p10 = p0 + d, p13 = p1 − d,
    p16 = p1 + d (eye corners along the eye line, clamped to the rect);
  * p5, p6, p8, p9 = p0; p11, p12, p14, p15 = p1 (brows and eye top/bottom = eye centres);

  and with the feature off: p5–p10 = p0, p11–p16 = p1. In both cases p17–p22 and p27–p30 = p2 (nose points = nose
  tip) and p23–p26 = (p3 + p4) / 2 (mouth points = mouth-corner midpoint). Then `UpdateFacePoints(ctx, 0, 2·N, pts)`
  with N from vtable +40 (31).

Consequences (OBSERVED arithmetic on the copies above):

* F43 brightness block (p3, p4, p9, p15): p9 = p0, p15 = p1 on either Retina variant, so the block is bounded by
  the **eye centres and mouth corners** of the five detector points — the block linux_hello used with SCRFD's five
  points, now taken from the Retina points.
* F13 anchors: A = mean(p0, p7, p10, p8, p9) = p0 (the ±d corners cancel), B = p1, C = mean(p3, p4, 4 × midpoint)
  = (p3 + p4) / 2. The affine warp is fitted through the eye centres and the mouth midpoint.
* A stock install runs the Retina path (the flag is compiled enabled by default and the byte has no writer, above).
  linux_hello follows it: its other converted models are the compliance-flag variants (occlusion 120, detector
  112 ≡ 110, cefa.md §5), under which `IsRetinaFaceAlignmentEnabled` is unconditionally true.

### Point layout (names INFERRED from mean shape + anchor groups; "L" = image left = subject's right)

| # | name | # | name | # | name |
|---|---|---|---|---|---|
| 0 | eye centre L | 11 | brow inner R | 22 | nose wing R |
| 1 | eye centre R | 12 | brow outer R | 23 | upper lip top (mid) |
| 2 | nose tip | 13 | eye inner corner R | 24 | upper lip bottom |
| 3 | mouth corner L | 14 | eye top R | 25 | lower lip top |
| 4 | mouth corner R | 15 | eye bottom R | 26 | lower lip bottom |
| 5 | brow outer L | 16 | eye outer corner R | 27 | glabella |
| 6 | brow inner L | 17 | nose root L | 28 | nostril L |
| 7 | eye outer corner L | 18 | nose root R | 29 | nose bottom (mid) |
| 8 | eye top L | 19 | nose side upper L | 30 | nostril R |
| 9 | eye bottom L | 20 | nose side upper R | | |
| 10 | eye inner corner L | 21 | nose wing L | | |

The public PDB is stripped of types, so the real `FaceAlignmentPoints` enum names are UNKNOWN.

## Format: MODEL_FILES 104–107 (IrFaceOrientation)

`CreateIrFaceOrientation` 0x180043278 → `IrFaceOrientation::Initialize` 0x180022754 →
`CreateRegressor(yaw, 107, 106)` and `CreateRegressor(pitch, 105, 104)` (0x18001e280).

LDA file (`LoadLDA` 0x180018ad8):

| Offset | Type | Field |
|---|---|---|
| 0 | u32 | D (LDA dimension): 90 pitch, 120 yaw |
| 4 | f32[7139] | feature mean (read as 28556 bytes) |
| 28560 | f32[D][7139] | basis rows (stored in memory with a 7168-float stride) |

KNN file (`LoadKNN` 0x180018810):

| Offset | Type | Field | pitch / yaw |
|---|---|---|---|
| 0 | u32 | k (LDARegressor+40) | 1 / 1 |
| 4 | u32 | γ (+44), weight `exp(−γ·dist)` on the prior path | 90 / 90 |
| 8 | u32 | N centroids (+48) | 91 / 121 |
| 12 | u32 | M centroid dim (+52), must equal D | 90 / 120 |
| 16 | f32[N] | label per centroid (degrees) | −45…45 / −60…60 step 1 |
| 16+4N | f32[N][M] | centroids in LDA space | |

## Algorithm: IrFaceOrientation (OBSERVED, `ComputeOrientation` 0x18000cfe0)

1. `CalculateSimilarityTransform` 0x18000d4c8: least-squares similarity from the first min(N, 27) aligner points to a
   fixed 27-point template in a 64×64 image (constants in `Initialize` 0x180022754: rdata 0x18004b530…0x18004b5f0 +
   immediates 0x4201FEFA/0x42379FBE; table in `pose.py`).
2. `ApplySimilarityTransformToImage` 0x18000c010: inverse-map a 64×64 float face with
   `BilinearInterpolationWithPadding<uchar>` 0x18000c6b0 (outside → 0), × 1/255 (0x3b808081).
3. `DifferenceOfGaussainSigma2MinusSigma4` 0x18000d6dc: G(σ2, 13 taps) − G(σ4, 25 taps), kernels at rdata
   0x18004b1a0 / 0x18004b1e0. As executed, both filters are separable with mirror padding that repeats the edge
   sample, **but the horizontal pass only computes columns 4g and 4g+1 and writes copies of them into columns
   4g+2 and 4g+3** (found by impulse tests on the emulated code; numpy model matches the x64 code to 1.8e-7 on
   random input). INFERRED: a lane-shuffle bug in the hand-written SSE; the models were presumably trained with it.
4. `EncodeLBPFast` 0x18000ca00: 8-neighbour LBP at radius 1 on the DoG image, bit set when neighbour > centre,
   bit order TL, T, TR, R, BR, B, BL, L; codes mapped by `lbpLookUpTable` (0x18004b0a0) = the standard 59-bin
   uniform (u2) table (verified byte for byte); only x, y ∈ [2, 62) coded, rest 0.
5. `ComputeOrientationLBPFeature` 0x18000ce90 with `LbpConfig {10,10,11,11,64,64,1,1,1}`: 11×11 overlapping 10×10
   windows at stride 5 starting at (2, 2), 59-bin histograms → 7139 counts; × 0.01 → float feature.
6. `LDARegressor::compute` 0x18000ba00 per angle: `Project` 0x18000bb20 (x − mean)·basisᵢ; `Regress`:
   * no prior (previous estimate older than 0x325AA0 × 100 ns = 330 ms, or none): k = 1 → label of the nearest
     centroid (Euclidean, first minimum) — 0x18000b498;
   * with prior: first label > prior (else last), window ±10 centroids, Σ exp(−γ·dᵢ)·labelᵢ / Σ exp(−γ·dᵢ) —
     0x18000bdc0.
7. Flags (FaceOrientationResult +12..15): yaw > 20, yaw < −20, pitch > 25, pitch < −25.
   `IsFrontal` 0x18001a404 = no flag set. `ComputeHeadOrientation` 0x180006b54 swaps the two yaw flags when the
   byte at `*(FaceProcessor+32)+33` is 0 (INFERRED: a "pixels flipped horizontally" frame flag; the string
   `wasPixelsFlippedHorizontally` exists in the DLL).
8. If no flag is set, `EstimateFaceFrontalFromAlignmentPoints` 0x18000b640: quad (p0, p1, p4, p3); the nose p2
   must be inside; 5 bisection steps shrink the quad toward opposite corners (t from 0.25 within [0, 0.5]); if the
   resulting margin t < 0.125, the edge closest to the nose (in the last tested quad) sets a flag
   (eye edge → pitch>25, right side → yaw<−20, mouth edge → pitch<−25, left side → yaw>20). t is stored at +16.
9. FaceOrientationResult = {f32 pitch, f32 yaw, f32 "roll", u8 flags[4], f32 frontal margin}. "Roll" =
   2·acos(w) of `XMQuaternionRotationMatrix` (0x18000c87c, normalising) of the image→template similarity; it is
   unsigned and shrunk by the scale, so it is not a true roll angle (our value agrees to ≤ 0.05° because
   `XMScalarACos` is a polynomial approximation).

## Landmarks → crops used by the liveness / recognition nets

* Ambient-light anchors (`AmbientLightSubtracterBase::ComputeAnchorPoints` 0x1800372c8, FPO+256488 = alignment
  result): A = mean(p0, p7, p10, p8, p9), B = mean(p1, p16, p13, p14, p15), C = mean(p3, p4, p23, p24, p25, p26),
  each + crop origin (FPO+24/+28). OBSERVED; this corrects "6 nose/mouth landmarks" in F13 to mouth points.
* Face brightness box (F43) uses p3, p4, p9, p15.
* Engine 128×128 CNN input (FaceRecognitionEngineAdapter.dll, no PDB): `sub_180015F68(img, pts, w, h, stride,
  128, 128, ch, out)` → `sub_18001614C` least-squares similarity from the **first 5 points** to 5 reference points
  in a 281×281 canvas (rdata 0x1800D7F68: (97.8299, 127.7792), (181.0532, 126.3381), (139.574, 179.0006),
  (102.6964, 216.7014), (179.3068, 215.3733)) → `sub_180017200` inverse → `sub_180016B20` bilinear warp into the
  canvas (uint8 truncation, outside → 0) → `sub_180016440` resample the canvas at (i − 64)·230/127 + 140.5 to
  128×128. Called by `RunDualIRAntiSpoofingClassifier` (0x18004a6d0) with the liveness crop at record+333 and
  points at record+44 (1 channel) and by the colour classifier (0x180049ec0, 3 channels).
  `crop.engine_crop_128` reproduces it bit-exact (0 differing pixels vs emulation, 9 frames).
  INFERRED: the recognition crop (`sub_180015EF0`) uses the same routine.

## Validation

Data: the 9 lit owner IR frames (`ir_{11,13,15,17,19}_lit.png`, `watch_{0014,0147,0258,0435}_ir_lit.png`,
640×360). Start box = SCRFD det_10g box as-is (no tuning). In the `ir_*` frames the chin and part of the mouth
are below the frame edge.

1. Microsoft code vs numpy (emulated `Aligner::Predict`, `ComputeOrientation`, engine crop), all 9 frames:
   alignment (detection and one tracking pass) max |Δ| = **0.0**, accept/reject flag equal, 7139-d LBP feature
   max |Δ| = **0.0**, pitch/yaw/flags/frontal margin equal (also on the prior path), engine crop max |Δ| = **0**.
2. Landmarks vs SCRFD 5 keypoints (pixels; IOD = SCRFD inter-ocular distance):

| frame | IOD | eye L | eye R | nose | mouth L | mouth R | mean | classifier | pitch / yaw | frontal |
|---|---|---|---|---|---|---|---|---|---|---|
| ir_11_lit.png | 59.7 | 0.59 | 4.87 | 1.11 | 1.16 | 2.91 | 2.13 | 1.197 | -30 / 7 | no |
| ir_13_lit.png | 59.4 | 1.08 | 2.15 | 2.68 | 2.01 | 0.72 | 1.73 | 1.5466 | -26 / 7 | no |
| ir_15_lit.png | 59.6 | 1.54 | 3.8 | 1.31 | 1.31 | 1.61 | 1.91 | 1.2798 | -14 / 7 | yes |
| ir_17_lit.png | 59.3 | 1.28 | 6.55 | 2.04 | 1.52 | 3.21 | 2.92 | 0.9138 | -30 / 7 | no |
| ir_19_lit.png | 59.7 | 0.79 | 4.04 | 1.12 | 3.0 | 3.62 | 2.52 | 2.554 | -26 / 7 | no |
| watch_0014_ir_lit.png | 51.2 | 3.51 | 2.12 | 1.35 | 1.43 | 2.21 | 2.13 | 4.3786 | -9 / 26 | no |
| watch_0147_ir_lit.png | 51.7 | 4.39 | 1.44 | 0.18 | 1.26 | 2.79 | 2.01 | 5.7526 | -9 / 26 | no |
| watch_0258_ir_lit.png | 51.0 | 0.89 | 1.84 | 1.23 | 1.14 | 2.48 | 1.52 | 6.6197 | -9 / 24 | no |
| watch_0435_ir_lit.png | 51.8 | 1.83 | 0.44 | 3.47 | 1.55 | 2.57 | 1.97 | 5.0243 | -15 / 24 | no |

Mean 2.09 px (eye L 1.77, eye R 3.03, nose 1.61, mouth L 1.60, mouth R 2.46), NME 0.037. One extra tracking pass
(re-init from the result, as `TrackFace` does on the next frame) gives 2.98 px. Visual check of
`overlay_*.png`: eye contours, brows, nose and lip points sit on the right features.
3. Classifier: all 9 faces accepted (scores 0.9–6.6); 46 non-overlapping background boxes in `watch_0147` all
   rejected (max score −4.6).
4. Out-of-domain check (`strangers_insightface_t1.jpg`, visible-light photo converted to gray): the 3 near-frontal
   faces align (score 2.7–5.7, 2.8–4.6 px error) and the 3 tilted/turned faces fail and are rejected by the
   classifier (score < 0) — `overlay_strangers_rgb_gray.png`.
5. ONNX head pose (`headpose_lda_knn.onnx`, ORT 1.31) returns the same pitch/yaw as numpy on all frames.

Head pose on this camera: the yaw regressor gives +24…26° on the visually frontal `watch_*` faces, and the value
does not change sign when the frame is mirrored, so it reflects a domain shift (illumination/sensor) rather than
geometry; the `ir_*` frames get pitch −26…−30° (chin out of frame). Because the numpy code matches the x64 code
exactly given the same landmarks, this is what the legacy path would compute on these inputs. Whether Windows
feeds this path differently (orientation/mirroring, other preprocessing) is UNKNOWN.

## Rust port (`src/ms_models/lbf.rs`, `src/ms_models/pose.rs`)

* `LbfAligner::load(MODEL_FILES_102.cefa)` parses the database as above (to EOF) and
  `align(lit, [left, top, right, bottom])` runs the detection mode of `Aligner::Predict` → 31 points + classifier
  score (`Alignment::accepted` = score > 0). Tracking init is not ported (no tracker).
* `IrFaceOrientation::load(dir)` reads `MODEL_FILES_104..107.bin`; `estimate(lit, points)` returns the
  `FaceOrientationResult` (pitch, yaw, "roll", the 4 flags, frontal margin). Only the no-prior regression is ported
  (the prior path needs the same tracked face's estimate from the last 330 ms). `headpose_lda_knn.onnx` is not used.
* Inputs used in linux_hello: the SCRFD box truncated to integers (`as i32`, like `int()` in the Python tools) on the
  8-bit lit frame. Exposed in `examples/ms_check` (`pose.legacy`, and its eye contour as the `lbf` brightness
  block); **not used in the pipeline**: Windows' Retina path never runs it ("Which aligner runs"), and as a pose
  gate it reads +24…26° of yaw on frontal faces from this camera (see Validation), so Windows' ±20° limit would
  reject the owner. The frontal gate uses the RetinaEngine pose instead (`ldmf.md` §11).

Verification: Rust vs `aligner.py` + `pose.py`, same image and same integer rect, on the 9 owner IR frames and the
6 faces of the stranger photo (converted to gray with PIL and saved as PNG, so both read identical pixels):
31 points max |Δ| **0.0**, classifier score |Δ| **0.0**, accept flag equal, pitch/yaw equal, roll |Δ| 0.0,
flags and frontal margin equal on all 15 faces. The values reproduce the Validation table above (e.g.
watch_0014: score 4.3786, pitch −9 / yaw 26; ir_15: frontal, margin 0.305). Run time 1.3–2.0 ms per face (release).
The LDA projection is summed in f64 and the centroid distance in f32; both only feed the nearest-centroid choice,
which was equal everywhere.

## Verified vs assumed

* Verified (bit-exact vs Microsoft code): parsers, aligner (both init modes), classifier decision, head-pose
  feature and angles (both regress paths), frontal flags/margin, engine 128×128 crop.
* Assumed: SCRFD's box stands in for the Microsoft IR detector box (`FaceDetectionResult` from FaceTrackerInternal);
  the real detector's box convention is UNKNOWN and may shift the start shape (results above are with the raw
  SCRFD box). Landmark names are inferred. The emulated harness builds the C++ objects by hand with the
  layouts recovered from the loaders (not by running `LoadDatabase` on a `std::istream`).

## Open questions (UNKNOWN)

* Meaning of header bytes 3–9 of MODEL_FILES 102 (`00 02 00 90 23 70 00`); `LoadDatabase` reads but ignores them.
* Whether a given Windows install uses this legacy path or RetinaEngine: `IsRetinaFaceAlignmentEnabled` is forced
  on by feature `IntegrateComplianceHelloModels`, otherwise read from a configuration byte whose writer was not
  traced ("Which aligner runs"); the legacy models still ship and load on the legacy path.
* Exact detector box convention for the IR detector (FaceTrackerInternal) that seeds the aligner.
* The k > 1 KNN branch of `LDARegressor::Regress` is not implemented (the shipped models use k = 1).
* Why the head-pose yaw is biased on this camera (see Validation).

## Finding ledger

| # | Tag | Finding | Evidence |
|---|---|---|---|
| L1 | OBSERVED | 102 = LbfAligner DB (magic 0xFACE, version 10); 104/105/106/107 = PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA | 0x18003a954, 0x180017d80 |
| L2 | OBSERVED | 31 points, 5 stages × 310 depth-5 trees (10 per point), int16 leaf regressors, float classifier on last-stage leaves | parse to EOF; 0x180018c30, 0x180017bc8, 0x180043194 |
| L3 | OBSERVED | pixel-difference feature (I1−I2)/(I1+I2+FLT_MIN), offsets in mean-shape units rotated/scaled by the mean→shape similarity | 0x180010cb0, 0x180011534 |
| L4 | OBSERVED | head pose = 64×64 warp → DoG(σ2−σ4) → uniform LBP 11×11×59 → LDA → nearest centroid; prior path ±10 window, exp(−90·d) | 0x18000cfe0, 0x18000ba00, 0x18000bdc0 |
| L5 | OBSERVED | frontal = |yaw| ≤ 20 and |pitch| ≤ 25 and nose inside the eye/mouth quad with margin ≥ 0.125 | 0x18000cfe0, 0x18000b640, 0x18001a404 |
| L6 | OBSERVED (emulation) | Gaussian horizontal pass writes columns 4g, 4g+1 into 4g+2, 4g+3 | impulse tests on 0x18000d7a0/0x18000e8a0 |
| L7 | OBSERVED | ambient anchors: eye L (0,7,8,9,10), eye R (1,13,14,15,16), mouth (3,4,23,24,25,26) | 0x1800372c8 |
| L8 | OBSERVED | engine 128×128 crop from first 5 points via 281×281 canvas, ref points at ENG 0x1800D7F68 | ENG 0x180015f68, 0x18001614c, 0x180016b20, 0x180016440 |
| L9 | OBSERVED | RetinaEngine fills slots 0–4 with its 5 points | 0x18003d270 |
| L10 | INFERRED | slots 0–4 = eyes, nose, mouth corners (RetinaFace order); other names from geometry | L7, L9, mean shape |
| L11 | OBSERVED | one `ComputeAlignment` per face; the LBF aligner is created only when no Retina alignment is installed; Retina alignment is forced by `IntegrateComplianceHelloModels`, else a config byte | 0x180014db4, 0x18003a954, 0x18002e120, 0x18002f72c |
| L12 | OBSERVED | Retina path fills slots 5–30 by copying: eye slots = eye centres (corners ±0.1875·IOD with the compliance flag), nose slots = nose tip, mouth slots 23–26 = mouth midpoint; hence F43 block = eye centres/mouth corners and F13 anchors = eye centres + mouth midpoint | 0x18003d270 |
| L13 | OBSERVED | ambient anchors = `cur·(1−t') + prev·t'`, `t' = (t_amb − t_prev)/(t_cur − t_prev)` (prev = last lit frame, this+5; t_amb = this+6 set by `CropAndStoreAmbientImage`; t_cur = frame+16, also checked `< 300 ms` after prev); affine cur → interpolated through `DeriveTransform` (3 rows `[x, y, 1 | u, v]`, Gauss-Jordan); warped(p) = ambient(M·p) bilinear, zero padded, truncated | 0x180036c10 (asm 0x180036ca4–0x180036df2), 0x180036e78 (0x180037056–0x18003707f), 0x18003753c, 0x180032c34, 0x18002ad88 |

Scratch (IDA copies, test scripts, dumps): `~/SiteResearch/windows_hello/re/conv/conv-lbf/`; the alignment-path
and warp disassembly: `~/SiteResearch/windows_hello/re/conv/landmarks/` (`adjust.asm`).
