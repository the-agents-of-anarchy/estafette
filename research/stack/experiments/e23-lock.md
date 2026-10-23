# E23: identity lock on Linux, power cut, address-change trigger

Date 2026-10-21. Branch `spike/e23-lock` on estafette, commit `006cbfe` (pushed), worktree `/Users/ryzhakar/pp/estafette-e23`. Packages `spikes/e23-lock/` (std only) and `spikes/e23-addr/` (iroh 1.3.0, iroh-gossip 0.101.0, iroh-relay 1.3.0 `server`, netwatch 0.19.3, n0-watcher 1.0.1), raw logs in `spikes/e23-addr/out/`.

## Answer

- Lock on Linux (ext4, kernel 6.8.0-138, cpx22): second instance refused 5/5; free 154-232 us after SIGKILL 5/5; restart starts 5/5; freed with a live forked child 5/5.
- Power cut (hard reset, n=5 per variant): the lock was free on every boot (20/20) and a new instance started on the first try, but the sidecar did not always survive. A sidecar written without fsync and cut 1.1-1.7 s later was MISSING 5/5 (the card key is lost, the instance would mint a new identity); with file and directory fsync it was intact 5/5 (cut at 1.1-2.1 s); without fsync it was intact 5/5 at about 11 s and 5/5 at about 46 s. Never empty, never half-written. A continuously appended file without fsync lost its tail and showed NUL-filled blocks and no final newline in 10/10 cuts at 11 s and 46 s.
- So S14 needs one added rule: write the sidecar with `sync_all` on the file and on its directory before taking the lock. Lock itself needs nothing (kernel memory, gone at reset).
- Part 2: no unprivileged way to change an interface address on this Mac (`sudo -n` needs a password). Measured instead: no change gives no watcher item and no announcement (0 in 100 s); `network_change()` gives none (0 in 15 calls); stopping the home relay gives a `home_relay_status` item at once (0.3-1.3 ms) but `watch_addr` stays silent for 36.7-43.0 s (4 of 5 runs; 1 of 5 none in 45 s), then peers see the new address 1.2-1.6 ms after the watcher fires.
- S13 text as staged would announce about 20 times in 12 s while a relay is down (every status item, address unchanged). Debounce needed.

## Setup

Part 2 (Mac, Apple M2 Max, Darwin 24.3.0, rustc 1.99.1, no sudo). One process holds relays RA and RB (in-process, plain HTTP, 127.0.0.1), member M (bound 0.0.0.0:0, both relays, key fixed, address `192.168.0.140:<port>` on en0 plus a relay URL), observers O1 and O2 (0.0.0.0:0, relay disabled, learn M only from the bootstrap address and from M's Presence). M announces when gossip reports its first neighbor (S15), then on every `watch_addr()` item and every `home_relay_status()` item after the first (S13 as staged); the Presence carries `ep.addr()` as text. Clock: wall-clock ns, one process. Scenarios: `control` (20 s idle), `netchange` (three `network_change()` calls 5 s apart), `relayswitch` (kill M's home relay, window 12 s and 45 s), `relayswitch-nowatch` (same, no re-announce, items only logged), `restart` (close M, rebind same key on a new port, bootstrap through observers). n=5 runs each.

Part 1 (Hetzner lab server; kill tests also on Mac and a Docker Desktop Linux VM): `e23-lock` takes `File::try_lock()` (flock(2), non-blocking exclusive) on the sidecar `card.key` (32 random bytes, mode 0600, created if absent, never unlinked), then holds. Modes: `hold`, `check` (what a new instance sees after a cut: sidecar state, journal tail, lock, START_OK), `killtest` (control, SIGKILL, spin on try_lock from kill return, restart), `childtest` (holder forks `sleep 60` via `std::process::Command`, holder SIGKILLed). Power cut on the lab server: `cut.py` (Mac side, committed with `cut-results.jsonl`). Per trial: `mkdir` of a fresh directory plus `sync` (so the directory entry is not the variable), start `hold` over ssh (creates the sidecar, with `--fsync` = `sync_all` on the file then on its directory; takes the lock; appends a journal line every 2 ms without fsync), cut with `hcloud server reset estafette-e23` (hard reset), poll ssh every second, then run `check` against the key printed at creation and start `hold` once more. Variants A: no fsync, B: fsync, both cut about 1-2 s after creation (the `hcloud` call blocks about 10 s, but the VM stops about 1-2 s after the first journal write; the exact cut time is the last TICK seen, column below); C: no fsync, cut at about 11 s; D: no fsync, cut at about 46 s. n=5 each, 20 hard resets.

