#!/usr/bin/env python3
"""E18 late-attachment table and B2 timeline.

usage:
  python3 -I analyze_e18.py logs/e18 --table <run-name>...   one row per rebound member generation: attach delay (first NU of the member after REBOUND)
  python3 -I analyze_e18.py logs/e18 --b2 <run-name> <M1|M2|M3> [lo hi]   B2 trace lines concerning that member (state, messages, connections), seconds after KILL
Late = first NU of the member 20 s or more after its REBOUND.
"""
import gzip
import re
import sys
from datetime import datetime

IDS = {"8a88e3dd74": "B1", "8139770ea8": "B2", "66be7e332c": "M1", "0b513ad9b4": "M2", "91a28a0b74": "M3"}
SHORT = {v: k for k, v in IDS.items()}


def rd(path):
    try:
        with gzip.open(path, "rt") as f:
            return f.read().splitlines()
    except FileNotFoundError:
        return []


def iso_ms(s):
    return int(datetime.strptime(s[:26] + "+0000", "%Y-%m-%dT%H:%M:%S.%f%z").timestamp() * 1000)


def raw(d, run):
    return [l.split(" ", 3) for l in rd(f"{d}/{run}.raw.log.gz")]


def rows(d, run):
    """One dict per rebound member generation of a run (times in s)."""
    r = raw(d, run)
    kill = next(int(x[0]) for x in r if x[2] == "KILL")
    out = []
    for m in ("M1", "M2", "M3"):
        rebounds = sorted(int(x[0]) for x in r if x[1] == m and x[2] == "REBOUND")
        for g, t0 in enumerate(rebounds, start=1):
            t1 = rebounds[g] if g < len(rebounds) else 10**15
            jd = next((x for x in r if x[1] == m and x[2] in ("JOINED", "JOIN_FAIL") and t0 <= int(x[0]) < t1), None)
            hb = next((int(x[0]) for x in r if x[1] == m and x[2] == "RX" and x[3].startswith("hb") and t0 <= int(x[0]) < t1), None)
            jt = None if jd is None else (int(jd[0]) - t0) / 1000
            out.append(dict(run=run, m=m, g=g, t0=(t0 - kill) / 1000, res=jd[2] if jd else "-", jt=jt, hb=None if hb is None else (hb - t0) / 1000,
                            late_join=jd is None or jt >= 20 or jd[2] == "JOIN_FAIL", late_hb=hb is None or (hb - t0) / 1000 >= 20))
    return out


def table(d, runs):
    """late_join = no JOINED within 20 s of the member's REBOUND (JOIN_FAIL counts); late_hb = first heartbeat received 20 s or more after REBOUND."""
    print("run member gen rebound_s join_result join_s first_hb_s late_join late_hb")
    f = lambda t: "-" if t is None else f"{t:.2f}"
    for run in runs:
        for w in rows(d, run):
            print(w["run"], w["m"], w["g"], f(w["t0"]), w["res"], f(w["jt"]), f(w["hb"]), "LATE_JOIN" if w["late_join"] else "ok", "LATE_HB" if w["late_hb"] else "ok")


def b2(d, run, m, lo=-10.0, hi=200.0):
    r = raw(d, run)
    kill = next(int(x[0]) for x in r if x[2] == "KILL")
    pid = SHORT[m]
    for l in rd(f"{d}/{run}.B2.trace.log.gz"):
        if not l or l[0] != "2":
            continue
        t = (iso_ms(l) - kill) / 1000
        if not lo <= t <= hi:
            continue
        keep = False
        if "E18" in l and (f"peer={pid}" in l or f"from={pid}" in l or "shuffle_timer" in l):
            keep = True
        elif f"peer={pid}" in l and ("connection" in l):
            keep = True
        elif pid in l and ("handle in_event" in l or "handle out_event" in l) and "Received(" not in l and "Gossip(Gossip" not in l:
            keep = True
        elif "add to active view" in l and pid in l:
            keep = True
        if keep:
            l = re.sub(r"PublicKey\((\w{10})\w+\)", lambda mo: IDS.get(mo.group(1), mo.group(1)), l)
            l = re.sub(r"TopicId\(\w+\)", "T", l)
            print(f"{t:8.2f} " + l[28:420])


def joins(d, runs):
    """B2's state when each post-KILL Join arrived, against the member's join result.
    cols: B2 state at Join (act = peer in active view, pend = peer in pending_neighbor_requests), whether B2 sent a Neighbor for that Join (fresh), member result."""
    print("run member t_join_s act pend neighbor_sent member_result join_s")
    for run in runs:
        r = raw(d, run)
        kill = next(int(x[0]) for x in r if x[2] == "KILL")
        lines = [l for l in rd(f"{d}/{run}.B2.trace.log.gz") if l and l[0] == "2"]
        for i, l in enumerate(lines):
            mo = re.search(r"E18 handle Join active=\[(.*?)\] pending=\[(.*?)\] from=(\w+)", l)
            if not mo:
                continue
            t = iso_ms(l)
            if t < kill:
                continue
            who = IDS.get(mo.group(3))
            act = mo.group(3) in mo.group(1)
            pend = mo.group(3) in mo.group(2)
            sent = "-"
            for l2 in lines[i + 1 : i + 40]:
                m2 = re.search(r"E18 send_neighbor .* peer=(\w+) fresh=(\w+)", l2)
                if m2 and m2.group(1) == mo.group(3):
                    sent = "yes" if m2.group(2) == "true" else "NO"
                    break
                if "E18 handle Join" in l2:
                    break
            js = [x for x in r if x[1] == who and x[2] == "JOIN_START" and abs(int(x[0]) - t) < 1500]
            res = "?"
            jt = "-"
            if js:
                t0 = int(js[0][0])
                jd = next((x for x in r if x[1] == who and x[2] in ("JOINED", "JOIN_FAIL") and int(x[0]) >= t0), None)
                if jd:
                    res = jd[2]
                    jt = f"{(int(jd[0]) - t0) / 1000:.2f}"
            print(run, who, f"{(t - kill) / 1000:.2f}", "act" if act else "-", "pend" if pend else "-", sent, res, jt)


if __name__ == "__main__":
    d = sys.argv[1]
    if sys.argv[2] == "--table":
        table(d, sys.argv[3:])
    elif sys.argv[2] == "--joins":
        joins(d, sys.argv[3:])
    elif sys.argv[2] == "--b2":
        b2(d, sys.argv[3], sys.argv[4], *(float(x) for x in sys.argv[5:7]))
