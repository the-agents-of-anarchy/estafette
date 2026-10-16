#!/usr/bin/env python3
"""Dup scenario extras: first held-probe error and peers' NeighborDown(K), ms after the K2 kill."""
import sys, glob
sys.path.insert(0, __file__.rsplit("/", 1)[0])
from analyze import parse, kv, rel

for f in sorted(glob.glob(sys.argv[1])):
    ev = parse(f)
    kill = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("KILL K2"))
    k2 = next(t for t, n, e, r in ev if n == "K2" and e == "READY")
    herr = {}
    for t, n, e, r in ev:
        if e == "PROBE" and n in ("P1", "P2"):
            d = kv(r)
            if d["kind"] == "held" and d["res"] == "err" and n not in herr:
                herr[n] = rel(t, kill)
    nd = {n: rel(t, kill) for t, n, e, r in ev if n in ("P1", "P2") and e == "NEIGHBOR_DOWN" and r == "K"}
    k1nd = [rel(t, k2) for t, n, e, r in ev if n == "K" and e == "NEIGHBOR_DOWN" and t >= k2]
    print(f.split("/")[-1], "first_held_err_after_K2_kill", herr, "peer_NeighborDown_K_after_K2_kill", nd, "K1_NeighborDown_after_K2_start", k1nd)
