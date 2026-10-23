# E18 late attachment after a W1 rebind (estafette)

Date 2026-10-21 (runs 03:26-04:58 UTC). Question: why a member that rebinds and re-joins through B2 gets no gossip neighbor for 57-120 s, and what counters it.

## Answer

- Cause, established: B2 holds a stale `pending_neighbor_requests` entry for the member from before the kill, and the member is still in B2's active view. The member's new Join then reaches `add_active` -> `send_neighbor`, which sends nothing for a peer already pending (`proto/hyparview.rs:380-383, 694-711, 745-752`, 0.101.0). The joiner gets no Neighbor, so `subscribe_and_join` never returns. 15/15 Joins that met `active + pending` at B2 got no Neighbor from B2 (11 in arms without the fix, all late; the 4 in the fix arm got one at once); 0/66 other Joins were late.
- The stale entry comes from a protocol collision, pre-kill: B2 answers a Join with a Neighbor, then a ForwardJoin for the same peer, relayed back by another member, makes B2 send a second Neighbor (`hyparview.rs:403-409`). The member takes that second one as the reply to its own and does not answer (`:450-452`). B2's entry is never cleared: no timer is armed on this path, and the old connection's idle-timeout close does not reach the protocol while a newer connection exists (`net.rs:575-588, 790-799`).
- The wait ended by the member's own 60 s join timeout (harness `main.rs:352`) in 6/7 cases (the rebind closes the connection, B2 clears the entry); once (1/7) by a shuffle forwarded by B2 from another member at +57.4 s.
- Fixed on iroh-gossip main (c5f44db, same as current head): PR #161 (fcd81ae8, 2026-10-09) clears the pending request on Join and arms a timer on every Neighbor. Not in a release: 0.101.0 (2026-06-15) is the latest on crates.io.
- Counters (n=8 each, kill runs): member-side join timeout 5 s (`E18_JOIN_T_MS=5000`) cuts the wait from 57.4-60 s to 5 s and all-pairs-flowing from median 78.7 s to 38.0 s; gossip-side Join fix at B2 (vendored switch `E18_FIX=1`) gives 0 late joins, median 31.5 s. No-kill control with the member-side counter: 0 rebinds in 3 runs.

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e18-attach`, branch `spike/e18-attach` from `spike/e17-relapse` (83d75bd). Commits `3558c5d` (harness, logs) and `85ef8a2` (archive-dir override); pushed. Rust 1.99.1, debug build, macOS 15.3.1 arm64 (Apple M2 Max), loopback, plain-HTTP relays, SIGKILL of R1+B1 (scenario 1), probers on, 1 s heartbeat, QUIC idle 10 s, preset `Minimal`, 180 s window, W1 T=3000, iroh =1.3.0 (stock, no E14/E17 patch), iroh-gossip =0.101.0 vendored with logging. Load average (1 min) at cell starts 0.46-2.99.
- Diagnostic package `spikes/e18-attach-gossip` (same `src/main.rs` through `[[bin]] path`, `[patch.crates-io] iroh-gossip` -> `vendor/iroh-gossip-0.101.0`, made by `sh vendor.sh`, patch `e18-iroh-gossip-0.101.0.patch`, 93 lines). The patch logs, for every handled message, disconnect, connection close, shuffle timer, pending timer and `send_neighbor`: the peer id (10 hex), B2's active view and `pending_neighbor_requests` (as `E18 handle <Kind> active=[..] pending=[..] from=..`, `E18 send_neighbor .. fresh=<bool>`, `E18 connection_closed ..`). Plus the switch `E18_FIX` (4 lines in `on_join`, see Counter). Binary copied to `spikes/e18-attach-gossip/target/bin/e10-a`.
- Harness diff in `spikes/e10-gossip-main`: `src/main.rs` +3/-1 (`E18_JOIN_T_MS`, default 60000, replaces the fixed 60 s of `subscribe_and_join` at line 352), `run-cell.sh` +3 (tags `x2cw`, `x3cw`), new `e18-run.sh`, `e18-batch.sh`, `e18-archive.sh`, `analyze_e18.py`, `analyze_e18_table.py`.
- Arm (baseline = x1cw of E17: W1 + `E17_FRESH_LOOKUP=1`, the arm with the most late attachments in E17):
  - A0 `x1cw`, runs 1801-1808 (kill), control `x1cw` scenario 0 run 1800.
  - A1 `x2cw` = x1cw + `E18_JOIN_T_MS=5000`, runs 1811-1818; no-kill control scenario 0 runs 1831-1833.
  - A2 `x3cw` = x1cw + `E18_FIX=1`, runs 1821-1828.
- Commands (each run about 185 s, foreground, `sh e18-run.sh <scn> <tag> <run>` sets `E15_BIN`, `E17_ERR_DIR=/tmp/e18err`, `E12_TRACE=iroh_gossip=debug`, runs `./run-cell.sh a <scn> <tag> 1 <run>`, archives into `logs/e18`; `e18-batch.sh <scn> <tag> <run0> <n>` loops it): `sh e18-run.sh 0 x1cw 1800`; `sh e18-batch.sh 1 x1cw 1801 3` (and 1804 3, 1807 2); `sh e18-batch.sh 0 x2cw 1831 3`; `sh e18-batch.sh 1 x2cw 1811 3` (1814 3, 1817 2); `sh e18-batch.sh 1 x3cw 1821 3` (1824 3, 1827 2). `pgrep -fl "e10-a|e18-attach"` printed nothing after every run (checked by the wrapper).
- Order: control 1800, A0 1801-1808, A1 control 1831-1833, A1 1811-1818, A2 1821-1828. Not interleaved, not randomised. Committed logs of E10-E17 not touched; `logs/summary` got only new names (`a-*x[123]cw-r18*`); the tracked `__pycache__/analyze.cpython-314.pyc` that python rewrote was restored with `git checkout`.
- Binary note: A0 and the control 1800 ran on the build before the two switches (`E18_FIX`, `E18_JOIN_T_MS`) existed; A1 and A2 ran on the rebuilt binary (sha256 4ab923da6864bcd6...). With the switches unset the code paths are the same (one env read per Join and per join call). I did not record the earlier binary's hash.
- Analysis, logs/e18 only: `python3 -I analyze_e18_table.py logs/e18` (E15 definitions plus late joins; saved `logs/e18/table.md`), `python3 -I analyze_e18.py logs/e18 --table|--joins <run>...` (saved `endpoints.txt`, `joins.txt`), `--b2 <run> <M1|M2|M3> [lo hi]` (B2 trace lines of one member). Times are seconds after KILL.

## Control first

`x1cw`, scenario 0 (no kill), tracing on, run 1800: 0 REBIND, 0 JOIN_FAIL, 3 JOINED, 1102 probes, heartbeats flow. The vendored logging did not cause rebinds. Counter control, `x2cw` scenario 0, runs 1831-1833: 0 REBIND, 0 JOIN_FAIL, 3 JOINED each (join takes ms; the 5 s timeout never fires).

## Observations per late-attachment case

Definition: a rebound endpoint whose `subscribe_and_join` returned 20 s or more after REBOUND, or timed out (JOIN_FAIL). A0 had 7 of 22 first-generation endpoints (31 rebound endpoints in all; no late join in a later generation). A1 4/22, A2 0/24.

All A0 cases (`logs/e18/joins.txt` from `analyze_e18.py --joins`; old-connection close times from the B2 trace):

| run, member | B2 state at the Join | Join at B2 | old connection close logged at B2 (`remaining 1 other connections`) | wait ended | by |
|---|---|---|---|---|---|
| 1801 M2 | active, pending, no Neighbor | +15.03 | +19.71 | +75.03 (60.0 s) | JOIN_FAIL -> REBIND -> close -> B2 `connection_closed` +75.03 -> clean Join +75.04 |
| 1802 M3 | active, pending, no Neighbor | +14.33 | +24.98 | +74.33 | same |
| 1802 M2 | active, pending, no Neighbor | +16.25 | +16.95 | +76.24 | same |
| 1804 M2 | active, pending, no Neighbor | +12.13 | +14.93 | +72.13 | same |
| 1806 M2 | active, pending, no Neighbor | +13.10 | +14.92 | +73.09 | same |
| 1807 M3 | active, pending, no Neighbor | +12.79 | +13.92 | +72.79 | same |
| 1808 M3 | active, pending, no Neighbor | +12.13 | +14.93 | +69.53 (57.4 s) | a Shuffle from M2 (M2's first shuffle, 60 s after M2's own start at +9.5) relayed by B2 to M3; M3 JOINED ms=57405, M2 `NU M3` 1 ms later; no `connection_closed` for M3 |

Same state in A1 (4 cases: 1812 M2, 1815 M2, 1816 M2, 1818 M1; Join 0.8-4.7 s before the old-connection close; wait 5.0 s each) and A2 (4 cases, answered at once). Across all 11 stale Joins without the fix the Join arrived 0.7-10.7 s before B2 logged the old connection's timeout.

Timeline, run 1801 M2 (B2 log, `--b2 a-1x1cw-1801 M2`; ids named, `Received(..)` lines dropped):
```
-4.06 handle Join       active=[M3,M1] pending=[]            from=M2   -> add to active view M2
-4.06 send_neighbor     active=[M3,M1,M2] pending=[M2]       peer=M2 fresh=true    (Neighbor #1 to M2)
-4.06 handle Neighbor   pending=[M2] from=M2                 (M2's reply; B2 removes M2 from pending)
-4.06 RecvMessage(M1, ForwardJoin(peer M2, ttl 3))   -> on_forward_join: M2 already active
-4.06 send_neighbor     pending=[M2] peer=M2 fresh=true      (Neighbor #2 to M2; M2 never answers it)
-4.06 send_neighbor     peer=M2 fresh=false   (two more ForwardJoin echoes, from M3 and M1)
 0     KILL
15.03 conn established (new M2); RecvMessage(M2, Join)
15.03 handle Join       active=[M3,M1,M2] pending=[M2]       from=M2
15.03 send_neighbor     pending=[M2] peer=M2 fresh=false     <- no Neighbor goes out; B2 sends ForwardJoin(M2) to M3 and M1 only
19.71 conn closed reason=timed out; "remaining 1 other connections"   (old M2 connection; no PeerDisconnected)
54.34 shuffle_timer     pending=[M2]        (B2 shuffles to M2: M2's new Gossip does not answer with a Neighbor)
75.03 conn closed "closed by peer: 0"; "active send connection closed, mark peer as disconnected"
75.03 E18 connection_closed pending=[M2]   -> NeighborDown(M2), pending cleared
75.04 handle Join       active=[M3,M1] pending=[]   -> add to active view, send_neighbor fresh=true -> M2 JOINED ms=8
```
M2's side of the same run (`a-1x1cw-1801.M2.trace.log.gz`, before kill): `handle Neighbor active=[] pending=[] from=B2` -> `send_neighbor peer=B2 fresh=true` (its reply) -> second `handle Neighbor active=[B2] pending=[B2] from=B2` (taken as the reply, `do_reply=false`, no send).

Pre-kill check over the 15 stale cases (all arms): B2 sent 2 fresh Neighbors to the member, received 1 from it, the second send triggered by `handle ForwardJoin` (15/15); the member received 2 Neighbors from B2, took 1 as reply, sent 1 (15/15; `/tmp/e18_origin.py`, `/tmp/e18_peer2.py`, not committed). 

Contrast, non-stale Joins (66 of the 81 post-kill Joins, `joins.txt`): 39 with the member not in B2's active view (the old connection had closed first, `connection_closed` cleared it) and 27 active but not pending: B2 sent a Neighbor at once, `JOINED` within 10 ms. In run 1803 (no late join) M1 was pending at kill (state from the last pre-kill line) and was cleared because its old connection timed out before its Join arrived. B2's last pre-kill state held the member in `pending` in 34 of 72 member-runs; the entry is stale only if the Join arrives before the idle close.

## Mechanism and evidence

Chain, each link observed:
1. Pre-kill, B2 sends the member two Neighbors for one join (Join reply, then a ForwardJoin renewal): `hyparview.rs:380-383` (on_join -> add_active reply=true -> send_neighbor), `:403-409` (on_forward_join: peer already active -> `send_neighbor`), `:745-752` (`send_neighbor` inserts into `pending_neighbor_requests`, sends only if the insert was new). Logged 15/15.
2. The member cannot tell a request from a reply: `on_neighbor` (`:450-452`) treats a Neighbor from a peer in its own pending set as a reply and does not answer. The member's pending entry for B2 was set by its own reply to Neighbor #1, so Neighbor #2 is consumed. B2's entry from #2 stays. Logged 15/15.
3. Nothing clears it. Removal sites: `on_neighbor` (`:451`), `on_disconnect` (`:331`), `handle_connection_closed` (`:347`), the pending timer (`:632-633`, scheduled only by `refill_active_from_passive`, `:627`, not by `add_active`/`send_neighbor`).
4. The kill leaves B2's old connection to the member dead but open; the member's new connection is accepted and becomes the active one while the old one moves to `other_conns` (`net.rs:790-799`). When the old one idles out (10 s) `handle_connection_task_finished` only logs `remaining N other connections` (`net.rs:575-588`) and sends no `PeerDisconnected`. Logged 11/11 cases (the Join came 0.7-10.7 s before that close).
5. The new Join hits `add_active` (`:694-711`): peer in active view, `reply=true` -> `send_neighbor` -> insert returns false -> nothing sent. No Neighbor, no NeighborUp at the joiner. Logged `fresh=false` at 11/11.
6. End of the wait: `connection_closed` for the member (the member's REBIND closes it, 6/7 of A0: B2 removes pending and the active entry, the next Join is a clean add) or a Neighbor request from another member (1/7: 60 s shuffle, B2 relays it). Without the harness's 60 s join timeout I expect the wait to last until a shuffle reaches the member or the member leaves; not measured (every run here has the timeout).

Causal tests: A2 clears the pending entry in `on_join` (the one change main has): 4 stale Joins, 4 answered within 10 ms, 0 late joins in 24 first-generation endpoints, against 7/22 (A0). A1 does not touch the cause; it bounds the wait.

E17's reading is confirmed (no Neighbor because `pending_neighbor_requests` holds the peer) and extended with the cause of the pending entry (the unanswered duplicate Neighbor) and the reason the close does not clear it (second connection). E17's "next Neighbor after the new connection closed" is the member's own REBIND.

## iroh-gossip main check

Head of `n0-computer/iroh-gossip` main is `c5f44db7` (2026-10-20, "refactor!: mark event and command enums ... non-exhaustive (#198)"; queried through the GitHub API; I read the cargo checkout `~/.cargo/git/checkouts/iroh-gossip-733be91e1e16bcf1/c5f44db`). Fixed there. `src/proto/hyparview.rs`:
```rust
549    fn on_join(&mut self, peer: PI, data: Option<PeerData>, io: &mut impl IO<PI>) {
550        // Drop a neighbor request we may still have pending to this peer. The peer
551        // is joining, usually after a restart, so it has no memory of a request we
552        // sent it before and will not answer it. While that request is pending,
553        // `send_neighbor` would send nothing, and the Join would go unanswered.
554        // The request and the Join can also cross in flight: another of the
555        // peer's bootstrap peers forwarded its join to us, and we sent the peer a
556        // request before its own Join reached us. Then the peer gets one Neighbor
557        // more than needed and takes it as the reply to its own.
558        self.pending_neighbor_requests.remove(&peer);
```
and `send_neighbor` (`:1029-1046`) inserts with an id and pushes `OutEvent::ScheduleTimer(self.config.neighbor_request_timeout, Timer::PendingNeighborRequest(peer, id))` on every Neighbor, request or reply (`neighbor_request_timeout` 3 s, was 500 ms; `handle_pending_neighbor_timer` `:900-902` clears only the entry with the same id). Doc comment at `:1020-1028` describes the request/reply ambiguity. The introducing commit is `fcd81ae8` "fix: fix bugs in peer lifecycle management (#161)", 2026-10-09 (parent `b89fa981` has neither; its message has the bullet "answer a join from a peer that is already a neighbor ... A join now clears that pending request, and every Neighbor we send, request or reply, now arms the request timeout"). Present in `f02f79e8` (#178) and `dfeec141` (#191) too (checked by fetching the file at each commit). So the E16 check of #178 was incomplete for this path: the fix is #161, earlier. No release carries it: crates.io latest is 0.101.0 (2026-06-15). Main also has maintenance rejoin (`MAX_BOOTSTRAP_PEERS`, `DoMaintenance`, `dfeec141`) that would re-send Join from an isolated node; I did not test main.

## Counter

Two arms.

A1, member-side, `E18_JOIN_T_MS=5000` (harness, no gossip patch), `main.rs:352`:
```rust
// E18 counter (`E18_JOIN_T_MS`, default 60000): a join that sees no NeighborUp within T ms fails, and W1 then rebinds, which closes the connection to the bootstrap node.
let join_t = std::env::var("E18_JOIN_T_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(60_000);
let topic_h = timeout(Duration::from_millis(join_t), gossip.subscribe_and_join(topic(), boot)).await;
```
(+3/-1). It reuses W1's JOIN_FAIL path: return, `ep.close()` (5 s cap), new endpoint, new `Gossip`, join again. Closing the endpoint makes B2 see `PeerDisconnected` for the member's active connection, which clears the stale entry. Real-member form: the join call returns on the first NeighborUp, so "no NeighborUp within N s" is a thing the member sees. Cost: a full rebind (all connections drop) per stale join; a legitimate slow join under 5 s load would rebind needlessly (0 in the 3 no-kill runs; every non-stale join here took 10 ms or less).

A2, gossip-side, `E18_FIX=1` in the vendored copy (runtime switch, applies in whichever process runs `on_join`; only bootstrap nodes receive Joins, so it is bootstrap-only in effect; main's own change):
```rust
fn on_join(&mut self, peer: PI, data: Option<PeerData>, io: &mut impl IO<PI>) {
    // E18_FIX (runtime switch, diagnostic): as iroh-gossip main (fcd81ae8, #161), a Join drops the neighbor request still pending to the joiner.
    if std::env::var("E18_FIX").is_ok() {
        self.pending_neighbor_requests.remove(&peer);
    }
```
Only the `on_join` part of main; it leaves the duplicate Neighbor (cause 1-2) in place and the missing timer, which are harmless once a Join clears the entry.

### Results (`logs/e18/table.md`)

| Arm | runs | runs stalled | stalled pairs flowing at 180 s | first-gen endpoints | first-gen late joins | late-join wait s | rebinds per member | all stalled pairs flowing from, s min / med / max | per run, s |
|---|---|---|---|---|---|---|---|---|---|
| A0n x1cw no kill | 1 | 0 | - | 0 | 0 | - | 0 | - | - |
| A1n x2cw no kill (counter on) | 3 | 0 | - | 0 | 0 | - | 0 | - | - |
| A0 x1cw kill | 8 | 8 | 46/46 | 22 | 7 | 57.4 / 60.0 / 60.0 | 0-4 | 33.3 / 78.7 / 94.7 | 76.1, 94.7, 33.3, 81.2, 38.8, 83.1, 81.8, 72.5 |
| A1 x2cw kill, `E18_JOIN_T_MS=5000` | 8 | 8 | 46/46 | 22 | 4 | 5.0 / 5.0 / 5.0 | 0-2 | 30.1 / 38.0 / 58.2 | 31.2, 39.4, 31.2, 50.4, 39.0, 58.2, 30.1, 37.0 |
| A2 x3cw kill, `E18_FIX=1` | 8 | 8 | 48/48 | 24 | 0 | - | 1-1 | 21.1 / 31.5 / 33.9 | 31.2, 33.9, 30.1, 33.2, 33.9, 21.1, 30.3, 31.9 |

(Late join = JOINED 20 s or more after REBOUND, or JOIN_FAIL. 1805 had only M2 rebound; the other runs had 3 members rebound.)

- Exact permutation test on the difference of means of "all stalled pairs flowing from" (n=8 each): A0 vs A1 p = 0.005; A1 vs A2 p = 0.022; A0 vs A2 p = 0.0005 (`/tmp/e18_perm.py`, not committed). Medians 78.7, 38.0, 31.5 s.
- Within A0 the two runs without a late join (1803, 1805) healed at 33.3 and 38.8 s; the six with one at 72.5-94.7 s.
- The late-join frequency itself is not changed by A1 (4/22 vs 7/22 is chance at this n; the entry is created before the counter acts); A1 shortens each from 60 s to 5 s. A2 removes them.
- Floor of about 30 s in A1 and A2: the remaining time from kill to all pairs flowing is W1's detection (first rebind 6-25 s after kill), plus member-to-member neighbors forming after a clean join. In A0 run 1802 M1 (first generation) joined B2 at once and still got no heartbeat for its whole generation; that is not a late join and I did not trace it.

## Hypotheses and verdicts

- H1 (E17: B2's `send_neighbor` sends nothing because `pending_neighbor_requests` holds the peer): supported, 15/15 Joins in `active + pending` got no Neighbor; with the pending entry cleared in `on_join`, 4/4 got one.
- H2 (the pending entry comes from an unanswered Neighbor created by a ForwardJoin renewal, which the member takes as the reply to its own): supported, 15/15 pre-kill traces show 2 fresh Neighbors out, 1 received; member side shows 2 received, 1 sent.
- H3 (the old connection's idle close would clear it): refuted for this path; the close is logged as `remaining 1 other connections` and clears nothing (11/11). It clears the entry when the member has no newer connection (39 Joins where B2 had no active entry).
- H4 (E17: the 60 s shuffle ends the wait): partly. 1/7 A0 cases ended by a shuffle (M2's, relayed); 6/7 ended by the harness's 60 s join timeout and rebind. Shuffle interval 60 s (`hyparview.rs:216`).
- H5 (a member-side re-join with a short timeout counters it): supported: wait 60 s -> 5 s, median heal 78.7 -> 38.0 s (p = 0.005, n=8 each), 0 false rebinds in 3 no-kill runs.
- H6 (main fixes it): supported by source (`fcd81ae8`), not run; and A2 shows the `on_join` change alone removes the late joins.
- H7 (C1 raises the late-join frequency: 0/24 W1, 2/18 C2, 4/9 C1 in E17; 7/22 here with C1): not explained. The mechanism does not depend on C1 (its precondition is pre-kill traffic at B2); the frequency depends on whether the Join arrives before B2 notices the old connection died (0.7-10.7 s margins above) and on whether B2 holds the stale entry (34/72 member-runs at the last pre-kill state). I did not run W1 without C1 on this build.

## What could make this wrong

- All runs use C1 (empty lookup at rebind), which E17 found makes late attachment most frequent. Under plain W1 (E17: 0/24) I measured nothing; the mechanism could be rarer or masked there (e.g. rebinds landing after the old connection closed). I did not run the x1w arm with this build.
- The 60 s ending is the harness's join timeout, not a property of the protocol; the answer to "what ends it in a real deployment" is a source reading (steps 3 and 6), not a measurement.
- The logging runs in the protocol path (debug build, file tracing, `E12_TRACE`); timing may differ from E17 runs. A0 ran on a binary built before the two switches existed (hash not recorded); arms are sequential, not interleaved (A0, A1, A2), load 0.46-2.99.
- n=8 per arm; the heal-time differences are from means with an exact permutation test, not a power analysis. The A1 floor (about 30 s) and A2's tail (21-34 s) are W1 plus gossip's own attach and I did not decompose them.
- The "stale at kill" fraction (34/72) is read from the last pre-kill E18 line with a pending list, not the instant of the kill.
- One path only: a bootstrap contact (B2). A member rejoining through B1 was not instrumented (B1 is dead after the kill).
- B2 here is a harness node running the same gossip as the members; the estafette bootstrap binary itself was not run.

## Exact commands to rerun on a Linux host

Branch arrives as a git bundle on top of `spike/e17-relapse`. In the estafette checkout (already has `spike/e17-relapse`):
```
git fetch /path/to/e18.bundle spike/e18-attach:spike/e18-attach
git worktree add ../estafette-e18-attach spike/e18-attach
cd ../estafette-e18-attach/spikes/e18-attach-gossip
sh vendor.sh                       # copies iroh-gossip-0.101.0 from ~/.cargo/registry, applies e18-iroh-gossip-0.101.0.patch
cargo build --features a
mkdir -p target/bin && cp target/debug/e18-attach-gossip target/bin/e10-a
cd ../e10-gossip-main
export E18_LOGDIR=logs/e18-linux   # archive here, so analysis sees only the Linux runs
```
Runs (foreground, about 185 s each; use fresh run numbers; check `pgrep -fl "e10-a|e18-attach"` is empty before each):
```
sh e18-run.sh 0 x1cw 2800                  # control, no kill
sh e18-batch.sh 1 x1cw 2801 8              # A0
sh e18-batch.sh 1 x2cw 2811 8              # A1  (E18_JOIN_T_MS=5000)
sh e18-batch.sh 0 x2cw 2831 3              # A1 no-kill control
sh e18-batch.sh 1 x3cw 2821 8              # A2  (E18_FIX=1)
```
Interleave the three kill arms (e.g. one run of each in turn) on the Linux host if time allows; here they were sequential. Optional, the missing arm: add a tag `x1w` as in E17 (`E15_W1=1`, no C1) and run `sh e18-batch.sh 1 x1w 2841 8`.
Analyze only your own logs:
```
python3 -I analyze_e18_table.py logs/e18-linux
python3 -I analyze_e18.py logs/e18-linux --joins $(ls logs/e18-linux | grep raw | sed 's/.raw.log.gz//' | grep -v a-0x)
python3 -I analyze_e18.py logs/e18-linux --b2 a-1x1cw-2801 M2 -8 80
```
`analyze_e18_table.py` expects the `a-1x1cw|x2cw|x3cw-N` names. `run-cell.sh` writes `logs/summary/a-<scn>x?cw-r<run>.txt`; use run numbers not in `logs/summary` already (2800+) so no committed file is overwritten, otherwise `git checkout` it.

Files: logs `spikes/e10-gossip-main/logs/e18` (raw logs, B2/member traces), `table.md`, `joins.txt`, `endpoints.txt`.
