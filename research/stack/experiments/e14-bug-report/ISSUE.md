## One shared RelayActor awaits a dead relay's full queue and stalls all relay sends, including to live relays (1.3.0, main 55908a5)

**Summary.** With a relay map of 2+ relays, once about 65+ datagrams have been addressed to a relay that is down, every `connect` and every reply through the *other*, live relay times out until the dead relay's connection actor exits (61-80 s), and often again after that. Affects iroh 1.3.0 and main `55908a5809b61c72ec8deeff024038f4a9d68ab7` (iroh 1.4.0); `actor.rs` is byte-identical in both.

**Repro** (one file, no gossip, macOS and Linux, Rust 1.99.1):
```sh
cd repro/hol-repro && cargo run --quiet     # prints RESULT PASS|FAIL, exit 1 on fail
```
Two in-process relays R1/R2, endpoints X and Y homed on R2. X dials Y (ok, 5 ms), R1 is shut down, X starts 200 abandoned `connect`s to an id addressed via R1, then X dials Y once a second. Counts: 1.3.0 20/20 FAIL on macOS, 19/20 on Linux (Ubuntu 24.04, 2 vCPU); main 20/20 and 18/20 FAIL. Controls: R1 alive 20/20 PASS, no burst 10/10 PASS, burst 64 10/10 PASS, burst 100 and 150 10/10 FAIL; a relay url that was never up fails too.

**Observed vs expected.** Expected: X to Y (both on live R2, never naming R1) stays at single-digit ms. Observed: every dial times out (3 s); Y to X fails too because X cannot send the reply. No success in 150-200 s at burst 200 (blocked intervals 71.5 s, 76.8 s, then blocked again); first success 140 s after burst 100. Nothing is logged while blocked.

**Mechanism** (`iroh-1.3.0/src/...`; main has the same lines in `actor.rs`, `socket.rs` +3):
1. All relay datagrams go through one 256-slot channel (`socket/transports/relay.rs:54`, `286-317`) to one `RelayActor` task (`relay/actor.rs:1130-1177`), which `try_send`s to a 64-slot per-relay queue (`actor.rs:1206-1232`, queue `1360`).
2. On a full queue it returns a future awaiting `send()` with no timeout and stops polling the channel (`actor.rs:1220-1227`, `1137`, `1159`, comment `1135-1136`), so no datagram for any relay moves.
3. The dead relay's `ActiveRelayActor` loops dial (refused at once), then `sleep_backoff` (10 ms-16 s, `actor.rs:326-389`), which selects only on `stop_token`, `prio_inbox`, the sleep and never drains the queue. The 3 s flush (`actor.rs:460-463`, `514-522`) lives in `run_dialing`, which a refused dial leaves first.
4. The block ends when `inactive_timeout` (60 s, `actor.rs:68`, polled at `524-527`) fires at the first dial after 60 s: actor exits, `send()` fails, `WARN Dropped datagram(s): ActiveRelayActor closed`. The next channel items re-create the actor (`actor.rs:1322-1337`) and refill its queue, so it re-blocks within 1 ms while a backlog remains.
5. `remove_relay` only edits the map and re-stuns (`socket.rs:1245-1252`, `1759-1762`; main `1248-1255`, `1762`); `actor.rs` reads the map once, for the auth token (`1346`). Measured no effect.

**Causal proof.** A vendored 1.3.0 with logging (`repro/hol-repro-patched`, `HOL_DIAG=1`) prints `try_send_datagram Full queue_len=64 max=64`, `RelayActor BLOCKED`, and `queue_len=64` at every backoff entry; UNBLOCKED coincides with the actor's `Inactive, exiting` and the WARN. Raw logs in the package.

**Candidate fixes** (diagnostic, not reviewed; 20/20 PASS each on macOS and Linux; the same patched build with neither fix enabled fails):
- `drain`: in `sleep_backoff`, add a `select!` branch that receives and discards `relay_datagrams_send` (like the existing 3 s flush). Small; keeps backpressure for connected relays; RelayActor blocked for 1 ms at burst 2000.
- `drop`: in the Full branch of `try_send_datagram`, log and drop instead of awaiting. One relay can never stall another; loses backpressure for a live-but-slow relay (a per-relay policy would be safer).

**Workarounds measured.** Stop addressing peers on the dead relay before the queue fills: no stall (2/2). Close and rebind the endpoint: recovers in 5-7 s (5/5 stalled runs), drops all connections. `remove_relay` and removing the peer from `MemoryLookup`: no effect.

**Package.** `README.md` (full report, diagrams, caveats), `repro/` (code, patch, `vendor.sh`), `results/` (raw macOS and Linux outputs), `run.sh` (all counts).
