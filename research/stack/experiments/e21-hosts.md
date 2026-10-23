# E21 self-announcing infrastructure hosts (estafette, S21 with S26)

Date 2026-10-21 (runs 07:52-09:03 UTC). Branch `spike/e21-hosts` (from `spike/e20-delay` 9abb991), commits `215e631` (code, runs, analysis) and `bb66277` (helpers, tables), pushed. macOS arm64, loopback, plain-HTTP relays, debug build.

## Answer

- A host added at runtime works. Host C's signed record reached all 3 members 24-114 ms after C's process start (n=5, plus 3 more runs); each member called `insert_relay` and listed BC with no restart (0 rebinds before the kill in 15/15 members, no heartbeat gap above 1.1 s).
- `insert_relay` forces a full net report, and the new relay became the member's home in 11/15 members (about 0.2 s later), because on a full report iroh 1.3.0 applies no home hysteresis.
- After SIGKILL of host A, 15/15 members rebound, rejoined through BC within 14 ms and re-homed on RC at 0.40 s; all stalled pairs flowing from 5.9-7.0 s, no second rebind. Controls with C static: 12.4-83.1 s in 7/10 runs ending slow or with second rebinds in 4/10; the comparison is confounded by home distribution and stall onset (see "What could make this wrong").
- A new member that knows only BC joined in 9-11 ms and had heartbeats from all three members in 105-216 ms (n=5, A dead 180 s). Unit level: both false records (wrong signer, non-host member) are rejected. A restarted member that loads the saved hosts file joins a network whose only live host is C; the same restart without the file never joins (0 heartbeats in 45 s, 5 JOIN_FAIL).
- The spec has no way to tell a host from a member. I added an operator key and per-host certificate; this is a design change for you to rule on (see "Code added").

## Setup

- Worktree `/Users/ryzhakar/pp/estafette-e21-hosts`, branch `spike/e21-hosts`. Build `spikes/e18-attach-gossip` (stock iroh 1.3.0, vendored iroh-gossip 0.101.0 with the E18 patch): `sh vendor.sh`, `cargo build --features a`, copied to `target/bin/e10-a`. Rust 1.99.1. Same main.rs source as `spikes/e10-gossip-main`. Not committed: `target/`, `vendor/`.
- Binaries (sha256 prefix): `9fc17d01` runs 2100-2114, 2120, 2190; `888c3d2b` run 2121 (adds `E21_NOFILE` in scenario 9); `9f63f28c` runs 2130-2144 (adds `E21_HOMES`, default `111` = unchanged behaviour). Arm and z0w control used the same binary.
- Conditions as E19/E20: SIGKILL of host A (R1 + B1 in one process), probers on, 1 s heartbeat, QUIC idle 10 s, 180 s window, `E12_TRACE=iroh_gossip=debug` (run 2190: also `iroh::socket=debug,iroh::net_report=debug`). Combined workaround on in every cell: `E15_W1=1 E17_PROBE=1 E18_JOIN_T_MS=5000 E18_FIX=1 E20_MJOIN_MS=1500`, plus `E21_HOST=1` (hosts broadcast records every 5 s). Foreground, 1-2 runs per call, `pgrep` "no stray" after every run. Load average (1 min) at run start 0.66-3.27 (`logs/summary/a-8z*`, `a-9z*`), other users present.
- Tags (run-cell.sh): `z1w` arm (members know only A; C starts later), `z0w` control (C up before the members, in their static config, homes R1/R1/R1), `z6w` control homes R1/R2/R2, `z5w` control homes R2/R2/R2, `z3w` H1 only (diagnostic tracing), `z2w`/`z4w` saved hosts (scenario 9; z4w restarts M3 without the file).
- Commands, from `spikes/e10-gossip-main`, `sh e21-batch.sh <scn> <tag> <run0> <n>` (scn 8 = H1+H2+H3, scn 9 = saved hosts), order as run (control first):
  - `8 z0w 2100 1`; `8 z3w 2190 1` (H1 diagnostic); then alternating `8 z1w 2110 1`, `8 z0w 2101 1`, `8 z1w 2111 1`, `8 z0w 2102 1`, `8 z1w 2112 1`, `8 z0w 2103 1`, `8 z1w 2113 1`, `8 z0w 2104 1`, `8 z1w 2114 1` (two runs per call);
  - `9 z2w 2120 1`, `9 z4w 2121 1`;
  - added after the first results (control for the home distribution): `8 z5w 2130 2`, `8 z5w 2132 2`, `8 z5w 2134 1`, `8 z6w 2140 2`, `8 z6w 2142 2`, `8 z6w 2144 1`. Not interleaved with the arm; run later the same afternoon.
