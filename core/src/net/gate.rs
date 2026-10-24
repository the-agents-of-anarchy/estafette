use std::fmt;
use std::time::Duration;

use iroh::EndpointId;
use iroh::endpoint::{
    AfterHandshakeOutcome, ClosedStream, Connection, ConnectionError, EndpointHooks,
    ReadExactError, VarInt, WriteError,
};
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

    /// Passes when the peer answered this side's nonce correctly and this side answered the peer's, both within the gate timeout.
    async fn peer_proves_network_secret(&self, connection: &Connection) -> Result<(), GateFailure> {
        let both_challenges = async {
            tokio::try_join!(
                self.peer_answers_nonce(connection),
                self.answer_peer_nonce(connection),
            )
        };
        timeout(GATE_TIMEOUT, both_challenges)
            .await
            .unwrap_or(Err(GateFailure::Timeout))
            .map(|((), ())| ())
    }

    /// Sends a fresh nonce on a stream this side opens and accepts only the peer's correct proof.
    async fn peer_answers_nonce(&self, connection: &Connection) -> Result<(), GateFailure> {
        let nonce = self.fresh_nonce();
        let answer = send_nonce_for_answer(connection, &nonce).await?;
        let expected = gate_proof(&self.gate_key, &nonce, self.own_id, connection.remote_id());
        if blake3::Hash::from_bytes(answer) == expected {
            Ok(())
        } else {
            Err(GateFailure::WrongProof)
        }
    }

    /// Reads the nonce the peer sends on the stream it opens and answers it with this side's proof.
    async fn answer_peer_nonce(&self, connection: &Connection) -> Result<(), GateFailure> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let mut nonce = [0; NONCE_BYTES];
        recv.read_exact(&mut nonce).await?;
        let proof = gate_proof(&self.gate_key, &nonce, connection.remote_id(), self.own_id);
        send.write_all(proof.as_bytes()).await?;
        Ok(send.finish()?)
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
        match self.peer_proves_network_secret(connection).await {
            Ok(()) => AfterHandshakeOutcome::accept(),
            Err(_closing_reason) => gate_refusal(),
        }
    }
}

/// Why the gate closed a gossip connection.
#[derive(Debug)]
enum GateFailure {
    Transport(ConnectionError),
    StreamWrite(WriteError),
    StreamFinish(ClosedStream),
    StreamRead(ReadExactError),
    WrongProof,
    Timeout,
}

impl fmt::Display for GateFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "the gate stream could not open: {error}"),
            Self::StreamWrite(error) => {
                write!(formatter, "the gate stream failed to write: {error}")
            }
            Self::StreamFinish(error) => {
                write!(formatter, "the gate stream failed to finish: {error}")
            }
            Self::StreamRead(error) => write!(formatter, "the gate stream failed to read: {error}"),
            Self::WrongProof => formatter.write_str("the peer's proof is not the gate proof"),
            Self::Timeout => formatter.write_str("the gate timeout passed before both proofs"),
        }
    }
}

impl std::error::Error for GateFailure {}

impl From<ConnectionError> for GateFailure {
    fn from(error: ConnectionError) -> Self {
        Self::Transport(error)
    }
}

impl From<WriteError> for GateFailure {
    fn from(error: WriteError) -> Self {
        Self::StreamWrite(error)
    }
}

impl From<ClosedStream> for GateFailure {
    fn from(error: ClosedStream) -> Self {
        Self::StreamFinish(error)
    }
}

impl From<ReadExactError> for GateFailure {
    fn from(error: ReadExactError) -> Self {
        Self::StreamRead(error)
    }
}

/// Opens a stream, sends the nonce, and reads the peer's answer.
async fn send_nonce_for_answer(
    connection: &Connection,
    nonce: &Nonce,
) -> Result<Proof, GateFailure> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(nonce).await?;
    send.finish()?;
    let mut answer = [0; PROOF_BYTES];
    recv.read_exact(&mut answer).await?;
    Ok(answer)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use iroh::endpoint::presets;
    use iroh::{Endpoint, SecretKey};

    const DIALER_SEED: u8 = 11;
    const ACCEPTOR_SEED: u8 = 12;
    const MEMBER_SECRET: [u8; KEY_LEN] = [21; KEY_LEN];
    const OUTSIDER_SECRET: [u8; KEY_LEN] = [12; KEY_LEN];

    async fn bind_ungated_endpoint(seed: u8) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&[seed; KEY_LEN]))
            .alpns(vec![GOSSIP_ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    async fn gossip_connection_between(
        dialer: &Endpoint,
        acceptor: &Endpoint,
    ) -> (Connection, Connection) {
        let (outgoing, incoming) =
            tokio::join!(dialer.connect(acceptor.addr(), GOSSIP_ALPN), async {
                acceptor.accept().await.unwrap().await
            });
        (outgoing.unwrap(), incoming.unwrap())
    }

    fn gate_of(endpoint: &Endpoint, secret: [u8; KEY_LEN], seed: u8) -> NetworkSecretGate {
        NetworkSecretGate::new(
            *NetworkKeys::derive(&NetworkSecret::from_bytes(secret)).gate_key(),
            endpoint.id(),
            move |nonce: &mut Nonce| *nonce = [seed; NONCE_BYTES],
        )
    }

    #[tokio::test]
    async fn spec_3_6_gate_names_a_wrong_answer_as_the_reason_it_closes() {
        let dialer = bind_ungated_endpoint(DIALER_SEED).await;
        let acceptor = bind_ungated_endpoint(ACCEPTOR_SEED).await;
        let (outgoing, incoming) = gossip_connection_between(&dialer, &acceptor).await;
        let member_gate = gate_of(&dialer, MEMBER_SECRET, DIALER_SEED);
        let outsider_gate = gate_of(&acceptor, OUTSIDER_SECRET, ACCEPTOR_SEED);

        let (member_outcome, outsider_answer) = tokio::join!(
            member_gate.peer_proves_network_secret(&outgoing),
            outsider_gate.answer_peer_nonce(&incoming)
        );

        assert!(matches!(member_outcome, Err(GateFailure::WrongProof)));
        assert!(matches!(outsider_answer, Ok(())));
    }

    #[tokio::test]
    async fn spec_3_14_gate_names_the_timeout_as_the_reason_it_closes_on_a_silent_peer() {
        let dialer = bind_ungated_endpoint(DIALER_SEED).await;
        let acceptor = bind_ungated_endpoint(ACCEPTOR_SEED).await;
        let (outgoing, _silent_incoming) = gossip_connection_between(&dialer, &acceptor).await;
        let member_gate = gate_of(&dialer, MEMBER_SECRET, DIALER_SEED);

        let outcome = member_gate.peer_proves_network_secret(&outgoing).await;

        assert!(matches!(outcome, Err(GateFailure::Timeout)));
    }
}
