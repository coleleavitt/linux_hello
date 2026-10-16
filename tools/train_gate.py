#!/usr/bin/env python3
"""Train the lighting gate of docs/liveness-physics.md §8: a logistic regression over the
physics checks' scores and the lighting numbers, from `examples/physics_replay` output on
labelled recordings, written as JSON for `linux_hello --trained-gate`.

Usage:
  physics_replay --max-ambient 50 captures/bench/genuine_01 > genuine_01.jsonl   (one per recording)
  tools/train_gate.py --genuine genuine_*.jsonl --attack print_*.jsonl screen_*.jsonl \
      --out models/trained_gate.json [--genuine-accept 0.99]

Every recording is one cross-validation fold (leave one recording out): the report lists, per
held-out recording, the share of its scored pairs the gate accepts at the threshold that keeps
`--genuine-accept` of the training genuine pairs. The final model is fitted on everything with
that threshold. Pairs missing a feature (the first three pairs of a session have no flicker or
motion) are left out of training; the gate abstains on them at run time. numpy only.
"""

import argparse
import json
import sys

import numpy as np

FEATURES = ["glint", "shape", "flicker", "material", "motion", "parallax", "lit", "ambient", "d"]


def load(path):
    rows = []
    for line in open(path):
        r = json.loads(line)
        if r.get("status") not in ("good", "weak"):
            continue
        values = [r["scores"].get(f, r.get(f)) for f in FEATURES]
        if any(v is None for v in values):
            continue
        rows.append(values)
    return np.array(rows, dtype=np.float64)


def fit(x, y, l2=1.0, steps=3000, lr=0.1):
    """Logistic regression on standardised features by gradient descent with L2; returns
    (mean, scale, weights, bias)."""
    mean, scale = x.mean(0), x.std(0) + 1e-9
    z = (x - mean) / scale
    w, b = np.zeros(z.shape[1]), 0.0
    # Balance the classes: each class weighs 1/2 in the loss.
    weight = np.where(y == 1, 0.5 / max(1, (y == 1).sum()), 0.5 / max(1, (y == 0).sum()))
    for _ in range(steps):
        p = 1 / (1 + np.exp(-(z @ w + b)))
        g = (p - y) * weight
        w -= lr * (z.T @ g + l2 * w / len(y))
        b -= lr * g.sum()
    return mean, scale, w, b


def score(model, x):
    mean, scale, w, b = model
    return 1 / (1 + np.exp(-(((x - mean) / scale) @ w + b)))


def threshold_for(model, genuine, accept):
    """The score below which `1 - accept` of the genuine training pairs fall."""
    return float(np.quantile(score(model, genuine), 1 - accept))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--genuine", nargs="+", required=True)
    ap.add_argument("--attack", nargs="+", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--genuine-accept", type=float, default=0.99)
    args = ap.parse_args()

    sets = [(p, 1, load(p)) for p in args.genuine] + [(p, 0, load(p)) for p in args.attack]
    for p, _, x in sets:
        if len(x) == 0:
            sys.exit(f"{p}: no scored pairs")
    print(f"{'held out':40} label  pairs  accepted")
    for i, (path, label, x) in enumerate(sets):
        train = [(l, v) for j, (_, l, v) in enumerate(sets) if j != i]
        xt = np.vstack([v for _, v in train])
        yt = np.concatenate([np.full(len(v), l) for l, v in train])
        if len(set(yt)) < 2:
            print(f"{path:40} {label}      {len(x):5}  (no fold: the other recordings are one class)")
            continue
        model = fit(xt, yt)
        t = threshold_for(model, np.vstack([v for l, v in train if l == 1]), args.genuine_accept)
        accepted = (score(model, x) >= t).mean()
        print(f"{path:40} {label}      {len(x):5}  {accepted:6.1%}")

    x = np.vstack([v for _, _, v in sets])
    y = np.concatenate([np.full(len(v), l) for _, l, v in sets])
    model = fit(x, y)
    t = threshold_for(model, np.vstack([v for _, l, v in sets if l == 1]), args.genuine_accept)
    mean, scale, w, b = model
    json.dump(
        {
            "features": FEATURES,
            "mean": mean.tolist(),
            "scale": scale.tolist(),
            "weights": w.tolist(),
            "bias": float(b),
            "threshold": t,
            "trained_on": {p: {"label": l, "pairs": len(v)} for p, l, v in sets},
        },
        open(args.out, "w"),
        indent=1,
    )
    print(f"\nwrote {args.out}: threshold {t:.3f}; weights", dict(zip(FEATURES, np.round(w, 3))))


if __name__ == "__main__":
    main()
