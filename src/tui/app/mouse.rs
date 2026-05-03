//! Mouse event handling — click to focus tasks/columns, scroll to navigate.

use super::App;
use crate::config::types::CortexConfig;
use crate::state::types::AppState;
use crate::tui::Terminal;
use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::prelude::Size;


/// Handle a mouse event — left-click to focus tasks and columns.
///
/// Supports:
/// - Click on a kanban column header → focus that column
/// - Click on a task card → focus that task
/// - Scroll wheel → navigate tasks up/down within the focused column
pub fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    // We only handle left-button press (not release, drag, etc.)
    let MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse.kind else {
        // Handle scroll wheel for task navigation
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                super::handlers::kanban::handle_nav_task(app, -1);
                return;
            }
            MouseEventKind::ScrollDown => {
                super::handlers::kanban::handle_nav_task(app, 1);
                return;
            }
            _ => return,
        }
    };

    let area = match app.terminal.size() {
        Ok(size) => size,
        Err(_) => return,
    };

    let sidebar_width = app.config.theme.sidebar_width;
    let col_width = app.config.theme.column_width;

    // Ignore clicks in the sidebar area
    if mouse.column < sidebar_width {
        return;
    }

    // Ignore clicks in the status bar (last 2 rows: top border + content)
    if mouse.row >= area.height.saturating_sub(2) {
        return;
    }

    let kanban_x = mouse.column - sidebar_width;
    let visible = app.config.columns.visible_column_ids();

    // Account for scroll indicators
    let available_for_columns = area.width.saturating_sub(sidebar_width).saturating_sub(6);
    let max_visible = std::cmp::max(1, (available_for_columns / col_width) as usize);
    let can_show_all = visible.len() <= max_visible;

    let has_left_indicator = !can_show_all
        && {
            let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.kanban.kanban_scroll_offset > 0
        };

    let scroll_offset = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        if can_show_all {
            0
        } else {
            state
                .kanban
                .kanban_scroll_offset
                .min(visible.len().saturating_sub(max_visible))
        }
    };

    let x_offset: u16 = if has_left_indicator { 3 } else { 0 };

    // Determine which column was clicked
    let col_index = if kanban_x >= x_offset {
        ((kanban_x - x_offset) / col_width) as usize
    } else {
        return;
    };

    if col_index >= max_visible || col_index + scroll_offset >= visible.len() {
        return;
    }

    let clicked_col_id = &visible[col_index + scroll_offset];

    // Determine if the click was on the column header (row 0 or 1)
    // or on a task card (row >= 2)
    let is_header_click = mouse.row < 2;

    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());

    // Always focus the clicked column
    let col_idx = col_index + scroll_offset;
    if let Some(col_id) = visible.get(col_idx) {
        state.kanban.focused_column_index = col_idx;
        state.set_focused_column(col_id);
    }
    ensure_column_visible(&mut state, &app.config, &app.terminal);

    if is_header_click {
        // Click on column header — just focus the column (already done above)
        state.mark_render_dirty();
    } else {
        // Click in the task area — determine which task was clicked
        // Tasks start at row 2 (after the header), each task card is 6 rows
        // (5 rows for card + 1 row gap)
        let task_row = (mouse.row - 2) as usize;
        let card_height = 6usize; // 5 rows for card + 1 row gap
        let task_index = task_row / card_height;

        let task_id = {
            let task_ids = state.kanban.columns.get(clicked_col_id.as_str());
            if let Some(task_ids) = task_ids {
                if task_index < task_ids.len() {
                    task_ids.get(task_index).cloned()
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(task_id) = task_id {
            state
                .kanban
                .focused_task_index
                .insert(clicked_col_id.clone(), task_index);
            state.ui.focused_task_id = Some(task_id);
        }
        state.mark_render_dirty();
    }
}

// ── Horizontal scroll helpers ──

/// Calculate the maximum number of kanban columns that can fit.
pub fn max_visible_columns(config: &CortexConfig, terminal: &Terminal) -> usize {
    let term_width = terminal.size().unwrap_or(Size::new(80, 24)).width;
    let sidebar_width = config.theme.sidebar_width;
    let kanban_width = term_width.saturating_sub(sidebar_width);
    let available = kanban_width.saturating_sub(6);
    let col_width = config.theme.column_width;
    std::cmp::max(1, (available / col_width) as usize)
}

/// Ensure the focused column is visible by adjusting the scroll offset.
pub fn ensure_column_visible(state: &mut AppState, config: &CortexConfig, terminal: &Terminal) {
    let total_cols = config.columns.visible_column_ids().len();
    if total_cols == 0 {
        return;
    }

    let max_visible = max_visible_columns(config, terminal);

    if total_cols <= max_visible {
        state.kanban.kanban_scroll_offset = 0;
        return;
    }

    let focused = state.kanban.focused_column_index;
    let offset = &mut state.kanban.kanban_scroll_offset;

    if focused < *offset {
        *offset = focused;
    } else if focused >= *offset + max_visible {
        *offset = focused - max_visible + 1;
    }

    let max_offset = total_cols.saturating_sub(max_visible);
    *offset = (*offset).min(max_offset);
}

/// Update the focused task ID based on the column's focused task index.
pub fn update_focused_task_id(state: &mut AppState, col_id: &str) {
    let idx = state
        .kanban
        .focused_task_index
        .get(col_id)
        .copied()
        .unwrap_or(0);
    if let Some(task_ids) = state.kanban.columns.get(col_id) {
        let clamped = idx.min(task_ids.len().saturating_sub(1));
        state.ui.focused_task_id = task_ids.get(clamped).cloned();
    }
}
