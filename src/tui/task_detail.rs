//! Task detail view — full-screen panel for viewing task metadata, streaming output, messages, and permissions.

use crate::state::types::{
    AgentStatus, AppState, CortexTask, MessageRole, TaskDetailSession, TaskMessagePart, ToolState,
};
use super::format_elapsed_time;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Render the task detail panel in the given area.
///
/// Shows task metadata (title, status, timer, agent), description,
/// streaming agent output, messages, and pending permissions.
/// When drilled into a subagent (via ctrl+x), shows the subagent's
/// output with a breadcrumb navigation bar.
pub fn render_task_detail(
    f: &mut Frame,
    area: Rect,
    state: &mut AppState,
    task_id: &str,
    theme: &crate::config::types::ThemeConfig,
    now: i64,
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

    // Check if we're drilled into a subagent
    let drilled_session_id = state.get_drilldown_session_id().map(|s| s.to_string());
    let is_drilled = drilled_session_id.is_some();

    // Extract permission data early so we can drop the immutable borrow
    // before passing &mut state to render_streaming_block.
    let has_permissions = if is_drilled {
        // For drilled-down subagents, check subagent session data
        drilled_session_id
            .as_ref()
            .and_then(|sid| state.session_tracker.subagent_session_data.get(sid))
            .map(|s| !s.pending_permissions.is_empty() || !s.pending_questions.is_empty())
            .unwrap_or(false)
    } else {
        state
            .session_tracker.task_sessions
            .get(task_id)
            .map(|s| !s.pending_permissions.is_empty() || !s.pending_questions.is_empty())
            .unwrap_or(false)
    };
    let permission_rows: u16 = if has_permissions { 2 } else { 0 };

    // Breadcrumb row when drilled into subagent
    let breadcrumb_rows: u16 = if is_drilled { 1 } else { 0 };

    // Outer block with task title (derived from description)
    let display_title = crate::state::types::derive_title_from_description(&task.description);
    let title = format!(" #{}: {} ", task.number, display_title);
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
    // Breadcrumb: 1 row (only when drilled into subagent)
    // Description block: min 3 rows (border + content)
    // Subagent summary: 0 or variable rows (only when not drilled in and has subagents)
    // Streaming block: remaining space
    // Permissions/questions: up to 2 rows
    // Footer hints: 1 row

    // Calculate subagent summary rows
    let subagent_summary_rows: u16 = if !is_drilled {
        let sub_count = state.get_subagent_sessions(task_id).len();
        if sub_count > 0 { (sub_count as u16).min(5) } else { 0 }
    } else {
        0
    };

    // Reserve footer row
    let footer_height: u16 = 1;
    // Reserve metadata row
    let metadata_height: u16 = 1;
    // Reserve spacer
    let spacer_height: u16 = 1;

    let used_fixed = metadata_height + breadcrumb_rows + spacer_height + subagent_summary_rows + permission_rows + footer_height;
    let remaining = inner.height.saturating_sub(used_fixed);

    // Description block: min 3 rows (border), max 25% of remaining
    let desc_min = if remaining >= 6 { 4u16 } else { 3u16 };
    let desc_height = remaining.max(desc_min).min(remaining) / 4;
    let desc_height = desc_height.max(desc_min).min(remaining.saturating_sub(3));

    let v_constraints: Vec<Constraint> = vec![
        Constraint::Length(metadata_height), // Metadata line
        Constraint::Length(breadcrumb_rows), // Breadcrumb (when drilled)
        Constraint::Length(spacer_height),   // Spacer
        Constraint::Length(desc_height),     // Description block
        Constraint::Length(subagent_summary_rows), // Subagent summary
        Constraint::Min(0),                  // Streaming block (fills remaining)
        Constraint::Length(permission_rows), // Permissions/questions
        Constraint::Length(footer_height),   // Key hints
    ];

    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(inner);

    // ── 1. Metadata line ─────────────────────────────────────────────
    render_metadata_line(f, v_layout[0], task, theme, now);

    // ── 2. Breadcrumb (only when drilled into subagent) ───────────────
    if is_drilled {
        render_breadcrumb(f, v_layout[1], state);
    }

    // ── 3. Description block ─────────────────────────────────────────
    render_description_block(f, v_layout[3], task);

    // ── 4. Subagent summary (only when not drilled in and has subagents)
    if subagent_summary_rows > 0 {
        render_subagent_summary(f, v_layout[4], state, task_id, theme);
    }

    // ── 5. Streaming output + messages ───────────────────────────────
    if is_drilled {
        // Render subagent's output
        render_subagent_streaming_block(f, v_layout[5], state);
    } else {
        // Render parent task's output
        render_streaming_block(f, v_layout[5], state, task_id);
    }

    // ── 6. Pending permissions / questions ───────────────────────────
    if has_permissions {
        if is_drilled {
            if let Some(ref sid) = drilled_session_id {
                if let Some(session) = state.session_tracker.subagent_session_data.get(sid) {
                    render_permissions(f, v_layout[6], session);
                }
            }
        } else if let Some(session) = state.session_tracker.task_sessions.get(task_id) {
            render_permissions(f, v_layout[6], session);
        }
    }

    // ── 7. Footer key hints ──────────────────────────────────────────
    let has_scrollable_output = if is_drilled {
        drilled_session_id
            .as_ref()
            .and_then(|sid| state.session_tracker.cached_streaming_lines.get(sid))
            .map(|(_, lines)| lines.len() > v_layout[5].height as usize)
            .unwrap_or(false)
    } else {
        state
            .session_tracker.cached_streaming_lines
            .get(task_id)
            .map(|(_, lines)| lines.len() > v_layout[5].height as usize)
            .unwrap_or(false)
    };
    render_footer(f, v_layout[7], has_scrollable_output, is_drilled);
}

