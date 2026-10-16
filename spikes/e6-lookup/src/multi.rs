use std::time::Duration;

use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, address_lookup::memory::MemoryLookup,
    endpoint::presets::Minimal,
};
use tokio::time::timeout;

use crate::{ECHO, Res, dial_until, free_port, relay_url, serve, spawn_relay};

async fn bind_with(urls: Vec<RelayUrl>) -> Res<Endpoint> {
    let map = RelayMap::from_iter(urls);
    Ok(Endpoint::builder(Minimal).relay_mode(RelayMode::Custom(map)).clear_ip_transports().bind().await?)
}

/// Does relay-map order decide the home relay when both relays are up?
pub async fn probe() {
    let (p1, p2) = (free_port(), free_port());
    let (u1, u2) = (relay_url(p1), relay_url(p2));
    let _s1 = spawn_relay(p1).await.unwrap();
    let _s2 = spawn_relay(p2).await.unwrap();
    for i in 0..4 {
        for (tag, order) in [("[R1,R2]", vec![u1.clone(), u2.clone()]), ("[R2,R1]", vec![u2.clone(), u1.clone()])] {
            let e = bind_with(order).await.unwrap();
            let _ = timeout(Duration::from_secs(10), e.online()).await;
            let home: Vec<String> = e.addr().relay_urls().map(|u| u.to_string()).collect();
            println!("PROBE i={i} map={tag} home={home:?} (R1={u1} R2={u2})");
            e.close().await;
        }
    }
}

/// Bind until the endpoint's home relay is `want` (home choice among local relays is arbitrary; probe: map order does not decide).
async fn homed(want: &RelayUrl, urls: &[RelayUrl], lk: Option<&MemoryLookup>, alpn: bool) -> Res<Endpoint> {
    for _ in 0..30 {
        let mut b = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::from_iter(urls.to_vec())))
            .clear_ip_transports();
        if alpn {
            b = b.alpns(vec![ECHO.to_vec()]);
        }
        if let Some(lk) = lk {
            b = b.address_lookup(lk.clone());
        }
        let e = b.bind().await?;
        let _ = timeout(Duration::from_secs(10), e.online()).await;
        if e.addr().relay_urls().cloned().collect::<Vec<_>>() == [want.clone()] {
            return Ok(e);
        }
        e.close().await;
    }
    Err("could not home endpoint".into())
}

pub async fn run(run: u32) -> Res<()> {
    let (p1, p2) = (free_port(), free_port());
    let (u1, u2) = (relay_url(p1), relay_url(p2));
    let both = vec![u1.clone(), u2.clone()];
    let (s1, s2) = (spawn_relay(p1).await?, spawn_relay(p2).await?);
    let b = homed(&u2, &both, None, true).await?;
    let b_addr = b.addr();
    let r = serve(b.clone()).await;
    println!("ROW multi-relay run={run} B homed {:?} (R1={u1} R2={u2})", b_addr.relay_urls().collect::<Vec<_>>());
    let cap = Duration::from_secs(8);
    let id = b.id();
    for (tag, extra) in [("id+R1", 1u8), ("id+R2", 2), ("id+MemoryLookup(B full addr)", 3), ("id+R1+R2", 4), ("A map=[R1] only, id+R2", 5)] {
        let lk = MemoryLookup::new();
        let (urls, lkr) = match extra {
            3 => {
                lk.add_endpoint_info(b_addr.clone());
                (both.clone(), Some(&lk))
            }
            5 => (vec![u1.clone()], None),
            _ => (both.clone(), None),
        };
        let a = homed(&u1, &urls, lkr, false).await?;
        let b_home_now: Vec<_> = b.addr().relay_urls().cloned().collect();
        let target = match extra {
            1 => EndpointAddr::new(id).with_relay_url(u1.clone()),
            2 | 5 => EndpointAddr::new(id).with_relay_url(u2.clone()),
            3 => EndpointAddr::from(id),
            _ => EndpointAddr::new(id).with_relay_url(u1.clone()).with_relay_url(u2.clone()),
        };
        let d = dial_until(&a, target, cap).await;
        println!("  {tag}: {d} (A home {:?}, B home now {:?})", a.addr().relay_urls().cloned().collect::<Vec<_>>(), b_home_now);
        a.close().await;
    }
    r.shutdown().await?;
    s1.shutdown().await?;
    s2.shutdown().await?;
    Ok(())
}

