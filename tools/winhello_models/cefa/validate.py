"""Validation: owner (IR frames) vs strangers with the converted v5 recognition nets.

Faces/5 landmarks come from SCRFD det_10g (linux_hello open model) because the Windows
27-point IR aligner is not used here; the crop itself follows FaceRecognitionCnnImpl2.
"""
from __future__ import annotations

import glob
import json
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

from . import align

REPO = Path(__file__).resolve().parents[3]
VF = Path("/home/cole/SiteResearch/windows_hello/re/validation_frames")
CONV = REPO / "models/windows_hello/converted/cefa"


class Scrfd:
    def __init__(self, path):
        self.s = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
        self.inp = self.s.get_inputs()[0].name

    def detect(self, rgb: np.ndarray, thr=0.5, size=640):
        h, w = rgb.shape[:2]
        sc = size / max(h, w)
        nh, nw = int(round(h * sc)), int(round(w * sc))
        im = np.array(Image.fromarray(rgb).resize((nw, nh), Image.BILINEAR), np.float32)
        pad = np.zeros((size, size, 3), np.float32)
        pad[:nh, :nw] = im
        x = ((pad - 127.5) / 128.0).transpose(2, 0, 1)[None]
        outs = self.s.run(None, {self.inp: x})
        dets = []
        for k, st in enumerate((8, 16, 32)):
            score, bbox, kps = outs[k][:, 0], outs[k + 3] * st, outs[k + 6] * st
            n = size // st
            cy, cx = np.mgrid[:n, :n]
            ctr = np.stack([cx, cy], -1).reshape(-1, 2).repeat(2, 0).astype(np.float32) * st
            for i in np.where(score > thr)[0]:
                b = np.r_[ctr[i] - bbox[i, :2], ctr[i] + bbox[i, 2:]] / sc
                p = (ctr[i] + kps[i].reshape(5, 2)) / sc
                dets.append((float(score[i]), b, p))
        dets.sort(key=lambda d: -d[0])
        keep = []
        for d in dets:
            if all(_iou(d[1], k[1]) < 0.4 for k in keep):
                keep.append(d)
        return keep


def _iou(a, b):
    ix = max(0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
    i = ix * iy
    return i / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i)


def cosdist(a, b):
    """Engine distance (F50): 1 - cos, clamped to <= 1."""
    c = float(a @ b / np.sqrt((a @ a) * (b @ b)))
    return min(1.0, 1.0 - c)


def collect_crops(det):
    owner, strangers = [], []
    for p in sorted(glob.glob(str(VF / "ir_*_lit.png"))) + sorted(glob.glob(str(VF / "watch_*_ir_lit.png"))):
        gray = np.array(Image.open(p).convert("L"))
        d = det.detect(np.stack([gray] * 3, -1))
        if d:
            owner.append((Path(p).name, align.recognition_crop(gray, d[0][2])))
    rgb = np.array(Image.open(VF / "strangers_insightface_t1.jpg").convert("RGB"))
    gray = np.array(Image.open(VF / "strangers_insightface_t1.jpg").convert("L"))
    for i, d in enumerate(det.detect(rgb)):
        strangers.append((f"stranger_{i}", align.recognition_crop(gray, d[2])))
    return owner, strangers


def main():
    det = Scrfd(REPO / "models/det_10g.onnx")
    owner, strangers = collect_crops(det)
    out = CONV / "validation"
    out.mkdir(exist_ok=True)
    for n, c in owner + strangers:
        Image.fromarray(c).save(out / f"crop_{Path(n).stem}.png")
    res = {"n_owner": len(owner), "n_strangers": len(strangers)}
    for model in ("MODEL_FILES_123", "MODEL_FILES_125"):
        s = ort.InferenceSession(str(CONV / f"{model}.onnx"), providers=["CPUExecutionProvider"])
        name = s.get_inputs()[0].name
        emb = {n: s.run(None, {name: c.astype(np.float32)[None, None]})[0].ravel() for n, c in owner + strangers}
        spaces = {"v5": emb}
        conv = "MODEL_FILES_126" if model.endswith("125") else "MODEL_FILES_124"
        cs = ort.InferenceSession(str(CONV / f"{conv}.blob0.onnx"), providers=["CPUExecutionProvider"])
        cn = cs.get_inputs()[0].name
        spaces[f"{conv}.blob0"] = {
            n: cs.run(None, {cn: (e / np.linalg.norm(e))[None].astype(np.float32)})[0].ravel() for n, e in emb.items()
        }
        for sp, E in spaces.items():
            on = [n for n, _ in owner]
            st = [n for n, _ in strangers]
            oo = [cosdist(E[a], E[b]) for i, a in enumerate(on) for b in on[i + 1 :]]
            os_ = [cosdist(E[a], E[b]) for a in on for b in st]
            ss = [cosdist(E[a], E[b]) for i, a in enumerate(st) for b in st[i + 1 :]]
            res[f"{model}/{sp}"] = {
                "owner_owner": [float(np.min(oo)), float(np.mean(oo)), float(np.max(oo))],
                "owner_stranger": [float(np.min(os_)), float(np.mean(os_)), float(np.max(os_))],
                "stranger_stranger": [float(np.min(ss)), float(np.mean(ss)), float(np.max(ss))] if ss else None,
                "owner_accepted_at_0.350": float(np.mean(np.array(oo) <= 0.350)),
                "stranger_accepted_at_0.350": float(np.mean(np.array(os_) <= 0.350)),
            }
        if model.endswith("123"):
            e123 = emb
        else:
            res["max_abs_diff_123_vs_125"] = float(max(np.abs(e123[n] - emb[n]).max() for n in emb))
    (out / "results.json").write_text(json.dumps(res, indent=1))
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
