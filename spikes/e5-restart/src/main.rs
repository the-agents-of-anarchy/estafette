//! E5: node-as-identity under crash and same-key restart. One binary, three roles:
//! `node` (a peer or K), `lock` (flock holder), `scen` (orchestrator spawning the others as OS processes).
use std::{
    collections::HashMap,
    fmt::Display,
    fs::File,
    io::Read,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, QuicTransportConfig, presets::Minimal},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{Gossip, TopicId, api::Event, net::GOSSIP_ALPN};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use n0_future::StreamExt;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::{sleep, timeout},
};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ECHO: &[u8] = b"e5/echo/0";

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn ev(name: &str, event: &str, kv: impl Display) {
    println!("{} {name} {event} {kv}", now_ns());
}

// ---------------------------------------------------------------- args
struct Args(HashMap<String, Vec<String>>);

impl Args {
    fn parse(v: &[String]) -> Self {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < v.len() {
            if let Some(k) = v[i].strip_prefix("--") {
                if i + 1 < v.len() && !v[i + 1].starts_with("--") {
                    m.entry(k.into()).or_default().push(v[i + 1].clone());
                    i += 2;
                } else {
                    m.entry(k.into()).or_default().push(String::new());
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        Self(m)
    }
    fn one(&self, k: &str) -> Option<&str> {
        self.0.get(k).and_then(|v| v.first()).map(String::as_str)
    }
    fn all(&self, k: &str) -> &[String] {
        self.0.get(k).map_or(&[], Vec::as_slice)
    }
    fn flag(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
}

// ---------------------------------------------------------------- lock
/// Exclusive flock on the key file, retried every 10 ms when `retry`; held for the life of the returned File.
async fn take_lock(name: &str, path: &str, retry: bool) -> Res<File> {
    let f = File::open(path)?;
    let t0 = Instant::now();
    let mut failed = false;
    loop {
        match f.try_lock() {
            Ok(()) => {
                ev(name, "LOCKED", format!("waited_ms={:.1}", t0.elapsed().as_secs_f64() * 1000.0));
                return Ok(f);
            }
            Err(e) => {
                if !failed {
                    ev(name, "LOCKFAIL", format!("debug={e:?} display={e}"));
                    failed = true;
                }
                if !retry {
                    std::process::exit(3);
                }
                sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn lock_role(a: Args) -> Res<()> {
    let name = a.one("name").unwrap_or("L");
    let _f = take_lock(name, a.one("key").ok_or("no key")?, a.flag("retry")).await?;
    std::future::pending::<()>().await;
    Ok(())
}

// ---------------------------------------------------------------- node
#[derive(Debug, Clone)]
struct Echo {
    pid: u32,
}

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        while let Ok((mut tx, mut rx)) = conn.accept_bi().await {
            if rx.read_to_end(64).await.is_err() {
                break;
            }
            let _ = tx.write_all(self.pid.to_string().as_bytes()).await;
            let _ = tx.finish();
        }
        Ok(())
    }
}

fn short(e: impl Display) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(120).collect::<String>().replace(' ', "_")
}

async fn probe_held(name: String, conn: Connection, seq: u64) {
    let t0 = Instant::now();
    let start = now_ns();
    let r = timeout(Duration::from_secs(5), async {
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| format!("open_bi:{}", short(e)))?;
        tx.write_all(b"ping").await.map_err(|e| format!("write:{}", short(e)))?;
        tx.finish().map_err(|e| format!("finish:{}", short(e)))?;
        let b = rx.read_to_end(64).await.map_err(|e| format!("read:{}", short(e)))?;
        Ok::<_, String>(String::from_utf8_lossy(&b).to_string())
    })
    .await;
    report_probe(&name, "held", seq, start, t0, r);
}

async fn probe_fresh(name: String, ep: Endpoint, target: EndpointId, seq: u64) {
    let t0 = Instant::now();
    let start = now_ns();
    let r = timeout(Duration::from_secs(5), async {
        let conn = ep.connect(target, ECHO).await.map_err(|e| format!("connect:{}", short(e)))?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| format!("open_bi:{}", short(e)))?;
        tx.write_all(b"ping").await.map_err(|e| format!("write:{}", short(e)))?;
        tx.finish().map_err(|e| format!("finish:{}", short(e)))?;
        let b = rx.read_to_end(64).await.map_err(|e| format!("read:{}", short(e)))?;
        conn.close(0u32.into(), b"done");
        Ok::<_, String>(String::from_utf8_lossy(&b).to_string())
    })
    .await;
    report_probe(&name, "fresh", seq, start, t0, r);
}

fn report_probe(name: &str, kind: &str, seq: u64, start: u128, t0: Instant, r: Result<Result<String, String>, tokio::time::error::Elapsed>) {
    let dur = t0.elapsed().as_secs_f64() * 1000.0;
    let res = match r {
        Ok(Ok(reply)) => format!("res=ok reply={reply}"),
        Ok(Err(e)) => format!("res=err msg={e}"),
        Err(_) => "res=timeout".to_string(),
    };
    ev(name, "PROBE", format!("kind={kind} seq={seq} start={start} dur_ms={dur:.1} {res}"));
}

async fn node(a: Args) -> Res<()> {
    let name = a.one("name").ok_or("no name")?.to_string();
    let pid = std::process::id();
    let keyp = a.one("key").ok_or("no key")?;
    let _lock = if a.flag("nolock") { None } else { Some(take_lock(&name, keyp, true).await?) };
    let mut kb = [0u8; 32];
    File::open(keyp)?.read_exact(&mut kb)?;
    let sk = SecretKey::from_bytes(&kb);

    let relay_mode = a.one("mode") == Some("relay");
    let relay_url: Option<RelayUrl> = a.one("relay").map(str::parse).transpose()?;
    let port: u16 = a.one("port").unwrap_or("0").parse()?;
    let lookup = MemoryLookup::new();
    let tc = QuicTransportConfig::builder().max_idle_timeout(Some(Duration::from_secs(10).try_into()?)).build();
    let mut b = Endpoint::builder(Minimal)
        .secret_key(sk.clone())
        .address_lookup(lookup.clone())
        .transport_config(tc)
        .clear_ip_transports();
    if relay_mode {
        let url = relay_url.clone().ok_or("no relay url")?;
        b = b.relay_mode(RelayMode::Custom(RelayMap::from(url)));
    } else {
        b = b.relay_mode(RelayMode::Disabled).bind_addr(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))?;
    }
    let ep = b.bind().await?;

    let mut names: HashMap<EndpointId, String> = HashMap::new();
    let mut ids: HashMap<String, EndpointId> = HashMap::new();
    names.insert(sk.public(), name.clone());
    for spec in a.all("peer") {
        let p: Vec<&str> = spec.split(',').collect();
        let id: EndpointId = p[1].parse()?;
        let pport: u16 = p[2].parse()?;
        let mut addr = EndpointAddr::new(id);
        if relay_mode {
            addr = addr.with_relay_url(relay_url.clone().ok_or("no relay url")?);
        } else {
            addr = addr.with_ip_addr(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), pport));
        }
        lookup.add_endpoint_info(addr);
        names.insert(id, p[0].to_string());
        ids.insert(p[0].to_string(), id);
    }
    if relay_mode {
        let r = timeout(Duration::from_secs(15), ep.online()).await;
        ev(&name, "ONLINE", format!("{}", r.is_ok()));
    }
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(ECHO, Echo { pid })
        .spawn();
    ev(&name, "READY", format!("pid={pid} id={} bound={:?}", sk.public(), ep.bound_sockets()));

