"""Numpy re-implementation of FaceProcessorCore's IrFaceOrientation (legacy IR head pose).

Pipeline (IrFaceOrientation::ComputeOrientation 0x18000cfe0):
  27 aligner landmarks -> similarity to a fixed 64x64 template (CalculateSimilarityTransform 0x18000d4c8)
  -> 64x64 bilinear warp, zero padding (ApplySimilarityTransformToImage 0x18000c010) -> x/255
  -> DoG = G(sigma 2) - G(sigma 4) (0x18000d6dc) -> 8-neighbour uniform LBP (EncodeLBPFast 0x18000ca00)
  -> 11x11 overlapping 10x10-cell histograms x 59 bins = 7139 features x 0.01 (0x18000ce90)
  -> per angle: LDA projection + nearest class centroid (LDARegressor::compute 0x18000ba00).
"""
from __future__ import annotations

import math

import numpy as np

from .format import LdaRegressor

F = np.float32

# IrFaceOrientation::Initialize (0x180022754): rdata xmmwords + two immediates; 64x64 template coords.
TEMPLATE_27 = np.array([
    [21.661, 20.715], [42.995, 20.614], [32.072, 32.190], [23.588, 42.566], [42.102, 42.218],
    [13.550, 16.565], [26.701, 15.975], [17.949, 21.326], [21.473, 19.455], [21.588, 22.170],
    [25.673, 21.481], [38.041, 16.014], [51.482, 16.429], [39.577, 21.385], [43.209, 19.343],
    [43.202, 21.957], [46.940, 21.119], [29.572, 21.648], [35.385, 21.644], [27.963, 29.334],
    [36.591, 29.083], [25.790, 33.699], [38.957, 33.061], [32.344, 40.653], [32.416, 42.882],
    [32.316, 43.290], [32.499, 45.906]], F)

# GaussianFilteringSigma2 / Sigma4 kernels (rdata 0x18004b1a0, 0x18004b1e0)
K_SIGMA2 = np.array([0.0022182, 0.0087731, 0.0270232, 0.0648252, 0.1211094, 0.1762131, 0.1996756,
                     0.1762131, 0.1211094, 0.0648252, 0.0270232, 0.0087731, 0.0022182], F)
K_SIGMA4 = np.array([0.0011099, 0.0022773, 0.0043897, 0.0079487, 0.0135211, 0.0216067, 0.0324355,
                     0.0457414, 0.0605975, 0.0754148, 0.0881688, 0.0968345, 0.0999084, 0.0968345,
                     0.0881688, 0.0754148, 0.0605975, 0.0457414, 0.0324355, 0.0216067, 0.0135211,
                     0.0079487, 0.0043897, 0.0022773, 0.0011099], F)
INV255 = np.frombuffer(bytes.fromhex("8180803b"), "<f4")[0]  # 0x3b808081
SIZE = 64
CELLS = 11       # LbpConfig {10,10,11,11,64,64,1,1,1}: cell w/h, cells x/y, image w/h, margin x/y, radius
CELL = 10
STEP = SIZE // CELLS   # 5
MARGIN = 1
RADIUS = 1
PRIOR_WINDOW_100NS = 0x325AA0   # 330 ms: previous angle used as prior if newer than this
YAW_LIMIT = 20.0
PITCH_LIMIT = 25.0


def _uniform_lbp_lut() -> np.ndarray:
    """LBP::lbpLookUpTable (0x18004b0a0) == standard 59-bin uniform (u2) mapping; verified byte-for-byte."""
    lut = np.full(256, 58, np.uint8)
    nxt = 0
    for c in range(256):
        bits = [(c >> i) & 1 for i in range(8)]
        if sum(bits[i] != bits[(i + 1) % 8] for i in range(8)) <= 2:
            lut[c] = nxt
            nxt += 1
    return lut


LBP_LUT = _uniform_lbp_lut()


def similarity_to_template(points: np.ndarray, template: np.ndarray = TEMPLATE_27):
    """CalculateSimilarityTransform (0x18000d4c8): (a, b, tx, ty) with template ~ [a b; -b a] p + t."""
    n = min(len(points), len(template))
    sx = sy = tx_ = ty_ = sxx = syy = xs = sxty = txsy = syty = F(0)
    for i in range(n):
        x, y = F(points[i, 0]), F(points[i, 1])
        u, v = F(template[i, 0]), F(template[i, 1])
        sx = F(sx + x); sy = F(sy + y)
        ty_ = F(ty_ + v); tx_ = F(tx_ + u)
        sxx = F(sxx + F(x * x)); syy = F(syy + F(y * y))
        xs = F(xs + F(u * x)); sxty = F(sxty + F(x * v))
        txsy = F(txsy + F(u * y)); syty = F(syty + F(y * v))
    s2 = F(syy + sxx)
    nf = F(n)
    den = F(F(F(sx * sx) + F(sy * sy)) - F(nf * s2))
    if abs(den) < 1e-6:
        raise ValueError("degenerate landmarks")
    dot = F(syty + xs)
    cross = F(txsy - sxty)
    a = F(F(F(F(tx_ * sx) + F(ty_ * sy)) - F(dot * nf)) / den)
    b = F(F(F(F(tx_ * sy) - F(ty_ * sx)) - F(cross * nf)) / den)
    tx = F(F(F(F(cross * sy) + F(dot * sx)) - F(s2 * tx_)) / den)
    ty = F(F(F(F(dot * sy) - F(cross * sx)) - F(s2 * ty_)) / den)
    return a, b, tx, ty


