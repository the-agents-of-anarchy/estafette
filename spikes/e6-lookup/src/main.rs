//! E6 address lookup by id. Prints `ROW` lines; one per run.
mod mdns;
mod multi;
mod n0;
mod presence;
mod selfhost;

use std::{net::Ipv4Addr, time::{Duration, Instant}};

use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use tokio::time::{sleep, timeout};

pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub const ECHO: &[u8] = b"e6/echo/0";

#[derive(Debug, Clone)]
pub struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(AcceptError::from_err)?;
        let data = rx.read_to_end(1 << 20).await.map_err(AcceptError::from_err)?;
        tx.write_all(&data).await.map_err(AcceptError::from_err)?;
        tx.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }
}

pub fn short<E: std::fmt::Display>(e: E) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(140).collect()
}

pub fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

pub fn paths_str(c: &Connection) -> String {
    c.paths()
        .iter()
        .map(|p| format!("{}{:?}", if p.is_selected() { "*" } else { "" }, p.remote_addr()))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn ports(e: &Endpoint) -> String {
    e.bound_sockets().iter().map(|s| s.port().to_string()).collect::<Vec<_>>().join("/")
}

pub async fn echo_roundtrip(conn: &Connection, msg: &[u8]) -> Res<Vec<u8>> {
    let (mut tx, mut rx) = conn.open_bi().await?;
    tx.write_all(msg).await?;
    tx.finish()?;
    Ok(rx.read_to_end(1 << 20).await?)
}

pub async fn serve(ep: Endpoint) -> Router {
    Router::builder(ep).accept(ECHO, Echo).spawn()
}

pub async fn spawn_relay(port: u16) -> Res<Server> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, port)));
    Ok(Server::spawn(cfg).await?)
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub fn relay_url(port: u16) -> RelayUrl {
    format!("http://127.0.0.1:{port}").parse().unwrap()
}

pub fn relay_mode(url: &RelayUrl) -> RelayMode {
    RelayMode::Custom(RelayMap::from(url.clone()))
}

#[derive(Debug, Clone, Default)]
pub struct Dial {
    pub ok: bool,
    pub ms: f64,
    pub attempts: u32,
    pub path: String,
    pub last_err: String,
    /// Per attempt: elapsed-at-end ms and outcome (first 6).
    pub log: Vec<String>,
}

impl std::fmt::Display for Dial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ok={} ms={} attempts={} path=[{}] last_err=[{}] log={:?}", self.ok, self.ms, self.attempts, self.path, self.last_err, self.log)
    }
}

