"""Python reimplementation of the Windows Hello infrared face detector (FaceTrackerInternal.dll).

Every step cites the function it mirrors (addresses: FaceTrackerInternal.dll, build 26300):
  Pyramid::InitAllLayers 0x1800040f0, Pyramid::Construct 0x180004680, Pyramid::ResizeImage 0x180003e10
  JointCascadeFaceDetector::DetectInLayer 0x1800061f0  (window scan, ferns, JDA, post filter, box)
  FernPredict1..8 (0x180007210 ...)                      (fern pre-filter; structure in the converted npz)
  JointCascadePredictProductionInfraredModel 0x18000ca80 (JDA cascade; LowEnd: 0x18000d610)
  ComputeSimilarityTransformationCoeff 0x18000df70, Inverse 0x18000e1b8, AffineTransform 0x18000c740
  OrientedHogPostFilter::Predict 0x1800054f0, LocalOrientedHoGDescriptor::CalculateOnePointDescriptor 0x18000a860
  JointCascadeFaceDetector::MergeDetectionResult 0x1800314a0
"""
from __future__ import annotations

import math
from dataclasses import dataclass
from pathlib import Path

import numpy as np

f32 = np.float32
WIN = 96          # AssignCascadeModel: this+30/+31 = 96
BORDER = 24       # JointCascadeFaceDetector::Detect: (WindowSlideConfig[4] - [0]) / 2 = (96 - 48) / 2
LAYERS_PER_OCTAVE = 4
FACE_IN_WINDOW = 48   # WindowSlideConfig[0]/[1]
STEP = 8              # WindowSlideConfig[2]/[3]
BOX_FACTOR = f32(1.1)
# ferns after which FernPredict1..8 increment the *a5 counter (OBSERVED via the symbolic harness)
FERN_MILESTONES = (1, 2, 4, 8, 16, 32, 64, 128, 256, 399, 400)  # JointCascadeFaceDetector ctor: this+38 = 0x3F8CCCCD


@dataclass
class Detection:
    box: tuple            # (left, top, right, bottom) in input-image pixels, ints like tagRECT
    score: float          # post-filter (oriented HoG) score; must be >= hog_threshold
    jda_score: float      # cascade score / 2^24 before the post filter
    landmarks: np.ndarray  # (L, 2) float32 in input-image pixels
    layer: int = -1
    window: tuple = ()


class Model:
    def __init__(self, npz: str | Path):
        z = np.load(npz, allow_pickle=False)
        self.z = z
        self.L = int(z["n_landmarks"])
        self.T = int(z["trees_per_stage"])
        self.n_stages = int(z["n_stages"])
        self.init_score = int(z["init_score"])
        self.leaf_weights = z["leaf_weights"].reshape(self.n_stages, self.T * 32).astype(np.int64)
        self.s0 = z["stage0_trees"]            # (T,31,7) imgA xA yA imgB xB yB thr, heap order
        self.s0_thr = z["stage0_reject"].astype(np.int64)
        nodes = z["nodes"]
        self.nodes = nodes.reshape(self.n_stages - 1, self.T, 63, 11)
        self.node_f = nodes.view(np.float32).reshape(self.n_stages - 1, self.T, 63, 11)
        self.reject = z["reject"].reshape(self.n_stages - 1, self.T).astype(np.int64)
        self.mean_shape = z["mean_shape"]
        self.deltas0 = z["deltas0"]
        self.bias = z["stage_bias"]
        self.deltas = z["stage_deltas"]
        self.ref = z["ref_shape"]
        self.hog_w = z["hog_weights"]
        self.hog_thr = f32(z["hog_threshold"])
        self.tmpl5 = z["hog_template5"]
        self.atan = z["atan_lut"]
        self.fern_tests = z["fern_tests"].astype(np.int64)
        self.fern_thr = z["fern_reject"].astype(np.int64)
        self.fern_w = z["fern_weights"].astype(np.int64)
        self.fern_init = int(z["fern_init_score"])
        self.fern_min_passed = int(z["fern_min_passed"])
        self.score_scale = float(z["score_scale"])


