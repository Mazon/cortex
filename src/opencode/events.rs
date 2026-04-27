//! SSE event loop — subscribe to events, match variants, update state directly.

use futures::StreamExt;
use std::sync::{Arc, Mutex};

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::opencode::client::{
    convert_session_error, extract_permission_fields, is_safe_tool,
    OpenCodeClient,
};
use crate::opencode::sse::SseStreamError;
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
///
/// `project_id` identifies which project this loop serves. Connection state
/// is tracked per-project on `CortexProject` rather than globally on `AppState`.
pub async fn sse_event_loop(
    client: OpenCodeClient,
    state: Arc<Mutex<AppState>>,
    columns_config: ColumnsConfig,
    opencode_config: OpenCodeConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    project_id: String,
) {
    let mut backoff_ms: u64 = 2000;
    let mut reconnect_attempt: u32 = 0;

    // Add per-project jitter to avoid thundering herd when a shared server goes
    // down.  A simple deterministic hash of the project ID produces a stable
    // 0–500 ms offset so different projects spread their reconnect attempts
    // evenly without adding a random dependency.
    let jitter: u64 = (project_id
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64)))
        % 501;
    backoff_ms += jitter;

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
            return;
        }

        match client.subscribe_to_events().await {
            Ok(stream) => {
                backoff_ms = 2000 + jitter; // Reset backoff on successful connection
                reconnect_attempt = 0; // Reset consecutive failure counter on success
                let mut stream = stream;

                // Mark reconnection complete — we have a live stream.
                {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_project_connected(&project_id, true);
                }

                loop {
                    tokio::select! {
                        event_result = stream.next() => {
                            let Some(event_result) = event_result else {
                                // Stream closed by the server.
                                {
                                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                    state.set_project_reconnecting(&project_id, true);
                                }
                                break;
                            };

                            let event = match event_result {
                                Ok(e) => e,
                                Err(e) => {
                                    match &e {
                                        SseStreamError::Json(json_err) => {
                                            let msg = json_err.to_string();
                                            if msg.contains("unknown variant") || msg.contains("missing field") {
                                                // Unknown event type or structurally expected field missing from
                                                // the server payload (e.g. FileDiff.before for new files).
                                                // The stream is still healthy — skip silently.
                                            } else {
                                                tracing::warn!("Skipping malformed SSE event: {}", msg);
                                            }
                                        }
                                        SseStreamError::Connection(msg) => {
                                            // Connection error — stream is dead, break to reconnect.
                                            tracing::debug!("SSE stream error (reconnecting): {}", msg);
                                            {
                                                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                                state.set_project_reconnecting(&project_id, true);
                                            }
                                            break;
                                        }
                                    }
                                    continue;
                                }
                            };

                            let (action, finalize_session_id, finalize_task_id) = {
                                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                let (action, finalize_session_id) =
                                    process_event(&event, &mut state, &client, &columns_config);
                                // Close the race window: set Running status + agent_type while
                                // still holding the lock, same as the manual move path in app.rs.
                                // Also capture previous agent for cross-contamination detection.
                                let (action, finalize_task_id) = match action {
                                    Some(a) => {
                                        let previous_agent = state.tasks.get(&a.task_id)
                                            .and_then(|t| t.agent_type.clone());
                                        // Clear old session mapping immediately so stale SessionStatus
                                        // events for the old session can't find this task and overwrite
                                        // the Running status we're about to set.
                                        let old_session_id = state.tasks.get(&a.task_id)
                                            .and_then(|t| t.session_id.clone());
                                        if let Some(old_sid) = old_session_id {
                                            state.session_tracker.session_to_task.remove(&old_sid);
                                            // Keep the session_id on the task for now so start_agent()
                                            // can detect the agent change and create a fresh session.
                                            // The mapping is what matters for event routing.
                                        }
                                        state.update_task_agent_status(&a.task_id, AgentStatus::Running);
                                        state.set_task_agent_type(&a.task_id, Some(a.agent.clone()));
                                        // Return the task_id for finalization since we just broke the
                                        // session→task lookup that the finalize logic depends on.
                                        let finalize_task_id = Some(a.task_id.clone());
                                        (Some((a, previous_agent)), finalize_task_id)
                                    }
                                    None => (None, None),
                                };
                                (action, finalize_session_id, finalize_task_id)
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
                                // Use finalize_task_id if available (auto-progression may
                                // have cleared the session→task mapping to prevent stale events).
                                let task_id = finalize_task_id.or_else(|| {
                                    let s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.get_task_id_by_session(&session_id)
                                        .map(|tid| tid.to_string())
                                });
                                if let Some(task_id) = task_id {
                                    let client_clone = client.clone();
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        // Check if there's streaming text to finalize
                                        let needs_finalize = {
                                            let s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                                            s.session_tracker.task_sessions.get(&task_id)
                                                .is_some_and(|ts| ts.streaming_text.is_some())
                                        };
                                        if !needs_finalize {
                                            return;
                                        }

                                        match client_clone.fetch_session_messages(&session_id).await {
                                            Ok(messages) => {
                                                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                                                s.finalize_session_streaming(&task_id, messages);
                                            }
                                            Err(e) => {
                                                tracing::warn!("Failed to fetch session messages for finalization (streaming text preserved): {}", e);
                                                // Don't clear streaming_text — it's the only copy of the agent's output.
                                                // It will be cleaned up when a new session starts.
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        result = shutdown.changed() => {
                            if result.is_err() || *shutdown.borrow() {
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let _ = e;
                {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_project_reconnecting(&project_id, true);
                }
            }
        }

        // Check if we've exceeded the max retry limit.
        if reconnect_attempt >= max_retries {
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_project_permanently_disconnected(&project_id);
                state.mark_render_dirty();
            }
            return;
        }

        // Exponential backoff with max 30s, but also break on shutdown.
        reconnect_attempt += 1;
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_project_reconnecting(&project_id, true);
            state.set_project_reconnect_attempt(&project_id, reconnect_attempt);
            state.mark_render_dirty();
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
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
                // Extract plan output NOW from streaming_text, so the has_plan
                // check below can see it. finalize_session_streaming will later
                // overwrite with the full message-based version.
                state.extract_plan_output(&task_id);

                // process_session_idle sets Complete by default — override to Ready
                // when the task has a plan ready for the next step, or when the
                // column has auto_progress_to configured.
                if let Some(ref col) = state.tasks.get(&task_id).map(|t| t.column.clone()) {
                    let has_auto_progress = columns_config.auto_progress_for(&col.0).is_some();
                    let has_plan = state.tasks.get(&task_id)
                        .and_then(|t| t.plan_output.as_ref())
                        .is_some_and(|p| !p.is_empty());
                    if has_auto_progress || has_plan {
                        state.update_task_agent_status(&task_id, AgentStatus::Ready);
                    }
                }
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
                    tokio::spawn(async move {
                        if let Err(_e) = client_clone.resolve_permission(&sid, &pid, true).await {
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
                let project_id = state.tasks.get(&task_id)
                    .map(|t| t.project_id.clone())
                    .unwrap_or_default();
                if !project_id.is_empty() {
                    state.update_project_status(&project_id);
                }

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
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("proj-1".to_string());

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
            .session_tracker.session_to_task
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
        assert!(state.dirty_flags.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
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
    fn session_idle_sets_ready_for_intermediate_column_and_shows_notification() {
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
        // Task was in "planning" which has auto_progress_to → Ready, not Complete
        assert_eq!(task.agent_status, AgentStatus::Ready);
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

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
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

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
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

        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
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
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
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
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
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
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
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
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
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
        assert!(state.dirty_flags.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── render_dirty always set ─────────────────────────────────────────

    #[test]
    fn process_event_always_marks_render_dirty() {
        let (mut state, _task_id, _session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Clear the flag first
        state
            .dirty_flags.render_dirty
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let event = EventListResponse::ServerConnected {
            properties: opencode_sdk_rs::resources::event::EmptyProps {},
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        assert!(state.dirty_flags.render_dirty.load(std::sync::atomic::Ordering::Relaxed));
    }

    // ── Race condition fix: stale SessionStatus can't overwrite Running ───

    #[test]
    fn stale_session_status_does_not_overwrite_after_mapping_cleared() {
        // Simulate the core invariant of the race condition fix:
        // After the session→task mapping is cleared (by auto-progression),
        // a stale SessionStatus "complete" for the OLD session cannot find
        // the task and therefore cannot overwrite its status.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate auto-progression clearing the session mapping
        state.session_tracker.session_to_task.remove(&session_id);

        // Set the task to Running (as auto-progression would)
        state.update_task_agent_status(&task_id, AgentStatus::Running);

        // Now a stale SessionStatus "complete" arrives for the old session
        let stale_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, _finalize) = process_event(&stale_event, &mut state, &client, &columns_config);

        // Task should still be Running — the stale event couldn't find it
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Running,
            "Stale SessionStatus should not overwrite Running when mapping is cleared"
        );
    }

    #[test]
    fn session_mapping_present_allows_status_update() {
        // Control test: when the session→task mapping IS present, SessionStatus
        // updates the task as expected.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Mapping is present (from make_test_state)
        assert!(state.get_task_id_by_session(&session_id).is_some());

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task status should be updated to Complete
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete,
        );
    }

    // ── Ready vs Complete status ─────────────────────────────────────────

    #[test]
    fn terminal_column_gets_complete_not_ready() {
        // When a task is in a terminal column (no auto_progress_to), SessionIdle
        // should set Complete, not Ready.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Move task to "running" (terminal column — no auto_progress_to)
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Terminal column should get Complete, not Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    // ── Ready status from plan_output ────────────────────────────────────

    #[test]
    fn session_idle_sets_ready_when_plan_output_exists() {
        // A task in a terminal column (no auto_progress_to) should still get
        // Ready status when it has a non-empty plan_output.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Pre-set plan_output on the task (simulating what extract_plan_output does)
        state.tasks.get_mut(&task_id).unwrap().plan_output = Some(
            "Here is the plan:\n1. Do X\n2. Do Y".to_string()
        );

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should get Ready because plan_output is non-empty, even though
        // the column has no auto_progress_to.
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Ready
        );
    }

    #[test]
    fn terminal_column_without_plan_output_gets_complete() {
        // A task in a terminal column (no auto_progress_to) WITHOUT plan_output
        // should get Complete (not Ready).
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Move task to "running" (terminal column — no auto_progress_to)
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        // No plan_output set — task.plan_output is None

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Terminal column without plan_output → Complete
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    #[test]
    fn empty_plan_output_does_not_trigger_ready() {
        // An empty string plan_output should NOT trigger Ready status.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Config without auto-progression for "planning"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

        // Set an empty plan_output
        state.tasks.get_mut(&task_id).unwrap().plan_output = Some(String::new());

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Empty plan_output should not trigger Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    #[test]
    fn auto_progress_column_with_plan_output_still_gets_ready() {
        // A task in a column WITH auto_progress_to AND plan_output should
        // still get Ready status (both conditions independently trigger Ready).
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();

        // Use default config: "planning" has auto_progress_to → "running"
        let columns_config = make_columns_config();

        // Pre-set plan_output on the task
        state.tasks.get_mut(&task_id).unwrap().plan_output = Some(
            "Step 1: Refactor module\nStep 2: Add tests".to_string()
        );

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should get Ready — both auto_progress_to and plan_output are set.
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Ready
        );
    }

    // ── Plan output extraction ───────────────────────────────────────────

    #[test]
    fn extract_plan_output_from_messages() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Add messages to the session
        state.session_tracker.task_sessions.entry(task_id.clone()).or_default().messages = vec![
            TaskMessage {
                id: "msg-1".to_string(),
                role: MessageRole::User,
                parts: vec![TaskMessagePart::Text { text: "Plan this".to_string() }],
                created_at: None,
            },
            TaskMessage {
                id: "msg-2".to_string(),
                role: MessageRole::Assistant,
                parts: vec![TaskMessagePart::Text { text: "Here is the plan:\n1. Do X\n2. Do Y".to_string() }],
                created_at: None,
            },
        ];

        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert!(task.plan_output.is_some());
        let plan = task.plan_output.as_ref().unwrap();
        assert!(plan.contains("Here is the plan"));
        assert!(plan.contains("Do X"));
        assert!(plan.contains("Do Y"));
    }

    #[test]
    fn extract_plan_output_from_streaming_text() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Add streaming text (no finalized messages)
        let session = state.session_tracker.task_sessions.entry(task_id.clone()).or_default();
        session.streaming_text = Some("Streaming plan output...".to_string());

        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.plan_output.as_deref(), Some("Streaming plan output..."));
    }

    #[test]
    fn extract_plan_output_noop_when_no_session() {
        let (mut state, task_id, _session_id) = make_test_state();

        // No session data at all
        state.extract_plan_output(&task_id);

        let task = state.tasks.get(&task_id).unwrap();
        assert!(task.plan_output.is_none());
    }

    #[test]
    fn extract_plan_output_marks_task_dirty() {
        let (mut state, task_id, _session_id) = make_test_state();

        // Clear dirty flag
        state.dirty_flags.dirty_tasks.clear();

        state.session_tracker.task_sessions.entry(task_id.clone()).or_default().messages = vec![
            TaskMessage {
                id: "msg-1".to_string(),
                role: MessageRole::Assistant,
                parts: vec![TaskMessagePart::Text { text: "Plan".to_string() }],
                created_at: None,
            },
        ];

        state.extract_plan_output(&task_id);

        assert!(state.dirty_flags.dirty_tasks.contains(&task_id));
    }

    // ── Integration-style tests: full event lifecycle ──────────────────

    /// Test the full lifecycle of an agent session through SSE events:
    /// status:running → delta → delta → status:completed → session:idle.
    ///
    /// Verifies that:
    /// - Streaming text accumulates correctly from deltas
    /// - Status transitions are correct
    /// - Finalization session ID is signaled
    /// - Auto-progression moves the task to the next column
    #[test]
    fn integration_full_agent_lifecycle() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Session starts running
        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        let (_action, finalize) = process_event(&event, &mut state, &client, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().agent_status, AgentStatus::Running);
        assert!(finalize.is_none());

        // 2. Receive streaming deltas
        for delta in &["Hello ", "world", "! This is a test."] {
            let delta_event = EventListResponse::MessagePartDelta {
                properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                    session_id: session_id.clone(),
                    message_id: "msg-1".to_string(),
                    part_id: "part-1".to_string(),
                    field: "text".to_string(),
                    delta: delta.to_string(),
                },
            };
            process_event(&delta_event, &mut state, &client, &columns_config);
        }

        // Verify streaming text accumulated
        let session = state.session_tracker.task_sessions.get(&task_id).unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello world! This is a test."));

        // 3. Session completes
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (_action, finalize) = process_event(&complete_event, &mut state, &client, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().agent_status, AgentStatus::Complete);
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));

        // 4. Session goes idle → triggers auto-progression
        let idle_event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (action, finalize) = process_event(&idle_event, &mut state, &client, &columns_config);

        // Task should be in "running" column now (auto-progressed from "planning")
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // Should have an auto-progress action
        assert!(action.is_some());
        // Should signal finalization
        assert!(finalize.is_some());
    }

    /// Test SSE deduplication: receiving the exact same delta twice (same key,
    /// same content) should not double the streaming text. This simulates
    /// what happens when concurrent SSE connections deliver the same event.
    ///
    /// Also tests that replaying an old part (different key that was already
    /// seen) is correctly skipped.
    #[test]
    fn integration_dedup_prevents_text_doubling() {
        let (mut state, _task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Send first delta for part-1
        let delta1 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Hello ".to_string(),
            },
        };
        process_event(&delta1, &mut state, &client, &columns_config);

        // 2. Send continuation delta for part-1 (same key, different content)
        let delta2 = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(),
            },
        };
        process_event(&delta2, &mut state, &client, &columns_config);

        // Verify accumulated text
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));

        // 3. Simulate concurrent SSE loop delivering the exact same last delta
        // (same key, same content) — defense-in-depth dedup should catch this
        let delta_dup = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "World".to_string(),  // Same content as last delta
            },
        };
        process_event(&delta_dup, &mut state, &client, &columns_config);

        // Text should NOT have doubled
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World"));

        // 4. Now send a genuinely NEW part — should be accepted
        let delta_new = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-2".to_string(),
                field: "text".to_string(),
                delta: " More text".to_string(),
            },
        };
        process_event(&delta_new, &mut state, &client, &columns_config);

        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World More text"));

        // 5. Replay an old part that was already seen (different from current)
        // This simulates server replaying events after reconnection.
        // Since the key ("msg-1", "part-1") is in seen_delta_keys and
        // is NOT the current continuation, it's correctly identified as
        // a replay and skipped.
        let delta_old_replay = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Old replay".to_string(),
            },
        };
        process_event(&delta_old_replay, &mut state, &client, &columns_config);

        // Old replay should be skipped — text unchanged
        let session = state.session_tracker.task_sessions.get("task-1").unwrap();
        assert_eq!(session.streaming_text.as_deref(), Some("Hello World More text"));
    }

    /// Test error recovery: a session error should mark the task as Error
    /// and allow a new session to be started afterward.
    #[test]
    fn integration_error_then_restart() {
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // 1. Agent starts running
        let running_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "running" }),
            },
        };
        process_event(&running_event, &mut state, &client, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().agent_status, AgentStatus::Running);

        // 2. Agent errors
        let error_event = EventListResponse::SessionError {
            properties: opencode_sdk_rs::resources::event::SessionErrorProps {
                error: Some(
                    opencode_sdk_rs::resources::shared::SessionError::UnknownError {
                        data: opencode_sdk_rs::resources::shared::UnknownErrorData {
                            message: "API rate limit exceeded".to_string(),
                        },
                    },
                ),
                session_id: Some(session_id.clone()),
            },
        };
        process_event(&error_event, &mut state, &client, &columns_config);

        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.agent_status, AgentStatus::Error);
        assert!(task.error_message.as_ref().unwrap().contains("API rate limit exceeded"));

        // 3. A new session can be started (simulate by setting Running again)
        state.update_task_agent_status(&task_id, AgentStatus::Running);
        assert_eq!(state.tasks.get(&task_id).unwrap().agent_status, AgentStatus::Running);
    }
}
