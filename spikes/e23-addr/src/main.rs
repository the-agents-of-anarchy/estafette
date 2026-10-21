//! E23 part 2: does a member notice its own address change and re-announce, and how fast do peers see it.
//! One process: relays RA and RB, member M (0.0.0.0:0, both relays), observers O1 and O2 (loopback, no relay).
//! Observers learn M's address only from M's Presence (S20). Usage: e23-addr <scenario> <runs>
use std::{
    fmt::Debug,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr,
    address_lookup::memory::MemoryLookup,
    endpoint::presets::Minimal,
    protocol::Router,
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use n0_future::StreamExt;
use n0_watcher::Watcher;
use tokio::time::{sleep, timeout};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Log = Arc<Mutex<Vec<(u128, String)>>>;

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn ev(log: &Log, who: &str, event: &str, kv: impl Debug) {
    let ns = now_ns();
    let line = format!("{ns} {who} {event} {kv:?}");
    println!("{line}");
    if let Ok(mut l) = log.lock() {
        l.push((ns, line));
    }
}

async fn spawn_relay() -> Res<(Server, RelayUrl)> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let s = Server::spawn(cfg).await?;
    let url: RelayUrl = format!("http://{}", s.http_addr().ok_or("no http addr")?).parse()?;
    Ok((s, url))
}

fn describe(a: &EndpointAddr) -> String {
    let mut relays = vec![];
    let mut ips = vec![];
    for t in &a.addrs {
        match t {
            TransportAddr::Relay(u) => relays.push(u.to_string()),
            TransportAddr::Ip(s) => ips.push(s.to_string()),
            other => ips.push(format!("{other:?}")),
        }
    }
    ips.sort();
    format!("relay={relays:?} ip={ips:?}")
}

struct Obs {
    ep: Endpoint,
    _router: Router,
}

async fn observer(log: Log, name: &'static str, topic: TopicId, boot: EndpointAddr) -> Res<Obs> {
    let lookup = MemoryLookup::new();
    let ep = Endpoint::builder(Minimal)
        .address_lookup(lookup.clone())
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))?
        .bind()
        .await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).spawn();
    let id = boot.id;
    lookup.add_endpoint_info(boot);
    let t = gossip.subscribe(topic, vec![id]).await?;
    let (_tx, mut rx) = t.split();
    tokio::spawn(async move {
        while let Some(e) = rx.next().await {
            if let Ok(Event::Received(m)) = e {
                let s = String::from_utf8_lossy(&m.content).to_string();
                // payload: "<seq>|<trigger>|<addr text>"
                let mut p = s.splitn(3, '|');
                let (seq, trig, desc) = (p.next().unwrap_or(""), p.next().unwrap_or(""), p.next().unwrap_or(""));
                ev(&log, name, "PRESENCE_RX", format!("seq={seq} trig={trig} {desc}"));
            }
        }
    });
    Ok(Obs { ep, _router: router })
}

struct Member {
    ep: Endpoint,
    _router: Router,
}

