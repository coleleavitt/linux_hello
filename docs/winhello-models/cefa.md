# Windows Hello Face: `.cefa` recognition containers and HelloFaceCipher

Research and interoperability notes. The converted models are Microsoft proprietary. They stay
local and git-ignored (`models/windows_hello/converted/`); linux_hello uses the v5 recogniser when it is
there (§7) and never ships it.
Tool: `tools/winhello_models/cefa/` (README there). Shared cipher module: `tools/winhello_models/cipher.py`.
Agent: conv-cefa. Binary: Windows 11 build 26300 `FaceRecognitionEngineAdapter.dll` (ENG, no PDB; names from
RTTI and TraceLogging strings), `FaceProcessorCore.dll` / `FaceProcessor.dll` (public PDBs).

Tags: **OBSERVED** = read from code or data (function + address given). **INFERRED** = conclusion from
observed facts. **UNKNOWN** = not resolved.

## Summary

- The `.cefa` files are **not encrypted** and have **no checksum**. `ce fa 0a` is a format header
  (u16 magic `0xFACE`, version byte 10), shared with the LBF aligner database
  (`facerecognitionsensoradapterresources/MODEL_FILES_102.cefa`, owned by conv-lbf).
- v5 (`MODEL_FILES_123/125`, tech 11) hold the recognition CNN as LDMF/NNDR-wrapped `conv_net_v1`.
  It was converted to ONNX with conv-convnet's converter. Input 1×1×128×128 raw 0..255, output 256-d.
  **123 and 125 compute the same function** (max |Δ| 1.1e-6 on real crops).
- v1–v4 (`120, 121, 122, 124, 126`, tech 7–10) **do not contain the old CNNs**. They contain two small
  ORT-format MLPs each: "feature converters" that map embeddings into a common 256-d space, so
  templates enrolled with older engine versions can still be matched. They load in ONNX Runtime as they are.
  Re-serialised `.onnx` copies are bit-exact.
- **HelloFaceCipher** is a fixed XOR (17-byte key + byte index). It is applied to whole ONNX/ORT
  resources, not to `.cefa`. With it, `facerecognitionengineadapterresourcescore/MODEL_FILES_120.bin`
  decrypts to the face **occlusion** classifier (ONNX). `FaceDetectorResources/MODEL_FILES_110–113.bin`
  decrypt to ORT face detectors (conv-ldmf owns them).
- Validation (v5 CNN, Microsoft crop reimplemented from the DLL, cosine distance as in the engine):
  owner vs owner, same session: **0.007–0.033**. Owner vs owner, other session: **0.40–0.48**, because the
  `ir_*` frames cut the face off at the chin. Owner IR vs strangers: **≥ 0.806** (mean 0.93). The engine's
  FAR 1e-5 threshold is **0.350**. No stranger pair is accepted.
- linux_hello runs the v5 net as a second opinion next to ArcFace (`src/ms_models/recognizer.rs`, §7): the
  Rust port is exact to 4.5e-8 on identical crops.

## 1. Who loads the containers

| Step | Function | Tag |
|---|---|---|
| Resource registry: tech → DLL, key → MODEL_FILES id | ENG `sub_1800989E4` | OBSERVED |
| Model object factory by tech | ENG `sub_180051A08` (called from `FaceRecognition::Initialize` 0x180051772) | OBSERVED |
| Resource stream → `IFaceVerification` slot 1 `Load(istream)` | `FaceRecognition::LoadModels` 0x18009d320 (call at 0x18009e07a, object at engine+0x11B98) | OBSERVED |
| Header check (10 bytes) then slot 28 `LoadBody` | ENG `sub_1800AF8C0` → `sub_1800C69B4` | OBSERVED |
| Common body: tech id, flag, calibration tables | ENG `sub_18005B170`, per table `sub_180059DC0` | OBSERVED |
| CNN body (tech 7–11): u32 len + blob → `FaceRecognitionCnnImpl[2]` slot 0 | ENG `sub_1800B29D0` | OBSERVED |
| LDMF/NNDR unwrap → conv_net parser | ENG `sub_1800B65E0` → `sub_1800B3F58` | OBSERVED |
| FeatureConverter body: two ORT sessions (second optional) | ENG `sub_1800B2060` (vtable 0x1800d4cc0) → `sub_1800B44A0` → `OnnxTester::LoadAndInitialize` 0x1800b0fcc | OBSERVED |

