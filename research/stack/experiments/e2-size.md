# E2 gossip frame size

Date: 2026-10-16. Rust 1.99.1. iroh =1.3.0, iroh-gossip =0.101.0, postcard =1.1.3 (alloc), tokio 1, serde 1.
Commit: `1117a66` on branch `spike/e2-size`, worktree `/Users/ryzhakar/pp/estafette-e2-size`, package `spikes/e2-size/` (excluded from workspace).
Spec: `spec/blueprint.md` 3.1, 3.4, 3.7, 3.14.

Worst case built: handle 32 chars (`abcdefghijklmnopqrstuvwxyz012345`); effective handle 53 chars (handle + `-` + `18446744073709551615`); `public` exactly 4096 bytes; every u64 = u64::MAX (10-byte varint); `v` = 1; `attached` = true; all byte arrays 0xff. Types are exactly as the spec writes them; `v` is `u8`, `sig` is `[u8; 64]` via a custom serde tuple (serde derives only reach 32).

## Sizes (postcard)

| Structure | Bytes |
|---|---|
| Claim | 172 |
| Presence | 4368 |
| Departure | 107 |
| Unreachable | 279 |
| Payload::Presence (enum, 1-byte tag) | 4369 |
| Payload::Departure (enum) | 108 |
| Payload::Unreachable (enum) | 280 |

Spec writes `Payload = Presence | Departure | Unreachable` with no tag encoding. The enum rows are my assumption (postcard enum, 1-byte tag). The gossip runs below send bare Presence (4368), as tasked. Presence dominates; its 4368 = 4096 `public` + 2-byte length + 270 other.

## Threshold runs

Setup: two endpoints, one process, `RelayMode::Disabled`, direct addresses exchanged through `MemoryLookup`, `Router` per endpoint, same topic, both `subscribe` with the other as bootstrap, both `joined()` true. Both gossip instances use the same `max_message_size`. A broadcasts the 4368-byte Presence. Window: 4 s. "Received" = B got identical bytes.

| max_message_size | Received | Sender observed |
|---|---|---|
| 4096 (default) | no | `broadcast()` returned Ok. No error, no warn/error log, no NeighborDown, connection stays open. |
| 4368 (= payload) | no | same |
| 4369 | no | same |
| 4370 | no | same |
| 4372 | no | same |
| 4376 | no | same |
| 4384 | no | same |
| 4392 | no | same |
| 4400 | no | same |
| 4408 | yes | `broadcast()` Ok |
| 4416 | yes | Ok |
| 4424 | yes | Ok |
| 4432 | yes | Ok |
| 4432 (=p+64) | yes | Ok |
| 4416, 4408 (bisect) | yes | Ok |
| 4404 | no | same as 4096 |
| 4406 | no | same |
| 4407 | yes | Ok |
| 4406 x5 repeats | no, no, no, no, no | Ok each |
| 4407 x5 repeats | yes x5 | Ok each |

Follow-up probe (separate run, 4096, 4367, 4368, 4400, 4406, then 4407): after a failed oversized broadcast, A broadcast a 100-byte message. `send=Ok`, B never received it (3 s). So a failed oversize send leaves the link dead for later messages on that connection, silently. At 4407 no follow-up was needed (delivered).

Same bisection with the enum-tagged payload (4369 bytes): 4407 no, 4408 yes.

## Result: minimum setting, overhead

- Minimum `max_message_size` that carries bare worst-case Presence (4368 B): **4407**. Payload + 39.
- Minimum for tagged `Payload::Presence` (4369 B): **4408**. Payload + 39.
- Default 4096 does not carry it. Shortfall: 311.
- Framing overhead in the frame: **38 bytes**. The check is strict on the sender (`len >= max_message_size` rejects, `net/util.rs` write_frame), so the threshold is frame + 1 = payload + 39.
- 38 = 1 (topic::Message tag) + 1 (plumtree Message tag) + 32 (MessageId, blake3 of content) + 2 (content length varint, for 4096..16383) + 2 (DeliveryScope tag + Round varint). Derived from reading iroh-gossip 0.101.0 source; the sum matches the measured 38 exactly. Not independently measured field by field.
- A 4-byte length prefix and the per-stream topic header sit outside the checked length.
- Receiver check is `size > max`, non-strict; sender check is `len >= max`. The sender is the binding one.
- Size-dependent terms: content length varint grows to 3 bytes at 16384+ (overhead 39); Round is a u16 varint, 1 byte up to 127 hops, 2 bytes from 128. Neither matters for a 4368-byte payload within 127 hops.
- Recommended floor for implementation: set `max_message_size` to 4407 minimum for bare Presence, with headroom. 4096 (default) silently drops the worst-case Presence. Spec limit "public mapping, serialized 4096 bytes" is the cause: it forces Presence > 4096 whatever the other fields.

## What could make this wrong

- One process, one LAN host (direct address 192.168.1.4), two peers. Multi-hop forwarding, lazy-push (IHave/Graft) and larger swarms untested. Round growth beyond 127 hops would add a byte per extra varint width (code-inferred, not measured).
- Both nodes ran the same `max_message_size`. Mixed settings untested. By code reading, a sender with a larger limit than a receiver passes its check and the receiver rejects (`size > max`); not run.
- Cause of the silent drop is inferred, not observed. Debug logs for `iroh_gossip` showed the Broadcast command, then no further send event and no error line within 4 s. The code path (`write_frame` returns `TooLarge`, ending the send loop) matches the symptom. The follow-up probe confirms the link is dead afterwards. The error would only surface when the connection loop's join completes; I did not wait for that beyond 4 s plus 3 s.
- "Not received" means not received in 4 s. A slower delivery path is not excluded, though at 4407 delivery took well under 1 s and neighbors were up.
- The mid-run log column in my first full run mixed in teardown noise from my own shutdown (lines such as "closed by peer"). I discarded it and re-ran with pre-shutdown logs. No warn/error lines appeared in any failing run.
- The sizes table came from a single run each; they are deterministic (fixed 0xff content, varints at max). Worst-case varint widths assume u64::MAX for ts; real timestamps (ms since epoch) are 7 bytes, saving 3 bytes per ts (2 u64 fields in Presence: Claim.ts and Presence.ts, 6 bytes total). Real effective handles are shorter. 4368 is therefore an upper bound.
- `Payload` tag encoding is my assumption. If the real encoding adds more than 1 byte of wrapper, add the difference to the threshold.
- `[u8; 64]` via custom serde tuple encodes as 64 raw bytes. A different encoding (Vec<u8>, serde-big-array) could add a length byte. Verified roundtrip of Presence only.
- Bisection at 1-byte granularity: single trial per size in the sweep, 5 repeats at 4406 and 4407. Consistent in all; sample small.
- Host timings and the n0 `Minimal` preset (no address lookup, no relay) differ from production presets; not expected to affect frame size.
