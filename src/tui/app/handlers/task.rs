//! Task handlers — create, open detail, move, delete, abort, retry.

use super::super::App;
use super::super::utils;

/// Open the task editor to create a new task.
pub fn handle_create_task(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    let col_id = state.ui.focused_column.clone();
    state.open_task_editor_create(&col_id);
}

/// Open the task detail view for the focused task.
/// Async loads changed files for reviewable tasks.
pub fn handle_open_task_detail(app: &mut App) {
    use crate::state::types::FocusedPanel;

    let (task_id, is_reviewable, working_dir) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let tid = state.ui.focused_task_id.clone();

        let (reviewable, wd) = tid.as_ref().and_then(|id| {
            let task = state.tasks.get(id)?;
            let reviewable = matches!(
                task.agent_status,
                crate::state::types::AgentStatus::Complete
                    | crate::state::types::AgentStatus::Ready
            );
            let project = state
                .project_registry
                .projects
                .iter()
                .find(|p| p.id == task.project_id);
            let wd = project
                .filter(|p| !p.working_directory.is_empty())
                .map(|p| p.working_directory.clone());
            Some((reviewable, wd))
        }).unwrap_or((false, None));

        (tid, reviewable, wd)
    };

    {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        match task_id {
            Some(ref id) => {
                state.open_task_detail(id);
                // Set diff review source so Esc returns to task detail
                state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
            }
            None => state.set_notification(
                "No task selected".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            ),
        }
    }

    // Async load changed files for reviewable tasks
    if is_reviewable {
        if let (Some(tid), Some(wd)) = (task_id, working_dir) {
            let state = app.state.clone();
            tokio::task::spawn_blocking(move || {
                let numstat = std::process::Command::new("git")
                    .args(["diff", "--numstat", "HEAD"])
                    .current_dir(&wd)
                    .output();
                let name_status = std::process::Command::new("git")
                    .args(["diff", "--name-status", "HEAD"])
                    .current_dir(&wd)
                    .output();

                let files = if let (Ok(ns_out), Ok(ns_stat)) = (numstat, name_status) {
                    if ns_out.status.success() && ns_stat.status.success() {
                        utils::parse_changed_files(&ns_out.stdout, &ns_stat.stdout)
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                    state.ui.changed_files = if files.is_empty() {
                        None
                    } else {
                        Some(files)
                    };
                    state.ui.selected_changed_file_index = 0;
                    state.mark_render_dirty();
                }
            });
        }
    }
}

/// Move the focused task forward or backward by `direction` columns (+1 or -1).
pub fn handle_move_task(app: &mut App, direction: i32) {
    let visible = app.config.columns.visible_column_ids();
    let (task_id, current_col_idx) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let tid = state.ui.focused_task_id.clone();
        let idx = state.kanban.focused_column_index;
        (tid, idx)
    };
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    match task_id {
        Some(tid) => {
            let target_idx = current_col_idx as i32 + direction;
            if target_idx >= 0 && (target_idx as usize) < visible.len() {
                let target_col = visible[target_idx as usize].clone();
                state.move_task(&tid, crate::state::types::KanbanColumn(target_col.clone()));

                // Trigger orchestration engine if the target column has an agent configured
                if let Some(_agent) = app.config.columns.agent_for_column(&target_col) {
                    let already_running = state
                        .tasks
                        .get(&tid)
                        .map(|t| {
                            matches!(
                                t.agent_status,
                                crate::state::types::AgentStatus::Running
                                    | crate::state::types::AgentStatus::Hung
                            )
                        })
                        .unwrap_or(false);
                    if already_running {
                        let status = state
                            .tasks
                            .get(&tid)
                            .map(|t| t.agent_status.clone())
                            .unwrap_or(crate::state::types::AgentStatus::Pending);
                        if status == crate::state::types::AgentStatus::Hung {
                            state.set_notification(
                                "Task is hung — abort the session before re-dispatching"
                                    .to_string(),
                                crate::state::types::NotificationVariant::Warning,
                                5000,
                            );
                        }
                    } else {
                        if let Some(project_id) =
                            state.project_registry.active_project_id.clone()
                        {
                            if let Some(client) =
                                app.opencode_clients.get(&project_id).cloned()
                            {
                                // Capture the PREVIOUS agent type before overwriting it,
                                // so start_agent can detect the change and create a fresh session.
                                let previous_agent =
                                    state.tasks.get(&tid).and_then(|t| t.agent_type.clone());
                                // Set status to Running while holding the lock to close the race window
                                state.update_task_agent_status(
                                    &tid,
                                    crate::state::types::AgentStatus::Running,
                                );
                                state.set_task_agent_type(
                                    &tid,
                                    app.config.columns.agent_for_column(&target_col),
                                );
                                drop(state); // Release lock before spawning async
                                crate::orchestration::engine::on_task_moved(
                                    &tid,
                                    &crate::state::types::KanbanColumn(target_col),
                                    &app.state,
                                    &client,
                                    &app.config.columns,
                                    &app.config.opencode,
                                    previous_agent,
                                );
                                return; // Lock already dropped
                            } else {
                                state.set_notification(
                                    "No OpenCode client for this project".to_string(),
                                    crate::state::types::NotificationVariant::Warning,
                                    3000,
                                );
                            }
                        } else {
                            state.set_notification(
                                "No active project — agent dispatch skipped".to_string(),
                                crate::state::types::NotificationVariant::Warning,
                                3000,
                            );
                        }
                    }
                }
            } else {
                let msg = if direction > 0 {
                    "Already at the last column"
                } else {
                    "Already at the first column"
                };
                state.set_notification(
                    msg.to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    2000,
                );
            }
        }
        None => {
            state.set_notification(
                "No task selected to move".to_string(),
                crate::state::types::NotificationVariant::Warning,
                2000,
            );
        }
    }
}

