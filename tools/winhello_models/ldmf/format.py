"""LDMF ("FaceSDK Team") joint-cascade face detector file parser.

Byte layout derived from FaceTrackerInternal.dll (public PDB names):
  JointCascadeModel::Read            0x180001bd8  (360-byte header, dispatch on model id)
  JointCascadeDetectionModel::Read   0x180002450  (cascade body)
  OrientedHogPostFilter::Read        0x180001974  (post-filter body)
  JointCascadeFaceDetector::AssignCascadeModel 0x18009c170 (model type tags)
All integers little-endian. See docs/winhello-models/ldmf.md for the tables.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

MAGIC = b"LDMF"
TYPE_INFRARED = 0xFACE0C06  # AssignCascadeModel: 248 trees/stage, ref shape @0x180134440
TYPE_LOWEND = 0xFACE4DB5    # AssignCascadeModel:  81 trees/stage, ref shape @0x180134FF0

NODE_DTYPE = np.dtype([
    ("leaf", "<i4"),     # -1 = split node, else leaf index within the stage
    ("scale", "<i4"),    # 0 full, 1 half, 2 quarter resolution image
    ("a_idx", "<i4"),    # float index (2*landmark) of point A into the shape vector
    ("a_dx", "<f4"), ("a_dy", "<f4"),
    ("b_idx", "<i4"),
    ("b_dx", "<f4"), ("b_dy", "<f4"),
    ("thr", "<i4"),      # go to `right` if pixel(A) - pixel(B) <= thr, else `left`
    ("left", "<i4"), ("right", "<i4"),  # child node index relative to the tree's first node
])
TREEINFO_DTYPE = np.dtype([("leaf_end", "<i4"), ("n_nodes", "<i4"), ("reject_thr", "<i4")])


@dataclass
class LdmfModel:
    path: Path
    header: bytes                 # raw 360-byte header
    model_id: int                 # header +8 (101 / 102), must be in 101..595
    payload_size: int             # header +12
    author: str                   # header +16 ("FaceSDK Team")
    timestamp: int                # header +0x60 u32; looks like a Unix time (INFERRED: 2015-02-27 / 2014-10-28)
    description: str              # header +0x68 free text
    body_tag: bytes               # "SACD"
    body_size: int
    model_type: int               # TYPE_INFRARED / TYPE_LOWEND
    n_landmarks: int
    leaf_weights: np.ndarray      # int32 [n_stages * trees * 32] (stage 0 weights included)
    nodes: np.ndarray             # NODE_DTYPE [n_file_trees * 63]  (stages 1..)
    tree_info: np.ndarray         # TREEINFO_DTYPE [n_file_trees]
    shapes: np.ndarray            # float32 [n_shapes, L, 2]: mean, stage-0 leaf deltas, then per stage (bias, leaf deltas)
    hog_tag: bytes
    hog_size: int
    hog_threshold: float
    hog_weights: np.ndarray       # float32 [L*128]
    extra: dict = field(default_factory=dict)

    @property
    def type_name(self) -> str:
        return {TYPE_INFRARED: "infrared", TYPE_LOWEND: "lowend"}.get(self.model_type, hex(self.model_type))


def _cstr(b: bytes) -> str:
    return b.split(b"\0", 1)[0].decode("latin-1")


def parse(path: str | Path) -> LdmfModel:
    path = Path(path)
    b = path.read_bytes()
    magic, hsize, model_id, payload = struct.unpack_from("<4sIII", b, 0)
    if magic != MAGIC or hsize not in (16, 360) or not (101 <= model_id <= 595):
        raise ValueError(f"{path}: not an LDMF model (JointCascadeModel::Read checks)")
    hdr = b[:360]
    o = 360
    body_tag, body_size, model_type = struct.unpack_from("<4sII", b, o); o += 12
    (L,) = struct.unpack_from("<i", b, o); o += 4

    def arr(dt, n):
        nonlocal o
        a = np.frombuffer(b, dt, n, o).copy(); o += a.nbytes
        return a

    (n,) = struct.unpack_from("<i", b, o); o += 4
    leaf_weights = arr("<i4", n)
    (n,) = struct.unpack_from("<i", b, o); o += 4
    nodes = arr(NODE_DTYPE, n)
    (n,) = struct.unpack_from("<i", b, o); o += 4
    tree_info = arr(TREEINFO_DTYPE, n)
    (n_shapes,) = struct.unpack_from("<i", b, o); o += 4
    (n_bytes,) = struct.unpack_from("<i", b, o); o += 4
    unused = b[o:o + n_bytes]; o += n_bytes   # std::vector<char>, read and never used by the predictor
    pad = 8 * (2 * ((8 * L + 15) >> 4) - L)   # DeltaShape is a 256-byte slot; file rows padded to 16 bytes
    shapes = np.empty((n_shapes, L, 2), np.float32)
    for i in range(n_shapes):
        shapes[i] = np.frombuffer(b, "<f4", 2 * L, o).reshape(L, 2); o += 8 * L + pad
    hog_tag, hog_size, hog_thr = struct.unpack_from("<4sIf", b, o); o += 12
    (n,) = struct.unpack_from("<i", b, o); o += 4
    hog_w = arr("<f4", n)
    if o != len(b):
        raise ValueError(f"{path}: {len(b) - o} trailing bytes")
    return LdmfModel(path=path, header=hdr, model_id=model_id, payload_size=payload,
                     author=_cstr(hdr[16:0x50]), timestamp=struct.unpack_from("<I", hdr, 0x60)[0], description=_cstr(hdr[0x68:360]),
                     body_tag=body_tag, body_size=body_size, model_type=model_type, n_landmarks=L,
                     leaf_weights=leaf_weights, nodes=nodes, tree_info=tree_info, shapes=shapes,
                     hog_tag=hog_tag, hog_size=hog_size, hog_threshold=float(hog_thr), hog_weights=hog_w,
                     extra={"unused_bytes": unused.hex(), "header_ints_0x1c": list(struct.unpack_from("<17i", hdr, 0x1c))})


def summary(m: LdmfModel) -> dict:
    T = int(m.tree_info.size)
    return {
        "file": m.path.name, "model_id": m.model_id, "type": m.type_name, "model_type_tag": hex(m.model_type),
        "author": m.author, "timestamp": m.timestamp, "description": m.description,
        "payload_size": m.payload_size, "body_tag": m.body_tag.decode(), "body_size": m.body_size,
        "landmarks": m.n_landmarks, "leaf_weights": int(m.leaf_weights.size), "file_trees": T,
        "nodes_per_tree": sorted(set(m.tree_info["n_nodes"].tolist())), "shapes": int(m.shapes.shape[0]),
        "hog_tag": m.hog_tag.decode(), "hog_size": m.hog_size, "hog_threshold": m.hog_threshold,
        "hog_weights": int(m.hog_weights.size),
    }
