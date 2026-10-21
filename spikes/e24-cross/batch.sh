#!/bin/sh
# E24 batch (run from spikes/e24-cross on A). Order: control (hb trigger) first, then interleaved with the spec arm; relay-only, hosts apply the deaf rule.
cd "$(dirname "$0")" || exit 1
R="E24_RELAYONLY=1 E24_HOST_DEAF=1"
for i in 1 2 3 4 5 6 7 8; do
  python3 -I e24run.py kill "k-hb-$i" hb 120 $R
  python3 -I e24run.py kill "k-spec-$i" spec 120 $R
  echo "done pair $i $(date -u +%T)"
done
# isolate the host rule: spec trigger, hosts without the deaf rule, relay-only
for i in 1 2 3 4; do
  python3 -I e24run.py kill "k-spec-nohd-$i" spec 120 E24_RELAYONLY=1
  echo "done nohd $i $(date -u +%T)"
done
# IP transports on (as a product would run), spec trigger and control, hosts with the deaf rule
for i in 1 2 3 4; do
  python3 -I e24run.py kill "ip-hb-$i" hb 120 E24_HOST_DEAF=1
  python3 -I e24run.py kill "ip-spec-$i" spec 120 E24_HOST_DEAF=1
  echo "done ip $i $(date -u +%T)"
done
python3 -I e24run.py quiet "quiet-1" 600 E24_HOST_DEAF=1
echo "done quiet $(date -u +%T)"
for i in 1 2 3 4 5; do
  python3 -I e24run.py runtime "rt-$i" E24_HOST_DEAF=1
  echo "done rt $i $(date -u +%T)"
done
python3 -I e24run.py invite "inv-1" 5 E24_HOST_DEAF=1
echo "ALL DONE $(date -u +%T)"
