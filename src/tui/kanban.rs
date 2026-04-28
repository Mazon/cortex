//! Kanban board renderer — column lanes with task cards.
//!
//! Supports horizontal scrolling when the number of visible columns exceeds
//! the available terminal width. Scroll indicators are rendered at the edges
//! when columns are hidden off-screen.

use crate::config::types::CortexConfig;
use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Width reserved for each horizontal scroll indicator.
const SCROLL_INDICATOR_WIDTH: u16 = 3;

/// Render the kanban board in the given area.
pub fn render_kanban(f: &mut Frame, area: Rect, state: &AppState, config: &CortexConfig, now: i64) {
    let all_visible = config.columns.visible_column_ids();
    if all_visible.is_empty() {
        return;
    }

    let total_cols = all_visible.len();
    let col_width = config.theme.column_width;

    let available_for_columns = area.width.saturating_sub(SCROLL_INDICATOR_WIDTH * 2);
    let max_visible = std::cmp::max(1, (available_for_columns / col_width) as usize);

    let can_show_all = total_cols <= max_visible;

    let scroll_offset = if can_show_all {
        0
    } else {
        state
            .kanban
            .kanban_scroll_offset
            .min(total_cols.saturating_sub(max_visible))
    };

    let end = std::cmp::min(scroll_offset + max_visible, total_cols);
    let visible_slice = &all_visible[scroll_offset..end];
    let num_visible = visible_slice.len();

    let has_left_indicator = !can_show_all && scroll_offset > 0;
    let has_right_indicator = !can_show_all && end < total_cols;

    let mut constraints: Vec<Constraint> = Vec::with_capacity(num_visible + 2);
    if has_left_indicator {
        constraints.push(Constraint::Length(SCROLL_INDICATOR_WIDTH));
    }
    for _ in 0..num_visible {
        constraints.push(Constraint::Min(col_width));
    }
    if has_right_indicator {
        constraints.push(Constraint::Length(SCROLL_INDICATOR_WIDTH));
    }

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    let mut chunk_idx = 0usize;

    if has_left_indicator {
        let indicator = Paragraph::new("\u{25c0}")
            .style(Style::default().fg(Color::Yellow))
            .alignment(Alignment::Center);
        f.render_widget(indicator, layout[chunk_idx]);
        chunk_idx += 1;
    }

    for col_id in visible_slice {
        if chunk_idx >= layout.len() {
            break;
        }

        let col_area = layout[chunk_idx];
        let is_focused = state.ui.focused_column == *col_id;

        let display_name = config.columns.display_name_for(col_id);
        let task_count = state
            .kanban
            .columns
            .get(col_id.as_str())
            .map(|v| v.len())
            .unwrap_or(0);
        let header_style = if is_focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let header_title: Line = Line::from(Span::styled(
            format!(" {} ({}) ", display_name, task_count),
            header_style,
        ));

        let border_style = if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let v_constraints = [Constraint::Length(2), Constraint::Min(0)];
        let v_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(v_constraints)
            .split(col_area);

        let header_block = Block::default()
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .border_style(border_style)
            .title(header_title);
        let header_paragraph = Paragraph::new("").block(header_block);
        f.render_widget(header_paragraph, v_layout[0]);

        let task_block = Block::default()
            .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
            .border_style(border_style);

        let inner = task_block.inner(v_layout[1]);
        f.render_widget(task_block, v_layout[1]);

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

                    let card_height = 5u16;
                    if card_y + card_height > inner.y + inner.height {
                        break;
                    }

                    let card_area = Rect {
                        x: inner.x,
                        y: card_y,
                        width: inner.width,
                        height: card_height,
                    };

                    crate::tui::task_card::render_task_card(
                        f, card_area, task, is_task_focused, &config.theme, now,
                    );
                    card_y += card_height + 1;
                    rendered_count += 1;
                }
            }

            let remaining = task_ids.len() - rendered_count;
            if remaining > 0 && inner.height > 0 {
                let indicator_y = inner.y + inner.height.saturating_sub(1);
                let indicator_text = format!("  \u{25bc} {} more", remaining);
                let indicator =
                    Paragraph::new(indicator_text).style(Style::default().fg(Color::DarkGray));
                f.render_widget(
                    indicator,
                    Rect { x: inner.x, y: indicator_y, width: inner.width, height: 1 },
                );
            }
        } else {
            let center_y = inner.y + inner.height / 2;
            if is_focused {
                let line1 = Paragraph::new("No tasks")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                f.render_widget(line1, Rect { x: inner.x, y: center_y.saturating_sub(1), width: inner.width, height: 1 });
                let line2 = Paragraph::new("Press n to create one")
                    .style(Style::default().fg(Color::Gray))
                    .alignment(Alignment::Center);
                f.render_widget(line2, Rect { x: inner.x, y: center_y, width: inner.width, height: 1 });
            } else {
                let placeholder = Paragraph::new("No tasks")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                f.render_widget(placeholder, Rect { x: inner.x, y: center_y, width: inner.width, height: 1 });
            }
        }

        chunk_idx += 1;
    }

    if has_right_indicator {
        if chunk_idx < layout.len() {
            let indicator = Paragraph::new("\u{25b6}")
                .style(Style::default().fg(Color::Yellow))
                .alignment(Alignment::Center);
            f.render_widget(indicator, layout[chunk_idx]);
        }
    }
}
