#!/usr/bin/env python3
"""E16: per run and directed prober pair (me -> tgt), seconds after the kill of the first `PROBE tgt=.. ok` that follows that prober's first `PROBE tgt=.. fail` after the kill (None = no ok in the window).
usage: python3 -I analyze_e16_probe.py <dir> <arm-glob-prefix>...   e.g. logs/e16 a-1f0w- a-1f2w-   (runs numbered 1-9 only; diagnostic runs 101+ live in <dir>/diag)
"""
import glob
import gzip
import os
import sys


def load(p):
    out = []
    for line in gzip.open(p, "rt"):
        a = line.rstrip("\n").split(" ", 3)
        if len(a) >= 3:
            out.append((int(a[0]), a[1], a[2], a[3] if len(a) > 3 else ""))
    return out


d = sys.argv[1]
for arm in sys.argv[2:]:
    print("==", arm)
    for p in sorted(glob.glob(os.path.join(d, f"{arm}[0-9].raw.log.gz"))):
        r = load(p)
        k = next(x[0] for x in r if x[2] == "KILL")
        res = []
        for w in ("M1", "M2", "M3"):
            for t in ("M1", "M2", "M3"):
                if t == w:
                    continue
                pr = [x for x in r if x[1] == w and x[2] == "PROBE" and x[3].startswith(f"tgt={t} ") and x[0] >= k]
                f = [x[0] for x in pr if " fail" in x[3]]
                if not f:
                    res.append(None)
                    continue
                ok = [x[0] for x in pr if " ok" in x[3] and x[0] > f[0]]
                res.append(round((min(ok) - k) / 1000, 1) if ok else None)
        print(os.path.basename(p).split(".raw")[0], "pairs with an ok after the first fail:", sum(1 for x in res if x is not None), "/ 6; first ok s:", res)
