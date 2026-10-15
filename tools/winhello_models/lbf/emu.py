"""Run Microsoft's own FaceProcessorCore.dll x64 code under Unicorn to cross-check the numpy port.

Validation-only tool: needs the original DLL (not shipped). The DLL is mapped at its preferred
image base (no relocations needed), CRT imports are replaced by Python stubs, and C++ objects
are built in emulated memory with the field layouts recovered from the loader functions.
"""
from __future__ import annotations

import math
import struct

import numpy as np
import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64, UC_HOOK_CODE
from unicorn.x86_const import (UC_X86_REG_RAX, UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8,
                               UC_X86_REG_R9, UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_XMM0,
                               UC_X86_REG_XMM1, UC_X86_REG_XMM2, UC_X86_REG_XMM3)

from .format import LbfModel, LdaRegressor

STUB = 0x10_0000_0000
STACK = 0x7000_0000
STACK_SIZE = 0x40_0000
HEAP = 0x2000_0000
HEAP_SIZE = 0x1000_0000
RET_MAGIC = STUB + 0xFFF0

# FaceProcessorCore.dll 10.0.26100.9444
ALIGNER_PREDICT = 0x180010CB0
IRFO_COMPUTE_ORIENTATION = 0x18000CFE0
IRFO_VTABLE = 0x180049388
IRFO_CONTEXT_VTABLE = 0x180049088
IRFO_TEMPLATE_INIT = 0x180022754  # only for reference; template read via read_template()
IRFO_TEMPLATE_RDATA = [0x18004B5B0, 0x18004B5D0, 0x18004B5E0, 0x18004B550, 0x18004B5C0, 0x18004B590,
                       0x18004B580, 0x18004B540, 0x18004B560, 0x18004B5A0, 0x18004B530, 0x18004B570,
                       0x18004B5F0]
LBP_LUT = 0x18004B0A0

XMM = [UC_X86_REG_XMM0, UC_X86_REG_XMM1, UC_X86_REG_XMM2, UC_X86_REG_XMM3]
GPR = [UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9]


def f2i(v: float) -> int:
    return struct.unpack("<I", struct.pack("<f", v))[0]


def i2f(v: int) -> float:
    return struct.unpack("<f", struct.pack("<I", v & 0xFFFFFFFF))[0]


