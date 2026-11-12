# Resource-aware orchestration: the ingredients of the model

Scope: how the lead plans and spends the two resources that bound the build, wall time and the account's token limit, from resource planning down to the wording of a delegation. Unbuilt: this record collects ingredients; the rules drawn from them so far stand as working directives in `docs/conventions.md` (`resource-aware-orchestration`, `cache-aware-resumption`, `spare-the-strong-context`) and are amended as ingredients settle. Nothing here is a conclusion until its ingredient is marked settled.

Each ingredient carries its kind: owner-stated (the owner's words, dated), published (a source and the date it was read), measured (what was counted, by which tool, when), reported (an agent's claim the lead has not rerun), not determined (and what would settle it).

## The owner's statements, 2026-11-12, verbatim

1. "you should track everything in flight with stats: which model, how much token has it invested into it already, etc. you make the decisions to the best of your humongous ability. are you sure you can't track the token limit spend by yourself? doesn't claude ship some sort of hook or something that allows introspecting into those stats?"
2. "the investigators on fable level - those you need to leave alone and do the grunt work with cheap agents, then you can still follow-up with them and their judgement when enough things can be brought to them back. their context - as is yours - are the most precious commodity we have."
3. "my take on the limit-exhaustion crash hazard is that it guarantees to let the cached context for all the agents and for the session to expire. if you wait to continue on limit reset - chances are the limit reset won't come in an hour (which is the cache TTL and cannot be configured). and if you re-login to get a new limit, the cache is identity-tied and you evict it anyway. this means that for every conversation - main or subagent - that must carry on through the limit exhaustion crash, we must now burn a big chunk of this new limit on paying the cost again."
4. "which means that resumption must be cache-and-token-spend-aware. and token spend and subagent launches and models choices token-limit-informed."
5. "a trick: in anticipation of cache getting cold, you can follow-up with agents while they are warm and export the most expensive things to re-derive, if they are not in files yet. with methodology that would allow to reproduce the \"results\"."
6. "token cost is not distributed evenly - different models can be orders of magnitude different in expense per token. and further discrepancy is in how unit of value delivered maps onto tokens spent - capability allows less tokens spend for the value. 'bang-for-token' and 'dollar-per-token' (in token-k). these MUST be taken into account. we need a unified mental model of token spend, with effects on everything starting with resource planning and ending with delegation specifics."
7. "all of this gradually ties up into single 'resource-aware-orchestration'. wall time and token limits are usually the resources 90% need to plan."
8. "there is a capability comparison somewhere online as well. i need info on bang-per-token. and different effort levels for different models change the picture drastically. additionally, i'm not sure how that works, but i think the effort level set in the harness propagates to all the subagents? before we jump to conclusions prematurely, we should dilligently collect the precious ingredients of the mental model we're building. nothing gets lost to time. the WIP directive you can keep and modify throughout."

## Dollar per token

- Published, read 2026-11-12 from `https://platform.claude.com/docs/en/about-claude/pricing.md` (model pricing table), US dollars per million tokens, in the order fresh input, output, cache write kept five minutes, cache write kept one hour, cache read: fable 5.2: 8, 40, 10, 16, 0.2; fable 5.1: 10, 50, 12.5, 20, 0.25; opus 5.5: 4, 20, 5, 8, 0.2; sonnet 5.5: 2, 10, 2.5, 4, 0.1; haiku 5.5 (prompts up to 100,000 tokens): 0.1, 0.5, 0.125, 0.2, 0.01; haiku 5.5 over 100,000: 0.5, 2.5, 0.625, 1, 0.05. The lead read these from the research agent's price file; the page itself was read by that agent.
- Derived by arithmetic from that table: on fable a cached token costs eighty times more to write for an hour than to read; output costs two hundred times a cached read; fable and opus read a cached token at the same price; a token read by haiku costs an eightieth of one read by fable.
- Not determined: how the 5-hour and weekly limits weigh a token by model and by kind (no published weight found on 2026-11-12); a `/usage` reading before and after a known spend would settle it. Whether the account pays list price.

## The cache

- Stated in the harness's own tool description for this session: requests use a one-hour prompt-cache lifetime.
- Owner-stated (statement 3): the lifetime cannot be configured; a new login evicts the cache because it is tied to the identity. Not found in the documentation read on 2026-11-12; a write under one login and a read under another would settle it.
- Derived: a conversation continued within the hour pays the read price on its context each turn; continued after the hour it pays the write price on the whole context once. What a conversation ingests (tool results, files) is written once at the write price of its model.

## What conversations hold (measured)

- Measured 2026-11-12 14:35 from the transcripts' usage entries (unique message ids, the last message's prompt size as the context): trace-seam's implementer 516 thousand tokens (opus), the lead 423 (fable), trace-seam's quality reviewer 379 and spec reviewer 335 (opus), the path-reselect implementer 333 (opus), the two fable investigators 320 and 297; the thirteen agents active in the hour before held 3,466 thousand between them.
- Measured earlier the same day, 60 investigation agents of the restart-failure family: sonnet, 53 launches, 10.5 million fresh and 159 million cache-read tokens; opus, 6 launches, 1.7 million and 81 million.
- Measured: the two fable investigators of lab tests 7 and 26 each confirmed a root cause with a one-variable change in 27 and 32 minutes, about 0.4 million fresh and 14 and 18 million cache-read tokens each. Not a controlled comparison with the family above: different problems.
- Reported, not rerun by the lead (research agent, 2026-11-12, list prices): this session to that hour cost 2,793 dollars, the lead's own conversation 846 of it, cache reads and writes 83 percent of the dollars.

## The limit, seen from inside

- Reported from the Claude Code documentation (research agent, 2026-11-12; this machine runs 2.1.322): since 2.1.80 the status line command receives `rate_limits.five_hour` and `rate_limits.seven_day`, each with `used_percentage` and `resets_at`; `refreshInterval` reruns it while a session waits. Not determined: whether this login's input carries them, and whether the interval fires while the lead is idle; the tooling unit usage-figures settles both on its first live run.
- Measured: the limit stopped the work four times between 2026-11-11 and 2026-11-12; at the last stop (13:52) four agents were running.

## Bang per token

- Not determined: the published capability comparison of the models in use; effort levels, what each costs in tokens and what it buys, per model; how Claude Code sets effort and whether subagents take the session's level. Three research reports are ordered for these.
- Known from the lead's own tool description: an agent's model and reasoning effort come from its definition; a launch may name an effort, and the tool allows that only on the owner's or an instruction's explicit word; a launch that names none runs the agent "at its usual effort". The lead has named none on any launch.
- Not determined: this project's own cost per integrated unit by implementer tier and by number of review cycles.

## Rules drawn so far (working, in `docs/conventions.md`)

`resource-aware-orchestration` (two planned resources; acts priced before they are made; reading on the cheapest model; strong contexts small; conversations kept warm or exported), `cache-aware-resumption` (a cold conversation is weighed against a fresh agent on files), `spare-the-strong-context` (cheap agents gather, the strong agent judges), `wakes-and-resumes` (wakes less than an hour apart while work is in flight).

— owner-directed, 2026-11-12; written by the lead (self) from the owner's statements quoted above, the published table, and the measurements named, each marked with its kind.
