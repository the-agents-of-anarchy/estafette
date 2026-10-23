# iroh ecosystem index

Fetched: 2026-10-15.

Sources, in order:
- Awesome list: https://github.com/n0-computer/awesome-iroh (file `readme.md`, raw at `https://raw.githubusercontent.com/n0-computer/awesome-iroh/main/readme.md`; the `README.md` path returns 404).
- n0-computer public repos: https://github.com/n0-computer (via `gh repo list n0-computer --limit 200`).
- crates.io reverse dependencies: https://crates.io/api/v1/crates/iroh/reverse_dependencies and https://crates.io/api/v1/crates/iroh-gossip/reverse_dependencies.
- iroh site showcase: https://www.iroh.computer (WebFetch, homepage).

## Categories

Relevance tags, applied from each entry's description and visible crate dependencies, not judged further:
- `gossip`: presence or membership over gossip.
- `direct`: direct point-to-point connection or protocol use.
- `privnet`: identity or private networks.
- `relay-boot`: relays, bootstrap, or peer discovery and address lookup.
- `chat`: chat or messaging.
- `-`: no tag applied from visible evidence.

### Awesome list

| # | Name | URL | Description | iroh crates | Last activity | Relevance tags |
|---|---|---|---|---|---|---|
| 1 | Prime Intellect | https://github.com/PrimeIntellect-ai/protocol | Decentralized Compute Infrastructure for AI. | iroh | 2025-11-10 (archived) | - |
| 2 | Psyche | https://github.com/PsycheFoundation/psyche/ | An open infrastructure to democratize and decentralize the development of superintelligence for humanity. | iroh, iroh-blobs, iroh-gossip, iroh-relay | 2026-03-24 | gossip, relay-boot |
| 3 | Tandemn | https://github.com/Tandemn-Labs/tensor-iroh | Iroh for tensors, used for inference over heterogeneous GPUs. | iroh, iroh-base, iroh-net | 2026-03-27 | direct |
| 4 | Aster | https://github.com/fractalbeauty/aster | Local-first peer-to-peer music streaming prototype. | iroh, iroh-base | 2026-06-18 | direct |
| 5 | OpusVoice | https://github.com/IAFahim/OpusVoice | Android and browser Opus voice streaming with Pinhole sessions and native Iroh address and relay interoperability. | no root Cargo.toml | 2026-10-10 | direct, relay-boot |
| 6 | OpusVoice.Receiver | https://github.com/YouAnd-I/OpusVoice.Receiver | A C# RTP/Opus test sink that captures streams over UDP, WebSocket, and Pinhole/Iroh routes. | no root Cargo.toml | 2026-10-08 | relay-boot |
| 7 | Telepathy | https://github.com/chanderlud/telepathy | Cross-platform real time communication with Iroh networking. | no root Cargo.toml | 2026-08-09 | direct, chat |
| 8 | Backup Buddies | https://github.com/mspencerl87/backup-buddies-client | Peer-to-peer file backups with local encryption, delta scans, and retained restore versions. | iroh, iroh-mdns-address-lookup | 2026-10-14 | direct |
| 9 | omry | https://codeberg.org/omry/omry | Save web pages you don't want to lose to a locally-hostable, searchable archive. | n/a (non-GitHub) | 2026-09-25 (codeberg) | - |
| 10 | P-Pass | https://github.com/hawkeye-xb/P-Pass | Phones back up photos automatically to the computer at home, with no account and no cloud storage. | none at root | 2026-10-15 | direct |
| 11 | Save DWeb Backend | https://github.com/OpenArchive/save-dweb-backend | DWeb Backend for the Save app based on Veilid and Iroh. | iroh, iroh-base, iroh-blobs, iroh-docs, iroh-gossip, iroh-metrics, iroh-net | 2026-06-29 | gossip |
| 12 | Oku | https://okubrowser.github.io/ | A Web browser with an emphasis on local-first data storage. | n/a (non-GitHub) | not fetched (website) | - |
| 13 | biter | https://github.com/LukeDSchenk/biter | Synchronize device clipboards via iroh. | iroh | 2026-02-05 | direct |
| 14 | linXiv | https://github.com/linxiv-dev/linXiv | Local-first academic paper manager with p2p sharing enabled using iroh. | no root Cargo.toml | 2026-10-11 | direct |
| 15 | p2pmux | https://github.com/pelazas/p2pmux | Terminal multiplexer where every pane is a PTY on its owner's own machine. | iroh | 2026-09-19 | direct |
| 16 | Teamtype | https://github.com/teamtype/teamtype | Editor-agnostic, real-time collaborative editing of local text files. | none at root | 2026-10-15 | - |
| 17 | Zeco | https://github.com/julianbuettner/zeco | Share zellij sessions via the internet (using iroh). | iroh | 2026-09-12 | direct |
| 18 | Codux | https://github.com/duxweb/codux | A native connected terminal for AI coding agents, using iroh for encrypted peer-to-peer control across desktop, mobile, and headless hosts. | none at root | 2026-07-21 | direct |
| 19 | cross.stream | https://github.com/cablehead/xs | Local-first event streaming for reactive workflows, with P2P sharing via Iroh. | iroh, iroh-base, iroh-tickets | 2026-09-29 | direct |
| 20 | do-ssh | https://github.com/doEggi/do-ssh | Tunnels an ssh-connection over iroh. | iroh | 2025-10-08 (archived) | direct |
| 21 | Datum | https://github.com/datum-cloud/app | Expose local services to the internet. | iroh, iroh-base, iroh-metrics, iroh-proxy-utils, iroh-quinn, iroh-relay, iroh-services, iroh-tickets | 2026-10-14 | direct, relay-boot |
| 22 | Dumbpipe | https://github.com/n0-computer/dumbpipe | Cross-device unix pipe. | iroh, iroh-tickets | 2026-10-15 | direct |
| 23 | Edky | https://github.com/artob/edky | Convert Ed25519 public keys between various encoding formats (iroh, libp2p, IPFS, OpenSSH, etc). | no root Cargo.toml | 2026-10-14 | - |
| 24 | iroh-ssh | https://github.com/rustonbsd/iroh-ssh | SSH to any machine without ip. | iroh, iroh-mainline-address-lookup, iroh-mdns-peer-lookup | 2026-10-10 | direct, relay-boot |
| 25 | iroh-ssh-android | https://github.com/futpib/iroh-ssh-android | Android SSH client for hosts running iroh-ssh (and OpenSSH too). | no root Cargo.toml | 2026-10-10 | direct |
| 26 | pai-sho | https://github.com/cablehead/pai-sho | P2P TCP port forwarding: dumbpipe, but multiple ports, and it reconnects. | iroh | 2026-10-01 | direct |
| 27 | Rayfish | https://github.com/rayfish/rayfish | A P2P mesh VPN that connects peers by cryptographic identity instead of IP address. | iroh, iroh-base, iroh-blobs, iroh-dns, iroh-mdns-address-lookup, iroh-metrics, iroh-relay, iroh-tor-transport | 2026-10-15 | privnet, relay-boot |
| 28 | Resilum Core | https://github.com/Resilum/resilum-core | Multi-transport Reticulum node: one peer identity across Tor, I2P, Yggdrasil, BLE and iroh/QUIC, with LXMF messaging and SOCKS5 egress. | none at root | 2026-09-15 | privnet, chat |
| 29 | Tunnet | https://github.com/tunnetio/Tunnet | Open-source mesh networking platform that scales from direct p2p connections for individuals to fully managed deployments for large organizations. | iroh, iroh-blobs, iroh-docs, iroh-gossip, iroh-mainline-address-lookup, iroh-mdns-address-lookup, iroh-relay | 2026-10-15 | gossip, privnet, relay-boot |
| 30 | locho | https://github.com/trchopan/locho | Local + echo = locho: Access a private service on another machine as if it were local. | iroh | 2026-10-15 | direct, privnet |
| 31 | koh | https://github.com/gold-silver-copper/koh | A remote shell inspired by mosh. Serve and connect from anywhere, survive network changes, no port forwarding required. | iroh, iroh-base | 2026-10-13 | direct |
| 32 | APT Repository | https://gitlab.com/megalithic-llc/apt-repository | A Debian APT Repository, and web console for remote administration. | n/a (non-GitHub) | unreachable (gitlab 404) | - |
| 33 | ARK Drop Desktop | https://github.com/ARK-Builders/ARK-Drop-Desktop | Use QR codes to quickly send and receive files between devices. | none at root | 2026-01-22 | direct |
| 34 | Alt-sendme | https://github.com/tonyantony300/alt-sendme | A cross-platform desktop application for file sharing, built with Tauri. | no root Cargo.toml | 2026-10-12 | direct |
| 35 | DataBeam | https://github.com/vinay-winai/DataBeam | File sharing desktop app with features like: share via short custom code or QR code, share text, auto-retry on failure, and automatic overwrite conflict resolution. | none at root | 2026-10-03 | direct |
| 36 | Dropwire | https://github.com/muhamadjawdatsalemalakoum/dropwire | Send any file directly between devices, end-to-end encrypted and resumable, with no account and no server. | iroh, iroh-blobs, iroh-mainline-address-lookup | 2026-10-12 | direct, relay-boot |
| 37 | fsend | https://github.com/maxomatic458/fsend | Peer-to-peer file sharing webapp and cli tool, allows transferring of files and folders with resumable transfers. | no root Cargo.toml | 2026-10-05 | direct |
| 38 | iroh_send | https://github.com/thiswillbeyourgithub/iroh-send | Send files or directories by sharing an env variable (python). | no root Cargo.toml | 2026-03-29 | direct |
| 39 | lis | https://github.com/riffcc/lis | Life is short, but data should live forever. | iroh | 2025-08-13 | - |
| 40 | Quicksend | https://github.com/israelyago/QuickSend | Send files peer to peer. | no root Cargo.toml | 2026-04-03 | direct |
| 41 | ringdrop | https://github.com/rikettsie/ringdrop | Frugal, daemonized P2P file transfer with ring-based access control, built on iroh and bao protocols. | iroh, iroh-blobs, iroh-io, iroh-rings | 2026-09-25 | direct, privnet |
| 42 | ringdrop-gui | https://github.com/rikettsie/ringdrop-gui | Desktop GUI for ringdrop, exposing the full CLI as a native app. | no root Cargo.toml | 2026-09-25 | direct |
| 43 | Rust Patcher | https://github.com/rustonbsd/rustpatcher | Secure Decentralized Software updates. | none at root | 2025-10-26 | - |
| 44 | sendblob | https://github.com/hboisgibault/sendblob | Share files from the browser ([sendblob.app](https://sendblob.app)) via iroh-blobs relays, interoperable with any iroh node. | iroh, iroh-blobs, iroh-io | 2026-09-03 | direct, relay-boot |
| 45 | Sendme-egui | https://github.com/zignig/sendme-egui | Egui interface for sendme. | iroh, iroh-blobs | 2025-10-01 | direct |
| 46 | Sendme | https://github.com/n0-computer/sendme | A tool to send files and directories, based on iroh. | iroh, iroh-blobs | 2026-10-15 | direct |
| 47 | Strada | https://strada.tech | Beatiful file transfer & management with no cloud costs. | n/a (non-GitHub) | not fetched (website) | - |
| 48 | Peernet | https://github.com/dvc94ch/peernet | Easy & reliable p2p networking. | iroh | 2025-03-22 | direct |
| 49 | p2panda | https://github.com/p2panda/p2panda | Building blocks for peer-to-peer applications. | none at root | 2026-10-15 | - |
| 50 | distributed-topic-tracker | https://github.com/rustonbsd/distributed-topic-tracker | Iroh-gossip with auto discovery (no servers!) | iroh, iroh-gossip | 2026-06-15 | gossip, relay-boot |
| 51 | imsg | https://github.com/n0-computer/imsg | A base protocol providing streams of messages. | iroh | 2025-08-22 | direct, chat |
| 52 | iroh-gossip-discovery | https://github.com/therishidesai/iroh-gossip-discovery | Peer discovery for iroh gossip topics. | iroh, iroh-gossip | 2025-10-08 | gossip, relay-boot |
| 53 | iroh-http | https://github.com/changesbyjames/iroh-http | An HTTP-like request/response protocol over Iroh QUIC streams, with Rust, Node.js, and browser/WebAssembly adapters. | iroh, iroh-tickets | 2026-10-11 | direct |
| 54 | iroh-lighthouse | https://github.com/Jezza/iroh-lighthouse | A self-hostable rendezvous server for topic based peer discovery. | iroh, iroh-lighthouse-client, iroh-lighthouse-protocol | 2026-10-02 | relay-boot |
| 55 | iroh-rings | https://github.com/rikettsie/iroh-rings | A relationship-based access control protocol for resources over iroh. | iroh, iroh-blobs, iroh-io | 2026-09-25 | privnet |
| 56 | irpc | https://github.com/n0-computer/irpc | RPC for memory, quinn or iroh connections. | iroh | 2026-10-15 | direct |
| 57 | Bones | https://github.com/fishfolk/bones | An easy-to-use game engine for making real games, networked with iroh. | iroh | 2026-10-12 | direct |
| 58 | Fish Folk: Jumpy | https://github.com/fishfolk/jumpy | Tactical 2D shooter in fishy pixels style. Made with Rust-lang 🦀, built on Bones. | none at root | 2026-01-30 | - |
| 59 | Godot Iroh | https://github.com/tipragot/godot-iroh | A peer-to-peer multiplayer extension for Godot based on iroh. | iroh | 2026-10-14 | direct |
| 60 | iroh-lan | https://github.com/rustonbsd/iroh-lan | Have a lan party with iroh (iroh-lan = hamachi - account - install). | iroh, iroh-blobs, iroh-docs, iroh-gossip | 2026-07-26 | gossip, privnet, direct |
| 61 | USD Renderer | https://github.com/expenses/usd-render | Universal Scene Description Render, linked with iroh. | iroh-net | 2024-02-21 | - |
| 62 | Pigg | https://github.com/andrewdavidmackenzie/pigg | Raspberry Pi GPIO GUI. | none at root | 2026-10-12 | - |
| 63 | Bru | https://github.com/sverrejb/bru | Read and send your phone's SMS, and sync clipboard text, from your desktop. | no root Cargo.toml | 2026-10-14 | chat |
| 64 | Dash Chat | https://darksoil.studio | Secure Chat for Internet Shutdowns. | n/a (non-GitHub) | not fetched (website) | chat |
| 65 | Delta Chat | https://delta.chat | A decentralized and secure messenger app. | n/a (non-GitHub) | not fetched (website) | chat |
| 66 | Tauri + Leptos + Tailwind CSS + Iroh + Keyhive/Beelay | https://github.com/Zyell/beelay-iroh-chat | Proof of concept chat app combining automerge keyhive/beelay, iroh and tauri. | none at root | 2025-06-17 | chat |
| 67 | Tauri Chat Example | https://github.com/jamessizeland/peer-to-peer | Example cross-platform chat app demo, using Tauri v2 & React. | no root Cargo.toml | 2025-06-14 | chat |
| 68 | Rostra | https://radicle.network/nodes/radicle.dpc.pw/rad%3AzzK566qFsZnXomX2juRjxj9K1LuF | A p2p (or rather f2f) social network. | n/a (non-GitHub) | not fetched (website) | - |
| 69 | Weird | https://weird.one/ | A way to be on the web. | n/a (non-GitHub) | not fetched (website) | - |
| 70 | Aurora | https://github.com/Lizeyun8501/Aurora | A personal knowledge-management app for notes, tasks, and organizing ideas. | iroh | 2026-10-15 | - |
| 71 | Iroh Retro | https://codeberg.org/cemoktra/iroh-retro | Serverless, no-account, retrospective for development teams. | n/a (non-GitHub) | 2026-08-31 (codeberg) | - |
| 72 | Kith | https://github.com/muhamadjawdatsalemalakoum/kith | Serverless, no-account, end-to-end-encrypted sync for your own devices — memory, tabs, and files — that your AI can also read and write over MCP. | iroh, iroh-blobs, iroh-mainline-address-lookup | 2026-07-05 | direct, relay-boot |
| 73 | Obsiroh | https://github.com/DrHongos/obsiroh | An Obsidian sync method with iroh. | iroh | 2024-02-24 | direct |
| 74 | Analog | https://github.com/Analog-Labs/timechain | An omnichain network powered by Proof of Time. | none at root | 2026-03-26 | - |
| 75 | Fedimint | https://github.com/fedimint/fedimint | Federated E-Cash Mint. | iroh, iroh-base, iroh-mainline-address-lookup, iroh-next, iroh-next-base, iroh-relay | 2026-10-15 | relay-boot |
| 76 | Recall | https://github.com/recallnet/ipc | Lets any agent prove, refine, and earn from their intelligence, onchain. | iroh, iroh-base, iroh-blobs, iroh-quinn, iroh-relay | 2025-06-18 | relay-boot |
| 77 | Thresh | https://github.com/rodrigomd94/Thresh | A Desktop Cardano Wallet. | no root Cargo.toml | 2025-08-29 | - |