def warp_face(img: np.ndarray, coeff) -> np.ndarray:
    """ApplySimilarityTransformToImage<LuminanceImage> (0x18000c010) + BilinearInterpolationWithPadding (0x18000c6b0)."""
    a, b, tx, ty = (F(v) for v in coeff)
    n2 = F(F(a * a) + F(b * b))
    c13 = F(a / n2)
    c14 = F(-b / n2)
    c15 = F(F(F(-tx * b) - F(ty * a)) / n2)
    c16 = F(F(F(ty * b) - F(tx * a)) / n2)
    col = np.arange(SIZE, dtype=F)[None, :]
    row = np.arange(SIZE, dtype=F)[:, None]
    x = ((col * c13) + (row * c14)) + c16
    y = ((row * c13) - (col * c14)) + c15
    h, w = img.shape
    x0 = np.floor(x); y0 = np.floor(y)
    fx = (x - x0).astype(F); fy = (y - y0).astype(F)
    x0 = x0.astype(np.int64); y0 = y0.astype(np.int64)

    def px(cx, cy):
        ok = (cx >= 0) & (cx <= w - 1) & (cy >= 0) & (cy <= h - 1)
        v = np.zeros(cx.shape, F)
        v[ok] = img[cy[ok], cx[ok]]
        return v

    p00, p10 = px(x0, y0), px(x0 + 1, y0)
    p01, p11 = px(x0, y0 + 1), px(x0 + 1, y0 + 1)
    top = (p10 - p00) * fx + p00
    bot = (p11 - p01) * fx + p01
    return ((bot - top) * fy + top).astype(F)


def _conv_sym(a: np.ndarray, k: np.ndarray, axis: int) -> np.ndarray:
    r = len(k) // 2
    a = np.moveaxis(a, axis, -1)
    p = np.pad(a, [(0, 0), (r, r)], mode="symmetric")
    out = np.zeros_like(a)
    for i in range(len(k)):
        out = out + k[i] * p[:, i:i + a.shape[1]]
    return np.moveaxis(out, -1, axis)


def gaussian_ms(x: np.ndarray, k: np.ndarray) -> np.ndarray:
    """GaussianFilteringSigma2/4 as executed: separable, mirror padding that repeats the edge sample,
    BUT the horizontal pass only produces columns 4g and 4g+1; columns 4g+2/4g+3 receive copies of them
    (observed by running the x64 code under emulation; matches to 1.8e-7)."""
    h = _conv_sym(x, k, 1)
    h = h[:, np.arange(x.shape[1]) & ~2]
    return _conv_sym(h, k, 0)


def lbp_codes(d: np.ndarray) -> np.ndarray:
    """EncodeLBPFast (0x18000ca00): bit set when neighbour > centre; bit order TL,T,TR,R,BR,B,BL,L."""
    out = np.zeros(d.shape, np.uint8)
    r = RADIUS
    lo, hi = MARGIN + r, SIZE - MARGIN - r
    c = d[lo:hi, lo:hi]
    nb = [(-r, -r), (-r, 0), (-r, r), (0, r), (r, r), (r, 0), (r, -r), (0, -r)]
    code = np.zeros(c.shape, np.int32)
    for bit, (dy, dx) in enumerate(nb):
        code |= (c < d[lo + dy:hi + dy, lo + dx:hi + dx]).astype(np.int32) << bit
    out[lo:hi, lo:hi] = LBP_LUT[code]
    return out


def lbp_feature(codes: np.ndarray) -> np.ndarray:
    """ComputeOrientationLBPFeature (0x18000ce90) and the x0.01 conversion in ComputeOrientation."""
    feat = np.zeros((CELLS, CELLS, 59), np.int32)
    o = MARGIN + RADIUS
    for cy in range(CELLS):
        for cx in range(CELLS):
            blk = codes[o + STEP * cy:o + STEP * cy + CELL, o + STEP * cx:o + STEP * cx + CELL]
            feat[cy, cx] = np.bincount(blk.reshape(-1), minlength=59)
    return feat.reshape(-1).astype(F) * F(0.01)


