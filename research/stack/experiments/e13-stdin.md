# E13 stdin reader: departure at once

Date: 2026-10-20. Same toolchain and crates as E11: Rust 1.99.1, rmcp `=3.5.1`, tokio 1.54.0. Release build. Machine: Apple M2 Max, macOS 15.3.1 (24D70). Linux not run.
Branch `spike/e13-stdin` (from `spike/e11-mcp`), worktree `/Users/ryzhakar/pp/estafette-e13-stdin`, commit `5b58d10`, pushed. `target/` not committed.
Paths: `main.rs` = `spikes/e11-mcp/src/main.rs`; `driver.rs` = `spikes/e11-mcp/src/bin/driver.rs`; `rmcp:` = `~/.cargo/registry/src/index.crates.io-*/rmcp-3.5.1/src/`.
Reproduce from `spikes/e11-mcp/`: `rustup run 1.99.1 cargo build --release`; `./target/release/driver wire <version>`; `./target/release/driver shutdown 10` (5 min 22 s).

## Answer

Departure line goes out at once. Stdin close: 0 ms median, max 1 ms (n=10, all six eof cases). SIGTERM: 0 ms median, max 3 ms (n=10, all eight cases). Same runs, a departure sent after `waiting()` returns would be 5003 ms late (eof, call in flight) and 2002 ms late (SIGTERM, call in flight, uncooperative).
Process exit unchanged by the reader: eof with call in flight still exits at 5004 ms; SIGTERM with call in flight and `bg` still 2003 ms. Reader moves the departure, not the exit.
Wire: five protocol versions, 7 checks each, PASS. Wire output byte-identical to the E11 binary (diff of driver output minus the `stderr:` line, 5/5).
Cost: 26 non-blank lines (`main.rs:244-273`), no new crate, no task, no pipe copy. Two `depart` call sites for SIGTERM (`main.rs:297`, `:307`), one transport tuple (`main.rs:295`).

## Reader as built

Choice: wrap `stdin` in an `AsyncRead` that notes end of input. No duplex pipe, no forwarding task.
Reason, from rmcp source: `(R, W)` with `R: AsyncRead + Send + 'static + Unpin`, `W: AsyncWrite + Send + 'static + Unpin` is an `IntoTransport` (`rmcp:transport/async_rw.rs:24-33`). Transport reads through `FramedRead::new(reader, codec)` (`rmcp:transport/async_rw.rs:483-490` shows the pattern; test module, the production path not separately read). EOF on that stream ends `transport.receive()` with `None`, which sets `QuitReason::Closed` (`rmcp:service.rs:1420-1428`). `stdio()` is `(tokio::io::stdin(), tokio::io::stdout())` (`rmcp:transport/io.rs:4-6`), so substituting the first element changes nothing else. The wrapper sees `Ok(0)` in `poll_read` before `FramedRead` does, since it sits under it.

Code:
- `main.rs:245-250`: `static DEPARTED: Once`; `depart(t0, why)` writes `<elapsed> departure (<why>)` to stderr once (stub for broadcasting Departure).
- `main.rs:252-272`: `DepartOnEof { inner: Stdin, t0 }`; `poll_read` delegates, then calls `depart` on `Ready(Ok)` with no bytes added (`:267`, EOF) or on `Ready(Err)` (`:268`).
- `main.rs:295`: `serve((DepartOnEof { inner: tokio::io::stdin(), t0 }, tokio::io::stdout()))` replaces `serve(stdio())`.
- `main.rs:297` (before handshake) and `:307` (serving): `depart(t0, "SIGTERM")` first, then the E11 path unchanged (`stop.cancel()`, drain, `bg` runtime shutdown).
- Diff vs E11 `main.rs`: +46/-4 lines including imports and rustfmt wrapping. `driver.rs`: timing columns added (+116/-40), `stamped_lines`, `spread`; `median` removed.
- Stub tool bodies, envelope, tool list: untouched.

## Measurement

Method. E11's `driver shutdown 10`, extended. Trigger fires 300 ms after spawn. Departure and stopped times are the driver's own arrival stamp (`Instant::now()` in the stderr reader thread) minus the trigger time, so they do not rely on the server's clock. Exit time as E11: from `drop(stdin)` / `kill(SIGTERM)` returning to `try_wait` seeing exit (1 ms poll). Cap 12 s eof, 4 s SIGTERM. Column "stopped" is the `stopped` stderr line, printed when `waiting()` returns: what a departure sent after `waiting()` would have been in the same run (internal control). Modes as E11: naive = `drop(rt)`; bg = `rt.shutdown_background()`. SIGTERM is ruled to use bg; naive rows kept for comparison with E11.

Milliseconds, min / median / max, n=10 each. E11 column = E11 table median.

