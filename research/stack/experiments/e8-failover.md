# E8 relay and bootstrap failover

Date 2026-10-20 (runs 2026-10-16 to 2026-10-20). Rust 1.99.1, debug build. iroh =1.3.0, iroh-gossip =0.101.0, iroh-relay =1.3.0 (feature `server`), tokio 1, postcard 1, preset `Minimal`.
Branch `spike/e8-failover`, worktree `/Users/ryzhakar/pp/estafette-e8-failover`, package `spikes/e8-failover/` (excluded from the workspace via `exclude = ["spikes"]`). Commit: `619b57f` (earlier partial commit `ae14493`). Raw per-run logs were kept in `/tmp/e8/*.raw.log` and are not committed; condensed traces and the per-run summaries are in `spikes/e8-failover/logs/`.
Machine: Apple M2 Max, macOS 15.3.1 (Darwin 24.3.0 arm64). All processes on one host over loopback.
Run: `cd spikes/e8-failover && E8_PAYLOAD=1 ./target/debug/e8-failover ctl <scn> <runs>` (scn 1, 2, 3, 4; controls 0, 6). Env switches: `E8_PAYLOAD` (see Setup), `E8_NOPROBE`, `E8_REJOIN`, `E8_ANN_PERIOD`, `E8_TAG`.

## Setup actually run

- R1, R2: plain-HTTP `iroh-relay` servers, each in its own OS process (`relay <n> <port>`) with a bootstrap node B1/B2 (stable key, relay map = its own relay only, `clear_ip_transports()`, gossip). SIGKILL of the process kills relay and bootstrap together. Fresh ports each run; restart reuses the port and the key.
- M1, M2, M3 (M4 in scenario 3): separate processes, relay map [R1, R2], `clear_ip_transports()`, QUIC `max_idle_timeout` 10 s on every endpoint (members and bootstrap nodes), stable keys derived from fixed bytes so ids are known to all. Home forced by rebinding until `addr().relay_urls()` equals the wanted relay (M1 on R1, M2 and M3 on R2; scenario 3: all on R2; scenario 4: M1 R1, M2 and M3 R2). Bootstrap peers are `EndpointAddr` = id + own relay URL, put in a `MemoryLookup`.
- Gossip: one topic, `subscribe_and_join(topic, [B1, B2])` (order varies in scenario 3). Each member broadcasts a heartbeat `Hb{from, seq}` every 1 s and an `Announce{from, addr}` of its current `EndpointAddr` on every `home_relay_status` or `watch_addr` update (estafette S13). "Presence" below means heartbeat receipt.
- Fresh dial by id (prober): every member dials each other member of M1..M3 by `EndpointAddr::from(id)` every ~1 s, new connection each time, 3 s attempt timeout, one echo round trip, then closes. A failed dial is a failure at 3 s (`timeout3s`) unless the error was immediate (`No addressing information available`).
- **Deviation from the brief.** The brief says "dial by id only (gossip's built-in lookup)". That did not work in this topology: with only the built-in lookup, all 41 probes per pair over 40 s failed with `No addressing information available` and setup never became healthy (log `logs/` not kept; one run, 524 events, 0 ok probes). Members were gossip neighbors only of B1 and B2, never of each other, so the built-in lookup never learned a member address. Every number below therefore uses `E8_PAYLOAD=1`: receivers call `MemoryLookup::set_endpoint_info(addr)` for each received `Announce` (the S13 design) in addition to the built-in lookup. "By id only" still holds: dials carry the id and nothing else.
- Window per run: scenario 1 and 2 88 s after the kill, scenario 3 up to 30 s, scenario 4 85 s. Setup (relays, join, mesh healthy by heartbeat and probe, +3 s) is excluded. Times are ms after the kill unless marked "after restart". Timestamps are unix ms from the same host clock. A dial time is the time the probe finished (connect time included, at most 3 s).

## Results

All rows: n = 3 runs unless stated. "never" = not observed within the window in any run (0/3).

### Scenario 1: kill R1+B1, M1 homed on R1, M2 and M3 on R2 (probers on)

