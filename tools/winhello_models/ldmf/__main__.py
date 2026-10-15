"""CLI: python -m ldmf {info,convert,detect,validate}  (run from tools/winhello_models with its .venv)."""
import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
RES = ROOT / "models/windows_hello/FaceDetectorResources"
OUT = ROOT / "models/windows_hello/converted/ldmf"


def main(argv=None):
    ap = argparse.ArgumentParser(prog="ldmf")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("info"); p.add_argument("files", nargs="+", type=Path)
    p = sub.add_parser("convert", help="LDMF + FaceTrackerInternal.dll -> self-contained .npz")
    p.add_argument("files", nargs="*", type=Path, default=[RES / "MODEL_FILES_101.ldmf", RES / "MODEL_FILES_102.ldmf"])
    p.add_argument("--dll", type=Path, required=True, help="FaceTrackerInternal.dll (build 26300)")
    p.add_argument("--structure", type=Path, default=OUT / "hardcoded_structure.npz",
                   help="output of ldmf/extract (stage-0 trees + fern tests)")
    p.add_argument("-o", "--out", type=Path, default=OUT)
    p = sub.add_parser("detect"); p.add_argument("images", nargs="+", type=Path)
    p.add_argument("--model", type=Path, default=OUT / "MODEL_FILES_101_infrared.npz")
    p.add_argument("--min-face", type=int, default=48); p.add_argument("--dense", action="store_true")
    p.add_argument("--draw", type=Path, help="write annotated PNGs into this directory")
    p = sub.add_parser("validate")
    p.add_argument("--frames", type=Path, default=Path("/home/cole/SiteResearch/windows_hello/re/validation_frames"))
    p.add_argument("--scrfd", type=Path, default=ROOT / "models/det_10g.onnx")
    p.add_argument("--dll", type=Path, help="FaceTrackerInternal.dll: enables the emulator cross-check")
    p.add_argument("-o", "--out", type=Path, default=OUT / "validation.json")
    a = ap.parse_args(argv)
    if a.cmd == "info":
        from . import format as fmt
        for f in a.files:
            print(json.dumps(fmt.summary(fmt.parse(f)), indent=1))
    elif a.cmd == "convert":
        from .convert import convert
        for f in a.files:
            print(convert(f, a.dll, a.structure, a.out))
    elif a.cmd == "detect":
        import numpy as np
        from PIL import Image, ImageDraw
        from . import jda
        m = jda.Model(a.model)
        for f in a.images:
            g = np.asarray(Image.open(f).convert("L"))
            dets = jda.detect(m, g, min_face=a.min_face, dense=a.dense)
            print(f, [{"box": d.box, "score": round(d.score, 3)} for d in dets])
            if a.draw:
                a.draw.mkdir(parents=True, exist_ok=True)
                im = Image.fromarray(g).convert("RGB"); dr = ImageDraw.Draw(im)
                for d in dets:
                    dr.rectangle(d.box, outline=(0, 255, 0))
                    for x, y in d.landmarks:
                        dr.point((float(x), float(y)), fill=(255, 0, 0))
                im.save(a.draw / (f.stem + "_ldmf.png"))
    elif a.cmd == "validate":
        from . import jda, validate
        models = {"101_infrared": jda.Model(OUT / "MODEL_FILES_101_infrared.npz"),
                  "102_lowend": jda.Model(OUT / "MODEL_FILES_102_lowend.npz")}
        files = {"101_infrared": RES / "MODEL_FILES_101.ldmf", "102_lowend": RES / "MODEL_FILES_102.ldmf"}
        rep = validate.run(models, a.frames, a.scrfd, a.dll, files, a.out)
        print(json.dumps(rep, indent=1))


if __name__ == "__main__":
    main()
