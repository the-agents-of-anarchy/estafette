# E17 why the W1 rebind relapses (second stall after a first heal), and a member-side counter

Date 2026-10-21 (runs about 01:36-03:45 UTC, 04:33-06:45 local). Experiment E17 for estafette.

## Answer

- Cause of 3 of 3 traced relapses with a second stall (X runs 1, 3, 6; 4 rebound endpoints): the rebound endpoint's R1 queue (R1 = the dead relay) fills to 64 and blocks its `RelayActor` (the E14 block) 57.2-57.4 s after the rebind. Fill = 58 datagrams addressed to M1 over R1, one per second, plus 6 retransmits of the dial to B1. The R1 actor sits in backoff, never drains, and its 60 s inactivity exit cannot come first (it needs the first dial attempt after 60 s).
- Why only some endpoints: the M1-over-R1 feed exists only when the rebound member has no open gossip connection to M1. In 16 first-generation endpoints of M2 and M3 (8 runs): 4 had 0 s of an M1 connection in their first 60 s and all 4 blocked; 12 had 56-60 s of one and none blocked. With a connection, iroh sends on the selected path only; without one it sends each handshake datagram on every known path, including the stale R1 path of M1 (source reading, `remote_state.rs:792-870`).
- Counter C2 (rebind: probe each configured relay with a TCP connect, strip the relays found down from the other members' address-book entries; 24 lines, env `E17_PROBE`) removed the block: 0 `RelayActor` blocks in 8 runs (6 stalled) against 3 relapsing runs with a second block in 8 W1 runs. It did not remove relapses: 2 of 6 stalled C2 runs relapsed (against 3 of 8) by a second mechanism, late attachment (below). Net: the cause is found and removed, relapses are not.
- Second mechanism, seen but not explained: after a rebind the member joins B2 and gets no neighbor for 57-120 s, with no `RelayActor` block. One B2 trace shows B2 received the Join and sent no Neighbor reply. First-generation attach times of 20 s or more: 0 of 24 member-endpoints in W1, 2 of 18 with C2, 4 of 9 with the fresh-lookup variant C1.

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e17-relapse`, branch `spike/e17-relapse` from `spike/e16-redial` (c86fdbf). Commit `83d75bd` (pushed, includes logs under `spikes/e10-gossip-main/logs/e17`, 293 files, 6.8 MB). Harness `spikes/e10-gossip-main` (variant a, iroh-gossip =0.101.0, iroh =1.3.0); diagnostic package `spikes/e17-relapse-gossip` (same `src/main.rs`, iroh 1.3.0 vendored with the E14 diagnostic patch plus `e17-iroh-1.3.0.patch`, 58 lines: instance tag on BLOCKED/UNBLOCKED lines, `E17_ITEMS=1` logs every datagram that finds a non-empty per-relay queue with url and remote id). Rust 1.99.1, debug, macOS 15.3.1 arm64 (Apple M2 Max), loopback, plain-HTTP relays, SIGKILL of the R1+B1 process (scenario 1), probers on, 1 s heartbeat, QUIC idle 10 s, preset `Minimal`, 180 s window, W1 T=3000.
- One run per call, foreground: `sh e17-run.sh <scn> <tag> <run>` (sets `E15_BIN=spikes/e17-relapse-gossip/target/bin/e10-a E14_DIAG=1 E17_ITEMS=1 E17_ERR_DIR=/tmp/e17err E12_TRACE="iroh::socket::transports::relay=debug,iroh_gossip=debug"`, runs `./run-cell.sh a <scn> <tag> 1 <run>`, archives raw log, member stderr, member and B1/B2 tracing into `logs/e17`). Stock-build cells: `sh e17-stock.sh <scn> <tag> <run0> <runs>`.
- Tags (added to `run-cell.sh`): `x1w` W1; `x1nw` W1 probers off; `x1cw` W1 + `E17_FRESH_LOOKUP=1` (C1); `x1pw` W1 + `E17_PROBE=1` (C2); `c0w`, `c1w` W1 and W1 + C1 on the stock binary.
- Runs and order (not interleaved, not randomised): control `x1w` scenario 0 run 901; `x1w` 1-6; `x1cw` 1; stock `c0w` 1-2, `c1w` 1-2; `x1cw` 2; `x1pw` 1; `x1cw` 3 (B1/B2 tracing added before it, so B2 is traced from here on); `x1pw` 2, `x1w` 7, `x1pw` 3, `x1w` 8, `x1pw` 4-8; `x1pw` scenario 0 runs 1-3; `x1nw` 1-3. The counters were added after the first six W1 runs; X runs 7 and 8 were interleaved with Xp. Load average (1 min) at run starts: 0.34-3.64 (host had other users).
- Analysis, logs/e17 only: `python3 -I analyze_e17_table.py logs/e17 --table|--runs` (E15 definitions), `python3 -I analyze_e17.py logs/e17 <run>` (merged per-member timeline), `python3 -I analyze_e17_summary.py logs/e17 <run>...` (one row per rebound endpoint). Outputs saved: `logs/e17/table.md`, `runs.md`, `generations.txt`. I did not overwrite committed E10/E15/E16 logs (all new names; `git status` showed only new files in `logs/summary`; I restored a tracked `__pycache__/analyze.cpython-314.pyc` that python had rewritten).
- `pgrep -fl "e10-a|e17-relapse|e15-diag"` printed nothing after every run (checked by the wrapper).

## Control first

`x1w` scenario 0 (no kill), diagnostic build with full tracing, run 901: 0 REBIND, 0 BLOCKED lines, 6 pairs flowing, max heartbeat gap 1.0 s. The tracing itself did not trigger rebinds. `E17_ITEMS` logged 1647 items in the member that I read (queue length 1, healthy). No-kill with C2: 3 runs, 0 rebinds, 0 pairs stalled.

## Observations per relapsing run (times: s after KILL; M1 homed on R1, M2 and M3 on R2)

Relapses in the 8 W1 diagnostic runs: runs 1, 3, 6 (second rebind and second stall); runs 2, 4, 5, 7, 8 healed once and stayed healed (first heal of all pairs at 14.1-19.0 s).

### Run 1 (`logs/e17/a-1x1w-1.*`)
```
 8.6  M2 old endpoint inst=f0a510 RelayActor BLOCKED           (first, normal E14 block)
11.6  M3 REBIND n=1   14.6 M3 REBOUND   14.6 M1 REBIND   14.7 M2 REBIND   17.7 M1, M2 REBOUND
18.3  M2, M3 receive M1's announce relays=[R2]; no announce with R1 after 10 s
18.7-74.1  M2 PROBE tgt=M1 ok ms=4, once per second (all succeed over R2)
M2 inst=54f0 (rebound 17.67): E17ITEM url=R1 remote=M1 at 17.68, 18.68, 19.69, ... 75.06 (58 lines, period 1.00-1.01 s),
                              url=R1 remote=B1 6 lines (18.67-23.76)   -> 64
75.06 M2 RelayActor BLOCKED inst=54f0   (57.4 s after REBOUND)
72.02 M3 RelayActor BLOCKED inst=207d40 (57.4 s after REBOUND 14.65; same feed: M1 58 lines, B1 6 lines)
77.84 M2 heartbeat silence starts; 81.0 M2 REBIND n=2; 84.1 REBOUND
```
M1 itself: no block; its heartbeats stop at 74.8 (peers blocked); REBIND n=2 at 78.0, heartbeat back 85.15. M3: REBIND n=2 81.0, heartbeat 84.13. M2 gossip trace after REBOUND n=1: connections to B2 and M3 only, never to M1 (the other endpoints that did not block had an M1 connection).
M2 after the second rebind (84.1): `JOINED ms=7` but heartbeats only at 145.2 s; REBIND n=3 101.1, n=4 121.3, n=5 141.5, REBOUND 144.6, heartbeat 145.23. No BLOCKED in generations 2-5 (R1 items: B1 6 each). This is the second mechanism (below), not a block. All pairs flowing from 165.6 s (E15 definition).

### Run 3 (`a-1x1w-3.*`)
M2 only relapsed. M2 REBIND n=1 20.2, REBOUND 23.3 (inst=b05440): R1 items M1 58 (23.26-80.62), B1 6; `BLOCKED` 80.62 (57.4 s). Silence from 91.2; REBIND n=2 94.2. `UNBLOCKED inst=b05440` at 97.06 together with `Inactive, exiting. RELAY_INACTIVE_CLEANUP_TIME=60s` and `Dropped datagram(s): ActiveRelayActor closed` for R1; REBOUND n=2 at 97.2; first heartbeat 98.18. All pairs flowing from 155.3 s (two pairs only at 155.3).

### Run 6 (`a-1x1w-6.*`)
M3 only. REBIND n=1 13.2, REBOUND 16.2 (inst=4053b0): R1 items M1 58 (16.25-73.64), B1 6; `BLOCKED` 73.64 (57.4 s). Silence 83.4, REBIND n=2 86.4, REBOUND 89.4, heartbeat 90.43. All pairs flowing from 147.6 s.

### What the datagrams were and where the R1 address came from
- Fill = M1-addressed datagrams (58 items per blocked endpoint, 1.00 s period equal to the harness prober period; PROBE results ok) + B1-addressed (6, the gossip dial to B1 that fails after 10 s) = 64 = queue size. The logger records only datagrams that find the queue already non-empty, so the first of each fill is not counted; 58 + 6 = 64 is the count with that off-by-one included in the 58.
- Not the lookup at later times: M2 received M1's announce with R2 at 18.3 s and none with R1 afterwards, so the harness `MemoryLookup` entry for M1 had no R1 after 18.3 s, yet the R1 datagrams continued at 1 per second until 75 s. The R1 address for M1 is therefore held inside the rebound endpoint (remote state path set; `remote_state.rs:869-877` `insert_multiple` only adds; `792-870` sends on all known paths when no path is selected). I did not log the path set. It entered in the first second after the rebind, when the lookup still had M1's pre-kill entry (R1): REBOUND at 17.67, first M1/R1 item 17.68, M1's announce with R2 at 18.3.
- Gossip PeerData did not carry R1 for M1: the `add known addrs` lines show R1 only for B1; M1/M2/M3 PeerData has relay_url None or R2.
- Intervention check: C2 (below) strips R1 from M1's lookup entry at rebind; the M1/R1 feed vanished in all 18 first-generation endpoints of the 6 stalled C2 runs (R1 items: B1 6 only).

### Non-relapsing W1 runs (2, 4, 5, 7, 8)
The rebound endpoints of M2 and M3 never blocked (0 BLOCKED lines after any REBOUND in these 5 runs: 10 first-generation endpoints, plus 2 non-blocking first generations in runs 3 and 6: 12 in all, see `generations.txt`). M1-addressed R1 items stayed at 0-7 (they stop when M1's announce replaces the lookup entry), and the M2/M3 endpoints held a gossip connection to M1 for 56-60 s of their first 60 s (run 2 M2 56 s, run 7 M2 60 s, ...). The R1 actor of each such endpoint exited on its own at `Inactive, exiting` 77-108 s after kill without having blocked.

## Cause and its evidence

Chain, each link observed: (1) at rebind the shared lookup still holds M1 at R1; (2) the new endpoint resolves M1 through it and keeps R1 as a known path (inferred; path set not logged); (3) the member has no open connection to M1, so each fresh dial (probers, 1 per second) sends its Initial on every known path, one datagram lands in the R1 queue per second; (4) the R1 `ActiveRelayActor` is in backoff and does not drain (E14 report section 5, item 4; observed: queue stayed at 64, backoff delays 22 s and 23 s at the time); (5) 58 + 6 = 64 reached 57.2-57.4 s after rebind; (6) `RelayActor` BLOCKED (E14 diag line) and every datagram of the endpoint stops, heartbeats at the member stop 2.8-10.6 s later (heartbeats of other peers keep arriving for a while), W1 rebinds 3.0-3.2 s after the silence starts.
Why the 60 s inactivity exit does not save the endpoint: it happens at the first dial attempt after the 60 s mark; the dial attempts of the blocked endpoints were at 73.47 s (M2 run 1) and then 96.6 s; the fill came at 75.06 s, 57.4 s after the actor's creation.
Predictor: m1conn = seconds with an open gossip connection to M1 in the first 60 s after REBOUND. First-generation endpoints of M2 and M3, 8 W1 runs (16 endpoints): m1conn 0 s -> blocked 4/4; m1conn 56-60 s -> blocked 0/12.
Not shown: why some rebound members lack the M1 connection. Gossip's neighbor choice is random; I did not test a forced connection.
The relapse is not specific to M1's address: any peer on a dead relay whose entry the lookup still holds would do the same (the harness has only M1 and B1 on R1; B1 gives 6 per dial and is not enough alone).

## Counter

Smallest member-side change that removes the cause, inside the member (no iroh or gossip patch): `E17_PROBE=1`. At each rebind, after the old endpoint closes: TCP-connect (1 s timeout) to host and port of each configured relay URL; relays that refuse or time out form `down`; remove those URLs from the lookup entries of the other members before binding. The signal is the member's own probe of the relay URLs from its configuration, a thing a real member has (it does not read `RelayActor` state, which the public API does not show; the existing W1 trigger, heartbeat silence, decides when). In the runs the probe reported `down=[R1]` at every rebind (example: `25.2 down=[59765]`, R2 up).
Diff (`git diff -U0 spikes/e10-gossip-main/src/main.rs`, C2 part, 24 lines):
```rust
async fn relay_up(u: &RelayUrl) -> bool {
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else { return true };
    timeout(Duration::from_secs(1), tokio::net::TcpStream::connect((host, port))).await.is_ok_and(|r| r.is_ok())
}
fn scrub(lk: &MemoryLookup, down: &HashSet<RelayUrl>, k: u8) {
    for id in (1..=3u8).filter(|t| *t != k).map(|t| key(10 + t).public()) {
        let Some(info) = lk.get_endpoint_info(id) else { continue };
        let mut addr = info.into_endpoint_addr();
        addr.addrs.retain(|a| !matches!(a, TransportAddr::Relay(u) if down.contains(u)));
        lk.set_endpoint_info(addr);
    }
}
// in member(), after ep.close():
if std::env::var("E17_PROBE").is_ok() {
    let mut down: HashSet<RelayUrl> = HashSet::new();
    for u in [&u1, &u2] { if !relay_up(u).await { down.insert(u.clone()); } }
    ev(&name, "PROBE_RELAYS", format!("down=[{}]", urls_str(down.iter())));
    scrub(&lk, &down, k);
}
```
B1's address (R1) is not scrubbed (`session` re-adds it): 6 items per dial remain. Variant C1 (`E17_FRESH_LOOKUP=1`, 4 lines): new empty `MemoryLookup` at each rebind.
Other diff in `main.rs` (not counters): `E17_ERR_DIR` keeps child stderr; `init_trace` also in `boot_node`. Total `main.rs` diff +40 / -3.

### Results (diagnostic build, all runs traced; `logs/e17/table.md`)

| Arm | runs | runs stalled | runs with a second RelayActor block | runs with a 2nd rebind | stalled pairs flowing at 180 s | all stalled pairs flowing from, s min / med / max (stalled runs) | first-generation endpoints attaching after 20 s or more |
|---|---|---|---|---|---|---|---|
| X: W1 | 8 | 8 | 3 (runs 1, 3, 6; 4 endpoints) | 3 | 48/48 | 14.1 / 18.8 / 165.6 (n=8) | 0/24 |
| Xp: W1 + C2 (probe + scrub) | 8 | 6 (runs 4 and 8 never stalled) | 0 | 2 (runs 1, 7) | 36/36 | 22.1 / 32.5 / 160.4 (n=6) | 2/18 (xp-1 M1 120.3 s, xp-7 M3 60.4 s) |
| Xp0: C2, no kill | 3 | 0 | 0 | 0 | - | - | - |
| Xc: W1 + C1 (fresh lookup), exploratory | 3 | 3 | 0 | 1 | 18/18 | 72.1 / 75.0 / 86.3 (n=3) | 4/9 |
| Xn: W1, probers off, exploratory | 3 | 2 (one member rebound in each) | 0 | 0 | 4/4 | 26.2 / 29.8 / 33.4 (n=2) | 0/2 |
| stock build W1 (supplementary) | 2 | 2 | not traced | 0 | 12/12 | 15.1 / 15.1 / 15.1 | not traced |
| stock build W1 + C1 (supplementary) | 2 | 2 | not traced | 1 (run 2: 91.0 s) | 12/12 | 21.9 / 56.4 / 91.0 | not traced |

- Block outcome: M2 and M3 first-generation endpoints blocked 4/16 in X, 0/12 in Xp (Fisher exact two-sided p = 0.11); all endpoints in Xp, including second generations: 0 of 25 blocked. Every Xp first-generation endpoint shows R1 items = B1 6 only.
- The counter did not reduce relapses at run level: second rebind in 3/8 (X) against 2/6 stalled runs (Xp); both Xp relapses are late attachment, not a block. First heals were slower with C2 (median all-flowing-from 32.5 s against 18.8 s; ranges overlap; n=6 against 8).
- I asked for n=8 each: the diagnostic-build arms X and Xp are n=8 (Xp 6 stalled: the stall onset is bimodal, runs 4 and 8 never stalled). The stock-build arms are n=2 each, and use C1, not C2; do not read them as n=8.

## Second mechanism: late attachment after a rebind (observed, not explained)

Shape: the member rebinds, `JOINED` (`subscribe_and_join`) returns or does not, B2's active view/Neighbor reply does not arrive, and the member gets no heartbeat for 57-120 s; no `RelayActor` block in that endpoint. Instances: X run 1 M2 generations 2-5 (all end at about 145 s after kill, `101.1 + 44.2`, `121.3 + 23.9`, `84.1 + 61.1`: one external event, not the rebinds); Xp run 1 M1 (7 rebinds, heartbeat at 145.6 s); Xp run 7 M3 (60.4 s); Xc runs 1, 2, 3 (57.1, 57.5, 60.3 s). In two traced cases the first neighbor came 57 s after the join (xc-1 M2: Join sent 14.05, first NeighborUp 71.02; xp-7 M3: Join 18.85, first dial to M1 76.07); hyparview's `shuffle_interval` is 60 s (`proto/hyparview.rs:216`); I did not verify that the shuffle is what ends the wait.
One B2 trace (Xp run 7, `a-1x1pw-7.B2.trace.log.gz`): M3 was added to B2's active view at -4.10 and not removed before its rebind; Join from the new M3 connection received at 18.86; B2 logged no `add to active view other=M3` and sent no `Neighbor` to M3 (it sent ForwardJoin to M1 and M2); the next `Neighbor` to M3 at 78.87, after the new connection closed. Same trace for M1 and M2 at 16.08 and 16.13: both lines present (their entries had been removed). Reading, not instrumented: `add_active` for an already-active peer calls `send_neighbor`, which sends nothing when `pending_neighbor_requests` already holds the peer (`hyparview.rs:694-720, 740-752`); B2 sent a second Neighbor to M3 at -4.09 that no reply clears. W1 run 7 shows the opposite case at B2 (M3's entry removed at 7.76 before its Join at 10.86: clean add and Neighbor).
Why it appears more with the counters is not established. C1 and C2 leave the rebound member (and, for C2, its peers' entries for M1) without addresses; at B2 the cause is B2's stale entry, which the counters do not touch. I have 0/24 against 2/18 (p = 0.18) and 4/9 (p = 0.003); n is small.

## Hypotheses and verdicts

- H1 (E15 note 5: the shared lookup holds M1/B1 addresses with the dead R1 URL, the new endpoint sends toward R1 and its RelayActor blocks again): supported, with a refinement. M1's entry gives the fill (58 of 64); B1's gives 6 per dial. The stale R1 persists in the endpoint after the lookup entry is replaced (M1's announce at 18.3 s); I infer it is the endpoint's path set, not logged.
- H2 (the 60 s relay actor exit or UNBLOCKED coincides with the second heal): not supported as a cause. One UNBLOCKED line in 4 blocked endpoints (run 3 M2, 97.06 s), simultaneous with the 3 s `ep.close()` timeout after REBIND (94.2 + 3.0) and the R1 actor's `Inactive, exiting`; the heal followed the rebind by 1.0 s. In the other three the old endpoint was closed, no UNBLOCKED line. I cannot separate "heal by rebind" from "heal by unblock" in run 3; in runs 1 and 6 the heal followed the rebind.
- H3 (the non-relapsing W1 endpoints never block): supported for 12/12 M2 and M3 first generations; M1's own endpoints cannot show the M1-addressed feed.
- H4 (a lookup scrub of dead relay URLs removes the relapses): contradicted. It removes the second block (0 of 25 Xp endpoints) and not the relapses (2/6 stalled runs).
- H5 (the counters induce the late attachment): not established (0/24 against 2/18, p = 0.18, for C2; 0/24 against 4/9, p = 0.003, for C1).
- H6 (probers are the feed): supported for the rate (1.00 s period equals the prober period, probes succeed) and not tested for absence: the probers-off arm has 3 runs, 2 stalled, one rebound member each, which says nothing about a second block. A real member's rate of fresh dials to peers without a live connection is unknown.

## What could make this wrong

- The 1 per second feed is the harness probers' (E8 probers on, as E15/E16). Without probers a real member may send far fewer datagrams over the stale path, and the fill could take longer than the 60 s exit. I did not measure that (Xn has no relevant data).
- The path-set claim is a reading of `remote_state.rs` (792-877, 931) plus the intervention (C2 removes the feed); I did not log the set or read the whole remote map.
- The queue logger records datagrams that find the queue non-empty. Counting is lower-bound; 58 + 6 = 64 matches the queue size and the block times (57.2-57.4 s), the match is exact in all 4 endpoints, which I read as support, not proof of the composition.
- `m1conn` is derived from gossip `connection established/closed` lines for peer M1; the endpoint may hold other QUIC connections to M1 (probers) which do not select a path for long.
- n: 8 W1 runs, 3 relapsing; first-generation counts 16 and 12. Arms ran in sequence on a host with other users (load 0.3-3.6). Onset is bimodal (Xp runs 4 and 8, Xn run 2 never stalled).
- Tracing at debug level changes timing; both X and Xp carry it, the stock build arms do not (n=2, C1 only).
- Relapse in runs 1, 3, 6 was classified from block lines on M2/M3; M1 never blocked in those runs. Relapse in Xp and Xc is classified by absence of any BLOCKED line and a late first heartbeat; I did not trace B2 for those besides Xp run 7 and nothing for Xc 1 and 2 (B2 tracing started at Xc run 3).
- The second mechanism's explanation is source reading plus one B2 trace; `pending_neighbor_requests` was not logged.
- Debug build, loopback, plain HTTP, SIGKILL, 3 members, QUIC idle 10 s, one host (macOS). B2 is a bootstrap node with no address lookup and relay map R2 only; scenario 1 only.
- `__pycache__/analyze.cpython-314.pyc` in the repo is tracked; python rewrites it. I restored it.

## Exact commands to rerun on a Linux host

Branch arrives as a git bundle on top of `spike/e16-redial`; the host reaches crates.io and GitHub; analyze only `logs/e17`. The archives under `logs/e17` are macOS runs: move them away or write new runs with run numbers above 100, and pass only your own files to the analysis (copy them to a fresh directory).

```sh
git fetch /path/to/e17.bundle spike/e17-relapse:spike/e17-relapse && git checkout spike/e17-relapse
cd spikes/e10-gossip-main && cargo build --features a >/dev/null        # warms the registry (iroh, iroh-gossip)
cd ../e17-relapse-gossip && sh vendor.sh && cargo build --features a && mkdir -p target/bin && cp target/debug/e17-relapse-gossip target/bin/e10-a
grep -c PROBE_RELAYS target/bin/e10-a || true                                 # binary carries the counter; strings -a if grep says binary
cd ../e10-gossip-main && mkdir -p /tmp/e10 && mkdir -p ../../e17-linux && git mv -k logs/e17 logs/e17-macos 2>/dev/null || mv logs/e17 logs/e17-macos
mkdir -p logs/e17 && pgrep -fl e10- ; true                                    # must print nothing
# one run per call (each about 185 s), foreground. Control first: no kill, must show 0 REBIND and 0 BLOCKED
sh e17-run.sh 0 x1w 901
# W1 baseline: need at least 3 relapsing runs (about 1 in 3 on macOS, 1 in 8 on Linux in E15); cap at 12
sh e17-run.sh 1 x1w 1       # ... run numbers 1..12; stop at 3 relapses (a run with a member REBIND n=2)
# per run: timeline and per-endpoint table
python3 -I analyze_e17.py logs/e17 a-1x1w-1 | grep -E "BLOCKED|REBIND|REBOUND|SILENCE|RESUME|^   [0-9a-f]+ R1"
python3 -I analyze_e17_summary.py logs/e17 a-1x1w-1 a-1x1w-2 a-1x1w-3     # columns: blocked_s, r1_items, m1conn_s, heal_first_hb_s
# counter C2, n=8, and no-kill control n=3
sh e17-run.sh 1 x1pw 1      # ... 1..8
sh e17-run.sh 0 x1pw 1      # ... 1..3
python3 -I analyze_e17_table.py logs/e17 --table
python3 -I analyze_e17_table.py logs/e17 --runs
# exploratory: C1 (x1cw), probers off (x1nw): same wrapper
sh e17-run.sh 1 x1cw 1; sh e17-run.sh 1 x1nw 1
```
The wrapper needs `pgrep` clean before each run (run-cell.sh aborts otherwise). Expect on Linux (E15): fewer relapses (1 of 8); the diagnosis check is the same: a relapse is a run with a member `REBIND n=2`; then `analyze_e17_summary.py` gives `blocked_s` about 57 s and `m1conn_s` 0 for the blocked endpoint.
