//! Minimal SSE (Server-Sent Events) decoder and stream wrapper.
//!
//! This module provides a self-contained SSE parser that consumes an hpx
//! byte stream and yields typed events. It exists because the SDK's
//! `SseStream::new()` is `pub(crate)` and cannot be reused, and because
//! we need to construct an hpx client with `read_timeout` (per-chunk)
//! instead of the SDK's `total_timeout` (single-shot) for proper SSE
//! stream longevity.

use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

// ---------------------------------------------------------------------------
// SSE Decoder
// ---------------------------------------------------------------------------

/// A single parsed SSE event.
#[derive(Debug, Clone, Default)]
struct ServerSentEvent {
    /// Concatenated `data:` field values (joined with `\n`).
    data: String,
    /// `event:` field value (unused for JSON parsing, but tracked for completeness).
    #[allow(dead_code)]
    event_type: Option<String>,
    /// `id:` field value.
    #[allow(dead_code)]
    id: Option<String>,
}

/// Minimal SSE protocol decoder.
///
/// Accumulates raw bytes into line buffers and emits [`ServerSentEvent`]s
/// on empty-line boundaries (per the SSE specification). Handles:
/// - `data:` — concatenated with `\n` if multiple lines
/// - `event:` — stored but not used for JSON parsing
/// - `id:` — stored but not used
/// - `:` — comment lines, silently ignored
/// - CRLF and LF line endings
struct SseDecoder {
    /// Partial line buffer (may span multiple byte chunks).
    buffer: Vec<u8>,
    /// Accumulator for the current event's `data:` field.
    current_data: String,
    /// Accumulator for the current event's `event:` field.
    current_event_type: Option<String>,
    /// Accumulator for the current event's `id:` field.
    current_id: Option<String>,
}

impl SseDecoder {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
            current_data: String::new(),
            current_event_type: None,
            current_id: None,
        }
    }

    /// Feed raw bytes into the decoder. Returns all complete events found.
    fn feed(&mut self, chunk: &[u8]) -> Vec<ServerSentEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        // Process complete lines.
        loop {
            // Find the next LF character (SSE line terminator).
            let pos = match self.buffer.iter().position(|&b| b == b'\n') {
                Some(p) => p,
                None => break,
            };

            // Determine where the line content ends (before the line ending).
            // For CRLF, exclude both the CR and LF. For bare LF, just the LF.
            let content_end = if pos > 0 && self.buffer[pos - 1] == b'\r' {
                pos - 1
            } else {
                pos
            };

            // Extract line content (without the line ending).
            let line: Vec<u8> = self.buffer.drain(..content_end).collect();
            // Drain the line ending characters (1 for LF, 2 for CRLF).
            let ending_len = pos + 1 - content_end;
            self.buffer.drain(..ending_len);

            if let Some(event) = self.process_line(&line) {
                events.push(event);
            }
        }

        events
    }

    /// Flush any remaining buffered data as a final event (end-of-stream).
    fn flush(&mut self) -> Option<ServerSentEvent> {
        if self.buffer.is_empty() {
            return None;
        }
        let line: Vec<u8> = std::mem::take(&mut self.buffer);
        self.process_line(&line)
    }

    /// Process a single line and potentially emit an event.
    /// An empty line signals the end of an event (SSE dispatch boundary).
    fn process_line(&mut self, line: &[u8]) -> Option<ServerSentEvent> {
        let line = std::str::from_utf8(line).unwrap_or("");

        // Empty line = dispatch boundary.
        if line.is_empty() {
            return self.emit_event();
        }

        // Comment line.
        if line.starts_with(':') {
            return None;
        }

        // Parse field.
        if let Some((field, value)) = line.split_once(':') {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    // The SDK's decoder appends a newline before each subsequent
                    // data line, matching the SSE spec's joining behavior.
                    if !self.current_data.is_empty() {
                        self.current_data.push('\n');
                    }
                    self.current_data.push_str(value);
                }
                "event" => {
                    self.current_event_type = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "id" => {
                    self.current_id = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                _ => {
                    // Ignore unknown fields (retry, etc.)
                }
            }
        } else if line == "data" {
            // `data:` with no value — append empty line to data.
            if !self.current_data.is_empty() {
                self.current_data.push('\n');
            }
        }

        None
    }

    /// Emit a complete event from the accumulated fields and reset state.
    fn emit_event(&mut self) -> Option<ServerSentEvent> {
        if self.current_data.is_empty() && self.current_event_type.is_none() && self.current_id.is_none() {
            return None;
        }
        let event = ServerSentEvent {
            data: std::mem::take(&mut self.current_data),
            event_type: self.current_event_type.take(),
            id: self.current_id.take(),
        };
        Some(event)
    }
}

// ---------------------------------------------------------------------------
// SseEventStream
// ---------------------------------------------------------------------------

pin_project! {
    /// A stream of typed items parsed from Server-Sent Events.
    ///
    /// Wraps an hpx response byte stream with per-chunk `read_timeout` and
    /// parses each SSE event's `data` field as JSON of type `T`.
    ///
    /// This is the hpx-direct equivalent of `opencode_sdk_rs::SseStream`,
    /// but constructed with a client that uses `read_timeout` instead of
    /// `total_timeout`, which is critical for long-lived SSE connections.
    pub struct SseEventStream<T> {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<Bytes, hpx::Error>> + Send>>,
        decoder: SseDecoder,
        pending: Vec<ServerSentEvent>,
        _marker: std::marker::PhantomData<T>,
    }
}

