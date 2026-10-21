#!/usr/bin/env python3
"""E24 owner-assisted tests: analysis. usage: python3 -I analyze_e24owner.py <run dir>
Reference instant: sleep = first tick after the largest gap in ticks.txt (wake); wifi = first ip.txt line whose public IPv4 differs from the first one
(also printed: first line with no answer, i.e. the network was down). Pairs involving A's members (M1, M2) are measured against it.
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze_e24 as an  # noqa: E402


def main():
    d = sys.argv[1]
    meta, recs = an.load(d)
    kind = meta["kind"]
    out = {"kind": kind}
    ticks = [int(x) for x in open(os.path.join(d, "ticks.txt")).read().split()]
    gaps = sorted(((b - a, a, b) for a, b in zip(ticks, ticks[1:])), reverse=True)
    if kind == "owner-sleep":
        g, a, b = gaps[0]
        out["sleep_gap_s"] = round(g / 1000, 1)
        ref = b
        out["sleep_start_to_enter_s"] = round((a - meta["events"]["enter_ms"]) / 1000, 1)
    else:
        ips = [l.split() for l in open(os.path.join(d, "ip.txt"))]
        first_ip = next(x[1] for x in ips if x[1] != "-")
        down = next((int(x[0]) for x in ips if x[1] == "-"), None)
        new = next((int(x[0]) for x in ips if x[1] not in ("-", first_ip)), None)
        out["public_ip_changed"] = new is not None
        out["first_ip_octets"] = first_ip.split(".")[0] + ".x"
        out["new_ip_octets"] = None if new is None else next(x[1] for x in ips if x[1] not in ("-", first_ip)).split(".")[0] + ".x"
        out["down_ms_before_new"] = None if down is None or new is None else new - down
        ref = new if new is not None else meta["events"]["enter_ms"]
        out["enter_after_new_s"] = round((meta["events"]["enter_ms"] - ref) / 1000, 1)
    end = max(t for t, *_ in recs)
    out["ref_to_end_s"] = round((end - ref) / 1000)
    per = {}
    for m in ("M1", "M2"):
        per[m] = {
            "rebinds_after_ref_s": [round((t - ref) / 1000, 1) for t, w, k, r in recs if w == m and k == "REBIND" and t >= ref - 3000],
            "rebinds_before_ref": sum(1 for t, w, k, r in recs if w == m and k == "REBIND" and t < ref - 3000),
            "join_fail_after_ref": sum(1 for t, w, k, r in recs if w == m and k == "JOIN_FAIL" and t >= ref - 3000),
            "addr_events_after_ref": [(round((t - ref) / 1000, 1), r) for t, w, k, r in recs if w == m and k == "ADDR" and t >= ref - 3000][:6],
        }
    out["A_members"] = per
    pairs = {}
    for rx in an.MEM:
        for tx in an.MEM:
            if rx == tx or not ({rx, tx} & {"M1", "M2"}):
                continue
            ts = an.hb(recs, rx, tx, ref - 60000, end)
            before = [t for t in ts if t < ref]
            after = [t for t in ts if t >= ref]
            last_before = max(before) if before else None
            first_after = min(after) if after else None
            pairs[f"{rx}<-{tx}"] = {
                "last_before_ref_s": None if last_before is None else round((last_before - ref) / 1000, 1),
                "first_after_ref_s": None if first_after is None else round((first_after - ref) / 1000, 1),
                "gap_s": None if last_before is None or first_after is None else round((first_after - last_before) / 1000, 1),
                "flowing_at_end": bool(ts) and end - max(ts) < an.STALL,
            }
    out["pairs"] = pairs
    peers = [v["first_after_ref_s"] for k, v in pairs.items() if k.split("<-")[0] in ("M3", "M4") and v["first_after_ref_s"] is not None]
    out["peers_first_hb_from_A_members_after_ref_s"] = sorted(peers)
    out["all_pairs_resumed_by_s"] = None if any(v["first_after_ref_s"] is None for v in pairs.values()) else max(v["first_after_ref_s"] for v in pairs.values())
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
