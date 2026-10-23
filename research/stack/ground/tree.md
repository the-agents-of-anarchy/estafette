# Dependency tree measurement

Author: worker r1-tree (haiku), 2026-10-15. Saved by the orchestrator from the worker's two messages; the worker declined to write files. Peer content, unverified.
Toolchain: rustc and cargo 1.99.1 via `rustup run 1.99.1`. Scratch: /tmp/ra-tree/probe, edition 2024.

## Versions
| Crate | Version | MSRV | Repository | Source |
|---|---|---|---|---|
| iroh | 1.3.0 | 1.91 | https://github.com/n0-computer/iroh | cargo info |
| iroh-gossip | 0.101.0 | 1.91 | https://github.com/n0-computer/iroh-gossip | cargo info |
| rmcp | 3.5.1 | 1.88 | https://github.com/modelcontextprotocol/rust-sdk/ | cargo info (official SDK confirmed) |
| clap | 4.6.7 | 1.85 | https://github.com/clap-rs/clap | cargo info |

## Tree growth (unique name+version, root included)
| After adding | Unique |
|---|---|
| baseline | 1 |
| iroh (default features) | 239 |
| iroh-gossip | 246 |
| rmcp (server, transport-io) | 265 |
| clap (derive) | 276 |
`cargo metadata` reports 400 packages across all targets; 276 is the host tree.

## Small crates already present
| Crate | Version | Pulled by (first parents) |
|---|---|---|
| tokio | 1.53.3 | iroh, h2, hyper, ... |
| serde | 1.0.229 | iroh, iroh-base, iroh-gossip, ed25519-dalek, ... |
| serde_json | 1.0.154 | rmcp, rmcp-macros, schemars |
| postcard | 1.1.3 | iroh-gossip, iroh-relay |
| ed25519-dalek | 3.0.0 | iroh, iroh-base |
| blake3 | 1.8.7 | iroh, iroh-gossip, iroh-relay |
| rand | 0.10.3 | iroh, iroh-base, iroh-gossip, iroh-relay, ... |
| data-encoding | 2.11.1 | iroh, iroh-base, iroh-gossip, iroh-relay |
| uuid | 1.28.0 | rmcp |
| tracing | 0.1.44 | iroh, iroh-gossip, iroh-relay, irpc, ... |
| thiserror | 2.0.21 (1.0.69 on other targets via netdev, netwatch) | noq, noq-proto, rmcp, cobs |
| anyhow | 1.0.104 | n0-error |
| bytes | 1.12.1 | iroh, hyper, ... |
| futures | 0.3.34 | rmcp, igd-next |
| irpc | 0.17.0 | iroh-gossip |
Absent: base32, toml, serde_yaml, serde_yml, serde_norway, yaml-rust2, saphyr, dirs, directories, xdg, n0-snafu, snafu, quic-rpc. toml would come only with iroh-gossip's off-by-default `simulator` feature.

## Features
| Crate | Default | Enabled in this build |
|---|---|---|
| iroh 1.3.0 | metrics, fast-apple-datapath, portmapper, tls-ring | same |
| iroh-gossip 0.101.0 | net, metrics | same |
| rmcp 3.5.1 | base64, macros, server | plus schemars, transport-async-rw, transport-io, uuid |
| clap 4.6.7 | color, error-context, help, std, suggestions, usage | plus derive |

## Build
Success, exit 0, zero warnings. `rustup run 1.99.1 cargo build`, 23.66 s wall (113.36 s user). Tail:
```
   Compiling portmapper v0.19.3
   Compiling iroh-gossip v0.101.0
   Compiling probe v0.1.0 (/private/tmp/ra-tree/probe)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.63s
```
main.rs calls none of these crates: the build proves resolution and compilation, not API use.

## Notes
- iroh-gossip costs seven crates on top of iroh.
- rmcp's stdio server needs only `transport-io` beyond defaults.
- iroh is 1.x, iroh-gossip 0.101: pin both deliberately; their versions do not track.
- 1.99.1 exceeds every MSRV (max 1.91).
