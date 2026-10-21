import sys, gzip, re
# usage: merge.py logdir run t0 t1 [regex]   merged hyparview timeline of B2,M1,M2,M3 traces in [t0,t1] (HH:MM:SS.s)
d, run, t0, t1 = sys.argv[1:5]
inc = re.compile(sys.argv[5] if len(sys.argv) > 5 else r'E18 (handle (Join|ForwardJoin)|send_neighbor)|add to active')
names = {'66be7e332c': 'M1', '0b513ad9b4': 'M2', '91a28a0b74': 'M3', '8139770ea8': 'B2', '8a88e3dd74': 'B1'}
out = []
for n in ('B2', 'M1', 'M2', 'M3'):
    for l in gzip.open(f'{d}/{run}.{n}.trace.log.gz', 'rt', errors='replace'):
        m = re.match(r'\d{4}-\d\d-\d\dT([\d:.]+)Z\s+(\w+)\s+(.*)', l)
        if not m or m.group(1) < t0 or m.group(1) > t1:
            continue
        s = re.sub(r'gossip\{me=[0-9a-f]+\}:?', '', m.group(3))
        if not inc.search(s):
            continue
        s = re.sub(r'iroh_gossip::proto::hyparview: ', '', s)
        s = re.sub(r'"?([0-9a-f]{10})"?', lambda x: names.get(x.group(1), x.group(0)), s)
        s = re.sub(r'PublicKey\(([0-9a-f]{10})[0-9a-f]*\)', lambda x: names.get(x.group(1), x.group(1)), s)
        out.append((m.group(1), n, s[:170]))
for t, n, s in sorted(out):
    print(t, n, s)
