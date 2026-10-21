#!/usr/bin/env python3
"""E18 per-arm table: analyze_e15 definitions (stalled pair, heal, flowing at 180 s) plus late joins, from logs/e18 only.

usage: python3 -I analyze_e18_table.py logs/e18
Arms (file-name regex): A0 a-1x1cw-N (baseline, W1 + E17_FRESH_LOOKUP), A1 a-1x2cw-N (A0 + E18_JOIN_T_MS=5000), A2 a-1x3cw-N (A0 + E18_FIX=1),
A0n a-0x1cw-N and A1n a-0x2cw-N (no kill). Late join = a rebound endpoint whose JOINED came 20 s or more after its REBOUND, or JOIN_FAIL (analyze_e18.rows).
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
    ("A0n", "no kill, no counter", r"^a-0x1cw-\d+$"),
    ("A1n", "no kill, E18_JOIN_T_MS=5000", r"^a-0x2cw-\d+$"),
    ("A0", "kill, no counter (W1 + C1)", r"^a-1x1cw-\d+$"),
    ("A1", "kill, E18_JOIN_T_MS=5000 (member-side)", r"^a-1x2cw-\d+$"),
    ("A2", "kill, E18_FIX=1 (vendored gossip, Join drops pending Neighbor)", r"^a-1x3cw-\d+$"),
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
