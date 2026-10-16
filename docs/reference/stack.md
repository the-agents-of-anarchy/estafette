# estafette stack

> Basis: 26 reports in agents-of-anarchy-marketplace research/stack/ (ground 2, surveys 21, verify 3) plus plan.md with its orchestrator corrections, 2026-10-16. Spec: spec/blueprint.md.
> Owner rule: "build the least amount possible with no real compromises and with strictest standards upheld for things we DO write."
> Marks: (unverified) = worker claim no verifier checked. Verified claims come only from v1a, v1b, v2. (arith) = this document's own arithmetic.

## Answer

iroh 1.3.0 solves transport, keys, the remote-id check of 3.6 step 4, the router, and relay-only paths; iroh-gossip 0.101.0 solves topic membership and broadcast; iroh-relay `server` and iroh-mdns-address-lookup solve the embedded relay and LAN discovery (f1a, f1b, f2, f3).
Every other dependency estafette needs, except YAML, TOML, and the `signal` feature, is already in the tree: postcard, serde_json, blake3, data-encoding, uuid, libc, rand, tokio (tree.md, s1).
We write the direct protocol on raw iroh streams, not irpc, because irpc fixes a 16 MiB frame cap and allocates before any MAC check, against the 1,114,112-byte cap of 3.14 (d1).
We also write presence and the peer table, the session state machine, the inbox socket, the MCP tool bodies, XDG paths, the card converter, and the invitation format; no surveyed project implements D18-D20, so there is nothing to copy (p1, p3a, p3b).
The tree grows from 276 crates to at most about 363 (arith); the embedded relay is up to 71 of those (f3).
Two spec parameters block code: the gossip `max_message_size` must be at least 4420 bytes, or the spec's own largest Presence cannot be sent (v2), and the node id text form is unfixed (f1a).
Three pinned-version risks need tracking: churn fixes for iroh-gossip are merged but unreleased (v1b), iroh's mapped-address cache never evicts (v1a), and swarm-discovery must resolve to 0.6.4 or later (v1b).

## Ranges: solved, copied, written

