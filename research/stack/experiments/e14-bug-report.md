# iroh: datagrams toward an unreachable relay block all relay sends of the endpoint, including to live relays

Status: internal bug report, written to go upstream (n0-computer/iroh) unchanged. Experiment E14 follow-up, 2026-10-21.
Branch `spike/e14-bug-report` (from `spike/e14-dials`), worktree `/Users/ryzhakar/pp/estafette-e14-dials`, pushed, commit `983118f`. Paths in `spikes/` are relative to that worktree. Source lines are `iroh-1.3.0/src/...`; iroh main (1.4.0, rev below) has byte-identical `src/socket/transports/relay/actor.rs`.

## 1. Summary

An endpoint with a relay map of two or more relays stops sending anything through any relay when a few dozen datagrams are addressed to a relay that is down: one `RelayActor` task serves all relays' outgoing datagrams and waits, without a timeout, on the dead relay's full 64-slot queue, which its connection actor does not drain while it backs off. Every fresh `connect` and every reply to a peer on a live relay then times out, for 70 to 150+ s per blocking cycle. It hits any iroh user whose peers advertise a relay that is down (dead host, partition, restart), and it takes the whole endpoint's relay traffic with it; it is easy to hit and long-lasting.

## 2. Environment

- Rust 1.99.1 (`rust-toolchain.toml`), debug build.
- iroh =1.3.0, iroh-relay =1.3.0 (feature `server`), preset `Minimal`, `clear_ip_transports()` (relay-only), tokio 1, tracing-subscriber 0.3.23. Cargo.lock in `spikes/e14-repro/`.
- macOS 15.3.1 (Darwin 24.3.0 arm64), Apple M2 Max, loopback, plain-HTTP relays in-process.
- Also fails on iroh main: git rev `55908a5809b61c72ec8deeff024038f4a9d68ab7` (iroh 1.4.0), same source file built by `spikes/e14-repro-main` (git dependency, no source change). 20/20 FAIL, controls pass (section 3).
- Linux: the E10/E12/E14 gossip harness reproduces the same mechanism on Ubuntu 24.04.4, Linux 6.8.0-138, 2 vCPU AMD EPYC-Rome, rustc 1.99.1 (lab notes, section 7). The single-file repro on Linux (lab sibling, `research/stack/lab/linux/linux-e14-repro.md`, same commit, debug): stock b200 19/20 FAIL, main 55908a5 18/20 FAIL, b100 3/5 FAIL, b64 5/5 PASS, no-kill 10/10 PASS, b0 10/10 PASS, drain 20/20 PASS, drop 20/20 PASS, patched without a fix 5/5 FAIL. The 6 passing runs out of 48 burst runs had 0 failed dials (the stall never started; the queue did not fill), not a recovery. `--rebind-on-fail`: recovered 5.4 s after the burst in 2 runs (labelled FAIL by the pass rule, which needs zero failed dials), 1 run never stalled. The macOS rebind runs' output was not kept as a log file.
- Found with iroh-gossip 0.101.0 (E8 scenario 1, E10, E12); the repro below has no gossip.

## 3. Minimal reproduction

One file, `spikes/e14-repro/src/main.rs` (181 lines), one package (`e14-repro`), no gossip, two relays and two endpoints (plus a ghost id with no endpoint behind it).

Run: `cd spikes/e14-repro && cargo run --quiet` (exit code 0 pass, 1 fail, 2 setup error; last line `RESULT PASS|FAIL ...`).
On iroh main: `cd spikes/e14-repro-main && cargo run --quiet` (same file).

What it does:
1. Starts relays R1 and R2 in process. Binds X and Y with relay map [R1, R2]; rebinds until both are homed on R2. Y's and X's addresses are in a shared `MemoryLookup`.
2. G is an id with no endpoint, address in the lookup = id + R1 url.
3. Pre-check: X dials Y (fresh `connect`, one echo round trip). Must succeed.
4. Shuts R1 down (`Server::shutdown`). Waits 1 s.
5. Trigger: X starts `--burst N` (default 200) `connect` attempts toward G, each 1 s timeout, all at once. Each sends QUIC Initial datagrams addressed to R1.
6. X dials Y with a fresh connection (3 s timeout) once per second for `--wait-secs` (default 15). PASS if the first dial succeeds, else FAIL. Y is homed on the live R2; the X-to-Y dial never names R1.

Controls built in: `--no-kill` (R1 stays up), `--never-up` (R1 never started, its url is a closed port), `--burst 0`.

Failure rate (macOS, debug, one machine; each run is a fresh process, fresh ports):