| Measure | min | median | max | n |
|---|---|---|---|---|
| M1 `watch_addr` lists only R2 (re-home) | 40614 | 41716 | 42873 | 3/3 |
| M1 `home_relay_status` connected on R2 (same instant as above) | 40614 | 41716 | 42873 | 3/3 |
| M1 `home_relay_status` shows `connected=false` | 2 | 2 | 2 | 3/3 (all within 2 ms of the kill) |
| M2 first failed fresh dial to M1 | 3844 | 3933 | 6976 | 3/3 |
| M3 first failed fresh dial to M1 | 3533 | 3842 | 3935 | 3/3 |
| M2, M3 fresh dial to M1 ok again | never | never | never | 0/3 in 88 s (re-home at ~41 s did not help) |
| Heartbeats M1 to M2/M3 resume (and reverse) | never | never | never | 0/3. Longest gap 84397 / 85998 / 86097 ms |
| First NeighborDown at M1 | 10003 | 10004 | 10011 | 3/3 |
| First NeighborDown at M2 | 10004 | 10603 | 10820 | 3/3 |
| First NeighborDown at M3 | 10003 | 10514 | 10819 | 3/3 |
| NeighborDown count per member over 88 s (M1, M2, M3 medians) | 4 / 4 / 4 | | | |
| M2 to M3 (both homed on R2, R2 alive): longest heartbeat gap | 71161 | 75065 | 80475 | 3/3 |
| M2 to M3: first failed fresh dial | 11000 | 16008 | 19966 | 3/3 |
| M2 to M3: fresh dial ok again | 79052 | 79052 | 79052 | 1/3 (the other two never) |

NeighborDown at 10.0 s is the 10 s QUIC idle timeout. The first NeighborDown is B1 at M2, M3 and M1; M1 also loses M2 and M3 then.

Supplementary variants, scenario 1 (same code, one switch changed):

| Variant | Measure | min | median | max | n |
|---|---|---|---|---|---|
| No probers (`E8_NOPROBE`) | M1 re-home | 38519 | 41833 | 41945 | 3 |
| | longest HB gap M2 from M1 | 1182 | 84035 | 84088 | 3 (1 healthy, 2 dead) |
| | longest HB gap M1 from M2 | 1091 | 74060 | 78076 | 3 |
| | longest HB gap M2 from M3 (both on R2) | 1006 | 1093 | 68206 | 3 (2 healthy, 1 dead) |
| Re-join (`E8_REJOIN`: `join_peers([B1,B2])` every 5 s while the neighbor set is empty) | M1 re-home | 3517 | 20351 | 41900 | 3 |
| | HB resumes anywhere | never | never | never | 0/3. Gaps 74479-84490 ms |
| Control 0: nothing killed | longest HB gap, all pairs | 1003 | 1006 | 1092 | 2 runs; 0 NeighborDown |
| Control 6: SIGKILL M1 process, relays alive | longest HB gap M2 from M3, M3 from M2 | 1005 | 1091 | 1091 | 2 runs; 0 NeighborDown at M2, M3 in 88 s |

An earlier set of three probe-on runs (before the final code edit) gave the same picture: re-home 40738 / 42402 / 42702, HB never resumed.

### Scenario 2: as 1, R1 and B1 restarted in place at +20 s (probers on)

| Measure | min | median | max | n |
|---|---|---|---|---|
| Restart (R1 listening), after kill | 20007 | 20008 | 20017 | 3/3 |
| M1 `home_relay_status` connected on R1 after restart, after restart | 7563 | 13225 | 14455 | 3/3 |
| M1 re-homed to R2 at all (`watch_addr` lists only R2) | 40445 | 40445 | 40445 | 1/3 (runs 2-1, 2-3: M1 stayed homed on R1 and reconnected, `watch_addr` never emitted) |
| In that one run: M1 back on R1 (`watch_addr`), after restart | 63873 | | | 1/3 (flip R1 to R2 at 40 s, back at 84 s after kill) |
| M2 fresh dial to M1 ok again, after kill | 28828 | 34059 | 36881 | 3/3 |
| M3 fresh dial to M1 ok again, after kill | 31030 | 33840 | 36861 | 3/3 |
| HB M1 to M2/M3 resume | never | never | never | 0/3. Gap 70255-80282 ms |
| M2 or M3 heartbeat gap from the other (both on R2), longest | 1005 | 1094 | 80186 (M2) / 70169 (M3) | 3 (1 degraded) |

Final state of M1: homed on R1 in all three runs at 88 s.

### Scenario 3: B1 dead from the start, new member M4 joins

