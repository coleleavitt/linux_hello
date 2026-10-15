"""Parser for the Windows Hello Face recognition model container (``.cefa``).

Layout derived from FaceRecognitionEngineAdapter.dll (Windows 11 26300); see
docs/winhello-models/cefa.md for the byte tables and the function addresses.
The container is not encrypted and has no checksum.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass, field

import numpy as np

MAGIC = 0xFACE
VERSION = 10
HEADER_SIZE = 10

LDMF_MAGIC = b"LDMF"
NNDR_MAGIC = b"NNDR"
LDMF_TYPE_CONVNET = 302

# FaceVerificationTech -> template byte size (engine sub_1800AF6E0); /4 = feature length
TEMPLATE_BYTES = {3: 3604, 7: 512, 8: 512, 9: 512, 10: 1024, 11: 1024}


@dataclass
class CalibrationTable:
    hardware_model: int
    ids: tuple[int, ...]
    # rows of (false positive rate, true accept rate, cosine distance)
    rows: np.ndarray


@dataclass
class Blob:
    offset: int
    data: bytes

    @property
    def kind(self) -> str:
        if self.data[4:8] == b"ORTM":
            return "ort"
        if self.data[:4] == LDMF_MAGIC:
            return "ldmf"
        return "bin"


@dataclass
class Container:
    version: int
    reserved: bytes
    tech: int
    flag: int
    tables: list[CalibrationTable] = field(default_factory=list)
    blobs: list[Blob] = field(default_factory=list)


class FormatError(ValueError):
    pass


def parse(buf: bytes) -> Container:
    if len(buf) < HEADER_SIZE + 9:
        raise FormatError("too short")
    magic, version = struct.unpack_from("<HB", buf, 0)
    if magic != MAGIC or version != VERSION:
        raise FormatError(f"bad magic/version {magic:#x}/{version}")
    (body_size,) = struct.unpack_from("<I", buf, 6)
    if body_size != len(buf) - HEADER_SIZE:
        raise FormatError(f"body size {body_size} != {len(buf) - HEADER_SIZE}")
    off = HEADER_SIZE
    tech, flag, n_tables = struct.unpack_from("<IBI", buf, off)
    off += 9
    tables = []
    for _ in range(n_tables):
        hw, n_ids = struct.unpack_from("<II", buf, off)
        off += 8
        ids = struct.unpack_from(f"<{n_ids}I", buf, off)
        off += 4 * n_ids
        (n_rows,) = struct.unpack_from("<I", buf, off)
        off += 4
        rows = np.frombuffer(buf, "<f4", 3 * n_rows, off).reshape(n_rows, 3).copy()
        off += 12 * n_rows
        tables.append(CalibrationTable(hw, ids, rows))
    blobs = []
    while off < len(buf):
        (n,) = struct.unpack_from("<I", buf, off)
        if off + 4 + n > len(buf):
            raise FormatError(f"blob at {off} overruns file")
        blobs.append(Blob(off + 4, bytes(buf[off + 4 : off + 4 + n])))
        off += 4 + n
    return Container(version, bytes(buf[3:6]), tech, flag, tables, blobs)


def unwrap_ldmf(blob: bytes) -> bytes:
    """Return the conv_net_v1 payload of an LDMF/NNDR wrapper (engine sub_1800B65E0)."""
    magic, hdr_size, typ, total = struct.unpack_from("<4sIII", blob, 0)
    if magic != LDMF_MAGIC or hdr_size not in (16, 360) or total != len(blob) or typ != LDMF_TYPE_CONVNET:
        raise FormatError(f"bad LDMF header {magic!r} {hdr_size} {typ} {total}")
    tag, size = struct.unpack_from("<4sI", blob, hdr_size)
    if tag != NNDR_MAGIC or size != len(blob) - hdr_size:
        raise FormatError("bad NNDR chunk")
    return blob[hdr_size + 8 :]
