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
