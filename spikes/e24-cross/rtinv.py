#!/usr/bin/env python3
"""Print test 4 and test 5 tables from analyze_e24 output. usage: python3 -I rtinv.py <logs dir>"""
import json
import os
import subprocess
import sys

base = sys.argv[1]
here = os.path.dirname(os.path.abspath(__file__))
out = subprocess.run([sys.executable, "-I", os.path.join(here, "analyze_e24.py"), base, "rt-1", "rt-2", "rt-3", "rt-4", "rt-5", "inv-1"], capture_output=True, text=True).stdout
d = json.loads(out)
for r in ("rt-1", "rt-2", "rt-3", "rt-4", "rt-5"):
    v = d[r]
    print(r, "H2 learned (ms after H2 process start):", {k: round(x) for k, x in v["h2_learned_ms"].items()}, "H2 ready:", v["h2_ready_ms"], "first record sent:", v["h2_first_record_ms"])
    print("   after kill: rebinds", {k: [round(x, 1) for x in y] for k, y in v["rebind"].items()}, "stalled pairs", v["stalled_pairs"], "all flowing from", v["all_flowing_from_s"], "flowing at end", v["flowing_at_end"], "unhealed", v["stalled_unhealed"], "join_fail", v["join_fail"], "rebinds before kill", v["rebinds_before_kill"])
for k, v in d["inv-1"].items():
    print(k, v)
