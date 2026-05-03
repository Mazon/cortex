//! Confirm delete dialog — y/n confirmation for task deletion.

use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Render a centered confirmation dialog for task deletion.
pub fn render_confirm_delete_dialog(f: &mut Frame, area: Rect, state: &AppState) {
    let task_desc = state
        .ui
        .pending_delete_task_id
        .as_ref()
        .and_then(|id| state.tasks.get(id))
        .map(|t| crate::state::types::display_title_for_task(t, 60))
        .unwrap_or_default();
    let text = format!(
        "Delete task \"{}\"?\n\n  y — confirm    n / Esc — cancel",
        task_desc
    );
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm Delete "),
        )
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: true })
        .alignment(Alignment::Center);
    let dialog_area = centered_rect(50, 7, area);
    f.render_widget(paragraph, dialog_area);
}

/// Compute a centered rectangle within the given area.
fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = (r.width.saturating_sub(popup_width)) / 2;
    let y = (r.height.saturating_sub(height)) / 2;
    Rect::new(
        r.x + x,
        r.y + y,
        popup_width.min(r.width),
        height.min(r.height),
    )
}
