# Lab: remote machines for the multi-machine tests

Spec 3.15 tests 3, 15, and 22 to 28 need a second machine. The project skill `.claude/skills/run-hetzner-lab/` provisions one Hetzner server for them and removes it by a deadline; read its SKILL.md before any `hcloud` command.

- Recipes: `just remote::help` (module `remote.just`): `ip`, `ssh`, `mosh`, `plugins`, `list`, `forget-host`, `backstop-status`.
- Use: one server reused across tests, deadline label on every resource, `just remote::list` before creating, teardown at the end of each stretch.
- The owner's Mac must hold the hourly deadline backstop (`launchctl list | grep com.estafette.lab-backstop`) before any creation; the install note is in the skill's SKILL.md.
- Credentials: hcloud context `estafette-lab` and key `~/.ssh/estafette_lab`, both held outside the repository.
