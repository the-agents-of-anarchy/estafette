# Stack research plan

Owner: ryzhakar. Adopted 2026-10-16 ("we follow it"). Orchestrator: resident-anarchist (claude-fable-5-1, now Opus 5.5).

## Question
For estafette (spec: agents-of-anarchy-marketplace `spec/blueprint.md`; repo github.com/the-agents-of-anarchy/estafette), which ranges of functionality do iroh, its ecosystem, and the wider crate world solve; what each costs in our dependency tree; what we copy; what we write.

## Fixed by the owner
- MCP: take the official Rust crate. Not a decision; only version, features, and tree cost are measured.
- Test harness: shallow now, researched later.
- What iroh projects chose: deep.
- Claude Code and pi bridges: out of scope.
- Models, as in the plan the owner adopted: haiku for mechanical work (fetch, parse, measure, index, shallow rows), now in the mix because it is more capable and cheap, always with small context batches (one narrow scope, at most three projects or one crate family per agent); sonnet for the deep source-level rows; opus for the synthesis. Corrected 2026-10-16 on the owner's word: "that's not what i said."

## Round 1 (parallel)
| Agent | Output |
|---|---|
| capabilities: every capability Part 3 of the spec needs, by section, tagged by range | ground/capabilities.md |
| tree: scratch crate outside the repo with iroh 1.x, iroh-gossip, the official MCP crate (stdio server), clap derive; resolved tree, enabled features, small crates already present, MSRVs vs 1.99.1 | ground/tree.md |
| index: awesome-iroh, n0-computer repos and examples, crates.io reverse deps of iroh and iroh-gossip with activity, iroh site project list | index/index.md |

## Round 2 (after round 1; surveys by decision, not by index category)
| Survey | Depth |
|---|---|
| foundation: endpoint, ALPN protocol routing, embeddable relay server, local discovery, tickets; iroh-gossip version, events, stability | deep, source; split across agents by crate |
| projects: presence over gossip | deep, source; batches of at most three projects |
| projects: direct protocol (framing, encoding, any n0 RPC layer) | deep, source; batches |
| projects: identity and membership (signed claims, private networks, tickets as invitations) | deep, source; batches |
| projects: infrastructure (self-hosted relay, bootstrap nodes) | deep, source; batches |
| small pieces: YAML, TOML, JSON lines, UUIDv7, base32, XDG paths, signals; each in-tree, copy, or pull | shallow |
| test harness: what iroh offers for tests | shallow |
| breadth: libraries that already do presence plus direct messaging over iroh | shallow |
Projects agents read source, not READMEs. Outputs: surveys/<slug>.md.

## Round 3
Verify only what round 2 promotes: any library that would take over a whole range of the spec. One agent per candidate, source-level. Contradictions get a resolver. Outputs: verify/<slug>.md.

## Round 4
One opus agent reads every report; writes synthesis/stack.md: per range, the crate that solves it (in tree or new pull), what we copy, what we write; a draft manifest with features. Where a project's practice argues against a spec decision, it lists the point for the owner to deliberate; it never edits the spec. Promoted to estafette docs/reference/ like the bootstrap standard.

## Rules for every agent
Bind cargo-cult-science and stop-yapping. Primary sources: manifests, registry API, source. README claims are hypotheses. Evidence, not recommendations, except the synthesis.

