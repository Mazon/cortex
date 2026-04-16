//! SSE event loop — subscribe to events, match variants, update state directly.

use futures::StreamExt;
use tracing::{debug, warn};
use std::sync::{Arc, Mutex};

use crate::opencode::client::{
    convert_session_error, extract_permission_fields, extract_session_id, is_safe_tool,
    OpenCodeClient,
};
use crate::state::types::AppState;

/// Run the SSE event loop for a single project's OpenCode client.
/// This is spawned as a tokio task per active project.
pub async fn sse_event_loop(
    client: OpenCodeClient,
    state: Arc<Mutex<AppState>>,
) {
    let mut backoff_ms: u64 = 2000;
    let mut reconnect_count: u64 = 0;

    loop {
        debug!("Subscribing to SSE events from {}", client.base_url());

        match client.subscribe_to_events().await {
            Ok(stream) => {
                backoff_ms = 2000; // Reset backoff on successful connection
                reconnect_count += 1;
                let mut stream = stream;

                if reconnect_count > 1 {
                    debug!(
                        "SSE reconnected successfully (reconnect #{})",
                        reconnect_count - 1,
                    );
                }

                while let Some(event_result) = stream.next().await {
                    let event = match event_result {
                        Ok(e) => e,
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("unknown variant") {
                                debug!("SSE unknown event type: {}", msg);
                            } else {
                                warn!("SSE event error: {}", msg);
                            }
                            continue;
                        }
                    };

                    let mut state = state.lock().unwrap();
                    process_event(&event, &mut state, &client);
                }

                warn!("SSE stream ended, reconnecting...");
            }
            Err(e) => {
                warn!("Failed to subscribe to SSE events: {}", e);
            }
        }

        // Exponential backoff with max 30s
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(30_000);
    }
}

/// Process a single SSE event, updating state directly.
fn process_event(event: &opencode_sdk_rs::resources::event::EventListResponse, state: &mut AppState, client: &OpenCodeClient) {
    // Any incoming SSE event potentially changes the UI — mark for re-render.
    state.mark_render_dirty();

    use opencode_sdk_rs::resources::event::EventListResponse;

    match event {
        EventListResponse::SessionStatus { properties } => {
            let status = properties
                .status
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            state.process_session_status(&properties.session_id, status);
        }

        EventListResponse::SessionIdle { properties } => {
            state.process_session_idle(&properties.session_id);
        }

        EventListResponse::SessionError { properties } => {
            if let Some(ref sid) = properties.session_id {
                let msg = properties
                    .error
                    .as_ref()
                    .map(|e| convert_session_error(e))
                    .unwrap_or_default();
                state.process_session_error(sid, &msg);
            }
        }

        EventListResponse::MessagePartDelta { properties } => {
            state.process_message_part_delta(&properties.session_id, &properties.delta);
        }

        EventListResponse::PermissionAsked { properties } => {
            if let Some((perm_id, session_id, tool_name, desc, _details)) =
                extract_permission_fields(properties)
            {
                state.process_permission_asked(&session_id, &perm_id, &tool_name, &desc);

                // Auto-approve safe tools (fire-and-forget)
                if is_safe_tool(&tool_name) {
                    let client_clone = client.clone();
                    let sid = session_id.clone();
                    let pid = perm_id.clone();
                    let tool_name_clone = tool_name.clone();
                    tokio::spawn(async move {
                        debug!("Auto-approving safe tool: {} ({})", tool_name_clone, pid);
                        if let Err(e) = client_clone.resolve_permission(&sid, &pid, true).await {
                            warn!("Failed to auto-approve permission: {}", e);
                        }
                    });
                }
            }
        }

        EventListResponse::PermissionReplied { properties } => {
            if let Some(task_id) = state.get_task_id_by_session(&properties.session_id).map(|s| s.to_string()) {
                state.resolve_permission_request(&task_id, &properties.request_id, true);
            }
        }

        EventListResponse::QuestionAsked { properties } => {
            let session_id = properties.get("sessionID").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(task_id) = state.get_task_id_by_session(session_id).map(|s| s.to_string()) {
                let question: String = properties
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                state.update_project_status(&task_id);
                let preview: String = question.chars().take(50).collect();
                state.set_notification(
                    format!("Question pending: {}", preview),
                    crate::state::types::NotificationVariant::Warning,
                    10000,
                );
            }
        }

        _ => {} // Ignore events we don't care about
    }
}
