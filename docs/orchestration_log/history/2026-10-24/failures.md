# 2026-10-24 — failures

## Lead ruled against the spec on a reviewer's word

What happened: What happened: the spec-U18 verdict called the Probe/Introduce silence while stopping an Extra, citing spec 3.9; the lead ruled to remove it and continued the implementer without reading the row; the implementer removed it, then cited spec:784, which requires the silence.

Mechanism: Mechanism: a reviewer's claim about the spec was treated as checked; the lead's rule to read a verdict's line and headings only, never its body, left the claim unverified, and no spec line was read before ruling on a spec question.

Correction: Correction: a ruling that overturns code on a spec question is made only after the lead reads the spec lines the finding names and the implementer's own citation; a correction file now sits beside the verdict and the implementer reverts the change.

## CI watch lapsed for 23 red runs

What happened: What happened: every CI run on main from b318cdd (2026-10-24 12:41Z, U30's integration) to 0db693b failed at the Run just check step while the lead kept fast-forwarding main; the owner noticed by mail on 2026-10-28.

Mechanism: Mechanism: the ci-watch convention names a recurring check by commit, but the lead's heartbeat prompt and wakes carried no CI step, and the session was stopped by the weekly limit for 3.5 days; the local gate on the Mac passed each time, so nothing in the loop looked at Linux.

Correction: Correction: the heartbeat prompt gains the CI check by commit; main fast-forwards hold until CI is green; a diagnosis agent reads the failed logs now and the owning unit's implementer fixes the cause.
