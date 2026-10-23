# E14 why fresh dials fail after a relay host dies

Date 2026-10-20. Rust 1.99.1, debug build, iroh =1.3.0, iroh-relay =1.3.0, iroh-gossip =0.101.0 (E12 harness only), preset `Minimal`. Apple M2 Max, macOS, one host, loopback, SIGKILL of relay processes.
Branch `spike/e14-dials` (from `spike/e12-rejoin`), worktree `/Users/ryzhakar/pp/estafette-e14-dials`, pushed, commit `b849d40`. Foreground only; no process left running (checked with pgrep).
Paths below are relative to the iroh-1.3.0 crate source unless noted.

## Answer

Head-of-line blocking in the socket's relay send path. One `RelayActor` task serves all relays' outgoing datagrams. It hands each datagram to a per-relay `ActiveRelayActor` through a 64-slot queue (`src/socket/transports/relay/actor.rs:1359`). When the dead relay's queue is full, `RelayActor` waits on that queue and takes no more datagrams from anyone (`actor.rs:1137,1159,1172,1207-1233`, comment at 1136-1137: "We can not process further datagrams during this time"). The dead relay's actor does not drain its queue: it loops dial (fails at once, connection refused) then `sleep_backoff` (up to 16 s, jittered), and `sleep_backoff` reads only the priority inbox (`actor.rs:366-389`). The only drain is the 3 s flush tick inside `run_dialing` (`actor.rs:460-523`), which a dial that fails in under 1 ms never reaches. So every datagram, including those for R2, queues behind R1. Fresh dials time out; existing connections starve and idle out at 10 s.
What ends it: after 60 s since the last `reset_inactive_timeout` (`actor.rs:429-432`, `68`) the dead relay's actor exits at its next dialing entry (`actor.rs:524-527`); its queue closes, the blocked send fails and logs `WARN Dropped datagram(s): ActiveRelayActor closed` (`actor.rs:1216-1226`), `RelayActor` is free. Recovery came at 70.5 s (gossip trace) and 73.7 s (repro, traffic stopped at 25 s). If anything still sends toward the dead relay, a new actor is created (`actor.rs:1322-1337`) and the block returns (repro `p1`: no recovery in 90 s).
Trigger for a member is traffic addressed to the dead relay URL, not the member's own home. Rate decides when: 1 dial/s toward the dead peer fills the queue at about 17 s after the kill, 5/s at about 4 s.

## Evidence

1. Real harness trace (E12 harness, scenario 1, probers on, no levers, run `a-1-201`, `E12_TRACE=iroh=debug,iroh_relay=debug,iroh_gossip=debug`; gz traces `spikes/e10-gossip-main/logs/diag/e14-a-1-201-M2.log.gz`, `-M3.log.gz`). Kill at 19:24:31.233. Run stalled: M2-M3 gap 74.3 s, M2 to M3 fresh dial ok again at 70.5 s.
   - M2 and M3 hold an `active-relay{url=R1}` actor (non-home, created at startup, `Adding relay connection ... 52447`). After the kill it logs `Lost connection ... Stream closed by server`, then `Connection refused`, `retry in` 19 ms, 34 ms, ... 2.07 s, 2.86 s, 10.1 s, 10.4 s, 18.4 s, 25.0 s (M2, from the trace).
   - Fresh probes M2 to M3 over R2 succeed each second until 19:24:44.06 (13 s after the kill), then the dial at 45.064 gets no `connected` event; M3 logs `incoming` from M2 at 45.069 and no handshake completes (`aborted by peer ... during the handshake` at 48.067). Last heartbeat M2 from M3 at 13.7 s after the kill; M3 from M2 at 23.8 s (10 s idle after M2's sends stopped).
   - 19:25:41.707 M2: `Inactive, exiting. RELAY_INACTIVE_CLEANUP_TIME=60s` on the R1 actor (its last `retry in` was 24.98 s, ending 41.704), then 19:25:41.708 `WARN Dropped datagram(s): ActiveRelayActor closed url=R1`. The first fresh dial after that, started 19:25:42.784, succeeded. Gossip also sends toward R1-homed peers (B1, M1), so M2 produced such traffic itself.
   - Not seen: `Dropping datagrams to send` (the flush) in 0 of 3 member traces.
2. Minimal repro without gossip (`spikes/e14-dials`, run `cd spikes/e14-dials && ./target/debug/e14-dials run <variant> <window_s>`; logs `spikes/e14-dials/logs/`). R1 and R2 are separate OS processes, SIGKILL on R1. X and Y homed on R2 (relay map R1+R2), P homed on R1. X dials P with a fresh connection N times per second (3 s timeout, echo); every second X dials Y and Y dials X (3 s timeout). Failure is a timeout in every case; no error other than `timeout3s` was logged.

| Variant (X toward P after the kill) | runs | first failed X-Y dial started at (s after kill) | recovered |
|---|---|---|---|
| `ctl` stops dialing P at the kill | 1 | none in 60 s | n/a |
| `p1` 1 dial/s, never stops | 3 (90 s, 40 s, 90 s) | 17.05 / 17.05 / 17.05 (first fail logged at 20.05, dial is 3 s) | 0/3 (90 s window; R1 actor exit at 76.5 s, block returns) |
| `p5` 5 dials/s | 1 (40 s) | about 4.0 (logged 7.01) | 0/1 |
| `p1s5` stops dialing P at 5 s | 2 (90 s, 50 s) | none | n/a |
| `p1s25` stops dialing P at 25 s | 1 (110 s) | 17.06 | dial started 70.6, logged ok at 73.7 (R1 actor exit at 20:41:59.00, WARN same ms) |
| `p1rm` X `remove_relay(R1)` at 0.5 s | 2 (60 s, 50 s) | 17.04 / 17.04 | 0/2 |
| `p1rmall` X and Y `remove_relay(R1)` at 0.5 s | 1 (50 s) | 17.03 | 0/1 |
| `p1a5` P's lookup entry removed at 5 s, dialing continues | 1 (90 s) | 17.05 | 0/1 (actor exit and WARN at 76.5 s, then block again) |

   Y to X fails together with X to Y in every stalled run: X's `RelayActor` is blocked, so X cannot send its replies. Y is not blocked by anything of its own (Y sends nothing toward R1 except the connection keep-alives; Y's own queue is not the cause).
   Relay trace of `p1` (`logs/trace-p1-relay.log.gz`, filter `iroh::socket::transports::relay=debug`): same sequence as in item 1: `retry in` growing to 21 s, `Inactive, exiting` at 20:34:00.491 plus `WARN Dropped datagram(s)` 0.14 ms later, new actor retrying at 16 ms again.