### n0-computer repos

Source: `gh repo list n0-computer --limit 200`. Archived repos are excluded (8 archived of 129 total). Tags here come from name and description only, with no code scan.

| # | Name | URL | Description | iroh crate | Last activity | Relevance tags |
|---|---|---|---|---|---|---|
| 1 | noq | https://github.com/n0-computer/noq | noq, a QUIC implementation in Rust | - | 2026-10-15 | - |
| 2 | iroh | https://github.com/n0-computer/iroh | IP addresses break, dial keys instead. A library that adds QUIC + NAT Traversal to your apps. | yes (repo is the crate) | 2026-10-15 | direct, relay-boot |
| 3 | net-tools | https://github.com/n0-computer/net-tools | Cross platform, networking utilties in Rust | - | 2026-10-15 | - |
| 4 | n0-dns-resolver | https://github.com/n0-computer/n0-dns-resolver | A lightweight async DNS resolver | - | 2026-10-15 | relay-boot |
| 5 | iroh-address-lookups | https://github.com/n0-computer/iroh-address-lookups | DHT and mDNS-based address lookup for iroh | yes (repo is the crate) | 2026-10-15 | relay-boot |
| 6 | iroh-gossip | https://github.com/n0-computer/iroh-gossip | (no description) | yes (repo is the crate) | 2026-10-15 | gossip |
| 7 | irpc | https://github.com/n0-computer/irpc | A streaming rpc system for iroh | - | 2026-10-15 | direct |
| 8 | iroh-metrics | https://github.com/n0-computer/iroh-metrics | (no description) | yes (repo is the crate) | 2026-10-15 | - |
| 9 | pigeons | https://github.com/n0-computer/pigeons | Carrier pigeons always find their way home. | - | 2026-10-15 | direct |
| 10 | iroh-c-ffi | https://github.com/n0-computer/iroh-c-ffi | C FFI for iroh | yes (repo is the crate) | 2026-10-15 | - |
| 11 | iroh-blobs | https://github.com/n0-computer/iroh-blobs | Blobs layer for iroh | yes (repo is the crate) | 2026-10-15 | direct |
| 12 | iroh-live | https://github.com/n0-computer/iroh-live | media livestreaming over iroh | yes (repo is the crate) | 2026-10-15 | direct |
| 13 | iroh-content-discovery | https://github.com/n0-computer/iroh-content-discovery | (no description) | yes (repo is the crate) | 2026-10-15 | relay-boot |
| 14 | dumbpipe | https://github.com/n0-computer/dumbpipe | Unix pipes between devices | - | 2026-10-15 | direct |
| 15 | patchbay | https://github.com/n0-computer/patchbay | Rust library for realistic network simulation via Linux namespaces | - | 2026-10-15 | - |
| 16 | sendme | https://github.com/n0-computer/sendme | A tool to send files and directories, based on iroh | - | 2026-10-15 | direct |
| 17 | n0-watcher | https://github.com/n0-computer/n0-watcher | Asynchronously listen for (latest) changes of values and combine them. Async runtime independent + WebAssembly supported. | - | 2026-10-15 | - |
| 18 | iroh-share | https://github.com/n0-computer/iroh-share | Blob sharing via bittorrent mainline DHT | yes (repo is the crate) | 2026-10-15 | relay-boot |
| 19 | iroh-util | https://github.com/n0-computer/iroh-util | Utilities for iroh | yes (repo is the crate) | 2026-10-15 | direct |
| 20 | iroh-tickets | https://github.com/n0-computer/iroh-tickets | Simple ticket system used for signaling with iroh | yes (repo is the crate) | 2026-10-15 | relay-boot |
| 21 | tokio-rustls-acme | https://github.com/n0-computer/tokio-rustls-acme | Automatic TLS certificate management using rustls | - | 2026-10-15 | - |
| 22 | iroh-workshop-esp32 | https://github.com/n0-computer/iroh-workshop-esp32 | iroh on ESP32 microcontrollers: a hands-on workshop in small steps | yes (repo is the crate) | 2026-10-15 | direct |
| 23 | iroh-services | https://github.com/n0-computer/iroh-services | iroh based client to interact with iroh-services | yes (repo is the crate) | 2026-10-14 | relay-boot |
| 24 | iroh-esp32-examples | https://github.com/n0-computer/iroh-esp32-examples | Self-contained example how to run iroh on an ESP32 | yes (repo is the crate) | 2026-10-14 | direct |
| 25 | workflows | https://github.com/n0-computer/workflows | Collection of reusable workflows for n0 repos | - | 2026-10-14 | - |
| 26 | n0-tracing-test | https://github.com/n0-computer/n0-tracing-test | Access and evaluate tracing logs in async and sync tests. | - | 2026-10-13 | - |
| 27 | iroh-mainline-address-lookup-blinded | https://github.com/n0-computer/iroh-mainline-address-lookup-blinded | Iroh address lookup using the mainline DHT with blinded ed25519 keys and encryption. | yes (repo is the crate) | 2026-10-13 | relay-boot |
| 28 | iroh.computer | https://github.com/n0-computer/iroh.computer | iroh website | yes (repo is the crate) | 2026-10-13 | - |
| 29 | awesome-iroh | https://github.com/n0-computer/awesome-iroh | Curated list of awesome stuff built on & with iroh | - | 2026-10-09 | - |
| 30 | wg-iroh | https://github.com/n0-computer/wg-iroh | Experiment: an iroh endpoint inside a userspace WireGuard tunnel | - | 2026-10-09 | privnet |
| 31 | n0-mainline | https://github.com/n0-computer/n0-mainline | Simple, robust, BitTorrent's Mainline DHT implementation | - | 2026-10-08 | relay-boot |
| 32 | n0-error | https://github.com/n0-computer/n0-error | An Rust error library that supports tracking the call-site location of errors. | - | 2026-10-07 | - |
| 33 | iroh-co2-monitor | https://github.com/n0-computer/iroh-co2-monitor | (no description) | yes (repo is the crate) | 2026-10-07 | direct |
| 34 | n0-future | https://github.com/n0-computer/n0-future | Number 0's way of doing rust futures. Re-exports what we think are useful abstractions and good implementations. | - | 2026-10-07 | - |
| 35 | irpc-schema | https://github.com/n0-computer/irpc-schema | rust-rpc-schema | - | 2026-10-06 | direct |
| 36 | docs.iroh.computer | https://github.com/n0-computer/docs.iroh.computer | (no description) | - | 2026-10-06 | - |
| 37 | iroh-ffi | https://github.com/n0-computer/iroh-ffi | FFI bindings for iroh | yes (repo is the crate) | 2026-10-05 | direct |
| 38 | rcan | https://github.com/n0-computer/rcan | Really simple user Controlled Authorization Networks | - | 2026-10-05 | privnet |
| 39 | iroh-ping | https://github.com/n0-computer/iroh-ping | high level ping protocol for iroh | yes (repo is the crate) | 2026-09-30 | direct |
| 40 | rustls-rustcrypto | https://github.com/n0-computer/rustls-rustcrypto | Rustls cryptography provider built on the pure Rust crates from the RustCrypto organization | - | 2026-09-28 | - |
| 41 | n0.computer | https://github.com/n0-computer/n0.computer | (no description) | - | 2026-09-15 | - |
| 42 | iroh-pico-examples | https://github.com/n0-computer/iroh-pico-examples | Examples how to use iroh on raspberry pi pico (RP2350) microcontrollers | yes (repo is the crate) | 2026-09-15 | direct |
| 43 | n0-dns-resolver-android-demo | https://github.com/n0-computer/n0-dns-resolver-android-demo | (no description) | - | 2026-09-02 | - |
| 44 | iroh-willow | https://github.com/n0-computer/iroh-willow | https://willowprotocol.org implementation using https://iroh.computer | yes (repo is the crate) | 2026-08-31 | direct |
| 45 | n0-snafu | https://github.com/n0-computer/n0-snafu | Handle snafus with ease | - | 2026-08-31 | - |
| 46 | bao-tree | https://github.com/n0-computer/bao-tree | An implementation of BLAKE3 verified streaming | - | 2026-08-26 | - |
| 47 | iroh-dht-experiment | https://github.com/n0-computer/iroh-dht-experiment | A kademlia DHT with a 32 byte keyspace, using iroh connections | yes (repo is the crate) | 2026-08-25 | relay-boot |
| 48 | iroh-doctor | https://github.com/n0-computer/iroh-doctor | Your tool for testing iroh connectivity | yes (repo is the crate) | 2026-08-24 | direct |
| 49 | n0-quic-nat-traversal | https://github.com/n0-computer/n0-quic-nat-traversal | n0 QUIC NAT Traversal Internet-Draft | - | 2026-08-20 | - |
| 50 | iroh-docs | https://github.com/n0-computer/iroh-docs | Multi-dimensional key-value documents with an efficient synchronization protocol. | yes (repo is the crate) | 2026-08-19 | direct |
| 51 | iroh-websocket-bridge | https://github.com/n0-computer/iroh-websocket-bridge | Demo how to bridge WebSockets to Iroh | yes (repo is the crate) | 2026-07-30 | direct |
| 52 | dasl | https://github.com/n0-computer/dasl | (no description) | - | 2026-07-06 | - |
| 53 | hcloud-rust | https://github.com/n0-computer/hcloud-rust | Unofficial Rust crate for accessing the Hetzner Cloud API | - | 2026-07-03 | - |
| 54 | iroh-smart-fan | https://github.com/n0-computer/iroh-smart-fan | A smart fan example project using iroh on ESP32 | yes (repo is the crate) | 2026-07-02 | direct |
| 55 | qlog-viewer | https://github.com/n0-computer/qlog-viewer | (no description) | - | 2026-06-25 | - |
| 56 | iroh-examples | https://github.com/n0-computer/iroh-examples | Iroh in action | yes (repo is the crate) | 2026-06-24 | direct |
| 57 | iroh-node-util | https://github.com/n0-computer/iroh-node-util | Utilities to build binaries containing an iroh endpoint | yes (repo is the crate) | 2026-06-22 | direct |
| 58 | n0-qlog | https://github.com/n0-computer/n0-qlog | qlog data model for QUIC and HTTP/3 | - | 2026-06-22 | - |
| 59 | hello-iroh-ffi | https://github.com/n0-computer/hello-iroh-ffi | (no description) | - | 2026-06-15 | - |
| 60 | iroh-nym-transport | https://github.com/n0-computer/iroh-nym-transport | (no description) | yes (repo is the crate) | 2026-06-15 | direct |
| 61 | iroh-tor-transport | https://github.com/n0-computer/iroh-tor-transport | Experimental iroh custom transport for TOR | yes (repo is the crate) | 2026-06-15 | direct, privnet |
| 62 | iroh-smol-kv | https://github.com/n0-computer/iroh-smol-kv | Tiny gossip based in memory kv database for iroh | yes (repo is the crate) | 2026-06-15 | gossip |
| 63 | iroh-proxy-utils | https://github.com/n0-computer/iroh-proxy-utils | Utiltity functions for building network proxies with iroh | yes (repo is the crate) | 2026-06-15 | direct |
| 64 | iroh-experiments | https://github.com/n0-computer/iroh-experiments | a place for things that aren't in iroh, but are useful nonetheless | yes (repo is the crate) | 2026-06-01 | direct |
| 65 | aws-sdk-rust | https://github.com/n0-computer/aws-sdk-rust | AWS SDK for the Rust Programming Language | - | 2026-05-27 | - |
| 66 | swarm-discovery | https://github.com/n0-computer/swarm-discovery | A lightweight library for mDNS swarm discovery with bounded bandwidth usage | - | 2026-04-15 | relay-boot |
| 67 | abao | https://github.com/n0-computer/abao | an implementation of BLAKE3 verified streaming | - | 2026-03-25 | - |
| 68 | nested-enum-utils | https://github.com/n0-computer/nested-enum-utils | Macros to provide conversions for nested enums | - | 2026-03-25 | - |
| 69 | chuck | https://github.com/n0-computer/chuck | A place to chuck in various integration type tests and benchmarks. | - | 2026-03-21 | - |
| 70 | hickory-dns | https://github.com/n0-computer/hickory-dns | A Rust based DNS client, server, and resolver | - | 2026-03-06 | - |
| 71 | dumbpipe.dev | https://github.com/n0-computer/dumbpipe.dev | (no description) | - | 2026-02-23 | - |
| 72 | sccache-action | https://github.com/n0-computer/sccache-action | sccache github action | - | 2026-01-14 | - |
| 73 | iroh-workshop-39c3 | https://github.com/n0-computer/iroh-workshop-39c3 | iroh & iroh-gossop workshop 39c3 | yes (repo is the crate) | 2025-12-28 | gossip, direct |
| 74 | sorted-index-buffer | https://github.com/n0-computer/sorted-index-buffer | (no description) | - | 2025-12-17 | - |
| 75 | n0des-example-project | https://github.com/n0-computer/n0des-example-project | (no description) | - | 2025-11-06 | - |
| 76 | iroh-workshop-omniopencon2025 | https://github.com/n0-computer/iroh-workshop-omniopencon2025 | (no description) | yes (repo is the crate) | 2025-10-17 | direct |
| 77 | rustls | https://github.com/n0-computer/rustls | A modern TLS library in Rust | - | 2025-10-14 | - |
| 78 | big-block-sync | https://github.com/n0-computer/big-block-sync | A sync algorithm for somewhat content-addressed data | - | 2025-10-07 | - |
| 79 | iroh-car | https://github.com/n0-computer/iroh-car | General CAR file support. | yes (repo is the crate) | 2025-10-01 | direct |
| 80 | iroh-docs-mini | https://github.com/n0-computer/iroh-docs-mini | Minimalist impl of something like the old iroh docs | yes (repo is the crate) | 2025-09-19 | direct |
| 81 | imsg | https://github.com/n0-computer/imsg | A base protocol providing streams of messages | - | 2025-08-22 | direct, chat |
| 82 | iroh-streamplace | https://github.com/n0-computer/iroh-streamplace | (no description) | yes (repo is the crate) | 2025-08-15 | direct |
| 83 | ufotofu | https://github.com/n0-computer/ufotofu | Abstractions for lazily consuming and producing sequences in Rust. | - | 2025-08-03 | - |
| 84 | iroh-duck | https://github.com/n0-computer/iroh-duck | (no description) | yes (repo is the crate) | 2025-07-16 | direct |
| 85 | iroh-roq | https://github.com/n0-computer/iroh-roq | RTP over QUIC implemented as iroh protocol | yes (repo is the crate) | 2025-06-18 | direct |
| 86 | quic-rpc | https://github.com/n0-computer/quic-rpc | A streaming rpc system based on quic | - | 2025-05-12 | direct |
| 87 | iroh-io | https://github.com/n0-computer/iroh-io | (no description) | yes (repo is the crate) | 2025-04-14 | - |
| 88 | iroh-blake3 | https://github.com/n0-computer/iroh-blake3 | Fork of the blake3 crate that allows quicker subtree hashing, until it gets upstreamed | yes (repo is the crate) | 2025-04-03 | - |
| 89 | callme | https://github.com/n0-computer/callme | peer to peer audio calls with iroh | - | 2025-03-17 | direct, chat |
| 90 | gst-plugin-iroh | https://github.com/n0-computer/gst-plugin-iroh | (no description) | - | 2025-03-13 | direct |
| 91 | rustls-platform-verifier | https://github.com/n0-computer/rustls-platform-verifier | A certificate verification library for rustls that uses the operating system's verifier | - | 2025-02-17 | - |
| 92 | willow-store | https://github.com/n0-computer/willow-store | Trying out stuff for a store for the willow protocol | - | 2025-01-17 | - |
| 93 | willow-rs | https://github.com/n0-computer/willow-rs | Implementations of the Willow family of specifications in Rust. | - | 2025-01-17 | - |
| 94 | cargo-semver-checks-action | https://github.com/n0-computer/cargo-semver-checks-action | A GitHub Action for running cargo-semver-checks | - | 2024-12-02 | - |
| 95 | squiggle | https://github.com/n0-computer/squiggle | (no description) | - | 2024-11-27 | - |
| 96 | ticket.iroh.computer | https://github.com/n0-computer/ticket.iroh.computer | (no description) | - | 2024-09-30 | - |
| 97 | iroh-workshop-web3summit | https://github.com/n0-computer/iroh-workshop-web3summit | Iroh workshop at web3 summit 2024 | yes (repo is the crate) | 2024-09-27 | direct |
| 98 | iroh-workshop-jonthebeach | https://github.com/n0-computer/iroh-workshop-jonthebeach | (no description) | yes (repo is the crate) | 2024-09-27 | direct |
| 99 | iroh-workshop-omniopencon | https://github.com/n0-computer/iroh-workshop-omniopencon | Examples for the workshop about iroh at omniopencon.org in Bucharest | yes (repo is the crate) | 2024-09-27 | direct |
| 100 | iroh-thorium-reader | https://github.com/n0-computer/iroh-thorium-reader | (no description) | yes (repo is the crate) | 2024-09-25 | direct |
| 101 | async-channel | https://github.com/n0-computer/async-channel | Async multi-producer multi-consumer channel | - | 2024-07-29 | - |
| 102 | appa | https://github.com/n0-computer/appa | (no description) | - | 2024-07-29 | - |
| 103 | axum-server | https://github.com/n0-computer/axum-server | High level server designed to be used with axum framework. | - | 2024-06-12 | - |
| 104 | riblt-ts | https://github.com/n0-computer/riblt-ts | port of rateless invertible bloom filters to typescript | - | 2024-05-09 | - |
| 105 | actions-upload-release-asset | https://github.com/n0-computer/actions-upload-release-asset | Yet Another Upload Release Asset Action | - | 2024-04-29 | - |
| 106 | migrate-bao-store-redb | https://github.com/n0-computer/migrate-bao-store-redb | Tools for manual migration of the redb bao store used in iroh-bytes | - | 2024-03-25 | - |
| 107 | iroh-example-github-actions | https://github.com/n0-computer/iroh-example-github-actions | (no description) | yes (repo is the crate) | 2024-01-10 | - |
| 108 | discord-webhook-notify | https://github.com/n0-computer/discord-webhook-notify | Sends a notification to discord using a webhook URL. It is written in JavaScript so it will work with windows, osx, and linux execution environments. | - | 2024-01-09 | - |
| 109 | install-action | https://github.com/n0-computer/install-action | GitHub Action for installing development tools (mainly from GitHub Releases). | - | 2023-12-13 | - |
| 110 | iroh-js | https://github.com/n0-computer/iroh-js | iroh javascript/typescript client | yes (repo is the crate) | 2023-12-07 | direct |
| 111 | iroh-discovery-cloudflare-worker | https://github.com/n0-computer/iroh-discovery-cloudflare-worker | A rust cloudflare worker that supports the pkarr relay format | yes (repo is the crate) | 2023-10-18 | relay-boot |
| 112 | create-an-issue | https://github.com/n0-computer/create-an-issue | A GitHub Action for creating a new issue from a template file. | - | 2023-09-06 | - |
| 113 | metrics_exporter | https://github.com/n0-computer/metrics_exporter | (no description) | - | 2023-07-18 | - |
| 114 | iroh-sync | https://github.com/n0-computer/iroh-sync | (no description) | yes (repo is the crate) | 2023-06-22 | direct |
| 115 | waht | https://github.com/n0-computer/waht | (no description) | - | 2023-05-29 | - |
| 116 | bao-docs | https://github.com/n0-computer/bao-docs | Documentation for BAO verified streaming as used in the iroh ipfs implementation | - | 2023-04-20 | - |
| 117 | dag-cbor-references | https://github.com/n0-computer/dag-cbor-references | A zero dependency lib that can extract blake3 ipld links out of dag-cbor or cbor data. | - | 2023-04-20 | - |
| 118 | varint-bench | https://github.com/n0-computer/varint-bench | Benchmarks for Unsigned Variable Size Integer encodings | - | 2023-01-16 | - |
| 119 | attic | https://github.com/n0-computer/attic | Stuff we want to keep around for historic reasons, but don't want to have in the main iroh repo | - | 2023-01-11 | - |
| 120 | krakensync | https://github.com/n0-computer/krakensync | Sync experiment: Can we use bitmaps with memesync? | - | 2022-12-14 | - |
| 121 | discord_zerobot | https://github.com/n0-computer/discord_zerobot | (no description) | - | 2022-03-09 | - |

