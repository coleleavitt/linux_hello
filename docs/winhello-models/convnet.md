# Windows Hello Face `conv_net_v1.0` (DNNTestLib) models

Research/interoperability notes for the Microsoft `conv_net_v1.0` model format used by
`FaceRecognitionEngineAdapter.dll` (Windows Hello Face). Outputs stay local and
git-ignored; linux_hello does not ship or use them.

- Converter: `tools/winhello_models/convnet/` (Python package, CLI `python -m convnet`).
- Converted models: `models/windows_hello/converted/convnet/MODEL_FILES_<id>.onnx` (+ `.json` sidecars).
- Validation crops and results: `models/windows_hello/converted/convnet/validation/`.
- RE scratch: `/home/cole/SiteResearch/windows_hello/re/conv/conv-convnet/` (copied IDA 9.5 databases, idalib query script).

Tags follow the main RE doc: **OBSERVED** = read from code or data (function and address
given), **INFERRED** = a conclusion from observed facts, **UNKNOWN** = open.
All addresses are in `FaceRecognitionEngineAdapter.dll` (ENG) unless another file is named.

## 1. Summary

| Item | Result |
|---|---|
| Files | `facerecognitionengineadapterresourcessecure/MODEL_FILES_115..119, 121..125.convnet` (10 files). They also cover the recognition nets in the v5 `cefa` containers (123/125, extracted by conv-cefa). |
| Architecture | All 10 anti-spoofing files are SqueezeNet 1.1 (conv1 3x3/2, 8 fire modules, avg pool) on a 128x128 input. 115–119 use Caffe layer names. 121–125 use PyTorch→ONNX names. |
| Converted | All 10 secure files, plus cefa 123/125 (172-layer Inception nets). ONNX opset 17, float32, NCHW. |
| ONNX vs NumPy reimplementation of DNNTestLib | Each layer was compared on random input. The largest relative error is 2.2e-6 over all 12 files (`python -m convnet check`). |
| Ablation | Swapping the kernel H/W axes, or transposing the image, turns the owner's "live" scores (0.74–1.00) into 0.00. This supports the derived weight and tensor layout. |

### Slot map (OBSERVED, loader `sub_18009D320`)

The loader opens `FaceRecognitionEngineAdapterResourcesSecure.dll`. For each slot it calls
`sub_18003CC50(..., L"MODEL_FILES", id)` and then `sub_1800C4B14(stream, slot)`. A feature flag
(`sub_18007AB90(&unk_1801246E8)`, name unknown) chooses between two complete sets:

| Slot | Classifier (F22 telemetry names) | Flag off | Flag on | Input image |
|---|---|---|---|---|
| 11 | general IR CNN v2 | 115 | 121 | aligned 128x128 crop of the liveness crop when it is **not** a difference image |
| 12 | "diffusion" IR CNN v2 | 116 | 122 | the same crop when it **is** a lit−ambient difference image (F42) |
| 13 | color CNN v2 | 117 | 123 | aligned 128x128x3 BGR crop from the RGB camera |
| 14 | multi-task IR "border" | 118 | 124 | whole IR frame resized to 128x128 |
| 15 | multi-task IR "HH" | 119 | 125 | whole IR frame resized to 128x128 |

117 and 123 have 3 input channels. They are the color nets, not IR nets. Only 115/116/121/122
carry the 4096-byte cosine head (section 3.4). This matches the loader: only slots 11 and 12
read the head.

### Thresholds and decision (OBSERVED; static initialisers 0x180014020, 0x180014380, 0x180015450, 0x180015590)

Each threshold map is `map<slot, map<mode, float>>`. The mode is the byte at `engine+44`
(two modes, 0 and 1; their meaning is UNKNOWN). `p` is the softmax output.

| Slot | Rule (RunDual 0x18004a6d0 / RunColor 0x180049ec0 / RunMultiTask 0x18004afa0) | Flag off: mode0 / mode1 | Flag on: mode0 / mode1 |
|---|---|---|---|
| 11 | `p0 > real` → live; else `p1 > spoof` → spoof | real 0.5 / 0.9; spoof 1.0 / 0.5 | real 0.4711 / 0.4921; spoof 1.0 / 0.5289 |
| 12 | same | real 0.5 / 0.9; spoof 1.0 / 0.5 | real 0.5037 / 0.5197; spoof 1.0 / 0.4963 |
| 13 | `p0 > thr` → spoof (veto only) | 0.5 / 0.5 | 0.5 / 0.5 |
| 14 | `p1 > thr` → spoof (veto only) | 0.7 / 0.7 | 0.7 / 0.7 |
| 15 | `p1 > thr` → spoof (veto only) | 0.5 / 0.5 | 0.5 / 0.5 |

