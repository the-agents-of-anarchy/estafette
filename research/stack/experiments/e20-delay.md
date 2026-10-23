# E20 why the combined workaround heals about 14 s after W1 alone, and a member-side change (estafette)

Date 2026-10-21 (runs 06:40-07:40 UTC). Question: why does B1 (W1 + E17_PROBE + E18_JOIN_T_MS=5000 + E18_FIX) heal about 14 s later than W1 alone, and does a small member-side change remove the delay.

## Answer

- The delay is not relay homing. In every rebound endpoint (E19 B1 and the E20 reruns, 42 endpoints) the home relay was connected 401-404 ms after bind, late endpoints included. I did not run the iroh relay/net-report tracing you listed; the 402 ms figure rejects the hypothesis without it.
- The delay is a gossip-membership miss. All 9 E19 B1 endpoints whose first heartbeat came 10 s or more after REBOUND had no NeighborUp from another member in their first 5 s; all 12 others had one. Their only neighbor was B2, and heartbeats reached them over B2's lazy push 11.2-15.8 s after REBOUND. E20 B1 rerun: 5/21 late endpoints, same pattern (5/5 without a member neighbor).
- Two traced reasons for the missing member neighbor, 14 late endpoints in E19 B1 plus E20 B1: (A) 6: the join random walk ended at B2, which still holds the rebound member in its active view and only renews it (`on_forward_join`, `hyparview.rs:430-434`); (B) 8: members' dials to each other failed at once with "No addressing information available" (empty address-book entry for the peer). Classification by co-occurring log lines, not by an intervention on each.
- Change `E20_MJOIN_MS=1500` (13 lines in `main.rs`): a rebound member with no member neighbor 1.5 s after JOINED calls `join_peers(other members)`, repeated every 3 s. B3 (B1 + change): 0/24 late endpoints against 5/21 in B1 (Fisher p = 0.017); first heartbeat after REBOUND 0.0-5.1 s in all 24. 0 second rebinds in 8 runs, 48/48 pairs flowing at 180 s, so no relapse regression seen.
- Not established: that the change makes the run-level heal faster. All-flowing-from, stalled runs, B1 n=7 vs B3 n=8: median 18.0 vs 18.1 s, mean 22.2 vs 18.8 s, exact permutation p = 0.34 (all runs counted: p = 0.92). B1 here was bimodal, 4 runs at 13.6-18.0 s and 3 at 30.3-31.6 s; the 3 slow runs are exactly the 3 runs with a late endpoint.

## Setup

- Repo `/Users/ryzhakar/pp/estafette`, worktree `/Users/ryzhakar/pp/estafette-e20-delay`, branch `spike/e20-delay` from `spike/e19-combined` (b1357d1). Commits `edf3c0d` (change, runs, analysis, `logs/e20`) and `9abb991` (classification scripts, `logs/e19/first_hb_B1_e20view.txt`); pushed. No `target/`, no `vendor/` committed.
- Build: `spikes/e18-attach-gossip` (stock iroh 1.3.0, vendored iroh-gossip 0.101.0 with the E18 patch): `sh vendor.sh`, `cargo build --features a`, copied to `target/bin/e10-a`, sha256 b7b6705a62951ba0... (one binary for both arms; the arm is chosen by env). Rust 1.99.1, debug, same macOS host as E19 (arm64, loopback, plain-HTTP relays), SIGKILL of R1+B1 (scenario 1), probers on, 1 s heartbeat, QUIC idle 10 s, 180 s window, `E12_TRACE=iroh_gossip=debug`. Host load average (1 min) at run starts 0.87-3.28, other users present.
- Tags in `run-cell.sh`: `y1w` (B1, unchanged from E19) and new `y3w` = `y1w` + `E20_MJOIN_MS=1500`; every other tag unsets `E20_MJOIN_MS`.
- Commands, from `spikes/e10-gossip-main`, `E18_LOGDIR=logs/e20`, `sh e18-batch.sh <scn> <tag> <run0> <n>`, foreground, 2 runs per call:
  - control first: `0 y3w 2030 1` (0 REBIND, 0 MJOIN, 3 JOINED), later `0 y3w 2031 2` (same)
  - kill runs, alternating pairs, baseline first in each round: `1 y1w 2000 2`, `1 y3w 2010 2`, `1 y1w 2002 2`, `1 y3w 2012 2`, `1 y1w 2004 2`, `1 y3w 2014 2`, `1 y1w 2006 2`, `1 y3w 2016 2`.
  - `pgrep` printed "no stray" after every run.
- Analysis, `logs/e20` only: `analyze_e20_table.py` (`table.md`), `analyze_e20_stats.py` (`stats.txt`), `analyze_e20_delta.py` (`delta.txt`), `analyze_e20_who.py` (`first_hb_B1.txt`, `first_hb_B3.txt`). Helpers `analyze_e20_dialfail.py`, `analyze_e20_merge.py`, `analyze_e20_online.py`.

