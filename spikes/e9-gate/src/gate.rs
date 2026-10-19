use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use iroh::{
    EndpointId, SecretKey,
    endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, Side},
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};

use crate::node::now_us;

const WAIT: Duration = Duration::from_secs(3);

fn proof(key: &[u8; 32], nonce: &[u8; 32], verifier: &EndpointId, prover: &EndpointId) -> blake3::Hash {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(nonce).update(verifier.as_bytes()).update(prover.as_bytes());
    h.finalize()
}

/// Verifier side: open a bi-stream, send a nonce, require the keyed proof back.
async fn challenge(conn: &Connection, key: &[u8; 32], me: &EndpointId, peer: &EndpointId) -> Result<()> {
    let nonce = SecretKey::generate().to_bytes();
    let (mut tx, mut rx) = conn.open_bi().await?;
    tx.write_all(&nonce).await?;
    tx.finish()?;
    let mut got = [0u8; 32];
    rx.read_exact(&mut got).await?;
    ensure!(blake3::Hash::from(got) == proof(key, &nonce, me, peer), "bad proof");
    Ok(())
}

/// Prover side: accept the peer's stream, answer its nonce.
async fn respond(conn: &Connection, key: &[u8; 32], me: &EndpointId, peer: &EndpointId) -> Result<()> {
    let (mut tx, mut rx) = conn.accept_bi().await?;
    let mut nonce = [0u8; 32];
    rx.read_exact(&mut nonce).await?;
    tx.write_all(proof(key, &nonce, peer, me).as_bytes()).await?;
    tx.finish()?;
    Ok(())
}

fn report(name: &str, dir: &str, peer: &EndpointId, t: Instant, r: &Result<()>) {
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    match r {
        Ok(()) => println!("EV {} {name} GATE_OK dir={dir} ms={ms:.3}", now_us()),
        Err(e) => println!("EV {} {name} GATE_FAIL dir={dir} peer={} ms={ms:.3} why={e}", now_us(), peer.fmt_short()),
    }
}

/// Mutual: both directions on both sides, inside `after_handshake`, so gossip's own dials are
/// covered. One-way (`mutual == false`): the dialer only answers; the acceptor is `GatedGossip`.
#[derive(Debug)]
pub struct GateHook {
    pub name: String,
    pub key: [u8; 32],
    pub me: EndpointId,
    pub mutual: bool,
}

impl EndpointHooks for GateHook {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if conn.alpn() != GOSSIP_ALPN || !(self.mutual || conn.side() == Side::Client) {
            return AfterHandshakeOutcome::accept();
        }
        let (peer, t) = (conn.remote_id(), Instant::now());
        let both = async {
            if self.mutual {
                tokio::try_join!(challenge(conn, &self.key, &self.me, &peer), respond(conn, &self.key, &self.me, &peer))?;
                Ok(())
            } else {
                respond(conn, &self.key, &self.me, &peer).await
            }
        };
        let r = tokio::time::timeout(WAIT, both).await.unwrap_or_else(|_| Err(anyhow::anyhow!("timeout")));
        report(&self.name, if conn.side() == Side::Client { "out" } else { "in" }, &peer, t, &r);
        match r {
            Ok(()) => AfterHandshakeOutcome::accept(),
            Err(_) => AfterHandshakeOutcome::Reject { error_code: 403u32.into(), reason: b"gate".to_vec() },
        }
    }
}

/// Accept-loop wrapper: challenge, then hand the connection to gossip.
#[derive(Debug, Clone)]
pub struct GatedGossip {
    pub name: String,
    pub key: [u8; 32],
    pub me: EndpointId,
    pub gossip: Gossip,
    /// Test only: a lying acceptor that challenges but admits whoever answers.
    pub lax: bool,
}

impl ProtocolHandler for GatedGossip {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (peer, t) = (conn.remote_id(), Instant::now());
        let r = tokio::time::timeout(WAIT, challenge(&conn, &self.key, &self.me, &peer))
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("timeout")));
        report(&self.name, "in", &peer, t, &r);
        if r.is_err() && !self.lax {
            conn.close(403u32.into(), b"gate");
            return Ok(());
        }
        self.gossip.handle_connection(conn).await.map_err(AcceptError::from_err)
    }
}
