//! Newline-delimited JSON-RPC 2.0 over stdio for rmcp, with explicit error replies.
//!
//! rmcp's stock stdio transport silently drops unparsable lines. The MCP contract here is
//! stricter: unparsable JSON gets a JSON-RPC Parse error (-32700, id null) and well-formed
//! JSON that is not a valid message gets Invalid Request (-32600, with its id when it has
//! one); the server then continues with the next line. Oversized lines (over
//! `MAX_LINE_BYTES`) are discarded with an Invalid Request error instead of being buffered.
//!
//! Every outgoing line (rmcp responses and these errors) goes through one writer task, so
//! lines are never interleaved and stdout only ever carries complete protocol frames.
//! `receive` is cancellation-safe: rmcp polls it inside `select!`, so partial lines are
//! kept in the transport between polls.
use rmcp::{
    RoleServer,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::Transport,
};
use serde_json::{Value, json};
use std::io;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

/// Largest accepted JSON-RPC message line (tool arguments are far smaller).
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// Outgoing lines queued for stdout; a client that stops reading applies backpressure.
pub const OUT_CAPACITY: usize = 64;

pub struct LineTransport<R> {
    reader: R,
    out: mpsc::Sender<Vec<u8>>,
    line: Vec<u8>,
    oversized: bool,
    /// Error replies produced while reading, sent before the next read.
    errors: std::collections::VecDeque<Vec<u8>>,
}

/// Starts the single stdout writer. Dropping every sender (the transport) ends the task
/// after it has written and flushed everything queued.
pub fn writer_task<W>(mut writer: W) -> (mpsc::Sender<Vec<u8>>, JoinHandle<()>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUT_CAPACITY);
    let handle = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
                crate::log::warn("stdout closed; stopping");
                return;
            }
        }
    });
    (tx, handle)
}

enum Read {
    Line,
    Oversized,
    Eof,
}

impl<R: AsyncBufRead + Unpin + Send> LineTransport<R> {
    pub fn new(reader: R, out: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            reader,
            out,
            line: Vec::new(),
            oversized: false,
            errors: std::collections::VecDeque::new(),
        }
    }

    /// Sends queued error replies. Cancellation-safe: a reply leaves the queue only once a
    /// channel slot is reserved for it. `false` when stdout is gone.
    async fn flush_errors(&mut self) -> bool {
        while !self.errors.is_empty() {
            let Ok(permit) = self.out.reserve().await else {
                return false;
            };
            if let Some(line) = self.errors.pop_front() {
                permit.send(line);
            }
        }
        true
    }

    /// Appends to `self.line` until a newline; state survives a dropped (cancelled) poll.
    async fn read_line(&mut self) -> io::Result<Read> {
        loop {
            let buf = self.reader.fill_buf().await?;
            if buf.is_empty() {
                // EOF: a final line without a newline is still a message.
                if !self.line.is_empty() && !self.oversized {
                    return Ok(Read::Line);
                }
                return Ok(Read::Eof);
            }
            let newline = buf.iter().position(|b| *b == b'\n');
            let (chunk, consumed) = match newline {
                Some(i) => (&buf[..i], i + 1),
                None => (buf, buf.len()),
            };
            if !self.oversized {
                if self.line.len() + chunk.len() > MAX_LINE_BYTES {
                    self.oversized = true;
                    self.line = Vec::new();
                } else {
                    self.line.extend_from_slice(chunk);
                }
            }
            self.reader.consume(consumed);
            if newline.is_some() {
                if std::mem::take(&mut self.oversized) {
                    return Ok(Read::Oversized);
                }
                return Ok(Read::Line);
            }
        }
    }

    fn reply_error(&mut self, id: Value, code: i64, message: &str) {
        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }))
        .unwrap_or_default();
        bytes.push(b'\n');
        self.errors.push_back(bytes);
    }

    /// Parses one complete line; `None` when it was handled here (error reply or ignored).
    fn parse(&mut self, raw: &[u8]) -> Option<ClientJsonRpcMessage> {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        match serde_json::from_slice::<ClientJsonRpcMessage>(line) {
            Ok(message) => Some(message),
            Err(e) => {
                match serde_json::from_slice::<Value>(line) {
                    Err(_) => {
                        crate::log::warn(&format!("unparsable JSON-RPC input ({e})"));
                        self.reply_error(Value::Null, -32700, "Parse error: invalid JSON");
                    }
                    Ok(value) => {
                        let id = value
                            .get("id")
                            .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
                            .cloned();
                        let is_notification =
                            value.get("method").is_some() && value.get("id").is_none();
                        crate::log::warn(&format!("invalid JSON-RPC message ({e})"));
                        // Notifications get no reply, even when malformed (JSON-RPC 2.0 §4.1).
                        if !is_notification {
                            self.reply_error(
                                id.unwrap_or(Value::Null),
                                -32600,
                                &format!("Invalid Request: {e}"),
                            );
                        }
                    }
                }
                None
            }
        }
    }
}