/// Render the metadata line: status icon, status text, timer, agent name.
fn render_metadata_line(
    f: &mut Frame,
    area: Rect,
    task: &CortexTask,
    theme: &crate::config::types::ThemeConfig,
    now: i64,
) {
    let status_icon = task.agent_status.icon();
    let status_text = task.agent_status.to_string();
    let status_color = match task.agent_status {
        AgentStatus::Running => theme.working_color(),
        AgentStatus::Ready => Color::Cyan,
        AgentStatus::Complete => theme.done_color(),
        AgentStatus::Error => theme.error_color(),
        AgentStatus::Hung => theme.question_color(),
        AgentStatus::Pending => Color::DarkGray,
    };

    let elapsed = format_elapsed_time(task.entered_column_at, now);
    let agent_name = task.agent_type.as_deref().unwrap_or("none");

    // Task number badge
    let badge = format!("#{}", task.number);
    let badge_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span> = vec![
        Span::styled(badge, badge_style),
        Span::raw("  "),
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

    if task.description.is_empty() {
        // Styled placeholder for empty description
        let placeholder = Paragraph::new(Span::styled(
            "  (no description)",
            Style::default()
                .fg(Color::Rgb(100, 100, 120))
                .add_modifier(Modifier::ITALIC),
        ));
        f.render_widget(placeholder, inner);
    } else {
        let para = Paragraph::new(task.description.as_str())
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true });
        f.render_widget(para, inner);
    }
}

