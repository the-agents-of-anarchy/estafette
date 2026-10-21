//! E24 cross-machine harness: member and host programs for the relay-death workaround as D46 now states it.
//! Modes: `host <seed> <relay_url> [<via_seed>=<via_url>]`, `member <name> <seed> <hosts> <others>` with `hosts` = `seed=url,seed=url` and `others` = comma list of the other members' seeds.
//! Switches (env): `E24_TRIG` = `spec` (default; every received gossip payload except the test heartbeat Hb resets the clock, T = `E24_TRIG_MS` default 15000) or `hb` (control: only Hb resets the clock, T default 3000);
//! `E24_RELAYONLY` clears IP transports; `E24_JOIN_T_MS` (5000); `E24_MJOIN_MS` (1500); `E24_HOST_DEAF` (hosts apply the deaf rule, counting only other hosts); `E24_NOPROBE`.
//! Output: `EV <unix_ms> <who> <kind> <rest>` lines on stdout. Derived from the E21 harness (spikes/e10-gossip-main/src/main.rs, commit bb66277).
use std::{
    collections::HashSet,
    fmt::Display,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayConfig as MapRelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Signature, TransportAddr, Watcher,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, QuicTransportConfig, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ECHO: &[u8] = b"e24/echo/0";

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

fn ev(who: &str, kind: &str, rest: impl Display) {
    println!("EV {} {who} {kind} {rest}", now_ms());
}

fn short<E: Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(110).collect::<String>().replace(' ', "_")
}

fn key(n: u8) -> SecretKey {
    SecretKey::from_bytes(&[n; 32])
}

fn topic() -> TopicId {
    TopicId::from_bytes([24; 32])
}

fn id_name(id: &EndpointId) -> String {
    for n in (1..=2u8).chain(11..=30u8) {
        if key(n).public() == *id {
            return if n < 10 { format!("H{n}") } else { format!("M{}", n - 10) };
        }
    }
    id.fmt_short().to_string()
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn urls_str<'a>(it: impl Iterator<Item = &'a RelayUrl>) -> String {
    it.map(|u| u.to_string().replace("https://", "").replace(".sslip.io", "").replace('/', "")).collect::<Vec<_>>().join(",")
}

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(AcceptError::from_err)?;
        let data = rx.read_to_end(1 << 16).await.map_err(AcceptError::from_err)?;
        tx.write_all(&data).await.map_err(AcceptError::from_err)?;
        tx.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
enum Msg {
    Announce { from: String, addr: EndpointAddr, n: u64 },
    Hb { from: String, seq: u64 },
    Infra { rec: Vec<u8>, sig: Vec<u8>, cert: Vec<u8> },
}

// ---------------- infrastructure records (as E21) ----------------

fn operator() -> SecretKey {
    key(99)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct InfraRec {
    host: [u8; 32],
    relay: String,
    boot: EndpointAddr,
    ts: u64,
}

fn rec_msg(rec: &[u8]) -> Vec<u8> {
    [b"e21-rec".as_slice(), rec].concat()
}

fn cert_msg(host: &[u8; 32]) -> Vec<u8> {
    [b"e21-cert".as_slice(), host.as_slice()].concat()
}

fn make_cert(host: &EndpointId) -> Vec<u8> {
    operator().sign(&cert_msg(host.as_bytes())).to_bytes().to_vec()
}

fn make_infra(host: &SecretKey, relay: &RelayUrl, cert: Vec<u8>, ts: u64) -> Option<Msg> {
    let id = host.public();
    let rec = InfraRec { host: *id.as_bytes(), relay: relay.to_string(), boot: EndpointAddr::new(id).with_relay_url(relay.clone()), ts };
    let bytes = postcard::to_stdvec(&rec).ok()?;
    let sig = host.sign(&rec_msg(&bytes)).to_bytes().to_vec();
    Some(Msg::Infra { rec: bytes, sig, cert })
}

fn sig_of(b: &[u8]) -> Option<Signature> {
    Some(Signature::from_bytes(<&[u8; 64]>::try_from(b).ok()?))
}

fn verify_infra(rec: &[u8], sig: &[u8], cert: &[u8], op: &EndpointId) -> Result<(InfraRec, EndpointId, RelayUrl), &'static str> {
    let r: InfraRec = postcard::from_bytes(rec).map_err(|_| "bad record")?;
    let host = EndpointId::from_bytes(&r.host).map_err(|_| "bad host id")?;
    op.verify(&cert_msg(&r.host), &sig_of(cert).ok_or("bad cert length")?).map_err(|_| "not a host: operator certificate invalid")?;
    host.verify(&rec_msg(rec), &sig_of(sig).ok_or("bad sig length")?).map_err(|_| "signature does not match host id")?;
    let relay: RelayUrl = r.relay.parse().map_err(|_| "bad relay url")?;
    if r.boot.id != host {
        return Err("bootstrap address is not the host's");
    }
    Ok((r, host, relay))
}

