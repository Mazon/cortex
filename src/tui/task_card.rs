//! Task card component — renders a task as a bordered card within a column.

use crate::config::types::ThemeConfig;
use crate::state::types::{AgentStatus, CortexTask};
use super::format_elapsed_time;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

/// Render a task card in the given area.
pub fn render_task_card(
    f: &mut Frame,
    area: Rect,
    task: &CortexTask,
    is_selected: bool,
    theme: &ThemeConfig,
    now: i64,
) {
    let border_color = if is_selected {
        Color::Cyan
    } else if task.agent_status == AgentStatus::Running {
        // Running tasks get a colored border using the theme working color
        // for a subtle visual indicator that distinguishes them from idle tasks.
        theme.working_color()
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

    // Line 1: #<number> <first line of description> (truncated)
    let prefix_len = format!("#{} ", task.number).chars().count();
    let max_title_len = inner.width as usize;
    let display_title = crate::state::types::display_title_for_task(task, max_title_len.saturating_sub(prefix_len));
    let title_line = format!("#{} {}", task.number, display_title);

    // Line 2: status text — use theme colors
    let status_icon = task.agent_status.icon();
    let status_text = task.agent_status.to_string();
    let status_color = match task.agent_status {
        AgentStatus::Running => theme.working_color(),
        AgentStatus::Ready => Color::Cyan,
        AgentStatus::Complete => theme.done_color(),
        AgentStatus::Error => theme.error_color(),
        AgentStatus::Hung => theme.question_color(),
        AgentStatus::Question => theme.question_color(),
        AgentStatus::Pending => Color::DarkGray,
    };

    let mut status_line = format!("{} {}", status_icon, status_text);
    // Permission/question indicators — bold + bright colors for visibility
    let has_permissions = task.pending_permission_count > 0;
    let has_questions = task.pending_question_count > 0;
    if has_permissions {
        status_line.push_str(&format!(" !{}", task.pending_permission_count));
    }
    if has_questions {
        status_line.push_str(&format!(" ?{}", task.pending_question_count));
    }

    // Build status line as styled spans so indicators get bold + bright colors
    let mut status_spans: Vec<Span<'_>> = if !has_permissions && !has_questions {
        vec![Span::styled(status_line, Style::default().fg(status_color))]
    } else {
        let mut spans = Vec::new();

        // Collect indicator positions: (byte_position, kind)
        let mut indicator_positions: Vec<(usize, char)> = Vec::new();
        let mut search_from = 0;
        while search_from < status_line.len() {
            let perm_idx = status_line[search_from..]
                .find(" !")
                .map(|i| search_from + i);
            let quest_idx = status_line[search_from..]
                .find(" ?")
                .map(|i| search_from + i);

            match (perm_idx, quest_idx) {
                (Some(p), Some(q)) => {
                    if p <= q {
                        indicator_positions.push((p, '!'));
                        search_from = p + 2;
                    } else {
                        indicator_positions.push((q, '?'));
                        search_from = q + 2;
                    }
                }
                (Some(p), None) => {
                    indicator_positions.push((p, '!'));
                    search_from = p + 2;
                }
                (None, Some(q)) => {
                    indicator_positions.push((q, '?'));
                    search_from = q + 2;
                }
                (None, None) => break,
            }
        }

        // Build spans from indicator positions
        let mut last_end = 0;
        for (pos, kind) in &indicator_positions {
            // Text before this indicator
            if *pos > last_end {
                spans.push(Span::styled(
                    status_line[last_end..*pos].to_string(),
                    Style::default().fg(status_color),
                ));
            }

            // Extract the number after the indicator character
            let num_start = *pos + 2; // skip " !" or " ?"
            let num_end = status_line[num_start..]
                .find(|c: char| !c.is_ascii_digit())
                .map(|i| num_start + i)
                .unwrap_or(status_line.len());

            let indicator_str = format!("{}{}", kind, &status_line[num_start..num_end]);
            let indicator_style = if *kind == '!' {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                // Question indicator — bright yellow for high visibility
                Style::default()
                    .fg(Color::Rgb(255, 255, 0))
                    .add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled(indicator_str, indicator_style));

            last_end = num_end;
        }

        // Remaining text after the last indicator
        if last_end < status_line.len() {
            spans.push(Span::styled(
                status_line[last_end..].to_string(),
                Style::default().fg(status_color),
            ));
        }

        // Fallback: if parsing produced no spans, render as plain text
        if spans.is_empty() {
            spans.push(Span::styled(status_line, Style::default().fg(status_color)));
        }

        spans
    };

    // Add a dim keybinding hint for hung/error tasks to improve discoverability
    if matches!(task.agent_status, AgentStatus::Hung | AgentStatus::Error) {
        status_spans.push(Span::styled(
            " [R]",
            Style::default().fg(Color::DarkGray),
        ));
    }

    if inner.height >= 1 {
        // Line 1 (title) — always render if we have any space
        let title_para = Paragraph::new(Span::styled(
            title_line,
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
            let status_para = Paragraph::new(Line::from(status_spans));
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

        // Line 3 (timer) — show elapsed time since entering current column.
        // For terminal states (done, ready, hung, error), freeze the timer at
        // the point the task finished (last_activity_at) instead of ticking.
        if inner.height >= 3 {
            let timer_end = if task.agent_status.is_terminal() {
                task.last_activity_at
            } else {
                now
            };
            let elapsed = format_elapsed_time(task.entered_column_at, timer_end);
            if !elapsed.is_empty() {
                let timer_spans = vec![
                    Span::styled("⏱ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(elapsed, Style::default().fg(Color::Rgb(160, 160, 180))),
                ];
                let timer_para = Paragraph::new(Line::from(timer_spans));
                f.render_widget(
                    timer_para,
                    Rect {
                        x: inner.x,
                        y: inner.y + 2,
                        width: inner.width,
                        height: 1,
                    },
                );
            }
        }
    }
}
