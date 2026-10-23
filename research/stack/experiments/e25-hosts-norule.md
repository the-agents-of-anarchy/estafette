# E25 hosts without the deaf rule, at n=8

Date 2026-10-21 (runs 16:00-17:31 UTC). Branch `spike/e24-cross` (same worktree `/Users/ryzhakar/pp/estafette-e24`), package `spikes/e24-cross/`; logs `logs/e24/e25r-*` (relay-only) and `e25i-*` (IP transports on); commit stated in the reply. Not committed in the research repo, as asked. Same binary, harness and analysis as E24 (`batch25.sh`, `summarize_e24.py`, new `h2blocked.py`); read E24 (`e24-cross.md`) for the setup.

## Answer

- Hosts without the rule and without a join timeout: the member trigger (T = 15 s) is not the problem, but the case of pilot2 is common, not rare: **host H2 itself was blocked after H1 died in 10 of the 21 relay-only runs without the rule (9 of 16 in this run, plus pilot2), and in 10 of those 10 it stayed blocked past 60 s (62.6-87.3 s)**. In 0 of 17 IP-transport runs. Hosts need their own recovery on the relay-only path.
- Relay-only, kill H1, n=8 per arm, interleaved pairs: spec trigger healed all 12 pairs in 32.4-80.3 s (median 49.8), the 3 s control in 11.9-90.9 s (median 74.6). They do not differ (permutation p = 0.50 on heal time, 0.16 on rebinds per run), because both are dominated by the blocked host: 3 of 8 spec runs and 6 of 8 control runs had it.
- Without the blocked host the spec trigger is as E24 measured: 5 runs, first rebind 26.9-27.2 s, 1 rebind per member, 0 join failures; 4 of the 5 healed at 32.4-33.1 s. The fifth (`e25r-sp-3`) is a new case: all members rebound once and stayed joined, all four kept receiving H2's records every 5 s to the end, and 8 of the 12 pairs still stalled 71 s (healed 74.5-75.2 s). No rule can fire there: the open risk of the review, seen once in 8 (spec arm only).
- IP transports on, kill H1, n=8 per arm: no stall in any run of either arm (0 of 16), no rebinds, no join failures. The rule never fires and never harms.
- First rebind per member (kill to rebind), relay-only: control 6.1-7.3 s (median 6.3, n=32 members); spec 19.1-27.2 s (median 27.0, n=32); IP-on: none rebound.

## Results

### Test 1: kill H1, hosts without the rule (n=8 per arm)

| Arm | n | runs with a stall | first rebind per member, s min / med / max | all 12 pairs flowing from, s per run | flowing at the end (12) | rebinds per run (4 members) | runs with a relapse | JOIN_FAIL per run | rebinds before the kill |
|---|---|---|---|---|---|---|---|---|---|
| relay-only control (3 s heartbeat) | 8 | 8 | 6.1 / 6.3 / 7.3 | 13.1, 76.6, 72.7, 67.0, 90.9, 83.5, 11.9, 85.6 | 12 in all | 4, 20, 18, 32, 44, 40, 4, 22 | 6 | 0, 16, 14, 28, 40, 36, 0, 18 | 0 |
| relay-only spec (15 s) | 8 | 8 | 19.1 / 27.0 / 27.2 | 80.3, 66.6, 75.2, 32.4, 32.6, 70.5, 32.6, 33.1 | 12 in all | 32, 24, 4, 4, 4, 24, 4, 4 | 3 | 28, 20, 0, 0, 0, 20, 0, 0 | 0 |
| IP-on control | 8 | 0 | - | - | 12 in all | 0 | 0 | 0 | 0 |
| IP-on spec | 8 | 0 | - | - | 12 in all | 0 | 0 | 0 | 0 |

