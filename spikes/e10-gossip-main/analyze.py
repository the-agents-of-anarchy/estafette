#!/usr/bin/env python3
"""E10 analysis: recompute per-run measures from the raw logs, independent of the harness's own summary.

usage: python3 -I analyze.py <logs-dir> [window_ms] [name-regex]
Reads <logs-dir>/raw/<variant>-<scn><tag>-<run>.raw.log(.gz); prints per-run lines and per-cell tables.
Run tag: variant a|b, scn 1 (kill R1+B1) or 7 (kill B1 only); tag np = no probers, npw = no probers 180 s window,
w = probers 180 s window. The window defaults to 88000 (180000 for npw and w); window_ms truncates every run to that window.
A record is `<unix_ms> <who> <kind> <rest>`; the KILL record of CTL gives t0.
"""
import glob
import gzip
import os
import re
import statistics
import sys

STALL_MS = 5000  # an ordinary heartbeat gap is ~1 s; every stalled pair in the data is >= 20 s


def read_records(path):
    opener = gzip.open if path.endswith(".gz") else open
    recs = []
    with opener(path, "rt") as f:
        for line in f:
            parts = line.rstrip("\n").split(" ", 3)
            if len(parts) < 3:
                continue
            ts, who, kind = int(parts[0]), parts[1], parts[2]
            recs.append((ts, who, kind, parts[3] if len(parts) > 3 else ""))
    return recs


def hb_times(recs, rx, tx, lo, hi):
    pat = f"hb from={tx} "
    return sorted(ts for ts, who, kind, rest in recs if who == rx and kind == "RX" and rest.startswith(pat) and lo <= ts <= hi)


def longest_gap(ts_list, hi):
    """(gap_ms, resume_ts or None). Same definition as the E8 harness: gaps between consecutive HBs plus the trailing gap to the window end."""
    best, resume = 0, None
    for a, b in zip(ts_list, ts_list[1:]):
        if b - a > best:
            best, resume = b - a, b
    if ts_list and hi - ts_list[-1] > best:
        best, resume = hi - ts_list[-1], None
    if not ts_list:
        best, resume = None, None
    return best, resume


def first_after(recs, after, pred):
    c = [ts for ts, who, kind, rest in recs if ts >= after and pred(who, kind, rest)]
    return min(c) if c else None


