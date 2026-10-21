#!/usr/bin/env python3
"""E24 analysis. usage: python3 -I analyze_e24.py <logs dir> <run dir name>... [--json]
Per run (kind from meta.json): clocks of B and C are converted to A's clock with the offsets measured at run start.
Stalled pair = directed test-heartbeat pair (rx, tx) with a gap >= 5 s; heal = end of the pair's last gap (E15 definitions).
"""
import glob
import json
import os
import re
import sys

STALL = 5000
MEM = ("M1", "M2", "M3", "M4")


def load(d):
    meta = json.load(open(os.path.join(d, "meta.json")))
    off = {"A": 0, "B": meta["B"]["off_ms"], "C": meta["C"]["off_ms"]}
    recs = []
    for path in glob.glob(os.path.join(d, "**", "*.log"), recursive=True):
        if os.path.basename(path) == "relay.log":
            continue
        m = path.replace(d + os.sep, "")
        machine = "A" if m.startswith("A-") else m[0]
        for line in open(path, errors="replace"):
            p = line.rstrip("\n").split(" ", 4)
            if len(p) >= 4 and p[0] == "EV":
                recs.append((int(p[1]) - off[machine], p[2], p[3], p[4] if len(p) > 4 else ""))
    recs.sort(key=lambda r: r[0])
    return meta, recs


def hb(recs, rx, tx, lo, hi):
    pat = f"hb from={tx} "
    return [t for t, who, kind, rest in recs if who == rx and kind == "RX" and rest.startswith(pat) and lo <= t <= hi]


def gaps(ts, lo, hi):
    pts = [lo] + ts + [hi]
    return [(a, b) for a, b in zip(pts, pts[1:]) if b - a >= STALL]


def kill_stats(meta, recs, window=None, members=MEM):
    t0 = meta["kill_B_ms"][0] - meta["B"]["off_ms"]
    end = max(t for t, *_ in recs) if window is None else t0 + window
    end = min(end, max(t for t, *_ in recs))
    out = {"t0": t0, "window_s": round((end - t0) / 1000)}
    rb = {m: sorted((t - t0) / 1000 for t, who, kind, rest in recs if who == m and kind == "REBIND" and t >= t0) for m in members}
    out["rebinds_before_kill"] = sum(1 for t, who, kind, rest in recs if kind == "REBIND" and who in members and t < t0)
    out["rebind"] = rb
    firsts = [v[0] for v in rb.values() if v]
    out["first_rebind_min"] = min(firsts) if firsts else None
    out["members_rebound"] = sum(1 for v in rb.values() if v)
    out["second_rebind_members"] = sum(1 for v in rb.values() if len(v) > 1)
    out["join_fail"] = sum(1 for t, who, kind, rest in recs if kind == "JOIN_FAIL" and who in members and t >= t0)
    pairs = {}
    heal = []
    stalled = []
    flowing_end = 0
    npairs = 0
    for rx in members:
        for tx in members:
            if rx == tx:
                continue
            npairs += 1
            ts = hb(recs, rx, tx, t0 - 5000, end)
            g = gaps([t for t in ts if t >= t0 - 5000], t0, end)
            # a gap counts as a stall when it starts at or after the kill window start (the first beat before the kill anchors it)
            before = [t for t in ts if t < t0]
            anchor = max(before) if before else t0
            ts2 = [anchor] + [t for t in ts if t >= t0]
            g = [(a, b) for a, b in zip(ts2, ts2[1:] + [end]) if b - a >= STALL]
            if g:
                stalled.append((rx, tx))
                last = g[-1]
                still = last[1] == end and (not ts or end - max(ts) >= STALL)
                pairs[f"{rx}<-{tx}"] = {"episodes": len(g), "longest_s": round(max(b - a for a, b in g) / 1000, 1), "healed_at_s": None if still else round((last[1] - t0) / 1000, 1)}
                if not still:
                    heal.append((last[1] - t0) / 1000)
            if ts and end - max(ts) < STALL:
                flowing_end += 1
    out["pairs_total"] = npairs
    out["stalled_pairs"] = len(stalled)
    out["pairs_detail"] = pairs
    out["flowing_at_end"] = flowing_end
    out["all_flowing_from_s"] = None if (len(heal) < len(stalled)) else (max(heal) if heal else None)
    out["stalled_unhealed"] = len(stalled) - len(heal)
    return out


