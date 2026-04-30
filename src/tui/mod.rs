//! TUI module — terminal user interface for cortex.

pub mod app;
pub mod diff_view;
pub mod editor_handler;
pub mod help;
pub mod kanban;
pub mod keys;
pub mod loading;
pub mod permission_modal;
pub mod prompt;
pub mod reports;
pub mod sidebar;
pub mod status_bar;
pub mod task_card;
pub mod task_detail;
pub mod task_editor;
pub mod tracing_layer;

use crate::state::types::FocusedPanel;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Minimum terminal dimensions (width × height) required for the TUI layout.
/// Below this, a "terminal too small" message is shown instead of the normal
/// layout, preventing garbled output from overlapping widgets.
const MIN_TERM_WIDTH: u16 = 60;
const MIN_TERM_HEIGHT: u16 = 10;

/// Width of the changed-files sidebar panel in the task detail view.
const CHANGED_FILES_PANEL_WIDTH: u16 = 28;
/// Minimum width required for the task detail panel when the changed-files sidebar is shown.
const MIN_TASK_DETAIL_WIDTH: u16 = 40;

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

    // Vertical layout: content area (sidebar | kanban) | status bar
    // Status bar spans the full terminal width so "● connected" is at the far left.
    let sidebar_width = config.theme.sidebar_width;
    let v_constraints = [
        Constraint::Min(0),   // Content area (sidebar + kanban)
        Constraint::Length(2), // Status bar (1 top border + 1 content)
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(area);

    // Horizontal layout for content area: sidebar | kanban (or sidebar | task_detail | changed_files)
    let show_changed_files = matches!(state.ui.focused_panel, FocusedPanel::TaskDetail)
        && state
            .ui
            .changed_files
            .as_ref()
            .map_or(false, |f| !f.is_empty())
        && area.width
            >= sidebar_width + CHANGED_FILES_PANEL_WIDTH + MIN_TASK_DETAIL_WIDTH;

    let h_constraints: Vec<Constraint> = if show_changed_files {
        vec![
            Constraint::Length(sidebar_width),
            Constraint::Min(MIN_TASK_DETAIL_WIDTH),
            Constraint::Length(CHANGED_FILES_PANEL_WIDTH),
        ]
    } else {
        vec![
            Constraint::Length(sidebar_width),
            Constraint::Min(0),
        ]
    };
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(h_constraints)
        .split(v_layout[0]);

    sidebar::render_sidebar(f, h_layout[0], state, config);
    match state.ui.focused_panel {
        FocusedPanel::Kanban => {
            kanban::render_kanban(f, h_layout[1], state, config, now);
        }
        FocusedPanel::TaskDetail => {
            if let Some(task_id) = state.ui.viewing_task_id.clone() {
                task_detail::render_task_detail(
                    f,
                    h_layout[1],
                    state,
                    &task_id,
                    &config.theme,
                    now,
                );
            } else {
                kanban::render_kanban(f, h_layout[1], state, config, now);
            }
        }
    }

    // Render the changed-files sidebar if visible
    if show_changed_files {
        task_detail::render_changed_files_panel(f, h_layout[2], state, &config.theme);
    }
    let status_area = v_layout[1].inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    status_bar::render_status_bar(f, status_area, state, &config.theme);
}
