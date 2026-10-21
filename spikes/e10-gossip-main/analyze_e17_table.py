#!/usr/bin/env python3
"""E17 per-arm table on top of analyze_e15.py (same definitions: stalled pair = directed HB pair with a gap >= 5 s; heal; flowing at 180 s).

usage: python3 -I analyze_e17_table.py <logs/e17> [--table|--runs]
Arms (file-name regex): X a-1x1w-N (diagnostic build, W1, traced), Xn a-1x1nw-N (diagnostic build, W1, probers off), X0 a-0x1w-N (diagnostic build, no kill),
Xc a-1x1cw-N (diagnostic build, W1 + counter), W a-1c0w-N (stock build, W1), C a-1c1w-N (stock build, W1 + E17_FRESH_LOOKUP), C0 a-0c1w-N (stock build, counter on, no kill).
Relapse = a run in which some member rebinds twice or more after the kill ("2nd rebind"), and/or a pair has a second >= 5 s gap episode ("2nd stall").
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("a15", os.path.join(HERE, "analyze_e15.py"))
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)
a.ARMS = [
    ("X", "diagnostic build, W1 (scn 1)", r"^a-1x1w-\d+$"),
    ("Xn", "diagnostic build, W1, probers off (scn 1)", r"^a-1x1nw-\d+$"),
    ("Xc", "diagnostic build, W1 + E17_FRESH_LOOKUP (C1, scn 1)", r"^a-1x1cw-\d+$"),
    ("Xp", "diagnostic build, W1 + E17_PROBE (C2, scn 1)", r"^a-1x1pw-\d+$"),
    ("Xp0", "diagnostic build, W1 + E17_PROBE (C2), no kill (scn 0)", r"^a-0x1pw-\d+$"),
    ("X0", "diagnostic build, W1, no kill (scn 0)", r"^a-0x1w-\d+$"),
    ("W", "stock build, W1 (scn 1)", r"^a-1c0w-\d+$"),
    ("C", "stock build, W1 + E17_FRESH_LOOKUP (scn 1)", r"^a-1c1w-\d+$"),
    ("C0", "stock build, W1 + E17_FRESH_LOOKUP, no kill (scn 0)", r"^a-0c1w-\d+$"),
]


def table(d):
    print("| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | runs with a 2nd rebind | runs with a 2nd stall episode | rebinds per member | first rebind s min/med/max | all stalled pairs flowing from s min/med/max | rebinds before the kill |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for k, lab, rx in a.ARMS:
        ms = [a.measures(p) for p in a.paths(d, rx)]
        if not ms:
            continue
        rbs = [len(v) for m in ms for v in m["rebind_after"].values()]
        rb2 = sum(1 for m in ms if any(len(v) >= 2 for v in m["rebind_after"].values()))
        sec2 = sum(1 for m in ms if m["gap_episodes"] > m["stalled"])
        print(f"| {k} {lab} | {len(ms)} | {sum(1 for m in ms if m['stalled'])} | {sum(m['stalled'] for m in ms)} | {sum(m['healed_end'] for m in ms)} | {rb2} | {sec2} | {min(rbs)}-{max(rbs)} | {a.rng([m['first_rebind'] for m in ms])} | {a.rng([m['all_flowing_from'] for m in ms])} | {sum(m['rebind_before'] for m in ms)} |")


if __name__ == "__main__":
    {"--table": table, "--runs": a.runs}[sys.argv[1 + 1]](sys.argv[1])
