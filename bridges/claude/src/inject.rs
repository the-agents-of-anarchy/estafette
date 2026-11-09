use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use serde::Serialize;

use crate::closing_tag::escape_closing_tags;
use crate::inbox_client::{InboxConnector, InboxFrame};
use crate::permission_class::PermissionClass;
use crate::ratelimit::{Admission, SenderRateLimit};

const LINE_TERMINATOR: char = '\n';
const ENVELOPE_TAG: &str = "cross-session-message";
const HARNESS_MESSAGE_VERSION: u8 = 1;
const HARNESS_DEFAULT_PRIORITY: &str = "next";
const LONGEST_ENVELOPE_NAME_CHARACTERS: usize = 64;
const ENVELOPE_NAME_FORBIDDEN_CHARACTERS: [char; 3] = ['"', '<', '>'];
const TRUNCATED_NAME_MARK: char = '\u{2026}';

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum LineType {
    Auth,
    User,
}

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
struct AuthLine<'a> {
    #[serde(rename = "type")]
    line_type: LineType,
    token: &'a str,
}

#[derive(Serialize)]
struct PeerLine<'a> {
    #[serde(rename = "msgV")]
    message_version: u8,
    msg_id: &'a str,
    #[serde(rename = "type")]
    line_type: LineType,
    message: UserMessage,
    priority: &'static str,
}

/// Writes a sender name as the harness's envelope rebuild reproduces it: cut to 64 characters with an ellipsis, or nothing for a name the rebuild would alter.
fn envelope_name(name: &str) -> Option<String> {
    let is_rebuilt_unchanged = !name.is_empty()
        && name.chars().all(|character| character.is_ascii_graphic())
        && !name.contains(ENVELOPE_NAME_FORBIDDEN_CHARACTERS);
    if !is_rebuilt_unchanged {
        return None;
    }
    if name.chars().count() <= LONGEST_ENVELOPE_NAME_CHARACTERS {
        return Some(name.to_owned());
    }
    let mut truncated: String = name
        .chars()
        .take(LONGEST_ENVELOPE_NAME_CHARACTERS)
        .collect();
    truncated.push(TRUNCATED_NAME_MARK);
    Some(truncated)
}

/// Wraps the escaped body in the envelope the harness writes for a peer send, naming the sender and attesting the class.
fn envelope_content(name: &str, class: PermissionClass, body: &str) -> String {
    let body = escape_closing_tags(body);
    format!(
        "<{ENVELOPE_TAG} from-name=\"{name}\" from-mode=\"{class}\">\n{body}\n</{ENVELOPE_TAG}>"
    )
}

fn write_line(stream: &mut impl Write, line: &impl Serialize) -> io::Result<()> {
    let mut text = serde_json::to_string(line)?;
    text.push(LINE_TERMINATOR);
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

#[derive(Debug)]
pub enum InjectError {
    ConnectFailed(io::Error),
    WriteFailed(io::Error),
    SenderNameRefused(String),
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
            Self::SenderNameRefused(name) => write!(
                formatter,
                "frame from {name:?} dropped: the harness's envelope rebuild alters a sender name that is empty or holds anything but visible ASCII without \" < >"
            ),
        }
    }
}

impl std::error::Error for InjectError {}

pub struct SessionNotifier<C: InboxConnector> {
    connector: C,
    session_token: String,
    class: PermissionClass,
    rate_limit: SenderRateLimit,
    refused_frames: u64,
}

impl<C: InboxConnector> SessionNotifier<C> {
    /// Names the messaging socket connector, the session token every line authenticates with and the class attested until the session's live class is read.
    pub fn new(connector: C, session_token: String, class: PermissionClass) -> Self {
        Self {
            connector,
            session_token,
            class,
            rate_limit: SenderRateLimit::default(),
            refused_frames: 0,
        }
    }

    /// Attests the permission class the session holds now on the frames written from here on.
    pub const fn attest(&mut self, class: PermissionClass) {
        self.class = class;
    }

    /// Injects one frame as the harness's peer line unless its sender name is refused or the rate limit drops it.
    /// # Errors
    /// Returns the `InjectError` of the refused name or of the failed connection or write.
    pub fn notify(&mut self, frame: &InboxFrame, now: Duration) -> Result<(), InjectError> {
        let Some(name) = envelope_name(&frame.from) else {
            self.refused_frames = self.refused_frames.saturating_add(1);
            return Err(InjectError::SenderNameRefused(frame.from.clone()));
        };
        match self.rate_limit.admit(&frame.from, &frame.body, now) {
            Admission::Dropped => Ok(()),
            Admission::Admitted => self.write_peer_line(frame, &name),
        }
    }

    fn write_peer_line(&mut self, frame: &InboxFrame, name: &str) -> Result<(), InjectError> {
        let mut stream = self
            .connector
            .connect()
            .map_err(InjectError::ConnectFailed)?;
        let auth = AuthLine {
            line_type: LineType::Auth,
            token: &self.session_token,
        };
        let peer_line = PeerLine {
            message_version: HARNESS_MESSAGE_VERSION,
            msg_id: &frame.id,
            line_type: LineType::User,
            message: UserMessage {
                role: Role::User,
                content: envelope_content(name, self.class, &frame.body),
            },
            priority: HARNESS_DEFAULT_PRIORITY,
        };
        write_line(&mut stream, &auth)
            .and_then(|()| write_line(&mut stream, &peer_line))
            .map_err(InjectError::WriteFailed)
    }

