#!/bin/sh
# Gzip the raw logs of /tmp/e10 (written by `ctl`) into logs/e16/. usage: e16-archive.sh <glob-prefix> e.g. a-1w-
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$dir/logs/e16"
for f in /tmp/e10/"$1"*.raw.log; do
  gzip -c "$f" > "$dir/logs/e16/$(basename "$f").gz"
  gzip -c "${f%.raw.log}.trace.txt" > "$dir/logs/e16/$(basename "${f%.raw.log}").trace.txt.gz"
done
ls "$dir/logs/e16"
