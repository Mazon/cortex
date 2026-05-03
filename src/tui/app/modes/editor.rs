//! Editor mode key handler — task editor, rename, input prompt.

use super::super::App;

/// Identifies which input prompt is active, so `handle_text_input` can
/// dispatch submit/cancel to the correct state method.
#[derive(Clone, Copy)]
enum InputPrompt {
    RenameProject,
    WorkingDirectory,
    NewProjectDirectory,
    AddDependency,
}

/// Handle key events in TaskEditor mode.
pub fn handle_editor_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crate::tui::editor_handler::{handle_editor_input, EditorAction};

    let action = {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(editor) = state.get_task_editor_mut() {
            handle_editor_input(editor, key, &app.editor_key_matcher)
        } else {
            EditorAction::None
        }
    };

    match action {
        EditorAction::Save => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            match state.save_task_editor() {
                Ok(task_id) => {
                    // Extract column ID before closing editor
                    let column_id = state.get_task_editor().and_then(|ed| ed.column_id.clone());

                    // Close the editor and return to normal mode
                    state.cancel_task_editor();
                    state.set_notification(
                        format!("Task saved: {}", task_id),
                        crate::state::types::NotificationVariant::Success,
                        3000,
                    );

                    // Focus the newly created/saved task
                    state.ui.focused_task_id = Some(task_id.clone());

                    // Highlight the saved task for visual feedback
                    state.highlight_task(task_id.clone(), 3000);

                    // Update focused column to match the saved task's column
                    if let Some(ref col_id) = column_id {
                        let visible = app.config.columns.visible_column_ids();
                        if let Some(idx) = visible.iter().position(|c| c == col_id) {
                            state.ui.focused_column = col_id.clone();
                            state.kanban.focused_column_index = idx;
                        }
                    }

                    // Auto-launch agent if column has one configured
                    if let Some(ref col_id) = column_id {
                        let agent_name = app.config.columns.agent_for_column(col_id);
                        tracing::debug!(
                            "Task {} saved in column '{}', agent_for_column={:?}",
                            task_id,
                            col_id,
                            agent_name
                        );
                        if let Some(_agent) = agent_name {
                            // Check if task already has a running agent
                            let already_running = state
                                .tasks
                                .get(&task_id)
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
                                    .get(&task_id)
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
                                        let previous_agent = state
                                            .tasks
                                            .get(&task_id)
                                            .and_then(|t| t.agent_type.clone());
                                        // Set status to Running while holding the lock to close the race window
                                        state.update_task_agent_status(
                                            &task_id,
                                            crate::state::types::AgentStatus::Running,
                                        );
                                        state.set_task_agent_type(
                                            &task_id,
                                            app.config.columns.agent_for_column(col_id),
                                        );
                                        drop(state); // Release lock before spawning async
                                        crate::orchestration::engine::on_task_moved(
                                            &task_id,
                                            &crate::state::types::KanbanColumn(col_id.clone()),
                                            &app.state,
                                            &client,
                                            &app.config.columns,
                                            &app.config.opencode,
                                            previous_agent,
                                        );
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
                    }
                }
                Err(e) => {
                    // Only show a notification toast if there's no inline
                    // validation error (which is already visible in the editor).
                    let has_inline_error = state
                        .get_task_editor()
                        .map_or(false, |ed| ed.validation_error.is_some());
                    if !has_inline_error {
                        state.set_notification(
                            format!("Save failed: {}", e),
                            crate::state::types::NotificationVariant::Error,
                            3000,
                        );
                    }
                }
            }
        }
        EditorAction::Cancel => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.cancel_task_editor();
        }
        EditorAction::None => {}
    }
}

/// Handle key events in ProjectRename mode.
pub fn handle_rename_key(app: &mut App, key: crossterm::event::KeyEvent) {
    handle_text_input(app, key, InputPrompt::RenameProject);
}

/// Handle key events in InputPrompt mode (used for working directory and
/// new project directory).
pub fn handle_input_prompt_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let prompt_type = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.prompt_context.as_deref().map(|c| match c {
            "new_project_directory" => InputPrompt::NewProjectDirectory,
            "set_working_directory" => InputPrompt::WorkingDirectory,
            "add_dependency" => InputPrompt::AddDependency,
            _ => InputPrompt::WorkingDirectory,
        })
    };
    if let Some(pt) = prompt_type {
        handle_text_input(app, key, pt);
    }
}