    let boots: Vec<EndpointId> = a.all("bootstrap").iter().filter_map(|n| ids.get(n).copied()).collect();
    let topic = gossip.subscribe(TopicId::from_bytes([7u8; 32]), boots).await?;
    let (tx, mut rx) = topic.split();

    let nm = name.clone();
    let names2 = names.clone();
    tokio::spawn(async move {
        let who = |id: &EndpointId| names2.get(id).cloned().unwrap_or_else(|| id.fmt_short().to_string());
        while let Some(e) = rx.next().await {
            match e {
                Ok(Event::NeighborUp(id)) => ev(&nm, "NEIGHBOR_UP", who(&id)),
                Ok(Event::NeighborDown(id)) => ev(&nm, "NEIGHBOR_DOWN", who(&id)),
                Ok(Event::Received(m)) => ev(
                    &nm,
                    "RECV",
                    format!("from={} msg={}", who(&m.delivered_from), String::from_utf8_lossy(&m.content)),
                ),
                Ok(Event::Lagged) => ev(&nm, "LAGGED", ""),
                Err(e) => ev(&nm, "GOSSIP_ERR", short(e)),
            }
        }
        ev(&nm, "GOSSIP_STREAM_END", "");
    });

    if a.flag("broadcast") {
        tokio::spawn(async move {
            let mut seq = 0u64;
            loop {
                seq += 1;
                let _ = tx.broadcast(Bytes::from(format!("{pid}:{seq}"))).await;
                sleep(Duration::from_millis(200)).await;
            }
        });
    }

