# E4 presence flow

2026-10-16. iroh 1.3.0, iroh-gossip 0.101.0, rustc 1.99.1. Commit 6bd29ac on `spike/e4-presence` (pushed). Apple M2 Max, Darwin 24.3.0 arm64, loopback, relays disabled, QUIC idle 10 s, keep-alive 5 s. Package `spikes/e4-presence/`, excluded from the workspace (`exclude = ["spikes"]`). Run: `e4-presence driver all --reps 3` and `... --init-delay 0 --reps 3`. Raw tables: /tmp/e4-main.out, /tmp/e4-init0.out (not committed).

## What was built

1080 lines (node.rs 516, driver.rs 552, main.rs 12). One binary: `node` (a member, controlled over stdin, events on stdout with microsecond wall-clock stamps) and `driver` (spawns each node as its own OS process, SIGKILL via `start_kill`, derives every metric from the event log).

Mechanics:
- Wire = `{nonce (unsigned), body (JSON bytes), sig}`; body signed with the node's iroh SecretKey; node key is the member identity. No MAC, card, or MCP layer.
- Payloads: Presence{node, seq}, Departure{node, seq}, Unreachable{subject, reporter, ts}. Gossip topic, default config (4096 max message).
- Direct ALPN: Probe (3 s timeout), Introduce -> IntroduceAck (carries a fresh Presence).
- 3.8 rows implemented: Presence unknown -> insert Online + Introduce; NeighborUp -> Introduce unless Online; Departure -> Gone; Unreachable -> Unreachable unless contact newer than ts; Unreachable about self -> re-announce; NeighborDown -> Probe, on failure Unreachable + broadcast; direct contact -> Online.
- S2: idle timeout 10 s. S12: seq = max(now_ms, last+1); keep highest, drop `seq <= held`; drop if older than max age (300 s; 3 s in 6b); Departure tombstones the seq, also for an unknown member. `--no-s12` disables all three (control).
- S13: address-change watcher calls announce. Relay-status trigger not built (relays disabled).

Deviations from the spec: handle assignment, `attached`, observer-wait handle logic, MACs, cards omitted. Direct contact sets Online only from Unreachable, never from Gone. "Presence for own card from another node" not built. Addresses are injected into a MemoryLookup for every node (needed for Introduce dials; gossip bootstrap still follows the per-node topology). Topology: n0 none; n1->n0; n2(C)->n1 only; n3->n0; n4->n2.

## Results