# ---------------------------------------------------------------- pyramid
@dataclass
class Layer:
    img: np.ndarray   # padded (h + 2B, w + 2B) uint8, zero border
    w: int
    h: int
    sx: float
    ox: float
    sy: float
    oy: float


def _nearest_resize(src: np.ndarray, w: int, h: int) -> np.ndarray:
    H, W = src.shape
    rx = f32(W - 1) / f32(w - 1)
    ry = f32(H - 1) / f32(h - 1)
    xs = (np.arange(w, dtype=f32) * rx + f32(0.5)).astype(np.int64)
    ys = (np.arange(h, dtype=f32) * ry + f32(0.5)).astype(np.int64)
    return src[ys[:, None], xs[None, :]]


def _half(src: np.ndarray, w: int, h: int) -> np.ndarray:
    """Pyramid::Construct 2x reduction: (a+b+c+d+2)>>2, odd edge column/row averaged, odd corner copied."""
    s = src.astype(np.int32)
    H, W = s.shape
    w2, h2 = (W + 1) >> 1, (H + 1) >> 1
    assert (w2, h2) == (w, h)
    out = np.empty((h2, w2), np.int32)
    hw, hh = W >> 1, H >> 1
    out[:hh, :hw] = (s[0:2 * hh:2, 0:2 * hw:2] + s[1:2 * hh:2, 0:2 * hw:2] + s[0:2 * hh:2, 1:2 * hw:2] + s[1:2 * hh:2, 1:2 * hw:2] + 2) >> 2
    if hw != w2:
        out[:hh, hw] = (s[0:2 * hh:2, 2 * hw] + s[1:2 * hh:2, 2 * hw]) >> 1
    if hh != h2:
        out[hh, :hw] = (s[2 * hh, 0:2 * hw:2] + s[2 * hh, 1:2 * hw:2]) >> 1
        if hw != w2:
            out[hh, hw] = s[2 * hh, 2 * hw]
    return out.astype(np.uint8)


def build_pyramid(gray: np.ndarray) -> list[Layer]:
    H, W = gray.shape
    layers: list[Layer] = []
    raw: list[np.ndarray] = []

    def add(img, w, h, sx, ox, sy, oy):
        p = np.zeros((h + 2 * BORDER, w + 2 * BORDER), np.uint8)
        p[BORDER:BORDER + h, BORDER:BORDER + w] = img
        layers.append(Layer(p, w, h, sx, ox, sy, oy)); raw.append(img)

    add(gray, W, H, 1.0, 0.0, 1.0, 0.0)
    for i in range(1, LAYERS_PER_OCTAVE):
        r = f32(0.5) ** (f32(i) / f32(LAYERS_PER_OCTAVE))
        w = int(float(f32(W) * r) + 0.5); h = int(float(f32(H) * r) + 0.5)  # cvttsd2si after +0.5 in double
        add(_nearest_resize(gray, w, h), w, h, float(f32(W - 1) / f32(w - 1)), 0.0, float(f32(H - 1) / f32(h - 1)), 0.0)
    i = LAYERS_PER_OCTAVE
    while True:
        p = layers[i - LAYERS_PER_OCTAVE]
        w, h = (p.w + 1) >> 1, (p.h + 1) >> 1
        if w <= 3 or h <= 3:
            break
        add(_half(raw[i - LAYERS_PER_OCTAVE], w, h), w, h, p.sx * 2, p.sx * 0.5 + p.ox, p.sy * 2, p.sy * 0.5 + p.oy)
        i += 1
    return layers