### Reverse dependencies of iroh

Source: `https://crates.io/api/v1/crates/iroh/reverse_dependencies`, pages 1 to 4, 306 dependent crates in total (the response reports 306). Ranked by the dependent version's `updated_at`. The top 60 are shown, with a cutoff at 2026-10-04. The "iroh crates used" column shows only the iroh dependency visible in that response, not the other iroh crates each project may use. Downloads are per dependent version. Crate downloads are the crate's total from its crate endpoint.

| # | Name | URL | Description | iroh crates used (visible) | Last activity | Downloads (dependent version) | Crate downloads (total) | Relevance tags |
|---|---|---|---|---|---|---|---|---|
| 1 | blvm-node | https://github.com/BTCDecoded/blvm-node | Bitcoin Commons BLVM: Minimal Bitcoin node implementation using blvm-protocol and blvm-consensus | iroh =0.95 | 2026-10-15 | 0 | 2711 | - |
| 2 | openrtc | https://github.com/bluestarburst/openrtc | OpenRTC: a Rust-first P2P runtime for device discovery, signaling, and iroh/QUIC networking. | iroh =1.2.0 | 2026-10-15 | 14 | 1987 | direct, relay-boot |
| 3 | openrtc-tauri-plugin | https://github.com/bluestarburst/openrtc | First-party Tauri v2 plugin for the OpenRTC native runtime. | iroh =1.2.0 (dev) | 2026-10-15 | 8 | 1469 | direct |
| 4 | web-transport-iroh | https://github.com/moq-dev/web-transport | WebTransport library for Iroh | iroh ^1 | 2026-10-14 | 16 | 162192 | direct |
| 5 | miden-validator | https://github.com/0xMiden/node | Miden validator | iroh =1.0.3 | 2026-10-14 | 10 | 1320 | - |
| 6 | rho-harness-core | https://github.com/casonadams/rho | rho host domain: deterministic workspace, session, config, and provider identity | iroh ^1.3 | 2026-10-14 | 14 | 733 | - |
| 7 | rho-engine | https://github.com/casonadams/rho | rho turn orchestration, provider assembly, tools, and MCP runtime | iroh ^1.3 | 2026-10-14 | 13 | 614 | - |
| 8 | rho | https://github.com/casonadams/rho | Minimal, clean agentic coding CLI harness in Rust | iroh ^1.3 (dev) | 2026-10-14 | 13 | 495 | - |
| 9 | sapphire-framework-bridge | https://github.com/sapphire-turtle/sapphire-framework | Host-wide sapphire daemon: device identity, workgroup authorization and a switchboard to app servers | iroh ^1.2 | 2026-10-14 | 71 | 286 | privnet |
| 10 | vgi-client | https://github.com/Query-farm/vgi-rust | Client for the VGI protocol — attach a remote catalog, discover its contents, and call its functions over Arrow IPC | iroh ^1.1.0 | 2026-10-14 | 10 | 247 | direct |
| 11 | decdn-common | https://github.com/decdn/decdn | Shared types for deCDN binaries: config schema, identity, CLI args | iroh ^1.3 | 2026-10-14 | 117 | 215 | privnet |
| 12 | decdn-incentive | https://github.com/decdn/decdn | The deCDN payment layer: USDC payment pools, off-chain EIP-712 vouchers, and the contract bindings that settle them | iroh ^1.3 | 2026-10-14 | 115 | 207 | - |
| 13 | decdn-client | https://github.com/decdn/decdn | The deCDN client SDK: fetch content-addressed blobs from deCDN nodes, pay per megabyte from a USDC payment pool, and verify every byte as it arrives | iroh ^1.3 | 2026-10-14 | 113 | 197 | direct |
| 14 | qualia-transport | https://github.com/Qualia2FA/QualiaTransport | Transport channels for Qualia2FA: QR code carriage and LAN discovery. | iroh ^1.3.0 | 2026-10-14 | 80 | 173 | direct, relay-boot |
| 15 | decdn-cache | https://github.com/decdn/decdn | Cache engine wrapping iroh-blobs with origin pull-through for deCDN | iroh ^1.3 | 2026-10-14 | 15 | 69 | direct |
| 16 | decdn-reputation | https://github.com/decdn/decdn | Local per-peer reputation scoring for deCDN nodes (ADR 008): EWMA over observed outcomes, with no gossip aggregation | iroh ^1.3 | 2026-10-14 | 15 | 69 | - |
| 17 | decdn-cli | https://github.com/decdn/decdn | User-facing deCDN CLI (decdn binary): probe, fetch, pools, publishing, origin import, node admin | iroh ^1.3 | 2026-10-14 | 12 | 58 | - |
| 18 | decdn-node | https://github.com/decdn/decdn | The deCDN node daemon (decdn-node): runtime, request handlers, payment settlement, and the admin RPC server | iroh ^1.3 | 2026-10-14 | 13 | 58 | direct |
| 19 | asimov-protocol | https://github.com/asimov-platform/asimov.rs | ASIMOV Software Development Kit (SDK) for Rust | iroh ^1.2 | 2026-10-13 | 1227 | 68593 | - |
| 20 | asimov-id | https://github.com/asimov-platform/asimov.rs | ASIMOV Software Development Kit (SDK) for Rust | iroh ^1.2 | 2026-10-13 | 1244 | 56212 | privnet |
| 21 | azula | https://github.com/Azula-App/azula-cli | Command-line companion for the azula p2p app: an MCP server for your phone, remote terminals with CI handoff, agent-drawn UI, and an offline-forwarding relay — all over direct iroh connections. | iroh ^1 | 2026-10-13 | 15 | 103 | direct, relay-boot |
| 22 | chorrent | https://github.com/mrsarthi/Chorrent | Peer-to-peer and swarm file transfer over Iroh/QUIC with verified, chunked downloads | iroh ^1.0.3 | 2026-10-13 | 15 | 78 | direct |
| 23 | heyo-sdk | https://docs.heyo.computer | Rust SDK for the Heyo cloud sandbox API. | iroh ^1 | 2026-10-12 | 11 | 809 | - |
| 24 | radicle-artifact-node | https://radicle.network/nodes/iris.radicle.network/rad:z4VYyJ9KuwMNkXGQnmKuGPGKw3inv | Seeding node for radicle artifacts: iroh-blobs store and blob serving daemon | iroh ^1.3.0 | 2026-10-12 | 14 | 188 | direct |
| 25 | raemote | https://github.com/raemote/raemote_cli | Private, account-free access to the web apps running on your own machine. | iroh ^1.2.0 | 2026-10-12 | 10 | 42 | direct |
| 26 | bevy_replicon_iroh | https://github.com/mitchty/bevy_replicon_iroh | Minimal iroh backend for bevy_replicon | iroh ~1.3 | 2026-10-11 | 17 | 55 | direct |
| 27 | iroh-socks5 | https://crates.io/crates/iroh-socks5 | A SOCKS5 proxy protocol over iroh | iroh =1.3.0 | 2026-10-11 | 11 | 26 | direct |
| 28 | mshr | https://github.com/yah-ai/mshr | Opinionated wrapper over iroh: per-machine Ed25519 identity, ALPN-multiplexed Endpoint, composed peer discovery (mDNS / iroh-relay swarm / pinned / external roster), and an embeddable relay server. | iroh ^1.3 | 2026-10-10 | 66 | 570 | privnet, relay-boot |
| 29 | xlb | https://github.com/yah-ai/xlb | Chunked, Bao-verified blob distribution with multi-source concurrent fetch (LAN + peer + edge), app-namespaced | iroh ^1.3 | 2026-10-10 | 14 | 371 | direct |
| 30 | tincan-chat | https://github.com/bilalyazicioglu/tincan-cli | Serverless peer-to-peer voice and text chat for your terminal | iroh ^1 | 2026-10-10 | 20 | 97 | chat, direct |
| 31 | walkie | https://github.com/Squadrant-AI/walkie | Walkie-talkie for AI agents: quick, end-to-end encrypted chat between any two agents. | iroh ^1 | 2026-10-10 | 10 | 10 | chat |
| 32 | vgi-iroh-transport | https://github.com/Query-farm/vgi-rpc-rust | Arrow-free native Iroh transport core for VGI clients | iroh =1.1.0 | 2026-10-09 | 236 | 3418 | direct |
| 33 | vgi-rpc-iroh | https://github.com/Query-farm/vgi-rpc-rust | Stateful authenticated Iroh transport for vgi-rpc | iroh =1.1.0 | 2026-10-09 | 226 | 3188 | direct |
| 34 | rag-rat-core | https://github.com/cq27-dev/rag-rat | Repository evidence engine for source chunks, symbols, graph edges, Git history, GitHub rationale, and source-bound memories. | iroh ^1.2.0 | 2026-10-09 | 17 | 2255 | - |
| 35 | rag-rat-sync | https://github.com/cq27-dev/rag-rat | Peer/p2p transport for the rag-rat op log: an iroh QUIC session that exchanges signed account entries between peers, feeding each through the op-log ingest seams (phase D, #406). | iroh ^1.2.0 | 2026-10-09 | 18 | 857 | direct |
| 36 | sivtr | https://github.com/Ariestar/sivtr | Terminal output workspace for the AI era - capture, browse, search, select, and reuse terminal output and AI coding sessions | iroh ^1.0 | 2026-10-09 | 37 | 632 | - |
| 37 | sendmer | https://github.com/bruceblink/sendmer | A cli tool to send directories over the network, with NAT hole punching | iroh ^1 | 2026-10-09 | 10 | 491 | direct |
| 38 | vgi-iroh-cabi | https://github.com/Query-farm/vgi-rpc-rust | Stable C ABI for the native VGI Iroh client transport | iroh =1.1.0 | 2026-10-09 | 10 | 275 | direct |
| 39 | vgi-rpc-iroh-browser | https://github.com/Query-farm/vgi-rpc-rust | Browser-capable client-side HTTP over a shared Iroh endpoint | iroh =1.1.0 | 2026-10-09 | 10 | 268 | direct |
| 40 | modelpipe | https://github.com/mmogr/modelpipe | Reach an OpenAI-compatible model server from anywhere over p2p — no VPN, no account, no cloud in the path | iroh ^1 | 2026-10-08 | 1099 | 6204 | direct |
| 41 | koh | https://github.com/gold-silver-copper/koh | koh — a resilient peer-to-peer remote shell over iroh, with predictive local echo | iroh =1.3.0 | 2026-10-08 | 18 | 388 | direct |
| 42 | agentaps | https://github.com/domenkozar/agentaps | Desktop client for agents that speak the Agent Client Protocol | iroh ^1.1 | 2026-10-08 | 12 | 52 | - |
| 43 | dog-transport | https://github.com/Jitpomi/dogrs | HTTP, gRPC, CLI and Iroh transports for DogRS | iroh ^1.0.0 | 2026-10-08 | 33 | 50 | direct |
| 44 | subduction_iroh | https://github.com/inkandswitch/subduction | Iroh (QUIC) transport layer for the Subduction sync protocol | iroh ^1.0 | 2026-10-07 | 198 | 1071 | direct |
| 45 | subduction_cli | https://github.com/inkandswitch/subduction | CLI server for Subduction sync over WebSocket, HTTP long-poll, and Iroh (QUIC) | iroh ^1.0 | 2026-10-07 | 13 | 360 | direct |
| 46 | blossom-lfs | https://github.com/MonumentalSystems/blossom-lfs | Git LFS daemon for Blossom blob storage with chunking and lock support | iroh ^1.3.0 | 2026-10-07 | 11 | 308 | - |
| 47 | pixel-change-check-client | https://github.com/carterlasalle/pixelchangecheck | Replicates your screen exactly and sends only the pixels that changed, over QUIC or a relay, to a native or browser viewer. | iroh ^1 | 2026-10-07 | 10 | 111 | direct |
| 48 | alink | https://github.com/dstotijn/alink | Encrypted, asynchronous agent-to-agent messaging over iroh | iroh ^1.3.0 | 2026-10-07 | 14 | 14 | chat |
| 49 | iroh-blobs | https://github.com/n0-computer/iroh-blobs | content-addressed blobs for iroh | iroh ^1.0.0 | 2026-10-06 | 21388 | 817001 | direct |
| 50 | burn-remote | https://github.com/tracel-ai/burn/tree/main/crates/burn-remote | Backend router decorator over the network. | iroh ^1.3.0 | 2026-10-06 | 1839 | 443726 | direct |
| 51 | sendme | https://github.com/n0-computer/sendme | A cli tool to send directories over the network, with NAT hole punching | iroh ^1.0.0 | 2026-10-06 | 108 | 32819 | direct |
| 52 | kobectl | https://github.com/kunobi-ninja/kobe | kobe — CLI for the kobe cluster-pool operator: lease, inspect and manage instant CI/dev Kubernetes clusters | iroh ^1 | 2026-10-06 | 10 | 679 | - |
| 53 | zim-peer | https://github.com/zim/zim | Zim peer: the network-participating runtime — sync orchestration, vault adapter, sqlite/memory vault logs, peer store, iroh wire protocol | iroh ^0.93 | 2026-10-06 | 17 | 111 | direct |
| 54 | mesh-llm-client | https://github.com/Mesh-LLM/mesh-llm | Low-level Rust client implementation for Mesh LLM embedded integrations | iroh ^1.0.3 | 2026-10-05 | 31 | 532 | direct |
| 55 | mesh-llm-routing | https://github.com/Mesh-LLM/mesh-llm | Shared routing targets and model routing helpers for Mesh LLM | iroh ^1.0.3 | 2026-10-05 | 33 | 447 | - |
| 56 | mesh-llm-protocol | https://github.com/Mesh-LLM/mesh-llm | Mesh LLM wire protocol types, constants, and frame helpers | iroh ^1.0.3 | 2026-10-05 | 32 | 442 | direct |
| 57 | gantz_collab | https://github.com/nannou-org/gantz | Peer-to-peer collaborative session networking for gantz. | iroh ^1 | 2026-10-05 | 30 | 266 | direct |
| 58 | mesh-llm-host-runtime | https://github.com/Mesh-LLM/mesh-llm | Host runtime orchestration for mesh-llm nodes | iroh ^1.0.3 | 2026-10-05 | 17 | 104 | - |
| 59 | gantz_collab_sync | https://github.com/nannou-org/gantz | The host-agnostic sync plane for gantz collaborative sessions: join, fetch, converge and announce over gantz_collab. | iroh ^1 (dev) | 2026-10-05 | 29 | 103 | direct |
| 60 | rlx-driver | https://github.com/MIT-RLX/rlx | RLX driver layer — device handles, arenas, buffers, command streams | iroh =1.0.0-rc.1 | 2026-10-04 | 92 | 6237 | - |

