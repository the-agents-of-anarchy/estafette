# Capabilities

## Operator commands

`create network` writes the configuration file and the owner key file and prints no invitation. The configuration has no host at that point (spec:977) and an invitation carries one or more hosts (spec:967), so the invitation string spec:941 lists as its output cannot exist yet; one line on standard error says so. `show invitation` prints the invitation once `join network` has added a host (spec:980-982). — reported by spec-whole-W7, checked at core/src/cli/network.rs:328-332 and core/tests/cli_network.rs:299 by findings-ledger, 2026-11-10, at 0348b5d.