The real-threshold map is `qword_1801265B0` (flag off) or `unk_180126630` (flag on). The spoof map
for 11/12 is `qword_1801265F0` or `unk_180126670`. Slots 13–15 always use `qword_1801265F0`.
INFERRED: the flag-on thresholds sit near 0.5 because the flag-on heads have unit-norm rows, so
their logits are raw cosines in [−1, 1] and `p` can only fall in [0.12, 0.88] (section 3.4).
Mode 1 of the flag-off set needs `p0 > 0.9`, which only a scaled (×30) head can reach.

## 2. File format (byte layout)

All integers are little-endian `int32`. Strings are `int32 len` + `len` bytes (no NUL). Floats are
little-endian `float32`. The stream is read sequentially through `sub_18003676C` (an istream read).
There is no alignment and no checksum.

### 2.1 File

| Field | Type | Notes |
|---|---|---|
| magic | string | must equal `"conv_net_v1.0"` (`sub_180014F50` builds the constant; `sub_180033FE0` = `DNNTester::Load`, vtable 0x1800d3250 slot 2, compares it) |
| layer_count | int32 | must be ≥ 2 |
| layers | layer × layer_count | read by the layer factory `sub_1800359E0` |
| trailer | bytes | only for slots 11/12: 1024 float32 = 2×512 head matrix, read raw by `sub_1800C4B14` after `Load` |

### 2.2 Layer record (`sub_1800359E0` + base constructor `sub_180035660`)

| Field | Type | Notes |
|---|---|---|
| type | string | `data, image_data, vary_data, face_data, conv, local_share, pool, neuron, fc, flatten, concat, dropout, padding, across_channel_norm, lrnorm/within_channel_norm, softmax, joint_bayesian, crop_face_data, eltwise_max, bgr2gray`; anything else throws "Unsupported Layer type" |
| subtype | string | `pool`: `max`/`avg`; `neuron`: `relu`/`sigmoid`; otherwise empty |
| name | string | output tensor name |
| n_inputs | int32 | |
| inputs | string × n_inputs | names of earlier layers |
| payload | per type, below | |

Layer payloads seen in shipped files (constructors in parentheses):

| type | payload | semantics (forward / shape routine) |
|---|---|---|
| `image_data` (`sub_18004EB80` → `sub_18004ED48`) | int32 n, c, a(width), b(height); then blob `mean` | `sub_180017AA0`: copies u8 rows (row length = w·c, source stride) into an HWC float tensor. With no mean blob: `x/127.5 − 1.0`. With a mean blob: `x − mean`. All shipped files have an empty mean blob. |
| `conv` (inline in factory, vtable 0x1800d3588) | int32 padA(x), padB(y), strideA(x), strideB(y); blob W (n=Cout, c=Cin, a=kw, b=kh); blob bias (n=Cout) | `sub_18001A070`: `out = (in + 2·pad − k)/stride + 1` (floor). `sub_180018A30` → `sub_1800BEE00` (SSE/AVX variants `sub_18001A3A0`, `sub_1800197E0`): cross-correlation (no kernel flip), starting from the bias, zero padding (the activation buffers are allocated with zeroed borders) |
| `pool` max/avg (`sub_1800BDA30`/`sub_1800BD960`) | int32 strideA, strideB, kA, kB | shape `sub_180015AE0`: `out = ceil((in − kA)/stride) + 1` for **both** axes (uses kA for height too). Forward `sub_180019470`/`sub_180024D50`: windows are clipped at the far border; avg divides by the clipped window area. No pooling padding. |
| `neuron` relu | none | `sub_180019170`: `max(x, 0)` |
| `concat` | none | `sub_180017FE0`: channel concat in input order |
| `flatten` | none | `sub_180024410`: output index `c·H·W + y·W + x` (CHW order, the same as ONNX Flatten on NCHW) |
| `fc` (`sub_1800C0940`) | blob W (n=out, c=in); blob bias | `sub_1800246A0`: `out = bias + Σ_i x[i]·W[i,:]` |
| `padding` (`sub_1800C1B48`) | int32 padA(x), padB(y) | `sub_180024BB0`/`sub_180015980`: copies into a zeroed, larger tensor (zero padding) |
| `dropout` (`sub_1800C1790`) | int32 | identity at inference (not used in shipped nets) |

