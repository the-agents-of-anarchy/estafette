#!/bin/sh
# usage: e17stock.sh <scn> <tag> <run0> <runs>
# stock build (spikes/e10-gossip-main/target/bin/e10-a), no diagnostics; archives raw log and trace text to logs/e17
set -u
W=$(cd "$(dirname "$0")/.." && pwd)
cd "$W/e10-gossip-main"
rm -rf /tmp/e17err /tmp/e12-trace-*.log
unset E15_BIN E14_DIAG E17_ITEMS E17_ERR_DIR E12_TRACE
./run-cell.sh a "$1" "$2" "$4" "$3" > /tmp/e17-last-cell.txt 2>&1
grep -E "^RUN|SETUP" /tmp/e17-last-cell.txt
mkdir -p logs/e17
i=$3
end=$(($3 + $4 - 1))
while [ "$i" -le "$end" ]; do
  n="a-$1$2-$i"
  gzip -c "/tmp/e10/$n.raw.log" > "logs/e17/$n.raw.log.gz"
  gzip -c "/tmp/e10/$n.trace.txt" > "logs/e17/$n.trace.txt.gz"
  i=$((i + 1))
done
pgrep -fl "e10-a|e17-relapse|e15-diag" || echo "no stray"
