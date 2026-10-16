use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use iroh::SecretKey;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
};

use crate::node::{derive, now_us};

type Log = Arc<Mutex<Vec<(String, u64, String)>>>;

struct Proc {
    name: String,
    child: Child,
    stdin: ChildStdin,
    id: String,
    sock: String,
}

fn hex(b: [u8; 32]) -> String {
    blake3::Hash::from_bytes(b).to_hex().to_string()
}

fn rnd() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

async fn spawn(name: &str, extra: &[String], log: &Log) -> Result<Proc> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .args(["node", "--name", name])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().context("stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("stdout")?).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .context("READY timeout")??
        .context("eof before READY")?;
    let p: Vec<&str> = first.split_whitespace().collect();
    let (Some("READY"), Some(id), Some(sock)) = (p.first().copied(), p.get(1), p.get(2)) else {
        bail!("bad READY line: {first}");
    };
    let (id, sock) = ((*id).to_string(), (*sock).to_string());
    let (log, nm) = (log.clone(), name.to_string());
    tokio::spawn(async move {
        while let Ok(Some(l)) = lines.next_line().await {
            let mut it = l.splitn(4, ' ');
            if let (Some("EV"), Some(ts), Some(_), Some(text)) = (it.next(), it.next(), it.next(), it.next())
                && let Ok(ts) = ts.parse::<u64>()
                && let Ok(mut g) = log.lock()
            {
                g.push((nm.clone(), ts, text.to_string()));
            }
        }
    });
    Ok(Proc { name: name.to_string(), child, stdin, id, sock })
}

impl Proc {
    async fn send(&mut self, line: &str) -> Result<()> {
        self.stdin.write_all(format!("{line}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

fn count(log: &Log, who: &str, pat: &str) -> usize {
    log.lock().map_or(0, |g| g.iter().filter(|(n, _, t)| n == who && t.contains(pat)).count())
}

async fn wait_for(log: &Log, who: &str, pat: &str, secs: u64) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if count(log, who, pat) > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace().find_map(|w| w.strip_prefix(key))
}

fn median(v: &mut [f64]) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        None
    } else {
        v.get(n / 2).copied()
    }
}

type M = BTreeMap<String, f64>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scn {
    /// Outsider joins; members hold the outsider's address (as discovery would give them).
    Open,
    /// Outsider joins; members do not hold the outsider's address.
    Closed,
    /// As Open, but members install an after_handshake hook denying the outsider's id on the gossip ALPN.
    Deny,
    /// Secret-holder using a wrong topic id.
    Control,
}

impl Scn {
    fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Deny => "deny",
            Self::Control => "control",
        }
    }
}

