"""FaceDetectorResources MODEL_FILES_110..113: XOR-obfuscated ORT RetinaFace/img2pose-style IR face detectors.

Pre/post-processing mirrors FaceProcessorCore.dll (build 26300, public PDB):
  RetinaEngine::Initialize            0x18001fd30  (resource id, strides, anchors, thresholds, pose mean/std)
  RetinaEngine::ProcessNextFrame      0x1800041d0  (input tensor = raw 8-bit pixels as float, 1x1xHxW)
  RetinaEngine::GetNumPriorBoxes      0x180006708
  RetinaEngine::ProcessResultCandidates 0x180005c3c (anchor decode, threshold, greedy non-overlap selection)
  RetinaEngine::DecodeRotationVec     0x18001a8a8  (6-DoF -> yaw/pitch/roll)
  FaceProcessorUtils::HistogramEqualization 0x18000b31c (applied before the helper detector in FaceDetectorWithHelper)
Decryption: HelloFaceCipher (see ../cipher.py).
"""
from __future__ import annotations

import math
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

f32 = np.float32

# RetinaEngine::Initialize: type 0 -> resource 110 (112 with feature IntegrateComplianceHelloModels),
# type 1 -> 111 (113), and for type 1 strides and anchor sizes are doubled.
CONFIG = {
    0: dict(strides=(16, 32), anchors=((32, 64), (128, 256))),
    1: dict(strides=(32, 64), anchors=((64, 128), (256, 512))),
}
RESOURCE_TYPE = {110: 0, 112: 0, 111: 1, 113: 1}
CONF_THRESHOLD = f32(0.925)                    # this+52 = 0x3F6CCCCD
VARIANCES = (f32(0.1), f32(0.2))                # this+216
POSE_MEAN = np.array([-0.09145801, 0.01599428, -0.02233053, 0.02951323, 0.23168705, 3.5088696], f32)  # this+240
POSE_STD = np.array([0.29370177, 0.5890974, 0.26571402, 0.62606615, 0.18116798, 0.40033603], f32)      # this+264


def histogram_equalize(gray: np.ndarray) -> np.ndarray:
    """FaceProcessorUtils::HistogramEqualization: lut[v] = trunc(cdf[v] * 255/N + 0.5) (cdf split hi*256+lo in float)."""
    h, w = gray.shape
    cdf = np.cumsum(np.bincount(gray.ravel(), minlength=256)).astype(np.uint32)
    scale = f32(255.0) / f32(h * w)
    lut = ((((cdf >> 8).astype(f32) * f32(256.0)) + (cdf & 0xFF).astype(f32)) * scale + f32(0.5)).astype(np.int32)
    return lut.astype(np.uint8)[gray]


@dataclass
class Face:
    box: tuple                 # (left, top, right, bottom) ints
    score: float
    landmarks: np.ndarray      # (5, 2): two eyes, nose, two mouth corners (SCRFD/RetinaFace order, INFERRED)
    dofs: np.ndarray | None = None   # de-normalised 6-DoF (rotation vector, translation) in the anchor frame
    rotation_xyz: tuple | None = None   # DecodeRotationVec outputs (deg)
    prior: int = -1


def num_priors(cfg, h, w):
    return sum(2 * math.ceil(h / s) * math.ceil(w / s) for s in cfg["strides"])


