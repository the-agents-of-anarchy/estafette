---
name: run-hetzner-lab
description: >
  Run the Hetzner Cloud lab for estafette from creation to deletion: one reused server, each resource under its own deadline and an hourly backstop check. Use it for the multi-machine acceptance tests (spec 3.15 tests 3, 15, 22 to 28).
  "create a lab server", "hetzner", "hcloud", "a second machine", "a remote machine for the test", "extend the deadline", "tear down the lab", "delete the server", "lab backstop", or any request to create, check, extend, or delete a Hetzner Cloud resource.
---

<guard-the-tokens>
Work through the `hcloud` command under its context `estafette-lab`, the named credential set whose token lives in `~/.config/hcloud/cli.toml`; never read that file.

Keep every token out of the conversation and out of every commit, the Hetzner token and a Claude Code token alike; never print one.

Move a token between machines as a file with permissions 0600 over SSH; never move one another way.
</guard-the-tokens>

<use-the-lab-responsibly>
List before creating: run `just remote::list`, which shows every server, primary IP, firewall, and SSH key in the project; never create a resource that a listed one can serve.

Reuse one server for every multi-machine test of a stretch of work; never create a second server while one runs.

Put the deadline label on every resource at creation; never leave a resource unlabeled.

Tear the server down at the end of each stretch of work, in the order below, and confirm with the lists; never end a stretch with a resource still running.

Never leave a server running unattended: before ending a turn that leaves one, set the wake that ends its use by the `check-back` skill.

Extend a deadline only while lab work on that resource runs or on the owner's word; never extend one for an idle resource.
</use-the-lab-responsibly>

<keep-the-backstop-running>
Count as a lab resource every server, primary IP address, firewall, and SSH key in the Hetzner project; never leave one uncounted.

The backstop job is the launchd job on the owner's Mac that checks every lab resource each hour and deletes those past the deadline their `expires` label names. It must be installed once on that Mac; this skill does not install it on its own. Confirm before each creation that it is loaded with `just remote::backstop-status` (that is `launchctl list | grep com.estafette.lab-backstop`, which prints one line when loaded and nothing otherwise); never create a lab resource while it is absent, and ask the owner to install it when it is.

Install note, run once by the owner or on the owner's word, from this skill's directory:

```
mkdir -p ~/.local/share/estafette ~/.local/state/estafette ~/Library/LaunchAgents
cp lab-backstop.py ~/.local/share/estafette/lab-backstop.py
sed "s|__HOME__|$HOME|g" com.estafette.lab-backstop.plist > ~/Library/LaunchAgents/com.estafette.lab-backstop.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.estafette.lab-backstop.plist
launchctl list | grep com.estafette.lab-backstop
```

Never run the backstop from this skill's directory; copy `lab-backstop.py` into `~/.local/share/estafette/` again whenever this skill's copy changes, and never let the two differ.

Read the backstop's log at `~/.local/state/estafette/backstop.log`, one line per lab resource per run; never take the backstop as working without a line from its last hour.

Remove it on the owner's word with `launchctl bootout gui/$(id -u)/com.estafette.lab-backstop` and the deletion of its plist; never remove it while a lab resource exists.

An earlier lab used the label `com.ryzhakar.estafette-lab-control` and the owner label `resident-anarchist`; that job does not manage resources of this skill, and this backstop does not manage its.
</keep-the-backstop-running>

<create-on-the-owners-go>
Create a lab resource on the owner's go for it; never create one on inference. The owner's standing ruling for the estafette work: lab use is free, with the resource management in this skill.

Choose a server type and location from the live API, where `hcloud server-type describe <type> -o json` lists each location with `available`; never choose from Hetzner's web pages.

Use the type, location, and image approved for this server (default cx23, nbg1, ubuntu-24.04); never substitute another.
</create-on-the-owners-go>

<give-each-lab-resource-its-deadline>
Set each lab resource's deadline 24 hours after its creation; never create one without a deadline.

