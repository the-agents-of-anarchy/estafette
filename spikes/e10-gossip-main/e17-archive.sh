#!/bin/sh
# Gzip one E17 run into logs/e17/: the raw log, the trace text, the members' stderr (E17_ERR_DIR=/tmp/e17err) and tracing files (/tmp/e12-trace-M*.log).
# usage: e17-archive.sh <run-name> e.g. a-1x1w-1
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$dir/logs/e17"
n=$1
gzip -c "/tmp/e10/$n.raw.log" > "$dir/logs/e17/$n.raw.log.gz"
gzip -c "/tmp/e10/$n.trace.txt" > "$dir/logs/e17/$n.trace.txt.gz"
for m in M1 M2 M3 B1 B2; do
  [ -f "/tmp/e17err/err-$m-member.log" ] && gzip -c "/tmp/e17err/err-$m-member.log" > "$dir/logs/e17/$n.$m.err.gz"
  [ -f "/tmp/e12-trace-$m.log" ] && gzip -c "/tmp/e12-trace-$m.log" > "$dir/logs/e17/$n.$m.trace.log.gz"
done
ls -la "$dir/logs/e17" | grep "$n"
for f in /tmp/e17err/err-*-boot.log /tmp/e17err/err-*-relay*.log; do
  [ -f "$f" ] && gzip -c "$f" > "$dir/logs/e17/$n.$(basename "$f" .log).gz"
done
ls "$dir/logs/e17" | grep -c "^$n"
