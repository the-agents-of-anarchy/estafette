# E3 crash detection timing

- Date: 2026-10-16
- Versions: iroh =1.3.0, iroh-gossip =0.101.0, noq(-proto) 1.3.0, rustc/cargo 1.99.1, tokio 1
- Commit: `1bf7209` on branch `spike/e3-kill`, worktree `/Users/ryzhakar/pp/estafette-e3-kill`, code in `spikes/e3-kill/` (excluded from workspace)
- Machine: `Darwin macmax.routerlogin.net 24.3.0 Darwin Kernel Version 24.3.0: Thu Jan  2 20:24:23 PST 2025; root:xnu-11215.81.4~3/RELEASE_ARM64_T6020 arm64`
- Setup: two OS processes, loopback `127.0.0.1`, relays disabled, no address lookup besides in-memory, addresses passed via argv/stdout. Watcher spawns victim, waits for NeighborUp, opens a second QUIC connection (own ALPN `e3/echo/0`) to victim with an open bi-stream (echo round trip done), waits 2 s, then kills (t0). All measures are relative to t0, capped at 120 s.

## Defaults found in source (file:line)

Paths under `~/.cargo/registry/src/*/`.

| Setting | Value | Source |
|---|---|---|
| QUIC connection `max_idle_timeout` | 30 000 ms | `noq-proto-1.3.0/src/config/transport.rs:560` |
| Connection keep-alive (noq default) | None | `noq-proto-1.3.0/src/config/transport.rs:578` |
| iroh overrides: connection keep-alive | 5 s (`HEARTBEAT_INTERVAL`) | `iroh-1.3.0/src/endpoint/quic.rs:157`, const `iroh-1.3.0/src/socket.rs:109` |
| iroh overrides: default path keep-alive | 5 s | `iroh-1.3.0/src/endpoint/quic.rs:158` |
| iroh overrides: default path max idle | 15 s (`PATH_MAX_IDLE_TIMEOUT`) | `iroh-1.3.0/src/endpoint/quic.rs:159`, const `iroh-1.3.0/src/socket.rs:117` |
| Relay path max idle | 30 s | `iroh-1.3.0/src/socket.rs:129` |

iroh does not override `max_idle_timeout`, so the connection-level idle timeout is noq's 30 s.

Observed: connection close event on SIGKILL is `TimedOut` at 30.002 to 30.003 s. That matches the 30 s connection idle timeout. It does not match the 15 s path idle timeout. Why the path timeout did not close the last path at 15 s is not established here (not investigated in source).

Observed: NeighborDown arrives about 3.0 s after the connection close, in both default and tuned runs. Source of the ~3 s gossip delay not found (no matching constant in `iroh-gossip-0.101.0/src/proto/hyparview.rs`, `net.rs`, `net/util.rs`; `neighbor_request_timeout` is 500 ms at hyparview.rs:218, shuffle 60 s at :216).

## Runs

All times ms from t0. Trials of each scenario ran concurrently (13 processes at once on one machine), separate ephemeral ports. Plus one earlier solo pilot (default SIGKILL): NeighborDown 32 999, close 30 002, fresh connect 30 002, stream 30 001. Pilot is not in the tables.

| Scenario | Measure | min | median | max | n |
|---|---|---|---|---|---|
| Default, SIGKILL | NeighborDown (victim) | 32 975 | 32 994 | 32 996 | 5 |
| Default, SIGKILL | QUIC connection closed event (`TimedOut`) | 30 002 | 30 002 | 30 003 | 5 |
| Default, SIGKILL | Fresh `connect` fails (`timedout`) | 30 002 | 30 002 | 30 003 | 5 |
| Default, SIGKILL | Request on open bi-stream fails (`ConnectionLost(TimedOut)`) | 30 002 | 30 002 | 30 003 | 5 |
| Default, SIGTERM clean stop | NeighborDown | 2 | 3 | 4 | 5 |
| Default, SIGTERM clean stop | Connection closed (`ApplicationClosed`, code 0) | 3 | 3 | 4 | 5 |
| Default, SIGTERM clean stop | Fresh `connect` fails | 3 | 4 | 30 004 | 5 |
| Default, SIGTERM clean stop | Request on open bi-stream fails | 3 | 3 | 4 | 5 |
| Tuned 5 s keep-alive / 10 s idle, SIGKILL | NeighborDown | 12 990 | 12 996 | 12 996 | 3 |
| Tuned, SIGKILL | Connection closed (`TimedOut`) | 10 002 | 10 003 | 10 012 | 3 |
| Tuned, SIGKILL | Fresh `connect` fails (`timedout`) | 10 003 | 10 004 | 10 014 | 3 |
| Tuned, SIGKILL | Request on open bi-stream fails | 10 002 | 10 003 | 10 012 | 3 |