| Range (capabilities.md) | Solved by (crate, version, feature) | In tree already? | Copy | Write ourselves | Evidence |
|---|---|---|---|---|---|
| transport | iroh 1.3.0, defaults; `presets::Minimal` + `relay_mode(RelayMode::custom([relay]))`; `Router` with one `accept` per ALPN (estafette ALPN, gossip ALPN) | yes | accept-read-respond sequence from `iroh/examples/echo.rs`; carry its notice | ALPN handler; `remote_id()` check for step 4; explicit stream reset/close codes for steps 1-2; every timeout | f1a, d2 (unverified) |
| transport: LAN discovery | iroh-mdns-address-lookup 0.6.0, no features | no | crate doc example for adding the lookup | nothing | f1b (unverified) |
| transport: relay-only test 9 | iroh `clear_ip_transports()` | yes | nothing | nothing | f1a, s2 (unverified) |
| gossip | iroh-gossip 0.101.0, defaults (`net`, `metrics`); `Builder::max_message_size` set per spec gap 1 | no | drop-and-continue on a bad payload (iroh-smol-kv `lib.rs:803-808`); never the `?` abort of chat.rs | subscribe with bootstrap + mDNS ids; `join_peers` after `split()`; payload MAC and claim checks; handling of `Lagged` | f2, p1 (unverified); v2 |
| crypto | iroh `SecretKey`/`PublicKey`/`Signature` (Ed25519, `verify_strict`); blake3 1.8.7 `derive_key` and keyed hash | yes | nothing | claim sign and verify over postcard with zeroed `sig`; MAC with zeroed `mac`; constant-time compare (source: open question 9) | f1a, tree.md (unverified) |
| crypto: text forms | data-encoding 2.11.1 with a lowercase `new_encoding!` const via data-encoding-macro 0.1.21 | yes, both | the `new_encoding!` const form from the macro doc | 52-char card key, 7-char fingerprint, secret text; sidecar hex parse (trim the newline, then `from_bytes`) | s1, f1a (unverified) |
| wire-encoding | postcard 1.1.3 + serde | yes | nothing | all eleven structures, three sum types, both size limits | tree.md (unverified) |
| local-encoding | serde_json 1.0.154 | yes | nothing | envelopes, inbox frames, `public` as verbatim JSON bytes | tree.md (unverified) |
| files: card YAML | yaml-rust2 0.13.0 (no serde layer) | no | nothing | YAML-to-JSON converter for `public`; card writer for `handle` and `public_key` | s1 (unverified) |
| files: network TOML | toml 1.1.8, defaults | no; the relay `server` feature pulls a toml too | nothing | 3-key struct | s1, f3 (unverified) |
| files: XDG dirs, modes | std only | yes | nothing | about 10 lines; only hand code matches 3.3 on every row | s1 (unverified) |
| local-io: inbox socket | tokio `net` `UnixListener`; std `PermissionsExt` | yes | nothing | about 4-6 lines for bind and 0600; replay buffer; attach tracking | s1 (unverified) |
| local-io: stale socket | libc 0.2.190 `kill(pid, 0)` | yes | nothing | about 6 lines, one unsafe call | s1 (unverified) |
| local-io: stop triggers | tokio feature `signal`; stdin close on a std thread | tokio yes, `signal` no | nothing | about 4 lines for signals, about 8 for stdin; tokio stdin has a documented shutdown hang | s1 (unverified) |
| mcp | rmcp 3.5.1, `server` + `transport-io` (owner ruling) | no | nothing | six tools, `you` in every response, closed error list, `bad_request` mapping | tree.md (unverified) |
| cli | clap 4.6.7, `derive` (owner ruling) | no | nothing | eight commands; exit codes 0-3; invitation and ticket text | tree.md, capabilities.md (unverified) |
| time-and-ids | uuid 1.28.0, add feature `v7` (`Uuid::now_v7()`); tokio `time` for timeouts | yes; `v7` adds no crate | nothing | ms timestamps from std; five timeouts of 3.14 | s1 (unverified) |
| state | none | n/a | nothing | peer table, tie-break, probe rules, effective handles, duplicate set, inbox buffer, D18-D20 | p1, p3a, p3b (unverified) |
| infra | iroh-relay 1.3.0, feature `server`; `Server::spawn(ServerConfig)` | no | the `main` flow of iroh-relay's binary and `iroh/src/test_utils.rs:45-85` for embedding; carry notice | key file on first run; ticket print; Probe/Introduce answers with `claim: None` | f3, r1 (unverified) |

## Draft manifest

```toml
[workspace.dependencies]
# transport, Ed25519 key types, Router, relay modes; defaults (metrics, fast-apple-datapath, portmapper, tls-ring)
iroh = "=1.3.0"
# presence topic: HyParView membership, Plumtree broadcast; pre-1.0, pinned exactly
iroh-gossip = "=0.101.0"
# embedded relay for `run infrastructure` (3.14, D43); same version line as iroh
iroh-relay = { version = "=1.3.0", features = ["server"] }
# LAN discovery (3.6, D15); Cargo.lock must resolve swarm-discovery >= 0.6.4
iroh-mdns-address-lookup = "=0.6.0"
# MCP over stdio (owner ruling); default features plus transport-io
rmcp = { version = "=3.5.1", features = ["server", "transport-io"] }
# operator commands (owner ruling)
clap = { version = "4.6.7", features = ["derive"] }
# wire and local structures (owner ruling); whether serde_derive is already in tree was not measured
serde = { version = "1.0.229", features = ["derive"] }
# wire encoding of 3.1; in tree
postcard = { version = "1.1.3", features = ["alloc"] }
# local encoding of 3.1; in tree via rmcp
serde_json = "1.0.154"
# derive_key for topic and mac key, keyed hash for the mac (3.4); in tree
blake3 = "1.8.7"
# lowercase unpadded base32 and hex; in tree
data-encoding = "2.11.1"
# const lowercase base32 encoding; in tree via iroh-base, adds no lock entry
data-encoding-macro = "0.1.21"
# message ids (3.1); v7 needs only rng, already on
uuid = { version = "1.28.0", features = ["v7"] }
# runtime; every feature but `signal` already on in tree; `signal` adds signal-hook-registry and errno
tokio = { version = "1.53.3", features = ["rt", "macros", "net", "time", "sync", "io-util", "signal"] }
# network configuration file (3.3)
toml = "1.1.8"
# card file (3.5); parser only, 6 transitive
yaml-rust2 = "0.13.0"
# kill(pid, 0) for the stale socket check (3.3); in tree
libc = "0.2.190"
# network secret and probe nonce; in tree
rand = "0.10.3"
# the closed error code list of 3.10 as one enum; in tree
thiserror = "2.0.21"
```

