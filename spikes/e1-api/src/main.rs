//! E1 API spike. Each check prints `PASS|FAIL #n name :: observed`.
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use n0_future::StreamExt;
use tokio::{sync::mpsc, time::timeout};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ECHO: &[u8] = b"e1/echo/0";
const MDNS_SVC: &str = "e1spike";

fn rec(n: u32, name: &str, ok: bool, observed: impl std::fmt::Display) {
    println!("{} #{n} {name} :: {observed}", if ok { "PASS" } else { "FAIL" });
}

// ---------- echo protocol that reports remote_id ----------
#[derive(Debug, Clone)]
struct Echo {
    seen: mpsc::UnboundedSender<iroh::EndpointId>,
}

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let _ = self.seen.send(conn.remote_id());
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(AcceptError::from_err)?;
        let data = rx.read_to_end(1 << 20).await.map_err(AcceptError::from_err)?;
        tx.write_all(&data).await.map_err(AcceptError::from_err)?;
        tx.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }
}

async fn echo_roundtrip(conn: &Connection, msg: &[u8]) -> Res<Vec<u8>> {
    let (mut tx, mut rx) = conn.open_bi().await?;
    tx.write_all(msg).await?;
    tx.finish()?;
    Ok(rx.read_to_end(1 << 20).await?)
}

async fn ep(sk: Option<SecretKey>, mode: RelayMode, ip: bool, alpns: Vec<Vec<u8>>) -> Res<Endpoint> {
    let mut b = Endpoint::builder(Minimal).relay_mode(mode).alpns(alpns);
    if !ip {
        b = b.clear_ip_transports();
    }
    if let Some(sk) = sk {
        b = b.secret_key(sk);
    }
    Ok(b.bind().await?)
}

fn direct(e: &Endpoint) -> EndpointAddr {
    let mut a = EndpointAddr::new(e.id());
    for s in e.bound_sockets() {
        let ip = if s.ip().is_unspecified() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            s.ip()
        };
        if ip.is_ipv4() {
            a = a.with_ip_addr(SocketAddr::new(ip, s.port()));
        }
    }
    a
}

async fn spawn_relay(port: u16) -> Res<Server> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, port)));
    Ok(Server::spawn(cfg).await?)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn relay_mode(url: &RelayUrl) -> RelayMode {
    RelayMode::Custom(RelayMap::from(url.clone()))
}

fn short<E: std::fmt::Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(160).collect()
}

// ---------- check 1 ----------
async fn check1() {
    let r: Res<String> = async {
        let sk = SecretKey::generate();
        let e = ep(Some(sk.clone()), RelayMode::Disabled, true, vec![]).await?;
        let id_match = e.id() == sk.public();
        let msg = b"arbitrary bytes \x00\xff";
        let sig = e.secret_key().sign(msg);
        let ok = e.id().verify(msg, &sig).is_ok();
        let tampered = e.id().verify(b"other", &sig).is_err();
        let wrong = SecretKey::generate().public().verify(msg, &sig).is_err();
        e.close().await;
        let s = format!(
            "id==sk.public {id_match}; verify ok {ok}; tampered rejected {tampered}; wrong key rejected {wrong}; sig len {}",
            sig.to_bytes().len()
        );
        if id_match && ok && tampered && wrong { Ok(s) } else { Err(s.into()) }
    }
    .await;
    match r {
        Ok(s) => rec(1, "sign/verify", true, s),
        Err(e) => rec(1, "sign/verify", false, e),
    }
}