    if let Some(t) = a.one("probe") {
        let target = *ids.get(t).ok_or("probe target unknown")?;
        let held = loop {
            match timeout(Duration::from_secs(5), ep.connect(target, ECHO)).await {
                Ok(Ok(c)) => break c,
                _ => sleep(Duration::from_millis(200)).await,
            }
        };
        ev(&name, "HELD_OPEN", "");
        let mut seq = 0u64;
        loop {
            seq += 1;
            tokio::spawn(probe_held(name.clone(), held.clone(), seq));
            tokio::spawn(probe_fresh(name.clone(), ep.clone(), target, seq));
            sleep(Duration::from_millis(200)).await;
        }
    }
    std::future::pending::<()>().await;
    Ok(())
}

// ---------------------------------------------------------------- orchestrator
type Lines = Arc<Mutex<Vec<(u128, String)>>>;

struct Harness {
    exe: PathBuf,
    lines: Lines,
    out: PathBuf,
}

impl Harness {
    fn note(&self, text: impl Display) {
        let ns = now_ns();
        if let Ok(mut l) = self.lines.lock() {
            l.push((ns, format!("{ns} ORCH NOTE {text}")));
        }
    }
    fn start(&self, name: &str, args: &[String]) -> Res<Child> {
        let mut c = Command::new(&self.exe)
            .args(args)
            .env("RUST_LOG", std::env::var("E5_CHILD_LOG").unwrap_or_else(|_| "warn".into()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let so = c.stdout.take().ok_or("stdout")?;
        let se = c.stderr.take().ok_or("stderr")?;
        let l1 = self.lines.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(so).lines();
            while let Ok(Some(line)) = r.next_line().await {
                let ns = line.split(' ').next().and_then(|t| t.parse::<u128>().ok()).unwrap_or_else(now_ns);
                if let Ok(mut l) = l1.lock() {
                    l.push((ns, line));
                }
            }
        });
        let l2 = self.lines.clone();
        let nm = name.to_string();
        tokio::spawn(async move {
            let mut r = BufReader::new(se).lines();
            while let Ok(Some(line)) = r.next_line().await {
                let ns = now_ns();
                if let Ok(mut l) = l2.lock() {
                    l.push((ns, format!("{ns} {nm} STDERR {line}")));
                }
            }
        });
        self.note(format!("spawned {name} pid={:?}", c.id()));
        Ok(c)
    }
    async fn wait_for(&self, name: &str, event: &str, after: u128, secs: u64) -> Option<u128> {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if let Ok(l) = self.lines.lock() {
                for (ns, line) in l.iter() {
                    let mut t = line.split(' ');
                    let _ = t.next();
                    if *ns >= after && t.next() == Some(name) && t.next() == Some(event) {
                        return Some(*ns);
                    }
                }
            }
            sleep(Duration::from_millis(20)).await;
        }
        None
    }
    async fn kill(&self, name: &str, c: &mut Child) -> u128 {
        let t = now_ns();
        self.note(format!("KILL {name} t={t}"));
        let _ = c.start_kill();
        let _ = c.wait().await;
        self.note(format!("REAPED {name}"));
        t
    }
    fn dump(&self) -> Res<()> {
        let mut l = self.lines.lock().map_err(|_| "poison")?.clone();
        l.sort_by_key(|(n, _)| *n);
        let s: String = l.iter().map(|(_, s)| format!("{s}\n")).collect();
        std::fs::write(&self.out, s)?;
        Ok(())
    }
}

struct Cfg {
    dir: PathBuf,
    relay: Option<String>,
    ids: HashMap<String, String>,
    ports: HashMap<String, u16>,
}

