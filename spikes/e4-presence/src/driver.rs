use std::{
    collections::{BTreeMap, HashMap},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
};

use crate::node::now_us;

#[derive(Clone)]
struct Ev {
    us: u64,
    node: String,
    text: String,
}
type Evs = Arc<Mutex<Vec<Ev>>>;

#[derive(Clone)]
struct Flags {
    s12: bool,
    max_age: u64,
    init_delay: u64,
    sleep: u64,
}
impl Flags {
    fn args(&self) -> Vec<String> {
        let mut a = vec![
            "--max-age".to_string(),
            self.max_age.to_string(),
            "--init-delay".to_string(),
            self.init_delay.to_string(),
        ];
        if !self.s12 {
            a.push("--no-s12".into());
        }
        a.push("--depart-sleep".into());
        a.push(self.sleep.to_string());
        a
    }
}

struct N {
    name: String,
    id: String,
    sock: String,
    proc: Child,
    stdin: ChildStdin,
    alive: bool,
}

struct Cl {
    nodes: Vec<N>,
    evs: Evs,
    flags: Flags,
}

fn kv<'a>(t: &'a str, k: &str) -> Option<&'a str> {
    let p = format!("{k}=");
    t.split_whitespace().find_map(|w| w.strip_prefix(p.as_str()))
}
fn head(t: &str) -> &str {
    t.split_whitespace().next().unwrap_or("")
}
fn ms(t0: u64, t: u64) -> f64 {
    (t as f64 - t0 as f64) / 1000.0
}
async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

