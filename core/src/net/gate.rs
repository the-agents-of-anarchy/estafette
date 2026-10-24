use std::fmt;
use std::time::Duration;

use iroh::EndpointId;
use iroh::endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, VarInt};
use iroh_gossip::ALPN as GOSSIP_ALPN;
use tokio::time::timeout;

use crate::crypto::GateKey;

pub const NONCE_BYTES: usize = 32;
const PROOF_BYTES: usize = blake3::OUT_LEN;
const GATE_TIMEOUT: Duration = Duration::from_millis(3000);
const GATE_REFUSAL_ERROR_CODE: u32 = 403;
const GATE_REFUSAL_REASON: &[u8] = b"gate";

type Nonce = [u8; NONCE_BYTES];
type Proof = [u8; PROOF_BYTES];

/// The after-handshake hook that closes every gossip connection whose peer does not prove the network secret.
pub struct NetworkSecretGate {
    gate_key: GateKey,
    own_id: EndpointId,
    fill_nonce: Box<dyn Fn(&mut Nonce) + Send + Sync>,
}

impl NetworkSecretGate {
    /// Builds the gate for this endpoint's id under the network's gate key, drawing each nonce from the source.
    pub fn new(
        gate_key: GateKey,
        own_id: EndpointId,
        fill_nonce: impl Fn(&mut Nonce) + Send + Sync + 'static,
    ) -> Self {
        Self {
            gate_key,
            own_id,
            fill_nonce: Box::new(fill_nonce),
        }
    }

    /// Says whether the peer answered this side's nonce correctly and this side answered the peer's, both within the gate timeout.
    async fn peer_proves_network_secret(&self, connection: &Connection) -> bool {
        let both_challenges = async {
            tokio::try_join!(
                async { self.peer_answers_nonce(connection).await.ok_or(Unproven) },
                async { self.answer_peer_nonce(connection).await.ok_or(Unproven) },
            )
        };
        matches!(timeout(GATE_TIMEOUT, both_challenges).await, Ok(Ok(_)))
    }

    /// Sends a fresh nonce on a stream this side opens and accepts only the peer's correct proof.
    async fn peer_answers_nonce(&self, connection: &Connection) -> Option<()> {
        let nonce = self.fresh_nonce();
        let answer = send_nonce_for_answer(connection, &nonce).await?;
        let expected = gate_proof(&self.gate_key, &nonce, self.own_id, connection.remote_id());
        (blake3::Hash::from_bytes(answer) == expected).then_some(())
    }

    /// Reads the nonce the peer sends on the stream it opens and answers it with this side's proof.
    async fn answer_peer_nonce(&self, connection: &Connection) -> Option<()> {
        let (mut send, mut recv) = connection.accept_bi().await.ok()?;
        let mut nonce = [0; NONCE_BYTES];
        recv.read_exact(&mut nonce).await.ok()?;
        let proof = gate_proof(&self.gate_key, &nonce, connection.remote_id(), self.own_id);
        send.write_all(proof.as_bytes()).await.ok()?;
        send.finish().ok()
    }

    /// Draws a nonce from the source the gate was built with.
    fn fresh_nonce(&self) -> Nonce {
        let mut nonce = [0; NONCE_BYTES];
        (self.fill_nonce)(&mut nonce);
        nonce
    }
}

impl fmt::Debug for NetworkSecretGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkSecretGate")
            .field("own_id", &self.own_id)
            .finish_non_exhaustive()
    }
}

impl EndpointHooks for NetworkSecretGate {
    async fn after_handshake<'a>(&'a self, connection: &'a Connection) -> AfterHandshakeOutcome {
        if connection.alpn() != GOSSIP_ALPN {
            return AfterHandshakeOutcome::accept();
        }
        if self.peer_proves_network_secret(connection).await {
            AfterHandshakeOutcome::accept()
        } else {
            gate_refusal()
        }
    }
}

/// A half of the gate that ended without the proof it needed.
struct Unproven;

/// Opens a stream, sends the nonce, and reads the peer's answer, if one arrives.
async fn send_nonce_for_answer(connection: &Connection, nonce: &Nonce) -> Option<Proof> {
    let (mut send, mut recv) = connection.open_bi().await.ok()?;
    send.write_all(nonce).await.ok()?;
    send.finish().ok()?;
    let mut answer = [0; PROOF_BYTES];
    recv.read_exact(&mut answer).await.ok()?;
    Some(answer)
}

/// The keyed hash under the gate key of the nonce, the verifier's id and the prover's id (spec 3.6).
fn gate_proof(
    gate_key: &GateKey,
    nonce: &Nonce,
    verifier: EndpointId,
    prover: EndpointId,
) -> blake3::Hash {
    blake3::keyed_hash(
        gate_key.as_bytes(),
        &[nonce.as_slice(), verifier.as_bytes(), prover.as_bytes()].concat(),
    )
}

/// The outcome that closes a connection whose peer failed the gate.
fn gate_refusal() -> AfterHandshakeOutcome {
    AfterHandshakeOutcome::Reject {
        error_code: VarInt::from_u32(GATE_REFUSAL_ERROR_CODE),
        reason: GATE_REFUSAL_REASON.to_vec(),
    }
}
