"""CLI: python -m convnet {info,convert,convert-all,check,run}  (run from tools/winhello_models)."""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

from . import load, to_onnx
from .reference import classify

REPO = Path(__file__).resolve().parents[3]
SECURE_DIR = REPO / "models/windows_hello/facerecognitionengineadapterresourcessecure"
OUT_DIR = REPO / "models/windows_hello/converted/convnet"

# Resource id -> AntiSpoofingDecisionEngine slot (loader sub_18009D320 -> sub_1800C4B14(stream, slot)).
# The engine loads 115-119 when feature flag unk_1801246E8 is off, 121-125 when it is on.
SLOTS = {
    115: (11, "general IR CNN v2", "off"),
    116: (12, "diffusion (difference-image) IR CNN v2", "off"),
    117: (13, "color CNN v2", "off"),
    118: (14, "multi-task IR 'border'", "off"),
    119: (15, "multi-task IR 'HH'", "off"),
    121: (11, "general IR CNN v2", "on"),
    122: (12, "diffusion (difference-image) IR CNN v2", "on"),
    123: (13, "color CNN v2", "on"),
    124: (14, "multi-task IR 'border'", "on"),
    125: (15, "multi-task IR 'HH'", "on"),
}


def _rid(path: Path) -> int | None:
    try:
        return int(path.stem.rsplit("_", 1)[1])
    except (IndexError, ValueError):
        return None


def cmd_info(a):
    net = load(a.model)
    c, h, w = net.input_shape
    print(f"{a.model}: {len(net.layers)} layers, input {c}x{h}x{w}, trailer {len(net.trailer)} bytes")
    for L in net.layers:
        extra = []
        if L.ints:
            extra.append(f"ints={L.ints}")
        for k in ("weight", "bias", "mean"):
            b = getattr(L, k)
            if b is not None:
                extra.append(f"{k}={b.dims}")
        print(f"  {L.type:<11} {L.subtype:<5} {L.name}  <- {', '.join(L.inputs)}  {' '.join(extra)}")


def _convert(src: Path, dst: Path, engine_head: bool):
    net = load(src)
    rid = _rid(src)
    slot = SLOTS.get(rid) if src.parent.name.lower() == SECURE_DIR.name else None
    doc = f"Converted from {src.name} (Microsoft DNNTestLib conv_net_v1.0)."
    if slot:
        doc += f" AntiSpoofingDecisionEngine slot {slot[0]} ({slot[1]}), feature flag {slot[2]}."
    model = to_onnx(net, engine_head=engine_head, doc=doc)
    dst.parent.mkdir(parents=True, exist_ok=True)
    import onnx
    onnx.save(model, dst)
    meta = {"source": str(src), "layers": len(net.layers), "input_chw": net.input_shape,
            "has_cosine_head": net.head_weights() is not None,
            "input": "image: float32 [1,C,128,128], raw 0..255 values (normalised inside the graph)",
            "outputs": [o.name for o in model.graph.output]}
    if slot:
        meta.update(slot=slot[0], role=slot[1], feature_flag_unk_1801246E8=slot[2])
    dst.with_suffix(".json").write_text(json.dumps(meta, indent=2))
    print(f"{src.name} -> {dst}")


def cmd_convert(a):
    _convert(Path(a.model), Path(a.output), not a.no_head)


def cmd_convert_all(a):
    src = Path(a.src)
    files = sorted(src.glob("*.convnet"))
    if not files:
        sys.exit(f"no .convnet files in {src}")
    for f in files:
        _convert(f, Path(a.dst) / (f.stem + ".onnx"), True)


def cmd_run(a):
    from PIL import Image

    net = load(a.model)
    c, h, w = net.input_shape
    img = Image.open(a.image).convert("L" if c == 1 else "RGB").resize((w, h))
    arr = np.asarray(img)
    if c == 3:
        arr = arr[:, :, ::-1]  # assumed BGR channel order, see doc
    print("reference p =", classify(net, arr))


def cmd_check(a):
    """Layer-by-layer ONNX (onnxruntime) vs NumPy reference on a random u8 image."""
    import onnxruntime as ort

    from . import reference as ref

    worst_all = 0.0
    for path in a.models:
        net = load(path)
        c, h, w = net.input_shape
        img = np.random.default_rng(a.seed).integers(0, 256, (h, w, c), dtype=np.uint8)
        model = to_onnx(net, expose_all=True)
        sess = ort.InferenceSession(model.SerializeToString(), providers=["CPUExecutionProvider"])
        got = dict(zip([o.name for o in sess.get_outputs()],
                       sess.run(None, {"image": img.transpose(2, 0, 1)[None].astype(np.float32)})))
        acts = ref.forward(net, img, keep_all=True)
        worst, n = 0.0, 0
        for L in net.layers:
            o = got.get(L.name, got.get(L.name + "__layer"))
            if o is None:
                continue
            o = o[0].transpose(1, 2, 0) if o[0].ndim == 3 else o[0].reshape(acts[L.name].shape)
            ra = acts[L.name]
            worst = max(worst, float(np.abs(o - ra).max() / (np.abs(ra).max() + 1e-12)))
            n += 1
        dp = float(np.abs(got["prob"][0] - ref.classify(net, img[..., 0] if c == 1 else img)).max())
        worst_all = max(worst_all, worst)
        print(f"{Path(path).name}: {n}/{len(net.layers)} layers compared, max rel err {worst:.2e}, |prob diff| {dp:.2e}")
    print(f"overall max rel err {worst_all:.2e}")


def main(argv=None):
    p = argparse.ArgumentParser(prog="python -m convnet", description=__doc__)
    sp = p.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("info"); s.add_argument("model"); s.set_defaults(fn=cmd_info)
    s = sp.add_parser("convert"); s.add_argument("model"); s.add_argument("-o", "--output", required=True)
    s.add_argument("--no-head", action="store_true", help="omit the engine's cosine head / softmax"); s.set_defaults(fn=cmd_convert)
    s = sp.add_parser("convert-all"); s.add_argument("--src", default=str(SECURE_DIR)); s.add_argument("--dst", default=str(OUT_DIR))
    s.set_defaults(fn=cmd_convert_all)
    s = sp.add_parser("check", help="layer-by-layer ONNX vs NumPy reference")
    s.add_argument("models", nargs="+"); s.add_argument("--seed", type=int, default=0); s.set_defaults(fn=cmd_check)
    s = sp.add_parser("run", help="run the NumPy reference on an already aligned 128x128 crop")
    s.add_argument("model"); s.add_argument("image"); s.set_defaults(fn=cmd_run)
    a = p.parse_args(argv)
    a.fn(a)


if __name__ == "__main__":
    main()
