use std::convert::Infallible;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
const NO_FRAME_SEEN_YET: u64 = 0;
const FRAME_LINE_TERMINATOR: char = '\n';

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InboxFrame {
    pub seq: u64,
    pub id: String,
    pub from: String,
    pub body: String,
    pub sent_at: u64,
    pub received_at: u64,
}

#[derive(Serialize)]
struct ResumeRequest {
    since: u64,
}

#[derive(Debug)]
pub enum InboxClientError {
    ConnectFailed(io::Error),
    ResumeRequestFailed(io::Error),
    ReadFailed(io::Error),
    MalformedFrame {
        line: String,
        reason: serde_json::Error,
    },
}

impl fmt::Display for InboxClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectFailed(source) => {
                write!(formatter, "cannot connect to the inbox socket: {source}")
            }
            Self::ResumeRequestFailed(source) => write!(
                formatter,
                "cannot send the since line to the inbox socket: {source}"
            ),
            Self::ReadFailed(source) => write!(formatter, "cannot read the inbox socket: {source}"),
            Self::MalformedFrame { line, reason } => write!(
                formatter,
                "the inbox socket sent a frame it cannot read ({reason}): {line}"
            ),
        }
    }
}

impl std::error::Error for InboxClientError {}

pub trait InboxConnector {
    type Stream: Read + Write;

    /// Opens one connection to an inbox socket.
    /// # Errors
    /// Returns the `io::Error` of the failed connection.
    fn connect(&mut self) -> io::Result<Self::Stream>;
}

pub struct UnixSocketConnector {
    socket_path: PathBuf,
}

impl UnixSocketConnector {
    /// Names the inbox socket every connection goes to.
    #[must_use]
    pub const fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

impl InboxConnector for UnixSocketConnector {
    type Stream = UnixStream;

    fn connect(&mut self) -> io::Result<UnixStream> {
        UnixStream::connect(&self.socket_path)
    }
}

/// Delivers each inbox frame once, in order, reconnecting with the last seq seen whenever the socket closes.
/// # Errors
/// Returns the `InboxClientError` that ended the following; a closed socket alone never ends it.
pub fn follow_inbox<C: InboxConnector>(
    connector: &mut C,
    wait_before_reconnect: &mut impl FnMut(Duration),
    deliver_frame: &mut impl FnMut(&InboxFrame),
) -> Result<Infallible, InboxClientError> {
    let mut last_seq = NO_FRAME_SEEN_YET;
    loop {
        let stream = connector
            .connect()
            .map_err(InboxClientError::ConnectFailed)?;
        last_seq = deliver_until_close(stream, last_seq, deliver_frame)?;
        wait_before_reconnect(RECONNECT_PAUSE);
    }
}

fn deliver_until_close(
    mut stream: impl Read + Write,
    since: u64,
    deliver_frame: &mut impl FnMut(&InboxFrame),
) -> Result<u64, InboxClientError> {
    send_resume_request(&mut stream, since).map_err(InboxClientError::ResumeRequestFailed)?;
    let mut reader = BufReader::new(stream);
    let mut last_seq = since;
    while let Some(line) = read_complete_line(&mut reader)? {
        let frame = parse_frame(&line)?;
        if frame.seq > last_seq {
            last_seq = frame.seq;
            deliver_frame(&frame);
        }
    }
    Ok(last_seq)
}

fn send_resume_request(stream: &mut impl Write, since: u64) -> io::Result<()> {
    let mut request = serde_json::to_string(&ResumeRequest { since })?;
    request.push(FRAME_LINE_TERMINATOR);
    stream.write_all(request.as_bytes())?;
    stream.flush()
}

fn read_complete_line(reader: &mut impl BufRead) -> Result<Option<String>, InboxClientError> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(InboxClientError::ReadFailed)?;
    Ok(line.strip_suffix(FRAME_LINE_TERMINATOR).map(str::to_owned))
}

