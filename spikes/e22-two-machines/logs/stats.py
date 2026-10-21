import re, sys, statistics as st
for f in sys.argv[1:]:
    rows = []
    for l in open(f):
        if ' TRY ' in l and ' ok ' in l:
            d = dict(re.findall(r'(\w+)=(\S+)', l))
            rows.append(d)
    def col(k):
        v = [int(r[k]) for r in rows if r.get(k, 'none') != 'none']
        return v
    def s(k):
        v = col(k)
        return f"{min(v)}/{int(st.median(v))}/{max(v)} (n={len(v)})" if v else "-"
    direct = sum(1 for r in rows if 'ip(' in r['final'] and '*' in r['final'].split('+')[-1])
    print(f, 'ok', len(rows), 'final_direct', direct, '| online', s('online_ms'), '| connect', s('connect_ms'), '| echo', s('echo_ms'), '| upgrade', s('upgrade_ms'), '| steady', s('steady_echo_ms'))
