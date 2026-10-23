# E12 own fix for the gossip stall on relay death

Date 2026-10-20 (runs 21:44-22:17 local, 18:44-19:17 UTC). Rust 1.99.1 (repo `rust-toolchain.toml`), debug build, iroh =1.3.0, iroh-relay =1.3.0 (feature `server`), iroh-gossip =0.101.0 (variant A only), tokio 1, postcard 1, preset `Minimal`. Harness = E10's (E8 scenarios 1 and 7), unchanged except the two levers and extra measures.
Branch `spike/e12-rejoin` (from `spike/e10-gossip-main`), worktree `/Users/ryzhakar/pp/estafette-e12-rejoin`, package `spikes/e10-gossip-main/`. Pushed. Commits: `0c29387` levers and measures (all 19 required runs ran the binary built from this source), `d4b4ca0` runs, analysis, diagnostic tracing switch (`E12_TRACE`, used only by the 2 diagnostic runs; adds `tracing-subscriber` with `env-filter`, same locked version 0.3.23).
Machine: Apple M2 Max, macOS 15.3.1 (Darwin 24.3.0 arm64), all processes on one host over loopback. 1-min load average at cell start: 18.3 (first smoke run, other work on the host), then 3.9-6.8. Per-cell values in `logs/summary/*.txt`.
Run: `cd spikes/e10-gossip-main && ./run-cell.sh a <1|7> <l12|l12np|l1np|l2np> <runs> [run0]`. Recompute: `python3 -I analyze_e12.py logs --table|--runs`. Logs: `logs/raw/a-1l12*.gz`, `a-1l1np*`, `a-1l2np*`, `a-7l12*`; traces `logs/trace/`; diagnostics `logs/diag/`.
Foreground only: each cell was one blocking call; no background processes. Window 88 s after the kill. Definitions as E10 (stalled = pair gap >= 5 s; M2-M3 gap = max of both directions).

## Levers as built (code: `spikes/e10-gossip-main/src/main.rs`)

Both are member-side, env-switched (`E12_L1`, `E12_L2`, `E12_T_MS` default 3000, `E12_L1_DEBOUNCE_MS` default 500). Both armed only after `subscribe_and_join` returned.

**L1 relay removal.** The status task on `ep.home_relay_status().stream()` is the harness's existing `STATUS` source (E8: `connected=false` within 2 ms of the kill). On any status with a relay `!is_connected()`, wait 500 ms and re-read `ep.home_relay_status().get()`. If that relay URL is still disconnected: log `L1_FIRE`, call `Endpoint::remove_relay(&url)` (5 s timeout), log `L1_DONE`. Otherwise log `L1_SKIP`.
Library: `Endpoint::remove_relay` iroh-1.3.0 `src/endpoint.rs:1014-1019` calls `Socket::remove_relay` `src/socket.rs:1245-1252` (removes from the map, sends `ActorMessage::RelayMapChange`). That the change counts as major (`src/socket.rs:725-727`) is E10's reading; I did not re-read it. The measured re-home confirms the effect.
Debounce 500 ms is a guess: longer than a reconnect blip, not tuned.

