#!/bin/sh
# E10 cell runner. Foreground only; the caller caps the wait.
# usage: run-cell.sh <a|b> <scn> <tag> <runs> [run0]
#   scn 1 = kill R1+B1 (one process); scn 7 = kill B1 only (R1 and B1 in separate processes)
#   tag "np"  = no probers (E8_NOPROBE=1), 88 s window
#   tag "npw" = no probers, 180 s window (supplementary)
#   tag "w"   = probers on, 180 s window (supplementary)
#   E12 tags: l12 = L1+L2, probers on; l12np = L1+L2, probers off; l1np = L1 only, probers off; l2np = L2 only, probers off (88 s)
#   E15 tags (probers on, 180 s window): l12w = L1+L2; w1w = W1 (T 3000); w1w10 = W1 T 10000; w1l1w = W1+L1.
#     p-prefixed tags (pw, pl12w, pc) are the same as w, l12w, w for the diagnostic binary given in E15_BIN; E14_FIX=drain is set for pw and pl12w, unset for pc (stock behavior control).
#   E16 tags (probers on, 180 s window; binary from E15_BIN = spikes/e16-gossip-fix): f0w = E16_FIX unset (control); f1w = E16_FIX=1; f2w = E16_FIX=1 + L2; f3w = E16_FIX=1 + W1 (T 3000); f4w = E16_FIX=1 (use with scn 0).
#   E17 tags (180 s window): x1w = W1 (T 3000), probers on, as w1w, with the diagnostic binary; x1nw = same with probers off; x1cw = W1 + E17_FRESH_LOOKUP=1 with the diagnostic binary; c0w = W1, c1w = W1 + E17_FRESH_LOOKUP=1 (stock binary, E15_BIN unset). The diagnostic binary logs through E15_BIN = spikes/e17-relapse-gossip with E14_DIAG=1 E17_ITEMS=1 E17_ERR_DIR=<dir> E12_TRACE=....
#   E18 tags (180 s window; binary from E15_BIN = spikes/e18-attach-gossip via e18-run.sh): x1cw (above) is the E18 baseline; x2cw = x1cw + E18_JOIN_T_MS=5000 (member-side counter); x3cw = x1cw + E18_FIX=1 (vendored gossip drops a pending Neighbor on Join).
#   E19 tags (180 s window; binary from E15_BIN = spikes/e18-attach-gossip via e18-run.sh; E17_FRESH_LOOKUP must be unset): y0w = W1 alone (B0); y1w = W1 + E17_PROBE=1 + E18_JOIN_T_MS=5000 + E18_FIX=1 (B1, combined; scn 0 = its no-kill control); y2w = W1 + E18_JOIN_T_MS=5000 + E18_FIX=1 (B2, combined without C2).
#   any other tag (use "-") = probers on, 88 s window, as E8
set -u
variant=$1
scn=$2
tag=$3
runs=$4
run0=${5:-1}
dir=$(cd "$(dirname "$0")" && pwd)
export E8_PAYLOAD=1
export E10_RUN0="$run0"
export E8_TAG=
case "$tag" in
  np)
    export E8_NOPROBE=1
    export E8_TAG=np
    ;;
  l12) export E12_L1=1 E12_L2=1 E8_TAG=l12 ;;
  l12np) export E12_L1=1 E12_L2=1 E8_NOPROBE=1 E8_TAG=l12np ;;
  l1np) export E12_L1=1 E8_NOPROBE=1 E8_TAG=l1np ;;
  l2np) export E12_L2=1 E8_NOPROBE=1 E8_TAG=l2np ;;
  w|pw|pc) export E8_TAG=$tag E10_WINDOW_MS=180000 ;;
  l12w|pl12w) export E12_L1=1 E12_L2=1 E8_TAG=$tag E10_WINDOW_MS=180000 ;;
  w1w) export E15_W1=1 E8_TAG=w1w E10_WINDOW_MS=180000 ;;
  w1w10) export E15_W1=1 E15_W1_T_MS=10000 E8_TAG=w1w10 E10_WINDOW_MS=180000 ;;
  w1l1w) export E15_W1=1 E12_L1=1 E8_TAG=w1l1w E10_WINDOW_MS=180000 ;;
  f0w) unset E16_FIX; export E8_TAG=f0w E10_WINDOW_MS=180000 ;;
  f1w) export E16_FIX=1 E8_TAG=f1w E10_WINDOW_MS=180000 ;;
  f2w) export E16_FIX=1 E12_L2=1 E8_TAG=f2w E10_WINDOW_MS=180000 ;;
  f2cw) unset E16_FIX; export E12_L2=1 E8_TAG=f2cw E10_WINDOW_MS=180000 ;;
  f3w) export E16_FIX=1 E15_W1=1 E8_TAG=f3w E10_WINDOW_MS=180000 ;;
  f4w) export E16_FIX=1 E8_TAG=f4w E10_WINDOW_MS=180000 ;;
  x1w) export E15_W1=1 E8_TAG=x1w E10_WINDOW_MS=180000 ;;
  x1nw) export E15_W1=1 E8_NOPROBE=1 E8_TAG=x1nw E10_WINDOW_MS=180000 ;;
  x1cw) export E15_W1=1 E17_FRESH_LOOKUP=1 E8_TAG=x1cw E10_WINDOW_MS=180000 ;;
  x1pw) export E15_W1=1 E17_PROBE=1 E8_TAG=x1pw E10_WINDOW_MS=180000 ;;
  c0w) export E15_W1=1 E8_TAG=c0w E10_WINDOW_MS=180000 ;;
  c1w) export E15_W1=1 E17_FRESH_LOOKUP=1 E8_TAG=c1w E10_WINDOW_MS=180000 ;;
  x2cw) export E15_W1=1 E17_FRESH_LOOKUP=1 E18_JOIN_T_MS=5000 E8_TAG=x2cw E10_WINDOW_MS=180000 ;;
  x3cw) export E15_W1=1 E17_FRESH_LOOKUP=1 E18_FIX=1 E8_TAG=x3cw E10_WINDOW_MS=180000 ;;
  y0w) unset E17_FRESH_LOOKUP E17_PROBE E18_JOIN_T_MS E18_FIX; export E15_W1=1 E8_TAG=y0w E10_WINDOW_MS=180000 ;;
  y1w) unset E17_FRESH_LOOKUP; export E15_W1=1 E17_PROBE=1 E18_JOIN_T_MS=5000 E18_FIX=1 E8_TAG=y1w E10_WINDOW_MS=180000 ;;
  y2w) unset E17_FRESH_LOOKUP E17_PROBE; export E15_W1=1 E18_JOIN_T_MS=5000 E18_FIX=1 E8_TAG=y2w E10_WINDOW_MS=180000 ;;
  npw)
    export E8_NOPROBE=1
    export E8_TAG=npw
    export E10_WINDOW_MS=180000
    ;;
esac
if pgrep -f "e10-[ab] (relay|relayonly|boot|member|ctl)" >/dev/null; then
  echo "ABORT: stray e10 processes running"
  pgrep -fl "e10-[ab] (relay|relayonly|boot|member|ctl)"
  exit 2
fi
mkdir -p /tmp/e10 "$dir/logs/summary"
out="$dir/logs/summary/$variant-$scn$E8_TAG-r$run0.txt"
{
  echo "CELL variant=$variant scn=$scn tag=$E8_TAG runs=$runs run0=$run0 window_ms=${E10_WINDOW_MS:-88000} start=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOAD before: $(uptime)"
  "${E15_BIN:-$dir/target/bin/e10-$variant}" ctl "$scn" "$runs"
  echo "LOAD after: $(uptime)"
  echo "CELL end=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} 2>&1 | tee "$out"
