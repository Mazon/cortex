//! Confirm mode key handler — handles y/n confirmation for destructive actions.

use super::super::App;

/// Handle key events in ConfirmDelete mode.
///
/// - `y` or `Enter` → execute the pending deletion
/// - `n` or `Esc` or `q` → cancel and return to Normal mode
/// - Any other key → ignored
pub fn handle_confirm_delete_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            // Confirm deletion
            super::super::handlers::task::execute_delete_task(app);
        }
        KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
            // Cancel — return to Normal mode
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.pending_delete_task_id = None;
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.mark_render_dirty();
        }
        _ => {
            // Ignore all other keys
        }
    }
}