impl Cl {
    fn new(flags: Flags) -> Self {
        Self { nodes: Vec::new(), evs: Arc::new(Mutex::new(Vec::new())), flags }
    }
    fn snap(&self) -> Vec<Ev> {
        self.evs.lock().unwrap().clone()
    }
    fn idx(&self, name: &str) -> usize {
        self.nodes.iter().position(|n| n.name == name).unwrap()
    }
    async fn spawn(&mut self, name: &str) -> Result<()> {
        let mut proc = Command::new(std::env::current_exe()?)
            .args(["node", "--name", name])
            .args(self.flags.args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = proc.stdin.take().context("stdin")?;
        let mut lines = BufReader::new(proc.stdout.take().context("stdout")?).lines();
        let ready = lines.next_line().await?.context("no READY")?;
        let mut it = ready.split_whitespace().skip(1);
        let id = it.next().context("id")?.to_string();
        let sock = it.next().context("sock")?.to_string();
        let evs = self.evs.clone();
        tokio::spawn(async move {
            while let Ok(Some(l)) = lines.next_line().await {
                let mut p = l.splitn(4, ' ');
                if p.next() != Some("EV") {
                    continue;
                }
                if let (Some(us), Some(node), Some(text)) = (p.next(), p.next(), p.next())
                    && let Ok(us) = us.parse()
                {
                    if std::env::var_os("E4_DEBUG").is_some() {
                        eprintln!("    {us} {node} {text}");
                    }
                    evs.lock().unwrap().push(Ev { us, node: node.into(), text: text.into() });
                }
            }
        });
        self.nodes.push(N { name: name.into(), id, sock, proc, stdin, alive: true });
        Ok(())
    }
    async fn send(&mut self, name: &str, line: &str) -> Result<()> {
        let i = self.idx(name);
        self.nodes[i].stdin.write_all(format!("{line}\n").as_bytes()).await?;
        self.nodes[i].stdin.flush().await?;
        Ok(())
    }
    async fn exchange(&mut self) -> Result<()> {
        let all: Vec<(String, String, String)> = self
            .nodes
            .iter()
            .filter(|n| n.alive)
            .map(|n| (n.name.clone(), n.id.clone(), n.sock.clone()))
            .collect();
        for (to, ..) in &all {
            for (name, id, sock) in &all {
                if name != to {
                    self.send(to, &format!("ADDR {name} {id} {sock}")).await?;
                }
            }
        }
        Ok(())
    }
    async fn start(&mut self, name: &str, boots: &[&str]) -> Result<()> {
        let ids: Vec<String> = boots.iter().map(|b| self.nodes[self.idx(b)].id.clone()).collect();
        let arg = if ids.is_empty() { "-".to_string() } else { ids.join(",") };
        self.send(name, &format!("START {arg}")).await
    }
    async fn wait<F: Fn(&[Ev]) -> bool>(&self, cap_ms: u64, f: F) -> bool {
        let t = std::time::Instant::now();
        while t.elapsed() < Duration::from_millis(cap_ms) {
            if f(&self.evs.lock().unwrap()) {
                return true;
            }
            sleep(20).await;
        }
        false
    }
    async fn kill(&mut self, name: &str) -> Result<()> {
        let i = self.idx(name);
        self.nodes[i].alive = false;
        self.nodes[i].proc.start_kill()?;
        Ok(())
    }
    async fn dump(&mut self, names: &[&str]) -> Result<HashMap<String, HashMap<String, String>>> {
        let count = |evs: &[Ev], n: &str| evs.iter().filter(|e| e.node == n && head(&e.text) == "DUMP").count();
        let before: Vec<usize> = {
            let s = self.snap();
            names.iter().map(|n| count(&s, n)).collect()
        };
        for n in names {
            self.send(n, "DUMP").await?;
        }
        self.wait(5000, |e| names.iter().zip(&before).all(|(n, b)| count(e, n) > *b)).await;
        let s = self.snap();
        let mut out = HashMap::new();
        for n in names {
            if let Some(e) = s.iter().rev().find(|e| e.node == *n && head(&e.text) == "DUMP") {
                let m = e
                    .text
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|t| t.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                    .collect();
                out.insert((*n).to_string(), m);
            }
        }
        Ok(out)
    }
    async fn end(&mut self) {
        for n in &mut self.nodes {
            let _ = n.proc.start_kill();
            let _ = n.proc.wait().await;
        }
    }
}

// ---- analysis over the event log ----

fn tc(evs: &[Ev], obs: &str, subj: &str, status: &str, after: u64) -> Option<u64> {
    evs.iter()
        .find(|e| {
            e.node == obs
                && e.us >= after
                && head(&e.text) == "TABLE_CHANGE"
                && kv(&e.text, "subject") == Some(subj)
                && kv(&e.text, "status") == Some(status)
        })
        .map(|e| e.us)
}

/// First moment `obs` holds every subject Online at once.
fn full_time(evs: &[Ev], obs: &str, subjects: &[&str]) -> Option<u64> {
    let mut st: HashMap<&str, &str> = HashMap::new();
    for e in evs.iter().filter(|e| e.node == obs && head(&e.text) == "TABLE_CHANGE") {
        if let (Some(s), Some(t)) = (kv(&e.text, "subject"), kv(&e.text, "status")) {
            st.insert(s, t);
            if subjects.iter().all(|x| st.get(x) == Some(&"Online")) {
                return Some(e.us);
            }
        }
    }
    None
}

fn all_full(evs: &[Ev], nodes: &[&str]) -> Option<u64> {
    let mut mx = 0;
    for o in nodes {
        let others: Vec<&str> = nodes.iter().copied().filter(|x| x != o).collect();
        mx = mx.max(full_time(evs, o, &others)?);
    }
    Some(mx)
}

fn first(evs: &[Ev], pred: impl Fn(&Ev) -> bool) -> Option<u64> {
    evs.iter().find(|e| pred(e)).map(|e| e.us)
}

// ---- stats ----

#[derive(Default)]
struct Stats {
    m: BTreeMap<(String, String), Vec<Option<f64>>>,
    notes: Vec<String>,
}
impl Stats {
    fn rec(&mut self, scn: &str, metric: &str, v: Option<f64>) {
        eprintln!("  M {scn} {metric} = {v:?}");
        self.m.entry((scn.into(), metric.into())).or_default().push(v);
    }
    fn note(&mut self, scn: &str, text: String) {
        eprintln!("  NOTE {scn}: {text}");
        self.notes.push(format!("{scn}: {text}"));
    }
    fn print(&self) {
        println!("| Scenario | Measure | min | median | max | n (missing) |\n|---|---|---|---|---|---|");
        for ((s, m), v) in &self.m {
            let mut x: Vec<f64> = v.iter().flatten().copied().collect();
            x.sort_by(f64::total_cmp);
            let miss = v.len() - x.len();
            if x.is_empty() {
                println!("| {s} | {m} | - | - | - | 0 ({miss}) |");
            } else {
                let med = x[x.len() / 2];
                println!("| {s} | {m} | {:.0} | {:.0} | {:.0} | {} ({miss}) |", x[0], med, x[x.len() - 1], x.len());
            }
        }
        println!("\nNOTES");
        for n in &self.notes {
            println!("- {n}");
        }
    }
}

const N5: [&str; 5] = ["n0", "n1", "n2", "n3", "n4"];
const BOOTS: [&[&str]; 5] = [&[], &["n0"], &["n1"], &["n0"], &["n2"]];

async fn base5(cl: &mut Cl, scn: &str, st: &mut Stats) -> Result<u64> {
    for n in N5 {
        cl.spawn(n).await?;
    }
    cl.exchange().await?;
    let t0 = now_us();
    for (n, b) in N5.iter().zip(BOOTS) {
        cl.start(n, b).await?;
    }
    let ok = cl.wait(20_000, |e| all_full(e, &N5).is_some()).await;
    if !ok {
        st.note(scn, "5-node tables did not fill within 20 s".into());
    }
    Ok(t0)
}

fn sum(d: &HashMap<String, HashMap<String, String>>, k: &str) -> f64 {
    d.values().filter_map(|m| m.get(k)).filter_map(|v| v.parse::<f64>().ok()).sum()
}

async fn s1(f: &Flags, scn: &str, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    let t0 = base5(&mut cl, scn, st).await?;
    sleep(1500).await;
    let e = cl.snap();
    st.rec(scn, "all_five_tables_full_ms", all_full(&e, &N5).map(|t| ms(t0, t)));
    let fulls: Vec<u64> = N5
        .iter()
        .filter_map(|o| full_time(&e, o, &N5.iter().copied().filter(|x| x != o).collect::<Vec<_>>()))
        .collect();
    st.rec(scn, "first_node_full_ms", fulls.iter().min().map(|t| ms(t0, *t)));
    st.rec(scn, "A_learns_C_ms", tc(&e, "n0", "n2", "Online", 0).map(|t| ms(t0, t)));
    st.rec(scn, "C_learns_A_ms", tc(&e, "n2", "n0", "Online", 0).map(|t| ms(t0, t)));
    for (o, s) in [("n0", "n2"), ("n2", "n0")] {
        let via = e
            .iter()
            .find(|x| x.node == o && head(&x.text) == "TABLE_CHANGE" && kv(&x.text, "subject") == Some(s))
            .and_then(|x| kv(&x.text, "via").map(str::to_string));
        st.note(scn, format!("{o} first learned {s} via {via:?}"));
    }
    let d = cl.dump(&N5).await?;
    st.rec(scn, "presence_tx_total", Some(sum(&d, "presence_tx")));
    st.rec(scn, "introduce_tx_total", Some(sum(&d, "introduce_tx")));
    st.rec(scn, "introack_tx_total", Some(sum(&d, "introack_tx")));
    cl.end().await;
    Ok(())
}

async fn s2(f: &Flags, scn: &str, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    base5(&mut cl, scn, st).await?;
    sleep(2000).await;
    let before = cl.dump(&N5).await?;
    cl.spawn("n5").await?;
    cl.exchange().await?;
    let t0 = now_us();
    cl.start("n5", &["n0"]).await?;
    let six = ["n0", "n1", "n2", "n3", "n4", "n5"];
    cl.wait(30_000, |e| all_full(e, &six).is_some()).await;
    sleep(2000).await;
    let e = cl.snap();
    st.rec(scn, "late_holds_all_five_ms", full_time(&e, "n5", &N5).map(|t| ms(t0, t)));
    let holds: Vec<Option<u64>> = N5.iter().map(|o| tc(&e, o, "n5", "Online", t0)).collect();
    st.rec(scn, "all_five_hold_late_ms", holds.iter().copied().collect::<Option<Vec<u64>>>().and_then(|v| v.into_iter().max()).map(|t| ms(t0, t)));
    let first_hold = holds.iter().flatten().min().copied();
    st.rec(scn, "first_peer_holds_late_ms", first_hold.map(|t| ms(t0, t)));
    let after = cl.dump(&six).await?;
    for k in ["presence_tx", "introduce_tx", "introack_tx"] {
        st.rec(scn, &format!("{k}_delta"), Some(sum(&after, k) - sum(&before, k)));
    }
    st.rec(scn, "n5_presence_tx", after.get("n5").and_then(|m| m.get("presence_tx")).and_then(|v| v.parse().ok()));
    st.rec(scn, "n5_introduce_tx", after.get("n5").and_then(|m| m.get("introduce_tx")).and_then(|v| v.parse().ok()));
    cl.end().await;
    Ok(())
}

async fn s3(f: &Flags, scn: &str, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    base5(&mut cl, scn, st).await?;
    sleep(2000).await;
    let t0 = now_us();
    cl.send("n3", "DEPART").await?;
    let peers = ["n0", "n1", "n2", "n4"];
    cl.wait(20_000, |e| {
        first(e, |x| x.node == "n3" && x.text == "EXIT").is_some()
            && peers.iter().all(|p| tc(e, p, "n3", "Gone", 0).is_some() || tc(e, p, "n3", "Unreachable", 0).is_some())
    })
    .await;
    sleep(5000).await;
    let e = cl.snap();
    let exit = first(&e, |x| x.node == "n3" && x.text == "EXIT");
    st.rec(scn, "departure_bcast_returned_ms", first(&e, |x| x.node == "n3" && x.text == "DEPART_BCAST_RETURNED").map(|t| ms(t0, t)));
    st.rec(scn, "n3_exit_ms", exit.map(|t| ms(t0, t)));
    let gone: Vec<Option<u64>> = peers.iter().map(|p| tc(&e, p, "n3", "Gone", t0)).collect();
    st.rec(scn, "first_peer_gone_ms", gone.iter().flatten().min().map(|t| ms(t0, *t)));
    st.rec(scn, "last_peer_gone_ms", gone.iter().copied().collect::<Option<Vec<u64>>>().and_then(|v| v.into_iter().max()).map(|t| ms(t0, t)));
    let rx_before = peers
        .iter()
        .filter(|p| first(&e, |x| &x.node == *p && head(&x.text) == "RX_DEPARTURE").zip(exit).is_some_and(|(r, x)| r < x))
        .count();
    st.rec(scn, "peers_with_departure_rx_before_exit_of_4", Some(rx_before as f64));
    let rx_any = peers.iter().filter(|p| first(&e, |x| &x.node == *p && head(&x.text) == "RX_DEPARTURE").is_some()).count();
    st.rec(scn, "peers_with_departure_rx_ever_of_4", Some(rx_any as f64));
    let unreach = peers.iter().filter(|p| tc(&e, p, "n3", "Unreachable", 0).is_some()).count();
    st.rec(scn, "peers_that_ever_marked_n3_unreachable_of_4", Some(unreach as f64));
    let d = cl.dump(&peers).await?;
    let not_gone = peers.iter().filter(|p| d.get(**p).and_then(|m| m.get("T:n3")).map(String::as_str) != Some("Gone")).count();
    st.rec(scn, "peers_final_not_gone_of_4", Some(not_gone as f64));
    cl.end().await;
    Ok(())
}

async fn s4(f: &Flags, scn: &str, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    base5(&mut cl, scn, st).await?;
    sleep(2000).await;
    let peers = ["n0", "n1", "n2", "n4"];
    let pre = cl.snap();
    let neighbors: Vec<&str> = peers
        .iter()
        .copied()
        .filter(|p| first(&pre, |x| x.node == *p && head(&x.text) == "NEIGHBOR_UP" && kv(&x.text, "subject") == Some("n3")).is_some())
        .collect();
    let t0 = now_us();
    cl.kill("n3").await?;
    cl.wait(60_000, |e| peers.iter().all(|p| tc(e, p, "n3", "Unreachable", t0).is_some())).await;
    sleep(1000).await;
    let e = cl.snap();
    st.rec(scn, "neighbors_of_n3_before_kill_of_4", Some(neighbors.len() as f64));
    let nd: Vec<(&str, u64)> = peers
        .iter()
        .filter_map(|p| first(&e, |x| &x.node == *p && x.us >= t0 && head(&x.text) == "NEIGHBOR_DOWN" && kv(&x.text, "subject") == Some("n3")).map(|t| (*p, t)))
        .collect();
    st.rec(scn, "neighbor_down_observers_of_4", Some(nd.len() as f64));
    st.rec(scn, "neighbor_down_first_ms", nd.iter().map(|x| x.1).min().map(|t| ms(t0, t)));
    st.rec(scn, "neighbor_down_last_ms", nd.iter().map(|x| x.1).max().map(|t| ms(t0, t)));
    let probes: Vec<&Ev> = e.iter().filter(|x| x.us >= t0 && head(&x.text) == "PROBE" && kv(&x.text, "subject") == Some("n3")).collect();
    st.rec(scn, "probe_fail_count", Some(probes.iter().filter(|x| kv(&x.text, "ok") == Some("false")).count() as f64));
    st.rec(scn, "probe_ok_count", Some(probes.iter().filter(|x| kv(&x.text, "ok") == Some("true")).count() as f64));
    st.rec(scn, "probe_duration_max_ms", probes.iter().filter_map(|x| kv(&x.text, "dur_ms")?.parse().ok()).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v)))));
    let us: Vec<u64> = e.iter().filter(|x| x.us >= t0 && head(&x.text) == "UNREACH_SENT" && kv(&x.text, "subject") == Some("n3")).map(|x| x.us).collect();
    st.rec(scn, "unreachable_gossip_sent_count", Some(us.len() as f64));
    st.rec(scn, "unreachable_gossip_first_sent_ms", us.iter().min().map(|t| ms(t0, *t)));
    let marks: Vec<(&str, Option<u64>)> = peers.iter().map(|p| (*p, tc(&e, p, "n3", "Unreachable", t0))).collect();
    st.rec(scn, "first_peer_marks_unreachable_ms", marks.iter().filter_map(|x| x.1).min().map(|t| ms(t0, t)));
    st.rec(scn, "last_peer_marks_unreachable_ms", marks.iter().map(|x| x.1).collect::<Option<Vec<u64>>>().and_then(|v| v.into_iter().max()).map(|t| ms(t0, t)));
    // test 5 shape: neighbor-down to mark, per observer of NeighborDown
    let nd2mark = nd.iter().filter_map(|(p, t)| marks.iter().find(|m| m.0 == *p).and_then(|m| m.1).map(|m| ms(*t, m))).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))));
    st.rec(scn, "neighbor_down_to_own_mark_max_ms", nd2mark);
    let first_sent = us.iter().min().copied();
    let lag = marks
        .iter()
        .filter(|m| !nd.iter().any(|n| n.0 == m.0))
        .filter_map(|m| m.1.zip(first_sent).map(|(m, s)| ms(s, m)))
        .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))));
    st.rec(scn, "non_neighbor_mark_after_first_unreachable_gossip_max_ms", lag);
    let fp = e.iter().filter(|x| x.us >= t0 && head(&x.text) == "TABLE_CHANGE" && kv(&x.text, "status") == Some("Unreachable") && kv(&x.text, "subject") != Some("n3")).count();
    st.rec(scn, "false_unreachable_marks_of_live_members", Some(fp as f64));
    cl.end().await;
    Ok(())
}

