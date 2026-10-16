use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, QuicTransportConfig, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{
    api::Event,
    net::{GOSSIP_ALPN, Gossip},
    proto::TopicId,
};
use n0_future::StreamExt;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::oneshot,
};

const ECHO_ALPN: &[u8] = b"e3/echo/0";
const CAP: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        while let Ok((mut tx, mut rx)) = conn.accept_bi().await {
            let mut buf = [0u8; 64];
            while let Ok(Some(n)) = rx.read(&mut buf).await {
                if tx.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn build(tuned: bool, lookup: &MemoryLookup) -> Result<Endpoint> {
    let mut b = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind_addr(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    if tuned {
        let cfg = QuicTransportConfig::builder()
            .keep_alive_interval(Duration::from_secs(5))
            .default_path_keep_alive_interval(Duration::from_secs(5))
            .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
            .default_path_max_idle_timeout(Duration::from_secs(10))
            .build();
        b = b.transport_config(cfg);
    }
    Ok(b.bind().await?)
}

fn topic() -> TopicId {
    TopicId::from_bytes([7u8; 32])
}

fn ms(d: Option<Duration>) -> String {
    d.map_or_else(|| "none".into(), |d| format!("{:.0}", d.as_secs_f64() * 1000.0))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let tuned = args.iter().any(|a| a == "--tuned");
    match args.get(1).map(String::as_str) {
        Some("victim") => victim(&args, tuned).await,
        Some("watcher") => watcher(tuned, args.iter().any(|a| a == "--clean")).await,
        _ => bail!("usage: e3-kill watcher|victim [--tuned] [--clean]"),
    }
}

async fn victim(args: &[String], tuned: bool) -> Result<()> {
    let wid: EndpointId = args.get(2).context("wid")?.parse()?;
    let waddr: SocketAddr = args.get(3).context("waddr")?.parse()?;
    let lookup = MemoryLookup::new();
    lookup.add_endpoint_info(EndpointAddr::new(wid).with_ip_addr(waddr));
    let ep = build(tuned, &lookup).await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let router = Router::builder(ep.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(ECHO_ALPN, Echo)
        .spawn();
    let sock = ep.bound_sockets().first().copied().context("no sock")?;
    let _topic = gossip.subscribe(topic(), vec![wid]).await?;
    println!("READY {} {}", ep.id(), sock);
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
        .recv()
        .await;
    gossip.shutdown().await?;
    router.shutdown().await?;
    Ok(())
}

async fn watcher(tuned: bool, clean: bool) -> Result<()> {
    let lookup = MemoryLookup::new();
    let ep = build(tuned, &lookup).await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let _router = Router::builder(ep.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();
    let wsock = ep.bound_sockets().first().copied().context("no sock")?;

    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("victim").arg(ep.id().to_string()).arg(wsock.to_string());
    if tuned {
        cmd.arg("--tuned");
    }
    let mut child = cmd.stdout(Stdio::piped()).kill_on_drop(true).spawn()?;
    let pid = child.id().context("pid")?;
    let mut lines = BufReader::new(child.stdout.take().context("stdout")?).lines();
    let line = lines.next_line().await?.context("no READY")?;
    let mut it = line.split_whitespace();
    it.next();
    let vid: EndpointId = it.next().context("vid")?.parse()?;
    let vsock: SocketAddr = it.next().context("vsock")?.parse()?;
    let vaddr = EndpointAddr::new(vid).with_ip_addr(vsock);
    lookup.add_endpoint_info(vaddr.clone());

    let mut topic = gossip.subscribe(topic(), vec![vid]).await?;
    // wait for NeighborUp(victim)
    loop {
        match tokio::time::timeout(Duration::from_secs(30), topic.next()).await {
            Ok(Some(Ok(Event::NeighborUp(id)))) if id == vid => break,
            Ok(Some(Ok(_))) => {}
            other => bail!("no NeighborUp: {other:?}"),
        }
    }
    // probe connection with an open bi-stream
    let conn = ep.connect(vaddr.clone(), ECHO_ALPN).await?;
    let (mut tx, mut rx) = conn.open_bi().await?;
    tx.write_all(b"ping").await?;
    let mut b4 = [0u8; 4];
    rx.read_exact(&mut b4).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // NeighborDown watcher task
    let (nd_tx, nd_rx) = oneshot::channel::<Instant>();
    let mut nd_tx = Some(nd_tx);
    let (_, mut receiver) = topic.split();
    tokio::spawn(async move {
        while let Some(ev) = receiver.next().await {
            if let Ok(Event::NeighborDown(id)) = ev
                && id == vid
                && let Some(t) = nd_tx.take()
            {
                let _ = t.send(Instant::now());
            }
        }
    });

    // kill
    let t0 = Instant::now();
    if clean {
        Command::new("kill").arg("-TERM").arg(pid.to_string()).status().await?;
    } else {
        child.start_kill()?;
    }

    let nd = async {
        let r = tokio::time::timeout(CAP, nd_rx).await;
        r.ok().and_then(Result::ok).map(|t| t.duration_since(t0))
    };
    let closed = async {
        let r = tokio::time::timeout(CAP, conn.closed()).await;
        r.ok().map(|e| (t0.elapsed(), format!("{e:?}").replace(' ', "")))
    };
    let fresh = async {
        let r = tokio::time::timeout(CAP, ep.connect(vaddr.clone(), ECHO_ALPN)).await;
        match r {
            Ok(Err(e)) => (Some(t0.elapsed()), format!("{e:?}").replace(' ', "")),
            Ok(Ok(_)) => (Some(t0.elapsed()), "CONNECTED".into()),
            Err(_) => (None, "cap".into()),
        }
    };
    let stream = async {
        let _ = tx.write_all(b"ping2").await;
        let r = tokio::time::timeout(CAP, rx.read_exact(&mut b4)).await;
        match r {
            Ok(Err(e)) => (Some(t0.elapsed()), format!("{e:?}").replace(' ', "")),
            Ok(Ok(())) => (Some(t0.elapsed()), "REPLIED".into()),
            Err(_) => (None, "cap".into()),
        }
    };
    let (nd, closed, fresh, stream) = tokio::join!(nd, closed, fresh, stream);
    let _ = child.wait().await;
    println!(
        "RESULT tuned={tuned} clean={clean} neighbor_down_ms={} conn_closed_ms={} conn_closed_err={} fresh_connect_fail_ms={} fresh_err={} stream_fail_ms={} stream_err={}",
        ms(nd),
        ms(closed.as_ref().map(|c| c.0)),
        closed.map_or_else(|| "none".into(), |c| c.1),
        ms(fresh.0),
        fresh.1,
        ms(stream.0),
        stream.1,
    );
    Ok(())
}
