import gzip, sys
d, run, who, after_kind = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
rows = [l.split(" ", 3) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt").read().splitlines()]
t1 = next(int(r[0]) for r in rows if r[1] == "CTL" and r[2] == after_kind)
for src in ("M1", "M2", "M3"):
    if src == who:
        continue
    ts = [int(r[0]) for r in rows if r[1] == who and r[2] == "RX" and r[3].startswith(f"hb from={src} ") and int(r[0]) >= t1]
    print(who, "from", src, "first hb after restart ms:", (ts[0] - t1) if ts else None, "count in 45 s:", len(ts), "last ms:", (ts[-1] - t1) if ts else None)
