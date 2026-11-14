//! The session state machine of spec 3.9: Unclaimed, Claimed, Stopping, and the stopping sequence of spec 3.8.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::errors::ErrorCode;
use crate::event::{Event, ProcessEvent, RunRecord};
use crate::handle::{Handle, LocalHandle};
use crate::node::presence::Node;
use crate::sidecar::HeldSidecar;

const STANDARD_INPUT_CHUNK_BYTES: usize = 8192;

/// How long a stopping node stays up after handing over its Departure (spec 3.14).
pub const DEPARTURE_WAIT: Duration = Duration::from_millis(5000);

/// A signal that starts Stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    Terminate,
    Interrupt,
    Hangup,
}

/// How the runtime is to be shut down once the process may exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeShutdown {
    Awaited,
    InBackground,
}

/// How the process is to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    AtOnce,
    Stopped(RuntimeShutdown),
}

/// The card this session claimed and the effective handle its node assigned itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedCard {
    pub card: Handle,
    pub handle: LocalHandle,
}

struct Claimed {
    node: Arc<Node>,
    lock: HeldSidecar,
    identity: ClaimedCard,
}

enum Phase {
    Unclaimed,
    Claimed(Box<Claimed>),
    Stopping(Option<ClaimedCard>),
}

/// The one card this process claims for its life, and the end of that life.
pub struct Session {
    phase: Mutex<Phase>,
    record: RunRecord,
}

impl Session {
    /// A session in which no card is claimed, recording nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::recording_to(Arc::new(|_event: Event| {}))
    }

    /// A session in which no card is claimed, recording its claim and its stop to the run record.
    #[must_use]
    pub const fn recording_to(record: RunRecord) -> Self {
        Self {
            phase: Mutex::new(Phase::Unclaimed),
            record,
        }
    }

    /// Claims the card for the node, refusing a second claim with `bad_request` and a claim while Stopping with `refused`, and records the outcome.
    ///
    /// # Errors
    /// `ErrorCode::BadRequest` when a card is claimed, `ErrorCode::Refused` when the session is stopping.
    pub fn claim(
        &self,
        card: Handle,
        node: Node,
        lock: HeldSidecar,
    ) -> Result<Arc<Node>, ErrorCode> {
        match self.claimed_by(card, node, lock) {
            Ok((node, identity)) => {
                self.record(ProcessEvent::Claimed(identity));
                Ok(node)
            }
            Err(refusal) => {
                self.record(ProcessEvent::ClaimRefused {
                    code: refusal.code(),
                });
                Err(refusal)
            }
        }
    }

    /// Takes the claim into the session while it is Unclaimed, returning the node and what the claim names.
    fn claimed_by(
        &self,
        card: Handle,
        node: Node,
        lock: HeldSidecar,
    ) -> Result<(Arc<Node>, ClaimedCard), ErrorCode> {
        let mut phase = self.phase();
        match &*phase {
            Phase::Unclaimed => {
                let identity = ClaimedCard {
                    card,
                    handle: node.own_handle().clone(),
                };
                let node = Arc::new(node);
                *phase = Phase::Claimed(Box::new(Claimed {
                    node: Arc::clone(&node),
                    lock,
                    identity: identity.clone(),
                }));
                drop(phase);
                Ok((node, identity))
            }
            Phase::Claimed(_) => Err(ErrorCode::BadRequest),
            Phase::Stopping(_) => Err(ErrorCode::Refused),
        }
    }

    /// The card and handle this session claimed, kept while it stops, none before a claim.
    #[must_use]
    pub fn claimed_card(&self) -> Option<ClaimedCard> {
        match &*self.phase() {
            Phase::Unclaimed => None,
            Phase::Claimed(claimed) => Some(claimed.identity.clone()),
            Phase::Stopping(identity) => identity.clone(),
        }
    }

    /// The claimed node, for every tool but `list_cards` and `init`.
    ///
    /// # Errors
    /// `ErrorCode::NotClaimed` until init succeeds, `ErrorCode::Refused` once the session is stopping.
    pub fn node(&self) -> Result<Arc<Node>, ErrorCode> {
        match &*self.phase() {
            Phase::Unclaimed => Err(ErrorCode::NotClaimed),
            Phase::Claimed(claimed) => Ok(Arc::clone(&claimed.node)),
            Phase::Stopping(_) => Err(ErrorCode::Refused),
        }
    }

    /// Whether no card is claimed yet.
    pub fn is_unclaimed(&self) -> bool {
        matches!(*self.phase(), Phase::Unclaimed)
    }

    fn phase(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands the event to the run record.
    fn record(&self, event: ProcessEvent) {
        (self.record)(event.into());
    }

    /// Waits for the end of standard input or a signal, runs the stopping sequence of spec 3.8, and says how the process is to end.
    pub async fn run_until_exit(
        &self,
        end_of_input: oneshot::Receiver<()>,
        mut signals: mpsc::UnboundedReceiver<ShutdownSignal>,
        departure_wait: Duration,
    ) -> Exit {
        let runtime = self.first_stop(end_of_input, &mut signals).await;
        tokio::select! {
            () = self.stop(departure_wait) => Exit::Stopped(runtime),
            Some(signal) = signals.recv() => {
                self.record(ProcessEvent::SignalReceived { signal });
                Exit::AtOnce
            }
        }
    }

    /// Waits for the end of standard input or the first signal, records which came, and says how the runtime is to shut down after it.
    async fn first_stop(
        &self,
        end_of_input: oneshot::Receiver<()>,
        signals: &mut mpsc::UnboundedReceiver<ShutdownSignal>,
    ) -> RuntimeShutdown {
        tokio::select! {
            _ended = end_of_input => {
                self.record(ProcessEvent::InputEnded);
                RuntimeShutdown::Awaited
            }
            Some(signal) = signals.recv() => {
                self.record(ProcessEvent::SignalReceived { signal });
                runtime_shutdown_after(signal)
            }
        }
    }

    /// Enters Stopping, handing the Departure over, releasing the lock, dropping held connections, then waits the departure wait, recording each step as it ends.
    async fn stop(&self, departure_wait: Duration) {
        let Some(claimed) = self.enter_stopping() else {
            return;
        };
        claimed.node.start_stopping();
        self.record(ProcessEvent::DepartureHandedOver);
        drop(claimed.lock);
        self.record(ProcessEvent::LockReleased);
        claimed.node.drop_held_connections().await;
        self.record(ProcessEvent::HeldConnectionsDropped);
        sleep(departure_wait).await;
        self.record(ProcessEvent::DepartureWaitOver {
            waited: departure_wait,
        });
    }

    /// Takes the claim out of the session and leaves it Stopping.
    fn enter_stopping(&self) -> Option<Box<Claimed>> {
        let mut phase = self.phase();
        match std::mem::replace(&mut *phase, Phase::Stopping(None)) {
            Phase::Claimed(claimed) => {
                *phase = Phase::Stopping(Some(claimed.identity.clone()));
                Some(claimed)
            }
            Phase::Unclaimed => None,
            Phase::Stopping(identity) => {
                *phase = Phase::Stopping(identity);
                None
            }
        }
    }
}

