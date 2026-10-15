"""Engine-side 128x128 face crop that feeds the IR/colour liveness CNNs (and, INFERRED, recognition).

FaceRecognitionEngineAdapter.dll (no public PDB; addresses 10.0.26100.9444):
  sub_180015F68  crop entry: (image, 5 landmarks, w, h, stride, 128, 128, channels, out)
  sub_18001614C  least-squares similarity landmarks -> 5 reference points (same formula as
                 LbfAligner::ComputeSimilarityTransformCoefficient)
  sub_180017200  3x3 inverse
  sub_180016B20  warp into a 281x281 canvas: bilinear, result truncated to uint8, outside -> 0
  sub_180016440  resample canvas -> 128x128 at (i - 64) * 230/127 + 140.5, bilinear, truncation
The 5 landmarks are the first five aligner points (eye centres, nose tip, mouth corners), which is
also the RetinaFace/SCRFD 5-point convention.
"""
from __future__ import annotations

import numpy as np

from .aligner import similarity_coeff

F = np.float32

# rdata 0x1800D7F68 in FaceRecognitionEngineAdapter.dll
REF5_281 = np.array([[97.8299, 127.7792], [181.0532, 126.3381], [139.574, 179.0006],
                     [102.6964, 216.7014], [179.3068, 215.3733]], F)
CANVAS = 281


def _inv3(m):
    # sub_180017200: adjugate / determinant, float32
    a = [F(v) for v in m.reshape(-1)]
    c0 = F(F(a[4] * a[8]) - F(a[5] * a[7]))
    c3 = F(F(a[6] * a[5]) - F(a[3] * a[8]))
    c6 = F(F(a[3] * a[7]) - F(a[6] * a[4]))
    det = F(F(F(a[0] * c0) + F(a[1] * c3)) + F(a[2] * c6))
    r = np.empty(9, F)
    r[0] = c0 / det
    r[1] = F(F(a[2] * a[7]) - F(a[1] * a[8])) / det
    r[2] = F(F(a[1] * a[5]) - F(a[2] * a[4])) / det
    r[3] = c3 / det
    r[4] = F(F(a[0] * a[8]) - F(a[2] * a[6])) / det
    r[5] = F(F(a[2] * a[3]) - F(a[0] * a[5])) / det
    r[6] = c6 / det
    r[7] = F(F(a[1] * a[6]) - F(a[0] * a[7])) / det
    r[8] = F(F(a[0] * a[4]) - F(a[1] * a[3])) / det
    return r.reshape(3, 3)


def _bilinear_trunc(src, x, y, xmax, ymax, clamp):
    """Shared sampling rule of sub_180016B20/sub_180016440 (single channel)."""
    out = np.zeros(x.shape, np.uint8)
    ok = (x >= 0) & (x <= xmax) & (y >= 0) & (y <= ymax)
    xi, yi = x[ok], y[ok]
    x0 = np.floor(xi).astype(np.int64); y0 = np.floor(yi).astype(np.int64)
    exact = (xi == x0) & (yi == y0)
    x1 = np.minimum(x0 + 1, clamp[0]); y1 = np.minimum(y0 + 1, clamp[1])
    fx = (xi - x0).astype(F); gx = ((x0 + 1) - xi).astype(F)
    fy = (yi - y0).astype(F); gy = ((y0 + 1) - yi).astype(F)
    p00 = src[y0, x0].astype(F); p10 = src[y0, x1].astype(F)
    p01 = src[y1, x0].astype(F); p11 = src[y1, x1].astype(F)
    v = ((p00 * (gy * gx) + p01 * (fy * gx)) + p11 * (fy * fx)) + p10 * (gy * fx)
    v = np.where(exact, p00, np.trunc(v))
    out[ok] = v.astype(np.uint8)
    return out


def engine_crop_128(img: np.ndarray, pts5: np.ndarray) -> np.ndarray:
    """128x128 uint8 crop of a single-channel image from the five landmarks."""
    img = np.ascontiguousarray(img, np.uint8)
    h, w = img.shape
    m = similarity_coeff(np.asarray(pts5, F), REF5_281)
    inv = _inv3(m)
    r, c = np.mgrid[:CANVAS, :CANVAS].astype(F)
    x = ((c * inv[0, 0]) + (r * inv[0, 1])) + inv[0, 2]
    y = ((c * inv[1, 0]) + (r * inv[1, 1])) + inv[1, 2]
    canvas = _bilinear_trunc(img, x, y, F(w - 1), F(h - 1), (w - 1, h - 1))
    step = F(F(230.0) / F(127))
    i = np.arange(128) - 64
    s = (i.astype(F) * step) + F(140.5)
    yy, xx = np.meshgrid(s, s, indexing="ij")
    return _bilinear_trunc(canvas, xx, yy, F(280), F(280), (280, 280))
