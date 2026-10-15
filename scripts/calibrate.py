#!/usr/bin/env python3
"""Calibrate the linux_hello face-match threshold on LFW embeddings (numpy only).

Input: files written by `cargo run --release --example embed_dir`:
  <prefix>.<variant>.f32  (N x 512 float32, zero rows where no face was found)
  <prefix>.<variant>.tsv  (idx, path, faces, score, bbox)
Several comma-separated prefixes may be given; later ones override rows by path.

Usage: calibrate.py <prefix>[,<prefix>...] <pairs.txt> <same_person.tsv> <out.json> [variant ...]

A comparison is accepted when cosine > threshold (strict, as FaceMatcher does).
"""
import json
import sys

import numpy as np

DIM = 512
TAIL_MIN = 0.15          # impostor scores above this are kept exactly
BLOCK = 2048
FARS = [1e-3, 1e-4, 1e-5, 1e-6, 1e-7]
PRINTS, FRAMES, ATTEMPT_FAR = 15, 16, 1e-5
GRID = [round(0.30 + 0.025 * k, 3) for k in range(13)]


def load(prefixes, variant):
    emb, rows, at = None, [], {}
    for prefix in prefixes.split(","):
        e = np.fromfile(f"{prefix}.{variant}.f32", dtype="<f4").reshape(-1, DIM)
        r = [l.rstrip("\n").split("\t") for l in open(f"{prefix}.{variant}.tsv")][1:]
        if emb is None:
            emb, rows = e.copy(), r
            at = {row[1]: k for k, row in enumerate(rows)}
            continue
        for k, row in enumerate(r):
            emb[at[row[1]]], rows[at[row[1]]] = e[k], row
    paths = [r[1] for r in rows]
    faces = np.array([int(r[2]) for r in rows])
    return emb, paths, faces


def persons(names, same_person):
    """Map identity labels to person ids, merging label pairs known to be the same person."""
    parent = {n: n for n in names}

    def find(n):
        while parent[n] != n:
            n = parent[n]
        return n

    for a, b in same_person:
        parent[find(a)] = find(b)
    roots = {n: find(n) for n in names}
    _, pid = np.unique([roots[n] for n in names], return_inverse=True)
    return pid


def impostor_and_genuine(emb, label, person):
    """Exact upper tail (> TAIL_MIN) of all different-person scores, and all same-label scores."""
    n = len(emb)
    n_imp, tail, tail_ij, genuine = 0, [], [], []
    for a in range(0, n, BLOCK):
        s = emb[a:a + BLOCK] @ emb[a:].T
        i = np.arange(a, min(a + BLOCK, n))[:, None]
        j = np.arange(a, n)[None, :]
        upper = j > i
        imp = upper & (person[i] != person[j])
        n_imp += int(imp.sum())
        genuine.append(s[upper & (label[i] == label[j])])
        ii, jj = np.nonzero(imp & (s > TAIL_MIN))
        tail.append(s[ii, jj])
        tail_ij.append(np.stack([ii + a, jj + a], 1))
    tail = np.concatenate(tail)
    order = np.argsort(-tail)
    return n_imp, tail[order], np.concatenate(tail_ij)[order], np.concatenate(genuine)


def threshold_at(tail, n_imp, far):
    """Smallest threshold t with count(score > t) <= far * n_imp."""
    k = int(np.floor(far * n_imp))
    return float(tail[k]) if k < len(tail) else None


def tail_fit(tail, n_imp, lo=1e-6, hi=1e-3):
    """Fit log10(FAR) linearly in the score on the [lo, hi] FAR band (exponential tail)."""
    far = np.arange(1, len(tail) + 1) / n_imp
    band = (far <= hi) & (far >= lo)
    slope, icept = np.polyfit(tail[band], np.log10(far[band]), 1)
    return float(slope), float(icept)


def lfw_pairs(pairs_file, index, emb, ok):
    """Scores of the standard 6000 view-2 pairs where both faces were found."""
    scores, same = [], []
    for l in open(pairs_file).read().split("\n")[1:]:
        p = l.split("\t")
        if len(p) == 3:
            a, b, s = (p[0], int(p[1])), (p[0], int(p[2])), True
        elif len(p) == 4:
            a, b, s = (p[0], int(p[1])), (p[2], int(p[3])), False
        else:
            continue
        a, b = index[a], index[b]
        if ok[a] and ok[b]:
            scores.append(float(emb[a] @ emb[b]))
            same.append(s)
    return np.array(scores), np.array(same)


def simulate(emb, label, person, ok):
    """Each identity with >= PRINTS images is an enrolled user holding PRINTS prints (its first
    PRINTS images).
    impostor probe:   one image of another person, score = max over the PRINTS prints;
    impostor attempt: up to FRAMES images of one other person stand in for the frames of one
                      unlock attempt, score = max over prints and frames;
    genuine probe:    the user's own images beyond the first PRINTS, max over prints."""
    idx = np.nonzero(ok)[0]
    lab, per = label[idx], person[idx]
    users, counts = np.unique(lab, return_counts=True)
    users = users[counts >= PRINTS]
    order = np.argsort(per, kind="stable")
    sper = per[order]
    starts = np.r_[0, np.nonzero(np.diff(sper))[0] + 1]
    rank = np.arange(len(sper)) - np.repeat(starts, np.diff(np.r_[starts, len(sper)]))
    frame = rank < FRAMES
    f_starts = np.r_[0, np.nonzero(np.diff(sper[frame]))[0] + 1]
    f_person = sper[frame][f_starts]
    probe, attempt, genuine = [], [], []
    for u in users:
        own = idx[lab == u]
        prints, rest = own[:PRINTS], own[PRINTS:]
        upers = person[own[0]]
        best = (emb[idx] @ emb[prints].T).max(1)[order]
        probe.append(best[sper != upers])
        attempt.append(np.maximum.reduceat(best[frame], f_starts)[f_person != upers])
        if len(rest):
            genuine.append((emb[rest] @ emb[prints].T).max(1))
    return len(users), np.concatenate(probe), np.concatenate(attempt), np.concatenate(genuine)


