#!/bin/sh
# Main and patched runs per the run list, in order, foreground, no source change.
R=$RESULTS_DIR
cd $PKG/repro/hol-repro-main
for i in $(seq 20); do ./target/debug/hol-repro-main --wait-secs 8 | grep RESULT; done > $R/main-b200.txt
echo MAINDONE > $R/main-done.txt
cd $PKG/repro/hol-repro-patched
/usr/bin/time -f "WALL %e s" sh vendor.sh > $R/vendor.txt 2>&1; echo "vendor exit=$?" >> $R/vendor.txt
/usr/bin/time -f "WALL %e s" cargo build --quiet > $R/build-patched.txt 2>&1; echo "build exit=$?" >> $R/build-patched.txt
for f in drain drop; do for i in $(seq 20); do E14_FIX=$f ./target/debug/hol-repro-patched --wait-secs 8 | grep RESULT; done > $R/patched-$f.txt; done
: > $R/patched-stock.txt
for i in $(seq 5); do
  [ $i = 5 ] && echo "last $(date -u +%T) $(uptime)" >> $R/uptime.txt
  ./target/debug/hol-repro-patched --wait-secs 8 | grep RESULT >> $R/patched-stock.txt
done
pgrep -x hol-repro > $R/pgrep-after.txt; pgrep -x hol-repro-main >> $R/pgrep-after.txt; pgrep -x hol-repro-patched >> $R/pgrep-after.txt
echo RESTDONE > $R/rest-done.txt