### Reverse dependencies of iroh-gossip

Source: `https://crates.io/api/v1/crates/iroh-gossip/reverse_dependencies`, one page, 38 dependent crates in total, all included. Ranked by the dependent version's `updated_at`. Every row carries the `gossip` tag because each crate depends on iroh-gossip. Other tags come from descriptions.

| # | Name | URL | Description | iroh crates used (visible) | Last activity | Downloads (dependent version) | Crate downloads (total) | Relevance tags |
|---|---|---|---|---|---|---|---|---|
| 1 | openrtc | https://github.com/bluestarburst/openrtc | OpenRTC: a Rust-first P2P runtime for device discovery, signaling, and iroh/QUIC networking. | iroh-gossip =0.101.0 | 2026-10-15 | 14 | 1987 | gossip, relay-boot |
| 2 | asimov-protocol | https://github.com/asimov-platform/asimov.rs | ASIMOV Software Development Kit (SDK) for Rust | iroh-gossip ^0.101 | 2026-10-13 | 1227 | 68593 | gossip |
| 3 | chorrent | https://github.com/mrsarthi/Chorrent | Peer-to-peer and swarm file transfer over Iroh/QUIC with verified, chunked downloads | iroh-gossip ^0.101.0 | 2026-10-13 | 15 | 78 | gossip, direct |
| 4 | gantz_collab | https://github.com/nannou-org/gantz | Peer-to-peer collaborative session networking for gantz. | iroh-gossip ^0.101 | 2026-10-05 | 30 | 266 | gossip, direct |
| 5 | vector-core | https://github.com/VectorPrivacy/Vector | Core library for Vector — the single source of truth for all Vector clients, SDKs, and interfaces. | iroh-gossip ^0.101 | 2026-10-01 | 54 | 817 | gossip, chat |
| 6 | iroh-mainline-endpoint-discovery | https://github.com/n0-computer/iroh-content-discovery | Publish and discover iroh endpoints through Mainline | iroh-gossip ^0.101 (dev) | 2026-09-30 | 43 | 43 | gossip, relay-boot |
| 7 | irosh | https://github.com/shedrackgodstime/irosh | Secure P2P SSH and data transfer protocol. Connect to any machine without an IP address, open ports, or VPNs. | iroh-gossip ^0.101.0 | 2026-09-21 | 54 | 204 | gossip, direct, privnet |
| 8 | mcpmesh-node | https://github.com/counterpunchtech/mcpmesh | Embed a full mcpmesh node in-process — the daemon core as a library | iroh-gossip =0.101.0 | 2026-09-15 | 27 | 2621 | gossip, direct, privnet |
| 9 | bevy_iroh | https://github.com/rvdende/bevy_iroh | Share Bevy entities between peers over iroh: replication, rooms, presence and typed messages, with optional voice and video | iroh-gossip ^0.101 | 2026-09-10 | 15 | 123 | gossip, direct, chat |
| 10 | mosaik | https://github.com/flashbots/mosaik | A Rust runtime for building self-organizing, leaderless distributed systems. | iroh-gossip ^0.101 | 2026-09-08 | 16 | 1069 | gossip, direct |
| 11 | mere-p2panda-net | https://github.com/p2panda/p2panda | Data-type-agnostic p2p networking, discovery, gossip and local-first sync | iroh-gossip ^0.101.0 | 2026-08-24 | 120 | 256 | gossip, relay-boot |
| 12 | p2panda-net | https://github.com/p2panda/p2panda | Data-type-agnostic p2p networking, discovery, gossip and local-first sync | iroh-gossip ^0.101.0 | 2026-08-21 | 560 | 10071 | gossip, relay-boot |
| 13 | peersey | https://github.com/antepodeum/Peersey | Batteries-included P2P messaging and content sharing | iroh-gossip ^0.99 | 2026-08-20 | 16 | 16 | gossip, chat |
| 14 | chio-federation-transport-iroh | https://github.com/backbay-labs/chio | Iroh federation-transport adapter: accept-time admission gate over an issuer-signed EndpointId directory (sits strictly under chio-federation, replaces no trust logic) | iroh-gossip ^0.101 | 2026-08-02 | 47 | 47 | gossip, privnet |
| 15 | guardian-db | https://github.com/wmaslonek/guardian-db/ | High-performance, local-first decentralized database that stores and processes data, built on Rust and Iroh | iroh-gossip ^0.101.0 | 2026-07-31 | 37 | 526 | gossip, direct |
| 16 | syncweb-core | https://github.com/chapmanjacobd/syncweb | delay-tolerant web surfing | iroh-gossip ^0.101.0 | 2026-07-20 | 16 | 207 | gossip, direct |
| 17 | tunnet-core | https://github.com/tunnetio/Tunnet | Core mesh runtime for Tunnet (identity, routing, streams, sync) | iroh-gossip ^0.101 | 2026-07-18 | 55 | 74 | gossip, direct, privnet |
| 18 | lait | https://github.com/Nixie-Tech-LLC/lait | A local-first, peer-to-peer, end-to-end-encrypted issue tracker built on iroh + Loro CRDTs | iroh-gossip ^0.100 | 2026-07-13 | 30 | 108 | gossip, direct |
| 19 | iroh-docs | https://github.com/n0-computer/iroh-docs | Iroh sync | iroh-gossip ^0.101.0 | 2026-06-15 | 90155 | 193427 | gossip, direct |
| 20 | iroh-smol-kv | https://github.com/n0-computer/iroh-smol-kv | A tiny replicated kv store that sync over an iroh-gossip topic | iroh-gossip ^0.101.0 | 2026-06-15 | 891 | 26343 | gossip, direct |
| 21 | distributed-topic-tracker | https://github.com/rustonbsd/distributed-topic-tracker | automagically find peers interested in a topic + iroh-gossip integration | iroh-gossip ^0.101 | 2026-06-15 | 12520 | 19521 | gossip, relay-boot |
| 22 | iroh-topic-tracker | https://github.com/rustonbsd/iroh-topic-tracker | Iroh universal (gossip) topic tracker based on @Nuhvi's mainline draft proposal: https://github.com/bittorrent/bittorrent.org/pull/174. | iroh-gossip ^0.101 | 2026-06-15 | 67 | 9738 | gossip, relay-boot |
| 23 | iroh-auth | https://github.com/rustonbsd/iroh-auth | Authentication middleware for iroh | iroh-gossip ^0.101 | 2026-06-15 | 35 | 202 | gossip, privnet |
| 24 | ssp-client | https://github.com/Thomas-995/ssp | Client library for SSP, a Slippi session protocol | iroh-gossip ^0.95.0 | 2026-06-05 | 24 | 24 | gossip, direct |
| 25 | cantrip-vtt | https://github.com/hafihaf123/cantrip | A decentralized, terminal-based Virtual Tabletop (VTT) for RPGs, powered by Iroh. | iroh-gossip ^0.95.0 | 2026-05-27 | 24 | 93 | gossip, direct |
| 26 | agent-swarm | https://github.com/agent-habilis/agent-swarm | P2P swarm network for AI agents | iroh-gossip ^0.98 | 2026-05-15 | 29 | 29 | gossip, direct |
| 27 | iroh-gossip-rendezvous | https://github.com/swaits/iroh-gossip-rendezvous | Passphrase-based rendezvous for iroh-gossip swarms via Mainline DHT | iroh-gossip ^0.99 | 2026-05-11 | 194 | 284 | gossip, relay-boot |
| 28 | donglora-bridge | https://github.com/donglora/bridge | Peer-to-peer LoRa bridge using iroh and DongLoRa | iroh-gossip ^0.99 | 2026-05-11 | 31 | 255 | gossip, direct |
| 29 | igc-net | https://github.com/igc-net/igc-net-rs | igc-net protocol rust library — publish and add metadata to IGC flight files | iroh-gossip ^0.97 | 2026-05-10 | 87 | 130 | gossip, direct |
| 30 | enlace | https://github.com/paltaio/enlace | Encrypted mailbox and latest-value slot fan-out. | iroh-gossip =0.98.0 | 2026-04-30 | 24 | 105 | gossip, direct |
| 31 | irohds-daemon | https://codeberg.org/cab-lab/irohds | Decentralized function memoization daemon (iroh P2P) | iroh-gossip ^0.97 | 2026-03-24 | 23 | 108 | gossip, direct |
| 32 | libmarathon | https://github.com/r3t-studios/marathon | A peer-to-peer game engine development kit with CRDT-based state synchronization | iroh-gossip ^0.95.0 | 2026-02-07 | 139 | 160 | gossip, direct |
| 33 | caretta-framework | https://github.com/fluo10/caretta-framework | A local-first application framework for lazy person | iroh-gossip ^0.95.0 | 2025-12-25 | 31 | 31 | gossip, direct |
| 34 | xaeroflux | https://crates.io/crates/xaeroflux | a decentralized, p2p, offline first event log built on tokio, rusqlite and iroh | iroh-gossip ^0.95 | 2025-11-24 | 46 | 10986 | gossip, direct |
| 35 | iroh-lan | https://github.com/rustonbsd/iroh-lan | iroh-lan = hamachi - account - install | iroh-gossip ^0.93 | 2025-10-18 | 217 | 482 | gossip, privnet, direct |
| 36 | vendimint | https://github.com/lightningvend/vendimint | Fedimint point-of-sale remote receive protocol | iroh-gossip ^0.34.1 | 2025-08-07 | 435 | 3741 | gossip, direct |
| 37 | iroh-gossip-discovery | https://github.com/therishidesai/iroh-gossip-discovery | Discover iroh nodes via iroh-gossip | iroh-gossip ^0.35.0 | 2025-07-03 | 548 | 2679 | gossip, relay-boot |
| 38 | iroh-cli | https://github.com/n0-computer/iroh | Bytes. Distributed. | iroh-gossip ^0.28.1 | 2024-11-04 | 1307 | 19130 | gossip, direct |