type Known = Arc<Mutex<Vec<(EndpointId, RelayUrl)>>>;

fn parse_hosts(s: &str) -> Vec<(EndpointId, RelayUrl)> {
    s.split(',')
        .filter_map(|p| p.split_once('='))
        .filter_map(|(seed, url)| Some((key(seed.parse().ok()?).public(), url.parse().ok()?)))
        .collect()
}

fn quic() -> QuicTransportConfig {
    QuicTransportConfig::builder().max_idle_timeout(Some(Duration::from_secs(10).try_into().expect("idle"))).build()
}

fn relay_only() -> bool {
    std::env::var("E24_RELAYONLY").is_ok()
}

async fn relay_up(u: &RelayUrl) -> bool {
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else { return true };
    timeout(Duration::from_secs(1), tokio::net::TcpStream::connect((host, port))).await.is_ok_and(|r| r.is_ok())
}

/// D46 relay scrub: remove the relay URLs in `down` from the lookup entries of the other members.
fn scrub(lk: &MemoryLookup, down: &HashSet<RelayUrl>, others: &[u8]) {
    for id in others.iter().map(|t| key(*t).public()) {
        let Some(info) = lk.get_endpoint_info(id) else { continue };
        let mut addr = info.into_endpoint_addr();
        addr.addrs.retain(|a| !matches!(a, TransportAddr::Relay(u) if down.contains(u)));
        lk.set_endpoint_info(addr);
    }
}