**L2 rejoin.** `last_hb` holds the unix ms of the last `Hb` received from any peer (set at join). A task wakes every 250 ms. If `now - last_hb >= T` (T = 3000 ms) it calls `GossipSender::join_peers(peers)` with peers = [B1, B2, the other members' ids], 5 s timeout, and logs `REJOIN n=.. silent_ms=..`. Backoff while still silent: first rejoin at T, then 2, 4, 8, 16, 16, ... s between rejoins. Any heartbeat resets the count.
Why `join_peers`: iroh-gossip-0.101.0 `src/api.rs:192-195` sends `Command::JoinPeers`; `net.rs:510` maps it to `ProtoCommand::Join(peers)`; `proto/topic.rs:266-271` turns it into `SwarmIn::RequestJoin(peer)` per peer; `proto/hyparview.rs:322-327` (`handle_join`) emits a `Message::Join` to that peer; `net.rs:682-702` sends it over the active connection, or queues it and dials (`net.rs:695-699`). A fresh `subscribe` needs the old handle dropped, which leaves the topic (`api.rs:207`); `join_peers` keeps the topic state.
T = 3 s: E10 healthy runs had a maximum pair gap of 1.18 s with a 1 s heartbeat, and each member hears two peers, so 3 s is more than two missed heartbeats. Other T not tried.
Trigger limit: "no heartbeat from any peer". A member that still hears one peer is not isolated by this rule (see Surprises).

## Results

Baseline = E10 variant A (same harness, 0.101.0).

| Condition | E10 variant A | E12 |
|---|---|---|
| scn 1, probers on | M2-M3 stalled 5/5 (88 s runs: gap 78.1/79.1/82.5 s, n=3), flowing again 0/5; M1 re-home 41.4/42.0/44.1 s (n=3) | cell 2 (L1+L2, n=5): stalled 5/5, flowing again 0/5, gap 77.9/82.2/84.5 s; M1 re-home 0.704-0.709 s |
| scn 1, probers off | stalled 5/9 (pooled), flowing again 0/5; re-home 41.7/42.6/44.0 s (runs 1-3), 39.0-45.7 s (runs 1-6) | cell 1 (L1+L2, n=5): stalled 3/5, flowing again 0/3, re-home 0.704-0.709 s. Cell 3 (L1, n=3): stalled 2/3, 0/2, re-home 0.709-0.713 s. Cell 4 (L2, n=3): stalled 2/3, 0/2, re-home 39.9/41.5/46.1 s |
| scn 7 control, probers on | stalled 0/3, gap 1.0-1.1 s | cell 5 (L1+L2, n=3): stalled 0/3, gap 1.0-1.1 s |

Per cell (`logs/e12-table.md`; times in s after the kill, min / median / max):

| Cell | runs | M2-M3 stalled (gap >= 5 s) | M2-M3 flowing again at end, of stalled | M2-M3 longest gap s | any HB pair stalled | stalled pairs flowing at end | M1 re-home s | M1 L1 fire s | rejoins/run after kill (min-max) |
|---|---|---|---|---|---|---|---|---|---|
| 1 L1+L2, probers off, scn 1 | 5 | 3/5 | 0/3 | 1.0 / 79.1 / 80.1 | 3/5 | 0/14 | 0.7 / 0.7 / 0.7 | 0.5 / 0.5 / 0.5 | 0-21 |
| 2 L1+L2, probers on, scn 1 | 5 | 5/5 | 0/5 | 77.9 / 82.2 / 84.5 | 5/5 | 0/30 | 0.7 / 0.7 / 0.7 | 0.5 / 0.5 / 0.5 | 21-22 |
| 3 L1 alone, probers off, scn 1 | 3 | 2/3 | 0/2 | 1.0 / 80.1 / 83.9 | 2/3 | 0/10 | 0.7 / 0.7 / 0.7 | 0.5 / 0.5 / 0.5 | 0-0 |
| 4 L2 alone, probers off, scn 1 | 3 | 2/3 | 0/2 | 1.0 / 80.0 / 81.1 | 2/3 | 0/8 | 39.9 / 41.5 / 46.1 | - | 0-7 |
| 5 control scn 7, L1+L2, probers on | 3 | 0/3 | 0/0 | 1.0 / 1.1 / 1.1 | 0/3 | 0/0 | 43.9 (n=1) | - | 0-0 |

Per run (`logs/e12-runs.md`; "pairs stalled" counts the 6 directed M1-M3 pairs; rejoins are L2 `join_peers` calls after the kill, M1/M2/M3):

| Run | M1 L1 fired (s) | M1 re-home (s) | re-home minus L1 (s) | M2-M3 gap (s) | pairs stalled (of 6) | pairs flowing at end | L1 fires (all members) | rejoins after kill M1/M2/M3 | first rejoin (s) | rejoins before kill |
|---|---|---|---|---|---|---|---|---|---|---|
| a-1l12np-1 | 0.5 | 0.7 | 0.2 | 79.1 | 4 | 0 | 1 | 0/0/7 | 21.9 | 0 |
| a-1l12np-2 | 0.5 | 0.7 | 0.2 | 1.0 | 0 | 0 | 1 | 0/0/0 | - | 0 |
| a-1l12np-3 | 0.5 | 0.7 | 0.2 | 79.3 | 4 | 0 | 1 | 0/0/7 | 21.4 | 0 |
| a-1l12np-4 | 0.5 | 0.7 | 0.2 | 1.1 | 0 | 0 | 1 | 0/0/0 | - | 0 |
| a-1l12np-5 | 0.5 | 0.7 | 0.2 | 80.1 | 6 | 0 | 1 | 7/7/7 | 11.0 | 0 |
| a-1l12-1 | 0.5 | 0.7 | 0.2 | 82.2 | 6 | 0 | 1 | 7/8/7 | 8.9 | 0 |
| a-1l12-2 | 0.5 | 0.7 | 0.2 | 84.5 | 6 | 0 | 1 | 7/8/7 | 6.5 | 0 |
| a-1l12-3 | 0.5 | 0.7 | 0.2 | 78.1 | 6 | 0 | 1 | 7/7/7 | 13.1 | 0 |
| a-1l12-4 | 0.5 | 0.7 | 0.2 | 84.1 | 6 | 0 | 1 | 7/8/7 | 7.0 | 0 |
| a-1l12-5 | 0.5 | 0.7 | 0.2 | 77.9 | 6 | 0 | 1 | 7/7/7 | 13.3 | 0 |
| a-1l1np-1 | 0.5 | 0.7 | 0.2 | 80.1 | 4 | 0 | 1 | 0/0/0 | - | 0 |
| a-1l1np-2 | 0.5 | 0.7 | 0.2 | 83.9 | 6 | 0 | 1 | 0/0/0 | - | 0 |
| a-1l1np-3 | 0.5 | 0.7 | 0.2 | 1.0 | 0 | 0 | 1 | 0/0/0 | - | 0 |
| a-1l2np-1 | - | 41.5 | - | 81.1 | 4 | 0 | 0 | 0/0/7 | 19.3 | 0 |
| a-1l2np-2 | - | 46.1 | - | 1.0 | 0 | 0 | 0 | 0/0/0 | - | 0 |
| a-1l2np-3 | - | 39.9 | - | 80.0 | 4 | 0 | 0 | 0/7/0 | 20.7 | 0 |
| a-7l12-1 | - | - | - | 1.1 | 0 | 0 | 0 | 0/0/0 | - | 0 |
| a-7l12-2 | - | 43.9 | - | 1.1 | 0 | 0 | 0 | 0/0/0 | - | 0 |
| a-7l12-3 | - | - | - | 1.0 | 0 | 0 | 0 | 0/0/0 | - | 0 |

Other counts:
- L1 fired in 13/13 scenario-1 runs with L1 (cells 1, 2, 3), only at M1, at 0.503-0.506 s (status `connected=false` at 1-2 ms plus the 500 ms debounce). `remove_relay` returned `Some` in 0 ms. M1 `watch_addr` listed only R2 at 0.704-0.713 s (0.2 s after the fire), and `home_relay_status` showed R2 connected at the same instant. M2 and M3 never fired (R2 alive).
- Re-home without L1: 39.9-46.1 s (cell 4, n=3), 41.4-44.1 s (E10 A, n=3).
- Stalled pairs flowing at window end: 0/62 over cells 1-4 (0/14, 0/30, 0/10, 0/8).
- L2 `join_peers` calls all returned `ok` (the call returns once the command is queued, `ms=0`). Cell 2: 21-22 calls per run (7-8 per member), first at 6.5-13.3 s. Cell 1: 0 calls in the 2 unstalled runs, 7 (M3 only) in 2 runs, 21 in 1 run, first call 11.0-21.9 s. Cell 4: 7 calls in 2 of 3 runs. Calls before the kill: 0 in all 19 runs. Calls in the control: 0.
- Control (cell 5): L1 fires 0, rejoins 0, all 6 pairs healthy in 3/3, max gap 1.1 s. M1 re-homed R1 to R2 by itself at 43.9 s in 1 of 3 runs (no L1 fire, its status stayed connected); E10 saw the same in `b-7-2`.
- NeighborUp events at 55-64 s appear in healthy and stalled runs, with L2 and without, and in the control: the 60 s first shuffle after join (E10 note), not a rejoin effect. None restarted heartbeat flow in a stalled pair.

## Answer

- **L1 works and is fast.** Re-home 0.7 s after the kill against 39.9-46.1 s without it (13 runs against 3 here, 3 in E10 A). It does not end the stall: M2-M3 stalled in 2/3 (L1 alone) and in 3/5 and 5/5 (with L2), the same rate as E10 A (5/9 probers off, 5/5 probers on). It does not restore M1's stalled pairs (0 recovered in 88 s). Costs: the removed relay stays out of the map (re-insert after a relay restart not built or tested); 500 ms debounce; 0 false removals in the control (n=3).
- **L2 does not end the stall.** 0 of 62 stalled pairs flowing again at window end across cells 1-4. Stall rate with L2 alone (2/3) and with L1+L2 (3/5, 5/5) equals E10 A's. Cost: 7-8 `join_peers` calls per member in 88 s while silent (21-22 per run when all pairs stall), 0 in a healthy mesh.
- **Healthy mesh unaffected** (cell 5, n=3): no false removal, no rejoin, no gap above 1.1 s.
- **Why L2 fails here (diagnostic: one probers-on run `a-1l12-101` with `E12_TRACE=iroh_gossip=debug`, `logs/diag/`).** The rejoin reaches the gossip actor (`handle command ... JoinPeers`, then `SendMessage` to B1, B2 and the members, `net.rs:682-702`), and the actor starts dials (`start to dial`, `net.rs:698`). After the kill every dial gossip made at M1, M2 and M3 failed with `dial failed: timed out` (handler `net.rs:435-443`), 10.0 s after it started; 0 successful dials after the kill within the traced 47 s. M3: to M1 and M2 started 7.0 s, failed 17.0 s; to B1 started 13.1 s, failed 23.1 s; to B2 started 37.3 s (after its connection to B2 idle-closed, `NeighborDown` at 36.8 s), failed 47.3 s. M1 and M2 show the same pattern (dial starts 13.1-30.0 s, all fail). The gossip dial is `endpoint.connect(id, alpn)` with no timeout of its own (`net.rs:1026`). A rejoin cannot work while the members' fresh connections fail, which matches E10's fresh-dial data. Short ids in the trace are mapped by NeighborDown timing (M1 `66be`, M2 `0b51`, B1 `8a88`, B2 `8139`), inferred, not read from the harness.

## Surprises

- Gossip dials from M3 to B2 failed although B2 and M3 both sit on the live R2. The layer below gossip is broken for members that did not lose their relay. Cause not isolated (E10 saw it for fresh dials M2-M3 and did not isolate it either).
- Partial stalls: in `a-1l12np-1` and `-3` only M3 was cut off (4 of 6 pairs stalled; M1 and M2 kept exchanging heartbeats). Only M3 rejoined (7 calls); the rule "no heartbeat from any peer" never fired at M1 or M2 for their lost pairs. M3's 7 rejoins did not heal it. In `a-1l12np-5` and every probers-on run all 6 pairs stalled and all three members rejoined.
- L1 re-home came 0.2 s after the fire. By E10's reading (re-home item 5) the 1.5x latency hysteresis applies only while the old relay answers, so a dead home does not hold; consistent with this, not separately tested.

## What could make this wrong

- n is 3-5 per cell and the outcome is bimodal. Stall rates (3/5, 2/3, 2/3 probers off) cannot be told from E10 A's 5/9 at these n. "No effect on stall rate" means no visible effect, not proof of none. The healing claim (0/62 pairs) is stronger than the rate claim.
- Not randomised or interleaved; cells ran in one order on a loaded laptop (load 3.9-18.3 at cell start). The smoke run (`a-1l12np-1`, load 18.3) is kept as run 1 of cell 1. No run discarded. Two diagnostic runs (101) are excluded from the tables.
- L2 as built is one design: `join_peers` to bootstrap plus members, trigger "no heartbeat from any peer", T = 3 s, backoff 2-16 s. A per-peer silence trigger, another T, or a smaller peer set were not tried. A fresh subscribe (new topic state) was not tried; the diagnostic shows the block is the dial, which a fresh subscribe would also need (reading, untested).
- L1 as built never re-adds the relay. E8 scenario 2 (restart in place) not run. The 500 ms debounce is untuned; the control (n=3, one kill) shows no false removal for that case, not under relay flaps.
- Diagnostic is one run, filtered to `iroh_gossip=debug`; no iroh socket or relay tracing, so why the dials time out is open. Short-id mapping inferred.
- `L1_FIRE` time includes the 500 ms debounce.
- Same environment limits as E10: one host, loopback, plain-HTTP relays, SIGKILL closes sockets at once, debug build, QUIC idle 10 s on all endpoints, preset `Minimal`, `E8_PAYLOAD=1`. Gossip main (E10 B heals by itself after 62-145 s) not part of E12.
- The tracing build (`d4b4ca0`) differs from the measured build (`0c29387`) by the `E12_TRACE` switch only. The probers-on diagnostic run stalled like the others (`a-1l12-101`, M2-M3 gap 75.3 s); the probers-off diagnostic run (`a-1l12np-101`) did not stall.
