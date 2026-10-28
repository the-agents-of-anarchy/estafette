# Architecture Log
## 2026-10-24 → 2026-10-24 — Runtime directory suffix is the config directory owner's uid
KIND:          runnable
FROM → TO:     No runtime directory existed → `<base>/estafette-<uid>` where uid is the owner of the resolved config directory, read through MetadataExt, since libc::getuid needs unsafe code, which the workspace forbids
WHY:           spec:516 puts the user id in the runtime directory name and leaves how it is read to the implementer (spec:450 call); the config directory is created by the same user before the runtime directory, so its owner is that user without unsafe code
INVALIDATES:   A config directory owned by another user yields that user's runtime directory; a deployment that pre-creates the config directory as root must chown it
               U30 (bridge) assumes the same `estafette-<uid>` suffix and is checked against this at integration
EVIDENCE:      core/src/dirs.rs at e935909; spec review 112404 with its correction; quality review 112512 Ready Yes

## 2026-10-24 → 2026-10-24 — Card public mapping serializes with its keys in sorted order
KIND:          computed
FROM → TO:     No converter existed → the card's `public` mapping is rendered to compact JSON with keys in sorted byte order, whatever order the YAML source lists them; the bytes then travel and are stored verbatim (spec:591)
WHY:           spec:591 fixes compact JSON and leaves key order unstated; one fixed order makes the serialized bytes a function of the mapping's content alone, so two renderings of one card agree (spec:450 call of U10, lead ruling at merge)
INVALIDATES:   A card whose public keys are written in a deliberate order sees that order lost on the wire; nothing in the spec reads order
EVIDENCE:      core/src/card.rs at fc6ebf3; spec review 112930 PASS; quality review 113032 Ready Yes

## 2026-10-24 → 2026-10-24 — Stale inbox sockets are detected by asking the OS process table for the pid
KIND:          runnable
FROM → TO:     No stale-socket rule existed → at inbox-socket start every `<runtime>/<pid>.sock` other than its own is kept if `ps -p <pid>` exits 0, removed if it exits 1, and any other outcome (spawn failure, signal, other exit code) refuses start with `InboxSocketError::QueryProcessTable` after a scan that removes nothing
WHY:           spec:549 permits removal only when no process with that pid exists and spec:450 leaves the check, pid reuse included, to the implementer; `ps -p` is the one portable query that needs no unsafe code; a query that did not run must not read as dead, else startup would delete a live node's socket
INVALIDATES:   A live unrelated process reusing a dead node's pid keeps that node's stale socket file until the pid is free; the file is then removed at the next start
               A host without `ps` on PATH cannot start the inbox socket; the error names the pid queried and that nothing was removed
               `ps -p` exit codes are verified on macOS only; Linux is settled by the process-table integration test on the lab (core/tests/inbox_socket_process_table.rs)
EVIDENCE:      core/src/inbox/socket.rs at af620dd; spec reviews 152944 FAIL then 153526 and 154500 PASS; quality review 154552 Ready Yes

## 2026-10-24 → 2026-10-24 — Gate and probe nonces come from a generator the composition root hands in
KIND:          runnable
FROM → TO:     No gate existed → `NodeEndpoint::bind` takes `fill_nonce: Fn(&mut [u8; 32])`, the one nonce source for the gossip gate's 32-byte challenge, and a `report_gate_closing` sink that receives each refusal's typed `GateFailure` before the connection closes with code 403 and reason `gate`
WHY:           spec:450 leaves the nonce source to the implementer and the tdd stance passes every side effect in from the composition root, so the node (U20) chooses the CSPRNG and the stderr record for refusals; the gate keys on `iroh_gossip::ALPN` alone (spec 3.6 'every gossip connection') and the endpoint accepts `estafette/direct/1` and the gossip ALPN from any node id
INVALIDATES:   A caller that hands in a weak generator weakens the gate; the node must pass a cryptographically secure one (spec:450)
               A node that registers gossip under another ALPN, or lets iroh's Router::spawn replace the ALPN list, bypasses the gate — U20 must keep DIRECT_ALPN and iroh_gossip::ALPN registered
               The sink runs inside the handshake and must not block