### Site showcase

Source: https://www.iroh.computer, read with WebFetch. The page has no projects or showcase page. The closest sections are a customer and project logo strip and a Learn section. The rows below come from a model-summarized page read, so each URL was taken from that summary, not from raw HTML. Use-case pages (`/solutions/vpn`, `/solutions/distributed-ai`, `/solutions/rave`, `/solutions/data-sync`, `/solutions/pos`, `/solutions/iot`) are product pages, not projects, and are not listed.

| # | Name | URL | Description | iroh crates | Last activity | Relevance tags |
|---|---|---|---|---|---|---|
| 1 | cmux | https://cmux.com/ | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 2 | Spacedrive | https://www.spacedrive.com | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 3 | Nous | https://nousresearch.com | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 4 | meshllm | https://meshllm.cloud | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 5 | Strada | https://strada.tech | Logo on the iroh.computer homepage; also in the awesome list as #47 (File Sharing). | n/a | see awesome list | - |
| 6 | Paycode | https://www.paycode.com.mx | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 7 | Rave | https://rave.io | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 8 | Block (blog post) | https://iroh.computer/blog/buzz-agent-workspaces | Blog post linked from the logo strip, not a project page. | n/a | not fetched (website) | - |
| 9 | delta.chat | https://delta.chat | Logo on the iroh.computer homepage; also in the awesome list as #65 (Social Media). | n/a | see awesome list | chat |
| 10 | Ottomatic | https://ottomatic.io | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 11 | Rayfish | https://rayfish.xyz | Logo on the iroh.computer homepage; also in the awesome list as #27 (DevTools). | n/a | see awesome list | privnet, relay-boot |
| 12 | Outl | https://outl.app | Logo on the iroh.computer homepage; the page gives no description. | n/a | not fetched (website) | - |
| 13 | Datum | https://www.datum.net | Logo on the iroh.computer homepage; also in the awesome list as #21 (DevTools). | n/a | see awesome list | direct, relay-boot |
| 14 | Fedimint | https://fedimint.org | Logo on the iroh.computer homepage; also in the awesome list as #75 (Web3). | n/a | see awesome list | relay-boot |
| 15 | Dumbpipe (learn section) | https://dumbpipe.dev | Learn-section link; the n0 repo of the same name is in its own table. | n/a | see n0 repos table | direct |
| 16 | Sendme (learn section) | https://iroh.computer/sendme | Learn-section link to the sendme tool; the n0 repo is in its own table. | n/a | see n0 repos table | direct |

