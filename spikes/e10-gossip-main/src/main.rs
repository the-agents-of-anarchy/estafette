//! E10 relay death vs iroh-gossip main. Harness copied from E8 (spike/e8-failover, commit 619b57f); the dependency under test
//! is chosen by cargo feature: `a` = iroh-gossip =0.101.0 (as E8), `b` = iroh-gossip git main at the pinned rev.
//! Changes against E8: gossip alias, split relay/bootstrap modes plus scenario 7 (kill B1 only, R1 alive), output dir
//! `/tmp/e10`, variant in the run tag, `E10_RUN0` run-number offset, extra measures (HB after re-home, last HB), and
//! `E10_WINDOW_MS` (default 88000, the E8 window; the supplementary long-window runs set 180000).
//! Modes: `relay <n> <port>` (relay + bootstrap), `relayonly <n> <port>`, `boot <n> <port>`,
//! `member <name> <k> <home> <p1> <p2> <order>`, `ctl <scn> [runs]`.
//! Children print `EV <unix_ms> <who> <kind> <rest>` lines; `ctl` collects, kills, and measures.
use std::{
    collections::{BTreeMap, HashSet},
    fmt::Display,
    net::Ipv4Addr,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
#[cfg(feature = "a")]
use gossip_a as iroh_gossip;
#[cfg(feature = "b")]
use gossip_b as iroh_gossip;
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::{sleep, timeout},
};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(not(any(feature = "a", feature = "b")))]
compile_error!("enable exactly one of the features `a` and `b`");
#[cfg(all(feature = "a", feature = "b"))]
compile_error!("enable exactly one of the features `a` and `b`");

const VARIANT: &str = if cfg!(feature = "a") { "a" } else { "b" };

const ECHO: &[u8] = b"e8/echo/0";

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

fn ev(who: &str, kind: &str, rest: impl Display) {
    println!("EV {} {who} {kind} {rest}", now_ms());
}

fn cev(log: &Log, ts: u128, kind: &str, rest: impl Display) {
    if let Ok(mut g) = log.lock() {
        g.push(Rec { ts, who: "CTL".into(), kind: kind.into(), rest: rest.to_string() });
    }
}

fn short<E: Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(110).collect::<String>().replace(' ', "_")
}

fn key(n: u8) -> SecretKey {
    SecretKey::from_bytes(&[n; 32])
}

fn topic() -> TopicId {
    TopicId::from_bytes([7; 32])
}

fn id_name(id: &EndpointId) -> String {
    for (n, l) in [(1, "B1"), (2, "B2"), (11, "M1"), (12, "M2"), (13, "M3"), (14, "M4")] {
        if key(n).public() == *id {
            return l.into();
        }
    }
    id.fmt_short().to_string()
}

fn relay_url(port: u16) -> RelayUrl {
    format!("http://127.0.0.1:{port}").parse().expect("url")
}

fn urls_str<'a>(it: impl Iterator<Item = &'a RelayUrl>) -> String {
    it.map(|u| u.to_string().replace("http://127.0.0.1:", "").replace('/', "")).collect::<Vec<_>>().join(",")
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
    /// E21: signed infrastructure record (`InfraRec` bytes, host signature, operator certificate).
    Infra { rec: Vec<u8>, sig: Vec<u8>, cert: Vec<u8> },
}

// ---------------- E21: self-announcing infrastructure hosts ----------------

/// E21: the operator key (seed 99). Members know its public half; a host holds the operator's signature over its id (`cert`). Gossip names no origin (`delivered_from` is the forwarder), so the host id inside the record is the sender id.
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

fn sign_infra(signer: &SecretKey, rec: &InfraRec, cert: Vec<u8>) -> Option<Msg> {
    let bytes = postcard::to_stdvec(rec).ok()?;
    let sig = signer.sign(&rec_msg(&bytes)).to_bytes().to_vec();
    Some(Msg::Infra { rec: bytes, sig, cert })
}

fn make_infra(host: &SecretKey, relay: &RelayUrl, cert: Vec<u8>, ts: u64) -> Option<Msg> {
    let id = host.public();
    let rec = InfraRec { host: *id.as_bytes(), relay: relay.to_string(), boot: EndpointAddr::new(id).with_relay_url(relay.clone()), ts };
    sign_infra(host, &rec, cert)
}

fn sig_of(b: &[u8]) -> Option<Signature> {
    Some(Signature::from_bytes(<&[u8; 64]>::try_from(b).ok()?))
}

/// A record counts when the operator certified its host id and the host signed the record.
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

/// E21: hosts a member knows: bootstrap id plus its relay URL, in the order tried at (re)join.
type Known = Arc<Mutex<Vec<(EndpointId, RelayUrl)>>>;

/// Static hosts from the `order` argument: `1` = host A (key 1, relay p1), `2` = host C (key 2, relay p2). `12` and `21` are the E8 orders.
fn hosts_of(order: &str, p1: u16, p2: u16) -> Vec<(EndpointId, RelayUrl)> {
    order
        .chars()
        .filter_map(|c| match c {
            '1' => Some((key(1).public(), relay_url(p1))),
            '2' => Some((key(2).public(), relay_url(p2))),
            _ => None,
        })
        .collect()
}

fn known_file(name: &str) -> Option<String> {
    std::env::var("E21_DIR").ok().map(|d| format!("{d}/known-{name}.txt"))
}

/// The saved file: one host per line, `<endpoint id, 64 hex> <relay url>`.
fn save_known(name: &str, k: &[(EndpointId, RelayUrl)]) {
    if let Some(p) = known_file(name) {
        let _ = std::fs::write(p, k.iter().map(|(i, u)| format!("{i} {u}\n")).collect::<String>());
    }
}

fn load_known(name: &str) -> Vec<(EndpointId, RelayUrl)> {
    let Some(txt) = known_file(name).and_then(|p| std::fs::read_to_string(p).ok()) else { return vec![] };
    txt.lines().filter_map(|l| l.split_once(' ')).filter_map(|(i, u)| Some((i.parse().ok()?, u.parse().ok()?))).collect()
}

fn quic() -> QuicTransportConfig {
    QuicTransportConfig::builder().max_idle_timeout(Some(Duration::from_secs(10).try_into().expect("idle"))).build()
}

async fn spawn_relay(port: u16) -> Res<Server> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, port)));
    Ok(Server::spawn(cfg).await?)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr").port()
}

// ---------------- relay process: relay Rn plus bootstrap node Bn ----------------

async fn start_relay(n: u8, port: u16) -> Res<Server> {
    let who = format!("R{n}");
    let mut srv = None;
    for _ in 0..100 {
        match spawn_relay(port).await {
            Ok(s) => {
                srv = Some(s);
                break;
            }
            Err(_) => sleep(Duration::from_millis(100)).await,
        }
    }
    let srv = srv.ok_or("relay bind failed")?;
    ev(&who, "RELAY_UP", port);
    Ok(srv)
}

async fn relay_proc(n: u8, port: u16, via: Option<u16>) -> Res<()> {
    let _srv = start_relay(n, port).await?;
    boot_node(n, port, via).await
}

/// Relay without a bootstrap node (control 7: B1 runs in its own process).
async fn relay_only(n: u8, port: u16) -> Res<()> {
    let _srv = start_relay(n, port).await?;
    sleep_forever().await
}

/// Bootstrap node Bn: stable key, relay map = relay Rn only, gossip on the shared topic.
/// E21 (`via` = port of host A's relay): the node joins the topic through host A (id key 1, relay URL from `via`), with only its own relay in its map. With `E21_HOST` set it broadcasts its signed infrastructure record when it has joined and every `E21_REC_PERIOD_MS` (5000).
async fn boot_node(n: u8, port: u16, via: Option<u16>) -> Res<()> {
    init_trace(&format!("B{n}"));
    let lk = MemoryLookup::new();
    let mut b = Endpoint::builder(Minimal).relay_mode(RelayMode::Custom(RelayMap::from(relay_url(port)))).clear_ip_transports().secret_key(key(n)).transport_config(quic());
    if let Some(pa) = via {
        lk.add_endpoint_info(EndpointAddr::new(key(1).public()).with_relay_url(relay_url(pa)));
        b = b.address_lookup(lk.clone());
    }
    let ep = b.bind().await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).spawn();
    let topic_h = if via.is_some() { gossip.subscribe_and_join(topic(), vec![key(1).public()]).await? } else { gossip.subscribe(topic(), vec![]).await? };
    let (tx, mut rx) = topic_h.split();
    let bw = format!("B{n}");
    if std::env::var("E21_HOST").is_ok() {
        let (sk, relay, cert) = (key(n), relay_url(port), make_cert(&ep.id()));
        let period = std::env::var("E21_REC_PERIOD_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(5000);
        let bw2 = bw.clone();
        tokio::spawn(async move {
            loop {
                if let Some(Msg::Infra { rec, sig, cert }) = make_infra(&sk, &relay, cert.clone(), now_ms() as u64) {
                    if let Ok(b) = postcard::to_stdvec(&Msg::Infra { rec, sig, cert }) {
                        let r = tx.broadcast(Bytes::from(b)).await;
                        ev(&bw2, "INFRA_SENT", if r.is_ok() { "ok" } else { "err" });
                    }
                }
                sleep(Duration::from_millis(period)).await;
            }
        });
    }
    ev(&bw, "READY", ep.id().fmt_short());
    while let Some(e) = rx.next().await {
        match e {
            Ok(Event::NeighborUp(i)) => ev(&bw, "NU", id_name(&i)),
            Ok(Event::NeighborDown(i)) => ev(&bw, "ND", id_name(&i)),
            _ => {}
        }
    }
    Ok(())
}

