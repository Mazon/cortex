//! SSE event loop — subscribe to events, match variants, update state directly.

use futures::StreamExt;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

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

/// Maximum concurrent auto-approve tasks. When the semaphore is full,
/// safe-tool permissions fall through to manual approval.
const MAX_CONCURRENT_AUTO_APPROVES: usize = 8;

/// Semaphore limiting concurrent auto-approve spawns. Uses `try_acquire`
/// to avoid blocking — if full, the permission falls through to manual approval.
static AUTO_APPROVE_SEMAPHORE: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_AUTO_APPROVES));

/// Run the SSE event loop for an OpenCode server shared by one or more projects.
/// This is spawned as a tokio task per unique server URL.
///
/// The `shutdown` receiver is watched so the loop can exit cleanly when the
/// app is shutting down, instead of relying solely on task cancellation via
/// `abort()`.
///
/// `project_ids` identifies all projects sharing this server URL. Connection
/// state changes (connected, reconnecting, permanently_disconnected) are
/// propagated to every project in the list so that the status bar stays
/// consistent across multi-project setups.
pub async fn sse_event_loop(
    client: OpenCodeClient,
    state: Arc<Mutex<AppState>>,
    columns_config: ColumnsConfig,
    opencode_config: OpenCodeConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    project_ids: Vec<String>,
) {
    let base_backoff_ms: u64 = 2000;
    let mut backoff_power: u32 = 0; // 2^backoff_power * base_backoff_ms + jitter
    let mut reconnect_attempt: u32 = 0;

    // Add per-project jitter to avoid thundering herd when a shared server goes
    // down.  A simple deterministic hash of the first project ID produces a
    // stable 0–500 ms offset so different server groups spread their reconnect
    // attempts evenly without adding a random dependency.
    let jitter: u64 = (project_ids
        .first()
        .map(|pid| pid.bytes())
        .unwrap_or_else(|| "".bytes())
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64)))
        % 501;

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
                tracing::debug!(
                    "SSE stream established (server: {})",
                    client.base_url()
                );
                // Snapshot reconnect count before resetting — used below for
                // recovery diagnostics.
                let was_reconnecting = reconnect_attempt > 0;
                backoff_power = 0; // Reset backoff on successful connection
                reconnect_attempt = 0; // Reset consecutive failure counter on success
                let mut stream = stream;
                let mut first_event_received = false;

                loop {
                    tokio::select! {
                        event_result = stream.next() => {
                            let Some(event_result) = event_result else {
                                // Stream closed by the server — break to reconnect.
                                // Don't set reconnecting here; the outer loop's
                                // grace period will handle it.
                                tracing::debug!(
                                    "SSE stream closed by server (clean close, will reconnect)"
                                );
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
                                                tracing::debug!("Skipping malformed SSE event: {}", msg);
                                            }
                                        }
                                        SseStreamError::Connection(msg) => {
                                            // Connection error — stream is dead, break to reconnect.
                                            // Don't set reconnecting here; the outer loop's
                                            // grace period will handle it.
                                            tracing::debug!(
                                                "SSE connection error (will reconnect): {}",
                                                msg
                                            );
                                            break;
                                        }
                                    }
                                    continue;
                                }
                            };

                            // Mark connected only after the first successful event —
                            // avoids a brief "connected" flash on short-lived streams
                            // that return 200 but close before delivering data.
                            if !first_event_received {
                                first_event_received = true;
                                if was_reconnecting {
                                    tracing::debug!(
                                        "SSE reconnected successfully after prior failures; \
                                         events missed during disconnection may cause stale state"
                                    );
                                }
                                {
                                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                    for pid in &project_ids {
                                        state.set_project_connected(pid, true);
                                    }
                                }
                            }

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
                                            tracing::debug!(
                                                task_id = %task_id,
                                                session_id = %session_id,
                                                "Skipping finalization — streaming_text already cleared (plan captured from streaming buffer)"
                                            );
                                            return;
                                        }

                                        match client_clone.fetch_session_messages(&session_id).await {
                                            Ok(messages) => {
                                                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                                                s.finalize_session_streaming(&task_id, messages);
                                            }
                                            Err(e) => {
                                                tracing::debug!("Failed to fetch session messages for finalization (streaming text preserved): {}", e);
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
                // Initial connection failure — don't set reconnecting here.
                // The outer loop's grace period will handle it.
                tracing::debug!(
                    "SSE connection failed (attempt {}): {}",
                    reconnect_attempt + 1,
                    e
                );
            }
        }

        // Check if we've exceeded the max retry limit.
        if reconnect_attempt >= max_retries {
            tracing::debug!(
                "SSE max retries reached ({}), entering slow-retry mode (projects: {:?})",
                max_retries,
                project_ids
            );
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                for pid in &project_ids {
                    state.set_project_permanently_disconnected(pid);
                }
                state.mark_render_dirty();
            }
            // Enter slow-retry mode instead of giving up permanently.
            // The permanently_disconnected state is still set (red indicator)
            // but the loop keeps trying at a very slow rate so the app
            // recovers automatically when the server comes back online.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    tracing::debug!(
                        "SSE slow-retry: resetting after permanent disconnect cooldown"
                    );
                    reconnect_attempt = 0;
                    backoff_power = 0;
                    continue;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }

        // Exponential backoff with max 30s, but also break on shutdown.
        reconnect_attempt += 1;

        // Compute fresh backoff: base * 2^power + jitter. Jitter is always
        // 0–500 ms regardless of retry count, avoiding accumulation from
        // exponential doubling.
        let backoff_ms =
            (base_backoff_ms * 2u64.pow(backoff_power)).min(30_000) + jitter;

        // Grace period: sleep before updating the reconnecting indicator.
        // This prevents a yellow "reconnecting" flash for transient connection
        // blips (e.g., a single read-timeout that reconnects quickly). The
        // user continues to see "connected" (green) during this window. If
        // reconnection succeeds on the next loop iteration, the reconnecting
        // flag is never set and the yellow indicator never appears.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }

        // Update reconnecting state after grace period has elapsed.
        // Propagate to all projects sharing this server URL.
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for pid in &project_ids {
                state.set_project_reconnecting(pid, true);
                state.set_project_reconnect_attempt(pid, reconnect_attempt);
            }
            state.mark_render_dirty();
        }
        tracing::debug!(
            "SSE reconnecting (attempt {}, backoff {}ms, projects: {:?})",
            reconnect_attempt,
            backoff_ms,
            project_ids
        );
        backoff_power += 1;
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
            // Also trigger auto-progression as a fallback in case SessionIdle
            // doesn't arrive or is delayed — the task should still move to the
            // next column.
            let (finalize, action) = if matches!(status, "complete" | "completed") {
                // Extract plan output early on SessionStatus "complete" to
                // capture the plan before streaming truncation might discard
                // early content. The SessionIdle handler will also extract
                // (and the finalize task may overwrite with richer content),
                // but this early capture protects against truncation.
                let task_id = state
                    .get_task_id_by_session(&properties.session_id)
                    .map(|s| s.to_string());
                if let Some(ref tid) = task_id {
                    state.extract_plan_output(tid);
                }
                // Trigger auto-progression as a fallback.  If SessionIdle
                // already ran and moved the task, the session mapping will
                // have been cleared (or the task will be Running), so this
                // is a safe no-op.
                let action = if let Some(ref tid) = task_id {
                    // Only progress if the task is still Complete (not already
                    // auto-progressed by a prior SessionIdle).
                    let is_complete = state
                        .tasks
                        .get(tid)
                        .map(|t| t.agent_status == AgentStatus::Complete)
                        .unwrap_or(false);
                    if is_complete {
                        // Check if the task has pending questions — if so,
                        // set Question status and block auto-progression.
                        let has_questions = state
                            .tasks
                            .get(tid)
                            .map(|t| t.pending_question_count > 0)
                            .unwrap_or(false);
                        if has_questions {
                            state.update_task_agent_status(tid, AgentStatus::Question);
                            None
                        } else if let Some(ref col) = state.tasks.get(tid).map(|t| t.column.clone()) {
                            let has_auto_progress =
                                columns_config.auto_progress_for(&col.0).is_some();
                            let has_plan = state
                                .tasks
                                .get(tid)
                                .and_then(|t| t.plan_output.as_ref())
                                .map(|p| !p.trim().is_empty())
                                .unwrap_or(false);
                            if has_auto_progress || has_plan {
                                state.update_task_agent_status(tid, AgentStatus::Ready);
                            }
                            on_agent_completed(tid, state, columns_config)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                (Some(properties.session_id.clone()), action)
            } else {
                (None, None)
            };
            (action, finalize)
        }

        EventListResponse::SessionIdle { properties } => {
            let action = if let Some(task_id) = state.process_session_idle(&properties.session_id) {
                // Extract plan output NOW from streaming_text, so the has_plan
                // check below can see it. finalize_session_streaming will later
                // overwrite with the full message-based version.
                state.extract_plan_output(&task_id);

                // Check if the task has pending questions — if so,
                // set Question status and block auto-progression.
                let has_questions = state
                    .tasks
                    .get(&task_id)
                    .map(|t| t.pending_question_count > 0)
                    .unwrap_or(false);

                if has_questions {
                    state.update_task_agent_status(&task_id, AgentStatus::Question);
                    None
                } else if let Some(ref col) = state.tasks.get(&task_id).map(|t| t.column.clone()) {
                    // process_session_idle sets Complete by default — override to Ready
                    // when the column has auto_progress_to configured, meaning the agent
                    // completed but the task has a next step, OR when the task has a
                    // non-empty plan_output (meaning the agent produced a plan but hasn't
                    // acted on it yet). Terminal columns without a plan stay Complete ("done").
                    let has_auto_progress = columns_config.auto_progress_for(&col.0).is_some();
                    let has_plan = state
                        .tasks
                        .get(&task_id)
                        .and_then(|t| t.plan_output.as_ref())
                        .map(|p| !p.trim().is_empty())
                        .unwrap_or(false);
                    if has_auto_progress || has_plan {
                        state.update_task_agent_status(&task_id, AgentStatus::Ready);
                    }
                    // Trigger auto-progression if configured for this column
                    on_agent_completed(&task_id, state, columns_config)
                } else {
                    None
                }
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
                    match AUTO_APPROVE_SEMAPHORE.try_acquire() {
                        Ok(permit) => {
                            tokio::spawn(async move {
                                let _permit = permit; // hold permit for duration of task
                                if let Err(_e) = client_clone.resolve_permission(&sid, &pid, true).await {
                                }
                            });
                        }
                        Err(_) => {
                            // Semaphore full — fall through to manual approval queue
                            state.process_permission_asked(&session_id, &perm_id, &tool_name, &desc);
                        }
                    }

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
            planning_context: None,
            pending_description: None,
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
                    auto_progress_to: Some("review".to_string()),
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: Some("Review".to_string()),
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
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
        // Use a config without auto-progression so we can test the
        // status update in isolation (auto-progression fallback would
        // otherwise move the task and set Running).
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

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
        // Disable auto-progression so we test pure status update.
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;

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

        // Config without auto-progression for "planning" or "running"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;
        columns_config.definitions[2].auto_progress_to = None;

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
    fn terminal_column_with_plan_output_gets_ready() {
        // A task in a terminal column (no auto_progress_to) should get Ready
        // ("ready") when it has a non-empty plan_output — the plan signals
        // there's more work to do.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();

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

        // Should get Ready — plan_output triggers Ready even in terminal columns.
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

        // Config without auto-progression for "planning" or "running"
        let mut columns_config = make_columns_config();
        columns_config.definitions[1].auto_progress_to = None;
        columns_config.definitions[2].auto_progress_to = None;

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

        // 3. Session completes — auto-progression fallback triggers,
        // moving task from "planning" → "running" and returning an action.
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (action, finalize) = process_event(&complete_event, &mut state, &client, &columns_config);

        // Task should be in "running" column now (auto-progressed from "planning")
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // Should have an auto-progress action
        assert!(action.is_some());
        // Should signal finalization
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));

        // Simulate the event loop post-processing that the real code does:
        // clear the old session mapping and set Running status.
        {
            let old_sid = state.tasks.get(&task_id).and_then(|t| t.session_id.clone());
            if let Some(old_sid) = old_sid {
                state.session_tracker.session_to_task.remove(&old_sid);
            }
            state.update_task_agent_status(&task_id, AgentStatus::Running);
        }

        // 4. SessionIdle arrives later — session mapping was cleared,
        // so process_session_idle won't find the task → no action.
        let idle_event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (idle_action, idle_finalize) = process_event(&idle_event, &mut state, &client, &columns_config);

        // No action should be triggered (mapping was cleared)
        assert!(idle_action.is_none());
        // Finalization may still be signaled
        assert!(idle_finalize.is_some());
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

    // ── Task 4.1: planning→do auto-progression preserves context ──────────

    #[test]
    fn planning_to_do_preserves_plan_output() {
        // Simulate the full planning→do auto-progression flow:
        // 1. Planning agent streams text
        // 2. Session completes → extract_plan_output called
        // 3. Session idle → auto-progression to "running" column
        // 4. New session created → session data cleared but plan_output preserved
        // 5. build_prompt_for_agent includes the plan
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // 1. Planning agent streams a plan
        let plan_text = "Step 1: Analyze codebase\nStep 2: Refactor module X\nStep 3: Add tests";
        for delta in plan_text.split_inclusive('\n') {
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

        // 2. Session completes → extract_plan_output called (via SessionStatus "completed")
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (complete_action, _finalize) = process_event(&complete_event, &mut state, &client, &columns_config);

        // Simulate the event loop post-processing: clear old session mapping
        // and set Running status (what the real event loop does at lines 139-151).
        if complete_action.is_some() {
            let old_sid = state.tasks.get(&task_id).and_then(|t| t.session_id.clone());
            if let Some(old_sid) = old_sid {
                state.session_tracker.session_to_task.remove(&old_sid);
            }
            state.update_task_agent_status(&task_id, AgentStatus::Running);
        }

        // Verify plan_output was extracted on SessionStatus "completed"
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should be extracted on SessionStatus completed"
        );

        // Verify auto-progression happened on SessionStatus "completed"
        assert!(complete_action.is_some(), "auto-progression should trigger on SessionStatus completed");
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");

        // 3. Session idle arrives later — mapping was cleared, so no-op
        let idle_event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (idle_action, _idle_finalize) = process_event(&idle_event, &mut state, &client, &columns_config);

        // No action — mapping was cleared by post-processing above
        assert!(idle_action.is_none(), "SessionIdle should be no-op after auto-progression");

        // 4. Simulate start_agent creating a new session (clearing session data)
        state.set_task_session_id(&task_id, Some("session-do-agent".to_string()));
        state.clear_session_data(&task_id);

        // 5. Verify plan_output is STILL preserved after session data clearing
        let task = state.tasks.get(&task_id).unwrap();
        assert!(
            task.plan_output.is_some(),
            "plan_output must survive session data clearing"
        );
        let plan = task.plan_output.as_ref().unwrap();
        assert!(plan.contains("Step 1: Analyze codebase"));
        assert!(plan.contains("Step 2: Refactor module X"));

        // 6. Verify build_prompt_for_agent includes the plan
        let prompt = OpenCodeClient::build_prompt_for_agent(task, "do", None);
        assert!(prompt.contains("## Plan (from planning phase)"));
        assert!(prompt.contains("Step 1: Analyze codebase"));
    }

    // ── Task 4.2: manual planning→do move preserves context ───────────────

    #[test]
    fn manual_move_preserves_plan_output() {
        // Simulate a manual move from planning to running column:
        // plan_output should be preserved through the transition.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // Planning agent streams a plan
        let plan_text = "My plan: refactor the parser";
        let delta_event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: plan_text.to_string(),
            },
        };
        process_event(&delta_event, &mut state, &client, &columns_config);

        // Extract plan
        state.extract_plan_output(&task_id);
        assert!(state.tasks.get(&task_id).unwrap().plan_output.is_some());

        // Manually move task to running column
        state.move_task(&task_id, KanbanColumn("running".to_string()));

        // Plan should still be there
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should survive manual column move"
        );
    }

    // ── Task 4.4: extract_plan_output edge cases ──────────────────────────

    #[test]
    fn extract_plan_output_preserves_existing_when_empty() {
        // If a task already has plan_output and extraction yields empty,
        // the existing value should be preserved.
        let (mut state, task_id, _session_id) = make_test_state();

        // Pre-set a plan_output
        state.tasks.get_mut(&task_id).unwrap().plan_output = Some(
            "Existing plan from previous extraction".to_string()
        );

        // Create session with empty streaming_text
        let session = state.session_tracker.task_sessions.entry(task_id.clone()).or_default();
        session.streaming_text = Some("   ".to_string()); // whitespace only

        state.extract_plan_output(&task_id);

        // Should preserve the existing plan_output
        assert_eq!(
            state.tasks.get(&task_id).unwrap().plan_output.as_deref(),
            Some("Existing plan from previous extraction"),
            "existing plan_output should not be overwritten by empty extraction"
        );
    }

    #[test]
    fn extract_plan_output_creates_lazy_session_entry() {
        // If no session data exists but the task does, a lazy entry should be created.
        let (mut state, task_id, _session_id) = make_test_state();

        // No session data exists yet
        assert!(!state.session_tracker.task_sessions.contains_key(&task_id));

        // Extract should not panic and should create a lazy entry
        state.extract_plan_output(&task_id);

        // A lazy session entry should now exist
        assert!(
            state.session_tracker.task_sessions.contains_key(&task_id),
            "lazy session entry should be created for existing task"
        );
        // plan_output should still be None (no data to extract)
        assert!(state.tasks.get(&task_id).unwrap().plan_output.is_none());
    }

    #[test]
    fn extract_plan_output_from_messages_over_streaming() {
        // Messages should take priority over streaming_text.
        let (mut state, task_id, _session_id) = make_test_state();

        let session = state.session_tracker.task_sessions.entry(task_id.clone()).or_default();
        session.streaming_text = Some("Streaming fallback text".to_string());
        session.messages = vec![
            TaskMessage {
                id: "msg-1".to_string(),
                role: MessageRole::Assistant,
                parts: vec![TaskMessagePart::Text { text: "Rich plan from messages".to_string() }],
                created_at: None,
            },
        ];

        state.extract_plan_output(&task_id);

        let plan = state.tasks.get(&task_id).unwrap().plan_output.as_ref().unwrap();
        assert_eq!(plan, "Rich plan from messages");
        assert!(!plan.contains("Streaming fallback"));
    }

    #[test]
    fn session_status_complete_extracts_plan_early() {
        // SessionStatus "complete" should trigger plan extraction
        // even before SessionIdle arrives.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1").unwrap();
        let columns_config = make_columns_config();

        // Stream some plan text
        let delta_event = EventListResponse::MessagePartDelta {
            properties: opencode_sdk_rs::resources::event::MessagePartDeltaProps {
                session_id: session_id.clone(),
                message_id: "msg-1".to_string(),
                part_id: "part-1".to_string(),
                field: "text".to_string(),
                delta: "Early plan extraction test".to_string(),
            },
        };
        process_event(&delta_event, &mut state, &client, &columns_config);

        // Send SessionStatus "completed" (NOT SessionIdle)
        let complete_event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        process_event(&complete_event, &mut state, &client, &columns_config);

        // Plan should be extracted from streaming_text already
        assert!(
            state.tasks.get(&task_id).unwrap().plan_output.is_some(),
            "plan_output should be extracted on SessionStatus completed (before SessionIdle)"
        );
        assert_eq!(
            state.tasks.get(&task_id).unwrap().plan_output.as_deref(),
            Some("Early plan extraction test")
        );
    }

    // ── Question Status (pending questions block auto-progression) ──────

    #[test]
    fn session_idle_with_pending_questions_sets_question_status() {
        // When a task has pending questions and the agent goes idle,
        // the task should enter Question status instead of Ready/Complete,
        // and auto-progression should be blocked.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate a pending question on the task
        state.tasks.get_mut(&task_id).unwrap().pending_question_count = 1;

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should be in Question status, not Ready
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Question,
            "task with pending questions should enter Question status on SessionIdle"
        );
        // No auto-progression action should be returned
        assert!(
            action.is_none(),
            "auto-progression should be blocked when questions are pending"
        );
        // Finalization should still happen
        assert_eq!(finalize.as_deref(), Some(session_id.as_str()));
    }

    #[test]
    fn session_status_complete_with_pending_questions_sets_question_status() {
        // SessionStatus "complete" should also set Question status when
        // pending questions exist, blocking auto-progression.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // Simulate a pending question on the task
        state.tasks.get_mut(&task_id).unwrap().pending_question_count = 2;

        let event = EventListResponse::SessionStatus {
            properties: opencode_sdk_rs::resources::event::SessionStatusProps {
                session_id: session_id.clone(),
                status: serde_json::json!({ "type": "completed" }),
            },
        };
        let (action, finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Task should be in Question status
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Question,
            "task with pending questions should enter Question status on SessionStatus complete"
        );
        // No auto-progression
        assert!(action.is_none());
        // Finalization should still happen
        assert!(finalize.is_some());
    }

    #[test]
    fn session_idle_without_pending_questions_proceeds_normally() {
        // When no questions are pending, the existing Ready/Complete +
        // auto-progression behavior should be preserved.
        let (mut state, task_id, session_id) = make_test_state();
        let client = OpenCodeClient::new("http://127.0.0.1:1").unwrap();
        let columns_config = make_columns_config();

        // No pending questions — default behavior
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 0);

        let event = EventListResponse::SessionIdle {
            properties: opencode_sdk_rs::resources::event::SessionIdleProps {
                session_id: session_id.clone(),
            },
        };
        let (_action, _finalize) = process_event(&event, &mut state, &client, &columns_config);

        // Should be Ready (planning column has auto_progress_to configured)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Ready,
            "task without pending questions should proceed to Ready as before"
        );
    }
}
