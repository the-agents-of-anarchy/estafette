#!/bin/sh
# usage: e21-run.sh <scn> <tag> <run>   (E21_*, E18_* switches come from run-cell.sh's tag)
# one run, E18 diagnostic build (stock iroh 1.3.0, vendored iroh-gossip with the E18 patch), gossip tracing, archived to logs/e21 (or $E21_LOGDIR)
# E21_TRACE overrides the tracing filter (default iroh_gossip=debug, as E19 and E20).
set -u
W=$(cd "$(dirname "$0")/.." && pwd)
cd "$W/e10-gossip-main"
rm -rf /tmp/e21err /tmp/e12-trace-*.log /tmp/e21known
mkdir -p /tmp/e21err /tmp/e21known
export E15_BIN=$W/e18-attach-gossip/target/bin/e10-a
export E17_ERR_DIR=/tmp/e21err
export E12_TRACE="${E21_TRACE:-iroh_gossip=debug}"
./run-cell.sh a "$1" "$2" 1 "$3" > /tmp/e21-last-cell.txt 2>&1
tail -5 /tmp/e21-last-cell.txt
sh e21-archive.sh "a-$1$2-$3"
pgrep -fl "e10-a|e18-attach" || echo "no stray"
