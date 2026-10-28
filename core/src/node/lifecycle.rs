//! The session state machine of spec 3.9: Unclaimed, Claimed, Stopping, and the stopping sequence of spec 3.8.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::errors::ErrorCode;
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

/// Where the session stands in spec 3.9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Unclaimed,
    Claimed,
    Stopping,
}

struct Claimed {
    node: Arc<Node>,
    lock: HeldSidecar,
}

enum Phase {
    Unclaimed,
    Claimed(Box<Claimed>),
    Stopping,
}

/// The one card this process claims for its life, and the end of that life.
pub struct Session {
    phase: Mutex<Phase>,
}

impl Session {
    /// A session in which no card is claimed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Unclaimed),
        }
    }

    /// Where the session stands now.
    #[must_use]
    pub fn state(&self) -> SessionState {
        match &*self.phase() {
            Phase::Unclaimed => SessionState::Unclaimed,
            Phase::Claimed(_) => SessionState::Claimed,
            Phase::Stopping => SessionState::Stopping,
        }
    }

    /// Claims the card for the node, refusing a second claim with `bad_request` and a claim while Stopping with `refused`.
    ///
    /// # Errors
    /// `ErrorCode::BadRequest` when a card is claimed, `ErrorCode::Refused` when the session is stopping.
    pub fn claim(&self, node: Node, lock: HeldSidecar) -> Result<Arc<Node>, ErrorCode> {
        claim_in(&mut self.phase(), node, lock)
    }

    /// The claimed node, for every tool but `list_cards` and `init`.
    ///
    /// # Errors
    /// `ErrorCode::NotClaimed` until init succeeds, `ErrorCode::Refused` once the session is stopping.
    pub fn node(&self) -> Result<Arc<Node>, ErrorCode> {
        match &*self.phase() {
            Phase::Unclaimed => Err(ErrorCode::NotClaimed),
            Phase::Claimed(claimed) => Ok(Arc::clone(&claimed.node)),
            Phase::Stopping => Err(ErrorCode::Refused),
        }
    }

    fn phase(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the end of standard input or a signal, runs the stopping sequence of spec 3.8, and says how the process is to end.
    pub async fn run_until_exit(
        &self,
        end_of_input: oneshot::Receiver<()>,
        mut signals: mpsc::UnboundedReceiver<ShutdownSignal>,
        departure_wait: Duration,
    ) -> Exit {
        let runtime = first_stop(end_of_input, &mut signals).await;
        tokio::select! {
            () = self.stop(departure_wait) => Exit::Stopped(runtime),
            Some(_second_signal) = signals.recv() => Exit::AtOnce,
        }
    }

    /// Enters Stopping, handing the Departure over, releasing the lock, dropping held connections, then waits the departure wait.
    async fn stop(&self, departure_wait: Duration) {
        let Some(claimed) = self.enter_stopping() else {
            return;
        };
        claimed.node.start_stopping();
        drop(claimed.lock);
        claimed.node.drop_held_connections().await;
        sleep(departure_wait).await;
    }

    /// Takes the claim out of the session and leaves it Stopping.
    fn enter_stopping(&self) -> Option<Box<Claimed>> {
        let leaving = std::mem::replace(&mut *self.phase(), Phase::Stopping);
        match leaving {
            Phase::Claimed(claimed) => Some(claimed),
            Phase::Unclaimed | Phase::Stopping => None,
        }
    }
}

/// Claims the card in an Unclaimed phase and refuses it in any other.
fn claim_in(phase: &mut Phase, node: Node, lock: HeldSidecar) -> Result<Arc<Node>, ErrorCode> {
    match phase {
        Phase::Unclaimed => {
            let node = Arc::new(node);
            *phase = Phase::Claimed(Box::new(Claimed {
                node: Arc::clone(&node),
                lock,
            }));
            Ok(node)
        }
        Phase::Claimed(_) => Err(ErrorCode::BadRequest),
        Phase::Stopping => Err(ErrorCode::Refused),
    }
}

