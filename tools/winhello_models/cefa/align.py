"""Reimplementation of Microsoft's recognition crop for the v5 CNN.

Source: FaceRecognitionEngineAdapter.dll, facesdk::recognition::FaceRecognitionCnnImpl2
(vtable slot 6, sub_180015BE0):
  1. sub_18001614C: least-squares similarity transform from the first 5 of the 27 IR
     aligner landmarks (left eye, right eye, nose tip, left / right mouth corner) to the
     reference points REF_281 (table at 0x1800D7F40) in a 281x281 canvas.
  2. sub_180017200 inverts it; sub_180016B20 warps the face into the 281x281 canvas
     (bilinear, truncating float->uint8, outside -> 0).
  3. sub_180016440 resamples the canvas to 128x128: sample (r, c) is taken at canvas
     coordinate (k - 64) * 230/127 + 140.5 (bilinear, outside [0, 280] -> 0).
  4. DNNTestLib FaceDataLayer (sub_180017AA0) converts uint8 to float as x/127.5 - 1
     when the model has no mean image (otherwise x - mean).
"""
from __future__ import annotations

import numpy as np

REF_281 = np.array(
    [
        [97.82990264892578, 127.7791976928711],
        [181.05320739746094, 126.33809661865234],
        [139.57400512695312, 179.00059509277344],
        [102.69640350341797, 216.70140075683594],
        [179.30679321289062, 215.37330627441406],
    ],
    dtype=np.float32,
)
CANVAS = 281
OUT = 128
SPAN = 230.0
CENTER = 140.5


def similarity_image_to_canvas(pts: np.ndarray, ref: np.ndarray = REF_281) -> np.ndarray:
    """sub_18001614C: 3x3 matrix mapping image (x, y) -> canvas (u, w)."""
    p = np.asarray(pts, np.float32)[:5]
    x, y = p[:, 0], p[:, 1]
    u, w = ref[:, 0], ref[:, 1]
    mx, my, mu, mw = x.mean(), y.mean(), u.mean(), w.mean()
    sxy2 = (x * x + y * y).mean()
    s1 = (u * x + w * y).mean()
    s2 = (u * y - w * x).mean()
    var = sxy2 - mx * mx - my * my
    if abs(var) < 1e-6:
        raise ValueError("degenerate landmarks")
    a = (s1 - mx * mu - my * mw) / var
    b = (s2 - my * mu + mx * mw) / var
    tx = (mu * sxy2 - s1 * mx - s2 * my) / var
    ty = (mw * sxy2 - s1 * my + s2 * mx) / var
    return np.array([[a, b, tx], [-b, a, ty], [0, 0, 1]], np.float32)


def _bilinear(img: np.ndarray, xs: np.ndarray, ys: np.ndarray, xmax: float, ymax: float) -> np.ndarray:
    h, w = img.shape
    out = np.zeros(xs.shape, np.uint8)
    ok = (xs >= 0) & (xs <= xmax) & (ys >= 0) & (ys <= ymax)
    x, y = xs[ok], ys[ok]
    x0 = np.floor(x).astype(np.int64)
    y0 = np.floor(y).astype(np.int64)
    x1 = np.minimum(x0 + 1, w - 1)
    y1 = np.minimum(y0 + 1, h - 1)
    fx1, fy1 = (x0 + 1) - x, (y0 + 1) - y
    fx0, fy0 = x - x0, y - y0
    f = img.astype(np.float32)
    v = f[y0, x0] * fy1 * fx1 + f[y1, x0] * fy0 * fx1 + f[y1, x1] * fy0 * fx0 + f[y0, x1] * fy1 * fx0
    out[ok] = v.astype(np.int64).clip(0, 255).astype(np.uint8)
    return out


def warp_to_canvas(gray: np.ndarray, pts5: np.ndarray) -> np.ndarray:
    m = similarity_image_to_canvas(pts5)
    inv = np.linalg.inv(m.astype(np.float64)).astype(np.float32)
    jj, ii = np.meshgrid(np.arange(CANVAS, dtype=np.float32), np.arange(CANVAS, dtype=np.float32))
    xs = inv[0, 0] * jj + inv[0, 1] * ii + inv[0, 2]
    ys = inv[1, 0] * jj + inv[1, 1] * ii + inv[1, 2]
    h, w = gray.shape
    return _bilinear(gray, xs, ys, w - 1, h - 1)


def canvas_to_128(canvas: np.ndarray) -> np.ndarray:
    k = np.arange(OUT, dtype=np.float32)
    coord = (k - OUT // 2) * np.float32(SPAN / (OUT - 1)) + np.float32(CENTER)
    xs, ys = np.meshgrid(coord, coord)
    return _bilinear(canvas, xs, ys, CANVAS - 1, CANVAS - 1)


def recognition_crop(gray: np.ndarray, pts5: np.ndarray) -> np.ndarray:
    """uint8 128x128 crop exactly as FaceRecognitionCnnImpl2 builds it."""
    return canvas_to_128(warp_to_canvas(gray, pts5))


def to_input(crop_u8: np.ndarray) -> np.ndarray:
    """FaceDataLayer default normalisation (no mean image): 1x1x128x128 float32."""
    return (crop_u8.astype(np.float32) / np.float32(127.5) - np.float32(1.0))[None, None]
