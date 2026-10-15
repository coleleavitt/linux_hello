"""CLI: unpack Windows Hello `.cefa` recognition containers.

    cd tools/winhello_models
    .venv/bin/python -m cefa info  FILE.cefa...
    .venv/bin/python -m cefa unpack FILE.cefa... -o OUTDIR

`unpack` writes per container:
  <stem>.json                 header, tech id, calibration tables (FPR, TAR, distance)
  <stem>.blob<i>.ort/.onnx    ORT-format payloads and their re-serialised ONNX form
  <stem>.convnet              conv_net_v1 payload of an LDMF/NNDR blob
  <stem>.onnx                 ONNX of that conv_net (needs ../convnet, conv-convnet's converter)

"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import container

def describe(path: Path) -> dict:
    c = container.parse(path.read_bytes())
    return {
        "file": path.name,
        "format_version": c.version,
        "reserved_bytes_3_5": c.reserved.hex(),
        "tech": c.tech,
        "flag": c.flag,
        "template_bytes": container.TEMPLATE_BYTES.get(c.tech),
        "calibration": [
            {"hardware_model": t.hardware_model, "ids": list(t.ids), "rows_fpr_tar_distance": t.rows.tolist()}
            for t in c.tables
        ],
        "blobs": [{"offset": b.offset, "size": len(b.data), "kind": b.kind} for b in c.blobs],
    }


def _convnet_to_onnx(convnet_path: Path, onnx_path: Path) -> str:
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    try:
        import onnx
        from convnet import load, to_onnx
    except ImportError as e:
        return f"skipped ONNX export ({e})"
    onnx.save(to_onnx(load(str(convnet_path)), engine_head=False), str(onnx_path))
    return f"wrote {onnx_path.name}"


def unpack(path: Path, out: Path) -> None:
    import onnx

    from . import ort2onnx

    out.mkdir(parents=True, exist_ok=True)
    c = container.parse(path.read_bytes())
    stem = path.stem
    info = describe(path)
    (out / f"{stem}.json").write_text(json.dumps(info, indent=1))
    for i, b in enumerate(c.blobs):
        if b.kind == "ort":
            (out / f"{stem}.blob{i}.ort").write_bytes(b.data)
            m = ort2onnx.convert(b.data)
            onnx.checker.check_model(m)
            onnx.save(m, str(out / f"{stem}.blob{i}.onnx"))
            print(f"{stem}: blob{i} ORT -> {stem}.blob{i}.onnx")
        elif b.kind == "ldmf":
            cn = out / f"{stem}.convnet"
            cn.write_bytes(container.unwrap_ldmf(b.data))
            print(f"{stem}: blob{i} LDMF -> {cn.name}; {_convnet_to_onnx(cn, out / f'{stem}.onnx')}")
        else:
            (out / f"{stem}.blob{i}.bin").write_bytes(b.data)
            print(f"{stem}: blob{i} unknown payload saved")


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(prog="cefa")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("info")
    p.add_argument("files", nargs="+", type=Path)
    p = sub.add_parser("unpack")
    p.add_argument("files", nargs="+", type=Path)
    p.add_argument("-o", "--out", type=Path, required=True)
    a = ap.parse_args(argv)
    for f in a.files:
        if a.cmd == "info":
            d = describe(f)
            for t in d["calibration"]:
                t["rows_fpr_tar_distance"] = f"{len(t['rows_fpr_tar_distance'])} rows"
            print(json.dumps(d, indent=1))
        else:
            unpack(f, a.out)


if __name__ == "__main__":
    main()