/// Per-attempt connect timeout; env `E6_ATTEMPT_MS`, default 3000.
fn attempt_timeout() -> Duration {
    Duration::from_millis(std::env::var("E6_ATTEMPT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000))
}

/// Retry `a.connect(target)` + echo until success or `cap`. `attempt_timeout()` per attempt, 10 ms between attempts.
pub async fn dial_until(a: &Endpoint, target: EndpointAddr, cap: Duration) -> Dial {
    let t0 = Instant::now();
    let mut d = Dial::default();
    while t0.elapsed() < cap {
        d.attempts += 1;
        let before = d.last_err.clone();
        match timeout(attempt_timeout(), a.connect(target.clone(), ECHO)).await {
            Ok(Ok(c)) => match timeout(Duration::from_secs(5), echo_roundtrip(&c, b"e6")).await {
                Ok(Ok(_)) => {
                    d.ok = true;
                    d.ms = ms(t0.elapsed());
                    d.path = paths_str(&c);
                    d.log.push(format!("{}ms:ok", d.ms));
                    c.close(0u32.into(), b"done");
                    return d;
                }
                Ok(Err(e)) => d.last_err = format!("echo {}", short(e)),
                Err(_) => d.last_err = "echo timeout 5s".into(),
            },
            Ok(Err(e)) => d.last_err = short(e),
            Err(_) => d.last_err = format!("attempt timeout {}ms", attempt_timeout().as_millis()),
        }
        if d.log.len() < 6 {
            let e = if d.last_err == before { "?" } else { d.last_err.as_str() };
            d.log.push(format!("{}ms:{}", ms(t0.elapsed()), e.chars().take(30).collect::<String>()));
        }
        sleep(Duration::from_millis(10)).await;
    }
    d.ms = ms(t0.elapsed());
    d
}

/// Bind B, dial by id from A, close B, rebind B with the same key (new port), dial by id again.
/// `mk(sk, alpns)` binds one endpoint; `seed(b)` runs after B's first bind, before the first dial.
pub async fn two_phase(
    name: &str,
    run: u32,
    cap: Duration,
    settle: Duration,
    mk: impl AsyncFn(Option<SecretKey>, Vec<Vec<u8>>) -> Res<Endpoint>,
    seed: impl AsyncFn(&Endpoint),
) -> Res<()> {
    let a = mk(None, vec![]).await?;
    let a2 = mk(None, vec![]).await?;
    let sk = SecretKey::generate();
    let b = mk(Some(sk.clone()), vec![ECHO.to_vec()]).await?;
    let id = b.id();
    let old = ports(&b);
    seed(&b).await;
    let r = serve(b).await;
    let first = dial_until(&a, id.into(), cap).await;
    // Control: a second dialer that never saw B dials after `settle`, when B's record is long published.
    sleep(settle).await;
    let first_settled = dial_until(&a2, id.into(), cap).await;
    r.shutdown().await?;
    let sk2 = sk.clone();
    let b2 = mk(Some(sk), vec![ECHO.to_vec()]).await?;
    let new = ports(&b2);
    seed(&b2).await;
    let r2 = serve(b2).await;
    let restart = dial_until(&a, id.into(), cap).await;
    // Control: restart again, wait `settle` before dialing, so the new address is already known when the dial starts.
    r2.shutdown().await?;
    let b3 = mk(Some(sk2), vec![ECHO.to_vec()]).await?;
    seed(&b3).await;
    let r3 = serve(b3).await;
    sleep(settle).await;
    let settled = dial_until(&a, id.into(), cap).await;
    println!("ROW {name} run={run} T_ms={} old_port={old} new_port={new}\n  first: {first}\n  first-settled({settle:?}, fresh dialer): {first_settled}\n  restart: {restart}\n  restart-settled({settle:?}): {settled}", attempt_timeout().as_millis());
    r3.shutdown().await?;
    a2.close().await;
    a.close().await;
    Ok(())
}

/// Baseline: no lookup service. A holds B's first address in a MemoryLookup and is never told the new one.
async fn stale(run: u32, relay: bool) -> Res<()> {
    let srv = spawn_relay(0).await?;
    let url: RelayUrl = format!("http://{}", srv.http_addr().unwrap()).parse()?;
    let lookup = MemoryLookup::new();
    let mode = move || if relay { relay_mode(&url) } else { RelayMode::Disabled };
    let lk = lookup.clone();
    let mk = async |sk: Option<SecretKey>, alpns: Vec<Vec<u8>>| -> Res<Endpoint> {
        let mut b = Endpoint::builder(Minimal).relay_mode(mode()).alpns(alpns);
        if let Some(sk) = sk {
            b = b.secret_key(sk);
        } else {
            b = b.address_lookup(lk.clone());
        }
        Ok(b.bind().await?)
    };
    let seeded = std::sync::atomic::AtomicBool::new(false);
    let seed = async |b: &Endpoint| {
        if relay {
            let _ = timeout(Duration::from_secs(10), b.online()).await;
        }
        if !seeded.swap(true, std::sync::atomic::Ordering::SeqCst) {
            lookup.add_endpoint_info(b.addr());
        }
    };
    let name = if relay { "stale-memory+relay" } else { "stale-memory-direct" };
    two_phase(name, run, Duration::from_secs(15), Duration::from_secs(2), mk, seed).await?;
    srv.shutdown().await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let which = args.get(1).map_or("all", String::as_str);
    let runs: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let all = which == "all";
    for run in 1..=runs {
        if all || which == "stale" {
            for relay in [true, false] {
                if let Err(e) = stale(run, relay).await { println!("ERR stale relay={relay} run={run} {}", short(e)); }
            }
        }
        if all || which == "n0" {
            for (relay, unfiltered) in [(true, false), (false, false), (false, true)] {
                if let Err(e) = n0::run(run, relay, unfiltered).await { println!("ERR n0 relay={relay} unfiltered={unfiltered} run={run} {}", short(e)); }
            }
        }
        if all || which == "mdns" {
            if let Err(e) = mdns::run(run).await { println!("ERR mdns run={run} {}", short(e)); }
        }
        if all || which == "presence" {
            for relay in [true, false] {
                for payload in [true, false] {
                    if let Err(e) = presence::run(run, relay, payload).await { println!("ERR presence relay={relay} payload={payload} run={run} {}", short(e)); }
                }
            }
        }
        if which == "selfhost" {
            for relay in [true, false] {
                if let Err(e) = selfhost::run(run, relay).await { println!("ERR selfhost relay={relay} run={run} {}", short(e)); }
            }
        }
        if all || which == "multi" {
            if let Err(e) = multi::run(run).await { println!("ERR multi run={run} {}", short(e)); }
        }
    }
    if which == "publish" {
        multi::publish_probe().await;
    }
    if which == "online" {
        multi::online_probe().await;
    }
    if which == "probe" {
        multi::probe().await;
    }
}
