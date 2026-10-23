#!/bin/sh
# Vendor iroh 1.3.0 from the cargo registry and apply the HOL diagnostic patch. vendor/ is not committed.
set -eu
cd "$(dirname "$0")"
cargo fetch --manifest-path ../hol-repro/Cargo.toml >/dev/null
src=$(ls -d "$HOME"/.cargo/registry/src/*/iroh-1.3.0 | head -1)
rm -rf vendor
mkdir vendor
cp -R "$src" vendor/iroh-1.3.0
(cd vendor/iroh-1.3.0 && patch -p0 src/socket/transports/relay/actor.rs < ../../hol-iroh-1.3.0.patch)
