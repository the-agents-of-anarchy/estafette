"""usage: tr.py <logsdir> <run> <member> <t0> <t1> [substr...] -- M's tracing lines (relative to KILL), gossip lines compacted."""
import gzip
import re
import sys
from datetime import datetime

d, run, who, t0, t1 = sys.argv[1:6]
subs = sys.argv[6:]
t0, t1 = float(t0), float(t1)
kill = next(int(l.split(" ", 3)[0]) for l in gzip.open(f"{d}/{run}.raw.log.gz", "rt") if " KILL " in l)
IDS = {"8a88e3dd74": "B1", "8139770ea8": "B2", "66be7e332c": "M1", "0b513ad9b4": "M2", "91a28a0b74": "M3"}
for l in gzip.open(f"{d}/{run}.{who}.trace.log.gz", "rt").read().splitlines():
    if not l or l[0] != "2":
        continue
    ts = int(datetime.strptime(l[:26] + "+0000", "%Y-%m-%dT%H:%M:%S.%f%z").timestamp() * 1000)
    t = (ts - kill) / 1000
    if not (t0 <= t <= t1):
        continue
    if subs and not any(s in l for s in subs):
        continue
    l = re.sub(r"^\S+\s+(\w+)\s+", r"\1 ", l)
    l = re.sub(r"gossip\{me=\w+\}:?", "", l)
    l = re.sub(r"iroh_gossip::(net|proto::hyparview|proto::plumtree):?", "", l)
    for k, v in IDS.items():
        l = l.replace(k, v)
    l = re.sub(r"TopicId\([0-9a-f]+\)", "T", l)
    print(f"{t:8.2f} {l[:230]}")