impl<T: serde::de::DeserializeOwned> SseEventStream<T> {
    /// Create an `SseEventStream` from an hpx response byte stream.
    pub fn new(
        byte_stream: impl Stream<Item = Result<Bytes, hpx::Error>> + Send + 'static,
    ) -> Self {
        Self {
            inner: Box::pin(byte_stream),
            decoder: SseDecoder::new(),
            pending: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: serde::de::DeserializeOwned> Stream for SseEventStream<T> {
    type Item = Result<T, SseStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // First, drain any pending events from a previous chunk.
        if !this.pending.is_empty() {
            let event = this.pending.remove(0);
            if event.data.is_empty() {
                // Skip events with no data (heartbeats, etc.).
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let parsed =
                serde_json::from_str::<T>(&event.data).map_err(|e| SseStreamError::Json(e));
            return Poll::Ready(Some(parsed));
        }

        // Poll the inner byte stream for more data.
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                let events = this.decoder.feed(&bytes);
                *this.pending = events;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(SseStreamError::Connection(e.to_string()))))
            }
            Poll::Ready(None) => {
                // Stream ended — flush any remaining buffered data.
                if let Some(event) = this.decoder.flush() {
                    if !event.data.is_empty() {
                        let parsed = serde_json::from_str::<T>(&event.data)
                            .map_err(|e| SseStreamError::Json(e));
                        return Poll::Ready(Some(parsed));
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur while parsing an SSE event stream.
#[derive(Debug)]
pub enum SseStreamError {
    /// The underlying connection produced an error (timeout, reset, etc.).
    Connection(String),
    /// Failed to deserialize an SSE event's `data` field as JSON.
    Json(serde_json::Error),
}

impl std::fmt::Display for SseStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SseStreamError::Connection(msg) => write!(f, "Connection error: {}", msg),
            SseStreamError::Json(e) => write!(f, "JSON parse error: {}", e),
        }
    }
}

impl std::error::Error for SseStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SseStreamError::Json(e) => Some(e),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// Helper: build an `SseEventStream` from a static byte slice.
    fn stream_from_bytes(data: &'static [u8]) -> SseEventStream<serde_json::Value> {
        // Convert to a stream of single-chunk results.
        let stream = futures::stream::once(async move { Ok(Bytes::from_static(data)) });
        SseEventStream::new(stream)
    }

    /// Helper: build an `SseEventStream` from a multi-chunk byte stream.
    fn stream_from_chunks(chunks: Vec<&'static [u8]>) -> SseEventStream<serde_json::Value> {
        let stream = futures::stream::iter(
            chunks.into_iter().map(|c| Ok::<_, hpx::Error>(Bytes::from_static(c))),
        );
        SseEventStream::new(stream)
    }

    #[tokio::test]
    async fn single_event() {
        let data = b"data: {\"type\":\"test\"}\n\n";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["type"], "test");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn multiple_events() {
        let data = b"data: {\"n\":1}\n\ndata: {\"n\":2}\n\ndata: {\"n\":3}\n\n";
        let mut stream = stream_from_bytes(data);
        for i in 1..=3 {
            let event = stream.next().await.unwrap().unwrap();
            assert_eq!(event["n"], i);
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn multiline_data_joined() {
        let data = b"data: {\"type\":\ndata:  \"test\"}\n\n";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["type"], "test");
    }

    #[tokio::test]
    async fn empty_data_skipped() {
        let data = b"data: {\"a\":1}\n\ndata:\n\ndata: {\"b\":2}\n\n";
        let mut stream = stream_from_bytes(data);
        let e1 = stream.next().await.unwrap().unwrap();
        assert_eq!(e1["a"], 1);
        let e2 = stream.next().await.unwrap().unwrap();
        assert_eq!(e2["b"], 2);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn comment_lines_ignored() {
        let data = b": keep-alive\ndata: {\"ok\":true}\n\n";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["ok"], true);
    }

    #[tokio::test]
    async fn crlf_endings() {
        let data = b"data: {\"crlf\":true}\r\n\r\n";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["crlf"], true);
    }

    #[tokio::test]
    async fn event_split_across_chunks() {
        let chunks: Vec<&[u8]> = vec![
            b"data: {\"split\":",
            b"true}\n\n",
        ];
        let mut stream = stream_from_chunks(chunks);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["split"], true);
    }

    #[tokio::test]
    async fn invalid_json_produces_error() {
        let data = b"data: {not valid}\n\n";
        let mut stream = stream_from_bytes(data);
        let result = stream.next().await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn event_and_id_fields_tracked() {
        let data = b"event: custom\nid: 42\ndata: {\"val\":1}\n\n";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["val"], 1);
    }

    #[tokio::test]
    async fn empty_stream_no_events() {
        let data = b"";
        let mut stream = stream_from_bytes(data);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn connection_error_formatting() {
        // Verify that SseStreamError::Connection formats correctly,
        // matching the pattern expected by events.rs error detection.
        let err = SseStreamError::Connection("request or response body error: operation timed out".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Connection error"), "got: {}", msg);
        assert!(msg.contains("timed out"), "got: {}", msg);
    }
}
