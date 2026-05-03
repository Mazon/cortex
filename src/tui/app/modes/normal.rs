//! Normal mode key handler — layered interception of key events.
//!
//! Key interception order (first match wins):
//! 1. Detail editor inline editing (when editor is focused)
//! 2. Tab to focus editor (when detail view is open but editor not focused)
//! 3. Changed-files sidebar focus
//! 4. Permission modal key interception
//! 5. Task detail view — Escape, scroll, y/n for permissions, digit keys for questions
//! 6. Vim-style Ctrl+R (circuit breaker reset)
//! 7. Configurable keybinding dispatch (Action-based)

use super::super::App;
use crate::opencode::client::OpenCodeClient;
use crate::tui::keys::Action;

/// Handle key events in Normal mode.
///
/// Resolves the key to an [`Action`] via the configured keybindings and
/// dispatches to the appropriate handler method.
pub fn handle_normal_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};

    // ── Detail editor inline editing ──────────────────────────────
    // When the detail editor is focused, intercept editing keys.
    // This must come before all other handlers to prevent key conflicts.
    {
        let is_detail_editor_focused = {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                && state
                    .ui
                    .detail_editor
                    .as_ref()
                    .map_or(false, |e| e.is_focused)
        };

        if is_detail_editor_focused {
            use crate::state::types::{AgentStatus, CursorDirection};
            use crate::tui::keys::EditorKeyAction;

            // Check configurable editor keybindings first (Ctrl+S, Esc, Tab, Enter)
            let editor_action = {
                let _state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                app.editor_key_matcher.match_key(key)
            };

            if let Some(action) = editor_action {
                match action {
                    EditorKeyAction::Save => {
                        // Ctrl+S in detail view: unfocus the editor.
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.is_focused = false;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    EditorKeyAction::Cancel => {
                        // Esc: unfocus the prompt input
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.is_focused = false;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    EditorKeyAction::CycleField => {
                        // Tab: toggle focus on/off
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            if ed.discard_warning_shown {
                                // Clear warning on Tab
                                ed.discard_warning_shown = false;
                            }
                            ed.is_focused = false;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    EditorKeyAction::Submit => {
                        // Submit is handled via Newline (Enter) in the detail view
                        return;
                    }
                    EditorKeyAction::Newline => {
                        // Enter: submit prompt (only without ctrl/alt modifiers)
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            // Ctrl+Enter = save description, don't submit prompt
                            return;
                        }
                        // Submit the prompt from the input field
                        {
                            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                            let (prompt_text, task_id, agent_status, session_id, agent_type) = {
                                let task_id = state.ui.viewing_task_id.clone();
                                let prompt = state
                                    .ui
                                    .detail_editor
                                    .as_ref()
                                    .map(|ed| ed.description())
                                    .unwrap_or_default();
                                let prompt = prompt.trim().to_string();
                                let (status, sid, agent) =
                                    task_id.as_ref().and_then(|tid| {
                                        state.tasks.get(tid).map(|t| {
                                            (
                                                t.agent_status.clone(),
                                                t.session_id.clone(),
                                                t.agent_type.clone(),
                                            )
                                        })
                                    }).unwrap_or((AgentStatus::Pending, None, None));
                                (prompt, task_id, status, sid, agent)
                            };

                            if prompt_text.is_empty() {
                                // Don't submit empty prompts
                                drop(state);
                                return;
                            }

                            let task_id = match task_id {
                                Some(id) => id,
                                None => {
                                    drop(state);
                                    return;
                                }
                            };

                            match agent_status {
                                AgentStatus::Running | AgentStatus::Pending => {
                                    // Agent is running — queue the prompt
                                    if let Some(task) = state.tasks.get_mut(&task_id) {
                                        task.queued_prompt = Some(prompt_text.clone());
                                    }
                                    // Clear the input field
                                    if let Some(ed) = state.ui.detail_editor.as_mut() {
                                        ed.desc_lines = vec![String::new()];
                                        ed.cached_description = None;
                                        ed.cursor_row = 0;
                                        ed.cursor_col = 0;
                                        ed.has_unsaved_changes = false;
                                        ed.is_focused = false;
                                    }
                                    state.set_notification(
                                        "Prompt queued — will be sent after current prompt completes".to_string(),
                                        crate::state::types::NotificationVariant::Info,
                                        3000,
                                    );
                                    state.mark_render_dirty();
                                }
                                AgentStatus::Ready | AgentStatus::Complete | AgentStatus::Question => {
                                    // Agent is idle — send immediately
                                    if session_id.is_some() {
                                        // Set active_prompt on the session
                                        if let Some(ref sid) = session_id {
                                            if let Some(session) =
                                                state.session_tracker.task_sessions.get_mut(sid)
                                            {
                                                session.active_prompt = Some(prompt_text.clone());
                                                session.render_version += 1;
                                            }
                                        }
                                        // Update agent status to Running
                                        state.update_task_agent_status(&task_id, AgentStatus::Running);
                                        // Clear the input field
                                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                                            ed.desc_lines = vec![String::new()];
                                            ed.cached_description = None;
                                            ed.cursor_row = 0;
                                            ed.cursor_col = 0;
                                            ed.has_unsaved_changes = false;
                                            ed.is_focused = false;
                                        }
                                        state.set_notification(
                                            "Sending prompt...".to_string(),
                                            crate::state::types::NotificationVariant::Info,
                                            2000,
                                        );
                                        state.mark_render_dirty();

                                        // Spawn async task to send the prompt
                                        let state_clone = app.state.clone();
                                        let sid = session_id.unwrap();
                                        let agent = agent_type.clone();
                                        let opencode_config = app.config.opencode.clone();
                                        let prompt = prompt_text;

                                        // Get the client for this task's project
                                        let client = {
                                            let pid = state
                                                .tasks
                                                .get(&task_id)
                                                .map(|t| t.project_id.clone());
                                            drop(state);
                                            pid.and_then(|pid| app.opencode_clients.get(&pid).cloned())
                                        };

                                        if let Some(client) = client {
                                            tokio::spawn(async move {
                                                let agent_model = opencode_config
                                                    .agents
                                                    .get(agent.as_deref().unwrap_or(""))
                                                    .and_then(|a| a.model.clone());
                                                let model = agent_model
                                                    .as_deref()
                                                    .map(|m| {
                                                        if m.contains('/') {
                                                            m.to_string()
                                                        } else {
                                                            let provider = opencode_config
                                                                .model
                                                                .provider
                                                                .as_deref()
                                                                .unwrap_or("z.ai");
                                                            format!("{}/{}", provider, m)
                                                        }
                                                    })
                                                    .or_else(|| {
                                                        let provider = opencode_config
                                                            .model
                                                            .provider
                                                            .as_deref()
                                                            .unwrap_or("z.ai");
                                                        Some(format!(
                                                            "{}/{}",
                                                            provider,
                                                            opencode_config.model.id
                                                        ))
                                                    });

                                                match client
                                                    .send_prompt(
                                                        &sid,
                                                        &prompt,
                                                        agent.as_deref(),
                                                        model.as_deref(),
                                                    )
                                                    .await
                                                {
                                                    Ok(_) => {
                                                        tracing::debug!(
                                                            "Follow-up prompt sent: session={}",
                                                            sid
                                                        );
                                                    }
                                                    Err(e) => {
                                                        tracing::error!(
                                                            "Failed to send follow-up prompt: {}",
                                                            e
                                                        );
                                                        let mut s = state_clone
                                                            .lock()
                                                            .unwrap_or_else(|e| e.into_inner());
                                                        s.set_notification(
                                                            format!("Failed to send prompt: {}", e),
                                                            crate::state::types::NotificationVariant::Error,
                                                            5000,
                                                        );
                                                        s.mark_render_dirty();
                                                    }
                                                }
                                            });
                                        }
                                    } else {
                                        // No session — can't send
                                        state.set_notification(
                                            "No active session — cannot send prompt".to_string(),
                                            crate::state::types::NotificationVariant::Warning,
                                            3000,
                                        );
                                        state.mark_render_dirty();
                                    }
                                }
                                AgentStatus::Error => {
                                    state.set_notification(
                                        "Cannot send prompt to errored agent".to_string(),
                                        crate::state::types::NotificationVariant::Warning,
                                        3000,
                                    );
                                    state.mark_render_dirty();
                                }
                                AgentStatus::Hung => {
                                    state.set_notification(
                                        "Agent appears hung — try aborting first (ctrl+a)".to_string(),
                                        crate::state::types::NotificationVariant::Warning,
                                        5000,
                                    );
                                    state.mark_render_dirty();
                                }
                            }
                        }
                        return;
                    }
                }
            } else {
                // Handle non-configurable editing keys
                match (key.code, key.modifiers) {
                    // Arrow keys
                    (KeyCode::Up, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::Up);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Down, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::Down);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Left, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::Left);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Right, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::Right);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Home, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::Home);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::End, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.move_cursor(CursorDirection::End);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::PageUp, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.scroll_offset = ed.scroll_offset.saturating_sub(5);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::PageDown, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.scroll_offset =
                                (ed.scroll_offset + 5).min(ed.desc_lines.len().saturating_sub(1));
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    // Backspace
                    (KeyCode::Backspace, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.delete_char_back();
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    // Delete
                    (KeyCode::Delete, KeyModifiers::NONE) => {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.delete_char_forward();
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    // Printable characters
                    (KeyCode::Char(ch), modifiers)
                        if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.insert_char(ch);
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    _ => {
                        // Unrecognized key while editor is focused — consume it
                        return;
                    }
                }
            }
        }

        // When in detail view but editor is NOT focused, handle Tab to focus
        let is_detail_not_focused = {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                && !state.is_drilled_into_subagent()
                && state
                    .ui
                    .detail_editor
                    .as_ref()
                    .map_or(false, |e| !e.is_focused)
        };
        if is_detail_not_focused && key.code == KeyCode::Tab && key.modifiers.is_empty() {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            // Cycle focus: output scroll → changed files (if available) → prompt input
            let has_changed_files = state
                .ui
                .changed_files
                .as_ref()
                .map_or(false, |f| !f.is_empty());
            if has_changed_files && !state.ui.changed_files_focused {
                state.ui.changed_files_focused = true;
            } else {
                state.ui.changed_files_focused = false;
                if let Some(ed) = state.ui.detail_editor.as_mut() {
                    ed.is_focused = true;
                }
            }
            state.mark_render_dirty();
            return;
        }
    }

    // Handle changed-files sidebar focus: intercept keys when it's focused
    {
        let is_changed_files_focused = {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                && state.ui.changed_files_focused
                && !state
                    .ui
                    .detail_editor
                    .as_ref()
                    .map_or(false, |e| e.is_focused)
        };
        if is_changed_files_focused {
            use crossterm::event::KeyCode;
            match (key.code, key.modifiers) {
                (KeyCode::Up, KeyModifiers::NONE)
                | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.selected_changed_file_index > 0 {
                        state.ui.selected_changed_file_index -= 1;
                    }
                    state.mark_render_dirty();
                    return;
                }
                (KeyCode::Down, KeyModifiers::NONE)
                | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    let max = state
                        .ui
                        .changed_files
                        .as_ref()
                        .map_or(0, |f| f.len().saturating_sub(1));
                    if state.ui.selected_changed_file_index < max {
                        state.ui.selected_changed_file_index += 1;
                    }
                    state.mark_render_dirty();
                    return;
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    // Open diff review for the selected file
                    super::super::handlers::review::handle_open_file_diff(app);
                    return;
                }
                (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::Esc, KeyModifiers::NONE) => {
                    // Unfocus changed files → focus prompt input
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.ui.changed_files_focused = false;
                    if let Some(ed) = state.ui.detail_editor.as_mut() {
                        ed.is_focused = true;
                    }
                    state.mark_render_dirty();
                    return;
                }
                _ => {
                    // Consume other keys while focused on changed files
                    return;
                }
            }
        }
    }

    // ── Modal key interception ────────────────────────────────────
    {
        let modal_active = {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.permission_modal_active
        };
        if modal_active {
            // Clamp selected_index against current option count
            {
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                let max = super::super::permission::get_modal_option_count(&state);
                if max > 0 && state.ui.permission_modal_selected_index >= max {
                    state.ui.permission_modal_selected_index = max - 1;
                }
            }
            use crossterm::event::KeyCode;
            match key.code {
                // Arrow keys / vim keys for navigation
                KeyCode::Up | KeyCode::Char('k') => {
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.permission_modal_selected_index > 0 {
                        state.ui.permission_modal_selected_index -= 1;
                        state.mark_render_dirty();
                    }
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    let max_options = super::super::permission::get_modal_option_count(&state);
                    if state.ui.permission_modal_selected_index + 1 < max_options {
                        state.ui.permission_modal_selected_index += 1;
                        state.mark_render_dirty();
                    }
                    return;
                }
                // Enter — execute selected option
                KeyCode::Enter => {
                    super::super::permission::handle_modal_confirm(app);
                    return;
                }
                // Esc — close modal
                KeyCode::Esc => {
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.ui.permission_modal_active = false;
                    state.ui.permission_modal_selected_index = 0;
                    state.ui.permission_modal_dismissed_at = Some(std::time::Instant::now());
                    state.mark_render_dirty();
                    return;
                }
                // Quick shortcuts
                KeyCode::Char('y') => {
                    // Quick-approve (only for permissions)
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    if super::super::permission::has_pending_permission(&state) {
                        state.ui.permission_modal_selected_index = 0; // Yes
                        drop(state);
                        super::super::permission::handle_modal_confirm(app);
                    }
                    return;
                }
                KeyCode::Char('n') => {
                    // Quick-reject (only for permissions)
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    if super::super::permission::has_pending_permission(&state) {
                        state.ui.permission_modal_selected_index = 1; // No
                        drop(state);
                        super::super::permission::handle_modal_confirm(app);
                    }
                    return;
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    // Quick-select answer by number (only for questions)
                    let idx = (c as usize) - ('1' as usize);
                    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !super::super::permission::has_pending_permission(&state)
                        && idx < super::super::permission::get_modal_option_count(&state)
                    {
                        state.ui.permission_modal_selected_index = idx;
                        drop(state);
                        super::super::permission::handle_modal_confirm(app);
                    }
                    return;
                }
                _ => {
                    // Consume all other keys while modal is active
                    return;
                }
            }
        }
    }

    // Check if we're in task detail view — Escape pops subagent stack or closes detail
    {
        let is_detail_escape = {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                && key.code == KeyCode::Esc
        };
        // First lock dropped here
        if is_detail_escape {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            // If drilled into a subagent, pop back one level
            if state.is_drilled_into_subagent() {
                state.pop_subagent_drilldown();
                return;
            }
            // Otherwise, close the task detail view
            state.close_task_detail();
            return;
        }

        // Handle Up/Down arrows, j/k, and G/g for scrolling output in task detail view
        let is_scroll_key = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::Char('G')
                | KeyCode::Char('g')
        );
        let modifiers_ok = if matches!(key.code, KeyCode::Char('G')) {
            key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
        } else {
            key.modifiers.is_empty()
        };
        if is_scroll_key && modifiers_ok {
            let in_detail = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
            };
            if in_detail {
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                let total_lines = state
                    .ui
                    .viewing_task_id
                    .as_ref()
                    .and_then(|tid| state.session_tracker.cached_streaming_lines.get(tid))
                    .map(|(_, lines)| lines.len())
                    .unwrap_or(0);

                let max_offset = total_lines.saturating_sub(1);

                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if total_lines > 0 {
                            let current = state.ui.user_scroll_offset.unwrap_or(0);
                            let new_offset = current.saturating_sub(1);
                            state.ui.user_scroll_offset = Some(new_offset);
                            state.mark_render_dirty();
                        }
                        return;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if total_lines > 0 {
                            let current = state.ui.user_scroll_offset.unwrap_or(max_offset);
                            let new_offset = (current + 1).min(max_offset);
                            if new_offset >= max_offset {
                                state.ui.user_scroll_offset = None;
                            } else {
                                state.ui.user_scroll_offset = Some(new_offset);
                            }
                            state.mark_render_dirty();
                        }
                        return;
                    }
                    KeyCode::Char('G') => {
                        state.ui.user_scroll_offset = None;
                        state.mark_render_dirty();
                        return;
                    }
                    KeyCode::Char('g') => {
                        if total_lines > 0 {
                            state.ui.user_scroll_offset = Some(0);
                            state.mark_render_dirty();
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Handle y/n for permission approval when in task detail view
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('n')) {
            let approve = key.code == KeyCode::Char('y');
            // Batch read: get pending permission, task_id, and client in one lock.
            let (pending_perm, task_id, client): (
                Option<crate::state::types::PermissionRequest>,
                Option<String>,
                Option<OpenCodeClient>,
            ) = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                    (None, None, None)
                } else if let Some(ref tid) = state.ui.viewing_task_id {
                    let session =
                        super::super::permission::get_effective_session(&state);
                    let perm = session
                        .and_then(|s| s.pending_permissions.first().cloned());
                    let client = state
                        .project_registry
                        .active_project_id
                        .as_ref()
                        .and_then(|pid| app.opencode_clients.get(pid))
                        .cloned();
                    (perm, Some(tid.clone()), client)
                } else {
                    (None, None, None)
                }
            };

            if let (Some(perm), Some(tid)) = (pending_perm, task_id) {
                if let Some(client) = client {
                    super::super::permission::resolve_permission_async(
                        &app.state,
                        client,
                        perm.session_id.clone(),
                        perm.id.clone(),
                        tid,
                        approve,
                    );
                }
            } else {
                // In TaskDetail view with no pending permission — consume y/n
                // to prevent fallthrough to keybinding dispatch (e.g. n → CreateTask)
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                    return;
                }
            }
        }
    }

    // Handle 1-9 for answering questions when in task detail view
    if let KeyCode::Char(c) = key.code {
        if c.is_ascii_digit() && c != '0' {
            let answer_index = (c as usize) - ('1' as usize);
            // Batch read: get pending question, task_id, and client in one lock.
            let (pending_question, task_id, client): (
                Option<crate::state::types::QuestionRequest>,
                Option<String>,
                Option<OpenCodeClient>,
            ) = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                    (None, None, None)
                } else if let Some(ref tid) = state.ui.viewing_task_id {
                    let session =
                        super::super::permission::get_effective_session(&state);
                    let question = session
                        .and_then(|s| s.pending_questions.first().cloned())
                        .filter(|q| answer_index < q.answers.len());
                    let client = state
                        .project_registry
                        .active_project_id
                        .as_ref()
                        .and_then(|pid| app.opencode_clients.get(pid))
                        .cloned();
                    (question, Some(tid.clone()), client)
                } else {
                    (None, None, None)
                }
            };

            if let (Some(question), Some(tid)) = (pending_question, task_id) {
                let answer = question.answers[answer_index].clone();
                if let Some(client) = client {
                    super::super::utils::resolve_question_with_reassess(
                        app.state.clone(),
                        client,
                        question.id.clone(),
                        question.session_id.clone(),
                        answer,
                        tid,
                        app.config.columns.clone(),
                        app.config.opencode.clone(),
                    );
                }
                return;
            } else {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                    if let Some(ref tid) = state.ui.viewing_task_id {
                        if state
                            .session_tracker
                            .task_sessions
                            .get(tid)
                            .map(|s| !s.pending_questions.is_empty())
                            .unwrap_or(false)
                        {
                            return;
                        }
                    }
                }
            }
        }
    }

    // Handle vim-style keys that bypass the configurable keybinding system
    match (key.code, key.modifiers) {
        // Ctrl+R — reset circuit breaker for active project
        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let pid = state.project_registry.active_project_id.clone();
            if let Some(ref pid) = pid {
                let was_tripped = state.project_registry.is_circuit_breaker_tripped(
                    pid,
                    app.config.opencode.circuit_breaker_threshold,
                );
                state.project_registry.reset_circuit_breaker(pid);
                if was_tripped {
                    state.set_notification(
                        "Circuit breaker reset — auto-progression resumed.".to_string(),
                        crate::state::types::NotificationVariant::Success,
                        3000,
                    );
                } else {
                    state.set_notification(
                        "Circuit breaker was not tripped.".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        2000,
                    );
                }
            }
            return;
        }
        _ => {}
    }

    let action = app.key_matcher.match_key(key);

    match action {
        Some(Action::Quit) => super::super::handlers::project::handle_quit(app),
        Some(Action::HelpToggle) => super::super::handlers::project::handle_help_toggle(app),
        Some(Action::PrevProject) => super::super::handlers::project::handle_prev_project(app),
        Some(Action::NextProject) => super::super::handlers::project::handle_next_project(app),
        Some(Action::NewProject) => super::super::handlers::project::handle_new_project(app),
        Some(Action::RenameProject) => super::super::handlers::project::handle_rename_project(app),
        Some(Action::SetWorkingDirectory) => {
            super::super::handlers::project::handle_set_working_directory(app)
        }
        Some(Action::DeleteProject) => super::super::handlers::project::handle_delete_project(app),
        Some(Action::NavLeft) => super::super::handlers::kanban::handle_nav_column(app, -1),
        Some(Action::NavRight) => super::super::handlers::kanban::handle_nav_column(app, 1),
        Some(Action::NavUp) => super::super::handlers::kanban::handle_nav_task(app, -1),
        Some(Action::NavDown) => super::super::handlers::kanban::handle_nav_task(app, 1),
        Some(Action::CreateTask) => super::super::handlers::task::handle_create_task(app),
        Some(Action::OpenTaskDetail) => super::super::handlers::task::handle_open_task_detail(app),
        Some(Action::MoveForward) => super::super::handlers::task::handle_move_task(app, 1),
        Some(Action::MoveBackward) => super::super::handlers::task::handle_move_task(app, -1),
        Some(Action::DeleteTask) => super::super::handlers::task::handle_delete_task(app),
        Some(Action::AbortSession) => super::super::handlers::task::handle_abort_session(app),
        Some(Action::RetryTask) => super::super::handlers::task::handle_retry_task(app),
        Some(Action::DrillDownSubagent) => {
            super::super::handlers::subagent::handle_drill_down_subagent(app)
        }
        Some(Action::ReviewChanges) => super::super::handlers::review::handle_review_changes(app),
        Some(Action::AcceptReview) => super::super::handlers::review::handle_accept_review(app),
        Some(Action::RejectReview) => super::super::handlers::review::handle_reject_review(app),
        Some(Action::Reports) => super::super::modes::reports::handle_reports_toggle(app),
        Some(Action::AddDependency) => {
            handle_add_dependency(app);
        }
        Some(Action::RemoveDependency) => {
            handle_remove_dependency(app);
        }
        Some(Action::ViewArchive) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.archive_state = Some(crate::state::types::ArchiveState::default());
            state.ui.mode = crate::state::types::AppMode::Archive;
            state.mark_render_dirty();
        }
        Some(Action::OpenConfigEditor) => {
            super::config_editor::open_config_editor(app);
        }
        None => {} // Unmatched key, ignore
    }
}