Per-member first rebind per run (s), relay-only spec: 19.4/19.1/19.3/19.4 (run 1), 22.1/22.1/22.1/21.9 (2), 27.2/26.9/27.1/27.0 (3), 26.9/26.9/27.1/27.0 (4), 27.2/27.2/27.1/27.0 (5), 22.1/22.1/22.1/22.0 (6), 27.2/27.2/27.1/27.0 (7), 27.2/27.2/27.1/27.0 (8) (M1/M2/M3/M4). Control: 6.1-7.3 in all runs (7.0 for M1-M3 of run 1, 7.2-7.3 for M1, M3, M4 of run 7, 6.1-6.3 elsewhere); the script that prints the full list is in the logs' analysis (`analyze_e24.py` `kill_stats`, key `rebind`). The spec's earlier first rebinds (19-22 s) occur only in runs with the blocked host (runs 1, 2, 6); why the members stopped hearing records sooner there was not examined.

Tests (exact permutation on run means, never-healed counted as 120 s): relay-only control against spec, heal time p = 0.497, rebinds per run p = 0.155. IP-on: p = 1.0 (identical, all zero). Rebinds per member, relay-only: control mean 5.75 (max 11), spec 3.12 (max 8). Host rebinds after the kill: 0 in all 32 runs (the hosts have no rule).

Test 2 of E24 (records while blocked), relay-only: of 32 members per arm all had an outgoing stall; spec members received 1 / 3 / 3 records between the kill and their first rebind (min / median / max), 0 / 2 / 3 after their stall began, longest gap between records 15.0 / 15.2 / 17.6 s; control 1 / 1 / 2 records, longest gap 5.0 s. No member sat stalled past 60 s without rebinding, but see `e25r-sp-3` below, where pairs were stalled 71 s after the members' one rebind while records arrived.

### Test 2: host H2 blocked after H1's death

Criterion: a member's join fails (5 s timeout, `JOIN_FAIL`) after the kill. H1 is dead, so a failed join means H2 did not answer; "blocked until" is the log time of the last such failure (a lower bound on how long H2 did not answer; H2 has no event of its own for it). Hosts without the rule: 38 runs (E25 32, E24 `k-spec-nohd` 4, pilot1, pilot2): relay-only 21, IP-on 17.

| Case | blocked until (s after the kill) | members' rebinds each | all 12 pairs flowing from (s) | flowing at window end |
|---|---|---|---|---|
| pilot2 (spec) | 70.7 | 7, 7, 7, 7 | 74.5 | 12 |
| e25r-sp-1 | 76.1 | 8 x4 | 80.3 | 12 |
| e25r-sp-2 | 62.6 | 6 x4 | 66.6 | 12 |
| e25r-sp-6 | 62.6 | 6 x4 | 70.5 | 12 |
| e25r-hb-2 | 71.0 | 9, 9, 1, 1 | 76.6 | 12 |
| e25r-hb-3 | 62.9 | 8, 8, 1, 1 | 72.7 | 12 |
| e25r-hb-4 | 62.9 | 8 x4 | 67.0 | 12 |
| e25r-hb-5 | 87.3 | 11 x4 | 90.9 | 12 |
| e25r-hb-6 | 79.2 | 10 x4 | 83.5 | 12 |
| e25r-hb-8 | 79.1 | 10, 1, 10, 1 | 85.6 | 12 |

Count: 10 of 21 relay-only runs (47%): spec 3 of 9 (including pilot2), control 6 of 8, E24 nohd 0 of 4; 0 of 17 IP-on runs. **H2 stayed blocked past 60 s in all 10 cases (62.6-87.3 s).** A blocked H2 recovered by itself (all 12 pairs flowing from 66.6-90.9 s, every window ended healed): the first failed join comes 14-30 s after the kill, then each member's join fails every 8 s (5 s timeout plus rebind) for 33-73 s (last failure 62.6-87.3 s after the kill), after which the host answers again. The E17 re-block of about 57 s is a possible cause; I did not instrument H2, so the cause is not established. While it lasts, members rebind 6-11 times each (each rebind is one failed join), and in the control arm (3 s heartbeat) too: the control's relapse rate (6 of 8) is higher than the spec arm's (3 of 8) here, with n=8 each (Fisher p = 0.31, not significant).

