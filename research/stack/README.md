# Stack research: the spec tested against the stack

From 2026-10-15 to 2026-10-21 the spec was tested against the libraries it builds on (iroh 1.3.0, iroh-gossip 0.101.0, iroh-relay 1.3.0, rmcp) by running code, and revised where the code disagreed. The owner's goal: "test our theoretical spec against technological reality and revise anything that needs it." The owner closed the spec work on 2026-10-21 at 23:10: "this is as far as i'm willing to go in spec itself."

- The applied spec: [spec/blueprint.md](../../spec/blueprint.md), with every ruled revision (S1-S27), relay HTTPS (D51), and the host rule from E24.
- The pre-research blueprint: commit `03757cc` (2026-10-15), readable as `git show 03757cc:spec/blueprint.md`. The owner's original, before the deliberation edits of 2026-10-15, lies outside the repository at `~/Downloads/blueprint.md` ([journal](../../docs/orchestration_log/history/2026-10-15/events.md), 2026-10-15 16:08 and 23:22).
- How a finding becomes a spec change: [mechanic.md](mechanic.md); the plan the owner adopted on 2026-10-16: [plan.md](plan.md).

## Where to start

- [showcase.md](showcase.md): the owner's view, read cold, with two diagrams.
- [ledger.md](ledger.md): item by item, every collision (C1-C19), every revision (S1-S27), their status, and the evidence for each.
- [spec-apply-report.md](spec-apply-report.md), with [spec-review.md](spec-review.md) and [spec-review-2.md](spec-review-2.md): how the revisions entered the spec, the judgement calls made, and the two reviews that checked the result.

## Experiments

Each report sits in [experiments/](experiments/); Linux reruns sit in [lab/linux/](lab/linux/). Code lives in the estafette repository, on the branch named, at the commit the report's header names (the last one where it names several). Items are ledger ids: C for collisions, S for revisions, A for rows of "Spec assumptions, by status", O for the numbered items of "OPEN".

