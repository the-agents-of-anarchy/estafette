# E6 address lookup by id

Date 2026-10-16. Commit `ebefd1f` on `spike/e6-lookup` (worktree `/Users/ryzhakar/pp/estafette-e6-lookup`, pushed to origin). Package `spikes/e6-lookup/`, excluded from the workspace (`exclude = ["spikes"]`). Run: `cd spikes/e6-lookup && E6_ATTEMPT_MS=3000 ./target/debug/e6-lookup <n0|stale|mdns|presence|selfhost|multi> [runs]`; probes: `online`, `publish`, `probe`. `selfhost` needs a running `iroh-dns-server` (dev config, `[mainline] enabled = false`).

Versions: iroh =1.3.0, iroh-gossip =0.101.0, iroh-relay =1.3.0 (feature `server`), iroh-mdns-address-lookup =0.6.0 (swarm-discovery 0.6.4), iroh-dns-server 1.3.0, postcard 1.1.3. rustc 1.99.1, debug build. Cargo.lock copied from e1-api.

Machine: Apple M2 Max, macOS 15.3.1 (Darwin 24.3.0 arm64), LAN address 192.168.1.4. Network: normal internet. `curl https://dns.iroh.link/` and the n0 relay host returned 200. n0 PROD pkarr/DNS (`https://dns.iroh.link/pkarr`) and n0 relay `euc1-1.relay.n0.iroh.link` were used by the `N0` preset. All endpoints (dialer A, node B) run in one process on one host. B restart = `Router::shutdown()` (graceful close), then rebind with the same `SecretKey` on port 0 (new port every time; checked: old and new ports differ in every ROW).

Definitions. A dials B by `EndpointAddr::from(id)` only, in a loop: `connect` with per-attempt timeout T (default 3000 ms, env `E6_ATTEMPT_MS`), then one echo round trip, 10 ms between failed attempts. Clock starts when B's `bind()` returns (first, restart). Controls:
- `first-settled`: a second dialer A2 that never saw B dials after a 5 s (n0, selfhost), 3 s (mDNS), 2 s (stale) wait.
- `restart-settled`: B restarts again (same key, new port); A dials after the same wait.

## Results

Series T=3000 ms, n=3 runs per method, min/median/max over successful runs (ok/n shown). Path = what the connection used at echo completion (`*` = selected). Raw logs: `/tmp/e6-T3000/*.log`, `/tmp/e6-mdns6.log` (not committed).

