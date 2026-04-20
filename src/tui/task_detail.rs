//! Task detail view — full-screen panel for viewing task metadata, streaming output, messages, and permissions.

use crate::state::types::{
    AgentStatus, AppState, CortexTask, MessageRole, TaskDetailSession, TaskMessagePart, ToolState,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Format elapsed time since the given timestamp.
fn format_elapsed_time(entered_at: i64) -> String {
    if entered_at <= 0 {
        return String::new();
    }
    let now = chrono::Utc::now().timestamp();
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

/// Render the task detail panel in the given area.
///
/// Shows task metadata (title, status, timer, agent), description,
/// streaming agent output, messages, and pending permissions.
pub fn render_task_detail(
    f: &mut Frame,
    area: Rect,
    state: &mut AppState,
    task_id: &str,
    theme: &crate::config::types::ThemeConfig,
) {
    let task = match state.tasks.get(task_id) {
        Some(t) => t,
        None => {
            let not_found = Paragraph::new(Span::styled(
                format!("Task not found: {}", task_id),
                Style::default().fg(Color::Red),
            ));
            f.render_widget(not_found, area);
            return;
        }
    };

    // Extract permission data early so we can drop the immutable borrow
    // before passing &mut state to render_streaming_block.
    let has_permissions = state
        .task_sessions
        .get(task_id)
        .map(|s| !s.pending_permissions.is_empty() || !s.pending_questions.is_empty())
        .unwrap_or(false);
    let permission_rows: u16 = if has_permissions { 2 } else { 0 };

    // Outer block with task title
    let title = format!(" #{}: {} ", task.number, task.title);
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    if inner.height < 4 || inner.width < 10 {
        // Too small to render content
        return;
    }

    // ── Vertical layout ──────────────────────────────────────────────
    // Metadata line: 1 row
    // Description block: min 3 rows (border + content)
    // Streaming block: remaining space
    // Permissions/questions: up to 2 rows
    // Footer hints: 1 row

    // Reserve footer row
    let footer_height: u16 = 1;
    // Reserve metadata row
    let metadata_height: u16 = 1;
    // Reserve spacer
    let spacer_height: u16 = 1;

    let used_fixed = metadata_height + spacer_height + permission_rows + footer_height;
    let remaining = inner.height.saturating_sub(used_fixed);

    // Description block: min 3 rows (border), max 30% of remaining
    let desc_min = if remaining >= 6 { 4u16 } else { 3u16 };
    let desc_height = remaining.max(desc_min).min(remaining) / 3;
    let desc_height = desc_height.max(desc_min).min(remaining.saturating_sub(3));

    // Streaming block gets the rest
    let _stream_height = remaining.saturating_sub(desc_height);

    let v_constraints: Vec<Constraint> = vec![
        Constraint::Length(metadata_height), // Metadata line
        Constraint::Length(spacer_height),   // Spacer
        Constraint::Length(desc_height),     // Description block
        Constraint::Min(0),                  // Streaming block (fills remaining)
        Constraint::Length(permission_rows), // Permissions/questions
        Constraint::Length(footer_height),   // Key hints
    ];

    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(inner);

    // ── 1. Metadata line ─────────────────────────────────────────────
    render_metadata_line(f, v_layout[0], task, theme);

    // ── 2. Description block ─────────────────────────────────────────
    render_description_block(f, v_layout[2], task);

    // ── 3. Streaming output + messages (uses &mut state for cache) ────
    let session = state.task_sessions.get(task_id);
    render_streaming_block(f, v_layout[3], session, state, task_id);

    // ── 4. Pending permissions / questions ───────────────────────────
    if has_permissions {
        // Re-borrow session for permissions (immutable borrow is fine here
        // since the mutable borrow ended with render_streaming_block).
        if let Some(session) = state.task_sessions.get(task_id) {
            render_permissions(f, v_layout[4], session);
        }
    }

    // ── 5. Footer key hints ──────────────────────────────────────────
    render_footer(f, v_layout[5]);
}

/// Render the metadata line: status icon, status text, timer, agent name.
fn render_metadata_line(
    f: &mut Frame,
    area: Rect,
    task: &CortexTask,
    theme: &crate::config::types::ThemeConfig,
) {
    let status_icon = task.agent_status.icon();
    let status_text = task.agent_status.to_string();
    let status_color = match task.agent_status {
        AgentStatus::Running => theme.color_or(&theme.status_working, Color::Blue),
        AgentStatus::Complete => theme.color_or(&theme.status_done, Color::Green),
        AgentStatus::Error => theme.color_or(&theme.status_error, Color::Red),
        AgentStatus::Hung => Color::Rgb(255, 87, 34),
        AgentStatus::Pending => Color::DarkGray,
    };

    let elapsed = format_elapsed_time(task.entered_column_at);
    let agent_name = task.agent_type.as_str();

    let mut spans: Vec<Span> = vec![
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", status_icon),
            Style::default().fg(status_color),
        ),
        Span::styled(status_text, Style::default().fg(status_color)),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Timer: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if elapsed.is_empty() {
                "—".to_string()
            } else {
                elapsed.clone()
            },
            Style::default().fg(Color::White),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
        Span::styled(agent_name, Style::default().fg(Color::Yellow)),
    ];

    // Permission count indicator
    if task.pending_permission_count > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("!{}", task.pending_permission_count),
            Style::default().fg(Color::Red),
        ));
    }
    if task.pending_question_count > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("?{}", task.pending_question_count),
            Style::default().fg(Color::Yellow),
        ));
    }

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// Render the description block.
fn render_description_block(f: &mut Frame, area: Rect, task: &CortexTask) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " Description ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let desc_text = if task.description.is_empty() {
        "(no description)"
    } else {
        &task.description
    };

    let style = if task.description.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    let para = Paragraph::new(desc_text)
        .style(style)
        .wrap(Wrap { trim: true });
    f.render_widget(para, inner);
}