def analyze(path, window):
    recs = read_records(path)
    kill = [r for r in recs if r[2] == "KILL"]
    if not kill:
        return None
    t0 = kill[0][0]
    end = t0 + window
    r2 = next((re.search(r"relays=\[(\d+)\]", rest).group(1) for ts, who, kind, rest in recs if who == "M2" and kind == "BOUND"), None)
    m = {"t0": t0}
    rehome = first_after(recs, t0, lambda who, kind, rest: who == "M1" and kind == "ADDR" and rest == f"relays=[{r2}]")
    m["rehome"] = None if rehome is None else rehome - t0
    # HB pairs
    pairs = {}
    for rx in ("M1", "M2", "M3"):
        for tx in ("M1", "M2", "M3"):
            if rx == tx:
                continue
            ts_list = hb_times(recs, rx, tx, t0 - 3000, end)
            gap, resume = longest_gap(ts_list, end)
            last = max(ts_list) - t0 if ts_list else None
            after_rehome = None
            if rehome is not None:
                c = [t for t in ts_list if t >= rehome]
                after_rehome = (min(c) - t0) if c else None
            stalled = gap is not None and gap >= STALL_MS
            flowing_end = bool(ts_list) and (end - max(ts_list)) < STALL_MS
            pairs[(rx, tx)] = dict(gap=gap, resume=None if resume is None else resume - t0, last=last, after_rehome=after_rehome, stalled=stalled, flowing_end=flowing_end)
    m["pairs"] = pairs
    # M2-M3 summary (both homed on the live relay)
    g23 = [pairs[("M2", "M3")]["gap"], pairs[("M3", "M2")]["gap"]]
    m["m23_gap"] = None if None in g23 else max(g23)
    m["m23_stalled"] = any(pairs[p]["stalled"] for p in (("M2", "M3"), ("M3", "M2")))
    m["m23_recovered"] = m["m23_stalled"] and all(pairs[p]["flowing_end"] for p in (("M2", "M3"), ("M3", "M2")))
    rs = [pairs[p]["resume"] for p in (("M2", "M3"), ("M3", "M2")) if pairs[p]["stalled"] and pairs[p]["flowing_end"]]
    m["m23_resume"] = max(rs) if (m["m23_recovered"] and rs and None not in rs) else None
    # M1 pairs
    m1p = [("M2", "M1"), ("M3", "M1"), ("M1", "M2"), ("M1", "M3")]
    m["m1_stalled_pairs"] = sum(pairs[p]["stalled"] for p in m1p)
    m["m1_recovered_pairs"] = sum(pairs[p]["stalled"] and pairs[p]["flowing_end"] for p in m1p)
    m["m1_resume_first"] = min((pairs[p]["resume"] for p in m1p if pairs[p]["stalled"] and pairs[p]["flowing_end"] and pairs[p]["resume"] is not None), default=None)
    m["m1_gap_max"] = max((pairs[p]["gap"] for p in m1p if pairs[p]["gap"] is not None), default=None)
    # fresh dials (probers only)
    probe = [(ts, who, rest) for ts, who, kind, rest in recs if kind == "PROBE"]
    m["probers"] = bool(probe)

    def reach(prober, tgt):
        fail = min((ts for ts, who, rest in probe if who == prober and ts >= t0 and rest.startswith(f"tgt={tgt} fail")), default=None)
        ok = None if fail is None else min((ts for ts, who, rest in probe if who == prober and ts >= fail and rest.startswith(f"tgt={tgt} ok")), default=None)
        return (None if fail is None else fail - t0, None if ok is None else ok - t0)

    for p, t in (("M2", "M1"), ("M3", "M1"), ("M2", "M3"), ("M3", "M2")):
        m[f"dial_{p}_{t}"] = reach(p, t)
    # neighbor events seen by the members (the harness stream is incomplete: subscribe_and_join consumes the first NeighborUp)
    m["first_nd"] = min((ts for ts, who, kind, rest in recs if kind == "ND" and who in ("M1", "M2", "M3") and ts >= t0), default=None)
    m["first_nd"] = None if m["first_nd"] is None else m["first_nd"] - t0
    m["nu_after_kill"] = sorted(ts - t0 for ts, who, kind, rest in recs if kind == "NU" and who in ("M1", "M2", "M3") and t0 <= ts <= end)
    return m


def fmt(v):
    return "NA" if v is None else str(int(v))


def stats(vals):
    v = sorted(x for x in vals if x is not None)
    if not v:
        return None
    return (v[0], statistics.median(v), v[-1], len(v))


GROUPS = [
    # (variant label, condition label, file-name regex, window ms)
    ("A 0.101.0", "kill R1+B1, probers on, 88 s", r"^a-1-[123]$", 88000),
    ("B main", "kill R1+B1, probers on, 88 s", r"^b-1-[123]$", 88000),
    ("A 0.101.0", "kill R1+B1, no probers, 88 s", r"^a-1np-[123]$", 88000),
    ("B main", "kill R1+B1, no probers, 88 s", r"^b-1np-[123]$", 88000),
    ("A 0.101.0", "control: kill B1 only, R1 alive, probers on, 88 s", r"^a-7-[123]$", 88000),
    ("B main", "control: kill B1 only, R1 alive, probers on, 88 s", r"^b-7-[123]$", 88000),
    ("A 0.101.0", "ext: no probers, runs 1-6, 88 s", r"^a-1np-[1-6]$", 88000),
    ("B main", "ext: no probers, runs 1-6, 88 s", r"^b-1np-[1-6]$", 88000),
    ("A 0.101.0", "ext: no probers, all 9 runs cut to 88 s", r"^a-1npw?-\d+$", 88000),
    ("B main", "ext: no probers, all 9 runs cut to 88 s", r"^b-1npw?-\d+$", 88000),
    ("A 0.101.0", "supp: no probers, 180 s window", r"^a-1npw-\d+$", 180000),
    ("B main", "supp: no probers, 180 s window", r"^b-1npw-\d+$", 180000),
]


def cell(s, n):
    return ("-", "-", "-", f"0/{n}") if s is None else (fmt(s[0]), fmt(s[1]), fmt(s[2]), f"{s[3]}/{n}")