| Build | Flags | Result |
|---|---|---|
| iroh 1.3.0 | default (burst 200, wait 15 s) | 20/20 FAIL (`logs/runs-b200.txt`) |
| iroh 1.3.0 | `--burst 200 --no-kill` (R1 alive) | 20/20 PASS |
| iroh 1.3.0 | `--burst 0` (R1 dead, no trigger) | 10/10 PASS |
| iroh 1.3.0 | `--burst 200 --never-up --wait-secs 6` | 5/5 FAIL |
| iroh 1.3.0 | `--burst 64 --wait-secs 5` | 10/10 PASS |
| iroh 1.3.0 | `--burst 100 --wait-secs 5` | 10/10 FAIL |
| iroh 1.3.0 | `--burst 150 --wait-secs 5` | 10/10 FAIL |
| iroh 1.3.0 | single runs 65 / 66 / 67 / 68 / 70 / 75 / 80 | F / P / P / P / P / P / F |
| iroh main 55908a5 | `--burst 200 --wait-secs 8` | 20/20 FAIL (`logs/runs-main-b200.txt`) |
| iroh main 55908a5 | `--burst 200 --no-kill` | 10/10 PASS |

The trigger is a count, not a rate. Below about 65 abandoned dials the queue never fills (the actor is still in its 10 ms-scale early backoffs and drains by the flush path); above about 100 it always fills. 65 to 99 is a race between the burst and the actor's early retry cycles (single runs only).

Failing run, iroh 1.3.0 (`spikes/e14-repro-patched/logs/run-stock-200s.txt`, stock behavior with the diagnostic patch inactive; default build output is the same shape):

```
T +0ms setup done: x_home=[R2] y_home=[R2] r1_up=true
T +6ms pre-kill x->y ok in 6ms
T +6ms R1 shut down
T +1008ms burst of 200 connect attempts toward the ghost (addr id + R1 url)
T +4010ms x->y FAIL (0ms after burst): timeout after 3s
T +8014ms x->y FAIL (4002ms after burst): timeout after 3s
...
RESULT FAIL burst=200 kill=true never_up=false failed_dials=4 first_ok_after_burst_ms=None wait_secs=15
```

## 4. Observed vs expected

Expected: a relay that is down delays or loses only datagrams addressed to it. X to Y, both on the live R2 and neither naming R1, completes in single-digit ms as before the kill (pre-kill 5-6 ms).
Observed (iroh 1.3.0 and main):
- X to Y (and Y to X, whose reply X cannot send) times out. First failed dial starts at once after the burst; all dials fail for the whole window.
- Recovery needs the dead relay's actor to exit. Measured on 1.3.0: burst 200, no further traffic to the dead relay: no X-to-Y success in 150 s (`logs/recovery-1.3.0.txt`), nor on main in 150 s (`logs/recovery-main.txt`), nor in 200 s (patched build, stock behavior): blocked intervals of 71.5 s and 76.8 s, then blocked again (`logs/diag-stock-200s.txt`). Burst 100: first success 140.2 s after the burst (`logs/recovery-b100.txt`). A smaller E14 repro that stopped sending to the dead peer at 25 s recovered at 73.7 s.
- Same mechanism in the real harness (E12 gossip harness, macOS, `a-1-201` trace): fresh dials between two members on the live relay fail from 13 s after the kill, recover at 70.5 s.

## 5. Mechanism

Send path of a datagram addressed to relay URL R:
1. `RelaySender::poll_send` reserves a slot in a 256-slot channel and enqueues (`src/socket/transports/relay.rs:54` creates `mpsc::channel(256)`; `poll_send` at `relay.rs:286-317`). One channel, shared by all relays.
2. One `RelayActor` task consumes that channel (`src/socket/transports/relay/actor.rs:1130-1177`, `run`). For each item it calls `try_send_datagram` (`actor.rs:1206-1232`), which picks the `ActiveRelayActor` for the item's url and `try_send`s to its 64-slot queue (`datagrams_send_queue`, created `actor.rs:1360`: `mpsc::channel(64)`).
3. If that queue is full, `try_send_datagram` returns a future that awaits `sender.send(item)` with no timeout (`actor.rs:1220-1227`). `run` stores it in `datagram_send_fut` (`actor.rs:1137`, set `1168`) and, while it is set, does not poll the channel (`actor.rs:1159`, guard `if datagram_send_fut.is_none()`; the comment at `1135-1136` says so: "We can not process further datagrams during this time").
   So one full per-relay queue stops delivery of every datagram for every relay, including relays that are up.
