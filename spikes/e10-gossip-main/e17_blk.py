"""usage: blk.py <logsdir> <run> <member>... -- BLOCKED/UNBLOCKED lines of members' stderr, seconds after KILL, with REBIND/REBOUND times."""
import gzip
import sys

d, run = sys.argv[1:3]
rows = [l.split(" ", 3) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt").read().splitlines()]
kill = next(int(r[0]) for r in rows if r[2] == "KILL")
for m in sys.argv[3:]:
    out = []
    for l in gzip.open(f"{d}/{run}.{m}.err.gz", "rt").read().splitlines():
        p = l.split(" ", 3)
        if len(p) >= 4 and p[2] in ("RelayActor",) and ("BLOCKED" in p[3]):
            kind = "UNBLOCKED" if p[3].startswith("UNBLOCKED") else "BLOCKED"
            out.append((int(p[1]), f"{kind} {p[3].split(':')[0].split()[-1]}"))
    for r in rows:
        if r[1] == m and r[2] in ("REBIND", "REBOUND", "KILL"):
            out.append((int(r[0]), r[2] + " " + r[3].split()[0]))
    print(m, "; ".join(f"{(t - kill) / 1000:.1f} {s}" for t, s in sorted(out)))
