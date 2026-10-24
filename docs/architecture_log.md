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
