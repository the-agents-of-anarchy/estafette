import gzip, re, sys
d = sys.argv[1]
run = sys.argv[2]
for m in ("M1", "M2", "M3"):
    txt = gzip.open(f"{d}/{run}.{m}.trace.log.gz", "rt").read().splitlines()
    i0 = next((i for i, l in enumerate(txt) if "starting direct addr update (RelayMapChange)" in l), None)
    print(m, "RelayMapChange line:", i0)
    if i0 is None:
        continue
    for l in txt[i0:i0 + 40]:
        if "probe finished" in l:
            print("  ", re.sub(r"^.*relay=", "relay=", l)[:200])
        if "net_report generated" in l:
            mo = re.search(r"preferred_relay: (\S+ \S+)", l)
            print("   preferred:", mo.group(1) if mo else "?")
            mo = re.search(r"relay_latency: (.*?)\}?, (captive|\w+:)", l)
        if "home is now" in l:
            print("  ", l[l.index("home is now"):])
            break
