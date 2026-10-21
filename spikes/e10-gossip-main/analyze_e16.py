#!/usr/bin/env python3
"""E16 analysis on top of analyze_e15.py: per-arm table for the re-dial fix arms, 180 s window.

usage: python3 -I analyze_e16.py <dir-with-*.raw.log[.gz]> [--table|--runs|--dial]
Arms (file-name regex): F0 a-1f0w-N (fix binary, E16_FIX unset), F1 a-1f1w-N (E16_FIX=1), F2 a-1f2w-N (+L2), F3 a-1f3w-N (+W1 T=3000),
F4 a-0f4w-N (E16_FIX=1, no kill); supplementary F2c a-1f2cw-N (E16_FIX unset + L2). Same definitions as E15: stalled pair = directed HB pair (6 per run) with a gap >= 5 s; heal = first HB at the
receiver more than 5 s after the last HB before the pair's longest gap; flowing at 180 s = an HB within the last 5 s.
--dial: per stalled pair, heal time minus t_dial, t_dial = the earliest, over both directions of the pair, of the first `PROBE tgt=.. ok` that follows
that direction's first `PROBE tgt=.. fail` after the kill (harness definition, main.rs:634-635). Signed; negative = the heartbeat came before the probe.
"""
import importlib.util
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("a15", os.path.join(HERE, "analyze_e15.py"))
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)
an = a.an
a.ARMS = [
    ("F0", "fix binary, E16_FIX unset, no levers (scn 1)", r"^a-1f0w-\d+$"),
    ("F1", "E16_FIX=1, no levers (scn 1)", r"^a-1f1w-\d+$"),
    ("F2", "E16_FIX=1 + L2 (scn 1)", r"^a-1f2w-\d+$"),
    ("F2c", "supplementary: fix binary, E16_FIX unset + L2 (scn 1)", r"^a-1f2cw-\d+$"),
    ("F3","E16_FIX=1 + W1 T=3000 (scn 1)", r"^a-1f3w-\d+$"),
    ("F4", "E16_FIX=1, no kill (scn 0)", r"^a-0f4w-\d+$"),
]


def first_ok(recs, who, tgt, t0):
    fail = min((ts for ts, w, k, rest in recs if w == who and k == "PROBE" and ts >= t0 and rest.startswith(f"tgt={tgt} fail")), default=None)
    if fail is None:
        return None
    return min((ts for ts, w, k, rest in recs if w == who and k == "PROBE" and ts >= fail and rest.startswith(f"tgt={tgt} ok")), default=None)


def dial_deltas(p, m):
    recs = an.read_records(p)
    t0 = m["t0"]
    out = []
    for (rx, tx), pr in m["pairs"].items():
        if not (pr["stalled"] and pr["heal"] is not None):
            continue
        c = [t for t in (first_ok(recs, rx, tx, t0), first_ok(recs, tx, rx, t0)) if t is not None]
        out.append(None if not c else pr["heal"] - (min(c) - t0))
    return out


def stats(v):
    v = sorted(x for x in v if x is not None)
    return "-" if not v else f"{v[0] / 1000:.1f} / {statistics.median(v) / 1000:.1f} / {v[-1] / 1000:.1f} (n={len(v)})"


def table(d):
    print("| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | heal s min/med/max | per run: all stalled pairs flowing from s | dial-to-HB s min/med/max (signed) | runs with 2nd stall episode | rebinds per member | first rebind s | false rebinds |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for k, lab, rx in a.ARMS:
        ps = a.paths(d, rx)
        ms = [a.measures(p) for p in ps]
        if not ms:
            continue
        rbs = [len(v) for m in ms for v in m["rebind_after"].values()]
        dd = [x for p, m in zip(ps, ms) for x in dial_deltas(p, m)]
        sec2 = sum(1 for m in ms if m["gap_episodes"] > m["stalled"])
        print(f"| {k} {lab} | {len(ms)} | {sum(1 for m in ms if m['stalled'])} | {sum(m['stalled'] for m in ms)} | {sum(m['healed_end'] for m in ms)} | {a.rng([t for m in ms for t in m['heal_times']])} | {a.rng([m['all_flowing_from'] for m in ms])} | {stats(dd)} | {sec2} | {min(rbs)}-{max(rbs)} | {a.rng([m['first_rebind'] for m in ms])} | {sum(m['rebind_before'] for m in ms)} |")


def dial(d):
    for k, lab, rx in a.ARMS:
        for p in a.paths(d, rx):
            m = a.measures(p)
            print(m["name"], "dial-to-HB s per stalled+healed pair:", ", ".join("none" if x is None else f"{x / 1000:.1f}" for x in dial_deltas(p, m)) or "-")


if __name__ == "__main__":
    opt = sys.argv[2]
    {"--table": table, "--runs": a.runs, "--dial": dial}[opt](sys.argv[1])
