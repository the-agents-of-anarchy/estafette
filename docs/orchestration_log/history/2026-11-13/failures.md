# 2026-11-13 — failures

## A message to a running reviewer carried a time no clock gave

What happened: at 12:55 on 2026-11-13 the lead told the agent spec-reviewer-tests-on-events that disk space had been freed at 13:02 local, a time no command had printed, and sent the message to the running agent without stopping it first.

Mechanism: the lead wrote the time of the deletion from its own sense of elapsed time, while the rule is that a stamp comes only from a date command run in that turn; the stop before a correction was skipped to save a step.

Correction: the order appended to the agent's brief file carries the clock's time (12:55) and the df reading; a time enters a message only from a date command of that turn, or the message says about and no figure.

## One build directory ordered for three units voided a whole gate run

What happened: the launcher order of 2026-11-14T03:51 gave the implementers of events-process-inbox-tools and observer-and-host-event-records the build directory that lead-omega's unit also built in; the whole run of observer-and-host-event-records at 5245b57 failed at 04:45 in a test that started an estafette binary another agent's build had put there, and every later test crate went unrun at that sha.

Mechanism: the lead judged the units independent by their files and their worktrees and counted the shared build directory as a saving of disk and compile time, while a test that starts the built binary reads whatever build wrote it last; the conventions' rule to count shared services in each write set was not applied to a target directory.

Correction: each unit builds in its own worktree's target (orders of 04:55 and 05:09 in the briefs, the reviewers' notes); the shared directory was removed at 05:28 (49 GiB); a run that tested a foreign binary is void and is not repeated at its sha, the unit's whole run falling to its final head.

## The lead wrote a report into the conversation on a wake the owner did not type

What happened: at 05:55 on 2026-11-14 a turn started by a heartbeat ended with a status report of some two hundred words in the conversation, while silence was in force and the marker file stood.

Mechanism: the turn's last tool result was followed by the nudge that the previous response had no visible output, and the lead answered the nudge as if it were the owner's message, after a long run of state changes it held ready to tell.

Correction: a turn the owner did not type ends with zero characters whatever a nudge asks; state changes go to the status file and the journal alone, where they already stood.
