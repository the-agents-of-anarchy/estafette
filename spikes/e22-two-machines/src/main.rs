//! E22 two machines. Modes (all take `<relay_url>` first):
//! `serve <relay_url> <seed> [relay-only]`: long-lived endpoint with key from seed, echo + gossip topic heartbeats until killed.
//! `dial <relay_url> <peer_seed> <n> <relay-only|direct> <hold_s>`: n cold dials (fresh endpoint and key each), echo, path watch.
//! `warm <relay_url> <peer_seed> <n> <relay-only|direct>`: one endpoint, n fresh connections.
//! `gossip <relay_url> <seed> <peer_seed> <secs>`: join topic via the peer, heartbeat 1/s, log gaps.
use std::{
    fmt::Display,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ECHO: &[u8] = b"e22/echo/0";

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

fn ev(who: &str, kind: &str, rest: impl Display) {
    println!("EV {} {who} {kind} {rest}", now_ms());
}

fn short<E: Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(120).collect::<String>().replace(' ', "_")
}

fn key(n: u8) -> SecretKey {
    SecretKey::from_bytes(&[n; 32])
}

fn topic() -> TopicId {
    TopicId::from_bytes([22; 32])
}

#[derive(Serialize, Deserialize)]
struct Hb {
    from: String,
    seq: u64,
}

#[derive(Debug)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        loop {
            let Ok((mut tx, mut rx)) = conn.accept_bi().await else { return Ok(()) };
            let data = rx.read_to_end(1 << 16).await.map_err(AcceptError::from_err)?;
            tx.write_all(&data).await.map_err(AcceptError::from_err)?;
            tx.finish().map_err(AcceptError::from_err)?;
        }
    }
}

async fn mk(relay: &RelayUrl, seed: Option<u8>, relay_only: bool, lk: &MemoryLookup) -> Res<Endpoint> {
    // `RelayMap::from(url)` carries QUIC address discovery on UDP 7842; E22_NOQAD drops it.
    let map = if std::env::var("E22_NOQAD").is_ok() {
        RelayMap::from(iroh::RelayConfig::new(relay.clone(), None))
    } else {
        RelayMap::from(relay.clone())
    };
    let mut b = Endpoint::builder(Minimal)
        .relay_mode(RelayMode::Custom(map))
        .address_lookup(lk.clone())
        .alpns(vec![ECHO.to_vec()]);
    if let Some(s) = seed {
        b = b.secret_key(key(s));
    }
    if relay_only {
        b = b.clear_ip_transports();
    }
    if std::env::var("E22_NOPORTMAP").is_ok() {
        b = b.portmapper_config(iroh::endpoint::PortmapperConfig::Disabled);
    }
    Ok(b.bind().await?)
}

