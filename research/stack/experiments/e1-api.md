# E1 API spike

Date 2026-10-16. Rust 1.99.1 (`rustup run 1.99.1 cargo`), macOS arm64 host, debug build, one run per check.
Versions: iroh =1.3.0, iroh-gossip =0.101.0, iroh-relay =1.3.0 (feature `server`), iroh-mdns-address-lookup =0.6.0, tokio 1 (rt, rt-multi-thread, macros, time, net, signal). Glue deps added: `bytes`, `n0-future` (both already in the tree).
Commit `d18ff81` on `spike/e1-api` (worktree `/Users/ryzhakar/pp/estafette-e1-api`). Package `spikes/e1-api/`, excluded from workspace via `exclude = ["spikes"]`. Run: `cd spikes/e1-api && ./target/debug/e1-api [1|relay|5|6|8|9|all]`.
Endpoints use preset `Minimal` (no n0 DNS/pkarr lookup, no default relays), not `N0`.

| # | Check | Result | Observed | Code (path:line) |
|---|-------|--------|----------|------------------|
| 1 | Endpoint with given SecretKey; `SecretKey::sign`, `PublicKey::verify` | PASS | `ep.id() == sk.public()`. Sig 64 bytes. Valid sig verifies. Tampered message rejected. Wrong key rejected. No separate `verify_strict`: `PublicKey::verify` calls `verify_strict` internally (iroh-base-1.3.0 key.rs:134). | spikes/e1-api/src/main.rs:100-122 |
| 2a | Relays disabled, IP-only exchange | PASS | `RelayMode::Disabled`, dial by `EndpointAddr` with `127.0.0.1:port`. Echo of 12 bytes returned. | main.rs:148-172 |
| 2b | `clear_ip_transports()` + custom relay: relay-only connect and one message | PASS | Callee `addr()` had 0 IP addrs and 1 relay URL. Echo returned. Path list: one path, `relay:http://127.0.0.1:<port>/`. Dialer supplied the relay URL in the `EndpointAddr`. | main.rs:174-205 |
| 3 | Router with custom ALPN + gossip ALPN on one endpoint | PASS | Both ALPNs connect (gossip ALPN reads back `/iroh-gossip/1`). Unknown ALPN rejected: `peer doesn't support any known protocol`. | main.rs:174-197 |
| 4 | `remote_id()` on accepted connection equals dialer key | PASS | Match over the IP path and over the relay path. | main.rs:166, 203 |
| 5a | mDNS on builder before bind | PASS (compiles) | `.address_lookup(MdnsAddressLookup::builder())` works because the builder implements `AddressLookupBuilder`. `address_lookup()?.len()` is 1 after bind. | main.rs:211-226 |
| 5b | mDNS added after bind | PASS (compiles) | `MdnsAddressLookup::builder().build(ep.id())?` then `ep.address_lookup()?.add(m)`. Services 0 to 1. | main.rs:228-241 |
| 5c | Two local endpoints discover each other, no other address info | PASS | One endpoint via builder, other via add-after-bind, relays disabled. Id-only dial connected in 0.72 s. Echo returned. `remote_id` matched. | main.rs:243-272 |
| 6 | `home_relay_status()` / `watch_addr()` while relay stopped and restarted | PASS (emits) | See "Check 6 trace" below. | main.rs:276-312 |
| 7a | `Server::spawn`, plain HTTP, 127.0.0.1 | PASS | `RelayConfig::new((LOCALHOST, 0))`, `tls: None`. `http_addr()` is `Some`, `https_addr()` is `None`. | main.rs:81-85, 128-131 |
| 7b | Endpoints use it via `RelayMode::Custom(RelayMap::from(url))` | PASS | `online()` returned within 10 s. Status `Connected`. `addr()` lists the relay URL plus an IP. No insecure/CA setting needed for `http://`. | main.rs:133-146 |
| 8 | Gossip `max_message_size(8192)`, 2 endpoints, 5000-byte broadcast | PASS | `Gossip::max_message_size()` reports 8192. 5000 bytes received, byte-identical. | main.rs:316-362 |
| 8b | Control: default config, 5000 bytes | observation | Default max reports 4096. `broadcast` returned `Ok`. Message never arrived in 10 s. Silent loss. | main.rs:351-362 |
| 8c | Control: max 8192, 9000 bytes | observation | `broadcast` returned `Ok`. Never arrived in 10 s. Silent loss. | main.rs:351-362 |
| 9a | Id-only dial, relays disabled, no lookup | FAIL (connect) | Error after 148 microseconds: `No addressing information available`. | main.rs:365-381 |
| 9b | Id-only dial, both on shared relay, no lookup | FAIL (connect) | Error after 135 microseconds: `No addressing information available`. A shared relay does not resolve ids. | main.rs:383-391 |
| 9c | Id-only dial, shared relay + mDNS | PASS | Connected in 0.74 s. Selected path is `ip:192.168.1.4:<port>` (LAN IP), not the relay. | main.rs:393-400 |
| 9d | Id-only dial, relays disabled + mDNS | PASS | Connected in 0.74 s. Path is `ip:192.168.1.4:<port>`. | main.rs:402-410 |
| 9e | Id + relay URL only (no direct addrs, no mDNS), relays and IP transports on | PASS | Connected in 4.8 ms via relay. Path list at connect+3 s: relay path 0 and `ip:192.168.1.4:<port>` path 1. Selected path 1 (IP): the relay path upgraded to a direct LAN path. | main.rs:416-441 |
| 9f | Same with `clear_ip_transports()` on both | PASS | Connected in 4.3 ms. Only path: `relay:http://127.0.0.1:<port>/`, selected. | main.rs:416-441 |
| 10 | Dependency count, swarm-discovery version | n/a | `cargo tree --prefix none -e normal \| sort -u \| wc -l` = **416**. Same 416 without glue deps. swarm-discovery **0.6.4** (Cargo.lock). Cargo.lock holds 428 packages. | spikes/e1-api/Cargo.toml, Cargo.lock |

