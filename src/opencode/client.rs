//! Thin wrapper around `opencode_sdk_rs::Opencode`.

use anyhow::{Context, Result};
use tracing::{debug, info};
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
        info!("OpenCode client created for {}", base_url);
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
        info!("OpenCode client created for {} (timeout: {}s)", url, config.request_timeout_secs);
        Ok(Self { sdk })
    }

    pub async fn create_session(&self) -> Result<Session> {
        debug!("Creating new session");
        let session = self
            .sdk
            .session()
            .create(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create session: {}", e))?;
        debug!("Session created: {}", session.id);
        Ok(session)
    }

    pub async fn send_prompt(
        &self,
        session_id: &str,
        text: &str,
        agent: Option<&str>,
        model: Option<&str>,
    ) -> Result<SessionMessagesResponseItem> {
        debug!("Sending prompt to session {}: {} chars", session_id, text.len());
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

    pub async fn abort_session(&self, session_id: &str) -> Result<bool> {
        debug!("Aborting session: {}", session_id);
        let result = self
            .sdk
            .session()
            .abort(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to abort session: {}", e))?;
        Ok(result)
    }

    pub async fn get_messages(&self, session_id: &str) -> Result<SessionMessagesResponse> {
        debug!("Fetching messages for session: {}", session_id);
        let messages = self
            .sdk
            .session()
            .messages(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get messages: {}", e))?;
        Ok(messages)
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<bool> {
        debug!("Deleting session: {}", session_id);
        let result = self
            .sdk
            .session()
            .delete(session_id, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete session: {}", e))?;
        Ok(result)
    }

    pub async fn list_sessions(&self) -> Result<SessionListResponse> {
        debug!("Listing sessions");
        let sessions = self
            .sdk
            .session()
            .list(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list sessions: {}", e))?;
        Ok(sessions)
    }

    pub async fn resolve_permission(
        &self,
        session_id: &str,
        permission_id: &str,
        approved: bool,
    ) -> Result<()> {
        debug!("Resolving permission {} in session {}: approved={}", permission_id, session_id, approved);
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

    pub async fn subscribe_to_events(&self) -> Result<SseStream<EventListResponse>> {
        debug!("Subscribing to SSE events");
        let stream = self
            .sdk
            .event()
            .list()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to events: {}", e))?;
        Ok(stream)
    }

    pub async fn get_app_info(&self) -> Result<App> {
        debug!("Fetching app info");
        let app = self
            .sdk
            .app()
            .get(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get app info: {}", e))?;
        Ok(app)
    }

    pub fn build_prompt_for_agent(task: &CortexTask, agent: &str, context: Option<&str>) -> String {
        let mut parts = Vec::new();
        parts.push(format!("[{}] Working on task #{}: {}", agent, task.number, task.title));
        if !task.description.is_empty() {
            parts.push(format!("\nDescription:\n{}", task.description));
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

    pub fn sdk(&self) -> &opencode_sdk_rs::Opencode {
        &self.sdk
    }

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
            TaskMessagePart::Tool {
                id: tool_part.id.clone(),
                tool: tool_part.tool.clone(),
                state: cortex_state,
                input, output, error,
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

/// Extract session_id from an EventListResponse variant.
#[allow(dead_code)]
pub fn extract_session_id(event: &EventListResponse) -> Option<String> {
    match event {
        EventListResponse::SessionStatus { properties } => Some(properties.session_id.clone()),
        EventListResponse::SessionIdle { properties } => Some(properties.session_id.clone()),
        EventListResponse::SessionError { properties } => properties.session_id.clone(),
        EventListResponse::MessagePartDelta { properties } => Some(properties.session_id.clone()),
        EventListResponse::MessageUpdated { properties } => match &properties.info {
            Message::User(u) => Some(u.session_id.clone()),
            Message::Assistant(a) => Some(a.session_id.clone()),
        },
        EventListResponse::PermissionAsked { properties } => properties.get("sessionID").and_then(|v| v.as_str()).map(String::from),
        EventListResponse::PermissionReplied { properties } => Some(properties.session_id.clone()),
        EventListResponse::QuestionAsked { properties } => properties.get("sessionID").and_then(|v| v.as_str()).map(String::from),
        EventListResponse::QuestionReplied { properties } => Some(properties.session_id.clone()),
        _ => None,
    }
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
}