fn path_kind(c: &Connection) -> String {
    c.paths()
        .iter()
        .map(|p| {
            let kind = if p.is_ip() { format!("ip({})", p.remote_addr()) } else if p.is_relay() { "relay".into() } else { "other".into() };
            format!("{kind}{}", if p.is_selected() { "*" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn selected_is_ip(c: &Connection) -> bool {
    c.paths().iter().any(|p| p.is_selected() && p.is_ip())
}

fn selected_rtt_ms(c: &Connection) -> String {
    c.paths().iter().find(|p| p.is_selected()).map_or("-".into(), |p| format!("{:.1}", p.rtt().as_secs_f64() * 1000.0))
}

async fn echo_once(c: &Connection) -> Result<(), String> {
    let (mut s, mut r) = c.open_bi().await.map_err(short)?;
    s.write_all(b"e22").await.map_err(short)?;
    s.finish().map_err(short)?;
    r.read_to_end(64).await.map_err(short)?;
    Ok(())
}

fn median(v: &mut [u128]) -> u128 {
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

async fn serve(relay: RelayUrl, seed: u8, relay_only: bool) -> Res<()> {
    let lk = MemoryLookup::new();
    let t0 = Instant::now();
    let ep = mk(&relay, Some(seed), relay_only, &lk).await?;
    ep.online().await;
    ev("srv", "ONLINE", format!("id={} ms={} relays={:?}", ep.id(), t0.elapsed().as_millis(), ep.addr().relay_urls().collect::<Vec<_>>()));
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip).accept(ECHO, Echo).spawn();
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn dial(relay: RelayUrl, peer: u8, n: u32, mode: &str, hold_s: u64) -> Res<()> {
    let relay_only = mode == "relay-only";
    let target = EndpointAddr::new(key(peer).public()).with_relay_url(relay.clone());
    let (mut onl, mut con, mut echo, mut upg, mut rtt) = (vec![], vec![], vec![], vec![], vec![]);
    let (mut ok, mut direct_ok) = (0u32, 0u32);
    for i in 0..n {
        let lk = MemoryLookup::new();
        let tb = Instant::now();
        let ep = mk(&relay, None, relay_only, &lk).await?;
        if timeout(Duration::from_secs(20), ep.online()).await.is_err() {
            ev("dial", "TRY", format!("i={i} FAIL online_timeout"));
            continue;
        }
        let online = tb.elapsed().as_millis();
        let t0 = Instant::now();
        let r = timeout(Duration::from_secs(20), ep.connect(target.clone(), ECHO)).await;
        let c = match r {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                ev("dial", "TRY", format!("i={i} FAIL connect err={}", short(e)));
                continue;
            }
            Err(_) => {
                ev("dial", "TRY", format!("i={i} FAIL connect timeout20s"));
                continue;
            }
        };
        let connect = t0.elapsed().as_millis();
        let at_connect = path_kind(&c);
        let e = echo_once(&c).await;
        let echo_ms = t0.elapsed().as_millis();
        if let Err(e) = e {
            ev("dial", "TRY", format!("i={i} FAIL echo err={e}"));
            continue;
        }
        ok += 1;
        let mut upgrade = None;
        let hold = Instant::now();
        while hold.elapsed() < Duration::from_secs(hold_s) {
            if selected_is_ip(&c) {
                upgrade = Some(t0.elapsed().as_millis());
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        if relay_only {
            sleep(Duration::from_millis(300)).await;
        }
        // 10 steady echo round trips on the selected path.
        let mut rts = vec![];
        for _ in 0..10 {
            let t = Instant::now();
            if echo_once(&c).await.is_ok() {
                rts.push(t.elapsed().as_micros());
            }
        }
        let steady = median(&mut rts) / 1000;
        let final_paths = path_kind(&c);
        let final_ip = selected_is_ip(&c);
        if final_ip {
            direct_ok += 1;
        }
        ev(
            "dial",
            "TRY",
            format!(
                "i={i} ok online_ms={online} connect_ms={connect} echo_ms={echo_ms} at_connect={at_connect} upgrade_ms={} final={final_paths} steady_echo_ms={steady} qrtt_ms={}",
                upgrade.map_or("none".into(), |u| u.to_string()),
                selected_rtt_ms(&c)
            ),
        );
        onl.push(online);
        con.push(connect);
        echo.push(echo_ms);
        rtt.push(steady);
        if let Some(u) = upgrade {
            upg.push(u);
        }
        c.close(0u32.into(), b"done");
        let _ = timeout(Duration::from_secs(3), ep.close()).await;
    }
    ev(
        "dial",
        "SUM",
        format!(
            "mode={mode} relay={relay} tries={n} ok={ok} final_direct={direct_ok} upgraded={} online_med={} connect_med={} echo_med={} upgrade_med={} steady_echo_med={}",
            upg.len(),
            median(&mut onl),
            median(&mut con),
            median(&mut echo),
            median(&mut upg),
            median(&mut rtt)
        ),
    );
    Ok(())
}

async fn warm(relay: RelayUrl, peer: u8, n: u32, mode: &str) -> Res<()> {
    let relay_only = mode == "relay-only";
    let target = EndpointAddr::new(key(peer).public()).with_relay_url(relay.clone());
    let lk = MemoryLookup::new();
    let ep = mk(&relay, None, relay_only, &lk).await?;
    ep.online().await;
    let mut con = vec![];
    let mut ok = 0u32;
    for i in 0..n {
        let t0 = Instant::now();
        match timeout(Duration::from_secs(20), ep.connect(target.clone(), ECHO)).await {
            Ok(Ok(c)) => {
                let r = echo_once(&c).await;
                let ms = t0.elapsed().as_millis();
                ev("warm", "TRY", format!("i={i} ok={} total_ms={ms} paths={}", r.is_ok(), path_kind(&c)));
                if r.is_ok() {
                    ok += 1;
                    con.push(ms);
                }
                c.close(0u32.into(), b"done");
            }
            Ok(Err(e)) => ev("warm", "TRY", format!("i={i} FAIL {}", short(e))),
            Err(_) => ev("warm", "TRY", format!("i={i} FAIL timeout20s")),
        }
        sleep(Duration::from_millis(500)).await;
    }
    ev("warm", "SUM", format!("mode={mode} tries={n} ok={ok} total_med={}", median(&mut con)));
    Ok(())
}

async fn gossip_run(relay: RelayUrl, seed: u8, peer: u8, secs: u64) -> Res<()> {
    let lk = MemoryLookup::new();
    let ep = mk(&relay, Some(seed), false, &lk).await?;
    ep.online().await;
    let peer_addr = EndpointAddr::new(key(peer).public()).with_relay_url(relay.clone());
    lk.add_endpoint_info(peer_addr.clone());
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).accept(ECHO, Echo).spawn();
    let name = format!("g{seed}");
    // Side echo connection held open to log the path type over time.
    let side = {
        let (ep2, pa, nm) = (ep.clone(), peer_addr.clone(), name.clone());
        tokio::spawn(async move {
            if peer == 0 {
                return;
            }
            let mut last = String::new();
            let t0 = Instant::now();
            let Ok(Ok(c)) = timeout(Duration::from_secs(30), ep2.connect(pa, ECHO)).await else {
                ev(&nm, "SIDE", "connect_fail");
                return;
            };
            loop {
                let k = path_kind(&c);
                if k != last {
                    ev(&nm, "SIDE", format!("t={}ms paths={k}", t0.elapsed().as_millis()));
                    last = k;
                }
                sleep(Duration::from_millis(200)).await;
            }
        })
    };
    let t_join = Instant::now();
    let boot = if peer == 0 { vec![] } else { vec![key(peer).public()] };
    let th = timeout(Duration::from_secs(60), gossip.subscribe_and_join(topic(), boot)).await;
    let Ok(Ok(th)) = th else {
        ev(&name, "JOIN_FAIL", format!("after_ms={}", t_join.elapsed().as_millis()));
        return Ok(());
    };
    ev(&name, "JOINED", format!("ms={}", t_join.elapsed().as_millis()));
    let (tx, mut rx) = th.split();
    let run = Instant::now();
    let nm = name.clone();
    let recv = tokio::spawn(async move {
        let mut last: Option<Instant> = None;
        let (mut n, mut max_gap, mut over2) = (0u64, 0u128, 0u32);
        let mut first_ms = None;
        let mut seqs = std::collections::BTreeSet::new();
        let deadline = sleep(Duration::from_secs(secs + 2));
        tokio::pin!(deadline);
        loop {
            let e = tokio::select! {
                () = &mut deadline => break,
                e = rx.next() => match e { Some(e) => e, None => break },
            };
            match e {
                Ok(Event::Received(m)) => {
                    if let Ok(h) = postcard::from_bytes::<Hb>(&m.content) {
                        let now = Instant::now();
                        first_ms.get_or_insert(run.elapsed().as_millis());
                        if let Some(l) = last {
                            let g = now.duration_since(l).as_millis();
                            max_gap = max_gap.max(g);
                            if g > 2000 {
                                over2 += 1;
                                ev(&nm, "GAP", format!("ms={g} at_run_ms={} seq={}", run.elapsed().as_millis(), h.seq));
                            }
                        }
                        last = Some(now);
                        n += 1;
                        seqs.insert(h.seq);
                    }
                }
                Ok(Event::NeighborUp(i)) => ev(&nm, "NU", i.fmt_short()),
                Ok(Event::NeighborDown(i)) => ev(&nm, "ND", i.fmt_short()),
                _ => {}
            }
        }
        ev(&nm, "RXSUM", format!("rx={n} distinct_seq={} max_gap_ms={max_gap} gaps_over_2s={over2} first_rx_at_run_ms={}", seqs.len(), first_ms.map_or("none".into(), |v: u128| v.to_string())));
    });
    let mut sent = 0u64;
    while run.elapsed() < Duration::from_secs(secs) {
        let b = postcard::to_stdvec(&Hb { from: name.clone(), seq: sent })?;
        let _ = tx.broadcast(Bytes::from(b)).await;
        sent += 1;
        sleep(Duration::from_secs(1)).await;
    }
    ev(&name, "SENT", format!("n={sent}"));
    let _ = recv.await;
    side.abort();
    Ok(())
}

#[tokio::main]
async fn main() -> Res<()> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).map_or("", String::as_str);
    let relay: RelayUrl = arg(2).parse()?;
    match arg(1) {
        "serve" => serve(relay, arg(3).parse()?, arg(4) == "relay-only").await,
        "dial" => dial(relay, arg(3).parse()?, arg(4).parse()?, arg(5), arg(6).parse()?).await,
        "warm" => warm(relay, arg(3).parse()?, arg(4).parse()?, arg(5)).await,
        "gossip" => gossip_run(relay, arg(3).parse()?, arg(4).parse()?, arg(5).parse()?).await,
        _ => Err("modes: serve dial warm gossip".into()),
    }
}
