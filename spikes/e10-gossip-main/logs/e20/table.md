| Arm | runs | runs stalled | stalled pairs | flowing at 180 s | rebound endpoints (all gens) | late joins (all gens) | first-gen endpoints | first-gen late joins | JOIN_FAIL ends | join wait of late joins s min/med/max | rebinds per member | all stalled pairs flowing from s min/med/max | per run: all flowing from s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B3n no kill, B1 + E20_MJOIN_MS=1500 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | - | 0-0 | - | -, -, - |
| B1 kill, W1 + E17_PROBE + E18_JOIN_T_MS=5000 + E18_FIX | 8 | 7 | 42 | 42 | 21 | 0 | 21 | 0 | 0 | - | 0-1 | 13.6 / 18.0 / 31.6 (n=7) | 30.3, -, 17.7, 13.7, 18.0, 31.6, 30.3, 13.6 |
| B3 kill, B1 + E20_MJOIN_MS=1500 | 8 | 8 | 48 | 48 | 24 | 0 | 24 | 0 | 0 | - | 1-1 | 14.5 / 18.1 / 28.0 (n=8) | 16.9, 18.2, 19.5, 14.5, 20.0, 28.0, 15.1, 17.9 |
