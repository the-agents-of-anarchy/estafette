import sys, gzip, re, datetime, glob
# usage: who.py logdir glob  -> per first-gen rebound endpoint: first hb delay, nodes (not B2) that sent Neighbor to it within 3 s, ForwardJoin ends at B2 renewing it
d = sys.argv[1]
ids = {'M1': '66be7e332c', 'M2': '0b513ad9b4', 'M3': '91a28a0b74'}
rev = {v: k for k, v in ids.items()}
def tsec(t):
    h, m, s = t.split(':'); return int(h) * 3600 + int(m) * 60 + float(s)
for fn in sorted(glob.glob(f'{d}/{sys.argv[2]}.raw.log.gz')):
    run = fn.split('/')[-1][:-11]
    raw = [l.rstrip('\n').split(' ', 2) for l in gzip.open(fn, 'rt')]
    for m in ('M1', 'M2', 'M3'):
        ev = [(int(r[0]), r[2].strip()) for r in raw if r[1] == m]
        rb = [t for t, x in ev if x.startswith('REBOUND')]
        if not rb:
            continue
        t0 = rb[0]
        dt = datetime.datetime.fromtimestamp(t0 / 1000, datetime.timezone.utc)
        s0 = dt.hour * 3600 + dt.minute * 60 + dt.second + (t0 % 1000) / 1000
        hb = next((t - t0 for t, x in ev if t > t0 and x.startswith('RX hb')), None)
        senders = []
        for n in ('M1', 'M2', 'M3', 'B2'):
            if n == m:
                continue
            for l in gzip.open(f'{d}/{run}.{n}.trace.log.gz', 'rt', errors='replace'):
                mm = re.match(r'\d{4}-\d\d-\d\dT([\d:.]+)Z.*E18 send_neighbor .* peer=' + ids[m] + r' fresh=(\w+)', l)
                if mm and 0 <= tsec(mm.group(1)) - s0 <= 3:
                    senders.append(f'{n}:{mm.group(2)}')
        late = 'LATE' if hb is None or hb > 10000 else ''
        print(f'{run} {m} hb={hb} {late:4s} Neighbor-senders={sorted(set(senders))}')
