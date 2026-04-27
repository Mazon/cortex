//! TUI module — terminal user interface for cortex.

pub mod app;
pub mod editor_handler;
pub mod help;
pub mod kanban;
pub mod keys;
pub mod loading;
pub mod prompt;
pub mod sidebar;
pub mod status_bar;
pub mod task_card;
pub mod task_detail;
pub mod task_editor;
pub mod diff_view;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use crate::state::types::FocusedPanel;

/// Minimum terminal dimensions (width × height) required for the TUI layout.
/// Below this, a "terminal too small" message is shown instead of the normal
/// layout, preventing garbled output from overlapping widgets.
const MIN_TERM_WIDTH: u16 = 60;
const MIN_TERM_HEIGHT: u16 = 10;

/// Format elapsed time since the given timestamp.
pub(crate) fn format_elapsed_time(entered_at: i64, now: i64) -> String {
    if entered_at <= 0 {
        return String::new();
    }
    let elapsed = now.saturating_sub(entered_at).max(0) as u64;
    let secs = elapsed % 60;
    let mins = (elapsed / 60) % 60;
    let hours = elapsed / 3600;
    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Type alias for the terminal backend.
pub type CrosstermBackend = ratatui::backend::CrosstermBackend<std::io::Stdout>;
/// Type alias for the terminal.
pub type Terminal = ratatui::Terminal<CrosstermBackend>;

/// Render the normal mode layout: sidebar + kanban + status bar.
///
/// If the terminal is too small (below [`MIN_TERM_WIDTH`] × [`MIN_TERM_HEIGHT`]),
/// renders a centered "terminal too small" message instead of the normal layout
/// to prevent garbled output from overlapping widgets.
pub fn render_normal(
    f: &mut ratatui::Frame,
    state: &mut crate::state::types::AppState,
    config: &crate::config::types::CortexConfig,
) {
    let area = f.area();

    // Guard: show a fallback message when the terminal is too small.
    if area.width < MIN_TERM_WIDTH || area.height < MIN_TERM_HEIGHT {
        let msg = format!(
            "Terminal too small ({}×{}).\nMinimum required: {}×{}.\nResize your terminal window.",
            area.width, area.height, MIN_TERM_WIDTH, MIN_TERM_HEIGHT,
        );
        let paragraph = Paragraph::new(msg)
            .style(Style::default().fg(Color::Yellow))
            .alignment(Alignment::Center);
        f.render_widget(paragraph, area);
        return;
    }

    let now = chrono::Utc::now().timestamp();

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
            kanban::render_kanban(f, kanban_v[0], state, config, now);
        }
        FocusedPanel::TaskDetail => {
            if let Some(task_id) = state.ui.viewing_task_id.clone() {
                task_detail::render_task_detail(f, kanban_v[0], state, &task_id, &config.theme, now);
            } else {
                kanban::render_kanban(f, kanban_v[0], state, config, now);
            }
        }
    }
    status_bar::render_status_bar(f, kanban_v[1], state, &config.theme);
}
