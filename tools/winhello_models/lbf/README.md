# lbf — Windows Hello legacy IR landmark aligner + head pose (research converter)

Parses and runs `facerecognitionsensoradapterresources.dll` MODEL_FILES 102 (LbfAligner, 31 points) and
104–107 (IrFaceOrientation pitch/yaw LDA+KNN), plus the engine's 128×128 liveness crop.
Full format spec, derivation and validation: `docs/winhello-models/lbf.md`. Outputs are local only
(`models/windows_hello/converted/lbf/`, git-ignored).

```
cd tools/winhello_models
.venv/bin/python -m lbf convert
.venv/bin/python -m lbf align IMG.png [--box L T R B] [--track N] [--overlay out.png]
.venv/bin/python -m lbf validate [--no-emu]
```

```python
from lbf.__main__ import load_models
from lbf.aligner import align
from lbf.pose import head_pose
from lbf.crop import engine_crop_128
model, yaw, pitch = load_models()
pts, score = align(model, gray, rect=(l, t, r, b))      # score <= 0: Microsoft rejects the face
pose = head_pose(gray, pts, yaw, pitch)                 # pitch, yaw, flags, frontal
crop = engine_crop_128(gray, pts[:5])                   # CNN input crop
```

`emu.py` (cross-check against the original DLLs under Unicorn) needs `unicorn` (installed in the shared venv)
and the DLLs in `~/SiteResearch/windows_hello/re/bin/`.