def table(logs):
    print("| Variant | Condition | Measure | min | median | max | n |")
    print("|---|---|---|---|---|---|---|")
    for variant, cond, rx, window in GROUPS:
        runs = []
        for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*"))):
            name = os.path.basename(p).split(".raw.log")[0]
            if re.match(rx, name):
                m = analyze(p, window)
                if m:
                    runs.append(m)
        n = len(runs)
        if not n:
            continue
        probers = any(r["probers"] for r in runs)
        stalled = sum(r["m23_stalled"] for r in runs)
        rec = sum(r["m23_recovered"] for r in runs)
        m1s = sum(r["m1_stalled_pairs"] for r in runs)
        m1r = sum(r["m1_recovered_pairs"] for r in runs)
        d_m1 = [min([x for x in (r["dial_M2_M1"][1], r["dial_M3_M1"][1]) if x is not None], default=None) for r in runs]
        d_23 = [min([x for x in (r["dial_M2_M3"][1], r["dial_M3_M2"][1]) if x is not None], default=None) for r in runs]
        rows = [
            ("M2-M3 longest HB gap, ms (max of both directions)", cell(stats([r["m23_gap"] for r in runs]), n)),
            ("M2-M3 stalled (longest gap >= 5 s), runs", ("-", "-", "-", f"{stalled}/{n}")),
            ("M2-M3 flowing again at window end, of stalled runs", ("-", "-", "-", f"{rec}/{stalled}")),
            ("M2-M3 resume time, ms after kill", cell(stats([r["m23_resume"] for r in runs]), n)),
            ("M1 re-home (watch_addr lists only R2), ms after kill", cell(stats([r["rehome"] for r in runs]), n)),
            ("M1 HB pairs stalled (4 per run)", ("-", "-", "-", f"{m1s}/{4 * n}")),
            ("M1 HB pairs flowing again at window end, of stalled", ("-", "-", "-", f"{m1r}/{m1s}")),
            ("M1 HB first resume, ms after kill (earliest pair per run)", cell(stats([r["m1_resume_first"] for r in runs]), n)),
        ]
        if probers:
            rows.append(("fresh dial by id to M1 ok again, ms after kill (earlier of M2, M3 per run)", cell(stats(d_m1), n)))
            rows.append(("fresh dial M2 to M3 or M3 to M2 ok again, ms after kill", cell(stats(d_23), n)))
        for label, (a, b, c, d) in rows:
            print(f"| {variant} | {cond} | {label} | {a} | {b} | {c} | {d} |")


def runs_table(logs):
    print("| Run | Re-home (s) | M2-M3 longest gap (s) | M2-M3 flowing again at (s) | M1 pairs stalled | M1 pairs flowing again | M1 first resume (s) | fresh dial to M1 ok (s) | member NeighborUp after kill (s) |")
    print("|---|---|---|---|---|---|---|---|---|")
    order = []
    for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*"))):
        name = os.path.basename(p).split(".raw.log")[0]
        mo = re.match(r"^([ab])-(\d+)(np|npw|w)?-(\d+)$", name)
        if mo:
            order.append((mo.group(1), mo.group(2), {"": 0, "np": 1, "npw": 2, "w": 3}[mo.group(3) or ""], int(mo.group(4)), name, p, mo.group(3) or ""))
    order.sort(key=lambda t: (t[1], t[2], t[0], t[3]))

    def sec(v):
        return "-" if v is None else f"{v / 1000:.1f}"

    for variant, scn, _, run, name, p, tag in order:
        window = 180000 if tag in ("npw", "w") else 88000
        m = analyze(p, window)
        if m is None:
            continue
        d1 = min([x for x in (m["dial_M2_M1"][1], m["dial_M3_M1"][1]) if x is not None], default=None)
        nus = ",".join(f"{t / 1000:.0f}" for t in m["nu_after_kill"])
        dial = sec(d1) if m["probers"] else "n/a"
        print(f"| {name} | {sec(m['rehome'])} | {sec(m['m23_gap'])} | {sec(m['m23_resume'])} | {m['m1_stalled_pairs']}/4 | {m['m1_recovered_pairs']}/{m['m1_stalled_pairs']} | {sec(m['m1_resume_first'])} | {dial} | {nus or '-'} |")


