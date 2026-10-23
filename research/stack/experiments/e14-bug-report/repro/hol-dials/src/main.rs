//! Multi-variant two-process reproduction, no gossip: relays R1 and R2 (own OS processes, SIGKILL), endpoints X and Y homed on R2,
//! P homed on R1. After the kill of R1, X keeps sending toward P (variant-dependent); every second X dials Y and Y dials X
//! (fresh connection, 3 s timeout, one echo), both homed on the live R2.
//! usage: hol-dials relay <port> | hol-dials run <variant> [window_s]
//! variants: ctl (X stops dialing P at the kill), p<N> (X dials P N times per second, never stops), suffixes:
//!   rm = X calls remove_relay(R1) 500 ms after the kill; rmlate = X calls it at its first failed dial to Y;
//!   rmall = X and Y call it 500 ms after the kill (P not asked).
use std::{
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, QuicTransportConfig, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const ECHO: &[u8] = b"hol/echo/0";

fn key(n: u8) -> SecretKey {
    SecretKey::from_bytes(&[n; 32])
}
fn url(port: u16) -> RelayUrl {
    format!("http://127.0.0.1:{port}").parse().expect("url")
}
fn short<E: std::fmt::Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(90).collect::<String>().replace(' ', "_")
}
fn quic() -> QuicTransportConfig {
    QuicTransportConfig::builder().max_idle_timeout(Some(Duration::from_secs(10).try_into().expect("idle"))).build()
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr").port()
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

fn init_trace(path: &str) {
    let Ok(filter) = std::env::var("HOL_TRACE") else { return };
    let Ok(f) = std::fs::File::create(path) else { return };
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(Mutex::new(f)).with_ansi(false).try_init();
}

async fn relay(port: u16) -> Res<()> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, port)));
    let _srv = Server::spawn(cfg).await?;
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