// ---------------- member process ----------------

async fn bind_member(k: u8, urls: &[RelayUrl], lk: &MemoryLookup, want: Option<&RelayUrl>) -> Res<(Endpoint, RelayMap)> {
    for _ in 0..40 {
        let map = RelayMap::from_iter(urls.to_vec());
        let ep = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Custom(map.clone()))
            .clear_ip_transports()
            .secret_key(key(10 + k))
            .transport_config(quic())
            .address_lookup(lk.clone())
            .alpns(vec![ECHO.to_vec()])
            .bind()
            .await?;
        let Some(want) = want else { return Ok((ep, map)) };
        let _ = timeout(Duration::from_secs(10), ep.online()).await;
        if ep.addr().relay_urls().cloned().collect::<Vec<_>>() == [want.clone()] {
            return Ok((ep, map));
        }
        ep.close().await;
    }
    Err("could not home".into())
}

/// E17 C2: a relay is up when a TCP connect to its host and port succeeds within 1 s.
async fn relay_up(u: &RelayUrl) -> bool {
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else { return true };
    timeout(Duration::from_secs(1), tokio::net::TcpStream::connect((host, port))).await.is_ok_and(|r| r.is_ok())
}

/// E17 C2: remove the relay URLs in `down` from the lookup entries of the other members (M1..M3).
fn scrub(lk: &MemoryLookup, down: &HashSet<RelayUrl>, k: u8) {
    for id in (1..=3u8).filter(|t| *t != k).map(|t| key(10 + t).public()) {
        let Some(info) = lk.get_endpoint_info(id) else { continue };
        let mut addr = info.into_endpoint_addr();
        addr.addrs.retain(|a| !matches!(a, TransportAddr::Relay(u) if down.contains(u)));
        lk.set_endpoint_info(addr);
    }
}

/// Diagnostic only (`E12_TRACE=<env-filter>`): member tracing goes to /tmp/e12-trace-<name>.log.
fn init_trace(name: &str) {
    let Ok(filter) = std::env::var("E12_TRACE") else { return };
    let Ok(f) = std::fs::File::create(format!("/tmp/e12-trace-{name}.log")) else { return };
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(Mutex::new(f)).with_ansi(false).try_init();
}

async fn member(name: String, k: u8, home: u8, p1: u16, p2: u16, order: String) -> Res<()> {
    init_trace(&name);
    let (u1, u2) = (relay_url(p1), relay_url(p2));
    let mut lk = MemoryLookup::new();
    let want = match home {
        1 => Some(&u1),
        2 => Some(&u2),
        _ => None,
    };
    // E21: the hosts this member knows come from `order` (and, with `L`, from the saved file); records learned later join them. The relay map of every (re)bind is built from them.
    let known: Known = Arc::new(Mutex::new(hosts_of(&order, p1, p2)));
    if order.contains('L') {
        let loaded = load_known(&name);
        ev(&name, "LOADED", format!("hosts={}", loaded.len()));
        if let Ok(mut g) = known.lock() {
            for h in loaded {
                if !g.contains(&h) {
                    g.push(h);
                }
            }
        }
    }
    let relays_of = |known: &Known, dead: &HashSet<RelayUrl>| -> Vec<RelayUrl> {
        let mut v: Vec<RelayUrl> = vec![];
        for (_, u) in known.lock().map(|g| g.clone()).unwrap_or_default() {
            if !v.contains(&u) && !dead.contains(&u) {
                v.push(u);
            }
        }
        v
    };
    let (mut ep, mut relay_map) = bind_member(k, &relays_of(&known, &HashSet::new()), &lk, want).await?;
    ev(&name, "BOUND", format!("home_want={home} relays=[{}]", urls_str(ep.addr().relay_urls())));
    // E15 W1 (`E15_W1`, `E15_W1_T_MS` default 3000): rebind when no heartbeat arrived for T ms. `session` returns only then.
    let w1 = std::env::var("E15_W1").is_ok().then(|| std::env::var("E15_W1_T_MS").ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(3000));
    let dead: Arc<Mutex<HashSet<RelayUrl>>> = Arc::default();
    let (mut last_rebind, mut n) = (None, 0u32);
    loop {
        let silent = session(name.clone(), k, known.clone(), relay_map.clone(), lk.clone(), ep.clone(), dead.clone(), w1, last_rebind).await?;
        n += 1;
        last_rebind = Some(Instant::now());
        ev(&name, "REBIND", format!("n={n} silent_ms={silent}"));
        let _ = timeout(Duration::from_secs(5), ep.close()).await;
        // E17 C1 (`E17_FRESH_LOOKUP`): the rebound endpoint starts with an empty address book (`session` re-adds B1 and B2), so no peer address learned before the silence, with its dead relay URL, reaches the new endpoint.
        if std::env::var("E17_FRESH_LOOKUP").is_ok() {
            lk = MemoryLookup::new();
        }
        // E17 C2 (`E17_PROBE`): probe each configured relay with a TCP connect (1 s); a relay that refuses or times out is dropped from the address book entries of the other members before the new endpoint binds.
        if std::env::var("E17_PROBE").is_ok() {
            let mut down: HashSet<RelayUrl> = HashSet::new();
            for u in &relays_of(&known, &HashSet::new()) {
                if !relay_up(u).await {
                    down.insert(u.clone());
                }
            }
            ev(&name, "PROBE_RELAYS", format!("down=[{}]", urls_str(down.iter())));
            scrub(&lk, &down, k);
        }
        let urls = relays_of(&known, &dead.lock().map(|g| g.clone()).unwrap_or_default());
        let t = Instant::now();
        (ep, relay_map) = bind_member(k, &urls, &lk, None).await?;
        ev(&name, "REBOUND", format!("n={n} ms={} relays=[{}]", t.elapsed().as_millis(), urls_str(ep.addr().relay_urls())));
    }
}

