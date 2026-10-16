#!/usr/bin/env python3
"""Condensed event trace of one run. usage: trace.py restart|dup LOG. Times in ms relative to the kill (restart) or K2 READY (dup)."""
import sys
sys.path.insert(0, __file__.rsplit("/", 1)[0])
from analyze import parse, kv, rel

kind, path = sys.argv[1], sys.argv[2]
ev = parse(path)
if kind == "restart":
    t0 = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("KILL K"))
    ready = [kv(r)["pid"] for t, n, e, r in ev if n == "K" and e == "READY"]
    oldpid, newpid = ready[0], ready[1]
else:
    t0 = next(t for t, n, e, r in ev if n == "K2" and e == "READY")
    oldpid = next(kv(r)["pid"] for t, n, e, r in ev if n == "K" and e == "READY")
    newpid = next(kv(r)["pid"] for t, n, e, r in ev if n == "K2" and e == "READY")
seen, out = set(), []
for t, n, e, r in ev:
    rt = rel(t, t0)
    if e == "RECV":
        d = kv(r)
        who = "oldK" if d["msg"].startswith(oldpid + ":") else "newK" if d["msg"].startswith(newpid + ":") else None
        if who and rt >= 0 and (n, who) not in seen:
            seen.add((n, who))
            out.append((rt, f"{n} RECV first {who} broadcast {r}"))
        continue
    if e == "PROBE":
        d = kv(r)
        end = rel(int(d["start"]) + int(float(d["dur_ms"]) * 1e6), t0)
        who = "oldK" if d.get("reply") == oldpid else "newK" if d.get("reply") == newpid else "-"
        key = (n, d["kind"], d["res"], who)
        if rel(int(d["start"]), t0) >= -250 and key not in seen:
            seen.add(key)
            out.append((end, f"{n} PROBE {d['kind']} first {d['res']} {who if d['res']=='ok' else ''} started {rel(int(d['start']), t0)} dur {d['dur_ms']} {d.get('msg','')}"))
        continue
    if n == "ORCH" and r.startswith(("spawned", "END")):
        continue
    if rt < -1500 and e not in ("READY",):
        continue
    if e in ("STDERR", "NEIGHBOR_UP", "NEIGHBOR_DOWN", "READY", "LOCKED", "LOCKFAIL", "HELD_OPEN", "NOTE", "GOSSIP_ERR", "LAGGED"):
        out.append((rt, f"{n} {e} {r[:170]}"))
for rt, s in sorted(out, key=lambda x: x[0]):
    print(f"{rt:>9} {s}")
