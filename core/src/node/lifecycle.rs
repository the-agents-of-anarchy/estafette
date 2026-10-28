//! The session state machine of spec 3.9 and the own standard input reader of spec 3.9.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

const STANDARD_INPUT_CHUNK_BYTES: usize = 8192;

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
    use std::time::Duration;

    use super::*;

    const STALLED_SERVER_CAPACITY_BYTES: usize = 1;
    const TYPED_BEFORE_THE_END: &[u8] = b"a tool call the server is still answering";
    const END_NOTED_WITHIN: Duration = Duration::from_millis(500);

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
}