async fn sleep_forever() -> Res<()> {
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

// ---------------- host: bootstrap node of one host (the relay is the iroh-relay binary beside it) ----------------

async fn host(seed: u8, relay: RelayUrl, via: Option<(EndpointId, RelayUrl)>) -> Res<()> {
    let who = format!("H{seed}");
    let deaf_rule = std::env::var("E24_HOST_DEAF").is_ok();
    let t_deaf = env_u64("E24_TRIG_MS", 15_000) as u128;
    let join_t = env_u64("E24_JOIN_T_MS", 5000);
    let other_host_known = Arc::new(std::sync::atomic::AtomicBool::new(via.is_some()));
    let mut last_rebind: Option<Instant> = None;
    let mut n = 0u32;
    loop {
        let lk = MemoryLookup::new();
        let mut b = Endpoint::builder(Minimal).relay_mode(RelayMode::Custom(RelayMap::from(relay.clone()))).secret_key(key(seed)).transport_config(quic());
        if relay_only() {
            b = b.clear_ip_transports();
        }
        if let Some((vid, vurl)) = &via {
            lk.add_endpoint_info(EndpointAddr::new(*vid).with_relay_url(vurl.clone()));
            b = b.address_lookup(lk.clone());
        }
        let ep = b.bind().await?;
        let gossip = Gossip::builder().spawn(ep.clone());
        let _router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).spawn();
        let t_join = Instant::now();
        let th = match &via {
            Some((vid, _)) => {
                let j = gossip.subscribe_and_join(topic(), vec![*vid]);
                if deaf_rule {
                    match timeout(Duration::from_millis(join_t), j).await {
                        Ok(r) => Some(r?),
                        Err(_) => None,
                    }
                } else {
                    Some(j.await?)
                }
            }
            None => Some(gossip.subscribe(topic(), vec![]).await?),
        };
        let Some(th) = th else {
            ev(&who, "JOIN_FAIL", format!("after_ms={}", t_join.elapsed().as_millis()));
            n += 1;
            last_rebind = Some(Instant::now());
            ev(&who, "REBIND", format!("n={n} silent_ms=0"));
            let _ = timeout(Duration::from_secs(5), ep.close()).await;
            continue;
        };
        ev(&who, "READY", format!("id={} joined_ms={}", ep.id().fmt_short(), t_join.elapsed().as_millis()));
        let (tx, mut rx) = th.split();
        let last = Arc::new(AtomicU64::new(now_ms() as u64));
        let (sk, cert, relay2, who2) = (key(seed), make_cert(&ep.id()), relay.clone(), who.clone());
        let sender = tokio::spawn(async move {
            loop {
                if let Some(m) = make_infra(&sk, &relay2, cert.clone(), now_ms() as u64) {
                    if let Ok(b) = postcard::to_stdvec(&m) {
                        let r = tx.broadcast(Bytes::from(b)).await;
                        ev(&who2, "INFRA_SENT", if r.is_ok() { "ok" } else { "err" });
                    }
                }
                sleep(Duration::from_millis(env_u64("E24_REC_PERIOD_MS", 5000))).await;
            }
        });
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let silent = loop {
            tokio::select! {
                e = rx.next() => match e {
                    Some(Ok(Event::NeighborUp(i))) => ev(&who, "NU", id_name(&i)),
                    Some(Ok(Event::NeighborDown(i))) => ev(&who, "ND", id_name(&i)),
                    Some(Ok(Event::Received(m))) => match postcard::from_bytes::<Msg>(&m.content) {
                        Ok(Msg::Hb { .. }) => {}
                        Ok(Msg::Infra { rec, .. }) => {
                            if let Ok(r) = postcard::from_bytes::<InfraRec>(&rec) {
                                if r.host != *key(seed).public().as_bytes() {
                                    other_host_known.store(true, Ordering::Relaxed);
                                }
                            }
                            last.store(now_ms() as u64, Ordering::Relaxed);
                        }
                        _ => last.store(now_ms() as u64, Ordering::Relaxed),
                    },
                    Some(_) => {}
                    None => break 0,
                },
                _ = tick.tick() => {
                    let silent = now_ms().saturating_sub(u128::from(last.load(Ordering::Relaxed)));
                    if deaf_rule && other_host_known.load(Ordering::Relaxed) && silent >= t_deaf && last_rebind.is_none_or(|r| r.elapsed() >= Duration::from_secs(20)) {
                        break silent;
                    }
                }
            }
        };
        sender.abort();
        n += 1;
        last_rebind = Some(Instant::now());
        ev(&who, "REBIND", format!("n={n} silent_ms={silent}"));
        let _ = timeout(Duration::from_secs(5), ep.close()).await;
    }
}

// ---------------- member ----------------

async fn bind_member(seed: u8, urls: &[RelayUrl], lk: &MemoryLookup) -> Res<(Endpoint, RelayMap)> {
    let map = RelayMap::from_iter(urls.to_vec());
    let mut b = Endpoint::builder(Minimal).relay_mode(RelayMode::Custom(map.clone())).secret_key(key(seed)).transport_config(quic()).address_lookup(lk.clone()).alpns(vec![ECHO.to_vec()]);
    if relay_only() {
        b = b.clear_ip_transports();
    }
    Ok((b.bind().await?, map))
}

