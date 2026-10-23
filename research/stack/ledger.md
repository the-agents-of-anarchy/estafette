# Ledger: the spec against technological reality

Author: claude-fable-5-1 (orchestrator), 2026-10-16. Replaces the dropped stack synthesis. Every row cites a report I read in full: experiments/e1-api.md, e2-size.md, e3-kill.md, verify/v3-irpc.md, v1a-iroh-fixes.md, v1b-gossip-fixes.md. Everything else in research/stack/ is a lead.
Goal (owner): "test our theoretical spec against technological reality and revise anything that needs it."
Classes: RUN = observed by running code (one host per run: macOS arm64, or from 2026-10-20 the Linux lab VM, reports in lab/linux/; loopback or LAN, small n). RECORD = primary record (source at a release, git ancestry, PR state). RULED = owner ruling. OPEN = untested.
Tidied 2026-10-21: collision and staged-revision rows cut to claim, status and pointers; their run-by-run notes moved, unchanged in substance, to "Evidence by item" at the end. Ruling times are from docs/orchestration_log/history/2026-10-15/events.md (times +0300).
Final pass 2026-10-21, after the owner closed the spec work (23:10): E22-E25 and the two owner-assisted tests added to the surface map, OPEN, and "Evidence by item"; "Known limitations (accepted)" added. The index of the whole research is research/stack/README.md.

## Status at a glance
Spec edits are the owner's act, applied in one batch ("we'll apply everything at once", 2026-10-16 12:50); applied to spec/blueprint.md on 2026-10-21 at the owner's word "apply current changes, carefully", with the decisions and review fixes in research/stack/spec-apply-report.md, spec-review.md, spec-review-2.md.

| Id | Title | Status | Owner action |
|---|---|---|---|
| C1 | address lookup for dial by id | RULED 2026-10-20 19:06 "Address in presence" → S20 | none |
| C2 | direct-protocol streams and read timeout | RULED 2026-10-16 18:12 → S10 | none |
| C3 | inbox semantics | RULED 2026-10-16 20:27 → S18 | none |
| C4 | duplicate suppression (D25) | RULED 2026-10-16 18:12 → S11 | none |
| C5 | announcement ordering | RULED 2026-10-16 18:12 → S12 | none |
| C6 | network-change signal | RULED 2026-10-16 18:09 → S13 | none |
| C7 | relay and bootstrap topology | RULED 2026-10-20 19:10 "Hosts announce themselves" → S21; "No n0 at all" 19:24 | none |
| C8 | node as identity | RULED 2026-10-16 20:07 → S14 | none |
| C9 | first Presence timing | RULED 2026-10-16 20:07 → S15 | none |
| C10 | stop and Departure | RULED 2026-10-16 20:21 → S17 | none |
| C11 | transport retries | RULED 2026-10-16 20:27 → S19 | none |
| C12 | gossip admission gate | RULED 2026-10-16 20:07, mechanism 2026-10-20 19:06 "Hook, both ways" → S16 | none |
| C13 | gossip stall on relay death | RULED 2026-10-20 21:40 "Own fix on the release" → S24; measured (E12) | none |
| C14 | Departure late under rmcp's drain | RULED 2026-10-20 21:40 "Measure the fix" → S25; measured | none |
| C15 | MCP surface values G5 | RULED 2026-10-20 21:40 "Approve as drafted" → S23 | none |
| C16 | relay send path blocks behind a dead relay (iroh) | RULED 2026-10-21 00:00 workaround + bug report → S26; 12:25 the owner posts the report | post experiments/e14-bug-report.md upstream |
| C17 | gossip never re-dials a peer after a failed dial | under C13's ruling; measured: do not carry the E16 fix; fixed upstream, unreleased | none |
| C18 | Join unanswered after a rebind (stale pending Neighbor) | RULED 2026-10-21 12:30: on_join fix via Cargo [patch.crates-io] | none |
| C19 | no host authority for infrastructure records | RULED 2026-10-21 12:34 "Guard it with your key"; staged as S27 | none |
| S1 | public card fields in Presence, cap 3072 B | RULED 2026-10-20 19:20 | applied to spec 2026-10-21 |
| S2 | QUIC idle timeout 10 s | RULED 2026-10-16 12:40 | applied to spec 2026-10-21 |
| S3 | relay behind a cargo feature | RULED 2026-10-16 12:40 | applied to spec 2026-10-21 |
| S4 | yaml-rust2 plus own converter | RULED 2026-10-16 12:45 | applied to spec 2026-10-21 (stack D-record) |
| S5 | hex for every text form | RULED 2026-10-16 12:45 | applied to spec 2026-10-21 |
| S6 | no presence encryption | RULED 2026-10-16 12:50 | applied to spec 2026-10-21 |
| S7 | iroh-gossip pinned to 0.101.0 | RULED 2026-10-16 12:45 | applied to spec 2026-10-21 (stack D-record) |
| S8 | direct protocol by hand; irpc rejected | RULED 2026-10-21 14:20 "Keep the decision" (v3 run) | applied to spec 2026-10-21 (stack D-record) |
| S9 | sender checks gossip payload size | RULED 2026-10-21 14:24 "Keep the decision" | applied to spec 2026-10-21 |
| S10 | one stream per request; 10 s server read timeout | RULED 2026-10-16 18:12 | applied to spec 2026-10-21 |
| S11 | drop duplicate suppression | RULED 2026-10-16 18:12 | applied to spec 2026-10-21 |
| S12 | per-member seq, max age 300 s, tombstone | RULED 2026-10-16 18:12; max age 2026-10-20 19:06 | applied to spec 2026-10-21 |
| S13 | re-announce on relay status and address change | RULED 2026-10-16 18:09; measured E23 (debounce added) | applied to spec 2026-10-21 |
| S14 | node as identity, sidecar lock | RULED 2026-10-16 20:07; measured Linux and power cut E23 (fsync added) | applied to spec 2026-10-21 |
| S15 | first Presence after topic joined | RULED 2026-10-16 20:07 | applied to spec 2026-10-21 |
| S16 | keyed challenge in after_handshake, both ways | RULED 2026-10-16 20:07; 2026-10-20 19:06 | applied to spec 2026-10-21 |
| S17 | stop: Departure, release lock, wait 5 s | RULED 2026-10-16 20:21 | applied to spec 2026-10-21 |
| S18 | inbox ring of 1000; ack = delivered | RULED 2026-10-16 20:27 | applied to spec 2026-10-21 |
| S19 | no transport-level retries | RULED 2026-10-16 20:27 | applied to spec 2026-10-21 |
| S20 | address in every Presence and Introduce | RULED 2026-10-20 19:06 | applied to spec 2026-10-21 |
| S21 | self-announcing infrastructure hosts | RULED 2026-10-20 19:10; measured E21 (macOS, Linux), E24 (across machines) | applied to spec 2026-10-21 |
| S22 | agreement parameters G1-G4 | RULED 2026-10-20 19:32 | applied to spec 2026-10-21 |
| S23 | MCP surface values G5 | RULED 2026-10-20 21:40 | applied to spec 2026-10-21 |
| S24 | relay death: rejoin + remove_relay on the pin | RULED 2026-10-20 21:40; rejoin lever replaced by S26 | applied to spec 2026-10-21 |
| S25 | stop on end of stdin, own reader | RULED 2026-10-20 21:40; measured macOS, Linux | applied to spec 2026-10-21 |
| S26 | relay-death workaround, full fix | RULED 2026-10-21 12:30 "Full fix"; trigger 15 s ruled 15:15; measured across machines E24, E25; hosts never rebind (E24) | applied to spec 2026-10-21; two relay-only gaps accepted as known limitations |
| S27 | server records accepted only from ids the owner's key certified (C19) | RULED 2026-10-21 12:34 "Guard it with your key" | applied to spec 2026-10-21 |

## Surface map (the original surface, by range)
Marks: ■ settled by a run or a record · ▣ ruled by the owner · ◆ solid lead (a worker's claim later confirmed by our run or the orchestrator's check, or a sonnet source read whose project tests were run) · ◇ unconfirmed lead · ○ open · ▶ in flight. Update this map whenever a row changes.

