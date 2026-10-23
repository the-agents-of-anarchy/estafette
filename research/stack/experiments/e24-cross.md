# E24 cross-machine suite: the relay-death workaround as D46 now states it

Date 2026-10-21 (runs 13:30-15:40 UTC). Branch `spike/e24-cross` (from `spike/e21-hosts` bb66277), worktree `/Users/ryzhakar/pp/estafette-e24`, package `spikes/e24-cross/`, last commit `3f68e41` (pushed). Logs: `spikes/e24-cross/logs/e24/` (Mac address replaced by `A_PUBLIC`). Not filed in the research repo, as asked.

## Answer

- D46 as written does not work with hosts as written. Hosts that apply the deaf rule and the join timeout, with their only other host dead, rebind in a loop (about every 8 s, `JOIN_FAIL` 5 s then rebind) and never form a stable gossip instance; members rebind in step. Relay-only, kill H1, n=8 per arm: control (E19 3 s heartbeat trigger) healed all 12 stalled pairs in 12.8-27.7 s (median 13.0), spec trigger healed in 1 of 8 runs within 120 s (31.3 s, then relapsed), 7 of 8 still had stalled pairs at 120 s; rebinds per member 1.50 (control) against 7.09 (spec), p = 0.001.
- The member trigger alone (T = 15 s) is fine: with hosts that do not apply the deaf rule (n=4) every member rebound once (26.9-27.2 s after the kill), all 12 pairs flowing from 32.6-33.1 s, 0 relapses, 0 join failures. One earlier pilot of the same configuration (n=1, outside the n=4) was slow, 74 s with a 5 s join-timeout loop: host H2 itself was blocked after H1 died (the E21 "C16 on a host" case), so H2-without-W1 is not safe either.
- The open risk exists in a milder form: in the hosts-without-the-rule arm, 16 of 16 blocked members kept receiving H2's 5 s records for 2 more records (about 12 s) after their own sends stopped, which moves the first rebind from 15 s to about 27 s after the kill. No member stayed stalled past 60 s without rebinding in any run (the stalls that outlast 60 s are loops of rebinds, not silence).
- Quiet network: 0 rebinds of members and hosts in 604 s (n=1, IP transports on).
- Runtime host across machines: H2 on C known to all four members 319-379 ms after the process start (n=5); failover after killing H1 did not work cleanly under the spec rule (see test 4).
- Invitation across machines: a fresh member that knows only H2 joined in 290-304 ms and received its first heartbeat 948-1034 ms after start (n=5).
- With IP transports on (as a product runs; direct paths exist, members behind a home NAT), the control never stalled in 4 runs after the kill of H1, while the spec trigger plus hosts-as-written stalled 4 of 4 (first rebind 36.9-42.1 s, all pairs flowing from 94.8-116.7 s): the rule creates the outage.

## Setup

Machines. A: this Mac (Darwin 24.3.0 arm64), home router, public IPv4 behind NAT. B: `estafette-lab`, Hetzner Nuremberg, x86, 178.105.88.14. C: `estafette-e24`, cpx22, Helsinki (hel1), x86, 2.29.112.122, created and deleted for this run. TCP connect to the relays (curl, 5 each): A to B 36-40 ms, A to C 50-53 ms; ICMP is filtered by the lab firewall. Clock offsets of B and C against A were measured at the start of every run (7 `ssh date` samples, smallest round trip): B +82 ms (ssh round trip 77 ms), C +102 ms (113 ms) in the first run; all event times are converted to A's clock with the offset of that run, so cross-machine times carry about +-50 ms.

Software. Rust 1.99.1, release build, iroh =1.3.0, vendored iroh-gossip 0.101.0 with the E18 patch (the on_join fix of D49; `vendor.sh`), preset `Minimal`, `iroh-relay` 1.3.0 binary from E22. Mac binary built on the Mac; the Linux binary built once on C (3 m 18 s), copied to B (same sha256 prefix dfb6117d864c). Cargo.lock copied from E18 so the versions match E19-E22 (a first build without it pulled iroh-relay 1.4.0; rebuilt).