EVIDENCE:      core/src/net/{endpoint,gate}.rs at 59a34fc; spec reviews 124728, 130035, 131138 PASS; quality review 131320 Ready Yes

## 2026-10-28 → 2026-10-28 — A direct request whose body is not UTF-8 resets the stream instead of answering too_large
KIND:          runnable
FROM → TO:     No direct protocol existed → the wire decodes `body` as `String`, so a request carrying invalid UTF-8 fails decode before spec:673 step 4 and the server resets the stream with no response; the client reports every no-response close as `refused`; a valid body over the limit still answers `too_large`; the endpoint's Router registers both `estafette/direct/1` and `iroh_gossip::ALPN`; one connection per peer is held and reused, two concurrent first requests share one dial, and a failure empties the slot only while it still holds the failed connection (spec:854)
WHY:           spec:673 pairs the UTF-8 check with `too_large`, but the wire's typed decode (U04) rejects invalid UTF-8 before any step runs and the spec leaves a decode failure's close to the implementer (spec:450); a reset is the one close every reader already maps to `refused` (spec:601), so no raw-body decode path was added (lead ruling 2026-10-24, spec reviews 3 and 4)
INVALIDATES:   A sender of invalid UTF-8 sees `refused`, not `too_large`; a test expecting `too_large` for a non-UTF-8 body fails
               `ErrorCode::code` in core/src/errors.rs is public so the direct protocol reuses the reason strings; a change to a code's string reaches the wire
               A second concurrent caller to a dead peer can wait the connect timeout twice (about 10 s) before its own `unreachable` (spec review 041206, read as met)
               A write the peer stopped cannot be observed under the pinned transport's 1,250,000-byte receive window (max request 1,114,112 bytes); lowering that window below the largest request would need a `Stopped` arm
EVIDENCE:      core/src/net/direct.rs, core/src/errors.rs at 9b27694; spec reviews 033956 PASS and 041206 PASS; quality review 050614 Ready Yes

## 2026-10-28 → 2026-10-28 — The Claude Code bridge injects each inbox frame as a type-user socket line that the harness records as a peer message
KIND:          runnable
FROM → TO:     No bridge notice existed → the bridge opens the session's messaging socket per frame, writes the auth line, then one line `{"type":"user","message":{"role":"user","content":"Message from peer <name> (verified member of your network), sent <sent_at>.\n<body>\nReply with the send tool."}}`, and closes; a SessionStart hook on `startup|resume` starts the bridge with the harness pid, a second start for the same harness pid is refused, and `doctor` reports five checks plus the dropped-frame count, with `UNVERIFIED` for a refuse source it cannot read
WHY:           spec:357 (D36) asks for the harness's notice form where one exists that is not user input; the Claude Code docs specify no such socket line and the installed binary documents only the type-user line; a real `claude -p` session (2.1.312) recorded that line with origin kind peer, from unknown, synthetic, and the model treated it as a peer message, so the bridge sends it unwrapped (an own cross-session envelope could nest with the harness's wrapper); spec:928 names the doctor's five checks
INVALIDATES:   The receiving model sees the sender only in the header text: Claude Code shows from unknown for these notices; a socket-line field carrying a sender name is undetermined
               A `refuse` in MDM profiles or server-managed settings is not read; the doctor reports managed files and `--settings` values only, and UNVERIFIED where a source cannot be read
               A second SessionStart for one harness pid is refused by the process table; a resume in a new process starts a fresh bridge as intended
               Verified on macOS (Darwin 24.3.0, claude 2.1.312) only; the ancestor walk and `ps` on Linux are unverified
EVIDENCE:      bridges/claude/src/{main,doctor,inject}.rs at 2cea7dc; spec reviews 043658 FAIL, 044430, 050227, 051124, 052217 PASS; quality 052350 Ready Yes; live e2e at 2cea7dc one notice, doctor exit 0

