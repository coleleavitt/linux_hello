"""Face occlusion classifier (engine core MODEL_FILES 120 = HelloFaceCipher-obfuscated ONNX,
plain predecessor 106). Reimplements FaceOcclusionDetector::TestOcclusion
(FaceRecognitionEngineAdapter.dll 0x1800abe28):

  * input: the face rectangle (record +16: x0, y0, x1, y1) of the 8-bit lit luminance crop,
    resampled to 128x128 by sub_18004B6FC: sample (c, r) at
    x = (c - 63.5) * w/128 + (w - 1)/2,  y = (r - 63.5) * h/128 + (h - 1)/2  (bilinear, sub_180023580)
  * x/127.5 - 1, shape 1x1x128x128 (sub_18004B5B8)
  * softmax over the 4 outputs (sub_18003C0B0); telemetry names them
    faceWithoutOcclusion, faceWithMaskOcclusion, faceWithSunglassesOcclusion, faceWithHandOcclusion
  * occlusion score = 1 - p[0]; TrackedFace::IsFaceOccluded (0x1800a8408): occluded if
    score > 0.9, or > 0.5 while the track is enrolling (state 3).
"""
from __future__ import annotations

import numpy as np

CLASSES = ("none", "mask", "sunglasses", "hand")


def box_input(gray: np.ndarray, box) -> np.ndarray:
    x0, y0, x1, y1 = [int(round(v)) for v in box]
    h_img, w_img = gray.shape
    w, h = x1 - x0, y1 - y0
    k = np.arange(128, dtype=np.float32) - np.float32(63.5)
    xs = x0 + k * np.float32(w / 128) + np.float32((w - 1) * 0.5)
    ys = y0 + k * np.float32(h / 128) + np.float32((h - 1) * 0.5)
    X, Y = np.meshgrid(np.clip(xs, 0, w_img - 1), np.clip(ys, 0, h_img - 1))
    xa, ya = np.floor(X).astype(int), np.floor(Y).astype(int)
    xb, yb = np.minimum(xa + 1, w_img - 1), np.minimum(ya + 1, h_img - 1)
    fx, fy = X - xa, Y - ya
    f = gray.astype(np.float32)
    v = f[ya, xa] * (1 - fx) * (1 - fy) + f[ya, xb] * fx * (1 - fy) + f[yb, xa] * (1 - fx) * fy + f[yb, xb] * fx * fy
    u8 = v.astype(np.uint8)
    return (u8.astype(np.float32) / np.float32(127.5) - 1)[None, None]


def scores(session, x: np.ndarray) -> np.ndarray:
    z = session.run(None, {session.get_inputs()[0].name: x})[0].ravel().astype(np.float64)
    p = np.exp(z - z.max())
    return p / p.sum()


def occluded(p: np.ndarray, enrolling: bool = False) -> bool:
    return (1 - p[0]) > (0.5 if enrolling else 0.9)
