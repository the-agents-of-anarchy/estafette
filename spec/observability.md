# Observability: what estafette records about itself

Scope: what the binary in `core/` and the Claude Code bridge in `bridges/claude/` record about their own decisions and their transport, for a test, a lab run and a user after release. `spec/blueprint.md` names no log, trace or diagnostic beyond the `doctor` and `tail` commands, so this record adds a product surface beside Part 3; it changes no wire byte, error code, timing or standard-output byte that Part 3 states. Unbuilt at main acd7e3d.

Ground of the whole record: owner rulings of 2026-11-11 — a "structural systemic decision that would allow the source code to be very easily debugged - now and after initial release"; "both yes." to the event record as a product surface and to the `tracing-subscriber` dependency; "i would rather have a --debug/--no-debug flag. i despise having to remember bullshit env vars to set."; "before we release the v1.0.0, which is long from now, we use maximum observability by default."; "using any sleep in tests is forbidden."; "i don't want to come back to this concern ever again." Evidence it answers: one defect family failed every lab run from 2026-11-09 to 2026-11-11 through three fixes that passed every local gate, each reasoned without a record of what the nodes did (journal, 2026-11-09T16:17, 2026-11-10T13:26, 2026-11-11T14:38).

## The switch

1. One flag pair, `--debug` and `--no-debug`, on every command that starts an iroh endpoint and on the bridge. No environment variable and no configuration field switches or steers any of this. Until v1.0.0 the default is `--debug`; what the default is from v1.0.0 on is the owner's decision at that release.
2. `--debug` is the fullest level there is: both files of decision 4, every event of decisions 9 and 10. There are no levels, filters or sampling.
3. `--no-debug` installs nothing and writes nothing; standard error is byte for byte what it is at acd7e3d. With `--debug`, standard error gains exactly one line, first, naming the two files; standard output is never touched (it is the MCP protocol). The help of every command that has the flag states what it writes, where, which file is safe to post, and the bounds of decision 13.

## The files

4. Two files per process start, under `<data>/traces/` (the data base directory of spec 3.3, through the one module that derives paths; directory 0700, files 0600): `<start>-<run>-<program>.events.jsonl` and `<start>-<run>-<program>.raw.jsonl`, where `<start>` is the UTC start time in basic ISO 8601 to the second, `<run>` is the run id of decision 5 and `<program>` is `estafette` or `bridge`; the raw file exists only where the process starts an endpoint, so the bridge writes the events file alone. Never a pid in a name, never a file in the runtime directory (spec 3.16: "Do not add a lock file, a pid file, or a registry index", "Do not write a file that announces the inbox socket").
5. Every line of both files is one JSON object that begins with the same fields: `v`, the format version, 1; `run`, sixteen hex characters drawn at process start from the secure random source; `n`, a counter from 0 that never skips except where decision 12 says so; `mono_ms` and `wall_ms`, from the two clocks the composition root hands in; `node`, the first five bytes of the endpoint id in hex — the form iroh's own event lines use (journal, 2026-11-11T15:28) — absent where the process has no endpoint. A restarted process keeps `node` and changes `run`: that pair is what tells a restart from a reconnect.
6. The events file holds typed events only and is safe to post: it holds no message body, no key, secret, token, invitation or MAC, no sidecar path, no inbox socket path, no run of 64 hex characters, and no network location. A peer is named by handle and five-byte id; a connection by an id and its protocol; a network location by its kind (direct or relay), its port, and a hash salted per file with a salt that is never written. It does hold handles, card names, tool names and error codes. These hold because the event type has no field that could carry the forbidden things, and a test plants marker values in a body, the secrets, the socket path and an address and finds none.
7. The raw file holds iroh's own `tracing` lines as iroh words them, for the targets `iroh::_events` (debug), `iroh::socket::remote_map` (trace), `iroh_gossip::proto::hyparview` (debug) and `iroh_gossip::net` (debug) (journal, 2026-11-11T15:28). It holds real addresses and full ids: its first line and the help say it is not safe to post. `tracing-subscriber`, pinned, reached through one module, exists for this file alone; it is installed once per process, at the composition root.

## The events