    /// Counts the frames the rate limit has dropped and the frames whose sender name was refused.
    #[must_use]
    pub const fn dropped_frames(&self) -> u64 {
        self.rate_limit
            .dropped_frames()
            .saturating_add(self.refused_frames)
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

    fn frame_with_id(id: &str, from: &str, body: &str) -> InboxFrame {
        InboxFrame {
            id: id.to_owned(),
            ..frame_from(from, body, 0)
        }
    }

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

    fn notifier_of_class(
        class: PermissionClass,
    ) -> (SessionNotifier<RecordingConnector>, RecordedConnections) {
        let connections = RecordedConnections::default();
        let notifier = SessionNotifier::new(
            RecordingConnector(Rc::clone(&connections)),
            TEST_TOKEN.to_owned(),
            class,
        );
        (notifier, connections)
    }

    fn notifier() -> (SessionNotifier<RecordingConnector>, RecordedConnections) {
        notifier_of_class(PermissionClass::Bypass)
    }

    const HARNESS_LINE_ID: &str = "2d3f9084-db21-40dc-9e8a-91780c582877";

    #[test]
    fn harness_2_1_319_one_frame_opens_one_connection_with_the_auth_line_then_the_harness_peer_line()
     {
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
        notifier
            .notify(
                &frame_with_id(HARNESS_LINE_ID, "ana", "build is green"),
                Duration::ZERO,
            )
            .expect("notify");
        assert_eq!(
            *connections.borrow(),
            vec![
                "{\"type\":\"auth\",\"token\":\"session-token-77\"}\n\
                 {\"msgV\":1,\"msg_id\":\"2d3f9084-db21-40dc-9e8a-91780c582877\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<cross-session-message from-name=\\\"ana\\\" from-mode=\\\"bypass\\\">\\nbuild is green\\n</cross-session-message>\"},\"priority\":\"next\"}\n"
            ]
        );
    }

    #[test]
    fn harness_2_1_319_a_prompting_session_is_attested_as_prompting() {
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Prompting);
        notifier
            .notify(&frame_from("bo", "x", 0), Duration::ZERO)
            .expect("notify");
        assert!(connections.borrow()[0].contains("from-mode=\\\"prompting\\\""));
    }

    #[test]
    fn harness_2_1_319_the_body_travels_byte_for_byte_with_quotes_newlines_and_non_ascii() {
        let body = "say \"hi\"\nline 2 \u{2014} na\u{ef}ve \u{65e5}\u{672c} tail";
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
        notifier
            .notify(&frame_from("ana", body, 0), Duration::ZERO)
            .expect("notify");
        assert_eq!(
            *connections.borrow(),
            vec![
                "{\"type\":\"auth\",\"token\":\"session-token-77\"}\n\
                 {\"msgV\":1,\"msg_id\":\"id-1\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<cross-session-message from-name=\\\"ana\\\" from-mode=\\\"bypass\\\">\\nsay \\\"hi\\\"\\nline 2 \u{2014} na\u{ef}ve \u{65e5}\u{672c} tail\\n</cross-session-message>\"},\"priority\":\"next\"}\n"
            ]
        );
    }

    const LONGEST_ENVELOPE_NAME: usize = 64;
    const GRAMMAR_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";

    fn grammar_name_of_length(length: usize) -> String {
        std::iter::once('a')
            .chain(
                GRAMMAR_ALPHABET
                    .iter()
                    .cycle()
                    .skip(length)
                    .take(length.saturating_sub(1))
                    .map(|byte| char::from(*byte)),
            )
            .collect()
    }

    #[test]
    fn harness_2_1_319_every_handle_of_the_grammar_up_to_64_characters_is_delivered() {
        for length in 1..=LONGEST_ENVELOPE_NAME {
            let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
            let name = grammar_name_of_length(length);
            notifier
                .notify(&frame_from(&name, "x", 0), Duration::ZERO)
                .expect("notify");
            assert_eq!(connections.borrow().len(), 1, "{name}");
            assert!(
                connections.borrow()[0].contains(&format!("from-name=\\\"{name}\\\" from-mode")),
                "{name}"
            );
        }
    }

    #[test]
    fn harness_2_1_319_a_sender_name_the_rebuild_would_alter_is_refused_and_counted_as_dropped() {
        for name in ["an\"a", "an<a", "an>a", "an\na", "an a", "ana\u{2014}", ""] {
            let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
            let outcome = notifier.notify(&frame_from(name, "x", 0), Duration::ZERO);
            assert!(
                matches!(outcome, Err(InjectError::SenderNameRefused(_))),
                "{name:?}"
            );
            assert!(connections.borrow().is_empty(), "{name:?}");
            assert_eq!(notifier.dropped_frames(), 1, "{name:?}");
        }
    }

    const LONGEST_LOCAL_HANDLE: usize = 74;

