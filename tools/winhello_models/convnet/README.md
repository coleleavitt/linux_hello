# convnet — Microsoft DNNTestLib `conv_net_v1.0` → ONNX

Research converter for the Windows Hello Face anti-spoofing nets (secure resource DLL
`MODEL_FILES_115..125`) and the conv_net payloads inside the v5 `cefa` containers.
Format spec, derivation and validation: `docs/winhello-models/convnet.md`.

```
cd tools/winhello_models
.venv/bin/python -m convnet convert-all          # -> models/windows_hello/converted/convnet/*.onnx (+ .json)
.venv/bin/python -m convnet convert IN.convnet -o OUT.onnx [--no-head]
.venv/bin/python -m convnet info IN.convnet
.venv/bin/python -m convnet check IN.convnet ... # layer-by-layer ONNX vs NumPy reference
.venv/bin/python -m convnet.validate             # run on /home/cole/SiteResearch/windows_hello/re/validation_frames
```

ONNX input `image`: float32 `[1,C,128,128]`, raw 0..255 pixel values (normalisation is in the graph).
Outputs: `output` (last layer), `logits` (cosine head, slots 11/12 only), `prob` (engine softmax).

Modules: `format` (parser), `onnx_export` (`to_onnx`), `reference` (NumPy DNNTestLib
re-implementation, `classify` = full engine path), `msprep` (Microsoft crop / thumbnail /
difference-image re-implementation), `validate`.

Outputs are Microsoft-proprietary derived data: keep them under the git-ignored
`models/windows_hello/`.
