#!/bin/sh
# Stock hol-repro runs, per the run list, in order, foreground, no source change.
R=$RESULTS_DIR
cd $PKG/repro/hol-repro
echo "first $(date -u +%T) $(uptime)" > $R/uptime.txt
for i in $(seq 20); do ./target/debug/hol-repro | grep RESULT; done > $R/stock-b200.txt
for i in $(seq 10); do ./target/debug/hol-repro --no-kill | grep RESULT; done > $R/stock-nokill.txt
for i in $(seq 10); do ./target/debug/hol-repro --burst 0 | grep RESULT; done > $R/stock-b0.txt
for i in $(seq 5); do ./target/debug/hol-repro --burst 64 --wait-secs 5 | grep RESULT; done > $R/stock-b64.txt
for i in $(seq 5); do ./target/debug/hol-repro --burst 100 --wait-secs 5 | grep RESULT; done > $R/stock-b100.txt
for i in $(seq 3); do ./target/debug/hol-repro --rebind-on-fail | grep RESULT; done > $R/stock-rebind.txt
echo STOCKDONE > $R/stock-done.txt
