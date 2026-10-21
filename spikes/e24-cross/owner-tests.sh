#!/bin/sh
# E24 owner-assisted tests. Run on the Mac (machine A), from spikes/e24-cross.
#
# START (one command):   sh owner-tests.sh sleep      or      sh owner-tests.sh wifi
#
# Steps for the owner, test (a) sleep and wake:
#   1. Run "sh owner-tests.sh sleep" in a terminal on the Mac and wait about 1 minute for the line "READY".
#   2. Press Enter. The Mac goes to sleep 5 seconds later by itself.
#   3. Leave the Mac asleep for 2 minutes.
#   4. Wake the Mac by pressing a key or opening the lid.
#   5. Wait 2.5 minutes without touching anything; the line "Done" ends the test.
#
# Steps for the owner, test (b) Wi-Fi switch to a phone hotspot:
#   1. Switch on the phone's personal hotspot and keep the phone next to the Mac.
#   2. Run "sh owner-tests.sh wifi" in a terminal on the Mac and wait about 1 minute for the line "READY".
#   3. Switch the Mac from the home Wi-Fi to the phone hotspot and wait until the Mac shows it is connected.
#   4. Press Enter in the terminal.
#   5. Wait 2.5 minutes on the hotspot; the line "Done" ends the test. Then switch back to the home Wi-Fi.
#
# Needs: ssh access to B and C as in e24run.py (E24_B, E24_C, E24_KEY, E24_KNOWN_HOSTS), the hosts B and C set up by the E24 run (relays, e24-cross, ctl.sh), and ./target/release/e24-cross built on this Mac.
# Output: logs/e24/<test>-<date>/ ; read it with: python3 -I analyze_e24owner.py logs/e24/<that directory>
cd "$(dirname "$0")" || exit 1
case "$1" in
  sleep | wifi) ;;
  *)
    echo "usage: sh owner-tests.sh sleep|wifi"
    exit 1
    ;;
esac
tag="owner-$1-$(date +%m%d-%H%M)"
exec python3 e24owner.py "$1" "$tag"
