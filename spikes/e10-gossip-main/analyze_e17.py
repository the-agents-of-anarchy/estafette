#!/usr/bin/env python3
"""E17 timeline of one run from logs/e17 (raw log, member stderr E14DIAG, member tracing).

usage: python3 -I analyze_e17.py logs/e17 <run-name> [--items]   e.g. a-1x1w-1
Times are seconds after the KILL record. Ids and ports are named (R1 = home relay of M1/B1, killed).
"""
import gzip
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone

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
    d, run = sys.argv[1], sys.argv[2]
    items = "--items" in sys.argv
    raw = [l.split(" ", 3) for l in rd(f"{d}/{run}.raw.log.gz")]
    kill = next(int(r[0]) for r in raw if r[2] == "KILL")
    bound = {}
    for r in raw:
        if r[2] == "BOUND":
            m = re.search(r"relays=\[(\d+)\]", r[3])
            bound[r[1]] = m.group(1)
    ports = {}
    for r in raw:
        if r[2] == "BOUND" and "home_want=1" in r[3]:
            ports["R1"] = r[3].split("relays=[")[1].rstrip("]")
        if r[2] == "BOUND" and "home_want=2" in r[3]:
            ports["R2"] = r[3].split("relays=[")[1].rstrip("]")

    def pn(s):
        for k, v in ports.items():
            s = s.replace(f"127.0.0.1:{v}", k).replace(f":{v}/", k)
        for k, v in IDS.items():
            s = s.replace(k, v)
        return s

    def t(ms):
        return f"{(ms - kill) / 1000:8.2f}"

    print(f"RUN {run} ports {ports}")
    for m in ("M1", "M2", "M3"):
        print(f"\n===== {m}")
        ev = []
        hb = []
        for r in raw:
            if r[1] != m:
                continue
            ts = int(r[0])
            if r[2] == "RX" and r[3].startswith("hb"):
                hb.append(ts)
            elif r[2] in ("REBIND", "REBOUND", "JOINED", "JOIN_FAIL", "JOIN_START"):
                ev.append((ts, "raw", f"{r[2]} {pn(r[3])}"))
        hb.sort()
        prev = None
        for h in hb:
            if prev is not None and h - prev >= 3000:
                ev.append((prev, "hb", f"HB SILENCE start (gap {(h - prev) / 1000:.1f} s)"))
                ev.append((h, "hb", "HB RESUME"))
            prev = h
        if hb:
            ev.append((hb[-1], "hb", "LAST HB"))
        item_agg = defaultdict(lambda: [0, None, None])
        for l in rd(f"{d}/{run}.{m}.err.gz"):
            p = l.split(" ", 2)
            if len(p) < 3 or p[0] != "E14DIAG":
                continue
            ts, msg = int(p[1]), p[2]
            if msg.startswith("E17ITEM"):
                mm = re.match(r"E17ITEM inst=(\w+) url=\S+:(\d+)/ remote=(\w+) queue_len=(\d+)", msg)
                k = (mm.group(1), "R1" if mm.group(2) == ports.get("R1") else "R2" if mm.group(2) == ports.get("R2") else mm.group(2), IDS.get(mm.group(3), mm.group(3)))
                a = item_agg[k]
                a[0] += 1
                a[1] = a[1] or ts
                a[2] = ts
                if items and int(mm.group(4)) >= 8:
                    ev.append((ts, "item", pn(msg)))
            elif "BLOCKED" in msg or "UNBLOCKED" in msg or "Full" in msg:
                if "Full" in msg and not items:
                    continue
                ev.append((ts, "diag", pn(msg.split(":")[0] if "BLOCKED" in msg else msg)))
        for l in rd(f"{d}/{run}.{m}.trace.log.gz"):
            if not l or l[0] != "2":
                continue
            ts = iso_ms(l)
            if any(k in l for k in ("start to dial", "dial failed", "dial successful", "Inactive", "Dropped datagram", "add known addrs", "Adding relay connection", "UpdatePeerData", "connection established", "exiting", "Failed to connect to relay server", "retry in")):
                msg = re.sub(r"^\S+\s+(\w+)\s+", r"\1 ", l)
                msg = re.sub(r"gossip\{me=\w+\}:?", "", msg)
                msg = re.sub(r"iroh::socket::transports::relay::actor:|iroh_gossip::net:", "", msg)
                if "Failed to connect to relay server" in msg:
                    msg = re.sub(r"Failed to connect to relay server:.*", "Failed to connect to relay server", msg)
                ev.append((ts, "trace", pn(msg[:170])))
        for ts, src, msg in sorted(ev):
            print(f"{t(ts)} {src:5} {msg}")
        print("-- datagram items found behind a non-empty relay queue (inst, relay, remote): count first..last")
        for k, a in sorted(item_agg.items(), key=lambda kv: kv[1][1]):
            print(f"   {k[0]} {k[1]} {k[2]}: {a[0]}  {t(a[1])}..{t(a[2])}")


main()
