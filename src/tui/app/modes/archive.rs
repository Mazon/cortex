//! Archive viewer mode — view and manage archived tasks.

use super::super::App;

/// Handle key events in Archive mode.
///
/// - Up/Down or j/k: navigate the archived task list
/// - Enter: view task detail for the selected archived task
/// - u: unarchive the selected task
/// - Esc or q: return to Normal mode
pub fn handle_archive_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.mark_render_dirty();
        }
        (KeyCode::Up, KeyModifiers::NONE)
        | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let archived_ids = state.get_archived_task_ids(
                state.project_registry.active_project_id.as_deref().unwrap_or(""),
            );
            if let Some(ref mut s) = state.ui.archive_state {
                if s.selected_index > 0 {
                    s.selected_index -= 1;
                }
                s.selected_index = s.selected_index.min(archived_ids.len().saturating_sub(1));
            }
            state.mark_render_dirty();
        }
        (KeyCode::Down, KeyModifiers::NONE)
        | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            let archived_ids = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.get_archived_task_ids(
                    state.project_registry.active_project_id.as_deref().unwrap_or(""),
                )
            };
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = state.ui.archive_state {
                s.selected_index = s.selected_index.saturating_add(1);
                s.selected_index = s.selected_index.min(archived_ids.len().saturating_sub(1));
            }
            state.mark_render_dirty();
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            // View task detail for the selected archived task
            let archived_id = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                let archived_ids = state.get_archived_task_ids(
                    state.project_registry.active_project_id.as_deref().unwrap_or(""),
                );
                state.ui.archive_state.as_ref().and_then(|s| {
                    archived_ids.get(s.selected_index).cloned()
                })
            };
            if let Some(task_id) = archived_id {
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                state.open_task_detail(&task_id);
                state.ui.diff_review_source = Some(crate::state::types::FocusedPanel::Kanban);
                state.mark_render_dirty();
            }
        }
        (KeyCode::Char('u'), KeyModifiers::NONE) => {
            // Unarchive the selected task
            let archived_id = {
                let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                let archived_ids = state.get_archived_task_ids(
                    state.project_registry.active_project_id.as_deref().unwrap_or(""),
                );
                state.ui.archive_state.as_ref().and_then(|s| {
                    archived_ids.get(s.selected_index).cloned()
                })
            };
            if let Some(task_id) = archived_id {
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.unarchive_task(&task_id) {
                    state.set_notification(
                        "Task unarchived".to_string(),
                        crate::state::types::NotificationVariant::Success,
                        3000,
                    );
                }
                state.mark_render_dirty();
            }
        }
        _ => {
            // Ignore other keys
        }
    }
}