class MsDll:
    def __init__(self, dll_path: str):
        pe = pefile.PE(dll_path)
        self.pe = pe
        self.base = pe.OPTIONAL_HEADER.ImageBase
        size = (pe.OPTIONAL_HEADER.SizeOfImage + 0xFFF) & ~0xFFF
        uc = Uc(UC_ARCH_X86, UC_MODE_64)
        self.uc = uc
        uc.mem_map(self.base, size)
        uc.mem_write(self.base, pe.header)
        for s in pe.sections:
            data = s.get_data()
            uc.mem_write(self.base + s.VirtualAddress, data[:max(s.Misc_VirtualSize, len(data))])
        uc.mem_map(STUB, 0x10000)
        uc.mem_write(STUB, b"\xc3" * 0x10000)
        uc.mem_map(STACK, STACK_SIZE)
        uc.mem_map(HEAP, HEAP_SIZE)
        self.heap_top = HEAP
        self.stubs = {}
        i = 0
        for entry in pe.DIRECTORY_ENTRY_IMPORT:
            for imp in entry.imports:
                name = imp.name.decode() if imp.name else f"ord{imp.ordinal}"
                addr = STUB + 16 * i
                uc.mem_write(imp.address, struct.pack("<Q", addr))
                self.stubs[addr] = name
                i += 1
        uc.hook_add(UC_HOOK_CODE, self._stub_hook, begin=STUB, end=STUB + 0xFFFF)

    # --- memory helpers
    def alloc(self, n: int, align: int = 64) -> int:
        self.heap_top = (self.heap_top + align - 1) & ~(align - 1)
        p = self.heap_top
        self.heap_top += n
        if self.heap_top > HEAP + HEAP_SIZE:
            raise MemoryError("emulated heap exhausted")
        return p

    def put(self, data: bytes, align: int = 64) -> int:
        p = self.alloc(len(data), align)
        self.uc.mem_write(p, bytes(data))
        return p

    def read(self, addr: int, n: int) -> bytes:
        return bytes(self.uc.mem_read(addr, n))

    def reset_heap(self):
        self.heap_top = HEAP

    # --- CRT stubs
    def _stub_hook(self, uc, addr, size, _):
        if addr == RET_MAGIC:
            uc.emu_stop()
            return
        name = self.stubs.get(addr)
        if name is None:
            return
        rcx, rdx, r8 = (uc.reg_read(r) for r in GPR[:3])
        if name == "memset":
            uc.mem_write(rcx, bytes([rdx & 0xFF]) * r8)
            uc.reg_write(UC_X86_REG_RAX, rcx)
        elif name in ("memcpy", "memmove"):
            uc.mem_write(rcx, bytes(uc.mem_read(rdx, r8)))
            uc.reg_write(UC_X86_REG_RAX, rcx)
        elif name == "_o_sqrtf":
            x = i2f(uc.reg_read(UC_X86_REG_XMM0))
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.sqrt(np.float32(x)))))
        elif name == "_o_expf":
            x = i2f(uc.reg_read(UC_X86_REG_XMM0))
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.exp(np.float32(x)))))
        elif name == "_o_floorf":
            x = i2f(uc.reg_read(UC_X86_REG_XMM0))
            uc.reg_write(UC_X86_REG_XMM0, f2i(math.floor(x)))
        elif name == "_o_floor":
            x = struct.unpack("<d", struct.pack("<Q", uc.reg_read(UC_X86_REG_XMM0) & (2**64 - 1)))[0]
            uc.reg_write(UC_X86_REG_XMM0, struct.unpack("<Q", struct.pack("<d", math.floor(x)))[0])
        elif name in ("_o_malloc", "_o__malloc_base"):
            uc.reg_write(UC_X86_REG_RAX, self.alloc(max(rcx, 1), 16))
        elif name in ("_o_free", "_o__free_base"):
            pass
        elif name in ("QueryPerformanceCounter", "QueryPerformanceFrequency"):
            uc.mem_write(rcx, struct.pack("<Q", 1))
            uc.reg_write(UC_X86_REG_RAX, 1)
        elif name == "GetTickCount":
            uc.reg_write(UC_X86_REG_RAX, 0)
        else:
            raise RuntimeError(f"unhandled import {name}")
        # the stub byte is RET, which pops the caller's return address

    def call(self, fn: int, args: list, max_insn: int = 0) -> int:
        """Win64 call. args: int, or ('f', float) for float args."""
        uc = self.uc
        sp = STACK + STACK_SIZE - 0x10000
        sp &= ~0xF
        stack_args = args[4:]
        frame = sp - 8 - 0x20 - 8 * len(stack_args)
        frame &= ~0xF
        frame -= 8  # rsp % 16 == 8 at function entry
        uc.mem_write(frame, struct.pack("<Q", RET_MAGIC))
        for i, a in enumerate(stack_args):
            v = f2i(a[1]) if isinstance(a, tuple) else a
            uc.mem_write(frame + 0x28 + 8 * i, struct.pack("<Q", v & 0xFFFFFFFFFFFFFFFF))
        for i, a in enumerate(args[:4]):
            if isinstance(a, tuple):
                uc.reg_write(XMM[i], f2i(a[1]))
            else:
                uc.reg_write(GPR[i], a & 0xFFFFFFFFFFFFFFFF)
        uc.reg_write(UC_X86_REG_RSP, frame)
        uc.emu_start(fn, RET_MAGIC, count=max_insn)
        return uc.reg_read(UC_X86_REG_RAX)

    def read_template(self) -> np.ndarray:
        raw = b"".join(self.read(a, 16) for a in IRFO_TEMPLATE_RDATA)
        raw += struct.pack("<II", 0x4201FEFA, 0x42379FBE)  # immediates at 0x180022810/0x180022817
        return np.frombuffer(raw, "<f4").reshape(27, 2).copy()

    def read_lbp_lut(self) -> np.ndarray:
        return np.frombuffer(self.read(LBP_LUT, 256), np.uint8).copy()