Runtime flavor: `rt` without `rt-multi-thread` keeps the tree's feature set (s1). The cost of `rt-multi-thread` was not measured.

Tree cost:

| Step | Unique crates | Source |
|---|---|---|
| iroh | 239 | tree.md (unverified) |
| + iroh-gossip | 246 | tree.md (unverified) |
| + rmcp (server, transport-io) | 265 | tree.md (unverified) |
| + clap (derive) | 276 | tree.md (unverified) |
| + iroh-relay `server` | at most +71; clap and serde_json are among the 71 and already counted | f3 (unverified) |
| + iroh-mdns-address-lookup | +2 (swarm-discovery, acto) | f1b (unverified) |
| + toml | +0 to +6, depending on dedup with the relay's toml | s1, f3 (unverified) |
| + yaml-rust2 | +6 | s1 (unverified) |
| + tokio `signal` | +2 | s1 (unverified) |
| uuid v7, data-encoding-macro, blake3, postcard, libc, rand, thiserror | +0 | s1, tree.md (unverified) |
| After | about 357 to 363 (arith); a `cargo tree` run must replace this | arith |

## Prior art: what it teaches

| Project | Validates | Trap it shows | Take |
|---|---|---|---|
| iroh examples echo, auth-hook | One request per stream; finish delimits the request; `remote_id()` comes from TLS, not client bytes | Uncapped `copy`; unbounded `closed().await`; `Err` from `accept` sends no code | Accept-read-respond sequence (d2, f1a) |
| irpc 0.17.0 | One request per bi-stream, postcard body | Fixed 16 MiB cap, allocation before decode, stock handler hides the remote id and serializes requests | Nothing (d1) |
| imsg | Finish, then stop, on close | No max frame size; reserves before checking; pinned to iroh 0.91 | Close ordering only (d2) |
| chat.rs, workshop gossip4 | Author must live in the signed content; `delivered_from` is a hop | `?` on a bad payload ends the subscribe loop; content-hash dedup collapses identical payloads | Signed envelope inside content (p1) |
| iroh-smol-kv | Late-join catch-up by republish on `NeighborUp` | Periodic traffic; no departure; a documented knob is never read | Drop and continue on malformed payloads (p1) |
| Tunnet (iroh 1.3.0, gossip 0.101.0) | Signed presence checked against the id inside the record; online verdict uses an open connection | 30 s beacons; no departure; replayable beacons | Nothing (p3a) |
| Peersey | Announce on each new neighbor | `NeighborDown` taken as departure, the false inference D20 rejects; unsigned presence | Nothing (p3a) |
| p2panda-net (iroh 1.3.0) | Stale marking guarded by own reachability | Public network id; discovery default leaks all known nodes; sqlx and ractor weight | Nothing (p3b) |
| walkie, alink (iroh 1.3.0) | Unix socket between CLI and node; constant-time secret compare; 0600 file by atomic rename | Outboxes held for absent peers; walkie's README claims survival of a killed daemon that the code lacks | 0600 atomic-rename pattern (a1) |
| agent-swarm, habilis-network | Pull inbox with a per-session cursor; MCP push unused; engine apart from app | `Done` reported for a buffered frame; anti-entropy backfill acts as a mailbox; git-rev fork pins; ephemeral author key flagged by habilis itself | Nothing (a1b, a3) |
| mcpmesh | Accept-time gate before handler; no message store; dial failure reported as unreachable | Exact pins `iroh =1.2.0`, `rmcp =3.1.0` collide with ours; 60 s beats; design text disagrees with its own constants | Nothing (a2) |
| chio, iroh-rings | Admission keyed by TLS-authenticated `EndpointId` | Neither detects a second live holder of one key | Nothing (i2) |
| iroh-auth, rcan | Bind both node ids into the handshake; verify at decode | Per-connection auth conflicts with 3.6; blocklist keyed by an id a restart escapes | Nothing (i1) |
| rendezvous, distributed-topic-tracker | First contact without an always-on node, through DHT slots | Unstretched secrets in public records; still on n0 relays | Nothing (p2) |
| rayfish, mshr | Relay replace-or-augment as an explicit rule; in-process relay builder with three TLS modes | mshr enables iroh-relay `test-utils` unconditionally, so insecure TLS skip ships in every build | Never enable `test-utils` outside dev-dependencies (r1) |

