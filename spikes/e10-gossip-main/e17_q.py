"""usage: q.py <logsdir> <run> <member> <kind> [substr] [t0] [t1] -- print raw-log records of one member relative to KILL."""
import gzip
import sys

d, run, who, kind = sys.argv[1:5]
sub = sys.argv[5] if len(sys.argv) > 5 else ""
t0 = float(sys.argv[6]) if len(sys.argv) > 6 else -1e9
t1 = float(sys.argv[7]) if len(sys.argv) > 7 else 1e9
rows = [l.split(" ", 3) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt").read().splitlines()]
kill = next(int(r[0]) for r in rows if r[2] == "KILL")
out = []
for r in rows:
    if r[1] == who and r[2] == kind and sub in r[3]:
        t = (int(r[0]) - kill) / 1000
        if t0 <= t <= t1:
            out.append(f"{t:.1f} {r[3]}")
print(len(out))
print(" | ".join(out))