| Method | Measure | ok/n | min ms | median ms | max ms | Path |
|---|---|---|---|---|---|---|
| 1. n0 preset (relay on) | first connect, A knows only id | 3/3 | 3669 | 3677 | 3770 | n0 relay (1 of 3 also LAN IP) |
| | first-settled (fresh dialer) | 3/3 | 440 | 478 | 498 | relay |
| | restart, new port | 3/3 | 1225 | 1243 | 1304 | relay (stale relay URL still valid) |
| | restart-settled | 3/3 | 236 | 261 | 303 | relay |
| 1b. n0 preset, relays disabled (`N0DisableRelay`) | all four measures | 0/3 each | - | - | - | none, 30 s cap, 12-17 attempts per measure |
| 1c. n0 services, relays disabled, pkarr publisher unfiltered (IPs published) | first connect | 3/3 | 2932 | 3111 | 3666 | LAN IP |
| | first-settled | 3/3 | 223 | 233 | 259 | LAN IP |
| | restart, new port | 3/3 | 7033 | 7036 | 7039 | LAN IP |
| | restart-settled | 3/3 | 1007 | 1008 | 1011 | LAN IP |
| 2. mDNS, relays disabled (service name `e6spike`) | first connect | 6/6 (solo series) | 717 | 770 | 1476 | LAN IP |
| | first-settled | 6/6 | 6.8 | 10.1 | 11.7 | LAN IP |
| | restart, new port | 6/6 | 1006 | 1009 | 1012 | LAN IP (new port) |
| | restart-settled | 3/3 (T3000 series) | 4.9 | 6.1 | 23.0 | LAN IP |
| 3a. presence payload, in-process relay | first connect | 3/3 | 13.9 | 14.5 | 15.0 | LAN IP |
| | payload received at A, after B bind | 3/3 | 5.8 | 5.8 | 6.4 | - |
| | restart, new port | 3/3 | 1006 | 1019 | 1020 | LAN IP (new port) |
| | restart-settled (1 s after payload) | 3/3 | 4.5 | 14.3 | 15.1 | LAN IP |
| 3b. presence payload, no relay | first connect | 3/3 | 13.8 | 14.1 | 14.8 | LAN IP |
| | restart, new port | 3/3 | 1007 | 1008 | 1016 | LAN IP |
| | restart-settled | 3/3 | 4.6 | 9.6 | 16.6 | LAN IP |
| 3c. gossip built-in only (A does not read payload), relay | first connect | 3/3 | 14.1 | 14.1 | 14.5 | LAN IP |
| | restart, new port | 3/3 | 1006 | 1008 | 1024 | LAN IP |
| 3d. gossip built-in only, no relay | first connect | 3/3 | 14.1 | 14.9 | 15.4 | LAN IP |
| | restart, new port | 3/3 | 1005 | 1005 | 1022 | LAN IP |
| Control: no lookup, A holds B's first address in `MemoryLookup`, never updated, relay in B's address | restart, new port | 3/3 | 5.3 | 6.4 | 7.6 | relay (stale IP dead, relay URL valid) |
| Control: same, no relay | restart, new port | 0/3 | - | - | - | none, 15 s cap, 5 attempts each |
| 5. self-hosted `iroh-dns-server` (pkarr + DNS resolvers), local relay | first connect | 3/3 | 3091 | 3127 | 3386 | relay |
| | first-settled | 3/3 | 6.9 | 11.6 | 12.4 | relay |
| | restart, new port | 3/3 | 3018 | 3028 | 3034 | relay |
| | restart-settled | 3/3 | 9.1 | 10.7 | 18.6 | relay |
| 5b. self-hosted, no relay (IPs published) | first connect | 3/3 | 6.5 | 6.6 | 6.7 | LAN IP |
| | restart, new port | 3/3 | 3019 | 3023 | 3034 | LAN IP |
| | restart-settled | 3/3 | 10.3 | 12.3 | 18.3 | LAN IP |

What A had to know: the id only, in every row above. The presence rows also needed bootstrap: A and B each held seed S's full address out of band to join the gossip topic.

Sensitivity to dial timeout (series T=1000 ms, n=3, `/tmp/e6-T1000/*.log`):

| Method | Measure | min ms | median ms | max ms |
|---|---|---|---|---|
| n0 preset | first | 3034 | 3146 | 3228 |
| n0 preset | restart | 1263 | 1266 | 1283 |
| n0 services, IP publish | first | 262 | 3276 | 3505 |
| n0 services, IP publish | restart | 4059 | 4060 | 4060 |
| mDNS | first | 732 | 763 | 1469 |
| mDNS | restart | 1018 | 1020 | 1022 |
| self-hosted + relay | first | 3164 | 3269 | 3411 |
| self-hosted + relay | restart | 3045 | 3047 | 3052 |
| self-hosted, no relay | restart | 1023 | 1025 | 2036 |
| presence (4 variants) | first | 13.8 | 14.2 | 15.6 |
| presence (4 variants) | restart | 1017 | 1019 | 1023 |

Self-hosted, one resolver only (T=1000, n=3, `/tmp/e6-self/*.log`):

| Resolver | Relay | first (med ms) | restart (min/med/max ms) | restart-settled (med ms) |
|---|---|---|---|---|
| pkarr HTTP only | no | 18 | 1019 / 1025 / 1027 | 6 |
| pkarr HTTP only | yes | 3024 | 3048 / 3052 / 3053 | 6 |
| DNS only | no | 7 | 26358 / 26360 / 26371 | 25350 |
| DNS only | yes | 3169 | 3045 / 3046 / 3046 | 6 |

## Several relays: what reaches what

Setup: two in-process plain-HTTP relays R1, R2. Both endpoints list both relays and run `clear_ip_transports()` (relay-only paths). B homed on R2, A homed on R1, verified from `addr().relay_urls()` before each dial and re-read after (unchanged in all 15 dials). Forcing: map order does not decide the home relay (probe, 8 binds, `/tmp` output not kept: map `[R1,R2]` chose R2,R2,R2,R1; map `[R2,R1]` chose R1,R1,R1,R2). I rebind endpoints until the home matches (rejection sampling). The stop-R1-for-B's-start route was not used. Fresh A per test. Cap 8 s per test. n=3 runs.