| Id | Question | Answer | Report | Linux rerun | Code | Settles |
|---|---|---|---|---|---|---|
| E1 | Does iroh's API carry what the spec assumes? | Yes, with gaps: dial by id needs an address source; an oversize gossip send returns Ok and kills the link | [e1-api](experiments/e1-api.md) | | spike/e1-api d18ff81 | A1-A7, A9, A14, A28, C6 |
| E2 | Does the largest Presence fit a gossip frame? | No: 4368 B against a 4096 B default frame | [e2-size](experiments/e2-size.md) | | spike/e2-size 1117a66 | A8, S1, S9 |
| E3 | How fast is a crashed peer noticed? | 33 s by default, 13 s at a 10 s idle timeout; a clean stop in 3 ms | [e3-kill](experiments/e3-kill.md) | | spike/e3-kill 1bf7209 | A11, A13, A18, S2 |
| E4 | Does the Presence flow work as written? | Only once the topic has formed; Departure needs a wait before exit | [e4-presence](experiments/e4-presence.md) | | spike/e4-presence 6bd29ac | O3, C5, C9, C10, S12, S15, S17 |
| E5 | Is a same-key restart safe? | Yes with an exclusive lock; two live holders starve one | [e5-restart](experiments/e5-restart.md) | | spike/e5-restart f82f6ae | O1, C8, C11, S14 |
| E6 | How does a node find a peer's address from its id? | Gossip's lookup learns only neighbors; mDNS, n0 and a self-hosted DNS server each work at a cost | [e6-lookup](experiments/e6-lookup.md) | | spike/e6-lookup ebefd1f | O2, C1, C7 |
| E7 | Does the topic id alone admit an outsider? | Yes: it joins in about 6 ms and reads every Presence | [e7-admission](experiments/e7-admission.md) | | spike/e7-admission e09a1ed | O4, C12 |
| E8 | What happens when a relay or a bootstrap node dies? | Relay death stalls gossip mesh-wide; bootstrap failover takes 8-18 ms | [e8-failover](experiments/e8-failover.md) | | spike/e8-failover 619b57f | C1, C7, C13, S20 |
| E9 | Does a secret-proof gate in iroh's hook keep outsiders out? | Yes, both ways, at 1-2 ms per connection | [e9-gate](experiments/e9-gate.md) | | spike/e9-gate 76b5219 | C12, S16 |
| E10 | Does iroh-gossip main heal the relay-death stall? | Alone, at 62-145 s; 0.101.0 never heals in 180 s | [e10-gossip-main](experiments/e10-gossip-main.md) | [linux-e10](lab/linux/linux-e10.md) | spike/e10-gossip-main a7e39b6 | C13 |
| E11 | Does rmcp carry the MCP surface? | Yes; SIGTERM hangs on a pending stdin read, and a background shutdown exits in 1 ms | [e11-mcp](experiments/e11-mcp.md) | [linux-e11](lab/linux/linux-e11.md) | spike/e11-mcp e392d3b | O5, C14, C15, S23, A28 |
| E12 | Does our own rejoin heal the stall on the 0.101.0 pin? | No (0 of 62 pairs); relay removal re-homes in 0.7 s | [e12-rejoin](experiments/e12-rejoin.md) | [linux-e12](lab/linux/linux-e12.md) | spike/e12-rejoin d4b4ca0 | C13, S24 |
| E13 | Does our own stdin reader send Departure at once? | Yes, in under 1 ms; exit unchanged | [e13-stdin](experiments/e13-stdin.md) | [linux-e13](lab/linux/linux-e13.md) | spike/e13-stdin 5b58d10 | C14, S25 |
| E14 | Why do fresh dials fail after a relay host dies? | One dead relay's full queue blocks every relay send of the endpoint | [e14-dials](experiments/e14-dials.md) | [linux-e14](lab/linux/linux-e14.md), [linux-e14w](lab/linux/linux-e14w.md), [linux-dial-after-60s](lab/linux/linux-dial-after-60s.md) | spike/e14-dials b849d40 | C16 |
| E14 bug report | Can the iroh bug be shown in one file, for upstream? | Yes: 20 of 20 failures on 1.3.0 and on main; two candidate fixes pass | [e14-bug-report](experiments/e14-bug-report.md) | [linux-e14-repro](lab/linux/linux-e14-repro.md) | spike/e14-bug-report 983118f | C16 |
| E15 | Does a rebind of the endpoint heal the stall? | Every stalled pair, but 5 of 13 runs relapse | [e15-workaround](experiments/e15-workaround.md) | [linux-e15](lab/linux/linux-e15.md) | spike/e15-workaround 37b0a1a | C16, C17, S26 |
| E16 | Does a gossip re-dial fix help? | No: alone it heals nothing, and with the rejoin it feeds the block | [e16-redial](experiments/e16-redial.md) | | spike/e16-redial c86fdbf | C17, S26 |
| E17 | Why does the rebind relapse? | Cause 1: the fresh endpoint re-sends on the dead relay's path; C2 removes it | [e17-relapse](experiments/e17-relapse.md) | [linux-e17](lab/linux/linux-e17.md) | spike/e17-relapse 83d75bd | S26 |
| E18 | Why does a rebound member wait 60 s for a neighbor? | Cause 2: a stale pending Neighbor request at the bootstrap node | [e18-attach](experiments/e18-attach.md) | [linux-e18](lab/linux/linux-e18.md) | spike/e18-attach 85ef8a2 | C18, S26 |
| E19 | Do the parts together stop relapses? | Yes: 0 of 8 runs relapse on each machine | [e19-combined](experiments/e19-combined.md) | [linux-e19](lab/linux/linux-e19.md) | spike/e19-combined b1357d1 | S26 |
| E20 | Why does the combined fix heal 14 s later than the rebind alone? | A member with no member neighbor hears others 11-16 s late; MJOIN removes it | [e20-delay](experiments/e20-delay.md) | [linux-e20](lab/linux/linux-e20.md) | spike/e20-delay 9abb991 | S26 |
| E21 | Can a host announce itself to a running network? | Yes: every member learns it in 24-114 ms, with no restart | [e21-hosts](experiments/e21-hosts.md) | [linux-e21](lab/linux/linux-e21.md) | spike/e21-hosts bb66277 | S21, C19 |
| E22 | Do two machines connect through a real NAT, and is relay HTTPS cheap? | Yes, 20 of 20 both ways with a direct path; HTTPS costs 87 ms to come online | [e22-two-machines](experiments/e22-two-machines.md) | | spike/e22-two-machines 966d653 | O6, A3, relay TLS (D51) |
| E23 | Does the lock hold on Linux and through a power cut; what triggers S13? | Lock yes; the key file needs fsync; S13 needs a debounce | [e23-lock](experiments/e23-lock.md) | | spike/e23-lock 006cbfe | O1, O3, S13, S14 |
| E24 | Does the relay-death workaround, as the spec states it, work across machines? | Not with hosts that rebind: they loop; members alone heal in 32.6-33.1 s | [e24-cross](experiments/e24-cross.md) | | spike/e24-cross 3f68e41 | S21, S26 (D46) |
| E25 | Do hosts without the rebind rule recover on their own? | Over IP transports nothing stalls; relay-only, the surviving host is blocked in 10 of 21 runs | [e25-hosts-norule](experiments/e25-hosts-norule.md) | | spike/e24-cross 2c81e82 | S26, known limitations |

The E25 report names no commit ("commit stated in the reply"); `2c81e82` comes from the journal's receipt of 2026-10-21 20:36.

