#!/usr/bin/env python3
"""E21 per-run H1/H2 table from raw logs.

usage: python3 -I analyze_e21_runs.py <logdir> <run-name-regex>
Per run: C start to every member having RC in its relay map (HOSTS_LEARNED), home relay of each member at the kill (last ADDR before the kill: R1 = host A's relay, R2 = host C's relay), members that changed home after learning C and when, REBIND count before the kill, B2 (host C) neighbor losses within 30 s after the kill, rebinds and JOIN_FAIL per member after the kill, all stalled pairs flowing from (analyze_e15 definition), pairs flowing at 180 s.
"""
import glob
import gzip
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("analyze_e15", os.path.join(HERE, "analyze_e15.py"))
a15 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a15)


def rd(p):
    return [l.split(" ", 3) for l in gzip.open(p, "rt").read().splitlines()]


def main(d, rx):
    print("| run | C start to all learned, ms | learned per member, ms | home at kill M1/M2/M3 | home changes after learning (ms after learn) | REBIND before kill | B2 NeighborDown within 30 s of kill | rebinds after kill M1/M2/M3 | JOIN_FAIL after kill | all stalled pairs flowing from s | pairs flowing at 180 s |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for p in a15.paths(d, rx):
        name = os.path.basename(p).split(".raw.log")[0]
        r = rd(p)
        kill = next(int(x[0]) for x in r if x[2] == "KILL")
        cs = next((int(x[0]) for x in r if x[2] == "C_START"), None)
        ports = [re.search(r"relays=\[(\d+)\]", x[3]).group(1) for x in r if x[1] == "R1" and False]
        r1 = next(x[3].strip() for x in r if x[1] == "R1" and x[2] == "RELAY_UP")
        r2 = next((x[3].strip() for x in r if x[1] == "R2" and x[2] == "RELAY_UP"), None)
        learned, homes, flips = [], [], []
        for m in ("M1", "M2", "M3"):
            hl = [int(x[0]) for x in r if x[1] == m and x[2] == "HOSTS_LEARNED"]
            if cs and hl:
                learned.append(hl[0] - cs)
            adr = [(int(x[0]), x[3].strip()) for x in r if x[1] == m and x[2] == "ADDR" and int(x[0]) < kill]
            last = adr[-1][1] if adr else "?"
            homes.append("R1" if f"[{r1}]" in last else ("R2" if r2 and f"[{r2}]" in last else last))
            if hl:
                ch = [t for t, v in adr if t > hl[0] and r2 and f"[{r2}]" in v]
                flips.append(f"{m}+{ch[0] - hl[0]}" if ch else "")
        before = sum(1 for x in r if x[2] == "REBIND" and int(x[0]) < kill)
        nd = sum(1 for x in r if x[1] == "B2" and x[2] == "ND" and kill <= int(x[0]) <= kill + 30000 and x[3].startswith("M"))
        m = a15.measures(p)
        rb = "/".join(str(len(m["rebind_after"][k])) for k in ("M1", "M2", "M3"))
        jf = sum(1 for x in r if x[2] == "JOIN_FAIL" and int(x[0]) >= kill and x[1] in ("M1", "M2", "M3"))
        fl = "-" if m["all_flowing_from"] is None else f"{m['all_flowing_from'] / 1000:.1f}"
        print(f"| {name} | {max(learned) if len(learned) == 3 else '-'} | {'/'.join(map(str, learned)) or '-'} | {'/'.join(homes)} | {', '.join(f for f in flips if f) or '-'} | {before} | {nd} | {rb} | {jf} | {fl} | {m['flowing_all_end']}/6 |")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