Other layer types have readers in the DLL, but no Windows Hello conv_net file we have seen uses
them. The converter rejects them.

### 2.3 Blob (`sub_18004EBD4`)

| Field | Type | Notes |
|---|---|---|
| n, c, a, b | int32 ×4 | dimensions. If `n == 0` the blob is "empty": n is treated as 1, and if `a == 0` nothing more is read. |
| padA, padB | int32 ×2 | border padding of the tensor (0 in all files) |
| data | float32 × n·c·a·b | read in `b` slices of `n·c·a` floats; memory order `[b][a][c][n]` (n fastest) = storage type 1 of the index function `sub_180025188` |

For conv weights this is `[kh][kw][Cin][Cout]` (HWIO). The converter transposes it to OIHW.
An empty blob that still has data (`n == 0`, `a != 0`) is read and then thrown away.

### 2.4 Tensor layout at run time (OBSERVED, `sub_1800175D0`, `sub_180025188`, `sub_180019A54`)

The tensor struct is `{float* data; int n, c, a, b, padA, padB; int capacity; int storage}`.
Storage type 0 (activations) uses index `c + C·((a+padA) + (A+2padA)·((b+padB) + n·(B+2padB)))`,
i.e. NHWC with zeroed borders. `a` is x (width), `b` is y (height): the image copy advances one
source row per `b` step.

## 3. Inference path

### 3.1 Evaluation (`DNNTester::Evaluate` = `sub_180017880` → `sub_180018538`)

`Evaluate(net, img, w, h, stride, channels, ..., out, out_len)` resizes the input tensor
(`sub_180017700`) and copies the u8 image (`sub_180017AA0`). It runs every layer in file order
(single thread, or the multi-thread scheduler `sub_180018440` when the net has parallel groups),
then memcpys **the last layer's output** into `out`.

### 3.2 Slot 11/12 crop (`sub_180015F68(img, lm, w, h, stride, 128, 128, ch, out)`)

1. `sub_18001614C`: least-squares similarity `u = a·x + b·y + tx, v = −b·x + a·y + ty`. It maps the
   first 5 landmarks of the face record (`record+44`, crop coordinates) onto the template at
   `0x1800D7F68`:

   | point | x | y |
   |---|---|---|
   | 0 left eye (image left) | 97.8299 | 127.7792 |
   | 1 right eye | 181.0532 | 126.3381 |
   | 2 nose tip | 139.5740 | 179.0006 |
   | 3 left mouth corner | 102.6964 | 216.7014 |
   | 4 right mouth corner | 179.3068 | 215.3733 |

2. `sub_180016B20`: warp to **281×281** by inverse mapping. Bilinear, truncated to u8; any
   source position outside `[0,w−1]×[0,h−1]` gives 0.
3. `sub_180016440`: resample to 128×128 at `src = (i − 64)·230/127 + 140.5`. This is the central
   230×230 of the 281 image. Bilinear, truncated; out of range gives 0.
4. `Evaluate` with ch = 1; the ImageDataLayer applies `x/127.5 − 1`.

The 5 landmarks: `ComposeDataForEngine` (VsmSecure 0x180007310) copies `FPO+256456` to `rec+16`.
The engine's record pointer is 8 bytes into that buffer, so `record+44 = FPO+256492`. That is
landmark 0 of the FaceProcessor landmark array. This is OBSERVED twice: (a) `record+177056/60/64`
then line up with crop width/height/stride (`FPO+32/36/40`, `ComposeLuminanceImagePerFace`), and
(b) the liveness crop `record+333 = FPO+256781` (F40). `ComputeAnchorPoints` (FaceProcessorCore
0x1800372c8) uses landmarks {0,7,8,9,10} as the left eye, {1,13..16} as the right eye and
{3,4,23..26} as nose/mouth. INFERRED: the array follows the Microsoft 27-point layout (pupilLeft,
pupilRight, noseTip, mouthLeft, mouthRight, …), which agrees with the template geometry.
`ComposeLuminanceImagePerFace` moves the landmarks into crop coordinates.