## Progress
| Round | Agent | Status | Output |
|---|---|---|---|
| 1 | r1-capabilities (haiku) | done 2026-10-16 01:01 | ground/capabilities.md: 174 capabilities, 12 ranges, 28 ambiguities |
| 1 | r1-tree (haiku) | done 01:00; file saved by the orchestrator | ground/tree.md: iroh 1.3.0, iroh-gossip 0.101.0, rmcp 3.5.1, clap 4.6.7; 276 crates |
| 1 | r1-index (haiku) | done 01:04 | index/index.md: ~290 entries; prior art found: walkie, alink, agent-swarm, mcpmesh |
| 2 | first fan-out: 13 sonnet + 3 haiku at 01:10 | failed 01:44: session limit hit; one report survived (surveys/p1-presence-n0.md, complete); clones kept under /tmp/ra-src | |
| 2 | wave 1, haiku, 11:42: f1a-iroh-endpoint, f1b-iroh-discovery, f2-gossip, f3-relay, d1-irpc, a3-habilis | done 11:46 | surveys/ |
| 2 | wave 2, haiku, 11:47: a1 walkie+alink, a1b agent-swarm, a2 mcpmesh, a2b buzz post, d2 imsg + iroh examples, i1 iroh-auth + rcan | done 11:52 | surveys/ |
| 2 | wave 3a, haiku, 11:52: i2 chio + iroh-rings, p2 rendezvous + topic tracker, p3a peersey + tunnet, p3b p2panda, r1 rayfish + mshr, s1 small pieces | done 11:58 | surveys/ |
| 2 | wave 3b, haiku, 11:57: s2 tests, s3 breadth | done 12:03 | surveys/ |
| 3 | haiku, 11:57: v1a iroh fixes, v1b gossip and discovery fixes, v2 gossip admission and frame size | done 12:01 | verify/ |
| 4 | opus synthesis, 12:04 | done 12:09; dropped 13:15 on the owner's word (built on unverified leads) | (removed) |

## Facts round 1 hands to round 2
- Already in the tree, free to use: tokio, serde, serde_json, postcard, ed25519-dalek, blake3, rand, data-encoding, uuid, tracing, thiserror, bytes, futures, irpc (via iroh-gossip).
- Absent: any YAML crate, toml, any XDG crate, base32.
- irpc is n0's RPC layer and a candidate for the direct protocol of spec 3.6: the direct-protocol survey must read it.
- iroh-gossip is pre-1.0 (0.101.0) while iroh is 1.3.0.

## Ambiguity notes from round 1 (orchestrator's reading)
- Single-claim race: settled by spec D6/D7, tie-break plus probe, no lock.
- Node id text form: iroh's own; the foundation survey confirms which.
- `public` bytes: carried and verified verbatim; no canonical JSON needed.
- The other 25 in ground/capabilities.md go to the synthesis.

## Lessons for dispatch
- A sixteen-agent sonnet fan-out spent the session limit in minutes (owner: "commendable fan-out, but token limits stopped you"). Now: haiku for the survey rows, one or two projects per agent, waves of six, the next wave on the previous wave's end. Sonnet only where haiku output proves unreliable; opus for the synthesis.
- Agents write reports incrementally and reuse the shared clones in /tmp/ra-src.
- Some workers refuse to write report files under their harness's rule against unrequested docs. Every prompt states that the file at the named path is the requested deliverable.
- Haiku workers get one narrow scope and at most three projects or one crate family each.

## Round 3 candidates so far
- habilis fork patches (surveys/a3-habilis.md line 80): check whether iroh 1.3.0, iroh-gossip 0.101.0, netwatch, portmapper, swarm-discovery 0.6.1 already carry iroh#4294, the relay teardown drain, the RTM_MISS fix, rkuhn/swarm-discovery#28, n0-computer/iroh-gossip#147.
- gossip frame size: spec must set `Builder::max_message_size` (net.rs:154) above the largest Presence; compute the largest frame from 3.7.

## Orchestrator corrections to worker reports (for the synthesis)
- i1-auth.md says any member can attach any seen claim to a message. Spec 3.6 step 4 requires `from.node` to equal the TLS-authenticated remote id (f1a-iroh-endpoint.md: `Connection::remote_id()`), so a replayed claim from another node is refused.
- v2-gossip-gate.md says the handle grammar is undefined. Spec 3.1 defines it: `^[a-z][a-z0-9-]{0,31}$`; the effective handle may add `-n`, so the maximum handle on the wire is a few bytes above 32.
- a3-habilis.md says the RTM_MISS fix covers portmapper; v1a-iroh-fixes.md shows portmapper has no such code.
- capabilities.md's top three ambiguities: see "Ambiguity notes from round 1" above.