#[allow(clippy::too_many_arguments)]
async fn session(name: String, k: u8, known: Known, relay_map: RelayMap, lk: MemoryLookup, ep: Endpoint, dead: Arc<Mutex<HashSet<RelayUrl>>>, w1: Option<u128>, last_rebind: Option<Instant>) -> Res<u128> {
    let payload = std::env::var("E8_PAYLOAD").is_ok();
    let mut js = tokio::task::JoinSet::new();
    let t_bind = Instant::now();
    // Bootstrap addresses: id plus its own relay URL, for every host known now (E21: static order, saved file, learned records).
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
    // E12 levers. L1: remove a dead home relay from the relay map (`E12_L1`). L2: rejoin when no heartbeat arrived for T ms (`E12_L2`, `E12_T_MS`, default 3000).
    let l1 = std::env::var("E12_L1").is_ok();
    let l2 = std::env::var("E12_L2").is_ok();
    let t_ms: u128 = std::env::var("E12_T_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let l1_debounce: u64 = std::env::var("E12_L1_DEBOUNCE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let armed = Arc::new(AtomicBool::new(false));
    let last_hb = Arc::new(AtomicU64::new(0));
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    {
        let (n, e, tx, armed, dd) = (name.clone(), ep.clone(), status_tx.clone(), armed.clone(), dead.clone());
        js.spawn(async move {
            let mut s = e.home_relay_status().stream();
            let mut pending: HashSet<RelayUrl> = HashSet::new();
            while let Some(v) = s.next().await {
                let d: Vec<String> = v
                    .iter()
                    .map(|r| format!("relay={} connected={} err={}", urls_str(std::iter::once(r.url())), r.is_connected(), r.last_error().map_or("-".into(), short)))
                    .collect();
                ev(&n, "STATUS", d.join(";"));
                let _ = tx.send(());
                if l1 && armed.load(Ordering::Relaxed) {
                    for r in v.iter().filter(|r| !r.is_connected()) {
                        let url = r.url().clone();
                        if !pending.insert(url.clone()) {
                            continue;
                        }
                        let (n2, e2, dd2) = (n.clone(), e.clone(), dd.clone());
                        tokio::spawn(async move {
                            sleep(Duration::from_millis(l1_debounce)).await;
                            let dead = e2.home_relay_status().get().iter().any(|x| x.url() == &url && !x.is_connected());
                            if dead {
                                ev(&n2, "L1_FIRE", urls_str(std::iter::once(&url)));
                                if let Ok(mut g) = dd2.lock() {
                                    g.insert(url.clone());
                                }
                                let t = Instant::now();
                                let r = timeout(Duration::from_secs(5), e2.remove_relay(&url)).await;
                                ev(&n2, "L1_DONE", format!("removed={} ms={}", matches!(r, Ok(Some(_))), t.elapsed().as_millis()));
                            } else {
                                ev(&n2, "L1_SKIP", urls_str(std::iter::once(&url)));
                            }
                        });
                    }
                    pending.retain(|u| v.iter().any(|r| r.url() == u && !r.is_connected()));
                }
            }
        });
        let (n, e, tx) = (name.clone(), ep.clone(), status_tx);
        js.spawn(async move {
            let mut s = e.watch_addr().stream();
            while let Some(v) = s.next().await {
                ev(&n, "ADDR", format!("relays=[{}]", urls_str(v.relay_urls())));
                let _ = tx.send(());
            }
        });
    }
    ev(&name, "JOIN_START", format!("order={order}"));
    let t_join = Instant::now();
    // E18 counter (`E18_JOIN_T_MS`, default 60000): a join that sees no NeighborUp within T ms fails, and W1 then rebinds, which closes the connection to the bootstrap node.
    let join_t = std::env::var("E18_JOIN_T_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(60_000);
    let topic_h = timeout(Duration::from_millis(join_t), gossip.subscribe_and_join(topic(), boot.clone())).await;
    let Ok(Ok(th)) = topic_h else {
        ev(&name, "JOIN_FAIL", format!("after_ms={}", t_join.elapsed().as_millis()));
        if w1.is_some() {
            return Ok(0);
        }
        sleep_forever().await?;
        return Ok(0);
    };
    ev(&name, "JOINED", format!("ms={}", t_join.elapsed().as_millis()));
    armed.store(true, Ordering::Relaxed);
    last_hb.store(now_ms() as u64, Ordering::Relaxed);
    let (tx, mut rx) = th.split();
    let nbrs: Arc<Mutex<std::collections::HashSet<EndpointId>>> = Arc::default();
    {
        let (n, lk, nb, lh, kn, ep2, rm) = (name.clone(), lk.clone(), nbrs.clone(), last_hb.clone(), known.clone(), ep.clone(), relay_map.clone());
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
                    Ok(Event::Received(m)) => match postcard::from_bytes::<Msg>(&m.content) {
                        Ok(Msg::Hb { from, seq }) => {
                            lh.store(now_ms() as u64, Ordering::Relaxed);
                            ev(&n, "RX", format!("hb from={from} seq={seq} via={}", id_name(&m.delivered_from)));
                        }
                        Ok(Msg::Announce { from, addr, .. }) => {
                            ev(&n, "RX", format!("ann from={from} relays=[{}] via={}", urls_str(addr.relay_urls()), id_name(&m.delivered_from)));
                            if payload {
                                lk.set_endpoint_info(addr);
                            }
                        }
                        Ok(Msg::Infra { rec, sig, cert }) => match verify_infra(&rec, &sig, &cert, &operator().public()) {
                            Err(why) => ev(&n, "INFRA_REJECT", why.replace(' ', "_")),
                            Ok((r, host, relay)) => {
                                let (is_new, n_hosts, snap) = kn.lock().map_or((false, 0, vec![]), |mut g| {
                                    if g.iter().any(|(i, u)| *i == host && *u == relay) {
                                        return (false, g.len(), vec![]);
                                    }
                                    g.retain(|(i, _)| *i != host);
                                    g.push((host, relay.clone()));
                                    (true, g.len(), g.clone())
                                });
                                if is_new {
                                    lk.add_endpoint_info(r.boot.clone());
                                    let t = Instant::now();
                                    let prev = ep2.insert_relay(relay.clone(), Arc::new(MapRelayConfig::from(relay.clone()))).await;
                                    ev(&n, "HOSTS_LEARNED", format!("host={} relay={} insert_ms={} replaced={} in_map={} n_hosts={n_hosts}", id_name(&host), urls_str(std::iter::once(&relay)), t.elapsed().as_millis(), prev.is_some(), rm.contains(&relay)));
                                    save_known(&n, &snap);
                                }
                            }
                        },
                        Err(e) => ev(&n, "RX", format!("bad {}", short(e))),
                    },
                    Ok(other) => ev(&n, "EVT", short(format!("{other:?}"))),
                    Err(e) => ev(&n, "EVT", format!("err {}", short(e))),
                }
            }
        });
    }
    // S13: announce current EndpointAddr on any home-relay status or address change.
    {
        let (n, e, tx) = (name.clone(), ep.clone(), tx.clone());
        js.spawn(async move {
            let mut first = true;
            loop {
                if !first {
                    let period = std::env::var("E8_ANN_PERIOD").ok().and_then(|v| v.parse::<u64>().ok());
                    match period {
                        Some(p) => {
                            tokio::select! {
                                r = status_rx.recv() => if r.is_none() { break },
                                () = sleep(Duration::from_secs(p)) => {}
                            }
                        }
                        None => {
                            if status_rx.recv().await.is_none() {
                                break;
                            }
                        }
                    }
                }
                first = false;
                if let Ok(b) = postcard::to_stdvec(&Msg::Announce { from: n.clone(), addr: e.addr(), n: now_ms() as u64 }) {
                    let _ = tx.broadcast(Bytes::from(b)).await;
                }
                sleep(Duration::from_millis(300)).await;
            }
        });
    }
    // E20 counter (`E20_MJOIN_MS`): after a rebind, a member with no neighbor among the other members T ms after JOINED calls join_peers(other members), again every 3 s until one is a neighbor.
    if let (Some(t_ms), true) = (std::env::var("E20_MJOIN_MS").ok().and_then(|v| v.parse::<u64>().ok()), last_rebind.is_some()) {
        let (n, tx, nb) = (name.clone(), tx.clone(), nbrs.clone());
        let others: Vec<EndpointId> = (1..=3u8).filter(|t| *t != k).map(|t| key(10 + t).public()).collect();
        js.spawn(async move {
            sleep(Duration::from_millis(t_ms)).await;
            while !nb.lock().map_or(true, |g| others.iter().any(|o| g.contains(o))) {
                let r = timeout(Duration::from_secs(5), tx.join_peers(others.clone())).await;
                ev(&n, "MJOIN", format!("join_peers {}", if matches!(r, Ok(Ok(()))) { "ok" } else { "err/timeout" }));
                sleep(Duration::from_secs(3)).await;
            }
        });
    }
    // Optional (E8_REJOIN): when the neighbor set is empty, call join_peers(bootstrap) every 5 s.
    if std::env::var("E8_REJOIN").is_ok() {
        let (n, tx, nb) = (name.clone(), tx.clone(), nbrs.clone());
        let boot2 = boot.clone();
        js.spawn(async move {
            loop {
                sleep(Duration::from_secs(5)).await;
                let empty = nb.lock().map_or(false, |g| g.is_empty());
                if empty {
                    let r = timeout(Duration::from_secs(5), tx.join_peers(boot2.clone())).await;
                    ev(&n, "REJOIN", format!("join_peers {}", if matches!(r, Ok(Ok(()))) { "ok" } else { "err/timeout" }));
                }
            }
        });
    }
    // L2: no heartbeat from any peer for T ms -> join_peers(bootstrap + other members). First rejoin at T, then every 2, 4, 8, 16 s (cap 16) while still silent; any heartbeat resets.
    if l2 {
        let (n, tx, lh) = (name.clone(), tx.clone(), last_hb.clone());
        let mut peers = boot.clone();
        peers.extend((1..=3u8).filter(|t| *t != k).map(|t| key(10 + t).public()));
        js.spawn(async move {
            let (mut count, mut next_at) = (0u32, 0u128);
            loop {
                sleep(Duration::from_millis(250)).await;
                let now = now_ms();
                let silent = now.saturating_sub(u128::from(lh.load(Ordering::Relaxed)));
                if silent < t_ms {
                    (count, next_at) = (0, 0);
                    continue;
                }
                if now < next_at {
                    continue;
                }
                count += 1;
                next_at = now + u128::from((2000u64 << (count - 1).min(3)).min(16_000));
                let t = Instant::now();
                let r = timeout(Duration::from_secs(5), tx.join_peers(peers.clone())).await;
                ev(&n, "REJOIN", format!("n={count} silent_ms={silent} join_peers={} ms={}", if matches!(r, Ok(Ok(()))) { "ok" } else { "err/timeout" }, t.elapsed().as_millis()));
            }
        });
    }
    // Heartbeat, 1 s.
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
    // Probers: fresh id-only dial to each other of M1..M3 every ~1 s.
    for t in (1..=3u8).filter(|t| *t != k && std::env::var("E8_NOPROBE").is_err()) {
        let (n, e) = (name.clone(), ep.clone());
        js.spawn(async move {
            let id = key(10 + t).public();
            loop {
                let t0 = Instant::now();
                let r = timeout(Duration::from_secs(3), async {
                    let c = e.connect(EndpointAddr::from(id), ECHO).await.map_err(|x| short(x))?;
                    let (mut s, mut r) = c.open_bi().await.map_err(|x| short(x))?;
                    s.write_all(b"e8").await.map_err(|x| short(x))?;
                    s.finish().map_err(|x| short(x))?;
                    r.read_to_end(64).await.map_err(|x| short(x))?;
                    c.close(0u32.into(), b"done");
                    Ok::<(), String>(())
                })
                .await;
                let ms = t0.elapsed().as_millis();
                match r {
                    Ok(Ok(())) => ev(&n, "PROBE", format!("tgt=M{t} ok ms={ms}")),
                    Ok(Err(x)) => ev(&n, "PROBE", format!("tgt=M{t} fail ms={ms} err={x}")),
                    Err(_) => ev(&n, "PROBE", format!("tgt=M{t} fail ms={ms} err=timeout3s")),
                }
                sleep(Duration::from_secs(1)).await;
            }
        });
    }
    // W1: no heartbeat from any peer for T ms, at most one rebind per 20 s -> return to `member`, which rebinds.
    let Some(t_ms) = w1 else { sleep_forever().await?; return Ok(0) };
    loop {
        sleep(Duration::from_millis(250)).await;
        let silent = now_ms().saturating_sub(u128::from(last_hb.load(Ordering::Relaxed)));
        if silent >= t_ms && last_rebind.is_none_or(|r| r.elapsed() >= Duration::from_secs(20)) {
            return Ok(silent);
        }
    }
}