async fn member(name: String, seed: u8, hosts: String, others: Vec<u8>) -> Res<()> {
    let lk = MemoryLookup::new();
    let known: Known = Arc::new(Mutex::new(parse_hosts(&hosts)));
    let relays_of = |known: &Known| -> Vec<RelayUrl> {
        let mut v: Vec<RelayUrl> = vec![];
        for (_, u) in known.lock().map(|g| g.clone()).unwrap_or_default() {
            if !v.contains(&u) {
                v.push(u);
            }
        }
        v
    };
    let (mut ep, mut relay_map) = bind_member(seed, &relays_of(&known), &lk).await?;
    ev(&name, "BOUND", format!("relays=[{}]", urls_str(ep.addr().relay_urls())));
    let (mut last_rebind, mut n) = (None, 0u32);
    loop {
        let silent = session(name.clone(), seed, known.clone(), relay_map.clone(), lk.clone(), ep.clone(), others.clone(), last_rebind).await?;
        n += 1;
        last_rebind = Some(Instant::now());
        ev(&name, "REBIND", format!("n={n} silent_ms={silent}"));
        let _ = timeout(Duration::from_secs(5), ep.close()).await;
        // D46: probe the known relays, strip the relays found down from the addresses held for other members.
        let mut down: HashSet<RelayUrl> = HashSet::new();
        for u in &relays_of(&known) {
            if !relay_up(u).await {
                down.insert(u.clone());
            }
        }
        ev(&name, "PROBE_RELAYS", format!("down=[{}]", urls_str(down.iter())));
        scrub(&lk, &down, &others);
        let t = Instant::now();
        (ep, relay_map) = bind_member(seed, &relays_of(&known), &lk).await?;
        ev(&name, "REBOUND", format!("n={n} ms={} relays=[{}]", t.elapsed().as_millis(), urls_str(ep.addr().relay_urls())));
    }
}

