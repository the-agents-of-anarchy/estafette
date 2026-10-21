#!/usr/bin/env python3
"""E21 H2 detail: after the first rebind, did each member join through host C and re-home on C's relay?

usage: python3 -I analyze_e21_h2.py <logdir> <run-name-regex>
Per run and member (first REBOUND after the kill): ms from REBOUND to JOINED, ms from REBOUND to host C (B2) reporting NeighborUp for that member, ms from REBOUND to the member's ADDR listing only C's relay, ms from REBOUND to the first heartbeat received from any member.
"""
import glob
import gzip
import os
import re
import sys


def main(d, rx):
    print("| run | member | kill to REBIND, s | REBOUND to JOINED, ms | REBOUND to B2 NeighborUp(member), ms | REBOUND to ADDR only R2, ms | REBOUND to first HB, ms | PROBE_RELAYS down |")
    print("|---|---|---|---|---|---|---|---|")
    for p in sorted(glob.glob(os.path.join(d, "*.raw.log.gz"))):
        name = os.path.basename(p).split(".raw.log")[0]
        if not re.match(rx, name):
            continue
        r = [l.split(" ", 3) for l in gzip.open(p, "rt").read().splitlines()]
        kill = next(int(x[0]) for x in r if x[2] == "KILL")
        r2 = next(x[3].strip() for x in r if x[1] == "R2" and x[2] == "RELAY_UP")
        for m in ("M1", "M2", "M3"):
            rb = next((int(x[0]) for x in r if x[1] == m and x[2] == "REBOUND" and int(x[0]) > kill), None)
            rbi = next((int(x[0]) for x in r if x[1] == m and x[2] == "REBIND" and int(x[0]) > kill), None)
            if rb is None:
                print(f"| {name} | {m} | - | - | - | - | - | - |")
                continue
            f = lambda t: "-" if t is None else str(t - rb)
            jd = next((int(x[0]) for x in r if x[1] == m and x[2] == "JOINED" and int(x[0]) >= rb), None)
            nu = next((int(x[0]) for x in r if x[1] == "B2" and x[2] == "NU" and x[3].strip() == m and int(x[0]) >= rb), None)
            ad = next((int(x[0]) for x in r if x[1] == m and x[2] == "ADDR" and x[3].strip() == f"relays=[{r2}]" and int(x[0]) >= rb), None)
            hb = next((int(x[0]) for x in r if x[1] == m and x[2] == "RX" and x[3].startswith("hb") and int(x[0]) >= rb), None)
            pr = next((x[3].strip() for x in r if x[1] == m and x[2] == "PROBE_RELAYS" and int(x[0]) > kill), "-")
            print(f"| {name} | {m} | {(rbi - kill) / 1000:.1f} | {f(jd)} | {f(nu)} | {f(ad)} | {f(hb)} | {pr} |")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