## 2026-10-28 → 2026-10-28 — The endpoint sends a QUIC keep-alive every 500 ms so a vanished peer is detected within the idle timeout plus half a second
KIND:          runnable
FROM → TO:     iroh's default 5 s keep-alive (`HEARTBEAT_INTERVAL`) → `QUIC_KEEP_ALIVE_INTERVAL` = 500 ms set on the endpoint's transport config (core/src/net/endpoint.rs); the 10 s idle timeout (spec:597, spec:1003) is unchanged
WHY:           with the pinned transport (noq-proto 1.3.0) the idle timer restarts on the first ack-eliciting packet sent after a receive, so when the last packet received was the peer's own keep-alive the 5 s ping restarted the 10 s timer and a vanished peer was detected at 15 s in 4 of 50 solo runs, outside the test's [9 s, 11 s) window; a 500 ms interval bounds detection by idle + 0.5 s; the alternative, widening the test bound to 15 s, is forbidden (gate-discipline); decided by U38 after a three-hypothesis debugging round (reports debug-U18-H1/H2/H3, diagnose-U18)
INVALIDATES:   Every idle connection now carries about two packets a second instead of one per five seconds; iroh's documentation warns against tuning the interval; NAT and relay paths were not measured — lab test 28 and the compatibility pass settle it
               The idle test forces the race by letting the peer live 6 s past the handshake, which depends on iroh's 5 s default heartbeat on the plain peer endpoint; a change to that default changes what the test forces
               The gossip lagged test (core/src/net/gossip.rs) now waits for delivery before reading, so it proves lag, not a race with delivery; it no longer fails under load (0 of 140 runs)
EVIDENCE:      core/src/net/{endpoint,gossip}.rs at 6d4a132; spec reviews 091253, 095025 PASS; quality 100548 Ready Yes; idle test 0/50 vs 4/50 before, reverted fix fails 10/10

## 2026-10-28 → 2026-10-28 — The node loop takes its endpoint, clocks and record sink from the composition root and announces no handle before it owns one
KIND:          runnable
FROM → TO:     No node existed → `Node::start(endpoint, Identity, Network, NodeEffects)` runs the spec 3.8 rows over a `NodeEndpoint` the composition root binds (with the CSPRNG and the gate sink of U17) and a `NodeEffects` carrying the wall clock, a monotonic clock, the probe-nonce source, the run-record sink and the inbox socket; `Network::new(config, path)` derives the gate and card keys and the owner key once and refuses a bad key naming the field; before the own handle is assigned, Introduce and IntroduceAck carry `handle: None`; a Presence seq is `max(now, last + 1)` with no forward bound beyond the announcement max age; a newer record of a known host replaces it whole (relay and ts); the direct server holds the public card alone, never the card key; direct requests (drop, then introduce or probe) run in one spawned task per batch
WHY:           spec:450 leaves the composition and the join sequence to the implementer; passing every side effect in from the root (tdd) lets the loop's decisions be tested on frozen clocks; a joiner that announced an unassigned name during the observer wait would break spec 3.8's handle uniqueness; spec:751's spared connection never arises because DirectClient holds only connections it dialed, so only the IntroduceAck's drop is skipped; the ProbeAck 'from the probed card' (spec:678) rests on the TLS-verified dial by card id
INVALIDATES:   A caller that binds the endpoint with a weak nonce source weakens the gate; the composition root (U23) must pass a CSPRNG — none is chosen in core yet
               Learned hosts carry `heard` in milliseconds (spec 3.1) while core/src/config.rs compares it in seconds, so learned hosts never expire until the compatibility pass fixes config.rs
               A Message with a mismatched handle is dropped by the node but the U18 server has already acked it `accepted: true`; the compatibility pass decides which side moves
               `ANNOUNCEMENT_MAX_AGE_MS` lives in both peers.rs and presence.rs
               Rows spec:777-778 (relay address changes) are proved as pure helpers only; lab test 28 settles them end to end
               Node tests run at most two at once (`NODE_TESTS_AT_ONCE`); the core suite takes about 50 s on the owner's Mac
