use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::inbox_client::{InboxConnector, InboxFrame};
use crate::ratelimit::{Admission, SenderRateLimit};

const LINE_TERMINATOR: char = '\n';
const MILLIS_PER_SECOND: u64 = 1000;
const LATEST_RENDERABLE_SENT_AT: &str = "9999-12-31T23:59:59Z";

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
}

#[derive(Serialize)]
struct UserMessage {
    role: Role,
    content: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum SocketLine<'a> {
    Auth { token: &'a str },
    User { message: UserMessage },
}

fn iso_8601_utc(unix_millis: u64) -> String {
    let unix_seconds = unix_millis.div_euclid(MILLIS_PER_SECOND);
    i64::try_from(unix_seconds)
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|instant| instant.format(&Rfc3339).ok())
        .unwrap_or_else(|| LATEST_RENDERABLE_SENT_AT.to_owned())
}

fn notice_text(frame: &InboxFrame) -> String {
    format!(
        "Message from peer {} (verified member of your network), sent {}.\n{}\nReply with the send tool.",
        frame.from,
        iso_8601_utc(frame.sent_at),
        frame.body
    )
}

fn write_line(stream: &mut impl Write, line: &SocketLine) -> io::Result<()> {
    let mut text = serde_json::to_string(line)?;
    text.push(LINE_TERMINATOR);
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

#[derive(Debug)]
pub enum InjectError {
    ConnectFailed(io::Error),
    WriteFailed(io::Error),
}

impl fmt::Display for InjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectFailed(source) => {
                write!(
                    formatter,
                    "cannot connect to the messaging socket: {source}"
                )
            }
            Self::WriteFailed(source) => {
                write!(formatter, "cannot write to the messaging socket: {source}")
            }
        }
    }
}

impl std::error::Error for InjectError {}

pub struct SessionNotifier<C: InboxConnector> {
    connector: C,
    session_token: String,
    rate_limit: SenderRateLimit,
}

impl<C: InboxConnector> SessionNotifier<C> {
    /// Names the messaging socket connector and the session token every notice authenticates with.
    pub fn new(connector: C, session_token: String) -> Self {
        Self {
            connector,
            session_token,
            rate_limit: SenderRateLimit::default(),
        }
    }

    /// Injects one frame as a notice into the session unless the rate limit drops it.
    /// # Errors
    /// Returns the `InjectError` of the failed connection or write.
    pub fn notify(&mut self, frame: &InboxFrame, now: Duration) -> Result<(), InjectError> {
        match self.rate_limit.admit(&frame.from, &frame.body, now) {
            Admission::Dropped => Ok(()),
            Admission::Admitted => self.write_notice(frame),
        }
    }

    fn write_notice(&mut self, frame: &InboxFrame) -> Result<(), InjectError> {
        let mut stream = self
            .connector
            .connect()
            .map_err(InjectError::ConnectFailed)?;
        let auth = SocketLine::Auth {
            token: &self.session_token,
        };
        let notice = SocketLine::User {
            message: UserMessage {
                role: Role::User,
                content: notice_text(frame),
            },
        };
        write_line(&mut stream, &auth)
            .and_then(|()| write_line(&mut stream, &notice))
            .map_err(InjectError::WriteFailed)
    }

