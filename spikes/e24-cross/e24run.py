#!/usr/bin/env python3
"""E24 cross-machine runner (runs on machine A, the Mac). usage:
  e24run.py kill  <tag> <trig:spec|hb> [window_s] [extra env k=v ...]   kill H1 (relay + bootstrap, SIGKILL) at settle end
  e24run.py quiet <tag> <secs> [env k=v ...]                            no kill, hosts alive
  e24run.py runtime <tag> [env k=v ...]                                  H1 only, H2 started later on C, then H1 killed
  e24run.py invite <tag> <n> [env k=v ...]                              fresh member on A knowing only H2, n times
Machines come from E24_B, E24_C (IPv4) and E24_KEY (ssh key). Logs go to $E24_OUT/<tag>/ (default ./logs/e24).
"""
import json
import os
import subprocess
import sys
import time

B = os.environ.get("E24_B", "178.105.88.14")
C = os.environ.get("E24_C", "2.29.112.122")
KEY = os.environ.get("E24_KEY", os.path.expanduser("~/.ssh/estafette_lab"))
KH = os.environ.get("E24_KNOWN_HOSTS", os.path.expanduser("~/.ssh/known_hosts_estafette_lab"))
OUT = os.environ.get("E24_OUT", "logs/e24")
BIN = os.environ.get("E24_BIN", "./target/release/e24-cross")
URL = {1: "https://" + B.replace(".", "-") + ".sslip.io", 2: "https://" + C.replace(".", "-") + ".sslip.io"}
SSH = ["ssh", "-i", KEY, "-o", "IdentitiesOnly=yes", "-o", f"UserKnownHostsFile={KH}", "-o", "StrictHostKeyChecking=accept-new",
       "-o", "ControlMaster=auto", "-o", "ControlPath=/tmp/e24/cm-%h", "-o", "ControlPersist=900", "-o", "ServerAliveInterval=15"]
LOCAL = {}  # name -> Popen


def now_ms():
    return int(time.time() * 1000)