def analyse(prefixes, pairs_file, same_person, variant):
    emb, paths, faces = load(prefixes, variant)
    names = [p.split("/")[0] for p in paths]
    _, label = np.unique(names, return_inverse=True)
    person = persons(names, same_person)
    ok = faces > 0
    n_imp, tail, tail_ij, gen = impostor_and_genuine(emb[ok], label[ok], person[ok])
    okpaths = np.array(paths)[ok]

    def far(t):
        c = int((tail > t).sum())
        return c, c / n_imp

    res = {"variant": variant, "same_person_pairs_excluded": len(same_person),
           "images": len(paths), "no_face": int((~ok).sum()),
           "no_face_paths": list(np.array(paths)[~ok]),
           "impostor_pairs": n_imp, "genuine_pairs": int(len(gen)), "thresholds": []}
    for f in FARS:
        t = threshold_at(tail, n_imp, f)
        if t is not None:
            res["thresholds"].append({"far": f, "threshold": t, "impostors_above": far(t)[0],
                                      "frr_all_genuine": float((gen <= t).mean())})
    slope, icept = tail_fit(tail, n_imp)
    pair_far = ATTEMPT_FAR / (PRINTS * FRAMES)
    res["tail_fit"] = {"band": "1e-6..1e-3", "log10_far_slope": slope, "log10_far_intercept": icept}
    res["union_bound"] = {"pair_far_needed": pair_far,
                          "threshold_extrapolated": (np.log10(pair_far) - icept) / slope,
                          "threshold_empirical": threshold_at(tail, n_imp, pair_far)}
    res["top_impostors"] = [{"score": float(s), "a": okpaths[i], "b": okpaths[j]}
                            for s, (i, j) in zip(tail[:20], tail_ij[:20])]

    index = {(n, int(p.rsplit("_", 1)[1][:4])): k for k, (n, p) in enumerate(zip(names, paths))}
    ps, same = lfw_pairs(pairs_file, index, emb, ok)
    res["lfw_pairs"] = {"usable": int(len(ps)),
                        "best_accuracy": float(max(((ps > t) == same).mean() for t in ps))}

    n_users, probe, attempt, gprobe = simulate(emb, label, person, ok)
    res["simulation"] = {"users": n_users, "impostor_probes": int(len(probe)),
                         "impostor_attempts": int(len(attempt)), "genuine_probes": int(len(gprobe))}
    res["grid"] = [{
        "threshold": t,
        "pair_far": far(t)[1], "pair_above": far(t)[0],
        "pair_far_fit": float(10 ** (icept + slope * t)),
        "probe_far": float((probe > t).mean()), "probe_above": int((probe > t).sum()),
        "attempt_far": float((attempt > t).mean()), "attempt_above": int((attempt > t).sum()),
        "frr_pair": float((gen <= t).mean()),
        "frr_lfw_pairs": float((ps[same] <= t).mean()),
        "frr_15_prints": float((gprobe <= t).mean()),
    } for t in GRID]
    res["genuine_quantiles"] = {str(q): float(np.quantile(gen, q)) for q in [0.001, 0.01, 0.05, 0.5]}
    return res


def main():
    prefixes, pairs_file, same_file, out = sys.argv[1:5]
    variants = sys.argv[5:] or ["rgb", "gray"]
    same_person = [tuple(l.split("\t")[:2]) for l in open(same_file)
                   if l.strip() and not l.startswith("#")]
    results = []
    for v in variants:
        for sp in ([], same_person):
            results.append(analyse(prefixes, pairs_file, sp, v))
    json.dump(results, open(out, "w"), indent=1)
    for r in results:
        print(f"== {r['variant']} ({r['same_person_pairs_excluded']} same-person label pairs excluded): "
              f"{r['images']} images, {r['no_face']} no face, {r['impostor_pairs']} impostor / "
              f"{r['genuine_pairs']} genuine pairs, LFW-pairs accuracy {r['lfw_pairs']['best_accuracy']:.4f}, "
              f"simulation {r['simulation']}")
        for t in r["thresholds"]:
            print(f"  FAR {t['far']:.0e}: t={t['threshold']:.4f} above={t['impostors_above']} "
                  f"FRR={t['frr_all_genuine']:.4f}")
        print("  union bound:", r["union_bound"])
        print("  t      pairFAR(n)        fit       probeFAR(n)      attemptFAR(n)    FRRpair FRRlfw FRR15")
        for g in r["grid"]:
            print(f"  {g['threshold']:.3f} {g['pair_far']:.2e}({g['pair_above']:5d}) {g['pair_far_fit']:.1e} "
                  f"{g['probe_far']:.2e}({g['probe_above']:4d}) {g['attempt_far']:.2e}({g['attempt_above']:3d}) "
                  f"{g['frr_pair']:.4f} {g['frr_lfw_pairs']:.4f} {g['frr_15_prints']:.4f}")


if __name__ == "__main__":
    main()
