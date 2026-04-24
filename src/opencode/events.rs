//! SSE event loop — subscribe to events, match variants, update state directly.

use futures::StreamExt;
use tracing::{debug, warn};
use std::sync::{Arc, Mutex};

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::opencode::client::{
    convert_session_error, extract_permission_fields, is_safe_tool,
    OpenCodeClient,
};
use crate::orchestration::engine::{on_agent_completed, on_task_moved, AutoProgressAction};
use crate::state::types::{AgentStatus, AppState};

/// Default maximum consecutive SSE reconnection attempts.
/// Used when the config field is 0 (which would mean "retry forever").
const DEFAULT_SSE_MAX_RETRIES: u32 = 50;

/// Run the SSE event loop for a single project's OpenCode client.
/// This is spawned as a tokio task per active project.
///
/// The `shutdown` receiver is watched so the loop can exit cleanly when the
/// app is shutting down, instead of relying solely on task cancellation via
/// `abort()`.
pub async fn sse_event_loop(
    client: OpenCodeClient,
    state: Arc<Mutex<AppState>>,
    columns_config: ColumnsConfig,
    opencode_config: OpenCodeConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut backoff_ms: u64 = 2000;
    let mut reconnect_count: u64 = 0;
    let mut reconnect_attempt: u32 = 0;

    // Effective max retries: use config value, but treat 0 as "use default"
    // to avoid accidentally retrying forever.
    let max_retries = if opencode_config.sse_max_retries == 0 {
        DEFAULT_SSE_MAX_RETRIES
    } else {
        opencode_config.sse_max_retries
    };

    loop {
        // Check shutdown before each connection attempt.
        if *shutdown.borrow() {
            debug!("SSE event loop shutting down (received signal)");
            return;
        }

        debug!("Subscribing to SSE events from {}", client.base_url());

        match client.subscribe_to_events().await {
            Ok(stream) => {
                backoff_ms = 2000; // Reset backoff on successful connection
                reconnect_count += 1;
                reconnect_attempt = 0; // Reset consecutive failure counter on success
                let mut stream = stream;

                // Mark reconnection complete — we have a live stream.
                {
                    let mut state = state.lock().unwrap();
                    state.connected = true;
                    state.reconnecting = false;
                    state.reconnect_attempt = 0;
                    state.permanently_disconnected = false;
                }

                if reconnect_count > 1 {
                    debug!(
                        "SSE reconnected successfully (reconnect #{})",
                        reconnect_count - 1,
                    );
                }

                loop {
                    tokio::select! {
                        event_result = stream.next() => {
                            let Some(event_result) = event_result else {
                                // Stream closed by the server.
                                warn!("SSE stream ended, reconnecting...");
                                {
                                    let mut state = state.lock().unwrap();
                                    state.reconnecting = true;
                                }
                                break;
                            };

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

                            let (action, finalize_session_id) = {
                                let mut state = state.lock().unwrap();
                                let (action, finalize_session_id) =
                                    process_event(&event, &mut state, &client, &columns_config);
                                // Close the race window: set Running status + agent_type while
                                // still holding the lock, same as the manual move path in app.rs.
                                // Also capture previous agent for cross-contamination detection.
                                let action = match action {
                                    Some(a) => {
                                        let previous_agent = state.tasks.get(&a.task_id)
                                            .and_then(|t| t.agent_type.clone());
                                        state.update_task_agent_status(&a.task_id, AgentStatus::Running);
                                        state.set_task_agent_type(&a.task_id, Some(a.agent.clone()));
                                        Some((a, previous_agent))
                                    }
                                    None => None,
                                };
                                (action, finalize_session_id)
                            };

                            // Start deferred agent if auto-progression triggered one.
                            // This must happen after the MutexGuard is dropped to avoid
                            // deadlock (start_agent acquires its own lock).
                            if let Some((action, previous_agent)) = action {
                                on_task_moved(
                                    &action.task_id,
                                    &action.target_column,
                                    &state,
                                    &client,
                                    &columns_config,
                                    &opencode_config,
                                    previous_agent,
                                );
                            }

                            // Finalize streaming text into persistent message history
                            // when a session completes or goes idle.
                            if let Some(session_id) = finalize_session_id {
                                // Look up the task_id while we can, but the actual
                                // fetch must happen after the lock is released.
                                let task_id = {
                                    let s = state.lock().unwrap();
                                    s.get_task_id_by_session(&session_id)
                                        .map(|tid| tid.to_string())
                                };
                                if let Some(task_id) = task_id {
                                    let client_clone = client.clone();
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        // Check if there's streaming text to finalize
                                        let needs_finalize = {
                                            let s = state_clone.lock().unwrap();
                                            s.task_sessions.get(&task_id)
                                                .is_some_and(|ts| ts.streaming_text.is_some())
                                        };
                                        if !needs_finalize {
                                            debug!(
                                                "No streaming text to finalize for session {} (task {})",
                                                session_id, task_id
                                            );
                                            return;
                                        }

                                        debug!(
                                            "Finalizing streaming text for session {} (task {})",
                                            session_id, task_id
                                        );
                                        match client_clone.fetch_session_messages(&session_id).await {
                                            Ok(messages) => {
                                                let mut s = state_clone.lock().unwrap();
                                                let msg_count = messages.len();
                                                s.finalize_session_streaming(&task_id, messages);
                                                debug!(
                                                    "Finalized session {}: {} messages, streaming cleared",
                                                    session_id, msg_count
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "Failed to finalize session {} (task {}): {}",
                                                    session_id, task_id, e
                                                );
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        result = shutdown.changed() => {
                            if result.is_err() || *shutdown.borrow() {
                                debug!("SSE event loop shutting down (received signal during stream)");
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to subscribe to SSE events: {}", e);
                {
                    let mut state = state.lock().unwrap();
                    state.reconnecting = true;
                }
            }
        }

        // Check if we've exceeded the max retry limit.
        if reconnect_attempt >= max_retries {
            warn!(
                "SSE reconnection failed after {} consecutive attempts (max: {}). \
                 Giving up — the project will be marked as permanently disconnected. \
                 Restart the application to retry.",
                reconnect_attempt, max_retries,
            );
            {
                let mut state = state.lock().unwrap();
                state.reconnecting = false;
                state.connected = false;
                state.permanently_disconnected = true;
                state.reconnect_attempt = 0;
                state.mark_render_dirty();
            }
            return;
        }

        // Exponential backoff with max 30s, but also break on shutdown.
        reconnect_attempt += 1;
        {
            let mut state = state.lock().unwrap();
            state.reconnecting = true;
            state.reconnect_attempt = reconnect_attempt;
            state.mark_render_dirty();
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    debug!("SSE event loop shutting down during backoff");
                    return;
                }
            }
        }
        backoff_ms = (backoff_ms * 2).min(30_000);
    }
}

/// Process a single SSE event, updating state directly.
/// Returns a tuple of:
/// - `Option<AutoProgressAction>` if the event triggered auto-progression
///   and the target column has a configured agent. The caller is responsible
///   for starting the agent after releasing the MutexGuard.
/// - `Option<String>` — a session ID whose streaming output should be
///   finalized into persistent message history.  The caller spawns a
///   background task to fetch the complete messages and update state.
fn process_event(
    event: &opencode_sdk_rs::resources::event::EventListResponse,
    state: &mut AppState,
    client: &OpenCodeClient,
    columns_config: &ColumnsConfig,
) -> (Option<AutoProgressAction>, Option<String>) {
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
            // When the session completes, signal the caller to finalize
            // streaming text into persistent message history.
            let finalize = if matches!(status, "complete" | "completed") {
                Some(properties.session_id.clone())
            } else {
                None
            };
            (None, finalize)
        }

        EventListResponse::SessionIdle { properties } => {
            let action = if let Some(task_id) = state.process_session_idle(&properties.session_id) {
                // Trigger auto-progression if configured for this column
                on_agent_completed(&task_id, state, columns_config)
            } else {
                None
            };
            // Finalize streaming text on session idle (agent is done)
            (action, Some(properties.session_id.clone()))
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
            (None, None)
        }

        EventListResponse::MessagePartDelta { properties } => {
            state.process_message_part_delta(
                &properties.session_id,
                &properties.message_id,
                &properties.part_id,
                &properties.field,
                &properties.delta,
            );
            (None, None)
        }

        EventListResponse::PermissionAsked { properties } => {
            if let Some((perm_id, session_id, tool_name, desc, _details)) =
                extract_permission_fields(properties)
            {
                if is_safe_tool(&tool_name) {
                    // Auto-approve safe tools (read-only: read, glob, grep, list).
                    // Skip adding to pending_permissions to avoid a visual flash
                    // on the task card — the user gets a notification instead.
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

                    // Show a brief, non-intrusive notification
                    let preview: String = desc.chars().take(50).collect();
                    let preview = preview.trim_end();
                    state.set_notification(
                        format!("Auto-approved: {} — {}", tool_name, preview),
                        crate::state::types::NotificationVariant::Info,
                        2000,
                    );
                } else {
                    // Non-safe tools require explicit user approval
                    state.process_permission_asked(&session_id, &perm_id, &tool_name, &desc);
                }
            }
            (None, None)
        }

        EventListResponse::PermissionReplied { properties } => {
            if let Some(task_id) = state.get_task_id_by_session(&properties.session_id).map(|s| s.to_string()) {
                let approved = matches!(
                    properties.reply,
                    opencode_sdk_rs::resources::event::PermissionReply::Once
                        | opencode_sdk_rs::resources::event::PermissionReply::Always
                );
                state.resolve_permission_request(&task_id, &properties.request_id, approved);
            }
            (None, None)
        }

        EventListResponse::QuestionAsked { properties } => {
            let session_id = properties.get("sessionID").and_then(|v| v.as_str()).unwrap_or("");

            // Route to parent task if this is a subagent session
            let task_id = if let Some(parent) = state.get_parent_task_for_subagent(session_id) {
                Some(parent.to_string())
            } else {
                state.get_task_id_by_session(session_id).map(|s| s.to_string())
            };

            if let Some(task_id) = task_id {
                let question_id: String = properties
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let question_text: String = properties
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let answers: Vec<String> = properties
                    .get("answers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let request = crate::state::types::QuestionRequest {
                    id: question_id,
                    session_id: session_id.to_string(),
                    question: question_text.clone(),
                    answers,
                    status: "pending".to_string(),
                };
                state.add_question_request(&task_id, request);
                state.update_project_status(&task_id);

                let preview: String = question_text.chars().take(50).collect();
                state.set_notification(
                    format!("Question pending: {}", preview),
                    crate::state::types::NotificationVariant::Warning,
                    10000,
                );
            }
            (None, None)
        }

        EventListResponse::QuestionReplied { properties } => {
            if let Some(task_id) = state.get_task_id_by_session(&properties.session_id).map(|s| s.to_string()) {
                state.resolve_question_request(&task_id, &properties.request_id);
            }
            (None, None)
        }

        EventListResponse::QuestionRejected { properties } => {
            if let Some(task_id) = state.get_task_id_by_session(&properties.session_id).map(|s| s.to_string()) {
                state.resolve_question_request(&task_id, &properties.request_id);
            }
            (None, None)
        }

        _ => (None, None), // Ignore events we don't care about
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{ColumnConfig, ColumnsConfig};
    use crate::state::types::*;
    use opencode_sdk_rs::resources::event::EventListResponse;

    /// Build a minimal `AppState` with a task that has a known session mapping.
    fn make_test_state() -> (AppState, String, String) {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        state.add_project(project);
        state.active_project_id = Some("proj-1".to_string());

        let task_id = "task-1".to_string();
        let session_id = "session-abc".to_string();
        let task = CortexTask {
            id: task_id.clone(),
            number: 1,
            title: "Test Task".to_string(),
            description: String::new(),
            column: KanbanColumn("planning".to_string()),
            session_id: Some(session_id.clone()),
            agent_type: Some("planning".to_string()),
            agent_status: AgentStatus::Running,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        state.tasks.insert(task_id.clone(), task);
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default()
            .push(task_id.clone());
        state
            .session_to_task
            .insert(session_id.clone(), task_id.clone());

        // Tests that need a client construct their own; process_event only uses
        // the client for resolve_permission in the auto-approve path.

        (state, task_id, session_id)
    }

    /// Build a default `ColumnsConfig` with auto-progression on "planning" → "running".
    fn make_columns_config() -> ColumnsConfig {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "todo".to_string(),
                    display_name: Some("Todo".to_string()),
                    visible: true,
                    agent: None,
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "planning".to_string(),
                    display_name: Some("Plan".to_string()),
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some("running".to_string()),
                },
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: Some("Run".to_string()),
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        config
    }

    // ── SessionStatus ───────────────────────────────────────────────────

    #[test]
    fn session_status_running_updates_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
        // render_dirty should be set
        assert!(state.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
        // No finalization for "running" status
        assert!(_finalize.is_none());
    }

    #[test]
    fn session_status_completed_updates_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
        // Completed status should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_status_unknown_type_ignored() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Task starts as Running
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "something-weird" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should remain Running — unknown type is ignored
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
    }

    // ── SessionIdle ─────────────────────────────────────────────────────

    #[test]
    fn session_idle_marks_task_complete_and_shows_notification() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.agent_status, AgentStatus::Complete);
        // Notification should be set
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("completed"));
        assert_eq!(notif.variant, NotificationVariant::Success);
        // SessionIdle should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_idle_triggers_auto_progression() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Task starts in "planning", config has auto_progress_to "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should have triggered auto-progression
        assert!(action.is_some());
        // Task should have moved to "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // Should be in the running column in kanban
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        // Should be removed from planning column
        assert!(!state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&task_id));
    }

    #[test]
    fn session_idle_no_auto_progress_when_not_configured() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should still be in "planning"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "planning");
    }

    #[test]
    fn session_idle_unknown_session_ignored() {
        let (mut state, task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: "nonexistent-session".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should still be Running (no change)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running
        );
        // No notification should be set
        assert!(state.ui.notifications.is_empty());
    }

    // ── SessionError ────────────────────────────────────────────────────

    #[test]
    fn session_error_records_error_on_task() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::SessionError {
            properties: opencode_sdk_rs::resources::event::SessionErrorProps {
                error: Some(
                    opencode_sdk_rs::resources::shared::SessionError::UnknownError {
                        data: opencode_sdk_rs::resources::shared::UnknownErrorData {
                            message: "something broke".to_string(),
                        },
                    },
                ),
                session_id: Some(session_id.clone()),
            },
        };
        process_event(&event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.agent_status, AgentStatus::Error);
        assert!(task.error_message.as_ref().unwrap().contains("something broke"));
    }

    // ── MessagePartDelta ────────────────────────────────────────────────

    #[test]
    fn message_part_delta_appends_streaming_text() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Hello ".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        let session = state.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello "));

        // Append more text
        let event2 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event2, &mut state, &client, &columns_config);

        let session = state.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));
    }

    // ── PermissionAsked ─────────────────────────────────────────────────

    #[test]
    fn permission_asked_creates_pending_request() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        let event = EventListResponse::PermissionAsked {
            properties: serde_json::json!({
                "id": "perm-001",
                "sessionID": session_id,
                "tool": "bash",
                "title": "Run build command"
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should have a pending permission on the task
        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.pending_permission_count, 1);

        let session = state.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_permissions.len(), 1);
        assert_eq!(session.pending_permissions[0].id, "perm-001");
        assert_eq!(session.pending_permissions[0].tool_name, "bash");
    }

    // ── PermissionReplied ───────────────────────────────────────────────

    #[test]
    fn permission_replied_resolves_request() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // First add a pending permission
        state.add_permission_request(
            &task_id,
            PermissionRequest {
                id: "perm-001".to_string(),
                session_id: session_id.clone(),
                tool_name: "bash".to_string(),
                description: "Run cmd".to_string(),
                status: "pending".to_string(),
                details: None,
            },
        );
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_permission_count, 1);

        let event = EventListResponse::PermissionReplied {
            properties: opencode_sdk_rs::resources::event::PermissionRepliedProps {
                session_id: session_id.clone(),
                request_id: "perm-001".to_string(),
                reply: opencode_sdk_rs::resources::event::PermissionReply::Once,
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Permission should be resolved (count back to 0)
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_permission_count, 0);
        let session = state.task_sessions.get(&task_id).unwrap();
        assert!(session.pending_permissions.is_empty());
    }

    // ── QuestionAsked ───────────────────────────────────────────────────

    #[test]
    fn question_asked_sets_warning_notification() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-001",
                "question": "Which approach should I use for the refactoring?"
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("Question pending"));
        assert!(notif.message.contains("Which approach"));
        assert_eq!(notif.variant, NotificationVariant::Warning);
        let session = state.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_questions.len(), 1);
        assert_eq!(session.pending_questions[0].id, "q-001");
        assert_eq!(session.pending_questions[0].question, "Which approach should I use for the refactoring?");
        assert_eq!(session.pending_questions[0].status, "pending");
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 1);
    }

    #[test]
    fn question_asked_stores_answer_options() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-002",
                "question": "Which approach should I use?",
                "answers": ["Option A", "Option B", "Option C"]
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let session = state.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.pending_questions.len(), 1);
        assert_eq!(session.pending_questions[0].answers, vec!["Option A", "Option B", "Option C"]);
    }

    #[test]
    fn question_asked_truncates_long_question() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let long_question = "a".repeat(100);
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-003",
                "question": long_question
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.len() < long_question.len() + 30);
    }

    #[test]
    fn question_replied_removes_from_pending() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();
        let event = EventListResponse::QuestionAsked {
            properties: serde_json::json!({
                "sessionID": session_id,
                "id": "q-004",
                "question": "Should I proceed?",
                "answers": ["Yes", "No"]
            }),
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 1);
        let reply_event = EventListResponse::QuestionReplied {
            properties: opencode_sdk_rs::resources::event::QuestionRepliedProps {
                session_id: session_id.clone(),
                request_id: "q-004".to_string(),
                answers: vec![],
            },
        };
        let (_action, _finalize) = process_event(&reply_event, &mut state, &client, &columns_config);
        let session = state.task_sessions.get(&task_id).unwrap();
        assert!(session.pending_questions.is_empty());
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 0);
    }

    // ── Ignored events ──────────────────────────────────────────────────

    #[test]
    fn ignored_events_do_not_panic() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Events we don't handle should not panic
        let event = EventListResponse::FileEdited {
            properties: opencode_sdk_rs::resources::event::FileEditedProps {
                file: "src/main.rs".to_string(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        let event = EventListResponse::ServerConnected {
            properties: opencode_sdk_rs::resources::event::EmptyProps {},
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // render_dirty should still be set
        assert!(state.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── render_dirty always set ─────────────────────────────────────────

    #[test]
    fn process_event_always_marks_render_dirty() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Clear the flag first
        state
            .render_dirty
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let event = EventListResponse::ServerConnected {
            properties: opencode_sdk_rs::resources::event::EmptyProps {},
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert!(state.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
    }
}