## Hypotheses and verdicts

- Head-of-line block in the socket send path behind the dead relay's actor: supported. Evidence: items 1 and 2; control `ctl` and `p1s5` (no traffic toward R1 before the queue fills) do not stall; rate scaling 17 s to 4 s; recovery coincides to the millisecond with the dead actor's exit and the WARN. Causality is by intervention (traffic stopped early: no stall; stopped late: recovery only at the actor exit), not by a direct read of the queue length; I did not instrument the library.
- Stale relay URLs in address info routing the dial through R1: ruled out for the dials that fail. The probe dials log `connecting relay_url=None ip_addresses=[]`; the paths that opened were `Relay(http://127.0.0.1:52448/ ...)` (R2) until the stall. In the repro the X-Y dial targets only R2 addresses and fails anyway. R1 matters only as a destination of other datagrams.
- Inactive cleanup after 60 s: it is the recovery trigger, not the cause. It does not prevent the stall (the stall begins at 4-17 s); it ends it only if nothing sends to the dead relay again (`p1s25` yes, `p1` no).
- Dead peers' relay traffic as the trigger: supported by `p1s5` versus `p1`.
- QUIC-level or relay-server side (R2) fault: not supported; R2 stays up, the connection-refused loop is only at R1, and the stall clears when the R1 actor closes. R2 server had no tracing; its logs were not read.

## Counter-measure measured

- Works: do not send toward the dead relay. In the repro, stopping the dials to P at 5 s after the kill (before the queue filled) gave 0 failed X-Y dials in 2 runs (90 s, 50 s). Stopping at 25 s (after the stall began) recovered only when the dead actor exited, 73.7 s after the kill.
- Does not work: `Endpoint::remove_relay(R1)` on X (2 runs) or on X and Y (1 run): stall starts at 17 s unchanged, never recovers in 50-60 s. Reading: `Socket::remove_relay` (`src/socket.rs:1245-1252`) only edits the relay map and triggers a net-report; the `RelayActor` handlers I read (`actor.rs:1160-1233`) send to `item.url` and I found no code that closes the active actor of a removed relay. Not exhaustively checked.
- Does not work: removing P from the `MemoryLookup` (1 run). Stall unchanged. Likely the remote-state actor keeps P's addresses once learned (`src/socket/remote_map/`); not verified.
- Not built or tested in the gossip harness: a member-side rule that stops dials to peers homed on a relay seen dead. The repro shows the effect only for traffic the application controls (fresh dials). Traffic from existing QUIC connections to B1 or M1 (retransmits, keep-alives until the 10 s idle close) and gossip's own dials to B1/M1 (E12: gossip dial started 7-30 s after the kill) were not separated; in the E12 trace M3 dialed B1 at 13.1 s.
- Consequence for E12: L1 `remove_relay` at M1 only re-homes M1; it cannot help M2 and M3, whose stall is the actor above. E12's L2 (`join_peers`) starts more dials toward dead peers, which feeds the queue.

## Surprises

- The victim of the block is a member that never lost its home relay and never talks to the dead relay on purpose; the dead relay only needs to be a destination of a few datagrams per second.
- A single dead relay stops all outgoing relay traffic of an endpoint, including replies to peers on the live relay (Y to X fails with X).
- `remove_relay` and lookup removal, the two obvious application levers, do nothing to a running block.

## What could make this wrong

- The queue-full state is inferred, not observed. A direct observation needs a library patch or a debugger (not done, rules forbid patches). Alternative readings consistent with the data: another blocking point in the same actor that clears when the R1 actor closes. The WARN at the exact exit instant, the early-stop control, and the rate scaling favor the queue reading.
- The `Inactive` exit time depends on when `reset_inactive_timeout` last ran; I did not trace it (`actor.rs:515` flush, `:672` while connected). Observed exits: 70.5, 73.7(+), 76.5 s after the kill; E10 saw 79-158 s.
- Repro is n=1-3 per variant, one host, debug build, loopback; the 17 s and 4 s numbers depend on how many datagrams one failed dial emits on this machine. Counters were tried once or twice each.
- Repro runs both X and Y with their own R1 actor in one process; the real members are separate processes. The real trace (item 1) matches, but only one run was traced (stalled; M2 and M3 both).
- The E12 trace run `a-1-201` used the harness unchanged and left L1/L2 off; the table above is not a rerun of E12's matrix.
- "No code closes the removed relay's actor" is a reading of the code paths I opened, not a full search of iroh 1.3.0.
- iroh-gossip 0.101.0 involvement: gossip issues the dials and holds the connections; its code was not the blocker (traces show gossip's `connect` simply pending at `net.rs:1026`).
