# The settling mechanic

Owner (2026-10-16): "we incrementally settle the theoretical-spec-vs-tech-practice collision with interactive owner-approved spec revisions and tech stack decisions." Values in force: the least written code with no real compromise; strictest standards for what we write; every pulled crate owned; no center, no mailbox, a society-blind transport.

## Artifacts
| Artifact | Holds | Who writes |
|---|---|---|
| spec/blueprint.md | the spec; changes only by applied revision batches | orchestrator, on the owner's word |
| research/stack/ledger.md | surface map by range; spec assumptions with status; collisions; staged revisions | orchestrator |
| research/stack/plan.md | dispatch, corrections to worker reports | orchestrator |
| research/stack/{experiments,verify,state,...}/ | evidence | workers |
| journal (history/2026-10-15/events.md) | every ruling verbatim, every receipt, every failure | orchestrator |
| docs/orchestration_log/history/<date>-blueprint-edits-N.md | one report per applied batch, with the full diff | orchestrator |

## One collision, start to end
1. SURFACE. A spec sentence that assumes something about technology becomes a ledger row: ○ open.
2. EVIDENCE. A lead picks the test; a run or a primary record settles the row: ■ holds, or ■ CONTRADICTED. A lead never settles a row.
3. COLLISION. A contradicted or underdetermined row becomes a collision card: the spec text, the evidence, two to four options each costed in our values (lines written, crates pulled, centers or mailboxes created), one recommendation.
4. RULING. The owner rules through portable, rapid-fire questions in a turn answering the owner. The ruling is journaled verbatim and marked ▣.
5. STAGING. The ruling becomes a staged revision: either a spec edit (named sections, intended text) or a stack decision (crate, exact version, features, reason, rejected alternatives). Stack decisions are written as the spec's own D-records (Decision, Reason, Rejected, If you change this) so the stack lands inside the blueprint, not beside it.
6. PROPAGATION. A ruling that moves an upstream decision (node-as-identity) reopens every row resting on it; those rows go back to SURFACE.
7. APPLICATION. Staged revisions accumulate. On the owner's word, one batch is applied to spec/blueprint.md at once, with a diff report. The pre-research blueprint stays recoverable from git and the Downloads original.

## Gates
- Nothing enters the spec without a ruling. Nothing becomes evidence without a run or a record. The orchestrator checks a claim before it reaches the owner.
- Haiku fetches, counts, indexes. Anything a ruling rests on is run, or read at source by sonnet with the project's tests run.
- Questions to the owner carry their own context; no familiarity assumed.

## Done
The post-research blueprint is the pre-research blueprint plus applied batches, when:
- every surface row a spec section rests on is ■, ▣, or explicitly deferred by the owner;
- no staged revision is left unapplied;
- the stack is fully in the blueprint: one D-record per dependency choice, and a manifest table in Part 3 with exact versions and features;
- every acceptance test in 3.15 still describes behavior the evidence supports, or was restated;
- the original Downloads blueprint and every batch diff remain for audit.
