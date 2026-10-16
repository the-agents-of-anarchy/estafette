use crate::*;
use iroh::{Endpoint, EndpointAddr, RelayMode, endpoint::presets::Minimal};
use std::time::Duration;
use tokio::time::{sleep, timeout};

pub trait Impl {
    const NAME: &'static str;
    fn serve(ep: Endpoint) -> iroh::protocol::Router;
    fn frame(req: &Request) -> Vec<u8>;
    fn client(ep: &Endpoint, addr: EndpointAddr) -> impl Future<Output = anyhow::Result<Box<dyn Caller>>>;
}
pub trait Caller { fn call<'a>(&'a self, r: &'a Request) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Response>> + Send + 'a>>; }

async fn ep() -> Endpoint {
    Endpoint::builder(Minimal).relay_mode(RelayMode::Disabled).bind().await.unwrap()
}
fn msg(n: usize) -> Request { Request::Message(Message { v: 1, id: [7; 16], body: "a".repeat(n), mac: [9; 32] }) }
fn probe() -> Request { Request::Probe(Probe { v: 1, nonce: [3; 16], mac: [4; 32] }) }

/// Raw client: write `bytes` in 16 KiB pieces; optional finish; observe; then probe on same connection.
async fn raw<I: Impl>(cli: &Endpoint, addr: &EndpointAddr, label: &str, bytes: &[u8], finish: bool, hold_ms: u64) {
    println!("- {label}: {} bytes, finish={finish}", bytes.len());
    let conn = cli.connect(addr.clone(), ALPN).await.unwrap();
    let base = reset();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    let mut w = Ok(());
    for c in bytes.chunks(16384) {
        match timeout(Duration::from_secs(5), tx.write_all(c)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => { w = Err(format!("{e}")); break; }
            Err(_) => { w = Err("write timeout 5s".into()); break; }
        }
    }
    println!("  client write: {w:?}");
    if finish { println!("  client finish: {:?}", tx.finish().map_err(|e| e.to_string())); }
    sleep(Duration::from_millis(hold_ms)).await;
    report(label, base);
    if finish {
        let r = timeout(Duration::from_secs(3), rx.read_to_end(1 << 22)).await;
        match r {
            Ok(Ok(b)) => println!("  client read: Ok({} bytes)", b.len()),
            Ok(Err(e)) => println!("  client read: Err({e})"),
            Err(_) => println!("  client read: timeout 3s (stream left open by server)"),
        }
    }
    sleep(Duration::from_millis(200)).await;
    println!("  connection after: {}", match conn.close_reason() { None => "alive".to_string(), Some(r) => format!("closed: {r}") });
    if conn.close_reason().is_none() {
        let f = I::frame(&probe());
        match conn.open_bi().await {
            Ok((mut t, mut r)) => {
                let ok = t.write_all(&f).await.is_ok() && t.finish().is_ok();
                let got = timeout(Duration::from_secs(3), r.read_to_end(1 << 20)).await;
                println!("  follow-up probe on same connection: sent={ok} reply={:?}", got.map(|x| x.map(|b| b.len()).map_err(|e| e.to_string())).map_err(|_| "timeout"));
            }
            Err(e) => println!("  follow-up open_bi: Err({e})"),
        }
    }
    conn.close(0u32.into(), b"done");
}

pub async fn run<I: Impl>() {
    println!("=== {} ===", I::NAME);
    let (srv, cli) = (ep().await, ep().await);
    let _router = I::serve(srv.clone());
    let addr = srv.addr();
    let c = I::client(&cli, addr.clone()).await.unwrap();

    println!("- S1 roundtrip, one call per variant, remote-id visible to handler");
    for r in [msg(10), probe(), Request::Introduce(Introduce { v: 1, public: vec![1; 50], mac: [5; 32] })] {
        let resp = c.call(&r).await;
        println!("  {:?}", resp.map(|x| match x { Response::Message(m) => format!("MessageAck accepted={} reason={:?}", m.accepted, m.reason), o => format!("{o:?}").chars().take(60).collect() }));
    }
    println!("  client node id: {}", cli.id());

    println!("- S2 real client, 2,000,000-byte body (call API)");
    let t = std::time::Instant::now();
    let r = c.call(&msg(2_000_000)).await;
    println!("  result after {:?}: {:?}", t.elapsed(), r.map(|_| "Ok(response)").map_err(|e| format!("{e:#}")));
    println!("  next call on same client: {:?}", c.call(&probe()).await.map(|_| "Ok").map_err(|e| format!("{e:#}")));

    let big = I::frame(&msg(2_000_000));
    raw::<I>(&cli, &addr, "S3 raw 2,000,000-byte body, finished", &big, true, 300).await;
    raw::<I>(&cli, &addr, "S4 raw first 100,000 bytes of the 2 MB request, never finished", &big[..100_000], false, 500).await;
    let hdr = postcard::to_stdvec(&(16_777_216u64)).unwrap();
    raw::<I>(&cli, &addr, "S5 raw 4-byte header declaring 16 MiB (irpc max), then nothing", &hdr, false, 500).await;
    let hdr = postcard::to_stdvec(&(20_000_000u64)).unwrap();
    raw::<I>(&cli, &addr, "S6 raw 4-byte header declaring 20,000,000 (> irpc max)", &hdr, false, 500).await;
    raw::<I>(&cli, &addr, "S7 raw malformed: 64 bytes of 0xFF, finished", &[0xFF; 64], true, 200).await;
    raw::<I>(&cli, &addr, "S8 raw malformed: valid frame of a Probe with 1 trailing byte? (truncated: first 10 bytes of valid probe frame), finished", &I::frame(&probe())[..10], true, 200).await;

    // S9: 20 connections each declaring 16 MiB, then idle.
    let rss = || String::from_utf8(std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap().stdout).unwrap().trim().to_string();
    let base = reset(); let rss0 = rss();
    let mut keep = vec![];
    for _ in 0..20 {
        let conn = cli.connect(addr.clone(), ALPN).await.unwrap();
        let (mut tx, rx) = conn.open_bi().await.unwrap();
        tx.write_all(&postcard::to_stdvec(&(16_777_216u64)).unwrap()).await.unwrap();
        keep.push((conn, tx, rx));
    }
    sleep(Duration::from_millis(800)).await;
    println!("- S9 20 connections, each 4-byte header declaring 16 MiB, idle");
    println!("  live_heap_delta={} bytes, rss_before={} KiB rss_after={} KiB", LIVE.load(std::sync::atomic::Ordering::Relaxed) - base, rss0, rss());
}
