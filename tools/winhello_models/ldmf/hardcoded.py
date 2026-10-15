"""Constants that FaceTrackerInternal.dll keeps in code/.rdata instead of the LDMF file.

Data tables are read directly from the DLL (.rdata). The *structure* of the first
cascade stage and of the fern pre-filter is compiled into machine code
(JointCascadePredictProduction{Infrared,LowEnd}ModelBoostN, FernPredict1..8); it is
recovered once by `ldmf/extract` (IDA + a symbolic C++ harness) into
`hardcoded_structure.npz`. Addresses are for FaceTrackerInternal.dll from
Windows 11 build 26300 (PDB-named, image base 0x180000000).
"""
from __future__ import annotations

import hashlib
from pathlib import Path

import numpy as np
import pefile

FERN_WEIGHTS = 0x1800AE6F0     # int32[400][256]; DetectInLayer passes &unk_1800AE6F0 to FernPredict1
FERN_INDEX_LUT = 0x180112750   # uint8[65536]: _mm_movemask_epi8 bits -> 8-bit fern index (verified = lane bits)
ATAN_LUT_CENTER = 0x18012AB90  # float[128][128] centred: dword_18012AB90[128*(dy>>2) + (dx>>2)] = atan2 in degrees
REF_SHAPE = {"infrared": (0x180134440, 31), "lowend": (0x180134FF0, 27)}  # AssignCascadeModel
HOG_TEMPLATE5 = (0x180122A30, 0x180122A20)  # OrientedHogPostFilter::Predict: 5-point template (+2 immediates)
HOG_TEMPLATE5_TAIL = (1059015432, 1059761337)  # int bit patterns of the last two floats
INIT_SCORE = {"infrared": 126862, "lowend": -8221270}  # JointCascadePredictProduction*Model, *a11 = ...
FERN_INIT_SCORE = -25453782    # DetectInLayer: v182 = -25453782 before FernPredict1
FERN_MIN_PASSED = 8            # JointCascadeFaceDetector ctor: word @+164 = 8 (dense-rescan trigger)
SCORE_SCALE = float(1 << 24)   # AssignCascadeModel: this+35 = 16777216.0f


class Dll:
    def __init__(self, path: str | Path):
        self.path = Path(path)
        self.data = self.path.read_bytes()
        self.pe = pefile.PE(data=self.data, fast_load=True)
        self.base = self.pe.OPTIONAL_HEADER.ImageBase

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()

    def read(self, va: int, n: int) -> bytes:
        return self.pe.get_data(va - self.base, n)

    def array(self, va: int, dtype, count: int) -> np.ndarray:
        dt = np.dtype(dtype)
        return np.frombuffer(self.read(va, dt.itemsize * count), dt).copy()


def read_tables(dll: Dll) -> dict:
    t = {
        "fern_weights": dll.array(FERN_WEIGHTS, "<i4", 400 * 256).reshape(400, 256),
        "atan_lut": dll.array(ATAN_LUT_CENTER - 4 * (64 * 128 + 64), "<f4", 128 * 128).reshape(128, 128),
        "hog_template5": np.concatenate([dll.array(HOG_TEMPLATE5[0], "<f4", 4), dll.array(HOG_TEMPLATE5[1], "<f4", 4),
                                         np.array(HOG_TEMPLATE5_TAIL, np.int32).view(np.float32)]).reshape(5, 2),
    }
    for name, (va, L) in REF_SHAPE.items():
        t[f"ref_shape_{name}"] = dll.array(va, "<f4", 2 * L).reshape(L, 2)
    lut = dll.array(FERN_INDEX_LUT, "u1", 65536)
    expect = np.zeros(65536, np.uint8)
    for lanes in range(256):
        m = 0
        for i in range(8):
            if lanes >> i & 1:
                m |= 3 << (2 * i)
        expect[m] = lanes
    assert all(lut[np.nonzero(expect)[0]] == expect[np.nonzero(expect)[0]]), "fern index LUT is not plain lane bits"
    return t
