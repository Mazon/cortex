//! SSE event dispatching — process individual events and update state.

use crate::config::types::ColumnsConfig;
use crate::opencode::client::{
    convert_session_error, extract_permission_fields, is_safe_tool, OpenCodeClient,
};
use crate::orchestration::engine::{on_agent_completed, AgentCompletionAction};
use crate::state::types::AgentStatus;

/// Maximum concurrent auto-approve tasks. When the semaphore is full,
/// safe-tool permissions fall through to manual approval.
const MAX_CONCURRENT_AUTO_APPROVES: usize = 8;

/// Semaphore limiting concurrent auto-approve spawns. Uses `try_acquire`
/// to avoid blocking — if full, the permission falls through to manual approval.
static AUTO_APPROVE_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_AUTO_APPROVES));

/// After an agent completes, determine whether the task should be
/// "Ready" (plan created / no changes) or "Complete" (no plan / changes made).
/// Returns the appropriate status and whether auto-progression should proceed.
pub(crate) fn determine_completion_status(
    state: &mut crate::state::types::AppState,
    task_id: &str,
) -> (AgentStatus, bool) {
    let (column, has_plan, had_writes) = state
        .tasks
        .get(task_id)
        .map(|t| {
            (
                t.column.0.as_str(),
                t.plan_output
                    .as_ref()
                    .map_or(false, |p| !p.trim().is_empty()),
                t.had_write_operations,
            )
        })
        .unwrap_or(("", false, false));

    match column {
        "planning" if has_plan => (AgentStatus::Ready, true),
        "planning" => (AgentStatus::Complete, false),
        "running" if had_writes => (AgentStatus::Complete, true),
        "running" => (AgentStatus::Ready, false),
        _ => (AgentStatus::Complete, true),
    }
}

/// Process a single SSE event, updating state directly.
/// Returns a tuple of:
/// - `Option<AgentCompletionAction>` if the event triggered auto-progression
///   or a queued follow-up prompt. The caller is responsible for executing
///   the action after releasing the MutexGuard.
/// - `Option<String>` — a session ID whose streaming output should be
///   finalized into persistent message history.  The caller spawns a
///   background task to fetch the complete messages and update state.
pub(crate) fn process_event(
    event: &opencode_sdk_rs::resources::event::EventListResponse,
    state: &mut crate::state::types::AppState,
    client: &OpenCodeClient,
    columns_config: &ColumnsConfig,
) -> (Option<AgentCompletionAction>, Option<String>) {
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
                        } else {
                            // Determine Ready vs Complete + whether to auto-progress
                            let (status, should_progress) =
                                determine_completion_status(state, tid);
                            state.update_task_agent_status(tid, status);

                            if should_progress {
                                if let Some(ref _col) =
                                    state.tasks.get(tid).map(|t| t.column.clone())
                                {
                                    on_agent_completed(tid, state, columns_config)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
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

                // Check for pending questions first
                let has_questions = state
                    .tasks
                    .get(&task_id)
                    .map(|t| t.pending_question_count > 0)
                    .unwrap_or(false);
                if has_questions {
                    state.update_task_agent_status(&task_id, AgentStatus::Question);
                    None
                } else {
                    // Determine Ready vs Complete + whether to auto-progress
                    let (status, should_progress) =
                        determine_completion_status(state, &task_id);
                    state.update_task_agent_status(&task_id, status);

                    if should_progress {
                        if let Some(ref _col) =
                            state.tasks.get(&task_id).map(|t| t.column.clone())
                        {
                            on_agent_completed(&task_id, state, columns_config)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
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
                                if let Err(_e) =
                                    client_clone.resolve_permission(&sid, &pid, true).await
                                {
                                }
                            });
                        }
                        Err(_) => {
                            // Semaphore full — fall through to manual approval queue
                            state.process_permission_asked(
                                &session_id,
                                &perm_id,
                                &tool_name,
                                &desc,
                            );
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
            if let Some(task_id) = state
                .get_task_id_by_session(&properties.session_id)
                .map(|s| s.to_string())
            {
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
            let session_id = properties
                .get("sessionID")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Route to parent task if this is a subagent session
            let task_id = if let Some(parent) = state.get_parent_task_for_subagent(session_id) {
                Some(parent.to_string())
            } else {
                state
                    .get_task_id_by_session(session_id)
                    .map(|s| s.to_string())
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
                let project_id = state
                    .tasks
                    .get(&task_id)
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
            if let Some(task_id) = state
                .get_task_id_by_session(&properties.session_id)
                .map(|s| s.to_string())
            {
                state.resolve_question_request(&task_id, &properties.request_id);
            }
            (None, None)
        }

        EventListResponse::QuestionRejected { properties } => {
            if let Some(task_id) = state
                .get_task_id_by_session(&properties.session_id)
                .map(|s| s.to_string())
            {
                state.resolve_question_request(&task_id, &properties.request_id);
            }
            (None, None)
        }

        _ => (None, None), // Ignore events we don't care about
    }
}
