use std::{
    collections::{BTreeMap, HashMap},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, Signature,
    address_lookup::memory::MemoryLookup,
    endpoint::{Connection, QuicTransportConfig, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{
    api::{Event, GossipSender},
    net::{GOSSIP_ALPN, Gossip},
    proto::TopicId,
};
use n0_future::StreamExt;
use n0_watcher::Watcher;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

pub const DIRECT_ALPN: &[u8] = b"e4/direct/0";
const TOPIC: [u8; 32] = [9u8; 32];
const PROBE_TIMEOUT: Duration = Duration::from_millis(3000);
const INTRO_TIMEOUT: Duration = Duration::from_millis(15000);
const GOSSIP_MAX: usize = 4096;

pub fn now_us() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros()))
        .unwrap_or(0)
}
fn now_ms() -> u64 {
    now_us() / 1000
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum Body {
    Presence { node: EndpointId, seq: u64 },
    Departure { node: EndpointId, seq: u64 },
    Unreachable { subject: EndpointId, reporter: EndpointId, ts: u64 },
}

/// `nonce` is unsigned envelope data: it only changes the gossip message id.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Wire {
    nonce: u64,
    body: Vec<u8>,
    sig: Vec<u8>,
}

fn verify(w: &Wire) -> Option<Body> {
    let body: Body = serde_json::from_slice(&w.body).ok()?;
    let signer = match &body {
        Body::Presence { node, .. } | Body::Departure { node, .. } => *node,
        Body::Unreachable { reporter, .. } => *reporter,
    };
    let sig = Signature::from_bytes(&<[u8; 64]>::try_from(w.sig.as_slice()).ok()?);
    signer.verify(&w.body, &sig).ok()?;
    Some(body)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Online,
    Unreachable,
    Gone,
}

struct Peer {
    status: Status,
    seq: u64,
    last_contact: u64,
}

#[derive(Default)]
struct State {
    table: HashMap<EndpointId, Peer>,
    seq: u64,
    names: HashMap<EndpointId, String>,
    raw: HashMap<EndpointId, Wire>,
    ctr: BTreeMap<&'static str, u64>,
}

#[derive(Clone)]
pub struct Opts {
    pub s12: bool,
    pub max_age_ms: u64,
    pub init_delay_ms: u64,
    pub depart_sleep_ms: u64,
}

pub struct Node {
    name: String,
    sk: SecretKey,
    ep: Endpoint,
    lookup: MemoryLookup,
    gossip: Gossip,
    st: Mutex<State>,
    sender: OnceLock<GossipSender>,
    opts: Opts,
}

impl Node {
    fn ev(&self, text: &str) {
        println!("EV {} {} {text}", now_us(), self.name);
    }
    fn id(&self) -> EndpointId {
        self.ep.id()
    }
    fn nm(&self, st: &State, id: &EndpointId) -> String {
        st.names.get(id).cloned().unwrap_or_else(|| id.fmt_short().to_string())
    }
    fn count(&self, k: &'static str) {
        *self.st.lock().unwrap().ctr.entry(k).or_default() += 1;
    }
    fn wire(&self, body: &Body) -> Wire {
        let body = serde_json::to_vec(body).unwrap_or_default();
        let sig = self.sk.sign(&body).to_bytes().to_vec();
        Wire { nonce: rand_u64(), body, sig }
    }
    fn presence_wire(&self) -> Wire {
        let seq = {
            let mut st = self.st.lock().unwrap();
            // S12: per-member seq = max(now_ms, last + 1)
            st.seq = now_ms().max(st.seq.saturating_add(1));
            st.seq
        };
        self.wire(&Body::Presence { node: self.id(), seq })
    }
    async fn bcast(&self, w: &Wire) {
        let bytes = serde_json::to_vec(w).unwrap_or_default();
        if bytes.len() > GOSSIP_MAX {
            self.ev(&format!("OVERSIZE len={}", bytes.len()));
        }
        if let Some(s) = self.sender.get()
            && let Err(e) = s.broadcast(Bytes::from(bytes)).await
        {
            self.ev(&format!("BCAST_ERR {e:?}"));
        }
    }
    async fn announce(&self, reason: &str) {
        let w = self.presence_wire();
        self.count("presence_tx");
        self.ev(&format!("ANNOUNCE reason={reason}"));
        self.bcast(&w).await;
    }

