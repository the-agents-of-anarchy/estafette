#!/usr/bin/env python3
"""E19 per-run event list and second-rebind table.

usage:
  python3 -I analyze_e19.py <logdir> <run-name>...            REBIND / REBOUND / JOINED / JOIN_FAIL per member, s after KILL; hb+x = delay from REBOUND to the first heartbeat received after it (not bounded by the next REBOUND)
  python3 -I analyze_e19.py <logdir> --second <run-name>...   one row per REBIND after the first of a member: previous generation's REBOUND, join result and delay, first heartbeat of that generation (bounded), last heartbeat before the REBIND, silence start (REBIND - silent_ms), silence start since the previous REBOUND
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("analyze_e18", os.path.join(HERE, "analyze_e18.py"))
e18 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e18)

d = sys.argv[1]
second = sys.argv[2] == "--second"
runs = sys.argv[3:] if second else sys.argv[2:]
if second:
    print("run member rebind_s prev_rebound_s prev_join prev_join_s prev_gen_first_hb_s last_hb_before_s silence_start_s silence_after_prev_rebound_s")
for run in runs:
    r = e18.raw(d, run)
    kill = next(int(x[0]) for x in r if x[2] == "KILL")
    if not second:
        print("==", run)
    for m in ("M1", "M2", "M3"):
        ev = [x for x in r if x[1] == m and x[2] in ("REBIND", "REBOUND", "JOINED", "JOIN_FAIL")]
        hbs = [int(x[0]) for x in r if x[1] == m and x[2] == "RX" and x[3].startswith("hb")]
        s = lambda t: (t - kill) / 1000
        if not second:
            parts = []
            for x in ev:
                extra = ""
                if x[2] == "REBOUND":
                    nxt = next((h for h in hbs if h >= int(x[0])), None)
                    extra = " hb+%.1f" % ((nxt - int(x[0])) / 1000) if nxt else " hb none"
                parts.append("%s %.1f%s" % (x[2], s(int(x[0])), extra))
            print(m, "; ".join(parts))
            continue
        rebinds = [x for x in ev if x[2] == "REBIND"]
        rebounds = [int(x[0]) for x in ev if x[2] == "REBOUND"]
        for i, rb in enumerate(rebinds[1:], start=1):
            t = int(rb[0])
            pb = rebounds[i - 1]
            mo = re.search(r"silent_ms=(\d+)", rb[3])
            sil = t - int(mo.group(1)) if mo else None
            jd = next((x for x in ev if x[2] in ("JOINED", "JOIN_FAIL") and pb <= int(x[0]) <= t), None)
            fh = next((h for h in hbs if pb <= h <= t), None)
            lh = max((h for h in hbs if h <= t), default=None)
            f = lambda v: "-" if v is None else "%.1f" % v
            print(run, m, f(s(t)), f(s(pb)), jd[2] if jd else "-", f((int(jd[0]) - pb) / 1000) if jd else "-", f((fh - pb) / 1000) if fh else "none", f((lh - pb) / 1000) if lh else "-", f(s(sil)) if sil else "-", f((sil - pb) / 1000) if sil else "-")