M1-M3 homed on R2, R1 and B1 killed, 3 s pause, then M4 starts (map [R1 dead, R2], home not forced). Probers on at M1-M3 and M4.

| Bootstrap order | Measure | min | median | max | n |
|---|---|---|---|---|---|
| [B1, B2] | M4 joined (`subscribe_and_join` returns), ms from join start | 9 | 9 | 9 | 3 |
| | presence of all three (first HB from each) | 904 | 920 | 1006 | 3 |
| | M4 dialable by id, all three | never | never | never | 0/3 in 30 s |
| [B2, B1] | M4 joined | 8 | 9 | 9 | 3 |
| | presence of all three | 907 | 912 | 1042 | 3 |
| | M4 dialable by id, all three | never | never | never | 0/3 in 30 s |
| [B1, B2], members announce every 3 s (`E8_ANN_PERIOD=3`) | joined | 9 | 9 | 15 | 3 |
| | presence of all three | 912 | 918 | 941 | 3 |
| | dialable by id, all three | 4022 | 4022 | 4028 | 3 |
| [B2, B1], same | joined | 9 | 12 | 12 | 3 |
| | presence of all three | 893 | 926 | 932 | 3 |
| | dialable by id, all three | 1023 | 1025 | 3019 | 3 |

M4 `online()` 404-407 ms after bind in every run; M4 always homed on R2 (6 of 6 default runs listed R2 only). Heartbeat interval is 1 s, so presence time is bounded below by phase: first HB at 0.58-1.0 s is the heartbeat phase, not a join cost.

### Scenario 4: both relays killed at once, R1+B1 back at +20 s (R2+B2 stay dead), probers on

| Measure | min | median | max | n |
|---|---|---|---|---|
| `home_relay_status` `connected=false` at M1, M2, M3 | 1 | 1 | 2 | 3/3 each |
| First failed fresh dial (M1 / M2 / M3, medians) | 3728 | 3942 | 3911 | 3/3 |
| First NeighborDown (all members) | 10003 | 10004 | 10019 | 3/3 |
| Restart (R1 listening) | 20007 | 20007 | 20035 | 3/3 |
| M1 status connected after restart, after restart | 5155 | 17267 | 18045 | 3/3 |
| M2 status connected after restart, after restart | 8634 | 19215 | 23518 | 3/3 |
| M3 status connected after restart, after restart | 10814 | 19215 | 24368 | 3/3 |
| All 6 heartbeat pairs flowing again | never | never | never | 0/3 in 65 s after restart |
| All 6 fresh dials ok again | never | never | never | 0/3 in 65 s after restart |

M2 and M3 (homed on the dead R2) re-homed to R1 (`watch_addr` lists only R1) at 43525 and 44375 ms after kill in run 4-1 (23.5 s after restart); the other runs ended at 85 s with all three homed on R1. With `E8_REJOIN` (n=3): status connected after restart M1 7463 / 9024 / 11085, M2 19661 / 20754 / 24272, M3 18344 / 20590 / 24272; HB flowing again 0/3; fresh dials all-6 0/3; in 1 run the pair M1-M3 dialed ok again at 52.4 s (M1 to M3 53.1 s) after restart.

## Event traces (one run per scenario, ms relative to the kill)

Filtered: state changes only. Successful dials and announces omitted. Full traces: `spikes/e8-failover/logs/*.trace.txt`.

Scenario 1 (run 1-1):

```
       0  CTL KILL R1+B1
       2  M1 STATUS relay=R1 connected=false err=-
       2  M1 STATUS relay=R1 connected=false err=Failed_to_connect_to_relay_server
    3933  M1 dial FAIL to M2, M3 (timeout3s); M2, M3 dial FAIL to M1
   10003  M1 ND M2 / M1 ND M3 / M3 ND M1 / M3 ND B1 / M2 ND M1 / M2 ND B1 / M1 ND B1
   12119  B2 ND M1
   16008  M2 dial FAIL to M3; M3 dial FAIL to M2   (both homed on R2, R2 alive)
   22023  M1 ND B2
   22966  M2 ND M3
   23965  M3 ND M2
   30956  M3 ND B2
   36959  M2 ND B2          (M2 has no gossip neighbor left)
   41716  M1 STATUS connected=false; M1 ADDR relays=[R2]; M1 STATUS connected=true
   84061  M2 dial FAIL to M3 (still failing)
   88002  CTL FINAL M1 homed on R2
```

