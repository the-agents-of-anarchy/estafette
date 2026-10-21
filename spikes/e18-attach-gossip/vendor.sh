#!/bin/sh
# Vendor iroh-gossip 0.101.0 from the cargo registry and apply the E18 patch (state logging in proto/hyparview.rs, E18_FIX switch). vendor/ is not committed.
set -eu
cd "$(dirname "$0")"
cargo fetch --manifest-path ../e10-gossip-main/Cargo.toml >/dev/null
src=$(ls -d "$HOME"/.cargo/registry/src/*/iroh-gossip-0.101.0 | head -1)
rm -rf vendor
mkdir vendor
cp -R "$src" vendor/iroh-gossip-0.101.0
(cd vendor/iroh-gossip-0.101.0 && patch src/proto/hyparview.rs < ../../e18-iroh-gossip-0.101.0.patch)