- Run times: arm run about 3 min 15 s, control the same; one stack per run.

## iroh 1.3.0 source reading (read, not recalled)

- `Endpoint::insert_relay` (`endpoint.rs:1000-1011`): returns `None` if closed, else `self.inner.insert_relay(relay, config)`. `EndpointInner::insert_relay` (`socket.rs:1232-1243`): `let res = self.relay_map.insert(relay, endpoint); self.actor_sender.send(ActorMessage::RelayMapChange).await.ok(); res`.
- It does trigger a net report, and a full one: `ActorMessage::RelayMapChange => self.handle_relay_map_change()` (`socket.rs:1777-1779`) -> `self.re_stun(UpdateReason::RelayMapChange)` (`1759-1761`); `fn is_major(self) -> bool { matches!(self, Self::LinkChangeMajor | Self::RelayMapChange) }` (`725`); `let mut do_full = is_major || ...` (`net_report.rs:304`), and then `self.reports.last = None; // causes ProbePlan::new below to do a full (initial) plan` (`321`). Measured (run 2190, `iroh::socket=debug`): `starting direct addr update (RelayMapChange)`, `net_report starting do_full=true`, HTTPS probes of both relays, `net_report generated`, `home is now relay http://127.0.0.1:62820/, was Some(http://127.0.0.1:62819/)`, 0.20 s after the insert.
- The map the net report uses is the shared one: `RelayMap` holds `Arc<RwLock<BTreeMap<..>>>` (`iroh-relay-1.3.0/src/relay_map.rs:32`); the handle I kept from `RelayMode::Custom(map.clone())` reports `contains(RC)` true after the insert (15/15, `in_map=true` in `HOSTS_LEARNED`).
- Yes, it can become home, and on a full report with no stickiness. `add_report_history_and_set_preferred_relay` (`net_report.rs:749-818`) takes `prev_relay` from `self.reports.last` (`752`), which `do_full` has just set to `None`, so the hysteresis `if prev_relay.is_some() && r.preferred_relay != prev_relay && ... best_any > old_relay_cur_latency / 3 * 2` (`811-815`) cannot apply; the winner is the relay with the lowest best latency in the last 5 minutes (`800-805`). Run 2190, probe latencies (RA vs RC): M1 635 us vs 808 us stayed on RA; M2 474 vs 337 us moved (337/474 = 0.71, above the 2/3 that hysteresis would need); M3 513 vs 375 us moved. On loopback both relays are about equal, so the outcome is noise. Home change is applied by `on_network_change` (`relay/actor.rs:1232-1257`: `if report.preferred_relay.as_ref() == prev_url { return }` else `set_home_relay`).
- What the relay map is not: it is not needed to send through a relay. `start_active_relay` reads it only for the auth token (`relay/actor.rs:1344-1348`: `self.config.relay_map.get(&url).and_then(|cfg| cfg.auth_token.clone())`). Members can already reach a peer homed on RC from an address that carries RC's URL; the insert makes RC a candidate home and a probed relay.
- Not read: `remove_relay` beyond its twin at `socket.rs:1245-1252` (same RelayMapChange message).

## Code added

`spikes/e10-gossip-main/src/main.rs` +429/-35 (git numstat), `run-cell.sh` +9/-1; scripts `e21-run.sh`, `e21-batch.sh`, `e21-archive.sh`; analysis `analyze_e21_{table,runs,h2,h3,lat,hbafter,b2trace,h1gap}.py`. Break-down by line range: records, certificate, verify, saved file, `Known` type (`main.rs:118-204`, 87 lines); member, session and host edits about 85 lines (the `Msg::Infra` handler 24, host broadcast 20, known-host bookkeeping the rest); controller scenarios 8 and 9 (`1070-1260`, about 190 lines); 5 unit tests (`1351-1416`, 66 lines). Product-relevant total about 170 lines; everything else is harness.