def _quat_rotmat(rv):
    th = math.sqrt(float(np.dot(rv, rv)))
    k = math.sin(th * 0.5) / th if th > 0.001 else 0.5 - th * th / 48.0 + th ** 4 / 3840.0
    x, y, z = (k * rv).tolist()
    w = math.cos(th * 0.5)
    return np.array([[1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
                     [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
                     [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)]])


def decode_rotation(rv, k_box, k_img):
    """DecodeRotationVec: M = K_img^-1 . K_box . R(rv) (img2pose-style bbox->full-image pose transfer).
    Returns the DLL's three outputs in order, in degrees: (about x, about y, about z) =
    (atan2(M12, M22), asin(M02), -atan2(M01, M00)). Fitted to and verified against the function under emulation
    (max error < 1e-4 deg on 200 random inputs); the gimbal-lock branch (|M02| == 1) is not reproduced."""
    M = np.linalg.inv(k_img) @ k_box @ _quat_rotmat(np.asarray(rv, np.float64))
    return (math.degrees(math.atan2(M[1, 2], M[2, 2])), math.degrees(math.asin(max(-1.0, min(1.0, M[0, 2])))),
            -math.degrees(math.atan2(M[0, 1], M[0, 0])))


class RetinaDetector:
    def __init__(self, model: str | Path, resource_id: int | None = None):
        import onnxruntime as ort
        self.sess = ort.InferenceSession(str(model), providers=["CPUExecutionProvider"])
        rid = resource_id if resource_id is not None else int(Path(model).stem.split("_")[-1])
        self.cfg = CONFIG[RESOURCE_TYPE[rid]]
        self.outputs = [o.name for o in self.sess.get_outputs()]

    def raw(self, gray: np.ndarray) -> dict:
        x = gray.astype(f32)[None, None]
        return dict(zip(self.outputs, self.sess.run(None, {"input_0": x})))

    def detect(self, gray: np.ndarray, max_faces: int = 10, threshold: float | None = None, with_pose: bool = True):
        H, W = gray.shape
        out = self.raw(gray)
        conf = out["confidence"][0]; bb = out["bbox"][0]; lm = out["landmarks"][0]
        dofs = out.get("dofs")
        assert conf.shape[0] == num_priors(self.cfg, H, W)
        thr = CONF_THRESHOLD if threshold is None else f32(threshold)
        v0, v1 = VARIANCES
        cands = []
        off = 0
        for lvl, s in enumerate(self.cfg["strides"]):
            gh, gw = math.ceil(H / s), math.ceil(W / s)
            for j in range(gh):
                for k in range(gw):
                    ax = f32(f32(k + 0.5) * f32(s)); ay = f32(f32(j + 0.5) * f32(s))
                    for m in range(2):
                        p = off + m + 2 * (k + gw * j)
                        sc = conf[p, 1]
                        if not thr < sc:
                            continue
                        size = f32(self.cfg["anchors"][lvl][m])
                        cx = f32(v0 * bb[p, 0]) * size + ax; cy = f32(v0 * bb[p, 1]) * size + ay
                        w = f32(math.exp(f32(v1 * bb[p, 2]))) * size; h = f32(math.exp(f32(v1 * bb[p, 3]))) * size
                        if not (w >= 2 and h >= 2):
                            continue
                        box = (int(cx - w * f32(0.5)), int(cy - h * f32(0.5)), int(w * f32(0.5) + cx), int(h * f32(0.5) + cy))
                        pts = np.stack([f32(v0 * lm[p, 0::2]) * size + ax, f32(v0 * lm[p, 1::2]) * size + ay], 1).astype(f32)
                        face = Face(box, float(sc), pts, prior=p)
                        if dofs is not None:
                            d = dofs[0, p] * POSE_STD + POSE_MEAN
                            face.dofs = d.astype(f32)
                            if with_pose:
                                fsum = float(W + H)
                                kb = np.array([[fsum, 0, ax], [0, fsum, ay], [0, 0, 1]])
                                ki = np.array([[fsum, 0, W * 0.5], [0, fsum, H * 0.5], [0, 0, 1]])
                                face.rotation_xyz = decode_rotation(d[:3], kb, ki)
                        cands.append(face)
            off += 2 * gh * gw
        cands.sort(key=lambda f: -f.score)
        kept: list[Face] = []
        for f in cands:  # any_of(lambda_671ddd...): drop if it intersects an accepted box at all
            if len(kept) >= max_faces:
                break
            if any(f.box[0] < k.box[2] and f.box[2] > k.box[0] and f.box[1] < k.box[3] and f.box[3] > k.box[1] for k in kept):
                continue
            kept.append(f)
        # ShouldRejectCandidate (partly): all five landmarks must lie inside the frame
        kept = [f for f in kept if np.all((f.landmarks[:, 0] >= 0) & (f.landmarks[:, 0] < W) & (f.landmarks[:, 1] >= 0) & (f.landmarks[:, 1] < H))]
        return kept
