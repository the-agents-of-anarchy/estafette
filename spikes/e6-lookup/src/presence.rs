use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey, Watcher,
    address_lookup::memory::MemoryLookup,
    endpoint::presets::Minimal,
    protocol::Router,
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use n0_future::StreamExt;
use tokio::{sync::mpsc, time::timeout};

use crate::{Dial, Echo, ECHO, Res, dial_until, ms, ports, relay_mode, spawn_relay};

struct G {
    ep: Endpoint,
    router: Router,
    gossip: Gossip,
    lookup: MemoryLookup,
}

async fn g_node(sk: Option<SecretKey>, mode: RelayMode) -> Res<G> {
    let lookup = MemoryLookup::new();
    let mut b = Endpoint::builder(Minimal).relay_mode(mode).address_lookup(lookup.clone()).alpns(vec![ECHO.to_vec()]);
    if let Some(sk) = sk {
        b = b.secret_key(sk);
    }
    let ep = b.bind().await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let router = Router::builder(ep.clone()).accept(GOSSIP_ALPN, gossip.clone()).accept(ECHO, Echo).spawn();
    Ok(G { ep, router, gossip, lookup })
}

/// B joins via S, then broadcasts its current `EndpointAddr` now and on every address change.
async fn start_b(sk: SecretKey, mode: RelayMode, s: &EndpointAddr, topic: TopicId) -> Res<(G, Instant)> {
    let b = g_node(Some(sk), mode).await?;
    let t0 = Instant::now();
    b.lookup.add_endpoint_info(s.clone());
    let (gossip, ep, sid) = (b.gossip.clone(), b.ep.clone(), s.id);
    tokio::spawn(async move {
        let Ok(t) = gossip.subscribe_and_join(topic, vec![sid]).await else { return };
        let (tx, _rx) = t.split();
        let mut st = ep.watch_addr().stream();
        while let Some(addr) = st.next().await {
            if let Ok(bytes) = postcard::to_stdvec(&addr) {
                let _ = tx.broadcast(Bytes::from(bytes)).await;
            }
        }
    });
    Ok((b, t0))
}

/// Time from `t0` to the first presence message received at or after `t0`.
async fn payload_ms(rx: &mut mpsc::UnboundedReceiver<(Instant, EndpointAddr)>, t0: Instant) -> Option<f64> {
    let end = Instant::now() + Duration::from_secs(2);
    while let Ok(Some((at, _))) = timeout(end.saturating_duration_since(Instant::now()), rx.recv()).await {
        if at >= t0 {
            return Some(ms(at - t0));
        }
    }
    None
}

/// Three nodes: S (seed, address known to A and B out of band), A (dialer), B (restarts).
/// `payload`: A adds each received presence `EndpointAddr` to its `MemoryLookup`. Without it, only iroh-gossip's own
/// address lookup (attached to every endpoint that spawns `Gossip`) can supply B's address.
pub async fn run(run: u32, relay: bool, payload: bool) -> Res<()> {
    let srv = if relay { Some(spawn_relay(0).await?) } else { None };
    let mode = srv.as_ref().map_or(RelayMode::Disabled, |s| relay_mode(&format!("http://{}", s.http_addr().unwrap()).parse().unwrap()));
    let topic = TopicId::from_bytes(*SecretKey::generate().public().as_bytes());
    let s = g_node(None, mode.clone()).await?;
    if relay {
        let _ = timeout(Duration::from_secs(10), s.ep.online()).await;
    }
    let s_addr = s.ep.addr();
    let _s_topic = s.gossip.subscribe(topic, vec![]).await?;
    let a = g_node(None, mode.clone()).await?;
    a.lookup.add_endpoint_info(s_addr.clone());
    let (_a_tx, mut a_rx) = a.gossip.subscribe_and_join(topic, vec![s_addr.id]).await?.split();
    let (ptx, mut prx) = mpsc::unbounded_channel();
    let lk = a.lookup.clone();
    tokio::spawn(async move {
        while let Some(ev) = a_rx.next().await {
            if let Ok(Event::Received(m)) = ev {
                if let Ok(addr) = postcard::from_bytes::<EndpointAddr>(&m.content) {
                    if payload {
                        lk.add_endpoint_info(addr.clone());
                    }
                    let _ = ptx.send((Instant::now(), addr));
                }
            }
        }
    });
    let sk = SecretKey::generate();
    let id = sk.public();
    let cap = Duration::from_secs(30);
    let (b, t0) = start_b(sk.clone(), mode.clone(), &s_addr, topic).await?;
    let old = ports(&b.ep);
    let first: Dial = dial_until(&a.ep, id.into(), cap).await;
    let p1 = payload_ms(&mut prx, t0).await;
    b.router.shutdown().await?;
    let sk2 = sk.clone();
    let (b2, t1) = start_b(sk, mode.clone(), &s_addr, topic).await?;
    let new = ports(&b2.ep);
    let restart = dial_until(&a.ep, id.into(), cap).await;
    let p2 = payload_ms(&mut prx, t1).await;
    // Control: restart again and dial only after the presence has arrived and 1 s has passed.
    b2.router.shutdown().await?;
    let (b3, t2) = start_b(sk2, mode.clone(), &s_addr, topic).await?;
    let p3 = payload_ms(&mut prx, t2).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let settled = dial_until(&a.ep, id.into(), cap).await;
    let stale_ips = a.lookup.get_endpoint_info(id).map_or(0, |i| i.data.ip_addrs().count());
    let name = format!("presence-{}-{}", if relay { "relay" } else { "direct" }, if payload { "payload" } else { "builtin-only" });
    println!("ROW {name} run={run} old_port={old} new_port={new}\n  first: {first} payload_rx_ms={p1:?}\n  restart: {restart} payload_rx_ms={p2:?} a_lookup_ip_addrs={stale_ips}\n  restart-settled(1s after payload, payload_rx_ms={p3:?}): {settled}");
    b3.router.shutdown().await?;
    a.router.shutdown().await?;
    s.router.shutdown().await?;
    if let Some(srv) = srv {
        srv.shutdown().await?;
    }
    Ok(())
}
