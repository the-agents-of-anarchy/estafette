# Project Ground Truth

The problem as given to the lead engineer: what estafette is, where its documents live, how it is judged. Paths are from the repository root; `spec:<n>` is a line of `spec/blueprint.md`.

## The product

- One executable in `core/`: messaging between agents across machines and harnesses, over iroh. The primitive is spec Part 1, the decisions Part 2, the implementation Part 3 (spec:444-1078). Build exactly Part 3; where it is silent, Part 2 rules (spec:448). Vocabulary: spec 2.2 and 3.1.
- Scope: `core/` and `bridges/claude/` (spec 3.2; D35-D37, D39); `bridges/pi/` is deferred (owner ruling, 2026-10-23T20:12). Invariants I1-I7 (spec:110-118) are not revisable.
- Six choices are the implementer's (spec:450); each is a decision in `docs/architecture_log.md` when made.

## Where things live

| what | path |
|---|---|
| the spec, revisions S1-S27 applied, byte-identical to marketplace commit 3dcd9af | `spec/blueprint.md` |
| decision ledger: collisions C1-C19, revisions, evidence by item, accepted limitations | `research/stack/ledger.md` |
| research index, settling mechanic, agreement parameters G1-G5, research plan, ground and index leads | `research/stack/README.md`, `mechanic.md`, `parameters.md`, `plan.md`, `ground/`, `index/` |
| experiments e1-e25, whole, with their one-page index | `research/stack/experiments/`, `research/stack/experiments-index.md` |
| where moved material came from and which references dangle | `docs/reference/provenance.md` |
| the Rust standard: toolchain, lint table, panic and trait rules | `docs/reference/rust-bootstrap.md` |
| the lab for multi-machine tests | `docs/reference/lab.md`, `.claude/skills/run-hetzner-lab/SKILL.md`, `just/remote.just`; tasks are `just` modules under `just/` (`just --list --list-submodules`) |
| manifesto stack of the lead and of each subagent | `.manifestos.yaml`; the owner compiles its interplay and commitments himself (owner ruling, 2026-10-23T21:19) |
| records: schema, map, conventions, architecture log, facts, measurements, journal | `.claude/memento.yaml`, `.claude/memento-map.md`, `docs/conventions.md`, `docs/architecture_log.md`, `docs/capabilities.md`, `docs/measurements.yaml`, `docs/orchestration_log/` |

Moved documents (`spec/`, `research/stack/`, `docs/reference/`) stay byte-identical to their source; a correction is a new record beside them (`.claude/memento.yaml`, filing). The marketplace archive at 3dcd9af resolves every dangling reference `docs/reference/provenance.md` lists.

## Evaluation protocol

- Gate: `just check` runs `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` on toolchain 1.99.1 (`rust-toolchain.toml`); `.github/workflows/ci.yml` runs it on every push and pull request; `.pre-commit-config.yaml` runs it and `just records` at every commit, not at push. Records: `just records` runs `memento check` pinned to 0.10.0.
- Acceptance: the 28 tests of spec 3.15 (spec:1029-1056) and the 19 prohibitions of 3.16 (spec:1060-1078). Tests 3, 15, and 22-28 need a second machine.
- Lab: Hetzner servers, no count cap (owner ruling, 2026-10-23T20:10), each reused when possible and created only after `just remote::list` and with the backstop loaded on the owner's Mac (`just remote::backstop-status`; loaded 2026-10-23T21:27); credentials live outside the repository. Read the skill before any `hcloud` command.

## Known limitations the owner accepted

Not work (owner ruling, 2026-10-21T23:10; `research/stack/README.md:61-68`): a surviving host blocked 62-87 s on relay-only paths; the deaf rule silent while host records keep arriving; symmetric NAT, CGNAT, and UDP-blocked networks never met; QUIC address discovery in the embedded relay library unconfirmed. Deferred by design: spec:440. Nobody watches upstream releases; the pins and `[patch.crates-io]` of spec 2.12 stay (owner ruling, 2026-10-23T20:53).

— owner-directed, 2026-10-23T21:27 ("everything else you can land"); paths checked against branch integration/scaffold at d0f6081.