| A dials | ok/n | time | Path |
|---|---|---|---|
| id + R1 URL only (A's home, B is not there) | 0/3 | 3 attempts x 3 s timeout, 9.04 s | none |
| id + R2 URL only (B's home) | 3/3 | 7.5, 7.5, 7.8 ms | relay R2 |
| id via `MemoryLookup` holding B's full `EndpointAddr` | 3/3 | 7.4, 7.6, 7.7 ms | relay R2 |
| id + R1 URL + R2 URL | 3/3 | 6.1, 6.3, 7.5 ms | relay R2 |
| A's map lists only R1, dial id + R2 URL | 3/3 | 6.2, 6.3, 8.1 ms | relay R2 |

A relay does not forward to a peer homed on another relay: dial through A's own relay (R1) with B homed on R2 failed 0/3. Dial through B's home relay (R2) works, including when A's own relay map does not list R2: A connects to the relay named in the address. No same-relay control was run in this spike. e1-api checks 9e/9f (both endpoints on one relay, id + relay URL) pass, and A reached R1 as its home relay (`online`) in every test. So a node must learn B's current home relay URL, not just any shared relay.

## Self-hosted lookup

Crate `iroh-dns-server`, version 1.3.0 (same version as iroh 1.3.0), repository github.com/n0-computer/iroh, description "A pkarr relay and DNS server", license MIT OR Apache-2.0. Binary `iroh-dns-server -c <config.toml>`.
- Install: `cargo install iroh-dns-server --version 1.3.0 --locked --root <dir>`: 289 crates compiled, 42.7 s release build here, binary 16.0 MB.
- Needs: a config file (examples `config.dev.toml`, `config.prod.toml` ship in the crate); an HTTP and/or HTTPS listener (`/pkarr` GET/PUT signed packets, `/dns-query` DoH; HTTPS cert mode `self_signed` or `lets_encrypt`); a DNS listener (UDP+TCP; prod config port 53) authoritative for configured `origins`, with SOA, `rr_a`, `rr_ns`, so production needs a domain with NS delegation to the host; a persistent database (default `~/Library/Application Support/iroh-dns/signed-packets-1.db`, redb); a metrics listener (127.0.0.1:9117 in dev run); optional mainline DHT publishing (`[mainline] enabled`, on in the dev config, set `false` here); PUT rate limit (`disabled` in dev, `smart` in prod).
- Clients: the `N0` preset hardcodes n0's URLs, so self-hosting means three builder calls (spike `selfhost.rs:20-31`): `PkarrPublisher::builder(<http url>/pkarr)`, `PkarrResolver::builder(<http url>/pkarr)`, `DnsAddressLookup::builder(<origin>).dns_resolver(DnsResolver::with_nameserver(<addr>))`. All three types ship in iroh itself.
- Ran locally: dev config, HTTP 8080, DNS 5300, origin `irohdns.example.`, mainline off. Measure 1 repeated against it: see rows 5 and 5b and the one-resolver table above.
- Publish visibility, probe `publish` (GET `/pkarr/<z32 id>` polled every 50 ms until the served record carries a relay or address entry; n=3 each):

| Server | Endpoint | record with address visible after B bind |
|---|---|---|
| n0 `dns.iroh.link` | N0 preset | 664, 672, 753 ms (relay entry) |
| local iroh-dns-server | local plain-HTTP relay | 3021, 3025, 3033 ms (relay entry) |
| local iroh-dns-server | no relay, IP publish | 5.8, 5.8, 7.4 ms (address entry) |

- `online` probe (time from `bind()` to `online()`; n=3 each): local plain-HTTP relay 3.009, 3.010, 3.011 s; n0 preset 0.66, 0.68, 0.89 s.

## What each method costs

Crates are unique `cargo tree -e normal` entries added over `iroh =1.3.0` alone (iroh alone: 239 entries including the root line; scratch crates in `/tmp/e6-deps`, script `/tmp/e6-deps.sh`).

| Method | Crates added | Third parties | Spike lines |
|---|---|---|---|
| n0 preset | 0 (pkarr publisher/resolver and DNS lookup are inside iroh) | n0's public `dns.iroh.link` (publish + resolve) and n0 relays; records are readable without credentials (I fetched my own with plain `curl`), so anyone holding an id can read its relay URL or IPs; no SLA looked up | 1 line (`Endpoint::builder(presets::N0)`); `n0.rs` is 36 lines because it carries three variants |
| mDNS | 10: acto, critical-section, hickory-proto, iroh-mdns-address-lookup, lock_api, parking_lot, parking_lot_core, scopeguard, smol_str, swarm-discovery | none; LAN multicast only | 1 builder line (`mdns.rs:12`); file 18 lines |
| Presence payload | 0 beyond iroh-gossip (+10 if gossip is not otherwise used: anyhow, byteorder, fixedbitset, futures-concurrency, hash32, heapless, hex, iroh-gossip, irpc, irpc-derive); postcard already in the tree | none; needs one seed whose address is known out of band | about 24 (`presence.rs` broadcast task from line 41, receive handler from line 83); whole file 124 |
| Self-hosted iroh-dns-server | 0 client side; server is a separate 289-crate binary | operator runs the server, a domain with NS delegation, TLS, a database | about 12 client lines (`selfhost.rs:20-31`), plus config file and the process |
| (reference) in-process relay server | 62 (iroh-relay `server` feature) | operator | `main.rs` `spawn_relay` 5 lines |

## Surprises

- `N0DisableRelay` gives no id-only dial at all: 0/3 over 30 s on every measure, including a dialer that starts 5 s after B. The `PkarrPublisher` default filter is relay-only (`pkarr.rs` header, `AddrFilter::relay_only()` at `pkarr.rs:168`), so a node without a relay publishes nothing dialable. The doc comment on `N0DisableRelay` (`presets.rs`) says it publishes IP addresses. With the publisher set to `AddrFilter::unfiltered()` it works (row 1c).
- n0 first connect: record with relay entry is served 0.66-0.75 s after bind, yet A's id-only dial succeeds at 3.67-3.77 s (T=3000) or 3.03-3.23 s (T=1000). The 2-3 s gap is not explained. It does not appear against the local server (record at 3.02 s, dial at 3.09-3.39 s). Per-attempt logs show attempt 1 sits for the whole timeout or returns `No addressing information available` at 2.7-3.5 s.
- Restart on a new port with continuous/push lookups (mDNS, gossip payload, gossip built-in): 1.005-1.02 s every time, always attempt 1, with the new address already known at A within 6-25 ms (presence payload at A 5.5-7.4 ms after B bind). Dialing 1-3 s later takes 4.5-23 ms. So the 1 s is time spent on a dial that began before the address update; consistent with e5's "fixed ~1.0 s". Cause not isolated; my reading is the stale cached address is tried first and others after 1 s.
- Restart with pull lookups (pkarr HTTP, DNS): a dial in flight with the stale address does not re-resolve. First attempt hangs until the attempt timeout T, the second attempt succeeds in 20 ms. Restart time = T + ~20 ms (T=3000: 3.02 s; T=1000: 1.02 s). An application connect timeout of about 1 s with redial bounds it.
- DNS-only lookup is stale for about 26 s after restart even when the dial starts 5 s after (restart 26.4 s, restart-settled 25.3 s, 3/3). Publisher default TTL is 30 s (`DEFAULT_PKARR_TTL = 30`, `pkarr.rs:143`). I did not vary the TTL, so the link between the two is a reading, not a test. pkarr HTTP resolver has no such staleness (restart 1.02 s). The default `N0` preset runs both resolvers; the n0 relay rows hide this because the relay URL stays valid across the restart.
- iroh-gossip attaches its own `GossipAddressLookup` to every endpoint that spawns `Gossip` (`iroh-gossip net.rs:186-194`) and fills it from gossip peer data (`net.rs:737-749`). A node that is in the gossip mesh is dialable by id with no application payload: variants 3c/3d (A ignores the payload) match 3a/3b within noise. I cannot separate the two mechanisms in 3a/3b, since the built-in one cannot be switched off.
- `MemoryLookup::add_endpoint_info` merges direct addresses and overwrites the relay URL (`memory.rs:184-199`). After one restart A's entry held 3 IP addresses for B (old and new accumulated) in all six payload runs; 0 in the six built-in-only runs. Use `set_endpoint_info` to replace.
- Relay-homed restart is invisible to A: stale entry plus unchanged relay URL connects through the relay in 5-8 ms (stale control row). Without a relay the same stale entry fails 0/3 in 15 s. e5's "undialable by stale address" holds only for direct-address-only entries.
- A relay never forwards across relays (id + R1 URL fails 0/3). Home relay among two local relays is not decided by map order (6 of 8 binds chose the last-listed).
- A plain-HTTP in-process relay takes 3.01 s to become `online()` (3 of 3), against 0.66-0.89 s for the n0 relays. Every relay row here carries that 3 s in the publish-after-`online` path (row 5); earlier spikes that wait for `online()` on an in-process relay may carry it too (not checked).
- With IP publishing and the n0 server (row 1c) first connect is bimodal: 0.26-0.31 s in 4 of 12 samples pooled over series, 2.9-3.7 s in the other 8.
- mDNS first connect is bimodal: 0.72-0.80 s in 13 of 18 samples pooled over all series, 1.47-1.57 s in 5 of 18 (all three of the T=3000 series, one of T=1000, one of the solo series). Cause not isolated; looks like a multicast query interval.

## What could make this wrong

- One host, one process, one LAN. A and B share a machine and an IP; direct paths are always available and never cross NAT. mDNS is the easy case (same host, multicast works). Cross-host, cross-NAT, Wi-Fi isolation, VPNs untested. Linux untested.
- n=3 per cell (n=6 for mDNS solo). Medians of 3 are weak; several cells are bimodal (mDNS first, n0 IP-publish first). Ranges shown, not distributions.
- Dial policy shapes every restart number for pull lookups (quantized by T). I report T=3000 and T=1000. Other policies (shorter T, parallel redial) would give other numbers. The 10 ms retry sleep adds up to 10 ms to fast cells.
- The restart is a graceful close of B (`Router::shutdown`), not SIGKILL or power loss. A's side sees QUIC close frames and the relay sees a clean disconnect; a crash would differ (e5 used SIGKILL).
- A and B live in one process and one Tokio runtime. Per-endpoint DNS caches are separate objects but the process shares anything global (system resolver config). Not checked.
- n0 PROD service, one afternoon, one region (relay host `euc1-1`); n0's behavior at other times, from other networks, or under load not measured. The 2-3 s gap on n0 is unexplained. Hypothesis not tested: n0-side caching of negative DNS answers.
- The n0 rows published on the order of 100 ephemeral test ids to n0's production pkarr server (every endpoint with an n0 publisher publishes, dialers included; about 40 n0 runs over all series, plus the `publish` probe). Records have TTL 30 s but the server's retention is unknown.
- The local `iroh-dns-server` persisted my test records in `~/Library/Application Support/iroh-dns/signed-packets-1.db`. Not cleaned.
- Presence rows use a single seed S and a 3-node topic (HyParView with 3 nodes connects everyone). Bigger swarms may route presence differently. Broadcast happens on every address change plus at start; a lost broadcast (e.g., B joins the mesh after its first address emission) was not exercised.
- mDNS used custom service name `e6spike`, not the default. Other agents' mDNS traffic on this machine/LAN during runs is possible (several spikes ran concurrently on this host; I ran up to 6 of my own processes at once in the T=3000 and T=1000 series, mDNS solo series was run alone).
- Earlier series (retry sleep 100 ms, no first-settled control, `/tmp/e6-n0.log`, `/tmp/e6-presence.log`) gave the same picture (n0 first 3.06-3.95 s, mDNS restart 1.005 s, `N0DisableRelay` 0/3). The reported tables come from the last code version only.
- `dial_until` counts the echo round trip; `connect` alone is slightly shorter.
- Path is read at echo completion; an upgrade from relay to direct path later is not captured (e1 saw one within 3 s).
- The 1 s floor and the DNS 26 s staleness are read from timings and constants; the iroh source paths that cause them were not traced. The `GossipAddressLookup` claim is from reading `net.rs`, confirmed only by the 3c/3d runs matching 3a/3b.
- Debug build; release timings may differ for the sub-20 ms cells.
