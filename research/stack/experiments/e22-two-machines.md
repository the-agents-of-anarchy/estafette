# E22 two machines through NAT, relay over HTTPS

Date 2026-10-21. Branch `spike/e22-two-machines`, worktree `/Users/ryzhakar/pp/estafette-e22`, package `spikes/e22-two-machines/`, commit `966d653` (pushed to origin). Raw logs: `spikes/e22-two-machines/logs/` (A's public address replaced by `A_PUBLIC`). Reports not committed in the research repo, as asked.

## Answer

- HTTPS: low-effort, yes. `iroh-relay` 1.3.0 with a 10-line TOML (`cert_mode = "LetsEncrypt"`, hostname `178-105-88-14.sslip.io`) got a trusted Let's Encrypt production certificate 9.15 s after first start, no failure, ports 80 + 443 TCP only. A client with default trust roots (no CA config) connected; macOS `curl` verified it too (ssl_verify_result 0). Restart with the cache loads the certificate in 2 ms.
- Two machines through a real NAT work: A (192.168.0.x behind a home router, public address seen by B differs) to B and B to A by id + relay URL: 20/20 over relay-only, 20/20 upgraded to a direct path in both directions (median 190 ms A to B, 211-233 ms B to A). Gossip 120 s: 120/120 messages both ways, longest gap 1.08 s, over HTTP and over HTTPS.
- HTTPS cost against plain HTTP, same machines: +87 ms to come online (501 vs 414 ms, relay-only), nothing measurable in connect, echo, upgrade, steady round trip, gossip. A relay without QUIC address discovery (the default) costs 3.1 s to come online whenever IP transports are on; `enable_quic_addr_discovery = true` (needs the TLS config, UDP 7842) brought it to 0.49 s.
- Wire: plain HTTP shows `GET /relay`, `Host`, `x-iroh-challenge: ts_<hostname>`, `sec-websocket-protocol: iroh-relay-v2, iroh-relay-v1`, then binary frames. HTTPS shows a TLS handshake whose only readable string is the hostname (SNI).

## Setup

Machines. A: this Mac (Darwin 24.3.0 arm64), 192.168.0.x behind router 192.168.0.1, public IPv4 A_PUBLIC (`curl ifconfig.me`), no IPv6 default route on en0. B: Hetzner VM `estafette-lab`, 178.105.88.14, Ubuntu 24.04, 2 vCPU, ssh as root, work dir `/root/e22`. RTT A to B about 40 ms (selected-path RTT 39-47 ms in the tables).

Versions. Rust 1.99.1. iroh =1.3.0, iroh-gossip =0.101.0, preset `Minimal`. Relay: `cargo install iroh-relay --version =1.3.0 --features server --locked --root /root/e22/relay-bin` on B (release build, 2 m 49 s on 2 vCPU). Program: `spikes/e22-two-machines`, release build on both machines, same source (tarball, not git bundle; `git bundle create /tmp/e22/e22.bundle main..spike/e22-two-machines` was made but the source was shipped as `src.tgz` since B needs three files).

Hostname. `178-105-88-14.sslip.io` resolves to 178.105.88.14 (`dig @1.1.1.1`, `dig @8.8.8.8`, `nslookup`, `getent hosts` on B). Plain `dig +short` on A returned empty once (local resolver); the other four resolved.

Relay config, HTTPS (`logs/relay-https.toml`):

```
http_bind_addr = "0.0.0.0:80"
enable_relay = true
enable_metrics = false
[tls]
https_bind_addr = "0.0.0.0:443"
hostname = "178-105-88-14.sslip.io"
cert_mode = "LetsEncrypt"
cert_dir = "/root/e22/certs"
contact = "estafette-lab@178-105-88-14.sslip.io"
prod_tls = true
```

Run: `RUST_LOG=info,tokio_rustls_acme=debug,iroh_relay=debug /root/e22/relay-bin/bin/iroh-relay --config-path /root/e22/relay-https.toml`. QAD variant adds `enable_quic_addr_discovery = true` (top level). HTTP control: only the first three lines, port 80, no `[tls]`.

Source quoted (iroh-relay-1.3.0, `src/main.rs`, `/Users/ryzhakar/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`):
- `main.rs:412` "Possible options: 'Manual', 'LetsEncrypt'." (`cert_mode`); `main.rs:430-446` `prod_tls` default `true`, `contact` used for LetsEncrypt.
- `main.rs:639-647` `CertMode::LetsEncrypt`: bails if no hostname ("LetsEncrypt needs at least one hostname") or no contact ("LetsEncrypt needs a contact email"); `AcmeConfig::letsencrypt(tls.prod_tls)` with `.cache_path(tls.cert_dir())`; env `IROH_RELAY_ACME_CA` adds a trusted CA for the ACME server (pebble testing).
- `server.rs:742-790` the ACME state machine is spawned as a task of the server; HTTPS on `https_bind_addr`, plain HTTP port serves only a captive-portal service (`server.rs:798-815`), so the relay itself is only on 443 in TLS mode.
- `tokio-rustls-acme-0.9.1/src/lib.rs:1-3` validation is tls-alpn-01 on the HTTPS port; lines 8-9 "acquisition/renewal process is folded into the streams and futures being polled" (renewal exists in the library; not observed here, cert is 90 days).
- `iroh-relay-1.3.0/src/relay_map.rs:272-279` `RelayMap::from(url)` sets `quic: quic_config()` (UDP 7842, address discovery on); `RelayConfig::new(url, None)` drops it.

Program (`src/main.rs`): `serve` (long-lived endpoint, key from seed, echo ALPN + gossip), `dial` (n cold dials: fresh endpoint and random key each, `connect` with `EndpointAddr{id, relay url}` only, one echo, watch `conn.paths()` every 50 ms up to `hold_s` for a selected IP path, 10 steady echoes, close), `gossip` (join topic via the peer, heartbeat 1/s for 120 s, log gaps over 2 s, side echo connection logs path changes). Switches: `E22_NOPORTMAP` (portmapper off), `E22_NOQAD` (relay map without QUIC address discovery). "By id" = dial `EndpointAddr` with id + the relay URL, no direct addresses and no lookup service (E1 9b: id alone fails).

Per-run commands (A dials B): `e22-two-machines dial <relay_url> 22 20 relay-only 0` (R1), `... 22 20 direct 10` (R2); B dials A: A runs `serve <relay_url> 21 direct`, B runs `dial <relay_url> 21 20 direct 10` (R3); `gossip <relay_url> 22 0 120` on B and `gossip <relay_url> 21 22 120` on A (R4). Order of runs: HTTP control first (R1, R2, R3, R4), then HTTPS.

## Results

### Q1 Certificate (HTTPS, ACME, Let's Encrypt production)

| Step | Time (B clock, UTC, 11:19) | Since start |
|---|---|---|
| Process start | :29.03 | 0 |
| HTTPS listener up on :443 | :29.03 | 0.00 s |
| Order placed (status pending) | :31.21 | 2.2 s |
| Challenge triggered (tls-alpn-01) | :32.00 | 3.0 s |
| First validation request received | :33.15 | 4.1 s |
| All authorizations valid | :34.71 | 5.7 s |
| CSR sent | :35.61 | 6.6 s |
| Certificate downloaded and deployed (`DeployedNewCert`) | :38.17 | 9.15 s |
| Restart with cache: `DeployedCachedCert` | 11:31:27.256 | 2 ms after "serving" |

Four validation connections from AWS-range addresses arrived within 0.5 s. Issuer CN=YE1 (Let's Encrypt), notBefore 2026-10-21 10:21:06, notAfter 2027-01-19 10:21:05, SAN `178-105-88-14.sslip.io`. `curl https://178-105-88-14.sslip.io/healthz` on A: HTTP 200, TLS connect 92 ms. Failures: none in ACME. Self-signed fallback was not needed and not run.

### Q2/Q3 Dials (n = 20 each; values min/median/max in ms)

R1: relay-only (IP transports cleared on both), A dials B, cold endpoint each try.

| Relay | ok | online (bind to home relay connected) | connect | echo (connect + 1 round trip) | steady echo (median of 10) |
|---|---|---|---|---|---|
| HTTP :80 (control) | 20/20 | 408/414/603 | 40/41/43 | 84/88/98 | 38/39/40 |
| HTTPS :443 | 20/20 | 492/500/510 | 39/41/43 | 84/87/93 | 38/39/40 |

R2: direct allowed (IP transports on), A (behind NAT) dials B, 10 s watch.

| Relay | ok | final path is IP | online | connect | upgrade to selected IP path (from dial start) | steady echo |
|---|---|---|---|---|---|---|
| HTTP, default relay map | 20/20 | 20/20 | 3127/3129/3136 | 39/41/43 | 188/190/231 | 37/38/40 |
| HTTPS, default relay map | 20/20 | 20/20 | 3128/3133/3144 | 40/41/44 | 186/191/195 | 37/38/39 |
| HTTPS + QAD on relay | 20/20 | 20/20 | 485/493/514 | 40/41/43 | 187/190/194 | 37/38/40 |
| HTTP, clients `E22_NOQAD` | 20/20 | 20/20 | 406/412/416 | 39/41/43 | 187/191/200 | 37/38/39 |

All 80 connections started on the relay path (`at_connect=relay*`) and moved to `ip(178.105.88.14:<port>)` (B's public UDP address).

R3: B (public) dials A (behind NAT), A long-lived listener, B cold endpoint each try.

| Relay | ok | final path is IP | online (B) | connect | upgrade | steady echo | selected IP address |
|---|---|---|---|---|---|---|---|
| HTTP | 20/20 | 20/20 | 3004/3005/3006 | 40/42/148 | 188/233/314 | 37/38/40 | A_PUBLIC:62907 |
| HTTP, A portmapper off | 20/20 | 20/20 | 3004/3005/3006 | 40/42/159 | 188/192/316 | 37/38/39 | A_PUBLIC:52168 |
| HTTPS | 20/20 | 20/20 | 3007/3008/3012 | 40/42/135 | 186/211/288 | 37/38/39 | A_PUBLIC:54803 |

In each R3 run the selected port equals A's local UDP port (`lsof` on A: `UDP *:62907` and `*:52168`), with the portmapper on and off: the router preserved the source port. The direct address was A's public address, not its LAN address.

R4: gossip, one topic, heartbeat 1/s each side for 120 s, A bootstrap peer = B (id + relay URL in a `MemoryLookup`).

| Relay | joined (A, from subscribe) | A to B / B to A received | distinct | longest gap | gaps over 2 s | path of side echo connection |
|---|---|---|---|---|---|---|
| HTTP | 105 ms | 120/120 and 120/120 | 120, 120 | 1083 / 1011 ms | 0 / 0 | relay at 42 ms, relay+IP selected at 244 ms |
| HTTPS | 91 ms | 120/120 and 120/120 | 120, 120 | 1050 / 1016 ms | 0 / 0 | relay at 42 ms, relay+IP selected at 244 ms |

(Sent 120 each; B received A's 120 and A received B's 120. "Joined" counts from subscribe, after `online()`.)

### Q4 Wire (`tcpdump -c 30 -A -s 300`, packets from A only, on B's relay port, one relay-only dial)

| Relay | Visible |
|---|---|
| HTTP :80 | `GET /generate_204` with `x-iroh-challenge: ts_178-105-88-14.sslip.io`; `GET /ping`; `GET /relay` with `Host`, `Upgrade: websocket`, `sec-websocket-protocol: iroh-relay-v2, iroh-relay-v1`; afterwards binary WebSocket frames (104, 1242 and 2610-byte segments, not parsed) |
| HTTPS :443 | TCP handshake, then TLS segments only; the one readable string is `178-105-88-14.sslip.io` (SNI). No HTTP line visible. 21 extra short TCP connections from A closed before the TLS handshake (relay log "tls handshake eof", level ERROR) |

Header-only extracts: `logs/wire-http-headers.txt`, `logs/wire-https-headers.txt`. The payload bytes of the binary frames were not decoded, so what a passive observer learns from them (for example the node id in the relay handshake) is not measured here.

## What could make this wrong

- One pair of machines, one home router, one ISP, one afternoon; B is a public host. The NAT is port-preserving. Whether its filtering is endpoint-independent or address-dependent was not measured (A sent punch packets to B before B's packets arrived, so both fit). Symmetric NAT, CGNAT, and two NATs were not tested. Result is "this NAT is traversable", not "NATs are".
- The relay is co-located with B (RTT A to relay about 40 ms equals A to B), so relay and direct latency look the same (38 ms steady against 38-45 ms). The runs prove path selection, not a latency gain.
- A dials B direct "upgrade" is hole punching toward a public host: A's own outbound packets create the mapping. The inbound case is R3 only.
- `upgrade_ms` is polled at 50 ms and measured from dial start; `online_ms` includes DNS and the first relay connection of a cold endpoint. Clocks of A and B were not compared; no cross-host latency was derived from timestamps.
- Gossip max gap of about 1 s means no loss at 1 Hz; 120 s and one run per relay type say nothing about NAT mapping expiry (typically minutes) or relay reconnect.
- Release build here, debug build in E8; times are not comparable to earlier experiments. `online_ms` of 3.0 to 3.1 s with IP transports on and the relay lacking QAD is a probe timeout; I did not read the timeout from source, only measured that QAD on (relay) or off (client) removes it.
- HTTPS "cost" is one 40 ms-RTT path: +87 ms online equals about two RTTs (TLS 1.3 handshake plus relay request). On a longer path it grows with RTT; per-message cost over an established WebSocket was not separated from noise.
- sslip.io is a shared domain. Production Let's Encrypt rate limits per registered domain (and duplicate-certificate limits) were not measured; one certificate was issued once. A real deployment should own its hostname; nothing here shows how sslip.io behaves at volume.
- Renewal was not observed (cert is 90 days). Only the library text says renewal is folded into the polled stream.
- The cert cache file `cert_dir/cached_cert_*` held the private key and was created mode 644 (I set 600). Verified by `grep -c "PRIVATE KEY"` = 2 lines, contents not printed.
- Self-signed fallback not run. From source only: `CertMode::Manual` plus clients with an extra root (`CaTlsConfig::default().with_extra_roots(...)`, main.rs:665-666) would be needed; members would have to ship and trust that root. Cost is untested.
- ERROR-level log lines in normal HTTPS operation: 49 `ERROR` lines in one relay log of about 7 minutes (11:31 to 11:38), 41 of them "TLS[acme] handshake / tls handshake eof", 21 from A's early-closed probe connections and the rest from other internet hosts. Not failures of any dial, but noisy for anyone alerting on ERROR.
- Debug-level relay logging was on for all runs (including per-connection lines), which could add small server-side latency; not measured against info level.
- The first-start relay log was overwritten by the cached start; the Q1 timeline is copied from the console output during the run (`logs/acme-first-run.txt`), not from a retained file.
- QAD on a plain-HTTP relay was not tested: in my run the QUIC listener sat beside the TLS config, and I did not read the source to see whether HTTP-only servers can enable it. The HTTP control therefore ran with clients that drop QAD (`E22_NOQAD`) for the like-for-like online time (412 ms).

## Cleanup done

- B: all `e22-two-machines serve|gossip|dial`, `iroh-relay` and `tcpdump` processes stopped (checked with `pgrep -af`: none); ports 80, 443, UDP 7842 not listening (`ss`). A: no `e22-two-machines` process.
- Left on B in `/root/e22`: `relay-bin/` (iroh-relay binary, 13 MB), `target-prog/` (677 MB), `cargo-home/` (247 MB), `src/`, scripts, run logs (`relay-https.log`, `relay-https-qad.log`, `wire-*.txt`), and `certs/` with the Let's Encrypt account and certificate cache (mode 600; valid to 2027-01-19, reusable by restarting the relay). Free disk on B about 4.2 GB at the end. `/root/e22/target` (the relay build's target dir) was removed.
- Not touched: `/home/lab`, lab-sibling files, tmux, processes. No token, key or credential printed or copied; the ssh key was used only through `-i`.
- A: logs in `/tmp/e22/logs` (raw), worktree `/Users/ryzhakar/pp/estafette-e22`.
