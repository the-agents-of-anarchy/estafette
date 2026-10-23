# Unfixed spec parameters: classification and proposed values

Owner ruling (2026-10-20): settle "agreement ones only": every parameter two independently written pieces must agree on; name the rest as the implementer's call. Source list: research/stack/ground/capabilities.md "Ambiguities" (28 items), each checked against spec/blueprint.md and the rulings since (S1-S21). Drafted by claude-fable-5-1. Status: all four groups approved as drafted by the owner (2026-10-20); staged as S22.

## Already settled by a ruling (no new decision)
| Item | Settled by |
|---|---|
| Base32 alphabet; node id text form | S5: lowercase hex everywhere; node id = card public key in iroh's own hex form |
| Local discovery mechanism | S20: none now (mDNS deferred) |
| Relay implementation | stack: iroh-relay 1.3.0 `server`, behind a feature (S3) |
| Single-claim race | S14: exclusive lock on the card's sidecar |
| Duplicate-set eviction | S11: the duplicate set is removed |
| Transport key persistence | S14: the card key is the node key |

## G1 Wire agreement (nodes must match)
| # | Parameter | Proposed value | Reason |
|---|---|---|---|
| 1 | width of `v` | `u8` | one byte; versions will never exceed 255 |
| 2 | sum discriminants | postcard's variant index, in the order the spec lists; new variants only appended | postcard's own encoding; appending keeps old readers' indices valid |
| 3 | `public` bytes | carried as the sender's JSON bytes; receivers verify and store them verbatim and never re-serialize them for any check | signatures and MACs cover the bytes as carried; no canonical form needed |
| 4 | what the 3072-byte cap counts | the carried bytes; the converter emits compact JSON (no whitespace) | sender and receiver measure the same thing |
| 5 | direct-protocol ALPN | `estafette/direct/1` | both ends must offer the same string |
| 6 | oversize request | the sender refuses a body over 1,048,576 bytes locally with `too_large`; a receiver that reads past the 1,114,112-byte wire limit resets the stream with error code 1; the sender reports a reset as `refused` | measured in v3: `read_to_end(limit)` cannot answer, only reset |
| 7 | probe | ProbeAck echoes the nonce; a probe fails when no valid ProbeAck (MAC verifies, nonce matches, from the probed card) arrives within the probe timeout, for any reason | Unreachable rumors other nodes act on need one meaning of "failed" |
| 8 | Departure, Unreachable channel | the gossip topic | the spec implies it; state it |
| 9 | effective handle | handle + `-` + decimal n, n ≥ 2, no leading zeros; at most 53 characters | n is a u64; 53 bytes fits the Presence size budget |

## G2 Files and names (people and other tools read or type them)
| # | Parameter | Proposed value | Reason |
|---|---|---|---|
| 10 | program subdirectory, MCP server name | `estafette` | one name everywhere |
| 11 | directory modes | `estafette` config, data, and runtime directories created 0700; existing ones never loosened | the sidecars and the secret live there |
| 12 | sidecar text | written as 64 lowercase hex characters and one newline; read accepts either case and a missing newline | strict writer, tolerant reader |
| 13 | card and network names | the handle grammar `^[a-z][a-z0-9-]{0,31}$` | both appear in paths and on command lines |
| 14 | card `public` in YAML | a mapping with string keys whose values are strings, numbers, booleans, null, sequences, or mappings of the same; anything else (tags, dates, binary, merge keys, non-string or duplicate keys) fails init with `card_invalid` | card authors and our converter agree on exactly what converts |
| 15 | cards directory override | flag `--cards-dir`, environment `ESTAFETTE_CARDS_DIR` | users and harness configs type them |

## G3 Local contracts (harness and bridges are written independently)
| # | Parameter | Proposed value | Reason |
|---|---|---|---|
| 16 | what stops a node | end of stdin, SIGTERM, SIGINT, SIGHUP start Stopping; a second signal exits at once | the harness may close stdin or signal |
| 17 | `since` older than the buffer | the server sends from the oldest buffered frame; the jump in `seq` is the only sign | S18: gaps are the bridge's business |
| 18 | several socket clients | each client gets its own replay from its own `since`, then live frames; `attached` is true while any client is connected | bridges are independent of each other |
| 19 | Gone entries and handles | a Gone entry keeps its handle until it is removed (3600 s) | the spec's "an entry keeps its effective handle as long as it exists" |
| 20 | observer nodes | a fresh key each run, never a card key | an observer is not a member |

## G4 Formats between machines
| # | Parameter | Proposed value | Reason |
|---|---|---|---|
| 21 | invitation string | `estafette-invite-1:` + lowercase hex of the postcard encoding of {v, network name, secret, one or more host addresses (id + relay URL)} | travels between machines; hex per S5 |
| 22 | host address line (printed by run infrastructure, accepted by join) | `estafette-host-1:` + lowercase hex of the postcard encoding of the host's endpoint address | the same shape as the invitation |

## G5 MCP surface (from e11; approved as drafted by the owner 2026-10-20, staged as S23)
| # | Parameter | Proposed value | Reason |
|---|---|---|---|
| 23 | tool result form | `content[0].text` is the envelope as JSON text; `structuredContent` is the envelope; `isError` is the negation of `ok` | e11 row 2: both carry the same envelope; how a harness renders them is untested |
| 24 | which error first, before init | the input is checked first: invalid input returns `bad_request`, valid input `not_claimed` | the schema check needs no state (e11, D28 row) |
| 25 | `since` | integer with minimum 0 in the advertised schema; a negative value is `bad_request` | e11 Surprises 9 |
| 26 | unknown tool name | `bad_request` envelope with `you` | keeps the closed list and D30 (e11, closed-list row) |
| 27 | key order in JSON | carries no meaning; no `preserve_order` | e11 Surprises 7; saves a crate |
| 28 | `arguments` not an object | the MCP library's own JSON-RPC error (-32601), with no envelope; D30 covers tool results only | e11 row 3b: overriding it needs untested code |
| 29 | rmcp build | default features off; `server` and `transport-io`; no macros; a hand-written handler and server info | e11 row 3, Surprises 1, 2, 12: the macro path breaks the envelope, the server name, and the tool order |

## The implementer's call (named in the spec as such)
- the source of probe nonces (any cryptographically secure generator)
- how the stale-socket check finds out whether a process exists, including PID reuse
- where the user id for the runtime directory comes from
- per-line output formats of list cards, show peers, tail, and doctor
- how many broadcast sends run at once
- how timers (Gone retention, waits) are implemented
