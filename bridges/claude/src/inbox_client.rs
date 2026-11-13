use std::convert::Infallible;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
const FRAME_LINE_TERMINATOR_BYTE: u8 = b'\n';
const WIRE_STRUCTURE_MAX_BYTES: u64 = 1_114_112;
const WORST_CASE_ESCAPED_BYTES_PER_TEXT_BYTE: u64 = 6;
const FRAME_FIELDS_ALLOWANCE_BYTES: u64 = 4096;
const MAX_FRAME_LINE_BYTES: u64 = WIRE_STRUCTURE_MAX_BYTES
    .saturating_mul(WORST_CASE_ESCAPED_BYTES_PER_TEXT_BYTE)
    .saturating_add(FRAME_FIELDS_ALLOWANCE_BYTES);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InboxFrame {
    pub seq: u64,
    pub id: String,
    pub from: String,
    pub body: String,
}

/// A change of one connection to an inbox socket while it is followed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxConnection {
    /// The socket took the connection.
    Opened,
    /// The socket closed the connection; the following reconnects.
    Closed,
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
    MalformedFrame(serde_json::Error),
    FrameLineTooLong,
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
            Self::MalformedFrame(reason) => write!(
                formatter,
                "the inbox socket sent a frame it cannot read: {reason}"
            ),
            Self::FrameLineTooLong => write!(
                formatter,
                "the inbox socket sent a frame line over {MAX_FRAME_LINE_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for InboxClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConnectFailed(source)
            | Self::ResumeRequestFailed(source)
            | Self::ReadFailed(source) => Some(source),
            Self::MalformedFrame(reason) => Some(reason),
            Self::FrameLineTooLong => None,
        }
    }
}

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

/// Delivers each inbox frame above `since` once, in order, reconnecting with the last seq seen whenever the socket closes, and reports each connection opened and closed.
/// # Errors
/// Returns the `InboxClientError` that ended the following; a closed socket alone never ends it.
pub fn follow_inbox<C: InboxConnector>(
    connector: &mut C,
    since: u64,
    wait_before_reconnect: &mut impl FnMut(Duration),
    report_connection: &mut impl FnMut(InboxConnection),
    deliver_frame: &mut impl FnMut(&InboxFrame),
) -> Result<Infallible, InboxClientError> {
    let mut last_seq = since;
    loop {
        let stream = connector
            .connect()
            .map_err(InboxClientError::ConnectFailed)?;
        report_connection(InboxConnection::Opened);
        last_seq = deliver_until_close(stream, last_seq, deliver_frame)?;
        report_connection(InboxConnection::Closed);
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
    request.push(char::from(FRAME_LINE_TERMINATOR_BYTE));
    stream.write_all(request.as_bytes())?;
    stream.flush()
}

fn read_complete_line(reader: &mut impl BufRead) -> Result<Option<String>, InboxClientError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_FRAME_LINE_BYTES)
        .read_until(FRAME_LINE_TERMINATOR_BYTE, &mut bytes)
        .map_err(InboxClientError::ReadFailed)?;
    if bytes.last() != Some(&FRAME_LINE_TERMINATOR_BYTE) {
        return if u64::try_from(bytes.len()).is_ok_and(|length| length >= MAX_FRAME_LINE_BYTES) {
            Err(InboxClientError::FrameLineTooLong)
        } else {
            Ok(None)
        };
    }
    bytes.pop();
    String::from_utf8(bytes).map(Some).map_err(|error| {
        InboxClientError::ReadFailed(io::Error::new(io::ErrorKind::InvalidData, error))
    })
}

