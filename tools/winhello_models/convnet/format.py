"""Parser for Microsoft DNNTestLib ``conv_net_v1.0`` model files.

Byte layout and semantics are derived from FaceRecognitionEngineAdapter.dll
(Windows Hello Face); see docs/winhello-models/convnet.md for addresses.

All integers are little-endian int32, strings are ``int32 length + bytes``
(no terminator), floats are little-endian float32.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass, field

import numpy as np

MAGIC = "conv_net_v1.0"

# Layer types accepted by the engine's factory (sub_1800359E0).  Only the ones
# that carry no extra payload beyond the common header are listed here; the
# others have dedicated readers below or are rejected.
_HEADER_ONLY = {"neuron", "flatten", "concat", "softmax", "eltwise_max", "bgr2gray"}


class FormatError(ValueError):
    pass


@dataclass
class Blob:
    """A DNNTestLib parameter tensor (reader sub_18004EBD4).

    ``dims`` = (n, c, a, b) as stored; on disk the data is ordered
    ``[b][a][c][n]`` (n fastest; storage type 1, index fn sub_180025188).
    For conv weights n=out-channels, c=in-channels, a=kernel width,
    b=kernel height.  For fc weights n=outputs, c=inputs.
    """

    dims: tuple[int, int, int, int]
    pads: tuple[int, int]
    data: np.ndarray | None  # shape (b, a, c, n), float32; None if the blob is flagged empty

    def as_bacn(self) -> np.ndarray:
        assert self.data is not None
        return self.data


@dataclass
class Layer:
    type: str
    subtype: str
    name: str
    inputs: list[str]
    ints: list[int] = field(default_factory=list)
    weight: Blob | None = None
    bias: Blob | None = None
    mean: Blob | None = None

    # Convenience accessors with the semantics derived from the DLL.
    @property
    def conv_pad(self) -> tuple[int, int]:  # (x, y) = (padA, padB)
        return self.ints[0], self.ints[1]

    @property
    def conv_stride(self) -> tuple[int, int]:
        return self.ints[2], self.ints[3]

    @property
    def pool_stride(self) -> tuple[int, int]:  # (x, y)
        return self.ints[0], self.ints[1]

    @property
    def pool_kernel(self) -> tuple[int, int]:
        return self.ints[2], self.ints[3]


@dataclass
class ConvNet:
    magic: str
    layers: list[Layer]
    trailer: bytes  # bytes left after the layer list (dense head for classifiers 11/12)

    @property
    def input_layer(self) -> Layer:
        return self.layers[0]

    @property
    def input_shape(self) -> tuple[int, int, int]:
        """(channels, height, width) of the image_data layer."""
        n, c, w, h = self.layers[0].ints  # DataLayer ints: n, c, a(=width), b(=height)
        return c, h, w

    def head_weights(self) -> np.ndarray | None:
        """2x512 cosine head stored after the net (read by sub_1800C4B14 for slots 11/12)."""
        if len(self.trailer) != 4096:
            return None
        return np.frombuffer(self.trailer, "<f4").reshape(2, 512).copy()


class _Reader:
    def __init__(self, buf: bytes):
        self.buf = buf
        self.off = 0

    def i32(self) -> int:
        if self.off + 4 > len(self.buf):
            raise FormatError(f"truncated at {self.off}")
        (v,) = struct.unpack_from("<i", self.buf, self.off)
        self.off += 4
        return v

    def string(self) -> str:
        n = self.i32()
        if n < 0 or self.off + n > len(self.buf):
            raise FormatError(f"bad string length {n} at {self.off - 4}")
        s = self.buf[self.off : self.off + n].decode("latin-1")
        self.off += n
        return s

    def blob(self) -> Blob:
        n, c, a, b, pa, pb = (self.i32() for _ in range(6))
        empty = n == 0
        if empty:
            n = 1
            if a == 0:  # sub_18004EBD4 returns before reading any data
                return Blob((0, c, a, b), (pa, pb), None)
        count = n * c * a * b
        if count < 0 or self.off + 4 * count > len(self.buf):
            raise FormatError(f"bad blob dims {(n, c, a, b)} at {self.off}")
        arr = np.frombuffer(self.buf, "<f4", count, self.off).reshape(b, a, c, n).copy()
        self.off += 4 * count
        # an "empty" blob with data is skipped by the engine (returns nullptr)
        return Blob((0 if empty else n, c, a, b), (pa, pb), None if empty else arr)


def parse(buf: bytes) -> ConvNet:
    r = _Reader(buf)
    magic = r.string()
    if magic != MAGIC:
        raise FormatError(f"magic {magic!r} != {MAGIC!r}")
    count = r.i32()
    if count < 2:  # sub_180033FE0 rejects < 2 layers
        raise FormatError(f"layer count {count}")
    layers = []
    for _ in range(count):
        t, sub, name = r.string(), r.string(), r.string()
        inputs = [r.string() for _ in range(r.i32())]
        L = Layer(t, sub, name, inputs)
        if t in ("data", "image_data"):
            L.ints = [r.i32() for _ in range(4)]
            if t == "image_data":
                L.mean = r.blob()
        elif t == "conv":
            L.ints = [r.i32() for _ in range(4)]
            L.weight, L.bias = r.blob(), r.blob()
        elif t == "pool":
            if sub not in ("max", "avg"):
                raise FormatError(f"pool type {sub!r}")
            L.ints = [r.i32() for _ in range(4)]
        elif t == "fc":
            L.weight, L.bias = r.blob(), r.blob()
        elif t == "dropout":
            L.ints = [r.i32()]
        elif t == "padding":
            L.ints = [r.i32(), r.i32()]
        elif t == "neuron":
            if sub not in ("relu", "sigmoid"):
                raise FormatError(f"neuron type {sub!r}")
        elif t in _HEADER_ONLY:
            pass
        else:
            # vary_data, face_data, crop_face_data, local_share, across/within_channel_norm,
            # lrnorm, joint_bayesian: have readers in the DLL but are not used by any
            # shipped Windows Hello model we have seen; not implemented.
            raise FormatError(f"unsupported layer type {t!r} ({name})")
        layers.append(L)
    return ConvNet(magic, layers, buf[r.off :])


def load(path) -> ConvNet:
    with open(path, "rb") as f:
        return parse(f.read())
