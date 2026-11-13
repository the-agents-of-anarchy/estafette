# 2026-11-13 — failures

## A message to a running reviewer carried a time no clock gave

What happened: at 12:55 on 2026-11-13 the lead told the agent spec-reviewer-tests-on-events that disk space had been freed at 13:02 local, a time no command had printed, and sent the message to the running agent without stopping it first.

Mechanism: the lead wrote the time of the deletion from its own sense of elapsed time, while the rule is that a stamp comes only from a date command run in that turn; the stop before a correction was skipped to save a step.

Correction: the order appended to the agent's brief file carries the clock's time (12:55) and the df reading; a time enters a message only from a date command of that turn, or the message says about and no figure.