/// Delete the focused task (enter confirmation mode).
pub fn handle_delete_task(app: &mut App) {
    let task_id = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.focused_task_id.clone()
    };
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    match task_id {
        Some(tid) => {
            // Store pending delete and enter confirm mode
            state.ui.pending_delete_task_id = Some(tid);
            state.ui.mode = crate::state::types::AppMode::ConfirmDelete;
            state.mark_render_dirty();
        }
        None => {
            state.set_notification(
                "No task selected to delete".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
    }
}

/// Execute the pending deletion (called from confirm mode).
pub fn execute_delete_task(app: &mut App) {
    let task_id = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.pending_delete_task_id.clone()
    };
    let project_id = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.project_registry.active_project_id.clone()
    };

    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.ui.pending_delete_task_id = None;
    state.ui.mode = crate::state::types::AppMode::Normal;

    match task_id {
        Some(tid) => {
            let deleted_session_id = state.delete_task(&tid);

            // Clamp focused task index for the column
            let col_id = state.ui.focused_column.clone();
            state.clamp_focused_task_index(&col_id);

            // Close detail view if viewing the deleted task
            if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                state.close_task_detail();
            }

            state.set_notification(
                "Task deleted".to_string(),
                crate::state::types::NotificationVariant::Info,
                3000,
            );

            // Abort the remote session if one existed
            if let Some(session_id) = deleted_session_id {
                if let Some(pid) = &project_id {
                    if let Some(client) = app.opencode_clients.get(pid).cloned() {
                        tokio::spawn(async move {
                            if let Err(_e) = client.abort_session(&session_id).await {}
                        });
                    }
                }
            }
        }
        None => {}
    }
}

/// Abort the active session for the focused task.
pub fn handle_abort_session(app: &mut App) {
    // Batch read: extract session_id and client in a single lock hold.
    let (session_id, client) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let session_id = state
            .ui
            .focused_task_id
            .as_ref()
            .and_then(|tid| state.tasks.get(tid))
            .and_then(|t| t.session_id.clone());
        let client = state
            .project_registry
            .active_project_id
            .as_ref()
            .and_then(|pid| app.opencode_clients.get(pid))
            .cloned();
        (session_id, client)
    };

    if let Some(sid) = session_id {
        if let Some(client) = client {
            let state = app.state.clone();
            tokio::spawn(async move {
                let abort_failed = match client.abort_session(&sid).await {
                    Ok(aborted) => {
                        let _ = aborted;
                        false
                    }
                    Err(e) => {
                        tracing::error!("Failed to abort session {}: {}", sid, e);
                        // Tracing layer will also push a notification automatically
                        true
                    }
                };
                // Update notification after attempt
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if abort_failed {
                    state.set_notification(
                        format!("Failed to abort session: {}", sid),
                        crate::state::types::NotificationVariant::Error,
                        5000,
                    );
                } else {
                    state.set_notification(
                        format!("Session abort requested: {}", sid),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
                }
                state.mark_render_dirty();
            });
        } else {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No client available to abort session".to_string(),
                crate::state::types::NotificationVariant::Error,
                3000,
            );
        }
    } else {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "No active session to abort".to_string(),
            crate::state::types::NotificationVariant::Info,
            2000,
        );
    }
}

