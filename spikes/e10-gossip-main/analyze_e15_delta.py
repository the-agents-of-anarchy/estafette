#!/usr/bin/env python3
"""E15: per pair, seconds from the later of the two members' first rebind to the first heartbeat after it. usage: python3 -I analyze_e15_delta.py logs/e15"""
import glob
import importlib.util
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("a15", os.path.join(HERE, "analyze_e15.py"))
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)
an = a.an
for arm in ("a-1w1w-", "a-1w1w10-", "a-1w1l1w-"):
    d = []
    for p in sorted(glob.glob(os.path.join(sys.argv[1], f"{arm}[0-9].raw.log.gz"))):
        recs = an.read_records(p)
        t0 = next(r[0] for r in recs if r[2] == "KILL")
        rb = {w: sorted(ts for ts, who, kind, rest in recs if kind == "REBIND" and who == w) for w in a.MEM}
        for rx in a.MEM:
            for tx in a.MEM:
                if rx == tx or not rb[rx] or not rb[tx]:
                    continue
                lim = max(rb[rx][0], rb[tx][0])
                hb = [t for t in an.hb_times(recs, rx, tx, t0, t0 + a.WINDOW) if t >= lim]
                if hb:
                    d.append((min(hb) - lim) / 1000)
    d.sort()
    print(arm, "pairs", len(d), "first HB after both ends' first rebind, s: min %.1f median %.1f max %.1f" % (d[0], statistics.median(d), d[-1]))