## What the E19 logs show (B1, `logs/e19/a-1y1w-1910..1917`)

First heartbeat after each endpoint's REBOUND (`analyze_e19.py`, "hb+"): 21 first-generation endpoints, 9 at 11.2-15.1 s (1910 M1, 1912 M1, 1913 M1/M2/M3, 1914 M1, 1915 M1, 1916 M1, 1917 M3), 12 at 0.0-4.1 s. Run 1910 M1 in detail (times after REBOUND at 16.2 s after kill):
- Relay: `ONLINE ms_after_bind=404`, `STATUS relay=R2 connected=true`; address announce with R2 at +0.4 s. The member dials B1 (dead) and B2 at +0.0; B2 accepts at +0.006 s; JOINED +0.008 s.
- Neighbors: B2 handles Join (its active view still holds M1, M2, M3) and forwards ForwardJoin to M2 and M3. M2 and M3 hold only [B2, other member] (2 peers, ttl not expired), so they pass it on; both walks come back to B2, which already has M1 active and only renews (`send_neighbor ... fresh=false`, 00.4139). No member ever sends M1 a Neighbor. M1 gets no NeighborUp from M2 or M3.
- Heartbeats: B2 prunes M1's eager link at +1.0 s; M2 and M3 exchange heartbeats directly. M1 sends IHave to B2 every second and receives no heartbeat IHave from B2 between +1.2 s and +11.1 s. B2 grafts M2 at +11.1 s (after IHave from M2 and M3 at +11.0 s), sends M1 an IHave at +11.1 s, M1 grafts at +11.2 s, and the first heartbeat (M2's, via B2) arrives at +11.2 s. The dial to B1 times out at +10.01 s in every endpoint.
- Where heartbeats come from in the late endpoints: B2 only. Type A endpoints rebound last or together with the last member (1910, 1912, 1915 M1; 1914 M1 with M3; 1917 M3 with M1/M2 15.1-15.2 s), so the other two already held [B2, member] views; type B endpoints are those whose Neighbor dials failed (below).
Counts (`analyze_e20_who.py logs/e19 'a-1y1w-19*'`, `logs/e19/first_hb_B1_e20view.txt`): late endpoints whose only Neighbor sender was B2 (walk ended at B2, `fresh=false` renew): 1910 M1, 1912 M1, 1914 M1, 1915 M1, 1917 M3 (5). Late endpoints where a member did send Neighbor but a dial failed with "No addressing information available" in the same run at +0.1-2.0 s (`analyze_e20_dialfail.py`): 1913 M1, M2, M3 and 1916 M1 (4).
Other arms, same measure (first heartbeat 10 s or later): B0 1/24 (1904 M2, the E18 pending-Neighbor case, 60.4 s), E19 B2 (no C2) 3/24 (1922, 1924, 1925, all M1, no member neighbor).

## Hypothesis and trace evidence