## Statistics

Entries per category:

| Category | Entries listed | Source total | Note |
|---|---|---|---|
| Awesome list | 77 | 77 | Chat-room link under Follow excluded. |
| n0-computer repos | 121 | 129 | 8 archived repos excluded. |
| Reverse dependencies of iroh | 60 | 306 | Top 60 by recent activity; cutoff 2026-10-04. |
| Reverse dependencies of iroh-gossip | 38 | 38 | Complete. |
| Site showcase | 16 | 16 | Logo strip and Learn-section links. |
| Total rows | 312 | | |
| Deduplicated total | 290 | | Union by normalized name. Approximate: two distinct projects with one name would merge. |

Relevance tag counts per category:

| Tag | Awesome | n0 | iroh rev. deps | iroh-gossip rev. deps | Site | Total rows |
|---|---|---|---|---|---|---|
| gossip | 6 | 3 | 0 | 38 | 0 | 47 |
| relay-boot | 15 | 12 | 4 | 8 | 3 | 42 |
| direct | 41 | 43 | 36 | 25 | 3 | 148 |
| chat | 8 | 2 | 3 | 3 | 1 | 17 |
| privnet | 7 | 3 | 4 | 6 | 1 | 21 |

Categories list: Awesome list; n0-computer repos; Reverse dependencies of iroh; Reverse dependencies of iroh-gossip; Site showcase.

