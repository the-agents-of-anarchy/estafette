mod types;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use iroh::{
    Endpoint, RelayMode,
    address_lookup::memory::MemoryLookup,
    endpoint::presets,
    protocol::Router,
};
use iroh_gossip::{ALPN, api::Event, net::Gossip, proto::TopicId};
use n0_future::StreamExt;
use types::*;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

struct Outcome {
    received: bool,
    joined: bool,
    sender: String,
    pre: String,
}

async fn run_once(max: usize, payload: &Bytes, logs: &Buf) -> anyhow::Result<Outcome> {
    logs.0.lock().unwrap().clear();
    let mk = || async {
        let ep = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        anyhow::Ok(ep)
    };
    let (ea, eb) = (mk().await?, mk().await?);
    // direct addresses exchanged
    let (la, lb) = (MemoryLookup::new(), MemoryLookup::new());
    la.add_endpoint_info(eb.addr());
    lb.add_endpoint_info(ea.addr());
    ea.address_lookup()?.add(la);
    eb.address_lookup()?.add(lb);
    let ga = Gossip::builder().max_message_size(max).spawn(ea.clone());
    let gb = Gossip::builder().max_message_size(max).spawn(eb.clone());
    let ra = Router::builder(ea.clone()).accept(ALPN, ga.clone()).spawn();
    let rb = Router::builder(eb.clone()).accept(ALPN, gb.clone()).spawn();
    let topic = TopicId::from_bytes([7u8; 32]);

    let mut ta = ga.subscribe(topic, vec![eb.id()]).await?;
    let mut tb = gb.subscribe(topic, vec![ea.id()]).await?;
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        ta.joined().await?;
        tb.joined().await?;
        anyhow::Ok(())
    })
    .await
    .is_ok_and(|r| r.is_ok());
    let (sa, mut ra_rx) = ta.split();
    let (_sb, mut rb_rx) = tb.split();

    let sender = match sa.broadcast(payload.clone()).await {
        Ok(()) => "broadcast() Ok".to_string(),
        Err(e) => format!("broadcast() Err: {e}"),
    };
    // observe both sides for 4s
    let mut received = false;
    let mut a_events = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(4));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            ev = rb_rx.next() => match ev {
                Some(Ok(Event::Received(m))) => { received = m.content == *payload; if received { break; } }
                Some(Ok(_)) => {}
                Some(Err(e)) => { a_events.push(format!("B stream err: {e}")); break; }
                None => { a_events.push("B stream ended".into()); break; }
            },
            ev = ra_rx.next() => match ev {
                Some(Ok(Event::NeighborDown(_))) => a_events.push("A: NeighborDown".into()),
                Some(Ok(Event::Lagged)) => a_events.push("A: Lagged".into()),
                Some(Ok(_)) => {}
                Some(Err(e)) => a_events.push(format!("A stream err: {e}")),
                None => { a_events.push("A stream ended".into()); }
            },
        }
    }
    let mut sender = sender;
    if !received && std::env::var("FOLLOW").is_ok() {
        let small = Bytes::from(vec![9u8; 100]);
        let r = sa.broadcast(small.clone()).await;
        let mut got = false;
        let dl = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(dl);
        loop {
            tokio::select! {
                _ = &mut dl => break,
                ev = rb_rx.next() => if let Some(Ok(Event::Received(m))) = ev { if m.content == small { got = true; break; } } else if ev.is_none() { break; },
            }
        }
        sender.push_str(&format!("; follow-up 100B broadcast: send={} received={}", if r.is_ok() {"Ok"} else {"Err"}, got));
    }
    if !a_events.is_empty() { sender.push_str(&format!("; {}", a_events.join(", "))); }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pre = log_summary(logs);
    if std::env::var("DUMP").is_ok() { eprintln!("=== max={max} full pre-shutdown log\n{}", strip_ansi(&String::from_utf8_lossy(&logs.0.lock().unwrap()))); }
    drop((sa, ra_rx, rb_rx, _sb));
    ra.shutdown().await.ok();
    rb.shutdown().await.ok();
    Ok(Outcome { received, joined, sender, pre })
}