async fn s5(f: &Flags, scn: &str, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    base5(&mut cl, scn, st).await?;
    sleep(2000).await;
    let peers = ["n0", "n2", "n4"];
    let t0 = now_us();
    cl.send("n1", "RUMOR n3").await?;
    cl.wait(20_000, |e| {
        peers.iter().all(|p| {
            tc(e, p, "n3", "Unreachable", t0)
                .and_then(|u| e.iter().find(|x| x.node == *p && x.us > u && head(&x.text) == "TABLE_CHANGE" && kv(&x.text, "subject") == Some("n3") && kv(&x.text, "status") == Some("Online")))
                .is_some()
        })
    })
    .await;
    sleep(1000).await;
    let e = cl.snap();
    let unr: Vec<Option<u64>> = peers.iter().map(|p| tc(&e, p, "n3", "Unreachable", t0)).collect();
    st.rec(scn, "peers_that_marked_unreachable_of_3", Some(unr.iter().flatten().count() as f64));
    st.rec(scn, "first_peer_marks_unreachable_ms", unr.iter().flatten().min().map(|t| ms(t0, *t)));
    st.rec(scn, "n3_rx_rumor_ms", first(&e, |x| x.node == "n3" && x.us >= t0 && x.text == "RX_RUMOR_SELF").map(|t| ms(t0, t)));
    st.rec(scn, "n3_reannounce_ms", first(&e, |x| x.node == "n3" && x.us >= t0 && x.text == "ANNOUNCE reason=rumor").map(|t| ms(t0, t)));
    let back: Vec<Option<u64>> = peers
        .iter()
        .zip(&unr)
        .map(|(p, u)| u.and_then(|u| first(&e, |x| &x.node == *p && x.us > u && head(&x.text) == "TABLE_CHANGE" && kv(&x.text, "subject") == Some("n3") && kv(&x.text, "status") == Some("Online"))))
        .collect();
    st.rec(scn, "first_peer_back_online_ms", back.iter().flatten().min().map(|t| ms(t0, *t)));
    st.rec(scn, "last_peer_back_online_ms", back.iter().copied().collect::<Option<Vec<u64>>>().and_then(|v| v.into_iter().max()).map(|t| ms(t0, t)));
    let d = cl.dump(&N5).await?;
    let on = N5.iter().filter(|p| **p != "n3" && d.get(**p).and_then(|m| m.get("T:n3")).map(String::as_str) == Some("Online") || **p == "n3").count() - 1;
    st.rec(scn, "peers_final_hold_n3_online_of_4", Some(on as f64));
    cl.end().await;
    Ok(())
}

