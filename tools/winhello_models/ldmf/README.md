# ldmf — Windows Hello "FaceSDK Team" LDMF face detector (research converter)

Research / interoperability only. Inputs and outputs are Microsoft proprietary, git-ignored, and not used by
linux_hello. Full write-up: `docs/winhello-models/ldmf.md`.

LDMF (`FaceDetectorResources/MODEL_FILES_101.ldmf`, `_102.ldmf`) is **not a CNN**: it is Microsoft's JDA
(joint cascade face detection + 31/27-point alignment, boosted pixel-difference trees) plus an oriented-HoG
(SIFT-like) post filter. It is therefore converted to a self-contained `.npz` and run by a faithful numpy
reimplementation (`jda.py`), not ONNX. `MODEL_FILES_110..113.bin` are XOR-obfuscated ONNX-Runtime (`.ort`)
RetinaFace/img2pose-style detectors; they are decrypted (`../cipher.py`) and run with ONNX Runtime (`retina.py`).

Run everything from `tools/winhello_models` with its venv (`.venv/bin/python`). Extra package used: `unicorn`
(already in the venv; only for the emulator cross-check).

| module | what |
|---|---|
| `format.py` | LDMF parser (byte-exact, every byte accounted for) |
| `hardcoded.py` | tables read from `FaceTrackerInternal.dll` .rdata (fern weights, atan LUT, reference shapes, post-filter template) |
| `extract/` | one-off recovery of the code-only parts (stage-0 trees, 400 ferns) with IDA 9.5 idalib + a symbolic C++ harness |
| `convert.py` | LDMF + DLL tables + extracted structure → `converted/ldmf/MODEL_FILES_10x_<type>.npz` (+ `.json` summary) |
| `jda.py` | detector: pyramid, fern pre-filter, JDA cascade, post filter, landmarks→box, merge |
| `emu.py` | runs the DLL's own functions under Unicorn for window-level bit-exact comparison |
| `retina.py` | 110–113 ORT detectors: pre/post-processing from `FaceProcessorCore.dll` RetinaEngine |
| `validate.py` | emulator cross-check + detection on validation frames vs SCRFD (uses `lbf.scrfd`) |

```sh
.venv/bin/python -m ldmf info ../../models/windows_hello/FaceDetectorResources/MODEL_FILES_101.ldmf
# (once) recover code-only structure; needs idalib (system python3) + g++; copy the .i64 first
.venv/bin/python -m ldmf.extract.build_structure --idb /tmp/copy.i64 --dll FaceTrackerInternal.dll \
    --work /tmp/ldmf_extract -o ../../models/windows_hello/converted/ldmf/hardcoded_structure.npz
.venv/bin/python -m ldmf convert --dll FaceTrackerInternal.dll
.venv/bin/python -m ldmf detect --draw /tmp/out some_ir_frame.png        # default model: 101 infrared
.venv/bin/python -m ldmf validate --dll FaceTrackerInternal.dll          # writes converted/ldmf/validation.json
.venv/bin/python -m cipher ../../models/windows_hello/FaceDetectorResources/MODEL_FILES_11?.bin \
    -o ../../models/windows_hello/converted/ldmf/ort
```

Python API: `jda.detect(jda.Model(npz), gray_uint8)` → list of `Detection(box, score, jda_score, landmarks)`;
`retina.RetinaDetector(ort_path).detect(gray_uint8)` → list of `Face(box, score, landmarks, dofs, rotation_xyz)`.
