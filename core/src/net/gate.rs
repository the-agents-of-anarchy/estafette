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
    report_closing: Box<dyn Fn(GateFailure) + Send + Sync>,
}

impl NetworkSecretGate {
    /// Builds the gate for this endpoint's id under the network's gate key, drawing each nonce from the source and handing each closing reason to the sink.
    pub fn new(
        gate_key: GateKey,
        own_id: EndpointId,
        fill_nonce: impl Fn(&mut Nonce) + Send + Sync + 'static,
        report_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    ) -> Self {
        Self {
            gate_key,
            own_id,
            fill_nonce: Box::new(fill_nonce),
            report_closing: Box::new(report_closing),
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
            Err(closing_reason) => {
                (self.report_closing)(closing_reason);
                gate_refusal()
            }
        }
    }
}

/// Why the gate closed a gossip connection.
#[derive(Debug)]
pub enum GateFailure {
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
            Self::Transport(error) => write!(
                formatter,
                "the connection failed while opening or accepting the gate stream: {error}"
            ),
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