```
ORIGINAL SURFACE: which ranges are solved, at what cost, what we copy, what we write
│
├─ TRANSPORT ─────────────── iroh 1.3.0 ────────────────────────────── mostly settled
│   ├─ ■ direct, relay-only, relay→LAN upgrade, remote id = dialer key   e1 2,4,9e,9f
│   ├─ ■ one key type for card and node, strict verify                   e1 1
│   ├─ ■ one router, our ALPN + gossip ALPN                              e1 3
│   ├─ ■ dial needs an address: mDNS or relay URL in the address         e1 9
│   ├─ ◆ mDNS is a separate crate (iroh-mdns-address-lookup)             f1b ← e1 5
│   ├─ ◇ N0 preset = pkarr publish + DNS lookup                          f1a
│   ├─ ▣ no n0 at all                                      owner 2026-10-20 19:24
│   ├─ ◇ endpoint tickets carry no name or secret                        f1b
│   ├─ ■ iroh-gossip installs its own address lookup, but it learns only  e6, e8
│   │      gossip NEIGHBORS: 3 adjacent nodes dial by id in ~14 ms (e6); in a
│   │      5-node mesh members were undialable by id (e8) until Presence
│   │      carried the address and receivers added it to their lookup
│   ├─ ■ restart on a new port: ~1 s via gossip lookup or mDNS            e6
│   ├─ ■ n0 preset (pkarr): first dial ~3.7 s; records public; relay-off  e6
│   │      variant publishes nothing dialable by default
│   ├─ ■ relays never forward across relays: a dialer needs the peer's    e6
│   │      own home relay URL; home relay is not chosen by map order
│   ├─ ■ mDNS: ~0.8 s first, ~1 s restart, +10 crates                     e6
│   ├─ ■ self-hosted iroh-dns-server works: 289-crate binary, domain +    e6
│   │      NS delegation + DB; DNS-only lookups stale ~26 s
│   ├─ ■ relay death: home status false in 2 ms; re-home to the live     e8
│   │      relay takes ~40 s; peers keep the dead relay URL; dials to the
│   │      member recover only if the relay returns
│   ├─ ■ relay death (not bootstrap death) stalls gossip mesh-wide, in    e8, e10
│   │      0.101.0 and main alike (control 0/6); 0.101.0 never heals in
│   │      180 s; main rejoins alone and heals at 62-145 s (#191);
│   │      stall and ~41 s re-home reproduced on Linux       linux-e10
│   ├─ ■ re-home ~40 s is two periodic re-stun intervals; removing the     e10
│   │      dead relay forces a full report (code read, unrun)
│   ├─ ■ own rejoin plus remove_relay on 0.101.0 (C13): removal re-homes  e12,
│   │      in 0.7 s; rejoin heals 0/62 (Linux 0/38)            linux-e12
│   ├─ ■ cause (C16): one dead relay's full queue blocks every relay      e14, e14-bug-report,
│   │      send of the endpoint; iroh 1.3.0 and main; macOS and Linux   linux-e14*
│   ├─ ▣ relay-death workaround: full fix (S26)              owner 2026-10-21
│   ├─ ■ bootstrap failover cheap: one of two dead, join in 8-18 ms        e8
│   ├─ ■ insert_relay at runtime forces a full net report; can re-home    e21
│   ├─ ■ two machines through a home NAT: 20/20 both ways, direct path    e22
│   │      ~200 ms after the dial; gossip 120/120 (NAT port-preserving)
│   ├─ ■ relay death across machines: IP transports on, 0/16 runs stall  e24, e25
│   │      after a host's death; relay-only, the surviving host is
│   │      blocked 62.6-87.3 s in 10/21 runs (known limitation 1)
│   ├─ ■ sleep 247.5 s and wake: all pairs flowing within 3.7 s         owner test
│   ├─ ■ Wi-Fi to hotspot: no rebind, pairs resume within 0.9 s         owner test
│   └─ ○ symmetric NAT, CGNAT, UDP-blocked: never met (known limitation 3)
│
├─ GOSSIP ────────────────── iroh-gossip 0.101.0 ───────────────────── partly settled
│   ├─ ■ worst Presence 4368 B, overhead 38, minimum 4407, default 4096 e2
│   ├─ ■ oversize: Ok returned, message lost, link dead                  e1 8b, e2
│   ├─ ■ crash → NeighborDown 33 s default, 13 s at 10 s idle            e3
│   ├─ ■ clean stop → NeighborDown 3 ms                                  e3
│   ├─ ■ churn-leak fixes merged, unreleased                             v1b
│   ├─ ▣ lower the public cap · idle timeout 10 s · pin 0.101.0
│   ├─ ◆ max_message_size has a builder setter                           f2 ← e1 8
│   ├─ ◇ author hidden (delivered_from = previous hop); dedup 90 s       f2
│   ├─ ■ topic id alone admits an outsider: joins in ~6 ms, reads every  e7
│   │      Presence; forged payloads are dropped on MAC but forwarded first
│   ├─ ■ secret-proof gate works: keyed challenge both ways in iroh's     e9
│   │      after_handshake hook; outsider gets 0 neighbors, 0 payloads,
│   │      0 forgeries forwarded; ~1-2 ms per connection; ~60 lines, no crate
│   ├─ ■ an accept-loop-only gate cannot prove gossip's own outgoing      e9
│   │      dials (gossip dials internally); a lying acceptor read Presence
│   ├─ ■ Presence sent before gossip neighbors exist is lost; tables      e4
│   │      never fill (3/3); sent after the topic forms: all five in ~3 s
│   ├─ ■ Departure: broadcast only enqueues; exit at once reached 0-2 of  e4
│   │      4 peers; 1000 ms sleep before exit reached 4/4 (3/3)
│   ├─ ■ crash: NeighborDown ~15 s + probe 3 s → every peer Unreachable  e4
│   │      at ~18 s; no live member falsely marked (D20 holds)
│   ├─ ■ false rumor corrected within ~5 ms of reaching its subject       e4
│   ├─ ■ S12 holds: replay never revives a departed member; tombstone and e4
│   │      max age both needed (each covers one case)
│   ├─ ■ Introduce costs about N² requests per full join                 e4
│   ├─ ■ no re-dial after a failed dial (C17); fixed on main, unreleased  e15, e16
│   ├─ ■ Join unanswered after a rebind (C18); fixed on main #161         e18, linux-e18
│   ├─ ▣ carry the on_join fix by [patch.crates-io] (C18)    owner 2026-10-21
│   ├─ ■ S13 as staged re-announces ~20 times in 12 s while a relay is   e23
│   │      down; debounce applied (changed address set, at most once per 2 s)
│   └─ ○ S13 on a real interface change: not produced in E23 (no sudo);  e23, owner test
│          the owner's Wi-Fi switch: pairs resumed in 0.9 s, no rebind
│
├─ DIRECT PROTOCOL ───────── hand-written on iroh streams ──────────── settled
│   ├─ ■ read_to_end(1,114,112) caps, resets only the stream             v3
│   ├─ ■ irpc rejected: fixed 16 MiB, pre-allocates, +4 crates           v3
│   ├─ ◆ echo example = smallest accept-read-respond                     d2 ← v3
│   └─ ■ gap: no server read timeout in 3.14                             v3
│
├─ INFRASTRUCTURE ────────── iroh-relay `server` ───────────────────── mostly settled
│   ├─ ■ in-process relay over plain HTTP; http_url needs test-utils     e1 7
│   ├─ ◆ test-utils leaks insecure TLS if not dev-only                   f3, r1 ← e1 7
│   ├─ ▣ relay behind a cargo feature
│   ├─ ▣ hosts announce themselves (S21); records guarded by the owner's key (C19)
│   ├─ ■ host added at runtime reaches every member in 24-114 ms         e21 (macOS)
│   ├─ ■ the same on Linux in 136-163 ms; across machines 319-379 ms    linux-e21, e24
│   ├─ ▣ relay TLS: HTTPS if low-effort (owner 2026-10-21 14:35) → D51
│   ├─ ■ Let's Encrypt certificate 9.15 s after first start, 10-line     e22
│   │      config; HTTPS +87 ms to come online, nothing else measurable
│   ├─ ■ relay without QUIC address discovery: 3.1 s online, 0.49 s with e22
│   ├─ ■ hosts applying the deaf rule loop when their fellow host dies;  e24
│   │      hosts never rebind on silence (spec, 2026-10-21 18:54)
│   ├─ ○ QUIC address discovery in the embedded library (known limitation 4)
│   └─ ○ bootstrap member
│
├─ CRYPTO ────────────────── iroh keys, blake3 (in tree) ───────────── mostly settled
│   ├─ ■ sign / verify                                                   e1 1
│   ├─ ▣ hex everywhere · no presence encryption
│   └─ ○ derive_key, keyed MAC, constant-time compare (trivial)
│
├─ WIRE ENCODING ─────────── postcard (in tree) ────────────────────── settled
│   └─ ■ sizes of every structure                                        e2
│
├─ IDENTITY ─────────────────────────────────────────────────────────── mostly settled
│   ├─ ■ address cache never evicts → fresh keys leak                    v1a
│   ├─ ▣ node-as-identity propagated; sidecar lock for one holder
│   ├─ ◇ no surveyed project detects a second live holder at runtime     so4
│   ├─ ■ flock freed on SIGKILL in 6.5-10.4 ms; restarted node takes it   e5
│   │      first try 15/15; second holder gets WouldBlock; std try_lock,
│   │      no crate (macOS; Linux in e23)
│   ├─ ■ same-key restart, same address: gossip seamless, 0 NeighborDown/ e5
│   │      Up at peers in 30 runs; new instance heard in 0.2-1.0 s
│   ├─ ■ held connection to a killed node hangs silently until the idle   e5
│   │      timeout (10 s), then connection_lost: app timeouts must be < 10 s
│   ├─ ■ restart on a NEW address: dial by id with the old address fails   e5
│   │      85/85 for 22 s though gossip meshes: addresses must be refreshed
│   ├─ ■ two live holders: silent starvation of one (direct); relay's     e5
│   │      newest wins, the old loses gossip for good: the lock is essential
│   ├─ ◆ precedent for N: mcpmesh key file + exclusive lock in one root,  state/m4
│   │      order lock → key → bind (hooks unarmed) → arm → serve
│   ├─ ◆ never unlink the lock file; kernel is the only releaser          state/m4
│   ├─ ◆ key mint: 0600 temp, fsync, hard_link (no overwrite, race-safe)  state/m4
│   ├─ ■ Linux lock: refused 5/5, free 154-232 us after SIGKILL, free    e23
│   │      with a live forked child 5/5
│   └─ ■ power cut (hard reset, 20): lock free on every boot; unsynced   e23
│          key file lost 5/5 when cut 1-2 s after writing, intact 5/5
│          with fsync of file and directory; fsync rule added to S14;
│          a power loss sends no FIN to the relay: its effect untested
│
├─ FILES AND LOCAL ───────── small pieces ──────────────────────────── mostly leads
│   ├─ ■ serde YAML crates rest on unsafe-libyaml (RUSTSEC-2024-0449)    orchestrator check
│   ├─ ▣ yaml-rust2 + own converter
│   ├─ ◆ swarm-discovery fix in 0.6.4, resolved by a fresh lock          v1b ← e1 10
│   ├─ ◇ base32/hex in data-encoding; uuid v7 a feature; libc in tree    s1
│   ├─ ◇ dirs breaks spec 3.3 on macOS → hand XDG                        s1
│   └─ ◇ tokio stdin read uncancellable; `signal` adds two crates        s1
│
├─ MCP ───────────────────── rmcp 3.5.1 (ruled), hand-written handler ─────── run
│   ├─ ■ static six tools, envelope with `you`, bad_request, 3.10 text   e11
│   ├─ ■ SIGTERM hangs on a pending stdin read; shutdown_background      e11,
│   │      exits in 1 ms idle; same on Linux, every check and timing    linux-e11
│   └─ ■ Departure late by rmcp's drain (C14): own stdin reader departs  e13,
│          under 1 ms, exit unchanged (S25)                     linux-e13
│
├─ CLI ───────────────────── clap (ruled) ──────────────────────────── nothing to test
│
├─ STATE ─────────────────── mechanics mostly settled; structs and algorithms ours
│   ├─ ◆ roster ordering: per-origin seq = max(now_ms, last+1); accept   state/m1
│   │      only if signed, within max age, above held and above tombstone;
│   │      tombstone on gone (habilis link vectors, 9 tests run)
│   ├─ ◆ every side map bounded to live + returnable set; FIFO caps on    state/m1
│   │      anything a stranger can grow (habilis, flap-churn test run)
│   ├─ ◆ transport truth overrides gossip presence in status (Tunnet)     state/m1
│   ├─ ◇ periodic keepalive 30 s / timeout 90 s in habilis and Tunnet;    state/m1
│   │      estafette relies on the QUIC idle timeout instead (e3)
│   ├─ ◆ names: receiver-local name per key, first-come `-2` suffix      state/m2
│   │      (alink, test run) = D5 as written; persisted there, ours not
│   ├─ ◆ names never authorize; resolve to the key and keep it (mcpmesh  state/m2
│   │      #38 grant desync; agent-gossip's name-keyed roster gap)
│   ├─ ◇ validate inbound handles on the wire, not only local input      state/m2
│   ├─ ◆ dedup: bounded FIFO set sized to the retry horizon; key bound   state/m3
│   │      to the sender, checked after verification (agent-gossip)
│   ├─ ◆ inbox: ring with monotonic seq; leading gap marker when a       state/m3
│   │      reader's position was evicted (agent-gossip)
│   ├─ ◆ per-client `since` is the stateless side-read model; no         state/m3
│   │      server cursor needed (agent-gossip, iroh-gossip)
│   └─ ◇ open in spec: slow-client lag, inbox-full refuse vs drop,       state/m3
│          flood fairness in eviction
│
├─ TREE COST ───────────────────────────────────────────────────────── partial
│   ├─ ■ 314 unique deps without rmcp and clap                           e1 10
│   └─ ■ draft manifest 280 unique deps; 321 with the relay server       e11
│
├─ PRIOR ART (the deep row) ──────────────────────────────────────── leads only
│   ├─ ◇ every agent-messaging project keeps a mailbox                   a1, a3, sp2
│   ├─ ◇ habilis: push delivery tried 2 weeks, removed; 6 → 19 tools     sp3
│   └─ ◇ lineage of the habilis family                                   specimen (unread)
│
└─ TEST HARNESS ──────────────────────────────────────────────────── deferred (ruled)
    └─ ◇ n0's patchbay simulates networks; gossip simulator has no sockets   s2
```

