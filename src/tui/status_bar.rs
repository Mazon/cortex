//! Status bar renderer — bottom bar showing connection status, project info,
//! notifications, attention indicators, and contextual key hints.
//!
//! Hints are context-sensitive: only the keys relevant to the current UI state
//! are shown. When multiple contexts apply (e.g. a running task with pending
//! permissions), hints rotate on a ~3-second cycle so the user can discover
//! all applicable shortcuts. `?:help` is always shown as a fallback.

use crate::state::types::{
    AgentStatus, AppMode, AppState, FocusedPanel, NotificationVariant, MAX_NOTIFICATIONS,
};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds between hint rotations when multiple context groups apply.
const HINT_ROTATION_SECS: u64 = 3;

/// Render the status bar at the bottom of the kanban area.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    // Count pending permissions and questions across all tasks for the active project
    let (total_permissions, total_questions) = state
        .tasks
        .values()
        .filter(|t| {
            state
                .active_project_id
                .as_ref()
                .map_or(false, |pid| t.project_id == *pid)
        })
        .fold((0u32, 0u32), |(perm, quest), t| {
            (
                perm + t.pending_permission_count,
                quest + t.pending_question_count,
            )
        });
    let has_attention_items = total_permissions > 0 || total_questions > 0;

    // Build attention indicator text (shown prominently when there are pending items)
    let attention_text = if total_permissions > 0 && total_questions > 0 {
        format!(
            "\u{26A0} {} perm{}, {} quest{} \u{2014} press v",
            total_permissions,
            if total_permissions == 1 { "" } else { "s" },
            total_questions,
            if total_questions == 1 { "" } else { "s" },
        )
    } else if total_permissions > 0 {
        format!(
            "\u{26A0} {} permission{} pending \u{2014} press v",
            total_permissions,
            if total_permissions == 1 { "" } else { "s" },
        )
    } else if total_questions > 0 {
        format!(
            "\u{26A0} {} question{} pending \u{2014} press v",
            total_questions,
            if total_questions == 1 { "" } else { "s" },
        )
    } else {
        String::new()
    };

    // Connection status (left)
    let (conn_text, conn_color) = if state.permanently_disconnected {
        ("✕ disconnected (max retries exceeded — restart to retry)".to_string(), Color::Red)
    } else if state.reconnecting {
        let attempt = state.reconnect_attempt;
        if attempt > 0 {
            (format!("◐ reconnecting ({})...", attempt), Color::Yellow)
        } else {
            ("◐ reconnecting...".to_string(), Color::Yellow)
        }
    } else if state.connected {
        ("● connected".to_string(), Color::Green)
    } else {
        ("○ disconnected".to_string(), Color::DarkGray)
    };

    // Active project name + task count (displayed between connection status and notifications)
    let project_info = state
        .active_project_id
        .as_ref()
        .and_then(|pid| {
            state.projects.iter().find(|p| &p.id == pid).map(|p| {
                let task_count = state
                    .tasks
                    .values()
                    .filter(|t| t.project_id == *pid)
                    .count();
                let label = if task_count == 1 {
                    "1 task".to_string()
                } else {
                    format!("{} tasks", task_count)
                };
                format!(" │ {} ({})", p.name, label)
            })
        })
        .unwrap_or_default();

    // Notification (center) — show most recent with queue count indicator
    let (notif_text, notif_color) = if let Some(n) = state.ui.notifications.back() {
        let color = match n.variant {
            NotificationVariant::Info => Color::Blue,
            NotificationVariant::Success => Color::Green,
            NotificationVariant::Warning => Color::Yellow,
            NotificationVariant::Error => Color::Red,
        };
        let count = state.ui.notifications.len();
        if count > 1 {
            let display = format!("({}/{}) {}", count, MAX_NOTIFICATIONS, n.message);
            (display, color)
        } else {
            (n.message.clone(), color)
        }
    } else {
        (String::new(), Color::Reset)
    };

    // Build context-sensitive hint groups based on current state.
    // When multiple groups apply, they rotate on a ~3-second cycle.
    let hint_groups = build_contextual_hints(state);
    let rotation_index = current_rotation_index(hint_groups.len());
    let contextual = hint_groups
        .get(rotation_index)
        .map(|s| s.as_str())
        .unwrap_or("");

    // Always append "?:help" to the contextual hint
    let hints_full = if contextual.is_empty() {
        "?:help".to_string()
    } else {
        format!("{}  ?:help", contextual)
    };

    // Tiered fallbacks — progressively shorter versions
    let hints_medium = "?:help".to_string();
    let hints_short = "?:help".to_string();
    let hints_minimal = "?:help".to_string();

    // Build the status bar using a horizontal layout
    let total_width = area.width as usize;

    // Connection status width is dynamic: "● connected" (13) to "◐ reconnecting (99)..." (23)
    let conn_width = conn_text.chars().count().max(14) as u16;

    // Project info width (hide on narrow terminals)
    let proj_len = project_info.chars().count();
    let show_project = !project_info.is_empty() && total_width >= 70;
    let proj_width = if show_project { proj_len as u16 } else { 0 };

    // Attention indicator takes precedence over notification in the center area
    let has_center_text = has_attention_items || !notif_text.is_empty();

    // Available space for center text + hints
    let remaining = total_width
        .saturating_sub(conn_width as usize)
        .saturating_sub(proj_width as usize);

    // Choose the appropriate hint tier based on available space.
    let hints = if has_center_text {
        let hint_budget = remaining.saturating_sub(20);
        if hint_budget >= hints_full.chars().count() {
            hints_full.as_str()
        } else if hint_budget >= hints_medium.chars().count() {
            hints_medium.as_str()
        } else if hint_budget >= hints_short.chars().count() {
            hints_short.as_str()
        } else {
            hints_minimal.as_str()
        }
    } else {
        if remaining >= hints_full.chars().count() {
            hints_full.as_str()
        } else if remaining >= hints_medium.chars().count() {
            hints_medium.as_str()
        } else if remaining >= hints_short.chars().count() {
            hints_short.as_str()
        } else if total_width >= 60 {
            hints_minimal.as_str()
        } else {
            ""
        }
    };

    let hints_width = hints.chars().count() as u16;

    // Layout: connection (fixed) | project (fixed, conditional) | notification (flex) | hints (fixed)
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(conn_width), // Connection status (left, fixed)
    ];
    if show_project {
        constraints.push(Constraint::Length(proj_width)); // Project name + task count
    }
    constraints.push(Constraint::Min(0)); // Attention / Notification (center, flexible)
    constraints.push(Constraint::Length(hints_width)); // Key hints (right)

    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    let mut slot = 0;

    // Left: connection status
    let left = Paragraph::new(Span::styled(conn_text, Style::default().fg(conn_color)));
    f.render_widget(left, h_layout[slot]);
    slot += 1;

    // Project info
    if show_project {
        let proj_widget =
            Paragraph::new(Span::styled(project_info, Style::default().fg(Color::Cyan)));
        f.render_widget(proj_widget, h_layout[slot]);
        slot += 1;
    }

    // Center: attention indicator (takes precedence) or notification
    if has_attention_items {
        let inner = h_layout[slot].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        let center = Paragraph::new(Span::styled(
            attention_text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        f.render_widget(center, inner);
    } else if !notif_text.is_empty() {
        let inner = h_layout[slot].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        let center = Paragraph::new(Span::styled(notif_text, Style::default().fg(notif_color)));
        f.render_widget(center, inner);
    }
    slot += 1;

    // Right: key hints
    if !hints.is_empty() {
        let right = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
        f.render_widget(right, h_layout[slot]);
    }
}

/// Build context-sensitive hint groups based on the current application state.
///
/// Returns a list of hint strings. When multiple groups apply, the status bar
/// rotates through them so the user can discover all relevant shortcuts.
fn build_contextual_hints(state: &AppState) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();

    match state.ui.mode {
        AppMode::TaskEditor => {
            groups.push("Tab: next field  Ctrl+S: save  Esc: cancel".to_string());
            // If there's a validation error, hint about it
            if state
                .ui
                .task_editor
                .as_ref()
                .map_or(false, |e| e.validation_error.is_some())
            {
                groups.push("fix error above or Esc: cancel".to_string());
            }
            // If there are unsaved changes with the discard warning shown
            if state
                .ui
                .task_editor
                .as_ref()
                .map_or(false, |e| e.discard_warning_shown)
            {
                groups.push("Esc: discard changes  Ctrl+S: save".to_string());
            }
        }
        AppMode::Help => {
            groups.push("Esc: close help".to_string());
        }
        AppMode::ConfirmDialog => {
            groups.push("y: confirm  n/Esc: cancel".to_string());
        }
        AppMode::InputPrompt | AppMode::ProjectRename => {
            groups.push("Enter: submit  Esc: cancel".to_string());
        }
        AppMode::Normal => {
            match state.ui.focused_panel {
                FocusedPanel::TaskDetail => {
                    // Task detail view hints
                    groups.push("↑/↓: scroll  Esc: back".to_string());

                    // If viewing a task with pending permissions
                    if let Some(ref tid) = state.ui.viewing_task_id {
                        if state
                            .task_sessions
                            .get(tid)
                            .map_or(false, |s| !s.pending_permissions.is_empty())
                        {
                            groups.push("y: approve  n: reject  Esc: back".to_string());
                        }
                        // If viewing a task with pending questions
                        if state
                            .task_sessions
                            .get(tid)
                            .map_or(false, |s| !s.pending_questions.is_empty())
                        {
                            groups.push("1-9: answer question  Esc: back".to_string());
                        }
                    }
                }
                FocusedPanel::Kanban => {
                    // Check if focused column is empty
                    let column_tasks = state
                        .kanban
                        .columns
                        .get(&state.ui.focused_column)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    let is_empty = column_tasks.is_empty();

                    if is_empty {
                        // Empty column: just show the create shortcut
                        groups.push("n: new task".to_string());
                    } else if let Some(ref task_id) = state.ui.focused_task_id {
                        // A task is selected — build hints based on task state
                        if let Some(task) = state.tasks.get(task_id) {
                            // Check for pending permissions on this task
                            let has_pending = task.pending_permission_count > 0
                                || task.pending_question_count > 0;

                            if task.agent_status == AgentStatus::Running {
                                // Running task: abort is the most relevant action
                                groups.push("Ctrl+A A: abort  v: view".to_string());
                                if has_pending {
                                    groups.push("v: view (pending approval)".to_string());
                                }
                            } else if has_pending {
                                // Task with pending permissions/questions
                                groups.push("v: view (pending approval)".to_string());
                            } else {
                                // Normal task: show full action set
                                groups.push("v: view  e: edit  m: move  x: delete".to_string());
                            }

                            // Additional contextual hints for non-running tasks
                            if task.agent_status != AgentStatus::Running {
                                groups.push("n: new  ^j/^k: proj  ^q: quit".to_string());
                            }
                        }
                    } else {
                        // Column has tasks but none selected (shouldn't normally happen)
                        groups.push("j/k: select  n: new task".to_string());
                    }
                }
            }
        }
    }

    groups
}