/// One gossip instance on one endpoint. Returns the silence in ms when the trigger fires (0 for a failed join); `member` then rebinds.
#[allow(clippy::too_many_arguments)]
async fn session(name: String, _seed: u8, known: Known, relay_map: RelayMap, lk: MemoryLookup, ep: Endpoint, others: Vec<u8>, last_rebind: Option<Instant>) -> Res<u128> {
    let mut js = tokio::task::JoinSet::new();
    let t_bind = Instant::now();
    let hb_mode = std::env::var("E24_TRIG").is_ok_and(|v| v == "hb");
    let t_trig = env_u64("E24_TRIG_MS", if hb_mode { 3000 } else { 15_000 }) as u128;
    let hosts = known.lock().map(|g| g.clone()).unwrap_or_default();
    for (id, u) in &hosts {
        lk.add_endpoint_info(EndpointAddr::new(*id).with_relay_url(u.clone()));
    }
    let boot: Vec<EndpointId> = hosts.iter().map(|h| h.0).collect();
    let order = boot.iter().map(|i| id_name(i)).collect::<Vec<_>>().join("+");
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).accept(ECHO, Echo).spawn();
    {
        let (n, e) = (name.clone(), ep.clone());
        js.spawn(async move {
            e.online().await;
            ev(&n, "ONLINE", format!("ms_after_bind={}", t_bind.elapsed().as_millis()));
        });
    }
    let last_rx = Arc::new(AtomicU64::new(0));
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    {
        let (n, e, tx) = (name.clone(), ep.clone(), status_tx.clone());
        js.spawn(async move {
            let mut s = e.home_relay_status().stream();
            while let Some(v) = s.next().await {
                let d: Vec<String> = v.iter().map(|r| format!("relay={} connected={} err={}", urls_str(std::iter::once(r.url())), r.is_connected(), r.last_error().map_or("-".into(), short))).collect();
                ev(&n, "STATUS", d.join(";"));
                let _ = tx.send(());
            }
        });
        let (n, e, tx) = (name.clone(), ep.clone(), status_tx);
        js.spawn(async move {
            let mut s = e.watch_addr().stream();
            while let Some(v) = s.next().await {
                ev(&n, "ADDR", format!("relays=[{}] direct={}", urls_str(v.relay_urls()), v.ip_addrs().count()));
                let _ = tx.send(());
            }
        });
    }
    ev(&name, "JOIN_START", format!("order={order}"));
    let t_join = Instant::now();
    let join_t = env_u64("E24_JOIN_T_MS", 5000);
    let topic_h = timeout(Duration::from_millis(join_t), gossip.subscribe_and_join(topic(), boot.clone())).await;
    let Ok(Ok(th)) = topic_h else {
        ev(&name, "JOIN_FAIL", format!("after_ms={}", t_join.elapsed().as_millis()));
        return Ok(0);
    };
    ev(&name, "JOINED", format!("ms={}", t_join.elapsed().as_millis()));
    last_rx.store(now_ms() as u64, Ordering::Relaxed);
    let (tx, mut rx) = th.split();
    let nbrs: Arc<Mutex<HashSet<EndpointId>>> = Arc::default();
    {
        let (n, lk, nb, lr, kn, ep2, rm) = (name.clone(), lk.clone(), nbrs.clone(), last_rx.clone(), known.clone(), ep.clone(), relay_map.clone());
        js.spawn(async move {
            while let Some(e) = rx.next().await {
                match e {
                    Ok(Event::NeighborUp(i)) => {
                        if let Ok(mut g) = nb.lock() {
                            g.insert(i);
                        }
                        ev(&n, "NU", id_name(&i));
                    }
                    Ok(Event::NeighborDown(i)) => {
                        if let Ok(mut g) = nb.lock() {
                            g.remove(&i);
                        }
                        ev(&n, "ND", id_name(&i));
                    }
                    Ok(Event::Received(m)) => {
                        let via = id_name(&m.delivered_from);
                        match postcard::from_bytes::<Msg>(&m.content) {
                            Ok(Msg::Hb { from, seq }) => {
                                if hb_mode {
                                    lr.store(now_ms() as u64, Ordering::Relaxed);
                                }
                                ev(&n, "RX", format!("hb from={from} seq={seq} via={via}"));
                            }
                            Ok(Msg::Announce { from, addr, .. }) => {
                                if !hb_mode {
                                    lr.store(now_ms() as u64, Ordering::Relaxed);
                                }
                                ev(&n, "RX", format!("ann from={from} relays=[{}] direct={} via={via}", urls_str(addr.relay_urls()), addr.ip_addrs().count()));
                                lk.set_endpoint_info(addr);
                            }
                            Ok(Msg::Infra { rec, sig, cert }) => {
                                if !hb_mode {
                                    lr.store(now_ms() as u64, Ordering::Relaxed);
                                }
                                match verify_infra(&rec, &sig, &cert, &operator().public()) {
                                    Err(why) => ev(&n, "INFRA_REJECT", why.replace(' ', "_")),
                                    Ok((r, host, relay)) => {
                                        ev(&n, "RX", format!("infra host={} ts={} via={via}", id_name(&host), r.ts));
                                        let (is_new, n_hosts) = kn.lock().map_or((false, 0), |mut g| {
                                            if g.iter().any(|(i, u)| *i == host && *u == relay) {
                                                return (false, g.len());
                                            }
                                            g.retain(|(i, _)| *i != host);
                                            g.push((host, relay.clone()));
                                            (true, g.len())
                                        });
                                        if is_new {
                                            lk.add_endpoint_info(r.boot.clone());
                                            let t = Instant::now();
                                            let prev = ep2.insert_relay(relay.clone(), Arc::new(MapRelayConfig::from(relay.clone()))).await;
                                            ev(&n, "HOSTS_LEARNED", format!("host={} relay={} insert_ms={} replaced={} in_map={} n_hosts={n_hosts}", id_name(&host), urls_str(std::iter::once(&relay)), t.elapsed().as_millis(), prev.is_some(), rm.contains(&relay)));
                                        }
                                    }
                                }
                            }
                            Err(e) => ev(&n, "RX", format!("bad {}", short(e))),
                        }
                    }
                    Ok(other) => ev(&n, "EVT", short(format!("{other:?}"))),
                    Err(e) => ev(&n, "EVT", format!("err {}", short(e))),
                }
            }
        });
    }
    // Announce the current address on any home-relay status or address change.
    {
        let (n, e, tx) = (name.clone(), ep.clone(), tx.clone());
        js.spawn(async move {
            let mut first = true;
            loop {
                if !first && status_rx.recv().await.is_none() {
                    break;
                }
                first = false;
                if let Ok(b) = postcard::to_stdvec(&Msg::Announce { from: n.clone(), addr: e.addr(), n: now_ms() as u64 }) {
                    let _ = tx.broadcast(Bytes::from(b)).await;
                    ev(&n, "ANN_SENT", "");
                }
                sleep(Duration::from_millis(300)).await;
            }
        });
    }
    // D46 member join after a rebind: with no other member among the gossip neighbors `T` ms after JOINED, join_peers(other members), again every 3 s until one is a neighbor.
    if let (Some(t_ms), true) = (Some(env_u64("E24_MJOIN_MS", 1500)), last_rebind.is_some()) {
        let (n, tx, nb) = (name.clone(), tx.clone(), nbrs.clone());
        let ids: Vec<EndpointId> = others.iter().map(|t| key(*t).public()).collect();
        js.spawn(async move {
            sleep(Duration::from_millis(t_ms)).await;
            while !nb.lock().map_or(true, |g| ids.iter().any(|o| g.contains(o))) {
                let r = timeout(Duration::from_secs(5), tx.join_peers(ids.clone())).await;
                ev(&n, "MJOIN", format!("join_peers {}", if matches!(r, Ok(Ok(()))) { "ok" } else { "err/timeout" }));
                sleep(Duration::from_secs(3)).await;
            }
        });
    }
    // Test heartbeat, 1 s: measurement only; it resets the trigger clock only in `hb` mode.
    {
        let (n, tx) = (name.clone(), tx.clone());
        js.spawn(async move {
            let mut seq = 0u64;
            loop {
                seq += 1;
                if let Ok(b) = postcard::to_stdvec(&Msg::Hb { from: n.clone(), seq }) {
                    let _ = timeout(Duration::from_secs(2), tx.broadcast(Bytes::from(b))).await;
                }
                sleep(Duration::from_secs(1)).await;
            }
        });
    }
    // Probers: id-only dial to each other member every ~1 s (measurement).
    for t in others.iter().copied().filter(|_| std::env::var("E24_NOPROBE").is_err()) {
        let (n, e) = (name.clone(), ep.clone());
        js.spawn(async move {
            let id = key(t).public();
            loop {
                let t0 = Instant::now();
                let r = timeout(Duration::from_secs(3), async {
                    let c = e.connect(EndpointAddr::from(id), ECHO).await.map_err(short)?;
                    let (mut s, mut r) = c.open_bi().await.map_err(short)?;
                    s.write_all(b"e24").await.map_err(short)?;
                    s.finish().map_err(short)?;
                    r.read_to_end(64).await.map_err(short)?;
                    c.close(0u32.into(), b"done");
                    Ok::<(), String>(())
                })
                .await;
                let ms = t0.elapsed().as_millis();
                match r {
                    Ok(Ok(())) => ev(&n, "PROBE", format!("tgt={} ok ms={ms}", id_name(&id))),
                    Ok(Err(x)) => ev(&n, "PROBE", format!("tgt={} fail ms={ms} err={x}", id_name(&id))),
                    Err(_) => ev(&n, "PROBE", format!("tgt={} fail ms={ms} err=timeout3s", id_name(&id))),
                }
                sleep(Duration::from_secs(1)).await;
            }
        });
    }
    // D46 deaf rule: a node that knows another node (always true: a host is known) and has counted no payload for T ms rebinds; at most one rebind per 20 s.
    loop {
        sleep(Duration::from_millis(250)).await;
        let silent = now_ms().saturating_sub(u128::from(last_rx.load(Ordering::Relaxed)));
        if silent >= t_trig && last_rebind.is_none_or(|r| r.elapsed() >= Duration::from_secs(20)) {
            return Ok(silent);
        }
    }
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let g = |i: usize| a.get(i).cloned().unwrap_or_default();
    let r = match g(1).as_str() {
        "host" => {
            let via = parse_hosts(&g(4)).into_iter().next();
            match g(3).parse() {
                Ok(url) => host(g(2).parse().unwrap_or(1), url, via).await,
                Err(_) => Err("bad relay url".into()),
            }
        }
        "member" => {
            let others = g(5).split(',').filter_map(|s| s.parse().ok()).collect();
            member(g(2), g(3).parse().unwrap_or(11), g(4), others).await
        }
        _ => Err("usage".into()),
    };
    if let Err(e) = r {
        println!("EV {} {} FATAL {}", now_ms(), g(1), short(e));
    }
    let _ = sleep_forever;
}
