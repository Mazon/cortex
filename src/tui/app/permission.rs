//! Permission modal helpers — resolve permissions/questions, sync modal state.

use crate::opencode::client::OpenCodeClient;
use crate::state::types::{AppState, FocusedPanel, PermissionRequest, QuestionRequest};

use super::utils;

/// Get the effective session for the current view context.
///
/// When drilled into a subagent, returns the subagent's session data.
/// Otherwise, returns the main task's session data.
pub fn get_effective_session<'a>(
    state: &'a AppState,
) -> Option<&'a crate::state::types::TaskDetailSession> {
    if let Some(sid) = state.get_drilldown_session_id() {
        state.session_tracker.subagent_session_data.get(sid)
    } else if let Some(ref tid) = state.ui.viewing_task_id {
        state.session_tracker.task_sessions.get(tid)
    } else {
        None
    }
}

/// Check if there's a pending permission (vs question) for the current view.
pub fn has_pending_permission(state: &AppState) -> bool {
    if state.ui.focused_panel != FocusedPanel::TaskDetail {
        return false;
    }
    get_effective_session(state)
        .map(|s| !s.pending_permissions.is_empty())
        .unwrap_or(false)
}

/// Get the number of options in the current modal.
pub fn get_modal_option_count(state: &AppState) -> usize {
    if state.ui.focused_panel != FocusedPanel::TaskDetail {
        return 0;
    }
    get_effective_session(state)
        .map(|s| {
            if !s.pending_permissions.is_empty() {
                2 // Yes, No
            } else if !s.pending_questions.is_empty() {
                s.pending_questions[0].answers.len()
            } else {
                0
            }
        })
        .unwrap_or(0)
}

/// After resolving a permission/question, sync modal state.
/// Closes the modal if no more pending items remain, resets selection otherwise.
pub fn sync_modal_after_resolve(s: &mut AppState, task_id: &str) {
    if s.ui.permission_modal_active {
        let has_more = s
            .session_tracker
            .task_sessions
            .get(task_id)
            .map(|sess| {
                !sess.pending_permissions.is_empty() || !sess.pending_questions.is_empty()
            })
            .unwrap_or(false);
        if has_more {
            s.ui.permission_modal_selected_index = 0;
        } else {
            s.ui.permission_modal_active = false;
            s.ui.permission_modal_selected_index = 0;
        }
    }
}

/// Resolve a permission request asynchronously.
pub fn resolve_permission_async(
    state: &std::sync::Arc<std::sync::Mutex<AppState>>,
    client: OpenCodeClient,
    session_id: String,
    perm_id: String,
    task_id: String,
    approve: bool,
) {
    let state = state.clone();
    tokio::spawn(async move {
        match client
            .resolve_permission(&session_id, &perm_id, approve)
            .await
        {
            Ok(()) => {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.resolve_permission_request(&task_id, &perm_id, approve);
                sync_modal_after_resolve(&mut s, &task_id);
                s.set_notification(
                    if approve {
                        "Permission approved".to_string()
                    } else {
                        "Permission rejected".to_string()
                    },
                    crate::state::types::NotificationVariant::Success,
                    2000,
                );
                s.mark_render_dirty();
            }
            Err(e) => {
                tracing::error!("Failed to resolve permission {}: {}", perm_id, e);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    format!("Failed to resolve permission: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    5000,
                );
                s.mark_render_dirty();
            }
        }
    });
}

/// Execute the currently selected modal option (approve/reject permission or answer question).
pub fn handle_modal_confirm(app: &super::App) {
    tracing::debug!("handle_modal_confirm: invoked");
    // Determine what to do based on what's pending
    let (pending_perm, pending_question, task_id, client): (
        Option<PermissionRequest>,
        Option<QuestionRequest>,
        Option<String>,
        Option<OpenCodeClient>,
    ) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.ui.focused_panel != FocusedPanel::TaskDetail {
            (None, None, None, None)
        } else {
            let session = get_effective_session(&state);
            let perm = session.and_then(|s| s.pending_permissions.first().cloned());
            let question = session.and_then(|s| s.pending_questions.first().cloned());
            let client = state
                .project_registry
                .active_project_id
                .as_ref()
                .and_then(|pid| app.opencode_clients.get(pid))
                .cloned();
            // task_id is the viewing_task_id (main task)
            let tid = state.ui.viewing_task_id.clone();
            (perm, question, tid, client)
        }
    };

    let selected_index = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.permission_modal_selected_index
    };

    // --- Precondition checks with user-visible feedback ---

    if pending_perm.is_none() && pending_question.is_none() {
        if task_id.is_some() {
            tracing::warn!(
                "handle_modal_confirm: no pending permission/question found for task {:?}",
                task_id
            );
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.permission_modal_active = false;
            state.ui.permission_modal_selected_index = 0;
            state.set_notification(
                "No pending permission or question to resolve".to_string(),
                crate::state::types::NotificationVariant::Warning,
                3000,
            );
            state.mark_render_dirty();
        } else {
            tracing::debug!("handle_modal_confirm: no active task context");
        }
        return;
    }

    if client.is_none() {
        tracing::error!("handle_modal_confirm: no OpenCode client for active project");
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "Cannot resolve: no server connection for this project".to_string(),
            crate::state::types::NotificationVariant::Error,
            5000,
        );
        state.mark_render_dirty();
        return;
    }

    // At this point: client, task_id, and at least one of pending_perm/pending_question are Some.
    let client = client.unwrap();
    let task_id = task_id.unwrap();

    if let Some(perm) = pending_perm {
        // Bounds check — permissions have exactly 2 options (Yes/No)
        if selected_index > 1 {
            return;
        }
        // Permission — selected_index 0 = Yes (approve), 1 = No (reject)
        let approve = selected_index == 0;
        resolve_permission_async(
            &app.state,
            client,
            perm.session_id.clone(),
            perm.id.clone(),
            task_id,
            approve,
        );
    } else if let Some(question) = pending_question {
        // Question — selected_index maps to answer index
        if selected_index < question.answers.len() {
            let answer = question.answers[selected_index].clone();
            utils::resolve_question_with_reassess(
                app.state.clone(),
                client,
                question.id.clone(),
                question.session_id.clone(),
                answer,
                task_id,
                app.config.columns.clone(),
                app.config.opencode.clone(),
            );
        }
    }
}
