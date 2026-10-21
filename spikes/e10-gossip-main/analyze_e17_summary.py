#!/usr/bin/env python3
"""E17 per-generation summary over runs in logs/e17.

usage: python3 -I analyze_e17_summary.py logs/e17 <run-name>...
One row per rebound endpoint (generation g = after REBOUND n=g of a member), times in s after that REBOUND:
  blocked  first `RelayActor BLOCKED` of the generation (E14_DIAG stderr), '-' if none
  r1       E17ITEM lines toward the dead relay R1 (datagrams that found a non-empty R1 queue; the first datagram of a fill is not logged), by remote
  m1conn   seconds, within the first 60 s of the generation, in which a gossip connection to M1 was open (trace: connection established / closed, peer=M1); '-' for M1 itself
  end      s until the member's next REBIND (or 'run end')
"""
import gzip
import re
import sys
from datetime import datetime

IDS = {"8a88e3dd74": "B1", "8139770ea8": "B2", "66be7e332c": "M1", "0b513ad9b4": "M2", "91a28a0b74": "M3"}


def rd(path):
    try:
        with gzip.open(path, "rt") as f:
            return f.read().splitlines()
    except FileNotFoundError:
        return []


def iso_ms(s):
    return int(datetime.strptime(s[:26] + "+0000", "%Y-%m-%dT%H:%M:%S.%f%z").timestamp() * 1000)


def main():
    d = sys.argv[1]
    print("run member gen rebound_s end_s blocked_s r1_items(by remote) m1conn_s(first60) heal_first_hb_s")
    for run in sys.argv[2:]:
        raw = [l.split(" ", 3) for l in rd(f"{d}/{run}.raw.log.gz")]
        kill = next(int(r[0]) for r in raw if r[2] == "KILL")
        end_ms = max(int(r[0]) for r in raw)
        r1 = None
        for r in raw:
            if r[2] == "BOUND" and "home_want=1" in r[3]:
                r1 = r[3].split("relays=[")[1].rstrip("]")
        for m in ("M1", "M2", "M3"):
            rebinds = sorted(int(r[0]) for r in raw if r[1] == m and r[2] == "REBIND")
            rebounds = sorted(int(r[0]) for r in raw if r[1] == m and r[2] == "REBOUND")
            hbs = sorted(int(r[0]) for r in raw if r[1] == m and r[2] == "RX" and r[3].startswith("hb"))
            diag = []
            for l in rd(f"{d}/{run}.{m}.err.gz"):
                p = l.split(" ", 2)
                if len(p) == 3 and p[0] == "E14DIAG":
                    diag.append((int(p[1]), p[2]))
            conns = []  # (ts, 'up'|'down', peer)
            for l in rd(f"{d}/{run}.{m}.trace.log.gz"):
                if not l or l[0] != "2":
                    continue
                mm = re.search(r"conn\{peer=(\w+)\}.*(connection established|connection closed)", l)
                if mm and IDS.get(mm.group(1)) == "M1":
                    conns.append((iso_ms(l), "up" if "established" in mm.group(2) else "down"))
            for g, t0 in enumerate(rebounds, start=1):
                t1 = rebinds[g] if g < len(rebinds) else end_ms
                blocked = next((ts for ts, msg in diag if t0 <= ts < t1 and "BLOCKED" in msg and "UNBLOCKED" not in msg), None)
                cnt = {}
                for ts, msg in diag:
                    if t0 <= ts < t1 and msg.startswith("E17ITEM") and f":{r1}/" in msg:
                        rem = IDS.get(re.search(r"remote=(\w+)", msg).group(1), "?")
                        cnt[rem] = cnt.get(rem, 0) + 1
                if m == "M1":
                    open_s = "-"
                else:
                    lim = min(t1, t0 + 60000)
                    total, up = 0, None
                    for ts, kind in sorted(c for c in conns if t0 <= c[0] < t1):
                        if kind == "up":
                            if up is None:
                                up = max(ts, t0)
                        elif up is not None:
                            total += max(0, min(ts, lim) - up) if up < lim else 0
                            up = None
                    if up is not None and up < lim:
                        total += lim - up
                    open_s = f"{total / 1000:.0f}"
                first_hb = next((h for h in hbs if h >= t0), None)
                print(
                    f"{run} {m} {g} {(t0 - kill) / 1000:.1f} {'run end' if g >= len(rebinds) else f'{(t1 - kill) / 1000:.1f}'} "
                    f"{'-' if blocked is None else f'{(blocked - t0) / 1000:.1f}'} {cnt} {open_s} "
                    f"{'-' if first_hb is None else f'{(first_hb - t0) / 1000:.1f}'}"
                )


main()
