#!/bin/sh
# Builds the reproductions and runs the counts quoted in README.md section 3 and 6.
#   ./run.sh          full counts (about 25 min on a laptop; each failing run waits out its window)
#   ./run.sh quick    3 runs per count (about 3 min)
# Output: results/rerun-<UTC timestamp>/*.txt (one RESULT line per run) and a summary on stdout.
# Needs: rustup (rust-toolchain.toml pins 1.99.1), network for crates.io and the iroh git dependency of hol-repro-main.
set -eu
cd "$(dirname "$0")"
ROOT=$(pwd)
mode=${1:-full}
case "$mode" in
  full) n20=20 n10=10 n5=5 ;;
  quick) n20=3 n10=3 n5=3 ;;
  *) echo "usage: $0 [full|quick]" >&2; exit 2 ;;
esac
OUT="$ROOT/results/rerun-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$OUT"

echo "== build"
(cd "$ROOT/repro/hol-repro" && cargo build --quiet)
(cd "$ROOT/repro/hol-repro-main" && cargo build --quiet)
(cd "$ROOT/repro/hol-repro-patched" && sh vendor.sh >/dev/null && cargo build --quiet)
STOCK="$ROOT/repro/hol-repro/target/debug/hol-repro"
MAIN="$ROOT/repro/hol-repro-main/target/debug/hol-repro-main"
PATCHED="$ROOT/repro/hol-repro-patched/target/debug/hol-repro-patched"

# runs <name> <count> <command...>: runs the command <count> times, keeps the RESULT lines, prints PASS/FAIL counts.
runs() {
  name=$1
  count=$2
  shift 2
  : > "$OUT/$name.txt"
  i=0
  while [ "$i" -lt "$count" ]; do
    "$@" | grep RESULT >> "$OUT/$name.txt" || true
    i=$((i + 1))
  done
  printf '%-34s ' "$name"
  cut -d' ' -f2 "$OUT/$name.txt" | sort | uniq -c | tr '\n' ' '
  echo
}

echo "== iroh 1.3.0 (expect FAIL for burst >= 100 with R1 dead; PASS for the controls)"
runs stock-b200            "$n20" "$STOCK"
runs stock-nokill          "$n10" "$STOCK" --no-kill
runs stock-b0              "$n10" "$STOCK" --burst 0
runs stock-neverup-b200    "$n5"  "$STOCK" --never-up --wait-secs 6
runs stock-b64             "$n10" "$STOCK" --burst 64 --wait-secs 5
runs stock-b100            "$n10" "$STOCK" --burst 100 --wait-secs 5
runs stock-b150            "$n10" "$STOCK" --burst 150 --wait-secs 5
runs stock-rebind          3      "$STOCK" --rebind-on-fail --wait-secs 20
echo "== iroh main 55908a5 (expect the same)"
runs main-b200             "$n20" "$MAIN" --wait-secs 8
runs main-nokill           "$n10" "$MAIN" --no-kill
echo "== iroh 1.3.0 with the diagnostic patch (drain and drop expect PASS; none expects FAIL)"
HOL_FIX=drain; export HOL_FIX
runs patched-drain         "$n20" "$PATCHED" --wait-secs 8
HOL_FIX=drop; export HOL_FIX
runs patched-drop          "$n20" "$PATCHED" --wait-secs 8
unset HOL_FIX
runs patched-none          "$n5"  "$PATCHED" --wait-secs 8
echo "== queue and blocked-state observation (HOL_DIAG=1, 20 s window)"
HOL_DIAG=1 "$PATCHED" --wait-secs 20 > "$OUT/diag-run.out" 2> "$OUT/diag-run.err" || true
grep HOLDIAG "$OUT/diag-run.err" | grep -v sleep_backoff | cut -c1-160 | head -5
echo "== done: $OUT"