Main run: init Presence sent 3000 ms after START (the spec's observer wait). n=3 each, times in ms from the triggering command. "n (missing)" = samples with no value.

| Scenario | Measure | min | median | max | n (missing) |
|---|---|---|---|---|---|
| 1 five join | all five tables full | 3005 | 3005 | 3013 | 3 (0) |
| 1 | A learns C (test 19 shape) | 25 | 3004 | 3004 | 3 (0) |
| 1 | C learns A | 27 | 3004 | 3004 | 3 (0) |
| 1 | Introduce sent, 5 nodes | 30 | 30 | 32 | 3 (0) |
| 1 | IntroduceAck sent, 5 nodes | 30 | 30 | 32 | 3 (0) |
| 1 | Presence broadcast, 5 nodes | 5 | 5 | 5 | 3 (0) |
| 2 late joiner | joiner holds all five | 3098 | 3188 | 3207 | 3 (0) |
| 2 | all five hold joiner | 3093 | 3182 | 3199 | 3 (0) |
| 2 | first peer holds joiner | 11 | 11 | 12 | 3 (0) |
| 2 | Introduce sent (delta, all nodes) | 12 | 12 | 14 | 3 (0) |
| 2 | IntroduceAck sent (delta) | 12 | 12 | 14 | 3 (0) |
| 2 | Presence broadcast (delta) | 1 | 1 | 1 | 3 (0) |
| 2 | joiner's own Introduce sent | 6 | 6 | 7 | 3 (0) |
| 3 clean departure, shutdown only | exit after DEPART | 1 | 1 | 3 | 3 (0) |
| 3 | first peer Gone | 1 | 1 | 4 | 3 (0) |
| 3 | last peer Gone | 182 | 183 | 186 | 3 (0) |
| 3 | peers (of 4) with Departure before exit | 0 | 0 | 2 | 3 (0) |
| 3 | peers (of 4) that ever got it | 4 | 4 | 4 | 3 (0) |
| 3 | peers ever marking it Unreachable | 0 | 0 | 0 | 3 (0) |
| 3-sleep50ms | exit after DEPART | 53 | 54 | 59 | 3 (0) |
| 3-sleep50ms | peers (of 4) that ever got Departure | 0 | 0 | 4 | 3 (0) |
| 3-sleep50ms | peers (of 4) ending not Gone | 0 | 4 | 4 | 3 (0) |
| 3-sleep50ms | first / last peer Gone (one run only) | 3 / 136 | 3 / 136 | 3 / 136 | 1 (2) |
| 3-sleep1000ms | exit after DEPART | 1004 | 1004 | 1005 | 3 (0) |
| 3-sleep1000ms | peers (of 4) with Departure before exit | 4 | 4 | 4 | 3 (0) |
| 3-sleep1000ms | first peer Gone | 1 | 94 | 96 | 3 (0) |
| 3-sleep1000ms | last peer Gone | 92 | 96 | 196 | 3 (0) |
| 4 SIGKILL | NeighborDown, first observer | 14981 | 14988 | 14994 | 3 (0) |
| 4 | NeighborDown, last observer | 14995 | 14997 | 15001 | 3 (0) |
| 4 | observers of NeighborDown (of 4 peers) | 2 | 2 | 2 | 3 (0) |
| 4 | probes failed / succeeded | 2 / 0 | 2 / 0 | 2 / 0 | 3 (0) |
| 4 | probe duration | 3002 | 3003 | 3003 | 3 (0) |
| 4 | Unreachable gossip sent (count) | 1 | 2 | 2 | 3 (0) |
| 4 | first peer marks Unreachable | 17985 | 17991 | 17997 | 3 (0) |
| 4 | last peer marks Unreachable (all 4) | 17987 | 18003 | 18006 | 3 (0) |
| 4 | NeighborDown -> own mark (max over observers) | 3003 | 3004 | 3004 | 3 (0) |
| 4 | non-neighbor mark after first Unreachable gossip | 2 | 9 | 13 | 3 (0) |
| 4 | live members falsely marked Unreachable | 0 | 0 | 0 | 3 (0) |
| 5 false rumor | peers (of 3) marking live node Unreachable | 3 | 3 | 3 | 3 (0) |
| 5 | first peer marks Unreachable | 1 | 1 | 95 | 3 (0) |
| 5 | rumored node receives rumor | 91 | 182 | 188 | 3 (0) |
| 5 | rumored node re-announces | 92 | 183 | 189 | 3 (0) |
| 5 | first peer back Online | 93 | 183 | 193 | 3 (0) |
| 5 | last peer back Online | 93 | 184 | 195 | 3 (0) |
| 5 | peers (of 4) Online at end | 4 | 4 | 4 | 3 (0) |
| 6a replay, 5 settled nodes, S12 on | tables (of 3 watchers) showing departed node Online | 0 | 0 | 0 | 3 (0) |
| 6a control, S12 off | same | 0 | 3 | 3 | 3 (0) |
| 6b replay, fresh joiner, max age 3 s, S12 on | tables (of 4) showing it Online | 0 | 0 | 0 | 3 (0) |
| 6b control, S12 off | same | 4 | 4 | 4 | 3 (0) |

Variant: Presence at init with no delay (`--init-delay 0`), scenarios 1 and 2 only, n=3:

| Scenario | Measure | min | median | max | n (missing) |
|---|---|---|---|---|---|
| 1-init0 | all five tables full (20 s cap) | - | - | - | 0 (3) |
| 1-init0 | A learns C (20 s cap) | - | - | - | 0 (3) |
| 1-init0 | Introduce sent, 5 nodes | 20 | 20 | 20 | 3 (0) |
| 2-init0 | joiner holds all five (20 s cap) | - | - | - | 0 (3) |
| 2-init0 | first peer holds joiner | 11 | 11 | 11 | 3 (0) |

Scenario 1 and 2 times are dominated by the 3000 ms init delay; subtract it for protocol cost. The 25 and 27 ms minima are the one run where A and C learned each other by Introduce/ack before the broadcast (other two runs: gossip at ~3004).

## Against the spec

- D18 / test 19 (A shows C within the observer wait): CONFIRMED only when Presence is sent after the topic has formed. Init Presence at 3000 ms: A held C at 3004 ms (2 of 3 runs) or 25 ms (1 of 3). Presence at join (init0): tables never filled in 20 s, 3 of 3 runs. The first Presence is broadcast to zero neighbors and lost; NeighborUp Introduce reaches direct neighbors only (20 Introduce sent, vs 30 when the broadcast works); C (bootstrap B only) and A never learn each other. The spec says "init succeeds -> send Presence" and never says init must follow neighbor formation. The 3000 ms observer wait supplies that here; it is a timing assumption, not a mechanism.
- D18 late joiner learns everyone: CONFIRMED with init delay 3000 (joiner holds all five ~3.1-3.2 s after start; all five hold it at the same time). With init delay 0, the joiner reached only direct neighbors; held-all never reached in 20 s (3 of 3).
- D18 "no periodic traffic": CONFIRMED in shape. Cost: 5-node join = 30 Introduce + 30 IntroduceAck + 5 Presence; a sixth joiner = 12-14 Introduce + 12-14 acks + 1 Presence (the joiner sent 6-7 Introduce itself). Introduce runs both ways per pair (NeighborUp on both sides plus Presence-from-unknown), so about N^2 per full join. Many Introduce requests are dropped as stale at the receiver (seq already held) yet still acked.
- D19 / test 4 (A shows B gone within 1000 ms): CONFIRMED in the runs where Departure arrived (first Gone 1-96 ms, last Gone up to 196 ms). The "wait at most 1000 ms" does not work as an implicit flush. `broadcast()` returns in under 2 ms (enqueue). Shutdown only: Departure reached 0, 0, 2 of 4 peers before exit but all 4 eventually, 3 of 3 runs. 50 ms sleep: delivered to nobody in 2 of 3 runs; those peers ended Unreachable (NeighborDown + probe), not Gone. 1000 ms sleep: 4 of 4 before exit, 3 of 3. An earlier aborted run of the same code, shutdown only, delivered 0 of 4 (one run, not in the table). Cause of the 50 ms failures not found. Contradicts D19's "If you change this" direction: a lost Departure still ends Unreachable after about 3 s, not reachable-until-probe-fails forever.
- D20 / tests 4, 5 (probe before rumor; neighbor-down + one probe timeout): CONFIRMED. NeighborDown 14.98-15.0 s after SIGKILL. Probe failed in all 6 cases at 3002-3003 ms. Own mark came 3003-3004 ms after NeighborDown: "one neighbor-down event and one probe timeout" holds with 3-4 ms over 3000. Kill to every peer Unreachable: 18.0 s. Only 2 of 4 peers were neighbors of the victim; the others learned from Unreachable gossip within 2-13 ms of the first report. No live member falsely marked (0 in 3 runs).
- D20 "member that sees a rumor about itself announces again": CONFIRMED. Rumor reached the node 91-188 ms after injection; re-announce within 1-2 ms; every peer back Online within about 5 ms of that.
- Test 20 / S12 (replayed Presence changes nothing): CONFIRMED for a tombstoned member (6a: 0 of 3 watchers x 3 runs, drops logged `stale`) and for a member never seen departing (6b: 0 of 4 tables x 3 runs, drops logged `too_old`). Controls with S12 off: 6b resurrects it in 4 of 4 tables every run; 6a in 3 of 3 tables in 2 runs, 0 of 3 in 1 run (no drop logged; the replay may not have reached the tables; not investigated). The replay uses a fresh unsigned `nonce` so gossip does not de-duplicate it (an exact-bytes replay was not tried). Tombstone alone covers 6a, max age alone covers 6b; neither suffices for both.
- S13: NOT exercised. The address watcher is wired, but relays were disabled and no address change was induced. I did not count whether the watcher ever fired.
- Test 8 (message from unseen node accepted): not run (no Message layer).

## Surprises

1. Init Presence sent before neighbors exist is lost and nothing recovers it; tables stall at the direct-neighbor view.
2. `broadcast()` returning says nothing about delivery. The Departure wait is the deciding factor and the spec leaves it undefined. 50 ms did worse than 0 ms in these runs.
3. Rumor delivery to the target took 91-188 ms (two clusters), against 1-13 ms for the other gossip hops.
4. NeighborDown at 15 s, not the ~13 s measured earlier; this setup also set keep-alive 5 s and default-path idle 10 s.
5. One control replay (6a, S12 off) did not resurrect the member.

## What could make this wrong

- n=3 per cell; medians of three are not distributions. Bimodal results (A learns C, rumor delivery, Departure delivery) need more runs.
- One machine, one clock, loopback, no relays, no loss, debug build. Timestamps are wall-clock microseconds from separate processes.
- Every node has every address pre-loaded in a MemoryLookup; real discovery delay is absent, so Introduce dials always succeed.
- The 3000 ms init delay dominates scenarios 1 and 2.
- Max age (300 s; 3 s in 6b), the Gone-vs-contact rule, and the Departure sleep variants are my choices; S12 and D19 do not fix them.
- The sleep-50 loss and the 6a control anomaly are unexplained; I did not inspect per-node logs for them.
- Only 5-6 members; gossip active-view limits at larger N are untested.
