import sys, gzip, glob
# usage: online.py logdir glob  -> ms from REBOUND to ONLINE (home relay connected) per rebound endpoint, first generation
d = sys.argv[1]
for fn in sorted(glob.glob(f'{d}/{sys.argv[2]}.raw.log.gz')):
    raw = [l.rstrip('\n').split(' ', 2) for l in gzip.open(fn, 'rt')]
    out = []
    for m in ('M1', 'M2', 'M3'):
        ev = [(int(r[0]), r[2].strip()) for r in raw if r[1] == m]
        rb = [t for t, x in ev if x.startswith('REBOUND')]
        if not rb:
            continue
        on = next((x for t, x in ev if t > rb[0] and x.startswith('ONLINE')), None)
        hb = next((t - rb[0] for t, x in ev if t > rb[0] and x.startswith('RX hb')), None)
        out.append(f'{m} {on.split()[1] if on else None} hb={hb}')
    print(fn.split('/')[-1][:-11], ' | '.join(out))
