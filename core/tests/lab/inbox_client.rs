use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

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

    /// The frame the socket has already emitted and the client not yet read, none when there is none, waiting for nothing.
    pub fn frame_already_emitted(&mut self) -> Option<Value> {
        self.frames.get_ref().set_nonblocking(true).unwrap();
        let mut line = String::new();
        let read = self.frames.read_line(&mut line);
        self.frames.get_ref().set_nonblocking(false).unwrap();
        match read {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(failure) if failure.kind() == ErrorKind::WouldBlock && line.is_empty() => None,
            Err(failure) if failure.kind() == ErrorKind::WouldBlock => Some(Value::from(line)),
            Err(failure) => panic!("the inbox socket failed: {failure}"),
        }
    }

    /// The next frame the socket emits within the wait, none when it emits nothing.
    pub fn next_frame(&mut self, within: Duration) -> Option<Value> {
        self.frames
            .get_ref()
            .set_read_timeout(Some(within))
            .unwrap();
        let mut line = String::new();
        match self.frames.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(failure)
                if matches!(failure.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                None
            }
            Err(failure) => panic!("the inbox socket failed: {failure}"),
        }
    }
}