/// Only SIGTERM has the runtime shut down in the background, so a pending read of standard input does not hold the exit.
const fn runtime_shutdown_after(signal: ShutdownSignal) -> RuntimeShutdown {
    match signal {
        ShutdownSignal::Terminate => RuntimeShutdown::InBackground,
        ShutdownSignal::Interrupt | ShutdownSignal::Hangup => RuntimeShutdown::Awaited,
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Copies standard input to the MCP server's input, buffering every unread chunk in memory, and notes the end of standard input the moment it is read or fails.
pub async fn forward_standard_input(
    input: impl AsyncRead + Unpin,
    server_input: impl AsyncWrite + Unpin,
    input_ended: oneshot::Sender<()>,
    record_failure: impl Fn(io::Error),
) {
    let (chunks, queued) = mpsc::unbounded_channel();
    tokio::join!(
        read_until_end(input, chunks, input_ended, record_failure),
        write_queued(queued, server_input)
    );
}

/// Queues each chunk read from the input and signals the end the moment the input ends, recording a failure to read it first.
async fn read_until_end(
    mut input: impl AsyncRead + Unpin,
    chunks: mpsc::UnboundedSender<Vec<u8>>,
    input_ended: oneshot::Sender<()>,
    record_failure: impl Fn(io::Error),
) {
    loop {
        let mut chunk = Vec::with_capacity(STANDARD_INPUT_CHUNK_BYTES);
        match input.read_buf(&mut chunk).await {
            Ok(0) => break,
            Err(failure) => {
                record_failure(failure);
                break;
            }
            Ok(_) => drop(chunks.send(chunk)),
        }
    }
    match input_ended.send(()) {
        Ok(()) | Err(()) => {}
    }
}

/// Writes the queued chunks to the server's input in order, then closes it, once the queue is closed and empty.
async fn write_queued(
    mut queued: mpsc::UnboundedReceiver<Vec<u8>>,
    mut server_input: impl AsyncWrite + Unpin,
) {
    while let Some(chunk) = queued.recv().await {
        if server_input.write_all(&chunk).await.is_err() {
            return;
        }
    }
    drop(server_input.shutdown().await);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Instant;

    use tokio::io::ReadBuf;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::config::HostOrigin;
    use crate::event::{Event, EventFields, RunRecord};
    use crate::net::direct::{ClientTimeouts, DirectClient};
    use crate::node::addresses::wire_addr;
    use crate::node::presence::tests::{
        FIRST_SEED, HOST_RELAY, HOST_SEED, INTRODUCE_ROUND_TRIP, MARKER_SEED, MOVED_HOST_RELAY,
        Member, MemberPlan, PAYLOAD_WAIT, SECOND_HANDLE, SECOND_SEED, SPEC_OBSERVER_WAIT, Scratch,
        WITNESS_HANDLE, WITNESS_SEED, Witness, bind, card, host_saved_within, infrastructure_of,
        keys, listed_within, marker_reaches, relay_map_holds, saved_host, start_pair,
        start_witnessed, status_within, wall_clock_ms,
    };
    use crate::peers::Status;
    use crate::sidecar;
    use crate::wire::{Introduce, Nonce, Version};

    const FIRST_CARD: &str = "first-card";
    const SECOND_CARD: &str = "second-card";
    const STALLED_SERVER_CAPACITY_BYTES: usize = 1;
    const TYPED_BEFORE_THE_END: &[u8] = b"a tool call the server is still answering";
    const END_NOTED_WITHIN: Duration = Duration::from_millis(500);
    const TEST_DEPARTURE_WAIT: Duration = Duration::from_millis(2500);
    const LONG_DEPARTURE_WAIT: Duration = Duration::from_secs(60);
    const SPEC_GONE_WITHIN: Duration = Duration::from_millis(1000);
    const STOPPING_SETTLED: Duration = Duration::from_millis(300);
    const EXITS_AT_ONCE_WITHIN: Duration = Duration::from_millis(1000);
    const SHORT_DEPARTURE_WAIT: Duration = Duration::from_millis(800);
    const POLL: Duration = Duration::from_millis(20);
    const SENT_AFTER_EXIT: &str = "a message to a stopped member";
    const SENT_WHILE_STOPPING: &str = "a message to a stopping member";
    const PROBE_NONCE: Nonce = [9; 16];
    const CONFIG_SAVED_WITHIN: Duration = Duration::from_millis(1000);
    const CONNECTION_WAIT: Duration = Duration::from_secs(5);
    const CONNECTIONS_CLOSED_WITHIN: Duration = Duration::from_millis(1000);
    const NOTHING_MORE_WITHIN: Duration = Duration::from_millis(1500);
    const SOCKET_REMOVED_WITHIN: Duration = Duration::from_secs(2);
    const FIRST_MEMBER_PUBLIC: &[u8] = br#"{"role":"first"}"#;
    const ALL_STOP_CAUSES: [(ShutdownSignal, RuntimeShutdown); 3] = [
        (ShutdownSignal::Terminate, RuntimeShutdown::InBackground),
        (ShutdownSignal::Interrupt, RuntimeShutdown::Awaited),
        (ShutdownSignal::Hangup, RuntimeShutdown::Awaited),
    ];

    fn card_named(name: &str) -> Handle {
        name.parse().unwrap()
    }

    struct Run {
        input_ended: oneshot::Sender<()>,
        signals: mpsc::UnboundedSender<ShutdownSignal>,
        exit: JoinHandle<Exit>,
    }

    fn run(session: &Arc<Session>, departure_wait: Duration) -> Run {
        let (input_ended, ended) = oneshot::channel();
        let (signals, signals_received) = mpsc::unbounded_channel();
        let session = Arc::clone(session);
        let exit = tokio::spawn(async move {
            session
                .run_until_exit(ended, signals_received, departure_wait)
                .await
        });
        Run {
            input_ended,
            signals,
            exit,
        }
    }

    struct ClaimedSecond {
        first: Member,
        session: Arc<Session>,
        endpoint: iroh::Endpoint,
        inbox_path: PathBuf,
        sidecar_path: PathBuf,
    }

    async fn claimed_second(scratch: &Scratch) -> ClaimedSecond {
        claimed_second_recording_to(scratch, Arc::new(|_event: Event| {})).await
    }

    async fn claimed_second_recording_to(scratch: &Scratch, record: RunRecord) -> ClaimedSecond {
        let (first, second) = start_pair(scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        let (sidecar_path, lock) = scratch.locked_card(SECOND_SEED);
        let session = Arc::new(Session::recording_to(record));
        session
            .claim(card_named(SECOND_CARD), second.node, lock)
            .unwrap();
        ClaimedSecond {
            first,
            session,
            endpoint: second.endpoint,
            inbox_path: second.inbox_path,
            sidecar_path,
        }
    }

    type RecordedEvent = (&'static str, EventFields);

    /// A run record handing each event's name and fields to the receiver, and that receiver.
    fn recording_events() -> (RunRecord, mpsc::UnboundedReceiver<RecordedEvent>) {
        let (recorded, received) = mpsc::unbounded_channel();
        let record: RunRecord = Arc::new(move |event: Event| {
            drop(recorded.send((event.name(), event.fields())));
        });
        (record, received)
    }

    /// Every event recorded so far that the receiver has not handed out yet.
    fn recorded_so_far(
        received: &mut mpsc::UnboundedReceiver<RecordedEvent>,
    ) -> Vec<RecordedEvent> {
        std::iter::from_fn(|| received.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn observability_decision_9_each_step_of_the_stopping_sequence_is_recorded_once_in_the_order_of_spec_3_8()
     {
        let scratch = Scratch::new("stop-steps").await;
        let (record, mut recorded) = recording_events();
        let second = claimed_second_recording_to(&scratch, record).await;
        recorded_so_far(&mut recorded);
        let stopping = run(&second.session, SHORT_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        stopping.exit.await.unwrap();

        assert_eq!(
            recorded_so_far(&mut recorded),
            vec![
                ("process.input_ended", vec![]),
                ("process.departure_handed_over", vec![]),
                ("process.lock_released", vec![]),
                ("process.held_connections_dropped", vec![]),
                (
                    "process.departure_wait_over",
                    vec![("wait_ms", serde_json::Value::from(800))]
                ),
            ]
        );
        drop(scratch);
    }

    /// The next event recorded, failing with its name when none is recorded within the wait.
    async fn next_recorded(
        received: &mut mpsc::UnboundedReceiver<RecordedEvent>,
        awaited: &str,
    ) -> RecordedEvent {
        tokio::time::timeout(EXITS_AT_ONCE_WITHIN, received.recv())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("{awaited} not recorded within {EXITS_AT_ONCE_WITHIN:?}")
            })
            .unwrap()
    }

    #[tokio::test]
    async fn observability_decision_9_the_input_that_starts_stopping_is_recorded_as_the_end_of_input_or_the_signal_by_name_spec_3_9()
     {
        let causes = [
            (
                Some(ShutdownSignal::Terminate),
                "process.signal_received",
                vec![("signal", serde_json::Value::from("SIGTERM"))],
            ),
            (
                Some(ShutdownSignal::Interrupt),
                "process.signal_received",
                vec![("signal", serde_json::Value::from("SIGINT"))],
            ),
            (
                Some(ShutdownSignal::Hangup),
                "process.signal_received",
                vec![("signal", serde_json::Value::from("SIGHUP"))],
            ),
            (None, "process.input_ended", vec![]),
        ];
        for (signal, name, fields) in causes {
            let (record, mut recorded) = recording_events();
            let session = Arc::new(Session::recording_to(record));
            let stopping = run(&session, LONG_DEPARTURE_WAIT);

            match signal {
                Some(signal) => stopping.signals.send(signal).unwrap(),
                None => stopping.input_ended.send(()).unwrap(),
            }
            tokio::time::timeout(EXITS_AT_ONCE_WITHIN, stopping.exit)
                .await
                .unwrap()
                .unwrap();

            assert_eq!(
                recorded_so_far(&mut recorded),
                vec![(name, fields)],
                "{signal:?}"
            );
        }
    }

    #[tokio::test]
    async fn observability_decision_9_a_second_signal_that_ends_the_stop_at_once_is_recorded_after_the_steps_already_taken_spec_3_9()
     {
        let scratch = Scratch::new("second-signal-event").await;
        let (record, mut recorded) = recording_events();
        let second = claimed_second_recording_to(&scratch, record).await;
        recorded_so_far(&mut recorded);
        let stopping = run(&second.session, LONG_DEPARTURE_WAIT);

        stopping.signals.send(ShutdownSignal::Interrupt).unwrap();
        let mut steps = Vec::new();
        for awaited in [
            "process.signal_received",
            "process.departure_handed_over",
            "process.lock_released",
            "process.held_connections_dropped",
        ] {
            steps.push(next_recorded(&mut recorded, awaited).await);
        }
        stopping.signals.send(ShutdownSignal::Hangup).unwrap();
        let exit = tokio::time::timeout(EXITS_AT_ONCE_WITHIN, stopping.exit)
            .await
            .unwrap()
            .unwrap();
        steps.extend(recorded_so_far(&mut recorded));

        assert_eq!(exit, Exit::AtOnce);
        assert_eq!(
            steps,
            vec![
                (
                    "process.signal_received",
                    vec![("signal", serde_json::Value::from("SIGINT"))]
                ),
                ("process.departure_handed_over", vec![]),
                ("process.lock_released", vec![]),
                ("process.held_connections_dropped", vec![]),
                (
                    "process.signal_received",
                    vec![("signal", serde_json::Value::from("SIGHUP"))]
                ),
            ]
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_claim_is_recorded_with_the_card_and_the_handle_it_assigned_and_a_refused_claim_with_its_code_spec_3_9()
     {
        let scratch = Scratch::new("claim-events").await;
        let (first, second) = start_pair(&scratch).await;
        let (_, first_lock) = scratch.locked_card(FIRST_SEED);
        let (_, second_lock) = scratch.locked_card(SECOND_SEED);
        let (record, mut recorded) = recording_events();
        let session = Session::recording_to(record);
        let claimed_handle = first.node.own_handle().as_str().to_owned();

        session
            .claim(card_named(FIRST_CARD), first.node, first_lock)
            .unwrap();
        let refused = session.claim(card_named(SECOND_CARD), second.node, second_lock);

        assert_eq!(refused.err(), Some(ErrorCode::BadRequest));

        assert_eq!(
            recorded_so_far(&mut recorded),
            vec![
                (
                    "process.claimed",
                    vec![
                        ("card", serde_json::Value::from(FIRST_CARD)),
                        ("handle", serde_json::Value::from(claimed_handle)),
                    ]
                ),
                (
                    "process.claim_refused",
                    vec![("code", serde_json::Value::from("bad_request"))]
                ),
            ]
        );
        drop(scratch);
    }

    async fn second_gone_at_first_within(first: &Member, within: Duration) -> bool {
        status_within(
            first,
            SECOND_SEED,
            |status| matches!(status, Status::Gone { .. }),
            within,
        )
        .await
        .is_some()
    }

    #[tokio::test]
    async fn spec_3_10_the_session_names_the_card_and_handle_it_claimed_before_and_after_it_stops()
    {
        let scratch = Scratch::new("claimed-card").await;
        let claimed = claimed_second(&scratch).await;
        let expected = ClaimedCard {
            card: card_named(SECOND_CARD),
            handle: claimed.session.node().unwrap().own_handle().clone(),
        };
        assert_eq!(claimed.session.claimed_card(), Some(expected.clone()));
        assert_eq!(Session::new().claimed_card(), None);

        let run = run(&claimed.session, TEST_DEPARTURE_WAIT);
        run.input_ended.send(()).unwrap();
        run.exit.await.unwrap();

        assert_eq!(claimed.session.claimed_card(), Some(expected));
        drop(scratch);
    }

    #[test]
    fn spec_3_9_before_init_the_session_is_unclaimed_and_every_tool_gets_not_claimed() {
        let session = Session::new();

        assert_eq!(session.node().err(), Some(ErrorCode::NotClaimed));
    }

    #[tokio::test]
    async fn spec_3_9_init_in_claimed_is_bad_request_and_the_first_claim_stays() {
        let scratch = Scratch::new("claimed-twice").await;
        let (first, second) = start_pair(&scratch).await;
        let (_, first_lock) = scratch.locked_card(FIRST_SEED);
        let (_, second_lock) = scratch.locked_card(SECOND_SEED);
        let session = Session::new();

        let claimed = session
            .claim(card_named(FIRST_CARD), first.node, first_lock)
            .unwrap();
        let second_claim = session.claim(card_named(SECOND_CARD), second.node, second_lock);

        assert_eq!(second_claim.err(), Some(ErrorCode::BadRequest));
        assert!(Arc::ptr_eq(&session.node().unwrap(), &claimed));
        drop(scratch);
    }

    fn no_record(_failure: io::Error) {}

    struct FailingInput(io::ErrorKind);

    impl AsyncRead for FailingInput {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::from(self.0)))
        }
    }

    #[tokio::test]
    async fn spec_3_9_a_failed_read_of_standard_input_is_recorded_and_ends_the_input() {
        let (server_input, _server_reads) = tokio::io::duplex(STALLED_SERVER_CAPACITY_BYTES);
        let (input_ended, ended) = oneshot::channel();
        let recorded = std::sync::Mutex::new(Vec::new());

        forward_standard_input(
            FailingInput(io::ErrorKind::ConnectionReset),
            server_input,
            input_ended,
            |failure: io::Error| recorded.lock().unwrap().push(failure.kind()),
        )
        .await;

        assert_eq!(ended.await, Ok(()));
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![io::ErrorKind::ConnectionReset]
        );
    }

    #[tokio::test]
    async fn spec_3_9_standard_input_reaches_the_server_in_order_and_its_end_closes_the_server_input()
     {
        let (mut standard_input, input) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (server_input, mut server_reads) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (input_ended, ended) = oneshot::channel();
        tokio::spawn(forward_standard_input(
            input,
            server_input,
            input_ended,
            no_record,
        ));

        tokio::io::AsyncWriteExt::write_all(&mut standard_input, TYPED_BEFORE_THE_END)
            .await
            .unwrap();
        drop(standard_input);
        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut server_reads, &mut received)
            .await
            .unwrap();

        assert_eq!(received, TYPED_BEFORE_THE_END);
        assert_eq!(ended.await, Ok(()));
    }

    #[tokio::test]
    async fn spec_3_9_the_end_of_standard_input_is_noted_at_once_while_the_server_is_not_reading() {
        let (mut standard_input, input) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (server_input, _server_not_reading) = tokio::io::duplex(STALLED_SERVER_CAPACITY_BYTES);
        let (input_ended, ended) = oneshot::channel();
        tokio::spawn(forward_standard_input(
            input,
            server_input,
            input_ended,
            no_record,
        ));

        tokio::io::AsyncWriteExt::write_all(&mut standard_input, TYPED_BEFORE_THE_END)
            .await
            .unwrap();
        drop(standard_input);

        let noted = tokio::time::timeout(END_NOTED_WITHIN, ended).await;
        assert_eq!(noted.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn test_4_a_member_stopped_at_the_end_of_input_is_seen_gone_within_1000_ms_and_a_send_to_it_is_unreachable()
     {
        let scratch = Scratch::new("t4-clean-stop").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        let gone = second_gone_at_first_within(&second.first, SPEC_GONE_WITHIN).await;
        let exit = stopping.exit.await.unwrap();
        drop(second.session);
        drop(second.endpoint);

        assert!(gone);
        assert_eq!(exit, Exit::Stopped(RuntimeShutdown::Awaited));
        assert_eq!(
            second
                .first
                .node
                .send(SECOND_HANDLE, SENT_AFTER_EXIT)
                .await
                .err(),
            Some(ErrorCode::Unreachable)
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn test_4_with_a_tool_call_in_flight_when_standard_input_closes_the_departure_still_goes_out_within_1000_ms()
     {
        let scratch = Scratch::new("t4-in-flight").await;
        let second = claimed_second(&scratch).await;
        let (mut standard_input, input) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (server_input, _server_busy_in_a_tool_call) =
            tokio::io::duplex(STALLED_SERVER_CAPACITY_BYTES);
        let (input_ended, ended) = oneshot::channel();
        tokio::spawn(forward_standard_input(
            input,
            server_input,
            input_ended,
            no_record,
        ));
        let (_signals, signals_received) = mpsc::unbounded_channel();
        let session = Arc::clone(&second.session);
        tokio::spawn(async move {
            session
                .run_until_exit(ended, signals_received, TEST_DEPARTURE_WAIT)
                .await
        });

        tokio::io::AsyncWriteExt::write_all(&mut standard_input, TYPED_BEFORE_THE_END)
            .await
            .unwrap();
        drop(standard_input);

        assert!(second_gone_at_first_within(&second.first, SPEC_GONE_WITHIN).await);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_9_sigterm_starts_stopping_and_hands_the_departure_over() {
        let scratch = Scratch::new("sigterm").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, SHORT_DEPARTURE_WAIT);

        stopping.signals.send(ShutdownSignal::Terminate).unwrap();
        let gone = second_gone_at_first_within(&second.first, SPEC_GONE_WITHIN).await;

        assert!(gone);
        stopping.exit.await.unwrap();
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_9_each_of_sigterm_sigint_and_sighup_starts_stopping_and_only_sigterm_shuts_down_in_the_background()
     {
        for (signal, runtime) in ALL_STOP_CAUSES {
            let session = Arc::new(Session::new());
            let stopping = run(&session, LONG_DEPARTURE_WAIT);

            stopping.signals.send(signal).unwrap();
            let exit = tokio::time::timeout(EXITS_AT_ONCE_WITHIN, stopping.exit)
                .await
                .unwrap()
                .unwrap();

            assert_eq!(exit, Exit::Stopped(runtime));
            assert_eq!(session.node().err(), Some(ErrorCode::Refused));
        }
    }

    #[tokio::test]
    async fn spec_3_9_a_second_signal_exits_at_once_whatever_the_departure_wait() {
        let scratch = Scratch::new("second-signal").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, LONG_DEPARTURE_WAIT);

        stopping.signals.send(ShutdownSignal::Interrupt).unwrap();
        tokio::time::sleep(STOPPING_SETTLED).await;
        stopping.signals.send(ShutdownSignal::Hangup).unwrap();

        let exit = tokio::time::timeout(EXITS_AT_ONCE_WITHIN, stopping.exit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::AtOnce);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_the_process_exits_no_sooner_than_the_departure_wait_after_stopping_began() {
        let scratch = Scratch::new("wait-before-exit").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, SHORT_DEPARTURE_WAIT);

        let began = Instant::now();
        stopping.input_ended.send(()).unwrap();
        stopping.exit.await.unwrap();

        assert!(began.elapsed() >= SHORT_DEPARTURE_WAIT);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_session_that_claimed_no_card_has_nothing_to_wait_for() {
        let session = Arc::new(Session::new());
        let stopping = run(&session, LONG_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();

        let exit = tokio::time::timeout(EXITS_AT_ONCE_WITHIN, stopping.exit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Stopped(RuntimeShutdown::Awaited));
    }

    #[tokio::test]
    async fn spec_3_8_while_stopping_a_message_is_refused_and_a_probe_and_an_introduce_go_unanswered()
     {
        let scratch = Scratch::new("stopping-answers").await;
        let second = claimed_second(&scratch).await;
        let caller = bind(WITNESS_SEED).await;
        caller.replace_address(second.endpoint.addr());
        let client = DirectClient::new(
            caller.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );
        let introduction = Introduce {
            v: Version::CURRENT,
            handle: None,
            public: Vec::new(),
            attached: false,
            addr: wire_addr(&caller.endpoint().addr()),
            mac: crate::payload::ZERO_MAC,
        };
        client
            .probe(second.endpoint.id(), PROBE_NONCE)
            .await
            .unwrap();
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        tokio::time::sleep(STOPPING_SETTLED).await;

        assert_eq!(
            second
                .first
                .node
                .send(SECOND_HANDLE, SENT_WHILE_STOPPING)
                .await
                .err(),
            Some(ErrorCode::Refused)
        );
        client
            .probe(second.endpoint.id(), PROBE_NONCE)
            .await
            .unwrap_err();
        client
            .introduce(second.endpoint.id(), introduction)
            .await
            .unwrap_err();
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_the_sidecar_lock_is_released_while_the_departure_wait_runs() {
        let scratch = Scratch::new("lock-released").await;
        let second = claimed_second(&scratch).await;
        assert!(sidecar::is_held(&second.sidecar_path).unwrap());
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        tokio::time::timeout(STOPPING_SETTLED, async {
            while sidecar::is_held(&second.sidecar_path).unwrap() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();

        assert!(!stopping.exit.is_finished());
        sidecar::claim(
            &second.sidecar_path,
            &card(SECOND_SEED),
            &sidecar::ThreadSleep,
        )
        .unwrap();
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_3_stopping_rewrites_the_configuration_when_a_held_record_is_newer_than_the_file()
     {
        let scratch = Scratch::new("config-on-stop").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, SECOND_HANDLE, FIRST_MEMBER_PUBLIC)
                .with_hosts(&[HOST_SEED]),
            None,
        )
        .await;
        let record_ts = wall_clock_ms() / 1000;
        let (_, lock) = scratch.locked_card(FIRST_SEED);
        let session = Arc::new(Session::new());
        session
            .claim(card_named(FIRST_CARD), member.node, lock)
            .unwrap();
        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, record_ts),
            )
            .await;
        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, MOVED_HOST_RELAY, record_ts + 1),
            )
            .await;
        tokio::time::timeout(PAYLOAD_WAIT, async {
            while !relay_map_holds(&member.endpoint, MOVED_HOST_RELAY).await {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();
        let before_stop = saved_host(&member.config_path, HOST_SEED);

        let stopping = run(&session, TEST_DEPARTURE_WAIT);
        stopping.input_ended.send(()).unwrap();
        let after_stop =
            host_saved_within(&member.config_path, HOST_SEED, CONFIG_SAVED_WITHIN).await;

        assert_eq!(before_stop.and_then(|host| host.ts), None);
        let saved = after_stop.unwrap();
        assert_eq!(saved.ts, Some(record_ts + 1));
        assert!(matches!(saved.origin, HostOrigin::Configured));
        drop(witness);
        drop(scratch);
    }

    async fn stopping_closes_the_connection_held_to_a_witness(
        tag: &str,
        witness_handle: Option<&'static str>,
    ) {
        let scratch = Scratch::new(tag).await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, FIRST_MEMBER_PUBLIC),
            witness_handle,
        )
        .await;
        let (_, lock) = scratch.locked_card(SECOND_SEED);
        let session = Arc::new(Session::new());
        session
            .claim(card_named(FIRST_CARD), member.node, lock)
            .unwrap();
        tokio::time::timeout(CONNECTION_WAIT, async {
            while witness.accepted.lock().unwrap().is_empty() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();
        let held: Vec<_> = witness.accepted.lock().unwrap().clone();
        assert!(
            held.iter()
                .all(|connection| connection.close_reason().is_none())
        );

        let stopping = run(&session, TEST_DEPARTURE_WAIT);
        stopping.input_ended.send(()).unwrap();

        tokio::time::timeout(CONNECTIONS_CLOSED_WITHIN, async {
            while held
                .iter()
                .any(|connection| connection.close_reason().is_none())
            {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_stopping_drops_the_held_connection_to_a_card_in_the_table() {
        stopping_closes_the_connection_held_to_a_witness("held-in-table", Some(WITNESS_HANDLE))
            .await;
    }

    #[tokio::test]
    async fn spec_3_8_stopping_drops_the_held_connection_to_a_card_outside_the_table() {
        stopping_closes_the_connection_held_to_a_witness("held-outside-table", None).await;
    }

    #[tokio::test]
    async fn spec_3_8_the_gossip_broadcast_stays_alive_while_stopping() {
        let scratch = Scratch::new("gossip-alive").await;
        let second = claimed_second(&scratch).await;
        let witness = Witness::joined_through(WITNESS_SEED, None, &second.endpoint).await;
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        marker_reaches(&second.first, &witness, MARKER_SEED).await;

        assert!(!stopping.exit.is_finished());
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_stopping_node_introduces_itself_to_no_one() {
        let scratch = Scratch::new("no-introduce").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);
        stopping.input_ended.send(()).unwrap();
        tokio::time::sleep(STOPPING_SETTLED).await;

        let witness =
            Witness::joined_through(WITNESS_SEED, Some(WITNESS_HANDLE), &second.endpoint).await;
        tokio::time::sleep(NOTHING_MORE_WITHIN).await;

        assert!(
            !witness
                .introducers
                .lock()
                .unwrap()
                .contains(&card(SECOND_SEED))
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_3_the_inbox_socket_file_is_gone_once_the_process_has_exited() {
        let scratch = Scratch::new("socket-removed").await;
        let second = claimed_second(&scratch).await;
        assert!(second.inbox_path.exists());
        let stopping = run(&second.session, SHORT_DEPARTURE_WAIT);

        stopping.input_ended.send(()).unwrap();
        stopping.exit.await.unwrap();

        tokio::time::timeout(SOCKET_REMOVED_WITHIN, async {
            while second.inbox_path.exists() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap();
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_stopping_node_announces_no_presence_after_its_departure() {
        let scratch = Scratch::new("no-presence-after").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, TEST_DEPARTURE_WAIT);
        stopping.input_ended.send(()).unwrap();
        assert!(second_gone_at_first_within(&second.first, SPEC_GONE_WITHIN).await);

        let _attaching_client = tokio::net::UnixStream::connect(&second.inbox_path)
            .await
            .unwrap();
        tokio::time::sleep(NOTHING_MORE_WITHIN).await;

        assert!(second_gone_at_first_within(&second.first, Duration::ZERO).await);
        drop(scratch);
    }
}
