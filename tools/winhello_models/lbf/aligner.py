"""Faithful numpy re-implementation of FaceProcessorCore's LbfAligner (legacy IR landmark aligner).

Function names/addresses refer to FaceProcessorCore.dll 10.0.26100.9444 (public PDB).
Arithmetic is done in float32 in the same operation order as the x64 code so results match
the emulated Microsoft code (see emu.py) closely.
"""
from __future__ import annotations

import numpy as np

from .format import LbfModel, Stage, Regressor

F = np.float32
FLT_MIN = F(1.1754944e-38)


def similarity_coeff(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
    """LbfAligner::ComputeSimilarityTransformCoefficient (0x180011c10).

    Least-squares similarity src -> dst. Returns a 3x3 row-major matrix
    [[a, b, tx], [-b, a, ty], [0, 0, 1]]  (x' = a*x + b*y + tx, y' = -b*x + a*y + ty).
    """
    n = len(src)
    sx = sy = dx = dy = F(0)
    l0 = l1 = l2 = F(0)  # SSE lanes: |s|^2, s.d, cross
    for i in range(n):
        x, y = F(src[i, 0]), F(src[i, 1])
        u, v = F(dst[i, 0]), F(dst[i, 1])
        sy = F(sy + y)
        sx = F(sx + x)
        dx = F(dx + u)
        dy = F(dy + v)
        l0 = F(l0 + F(F(y * y) + F(x * x)))
        l1 = F(l1 + F(F(v * y) + F(u * x)))
        l2 = F(l2 + F(F(u * y) - F(v * x)))
    nf = F(n)
    my, mx = F(sy / nf), F(sx / nf)
    e2, cross = F(l0 / nf), F(l2 / nf)
    mu, mv, dot = F(dx / nf), F(dy / nf), F(l1 / nf)
    var = F(F(e2 - F(mx * mx)) - F(my * my))
    c31 = F(-mx / var)
    c32 = F(e2 / var)
    c33 = F(F(1.0) / var)
    c34 = F(-my / var)
    a = F(F(F(c31 * mu) + F(c33 * dot)) + F(c34 * mv))
    b = F(F(F(c34 * mu) + F(c33 * cross)) - F(c31 * mv))
    ty = F(F(F(c34 * dot) - F(c31 * cross)) + F(c32 * mv))
    tx = F(F(F(c34 * cross) + F(c31 * dot)) + F(c32 * mu))
    return np.array([[a, b, tx], [-b, a, ty], [0, 0, 1]], F)


def affine(m: np.ndarray, pts: np.ndarray) -> np.ndarray:
    """LbfAligner::AffineTransform (0x180011b8c)."""
    x, y = pts[:, 0].astype(F), pts[:, 1].astype(F)
    out = np.empty_like(pts, dtype=F)
    out[:, 0] = (m[0, 0] * x + m[0, 1] * y) + m[0, 2]
    out[:, 1] = (m[1, 0] * x + m[1, 1] * y) + m[1, 2]
    return out


def diff_similarity_coeff(mean: np.ndarray, shape: np.ndarray) -> np.ndarray:
    """LbfAligner::ComputeDifferencialSimilarityTransformationCoefficient (0x180011534).

    Rotation from a similarity fit of the centred mean shape to the centred, per-axis
    normalised current shape, then separate x/y scales and a translation, so that the
    returned matrix maps mean-shape coordinates onto the current shape.
    """
    n = len(mean)
    a = mean.astype(F).copy()
    s = shape.astype(F).copy()
    mxs = F(0); mys = F(0)
    for i in range(n):
        mys = F(mys + a[i, 1]); mxs = F(mxs + a[i, 0])
    mx, my = F(mxs / F(n)), F(mys / F(n))
    a[:, 0] -= mx; a[:, 1] -= my
    sxs = F(0); sys_ = F(0)
    for i in range(n):
        sys_ = F(sys_ + s[i, 1]); sxs = F(sxs + s[i, 0])
    cx, cy = F(sxs / F(n)), F(sys_ / F(n))
    s[:, 0] -= cx; s[:, 1] -= cy
    ax2 = ay2 = sx2 = sy2 = F(0)
    for i in range(n):
        ay2 = F(ay2 + F(a[i, 1] * a[i, 1])); ax2 = F(ax2 + F(a[i, 0] * a[i, 0]))
        sx2 = F(sx2 + F(s[i, 0] * s[i, 0])); sy2 = F(sy2 + F(s[i, 1] * s[i, 1]))
    nsx, nsy = F(np.sqrt(sx2)), F(np.sqrt(sy2))
    s[:, 0] *= F(F(np.sqrt(ax2)) / nsx)
    s[:, 1] *= F(F(np.sqrt(ay2)) / nsy)
    m = similarity_coeff(a, s)
    inv = F(F(1.0) / F(np.sqrt(F(m[0, 0] * m[0, 0]) + F(m[0, 1] * m[0, 1]))))
    m[0, 0] *= inv; m[0, 1] *= inv; m[1, 0] *= inv; m[1, 1] *= inv
    m[0, 2] = 0; m[1, 2] = 0
    r = affine(m, a)
    rx2 = ry2 = F(0)
    for i in range(n):
        rx2 = F(rx2 + F(r[i, 0] * r[i, 0])); ry2 = F(ry2 + F(r[i, 1] * r[i, 1]))
    kx = F(nsx / F(np.sqrt(rx2)))
    ky = F(nsy / F(np.sqrt(ry2)))
    m[0, 0] *= kx; m[0, 1] *= kx
    m[1, 0] *= ky; m[1, 1] *= ky
    m[1, 2] = F(cy - F(F(m[1, 1] * my) + F(m[1, 0] * mx)))
    m[0, 2] = F(cx - F(F(m[0, 1] * my) + F(m[0, 0] * mx)))
    m[2] = (0, 0, 1)
    return m


def _stage_arrays(stage: Stage):
    if not hasattr(stage, "_packed"):
        depth_nodes = max(len(t) for t in stage.trees)
        nodes = np.zeros((stage.num_trees, depth_nodes), dtype=stage.trees[0].dtype)
        for i, t in enumerate(stage.trees):
            nodes[i, :len(t)] = t
        stage._packed = nodes
    return stage._packed


def stage_leaves(stage: Stage, mean: np.ndarray, shape: np.ndarray, img: np.ndarray) -> np.ndarray:
    """Tree traversal of Aligner::Predict (0x180010cb0) / Stage::Predict (0x180010850).

    Each node holds two offsets in mean-shape units relative to its landmark, transformed by the
    rotation/scale of the mean->current similarity; feature = (I1 - I2) / (I1 + I2 + FLT_MIN),
    pixels outside the image read as 0; go to the low child when thr >= feature.
    Returns the global leaf index (regressor row) per tree.
    """
    m = diff_similarity_coeff(mean, shape)
    c0, c1, c3, c4 = m[0, 0], m[0, 1], m[1, 0], m[1, 1]
    nodes = _stage_arrays(stage)
    h, w = img.shape
    t = np.arange(stage.num_trees)
    lm = shape[t // stage.trees_per_point].astype(F)
    lx, ly = lm[:, 0], lm[:, 1]
    cur = np.zeros(stage.num_trees, np.int64)
    leaf = np.full(stage.num_trees, -1, np.int64)
    flat = img.reshape(-1)
    for _ in range(64):
        active = leaf < 0
        if not active.any():
            break
        nd = nodes[t, cur]

        def pix(dx, dy):
            px = np.trunc(((c0 * dx + lx) + c1 * dy) + F(0.5)).astype(np.int64)
            py = np.trunc(((c3 * dx + ly) + c4 * dy) + F(0.5)).astype(np.int64)
            ok = (px >= 0) & (px < w) & (py >= 0) & (py < h)
            v = np.zeros(len(px), F)
            v[ok] = flat[py[ok] * w + px[ok]]
            return v

        p1 = pix(nd["x1"], nd["y1"])
        p2 = pix(nd["x2"], nd["y2"])
        feat = (p1 - p2) / ((p2 + p1) + FLT_MIN)
        go_low = nd["thr"] >= feat
        ch = nd["children"].astype(np.int64)
        child = np.where(go_low, ch & 0xFFFF, ch >> 16)
        is_leaf = (child & 0x8000) != 0
        idx = child & 0x7FFF
        newleaf = active & is_leaf
        leaf[newleaf] = idx[newleaf]
        cur = np.where(active & ~is_leaf, idx, cur)
    return leaf


def regress(reg: Regressor, leaves: np.ndarray) -> np.ndarray:
    """Regressor::PredictCompressed / PredictNormal (0x180011ed0 / 0x180011920): sum of leaf rows + bias row."""
    out = np.zeros(reg.cols, F)
    if reg.compressed:
        sc = F(reg.scale)
        for r in leaves:
            out += reg.raw[r].astype(F) * sc
        out += reg.raw[reg.rows - 1].astype(F) * sc
    else:
        for r in leaves:
            out += reg.raw[r]
        out += reg.raw[reg.rows - 1]
    return out


def box_init(model: LbfModel, rect) -> np.ndarray:
    """Initial shape from a detector RECT (left, top, right, bottom; right/bottom exclusive)."""
    l, t, r, b = (int(v) for v in rect)
    corners = np.array([[l, t], [r - 1, t], [l, b - 1], [r - 1, b - 1]], F)
    return affine(similarity_coeff(model.unit_box, corners), model.mean_shape)


def track_init(model: LbfModel, prev: np.ndarray) -> np.ndarray:
    """Initial shape from previous landmarks (TrackFace 0x180042fe0 -> Predict(a6=true))."""
    return affine(diff_similarity_coeff(model.mean_shape, prev), model.mean_shape)


def align(model: LbfModel, img: np.ndarray, rect=None, prev=None, return_stages=False):
    """Aligner::Predict. img: 2-D uint8 luminance (full frame). Exactly one of rect/prev.

    Returns (points (N,2) float32, classifier_score or None[, per-stage shapes]).
    Microsoft marks the face as rejected (AlignerContext+260 / tracking failure) when score <= 0.
    """
    img = np.ascontiguousarray(img, np.uint8)
    shape = track_init(model, prev) if prev is not None else box_init(model, rect)
    stages = [shape.copy()]
    mean = model.mean_shape
    for st in model.stages:
        m = diff_similarity_coeff(mean, shape)
        leaves = stage_leaves(st, mean, shape, img)
        d = regress(st.regressor, leaves).reshape(-1, 2)
        nx = (m[0, 1] * d[:, 1] + m[0, 0] * d[:, 0]) + shape[:, 0]
        ny = (m[1, 1] * d[:, 1] + m[1, 0] * d[:, 0]) + shape[:, 1]
        shape = np.stack([nx, ny], 1).astype(F)
        stages.append(shape.copy())
    score = None
    if model.classifier is not None:
        leaves = stage_leaves(model.stages[-1], mean, shape, img)
        score = float(regress(model.classifier, leaves)[0])
    if return_stages:
        return shape, score, stages
    return shape, score
