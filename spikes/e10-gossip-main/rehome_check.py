"""E10 check of the re-home timer reading: re-home time measured from M1's bind, per kill run (and the control flip).

usage: python3 -I rehome_check.py <logs-dir>
Prediction from iroh-1.3.0 reading: M1's home flips at its 2nd periodic net report, i.e. 2 x U(20, 26) s = 40..52 s after its first report (~ bind).
"""
import glob
import gzip
import os
import re
import statistics
import sys

logs = sys.argv[1]
rows = []
for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log.gz"))):
    name = os.path.basename(p).split(".raw.log")[0]
    recs = []
    with gzip.open(p, "rt") as f:
        for line in f:
            parts = line.rstrip("\n").split(" ", 3)
            if len(parts) >= 3:
                recs.append((int(parts[0]), parts[1], parts[2], parts[3] if len(parts) > 3 else ""))
    kill = next((r[0] for r in recs if r[2] == "KILL"), None)
    bind = next((r[0] for r in recs if r[1] == "M1" and r[2] == "BOUND"), None)
    r2 = next((re.search(r"relays=\[(\d+)\]", r[3]).group(1) for r in recs if r[1] == "M2" and r[2] == "BOUND"), None)
    if kill is None or bind is None or r2 is None:
        continue
    rehome = next((r[0] for r in recs if r[1] == "M1" and r[2] == "ADDR" and r[0] >= kill and r[3] == f"relays=[{r2}]"), None)
    if rehome is None:
        continue
    rows.append((name, (rehome - kill) / 1000, (rehome - bind) / 1000, (kill - bind) / 1000))

print("run  rehome_after_kill_s  rehome_after_M1_bind_s  kill_after_bind_s")
for name, a, b, c in rows:
    print(f"{name:12s} {a:7.2f} {b:7.2f} {c:6.2f}")
kill_runs = [r for r in rows if not r[0].startswith(("a-7", "b-7"))]
for label, sel in (("kill runs", kill_runs), ("control flips", [r for r in rows if r[0].startswith(("a-7", "b-7"))])):
    if not sel:
        continue
    a = sorted(r[1] for r in sel)
    b = sorted(r[2] for r in sel)
    print(f"{label}: n={len(sel)} after kill min {a[0]:.1f} median {statistics.median(a):.1f} max {a[-1]:.1f}; after M1 bind min {b[0]:.1f} median {statistics.median(b):.1f} max {b[-1]:.1f}")