- Record: `InfraRec { host: [u8;32], relay: String, boot: EndpointAddr, ts }` in `Msg::Infra { rec, sig, cert }`. `sig` = host endpoint key over `"e21-rec" || rec`; the sender id is `rec.host` (gossip names no origin: `delivered_from` is the forwarder).
- `verify_infra` accepts only if (1) the operator's certificate over `"e21-cert" || host id` verifies, (2) the host's signature over the record verifies under `rec.host`, (3) the relay URL parses and `boot.id == host`.
- Design decision you need to rule on: a record "from a non-host member" can only be told apart from a host's with some authority. The spec has none (secret holders are all equal, D16). I added an operator key (seed 99 in the harness; members hold its public half, a new network-config field) and a certificate `operator.sign(host id)` that the infra host holds. Alternatives not tried: the operator signs the record itself offline and hosts only rebroadcast it; or trust-on-first-use of the invitation host.
- Member: on a valid record with a new (host, relay) pair, add it to `known`, `lk.add_endpoint_info(rec.boot)`, `ep.insert_relay(url, Arc::new(RelayConfig::from(url)))`, log `HOSTS_LEARNED`, save the file. `known` outlives rebinds: every rebind builds its relay map and its join list from it (this replaced the fixed `u1`,`u2`,`b1`,`b2`; E8 orders `12` and `21` give the old behaviour).
- Host (`boot_node`, `E21_HOST=1`): optional join through host A (`subscribe_and_join` with A's id, A's relay URL in a `MemoryLookup`; own relay map = own relay only); broadcasts its record when joined and every 5 s.
- Saved file (`E21_DIR/known-<member>.txt`): one host per line, `<endpoint id as 64 hex> <relay url>`; here:
  ```
  8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c http://127.0.0.1:55900/
  8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394 http://127.0.0.1:55901/
  ```
  It holds hosts only: ids and relay URLs, no secret, no key. Written on each learned host; loaded at member start when the join order string carries `L`.

## Results

### H1 add a host at runtime (arm z1w n=5; 2190 diagnostic and 2120, 2121 same phase)

| run | C start to RC in map and BC known, every member (ms) | C ready / first record sent (ms) | members whose home moved to RC (ms after learning) | REBIND before the kill | max member-pair heartbeat gap, C start to kill (ms) | failed probes in that span |
|---|---|---|---|---|---|---|
| 2110 | 113 | 21 / 22 | M1 (203) | 0 | 1003 | 0 |
| 2111 | 25 | 22 / 22 | M1 (203), M3 (202) | 0 | 1003 | 0 |
| 2112 | 26 | 22 / 22 | M1, M2, M3 (203) | 0 | 1003 | 0 |
| 2113 | 114 | 22 / 22 | M2, M3 (204) | 0 | 1093 | 0 |
| 2114 | 26 | 22 / 22 | M1, M2, M3 (201-202) | 0 | 1003 | 0 |
| 2190 (z3w) | 24 | 22 / 22 | M2, M3 | 0 | not computed | not computed |
| 2120 (z2w) | 26 | 23 / 23 | M1 (203), M3 (202) | 0 | - | - |
| 2121 (z4w) | 116 | - | M3 (205) | 0 | - | - |

Time is from the controller's timestamp just before it spawns C's process (includes process start, relay bind, the join through BA). `insert_relay` itself returned in 0 ms (`insert_ms=0`, `replaced=false`) in all members. Why 2 of 5 runs took 113 ms and the rest 25 ms: not examined (a repeat broadcast is 5 s away, so a miss of the first record would show 5 s, not seen). Home moved to RC in 11/15 arm members (16/24 over all eight runs with a C start). `logs/e21/lat-2190.txt` has the probe latencies. Table files: `logs/e21/runs.md`, `h1gap.md`.

### H2 kill host A

Per arm (`logs/e21/table.md`, `runs.md`; all stalled pairs flowing from = last end of a stall of any pair, `analyze_e15` definition):