async fn bind(k: u8, urls: &[RelayUrl], lk: &MemoryLookup, want: &RelayUrl) -> Res<Endpoint> {
    for _ in 0..40 {
        let ep = Endpoint::builder(Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::from_iter(urls.to_vec())))
            .clear_ip_transports()
            .secret_key(key(k))
            .transport_config(quic())
            .address_lookup(lk.clone())
            .alpns(vec![ECHO.to_vec()])
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

async fn dial(ep: &Endpoint, id: EndpointId, secs: u64) -> (bool, u128, String) {
    let t0 = Instant::now();
    let r = timeout(Duration::from_secs(secs), async {
        let c = ep.connect(EndpointAddr::from(id), ECHO).await.map_err(short)?;
        let (mut s, mut r) = c.open_bi().await.map_err(short)?;
        s.write_all(b"hol").await.map_err(short)?;
        s.finish().map_err(short)?;
        r.read_to_end(64).await.map_err(short)?;
        c.close(0u32.into(), b"done");
        Ok::<(), String>(())
    })
    .await;
    let ms = t0.elapsed().as_millis();
    match r {
        Ok(Ok(())) => (true, ms, String::new()),
        Ok(Err(e)) => (false, ms, e),
        Err(_) => (false, ms, format!("timeout{secs}s")),
    }
}

fn spawn_relay(port: u16) -> Res<Child> {
    let exe = std::env::current_exe()?;
    Ok(Command::new(exe).arg("relay").arg(port.to_string()).kill_on_drop(true).spawn()?)
}

async fn run(variant: String, window: u64) -> Res<()> {
    init_trace(&format!("/tmp/hol-trace-{variant}.log"));
    let (p1, p2) = (free_port(), free_port());
    let (u1, u2) = (url(p1), url(p2));
    let mut r1 = spawn_relay(p1)?;
    let _r2 = spawn_relay(p2)?;
    sleep(Duration::from_millis(800)).await;
    let lk = MemoryLookup::new();
    let x = bind(21, &[u1.clone(), u2.clone()], &lk, &u2).await?;
    let y = bind(22, &[u1.clone(), u2.clone()], &lk, &u2).await?;
    let p = bind(23, std::slice::from_ref(&u1), &lk, &u1).await?;
    let _rx = Router::builder(x.clone()).accept(ECHO, Echo).spawn();
    let _ry = Router::builder(y.clone()).accept(ECHO, Echo).spawn();
    let _rp = Router::builder(p.clone()).accept(ECHO, Echo).spawn();
    lk.add_endpoint_info(EndpointAddr::new(x.id()).with_relay_url(u2.clone()));
    lk.add_endpoint_info(EndpointAddr::new(y.id()).with_relay_url(u2.clone()));
    lk.add_endpoint_info(EndpointAddr::new(p.id()).with_relay_url(u1.clone()));
    println!(
        "SETUP x_home={:?} y_home={:?} p_home={:?} variant={variant}",
        x.addr().relay_urls().collect::<Vec<_>>(),
        y.addr().relay_urls().collect::<Vec<_>>(),
        p.addr().relay_urls().collect::<Vec<_>>()
    );
    let n_p: u64 = variant
        .strip_prefix('p')
        .and_then(|s| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok())
        .unwrap_or(0);
    // suffix s<N>: X stops dialing P at kill+N s; a<N>: the lookup entry of P is removed at kill+N s while X keeps dialing.
    let num_after = |c: char| -> Option<u64> { variant.split(c).nth(1).and_then(|t| t.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()) };
    let stop_after = if variant.starts_with('p') { num_after('s') } else { None };
    let addr_after = if variant.starts_with('p') { num_after('a') } else { None };
    let rm = variant.ends_with("rm");
    let rmlate = variant.ends_with("rmlate");
    let rmall = variant.ends_with("rmall");
    let stop = Arc::new(AtomicBool::new(false));
    let killed = Arc::new(AtomicBool::new(false));
    let killed_at = Arc::new(Mutex::new(None::<Instant>));
    // X dials P n_p times per second (ctl: once per second until the kill).
    {
        let (x, pid, stop, killed) = (x.clone(), p.id(), stop.clone(), killed.clone());
        let ctl = n_p == 0;
        let rate = n_p.max(1);
        let t_k = killed_at.clone();
        let stop_after = stop_after;
        tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) || (ctl && killed.load(Ordering::Relaxed)) {
                    return;
                }
                if let (Some(n), Some(t)) = (stop_after, *t_k.lock().expect("l")) {
                    if t.elapsed() >= Duration::from_secs(n) {
                        return;
                    }
                }
                for _ in 0..rate {
                    let x = x.clone();
                    tokio::spawn(async move {
                        let _ = dial(&x, pid, 3).await;
                    });
                }
                sleep(Duration::from_secs(1)).await;
            }
        });
    }
    sleep(Duration::from_secs(6)).await;
    let pre = dial(&x, y.id(), 3).await;
    println!("PRE dial x->y ok={} ms={}", pre.0, pre.1);
    let t_kill = Instant::now();
    killed.store(true, Ordering::Relaxed);
    *killed_at.lock().expect("l") = Some(t_kill);
    r1.kill().await?;
    if let Some(n) = addr_after {
        let (lk, pid) = (lk.clone(), p.id());
        tokio::spawn(async move {
            sleep(Duration::from_secs(n)).await;
            lk.remove_endpoint_info(pid);
            println!("ADDR_REMOVED t={}", t_kill.elapsed().as_millis());
        });
    }
    println!("KILL t=0");
    if rm || rmall {
        let (x, y, u) = (x.clone(), y.clone(), u1.clone());
        tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            let _ = x.remove_relay(&u).await;
            println!("RM x t={}", t_kill.elapsed().as_millis());
            if rmall {
                let _ = y.remove_relay(&u).await;
                println!("RM y t={}", t_kill.elapsed().as_millis());
            }
        });
    }
    let mut first_fail = None;
    let mut recovered = None;
    let mut removed = false;
    while t_kill.elapsed() < Duration::from_secs(window) {
        let (xy, yx) = tokio::join!(dial(&x, y.id(), 3), dial(&y, x.id(), 3));
        let t = t_kill.elapsed().as_millis();
        println!("DIAL t={t} x->y ok={} ms={} {} | y->x ok={} ms={} {}", xy.0, xy.1, xy.2, yx.0, yx.1, yx.2);
        if (!xy.0 || !yx.0) && first_fail.is_none() {
            first_fail = Some(t);
            if rmlate && !removed {
                removed = true;
                let _ = x.remove_relay(&u1).await;
                println!("RMLATE x t={}", t_kill.elapsed().as_millis());
            }
        }
        if first_fail.is_some() && xy.0 && yx.0 && recovered.is_none() {
            recovered = Some(t);
        }
        let spent = u64::try_from(xy.1.max(yx.1)).unwrap_or(1000).min(1000);
        sleep(Duration::from_millis(1000 - spent)).await;
    }
    stop.store(true, Ordering::Relaxed);
    println!("SUMMARY variant={variant} first_fail_ms={first_fail:?} recovered_ms={recovered:?}");
    Ok(())
}

#[tokio::main]
async fn main() -> Res<()> {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("relay") => relay(a.get(2).and_then(|s| s.parse().ok()).ok_or("port")?).await,
        Some("run") => run(a.get(2).cloned().ok_or("variant")?, a.get(3).and_then(|s| s.parse().ok()).unwrap_or(90)).await,
        _ => Err("usage".into()),
    }
}
