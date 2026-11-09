# 2026-11-09 — failures

## A secrets auditor printed the lab invitation into its own transcript

What happened: the audit-secrets agent's masking pattern keyed on the value shape and missed the 'invitation: ' prefix of lab/host.txt, so one check printed the whole lab invitation (which carries the lab network's secret) into the agent's tool output, which lives in the harness transcript outside the repository.

Mechanism: a mask applied after the match prints the line when the match fails; the brief told the agent what not to print but not to pipe every grep through a cut that keeps four characters whatever matched.

Correction: the lab network is a throwaway created today and dies with the host at teardown (no rotation of anything durable needed); future audit briefs order every search output through 'cut -c1-<prefix+4>' or an awk mask applied to every line, never a match-dependent mask; .gitignore gains .env, *.key, *.certs/ and lab data paths (tooling change launched).

## Stopping an uncommitted implementer removed its worktree

What happened: impl-U43 was stopped by TaskStop before its first commit so that Addendum 1 could be written into its brief; the harness removed the unchanged worktree on the stop, and the resume through SendMessage ran with the agent's context but no checkout, so it reported BLOCKED with nothing built.

Mechanism: The harness sweeps an agent's worktree on stop when the worktree holds no change; a resumed agent keeps its transcript, not its checkout.

Correction: Recreated the worktree with git worktree add on the surviving branch name at integration HEAD b56e309 and continued the agent with the path, as dev-cycle route-on-status prescribes; from now on an implementer is stopped only after its first commit, otherwise the correction is messaged without a stop.

## The lead asked the owner for an in-session act as if present

What happened: A request to switch test-peer's permission mode was put to the owner as a step on the critical path; the owner answered 'you're fucking assuming my presence again'.

Mechanism: The prompting-receiver cases needed a receiver that prompts, and the lead treated the owner's earlier offer to switch modes as an invitation to ask now.

Correction: The item moved to the unsolved queue as an owner-only act; the agent was told to finish without it; U43's live verification uses the owner-provided live sessions as they are and records the other mode as not determined.
