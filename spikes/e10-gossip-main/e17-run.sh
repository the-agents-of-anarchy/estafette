#!/bin/sh
# usage: e17run.sh <scn> <tag> <run>
# one run, diagnostic build, full tracing, archived to logs/e17
set -u
W=$(cd "$(dirname "$0")/.." && pwd)
cd "$W/e10-gossip-main"
rm -rf /tmp/e17err /tmp/e12-trace-*.log
mkdir -p /tmp/e17err
export E15_BIN=$W/e17-relapse-gossip/target/bin/e10-a
export E14_DIAG=1 E17_ITEMS=1 E17_ERR_DIR=/tmp/e17err
export E12_TRACE="iroh::socket::transports::relay=debug,iroh_gossip=debug"
./run-cell.sh a "$1" "$2" 1 "$3" > /tmp/e17-last-cell.txt 2>&1
tail -5 /tmp/e17-last-cell.txt
sh e17-archive.sh "a-$1$2-$3"
pgrep -fl "e10-a|e17-relapse|e15-diag" || echo "no stray"