8. One closed event type per program, emitted through the record sink the composition root already hands the node (architecture log, 2026-10-28, "The node loop takes its endpoint, clocks and record sink from the composition root"). Every line the programs write to standard error today as their run record is one of these events, produced from one value at one place; it still renders on standard error exactly as today. No second channel, and no global but the subscriber of decision 7 and the panic hook of decision 9, both set at the composition root.
9. The node records every input it receives and every decision it takes on the rows of spec 3.6 to 3.10, each with the facts that decided it:
   - process: start (version, command), claim and its outcome, each step of the stopping sequence, exit, and a panic's source location through the panic hook;
   - gossip: topic joined; neighbour up and down; each payload received and whether it was accepted or dropped, with the rule that dropped it and its sender, kind and seq; each payload broadcast;
   - direct protocol: each request sent and served by kind, its outcome and error code, its elapsed time, and whether it rode a held connection or a new dial;
   - peer table: an entry created, changed or removed with its cause; a handle requested and the handle assigned; a lookup replaced;
   - transport, read from iroh's programmatic path API (journal, 2026-11-11T15:28), not from its logs: a connection opened (direction, protocol, id) and closed (side, code, reason); a path opened, closed and selected (location as in decision 6, round-trip time); the home relay's status; a rebind with its cause and each relay probe's answer;
   - inbox and tools: a frame delivered attached or unattached (message id and frame number, never the body); an inbox client connected or gone; each tool call by name with its outcome and, for a send, the `to` handles and the message id;
   - state at intervals: a snapshot of the counters iroh already keeps for the socket and gossip, at start, at each rebind, at stop and every 60 s in between.
   A message id rides on every event of that message's send, receipt and acknowledgement, on both ends; a connection id on every event of that connection.
10. The bridge records: start; the inbox socket connected and lost; each frame received (message id, frame number); the permission class it read and from where; each notice written with its outcome; each frame dropped with the reason.
11. Event names are fixed strings; the set is pinned by a test. Renaming or removing one, or changing a field's meaning, raises `v`. Before v1.0.0 `v` may rise at any release; a reader that meets a `v` it does not know refuses, naming both versions.

## The writer

12. Writing never stalls the node and never misleads. Events cross a bounded queue to one writer thread per file; each line is written whole and at once, with nothing buffered between lines. When the queue is full the event is counted, not waited for, and the next line written is a `dropped` event carrying the count: the only place `n` skips. The stopping sequence empties the queue within one named bound before exit. A killed process loses only what the queue held at that instant; the time from an event to its line on disk is measured when this is built and recorded as a measurement.
13. Bounded by construction. Each file rolls over once at 64 MiB (`.1` replaces any earlier `.1`), so a run holds at most 128 MiB per file. At each process start the oldest runs under `<data>/traces/` are removed until the directory holds at most 1 GiB, skipping, both files together, every run whose events file was written in the last ten minutes (a live process writes it at least every 60 s, decision 9). The four numbers are named constants, stated in the help.

## Tests and the lab

14. Tests read the same stream. Test support has one wait: for a named event from a named member, within a bound, failing with the event's name and that member's timeline so far. It waits on the operating system's notice that the file grew or on a pipe, never on a timer; an in-process node is read from its sink. No test sleeps (`docs/conventions.md`, no-sleep-in-tests), and `just check` gains a step that refuses a sleep in test code, naming file and line.
15. A failing acceptance test prints each member's timeline. The acceptance tests of spec 3.15 run and must pass with the default, `--debug`; none is run with `--no-debug` to make it pass.
16. The lab keeps everything: every member and host the lab fixtures and recipes start runs with the default, the recipe that fetches run records from a server fetches both files of each run, and one recipe merges the events files of a run directory into one timeline ordered by `wall_ms`, then `run` and `n`.

## Not built

A switch by environment variable or configuration; levels, filters, sampling; upload, bundle or remote collection; a metrics server; QUIC-level logs (iroh's `qlog` feature stays off); a viewer; replay of a trace into the node; a rewrite of the node loop as a pure state machine. Each waits for a need that exists.

— owner-directed, 2026-11-11; written by the lead (self) from the owner rulings quoted above and the evidence each decision names.