### Check 6 trace (relay stopped at 2.0 s, restarted at 12.0 s, same port)
- t=0: `home_relay_status` emits `connected:true`. `watch_addr` emits relay URL + 1 IP.
- 2.0 s to 9.6 s: `home_relay_status` emits 20 updates. Each pair is `connected:false err:None` then `connected:false err:Some("Failed to connect to relay server")`. Retry gaps grow: 0.0, 0.0, 0.0, 0.1, 0.3, 0.6, 1.1, 2.0, 3.4 s.
- `watch_addr` emitted nothing after t=0. It still lists the dead relay URL until the end of the run.
- 18.3 s (6.3 s after restart): `connected:true`. Final `online`: true.
- Total 27 events.

## Surprises
- `Server::http_url()` is gated behind iroh-relay feature `test-utils`. With only `server`, build the URL from `http_addr()` by hand (helper `http_url`, main.rs:428).
- `ServerConfig` and `RelayConfig` are `#[non_exhaustive]`. Construct with `ServerConfig::default()` plus field assignment and `RelayConfig::new(addr)`.
- A shared relay does not act as an address lookup. Id-only dial fails instantly without a lookup service (9b). Estafette needs a ticket, a lookup, or mDNS for first contact.
- Id plus relay URL is enough to connect (9e, 9f), unlike id alone (9b). The relay URL must be in the `EndpointAddr`; the endpoint does not learn it from the shared relay. With IP transports on, the connection upgraded from relay to a direct LAN path within 3 s.
- Gossip drops oversize messages silently. `broadcast` returns `Ok` and the receiver gets nothing (8b, 8c). Default limit is 4096, not 8192. Sender must enforce its own size cap.
- `watch_addr` does not reflect relay loss. Use `home_relay_status` to detect it. `home_relay_status` is noisy: two updates per retry, alternating `err` None/Some.
- Reconnect after relay restart took 6.3 s, from backoff.
- mDNS with relays disabled published the LAN IP (192.168.1.4). Connections in 9c/9d used it, not loopback and not the relay, even with a relay configured.
- `PublicKey::verify` is already strict. No need to call anything else.
- `Router::accept` handlers run per connection on a spawned task. The handler must hold the connection open (here `conn.closed().await`) or the peer sees a close before reading.

## What could make this wrong
- One run per check on one macOS host. Flakiness unmeasured. Timings are single samples.
- Debug build only.
- Preset `Minimal`, not `N0`. With `N0`, id-only dial would also query n0 DNS/pkarr. 9a and 9b could differ on a networked host. The sandbox firewall blocks outbound calls, so that path was never exercised.
- mDNS used custom service name `e1spike`, not the default `irohv1`. Other agents on the same LAN could not interfere, but default-name behaviour is untested. mDNS success depends on this host having a multicast-capable LAN interface. A host without one may fail 5c, 9c, 9d.
- Gossip check 8 ran with relays disabled and `MemoryLookup`, direct IPs. Not over the relay-only path.
- 8b and 8c loss was observed within a 10 s window. A slower delivery path is not excluded. I did not read gossip source to confirm the drop is deliberate.
- Check 6 covers one stop/restart cycle on loopback. Real relay failure modes (TLS, DNS, partial network) differ. Relay was plain HTTP; HTTPS path untested.
- Check 4 compared ids only. It does not prove the id is authenticated against a forged dialer. That rests on iroh's TLS handshake, not tested here.
- Count 416 is the literal command output. It includes `(*)` repeat markers and the package line. Stripping markers and dedup gives 315 lines (314 dependencies). Feature set was default features of each crate. A leaner feature set would shrink it.
- The tree count covers `-e normal` only. Build and dev deps excluded.
- "PASS" for 8b/8c and 9a/9b labels in the program output are informational. The table classifies them by the question asked.
- Line numbers refer to commit d18ff81 `spikes/e1-api/src/main.rs`. Ranges are approximate to function bounds.