def rsh(host, *cmd, timeout=120):
    r = subprocess.run(SSH + [f"root@{host}"] + list(cmd), capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    return r.stdout.strip()


def ctl(host, *args):
    return rsh(host, "/root/e24/ctl.sh", *args)


def offset(host):
    """Clock of `host` minus clock of A, ms, from the sample with the smallest round trip (7 samples)."""
    best = None
    for _ in range(7):
        a0 = now_ms()
        t = int(ctl(host, "now"))
        a1 = now_ms()
        if best is None or a1 - a0 < best[0]:
            best = (a1 - a0, t - (a0 + a1) / 2)
    return {"off_ms": round(best[1]), "rtt_ms": best[0]}


def hosts_arg(ids):
    return ",".join(f"{i}={URL[i]}" for i in ids)


def others_of(seed, members):
    return ",".join(str(s) for s in members if s != seed)


def envs(extra):
    return [e for e in extra if "=" in e]


def start_local(name, seed, hosts, members, extra, outdir):
    f = open(os.path.join(outdir, f"{name}.log"), "w")
    env = dict(os.environ, **dict(e.split("=", 1) for e in envs(extra)))
    LOCAL[name] = subprocess.Popen([BIN, "member", name, str(seed), hosts, others_of(seed, members)], stdout=f, stderr=subprocess.STDOUT, env=env, stdin=subprocess.DEVNULL)


def start_remote_member(host, name, seed, hosts, members, extra):
    ctl(host, "proc", f"m-{name}.log", *envs(extra), "./e24-cross", "member", name, str(seed), hosts, others_of(seed, members))


def start_host(host, seed, via, extra):
    args = ["./e24-cross", "host", str(seed), URL[seed]] + ([f"{via}={URL[via]}"] if via else [])
    ctl(host, "proc", f"h-H{seed}.log", *envs(extra), *args)


def stop_all():
    for p in LOCAL.values():
        p.kill()
    for p in LOCAL.values():
        p.wait()
    LOCAL.clear()
    ctl(B, "stopall")
    ctl(C, "stopall")


def fetch(host, tag, outdir):
    """Copy the remote logs of this run to outdir as <tag>-<name>."""
    tmp = os.path.join(outdir, f"_{tag}.tgz")
    rsh(host, "tar", "czf", "/tmp/e24-logs.tgz", "-C", "/root/e24/logs", ".")
    subprocess.run(["scp", "-q", "-i", KEY, "-o", "IdentitiesOnly=yes", "-o", f"UserKnownHostsFile={KH}", f"root@{host}:/tmp/e24-logs.tgz", tmp], check=True, stdin=subprocess.DEVNULL)
    d = os.path.join(outdir, tag)
    os.makedirs(d, exist_ok=True)
    subprocess.run(["tar", "xzf", tmp, "-C", d], check=True, stdin=subprocess.DEVNULL)
    os.remove(tmp)


def wait_ready(secs):
    time.sleep(secs)


def common_start(outdir, extra, members=(11, 12, 13, 14), hosts_known=(1, 2), start_h2=True):
    """Start relays and hosts, then members: M1, M2 on A, M3 on B, M4 on C."""
    stop_all()
    os.makedirs(outdir, exist_ok=True)
    meta = {"B": offset(B), "C": offset(C), "extra": extra}
    ctl(B, "relay")
    if start_h2:
        ctl(C, "relay")
    time.sleep(3)
    start_host(B, 1, None, extra)
    meta["t_h1_start_B"] = int(ctl(B, "now"))
    time.sleep(2)
    if start_h2:
        start_host(C, 2, 1, extra)
    time.sleep(3)
    h = hosts_arg(hosts_known)
    m = list(members)
    start_local("M1", m[0], h, m, extra, outdir)
    start_local("M2", m[1], h, m, extra, outdir)
    start_remote_member(B, "M3", m[2], h, m, extra)
    start_remote_member(C, "M4", m[3], h, m, extra)
    meta["t_members_start_A"] = now_ms()
    return meta


def collect(outdir, tag, meta):
    d = os.path.join(outdir, tag)
    fetch(B, "B", d)
    fetch(C, "C", d)
    for n in sorted(f for f in os.listdir(d) if f.endswith(".log")):
        os.replace(os.path.join(d, n), os.path.join(d, "A-" + n))
    json.dump(meta, open(os.path.join(d, "meta.json"), "w"), indent=1)


def run_kill(tag, trig, window, extra):
    out = os.path.join(OUT, tag)
    extra = [f"E24_TRIG={trig}"] + extra
    meta = common_start(out, extra)
    meta["kind"] = "kill"
    time.sleep(float(os.environ.get("E24_SETTLE", 40)))
    meta["t_kill_start_A"] = now_ms()
    kb = ctl(B, "kill", "h-H1.log", "relay").split()
    meta["kill_B_ms"] = [int(x) for x in kb]  # clock of B just before and after the SIGKILLs
    time.sleep(window)
    collect(OUT, tag, meta)
    stop_all()


def run_quiet(tag, secs, extra):
    out = os.path.join(OUT, tag)
    extra = ["E24_TRIG=spec"] + extra
    meta = common_start(out, extra)
    meta["kind"] = "quiet"
    time.sleep(float(secs))
    collect(OUT, tag, meta)
    stop_all()


def run_runtime(tag, extra):
    out = os.path.join(OUT, tag)
    extra = ["E24_TRIG=spec"] + extra
    stop_all()
    os.makedirs(out, exist_ok=True)
    meta = {"B": offset(B), "C": offset(C), "extra": extra, "kind": "runtime"}
    ctl(B, "relay")
    time.sleep(3)
    start_host(B, 1, None, extra)
    time.sleep(2)
    members = [11, 12, 13, 14]
    h = hosts_arg([1])
    start_local("M1", 11, h, members, extra, out)
    start_local("M2", 12, h, members, extra, out)
    start_remote_member(B, "M3", 13, h, members, extra)
    start_remote_member(C, "M4", 14, h, members, extra)
    time.sleep(float(os.environ.get("E24_SETTLE", 40)))
    ctl(C, "relay")
    time.sleep(2)
    meta["t_h2_start_C_ms"] = int(ctl(C, "now"))
    start_host(C, 2, 1, extra)
    time.sleep(20)
    kb = ctl(B, "kill", "h-H1.log", "relay").split()
    meta["kill_B_ms"] = [int(x) for x in kb]
    time.sleep(90)
    collect(OUT, tag, meta)
    stop_all()


def run_invite(tag, n, extra):
    out = os.path.join(OUT, tag)
    extra = ["E24_TRIG=spec"] + extra
    meta = common_start(out, extra)
    meta["kind"] = "invite"
    time.sleep(float(os.environ.get("E24_SETTLE", 40)))
    members = [11, 12, 13, 14]
    for i in range(int(n)):
        seed = 15 + i
        name = f"N{i + 1}"
        t = now_ms()
        start_local(name, seed, hosts_arg([2]), members + [seed], extra, out)
        meta[f"t_start_{name}"] = t
        time.sleep(25)
        LOCAL[name].kill()
        LOCAL.pop(name).wait()
    collect(OUT, tag, meta)
    stop_all()


if __name__ == "__main__":
    a = sys.argv[1:]
    try:
        if a[0] == "kill":
            run_kill(a[1], a[2], float(a[3]) if len(a) > 3 and "=" not in a[3] else 120, [x for x in a[3:] if "=" in x])
        elif a[0] == "quiet":
            run_quiet(a[1], a[2], a[3:])
        elif a[0] == "runtime":
            run_runtime(a[1], a[2:])
        elif a[0] == "invite":
            run_invite(a[1], a[2], a[3:])
    finally:
        stop_all()