/// Time from `bind()` returning until `online()` resolves and until the address lists a relay URL.
pub async fn online_probe() {
    let srv = spawn_relay(0).await.unwrap();
    let url = relay_url(srv.http_addr().unwrap().port());
    for i in 0..3 {
        let e = Endpoint::builder(Minimal).relay_mode(RelayMode::Custom(RelayMap::from(url.clone()))).bind().await.unwrap();
        let t0 = std::time::Instant::now();
        let has0 = e.addr().relay_urls().count();
        e.online().await;
        let t_online = t0.elapsed();
        println!("ONLINE i={i} custom-relay: relay_urls at bind={has0} online after {t_online:?} relay_urls now={}", e.addr().relay_urls().count());
        e.close().await;
    }
    for i in 0..3 {
        let e = Endpoint::builder(iroh::endpoint::presets::N0).bind().await.unwrap();
        let t0 = std::time::Instant::now();
        let has0 = e.addr().relay_urls().count();
        e.online().await;
        println!("ONLINE i={i} n0-preset: relay_urls at bind={has0} online after {:?}", t0.elapsed());
        e.close().await;
    }
}

async fn http_code(url: &str) -> String {
    // "200" plus which address kinds the served record carries: relay= and/or addr= TXT strings.
    let out = tokio::process::Command::new("curl").args(["-s", "-m", "5", "-w", "\n%{http_code}", url]).output().await;
    out.map(|o| {
        let body = String::from_utf8_lossy(&o.stdout).to_string();
        let code = body.rsplit('\n').next().unwrap_or("").to_string();
        let kinds = format!("{}{}", if body.contains("relay=") { "+relay" } else { "" }, if body.contains("addr=") { "+addr" } else { "" });
        if code == "200" { format!("200{kinds}") } else { code }
    }).unwrap_or_default()
}

/// Time from `bind()` returning until the pkarr server serves B's record (HTTP 200 on GET /pkarr/<z32 id>).
pub async fn publish_probe() {
    use iroh_relay::server::Server;
    let _s: Option<Server> = None;
    for (tag, base, local) in [("n0 dns.iroh.link", "https://dns.iroh.link/pkarr/", false), ("local iroh-dns-server, local relay", "http://127.0.0.1:8080/pkarr/", true), ("local iroh-dns-server, no relay (ip publish)", "http://127.0.0.1:8080/pkarr/", true)] {
        for i in 0..3 {
            let srv = if tag.contains("local relay") { Some(spawn_relay(0).await.unwrap()) } else { None };
            let mut b = if !local {
                Endpoint::builder(iroh::endpoint::presets::N0)
            } else {
                let publisher = iroh::address_lookup::PkarrPublisher::builder("http://127.0.0.1:8080/pkarr".parse().unwrap());
                let publisher = if srv.is_some() { publisher } else { publisher.addr_filter(iroh::address_lookup::AddrFilter::unfiltered()) };
                Endpoint::builder(Minimal).address_lookup(publisher)
            };
            b = match &srv {
                Some(s) => b.relay_mode(RelayMode::Custom(RelayMap::from(relay_url(s.http_addr().unwrap().port())))),
                None if local => b.relay_mode(RelayMode::Disabled),
                None => b,
            };
            let e = b.bind().await.unwrap();
            let t0 = std::time::Instant::now();
            let url = format!("{base}{}", e.id().to_z32());
            let mut code = String::new();
            while t0.elapsed() < Duration::from_secs(30) {
                code = http_code(&url).await;
                if code.contains("+relay") || code.contains("+addr") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            println!("PUBLISH {tag} i={i}: GET -> {code} after {:?}", t0.elapsed());
            e.close().await;
            if let Some(s) = srv {
                s.shutdown().await.unwrap();
            }
        }
    }
}