## Against the spec (for the owner)

| # | Spec decision | Finding | Evidence | What deciding it would change |
|---|---|---|---|---|
| 1 | D14 reason "a relay is needed anyway" | rayfish and mshr both default to n0's relays, and neither ships a relay binary. Both DHT bootstrap projects also run on n0's relays (p2). | r1 (unverified) | Dropping the embedded relay saves up to 71 crates (f3) and the TLS question of gap 8. It adds a third-party dependency, which D14 avoids. |
| 2 | D14 rejection of "membership lookup in a public DHT"; reason "nothing else has a stable address" | Four projects find first contact through DHT or pkarr records at a secret- or name-derived slot, with no stable node key. All four still rely on n0 relays and services. | p2, r1 (unverified) | Replacing the bootstrap member with slots adds `mainline` and a public-DHT reach requirement. It removes the stable infrastructure key. |
| 3 | D13 "membership is possession of the secret"; D17 | Gossip has no admission. Knowing the topic id and one bootstrap id is enough to join, to read every Presence in plaintext, and to inject payloads that members forward before any MAC check. The MAC gates acceptance, not reading or relaying. | v2 | Closing this needs payload encryption, or an `EndpointHooks` gate on the gossip ALPN only (hook API: i1). Both are new protocol text. |
| 4 | D18 "introduction replaces heartbeats" | iroh-gossip has no keepalive. A silent peer is reported only when its connection closes, so crash detection waits on the QUIC idle timeout, which no report read. Tunnet, mcpmesh, and habilis all chose periodic beats. No project implements introduce-only presence. | f2, p1, p3a, a2, a3 (unverified) | Keeping D18 accepts a detection latency nobody measured. Changing it adds traffic that grows with member count, the cost D18 rejects. |
| 5 | D3 "each session makes a new transport key" | chio, Tunnet, and mcpmesh keep a persistent transport key as the identity. p2panda uses one key for peer and transport. habilis keeps ephemeral keys and lists durable identity as missing. iroh 1.3.0's mapped-address cache grows once per remote ever seen and is never evicted (v1a). D3 makes every session a new remote. | i2, p3a, a2, p3b, a3 (unverified); v1a | A stable key per card would remove the claim's node binding and the probe-based tie-break of D4 and D6. It would also bound the cache growth. |

## Spec gaps (parameters the spec must fix)

