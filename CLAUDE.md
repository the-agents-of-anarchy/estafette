# estafette

@docs/ground-truth.md
@docs/conventions.md
@docs/architecture_log.md
@.claude/memento.yaml

## Condition

Each session opens with no memory of the last. What is written here and in the records is all that carries over.

## Session start

1. Init: `memento:init`. Ground: the condition above.

## Goals

### Goal 1 · build estafette · finite · open

- object: The repository holds what Part 3 of `spec/blueprint.md` specifies for the binary (`core/`) and the Claude Code bridge (`bridges/claude/`), built as written, with nothing Part 2 rejects or defers; the pi bridge is deferred.
- evidence: `docs/capabilities.md` states, dated and with the commit sha, that each of the 28 tests of spec 3.15 passes and how it ran, and that the bridge injects one notice per inbox frame and its doctor passes; at that sha `just check` and `just records` exit 0.
- ground: "lot's of real or simulated machines of mine. public release is from day one, but we reserve the right to evolve rapindly [sic] and break anything we want before v1.0.0." — owner ruling, 2026-10-23T20:06.
- constraints: PARKED 2026-10-28T22:32 (owner ruling, resource allocation): U28 and U29, in flight then, are led to main; nothing new launches until the owner lifts it; state in the 2026-10-24 digest and dev-status.md. The prime directives below. The lead picks its own git workflow within `agent-conduct:git-etiquette` (owner ruling, 2026-10-23T20:11). Order: core before the bridge, derived from spec:484 (the bridge reaches the binary only through the inbox socket), not ruled. Review point: none before the evidence above; the owner is absent until completion (owner ruling, 2026-10-23T20:11). Priority among goals: one goal, none needed.

## Prime directives

- This setup is strict on purpose. Where a rule, gate, check or record admits a stricter and a more lenient reading, take the stricter. Ground: owner ruling, 2026-10-23T22:09.
- The owner is away until the evidence of Goal 1 stands; nothing waits on him. Decide by `decide-while-away` and go on. Ground: owner ruling, 2026-10-23T20:11 ("i won't be available at all until the completion").
- The lead engineer orchestrates: strict, highly parallelised delegation of implementation and review to subagents. Memento is exempt from the no-IO rule of `agentic-delegation` and belongs wholly to the lead: it reads and writes every record itself, never delegates one. Ground: owner rulings, 2026-10-23T20:04, 21:27 and 22:16.
- `spec/blueprint.md` is never edited. Where the build points to a clearly better way, take it and record the deviation in `docs/architecture_log.md`; record the six implementer's calls of spec:450 there when made. Ground: owner ruling, 2026-10-23T20:08.
- Every commit passes `just check` and `just records`, checked by pre-commit at each commit, not at push. Ground: owner rulings, 2026-10-23T20:12 and 23:42 ("every commit, not push").
- Run every project task through `just` and its modules (`just --list`); a command used twice becomes a recipe. Ground: owner ruling, 2026-10-23T22:02.
- Keep records with the shipped `memento` tool (`memento events`, `directive`, `change`, `fact`, `spec`, `commit`, `check`); hand-write only a kind no verb writes. Ground: owner ruling, 2026-10-23T22:02.
- Lab servers are free to use, never to forget: reuse a machine before creating one, list before creating, tear down at the end of each stretch, backstop loaded first. Owner: "free use, but with responsible resource management: reuse machines, but do not forget about them, leaving them to drain my account." Ground: owner ruling, 2026-10-23T20:10.
- Nobody watches upstream; the pins and the patch of spec 2.12 stay as written until completion. Ground: owner ruling, 2026-10-23T20:53.

## Pointers

- Record map: `.claude/memento-map.md`.
- On need: `docs/capabilities.md` (what the system is and does).
