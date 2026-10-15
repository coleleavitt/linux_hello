"""CLI: python -m lbf {convert,align,validate}

  convert   parse MODEL_FILES 102/104-107 -> converted/lbf/*.npz/*.json + head-pose ONNX
  align     run the aligner + head pose on one image (SCRFD box unless --box) and write an overlay
  validate  run on the owner IR validation frames: SCRFD 5-point error, emulator cross-check, overlays
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

import numpy as np

from .format import parse_lbf, load_lda_regressor, LbfModel, LdaRegressor
from .aligner import align
from .pose import head_pose, TEMPLATE_27, LBP_LUT, K_SIGMA2, K_SIGMA4
from .crop import engine_crop_128

REPO = Path(__file__).resolve().parents[3]
SRC = REPO / "models/windows_hello/facerecognitionsensoradapterresources"
OUT = REPO / "models/windows_hello/converted/lbf"
DLL = Path("/home/cole/SiteResearch/windows_hello/re/bin/FaceProcessorCore.dll")
ENGINE_DLL = Path("/home/cole/SiteResearch/windows_hello/re/bin/FaceRecognitionEngineAdapter.dll")
FRAMES = Path("/home/cole/SiteResearch/windows_hello/re/validation_frames")
SCRFD = REPO / "models/det_10g.onnx"

# INFERRED names (from the mean shape geometry, anchor groups in ComputeAnchorPoints 0x1800372c8 and the
# RetinaEngine 5-point mapping in RetinaEngine::ComputeAlignment 0x18003d270). Image left/right.
POINT_NAMES = [
    "eye_center_L", "eye_center_R", "nose_tip", "mouth_corner_L", "mouth_corner_R",
    "brow_outer_L", "brow_inner_L", "eye_outer_L", "eye_top_L", "eye_bottom_L", "eye_inner_L",
    "brow_inner_R", "brow_outer_R", "eye_inner_R", "eye_top_R", "eye_bottom_R", "eye_outer_R",
    "nose_root_L", "nose_root_R", "nose_side_upper_L", "nose_side_upper_R", "nose_wing_L", "nose_wing_R",
    "upper_lip_top", "upper_lip_bottom", "lower_lip_top", "lower_lip_bottom",
    "glabella", "nostril_L", "nose_bottom", "nostril_R",
]


def load_models(src: Path = SRC):
    model = parse_lbf((src / "MODEL_FILES_102.cefa").read_bytes())
    yaw = load_lda_regressor((src / "MODEL_FILES_107.bin").read_bytes(), (src / "MODEL_FILES_106.bin").read_bytes())
    pitch = load_lda_regressor((src / "MODEL_FILES_105.bin").read_bytes(), (src / "MODEL_FILES_104.bin").read_bytes())
    return model, yaw, pitch


def headpose_onnx(yaw: LdaRegressor, pitch: LdaRegressor, path: Path):
    """LDA projection + nearest-centroid lookup (the no-prior path of LDARegressor::Regress) as ONNX."""
    import onnx
    from onnx import helper, TensorProto, numpy_helper as nh
    nodes, inits = [], []
    for name, r in (("pitch", pitch), ("yaw", yaw)):
        inits += [nh.from_array(r.mean[None].astype(np.float32), f"{name}_mean"),
                  nh.from_array(np.ascontiguousarray(r.projection.T), f"{name}_lda"),
                  nh.from_array(r.centroids.astype(np.float32), f"{name}_centroids"),
                  nh.from_array(r.labels.astype(np.float32), f"{name}_labels"),
                  nh.from_array(np.array([1], np.int64), f"{name}_axis")]
        nodes += [
            helper.make_node("Sub", ["lbp_feature", f"{name}_mean"], [f"{name}_c"]),
            helper.make_node("MatMul", [f"{name}_c", f"{name}_lda"], [f"{name}_proj"]),
            helper.make_node("Sub", [f"{name}_proj", f"{name}_centroids"], [f"{name}_d"]),
            helper.make_node("Mul", [f"{name}_d", f"{name}_d"], [f"{name}_d2"]),
            helper.make_node("ReduceSum", [f"{name}_d2", f"{name}_axis"], [f"{name}_dist2"], keepdims=0),
            helper.make_node("ArgMin", [f"{name}_dist2"], [f"{name}_idx"], axis=0, keepdims=1,
                             select_last_index=0),
            helper.make_node("Gather", [f"{name}_labels", f"{name}_idx"], [name], axis=0),
        ]
    graph = helper.make_graph(
        nodes, "winhello_ir_headpose_lda_knn",
        [helper.make_tensor_value_info("lbp_feature", TensorProto.FLOAT, [1, 7139])],
        [helper.make_tensor_value_info("pitch", TensorProto.FLOAT, [1]),
         helper.make_tensor_value_info("yaw", TensorProto.FLOAT, [1])], inits)
    m = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)], producer_name="linux_hello lbf converter")
    m.ir_version = 10  # onnxruntime 1.31 accepts IR <= 13
    m.doc_string = ("Windows Hello IrFaceOrientation LDA+nearest-centroid head pose. Input: 7139 uniform-LBP "
                    "histogram feature (see lbf/pose.py). Output degrees. Research use only.")
    onnx.checker.check_model(m)
    onnx.save(m, str(path))


def convert(out: Path = OUT):
    out.mkdir(parents=True, exist_ok=True)
    model, yaw, pitch = load_models()
    arrays = {"mean_shape": model.mean_shape, "unit_box": model.unit_box}
    stages_meta = []
    for i, st in enumerate(model.stages):
        nodes = np.stack(st.trees) if len({len(t) for t in st.trees}) == 1 else None
        arrays[f"stage{i}_nodes"] = nodes
        arrays[f"stage{i}_weights_i16"] = st.regressor.raw
        stages_meta.append(dict(num_trees=st.num_trees, trees_per_point=st.trees_per_point,
                                nodes_per_tree=len(st.trees[0]), regressor_rows=st.regressor.rows,
                                regressor_cols=st.regressor.cols, weight_scale=st.regressor.scale,
                                compressed=st.regressor.compressed))
    arrays["classifier_weights"] = model.classifier.raw
    np.savez_compressed(out / "lbf_aligner.npz", **arrays)
    meta = dict(source="facerecognitionsensoradapterresources.dll MODEL_FILES 102 (LbfAligner database)",
                header_hex=model.header.hex(), num_points=model.num_points, point_names=POINT_NAMES,
                mean_shape=model.mean_shape.round(6).tolist(), stages=stages_meta,
                classifier=dict(rows=model.classifier.rows, cols=model.classifier.cols,
                                compressed=model.classifier.compressed, decision="score > 0 -> face accepted"),
                node_layout="x1,y1,x2,y2,thr: float32; children: u32 (low16 = child if thr >= feature, "
                            "high16 otherwise; bit15 = leaf, bits0-14 = node index or global leaf row)")
    (out / "lbf_aligner.json").write_text(json.dumps(meta, indent=1))
    np.savez_compressed(out / "headpose_lda_knn.npz",
                        yaw_mean=yaw.mean, yaw_projection=yaw.projection, yaw_labels=yaw.labels,
                        yaw_centroids=yaw.centroids, pitch_mean=pitch.mean, pitch_projection=pitch.projection,
                        pitch_labels=pitch.labels, pitch_centroids=pitch.centroids, template_27=TEMPLATE_27,
                        lbp_lut=LBP_LUT, k_sigma2=K_SIGMA2, k_sigma4=K_SIGMA4)
    (out / "headpose_lda_knn.json").write_text(json.dumps(dict(
        source="MODEL_FILES 104 PITCH_KNN, 105 PITCH_LDA, 106 YAW_KNN, 107 YAW_LDA",
        yaw=dict(lda_dim=int(yaw.projection.shape[0]), knn_k=yaw.knn_k, knn_gamma=yaw.knn_gamma,
                 labels=[float(yaw.labels[0]), float(yaw.labels[-1])], n=len(yaw.labels)),
        pitch=dict(lda_dim=int(pitch.projection.shape[0]), knn_k=pitch.knn_k, knn_gamma=pitch.knn_gamma,
                   labels=[float(pitch.labels[0]), float(pitch.labels[-1])], n=len(pitch.labels)),
        frontal_limits=dict(yaw=20.0, pitch=25.0)), indent=1))
    headpose_onnx(yaw, pitch, out / "headpose_lda_knn.onnx")
    print(f"wrote {out}")


def draw_overlay(img: np.ndarray, pts: np.ndarray, path: Path, kps=None, box=None, text=""):
    from PIL import Image, ImageDraw
    rgb = Image.fromarray(img).convert("RGB")
    scale = 2
    rgb = rgb.resize((rgb.width * scale, rgb.height * scale), Image.NEAREST)
    d = ImageDraw.Draw(rgb)
    if box is not None:
        d.rectangle([v * scale for v in box], outline=(80, 80, 255))
    for i, (x, y) in enumerate(pts):
        c = (255, 40, 40) if i < 5 else (40, 255, 40)
        d.ellipse([x * scale - 3, y * scale - 3, x * scale + 3, y * scale + 3], outline=c, width=2)
    if kps is not None:
        for x, y in kps:
            d.line([x * scale - 5, y * scale, x * scale + 5, y * scale], fill=(255, 255, 0))
            d.line([x * scale, y * scale - 5, x * scale, y * scale + 5], fill=(255, 255, 0))
    if text:
        d.text((6, 6), text, fill=(255, 255, 0))
    rgb.save(path)


def _load_gray(path):
    from PIL import Image
    return np.asarray(Image.open(path).convert("L"))


def cmd_align(args):
    model, yaw, pitch = load_models()
    img = _load_gray(args.image)
    kps = None
    if args.box:
        box = args.box
    else:
        from .scrfd import Scrfd
        dets = Scrfd(str(SCRFD)).detect(img)
        if not dets:
            sys.exit("no face found")
        box, _, kps = dets[0]
    rect = tuple(int(v) for v in box)
    pts, score = align(model, img, rect=rect)
    for _ in range(args.track):
        pts, score = align(model, img, prev=pts)
    hp = head_pose(img, pts, yaw, pitch)
    print(json.dumps(dict(rect=rect, classifier_score=score, accepted=score > 0, points=pts.round(2).tolist(),
                          pose={k: v for k, v in hp.items()}), indent=1))
    if args.overlay:
        draw_overlay(img, pts, Path(args.overlay), kps, box,
                     f"score {score:.2f} pitch {hp['pitch']:.0f} yaw {hp['yaw']:.0f}")


def cmd_validate(args):
    from .scrfd import Scrfd
    model, yaw, pitch = load_models()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    det = Scrfd(str(SCRFD))
    emu = None
    if DLL.exists() and not args.no_emu:
        from .emu import MsDll, EmuAligner, EmuPose
        dll = MsDll(str(DLL))
        emu = (EmuAligner(dll, model), EmuPose(dll, yaw, pitch),
               MsDll(str(ENGINE_DLL)) if ENGINE_DLL.exists() else None)
    files = sorted(FRAMES.glob("ir_*_lit.png")) + sorted(FRAMES.glob("watch_*_ir_lit.png"))
    names5 = POINT_NAMES[:5]
    rows = []
    crops = []
    for f in files:
        img = _load_gray(f)
        box, conf, kps = det.detect(img)[0]
        rect = tuple(int(v) for v in box)
        pts, score = align(model, img, rect=rect)
        tpts, tscore = align(model, img, prev=pts)
        hp = head_pose(img, pts, yaw, pitch)
        hp_prior = head_pose(img, tpts, yaw, pitch, prior_yaw=hp["yaw"], prior_pitch=hp["pitch"])
        crops.append(engine_crop_128(img, pts[:5]))
        iod = float(np.linalg.norm(kps[0] - kps[1]))
        e = np.linalg.norm(pts[:5] - kps, axis=1)
        et = np.linalg.norm(tpts[:5] - kps, axis=1)
        row = dict(frame=f.name, scrfd_box=[round(float(v), 1) for v in box], scrfd_conf=round(conf, 3),
                   classifier_score=round(score, 4), track_score=round(tscore, 4),
                   err_px={n: round(float(v), 2) for n, v in zip(names5, e)},
                   err_px_track={n: round(float(v), 2) for n, v in zip(names5, et)},
                   mean_err_px=round(float(e.mean()), 2), nme_iod=round(float(e.mean() / iod), 4),
                   iod_px=round(iod, 1),
                   pose=dict(pitch=hp["pitch"], yaw=hp["yaw"], roll=round(hp["roll"], 3), flags=hp["flags"],
                             frontal=hp["frontal"], frontal_margin=hp["frontal_margin"]),
                   pose_track_with_prior=dict(pitch=round(hp_prior["pitch"], 3), yaw=round(hp_prior["yaw"], 3)))
        if emu:
            hr, epts, rej = emu[0].predict(img, rect=rect)
            _, etp, trej = emu[0].predict(img, prev=epts)
            ep = emu[1].compute(img, epts, timestamp=10**9)
            ep2 = emu[1].compute(img, etp, timestamp=10**9, prev_ts=10**9 - 1000,
                                 prev_yaw=ep["yaw"], prev_pitch=ep["pitch"])
            fe = head_pose(img, pts, yaw, pitch, debug=True)["feature"]
            crop_diff = None
            if emu[2] is not None:
                from .emu import emu_engine_crop
                _, ecrop = emu_engine_crop(emu[2], img, pts[:5])
                crop_diff = int(np.abs(ecrop[:, :, 0].astype(int) - crops[-1].astype(int)).max())
            row["emulator"] = dict(
                align_max_abs_diff=float(np.abs(epts - pts).max()),
                track_max_abs_diff=float(np.abs(etp - tpts).max()),
                rejected_match=bool(rej == (score <= 0)) and bool(trej == (tscore <= 0)),
                feature_max_abs_diff=float(np.abs(ep["feature"] - fe).max()),
                pitch=ep["pitch"], yaw=ep["yaw"], roll=round(ep["roll"], 3), flags=list(ep["flags"]),
                frontal_margin=ep["frontal_margin"],
                pitch_prior=round(ep2["pitch"], 3), yaw_prior=round(ep2["yaw"], 3),
                engine_crop_max_abs_diff=crop_diff)
        rows.append(row)
        draw_overlay(img, pts, out / f"overlay_{f.stem}.png", kps, box,
                     f"{f.stem} score {score:.2f} err {e.mean():.1f}px pitch {hp['pitch']:.0f} yaw {hp['yaw']:.0f}")
        print(f.name, row["mean_err_px"], row["nme_iod"], row.get("emulator", {}).get("align_max_abs_diff"))
    from PIL import Image
    Image.fromarray(np.concatenate(crops, 1)).save(out / "engine_crops_128.png")
    allerr = np.array([[r["err_px"][n] for n in names5] for r in rows])
    allerr_t = np.array([[r["err_px_track"][n] for n in names5] for r in rows])
    summary = dict(frames=len(rows),
                   mean_err_px_per_point={n: round(float(v), 2) for n, v in zip(names5, allerr.mean(0))},
                   mean_err_px=round(float(allerr.mean()), 2),
                   mean_err_px_track=round(float(allerr_t.mean()), 2),
                   mean_nme_iod=round(float(np.mean([r["nme_iod"] for r in rows])), 4),
                   all_accepted=all(r["classifier_score"] > 0 for r in rows))
    if emu:
        summary["emulator_bit_exact_alignment"] = all(r["emulator"]["align_max_abs_diff"] == 0 and
                                                       r["emulator"]["track_max_abs_diff"] == 0 for r in rows)
        summary["emulator_feature_max_abs_diff"] = max(r["emulator"]["feature_max_abs_diff"] for r in rows)
        summary["emulator_pose_match"] = all(r["emulator"]["pitch"] == r["pose"]["pitch"] and
                                             r["emulator"]["yaw"] == r["pose"]["yaw"] and
                                             [bool(x) for x in r["emulator"]["flags"]] == r["pose"]["flags"]
                                             for r in rows)
        if emu[2] is not None:
            summary["emulator_engine_crop_max_abs_diff"] = max(r["emulator"]["engine_crop_max_abs_diff"] for r in rows)
        summary["emulator_prior_pose_max_abs_diff"] = max(
            max(abs(r["emulator"]["pitch_prior"] - r["pose_track_with_prior"]["pitch"]),
                abs(r["emulator"]["yaw_prior"] - r["pose_track_with_prior"]["yaw"])) for r in rows)
    (out / "validation.json").write_text(json.dumps(dict(summary=summary, frames=rows), indent=1))
    print(json.dumps(summary, indent=1))


def main(argv=None):
    p = argparse.ArgumentParser(prog="python -m lbf", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("convert")
    c.add_argument("--out", default=str(OUT))
    a = sub.add_parser("align")
    a.add_argument("image")
    a.add_argument("--box", type=int, nargs=4, metavar=("L", "T", "R", "B"))
    a.add_argument("--track", type=int, default=0, help="extra tracking passes (TrackFace re-init)")
    a.add_argument("--overlay")
    v = sub.add_parser("validate")
    v.add_argument("--out", default=str(OUT))
    v.add_argument("--no-emu", action="store_true")
    args = p.parse_args(argv)
    if args.cmd == "convert":
        convert(Path(args.out))
    elif args.cmd == "align":
        cmd_align(args)
    else:
        cmd_validate(args)


if __name__ == "__main__":
    main()