Scenario 2 (run 2-1; M1 stayed on R1):

```
       0  CTL KILL R1+B1
       1  M1 STATUS connected=false err=Failed_to_connect_to_relay_server
    3825  M1/M3 dial FAIL
    7850  M2/M1 dial FAIL
   10004  M1 ND M3;  10010 M1 ND B1;  10804 M2, M3 ND B1;  10804 M3 ND M1
   10869  M2 and M3 dial FAIL to each other
   20002  CTL RESTART R1+B1;  20007 R1 RELAY_UP
   34462  M1 STATUS connected=true (R1)    (14.5 s after restart)
   88023  CTL FINAL M1 homed on R1
```

Scenario 3 (run 312ann-1, kill at 0, M4 starts 3015 ms after):

```
    3015  M4 JOIN_START order=12
    3025  M4 JOINED ms=9  (B2 NU M4)
    3025  M4 dial FAIL M1, M2, M3: No_addressing_information_available
    3133  M4 RX announce from M1 (via B2)
    3419  M4 STATUS connected=true (R2); ONLINE ms_after_bind=404
    3508  first HB from M3;  3721 from M2;  3934 from M1
    4031  M4 dial ok to M1 (first announce in M4's lookup was M1's)
    6009-6437  next announces from M3, M2, M1; M4 dial ok to M3 at 6036
```

Scenario 4 (run 4-1):

```
       0  CTL KILL R1+B1 and R2+B2
       1  M1, M2, M3 STATUS connected=false;  2 ms: err=Failed_to_connect_to_relay_server
    3554  M1 dial FAIL;  3911 M3 dial FAIL;  3976 M2 dial FAIL
   10003  M1 ND B1, B2;  M2 ND B2, (B1 at 10010);  M3 ND B1, B2 (10009)
   20002  CTL RESTART R1+B1;  20007 R1 RELAY_UP
   25162  M1 STATUS connected=true (R1)
   43525  M2 ADDR relays=[R1]; connected=true
   44375  M3 ADDR relays=[R1]; connected=true
   85005  CTL FINAL all three homed on R1, no heartbeat flowing since the kill
```

## Answer: does a member homed on a dead relay degrade, and for how long

Yes, and not only the member homed there.

- The homed member's relay status turns `connected=false` within 2 ms of the kill, with error `Failed to connect to relay server`. `watch_addr` keeps listing the dead relay (as in e1 check 6).
- Re-home to the surviving relay in the map takes 38.5-42.9 s (n=9 across probe-on, probe-off and re-join runs), with one 3.5 s and one 20.4 s outlier in the re-join runs. It does happen; cause of the 40 s not isolated. When the dead relay returns after 20 s the member usually stays homed there and reconnects 7.6-14.5 s after the restart (2 of 3), once it flipped to R2 at 40 s and back at 84 s (1 of 3).
- Fresh dials by id to M1 failed from ~3.5 s (first 3 s timeout) in all runs. They recovered only when R1 returned (28.8-36.9 s after the kill, 8.8-16.9 s after restart). Re-homing at ~41 s did not restore them within the window (0/3). Reason consistent with the data: peers hold M1's old address (R1 URL) and a relay never forwards to a peer homed elsewhere (e6); the re-announce that carries the new home travels over gossip, and gossip was dead.
- Gossip is the larger failure. After R1+B1 died, gossip heartbeats between M1 and the others never resumed in 88 s in any probe-on run, with or without re-join calls. Connections idle out at 10 s (first NeighborDown 10.0-10.8 s). M2 and M3, both homed on the live R2, lost gossip with each other too in 3 of 3 probe-on runs (gap 71-80 s) and in 1 of 3 probe-off runs (68 s). The control with only M1's process killed and the relays up showed no such loss (max gap 1.1 s, 0 NeighborDown, n=2), and the no-kill control stayed healthy (n=2). So the loss follows the relay and B1 dying, not a dead peer as such. The member-side cause is not isolated. Reading iroh-gossip `net.rs:686` shows the actor awaits `active_send_tx.send(..)` on a bounded queue (`SEND_QUEUE_CAP` 64); a stuck connection could block it. This reading is untested here.
- Both relays dead: all three report `connected=false` within 2 ms, lose every neighbor at 10.0 s, and dials fail at the 3 s timeout. When one relay returns after 20 s all members reconnect to it (M1 5-18 s, M2 and M3 9-24 s after restart), M2 and M3 re-home to R1 in the 23 s range. Gossip and fresh dials did not recover within 65 s in any run (0/3). A 5 s `join_peers([B1,B2])` loop did not fix it (0/3 for the full mesh; one pair dialed again at ~52 s).
- Bootstrap failover is cheap: with B1 dead, M4 joined in 8-18 ms in both orders and received all three members' presence in 0.9-1.1 s (bounded by the 1 s heartbeat). Order does not matter in these runs; gossip contacts the bootstrap peers together. M4 `online()` took ~0.4 s with R1 dead and it homed on R2 every time.
- A late joiner is not dialable by id until members announce again. Heartbeats carry no address. With announce only on status change M4 got no address for 30 s (0/6); with a 3 s periodic announce M4 was dialable 1.0-4.0 s after joining.