4. The dead relay's `ActiveRelayActor` does not drain its queue while backing off. Its loop is `run` (`actor.rs:326-355`): `run_once` dials (`run_dialing`, `actor.rs:451-553`), the dial fails at once (connection refused), then `sleep_backoff` (`actor.rs:366-389`, backoff 10 ms to 16 s with jitter: `build_backoff`, `392-400`). `sleep_backoff` selects only on `stop_token`, `prio_inbox` and the sleep. It never reads `relay_datagrams_send`.
5. The only place queued datagrams are dropped while not connected is the 3 s `send_datagram_flush` tick inside `run_dialing` (`actor.rs:460-463`, `514-522`). A dial that fails in under a millisecond leaves `run_dialing` long before the first tick, so the flush never runs against a refused connection. (A dial that hangs on a black-holed address would reach the tick; not tested.)
6. What ends the block: `inactive_timeout` (`RELAY_INACTIVE_CLEANUP_TIME` = 60 s, `actor.rs:68`, reset only at `actor.rs:515` flush and `672` connected writes, otherwise set at construction `284`). It is polled in the `select!` of `run_dialing` (`actor.rs:524-527`), which is entered once per backoff cycle. So the actor exits at the first dial attempt after the 60 s mark: 61 to 80 s after the last reset in the traces (backoff delays reach 19-27 s). On exit the receiver drops, the pending `send` fails, `WARN Dropped datagram(s): ActiveRelayActor closed` is logged (`actor.rs:1216,1224`) and `RelayActor` resumes.
7. Why it comes back: the `RelayActor` then takes the next items of the 256-slot channel, which hold more datagrams for R. `active_relay_handle` creates a new actor (`actor.rs:1322-1337`), its queue fills again, and the block repeats (diag log: UNBLOCKED and BLOCKED within 1 ms, twice). Total time to clear = number of 65-item cycles in the backlog times about 70 s. Traffic that keeps being addressed to R (retransmits, keep-alives, further dials) keeps refilling it.
8. Knock-on effect at the QUIC layer: with `RelayActor` stalled, the 256-slot channel fills and `poll_reserve` in `poll_send` returns Pending, so the endpoint's relay transport stops accepting datagrams too (reading of `relay.rs:286-292`; not separately observed). Established connections to peers on live relays starve and idle out (observed in the gossip harness: heartbeats stop 13 s after the kill, connections close at the 10 s idle timeout).

Why `remove_relay` and lookup removal do not help:
- `Endpoint::remove_relay` calls `Socket::remove_relay` (`src/socket.rs:1245-1252`), which edits the relay map and sends `ActorMessage::RelayMapChange`; that handler only calls `re_stun` (`socket.rs:1759-1762`, `1777-1779`; main: `1248-1255`, `1762`, `1794`). In `actor.rs` the relay map is read in exactly one place, to fetch the auth token when an actor is started (`actor.rs:1346`). Nothing closes the active actor of a removed relay, and `try_send_datagram` routes by the item's own url, not by the map. Measured: no effect (section 8).
- Removing the peer from a `MemoryLookup` does not stop sends: the per-peer remote state already holds the learned addresses (not read in full; measured no effect, 1 run).

Also noted: `Dropped datagram(s): ActiveRelayActor closed` is logged at WARN only when the actor goes away; nothing is logged while `RelayActor` is blocked.

## 6. Causal proof

Method: `spikes/e14-repro-patched`, the same `main.rs`, built against a vendored copy of iroh 1.3.0 (`[patch.crates-io]`, `vendor/` generated by `sh vendor.sh` from the cargo registry plus `e14-iroh-1.3.0.patch`, 78 lines, only `actor.rs`). DIAGNOSTIC; do not merge. Runtime switches: `E14_DIAG=1` logs; `E14_FIX=drain|drop` selects a candidate fix; neither set = stock behavior (20/20 FAIL, same as unpatched).

(a) Observation. `E14_DIAG=1`, burst 200, kill (`spikes/e14-repro-patched/logs/diag-stock.txt`, `diag-stock-200s.txt`):
```
E14DIAG 1792531687962 sleep_backoff url=R1 delay=15.05ms queue_len=51
E14DIAG 1792531687967 try_send_datagram Full url=R1 queue_len=64 max=64
E14DIAG 1792531687967 RelayActor BLOCKED: datagram_send_fut set, no datagram from any relay is taken until it completes
E14DIAG ...989 sleep_backoff url=R1 delay=31ms   queue_len=64
... (every later backoff: queue_len=64, delays 46ms 123ms 290ms 480ms 1.17s 2.5s 3.7s 6.4s 18.6s)
```
200 s run: BLOCKED 1792532947941, UNBLOCKED 1792533019472 (71.5 s), BLOCKED 1 ms later, UNBLOCKED 1792533096223 (76.8 s), BLOCKED again 0 ms later. No UNBLOCKED between. The queue length is 64 at every backoff entry; the blocked state is set once and held. The inferred full queue is now observed. Each UNBLOCKED coincides with the `Inactive, exiting` of the R1 actor and the WARN (relay-level debug trace, `logs/trace-p1-relay.log.gz` in `spikes/e14-dials/logs` shows the same pairing).

