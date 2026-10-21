#!/bin/sh
# Gzip one E18 run into logs/e18/: the raw log, the trace text, the members' stderr (E17_ERR_DIR=/tmp/e18err) and tracing files (/tmp/e12-trace-*.log).
# usage: [E18_LOGDIR=logs/e18-linux] e18-archive.sh <run-name> e.g. a-1x1cw-1801
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
out="$dir/${E18_LOGDIR:-logs/e18}"
mkdir -p "$out"
n=$1
gzip -c "/tmp/e10/$n.raw.log" > "$out/$n.raw.log.gz"
gzip -c "/tmp/e10/$n.trace.txt" > "$out/$n.trace.txt.gz"
for m in M1 M2 M3 B1 B2; do
  [ -f "/tmp/e18err/err-$m-member.log" ] && gzip -c "/tmp/e18err/err-$m-member.log" > "$out/$n.$m.err.gz"
  [ -f "/tmp/e12-trace-$m.log" ] && gzip -c "/tmp/e12-trace-$m.log" > "$out/$n.$m.trace.log.gz"
done
ls "$out" | grep -c "^$n"