# ---------------------------------------------------------------- ferns
def fern_cascade(m: Model, half: np.ndarray, xs: np.ndarray, ys: np.ndarray):
    """FernPredict1..8 on the 24x24 half-resolution patch at padded (24 + x/2, 24 + y/2).
    Returns (passed: bool[N], milestones: int[N]) where milestones counts how many of FERN_MILESTONES ferns were
    passed (the `*a5` counter DetectInLayer compares with 8)."""
    ox = BORDER + (xs >> 1); oy = BORDER + (ys >> 1)
    N = xs.size
    score = np.full(N, m.fern_init, np.int64)
    alive = np.ones(N, bool)
    npass = np.zeros(N, np.int64)
    idx_alive = np.arange(N)
    for f in range(m.fern_tests.shape[0]):
        t = m.fern_tests[f]   # (8,5): xA yA xB yB thr ; bit j = (A - B) > thr
        a = half[oy[idx_alive, None] + t[None, :, 1], ox[idx_alive, None] + t[None, :, 0]].astype(np.int64)
        b = half[oy[idx_alive, None] + t[None, :, 3], ox[idx_alive, None] + t[None, :, 2]].astype(np.int64)
        bits = ((a - b) > t[None, :, 4]).astype(np.int64)
        fi = (bits << np.arange(8)).sum(1)
        score[idx_alive] += m.fern_w[f, fi]
        ok = score[idx_alive] >= m.fern_thr[f]
        if f + 1 in FERN_MILESTONES:   # FernPredict* only bump *a5 after these fern counts
            npass[idx_alive[ok]] += 1
        alive[idx_alive[~ok]] = False
        idx_alive = idx_alive[ok]
        if idx_alive.size == 0:
            break
    return alive, npass


