import gzip, glob, re, os, sys
d = sys.argv[1]
for p in sorted(glob.glob(f"{d}/a-8z1w-*.raw.log.gz")):
    r = [l.split(" ", 3) for l in gzip.open(p, "rt").read().splitlines()]
    kill = next(int(x[0]) for x in r if x[2] == "KILL")
    cs = next(int(x[0]) for x in r if x[2] == "C_START")
    worst = 0
    for rx in ("M1", "M2", "M3"):
        for tx in ("M1", "M2", "M3"):
            if rx == tx:
                continue
            ts = sorted(int(x[0]) for x in r if x[1] == rx and x[2] == "RX" and x[3].startswith(f"hb from={tx} ") and cs - 3000 <= int(x[0]) <= kill)
            g = max((b - a for a, b in zip(ts, ts[1:])), default=0)
            worst = max(worst, g)
    probes = [x for x in r if x[2] == "PROBE" and " fail " in x[3] and cs <= int(x[0]) <= kill]
    print(os.path.basename(p).split(".")[0], "max member-pair HB gap between C start and kill, ms:", worst, "| failed probes in that span:", len(probes))