/// Render the streaming output block with messages.
fn render_streaming_block(f: &mut Frame, area: Rect, session: Option<&TaskDetailSession>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " Agent Output ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let session = match session {
        Some(s) => s,
        None => {
            let para = Paragraph::new(Span::styled(
                "No session data available. Start an agent to see output here.",
                Style::default().fg(Color::DarkGray),
            ))
            .wrap(Wrap { trim: true });
            f.render_widget(para, inner);
            return;
        }
    };

    // Build content lines from messages + streaming text
    let mut lines: Vec<Line> = Vec::new();

    // Render messages as conversation
    for msg in &session.messages {
        for part in &msg.parts {
            match part {
                TaskMessagePart::Text { text } => {
                    let prefix = match msg.role {
                        MessageRole::User => "▸ ",
                        MessageRole::Assistant => "  ",
                    };
                    let prefix_style = match msg.role {
                        MessageRole::User => Style::default().fg(Color::Cyan),
                        MessageRole::Assistant => Style::default().fg(Color::DarkGray),
                    };
                    for line in text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled(prefix, prefix_style),
                            Span::styled(line.to_string(), Style::default().fg(Color::White)),
                        ]));
                    }
                }
                TaskMessagePart::Tool {
                    tool,
                    state: tool_state,
                    cached_summary,
                    error,
                    ..
                } => {
                    let state_icon = match tool_state {
                        ToolState::Pending => "○",
                        ToolState::Running => "◐",
                        ToolState::Completed => "✓",
                        ToolState::Error => "✗",
                    };
                    let state_color = match tool_state {
                        ToolState::Pending => Color::DarkGray,
                        ToolState::Running => Color::Blue,
                        ToolState::Completed => Color::Green,
                        ToolState::Error => Color::Red,
                    };

                    // Tool invocation line — use pre-computed summary
                    let tool_label = if let Some(ref summary) = cached_summary {
                        format!("{} {}({})", state_icon, tool, summary)
                    } else {
                        format!("{} {}", state_icon, tool)
                    };

                    lines.push(Line::from(vec![
                        Span::styled("  > ", Style::default().fg(Color::DarkGray)),
                        Span::styled(tool_label, Style::default().fg(state_color)),
                    ]));

                    // Show error if any
                    if let Some(err) = error {
                        for line in err.lines().take(3) {
                            lines.push(Line::from(vec![
                                Span::styled("    ", Style::default()),
                                Span::styled(line.to_string(), Style::default().fg(Color::Red)),
                            ]));
                        }
                    }
                }
                TaskMessagePart::StepStart { .. } => {
                    lines.push(Line::from(Span::styled(
                        "  ── step start ──",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                TaskMessagePart::StepFinish { .. } => {
                    lines.push(Line::from(Span::styled(
                        "  ── step done ──",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                TaskMessagePart::Reasoning { text } => {
                    for line in text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled("  💭 ", Style::default().fg(Color::Magenta)),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::Rgb(180, 140, 255)),
                            ),
                        ]));
                    }
                }
                _ => {}
            }
        }
    }

    // Append streaming text (currently being generated)
    if let Some(ref streaming) = session.streaming_text {
        if !streaming.is_empty() {
            for line in streaming.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
            // Cursor indicator
            lines.push(Line::from(Span::styled(
                "▊",
                Style::default().fg(Color::Cyan),
            )));
        }
    }

    if lines.is_empty() {
        let para = Paragraph::new(Span::styled(
            "Waiting for agent output...",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(para, inner);
        return;
    }

    // Calculate scroll: show the last N lines that fit in the visible area
    let visible_height = inner.height as usize;
    let total_lines = lines.len();

    let scroll_offset = if total_lines > visible_height {
        total_lines - visible_height
    } else {
        0
    };

    let para = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    // ── Scroll indicator ────────────────────────────────────────────
    // Show a compact "▼ X-Y/Z" indicator at the bottom-right of the
    // streaming area when content overflows the visible height.
    if total_lines > visible_height {
        let first_visible = scroll_offset + 1;
        let last_visible = (scroll_offset + visible_height).min(total_lines);

        let at_bottom = scroll_offset + visible_height >= total_lines;
        let scrollbar_char = if at_bottom { "▼" } else { "║" };

        let full_indicator = format!("{} {}-{}", scrollbar_char, first_visible, last_visible);
        let total_text = format!("/{}", total_lines);
        let indicator_width = (full_indicator.len() + total_text.len()) as u16;

        if indicator_width <= inner.width {
            // Render the indicator at the bottom-right of the streaming area
            let x = inner.x + inner.width - indicator_width;
            let y = inner.y + inner.height - 1;
            let area = Rect::new(x, y, indicator_width, 1);
            let indicator = Paragraph::new(Line::from(vec![
                Span::styled(full_indicator, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    total_text,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
            f.render_widget(indicator, area);
        }
    }
}

/// Render pending permissions and questions.
fn render_permissions(f: &mut Frame, area: Rect, session: &TaskDetailSession) {
    let mut spans: Vec<Span> = Vec::new();

    // Permissions
    for perm in &session.pending_permissions {
        spans.push(Span::styled(
            format!(" !{} ", perm.id),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{}({}) ", perm.description, perm.tool_name),
            Style::default().fg(Color::White),
        ));
        spans.push(Span::styled(
            "[y:approve / n:reject]",
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Questions
    for question in &session.pending_questions {
        spans.push(Span::styled(
            format!(" ?{} ", question.id),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{} ", question.question),
            Style::default().fg(Color::White),
        ));
        if !question.answers.is_empty() {
            let answers_str = question.answers.join(", ");
            spans.push(Span::styled(
                format!("[{}]", answers_str),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if !spans.is_empty() {
        let para = Paragraph::new(Line::from(spans));
        f.render_widget(para, area);
    }
}

/// Render the footer with key hints.
fn render_footer(f: &mut Frame, area: Rect) {
    let hints = "Esc: back  y: approve  n: reject";
    let para = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    f.render_widget(para, area);
}