1. **Gossip `max_message_size`.** The default of 4096 cannot carry a Presence with a 4096-byte `public`. The minimum is 4420 with a 32-byte handle (v2). The Presence `handle` is the effective handle, which may exceed 32 bytes (plan.md correction). An unbounded `-n` suffix makes the size unbounded (capabilities.md). With `n` capped at u64, the suffix adds at most 21 bytes, so the minimum becomes 4441 (arith). Every node must use one value (f2).
2. **Fresh `ts` per payload.** Gossip suppresses identical bytes within 90 s (f2). Every Presence, Departure, and Unreachable must carry a fresh `ts`, or repeats vanish.
3. **Network-change trigger.** iroh 1.3.0 emits no network-change event. The nearest signals are `home_relay_status()` for relay reconnects and `watch_addr()` for address changes (f1b, unverified). 3.8 must name which triggers Presence.
4. **`Lagged`.** The gossip event channel holds 256 events. On overflow, events are lost, `NeighborDown` included (f2, unverified). The spec must say what a node does on `Lagged`.
5. **Departure delivery.** `broadcast` returns once queued and has no flush (f2, unverified). The 1000 ms wait is the only delivery bound. The spec should say so.
6. **Close without response.** Steps 1 and 2 of 3.6 need a named QUIC stream reset or stop code (d2, unverified).
7. **Server-side waits.** No limit in 3.14 bounds a handler waiting on a peer (d2, unverified). The spec also does not say whether requests on one connection run concurrently (d1, unverified).
8. **Relay TLS.** Address discovery needs TLS. A self-signed relay is trusted by clients only through a test-only constructor (f3, r1, unverified). The spec must name the certificate mode, and whether `http://` relays are allowed.
9. **Relay fallback.** `RelayMode::Custom` replaces n0's relays. The spec does not say whether n0 is a fallback (r1, unverified).
10. **Node id text form.** iroh displays 64 lowercase hex. It parses hex or RFC base32, and has a separate z-base-32 form (f1a, unverified). The plan settles on "iroh's own" (plan.md). The spec must write which: hex.
11. **Signature verify.** iroh verifies with `verify_strict` (f1a, unverified). 3.4 says "verifies".
12. **Invitation and ticket formats.** An `EndpointTicket` cannot carry a network name or secret (f1b, unverified). The output formats of the operator commands are open (capabilities.md).
13. **Remaining parameters, open in capabilities.md.** Base32 alphabet, sidecar hex case and newline, `v` and discriminant widths, canonical JSON of `public`, the measure of the 4096-byte limit, YAML-to-JSON conversion, probe nonce source and the meaning of "failure", stopping signals, PID reuse, build constants, override names, directory modes, network and card name grammars, Gone entries and suffixes, duplicate-set eviction order, replay older than the buffer, concurrent inbox clients, the broadcast concurrency bound, observer transport keys. The plan settles three others: the single-claim race, the node id text form, and verbatim `public` bytes.

## Risks in the pinned versions

| Pin | Risk | Evidence |
|---|---|---|
| iroh-gossip 0.101.0 | The connection-churn leak fixes #161, #178, and #189 are merged on main and unreleased. Per #178, a 0.101.0 node may lose joins from new nodes, so a network that mixes versions after the next release can split. Per #189, connections to older peers still leak on main. | v1b |
| iroh-gossip 0.101.0 | Pre-1.0. Four or five of the last six releases were breaking. iroh bumps change the public API through `Gossip::spawn(Endpoint)`. `ApiError::Rpc` exposes `irpc::Error`. | f2 (unverified) |
| iroh-gossip 0.101.0 | `broadcast` checks no size. An oversized frame fails in the send loop and closes the connection to that peer. | f2 (unverified); v2 |
| iroh 1.3.0 | The mapped-address cache grows once per remote ever seen. The fix PR #4294 was closed unmerged; issue #4293 and PR #4447 are open. | v1a |
| iroh 1.3.0 | The relay teardown drain and the netwatch RTM_MISS fix are contained. | v1a |
| swarm-discovery 0.6.1 | It keeps the 10 ms GC retry loop. 0.6.4 fixes it and satisfies `^0.6`. Cargo.lock must resolve 0.6.4 or later. | v1b |
| iroh-relay `server` | The insecure TLS skip is compiled in only with `test-utils`. Keep it in dev-dependencies. | f3, r1 (unverified) |
| tokio stdin | A pending read cannot be cancelled and can hang runtime shutdown. | s1 (unverified) |

## Not adopted