/// Retry a hung or errored task — abort the old session, clear stale state,
/// and re-dispatch the agent for the task's current column.
pub fn handle_retry_task(app: &mut App) {
    use crate::state::types::AgentStatus;

    // Batch read: extract task info, client, and column config in one lock hold.
    let (task_info, client) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let tid = state.ui.focused_task_id.clone();
        let info = tid.as_ref().and_then(|id| {
            let task = state.tasks.get(id)?;
            Some((
                id.clone(),
                task.agent_status.clone(),
                task.session_id.clone(),
                task.column.0.clone(),
            ))
        });
        let client = state
            .project_registry
            .active_project_id
            .as_ref()
            .and_then(|pid| app.opencode_clients.get(pid))
            .cloned();
        (info, client)
    };

    let Some((task_id, agent_status, session_id, column_id)) = task_info else {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "No task selected to retry".to_string(),
            crate::state::types::NotificationVariant::Info,
            2000,
        );
        return;
    };

    // Only allow retry for Hung or Error tasks
    if !matches!(agent_status, AgentStatus::Hung | AgentStatus::Error) {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            format!(
                "Cannot retry — task status is {:?} (only Hung/Error can be retried)",
                agent_status
            ),
            crate::state::types::NotificationVariant::Info,
            3000,
        );
        return;
    }

    // Require an OpenCode client for the active project
    let Some(client) = client else {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "No OpenCode client for this project".to_string(),
            crate::state::types::NotificationVariant::Warning,
            3000,
        );
        return;
    };

    // Require the current column to have an agent configured
    if app.config.columns.agent_for_column(&column_id).is_none() {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "No agent configured for this column — cannot retry".to_string(),
            crate::state::types::NotificationVariant::Warning,
            3000,
        );
        return;
    }

    let state = app.state.clone();
    let columns_config = app.config.columns.clone();
    let opencode_config = app.config.opencode.clone();

    tokio::spawn(async move {
        // 1. Abort the old session if one exists
        if let Some(ref sid) = session_id {
            match client.abort_session(sid).await {
                Ok(_) => {
                    tracing::info!("Retry: aborted old session {}", sid);
                }
                Err(e) => {
                    tracing::warn!("Retry: failed to abort old session {}: {}", sid, e);
                }
            }
        }

        // 2. Clear session data, reset error state, set status to Running
        let previous_agent = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_task_session_id(&task_id, None);
            state.clear_session_data(&task_id);

            // Fix 1: Reset stale pending counts so indicators don't linger
            if let Some(task) = state.tasks.get_mut(&task_id) {
                task.error_message = None;
                task.pending_permission_count = 0;
                task.pending_question_count = 0;
            }

            // Fix 2: Clean up stale subagent sessions from previous run
            if let Some(sessions) = state.session_tracker.subagent_sessions.remove(&task_id) {
                for sub in &sessions {
                    state
                        .session_tracker
                        .subagent_to_parent
                        .remove(&sub.session_id);
                    state
                        .session_tracker
                        .subagent_session_data
                        .remove(&sub.session_id);
                }
            }

            state.update_task_agent_status(&task_id, AgentStatus::Running);

            // Fix 3: Recalculate project status so it reflects Running
            if let Some(task) = state.tasks.get(&task_id) {
                let project_id = task.project_id.clone();
                state.update_project_status(&project_id);
            }

            // Fix 4: Clear navigation stack if it references this task
            if state
                .ui
                .session_nav_stack
                .iter()
                .any(|r| r.task_id == task_id)
            {
                state.ui.session_nav_stack.clear();
                // If we were viewing this task's detail, close it since
                // the old session data is now invalid
                if state.ui.viewing_task_id.as_deref() == Some(&task_id) {
                    state.close_task_detail();
                }
            }

            state.mark_render_dirty();

            // Capture previous agent type for on_task_moved
            state.tasks.get(&task_id).and_then(|t| t.agent_type.clone())
        };

        // 3. Re-dispatch the agent for the task's current column
        crate::orchestration::engine::on_task_moved(
            &task_id,
            &crate::state::types::KanbanColumn(column_id.clone()),
            &state,
            &client,
            &columns_config,
            &opencode_config,
            previous_agent,
        );

        // 4. Notify the user
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "Task retry — re-dispatching agent".to_string(),
            crate::state::types::NotificationVariant::Success,
            3000,
        );
        state.mark_render_dirty();
    });
}
