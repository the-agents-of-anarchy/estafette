#!/bin/sh
# Gzip one E21 run into logs/e21/ (or $E21_LOGDIR): the raw log, the trace text, the members' stderr (E17_ERR_DIR=/tmp/e21err) and tracing files (/tmp/e12-trace-*.log), and the saved-hosts files (/tmp/e21known).
# usage: [E21_LOGDIR=logs/e21-linux] e21-archive.sh <run-name> e.g. a-8z1w-2100
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
out="$dir/${E21_LOGDIR:-logs/e21}"
mkdir -p "$out"
n=$1
gzip -c "/tmp/e10/$n.raw.log" > "$out/$n.raw.log.gz"
gzip -c "/tmp/e10/$n.trace.txt" > "$out/$n.trace.txt.gz"
for m in M1 M2 M3 M4 B1 B2; do
  [ -f "/tmp/e21err/err-$m-member.log" ] && gzip -c "/tmp/e21err/err-$m-member.log" > "$out/$n.$m.err.gz"
  [ -f "/tmp/e12-trace-$m.log" ] && gzip -c "/tmp/e12-trace-$m.log" > "$out/$n.$m.trace.log.gz"
done
if [ -d /tmp/e21known ]; then
  for f in /tmp/e21known/known-*.txt; do
    [ -f "$f" ] && cp "$f" "$out/$n.$(basename "$f")"
  done
fi
ls "$out" | grep -c "^$n"
