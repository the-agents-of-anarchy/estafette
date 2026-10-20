#!/usr/bin/env python3
"""E12 analysis on top of analyze.py (E10): per-run and per-cell measures from raw logs.

usage: python3 -I analyze_e12.py <logs-dir> [--runs|--table]
Cells (file-name regex): 1 = a-1l12np-N, 2 = a-1l12-N, 3 = a-1l1np-N, 4 = a-1l2np-N, 5 = a-7l12-N. Window 88000 ms.
Extra records read: L1_FIRE, L1_DONE, REJOIN (member-side events of the E12 levers).
"""
import glob
import importlib.util
import os
import re
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("analyze", os.path.join(HERE, "analyze.py"))
an = importlib.util.module_from_spec(spec)
spec.loader.exec_module(an)

CELLS = [
    ("1", "L1+L2, probers off, scn 1", r"^a-1l12np-\d+$"),
    ("2", "L1+L2, probers on, scn 1", r"^a-1l12-\d+$"),
    ("3", "L1 alone, probers off, scn 1", r"^a-1l1np-\d+$"),
    ("4", "L2 alone, probers off, scn 1", r"^a-1l2np-\d+$"),
    ("5", "control scn 7, L1+L2, probers on", r"^a-7l12-\d+$"),
]
WINDOW = 88000


def run_measures(path):
    m = an.analyze(path, WINDOW)
    recs = an.read_records(path)
    t0, end = m["t0"], m["t0"] + WINDOW
    l1 = [ts - t0 for ts, who, kind, rest in recs if kind == "L1_FIRE" and ts >= t0 and ts <= end]
    l1_pre = sum(1 for ts, who, kind, rest in recs if kind == "L1_FIRE" and ts < t0)
    m["l1_fire_first"] = min(l1) if l1 else None
    m["l1_fire_total"] = len(l1) + l1_pre
    m["l1_fire_members"] = sorted({who for ts, who, kind, rest in recs if kind == "L1_FIRE"})
    m["l1_to_rehome"] = (m["rehome"] - min(l1)) if (l1 and m["rehome"] is not None) else None
    rj = {who: sorted(ts - t0 for ts, w, kind, rest in recs if kind == "REJOIN" and w == who and ts <= end) for who in ("M1", "M2", "M3")}
    m["rejoin_after"] = {w: [t for t in v if t >= 0] for w, v in rj.items()}
    m["rejoin_before"] = sum(len([t for t in v if t < 0]) for v in rj.values())
    m["stalled_pairs"] = sorted(f"{rx}<-{tx}" for (rx, tx), v in m["pairs"].items() if v["stalled"])
    m["flowing_pairs"] = sorted(f"{rx}<-{tx}" for (rx, tx), v in m["pairs"].items() if v["stalled"] and v["flowing_end"])
    return m


def sec(v):
    return "-" if v is None else f"{v / 1000:.1f}"


def runs_table(logs):
    print("| Run | M1 L1 fired (s) | M1 re-home (s) | re-home minus L1 (s) | M2-M3 gap (s) | pairs stalled (of 6) | pairs flowing at end | L1 fires (all members) | rejoins after kill M1/M2/M3 | first rejoin (s) | rejoins before kill |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for key, label, rx in CELLS:
        for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*"))):
            name = os.path.basename(p).split(".raw.log")[0]
            if not re.match(rx, name):
                continue
            m = run_measures(p)
            ra = m["rejoin_after"]
            first = min((v[0] for v in ra.values() if v), default=None)
            print(
                f"| {name} | {sec(m['l1_fire_first'])} | {sec(m['rehome'])} | {sec(m['l1_to_rehome'])} | {sec(m['m23_gap'])} | {len(m['stalled_pairs'])} | {len(m['flowing_pairs'])} | {m['l1_fire_total']} | {len(ra['M1'])}/{len(ra['M2'])}/{len(ra['M3'])} | {sec(first)} | {m['rejoin_before']} |"
            )


def stat(vals):
    v = sorted(x for x in vals if x is not None)
    if not v:
        return "-"
    return f"{v[0] / 1000:.1f} / {statistics.median(v) / 1000:.1f} / {v[-1] / 1000:.1f} (n={len(v)})"


def table(logs):
    print("| Cell | runs | M2-M3 stalled (gap >= 5 s) | M2-M3 flowing again at end, of stalled | M2-M3 longest gap s min/med/max | any HB pair stalled | stalled pairs flowing at end | M1 re-home s min/med/max | M1 L1 fire s min/med/max | rejoins/run after kill (min-max) | L1 fires before kill |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for key, label, rx in CELLS:
        ms = []
        for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*"))):
            name = os.path.basename(p).split(".raw.log")[0]
            if re.match(rx, name):
                ms.append(run_measures(p))
        n = len(ms)
        st = sum(m["m23_stalled"] for m in ms)
        rec = sum(m["m23_recovered"] for m in ms)
        anyst = sum(1 for m in ms if m["stalled_pairs"])
        sp = sum(len(m["stalled_pairs"]) for m in ms)
        sf = sum(len(m["flowing_pairs"]) for m in ms)
        rj = [sum(len(v) for v in m["rejoin_after"].values()) for m in ms]
        print(
            f"| {key} {label} | {n} | {st}/{n} | {rec}/{st} | {stat([m['m23_gap'] for m in ms])} | {anyst}/{n} | {sf}/{sp} | {stat([m['rehome'] for m in ms])} | {stat([m['l1_fire_first'] for m in ms])} | {min(rj)}-{max(rj)} | {sum(m['l1_fire_total'] for m in ms) if key == '5' else '-'} |"
        )


if __name__ == "__main__":
    {"--runs": runs_table, "--table": table}[sys.argv[2]](sys.argv[1])