## Spec assumptions, by status

| # | Spec | Assumption | Status | Evidence |
|---|---|---|---|---|
| 1 | D2, 3.1 | One Ed25519 key type serves card and node; sign and verify available | RUN holds; verify is strict | e1 check 1 |
| 2 | 3.6 step 4 | The remote id of an accepted connection is the dialer's key | RUN holds (ids compared; forgery not attempted) | e1 check 4 |
| 3 | D11 | iroh connects two keys directly and via a relay | RUN holds on one host: direct (e1 2a), relay-only (e1 2b, 9f), relay then upgrade to LAN (e1 9e). NAT and two machines OPEN. Two machines through one home NAT: RUN holds, relay-only and direct, 20/20 each way (e22); stricter NATs untested | e1, e22 |
| 4 | D14, D43, 3.14 | A relay can run inside our process | RUN holds over plain HTTP; `http_url()` needs `test-utils`, build the URL by hand | e1 7a, 7b |
| 5 | D15 | Local discovery finds peers with no configuration | RUN holds, via the separate crate iroh-mdns-address-lookup; needs a multicast-capable LAN interface | e1 5a-5c, 9c, 9d |
| 6 | D12, 3.6 | One router serves the gossip protocol and ours | RUN holds | e1 3 |
| 7 | 3.10 step 3 | A node can dial a peer from its node id | RUN: fails with no address lookup configured (e1 9a, 9b, Minimal preset by choice); works with mDNS (9c, 9d) or with the relay URL in the address (9e, 9f). Dial by id through n0's or a self-hosted lookup OPEN | e1 9 |
| 8 | 3.7, 3.14 | A Presence fits a gossip frame | CONTRADICTED. Worst-case Presence 4368 bytes; gossip adds 38; minimum `max_message_size` 4407; default 4096 | e2 |
| 9 | 3.7 | An oversize gossip send fails visibly | CONTRADICTED. `broadcast` returns Ok, message lost, and the link then carries nothing more | e1 8b, 8c; e2 follow-up probe |
| 10 | — | Fix for 8 and 9 | RULED: lower the `public` cap so the default frame carries the largest Presence; exact cap after the Presence shape settles | owner 13:02 |
| 11 | D18, D20, 3.8 | A crashed peer is noticed without heartbeats | RUN holds, at a cost: NeighborDown 33 s after SIGKILL by default (QUIC idle timeout 30 s + ~3 s unexplained), 13 s with a 10 s idle timeout | e3 |
| 12 | — | Idle timeout value | RULED: 10 s | owner 12:40 |
| 13 | D19 | A clean stop is seen at once | RUN holds for gossip's own disconnect: NeighborDown 3 ms. Our Departure payload and the 1000 ms wait OPEN | e3 |
| 14 | 3.8 | The endpoint reports relay reconnects and network changes | RUN partly: `home_relay_status` reports relay loss and reconnect (6.3 s after restart); `watch_addr` stays silent on relay loss. A "network change" output event: none found (lead) | e1 6 |
| 15 | 3.6, 3.14 | A direct request can be capped at 1,114,112 bytes, bad requests closing only their stream | RUN holds by hand: `read_to_end(limit)` rejects 2 MB, largest allocation 40,960 B, stream reset, connection alive | v3 |
| 16 | — | irpc as the direct protocol | RUN rejected: fixed 16 MiB cap, allocates the declared size before the body, closes the connection on a bad frame, hides the remote id, +4 crates; 47 vs 39 lines | v3 |
| 17 | 3.14 | The limits table covers every wait | CONTRADICTED: no server-side read timeout; a stalled stream holds a task until the connection dies | v3 |
| 18 | 3.14 | 5 s connect timeout bounds a dial to a dead peer | RUN: a dial to a dead peer fails only at the idle timeout (30 s default, 10 s tuned); the spec's 5 s wrapper is what bounds it | e3 |
| 19 | D3 | A fresh node key per session costs nothing | CONTRADICTED by RECORD: iroh 1.3.0's address cache never evicts (fix PR closed unmerged); every session is a new remote | v1a |
| 20 | D3 | Churn of ids is safe in iroh-gossip 0.101.0 | RECORD: churn-leak fixes merged after 0.101.0, unreleased; per #178 a 0.101.0 node may lose joins from newer nodes | v1b |
| 21 | — | D3 itself | RULED: reopened; node-as-identity propagated (docs/orchestration_log/history/2026-10-16-node-as-identity.md), sidecar lock replaces D6 tie-break, untested | owner 12:50 |
| 22 | — | swarm-discovery retry-loop fix | RECORD: in 0.6.4; e1's fresh lock resolved 0.6.4 | v1b, e1 10 |
| 23 | 3.1 | Text forms | RULED: hex everywhere | owner 12:45 |
| 24 | 3.5 | YAML card parsing | RULED: yaml-rust2 plus our own converter (serde YAML crates rest on unsafe-libyaml, RUSTSEC-2024-0449; checked by me) | owner 12:45 |
| 25 | D14 | Relay ships in every build | RULED: behind a cargo feature | owner 12:40 |
| 26 | — | Presence encryption | RULED: none | owner 12:50 |
| 27 | — | iroh-gossip pin | RULED: release 0.101.0, upgrade later | owner 12:45 |
| 28 | — | Tree cost | RUN: e1's package (iroh, iroh-gossip, iroh-relay server, mDNS, tokio) 314 unique dependencies; rmcp and clap not included. RUN (resolution only): the full draft manifest, rmcp with default features off, 280 unique name+version; 321 with the iroh-relay server | e1 10, e11 |