| arm | n | homes at kill (R1 = A, R2 = C) | first rebind after kill (s) | all stalled pairs flowing from (s), per run | median | runs with a second rebind | runs with host C losing 3 member neighbors within 30 s | JOIN_FAIL | pairs flowing at 180 s |
|---|---|---|---|---|---|---|---|---|---|
| arm z1w: C learned at runtime | 5 | R2 on 11/15 members (all 15 started on R1) | 2.8-2.9 | 6.8, 7.0, 6.9, 6.9, 5.9 | 6.9 | 0/5 | 0/5 | 0 | 30/30 |
| control z0w: C static, R1/R1/R1 | 5 | R1 15/15 | 5.1-7.1 | 13.1, 81.7, 78.7, 12.4, 64.5 | 64.5 | 3/5 | 3/5 | 69 | 30/30 |
| control z6w: C static, R1/R2/R2 | 5 | R1 5/15 | 10.0-11.9 | 83.1, 17.2, 18.1, 15.0, 18.3 | 18.1 | 1/5 | 1/5 | 24 | 30/30 |
| control z5w: C static, R2/R2/R2 | 5 | R2 15/15 | no stall | - | - | 0/5 (0 rebinds in any run) | 0/5 | 0 | 30/30 (never stalled) |

Arm detail, first rebind of every member (`logs/e21/h2.md`, 15/15): kill to REBIND 2.8-2.9 s; REBOUND to JOINED 9-14 ms; REBOUND to host C reporting NeighborUp for that member 8-14 ms (the rejoin goes through BC); REBOUND to the member's address listing only RC 402-404 ms; `PROBE_RELAYS down=[RA]` in 15/15. First heartbeat after REBOUND 10-15 ms, except 5 members at 1.0-1.1 s. Each member rebound once; no late join, no JOIN_FAIL.

Tests (exact permutation on the means of all-flowing-from, no multiplicity correction): arm vs z0w p = 0.008 (all 252 splits); arm vs z6w p = 0.008; arm vs the 10 controls pooled p = 0.030. Runs with a stalled host C: arm 0/5 against 4/10 pooled (Fisher p = 0.23), against z0w 3/5 (p = 0.17). All 4 slow control runs (z0w 2101, 2102, 2104; z6w 2140; 64.5-83.1 s) are the runs where host C lost 3 member neighbors after the kill and members' joins to it timed out (5 s each, 7-10 rebinds per member); the 6 fast control runs lost at most 1. In run 2101 host C's trace has no Join from any member from +8 s to +14 s while its connections to M1-M3 time out at +12.95 s (`analyze_e21_b2trace.py logs/e21 a-8z0w-2101 8 14 45`); that fits a stall of C's own transport (C16: C also joined through A and has A's relay in its path set) but I did not instrument it.

Arm members that were re-homed to RC before the kill still stalled at the kill: runs 2112 and 2114 (R2/R2/R2 at kill, 6.9 and 5.9 s) against z5w (R2/R2/R2 from the start: 0/5 stalled): Fisher p = 0.048 on 2/2 vs 0/5. Cause not investigated.

### H3 new member that knows only BC, A dead 180 s

| run | JOINED (ms from join start) | first heartbeat from M1 / M2 / M3 (ms) | from all three (ms) | M4 REBIND / JOIN_FAIL | M4 home |
|---|---|---|---|---|---|
| 2110 | 9 | 104 / 105 / 104 | 105 | 0 / 0 | RC |
| 2111 | 11 | 216 / 216 / 216 | 216 | 0 / 0 | RC |
| 2112 | 9 | 193 / 192 / 193 | 193 | 0 / 0 | RC |
| 2113 | 9 | 212 / 213 / 212 | 213 | 0 / 0 | RC |
| 2114 | 9 | 203 / 203 / 202 | 203 | 0 / 0 | RC |

(`logs/e21/h3.md`.) M4 starts with order `2` (BC id and RC URL only), relay map {RC}. The members were all healthy by then.

### H4 false records (unit, `cargo test --features a e21`, 5 passed)

