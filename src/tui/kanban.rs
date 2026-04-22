//! Kanban board renderer — column lanes with task cards.

use crate::config::types::CortexConfig;
use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Render the kanban board in the given area.
pub fn render_kanban(f: &mut Frame, area: Rect, state: &AppState, config: &CortexConfig) {
    let visible_columns = config.columns.visible_column_ids();
    if visible_columns.is_empty() {
        return;
    }

    let num_cols = visible_columns.len() as u16;
    let col_constraints: Vec<Constraint> = (0..num_cols)
        .map(|_| Constraint::Min(config.theme.column_width))
        .collect();

    let columns_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(col_constraints)
        .split(area);

    for (i, col_id) in visible_columns.iter().enumerate() {
        if i >= columns_layout.len() as usize {
            break;
        }

        let col_area = columns_layout[i];
        let is_focused = state.ui.focused_column == *col_id;

        // Column header
        let display_name = config.columns.display_name_for(col_id);
        let task_count = state
            .kanban
            .columns
            .get(col_id.as_str())
            .map(|v| v.len())
            .unwrap_or(0);
        let header_text = format!(" {} ({}) ", display_name, task_count);

        let header_style = if is_focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let border_style = if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        // Vertical layout: header + tasks
        let v_constraints = [
            Constraint::Length(2), // Header
            Constraint::Min(0),    // Task list
        ];
        let v_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(v_constraints)
            .split(col_area);

        // Header block
        let header_block = Block::default()
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .border_style(border_style)
            .title(Span::styled(header_text, header_style));
        let header_paragraph = Paragraph::new("").block(header_block);
        f.render_widget(header_paragraph, v_layout[0]);

        // Task list area
        let task_block = Block::default()
            .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
            .border_style(border_style);

        let inner = task_block.inner(v_layout[1]);
        f.render_widget(task_block, v_layout[1]);

        // Render task cards or empty-state placeholder
        let task_ids = state.kanban.columns.get(col_id.as_str());
        let has_tasks = task_ids.map_or(false, |ids| !ids.is_empty());

        if has_tasks {
            let task_ids = task_ids.unwrap();
            let mut card_y = inner.y;
            let focused_idx = state
                .kanban
                .focused_task_index
                .get(col_id.as_str())
                .copied()
                .unwrap_or(0);

            let mut rendered_count = 0usize;
            for (task_idx, task_id) in task_ids.iter().enumerate() {
                if let Some(task) = state.tasks.get(task_id) {
                    let is_task_focused = is_focused
                        && task_idx == focused_idx
                        && state.ui.focused_task_id.as_deref() == Some(task_id.as_str());

                    let card_height = 4u16;
                    if card_y + card_height > inner.y + inner.height {
                        break; // No more space
                    }

                    let card_area = Rect {
                        x: inner.x,
                        y: card_y,
                        width: inner.width,
                        height: card_height,
                    };

                    crate::tui::task_card::render_task_card(
                        f,
                        card_area,
                        task,
                        is_task_focused,
                        &config.theme,
                    );
                    card_y += card_height + 1; // 1px gap between cards
                    rendered_count += 1;
                }
            }

            // Show scroll indicator if tasks were clipped
            let remaining = task_ids.len() - rendered_count;
            if remaining > 0 && inner.height > 0 {
                let indicator_y = inner.y + inner.height.saturating_sub(1);
                let indicator_text = format!("  ▼ {} more", remaining);
                let indicator = Paragraph::new(indicator_text)
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(
                    indicator,
                    Rect {
                        x: inner.x,
                        y: indicator_y,
                        width: inner.width,
                        height: 1,
                    },
                );
            }
        } else {
            // Empty column — show placeholder text
            let center_y = inner.y + inner.height / 2;
            if is_focused {
                let line1 = Paragraph::new("No tasks")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                f.render_widget(
                    line1,
                    Rect {
                        x: inner.x,
                        y: center_y.saturating_sub(1),
                        width: inner.width,
                        height: 1,
                    },
                );

                let line2 = Paragraph::new("Press n to create one")
                    .style(Style::default().fg(Color::Gray))
                    .alignment(Alignment::Center);
                f.render_widget(
                    line2,
                    Rect {
                        x: inner.x,
                        y: center_y,
                        width: inner.width,
                        height: 1,
                    },
                );
            } else {
                let placeholder = Paragraph::new("No tasks")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                f.render_widget(
                    placeholder,
                    Rect {
                        x: inner.x,
                        y: center_y,
                        width: inner.width,
                        height: 1,
                    },
                );
            }
        }
    }
}