def _regressor_bytes(dll: MsDll, reg) -> bytes:
    raw = np.ascontiguousarray(reg.raw)
    p = dll.put(raw.tobytes())
    out = bytearray(40)
    out[0] = 1 if reg.compressed else 0
    struct.pack_into("<QQfII", out, 8, p if reg.compressed else 0, 0 if reg.compressed else p,
                     reg.scale or 0.0, reg.rows, reg.cols)
    return bytes(out)


class EmuAligner:
    """LbfAligner::Aligner object built in emulated memory; calls Aligner::Predict (0x180010cb0)."""

    def __init__(self, dll: MsDll, model: LbfModel, num_stages: int | None = None, classifier: bool = True):
        self.dll = dll
        self.model = model
        n = model.num_points
        stages = model.stages[:num_stages] if num_stages else model.stages
        stage_ptr = dll.alloc(64 * len(stages))
        for si, st in enumerate(stages):
            trees_ptr = dll.alloc(16 * st.num_trees)
            for ti, t in enumerate(st.trees):
                nodes = dll.put(t.tobytes())
                dll.uc.mem_write(trees_ptr + 16 * ti, struct.pack("<iiQ", len(t), 0, nodes))
            hdr = struct.pack("<IIIIQ", st.num_trees, st.num_points, st.trees_per_point, 0, trees_ptr)
            dll.uc.mem_write(stage_ptr + 64 * si, hdr + _regressor_bytes(dll, st.regressor))
        mean_ptr = dll.put(model.mean_shape.astype("<f4").tobytes())
        obj = bytearray(0x80)
        struct.pack_into("<QIIQQI", obj, 0, 0x1800493D8, n, 0, mean_ptr, stage_ptr, len(stages))
        if classifier and model.classifier is not None:
            # Classifier at +40: {Stage* last (of the full model), Regressor}; enabled flag at +88
            last = stage_ptr + 64 * (len(stages) - 1)
            struct.pack_into("<Q", obj, 40, last)
            obj[48:88] = _regressor_bytes(dll, model.classifier)
            obj[88] = 1
        struct.pack_into("<8f", obj, 96, *model.unit_box.reshape(-1))
        self.ptr = dll.put(bytes(obj))
        self.scratch_size = 1 << 20
        self.scratch = dll.alloc(self.scratch_size)
        self.out_flag = dll.alloc(16)
        self.shape_ptr = dll.alloc(8 * n)
        self.rect_ptr = dll.alloc(16)
        self.img_ptr = None

    def predict(self, img: np.ndarray, rect=None, prev=None):
        dll = self.dll
        img = np.ascontiguousarray(img, np.uint8)
        h, w = img.shape
        if self.img_ptr is None or self._img_shape != img.shape:
            self.img_ptr = dll.alloc(img.size)
            self._img_shape = img.shape
        dll.uc.mem_write(self.img_ptr, img.tobytes())
        n = self.model.num_points
        track = prev is not None
        if track:
            dll.uc.mem_write(self.shape_ptr, np.asarray(prev, "<f4").tobytes())
        else:
            dll.uc.mem_write(self.rect_ptr, struct.pack("<iiii", *(int(v) for v in rect)))
        dll.uc.mem_write(self.out_flag, b"\0" * 8)
        hr = dll.call(ALIGNER_PREDICT, [self.ptr, self.img_ptr, w, h, w, 1 if track else 0,
                                        0 if track else self.rect_ptr, 2 * n, self.shape_ptr,
                                        self.scratch_size, self.scratch, self.out_flag])
        hr &= 0xFFFFFFFF
        pts = np.frombuffer(dll.read(self.shape_ptr, 8 * n), "<f4").reshape(n, 2).copy()
        rejected = dll.read(self.out_flag, 1)[0] != 0
        return hr, pts, rejected


def _lda_object(dll: MsDll, r: LdaRegressor) -> int:
    d = r.projection.shape[0]
    basis = np.zeros((d, 7168), "<f4")
    basis[:, :7139] = r.projection
    mean = np.zeros(7171, "<f4")
    mean[:7139] = r.mean
    obj = struct.pack("<iiQQQQiiiiQQ", d, 0, dll.put(basis.tobytes()), dll.put(mean.tobytes()),
                      dll.alloc(0x6F8C), dll.alloc(4 * d), r.knn_k, r.knn_gamma, len(r.labels),
                      r.centroids.shape[1], dll.put(r.centroids.astype("<f4").tobytes()),
                      dll.put(r.labels.astype("<f4").tobytes()))
    return dll.put(obj)


