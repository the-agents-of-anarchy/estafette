use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use iroh::EndpointId;
use iroh::endpoint::{
    AfterHandshakeOutcome, ClosedStream, Connection, ConnectionError, EndpointHooks,
    ReadExactError, VarInt, WriteError,
};
use iroh_gossip::ALPN as GOSSIP_ALPN;
use tokio::time::timeout;

use crate::crypto::GateKey;
use crate::event::ShortId;
use crate::net::direct::HeldConnections;
use crate::net::endpoint::{Close, ConnectionId, Direction};

const GATE_NONCE_BYTES: usize = 32;
const PROOF_BYTES: usize = blake3::OUT_LEN;
const GATE_TIMEOUT: Duration = Duration::from_millis(3000);
const GATE_REFUSAL_ERROR_CODE: u32 = 403;
pub(super) const GATE_REFUSAL_REASON: &[u8] = b"gate";

/// The 32-byte nonce each side of the gate challenges the other with (spec 3.6).
pub type GateNonce = [u8; GATE_NONCE_BYTES];
/// The source each gate nonce is drawn from.
pub type FillGateNonce = Arc<dyn Fn(&mut GateNonce) + Send + Sync>;
/// The sink each gossip connection the gate closed is handed to, with why.
pub type ReportGateClosing = Arc<dyn Fn(GateFailure) + Send + Sync>;
type Proof = [u8; PROOF_BYTES];

/// The after-handshake hook that closes every gossip connection whose peer does not prove the network secret and holds every connection it lets through in the register.
pub struct NetworkSecretGate {
    gate_key: GateKey,
    own_id: EndpointId,
    fill_nonce: FillGateNonce,
    report_closing: ReportGateClosing,
    connections: HeldConnections,
}

impl NetworkSecretGate {
    /// Builds the gate for this endpoint's id under the network's gate key, drawing each nonce from the source, handing each closing reason to the sink, which runs inside the handshake and must return without blocking, and holding each connection it lets through in the register.
    pub fn new(
        gate_key: GateKey,
        own_id: EndpointId,
        fill_nonce: FillGateNonce,
        report_closing: ReportGateClosing,
        connections: HeldConnections,
    ) -> Self {
        Self {
            gate_key,
            own_id,
            fill_nonce,
            report_closing,
            connections,
        }
    }

    /// Lets the connection through, holding it in the register.
    fn let_through(&self, connection: &Connection) -> AfterHandshakeOutcome {
        self.connections.hold(connection);
        AfterHandshakeOutcome::accept()
    }

    /// Passes when the peer answered this side's nonce correctly and this side answered the peer's, both within the gate timeout.
    async fn peer_proves_network_secret(
        &self,
        connection: &Connection,
    ) -> Result<(), GateFailureReason> {
        let both_challenges = async {
            tokio::try_join!(
                self.peer_answers_nonce(connection),
                self.answer_peer_nonce(connection),
            )
        };
        timeout(GATE_TIMEOUT, both_challenges)
            .await
            .unwrap_or(Err(GateFailureReason::Timeout))
            .map(|((), ())| ())
    }

    /// Sends a fresh nonce on a stream this side opens and accepts only the peer's correct proof.
    async fn peer_answers_nonce(&self, connection: &Connection) -> Result<(), GateFailureReason> {
        let nonce = self.fresh_nonce();
        let answer = send_nonce_for_answer(connection, &nonce).await?;
        let expected = gate_proof(&self.gate_key, &nonce, self.own_id, connection.remote_id());
        if blake3::Hash::from_bytes(answer) == expected {
            Ok(())
        } else {
            Err(GateFailureReason::WrongProof)
        }
    }

    /// Reads the nonce the peer sends on the stream it opens and answers it with this side's proof.
    async fn answer_peer_nonce(&self, connection: &Connection) -> Result<(), GateFailureReason> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let mut nonce = [0; GATE_NONCE_BYTES];
        recv.read_exact(&mut nonce).await?;
        let proof = gate_proof(&self.gate_key, &nonce, connection.remote_id(), self.own_id);
        send.write_all(proof.as_bytes()).await?;
        Ok(send.finish()?)
    }

    /// Draws a nonce from the source the gate was built with.
    fn fresh_nonce(&self) -> GateNonce {
        let mut nonce = [0; GATE_NONCE_BYTES];
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
            return self.let_through(connection);
        }
        match self.peer_proves_network_secret(connection).await {
            Ok(()) => self.let_through(connection),
            Err(reason) => {
                (self.report_closing)(GateFailure::of(connection, reason));
                gate_refusal()
            }
        }
    }
}

/// A gossip connection the gate closed: which connection, whose peer, which side opened it, and why.
#[derive(Debug)]
pub struct GateFailure {
    pub(crate) connection: ConnectionId,
    pub(crate) peer: ShortId,
    pub(crate) direction: Direction,
    pub(crate) reason: GateFailureReason,
}

impl GateFailure {
    /// The closing of the connection for the reason.
    fn of(connection: &Connection, reason: GateFailureReason) -> Self {
        Self {
            connection: ConnectionId::of(connection),
            peer: ShortId::from(connection.remote_id()),
            direction: Direction::of(connection.side()),
            reason,
        }
    }
}

/// Why the gate closed a gossip connection.
#[derive(Debug)]
pub enum GateFailureReason {
    Transport(Close),
    StreamWrite(WriteError),
    StreamFinish(ClosedStream),
    StreamRead(ReadExactError),
    WrongProof,
    Timeout,
}

impl fmt::Display for GateFailureReason {
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

impl From<ConnectionError> for GateFailureReason {
    fn from(error: ConnectionError) -> Self {
        Self::Transport(Close::of(&error))
    }
}

impl From<WriteError> for GateFailureReason {
    fn from(error: WriteError) -> Self {
        Self::StreamWrite(error)
    }
}

impl From<ClosedStream> for GateFailureReason {
    fn from(error: ClosedStream) -> Self {
        Self::StreamFinish(error)
    }
}

impl From<ReadExactError> for GateFailureReason {
    fn from(error: ReadExactError) -> Self {
        Self::StreamRead(error)
    }
}

/// Opens a stream, sends the nonce, and reads the peer's answer.
async fn send_nonce_for_answer(
    connection: &Connection,
    nonce: &GateNonce,
) -> Result<Proof, GateFailureReason> {
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
    nonce: &GateNonce,
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