H (formed from the E19 logs before any new run): the late first heartbeat is a membership miss, not relay homing; a rebound member without a gossip neighbor among the other members hears heartbeats only through B2's lazy push.
- Relay homing: `ONLINE` 401-404 ms for all 21 E19 B1 and all 21 E20 B1 endpoints; late and not late alike. Rejected.
- Association (E19 B1, 21 endpoints): late iff no member NeighborUp within 5 s; 9/9 late without, 12/12 not late with.
- E20 B1 rerun (21 endpoints, `first_hb_B1.txt`): 5 late (2000 M1, 2005 M1, 2006 M1/M2/M3); all 5 without a member NeighborUp in 5 s. Type A: 2000 M1 (Neighbor senders only B2, `fresh=false`). Type B: 2005 M1 (M1's dial to M2 "No addressing information" at +0.12 s), 2006 M1/M2/M3 (dials at +0.21, +1.77, +1.97 s).
- Intervention: with `E20_MJOIN_MS=1500` (below) the rebound member dials the other members itself at +1.5 s, when their announced relay (R2, arrived at +0.04 s) is in the address book. B3: 0/24 late; first heartbeat 0.0-5.1 s; `MJOIN` fired 14 times in 7 of 8 runs, every call returned ok.
- Not shown: why the B2 lazy path takes 11.2-15.8 s. It starts after the B1 dial timeout (+10.0 s) but only 1.2-5.8 s later, not at +10.0. I read `plumtree.rs` (`on_ihave`, `on_send_graft_timer`, `on_prune`) and the B2 trace (B2 sends M1 one IHave at +1.2 s, then none until +11.1 s); I did not find the trigger.

## Change

`spikes/e10-gossip-main/src/main.rs`, +13 lines in `session`, before the `E8_REJOIN` block (plus +3 in `run-cell.sh`):
```rust
// E20 counter (`E20_MJOIN_MS`): after a rebind, a member with no neighbor among the other members T ms after JOINED calls join_peers(other members), again every 3 s until one is a neighbor.
if let (Some(t_ms), true) = (std::env::var("E20_MJOIN_MS").ok().and_then(|v| v.parse::<u64>().ok()), last_rebind.is_some()) {
    let (n, tx, nb) = (name.clone(), tx.clone(), nbrs.clone());
    let others: Vec<EndpointId> = (1..=3u8).filter(|t| *t != k).map(|t| key(10 + t).public()).collect();
    js.spawn(async move {
        sleep(Duration::from_millis(t_ms)).await;
        while !nb.lock().map_or(true, |g| others.iter().any(|o| g.contains(o))) {
            let r = timeout(Duration::from_secs(5), tx.join_peers(others.clone())).await;
            ev(&n, "MJOIN", format!("join_peers {}", if matches!(r, Ok(Ok(()))) { "ok" } else { "err/timeout" }));
            sleep(Duration::from_secs(3)).await;
        }
    });
}
```
It acts only after a rebind (`last_rebind.is_some()`), so initial start-up is unchanged. It touches neither the relay map, the lookup, nor the C2 probe.

## Results (n=8 kill runs per arm, `logs/e20`)

| Arm | runs | stalled | stalled pairs flowing at 180 s | first-gen endpoints | first heartbeat 10 s or later | runs with a late endpoint | runs with a second rebind | all stalled pairs flowing from, s min / med / max | per run, s |
|---|---|---|---|---|---|---|---|---|---|
| B1 (y1w) | 8 | 7 (2001 never stalled) | 42/42 | 21 | 5 | 3 (2000, 2005, 2006) | 0 | 13.6 / 18.0 / 31.6 (n=7) | 30.3, -, 17.7, 13.7, 18.0, 31.6, 30.3, 13.6 |
| B3 (y3w) | 8 | 8 | 48/48 | 24 | 0 | 0 | 0 | 14.5 / 18.1 / 28.0 (n=8) | 16.9, 18.2, 19.5, 14.5, 20.0, 28.0, 15.1, 17.9 |
| B3n no kill (y3w, scn 0) | 3 | 0 | - | 0 | - | - | 0 | - | 0 REBIND, 0 MJOIN |

Tests (exact, runs not pairs or endpoints):
- All-flowing-from, difference of means, stalled runs only (B1 n=7, B3 n=8): means 22.2 vs 18.8 s, p = 0.335. All runs (never-stalled B1 = 0 s): means 19.4 vs 18.8 s, p = 0.918.
- Post-hoc, defined after seeing the data: all-flowing-from minus the last first REBOUND (removes stall-onset jitter): B1 median 1.1, mean 6.7 s (14.8, 1.0, 1.1, 1.1, 14.1, 14.0, 1.0); B3 median 2.0, mean 1.7 s; p = 0.106.
- Runs with a late endpoint: 3/8 vs 0/8, Fisher p = 0.20 (stalled only 3/7 vs 0/8, p = 0.077). Endpoints (not independent: 3 per run): 5/21 vs 0/24, p = 0.017.
- Second rebinds 0/8 vs 0/8 (p = 1). 0/24 vs 0/21 endpoints rebound twice.
- B3's 28.0 s (2015) is a late stall onset (first REBIND 20.0 s; B1 and B3 otherwise 6.5-12.2 s), first heartbeats 1.9, 4.7, 2.2 s after REBOUND; not a delay after the rebind.
- B3's slower floor against B1's fast runs: B3 median first heartbeat after REBOUND 1.1 s vs B1 1.2 s; B3 has 4.7-5.1 s values (M2 in 2014, 2015, 2016, 2012 M3) where MJOIN had to act at +1.5 s and +4.5 s.

## Hypotheses and verdicts

- H1 (the late heartbeat comes from relay homing of the new endpoint): rejected, home relay connected 401-404 ms after bind in all 42 endpoints.
- H2 (a rebound member without a member neighbor gets heartbeats only via B2, late): supported; association 21/21 (E19) and 21/21 (E20 B1), and removed by the change (0/24).
- H3 (type A: the join walk dead-ends at B2): supported for 6 endpoints by the `fresh=false` renew at B2 and no Neighbor from a member; source `hyparview.rs:430-434`.
- H4 (type B: dials fail on empty address-book entries): supported as co-occurrence for 8 endpoints; not shown which write emptied the entry (C2 scrub, or the rebound member's own first announce with `relays=[]` received at +0.01-0.04 s, which also occurs in B0 and B2).
- H5 (the 11-16 s is the B1 dial timeout of 10 s): not established; delays are 1.2-5.8 s after it.
- H6 (C2 causes the late endpoints): not established. Pooled B1 14/42 against E19 B2 3/24 (Fisher p = 0.082), against B0 1/24 (p = 0.006). B2 differs from B1 only by C2, but its n is small and it has its own 3 late endpoints.
- H7 (`E20_MJOIN_MS` removes the late endpoints without bringing relapses back): supported for the late endpoints (0/24 vs 5/21); no relapse in 8 runs, which fits a relapse rate up to about 31% (the same bound as E19's 0/8).
- H8 (the change shortens the run-level first heal): not established, p = 0.34 (0.106 post hoc).

## What could make this wrong

- Types A and B are classified from co-occurring log lines (a Neighbor sender list, a "No addressing" WARN in the same run), not from a per-endpoint intervention. Type B's link between the failed dial and the missing Neighbor is inferred.
- n=8 per arm; B1 had one run that never stalled (n=7 in the all-flowing-from test). Arms alternate in pairs, baseline first in every round; not randomised. B1's late-endpoint rate differs between E19 (9/21) and E20 (5/21), p = 0.33; the all-flowing-from effect of the change depends on that rate.
- The change is gated on `last_rebind`; the no-kill controls do not exercise it (3 runs, 0 REBIND, 0 MJOIN), so they show only that the build and harness run, not that MJOIN is harmless at start-up.
- Late endpoints need a tiny overlay: three members and one live bootstrap give active views of 2-3, so a walk returns to B2 almost surely. A larger cluster would end walks at members; the late-endpoint rate in a product may differ (not tested).
- B2 stayed alive and never noticed M1's death in these runs (its active view held M1 at the Join); an estafette bootstrap with different failure detection may not.
- `join_peers` needs the other members' ids; the harness knows them (`key(10+t)`); a product needs a member list.
- I did not find why B2's lazy push takes 11-16 s, nor which of C2's scrub and the empty first announce caused the type B entries.
- Debug build, tracing on (`iroh_gossip=debug`), loopback, plain HTTP, SIGKILL, one macOS host with other users (load 0.87-3.28), scenario 1 only.
- The iroh relay/net-report debug tracing you asked for was not run; ONLINE timing and the gossip trace were enough to reject and explain, and the 402 ms measure comes from `Endpoint::online()`, not from net-report lines.

## Exact commands to rerun on a Linux host

Branch arrives as a git bundle on top of `spike/e19-combined`. In the estafette checkout (already has `spike/e19-combined`):
```
git fetch /path/to/e20.bundle spike/e20-delay:spike/e20-delay
git worktree add ../estafette-e20-delay spike/e20-delay
cd ../estafette-e20-delay/spikes/e18-attach-gossip
sh vendor.sh
cargo build --features a
mkdir -p target/bin && cp target/debug/e18-attach-gossip target/bin/e10-a
cd ../e10-gossip-main
export E18_LOGDIR=logs/e20-linux
unset E17_FRESH_LOOKUP E17_PROBE E18_JOIN_T_MS E18_FIX E20_MJOIN_MS
```
Runs, foreground, about 185 s each, at most 3 per call; `pgrep -fl "e10-a|e18-attach"` must print nothing before each; run numbers 2900+ so `logs/summary` names do not clash:
```
sh e18-batch.sh 0 y3w 2930 1             # control first: expect 0 REBIND, 0 MJOIN
sh e18-batch.sh 1 y1w 2900 2             # B1
sh e18-batch.sh 1 y3w 2910 2             # B1 + change
sh e18-batch.sh 1 y1w 2902 2
sh e18-batch.sh 1 y3w 2912 2
sh e18-batch.sh 1 y1w 2904 2
sh e18-batch.sh 1 y3w 2914 2
sh e18-batch.sh 1 y1w 2906 2
sh e18-batch.sh 1 y3w 2916 2
sh e18-batch.sh 0 y3w 2931 2             # remaining no-kill controls
```
Analysis, own logs only (the scripts match `a-1y1w-N`, `a-1y3w-N`, `a-0y3w-N` for any N, so a fresh `E18_LOGDIR` keeps them off the macOS runs):
```
python3 -I analyze_e20_table.py logs/e20-linux
python3 -I analyze_e20_stats.py logs/e20-linux
python3 -I analyze_e20_delta.py logs/e20-linux
python3 -I analyze_e20_who.py logs/e20-linux 'a-1y1w-*'     # first heartbeat, Neighbor senders, LATE flag
python3 -I analyze_e20_who.py logs/e20-linux 'a-1y3w-*'
python3 -I analyze_e20_online.py logs/e20-linux 'a-1y1w-*'  # ms from bind to home relay connected
```