def lda_angle(reg: LdaRegressor, feat: np.ndarray, prior: float | None = None) -> float:
    """LDARegressor::compute -> Project (0x18000bb20) -> Regress (0x18000b498 / 0x18000bdc0)."""
    proj = (reg.projection.astype(np.float64) @ (feat - reg.mean).astype(np.float64)).astype(F)
    dist = np.sqrt(((reg.centroids - proj[None, :]) ** 2).sum(1, dtype=F)).astype(F)
    if prior is None:
        if reg.knn_k != 1:
            raise NotImplementedError("k > 1 KNN path not used by the shipped models")
        return float(reg.labels[int(np.argmin(dist))])
    above = np.nonzero(reg.labels > F(prior))[0]
    j = int(above[0]) if len(above) else len(reg.labels) - 1
    lo, hi = max(j - 10, 0), min(j + 10, len(reg.labels) - 1)
    w = np.exp(F(-reg.knn_gamma) * dist[lo:hi + 1]).astype(F)
    return float((w * reg.labels[lo:hi + 1]).sum(dtype=F) / w.sum(dtype=F))


def _quat_w_angle(a: float, b: float) -> float:
    """XMQuaternionRotationMatrix (0x18000c87c, normalising variant) of rows (a,-b,0),(b,a,0),(0,0,1),
    then 2*XMScalarACos(w) in degrees. Stored where we call it 'roll': an unsigned in-plane angle that
    is also shrunk by the image->template scale (a = s*cos, b = s*sin), so it is not a true roll angle."""
    if 2.0 * a > 0:
        w = (1.0 + a) / math.sqrt(b * b + (1.0 + a) ** 2)
    else:
        w = -b / math.sqrt((1.0 - a) ** 2 + b * b)
    return float(np.float32(2.0 * math.acos(max(-1.0, min(1.0, w)))) * np.float32(57.2957764))


def _inside(quad, p) -> bool:
    for i in range(4):
        e0 = quad[i] - p
        e1 = quad[(i + 1) & 3] - p
        if e1[1] * e0[0] - e1[0] * e0[1] < 0:   # same sign convention as the SSE code
            return False
    return True


def frontal_from_points(points: np.ndarray, flags: list[bool]):
    """IrFaceOrientation::EstimateFaceFrontalFromAlignmentPoints (0x18000b640).

    Quad = [p0 L-eye, p1 R-eye, p4 R-mouth, p3 L-mouth]; nose p2 must stay inside the quad shrunk toward
    the opposite corners by t (bisection, 5 steps from t=0.25 within [0, 0.5]). Margin t < 0.125 sets the
    pose flag of the closest edge (eyes -> flag2, right side -> flag1, mouth -> flag3, left side -> flag0).
    Only runs when the LDA angles set no flag.
    """
    p = points.astype(F)
    if any(flags):
        return flags, 0.0
    base = np.stack([p[0], p[1], p[4], p[3]])
    nose = p[2]
    t = F(0.0)
    if _inside(base, nose):
        hi, lo, t = F(0.5), F(0.0), F(0.25)
        for _ in range(5):
            quad = np.stack([t * base[(i - 2) & 3] + (F(1) - t) * base[i] for i in range(4)])
            if _inside(quad, nose):
                lo = t
            else:
                hi = t
            t = F((lo + hi) * F(0.5))
        # the edge search below uses the last tested quad, as the x64 code does
    else:
        quad = base
    flags = list(flags)
    if t < 0.125:
        best, bi = np.inf, 0
        for i in range(4):
            e = quad[(i + 1) & 3] - quad[i]
            v = nose - quad[i]
            d = np.linalg.norm(v - (v @ e) / (e @ e) * e)
            if d < best:
                best, bi = d, i
        flags[{0: 2, 1: 1, 2: 3, 3: 0}[bi]] = True
    return flags, float(t)


def head_pose(img: np.ndarray, points: np.ndarray, yaw: LdaRegressor, pitch: LdaRegressor,
              prior_yaw: float | None = None, prior_pitch: float | None = None, debug: bool = False):
    """Returns dict(pitch, yaw, roll, flags=[yaw>20, yaw<-20, pitch>25, pitch<-25], frontal, frontal_margin)."""
    img = np.ascontiguousarray(img, np.uint8)
    coeff = similarity_to_template(points)
    face = warp_face(img, coeff) * INV255
    dog = gaussian_ms(face, K_SIGMA2) - gaussian_ms(face, K_SIGMA4)
    codes = lbp_codes(dog)
    feat = lbp_feature(codes)
    p = lda_angle(pitch, feat, prior_pitch)
    y = lda_angle(yaw, feat, prior_yaw)
    flags = [y > YAW_LIMIT, y < -YAW_LIMIT, p > PITCH_LIMIT, p < -PITCH_LIMIT]
    flags, margin = frontal_from_points(points, flags)
    out = dict(pitch=p, yaw=y, roll=_quat_w_angle(float(coeff[0]), float(coeff[1])), flags=flags,
               frontal=not any(flags), frontal_margin=margin)
    if debug:
        out.update(face=face, dog=dog, codes=codes, feature=feat, coeff=coeff)
    return out