def risk(meta, recs, t0, end, members=MEM):
    """Test 2: per member, host records (from H2) received between the kill and its first rebind (or the window end), and its outgoing stall."""
    res = {}
    for m in members:
        rbs = [t for t, who, kind, rest in recs if who == m and kind == "REBIND" and t >= t0]
        stop = rbs[0] if rbs else end
        rec = [t for t, who, kind, rest in recs if who == m and kind == "RX" and rest.startswith("infra host=H2") and t0 <= t <= stop]
        # outgoing stall: some peer got no test heartbeat from m for >= 5 s after the kill
        out_stalls = []
        for rx in members:
            if rx == m:
                continue
            ts = hb(recs, rx, m, t0 - 5000, end)
            before = [t for t in ts if t < t0]
            anchor = max(before) if before else t0
            ts2 = [anchor] + [t for t in ts if t >= t0]
            out_stalls += [(a, b) for a, b in zip(ts2, ts2[1:] + [end]) if b - a >= STALL]
        st = min((a for a, b in out_stalls), default=None)
        longest = max((b - a for a, b in out_stalls), default=0)
        res[m] = {
            "first_rebind_s": round((rbs[0] - t0) / 1000, 1) if rbs else None,
            "stalled_out": bool(out_stalls),
            "stall_start_s": None if st is None else round((st - t0) / 1000, 1),
            "records_before_first_rebind": len(rec),
            "records_from_stall_start": len([t for t in rec if st is not None and t >= st]),
            "max_record_gap_s": round(max([b - a for a, b in zip([t0] + rec, rec + [stop])], default=0) / 1000, 1),
            "longest_out_stall_s": round(longest / 1000, 1),
            "stalled_past_60s_without_rebind": bool(out_stalls) and (not rbs or (rbs[0] - t0) > 60000) and longest > 60000,
        }
    return res


def quiet_stats(meta, recs, members=MEM):
    start = meta["t_members_start_A"]
    rb = [(round((t - start) / 1000, 1), who) for t, who, kind, rest in recs if kind == "REBIND" and who in members]
    jf = [(round((t - start) / 1000, 1), who) for t, who, kind, rest in recs if kind == "JOIN_FAIL" and who in members]
    hrb = [(round((t - start) / 1000, 1), who) for t, who, kind, rest in recs if kind == "REBIND" and who.startswith("H")]
    last = max(t for t, *_ in recs)
    return {"span_s": round((last - start) / 1000), "member_rebinds": rb, "join_fail": jf, "host_rebinds": hrb}


def runtime_stats(meta, recs, members=MEM):
    """Test 4: H2 started on C at t_h2 (C clock); time until each member logged HOSTS_LEARNED host=H2; then the kill statistics of H1."""
    t_h2 = meta["t_h2_start_C_ms"] - meta["C"]["off_ms"]
    learned = {}
    for t, who, kind, rest in recs:
        if kind == "HOSTS_LEARNED" and who in members and "host=H2" in rest and who not in learned:
            learned[who] = t - t_h2
    s = kill_stats(meta, recs, members=members)
    s["h2_learned_ms"] = learned
    s["h2_ready_ms"] = next((t - t_h2 for t, who, kind, rest in recs if who == "H2" and kind == "READY"), None)
    s["h2_first_record_ms"] = next((t - t_h2 for t, who, kind, rest in recs if who == "H2" and kind == "INFRA_SENT"), None)
    return s


def invite_stats(meta, recs):
    """Test 5: per fresh member Ni: time from process start to JOINED, to its first received test heartbeat, to heartbeats from all four members, and to its first heartbeat seen by M1."""
    out = {}
    for who in sorted({w for t, w, k, r in recs if w.startswith("N")}):
        mine = [(t, k, r) for t, w, k, r in recs if w == who]
        js = next((t for t, k, r in mine if k == "JOIN_START"), None)
        jd = next((t for t, k, r in mine if k == "JOINED"), None)
        first = {}
        for t, k, r in mine:
            if k == "RX" and r.startswith("hb from="):
                first.setdefault(r.split()[1].split("=")[1], t)
        m1 = next((t for t, w, k, r in recs if w == "M1" and k == "RX" and r.startswith(f"hb from={who} ")), None)
        t_start = meta.get(f"t_start_{who}")
        out[who] = {
            "start_to_join_start_ms": None if js is None or t_start is None else js - t_start,
            "joined_ms": None if js is None or jd is None else jd - js,
            "first_hb_ms": None if js is None or not first else min(first.values()) - js,
            "hb_from_all4_ms": None if js is None or len(first) < 4 else max(first[m] for m in MEM) - js,
            "first_hb_seen_by_M1_ms": None if js is None or m1 is None else m1 - js,
            "rebinds": sum(1 for t, k, r in mine if k == "REBIND"),
            "join_fail": sum(1 for t, k, r in mine if k == "JOIN_FAIL"),
        }
    return out


def main():
    base = sys.argv[1]
    runs = [a for a in sys.argv[2:] if not a.startswith("--")]
    allr = {}
    for r in runs:
        meta, recs = load(os.path.join(base, r))
        kind = meta.get("kind")
        if kind == "kill":
            s = kill_stats(meta, recs)
            s["risk"] = risk(meta, recs, s["t0"], s["t0"] + s["window_s"] * 1000)
            allr[r] = s
        elif kind == "quiet":
            allr[r] = quiet_stats(meta, recs)
        elif kind == "runtime":
            s = runtime_stats(meta, recs)
            s["risk"] = risk(meta, recs, s["t0"], s["t0"] + s["window_s"] * 1000)
            allr[r] = s
        elif kind == "invite":
            allr[r] = invite_stats(meta, recs)
    print(json.dumps(allr, indent=1))


if __name__ == "__main__":
    main()