    /// Insert or change an entry; log each status change. Returns true when the entry is new.
    fn upsert(&self, st: &mut State, id: EndpointId, status: Status, seq: Option<u64>, via: &str) -> bool {
        let new = !st.table.contains_key(&id);
        let p = st.table.entry(id).or_insert(Peer { status, seq: 0, last_contact: 0 });
        let changed = new || p.status != status;
        p.status = status;
        if let Some(s) = seq {
            p.seq = s;
        }
        if changed {
            let n = self.nm(st, &id);
            self.ev(&format!("TABLE_CHANGE subject={n} status={status:?} via={via}"));
        }
        new
    }

    fn contact(&self, id: EndpointId) {
        let mut st = self.st.lock().unwrap();
        let Some(p) = st.table.get_mut(&id) else { return };
        p.last_contact = now_ms();
        if p.status == Status::Unreachable {
            self.upsert(&mut st, id, Status::Online, None, "contact");
        }
    }

    fn handle(self: &Arc<Self>, w: &Wire, via: &'static str) {
        let Some(body) = verify(w) else {
            self.ev("DROP reason=bad_sig");
            return;
        };
        let now = now_ms();
        match body {
            Body::Presence { node, seq } => {
                if node == self.id() {
                    return;
                }
                let new = {
                    let mut st = self.st.lock().unwrap();
                    if self.opts.s12 {
                        if now.saturating_sub(seq) > self.opts.max_age_ms {
                            let n = self.nm(&st, &node);
                            self.ev(&format!("DROP reason=too_old subject={n} age_ms={}", now.saturating_sub(seq)));
                            return;
                        }
                        if let Some(p) = st.table.get(&node)
                            && seq <= p.seq
                        {
                            let n = self.nm(&st, &node);
                            self.ev(&format!("DROP reason=stale subject={n} status={:?} via={via}", p.status));
                            return;
                        }
                    }
                    st.raw.insert(node, w.clone());
                    self.upsert(&mut st, node, Status::Online, Some(seq), via)
                };
                if new {
                    let me = self.clone();
                    tokio::spawn(async move { me.introduce(node).await });
                }
            }
            Body::Departure { node, seq } => {
                if node == self.id() {
                    return;
                }
                let mut st = self.st.lock().unwrap();
                let n = self.nm(&st, &node);
                self.ev(&format!("RX_DEPARTURE subject={n}"));
                if self.opts.s12
                    && let Some(p) = st.table.get(&node)
                    && seq <= p.seq
                {
                    self.ev(&format!("DROP reason=stale_departure subject={n}"));
                    return;
                }
                // S12 tombstone: keep the departed member's last seq, even for an unknown member
                self.upsert(&mut st, node, Status::Gone, Some(seq), "departure");
            }
            Body::Unreachable { subject, reporter: _, ts } => {
                if subject == self.id() {
                    self.ev("RX_RUMOR_SELF");
                    let me = self.clone();
                    tokio::spawn(async move { me.announce("rumor").await });
                    return;
                }
                let mut st = self.st.lock().unwrap();
                if let Some(p) = st.table.get(&subject)
                    && p.status == Status::Online
                    && p.last_contact <= ts
                {
                    self.upsert(&mut st, subject, Status::Unreachable, None, "unreachable_rx");
                }
            }
        }
    }

    async fn direct(&self, to: EndpointId, req: Vec<u8>) -> Result<Vec<u8>> {
        let conn = self.ep.connect(EndpointAddr::new(to), DIRECT_ALPN).await?;
        let (mut tx, mut rx) = conn.open_bi().await?;
        tx.write_all(&req).await?;
        tx.finish()?;
        let out = rx.read_to_end(16384).await?;
        conn.close(0u32.into(), b"done");
        Ok(out)
    }

    async fn introduce(self: Arc<Self>, to: EndpointId) {
        self.count("introduce_tx");
        let mut req = vec![1u8];
        req.extend(serde_json::to_vec(&self.presence_wire()).unwrap_or_default());
        let r = tokio::time::timeout(INTRO_TIMEOUT, self.direct(to, req)).await;
        if let Ok(Ok(bytes)) = r
            && let Ok(w) = serde_json::from_slice::<Wire>(&bytes)
        {
            self.handle(&w, "ack");
            self.contact(to);
        }
    }

