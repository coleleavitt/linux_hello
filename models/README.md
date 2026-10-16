# Models

Not committed (two exceed GitHub's 100 MB limit). Run `../scripts/download_models.sh`.

| File | Used for | Source | SHA-256 |
|------|----------|--------|---------|
| `det_10g.onnx` | SCRFD-10G face detection | InsightFace `buffalo_l` v0.7 | `5838f7fe…5b91` |
| `w600k_r50.onnx` | ArcFace R50 (WebFace600K) embeddings | InsightFace `buffalo_l` v0.7 | `4c06341c…9e43` |
| `antispoof/facenox_antispoof.onnx` | Liveness | [facenox/face-antispoof-onnx](https://github.com/facenox/face-antispoof-onnx) `models/best/98.20/best_model.onnx` @ `2f8dd93` | `af2381b8…42b6` |
| `antispoof/MiniFASNetV2.onnx` | Liveness | [garciafido/minifasnet-v2-anti-spoofing-onnx](https://huggingface.co/garciafido/minifasnet-v2-anti-spoofing-onnx) | `d7b3cd9b…c7b` |

`calibration.json` (committed) holds the similarity → false-accept-rate tables per IR camera for `w600k_r50`
(`docs/calibration.md`, "Calibration table").

## Preprocessing notes

- **SCRFD:** 640×640 letterbox, RGB, `(x − 127.5) / 128`. Nine outputs in fixed order:
  scores ×3, box deltas ×3, keypoint deltas ×3 for strides 8/16/32, two anchors per cell.
- **ArcFace:** 112×112 face warped onto the standard 5-point template, `(x − 127.5) / 127.5`.
- **FaceNox:** square crop 1.5× the box (reflect padding), 128×128 RGB, `x / 255`;
  logits `[real, spoof]`.
- **MiniFASNetV2:** crop 2.7× the box shifted inside the image, 80×80 **BGR, raw 0–255**;
  softmax over 3 classes, **index 1 is real**. The Hugging Face model card says `/255` and
  index 0, but upstream Silent-Face removed the `/255` (`functional.py`) and treats
  `label == 1` as real (`test.py`).

## Licenses

InsightFace pretrained models: non-commercial research use only. FaceNox: MIT.
Silent-Face: Apache-2.0.
