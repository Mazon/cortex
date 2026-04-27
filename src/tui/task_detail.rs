//! Task detail view — full-screen panel for viewing task metadata, streaming output, messages, and permissions.
//!
//! Features:
//! - Two-row metadata header with status pill, agent type, column, and timer
//! - Description block with character count and styled borders
//! - Plan output section (when available)
//! - Error display section with red-tinted border
//! - Subagent summary with bordered block and done count
//! - Streaming output with message count, tool indicators, and subtle background
//! - Permissions block with styled action buttons
//! - Grouped footer key hints
//! - Animated progress indicator for running tasks

use crate::state::types::{
    AgentStatus, AppState, CortexTask, DetailEditorState, MessageRole, TaskDetailSession, TaskMessagePart, ToolState,
};
use super::format_elapsed_time;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Render the task detail panel in the given area.
///
/// Shows task metadata (title, status, timer, agent), description,
/// plan output, error display, streaming agent output, messages, subagent
/// summary, and pending permissions.
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

    if inner.height < 6 || inner.width < 10 {
        // Too small to render content
        return;
    }

    // ── Conditional section flags ──────────────────────────────────────

    // Editor state for footer hints
    let editor_is_focused = state.ui.detail_editor.as_ref().map_or(false, |e| e.is_focused);
    let editor_discard_warning = state.ui.detail_editor.as_ref().map_or(false, |e| e.discard_warning_shown);

    // Error section
    let has_error = task.agent_status == AgentStatus::Error
        && task.error_message
            .as_ref()
            .map_or(false, |e| !e.trim().is_empty());
    let error_rows: u16 = if has_error { 4 } else { 0 };

    // Plan section
    let has_plan = task
        .plan_output
        .as_ref()
        .map_or(false, |p| !p.trim().is_empty());

    // Clone data needed after mutable borrow of state (for description editor)
    let plan_output = task.plan_output.clone();
    let error_message = task.error_message.clone();

    // Subagent summary
    let subagent_count = if !is_drilled {
        state.get_subagent_sessions(task_id).len()
    } else {
        0
    };
    let subagent_block_rows: u16 = if subagent_count > 0 {
        (subagent_count as u16).min(4) + 2 // +2 for borders
    } else {
        0
    };

    // ── Height calculations ────────────────────────────────────────────

    // Two-row metadata header
    let metadata_height: u16 = 2;
    // Separator line
    let separator_height: u16 = 1;
    // Footer
    let footer_height: u16 = 1;
    // Permissions: bordered block needs 3 rows
    let permission_rows: u16 = if has_permissions { 3 } else { 0 };

    // Calculate available space for desc + plan + streaming
    let fixed_total = metadata_height
        + separator_height
        + breadcrumb_rows
        + error_rows
        + subagent_block_rows
        + permission_rows
        + footer_height;
    let available = inner.height.saturating_sub(fixed_total);

    // Description: 3-4 rows
    let desc_rows = if available >= 12 { 4u16 } else { 3u16 };

    // Plan: 3-6 rows, but only if there's enough space and plan exists
    let plan_rows = if has_plan {
        let after_desc = available.saturating_sub(desc_rows);
        if after_desc >= 10 {
            6u16
        } else if after_desc >= 7 {
            5u16
        } else if after_desc >= 5 {
            4u16
        } else {
            3u16
        }
    } else {
        0
    };

    // ── Build layout constraint vector ─────────────────────────────────
    //
    // Fixed indices:
    //   [0] metadata (2 rows)
    //   [1] separator (1 row)
    //   [2] breadcrumb (0 or 1)
    //   [3] description (variable)
    //   [4] plan (0 or variable)
    //   [5] error (0 or 4)
    //   [6] subagent summary (0 or variable)
    //   [7] streaming (Min(0) — fills remaining)
    //   [8] permissions (0 or 3)
    //   [9] footer (1 row)

    let v_constraints: Vec<Constraint> = vec![
        Constraint::Length(metadata_height),    // [0] Metadata header
        Constraint::Length(separator_height),   // [1] Separator
        Constraint::Length(breadcrumb_rows),    // [2] Breadcrumb
        Constraint::Length(desc_rows),          // [3] Description
        Constraint::Length(plan_rows),          // [4] Plan
        Constraint::Length(error_rows),         // [5] Error
        Constraint::Length(subagent_block_rows), // [6] Subagent summary
        Constraint::Min(0),                     // [7] Streaming block
        Constraint::Length(permission_rows),    // [8] Permissions
        Constraint::Length(footer_height),      // [9] Footer
    ];

    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(inner);

    // ── Render sections ────────────────────────────────────────────────

    // 1. Metadata header (two rows)
    render_metadata_header(f, v_layout[0], task, theme, now);

    // 2. Separator line
    render_separator(f, v_layout[1]);

    // 3. Breadcrumb (only when drilled into subagent)
    if is_drilled {
        render_breadcrumb(f, v_layout[2], state);
    }

    // 4. Description block (takes task_id to avoid borrow conflict with &mut state)
    render_description_block(f, v_layout[3], task_id, state);

    // 5. Plan section (conditional)
    if has_plan {
        if let Some(ref plan) = plan_output {
            render_plan_section(f, v_layout[4], plan);
        }
    }

    // 6. Error section (conditional)
    if has_error {
        if let Some(ref err) = error_message {
            render_error_section(f, v_layout[5], err);
        }
    }

    // 7. Subagent summary (only when not drilled in and has subagents)
    if subagent_count > 0 {
        render_subagent_summary(f, v_layout[6], state, task_id, theme);
    }

    // 8. Streaming output + messages
    if is_drilled {
        render_subagent_streaming_block(f, v_layout[7], state, now);
    } else {
        render_streaming_block(f, v_layout[7], state, task_id, now);
    }

    // 9. Pending permissions / questions
    if has_permissions {
        if is_drilled {
            if let Some(ref sid) = drilled_session_id {
                if let Some(session) = state.session_tracker.subagent_session_data.get(sid) {
                    render_permissions(f, v_layout[8], session);
                }
            }
        } else if let Some(session) = state.session_tracker.task_sessions.get(task_id) {
            render_permissions(f, v_layout[8], session);
        }
    }

    // 10. Footer key hints
    let has_scrollable_output = if is_drilled {
        drilled_session_id
            .as_ref()
            .and_then(|sid| state.session_tracker.cached_streaming_lines.get(sid))
            .map(|(_, lines)| lines.len() > v_layout[7].height as usize)
            .unwrap_or(false)
    } else {
        state
            .session_tracker.cached_streaming_lines
            .get(task_id)
            .map(|(_, lines)| lines.len() > v_layout[7].height as usize)
            .unwrap_or(false)
    };
    render_footer(f, v_layout[9], has_scrollable_output, is_drilled, editor_is_focused, editor_discard_warning);
}