EVIDENCE:      core/src/node/{mod,presence}.rs, core/src/net/direct.rs at c3e490d; spec reviews 092241 FAIL, 093420 PASS, 100345 PASS by correction; quality 094116 With fixes, 100712 Ready Yes

## 2026-10-28 → 2026-10-28 — Send and broadcast run the spec send algorithm with every handle dialed at once and ambiguity reported by card id
KIND:          runnable
FROM → TO:     No send existed → `Node::send(to, body) -> Result<Delivery, ErrorCode>` runs spec:844-854 (match the handle; `too_large` for a body over the limit before any connection; dial through the held connection; drop the held connection and set the peer Unreachable on any direct failure; any valid ack, accepted or refused, counts as a direct contact that succeeds) and `Node::broadcast(to, body) -> Vec<BroadcastResult>` sends to every handle concurrently with no cap, one result per handle in input order; a refusal's reason maps to its ErrorCode (`too_large` kept, anything else `refused`); the message id comes from the probe-nonce source (16 secure random bytes); an ambiguous handle lists the matching cards as 64-character hex ids
WHY:           spec:841 and D27 ask for concurrent sends with one result per handle and leave the concurrency count to the implementer (spec:450); the handle match cannot be ambiguous today because `PeerTable::free_handle_for` gives every peer a distinct effective handle, so the arm is reached by property tests over the matcher alone; the node hides the client and the table, so the algorithm lives inside the node module (core/src/node/send.rs) with `NodeState` opened to it (lead scope ruling)
INVALIDATES:   An unbounded fan-out: a broadcast to many handles opens as many concurrent dials; a cap would be a new decision
               Ambiguity candidates are card ids, not handle text; a spec or owner ruling on the `ambiguous_handle` message would change the list
               `Delivery` and `BroadcastResult` carry bare strings for handle and message id (quality Minor, noted)
               The `timeout` code is reached only through the mapping; the 10 s ack timeout itself is U18's
EVIDENCE:      core/src/node/send.rs, core/src/node/{mod,presence}.rs at c3f9cae; spec reviews 104141, 105617 PASS; quality 104558 With fixes, 110046 Ready Yes

## 2026-10-28 → 2026-10-28 — The session runs the stopping sequence from the composition root's signals and announces nothing after its Departure
KIND:          runnable
FROM → TO:     No lifecycle existed → `Session` (Unclaimed, Claimed, Stopping; private fields) owns the node; `claim` answers `bad_request` once Claimed and `refused` while Stopping, `node()` answers `not_claimed` before a claim; `run_until_exit(end_of_input, signals, departure_wait) -> Exit` runs spec:784 in order: mark stopping, release the lock, broadcast one Departure whose seq is the announcer's next, save the known hosts when a held record is newer than the file, drop every held direct connection (`DirectClient::drop_all_held_connections`), wait `DEPARTURE_WAIT` = 5000 ms, exit; a stopping node sends no Presence, Introduce or Probe; an unclaimed stop exits at once; any signal while Stopping exits at once; only SIGTERM yields `Exit::Stopped(RuntimeShutdown::InBackground)`; `forward_standard_input` notes EOF at once and buffers unread chunks without bound (spec:246-250)
WHY:           spec:784 fixes the stopping order and spec:450 leaves the session states, the signal wiring and the exit mapping to the implementer; signals and end of input arrive as an `mpsc::UnboundedReceiver<ShutdownSignal>` and a `oneshot::Receiver<()>` the composition root (U23) fills, so the sequence is tested on channels without a process; a stopping node's own late Introduce revived it as Online at peers under load, so outbound requests stop at the Departure; `clippy::exit` is denied, so the root maps `Exit` to the process exit
INVALIDATES:   An Introduce written before Stopping can reach a peer after the Departure and set the node Online there until a probe fails (sub-millisecond window, spec-inherent: any accepted Introduce leaves Gone); tests wait 1.5 s after listing
               A dial holding a connection slot can delay the exit by up to the connect timeout; a second signal still exits at once
               A read failure on standard input is treated as end of input with no record until the composition root hands a sink in (U23)
               The socket file's removal rests on `InboxSocket`'s Drop; every `Arc<InboxSocket>` holder in the root must drop before exit (U23)
               Gossip staying alive while stopping is proved weakly: the witness may reach the observer by a direct gossip link rather than through the stopping node
               The real signal streams, stdin and process exit are untested here; U23 and a process-level test settle them