fn log_summary(logs: &Buf) -> String {
    let s = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
    let mut keep: Vec<String> = Vec::new();
    for l in s.lines() {
        let l = strip_ansi(l);
        if (l.contains("WARN") || l.contains("ERROR") || l.contains("TooLarge") || l.contains("arger") || l.contains("with error") || l.contains("failed"))
            && !keep.iter().any(|k| k == &l)
        {
            keep.push(l);
        }
    }
    keep.iter().take(4).map(|l| l.chars().take(220).collect::<String>()).collect::<Vec<_>>().join(" || ")
}
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' { for d in it.by_ref() { if d.is_ascii_alphabetic() { break; } } } else { out.push(c); }
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logs = Buf::default();
    let l2 = logs.clone();
    tracing_subscriber::fmt()
        .with_env_filter("warn,iroh_gossip=debug")
        .with_writer(move || l2.clone())
        .init();

    let enc = |n: &str, b: Vec<u8>| println!("| {n} | {} |", b.len());
    println!("## sizes");
    println!("| Structure | Bytes |\n|---|---|");
    enc("Claim", postcard::to_allocvec(&claim_max())?);
    enc("Presence", postcard::to_allocvec(&presence_max())?);
    enc("Departure", postcard::to_allocvec(&departure_max())?);
    enc("Unreachable", postcard::to_allocvec(&unreachable_max())?);
    let pl = postcard::to_allocvec(&Payload::Presence(presence_max()))?;
    println!("| Payload::Presence (enum, 1-byte tag) | {} |", pl.len());
    println!("| Payload::Departure | {} |", postcard::to_allocvec(&Payload::Departure(departure_max()))?.len());
    println!("| Payload::Unreachable | {} |", postcard::to_allocvec(&Payload::Unreachable(unreachable_max()))?.len());
    println!("handle len {} effective len {}", MAX_HANDLE.len(), effective_max().len());
    // roundtrip
    let back: Payload = postcard::from_bytes(&pl)?;
    assert!(matches!(back, Payload::Presence(ref p) if p.public.len() == 4096 && p.handle == effective_max()));

    // The measured payload: bare Presence per task ("worst-case Presence"); Payload enum adds 1 tag byte.
    let which = std::env::args().nth(1).unwrap_or_else(|| "bare".into());
    let bytes = if which == "enum" { pl.clone() } else { postcard::to_allocvec(&presence_max())? };
    let p = bytes.len();
    println!("measuring payload kind={which} size={p}");
    let payload = Bytes::from(bytes);

    let try_max = |m: usize| {
        let payload = payload.clone();
        let logs = logs.clone();
        async move {
            let o = run_once(m, &payload, &logs).await?;
            println!("| {m} | {} | {} (joined={}) | {} |", if o.received { "yes" } else { "no" }, o.sender, o.joined, o.pre);
            anyhow::Ok(o.received)
        }
    };
    println!("## runs\n| max_message_size | Received | Sender observed | Log lines (warn/err/too-large) |\n|---|---|---|---|");
    if std::env::var("QUICK").is_ok() { for m in if let Ok(v)=std::env::var("ONLY") { vec![v.parse().unwrap()] } else { vec![4096usize, p-1, p, 4400, 4406, 4407, p+39, p+38] } { try_max(m).await?; } return Ok(()); }
    try_max(4096).await?;
    try_max(p).await?;
    for k in [1usize, 2, 4, 8, 16, 24, 32, 40, 48, 56, 64] {
        try_max(p + k).await?;
    }
    // bisect on [p, p+hi]
    let (mut lo, mut hi) = (p, p + 64);
    if !try_max(hi).await? { anyhow::bail!("no success by p+64; widen"); }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if try_max(mid).await? { hi = mid } else { lo = mid }
    }
    println!("THRESHOLD(single-trial bisect)={hi} overhead={}", hi - p);
    println!("## repeats at threshold-1 and threshold (5x)");
    for _ in 0..5 { try_max(hi - 1).await?; }
    for _ in 0..5 { try_max(hi).await?; }
    Ok(())
}