// ─── Section Renderers ────────────────────────────────────────────────────

/// Render a dim horizontal separator line.
fn render_separator(f: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let line = "─".repeat(width);
    let para = Paragraph::new(Span::styled(
        line,
        Style::default().fg(Color::Rgb(50, 54, 70)),
    ));
    f.render_widget(para, area);
}

/// Render the two-row metadata header.
///
/// Row 1: Task number badge + Title + status pill (right-aligned)
/// Row 2: Agent type icon + Column name + Timer + Permission indicators
fn render_metadata_header(
    f: &mut Frame,
    area: Rect,
    task: &CortexTask,
    theme: &crate::config::types::ThemeConfig,
    now: i64,
) {
    if area.height < 2 || area.width == 0 {
        return;
    }

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

    let elapsed = format_elapsed_time(
        task.entered_column_at,
        if task.agent_status.is_terminal() {
            task.last_activity_at
        } else {
            now
        },
    );
    let agent_name = task.agent_type.as_deref().unwrap_or("none");

    // ── Row 1: Badge + Title + Status pill ────────────────────────────
    let badge = format!("#{}", task.number);
    let display_title = crate::state::types::derive_title_from_description(&task.description);

    // Build left-aligned content: badge + title
    let left_spans = vec![
        Span::styled(
            badge,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            display_title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Build right-aligned status pill
    // For running tasks, add a spinning progress indicator
    let spinner = if task.agent_status == AgentStatus::Running {
        let spin_chars = ["◐", "◓", "◑", "◒"];
        let idx = (now as usize / 250) % spin_chars.len();
        format!("{} ", spin_chars[idx])
    } else {
        String::new()
    };

    let status_pill = format!("{}{} {}", spinner, status_icon, status_text);
    let right_spans = vec![Span::styled(
        status_pill,
        Style::default().fg(status_color),
    )];

    // Render row 1 with left/right alignment
    let row1_area = Rect::new(area.x, area.y, area.width, 1);
    render_split_line(f, row1_area, left_spans, right_spans);

    // ── Row 2: Agent + Column + Timer + Indicators ────────────────────
    let agent_icon = match agent_name {
        "planning" => "📋",
        "do" => "⚡",
        "reviewer-alpha" => "🔍",
        "reviewer-beta" => "🔍",
        "reviewer-gamma" => "🔍",
        "explore" => "🔎",
        _ => "🤖",
    };

    let elapsed_display = if elapsed.is_empty() {
        "—".to_string()
    } else {
        elapsed
    };

    let mut row2_spans: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", agent_icon),
            Style::default().fg(Color::White),
        ),
        Span::styled(agent_name, Style::default().fg(Color::Yellow)),
        Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 64, 80))),
        Span::styled("Column: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            task.column.to_string(),
            Style::default().fg(Color::Rgb(150, 150, 180)),
        ),
        Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 64, 80))),
        Span::styled("⏱ ", Style::default().fg(Color::DarkGray)),
        Span::styled(elapsed_display, Style::default().fg(Color::White)),
    ];

    // Permission/question indicators on the right
    if task.pending_permission_count > 0 {
        row2_spans.push(Span::raw("  "));
        row2_spans.push(Span::styled(
            format!("!{}", task.pending_permission_count),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if task.pending_question_count > 0 {
        row2_spans.push(Span::raw("  "));
        row2_spans.push(Span::styled(
            format!("?{}", task.pending_question_count),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Error indicator
    if task.agent_status == AgentStatus::Error {
        row2_spans.push(Span::raw("  "));
        row2_spans.push(Span::styled(
            "✗ error",
            Style::default()
                .fg(theme.error_color())
                .add_modifier(Modifier::BOLD),
        ));
    }

    let row2_area = Rect::new(area.x, area.y + 1, area.width, 1);
    let para = Paragraph::new(Line::from(row2_spans));
    f.render_widget(para, row2_area);
}

/// Render a line with left-aligned and right-aligned content.
fn render_split_line(
    f: &mut Frame,
    area: Rect,
    left_spans: Vec<Span<'_>>,
    right_spans: Vec<Span<'_>>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Calculate total width needed for both sides
    let left_width: usize = left_spans.iter().map(|s| s.width()).sum();
    let right_width: usize = right_spans.iter().map(|s| s.width()).sum();
    let total_needed = left_width + right_width;

    let total_width = area.width as usize;

    let mut spans = left_spans;
    if total_needed < total_width {
        let gap = total_width - total_needed;
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.extend(right_spans);

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// Render the description block with styled header and character count.
/// When the detail editor is focused, renders as an editable textarea with cursor.
fn render_description_block(f: &mut Frame, area: Rect, task_id: &str, state: &mut AppState) {
    let (display_desc, char_count, line_count, has_pending) = {
        let task = state.tasks.get(task_id);
        let has_pending = task.map_or(false, |t| t.pending_description.is_some());
        if let Some(ref editor) = state.ui.detail_editor {
            let desc = editor.description();
            let cc = desc.chars().count();
            let lc = desc.lines().count();
            (Some(desc), cc, lc, has_pending)
        } else if let Some(task) = task {
            let cc = task.description.chars().count();
            let lc = task.description.lines().count();
            (Some(task.description.clone()), cc, lc, has_pending)
        } else {
            (None, 0, 0, false)
        }
    };

    let display_desc = match display_desc {
        Some(d) => d,
        None => return,
    };

    let editor = state.ui.detail_editor.as_mut();
    let is_focused = editor.map_or(false, |e| e.is_focused);

    // Build header label
    let mut header_label = if display_desc.is_empty() {
        " Description ".to_string()
    } else if line_count > 1 {
        format!(" Description ({} chars, {} lines) ", char_count, line_count)
    } else {
        format!(" Description ({} chars) ", char_count)
    };

    // Append status indicators to the header
    if is_focused {
        header_label.push_str("[EDITING] ");
    } else if has_pending {
        header_label.push_str("[⧖ pending] ");
    }

    let border_color = if is_focused {
        Color::Cyan
    } else if has_pending {
        Color::Rgb(200, 170, 50) // Yellow/orange for pending
    } else {
        Color::Rgb(70, 74, 90)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            header_label,
            Style::default()
                .fg(if is_focused { Color::Cyan } else { Color::Rgb(130, 134, 160) })
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    if is_focused {
        // ── Editable textarea mode ──
        if let Some(ref mut editor) = state.ui.detail_editor {
            let visible_height = inner.height as usize;
            editor.ensure_cursor_visible(visible_height);

            let desc_lines = editor.desc_lines();
            let desc_is_empty = desc_lines.len() == 1 && desc_lines[0].is_empty();

            let lines: Vec<Line> = if desc_is_empty {
                vec![Line::from(Span::styled(
                    "Enter description...",
                    Style::default().fg(Color::Rgb(100, 100, 120)),
                ))]
            } else {
                desc_lines
                    .iter()
                    .skip(editor.scroll_offset)
                    .take(visible_height)
                    .map(|s| Line::from(s.as_str()))
                    .collect()
            };

            // Add cursor character
            let display_lines = if !desc_is_empty {
                let cursor_row = editor.cursor_row;
                let actual_visible_row = cursor_row.saturating_sub(editor.scroll_offset);

                let mut result = lines;
                if actual_visible_row < result.len() {
                    let line = desc_lines.get(cursor_row).map_or("", |l| l.as_str());
                    let col = editor.cursor_col.min(line.chars().count());
                    let mut chars: Vec<char> = line.chars().collect();
                    chars.insert(col, '▊');
                    let modified_line: String = chars.into_iter().collect();
                    result[actual_visible_row] = Line::from(modified_line);
                } else if actual_visible_row == result.len() && result.len() < visible_height {
                    result.push(Line::from("▊"));
                }
                result
            } else {
                // Show cursor on the placeholder line
                let mut result = lines;
                result[0] = Line::from("▊");
                result
            };

            let desc_para = Paragraph::new(display_lines).wrap(Wrap { trim: false });
            f.render_widget(desc_para, inner);

            // Set cursor position
            let line = desc_lines.get(editor.cursor_row).map_or("", |l| l.as_str());
            let cursor_x = inner.x + editor.cursor_col.min(line.chars().count()) as u16;
            let cursor_y = inner.y + (editor.cursor_row - editor.scroll_offset) as u16;
            if cursor_y < inner.y + inner.height {
                f.set_cursor_position((cursor_x, cursor_y));
            }
        }
    } else {
        // ── Read-only mode ──
        if display_desc.is_empty() {
            let placeholder = Paragraph::new(Span::styled(
                "  (no description)",
                Style::default()
                    .fg(Color::Rgb(100, 100, 120))
                    .add_modifier(Modifier::ITALIC),
            ));
            f.render_widget(placeholder, inner);

            // Show "Tab to edit" hint on the right
            let hint = Paragraph::new(Span::styled(
                "Tab to edit ",
                Style::default().fg(Color::Rgb(80, 84, 100)),
            ))
            .alignment(Alignment::Right);
            f.render_widget(hint, inner);
        } else {
            let para = Paragraph::new(display_desc.as_str())
                .style(Style::default().fg(Color::White))
                .wrap(Wrap { trim: true });
            f.render_widget(para, inner);
        }
    }

    // Show validation error if present
    if let Some(ref editor) = state.ui.detail_editor {
        if let Some(ref error) = editor.validation_error {
            let error_widget = Paragraph::new(Span::styled(
                format!(" ⚠ {}", error),
                Style::default().fg(Color::Red),
            ));
            let error_area = Rect {
                x: area.x + 1,
                y: area.y + area.height,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            if error_area.y < f.area().y + f.area().height {
                f.render_widget(error_widget, error_area);
            }
        }
    }
}

/// Render the plan output section.
///
/// Displays the task's plan_output in a bordered block with a styled header.
fn render_plan_section(f: &mut Frame, area: Rect, plan: &str) {
    let line_count = plan.lines().count();
    let header = format!(" Plan ({} lines) ", line_count);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(80, 100, 140)))
        .title(Span::styled(
            header,
            Style::default()
                .fg(Color::Rgb(120, 160, 220))
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Build plan lines with line numbers
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, line) in plan.lines().enumerate() {
        let line_num = format!("{:>2} ", i + 1);
        lines.push(Line::from(vec![
            Span::styled(line_num, Style::default().fg(Color::Rgb(70, 74, 90))),
            Span::styled(line.to_string(), Style::default().fg(Color::Rgb(180, 190, 210))),
        ]));
        if lines.len() >= inner.height as usize {
            break;
        }
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, inner);
}

/// Render the error section with red-tinted border and background.
fn render_error_section(f: &mut Frame, area: Rect, error_msg: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(180, 50, 50)))
        .title(Span::styled(
            " ✗ Error ",
            Style::default()
                .fg(Color::Rgb(220, 80, 80))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(40, 20, 20)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Truncate error message to fit
    let max_chars = (inner.width as usize) * (inner.height as usize);
    let truncated: String = error_msg.chars().take(max_chars).collect();
    let truncated = if error_msg.len() > max_chars {
        format!("{}...", truncated)
    } else {
        truncated
    };

    let para = Paragraph::new(truncated.as_str())
        .style(Style::default().fg(Color::Rgb(220, 120, 120)))
        .wrap(Wrap { trim: true });
    f.render_widget(para, inner);
}

/// Render the streaming output block with messages.
///
/// Uses a render version cache on `AppState` to avoid rebuilding `Vec<Line>`
/// on every frame when the session data hasn't changed.
fn render_streaming_block(f: &mut Frame, area: Rect, state: &mut AppState, task_id: &str, now: i64) {
    // ── Pre-compute scroll metrics for block title ──────────────────
    let total_lines = state
        .session_tracker
        .cached_streaming_lines
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

    // ── Count messages for block title ────────────────────────────────
    let msg_count = state
        .session_tracker
        .task_sessions
        .get(task_id)
        .map(|s| s.messages.len())
        .unwrap_or(0);

    // ── Block title with scroll position and message count ────────────
    let has_session = state.session_tracker.task_sessions.contains_key(task_id);
    let block_title = if can_scroll && has_session {
        let first_visible = scroll_offset + 1;
        let last_visible = (scroll_offset + visible_height).min(total_lines);
        let at_bottom = scroll_offset + visible_height >= total_lines;
        if at_bottom {
            format!(" Agent Output ({} msgs) ▼ {}/{} ", msg_count, last_visible, total_lines)
        } else {
            format!(
                " Agent Output ({} msgs) ║ {}-{}/{} ",
                msg_count, first_visible, last_visible, total_lines
            )
        }
    } else if has_session && msg_count > 0 {
        format!(" Agent Output ({} msgs) ", msg_count)
    } else {
        " Agent Output ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(70, 74, 90)))
        .title(Span::styled(
            block_title,
            Style::default()
                .fg(Color::Rgb(130, 134, 160))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(24, 26, 36)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Early return if no session data
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
        .session_tracker
        .task_sessions
        .get(task_id)
        .map(|s| s.render_version)
        .unwrap_or(0);

    // Check render cache: only rebuild lines when the session version changes.
    let cached_version = state
        .session_tracker
        .cached_streaming_lines
        .get(task_id)
        .map(|(v, _)| *v)
        .unwrap_or(0);

    let lines: Vec<Line<'static>> = if current_version == cached_version {
        // Cache hit — reuse previously built lines
        state
            .session_tracker
            .cached_streaming_lines
            .get(task_id)
            .map(|(_, lines)| lines.clone())
            .unwrap_or_default()
    } else {
        // Cache miss — re-borrow session (immutable), build lines, then
        // write to cache (mutable). These borrows are sequential, not
        // simultaneous, so the borrow checker is happy.
        let built = state
            .session_tracker
            .task_sessions
            .get(task_id)
            .map(|s| build_streaming_lines(s, now))
            .unwrap_or_default();
        state
            .session_tracker
            .cached_streaming_lines
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
            let indicator_area = Rect::new(x, y, label_width, 1);
            let indicator = Paragraph::new(Span::styled(
                manual_label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            f.render_widget(indicator, indicator_area);
        }
    }
}

/// Build the streaming output lines from session messages and streaming text.
///
/// Enhanced with:
/// - Colored role headers (You / Assistant) with accent bars
/// - Tool-type-specific colors and 2-line formatting
/// - Relative timestamps on role headers
/// - Streaming accent bar with inline cursor
/// - Reasoning blocks with accent bar and bold header
/// - Brighter step section headers
/// - Subagent indicators with accent bar
/// - Basic markdown rendering (bold, inline code, code fences)
fn build_streaming_lines(session: &TaskDetailSession, now: i64) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut prev_role: Option<MessageRole> = None;
    let mut in_code_fence = false;

    for (msg_idx, msg) in session.messages.iter().enumerate() {
        // ── Emit role header when role changes ─────────────────────
        if prev_role.as_ref() != Some(&msg.role) || msg_idx == 0 {
            // Empty line separator between role groups (not before first)
            if prev_role.is_some() {
                lines.push(Line::from(""));
            }

            let (label, accent) = match msg.role {
                MessageRole::User => ("You", Color::Rgb(80, 200, 255)),
                MessageRole::Assistant => ("Assistant", Color::Rgb(120, 220, 160)),
            };

            let timestamp = msg
                .created_at
                .as_ref()
                .map(|t| format_relative_time(t, now));

            let mut header_spans: Vec<Span<'static>> = vec![
                Span::styled(" ▎ ", Style::default().fg(accent)),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " ──────────────────────────────────────",
                    Style::default().fg(Color::Rgb(45, 48, 62)),
                ),
            ];

            if let Some(ts) = timestamp {
                header_spans.push(Span::styled(
                    format!(" {}", ts),
                    Style::default().fg(Color::Rgb(100, 105, 125)),
                ));
            }

            lines.push(Line::from(header_spans));
        }

        for part in &msg.parts {
            match part {
                TaskMessagePart::Text { text } => {
                    let accent = match msg.role {
                        MessageRole::User => Color::Rgb(80, 200, 255),
                        MessageRole::Assistant => Color::Rgb(120, 220, 160),
                    };

                    for line in text.lines() {
                        let md_spans = render_markdown_line(line, &mut in_code_fence);
                        let mut spans = vec![Span::styled(" ▎ ", Style::default().fg(accent))];
                        spans.extend(md_spans);
                        lines.push(Line::from(spans));
                    }
                }
                TaskMessagePart::Tool {
                    tool,
                    state: tool_state,
                    cached_summary,
                    error,
                    ..
                } => {
                    let (state_icon, state_color, bar_color) = match tool_state {
                        ToolState::Pending => ("○", Color::Rgb(100, 100, 120), Color::Rgb(60, 60, 70)),
                        ToolState::Running => ("◐", Color::Blue, Color::Rgb(50, 100, 200)),
                        ToolState::Completed => ("✓", Color::Green, Color::Rgb(60, 160, 80)),
                        ToolState::Error => ("✗", Color::Red, Color::Rgb(180, 50, 50)),
                    };

                    let tool_name_color = tool_accent_color(tool);

                    let status_label = match tool_state {
                        ToolState::Pending => " pending",
                        ToolState::Running => " running",
                        ToolState::Completed => " done",
                        ToolState::Error => " error",
                    };

                    // Header line: accent bar + state icon + tool name + status
                    lines.push(Line::from(vec![
                        Span::styled(" ▎ ", Style::default().fg(bar_color)),
                        Span::styled(format!("{} ", state_icon), Style::default().fg(state_color)),
                        Span::styled(
                            tool.to_string(),
                            Style::default()
                                .fg(tool_name_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            status_label.to_string(),
                            Style::default().fg(Color::Rgb(80, 83, 100)),
                        ),
                    ]));

                    // Content line: tree branch + summary
                    if let Some(ref summary) = cached_summary {
                        lines.push(Line::from(vec![
                            Span::styled("   ", Style::default()),
                            Span::styled("│ ", Style::default().fg(bar_color)),
                            Span::styled(
                                summary.to_string(),
                                Style::default().fg(Color::Rgb(170, 173, 190)),
                            ),
                        ]));
                    }

                    // Error lines
                    if let Some(err) = error {
                        for err_line in err.lines().take(3) {
                            lines.push(Line::from(vec![
                                Span::styled("   ", Style::default()),
                                Span::styled("│ ", Style::default().fg(Color::Rgb(180, 50, 50))),
                                Span::styled(
                                    err_line.to_string(),
                                    Style::default().fg(Color::Red),
                                ),
                            ]));
                        }
                    }
                }
                TaskMessagePart::StepStart { .. } => {
                    lines.push(Line::from(Span::styled(
                        " ━━━━ Step ─────────────────────────────",
                        Style::default().fg(Color::Rgb(70, 75, 95)),
                    )));
                }
                TaskMessagePart::StepFinish { .. } => {
                    lines.push(Line::from(Span::styled(
                        " ━━━━ Step Complete ────────────────────",
                        Style::default().fg(Color::Rgb(60, 100, 70)),
                    )));
                }
                TaskMessagePart::Reasoning { text } => {
                    let accent = Color::Rgb(100, 70, 140);
                    lines.push(Line::from(vec![
                        Span::styled(" ▎ ", Style::default().fg(accent)),
                        Span::styled(
                            "💭 Reasoning ──────────────────────────",
                            Style::default()
                                .fg(accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for line in text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled(" ▎ ", Style::default().fg(Color::Rgb(60, 45, 80))),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::Rgb(180, 140, 255)),
                            ),
                        ]));
                    }
                }
                TaskMessagePart::Agent { id: _, agent } => {
                    lines.push(Line::from(vec![
                        Span::styled(" ▎ ", Style::default().fg(Color::Rgb(200, 180, 60))),
                        Span::styled(
                            format!("▶ {} ", agent),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "[ctrl+x to drill in]",
                            Style::default().fg(Color::Rgb(100, 100, 120)),
                        ),
                    ]));
                }
                TaskMessagePart::Unknown => {}
            }
        }

        prev_role = Some(msg.role.clone());
    }

    // ── Streaming text (active assistant response) ───────────────
    if let Some(ref streaming) = session.streaming_text {
        if !streaming.is_empty() {
            let accent = Color::Rgb(60, 140, 220);

            // If no messages yet (or last message wasn't assistant), add header
            if prev_role.as_ref() != Some(&MessageRole::Assistant) {
                if prev_role.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(" ▎ ", Style::default().fg(Color::Rgb(120, 220, 160))),
                    Span::styled(
                        "Assistant".to_string(),
                        Style::default()
                            .fg(Color::Rgb(120, 220, 160))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        " ──────────────────────────────────────",
                        Style::default().fg(Color::Rgb(45, 48, 62)),
                    ),
                ]));
            }

            let stream_lines: Vec<&str> = streaming.lines().collect();
            let last_idx = stream_lines.len().saturating_sub(1);

            for (i, line) in stream_lines.iter().enumerate() {
                if i == last_idx {
                    // Append cursor inline to the last line
                    lines.push(Line::from(vec![
                        Span::styled(" ▎ ", Style::default().fg(accent)),
                        Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Rgb(220, 220, 230)),
                        ),
                        Span::styled("▊", Style::default().fg(Color::Cyan)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(" ▎ ", Style::default().fg(accent)),
                        Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Rgb(220, 220, 230)),
                        ),
                    ]));
                }
            }
        }
    }

    lines
}

/// Return a color for a tool name, used for visual differentiation.
fn tool_accent_color(tool: &str) -> Color {
    match tool {
        "bash" => Color::Rgb(255, 180, 80),
        "edit" | "write" => Color::Rgb(100, 200, 255),
        "read" | "view" => Color::Rgb(160, 220, 160),
        "grep" | "glob" => Color::Rgb(220, 180, 255),
        "patch" => Color::Rgb(255, 140, 140),
        _ => Color::Rgb(180, 190, 210),
    }
}

/// Format a relative time string from a `t{unix_seconds}` timestamp.
fn format_relative_time(created_at: &str, now: i64) -> String {
    let secs = match created_at.strip_prefix('t').and_then(|s| s.parse::<i64>().ok()) {
        Some(s) => s,
        None => return String::new(),
    };
    let diff = now.saturating_sub(secs);
    if diff < 5 {
        "just now".to_string()
    } else if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else {
        format!("{}h ago", diff / 3600)
    }
}

/// Render a single line of text with basic markdown-like formatting.
///
/// Supports:
/// - `**bold text**` → bold modifier
/// - `` `inline code` `` → warm yellow on dark background
/// - Code fences (`` ``` ``) → tracked via `in_code_fence` state
fn render_markdown_line(text: &str, in_code_fence: &mut bool) -> Vec<Span<'static>> {
    let trimmed = text.trim_start();

    // Code fence toggling
    if trimmed.starts_with("```") {
        *in_code_fence = !*in_code_fence;
        let fence_label = if *in_code_fence {
            let lang = trimmed.trim_start_matches('`').trim();
            if lang.is_empty() {
                "```"
            } else {
                trimmed
            }
        } else {
            "```"
        };
        return vec![Span::styled(
            fence_label.to_string(),
            Style::default().fg(Color::Rgb(80, 80, 100)),
        )];
    }

    // Inside a code fence — render as plain monospace text
    if *in_code_fence {
        return vec![Span::styled(
            text.to_string(),
            Style::default().fg(Color::Rgb(200, 200, 210)),
        )];
    }

    // Parse inline formatting: **bold** and `code`
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = text;
    let text_color = Color::Rgb(220, 220, 230);

    while !remaining.is_empty() {
        // Try to match **bold**
        if let Some(rest) = remaining.strip_prefix("**") {
            if let Some(end) = rest.find("**") {
                spans.push(Span::styled(
                    rest[..end].to_string(),
                    Style::default().fg(text_color).add_modifier(Modifier::BOLD),
                ));
                remaining = &rest[end + 2..];
                continue;
            }
        }

        // Try to match `code`
        if let Some(rest) = remaining.strip_prefix('`') {
            if let Some(end) = rest.find('`') {
                spans.push(Span::styled(
                    format!(" {} ", &rest[..end]),
                    Style::default()
                        .fg(Color::Rgb(255, 200, 100))
                        .bg(Color::Rgb(40, 40, 50)),
                ));
                remaining = &rest[end + 1..];
                continue;
            }
        }

        // Regular text — advance to next special token
        let next_bold = remaining.find("**");
        let next_code = remaining.find('`');
        let end = match (next_bold, next_code) {
            (Some(b), Some(c)) => b.min(c),
            (Some(b), None) => b,
            (None, Some(c)) => c,
            (None, None) => remaining.len(),
        };

        if end > 0 {
            spans.push(Span::styled(
                remaining[..end].to_string(),
                Style::default().fg(text_color),
            ));
        }
        remaining = &remaining[end..];
    }

    if spans.is_empty() {
        spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(text_color),
        ));
    }

    spans
}

