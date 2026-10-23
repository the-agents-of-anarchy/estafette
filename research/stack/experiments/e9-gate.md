# E9 secret-proof gossip gate

Date 2026-10-20. iroh 1.3.0, iroh-gossip 0.101.0, blake3 1, rustc 1.99.1 (`rustup run 1.99.1 cargo`), loopback only, relays disabled, separate OS processes, debug build. Commit `76b5219` on branch `spike/e9-gate` (package `spikes/e9-gate/`, excluded from the workspace; raw per-run logs in `spikes/e9-gate/logs/`). Machine: Apple M2 Max, macOS 15.3.1, arm64.

Setup. Members M1, M2, M3 derive `topic = derive_key("messaging topic v1", secret)` and `gate_key = derive_key("messaging gate v1", secret)`. M2, M3 bootstrap M1. Payloads and MAC as in E7. Outsider O: topic id and M1's address only, fresh identity, no secret. Timeline per run: members form; M1 pings x20; O starts, settles 5 s; each member announces Presence; M1 pings x20; O sends 10 forged Presence; M3 killed and restarted (same key, new port, bootstraps M1) then M1 pings x10; member M4 (real secret) bootstraps on O only (gossip dials O), announces Presence. 3 runs per scenario, 6 scenarios, one driver process per scenario.

## Design (what was built, lines, why this shape)

Proof of one challenge: verifier V sends a 32-byte random nonce on a bi-stream it opens on the QUIC connection; prover P answers `blake3::keyed_hash(gate_key, nonce || V_id || P_id)` (as a keyed Hasher, three updates); V compares (constant-time `Hash ==`). The ids bind the proof to the pair and to the direction. Timeout 3 s. Failure closes the connection with code 403.

Two variants, both measured, share `challenge`/`respond` (`spikes/e9-gate/src/gate.rs`):

- `hook`: one `EndpointHooks::after_handshake` on every endpoint. For gossip-ALPN connections, in both directions on both sides, it runs `challenge` and `respond` concurrently (`try_join!`) and returns Accept or Reject. The hook is awaited inside the connecting future before the `Connection` is returned (`iroh-1.3.0/src/endpoint/connection.rs:344-362`), so neither `Router::accept` nor gossip's own dialer sees an unproven connection.
- `wrapper`: `GatedGossip`, a `ProtocolHandler` on `GOSSIP_ALPN` that runs `challenge` (acceptor verifies dialer), then calls `Gossip::handle_connection` (the accept-loop route in `iroh-gossip-0.101.0/src/net.rs:79-81`); on failure it closes and never calls gossip. Dialer side: a one-way hook answers (`respond`) on client-side connections. The acceptor is not verified by the dialer.

Lines: `gate.rs` 108 lines, 91 non-blank non-comment, of which about 25 are `GateHook` and 15 `GatedGossip` (including 3 for the test-only `lax` flag). Wiring in `node.rs`: 18 added lines (E7 diff, with the E7 deny hook removed). Either variant alone is about 60 lines of gate (shared 40 + hook 25 or wrapper 15 + one-way hook 20).

Why this shape: `after_handshake` is the only point that sees gossip's outgoing connections (see next section) and costs no change to gossip. The wrapper is the owner-ruling shape ("our own accept loop") and is smaller on the accept side but cannot cover the dial side alone. No new dependency: blake3, iroh, tokio, anyhow already in E7's tree; nothing pulled.

## Results

Per-scenario run values are min / median / max over 3 runs. "plain O" = outsider runs no gate code. "wrong O" = outsider runs the same gate variant with a random key. "liar O" = outsider runs the wrapper acceptor with verification disabled (`lax`). Baseline `none` = no gate, plain O (E7 `open` equivalent).

Outsider O (counts over the whole run; forged sent = 10):

| Measure | none | hook, plain O | hook, wrong O | wrapper, plain O | wrapper, wrong O | n |
|---|---|---|---|---|---|---|
| O `subscribe` Ok | 1/1/1 | 1/1/1 | 1/1/1 | 1/1/1 | 1/1/1 | 3 each |
| O NeighborUp for members | 2/2/2 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Members' NeighborUp for O | 1/1/1 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Payloads O received from members | 33/33/33 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Payloads members received from O (any) | 30/30/30 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Forged copies delivered to members (M1,M2,M3,M3r) | 30/30/30 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Other gossip events at O (NeighborDown, Lagged, errors, stream end) | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 3 each |
| Gate failures logged at members (original three, whole run) | 0 | 1/1/1 | 1/1/1 | 1/1/1 | 1/1/1 | 3 each |
| M1-to-M2,M3 pings delivered while O present (of 40) | 40/40/40 | 40/40/40 | 40/40/40 | 40/40/40 | 40/40/40 | 3 each |