/// Shared text-input key handler for single-line input prompts.
///
/// Used by both the project-rename and working-directory prompts.
/// Handles character insertion, backspace, delete, cursor movement,
/// Home/End, Enter (submit), and Escape (cancel).
fn handle_text_input(app: &mut App, key: crossterm::event::KeyEvent, prompt: InputPrompt) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Enter => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            match prompt {
                InputPrompt::RenameProject => {
                    match state.submit_project_rename() {
                        Some((_old, new)) => {
                            state.set_notification(
                                format!("Project renamed to \"{}\"", new),
                                crate::state::types::NotificationVariant::Success,
                                3000,
                            );
                        }
                        None => {
                            // Empty name — show warning and stay in rename mode
                            state.set_notification(
                                "Project name cannot be empty".to_string(),
                                crate::state::types::NotificationVariant::Warning,
                                2000,
                            );
                        }
                    }
                }
                InputPrompt::WorkingDirectory => match state.submit_working_directory() {
                    Ok(true) => {
                        state.set_notification(
                            "Working directory updated".to_string(),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );
                    }
                    Ok(false) => {
                        state.set_notification(
                            "Working directory cannot be empty".to_string(),
                            crate::state::types::NotificationVariant::Warning,
                            2000,
                        );
                    }
                    Err(msg) => {
                        state.set_notification(
                            msg,
                            crate::state::types::NotificationVariant::Error,
                            3000,
                        );
                    }
                },
                InputPrompt::NewProjectDirectory => {
                    match state.submit_new_project_directory() {
                        Ok(name) => {
                            // Register the shared OpenCode client for the new project.
                            // All projects share a single server, so we clone any existing client.
                            if let Some(new_pid) =
                                state.project_registry.active_project_id.clone()
                            {
                                if let Some(existing_client) =
                                    app.opencode_clients.values().next()
                                {
                                    app.opencode_clients
                                        .insert(new_pid.clone(), existing_client.clone());
                                    state.set_project_connected(&new_pid, true);
                                }
                            }
                            state.set_notification(
                                format!("Created project \"{}\"", name),
                                crate::state::types::NotificationVariant::Success,
                                3000,
                            );
                        }
                        Err(msg) => {
                            state.set_notification(
                                msg,
                                crate::state::types::NotificationVariant::Error,
                                3000,
                            );
                        }
                    }
                }
                InputPrompt::AddDependency => {
                    let input = state.ui.input_text.trim().to_string();
                    let focused_task_id = state.ui.focused_task_id.clone();
                    if input.is_empty() {
                        state.set_notification(
                            "Task number cannot be empty".to_string(),
                            crate::state::types::NotificationVariant::Warning,
                            2000,
                        );
                    } else if let Some(tid) = focused_task_id {
                        // Find the task by number
                        let dep_id = state
                            .tasks
                            .values()
                            .find(|t| {
                                t.project_id
                                    == state
                                        .project_registry
                                        .active_project_id
                                        .as_deref()
                                        .unwrap_or("")
                                    && t.number.to_string() == input
                            })
                            .map(|t| t.id.clone());

                        match dep_id {
                            Some(dep_id) => {
                                match state.add_dependency(&tid, &dep_id) {
                                    Ok(()) => {
                                        state.set_notification(
                                            "Dependency added".to_string(),
                                            crate::state::types::NotificationVariant::Success,
                                            3000,
                                        );
                                    }
                                    Err(msg) => {
                                        state.set_notification(
                                            msg,
                                            crate::state::types::NotificationVariant::Warning,
                                            3000,
                                        );
                                    }
                                }
                            }
                            None => {
                                state.set_notification(
                                    format!("Task #{} not found in current project", input),
                                    crate::state::types::NotificationVariant::Warning,
                                    3000,
                                );
                            }
                        }
                    }
                }
            }
        }
        KeyCode::Esc => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            match prompt {
                InputPrompt::RenameProject => state.cancel_project_rename(),
                InputPrompt::WorkingDirectory => state.cancel_working_directory(),
                InputPrompt::NewProjectDirectory => state.cancel_new_project_directory(),
                InputPrompt::AddDependency => {
                    state.ui.mode = crate::state::types::AppMode::Normal;
                    state.ui.prompt_context = None;
                }
            }
        }
        KeyCode::Char(c) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let char_count = state.ui.input_text.chars().count();
            let cursor = state.ui.input_cursor.min(char_count);
            // Convert char index to byte offset for insertion.
            let byte_pos = state
                .ui
                .input_text
                .char_indices()
                .nth(cursor)
                .map(|(i, _)| i)
                .unwrap_or(state.ui.input_text.len());
            state.ui.input_text.insert(byte_pos, c);
            // The inserted char is exactly 1 char wide; advance cursor.
            state.ui.input_cursor = cursor + 1;
        }
        KeyCode::Backspace => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.ui.input_cursor > 0 {
                let cursor = state.ui.input_cursor;
                // Find the byte range of the char just before the cursor.
                let char_indices: Vec<(usize, char)> =
                    state.ui.input_text.char_indices().collect();
                if let Some(&(byte_start, ch)) = char_indices.get(cursor - 1) {
                    let byte_end = byte_start + ch.len_utf8();
                    state.ui.input_text.replace_range(byte_start..byte_end, "");
                }
                state.ui.input_cursor = cursor - 1;
            }
        }
        KeyCode::Delete => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let char_count = state.ui.input_text.chars().count();
            if state.ui.input_cursor < char_count {
                let cursor = state.ui.input_cursor;
                let char_indices: Vec<(usize, char)> =
                    state.ui.input_text.char_indices().collect();
                if let Some(&(byte_start, ch)) = char_indices.get(cursor) {
                    let byte_end = byte_start + ch.len_utf8();
                    state.ui.input_text.replace_range(byte_start..byte_end, "");
                }
            }
        }
        KeyCode::Left => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.input_cursor = state.ui.input_cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let char_count = state.ui.input_text.chars().count();
            let new_pos = state.ui.input_cursor + 1;
            state.ui.input_cursor = new_pos.min(char_count);
        }
        KeyCode::Home => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.input_cursor = 0;
        }
        KeyCode::End => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.input_cursor = state.ui.input_text.chars().count();
        }
        _ => {} // Ignore other keys
    }
}
