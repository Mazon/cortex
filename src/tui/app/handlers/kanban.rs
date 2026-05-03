//! Kanban column/task navigation handlers.

use super::super::App;
use super::super::mouse;

/// Move the focused column left or right by `direction` (-1 or +1).
/// Auto-scrolls the kanban view to keep the focused column visible.
pub fn handle_nav_column(app: &mut App, direction: i32) {
    let visible = app.config.columns.visible_column_ids();
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    let new_idx = state.kanban.focused_column_index as i32 + direction;
    if new_idx >= 0 && (new_idx as usize) < visible.len() {
        state.kanban.focused_column_index = new_idx as usize;
        if let Some(col_id) = visible.get(state.kanban.focused_column_index) {
            state.set_focused_column(col_id);
        }
        // Auto-scroll to keep the focused column visible.
        mouse::ensure_column_visible(&mut state, &app.config, &app.terminal);
    }
}

/// Move the focused task up or down by `direction` (-1 or +1).
pub fn handle_nav_task(app: &mut App, direction: i32) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    let col_id = state.ui.focused_column.clone();
    let task_count = state
        .kanban
        .columns
        .get(&col_id)
        .map(|v| v.len())
        .unwrap_or(0);
    let current = state
        .kanban
        .focused_task_index
        .get(&col_id)
        .copied()
        .unwrap_or(0);
    let new_idx = current as i32 + direction;
    if new_idx >= 0 && (new_idx as usize) < task_count {
        state
            .kanban
            .focused_task_index
            .insert(col_id.clone(), new_idx as usize);
        mouse::update_focused_task_id(&mut state, &col_id);
    }
}