## Results

### Source: what signals an address change (iroh 1.3.0, iroh-gossip 0.101.0, netwatch 0.19.3, read at those versions)

`iroh-1.3.0/src/endpoint.rs:1289-1303`: `watch_addr` is `watch_addrs.or(watch_relay).map(...)`, i.e. the direct-address set joined with the home-relay list, mapped to an `EndpointAddr`:
```
let watch_addrs = self.inner.ip_addrs();
let watch_relay = self.inner.home_relay();
...
watch_addrs.or(watch_relay).map(move |(addrs, relays)| { EndpointAddr::from_parts(endpoint_id, relays.into_iter().map(TransportAddr::Relay).chain(addrs.into_iter().map(|x| TransportAddr::Ip(x.addr)))) })
```
`socket.rs:1895-1950` (`collect_local_addresses`): with a socket bound to the unspecified address, every local IP is paired with the bound port; this is the only place local interface addresses enter the set. `socket.rs:1600-1609`: a netmon state item runs `is_major_change`, then `handle_network_change(is_major)`; `socket.rs:1657-1679`: major rebinds the transports, checks the relay connection, resets DNS and re-stuns; minor only re-stuns. `netwatch-0.19.3/src/interfaces.rs:320-326`: major when `have_v4`, `have_v6`, `is_expensive` or `default_route_interface` differ, or an interesting interface differs or its prefixes differ. `netwatch/src/netmon/bsd.rs:43`: on macOS the monitor is a raw `AF_ROUTE` socket; interface-address and route messages count, `RTM_MISS` does not (`bsd.rs:175-178`). `socket.rs:2000-2004`: periodic re-stun every 20-26 s ("a random duration between 20 and 26 seconds"). `endpoint.rs:1665-1670` doc of `network_change()`: "Even when the network did not change, or iroh was already able to detect the network change itself, there is no harm in calling this function." `iroh-gossip-0.101.0/src/net.rs:368,485-487`: gossip itself subscribes to `watch_addr().stream()` and feeds each item to `UpdatePeerData`, so its own Neighbor/Join messages carry the current address; this is separate from our Presence.

### Source: lock

`std::fs::File::try_lock` on Linux and macOS is `libc::flock(fd, LOCK_EX | LOCK_NB)` (`library/std/src/sys/fs/unix.rs:1370-1371`, the stable toolchain's std on this Mac); `WouldBlock` maps to `TryLockError::WouldBlock`. File opens are `O_CLOEXEC` (`unix.rs:1198`). flock locks belong to the open file description and go when the last descriptor to it closes.

### Part 1, Linux, kill only (Docker Desktop VM, kernel 6.12.5-linuxkit, root on overlayfs; binary built in rust:1.99.1, run in debian:stable-slim)

Control (a) and SIGKILL (b), n=5, spin on `try_lock` from the moment `kill()` returned:

| Measure | min | median | max | n |
|---|---|---|---|---|
| second instance, other process, refused (`CARD_HELD`, exit 3) | | | | 5/5 |
| second open of the sidecar in the holder's own process, refused | | | | 5/5 |
| SIGKILL to first successful `try_lock` (us) | 64 | 290 | 404 | 5/5 |
| lock free after `wait()` on the killed holder | | | | 5/5 |
| restarted instance took the lock and ran | | | | 5/5 |
| holder with a live forked child (`sleep`, via `Command`), lock free after holder SIGKILL, child still alive | | | | 5/5 |

Same program on macOS (local control for the harness): refused 5/5, free 114-120 us after kill (12 tries each), restart 5/5; matches e5's 6.5-10.4 ms (that figure was bounded by e5's 10 ms retry).

### Part 1, Linux lab server (Hetzner cpx22, ubuntu-24.04, nbg1, kernel 6.8.0-138-generic, rustc 1.99.1)

Filesystem: `/dev/sda1 ext4 rw,relatime,mb_optimize_scan=0` (root, default mount; `/home` is on it); `vm.dirty_expire_centisecs=3000`, `vm.dirty_writeback_centisecs=500`.

Control (a) and SIGKILL (b), n=5 (spin on `try_lock` from `kill()`'s return): second instance refused 5/5 (other process and a second open in the holder's own process); lock free 154, 188, 191, 224, 232 us after the kill (13-29 tries); free after `wait()` 5/5; restarted instance ran 5/5. Holder with a forked `sleep` child alive: lock free after holder SIGKILL 5/5, child alive 5/5.