(b) Candidate fixes, each 20 runs at `--burst 200 --wait-secs 8`, plus a 2000 burst:

| Switch | Change | 20 runs | burst 2000: kill / never-up / no-kill |
|---|---|---|---|
| none (stock) | none | 20/20 FAIL | not run / not run / PASS |
| `E14_FIX=drain` | `sleep_backoff` also receives from `relay_datagrams_send` and discards (new `select!` branch, `actor.rs` after the `stop_token` branch) | 20/20 PASS | PASS / PASS / PASS |
| `E14_FIX=drop` | in `try_send_datagram`'s Full branch, log and return `None` instead of a waiting future | 20/20 PASS | PASS / PASS / PASS |

Observations: with `drain` at burst 2000, `RelayActor` blocked once for 1 ms (`logs/diag-drain-burst2000.txt`). With `drop` at burst 2000, 1264 datagrams were dropped at the Full branch, 0 BLOCKED.

Trade-offs:
- `drain`: drops datagrams addressed to a relay that is not connected, as the 3 s flush in `run_dialing` already does (the comment at `actor.rs:453-458` accepts that). Smallest change; keeps backpressure for connected relays. Still allows a short block (until the next `sleep_backoff` entry or flush) while the actor is inside a long dial; a hanging dial reaches the 3 s flush, a refused one now drains. Does not stop the shared channel from holding stale datagrams for a dead relay in front of fresh ones. QUIC retransmits; DISCO packets have no retry (same caveat as the existing flush).
- `drop`: structural fix; one slow or dead relay can never stall another. Cost: loses backpressure for a live relay whose queue is full (a slow but connected relay now drops tail datagrams and QUIC sees loss instead of delay, which changes congestion behavior), and a burst toward a healthy relay can lose datagrams that waiting would have delivered. Probably wants a per-relay policy: wait only when the relay is connected, drop when it is not.
- Both: untested beyond this repro (no gossip, no hole-punching, no multi-peer load, macOS only), one runtime path each; neither is a reviewed patch.

## 7. Open discrepancy: Linux versus macOS

The brief: on Linux, 0/6 runs had a fresh dial succeed after 60 s in 88 s windows, macOS 5/8 (`research/stack/lab/linux/linux-dial-after-60s.md`).
What landed (peer-sourced from the lab sibling, not run by me; `research/stack/lab/linux/linux-e14w.md`, 180 s windows, E12 harness, probers on and off, Hetzner VM): the 60 s exit happens on Linux. In the traced run each member's dead-relay actor logged `Inactive, exiting. RELAY_INACTIVE_CLEANUP_TIME=60s` at 61.1 s (M2), 76.5 s (M3), 79.6 s (M1), M1 again at 145.9 s, each with `Dropped datagram(s): ActiveRelayActor closed` when probers were on. Each pair's first fresh-dial success came 0.1 to 2.4 s after the later of its two members' exits (6 pairs, one run). First success after 60 s: 74 to 87 s in 4/4 probers-on runs. The earlier 0/6 came from an 88 s window that ends 1 to 14 s after the release begins.
What it means for the mechanism: the Linux data fit it step for step: exit time 61-80 s (section 5 item 6: the exit waits for the next dial attempt after 60 s; no need for sends to reset the clock, though that is not excluded), recovery within 2.4 s of the exit, second exit at 145.9 s (item 7: backlog refills a new actor). Not a Linux-specific failure. Remaining differences are within the spread of n=3 to 4 per cell.
Not explained by this bug: gossip heartbeats never resumed on Linux in 180 s (0/16 pairs) although fresh dials succeeded from 74 s on. That is a second effect in gossip's dial/neighbor logic after the transport recovers, outside this report and not investigated.

## 8. Impact and workarounds

