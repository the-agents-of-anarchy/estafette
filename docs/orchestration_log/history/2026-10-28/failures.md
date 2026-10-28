# 2026-10-28 — failures

## A pre-commit stash restore in the root checkout erased record writes made during the hook run

What happened: integrator-U29 retried its merge-fix commit in the root checkout while the lead wrote records there; pre-commit's 'files were modified by this hook' retry stashed and restored unstaged tracked files, erasing the 22:37 span close (session.md, events.md), the CLAUDE.md parking line and the U24 architecture-log entry written between 22:28 and 22:40; the untracked 2026-10-28 files survived.

Mechanism: pre-commit stashes unstaged changes to tracked files before its hooks and restores the stash afterwards; a write to a tracked file during the five-minute just check run is overwritten by the restore, and the lead and an integrator share one root checkout.

Correction: no write to a tracked record while an integrator holds the root checkout; writes to the open span's untracked files are safe; the lost records were restored by hand at 22:50 with their original stamps and the U24 entry re-added through memento change add; an integrator brief states that the lead writes nothing tracked until the integrator reports.
