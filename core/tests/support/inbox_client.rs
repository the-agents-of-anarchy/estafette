use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

/// How long an inbox socket may take to emit the frame of a delivered message.
pub const FRAME_WAIT: Duration = Duration::from_secs(10);
/// How long after a frame an inbox socket is watched for one that must not come, ended by the member's own counters snapshot past it.
pub const NO_FURTHER_FRAME_WINDOW: Duration = Duration::from_millis(1000);

/// A client of a member's inbox socket, reading its frames from the `since` it connected with.
pub struct InboxClient {
    frames: BufReader<UnixStream>,
}

impl InboxClient {
    /// Connects to the inbox socket at the path init returned and asks for the frames after `since`.
    pub fn connect(inbox_path: &Value, since: u64) -> Self {
        let mut socket = UnixStream::connect(inbox_path.as_str().unwrap()).unwrap();
        writeln!(socket, "{}", json!({ "since": since })).unwrap();
        Self {
            frames: BufReader::new(socket),
        }
    }

    /// The frame the socket has already emitted and the client not yet read, none when the socket is open and has none, waiting for nothing and failing on a closed socket.
    #[track_caller]
    pub fn frame_already_emitted(&mut self) -> Option<Value> {
        self.frames.get_ref().set_nonblocking(true).unwrap();
        let mut line = String::new();
        let read = self.frames.read_line(&mut line);
        self.frames.get_ref().set_nonblocking(false).unwrap();
        match read {
            Ok(0) => panic!("the inbox socket closed, so a further frame cannot be told from none"),
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(failure) if failure.kind() == ErrorKind::WouldBlock && line.is_empty() => None,
            Err(failure) if failure.kind() == ErrorKind::WouldBlock => Some(Value::from(line)),
            Err(failure) => panic!("the inbox socket failed: {failure}"),
        }
    }

    /// The next frame the socket emits, requiring it within the wait.
    #[track_caller]
    pub fn next_frame(&mut self, within: Duration) -> Value {
        self.frames
            .get_ref()
            .set_read_timeout(Some(within))
            .unwrap();
        let mut line = String::new();
        match self.frames.read_line(&mut line) {
            Ok(0) => panic!("the inbox socket closed with no frame within {within:?}"),
            Ok(_) => serde_json::from_str(&line).unwrap(),
            Err(failure)
                if matches!(failure.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                panic!("the inbox socket emitted no frame within {within:?}")
            }
            Err(failure) => panic!("the inbox socket failed: {failure}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;

    use serde_json::json;

    use super::InboxClient;

    const FIRST_FRAME_LINE: &str = "{\"seq\":1}\n";

    fn client_of_a_stand_in_socket() -> (InboxClient, UnixStream) {
        let (client_end, stand_in_end) = UnixStream::pair().unwrap();
        let client = InboxClient {
            frames: BufReader::new(client_end),
        };
        (client, stand_in_end)
    }

    #[test]
    #[should_panic(expected = "the inbox socket closed")]
    fn spec_3_15_test_1_the_absence_check_refuses_an_inbox_socket_that_closed_after_its_first_frame()
     {
        let (mut client, mut stand_in) = client_of_a_stand_in_socket();
        stand_in.write_all(FIRST_FRAME_LINE.as_bytes()).unwrap();
        drop(stand_in);
        assert_eq!(client.frame_already_emitted(), Some(json!({ "seq": 1 })));

        client.frame_already_emitted();
    }

    #[test]
    fn spec_3_15_test_1_the_absence_check_finds_none_on_an_inbox_socket_that_stays_open_and_silent()
    {
        let (mut client, _stand_in_kept_open) = client_of_a_stand_in_socket();

        assert_eq!(client.frame_already_emitted(), None);
    }
}