/// Returns a rotation index based on the current wall-clock time.
///
/// The index cycles through `0..count` every `HINT_ROTATION_SECS` seconds,
/// so when multiple context groups apply they rotate automatically.
fn current_rotation_index(count: usize) -> usize {
    if count <= 1 {
        return 0;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / HINT_ROTATION_SECS) as usize % count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::types::{KanbanColumn, KanbanState, UIState};
    use std::collections::HashMap;

    fn base_state() -> AppState {
        let mut state = AppState::default();
        state.ui = UIState::default();
        state.kanban = KanbanState::default();
        state.connected = true;
        state
    }

    fn make_task(
        id: &str,
        status: AgentStatus,
        perm_count: u32,
        question_count: u32,
    ) -> crate::state::types::CortexTask {
        crate::state::types::CortexTask {
            id: id.to_string(),
            number: 1,
            title: "Test".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: if status == AgentStatus::Running {
                Some("s1".to_string())
            } else {
                None
            },
            agent_type: if status == AgentStatus::Running {
                Some("do".to_string())
            } else {
                None
            },
            agent_status: status,
            entered_column_at: 0,
            last_activity_at: 0,
            error_message: None,
            plan_output: None,
            pending_permission_count: perm_count,
            pending_question_count: question_count,
            created_at: 0,
            updated_at: 0,
            project_id: "proj-1".to_string(),
        }
    }

    #[test]
    fn empty_column_shows_new_task_hint() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.kanban.columns.insert("todo".to_string(), vec![]);
        state.ui.focused_task_id = None;

        let hints = build_contextual_hints(&state);
        assert_eq!(hints, vec!["n: new task"]);
    }

    #[test]
    fn task_selected_shows_action_hints() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.ui.focused_task_id = Some("task-1".to_string());
        state
            .kanban
            .columns
            .insert("todo".to_string(), vec!["task-1".to_string()]);
        state.tasks.insert(
            "task-1".to_string(),
            make_task("task-1", AgentStatus::Pending, 0, 0),
        );

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"v: view  e: edit  m: move  x: delete".to_string()));
        assert!(hints.contains(&"n: new  ^j/^k: proj  ^q: quit".to_string()));
    }

    #[test]
    fn running_task_shows_abort_hint() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.ui.focused_task_id = Some("task-1".to_string());
        state
            .kanban
            .columns
            .insert("todo".to_string(), vec!["task-1".to_string()]);
        state.tasks.insert(
            "task-1".to_string(),
            make_task("task-1", AgentStatus::Running, 0, 0),
        );

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"Ctrl+A A: abort  v: view".to_string()));
    }

    #[test]
    fn running_task_with_pending_shows_approval_hint() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.ui.focused_task_id = Some("task-1".to_string());
        state
            .kanban
            .columns
            .insert("todo".to_string(), vec!["task-1".to_string()]);
        state.tasks.insert(
            "task-1".to_string(),
            make_task("task-1", AgentStatus::Running, 1, 0),
        );

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"Ctrl+A A: abort  v: view".to_string()));
        assert!(hints.contains(&"v: view (pending approval)".to_string()));
    }

    #[test]
    fn task_with_pending_permissions_shows_approval_hint() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.ui.focused_task_id = Some("task-1".to_string());
        state
            .kanban
            .columns
            .insert("todo".to_string(), vec!["task-1".to_string()]);
        state.tasks.insert(
            "task-1".to_string(),
            make_task("task-1", AgentStatus::Pending, 2, 0),
        );

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"v: view (pending approval)".to_string()));
    }

    #[test]
    fn task_detail_view_shows_scroll_and_back() {
        let mut state = base_state();
        state.ui.focused_panel = FocusedPanel::TaskDetail;
        state.ui.viewing_task_id = Some("task-1".to_string());

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"↑/↓: scroll  Esc: back".to_string()));
    }

    #[test]
    fn task_editor_shows_field_and_save_hints() {
        let mut state = base_state();
        state.ui.mode = AppMode::TaskEditor;

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"Tab: next field  Ctrl+S: save  Esc: cancel".to_string()));
    }

    #[test]
    fn help_overlay_shows_close_hint() {
        let mut state = base_state();
        state.ui.mode = AppMode::Help;

        let hints = build_contextual_hints(&state);
        assert_eq!(hints, vec!["Esc: close help"]);
    }

    #[test]
    fn confirm_dialog_shows_confirm_cancel() {
        let mut state = base_state();
        state.ui.mode = AppMode::ConfirmDialog;

        let hints = build_contextual_hints(&state);
        assert_eq!(hints, vec!["y: confirm  n/Esc: cancel"]);
    }

    #[test]
    fn input_prompt_shows_submit_cancel() {
        let mut state = base_state();
        state.ui.mode = AppMode::InputPrompt;

        let hints = build_contextual_hints(&state);
        assert_eq!(hints, vec!["Enter: submit  Esc: cancel"]);
    }

    #[test]
    fn rotation_index_cycles() {
        let idx = current_rotation_index(3);
        assert!(idx < 3);

        let idx = current_rotation_index(1);
        assert_eq!(idx, 0);

        let idx = current_rotation_index(0);
        assert_eq!(idx, 0);
    }

    #[test]
    fn pending_permissions_in_task_detail() {
        use crate::state::types::{PermissionRequest, TaskDetailSession};

        let mut state = base_state();
        state.ui.focused_panel = FocusedPanel::TaskDetail;
        state.ui.viewing_task_id = Some("task-1".to_string());

        let session = TaskDetailSession {
            task_id: "task-1".to_string(),
            session_id: Some("s1".to_string()),
            messages: vec![],
            streaming_text: None,
            pending_permissions: vec![PermissionRequest {
                id: "p1".to_string(),
                session_id: "s1".to_string(),
                tool_name: "bash".to_string(),
                description: "run tests".to_string(),
                status: "pending".to_string(),
                details: None,
            }],
            pending_questions: vec![],
            render_version: 0,
            seen_delta_keys: std::collections::HashSet::new(),
            last_delta_key: None,
        };
        state.task_sessions.insert("task-1".to_string(), session);

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"y: approve  n: reject  Esc: back".to_string()));
    }

    #[test]
    fn pending_questions_in_task_detail() {
        use crate::state::types::{QuestionRequest, TaskDetailSession};

        let mut state = base_state();
        state.ui.focused_panel = FocusedPanel::TaskDetail;
        state.ui.viewing_task_id = Some("task-1".to_string());

        let session = TaskDetailSession {
            task_id: "task-1".to_string(),
            session_id: Some("s1".to_string()),
            messages: vec![],
            streaming_text: None,
            pending_permissions: vec![],
            pending_questions: vec![QuestionRequest {
                id: "q1".to_string(),
                session_id: "s1".to_string(),
                question: "Which approach?".to_string(),
                answers: vec!["Option A".to_string(), "Option B".to_string()],
                status: "pending".to_string(),
            }],
            render_version: 0,
            seen_delta_keys: std::collections::HashSet::new(),
            last_delta_key: None,
        };
        state.task_sessions.insert("task-1".to_string(), session);

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"1-9: answer question  Esc: back".to_string()));
    }

    #[test]
    fn no_column_tasks_no_selection_shows_select_hint() {
        let mut state = base_state();
        state.ui.focused_column = "todo".to_string();
        state.ui.focused_task_id = None;
        // Column has tasks but none focused
        state
            .kanban
            .columns
            .insert("todo".to_string(), vec!["task-1".to_string()]);

        let hints = build_contextual_hints(&state);
        assert!(hints.contains(&"j/k: select  n: new task".to_string()));
    }
}
