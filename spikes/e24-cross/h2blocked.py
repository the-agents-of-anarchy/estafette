#!/usr/bin/env python3
"""E25: how often host H2 is blocked after H1's death. usage: python3 -I h2blocked.py <logs dir> <run prefix or name>...
Signal: a member's join (through H2, H1 being dead) fails after its rebind: JOIN_FAIL at a member after the kill. H2 is called blocked from the kill to the end of the last
JOIN_FAIL of any member (a lower bound on how long it did not answer), and H2's neighbor losses within 30 s of the kill are listed.
"""
import glob
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze_e24 as an  # noqa: E402


def one(base, r):
    meta, recs = an.load(os.path.join(base, r))
    s = an.kill_stats(meta, recs)
    t0 = s["t0"]
    jf = sorted((t - t0) / 1000 for t, w, k, rest in recs if k == "JOIN_FAIL" and w in an.MEM and t >= t0)
    # end of last failed join = its log time (JOIN_FAIL is logged at the end of the 5 s wait)
    h2nd = sorted(round((t - t0) / 1000, 1) for t, w, k, rest in recs if w == "H2" and k == "ND" and rest.strip() in an.MEM and 0 <= t - t0 <= 30000)
    jd = sorted((t - t0) / 1000 for t, w, k, rest in recs if k == "JOINED" and w in an.MEM and t >= t0)
    return {
        "run": r,
        "join_fail": len(jf),
        "blocked_until_s": round(jf[-1], 1) if jf else None,
        "first_join_fail_s": round(jf[0], 1) if jf else None,
        "h2_lost_members_30s": len(h2nd),
        "rebinds_per_member": [len(v) for v in s["rebind"].values()],
        "all_flowing_from_s": s["all_flowing_from_s"],
        "flowing_at_end": s["flowing_at_end"],
        "window_s": s["window_s"],
    }


def main():
    base = sys.argv[1]
    names = []
    for a in sys.argv[2:]:
        names += sorted(os.path.basename(p) for p in glob.glob(os.path.join(base, a if os.path.isdir(os.path.join(base, a)) else a + "-[0-9]*")) if os.path.isdir(p))
    rows = [one(base, n) for n in names]
    for r in rows:
        print(r)
    bl = [r for r in rows if r["join_fail"]]
    print(f"runs {len(rows)}; with a member join failure after the kill (H2 not answering): {len(bl)}; blocked past 60 s: {[r['run'] for r in bl if r['blocked_until_s'] and r['blocked_until_s'] > 60]}")


if __name__ == "__main__":
    main()