Resource registry (`sub_1800989E4`, OBSERVED). "Flag" is the feature check `sub_18007AB90(&unk_1801246E8)`.
The same flag also selects occlusion model 120 instead of 106 (§5).

| Tech | DLL | key 0 (CNN) | key 2 | key 3 (feature converter) | Shipped in 26300 |
|---|---|---|---|---|---|
| 11 | `…Resources_v5.dll` | 125 if flag else 123 | – | – | 123, 125 |
| 10 | `…Resources_v4.dll` | 118 | – | 126 if flag else 124 | 124, 126 |
| 9 | `…Resources_v3.dll` | 116 | – | 122 | 122 |
| 8 | `…Resources_v2.dll` | 113 | – | 121 | 121 |
| 7 | `…Resources.dll` | 106 | 112 | 120 | 120 |

Factory (`sub_180051A08`, OBSERVED): if the tech has a key-3 entry (`sub_1800980F8`), the object is the
FeatureConverter (vtable 0x1800d4cc0). Otherwise tech 3/5 → LBP + JointBayesian model (vtables 0x1800d5088 /
0x1800d4bb0). Tech 7–10 → `FaceVerificationCNN<FaceRecognitionCnnImpl>`. Tech 11 →
`FaceVerificationCNN<FaceRecognitionCnnImpl2>` (RTTI names). Feature length per tech (`sub_1800AF6E0`
template bytes / 4): tech 7–9 = 128, tech 10–11 = 256 (tech 3 = 3604 bytes).

INFERRED: the CNN resources of tech 7–10 (106/112/113/116/118) were removed from the DLLs. Only the
converters ship, so old templates keep working.

## 2. Byte layout (all little-endian)

### 2.1 Header (10 bytes, `sub_1800C69B4`, `sub_1800C3BD0`)

| Off | Size | Field | Value | Tag |
|---|---|---|---|---|
| 0 | 2 | magic | `0xFACE` (else HRESULT 0x83010003) | OBSERVED |
| 2 | 1 | version | 10 (else 0x83010003) | OBSERVED |
| 3 | 3 | reserved | `00 00 00` in all files | OBSERVED value, meaning UNKNOWN |
| 6 | 4 | body size | file size − 10 (checked by `sub_1800C3BD0`; `sub_1800AF8C0` reads it but does not check it) | OBSERVED |

### 2.2 Common body (`sub_18005B170`)

| Field | Size | Notes | Tag |
|---|---|---|---|
| tech | u32 | must equal the object's tech (this+108). 7, 8, 9, 10 (124 and 126), 11 | OBSERVED |
| flag | u8 | stored at this+113. 0 in all files | OBSERVED, meaning UNKNOWN |
| n_tables | u32 | must be > 0. 5 in all files | OBSERVED |
| tables | n × table | see below | OBSERVED |

Calibration table (`sub_180059DC0`):

| Field | Size | Notes |
|---|---|---|
| hardware_model | u32 | v5: 0, 1, 2, 4, 5; v1–v4: 0, 1, 2, 5, 5 |
| n_ids | u32 | 0 in all files |
| ids | n_ids × u32 | |
| n_rows | u32 | 20 |
| rows | n_rows × (f32 FPR, f32 TAR, f32 distance) | used by the distance → GFP curve (F51) |

All five tables in a file are identical. At FPR 1e-5 (row 4) the distance is **0.350** for v5
(TAR 0.99766). For v1–v4 it is **0.410** (TAR 0.969 / 0.975 / 0.965 / 0.993 / 0.993 for 120/121/122/124/126).

### 2.3 Payload