Impact. Any endpoint configured with 2+ relays, where any peer advertises (in `EndpointAddr`, lookup, or tickets) a relay that is down or unreachable, and where the endpoint addresses 65+ datagrams to it before its connection actor's first long backoff ends. Typical sources: dialing a peer whose home relay died, QUIC retransmits and keep-alives of existing connections to such peers, protocol layers that dial many peers (gossip: 1 dial/s toward a dead peer filled the queue in 17 s; 5/s in 4 s, E14 repro). Effect: all relay traffic of that endpoint stops for 70 s to several minutes, not only traffic to the dead relay. An endpoint with one relay is not affected (nothing else to block). Also reachable with a relay URL that was never up (`--never-up`).

Workarounds measured (E14 and repro; macOS):

| Workaround | Measured effect |
|---|---|
| Stop addressing peers that live on the dead relay before the queue fills (E14 `p1s5`: stop dialing the dead peer 5 s after the kill) | no stall, 2/2 runs. Stopping at 25 s (after the stall began): recovery only at the actor exit, 73.7 s (1 run). Needs the app to learn the relay is dead and to control all traffic toward it; QUIC retransmits of existing connections are not controlled. |
| Close the endpoint and bind a new one with the same key after the first failed dial (`--rebind-on-fail`; also drops the dead address) | X-to-Y dial succeeds 7.4 s after the burst in 3/3 runs (3 s failed dial + close + bind). Drops all connections of the endpoint. |
| `Endpoint::remove_relay(dead)` on the blocked endpoint (E14 `p1rm`) | no effect, 2/2 runs; on both endpoints (`p1rmall`) no effect, 1/1 |
| Remove the dead peer from `MemoryLookup` (E14 `p1a5`) | no effect, 1/1 |
| Wait | 71 to 77 s per 65-datagram cycle; 140 s at burst 100; not recovered in 150-200 s at burst 200 |

Detection hint: a `home_relay_status` stream only reports the home relay; the non-home dead relay appears only in `tracing` output (`Lost connection to relay server`, `retry in ...`). An application cannot see the blocked state through the public API.

## 9. What could make this wrong

- The repro's trigger (abandoned connects toward a ghost) is synthetic. The mechanism is the same in the gossip trace (retry loop, exit after 60 s, WARN, recovery 0.1-2.4 s later), but the queue contents there are retransmits and dials to B1 and M1, not counted.
- Threshold behavior between 65 and 99 is a race I sampled with single runs; the stated 65 is an estimate (queue 64 plus items in flight).
- All runs: one macOS host, debug build, loopback, in-process relays; Linux evidence is the gossip harness, not this repro (n=1 traced run, n=3 to 4 untraced per cell).
- `Server::shutdown` was used to kill R1 in the repro; E8/E10 used SIGKILL of a process. Both give connection refused; the earlier two-process E14 repro matches (`spikes/e14-dials`).
- Item 8 (QUIC-level starvation through the 256-slot channel) is a reading of `poll_send`, not observed with instrumentation.
- "Nothing closes the removed relay's actor" is a reading of the paths I opened in `actor.rs` and `socket.rs`, not a full search. The same applies to the remote-state reading for lookup removal.
- The candidate fixes were tested only in this repro; `drop` in particular may break loaded scenarios; neither is a proposed patch.
- The `Inactive` clock: I did not log its last reset. The 61-80 s exits are explained by "next dial attempt after 60 s"; they are also consistent with extra resets.
- The 20/20 results use a 15 s (1.3.0) and 8 s (main) window; a FAIL means no success inside it, not never. Recovery of burst 200 was not observed up to 200 s.
- Y-to-X failing together with X-to-Y is expected under the mechanism (X cannot send its handshake replies); it is not a separate finding.

## 10. To run on the lab server

```sh
git fetch origin spike/e14-bug-report && git checkout spike/e14-bug-report
cd spikes/e14-repro && cargo build --quiet
for i in $(seq 20); do ./target/debug/e14-repro | grep RESULT; done | cut -d' ' -f2 | sort | uniq -c   # expect 20 FAIL
for i in $(seq 10); do ./target/debug/e14-repro --no-kill | grep RESULT; done | cut -d' ' -f2 | sort | uniq -c  # expect 10 PASS
cd ../e14-repro-main && cargo build --quiet && for i in $(seq 20); do ./target/debug/e14-repro-main --wait-secs 8 | grep RESULT; done | cut -d' ' -f2 | sort | uniq -c
cd ../e14-repro-patched && sh vendor.sh && cargo build --quiet && for f in drain drop; do for i in $(seq 20); do E14_FIX=$f ./target/debug/e14-repro-patched --wait-secs 8 | grep RESULT; done | cut -d' ' -f2 | sort | uniq -c; done
```
Each failing run takes about 17 s (default 15 s wait); each passing run about 3 s.