| test | case | result |
|---|---|---|
| `valid_record_is_accepted` | B2 key, own certificate | accepted, host/relay/ts as sent |
| `record_signed_by_a_key_that_is_not_the_senders_id_is_rejected` | claims host B2 with B2's real certificate, signed by M1's key | `signature does not match host id` |
| `record_from_a_non_host_member_is_rejected` | M1 signs a record about itself: (a) no certificate; (b) a certificate M1 issued itself; (c) B2's certificate reused | `bad cert length`; `not a host: operator certificate invalid` twice |
| `tampered_record_is_rejected` | one bit flipped after signing | rejected |
| `known_hosts_file_round_trips` | save and load | equal |

Not tested: the reject path in a running network (`INFRA_REJECT` event exists, never fired in 23 runs; every record on the wire was valid).

### Q5 saved hosts (n=1 each, scenario 9)

A killed after C is learned; M3's process killed 3 s later and restarted with the same key.

| case | run | LOADED | JOINED | first HB from M1 / M2 (after restart) | heartbeats in 45 s | JOIN_FAIL |
|---|---|---|---|---|---|---|
| M3 restarted, order `1L` (file) | 2120 | 2 hosts | 9 ms | 2892 / 2891 ms | 39 / 39 | 0 |
| M3 restarted, order `1` (no file) | 2121 | - | never | none | 0 | 5 |
| new member M4 (never in the overlay), order `1`, no file | 2120 | - | never | none | 0 | 5 |

The 2.9 s to the first heartbeat is the stall of M1 and M2 healing after A's death (their rebinds), not the join. File contents above.

## Hypotheses and verdicts

