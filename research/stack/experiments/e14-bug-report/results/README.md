# Raw results

All files are program output, unedited except for identifier renames (`e14` to `hol` in names and environment variables) and path scrubbing in the shell scripts under `linux/`.

## macos/ (Apple M2 Max, macOS 15.3.1 arm64, loopback, debug builds, Rust 1.99.1)

| Directory | Content |
|---|---|
| `repro/` | `runs-b200.txt` (20 RESULT lines, iroh 1.3.0, default burst 200, wait 15 s), `runs-main-b200.txt` (20 lines, main 55908a5, wait 8 s), `recovery-1.3.0.txt`, `recovery-main.txt` (burst 200, 150 s window, full timeline), `recovery-b100.txt` (burst 100, 220 s window, first success at 140.2 s) |
| `patched-diag/` | `diag-stock.txt` (queue length and BLOCKED line, burst 200), `diag-stock-200s.txt` and `run-stock-200s.txt` (200 s window: BLOCKED/UNBLOCKED times and the run's own output), `diag-drain-burst2000.txt` (fix `drain`, burst 2000) |
| `two-process/` | output of `repro/hol-dials` variants (`ctl`, `p1`, `p1b`, `p5`, `p1s5`, `p1s5-b`, `p1s25`, `p1a5`, `p1rm`, `p1rm-b`, `p1rmall-b`) and `trace-p1-relay.log.gz` (relay-actor debug trace of `p1`) |
| `gossip-harness-trace/` | `member-A-trace.log.gz`, `member-B-trace.log.gz`: iroh debug traces of two members of an iroh-gossip 0.101.0 test (harness not included) around a relay kill at 19:24:31.233 UTC |

Not saved as files (read from the terminal at the time): the macOS counts for `--no-kill`, `--burst 0`, `--never-up`, the burst sweep (64/100/150 and singles 65-80), the 20-run `drain`/`drop`/none counts, the burst-2000 controls, and the 3 macOS `--rebind-on-fail` runs (RESULT first_ok 7418-7421 ms). `../run.sh` regenerates all of them as files.

## linux/ (Ubuntu 24.04.4, Linux 6.8.0-138-generic x86_64, 2 vCPU AMD EPYC-Rome VM, rustc 1.99.1, loopback, debug builds)

| Directory | Content |
|---|---|
| `repro-single-file/` | one file per count, one RESULT line per run: `stock-b200` (20), `stock-nokill` (10), `stock-b0` (10), `stock-b64` (5), `stock-b100` (5), `stock-rebind` (3), `main-b200` (20), `patched-drain` (20), `patched-drop` (20), `patched-stock` (5); `build-*.txt`, `vendor.txt`, `uptime.txt`, the two driver scripts (paths replaced by `$RESULTS_DIR`, `$PKG`). Counts: stock-b200 1 PASS / 19 FAIL, main-b200 2 / 18, b100 2 / 3, rebind 1 / 2, all others as expected |
| `gossip-harness-180s/` | output of 180 s runs of an iroh-gossip test on the same VM: `analyze.txt`, `runs.txt`, `cells.txt`, per-run `diag-a-1w.txt` (dial probers on) and `diag-a-1npw.txt` (off), and `exit-lines-w.txt` / `exit-lines-npw.txt` (the `Inactive, exiting` and `Dropped datagram(s)` lines of each member's trace; the kill was at 21:12:34.233 UTC for the first, 21:15:38.802 UTC for the second) |
