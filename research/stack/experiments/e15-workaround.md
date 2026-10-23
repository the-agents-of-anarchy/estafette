# E15 member-side workaround for the relay-death gossip stall (C16)

Date 2026-10-21 (runs 21:58-23:56 UTC, 00:58-02:56 local). Experiment E15 for estafette. Question: which member-side rule restores gossip heartbeats after a relay host dies, how fast, at what cost; and does gossip heal itself once the transport is unblocked.

## Answer

- W1 (rebind: close the endpoint, bind a new one with the same key and relay map, new `Gossip`, re-subscribe through B1+B2 when no heartbeat from any peer for T ms) restored all stalled pairs: 30/30 (A2, T=3 s), 30/30 (A3, W1+L1), 16/16 (A2b, T=10 s) flowing at 180 s, against 0/24 (A0) and 2/24 (A1, L1+L2).
- Speed: first rebind 7.0-23.8 s after the kill (A2, A3), then a median 4.1-4.5 s from rebind to first heartbeat per pair. In 7 of 13 scenario-1 W1 runs (A2 runs 2, 4, 5; A3 runs 1-4) all stalled pairs flowed again 14.1-31.3 s after the kill. A2b run 1 (T=10 s, only M2 rebound): 61.1 s. In 5 of 13 W1 runs a second stall began and a second rebind (62-108 s after the kill) healed it: all pairs flowing again at 91.2-169.5 s in those runs (cause not found).
- Cost: 1-2 rebinds per member per run (max 2), all connections dropped at each rebind; 0 false rebinds (3 no-kill runs of 180 s, plus 0 rebinds before the kill in the 13 W1 kill runs); only T=3 s was run in the no-kill control.
- Gossip does not heal itself after the transport is unblocked, and L2's rejoin does not either. In all 8 stalled A0/A1 runs fresh dials worked again for at least one pair 72-161 s after the kill, while heartbeats resumed in 2 of the 48 stalled directed pairs. Source reading plus a trace says the cause is in iroh-gossip 0.101.0 (no further dials once the dial of a non-active peer failed), not in the transport. W1 works because it replaces both the endpoint and the `Gossip` state; I did not separate the two.
- With the transport fixed (diagnostic `E14_FIX=drain`) the stall never starts: 0/6 runs stalled (A4, A4b), against 3/3 with the same build and the fix off (A4c).

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e15-workaround`, branch `spike/e15-workaround` from `spike/e14-bug-report` (983118f). Harness and runs commit `2e7a1a4`; `37b0a1a` adds `analyze_e15_delta.py` (both pushed; branch head `37b0a1a`). Package `spikes/e10-gossip-main`, variant `a` (iroh-gossip =0.101.0), iroh =1.3.0, iroh-relay =1.3.0, Rust 1.99.1, debug build, macOS 15.3.1 arm64 (Apple M2 Max), loopback, plain-HTTP relays, SIGKILL of the R1+B1 process (E10/E12 scenario 1). Cargo.lock unchanged; no new crates.
- Binary: `cargo build --features a`, copied to `target/bin/e10-a` (sha256 11e792d3...). A0 and A1 ran this same binary with `E15_W1` unset, so the control includes the refactor (see What could make this wrong).
- A4 binary: package `spikes/e15-diag-gossip` (same `src/main.rs` through `[[bin]] path`, `[patch.crates-io]` to `spikes/e14-repro-patched/vendor/iroh-1.3.0` made by `sh vendor.sh`, patch `e14-iroh-1.3.0.patch`). Runtime switch `E14_FIX=drain`; unset = stock behavior (A4c).
- Window 180 s after the kill, probers on, `E8_PAYLOAD=1`, 1 s heartbeat, QUIC idle 10 s, preset `Minimal`. Stalled = directed heartbeat pair (6 per run) with a gap >= 5 s (E10 definition).
- Commands (one `ctl` call per cell, foreground, each run about 185 s; run numbers shown):
  - A0 `./run-cell.sh a 1 w 2 1`, `./run-cell.sh a 1 w 3 3`
  - A1 `./run-cell.sh a 1 l12w 3 1`, `./run-cell.sh a 1 l12w 2 4`
  - A2 `./run-cell.sh a 1 w1w 2 1`, `./run-cell.sh a 1 w1w 3 3`
  - A2b `./run-cell.sh a 1 w1w10 3 1`
  - A2c (no kill, scenario 0) `./run-cell.sh a 0 w1w 3 1`
  - A3 `./run-cell.sh a 1 w1l1w 3 1`, `./run-cell.sh a 1 w1l1w 2 4`
  - A4 `E14_FIX=drain E15_BIN=<abs>/spikes/e15-diag-gossip/target/bin/e10-a ./run-cell.sh a 1 pw 3 1`; A4b same with tag `pl12w`; A4c same binary, `E14_FIX` unset, tag `pc`.
  - Diagnostic traced runs (excluded from tables): `E12_TRACE="iroh_gossip=debug" ./run-cell.sh a 1 w 1 101` and `... l12w 1 101`.
  - Tags: `l12w` = `E12_L1=1 E12_L2=1`; `w1w` = `E15_W1=1`; `w1w10` = `E15_W1=1 E15_W1_T_MS=10000`; `w1l1w` = `E15_W1=1 E12_L1=1`. All with `E10_WINDOW_MS=180000`.
  - Analysis: `sh e15-archive.sh <prefix>` (gzips `/tmp/e10/*.raw.log` into `logs/e15/`), `python3 -I analyze_e15.py logs/e15 --table|--runs`, `python3 -I analyze_e15_delta.py logs/e15`.
- Order: A0, A1, A2, A2b, A2c, A3, A4, A4b, A4c, then the two traced runs; not interleaved, not randomised. 35 table runs plus 2 traced. Load average (1 min) at cell starts, from `logs/summary/*.txt`: 0.5 to 4.4 (3.9 at the first A0 cell; the host had other users and work). Cell starts and loads: `logs/e15/summary/`, `logs/summary/`.
- Stray process check: `pgrep -fl "e10-a|e15-diag"` empty after the last cell and again before the final return. Note: running `run-cell.sh a 1 w 2 1` overwrites the committed E10 file `logs/summary/a-1w-r1.txt`; I restored it with `git checkout` and kept mine as `logs/e15/summary/a-1w-r1-e15.txt`.

## Results

Per arm (`logs/e15/table.md`, per run `logs/e15/runs.md`). Times in seconds after the kill. "Stalled pairs" counts directed pairs of M1-M3 (6 per run).

| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | heal time per pair, min / median / max | per run: all stalled pairs flowing from, min / median / max | first rebind, min / median / max | rebinds per member | false rebinds |
|---|---|---|---|---|---|---|---|---|---|
| A0 control, no levers | 5 | 4/5 | 24 | 0 | - | - | - | 0 | 0 |
| A1 L1+L2 | 5 | 4/5 | 24 | 2 (run 4 only) | 64.1 / 73.6 / 83.1 (n=2) | - | - | 0 | 0 |
| A2 W1 T=3000 | 5 | 5/5 | 30 | 30 | 13.1 / 24.0 / 150.5 | 14.1 / 27.7 / 150.5 | 7.0 / 15.9 / 21.0 | 1-2 | 0 |
| A2b W1 T=10000 | 3 | 3/3 | 16 | 16 | 26.0 / 39.5 / 169.5 | 61.1 / 161.4 / 169.5 | 17.9 / 19.2 / 36.6 | 0-2 | 0 |
| A3 W1 T=3000 + L1 | 5 | 5/5 | 30 | 30 | 15.1 / 20.1 / 91.2 | 16.1 / 21.1 / 91.2 | 9.0 / 13.1 / 23.8 | 1-2 | 0 |
| A2c W1, no kill | 3 | 0/3 | 0 | 18/18 flowing | - | - | - | 0 | 0 |
| A4 diag transport (drain), no levers | 3 | 0/3 | 0 | 18/18 flowing | - | - | - | 0 | 0 |
| A4b diag transport (drain), L1+L2 | 3 | 0/3 | 0 | 18/18 flowing | - | - | - | 0 | 0 |
| A4c diag build, fix off | 3 | 3/3 | 18 | 0 | - | - | - | 0 | 0 |

Heal time = first heartbeat at the receiver more than 5 s after the last heartbeat before the pair's longest gap. A run is "stalled" when any pair has a gap >= 5 s. In A2, A2b, A3 every run stalled, so all pairs of a run are listed.

Rebind-to-heartbeat (`analyze_e15_delta.py`, per pair, from the later of the two members' first rebind to the first heartbeat after it): A2 30 pairs, min 3.0 / median 4.1 / max 32.1 s; A2b 12 pairs, 3.0 / 4.5 / 5.0; A3 30 pairs, 3.0 / 4.1 / 63.5. `REBOUND ms` (time to bind only; relay connection and join not included) 6-7 ms.

Relapses (second stall episode after a first heal, then a second rebind): A2 runs 1 and 3 (second rebind at 62.2 s by M1; 89.3 s by M3), A2b runs 2 and 3 (103.2 s, 108.3 s by M2), A3 run 5 (90.7 s by M3): 5 of 13 W1 scenario-1 runs. In those runs "all flowing from" is 91.2-169.5 s (101.3, 150.5, 161.4, 169.5, 91.2). A2b run 1 is not a relapse: only M2 rebound (36.6 s), 4 pairs stalled and healed at 39.5 and 61.1 s.

Per-run detail: `logs/e15/runs.md`. A0 run 5 and A1 run 2 did not stall (bimodal, as E10/E12).

Fresh dial versus heartbeat in the stalled A0 and A1 runs (n=8 stalled runs; computed from the PROBE and RX records of `logs/e15/a-1w-[1-4]`, `a-1l12w-[1,3,4,5]`): the first fresh dial ok after the stall was at 72-161 s after the kill for 44 of the 48 stalled directed pairs (the 4 M1 pairs of A0 run 2 had no ok in 180 s). Heartbeats after that moment: resumed in 2 of those 44 pairs (M1<->M3 in A1 run 4, heal 64.1 and 83.1 s); 0 in the other 42.

## Mechanism notes

Claims from my own reading of iroh-gossip 0.101.0 (`~/.cargo/registry/src/*/iroh-gossip-0.101.0/src`) and two traced runs. Claims about iroh internals are the E14 report's (`actor.rs` lines there); I did not re-read them.

1. After a dial fails, gossip does not dial that peer again from the same `Gossip` instance. `net.rs:436-443`: on dial failure, if the peer is not `PeerState::Active`, only `InEvent::PeerDisconnected` goes to the protocol; the actor's `peers` entry is untouched. The only removal of a `peers` entry is `net.rs:732-736` (`OutEvent::DisconnectPeer`). The protocol emits `DisconnectPeer` in `remove_active_by_index` (`proto/hyparview.rs:648-655`) for active-view peers; for a peer not in the active view, `handle_connection_closed` (`proto/hyparview.rs:346-354`) only removes it from the passive view and peer data. A dial starts only when the pending queue is empty (`net.rs:695-699`, `if queue.is_empty()`). Reading: a non-active peer whose dial failed stays `Pending` with a non-empty queue, so later sends are queued and never dialed. Not instrumented (the `peers` map is not logged).
2. Trace support (`logs/e15/diag/a-1l12w-101-M{1,2,3}.trace.log.gz`, filter `iroh_gossip=debug`, L1+L2 on, one run): each member made 13 `JoinPeers` calls over 180 s and logged exactly 4 `start to dial` events, the last at 44.2 s (M2), 31.0 s (M3), 31.0 s (M1) after the kill, and the last `dial failed` at 54.2 / 41.0 / 41.1 s. No dial start after that, while fresh endpoint dials from the probers succeeded from about 70 s on. Consistent with note 1; does not prove it.
3. Without L2 (run `a-1w-101`, raw log `logs/e15/diag/a-1w-101.raw.log.gz`; its gossip trace was overwritten by the next traced run and is not archived, so these figures are from my read of it and can not be rechecked from the files): the connections timed out at 10-49 s, all three members emitted `PeerDisconnected` and `DisconnectPeer` for each, and the last `SendMessage` was at 23 s (M2), 48 s (M3), 24 s (M1). Zero `start to dial` lines. With an empty active view and nothing to dial, gossip sends nothing; no rule in the harness restarts it.
4. Why W1 heals: a rebind builds a new `Endpoint` (new socket and, per the E14 report `actor.rs:1360`, new per-relay queues and a new `RelayActor` with an empty 256-slot channel) and a new `Gossip` with empty `peers`, then `subscribe_and_join` dials B1 and B2 again. Heartbeats returned a median 4.1-4.5 s after the rebind (min 3.0 s; 72 pairs in three arms; tail 32 s and 63 s when the first rebind did not take). Which of the two (new transport, new gossip state) matters is not separated: no arm rebuilds only `Gossip` on the old endpoint.
5. Relapse after a first heal (5 of 13 W1 runs): not explained. The new endpoint still shares the harness `MemoryLookup`, which holds B1 and (until re-announced) M1 addresses carrying the dead R1 URL, so datagrams toward R1 can refill the new queue (E14 report section 5; item 7 describes the refill on actor exit). I did not trace a relapse. L1's `remove_relay` does not stop sends to a removed relay (E14 report section 5), so A3's dead-set (applied to the rebound relay map at the one member where L1 fires, M1) is a weak lever.
6. A4: `drain` makes the dead relay's actor discard queued datagrams while it backs off (E14 report section 6). In the harness it removed the stall in 6/6 runs (A4, A4b) while the same build with the switch off stalled 3/3 (A4c). Heartbeats were continuous (all 18 pairs flowing at 180 s in each A4 run): the question "transport unblocked, does gossip heal itself" cannot be asked there, because the transport never blocks. The answer comes from the A0/A1 data above, where the transport unblocks late (after the actor exit, 72-161 s) and gossip does not recover.

## Harness change (diff, lines added)

`spikes/e10-gossip-main/src/main.rs`: +51 / -16 lines (`git diff 983118f 2e7a1a4 -- spikes/e10-gossip-main/src/main.rs`). `member` is split in two: `member` binds, then loops over `session`; `session` is the old body from the topic setup on, with its tasks in a `JoinSet` (aborted when `session` returns). Added logic:

```rust
// in member(), after the first bind:
let w1 = std::env::var("E15_W1").is_ok().then(|| std::env::var("E15_W1_T_MS").ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(3000));
let dead: Arc<Mutex<HashSet<RelayUrl>>> = Arc::default();
let (mut last_rebind, mut n) = (None, 0u32);
loop {
    let silent = session(name.clone(), k, order.clone(), u1.clone(), u2.clone(), lk.clone(), ep.clone(), dead.clone(), w1, last_rebind).await?;
    n += 1;
    last_rebind = Some(Instant::now());
    ev(&name, "REBIND", format!("n={n} silent_ms={silent}"));
    let _ = timeout(Duration::from_secs(5), ep.close()).await;
    let urls: Vec<RelayUrl> = [&u1, &u2].into_iter().filter(|u| !dead.lock().is_ok_and(|g| g.contains(*u))).cloned().collect();
    let t = Instant::now();
    ep = bind_member(k, &urls, &lk, None).await?;
    ev(&name, "REBOUND", format!("n={n} ms={} relays=[{}]", t.elapsed().as_millis(), urls_str(ep.addr().relay_urls())));
}
// at the end of session():
let Some(t_ms) = w1 else { sleep_forever().await?; return Ok(0) };
loop {
    sleep(Duration::from_millis(250)).await;
    let silent = now_ms().saturating_sub(u128::from(last_hb.load(Ordering::Relaxed)));
    if silent >= t_ms && last_rebind.is_none_or(|r| r.elapsed() >= Duration::from_secs(20)) {
        return Ok(silent);
    }
}
// L1 records the removed relay: after L1_FIRE, dd2.lock().insert(url.clone());
// JOIN_FAIL: with W1 on, return Ok(0) (rebind) instead of sleeping forever.
```
Env: `E15_W1`, `E15_W1_T_MS` (default 3000). Debounce: no more than one rebind per 20 s per member (checked in the trigger). `last_hb` is set at `JOINED` of each session, so the silence clock restarts after every rebind. The `MemoryLookup` is kept across rebinds. Also changed: `run-cell.sh` (new tags, `E15_BIN`), new `analyze_e15.py`, `analyze_e15_delta.py`, `e15-archive.sh`, package `spikes/e15-diag-gossip`.

## Hypotheses and verdicts

- H1 A member-side rebind on heartbeat silence restores heartbeats: supported. 76/76 stalled pairs flowing at 180 s over A2, A2b, A3 (13 runs), 0/24 (A0) and 2/24 (A1) without it. Timing is not uniform: 7 of 13 runs fully healed by 31.3 s, 1 at 61.1 s, 5 of 13 relapsed and healed at 91-170 s.
- H2 Longer T is a cost, not a gain: not shown. T=10 s: first rebind median 19.2 s against 15.9 s at T=3 s, but 2 of 3 runs relapsed and all-flowing-from median was 161 s against 27.7 s. n=3 against n=5, ranges overlap in the first rebind; I can not separate T from run-to-run variation.
- H3 W1 gives false positives in a healthy mesh: not seen at T=3 s (0 rebinds in 3 x 180 s with 3 members, 0 before the kill in the 13 W1 kill runs). A healthy mesh had max heartbeat gap about 1.1 s in E10/E12, so 3 s is 2+ missed heartbeats; n=3, one T, no flapping or load test.
- H4 L1 adds speed to W1 (A3 against A2): not distinguishable at n=5 (all-flowing-from median 21.1 against 27.7 s, ranges 16.1-91.2 against 14.1-150.5). I ran A3 although A2's numbers do not contain an L1 effect: W1 alone already rebinds with the full relay map, and L1 only changes the map of the member whose home died.
- H5 The stall is the transport block alone (at the harness level): supported. `drain` 0/6 runs stalled; same build, fix off 3/3; stock A0 4/5.
- H6 Once unblocked, gossip heals itself: contradicted. 2 of 48 stalled directed pairs, fresh dials working at 72-161 s, no further gossip dials in the trace.
- H7 L2's rejoin heals once the transport works: contradicted (2/24, one run; 26-39 `join_peers` calls per run in the stalled A1 runs). Consistent with E12.

## What could make this wrong

- n is 3-5 per arm and stall onset is bimodal. A0 and A1 stalled 4/5 each; A2, A2b, A3 stalled in every run (13/13), which is not an effect of W1 (the stall happens before the first rebind). Counts like 30/30 are pair counts inside 5 runs, correlated within a run; the run count is the honest n.
- Arms ran in sequence, not interleaved, over 2 h on a host with other users (load 0.5-4.4 at cell starts). A0 and A1 ran the refactored binary (`JoinSet` session), not the E12 binary; A0 and A1 results match E12 and E10 (0/62 healed there), which is the only check I did.
- "Heal" is a heartbeat at a receiver; it says nothing about the delivery of other messages, and I did not check payload or ordering. A pair that heals and relapses counts once at 180 s if it flows at the end.
- The W1 rebind reuses the harness `MemoryLookup`, still holding addresses with the dead relay URL. A real member's address sources differ; the relapse may depend on this and I did not test a scrubbed lookup.
- The rebound relay map is not reduced for M2 and M3 (L1 fires only at the member whose home died), so the "minus any relay seen dead" part of W1 was exercised at M1 in A3 only.
- `REBOUND ms` is bind time only; the endpoint was not awaited to be online and the join time was not analysed (JOINED events are in the raw logs).
- Relapse cause unknown (no trace of one). The 20 s debounce could have delayed a rebind; I did not check which rebinds were delayed by it.
- Mechanism notes 1 and 2 are source reading plus two traced runs (one with L2, one without); the `peers` map state was not logged. The same-instance "no re-dial" claim is a reading; the rebind result does not test it.
- A4 uses a vendored, patched iroh that drops datagrams to an unconnected relay; diagnostic only. In A4, scenario 1 never blocked, so no heal-after-unblock was observed there.
- One host, loopback, debug build, plain-HTTP relays, SIGKILL closes sockets at once, 3 members, QUIC idle 10 s, preset `Minimal`, only scenario 1 (R1+B1 killed together, B2 stays up for the re-subscribe). A restart of R1 (scenario 2) was not run; a rebind map without a dead relay that later returns was not tested.
- gossip main (E10 variant B) was not run.

## Exact commands to rerun on a Linux host

Assume the branch arrives as a bundle on top of `spike/e14-bug-report`, and the host has the cargo registry and git cache that E12 used (the package carries the optional git dependency of variant `b`; `Cargo.lock` is unchanged from E12).

```sh
git fetch /path/to/e15.bundle spike/e15-workaround:spike/e15-workaround   # or: git clone -b spike/e14-bug-report ... && git pull the bundle
git checkout spike/e15-workaround
cd spikes/e10-gossip-main
cargo build --features a && mkdir -p target/bin && cp target/debug/e10-gossip-main target/bin/e10-a
mkdir -p logs/e15 && mkdir -p /tmp/e10
# one foreground call per cell; each run is about 185 s, so keep calls to 3 runs (under 10 min)
./run-cell.sh a 1 w 3 1;      sh e15-archive.sh a-1w-       # A0
./run-cell.sh a 1 l12w 3 1;   sh e15-archive.sh a-1l12w-    # A1
./run-cell.sh a 1 w1w 3 1;    sh e15-archive.sh a-1w1w-     # A2
./run-cell.sh a 1 w1w10 3 1;  sh e15-archive.sh a-1w1w10-   # A2b
./run-cell.sh a 1 w1l1w 3 1;  sh e15-archive.sh a-1w1l1w-   # A3
./run-cell.sh a 0 w1w 3 1;    sh e15-archive.sh a-0w1w-     # A2c, no kill
git checkout -- logs/summary/a-1w-r1.txt    # run-cell.sh overwrites E10's committed summary of the same name
# A4 (diagnostic transport; needs ~/.cargo/registry/src/*/iroh-1.3.0, present after the stock build)
sh ../e14-repro-patched/vendor.sh
(cd ../e15-diag-gossip && cargo build --features a && mkdir -p target/bin && cp target/debug/e15-diag-gossip target/bin/e10-a)
D=$(cd ../e15-diag-gossip && pwd)/target/bin/e10-a
E14_FIX=drain E15_BIN=$D ./run-cell.sh a 1 pw 3 1;     sh e15-archive.sh a-1pw-
E14_FIX=drain E15_BIN=$D ./run-cell.sh a 1 pl12w 3 1;  sh e15-archive.sh a-1pl12w-
E15_BIN=$D ./run-cell.sh a 1 pc 3 1;                   sh e15-archive.sh a-1pc-
python3 -I analyze_e15.py logs/e15 --table
python3 -I analyze_e15.py logs/e15 --runs
python3 -I analyze_e15_delta.py logs/e15
```
Use run numbers above 3 (`./run-cell.sh a 1 w 2 4`) to extend an arm to n=5; the runner refuses to start when `e10-[ab] ...` processes are present (check `pgrep -fl e10-` before each cell). Total for the full table: 35 runs, about 1 h 50 min. For a traced run add `E12_TRACE="iroh=debug,iroh_relay=debug,iroh_gossip=debug"` and run number 101 (excluded by the file-name patterns when kept out of `logs/e15/`).