EVIDENCE:      core/src/node/{lifecycle,presence,mod}.rs, core/src/net/direct.rs at 46a1f93; spec reviews 120705 FAIL, 122619 and 124655 PASS; quality 123228 With fixes, 124655q Ready Yes

## 2026-10-28 → 2026-10-28 — A deaf member rebinds its endpoint in place and asks members to join when no neighbor appears
KIND:          runnable
FROM → TO:     No rebind existed → the presence loop records every verified payload on a rebind schedule (core/src/node/rebind.rs); after 15 s with no payload, or a join with no neighbor within 5 s, a member that knows another node, is not stopping, and last rebound at least 20 s ago rebinds: the router closes the old endpoint, each known host's relay is probed by TCP with `RELAY_PROBE_TIMEOUT` = 1000 ms and relays found down are stripped from the other members' lookup entries, `NodeEndpoint::rebind` binds a fresh endpoint with the same key, relay map, lookup and gate inputs, `DirectClient::follow` moves the client, the node rejoins through every known host and Online peer and announces the next seq; 1.5 s after a rebind's join with no member neighbor it asks gossip to join the Online and Unreachable members through a throwaway second subscription, every 3 s, at most 10 times; the run record gains `Rebinding` and `RouterShutdownFailed`; `NodeEffects` gains `probe_relay`; a send turning a peer Online wakes the loop
WHY:           spec:781-786 and D46 fix the deaf rule, the probe, what survives and the member join; spec:450 leaves the probe, the extra timing, the ask and the bind capability's home to the implementer; both gate inputs still enter from the composition root, so `NodeEndpoint` keeps them (classifier-U23, lead ruling); gossip.rs exposes no join call, so the ask is a dropped second subscription, proved on the vendored iroh-gossip 0.101.0 by a test that fails without cards
INVALIDATES:   The loop handles no other event while a rebind closes, probes and binds, about 4 s on the owner's Mac (closing the endpoint alone takes about 3 s, iroh's documented bound); a dead relay on a real network is settled by lab test 23
               The member-join ask relies on how the vendored iroh-gossip 0.101.0 handles a repeated subscribe; the test proves it there and nowhere else
               No bootstrap flag exists: `Node` is always a member; the bootstrap host is U29's own program path and never rebinds by construction
               The composition root (U24) must pass `relay_answers` as `probe_relay`; until then nothing in production probes a relay
               A failed `Router::shutdown` is recorded and the rebind goes on; the record is proved by type alone since no stand-in forces the failure
               Any node test running past 15 s with a known node sees a rebind and must expect the endpoint to change; the core lib suite now takes about 187 s on the owner's Mac, up from 111 s
               Test 24 and the timing rules run as properties over synthetic instants (500 seeded cases, 180 s of runtime each); the whole node is not clock-scaled because the gossip join timeout runs on tokio time
               macOS only; Linux and lab tests 23, 24 and 28 are U36's
EVIDENCE:      core/src/node/{rebind,presence,mod}.rs, core/src/net/{endpoint,direct}.rs at 9d5bcec; spec reviews 142434 PASS with O1-O3, 150359, 161712 PASS; quality 150418 With fixes, 161712q Ready Yes; classifier-U23 all observations

