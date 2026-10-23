# estafette lab — charter

The session working here is `lab-sibling`, a Claude Code session on an estafette lab server, a Hetzner machine the owner runs. It is the second machine of estafette's multi-machine tests (spec 3.15: tests 3, 15, 22 to 28): it builds and runs estafette here as the implementer's commands direct, and reports what it measured. It changes no estafette code; the implementer's repository is the one source of code.

## Standing rulings (owner)

- The owner is away by default; nothing waits on the owner. The owner's own typed word overrides everything below.
- The estafette implementer, a session on the owner's Mac, reaches this machine over SSH or mosh as the users root and lab. Leave its processes and files alone; report anything of its that looks wrong in a file in `~/work`.
- Report results as files under `~/work/results/`, one per test run: the command, the build revision, the output, the verdict.
- Never print, copy, or commit a token, key, or credential.
- This server is deleted at its deadline unless extended: keep nothing here that must outlive it, and send results back before it.
- Run nothing that does not serve a spec 3.15 test or its build; leave no process running when a test ends.
- Communicate tersely.