    async fn probe(&self, to: EndpointId) -> bool {
        self.count("probe_tx");
        let r = tokio::time::timeout(PROBE_TIMEOUT, self.direct(to, vec![0u8])).await;
        let ok = matches!(r, Ok(Ok(_)));
        if ok {
            self.contact(to);
        }
        ok
    }

    async fn neighbor_down(self: Arc<Self>, id: EndpointId) {
        let t = std::time::Instant::now();
        let ok = self.probe(id).await;
        let n = self.nm(&self.st.lock().unwrap(), &id);
        self.ev(&format!("PROBE subject={n} ok={ok} dur_ms={}", t.elapsed().as_millis()));
        if ok {
            return;
        }
        let was_online = {
            let mut st = self.st.lock().unwrap();
            if st.table.get(&id).is_some_and(|p| p.status == Status::Online) {
                self.upsert(&mut st, id, Status::Unreachable, None, "probe_fail");
                true
            } else {
                false
            }
        };
        if was_online {
            self.count("unreach_tx");
            self.ev(&format!("UNREACH_SENT subject={n}"));
            let w = self.wire(&Body::Unreachable { subject: id, reporter: self.id(), ts: now_ms() });
            self.bcast(&w).await;
        }
    }

    async fn start(self: &Arc<Self>, boots: Vec<EndpointId>) -> Result<()> {
        let topic = self.gossip.subscribe(TopicId::from_bytes(TOPIC), boots).await?;
        let (tx, mut rx) = topic.split();
        let _ = self.sender.set(tx);
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.next().await {
                match ev {
                    Ok(Event::Received(m)) => match serde_json::from_slice::<Wire>(&m.content) {
                        Ok(w) => me.handle(&w, "gossip"),
                        Err(_) => me.ev("DROP reason=undecodable"),
                    },
                    Ok(Event::NeighborUp(id)) => {
                        let (n, online) = {
                            let st = me.st.lock().unwrap();
                            (me.nm(&st, &id), st.table.get(&id).is_some_and(|p| p.status == Status::Online))
                        };
                        me.ev(&format!("NEIGHBOR_UP subject={n} known_online={online}"));
                        if !online {
                            let m2 = me.clone();
                            tokio::spawn(async move { m2.introduce(id).await });
                        }
                    }
                    Ok(Event::NeighborDown(id)) => {
                        let n = me.nm(&me.st.lock().unwrap(), &id);
                        me.ev(&format!("NEIGHBOR_DOWN subject={n}"));
                        let m2 = me.clone();
                        tokio::spawn(async move { m2.neighbor_down(id).await });
                    }
                    _ => {}
                }
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(me.opts.init_delay_ms)).await;
            me.announce("init").await;
            // S13: re-announce on address change (relay status is not exercisable: relays disabled)
            let mut updates = me.ep.watch_addr().stream_updates_only();
            while updates.next().await.is_some() {
                me.ev("ADDR_CHANGE");
                me.announce("addr_change").await;
            }
        });
        Ok(())
    }

    async fn depart(&self) {
        let seq = {
            let mut st = self.st.lock().unwrap();
            st.seq = now_ms().max(st.seq.saturating_add(1));
            st.seq
        };
        let w = self.wire(&Body::Departure { node: self.id(), seq });
        self.count("departure_tx");
        self.ev("DEPART_SEND");
        self.bcast(&w).await;
        self.ev("DEPART_BCAST_RETURNED");
        tokio::time::sleep(Duration::from_millis(self.opts.depart_sleep_ms)).await;
        let _ = tokio::time::timeout(Duration::from_millis(1000), self.gossip.shutdown()).await;
        self.ev("EXIT");
        std::process::exit(0);
    }

    fn dump(&self) {
        let st = self.st.lock().unwrap();
        let mut s = String::from("DUMP");
        for k in ["presence_tx", "introduce_tx", "introack_tx", "probe_tx", "departure_tx", "unreach_tx"] {
            s.push_str(&format!(" {k}={}", st.ctr.get(k).copied().unwrap_or(0)));
        }
        for (id, p) in &st.table {
            s.push_str(&format!(" T:{}={:?}", self.nm(&st, id), p.status));
        }
        self.ev(&s);
    }
}

