#!/usr/bin/env python3
"""Reconstruct the Windows 11 Hello eye (`AnimatedVisuals::HelloSolidAnimations`, LottieGen output compiled
into Windows.UI.Logon.dll) as data: docs/windows-hello-ui.md §1.3 and §6.

Two phases, both reproducible:

1. `dump`: open the IDA database (idalib) and write every instruction of every `HelloSolidAnimations*` function
   to a JSON file (mnemonic, operands, the 16 bytes behind constant operands, string literals).
2. `extract`: emulate `HelloSolidAnimations_AnimatedVisual_UAPv15`'s constructor with a symbolic x64
   interpreter (the code is straight-line LottieGen output: object factories, property setters, key frames)
   and write the composition tree, geometries, animations and markers to `solid.json`.

    python3 tools/winhello_ui/lottie_extract.py dump  <Windows.UI.Logon.dll.i64> <funcs.json>
    python3 tools/winhello_ui/lottie_extract.py extract <funcs.json> <winrt-impl-dir> <solid.json>

`<winrt-impl-dir>` is a C++/WinRT `winrt/impl` directory (Windows.UI.Composition.0.h gives the vtable order
of every interface; the public PDB folds identical thunks so their names cannot be trusted, only slots).
"""
import json
import re
import struct
import sys


# ---------------------------------------------------------------- phase 1: dump (needs idalib)

def dump(idb, out):
    import idapro
    if idapro.open_database(idb, False) != 0:
        sys.exit(f"cannot open {idb}")
    import ida_bytes, ida_funcs, ida_idp, ida_name, ida_nalt, ida_ua, idautils, idc

    def name_at(ea):
        return ida_name.get_demangled_name(ea, 0, 0, 0) or ida_name.get_name(ea)

    funcs = [ea for ea in idautils.Functions() if 'HelloSolidAnimations' in (name_at(ea) or '')]
    result = {}
    thunks = {}
    for fea in funcs:
        f = ida_funcs.get_func(fea)
        ins = []
        for ea in idautils.FuncItems(fea):
            insn = ida_ua.insn_t()
            if not ida_ua.decode_insn(insn, ea):
                continue
            ops = []
            for op in insn.ops:
                if op.type == ida_ua.o_void:
                    break
                d = {"t": op.type, "dt": op.dtype}
                if op.type == ida_ua.o_reg:
                    d["reg"] = ida_idp.get_reg_name(op.reg, {ida_ua.dt_dword: 4, ida_ua.dt_word: 2, ida_ua.dt_byte: 1, ida_ua.dt_byte16: 16}.get(op.dtype, 8))
                elif op.type == ida_ua.o_imm:
                    d["v"] = op.value
                elif op.type in (ida_ua.o_mem, ida_ua.o_near, ida_ua.o_far):
                    d["addr"] = op.addr
                    d["name"] = name_at(op.addr)
                    if op.type == ida_ua.o_mem:
                        s = ida_bytes.get_strlit_contents(op.addr, -1, ida_nalt.STRTYPE_C_16)
                        if s is not None and len(s) < 200:
                            d["str"] = s.decode("utf-8", "replace")
                        d["bytes"] = ida_bytes.get_bytes(op.addr, 16).hex()
                elif op.type in (ida_ua.o_displ, ida_ua.o_phrase):
                    d["reg"] = ida_idp.get_reg_name(op.reg, 8)
                    d["displ"] = op.addr
                ops.append(d)
            ins.append({"ea": ea, "m": insn.get_canon_mnem(), "ops": ops, "txt": idc.GetDisasm(ea)})
            if insn.get_canon_mnem() == 'call' and ops and ops[0]['t'] in (ida_ua.o_near, ida_ua.o_far):
                thunks.setdefault(ops[0]['addr'], None)
        result[fea] = {"name": name_at(fea), "ins": ins}
    # the external call targets: thunk bodies (slot number of the virtual call they make)
    for tea in list(thunks):
        if tea in result:
            del thunks[tea]
            continue
        lines, cur = [], tea
        for _ in range(12):
            lines.append(idc.GetDisasm(cur))
            if idc.print_insn_mnem(cur) in ('jmp', 'retn'):
                break
            cur = idc.next_head(cur)
        thunks[tea] = {"name": name_at(tea), "lines": lines}
    json.dump({"functions": result, "thunks": thunks}, open(out, "w"))
    idapro.close_database(False)


# ---------------------------------------------------------------- phase 2: emulate

IVECTOR = {6: 'GetAt', 7: 'get_Size', 8: 'GetView', 9: 'IndexOf', 10: 'SetAt', 11: 'InsertAt', 12: 'RemoveAt',
           13: 'Append', 14: 'RemoveAtEnd', 15: 'Clear', 16: 'GetMany', 17: 'ReplaceAll'}
# d2d1.h vtables (IUnknown 0-2, ID2D1Resource 3, ID2D1Geometry 4-16; ID2D1SimplifiedGeometrySink 3-9)
D2D_FACTORY = {8: 'CreateGeometryGroup', 10: 'CreatePathGeometry'}
D2D_PATHGEOM = {17: 'Open'}
D2D_SINK = {3: 'SetFillMode', 4: 'SetSegmentFlags', 5: 'BeginFigure', 6: 'AddLines', 7: 'AddBeziers', 8: 'EndFigure',
            9: 'Close', 10: 'AddLine', 11: 'AddBezier', 12: 'AddQuadraticBezier', 13: 'AddQuadraticBeziers', 14: 'AddArc'}

ALIAS = {'eax': 'rax', 'ecx': 'rcx', 'edx': 'rdx', 'ebx': 'rbx', 'esp': 'rsp', 'ebp': 'rbp', 'esi': 'rsi', 'edi': 'rdi',
         'al': 'rax', 'cl': 'rcx', 'dl': 'rdx', 'bl': 'rbx', 'sil': 'rsi', 'dil': 'rdi', 'bpl': 'rbp', 'spl': 'rsp'}
for _i in range(8, 16):
    for _suf in ('d', 'w', 'b'):
        ALIAS[f'r{_i}{_suf}'] = f'r{_i}'


def norm_reg(r):
    return ALIAS.get(r, r)


def f32(bits):
    return ('f32', bits & 0xffffffff)


def f32_to_float(v):
    if isinstance(v, tuple) and v[0] == 'f32':
        return struct.unpack('<f', struct.pack('<I', v[1]))[0]
    if isinstance(v, int):
        return struct.unpack('<f', struct.pack('<I', v & 0xffffffff))[0]
    raise ValueError(v)