impl<R: AsyncBufRead + Unpin + Send + 'static> Transport<RoleServer> for LineTransport<R> {
    type Error = io::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let out = self.out.clone();
        let encoded = serde_json::to_vec(&item).map_err(io::Error::other);
        async move {
            let mut bytes = encoded?;
            bytes.push(b'\n');
            out.send(bytes)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"))
        }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        loop {
            if !self.flush_errors().await {
                return None;
            }
            let read = match self.read_line().await {
                Ok(read) => read,
                Err(e) => {
                    crate::log::warn(&format!("stdin read failed: {e}"));
                    return None;
                }
            };
            match read {
                Read::Eof => {
                    self.flush_errors().await;
                    return None;
                }
                Read::Oversized => {
                    crate::log::warn("discarded an oversized JSON-RPC line");
                    self.reply_error(
                        Value::Null,
                        -32600,
                        "Invalid Request: message exceeds 4 MiB",
                    );
                }
                Read::Line => {
                    let line = std::mem::take(&mut self.line);
                    if let Some(message) = self.parse(&line) {
                        return Some(message);
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, BufReader};

    async fn run(input: Vec<u8>) -> (Vec<ClientJsonRpcMessage>, Vec<Value>) {
        let (tx, mut rx) = mpsc::channel(1024);
        let mut transport = LineTransport::new(BufReader::new(std::io::Cursor::new(input)), tx);
        let mut messages = Vec::new();
        while let Some(m) = Transport::<RoleServer>::receive(&mut transport).await {
            messages.push(m);
        }
        drop(transport);
        let mut replies = Vec::new();
        while let Some(line) = rx.recv().await {
            assert_eq!(line.last(), Some(&b'\n'));
            replies.push(serde_json::from_slice(&line).unwrap());
        }
        (messages, replies)
    }

    #[tokio::test]
    async fn malformed_lines_get_errors_and_reading_continues() {
        let ping = br#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let mut input = Vec::new();
        input.extend_from_slice(b"not json\n");
        input.extend_from_slice(b"\n   \n");
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":5}"#);
        input.extend_from_slice(b"\n");
        input.extend_from_slice(br#"{"jsonrpc":"2.0","method":"notifications/x","params":5}"#);
        input.extend_from_slice(b"\n[1,2]\r\n");
        input.extend_from_slice(ping);
        input.extend_from_slice(b"\n");
        input.extend_from_slice(ping); // final line without newline
        let (messages, replies) = run(input).await;
        assert_eq!(messages.len(), 2, "{messages:?}");
        let codes: Vec<(Value, i64)> = replies
            .iter()
            .map(|r| (r["id"].clone(), r["error"]["code"].as_i64().unwrap()))
            .collect();
        assert_eq!(
            codes,
            vec![
                (Value::Null, -32700),
                (json!(7), -32600),
                (Value::Null, -32600)
            ]
        );
    }

    #[tokio::test]
    async fn oversized_lines_are_discarded_without_buffering() {
        let mut input = vec![b'x'; MAX_LINE_BYTES + 10];
        input.push(b'\n');
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        input.push(b'\n');
        let (messages, replies) = run(input).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn the_writer_task_flushes_everything_queued_before_ending() {
        let (client, mut server) = tokio::io::duplex(64);
        let (tx, handle) = writer_task(client);
        for i in 0..50 {
            tx.send(format!("{{\"n\":{i}}}\n").into_bytes())
                .await
                .unwrap();
        }
        drop(tx);
        let mut all = String::new();
        let reader = tokio::spawn(async move {
            server.read_to_string(&mut all).await.unwrap();
            all
        });
        handle.await.unwrap();
        let all = reader.await.unwrap();
        assert_eq!(all.lines().count(), 50);
    }
}
