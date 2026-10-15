"""Recover the hard-coded first JDA stage and the fern pre-filter from FaceTrackerInternal.dll.

Pipeline (needs IDA 9.5 idalib + g++; run once, output is cached in converted/ldmf/hardcoded_structure.npz):
  1. ida_decompile.py: renames every __xmm@ constant to XMMC_<ea> and decompiles FernPredict1..8,
     JointCascadePredictProduction{Infrared,LowEnd}ModelBoostN with Hex-Rays.
  2. this script rewrites the pseudocode into C++ where every local is a symbolic value `V` (symrt.hpp):
     image reads become pixel terms, pixel differences become linear terms, comparisons of a pixel difference
     with a constant are decisions answered by an oracle and logged; leaf-weight reads mark the end of a tree,
     SSE fern code (_mm_sub/_mm_cmpgt/_mm_movemask + LUT 0x180112750) yields the 8 tests of each fern.
  3. Each Boost function is run depth-first over the oracle answers per tree (32 paths per tree), giving
     the complete depth-5 tree; each FernPredict function is run once.
usage: python -m ldmf.extract.build_structure --idb FaceTrackerInternal.dll.i64 --dll FaceTrackerInternal.dll
       --work /tmp/ldmf_extract -o converted/ldmf/hardcoded_structure.npz
Copy the .i64 first: the IDB is modified (renames) by step 1.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import subprocess
from collections import Counter
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
TARGETS = ["FernPredict", "ProductionInfraredModelBoost", "ProductionLowEndModelBoost"]
FERN_FUNCS = [f"FernPredict{i}" for i in range(1, 9)]
IR_FUNCS = [f"InfraredModelBoost{i}" for i in range(1, 11)]
LE_FUNCS = ["LowEndModelBoost1", "LowEndModelBoost2"]
# locals the decompiler typed `int *` (pointer arithmetic in elements, not bytes)
INT_PTR_LOCALS = {"FernPredict4": ["v12", "v28", "v30", "v31", "v34", "v39", "v131", "v132", "v133", "v175", "v270"],
                  "FernPredict7": ["v276"], "InfraredModelBoost8": ["v44"], "InfraredModelBoost9": ["v20"],
                  "InfraredModelBoost10": ["v146"]}

def match_close(s, i):
    # s[i] is an opening bracket; return index of matching close
    op=s[i]; cl={'(':')','[':']'}[op]; d=0
    for j in range(i,len(s)):
        if s[j]==op: d+=1
        elif s[j]==cl:
            d-=1
            if d==0: return j
    raise ValueError

def transform(txt, kind, fname):
    body=txt[txt.index('\n{\n')+3: txt.rindex('}')]
    # declarations
    lines=body.split('\n'); out=[]
    for l in lines:
        m=re.match(r'^\s+(?:const )?[\w\s]+?\s*\**(v\d+|result)(\[\d+\])?; //',l)
        if m and not '=' in l:
            ty='M' if '__m128i' in l else 'V'
            out.append(f'  {ty} {m.group(1)}{m.group(2) or ""};'); continue
        out.append(l)
    b='\n'.join(out)
    # OFSUB pattern
    for m in list(re.finditer(r'(v\d+) = __OFSUB__\((v\d+), (-?\d+)\);',b)):
        vof,va,k=m.groups()
        mz=re.search(rf'(v\d+) = {va} == {re.escape(k)};',b); ms=re.search(rf'(v\d+) = {va} [-+] \d+ < 0;',b)
        vz,vs=mz.group(1),ms.group(1)
        b=b.replace(m.group(0),'').replace(mz.group(0),'').replace(ms.group(0),'')
        b=b.replace(f'{vs} ^ {vof} | {vz}', f'({va} <= {k})')
    assert '__OFSUB__' not in b
    b=re.sub(r'\bthis\b','self_',b)
    b=re.sub(r'\b(LOBYTE|LODWORD)\((v\d+)\)',r'\2',b)
    # &X[...] -> ADDR(X, ...)
    while True:
        m=re.search(r'&(\w+)\[',b)
        if not m: break
        j=match_close(b,m.end()-1)
        b=b[:m.start()]+f'ADDR({m.group(1)}, '+b[m.end():j]+')'+b[j+1:]
    # remove casts
    b=re.sub(r'\((?:const )?(?:unsigned )?(?:__int8|__int16|__int64|int|char|_DWORD|_BYTE|bool|__m128i)\s*\**\s*\**\)','',b)
    b=b.replace('int const **','')
    if kind=='boost':
        hdr=f'V {fname}(V self_, V a2, V a3, V a4, V a5, V a6, V a7, V a8, V* a9, V* a10)\n{{'
    else:
        hdr=f'V {fname}(V self_, V a2, V a3, V* a4, V* a5, V a6, V* a7)\n{{'
    return hdr+b+'}\n'


def transform2(txt, kind, fname):
    t = transform(txt, kind, fname)
    t = re.sub(r"^\s+__m128i (\w+); //.*$", r"  M \1;", t, flags=re.M)
    t = re.sub(r"^\s+(?:const )?(?:unsigned )?[\w]+\s*\**(\w+); //.*$", r"  V \1;", t, flags=re.M)
    for v in INT_PTR_LOCALS.get(fname, []):
        t = re.sub(rf"^  V {v};$", f"  VP {v};", t, count=1, flags=re.M)
    return t.replace("V* a9, V* a10)", "V a9, V a10)").replace("V* a4, V* a5, V a6, V* a7)", "V a4, V a5, V a6, V a7)")


def build_harness(pseudo: dict, dll_read) -> str:
    rt = (HERE / "symrt.hpp").read_text()
    funcs, srcs, xmm = [], [], set()
    for nm, txt in sorted(pseudo.items()):
        kind = "fern" if nm.startswith("Fern") else "boost"
        fn = nm.replace("JointCascadePredictProduction", "")
        t = transform2(txt, kind, fn)
        xmm |= set(re.findall(r"XMMC_[0-9a-f]+", t))
        funcs.append((fn, kind)); srcs.append(t)
    consts = [f"static M {x} = mkc({','.join(map(str, np.frombuffer(dll_read(int(x[5:], 16), 16), np.int16)))});" for x in sorted(xmm)]
    main = "int main(int argc, char** argv){\n  std::string which=argv[1];\n"
    for fn, kind in funcs:
        if kind == "fern":
            main += (f'  if(which=="{fn}"){{ store_[13].k=SCORE; store_[14]=V(0LL); store_[15]=mkptr(fernw_img,0); target=-1; cur_tree=0; '
                     f'V r={fn}(V(0LL),mkptr(0,0),V(8LL),mkptr(13,0),mkptr(14,0),V(102400LL),mkptr(15,0)); '
                     f'printf("R %lld %lld\\n", r.c, store_[15].c); return 0; }}\n')
        else:
            main += (f'  if(which=="{fn}"){{ for(int tgt=0;;tgt++){{ std::vector<std::vector<int>> st; st.push_back({{}}); int ntrees=0;\n'
                     f'    while(!st.empty()){{ plan=st.back(); st.pop_back(); taken.clear(); cur_tree=0; target=tgt; dcount=0;\n'
                     f'      store_[13]=V(); store_[13].k=SCORE; printf("P %d\\n", tgt);\n'
                     f'      {fn}(V(8LL),V(8LL),V(8LL),mkptr(0,0),mkptr(1,0),mkptr(2,0),V(0LL),mkptr(weight_img,0),mkptr(12,0),mkptr(13,0));\n'
                     f'      ntrees=cur_tree; for(size_t i=plan.size();i<taken.size();i++){{ std::vector<int> p(taken.begin(),taken.begin()+i); p.push_back(0); st.push_back(p);}} }}\n'
                     f'    if(ntrees<=tgt) break; }} return 0; }}\n')
    main += "  return 1;\n}\n"
    return "\n".join([rt] + consts + srcs) + main


def parse_boost(work, fn):
    L=open(f"{work}/{fn}.log").read().splitlines()
    trees={}; tgt=None; path=[]; 
    def flush():
        pass
    run=None
    for l in L:
        p=l.split(' ')
        if p[0]=='P':
            tgt=int(p[1]); run={'nodes':[], 'leaf':None,'thr':None}
            trees.setdefault(tgt,{'nodes':{},'leaves':{},'thr':set()})
        elif p[0]=='N':
            t,i=int(p[1]),int(p[2]); lin=json.loads(p[3]); thr=int(p[4]); ans=int(p[5])
            assert t==tgt
            run['nodes'].append((lin,thr,ans))
        elif p[0]=='L':
            if int(p[1])==tgt:
                tr=trees[tgt]; path=tuple(a for _,_,a in run['nodes'])
                for k,(lin,thr,ans) in enumerate(run['nodes']):
                    key=path[:k]; v=(json.dumps(lin),thr)
                    assert tr['nodes'].get(key,v)==v, (fn,tgt,key)
                    tr['nodes'][key]=v
                tr['leaves'][path]=int(p[2])
        elif p[0]=='S':
            if int(p[1])==tgt+1: trees[tgt]['thr'].add((int(p[2]),int(p[3])))
    return [trees[k] for k in sorted(trees) if trees[k]['leaves']]

def tree_arrays(t):
    # returns feat array (31, 7): imgA,xA,yA,imgB,xB,yB,thr  ; complete binary tree in BFS order; left = '>' (ans 0), right = '<=' (ans 1)
    out=np.zeros((31,7),np.int32)
    for key,(lin,thr) in t['nodes'].items():
        d=len(key); j=(1<<d)-1+sum(a<<(d-1-k) for k,a in enumerate(key))
        lin=json.loads(lin); A=[x for x in lin if x[0]==1]; B=[x for x in lin if x[0]==-1]
        assert len(A)==1 and len(B)==1, lin
        a,b=A[0],B[0]
        out[j]=[a[1],a[2]%256,a[2]//256,b[1],b[2]%256,b[2]//256,thr]
    return out


def parse_ferns(work: Path):
    ferns = []
    base = 0
    for fn in FERN_FUNCS:
        cur = None
        for line in (work / f"{fn}.log").read_text().splitlines():
            p = line.split(" ", 3)
            if p[0] == "F":
                cur = {"woff": base + int(p[2]), "tests": []}; ferns.append(cur)
            elif p[0] == "FT":
                lin, t = (p[2] + (" " + p[3] if len(p) > 3 else "")).rsplit(" ", 1)
                cur["tests"].append((json.loads(lin), int(t)))
            elif p[0] == "S":
                ferns[-1]["thr"] = int(p[3])
        base += 51200 // 4
    tests = np.zeros((len(ferns), 8, 5), np.int32)
    for i, f in enumerate(ferns):
        assert f["woff"] == 256 * i
        for j, (terms, thr) in enumerate(f["tests"]):
            A = [x for x in terms if x[0] == 1][0]; B = [x for x in terms if x[0] == -1][0]
            tests[i, j] = [A[2] % 256, A[2] // 256, B[2] % 256, B[2] // 256, thr]
    return tests, np.array([f["thr"] for f in ferns], np.int64)


def main(argv=None):
    from ..hardcoded import Dll, FERN_WEIGHTS
    ap = argparse.ArgumentParser()
    ap.add_argument("--idb", type=Path, required=True); ap.add_argument("--dll", type=Path, required=True)
    ap.add_argument("--work", type=Path, required=True); ap.add_argument("-o", "--out", type=Path, required=True)
    ap.add_argument("--ida-python", default="python3", help="python with idalib (idapro) importable")
    a = ap.parse_args(argv)
    a.work.mkdir(parents=True, exist_ok=True)
    subprocess.run([a.ida_python, str(HERE / "ida_decompile.py"), str(a.idb), str(a.work / "pseudo")] + TARGETS, check=True)
    pseudo = {}
    for p in glob.glob(str(a.work / "pseudo" / "*.c")):
        t = Path(p).read_text()
        pseudo[t.split("\n", 1)[0].split("FaceDetector::")[1].split("(")[0]] = t
    dll = Dll(a.dll)
    (a.work / "hc_sym.cpp").write_text(build_harness(pseudo, dll.read))
    subprocess.run(["g++", "-O1", "-w", "-fpermissive", "-o", str(a.work / "hc_sym"), str(a.work / "hc_sym.cpp")], check=True)
    for fn in FERN_FUNCS + IR_FUNCS + LE_FUNCS:
        with open(a.work / f"{fn}.log", "w") as fh:
            subprocess.run([str(a.work / "hc_sym"), fn], stdout=fh, check=True)
    out = {}
    for name, order in (("infrared", IR_FUNCS), ("lowend", LE_FUNCS)):
        trees = []
        for fn in order:
            trees += parse_boost(a.work, fn)
        out[f"{name}_trees"] = np.stack([tree_arrays(t) for t in trees])
        out[f"{name}_thr"] = np.array([list(t["thr"])[0][1] for t in trees], np.int64)
    out["fern_tests"], out["fern_thr"] = parse_ferns(a.work)
    out["fern_w"] = dll.array(FERN_WEIGHTS, "<i4", 400 * 256).reshape(400, 256)
    np.savez(a.out, **out)
    print(a.out, {k: v.shape for k, v in out.items()})


if __name__ == "__main__":
    main()
