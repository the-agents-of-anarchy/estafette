#!/bin/sh
# usage: e18-batch.sh <scn> <tag> <run0> <n>   runs e18-run.sh for run0 .. run0+n-1, foreground
set -u
cd "$(dirname "$0")"
i=0
while [ "$i" -lt "$4" ]; do
  r=$(( $3 + i ))
  sh e18-run.sh "$1" "$2" "$r" | tail -3
  i=$(( i + 1 ))
done