fn parse_frame(line: &str) -> Result<InboxFrame, InboxClientError> {
    serde_json::from_str(line).map_err(|reason| InboxClientError::MalformedFrame {
        line: line.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::rc::Rc;

    struct ScriptedStream {
        replies: Cursor<Vec<u8>>,
        requests: Rc<RefCell<Vec<String>>>,
        request_bytes: Vec<u8>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.replies.read(buffer)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.request_bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.requests.borrow_mut().push(
                String::from_utf8_lossy(&std::mem::take(&mut self.request_bytes)).into_owned(),
            );
            Ok(())
        }
    }

    struct ScriptedConnector {
        connections: VecDeque<String>,
        requests: Rc<RefCell<Vec<String>>>,
    }

    impl InboxConnector for ScriptedConnector {
        type Stream = ScriptedStream;

        fn connect(&mut self) -> io::Result<ScriptedStream> {
            let replies = self
                .connections
                .pop_front()
                .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))?;
            Ok(ScriptedStream {
                replies: Cursor::new(replies.into_bytes()),
                requests: Rc::clone(&self.requests),
                request_bytes: Vec::new(),
            })
        }
    }

    struct Followed {
        requests: Vec<String>,
        delivered: Vec<InboxFrame>,
        pauses: Vec<Duration>,
        ended_with: InboxClientError,
    }

    const TEST_MILLIS_PER_SEQ: u64 = 1000;
    const TEST_SENT_AT_OFFSET: u64 = 1;
    const TEST_RECEIVED_AT_OFFSET: u64 = 2;

    fn sent_at_of(seq: u64) -> u64 {
        seq.saturating_mul(TEST_MILLIS_PER_SEQ)
            .saturating_add(TEST_SENT_AT_OFFSET)
    }

    fn received_at_of(seq: u64) -> u64 {
        seq.saturating_mul(TEST_MILLIS_PER_SEQ)
            .saturating_add(TEST_RECEIVED_AT_OFFSET)
    }

    fn frame_line(seq: u64) -> String {
        format!(
            "{{\"seq\":{seq},\"id\":\"id-{seq}\",\"from\":\"from-{seq}\",\"body\":\"body {seq}\",\"sent_at\":{},\"received_at\":{}}}\n",
            sent_at_of(seq),
            received_at_of(seq)
        )
    }

    fn frame(seq: u64) -> InboxFrame {
        InboxFrame {
            seq,
            id: format!("id-{seq}"),
            from: format!("from-{seq}"),
            body: format!("body {seq}"),
            sent_at: sent_at_of(seq),
            received_at: received_at_of(seq),
        }
    }

    fn follow_scripted(connections: &[String]) -> Followed {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut connector = ScriptedConnector {
            connections: connections.iter().cloned().collect(),
            requests: Rc::clone(&requests),
        };
        let mut delivered = Vec::new();
        let mut pauses = Vec::new();
        let ended_with = follow_inbox(
            &mut connector,
            &mut |pause| pauses.push(pause),
            &mut |received| delivered.push(received.clone()),
        )
        .expect_err("a followed inbox never returns Ok");
        let requests = requests.borrow().clone();
        Followed {
            requests,
            delivered,
            pauses,
            ended_with,
        }
    }

    #[test]
    fn spec_3_11_first_connection_asks_since_zero_and_delivers_replayed_frames_in_order() {
        let followed = follow_scripted(&[format!("{}{}", frame_line(7), frame_line(9))]);
        assert_eq!(followed.requests[0], "{\"since\":0}\n");
        assert_eq!(followed.delivered, vec![frame(7), frame(9)]);
    }

    #[test]
    fn spec_3_12_step_5_reconnect_asks_since_the_last_seq_seen() {
        let followed = follow_scripted(&[
            format!("{}{}", frame_line(3), frame_line(4)),
            frame_line(8),
            String::new(),
        ]);
        assert_eq!(
            followed.requests,
            vec!["{\"since\":0}\n", "{\"since\":4}\n", "{\"since\":8}\n"]
        );
        assert_eq!(followed.delivered, vec![frame(3), frame(4), frame(8)]);
    }

    #[test]
    fn spec_3_12_step_5_a_frame_replayed_after_reconnect_is_delivered_once() {
        let followed = follow_scripted(&[
            format!("{}{}", frame_line(3), frame_line(4)),
            format!("{}{}", frame_line(4), frame_line(5)),
        ]);
        assert_eq!(followed.delivered, vec![frame(3), frame(4), frame(5)]);
    }

    #[test]
    fn spec_3_11_a_line_cut_off_by_the_close_is_dropped_and_resent_after_reconnect() {
        let followed = follow_scripted(&[
            format!("{}{{\"seq\":4,\"id\":\"id-4\",\"fr", frame_line(3)),
            frame_line(4),
        ]);
        assert_eq!(followed.requests[1], "{\"since\":3}\n");
        assert_eq!(followed.delivered, vec![frame(3), frame(4)]);
    }

    #[test]
    fn spec_3_12_step_5_waits_the_reconnect_pause_before_each_reconnect() {
        let followed = follow_scripted(&[frame_line(1), frame_line(2)]);
        assert_eq!(followed.pauses, vec![RECONNECT_PAUSE, RECONNECT_PAUSE]);
    }

    #[test]
    fn spec_3_12_step_3_a_refused_connection_ends_the_following() {
        let followed = follow_scripted(&[]);
        assert!(matches!(
            followed.ended_with,
            InboxClientError::ConnectFailed(_)
        ));
        assert_eq!(followed.pauses, Vec::<Duration>::new());
    }

    #[test]
    fn spec_3_11_a_frame_that_is_not_json_ends_the_following() {
        let followed = follow_scripted(&[format!("{}not json\n", frame_line(1))]);
        assert!(matches!(
            &followed.ended_with,
            InboxClientError::MalformedFrame { line, .. } if line == "not json"
        ));
        assert_eq!(followed.delivered, vec![frame(1)]);
    }

    fn serve_reply(
        listener: &std::os::unix::net::UnixListener,
        reply: &str,
    ) -> (String, std::os::unix::net::UnixStream) {
        let (mut connection, _) = listener.accept().expect("accept");
        let mut since_line = String::new();
        BufReader::new(&connection)
            .read_line(&mut since_line)
            .expect("since line");
        connection.write_all(reply.as_bytes()).expect("reply");
        (since_line, connection)
    }

    #[test]
    fn spec_3_11_a_real_unix_socket_replays_then_resumes_after_the_server_closes() {
        use std::os::unix::net::UnixListener;
        let socket_path =
            std::env::temp_dir().join(format!("estf-u31-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let socket_path_to_unlink = socket_path.clone();
        let server = std::thread::spawn(move || {
            let (first_since_line, first_connection) =
                serve_reply(&listener, &format!("{}{}", frame_line(5), frame_line(6)));
            drop(first_connection);
            let (second_since_line, second_connection) = serve_reply(&listener, &frame_line(11));
            std::fs::remove_file(&socket_path_to_unlink).expect("unlink socket");
            drop(listener);
            drop(second_connection);
            vec![first_since_line, second_since_line]
        });
        let mut connector = UnixSocketConnector::new(socket_path);
        let mut delivered = Vec::new();
        let ended_with = follow_inbox(&mut connector, &mut |_| {}, &mut |received| {
            delivered.push(received.clone());
        })
        .expect_err("a followed inbox never returns Ok");
        let since_lines = server.join().expect("server thread");
        assert_eq!(since_lines, vec!["{\"since\":0}\n", "{\"since\":6}\n"]);
        assert_eq!(delivered, vec![frame(5), frame(6), frame(11)]);
        assert!(matches!(ended_with, InboxClientError::ConnectFailed(_)));
    }
}