fn free_ports(n: usize) -> Res<Vec<u16>> {
    let socks: Vec<UdpSocket> = (0..n).map(|_| UdpSocket::bind("127.0.0.1:0")).collect::<Result<_, _>>()?;
    Ok(socks.iter().map(|s| s.local_addr().map(|a| a.port()).unwrap_or(0)).collect())
}

fn write_key(path: &PathBuf) -> Res<String> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let sk = SecretKey::generate();
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(&sk.to_bytes())?;
    Ok(sk.public().to_string())
}

fn setup(dir: PathBuf, relay: Option<String>) -> Res<Cfg> {
    std::fs::create_dir_all(&dir)?;
    let names = ["K", "P1", "P2"];
    let ports = free_ports(4)?;
    let mut ids = HashMap::new();
    let mut pm = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        ids.insert((*n).to_string(), write_key(&dir.join(format!("{n}.key")))?);
        pm.insert((*n).to_string(), if relay.is_some() { 0 } else { ports[i] });
    }
    pm.insert("Knew".into(), if relay.is_some() { 0 } else { ports[3] });
    Ok(Cfg { dir, relay, ids, ports: pm })
}

impl Cfg {
    fn node_args(&self, name: &str, key_of: &str, port: u16, bootstrap: &[&str], probe: Option<&str>, extra: &[&str]) -> Vec<String> {
        let mut a: Vec<String> = vec!["node".into(), "--name".into(), name.into(), "--key".into(), self.dir.join(format!("{key_of}.key")).display().to_string()];
        a.extend(["--port".into(), port.to_string()]);
        match &self.relay {
            Some(u) => a.extend(["--mode".into(), "relay".into(), "--relay".into(), u.clone()]),
            None => a.extend(["--mode".into(), "direct".into()]),
        }
        for p in ["K", "P1", "P2"] {
            if p != key_of {
                a.extend(["--peer".into(), format!("{p},{},{}", self.ids[p], self.ports[p])]);
            }
        }
        for b in bootstrap {
            a.extend(["--bootstrap".into(), (*b).into()]);
        }
        if let Some(p) = probe {
            a.extend(["--probe".into(), p.into()]);
        }
        a.extend(extra.iter().map(|s| (*s).to_string()));
        a
    }
}

async fn spawn_relay() -> Res<(Server, String)> {
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let s = Server::spawn(cfg).await?;
    let url = format!("http://{}", s.http_addr().ok_or("no http addr")?);
    Ok((s, url))
}

async fn scen_lock(h: &Harness, dir: PathBuf) -> Res<()> {
    let cfg = setup(dir, None)?;
    let key = cfg.dir.join("K.key").display().to_string();
    let mut a = h.start("A", &["lock".into(), "--name".into(), "A".into(), "--key".into(), key.clone()])?;
    h.wait_for("A", "LOCKED", 0, 10).await.ok_or("A no lock")?;
    let mut b = h.start("B", &["lock".into(), "--name".into(), "B".into(), "--key".into(), key, "--retry".into()])?;
    h.wait_for("B", "LOCKFAIL", 0, 10).await.ok_or("B no lockfail")?;
    sleep(Duration::from_secs(1)).await;
    let t = h.kill("A", &mut a).await;
    let got = h.wait_for("B", "LOCKED", t, 10).await;
    h.note(format!("RESULT lock_after_kill_ms={:?}", got.map(|g| (g - t) as f64 / 1e6)));
    let _ = b.start_kill();
    Ok(())
}

async fn start_base(h: &Harness, cfg: &Cfg) -> Res<(Child, Child, Child)> {
    let k = h.start("K", &cfg.node_args("K", "K", cfg.ports["K"], &["P1", "P2"], None, &["--broadcast"]))?;
    let p1 = h.start("P1", &cfg.node_args("P1", "P1", cfg.ports["P1"], &["K"], Some("K"), &["--broadcast"]))?;
    let p2 = h.start("P2", &cfg.node_args("P2", "P2", cfg.ports["P2"], &["K"], Some("K"), &["--broadcast"]))?;
    for n in ["K", "P1", "P2"] {
        h.wait_for(n, "READY", 0, 30).await.ok_or(format!("{n} not ready"))?;
    }
    for n in ["P1", "P2"] {
        h.wait_for(n, "HELD_OPEN", 0, 30).await.ok_or(format!("{n} no held conn"))?;
        h.wait_for(n, "RECV", 0, 30).await.ok_or(format!("{n} no gossip from K"))?;
    }
    sleep(Duration::from_secs(2)).await;
    Ok((k, p1, p2))
}

