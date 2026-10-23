# E10 relay death vs iroh-gossip main

Date 2026-10-20 (runs 16:17-17:37 UTC). Rust 1.99.1 (rustc 1.99.1 58cf8f9c3 2026-10-13), debug build. Harness = E8's (`spike/e8-failover`, commit `619b57f`); the gossip dependency is chosen by cargo feature, member and prober code unchanged.

| | A | B |
|---|---|---|
| iroh-gossip | `=0.101.0`, crates.io (as E8) | git main `c5f44db74ccc232d4e81523117b7e95e36d4c5c5`: HEAD of `n0-computer/iroh-gossip` main on 2026-10-20, committed 09:49Z, "refactor!: mark event and command enums and Metrics as non-exhaustive (#198)". Its Cargo.toml still says 0.101.0. 15 commits after v0.101.0: #153 #164 #171 #183 #161 #178 #189 #191 #194 #187 #195 #186 #201 #200 #198 (GitHub compare API) |
| iroh, iroh-relay | `=1.3.0`, `=1.3.0` (feature `server`) | same. B's Cargo.toml asks `iroh = "1"` from crates.io: no iroh git revision needed |

- tokio 1, postcard 1, preset `Minimal`, same as E8. `Cargo.lock` = E8's lock plus one package (cargo: "Locking 1 package"); no other version moved. The harness source compiled unchanged against main (E8's `Event` matches already carry wildcard arms, needed after #198).
- Branch `spike/e10-gossip-main`, worktree `/Users/ryzhakar/pp/estafette-e10-gossip-main`, package `spikes/e10-gossip-main/` (excluded from the workspace, own `.gitignore` of `/target`). Pushed. Commits: `3fc992b` harness, `2ca1a4b` 18 required runs, `d3a869f` extension and supplementary runs, `ec25678` analysis tables, `a7e39b6` final (adds `--onsets`).
- Binaries: the 18 required runs ran binaries built from `3fc992b` (sha256 A `5ce2ca83b231…`, B `4b42c55bc591…`). The 16 later runs ran binaries built from the `2ca1a4b` source (A `ea6dd9f2fd97…`, B `5caf6f97d92b…`), which differs only by reading `E10_WINDOW_MS` (default 88000).
- Machine: Apple M2 Max, macOS 15.3.1 (Darwin 24.3.0 arm64). All processes on one host over loopback. 1-min load average at cell start 0.87-8.45 (other work on the host; per-cell values in `logs/summary/*.txt`).
- Run: `cd spikes/e10-gossip-main && ./run-cell.sh <a|b> <1|7> <-|np|npw|w> <runs> [run0]` (sets `E8_PAYLOAD=1`, as in E8). Recompute: `python3 -I analyze.py --table|--runs|--gaps|--onsets logs`, `python3 -I rehome_check.py logs`. Logs: `spikes/e10-gossip-main/logs/` (raw per-run logs gz, traces, per-cell stdout, outputs).

## Setup run

- E8 scenario 1: R1+B1 in one process, SIGKILLed; M1 homed on R1, M2 and M3 homed on R2; 1 s heartbeats; announce on status change; QUIC idle 10 s; window 88 s after the kill. `E8_PAYLOAD=1` in every run (as E8).
- Conditions: probers on (E8 default; every member dials the other two by id each ~1 s); probers off (`E8_NOPROBE`); control = scenario 7: R1 and B1 as separate processes, only B1 killed, R1 alive, probers on (R2+B2 stay one process).
- Changes against E8 (diff in git): gossip alias by feature; relay and bootstrap split modes (`relayonly`, `boot`) and scenario 7; output dir `/tmp/e10`; variant in the run tag; `E10_RUN0`; extra measures (first HB after M1 re-home, last HB per pair); `E10_WINDOW_MS`.
- Runs: 34. Required: 18 (6 cells x 3, 88 s). Extension: 6 (no-prober runs 4-6, both variants, 88 s). Supplementary: 10 at a 180 s window (no probers 3 per variant, probers 2 per variant). The supplementary runs answer "does main fix the stall"; none was run for the re-home question.
- Bookkeeping: `a-1-1` was the first (smoke) run, kept as run 1 of its cell. Cell `b-7`: runs 1-2 completed; run 3 was interrupted by a session stop, no result of it was seen, no stray process remained, it was run again from scratch as `b-7-3`. No run discarded.
- Foreground only: each cell was one blocking call (at most 6.5 min); no background monitors.