## Other material

- [verify/](verify/): checks of upstream records. v1a (iroh's address cache), v1b (gossip churn fixes), and v3 (irpc, run) are evidence; v2 (the gossip gate, a source trace) is a lead.
- Leads, never cited as findings (ledger "Leads"): [surveys/](surveys/), [specimen/](specimen/), [solutions/](solutions/), [state/](state/), [ground/](ground/), [index/index.md](index/index.md). The known errors in them are listed in [plan.md](plan.md) under "Orchestrator corrections".
- [lab/](lab/): the Linux lab. [lab/server.md](lab/server.md) holds the server's state and backstop, [lab/hetzner.md](lab/hetzner.md) the provider recon, [lab/briefs/](lab/briefs/) the brief behind each Linux rerun, and [lab/linux/](lab/linux/) the reruns. [lab/claude-code-remote.md](lab/claude-code-remote.md), [lab/socket-bridge.md](lab/socket-bridge.md), and [lab/post.py](lab/post.py) cover linking a Claude Code session on the server.
- The bug-report package for iroh: [experiments/e14-bug-report/](experiments/e14-bug-report/) holds the report ([README.md](experiments/e14-bug-report/README.md)), the issue text ([ISSUE.md](experiments/e14-bug-report/ISSUE.md)), the reproductions ([repro/](experiments/e14-bug-report/repro/)), [run.sh](experiments/e14-bug-report/run.sh), and the raw Mac and Linux [results/](experiments/e14-bug-report/results/); [experiments/e14-bug-report.tar.gz](experiments/e14-bug-report.tar.gz) packs it for posting. The owner posts it.
- [parameters.md](parameters.md): the agreement parameters G1-G5 the owner approved.
- [staged-revisions.md](staged-revisions.md): the intended spec text of each revision, now applied to the spec.
- Skills: [.claude/skills/run-hetzner-lab](../../.claude/skills/run-hetzner-lab/SKILL.md) creates and deletes a throwaway lab server with a backstop; [.claude/skills/link-remote-peer](../../.claude/skills/link-remote-peer/SKILL.md) links a session on another machine, as [lab/remote-peer-link.md](lab/remote-peer-link.md) describes.
- The journal, with every ruling quoted: [docs/orchestration_log/history/2026-10-15/events.md](../../docs/orchestration_log/history/2026-10-15/events.md).

## Known limitations the owner accepted

The owner accepted two relay-only gaps when closing the spec work (journal, 2026-10-21 23:10); the other two are gaps in what was tested.

1. On relay-only paths, the surviving host can itself be blocked by the iroh relay bug, and the spec gives it no recovery rule. E25 saw it in 10 of 21 relay-only runs; each lasted 62.6-87.3 s and then cleared by itself, after 60-90 s without traffic between the pairs that went through that host ([e25-hosts-norule](experiments/e25-hosts-norule.md), test 2).
2. The deaf rule cannot fire for a member that keeps hearing host records while it cannot reach its peers. One E25 run (`e25r-sp-3`) showed it: 8 of 12 pairs stalled 71 s while every member received a host record every 5 s ([e25-hosts-norule](experiments/e25-hosts-norule.md); the risk was first named in [spec-review.md](spec-review.md)).
3. A strict network, one with no direct path between members, was simulated by turning off IP transports and never met. No symmetric NAT, CGNAT, or UDP-blocked network was tested; E22's home NAT preserved ports and was traversable ([e22-two-machines](experiments/e22-two-machines.md), [e24-cross](experiments/e24-cross.md), [e25-hosts-norule](experiments/e25-hosts-norule.md)).
4. QUIC address discovery in the embedded relay library is unconfirmed. E22 and E24 ran the iroh-relay binary from a TOML file; the spec embeds the library and assumes it behaves as the binary's `enable_quic_addr_discovery = true` ([spec-apply-report.md](spec-apply-report.md), E22 notes).

Over IP transports, the condition a product runs in on a traversable NAT, no run stalled after a host's death: 0 of 16 runs in E25 and 0 of 4 in E24's control arm.

The owner ran two tests by hand on 2026-10-21, with the Mac's members talking to members on the lab servers (journal receipts, 22:55 and 23:02; logs in the estafette-e24 worktree):

- Sleep and wake: the Mac slept 247.5 s. On waking, each Mac member rebound at once, and all 10 pairs flowed again within 3.7 s, with no failed join.
- Wi-Fi to a phone hotspot: the public address changed and the network was down about 4.8 s. No member rebound; peers heard the Mac members 0.3-0.4 s after the new address, and all 10 pairs resumed within 0.9 s. At the window's end the remote side no longer heard the Mac, most likely because the owner switched back to home Wi-Fi; that cause is unconfirmed.
