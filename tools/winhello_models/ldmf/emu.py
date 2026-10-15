"""Run FaceTrackerInternal.dll's own x64 code under Unicorn to cross-check `ldmf.jda` (validation only).

The DLL is mapped at its preferred image base (no relocations), CRT imports are Python stubs,
and the JointCascadeFaceDetector fields used by the predict functions are filled in emulated memory
with the offsets read from AssignCascadeModel (0x18009c170).
"""
from __future__ import annotations

import math
import struct

import numpy as np
import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64, UC_HOOK_CODE
from unicorn.x86_const import (UC_X86_REG_RAX, UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9,
                               UC_X86_REG_RSP, UC_X86_REG_XMM0, UC_X86_REG_XMM1, UC_X86_REG_XMM2, UC_X86_REG_XMM3)

FERN_PREDICT = [0x180007210, 0x18001B8C0, 0x18001EE20, 0x180022490, 0x180025C40, 0x180029530, 0x18002CB60, 0x180036C70]
FERN_WEIGHTS = 0x1800AE6F0
PREDICT = {"infrared": 0x18000CA80, "lowend": 0x18000D610}
REF_SHAPE = {"infrared": 0x180134440, "lowend": 0x180134FF0}
HOG_PREDICT = 0x1800054F0
CHKSTK = 0x1800AA1D0  # __chkstk reads gs:[10h]; the emulated stack is large, so it is patched to RET

STUB = 0x10_0000_0000
STACK = 0x7000_0000
STACK_SIZE = 0x40_0000
HEAP = 0x2000_0000
HEAP_SIZE = 0x2000_0000
RET_MAGIC = STUB + 0xFFF0
GPR = [UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9]
XMM = [UC_X86_REG_XMM0, UC_X86_REG_XMM1, UC_X86_REG_XMM2, UC_X86_REG_XMM3]


def f2i(v):
    return struct.unpack("<I", struct.pack("<f", v))[0]


def i2f(v):
    return struct.unpack("<f", struct.pack("<I", v & 0xFFFFFFFF))[0]