Hosts. H1 on B and H2 on C, each an `iroh-relay` process with a TOML (`cert_mode = "LetsEncrypt"`, hostname `<ip with dashes>.sslip.io`, `enable_quic_addr_discovery = true`; B reused E22's cached certificate, C obtained its own in the first start) and, beside it, one `e24-cross host` process (the bootstrap node): key from seed, relay map = its own relay only, signed infrastructure record broadcast when joined and every 5 s; H2 joins the topic through H1. Members know both hosts in their configuration.

Members. M1, M2 on A, M3 on B (beside H1), M4 on C (beside H2). Per member: the E21 session code rewritten as `e24-cross member` (`spikes/e24-cross/src/main.rs`, no loopback controller): 1 s test heartbeat (measurement), announce on address change, probers (id-only dial of every other member each second, measurement), D46 rebind (20 s minimum interval, relay probe by TCP connect, scrub of the relay URLs found down from the other members' addresses), join timeout 5 s, member join after a rebind (1.5 s, then every 3 s, `join_peers` of the other members), host records learned at runtime with `insert_relay`.

Trigger. `E24_TRIG=spec` (default): the clock is reset by every received gossip payload except the test heartbeat (host records and member announces count), T = 15 s, a member always "knows another node" (a host). `E24_TRIG=hb` (control, as E19): only a received test heartbeat resets it, T = 3 s. The test heartbeat never resets the spec clock.

Host rule. `E24_HOST_DEAF=1`: the host applies the same deaf rule (T = 15 s, counts every non-heartbeat payload from anyone, counts as knowing another host when it was given a via host or has received another host's record; 20 s minimum interval) and the 5 s join timeout when it has a via host; a rebind builds a new endpoint with the same key and joins again through the via host. D46 text read: "The bootstrap node of a host applies the same deaf rule and rebind, counting only other hosts as other nodes", and section 4 "with none [other host], it subscribes alone, is its own bootstrap, and applies no join timeout", so a host with a (dead) other host keeps the join timeout. I counted member announces too (spec says only other hosts count; counting fewer payloads would only make hosts rebind more).

Kill. `kill -9` of the relay and the bootstrap process of H1 in one ssh call on B (clock read before and after; the first value is t0). Window 120 s after the kill, settle 40 s before it (mesh check by the data, below).

Conditions per arm.

| Arm | Transports | Trigger | Hosts | n | Tags |
|---|---|---|---|---|---|
| control | relay only (`E24_RELAYONLY`, as E19-E21) | hb, 3 s | deaf rule on | 8 | `k-hb-1..8` |
| spec | relay only | spec, 15 s | deaf rule on (spec as written) | 8 | `k-spec-1..8` |
| spec, hosts without the rule | relay only | spec, 15 s | rule off | 4 | `k-spec-nohd-1..4` |
| ip control | IP transports on | hb, 3 s | deaf rule on | 4 | `ip-hb-1..4` |
| ip spec | IP transports on | spec, 15 s | deaf rule on | 4 | `ip-spec-1..4` |

Order. Control first, then control and spec interleaved in pairs (`batch.sh`), then the 4 nohd runs, then the 8 ip runs (interleaved in pairs), then tests 3-5. Controlled by one script, one afternoon. Pilots `pilot1` (IP on, spec, hosts without the rule, n=1: no stall), `pilot2` (relay-only, spec, hosts without the rule: the slow 74 s run), `pilot3` (relay-only, spec, hosts with the rule) are outside the counts except where named.

Commands. From `spikes/e24-cross`: `python3 -I e24run.py kill <tag> <hb|spec> 120 [E24_RELAYONLY=1] [E24_HOST_DEAF=1]`, `quiet <tag> 600 E24_HOST_DEAF=1`, `runtime <tag> E24_HOST_DEAF=1`, `invite <tag> 5 E24_HOST_DEAF=1`; the whole batch is `sh batch.sh`. Analysis: `python3 -I summarize_e24.py logs/e24 k-hb k-spec` (also `k-spec-nohd`, `ip-hb ip-spec`), `python3 -I rtinv.py logs/e24`, `python3 -I analyze_e24.py logs/e24 quiet-1`.

Definitions (E15). Stalled pair: a directed test-heartbeat pair (receiver, sender) with a gap of 5 s or more after the kill, anchored at the last heartbeat before it. All-flowing-from: the end of the last such gap over all stalled pairs (a pair silent at the window end makes it undefined, "never"). Flowing at end: a heartbeat in the last 5 s. Relapse: a member rebinding a second time. 4 members, 12 pairs.

## Results

### Test 1 and 2: kill H1 (n per arm below)

| Arm | n | runs with a stall | first rebind per member, s min / med / max | all 12 pairs flowing from, s per run | pairs flowing at the window end, per run | rebinds per run (4 members) | runs with a relapse | JOIN_FAIL per run | rebinds before the kill |
|---|---|---|---|---|---|---|---|---|---|
| control relay-only | 8 | 8 | 5.9 / 7.0 / 8.3 | 13.1, 12.9, 27.7, 26.6, 13.0, 13.0, 12.9, 12.8 | 12 in all 8 | 4, 4, 12, 12, 4, 4, 4, 4 | 2 (runs 3, 4) | 0, 0, 8, 8, 0, 0, 0, 0 | 0 |
| spec relay-only | 8 | 8 | 18.4 / 27.0 / 27.2 | 31.3, never x7 | 12, 4, 0, 4, 0, 0, 0, 0 | 15, 12, 52, 12, 32, 28, 38, 38 | 8 | 0, 0, 48, 0, 20, 16, 30, 30 | 0 |
| spec, hosts without the rule | 4 | 4 | 26.9 / 27.0 / 27.2 | 32.6, 32.6, 33.1, 33.1 | 12 in all 4 | 4, 4, 4, 4 | 0 | 0 in all | 0 |
| ip control | 4 | 0 | - | - | 12 in all 4 | 0 | 0 | 0 | 0 |
| ip spec | 4 | 4 | 36.9 / 42.0 / 42.1 | 94.8, 101.6, 110.3, 116.7 | 12 in all 4 | 30, 23, 12, 12 | 4 | 24, 15, 0, 0 | 0 |

Tests (exact permutation on the means, runs as units; a run never flowing counted as 120 s, a run with no stall as 0 s): control against spec, all-flowing-from p < 0.001 (printed 0.000 of 12870 splits); rebinds per run p = 0.001. ip control against ip spec: p = 0.029 on both measures (the smallest value possible with 4 against 4). Rebinds per member: control mean 1.50 (max 3), spec 7.09 (max 13), nohd 1.00, ip spec 4.81 (max 8).

Host rebinds after the kill (hosts with the rule, per run): control 12, 13, 12, 12, 13, 12, 13, 13; spec 8, 9, 14, 8, 10, 10, 12, 12; ip control 13, 12, 12, 13; ip spec 11, 9, 7, 8. H2 rebinds in every run of every arm that has the rule, also in the control and in the ip control, where the members are unaffected because the 3 s heartbeat trigger never starves.

Mechanism, from the event lines of `k-spec-3` (A clock, seconds after the kill): 10.0 M1 loses H1; 18.9 H2 deaf (silent 15.2 s) and rebinds; 22.1 M1 deaf (15.1 s) and rebinds; 23.9 H2 `JOIN_FAIL` (5 s) and rebinds; 25.2 M1 probes the relays (H1's down); 30.2 M1 `JOIN_FAIL` and rebinds; 31.9 H2 `JOIN_FAIL`; then H2 every 8.0 s and M1 every 8.1 s until the window ends, 12 pairs stalled throughout. The cause is the loop on both sides: the host's join through the dead host and the member's join through hosts of which one is dead and the other is mid-rebind each take 5 s and end in a rebind.

Test 2, relay-only, per member that had an outgoing stall (heartbeats from it missing at a peer for 5 s or more):

| Arm | members stalled / members | records from H2 received between the kill and the first rebind (min / med / max) | records received after its stall began | longest gap between records, s | stalled past 60 s without rebinding |
|---|---|---|---|---|---|
| control | 32 / 32 | 1 / 2 / 2 | 0 / 1 / 1 | 4.0 / 5.0 / 5.4 | 0 |
| spec | 32 / 32 | 1 / 3 / 3 | 0 / 2 / 2 | 15.0 / 15.1 / 16.7 | 0 |
| spec, hosts without the rule | 16 / 16 | 3 / 3 / 3 | 2 / 2 / 2 | 15.0 / 15.1 / 15.2 | 0 |
| ip spec | 16 / 16 | 5 / 6 / 6 | 0 / 0 / 0 | 15.0 / 15.1 / 15.2 | 0 |

Reading: in the nohd arm every blocked member heard 2 more records after its own sends were blocked, until about 12 s after the kill, when the host's connections to members time out (QUIC idle 10 s); the member's 15 s then runs from there, first rebind about 27 s. The risk "a blocked member keeps hearing records and never goes deaf" did not happen; it delays the rule by about 12 s. In `ip spec` the 5-6 records came before the stall began (the members were unaffected until H2 started flapping), the stall began after.

### Test 3: quiet network, 10 minutes (n=1)

IP transports on, both hosts alive and applying the deaf rule, test heartbeats not counted: 0 member rebinds, 0 host rebinds, 0 join failures in 604 s (`quiet-1`). H1 and H2 each sent 122 records; M1 received 241. Each member's counted payloads were the 5 s records of two hosts and the announces.

### Test 4: runtime host across machines (n=5, IP transports on, hosts with the rule)

H1 only, four members know only H1; H2 started on C after a 40 s settle (relay first, then the host process joining through H1); 20 s later H1 killed.

| run | H2 known to M4 / M3 / M1 / M2 (ms after the H2 process start) | H2 ready, first record sent (ms) | after the kill: first rebind of the members, s | stalled pairs | all 12 flowing from, s | flowing at 90 s |
|---|---|---|---|---|---|---|
| rt-1 | 322 / 354 / 367 / 367 | 296 | 21.1 (M2), 51.3-51.5 | 12 | never | 0 / 12 |
| rt-2 | 320 / 330 / 371 / 371 | 294 | 42.6-42.7 | 6 | 48.4 | 12 |
| rt-3 | 330 / 337 / 379 / 379 | 304 | 22.4-23.6 | 10 | 90.9 | 12 |
| rt-4 | 322 / 332 / 371 / 371 | 297 | 22.9-24.4 | 7 | 63.2 | 12 |
| rt-5 | 319 / 326 / 364 / 364 | 292 | 35.1-35.3 | 6 | 75.5 | 12 |

Learning: all four members called `insert_relay` for H2's HTTPS relay 319-379 ms after the process start, 25-76 ms after the host's first record (offset error about +-50 ms, so read the gaps as tens of ms). A's members (M1, M2) learned it 364-379 ms. Failover: the members did not fail over cleanly: every run stalled 6-12 pairs, first rebinds 21-51 s after the kill, and every member rebound two to four times; run 1 never recovered within 90 s (0 of 12 flowing). Cause is the one of test 1: H2, whose only other host died, rebinds in a loop. No JOIN_FAIL in runs 2-5, 2 in run 1.

### Test 5: invitation across machines (n=5, IP transports on, hosts with the rule)

Fresh member (new key, on A) given only H2's relay URL and id, hosts and four members running.

| run | JOINED (ms from join start) | first heartbeat received from any member (ms) | heartbeats from all four members (ms) | its first heartbeat seen by M1 (ms) | rebinds, JOIN_FAIL |
|---|---|---|---|---|---|
| N1 | 300 | 948 | 1092 | 449 | 0, 0 |
| N2 | 304 | 971 | 1121 | 368 | 0, 0 |
| N3 | 290 | 993 | 1144 | 352 | 0, 0 |
| N4 | 301 | 1011 | 1172 | 365 | 0, 0 |
| N5 | 291 | 1034 | 1212 | 353 | 0, 0 |

Time to first received message is bounded below by the 1 s test heartbeat period (a message arrives at the next beat); the join itself took 290-304 ms, M1 saw the fresh member's first beat 352-449 ms after the join started.

## What could make this wrong

- The main arms run relay-only, the condition of E19-E21, because with IP transports the control never stalls (0 of 4) and there is nothing for the rule to repair. A product with working direct paths meets the rule in the `ip` arms (n=4 each), where the spec arm stalls because of the hosts' flapping, not because of a relay death. Which of the two a user meets depends on whether direct paths exist; symmetric NAT, UDP-blocked networks and CGNAT were not tested. E22's NAT was port-preserving and traversable.
- The host reading is mine. D46 says hosts "apply the same deaf rule and rebind, counting only other hosts"; section 4 says a host with no other host applies no join timeout. A host that knows a dead host keeps the join timeout in my reading. A different reading (hosts never rebind, or no join timeout for hosts) is the `nohd` arm (n=4) and gives a healthy result; I did not run "rule on, join timeout off" separately, so I do not know which half of the host rule makes the loop. My hosts also count member announces as payloads (the spec counts only other hosts), a difference that cannot reduce the flapping.
- `nohd` (n=4) was not interleaved with the other arms (run after them, same afternoon, same binary, same hosts); one more run of that configuration (`pilot2`, 100 s window, outside the n) was slow: 74 s to heal with a 8 s join-failure loop of all four members (H2 blocked after H1's death, the E21 C16-on-a-host finding). So hosts without the rule healed in 4 of 5 runs, slowly in 1.
- n=8 per arm for the main comparison, n=4 for the others, n=5 for tests 4 and 5, n=1 for test 3. The p-values are post hoc permutation tests on runs; no multiplicity correction. The 7 of 8 "never healed" spec runs are bounded by a 120 s window: a longer window could show healing (the loops had not ended at 120 s in any run).
- Clocks: offsets measured through ssh with a 77-113 ms round trip, so cross-machine times (all-flowing-from, test 4 learning times) carry about +-50 ms; per-process intervals (join durations, silence, first-heartbeat in test 5) use one clock. Test 4's 319-379 ms learning times compare B or C clocks to A's and are the least certain.
- Control hosts also rebind after the kill (12-13 per run), so the control is "3 s trigger with flapping hosts", not "3 s trigger with calm hosts"; the E19 control had no hosts of this kind. Control runs 3 and 4 relapsed (27 s, 12 rebinds, 8 `JOIN_FAIL`) probably from the same loop; not examined.
- M3 runs on B beside H1 and M4 on C beside H2; a member on its own host's machine is hit by the kill differently (same kernel, no network) from members elsewhere; M1/M2 share one NAT address.
- The trigger counts announces: my announce-on-address-change fires several times at start and on each rebind; in a quiet network (test 3) it adds a few resets in the first seconds only. Real members send other payloads (messages, presence) that this harness does not, which would reset the clock more often than here; a busy network would not go deaf, a quiet one would, as the spec says ("a network with no live host gives no such floor").
- Release build, one Mac (other users present, load not recorded), tracing off, 3 s relay probe is a TCP connect (as E17 C2), QUIC idle 10 s in the harness (a product default would differ and moves the 12 s record tail of test 2). Both relays carry QUIC address discovery, UDP 7842.
- sslip.io certificates for two IPs; Let's Encrypt production, one certificate each. The cert for C was issued in the first start (about 10 s) before the pilots.
- I changed `e24run.py` after the batch to give subprocesses a closed stdin (ssh swallowed the keyboard line in the owner script); the batch ran with the earlier version, with the same behavior otherwise.

## Owner-assisted tests: how to start them and what the owner does

Script `spikes/e24-cross/owner-tests.sh` (steps in its first lines; Python in `e24owner.py`; analysis `analyze_e24owner.py`). Dry run done on the Mac up to and through the owner step with the wake and the network switch replaced by nothing (`dry-wifi`, `dry-sleep` with `E24_NOSLEEP=1`): the script starts the hosts and members on B and C, prints READY after 40 s, accepts Enter, waits 150 s, collects the logs; the analysis ran on both. Not tested: a real sleep, a real network change.

Server C was deleted after this run (its address was 2.29.112.122); to run the tests, create it again with the run-hetzner-lab skill (name `estafette-e24`, cpx22, same setup in `deploy/`: `build-c.sh`, `deploy-c.sh`, `relay-c.toml` with the new address' sslip.io name) and set `E24_C` before the start. The hosts need the Mac binary at `./target/release/e24-cross` (`sh vendor.sh && cargo build --release`).

Start (from `/Users/ryzhakar/pp/estafette-e24/spikes/e24-cross`): `sh owner-tests.sh sleep` or `sh owner-tests.sh wifi`; read the result with `python3 -I analyze_e24owner.py logs/e24/<the directory it prints>`. The analysis prints the first octet of the public address only; `ip.txt` holds the full address, so replace it before committing logs.

Owner steps, (a) sleep and wake:
1. Run `sh owner-tests.sh sleep` in a terminal on the Mac and wait about 1 minute for the line READY.
2. Press Enter. The Mac goes to sleep 5 seconds later by itself.
3. Leave the Mac asleep for 2 minutes.
4. Wake the Mac by pressing a key or opening the lid.
5. Wait 2.5 minutes without touching anything; the line Done ends the test.

Owner steps, (b) Wi-Fi switch to a phone hotspot:
1. Switch on the phone's personal hotspot and keep the phone next to the Mac.
2. Run `sh owner-tests.sh wifi` in a terminal on the Mac and wait about 1 minute for the line READY.
3. Switch the Mac from the home Wi-Fi to the phone hotspot and wait until the Mac shows it is connected.
4. Press Enter in the terminal.
5. Wait 2.5 minutes on the hotspot; the line Done ends the test. Then switch back to the home Wi-Fi.

What it measures: for (a), the tick gap (sleep length), rebinds of M1 and M2 after the wake and when, each pair involving them against the wake instant (first heartbeat after it, gap), all pairs resumed by; for (b), the instant the public address changes (poll of ifconfig.me every 2 s), the first rebinds of M1, M2, `ADDR` events, and for M3 and M4 (on B and C) the first heartbeat from M1 and M2 after the change; whether the pairs flow at the end. The members use IP transports (default), so a recovery over the relay is not told from a direct path in these logs; only the heartbeat timing is.

## Lab resources created and deleted

- Created 2026-10-21 13:29Z, per the run-hetzner-lab skill (backstop job `com.ryzhakar.estafette-lab-control` loaded and logging each hour before creation; every resource labelled `owner=resident-anarchist`, `expires=1792675724`): firewall `estafette-e24-fw` (id 11854113, from `firewall-rules.json`, 4 rules), server `estafette-e24` (id 171601176, cpx22, hel1, ubuntu-24.04, ssh key `estafette-lab`, setup from `fill-setup.py`), its primary IPs 156876685 (IPv4 2.29.112.122) and 156876686 (IPv6), labelled by `lab-control.py label primary-ip`.
- Deleted 2026-10-21 15:50Z in the skill's order: server, then firewall; the primary IPs went with the server (auto delete). Confirmed by `hcloud server list`, `firewall list`, `primary-ip list`, `ssh-key list`: only `estafette-lab`, `estafette-lab-fw`, the two primary IPs of `estafette-lab` and the key `estafette-lab` (still in use by that server) remain. No Hetzner token was read or printed.
- Journal: the memento journal of this worktree has no entry for today; the creation and deletion lines are in `/tmp/e24/lab-changes.txt` (not committed) for the filing session to write by `event-capture`.
- On B: `/root/e24/` (e24-cross, iroh-relay, relay.toml, certs copy, ctl.sh, logs; about 30 MB) left in place; all E24 processes stopped (`pgrep` none). `/home/lab/work`, its tmux and its processes were not touched. A: no `e24-cross` process left.