// ---------- check 7 + 2 + 3 + 4 ----------
async fn check_relay_group() {
    let server = match spawn_relay(0).await {
        Ok(s) => s,
        Err(e) => return rec(7, "relay spawn", false, short(e)),
    };
    let url = http_url(&server);
    rec(7, "relay Server::spawn tls None", true, format!("http_url={url} http_addr={:?} https={:?}", server.http_addr(), server.https_addr()));

    // 7b: endpoint uses it via custom RelayMode
    let r: Res<String> = async {
        let (tx, _rx) = mpsc::unbounded_channel();
        let e = ep(None, relay_mode(&url), true, vec![ECHO.to_vec()]).await?;
        let _r = Router::builder(e.clone()).accept(ECHO, Echo { seen: tx }).spawn();
        let t = timeout(Duration::from_secs(10), e.online()).await;
        let st = e.home_relay_status().get();
        Ok(format!("online within 10s: {}; status {:?}; addr {:?}", t.is_ok(), st, e.addr()))
    }
    .await;
    match r {
        Ok(s) => rec(7, "endpoint via custom RelayMode (http)", s.contains("online within 10s: true"), s),
        Err(e) => rec(7, "endpoint via custom RelayMode (http)", false, short(e)),
    }

    // 2a: relays disabled, direct IP
    let r: Res<String> = async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = ep(None, RelayMode::Disabled, true, vec![ECHO.to_vec()]).await?;
        let _rt = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
        let sk = SecretKey::generate();
        let a = ep(Some(sk.clone()), RelayMode::Disabled, true, vec![]).await?;
        let baddr = direct(&b);
        let conn = timeout(Duration::from_secs(10), a.connect(baddr.clone(), ECHO)).await??;
        let back = echo_roundtrip(&conn, b"hello-direct").await?;
        let seen = timeout(Duration::from_secs(5), rx.recv()).await?;
        let s = format!(
            "b addr {:?}; echo {:?}; remote_id==dialer {}",
            baddr,
            String::from_utf8_lossy(&back),
            seen == Some(sk.public())
        );
        // 4
        rec(4, "remote_id on accepted conn == dialer key", seen == Some(sk.public()), format!("accepted {:?} dialer {}", seen.map(|x| x.fmt_short().to_string()), sk.public().fmt_short()));
        Ok(s)
    }
    .await;
    match r {
        Ok(s) => rec(2, "relays disabled, IP-only exchange", true, s),
        Err(e) => rec(2, "relays disabled, IP-only exchange", false, short(e)),
    }

    // 2b: relay-only (clear_ip_transports) + 3: router with echo + gossip ALPN
    let r: Res<String> = async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = ep(None, relay_mode(&url), false, vec![]).await?;
        let gossip = Gossip::builder().spawn(b.clone());
        let _rt = Router::builder(b.clone())
            .accept(ECHO, Echo { seen: tx })
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        let sk = SecretKey::generate();
        let a = ep(Some(sk.clone()), relay_mode(&url), false, vec![]).await?;
        timeout(Duration::from_secs(10), b.online()).await?;
        let baddr = b.addr();
        let ips = baddr.ip_addrs().count();
        let conn = timeout(Duration::from_secs(15), a.connect(EndpointAddr::new(b.id()).with_relay_url(url.clone()), ECHO)).await??;
        let back = echo_roundtrip(&conn, b"hello-relay").await?;
        let seen = timeout(Duration::from_secs(5), rx.recv()).await?;
        // 3: second ALPN on same endpoint
        let g = timeout(Duration::from_secs(15), a.connect(EndpointAddr::new(b.id()).with_relay_url(url.clone()), GOSSIP_ALPN)).await;
        let g_alpn = g.as_ref().ok().and_then(|r| r.as_ref().ok()).map(|c| String::from_utf8_lossy(c.alpn()).to_string());
        let bogus = timeout(Duration::from_secs(15), a.connect(EndpointAddr::new(b.id()).with_relay_url(url.clone()), b"e1/nope")).await;
        let bogus_s = match &bogus { Ok(Ok(_)) => "connected(!)".to_string(), Ok(Err(e)) => format!("rejected: {}", short(e)), Err(_) => "timeout".into() };
        rec(3, "Router: custom ALPN + gossip ALPN on one endpoint", back == b"hello-relay" && g_alpn.is_some() && !matches!(bogus, Ok(Ok(_))),
            format!("echo ok {}; gossip ALPN connect -> {:?}; unknown ALPN -> {}", back == b"hello-relay", g_alpn, bogus_s));
        let paths = format!("{:?}", conn.paths());
        Ok(format!("b addr has {ips} IP addrs, relays {:?}; echo {:?}; remote_id match {}; paths {paths}", baddr.relay_urls().collect::<Vec<_>>(), String::from_utf8_lossy(&back), seen == Some(sk.public())))
    }
    .await;
    match r {
        Ok(s) => rec(2, "relay-only (clear_ip_transports) exchange", s.contains("has 0 IP"), s),
        Err(e) => rec(2, "relay-only (clear_ip_transports) exchange", false, short(e)),
    }
    let _ = server.shutdown().await;
}