    /// Counts the frames the rate limit has dropped.
    #[must_use]
    pub const fn dropped_frames(&self) -> u64 {
        self.rate_limit.dropped_frames()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::rc::Rc;

    type RecordedConnections = Rc<RefCell<Vec<String>>>;

    struct RecordingStream {
        written: Vec<u8>,
        connections: RecordedConnections,
    }

    impl Read for RecordingStream {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for RecordingStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for RecordingStream {
        fn drop(&mut self) {
            self.connections
                .borrow_mut()
                .push(String::from_utf8_lossy(&self.written).into_owned());
        }
    }

    struct RecordingConnector(RecordedConnections);

    impl InboxConnector for RecordingConnector {
        type Stream = RecordingStream;

        fn connect(&mut self) -> io::Result<RecordingStream> {
            Ok(RecordingStream {
                written: Vec::new(),
                connections: Rc::clone(&self.0),
            })
        }
    }

    const TEST_TOKEN: &str = "session-token-77";

    fn frame_from(from: &str, body: &str, sent_at: u64) -> InboxFrame {
        InboxFrame {
            seq: 1,
            id: "id-1".to_owned(),
            from: from.to_owned(),
            body: body.to_owned(),
            sent_at,
            received_at: sent_at,
        }
    }

    fn notifier() -> (SessionNotifier<RecordingConnector>, RecordedConnections) {
        let connections = RecordedConnections::default();
        let notifier = SessionNotifier::new(
            RecordingConnector(Rc::clone(&connections)),
            TEST_TOKEN.to_owned(),
        );
        (notifier, connections)
    }

    #[test]
    fn spec_3_12_one_frame_opens_one_connection_with_the_auth_line_then_the_notice_line() {
        let (mut notifier, connections) = notifier();
        notifier
            .notify(
                &frame_from("ana", "build is green", 1_700_000_000_000),
                Duration::ZERO,
            )
            .expect("notify");
        assert_eq!(
            *connections.borrow(),
            vec![
                "{\"type\":\"auth\",\"token\":\"session-token-77\"}\n\
                 {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Message from peer ana (verified member of your network), sent 2023-11-14T22:13:20Z.\\nbuild is green\\nReply with the send tool.\"}}\n"
            ]
        );
    }

    fn notice_content_lines(written: &str) -> Vec<String> {
        let notice_line = written.lines().nth(1).expect("notice line");
        let notice: serde_json::Value = serde_json::from_str(notice_line).expect("notice json");
        notice["message"]["content"]
            .as_str()
            .expect("content")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn notice_sent_header_for(sent_at: u64) -> String {
        let (mut notifier, connections) = notifier();
        notifier
            .notify(&frame_from("ana", "x", sent_at), Duration::ZERO)
            .expect("notify");
        let header = notice_content_lines(&connections.borrow()[0])[0].clone();
        let (_, timestamp_and_stop) = header.split_once(", sent ").expect("sent marker");
        timestamp_and_stop
            .strip_suffix('.')
            .expect("closing stop")
            .to_owned()
    }

    #[test]
    fn spec_3_12_sent_at_renders_as_iso_8601_utc_across_calendar_edges() {
        let rendered: Vec<String> = [
            0,
            951_782_400_000,
            4_107_542_400_000,
            1_704_067_199_000,
            1_700_000_000_999,
        ]
        .into_iter()
        .map(notice_sent_header_for)
        .collect();
        assert_eq!(
            rendered,
            vec![
                "1970-01-01T00:00:00Z",
                "2000-02-29T00:00:00Z",
                "2100-03-01T00:00:00Z",
                "2023-12-31T23:59:59Z",
                "2023-11-14T22:13:20Z",
            ]
        );
    }

    fn notify_at(
        notifier: &mut SessionNotifier<RecordingConnector>,
        from: &str,
        body: &str,
        seconds: u64,
    ) {
        notifier
            .notify(&frame_from(from, body, 0), Duration::from_secs(seconds))
            .expect("notify");
    }

    fn injected_bodies(connections: &RecordedConnections) -> Vec<String> {
        connections
            .borrow()
            .iter()
            .map(|written| notice_content_lines(written)[1].clone())
            .collect()
    }

    #[test]
    fn spec_3_12_the_eleventh_injection_from_one_sender_within_60_seconds_is_dropped() {
        let (mut notifier, connections) = notifier();
        for index in 0..10 {
            notify_at(&mut notifier, "ana", &format!("body {index}"), index);
        }
        notify_at(&mut notifier, "ana", "body 10", 59);
        assert_eq!(connections.borrow().len(), 10);
        assert_eq!(notifier.dropped_frames(), 1);
    }

    #[test]
    fn spec_3_12_a_sender_over_the_limit_is_admitted_once_its_oldest_injection_is_60_seconds_old() {
        let (mut notifier, connections) = notifier();
        for index in 0..10 {
            notify_at(&mut notifier, "ana", &format!("body {index}"), index);
        }
        notify_at(&mut notifier, "ana", "body 10", 60);
        assert_eq!(connections.borrow().len(), 11);
        assert_eq!(notifier.dropped_frames(), 0);
    }

    #[test]
    fn spec_3_12_the_limit_counts_each_sender_apart() {
        let (mut notifier, connections) = notifier();
        for index in 0..10 {
            notify_at(&mut notifier, "ana", &format!("body {index}"), index);
        }
        notify_at(&mut notifier, "bo", "body 0", 10);
        assert_eq!(connections.borrow().len(), 11);
        assert_eq!(notifier.dropped_frames(), 0);
    }

    #[test]
    fn spec_3_12_a_body_identical_to_the_previous_one_within_30_seconds_is_dropped() {
        let (mut notifier, connections) = notifier();
        notify_at(&mut notifier, "ana", "same", 0);
        notify_at(&mut notifier, "ana", "same", 29);
        assert_eq!(injected_bodies(&connections), vec!["same"]);
        assert_eq!(notifier.dropped_frames(), 1);
    }

    #[test]
    fn spec_3_12_a_body_identical_to_the_previous_one_passes_after_30_seconds() {
        let (mut notifier, connections) = notifier();
        notify_at(&mut notifier, "ana", "same", 0);
        notify_at(&mut notifier, "ana", "same", 30);
        assert_eq!(injected_bodies(&connections), vec!["same", "same"]);
        assert_eq!(notifier.dropped_frames(), 0);
    }

    #[test]
    fn spec_3_12_a_different_body_passes_and_only_the_previous_body_counts_as_identical() {
        let (mut notifier, connections) = notifier();
        notify_at(&mut notifier, "ana", "first", 0);
        notify_at(&mut notifier, "ana", "second", 1);
        notify_at(&mut notifier, "ana", "first", 2);
        notify_at(&mut notifier, "bo", "first", 3);
        assert_eq!(
            injected_bodies(&connections),
            vec!["first", "second", "first", "first"]
        );
        assert_eq!(notifier.dropped_frames(), 0);
    }

    #[test]
    fn spec_3_12_a_dropped_body_is_the_previous_body_so_a_repeat_every_10_seconds_stays_dropped() {
        let (mut notifier, connections) = notifier();
        for seconds in [0, 10, 20, 30, 40, 70] {
            notify_at(&mut notifier, "ana", "same", seconds);
        }
        assert_eq!(injected_bodies(&connections), vec!["same", "same"]);
        assert_eq!(notifier.dropped_frames(), 4);
    }

    struct BrokenConnector(Option<io::ErrorKind>);

    struct BrokenStream;

    impl Read for BrokenStream {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for BrokenStream {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl InboxConnector for BrokenConnector {
        type Stream = BrokenStream;

        fn connect(&mut self) -> io::Result<BrokenStream> {
            self.0
                .map_or(Ok(BrokenStream), |kind| Err(io::Error::from(kind)))
        }
    }

    #[test]
    fn spec_3_12_a_refused_connection_is_reported_as_connect_failed() {
        let mut notifier = SessionNotifier::new(
            BrokenConnector(Some(io::ErrorKind::ConnectionRefused)),
            TEST_TOKEN.to_owned(),
        );
        let outcome = notifier.notify(&frame_from("ana", "x", 0), Duration::ZERO);
        assert!(matches!(outcome, Err(InjectError::ConnectFailed(_))));
    }

    #[test]
    fn spec_3_12_a_socket_that_cannot_be_written_is_reported_as_write_failed() {
        let mut notifier = SessionNotifier::new(BrokenConnector(None), TEST_TOKEN.to_owned());
        let outcome = notifier.notify(&frame_from("ana", "x", 0), Duration::ZERO);
        assert!(matches!(outcome, Err(InjectError::WriteFailed(_))));
    }

    #[test]
    fn spec_3_12_a_real_unix_socket_receives_both_lines_and_is_closed_after_them() {
        use crate::inbox_client::UnixSocketConnector;
        use std::os::unix::net::UnixListener;
        let socket_path =
            std::env::temp_dir().join(format!("estf-u32-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket_path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().expect("accept");
            let mut received = String::new();
            connection
                .read_to_string(&mut received)
                .expect("read to close");
            received
        });
        let mut notifier = SessionNotifier::new(
            UnixSocketConnector::new(socket_path.clone()),
            TEST_TOKEN.to_owned(),
        );
        notifier
            .notify(&frame_from("ana", "over a real socket", 0), Duration::ZERO)
            .expect("notify");
        let received = server.join().expect("server thread");
        std::fs::remove_file(&socket_path).expect("unlink socket");
        let lines: Vec<&str> = received.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "{\"type\":\"auth\",\"token\":\"session-token-77\"}"
        );
        assert!(lines[1].contains("over a real socket"));
    }

    #[test]
    fn spec_3_12_a_peer_chosen_sent_at_beyond_year_9999_renders_as_the_latest_iso_8601_instant() {
        let rendered: Vec<String> = [253_402_300_798_000, 253_402_300_799_000, u64::MAX]
            .into_iter()
            .map(notice_sent_header_for)
            .collect();
        assert_eq!(
            rendered,
            vec![
                "9999-12-31T23:59:58Z",
                "9999-12-31T23:59:59Z",
                "9999-12-31T23:59:59Z",
            ]
        );
    }
}