/// Replay scenarios. `fresh`: a sixth node joins after the departure (no tombstone possible).
async fn s6(f: &Flags, scn: &str, fresh: bool, st: &mut Stats) -> Result<()> {
    let mut cl = Cl::new(f.clone());
    base5(&mut cl, scn, st).await?;
    sleep(if fresh { f.max_age + 2000 } else { 1500 }).await;
    cl.send("n3", "DEPART").await?;
    let peers = ["n0", "n1", "n2", "n4"];
    cl.wait(20_000, |e| peers.iter().all(|p| tc(e, p, "n3", "Gone", 0).is_some() || tc(e, p, "n3", "Unreachable", 0).is_some())).await;
    sleep(1000).await;
    let i3 = cl.idx("n3");
    cl.nodes[i3].alive = false;
    let mut watch: Vec<&str> = vec!["n0", "n2", "n4"];
    if fresh {
        cl.spawn("n5").await?;
        cl.exchange().await?;
        // name only (the address is dead): lets n5 log n3 under its name
        let (id3, sock3) = (cl.nodes[i3].id.clone(), cl.nodes[i3].sock.clone());
        cl.send("n5", &format!("ADDR n3 {id3} {sock3}")).await?;
        cl.start("n5", &["n0"]).await?;
        cl.wait(20_000, |e| full_time(e, "n5", &["n0", "n1", "n2", "n4"]).is_some()).await;
        sleep(1000).await;
        watch.push("n5");
    }
    let t0 = now_us();
    cl.send("n1", "REPLAY n3").await?;
    sleep(3500).await;
    let e = cl.snap();
    st.rec(scn, "replay_sent", first(&e, |x| x.node == "n1" && x.text == "REPLAY_SENT").map(|_| 1.0));
    let d = cl.dump(&watch).await?;
    let online: Vec<&str> = watch.iter().copied().filter(|p| d.get(*p).and_then(|m| m.get("T:n3")).map(String::as_str) == Some("Online")).collect();
    st.rec(scn, &format!("tables_showing_n3_online_after_replay_of_{}", watch.len()), Some(online.len() as f64));
    let drops: Vec<String> = e.iter().filter(|x| x.us >= t0 && head(&x.text) == "DROP").map(|x| format!("{}:{}", x.node, kv(&x.text, "reason").unwrap_or("?"))).collect();
    st.note(scn, format!("watchers {watch:?}; Online={online:?}; drops after replay={drops:?}; n5 n3-entry={:?}", d.get("n5").and_then(|m| m.get("T:n3"))));
    cl.end().await;
    Ok(())
}

