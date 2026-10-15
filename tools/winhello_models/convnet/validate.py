"""Validation of the converted anti-spoofing nets on recorded frames.

    cd tools/winhello_models && .venv/bin/python -m convnet.validate [--frames DIR] [--out DIR]

Faces/landmarks come from the repo's open SCRFD det_10g (stand-in for Microsoft's
own landmark tracker).  Inputs are then built with the re-implemented Microsoft
preparation (convnet.msprep), and every converted ONNX model is compared with the
NumPy DNNTestLib re-implementation (convnet.reference).
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

from . import load
from . import msprep
from . import reference as ref
from .__main__ import OUT_DIR, REPO, SECURE_DIR, SLOTS

FRAMES = Path("/home/cole/SiteResearch/windows_hello/re/validation_frames")

# Threshold maps (static initialisers 0x180014020/0x180014380/0x180015450/0x180015590),
# indexed [flag][slot] -> (mode0, mode1).
REAL_THR = {"off": {11: (0.5, 0.9), 12: (0.5, 0.9)},
            "on": {11: (0.4711, 0.4921), 12: (0.5037, 0.5197)}}
SPOOF_THR = {"off": {11: (1.0, 0.5), 12: (1.0, 0.5), 13: (0.5, 0.5), 14: (0.7, 0.7), 15: (0.5, 0.5)},
             "on": {11: (1.0, 0.5289), 12: (1.0, 0.4963), 13: (0.5, 0.5), 14: (0.7, 0.7), 15: (0.5, 0.5)}}


def decide(slot, flag, p, mode):
    """Per-classifier vote as in RunDual/RunColor/RunMultiTask (F24): 'live', 'spoof' or '-'."""
    if slot in (11, 12):
        if p[0] > REAL_THR[flag][slot][mode]:
            return "live"
        return "spoof" if p[1] > SPOOF_THR[flag][slot][mode] else "-"
    if slot == 13:
        return "spoof" if p[0] > SPOOF_THR[flag][13][mode] else "-"
    return "spoof" if p[1] > SPOOF_THR[flag][slot][mode] else "-"


class SCRFD:
    def __init__(self, path):
        self.s = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])

    def detect(self, rgb: np.ndarray, thr=0.5):
        h, w = rgb.shape[:2]
        scale = 640 / max(h, w)
        nh, nw = int(round(h * scale)), int(round(w * scale))
        canvas = np.zeros((640, 640, 3), np.float32)
        canvas[:nh, :nw] = np.asarray(Image.fromarray(rgb).resize((nw, nh), Image.BILINEAR), np.float32)
        blob = ((canvas - 127.5) / 128.0).transpose(2, 0, 1)[None]
        outs = self.s.run(None, {self.s.get_inputs()[0].name: blob})
        dets = []
        for i, stride in enumerate((8, 16, 32)):
            sc, bb, kp = outs[i][:, 0], outs[i + 3] * stride, outs[i + 6] * stride
            fs = 640 // stride
            ys, xs = np.mgrid[0:fs, 0:fs]
            ctr = np.repeat(np.stack([xs, ys], -1).reshape(-1, 2) * stride, 2, axis=0).astype(np.float32)
            for j in np.where(sc >= thr)[0]:
                c = ctr[j]
                box = np.array([c[0] - bb[j, 0], c[1] - bb[j, 1], c[0] + bb[j, 2], c[1] + bb[j, 3]]) / scale
                kps = (c + kp[j].reshape(5, 2)) / scale
                dets.append((float(sc[j]), box, kps))
        dets.sort(key=lambda d: -d[0])
        keep = []
        for d in dets:  # NMS
            if all(_iou(d[1], k[1]) < 0.4 for k in keep):
                keep.append(d)
        return keep


def _iou(a, b):
    ix = max(0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    return inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", default=str(FRAMES))
    ap.add_argument("--models", default=str(OUT_DIR))
    ap.add_argument("--out", default=str(OUT_DIR / "validation"))
    a = ap.parse_args(argv)
    frames, out = Path(a.frames), Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    det = SCRFD(REPO / "models/det_10g.onnx")

    nets, sess = {}, {}
    for rid in SLOTS:
        nets[rid] = load(SECURE_DIR / f"MODEL_FILES_{rid}.convnet")
        sess[rid] = ort.InferenceSession(str(Path(a.models) / f"MODEL_FILES_{rid}.onnx"), providers=["CPUExecutionProvider"])

    def run(rid, img_u8):
        x = img_u8[None, None] if img_u8.ndim == 2 else img_u8.transpose(2, 0, 1)[None]
        p_onnx = sess[rid].run(["prob"], {"image": x.astype(np.float32)})[0][0]
        p_ref = ref.classify(nets[rid], img_u8)
        return p_onnx, float(np.abs(p_onnx - p_ref).max())

    # Build the test inputs: (case, kind, image) where kind selects the slots it is fed to.
    cases = []
    def face(gray_or_rgb):
        rgb = gray_or_rgb if gray_or_rgb.ndim == 3 else np.repeat(gray_or_rgb[..., None], 3, 2)
        return det.detect(rgb)

    for lit_path in sorted(frames.glob("ir_*_lit.png")):
        n = int(lit_path.stem.split("_")[1])
        lit = np.asarray(Image.open(lit_path).convert("L"))
        dark_path = frames / f"ir_{n - 1}_dark.png"
        faces = face(lit)
        if not faces:
            print("no face in", lit_path.name); continue
        kps = faces[0][2]
        cases.append((f"owner {lit_path.stem}", "ir_crop", msprep.align_crop(lit, kps)))
        if dark_path.exists():
            dark = np.asarray(Image.open(dark_path).convert("L"))
            cases.append((f"owner {lit_path.stem}-{dark_path.stem}", "ir_diff", msprep.align_crop(msprep.difference_image(lit, dark), kps)))
        cases.append((f"owner {lit_path.stem}", "frame", msprep.thumbnail(lit)))
    for lit_path in sorted(frames.glob("watch_*_ir_lit.png")):
        lit = np.asarray(Image.open(lit_path).convert("L"))
        faces = face(lit)
        if faces:
            cases.append((f"owner {lit_path.stem}", "ir_crop", msprep.align_crop(lit, faces[0][2])))
        cases.append((f"owner {lit_path.stem}", "frame", msprep.thumbnail(lit)))
        rgb_path = frames / lit_path.name.replace("_ir_lit", "_rgb")
        if rgb_path.exists():
            rgb = np.asarray(Image.open(rgb_path).convert("RGB"))
            f = face(rgb)
            if f:
                cases.append((f"owner {rgb_path.stem}", "color", msprep.align_crop(np.ascontiguousarray(rgb[:, :, ::-1]), f[0][2])))
    st = frames / "strangers_insightface_t1.jpg"
    if st.exists():
        rgb = np.asarray(Image.open(st).convert("RGB"))
        gray = np.asarray(Image.open(st).convert("L"))
        for i, (_, _, kps) in enumerate(face(rgb)):
            cases.append((f"stranger photo face{i}", "ir_crop", msprep.align_crop(gray, kps)))
            cases.append((f"stranger photo face{i}", "ir_diff", msprep.align_crop(gray, kps)))
            cases.append((f"stranger photo face{i}", "color", msprep.align_crop(np.ascontiguousarray(rgb[:, :, ::-1]), kps)))
        cases.append(("stranger photo (gray, whole)", "frame", msprep.thumbnail(gray)))

    kind_slots = {"ir_crop": (11,), "ir_diff": (12,), "frame": (14, 15), "color": (13,)}
    rows, worst = [], 0.0
    for name, kind, img in cases:
        safe = name.replace(" ", "_").replace("(", "").replace(")", "").replace(",", "")
        Image.fromarray(img[:, :, ::-1] if img.ndim == 3 else img).save(out / f"{safe}_{kind}.png")  # PNGs in RGB
        for rid, (slot, role, flag) in SLOTS.items():
            if slot not in kind_slots[kind]:
                continue
            p, err = run(rid, img)
            worst = max(worst, err)
            rows.append(dict(case=name, input=kind, model=rid, slot=slot, flag=flag, p0=round(float(p[0]), 4),
                             p1=round(float(p[1]), 4), vote_mode0=decide(slot, flag, p, 0), vote_mode1=decide(slot, flag, p, 1),
                             onnx_vs_ref=err))
    (out / "results.json").write_text(json.dumps(rows, indent=1))
    print(f"{len(cases)} inputs, {len(rows)} runs, max |p_onnx - p_ref| = {worst:.2e}")
    print(f"{'case':<40} {'input':<8} {'model':>5} {'slot':>4} {'flag':>4} {'p0':>7} {'p1':>7}  vote(m0/m1)")
    for r in rows:
        print(f"{r['case']:<40} {r['input']:<8} {r['model']:>5} {r['slot']:>4} {r['flag']:>4} {r['p0']:>7.4f} {r['p1']:>7.4f}  {r['vote_mode0']}/{r['vote_mode1']}")


if __name__ == "__main__":
    main()