| Container | Payload (OBSERVED) |
|---|---|
| tech 11 (123, 125) | u32 len + LDMF blob (CNN body `sub_1800B29D0`). After loading, `Impl2` slot 3 must equal this+60 (embedding size) |
| tech 7–10 (120–126) | u32 len + ORT model A, then u32 len + ORT model B (`sub_1800B44A0` reads each; B is optional — a load failure is logged as `InitializeFeatureConverter…` and ignored) |

LDMF wrapper (`sub_1800B65E0`, OBSERVED):

| Off | Size | Field | Value |
|---|---|---|---|
| 0 | 4 | magic | `LDMF` |
| 4 | 4 | header size | 16 or 360 (16 here) |
| 8 | 4 | type | must be 302 (101 ≤ type ≤ 502 range check, then == 302) |
| 12 | 4 | total size | = blob length |
| hs | 4 | chunk tag | `NNDR` |
| hs+4 | 4 | chunk size | = total − hs |
| hs+8 | … | conv_net_v1 model | `0d 00 00 00 "conv_net_v1.0"` … (format: docs/winhello-models/convnet.md) |

ORT payloads (OBSERVED by loading them):

| File | Model A | Model B | Producer |
|---|---|---|---|
| 120, 121, 122 | 256 → 256 (4 Gemm, 3 ReLU) | 128 → 256 | tf2onnx, opset 10 |
| 124 | 256 → 256 | 256 → 256 | tf2onnx, opset 10 |
| 126 | 256 → 256 (Gemm + BN + ReLU ×3, Gemm) | 256 → 256 | pytorch, opset 11 |

## 3. Feature converter semantics

- OBSERVED: telemetry names `InitializeFeatureConverter`, `FeatureExtractionFeatureConverter`,
  `FeatureUnificationFeatureConverter`, `FeatureDistanceFeatureConverter`,
  `InitializeFeatureConverter.InitializeForwardConversionModel.Failed`. Run path (`sub_1800B4300`):
  the input vector is **L2-normalised** (`sub_18004178C`), then run as shape [1, n]. The output size must match.
  Distance (slot 21, `sub_1800B1F10`) = `sub_180054190` = 1 − cos (F50).
- OBSERVED: model B input size = the legacy template size of that tech (128 for tech 7–9, 256 for tech 10).
  Model A input = 256 = the current tech-11 embedding size. Both outputs are 256.
- INFERRED: A ("forward conversion") maps a fresh v5 embedding, and B maps a stored legacy template, into one
  shared 256-d space where they are compared with cosine distance. The 0.410 threshold in the v1–v4 tables
  applies in that space. FeatureExtraction (slot 6) uses the CNN object stored at this+160 (slot 27).
  The exact call chain is through CFG indirect calls and was not traced end to end.
- Pairing (registry): 125 + 126 when the flag is on, 123 + 124 when it is off.

## 4. Recognition crop and input (`FaceRecognitionCnnImpl2` slot 6, `sub_180015BE0`)

Reimplemented in `cefa/align.py`. All OBSERVED:

1. Needs 27 landmarks (`a3 == 27`) and a 1-channel (or 3-channel) 8-bit image.
2. `sub_18001614C`: least-squares similarity (rotation + uniform scale + translation) from the **first 5
   landmarks** (left eye, right eye, nose tip, left and right mouth corner) to the reference points at
   0x1800D7F40 in a 281×281 canvas: (97.830, 127.779), (181.053, 126.338), (139.574, 179.001),
   (102.696, 216.701), (179.307, 215.373).
3. `sub_180017200` inverts the 3×3 matrix. `sub_180016B20` warps into 281×281: bilinear, result truncated to
   uint8, pixels whose source is outside [0, w−1]×[0, h−1] become 0.
4. `sub_180016440`: 128×128 sample (r, c) from canvas coordinate `(k − 64)·230/127 + 140.5`, bilinear,
   outside [0, 280] → 0. So the network sees canvas x, y ∈ [24.6, 254.6].
5. DNNTestLib FaceDataLayer (`sub_180017AA0`): `x/127.5 − 1` if the model has no mean image, else `x − mean`
   (123 has an empty mean blob). conv-convnet's ONNX export contains this step, so it takes raw 0..255.