### 3.3 Slot 13 (color) and 14/15 (multi-task) inputs

- Color: `sub_180023AA0` converts the NV12 RGB frame (`record+433504`) to **BGR** u8 (BT.601
  limited range: B from 2.018·U, R from 1.596·V). The same `sub_180015F68` alignment runs with
  ch = 3 and the color landmarks at `record+433496 → +36`.
- Multi-task: `Evaluate(net, record+160333, 128, 128, 128, 1)`. `record+160333 = FPO+416781` is
  the 128×128 thumbnail written by `ComposeAntiSpoofingInformation` (FaceProcessorCore 0x18002db60)
  → `ResizeImage`: the whole IR frame is stretched (no aspect ratio kept),
  `src = (i − 63.5)·(W/128) + (W−1)/2` (`Bilinear<uchar>` 0x1800198fc, truncated).

### 3.4 Head and softmax

- Slots 11/12 (`RunDualIRAntiSpoofingClassifier` 0x18004a6d0): the 512-d net output (pool10 /
  `costh_/Flatten`) is L2-normalised in place (`sub_18004178C`, skipped if the norm is 0). The
  logits are `W·f` with W the 2×512 trailer, row-major, no bias (`sub_18003F8E8`). Then softmax
  (`sub_18003C0B0`: `exp(x − max)/Σ`). OBSERVED from data: the trailer rows have norm **30.0** in
  115/116 (CosFace-style scale s = 30 folded in) and **1.0** in 121/122.
- Slots 13–15: the net ends in `fc` with 2 outputs; the engine applies the same softmax.

## 4. Converter

```
cd tools/winhello_models
.venv/bin/python -m convnet convert-all              # secure DLL files -> models/windows_hello/converted/convnet/
.venv/bin/python -m convnet convert FILE -o OUT.onnx [--no-head]
.venv/bin/python -m convnet info FILE                 # layer table
.venv/bin/python -m convnet check FILE...             # layer-by-layer ONNX vs NumPy reference
.venv/bin/python -m convnet.validate                  # validation on recorded frames
```

ONNX graph: input `image` float32 `[1,C,128,128]` with raw 0–255 values (the ImageDataLayer
normalisation is part of the graph). Outputs:
- `output` = last layer.
- `logits` = `W·l2norm(output)` (slots 11/12 only).
- `prob` = softmax as the engine computes it. For an anti-spoofing slot, `prob` is the `p` used
  by the threshold table.

The ceil-mode pools map to ONNX `MaxPool`/`AveragePool` with `ceil_mode=1` and
`count_include_pad=0`. The `padding` layer maps to `Pad`.

Python API (`sys.path` += `tools/winhello_models`): `convnet.load/parse`, `convnet.to_onnx`,
`convnet.reference` (NumPy DNNTestLib: `forward`, `classify`), `convnet.msprep` (crop
re-implementation: `align_crop`, `thumbnail`, `difference_image`). No extra packages were
installed into the shared venv.

## 5. Validation

Run with `python -m convnet.validate` (results in `validation/results.json`, crops as PNG).
- Landmarks: SCRFD `det_10g` 5-point keypoints stand in for Microsoft's landmark tracker.
- Owner: `ir_{11,13,15,17,19}_lit` (paired with the preceding `*_dark` frame for the difference
  image) and `watch_*_ir_lit` / `watch_*_rgb`.
- Stranger: the 6 faces of `strangers_insightface_t1.jpg`, converted to gray for the IR nets, and
  BGR for the color nets.
- ONNX vs NumPy reference on all 112 runs: max `|Δp|` = 2.9e-6.