Set the deadline at creation with `--label owner=estafette --label expires=<unix seconds>`, where `date -v+24H +%s` prints the unix seconds 24 hours ahead; never let a lab resource run unlabeled.

Label a lab resource whose creation took no labels at once with `python3 -I lab-backstop.py label <kind> <id>`, kind being `server`, `primary-ip`, `firewall`, or `ssh-key`; never defer its label.

Know that the backstop keeps an expired server while its type is not available in its location (the owner's scarce-type ruling of 2026-10-21); delete such a server yourself at the end of the stretch.
</give-each-lab-resource-its-deadline>

<create-the-server-from-this-skills-setup>
Upload the SSH key when `hcloud ssh-key list` lacks `estafette-lab`, with `hcloud ssh-key create --name estafette-lab --public-key-from-file ~/.ssh/estafette_lab.pub` and its deadline labels; never upload another key. The key pair `~/.ssh/estafette_lab` must exist; when it does not, ask the owner and never generate one unasked.

Create the firewall from this skill's `firewall-rules.json` with `hcloud firewall create --name <name> --rules-file firewall-rules.json` and its deadline labels; never open more than its four rules, inbound TCP 22, 80, and 443 and all UDP.

Write the server's setup file with `python3 -I fill-setup.py <output path>`, which fills the public half of `~/.ssh/estafette_lab` and the channel of the repository's `rust-toolchain.toml` into this skill's `cloud-init.template.yaml`; never edit the template for one server.

Keep in the template, for every server, the user `lab` without sudo, root by key only, mosh for the owner's terminal, uv and just in `/usr/local/bin`, and for `lab` the Rust toolchain the repository pins, with the default profile plus `rust-src` and `rust-analyzer`, cargo-binstall, Claude Code, the plugins this skill's `plugins.sh` installs, and the working directory `/home/lab/work` holding this skill's `lab-charter.md` as `CLAUDE.md` and a git repository; never drop one of them.

Create the server with `hcloud server create --name <name> --type <type> --image <image> --location <location> --ssh-key estafette-lab --firewall <firewall> --user-data-from-file <setup file> --label owner=estafette --label expires=<unix seconds>`; never create it without the setup file.

Label the server's two primary IP addresses at once, finding them with `hcloud primary-ip list`; never leave them unlabeled.
</create-the-server-from-this-skills-setup>

<bound-each-waiting-step>
Set a one-shot wake by the `check-back` skill at the latest moment each waiting step should end, the setup's run, an install, or a login; never leave a waiting step unbounded.

Reach the server with `just remote::ssh` (root) or `just remote::ssh lab <command>`, or `just remote::mosh` for a terminal, all of which read the address from the live API; never log in with a password.

Take the setup as finished when `cloud-init status --long` reads `done` with no errors and `/var/lib/lab-setup-done` exists; never take it as finished from the server's running state.

Check the tools after the setup with `uv --version` and `just --version`, and as `lab` with `rustc --version` (it must equal the channel in `rust-toolchain.toml`), `cargo clippy --version`, `rust-analyzer --version`, `cargo binstall -V`, `claude --version`, and `claude plugin list`; never report a tool installed without its printed version.

Take a new server at a reused address as a new host: run `just remote::forget-host` first.
</bound-each-waiting-step>

<extend-a-deadline-while-work-runs>
Extend a deadline on the owner's word, or while lab work on that lab resource is running, with `python3 -I lab-backstop.py extend <kind> <id> <hours>`; never extend one for an idle lab resource.
</extend-a-deadline-while-work-runs>

<tear-down-in-order>
Delete the server first, then its firewall, then any primary IP address its deletion left, then the SSH key once no server needs it; never delete a firewall a server still holds.

Copy results off the server before deleting it; the server's `~/work/results/` is lost with it.

Confirm a teardown with `just remote::list`; never report a lab resource deleted without a list that lacks it.
</tear-down-in-order>

<record-every-change>
Write each creation, extension, and deletion of a lab resource into the journal through the `event-capture` skill; never leave one unwritten.
</record-every-change>