Members' fate: all healed within the window, none were lost for good; the cost is 60-90 s of no traffic between the pairs that went through H2. M3 on B and M4 on C stayed out of the failure in 3 control runs (rebinds 1) while M1, M2 on A failed 8-9 times: the member-to-member paths via the other machines were intact for those members, and A's two members were stuck on H2. Not explained.

Run `e25r-sp-3` (a different case, no join failure): all four members rebound once at 27 s, joined H2 at 30.6 s, and received H2's record every 5 s until the window ended; 8 of 12 pairs nevertheless stalled 71 s (M1<-M2, M1<-M4, M2<-M1, M2<-M3, M3<-M2, M3<-M4, M4<-M1, M4<-M3; the other four healed at 31.2-31.7 s) and came back at 74.5-75.2 s. With the 15 s rule no member was deaf (records kept arriving), so nothing could fire. That is the "member blocked but still hears records" risk of the review, 1 of 8 spec runs here; 0 of the 4 `k-spec-nohd` runs of E24.

### What this decides

Hosts without the rule do not recover by themselves on the relay-only path: H2 is blocked 60-90 s in about half of the runs, in both trigger arms, so the member trigger cannot repair what the host's own relay block causes, and the spec trigger can be blind while a host record keeps arriving. Hosts need their own recovery (for example the deaf rule and rebind counting the join failure of members, which E24 showed loops when the host's join has a timeout and its only peer is dead, so the host's recovery must not use a join timeout through a dead host). I did not test a host recovery variant. Over IP transports none of this appears: no stall in 16 of 16 runs.

## What could make this wrong

- Relay-only is the condition where the stall exists; IP-on is the product condition on a traversable NAT, where nothing stalled at all (also in E24: the control 0 of 4, 0 of 8 here). Symmetric NAT, UDP-blocked and CGNAT networks were not tested, so the real mix is unknown.
- "H2 blocked" is inferred from member join failures, not from an H2 signal; the duration is a lower bound; a blocked H2 whose members did not try to join in the interval would not be counted. The count is for runs, not for H2 episodes.
- Pairs of arms are interleaved in pairs (control first) within a condition; the relay-only block ran before the IP-on block. n=8 each; permutation tests are post hoc. A 120 s window ends 28-57 s after the last blocked-host recovery, so a later relapse is not seen.
- The 57-63 s duration is the same as E17's re-block; I did not separate "H2's relay path blocked" from other causes. H2 runs on C with its relay on the same machine; members M4 on C and M3 on B share a machine with a host.
- Clock offsets about +-50 ms (see E24); the seconds-scale numbers here are insensitive to that.
- The hosts' join to H1: H2 joins through H1 with no timeout (the spec); if H1 is dead at H2's start it never joins, not tested here.
- Server C is new (same address 2.29.112.122, new key, new certificate), Linux binary copied from the E24 build (same sha256); one cloud region (Helsinki) for C, Nuremberg for B.

## Owner-assisted tests

See the reply and `spikes/e24-cross/owner-tests.sh`; the owner script now starts the hosts without the deaf rule.

## Lab resources

- Created 2026-10-21 15:53Z: firewall `estafette-e24-fw` (id 11855774), server `estafette-e24` (cpx22, hel1, IPv4 2.29.112.122; same address as E24's), primary IPs 156902531 (IPv4) and 156902532 (IPv6) labelled by `lab-control.py`; all `expires=1792684429` (2026-10-22 15:53Z). Backstop job loaded and logging before creation. Lines in `/tmp/e24/lab-changes.txt`.
- Not deleted: C stays up for the owner-assisted tests, until the lead says the owner is done; then the deletion follows the skill's order (server, firewall, primary IPs) and is recorded in the same file.
