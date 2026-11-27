//! No dial holds the loop: direct requests run in one task in their order, a stopping node sends no Introduce or Probe, and each answer goes back through the requester.

use std::sync::Arc;

use super::addresses::endpoint_id;
use crate::event::{DirectEvent, Event};
use crate::net::direct::{DirectClient, DirectError};
use crate::wire::{self, Introduce, IntroduceAck, Nonce};

/// A direct request the loop queued, which a stopping node skips before sending it, or the answer to one that it applied, each with the cause of the request, as a test of the node reads them.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectExchange {
    Made {
        card: wire::PublicKey,
        kind: DirectContact,
        cause: Option<RequestCause>,
    },
    Answered {
        card: wire::PublicKey,
        kind: DirectContact,
        outcome: DirectOutcome,
        cause: Option<RequestCause>,
    },
}

/// The kind of a direct request the presence loop makes: an Introduce or a Probe, never a Message.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectContact {
    Introduce,
    Probe,
}

/// What the presence loop was taking when it made a direct request.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestCause {
    NeighborUp(wire::PublicKey),
    NeighborDown {
        card: wire::PublicKey,
        earlier_probes: usize,
    },
    Presence(wire::PublicKey),
    Departure(wire::PublicKey),
    Message {
        sender: wire::PublicKey,
        id: wire::MessageId,
    },
    Introduce(wire::PublicKey),
    SendFailed(wire::PublicKey),
    Answer {
        card: wire::PublicKey,
        kind: DirectContact,
    },
    HandleAssigned,
}

/// Whether a direct request was acknowledged or failed.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectOutcome {
    Acknowledged,
    Failed,
}

#[cfg(test)]
impl DirectOutcome {
    /// Acknowledged for an answer that holds a valid acknowledgement, failed otherwise.
    fn of<Acknowledgement>(answer: &Result<Acknowledgement, DirectError>) -> Self {
        match answer {
            Ok(_acknowledgement) => Self::Acknowledged,
            Err(_failure) => Self::Failed,
        }
    }
}

/// A direct request a loop hands to a task, so no dial holds the loop.
pub enum DirectRequest {
    DropHeldConnection(wire::PublicKey),
    SendIntroduce(wire::PublicKey),
    SendProbe(wire::PublicKey, Nonce),
}

#[cfg(test)]
impl DirectRequest {
    /// The request made for the cause as a test reads it, or none for a request that sends nothing.
    pub(super) fn made(&self, cause: Option<RequestCause>) -> Option<DirectExchange> {
        match self {
            Self::DropHeldConnection(_) => None,
            Self::SendIntroduce(card) => Some(DirectExchange::Made {
                card: *card,
                kind: DirectContact::Introduce,
                cause,
            }),
            Self::SendProbe(card, _nonce) => Some(DirectExchange::Made {
                card: *card,
                kind: DirectContact::Probe,
                cause,
            }),
        }
    }
}

/// The answer to a direct request a node sent, for the loop that sent it to apply.
pub enum DirectAnswer {
    Introduce {
        card: wire::PublicKey,
        answer: Result<IntroduceAck, DirectError>,
    },
    Probe {
        card: wire::PublicKey,
        answer: Result<(), DirectError>,
    },
}

impl DirectAnswer {
    /// The answer to a request made for the cause as a test reads it once applied: the card, the kind of request and whether it was acknowledged.
    #[cfg(test)]
    pub(super) fn answered(&self, cause: Option<RequestCause>) -> DirectExchange {
        match self {
            Self::Introduce { card, answer } => DirectExchange::Answered {
                card: *card,
                kind: DirectContact::Introduce,
                outcome: DirectOutcome::of(answer),
                cause,
            },
            Self::Probe { card, answer } => DirectExchange::Answered {
                card: *card,
                kind: DirectContact::Probe,
                outcome: DirectOutcome::of(answer),
                cause,
            },
        }
    }

    /// The cause of the requests this answer calls for.
    #[cfg(test)]
    pub(super) const fn as_cause(&self) -> RequestCause {
        match self {
            Self::Introduce { card, .. } => RequestCause::Answer {
                card: *card,
                kind: DirectContact::Introduce,
            },
            Self::Probe { card, .. } => RequestCause::Answer {
                card: *card,
                kind: DirectContact::Probe,
            },
        }
    }

    /// The event of a request that got no valid acknowledgement, naming the node it went to and why.
    pub(super) fn failure_record(&self) -> Option<Event> {
        match self {
            Self::Introduce {
                card,
                answer: Err(reason),
            }
            | Self::Probe {
                card,
                answer: Err(reason),
            } => endpoint_id(card).map(|peer| {
                DirectEvent::ContactFailed {
                    peer,
                    reason: reason.clone(),
                }
                .into()
            }),
            Self::Introduce { answer: Ok(_), .. } | Self::Probe { answer: Ok(()), .. } => None,
        }
    }
}

/// What sending direct requests needs from a node of any kind: whether it is stopping, its introduction, and where each answer goes.
pub trait DirectRequester: Send + Sync + 'static {
    /// Whether this node is stopping, so it sends no Introduce or Probe.
    fn is_stopping(&self) -> bool;

    /// The introduction this node sends, and answers an Introduce with.
    fn introduction(&self) -> Introduce;

    /// Hands the answer of a direct request this node sent to the loop that applies it.
    fn hand_answer(&self, answer: DirectAnswer);
}

/// Sends the direct requests in one task in their order, unless there are none, so no dial holds the loop.
pub fn send_in_background<R: DirectRequester>(
    requester: R,
    client: &Arc<DirectClient>,
    requests: Vec<DirectRequest>,
) {
    if !requests.is_empty() {
        tokio::spawn(run_requests(requester, Arc::clone(client), requests));
    }
}

/// Runs the requests in their order, handing each answer back to the loop, and sends no Introduce or Probe once the node is stopping.
async fn run_requests<R: DirectRequester>(
    requester: R,
    client: Arc<DirectClient>,
    requests: Vec<DirectRequest>,
) {
    for request in requests {
        match request {
            DirectRequest::DropHeldConnection(card) => {
                if let Some(id) = endpoint_id(&card) {
                    client.drop_held_connection(id).await;
                }
            }
            DirectRequest::SendIntroduce(_) | DirectRequest::SendProbe(..)
                if requester.is_stopping() => {}
            DirectRequest::SendIntroduce(card) => {
                let answer = match endpoint_id(&card) {
                    Some(id) => client.introduce(id, requester.introduction()).await,
                    None => Err(DirectError::Unreachable),
                };
                requester.hand_answer(DirectAnswer::Introduce { card, answer });
            }
            DirectRequest::SendProbe(card, nonce) => {
                let answer = match endpoint_id(&card) {
                    Some(id) => client.probe(id, nonce).await.map(|_acknowledged| ()),
                    None => Err(DirectError::Unreachable),
                };
                requester.hand_answer(DirectAnswer::Probe { card, answer });
            }
        }
    }
}
