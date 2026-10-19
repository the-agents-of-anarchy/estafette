import collections, sys
tag = sys.argv[1]
runs = int(sys.argv[2])
for r in range(1, runs + 1):
    L = [l.rstrip('\n').split(' ', 3) for l in open(f'/tmp/e8/{tag}-{r}.raw.log')]
    L = [(int(a), b, c, d) for a, b, c, d in L]
    k = [t for t, w, kd, _ in L if w == 'CTL' and kd == 'KILL'][0]
    print('run', r)
    last = {}
    n = collections.Counter()
    for t, w, kd, rest in L:
        if t > k and kd == 'RX':
            p = rest.split()
            key = (w, p[0], p[1])
            last[key] = t - k
            n[key] += 1
    for key in sorted(last):
        print('  ', key, 'last@', last[key], 'n', n[key])
    pr = [(t - k, w, rest) for t, w, kd, rest in L if t > k and kd == 'PROBE' and w in ('M2', 'M3') and ('tgt=M' + ('3' if w == 'M2' else '2')) in rest]
    ok = [x for x in pr if ' ok ' in x[2]]
    print('   M2<->M3 dial ok after kill:', len(ok), 'last ok', ok[-1][0] if ok else None, 'first fail', next((x[0] for x in pr if 'fail' in x[2]), None))
    print('   M1 ADDR after kill', [(t - k, rest) for t, w, kd, rest in L if w == 'M1' and kd == 'ADDR' and t > k][:3])
