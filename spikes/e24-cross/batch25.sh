#!/bin/sh
# E25 batch: hosts without the deaf rule and without a host join timeout; control (hb, 3 s) first in each pair; relay-only block, then IP-transport block.
cd "$(dirname "$0")" || exit 1
for i in 1 2 3 4 5 6 7 8; do
  python3 -I e24run.py kill "e25r-hb-$i" hb 120 E24_RELAYONLY=1
  python3 -I e24run.py kill "e25r-sp-$i" spec 120 E24_RELAYONLY=1
  echo "done relay pair $i $(date -u +%T)"
done
for i in 1 2 3 4 5 6 7 8; do
  python3 -I e24run.py kill "e25i-hb-$i" hb 120
  python3 -I e24run.py kill "e25i-sp-$i" spec 120
  echo "done ip pair $i $(date -u +%T)"
done
echo "ALL DONE $(date -u +%T)"
