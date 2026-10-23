# E7 gossip admission

Date 2026-10-16. iroh 1.3.0, iroh-gossip 0.101.0, rustc 1.99.1 (`rustup run 1.99.1 cargo`), loopback only, relays disabled, separate OS processes. Commit `e09a1ed` on branch `spike/e7-admission` (package `spikes/e7-admission/`, excluded from workspace; raw per-run logs in `spikes/e7-admission/logs/`). Machine: Apple M2 Max, macOS 15.3.1, arm64.

Setup. Members M1, M2, M3 on topic `blake3::derive_key("messaging topic v1", secret)`. M1 bootstraps nothing; M2, M3 bootstrap M1. Each payload (JSON: kind, node, seq, sent_us, nonce, mac) carries `blake3::keyed_hash(derive_key("messaging mac v1", secret), kind|node|seq|sent_us|nonce)`. Members check the MAC on every received payload and would drop failures; the harness logs every payload and its verdict at the point of delivery to the application, before the verdict. Outsider O: given the topic id and M1's EndpointAddr only; random MAC key; subscribes with bootstrap M1. Timeline per run: members form, M1 sends 20 pings (baseline), O starts and settles 5 s, each member announces Presence once, M1 sends 20 pings (with O), O sends 10 forged Presence (random MAC), stop. 3 runs per scenario.

Scenarios:
- `open`: members hold O's address (as discovery would give them).
- `closed`: members know O's id only, no address.
- `deny`: as `open`, plus every member installs an `EndpointHooks::after_handshake` hook rejecting O's id on `GOSSIP_ALPN`.
- `control`: member W holds the real secret (real MAC key), uses a wrong random topic id, bootstraps M1.

## Results

| Question | Measure | scenario | min | median | max | n |
|---|---|---|---|---|---|---|
| 1 | O `subscribe` returns Ok | open | 1 | 1 | 1 | 3 |
| 1 | O `subscribe` returns Ok | closed | 1 | 1 | 1 | 3 |
| 1 | O NeighborUp count | open | 1 | 1 | 1 | 3 |
| 1 | O NeighborUp count | closed | 1 | 1 | 1 | 3 |
| 1 | O neighbors other than M1 | open / closed | 0 | 0 | 0 | 3 / 3 |
| 1 | O start to first NeighborUp (ms) | open | 5.832 | 5.889 | 6.031 | 3 |
| 1 | O start to first NeighborUp (ms) | closed | 5.801 | 5.839 | 5.887 | 3 |
| 1 | Distinct members whose Presence O received (of 3) | open | 3 | 3 | 3 | 3 |
| 1 | Distinct members whose Presence O received (of 3) | closed | 3 | 3 | 3 | 3 |
| 1 | First member announce to O receives Presence (ms) | open | 2.183 | 2.295 | 2.469 | 3 |
| 1 | First member announce to O receives Presence (ms) | closed | 0.670 | 1.918 | 2.431 | 3 |
| 1 | Member pings O received (of 20) | open / closed | 20 | 20 | 20 | 3 / 3 |
| 2 | Forged sent by O | open / closed | 10 | 10 | 10 | 3 / 3 |
| 2 | Forged copies delivered to members (M1+M2+M3) | open / closed | 30 | 30 | 30 | 3 / 3 |
| 2 | Of those, MAC fails (dropped) | open / closed | 30 | 30 | 30 | 3 / 3 |
| 2 | Of those, MAC passes | open / closed | 0 | 0 | 0 | 3 / 3 |
| 2 | Copies delivered directly from O (at M1) | open / closed | 10 | 10 | 10 | 3 / 3 |
| 2 | Copies relayed by a member (at M2, M3; `from=M1`, `Swarm(Round(1))`) | open / closed | 20 | 20 | 20 | 3 / 3 |
| 3 | Members' NeighborUp for O | open / closed | 1 | 1 | 1 | 3 / 3 |
| 3 | Members' NeighborUp for any other peer after O started | open / closed | 0 | 0 | 0 | 3 / 3 |
| 3 | Members' NeighborDown, any peer, before / after O started | open / closed | 0 / 0 | 0 / 0 | 0 / 0 | 3 / 3 |
| 3 | M1 to M2,M3 pings delivered (of 40), baseline / with O | open / closed | 40 / 40 | 40 / 40 | 40 / 40 | 3 / 3 |
| 3 | Delivery latency median (ms), baseline | open | 0.591 | 1.951 | 2.180 | 3 |
| 3 | Delivery latency median (ms), with O | open | 1.576 | 1.715 | 1.884 | 3 |
| 3 | Delivery latency median (ms), baseline | closed | 1.602 | 1.689 | 1.828 | 3 |
| 3 | Delivery latency median (ms), with O | closed | 1.636 | 1.788 | 1.967 | 3 |
| gate | O `subscribe` returns Ok under hook | deny | 1 | 1 | 1 | 3 |
| gate | HOOK_REJECT events at members (total) | deny | 1 | 1 | 1 | 3 |
| gate | O NeighborUp / members' NeighborUp for O | deny | 0 / 0 | 0 / 0 | 0 / 0 | 3 |
| gate | Presence / pings O received | deny | 0 / 0 | 0 / 0 | 0 / 0 | 3 |
| gate | Forged copies delivered to members | deny | 0 | 0 | 0 | 3 |
| gate | Member-to-member pings delivered (of 40), baseline / with O | deny | 40 / 40 | 40 / 40 | 40 / 40 | 3 |
| 4 | W NeighborUp; members' NeighborUp for W | control | 0 / 0 | 0 / 0 | 0 / 0 | 3 |
| 4 | Payloads W received; payloads members received from W (W sent 5 valid-MAC pings) | control | 0 / 0 | 0 / 0 | 0 / 0 | 3 |
| 4 | Members formed on the real topic; pings delivered (of 40) | control | 1; 40 | 1; 40 | 1; 40 | 3 |

