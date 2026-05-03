//! Review handlers — review changes, file diff, accept/reject.

use super::super::App;


/// Open the diff review view focused on a specific file from the changed-files sidebar.
///
/// Runs `git diff HEAD -- <path>` in the project's working directory, parses the
/// output, and switches to `AppMode::DiffReview` with the selected file pre-loaded.
pub fn handle_open_file_diff(app: &mut App) {
    use crate::state::types::{AppMode, DiffReviewState, FocusedPanel};

    let (working_dir, file_path, task_number) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let files = match &state.ui.changed_files {
            Some(f) if !f.is_empty() => f.clone(),
            _ => return,
        };
        let idx = state.ui.selected_changed_file_index.min(files.len() - 1);
        let file = &files[idx];
        let path = file.path.clone();

        let task_id = match &state.ui.viewing_task_id {
            Some(id) => id.clone(),
            None => return,
        };

        let task = match state.tasks.get(&task_id) {
            Some(t) => t,
            None => return,
        };

        let project_id = task.project_id.clone();
        let task_number = task.number;

        let project = state
            .project_registry
            .projects
            .iter()
            .find(|p| p.id == project_id);

        let wd = match project {
            Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
            _ => return,
        };

        (wd, path, task_number)
    };

    // Spawn git diff for the specific file on a blocking thread
    let state = app.state.clone();
    let target_file_path = file_path.clone();
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("git")
            .args(["diff", "HEAD", "--", &target_file_path])
            .current_dir(&working_dir)
            .output();

        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());

        match output {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout);
                let files = crate::tui::diff_view::parse_git_diff(&raw);

                // Select the file matching the target path
                let selected_idx = files
                    .iter()
                    .position(|f| f.path == target_file_path)
                    .unwrap_or(0);

                state.ui.diff_review = Some(DiffReviewState {
                    files,
                    selected_file_index: selected_idx,
                    scroll_offset: 0,
                    error: None,
                    task_number,
                    files_list_focused: false,
                });
                state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                state.ui.mode = AppMode::DiffReview;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                state.ui.diff_review = Some(DiffReviewState {
                    files: Vec::new(),
                    selected_file_index: 0,
                    scroll_offset: 0,
                    error: Some(stderr.to_string()),
                    task_number,
                    files_list_focused: false,
                });
                state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                state.ui.mode = AppMode::DiffReview;
            }
            Err(e) => {
                state.set_notification(
                    format!("Failed to run git diff: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    3000,
                );
            }
        }
    });
}

/// Open the diff review view for the focused task.
///
/// Runs `git diff HEAD` in the project's working directory, parses the
/// output, and switches to `AppMode::DiffReview`.
pub fn handle_review_changes(app: &mut App) {
    use crate::state::types::{AgentStatus, AppMode, DiffReviewState, FocusedPanel};

    // Batch-read everything we need while holding the lock once.
    let (working_dir, task_number) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());

        // When viewing the task detail panel, prefer viewing_task_id (the task
        // whose detail page is open) over focused_task_id (the kanban cursor).
        let tid = if state.ui.focused_panel == FocusedPanel::TaskDetail {
            state
                .ui
                .viewing_task_id
                .as_ref()
                .or(state.ui.focused_task_id.as_ref())
        } else {
            state.ui.focused_task_id.as_ref()
        };

        let tid = match tid {
            Some(id) => id.clone(),
            None => {
                drop(state);
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No task selected".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
                return;
            }
        };

        let task = match state.tasks.get(&tid) {
            Some(t) => t,
            None => return,
        };

        // Only allow review for tasks that are done / ready / complete.
        let is_reviewable = matches!(
            task.agent_status,
            AgentStatus::Complete | AgentStatus::Ready
        );
        let agent_status_display = task.agent_status.clone();
        let project_id = task.project_id.clone();
        let task_number = task.number;

        if !is_reviewable {
            drop(state);
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                format!(
                    "Cannot review — task is {:?} (only Ready/Complete tasks can be reviewed)",
                    agent_status_display
                ),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            return;
        }

        let project = state
            .project_registry
            .projects
            .iter()
            .find(|p| p.id == project_id);

        let wd = match project {
            Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
            _ => {
                drop(state);
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No working directory configured for this project".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    3000,
                );
                return;
            }
        };

        (wd, task_number)
    };

    // Remember where we came from so Esc can return correctly
    {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.diff_review_source = Some(state.ui.focused_panel.clone());
    }

    // Spawn git diff on a blocking thread so we don't freeze the UI.
    let state = app.state.clone();
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&working_dir)
            .output();

        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());

        match output {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout);
                let files = crate::tui::diff_view::parse_git_diff(&raw);

                if files.is_empty() && raw.trim().is_empty() {
                    state.set_notification(
                        "No uncommitted changes to review".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        3000,
                    );
                    return;
                }

                state.ui.diff_review = Some(DiffReviewState {
                    files,
                    selected_file_index: 0,
                    scroll_offset: 0,
                    error: None,
                    task_number,
                    files_list_focused: false,
                });
                state.ui.mode = AppMode::DiffReview;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                state.ui.diff_review = Some(DiffReviewState {
                    files: Vec::new(),
                    selected_file_index: 0,
                    scroll_offset: 0,
                    error: Some(stderr.to_string()),
                    task_number,
                    files_list_focused: false,
                });
                state.ui.mode = AppMode::DiffReview;
            }
            Err(e) => {
                state.set_notification(
                    format!("Failed to run git diff: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    3000,
                );
            }
        }
    });
}

