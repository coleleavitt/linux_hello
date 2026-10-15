"""Convert an LDMF model + FaceTrackerInternal.dll constants into one self-contained .npz for `ldmf.jda`."""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from . import format as fmt
from . import hardcoded as hc


def convert(ldmf_path: Path, dll_path: Path, structure_npz: Path, out_dir: Path) -> Path:
    m = fmt.parse(ldmf_path)
    kind = m.type_name
    dll = hc.Dll(dll_path)
    tables = hc.read_tables(dll)
    st = np.load(structure_npz)
    L = m.n_landmarks
    s0 = st[f"{kind}_trees"]
    T = s0.shape[0]
    n_file_trees = m.tree_info.size
    assert n_file_trees % T == 0
    n_stages = 1 + n_file_trees // T
    assert m.leaf_weights.size == n_stages * T * 32
    assert np.all(m.tree_info["n_nodes"] == 63)
    for k in range(n_stages - 1):  # TreeInfo.leaf_end is the cumulative leaf count inside a stage
        assert np.all(m.tree_info["leaf_end"][k * T:(k + 1) * T] == 32 * np.arange(1, T + 1))
    leaves_per_stage = T * 32
    shp = m.shapes
    assert shp.shape[0] == 1 + leaves_per_stage + (n_stages - 1) * (1 + leaves_per_stage)
    rest = shp[1 + leaves_per_stage:].reshape(n_stages - 1, 1 + leaves_per_stage, L, 2)
    out = dict(
        n_landmarks=L, trees_per_stage=T, n_stages=n_stages, model_type=m.model_type,
        init_score=hc.INIT_SCORE[kind], score_scale=hc.SCORE_SCALE,
        leaf_weights=m.leaf_weights, stage0_trees=s0, stage0_reject=st[f"{kind}_thr"],
        nodes=m.nodes.view(np.int32).reshape(-1, 11), reject=m.tree_info["reject_thr"],
        mean_shape=shp[0], deltas0=shp[1:1 + leaves_per_stage], stage_bias=rest[:, 0], stage_deltas=rest[:, 1:],
        ref_shape=tables[f"ref_shape_{kind}"], hog_weights=m.hog_weights, hog_threshold=np.float32(m.hog_threshold),
        hog_template5=tables["hog_template5"], atan_lut=tables["atan_lut"],
        fern_tests=st["fern_tests"], fern_reject=st["fern_thr"], fern_weights=tables["fern_weights"],
        fern_init_score=hc.FERN_INIT_SCORE, fern_min_passed=hc.FERN_MIN_PASSED,
    )
    assert np.array_equal(st["fern_w"], tables["fern_weights"])
    out_dir.mkdir(parents=True, exist_ok=True)
    stem = ldmf_path.stem
    npz = out_dir / f"{stem}_{kind}.npz"
    np.savez_compressed(npz, **out)
    meta = fmt.summary(m) | {"dll_sha256": dll.sha256, "trees_per_stage": T, "n_stages": n_stages,
                             "npz": npz.name, "stage0_source": "FaceTrackerInternal.dll code (extracted)"}
    (out_dir / f"{stem}_{kind}.json").write_text(json.dumps(meta, indent=1))
    return npz
