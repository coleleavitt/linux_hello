"""Re-implementation of the Windows Hello anti-spoofing input preparation.

Sources (FaceRecognitionEngineAdapter.dll unless noted):

* ``align_crop``  sub_180015F68 -> sub_18001614C (5-point similarity fit to the
  template at 0x1800D7F68, 281x281 space) -> sub_180016B20 (inverse-map warp to
  281x281, bilinear, truncation to u8, outside pixels = 0) -> sub_180016440
  (resample the central 230x230 of that to 128x128, bilinear, truncation).
  Used by slots 11/12 (and 13 with 3 channels).
* ``thumbnail``  FaceProcessorCore ResizeImage + Bilinear<uchar> 0x1800198fc:
  whole IR frame stretched to 128x128.  Used by slots 14/15 (record+160333).

The 5 landmarks are the first five points of the FaceProcessor landmark array
(left eye centre, right eye centre, nose tip, left mouth corner, right mouth
corner in image orientation, see doc).  All arithmetic is float32 like the DLL.
"""
from __future__ import annotations

import numpy as np

f32 = np.float32

# unk_1800D7F68: 5 (x, y) points in the 281x281 aligned face.
TEMPLATE_281 = np.array(
    [[97.82990264892578, 127.7791976928711],
     [181.05320739746094, 126.33809661865234],
     [139.57400512695312, 179.00059509277344],
     [102.69640350341797, 216.70140075683594],
     [179.30679321289062, 215.37330627441406]], dtype=f32)
ALIGN_SIZE = 281


def fit_similarity(src: np.ndarray, dst: np.ndarray = TEMPLATE_281) -> np.ndarray:
    """sub_18001614C: least-squares u = a*x + b*y + tx, v = -b*x + a*y + ty (src -> dst).

    Returns the 2x3 matrix [[a, b, tx], [-b, a, ty]].
    """
    src = np.asarray(src, f32)
    dst = np.asarray(dst, f32)
    x, y = src[:, 0], src[:, 1]
    u, v = dst[:, 0], dst[:, 1]
    n = f32(5.0)
    mx, my, mu, mv = x.sum() / n, y.sum() / n, u.sum() / n, v.sum() / n
    sxx = (x * x + y * y).sum() / n
    sp = (u * x + v * y).sum() / n
    sm = (u * y - v * x).sum() / n
    var = sxx - mx * mx - my * my
    if abs(var) < 1e-6:
        raise ValueError("degenerate landmarks")
    a = (sp - mx * mu - my * mv) / var
    b = (sm - my * mu + mx * mv) / var
    tx = (-my * sm - mx * sp + sxx * mu) / var
    ty = (-my * sp + mx * sm + sxx * mv) / var
    return np.array([[a, b, tx], [-b, a, ty]], f32)


def _bilinear_trunc(img: np.ndarray, sx: np.ndarray, sy: np.ndarray) -> np.ndarray:
    """Bilinear sample with the DLL's rules: outside [0,w-1]x[0,h-1] -> 0, result truncated."""
    h, w = img.shape[:2]
    chans = img.shape[2] if img.ndim == 3 else 1
    im = img.reshape(h, w, chans).astype(f32)
    inside = (sx >= 0) & (sx <= w - 1) & (sy >= 0) & (sy <= h - 1)
    sxc = np.where(inside, sx, 0).astype(f32)
    syc = np.where(inside, sy, 0).astype(f32)
    x0 = np.floor(sxc).astype(np.int64)
    y0 = np.floor(syc).astype(np.int64)
    x1 = np.minimum(x0 + 1, w - 1)
    y1 = np.minimum(y0 + 1, h - 1)
    fx = (sxc - x0.astype(f32))[..., None]
    gx = ((x0 + 1).astype(f32) - sxc)[..., None]
    fy = (syc - y0.astype(f32))[..., None]
    gy = ((y0 + 1).astype(f32) - syc)[..., None]
    val = (im[y0, x0] * (gy * gx) + im[y0, x1] * (gy * fx)) + im[y1, x0] * (fy * gx) + im[y1, x1] * (fy * fx)
    out = np.where(inside[..., None], np.trunc(val), 0).astype(np.uint8)
    return out if img.ndim == 3 else out[..., 0]


def warp_281(img: np.ndarray, m: np.ndarray) -> np.ndarray:
    """sub_180016B20: out(x, y) = img(inv(M) @ (x, y, 1))."""
    full = np.vstack([m.astype(np.float64), [0, 0, 1]])
    inv = np.linalg.inv(full).astype(f32)
    ys, xs = np.mgrid[0:ALIGN_SIZE, 0:ALIGN_SIZE].astype(f32)
    sx = xs * inv[0, 0] + ys * inv[0, 1] + inv[0, 2]
    sy = xs * inv[1, 0] + ys * inv[1, 1] + inv[1, 2]
    return _bilinear_trunc(img, sx, sy)


def resize_281_to(img281: np.ndarray, out_w: int = 128, out_h: int = 128) -> np.ndarray:
    """sub_180016440: src = (i - out/2) * 230/(out-1) + 140.5 on both axes."""
    i = np.arange(out_w, dtype=f32)
    j = np.arange(out_h, dtype=f32)
    sx = (i + f32(-(out_w // 2))) * (f32(230.0) / f32(out_w - 1)) + f32(140.5)
    sy = (j + f32(-(out_h // 2))) * (f32(230.0) / f32(out_h - 1)) + f32(140.5)
    gx, gy = np.meshgrid(sx, sy)
    return _bilinear_trunc(img281, gx, gy)


def align_crop(img: np.ndarray, landmarks5: np.ndarray, size: int = 128) -> np.ndarray:
    """Full sub_180015F68 path. img: HxW (or HxWxC) uint8; landmarks5: 5x2 (x, y) in img coords."""
    return resize_281_to(warp_281(img, fit_similarity(landmarks5)), size, size)


def thumbnail(frame: np.ndarray, size: int = 128) -> np.ndarray:
    """FaceProcessorCore ResizeImage: whole frame -> size x size (no aspect preservation)."""
    h, w = frame.shape[:2]
    i = np.arange(size, dtype=f32)
    sx = (i - f32(63.5)) * (f32(w) * f32(0.0078125)) + f32(w - 1) * f32(0.5)
    sy = (i - f32(63.5)) * (f32(h) * f32(0.0078125)) + f32(h - 1) * f32(0.5)
    gx, gy = np.meshgrid(sx, sy)
    return _bilinear_trunc(frame, gx, gy)


def difference_image(lit: np.ndarray, ambient: np.ndarray) -> np.ndarray:
    """FaceProcessorCore SubtractAmbientFrameFromIlluminatedFrame<uchar>: lit - min(lit, ambient)."""
    lit = lit.astype(np.int16)
    return (lit - np.minimum(lit, ambient.astype(np.int16))).astype(np.uint8)
