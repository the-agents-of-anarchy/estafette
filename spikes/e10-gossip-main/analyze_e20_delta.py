#!/usr/bin/env python3
"""E20: per kill run, all-flowing-from minus the last first-generation REBOUND (s), and the first-heartbeat delay after REBOUND per first-generation endpoint, for the arms B1 (y1w) and B3 (y3w); exact permutation test (difference of means, two-sided) on the first.

usage: python3 -I analyze_e20_delta.py <logdir>
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
res = {}
for arm, rx in (("B1", r"^a-1y1w-\d+$"), ("B3", r"^a-1y3w-\d+$")):
    deltas, hbs = [], []
    for p in a15.paths(d, rx):
        name = os.path.basename(p).split(".")[0]
        m = a15.measures(p)
        raw = e18.raw(d, name)
        kill = next(int(x[0]) for x in raw if x[2] == "KILL")
        first_rb = {}
        for x in raw:
            if x[2] == "REBOUND" and x[1] not in first_rb:
                first_rb[x[1]] = int(x[0])
        for mem, t0 in first_rb.items():
            hb = next((int(x[0]) - t0 for x in raw if x[1] == mem and x[2] == "RX" and int(x[0]) > t0 and " ".join(str(y) for y in x[3:]).startswith("hb")), None)
            hbs.append(hb / 1000 if hb is not None else None)
        if m["stalled"] and m["all_flowing_from"] is not None and first_rb:
            deltas.append(m["all_flowing_from"] / 1000 - (max(first_rb.values()) - kill) / 1000)
    res[arm] = (deltas, hbs)
    print(arm, "runs with a rebind", len(deltas), "all-flowing-from minus last REBOUND (s):", [round(x, 1) for x in deltas], "median %.1f mean %.1f" % (statistics.median(deltas), statistics.mean(deltas)))
    ok = [h for h in hbs if h is not None]
    print(arm, "first-gen endpoints", len(hbs), "first hb delay s sorted:", sorted(round(h, 1) for h in ok), "no hb", len(hbs) - len(ok), ">=10 s", sum(h >= 10 for h in ok))


def perm(a, b):
    obs = abs(statistics.mean(a) - statistics.mean(b))
    allv = a + b
    tot = hit = 0
    for c in itertools.combinations(range(len(allv)), len(a)):
        s = set(c)
        x = [allv[i] for i in c]
        y = [allv[i] for i in range(len(allv)) if i not in s]
        tot += 1
        hit += abs(statistics.mean(x) - statistics.mean(y)) >= obs - 1e-9
    return hit / tot


print("permutation p (all-flowing-from minus last REBOUND) = %.4f" % perm(res["B1"][0], res["B3"][0]))
