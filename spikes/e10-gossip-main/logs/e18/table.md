| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | rebound endpoints (all gens) | late joins (all gens) | first-gen endpoints | first-gen late joins | JOIN_FAIL ends | join wait of late joins s min/med/max | rebinds per member | all stalled pairs flowing from s min/med/max | per run: all flowing from s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A0n no kill, no counter | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | - | 0-0 | - | - |
| A1n no kill, E18_JOIN_T_MS=5000 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | - | 0-0 | - | -, -, - |
| A0 kill, no counter (W1 + C1) | 8 | 8 | 46 | 46 | 31 | 7 | 22 | 7 | 6 | 57.4 / 60.0 / 60.0 (n=7) | 0-4 | 33.3 / 78.7 / 94.7 (n=8) | 76.1, 94.7, 33.3, 81.2, 38.8, 83.1, 81.8, 72.5 |
| A1 kill, E18_JOIN_T_MS=5000 (member-side) | 8 | 8 | 46 | 46 | 26 | 4 | 22 | 4 | 4 | 5.0 / 5.0 / 5.0 (n=4) | 0-2 | 30.1 / 38.0 / 58.2 (n=8) | 31.2, 39.4, 31.2, 50.4, 39.0, 58.2, 30.1, 37.0 |
| A2 kill, E18_FIX=1 (vendored gossip, Join drops pending Neighbor) | 8 | 8 | 48 | 48 | 24 | 0 | 24 | 0 | 0 | - | 1-1 | 21.1 / 31.5 / 33.9 (n=8) | 31.2, 33.9, 30.1, 33.2, 33.9, 21.1, 30.3, 31.9 |
