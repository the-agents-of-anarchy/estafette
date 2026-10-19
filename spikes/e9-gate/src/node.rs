use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey,
    address_lookup::memory::MemoryLookup,
    endpoint::presets,
    protocol::Router,
};
use iroh_gossip::{
    api::{Event, GossipSender},
    net::{GOSSIP_ALPN, Gossip},
    proto::TopicId,
};
use crate::gate::{GateHook, GatedGossip};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

pub fn now_us() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros()))
        .unwrap_or(0)
}

pub fn derive(ctx: &str, secret: &[u8]) -> [u8; 32] {
    blake3::derive_key(ctx, secret)
}

pub fn unhex32(s: &str) -> Result<[u8; 32]> {
    Ok(blake3::Hash::from_hex(s)?.into())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Wire {
    kind: String,
    node: String,
    seq: u64,
    sent_us: u64,
    nonce: u64,
    mac: String,
}

fn mac_of(key: &[u8; 32], w: &Wire) -> blake3::Hash {
    let msg = format!("{}|{}|{}|{}|{}", w.kind, w.node, w.seq, w.sent_us, w.nonce);
    blake3::keyed_hash(key, msg.as_bytes())
}

fn rand_bytes() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

fn rand_u64() -> u64 {
    let b = rand_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

struct Node {
    name: String,
    ep: Endpoint,
    lookup: MemoryLookup,
    gossip: Gossip,
    topic: TopicId,
    mac_key: [u8; 32],
    verify: bool,
    names: Mutex<HashMap<EndpointId, String>>,
    sender: OnceLock<GossipSender>,
}

impl Node {
    fn ev(&self, text: &str) {
        println!("EV {} {} {text}", now_us(), self.name);
    }
    fn nm(&self, id: &EndpointId) -> String {
        self.names.lock().map_or_else(
            |_| id.fmt_short().to_string(),
            |m| m.get(id).cloned().unwrap_or_else(|| format!("?{}", id.fmt_short())),
        )
    }
    fn wire(&self, kind: &str, seq: u64, forged: bool) -> Wire {
        let mut w = Wire {
            kind: kind.to_string(),
            node: self.ep.id().to_string(),
            seq,
            sent_us: now_us(),
            nonce: rand_u64(),
            mac: String::new(),
        };
        w.mac = if forged {
            blake3::hash(&rand_bytes()).to_hex().to_string()
        } else {
            mac_of(&self.mac_key, &w).to_hex().to_string()
        };
        w
    }
    async fn bcast(&self, w: &Wire) {
        let bytes = serde_json::to_vec(w).unwrap_or_default();
        if let Some(s) = self.sender.get()
            && let Err(e) = s.broadcast(Bytes::from(bytes)).await
        {
            self.ev(&format!("BCAST_ERR {e:?}"));
        }
    }
    /// Logs every payload before any MAC decision; `mac=` is the verdict a member would apply.
    fn on_received(&self, content: &[u8], scope: &str, from: &EndpointId) {
        let Ok(w) = serde_json::from_slice::<Wire>(content) else {
            self.ev(&format!("RX undecodable len={}", content.len()));
            return;
        };
        let mac = if self.verify {
            let ok = blake3::Hash::from_hex(&w.mac).is_ok_and(|m| m == mac_of(&self.mac_key, &w));
            if ok { "ok" } else { "bad" }
        } else {
            "unchecked"
        };
        let origin = w.node.parse::<EndpointId>().map_or_else(|_| "?".to_string(), |id| self.nm(&id));
        self.ev(&format!(
            "RX kind={} seq={} sent_us={} from={} scope={scope} mac={mac} origin={origin}",
            w.kind,
            w.seq,
            w.sent_us,
            self.nm(from),
        ));
    }
}

pub async fn run(args: &[String]) -> Result<()> {
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let name = val("--name").context("--name")?;
    let secret = val("--secret-hex").map(|s| unhex32(&s)).transpose()?;
    let topic_bytes = match (val("--topic-hex"), &secret) {
        (Some(t), _) => unhex32(&t)?,
        (None, Some(s)) => derive("messaging topic v1", s),
        (None, None) => anyhow::bail!("need --topic-hex or --secret-hex"),
    };
    let mac_key = secret.map_or_else(rand_bytes, |s| derive("messaging mac v1", &s));
    let sk = match val("--key-hex") {
        Some(k) => SecretKey::from_bytes(&unhex32(&k)?),
        None => SecretKey::generate(),
    };
    let mode = val("--gate").unwrap_or_else(|| "none".into());
    let gate_key = match (val("--gate-key-hex"), &secret) {
        (Some(k), _) => unhex32(&k)?,
        (None, Some(s)) => derive("messaging gate v1", s),
        (None, None) => rand_bytes(),
    };
    let me = sk.public();

    let lookup = MemoryLookup::new();
    let mut b = Endpoint::builder(presets::Minimal)
        .secret_key(sk)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind_addr(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    if mode != "none" {
        b = b.hooks(GateHook { name: name.clone(), key: gate_key, me, mutual: mode == "hook" });
    }
    let ep = b.bind().await?;
    let gossip = Gossip::builder().spawn(ep.clone());
    let node = Arc::new(Node {
        name,
        ep: ep.clone(),
        lookup,
        gossip: gossip.clone(),
        topic: TopicId::from_bytes(topic_bytes),
        mac_key,
        verify: secret.is_some(),
        names: Mutex::new(HashMap::new()),
        sender: OnceLock::new(),
    });
    let router = Router::builder(ep.clone());
    let _router = if mode.starts_with("wrapper") {
        router.accept(GOSSIP_ALPN, GatedGossip { name: node.name.clone(), key: gate_key, me, gossip, lax: mode == "wrapper-lax" }).spawn()
    } else {
        router.accept(GOSSIP_ALPN, gossip).spawn()
    };
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
                node.names.lock().map_err(|e| anyhow::anyhow!("{e}"))?.insert(id, (*name).to_string());
            }
            ["NAME", name, id] => {
                let id: EndpointId = id.parse()?;
                node.names.lock().map_err(|e| anyhow::anyhow!("{e}"))?.insert(id, (*name).to_string());
            }
            ["START", boots] => {
                let boots: Vec<EndpointId> = if *boots == "-" {
                    Vec::new()
                } else {
                    boots.split(',').map(str::parse).collect::<Result<_, _>>()?
                };
                node.ev("START");
                match node.gossip.subscribe(node.topic, boots).await {
                    Ok(t) => {
                        node.ev("SUBSCRIBE ok");
                        let (tx, mut rx) = t.split();
                        let _ = node.sender.set(tx);
                        let me = node.clone();
                        tokio::spawn(async move {
                            while let Some(ev) = rx.next().await {
                                match ev {
                                    Ok(Event::Received(m)) => {
                                        me.on_received(&m.content, &format!("{:?}", m.scope), &m.delivered_from);
                                    }
                                    Ok(Event::NeighborUp(id)) => me.ev(&format!("NEIGHBOR_UP subject={}", me.nm(&id))),
                                    Ok(Event::NeighborDown(id)) => me.ev(&format!("NEIGHBOR_DOWN subject={}", me.nm(&id))),
                                    Ok(Event::Lagged) => me.ev("LAGGED"),
                                    Err(e) => me.ev(&format!("STREAM_ERR {e:?}")),
                                }
                            }
                            me.ev("STREAM_END");
                        });
                    }
                    Err(e) => node.ev(&format!("SUBSCRIBE err {e:?}")),
                }
            }
            ["PING", start, n] => {
                let (start, n): (u64, u64) = (start.parse()?, n.parse()?);
                let me = node.clone();
                tokio::spawn(async move {
                    for i in 0..n {
                        let w = me.wire("ping", start.saturating_add(i), false);
                        me.bcast(&w).await;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    me.ev("PING_DONE");
                });
            }
            ["ANNOUNCE"] => {
                let w = node.wire("presence", 0, false);
                node.ev("TX kind=presence");
                node.bcast(&w).await;
            }
            ["FORGE", n] => {
                let n: u64 = n.parse()?;
                let me = node.clone();
                tokio::spawn(async move {
                    for i in 0..n {
                        let w = me.wire("presence", 1000_u64.saturating_add(i), true);
                        me.ev(&format!("TX kind=forged seq={}", w.seq));
                        me.bcast(&w).await;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    me.ev("FORGE_DONE");
                });
            }
            ["QUIT"] => break,
            _ => node.ev(&format!("BAD_CMD {line}")),
        }
    }
    Ok(())
}