Power cut (c), hard reset, n=5 per variant. "cut after creation" is the server-clock time of the last TICK the Mac saw minus the sidecar creation time (upper bound, ssh-stream delay is small); "boot" is the reset call returning to the first successful ssh.

| Variant | cut after creation (s) | sidecar after boot | lock | new instance starts | journal after boot | boot (s) |
|---|---|---|---|---|---|---|
| A: no fsync, cut early | 1.05-1.69 | missing 5/5 (file absent, directory present) | `try_lock` failed 5/5 only because the file did not exist; a new `hold` then created a fresh key | yes, with a NEW key 5/5 | missing 5/5 | 16.4-23.7 |
| B: file and directory fsync, cut early | 1.05-2.12 | intact 5/5 (32 bytes, key equal to the printed one) | free first try 5/5 (0.56-1.24 ms) | yes 5/5 (START_OK, `hold` LOCKED) | missing 5/5 (never synced, created after the sync) | 16.6-23.3 |
| C: no fsync, cut at about 11 s | 11.0-11.6 | intact 5/5 | free first try 5/5 (1.5-2.1 ms) | yes 5/5 | present 5/5, size 61440-65536 bytes, NUL bytes 422-3118, no final newline, last counter 2405-2541 (about 5000 lines had been written: roughly half lost) | 14.7-23.6 |
| D: no fsync, cut at about 46 s | 45.9-47.0 | intact 5/5 | free first try 5/5 (2.1-4.5 ms) | yes 5/5 | present 5/5, 241664-499712 bytes, NUL bytes 44-3636, no final newline, last counter 9707-19507 (about 21000 had been written) | 17.6-24.9 |

Boot over all 20: 14.7-24.9 s from reset return to ssh (uptime at first ssh 12-19 s; the `hcloud server reset` call itself returns after about 10 s). The sidecar was never empty and never half-written in any of 20 cuts; it was either whole or absent.

Mac, same program (harness control): refused 5/5, free 114-120 us after kill, restart 5/5; e5 reported 6.5-10.4 ms (bounded by its 10 ms retry).

Docker Desktop VM (Linux 6.12.5-linuxkit, overlayfs), kill only, before the lab existed: refused 5/5, free 64-404 us, restart 5/5, forked child case 5/5. Agrees with the lab numbers.

### Part 2, address change, Mac

| Scenario | Measure | Values | n |
|---|---|---|---|
| control, 20 s steady | `watch_addr` items / `home_relay_status` items / announcements | 0 / 0 / 0 each run | 5/5 |
| `network_change()` x3 per run | same three counts per call | 0 / 0 / 0 in all 15 calls | 15/15 |
| home relay stopped, window 12 s, watcher on | first `home_relay_status` item after the stop (ms) | 0.45-1.35 (median 0.67) | 5/5 |
| | first announcement after the stop (ms) | 0.50-1.59 (median 0.78) | 5/5 |
| | `watch_addr` items in 12 s | 0 | 5/5 |
| | status items / announcements in 12 s | 20 / 20 | 5/5 |
| same, watcher off | status items / announcements | 20 / 0 | 5/5 |
| home relay stopped, window 45 s, watcher on | `watch_addr` fires with the other relay (ms after stop) | 36693, 37081, 41418, 43010; none in run 2 | 4/5 |
| | observers see the new relay in Presence after that item (ms) | 1.2-1.6 (O1 and O2) | 4/4 |
| | status items / announcements in 45 s | 24-27 / 24-28 | 5/5 |
| same key, new port, rebind after close | first Presence after rebind returns, at O1 / O2 (ms) | 8.2-14.6 / 8.4-14.8, new address in the Presence | 5/5 |

With a real interface change impossible here, `watch_addr` triggers by a direct-address change were not exercised. The restart row is a process restart, not an in-place change.

## What could make this wrong

