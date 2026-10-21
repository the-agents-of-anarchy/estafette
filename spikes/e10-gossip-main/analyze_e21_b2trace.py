import gzip, sys, re
d, run, lo, hi = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
raw = [l.split(" ", 3) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt").read().splitlines()]
kill = next(int(x[0]) for x in raw if x[2] == "KILL")
from datetime import datetime
def iso(s):
    return int(datetime.strptime(s[:26] + "+0000", "%Y-%m-%dT%H:%M:%S.%f%z").timestamp() * 1000)
n = 0
for l in gzip.open(f"{d}/{run}.B2.trace.log.gz", "rt"):
    if len(l) < 27 or not l[0].isdigit():
        continue
    t = (iso(l) - kill) / 1000
    if lo <= t <= hi:
        n += 1
        if n <= int(sys.argv[5]):
            print(f"{t:8.2f}", l[27:].strip()[:230])
print("lines in window:", n)
