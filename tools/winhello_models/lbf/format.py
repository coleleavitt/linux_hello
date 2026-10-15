"""Parsers for the Windows Hello legacy IR face-alignment resources.

facerecognitionsensoradapterresources.dll, resource type MODEL_FILES:
  102  LbfAligner database      (FaceProcessorCore LbfAligner::Aligner::LoadDatabase 0x180017d80)
  104  PITCH_KNN, 105 PITCH_LDA (IrFaceOrientation, LDARegressor::LoadKNN 0x180018810 / LoadLDA 0x180018ad8)
  106  YAW_KNN,   107 YAW_LDA

All integers and floats are little-endian. See docs/winhello-models/lbf.md for the byte layout.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass, field

import numpy as np

LBF_MAGIC = 0xFACE
LBF_VERSION = 10
LDA_FEATURE_DIM = 7139  # 11 x 11 cells x 59 uniform-LBP bins; read as 28556 bytes

NODE_DTYPE = np.dtype([("x1", "<f4"), ("y1", "<f4"), ("x2", "<f4"), ("y2", "<f4"),
                       ("thr", "<f4"), ("children", "<u4")])


class _Reader:
    def __init__(self, data: bytes):
        self.b = data
        self.o = 0

    def take(self, n: int) -> bytes:
        if self.o + n > len(self.b):
            raise ValueError(f"truncated at offset {self.o} (+{n})")
        v = self.b[self.o:self.o + n]
        self.o += n
        return v

    def u8(self) -> int:
        return self.take(1)[0]

    def u32(self) -> int:
        return struct.unpack("<I", self.take(4))[0]

    def i32(self) -> int:
        return struct.unpack("<i", self.take(4))[0]

    def f32(self) -> float:
        return struct.unpack("<f", self.take(4))[0]

    def array(self, dtype, count: int) -> np.ndarray:
        dt = np.dtype(dtype)
        return np.frombuffer(self.take(dt.itemsize * count), dt).copy()


@dataclass
class Regressor:
    """Linear leaf regressor. Row r of `weights` is added for every reached leaf r; the last row is the bias."""
    compressed: bool
    rows: int
    cols: int
    scale: float | None        # only for compressed (int16) weights
    raw: np.ndarray            # int16 (compressed) or float32, shape (rows, cols)

    @property
    def weights(self) -> np.ndarray:
        if self.compressed:
            return self.raw.astype(np.float32) * np.float32(self.scale)
        return self.raw


@dataclass
class Stage:
    num_trees: int
    trees_per_point: int
    num_points: int
    trees: list[np.ndarray]    # one NODE_DTYPE array per tree
    regressor: Regressor


@dataclass
class LbfModel:
    header: bytes
    num_points: int
    mean_shape: np.ndarray     # (num_points, 2) float32, in the unit face box
    stages: list[Stage]
    classifier: Regressor | None   # 1-column regressor evaluated on the last stage's trees
    unit_box: np.ndarray = field(default_factory=lambda: np.array(
        [[0, 0], [1, 0], [0, 1], [1, 1]], np.float32))  # Aligner+96, set in LoadDatabase


def _read_regressor(r: _Reader) -> Regressor:
    # Regressor::Init 0x180017bc8
    compressed = r.u8() != 0
    rows = r.u32()
    cols = r.u32()
    if compressed:
        scale = r.f32()
        raw = r.array("<i2", rows * cols).reshape(rows, cols)
    else:
        scale = None
        raw = r.array("<f4", rows * cols).reshape(rows, cols)
    return Regressor(compressed, rows, cols, scale, raw)


def parse_lbf(data: bytes) -> LbfModel:
    """LbfAligner::Aligner::LoadDatabase (0x180017d80)."""
    r = _Reader(data)
    header = r.take(10)
    magic, version = struct.unpack_from("<HB", header)
    if magic != LBF_MAGIC or version != LBF_VERSION:
        raise ValueError(f"not an LbfAligner database (magic {magic:#x}, version {version})")
    num_points = r.u32()
    num_stages = r.u32()
    mean = r.array("<f4", 2 * num_points).reshape(num_points, 2)
    stages = []
    for _ in range(num_stages):
        # Stage::Init 0x180018c30
        num_trees = r.u32()
        per_point = r.u32()
        if per_point == 0:
            raise ValueError("trees_per_point == 0")
        trees = []
        for _ in range(num_trees):
            # Tree::Init 0x180018d84
            n = r.i32()
            trees.append(r.array(NODE_DTYPE, n))
        stages.append(Stage(num_trees, per_point, num_trees // per_point, trees, _read_regressor(r)))
    classifier = None
    if r.o < len(data):
        flag = r.u32()
        if flag != 1:
            raise ValueError(f"unexpected classifier flag {flag}")
        classifier = _read_regressor(r)
    if r.o != len(data):
        raise ValueError(f"{len(data) - r.o} trailing bytes")
    return LbfModel(header, num_points, mean, stages, classifier)


@dataclass
class LdaRegressor:
    """One head-pose angle regressor (CreateRegressor 0x18001e280 = LoadLDA + LoadKNN)."""
    mean: np.ndarray           # (7139,) float32 feature mean
    projection: np.ndarray     # (D, 7139) float32 LDA basis
    knn_k: int                 # KNN +40: 1 -> nearest centroid
    knn_gamma: int             # KNN +44: weight exp(-gamma * distance) when a prior angle exists
    labels: np.ndarray         # (N,) float32 angle per centroid (degrees)
    centroids: np.ndarray      # (N, D) float32 in LDA space


def parse_lda(data: bytes) -> tuple[np.ndarray, np.ndarray]:
    """LDARegressor::LoadLDA (0x180018ad8): u32 D, float[7139] mean, D x float[7139] basis."""
    r = _Reader(data)
    d = r.u32()
    mean = r.array("<f4", LDA_FEATURE_DIM)
    proj = r.array("<f4", LDA_FEATURE_DIM * d).reshape(d, LDA_FEATURE_DIM)
    if r.o != len(data):
        raise ValueError(f"{len(data) - r.o} trailing bytes")
    return mean, proj


def parse_knn(data: bytes) -> tuple[int, int, np.ndarray, np.ndarray]:
    """LDARegressor::LoadKNN (0x180018810): u32 k, u32 gamma, u32 N, u32 M, float[N] labels, float[N*M] centroids."""
    r = _Reader(data)
    k, gamma, n, m = r.u32(), r.u32(), r.u32(), r.u32()
    labels = r.array("<f4", n)
    centroids = r.array("<f4", n * m).reshape(n, m)
    if r.o != len(data):
        raise ValueError(f"{len(data) - r.o} trailing bytes")
    return k, gamma, labels, centroids


def load_lda_regressor(lda: bytes, knn: bytes) -> LdaRegressor:
    mean, proj = parse_lda(lda)
    k, gamma, labels, centroids = parse_knn(knn)
    if centroids.shape[1] != proj.shape[0]:   # checked in CreateRegressor
        raise ValueError("KNN dimension != LDA dimension")
    return LdaRegressor(mean, proj, k, gamma, labels, centroids)
