#!/usr/bin/env python3
"""Dup scenario: which instance answers fresh dials, and when K1 is answering again after K2 is killed."""
import sys, glob
sys.path.insert(0, __file__.rsplit("/", 1)[0])
from analyze import parse, kv, rel

for f in sorted(glob.glob(sys.argv[1])):
    ev = parse(f)
    k2 = next(t for t, n, e, r in ev if n == "K2" and e == "READY")
    kill = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("KILL K2"))
    pid1 = next(kv(r)["pid"] for t, n, e, r in ev if n == "K" and e == "READY")
    out = []
    for p in ("P1", "P2"):
        pr = []
        for t, n, e, r in ev:
            if n == p and e == "PROBE":
                d = kv(r)
                s = int(d["start"])
                pr.append((s, s + int(float(d["dur_ms"]) * 1e6), d))
        after = [x for x in pr if x[0] >= kill]
        bad = [x for x in after if x[2]["kind"] == "fresh" and x[2]["res"] != "ok"]
        dupbad = [x for x in pr if k2 <= x[0] < kill and x[2]["kind"] == "fresh" and x[2]["res"] != "ok"]
        fk = [x for x in after if x[2]["kind"] == "fresh" and x[2]["res"] == "ok" and x[2].get("reply") == pid1]
        heldok = [x for x in after if x[2]["kind"] == "held" and x[2]["res"] == "ok"]
        out.append((p, "first_fresh_ok_by_K1_after_K2_kill_ms", rel(min(x[1] for x in fk), kill) if fk else None,
                    "fresh_nonok_after_kill", len(bad), "fresh_nonok_in_dup", len(dupbad), "held_ok_after_kill", len(heldok)))
    print(f.split("/")[-1], out)
