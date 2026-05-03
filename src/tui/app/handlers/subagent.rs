//! Subagent drill-down handlers — drill into subagent sessions, find drillable agents.

use super::super::App;

/// Handle drill-down into a subagent session (ctrl+x).
///
/// When in the task detail view, looks for `TaskMessagePart::Agent` parts
/// in the currently viewed session's messages. If a subagent is found,
/// fetches its messages (lazy-load) and pushes onto the navigation stack.
pub fn handle_drill_down_subagent(app: &mut App) {
    // Must be in task detail view
    {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
            return;
        }
    }

    // Find the first navigable Agent part in the current view.
    // We extract the needed data while holding the lock, then drop it.
    let found = find_drillable_subagent(&app.state);

    let (session_id, agent, task_id, depth) = match found {
        Some(f) => f,
        None => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No subagent to drill into".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
            return;
        }
    };

    // Fetch subagent messages lazily
    let client = get_active_client(app);
    let state = app.state.clone();

    tokio::spawn(async move {
        // Check if we already have cached data
        let needs_fetch = {
            let s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.session_tracker
                .subagent_session_data
                .get(&session_id)
                .map(|d| d.messages.is_empty())
                .unwrap_or(true)
        };

        if needs_fetch {
            if let Some(client) = client {
                match client.fetch_subagent_messages(&session_id).await {
                    Ok(messages) => {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        let entry = s
                            .session_tracker
                            .subagent_session_data
                            .entry(session_id.clone())
                            .or_insert_with(crate::state::types::TaskDetailSession::default);
                        entry.session_id = Some(session_id.clone());
                        entry.task_id = task_id.clone();
                        entry.streaming_text = None; // Clear to avoid double-rendering with messages
                        entry.messages = messages;
                        entry.render_version += 1;
                    }
                    Err(e) => {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.set_notification(
                            format!("Failed to load subagent: {}", e),
                            crate::state::types::NotificationVariant::Error,
                            3000,
                        );
                        return;
                    }
                }
            } else {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    "No OpenCode client available".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    3000,
                );
                return;
            }
        }

        // Push onto navigation stack
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        // Guard against duplicate push from rapid key presses
        let already_on_stack = s
            .ui
            .session_nav_stack
            .iter()
            .any(|r| r.session_id == session_id);
        if already_on_stack {
            return; // Already pushed by a prior keypress
        }
        // For nested drill-downs, use only the agent name to avoid
        // repeating the task label (e.g., "Task #3 > planning > do"
        // instead of "Task #3 > planning > Task #3 > do").
        let label = if s.is_drilled_into_subagent() {
            agent.clone()
        } else {
            let task_label = s
                .tasks
                .get(&task_id)
                .map(|t| format!("Task #{}", t.number))
                .unwrap_or_else(|| task_id.clone());
            format!("{} > {}", task_label, agent)
        };
        let session_ref = crate::state::types::SessionRef {
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            label,
            depth,
        };
        s.push_subagent_drilldown(session_ref);
    });
}

/// Scan the current view for a drillable subagent `Agent` part.
///
/// Returns `Some((session_id, agent_name, parent_task_id, depth))` if a
/// navigable subagent is found, or `None` otherwise.
pub fn find_drillable_subagent(
    state: &std::sync::Arc<std::sync::Mutex<crate::state::types::AppState>>,
) -> Option<(String, String, String, u32)> {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());

    let session_id_to_scan = state.get_drilldown_session_id().map(|s| s.to_string());

    if let Some(scan_id) = session_id_to_scan {
        // Scanning subagent session data
        if let Some(session_data) = state.session_tracker.subagent_session_data.get(&scan_id) {
            let task_id = state.ui.viewing_task_id.clone().unwrap_or_default();
            let current_depth = state
                .ui
                .session_nav_stack
                .last()
                .map(|r| r.depth)
                .unwrap_or(0);
            for msg in &session_data.messages {
                for part in &msg.parts {
                    if let crate::state::types::TaskMessagePart::Agent { id, agent } = part {
                        let already_in_stack = state
                            .ui
                            .session_nav_stack
                            .iter()
                            .any(|r| r.session_id == *id);
                        if !already_in_stack {
                            return Some((
                                id.clone(),
                                agent.clone(),
                                task_id,
                                current_depth + 1,
                            ));
                        }
                    }
                }
            }
        }
    } else {
        // Scanning parent task's messages
        if let Some(ref tid) = state.ui.viewing_task_id {
            if let Some(session) = state.session_tracker.task_sessions.get(tid) {
                let task_id = tid.clone();
                for msg in &session.messages {
                    for part in &msg.parts {
                        if let crate::state::types::TaskMessagePart::Agent { id, agent } = part {
                            let already_in_stack = state
                                .ui
                                .session_nav_stack
                                .iter()
                                .any(|r| r.session_id == *id);
                            if !already_in_stack {
                                return Some((id.clone(), agent.clone(), task_id, 1));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Get the OpenCode client for the active project, or `None` if unavailable.
fn get_active_client(app: &App) -> Option<crate::opencode::client::OpenCodeClient> {
    let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state
        .project_registry
        .active_project_id
        .as_ref()
        .and_then(|pid| app.opencode_clients.get(pid))
        .cloned()
}