6. Network forward `sub_180017880` → output = last layer (256 floats).

The older `FaceRecognitionCnnImpl` (`sub_1800B6210`, tech 7–10) uses a 200 or 281 canvas, a box-centre point,
and different reference tables (0x1800DF430 / 0x1800DF450). It is not needed because its CNNs are not shipped.

## 5. HelloFaceCipher and the occlusion model

### 5.1 Cipher (OBSERVED)

`Windows::Analog::Face::HelloFaceCipher::EncryptDecrypt(uint n, uchar* p)` — FaceProcessorCore 0x18001b104,
FaceProcessor 0x180061200, ENG inlined copy `sub_180096E44`:

```
K = struct.pack('<4i', -1084033398, 172859493, 950223013, 1876138409) + b'\0'
  = 8a f6 62 bf 65 a0 4d 0a a5 40 a3 38 a9 99 d3 6f 00          (17 bytes)
out[i] = in[i] ^ K[i % 17] ^ (i & 0xFF)                          (self-inverse)
```

There is no key derivation and no integrity check. Callers:

| Caller | Data | Condition |
|---|---|---|
| FaceProcessorCore `OnnxTester::LoadAndInitialize` 0x180040c7c, FaceProcessor 0x1800c047c | whole MODEL_FILES resource from FaceDetectorResources, before ORT load | always |
| ENG `FaceOcclusionDetector::LoadOnnxOcclusionModel` 0x1800ab92c | occlusion ONNX buffer | flag `unk_1801246E8` on (the same flag that picks core 120 instead of 106) |
| ENG `LogExtendedAntispoofingDataForCnnAntiSpoofing` 0x180086df4, `sub_1800ADACC` | telemetry float vectors (encrypt, log, decrypt again) | – |

Decryption results (OBSERVED by loading in onnxruntime 1.31):
`core/MODEL_FILES_120.bin` → ONNX (pytorch 1.13.1), 1×1×128×128 → 1×4.
`FaceDetectorResources/MODEL_FILES_110–113.bin` → ORT format: `input_0` 1×1×H×W → bbox, class, landmarks (10),
confidence, plus `dofs` (6) in 110/112. Those belong to conv-ldmf.

### 5.2 Occlusion classifier (core 120 decrypted; 106 is the plain predecessor)

`FaceOcclusionDetector::TestOcclusion` 0x1800abe28, reimplemented in `cefa/occlusion.py`:

- Input (`sub_18004B5B8` / `sub_18004B6FC`, OBSERVED): the face rectangle stored at record+16 (x0, y0, x1, y1) inside
  the 8-bit lit luminance crop, resampled to 128×128 with sample `(k − 63.5)·size/128 + (size − 1)/2` (bilinear
  `sub_180023580`), then `x/127.5 − 1`.
- Output (OBSERVED): softmax of 4 logits (`sub_18003C0B0`). Telemetry event
  `FaceOcclusionDetector::TestOcclusion.FaceOcclusionResult` names them **faceWithoutOcclusionScore,
  faceWithMaskOcclusionScore, faceWithSunglassesOcclusionScore, faceWithHandOcclusionScore**.
- Decision (`TrackedFace::IsFaceOccluded` 0x1800a8408, OBSERVED): `score = 1 − p[none]`. Occluded if score > 0.9,
  or > 0.5 while the track is enrolling (state 3).
- INFERRED: the rectangle is the IR face detector box. Validation below uses SCRFD boxes instead.

## 6. Validation

Setup: SCRFD `det_10g` (open model) gives each face's box and 5 landmarks. The Windows 27-point aligner is not
used; its first 5 points have the same meaning as SCRFD's 5. Crops follow §4 exactly. Embeddings come from
`converted/cefa/MODEL_FILES_123.onnx` / `125.onnx`. Distance = engine distance `min(1, 1 − cos)` (F50).
Script: `python -m cefa.validate`; ad-hoc matrix script in the scratch dir. Results in
`converted/cefa/validation/results.json` and crops in `converted/cefa/validation/crop_*.png`.

