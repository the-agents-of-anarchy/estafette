#!/usr/bin/env python3
"""E19 per-arm table: analyze_e15 definitions (stalled pair, heal, flowing at 180 s) plus late joins, from a logs dir given as argument.

usage: python3 -I analyze_e19_table.py <logdir>
Arms: see ARMS (tags y0w, y1w, y2w; names a-<scn><tag>-N). Late join = a rebound endpoint whose JOINED came 20 s or more after its REBOUND, or JOIN_FAIL (analyze_e18.rows).
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


a15 = load("analyze_e15")
e18 = load("analyze_e18")
ARMS = [
    ("B1n", "no kill, combined (B1 settings)", r"^a-0y1w-\d+$"),
    ("B0", "kill, W1 alone", r"^a-1y0w-\d+$"),
    ("B1", "kill, W1 + E17_PROBE + E18_JOIN_T_MS=5000 + E18_FIX", r"^a-1y1w-\d+$"),
    ("B2", "kill, W1 + E18_JOIN_T_MS=5000 + E18_FIX (no C2)", r"^a-1y2w-\d+$"),
]
a15.ARMS = ARMS


def main(d):
    print("| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | rebound endpoints (all gens) | late joins (all gens) | first-gen endpoints | first-gen late joins | JOIN_FAIL ends | join wait of late joins s min/med/max | rebinds per member | all stalled pairs flowing from s min/med/max | per run: all flowing from s |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for k, lab, rx in ARMS:
        ps = a15.paths(d, rx)
        ms = [a15.measures(p) for p in ps]
        if not ms:
            continue
        rs = [w for p in ps for w in e18.rows(d, os.path.basename(p).split(".")[0])]
        first = [w for w in rs if w["g"] == 1]
        rbs = [len(v) for m in ms for v in m["rebind_after"].values()]
        flow = [m["all_flowing_from"] for m in ms]
        print(f"| {k} {lab} | {len(ms)} | {sum(1 for m in ms if m['stalled'])} | {sum(m['stalled'] for m in ms)} | {sum(m['healed_end'] for m in ms)} | {len(rs)} | {sum(w['late_join'] for w in rs)} | {len(first)} | {sum(w['late_join'] for w in first)} | {sum(1 for w in rs if w['res'] == 'JOIN_FAIL')} | {a15.rng([w['jt'] * 1000 for w in rs if w['late_join']]) if any(w['late_join'] for w in rs) else '-'} | {min(rbs)}-{max(rbs)} | {a15.rng(flow)} | {', '.join('-' if x is None else f'{x / 1000:.1f}' for x in flow)} |")


if __name__ == "__main__":
    main(sys.argv[1])