pub async fn run(args: &[String]) -> Result<()> {
    let which = args.get(2).map_or("all", String::as_str);
    let init_delay = args.iter().position(|a| a == "--init-delay").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(3000);
    let reps: usize = args.iter().position(|a| a == "--reps").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(3);
    let base = Flags { s12: true, max_age: 300_000, init_delay, sleep: 0 };
    let mut st = Stats::default();
    let want = |k: &str| which == "all" || which == k;
    for r in 1..=reps {
        eprintln!("== run {r}");
        let tag = if init_delay != 3000 { format!("-init{init_delay}") } else { String::new() };
        if want("s1") {
            s1(&base, &format!("1{tag}"), &mut st).await?;
        }
        if want("s2") {
            s2(&base, &format!("2{tag}"), &mut st).await?;
        }
        if init_delay != 3000 {
            continue;
        }
        if want("s3") {
            s3(&base, "3", &mut st).await?;
            s3(&Flags { sleep: 50, ..base.clone() }, "3-sleep50ms", &mut st).await?;
            s3(&Flags { sleep: 1000, ..base.clone() }, "3-sleep1000ms", &mut st).await?;
        }
        if want("s4") {
            s4(&base, "4", &mut st).await?;
        }
        if want("s5") {
            s5(&base, "5", &mut st).await?;
        }
        if want("s6") {
            s6(&Flags { sleep: 200, ..base.clone() }, "6a-tombstone", false, &mut st).await?;
            s6(&Flags { s12: false, sleep: 200, ..base.clone() }, "6a-control-no-S12", false, &mut st).await?;
            let short = Flags { max_age: 3000, sleep: 200, ..base.clone() };
            s6(&short, "6b-fresh-joiner-maxage3s", true, &mut st).await?;
            s6(&Flags { s12: false, ..short }, "6b-control-no-S12", true, &mut st).await?;
        }
    }
    st.print();
    Ok(())
}
