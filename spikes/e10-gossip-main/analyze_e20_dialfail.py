import sys, gzip, re, datetime, glob, os
# usage: dialfail.py logdir run...   per rebound endpoint: REBOUND, first hb, B1-dial-fail time, first Graft/Neighbor sent
d = sys.argv[1]
def tsec(t):  # 'HH:MM:SS.ffffff' -> seconds in day
    h, m, s = t.split(':'); return int(h) * 3600 + int(m) * 60 + float(s)
for run in sys.argv[2:]:
    raw = [l.split(' ', 2) for l in gzip.open(f'{d}/{run}.raw.log.gz', 'rt')]
    kill = next(int(r[0]) for r in raw if r[1] == 'CTL' and r[2].startswith('KILL'))
    day0 = datetime.datetime.fromtimestamp(kill / 1000, datetime.timezone.utc)
    base = day0.hour * 3600 + day0.minute * 60 + day0.second + (kill % 1000) / 1000  # trace-seconds at kill
    print('==', run)
    for m in ('M1', 'M2', 'M3'):
        ev = [(int(r[0]), r[2].strip()) for r in raw if r[1] == m]
        rb = [t for t, x in ev if x.startswith('REBOUND')]
        if not rb:
            continue
        t0 = rb[0]
        hb = next((t for t, x in ev if t > t0 and x.startswith('RX hb')), None)
        tr = f'{d}/{run}.{m}.trace.log.gz'
        fails = []
        for l in gzip.open(tr, 'rt', errors='replace'):
            mm = re.match(r'\d{4}-\d\d-\d\dT([\d:.]+)Z\s+WARN.*dial failed: (.*) peer=(\w+)', l)
            if mm:
                fails.append((round(tsec(mm.group(1)) - base - (t0 - kill) / 1000, 2), mm.group(2), mm.group(3)))
        print(m, 'REBOUND +%.1f' % ((t0 - kill) / 1000), 'first hb +%.1f' % ((hb - t0) / 1000) if hb else 'no hb',
              'dial fails after rebound(+s from REBOUND):', [f for f in fails if f[0] > -0.5][:4])
