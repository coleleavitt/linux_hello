"""Validation of the LDMF reimplementation.

1. Window-level cross-check against FaceTrackerInternal.dll executed under Unicorn (ldmf.emu):
   fern cascade (pass flag + *a5 milestone counter), JDA cascade (alive flag, integer score, 31/27 landmarks)
   and OrientedHogPostFilter score, on every fern-passing window plus random windows.
2. Full-frame detection on the owner's IR frames and a stranger photo, compared with SCRFD (det_10g).
"""
from __future__ import annotations

import glob
import json
import time
from pathlib import Path

import numpy as np
from PIL import Image

from . import jda


def _gray(p):
    return np.asarray(Image.open(p).convert("L"))


def iou(a, b):
    ix = max(0, min(a[2], b[2]) - max(a[0], b[0])); iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
    i = ix * iy
    return i / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i)


def emu_crosscheck(model: jda.Model, ldmf_model, kind: str, dll: Path, img: np.ndarray, n_random=40, seed=0):
    from .emu import EmuDetector
    E = EmuDetector(dll, ldmf_model, kind)
    layers = jda.build_pyramid(img)
    st = dict(fern_windows=0, fern_mismatch=0, jda_windows=0, jda_mismatch=0, shape_maxdiff=0.0,
              hog_windows=0, hog_decision_mismatch=0, hog_maxdiff=0.0)
    rs = np.random.RandomState(seed)
    for li in range(len(layers) - 8):
        ly = layers[li]; half = layers[li + 4].img; q = layers[li + 8].img
        if ly.img.shape[0] < 96 or ly.img.shape[1] < 96:
            break
        xs = np.arange(0, ly.img.shape[1] - 95, 8); ys = np.arange(0, ly.img.shape[0] - 95, 8)
        gx, gy = (a.ravel() for a in np.meshgrid(xs, ys))
        p, n = jda.fern_cascade(model, half, gx, gy)
        sel = set(np.nonzero(p)[0].tolist()) | set(rs.choice(gx.size, min(n_random, gx.size), replace=False).tolist())
        for k in sorted(sel):
            ep, en, _ = E.ferns(half, 24 + gx[k] // 2, 24 + gy[k] // 2)
            st["fern_windows"] += 1
            st["fern_mismatch"] += int(ep != bool(p[k]) or en != int(n[k]))
        cand = np.nonzero(p)[0]
        if cand.size == 0:
            continue
        a, s, sh = jda.jda_predict(model, ly.img, half, q, gx[cand], gy[cand])
        for i, k in enumerate(cand):
            x, y = int(gx[k]), int(gy[k])
            fw = ly.img[y:y + 96, x:x + 96]
            ea, esc, esh = E.jda(fw, half[12 + y // 2:60 + y // 2, 12 + x // 2:60 + x // 2], q[18 + y // 4:42 + y // 4, 18 + x // 4:42 + x // 4])
            st["jda_windows"] += 1
            if ea != bool(a[i]) or esc != int(s[i]):
                st["jda_mismatch"] += 1
                continue
            if not ea:
                continue
            st["shape_maxdiff"] = max(st["shape_maxdiff"], float(np.abs(esh - sh[i]).max()))
            ph, pok = jda.post_filter(model, fw, sh[i])
            eh, eok = E.post(fw, esh, esc / model.score_scale)
            st["hog_windows"] += 1
            st["hog_decision_mismatch"] += int(pok != eok)
            st["hog_maxdiff"] = max(st["hog_maxdiff"], abs(ph - eh))
    return st


def run(models: dict, frames_dir: Path, scrfd_path: Path, dll: Path | None, ldmf_files: dict, out_json: Path,
        emu_frames: int = 2):
    from lbf.scrfd import Scrfd  # sibling validation helper (insightface det_10g)
    sc = Scrfd(str(scrfd_path))
    frames = sorted(glob.glob(str(frames_dir / "ir_*_lit.png"))) + sorted(glob.glob(str(frames_dir / "watch_*_ir_lit.png")))
    stranger = sorted(glob.glob(str(frames_dir / "strangers*.jpg")))
    report = {"frames": {}, "emu": {}}
    for f in frames + stranger:
        g = _gray(f)
        ref = sc.detect(g, 0.5)
        entry = {"scrfd": [{"box": [round(float(v), 1) for v in r[0]], "score": round(r[1], 3)} for r in ref]}
        for name, m in models.items():
            t = time.time()
            dets = jda.detect(m, g)
            dt = time.time() - t
            rows = []
            for d in dets:
                best = max(ref, key=lambda r: iou(d.box, r[0])) if ref else None
                row = {"box": list(d.box), "score": round(d.score, 3), "jda_score": round(d.jda_score, 3)}
                if best is not None:
                    row["iou_scrfd"] = round(float(iou(d.box, best[0])), 3)
                    kp = best[2]
                    iod = float(np.linalg.norm(kp[0] - kp[1]))
                    lm5 = d.landmarks[:5]
                    row["lm5_nme_vs_scrfd"] = round(float(np.linalg.norm(lm5 - kp, axis=1).mean() / iod), 3)
                rows.append(row)
            entry[name] = {"detections": rows, "seconds": round(dt, 2)}
        report["frames"][Path(f).name] = entry
    from . import retina
    ort_dir = Path(ldmf_files["101_infrared"]).parents[1] / "converted/ldmf/ort"
    report["retina"] = {}
    for rid in (110, 111, 112, 113):
        p = ort_dir / f"MODEL_FILES_{rid}.ort"
        if not p.exists():
            continue
        det = retina.RetinaDetector(p)
        res = {}
        for f in frames + stranger:
            g = _gray(f)
            ref = sc.detect(g, 0.5)
            for eq in (False, True):
                img = retina.histogram_equalize(g) if eq else g
                rows = []
                for fc in det.detect(img):
                    row = {"box": list(fc.box), "score": round(fc.score, 4)}
                    if fc.rotation_xyz is not None:
                        row["rotation_xyz_deg"] = [round(v, 1) for v in fc.rotation_xyz]
                    if ref:
                        best = max(ref, key=lambda r: iou(fc.box, r[0]))
                        row["iou_scrfd"] = round(float(iou(fc.box, best[0])), 3)
                        row["lm5_nme_vs_scrfd"] = round(float(np.linalg.norm(fc.landmarks - best[2], axis=1).mean()
                                                              / np.linalg.norm(best[2][0] - best[2][1])), 3)
                    rows.append(row)
                res[Path(f).name + (" (equalized)" if eq else "")] = rows
        report["retina"][str(rid)] = res
    if dll is not None:
        for name, m in models.items():
            from . import format as fmt
            lm = fmt.parse(ldmf_files[name])
            agg = {}
            for f in [p for p in frames if "watch" in p][:emu_frames] + stranger:
                st = emu_crosscheck(m, lm, lm.type_name, dll, _gray(f))
                agg[Path(f).name] = st
            report["emu"][name] = agg
    out_json.write_text(json.dumps(report, indent=1))
    return report
