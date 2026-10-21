#!/usr/bin/env python3
"""E20 arm statistics: first rebind, rebinds per member, runs with a second rebind, and exact permutation tests (difference of means, two-sided) on 'all stalled pairs flowing from' per run.

usage: python3 -I analyze_e19_stats.py <logdir>
Per-run values come from analyze_e15.measures (E15 definitions). A stalled run whose pairs were not all flowing at 180 s is right-censored at 180 s; a run that never stalled counts as 0 s in the 'all runs' variant and is left out in the 'stalled runs' variant.
"""
import importlib.util
import itertools
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


a15 = load("analyze_e15")
e18 = load("analyze_e18")
d = sys.argv[1]
ARMS = {"B1": r"^a-1y1w-\d+$", "B3": r"^a-1y3w-\d+$"}
vals = {}
for k, rx in ARMS.items():
    ps = a15.paths(d, rx)
    runs = []
    for p in ps:
        name = os.path.basename(p).split(".")[0]
        m = a15.measures(p)
        raw = e18.raw(d, name)
        kill = next(int(x[0]) for x in raw if x[2] == "KILL")
        rb = [(x[1], (int(x[0]) - kill) / 1000) for x in raw if x[2] == "REBIND"]
        per_member = {mm: [t for n, t in rb if n == mm] for mm in ("M1", "M2", "M3")}
        runs.append(dict(name=name, stalled=m["stalled"], flow=m["all_flowing_from"], first=min((t for _, t in rb), default=None),
                         second=any(len(v) >= 2 for v in per_member.values()), nreb=[len(v) for v in per_member.values()], healed_end=m["healed_end"], npairs=m["stalled"]))
    vals[k] = runs
    st = [r for r in runs if r["stalled"]]
    firsts = [r["first"] for r in st if r["first"] is not None]
    print(k, "runs", len(runs), "stalled", len(st), "first rebind min/med/max", min(firsts), statistics.median(firsts), max(firsts),
          "runs with a 2nd rebind", sum(r["second"] for r in runs), "rebinds per member (all members, all runs) mean %.2f max %d" % (statistics.mean(x for r in runs for x in r["nreb"]), max(x for r in runs for x in r["nreb"])),
          "rebinds total", sum(sum(r["nreb"]) for r in runs))
    print("  per run flow (s, None=stalled-not-all-flowing at 180):", [(r["name"][-4:], r["flow"]) for r in runs])


def vec(k, include_nonstalled):
    out = []
    for r in vals[k]:
        if not r["stalled"]:
            if include_nonstalled:
                out.append(0.0)
            continue
        out.append(180.0 if r["flow"] is None else r["flow"] / 1000)
    return out


def perm(a, b):
    obs = abs(statistics.mean(a) - statistics.mean(b))
    allv = a + b
    n = len(a)
    idx = range(len(allv))
    tot = hit = 0
    for c in itertools.combinations(idx, n):
        s = set(c)
        x = [allv[i] for i in c]
        y = [allv[i] for i in idx if i not in s]
        tot += 1
        if abs(statistics.mean(x) - statistics.mean(y)) >= obs - 1e-9:
            hit += 1
    return hit / tot


for variant, inc in (("stalled runs only, censored at 180", False), ("all runs, never-stalled = 0", True)):
    print(variant)
    v = {k: vec(k, inc) for k in ARMS}
    for k in ARMS:
        print("  ", k, "n=%d" % len(v[k]), "min/med/max %.1f / %.1f / %.1f" % (min(v[k]), statistics.median(v[k]), max(v[k])), "mean %.1f" % statistics.mean(v[k]))
    for a, b in (("B1", "B3"),):
        print("  ", a, "vs", b, "p = %.4f" % perm(v[a], v[b]))

# runs with a second rebind, Fisher exact (two-sided) via hypergeometric
from math import comb


def fisher(a1, n1, a2, n2):
    k = a1 + a2
    n = n1 + n2

    def pr(x):
        return comb(n1, x) * comb(n2, k - x) / comb(n, k)
    p0 = pr(a1)
    return sum(pr(x) for x in range(max(0, k - n2), min(k, n1) + 1) if pr(x) <= p0 + 1e-12)


sec = {k: (sum(r["second"] for r in vals[k]), len(vals[k])) for k in ARMS}
print("runs with a second rebind", sec)
for a, b in (("B1", "B3"),):
    print("  Fisher", a, "vs", b, "p = %.4f" % fisher(sec[a][0], sec[a][1], sec[b][0], sec[b][1]))