## 2026-10-28 → 2026-10-28 — The MCP server serializes init behind one gate, records every post-lock failure cause, and caps the awaited shutdown at one second
KIND:          runnable
FROM → TO:     No MCP server existed → `estafette mcp <network> [--cards-dir]` runs `mcp::serve(stdin, stdout, signals, ServedNetwork, ServerEffects) -> Exit` over rmcp 3.5.1 with a hand-written handler named `estafette`, six static tools with the spec:875-880 descriptions and spec:835-842 schemas, every answer in the spec:809 envelope with `you`, no schema checking by rmcp so bad input reaches `bad_request` (unknown tool, `to` over 74 characters, `since` below 0, unknown keys); a non-object `arguments` gets rmcp's own -32601; init runs spec:838 in order behind an `init_gate` Mutex, so two overlapping inits yield one handle, one `bad_request` and one endpoint bind; every failure after the lock answers `unreachable`, releases the lock and records a `ServerRecord` cause on stderr; `InboxSocket::frames_since(since)` serves the inbox tool read-only; the composition root in main.rs builds every side effect (signal streams into the `ShutdownSignal` channel, `relay_answers` as `probe_relay`, a CSPRNG, stderr sinks, a current-thread runtime) and caps an awaited shutdown at 1000 ms because a blocking stdin read otherwise holds the runtime open after SIGINT or SIGHUP
WHY:           spec:805-880 fixes the tools, schemas, envelope and init order and spec:450 leaves the handler, the runtime and the silences to the implementer; rmcp runs each request in its own task (rmcp-3.5.1 service.rs:1603), so the claim needed one critical section (quality review 185543); spec 3.9 names a background shutdown for SIGTERM alone, and U22's awaited shutdown hung 25 s with stdin open until the cap (implementer's call)
INVALIDATES:   Tests 2 and 6 of spec 3.15 are proved by two servers inside one process (the file lock holds per descriptor; the endpoints learn each other at bind); two OS processes are U34's and the lab's
               The 1000 ms cap is the implementer's number; a slower host could cut an awaited shutdown short — a process-level test on Linux settles it
               A card naming another network answers `card_invalid`; a card name outside the handle grammar answers `card_not_found`; `list_cards` skips files that are not valid cards; `public` is `{}` until valid JSON object bytes arrive; a stopped process exits 0 and a second signal 1
               `ListingTaskFailed` and `AnsweringFailed` are recorded only on a task panic and are untested; a bind failure after the lock is recorded by the same call but untested because `BindError` cannot be built
               `NodeEffects` is copied field by field in mcp.rs until `derive(Clone)` lands in presence.rs
               Verified on macOS only
EVIDENCE:      core/src/mcp.rs, core/src/inbox/socket.rs, core/src/main.rs at debaf02; spec reviews 184622, 200021 PASS; quality 185543 With fixes, 200021q Ready Yes

## 2026-10-28 → 2026-10-28 — A direct host may serve without a card, so one process runs an embedded relay and a card-less bootstrap node
KIND:          runnable
FROM → TO:     `DirectHost::own_card` returned a card and only a member or an observer served direct requests → `own_card -> Option<wire::PublicKey>`: the observer and the bootstrap host answer `None` and serve introduce, probe and drop without a card of their own; `run-infrastructure` starts the embedded iroh-relay (feature `relay`, rustls =0.23.46) and a bootstrap node in one process, shuts the relay down after the node, and never rebinds by construction; `KnownHosts` is `pub(crate)` so the bootstrap host reads the host file; the integration merge made the observer host drop its card field
WHY:           spec:949-958 and D46 ask for a bootstrap host that members join through and a relay the owner can run; spec:450 leaves the process shape and the card question to the implementer; a host that holds no card cannot be mistaken for a member by the presence loop, and an `Option` makes the card-less case a type, not a sentinel (lead ruling at the U29 scope widening)
INVALIDATES:   A bootstrap host never announces Presence and is never a peer; a joiner that expects the host in its peer table is wrong by design
               The relay's ACME path and the relay under a real public address were never executed on the owner's Mac; lab tests 22-28 settle the relay
               Every direct-request reader must handle `own_card == None`; a reader that unwraps it panics on the observer and the bootstrap host
               `KnownHosts` is reachable across the crate; a second writer of the host file would race the node's save
EVIDENCE:      core/src/cli/infra.rs, core/src/net/direct.rs, core/src/node/{presence,mod}.rs, core/src/cli/observer.rs at 1eb43e4; spec reviews 181455, 200127 PASS; quality 182048 With fixes, 200127q Ready Yes; integration CI 40772291162 green
