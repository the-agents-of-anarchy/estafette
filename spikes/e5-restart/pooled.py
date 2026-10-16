#!/usr/bin/env python3
"""Pooled min/median/max markdown rows over runs and peers (P1,P2) for restart scenarios."""
import sys, glob, statistics
sys.path.insert(0, __file__.rsplit("/", 1)[0])
from analyze import parse, analyze_restart

def row(label, vals, unit="ms", total=None):
    nums = [v for v in vals if isinstance(v, (int, float))]
    total = total if total is not None else len(vals)
    if not nums:
        return f"| {label} | none observed | | | 0/{total} |"
    f = lambda x: f"{x:.0f}" if abs(x) >= 100 else f"{x:.1f}"
    return f"| {label} | {f(min(nums))} | {f(statistics.median(nums))} | {f(max(nums))} | {len(nums)}/{total} |"

for pat in sys.argv[1:]:
    runs = [analyze_restart(parse(f)) for f in sorted(glob.glob(pat))]
    print(f"\n#### {pat}  (runs={len(runs)}; peer metrics pooled over P1,P2)\n")
    print("| Measure (ms after kill unless noted) | min | median | max | n |\n|---|---|---|---|---|")
    both = lambda k: [r[f"{p}_{k}"] for r in runs for p in ("P1", "P2")]
    print(row("K' lock acquired (waited, retry 10 ms)", [r["k_restart_lock_wait_ms"] for r in runs]))
    print(row("K' READY", [r["k_restart_ready_ms"] for r in runs]))
    print(row("NeighborDown(K) at peer: events/run", both("neighbor_down_K_count")))
    print(row("NeighborUp(K) at peer: events/run", both("neighbor_up_K_count")))
    print(row("first broadcast from K' received at peer", both("first_broadcast_from_new_K_ms")))
    print(row("first fresh direct request answered by K' (completion)", both("fresh_first_ok_new_complete_ms")))
    print(row("longest fresh request, started after kill (dur)", both("fresh_max_dur_ms")))
    print(row("fresh requests failed or timed out after kill: count/run", both("fresh_nonok_count")))
    print(row("fresh requests answered by dead K after kill: count/run", both("fresh_ok_old_pid_after_kill")))
    print(row("held-connection request: first timeout (5 s cap) completes", both("held_first_nonok_complete_ms")))
    print(row("held-connection request: first connection_lost error", both("held_first_err_ms")))
    print(row("held-connection requests answered by K' (count/run)", both("held_ok_new_count")))
    print(row("K' receives first gossip msg from a peer", [r["K_new_first_recv_peer_msg_ms"] for r in runs]))
    print(row("K' NeighborUp events seen by K' (count/run)", [r["k_neighbor_up_count"] for r in runs]))