## OPEN, ordered by how much of the spec rests on it
Each item keeps its original text; the line under it says what has since settled it.
1. Node as identity: sidecar lock released on crash (macOS, Linux); a restarted node reusing its id while peers hold a stale connection; which instance answers a dial in that window. Gates D2-D7, D10, D30, 3.4, 3.6, 3.8, 3.9.
   Settled on macOS by e5 (RUN) and ruled as S14 (2026-10-16 20:07). Linux lock semantics and power loss settled by e23 (RUN): the lock holds and frees on Linux, and the key file survives a power cut only with fsync, a rule added to S14.
2. Address lookup: dial by id with n0's preset (pkarr), with a self-hosted lookup, and with addresses carried in Presence; several relays in one map and how a peer homed elsewhere is reached. Gates 3.3 config, 3.10 step 3, D14, relay TLS.
   Settled by e6, e8 (RUN) and the rulings S20 (2026-10-20 19:06), "No n0 at all" (2026-10-20 19:24), S21 (2026-10-20 19:10, measured in e21). Relay TLS settled: ruled 2026-10-21 14:35 (HTTPS if low-effort), measured in e22, written as D51. Known limitation 4: QUIC address discovery in the embedded library.
3. Presence flow: join, Introduce to a late joiner, Departure delivery within 1000 ms, probe after NeighborDown, Unreachable rumor and self-correction (D18-D20, 3.8, tests 4, 5, 8, 19, 20).
   Settled by e4 (RUN); ruled as S15, S17. S13's trigger measured in e23 on the relay-status path, debounce added; a real interface change was not produced there. The owner's Wi-Fi switch (2026-10-21) changed the public address and every pair resumed within 0.9 s with no rebind.
4. Gossip admission: does an outsider holding only the topic id and a bootstrap address receive Presence (lead: verify/v2-gossip-gate.md, a source trace).
   Settled: yes, e7 (RUN); the gate measured in e9 and ruled as S16 (2026-10-20 19:06).
5. rmcp: answered by e11 (static list, envelope, stdin close in 1 ms when idle, the SIGTERM hang and its fix); open: the Departure delay under rmcp's drain (C14).
   Settled: C14 measured, e13 and lab/linux/linux-e13 (S25).
6. Two machines: NAT, relay-only across hosts (test 3).
   Through E21 every lab run, Linux included, was one host on loopback (E21 too, lab/linux/linux-e21). Settled by e22, e24, e25 (RUN, three machines: the Mac behind a home NAT, the lab in Nuremberg, a throwaway server in Helsinki) and the two owner-assisted tests. Known limitations 1-3 remain: the host on relay-only paths, partial deafness, strict networks never met.

## Known limitations (accepted)
The owner closed the spec work on 2026-10-21 at 23:10 ("this is as far as i'm willing to go in spec itself"), accepting the two relay-only gaps (1, 2) as known limitations; 3 and 4 are gaps in what was tested.
1. On relay-only paths a surviving host can be blocked by the iroh relay bug (C16) for 62.6-87.3 s, and the spec gives a host no recovery rule (hosts never rebind on silence, D46). Seen in 10 of 21 relay-only runs, 0 of 17 with IP transports on. Evidence: experiments/e25-hosts-norule.md test 2; experiments/e24-cross.md (pilot2).
2. The deaf rule cannot fire for a member that keeps hearing host records while it cannot reach its peers: in one run of 8 (`e25r-sp-3`) 8 of 12 pairs stalled 71 s while every member received a host record every 5 s. Evidence: experiments/e25-hosts-norule.md; the risk was named in research/stack/spec-review.md.
3. A strict network, with no direct path between members, was simulated by turning off IP transports and never met: no symmetric NAT, CGNAT or UDP-blocked network was tested, and E22's home NAT preserved ports. Evidence: experiments/e22-two-machines.md, e24-cross.md, e25-hosts-norule.md ("What could make this wrong").
4. QUIC address discovery in the embedded relay library is unconfirmed: E22 and E24 ran the iroh-relay binary, and the spec embeds the library, assuming it behaves as the binary's `enable_quic_addr_discovery = true`. Evidence: research/stack/spec-apply-report.md (E22 notes); experiments/e22-two-machines.md.