class WinRTVtables:
    """vtable order of the Windows.UI.Composition interfaces, from the C++/WinRT impl headers."""

    def __init__(self, impl_dir):
        self.tables = {}
        for fn in ('Windows.UI.Composition.0.h', 'Windows.Graphics.Effects.0.h', 'Windows.UI.Composition.Effects.0.h'):
            try:
                text = open(f'{impl_dir}/{fn}').read()
            except FileNotFoundError:
                continue
            for m in re.finditer(r'template <> struct abi<winrt::([\w:]+)>\s*\{\s*struct WINRT_IMPL_NOVTABLE type : (\w+)\s*\{(.*?)\};\s*\};', text, re.S):
                iface, base, body = m.groups()
                methods = re.findall(r'virtual int32_t __stdcall (\w+)\((.*?)\) noexcept = 0;', body)
                first = {'inspectable_abi': 6, 'unknown_abi': 3}[base]
                self.tables[iface.split('::')[-1]] = {first + i: (n, [p.strip() for p in a.split(',') if p.strip()]) for i, (n, a) in enumerate(methods)}

    def method(self, iface, slot):
        if iface is None:
            return None
        if iface == 'IVector':
            return IVECTOR.get(slot)
        if iface == 'ID2D1Factory':
            return D2D_FACTORY.get(slot)
        if iface == 'ID2D1PathGeometry':
            return D2D_PATHGEOM.get(slot)
        if iface == 'ID2D1GeometrySink':
            return D2D_SINK.get(slot)
        t = self.tables.get(iface)
        return t[slot][0] if t and slot in t else None

    def params(self, iface, meth):
        for s, (n, a) in (self.tables.get(iface) or {}).items():
            if n == meth:
                return a
        return None


def iface_from_uuid_name(nm):
    # "__uuidof_?AUICompositor5@Composition@UI@Windows@@" (AU = interface) / "?AVCompositionShape@..." (AV = class)
    m = re.match(r'__uuidof_\?A([UV])(?:\?\$)?(\w+)@', nm)
    if not m:
        return None
    kind, name = m.groups()
    return name if kind == 'U' else 'I' + name


def thunk_slot(lines):
    for l in lines:
        m = re.search(r'mov\s+rax, \[rax\+([0-9A-Fa-f]+)h?\]', l)
        if m:
            return int(m.group(1), 16) // 8
    return None


class Obj:
    """one composition object (or D2D geometry) the generated code created"""
    _n = 0

    def __init__(self, kind, creator=None):
        Obj._n += 1
        self.id = Obj._n
        self.kind = kind
        self.creator = creator
        self.props = {}
        self.anims = []
        self.children = []

    def __repr__(self):
        return f"<{self.kind}#{self.id} {self.creator}>"


DEFAULT_IFACE = {
    'ContainerShape': 'ICompositionContainerShape', 'SpriteShape': 'ICompositionSpriteShape', 'ShapeVisual': 'IShapeVisual',
    'ContainerVisual': 'IContainerVisual', 'SpriteVisual': 'ISpriteVisual', 'PathGeometry': 'ICompositionPathGeometry',
    'EllipseGeometry': 'ICompositionEllipseGeometry', 'RectangleGeometry': 'ICompositionRectangleGeometry',
    'RoundedRectangleGeometry': 'ICompositionRoundedRectangleGeometry', 'ScalarKeyFrameAnimation': 'IScalarKeyFrameAnimation',
    'Vector2KeyFrameAnimation': 'IVector2KeyFrameAnimation', 'Vector3KeyFrameAnimation': 'IVector3KeyFrameAnimation',
    'ColorKeyFrameAnimation': 'IColorKeyFrameAnimation', 'PathKeyFrameAnimation': 'IPathKeyFrameAnimation',
    'BooleanKeyFrameAnimation': 'IBooleanKeyFrameAnimation', 'ExpressionAnimation': 'IExpressionAnimation',
    'CubicBezierEasingFunction': 'ICubicBezierEasingFunction', 'StepEasingFunction': 'IStepEasingFunction',
    'LinearEasingFunction': 'ILinearEasingFunction', 'ColorBrush': 'ICompositionColorBrush',
    'SurfaceBrush': 'ICompositionSurfaceBrush', 'EffectBrush': 'ICompositionEffectBrush',
    'EffectFactory': 'ICompositionEffectFactory', 'VisualSurface': 'ICompositionVisualSurface',
    'PropertySet': 'ICompositionPropertySet', 'InsetClip': 'IInsetClip', 'GeometricClip': 'ICompositionGeometricClip',
    'ViewBox': 'ICompositionViewBox', 'LinearGradientBrush': 'ICompositionLinearGradientBrush',
    'ColorGradientStop': 'ICompositionColorGradientStop', 'AnimationController': 'IAnimationController',
    'Children': 'IVector', 'ColorStops': 'IVector', 'D2DFactory': 'ID2D1Factory', 'D2DPathGeometry': 'ID2D1PathGeometry',
    'D2DSink': 'ID2D1GeometrySink', 'Compositor': 'ICompositor', 'ThemeProperties': 'ICompositionPropertySet',
    'CompositionPath': 'ICompositionPath', 'EffectSourceParameter': 'ICompositionEffectSourceParameter',
}


def ptr(obj, iface=None):
    return ('ptr', obj, iface or DEFAULT_IFACE.get(obj.kind))


class EmuError(Exception):
    pass


XMM_CONST_RE = re.compile(r'__xmm@([0-9a-f]{32})')
REAL_RE = re.compile(r'__real@([0-9a-f]{8})$')
# o_reg=1, o_mem=2, o_phrase=3, o_displ=4, o_imm=5, o_far=6, o_near=7
DT_SIZE = {0: 1, 1: 2, 2: 4, 3: 8, 4: 16, 7: 8}