Answers.
1. Yes. O subscribes, gets one NeighborUp (M1 only), and reads every Presence and ping in plain JSON within about 2 ms of announce. The secret gates nothing at join time.
2. Members drop all forged payloads on MAC (30/30), but gossip forwards them first: for every forged payload, M2 and M3 each received a relayed copy through M1 (20 relayed copies for 10 sent). The MAC check runs after delivery and cannot stop forwarding.
3. No change observed: zero NeighborDown, no extra NeighborUp at members, 40/40 delivery with and without O. Latency medians vary 0.59 to 2.18 ms between runs of the same scenario, more than any with/without-O difference; no slowdown detectable at this resolution.
4. Control confirmed: W with the right secret and the wrong topic never met the members, in either direction.

## Gate options found in source (path:line)

Paths under `~/.cargo/registry/src/*/`.

- iroh-gossip has no join, admission, or peer-filter hook. `grep -i hook` over `iroh-gossip-0.101.0/src` finds nothing.
- Gossip is a `ProtocolHandler` with no remote-id check: `iroh-gossip-0.101.0/src/net.rs:130` (`accept` calls `handle_connection`); `net.rs:246` (`handle_connection` queues the connection); `net.rs:395-397` (the actor adds it, `ConnOrigin::Accept`, no condition); `net.rs:525` (`handle_connection` spawns the connection loop).
- HyParView accepts any joiner: `iroh-gossip-0.101.0/src/proto/hyparview.rs:307` (`Message::Join` dispatches to `on_join`); `hyparview.rs:380-383` (`on_join` adds the peer to the active view at high priority); `hyparview.rs:394-400` (ForwardJoin to the other active peers, no check).
- Library docs say the application owns the accept loop and may check before forwarding: `iroh-gossip-0.101.0/src/net.rs:79-81` ("check the ALPN protocol of incoming connections ... forward the connection to the gossip actor through `Self::handle_connection`"). A wrapper `ProtocolHandler` around `Gossip` can therefore refuse by `conn.remote_id()` before gossip sees the connection. Not built or tested here.
- `ProtocolHandler::on_accepting` early interception, may reject during handshake: `iroh-1.3.0/src/protocol.rs:248-256`. Not tested.
- `EndpointHooks`: trait at `iroh-1.3.0/src/endpoint/hooks.rs:68`; `before_connect(remote_addr, alpn)` for outgoing at `hooks.rs:79-84`; `after_handshake(&Connection)` for incoming and outgoing at `hooks.rs:102-106`, documented "we know the remote's endpoint id and ALPN"; installed with `Builder::hooks` at `iroh-1.3.0/src/endpoint.rs:787`; invoked at `iroh-1.3.0/src/endpoint/connection.rs:358-362` (rejected connection is closed with the given code and reason); `before_connect` invoked at `endpoint.rs:1124`. `Connection<HandshakeCompleted>::alpn()` at `connection.rs:1155` and `remote_id()` at `connection.rs:1167` expose both inputs.

