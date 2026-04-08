//! Task card component — renders a task as a bordered card within a column.

use crate::state::types::{AgentStatus, CortexTask};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

/// Render a task card in the given area.
pub fn render_task_card(f: &mut Frame, area: Rect, task: &CortexTask, is_selected: bool) {
    let border_color = if is_selected {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let bg_color = if is_selected {
        Color::Rgb(36, 40, 56)
    } else {
        Color::Reset
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_color));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Line 1: #<number> <title> (truncated)
    let max_title_len = inner.width as usize;
    let title_line = format!("#{} {}", task.number, task.title);
    let truncated_title = if title_line.len() > max_title_len {
        format!("{}...", &title_line[..max_title_len.saturating_sub(3)])
    } else {
        title_line
    };

    // Line 2: status text
    let status_icon = task.agent_status.icon();
    let status_text = task.agent_status.to_string();
    let status_color = match task.agent_status {
        AgentStatus::Running => Color::Blue,
        AgentStatus::Complete => Color::Green,
        AgentStatus::Error => Color::Red,
        AgentStatus::Hung => Color::Rgb(255, 87, 34),
        AgentStatus::Pending => Color::DarkGray,
    };

    let mut status_line = format!("{} {}", status_icon, status_text);
    // Permission/question indicators
    if task.pending_permission_count > 0 {
        status_line.push_str(&format!(" !{}", task.pending_permission_count));
    }
    if task.pending_question_count > 0 {
        status_line.push_str(&format!(" ?{}", task.pending_question_count));
    }

    if inner.height >= 1 {
        // Line 1 (title) — always render if we have any space
        let title_para = Paragraph::new(Span::styled(
            truncated_title,
            Style::default().fg(Color::White),
        ));
        f.render_widget(
            title_para,
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );

        // Line 2 (status) — only if we have enough room
        if inner.height >= 2 {
            let status_para = Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{} ", status_icon),
                    Style::default().fg(status_color),
                ),
                Span::styled(status_text, Style::default().fg(status_color)),
            ]));
            f.render_widget(
                status_para,
                Rect {
                    x: inner.x,
                    y: inner.y + 1,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }
}