def gaps(logs):
    """Distribution of the per-pair longest heartbeat gap over every run: shows the gap between healthy and stalled."""
    vals = []
    for p in sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*"))):
        name = os.path.basename(p).split(".raw.log")[0]
        mo = re.match(r"^([ab])-(\d+)(np|npw|w)?-(\d+)$", name)
        if not mo:
            continue
        m = analyze(p, 180000 if mo.group(3) in ("npw", "w") else 88000)
        vals.extend(v["gap"] for v in m["pairs"].values() if v["gap"] is not None)
    vals.sort()
    print(f"pairs: {len(vals)}; healthy (< 5 s): {sum(v < 5000 for v in vals)}, max healthy gap {max(v for v in vals if v < 5000)} ms; stalled (>= 5 s): {sum(v >= 5000 for v in vals)}, min stalled gap {min(v for v in vals if v >= 5000)} ms")
    print("gaps between 1.3 s and 50 s:", [v for v in vals if 1300 <= v <= 50000])


def main():
    if sys.argv[1] == "--gaps":
        gaps(sys.argv[2])
        return
    if sys.argv[1] == "--runs":
        runs_table(sys.argv[2])
        return
    if sys.argv[1] == "--table":
        table(sys.argv[2])
        return
    logs = sys.argv[1]
    override = int(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2] != "-" else None
    only = re.compile(sys.argv[3]) if len(sys.argv) > 3 else None
    paths = sorted(glob.glob(os.path.join(logs, "raw", "*.raw.log*")))
    cells = {}
    for p in paths:
        name = os.path.basename(p).split(".raw.log")[0]
        mo = re.match(r"^([ab])-(\d+)(np|npw|w)?-(\d+)$", name)
        if not mo or (only and not only.search(name)):
            continue
        variant, scn, tag, run = mo.group(1), mo.group(2), mo.group(3) or "", int(mo.group(4))
        np_ = "np" if tag.startswith("np") else ""
        window = override or (180000 if tag in ("npw", "w") else 88000)
        m = analyze(p, window)
        if m is None:
            print(f"{name}: no KILL record")
            continue
        cells.setdefault((variant, scn, tag), []).append((run, name, m))
    for key in sorted(cells):
        variant, scn, tag = key
        print(f"\n## cell variant={variant} scn={scn} tag={tag or '-'} ({'no probers' if tag.startswith('np') else 'probers'}) window={override or (180000 if tag in ('npw', 'w') else 88000)}")
        for run, name, m in sorted(cells[key]):
            nus = ",".join(str(int(t)) for t in m["nu_after_kill"])
            print(
                f"{name}: rehome={fmt(m['rehome'])} m23_gap={fmt(m['m23_gap'])} m23_stalled={m['m23_stalled']} m23_recovered={m['m23_recovered']} m23_resume={fmt(m['m23_resume'])} "
                f"m1_stalled_pairs={m['m1_stalled_pairs']} m1_recovered_pairs={m['m1_recovered_pairs']} m1_resume_first={fmt(m['m1_resume_first'])} m1_gap_max={fmt(m['m1_gap_max'])} "
                f"first_nd={fmt(m['first_nd'])} dials(M2>M1,M3>M1)={m['dial_M2_M1']},{m['dial_M3_M1']} dials(M2>M3,M3>M2)={m['dial_M2_M3']},{m['dial_M3_M2']} member-NU-after-kill[{nus}]"
            )
        runs = [m for _, _, m in cells[key]]
        n = len(runs)
        rows = [
            ("M2-M3 longest HB gap, max of both directions (ms)", stats([r["m23_gap"] for r in runs])),
            ("M1 re-home: watch_addr lists only R2 (ms after kill)", stats([r["rehome"] for r in runs])),
            ("M2-M3 stall resumed, time (ms after kill)", stats([r["m23_resume"] for r in runs])),
            ("M1 pairs resumed, first resume (ms after kill)", stats([r["m1_resume_first"] for r in runs])),
            ("fresh dial to M1 ok again, M2 or M3 (ms after kill)", stats([min([x for x in (r["dial_M2_M1"][1], r["dial_M3_M1"][1]) if x is not None], default=None) for r in runs])),
        ]
        for label, s in rows:
            print(f"  {label}: " + ("none" if s is None else f"min {fmt(s[0])} median {fmt(s[1])} max {fmt(s[2])} n {s[3]}/{n}"))
        print(f"  M2-M3 stalled: {sum(r['m23_stalled'] for r in runs)}/{n}; recovered in window: {sum(r['m23_recovered'] for r in runs)}/{n}")
        print(f"  M1 pairs stalled (of {4 * n}): {sum(r['m1_stalled_pairs'] for r in runs)}; recovered in window: {sum(r['m1_recovered_pairs'] for r in runs)}")


if __name__ == "__main__":
    main()