class Emu:
    def __init__(self, dll_path, chkstk=CHKSTK):
        pe = pefile.PE(dll_path)
        self.base = pe.OPTIONAL_HEADER.ImageBase
        uc = Uc(UC_ARCH_X86, UC_MODE_64)
        self.uc = uc
        uc.mem_map(self.base, (pe.OPTIONAL_HEADER.SizeOfImage + 0xFFF) & ~0xFFF)
        uc.mem_write(self.base, pe.header)
        for s in pe.sections:
            d = s.get_data()
            uc.mem_write(self.base + s.VirtualAddress, d[:max(s.Misc_VirtualSize, len(d))])
        uc.mem_map(STUB, 0x10000)
        uc.mem_write(STUB, b"\xc3" * 0x10000)
        uc.mem_map(STACK, STACK_SIZE)
        uc.mem_map(HEAP, HEAP_SIZE)
        self.top = HEAP
        self.stubs = {}
        i = 0
        for e in pe.DIRECTORY_ENTRY_IMPORT:
            for imp in e.imports:
                name = imp.name.decode() if imp.name else f"ord{imp.ordinal}"
                a = STUB + 16 * i
                uc.mem_write(imp.address, struct.pack("<Q", a))
                self.stubs[a] = name
                i += 1
        uc.hook_add(UC_HOOK_CODE, self._hook, begin=STUB, end=STUB + 0xFFFF)
        if chkstk:
            uc.mem_write(chkstk, b"\xc3")
        # minimal TEB so that MSVC thread-safe static init (gs:[58h] TLS array, epoch at +4) runs its init path
        from unicorn.x86_const import UC_X86_REG_GS_BASE
        teb = self.alloc(0x2000, 0x1000)
        tls_arr = teb + 0x1000
        tls_blk = teb + 0x1800
        tls_index = struct.unpack("<I", self.read(self.base + pe.DIRECTORY_ENTRY_TLS.struct.AddressOfIndex - self.base, 4))[0] \
            if hasattr(pe, "DIRECTORY_ENTRY_TLS") else 0
        uc.mem_write(teb + 0x58, struct.pack("<Q", tls_arr))
        uc.mem_write(tls_arr + 8 * tls_index, struct.pack("<Q", tls_blk))
        uc.mem_write(tls_blk, struct.pack("<ii", 0, -2**31))
        uc.reg_write(UC_X86_REG_GS_BASE, teb)

    def alloc(self, n, align=64):
        self.top = (self.top + align - 1) & ~(align - 1)
        p = self.top
        self.top += max(n, 1)
        assert self.top < HEAP + HEAP_SIZE
        return p

    def put(self, data, align=64):
        p = self.alloc(len(data), align)
        self.uc.mem_write(p, bytes(data))
        return p

    def read(self, a, n):
        return bytes(self.uc.mem_read(a, n))

    def _f(self, r):
        return np.float32(i2f(self.uc.reg_read(r)))

    def _hook(self, uc, addr, size, _):
        if addr == RET_MAGIC:
            uc.emu_stop()
            return
        name = self.stubs.get(addr)
        if name is None:
            return
        rcx, rdx, r8, r9 = (uc.reg_read(r) for r in GPR)
        ret = None
        if name == "memset":
            uc.mem_write(rcx, bytes([rdx & 0xFF]) * r8); ret = rcx
        elif name in ("memcpy", "memmove"):
            uc.mem_write(rcx, bytes(uc.mem_read(rdx, r8))); ret = rcx
        elif name in ("memcpy_s", "memmove_s"):
            uc.mem_write(rcx, bytes(uc.mem_read(r8, r9))); ret = 0
        elif name in ("_o_sqrtf", "sqrtf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.sqrt(self._f(UC_X86_REG_XMM0)))))
        elif name in ("_o_atan2f", "atan2f"):
            y, x = self._f(UC_X86_REG_XMM0), self._f(UC_X86_REG_XMM1)
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.float32(math.atan2(y, x)))))
        elif name in ("_o_sinf", "sinf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.float32(math.sin(self._f(UC_X86_REG_XMM0))))))
        elif name in ("_o_cosf", "cosf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.float32(math.cos(self._f(UC_X86_REG_XMM0))))))
        elif name in ("_o_asinf", "asinf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.float32(math.asin(self._f(UC_X86_REG_XMM0))))))
        elif name in ("_o_ceilf", "ceilf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(math.ceil(self._f(UC_X86_REG_XMM0)))))
        elif name in ("_o_expf", "expf"):
            uc.reg_write(UC_X86_REG_XMM0, f2i(float(np.exp(self._f(UC_X86_REG_XMM0)))))
        elif name in ("AcquireSRWLockExclusive", "ReleaseSRWLockExclusive", "WakeAllConditionVariable",
                      "EnterCriticalSection", "LeaveCriticalSection", "SetEvent", "ResetEvent"):
            ret = 0
        elif name == "SleepConditionVariableSRW":
            ret = 1
        elif name.startswith("??2@") or name.startswith("??_U@") or name in ("_o_malloc", "malloc", "operator new"):
            ret = self.alloc(rcx, 16)
        elif name.startswith("??3@") or name.startswith("??_V@") or name in ("_o_free", "free"):
            ret = 0
        else:
            raise RuntimeError(f"unhandled import {name}")
        if ret is not None:
            uc.reg_write(UC_X86_REG_RAX, ret)

    def call(self, fn, args, max_insn=0):
        uc = self.uc
        sp = (STACK + STACK_SIZE - 0x10000) & ~0xF
        st = args[4:]
        frame = ((sp - 8 - 0x20 - 8 * len(st)) & ~0xF) - 8
        uc.mem_write(frame, struct.pack("<Q", RET_MAGIC))
        for i, a in enumerate(st):
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


def put_image(emu, img: np.ndarray):
    """Copy a 2-D uint8 image into emulated memory with a power-of-two stride; returns (ptr, log2 stride)."""
    h, w = img.shape
    sh = max(1, int(math.ceil(math.log2(w))))
    buf = np.zeros((h, 1 << sh), np.uint8)
    buf[:, :w] = img
    return emu.put(buf.tobytes()), sh


class EmuDetector:
    """Calls FernPredict1..8 and JointCascadePredictProduction*Model + OrientedHogPostFilter::Predict."""

    def __init__(self, dll_path, model, kind):
        self.emu = Emu(dll_path)
        self.kind = kind
        e = self.emu
        self.mark = None
        m = model  # ldmf.format.LdmfModel
        L = m.n_landmarks
        T = m.tree_info.size // 4
        lw = e.put(m.leaf_weights.astype("<i4").tobytes())
        nodes = e.put(m.nodes.tobytes())
        ti = e.put(m.tree_info.tobytes())
        shp = np.zeros((m.shapes.shape[0], 64), np.float32)
        shp[:, :2 * L] = m.shapes.reshape(-1, 2 * L)
        deltas = e.put(shp.tobytes())
        obj = bytearray(0x110)
        struct.pack_into("<ii", obj, 24, 0, L)                       # +24 type (unused here), +28 L
        struct.pack_into("<iiii", obj, 32, m.leaf_weights.size, m.shapes.shape[0], m.nodes.size, m.tree_info.size)
        struct.pack_into("<QQQQ", obj, 48, lw, deltas, nodes, ti)    # +48 leaf w, +56 deltas, +64 nodes, +72 tree info
        struct.pack_into("<ii", obj, 120, 96, 96)                    # +120/+124 window
        struct.pack_into("<iiif", obj, 128, T, T * 32, 5, 16777216.0)  # +128 trees/stage, +132 leaves/stage, +136 stages, +140 scale
        struct.pack_into("<Q", obj, 144, REF_SHAPE[kind])            # +144 reference shape
        self.obj = e.put(bytes(obj))
        hog = bytearray(32)
        hw = e.put(m.hog_weights.astype("<f4").tobytes())
        struct.pack_into("<QQQf", hog, 0, hw, hw + 4 * m.hog_weights.size, hw + 4 * m.hog_weights.size, m.hog_threshold)
        self.hog = e.put(bytes(hog))
        self.L = L
        self.mark = e.top

    def ferns(self, patch24_img: np.ndarray, px: int, py: int):
        """Fern cascade on a half-resolution padded layer image, patch origin (px, py). Returns (passed, n_passed, score)."""
        e = self.emu
        e.top = self.mark
        sub = patch24_img[py:py + 24, px:px + 24]
        p, sh = put_image(e, sub)
        score = e.put(struct.pack("<i", -25453782))
        cnt = e.put(struct.pack("<i", 0))
        tab = e.put(struct.pack("<Q", FERN_WEIGHTS))
        for fn in FERN_PREDICT:
            r = e.call(fn, [0, p, sh, score, cnt, 102400, tab]) & 0xFFFFFFFF
            if r != 1:
                return False, struct.unpack("<i", e.read(cnt, 4))[0], struct.unpack("<i", e.read(score, 4))[0]
        return True, struct.unpack("<i", e.read(cnt, 4))[0], struct.unpack("<i", e.read(score, 4))[0]

    def jda(self, full_win: np.ndarray, half_win: np.ndarray, quarter_win: np.ndarray):
        """Window crops: full 96x96, half 48x48, quarter 24x24 (origins as in DetectInLayer)."""
        e = self.emu
        e.top = self.mark
        pf, s0 = put_image(e, full_win); ph, s1 = put_image(e, half_win); pq, s2 = put_image(e, quarter_win)
        leaf = e.alloc(4 * 1024); a9 = e.alloc(4 * 64); dst = e.alloc(4 * 64); score = e.put(struct.pack("<i", 0))
        e.uc.mem_write(a9, b"\0" * 256); e.uc.mem_write(dst, b"\0" * 256)
        r = e.call(PREDICT[self.kind], [self.obj, s0, s1, s2, pf, ph, pq, leaf, a9, dst, score]) & 0xFFFFFFFF
        sc = struct.unpack("<i", e.read(score, 4))[0]
        shape = np.frombuffer(e.read(dst, 8 * self.L), "<f4").reshape(self.L, 2).copy()
        return r == 1, sc, shape

    def post(self, full_win: np.ndarray, shape: np.ndarray, jda_score: float):
        e = self.emu
        e.top = self.mark
        pf, s0 = put_image(e, full_win)
        sh = e.put(shape.astype("<f4").tobytes())
        out = e.put(struct.pack("<f", jda_score))
        r = e.call(HOG_PREDICT, [self.hog, 96, 96, 1 << s0, pf, self.L, sh, out]) & 0xFF
        return struct.unpack("<f", e.read(out, 4))[0], bool(r)