async fn sleep_forever() -> Res<()> {
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

// ---------------- controller ----------------

#[derive(Debug, Clone)]
struct Rec {
    ts: u128,
    who: String,
    kind: String,
    rest: String,
}

type Log = Arc<Mutex<Vec<Rec>>>;

fn spawn_child(log: &Log, args: &[String]) -> Res<Child> {
    // E17 diagnostic: `E17_ERR_DIR=<dir>` keeps each child's stderr (the vendored iroh's E14DIAG lines) in <dir>/err-<args 1 and 2>.log.
    let err = std::env::var("E17_ERR_DIR")
        .ok()
        .and_then(|d| std::fs::File::create(format!("{d}/err-{}-{}.log", args.get(1).map_or("", String::as_str), args.first().map_or("", String::as_str))).ok())
        .map_or_else(Stdio::null, Stdio::from);
    let mut c = Command::new(std::env::current_exe()?)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(err)
        .kill_on_drop(true)
        .spawn()?;
    let out = c.stdout.take().ok_or("no stdout")?;
    let log = log.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let mut p = l.splitn(5, ' ');
            if p.next() != Some("EV") {
                continue;
            }
            let (Some(ts), Some(who), Some(kind)) = (p.next(), p.next(), p.next()) else { continue };
            let rest = p.next().unwrap_or("").to_string();
            if let (Ok(ts), Ok(mut g)) = (ts.parse::<u128>(), log.lock()) {
                g.push(Rec { ts, who: who.into(), kind: kind.into(), rest });
            }
        }
    });
    Ok(c)
}

fn snap(log: &Log) -> Vec<Rec> {
    log.lock().map(|g| g.clone()).unwrap_or_default()
}

