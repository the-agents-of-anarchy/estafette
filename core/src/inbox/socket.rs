use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::dirs::Directories;
use crate::inbox::buffer::{InboxBuffer, InboxFrame, ReceivedMessage};

const SOCKET_MODE: u32 = 0o600;
const MAX_REQUEST_LINE_BYTES: u64 = 64;
const DISCARDED_INPUT_CHUNK_BYTES: usize = 64;
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
const FRAME_TERMINATOR: u8 = b'\n';
const PROCESS_STATUS_PROGRAM: &str = "ps";
const PROCESS_STATUS_PROCESS_ID_FLAG: &str = "-p";

/// Answers whether a process with a given id exists, supplied by the composition root.
pub trait ProcessTable {
    /// Whether a process with this id is running.
    fn is_running(&self, process_id: u32) -> bool;
}

/// The inbox socket could not be made ready.
#[derive(Debug)]
pub enum InboxSocketError {
    ClearStaleSockets { path: PathBuf, source: io::Error },
    Bind { path: PathBuf, source: io::Error },
    RestrictPermissions { path: PathBuf, source: io::Error },
}

impl fmt::Display for InboxSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClearStaleSockets { path, source } => {
                write!(
                    formatter,
                    "cannot clear stale sockets in {}: {source}",
                    path.display()
                )
            }
            Self::Bind { path, source } => {
                write!(formatter, "cannot listen on {}: {source}", path.display())
            }
            Self::RestrictPermissions { path, source } => {
                write!(
                    formatter,
                    "cannot restrict {} to its owner: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for InboxSocketError {}

/// The operating system's process table, read through `ps -p`.
pub struct OperatingSystemProcessTable;

impl ProcessTable for OperatingSystemProcessTable {
    fn is_running(&self, process_id: u32) -> bool {
        Command::new(PROCESS_STATUS_PROGRAM)
            .args([PROCESS_STATUS_PROCESS_ID_FLAG, &process_id.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SinceRequest {
    since: u64,
}

/// How many times accepting a client failed, and the kind of the latest failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcceptFailures {
    pub count: u64,
    pub last_kind: Option<io::ErrorKind>,
}

trait ClientAcceptor: Send + 'static {
    fn accept_client(&self) -> impl Future<Output = io::Result<UnixStream>> + Send;
}

impl ClientAcceptor for UnixListener {
    async fn accept_client(&self) -> io::Result<UnixStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

struct Shared {
    buffer: Mutex<InboxBuffer>,
    newest_seq: watch::Sender<u64>,
    accept_failures: watch::Sender<AcceptFailures>,
    clients: watch::Sender<usize>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Attachment {
    shared: Arc<Shared>,
}

impl Attachment {
    fn begin(shared: Arc<Shared>) -> Self {
        shared
            .clients
            .send_modify(|clients| *clients = clients.saturating_add(1));
        Self { shared }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.shared
            .clients
            .send_modify(|clients| *clients = clients.saturating_sub(1));
    }
}

/// Whether at least one client is connected, and the moments that changes.
pub struct Attached {
    clients: watch::Receiver<usize>,
    observed: bool,
}

impl Attached {
    fn watching(clients: watch::Receiver<usize>) -> Self {
        let observed = *clients.borrow() > 0;
        Self { clients, observed }
    }

    /// Whether at least one client is connected now.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        *self.clients.borrow() > 0
    }

    /// Waits for the first connect or the last disconnect and returns the new state.
    ///
    /// # Errors
    ///
    /// Fails when the socket has been dropped.
    pub async fn changed(&mut self) -> Result<bool, watch::error::RecvError> {
        loop {
            self.clients.changed().await?;
            let now = self.is_attached();
            if now != self.observed {
                self.observed = now;
                return Ok(now);
            }
        }
    }
}

/// The inbox socket of one process: listening, replaying and streaming frames, removed when dropped.
pub struct InboxSocket {
    socket_path: PathBuf,
    shared: Arc<Shared>,
    accept_task: JoinHandle<()>,
}

impl InboxSocket {
    /// Removes stale sockets, listens on this process's socket, and serves clients; must run inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// Fails when a stale socket cannot be removed or the socket cannot be bound and restricted to 0600.
    pub fn start(
        directories: &Directories,
        process_id: u32,
        processes: &impl ProcessTable,
    ) -> Result<Self, InboxSocketError> {
        let socket_path = directories.inbox_socket_file(process_id);
        remove_stale_sockets(directories, &socket_path, process_id, processes)?;
        let listener = bind_owner_only(&socket_path)?;
        Ok(Self::serving(socket_path, listener))
    }

    fn serving(socket_path: PathBuf, acceptor: impl ClientAcceptor) -> Self {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(InboxBuffer::new()),
            newest_seq: watch::Sender::new(0),
            accept_failures: watch::Sender::default(),
            clients: watch::Sender::new(0),
        });
        let accept_task = tokio::spawn(accept_clients(acceptor, Arc::clone(&shared)));
        Self {
            socket_path,
            shared,
            accept_task,
        }
    }

    /// Numbers the message, buffers it, and streams it to every attached client.
    pub fn publish(&self, message: ReceivedMessage) {
        let frame = locked(&self.shared.buffer).push(message);
        self.shared.newest_seq.send_replace(frame.seq);
    }

    /// The count and latest kind of failures to accept a client, which the node must surface.
    #[must_use]
    pub fn accept_failures(&self) -> watch::Receiver<AcceptFailures> {
        self.shared.accept_failures.subscribe()
    }

    /// Whether at least one client is connected, updated on the first connect and the last disconnect.
    #[must_use]
    pub fn attached(&self) -> Attached {
        Attached::watching(self.shared.clients.subscribe())
    }
}

impl Drop for InboxSocket {
    fn drop(&mut self) {
        self.accept_task.abort();
        drop(fs::remove_file(&self.socket_path));
    }
}

fn remove_stale_sockets(
    directories: &Directories,
    own_socket_path: &Path,
    own_process_id: u32,
    processes: &impl ProcessTable,
) -> Result<(), InboxSocketError> {
    let Some(runtime) = own_socket_path.parent() else {
        return Ok(());
    };
    let clear_failure = |source| InboxSocketError::ClearStaleSockets {
        path: runtime.to_path_buf(),
        source,
    };
    for entry in fs::read_dir(runtime).map_err(clear_failure)? {
        let path = entry.map_err(clear_failure)?.path();
        let Some(process_id) = socket_owner_process_id(directories, &path) else {
            continue;
        };
        if process_id == own_process_id || !processes.is_running(process_id) {
            fs::remove_file(&path).map_err(clear_failure)?;
        }
    }
    Ok(())
}

fn socket_owner_process_id(directories: &Directories, path: &Path) -> Option<u32> {
    let process_id: u32 = path.file_stem()?.to_str()?.parse().ok()?;
    (directories.inbox_socket_file(process_id) == path).then_some(process_id)
}

fn bind_owner_only(socket_path: &Path) -> Result<UnixListener, InboxSocketError> {
    let listener = UnixListener::bind(socket_path).map_err(|source| InboxSocketError::Bind {
        path: socket_path.to_path_buf(),
        source,
    })?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(SOCKET_MODE)).map_err(
        |source| {
            drop(fs::remove_file(socket_path));
            InboxSocketError::RestrictPermissions {
                path: socket_path.to_path_buf(),
                source,
            }
        },
    )?;
    Ok(listener)
}

async fn accept_clients(acceptor: impl ClientAcceptor, shared: Arc<Shared>) {
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            accepted = acceptor.accept_client() => match accepted {
                Ok(stream) => {
                    let attachment = Attachment::begin(Arc::clone(&shared));
                    clients.spawn(serve_client(stream, Arc::clone(&shared), attachment));
                }
                Err(failure) => {
                    shared.accept_failures.send_modify(|failures| {
                        failures.count = failures.count.saturating_add(1);
                        failures.last_kind = Some(failure.kind());
                    });
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                }
            },
            Some(_) = clients.join_next() => {}
        }
    }
}