/// Render a compact summary of subagent sessions for a parent task.
///
/// Shows each subagent's agent name, status (active/error/done), and
/// error message if applicable. Displayed in a bordered block with a
/// done count in the header.
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

    let done_count = subagents.iter().filter(|s| !s.active && s.error_message.is_none()).count();
    let error_count = subagents.iter().filter(|s| s.error_message.is_some()).count();

    let header = format!(
        " Subagents ({}/{}) ",
        done_count,
        subagents.len()
    );
    let header_style = if error_count > 0 {
        Style::default()
            .fg(theme.error_color())
            .add_modifier(Modifier::BOLD)
    } else if done_count == subagents.len() {
        Style::default()
            .fg(theme.done_color())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Rgb(130, 134, 160))
            .add_modifier(Modifier::BOLD)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(70, 74, 90)))
        .title(Span::styled(header, header_style));

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

            // Agent type icon
            let agent_icon = match sub.agent_name.as_str() {
                "planning" => "📋",
                "do" => "⚡",
                "reviewer-alpha" | "reviewer-beta" | "reviewer-gamma" => "🔍",
                "explore" => "🔎",
                _ => "🤖",
            };

            let label = format!(" {} {} {} (depth {})", icon, agent_icon, sub.agent_name, sub.depth);
            let mut spans = vec![Span::styled(label, Style::default().fg(color))];
            if let Some(ref err) = sub.error_message {
                let truncated: String = err.chars().take(50).collect();
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
fn render_subagent_streaming_block(f: &mut Frame, area: Rect, state: &mut AppState, now: i64) {
    let session_id = match state.get_drilldown_session_id() {
        Some(sid) => sid.to_string(),
        None => return,
    };

    // Pre-compute scroll metrics
    let total_lines = state
        .session_tracker
        .cached_streaming_lines
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

    // Count messages for block title
    let msg_count = state
        .session_tracker
        .subagent_session_data
        .get(&session_id)
        .map(|s| s.messages.len())
        .unwrap_or(0);

    // Block title with scroll indicator and message count
    let block_title = if can_scroll {
        let first_visible = scroll_offset + 1;
        let last_visible = (scroll_offset + visible_height).min(total_lines);
        let at_bottom = scroll_offset + visible_height >= total_lines;
        if at_bottom {
            format!(
                " Subagent Output ({} msgs) ▼ {}/{} ",
                msg_count, last_visible, total_lines
            )
        } else {
            format!(
                " Subagent Output ({} msgs) ║ {}-{}/{} ",
                msg_count, first_visible, last_visible, total_lines
            )
        }
    } else if msg_count > 0 {
        format!(" Subagent Output ({} msgs) ", msg_count)
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
        ))
        .style(Style::default().bg(Color::Rgb(24, 26, 36)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Check render cache
    let current_version = state
        .session_tracker
        .subagent_session_data
        .get(&session_id)
        .map(|s| s.render_version)
        .unwrap_or(0);

    let cached_version = state
        .session_tracker
        .cached_streaming_lines
        .get(&session_id)
        .map(|(v, _)| *v)
        .unwrap_or(0);

    let lines: Vec<Line<'static>> = if current_version == cached_version {
        state
            .session_tracker
            .cached_streaming_lines
            .get(&session_id)
            .map(|(_, lines)| lines.clone())
            .unwrap_or_default()
    } else {
        let built = state
            .session_tracker
            .subagent_session_data
            .get(&session_id)
            .map(|s| build_streaming_lines(s, now))
            .unwrap_or_default();
        state
            .session_tracker
            .cached_streaming_lines
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
            let indicator_area = Rect::new(x, y, label_width, 1);
            let indicator = Paragraph::new(Span::styled(
                manual_label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            f.render_widget(indicator, indicator_area);
        }
    }
}

/// Render pending permissions and questions in a bordered block.
///
/// Shows permission details with tool name, description, and action hints.
fn render_permissions(f: &mut Frame, area: Rect, session: &TaskDetailSession) {
    if area.height < 3 || area.width == 0 {
        return;
    }

    let has_perms = !session.pending_permissions.is_empty();

    let (border_color, title_text) = if has_perms {
        (
            Color::Rgb(180, 80, 50),
            format!(
                " ⚠ Pending ({} perm, {} question{}) ",
                session.pending_permissions.len(),
                session.pending_questions.len(),
                if session.pending_questions.len() != 1 { "s" } else { "" }
            ),
        )
    } else {
        (
            Color::Rgb(160, 140, 50),
            format!(
                " ? Questions ({}) ",
                session.pending_questions.len()
            ),
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title_text,
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Build permission and question lines
    let mut content_lines: Vec<Line<'_>> = Vec::new();

    for perm in &session.pending_permissions {
        // Row 1: Tool name + permission ID
        content_lines.push(Line::from(vec![
            Span::styled(
                format!(" !{} ", perm.id),
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                perm.tool_name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        // Row 2: Description + action hints
        content_lines.push(Line::from(vec![
            Span::styled(
                format!("   {} ", perm.description),
                Style::default().fg(Color::Rgb(200, 200, 210)),
            ),
            Span::styled(
                " [",
                Style::default().fg(Color::Rgb(80, 84, 100)),
            ),
            Span::styled(
                "y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ":approve",
                Style::default().fg(Color::Rgb(80, 84, 100)),
            ),
            Span::styled(
                " / ",
                Style::default().fg(Color::Rgb(60, 64, 80)),
            ),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ":reject",
                Style::default().fg(Color::Rgb(80, 84, 100)),
            ),
            Span::styled(
                "]",
                Style::default().fg(Color::Rgb(80, 84, 100)),
            ),
        ]));
    }

    for question in &session.pending_questions {
        // Row 1: Question label
        content_lines.push(Line::from(vec![
            Span::styled(
                " ? ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                question.question.clone(),
                Style::default().fg(Color::White),
            ),
        ]));
        // Row 2: Answer options
        if !question.answers.is_empty() {
            let mut spans: Vec<Span> = vec![Span::styled(
                "   Answers: ".to_string(),
                Style::default().fg(Color::DarkGray),
            )];
            for (i, answer) in question.answers.iter().enumerate() {
                let key = format!("{}", i + 1);
                spans.push(Span::styled(
                    "[",
                    Style::default().fg(Color::Rgb(80, 84, 100)),
                ));
                spans.push(Span::styled(
                    key,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("]",),
                    Style::default().fg(Color::Rgb(80, 84, 100)),
                ));
                spans.push(Span::styled(
                    format!("{} ", answer),
                    Style::default().fg(Color::White),
                ));
            }
            content_lines.push(Line::from(spans));
        }
    }

    if content_lines.is_empty() {
        return;
    }

    // Truncate to available height
    let max_lines = inner.height as usize;
    content_lines.truncate(max_lines);

    let paragraph = Paragraph::new(content_lines);
    f.render_widget(paragraph, inner);
}

/// Render the footer with grouped key hints.
///
/// Groups hints into Navigation, Actions, and General categories
/// separated by dim pipe characters.
fn render_footer(f: &mut Frame, area: Rect, has_scrollable_output: bool, is_drilled: bool, editor_focused: bool, editor_discard_warning: bool) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let key = Style::default()
        .fg(Color::Rgb(140, 144, 170))
        .add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(Color::Rgb(90, 94, 110));
    let pipe = Style::default().fg(Color::Rgb(45, 48, 62));
    let warn = Style::default().fg(Color::Rgb(200, 170, 50));

    let mut groups: Vec<Vec<Span<'_>>> = Vec::new();

    if editor_focused {
        // Editor-focused footer hints
        if editor_discard_warning {
            groups.push(vec![
                Span::styled("Esc", key),
                Span::styled(" confirm discard  ", warn),
                Span::styled("ctrl+s", key),
                Span::styled(" save  ", desc),
                Span::styled("Tab", key),
                Span::styled(" unfocus", desc),
            ]);
        } else {
            groups.push(vec![
                Span::styled("ctrl+s", key),
                Span::styled(" save  ", desc),
                Span::styled("Esc", key),
                Span::styled(" unfocus  ", desc),
                Span::styled("Tab", key),
                Span::styled(" unfocus", desc),
            ]);
        }
    } else {
        // Normal footer hints
        // Navigation group
        if has_scrollable_output {
            groups.push(vec![
                Span::styled("↑↓", key),
                Span::styled("/", pipe),
                Span::styled("j/k", key),
                Span::styled(" scroll  ", desc),
                Span::styled("G", key),
                Span::styled(" bottom  ", desc),
                Span::styled("g", key),
                Span::styled(" top", desc),
            ]);
        }

        // Edit description group
        groups.push(vec![
            Span::styled("Tab", key),
            Span::styled(" edit description", desc),
        ]);

        // Actions group
        let drill_label = if is_drilled {
            "ctrl+x drill deeper"
        } else {
            "ctrl+x drill into subagent"
        };
        groups.push(vec![
            Span::styled("ctrl+x", key),
            Span::styled(" ", desc),
            Span::styled(drill_label, desc),
        ]);

        // Approve/reject group
        groups.push(vec![
            Span::styled("y", key),
            Span::styled(" approve  ", desc),
            Span::styled("n", key),
            Span::styled(" reject", desc),
        ]);

        // General group
        groups.push(vec![
            Span::styled("Esc", key),
            Span::styled(" back", desc),
        ]);
    }

    // Build the final spans with pipe separators between groups
    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", pipe));
        }
        spans.extend(group.iter().cloned());
    }

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}