- H1 (a host added at runtime reaches every member's relay map and known hosts with no restart): supported, 5/5 runs, 24-114 ms, 0 rebinds, no heartbeat gap above 1.1 s. Side effect measured: `insert_relay` re-homed 11/15 members onto the new relay.
- H2 (members fail over onto the learned host as well as onto a static one): supported for "works" (15/15 re-home on RC, rejoin through BC, flowing at 5.9-7.0 s, no second rebind). Not supported as "faster than static": the comparison is confounded (below).
- H3 (an invitation needs one reachable host): supported, 5/5, with A dead.
- H4 (false records are rejected): supported at unit level (n=1 per case); needs an operator-key design the spec lacks.
- Saved hosts for later sessions: supported, n=1; the file is a prerequisite for the join in this setup (n=1 without it fails).
- Not predicted: `insert_relay` can move the home relay (11/15), and members that were re-homed still stall when the old relay dies (2/2 against z5w 0/5).

## What could make this wrong

- The arm is not a clean twin of any control. All arm members start on RA (only relay) and flip to RC at random (11/15); z0w forces all on RA, z6w is 1+2, z5w never uses RA. The arm's first stall appears 2.8 s after the kill (immediately), controls' 5-12 s; this onset difference accounts for most of the 6-7 s vs 12-18 s gap in the fast runs and I do not know why it differs. Do not read the arm result as "learning a host is faster than a static config".
- The slow control runs track host C stalling after A's death (4/4), but host C stalling is inferred from neighbor-down events and a missing Join in its trace, not from instrumentation (stock build). Whether C stalled less in the arm because fewer datagrams went toward RA, or for another reason, is not shown. Hosts run no W1; a real host that joined through A would be exposed to C16.
- n=5 per arm, arm and z0w interleaved, z5w and z6w run afterwards (not interleaved), different binary hashes (only scenario 9 and a default-off `E21_HOMES` differ). No multiplicity correction; the p-values are post hoc on definitions fixed by E15.
- H1 times are loopback with 3 members and one new host. Records every 5 s were not needed in any run; with a lost first record a member would learn up to 5 s later.
- A valid old record can be replayed by any holder of the secret: `ts` is carried but not checked. A replay can only add a dead relay (not chosen as home when its probe fails); not tested. A rogue operator-less design (everyone with the secret) cannot be told from a host without some operator key.
- `rm.contains` shows the map handle I kept, shared by `Arc`; I did not read the map out of the endpoint (no public accessor).
- Debug build, loopback, plain HTTP, SIGKILL of both relay and bootstrap of one host, one macOS host with other users (load 0.66-3.27), tracing on (`iroh_gossip=debug`), scenario of 3 members. Linux not run.
- H3 members were healthy; a joiner arriving during a stall was not tested. Scenario 9 is n=1 and M3's restart coincides with M1/M2 healing.
- Home flips cost a full net report (HTTPS probes of every relay) per inserted relay per member; with many hosts or relay maps over the internet this is more traffic and a possible home change on each host announcement; not measured beyond loopback.

## Exact commands to rerun on a Linux host

Branch arrives as a git bundle on top of `spike/e20-delay`. In the estafette checkout (already has `spike/e20-delay`):
```
git fetch /path/to/e21.bundle spike/e21-hosts:spike/e21-hosts
git worktree add ../estafette-e21-hosts spike/e21-hosts
cd ../estafette-e21-hosts/spikes/e18-attach-gossip
sh vendor.sh
cargo build --features a
cargo test --features a e21
mkdir -p target/bin && cp target/debug/e18-attach-gossip target/bin/e10-a
cd ../e10-gossip-main
export E21_LOGDIR=logs/e21-linux
unset E17_FRESH_LOOKUP E17_PROBE E18_JOIN_T_MS E18_FIX E20_MJOIN_MS E21_TRACE
```
Runs, foreground, about 3 min 15 s each (scenario 8) or 1 min (scenario 9), at most 3 per call (the tag sets all switches; `pgrep -fl "e10-a|e18-attach"` must print nothing first); run numbers 2900+:
```
sh e21-batch.sh 8 z0w 2900 1            # control first: C static, homes R1/R1/R1
sh e21-batch.sh 8 z1w 2910 1            # arm
sh e21-batch.sh 8 z0w 2901 1 && sh e21-batch.sh 8 z1w 2911 1    # 2 runs in one call
sh e21-batch.sh 8 z0w 2902 1 && sh e21-batch.sh 8 z1w 2912 1
sh e21-batch.sh 8 z0w 2903 1 && sh e21-batch.sh 8 z1w 2913 1
sh e21-batch.sh 8 z0w 2904 1 && sh e21-batch.sh 8 z1w 2914 1
sh e21-batch.sh 8 z6w 2920 2            # control homes R1/R2/R2 (repeat for 2922, 2924)
sh e21-batch.sh 8 z5w 2930 2            # control homes R2/R2/R2 (repeat for 2932, 2934)
sh e21-batch.sh 9 z2w 2940 1            # saved file, M3 restart with the file
sh e21-batch.sh 9 z4w 2941 1            # M3 restart without the file
E21_TRACE="iroh_gossip=debug,iroh::socket=debug,iroh::net_report=debug" sh e21-batch.sh 8 z3w 2990 1   # H1 only with net report tracing
```
Analysis, own logs only (scripts match `a-8z1w-N`, `a-8z0w-N`, `a-8z5w-N`, `a-8z6w-N`, so a fresh dir keeps them off the macOS runs):
```
python3 -I analyze_e21_table.py logs/e21-linux
python3 -I analyze_e21_runs.py logs/e21-linux '^a-8z1w-\d+$'       # also z0w, z6w, z5w
python3 -I analyze_e21_h2.py logs/e21-linux '^a-8z1w-\d+$'
python3 -I analyze_e21_h3.py logs/e21-linux '^a-8z1w-\d+$'
python3 -I analyze_e21_h1gap.py logs/e21-linux
python3 -I analyze_e21_hbafter.py logs/e21-linux a-9z2w-2940 M3 M3_RESTART
python3 -I analyze_e21_lat.py logs/e21-linux a-8z3w-2990
```
(`analyze_e21_h1gap.py` and `analyze_e21_table.py` take the log dir as the only argument; the table script's ARMS regexes name the runs by tag, so no further edit is needed.)

## Files

- Logs: `/Users/ryzhakar/pp/estafette-e21-hosts/spikes/e10-gossip-main/logs/e21` (raw logs, traces, stderr, saved files, tables: `table.md`, `runs.md`, `h2.md`, `h3.md`, `h1gap.md`, `persist.txt`, `lat-2190.txt`); `logs/summary/a-8z*`, `a-9z*`.