/// Member: after the gossip layer reports a neighbor, announce (S15); then re-announce on every item of
/// watch_addr() and home_relay_status() after the first (S13). `watch` false: no re-announce, items only logged.
async fn member(log: Log, relays: Vec<RelayUrl>, key: SecretKey, topic: TopicId, boot: Vec<EndpointAddr>, watch: bool) -> Res<Member> {
    let lookup = MemoryLookup::new();
    let ep = Endpoint::builder(Minimal)
        .secret_key(key)
        .address_lookup(lookup.clone())
        .relay_mode(RelayMode::Custom(RelayMap::from_iter(relays)))
        .clear_ip_transports()
        .bind_addr(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))?
        .bind()
        .await?;
    let r = timeout(Duration::from_secs(15), ep.online()).await;
    ev(&log, "M", "ONLINE", r.is_ok());
    let gossip = Gossip::builder().spawn(ep.clone());
    let router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).spawn();
    let ids = boot.iter().map(|b| b.id).collect::<Vec<_>>();
    for b in boot {
        lookup.add_endpoint_info(b);
    }
    let t = gossip.subscribe(topic, ids).await?;
    let (tx, mut rx) = t.split();
    let seq = Arc::new(Mutex::new(0u64));
    let announce = {
        let ep = ep.clone();
        let log = log.clone();
        move |trig: &'static str| {
            let ep = ep.clone();
            let log = log.clone();
            let tx = tx.clone();
            let seq = seq.clone();
            async move {
                let n = seq.lock().map(|mut s| { *s += 1; *s }).unwrap_or(0);
                let d = describe(&ep.addr());
                ev(&log, "M", "ANNOUNCE_TX", format!("seq={n} trig={trig} {d}"));
                let _ = tx.broadcast(Bytes::from(format!("{n}|{trig}|{d}"))).await;
            }
        }
    };
    let log2 = log.clone();
    let ann = announce.clone();
    tokio::spawn(async move {
        let mut announced = false;
        while let Some(e) = rx.next().await {
            if let Ok(Event::NeighborUp(_)) = e {
                ev(&log2, "M", "NEIGHBOR_UP", "");
                if !announced {
                    announced = true;
                    ann("join").await;
                }
            }
        }
    });
    let mut addr_w = ep.watch_addr().stream();
    let mut relay_w = ep.home_relay_status().stream();
    let (l1, a1) = (log.clone(), announce.clone());
    tokio::spawn(async move {
        let mut first = true;
        while let Some(a) = addr_w.next().await {
            ev(&l1, "M", "WATCH_ADDR", describe(&a));
            if first {
                first = false;
                continue;
            }
            if watch {
                a1("watch_addr").await;
            }
        }
    });
    let (l2, a2) = (log.clone(), announce);
    tokio::spawn(async move {
        let mut first = true;
        while let Some(s) = relay_w.next().await {
            let d = s.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>();
            ev(&l2, "M", "RELAY_STATUS", d);
            if first {
                first = false;
                continue;
            }
            if watch {
                a2("relay_status").await;
            }
        }
    });
    Ok(Member { ep, _router: router })
}

fn first_after(log: &Log, who: &str, event: &str, after: u128, contains: Option<&str>) -> Option<u128> {
    let l = log.lock().ok()?;
    l.iter()
        .find(|(ns, line)| {
            let mut t = line.split(' ');
            let _ = t.next();
            *ns >= after && t.next() == Some(who) && t.next() == Some(event) && contains.is_none_or(|c| line.contains(c))
        })
        .map(|(ns, _)| *ns)
}

fn count_after(log: &Log, who: &str, event: &str, after: u128, before: u128) -> usize {
    log.lock()
        .map(|l| {
            l.iter()
                .filter(|(ns, line)| {
                    let mut t = line.split(' ');
                    let _ = t.next();
                    *ns >= after && *ns < before && t.next() == Some(who) && t.next() == Some(event)
                })
                .count()
        })
        .unwrap_or(0)
}

fn ms(a: u128, b: u128) -> f64 {
    b.saturating_sub(a) as f64 / 1e6
}

struct World {
    ra: Server,
    rb: Server,
    ua: RelayUrl,
    ub: RelayUrl,
    m: Member,
    o1: Obs,
    o2: Obs,
    key: SecretKey,
}