## Risk retirement (owner ruling 2026-10-16)
| Item | Agent | Model | Where | Output |
|---|---|---|---|---|
| irpc re-research, fresh eyes, prototypes both paths | v3-irpc | sonnet | /tmp/ra-v3 | verify/v3-irpc.md: done 12:30, hand-written confirmed |
| E1 compile spike of risky APIs, tree count | e1-api | sonnet | worktree estafette-e1-api, spike/e1-api | experiments/e1-api.md: done 12:30 (bb3ef21) |
| E2 serialized-size threshold | e2-size | sonnet | worktree estafette-e2-size, spike/e2-size | experiments/e2-size.md: done 12:29 (1117a66), minimum 4407 |
| E3 two-node kill timing | e3-kill | sonnet | worktree estafette-e3-kill, spike/e3-kill | experiments/e3-kill.md: done 12:27 (1bf7209), 33 s default, 13 s tuned |
| How projects solve the presence problems (liveness, late join, departure, identity binding, first contact) | so1-so5 | haiku, one problem each | reads prior reports by problem | solutions/*.md: done 12:34 |
| Specimen synthesis | sp-synth | sonnet | reads specimen/sp1-sp5, a3, a1b, s3 | specimen/specimen.md: done 12:34 |
- a3-habilis.md lines 8, 16, 85, 86 misstate the lineage: per specimen/sp1 and sp4, the engine began in agent-gossip, passed through a vendored copy in mallorca, and became habilis-network, which agent-gossip (same repo as agent-swarm) now consumes by git rev. Unverified; the specimen synthesis reconciles.
- e1-api.md line 54, so5-first-contact.md line 62, i1-auth.md line 74: "the sandbox firewall blocks outbound calls" and "the sandbox returns 403 for crates.io" are false. There is no sandbox (no /run/sandbox); dns.iroh.link answers 200 and crates.io answers 200 with a User-Agent (it returns 403 only without one). The pkarr path was not exercised because E1 chose the Minimal preset, not because anything blocked it.

## Goal, as rephrased by the owner (2026-10-16 13:30)
"now we need to inform the spec with tech reality, optimising for our values at stack level already." Values in force: the least written code with no real compromise; strictest standards for what we do write; every pulled crate owned; no center, no mailbox, a society-blind transport. Ground truth: research/stack/ledger.md.

## State mechanics (launched 15:06 without the owner's go; kept running on the owner's word)
| Mechanic | Agent | Projects | Should settle | Output |
|---|---|---|---|---|
| Roster | m1-roster (sonnet) | habilis-network, iroh-smol-kv, Tunnet | is last-writer-wins by timestamp enough; replay rejection; Gone retention; Unreachable as a status; memory bounds | state/m1-roster.md |
| Names | m2-names (sonnet) | alink, agent-gossip, mcpmesh | first-seen suffixing (D5) in practice; nodes naming one peer differently; key vs name addressing | state/m2-names.md |
| Inbox | m3-inbox (sonnet) | agent-gossip, walkie, iroh-gossip | one sequence number vs per-consumer cursors; buffer and dedup sizes; overflow | state/m3-inbox.md |
| Lifecycle | m4-lifecycle (sonnet) | agent-gossip, mcpmesh, Tunnet | file lock for one instance; key load vs endpoint start order; tests node-as-identity | state/m4-lifecycle.md |
Reading settles which proven designs exist, not whether they hold for estafette. Same-key restart behavior stays an experiment.

Progress is tracked in research/stack/ledger.md: the surface map by range, then the assumption table, then OPEN.
- state/m1-roster.md says "estafette today: no timestamp or sequence field on Presence"; spec 3.7 has `ts` on Presence and in the Claim. It also maps habilis's ghost rule onto estafette's inbox replay; the inbox replays to the local bridge only and never feeds the peer table, so that hazard does not transfer.

## Experiments, round 2 (owner: all four approved, "2 at a time, not all 4")
| Pair | Experiment | Agent | Worktree, branch | Output | Status |
|---|---|---|---|---|---|
| 1 | presence flow end to end, with S2, S12, S13 applied | e4-presence (sonnet) | estafette-e4-presence, spike/e4-presence | experiments/e4-presence.md | done 19:45 after a stall and resume |
| 1 | same-key restart, lock, duplicate holder | e5-restart (sonnet) | estafette-e5-restart, spike/e5-restart | experiments/e5-restart.md | done 18:47 |
| 2 | address lookup by id: n0 pkarr, mDNS, address in presence, several relays, self-hosted DNS | e6-lookup (sonnet) | estafette-e6-lookup, spike/e6-lookup | experiments/e6-lookup.md | running |
| 2 | gossip admission: an outsider with topic id and bootstrap address | e7-admission (sonnet) | estafette-e7-admission, spike/e7-admission | experiments/e7-admission.md | running |