async fn scen_restart(h: &Harness, cfg: &Cfg, delay_ms: u64, new_port: bool) -> Res<()> {
    let (mut k, mut p1, mut p2) = start_base(h, cfg).await?;
    let t = h.kill("K", &mut k).await;
    sleep(Duration::from_millis(delay_ms)).await;
    let port = if new_port { cfg.ports["Knew"] } else { cfg.ports["K"] };
    h.note(format!("RESTART_SPAWN delay_ms={delay_ms} port={port}"));
    let mut k2 = h.start("K", &cfg.node_args("K", "K", port, &["P1", "P2"], None, &["--broadcast"]))?;
    sleep(Duration::from_secs(22).saturating_sub(Duration::from_millis(delay_ms))).await;
    h.note(format!("END after_kill_s=22 t_kill={t}"));
    let _ = k2.start_kill();
    let _ = p1.start_kill();
    let _ = p2.start_kill();
    Ok(())
}

async fn scen_dup(h: &Harness, cfg: &Cfg) -> Res<()> {
    let (mut k, mut p1, mut p2) = start_base(h, cfg).await?;
    h.note("DUP_START K2 with same key, lock bypassed");
    let mut k2 = h.start("K2", &cfg.node_args("K2", "K", cfg.ports["Knew"], &["P1", "P2"], None, &["--broadcast", "--nolock"]))?;
    h.wait_for("K2", "READY", 0, 30).await.ok_or("K2 not ready")?;
    sleep(Duration::from_secs(15)).await;
    let t = h.kill("K2", &mut k2).await;
    h.note(format!("K2 killed t={t}"));
    sleep(Duration::from_secs(25)).await;
    h.note("END");
    let _ = k.start_kill();
    let _ = p1.start_kill();
    let _ = p2.start_kill();
    Ok(())
}

async fn orchestrate(a: Args) -> Res<()> {
    let which = a.one("s").ok_or("no --s")?.to_string();
    let runs: usize = a.one("runs").unwrap_or("3").parse()?;
    let delay: u64 = a.one("delay").unwrap_or("500").parse()?;
    let root = PathBuf::from(a.one("out").unwrap_or("/tmp/e5/out"));
    std::fs::create_dir_all(&root)?;
    for i in 1..=runs {
        let tag = format!("{which}-d{delay}-{i}");
        let h = Harness { exe: std::env::current_exe()?, lines: Arc::default(), out: root.join(format!("{tag}.log")) };
        let dir = PathBuf::from(format!("/tmp/e5/run/{tag}"));
        let relay_variant = which.ends_with("relay");
        let relay = if relay_variant { Some(spawn_relay().await?) } else { None };
        let r = match which.as_str() {
            "lock" => scen_lock(&h, dir).await,
            "restart-direct" | "restart-newport" | "restart-relay" => {
                let cfg = setup(dir, relay.as_ref().map(|r| r.1.clone()))?;
                scen_restart(&h, &cfg, delay, which == "restart-newport").await
            }
            "dup-direct" | "dup-relay" => {
                let cfg = setup(dir, relay.as_ref().map(|r| r.1.clone()))?;
                scen_dup(&h, &cfg).await
            }
            _ => Err("unknown scenario".into()),
        };
        if let Err(e) = &r {
            h.note(format!("RUN_FAILED {e}"));
        }
        h.dump()?;
        println!("run {tag}: {}", if r.is_ok() { "done".to_string() } else { format!("FAILED {}", r.err().map(|e| e.to_string()).unwrap_or_default()) });
        if let Some((s, _)) = relay {
            let _ = s.shutdown().await;
        }
        sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let argv: Vec<String> = std::env::args().collect();
    let args = Args::parse(&argv[2.min(argv.len())..]);
    let r = match argv.get(1).map(String::as_str) {
        Some("node") => node(args).await,
        Some("lock") => lock_role(args).await,
        Some("scen") => orchestrate(args).await,
        _ => Err("usage: node|lock|scen".into()),
    };
    if let Err(e) = r {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}