async fn setup(log: &Log, watch: bool) -> Res<World> {
    let (ra, ua) = spawn_relay().await?;
    let (rb, ub) = spawn_relay().await?;
    let topic = TopicId::from_bytes([23u8; 32]);
    let key = SecretKey::generate();
    let m = member(log.clone(), vec![ua.clone(), ub.clone()], key.clone(), topic, vec![], watch).await?;
    let boot = m.ep.addr();
    ev(log, "M", "BOOT_ADDR", describe(&boot));
    let o1 = observer(log.clone(), "O1", topic, boot.clone()).await?;
    let o2 = observer(log.clone(), "O2", topic, boot).await?;
    for _ in 0..200 {
        if first_after(log, "O1", "PRESENCE_RX", 0, None).is_some() && first_after(log, "O2", "PRESENCE_RX", 0, None).is_some() {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    Ok(World { ra, rb, ua, ub, m, o1, o2, key })
}

#[tokio::main]
async fn main() -> Res<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().collect();
    let scen = args.get(1).map_or("control", String::as_str).to_string();
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    for run in 0..runs {
        let log: Log = Arc::new(Mutex::new(vec![]));
        println!("== {scen} run {run}");
        match scen.as_str() {
            "control" => {
                let w = setup(&log, true).await?;
                sleep(Duration::from_secs(2)).await;
                let t0 = now_ns();
                sleep(Duration::from_secs(20)).await;
                let t1 = now_ns();
                println!(
                    "RESULT control run={run} watch_addr={} relay_status={} announce_tx={} presence_rx_O1={}",
                    count_after(&log, "M", "WATCH_ADDR", t0, t1),
                    count_after(&log, "M", "RELAY_STATUS", t0, t1),
                    count_after(&log, "M", "ANNOUNCE_TX", t0, t1),
                    count_after(&log, "O1", "PRESENCE_RX", t0, t1)
                );
                drop(w);
            }
            "netchange" => {
                let w = setup(&log, true).await?;
                sleep(Duration::from_secs(2)).await;
                for i in 0..3 {
                    let t0 = now_ns();
                    w.m.ep.network_change().await;
                    sleep(Duration::from_secs(5)).await;
                    let t1 = now_ns();
                    println!(
                        "RESULT netchange run={run} call={i} watch_addr={} relay_status={} announce_tx={}",
                        count_after(&log, "M", "WATCH_ADDR", t0, t1),
                        count_after(&log, "M", "RELAY_STATUS", t0, t1),
                        count_after(&log, "M", "ANNOUNCE_TX", t0, t1)
                    );
                }
            }
            "relayswitch" | "relayswitch-nowatch" => {
                let watch = scen == "relayswitch";
                let w = setup(&log, watch).await?;
                sleep(Duration::from_secs(2)).await;
                let home: Vec<RelayUrl> = w.m.ep.addr().relay_urls().cloned().collect();
                let kill_a = home.first() == Some(&w.ua);
                let (victim, other) = if kill_a { (w.ua.clone(), w.ub.clone()) } else { (w.ub.clone(), w.ua.clone()) };
                println!("home={home:?} victim={victim} other={other}");
                let t = now_ns();
                ev(&log, "ORCH", "KILL_RELAY", victim.to_string());
                let World { ra, rb, m, .. } = w;
                if kill_a {
                    let _ = ra.shutdown().await;
                } else {
                    let _ = rb.shutdown().await;
                }
                sleep(Duration::from_secs(std::env::var("E23_WINDOW").ok().and_then(|s| s.parse().ok()).unwrap_or(12))).await;
                let t1 = now_ns();
                let oth = other.to_string();
                let f = |who: &str, e: &str, c: Option<&str>| first_after(&log, who, e, t, c).map(|x| ms(t, x));
                println!(
                    "RESULT {scen} run={run} relay_status_ms={:?} watch_addr_any_ms={:?} watch_addr_new_relay_ms={:?} announce_tx_ms={:?} O1_sees_new_relay_ms={:?} O2_sees_new_relay_ms={:?} n_relay_status={} n_watch_addr={} n_announce={}",
                    f("M", "RELAY_STATUS", None),
                    f("M", "WATCH_ADDR", None),
                    f("M", "WATCH_ADDR", Some(&oth)),
                    f("M", "ANNOUNCE_TX", None),
                    f("O1", "PRESENCE_RX", Some(&oth)),
                    f("O2", "PRESENCE_RX", Some(&oth)),
                    count_after(&log, "M", "RELAY_STATUS", t, t1),
                    count_after(&log, "M", "WATCH_ADDR", t, t1),
                    count_after(&log, "M", "ANNOUNCE_TX", t, t1)
                );
                println!("FINAL M addr={}", describe(&m.ep.addr()));
            }
            "restart" => {
                let w = setup(&log, true).await?;
                sleep(Duration::from_secs(2)).await;
                let old = describe(&w.m.ep.addr());
                let topic = TopicId::from_bytes([23u8; 32]);
                let t = now_ns();
                ev(&log, "ORCH", "CLOSE_M", "");
                w.m.ep.close().await;
                let World { m, key, ua, ub, o1: ob1, o2: ob2, .. } = w;
                drop(m);
                let t_closed = now_ns();
                let boots = vec![ob1.ep.addr(), ob2.ep.addr()];
                let m2 = member(log.clone(), vec![ua, ub], key, topic, boots, true).await?;
                let t_up = now_ns();
                println!("old={old}\nnew={}", describe(&m2.ep.addr()));
                sleep(Duration::from_secs(8)).await;
                let o1 = first_after(&log, "O1", "PRESENCE_RX", t_up, None);
                let o2 = first_after(&log, "O2", "PRESENCE_RX", t_up, None);
                println!(
                    "RESULT restart run={run} close_ms={:.1} rebind_ms={:.1} O1_first_presence_after_rebind_ms={:?} O2={:?}",
                    ms(t, t_closed),
                    ms(t_closed, t_up),
                    o1.map(|x| ms(t_up, x)),
                    o2.map(|x| ms(t_up, x))
                );
            }
            _ => return Err("unknown scenario".into()),
        }
        sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}
