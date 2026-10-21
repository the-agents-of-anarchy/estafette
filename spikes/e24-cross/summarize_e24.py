#!/usr/bin/env python3
"""E24 arm summary. usage: python3 -I summarize_e24.py <logs dir> <arm prefix>...   (an arm = runs named <prefix>-<n>)"""
import glob
import itertools
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze_e24 as an  # noqa: E402


def runs_of(base, prefix):
    return sorted(os.path.basename(p) for p in glob.glob(os.path.join(base, prefix + "-[0-9]*")) if os.path.isdir(p))


def mm(v):
    v = sorted(x for x in v if x is not None)
    return "-" if not v else f"{v[0]:.1f} / {statistics.median(v):.1f} / {v[-1]:.1f}"


def perm_p(a, b):
    """Exact two-sided permutation test on the difference of means."""
    pool = a + b
    n = len(a)
    obs = abs(statistics.mean(a) - statistics.mean(b))
    cnt = tot = 0
    for idx in itertools.combinations(range(len(pool)), n):
        s = set(idx)
        x = [pool[i] for i in s]
        y = [pool[i] for i in range(len(pool)) if i not in s]
        tot += 1
        if abs(statistics.mean(x) - statistics.mean(y)) >= obs - 1e-9:
            cnt += 1
    return cnt / tot


def arm(base, prefix):
    rows = []
    for r in runs_of(base, prefix):
        meta, recs = an.load(os.path.join(base, r))
        s = an.kill_stats(meta, recs)
        s["risk"] = an.risk(meta, recs, s["t0"], s["t0"] + s["window_s"] * 1000)
        s["host_rebinds"] = sum(1 for t, w, k, rest in recs if k == "REBIND" and w.startswith("H") and t >= s["t0"])
        s["run"] = r
        rows.append(s)
    return rows


def show(prefix, rows):
    n = len(rows)
    print(f"## {prefix} n={n}")
    stalled = [s for s in rows if s["stalled_pairs"]]
    print(f"runs with a stall: {len(stalled)}/{n}; stalled pairs per run: {[s['stalled_pairs'] for s in rows]}")
    firsts = [m for s in rows for m in [v[0] for v in s["rebind"].values() if v]]
    print(f"first rebind per member, s (min/med/max): {mm(firsts)}  members rebound {sum(s['members_rebound'] for s in rows)}/{4 * n}")
    print(f"first rebind per run (earliest member): {mm([s['first_rebind_min'] for s in rows])}")
    aff = [s["all_flowing_from_s"] for s in rows]
    print(f"all stalled pairs flowing from, s per run: {aff}; unhealed pairs at end: {[s['stalled_unhealed'] for s in rows]}")
    print(f"all-flowing-from (min/med/max): {mm(aff)}")
    tot = [sum(len(v) for v in s["rebind"].values()) for s in rows]
    print(f"rebinds in window per run (4 members): {tot}; runs with a member rebinding twice or more: {sum(1 for s in rows if s['second_rebind_members'])}/{n}")
    per_member = [len(v) for s in rows for v in s["rebind"].values()]
    print(f"rebinds per member mean {statistics.mean(per_member):.2f} max {max(per_member)}; host rebinds after the kill per run: {[s['host_rebinds'] for s in rows]}")
    print(f"JOIN_FAIL per run: {[s['join_fail'] for s in rows]}; rebinds before the kill: {sum(s['rebinds_before_kill'] for s in rows)}; flowing at end (of 12) per run: {[s['flowing_at_end'] for s in rows]}")
    # test 2
    risk = [(s["run"], m, x) for s in rows for m, x in s["risk"].items()]
    outst = [x for _, _, x in risk if x["stalled_out"]]
    print(f"test 2: members with an outgoing stall {len(outst)}/{4 * n}; of these records received before first rebind: {mm([x['records_before_first_rebind'] for x in outst])}; after stall start: {mm([x['records_from_stall_start'] for x in outst])}; max gap between records s: {mm([x['max_record_gap_s'] for x in outst])}")
    print(f"        stalled past 60 s without rebinding: {[(r, m) for r, m, x in risk if x['stalled_past_60s_without_rebind']]}; members with no rebind in window: {[(r, m) for r, m, x in risk if x['first_rebind_s'] is None and x['stalled_out']]}")
    print(f"        longest outgoing stall per member, s: {mm([x['longest_out_stall_s'] for x in outst])}")


def main():
    base = sys.argv[1]
    res = {}
    for p in sys.argv[2:]:
        res[p] = arm(base, p)
        show(p, res[p])
        print()
    ks = list(res)
    if len(ks) >= 2 and all(res[k] for k in ks[:2]):
        a, b = res[ks[0]], res[ks[1]]
        fa = [s["all_flowing_from_s"] if s["all_flowing_from_s"] is not None else 120.0 for s in a]
        fb = [s["all_flowing_from_s"] if s["all_flowing_from_s"] is not None else 120.0 for s in b]
        print(f"permutation p, all-flowing-from (unhealed = window end): {ks[0]} vs {ks[1]}: {perm_p(fa, fb):.3f}")
        ra = [sum(len(v) for v in s['rebind'].values()) for s in a]
        rb = [sum(len(v) for v in s['rebind'].values()) for s in b]
        print(f"permutation p, rebinds per run: {perm_p([float(x) for x in ra], [float(x) for x in rb]):.3f}")


if __name__ == "__main__":
    main()