fn rand_u64() -> u64 {
    let b: [u8; 32] = SecretKey::generate().to_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[derive(Clone)]
struct Direct(Arc<Node>);

impl std::fmt::Debug for Direct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Direct")
    }
}

impl ProtocolHandler for Direct {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let node = &self.0;
        let remote = conn.remote_id();
        let Ok((mut tx, mut rx)) = conn.accept_bi().await else { return Ok(()) };
        let Ok(req) = rx.read_to_end(16384).await else { return Ok(()) };
        let reply = match req.split_first() {
            Some((0, _)) => b"ok".to_vec(),
            Some((1, rest)) => match serde_json::from_slice::<Wire>(rest) {
                Ok(w) => {
                    node.handle(&w, "introduce");
                    node.contact(remote);
                    node.count("introack_tx");
                    serde_json::to_vec(&node.presence_wire()).unwrap_or_default()
                }
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
        let _ = tx.write_all(&reply).await;
        let _ = tx.finish();
        conn.closed().await;
        Ok(())
    }
}

pub async fn run(args: &[String]) -> Result<()> {
    let flag = |k: &str| args.iter().any(|a| a == k);
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let name = val("--name").context("--name")?;
    let opts = Opts {
        s12: !flag("--no-s12"),
        max_age_ms: val("--max-age").map_or(300_000, |v| v.parse().unwrap_or(300_000)),
        init_delay_ms: val("--init-delay").map_or(0, |v| v.parse().unwrap_or(0)),
        depart_sleep_ms: val("--depart-sleep").map_or(0, |v| v.parse().unwrap_or(0)),
    };
    let lookup = MemoryLookup::new();
    let sk = SecretKey::generate();
    let cfg = QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(5))
        .default_path_keep_alive_interval(Duration::from_secs(5))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        .default_path_max_idle_timeout(Duration::from_secs(10))
        .build();
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(sk.clone())
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind_addr(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?
        .transport_config(cfg)
        .bind()
        .await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let node = Arc::new(Node {
        name,
        sk,
        ep: ep.clone(),
        lookup,
        gossip: gossip.clone(),
        st: Mutex::new(State::default()),
        sender: OnceLock::new(),
        opts,
    });
    let _router = Router::builder(ep.clone())
        .accept(GOSSIP_ALPN, gossip)
        .accept(DIRECT_ALPN, Direct(node.clone()))
        .spawn();
    let sock = ep.bound_sockets().first().copied().context("no sock")?;
    println!("READY {} {sock}", ep.id());

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let p: Vec<&str> = line.split_whitespace().collect();
        match p.as_slice() {
            ["ADDR", name, id, sock] => {
                let id: EndpointId = id.parse()?;
                let sock: SocketAddr = sock.parse()?;
                node.lookup.add_endpoint_info(EndpointAddr::new(id).with_ip_addr(sock));
                node.st.lock().unwrap().names.insert(id, (*name).to_string());
            }
            ["START", boots] => {
                let boots = if *boots == "-" {
                    Vec::new()
                } else {
                    boots.split(',').map(str::parse).collect::<Result<_, _>>()?
                };
                node.start(boots).await?;
            }
            ["DEPART"] => {
                let n = node.clone();
                tokio::spawn(async move { n.depart().await });
            }
            ["RUMOR", who] | ["REPLAY", who] => {
                let id = node.st.lock().unwrap().names.iter().find(|(_, n)| n == who).map(|(i, _)| *i);
                let Some(id) = id else { bail!("unknown name {who}") };
                let n = node.clone();
                let rumor = p.first() == Some(&"RUMOR");
                tokio::spawn(async move {
                    let w = if rumor {
                        Some(n.wire(&Body::Unreachable { subject: id, reporter: n.id(), ts: now_ms() }))
                    } else {
                        n.st.lock().unwrap().raw.get(&id).cloned().map(|mut w| {
                            w.nonce = rand_u64();
                            w
                        })
                    };
                    if let Some(w) = w {
                        n.ev(if rumor { "RUMOR_SENT" } else { "REPLAY_SENT" });
                        n.bcast(&w).await;
                    } else {
                        n.ev("REPLAY_NOTHING_CAPTURED");
                    }
                });
            }
            ["DUMP"] => node.dump(),
            ["QUIT"] => break,
            _ => node.ev(&format!("BAD_CMD {line}")),
        }
    }
    Ok(())
}
