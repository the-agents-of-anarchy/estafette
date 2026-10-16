#!/usr/bin/env python3
"""Analyze e5 run logs: per-run metrics (ms relative to the kill) and min/median/max tables."""
import glob, re, statistics, sys, json, collections

def parse(path):
    ev = []
    for line in open(path):
        p = line.rstrip("\n").split(" ", 3)
        if len(p) < 3 or not p[0].isdigit():
            continue
        ev.append((int(p[0]), p[1], p[2], p[3] if len(p) > 3 else ""))
    return ev

def kv(s):
    return dict(re.findall(r"(\w+)=(\S+)", s))

def rel(t, t0):
    return round((t - t0) / 1e6, 1)

def analyze_restart(ev):
    m = {}
    kill = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("KILL K"))
    reaped = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("REAPED K"))
    ready = [(t, kv(r)) for t, n, e, r in ev if n == "K" and e == "READY"]
    oldpid, newpid = ready[0][1]["pid"], ready[1][1]["pid"] if len(ready) > 1 else None
    m["k_restart_ready_ms"] = rel(ready[1][0], kill) if newpid else None
    lk = [kv(r) for t, n, e, r in ev if n == "K" and e == "LOCKED" and t > kill]
    m["k_restart_lock_wait_ms"] = float(lk[0]["waited_ms"]) if lk else None
    kup = [t for t, n, e, r in ev if n == "K" and e == "NEIGHBOR_UP" and t > kill]
    m["k_neighbor_up_first_ms"] = rel(kup[0], kill) if kup else None
    m["k_neighbor_up_count"] = len(kup)
    for p in ("P1", "P2"):
        nd = [rel(t, kill) for t, n, e, r in ev if n == p and e == "NEIGHBOR_DOWN" and r == "K" and t > kill]
        nu = [rel(t, kill) for t, n, e, r in ev if n == p and e == "NEIGHBOR_UP" and r == "K" and t > kill]
        m[f"{p}_neighbor_down_K_count"] = len(nd)
        m[f"{p}_neighbor_down_K_first_ms"] = nd[0] if nd else None
        m[f"{p}_neighbor_up_K_count"] = len(nu)
        m[f"{p}_neighbor_up_K_first_ms"] = nu[0] if nu else None
        recv = [(t, kv(r)) for t, n, e, r in ev if n == p and e == "RECV"]
        new = [t for t, k in recv if newpid and k["msg"].startswith(newpid + ":")]
        m[f"{p}_first_broadcast_from_new_K_ms"] = rel(new[0], kill) if new else None
        newfrom = [k["from"] for t, k in recv if newpid and k["msg"].startswith(newpid + ":")]
        m[f"{p}_new_K_msg_delivered_from"] = newfrom[0] if newfrom else None
        old_last = [t for t, k in recv if k["msg"].startswith(oldpid + ":")]
        m[f"{p}_last_old_K_msg_ms"] = rel(old_last[-1], kill) if old_last else None
        probes = []
        for t, n, e, r in ev:
            if n == p and e == "PROBE":
                d = kv(r)
                d["start"] = int(d["start"]); d["dur"] = float(d["dur_ms"]); d["end"] = d["start"] + int(d["dur"] * 1e6)
                probes.append(d)
        fresh = [d for d in probes if d["kind"] == "fresh" and d["start"] >= kill]
        ok_new = [d for d in fresh if d["res"] == "ok" and d.get("reply") == newpid]
        m[f"{p}_fresh_first_ok_new_complete_ms"] = rel(min(d["end"] for d in ok_new), kill) if ok_new else None
        m[f"{p}_fresh_first_ok_new_start_ms"] = rel(min(d["start"] for d in ok_new), kill) if ok_new else None
        pre = [d for d in fresh if not ok_new or d["start"] < min(x["start"] for x in ok_new)]
        bad = [d for d in fresh if d["res"] != "ok"]
        m[f"{p}_fresh_nonok_count"] = len(bad)
        m[f"{p}_fresh_timeout_count"] = len([d for d in bad if d["res"] == "timeout"])
        m[f"{p}_fresh_max_dur_ms"] = max((d["dur"] for d in fresh), default=None)
        m[f"{p}_fresh_nonok_max_dur_ms"] = max((d["dur"] for d in bad), default=None)
        after_ok_bad = [d for d in bad if ok_new and d["start"] > min(x["start"] for x in ok_new)]
        m[f"{p}_fresh_nonok_after_first_ok_count"] = len(after_ok_bad)
        m[f"{p}_fresh_ok_old_pid_after_kill"] = len([d for d in fresh if d["res"] == "ok" and d.get("reply") == oldpid])
        held = [d for d in probes if d["kind"] == "held" and d["start"] >= kill]
        hb = [d for d in held if d["res"] != "ok"]
        m[f"{p}_held_ok_new_count"] = len([d for d in held if d["res"] == "ok" and d.get("reply") == newpid])
        m[f"{p}_held_ok_old_count"] = len([d for d in held if d["res"] == "ok" and d.get("reply") == oldpid])
        m[f"{p}_held_first_nonok_complete_ms"] = rel(min(d["end"] for d in hb), kill) if hb else None
        m[f"{p}_held_timeout_count"] = len([d for d in held if d["res"] == "timeout"])
        errs = collections.Counter(d.get("msg", d["res"]).split(":")[0] + ":" + (d.get("msg", "").split(":")[-1] if d["res"] == "err" else "") for d in hb)
        m[f"{p}_held_errors"] = dict(errs)
        errs2 = [(rel(d["end"], kill), d["res"], d.get("msg", "")) for d in sorted(hb, key=lambda d: d["end"]) if d["res"] == "err"]
        m[f"{p}_held_first_err_ms"] = errs2[0][0] if errs2 else None
        m[f"{p}_held_first_err_msg"] = errs2[0][2] if errs2 else None
    ppids = {kv(r)["pid"] for t, n, e, r in ev if n in ("P1", "P2") and e == "READY"}
    kr = [t for t, n, e, r in ev if n == "K" and e == "RECV" and t > kill and kv(r)["msg"].split(":")[0] in ppids]
    m["K_new_first_recv_peer_msg_ms"] = rel(kr[0], kill) if kr else None
    m["K_new_recv_peer_msg_count"] = len(kr)
    m["_oldpid"], m["_newpid"] = oldpid, newpid
    return m

