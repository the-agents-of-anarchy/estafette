#!/bin/sh
# Vendor iroh-gossip 0.101.0 from the cargo registry and apply the E18 patch (state logging in proto/hyparview.rs, E18_FIX switch). vendor/ is not committed.
set -eu
cd "$(dirname "$0")"
mkdir -p vendor
rm -rf vendor/iroh-gossip-0.101.0
tmp=$(mktemp -d)
mkdir -p "$tmp/src"
printf '[package]\nname="f"\nversion="0.0.0"\nedition="2021"\n[dependencies]\niroh-gossip="=0.101.0"\n' > "$tmp/Cargo.toml"
: > "$tmp/src/lib.rs"
(cd "$tmp" && cargo fetch >/dev/null 2>&1)
src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/iroh-gossip-0.101.0 | head -1)
cp -R "$src" vendor/iroh-gossip-0.101.0
rm -rf "$tmp"
(cd vendor/iroh-gossip-0.101.0 && patch src/proto/hyparview.rs < ../../e18-iroh-gossip-0.101.0.patch)
