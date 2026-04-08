//! TUI module — terminal user interface for cortex.

pub mod app;
pub mod editor_handler;
pub mod help;
pub mod kanban;
pub mod keys;
pub mod sidebar;
pub mod status_bar;
pub mod task_card;
pub mod task_editor;

use ratatui::prelude::*;

/// Type alias for the terminal backend.
pub type CrosstermBackend = ratatui::backend::CrosstermBackend<std::io::Stdout>;
/// Type alias for the terminal.
pub type Terminal = ratatui::Terminal<CrosstermBackend>;

/// Render the entire application based on current mode.
pub fn render(
    f: &mut ratatui::Frame,
    state: &std::sync::Mutex<crate::state::types::AppState>,
    config: &crate::config::types::CortexConfig,
) {
    let state = state.lock().unwrap();
    match state.ui.mode {
        crate::state::types::AppMode::Normal => {
            render_normal(f, &state, config);
        }
        crate::state::types::AppMode::TaskEditor => {
            task_editor::render_task_editor(f, &state);
        }
        crate::state::types::AppMode::Help => {
            render_normal(f, &state, config);
            help::render_help_overlay(f);
        }
    }
}

/// Render the normal mode layout: sidebar + kanban + status bar.
pub fn render_normal(
    f: &mut ratatui::Frame,
    state: &crate::state::types::AppState,
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
    kanban::render_kanban(f, kanban_v[0], state, config);
    status_bar::render_status_bar(f, kanban_v[1], state);
}