class Emu:
    """Symbolic x64 interpreter for LottieGen's straight-line code.

    Values: int (immediate), ('f32', bits), ('sp', off) stack address, ('this', off) field address,
    ('ptr', Obj, iface) object pointer, ('str', s) string literal, ('xmm', [lane0..3]), ('pack', [lo, hi])
    (two floats packed into a GPR = a float2 by value), ('vtbl', ptr), ('slot', ptr, n). Conditional jumps are
    ignored: the only branches skip Release() on null pointers or raise exceptions."""

    def __init__(self, functions, thunks, vt):
        self.F = functions
        self.vt = vt
        self.thunk_slots = {int(k): (v['name'], thunk_slot(v['lines'])) for k, v in thunks.items()}
        self.by_name = {v['name']: k for k, v in functions.items()}
        self.fields = {}
        self.stack = {}
        self.objs = []
        self.warnings = []
        self.cur_name = '?'
        self.depth = 0
        self.callstack = []

    # ---------- memory ----------
    def addr_of(self, op, regs):
        if op['t'] in (3, 4):
            base = regs.get(norm_reg(op['reg']))
            d = op.get('displ', 0)
            if d >= 2**63:
                d -= 2**64
            if isinstance(base, tuple) and base[0] in ('sp', 'this'):
                return (base[0], base[1] + d)
            if isinstance(base, tuple) and base[0] == 'ptr':
                return ('objmem', base, d)
            if isinstance(base, tuple) and base[0] == 'vtbl':
                return ('vtblmem', base[1], d)
            return ('unk', base, d)
        if op['t'] == 2:
            return ('mem', op['addr'], op)
        raise EmuError(f'addr_of {op}')

    def load(self, addr, size):
        kind = addr[0]
        if kind == 'sp':
            e = self.stack.get(addr[1])
            if e is None:
                # the second float of a packed float2 stored as one 8-byte value
                below = self.stack.get(addr[1] - 4)
                if below and below[0] == 8 and isinstance(below[1], tuple) and below[1][0] == 'pack':
                    return below[1][1][1]
                return None
            if size == 8 and e[0] == 4:
                nxt = self.stack.get(addr[1] + 4)
                return ('pack', [e[1], nxt[1] if nxt else None])
            return e[1]
        if kind == 'unk' and (addr[1] is None or isinstance(addr[1], int)):
            return None  # a load through a null pointer: the skipped side of a null check
        if kind == 'this':
            return self.fields.get(addr[1], ('field', addr[1]))
        if kind == 'objmem':
            p, d = addr[1], addr[2]
            if d == 0:
                return ('vtbl', p)
            return getattr(p[1], 'mem', {}).get(d)
        if kind == 'vtblmem':
            return ('slot', addr[1], addr[2] // 8)
        if kind == 'mem':
            op = addr[2]
            nm = op.get('name') or ''
            raw = bytes.fromhex(op['bytes']) if 'bytes' in op else None
            m = XMM_CONST_RE.search(nm)
            if m or (size == 16 and raw):
                return ('xmm', [f32(struct.unpack_from('<I', raw, 4 * k)[0]) for k in range(4)])
            if REAL_RE.search(nm) or (size in (4, 8) and raw and 'str' not in op):
                if size == 8 and not REAL_RE.search(nm):
                    return ('xmm', [f32(struct.unpack_from('<I', raw, 0)[0]), f32(struct.unpack_from('<I', raw, 4)[0]), None, None])
                return f32(struct.unpack_from('<I', raw, 0)[0])
            if 'str' in op:
                return ('str', op['str'])
            return ('mem', nm)
        raise EmuError(f'load {addr}')

    def store(self, addr, size, val):
        kind = addr[0]
        if kind == 'sp':
            self.stack[addr[1]] = (size, val)
        elif kind == 'this':
            self.fields[addr[1]] = val
        elif kind == 'objmem':
            o = addr[1][1]
            if not hasattr(o, 'mem'):
                o.mem = {}
            o.mem[addr[2]] = val
        elif kind == 'unk':
            self.warnings.append(f'{self.cur_name}: store through unknown pointer {addr}')
        else:
            raise EmuError(f'store {addr}')

    # ---------- operands ----------
    def rd(self, op, regs, size=None):
        t = op['t']
        if t == 1:
            r = op['reg']
            if r.startswith('xmm'):
                return regs.get(r, ('xmm', [None] * 4))
            return regs.get(norm_reg(r))
        if t == 5:
            return op['v']
        if t in (2, 3, 4):
            return self.load(self.addr_of(op, regs), size or DT_SIZE.get(op['dt'], 8))
        if t in (6, 7):
            return ('code', op['addr'])
        raise EmuError(f'rd {op}')

    def wr(self, op, regs, val):
        t = op['t']
        if t == 1:
            r = op['reg']
            regs[r if r.startswith('xmm') else norm_reg(r)] = val
        elif t in (2, 3, 4):
            self.store(self.addr_of(op, regs), DT_SIZE.get(op['dt'], 8), val)
        else:
            raise EmuError(f'wr {op}')

    def lea(self, op, regs):
        a = self.addr_of(op, regs)
        if a[0] == 'unk' and isinstance(a[1], int):
            return a[1] + a[2]
        if a[0] == 'mem':
            nm = op.get('name') or ''
            if 'str' in op:
                return ('str', op['str'])
            if nm.startswith('__uuidof_'):
                return ('uuid', iface_from_uuid_name(nm))
            return ('mem', nm)
        return a

    def lanes(self, v):
        if isinstance(v, tuple) and v[0] == 'xmm':
            return list(v[1])
        if isinstance(v, tuple) and v[0] == 'pack':
            return [v[1][0], v[1][1], None, None]
        return [v, None, None, None]

    def mem_lanes(self, op, regs, n):
        a = self.addr_of(op, regs)
        if a[0] in ('sp', 'this'):
            return [self.load((a[0], a[1] + 4 * k), 4) for k in range(n)] + [None] * (4 - n)
        v = self.load(a, 4 * n)
        return self.lanes(v)

    def store_lanes(self, op, regs, lanes, n):
        a = self.addr_of(op, regs)
        if a[0] in ('sp', 'this'):
            for k in range(n):
                self.store((a[0], a[1] + 4 * k), 4, lanes[k])
        elif n == 2:
            self.store(a, 8, ('pack', lanes[:2]))
        else:
            self.store(a, 4 * n, ('xmm', lanes))

    # ---------- run ----------
    def run(self, fea, regs):
        f = self.F[fea]
        prev_name, self.cur_name = self.cur_name, f['name'].split('::')[-1]
        self.depth += 1
        self.callstack.append(self.cur_name)
        regs = dict(regs)
        regs['rsp'] = ('sp', regs['rsp'][1] - 8)  # the return address
        ins = f['ins']
        ret = None
        i = 0
        while i < len(ins):
            x = ins[i]
            m, ops = x['m'], x['ops']
            try:
                if m == 'retn':
                    ret = regs.get('rax')
                    break
                self.step(m, ops, regs, x)
            except EmuError as e:
                if ' @ ' in str(e):
                    raise
                raise EmuError(f"{f['name'].split('::')[-1]} @ {x['ea']:x} {x['txt']}: {e}") from None
            except Exception as e:
                raise EmuError(f"{f['name'].split('::')[-1]} @ {x['ea']:x} {x['txt']}: {type(e).__name__} {e}") from e
            i += 1
        for y in ins[i + 1:]:
            if y['m'] == 'call':
                nm = y['ops'][0].get('name') or ''
                if 'raiseException' not in nm and 'RoFailFast' not in nm:
                    self.warnings.append(f"{f['name']}: code after retn: {y['txt']}")
        self.depth -= 1
        self.callstack.pop()
        self.cur_name = prev_name
        return ret

    def step(self, m, ops, regs, x):
        if m == 'push':
            v = self.rd(ops[0], regs)
            regs['rsp'] = ('sp', regs['rsp'][1] - 8)
            self.store(regs['rsp'], 8, v)
        elif m == 'pop':
            v = self.load(regs['rsp'], 8)
            regs['rsp'] = ('sp', regs['rsp'][1] + 8)
            self.wr(ops[0], regs, v)
        elif m in ('sub', 'add') and ops[1]['t'] == 5:
            k = ops[1]['v']
            if k >= 2**63:
                k -= 2**64
            if m == 'sub':
                k = -k
            v = self.rd(ops[0], regs)
            if isinstance(v, tuple) and v[0] in ('sp', 'this'):
                self.wr(ops[0], regs, (v[0], v[1] + k))
            elif isinstance(v, int):
                self.wr(ops[0], regs, v + k)
            else:
                self.wr(ops[0], regs, None)
        elif m == 'mov':
            if not (ops[0]['t'] == 1 and ops[1]['t'] == 1 and norm_reg(ops[0]['reg']) == norm_reg(ops[1]['reg'])):
                self.wr(ops[0], regs, self.rd(ops[1], regs, DT_SIZE.get(ops[0]['dt'], 8)))
        elif m == 'lea':
            self.wr(ops[0], regs, self.lea(ops[1], regs))
        elif m in ('movss', 'movsd'):
            n = 1 if m == 'movss' else 2
            if ops[0]['t'] == 1 and ops[1]['t'] == 1:
                d = self.lanes(self.rd(ops[0], regs))
                s = self.lanes(self.rd(ops[1], regs))
                d[:n] = s[:n]
                regs[ops[0]['reg']] = ('xmm', d)
            elif ops[0]['t'] == 1:
                l = self.mem_lanes(ops[1], regs, n)
                regs[ops[0]['reg']] = ('xmm', l[:n] + [None] * (4 - n))
            else:
                self.store_lanes(ops[0], regs, self.lanes(self.rd(ops[1], regs)), n)
        elif m in ('movaps', 'movups'):
            if ops[0]['t'] == 1 and ops[1]['t'] == 1:
                regs[ops[0]['reg']] = self.rd(ops[1], regs)
            elif ops[0]['t'] == 1:
                regs[ops[0]['reg']] = ('xmm', self.mem_lanes(ops[1], regs, 4))
            else:
                self.store_lanes(ops[0], regs, self.lanes(self.rd(ops[1], regs)), 4)
        elif m == 'movq':
            if ops[0]['t'] == 1 and ops[0]['reg'].startswith('xmm'):
                v = self.rd(ops[1], regs, 8) if ops[1]['t'] != 1 else self.rd(ops[1], regs)
                regs[ops[0]['reg']] = ('xmm', self.lanes(v)[:2] + [None, None])
            else:
                s = self.lanes(self.rd(ops[1], regs))
                self.wr(ops[0], regs, ('pack', s[:2]))
        elif m == 'movd':
            if ops[0]['t'] == 1 and ops[0]['reg'].startswith('xmm'):
                regs[ops[0]['reg']] = ('xmm', [self.rd(ops[1], regs, 4), None, None, None])
            else:
                self.wr(ops[0], regs, self.lanes(self.rd(ops[1], regs))[0])
        elif m == 'xorps':
            if ops[0]['reg'] != ops[1]['reg']:
                raise EmuError('xorps of two registers')
            regs[ops[0]['reg']] = ('xmm', [f32(0)] * 4)
        elif m == 'unpcklps':
            d = self.lanes(self.rd(ops[0], regs))
            s = self.lanes(self.rd(ops[1], regs))
            regs[ops[0]['reg']] = ('xmm', [d[0], s[0], d[1], s[1]])
        elif m == 'xor':
            if ops[0]['t'] == 1 and ops[1]['t'] == 1 and norm_reg(ops[0]['reg']) == norm_reg(ops[1]['reg']):
                self.wr(ops[0], regs, 0)
        elif m == 'and':
            if ops[1]['t'] == 5 and ops[1]['v'] == 0:
                self.wr(ops[0], regs, 0)
        elif m == 'or':
            if ops[1]['t'] == 5:
                self.wr(ops[0], regs, ops[1]['v'])
        elif m in ('test', 'cmp', 'jz', 'jnz', 'js', 'jns', 'nop', 'inc'):
            pass
        elif m == 'call':
            self.do_call(x, regs)
        else:
            raise EmuError(f"unhandled {m}: {x['txt']}")

    # ---------- calls ----------
    def do_call(self, x, regs):
        op = x['ops'][0]
        tgt = op.get('addr')
        nm = op.get('name') or ''
        if tgt in self.F:
            regs['rax'] = self.run(tgt, regs)
            return
        self.ext_call(nm, tgt, regs, x)

    def ext_call(self, nm, tgt, regs, x):
        rcx, rdx, r8, r9 = regs.get('rcx'), regs.get('rdx'), regs.get('r8'), regs.get('r9')
        if nm.startswith('?__abi_winrt_cast_to'):
            if isinstance(rdx, tuple) and rdx[0] == 'ptr':
                iface = r8[1] if isinstance(r8, tuple) and r8[0] == 'uuid' else None
                regs['rax'] = ('ptr', rdx[1], iface)
            else:
                regs['rax'] = rdx
        elif nm.startswith('?__abi_winrt_ptr_ctor'):
            regs['rax'] = rcx
        elif nm.startswith('?__abi_winrt_ptr_assign'):
            self.store(rcx, 8, rdx)
            regs['rax'] = rdx
        elif nm.startswith('WindowsCreateStringReference'):
            self.store(r9, 8, rcx)
            regs['rax'] = 0
        elif nm.startswith('??0float2@'):
            self.store((rcx[0], rcx[1]), 4, self.lanes(regs['xmm1'])[0])
            self.store((rcx[0], rcx[1] + 4), 4, self.lanes(regs['xmm2'])[0])
            regs['rax'] = rcx
        elif nm.startswith('??0float3@'):
            for k, xr in enumerate(['xmm1', 'xmm2', 'xmm3']):
                self.store((rcx[0], rcx[1] + 4 * k), 4, self.lanes(regs[xr])[0])
            regs['rax'] = rcx
        elif nm.startswith('??0float3x2@') or nm.startswith('??0float4x4@'):
            n = 6 if 'float3x2' in nm else 16
            vals = [self.lanes(regs[xr])[0] for xr in ('xmm1', 'xmm2', 'xmm3')]
            sp = regs['rsp'][1]
            vals += [self.load(('sp', sp + 0x20 + 8 * k), 4) for k in range(n - 3)]
            for k, v in enumerate(vals):
                self.store((rcx[0], rcx[1] + 4 * k), 4, v)
            regs['rax'] = rcx
        elif nm.startswith('??2@'):
            regs['rax'] = self.new('Heap', size=rcx)  # operator new: LottieGen's CompositeEffect / GeoSource
        elif "GeoSource::GeoSource" in nm:
            regs['rax'] = rdx  # GeoSource(mem, ID2D1PathGeometry*): the wrapper is the geometry
        elif "GeoSource>::operator=_0" in nm:
            self.store(rcx, 8, rdx)
            regs['rax'] = rdx
        elif "GeoSource>::operator=" in nm:
            v = self.load(rdx, 8) if isinstance(rdx, tuple) and rdx[0] in ('sp', 'this') else rdx
            self.store(rcx, 8, v)
            regs['rax'] = rcx
        elif "GeoSource::GetGeometry" in nm:
            # GeoSource::GetGeometry(this = &GeoSource::m_geometry, out): the wrapper is the geometry itself here
            self.store(rdx, 8, rcx[1] if isinstance(rcx, tuple) and rcx[0] == 'objmem' else rcx)
            regs['rax'] = 0
        elif 'InternalRelease' in nm or nm.endswith('::Release'):
            pass
        elif 'CanvasGeometryToIGeometrySource2D' in nm:
            regs['rax'] = rcx
        elif nm.startswith('??0CompositionPath@'):
            regs['rax'] = self.new('CompositionPath', geometry=rdx)
        elif nm.startswith('??0CompositionEffectSourceParameter@'):
            regs['rax'] = self.new('EffectSourceParameter', name=rdx)
        elif 'D2D1CreateFactory' in nm:
            self.store(r9, 8, ptr(Obj('D2DFactory')))
            regs['rax'] = 0
        elif nm.startswith('`eh vector') or nm.startswith('__security_check_cookie'):
            pass  # keep rax: these do not return a value the generated code uses
        elif (nm.startswith('RoFailFast') or nm.startswith('?__abi_WinRTraiseException')
              or nm.startswith('__imp_??0Object') or 'FTMWeakRefData' in nm or nm.startswith('??_L') or nm.startswith('??_M')):
            regs['rax'] = 0
        elif nm.startswith('_guard_dispatch_icall'):
            rax = regs.get('rax')
            if rax is None:
                self.warnings.append(f"{self.cur_name} @ {x['ea']:x}: indirect call through a null pointer (skipped side of a null check)")
                return
            if not (isinstance(rax, tuple) and rax[0] == 'slot'):
                raise EmuError(f'{self.cur_name}: indirect call through {rax}')
            p, slot = rax[1], rax[2]
            if slot in (1, 2):  # AddRef / Release
                return
            iface = p[2] if isinstance(p, tuple) else None
            meth = self.vt.method(iface, slot)
            if meth is None:
                raise EmuError(f'{self.cur_name}: slot {slot} of {iface} ({p})')
            self.method_call(p, iface, meth, regs)
        elif nm.startswith("`anonymous namespace'::CompositeEffect::AddSource"):
            self.method_call(rcx, 'CompositeEffect', 'AddSource', regs)
        elif tgt in self.thunk_slots and self.thunk_slots[tgt][1] is not None and not nm.startswith(('?set@?Q', '?get@?Q')):
            slot = self.thunk_slots[tgt][1]
            iface = rcx[2] if isinstance(rcx, tuple) and rcx[0] == 'ptr' else None
            meth = self.vt.method(iface, slot)
            if meth is None:
                raise EmuError(f'{self.cur_name}: thunk {nm[:60]} slot {slot} of {iface} ({rcx})')
            self.method_call(rcx, iface, meth, regs)
        else:
            m = re.match(r'\?(set|get)@\?Q(I\w+)@Composition@UI@Windows@@(\w+)@', nm)
            if m:
                kind, iface, prop = m.groups()
                self.method_call(rcx, iface, ('put_' if kind == 'set' else 'get_') + prop, regs)
                return
            m = re.match(r'\?(\w+)@\?Q(I\w+)@Composition@UI@Windows@@', nm)
            if m:
                meth, iface = m.groups()
                self.method_call(rcx, iface, meth, regs)
                return
            raise EmuError(f'{self.cur_name}: external call {nm[:80]}')

    HELPERS = ('Create', 'Bind', 'CanvasGeometry')

    def new(self, kind, **props):
        # attribute the object to the generated LottieGen function, not to the shared helper it called
        creator = next((n for n in reversed(self.callstack) if not n.startswith(self.HELPERS)), self.cur_name)
        o = Obj(kind, creator)
        o.props.update(props)
        self.objs.append(o)
        return ptr(o)

    def method_call(self, p, iface, meth, regs):
        if not (isinstance(p, tuple) and p[0] == 'ptr'):
            raise EmuError(f'{self.cur_name}: {meth} on {p}')
        sp = regs['rsp'][1]
        a = {k: regs.get(k) for k in ('rdx', 'r8', 'r9')}
        a['x1'], a['x2'], a['x3'] = (self.lanes(regs.get(k, ('xmm', [None] * 4)))[0] for k in ('xmm1', 'xmm2', 'xmm3'))
        a['stack'] = [self.load(('sp', sp + 0x20 + 8 * k), 8) for k in range(4)]
        ret = self.semantics(p[1], iface, meth, a)
        regs['rax'] = 0 if ret is None else ret

    def materialize(self, ptype, v):
        """read struct arguments passed by pointer now, before the stack slot is reused"""
        n = {'float3': 3, 'float3x2': 6, 'float4x4': 16}.get(ptype.split('::')[-1])
        if n and isinstance(v, tuple) and v[0] in ('sp', 'this'):
            return ('floats', [self.load((v[0], v[1] + 4 * k), 4) for k in range(n)])
        if ptype.endswith('float2') and isinstance(v, tuple) and v[0] == 'pack':
            return ('floats', list(v[1]))
        return v

    def semantics(self, obj, iface, meth, a):
        rdx, r8, r9, x1, x2 = a['rdx'], a['r8'], a['r9'], a['x1'], a['x2']
        new = self.new
        if meth.startswith('Create') and iface.startswith('ICompositor'):
            k = meth[6:]
            if k.endswith('WithGeometry'):
                return new(k[:-12], geometry=rdx)
            if k.endswith('WithPath'):
                return new(k[:-8], path=rdx)
            if k.endswith('WithSurface'):
                return new(k[:-11], surface=rdx)
            if k.endswith('WithColor'):
                return new(k[:-9], color=rdx)
            if k == 'CubicBezierEasingFunction':
                return new(k, cp1=rdx, cp2=r8)
            if k == 'EffectFactory':
                return new(k, effect=rdx)
            if k.startswith('ColorGradientStop'):
                return new('ColorGradientStop', offset=x1, color=r8)
            if k == 'InsetClipWithInsets':
                return new('InsetClip', insets=(x1, x2, a['x3'], a['stack'][0]))
            return new(k)
        if iface == 'ICompositionEffectFactory' and meth == 'CreateBrush':
            return new('EffectBrush', factory=obj)
        if meth in ('Pause', 'Resume'):
            obj.props[meth.lower() + 'd'] = True
            return 0
        if meth == 'SetSourceParameter':
            obj.props.setdefault('sources', []).append((rdx, r8))
            return 0
        if iface == 'ID2D1Factory' and meth == 'CreatePathGeometry':
            g = new('D2DPathGeometry')
            g[1].figures = []
            self.store(rdx, 8, g)
            return 0
        if iface == 'ID2D1Factory' and meth == 'CreateGeometryGroup':
            g = new('D2DGeometryGroup', fill_mode=rdx)
            g[1].children = [self.load((r8[0], r8[1] + 8 * k), 8) for k in range(r9)]
            self.store(a['stack'][0], 8, g)
            return 0
        if iface == 'ID2D1PathGeometry' and meth == 'Open':
            s = new('D2DSink')
            s[1].geom = obj
            self.store(rdx, 8, s)
            return 0
        if iface == 'ID2D1GeometrySink':
            g = obj.geom
            if meth == 'SetFillMode':
                g.props['fill_mode'] = rdx
            elif meth == 'BeginFigure':
                g.figures.append({'start': rdx, 'begin': r8, 'segments': [], 'end': None})
            elif meth == 'AddBezier':
                g.figures[-1]['segments'].append(('C', [self.load((rdx[0], rdx[1] + 4 * k), 4) for k in range(6)]))
            elif meth == 'AddBeziers':
                vals = [self.load((rdx[0], rdx[1] + 4 * k), 4) for k in range(6 * r8)]
                for k in range(r8):
                    g.figures[-1]['segments'].append(('C', vals[6 * k:6 * k + 6]))
            elif meth == 'AddLine':
                g.figures[-1]['segments'].append(('L', self.lanes(rdx)[:2]))
            elif meth == 'AddLines':
                vals = [self.load((rdx[0], rdx[1] + 4 * k), 4) for k in range(2 * r8)]
                for k in range(r8):
                    g.figures[-1]['segments'].append(('L', vals[2 * k:2 * k + 2]))
            elif meth == 'EndFigure':
                g.figures[-1]['end'] = rdx
            elif meth == 'Close':
                pass
            else:
                raise EmuError(f'{self.cur_name}: sink {meth}')
            return 0
        if meth in ('get_Children', 'get_Shapes', 'get_ColorStops'):
            key = '_' + meth[4:].lower()
            ch = obj.props.get(key)
            if ch is None:
                ch = Obj('Children', self.cur_name)
                ch.owner = obj
                obj.props[key] = ch
            return ptr(ch, 'IVisualCollection' if meth == 'get_Children' else 'IVector')
        if meth == 'get_Properties':
            ps = obj.props.get('_propertyset')
            if ps is None:
                ps = Obj('PropertySet', self.cur_name)
                ps.owner = obj
                obj.props['_propertyset'] = ps
            return ptr(ps)
        if meth.startswith('get_'):
            raise EmuError(f'{self.cur_name}: getter {iface}.{meth}')
        if meth in ('Append', 'InsertAtTop'):
            obj.owner.children.append(rdx)
            return 0
        if meth == 'InsertScalar':
            obj.props.setdefault('scalars', []).append((rdx, x2))
            return 0
        if meth in ('StartAnimation', 'StartAnimationWithController'):
            entry = {'property': rdx, 'animation': r8, 'controller': r9 if meth.endswith('Controller') else None}
            anim = r8[1] if isinstance(r8, tuple) and r8[0] == 'ptr' else None
            if anim is not None and anim.kind == 'ExpressionAnimation':
                # the one reusable ExpressionAnimation is re-targeted for every binding: snapshot it
                entry['expression'] = anim.props.get('Expression', (None, None))[1]
                entry['refs'] = list(anim.props.get('refs', []))
            obj.anims.append(entry)
            return 0
        if meth in ('InsertKeyFrame', 'InsertKeyFrameWithEasingFunction'):
            # (float progress, T value[, easing]): the value is in xmm2 when T is float, else in r8
            easing = r9 if meth.endswith('Function') else None
            if iface == 'IScalarKeyFrameAnimation':
                value = x2
            elif iface == 'IBooleanKeyFrameAnimation':
                value = r8 & 0xff if isinstance(r8, int) else r8
            else:
                value = r8
            obj.props.setdefault('keyframes', []).append({'progress': x1, 'value': value, 'easing': easing})
            return 0
        if meth == 'InsertExpressionKeyFrame':
            obj.props.setdefault('keyframes', []).append({'progress': x1, 'expression': r8, 'easing': r9})
            return 0
        if meth == 'SetReferenceParameter':
            obj.props.setdefault('refs', []).append((rdx, r8))
            return 0
        if meth == 'ClearAllParameters':
            obj.props['refs'] = []
            return 0
        if meth == 'AddSource':
            obj.props.setdefault('sources', []).append(rdx)
            return 0
        if meth.startswith('put_'):
            params = self.vt.params(iface, meth) or ['?']
            ptype = params[0]
            obj.props[meth[4:]] = (ptype, self.materialize(ptype, x1 if ptype == 'float' else rdx))
            return 0
        raise EmuError(f'{self.cur_name}: {iface}.{meth}')


def emulate(functions, thunks, vt, variant='UAPv15'):
    Obj._n = 0
    emu = Emu(functions, thunks, vt)
    pfx = f"`anonymous namespace'::HelloSolidAnimations_AnimatedVisual_{variant}::"
    ctor = emu.by_name[pfx + f'HelloSolidAnimations_AnimatedVisual_{variant}']
    compositor, theme = Obj('Compositor'), Obj('ThemeProperties')
    regs = {'rsp': ('sp', 0), 'rcx': ('this', 0), 'rdx': ptr(compositor), 'r8': ptr(theme)}
    emu.run(ctor, regs)
    return emu



# ---------------------------------------------------------------- phase 3: the JSON document

ENUMS = {
    'StrokeStartCap': {0: 'flat', 1: 'square', 2: 'round', 3: 'triangle'},
    'StrokeEndCap': {0: 'flat', 1: 'square', 2: 'round', 3: 'triangle'},
    'StrokeDashCap': {0: 'flat', 1: 'square', 2: 'round', 3: 'triangle'},
    'StrokeLineJoin': {0: 'miter', 1: 'bevel', 2: 'round', 3: 'miter_or_bevel'},
    'BorderMode': {0: 'inherit', 1: 'soft', 2: 'hard'},
    'MappingMode': {0: 'absolute', 1: 'relative'},
    'InterpolationColorSpace': {0: 'auto', 1: 'hsl', 2: 'rgb', 3: 'hsl_linear', 4: 'rgb_linear'},
}
COMPOSITE_MODES = {0: 'source_over', 1: 'destination_over', 2: 'source_in', 3: 'destination_in', 4: 'source_out',
                   5: 'destination_out', 6: 'source_atop', 7: 'destination_atop', 8: 'xor', 9: 'plus'}
FILL_MODES = {0: 'even_odd', 1: 'nonzero'}
# the creation arguments of the factory calls, named after the property they set
CREATE_ARGS = {'geometry': 'Geometry', 'path': 'Path', 'surface': 'Surface', 'color': 'Color', 'cp1': 'ControlPoint1',
               'cp2': 'ControlPoint2', 'offset': 'Offset'}
COLOR_PROPS = {'Color'}
SPATIAL_RE = re.compile(r'^Pow\(1-_\.(t\d),3\)\*Vector2\(([^)]*)\)\+\(3\*Square\(1-_\.t\d\)\*_\.t\d\*Vector2\(([^)]*)\)\)'
                        r'\+\(3\*\(1-_\.t\d\)\*Square\(_\.t\d\)\*Vector2\(([^)]*)\)\)\+\(Pow\(_\.t\d,3\)\*Vector2\(([^)]*)\)\)$')


def color_str(v):
    v &= 0xffffffff  # Windows::UI::Color {A, R, G, B} as one little-endian dword
    a, r, g, b = v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff, (v >> 24) & 0xff
    return f'#{a:02X}{r:02X}{g:02X}{b:02X}'


class Exporter:
    def __init__(self, emu):
        self.emu = emu
        self.ids = {}
        self.objects = {}
        self.pending = []

    def ref(self, p):
        """the id of an object pointer (or a bare Obj), exporting it on first sight"""
        o = p[1] if isinstance(p, tuple) else p
        if o.id in self.ids:
            return self.ids[o.id]
        name = o.creator or o.kind
        base = f'{o.kind}:{name}'
        oid = base
        n = 1
        while oid in self.objects:
            n += 1
            oid = f'{base}#{n}'
        self.ids[o.id] = oid
        self.objects[oid] = None
        self.objects[oid] = self.convert(o)
        return oid

    @staticmethod
    def coord(x):
        """a float lane: an immediate dword store (`mov [rsp+..], 0C18578D5h`) is the float's bits"""
        return f32_to_float(x) if isinstance(x, int) else Exporter.value(None, x)

    def value(self, v, prop=None):
        if isinstance(v, tuple):
            t = v[0]
            if t == 'f32':
                return f32_to_float(v)
            if t in ('floats', 'pack'):
                return [self.coord(x) for x in v[1]]
            if t == 'str':
                return v[1]
            if t == 'ptr':
                return self.ref(v)
            if t == 'xmm':
                return [self.value(x) for x in v[1] if x is not None]
            raise EmuError(f'value {v}')
        if isinstance(v, Obj):
            return self.ref(v)
        if isinstance(v, list):
            return [self.value(x) for x in v]
        return v

    def typed(self, prop, tv):
        """a (abi type, raw value) property"""
        ptype, v = tv
        if prop in ENUMS:
            return ENUMS[prop].get(v, v)
        if ptype == 'bool':
            return bool(v)
        if 'Color' in ptype or prop in COLOR_PROPS:
            return color_str(v)
        if ptype == 'int64_t':  # TimeSpan ticks
            return v
        if ptype == 'float':
            return self.value(v)
        return self.value(v)

    def keyframe(self, kf):
        out = {'progress': self.value(kf['progress'])}
        if 'expression' in kf:
            expr = self.value(kf['expression'])
            out['expression'] = expr
            m = SPATIAL_RE.match(expr)
            if m:
                pts = [[float(c) for c in g.split(',')] for g in m.groups()[1:]]
                out['spatial_bezier'] = {'t': m.group(1), 'points': pts}
        else:
            v = kf['value']
            if kf.get('iface') == 'color':
                v = color_str(v)
            elif kf.get('iface') == 'bool':
                v = bool(v)
            elif isinstance(v, int):
                v = f32_to_float(v)
            else:
                v = self.value(v)
            out['value'] = v
        out['easing'] = self.ref(kf['easing']) if kf.get('easing') else None
        return out

    def animations(self, o):
        out = []
        for a in o.anims:
            e = {'property': self.value(a['property'])}
            if 'expression' in a:
                e['expression'] = a['expression'][1] if isinstance(a['expression'], tuple) else a['expression']
                e['refs'] = {self.value(n): self.ref(p) for n, p in a['refs']}
            else:
                e['animation'] = self.ref(a['animation'])
            out.append(e)
        return out

    def convert(self, o):
        d = {'kind': o.kind, 'name': o.creator}
        props = {}
        for k, v in o.props.items():
            if k.startswith('_') or k in ('keyframes', 'refs', 'sources', 'scalars', 'factory', 'effect', 'size', 'fill_mode', 'paused'):
                continue
            if k in CREATE_ARGS:
                k2 = CREATE_ARGS[k]
                props[k2] = color_str(v) if k2 == 'Color' else self.value(v)
            elif isinstance(v, tuple) and len(v) == 2 and isinstance(v[0], str) and ('::' in v[0] or v[0] in ('float', 'bool', 'void*', 'int32_t', 'int64_t', '?') or v[0].startswith('struct')):
                props[k] = self.typed(k, v)
            else:
                props[k] = self.value(v)
        for kf in o.props.get('keyframes', []):
            kf['iface'] = {'ColorKeyFrameAnimation': 'color', 'BooleanKeyFrameAnimation': 'bool'}.get(o.kind)
        if 'keyframes' in o.props:
            d['keyframes'] = [self.keyframe(k) for k in o.props['keyframes']]
        if o.kind == 'D2DPathGeometry':
            d['fill_mode'] = FILL_MODES.get(o.props.get('fill_mode'), o.props.get('fill_mode'))
            d['figures'] = []
            for fig in getattr(o, 'figures', []):
                segs = []
                for kind, vals in fig['segments']:
                    segs.append([kind] + [self.coord(x) for x in vals])
                d['figures'].append({'start': [self.coord(x) for x in fig['start'][1]], 'hollow': bool(fig['begin']),
                                     'closed': bool(fig['end']), 'segments': segs})
        if o.kind == 'D2DGeometryGroup':
            d['fill_mode'] = FILL_MODES.get(o.props.get('fill_mode'), o.props.get('fill_mode'))
            d['geometries'] = [self.ref(c) for c in o.children]
        if o.kind == 'EffectBrush':
            factory = o.props['factory']
            heap = factory.props['effect'][1]
            mem = getattr(heap, 'mem', {})
            if heap.props.get('sources'):
                d['effect'] = {'type': 'composite', 'mode': COMPOSITE_MODES.get(mem.get(0x1c), mem.get(0x1c)),
                               'inputs': [self.value(s[1].props['name']) for s in heap.props['sources']]}
            else:
                d['effect'] = {'type': 'gaussian_blur', 'blur_amount': f32_to_float(mem.get(0x28)),
                               'inputs': [self.value(mem[0x30][1].props['name'])]}
            d['sources'] = {self.value(n): self.ref(b) for n, b in o.props.get('sources', [])}
        if o.kind in ('ContainerVisual', 'ShapeVisual', 'ContainerShape', 'LinearGradientBrush'):
            key = {'ContainerVisual': 'children', 'ShapeVisual': 'shapes', 'ContainerShape': 'shapes', 'LinearGradientBrush': 'color_stops'}[o.kind]
            d[key] = [self.ref(c) for c in o.children]
        if '_propertyset' in o.props:
            ps = o.props['_propertyset']
            if ps.props.get('scalars') or ps.anims:
                d['property_set'] = {'scalars': {self.value(n): self.value(v) for n, v in ps.props.get('scalars', [])},
                                     'animations': self.animations(ps)}
        d['props'] = props
        anims = self.animations(o)
        if anims:
            d['animations'] = anims
        return d


def export(emu, functions, variant='UAPv15'):
    root = emu.fields[0x2E8]
    ex = Exporter(emu)
    root_id = ex.ref(root)
    theme = next(o for o in emu.objs + [root[1]] if o.kind == 'ThemeProperties') if any(o.kind == 'ThemeProperties' for o in emu.objs) else None
    # the root property set: Progress and the t0..t4 helpers
    root_ps = root[1].props.get('_propertyset')
    markers = []
    for fea, f in functions.items():
        if '@Markers@HelloSolidAnimations@AnimatedVisuals@@UE' in f['name']:
            markers = extract_markers(f['ins'])
    doc = {
        'format': 'linux_hello/hello-eye-solid/1',
        'source': {'binary': 'Windows.UI.Logon.dll', 'class': f'AnimatedVisuals::HelloSolidAnimations_AnimatedVisual_{variant}',
                   'tool': 'tools/winhello_ui/lottie_extract.py'},
        'canvas': [64.0, 64.0],
        'duration_ticks': 378333333,
        'frame_count': 2270,
        'frame_rate': 60,
        'markers': [{'name': n, 'progress': p, 'frame': round(p * 2270 - 0.05)} for n, p in markers],
        'theme': {'SolidColor': 'the theme colour: white on the dark sign-in screen (HelloSolidAnimations::SolidColor)'},
        'root': root_id,
        'root_properties': {'scalars': {ex.value(n): ex.value(v) for n, v in root_ps.props.get('scalars', [])},
                            'animations': ex.animations(root_ps)} if root_ps else None,
        'objects': ex.objects,
        'warnings': emu.warnings,
    }
    return doc


def extract_markers(ins):
    """`Markers::get`: a (name, double progress) pair per marker"""
    out, name = [], None
    for x in ins:
        for op in x['ops']:
            if 'str' in op and x['m'] == 'lea':
                name = op['str']
        if name and x['m'] == 'movsd' and x['ops'][1]['t'] == 2:
            out.append((name, struct.unpack('<d', bytes.fromhex(x['ops'][1]['bytes'])[:8])[0]))
            name = None
        elif name and x['m'] == 'xorps':
            out.append((name, 0.0))
            name = None
    return out

if __name__ == '__main__':
    if len(sys.argv) >= 2 and sys.argv[1] == 'dump':
        dump(sys.argv[2], sys.argv[3])
    elif len(sys.argv) >= 2 and sys.argv[1] == 'extract':
        data = json.load(open(sys.argv[2]))
        functions = {int(k): v for k, v in data['functions'].items()}
        emu = emulate(functions, data['thunks'], WinRTVtables(sys.argv[3]))
        doc = export(emu, functions)
        json.dump(doc, open(sys.argv[4], 'w'), indent=1)
        print(len(doc['objects']), 'objects,', len(doc['markers']), 'markers,', len(emu.warnings), 'warnings')
        for w in emu.warnings:
            print(' ', w)
    else:
        print(__doc__)
