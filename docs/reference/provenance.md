# Provenance of the moved research and spec

## Source

Everything below was copied from the repository
https://github.com/the-agents-of-anarchy/marketplace at commit
3dcd9af1cc773f2f862b4e9f80a3b067c7da7fb0, keeping each file's relative path.

## Moved

- `spec/blueprint.md`: the estafette spec, revisions S1-S27 applied.
- `research/stack/README.md`, `ledger.md`, `mechanic.md`, `parameters.md`,
  `plan.md`: the research index, decision ledger, and reference documents.
- `research/stack/ground/` and `research/stack/index/`.
- `research/stack/experiments/`: all 101 files (reports e1-e25, the e14 bug
  report package with its bundles and results).
- `research/stack/experiments-index.md`: an index of the experiments, written
  after the move and added unchanged.

Every moved document is byte-identical to its source (checked by SHA-256).
Nothing was edited, reformatted, or corrected, so stale statements travel with
the text (for example the header of `staged-revisions.md` upstream).

## Left in the marketplace archive

Under `research/stack/` at the same commit: `surveys/`, `specimen/`,
`solutions/`, `showcase.md` and `showcase/`, `state/`, `verify/`, `lab/`,
`spec-review.md`, `spec-review-2.md`, `staged-revisions.md`,
`spec-apply-report.md`. The Hetzner lab skill is provisioned separately; no
`.claude/` content was moved. The marketplace journal under
`docs/orchestration_log/` also stays there.

## Dangling references

Moved documents cite paths that do not exist here:

- `docs/orchestration_log/history/2026-10-15/events.md` (README, ledger,
  mechanic and others) and `docs/orchestration_log/history/2026-10-16-node-as-identity.md`
  (ledger): the marketplace journal.
- `.claude/skills/run-hetzner-lab/SKILL.md` and
  `.claude/skills/link-remote-peer/SKILL.md` (README).
- Links from the README and ledger into the left-behind directories above
  (`surveys/`, `specimen/`, `solutions/`, `state/`, `verify/`, `lab/`,
  `showcase*`, `spec-review*.md`, `staged-revisions.md`, `spec-apply-report.md`).
- `lab/linux/*` and `verify/v*` evidence cited by the ledger.

Resolve them by reading the archive at the commit above.
