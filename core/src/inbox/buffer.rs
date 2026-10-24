use std::collections::VecDeque;

use serde::Serialize;

pub const INBOX_BUFFER_CAPACITY: usize = 1000;
const FIRST_SEQ: u64 = 1;

/// The five fields of a received message, spec D31.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMessage {
    pub id: String,
    pub from: String,
    pub body: String,
    pub sent_at: u64,
    pub received_at: u64,
}

/// A received message numbered for the life of the process, as spec 3.11 serializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxFrame {
    pub seq: u64,
    pub id: String,
    pub from: String,
    pub body: String,
    pub sent_at: u64,
    pub received_at: u64,
}

/// The last 1000 received frames of one process, replayable from any `since`.
#[derive(Debug)]
pub struct InboxBuffer {
    frames: VecDeque<InboxFrame>,
    next_seq: u64,
}

impl InboxBuffer {
    /// An empty buffer whose first frame will carry seq 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(INBOX_BUFFER_CAPACITY),
            next_seq: FIRST_SEQ,
        }
    }

    /// Numbers the message, buffers it, drops the oldest frame past capacity, and returns the frame.
    pub fn push(&mut self, message: ReceivedMessage) -> InboxFrame {
        let frame = InboxFrame {
            seq: self.next_seq,
            id: message.id,
            from: message.from,
            body: message.body,
            sent_at: message.sent_at,
            received_at: message.received_at,
        };
        self.next_seq = self.next_seq.saturating_add(1);
        if self.frames.len() == INBOX_BUFFER_CAPACITY {
            self.frames.pop_front();
        }
        self.frames.push_back(frame.clone());
        frame
    }

    /// Every buffered frame with seq greater than `since`, oldest first, leaving the buffer untouched.
    #[must_use]
    pub fn frames_since(&self, since: u64) -> Vec<InboxFrame> {
        self.frames
            .iter()
            .filter(|frame| frame.seq > since)
            .cloned()
            .collect()
    }
}

impl Default for InboxBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENT_AT_BASE: u64 = 1_000;
    const RECEIVED_AT_BASE: u64 = 2_000;

    fn message_number(n: u64) -> ReceivedMessage {
        ReceivedMessage {
            id: format!("id-{n}"),
            from: format!("from-{n}"),
            body: format!("body-{n}"),
            sent_at: SENT_AT_BASE.saturating_add(n),
            received_at: RECEIVED_AT_BASE.saturating_add(n),
        }
    }

    fn buffer_after_pushes(count: u64) -> InboxBuffer {
        let mut buffer = InboxBuffer::new();
        for n in 1..=count {
            buffer.push(message_number(n));
        }
        buffer
    }

    fn seqs(frames: &[InboxFrame]) -> Vec<u64> {
        frames.iter().map(|frame| frame.seq).collect()
    }

    #[test]
    fn spec_3_11_test_11_since_3_after_5_messages_replays_4_and_5() {
        let buffer = buffer_after_pushes(5);
        assert_eq!(seqs(&buffer.frames_since(3)), vec![4, 5]);
    }

    #[test]
    fn spec_3_11_first_frame_is_seq_1_and_each_push_returns_its_numbered_frame() {
        let mut buffer = InboxBuffer::new();
        let first = buffer.push(message_number(7));
        let second = buffer.push(message_number(8));
        assert_eq!(
            (first.seq, first.id.as_str(), first.body.as_str()),
            (1, "id-7", "body-7")
        );
        assert_eq!((second.seq, second.id.as_str()), (2, "id-8"));
        assert_eq!(buffer.frames_since(0), vec![first, second]);
    }

    #[test]
    fn spec_3_11_the_1001st_push_drops_seq_1() {
        let buffer = buffer_after_pushes(1001);
        let replayed = seqs(&buffer.frames_since(0));
        assert_eq!(replayed.len(), INBOX_BUFFER_CAPACITY);
        assert_eq!(replayed.first(), Some(&2));
        assert_eq!(replayed.last(), Some(&1001));
    }

    #[test]
    fn spec_3_11_since_older_than_the_buffer_replays_from_the_oldest_frame() {
        let buffer = buffer_after_pushes(1500);
        let replayed = seqs(&buffer.frames_since(10));
        assert_eq!(replayed.first(), Some(&501));
        assert_eq!(replayed.last(), Some(&1500));
    }

    #[test]
    fn spec_3_11_since_at_or_past_the_newest_seq_replays_nothing() {
        let buffer = buffer_after_pushes(5);
        assert_eq!(seqs(&buffer.frames_since(5)), Vec::<u64>::new());
        assert_eq!(seqs(&buffer.frames_since(99)), Vec::<u64>::new());
    }

    #[test]
    fn spec_3_11_reading_removes_nothing() {
        let buffer = buffer_after_pushes(5);
        let first_read = buffer.frames_since(0);
        assert_eq!(buffer.frames_since(0), first_read);
        assert_eq!(first_read.len(), 5);
    }

    #[test]
    fn spec_3_11_frame_serializes_as_the_six_key_json_object() {
        let mut buffer = InboxBuffer::new();
        let frame = buffer.push(ReceivedMessage {
            id: "m-1".to_owned(),
            from: "ana".to_owned(),
            body: "hello".to_owned(),
            sent_at: 1_700_000_000_123,
            received_at: 1_700_000_000_456,
        });
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            serde_json::json!({
                "seq": 1,
                "id": "m-1",
                "from": "ana",
                "body": "hello",
                "sent_at": 1_700_000_000_123_u64,
                "received_at": 1_700_000_000_456_u64,
            })
        );
    }
}
