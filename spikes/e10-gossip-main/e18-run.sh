#!/bin/sh
# usage: e18-run.sh <scn> <tag> <run>   (E18_* switches are taken from the caller's environment)
# one run, E18 diagnostic build (stock iroh 1.3.0, vendored iroh-gossip with state logging), gossip tracing, archived to logs/e18
set -u
W=$(cd "$(dirname "$0")/.." && pwd)
cd "$W/e10-gossip-main"
rm -rf /tmp/e18err /tmp/e12-trace-*.log
mkdir -p /tmp/e18err
export E15_BIN=$W/e18-attach-gossip/target/bin/e10-a
export E17_ERR_DIR=/tmp/e18err
export E12_TRACE="iroh_gossip=debug"
./run-cell.sh a "$1" "$2" 1 "$3" > /tmp/e18-last-cell.txt 2>&1
tail -5 /tmp/e18-last-cell.txt
sh e18-archive.sh "a-$1$2-$3"
pgrep -fl "e10-a|e18-attach" || echo "no stray"