/// Accept a reviewed task — run `git add -A && git commit` natively,
/// then move the task to the "done" column.
pub fn handle_accept_review(app: &mut App) {
    use crate::state::types::{AgentStatus, ReviewStatus};

    // Batch-read: extract task info and working directory in one lock hold.
    let (task_info, working_dir) = {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());

        // Prefer viewing_task_id (task detail panel) over focused_task_id (kanban cursor).
        let tid = if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
            state
                .ui
                .viewing_task_id
                .as_ref()
                .or(state.ui.focused_task_id.as_ref())
                .cloned()
        } else {
            state.ui.focused_task_id.clone()
        };

        let tid = match tid {
            Some(id) => id,
            None => {
                drop(state);
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No task selected".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
                return;
            }
        };

        let task = match state.tasks.get(&tid) {
            Some(t) => t,
            None => return,
        };

        // Validate: must be in review column with AwaitingDecision status
        if task.column.0 != "review" || task.review_status != ReviewStatus::AwaitingDecision {
            drop(state);
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "Cannot accept — task is not awaiting review".to_string(),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            return;
        }

        let project_id = task.project_id.clone();
        let task_number = task.number;
        let description = task.description.clone();
        let task_id = tid;

        let project = state
            .project_registry
            .projects
            .iter()
            .find(|p| p.id == project_id);
        let wd = match project {
            Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
            _ => {
                drop(state);
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No working directory configured".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    3000,
                );
                return;
            }
        };

        // Mark as Approved + Running so the user sees activity
        if let Some(task) = state.tasks.get_mut(&task_id) {
            task.review_status = ReviewStatus::Approved;
        }
        state.update_task_agent_status(&task_id, AgentStatus::Running);

        ((task_id, task_number, description, project_id), wd)
    };

    let (task_id, task_number, description, project_id) = task_info;

    // Derive a commit message from the task title
    let commit_msg = {
        let title = crate::state::types::derive_title_from_description(&description);
        if title.is_empty() {
            format!("feat: task #{}", task_number)
        } else {
            // Capitalize first letter
            let mut msg = title;
            if let Some(first) = msg.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            format!("feat: {}", msg)
        }
    };

    let state = app.state.clone();
    let columns_config = app.config.columns.clone();
    let opencode_config = app.config.opencode.clone();

    // Spawn git add + commit on a blocking thread
    tokio::task::spawn_blocking(move || {
        // 1. git add -A
        let add_result = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&working_dir)
            .output();

        match add_result {
            Ok(_) => {}
            Err(e) => {
                tracing::error!("git add failed: {}", e);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    format!("git add failed: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    5000,
                );
                // Revert review status
                if let Some(task) = s.tasks.get_mut(&task_id) {
                    task.review_status = ReviewStatus::AwaitingDecision;
                    task.agent_status = AgentStatus::Complete;
                }
                s.mark_render_dirty();
                return;
            }
        }

        // 2. Check if there are changes to commit
        let status_result = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&working_dir)
            .output();

        let has_changes = match &status_result {
            Ok(out) => !out.stdout.is_empty(),
            Err(_) => true, // Assume there are changes if we can't check
        };

        if !has_changes {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.set_notification(
                "No changes to commit — moving to done".to_string(),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            // Move to done anyway
            s.move_task(&task_id, crate::state::types::KanbanColumn("done".to_string()));
            s.update_task_agent_status(&task_id, AgentStatus::Complete);
            s.mark_render_dirty();
            return;
        }

        // 3. git commit
        let commit_result = std::process::Command::new("git")
            .args(["commit", "-m", &commit_msg])
            .current_dir(&working_dir)
            .output();

        match commit_result {
            Ok(out) if out.status.success() => {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.move_task(&task_id, crate::state::types::KanbanColumn("done".to_string()));
                s.update_task_agent_status(&task_id, AgentStatus::Complete);
                s.set_notification(
                    format!("Task #{} committed and moved to done", task_number),
                    crate::state::types::NotificationVariant::Success,
                    3000,
                );
                s.mark_render_dirty();
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    format!("git commit failed: {}", stderr.trim()),
                    crate::state::types::NotificationVariant::Error,
                    5000,
                );
                // Revert review status
                if let Some(task) = s.tasks.get_mut(&task_id) {
                    task.review_status = ReviewStatus::AwaitingDecision;
                    task.agent_status = AgentStatus::Complete;
                }
                s.mark_render_dirty();
            }
            Err(e) => {
                tracing::error!("git commit failed: {}", e);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    format!("git commit failed: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    5000,
                );
                // Revert review status
                if let Some(task) = s.tasks.get_mut(&task_id) {
                    task.review_status = ReviewStatus::AwaitingDecision;
                    task.agent_status = AgentStatus::Complete;
                }
                s.mark_render_dirty();
            }
        }

        // Drop unused variables to suppress warnings
        let _ = (columns_config, opencode_config, project_id);
    });
}