| Case | Mode | Departure (E13) | Departure if after `waiting()` (E13 `stopped`) | Exit (E13) | Exit (E11 median) |
|---|---|---|---|---|---|
| eof, before handshake | any | 0 / 0 / 1 | no `stopped` line (serve fails) | 2 / 2 / 3 | 1 |
| eof, idle | any | 0 / 0 / 0 | 0 / 0 / 0 | 1 / 2 / 2 | 1 |
| eof, `send` in flight | any | 0 / 0 / 1 | 5001 / 5003 / 5006 | 5003 / 5004 / 5008 | 5003 |
| eof, cooperative `send` in flight | any | 0 / 0 / 0 | 5002 / 5003 / 5008 | 5003 / 5005 / 5013 | 5003 |
| SIGTERM, before handshake | bg | 0 / 0 / 2 | no `stopped` line | 1 / 2 / 3 | 1 |
| SIGTERM, idle | bg | 0 / 0 / 1 | 0 / 0 / 1 | 1 / 2 / 2 | 1 |
| SIGTERM, `send` in flight | bg | 0 / 0 / 2 | 2001 / 2002 / 2004 | 2002 / 2003 / 2009 | 2003 |
| SIGTERM, cooperative `send` in flight | bg | 0 / 0 / 0 | 0 / 0 / 1 | 2 / 2 / 2 | 1 |
| SIGTERM, before handshake | naive | 0 / 0 / 1 | none | hung 10/10; exits 2 / 2 / 8 after stdin closed | hung 10/10 |
| SIGTERM, idle | naive | 0 / 0 / 0 | 0 / 0 / 1 | hung 10/10; 2 / 2 / 2 after close | hung 10/10 |
| SIGTERM, `send` in flight | naive | 0 / 0 / 1 | 2001 / 2002 / 2004 | hung 10/10; 2 / 2 / 3 after close | hung 10/10 |
| SIGTERM, cooperative `send` in flight | naive | 0 / 0 / 3 | 0 / 1 / 11 | hung 10/10; 2 / 2 / 4 after close | hung 10/10 |

"Departure" at 0 ms means under 1 ms: stamped arrival, resolution well under the 1 ms printed (departure and `stopped` are separate lines; `+0.1 ms` typical in the stderr dump). E11 had no departure column; its comparable number is the `stopped` column here, which matches E11's 5003 ms and 2003 ms timelines.

Exit medians are 1 ms higher than E11 on four idle/coop rows (2 vs 1). Not explained; within E11's stated 2 ms resolution; the reader adds no work on those paths beyond one comparison per read. Not tested separately.

Last-run timeline, eof with call in flight (server stderr, process-relative): `303.657209ms departure (stdin closed)`, then `5.305965s stopped`, `runtime returned`, `runtime dropped` (driver arrival: departure +0.0 ms, stopped +5002.4 ms).

Wire (`driver wire <version>`): 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25, 2026-07-28; each 7 PASS (tests 12 and 13, names in 3.10 order, D28 `not_claimed`, text equals `structuredContent`, `isError` equals `!ok`, no unsolicited message). Run against the E11 build in `/Users/ryzhakar/pp/estafette-e11-mcp`: output identical, byte for byte, minus the `stderr:` line.

## Surprises

1. Departure fires at EOF before handshake too (eof-pre: 0 ms). 3.9 wants Stopping from any state on stdin close; this gives it without a separate path. The process still exits 1 with `ConnectionClosed("initialize request")` as in E11.
2. The reader does not shorten the exit. After the departure, rmcp still drains for 5 s (eof) or 2 s (SIGTERM). The process lives that long. For real Departure sending, that window is time during which the process can still write; for the 3.14/3.15 exit wait of 1000 ms it is not a bound.
3. Departure is written from inside `poll_read`, on the runtime thread, inside rmcp's read loop. The stub is one synchronous stderr write. A real broadcast cannot be awaited there; it must be handed to a task or channel from `poll_read`. Not built, not measured.
4. SIGTERM naive hang reproduced 10/10 (E11 result stands), and departure still fires at once in those cases: the hang is after the departure line.

## What could make this wrong

1. macOS only. Linux not run for E13. E11's 5003 ms matched on Linux; E13 reader behaviour there unmeasured.
2. Stub departure is a stderr line, not a gossip broadcast. Time to a peer seeing Gone depends on the real send, not measured. This shows the application learns at once, not that the broadcast completes in 1000 ms.
3. `poll_read` treats `Ready(Ok)` with zero bytes added as EOF. A caller passing a zero-capacity buffer would also read zero. rmcp reads through `FramedRead` (`rmcp:transport/async_rw.rs:483-485` is the test-module copy; the production `AsyncRwTransport` read path was not read line by line). No false departure seen in 120 runs plus 5 wire sessions, but a false departure under other buffer sizes is not excluded.
4. Pipe closed by the driver; a real harness may close stdin at other moments (or never, killing the child). A harness that sends SIGKILL gives no departure either way.
5. Resolution: driver reader thread stamps arrival; pipe and thread wake-up add well under 1 ms but are not separately calibrated. Departure 0 ms means "under measurement resolution", not zero.
6. Exit medians 1 ms above E11 on some rows, unexplained (see Measurement).
7. In-flight call is a sleep (E11 caveat 5 stands). Current-thread runtime only (E11 caveat 8). rmcp 3.5.1 only; 3.6.0 untested. Same driver and cases as E11, so any E11 bias carries over.
8. n=10 per case; 5 s and 2 s are rmcp constants, so low spread is expected, not evidence of robustness.
9. Wire identity checked on the scripted session only; no real harness, no rmcp client.
