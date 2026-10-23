# E5 same-key restart

Date 2026-10-16. iroh 1.3.0, iroh-gossip 0.101.0, iroh-relay 1.3.0 (feature `server`, in-process, plain HTTP), tokio 1, rustc 1.99.1 (`rustup run 1.99.1 cargo`). Commit `f82f6ae` on `spike/e5-restart` (pushed; `44e9712` holds the binary used for every run, `f82f6ae` adds `trace.py`, `dupprobe.py`, `dupextra.py`), package `spikes/e5-restart/`, excluded from the workspace (`exclude = ["spikes"]`; `cargo metadata` at the root still resolves). Machine: Apple M2 Max, 12 cores, Darwin 24.3.0 arm64, loopback only. Linux not run.

Method. One binary, separate OS processes. Key file = 32 bytes, 0600. Exclusive `std::fs::File::try_lock` (stable std, no new crate) taken before the endpoint is built, held for process life; retry every 10 ms. QUIC `max_idle_timeout` 10 s on every endpoint. Topic of 3 nodes: K (the node under test) and peers P1, P2; all broadcast one gossip message per 200 ms (`pid:seq`). Each peer holds one connection to K opened before the kill ("held") and every 200 ms sends one request on it, plus one request on a fresh dial `endpoint.connect(K id)` (address from an in-memory lookup, no discovery). A request is a bi-stream ping answered with the responder's pid; cap 5 s. Direct variant: relays disabled, fixed 127.0.0.1 ports. Relay variant: `clear_ip_transports` on all, one relay URL. Kill = SIGKILL (`Child::start_kill`), timestamps are wall-clock ns on one machine, relative to the orchestrator's timestamp taken immediately before the kill (reaped 0.5-2.6 ms later). K' = restarted K, same key file. Restart delay D = 500 ms (all scenarios) plus D = 0 (direct and relay). Runs: 3 per configuration; peer metrics pool P1 and P2 (n counts values, 6 = 3 runs x 2 peers). Raw logs, analysis: `spikes/e5-restart/{analyze,pooled,trace,dupprobe,dupextra}.py`; run with `e5-restart scen --s lock|restart-direct|restart-newport|restart-relay|dup-direct|dup-relay --runs 3 [--delay MS]`.

