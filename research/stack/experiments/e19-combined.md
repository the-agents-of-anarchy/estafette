# E19 combined relay-death workaround (estafette)

Date 2026-10-21 (runs 05:05-06:29 UTC). Question: does the workaround built from E15-E18 parts (W1 + C2 probe + E18 join timeout + E18 gossip fix) remove the stall and its relapses, and does C2 still earn its 24 lines.

## Answer

- Combined (B1) had 0 second rebinds in 8 kill runs, 0 late joins, all 42 stalled pairs flowing at 180 s, all-stalled-pairs-flowing-from 29.3 / 31.2 / 33.2 s (n=7 stalled runs; run 1911 never stalled). W1 alone (B0): 3/8 runs relapsed, 1 run never fully healed (46/48 pairs at 180 s), fast heals 15.7-18.6 s in the other 5 runs. Without C2 (B2): 5/8 runs relapsed, median all-flowing-from 141.4 s.
- C2 earns its lines on top of the E18 counters: second rebinds 0/8 (B1) vs 5/8 (B2), exact p = 0.026 (Fisher; permutation test on all-flowing-from p = 0.026, runs not pairs).
- B1 against B0 is not established at n=8: second rebinds 0/8 vs 3/8 (p = 0.20), all-flowing-from mean 31.1 vs 70.2 s (p = 0.20, B0 run 1904 censored at 180 s). B1 is slower than B0 whenever B0 does not relapse (31.2 vs 17.1 s median): it trades the fast heal for no tail.
- E18's two counters without C2 (B2) did not reduce relapses against W1 alone (5/8 vs 3/8, p = 0.62). The relapses in B2 are heartbeat silences 57-67 s after a clean rebind (inferred re-block, cause 1 of E17), plus a pattern I did not explain: JOINED at once, no heartbeat for 55-58 s, in 3 chains.
- 0 false rebinds in the 3 no-kill controls of B1.

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e19-combined`, branch `spike/e19-combined` from `spike/e18-attach` (85ef8a2). Commit `b1357d1` (pushed; logs under `spikes/e10-gossip-main/logs/e19`, 273 files, 7.1 MB; no `target/`, no `vendor/`).
- Build: the E18 package `spikes/e18-attach-gossip` (stock iroh 1.3.0, vendored iroh-gossip 0.101.0 with E18 logging and `E18_FIX`): `sh vendor.sh`, `cargo build --features a`, copied to `target/bin/e10-a` (sha256 6f522167c515...; a fresh build, not the E18 binary). Rust 1.99.1, debug, macOS 15.3.1 arm64 (Apple M2 Max), loopback, plain-HTTP relays, SIGKILL of R1+B1 (scenario 1), probers on, 1 s heartbeat, QUIC idle 10 s, preset `Minimal`, 180 s window, W1 T=3000, `E12_TRACE=iroh_gossip=debug`.
- Changes: no Rust change. `run-cell.sh` +3 tags (each unsets the switches it must not carry): `y0w` = `E15_W1=1` only (B0); `y1w` = `E15_W1=1 E17_PROBE=1 E18_JOIN_T_MS=5000 E18_FIX=1` (B1, and its scenario-0 control); `y2w` = `E15_W1=1 E18_JOIN_T_MS=5000 E18_FIX=1` (B2). Three new files in `spikes/e10-gossip-main`: `analyze_e19_table.py` (copy of `analyze_e18_table.py` with the new arm regexes), `analyze_e19.py` (per-run event list and second-rebind table), `analyze_e19_stats.py` (arm statistics, permutation and Fisher tests). `E17_FRESH_LOOKUP` is not set in any arm (plain shared lookup, no C1). Config check: `PROBE_RELAYS` present in B1 raw logs (`down=[R1]`), absent in B2; B2 and B1 Joins at B2 that met an active member got a Neighbor at once (E18_FIX effective).
- Commands, all foreground, `export E18_LOGDIR=logs/e19`, from `spikes/e10-gossip-main`; `sh e18-batch.sh <scn> <tag> <run0> <n>`:
  - control first: `sh e18-run.sh 0 y1w 1930` (0 REBIND, 0 JOIN_FAIL, 3 JOINED, 3 min)
  - order (interleaved by batch): `1 y0w 1900 3`, `1 y1w 1910 3`, `1 y2w 1920 3`, `1 y0w 1903 3`, `1 y1w 1913 3`, `1 y2w 1923 3`, `1 y0w 1906 2`, `1 y1w 1916 2`, `1 y2w 1926 2`, then `0 y1w 1931 2`.
  - The last three batches (2 runs each) were issued in one tool block and ran one after the other (cell ends 06:10, 06:16, 06:22 Z, each run 185 s; the runner aborts if a stray `e10` process exists). `pgrep -fl "e10-a|e18-attach"` printed nothing after every run and at the end.
- Order is interleaved by batch, not randomised; arms within a round always B0, B1, B2. Load average (1 min) at cell starts 0.43-4.06.
- Analysis, `logs/e19` only: `python3 -I analyze_e19_table.py logs/e19` (E15 definitions plus late joins; `table.md`), `python3 -I analyze_e19.py logs/e19 --second <runs>` (`second_rebinds.txt`), `python3 -I analyze_e19_stats.py logs/e19` (`stats.txt`). Times in seconds after KILL. Tracked `__pycache__/analyze.cpython-314.pyc` restored with `git checkout`; no committed log or summary file overwritten (`logs/summary` got new names only).

## Results

Per arm (`logs/e19/table.md`, `stats.txt`). n = runs.

| Arm | runs | runs stalled | stalled pairs flowing at 180 s | all stalled pairs flowing from, s min / med / max | first rebind, s min / med / max | rebinds per member (mean, max; total) | runs with a second rebind | late joins (rebound endpoints) | false rebinds |
|---|---|---|---|---|---|---|---|---|---|
| B0 W1 alone | 8 | 8 | 46/48 | 15.7 / 17.1 / 148.8 (n=7; run 1904 never all flowing, censored at 180: median 17.9) | 10.1 / 11.7 / 13.1 | 1.17, 3; 28 | 3 (1901, 1904, 1906) | 1 of 28 (1904 M2, JOIN_FAIL at 60.0 s) | - |
| B1 combined | 8 | 7 (1911 no stall, no rebind) | 42/42 | 29.3 / 31.2 / 33.2 (n=7) | 10.5 / 12.0 / 12.6 | 0.88, 1; 21 | 0 | 0 of 21 | - |
| B2 no C2 | 8 | 8 | 48/48 | 14.1 / 141.4 / 162.6 (n=8) | 6.8 / 11.4 / 12.0 | 1.75, 5; 42 | 5 (1920, 1922, 1924, 1925, 1926) | 0 of 42 | - |
| B1n no kill | 3 | 0 | - | - | - | 0 | 0 | 0 | 0 REBIND, 0 JOIN_FAIL in 3 runs |

Per run all-flowing-from, s (`-` = see note):
- B0: 15.7, 148.8, 17.0, 17.1, never (run 1904: 46/48 at 180 s), 16.7, 147.6, 18.6.
- B1: 31.2, no stall (1911), 29.3, 32.1, 31.7, 33.2, 30.2, 30.2.
- B2: 146.5, 18.0, 162.6, 14.1, 144.4, 138.4, 147.4, 14.1.

Tests (exact, all 12870 permutations of 8+8, difference of means, two-sided; n = runs; stalled runs only, B0 run 1904 censored at 180 s):
- B0 vs B1 p = 0.200 (B1 n=7); B0 vs B2 p = 0.568; B1 vs B2 p = 0.026.
- With the never-stalled B1 run counted as 0 s (n=8): B0 vs B1 p = 0.189; B1 vs B2 p = 0.024.
- Runs with a second rebind, Fisher exact two-sided: B0 vs B1 p = 0.200; B0 vs B2 p = 0.619; B1 vs B2 p = 0.026.
- Means are driven by the bimodal relapse; the medians and the fast-heal band are in the table.

Other observations:
- Non-relapsing first heals: B0 15.7-18.6 s (5 runs), B2 14.1-18.0 s (3 runs), B1 29.3-33.2 s (7 runs). B1's gap comes from the first heartbeat after REBOUND: delay of 10 s or more in 9 of 21 B1 rebound endpoints (11.2-15.1 s, mostly M1), against 1 of 24 first-generation endpoints in B0 (the late join) and 3 of 24 in B2 (M1, 14.0-14.1 s, in relapsing runs). Cause not investigated. E17 saw the same slower first heal with C2 (32.5 vs 18.8 s).
- B1 stall onset: run 1911 never stalled (bimodal onset, as E10/E12/E17).

## Second rebinds and their causes

`logs/e19/second_rebinds.txt` (one row per REBIND after a member's first). B1: none. 22 rows in B0 (4) and B2 (18). Classification uses only what the stock build shows; E14 is not in this build, so a re-block (E17 cause 1) cannot be seen as a BLOCKED line.

Inferred re-block (inferred, not seen): the previous generation JOINED at once, the heartbeats flowed after the rebound, no JOIN_FAIL, and heartbeat silence began 57.0-67.5 s after that REBOUND (E17: BLOCKED 57.2-57.4 s after rebind, heartbeats stop 2.8-10.6 s later, W1 rebinds 3 s after the silence). 14 rows:
- B0: 1901 M2 (silence from 67.1 s after REBOUND), 1906 M2 (67.5), 1904 M2 third generation (66.6).
- B2: 1920 M3 (66.9), 1926 M3 (67.4); runs 1922, 1924, 1925: all three members each (9 rows; silence 57.0-63.2 s after REBOUND; M1 57.0-57.1 in all three runs, M2 and M3 about 3 s later in the same runs, so some of these may be a peer's block seen as silence, not the member's own).
- Not verified: no transport trace, so "inferred" rests on timing only. A silence of 57-67 s after a clean rebind is also what an endpoint-level block would give; I have no line that separates it from another cause with the same timing.

Late join (E18 cause, stale pending Neighbor at B2): B0 run 1904 M2: Join at 18.86 s met `active + pending` at B2, no Neighbor sent, JOIN_FAIL at 60.0 s, then clean Join at 78.87 s (`analyze_e18.py --joins`). 1 case. In B1 and B2 (E18_FIX on) the Joins that met `active + pending` (e.g. 1920 M3 at 15.28 s, `active + pending`) were answered at once; 0 late joins.

Other, not explained (B2 only, 7 rebinds in 3 chains): JOINED returned at once, B2's Neighbor was sent, and no heartbeat reached the member for 55-58 s from the first REBOUND of the chain; W1 rebound it again about every 20 s (run 1922 M3: 101.3, 121.3, 141.5; spacing not explained), and all generations of the chain ended at one instant (run 1922 M3: 142.3-142.4 s; 1924 M3: 139.2; 1925 M2: 136.6), the next clean rebind. Chains: 1922 M3 (REBOUND at 84.4, 104.4, 121.3), 1924 M3 (84.2, 101.4), 1925 M2 (80.5, 97.4). Each chain started after the other two members had rebound a second time, within about 3 s. Trace seen in run 1922 M3 (member trace 84.4-88 s): B2 forwards the announce messages (43 and 68 bytes) to M3, the member sends its own 5-byte heartbeat to B2 every second, and no 5-byte heartbeat of M1 or M2 arrives; B2's trace shows B2 sending `Prune` and IHave to M3 each second with only 2 Grafts back (84.56, 85.46). I did not find the cause (candidates: M1 and M2 not sending heartbeats toward B2, a plumtree lazy-set state at M3, a block at the other members); not tested.

Count of the 22 rows: inferred re-block 14 (3 in B0, 11 in B2), late join 1 (B0 1904 M2), other 7 (B2, rows after a chain's start).

## Hypotheses and verdicts

- H1 (B1 removes the relapses): 0/8 runs with a second rebind vs 3/8 (B0) and 5/8 (B2). Supported as an observation; B1 vs B0 not significant (p = 0.20 both tests; 0/8 leaves an upper 95% bound near 31% per run).
- H2 (C2 still earns its 24 lines on top of E18 counters): supported, 0/8 vs 5/8 (p = 0.026), and the mean all-flowing-from 31.1 vs 98.2 s (p = 0.026), n=8 runs each.
- H3 (the E18 counters alone, without C2, make W1 safe): contradicted. B2 5/8 relapse vs B0 3/8 (p = 0.62), B2 more rebinds (42 vs 28), and the 55-58 s no-heartbeat chains appear only in B2. The E18 counters fix the pending-Neighbor late join (0/42 late joins in B2, B1 0/21; B0 1/28); they do not touch the re-block.
- H4 (the combined fix is faster than W1 alone): contradicted for the typical run (31.2 vs 17.9 s median; B1 always about 30 s, B0 15.7-18.6 s when it does not relapse); supported for the tail (max 33.2 s against 148.8 s and one run never all flowing). The 11-15 s first-heartbeat delay in B1 is unexplained.
- H5 (no false rebinds from the 5 s join timeout or the probe): 0 REBIND, 0 JOIN_FAIL in 3 no-kill runs; 0 rebinds before the kill in all 24 kill runs (first rebind at 6.8 s or later). Supported, n=3.
- H6 (plain-W1 build has the pending-Neighbor late join, E18's missing arm): 1/24 first-generation endpoints in B0 (1904 M2); E18's A0 had C1 and 7/22. Same mechanism seen once without C1; the frequency here is not comparable to E18 (different lookup, n small).

## What could make this wrong

- n=8 per arm; B1 vs B0 has p = 0.20. The B1 result (0 relapses) fits a relapse rate up to about 31%; B0's 3/8 fits a wide range. Arms are interleaved by batch of 3, 3, 2 but not randomised; B0 always first in a round.
- Re-block is inferred from timing only (stock transport, no E14 diagnostic, no `E17_ITEMS`). A different cause with a 57-67 s silence would be classed wrongly. E17 measured the block directly on a diagnostic build; here the same timing is the only evidence.
- Heartbeat silence start is `REBIND time - silent_ms` (the harness's own trigger), not the last heartbeat time; they agree within the 3 s resolution in the rows I compared.
- The 55-58 s no-heartbeat chains in B2 are unexplained and I looked at one member's trace (run 1922 M3) and its B2 trace; 1924 and 1925 are classified by the same table pattern only.
- Bimodal stall onset: run 1911 never stalled; its 0 s is in the "all runs" variant only; the stalled-only B1 n is 7.
- B0 run 1904 is censored at 180 s in the permutation test (46/48 pairs flowing); counted either way the B0-B1 p stays 0.19-0.20.
- Debug build, tracing on (`iroh_gossip=debug`), loopback, plain HTTP, SIGKILL, 3 members, one host (macOS, other users, load 0.43-4.06), scenario 1 only. B2 here is a harness node, not the estafette bootstrap binary.
- E18_FIX is a vendored runtime switch (main PR #161's `on_join` line), not a release; E17_PROBE is harness code in the member (TCP connect to the configured relay URLs, strips down relays from the other members' lookup entries), not iroh behaviour. A real deployment needs both counters re-implemented in the product (the former needs iroh-gossip main or a vendor patch).
- B1 pays about 14 s in the first heal against W1 alone in non-relapsing runs (31.2 vs 17.1 s median), cause not investigated; in a product the cost may differ.

## Exact commands to rerun on a Linux host

Branch arrives as a git bundle on top of `spike/e18-attach`. In the estafette checkout (already has `spike/e18-attach`):
```
git fetch /path/to/e19.bundle spike/e19-combined:spike/e19-combined
git worktree add ../estafette-e19-combined spike/e19-combined
cd ../estafette-e19-combined/spikes/e18-attach-gossip
sh vendor.sh                       # vendors iroh-gossip-0.101.0 from ~/.cargo/registry and applies the E18 patch
cargo build --features a
mkdir -p target/bin && cp target/debug/e18-attach-gossip target/bin/e10-a
cd ../e10-gossip-main
export E18_LOGDIR=logs/e19-linux   # fresh dir: the analysis below sees only the Linux runs
unset E17_FRESH_LOOKUP E17_PROBE E18_JOIN_T_MS E18_FIX
```
Runs (foreground, about 185 s each, at most 3 runs per call; `pgrep -fl "e10-a|e18-attach"` must print nothing before each; run numbers 2900+ so `logs/summary` names do not clash with committed ones):
```
sh e18-run.sh 0 y1w 2930                 # control first, no kill: expect 0 REBIND, 0 JOIN_FAIL
sh e18-batch.sh 1 y0w 2900 3             # B0, then in this order:
sh e18-batch.sh 1 y1w 2910 3             # B1
sh e18-batch.sh 1 y2w 2920 3             # B2
sh e18-batch.sh 1 y0w 2903 3
sh e18-batch.sh 1 y1w 2913 3
sh e18-batch.sh 1 y2w 2923 3
sh e18-batch.sh 1 y0w 2906 2
sh e18-batch.sh 1 y1w 2916 2
sh e18-batch.sh 1 y2w 2926 2
sh e18-batch.sh 0 y1w 2931 2             # remaining no-kill controls
```
Analysis, own logs only. `analyze_e19_table.py` and `analyze_e19_stats.py` match arm names `a-1y0w-N`, `a-1y1w-N`, `a-1y2w-N`, `a-0y1w-N` for any N, so a fresh `E18_LOGDIR` is what keeps them off the macOS runs:
```
python3 -I analyze_e19_table.py logs/e19-linux
python3 -I analyze_e19_stats.py logs/e19-linux
python3 -I analyze_e19.py logs/e19-linux --second $(ls logs/e19-linux | grep raw.log.gz | sed 's/.raw.log.gz//' | grep -v "a-0y")
python3 -I analyze_e18.py logs/e19-linux --joins $(ls logs/e19-linux | grep raw.log.gz | sed 's/.raw.log.gz//' | grep -v "a-0y")
```
On the Linux host `zcat` reads `.gz` directly; on macOS use `gzip -dc` (`zcat` there looks for `.Z`).