## Leads (hypotheses, not findings)
surveys/*.md, specimen/*.md, solutions/*.md, verify/v2-gossip-gate.md, ground/*.md, index/index.md. Known errors in them are listed in plan.md under "Orchestrator corrections". Use them to choose experiments; never cite them as findings.

## Collisions
All nineteen are ruled or under a ruling. Run-by-run evidence for C13, C14, C16-C19: "Evidence by item".

| # | Spec | Collision | Status | Reports |
|---|---|---|---|---|
| C1 | 3.10 step 3, 3.3 | dial by id needs an address source | RULED again → S20 revised (2026-10-16 20:32 mDNS deferred; 2026-10-20 19:06 "Address in presence") | e6, e8 |
| C2 | 3.14 | stream use and server read timeout | RULED → S10 (2026-10-16 18:12) | v3 |
| C3 | 3.11 | inbox semantics | RULED → S18 (2026-10-16 20:27) | state/m3 |
| C4 | D25 | duplicate suppression | RULED → S11 (2026-10-16 18:12) | |
| C5 | 3.4, 3.8 | announcement ordering | RULED → S12 (2026-10-16 18:12) | e4, state/m1 |
| C6 | 3.8 | network-change signal | RULED → S13 (2026-10-16 18:09) | e1 6 |
| C7 | D14 | relay and bootstrap topology | RULED → S21 (2026-10-20 19:10); n0 fallback: "No n0 at all" (2026-10-20 19:24) | e6, e8 |
| C8 | D3-D7 | node as identity | RULED → S14 (2026-10-16 20:07) | e5, v1a |
| C9 | 3.8 init row, D18 | first Presence timing | RULED → S15 (2026-10-16 20:07) | e4 |
| C10 | D19, 3.8 stop row | stop and Departure | RULED → S17 (2026-10-16 20:21) | e4 |
| C11 | 3.10, 3.14 | transport retries | RULED → S19 (2026-10-16 20:27) | e5 |
| C12 | D13, D17, 3.7 | gossip admission | RULED → S16 (2026-10-16 20:07); mechanism measured in e9, confirmed 2026-10-20 19:06 "Hook, both ways" | e7, e9 |
| C13 | D11, D14, S7 | relay death stalls gossip on the 0.101.0 pin | RULED → S24 (own fix on the 0.101.0 pin; measure first; 2026-10-20 21:40); measured: rejoin does not heal; the stall and re-home hold on Linux | e8, e10, e12, lab/linux/linux-e10, linux-e12 |
| C14 | 3.9, S17, test 4 | Departure late by rmcp's drain | RULED → S25 ("Measure the fix", 2026-10-20 21:40); own stdin reader measured: departure at once, exit unchanged | e11, e13, lab/linux/linux-e11, linux-e13 |
| C15 | 3.10, D30 | MCP surface values | RULED → S23 (2026-10-20 21:40) | e11 |
| C16 | D14, D20, 3.8, S21, S24 | after a relay host dies, all outgoing relay traffic of an endpoint stalls: iroh 1.3.0's single RelayActor waits on the dead relay's full 64-slot queue, which is not drained during backoff; same code on iroh main | RULED (2026-10-21 00:00): workaround in estafette, now S26; an internal bug report with a minimal reproduction, written; the owner posts it upstream (2026-10-21 12:25) | e14, e14-bug-report, e15; lab/linux/linux-dial-after-60s, linux-e14, linux-e14w, linux-e14-repro, linux-e15 |
| C17 | D14, 3.8, S24 | iroh-gossip 0.101.0 never re-dials a peer whose dial failed while it was not active | under C13's ruling; measured: do not carry the E16 fix (it feeds C16 under L2); the path goes away with the next iroh-gossip release (PR #178) | e15, e16 |
| C18 | D14, 3.8, S21, S26 | after a rebind (S26) a member's Join to a bootstrap node can go unanswered: the node holds a stale pending Neighbor request to that id | RULED 2026-10-21 (12:30): carry the one-line on_join fix through Cargo's [patch.crates-io] (the owner: if there is a robust idiomatic cargo-supported way, yes); drop it when an iroh-gossip release carries #161 | e18, lab/linux/linux-e18 |
| C19 | D16, S21 | the spec cannot tell an infrastructure host from a member: any secret holder could broadcast an infrastructure record | RULED 2026-10-21 (12:34): guard with the owner's signing key: its public half in the network config, one certificate per server id; members accept infrastructure records only from certified ids | e21 |

## Staged revisions (ruled; applied to the spec 2026-10-21)
Run-by-run evidence for S13, S14, S21, S24, S25, S26, S27: "Evidence by item".

| # | Ruling | Spec sections | Kind |
|---|---|---|---|
| S1 | public card fields stay in Presence (and in Introduce), capped at 3072 bytes so the largest Presence fits the default 4096-byte gossip frame with headroom (owner ruling) | 3.5, 3.7, 3.14 | spec edit |
| S2 | QUIC idle timeout 10 s | 3.14, D18-D20 | spec edit |
| S3 | relay behind a cargo feature | D14, D35, 3.2 | spec edit + stack D-record |
| S4 | cards parsed with yaml-rust2 plus our own converter | 3.5 | stack D-record |
| S5 | hex for every text form | 3.1, 3.3, 3.13 | spec edit |
| S6 | no presence encryption | 1.6, D13 | spec edit (record the reason) |
| S7 | iroh-gossip pinned to 0.101.0, upgrade when the churn fixes ship | D11 | stack D-record |
| S8 | direct protocol written by hand; irpc rejected | 3.6 | stack D-record |
| S9 | sender checks gossip payload size before broadcast | 3.7, 3.16 | spec edit (follows from E1/E2; confirm with S1) |
| S10 | one stream per request on a cached connection; server read timeout 10 s resets only the stalled stream | 3.6, 3.10, 3.14 | spec edit |
| S11 | drop duplicate suppression: D25 and the 10,000-id set removed; the binary never retries a send | D25, 3.6 step 7, 3.14, test 17 | spec edit |
| S12 | per-member announcement seq = max(now_ms, last+1); keep the highest; ignore older than a max age of 300 s (owner ruling); tombstone a departed member's last seq for the max age | 3.4, 3.7, 3.8, 3.14, test 20 | spec edit |
| S13 | re-announce presence on relay status change and on address change | 3.8 | spec edit |
| S14 | node as identity: the card key is the iroh endpoint key; one live holder per card by an exclusive lock on the card's key file, taken before the endpoint is built; Claim struct, tie-break, Displaced state retired; gossip payloads keep a signature by the card key | D2-D7, D10, D17, D30, D32, 3.4, 3.6, 3.7, 3.8, 3.9, 3.10, 3.15 tests 6, 18, 20, 3.16 | spec edit (largest) |
| S15 | first Presence only after iroh-gossip reports the topic joined | 3.8 init row, D18 | spec edit |
| S16 | every gossip connection, incoming and outgoing, passes a keyed challenge in both directions inside iroh's after_handshake hook before the gossip layer sees it (owner confirmed: hook, both ways); gate key derived from the secret; 3 s timeout | D13, D17, 3.4, 3.6, 3.7, 3.14 | spec edit |
| S17 | stop: broadcast Departure, release the card lock at once, wait a fixed 5 s, exit | D19, 3.8 stop row, 3.9, 3.14 | spec edit |
| S18 | inbox stays a ring of the last 1000 messages; the sender's ack means delivered into the inbox, never read; no gap marker, no read tracking; gaps, limits, and what fits a context are the bridge's business | D21, D22, 3.11, 3.12 | spec edit |
| S19 | no transport-level retries anywhere; a failed send returns its error as is (timeout, unreachable, refused), and the held connection is dropped on failure | 3.10, 3.14, 3.16 | spec edit |
| S20 | address lookup: every Presence and Introduce carries the sender's full current endpoint address (id, relay URL, direct addresses); a receiver replaces that member's entry in its lookup (replace, never merge); gossip's built-in lookup alone is not relied on (it learns only direct neighbors, e8); bootstrap addresses ride in the invitation; no pkarr, no DNS; mDNS deferred (D15 moves to Deferred) | D15, 3.3, 3.7, 3.8, 3.10, 3.13, 2.12 | spec edit |
| S21 | infrastructure: several machines the owner runs, each one relay plus one bootstrap node; each joins the topic through the secret gate and broadcasts a signed infrastructure record (relay URL, bootstrap address); members add the relay at runtime (iroh 1.3 Endpoint::insert_relay) and save the learned hosts into the machine's network config for later sessions; an invitation needs one reachable host; no restart or config edit on members when a host is added | D14, D43, 3.3, 3.7, 3.13, 3.14 | spec edit + experiment (self-announcing hosts) E21: measured on macOS; host authority ruled under C19 |
| S22 | unfixed parameters, agreement ones only: groups G1 wire, G2 files and names, G3 local contracts, G4 formats between machines approved as drafted in research/stack/parameters.md (22 values); six choices named in the spec as the implementer's call | 3.1, 3.3, 3.5, 3.6, 3.7, 3.8, 3.9, 3.10, 3.11, 3.13 | spec edit |
| S23 | MCP surface values G5 (23-29) approved as drafted in research/stack/parameters.md | 3.10, D30 | spec edit |
| S24 | gossip stall on relay death: keep iroh-gossip 0.101.0; a member whose Presence flow stops rejoins the topic through the known hosts; a member whose home relay dies removes it from the relay map (iroh 1.3 Endpoint::remove_relay, a relay-map change forcing a full report) and adds it back when its host announces again | D14, D20, 3.8, S7, S21 | spec edit + experiment (E12): removal measured; the rejoin lever (L2) is replaced by S26 |
| S25 | stop on end of stdin: the binary reads stdin through its own wrapper that notes end of input and starts Stopping at once (Departure handed to a task), before rmcp's 5 s drain; SIGTERM sends Departure first, then shuts the runtime down in the background | 3.8 stop row, 3.9, 3.14, S17 | spec edit; measured macOS and Linux |
| S26 | relay-death workaround (C16): a member that hears no Presence from any peer for 3 s closes its endpoint, binds a fresh one with the same key, builds a new gossip instance, and rejoins the topic through the known hosts; at most one rebind per 20 s; connections drop at each rebind. Replaces S24's rejoin lever (L2). The full fix adds: at rebind, probe the configured relays and strip the dead ones from other members' lookup entries (C2); a 5 s join timeout; the on_join backport at the bootstrap node (C18); MJOIN (after a rebind, join the other members directly while none is a neighbor). RULED 2026-10-21 by the owner: the full fix (MJOIN included) goes toward the spec; spec edit is the owner's act | D14, D20, 3.8, S21, S24 | spec edit |
| S27 | infrastructure records carry a certificate: the owner holds one signing key, its public half sits in the network config; the owner signs each server's endpoint id once; a member accepts an infrastructure record only if the certificate verifies and the record is signed by the certified id (E21's design, unit-tested: wrong signer and non-server member both rejected); replay: a member keeps the newest `ts` per host, persisted in the config, and drops records not newer or more than 300 s ahead; a host makes `ts` as max(last + 1, now) (team-lead decision, 2026-10-21, applied in the spec) | D16, S21, C19 | spec edit |

## Evidence by item
Bullets in date order. [macOS] = the Apple M2 Max host; [Linux] = the lab VM (Ubuntu 24.04.4, 2 vCPU AMD EPYC-Rome), run by lab-sibling. Every run is one host on loopback. Paths are under research/stack/.

### S13
- [macOS] E23 part 2 (experiments/e23-lock, spike/e23-lock 006cbfe): no change gives no watch_addr item and no announcement (0 in 100 s); network_change() gives none (0 in 15 calls); a stopped home relay gives a home_relay_status item in 0.3-1.3 ms, but watch_addr stays silent for 36.7-43.0 s (4 of 5 runs), then peers see the new address 1.2-1.6 ms after it fires. S13 as staged announced about 20 times in 12 s while a relay was down, the address unchanged; applied with a debounce (announce only on a changed address set, at most once per 2 s; journal 2026-10-21 16:27). A real interface change was not produced (no sudo).
- [owner-assisted] The Wi-Fi switch to a phone hotspot changed the public address; pairs resumed within 0.9 s, no rebind (S26 below).

### S14
- [Linux] E23 part 1 (experiments/e23-lock, spike/e23-lock 006cbfe; Hetzner cpx22, ext4, kernel 6.8.0-138): second instance refused 5/5; lock free 154-232 us after SIGKILL 5/5; restart 5/5; free with a live forked child 5/5. A Docker Desktop Linux VM agrees (free 64-404 us).
- [Linux] E23 power cut (hard reset, n=5 per variant, 20 cuts): the lock was free on every boot; a key file written without fsync and cut 1.1-1.7 s later was missing 5/5 (the identity would be lost); with fsync of file and directory, intact 5/5; without fsync, intact 5/5 at about 11 s and 5/5 at about 46 s; never empty, never half-written. Applied: the sidecar is written with sync_all on the file and its directory before the lock is taken (journal 2026-10-21 16:27, 16:31).

### Relay TLS (D51)
- RULED 2026-10-21 14:35: "if low-effort high-benefit, we commit to https."
- [cross-machine] E22 (experiments/e22-two-machines, spike/e22-two-machines 966d653; the Mac behind a home NAT and the lab VM): iroh-relay 1.3.0 with a 10-line TOML obtained a trusted Let's Encrypt certificate 9.15 s after first start, ports 80 and 443 TCP only; a restart loads the cached certificate in 2 ms. HTTPS adds 87 ms to coming online (501 against 414 ms) and nothing measurable to connect, echo, upgrade, or gossip. A relay without QUIC address discovery costs 3.1 s to come online when IP transports are on; with it (UDP 7842), 0.49 s. On the wire HTTPS shows only the hostname (SNI). Dials both ways through the NAT 20/20, relay-only and direct; gossip 120/120 each way over 120 s.
- Written into the spec as D51 (journal 2026-10-21 14:44). The library takes Let's Encrypt as CertConfig::LetsEncrypt (iroh-relay-1.3.0 server.rs:529-531, checked); QUIC address discovery in the library is unconfirmed (known limitation 4).

### S27
- E22-E25 did not exercise the host certificate: E24's hosts broadcast signed infrastructure records with no certificate check. The evidence stays E21's unit tests (C19 below).

### C13 and S24
- [macOS] e8, e10: relay death (not bootstrap death) stalls gossip mesh-wide; re-home ~40 s (surface map, TRANSPORT).
- [Linux] lab/linux/linux-e10: the stall and the ~41 s re-home hold on Linux.
- [macOS] E12: relay removal re-homes in 0.7 s instead of ~42 s, no false removal; own rejoin does not heal (0/62 stalled pairs), because every fresh dial after the kill times out, members on the live relay included (C16).
- [Linux] lab/linux/linux-e12: Linux agrees: L1 0.7 s 9/9, rejoin heals 0/38, every fresh dial after ~4 s times out.
- S26 replaces S24's rejoin lever (L2), which heals 0-2/24 and, with a re-dialing gossip, feeds the block (E16).

### C14 and S25
- [macOS] e11: SIGTERM hangs on a pending stdin read; shutdown_background exits in 1 ms idle. [Linux] lab/linux/linux-e11: the same, every check and timing.
- [macOS] e13: own stdin reader measured: departure at once, exit unchanged; departure under 1 ms in all cases, exit unchanged (5 s eof drain, 2 s cancel drain).
- [Linux] lab/linux/linux-e13: Linux agrees: departure 0 ms median in all 12 cases, exits unchanged.

### C16
- Mechanism, [macOS] e14 (source lines checked; main checked 2026-10-20): iroh 1.3.0's single RelayActor awaits the dead relay's full 64-slot queue (actor.rs:1136-1137, 1207-1233, 1359), and the dead relay's actor never drains it while it backs off (sleep_backoff reads only the priority inbox, actor.rs:366-389); recovery only when that actor exits after 60 s idle (actor.rs:68); any traffic toward the dead relay restarts it. Same code on iroh main today. remove_relay and lookup removal do not clear it; not sending toward the dead relay before its queue fills prevents it.
- [Linux] lab/linux/linux-e14, the E14 two-process repro (raw SUMMARY lines checked): Linux agrees. p1 onset 18.0 s 3/3, no recovery in 90 s; p5 4.0 s; ctl and p1s5 clean; p1s25 recovers at 76.1 and 87.1 s (macOS 73.7 s).
- [Linux] lab/linux/linux-e14w, 180 s windows (n=3+1 per cell): Linux agrees. Dead-relay actor exits at 61-80 s, each pair's first fresh dial ok 0.1-2.4 s after the later member's exit; but gossip heartbeats resume in 0/16 stalled pairs within 180 s, as on macOS: clearing the block does not by itself heal gossip, so the workaround must pair with S24's rejoin. The earlier Linux 60 s note (lab/linux/linux-dial-after-60s) was a window effect (retracted by lab-sibling).
- [macOS] Bug report written (experiments/e14-bug-report.md, spike/e14-bug-report 983118f): single-file repro e14-repro, 20/20 FAIL on 1.3.0 and on iroh main 55908a5 (1.4.0, actor.rs byte-identical), controls pass; trigger is a count (about 65+ datagrams toward the dead relay), not a rate; queue 64/64 and RelayActor blocked observed through a diagnostic patch; candidate upstream fixes drain-in-backoff and drop-on-full both 20/20 PASS.
- [macOS] Reproduced here 2026-10-21 (1 FAIL, 1 no-kill PASS); source lines rechecked (journal receipt, 2026-10-21 00:54).
- [macOS] Member-side workaround measured (e14-bug-report section 8): close and rebind the endpoint with the same key, X-Y ok 7.4 s after the burst, 3/3.
- [Linux] lab/linux/linux-e14-repro, the single-file repro (raw counts checked): stock 19/20 FAIL, main 18/20 FAIL, controls pass, drain and drop 20/20 PASS; 6/48 burst runs never stalled (queue not filled), so on this VM the trigger is a race near 200, not certain; rebind recovered at 5.4 s (2 runs).
- [macOS] E15 (experiments/e15-workaround, spike/e15-workaround 37b0a1a, gossip harness scenario 1): W1 rebind (close endpoint, bind fresh with same key, new Gossip, re-subscribe via bootstrap, on 3 s heartbeat silence, debounce 20 s) heals 76/76 stalled pairs by 180 s against 0/24 (none) and 2/24 (L1+L2); 7/13 runs fully healed by 31 s, 5/13 relapse and heal on a second rebind at 91-170 s (cause open); 0 false rebinds (3 no-kill runs); transport fixed (drain diagnostic) never stalls, 0/6.
- [Linux] lab/linux/linux-e15 (n=3 per arm, Linux-only table checked): Linux agrees. W1 heals 18/18 (T=3 s), 18/18 (T=10 s), 12/12 (W1+L1); none 0/12, L1+L2 0/18; first rebind 6.9-14.0 s, heartbeat a median 4.2 s after; relapse 1/8 stalled W1 runs; 0 rebinds without a kill; drain-fixed transport 0/6 stalled. Staged as S26.
- Options as first laid out (kept): (a) report upstream to n0 with E14's reproduction; (b) carry a patched iroh (the backoff drains or drops queued datagrams); (c) a member-side workaround: stop all traffic toward peers homed on a relay seen dead (gossip's own dials and open connections included), unmeasured; owner decides. Taken: a workaround (built as the rebind, S26) and the report, which the owner posts (ruled 2026-10-21).

### C17
- [macOS] e15 (source reading plus 2 traced runs; Pending state not instrumented): net.rs:436-443 sends only PeerDisconnected; the Pending entry stays with its queue; net.rs:695-699 dials only on an empty queue; the entry is removed only on DisconnectPeer (net.rs:732-736), which hyparview emits for active peers only (proto/hyparview.rs:346-354, 648-655); so after the transport recovers (fresh dials ok at 72-161 s) heartbeats resume in 2/48 stalled pairs and L2's join_peers starts no dial (trace: 13 JoinPeers, last dial start at 31-44 s). Source lines checked 2026-10-21.
- [macOS] E16 (experiments/e16-redial, spike/e16-redial c86fdbf): the one-place fix (drop the Pending entry on a failed dial) heals nothing: without levers the path is never reached (no sends after the kill; trace: 0 dials), with L2 it re-dials every 4-5 s and keeps the C16 block alive (0/30 prober pairs recover, against 10/18 with L2 and no fix); with W1 it heals as W1 alone (30/30, 2/5 relapse).
- RECORD: upstream iroh-gossip main already fixes the path (PR #178, f02f79e8, merged 2026-10-14; handle_in_event_inner drops the entry on every PeerDisconnected, net.rs:779-783 at c5f44db, checked 2026-10-21), unreleased after v0.101.0.

### C18
- Mechanism (source lines checked 2026-10-21): a ForwardJoin renewal sent a second Neighbor before the kill, which the member took as the reply to its own (hyparview.rs:403-409, 450-452), and send_neighbor sends nothing for a pending peer (745-752); the dead old connection's idle close does not clear it while the new connection exists (net.rs:575-588, 790-799).
- [macOS] e18 (experiments/e18-attach, spike/e18-attach 85ef8a2, n=8 per arm, vendored gossip logging): 11/11 Joins in that state unanswered without the fix (the 4 with the fix got a Neighbor at once), 0/66 others; waits 57-60 s, ended by the harness's 60 s join timeout (6/7) or a shuffle (1/7).
- [Linux] lab/linux/linux-e18 (n=8 per arm interleaved, table checked): Linux agrees. Late joins 6/24 (57.0-60.0 s), join timeout 8/24 at exactly 5.0 s, on_join fix 0/24; all flowing from median 75.5, 30.6, 27.1 s; one B2 trace shows the same active+pending state and no Neighbor; 0 rebinds in 4 no-kill runs.
- RECORD: fixed on iroh-gossip main by PR #161 (fcd81ae8, 2026-10-09: on_join drops the pending request; checked at c5f44db hyparview.rs:549-558), unreleased.

### C19
- [macOS] e21 (harness design, unit tests; replay of an old record untested): every secret holder is equal (D16), so any member could broadcast an infrastructure record and pull members onto its relay; and insert_relay of an announced relay can move a member's home relay (11/15 in E21).
- Threat named to the owner before the ruling: no confidentiality loss (end-to-end encryption), but availability (re-homing onto a fake relay, or announcing dead relays to trip C16) and metadata.

### S21
- [macOS] E21 (experiments/e21-hosts, spike/e21-hosts bb66277, n=5 per arm): a host added at runtime reaches every member in 24-114 ms with no restart; insert_relay forces a full net report with no home hysteresis (socket.rs:1232-1243, 1759-1762; net_report.rs:304, 321, 811-815, checked), so the new relay became home in 11/15 members; after the first host's death all 15 members re-home on the learned relay, rejoin through its bootstrap in 9-14 ms, flowing at 5.9-7.0 s, no second rebind (learned vs static not separable: confounded); a member knowing only the new host joins in 9-11 ms, heartbeats in 105-216 ms; a saved hosts file (ids and relay URLs only) lets a restarted member join (n=1); false records rejected at unit level, which needs a host authority the spec lacks (C19).
- [Linux] lab/linux/linux-e21 (n=5 per arm, table checked): H1 learned in 136-163 ms, home moved to the new relay in 6/15 members, 0 rebinds before the kill; H2 15/15 rebound once, all flowing from 6.8-6.9 s, 0/5 second rebinds; controls z0w 13.2-89.2 s (median 75.2, 4/5 second rebinds), z6w 13.1-81.5 s (1/5), z5w never stalled; H3 joined in 58-65 ms, heartbeats from all in 113-147 ms (3 runs) or about 1.04 s (2); saved hosts: rejoin with the file, none without; 5 unit tests pass. Joins slower than macOS (58-144 ms vs 9-14 ms) on this VM before the kill as well.
- [cross-machine] E24 (experiments/e24-cross, spike/e24-cross 3f68e41; the Mac behind a home NAT, the lab in Nuremberg, a throwaway server in Helsinki; HTTPS relays): a host started at runtime was known to all four members 319-379 ms after its process start (n=5, clock offsets about +-50 ms); failover after the first host's death did not work cleanly while hosts applied the deaf rule (6-12 pairs stalled per run, one run unrecovered at 90 s). A fresh member knowing only the second host joined in 290-304 ms and heard its first heartbeat 948-1034 ms after start (n=5). Quiet network, 604 s: 0 rebinds of members or hosts (n=1).

### S26
- Base W1, measured (gossip harness, scenario 1): heals every stalled pair by 180 s, [macOS] 76/76 (E15) and 30/30 (E16 F3), [Linux] 48/48 (linux-e15); heartbeat a median 4.1-4.2 s after the rebind; relapse needing a second rebind in 5/13 [macOS] and 1/8 [Linux] runs, cause open at the time; 0 false rebinds in 6 no-kill runs.
- [macOS] Relapse causes (E17, experiments/e17-relapse, spike/e17-relapse 83d75bd, diagnostic build): (1) the rebound endpoint learns a peer's pre-kill path through the dead relay from the lookup in its first second, keeps it (remote_state paths only grow), and with no open connection to that peer sends each fresh-dial datagram on every known path (remote_state.rs:805-830, checked), so the dead relay's queue refills to 64 and blocks again 57.2-57.4 s after the rebind: 4/4 endpoints without a connection to that peer, 0/12 with one; the 1/s feed is the harness probers, real-member rate unknown. Counter C2 (probe configured relays by TCP at rebind, strip relays found down from other members' lookup entries, 24 lines) removes the block (0/25 endpoints) but not the relapses (2/6 stalled runs against 3/8). (2) late attachment: after a rebind the bootstrap node still holds the old connection's peer in its active view and sends no Neighbor to the re-Join (one trace; hyparview pending_neighbor_requests reading, not instrumented), 57-120 s without a neighbor; frequency 0/24 endpoints in W1, 2/18 with C2, 4/9 with a fresh lookup.
- [Linux] lab/linux/linux-e17 (n=8 per arm, table checked): both relapse causes seen once each; W1 2/8 second rebinds (1 with the 58 s re-block at m1conn 0; 0/11 blocked with an M1 connection); W1+C2 0/8 second rebinds, 0 blocks. In the Linux late-attachment run all three members rebound exactly 60.0 s after the first rebound with silent_ms=0: the harness join timeout (main.rs:352, subscribe_and_join under timeout 60 s, JOIN_FAIL then rebind), so a 60 s wait for a first neighbor is ended by the harness, not by gossip.
- [macOS] Late attachment explained (C18, E18): stale pending Neighbor request at the bootstrap node; counters measured: join timeout 5 s then rebind (member), on_join backport of iroh-gossip #161 (bootstrap).
- [macOS] Combined (E19, experiments/e19-combined, spike/e19-combined b1357d1, n=8 per arm interleaved): W1 + C2 + 5 s join timeout + on_join backport: 0/8 runs with a second rebind, 42/42 pairs flowing, all flowing from 29.3-33.2 s, 0 false rebinds in 3 no-kill runs; W1 alone 3/8 relapse, one run never fully healed, fast heals 15.7-18.6 s otherwise (B1 vs B0 p=0.20); without C2 5/8 relapse, median 141 s (C2 earns its lines, p=0.026). Cost: about 14 s slower first heal than W1 alone when W1 does not relapse (cause open); re-blocks in the stock build inferred from timing only.
- [Linux] lab/linux/linux-e19 (n=8 per arm interleaved, table and stats checked): Linux agrees on E19. Combined 0/8 second rebinds, 48/48 flowing, median 20.4 s (14.2-34.7); W1 alone 2/8; without C2 4/8 (B1 vs B2 p=0.077 on Linux alone; both OSes together 0/16 vs 9/16 runs with a second rebind).
- [macOS] Slower first heal explained (E20, experiments/e20-delay, spike/e20-delay 9abb991): not relay homing (home relay connected 401-404 ms after bind in 42/42 endpoints); a rebound member with no member neighbor hears heartbeats only through the bootstrap node's lazy push, 11-16 s late (21/21 association in E19, 21/21 in the rerun); no member neighbor because the join walk ends at the bootstrap node, which holds the member active and only renews (hyparview.rs:403-409, 428-434, checked), or because member dials fail on emptied address entries (co-occurrence only). Change MJOIN (13 lines: 1.5 s after JOINED, with no member neighbor, join_peers(other members) every 3 s): late endpoints 0/24 vs 5/21 (p=0.017), 0/8 second rebinds; run-level heal not shown faster (p=0.34). Needs a member list in the product.
- [Linux] lab/linux/linux-e20 (n=8 per arm interleaved, stats checked): late endpoints rarer (1/22 in B1, no member neighbor), MJOIN 0/21; with MJOIN all flowing from median 15.9 s (14.1-19.0) against 20.1 s (17.2-40.4), p=0.008; 0 second rebinds either arm; home relay 402-410 ms.
- Variance: the same B1 arm gave macOS medians 31.2 s (E19) and 18.0 s (E20): run-set variance on one host is that wide.
- Trigger: the spec-apply found that a 3 s Presence trigger would rebind a quiet healthy network every 20 s (Presence has no timer); the owner ruled (2026-10-21 15:15) deaf = no gossip payload from anyone for 15 s, the hosts' 5 s records as the floor.
- [cross-machine] E24 (experiments/e24-cross, spike/e24-cross 3f68e41), relay-only, kill one host, n=8 per arm: hosts applying the deaf rule and the 5 s join timeout, with their only fellow host dead, rebound about every 8 s and pulled members along: the 15 s trigger healed 1 of 8 runs within 120 s, against 8 of 8 for the 3 s heartbeat control (12.8-27.7 s); rebinds per member 7.09 against 1.50 (p = 0.001). With IP transports on, the control never stalled (0/4) and the spec arm stalled 4/4, an outage the rule made. Hosts without the rule (n=4): one rebind per member, 26.9-27.2 s after the kill, all pairs flowing from 32.6-33.1 s, 0 relapses; blocked members heard 2 more host records, about 12 s, before going deaf. Spec changed: hosts never rebind on silence and have no join timeout (2026-10-21 18:54).
- [cross-machine] E25 (experiments/e25-hosts-norule, spike/e24-cross 2c81e82 per the journal receipt), hosts without the rule, n=8 per arm: IP transports on, 0 of 16 runs stalled in either arm. Relay-only, the 15 s trigger and the 3 s control did not differ (heal p = 0.50), both dominated by a blocked surviving host: 10 of 21 relay-only runs, each 62.6-87.3 s (known limitation 1). Without the blocked host the 15 s trigger healed at 32.4-33.1 s in 4 of 5 runs; the fifth (`e25r-sp-3`) stalled 8 of 12 pairs for 71 s while records kept arriving (known limitation 2).
- [owner-assisted] Sleep and wake (journal receipt 2026-10-21 22:55; logs/e24/owner-sleep-1021-2246 in the e24 worktree): the Mac slept 247.5 s; on wake each Mac member rebound at once and re-homed on the relay; all 10 pairs flowing within 3.7 s; 0 join failures.
- [owner-assisted] Wi-Fi to a phone hotspot (journal receipt 2026-10-21 23:02; logs/e24/owner-wifi-1021-2256): public address changed, down about 4.8 s; no member rebound; peers heard the Mac members 0.3-0.4 s after the new address; all 10 pairs resumed within 0.9 s. At the window's end the remote side was not hearing the Mac, most likely the owner's switch back to home Wi-Fi (not confirmed).