/// Handle add-dependency keybinding.
/// Enters InputPrompt mode asking for a task number to depend on.
fn handle_add_dependency(app: &mut App) {
    let task_id = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.focused_task_id.clone()
    };
    match task_id {
        Some(_tid) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.input_text = String::new();
            state.ui.input_cursor = 0;
            state.ui.prompt_label = "Depends on task #: ".to_string();
            state.ui.prompt_context = Some("add_dependency".to_string());
            // Store the task ID in input_text context so we know which task to add dep to
            state.ui.mode = crate::state::types::AppMode::InputPrompt;
            state.mark_render_dirty();
        }
        None => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No task selected".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
    }
}

/// Handle remove-dependency keybinding.
/// If the focused task has dependencies, removes the last one added.
fn handle_remove_dependency(app: &mut App) {
    let (task_id, last_dep) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        let tid = state.ui.focused_task_id.clone();
        let dep = tid.as_ref().and_then(|id| {
            state.tasks.get(id).and_then(|t| t.blocked_by.last().cloned())
        });
        (tid, dep)
    };
    match (task_id, last_dep) {
        (Some(tid), Some(dep_id)) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.remove_dependency(&tid, &dep_id);
            state.set_notification(
                "Removed dependency on task (by ID)".to_string(),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            state.mark_render_dirty();
        }
        (Some(_), None) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No dependencies to remove".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
        (None, _) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No task selected".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
    }
}