- Power cut is a hypervisor hard reset of a KVM guest (`hcloud server reset`), not mains loss: the guest's virtual disk write cache is whatever the host does with it. A real machine with a volatile disk cache and no flush honoring could lose fsynced data; this test cannot show that, and cannot show data the host had already persisted but a physical disk would not.
- One filesystem (ext4, default options, Ubuntu 24.04, kernel 6.8), one instance type, one location. Other filesystems (xfs, btrfs, tmpfs) and a home directory on a network mount (flock on NFS differs) not tested; tmpfs would lose everything.
- The early cuts are not controlled to the millisecond: the VM stopped 1.05-2.12 s after creation, a window inside ext4's 5 s commit and 30 s dirty expiry; the 10 s and 46 s variants bracket the point where the unsynced file turned durable, the cut point between 2 s and 11 s where it does is not located.
- The test sidecar is created in a directory that was `sync`ed beforehand, with the file never renamed. A real init that writes a temp file and renames, or creates the directory, has different durability steps; the fsync rule above assumes create-in-place.
- After case A the new instance "starts" only because `check`/`hold` creates a key when none exists; whether S14's binary creates or errors on a missing sidecar is its own rule, not measured here. The harm is a changed identity, not a stuck lock.
- The journal is only a probe for unsynced appended data; its loss pattern (about half lost, NUL tail) is what ext4 delayed allocation does, and says nothing about the card or the lock.
- flock child inheritance was tested with `std::process::Command` (O_CLOEXEC); a plain `fork` without exec would let the child keep the lock.
- Kill is not power loss: the kernel closes descriptors on SIGKILL; a hard reset loses the kernel. Flock state is in memory, so after any reboot the lock cannot be held; that is source reasoning (no on-disk lock state), not measured.
- No interface address changed on this Mac: no sudo (`sudo -n true` asked for a password), and I did not toggle Wi-Fi or any interface (it would cut every agent's network). The netmon path (`AF_ROUTE` message to `is_major_change` to rebind and `ip_addrs`) is read in source and not exercised. The first observed `watch_addr` change is a relay move, which moved only after the 20-26 s re-stun, not on the relay's stop.
- Run 2 of the 45 s relay move saw no move; it may have moved after 45 s.
- One process holds relays, member and observers: shared clock, shared scheduler; loopback latency (1-2 ms) is a floor. Relays are plain HTTP, default config, not n0's.
- Observers were bound to 0.0.0.0 and reach M on the LAN address of the same machine; no NAT, no second machine.
- `home_relay_status` items alternate error states two per retry (e1 measured the same); the count 20 in 12 s depends on retry gaps and a dead relay on loopback (connection refused at once). A relay dropping packets would pace differently.
- My first `control` and `netchange` start failed to deliver any Presence because observers were bound to 127.0.0.1 and could not dial M's LAN address; fixed (observers on 0.0.0.0), and control was rerun and is the reported one. The earlier `restart` batch (M' had no bootstrap, so no announcement, O1/O2 got none) was discarded and rerun with the observers as bootstrap.
- `rebind_ms` of about 3 s in the restart scenario is `ep.online()` on a relay; it is not part of the reported Presence delay (measured from rebind return).
- Mac childtest cannot test "child alive" (it reads `/proc`); only the Linux numbers count for it.

## Lab resources created and deleted

Owner ruling relayed by team-lead: cpx22 approved (cx23 unavailable). Labels on all: `owner=resident-anarchist`, `expires=1792669575` (2026-10-22T11:46Z). Backstop `com.ryzhakar.estafette-lab-control` loaded before creation, `lab-control.py` identical to the installed copy.

| Resource | Id | Created (UTC) | Deleted (UTC) |
|---|---|---|---|
| firewall estafette-e23-fw (the skill's four rules) | 11853126 | 2026-10-21 about 11:45 | 12:06:40 |
| server estafette-e23 (cpx22, ubuntu-24.04, nbg1, 178.104.120.183) | 171578487 | 11:46:28 | 12:06:36 |
| primary IP ipv4 | 156857701 | 11:46:28 (labeled by `lab-control.py label`) | auto-deleted with the server |
| primary IP ipv6 | 156857702 | 11:46:28 (labeled likewise) | auto-deleted with the server |

SSH key 131557923 `estafette-lab` reused and kept (estafette-lab uses it). Setup finished (cloud-init `done`, `/var/lib/lab-setup-done`); tools printed: uv 0.13.4, just 1.59.0, rustc 1.99.1, clippy 0.1.99, rust-analyzer 1.99.1, cargo-binstall 1.26.0, Claude Code 2.1.306 (`claude plugin list` not run). After deletion `hcloud server list`, `firewall list`, `primary-ip list`, `ssh-key list` show only the pre-existing estafette-lab server, firewall, two IPs and the key. Both changes written to the journal (`docs/orchestration_log/history/2026-10-15/events.md`, uncommitted append). estafette-lab untouched.
