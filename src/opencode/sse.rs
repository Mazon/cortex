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
use std::collections::VecDeque;
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

/// Maximum allowed size for the internal line buffer (1 MB).
/// A misbehaving server sending no LF characters would otherwise cause
/// unbounded memory growth. If exceeded, the buffer is silently cleared
/// and an error is logged — this is conservative but prevents OOM.
const MAX_BUFFER_SIZE: usize = 1_048_576;

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

        // Guard against unbounded buffer growth from a misbehaving server.
        if self.buffer.len() > MAX_BUFFER_SIZE {
            self.buffer.clear();
            self.current_data.clear();
            self.current_event_type = None;
            self.current_id = None;
            tracing::error!(
                "SSE buffer exceeded {} bytes — possible malformed stream, resetting",
                MAX_BUFFER_SIZE
            );
            return Vec::new();
        }

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
    ///
    /// Handles the case where the stream ends without a trailing blank line —
    /// any accumulated `data:` field is still emitted as a complete event.
    fn flush(&mut self) -> Option<ServerSentEvent> {
        if self.buffer.is_empty() && self.current_data.is_empty() {
            return None;
        }
        if !self.buffer.is_empty() {
            let line: Vec<u8> = std::mem::take(&mut self.buffer);
            let _ = self.process_line(&line);
        }
        self.emit_event()
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
        pending: VecDeque<ServerSentEvent>,
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
            pending: VecDeque::new(),
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
            let event = this.pending.pop_front().unwrap();
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
                *this.pending = events.into();
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
    use proptest::prelude::*;

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

    #[tokio::test]
    async fn stream_ending_without_trailing_blank_line_emits_event() {
        // When the server closes the connection without sending a trailing
        // blank line after the last data field, flush() should still emit
        // the accumulated event (H2 fix).
        let data = b"data: {\"flushed\":true}";
        let mut stream = stream_from_bytes(data);
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event["flushed"], true);
        assert!(stream.next().await.is_none());
    }

    // ── Property-based tests ───────────────────────────────────────────

    /// Property: the decoder never panics on arbitrary byte input.
    ///
    /// This is the most important property — the SSE decoder consumes
    /// untrusted bytes from the network and must never panic, regardless
    /// of how malformed the input is. It may produce 0 or more events,
    /// but it must always return successfully.
    proptest! {
        #[test]
        fn proptest_decoder_never_panics(
            bytes in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..=MAX_BUFFER_SIZE + 1024)
        ) {
            let mut decoder = SseDecoder::new();
            // Must not panic — this is the core invariant
            let _events = decoder.feed(&bytes);
        }
    }

    /// Property: well-formed SSE events are always parsed correctly,
    /// regardless of how the bytes are chunked.
    ///
    /// Given a valid JSON payload wrapped in SSE framing, splitting it
    /// at any boundary and feeding the chunks one byte at a time should
    /// produce the same events as feeding it all at once.
    proptest! {
        #[test]
        fn proptest_events_parsed_correctly_regardless_of_chunk_boundaries(
            payload in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..100)
        ) {
            // Build a well-formed SSE event with ASCII-safe payload as the data value.
            let safe_payload: String = payload
                .iter()
                .filter(|&&b| b >= 0x20 && b < 0x7f && b != b'"' && b != b'\\')
                .map(|&b| b as char)
                .collect();
            let json_val = serde_json::to_string(&safe_payload).unwrap_or_else(|_| "\"\"".to_string());
            let sse_event = format!("data: {}\n\n", json_val);

            // Feed all at once
            let mut decoder_full = SseDecoder::new();
            let events_full = decoder_full.feed(sse_event.as_bytes());

            // Feed one byte at a time
            let mut decoder_chunked = SseDecoder::new();
            let mut events_chunked = Vec::new();
            for byte in sse_event.as_bytes() {
                events_chunked.extend(decoder_chunked.feed(&[*byte]));
            }

            // Both approaches should produce the same number of events
            prop_assert_eq!(
                events_full.len(),
                events_chunked.len()
            );

            // If events were produced, their data should be identical
            for (full, chunked) in events_full.iter().zip(events_chunked.iter()) {
                prop_assert_eq!(&full.data, &chunked.data);
            }
        }
    }

    /// Property: comment lines (starting with ':') are always silently
    /// ignored, even when interspersed with valid events.
    proptest! {
        #[test]
        fn proptest_comment_lines_are_always_ignored(
            comment_text in "[a-zA-Z0-9 ]{0,100}"
        ) {
            let json = "{\"type\":\"test\"}";
            let sse = format!(
                ": {}\ndata: {}\n: another comment\n\n",
                comment_text, json
            );

            let mut decoder = SseDecoder::new();
            let events = decoder.feed(sse.as_bytes());

            // Should produce exactly one event (the data line)
            prop_assert_eq!(events.len(), 1);
            prop_assert_eq!(&events[0].data, json);
        }
    }

    /// Property: CRLF and LF line endings produce identical results.
    proptest! {
        #[test]
        fn proptest_crlf_and_lf_produce_identical_results(
            data_value in "[a-zA-Z0-9 ]{0,100}"
        ) {
            let json = serde_json::to_string(&data_value).unwrap_or_else(|_| "\"\"".to_string());

            let sse_lf = format!("data: {}\n\n", json);
            let sse_crlf = format!("data: {}\r\n\r\n", json);

            let mut decoder_lf = SseDecoder::new();
            let events_lf = decoder_lf.feed(sse_lf.as_bytes());

            let mut decoder_crlf = SseDecoder::new();
            let events_crlf = decoder_crlf.feed(sse_crlf.as_bytes());

            prop_assert_eq!(events_lf.len(), events_crlf.len());
            if !events_lf.is_empty() {
                prop_assert_eq!(&events_lf[0].data, &events_crlf[0].data);
            }
        }
    }

    /// Property: the decoder handles empty data fields (heartbeats)
    /// without producing events.
    proptest! {
        #[test]
        fn proptest_empty_data_fields_produce_no_events(
            count in 0..10usize
        ) {
            let mut sse = String::new();
            for _ in 0..count {
                sse.push_str("data:\n\n");
            }

            let mut decoder = SseDecoder::new();
            let events = decoder.feed(sse.as_bytes());

            // Empty data fields should not produce events (no non-data fields set)
            prop_assert_eq!(events.len(), 0);
        }
    }
}