A first batch of the same runs, without peer-side broadcasting, was discarded and rerun after adding peer broadcasts (to observe the peer-to-K direction). Its direct, new-port and relay results matched the table below (direct: fresh dials answered by K' at ~610-655 ms, 0 gossip neighbor events; new port: 84-85 fresh dial timeouts, 0 successes). The dup-relay window after the K2 kill was 12 s in that batch and 25 s in the reported one.

## Results

Times in ms; "after kill" is relative to the kill of K (scenarios 2-4) unless stated.

### 1. Lock (3 runs)

| Measure | min | median | max | n |
|---|---|---|---|---|
| B (same key file) first `try_lock` while A holds it | fails 3/3: `TryLockError::WouldBlock`, display "lock acquisition failed because the operation would block" | | | 3/3 |
| SIGKILL A -> B acquires (10 ms retry; resolution 10 ms) | 6.5 | 9.2 | 10.4 | 3/3 |
| K' acquires lock after SIGKILL of K (scenarios 2-4, all 15 restarts, first try) | 0.0 | 0.0 | 0.0 | 15/15 |

macOS only. The kernel released the flock before `wait()` on the killed child returned (K' never waited).

### 2. Same key, same address, direct, D = 500 ms

| Measure | min | median | max | n |
|---|---|---|---|---|
| K' READY | 516 | 526 | 537 | 3/3 |
| NeighborDown(K) events at a peer | 0 | 0 | 0 | 6/6 (none in 22 s) |
| NeighborUp(K) events at a peer | 0 | 0 | 0 | 6/6 |
| first broadcast from K' received at a peer | 732 | 810 | 831 | 6/6 |
| first fresh direct request answered by K' (completion) | 621 | 623 | 647 | 6/6 |
| longest fresh request started after kill (dur) | 1011 | 1017 | 1026 | 6/6 |
| fresh requests failed or timed out after kill, per peer-run | 0 | 0 | 0 | 6/6 |
| fresh requests answered by the dead instance after kill | 0 | 0 | 0 | 6/6 |
| held-connection request: first 5 s-cap timeout completes | 5012 | 5022 | 5203 | 6/6 |
| held-connection request: first `connection lost` error | 10013 | 10022 | 10038 | 6/6 |
| held-connection requests answered by K' | 0 | 0 | 0 | 6/6 |
| K' receives first gossip message from a peer | 608 | 618 | 628 | 3/3 |
| NeighborUp events seen by K' itself, per run | 0 | 1 | 1 | 3/3 |

D = 0 (restart immediately after reap), same setup:

| Measure | min | median | max | n |
|---|---|---|---|---|
| K' READY | 13.7 | 14.1 | 34.3 | 3/3 |
| NeighborDown(K) / NeighborUp(K) at a peer | 0 / 0 | 0 / 0 | 0 / 0 | 6/6 |
| first broadcast from K' received at a peer | 217 | 218 | 241 | 6/6 |
| first fresh direct request answered by K' (completion) | 20 | 41 | 209 | 6/6 |
| longest fresh request started after kill (dur) | 10 | 33 | 1007 | 6/6 |
| fresh requests failed or timed out | 0 | 0 | 0 | 6/6 |
| held-connection request: first timeout / first `connection lost` | 5003 / 10002 | 5017 / 10018 | 5023 / 10024 | 6/6 |
| held-connection requests answered by K' | 0 | 0 | 0 | 6/6 |
| K' receives first gossip message from a peer | 104 | 202 | 224 | 3/3 |

### 3. Same key, new port (D = 500 ms; peers keep the old address in their lookup)

| Measure | min | median | max | n |
|---|---|---|---|---|
| K' READY | 516 | 516 | 534 | 3/3 |
| NeighborDown(K) / NeighborUp(K) events at a peer | 0 / 0 | 0 / 0 | 0 / 0 | 6/6 |
| first broadcast from K' received at a peer | 810 | 810 | 830 | 6/6 |
| fresh direct request by id answered by K' | none observed | | | 0/6 |
| fresh direct requests per peer-run that timed out (5 s cap), 22 s window | 85 | 85 | 85 | 6/6 (all of them) |
| held-connection request: first timeout / first `connection lost` | 5008 / 10008 | 5014 / 10015 | 5018 / 10019 | 6/6 |
| K' receives first gossip message from a peer | 616 | 621 | 622 | 3/3 |

### 4. Same key, same address, relay only (D = 500 ms)

| Measure | min | median | max | n |
|---|---|---|---|---|
| K' READY (includes `online()` on the relay) | 721 | 722 | 722 | 3/3 |
| NeighborDown(K) / NeighborUp(K) events at a peer | 0 / 0 | 0 / 0 | 0 / 0 | 6/6 |
| first broadcast from K' received at a peer | 924 | 1012 | 1015 | 6/6 |
| first fresh direct request answered by K' (completion) | 818 | 830 | 832 | 6/6 |
| longest fresh request started after kill (dur) | 1008 | 1009 | 1032 | 6/6 |
| fresh requests failed or timed out | 0 | 0 | 0 | 6/6 |
| fresh requests answered by the dead instance | 0 | 0 | 0 | 6/6 |
| held-connection request: first timeout / first `connection lost` | 5003 / 10005 | 5016 / 10016 | 5021 / 10020 | 6/6 |
| held-connection requests answered by K' | 0 | 0 | 0 | 6/6 |
| K' receives first gossip message from a peer | 813 | 825 | 826 | 3/3 |
| relay DEBUG "multiple connections found" | 0 in 3 runs | | | 3/3 |

Relay variant, D = 0: K' READY 219/240/244; first broadcast at a peer 421/443/447; first fresh request answered 225/447/451; longest fresh request 1007/1019/1027; NeighborDown/Up 0/0 (6/6); held-connection first `connection lost` 10017/10029/10038; no failed fresh request; no "multiple connections found" (relay log).

### 5. Duplicate holder (lock bypassed with `--nolock` on K2; K1 holds the lock; K2 starts 2-3 s after steady state; 15 s dup window, then SIGKILL K2, 25 s window)

Direct (K2 on a different port; peers' lookup still holds K1's port):

| Measure | value | n |
|---|---|---|
| fresh dials by id during dup window answered by K1 / K2 | 450 / 0 (all) | 3 runs x 2 peers x 75 |
| NeighborDown/Up(K) at peers during dup window | 0 / 0 | 6/6 |
| broadcasts from K1 and K2 received at a peer in the window | K1 74-75, K2 74 (both reach peers) | 3 runs, P1 |
| peer gossip messages received by K1 / K2 in the window | K1 0, 0, 2 / K2 148, 148, 150 | 3 runs |
| held connection (to K1) requests | all answered by K1, 0 failures | 3/3 |
| K1: neighbor events or logs | none | 3/3 |
| after K2 kill: peers' NeighborDown(K) | at 10143-10159 ms after the kill (1 per peer) | 6/6 |
| after K2 kill: fresh dials answered by K1, failures | all K1, 0 failures | 3/3 |
| after K2 kill: peer gossip messages received by K1 (25 s) | 0 | 3/3 |
| after K2 kill: K1 broadcasts still received at peers | yes, 123 per peer-run (continuous) | 3/3 |

Relay (both register under one id):

| Measure | value | n |
|---|---|---|
| K1 log (WARN, `iroh::socket::transports::relay::actor`) | "Relay server reports problem: Another endpoint connected with the same endpoint id. No more messages will be received." at K2 READY -0.1 to +0.1 ms | 3/3 |
| relay log (DEBUG, `iroh_relay::server::clients`) | "multiple connections found, deactivating old connection", once per run | 3/3 |
| fresh dials by id in the dup window (counted by completion time), P1 / P2 | K2 224, K1 1, timeout 0 / K2 223, K1 0, timeout 1 (5 s cap, in flight at the registration) | 3 runs each |
| first fresh dial answered by K2, after K2 READY | 6 to 205 ms | 3/3 |
| broadcasts at a peer in the window | K1 49-50 (then stops), K2 74 | 3/3 |
| held connection (opened to K1) requests in the window | 0 answered, every one hit the 5 s cap | 3/3 |
| K1 NeighborDown(P1), NeighborDown(P2) after K2 start | 10002-10173 ms | 3/3 |
| NeighborDown/Up(K) at peers during the dup window | 0 / 0 | 6/6 |
| K2 kill -> first held-connection error at a peer | 105-293 ms | 6/6 |
| K2 kill -> peers' NeighborDown(K) | 10014-10173 ms | 6/6 |
| K2 kill -> first fresh dial answered by K1 | 143-195 ms (probe tick is 200 ms), 0 failed fresh dials | 6/6 |
| K2 kill: K1 NeighborUp, K1 broadcast received at a peer, peer message received by K1 (25 s) | none, none, none | 3/3 |

## Event traces

One representative run per scenario, ms relative to the kill (restart) or to K2 READY (dup). Only first occurrences of repetitive events shown (`trace.py`).

Scenario 2, direct, D = 500 (`restart-direct-d500-2`):
```
   -181.8 P1 PROBE held  first ok oldK
   -178.3 P1 PROBE fresh first ok oldK
      0.0 KILL K (SIGKILL)
      2.6 K reaped
    505.3 K' spawned, same key, same port
    517.9 K' LOCKED waited_ms=0.0
    526.5 K' READY
    532.7 K' NEIGHBOR_UP P2        (K' sees P2 only; no NeighborUp/Down at P1, P2)
    644.5 P1 PROBE fresh first ok newK (started 627.2)
    646.6 P2 PROBE fresh first ok newK
    732.3 P2 RECV first broadcast from K' (from=K)
    732.9 P1 RECV first broadcast from K' (from=P2)
   5021.3 P1 PROBE held first timeout (started 19.7, 5001.6 ms)
  10021.3 P2 PROBE held first err: read_error: connection_lost
  10022.8 P1 PROBE held first err: read_error: connection_lost
```
Scenario 3, new port (`restart-newport-d500-2`):
```
      0.0 KILL K
    504.3 K' spawned, new port
    508.1 K' LOCKED waited_ms=0.0
    522.0 K' NEIGHBOR_UP P2
    810.0 P1 RECV first broadcast from K'
    810.2 P2 RECV first broadcast from K' (from=P1)
   5013.9 P2 PROBE fresh first timeout (started 11.4)   <- every fresh dial to K's id times out, through 22 s
   5013.2 P2 PROBE held  first timeout
  10014.6 P2 PROBE held  first err: connection_lost
```
Scenario 4, relay, D = 500 (`restart-relay-d500-2`):
```
      0.0 KILL K
    504.0 K' spawned
    508.2 K' LOCKED waited_ms=0.0
    727.7 K' NEIGHBOR_UP P2     (READY ~720: online() on relay)
    829.7 P1 PROBE fresh first ok newK (started 824.6)
   1014.6 P1, P2 RECV first broadcast from K'
   5016.6 P1 PROBE held first timeout
  10015.6 P1 PROBE held first err: connection_lost
```
Scenario 5 direct (`dup-direct-d500-1`, t=0 is K2 READY):
```
   -28.7 K2 spawned, lock bypassed
     0.0 K2 READY (own port)
     6.5 K2 NEIGHBOR_UP P2;  7.5 K2 NEIGHBOR_UP P1
   191.9 P1 RECV first K1 broadcast;  202.6 P1 RECV first K2 broadcast
   281.1 K2 RECV first peer message (K1 receives none)
 15018.3 KILL K2
 25161.6 P2 NEIGHBOR_DOWN K;  25169.3 P1 NEIGHBOR_DOWN K     (10143 / 10151 ms after kill)
```
Scenario 5 relay (`dup-relay-d500-1`):
```
     0.0 K2 READY
     0.1 K1 WARN relay: "Another endpoint connected with the same endpoint id. No more messages will be received."
    10.8 K2 NEIGHBOR_UP P2;  11.2 K2 NEIGHBOR_UP P1
   200.9 P2 PROBE fresh first ok newK (K2);  202.6 P1 RECV first K2 broadcast
  5186.9 P2 PROBE held first timeout (the held connection to K1)
 10166.7 K1 NEIGHBOR_DOWN P1;  10172.9 K1 NEIGHBOR_DOWN P2
 15008.0 KILL K2
 15152.6 P1 PROBE held first err: connection_lost;  15301.4 P2 same
 25125.4 P2 NEIGHBOR_DOWN K;  25132.1 P1 NEIGHBOR_DOWN K
 after 15008: fresh dials answered by K1 again (first at ~164 ms); no K1 broadcast reaches a peer in 25 s
```
Relay server log (DEBUG, in-process): `iroh_relay::server::clients: multiple connections found, deactivating old connection remote_endpoint=263e0e5240` (dup-relay run 1).

## The three unknowns of node-as-identity

1. Kernel releases the flock on a killed holder. Answered on macOS: B took the lock 6.5-10.4 ms after SIGKILL of A (retry-bounded), K' took it on its first try in 15 of 15 restarts, and a second holder fails with `WouldBlock`. `std::fs::File::try_lock` compiled on 1.99.1 with no new crate. Linux not run: unanswered there.

2. Peer holds a stale connection to a crashed node id; same key returns. Answered for loopback, SIGKILL, same address, direct and relay, D = 0 and 500 ms: iroh-gossip does not stall. Zero NeighborDown and zero NeighborUp for K at either peer in every run (0 events in 30 peer-runs, over a 22 s window that includes the old connection's 10 s idle death). Gossip from K' reached both peers (217 ms to 1015 ms after kill, dominated by K' start time) and K' received peers' messages (104-826 ms). The stale connection lingers silently: requests on it hang and never reach K'; it errors with `connection_lost` at 10.0-10.04 s (the idle timeout). Source reading predicted this ("no NeighborDown/NeighborUp if the new one lands first"); observed, including the 10 s linger. The prediction does not hold when K' returns on a different port: gossip works (K' dials the peers) but peers' connect-by-id to K never succeeds in 22 s (see Surprises).

3. Which instance answers a dial in the window. Same-key restart (no overlap): no request was answered by the dead instance (0/30 peer-runs); requests started in the outage resolve after up to ~1.0 s (max 1007-1032 ms) and are answered by K'; held-connection requests hang with no answer until the connection dies at 10 s. Overlap (both alive): direct path, the dial goes to the address the dialer holds (K1, 450/450), and the gossip send path of the peers moved to the newer instance (K2 got all of the peers' gossip, K1 got 0-2 of ~150); no events anywhere. Relay path, the newest registration wins (K2 answered 447 of 449 dials counted in the window, K1 answered 1, 1 timed out at the 5 s cap), the older gets `SameEndpointIdConnected` (K1 log, relay log both observed), and when K2 is killed K1 answers dials again within one 200 ms probe tick.

## Surprises

- No gossip NeighborDown at the peers after K died: not at restart, and not when the old connection idled out at 10 s (0 events, 22 s). A peer's gossip membership does not reflect a crashed node that restarted with the same id. Presence cannot rely on NeighborDown for this.
- New port: a peer's dial of K's id by the old address timed out 85/85 times per peer-run (22 s), while gossip was meshed through K''s outbound dials and K' received 210+ peer messages. Inbound traffic from K' did not repair the peer's connect-by-id. Cause not isolated (address book not inspected).
- Requests on a held connection to a SIGKILLed node hang silently for 10 s (idle timeout) with no error; first error `connection_lost` at 10.0-10.04 s. An application request timeout below 10 s is needed to detect a dead node on a held connection; no held request started after the kill was answered, those started in the first ~5 s hit my 5 s cap, later ones errored once the connection died.
- Fresh dials that start during the outage take ~1.0 s (1007-1032 ms) to be answered even when K' is already listening (D = 0, K' READY at 14 ms: longest request 1007 ms). Looks like a fixed dial retry, not isolated.
- Relay duplicate: K1 loses gossip for good in the window I measured (25 s): its neighbors drop at ~10 s (K1 NeighborDown P1,P2), and after K2's death K1 answers dials again (relay re-promotes) but no K1 broadcast reaches a peer and K1 receives nothing, 0 in 25 s.
- Direct duplicate: no signal. K1 (the original) is silently starved of the peers' gossip (0-2 of ~150 messages) while it keeps broadcasting normally and still answers dials; peers see no event.
- `NeighborUp` is not a reliable "connected" signal at K': one direct run showed 0 NeighborUp at K' yet 212 peer messages received.
- K' always took the lock first try (0.0 ms): the kernel released it before the parent saw the child reaped.
- A relay in the restart runs never logged two registrations for one id ("multiple connections found" 0 in 6 runs): SIGKILL closes the TCP connection, so the relay unregistered the old client first.

## What could make this wrong

- Loopback, one machine, macOS arm64 only. Linux flock semantics and UDP-to-closed-port behaviour (ICMP) not tested; the 0.0 ms lock wait and the lack of dead-instance answers may differ.
- SIGKILL closes the process's sockets, which gives the kernel and the relay a close signal (TCP FIN to the relay). A power loss, a dropped NIC or a NAT rebinding sends none; the relay would then hold a stale registration and the "multiple connections" path (old registered, new arrives) is only exercised by the deliberate dup scenario, not by a restart.
- 3 runs per configuration; medians of 3 are weak. Ranges are tight (about 2-30 ms) except D = 0 first-answer times (20-209 ms).
- Probe cadence is 200 ms, so completion times have 200 ms granularity in the dup scenario and the "within one tick" re-promotion bound. The 5 s request cap hides how long a held request would hang beyond 5 s; I inferred the 10 s death from the first `connection_lost` error.
- The dead instance is inferred from reply pid and timeouts. A request delivered to the dead socket and unanswered looks like a timeout: "never answered by the dead instance" is the claim, not "never delivered to it".
- Direct dup: K2 listens on a different port and the peers' lookup only held K1's, so "K1 answers all dials" reflects the address I gave the dialers. With K2's address in the lookup, dials would go to K2. Two processes cannot share one UDP port here.
- New-port finding: peers had only the old address (no discovery, no address push). With mdns or an address update from K', dials may succeed. Window is 22 s; later success not excluded.
- Three-node topic: HyParView with 3 nodes makes everyone a neighbor of everyone; larger swarms (passive views, shuffles) may emit NeighborDown/Up differently.
- Gossip messages from peers were added after a first batch; the batch reported here is the second. Same code otherwise.
- The relay is in-process, plain HTTP, default config; n0's public relays may differ (rate limits, the status handling).
- All endpoints share one idle timeout of 10 s; timings tied to it (10.0 s, K1 neighbor drop at ~10 s) move with it. iroh's default keepalive interval was left unchanged.
- Timestamps compare wall-clock ns across processes on one machine; the kill timestamp precedes SIGKILL by microseconds.
