//! Reproduction: datagrams toward a dead relay block an endpoint's sends to a live relay.
//!
//! Topology: relay R2 (live), relay R1 (killed during the run), endpoints X and Y homed on R2.
//! A third identity G ("ghost", no endpoint behind it) is addressed as id + R1 url.
//! Steps: X dials Y (must work); R1 is shut down; X starts BURST connect attempts toward G (each one sends
//! QUIC Initial datagrams toward R1); X dials Y with a fresh connection once per second. Y is on the live R2.
//!
//! usage: hol-repro [--burst N] [--wait-secs S] [--no-kill] [--never-up]
//!   --burst N      connect attempts toward G after the kill (default 200)
//!   --wait-secs S  how long X->Y may take to succeed before the run fails (default 15)
//!   --no-kill      keep R1 alive (control)
//!   --rebind-on-fail  workaround probe: after the first failed X->Y dial, close X and bind a new X (same key)
//!   --never-up     R1 is never started: its url points at a closed port (control for "relay died")
//! Output: lines `T +ms ...`, then one `RESULT PASS|FAIL ...` line. Exit code 0 pass, 1 fail.
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use tokio::time::{sleep, timeout};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const ALPN: &[u8] = b"hol/echo/0";

#[derive(Debug, Clone)]
struct Echo;
impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(AcceptError::from_err)?;
        let data = rx.read_to_end(64).await.map_err(AcceptError::from_err)?;
        tx.write_all(&data).await.map_err(AcceptError::from_err)?;
        tx.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind").local_addr().expect("addr").port()
}

async fn relay(port: u16) -> Res<Server> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, port)));
    Ok(Server::spawn(cfg).await?)
}

fn url(port: u16) -> RelayUrl {
    format!("http://127.0.0.1:{port}").parse().expect("url")
}

async fn bind(seed: u8, urls: Vec<RelayUrl>, want: &RelayUrl, lk: &MemoryLookup) -> Res<Endpoint> {
    // The home relay is chosen by latency; rebind until it is `want`.
    for _ in 0..40 {
        let ep = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::from_iter(urls.clone())))
            .clear_ip_transports()
            .secret_key(SecretKey::from_bytes(&[seed; 32]))
            .address_lookup(lk.clone())
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        let _ = timeout(Duration::from_secs(10), ep.online()).await;
        if ep.addr().relay_urls().cloned().collect::<Vec<_>>() == [want.clone()] {
            return Ok(ep);
        }
        ep.close().await;
    }
    Err("could not home".into())
}

async fn dial(ep: &Endpoint, id: EndpointId, secs: u64) -> Result<u128, String> {
    let t0 = Instant::now();
    let r = timeout(Duration::from_secs(secs), async {
        let c = ep.connect(EndpointAddr::from(id), ALPN).await.map_err(|e| e.to_string())?;
        let (mut s, mut r) = c.open_bi().await.map_err(|e| e.to_string())?;
        s.write_all(b"hi").await.map_err(|e| e.to_string())?;
        s.finish().map_err(|e| e.to_string())?;
        r.read_to_end(64).await.map_err(|e| e.to_string())?;
        c.close(0u32.into(), b"done");
        Ok::<(), String>(())
    })
    .await;
    match r {
        Ok(Ok(())) => Ok(t0.elapsed().as_millis()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("timeout after {secs}s")),
    }
}

#[tokio::main]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let num = |flag: &str, d: u64| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d);
    let (burst, wait_secs) = (num("--burst", 200), num("--wait-secs", 15));
    let rebind = args.iter().any(|a| a == "--rebind-on-fail");
    let (no_kill, never_up) = (args.iter().any(|a| a == "--no-kill"), args.iter().any(|a| a == "--never-up"));
    if let Ok(filter) = std::env::var("REPRO_TRACE") {
        let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).try_init();
    }
    let (p1, p2) = (free_port(), free_port());
    let (u1, u2) = (url(p1), url(p2));
    let r2 = relay(p2).await?;
    let r1 = if never_up { None } else { Some(relay(p1).await?) };
    let lk = MemoryLookup::new();
    let urls = vec![u1.clone(), u2.clone()];
    let mut x = bind(1, urls.clone(), &u2, &lk).await?;
    let y = bind(2, urls, &u2, &lk).await?;
    let _rx = Router::builder(x.clone()).accept(ALPN, Echo).spawn();
    let _ry = Router::builder(y.clone()).accept(ALPN, Echo).spawn();
    let ghost = SecretKey::from_bytes(&[99; 32]).public();
    lk.add_endpoint_info(EndpointAddr::new(y.id()).with_relay_url(u2.clone()));
    lk.add_endpoint_info(EndpointAddr::new(ghost).with_relay_url(u1.clone()));
    let t0 = Instant::now();
    let ms = || t0.elapsed().as_millis();
    println!("T +{}ms setup done: x_home={:?} y_home={:?} r1_up={}", ms(), x.addr().relay_urls().collect::<Vec<_>>(), y.addr().relay_urls().collect::<Vec<_>>(), r1.is_some());
    match dial(&x, y.id(), 5).await {
        Ok(d) => println!("T +{}ms pre-kill x->y ok in {d}ms", ms()),
        Err(e) => {
            println!("RESULT ERROR pre-kill dial failed: {e}");
            std::process::exit(2);
        }
    }
    let mut r1_keep = None;
    if no_kill {
        r1_keep = r1;
    } else if let Some(r1) = r1 {
        r1.shutdown().await.ok();
        println!("T +{}ms R1 shut down", ms());
    }
    sleep(Duration::from_secs(1)).await;
    println!("T +{}ms burst of {burst} connect attempts toward the ghost (addr id + R1 url)", ms());
    for _ in 0..burst {
        let x = x.clone();
        tokio::spawn(async move {
            let _ = dial(&x, ghost, 1).await;
        });
    }
    let t_burst = Instant::now();
    let mut first_ok = None;
    let mut fails = 0u32;
    while t_burst.elapsed() < Duration::from_secs(wait_secs) {
        let t = t_burst.elapsed().as_millis();
        match dial(&x, y.id(), 3).await {
            Ok(d) => {
                println!("T +{}ms x->y ok in {d}ms ({t}ms after burst)", ms());
                first_ok = Some(t);
                if fails == 0 || t > 0 {
                    break;
                }
            }
            Err(e) => {
                fails += 1;
                println!("T +{}ms x->y FAIL ({t}ms after burst): {e}", ms());
                if rebind && fails == 1 {
                    // Workaround probe: drop the dead address, close the endpoint, bind a fresh one with the same key.
                    lk.remove_endpoint_info(ghost);
                    let closed = timeout(Duration::from_secs(5), x.close()).await.is_ok();
                    x = bind(1, vec![u1.clone(), u2.clone()], &u2, &lk).await?;
                    println!("T +{}ms rebound X (old endpoint closed within 5s: {closed})", ms());
                }
            }
        }
        sleep(Duration::from_secs(1)).await;
    }
    let pass = fails == 0 && first_ok.is_some();
    println!(
        "RESULT {} burst={burst} kill={} never_up={never_up} failed_dials={fails} first_ok_after_burst_ms={first_ok:?} wait_secs={wait_secs}",
        if pass { "PASS" } else { "FAIL" },
        !no_kill && !never_up
    );
    drop((r2, r1_keep));
    std::process::exit(i32::from(!pass));
}