// ---------- check 5 ----------
async fn check5() {
    // 5a: builder before bind
    let r: Res<String> = async {
        let a = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Disabled)
            .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC))
            .bind()
            .await?;
        let n = a.address_lookup()?.len();
        a.close().await;
        Ok(format!("compiles; lookup services after bind: {n}"))
    }
    .await;
    match r {
        Ok(s) => rec(5, "mdns builder before bind", true, s),
        Err(e) => rec(5, "mdns builder before bind", false, short(e)),
    }
    // 5b: after bind
    let r: Res<String> = async {
        let a = ep(None, RelayMode::Disabled, true, vec![]).await?;
        let before = a.address_lookup()?.len();
        let m = MdnsAddressLookup::builder().service_name(MDNS_SVC).build(a.id())?;
        a.address_lookup()?.add(m);
        let after = a.address_lookup()?.len();
        a.close().await;
        Ok(format!("compiles; services {before} -> {after}"))
    }
    .await;
    match r {
        Ok(s) => rec(5, "mdns added after bind (address_lookup().add)", true, s),
        Err(e) => rec(5, "mdns added after bind (address_lookup().add)", false, short(e)),
    }
    // 5c: discovery between two endpoints, id-only dial. One via builder, one via add-after-bind.
    let r: Res<String> = async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![ECHO.to_vec()])
            .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC))
            .bind()
            .await?;
        let _rt = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
        let sk = SecretKey::generate();
        let a = ep(Some(sk.clone()), RelayMode::Disabled, true, vec![]).await?;
        a.address_lookup()?.add(MdnsAddressLookup::builder().service_name(MDNS_SVC).build(a.id())?);
        let t0 = std::time::Instant::now();
        let conn = timeout(Duration::from_secs(20), a.connect(EndpointAddr::from(b.id()), ECHO)).await;
        match conn {
            Ok(Ok(c)) => {
                let back = echo_roundtrip(&c, b"hello-mdns").await?;
                let seen = rx.recv().await;
                Ok(format!("id-only dial connected in {:?}; echo {:?}; remote_id match {}", t0.elapsed(), String::from_utf8_lossy(&back), seen == Some(sk.public())))
            }
            Ok(Err(e)) => Err(format!("connect error after {:?}: {}", t0.elapsed(), short(e)).into()),
            Err(_) => Err("connect timeout 20s".into()),
        }
    }
    .await;
    match r {
        Ok(s) => rec(5, "mdns discovery, id-only dial", true, s),
        Err(e) => rec(5, "mdns discovery, id-only dial", false, short(e)),
    }
}