async fn run_one(scn: Scn, run: usize) -> Result<M> {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let secret = rnd();
    let topic = hex(derive("messaging topic v1", &secret));
    let secret_hex = hex(secret);
    let okey = rnd();
    let o_id = SecretKey::from_bytes(&okey).public().to_string();
    let mut mem_args = vec!["--secret-hex".to_string(), secret_hex.clone()];
    if scn == Scn::Deny {
        mem_args.extend(["--deny".to_string(), o_id.clone()]);
    }
    let mut ms = Vec::new();
    for n in ["M1", "M2", "M3"] {
        ms.push(spawn(n, &mem_args, &log).await?);
    }
    let infos: Vec<(String, String, String)> = ms.iter().map(|m| (m.name.clone(), m.id.clone(), m.sock.clone())).collect();
    for m in &mut ms {
        for (n, id, sock) in &infos {
            if *n != m.name {
                m.send(&format!("ADDR {n} {id} {sock}")).await?;
            }
        }
    }
    let m1_id = infos[0].1.clone();
    ms[0].send("START -").await?;
    ms[1].send(&format!("START {m1_id}")).await?;
    ms[2].send(&format!("START {m1_id}")).await?;
    let mut formed = true;
    for n in ["M1", "M2", "M3"] {
        formed &= wait_for(&log, n, "NEIGHBOR_UP", 10).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;

    ms[0].send("PING 0 20").await?;
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // outsider (or wrong-topic member)
    let (oname, oargs) = match scn {
        Scn::Control => {
            let wrong = hex(derive("messaging topic v1", &rnd()));
            ("W", vec!["--secret-hex".to_string(), secret_hex.clone(), "--topic-hex".to_string(), wrong])
        }
        _ => ("O", vec!["--topic-hex".to_string(), topic.clone(), "--key-hex".to_string(), hex(okey)]),
    };
    let mut o = spawn(oname, &oargs, &log).await?;
    o.send(&format!("ADDR M1 {} {}", infos[0].1, infos[0].2)).await?;
    for (n, id, _) in &infos[1..] {
        o.send(&format!("NAME {n} {id}")).await?;
    }
    for m in &mut ms {
        if scn == Scn::Closed {
            m.send(&format!("NAME {oname} {}", o.id)).await?;
        } else {
            m.send(&format!("ADDR {oname} {} {}", o.id, o.sock)).await?;
        }
    }
    o.send(&format!("START {m1_id}")).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;

    for m in &mut ms {
        m.send("ANNOUNCE").await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    ms[0].send("PING 100 20").await?;
    tokio::time::sleep(Duration::from_millis(2500)).await;

    if scn == Scn::Control {
        o.send("PING 2000 5").await?;
    } else {
        o.send("FORGE 10").await?;
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;

    for m in &mut ms {
        m.send("QUIT").await?;
    }
    o.send("QUIT").await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    for m in &mut ms {
        let _ = m.child.start_kill();
    }
    let _ = o.child.start_kill();

    let g = log.lock().map_err(|e| anyhow::anyhow!("{e}"))?.clone();
    std::fs::create_dir_all("logs")?;
    let dump: String = g.iter().map(|(n, ts, t)| format!("{ts} {n} {t}\n")).collect();
    std::fs::write(format!("logs/{}-{run}.log", scn.name()), dump)?;

    let mut m = M::new();
    let f = |b: bool| if b { 1.0 } else { 0.0 };
    let cnt = |who: &str, pat: &str| -> f64 { g.iter().filter(|(n, _, t)| n == who && t.contains(pat)).count() as f64 };
    let mem = ["M1", "M2", "M3"];
    m.insert("members_formed".into(), f(formed));
    m.insert("o_subscribe_ok".into(), cnt(oname, "SUBSCRIBE ok"));
    m.insert("o_neighbor_up_n".into(), cnt(oname, "NEIGHBOR_UP"));
    let o_start = g.iter().find(|(n, _, t)| n == oname && t == "START").map(|x| x.1);
    let o_up = g.iter().find(|(n, _, t)| n == oname && t.starts_with("NEIGHBOR_UP")).map(|x| x.1);
    if let (Some(s), Some(u)) = (o_start, o_up) {
        m.insert("o_neighbor_up_ms".into(), (u.saturating_sub(s)) as f64 / 1000.0);
    }
    let up_subj: Vec<String> = g
        .iter()
        .filter(|(n, _, t)| n == oname && t.starts_with("NEIGHBOR_UP"))
        .filter_map(|(_, _, t)| field(t, "subject=").map(str::to_string))
        .collect();
    m.insert("o_neighbors_M2_or_M3".into(), up_subj.iter().filter(|s| *s != "M1").count() as f64);
    // presence readable at the outsider
    let o_pres: Vec<&(String, u64, String)> =
        g.iter().filter(|(n, _, t)| n == oname && t.starts_with("RX kind=presence") && field(t, "origin=").is_some_and(|o| o.starts_with('M'))).collect();
    let mut origins: Vec<&str> = o_pres.iter().filter_map(|(_, _, t)| field(t, "origin=")).collect();
    origins.sort_unstable();
    origins.dedup();
    m.insert("o_presence_distinct".into(), origins.len() as f64);
    let first_tx = g.iter().filter(|(n, _, t)| mem.contains(&n.as_str()) && t == "TX kind=presence").map(|x| x.1).min();
    if let (Some(tx), Some(rx)) = (first_tx, o_pres.iter().map(|x| x.1).min()) {
        m.insert("o_first_presence_ms".into(), rx.saturating_sub(tx) as f64 / 1000.0);
    }
    m.insert(
        "o_ping_rx_of20".into(),
        g.iter().filter(|(n, _, t)| n == oname && t.starts_with("RX kind=ping") && field(t, "seq=").and_then(|s| s.parse::<u64>().ok()).is_some_and(|s| (100..120).contains(&s))).count() as f64,
    );
    // forged payloads at members, before the MAC decision
    let forged: Vec<&(String, u64, String)> = g
        .iter()
        .filter(|(n, _, t)| mem.contains(&n.as_str()) && t.starts_with("RX kind=presence") && field(t, "origin=") == Some(oname) && field(t, "seq=").and_then(|s| s.parse::<u64>().ok()).is_some_and(|s| s >= 1000))
        .collect();
    m.insert("forged_sent".into(), cnt(oname, "TX kind=forged"));
    m.insert("forged_rx_total".into(), forged.len() as f64);
    m.insert("forged_mac_ok".into(), forged.iter().filter(|x| x.2.contains("mac=ok")).count() as f64);
    m.insert("forged_mac_bad".into(), forged.iter().filter(|x| x.2.contains("mac=bad")).count() as f64);
    m.insert("forged_from_O_direct".into(), forged.iter().filter(|x| field(&x.2, "from=") == Some(oname)).count() as f64);
    m.insert("forged_relayed_by_member".into(), forged.iter().filter(|x| field(&x.2, "from=") != Some(oname)).count() as f64);
    for n in mem {
        m.insert(format!("forged_rx_{n}"), forged.iter().filter(|x| x.0 == n).count() as f64);
    }
    // member-side topology events
    m.insert("members_neighbor_up_O".into(), mem.iter().map(|n| cnt(n, &format!("NEIGHBOR_UP subject={oname}"))).sum());
    let o_t = o_start.unwrap_or(0);
    let down_after: usize = g.iter().filter(|(n, ts, t)| mem.contains(&n.as_str()) && *ts >= o_t && t.starts_with("NEIGHBOR_DOWN")).count();
    let up_after: usize = g.iter().filter(|(n, ts, t)| mem.contains(&n.as_str()) && *ts >= o_t && t.starts_with("NEIGHBOR_UP") && !t.contains(&format!("subject={oname}"))).count();
    m.insert("members_neighbor_down_after_O".into(), down_after as f64);
    m.insert("members_neighbor_up_other_after_O".into(), up_after as f64);
    m.insert("members_neighbor_down_before_O".into(), g.iter().filter(|(n, ts, t)| mem.contains(&n.as_str()) && *ts < o_t && t.starts_with("NEIGHBOR_DOWN")).count() as f64);
    m.insert("hook_rejects".into(), mem.iter().map(|n| cnt(n, "HOOK_REJECT")).sum());
    // member-to-member delivery: M1 pings -> M2, M3
    for (label, lo, hi) in [("base", 0u64, 20u64), ("withO", 100, 120)] {
        let mut lat = Vec::new();
        for r in ["M2", "M3"] {
            for (_, ts, t) in g.iter().filter(|(n, _, t)| n == r && t.starts_with("RX kind=ping") && t.contains("mac=ok")) {
                let seq = field(t, "seq=").and_then(|s| s.parse::<u64>().ok());
                let sent = field(t, "sent_us=").and_then(|s| s.parse::<u64>().ok());
                if let (Some(sq), Some(se)) = (seq, sent)
                    && (lo..hi).contains(&sq)
                {
                    lat.push(ts.saturating_sub(se) as f64 / 1000.0);
                }
            }
        }
        m.insert(format!("deliv_{label}_of40"), lat.len() as f64);
        if let Some(md) = median(&mut lat) {
            m.insert(format!("lat_{label}_median_ms"), md);
        }
    }
    // control: did the other side see anything
    if scn == Scn::Control {
        m.insert("members_rx_from_W".into(), g.iter().filter(|(n, _, t)| mem.contains(&n.as_str()) && t.starts_with("RX") && field(t, "origin=") == Some("W")).count() as f64);
        m.insert("w_rx_total".into(), g.iter().filter(|(n, _, t)| n == "W" && t.starts_with("RX")).count() as f64);
    }
    let _ = now_us();
    Ok(m)
}

pub async fn run(args: &[String]) -> Result<()> {
    let which = args.get(2).map(String::as_str);
    let runs: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    for scn in [Scn::Open, Scn::Closed, Scn::Deny, Scn::Control] {
        if which.is_some_and(|w| w != "all" && w != scn.name()) {
            continue;
        }
        let mut all: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for r in 0..runs {
            match run_one(scn, r).await {
                Ok(m) => {
                    println!("RUN {} {r} {m:?}", scn.name());
                    for (k, v) in m {
                        all.entry(k).or_default().push(v);
                    }
                }
                Err(e) => println!("RUN {} {r} FAILED {e:#}", scn.name()),
            }
        }
        println!("| scenario | measure | min | median | max | n |");
        for (k, mut v) in all {
            let n = v.len();
            let md = median(&mut v).unwrap_or(f64::NAN);
            println!(
                "| {} | {k} | {:.3} | {md:.3} | {:.3} | {n} |",
                scn.name(),
                v.first().copied().unwrap_or(f64::NAN),
                v.last().copied().unwrap_or(f64::NAN)
            );
        }
    }
    Ok(())
}
