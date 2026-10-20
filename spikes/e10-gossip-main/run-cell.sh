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
