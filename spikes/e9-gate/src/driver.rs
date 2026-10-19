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

/// (name, member gate mode, outsider gate: "none" = no gate code, "wrong" = gate with a random key)
const SCNS: [(&str, &str, &str); 6] = [
    ("none", "none", "none"),
    ("hook-plain", "hook", "none"),
    ("hook-wrong", "hook", "wrong"),
    ("wrapper-plain", "wrapper", "none"),
    ("wrapper-wrong", "wrapper", "wrong"),
    ("wrapper-liar", "wrapper", "lax"),
];

async fn run_one(name: &str, gate: &str, ogate: &str, run: usize) -> Result<M> {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let secret = rnd();
    let topic = hex(derive("messaging topic v1", &secret));
    let secret_hex = hex(secret);
    let okey = rnd();
    let mkeys: Vec<[u8; 32]> = (0..4).map(|_| rnd()).collect();
    let mem_args = |i: usize| -> Vec<String> {
        ["--secret-hex", &secret_hex, "--gate", gate, "--key-hex", &hex(mkeys[i])].iter().map(|s| (*s).to_string()).collect()
    };
    let mut ms = Vec::new();
    for (i, n) in ["M1", "M2", "M3"].into_iter().enumerate() {
        ms.push(spawn(n, &mem_args(i), &log).await?);
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

    // outsider: has the topic id, M1's address, a fresh identity, not the secret
    let mut oargs: Vec<String> = ["--topic-hex", &topic, "--key-hex", &hex(okey), "--gate", match ogate { "wrong" => gate, "lax" => "wrapper-lax", _ => "none" }]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    if ogate != "none" {
        oargs.extend(["--gate-key-hex".to_string(), hex(rnd())]);
    }
    let mut o = spawn("O", &oargs, &log).await?;
    o.send(&format!("ADDR M1 {} {}", infos[0].1, infos[0].2)).await?;
    for (n, id, _) in &infos[1..] {
        o.send(&format!("NAME {n} {id}")).await?;
    }
    for m in &mut ms {
        m.send(&format!("ADDR O {} {}", o.id, o.sock)).await?;
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

    o.send("FORGE 10").await?;
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // restart M3 with the same identity: same key, new port, rebootstrap on M1
    let _ = ms[2].child.start_kill();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut m3r = spawn("M3r", &mem_args(2), &log).await?;
    for (n, id, sock) in &infos[..2] {
        m3r.send(&format!("ADDR {n} {id} {sock}")).await?;
    }
    for m in &mut ms[..2] {
        m.send(&format!("ADDR M3 {} {}", infos[2].1, m3r.sock)).await?;
    }
    m3r.send(&format!("START {m1_id}")).await?;
    let restarted = wait_for(&log, "M3r", "NEIGHBOR_UP", 10).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    ms[0].send("PING 200 10").await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // dial-out: member M4 bootstraps on the outsider; gossip itself dials O
    let mut m4 = spawn("M4", &mem_args(3), &log).await?;
    m4.send(&format!("ADDR O {} {}", o.id, o.sock)).await?;
    o.send(&format!("NAME M4 {}", m4.id)).await?;
    m4.send(&format!("START {}", o.id)).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    m4.send("ANNOUNCE").await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    for m in ms.iter_mut().chain([&mut m3r, &mut m4, &mut o]) {
        let _ = m.send("QUIT").await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for m in ms.iter_mut().chain([&mut m3r, &mut m4, &mut o]) {
        let _ = m.child.start_kill();
    }

    let g = log.lock().map_err(|e| anyhow::anyhow!("{e}"))?.clone();
    std::fs::create_dir_all("logs")?;
    let dump: String = g.iter().map(|(n, ts, t)| format!("{ts} {n} {t}\n")).collect();
    std::fs::write(format!("logs/{name}-{run}.log", ), dump)?;

    let mut m = M::new();
    let cnt = |who: &str, pat: &str| -> f64 { g.iter().filter(|(n, _, t)| n == who && t.contains(pat)).count() as f64 };
    let ts_of = |who: &str, pat: &str| g.iter().filter(|(n, _, t)| n == who && t.starts_with(pat)).map(|x| x.1).min();
    let mem = ["M1", "M2", "M3"];
    let ms_between = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(a, b)| b.saturating_sub(a) as f64 / 1000.0);
    m.insert("members_formed".into(), if formed { 1.0 } else { 0.0 });
    // time to joined: first member START to the last member's first NEIGHBOR_UP
    let start = mem.iter().filter_map(|n| ts_of(n, "START")).min();
    let last_up = mem.iter().map(|n| ts_of(n, "NEIGHBOR_UP")).collect::<Option<Vec<_>>>().and_then(|v| v.into_iter().max());
    if let Some(d) = ms_between(start, last_up) {
        m.insert("join_all_ms".into(), d);
    }
    // presence: each announcer's TX to each other member's RX (ok MAC); 6 pairs expected
    let mut pair_ms = Vec::new();
    for a in mem {
        let tx = ts_of(a, "TX kind=presence");
        for r in mem.iter().filter(|r| **r != a) {
            let rx = g.iter().filter(|(n, _, t)| n == r && t.starts_with("RX kind=presence") && t.contains("mac=ok") && field(t, "origin=") == Some(a)).map(|x| x.1).min();
            if let Some(d) = ms_between(tx, rx) {
                pair_ms.push(d);
            }
        }
    }
    m.insert("presence_pairs_of6".into(), pair_ms.len() as f64);
    m.insert("presence_all_ms".into(), pair_ms.iter().copied().fold(0.0, f64::max));
    // outsider: everything it gets
    m.insert("o_subscribe_ok".into(), cnt("O", "SUBSCRIBE ok"));
    m.insert("o_neighbor_up_M".into(), g.iter().filter(|(n, _, t)| n == "O" && t.starts_with("NEIGHBOR_UP") && !t.contains("M4")).count() as f64);
    m.insert("o_rx_from_members".into(), g.iter().filter(|(n, _, t)| n == "O" && t.starts_with("RX") && field(t, "origin=").is_some_and(|o| o != "M4" && o != "O")).count() as f64);
    m.insert("o_events_other".into(), g.iter().filter(|(n, _, t)| n == "O" && !(t == "START" || t.starts_with("SUBSCRIBE ok") || t.starts_with("TX") || t.starts_with("FORGE_DONE") || t.starts_with("RX") || t.starts_with("NEIGHBOR_UP") || t.starts_with("GATE_"))).count() as f64);
    // forgeries
    m.insert("forged_sent".into(), cnt("O", "TX kind=forged"));
    let forged = |who: &[&str]| g.iter().filter(|(n, _, t)| who.contains(&n.as_str()) && t.starts_with("RX kind=presence") && field(t, "origin=") == Some("O")).count() as f64;
    m.insert("forged_rx_members".into(), forged(&["M1", "M2", "M3", "M3r"]));
    m.insert("members_neighbor_up_O".into(), mem.iter().chain(&["M3r"]).map(|n| cnt(n, "NEIGHBOR_UP subject=O")).sum());
    m.insert("members_rx_any_from_O".into(), g.iter().filter(|(n, _, t)| ["M1", "M2", "M3"].contains(&n.as_str()) && t.starts_with("RX") && field(t, "origin=") == Some("O")).count() as f64);
    m.insert("gate_fail_at_members".into(), mem.iter().map(|n| cnt(n, "GATE_FAIL")).sum());
    m.insert("gate_fail_at_O".into(), cnt("O", "GATE_FAIL"));
    // member-to-member delivery unaffected by O
    m.insert("deliv_with_O_of40".into(), g.iter().filter(|(n, _, t)| ["M2", "M3"].contains(&n.as_str()) && t.starts_with("RX kind=ping") && t.contains("mac=ok") && field(t, "seq=").and_then(|s| s.parse::<u64>().ok()).is_some_and(|s| (100..120).contains(&s))).count() as f64);
    // restart
    m.insert("restart_joined".into(), if restarted { 1.0 } else { 0.0 });
    if let Some(d) = ms_between(ts_of("M3r", "START"), ts_of("M3r", "NEIGHBOR_UP")) {
        m.insert("restart_join_ms".into(), d);
    }
    m.insert("restart_pings_of10".into(), g.iter().filter(|(n, _, t)| n == "M3r" && t.starts_with("RX kind=ping") && t.contains("mac=ok") && field(t, "seq=").and_then(|s| s.parse::<u64>().ok()).is_some_and(|s| (200..210).contains(&s))).count() as f64);
    // dial-out by a member toward the outsider
    m.insert("m4_neighbor_up".into(), cnt("M4", "NEIGHBOR_UP"));
    m.insert("o_neighbor_up_M4".into(), cnt("O", "NEIGHBOR_UP subject=M4"));
    m.insert("o_rx_presence_from_M4".into(), g.iter().filter(|(n, _, t)| n == "O" && t.starts_with("RX kind=presence") && field(t, "origin=") == Some("M4")).count() as f64);
    m.insert("gate_fail_at_M4".into(), cnt("M4", "GATE_FAIL"));
    // handshake cost
    let gate_ms: Vec<f64> = g.iter().filter(|(n, _, t)| ["M1", "M2", "M3", "M3r"].contains(&n.as_str()) && t.starts_with("GATE_OK")).filter_map(|(_, _, t)| field(t, "ms=")?.parse().ok()).collect();
    m.insert("gate_ok_events".into(), gate_ms.len() as f64);
    let mut gm = gate_ms.clone();
    if let Some(v) = median(&mut gm) {
        m.insert("gate_ok_median_ms".into(), v);
        m.insert("gate_ok_max_ms".into(), gm.last().copied().unwrap_or(f64::NAN));
        m.insert("gate_ok_min_ms".into(), gm.first().copied().unwrap_or(f64::NAN));
    }
    let _ = now_us();
    Ok(m)
}

pub async fn run(args: &[String]) -> Result<()> {
    let which = args.get(2).map(String::as_str);
    let runs: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    for (name, gate, ogate) in SCNS {
        if which.is_some_and(|w| w != "all" && w != name) {
            continue;
        }
        let mut all: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for r in 0..runs {
            match run_one(name, gate, ogate, r).await {
                Ok(m) => {
                    println!("RUN {name} {r} {m:?}");
                    for (k, v) in m {
                        all.entry(k).or_default().push(v);
                    }
                }
                Err(e) => println!("RUN {name} {r} FAILED {e:#}"),
            }
        }
        println!("| scenario | measure | min | median | max | n |");
        for (k, mut v) in all {
            let n = v.len();
            let md = median(&mut v).unwrap_or(f64::NAN);
            println!("| {name} | {k} | {:.3} | {md:.3} | {:.3} | {n} |", v.first().copied().unwrap_or(f64::NAN), v.last().copied().unwrap_or(f64::NAN));
        }
    }
    Ok(())
}