async fn wait_for(log: &Log, cap: Duration, f: impl Fn(&[Rec]) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < cap {
        if f(&snap(log)) {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    f(&snap(log))
}

fn has(l: &[Rec], who: &str, kind: &str, starts: &str, after: u128) -> bool {
    l.iter().any(|r| r.ts >= after && r.who == who && r.kind == kind && r.rest.starts_with(starts))
}

fn first_ts(l: &[Rec], after: u128, f: impl Fn(&Rec) -> bool) -> Option<u128> {
    l.iter().filter(|r| r.ts >= after && f(r)).map(|r| r.ts).min()
}

fn rel(t: Option<u128>, t0: u128) -> Option<f64> {
    t.map(|t| t as f64 - t0 as f64)
}

struct Stack {
    r1: Option<Child>,
    b1: Option<Child>,
    r2: Option<Child>,
    members: Vec<Child>,
    p1: u16,
    p2: u16,
}

fn relay_args(n: u8, port: u16) -> Vec<String> {
    vec!["relay".into(), n.to_string(), port.to_string()]
}

/// E21: a host process that joins the topic through host A's relay port `via`.
fn host_args(n: u8, port: u16, via: u16) -> Vec<String> {
    vec!["relay".into(), n.to_string(), port.to_string(), via.to_string()]
}

fn member_args(k: u8, home: u8, p1: u16, p2: u16, order: &str) -> Vec<String> {
    vec!["member".into(), format!("M{k}"), k.to_string(), home.to_string(), p1.to_string(), p2.to_string(), order.into()]
}

/// `split_b1`: R1 and B1 run as two processes (control 7); otherwise R1+B1 share one process as in E8.
async fn setup(log: &Log, homes: [u8; 3], split_b1: bool) -> Res<Stack> {
    let (p1, p2) = (free_port(), free_port());
    let (r1, b1) = if split_b1 {
        let r1 = spawn_child(log, &["relayonly".to_string(), "1".to_string(), p1.to_string()])?;
        if !wait_for(log, Duration::from_secs(20), |l| has(l, "R1", "RELAY_UP", "", 0)).await {
            return Err("r1 not up".into());
        }
        let b1 = spawn_child(log, &["boot".to_string(), "1".to_string(), p1.to_string()])?;
        (r1, Some(b1))
    } else {
        (spawn_child(log, &relay_args(1, p1))?, None)
    };
    let r2 = spawn_child(log, &relay_args(2, p2))?;
    if !wait_for(log, Duration::from_secs(20), |l| has(l, "B1", "READY", "", 0) && has(l, "B2", "READY", "", 0)).await {
        return Err("relays not ready".into());
    }
    let mut members = vec![];
    for (i, h) in homes.iter().enumerate() {
        members.push(spawn_child(log, &member_args(u8::try_from(i + 1)?, *h, p1, p2, "12"))?);
    }
    let ok = wait_for(log, Duration::from_secs(150), |l| (1..=3).all(|k| has(l, &format!("M{k}"), "JOINED", "", 0))).await;
    if !ok {
        return Err("members not joined".into());
    }
    let ok = wait_for(log, Duration::from_secs(40), |l| {
        (1..=3).all(|k| {
            let me = format!("M{k}");
            (1..=3).filter(|t| *t != k).all(|t| {
                (std::env::var("E8_NOPROBE").is_ok() || has(l, &me, "PROBE", &format!("tgt=M{t} ok"), 0)) && l.iter().any(|r| r.who == me && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{t} ")))
            })
        })
    })
    .await;
    if !ok {
        return Err("mesh not healthy".into());
    }
    sleep(Duration::from_secs(3)).await;
    Ok(Stack { r1: Some(r1), b1, r2: Some(r2), members, p1, p2 })
}

async fn kill(c: &mut Option<Child>) -> u128 {
    let t = now_ms();
    if let Some(ch) = c {
        let _ = ch.start_kill();
        let _ = ch.wait().await;
    }
    *c = None;
    t
}

/// (first failed probe rel to t0, first ok probe after that failure rel to t0)
fn reach(l: &[Rec], prober: &str, tgt: &str, t0: u128) -> (Option<f64>, Option<f64>) {
    let fail = first_ts(l, t0, |r| r.who == prober && r.kind == "PROBE" && r.rest.starts_with(&format!("tgt={tgt} fail")));
    let ok = fail.and_then(|f| first_ts(l, f, |r| r.who == prober && r.kind == "PROBE" && r.rest.starts_with(&format!("tgt={tgt} ok"))));
    (rel(fail, t0), rel(ok, t0))
}

/// Largest gap between heartbeats from `tx` at `rx` within [t0-3 s, end]; returns (gap ms, resume rel t0). Resume None if gap runs to the end.
fn hb_gap(l: &[Rec], rxr: &str, txr: &str, t0: u128, end: u128) -> (f64, Option<f64>) {
    let pat = format!("hb from={txr} ");
    let mut ts: Vec<u128> = l.iter().filter(|r| r.who == rxr && r.kind == "RX" && r.rest.starts_with(&pat) && r.ts + 3000 >= t0 && r.ts <= end).map(|r| r.ts).collect();
    ts.sort_unstable();
    let mut best = (0.0f64, None);
    for w in ts.windows(2) {
        let g = (w[1] - w[0]) as f64;
        if g > best.0 {
            best = (g, Some(w[1] as f64 - t0 as f64));
        }
    }
    if let Some(last) = ts.last() {
        let g = (end - last) as f64;
        if g > best.0 {
            best = (g, None);
        }
    }
    best
}

fn first_hb_after(l: &[Rec], rxr: &str, txr: &str, after: u128) -> Option<u128> {
    let pat = format!("hb from={txr} ");
    first_ts(l, after, |r| r.who == rxr && r.kind == "RX" && r.rest.starts_with(&pat))
}

type Meas = BTreeMap<String, Option<f64>>;

fn put(m: &mut Meas, k: impl Into<String>, v: Option<f64>) {
    m.insert(k.into(), v);
}

fn nd_stats(l: &[Rec], t0: u128, m: &mut Meas) {
    for me in ["M1", "M2", "M3"] {
        let nds: Vec<&Rec> = l.iter().filter(|r| r.ts >= t0 && r.who == me && r.kind == "ND").collect();
        put(m, format!("{me} NeighborDown count"), Some(nds.len() as f64));
        put(m, format!("{me} first NeighborDown"), rel(nds.iter().map(|r| r.ts).min(), t0));
    }
}

/// E12: when L1 fired and when the re-home followed; how many rejoins L2 issued and when (after the kill, and before it).
fn l12_stats(l: &[Rec], t0: u128, rehome: Option<u128>, m: &mut Meas) {
    for me in ["M1", "M2", "M3"] {
        let fire = first_ts(l, t0, |r| r.who == me && r.kind == "L1_FIRE");
        put(m, format!("{me} L1 fired"), rel(fire, t0));
        put(m, format!("{me} L1 fire count"), Some(l.iter().filter(|r| r.who == me && r.kind == "L1_FIRE").count() as f64));
        if me == "M1" {
            put(m, "M1 re-home minus L1 fire", fire.zip(rehome).map(|(f, h)| h as f64 - f as f64));
        }
        let rj = |after: u128, before: u128| l.iter().filter(|r| r.who == me && r.kind == "REJOIN" && r.ts >= after && r.ts < before).count() as f64;
        put(m, format!("{me} REJOIN count after kill"), Some(rj(t0, u128::MAX)));
        put(m, format!("{me} REJOIN count before kill"), Some(rj(0, t0)));
        put(m, format!("{me} first REJOIN"), rel(first_ts(l, t0, |r| r.who == me && r.kind == "REJOIN"), t0));
    }
}

fn last_state(l: &[Rec], who: &str, kind: &str) -> String {
    l.iter().rev().find(|r| r.who == who && r.kind == kind).map_or("?".into(), |r| r.rest.clone())
}

async fn scenario_12(log: &Log, restart: bool, ctrl: &str) -> Res<(Meas, u128)> {
    let mut st = setup(log, [1, 2, 2], ctrl == "b1").await?;
    let (u1, u2) = (urls_str(std::iter::once(&relay_url(st.p1))), urls_str(std::iter::once(&relay_url(st.p2))));
    let t0 = match ctrl {
        "none" => now_ms(),
        "m1" => {
            let t = now_ms();
            let _ = st.members[0].start_kill();
            t
        }
        "b1" => kill(&mut st.b1).await,
        _ => kill(&mut st.r1).await,
    };
    cev(log, t0, "KILL", format!("control={ctrl:?} (R1+B1 unless control)"));
    let window = std::env::var("E10_WINDOW_MS").ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(88_000u128);
    let mut t_restart = None;
    if restart {
        sleep(Duration::from_secs(20)).await;
        st.r1 = Some(spawn_child(log, &relay_args(1, st.p1))?);
        cev(log, now_ms(), "RESTART", "R1+B1");
        wait_for(log, Duration::from_secs(10), |l| has(l, "R1", "RELAY_UP", "", t0)).await;
        t_restart = first_ts(&snap(log), t0, |r| r.who == "R1" && r.kind == "RELAY_UP");
    }
    let rem = (t0 + window).saturating_sub(now_ms());
    sleep(Duration::from_millis(u64::try_from(rem)?)).await;
    let l = snap(log);
    let end = t0 + window;
    let mut m = Meas::new();
    let rehome = first_ts(&l, t0, |r| r.who == "M1" && r.kind == "ADDR" && r.rest == format!("relays=[{u2}]"));
    put(&mut m, "M1 addr lists only R2", rel(rehome, t0));
    put(&mut m, "M1 status connected on R2", rel(first_ts(&l, t0, |r| r.who == "M1" && r.kind == "STATUS" && r.rest.contains(&format!("relay={u2} connected=true"))), t0));
    for p in ["M2", "M3"] {
        let (f, o) = reach(&l, p, "M1", t0);
        put(&mut m, format!("{p} first failed dial to M1"), f);
        put(&mut m, format!("{p} fresh dial to M1 ok again"), o);
        let (g, r) = hb_gap(&l, p, "M1", t0, end);
        put(&mut m, format!("{p} max HB gap from M1"), Some(g));
        put(&mut m, format!("{p} HB from M1 resumed"), r);
    }
    for p in ["M2", "M3"] {
        let (g, r) = hb_gap(&l, "M1", p, t0, end);
        put(&mut m, format!("M1 max HB gap from {p}"), Some(g));
        put(&mut m, format!("M1 HB from {p} resumed"), r);
    }
    for (a, b) in [("M2", "M3"), ("M3", "M2")] {
        let (g, r) = hb_gap(&l, a, b, t0, end);
        put(&mut m, format!("{a} max HB gap from {b} (both homed R2)"), Some(g));
        put(&mut m, format!("{a} HB from {b} resumed"), r);
        let (f, o) = reach(&l, a, b, t0);
        put(&mut m, format!("{a} first failed dial to {b}"), f);
        put(&mut m, format!("{a} fresh dial to {b} ok again"), o);
    }
    // E10 additions: first HB received after M1's re-home (None if M1 never re-homed or nothing arrived), and last HB in the window, per pair.
    for (a, b) in [("M2", "M1"), ("M3", "M1"), ("M1", "M2"), ("M1", "M3"), ("M2", "M3"), ("M3", "M2")] {
        put(&mut m, format!("{a} first HB from {b} after M1 re-home"), rehome.and_then(|rh| rel(first_hb_after(&l, a, b, rh), t0)));
        let pat = format!("hb from={b} ");
        put(&mut m, format!("{a} last HB from {b}"), rel(l.iter().filter(|r| r.who == a && r.kind == "RX" && r.rest.starts_with(&pat) && r.ts <= end).map(|r| r.ts).max(), t0));
    }
    nd_stats(&l, t0, &mut m);
    l12_stats(&l, t0, rehome, &mut m);
    if let Some(tr) = t_restart {
        put(&mut m, "restart at", rel(Some(tr), t0));
        put(&mut m, "M1 addr lists R1 again (after restart)", rel(first_ts(&l, tr, |r| r.who == "M1" && r.kind == "ADDR" && r.rest.contains(&u1)), tr));
        put(&mut m, "M1 status connected on R1 (after restart)", rel(first_ts(&l, tr, |r| r.who == "M1" && r.kind == "STATUS" && r.rest.contains(&format!("relay={u1} connected=true"))), tr));
    }
    let fin = format!("final M1 addr {} | M1 status {}", last_state(&l, "M1", "ADDR"), last_state(&l, "M1", "STATUS"));
    cev(log, now_ms(), "FINAL", fin);
    Ok((m, t0))
}

async fn scenario_3(log: &Log, order: &str) -> Res<(Meas, u128)> {
    let mut st = setup(log, [2, 2, 2], false).await?;
    let t0 = kill(&mut st.r1).await;
    cev(log, t0, "KILL", "R1+B1");
    sleep(Duration::from_secs(3)).await;
    st.members.push(spawn_child(log, &member_args(4, 0, st.p1, st.p2, order))?);
    wait_for(log, Duration::from_secs(30), |l| {
        has(l, "M4", "JOIN_FAIL", "", 0)
            || (1..=3).all(|k| {
                l.iter().any(|r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{k} ")))
                    && l.iter().any(|r| r.who == "M4" && r.kind == "PROBE" && r.rest.starts_with(&format!("tgt=M{k} ok")))
            })
    })
    .await;
    sleep(Duration::from_secs(1)).await;
    let l = snap(log);
    let js = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "JOIN_START").unwrap_or(t0);
    let mut m = Meas::new();
    put(&mut m, "M4 joined (ms from join start)", first_ts(&l, 0, |r| r.who == "M4" && r.kind == "JOINED").map(|t| (t - js) as f64));
    put(&mut m, "M4 online (ms from bind)", l.iter().find(|r| r.who == "M4" && r.kind == "ONLINE").and_then(|r| r.rest.split('=').nth(1)).and_then(|v| v.parse().ok()));
    let mut worst: Option<f64> = Some(0.0);
    for k in 1..=3 {
        let t = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{k} "))).map(|t| (t - js) as f64);
        put(&mut m, format!("M4 first HB from M{k}"), t);
        worst = match (worst, t) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        let a = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with(&format!("ann from=M{k} "))).map(|t| (t - js) as f64);
        put(&mut m, format!("M4 first announce from M{k}"), a);
    }
    put(&mut m, "M4 presence from all three", worst);
    let mut worst_d: Option<f64> = Some(0.0);
    for k in 1..=3 {
        let d = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "PROBE" && r.rest.starts_with(&format!("tgt=M{k} ok"))).map(|t| (t - js) as f64);
        put(&mut m, format!("M4 first fresh dial ok to M{k}"), d);
        worst_d = match (worst_d, d) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
    }
    put(&mut m, "M4 dialable by id: all three", worst_d);
    put(&mut m, "M4 first fresh dial ok to any member", first_ts(&l, 0, |r| r.who == "M4" && r.kind == "PROBE" && r.rest.contains(" ok ")).map(|t| (t - js) as f64));
    Ok((m, js))
}

async fn scenario_4(log: &Log) -> Res<(Meas, u128)> {
    let mut st = setup(log, [1, 2, 2], false).await?;
    let t0 = now_ms();
    cev(log, t0, "KILL", "R1+B1 and R2+B2");
    let _ = st.r1.as_mut().map(Child::start_kill);
    let _ = st.r2.as_mut().map(Child::start_kill);
    sleep(Duration::from_secs(20)).await;
    st.r1 = Some(spawn_child(log, &relay_args(1, st.p1))?);
    cev(log, now_ms(), "RESTART", "R1+B1 (R2+B2 stay dead)");
    wait_for(log, Duration::from_secs(10), |l| has(l, "R1", "RELAY_UP", "", t0)).await;
    let tr = first_ts(&snap(log), t0, |r| r.who == "R1" && r.kind == "RELAY_UP").unwrap_or(t0 + 20_000);
    let end = t0 + 85_000;
    sleep(Duration::from_millis(u64::try_from(end.saturating_sub(now_ms()))?)).await;
    let l = snap(log);
    let mut m = Meas::new();
    put(&mut m, "restart at", rel(Some(tr), t0));
    nd_stats(&l, t0, &mut m);
    let mut all_hb: Option<f64> = Some(0.0);
    let mut all_pr: Option<f64> = Some(0.0);
    for me in 1..=3u8 {
        let who = format!("M{me}");
        put(&mut m, format!("{who} first status disconnected"), rel(first_ts(&l, t0, |r| r.who == who && r.kind == "STATUS" && r.rest.contains("connected=false")), t0));
        put(&mut m, format!("{who} status connected (after restart)"), rel(first_ts(&l, tr, |r| r.who == who && r.kind == "STATUS" && r.rest.contains("connected=true")), tr));
        put(&mut m, format!("{who} first failed dial"), rel(first_ts(&l, t0, |r| r.who == who && r.kind == "PROBE" && r.rest.contains(" fail ")), t0));
        for t in (1..=3u8).filter(|t| *t != me) {
            let h = rel(first_hb_after(&l, &who, &format!("M{t}"), tr), tr);
            all_hb = match (all_hb, h) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
            let p = rel(first_ts(&l, tr, |r| r.who == who && r.kind == "PROBE" && r.rest.starts_with(&format!("tgt=M{t} ok"))), tr);
            all_pr = match (all_pr, p) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
        }
    }
    put(&mut m, "all 6 HB pairs flowing (after restart)", all_hb);
    put(&mut m, "all 6 fresh dials ok (after restart)", all_pr);
    for me in 1..=3 {
        let who = format!("M{me}");
        cev(log, now_ms(), "FINAL", format!("{who} addr {} | status {}", last_state(&l, &who, "ADDR"), last_state(&l, &who, "STATUS")));
    }
    Ok((m, t0))
}

/// E21: wait until the three members have JOINED and every pair has a heartbeat and a probe (as `setup`).
async fn await_mesh(log: &Log) -> Res<()> {
    if !wait_for(log, Duration::from_secs(150), |l| (1..=3).all(|k| has(l, &format!("M{k}"), "JOINED", "", 0))).await {
        return Err("members not joined".into());
    }
    let ok = wait_for(log, Duration::from_secs(40), |l| {
        (1..=3).all(|k| {
            let me = format!("M{k}");
            (1..=3).filter(|t| *t != k).all(|t| has(l, &me, "PROBE", &format!("tgt=M{t} ok"), 0) && l.iter().any(|r| r.who == me && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{t} "))))
        })
    })
    .await;
    if !ok {
        return Err("mesh not healthy".into());
    }
    sleep(Duration::from_secs(3)).await;
    Ok(())
}

/// E21 start-up shared by scenarios 8 and 9: host A (R1+B1) up, members M1..M3 homed on R1; with `stat` host C (R2+B2) is up before the members and is in their static config (order `12`), otherwise it is started later and the members know only A (order `1`).
async fn start_21(log: &Log, stat: bool) -> Res<Stack> {
    let (p1, p2) = (free_port(), free_port());
    let r1 = spawn_child(log, &relay_args(1, p1))?;
    if !wait_for(log, Duration::from_secs(20), |l| has(l, "R1", "RELAY_UP", "", 0) && has(l, "B1", "READY", "", 0)).await {
        return Err("host A not ready".into());
    }
    let mut r2 = None;
    if stat {
        r2 = Some(spawn_child(log, &host_args(2, p2, p1))?);
        if !wait_for(log, Duration::from_secs(20), |l| has(l, "B2", "READY", "", 0)).await {
            return Err("host C not ready".into());
        }
    }
    let mut members = vec![];
    let homes = std::env::var("E21_HOMES").unwrap_or_else(|_| "111".into());
    for k in 1..=3u8 {
        let home = homes.chars().nth(usize::from(k) - 1).and_then(|c| c.to_digit(10)).map_or(1, |d| d as u8);
        members.push(spawn_child(log, &member_args(k, home, p1, p2, if stat { "12" } else { "1" }))?);
    }
    await_mesh(log).await?;
    Ok(Stack { r1: Some(r1), b1: None, r2, members, p1, p2 })
}

/// E21 H1: start host C (arm only) and wait until all members learned it. Returns the start time.
async fn add_host_c(log: &Log, st: &mut Stack, m: &mut Meas) -> Res<u128> {
    let t_c = now_ms();
    cev(log, t_c, "C_START", "host C (R2+B2) starts and joins through B1");
    st.r2 = Some(spawn_child(log, &host_args(2, st.p2, st.p1))?);
    let u2 = urls_str(std::iter::once(&relay_url(st.p2)));
    let ok = wait_for(log, Duration::from_secs(60), |l| (1..=3).all(|k| has(l, &format!("M{k}"), "HOSTS_LEARNED", &format!("host=B2 relay={u2}"), t_c))).await;
    let l = snap(log);
    put(m, "C ready (B2 READY)", rel(first_ts(&l, t_c, |r| r.who == "B2" && r.kind == "READY"), t_c));
    put(m, "C first record sent", rel(first_ts(&l, t_c, |r| r.who == "B2" && r.kind == "INFRA_SENT"), t_c));
    let mut worst = Some(0.0f64);
    for k in 1..=3 {
        let me = format!("M{k}");
        let t = rel(first_ts(&l, t_c, |r| r.who == me && r.kind == "HOSTS_LEARNED" && r.rest.starts_with(&format!("host=B2 relay={u2}"))), t_c);
        put(m, format!("{me} RC in relay map and BC known"), t);
        worst = match (worst, t) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        let rest = l.iter().find(|r| r.who == me && r.kind == "HOSTS_LEARNED" && r.ts >= t_c).map(|r| r.rest.clone()).unwrap_or_default();
        cev(log, now_ms(), "H1", format!("{me} {rest}"));
        put(m, format!("{me} REBIND count since C start"), Some(l.iter().filter(|r| r.who == me && r.kind == "REBIND" && r.ts >= t_c).count() as f64));
        put(m, format!("{me} ADDR change to R2 since C start"), rel(first_ts(&l, t_c, |r| r.who == me && r.kind == "ADDR" && r.rest == format!("relays=[{u2}]")), t_c));
    }
    put(m, "all three members learned C", worst);
    if !ok {
        cev(log, now_ms(), "H1", "NOT all members learned C within 60 s");
    }
    Ok(t_c)
}

/// E21 scenarios 8 (H1, H2, H3) and the control (`E21_STATIC`): kill host A after C is known, measure heartbeats; arm only: a new member knowing only C joins after the window.
async fn scenario_21(log: &Log) -> Res<(Meas, u128)> {
    let stat = std::env::var("E21_STATIC").is_ok();
    let mut st = start_21(log, stat).await?;
    let p2 = st.p2;
    let mut m = Meas::new();
    if !stat {
        add_host_c(log, &mut st, &mut m).await?;
    }
    sleep(Duration::from_secs(10)).await;
    if std::env::var("E21_STOP_H1").is_ok() {
        return Ok((m, now_ms()));
    }
    let t0 = kill(&mut st.r1).await;
    cev(log, t0, "KILL", format!("host A (R1+B1), static_c={stat}"));
    let window = std::env::var("E10_WINDOW_MS").ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(88_000u128);
    sleep(Duration::from_millis(u64::try_from((t0 + window).saturating_sub(now_ms()))?)).await;
    let l = snap(log);
    let end = t0 + window;
    let u2 = urls_str(std::iter::once(&relay_url(p2)));
    for k in 1..=3 {
        let me = format!("M{k}");
        put(&mut m, format!("{me} addr lists only R2"), rel(first_ts(&l, t0, |r| r.who == me && r.kind == "ADDR" && r.rest == format!("relays=[{u2}]")), t0));
        put(&mut m, format!("{me} first REBIND"), rel(first_ts(&l, t0, |r| r.who == me && r.kind == "REBIND"), t0));
        put(&mut m, format!("{me} REBIND count"), Some(l.iter().filter(|r| r.who == me && r.kind == "REBIND" && r.ts >= t0).count() as f64));
        put(&mut m, format!("{me} first NU B2 after first REBOUND"), first_ts(&l, t0, |r| r.who == me && r.kind == "REBOUND").and_then(|rb| rel(first_ts(&l, rb, |r| r.who == me && r.kind == "NU" && r.rest == "B2"), t0)));
    }
    for (a, b) in [("M1", "M2"), ("M1", "M3"), ("M2", "M1"), ("M2", "M3"), ("M3", "M1"), ("M3", "M2")] {
        let (g, r) = hb_gap(&l, a, b, t0, end);
        put(&mut m, format!("{a} max HB gap from {b}"), Some(g));
        put(&mut m, format!("{a} HB from {b} resumed"), r);
    }
    if !stat {
        st.members.push(spawn_child(log, &member_args(4, 0, st.p1, st.p2, "2"))?);
        wait_for(log, Duration::from_secs(40), |l| {
            has(l, "M4", "JOIN_FAIL", "", 0) || (1..=3).all(|k| l.iter().any(|r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{k} "))))
        })
        .await;
        sleep(Duration::from_secs(1)).await;
        let l = snap(log);
        let js = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "JOIN_START").unwrap_or(t0);
        put(&mut m, "H3 M4 joined (ms from join start)", first_ts(&l, 0, |r| r.who == "M4" && r.kind == "JOINED").map(|t| (t - js) as f64));
        let mut worst = Some(0.0f64);
        let mut any: Option<f64> = None;
        for k in 1..=3 {
            let t = first_ts(&l, 0, |r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{k} "))).map(|t| (t - js) as f64);
            put(&mut m, format!("H3 M4 first HB from M{k}"), t);
            worst = match (worst, t) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
            any = match (any, t) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (None, b) => b,
                (a, None) => a,
            };
        }
        put(&mut m, "H3 M4 first HB from any member", any);
        put(&mut m, "H3 M4 HB from all three", worst);
        put(&mut m, "H3 M4 REBIND count", Some(l.iter().filter(|r| r.who == "M4" && r.kind == "REBIND").count() as f64));
        put(&mut m, "H3 M4 JOIN_FAIL count", Some(l.iter().filter(|r| r.who == "M4" && r.kind == "JOIN_FAIL").count() as f64));
    }
    Ok((m, t0))
}

/// E21 scenario 9 (saved hosts): as the arm up to C known, then kill host A, restart M3 with the saved file (`order 1L`), start a new member M4 that knows only A (`1`, no file); measure both.
async fn scenario_21p(log: &Log) -> Res<(Meas, u128)> {
    let mut st = start_21(log, false).await?;
    let mut m = Meas::new();
    add_host_c(log, &mut st, &mut m).await?;
    sleep(Duration::from_secs(5)).await;
    let t0 = kill(&mut st.r1).await;
    cev(log, t0, "KILL", "host A (R1+B1)");
    sleep(Duration::from_secs(3)).await;
    let _ = st.members[2].start_kill();
    let _ = st.members[2].wait().await;
    let t1 = now_ms();
    cev(log, t1, "M3_RESTART", "M3 killed and restarted (order 1L, saved file; with E21_NOFILE order 1)");
    st.members[2] = spawn_child(log, &member_args(3, 0, st.p1, st.p2, if std::env::var("E21_NOFILE").is_ok() { "1" } else { "1L" }))?;
    st.members.push(spawn_child(log, &member_args(4, 0, st.p1, st.p2, "1"))?);
    sleep(Duration::from_secs(45)).await;
    let l = snap(log);
    let after = |who: &str, kind: &str| first_ts(&l, t1, |r| r.who == who && r.kind == kind);
    put(&mut m, "M3 restart: LOADED hosts", l.iter().rev().find(|r| r.who == "M3" && r.kind == "LOADED").and_then(|r| r.rest.strip_prefix("hosts=")).and_then(|v| v.parse().ok()));
    put(&mut m, "M3 restart: JOINED after restart (s)", rel(after("M3", "JOINED"), t1).map(|v| v / 1000.0));
    for k in [1, 2] {
        put(&mut m, format!("M3 restart: first HB from M{k} (s after restart)"), rel(first_ts(&l, t1, |r| r.who == "M3" && r.kind == "RX" && r.rest.starts_with(&format!("hb from=M{k} "))), t1).map(|v| v / 1000.0));
    }
    put(&mut m, "M3 restart: JOIN_FAIL count", Some(l.iter().filter(|r| r.who == "M3" && r.kind == "JOIN_FAIL" && r.ts >= t1).count() as f64));
    put(&mut m, "M4 (no file): JOINED (s after restart)", rel(after("M4", "JOINED"), t1).map(|v| v / 1000.0));
    put(&mut m, "M4 (no file): JOIN_FAIL count", Some(l.iter().filter(|r| r.who == "M4" && r.kind == "JOIN_FAIL").count() as f64));
    put(&mut m, "M4 (no file): HB received count", Some(l.iter().filter(|r| r.who == "M4" && r.kind == "RX" && r.rest.starts_with("hb ")).count() as f64));
    Ok((m, t0))
}

/// Trace: state changes only, ms relative to `t0`.
fn trace(l: &[Rec], t0: u128, from: u128) -> String {
    let mut out = String::new();
    let mut last_probe: BTreeMap<(String, String), bool> = BTreeMap::new();
    let mut last_hb: BTreeMap<(String, String), u128> = BTreeMap::new();
    let mut seen_err: BTreeMap<(String, String), u32> = BTreeMap::new();
    for r in l.iter().filter(|r| r.ts >= from) {
        let t = r.ts as i128 - t0 as i128;
        let line = match r.kind.as_str() {
            "PROBE" => {
                let (tgt, ok) = (r.rest.split(' ').next().unwrap_or("").to_string(), r.rest.contains(" ok "));
                let prev = last_probe.insert((r.who.clone(), tgt.clone()), ok);
                if prev == Some(ok) { continue; }
                Some(format!("{} {} {}", r.who, if ok { "dial ok" } else { "dial FAIL" }, r.rest))
            }
            "RX" if r.rest.starts_with("hb ") => {
                let from = r.rest.split(' ').nth(1).unwrap_or("").to_string();
                let prev = last_hb.insert((r.who.clone(), from.clone()), r.ts);
                match prev {
                    Some(p) if r.ts - p > 2500 => Some(format!("{} HB {} resumed after {} ms gap", r.who, from, r.ts - p)),
                    _ => continue,
                }
            }
            "RX" => Some(format!("{} {}", r.who, r.rest)),
            "STATUS" => {
                let c = seen_err.entry((r.who.clone(), r.rest.clone())).or_insert(0);
                *c += 1;
                if *c > 1 { continue; }
                Some(format!("{} STATUS {}", r.who, r.rest))
            }
            "HOSTS_LEARNED" | "INFRA_REJECT" | "LOADED" | "ADDR" | "L1_FIRE" | "L1_DONE" | "L1_SKIP" | "REJOIN" | "NU" | "ND" | "JOIN_START" | "JOINED" | "JOIN_FAIL" | "ONLINE" | "BOUND" | "KILL" | "RESTART" | "RELAY_UP" | "FINAL" => Some(format!("{} {} {}", r.who, r.kind, r.rest)),
            _ => continue,
        };
        if let Some(s) = line {
            out.push_str(&format!("{t:>8} ms  {s}\n"));
        }
    }
    out
}

async fn ctl(scn: &str, runs: u32) {
    let _ = std::fs::create_dir_all("/tmp/e10");
    let run0: u32 = std::env::var("E10_RUN0").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let mut agg: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    let variants: Vec<&str> = if scn == "3" { vec!["12", "21"] } else { vec![""] };
    for var in variants {
        agg.clear();
        for run in run0..run0 + runs {
            let log: Log = Arc::new(Mutex::new(vec![]));
            let t_run = Instant::now();
            let res = match scn {
                "1" => scenario_12(&log, false, "").await,
                "2" => scenario_12(&log, true, "").await,
                "0" => scenario_12(&log, false, "none").await,
                "8" => scenario_21(&log).await,
                "9" => scenario_21p(&log).await,
                "6" => scenario_12(&log, false, "m1").await,
                "7" => scenario_12(&log, false, "b1").await,
                "3" => scenario_3(&log, var).await,
                "4" => scenario_4(&log).await,
                _ => Err("scn".into()),
            };
            let l = snap(&log);
            let tag = format!("{VARIANT}-{scn}{var}{}-{run}", std::env::var("E8_TAG").unwrap_or_default());
            let t0_for_trace = l.iter().find(|r| r.kind == "KILL").map_or_else(|| l.iter().find(|r| r.kind == "JOIN_START" && r.who == "M4").map_or(0, |r| r.ts), |r| r.ts);
            let from = if scn == "3" { t0_for_trace.saturating_sub(10_000) } else { t0_for_trace.saturating_sub(4_000) };
            let raw: String = l.iter().map(|r| format!("{} {} {} {}\n", r.ts, r.who, r.kind, r.rest)).collect();
            let _ = std::fs::write(format!("/tmp/e10/{tag}.raw.log"), raw);
            let _ = std::fs::write(format!("/tmp/e10/{tag}.trace.txt"), trace(&l, t0_for_trace, from));
            match res {
                Ok((m, _)) => {
                    println!("RUN {tag} ok elapsed_s={}", t_run.elapsed().as_secs());
                    for (k, v) in &m {
                        println!("  {k}: {}", v.map_or("NA".into(), |x| format!("{x:.0}")));
                        agg.entry(k.clone()).or_default().push(*v);
                    }
                }
                Err(e) => println!("RUN {tag} SETUP FAILED: {e}"),
            }
        }
        println!("SUMMARY gossip={VARIANT} scn={scn} variant={var} runs={runs} run0={run0}");
        for (k, v) in &agg {
            let mut ok: Vec<f64> = v.iter().flatten().copied().collect();
            ok.sort_by(f64::total_cmp);
            if ok.is_empty() {
                println!("  {k}: none ({} runs NA)", v.len());
            } else {
                println!("  {k}: min {:.0} median {:.0} max {:.0} (n_ok {}/{})", ok[0], ok[ok.len() / 2], ok[ok.len() - 1], ok.len(), v.len());
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let g = |i: usize| a.get(i).cloned().unwrap_or_default();
    let r = match g(1).as_str() {
        "relay" => relay_proc(g(2).parse().unwrap_or(1), g(3).parse().unwrap_or(0), g(4).parse().ok()).await,
        "relayonly" => relay_only(g(2).parse().unwrap_or(1), g(3).parse().unwrap_or(0)).await,
        "boot" => boot_node(g(2).parse().unwrap_or(1), g(3).parse().unwrap_or(0), None).await,
        "member" => member(g(2), g(3).parse().unwrap_or(1), g(4).parse().unwrap_or(0), g(5).parse().unwrap_or(0), g(6).parse().unwrap_or(0), g(7)).await,
        "ctl" => {
            ctl(&g(2), g(3).parse().unwrap_or(3)).await;
            Ok(())
        }
        _ => Err("usage".into()),
    };
    if let Err(e) = r {
        println!("EV {} {} FATAL {}", now_ms(), g(1), short(e));
    }
}

#[cfg(test)]
mod e21_tests {
    use super::*;

    fn parts(m: Msg) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        match m {
            Msg::Infra { rec, sig, cert } => (rec, sig, cert),
            _ => panic!("not an infra message"),
        }
    }

    fn rec_of(host: &SecretKey) -> InfraRec {
        let id = host.public();
        let relay = relay_url(4000);
        InfraRec { host: *id.as_bytes(), relay: relay.to_string(), boot: EndpointAddr::new(id).with_relay_url(relay), ts: 1 }
    }

    #[test]
    fn valid_record_is_accepted() {
        let host = key(2);
        let (rec, sig, cert) = parts(make_infra(&host, &relay_url(4000), make_cert(&host.public()), 1).expect("msg"));
        let (r, id, relay) = verify_infra(&rec, &sig, &cert, &operator().public()).expect("accepted");
        assert_eq!((id, relay, r.ts), (host.public(), relay_url(4000), 1));
    }

    #[test]
    fn record_signed_by_a_key_that_is_not_the_senders_id_is_rejected() {
        // Claims host B2 (real certificate), signed by member M1's key.
        let (host, other) = (key(2), key(11));
        let (rec, sig, cert) = parts(sign_infra(&other, &rec_of(&host), make_cert(&host.public())).expect("msg"));
        assert_eq!(verify_infra(&rec, &sig, &cert, &operator().public()).err(), Some("signature does not match host id"));
    }

    #[test]
    fn record_from_a_non_host_member_is_rejected() {
        let member = key(11);
        // (a) valid self-signature, no certificate; (b) certificate issued by the member itself; (c) B2's certificate reused for the member's id.
        let (rec, sig, _) = parts(sign_infra(&member, &rec_of(&member), vec![]).expect("msg"));
        assert_eq!(verify_infra(&rec, &sig, &[], &operator().public()).err(), Some("bad cert length"));
        let self_cert = member.sign(&cert_msg(member.public().as_bytes())).to_bytes().to_vec();
        let (rec, sig, cert) = parts(sign_infra(&member, &rec_of(&member), self_cert).expect("msg"));
        assert_eq!(verify_infra(&rec, &sig, &cert, &operator().public()).err(), Some("not a host: operator certificate invalid"));
        let (rec, sig, cert) = parts(sign_infra(&member, &rec_of(&member), make_cert(&key(2).public())).expect("msg"));
        assert_eq!(verify_infra(&rec, &sig, &cert, &operator().public()).err(), Some("not a host: operator certificate invalid"));
    }

    #[test]
    fn tampered_record_is_rejected() {
        let host = key(2);
        let (mut rec, sig, cert) = parts(make_infra(&host, &relay_url(4000), make_cert(&host.public()), 1).expect("msg"));
        let last = rec.len() - 1;
        rec[last] ^= 1;
        assert!(verify_infra(&rec, &sig, &cert, &operator().public()).is_err());
    }

    #[test]
    fn known_hosts_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("e21-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        // SAFETY: the only test touching E21_DIR.
        unsafe { std::env::set_var("E21_DIR", &dir) };
        let hosts = vec![(key(1).public(), relay_url(4001)), (key(2).public(), relay_url(4002))];
        save_known("T", &hosts);
        assert_eq!(load_known("T"), hosts);
    }
}