def analyze_dup(ev):
    m = {}
    k2 = [(t, kv(r)) for t, n, e, r in ev if n == "K2" and e == "READY"]
    k1 = [(t, kv(r)) for t, n, e, r in ev if n == "K" and e == "READY"]
    t_start = k2[0][0]
    pid1, pid2 = k1[0][1]["pid"], k2[0][1]["pid"]
    t_kill2 = next(t for t, n, e, r in ev if n == "ORCH" and r.startswith("KILL K2"))
    m["_pid1"], m["_pid2"] = pid1, pid2
    phases = {"before": (0, t_start), "dup": (t_start, t_kill2), "after_k2_kill": (t_kill2, 10**30)}
    for p in ("P1", "P2"):
        for ph, (a, b) in phases.items():
            nd = [t for t, n, e, r in ev if n == p and e == "NEIGHBOR_DOWN" and r in ("K", "K2") and a <= t < b]
            nu = [t for t, n, e, r in ev if n == p and e == "NEIGHBOR_UP" and r in ("K", "K2") and a <= t < b]
            m[f"{p}_{ph}_neighbor_down"] = len(nd)
            m[f"{p}_{ph}_neighbor_up"] = len(nu)
            rc = collections.Counter(kv(r)["msg"].split(":")[0] for t, n, e, r in ev if n == p and e == "RECV" and a <= t < b and kv(r)["msg"].split(":")[0] in (pid1, pid2))
            m[f"{p}_{ph}_recv_by_pid"] = {("K1" if k == pid1 else "K2"): v for k, v in rc.items()}
            pr = collections.Counter()
            for t, n, e, r in ev:
                if n == p and e == "PROBE" and a <= t < b:
                    d = kv(r)
                    key = d["kind"] + ":" + (("K1" if d.get("reply") == pid1 else "K2" if d.get("reply") == pid2 else "?") if d["res"] == "ok" else d["res"])
                    pr[key] += 1
            m[f"{p}_{ph}_probes"] = dict(pr)
    # first K2 appearance
    for p in ("P1", "P2"):
        f = [t for t, n, e, r in ev if n == p and e == "PROBE" and kv(r).get("reply") == pid2]
        m[f"{p}_first_probe_answered_by_K2_ms"] = rel(f[0], t_start) if f else None
        f = [t for t, n, e, r in ev if n == p and e == "RECV" and kv(r)["msg"].startswith(pid2 + ":")]
        m[f"{p}_first_K2_broadcast_ms"] = rel(f[0], t_start) if f else None
    ppids = {kv(r)["pid"] for t, n, e, r in ev if n in ("P1", "P2") and e == "READY"}
    for who in ("K", "K2"):
        c = [t for t, n, e, r in ev if n == who and e == "RECV" and t_start <= t < t_kill2 and kv(r)["msg"].split(":")[0] in ppids]
        m[f"{who}_recv_peer_msgs_dup_phase"] = len(c)
    c = [t for t, n, e, r in ev if n == "K" and e == "RECV" and t >= t_kill2 and kv(r)["msg"].split(":")[0] in ppids]
    m["K1_recv_peer_msgs_after_k2_kill"] = len(c)
    c = [rel(t, t_kill2) for t, n, e, r in ev if n in ("P1", "P2") and e == "RECV" and t >= t_kill2 and kv(r)["msg"].startswith(pid1 + ":")]
    m["peers_first_K1_broadcast_after_k2_kill_ms"] = c[0] if c else None
    c = [rel(t, t_start) for t, n, e, r in ev if n == "K" and e in ("NEIGHBOR_DOWN", "NEIGHBOR_UP") and t >= t_start]
    m["K1_neighbor_events_after_k2_start_ms"] = c
    c = [rel(t, t_start) for t, n, e, r in ev if n == "K" and e == "STDERR" and "same endpoint id" in r]
    m["K1_relay_status_same_id_ms"] = c
    c = [rel(t, t_kill2) for t, n, e, r in ev if n in ("K", "K2") and e == "NEIGHBOR_UP" and t >= t_kill2]
    m["K1_neighbor_up_after_k2_kill_ms"] = c
    stderr = [r for t, n, e, r in ev if e == "STDERR"]
    m["stderr_lines"] = len(stderr)
    return m

if __name__ == "__main__":
    kind, pattern = sys.argv[1], sys.argv[2]
    runs = []
    for f in sorted(glob.glob(pattern)):
        ev = parse(f)
        if kind == "lock":
            r = next(r for t, n, e, r in ev if n == "ORCH" and "RESULT" in r)
            print(f, r)
            continue
        m = analyze_restart(ev) if kind == "restart" else analyze_dup(ev)
        runs.append(m)
        print(f)
        print(json.dumps(m, indent=1))
    if kind == "restart" and runs:
        print("\nAGG")
        for k in runs[0]:
            if k.startswith("_"):
                continue
            vals = [r[k] for r in runs]
            nums = [v for v in vals if isinstance(v, (int, float))]
            if nums and len(nums) == len(vals) or (nums and all(isinstance(v, (int, float)) or v is None for v in vals)):
                print(f"{k}\tmin={min(nums)}\tmed={statistics.median(nums)}\tmax={max(nums)}\tn={len(nums)}/{len(vals)}")
            else:
                print(f"{k}\t{vals}")