/// Waits for the end of standard input or the first signal, and says how the runtime is to shut down after it.
async fn first_stop(
    end_of_input: oneshot::Receiver<()>,
    signals: &mut mpsc::UnboundedReceiver<ShutdownSignal>,
) -> RuntimeShutdown {
    tokio::select! {
        _ended = end_of_input => RuntimeShutdown::Awaited,
        Some(signal) = signals.recv() => runtime_shutdown_after(signal),
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

/// Copies standard input to the MCP server's input and notes the end of standard input the moment it is read.
pub async fn forward_standard_input(
    input: impl AsyncRead + Unpin,
    server_input: impl AsyncWrite + Unpin,
    input_ended: oneshot::Sender<()>,
) {
    let (chunks, queued) = mpsc::unbounded_channel();
    tokio::join!(
        read_until_end(input, chunks, input_ended),
        write_queued(queued, server_input)
    );
}

/// Queues each chunk read from the input and signals the end the moment the input ends or fails.
async fn read_until_end(
    mut input: impl AsyncRead + Unpin,
    chunks: mpsc::UnboundedSender<Vec<u8>>,
    input_ended: oneshot::Sender<()>,
) {
    loop {
        let mut chunk = Vec::with_capacity(STANDARD_INPUT_CHUNK_BYTES);
        match input.read_buf(&mut chunk).await {
            Ok(0) | Err(_) => break,
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
    use std::time::Instant;

    use tokio::task::JoinHandle;

    use super::*;
    use crate::config::HostOrigin;
    use crate::net::direct::{ClientTimeouts, DirectClient};
    use crate::node::presence::tests::{
        FIRST_SEED, HOST_RELAY, HOST_SEED, INTRODUCE_ROUND_TRIP, MARKER_SEED, MOVED_HOST_RELAY,
        Member, MemberPlan, PAYLOAD_WAIT, SECOND_HANDLE, SECOND_SEED, SPEC_OBSERVER_WAIT, Scratch,
        WITNESS_HANDLE, WITNESS_SEED, Witness, bind, card, host_saved_within, infrastructure_of,
        keys, listed_within, marker_reaches, relay_map_holds, saved_host, start_pair,
        start_witnessed, status_within, wall_clock_ms,
    };
    use crate::node::presence::wire_addr;
    use crate::peers::Status;
    use crate::sidecar;
    use crate::wire::{Introduce, Nonce, Version};

    const STALLED_SERVER_CAPACITY_BYTES: usize = 1;
    const TYPED_BEFORE_THE_END: &[u8] = b"a tool call the server is still answering";
    const END_NOTED_WITHIN: Duration = Duration::from_millis(500);
    const TEST_DEPARTURE_WAIT: Duration = Duration::from_millis(2500);
    const LONG_DEPARTURE_WAIT: Duration = Duration::from_secs(60);
    const SPEC_GONE_WITHIN: Duration = Duration::from_millis(1000);
    const SPEC_DEPARTURE_WAIT: Duration = Duration::from_millis(5000);
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
        let (first, second) = start_pair(scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        let (sidecar_path, lock) = scratch.locked_card(SECOND_SEED);
        let session = Arc::new(Session::new());
        session.claim(second.node, lock).unwrap();
        ClaimedSecond {
            first,
            session,
            endpoint: second.endpoint,
            inbox_path: second.inbox_path,
            sidecar_path,
        }
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

    #[test]
    fn spec_3_14_the_departure_wait_is_5000_milliseconds() {
        assert_eq!(DEPARTURE_WAIT, SPEC_DEPARTURE_WAIT);
    }

    #[test]
    fn spec_3_9_before_init_the_session_is_unclaimed_and_every_tool_gets_not_claimed() {
        let session = Session::new();

        assert_eq!(session.state(), SessionState::Unclaimed);
        assert_eq!(session.node().err(), Some(ErrorCode::NotClaimed));
    }

    #[tokio::test]
    async fn spec_3_9_init_in_claimed_is_bad_request_and_the_first_claim_stays() {
        let scratch = Scratch::new("claimed-twice").await;
        let (first, second) = start_pair(&scratch).await;
        let (_, first_lock) = scratch.locked_card(FIRST_SEED);
        let (_, second_lock) = scratch.locked_card(SECOND_SEED);
        let session = Session::new();

        let claimed = session.claim(first.node, first_lock).unwrap();
        let second_claim = session.claim(second.node, second_lock);

        assert_eq!(second_claim.err(), Some(ErrorCode::BadRequest));
        assert_eq!(session.state(), SessionState::Claimed);
        assert!(Arc::ptr_eq(&session.node().unwrap(), &claimed));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_9_standard_input_reaches_the_server_in_order_and_its_end_closes_the_server_input()
     {
        let (mut standard_input, input) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (server_input, mut server_reads) = tokio::io::duplex(TYPED_BEFORE_THE_END.len());
        let (input_ended, ended) = oneshot::channel();
        tokio::spawn(forward_standard_input(input, server_input, input_ended));

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
        tokio::spawn(forward_standard_input(input, server_input, input_ended));

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
        tokio::spawn(forward_standard_input(input, server_input, input_ended));
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
    async fn spec_3_9_sigterm_starts_stopping_hands_over_the_departure_and_shuts_the_runtime_down_in_the_background()
     {
        let scratch = Scratch::new("sigterm").await;
        let second = claimed_second(&scratch).await;
        let stopping = run(&second.session, SHORT_DEPARTURE_WAIT);

        stopping.signals.send(ShutdownSignal::Terminate).unwrap();
        let gone = second_gone_at_first_within(&second.first, SPEC_GONE_WITHIN).await;

        assert!(gone);
        assert_eq!(
            stopping.exit.await.unwrap(),
            Exit::Stopped(RuntimeShutdown::InBackground)
        );
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
            assert_eq!(session.state(), SessionState::Stopping);
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
        let client = DirectClient::new(caller.endpoint().clone(), keys(), ClientTimeouts::SPEC);
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
        session.claim(member.node, lock).unwrap();
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

    #[tokio::test]
    async fn spec_3_8_stopping_drops_every_held_direct_connection() {
        let scratch = Scratch::new("held-connections").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, FIRST_MEMBER_PUBLIC),
            Some(WITNESS_HANDLE),
        )
        .await;
        let (_, lock) = scratch.locked_card(SECOND_SEED);
        let session = Arc::new(Session::new());
        session.claim(member.node, lock).unwrap();
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
