//! Archive viewer — view and manage archived tasks.

use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem};

/// Render the archive viewer panel.
pub fn render_archive_viewer(
    f: &mut Frame,
    area: Rect,
    state: &AppState,
    _config: &crate::config::types::ThemeConfig,
) {
    let archived_ids = state.get_archived_task_ids(
        state
            .project_registry
            .active_project_id
            .as_deref()
            .unwrap_or(""),
    );
    // Stub: show empty list with header
    let items: Vec<ListItem> = if archived_ids.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No archived tasks",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        archived_ids
            .iter()
            .map(|id| ListItem::new(id.clone()))
            .collect()
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Archive ")
            .style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(list, area);
}