/// Reject a reviewed task — move it back to the "running" column for re-work.
pub fn handle_reject_review(app: &mut App) {
    use crate::state::types::ReviewStatus;

    // Batch-read: extract task info in one lock hold.
    let task_info = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());

        let tid = if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
            state
                .ui
                .viewing_task_id
                .as_ref()
                .or(state.ui.focused_task_id.as_ref())
                .cloned()
        } else {
            state.ui.focused_task_id.clone()
        };

        let tid = match tid {
            Some(id) => id,
            None => {
                drop(state);
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No task selected".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
                return;
            }
        };

        let task = match state.tasks.get(&tid) {
            Some(t) => t,
            None => return,
        };

        // Validate: must be in review column with AwaitingDecision status
        if task.column.0 != "review" || task.review_status != ReviewStatus::AwaitingDecision {
            drop(state);
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "Cannot reject — task is not awaiting review".to_string(),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            return;
        }

        let task_id = tid;
        let task_number = task.number;
        let session_id = task.session_id.clone();
        let previous_agent = task.agent_type.clone();
        let project_id = task.project_id.clone();

        (task_id, task_number, session_id, previous_agent, project_id)
    };

    let (task_id, task_number, session_id, previous_agent, project_id) = task_info;

    // Get the OpenCode client for this project
    let client = {
        let _state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        app.opencode_clients.get(&project_id).cloned()
    };

    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());

    // Mark as Rejected
    if let Some(task) = state.tasks.get_mut(&task_id) {
        task.review_status = ReviewStatus::Rejected;
    }

    // Clear review agent session data so a fresh review runs next time
    state.clear_session_data(&task_id);
    // Also clear the session ID so the do agent gets a fresh session
    state.set_task_session_id(&task_id, None);

    // Move back to "running" column
    state.move_task(&task_id, crate::state::types::KanbanColumn("running".to_string()));

    // Reset review_status to Pending for the re-work cycle
    if let Some(task) = state.tasks.get_mut(&task_id) {
        task.review_status = ReviewStatus::Pending;
    }

    state.set_notification(
        format!("Task #{} rejected — sent back for re-work", task_number),
        crate::state::types::NotificationVariant::Info,
        3000,
    );
    state.mark_render_dirty();

    // Trigger orchestration: start the do agent for the running column
    if let Some(client) = client {
        state.update_task_agent_status(&task_id, crate::state::types::AgentStatus::Running);
        state.set_task_agent_type(
            &task_id,
            app.config.columns.agent_for_column("running"),
        );
        drop(state);

        crate::orchestration::engine::on_task_moved(
            &task_id,
            &crate::state::types::KanbanColumn("running".to_string()),
            &app.state,
            &client,
            &app.config.columns,
            &app.config.opencode,
            previous_agent,
        );
    } else {
        state.set_notification(
            "No OpenCode client — agent dispatch skipped".to_string(),
            crate::state::types::NotificationVariant::Warning,
            3000,
        );
    }

    // Suppress unused variable warning
    let _ = session_id;
}