Frames: `ir_11..19_lit` (5, session A: face at the bottom edge, chin cut off), `watch_*_ir_lit` (4, session B,
full face), `watch_*_rgb` (4, owner RGB), `strangers_insightface_t1.jpg` (6 faces, visible-light photo converted to gray).

| Pair group (v5, 123) | n | min | mean | max |
|---|---:|---:|---:|---:|
| owner IR A vs A | 10 | 0.007 | 0.019 | 0.030 |
| owner IR B vs B | 6 | 0.013 | 0.024 | 0.033 |
| owner IR A vs B | 20 | 0.402 | 0.443 | 0.481 |
| owner RGB vs RGB | 6 | 0.038 | 0.123 | 0.213 |
| owner IR B vs owner RGB | 16 | 0.473 | 0.508 | 0.540 |
| owner IR (A+B) vs strangers | 54 | **0.806** | 0.93 | 1.000 |
| strangers vs strangers | 15 | 0.629 | 0.787 | 0.966 |

- Owner-vs-stranger distances are always far above the owner-vs-owner ones: worst owner pair 0.481, best
  stranger pair 0.806. At the engine's FAR 1e-5 distance 0.350, 0 % of owner–stranger pairs pass. Same-session
  owner pairs pass. Cross-session pairs (A vs B) do not, because the A crops are missing the chin and mouth
  (black bottom rows). The engine itself would drop those frames (edge-cut flag, F46).
- Lit − dark subtraction (`lit − min(lit, dark)`, F40) changes nothing here. The dark frames have mean 0.05.
- 123 vs 125 on the same crops: max |Δembedding| = 1.1e-6. Model A of the paired converter (124 for 123, 126 for 125) on
  L2-normalised embeddings keeps the separation: owner–owner mean 0.233/0.234, owner–stranger min 0.811/0.848.
- Converter ORT vs re-serialised ONNX: max |Δ| = 0.0 for all 10 models (random inputs).
- Caveat: the strangers are visible-light faces, not IR. Part of their distance can come from the domain
  difference. Owner RGB vs owner IR is 0.47–0.67, so the net is clearly IR-tuned. A real IR stranger set would be a stronger test.

Occlusion model (`converted/occlusion/validation.json`, p = [none, mask, sunglasses, hand], core 120 decrypted):

| Input | core 120 | core 106 |
|---|---|---|
| owner IR B, full face (4 frames) | none 0.961–0.998 | none 0.998–0.9999 |
| same + dark bar over the eyes | sunglasses 0.63–0.78 | none 0.72–0.93 |
| same + bright block over nose–chin | mask 1.000 | mask 1.000 |
| owner IR A (chin cut off by the frame edge) | mask 0.98–0.99 | mask 0.98–0.99 |
| 6 strangers | none 0.94–1.00 | none 0.83–1.00 |

The outputs match the telemetry class names. Session A would be rejected as occluded (score > 0.9). That agrees
with the cut-off face.

What is verified, and what is assumed:
- Verified: the container layout (all 7 files parse to the exact end and every size check passes); ORT models load;
  ONNX copies are exact; crop math is taken from the DLL; distance metric and thresholds come from the DLL/data;
  cipher output loads as valid models.
- Not verified bit-for-bit: the crop against a Windows run (no Windows trace available). The landmark source is SCRFD,
  not Microsoft's aligner. The face box for occlusion is SCRFD's, not Microsoft's detector box.

## 7. Rust port (`src/ms_models/recognizer.rs`)