class EmuPose:
    """IrFaceOrientation object + context in emulated memory; calls ComputeOrientation (0x18000cfe0)."""

    def __init__(self, dll: MsDll, yaw: LdaRegressor, pitch: LdaRegressor):
        self.dll = dll
        obj = bytearray(0x118)
        struct.pack_into("<QQQI", obj, 0, IRFO_VTABLE, _lda_object(dll, yaw), _lda_object(dll, pitch), 27)
        obj[28:28 + 216] = dll.read_template().astype("<f4").tobytes()
        self.ptr = dll.put(bytes(obj))
        self.scratch_size = 0x40000
        self.scratch = dll.alloc(self.scratch_size)
        self.ctx = dll.alloc(0x200)
        dll.uc.mem_write(self.ctx, b"\0" * 0x200)
        self.lum = dll.alloc(32)
        self.img_ptr = None

    def compute(self, img: np.ndarray, points: np.ndarray, timestamp: int = 0, prev_ts: int | None = None,
                prev_yaw: float = 0.0, prev_pitch: float = 0.0):
        dll = self.dll
        img = np.ascontiguousarray(img, np.uint8)
        h, w = img.shape
        if self.img_ptr is None or self._img_shape != img.shape:
            self.img_ptr = dll.alloc(img.size)
            self._img_shape = img.shape
        dll.uc.mem_write(self.img_ptr, img.tobytes())
        dll.uc.mem_write(self.lum, struct.pack("<QIIIIQ", 0, w, h, w, 0, self.img_ptr))
        ctx = bytearray(0x200)
        struct.pack_into("<QQ", ctx, 0, IRFO_CONTEXT_VTABLE, self.lum)
        pts = np.asarray(points, "<f4")
        struct.pack_into("<I", ctx, 40, len(pts))
        ctx[44:44 + pts.nbytes] = pts.tobytes()
        struct.pack_into("<QI", ctx, 296, self.scratch, self.scratch_size)
        if prev_ts is None:
            prev_ts = timestamp - 0x7FFFFFFF  # long ago -> no prior
        struct.pack_into("<Qfff", ctx, 352, prev_ts & 0xFFFFFFFFFFFFFFFF, 0.0, prev_yaw, prev_pitch)
        dll.uc.mem_write(self.ctx, bytes(ctx))
        hr = dll.call(IRFO_COMPUTE_ORIENTATION, [self.ptr, timestamp, self.ctx]) & 0xFFFFFFFF
        res = dll.read(self.ctx + 308, 20)
        pitch, yaw, roll = struct.unpack_from("<fff", res)
        flags = tuple(res[12:16])
        frontal = struct.unpack_from("<f", res, 16)[0]
        feat = np.frombuffer(dll.read(self.scratch + self.scratch_size - 28556, 28556), "<f4").copy()
        return dict(hr=hr, pitch=pitch, yaw=yaw, roll=roll, flags=flags, frontal_margin=frontal, feature=feat)


# FaceRecognitionEngineAdapter.dll 10.0.26100.9444 (no public PDB; names are ours)
ENG_CROP_128 = 0x180015F68   # (img, pts5, w, h, stride, out_w, out_h, channels, out) -> HRESULT
ENG_REF5 = 0x1800D7F68       # 5 reference points in the 281x281 canvas


def emu_engine_crop(eng: MsDll, img: np.ndarray, pts5: np.ndarray, channels: int = 1) -> tuple[int, np.ndarray]:
    img = np.ascontiguousarray(img, np.uint8)
    h, w = img.shape[:2]
    pimg = eng.put(img.tobytes())
    ppts = eng.put(np.asarray(pts5, "<f4").tobytes())
    out = eng.alloc(128 * 128 * channels)
    hr = eng.call(ENG_CROP_128, [pimg, ppts, w, h, w * channels, 128, 128, channels, out]) & 0xFFFFFFFF
    return hr, np.frombuffer(eng.read(out, 128 * 128 * channels), np.uint8).reshape(128, 128, channels).copy()