/// Render the streaming output block with messages.
///
/// Uses a render version cache on `AppState` to avoid rebuilding `Vec<Line>`
/// on every frame when the session data hasn't changed.
fn render_streaming_block(f: &mut Frame, area: Rect, state: &mut AppState, task_id: &str) {
    // ── Pre-compute scroll metrics for block title ──────────────────
    // We estimate the inner height (area minus 2 for borders) to compute
    // scroll info before rendering the block itself.
    let total_lines = state
        .session_tracker.cached_streaming_lines
        .get(task_id)
        .map(|(_, lines)| lines.len())
        .unwrap_or(0);
    let inner_height_est = area.height.saturating_sub(2);
    let visible_height = inner_height_est as usize;
    let can_scroll = total_lines > visible_height && visible_height > 0;

    let auto_scroll_offset = if can_scroll {
        total_lines - visible_height
    } else {
        0
    };

    let scroll_offset = match state.ui.user_scroll_offset {
        Some(user_offset) if can_scroll => {
            let max_offset = total_lines.saturating_sub(visible_height);
            user_offset.min(max_offset)
        }
        Some(_) => 0,
        None => auto_scroll_offset,
    };

    let is_manual_scroll = state.ui.user_scroll_offset.is_some()
        && scroll_offset < auto_scroll_offset;

    // ── Block title with scroll position indicator ───────────────────
    let has_session = state.session_tracker.task_sessions.contains_key(task_id);
    let block_title = if can_scroll && has_session {
        let first_visible = scroll_offset + 1;
        let last_visible = (scroll_offset + visible_height).min(total_lines);
        let at_bottom = scroll_offset + visible_height >= total_lines;
        if at_bottom {
            format!(" Agent Output ▼ {}/{} ", last_visible, total_lines)
        } else {
            format!(" Agent Output ║ {}-{}/{} ", first_visible, last_visible, total_lines)
        }
    } else {
        " Agent Output ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            block_title,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Early return if no session data — avoids cache logic for non-existent sessions.
    if !has_session {
        let para = Paragraph::new(Span::styled(
            "No session data available. Start an agent to see output here.",
            Style::default().fg(Color::DarkGray),
        ))
        .wrap(Wrap { trim: true });
        f.render_widget(para, inner);
        return;
    }

    // Extract the session's render_version (short-lived borrow, then dropped).
    let current_version = state
        .session_tracker.task_sessions
        .get(task_id)
        .map(|s| s.render_version)
        .unwrap_or(0);

    // Check render cache: only rebuild lines when the session version changes.
    let cached_version = state
        .session_tracker.cached_streaming_lines
        .get(task_id)
        .map(|(v, _)| *v)
        .unwrap_or(0);

    let lines: Vec<Line<'static>> = if current_version == cached_version {
        // Cache hit — reuse previously built lines
        state
            .session_tracker.cached_streaming_lines
            .get(task_id)
            .map(|(_, lines)| lines.clone())
            .unwrap_or_default()
    } else {
        // Cache miss — re-borrow session (immutable), build lines, then
        // write to cache (mutable). These borrows are sequential, not
        // simultaneous, so the borrow checker is happy.
        let built = state
            .session_tracker.task_sessions
            .get(task_id)
            .map(|s| build_streaming_lines(s))
            .unwrap_or_default();
        state
            .session_tracker.cached_streaming_lines
            .insert(task_id.to_string(), (current_version, built.clone()));
        // Evict stale entries when cache grows too large
        state.prune_streaming_cache(10);
        built
    };

    if lines.is_empty() {
        let para = Paragraph::new(Span::styled(
            "Waiting for agent output...",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(para, inner);
        return;
    }

    // Use pre-computed scroll values for rendering
    let para = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    // ── Manual scroll indicator ──────────────────────────────────────
    if is_manual_scroll {
        let manual_label = "── manual scroll (press G to resume) ──";
        let label_width = manual_label.len() as u16;
        if label_width <= inner.width {
            let x = inner.x + (inner.width - label_width) / 2;
            let y = inner.y;
            let area = Rect::new(x, y, label_width, 1);
            let indicator = Paragraph::new(Span::styled(
                manual_label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            f.render_widget(indicator, area);
        }
    }
}

/// Build the streaming output lines from session messages and streaming text.
fn build_streaming_lines(session: &TaskDetailSession) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (msg_idx, msg) in session.messages.iter().enumerate() {
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
                            Span::styled(prefix.to_owned(), prefix_style),
                            Span::styled(line.to_string(), Style::default().fg(Color::White)),
                        ]));
                    }
                    // Add separator after each message for readability
                    if msg_idx + 1 < session.messages.len() {
                        lines.push(Line::from(Span::styled(
                            "  ─────────────────────────────────────────",
                            Style::default().fg(Color::Rgb(60, 64, 80)),
                        )));
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

                    let tool_label = if let Some(ref summary) = cached_summary {
                        format!("{} {}({})", state_icon, tool, summary)
                    } else {
                        format!("{} {}", state_icon, tool)
                    };

                    lines.push(Line::from(vec![
                        Span::styled("  > ".to_owned(), Style::default().fg(Color::DarkGray)),
                        Span::styled(tool_label, Style::default().fg(state_color)),
                    ]));

                    if let Some(err) = error {
                        for line in err.lines().take(3) {
                            lines.push(Line::from(vec![
                                Span::styled("    ".to_owned(), Style::default()),
                                Span::styled(line.to_string(), Style::default().fg(Color::Red)),
                            ]));
                        }
                    }
                }
                TaskMessagePart::StepStart { .. } => {
                    lines.push(Line::from(Span::styled(
                        "  ── step start ──",
                        Style::default().fg(Color::Rgb(60, 64, 80)),
                    )));
                }
                TaskMessagePart::StepFinish { .. } => {
                    lines.push(Line::from(Span::styled(
                        "  ── step done ──",
                        Style::default().fg(Color::Rgb(60, 64, 80)),
                    )));
                }
                TaskMessagePart::Reasoning { text } => {
                    // Add separator before reasoning block
                    lines.push(Line::from(Span::styled(
                        "  ── reasoning ──",
                        Style::default().fg(Color::Rgb(80, 60, 100)),
                    )));
                    for line in text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled("  💭 ".to_owned(), Style::default().fg(Color::Magenta)),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::Rgb(180, 140, 255)),
                            ),
                        ]));
                    }
                }
                TaskMessagePart::Agent { id, agent } => {
                    // Agent parts are drill-down targets — render with indicator
                    lines.push(Line::from(vec![
                        Span::styled("  ▸ ".to_owned(), Style::default().fg(Color::Yellow)),
                        Span::styled(
                            format!("▶ {} agent ", agent),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "[ctrl+x to drill in]".to_string(),
                            Style::default().fg(Color::Rgb(120, 120, 140)),
                        ),
                    ]));
                }
                TaskMessagePart::Unknown => {}
            }
        }
    }

    if let Some(ref streaming) = session.streaming_text {
        if !streaming.is_empty() {
            for line in streaming.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
            lines.push(Line::from(Span::styled(
                "▊",
                Style::default().fg(Color::Cyan),
            )));
        }
    }

    lines
}

