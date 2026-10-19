import sys
tag = sys.argv[1]
for r in (1, 2, 3):
    L = [l.rstrip('\n').split(' ', 3) for l in open(f'/tmp/e8/{tag}-{r}.raw.log')]
    L = [(int(a), b, c, d) for a, b, c, d in L]
    k = [t for t, w, kd, _ in L if w == 'CTL' and kd == 'KILL'][0]
    tr = [t for t, w, kd, _ in L if w == 'CTL' and kd == 'RESTART'][0]
    print('run', r, 'restart at', tr - k)
    for w in ('M1', 'M2', 'M3'):
        for t in ('M1', 'M2', 'M3'):
            if t == w: continue
            ok = [x - tr for x, ww, kd, rest in L if x > tr and ww == w and kd == 'PROBE' and rest.startswith(f'tgt={t} ok')]
            hb = [x - tr for x, ww, kd, rest in L if x > tr and ww == w and kd == 'RX' and rest.startswith(f'hb from={t} ')]
            print(f'  {w}<-{t} dial ok first {ok[:1]} n={len(ok)} | hb first {hb[:1]} n={len(hb)}')
    for x, w, kd, rest in L:
        if kd == 'FINAL': print('  FINAL', rest)
    errs = sorted({rest.split('err=')[-1] for t, w, kd, rest in L if kd == 'STATUS' and 'err=' in rest and 'err=-' not in rest})
    print('  status errors', errs)
    perr = sorted({rest.split('err=')[-1] for t, w, kd, rest in L if kd == 'PROBE' and t > k and 'fail' in rest})
    print('  probe errors', perr)
