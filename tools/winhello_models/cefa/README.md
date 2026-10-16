# cefa — Windows Hello Face recognition containers (research only)

Unpacks `FaceRecognitionEngineAdapterResources*/MODEL_FILES_12x.cefa` into ONNX Runtime-loadable
models. Outputs are Microsoft proprietary: they stay in the git-ignored
`models/windows_hello/converted/`. linux_hello runs the v5 recogniser (`MODEL_FILES_123.onnx` + `.json`)
as a second opinion in matching when it is there (`src/ms_models/recognizer.rs`). Format and derivation:
`docs/winhello-models/cefa.md`.

Run from `tools/winhello_models/` with the shared venv:

```sh
.venv/bin/python -m cefa info  ../../models/windows_hello/FaceRecognitionEngineAdapterResources_v5/MODEL_FILES_123.cefa
.venv/bin/python -m cefa unpack ../../models/windows_hello/FaceRecognitionEngineAdapterResources*/MODEL_FILES_12*.cefa \
    -o ../../models/windows_hello/converted/cefa
.venv/bin/python -m cefa.validate          # owner vs stranger distances (needs det_10g.onnx)
.venv/bin/python -m cipher ../../models/windows_hello/facerecognitionengineadapterresourcescore/MODEL_FILES_120.bin \
    -o ../../models/windows_hello/converted/occlusion   # HelloFaceCipher (shared module ../cipher.py)
```

`unpack` writes, per container: `<stem>.json` (header, tech id, calibration tables),
`<stem>.blob<i>.ort` + `<stem>.blob<i>.onnx` (ORT payloads and a re-serialised standard ONNX copy),
`<stem>.convnet` + `<stem>.onnx` (conv_net_v1 payload; the ONNX export uses `../convnet`).

Modules: `container.py` (parser), `ort2onnx.py` (ORT flatbuffer → ONNX, unfuses `FusedGemm`),
`align.py` (Microsoft's 128×128 recognition crop), `occlusion.py` (occlusion-model pre/post-processing),
`validate.py`.
