# E16 re-dial fix for iroh-gossip 0.101.0 (collision C17), measured in the gossip harness

Date 2026-10-21 (runs 00:04-01:27 UTC, 03:04-04:27 local). Experiment E16 for estafette.

## Answer

- The fix does not restore heartbeats in the harness. F1 (fix, no levers) 0/30 stalled pairs flowing at 180 s; F2 (fix + L2) 0/30. Control F0 also 0/16.
- F1: the fixed branch never runs. Traced run 101: 0 `start to dial`, 0 `dial failed`, 0 `E16 fix` lines after the kill. All peers were `Active`; timeouts emitted `DisconnectPeer`, which already removes the entry (`net.rs:732-736`). Nothing sends, so nothing dials. C17's path needs a send to a peer after a failed dial; without L2 or W1 there is none.
- F2: the fix works as built (traced run 102: `E16 fix` 39-40 times per member, 28-32 dial starts per member after 60 s) and every dial fails (`timed out`). Fresh prober dials never succeed in 180 s in 5/5 F2 runs (0/30 pairs), against 24/30 (F1) and 10/18 (F2c, L2 without the fix) pairs that do by 144-176 s. Reading, not instrumented: the re-dials keep addressing the dead relay and keep the E14 block alive.
- F3 (fix + W1): 30/30 pairs flowing at 180 s, heal 15.7 / 18.1 / 167.6 s; 2/5 runs relapsed. No evidence the fix removes relapses (E15: 5/13).
- gossip main (c5f44db, 2026-10-20) does not have this path: it removes the `peers` entry on every `PeerDisconnected` (`handle_in_event_inner`, introduced by PR #178, f02f79e857, 2026-10-14; the PR text names this bug: "we never dialed it again, see #155").

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e16-redial`, branch `spike/e16-redial` from `spike/e15-workaround` (37b0a1a). Commit `c86fdbf` (harness tags, analysis, patch, logs; pushed). Package `spikes/e16-gossip-fix`; harness `spikes/e10-gossip-main` (variant a).
- Package: same `src/main.rs` through `[[bin]] path = "../e10-gossip-main/src/main.rs"` (as `e15-diag-gossip`), iroh =1.3.0 stock, iroh-relay =1.3.0, iroh-gossip =0.101.0 from `[patch.crates-io] iroh-gossip = { path = "vendor/iroh-gossip-0.101.0" }`. `vendor.sh` copies the registry crate and applies `e16-iroh-gossip-0.101.0.patch`; `vendor/` and `target/` are not committed (`.gitignore`).
- Build: `sh vendor.sh && cargo build --features a && mkdir -p target/bin && cp target/debug/e16-gossip-fix target/bin/e10-a`. sha256 8ac8addb069d... Rust 1.99.1, debug, macOS 15.3.1 arm64 (Apple M2 Max), loopback, plain-HTTP relays, SIGKILL of R1+B1 (scenario 1), `E8_PAYLOAD=1`, 1 s heartbeat, QUIC idle 10 s, preset `Minimal`, probers on, 180 s window.
- Run order (not interleaved, not randomised): F0 (3), F1 (3, then traced 101, then 2), F2 (3, traced 102, then 2), F3 (3 + 2), F4 (3), F2c (3, supplementary). Cell commands, `E15_BIN=<abs>/spikes/e16-gossip-fix/target/bin/e10-a` set, one foreground call each, run numbers shown:
  - `./run-cell.sh a 1 f0w 3 1`
  - `./run-cell.sh a 1 f1w 3 1`; `./run-cell.sh a 1 f1w 2 4`; traced `E12_TRACE="iroh_gossip=debug" ./run-cell.sh a 1 f1w 1 101`
  - `./run-cell.sh a 1 f2w 3 1`; `./run-cell.sh a 1 f2w 2 4`; traced `E12_TRACE="iroh_gossip=debug" ./run-cell.sh a 1 f2w 1 102` (extra, not asked)
  - `./run-cell.sh a 1 f3w 3 1`; `./run-cell.sh a 1 f3w 2 4`
  - `./run-cell.sh a 0 f4w 3 1`
  - `./run-cell.sh a 1 f2cw 3 1` (extra: L2 without the fix, to separate L2 from the fix)
  - after each: `sh e16-archive.sh a-1<tag>-`; traced runs moved to `logs/e16/diag/` (excluded from tables).
- Tags (added to `run-cell.sh`): `f0w` `E16_FIX` unset (explicit unset), `f1w` `E16_FIX=1`, `f2w` `E16_FIX=1 E12_L2=1`, `f2cw` `E12_L2=1` with `E16_FIX` unset, `f3w` `E16_FIX=1 E15_W1=1` (T 3000), `f4w` `E16_FIX=1` (run with scenario 0). All `E10_WINDOW_MS=180000`.
- Analysis (new, over E15's `measures`): `python3 -I analyze_e16.py logs/e16 --table|--runs|--dial`, `python3 -I analyze_e16_probe.py logs/e16 <arm-prefix>...`. Outputs saved in `logs/e16/table.md`, `runs.md`, `dial-to-hb.txt`, `probe-first-ok.txt`.
- Load (1 min) at cell start: 0.53-2.33 (2.33 at F0, the rest 0.53-1.35). Other users on the host. Cell starts: `logs/summary/a-*f*-r*.txt`. The committed E10 file `logs/summary/a-1w-r1.txt` was not touched (no `w` tag cell run).
- `pgrep -fl "e10-|e16-|e15-diag"` empty after the last cell and before the return.

## The patch

`spikes/e16-gossip-fix/e16-iroh-gossip-0.101.0.patch` (applies with `patch -p0 src/net.rs`), +4 / -0 lines, `src/net.rs` only:

```diff
--- src/net.rs	orig
+++ src/net.rs	E16
@@ -438,6 +438,10 @@
                         let peer_state = self.peers.get(&peer_id);
                         let is_active = matches!(peer_state, Some(PeerState::Active { .. }));
                         if !is_active {
+                            // E16 (diagnostic, `E16_FIX=1`): drop the Pending entry and its queued messages, so the next send to this peer starts a new dial.
+                            if std::env::var_os("E16_FIX").is_some_and(|v| v == "1") && self.peers.remove(&peer_id).is_some() {
+                                debug!(peer = %peer_id.fmt_short(), "E16 fix: dropped pending entry after failed dial");
+                            }
                             self.handle_in_event(InEvent::PeerDisconnected(peer_id), Instant::now())
                                 .await;
                         }
```

Runtime switch `E16_FIX=1`; unset (or any other value) = stock behavior. Removal comes before `PeerDisconnected`, so a message the protocol emits to that peer while handling the event creates a fresh `Pending` entry and dials.

Can removing the entry break anything (source read, `iroh-gossip-0.101.0/src`):
- A dial already in the dialer: no. `Dialer::next_conn` removes the peer from `pending_dials` before returning the result (`net.rs:1052`), and `queue_dial` dedups on `is_pending` (`net.rs:1015`). At the failure branch (`net.rs:435-443`) no dial for that peer is in flight.
- A connection that arrived while we dialed: the entry is then `Active` (`handle_connection`, `net.rs:525-541`, `accept_conn` `net.rs:778-785`), `is_active` is true, nothing is removed.
- A connection that arrives later: `handle_connection` takes the `Vacant` arm (`net.rs:531-538`) and creates `Active` with an empty queue. Correct; the dropped queue is the only loss.
- Dropped messages: the queue holds messages of all topics for that peer. `PeerDisconnected` goes to every topic (`proto/state.rs:139`), which already treat the peer as gone, so the queue is stale. A real loss would be a one-shot message the protocol still wants delivered; I found none by reading, I did not test.
- Redial rate: with the fix, the protocol's next send to a failed peer dials at once. Harness dials took 10 s to time out (rate about 1 per 4-5 s per member under L2 with 4 peers). A dial that fails instantly (for example no addressing information, as the probers see in 0 ms) plus a protocol that re-sends at once could loop tightly. Not tested.
- Observed side effect: under a blocked transport the added dials keep addressing the dead relay (F2 below).

## Results

Times in seconds after the kill. Stalled pair = directed heartbeat pair (6 per run) with a gap >= 5 s. Heal = first heartbeat at the receiver after the pair's longest gap. Per-run detail `logs/e16/runs.md`.

| Arm | runs | runs stalled | stalled pairs | stalled pairs flowing at 180 s | heal s min / med / max | per run all stalled pairs flowing from s | rebinds per member | runs with a second stall episode |
|---|---|---|---|---|---|---|---|---|
| F0 fix binary, `E16_FIX` unset, no levers | 3 | 3 | 16 | 0 | - | - | 0 | 0 |
| F1 `E16_FIX=1`, no levers | 5 | 5 | 30 | 0 | - | - | 0 | 0 |
| F2 `E16_FIX=1` + L2 | 5 | 5 | 30 | 0 | - | - | 0 | 0 |
| F3 `E16_FIX=1` + W1 T=3000 | 5 | 5 | 30 | 30 | 15.7 / 18.1 / 167.6 (n=30) | 16.7 / 18.2 / 167.6 (n=5) | 1-3 | 2 (runs 2, 3) |
| F4 `E16_FIX=1`, no kill (scenario 0) | 3 | 0 | 0 | 18/18 pairs flowing | - | - | 0 | 0 |
| F2c (extra) `E12_L2=1`, `E16_FIX` unset | 3 | 3 | 18 | 0 | - | - | 0 | 0 |

F0 stalled pairs: 6, 4, 6 (run 2 kept 2 pairs flowing, as E10 and E15 A0 run 5 / A1 run 2 showed bimodal onsets). F3 first rebind 10.1 / 12.3 / 13.0 s (E15 A2: 7.0 / 15.9 / 21.0). F3 false rebinds 0; F3 rebinds per run 3, 4, 5, 3, 3 (relapse runs 2 and 3: 4 and 5 rebinds). L2 `join_peers` calls: 39 per run in F2 and F2c, 0 elsewhere.

Time from each pair's first ok fresh dial to its first heartbeat (`logs/e16/dial-to-hb.txt`; t_dial = earliest, over both directions of the pair, of the first prober `PROBE ok` after that direction's first `PROBE fail` after the kill; signed, heal minus t_dial):
- F0, F1, F2, F2c: no heartbeat after any ok dial (no pair healed). Pairs with an ok fresh dial after the stall began (directed prober pairs): F0 12/18, F1 24/30 (at 61-158 s; run 1: none), F2c 10/18 (144-176 s), F2 0/30 (`logs/e16/probe-first-ok.txt`).
- F3: min -0.0 / median 1.0 / max 149.1 s (n=30). 25 of 30 pairs within 2.2 s. The large values (73.2, 127.3, 128.3, 147.6, 149.1 s) are pairs in the two relapse runs, where "heal" is the end of the longest gap, which is the second one, while the first ok dial was early.

Traced runs (excluded from tables; `logs/e16/diag/`, `iroh_gossip=debug`):
- F1 run 101 (stalled like the others, 6/6 pairs, 0 flowing): `start to dial` lines per member 2 / 2 / 3, all at 00:23:16-17, 4-5 s before the kill (00:23:21). After 60 s: 0. `dial failed`: 0. `E16 fix`: 0. All gossip connections ended `timed out` at 10-49 s, each followed by `PeerDisconnected` and `DisconnectPeer`. So re-dials do not happen in F1 and there is no failed dial for the fix to act on.
- F2 run 102 (6/6 stalled, 0 flowing; last heartbeat 12.9 s after the kill): `E16 fix` lines M1 39, M2 40, M3 39; `start to dial` after the kill 43 / 40 / 43, of which after 60 s 32 / 28 / 32; `dial failed: timed out` 39 / 40 / 39 (the first at 16.8-20.1 s, the last at 169.6-176.4 s), 0 `dial successful` after the kill. Prober dials in the same run: 0 ok after the first fail, in any of the 6 directed pairs, for 180 s.

## Mechanism notes

Source lines are `iroh-gossip-0.101.0/src/net.rs` unless noted; I read them in this run.
1. Stock behavior (E15 note 1, confirmed by reading): dial failure `net.rs:435-443` sends only `InEvent::PeerDisconnected` for a non-active peer; `peers` keeps `Pending { queue }` (`net.rs:760-768`); a dial starts only if the queue is empty (`net.rs:695-699`); removal only at `OutEvent::DisconnectPeer` (`net.rs:732-736`), which the protocol emits for active-view peers and on `handle_connection_closed` (`proto/hyparview.rs:346-377`, `639-655`).
2. Precondition of C17: a send to a non-active peer, then a failed dial. In F1 (and E15 A0) there is none after the kill: connections idle out as `Active`, `DisconnectPeer` removes the entries, the active view is empty, and no timer or rule sends again (trace 101). So the fix has nothing to repair in the no-lever case. This is the same observation as E15 note 3 (no `start to dial` without L2).
3. With L2 or W1 there are sends, so Pending entries appear. Stock (E15 A1 trace): 4 dials per member, then none. With the fix (F2 trace): a dial every 4-5 s per member for the whole window. The fix therefore does what it was written to do.
4. Why F2 stays dead, from measurements: in all 5 F2 runs no fresh prober dial succeeds in 180 s (0/30 pairs), while F1 (same binary, `E16_FIX=1`, no L2) recovers in 4/5 runs at 61-158 s and F2c (L2, fix off) recovers in 3/3 runs (10/18 pairs, 144-176 s). The only difference F2 vs F2c is the fix, and it changes the dial count from about 4 to about 40 per member. Consistent with E14 report section 5 items 6-7: the dead relay's actor exits only when idle 60 s (`RELAY_INACTIVE_CLEANUP_TIME`), and datagrams still addressed to it (gossip's dials to B1 and M1, which advertise R1) refill its queue and block `RelayActor` again. I did not instrument the queue in F2 (the E14 diag patch was not applied), so this is a reading that fits, not a measurement. Gossip's own dials to M2/M3 (live R2) also time out for the same reason (shared `RelayActor`).
5. Why F3 works: W1 rebinds the endpoint and `Gossip` (E15 note 4); heartbeats return a median 1.0 s after the first ok dial. The relapses (2/5) happen with the fix as without it; not explained here either, and the fix adds dials that, by note 4, can refill a queue of the new endpoint.
6. gossip main check: below.

## iroh-gossip main check

- Rev checked: `c5f44db74ccc232d4e81523117b7e95e36d4c5c5` (main head on 2026-10-21, commit date 2026-10-20 09:49Z, `Cargo.toml` version still 0.101.0; latest tag v0.101.0 = 2ce78afe09, 2026-06-15). File `src/net.rs`, 3920 lines (0.101.0: 1977).
- The dial-failure branch is unchanged: `net.rs:475-484` (same `is_active` test, same `InEvent::PeerDisconnected` for non-active). The change is downstream. `handle_in_event_inner` (`net.rs:779-783`):

```rust
    async fn handle_in_event_inner(&mut self, event: InEvent, now: Instant) {
        // Drop the state first, so that a message the event causes dials anew.
        if let InEvent::PeerDisconnected(peer) = &event {
            self.peers.remove(peer);
        }
```

  so the `peers` entry is dropped on every `PeerDisconnected`, including after a failed dial. A queue of a dropped peer is kept as `PeerState::Dropped { queue }` while a dial runs (`net.rs:894-920`, `disconnect_peer`; `SendMessage` turns `Dropped` back into `Pending`, `net.rs:803-810`; dial started when the queue is empty, `net.rs:822-826`). `PeerState::Active` gains `stale_dial`.
- Introduced by PR #178 "fix: connection lifecycle fixes", commit `f02f79e857ab6b7cb697c4bbc08b9ad3c5ea6e11`, 2026-10-14. The comment "Drop the state first, so that a message the event causes dials anew." is absent at `b89fa9816e` (2026-10-09, parent side) and present at `f02f79e857`. PR text: "When a connection to a peer was closed before it became a neighbor, e.g. after a refused connection, we kept a dead send channel around without recovery. Later messages to it were lost, and we never dialed it again, see #155." Issue #155 (closed) is titled "Gossip connection refusal/protocol violation results in connection actor never terminating".
- Consequence: C17's code path is fixed upstream on main and unreleased (no tag after v0.101.0 in the tag list). The E16 patch is a narrower version of the same idea (removal only in the failed-dial branch for non-active peers; main removes on all `PeerDisconnected` and keeps the queue for the dial in flight). I did not run main in this experiment; E10 variant b pins an older git rev, not c5f44db.

## Hypotheses and verdicts

- H1 The one-place fix restores heartbeats after the transport recovers, without levers (F1): contradicted. 0/30 pairs, 5/5 runs; fix path not reached (trace 101).
- H2 With L2 the fix produces re-dials that heal: re-dials confirmed (F2 trace: 39-40 fix hits and 28-32 dial starts after 60 s per member); heal contradicted, 0/30 pairs, and fresh dials never work in the window (0/30 prober pairs) where they do without the fix. In the harness the fix makes L2 worse for the transport, by the reading in note 4.
- H3 The fix removes W1's relapses (E15 5/13): not shown. F3 relapsed in 2/5 runs (runs 2 and 3); E15 W1 T=3000 arms relapsed 3/10 (A2 2/5, A3 1/5). Not distinguishable at these n.
- H4 No change in a healthy mesh (F4): no stalled pair in 3 runs x 180 s; all 18 pairs flowing at the end. The fix branch fires only after a failed dial of a non-active peer; I did not trace a healthy run to count it.
- H5 (not asked) C17 exists on main: not present. Upstream already drops the entry (`f02f79e857`).

## What could make this wrong

- n is 3-5 per arm, onsets are bimodal (F0 run 2, E15 A0 run 5), and the arms ran in sequence over 83 minutes on a host with other users (load 0.5-2.3 at cell starts).
- F0 had n=3 and stalled 3/3 with 16 of 18 pairs, like E15 A0 (4/5); I treat it as stalling like A0. F0 and F1 differ only by `E16_FIX`; their equality (both 0 healed) is expected from note 2.
- The F2 explanation (re-dials refill the dead relay's queue) is a reading. The measured facts are: dial count about 40 vs 4 per member, and 0/30 vs 10/18 prober pairs recovering by 180 s. I did not apply the E14 diag patch to see the queue or the RelayActor block state in F2. Another cause for the F2 result is possible (for example retransmits of the many new QUIC handshakes), and I did not separate them.
- F2 traced run is one run (102); F1 traced run is one run (101). The F2c arm (extra) is n=3 with fix binary and `E16_FIX` unset; its first-ok times (144-176 s) are later than F1's (61-158 s) with overlapping ranges, so I do not read a difference between them.
- "Dial-to-heartbeat" for F3 uses the pair's longest gap (E15 definition), so relapse pairs show the second heal; the first-heal figure for those pairs is not in the table. My probe definition (first ok after the first fail, earliest of both directions) is my own choice made before seeing F3 data; the harness uses one direction (`main.rs:634-635`).
- F1 never reaches the fixed branch; the experiment cannot say how the fix behaves when the transport is healthy but gossip dials fail. That needs a failing-dial source other than the E14 block, for example a peer that is offline while the transport works (E8 scenario 7 style); not run. The E14 `drain` fix cannot be used for this: with it the stall never starts (E15 A4).
- Fix is gated by `std::env::var_os` on each failed dial; negligible cost, but it is not the shape of an upstream patch.
- One host, loopback, debug build, plain-HTTP relays, SIGKILL closes sockets at once, 3 members, QUIC idle 10 s, preset `Minimal`, scenario 1 only (B2 stays up). Stock iroh 1.3.0 (no transport fix) as asked. Source lines checked against registry 0.101.0 and the GitHub raw file at the rev above, not against a clone.
- `analyze_e16.py` is new; it reuses E15's `measures`. Its F0 to F2 rows match the per-run table by hand (6/6 pairs stalled in the F2 runs, counted in `runs.md`).

## Exact commands to rerun on a Linux host

Branch arrives as a bundle on top of `spike/e15-workaround`; the host reaches crates.io and GitHub.

```sh
git fetch /path/to/e16.bundle spike/e16-redial:spike/e16-redial && git checkout spike/e16-redial
cd spikes/e10-gossip-main
cargo build --features a >/dev/null          # warms the registry and fetches iroh-gossip-0.101.0 (also builds variant a)
cd ../e16-gossip-fix
sh vendor.sh                                  # cargo fetch, copies the registry crate to vendor/, applies the patch (prints "patching file 'src/net.rs'")
cargo build --features a && mkdir -p target/bin && cp target/debug/e16-gossip-fix target/bin/e10-a
grep -c "dropped pending entry" target/bin/e10-a    # expect 1
cd ../e10-gossip-main && mkdir -p /tmp/e10 logs/e16
export E15_BIN=$(cd ../e16-gossip-fix && pwd)/target/bin/e10-a
pgrep -fl e10-    # must be empty before each cell; run-cell.sh aborts otherwise
# each run is about 185 s; keep a call to 3 runs
./run-cell.sh a 1 f0w 3 1;   sh e16-archive.sh a-1f0w-       # F0 control, must stall (E15 A0: 4/5 runs)
./run-cell.sh a 1 f1w 3 1;   sh e16-archive.sh a-1f1w-       # F1
./run-cell.sh a 1 f1w 2 4;   sh e16-archive.sh a-1f1w-4; sh e16-archive.sh a-1f1w-5
./run-cell.sh a 1 f2w 3 1;   sh e16-archive.sh a-1f2w-       # F2
./run-cell.sh a 1 f2w 2 4;   sh e16-archive.sh a-1f2w-4; sh e16-archive.sh a-1f2w-5
./run-cell.sh a 1 f3w 3 1;   sh e16-archive.sh a-1f3w-       # F3
./run-cell.sh a 1 f3w 2 4;   sh e16-archive.sh a-1f3w-4; sh e16-archive.sh a-1f3w-5
./run-cell.sh a 0 f4w 3 1;   sh e16-archive.sh a-0f4w-       # F4, no kill
./run-cell.sh a 1 f2cw 3 1;  sh e16-archive.sh a-1f2cw-      # extra: L2 without the fix
# traced runs (excluded from tables; move their archives out of logs/e16 before analysis, traces are in /tmp/e12-trace-M{1,2,3}.log)
rm -f /tmp/e12-trace-*.log
E12_TRACE="iroh_gossip=debug" ./run-cell.sh a 1 f1w 1 101; sh e16-archive.sh a-1f1w-101; mkdir -p logs/e16/diag; mv logs/e16/a-1f1w-101.* logs/e16/diag/
grep -c "start to dial" /tmp/e12-trace-M*.log; grep -c "dial failed" /tmp/e12-trace-M*.log; grep -c "E16 fix" /tmp/e12-trace-M*.log
rm -f /tmp/e12-trace-*.log
E12_TRACE="iroh_gossip=debug" ./run-cell.sh a 1 f2w 1 102; sh e16-archive.sh a-1f2w-102; mv logs/e16/a-1f2w-102.* logs/e16/diag/
python3 -I analyze_e16.py logs/e16 --table
python3 -I analyze_e16.py logs/e16 --runs
python3 -I analyze_e16.py logs/e16 --dial
python3 -I analyze_e16_probe.py logs/e16 a-1f0w- a-1f1w- a-1f2w- a-1f2cw- a-1f3w-
# after-60 s dial starts per member in a trace: filter the trace lines by timestamp against the KILL line of /tmp/e10/a-1f1w-101.raw.log (epoch ms)
```

Total for the table: 24 runs (about 75 min) plus 2 traced (about 6 min). A Linux host: expect the E14 recovery at 61-80 s per member as in the lab notes; if F2 recovers there, my note 4 is wrong. Compare F2 against F2c first.