# ---------------------------------------------------------------- similarity transform helpers (float32 like the DLL)
def similarity(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
    """ComputeSimilarityTransformationCoeff: least-squares a,b,tx,ty mapping src -> dst; src (N,P,2), dst (P,2).
    Returns (N,6) = [a, b, tx, -b, a, ty] (row-major 2x3)."""
    n = f32(src.shape[1])
    sx = src[..., 0].astype(f32); sy = src[..., 1].astype(f32)
    dx = np.broadcast_to(dst[:, 0].astype(f32)[None], sx.shape); dy = np.broadcast_to(dst[:, 1].astype(f32)[None], sx.shape)

    def seq(v):  # the DLL accumulates point by point (no pairwise summation)
        return np.cumsum(v, axis=1, dtype=f32)[:, -1]
    msx = seq(sx) / n; msy = seq(sy) / n
    mdx = seq(dx) / n; mdy = seq(dy) / n
    mss = seq(sy * sy + sx * sx) / n
    sdot = seq(dy * sy + dx * sx) / n
    cr = seq(dx * sy - dy * sx) / n
    det = (mss - msx * msx) - msy * msy
    v28 = -msx / det; v29 = mss / det; v30 = f32(1) / det; v31 = -msy / det
    a = (v28 * mdx + v30 * sdot) + v31 * mdy
    b = (v31 * mdx + v30 * cr) - v28 * mdy
    ty = (v31 * sdot - v28 * cr) + v29 * mdy
    tx = (v31 * cr + v28 * sdot) + v29 * mdx
    return np.stack([a, b, tx, -b, a, ty], 1).astype(f32)


def invert(M: np.ndarray) -> np.ndarray:
    """Inverse of [[m0 m1 m2][m3 m4 m5][0 0 1]] (Inverse 0x18000e1b8, general 3x3 cofactor form)."""
    m0, m1, m2, m3, m4, m5 = (M[:, i] for i in range(6))
    z = f32(0); o = f32(1)
    v4 = m4 * o - m5 * z
    v5 = z * m5 - m3 * o
    v7 = m3 * z - z * m4
    det = (m0 * v4 + m1 * v5) + m2 * v7
    r0 = v4 / det
    r1 = (m2 * z - m1 * o) / det
    r2 = (m1 * m5 - m2 * m4) / det
    r3 = v5 / det
    r4 = (m0 * o - m2 * z) / det
    r5 = (m2 * m3 - m0 * m5) / det
    return np.stack([r0, r1, r2, r3, r4, r5], 1).astype(f32)


def affine(M: np.ndarray, pts: np.ndarray) -> np.ndarray:
    x = pts[..., 0]; y = pts[..., 1]
    X = (M[:, 0, None] * x + M[:, 1, None] * y) + M[:, 2, None]
    Y = (M[:, 3, None] * x + M[:, 4, None] * y) + M[:, 5, None]
    return np.stack([X, Y], -1).astype(f32)


# ---------------------------------------------------------------- JDA cascade
def jda_predict(m: Model, full, half, quarter, xs, ys):
    """JointCascadePredictProduction*Model for windows with padded full-layer origin (x, y).
    Returns (alive bool[N], score int64[N], shape float32[N,L,2] in window coordinates)."""
    N = xs.size
    origins = [(ys, xs), (BORDER // 2 + (ys >> 1), BORDER // 2 + (xs >> 1)),
               (3 * WIN // 16 + (ys >> 2), 3 * WIN // 16 + (xs >> 2))]
    imgs = [full, half, quarter]
    score = np.full(N, m.init_score, np.int64)
    alive = np.ones(N, bool)
    act = np.arange(N)
    leaves = np.zeros((N, m.T), np.int64)
    # stage 0 (hard-coded in the DLL; fixed window-relative pixel offsets)
    for t in range(m.T):
        tr = m.s0[t]
        j = np.zeros(act.size, np.int64)
        for _ in range(5):
            f = tr[j]
            pa = np.empty(act.size, np.int64); pb = np.empty(act.size, np.int64)
            for s in range(3):
                sa = f[:, 0] == s
                if sa.any():
                    oy, ox = origins[s][0][act[sa]], origins[s][1][act[sa]]
                    pa[sa] = imgs[s][oy + f[sa, 2], ox + f[sa, 1]]
                sb = f[:, 3] == s
                if sb.any():
                    oy, ox = origins[s][0][act[sb]], origins[s][1][act[sb]]
                    pb[sb] = imgs[s][oy + f[sb, 5], ox + f[sb, 4]]
            j = 2 * j + 1 + ((pa - pb) <= f[:, 6])
        leaf = 32 * t + (j - 31)
        leaves[act, t] = leaf
        score[act] += m.leaf_weights[0, leaf]
        ok = score[act] >= m.s0_thr[t]
        alive[act[~ok]] = False
        act = act[ok]
        if act.size == 0:
            return alive, score, None
    shape = np.repeat(m.mean_shape[None], N, 0).astype(f32)
    # the DLL accumulates leaf deltas sequentially (SSE adds in tree order)
    acc = np.repeat(m.mean_shape[None], act.size, 0).astype(f32)
    for t in range(m.T):
        acc = acc + m.deltas0[leaves[act, t]]
    shape[act] = acc
    for st in range(1, m.n_stages):
        cur = shape[act]
        M = similarity(cur, m.ref)
        Mi = invert(M)
        norm = affine(M, cur)                      # shape in reference frame (a9)
        flat = norm.reshape(act.size, -1)
        lw = m.leaf_weights[st]
        leaf_idx = np.zeros((act.size, m.T), np.int64)
        keep = np.ones(act.size, bool)
        k = np.arange(act.size)                  # rows of `act` still alive in this stage
        for t in range(m.T):
            nd = m.nodes[st - 1, t]; nf = m.node_f[st - 1, t]
            j = np.zeros(k.size, np.int64)
            while True:
                lf = nd[j, 0]
                inner = lf == -1
                if not inner.any():
                    break
                kk = k[inner]; jj = j[inner]
                pts = []
                for ci, dxi, dyi in ((2, 3, 4), (5, 6, 7)):
                    ii = nd[jj, ci]
                    px = flat[kk, ii] + nf[jj, dxi]
                    py = flat[kk, ii + 1] + nf[jj, dyi]
                    X = ((Mi[kk, 1] * py + Mi[kk, 0] * px) + Mi[kk, 2]) + f32(0.5)
                    Y = ((Mi[kk, 4] * py + Mi[kk, 3] * px) + Mi[kk, 5]) + f32(0.5)
                    X = np.clip(X.astype(np.int64), 0, WIN - 1); Y = np.clip(Y.astype(np.int64), 0, WIN - 1)
                    pts.append((X, Y))
                sc = nd[jj, 1]
                pa = np.empty(kk.size, np.int64); pb = np.empty(kk.size, np.int64)
                for s in range(3):
                    sel = sc == s
                    if sel.any():
                        a_ = act[kk[sel]]
                        oy, ox = origins[s][0][a_], origins[s][1][a_]
                        pa[sel] = imgs[s][oy + (pts[0][1][sel] >> s), ox + (pts[0][0][sel] >> s)]
                        pb[sel] = imgs[s][oy + (pts[1][1][sel] >> s), ox + (pts[1][0][sel] >> s)]
                go_right = (pa - pb) <= nd[jj, 8]
                j[inner] = np.where(go_right, nd[jj, 10], nd[jj, 9])
            leaf = nd[j, 0]
            leaf_idx[k, t] = leaf
            score[act[k]] += lw[leaf]
            ok = score[act[k]] >= m.reject[st - 1, t]
            keep[k[~ok]] = False
            k = k[ok]
            if k.size == 0:
                break
        alive[act[~keep]] = False
        acc = norm[keep] + m.bias[st - 1]
        for t in range(m.T):
            acc = acc + m.deltas[st - 1][leaf_idx[keep, t]]
        act_k = act[keep]
        shape[act_k] = affine(Mi[keep], acc)
        act = act_k
        if act.size == 0:
            break
    return alive, score, shape


# ---------------------------------------------------------------- post filter (oriented HoG = OpenCV SIFT descriptor)
def _cvceil(v):
    i = int(np.rint(v))
    return i + (1 if f32(i) < v else 0)


def _cvfloor(v):
    i = int(np.rint(v))
    return i - (1 if v < f32(i) else 0)


def sift_descriptor(img: np.ndarray, kx, ky, size, kangle, atan_lut, _hist_only=False) -> np.ndarray:
    """CalculateOnePointDescriptor<float>: img is the 96x96 window (rows=cols=96)."""
    rows, cols = img.shape
    I = img.astype(np.int64)
    ptx, pty = int(np.rint(f32(kx))), int(np.rint(f32(ky)))   # cvtss2si (round to nearest even)
    angle = f32(360) - f32(kangle)
    if abs(angle - f32(360)) < f32(1.1920929e-07):
        angle = f32(0)
    scl = f32(size) * f32(0.5)
    hw = scl * f32(3.0)
    radius = int(np.rint(f32(f32(f32(hw * f32(1.4142135)) * f32(5.0)) * f32(0.5))))
    radius = min(radius, max(rows, cols), 1024)
    rad = f32(angle * f32(3.1415927)) / f32(180.0)
    cos_t = f32(math.cos(rad)) / hw
    sin_t = f32(math.sin(rad)) / hw
    E = np.exp(np.arange(radius + 1, dtype=f32) ** 2 * f32(f32(-0.125) / f32(hw * hw))).astype(f32)
    bins_per = f32(0.022222223)
    v37 = f32(-angle) * bins_per
    hist = np.zeros(360, f32)
    eps = f32(1.1920929e-07)
    cos_small = abs(cos_t) < eps
    sin_small = abs(sin_t) < eps
    inv_c = f32(3.4028235e38) if cos_small else f32(1) / cos_t
    inv_s = f32(3.4028235e38) if sin_small else f32(1) / sin_t
    c_lo = max(-radius, 1 - ptx); c_hi = min(radius, cols - ptx - 2)
    i0 = max(-radius, 1 - pty); i1 = min(radius, rows - pty - 2)
    v43 = f32(f32(f32(i0) * cos_t) + f32(2.0)) - f32(0.5)
    v44 = f32(f32(2.0) - f32(f32(i0) * sin_t)) - f32(0.5)
    for i in range(i0, i1 + 1):
        fi = f32(i)
        lo, hi = c_lo, c_hi
        ok = True
        is_ = fi * sin_t
        a_ = is_ + f32(2.5); b_ = is_ - f32(2.5)
        if cos_small:
            if f32(a_ * b_) >= f32(-5.9604645e-06):
                ok = False
        else:
            p = b_ * inv_c; q = a_ * inv_c
            mn, mx = min(p, q), max(p, q)
            lo = max(lo, _cvceil(mn) + 1); hi = min(hi, _cvfloor(mx) - 1)
        if ok:
            ic = fi * cos_t
            p_ = f32(-2.5) - ic; q_ = f32(2.5) - ic
            if sin_small:
                if f32(q_ * p_) >= f32(-5.9604645e-06):
                    ok = False
            else:
                p = p_ * inv_s; q = q_ * inv_s
                mn, mx = min(p, q), max(p, q)
                lo = max(lo, _cvceil(mn) + 1); hi = min(hi, _cvfloor(mx) - 1)
        if ok and lo <= hi:
            y = pty + i
            js = np.arange(lo, hi + 1)
            x = ptx + js
            dx = I[y, x + 1] - I[y, x - 1]
            dy = I[y - 1, x] - I[y + 1, x]
            ori = atan_lut[(dy >> 2) + 64, (dx >> 2) + 64].astype(f32) * bins_per + v37
            mag = np.sqrt((dx * dx + dy * dy).astype(f32)).astype(f32)
            w = (mag * E[abs(i)]) * E[np.abs(js)]
            steps = np.arange(js.size, dtype=f32)
            cb = np.empty(js.size, f32); rb = np.empty(js.size, f32)
            c = f32(f32(lo) * cos_t) + v44; r = f32(f32(lo) * sin_t) + v43
            for k in range(js.size):
                cb[k] = c; rb[k] = r
                c = f32(c + cos_t); r = f32(r + sin_t)
            r0 = np.floor(rb).astype(np.int64); c0 = np.floor(cb).astype(np.int64)
            o0 = np.floor(ori).astype(np.int64)
            ofr = (ori - o0.astype(f32)).astype(f32)
            o0 = np.where(o0 < 0, o0 + 8, o0); o0 = np.where(o0 >= 8, o0 - 8, o0)
            rfr = (rb - r0.astype(f32)).astype(f32); cfr = (cb - c0.astype(f32)).astype(f32)
            idx = o0 + 10 * (c0 + 6 * r0 + 7)
            v_r1 = rfr * w; v_r0 = w - v_r1
            v_rc11 = v_r1 * cfr; v_rc10 = v_r1 - v_rc11
            v_rc01 = v_r0 * cfr; v_rc00 = v_r0 - v_rc01
            for k in range(js.size):
                o = ofr[k]; b = idx[k]
                for off, v in ((61, v_rc10[k] * o), (11, v_rc01[k] * o), (60, v_rc10[k] - v_rc10[k] * o),
                               (1, v_rc00[k] * o), (10, v_rc01[k] - v_rc01[k] * o), (0, v_rc00[k] - v_rc00[k] * o),
                               (71, v_rc11[k] * o), (70, v_rc11[k] - v_rc11[k] * o)):
                    hist[b + off] = f32(v + hist[b + off])
        v43 = f32(v43 + cos_t); v44 = f32(v44 - sin_t)
    if _hist_only:
        return hist
    dst = np.empty(128, f32)
    for i in range(4):
        for j in range(4):
            b = ((i + 1) * 6 + (j + 1)) * 10
            hist[b] = f32(hist[b + 8] + hist[b]); hist[b + 1] = f32(hist[b + 1] + hist[b + 9])
            dst[(i * 4 + j) * 8:(i * 4 + j) * 8 + 8] = hist[b:b + 8]
    nrm = f32(0)
    for v in dst:
        nrm = f32(nrm + v * v)
    thr = f32(math.sqrt(nrm)) * f32(0.2)
    dst = np.minimum(dst, thr)
    nrm = f32(0)
    for v in dst:
        nrm = f32(nrm + v * v)
    nrm = max(f32(math.sqrt(nrm)), f32(1.1920929e-07))
    sc = f32(512.0) / nrm
    q = np.rint((sc * dst).astype(f32)).astype(np.int64)   # cvtsd2si
    q = np.where((q < 0) | (q > 255), np.where(q > 0, 255, 0), q)
    return q.astype(f32)


def post_filter(m: Model, window: np.ndarray, shape: np.ndarray):
    """OrientedHogPostFilter::Predict. Returns (score, passed)."""
    M = similarity(shape[None, :5], m.tmpl5)[0]
    a, b = M[0], M[1]
    scale = f32(f32(f32(1.0) / f32(math.sqrt(f32(a * a + b * b)))) / f32(100.0)) * f32(3.5)
    ang = f32(f32(math.atan2(b, a)) / f32(3.1415927)) * f32(180.0)
    while ang < 0:
        ang = f32(ang + f32(360.0))
    while ang > 360:
        ang = f32(ang - f32(360.0))
    s = f32(0)
    w = m.hog_w.reshape(m.L, 128)
    for p in range(m.L):
        d = sift_descriptor(window, shape[p, 0], shape[p, 1], scale, ang, m.atan)
        for k in range(128):
            s = f32(s + f32(w[p, k] * d[k]))
    return float(s), bool(m.hog_thr <= s)


# ---------------------------------------------------------------- landmarks -> rectangle (DetectInLayer tail)
def window_to_image(layer: Layer, x: int, y: int, shape: np.ndarray) -> np.ndarray:
    out = np.empty_like(shape)
    out[:, 0] = (f32(x) + shape[:, 0] - f32(WIN) * f32(0.25)) * f32(layer.sx) + f32(layer.ox)
    out[:, 1] = (f32(y) + shape[:, 1] - f32(WIN) * f32(0.25)) * f32(layer.sy) + f32(layer.oy)
    return out


def landmarks_to_rect(lm: np.ndarray) -> tuple:
    L = lm.shape[0]
    pts = lm.reshape(-1)[: 2 * ((L + 1) // 2) + 0]  # the DLL loops m < L over x,y pairs -> first ceil(L/2) points
    xs = pts[0::2]; ys = pts[1::2]
    size = max((xs.max() - xs.min()) * BOX_FACTOR, (ys.max() - ys.min()) * BOX_FACTOR)
    side = int(f32(size + f32(0.5)))
    cx = (lm[0, 0] + lm[1, 0] + lm[3, 0] + lm[4, 0]) * f32(0.25)
    cy = (lm[0, 1] + lm[1, 1] + lm[3, 1] + lm[4, 1]) * f32(0.25)
    left = max(int(f32(cx - size * f32(0.5)) + f32(0.5)), 0)
    top = max(int(f32(cy - size * f32(0.5)) + f32(0.5)), 0)
    return (left, top, left + side, top + side)


# ---------------------------------------------------------------- merge (MergeDetectionResult)
def merge(dets: list[Detection], L: int) -> list[Detection]:
    if not dets:
        return []
    infos = []
    for d in sorted(dets, key=lambda d: (d.box, ), reverse=False):
        l, t, r, b = d.box
        x, y, w, h = f32(l), f32(t), f32(r - l), f32(b - t)
        s = (h / f32(WIN) + w / f32(WIN)) * f32(0.5)
        infos.append([x, y, w, h, s, f32(d.score), w * f32(0.5) + x, h * f32(0.5) + y, d.landmarks.copy(), d])
    out = []
    while infos:
        mode = max(infos, key=lambda q: q[5])
        x, y, w, h, s, sc, cx, cy = mode[:8]
        for _ in range(5):
            sw = sx_ = sy_ = ss = ssc = f32(0)
            for q in infos:
                k = max(f32(0), f32(1) - (((cx - q[6]) ** 2 + (cy - q[7]) ** 2) * (f32(1) / (w * f32(0.25) * w)) + (s - q[4]) ** 2 * f32(0.25)))
                sx_ += q[6] * k; sy_ += q[7] * k; ss += q[4] * k; ssc += k * q[5]; sw += k
            ncx, ncy = sx_ / sw, sy_ / sw
            conv = ((cx - ncx) ** 2 + (cy - ncy) ** 2) / w < f32(0.001)
            cx, cy = ncx, ncy
            s = ss / sw; sc = ssc / sw
            w = f32(WIN) * s; h = f32(WIN) * s
            x = cx - w * f32(0.5); y = cy - h * f32(0.5)
            if conv:
                break
        members = [mode]
        infos.remove(mode)
        rest = []
        for q in infos:
            ix0 = max(x, q[0]); ix1 = min(w + x, q[2] + q[0]); iy0 = max(y, q[1]); iy1 = min(h + y, q[3] + q[1])
            if ix0 < ix1 and iy0 < iy1 and (iy1 - iy0) * (ix1 - ix0) / min(h * w, q[3] * q[2]) > f32(0.4):
                members.append(q)
            else:
                rest.append(q)
        infos = rest
        members = sorted(members, key=lambda q: -q[5])[:6]
        lms = np.median(np.stack([q[8] for q in members]), axis=0).astype(f32)
        box = (int(f32(x + f32(0.5))), int(f32(y + f32(0.5))), int(f32(x + w + f32(0.5))), int(f32(y + h + f32(0.5))))
        out.append(Detection(box=box, score=float(sc), jda_score=max(q[9].jda_score for q in members), landmarks=lms))
    return out


# ---------------------------------------------------------------- top level
def detect(m: Model, gray: np.ndarray, min_face: int = 48, max_face: int | None = None,
           dense: bool = False, merge_results: bool = True, return_raw: bool = False):
    """Full-frame detection (JointCascadeFaceDetector::Detect -> DetectTopDown -> DetectInLayer per layer).
    dense=False reproduces DetectInLayer's adaptive horizontal step (16 px, re-scan at 8 px around windows that pass
    >= 8 ferns); dense=True scans every 8 px."""
    gray = np.ascontiguousarray(gray, np.uint8)
    H, W = gray.shape
    max_face = min(max_face or 2584, H, W)
    layers = build_pyramid(gray)
    raw: list[Detection] = []
    for li, ly in enumerate(layers):
        if li + 2 * LAYERS_PER_OCTAVE >= len(layers):
            break
        face = int(f32(FACE_IN_WINDOW) * f32(ly.sx) + f32(0.5))
        if face < min_face or int(f32(FACE_IN_WINDOW) * f32(ly.sy) + f32(0.5)) < min_face:
            continue
        if face > max_face:
            break
        Hp, Wp = ly.img.shape
        if Wp < WIN or Hp < WIN:
            break
        xs_all = np.arange(0, Wp - WIN + 1, STEP); ys_all = np.arange(0, Hp - WIN + 1, STEP)
        half = layers[li + LAYERS_PER_OCTAVE].img; quarter = layers[li + 2 * LAYERS_PER_OCTAVE].img
        gx, gy = np.meshgrid(xs_all, ys_all)
        passed, npass = fern_cascade(m, half, gx.ravel(), gy.ravel())
        passed = passed.reshape(gx.shape); npass = npass.reshape(gx.shape)
        # window visiting order (DetectInLayer inner loop), possibly with repeats
        visits = []
        for r in range(len(ys_all)):
            if dense:
                visits += [(r, c) for c in range(len(xs_all))]
                continue
            x = 0; v45 = 0
            while x <= Wp - WIN:
                c = x // STEP
                visits.append((r, c))
                nx = x; v189 = v45
                if npass[r, c] >= m.fern_min_passed and v45 <= 0:
                    v189 = 2 * 1 + 2
                    nx = max(x - STEP * (1 + 1), 0)
                step = (1 + 1) if v189 <= 0 else 1
                x = nx + STEP * step
                v45 = v189 - 1
        cand = sorted({rc for rc in visits if passed[rc]})
        if not cand:
            continue
        cy_ = np.array([ys_all[r] for r, _ in cand]); cx_ = np.array([xs_all[c] for _, c in cand])
        alive, score, shape = jda_predict(m, ly.img, half, quarter, cx_, cy_)
        mult = {rc: visits.count(rc) for rc in cand} if not dense else None
        for k in np.nonzero(alive)[0]:
            x, y = int(cx_[k]), int(cy_[k])
            win = ly.img[y:y + WIN, x:x + WIN]
            hs, ok = post_filter(m, win, shape[k])
            if not ok:
                continue
            lm = window_to_image(ly, x, y, shape[k])
            d = Detection(box=landmarks_to_rect(lm), score=hs, jda_score=float(score[k]) / m.score_scale,
                          landmarks=lm, layer=li, window=(x, y))
            for _ in range(mult[cand[k]] if mult else 1):
                raw.append(d)
    res = merge(raw, m.L) if merge_results else raw
    return (res, raw) if return_raw else res