/// Render a compact summary of subagent sessions for a parent task.
///
/// Shows each subagent's agent name, status (active/error/done), and
/// error message if applicable. Displayed between the description block
/// and streaming output in the task detail view.
fn render_subagent_summary(
    f: &mut Frame,
    area: Rect,
    state: &AppState,
    task_id: &str,
    theme: &crate::config::types::ThemeConfig,
) {
    let subagents = state.get_subagent_sessions(task_id);
    if subagents.is_empty() {
        return;
    }

    let block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(
            format!(" Subagents ({}) ", subagents.len()),
            Style::default().fg(Color::DarkGray),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let max_rows = inner.height as usize;
    let lines: Vec<Line> = subagents
        .iter()
        .take(max_rows)
        .map(|sub| {
            let (icon, color) = if let Some(ref _err) = sub.error_message {
                ("✗", theme.error_color())
            } else if sub.active {
                ("◐", theme.working_color())
            } else {
                ("✓", theme.done_color())
            };
            let label = format!("{} {} agent (depth {})", icon, sub.agent_name, sub.depth);
            let mut spans = vec![Span::styled(label, Style::default().fg(color))];
            if let Some(ref err) = sub.error_message {
                let truncated: String = err.chars().take(60).collect();
                spans.push(Span::styled(
                    format!(" — {}", truncated),
                    Style::default().fg(theme.error_color()),
                ));
            }
            Line::from(spans)
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Render the breadcrumb navigation bar when drilled into a subagent.
///
/// Shows the path like "Task #3 > planning > do" with visual indicators.
fn render_breadcrumb(f: &mut Frame, area: Rect, state: &AppState) {
    let breadcrumb = state.get_drilldown_breadcrumb();
    if breadcrumb.is_empty() {
        return;
    }

    let spans = vec![
        Span::styled(" ◀ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            breadcrumb,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
    ];
    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// Render the streaming output for a drilled-down subagent session.
///
/// Uses the same render cache pattern as `render_streaming_block` but
/// reads from `subagent_session_data` instead of `task_sessions`.
fn render_subagent_streaming_block(f: &mut Frame, area: Rect, state: &mut AppState) {
    let session_id = match state.get_drilldown_session_id() {
        Some(sid) => sid.to_string(),
        None => return,
    };

    // Pre-compute scroll metrics
    let total_lines = state
        .session_tracker.cached_streaming_lines
        .get(&session_id)
        .map(|(_, lines)| lines.len())
        .unwrap_or(0);
    let inner_height_est = area.height.saturating_sub(2);
    let visible_height = inner_height_est as usize;
    let can_scroll = total_lines > visible_height && visible_height > 0;

    let auto_scroll_offset = if can_scroll {
        total_lines - visible_height
    } else {
        0
    };

    let scroll_offset = match state.ui.user_scroll_offset {
        Some(user_offset) if can_scroll => {
            let max_offset = total_lines.saturating_sub(visible_height);
            user_offset.min(max_offset)
        }
        Some(_) => 0,
        None => auto_scroll_offset,
    };

    let is_manual_scroll = state.ui.user_scroll_offset.is_some()
        && scroll_offset < auto_scroll_offset;

    // Block title with scroll indicator
    let block_title = if can_scroll {
        let first_visible = scroll_offset + 1;
        let last_visible = (scroll_offset + visible_height).min(total_lines);
        let at_bottom = scroll_offset + visible_height >= total_lines;
        if at_bottom {
            format!(" Subagent Output ▼ {}/{} ", last_visible, total_lines)
        } else {
            format!(" Subagent Output ║ {}-{}/{} ", first_visible, last_visible, total_lines)
        }
    } else {
        " Subagent Output ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            block_title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Check render cache
    let current_version = state
        .session_tracker.subagent_session_data
        .get(&session_id)
        .map(|s| s.render_version)
        .unwrap_or(0);

    let cached_version = state
        .session_tracker.cached_streaming_lines
        .get(&session_id)
        .map(|(v, _)| *v)
        .unwrap_or(0);

    let lines: Vec<Line<'static>> = if current_version == cached_version {
        state
            .session_tracker.cached_streaming_lines
            .get(&session_id)
            .map(|(_, lines)| lines.clone())
            .unwrap_or_default()
    } else {
        let built = state
            .session_tracker.subagent_session_data
            .get(&session_id)
            .map(|s| build_streaming_lines(s))
            .unwrap_or_default();
        state
            .session_tracker.cached_streaming_lines
            .insert(session_id.clone(), (current_version, built.clone()));
        state.prune_streaming_cache(10);
        built
    };

    if lines.is_empty() {
        let para = Paragraph::new(Span::styled(
            "Loading subagent output...",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(para, inner);
        return;
    }

    let para = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    // Manual scroll indicator
    if is_manual_scroll {
        let manual_label = "── manual scroll (press G to resume) ──";
        let label_width = manual_label.len() as u16;
        if label_width <= inner.width {
            let x = inner.x + (inner.width - label_width) / 2;
            let y = inner.y;
            let area = Rect::new(x, y, label_width, 1);
            let indicator = Paragraph::new(Span::styled(
                manual_label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
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
        // Show question label and text
        spans.push(Span::styled(
            " ? ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{} ", question.question),
            Style::default().fg(Color::White),
        ));
        if !question.answers.is_empty() {
            // Show numbered answer options: [1] option1  [2] option2
            spans.push(Span::styled(
                "Answers: ",
                Style::default().fg(Color::DarkGray),
            ));
            for (i, answer) in question.answers.iter().enumerate() {
                let key = format!("{}", i + 1);
                spans.push(Span::styled(
                    format!("[{}]", key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("{} ", answer),
                    Style::default().fg(Color::White),
                ));
            }
        }
    }

    if !spans.is_empty() {
        let para = Paragraph::new(Line::from(spans));
        f.render_widget(para, area);
    }
}

/// Render the footer with key hints.
fn render_footer(f: &mut Frame, area: Rect, has_scrollable_output: bool, is_drilled: bool) {
    let hints = if is_drilled {
        if has_scrollable_output {
            "Esc: back  Up/Down: scroll  G: bottom  g: top  ctrl+x: drill deeper  y: approve  n: reject"
        } else {
            "Esc: back  ctrl+x: drill deeper  y: approve  n: reject  1-9: answer question"
        }
    } else if has_scrollable_output {
        "Esc: back  Up/Down: scroll  G: bottom  g: top  ctrl+x: drill into subagent  y: approve  n: reject"
    } else {
        "Esc: back  ctrl+x: drill into subagent  y: approve  n: reject  1-9: answer question"
    };
    let para = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    f.render_widget(para, area);
}
