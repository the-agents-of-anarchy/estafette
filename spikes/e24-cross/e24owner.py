#!/usr/bin/env python3
"""E24 owner-assisted tests, run on machine A (the Mac). The owner's steps are printed by `owner-tests.sh`; this file does the work.
usage: e24owner.py sleep|wifi <tag>
Flow: start H1 (B) and H2 (C) with their relays and members M1, M2 (A), M3 (B), M4 (C), settle 40 s, then wait for the owner (Enter), record
the event, wait for recovery (150 s), collect the logs. Files in $E24_OUT/<tag>/: member and host logs, ticks.txt (A's clock, once a second), ip.txt (public IPv4 of A, every 2 s), events.json.
"""
import json
import os
import subprocess
import sys
import threading
import time

import e24run as R

# no ssh connection sharing: a shared connection dies when the Mac changes network
R.SSH = ["ssh", "-i", R.KEY, "-o", "IdentitiesOnly=yes", "-o", f"UserKnownHostsFile={R.KH}", "-o", "StrictHostKeyChecking=accept-new", "-o", "ControlMaster=no", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=15"]


def ticker(path, stop):
    with open(path, "w") as f:
        while not stop.is_set():
            f.write(f"{R.now_ms()}\n")
            f.flush()
            time.sleep(1)


def ip_poller(path, stop):
    with open(path, "w") as f:
        while not stop.is_set():
            try:
                ip = subprocess.run(["curl", "-s", "-m", "4", "https://ifconfig.me"], capture_output=True, text=True, timeout=6).stdout.strip()
            except Exception:
                ip = "-"
            f.write(f"{R.now_ms()} {ip or '-'}\n")
            f.flush()
            time.sleep(2)


def main():
    kind, tag = sys.argv[1], sys.argv[2]
    out = os.path.join(R.OUT, tag)
    extra = ["E24_TRIG=spec", "E24_HOST_DEAF=1"]
    meta = R.common_start(out, extra)
    meta["kind"] = f"owner-{kind}"
    stop = threading.Event()
    threading.Thread(target=ticker, args=(os.path.join(out, "ticks.txt"), stop), daemon=True).start()
    if kind == "wifi":
        threading.Thread(target=ip_poller, args=(os.path.join(out, "ip.txt"), stop), daemon=True).start()
    time.sleep(40)
    ev = {}
    if kind == "sleep":
        print("READY. Members run. Press Enter; the Mac sleeps 5 s later. Wake it after 2 minutes by pressing a key.", flush=True)
        input()
        ev["enter_ms"] = R.now_ms()
        time.sleep(5)
        ev["sleep_cmd_ms"] = R.now_ms()
        subprocess.run(["pmset", "sleepnow"])
        # execution resumes after wake; the tick gap in ticks.txt shows the sleep
        time.sleep(2)
        ev["woke_seen_ms"] = R.now_ms()
        print("Awake. Recovery is measured for 150 s; do not touch the network.", flush=True)
    else:
        print("READY. Members run. Now switch the Mac from home Wi-Fi to the phone hotspot, wait until it is connected, then press Enter here.", flush=True)
        input()
        ev["enter_ms"] = R.now_ms()
        print("Recovery is measured for 150 s; stay on the hotspot.", flush=True)
    time.sleep(150)
    meta["events"] = ev
    stop.set()
    R.collect(R.OUT, tag, meta)
    R.stop_all()
    print("Done. Logs in", out, flush=True)


if __name__ == "__main__":
    try:
        main()
    finally:
        R.stop_all()