Members (all gated scenarios and baseline), times in ms:

| Measure | none | hook | wrapper | n |
|---|---|---|---|---|
| Time to joined (first START to last member's first NeighborUp), plain-O runs | 7.460 / 7.474 / 8.060 | 8.137 / 8.163 / 8.461 | 7.511 / 8.028 / 8.530 | 3 each |
| Time to all-presence (max over 6 announcer-receiver pairs), plain-O runs | 1.012 / 1.039 / 1.087 | 1.005 / 1.013 / 2.311 | 1.001 / 2.425 / 3.287 | 3 each |
| Presence pairs delivered (of 6) | 6/6/6 | 6/6/6 | 6/6/6 | 3 each |
| Member restart: START to NeighborUp, plain-O runs | 6.216 / 6.339 / 6.480 | 6.852 / 7.117 / 7.214 | 6.634 / 6.726 / 7.022 | 3 each |
| Restart: pings received after rejoin (of 10) | 10/10/10 | 10/10/10 | 10/10/10 | 3 each |

Gate handshake time (time spent in the gate per gated connection, member side; 8 successful gate events per run, 24 per scenario). Per-run extremes then min / median / max across the 3 runs of the plain-O scenario:

| Variant | per-run min (ms) | per-run median (ms) | per-run max (ms) | n runs |
|---|---|---|---|---|
| hook | 0.758 / 1.028 / 1.168 | 1.558 / 1.656 / 1.739 | 1.782 / 2.080 / 2.281 | 3 |
| wrapper | 0.560 / 0.688 / 0.967 | 1.206 / 1.473 / 1.513 | 1.677 / 1.746 / 2.029 | 3 |

Added handshake latency is the gate time: about 1 to 2 ms per gossip connection on loopback in a debug build. It was measured inside the hook or wrapper, not as a difference of two end-to-end connect times. The `hook` figure for the dialing side includes waiting for the peer's stream; both sides run concurrently.

Dial-out by a member toward an outsider acceptor (M4 bootstraps on O; counts over 3 runs):

| Scenario | M4 NeighborUp | O NeighborUp for M4 | M4's Presence read by O | gate failure at M4 |
|---|---|---|---|---|
| none | 1/1/1 | 1/1/1 | 1/1/1 | 0 |
| hook, plain O | 0/0/0 | 0/0/0 | 0/0/0 | 1/1/1 (3.001-3.002 s timeout) |
| hook, wrong O | 0/0/0 | 0/0/0 | 0/0/0 | 1/1/1 (`bad proof`, 1.7 ms) |
| wrapper, plain O | 0/0/0 | 0/0/0 | 0/0/0 | 1/1/1 (3.0 s timeout, see Surprises) |
| wrapper, wrong O | 0/0/0 | 0/0/0 | 0/0/0 | 0 |
| wrapper, liar O | 1/1/1 | 1/1/1 | 1/1/1 | 0 |

Answers.
1. Both variants stopped the outsider completely: 0 NeighborUp either side, 0 payloads either way, 0 of 10 forgeries delivered (3/3 runs, plain and wrong-key O), against 30/30 forgeries and 33 payloads read by O in the ungated baseline run in the same harness.
2. Members form, announce, and restart through the gate at about the same speed as ungated; the join-time and presence differences (under 1 ms, and a 3.3 ms max presence outlier in wrapper) are within run-to-run spread at n=3.
3. What O observes at the gossip layer: `subscribe` returns Ok, then nothing, in every gated scenario. At the connection layer O saw nothing logged for plain O (I did not log connection events there). Wrong-key O in `hook` logged `GATE_FAIL dir=out why=bad proof` on its own side (about 1.6 ms); in `wrapper` it logged a failure on the inbound path only for M4's dial (M4 proves correctly, O fails the check against its wrong key, closes).
4. `wrapper` does not protect a member that gossip dials toward a lying acceptor: M4 joined O and O read M4's Presence (liar O row). `hook` rejects that dial in every row.

## Dialer side: how gossip's outgoing connections are proven

Gossip dials internally with no hook point: the actor's `Dialer` produces the connection (`iroh-gossip-0.101.0/src/net.rs:430-434`, `dialer.next_conn()` then `handle_connection(peer_id, ConnOrigin::Dial, conn)`); the library doc says so (`net.rs:83`, "will, however, initiate new connections to other peers by itself"). The dialer cannot be made to answer a challenge from inside gossip. Two ways it can be proven anyway, both built and measured:

1. `EndpointHooks::after_handshake` runs on outgoing connections too and receives `&Connection` before `Endpoint::connect` returns it to gossip. The hook can `accept_bi` and answer (what `wrapper` uses on the dial side), and can also open its own stream and verify the acceptor (`hook` mode, both directions). A rejected dial surfaces to gossip as a failed dial, so gossip never receives the connection.
2. `before_connect(remote_addr, alpn)` sees only address and ALPN; it cannot prove anything and was not used.

Dialer-proves-itself works in both variants (members dial members through the gate in every run: M2, M3, M3r joins). Dialer-verifies-acceptor works only in `hook` mode.

## Surprises

- `wrapper` with a plain acceptor still failed M4's dial, after exactly 3 s: M4's one-way hook blocks in `accept_bi` waiting for a challenge, so an acceptor that never challenges times out. I expected the dial to succeed and leak. The protection is accidental: a liar that sends any nonce and admits all (`lax`) passes it, and M4 then joined O and O read Presence.
- A plain outsider holds one member connection for the full 3 s timeout before rejection (`GATE_FAIL ... ms=3001`, `why=timeout`). One such attempt per run was seen at M1 in a ~25 s window, no retry. Unauthenticated connections cost the member a task and a connection for 3 s each; a flood was not tested.
- Gate runs both ways at once in `hook` mode without deadlock; each side opens its own bi-stream and accepts the peer's; streams become visible to the peer only on first write, so open-then-accept ordering did not matter.
- `gate_ok_events` is 8 per run for members. Expected 8 if each of four connections (M2-M1, M3-M1, M3r-M1, and one M2-M3/M3r link) logs one event per side; I did not verify the fourth connection from the logs.
- First full run of the gate passed immediately. To guard against a harness that cannot see leaks, the ungated `none` scenario in the same driver shows the outsider admitted (33 payloads read, 30 forgeries delivered), so the zero rows are not a blind harness.

## What could make this wrong

- Loopback, one host, 3 runs per scenario, debug build. Latencies of 1-2 ms are loopback handshakes; over a network they add at least one round trip and the 3 s timeout interacts with real RTT. The presence and join differences between variants are not distinguishable from run-to-run spread.
- The gate time is measured inside the gate, not as end-to-end connect time with and without a gate. Dialer-side hook time includes waiting for the peer.
- "Never reaches the gossip layer" is shown by zero NeighborUp and zero payloads at members and by the code path (`after_handshake` awaited before return; wrapper returns before `handle_connection`); I did not read iroh-gossip metrics or trace the proto layer.
- Plain O's connection-level view (close code, reason, timing) was not logged. O's retry behaviour over a longer window was not measured; M1 logged one failure per run.
- Security claims are untested adversarially: no active MITM, replay, or reflection attack was run. The ids in the proof are meant to defeat relay and reflection of an honest member's answer; I did not try. A captured (nonce, proof) pair allows an offline guess of the secret, so the secret must be high-entropy; the keyed hash is not a PAKE.
- The gate authenticates once per connection. A later compromise of an admitted member is a member compromise; this gate does not address it. Presence remains a MAC (not encrypted); admitted peers read everything.
- `lax` liar is my own construction of a lying acceptor. It stands for any non-member acceptor; real attackers could behave differently (for example, close early, hold the connection, or answer partially).
- Only the gossip ALPN is gated. Other ALPNs on the same endpoint are untouched by design.
- The 8 gate events per run were not reconciled one-by-one against the expected connection count; see Surprises.
- Restart reuses the identity key and a new port with updated addresses injected into M1 and M2 by the harness; discovery of a restarted member's new address was not exercised. M1 may still have held the stale connection until idle timeout, since the killed process sent no close.