    #[test]
    fn harness_2_1_319_a_74_character_sender_name_is_written_cut_to_64_characters_with_an_ellipsis()
    {
        let name = grammar_name_of_length(LONGEST_LOCAL_HANDLE);
        let kept: String = name.chars().take(LONGEST_ENVELOPE_NAME).collect();
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
        notifier
            .notify(&frame_from(&name, "x", 0), Duration::ZERO)
            .expect("notify");
        assert_eq!(connections.borrow().len(), 1);
        assert!(
            connections.borrow()[0].contains(&format!(
                "<cross-session-message from-name=\\\"{kept}\u{2026}\\\" from-mode"
            )),
            "{}",
            connections.borrow()[0]
        );
        assert_eq!(notifier.dropped_frames(), 0);
    }

    fn peer_content(written: &str) -> String {
        let peer_line = written.lines().nth(1).expect("peer line");
        let line: serde_json::Value = serde_json::from_str(peer_line).expect("peer line json");
        line["message"]["content"]
            .as_str()
            .expect("content")
            .to_owned()
    }

    #[test]
    fn harness_2_1_319_each_closing_tag_in_a_body_is_rewritten_as_the_harness_rewrites_it() {
        let body = "a </cross-session-message> b </CROSS-Session-Message>c </cross-session-messages> d </cross-session-message-x e </cross-session-message";
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
        notifier
            .notify(&frame_from("ana", body, 0), Duration::ZERO)
            .expect("notify");
        assert_eq!(
            peer_content(&connections.borrow()[0]),
            "<cross-session-message from-name=\"ana\" from-mode=\"bypass\">\na <\\/cross-session-message> b <\\/CROSS-Session-Message>c </cross-session-messages> d </cross-session-message-x e <\\/cross-session-message\n</cross-session-message>"
        );
    }

    fn escaped_body_of(body: &str) -> String {
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Bypass);
        notifier
            .notify(&frame_from("ana", body, 0), Duration::ZERO)
            .expect("notify");
        injected_body(&connections.borrow()[0])
    }

    #[test]
    fn harness_2_1_319_a_closing_tag_the_harness_matches_in_a_lookalike_form_is_rewritten_too() {
        for (body, escaped) in [
            (
                "\u{ff1c}/cross-session-message>",
                "<\\/cross-session-message>",
            ),
            (
                "</cr\u{200b}oss-ses\u{200d}sion-message>",
                "<\\/cr\u{200b}oss-ses\u{200d}sion-message>",
            ),
            (
                "\u{ff1c}\u{ff0f}cross\u{2010}session\u{2212}message",
                "<\\\u{ff0f}cross\u{2010}session\u{2212}message",
            ),
            (
                "< \u{200b}/ \u{200b}cross-session-message",
                "<\\ \u{200b}/ \u{200b}cross-session-message",
            ),
            (
                "\u{2039}/CROSS-SESSION-MESSAGE",
                "<\\/CROSS-SESSION-MESSAGE",
            ),
        ] {
            let rewritten = escaped_body_of(body);
            assert_eq!(rewritten, escaped, "{body}");
        }
    }

    #[test]
    fn harness_2_1_319_text_the_harness_does_not_match_as_a_closing_tag_is_left_alone() {
        for body in [
            "<\\/cross-session-message>",
            "<x/cross-session-message>",
            "</cross\u{b7}session-message>",
            "</cross-session-messagE1",
            "<cross-session-message>",
        ] {
            assert_eq!(escaped_body_of(body), body, "{body}");
        }
    }

    #[test]
    fn harness_2_1_319_the_class_attested_last_is_the_class_written() {
        let (mut notifier, connections) = notifier_of_class(PermissionClass::Prompting);
        notifier.attest(PermissionClass::Bypass);
        notifier
            .notify(&frame_from("ana", "x", 0), Duration::ZERO)
            .expect("notify");
        assert!(connections.borrow()[0].contains("from-mode=\\\"bypass\\\""));
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

    fn injected_body(written: &str) -> String {
        let peer_line = written.lines().nth(1).expect("peer line");
        let line: serde_json::Value = serde_json::from_str(peer_line).expect("peer line json");
        let content = line["message"]["content"].as_str().expect("content");
        let (_, after_open_tag) = content.split_once(">\n").expect("open tag");
        after_open_tag
            .strip_suffix("\n</cross-session-message>")
            .expect("close tag")
            .to_owned()
    }

    fn injected_bodies(connections: &RecordedConnections) -> Vec<String> {
        connections
            .borrow()
            .iter()
            .map(|written| injected_body(written))
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
            PermissionClass::Bypass,
        );
        let outcome = notifier.notify(&frame_from("ana", "x", 0), Duration::ZERO);
        assert!(matches!(outcome, Err(InjectError::ConnectFailed(_))));
    }

    #[test]
    fn spec_3_12_a_socket_that_cannot_be_written_is_reported_as_write_failed() {
        let mut notifier = SessionNotifier::new(
            BrokenConnector(None),
            TEST_TOKEN.to_owned(),
            PermissionClass::Bypass,
        );
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
            PermissionClass::Bypass,
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
}
