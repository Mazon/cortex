//! Thin wrapper around `opencode_sdk_rs::Opencode`.

use anyhow::{Context, Result};
use opencode_sdk_rs::resources::app::App;
use opencode_sdk_rs::resources::event::EventListResponse;
use opencode_sdk_rs::resources::session::{
    Message, Part, PartInput, Session, SessionChatModel, SessionChatParams, SessionListResponse,
    SessionMessagesResponse, SessionMessagesResponseItem, TextPartInput,
    ToolState as SdkToolState,
};
use opencode_sdk_rs::resources::shared::SessionError as SdkSessionError;
use opencode_sdk_rs::SseStream;
use std::time::Duration;

use crate::config::types::OpenCodeConfig;
use crate::state::types::{CortexTask, MessageRole, TaskMessage, TaskMessagePart, ToolState};

/// Cortex-specific wrapper around the `opencode-sdk-rs` `Opencode` client.
#[derive(Clone)]
pub struct OpenCodeClient {
    sdk: opencode_sdk_rs::Opencode,
}

impl OpenCodeClient {
    /// Create a new `OpenCodeClient` connected to the given base URL.
    pub fn new(base_url: &str) -> Result<Self> {
        let sdk = opencode_sdk_rs::Opencode::builder()
            .base_url(base_url)
            .timeout(Duration::from_secs(120))
            .max_retries(2)
            .build()
            .context("Failed to build OpenCode SDK client")?;
        Ok(Self { sdk })
    }

    /// Create a new `OpenCodeClient` from an `OpenCodeConfig`.
    pub fn from_config(config: &OpenCodeConfig) -> Result<Self> {
        let url = format!("http://{}:{}", config.hostname, config.port);
        let sdk = opencode_sdk_rs::Opencode::builder()
            .base_url(&url)
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .max_retries(2)
            .build()
            .context("Failed to build OpenCode SDK client")?;
        Ok(Self { sdk })
    }