## Surprises

- The built-in gossip address lookup supplies nothing for members that are not gossip neighbors. In this 5-node topology (B1, B2, M1-M3) members were neighbors of the bootstrap nodes only. e6's "mesh members are dialable by id" held for 3 nodes where all are adjacent, not here.
- Gossip dedupes by message content. Identical periodic `Announce` messages were silently dropped in my first ann-period run (M4 never got one); adding a changing field fixed it. S13 re-announcements of an unchanged address will be dropped the same way. Heartbeats carry a `seq` for this reason.
- A relay failure takes gossip down for members not homed there (above). Not predicted by the brief.
- Re-home time is bimodal across runs (3.5 s, 20.4 s, 38-43 s). Home relay is not chosen by map order (e6); here the re-home target was the only live relay.
- Setup needed rebinding for homing: `online()` returned immediately in the final runs (ONLINE ms 0 for forced members) because the homing loop had already waited; e6's 3 s plain-HTTP delay shows up inside the homing loop, not in the reported numbers.
- `NeighborDown` is the idle timeout (10.0 s plus up to ~1 s), not a prompt relay-loss signal. The prompt signal is `home_relay_status` at 1-2 ms.
- Heartbeat "resumed" values for runs with small gaps (about 1 s) are the end of the largest ordinary gap, not a recovery. Read them with the gap column.

## What could make this wrong

- Prober effect. The 1 s fresh-dial probers produced dead gossip between M2 and M3 in 3 of 3 runs (scenario 1) but 1 of 3 without them. They may amplify the stall. Scenario 2 and 4 were run probe-on only. The recovery numbers for fresh dials need the probers, so that confound cannot be removed for them.
- n=3 per cell, bimodal cells (re-home, M2-M3 health). Min/median/max of three hide the split; per-run values are in the logs. Control runs n=2.
- B1 and R1 die together by design. I did not separate "relay dead" from "bootstrap node dead". The M1-process control shows a dead peer alone does not stall M2-M3, but a dead B1 with live R1 was not run.
- `E8_PAYLOAD=1` in all runs (see Setup). With built-in lookup only, the mesh never became dialable. Results with announce ingestion differ from "gossip's built-in lookup only".
- The first runs (scenario 1-2) used identical-content announces (deduped by gossip); later runs add a changing field. The scenario 1, 2 and 4 re-announce paths ran with the older form. A re-announce of a changed address is not deduped, so the re-home announce was sent; I did not verify per run that it was delivered after M1's re-home (gossip was down).
- One host, loopback, plain HTTP relays, no TLS, no NAT. Connection setup, relay latency and netreport behavior differ on a real network. The 40 s re-home is unexplained; it may be a relay-probe interval specific to localhost latencies.
- QUIC idle 10 s set on all endpoints, including relay-adjacent bootstrap nodes. Default idle timeout would change every NeighborDown time.
- SIGKILL of the relay process closes TCP connections with RST at once, so members see the loss within 2 ms. A silent network partition would be slower; not tested.
- Timestamps are logged when a line is printed by each child and read by the controller; sub-100 ms skew possible on a loaded host (up to ~6 processes).
- The head-of-line explanation (`net.rs:686`) is a code reading, not a test. Treat it as a hypothesis.
- Scenario 3 presence time is quantized by the 1 s heartbeat; join and online times are not.
- Debug build only.