## Definitions

- Pair gap = longest gap between consecutive heartbeats from tx at rx within [kill - 3 s, window end], trailing gap included (E8's definition). Over 34 runs: 204 pair gaps, 76 healthy (max 1179 ms), 128 stalled (min 50100 ms), none between 1.3 s and 50 s. Stalled = gap >= 5 s.
- M2-M3 gap = max of both directions. "Flowing again" = stalled pair with a heartbeat in the last 5 s of the window. "Resume time" = end of the longest gap. "Onset" = last heartbeat before the longest gap.
- "88 s" = harness window. "cut to 88 s" = 180 s runs re-analysed from raw logs up to kill + 88 s (dial records cut too).
- n column: min/median/max rows show runs with a value over runs; count rows show k/n (k of n runs, or pairs); "of stalled" rows show k over stalled runs or pairs.

## Results

Required cells, 3 runs each, 88 s window.

| Variant | Condition | Measure | min | median | max | n |
|---|---|---|---|---|---|---|
| A | kill R1+B1, probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 78076 | 79066 | 82463 | 3/3 |
| A | kill R1+B1, probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 3/3 |
| A | kill R1+B1, probers, 88 s | M2-M3 stall onset (ms after kill; last HB before the gap) | 5537 | 8934 | 9924 | 3/3 |
| A | kill R1+B1, probers, 88 s | M2-M3 flowing again at window end, of stalled runs | - | - | - | 0/3 |
| A | kill R1+B1, probers, 88 s | M2-M3 resume time (ms after kill) | - | - | - | 0/3 |
| A | kill R1+B1, probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | 41367 | 42038 | 44094 | 3/3 |
| A | kill R1+B1, probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 12/12 |
| A | kill R1+B1, probers, 88 s | M1 HB pairs flowing again at window end, of stalled | - | - | - | 0/12 |
| A | kill R1+B1, probers, 88 s | M1 HB first resume (ms after kill; earliest pair per run) | - | - | - | 0/3 |
| A | kill R1+B1, probers, 88 s | fresh dial by id to M1 ok again (ms after kill; earlier of M2, M3) | 78101 | 78101 | 78101 | 1/3 |
| A | kill R1+B1, probers, 88 s | fresh dial M2-M3 ok again (ms after kill; either direction) | - | - | - | 0/3 |
| B | kill R1+B1, probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 84494 | 85092 | 85479 | 3/3 |
| B | kill R1+B1, probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 3/3 |
| B | kill R1+B1, probers, 88 s | M2-M3 stall onset (ms after kill; last HB before the gap) | 2521 | 2908 | 3506 | 3/3 |
| B | kill R1+B1, probers, 88 s | M2-M3 flowing again at window end, of stalled runs | - | - | - | 0/3 |
| B | kill R1+B1, probers, 88 s | M2-M3 resume time (ms after kill) | - | - | - | 0/3 |
| B | kill R1+B1, probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | 43506 | 44939 | 45063 | 3/3 |
| B | kill R1+B1, probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 12/12 |
| B | kill R1+B1, probers, 88 s | M1 HB pairs flowing again at window end, of stalled | - | - | - | 3/12 |
| B | kill R1+B1, probers, 88 s | M1 HB first resume (ms after kill; earliest pair per run) | 74034 | 78333 | 82632 | 2/3 |
| B | kill R1+B1, probers, 88 s | fresh dial by id to M1 ok again (ms after kill; earlier of M2, M3) | - | - | - | 0/3 |
| B | kill R1+B1, probers, 88 s | fresh dial M2-M3 ok again (ms after kill; either direction) | - | - | - | 0/3 |
| A | kill R1+B1, no probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 1011 | 83061 | 83203 | 3/3 |
| A | kill R1+B1, no probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 2/3 |
| A | kill R1+B1, no probers, 88 s | M2-M3 stall onset (ms after kill; last HB before the gap) | 4797 | 4868 | 4939 | 2/3 |
| A | kill R1+B1, no probers, 88 s | M2-M3 flowing again at window end, of stalled runs | - | - | - | 0/2 |
| A | kill R1+B1, no probers, 88 s | M2-M3 resume time (ms after kill) | - | - | - | 0/3 |
| A | kill R1+B1, no probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | 41731 | 42616 | 43952 | 3/3 |
| A | kill R1+B1, no probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 8/12 |
| A | kill R1+B1, no probers, 88 s | M1 HB pairs flowing again at window end, of stalled | - | - | - | 0/8 |
| A | kill R1+B1, no probers, 88 s | M1 HB first resume (ms after kill; earliest pair per run) | - | - | - | 0/3 |
| B | kill R1+B1, no probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 1017 | 77151 | 79150 | 3/3 |
| B | kill R1+B1, no probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 2/3 |
| B | kill R1+B1, no probers, 88 s | M2-M3 stall onset (ms after kill; last HB before the gap) | 3914 | 3916 | 3919 | 2/3 |
| B | kill R1+B1, no probers, 88 s | M2-M3 flowing again at window end, of stalled runs | - | - | - | 2/2 |
| B | kill R1+B1, no probers, 88 s | M2-M3 resume time (ms after kill) | 81065 | 82286 | 83507 | 2/3 |
| B | kill R1+B1, no probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | 43297 | 43626 | 43817 | 3/3 |
| B | kill R1+B1, no probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 8/12 |
| B | kill R1+B1, no probers, 88 s | M1 HB pairs flowing again at window end, of stalled | - | - | - | 4/8 |
| B | kill R1+B1, no probers, 88 s | M1 HB first resume (ms after kill; earliest pair per run) | 85511 | 85511 | 85511 | 1/3 |
| A | control: kill B1 only (R1 alive), probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 1012 | 1014 | 1090 | 3/3 |
| A | control: kill B1 only (R1 alive), probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 0/3 |
| A | control: kill B1 only (R1 alive), probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | - | - | - | 0/3 |
| A | control: kill B1 only (R1 alive), probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 0/12 |
| A | control: kill B1 only (R1 alive), probers, 88 s | fresh dial by id to M1 failed after the kill (runs) | - | - | - | 0/3 |
| B | control: kill B1 only (R1 alive), probers, 88 s | M2-M3 longest HB gap (ms; max of both directions) | 1010 | 1012 | 1014 | 3/3 |
| B | control: kill B1 only (R1 alive), probers, 88 s | M2-M3 stalled (gap >= 5 s), runs | - | - | - | 0/3 |
| B | control: kill B1 only (R1 alive), probers, 88 s | M1 re-home (ms after kill; watch_addr lists only R2) | 45085 | 45085 | 45085 | 1/3 |
| B | control: kill B1 only (R1 alive), probers, 88 s | M1 HB pairs stalled (4 per run) | - | - | - | 0/12 |
| B | control: kill B1 only (R1 alive), probers, 88 s | fresh dial by id to M1 failed after the kill (runs) | - | - | - | 0/3 |

Control: first NeighborDown (B1) at 10.1-10.7 s in all 6 runs, nothing else lost. The control's `watch_addr` re-home row: M1 stayed on R1 in 5 of 6 runs; in `b-7-2` it moved to R2 at 45.1 s with R1 alive (see Re-home time).

### Extension and supplementary cells

Same measures, pooled. "Cut" = 180 s runs analysed up to 88 s. Counts are k/n. Times in seconds after the kill.

| Condition | Measure | A | B |
|---|---|---|---|
| no probers, runs 1-6, 88 s | M2-M3 stalled | 4/6 | 5/6 |
| | gossip M2-M3 flowing again at end, of stalled | 0/4 | 5/5 |
| | M2-M3 resume time, min / median / max | - | 66.0 / 81.1 / 86.3 (n=5) |
| no probers, 9 runs cut to 88 s | M2-M3 stalled | 5/9 | 7/9 |
| | flowing again at end, of stalled | 0/5 | 7/7 |
| | M2-M3 resume time, min / median / max | - | 62.3 / 76.1 / 86.3 (n=7) |
| | M1 HB pairs flowing again at end, of stalled | 0/18 | 10/26 |
| probers, 5 runs cut to 88 s | M2-M3 stalled | 5/5 | 5/5 |
| | flowing again at end, of stalled | 0/5 | 1/5 (87.2 s) |
| | M1 HB pairs flowing again at end, of stalled | 0/20 | 7/20 |
| | fresh dial to M1 ok again (earlier of M2, M3) | 2/5 (78.1, 85.1) | 1/5 (80.1) |
| | fresh dial M2-M3 ok again | 1/5 (79.0) | 1/5 (86.3) |
| no probers, 180 s | M2-M3 stalled | 1/3 | 2/3 |
| | flowing again at end, of stalled | 0/1 (M2 cut off 161-171 s) | 2/2 (62.3, 76.1) |
| | M1 HB pairs flowing again at end, of stalled | 0/2 | 8/8 (first resume 81.9 and 145.0) |
| probers, 180 s | M2-M3 stalled | 2/2 | 2/2 |
| | flowing again at end, of stalled | 0/2 | 1/2 (87.2) |
| | M1 HB pairs flowing again at end, of stalled | 0/8 | 4/8 |
| | fresh dial to M1 ok again | 2/2 (85.1, 156.1) | 2/2 (80.1, 156.1) |
| | fresh dial M2-M3 ok again | 2/2 (79.0, 158.0) | 1/2 (86.3) |
| all stalled runs (A 10, B 12) | M2-M3 stall onset: median (min-max) | 7.2 (3.7-26.0) | 3.7 (2.5-7.9) |

Long form of every cell: `logs/results-table.md`.

### Per-run values

Re-home = `watch_addr` lists only R2. M1 first resume = earliest M1 pair flowing again. Dial = earlier of M2, M3 first ok fresh dial to M1 after its first failure. NeighborUp = member NeighborUp events after the kill (rounded s). `-` = none or not applicable. Windows: 88 s, except `npw` and `w` runs (180 s).

| Run | Re-home (s) | M2-M3 longest gap (s) | M2-M3 flowing again at (s) | M1 pairs stalled | M1 pairs flowing again | M1 first resume (s) | fresh dial to M1 ok (s) | member NeighborUp after kill (s) |
|---|---|---|---|---|---|---|---|---|
| a-1-1 | 41.4 | 82.5 | - | 4/4 | 0/4 | - | 78.1 | - |
| a-1-2 | 42.0 | 78.1 | - | 4/4 | 0/4 | - | - | - |
| a-1-3 | 44.1 | 79.1 | - | 4/4 | 0/4 | - | - | - |
| b-1-1 | 44.9 | 85.1 | - | 4/4 | 1/4 | 74.0 | - | 70,73 |
| b-1-2 | 43.5 | 85.5 | - | 4/4 | 0/4 | - | - | - |
| b-1-3 | 45.1 | 84.5 | - | 4/4 | 2/4 | 82.6 | - | 81,83,83,83 |
| a-1np-1 | 44.0 | 83.2 | - | 4/4 | 0/4 | - | n/a | - |
| a-1np-2 | 42.6 | 1.0 | - | 0/4 | 0/0 | - | n/a | 56,56,56,56,56,56 |
| a-1np-3 | 41.7 | 83.1 | - | 4/4 | 0/4 | - | n/a | - |
| a-1np-4 | 41.2 | 84.1 | - | 4/4 | 0/4 | - | n/a | - |
| a-1np-5 | 39.0 | 1.0 | - | 0/4 | 0/0 | - | n/a | 56,56 |
| a-1np-6 | 45.7 | 75.1 | - | 4/4 | 0/4 | - | n/a | - |
| b-1np-1 | 43.6 | 79.2 | 83.5 | 4/4 | 0/4 | - | n/a | 83,83,83 |
| b-1np-2 | 43.3 | 77.2 | 81.1 | 4/4 | 4/4 | 85.5 | n/a | 81,81,81,85 |
| b-1np-3 | 43.8 | 1.0 | - | 0/4 | 0/0 | - | n/a | - |
| b-1np-4 | 43.5 | 62.1 | 66.0 | 4/4 | 0/4 | - | n/a | 65,65,65 |
| b-1np-5 | 40.2 | 78.1 | 86.3 | 2/4 | 2/2 | 86.1 | n/a | 86 |
| b-1np-6 | 44.9 | 67.1 | 70.6 | 4/4 | 0/4 | - | n/a | 66,70,70,70 |
| a-1npw-1 | 43.6 | 170.9 | - | 2/4 | 0/2 | - | n/a | 56,56 |
| a-1npw-2 | 40.8 | 1.0 | - | 0/4 | 0/0 | - | n/a | 56,56,116,116 |
| a-1npw-3 | 42.3 | 1.0 | - | 0/4 | 0/0 | - | n/a | 56,56,56,56 |
| b-1npw-1 | 44.4 | 72.1 | 76.1 | 4/4 | 4/4 | 81.9 | n/a | 76,76,76,82,148,180,180 |
| b-1npw-2 | 40.8 | 59.1 | 62.3 | 4/4 | 4/4 | 145.0 | n/a | 61,61,61,144,156,156 |
| b-1npw-3 | 38.4 | 1.0 | - | 0/4 | 0/0 | - | n/a | - |
| a-1w-1 | 41.1 | 154.0 | - | 4/4 | 0/4 | - | 85.1 | - |
| a-1w-2 | 41.3 | 176.3 | - | 4/4 | 0/4 | - | 156.1 | - |
| b-1w-1 | 46.6 | 80.2 | 87.2 | 4/4 | 4/4 | 86.1 | 80.1 | 86,86,86,86,86,121,121,126,126 |
| b-1w-2 | 41.7 | 177.3 | - | 4/4 | 0/4 | - | 156.1 | 76,151 |
| a-7-1 | - | 1.1 | - | 0/4 | 0/0 | - | - | 55,55,56,56 |
| a-7-2 | - | 1.0 | - | 0/4 | 0/0 | - | - | 55,55,56,56 |
| a-7-3 | - | 1.0 | - | 0/4 | 0/0 | - | - | 11,11,56,56 |
| b-7-1 | - | 1.0 | - | 0/4 | 0/0 | - | - | - |
| b-7-2 | 45.1 | 1.0 | - | 0/4 | 0/0 | - | - | - |
| b-7-3 | - | 1.0 | - | 0/4 | 0/0 | - | - | - |

## Answer: does main fix the stall, and under which conditions

**No.** Main `c5f44db` stalls in the E8 scenario as often as 0.101.0, and as early. It heals the gossip layer by itself after 62-145 s; 0.101.0 did not heal in any of its 10 stalled runs (88 s or 180 s).

| | A 0.101.0 | B main `c5f44db` |
|---|---|---|
| M2-M3 stalled, kill R1+B1, probers | 5/5 (3 at 88 s, 2 at 180 s) | 5/5 |
| M2-M3 stalled, kill R1+B1, no probers | 5/9 | 7/9 (Fisher p = 0.62 against A) |
| M2-M3 stalled, kill B1 only (R1 alive), probers | 0/3 | 0/3 |
| stall onset in stalled runs, s after kill | median 7.2 (3.7-26.0), n=10 | median 3.7 (2.5-7.9), n=12 |
| gossip healed by 88 s, of stalled, no probers | 0/5 | 7/7 (p = 0.0013) |
| gossip healed by 88 s, of stalled, probers | 0/5 | 1/5 |
| gossip healed by 180 s, of stalled | 0/3 | 3/4 (`b-1w-2` not healed) |

- **Trigger is relay death, not bootstrap death.** Killing only B1 with R1 alive stalled 0 of 6 runs (both variants). B1's loss shows as one NeighborDown at 10.1-10.7 s. This is the control E8 lacked.
- **Same stall frequency.** No probers: A 5/9, B 7/9, no detectable difference (p = 0.62). Probers on: 10/10 stalled (A 5, B 5); probers off: 12/18 (p = 0.062). Association, not isolated; E8's "probers amplify" is consistent, not shown.
- **Stall usually starts before any idle timeout.** Last M2-M3 heartbeat 2.5-7.9 s after the kill in B (12 of 12 runs before 10 s) and 3.7-26.0 s in A (8 of 10 before 10 s; 12.9 s and 26.0 s otherwise). The first NeighborDown (10.0 s, QUIC idle) comes after the stall begins in 20 of 22 stalled runs.
- **What main changes: gossip re-forms on its own.** No probers, 88 s: B healed M2-M3 in 7 of 7 stalled runs, at 62.3-86.3 s (median 76.1); A in 0 of 5. At 180 s B healed all 8 stalled M1 pairs (first resume 81.9 s in one run, 145.0 s in the other) and A's one stalled no-prober run kept M2 cut off for 161-171 s. With probers B heals less: 1 of 5 by 88 s (87.2 s), 1 of 2 by 180 s (p = 0.010 for 7/7 against 1/5 within B).
- **Plausible PR: #191** "rejoin when alone, shuffle early" (merged 2026-10-14, `dfeec14`). PR text: "We now rejoin the swarm from the bootstrap peers if we ever run out of neighbors"; "When both views are empty, it joins again through the latest 8 contacts the application joined, at first every 5s and later once a minute." Code at `c5f44db`, `src/proto/hyparview.rs`: `MAINTENANCE_INTERVAL` 5 s (line 31), `MAX_REJOIN_INTERVALS` 12 (line 37, comment: "An isolated node rejoins in its 1st, 2nd, 4th, and 8th maintenance pass alone, and then in every 12th"), `handle_maintenance_timer` (lines 760-786). Fit: the 12th pass falls about 55-60 s after isolation, the 24th about 115-120 s. Observed heals: M2-M3 at 62-87 s; M1 pairs at 82 s and 145 s (`b-1npw-1`, `b-1npw-2`). Fit only; not bisected (a build at `cd73d93`, V1b's #161/#178/#189 without #191, against `dfeec14` would separate it). #161, #178, #189, #195 target leaks and connection lifecycle; none addresses a node whose peers all vanished.
- **The head-of-line hypothesis from E8 is still in main.** 0.101.0 `net.rs:48` `SEND_QUEUE_CAP = 64`, `net.rs:686` `active_send_tx.send(message).await`; main `net.rs:49` and `net.rs:813`, same code. #186 (merged 2026-10-20 08:32Z, in B) fixes a busy publisher and partial stream headers and says: "This PR does not cover a slow peer. The actor still waits when the send queue of a peer is full, and that stops every topic. #185 covers that case." #185 is open, unmerged (head `f5b1504e06dced58decc6ea10f240927cb4c0e77`), not in B. Not tested: main + #185.
- **The stall is not only gossip.** With probers on, fresh dials M2 to M3 (both on live R2) first failed 6.0-29.1 s after the kill in 10 of 10 runs and recovered in 3 of 10 (79.0, 86.3, 158.0 s). In 0.101.0 dials recovered while gossip never did: `a-1w-1` dials ok at 79.0-85.1 s, heartbeats dead for 154-171 s; `a-1w-2` dials ok at 156-158 s, heartbeats dead 166-176 s. Gossip needs a rejoin step to use the recovered layer: main has one (#191), 0.101.0 has none.
- **Conditions under which main heals:** probers off (7 of 7 stalled runs by 88 s); probers on (1 of 5 by 88 s, 1 of 2 by 180 s). Probers-on timing against dial recovery: `b-1w-1` dials recovered at 80.1-88.1 s and gossip at 87.2 s; `b-1w-2` dials to M1 recovered at 156.1 s, gossip had not healed by 180 s (member NeighborUp at 76 s and 151 s, no heartbeat flow at the end). No-prober runs have no dial data.
- Next test that discriminates: main + #185 (variant C), and a build at `cd73d93` against `dfeec14`.

## Re-home time: where the ~40 s comes from (path:line)

Source: iroh 1.3.0, `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/iroh-1.3.0/`; paths below are relative to its root. Code reading; the claim that the first periodic report is empty is not tested.

1. The home relay is the latest net report's `preferred_relay`: `src/socket/transports/relay/actor.rs:1232-1257` (`on_network_change` switches home when `report.preferred_relay` differs from the current home).
2. Nothing in the relay actors asks for a report when the home relay dies (no net-report trigger in `src/socket/transports/relay/actor.rs`). Reports run at start, on link change, on portmapper update, on relay-map change, and on a timer: `src/socket.rs:1532-1536` (tick, `re_stun(UpdateReason::Periodic)`), `src/socket.rs:2000-2016` (`new_re_stun_timer`: random 20-26 s), restarted after every report at `src/socket.rs:1550-1557`.
3. A periodic report is incremental. `is_major` is true only for `LinkChangeMajor` and `RelayMapChange` (`src/socket.rs:725-727`). A full report needs `next_full` (true only at start, `src/net_report.rs:248`), a major change, or a last full report older than 5 min (`src/net_report.rs:132`, `304-306`).
4. An incremental report probes nothing when the previous report has latencies: `src/net_report/probes.rs:117-127` (`with_last_report` returns the initial plan only if `last_report.relay_latency.is_empty()`, else `Self::default()`, comment "TODO: is this good?"), chosen at `src/net_report/reportgen.rs:375-379`. The report then names no relay and the socket keeps the current home: `src/socket.rs:1963-1966`. `reports.last` is replaced by that empty-latency report: `src/net_report.rs:821`.
5. The next periodic report sees `last_report.relay_latency.is_empty()` and builds the initial plan: HTTP probes to every relay (`src/net_report/probes.rs:98-112`). R1 fails, R2 answers, `preferred_relay` = R2 (`src/net_report.rs:749-822`). The latency hysteresis (`src/net_report.rs:812-817`, switch only if the new relay is at least 1.5 times faster) applies only while the old relay still answers (`!old_relay_cur_latency.is_zero()`), so a dead home never holds.
6. Timing: first report at start, then two periodic intervals, 2 x U(20, 26) s = 40-52 s after the member's first report (about its bind). In this harness the kill falls 3.1-4.9 s after M1's bind.
7. Measured (`rehome_check.py`, `logs/rehome.txt`): 28 kill runs, both variants: re-home 42.5-50.7 s after M1's bind (median 46.8; predicted window 40-52 s) and 38.4-46.6 s after the kill (median 43.0; E8 had 38.5-42.9). The window was derived from the constants after seeing E8's ~40 s; the data fit it, they do not exclude other two-interval mechanisms.
8. Cross-check in the data: in control run `b-7-2` M1 moved R1 to R2 at 45.1 s (49.2 s after bind) with R1 alive and B1 the only thing killed. The same timer plus the 1.5 times latency rule explains it; no failure involved. E8 scenario 2 had one such flip in 3 runs.
9. Disagreeing data: E8's `E8_REJOIN` runs re-homed at 3.5 s and 20.4 s after the kill (E8 table), far below the 36-48 s this reading predicts. A major link change (host interface change) forces a full report at once (`src/socket.rs:725-727`, `src/net_report.rs:304`) and would give such outliers; not examined. E10: 0 of 28 early re-homes (earliest 38.4 s).

So the ~40 s is two re-stun periods, not a relay-failure reaction: the member keeps its dead home until the second periodic report.

## Surprises

- Fresh dials by id recover without gossip in 0.101.0 (`a-1w-1`, `a-1w-2`: dials ok at 79-85 s and 156-158 s; heartbeats never resumed in 180 s).
- Layer-below recovery comes in two clusters. Fresh dials recovered at 78.1-88.1 s in 3 runs (`a-1-1` 78.1; `a-1w-1` 79.0 and 85.1; `b-1w-1` 80.1, 86.3, 88.1) and at 156.1-158.1 s in 2 runs (`a-1w-2`, `b-1w-2`); never in the other 5 probers-on runs (`a-1-2`, `a-1-3`, `b-1-1`, `b-1-2`, `b-1-3`; 88 s windows). E8 saw 79.05 s. Not explained. Candidate, untested: non-home relay connections close after 60 s without a write (`src/socket/transports/relay/actor.rs:68`, `RELAY_INACTIVE_CLEANUP_TIME`).
- M2-M3 breaks 2.5-8 s after the kill in B, before the 10 s idle timeout and before any NeighborDown.
- A stall can be partial: `a-1npw-1` cut M2 off from M1 and M3 for 161-171 s while M1 and M3 kept exchanging heartbeats (2 of 4 M1 pairs stalled).
- M1 re-homed with R1 alive in `b-7-2` (see Re-home time).
- Healthy no-prober runs of 0.101.0 show member NeighborUp events at about 56 s (and 116 s): the 60 s first shuffle after join (#191 text: "The first shuffle came 60s after a join"). Those reshuffles did not heal any stalled run.
- Main's gossip dies earlier than 0.101.0's (onset median 3.7 s against 7.2 s). Cause not examined.
- The harness's NeighborUp stream omits the neighbor that completes `subscribe_and_join` (control `a-7-1`: `M1 ND B1` with no `M1 NU B1`). Neighbor sets cannot be rebuilt from NU/ND events. E8's trace note "M2 has no gossip neighbor left" is an inference from NeighborDown events, not an observation. E10 does not use neighbor sets.

## What could make this wrong

- n per cell is 3 (required) and the outcome is bimodal. Pooled counts carry the comparisons: stall frequency A 5/9 against B 7/9 (p = 0.62) cannot separate the variants; heal counts (7/7 against 0/5, p = 0.0013) can. Fisher exact, two-sided, runs treated as independent.
- Not randomised or interleaved within a cell. A and B alternate in time by cell; load differed (0.87-8.45 at cell start; highest 8.45 `a-1`, 7.72 `b-7`, 7.36 `a-1npw`). Stalls occurred across that range, including load 1.07 (`a-1w`); not controlled.
- Binary changed after the required cells (adds `E10_WINDOW_MS` only; default 88000). Extension and supplementary runs used the later binaries.
- "Cut to 88 s" results re-derive measures from raw logs, not from the harness's own end-of-run summary (which reports the 180 s window for those runs). Stall threshold 5 s; the data are bimodal (1.2 s against 50 s), so no threshold between 1.3 and 50 s changes a classification.
- Supplementary runs were added after seeing the first 18 (late heal in B). They are labelled; the required cells are reported on their own.
- Heal attribution to #191 rests on PR text, code constants and timing. No bisect, no intermediate commit built, no run with #185.
- B is a moving, unreleased commit (`c5f44db`, 09:49Z the day of the runs). Release code may differ.
- Control used probers only. Its process layout differs from the kill cells (B1 separate from R1; R2+B2 together). Probers-off control not run.
- Scenario 1 and its control only. E8 scenarios 2-4 not run for B.
- Layer-below evidence (fresh dials) exists only in probers-on runs; probers themselves may change the stall (10/10 against 12/18).
- One host, loopback, plain-HTTP relays, no TLS, no NAT; SIGKILL closes sockets with RST (members see it within ms); debug build; QUIC idle 10 s on every endpoint; preset `Minimal`. A silent partition would behave differently.
- Re-home reading is code reading of iroh 1.3.0; the empty first periodic report is inferred from `probes.rs:117-127`, not observed (no iroh logging was enabled). The 40-52 s window fits 28 runs; other two-interval mechanisms fit as well. E8's 3.5 s and 20.4 s re-homes are unexplained by it (Re-home time, item 9).
- `E8_PAYLOAD=1` in every run: members ingest `Announce` payloads into their lookup (the S13 design); with the built-in lookup alone E8 found the mesh never dialable.