fn parse_frame(line: &str) -> Result<InboxFrame, InboxClientError> {
    serde_json::from_str(line).map_err(InboxClientError::MalformedFrame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeded::next_below;
    use std::cell::{Cell, RefCell};
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

    const NO_FRAME_HANDLED_YET: u64 = 0;

    struct Followed {
        delivered: Vec<InboxFrame>,
        pauses: Vec<Duration>,
        ended_with: InboxClientError,
    }

    enum BrokenOperation {
        Write,
        ReadAfterReplies,
    }

    struct BrokenStream {
        replies: Cursor<Vec<u8>>,
        broken_operation: BrokenOperation,
    }

    impl Read for BrokenStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match (self.replies.read(buffer)?, &self.broken_operation) {
                (0, BrokenOperation::ReadAfterReplies) => {
                    Err(io::Error::from(io::ErrorKind::ConnectionReset))
                }
                (read_bytes, BrokenOperation::ReadAfterReplies | BrokenOperation::Write) => {
                    Ok(read_bytes)
                }
            }
        }
    }

    impl Write for BrokenStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match self.broken_operation {
                BrokenOperation::Write => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
                BrokenOperation::ReadAfterReplies => Ok(bytes.len()),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct OneShotConnector(Option<BrokenStream>);

    impl InboxConnector for OneShotConnector {
        type Stream = BrokenStream;

        fn connect(&mut self) -> io::Result<BrokenStream> {
            self.0
                .take()
                .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))
        }
    }

    fn follow_until_it_ends(
        connector: &mut impl InboxConnector,
    ) -> (Vec<InboxFrame>, Vec<Duration>, InboxClientError) {
        let mut delivered = Vec::new();
        let mut pauses = Vec::new();
        let ended_with = follow_inbox(
            connector,
            NO_FRAME_HANDLED_YET,
            &mut |pause| pauses.push(pause),
            &mut |_| {},
            &mut |received| delivered.push(received.clone()),
        )
        .expect_err("a followed inbox never returns Ok");
        (delivered, pauses, ended_with)
    }

    fn follow_broken(replies: String, broken_operation: BrokenOperation) -> Followed {
        let mut connector = OneShotConnector(Some(BrokenStream {
            replies: Cursor::new(replies.into_bytes()),
            broken_operation,
        }));
        let (delivered, pauses, ended_with) = follow_until_it_ends(&mut connector);
        Followed {
            delivered,
            pauses,
            ended_with,
        }
    }

    fn frame_line(seq: u64) -> String {
        format!(
            "{{\"seq\":{seq},\"id\":\"id-{seq}\",\"from\":\"from-{seq}\",\"body\":\"body {seq}\"}}\n"
        )
    }

    fn frame(seq: u64) -> InboxFrame {
        InboxFrame {
            seq,
            id: format!("id-{seq}"),
            from: format!("from-{seq}"),
            body: format!("body {seq}"),
        }
    }

    fn follow_scripted(connections: &[String]) -> (Vec<String>, Followed) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut connector = ScriptedConnector {
            connections: connections.iter().cloned().collect(),
            requests: Rc::clone(&requests),
        };
        let (delivered, pauses, ended_with) = follow_until_it_ends(&mut connector);
        let requests = requests.borrow().clone();
        (
            requests,
            Followed {
                delivered,
                pauses,
                ended_with,
            },
        )
    }

    #[test]
    fn spec_3_11_first_connection_asks_since_zero_and_delivers_replayed_frames_in_order() {
        let (requests, followed) =
            follow_scripted(&[format!("{}{}", frame_line(7), frame_line(9))]);
        assert_eq!(requests[0], "{\"since\":0}\n");
        assert_eq!(followed.delivered, vec![frame(7), frame(9)]);
    }

    #[test]
    fn spec_3_12_step_5_reconnect_asks_since_the_last_seq_seen() {
        let (requests, followed) = follow_scripted(&[
            format!("{}{}", frame_line(3), frame_line(4)),
            frame_line(8),
            String::new(),
        ]);
        assert_eq!(
            requests,
            vec!["{\"since\":0}\n", "{\"since\":4}\n", "{\"since\":8}\n"]
        );
        assert_eq!(followed.delivered, vec![frame(3), frame(4), frame(8)]);
    }

    #[test]
    fn spec_3_12_step_5_a_frame_replayed_after_reconnect_is_delivered_once() {
        let (_, followed) = follow_scripted(&[
            format!("{}{}", frame_line(3), frame_line(4)),
            format!("{}{}", frame_line(4), frame_line(5)),
        ]);
        assert_eq!(followed.delivered, vec![frame(3), frame(4), frame(5)]);
    }

    #[test]
    fn spec_3_11_a_line_cut_off_by_the_close_is_dropped_and_resent_after_reconnect() {
        let (requests, followed) = follow_scripted(&[
            format!("{}{{\"seq\":4,\"id\":\"id-4\",\"fr", frame_line(3)),
            frame_line(4),
        ]);
        assert_eq!(requests[1], "{\"since\":3}\n");
        assert_eq!(followed.delivered, vec![frame(3), frame(4)]);
    }

    #[test]
    fn spec_3_12_step_5_waits_the_reconnect_pause_before_each_reconnect() {
        let (_, followed) = follow_scripted(&[frame_line(1), frame_line(2)]);
        assert_eq!(followed.pauses, vec![RECONNECT_PAUSE, RECONNECT_PAUSE]);
    }

    #[test]
    fn spec_3_12_step_3_a_refused_connection_ends_the_following() {
        let (_, followed) = follow_scripted(&[]);
        assert!(matches!(
            followed.ended_with,
            InboxClientError::ConnectFailed(_)
        ));
        assert_eq!(followed.pauses, Vec::<Duration>::new());
    }

    #[test]
    fn spec_3_11_a_frame_that_is_not_json_ends_the_following() {
        let (_, followed) = follow_scripted(&[format!("{}not json\n", frame_line(1))]);
        assert!(matches!(
            &followed.ended_with,
            InboxClientError::MalformedFrame(_)
        ));
        assert_eq!(followed.delivered, vec![frame(1)]);
    }

    #[test]
    fn spec_3_11_the_error_of_a_frame_that_cannot_be_read_does_not_print_its_message_body() {
        const BODY_SECRET: &str = "top-secret-body";
        let (_, followed) = follow_scripted(&[format!(
            "{{\"seq\":1,\"id\":\"id-1\",\"from\":\"ana\",\"body\":\"{BODY_SECRET}\"\n"
        )]);
        assert!(matches!(
            &followed.ended_with,
            InboxClientError::MalformedFrame(_)
        ));
        assert!(!followed.ended_with.to_string().contains(BODY_SECRET));
    }

    #[test]
    fn spec_3_11_a_frame_line_longer_than_any_frame_can_be_ends_the_following() {
        let overlong_line_without_terminator =
            "x".repeat(usize::try_from(MAX_FRAME_LINE_BYTES).unwrap_or(usize::MAX));
        let (_, followed) = follow_scripted(&[
            frame_line(1),
            overlong_line_without_terminator,
            frame_line(2),
        ]);
        assert!(matches!(
            followed.ended_with,
            InboxClientError::FrameLineTooLong
        ));
        assert_eq!(followed.delivered, vec![frame(1)]);
    }

    #[test]
    fn spec_3_11_an_overlong_line_cut_inside_a_multi_byte_character_ends_the_following() {
        let characters_past_the_cap = usize::try_from(MAX_FRAME_LINE_BYTES).unwrap_or(usize::MAX);
        let overlong_line_cut_inside_a_character =
            format!("x{}", "\u{e9}".repeat(characters_past_the_cap));
        let (_, followed) = follow_scripted(&[frame_line(1), overlong_line_cut_inside_a_character]);
        assert!(
            matches!(followed.ended_with, InboxClientError::FrameLineTooLong),
            "{:?}",
            followed.ended_with
        );
        assert_eq!(followed.delivered, vec![frame(1)]);
    }

    struct EndlessLineStream {
        unread_bytes: u64,
        consumed_bytes: Rc<Cell<u64>>,
    }

    impl Read for EndlessLineStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let served = usize::try_from(self.unread_bytes)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            buffer.iter_mut().take(served).for_each(|byte| *byte = b'x');
            let served_bytes = u64::try_from(served).unwrap_or_default();
            self.unread_bytes = self.unread_bytes.saturating_sub(served_bytes);
            self.consumed_bytes
                .set(self.consumed_bytes.get().saturating_add(served_bytes));
            Ok(served)
        }
    }

    impl Write for EndlessLineStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct EndlessLineConnector(Option<EndlessLineStream>);

    impl InboxConnector for EndlessLineConnector {
        type Stream = EndlessLineStream;

        fn connect(&mut self) -> io::Result<EndlessLineStream> {
            self.0
                .take()
                .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))
        }
    }

    const READ_AHEAD_SLACK_BYTES: u64 = 65_536;

    #[test]
    fn spec_3_11_a_line_without_a_newline_is_read_no_further_than_the_frame_line_cap() {
        let consumed_bytes = Rc::new(Cell::new(0));
        let mut connector = EndlessLineConnector(Some(EndlessLineStream {
            unread_bytes: MAX_FRAME_LINE_BYTES.saturating_mul(2),
            consumed_bytes: Rc::clone(&consumed_bytes),
        }));
        let (_, _, ended_with) = follow_until_it_ends(&mut connector);
        assert!(matches!(ended_with, InboxClientError::FrameLineTooLong));
        assert!(
            consumed_bytes.get() <= MAX_FRAME_LINE_BYTES.saturating_add(READ_AHEAD_SLACK_BYTES),
            "{} bytes read",
            consumed_bytes.get()
        );
    }

    #[test]
    fn spec_3_11_a_frame_the_core_writer_can_emit_at_its_largest_is_delivered() {
        let escaped_control_byte = '\u{1}';
        let largest_text_bytes = usize::try_from(WIRE_STRUCTURE_MAX_BYTES).unwrap_or_default();
        let body: String = std::iter::repeat_n(escaped_control_byte, largest_text_bytes).collect();
        let line = format!(
            "{{\"seq\":1,\"id\":\"\",\"from\":\"\",\"body\":{},\"sent_at\":1,\"received_at\":1}}\n",
            serde_json::to_string(&body).expect("a string serializes")
        );
        assert!(
            u64::try_from(line.len()).unwrap_or(u64::MAX) <= MAX_FRAME_LINE_BYTES,
            "the largest writable line is {} bytes",
            line.len()
        );
        let (_, followed) = follow_scripted(&[line]);
        assert_eq!(followed.delivered.len(), 1);
        assert_eq!(
            followed.delivered.first().map(|frame| frame.body.len()),
            Some(largest_text_bytes)
        );
    }

    #[test]
    fn spec_3_11_the_six_key_envelope_of_a_frame_at_its_largest_numbers_fits_the_fields_allowance()
    {
        let envelope = format!(
            "{{\"seq\":{max},\"id\":\"\",\"from\":\"\",\"body\":\"\",\"sent_at\":{max},\"received_at\":{max}}}\n",
            max = u64::MAX
        );
        let envelope_bytes = u64::try_from(envelope.len()).unwrap_or(u64::MAX);
        assert!(
            envelope_bytes.saturating_mul(2) < FRAME_FIELDS_ALLOWANCE_BYTES,
            "the envelope is {envelope_bytes} bytes"
        );
        let (_, followed) = follow_scripted(&[envelope]);
        assert_eq!(followed.delivered.len(), 1);
    }

    #[test]
    fn spec_3_11_the_wire_limit_restated_here_is_the_one_core_enforces() {
        let core_wire = include_str!("../../../core/src/wire.rs");
        let core_limit = core_wire
            .lines()
            .find_map(|line| line.strip_prefix("pub const WIRE_LIMIT_BYTES: usize = "))
            .map(|rest| rest.trim_end_matches(';').replace('_', ""))
            .and_then(|digits| digits.parse::<u64>().ok());
        assert_eq!(core_limit, Some(WIRE_STRUCTURE_MAX_BYTES));
    }

    #[test]
    fn spec_3_11_a_valid_frame_line_of_exactly_the_cap_is_delivered() {
        let frame_text = r#"{"seq":1,"id":"","from":"","body":""}"#;
        let padding = usize::try_from(MAX_FRAME_LINE_BYTES)
            .unwrap_or_default()
            .saturating_sub(frame_text.len())
            .saturating_sub(1);
        let line = format!("{frame_text}{}\n", " ".repeat(padding));
        assert_eq!(u64::try_from(line.len()).ok(), Some(MAX_FRAME_LINE_BYTES));
        let (_, followed) = follow_scripted(&[line]);
        assert_eq!(followed.delivered.len(), 1);
    }

    #[test]
    fn spec_3_11_every_error_that_wraps_a_cause_names_it_as_its_source() {
        use std::error::Error;
        let cause = || io::Error::from(io::ErrorKind::BrokenPipe);
        for error in [
            InboxClientError::ConnectFailed(cause()),
            InboxClientError::ResumeRequestFailed(cause()),
            InboxClientError::ReadFailed(cause()),
            InboxClientError::MalformedFrame(
                serde_json::from_str::<InboxFrame>("x").expect_err("not a frame"),
            ),
        ] {
            assert!(error.source().is_some(), "{error}");
        }
        assert!(InboxClientError::FrameLineTooLong.source().is_none());
    }

    const GENERATED_CASES: u64 = 200;
    const MOST_RECONNECTS: usize = 5;
    const MOST_FRAMES_PER_CONNECTION: usize = 6;
    const MOST_FRAMES_REPLAYED_AGAIN: usize = 3;

    #[test]
    fn spec_3_12_step_5_generated_reconnects_deliver_each_seq_once_in_order() {
        for seed in 0..GENERATED_CASES {
            let mut state = seed;
            let mut connections = Vec::new();
            let mut expected = Vec::new();
            let mut highest_delivered = NO_FRAME_HANDLED_YET;
            for _ in 0..=next_below(&mut state, MOST_RECONNECTS) {
                let replayed_again =
                    u64::try_from(next_below(&mut state, MOST_FRAMES_REPLAYED_AGAIN))
                        .unwrap_or_default();
                let first_seq = highest_delivered
                    .saturating_sub(replayed_again)
                    .saturating_add(1);
                let frame_count = u64::try_from(next_below(&mut state, MOST_FRAMES_PER_CONNECTION))
                    .unwrap_or_default();
                let seqs: Vec<u64> = (first_seq..first_seq.saturating_add(frame_count)).collect();
                connections.push(seqs.iter().map(|seq| frame_line(*seq)).collect::<String>());
                for seq in seqs {
                    if seq > highest_delivered {
                        highest_delivered = seq;
                        expected.push(frame(seq));
                    }
                }
            }
            let (_, followed) = follow_scripted(&connections);
            assert_eq!(followed.delivered, expected, "seed {seed}");
        }
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
    fn spec_3_12_step_3_a_since_line_that_cannot_be_written_ends_the_following() {
        let followed = follow_broken(frame_line(1), BrokenOperation::Write);
        assert!(matches!(
            followed.ended_with,
            InboxClientError::ResumeRequestFailed(_)
        ));
        assert_eq!(followed.delivered, Vec::<InboxFrame>::new());
        assert_eq!(followed.pauses, Vec::<Duration>::new());
    }

    #[test]
    fn spec_3_11_a_socket_that_fails_mid_stream_ends_the_following_after_the_frames_before_it() {
        let followed = follow_broken(
            format!("{}{}", frame_line(4), frame_line(6)),
            BrokenOperation::ReadAfterReplies,
        );
        assert!(matches!(
            followed.ended_with,
            InboxClientError::ReadFailed(_)
        ));
        assert_eq!(followed.delivered, vec![frame(4), frame(6)]);
        assert_eq!(followed.pauses, Vec::<Duration>::new());
    }

    #[test]
    fn spec_3_11_a_real_unix_socket_replays_then_resumes_after_the_server_closes() {
        use std::os::unix::net::UnixListener;
        let socket_path =
            std::env::temp_dir().join(format!("estf-u31-{}.sock", std::process::id()));
        if let Err(failure) = std::fs::remove_file(&socket_path) {
            assert_eq!(failure.kind(), io::ErrorKind::NotFound, "stale socket file");
        }
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
        let ended_with = follow_inbox(
            &mut connector,
            NO_FRAME_HANDLED_YET,
            &mut |_| {},
            &mut |_| {},
            &mut |received| {
                delivered.push(received.clone());
            },
        )
        .expect_err("a followed inbox never returns Ok");
        let since_lines = server.join().expect("server thread");
        assert_eq!(since_lines, vec!["{\"since\":0}\n", "{\"since\":6}\n"]);
        assert_eq!(delivered, vec![frame(5), frame(6), frame(11)]);
        assert!(matches!(ended_with, InboxClientError::ConnectFailed(_)));
    }
}