Can EndpointHooks refuse a gossip-ALPN connection by remote id? Yes, measured: in `deny`, the `after_handshake` hook (`conn.alpn() == GOSSIP_ALPN && deny.contains(&conn.remote_id())`) rejected O's connection to M1 (1 HOOK_REJECT per run), O got no NeighborUp, received nothing, and none of its forgeries reached any member (0/10 delivered, 3/3 runs). But the hook sees only the remote id and the ALPN. It needs the id in advance, which is an allowlist or blocklist (blueprint D16 rules out both), and it has no view of the secret or the payload. A hook that authenticates by secret would need its own proof exchange on the connection; not tested.

## Surprises

- Outsider admission is total and instant: ~5.9 ms to NeighborUp, ~2 ms to the first Presence read. Presence is a MAC, not encryption (blueprint 3.7), so contents are readable by design; the topic id alone is the whole cost of entry.
- `closed` (members never given O's address) behaves identically to `open`. O dials M1 and M1 accepts; no address lookup of O is needed by members.
- O became a neighbor of M1 only, in both scenarios, even though M1 sends ForwardJoin to M2 and M3 (`hyparview.rs:394-400`). No NeighborUp for O appeared at M2 or M3.
- Under the deny hook, O's `subscribe` still returns Ok and no error event reaches O; it sees a silently empty topic. Members logged exactly one rejection per run in a ~25 s window; I did not see O retry.
- Forged copies at M2/M3 arrive as `scope=Swarm(Round(1))` with `delivered_from=M1`, so the application can see who handed it over, but cannot stop M1 from forwarding to others.

## What could make this wrong

- Three members on one host, loopback, 3 runs per scenario. Relay count of forged copies (20 = 2 members x 10) is topology-specific: O had one neighbor (M1). With larger swarms and several neighbors the relayed amplification would differ; I did not measure it.
- Latency compares wall-clock timestamps across processes on one machine; the run-to-run spread (0.59-2.32 ms) exceeds any effect. Per-run medians of 40 samples; with-O medians are not distinguishable from baseline here. A slowdown under load, with relays, or with many outsiders was not tested.
- "Plain text" is true because my payload is JSON with a MAC and no encryption, matching blueprint 3.7 as I read it. My payload omits the card signature and Claim; MAC input is my own format string, not the blueprint's.
- The harness logs a payload when gossip delivers it to the application. Whether the proto layer forwards a message independently of delivery is inferred from the relayed copies at M2/M3 arriving with `from=M1`, not read from the proto code path. I did not use iroh-gossip metrics.
- Control: absence of events at W and members shows no topic formed and no payload crossed. It does not show whether W's connection to M1 was opened and discarded; I did not log connection-level events.
- Deny hook: tested only for `after_handshake`, only one denied id, only on the incoming side at M1. `before_connect`, `on_accepting`, and the custom-accept-loop approach are cited from source, not run.
- The three `deny` runs each saw one rejection; a longer window might show retries.
- Timeline is fixed-sleep, not event-driven; members' Presence was announced once each after O had settled for 5 s. O joining mid-announcement was not tested.