`MsRecognizer` runs the v5 net as the second opinion of linux_hello's matching (capture-and-liveness.md §6,
calibration.md "Microsoft's table"). Loaded by `WindowsHelloModels` from `converted/cefa/MODEL_FILES_123.onnx`
plus the calibration table of the container header from `MODEL_FILES_123.json` (`calibration::load_windows`,
first hardware model; `contrib/install.sh` copies both). `MsRecognizer::crop` is §4 steps 1–4 — the same
`prep::align_crop` as the anti-spoofing nets, whose `sub_180015F68` calls the same `sub_18001614C` /
`sub_180016B20` / `sub_180016440` on the same template values (0x1800D7F40 here, 0x1800D7F68 in convnet.md
§3.2) — at the Windows IR detector's five points (SCRFD's when it has no face); `embed` feeds it raw 0..255
(the graph holds the FaceDataLayer's `x/127.5 − 1`) and L2-normalises the 256 floats. `recognizer::distance`
is F50. Input image: the recognition image of F40 (a), `lit − min(lit, ambient)` with good lighting, the
same one the ArcFace print comes from.

Checked against §6's Python on the validation frames (`examples/ms_recognize <frames> <models> <crops_dir>`,
then onnxruntime on the written crops with `align.py` on the same landmarks):

- Net: on identical crops, max |Δfeature| ≤ 4.5e-8 (ort in Rust vs onnxruntime in Python).
- Crop: 19–67 of the 16,384 pixels of an IR frame's crop differ by 1 grey level (one pixel by 21 at a
  sampling boundary), from the order of the float additions before the truncation to uint8; features then
  differ by ≤ 1.0e-3, distances by < 1e-3. Which order the DLL uses is not known; neither port is bit-exact to it.
- Distances (Rust, Windows detector points on the `watch_*` frames, SCRFD's on `ir_*` where that detector
  finds no face, F46): owner A vs A 0.007–0.030, B vs B 0.010–0.035, A vs B 0.417–0.467, owner vs strangers
  ≥ 0.847 (mean 0.949), strangers vs strangers ≥ 0.604 — §6's numbers within 0.01 (§6 used SCRFD points for
  all; the two point sets put a `watch_*` feature 0.006–0.012 apart).
- End to end (`Template::best_match`, both tables): every owner frame against the other frames of its
  session is recognised by ArcFace (similarity 0.965–0.986, FAR 2–3e-8) and by Microsoft's net (distance
  0.007–0.025, its FAR 7.7–8.1e-7, the extrapolation floor of its table); the chin-cut `ir_*` frames against
  the full-face `watch_*` template pass ArcFace (0.61–0.64, FAR 6e-8) but not Microsoft's net (0.42–0.45,
  FAR 1.1e-4–3.7e-4), so the second opinion refuses them; the six strangers fail both (ArcFace −0.05…0.03,
  Microsoft 0.846–1.0, FAR ≥ 0.067).
- Time: crop + net 4.1–5.6 ms per face (release build, CPU, this laptop), on top of the ArcFace print.

## 8. Open questions

- Header bytes 3–5 and the body flag byte (this+113): always 0. Meaning UNKNOWN.
- Hardware-model ids in the tables (0, 1, 2, 4/5, 5): which camera classes they mean is UNKNOWN (F51 has the GFP use).
- Feature converter: the exact roles of models A and B are INFERRED (§3). Model B cannot be tested without legacy templates.
- Feature flag `unk_1801246E8` (selects 125/126/core-120): its name and default are UNKNOWN.
- Why 123 and 125 differ byte-wise but compute the same function: per conv-convnet, 125 is an ONNX re-export
  of the same trained net (INFERRED).

## Files

| Path | Content |
|---|---|
| `tools/winhello_models/cefa/` | parser, CLI, ORT→ONNX, crop and occlusion reimplementations, validation |
| `tools/winhello_models/cipher.py` | HelloFaceCipher (`python -m cipher IN… -o DIR`) |
| `models/windows_hello/converted/cefa/` | `MODEL_FILES_12x.json`, `.blob{0,1}.ort/.onnx`, `123/125.convnet/.onnx`, `validation/` |
| `models/windows_hello/converted/occlusion/` | `MODEL_FILES_120.onnx` (decrypted), `validation.json` |
| `~/SiteResearch/windows_hello/re/conv/conv-cefa/` | IDA copy, query scripts, decompiled snippets |

No packages were installed. Only the shared venv was used. `ort2onnx` uses the `flatbuffers` package and the ORT schema bundled with onnxruntime.