// ---------- check 6 ----------
async fn check6() {
    let port = free_port();
    let server = match spawn_relay(port).await { Ok(s) => s, Err(e) => return rec(6, "setup", false, short(e)) };
    let url = http_url(&server);
    let e = match ep(None, relay_mode(&url), true, vec![]).await { Ok(e) => e, Err(x) => return rec(6, "setup", false, short(x)) };
    let _ = timeout(Duration::from_secs(10), e.online()).await;
    let t0 = std::time::Instant::now();
    let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    {
        let l = log.clone();
        let mut s = e.home_relay_status().stream();
        tokio::spawn(async move {
            while let Some(v) = s.next().await {
                let d: Vec<String> = v.iter().map(|r| format!("{{connected:{} err:{:?}}}", r.is_connected(), r.last_error().map(|x| short(x)))).collect();
                l.lock().unwrap().push(format!("[{:>6.1?}] home_relay_status {}", t0.elapsed(), d.join(",")));
            }
        });
        let l = log.clone();
        let mut s = e.watch_addr().stream();
        tokio::spawn(async move {
            while let Some(v) = s.next().await {
                l.lock().unwrap().push(format!("[{:>6.1?}] watch_addr relays={:?} ips={}", t0.elapsed(), v.relay_urls().collect::<Vec<_>>(), v.ip_addrs().count()));
            }
        });
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    log.lock().unwrap().push(format!("[{:>6.1?}] --- stopping relay", t0.elapsed()));
    let _ = server.shutdown().await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    log.lock().unwrap().push(format!("[{:>6.1?}] --- restarting relay on same port", t0.elapsed()));
    let server2 = spawn_relay(port).await;
    tokio::time::sleep(Duration::from_secs(20)).await;
    log.lock().unwrap().push(format!("[{:>6.1?}] --- end (restart ok: {})", t0.elapsed(), server2.is_ok()));
    let lines = log.lock().unwrap().clone();
    let final_connected = e.home_relay_status().get().iter().any(|r| r.is_connected());
    rec(6, "home_relay_status/watch_addr over relay stop/restart", lines.len() > 2, format!("{} events, final connected={final_connected}\n    {}", lines.len(), lines.join("\n    ")));
    if let Ok(s) = server2 { let _ = s.shutdown().await; }
}

// ---------- check 8 ----------
async fn gossip_case(max: Option<usize>, payload: usize) -> Res<String> {
    let mk = |lookup: MemoryLookup| async move {
        let e = Endpoint::builder(Minimal).relay_mode(RelayMode::Disabled).address_lookup(lookup).bind().await?;
        let mut gb = Gossip::builder();
        if let Some(m) = max { gb = gb.max_message_size(m); }
        let g = gb.spawn(e.clone());
        let r = Router::builder(e.clone()).accept(GOSSIP_ALPN, g.clone()).spawn();
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((e, g, r))
    };
    let (la, lb) = (MemoryLookup::new(), MemoryLookup::new());
    let (ea, ga, _ra) = mk(la.clone()).await?;
    let (eb, gb, _rb) = mk(lb.clone()).await?;
    la.add_endpoint_info(direct(&eb));
    lb.add_endpoint_info(direct(&ea));
    let topic = TopicId::from_bytes([7u8; 32]);
    let mut ta = ga.subscribe(topic, vec![eb.id()]).await?;
    let mut tb = timeout(Duration::from_secs(15), gb.subscribe_and_join(topic, vec![ea.id()])).await??;
    timeout(Duration::from_secs(15), ta.joined()).await??;
    let payload_bytes: Vec<u8> = (0..payload).map(|i| (i % 251) as u8).collect();
    let cfg = format!("max_message_size(a)={}", ga.max_message_size());
    let sent = ta.broadcast(Bytes::from(payload_bytes.clone())).await;
    if let Err(e) = sent { return Ok(format!("{cfg}; broadcast {payload}B -> Err({})", short(e))); }
    let got = timeout(Duration::from_secs(10), async {
        while let Some(ev) = tb.next().await {
            if let Ok(Event::Received(m)) = ev { return Some(m.content); }
        }
        None
    }).await;
    Ok(match got {
        Ok(Some(c)) => format!("{cfg}; broadcast {payload}B -> received {}B identical={}", c.len(), c.as_ref() == payload_bytes.as_slice()),
        Ok(None) => format!("{cfg}; broadcast {payload}B -> stream ended"),
        Err(_) => format!("{cfg}; broadcast {payload}B -> NOT received in 10s"),
    })
}

async fn check8() {
    for (max, n) in [(Some(8192usize), 5000usize), (None, 5000), (Some(8192), 9000)] {
        match gossip_case(max, n).await {
            Ok(s) => {
                let want = max == Some(8192) && n == 5000;
                let ok = if want { s.contains("identical=true") } else { true };
                rec(8, &format!("gossip max={max:?} payload={n}"), ok, s);
            }
            Err(e) => rec(8, &format!("gossip max={max:?} payload={n}"), false, short(e)),
        }
    }
}

// ---------- check 9 ----------
async fn dial_id_only(tag: &str, a: &Endpoint, b: &Endpoint) {
    let t0 = std::time::Instant::now();
    let r = timeout(Duration::from_secs(20), a.connect(EndpointAddr::from(b.id()), ECHO)).await;
    let (ok, s) = match r {
        Ok(Ok(c)) => (true, format!("connected in {:?}; paths {:?}", t0.elapsed(), c.paths())),
        Ok(Err(e)) => (false, format!("error after {:?}: {}", t0.elapsed(), short(e))),
        Err(_) => (false, "timeout 20s".into()),
    };
    rec(9, &format!("id-only dial, {tag}"), ok, s);
}

async fn check9() {
    // relays disabled, no lookup
    let (tx, _r) = mpsc::unbounded_channel();
    let b = ep(None, RelayMode::Disabled, true, vec![ECHO.to_vec()]).await.unwrap();
    let _rt = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
    let a = ep(None, RelayMode::Disabled, true, vec![]).await.unwrap();
    dial_id_only("relays disabled, no lookup", &a, &b).await;

    // shared relay, no lookup
    let server = spawn_relay(0).await.unwrap();
    let url = http_url(&server);
    let (tx, _r2) = mpsc::unbounded_channel();
    let b = ep(None, relay_mode(&url), true, vec![ECHO.to_vec()]).await.unwrap();
    let _rt2 = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
    let a = ep(None, relay_mode(&url), true, vec![]).await.unwrap();
    let _ = timeout(Duration::from_secs(10), b.online()).await;
    let _ = timeout(Duration::from_secs(10), a.online()).await;
    dial_id_only("shared relay, no lookup", &a, &b).await;

    // shared relay + mdns
    let (tx, _r3) = mpsc::unbounded_channel();
    let b = Endpoint::builder(Minimal).relay_mode(relay_mode(&url)).alpns(vec![ECHO.to_vec()])
        .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC)).bind().await.unwrap();
    let _rt3 = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
    let a = Endpoint::builder(Minimal).relay_mode(relay_mode(&url))
        .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC)).bind().await.unwrap();
    dial_id_only("shared relay + mdns", &a, &b).await;

    // relays disabled + mdns
    let (tx, _r4) = mpsc::unbounded_channel();
    let b = Endpoint::builder(Minimal).relay_mode(RelayMode::Disabled).alpns(vec![ECHO.to_vec()])
        .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC)).bind().await.unwrap();
    let _rt4 = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
    let a = Endpoint::builder(Minimal).relay_mode(RelayMode::Disabled)
        .address_lookup(MdnsAddressLookup::builder().service_name(MDNS_SVC)).bind().await.unwrap();
    dial_id_only("relays disabled + mdns", &a, &b).await;
    let _ = server.shutdown().await;
}


