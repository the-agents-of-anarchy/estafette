#!/usr/bin/env python3
"""E21 H3 table: a new member M4 that knows only host C (order 2) after host A has been dead for the whole window.

usage: python3 -I analyze_e21_h3.py <logdir> <run-name-regex>
Times in ms from M4's JOIN_START (the first one).
"""
import gzip
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def main(d, rx):
    import glob
    print("| run | JOINED, ms | first HB from M1 / M2 / M3, ms | first HB from any member, ms | HB from all three, ms | M4 REBIND | M4 JOIN_FAIL | M4 home relay |")
    print("|---|---|---|---|---|---|---|---|")
    for p in sorted(glob.glob(os.path.join(d, "*.raw.log.gz"))):
        name = os.path.basename(p).split(".raw.log")[0]
        if not re.match(rx, name):
            continue
        r = [l.split(" ", 3) for l in gzip.open(p, "rt").read().splitlines()]
        m4 = [x for x in r if x[1] == "M4"]
        js = next(int(x[0]) for x in m4 if x[2] == "JOIN_START")
        jd = next((int(x[0]) - js for x in m4 if x[2] == "JOINED"), None)
        hb = []
        for k in (1, 2, 3):
            hb.append(next((int(x[0]) - js for x in m4 if x[2] == "RX" and x[3].startswith(f"hb from=M{k} ")), None))
        addr = [x[3].strip() for x in m4 if x[2] == "ADDR"]
        print(f"| {name} | {jd} | {' / '.join(str(h) for h in hb)} | {min(h for h in hb if h is not None) if any(h is not None for h in hb) else '-'} | {max(hb) if None not in hb else '-'} | {sum(1 for x in m4 if x[2] == 'REBIND')} | {sum(1 for x in m4 if x[2] == 'JOIN_FAIL')} | {addr[-1] if addr else '-'} |")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
