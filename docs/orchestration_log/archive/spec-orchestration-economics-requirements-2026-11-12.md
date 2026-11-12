# Orchestration economics: requirements for the mental model

Scope: what the mental model of orchestration economics must be and must not be. This file holds requirements only; the model itself is not written yet. Everything else known (facts, measurements, derivations, unknowns, artifacts) is in `spec/orchestration-economics-ingredients.md`. Written by the lead (self) at 2026-11-12T16:41 on the owner's order: "dump the whole totality of your understanding around mental model (orchestration economics) into 2 files, right this instant: 1. the requirements 2. everything else besides requirements. it must enable you after compaction to write the orchestration economics mental model essentially fresh." Owner: Arthur Ryzhak. All quotes below are owner rulings of 2026-11-12, verbatim, also in the journal (`docs/orchestration_log/history/2026-11-09/events.md`); the eight earlier statements of that day stand verbatim in `spec/resource-aware-orchestration.md`, section "The owner's statements".

## The owner's words that set the requirements (in the order given)

1. "'bang-for-token' and 'dollar-per-token' (in token-k). these MUST be taken into account. we need a unified mental model of token spend"
2. "all of this gradually ties up into single 'resource-aware-orchestration'. wall time and token limits are usually the resources 90% need to plan."
3. "before we jump to conclusions prematurely, we should dilligently collect the precious ingredients ... nothing gets lost to time. the WIP directive you can keep and modify throughout."
4. On the count of the lead's own turns: "that's not something you have an option to configure. but you know now why i demand token frugality."
5. On the value side: "every time anthropic publishes a new model, they do a comparison of results you can achieve with the model, compared to other models in the lineup. usually, at different effort settings. additionally, people online do those comparisons routinely as well."
6. "the lower base cost of a model is, the higher effort (proxy for tokens spent on unit of "thought") we both can afford and usually need compensate capability drop with."
7. On logging every limit change: "costly and unnecessary. approximation is okay. we should allow checkpoint measurements at runtime."
8. "i intend on sending these upstream to update instructions for orchestration."
9. "you know which parameters are tied in as well? wall time. token generation costs time as well as money. it could be that on huge effort differences it's better to pick more capable models with lower effort settings." and "let's get more clarity. all the resources, variables and knobs we have and can control at runtime. no yapping, naturally."
10. On effort needing his word: "the goal is EXACTLY for me to not weigh in on this ever again. we're setting up a single mental model within which you operate and make EVERY decision." On the subagent-effort environment variable: "i won't deal with this." On speed: "effort is irrelevant to generation speed. but effort makes the model generate more tokens - increasing both capability and resource spend."
11. On keeping defaults until measured: "you take incredibly lazy hands-off stance on model effort management. i'm very disappointed."
12. "you essentially have not N-models models, but N-models x M-efforts models to manage."
13. On the lead launching an agent to author the policy: "wtf? you delegated that? we are not done discussing that."
14. "you jump to prescriptions too fast. we won't settle a table. no delegation table prescription. we must define a clear set of rules to abide by. by now we must have discovered the economics. we approximate the abstract value delivery by tasks on benches, etc. we take that and approximate work unit delivery for 100k tokens generated. for every model we have token cost - both monetary and temporal. for every model x effort we have what? capability? work effectiveness per 1k tokens? we do this from the first principles, hombre. to simplify things a bit, we only care for 3 effort settings - low, high, max."
15. "generation speed for claude code specifically should be online, measured by people. that can help approximate haiku."
16. "the mental model must be simple. least possible prescriptions. we hand a lego, you decide at runtime."
17. "on top of the atoms, we should share robust vectors on how parameter changes affect resource usage."
18. "atoms, mechanics, parameter chain influence demonstrations. most signal from our research, least noise."
19. "possibly, a just module delivered that would allow to compute things at runtime planning."
20. On the five-minute subagent cache: "that's horrendous :( interferes with the whole dev cycle assumptions." Then: "the dev-cycle reconciliation to the model-modified orchestration skill is not our concern. we just needed to know if we need to change something this instant, etc." and "let's rehash what we want there to be, conceptually, and what we must not leave there. don't output the mental model itself! we're refining requirements."
21. Earlier the same day (in force): "the investigators on fable level - those you need to leave alone and do the grunt work with cheap agents, then you can still follow-up with them and their judgement ... their context - as is yours - are the most precious commodity we have."; "resumption must be cache-and-token-spend-aware. and token spend and subagent launches and models choices token-limit-informed."; "a trick: in anticipation of cache getting cold, you can follow-up with agents while they are warm and export the most expensive things to re-derive ... with methodology that would allow to reproduce the "results"."