| Input | Model (slot, flag) | Owner p0 min / mean / max | Stranger-photo p0 min / mean / max |
|---|---|---|---|
| aligned IR crop | 115 (11, off) | 0.743 / 0.935 / 1.000 (9/9 live in mode 0, 7/9 in mode 1) | 0.000 / 0.815 / 1.000 (5/6 "live") |
| aligned IR crop | 121 (11, on) | 0.575 / 0.611 / 0.636 (9/9 live, both modes) | 0.409 / 0.528 / 0.634 (mode 1: 3 live, 1 spoof, 2 none) |
| difference image | 116 (12, off) | 0.178 / 0.248 / 0.449 (mode 1: 5/5 spoof) | 0.000 / 0.000 / 0.000 (mode 1: 6/6 spoof) |
| difference image | 122 (12, on) | 0.517 / 0.521 / 0.525 (5/5 live mode 0, 4/5 mode 1) | 0.428 / 0.465 / 0.526 (mode 1: 5/6 spoof) |
| whole frame | 118 (14, off) | p1 0.24–0.26 (no veto) | p1 0.725 → **spoof veto** (thr 0.7) |
| whole frame | 119 (15, off) | p1 0.000 | p1 0.158 |
| whole frame | 124 (14, on) | p1 0.268–0.269 | p1 0.571 (below 0.7) |
| whole frame | 125 (15, on) | p1 0.000 | p1 0.030 |
| color BGR crop | 117 / 123 (13) | p0 0.000 (no veto) | p0 ≤ 0.001 (no veto) |

Reading (INFERRED):
- The general IR nets (slot 11) call the owner's real IR faces live in every frame.
- The difference-image nets (slot 12) separate owner from stranger photo clearly: 116 gives
  0.18–0.45 vs 0.000, and 122 gives 0.52 vs 0.47. 122 votes live for the owner. 116 does not
  reach its live threshold for the owner. Our "difference images" use nearly black ambient frames
  (mean 0.1), so they are close to the plain lit image. Microsoft's real difference images may
  have different statistics.
- The multi-task "border" net 118 vetoes the whole stranger photo and not the owner frames.
- A gray visible-light photo is not a realistic IR attack. The general IR nets do not reject it.
  In the engine, the difference-image and multi-task nets add the spoof votes (F25 vote
  aggregation).
- The color nets never veto the real RGB faces (owner or the real people in the photo), which is
  as expected for a print/screen detector. No color attack data was available.

Ablation (`MODEL_FILES_115/121` on the owner's IR crops): the correct layout gives p0 0.74–1.00 /
0.58–0.64. Kernels with swapped H/W, or a transposed image, give 0.000 / 0.36–0.39.
Color channel order (`MODEL_FILES_117/123` on the 10 color crops): BGR input (as derived from
`sub_180023AA0`) gives spoof p0 ≤ 0.001 on every real face. RGB input gives 0.98–1.00 on all 4
owner frames. So the BGR order is also supported by the model's behaviour.

## 6. Verified vs assumed

- **Verified (code + data):** byte layout; layer semantics; weight/tensor order; padding and
  pooling rules; normalisation `x/127.5−1`; head (L2-normalise + 2×512 matrix, no bias) and
  softmax; slot map and threshold tables; the crop pipeline constants (template, 281, 230/127,
  140.5; thumbnail formula); BGR order for the color input (code + channel ablation).
- **Verified numerically:** ONNX = NumPy re-implementation of the decompiled code (≤ 2.2e-6 per
  layer). It is not compared against Microsoft's own binary output (no Windows run was possible).
- **Assumed:** SCRFD 5-point keypoints stand in for MS landmarks 0–4 (same semantics per the
  template, different detector). The warp is computed on the full frame instead of the
  FaceProcessor crop; this only differs where the crop border would cut the warped region (MS
  fills it with 0). `sub_180017200` is assumed to be a plain 3×3 inverse. The lit/dark pairing
  uses the preceding dark frame without motion compensation. The multi-task thumbnail is taken
  from the lit frame.

## 7. Open questions

- What the mode byte `engine+44` means, and the name of feature flag `unk_1801246E8` (which picks
  115–119 vs 121–125).
- Why 116 scores the owner's near-ambient-free difference images below 0.5. Real Windows
  difference images (driver mode 4 / lighting status 1) are needed to check this.
- Microsoft's landmark detector (27 points) has not been reproduced. Small landmark offsets change
  the crop.
- The cefa 123 and 125 recognition nets give the same 256-d output (to ~1e-7) despite different
  bytes. 125 looks like a re-export of 123 (reported to conv-cefa).
