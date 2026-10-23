"""lab-backstop.py: the backstop for the Hetzner resources estafette creates.

Owner rulings, 2026-10-20: every created resource has its own timed control check;
each expires 24 hours after its creation; the owner, or the estafette implementer while lab
work runs (each extension journaled), may extend it; the check runs hourly from
launchd on the owner's Mac, outside Claude Code, so it outlives the session.

Managed resources carry two labels: owner=estafette and expires=<unix seconds>.

usage:
  python3 -I lab-backstop.py check [--dry-run]
  python3 -I lab-backstop.py label  <kind> <id-or-name>          right after creation
  python3 -I lab-backstop.py extend <kind> <id-or-name> <hours>  deadline = now + hours
kinds: server, primary-ip, firewall, ssh-key

check prints one line per resource. It deletes managed resources past their deadline,
servers first, then primary IPs, firewalls, and SSH keys. It deletes nothing that lacks
the owner label or a deadline; it names those instead. Owner ruling, 2026-10-21: an
expired server is kept while its type is not available in its location (scarce types are
not given back); while one is kept, no IP, firewall or key is deleted either.
"""
import datetime
import json
import os
import shutil
import subprocess
import sys
import time

HCLOUD = shutil.which("hcloud", path=os.pathsep.join([os.environ.get("PATH", ""), "/opt/homebrew/bin", "/usr/local/bin"])) or "hcloud"
OWNER = "estafette"
TTL = 24 * 3600
KINDS = ["server", "primary-ip", "firewall", "ssh-key"]


def hc(*args):
    out = subprocess.run([HCLOUD, *args], capture_output=True, text=True)
    return out.returncode, out.stdout, out.stderr.strip()


def stamp(t):
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def describe(kind, ref):
    code, out, err = hc(kind, "describe", ref, "-o", "json")
    if code != 0:
        sys.exit(f"{kind} {ref}: describe failed: {err[:200]}")
    return json.loads(out)


def set_deadline(kind, ref, expires):
    code, _, err = hc(kind, "add-label", "--overwrite", ref, f"owner={OWNER}", f"expires={expires}")
    if code != 0:
        sys.exit(f"{kind} {ref}: add-label failed: {err[:200]}")
    print(f"{kind} {ref} expires {stamp(expires)}")


def replaceable(server):
    # Owner ruling 2026-10-21: return a server only if its type can be bought again in its location.
    code, out, _ = hc("server-type", "describe", server["server_type"]["name"], "-o", "json")
    if code != 0:
        return False
    loc = server["location"]["name"]
    return any(l["name"] == loc and l.get("available") for l in json.loads(out).get("locations", []))


def check(dry):
    now = int(time.time())
    kept = []
    for kind in KINDS:
        code, out, err = hc(kind, "list", "-o", "json")
        if code != 0:
            print(f"{stamp(now)} {kind} ERROR list failed: {err[:200]}", flush=True)
            continue
        for res in json.loads(out) or []:
            labels = res.get("labels") or {}
            head = f"{stamp(now)} {kind} {res['id']} {res['name']}"
            expires = labels.get("expires", "")
            if labels.get("owner") != OWNER:
                print(f"{head} UNMANAGED, left alone", flush=True)
            elif not expires.isdigit():
                print(f"{head} NO DEADLINE, left alone", flush=True)
            elif now < int(expires):
                print(f"{head} ok, expires {stamp(int(expires))}, {(int(expires) - now) // 60} min left", flush=True)
            elif kind == "server" and not replaceable(res):
                kept.append(res["name"])
                print(f"{head} EXPIRED {stamp(int(expires))}, KEPT: {res['server_type']['name']} not available in {res['location']['name']}", flush=True)
            elif kind != "server" and kept:
                print(f"{head} EXPIRED {stamp(int(expires))}, KEPT with server {', '.join(kept)}", flush=True)
            elif dry:
                print(f"{head} EXPIRED {stamp(int(expires))}, would delete (dry run)", flush=True)
            else:
                code, _, err = hc(kind, "delete", str(res["id"]))
                result = "deleted" if code == 0 else f"DELETE FAILED: {err[:200]}"
                print(f"{head} EXPIRED {stamp(int(expires))}, {result}", flush=True)


def main(argv):
    if len(argv) in (2, 3) and argv[1] == "check":
        check("--dry-run" in argv)
        return 0
    if len(argv) == 4 and argv[1] == "label" and argv[2] in KINDS:
        created = describe(argv[2], argv[3])["created"].replace("Z", "+00:00")
        set_deadline(argv[2], argv[3], int(datetime.datetime.fromisoformat(created).timestamp()) + TTL)
        return 0
    if len(argv) == 5 and argv[1] == "extend" and argv[2] in KINDS and argv[4].isdigit():
        describe(argv[2], argv[3])
        set_deadline(argv[2], argv[3], int(time.time()) + int(argv[4]) * 3600)
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