| Candidate | Why not |
|---|---|
| irpc 0.17.0 (`iroh`, `derive`) | Saves about 40 lines of about 415 with the 3.14 cap kept. Its 16 MiB cap is fixed, and it allocates before any MAC check (d1, unverified). |
| imsg 0.1.0 | Pinned to iroh 0.91. No frame cap. Adds nothing for one exchange per stream (d2, unverified). |
| iroh-tickets 1.0.0 | The invitation needs its own format anyway. The crate's tree cost was not measured, and its lock pins iroh-base 1.0.0 (f1b, unverified). Write one format, in the same shape: prefix plus lowercase base32 of postcard. |
| iroh-auth 0.2.0 | Authorizes per connection, against 3.6. Produces no message key. The spec's 32-byte random secret makes PAKE moot (i1, unverified). |
| rcan 0.5.0 | Signed grants are the machinery D13 rejects. It has no shared-secret membership (i1, unverified). |
| p2panda-net 0.7.1 | One key for peer and transport. Public network id. Adds sqlx, ractor, and four p2panda crates (p3b, unverified). |
| mcpmesh-node 0.58.0 | Exact pins on iroh 1.2.0 and rmcp 3.1.0. Stable endpoint identity. Per-pair gate (a2, unverified). |
| habilis-network, agent-gossip | Unpublished. Forked git revs (a3, s3, unverified). |
| iroh-smol-kv, iroh-gossip-rendezvous, distributed-topic-tracker | Wrong presence model, a public DHT, or other iroh pins (p1, p2, unverified). |
| serde_yaml, serde_yml | Archived or deprecated (s1, unverified). |
| serde_norway, serde_yaml_ng | 14 transitive crates against 6 for yaml-rust2. The YAML-to-JSON conversion gap must be decided in our code either way (s1, unverified). |
| saphyr | 15 transitive crates against 6 (s1, unverified). |
| dirs, directories, etcetera, xdg | dirs moves config on macOS. etcetera has no `$TMPDIR` fallback. directories is archived. None matches 3.3 on every row (s1, unverified). |
| rustix, nix | New crates for one `kill` call (s1, unverified). |
| ed25519-dalek as a direct dependency | iroh's key types cover sign, verify, and the seed (f1a, unverified). |
| iroh `test-utils`, `unstable-custom-transports` in builds | Test-only. Leaks insecure TLS (s2, r1, unverified). |
| libp2p | A second transport stack against D11 (s3, unverified). |

## Open questions and next experiments

1. Run a serialized-size test of the largest Presence inside a real gossip frame, to confirm 4418 to 4441 (v2 asks for it).
2. Run `cargo tree` on the draft manifest, to replace the 357-363 estimate.
3. Find the address a node dials for `claim.node` when only the node id is known. Test the network relay URL plus the node id, and mDNS on the LAN. No report covers it.
4. Check whether rmcp's stdio transport ending marks stdin close, and whether shutdown then hangs on tokio's stdin (s1).
5. Measure crash detection latency, set by the QUIC idle timeout, which no report read (f2, p3a).
6. Measure iroh with `default-features = false` plus `tls-ring`, to see what metrics and portmapper cost.
7. Decide, as owner, whether member builds pay up to 71 relay crates, or whether the relay sits behind a cargo feature for infrastructure builds.
8. Plan the iroh-gossip upgrade to the first release that carries #161, #178, and #189, including mixed-version behaviour (v1b).
9. Find the source of the constant-time MAC compare. blake3's hash type or `subtle` are candidates; no report checked either.
10. Round-trip every grammar-valid handle through the card writer and the yaml-rust2 parser, including words YAML may type as non-strings.
11. Test harness: owner deferred. `iroh::test_utils::run_relay_server` and the gossip simulator exist behind `test-utils`, as dev-dependencies only (s2).

## Confidence

| Claim class | Status |
|---|---|
| Upstream fix containment in iroh 1.3.0, netwatch and portmapper 0.19.3, iroh-gossip 0.101.0, swarm-discovery 0.6.1 | Verified by source and ancestry, not run (v1a, v1b) |
| Gossip admission and frame-size arithmetic | Verified by source reading; byte counts derived by hand, not serialized (v2) |
| API surfaces of iroh, iroh-gossip, iroh-relay, irpc, noq | Unverified; read from source at named commits, nothing compiled against them (f1a, f1b, f2, f3, d1, d2) |
| Tree counts and feature sets | Unverified; tree.md resolved and built a probe; f3 resolved only |
| Small-piece costs and line estimates | Unverified; registry and `cargo tree` facts; line counts are estimates (s1, d1) |
| Prior-art behaviour | Unverified; source reading of shallow clones; nothing run (p1-p3b, a1-a3, i1, i2, r1, s3) |
| Buzz | README-grade; a vendor post, no code read (a2b) |
| Tree total after the manifest, Presence bound with suffix | Own arithmetic; needs open questions 1 and 2 |
