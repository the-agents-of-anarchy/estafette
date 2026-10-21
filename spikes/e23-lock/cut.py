import subprocess, sys, time, threading, json

HOST = "178.104.120.183"
SSH = ["ssh", "-i", "/Users/ryzhakar/.ssh/estafette_lab", "-o", "IdentitiesOnly=yes",
       "-o", "UserKnownHostsFile=/Users/ryzhakar/.ssh/known_hosts_estafette_lab",
       "-o", "ConnectTimeout=3", "-o", "ServerAliveInterval=2", "-o", "ServerAliveCountMax=2", f"lab@{HOST}"]
BIN = "/home/lab/e23/target/release/e23-lock"
D = "/home/lab/e23/data"

def ssh(cmd, timeout=60):
    return subprocess.run(SSH + [cmd], capture_output=True, text=True, timeout=timeout)

def trial(variant, i, fsync, gap):
    d = f"{D}/{variant}{i}"
    r = ssh(f"rm -rf {d} && mkdir -p {d} && sync && echo prepared")
    assert "prepared" in r.stdout, r
    args = f"{BIN} hold {d}" + (" --fsync" if fsync else "")
    p = subprocess.Popen(SSH + [args], stdout=subprocess.PIPE, text=True, bufsize=1)
    lines = []
    ev = threading.Event()
    def rd():
        for l in p.stdout:
            t = time.time()
            lines.append((t, l.strip()))
            if l.startswith("LOCKED"):
                ev.set()
    th = threading.Thread(target=rd, daemon=True)
    th.start()
    assert ev.wait(30), "no LOCKED"
    t_locked = [t for t, l in lines if l.startswith("LOCKED")][0]
    created = [l for t, l in lines if l.startswith("CREATED")]
    key = created[0].split("hex=")[1].split()[0]
    time.sleep(max(0, gap - (time.time() - t_locked)))
    t_r0 = time.time()
    rr = subprocess.run(["hcloud", "server", "reset", "estafette-e23"], capture_output=True, text=True)
    t_r1 = time.time()
    last_tick = [l for t, l in lines if l.startswith("TICK")]
    last_tick_n = int(last_tick[-1].split()[1]) if last_tick else 0
    created_t = int(created[0].split("t=")[1])
    last_t = int(last_tick[-1].split("t=")[1]) if last_tick else created_t
    p.kill()
    # boot: poll ssh
    t_up = None
    while time.time() - t_r1 < 240:
        try:
            q = subprocess.run(SSH + ["uptime -s; cat /proc/uptime"], capture_output=True, text=True, timeout=8)
            if q.returncode == 0:
                t_up = time.time()
                up = q.stdout.strip().replace("\n", " | ")
                break
        except subprocess.TimeoutExpired:
            pass
        time.sleep(1)
    out = {"variant": variant, "run": i, "fsync": fsync, "gap_target_s": gap,
           "locked_to_reset_call_s": round(t_r0 - t_locked, 2), "locked_to_reset_return_s": round(t_r1 - t_locked, 2),
           "reset_rc": rr.returncode, "last_tick_seen_by_mac": last_tick_n, "cut_after_creation_s_upper_bound_lastTick": round((last_t - created_t) / 1e9, 2),
           "boot_s_reset_return_to_ssh": round(t_up - t_r1, 1) if t_up else None, "uptime": up if t_up else None}
    if t_up:
        c = ssh(f"{BIN} check {d} {key}; timeout 3 {BIN} hold {d}; echo hold_rc=$?")
        out["check"] = c.stdout.strip().split("\n")
        out["check_err"] = c.stderr.strip()
    return out

variants = {"A": (False, 0), "B": (True, 0), "C": (False, 10), "D": (False, 45)}
which = sys.argv[1].split(",")
n = int(sys.argv[2])
for v in which:
    fs, gap = variants[v]
    for i in range(n):
        o = trial(v, i, fs, gap)
        print(json.dumps(o), flush=True)
