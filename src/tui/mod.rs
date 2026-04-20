//! TUI module — terminal user interface for cortex.

pub mod app;
pub mod editor_handler;
pub mod help;
pub mod kanban;
pub mod keys;
pub mod prompt;
pub mod sidebar;
pub mod status_bar;
pub mod task_card;
pub mod task_detail;
pub mod task_editor;

use ratatui::prelude::*;
use crate::state::types::FocusedPanel;

/// Type alias for the terminal backend.
pub type CrosstermBackend = ratatui::backend::CrosstermBackend<std::io::Stdout>;
/// Type alias for the terminal.
pub type Terminal = ratatui::Terminal<CrosstermBackend>;

/// Render the normal mode layout: sidebar + kanban + status bar.
pub fn render_normal(
    f: &mut ratatui::Frame,
    state: &mut crate::state::types::AppState,
    config: &crate::config::types::CortexConfig,
) {
    let area = f.area();

    // Main horizontal layout: sidebar | kanban
    let sidebar_width = config.theme.sidebar_width;
    let constraints = [
        Constraint::Length(sidebar_width),
        Constraint::Min(0), // Kanban takes remaining space
    ];
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    // Vertical layout for sidebar: sidebar content | status bar
    let v_constraints = [
        Constraint::Min(0),
        Constraint::Length(1), // Status bar
    ];
    let sidebar_v = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(h_layout[0]);

    // Vertical layout for kanban: kanban content | status bar
    let kanban_v = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(h_layout[1]);

    sidebar::render_sidebar(f, sidebar_v[0], state, config);
    match state.ui.focused_panel {
        FocusedPanel::Kanban => {
            kanban::render_kanban(f, kanban_v[0], state, config);
        }
        FocusedPanel::TaskDetail => {
            if let Some(task_id) = state.ui.viewing_task_id.clone() {
                task_detail::render_task_detail(f, kanban_v[0], state, &task_id, &config.theme);
            } else {
                kanban::render_kanban(f, kanban_v[0], state, config);
            }
        }
    }
    status_bar::render_status_bar(f, kanban_v[1], state);
}