## Fetch notes

- The awesome list README is `readme.md`, not `README.md`. The `README.md` raw URL returns 404.
- Awesome list, non-GitHub entries: Codeberg fetched for two (omry, iroh Retro). The GitLab APT repository returns 404 through the API and is listed as unreachable. Six websites (Oku, Strada, Dash Chat, Delta Chat, Rostra, Weird) have no repository to query, so last activity is not fetched.
- Awesome list, "none at root" means a root Cargo.toml exists with no iroh dependency. Projects with iroh only in subdirectories show no crate here. "no root Cargo.toml" means the project is not a Rust crate at its root.
- The reverse-dependency response shows only the queried iroh crate. Other iroh crates a dependent uses are not shown, so the "iroh crates used" column lists only the queried crate.
- Download columns: the dependent-version downloads come from the reverse-dependency `versions` objects. Crate totals come from each crate's own endpoint, fetched for the 98 crates in the two shown tables.
- crates.io was queried at about one request per second, as its rate policy asks.
- n0 repo tags come from name and description only. No code was scanned.
- Relevance tags are assigned from descriptions and visible crates. They are not verified against source code. The next round judges them.
- The site showcase was read through a model-summarized fetch, so URLs come from that summary, not from raw HTML.
- Rows without visible descriptions carry an explicit note in the table rather than a made-up description.