async fn check9ef() {
    let server = spawn_relay(0).await.unwrap();
    let url = http_url(&server);
    for (tag, ip) in [("9e relay URL + id, IP transports on", true), ("9f relay URL + id, clear_ip_transports", false)] {
        let (tx, _r) = mpsc::unbounded_channel();
        let b = ep(None, relay_mode(&url), ip, vec![ECHO.to_vec()]).await.unwrap();
        let _rt = Router::builder(b.clone()).accept(ECHO, Echo { seen: tx }).spawn();
        let a = ep(None, relay_mode(&url), ip, vec![]).await.unwrap();
        let _ = timeout(Duration::from_secs(10), b.online()).await;
        let _ = timeout(Duration::from_secs(10), a.online()).await;
        let addr = EndpointAddr::new(b.id()).with_relay_url(url.clone());
        let t0 = std::time::Instant::now();
        let r = timeout(Duration::from_secs(20), a.connect(addr, ECHO)).await;
        match r {
            Ok(Ok(c)) => {
                let t = t0.elapsed();
                let back = echo_roundtrip(&c, b"9ef").await.ok();
                tokio::time::sleep(Duration::from_secs(3)).await;
                rec(9, tag, true, format!("connected in {t:?}; echo {:?}; paths at connect+3s {:?}", back.map(|b| String::from_utf8_lossy(&b).to_string()), c.paths()));
            }
            Ok(Err(e)) => rec(9, tag, false, format!("error after {:?}: {}", t0.elapsed(), short(e))),
            Err(_) => rec(9, tag, false, "timeout 20s"),
        }
    }
    let _ = server.shutdown().await;
}

#[tokio::main]
async fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let all = which == "all";
    if all || which == "1" { check1().await; }
    if all || which == "relay" { check_relay_group().await; }
    if all || which == "5" { check5().await; }
    if all || which == "8" { check8().await; }
    if all || which == "9" { check9().await; }
    if all || which == "9ef" { check9ef().await; }
    if all || which == "6" { check6().await; }
}

/// `Server::http_url` is gated behind iroh-relay feature `test-utils`; build the URL by hand.
fn http_url(s: &Server) -> RelayUrl {
    format!("http://{}", s.http_addr().unwrap()).parse().unwrap()
}
