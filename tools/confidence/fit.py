# usage: fit.py <fit.jsonl,...> <check.jsonl,...> <weights.json>: fits the logistic model on the
# first files together, by Newton's method, and reports calibration on each file of both lists.
import json, math, sys
MT = ["exact", "approximate", "street_only", "locality_only"]
def feats(r):
    gap = (r["score"] - r["next"]) if r["next"] is not None else 0.3
    lead = (r["top"] - r["score"])
    return [1.0, r["score"], min(gap, 0.5), min(lead, 0.5), 1.0 if r["rank"] == 0 else 0.0,
            1.0 if r["route"] == "autocomplete" else 0.0] + [1.0 if r["match_type"] == m else 0.0 for m in MT[1:]]
NAMES = ["bias", "score", "gap_next", "behind_top", "first", "autocomplete"] + MT[1:]
def load(p): return [json.loads(l) for l in open(p)]
fits = {p: load(p) for p in sys.argv[1].split(",")}
checks = {p: load(p) for p in sys.argv[2].split(",")}
# Each fit file weighs the same, so a country with more queries does not set the model alone.
X, Y, W = [], [], []
for rows in fits.values():
    for r in rows:
        X.append(feats(r)); Y.append(1.0 if r["ok"] else 0.0); W.append(1.0 / len(rows))
sig = lambda z: 1 / (1 + math.exp(-max(-30, min(30, z))))
d = len(X[0]); w = [0.0] * d
def solve(a, b):
    n = len(b); m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for c in range(n):
        p = max(range(c, n), key=lambda r: abs(m[r][c])); m[c], m[p] = m[p], m[c]
        for r in range(n):
            if r != c and m[c][c]:
                f = m[r][c] / m[c][c]; m[r] = [x - f * y for x, y in zip(m[r], m[c])]
    return [m[i][n] / m[i][i] if m[i][i] else 0.0 for i in range(n)]
for it in range(25):
    g = [0.0] * d; h = [[1e-6 if i == j else 0.0 for j in range(d)] for i in range(d)]
    for x, y, wt in zip(X, Y, W):
        p = sig(sum(a * b for a, b in zip(w, x))); s = wt * p * (1 - p)
        for i in range(d):
            g[i] += wt * (p - y) * x[i]
            for j in range(d): h[i][j] += s * x[i] * x[j]
    step = solve(h, g); w = [a - b for a, b in zip(w, step)]
    if max(abs(s) for s in step) < 1e-6: break
print("weights", {n: round(v, 3) for n, v in zip(NAMES, w)})
def report(rows, pred, name):
    bins = [[0, 0, 0] for _ in range(10)]
    brier = 0
    for r, p in zip(rows, pred):
        y = 1.0 if r["ok"] else 0.0
        b = min(int(p * 10), 9); bins[b][0] += 1; bins[b][1] += p; bins[b][2] += y
        brier += (p - y) ** 2
    ece = sum(abs(b[1] - b[2]) for b in bins) / len(rows)
    print(f"{name}: brier {brier/len(rows):.4f} ece {ece:.4f}")
for kind, sets in [("fit", fits), ("check", checks)]:
    for p, rows in sets.items():
        tag = f"{kind} {p.rsplit('/', 1)[-1]}"
        report(rows, [r["confidence"] for r in rows], tag + " current")
        report(rows, [sig(sum(a * b for a, b in zip(w, feats(r)))) for r in rows], tag + " model  ")
json.dump(dict(zip(NAMES, w)), open(sys.argv[3], "w"))
