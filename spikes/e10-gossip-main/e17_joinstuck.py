"""usage: joinstuck.py <logsdir> <run>... -- per member and generation: time of the previous generation's first BLOCKED, JOIN_START, JOINED ms (s after KILL)."""
import gzip
import sys

d = sys.argv[1]
print("run member gen block_prev_s join_start_s since_block_s joined_ms")
for run in sys.argv[2:]:
    rows = [l.split(" ", 3) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt").read().splitlines()]
    kill = next(int(r[0]) for r in rows if r[2] == "KILL")
    for m in ("M1", "M2", "M3"):
        starts = sorted(int(r[0]) for r in rows if r[1] == m and r[2] == "JOIN_START")
        joined = sorted((int(r[0]), r[3]) for r in rows if r[1] == m and r[2] == "JOINED")
        blocks = []
        try:
            for l in gzip.open(f"{d}/{run}.{m}.err.gz", "rt").read().splitlines():
                p = l.split(" ", 2)
                if len(p) == 3 and "BLOCKED" in p[2] and "UNBLOCKED" not in p[2]:
                    blocks.append(int(p[1]))
        except FileNotFoundError:
            pass
        rebounds = sorted(int(r[0]) for r in rows if r[1] == m and r[2] == "REBOUND")
        for g, js in enumerate(starts[1:], start=1):
            prev_t0 = rebounds[g - 2] if g >= 2 else 0
            b = next((x for x in blocks if prev_t0 <= x < js), None)
            jm = next((j for ts, j in joined if ts >= js), "?")
            print(f"{run} {m} {g} {'-' if b is None else f'{(b - kill) / 1000:.1f}'} {(js - kill) / 1000:.1f} {'-' if b is None else f'{(js - b) / 1000:.1f}'} {jm}")