    /// Create a new OpenCode session.
    pub async fn create_session(&self) -> Result<Session> {
        let session = self
            .sdk
            .session()
            .create(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create session: {}", e))?;
        Ok(session)
    }

    /// Send a chat prompt to an existing session. Optionally specify an agent
    /// name and/or model override.
    pub async fn send_prompt(
        &self,
        session_id: &str,
        text: &str,
        agent: Option<&str>,
        model: Option<&str>,
    ) -> Result<SessionMessagesResponseItem> {
        tracing::debug!(
            "send_prompt: session={}, agent={:?}, model={:?}, text_len={}",
            session_id, agent, model, text.len()
        );
        let text_input = TextPartInput {
            text: text.to_string(),
            id: None,
            synthetic: None,
            ignored: None,
            time: None,
            metadata: None,
        };
        let params = SessionChatParams {
            parts: vec![PartInput::Text(text_input)],
            model: model.map(|m| {
                let (provider_id, model_id) = m.split_once('/').unwrap_or(("z.ai", m));
                SessionChatModel {
                    provider_id: provider_id.to_string(),
                    model_id: model_id.to_string(),
                }
            }),
            message_id: None,
            agent: agent.map(String::from),
            no_reply: None,
            format: None,
            system: None,
            variant: None,
            tools: None,
        };
        let result = self
            .sdk
            .session()
            .chat(session_id, &params, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send prompt: {}", e))?;
        Ok(result)
    }

    /// Abort an active session. Returns `true` if the abort was acknowledged.
    pub async fn abort_session(&self, session_id: &str) -> Result<bool> {
        let result = self
            .sdk
            .session()
            .abort(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to abort session: {}", e))?;
        Ok(result)
    }

    /// Fetch all messages for a session.
    pub async fn get_messages(&self, session_id: &str) -> Result<SessionMessagesResponse> {
        let messages = self
            .sdk
            .session()
            .messages(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get messages: {}", e))?;
        Ok(messages)
    }

    /// Fetch messages for a subagent session and convert them to Cortex types.
    ///
    /// This is used for lazy-loading subagent output when the user drills
    /// down into a subagent via `ctrl+x`.
    pub async fn fetch_subagent_messages(&self, session_id: &str) -> Result<Vec<TaskMessage>> {
        let response = self.get_messages(session_id)
            .await
            .with_context(|| format!("Failed to fetch messages for subagent session {}", session_id))?;
        let messages: Vec<TaskMessage> = response
            .iter()
            .map(convert_sdk_message)
            .collect();
        Ok(messages)
    }

    /// Fetch messages for any session and convert them to Cortex types.
    ///
    /// Used after a session completes to persist the full message history
    /// into `session.messages`, replacing the transient `streaming_text`.
    pub async fn fetch_session_messages(&self, session_id: &str) -> Result<Vec<TaskMessage>> {
        let response = self.get_messages(session_id)
            .await
            .with_context(|| format!("Failed to fetch messages for session {}", session_id))?;
        let messages: Vec<TaskMessage> = response
            .iter()
            .map(convert_sdk_message)
            .collect();
        Ok(messages)
    }

    /// Delete a session. Returns `true` if the deletion was acknowledged.
    pub async fn delete_session(&self, session_id: &str) -> Result<bool> {
        let result = self
            .sdk
            .session()
            .delete(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete session: {}", e))?;
        Ok(result)
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Result<SessionListResponse> {
        let sessions = self
            .sdk
            .session()
            .list(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list sessions: {}", e))?;
        Ok(sessions)
    }

    /// Resolve a permission request (approve or reject).
    pub async fn resolve_permission(
        &self,
        session_id: &str,
        permission_id: &str,
        approved: bool,
    ) -> Result<()> {
        let reply = if approved { "once" } else { "reject" };
        let body = serde_json::json!({ "reply": reply });
        let path = format!("/session/{}/permission/{}", session_id, permission_id);
        let _: serde_json::Value = self
            .sdk
            .post(&path, Some(&body), None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve permission: {}", e))?;
        Ok(())
    }

    /// Answer a question from an agent.
    pub async fn resolve_question(
        &self,
        session_id: &str,
        question_id: &str,
        answer: &str,
    ) -> Result<()> {
        let body = serde_json::json!({ "answer": answer });
        let path = format!("/session/{}/question/{}", session_id, question_id);
        let _: serde_json::Value = self
            .sdk
            .post(&path, Some(&body), None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve question: {}", e))?;
        Ok(())
    }

    /// Subscribe to the OpenCode SSE event stream. Returns a stream of
    /// [`EventListResponse`] items that yields events as they arrive.
    pub async fn subscribe_to_events(&self) -> Result<SseStream<EventListResponse>> {
        let stream = self
            .sdk
            .event()
            .list()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to events: {}", e))?;
        Ok(stream)
    }

    /// Fetch OpenCode app info (version, status, etc.).
    pub async fn get_app_info(&self) -> Result<App> {
        let app = self
            .sdk
            .app()
            .get(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get app info: {}", e))?;
        Ok(app)
    }

    /// Build a prompt string for an agent working on a task.
    /// Uses the description as the primary content, with task number for context.
    /// Includes plan output and optional context if available.
    pub fn build_prompt_for_agent(task: &CortexTask, agent: &str, context: Option<&str>) -> String {
        let mut parts = Vec::new();
        parts.push(format!("[{}] Working on task #{}", agent, task.number));
        if !task.description.is_empty() {
            parts.push(format!("\n{}", task.description));
        }
        if let Some(plan) = &task.plan_output {
            if !plan.is_empty() {
                parts.push(format!("\nPlan:\n{}", plan));
            }
        }
        if let Some(ctx) = context {
            if !ctx.is_empty() {
                parts.push(format!("\nContext:\n{}", ctx));
            }
        }
        parts.join("\n")
    }

    /// Get a reference to the underlying SDK client.
    pub fn sdk(&self) -> &opencode_sdk_rs::Opencode {
        &self.sdk
    }

    /// Get the base URL this client is connected to.
    pub fn base_url(&self) -> &str {
        self.sdk.base_url()
    }
}

// ─── SDK→App Type Conversion Helpers ──────────────────────────────────────

/// Convert an SDK `SessionMessagesResponseItem` into a Cortex `TaskMessage`.
pub fn convert_sdk_message(item: &SessionMessagesResponseItem) -> TaskMessage {
    let (id, role, created_at) = match &item.info {
        Message::User(user_msg) => (
            user_msg.id.clone(),
            MessageRole::User,
            Some(format!("t{}", user_msg.time.created as i64)),
        ),
        Message::Assistant(asst_msg) => (
            asst_msg.id.clone(),
            MessageRole::Assistant,
            Some(format!("t{}", asst_msg.time.created as i64)),
        ),
    };
    let parts: Vec<TaskMessagePart> = item.parts.iter().map(convert_sdk_part).collect();
    TaskMessage { id, role, parts, created_at }
}

/// Convert a single SDK `Part` variant into a Cortex `TaskMessagePart`.
pub fn convert_sdk_part(part: &Part) -> TaskMessagePart {
    match part {
        Part::Text(text_part) => TaskMessagePart::Text { text: text_part.text.clone() },
        Part::Tool(tool_part) => {
            let cortex_state = convert_tool_state(&tool_part.state);
            let (input, output, error) = match &tool_part.state {
                SdkToolState::Pending(_) => (None, None, None),
                SdkToolState::Running(running) => (
                    running.input.as_ref().map(|v| serde_json::to_string_pretty(v).unwrap_or_default()),
                    None, None,
                ),
                SdkToolState::Completed(completed) => (
                    Some(serde_json::to_string_pretty(&completed.input).unwrap_or_default()),
                    Some(completed.output.clone()), None,
                ),
                SdkToolState::Error(error_state) => (
                    Some(serde_json::to_string_pretty(&error_state.input).unwrap_or_default()),
                    None, Some(error_state.error.clone()),
                ),
            };
            let cached_summary = input.as_ref().map(|i| {
                crate::state::types::extract_tool_summary(&tool_part.tool, i)
            });
            TaskMessagePart::Tool {
                id: tool_part.id.clone(),
                tool: tool_part.tool.clone(),
                state: cortex_state,
                input, output, error,
                cached_summary,
            }
        }
        Part::StepStart(s) => TaskMessagePart::StepStart { id: s.id.clone() },
        Part::StepFinish(s) => TaskMessagePart::StepFinish { id: s.id.clone() },
        Part::Agent(a) => TaskMessagePart::Agent { id: a.id.clone(), agent: a.name.clone() },
        Part::Reasoning(r) => TaskMessagePart::Reasoning { text: r.text.clone() },
        _ => TaskMessagePart::Unknown,
    }
}

/// Convert an SDK `ToolState` to a Cortex `ToolState`.
pub fn convert_tool_state(state: &SdkToolState) -> ToolState {
    match state {
        SdkToolState::Pending(_) => ToolState::Pending,
        SdkToolState::Running(_) => ToolState::Running,
        SdkToolState::Completed(_) => ToolState::Completed,
        SdkToolState::Error(_) => ToolState::Error,
    }
}

/// Convert an SDK `SessionError` to a human-readable error string.
pub fn convert_session_error(error: &SdkSessionError) -> String {
    match error {
        SdkSessionError::MessageAbortedError { data } => data.message.clone().unwrap_or_else(|| "Message aborted".to_string()),
        SdkSessionError::ProviderAuthError { data } => format!("Provider auth error: {} (provider: {})", data.message, data.provider_id),
        SdkSessionError::UnknownError { data } => data.message.clone(),
        SdkSessionError::ContextOverflowError { data } => data.message.clone(),
        SdkSessionError::APIError { data } => {
            let status = data.status_code.map(|s| format!(" (status: {})", s)).unwrap_or_default();
            format!("API error: {}{}", data.message, status)
        }
        SdkSessionError::MessageOutputLengthError { .. } => "Message output too long".to_string(),
        SdkSessionError::StructuredOutputError { data } => format!("Structured output error: {} (retries: {})", data.message, data.retries),
        _ => "Unknown error".to_string(),
    }
}

/// Extract permission fields from a `PermissionAsked` event's JSON properties.
pub fn extract_permission_fields(
    properties: &serde_json::Value,
) -> Option<(String, String, String, String, Option<String>)> {
    let id = properties.get("id")?.as_str()?.to_string();
    let session_id = properties.get("sessionID")?.as_str()?.to_string();
    let tool_name = properties.get("tool").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let description = properties.get("title").or_else(|| properties.get("description")).and_then(|v| v.as_str()).unwrap_or("Permission request").to_string();
    let details = properties.get("details").or_else(|| properties.get("input")).and_then(|v| v.as_str()).map(|s| s.to_string());
    Some((id, session_id, tool_name, description, details))
}

/// Tools that are safe to auto-approve (read-only operations only).
/// NEVER include "bash", "write", or other mutating tools here —
/// arbitrary commands and file modifications must require explicit user approval.
pub fn is_safe_tool(tool_name: &str) -> bool {
    matches!(tool_name, "read" | "glob" | "grep" | "list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use opencode_sdk_rs::resources::event::EventListResponse;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn safe_tools() {
        assert!(is_safe_tool("read"));
        assert!(is_safe_tool("glob"));
        assert!(is_safe_tool("grep"));
        assert!(is_safe_tool("list"));
    }

    #[test]
    fn unsafe_tools() {
        assert!(!is_safe_tool("write"));
        assert!(!is_safe_tool("bash"));
        assert!(!is_safe_tool("sh"));
        assert!(!is_safe_tool("exec"));
        assert!(!is_safe_tool("python"));
        assert!(!is_safe_tool(""));
        assert!(!is_safe_tool("unknown"));
    }

    // -----------------------------------------------------------------------
    // Full-pipeline SSE stream tests
    // -----------------------------------------------------------------------
    //
    // These tests spin up a minimal HTTP/1.1 server on localhost, serve SSE
    // payloads through it, and consume the resulting `SseStream<EventListResponse>`
    // via the real `OpenCodeClient`.  They exercise the complete pipeline:
    //
    //   HTTP response → hpx byte stream → SseStream → SSE decode → JSON parse

    /// Spin up a minimal HTTP/1.1 server that writes `payload` as the SSE
    /// body, then closes the connection.
    async fn spawn_sse_server(payload: &str) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = payload.to_owned();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Consume the incoming HTTP request.
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;

            let header = [
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: close\r\n",
                "\r\n",
            ]
            .concat();

            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        addr
    }

    /// Like [`spawn_sse_server`] but writes the payload in two separate TCP
    /// writes with a short delay, increasing the likelihood that the client
    /// receives them as distinct byte-stream chunks.
    async fn spawn_sse_server_split(part1: &str, part2: &str) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (p1, p2) = (part1.to_owned(), part2.to_owned());

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;

            let header = [
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: close\r\n",
                "\r\n",
            ]
            .concat();

            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(p1.as_bytes()).await.unwrap();

            // Brief pause so the client has a chance to poll the first chunk.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            stream.write_all(p2.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        addr
    }

    /// Collect all events from the stream into a `Vec`.
    async fn collect_events(
        client: &OpenCodeClient,
    ) -> Vec<Result<EventListResponse, opencode_sdk_rs::OpencodeError>> {
        let stream = client.subscribe_to_events().await.unwrap();
        stream.collect().await
    }

    /// A well-formed SSE stream with a single event should produce exactly one
    /// typed `EventListResponse`.
    #[tokio::test]
    async fn sse_well_formed_single_event() {
        let payload = "data: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"sess_001\",\"status\":{\"type\":\"running\"}}}\n\n";
        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1, "expected exactly 1 event, got {}", events.len());
        match &events[0] {
            Ok(EventListResponse::SessionStatus { properties }) => {
                assert_eq!(properties.session_id, "sess_001");
            }
            other => panic!("expected SessionStatus, got {other:?}"),
        }
    }

    /// Multiple SSE events in the same stream should be parsed in order.
    #[tokio::test]
    async fn sse_multiple_events_in_sequence() {
        let payload = concat!(
            "data: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"s1\",\"status\":{\"type\":\"running\"}}}\n\n",
            "data: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"s1\"}}\n\n",
            "data: {\"type\":\"file.edited\",\"properties\":{\"file\":\"src/main.rs\"}}\n\n",
        );

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 3);

        match &events[0] {
            Ok(EventListResponse::SessionStatus { properties }) => {
                assert_eq!(properties.session_id, "s1");
            }
            other => panic!("event 0: expected SessionStatus, got {other:?}"),
        }
        match &events[1] {
            Ok(EventListResponse::SessionIdle { properties }) => {
                assert_eq!(properties.session_id, "s1");
            }
            other => panic!("event 1: expected SessionIdle, got {other:?}"),
        }
        match &events[2] {
            Ok(EventListResponse::FileEdited { properties }) => {
                assert_eq!(properties.file, "src/main.rs");
            }
            other => panic!("event 2: expected FileEdited, got {other:?}"),
        }
    }

    /// When an SSE event has multiple `data:` lines, they should be joined
    /// with `\n` before JSON parsing.  JSON allows whitespace (including
    /// newlines) between structural tokens, so a multi-line JSON object is
    /// still valid.
    #[tokio::test]
    async fn sse_multiline_data_joined_with_newline() {
        let payload = concat!(
            "data: {\"type\":\"session.status\",\n",
            "data:  \"properties\":{\"sessionID\":\"s_ml\",\"status\":{\"type\":\"running\"}}}\n",
            "\n",
        );

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(EventListResponse::SessionStatus { properties }) => {
                assert_eq!(properties.session_id, "s_ml");
            }
            other => panic!("expected SessionStatus, got {other:?}"),
        }
    }

    /// An SSE event whose data is split across two TCP writes should still be
    /// correctly assembled by the internal `SseDecoder` buffer.
    #[tokio::test]
    async fn sse_event_split_across_chunks() {
        let part1 = "data: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"s_split\"}}";
        let part2 = "\n\n";

        let addr = spawn_sse_server_split(part1, part2).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(
            events.len(),
            1,
            "expected 1 event from split chunks, got {}",
            events.len()
        );
        match &events[0] {
            Ok(EventListResponse::SessionIdle { properties }) => {
                assert_eq!(properties.session_id, "s_split");
            }
            other => panic!("expected SessionIdle, got {other:?}"),
        }
    }

    /// SSE events with empty `data:` fields (heartbeats) should be silently
    /// skipped by the stream.
    #[tokio::test]
    async fn sse_empty_data_events_are_skipped() {
        let payload = concat!(
            "data: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"s_skip\"}}\n\n",
            // Empty data heartbeat
            "data:\n\n",
            // Another real event
            "data: {\"type\":\"file.edited\",\"properties\":{\"file\":\"Cargo.toml\"}}\n\n",
            // Bare blank line — no fields set, so no event at all
            "\n",
            // Third real event
            "data: {\"type\":\"server.connected\",\"properties\":{}}\n\n",
        );

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        // Only the 3 real events should surface.
        assert_eq!(
            events.len(),
            3,
            "expected 3 events (empty skipped), got {}",
            events.len()
        );
        match &events[0] {
            Ok(EventListResponse::SessionIdle { properties }) => {
                assert_eq!(properties.session_id, "s_skip");
            }
            other => panic!("event 0: expected SessionIdle, got {other:?}"),
        }
        match &events[1] {
            Ok(EventListResponse::FileEdited { properties }) => {
                assert_eq!(properties.file, "Cargo.toml");
            }
            other => panic!("event 1: expected FileEdited, got {other:?}"),
        }
        match &events[2] {
            Ok(EventListResponse::ServerConnected { .. }) => {}
            other => panic!("event 2: expected ServerConnected, got {other:?}"),
        }
    }

    /// SSE comment lines (prefixed with `:`) should be silently ignored.
    #[tokio::test]
    async fn sse_comment_lines_are_ignored() {
        let payload = concat!(
            ": this is a comment and should be ignored\n",
            ": another comment\n",
            "data: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"s_cmt\",\"status\":{\"type\":\"running\"}}}\n",
            ": trailing comment\n",
            "\n",
        );

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(EventListResponse::SessionStatus { properties }) => {
                assert_eq!(properties.session_id, "s_cmt");
            }
            other => panic!("expected SessionStatus, got {other:?}"),
        }
    }

    /// When the server closes the connection the stream should terminate
    /// cleanly (return `None`) rather than erroring.
    #[tokio::test]
    async fn sse_stream_ends_on_connection_close() {
        let payload = "data: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"s_end\"}}\n\n";
        let addr = spawn_sse_server(payload).await;

        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let mut stream = client.subscribe_to_events().await.unwrap();

        let e = stream.next().await.expect("expected event").expect("event should be Ok");
        match &e {
            EventListResponse::SessionIdle { properties } => {
                assert_eq!(properties.session_id, "s_end");
            }
            other => panic!("expected SessionIdle, got {other:?}"),
        }

        assert!(
            stream.next().await.is_none(),
            "stream should end after server closes"
        );
    }

    /// An SSE event with an `event:` type field and an `id:` field should
    /// still parse correctly — only the `data:` field is used for JSON
    /// deserialization.
    #[tokio::test]
    async fn sse_event_and_id_fields_ignored_for_parsing() {
        let payload = concat!(
            "id: 42\n",
            "event: custom-event\n",
            "data: {\"type\":\"file.edited\",\"properties\":{\"file\":\"README.md\"}}\n",
            "\n",
        );

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(EventListResponse::FileEdited { properties }) => {
                assert_eq!(properties.file, "README.md");
            }
            other => panic!("expected FileEdited, got {other:?}"),
        }
    }

    /// CRLF line endings should be handled correctly (the SSE spec requires
    /// both LF and CRLF to be treated as line terminators).
    #[tokio::test]
    async fn sse_crlf_line_endings() {
        let payload = "data: {\"type\":\"server.connected\",\"properties\":{}}\r\n\r\n";

        let addr = spawn_sse_server(payload).await;
        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Ok(EventListResponse::ServerConnected { .. })
        ));
    }

    /// Invalid JSON in the `data:` field should produce a deserialization
    /// error rather than panicking or being silently dropped.
    #[tokio::test]
    async fn sse_invalid_json_produces_error() {
        let payload = "data: {not valid json}\n\n";
        let addr = spawn_sse_server(payload).await;

        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert_eq!(events.len(), 1);
        assert!(
            events[0].is_err(),
            "expected a deserialization error for invalid JSON, got {:?}",
            events[0]
        );
    }

    /// An empty SSE stream (server sends headers but no events) should
    /// produce zero events and terminate cleanly.
    #[tokio::test]
    async fn sse_empty_stream_no_events() {
        let payload = "";
        let addr = spawn_sse_server(payload).await;

        let client = OpenCodeClient::new(&format!("http://{addr}")).unwrap();
        let events = collect_events(&client).await;

        assert!(
            events.is_empty(),
            "expected no events from empty stream, got {}",
            events.len()
        );
    }
}
