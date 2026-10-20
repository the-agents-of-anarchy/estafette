#!/bin/sh
# E10 cell runner. Foreground only; the caller caps the wait.
# usage: run-cell.sh <a|b> <scn> <tag> <runs> [run0]
#   scn 1 = kill R1+B1 (one process); scn 7 = kill B1 only (R1 and B1 in separate processes)
#   tag "np"  = no probers (E8_NOPROBE=1), 88 s window
#   tag "npw" = no probers, 180 s window (supplementary)
#   tag "w"   = probers on, 180 s window (supplementary)
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
  npw)
    export E8_NOPROBE=1
    export E8_TAG=npw
    export E10_WINDOW_MS=180000
    ;;
  w)
    export E8_TAG=w
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
  "$dir/target/bin/e10-$variant" ctl "$scn" "$runs"
  echo "LOAD after: $(uptime)"
  echo "CELL end=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} 2>&1 | tee "$out"