## What the model must be (as put to the owner at 16:20; he did not object, he has not confirmed)

- One model inside which the lead makes every launch, continuation and stop decision with no question back to the owner; effort included (10, 11).
- Simple: blocks handed over ("lego"), decisions made at runtime; least possible prescriptions (16).
- From first principles (14), in three layers: atoms, mechanics, parameter chain influence demonstrations (18), with robust vectors of how a parameter change moves resource use (17).
- About two resources: wall time and the usage limit (2, 9).
- Model x effort is one lineup of N x M models (12); three effort settings only: low, high, max (14).
- Value approximated from benchmarks as work units delivered per tokens generated (per 100k); cost per token both monetary and temporal, per model; a quantity per model x effort (14, 1, 5).
- Uneven token cost visible: by model and by token kind (1).
- Cache behaviour a first-class part, since it decides what continuing costs (21, 20).
- Approximate, correctable at runtime by checkpoint measurements (7).
- Portable: fit to go upstream into orchestration instructions (8).
- Most signal from our research, least noise (18).
- Possibly accompanied by a `just` module that computes it at planning time (19).

## What it must let the lead do, from the text alone

- Choose a model and an effort for a launch.
- Choose between continuing an agent and starting a fresh one.
- Choose how much runs at once.
- Explain afterwards why the limit or the clock moved as it did.
- Re-derive all of that when a price, a weight or a harness behaviour changes.

## What must not be in it

- A delegation table or any settled assignment of model and effort to roles (14). The lead's role tables of 15:33 and 15:37 were withdrawn.
- Rules that only restate what the mechanics imply (16). The nine rules the lead listed at 16:05 were dropped for that reason; they are kept as derivations in the ingredients file.
- Anything that needs the owner's word at runtime (10).
- How the dev cycle should change (20).
- Noise: the history of how it was found, process steps, briefing habits (18).

## Proposed by the lead, not yet confirmed by the owner

- No project particulars: unit names, review-cycle counts, limit readings, logins.
- No harness behaviour stated as a law (the five-minute subagent cache, effort not crossing models): name the parameter and how to measure it.
- No unchecked figure: only what the opus check or the lead's own transcript check confirmed.
- No commands of the `just` module: the module computes the model, the model stands without it.

## Open point put to the owner, unanswered

"Most signal from our research" against "portable": either the demonstrations carry our dated numbers as worked examples and the atoms carry none, or figures live only in a parameter file beside the text. The lead leans to the first.

## How the owner wants the work on it done (process requirements, from his reactions)

- The thinking is the lead's, in the conversation with him; no agent authors the model (13). Cheap agents do grunt extraction only (21).
- No prescription before the economics is laid out; no jumping to tables (14). Do not output the model before he asks for it (20).
- Active management of effort, not defaults (11); never ask him about effort again (10).
- Token frugality in the lead's own turns (4); no figure relayed unchecked (the lead relayed three wrong sonnet figures and had to correct them).

- owner-directed, 2026-11-12; written by the lead (self).

## Names

The names are settled (2026-11-12): `spec/orchestration-economics-ingredients.md`, section "Names settled by the owner". They replace every name used above; orchestration economics is written in them alone.
