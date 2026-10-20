#!/usr/bin/env python3
"""E15 analysis on top of analyze.py (E10): per-run and per-arm measures from raw logs, 180 s window.

usage: python3 -I analyze_e15.py <dir-with-*.raw.log[.gz]> [--runs|--table]
Arms (file-name regex): A0 a-1w-N, A1 a-1l12w-N, A2 a-1w1w-N, A2b a-1w1w10-N, A3 a-1w1l1w-N, A2c (no-kill control) a-0w1w-N,
A4 a-1pw-N (diagnostic transport, no levers), A4b a-1pl12w-N (diagnostic transport, L1+L2).
Stalled pair = directed HB pair (6 per run) with a gap >= 5 s. Heal time = first HB at the receiver more than 5 s after the last HB before the gap, seconds after the kill.
Healed at end = an HB within the last 5 s of the window. Rebinds: REBIND events (member side, W1); after = at or after the kill.
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

WINDOW = 180000
ARMS = [
    ("A0", "control, no levers (scn 1)", r"^a-1w-\d+$"),
    ("A1", "L1+L2 (scn 1)", r"^a-1l12w-\d+$"),
    ("A2", "W1 T=3000 (scn 1)", r"^a-1w1w-\d+$"),
    ("A2b", "W1 T=10000 (scn 1)", r"^a-1w1w10-\d+$"),
    ("A3", "W1 T=3000 + L1 (scn 1)", r"^a-1w1l1w-\d+$"),
    ("A2c", "W1 T=3000, no kill (scn 0)", r"^a-0w1w-\d+$"),
    ("A4", "diagnostic transport, no levers (scn 1)", r"^a-1pw-\d+$"),
    ("A4b", "diagnostic transport, L1+L2 (scn 1)", r"^a-1pl12w-\d+$"),
    ("A4c", "diagnostic build, E14_FIX unset = stock behavior (scn 1)", r"^a-1pc-\d+$"),
]
MEM = ("M1", "M2", "M3")


def last_gap_end(ts, end, t0):
    """ms after the kill at which the last >= 5 s gap of this pair ended; None if the pair is still silent at the window end or has no such gap."""
    if not ts or end - max(ts) >= an.STALL_MS:
        return None
    ends = [b for a, b in zip(ts, ts[1:]) if b - a >= an.STALL_MS]
    return (max(ends) - t0) if ends else None


def episodes(ts, end):
    """Number of >= 5 s gaps of one pair (including a trailing one)."""
    pts = ts + [end]
    return sum(1 for a, b in zip(pts, pts[1:]) if b - a >= an.STALL_MS)


def measures(path):
    recs = an.read_records(path)
    t0 = next(r[0] for r in recs if r[2] == "KILL")
    end = t0 + WINDOW
    m = {"name": os.path.basename(path).split(".raw.log")[0], "t0": t0}
    rb = {w: sorted(ts - t0 for ts, who, kind, rest in recs if kind == "REBIND" and who == w) for w in MEM}
    m["rebind_after"] = {w: [t for t in v if t >= 0 and t <= WINDOW] for w, v in rb.items()}
    m["rebind_before"] = sum(1 for v in rb.values() for t in v if t < 0)
    firsts = [v[0] for v in m["rebind_after"].values() if v]
    m["first_rebind"] = min(firsts) if firsts else None
    m["rebound_ms"] = [int(re.search(r"ms=(\d+)", rest).group(1)) for ts, who, kind, rest in recs if kind == "REBOUND"]
    l1 = [ts - t0 for ts, who, kind, rest in recs if kind == "L1_FIRE" and t0 <= ts <= end]
    m["l1_first"] = min(l1) if l1 else None
    m["join_fail"] = sum(1 for r in recs if r[2] == "JOIN_FAIL")
    m["rejoins"] = sum(1 for ts, who, kind, rest in recs if kind == "REJOIN" and t0 <= ts <= end)
    pairs = {}
    m["after_rebind"] = {}
    for rx in MEM:
        for tx in MEM:
            if rx == tx:
                continue
            ts = an.hb_times(recs, rx, tx, t0 - 3000, end)
            gap, resume = an.longest_gap(ts, end)
            stalled = gap is not None and gap >= an.STALL_MS
            flow = bool(ts) and end - max(ts) < an.STALL_MS
            heal = None
            if stalled:
                gs = (resume - gap) if resume is not None else max(ts)
                c = [t for t in ts if t > gs + an.STALL_MS]
                heal = (min(c) - t0) if c else None
                # first HB after both ends rebound (after the kill)
                rbt = [rb[w][0] + t0 for w in (rx, tx) if m["rebind_after"][w]]
                after = [t for t in ts if rbt and t >= max(rbt)]
                m["after_rebind"][(rx, tx)] = (min(after) - t0) if after and len(rbt) == 2 else None
            pairs[(rx, tx)] = dict(gap=gap, stalled=stalled, flow=flow, heal=heal, last_gap_end=last_gap_end(ts, end, t0))
    m["pairs"] = pairs
    ends = [p["last_gap_end"] for p in pairs.values() if p["stalled"]]
    m["all_flowing_from"] = None if (not ends or None in ends) else max(ends)
    m["gap_episodes"] = sum(episodes(an.hb_times(recs, rx, tx, t0 - 3000, end), end) for rx in MEM for tx in MEM if rx != tx)
    m["stalled"] = sum(p["stalled"] for p in pairs.values())
    m["healed_end"] = sum(p["stalled"] and p["flow"] for p in pairs.values())
    m["heal_times"] = [p["heal"] for p in pairs.values() if p["stalled"] and p["flow"] and p["heal"] is not None]
    m["flowing_all_end"] = sum(p["flow"] for p in pairs.values())
    return m


def paths(d, rx):
    out = []
    for p in sorted(glob.glob(os.path.join(d, "*.raw.log*"))):
        if re.match(rx, os.path.basename(p).split(".raw.log")[0]):
            out.append(p)
    return out


def sec(v):
    return "-" if v is None else f"{v / 1000:.1f}"


def rng(v):
    v = sorted(x for x in v if x is not None)
    return "-" if not v else f"{v[0] / 1000:.1f} / {statistics.median(v) / 1000:.1f} / {v[-1] / 1000:.1f} (n={len(v)})"


def runs(d):
    print("| Run | stalled pairs (of 6) | healed at end | heal s (per healed pair) | all stalled pairs flowing from (s) | gap episodes (all pairs) | pairs flowing at end (of 6) | rebinds M1/M2/M3 | first rebind s | rebinds before kill | JOIN_FAIL | rejoins |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for k, lab, rx in ARMS:
        for p in paths(d, rx):
            m = measures(p)
            ra = m["rebind_after"]
            print(f"| {m['name']} | {m['stalled']} | {m['healed_end']} | {', '.join(sec(t) for t in sorted(m['heal_times'])) or '-'} | {sec(m['all_flowing_from'])} | {m['gap_episodes']} | {m['flowing_all_end']} | {len(ra['M1'])}/{len(ra['M2'])}/{len(ra['M3'])} | {sec(m['first_rebind'])} | {m['rebind_before']} | {m['join_fail']} | {m['rejoins']} |")


def table(d):
    print("| Arm | runs | stalled pairs | healed at 180 s | heal s min/med/max (first HB after the pair's stall) | per run: all stalled pairs flowing from s, min/med/max | runs with a second stall episode | pairs flowing at end / (6 x runs) | rebinds per member (min-max) | first rebind s min/med/max | rebound ms min/med/max | false rebinds (before kill) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for k, lab, rx in ARMS:
        ms = [measures(p) for p in paths(d, rx)]
        if not ms:
            continue
        n = len(ms)
        rbs = [len(v) for m in ms for v in m["rebind_after"].values()]
        rbound = [x for m in ms for x in m["rebound_ms"]]
        ht = [t for m in ms for t in m["heal_times"]]
        rbm = "-" if not rbound else f"{min(rbound)} / {statistics.median(rbound):.0f} / {max(rbound)}"
        sec2 = sum(1 for m in ms if m["gap_episodes"] > len([1 for p in m["pairs"].values() if p["stalled"]]))
        print(f"| {k} {lab} | {n} | {sum(m['stalled'] for m in ms)} | {sum(m['healed_end'] for m in ms)} | {rng(ht)} | {rng([m['all_flowing_from'] for m in ms])} | {sec2} | {sum(m['flowing_all_end'] for m in ms)}/{6 * n} | {min(rbs)}-{max(rbs)} | {rng([m['first_rebind'] for m in ms])} | {rbm} | {sum(m['rebind_before'] for m in ms)} |")


if __name__ == "__main__":
    {"--runs": runs, "--table": table}[sys.argv[2]](sys.argv[1])