async fn serve_client(stream: UnixStream, shared: Arc<Shared>, _attachment: Attachment) {
    let mut newest_seq = shared.newest_seq.subscribe();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let Some(mut last_sent) = read_since(&mut reader).await else {
        return;
    };
    loop {
        let frames = locked(&shared.buffer).frames_since(last_sent);
        for frame in &frames {
            if write_frame(&mut write_half, frame).await.is_err() {
                return;
            }
            last_sent = frame.seq;
        }
        tokio::select! {
            changed = newest_seq.changed() => if changed.is_err() { return; },
            () = wait_for_disconnect(&mut reader) => return,
        }
    }
}

async fn read_since(reader: &mut BufReader<impl AsyncRead + Unpin>) -> Option<u64> {
    let mut line = String::new();
    let read = reader
        .take(MAX_REQUEST_LINE_BYTES)
        .read_line(&mut line)
        .await
        .ok()?;
    if read == 0 {
        return None;
    }
    serde_json::from_str::<SinceRequest>(&line)
        .ok()
        .map(|request| request.since)
}

async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), frame: &InboxFrame) -> io::Result<()> {
    let mut line = serde_json::to_vec(frame)?;
    line.push(FRAME_TERMINATOR);
    writer.write_all(&line).await
}

async fn wait_for_disconnect(reader: &mut BufReader<impl AsyncRead + Unpin>) {
    let mut discarded = [0_u8; DISCARDED_INPUT_CHUNK_BYTES];
    while reader.read(&mut discarded).await.is_ok_and(|read| read > 0) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirs::OperatingSystemFilesystem;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    const OWN_PROCESS_ID: u32 = 4_242;
    const READ_DEADLINE: Duration = Duration::from_secs(5);
    const SENT_AT_BASE: u64 = 1_000;
    const RECEIVED_AT_BASE: u64 = 2_000;

    struct RunningProcesses(Vec<u32>);

    impl ProcessTable for RunningProcesses {
        fn is_running(&self, process_id: u32) -> bool {
            self.0.contains(&process_id)
        }
    }

    fn scratch_directories(test_name: &str) -> Directories {
        let root = std::env::temp_dir().join(format!("e15-{test_name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let environment = move |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(root.join("config").into_os_string()),
            "XDG_DATA_HOME" => Some(root.join("data").into_os_string()),
            "XDG_RUNTIME_DIR" => Some(root.join("run").into_os_string()),
            _ => None::<OsString>,
        };
        Directories::establish(environment, &OperatingSystemFilesystem, None).unwrap()
    }

    fn message_number(n: u64) -> ReceivedMessage {
        ReceivedMessage {
            id: format!("id-{n}"),
            from: format!("from-{n}"),
            body: format!("body-{n}"),
            sent_at: SENT_AT_BASE.saturating_add(n),
            received_at: RECEIVED_AT_BASE.saturating_add(n),
        }
    }

    fn started(directories: &Directories) -> InboxSocket {
        InboxSocket::start(
            directories,
            OWN_PROCESS_ID,
            &RunningProcesses(vec![OWN_PROCESS_ID]),
        )
        .unwrap()
    }

    async fn connect_with_since(socket_path: &Path, since: u64) -> BufReader<UnixStream> {
        let mut stream = UnixStream::connect(socket_path).await.unwrap();
        stream
            .write_all(format!("{{\"since\": {since}}}\n").as_bytes())
            .await
            .unwrap();
        BufReader::new(stream)
    }

    async fn next_seq(client: &mut BufReader<UnixStream>) -> u64 {
        let mut line = String::new();
        tokio::time::timeout(READ_DEADLINE, client.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["seq"]
            .as_u64()
            .unwrap()
    }

    #[tokio::test]
    async fn spec_3_15_test_11_since_3_after_5_frames_replays_4_and_5_then_live_6() {
        let directories = scratch_directories("test11");
        let socket = started(&directories);
        for n in 1..=5 {
            socket.publish(message_number(n));
        }
        let mut client =
            connect_with_since(&directories.inbox_socket_file(OWN_PROCESS_ID), 3).await;
        assert_eq!(next_seq(&mut client).await, 4);
        assert_eq!(next_seq(&mut client).await, 5);
        socket.publish(message_number(6));
        assert_eq!(next_seq(&mut client).await, 6);
    }

    #[tokio::test]
    async fn spec_3_11_each_client_gets_its_own_replay_from_its_own_since() {
        let directories = scratch_directories("two-clients");
        let socket = started(&directories);
        for n in 1..=5 {
            socket.publish(message_number(n));
        }
        let path = directories.inbox_socket_file(OWN_PROCESS_ID);
        let mut from_start = connect_with_since(&path, 0).await;
        let mut from_four = connect_with_since(&path, 4).await;
        assert_eq!(next_seq(&mut from_four).await, 5);
        for expected in 1..=5 {
            assert_eq!(next_seq(&mut from_start).await, expected);
        }
        socket.publish(message_number(6));
        assert_eq!(next_seq(&mut from_four).await, 6);
        assert_eq!(next_seq(&mut from_start).await, 6);
    }

    #[tokio::test]
    async fn spec_3_11_a_frame_carries_the_six_spec_keys() {
        let directories = scratch_directories("frame-keys");
        let socket = started(&directories);
        socket.publish(message_number(7));
        let mut client =
            connect_with_since(&directories.inbox_socket_file(OWN_PROCESS_ID), 0).await;
        let mut line = String::new();
        tokio::time::timeout(READ_DEADLINE, client.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            serde_json::json!({
                "seq": 1, "id": "id-7", "from": "from-7", "body": "body-7",
                "sent_at": 1_007, "received_at": 2_007,
            })
        );
    }

    #[tokio::test]
    async fn spec_3_11_attached_is_true_from_the_first_connect_to_the_last_disconnect() {
        let directories = scratch_directories("attached");
        let socket = started(&directories);
        let mut attached = socket.attached();
        assert!(!attached.is_attached());
        let path = directories.inbox_socket_file(OWN_PROCESS_ID);
        let first = connect_with_since(&path, 0).await;
        assert!(
            tokio::time::timeout(READ_DEADLINE, attached.changed())
                .await
                .unwrap()
                .unwrap()
        );
        let mut second = connect_with_since(&path, 0).await;
        drop(first);
        socket.publish(message_number(1));
        assert_eq!(next_seq(&mut second).await, 1);
        assert!(attached.is_attached());
        drop(second);
        assert!(
            !tokio::time::timeout(READ_DEADLINE, attached.changed())
                .await
                .unwrap()
                .unwrap()
        );
    }

    #[tokio::test]
    async fn spec_3_11_a_request_that_is_not_since_json_closes_the_connection_with_nothing_written()
    {
        let directories = scratch_directories("bad-request");
        let _socket = started(&directories);
        let mut stream = UnixStream::connect(directories.inbox_socket_file(OWN_PROCESS_ID))
            .await
            .unwrap();
        stream.write_all(b"{\"since\": \"three\"}\n").await.unwrap();
        let mut received = Vec::new();
        tokio::time::timeout(
            READ_DEADLINE,
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(received.len(), 0);
    }

    #[tokio::test]
    async fn spec_3_11_the_socket_is_mode_0600_and_gone_after_exit() {
        let directories = scratch_directories("mode-and-exit");
        let socket = started(&directories);
        let path = directories.inbox_socket_file(OWN_PROCESS_ID);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(socket);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn spec_3_3_start_removes_sockets_of_dead_processes_and_keeps_live_ones() {
        const DEAD: u32 = 7_001;
        const LIVE: u32 = 7_002;
        let directories = scratch_directories("stale");
        let own = directories.inbox_socket_file(OWN_PROCESS_ID);
        let runtime = own.parent().unwrap().to_path_buf();
        let dead = directories.inbox_socket_file(DEAD);
        let live = directories.inbox_socket_file(LIVE);
        let unrelated = [
            runtime.join("notes.txt"),
            runtime.join("abc.sock"),
            runtime.join("7003.sock.bak"),
        ];
        for path in [&dead, &live, &own].into_iter().chain(unrelated.iter()) {
            fs::write(path, b"").unwrap();
        }
        let processes = RunningProcesses(vec![OWN_PROCESS_ID, LIVE]);
        let _socket = InboxSocket::start(&directories, OWN_PROCESS_ID, &processes).unwrap();
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(unrelated.iter().all(|path| path.exists()));
        UnixStream::connect(&own).await.unwrap();
    }

    struct FailingAcceptor(io::ErrorKind);

    impl ClientAcceptor for FailingAcceptor {
        fn accept_client(&self) -> impl Future<Output = io::Result<UnixStream>> + Send {
            std::future::ready(Err(io::Error::from(self.0)))
        }
    }

    #[tokio::test]
    async fn spec_3_11_an_accept_failure_reaches_the_handle_counted_with_its_kind() {
        let directories = scratch_directories("accept-failure");
        let socket_path = directories.inbox_socket_file(OWN_PROCESS_ID);
        let socket = InboxSocket::serving(socket_path, FailingAcceptor(io::ErrorKind::OutOfMemory));
        let mut failures = socket.accept_failures();
        assert_eq!(*failures.borrow(), AcceptFailures::default());
        tokio::time::timeout(READ_DEADLINE, failures.changed())
            .await
            .unwrap()
            .unwrap();
        let first = *failures.borrow_and_update();
        assert_eq!(first.last_kind, Some(io::ErrorKind::OutOfMemory));
        tokio::time::timeout(READ_DEADLINE, failures.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failures.borrow().count, first.count.saturating_add(1));
    }
}