Notes on the data:
- Clean-stop fresh connect: 4 of 5 failed in 3 to 5 ms with `aborted by peer: the server refused to accept a new connection`. One run (max, 30 004) raced the shutdown, dialed while the victim was still closing, and failed by handshake timeout. Kept in the table.
- Fresh-connect failure time equals the idle timeout in both configs (30 s default, 10 s tuned). Consistent with the handshake being bounded by `max_idle_timeout`; not verified in source.
- NeighborDown never failed to arrive within 120 s in any run.

## Tuning API and its effect

API (`iroh::endpoint::QuicTransportConfig::builder()`, passed with `Endpoint::builder(..).transport_config(cfg)`, `iroh-1.3.0/src/endpoint.rs:680`):
- `max_idle_timeout(Some(IdleTimeout))` at `iroh-1.3.0/src/endpoint/quic.rs:212`. `IdleTimeout` from `Duration` via `try_into()`.
- `keep_alive_interval(Duration)` at `quic.rs:366`.
- `default_path_keep_alive_interval(Duration)` at `quic.rs:514`. Values above 5 s are ignored with a warning (`quic.rs:515-520`). Can only go lower.
- `default_path_max_idle_timeout(Duration)` at `quic.rs:492`. Values above 15 s are clamped (`quic.rs:493-501`). Can only go lower.

Run config (both watcher and victim): keep-alive 5 s (connection and path), `max_idle_timeout` 10 s, path idle 10 s.

Effect, SIGKILL, n=3: connection close 30 s to 10 s, NeighborDown 33 s to 13 s. Detection tracks `max_idle_timeout` plus a constant ~3 s for gossip. Keep-alive was already 5 s by default, so that setting changed nothing. I did not test other idle values, so linearity below 10 s is unmeasured.

## What could make this wrong

- Loopback only, two processes on one host. No packet loss, no NAT, no real path RTT. Real networks may add ICMP or path-change behavior.
- SIGKILL on macOS: the kernel closes the victim's UDP socket. Loopback ICMP port-unreachable did not shorten detection here, but behavior on Linux or over a real link may differ.
- Direct-address path only. With a relay configured, a relay path (30 s idle, `socket.rs:129`) and relay-side notification could change timings. Not tested.
- 13 processes ran concurrently; CPU contention is a possible minor confound for the millisecond-level clean-stop numbers. Results are tight, so I judge it small, but did not test solo for clean/tuned.
- Small n (5, 5, 3). Spread is tiny, but that says nothing about tail behavior.
- The ~3 s gap between connection close and NeighborDown is observed, not explained. It could be a gossip-internal timer I did not find, or event-loop delay.
- Close event measured on a separate probe connection (own ALPN), not on the gossip connection itself. Gossip's own connection close time is inferred from NeighborDown, not observed directly.
- Fresh-connect measure started at t0 simultaneously with the other measures; concurrent attempts to a dead peer may interact (not tested).
- Debug build, not release. Should not affect timeouts, but not verified.
- Tuned run did not include a SIGTERM scenario, and only one tuned setting pair (5 s / 10 s) was tried.
- iroh 1.3.0 path idle (15 s) not closing the connection before 30 s is an unexplained observation; a different mechanism than I assume may be at work.
