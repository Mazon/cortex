//! Permission/question modal overlay.
//!
//! Renders a centered modal on top of the task detail view when the agent
//! asks for permission or has a question. Shows full context and selectable
//! options navigable with arrow keys + Enter (or y/n / digit shortcuts).

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::state::types::AppState;

/// Render the permission/question modal overlay.
///
/// Draws a centered bordered block with:
/// - Title (Permission Request or Question) with a counter for stacked items
/// - Tool name / question text (full, wrapped)
/// - Description / details (full, wrapped)
/// - Selectable options list with a `▶` cursor indicator
/// - Footer with navigation hints
pub fn render_permission_modal(
    f: &mut Frame,
    area: Rect,
    state: &mut AppState,
    theme: &crate::config::types::ThemeConfig,
) {
    // Determine which task/session to read from
    let viewing_task_id = state.ui.viewing_task_id.clone();
    let drilled_session_id = state.get_drilldown_session_id().map(|s| s.to_string());

    // Gather pending permission/question data
    let (is_permission, tool_or_question, description_or_details, options, total_pending, _current_index) = {
        if let Some(ref sid) = drilled_session_id {
            // Permissions/questions for subagents are stored in the parent task's
            // session, not in subagent_session_data (see sse_processor.rs —
            // process_permission_asked always routes to the parent task).
            // Try subagent data first (for messages/output), then fall through
            // to parent task session for permissions/questions.
            let sub_session = state.session_tracker.subagent_session_data.get(sid);
            let result = gather_modal_data(sub_session, state.ui.permission_modal_selected_index);
            if result.1.is_none() {
                // No data in subagent session — fall through to parent task session
                let session = state
                    .ui
                    .viewing_task_id
                    .as_ref()
                    .and_then(|tid| state.session_tracker.task_sessions.get(tid));
                gather_modal_data(session, state.ui.permission_modal_selected_index)
            } else {
                result
            }
        } else if let Some(ref tid) = viewing_task_id {
            // Main task view
            let session = state.session_tracker.task_sessions.get(tid);
            gather_modal_data(session, state.ui.permission_modal_selected_index)
        } else {
            return;
        }
    };

    let Some(((tool_or_question, description_or_details), options)) =
        tool_or_question.zip(description_or_details).zip(Some(options))
    else {
        return;
    };

    // ── Compute modal dimensions ──────────────────────────────────────

    // Title row: border + padding
    // Tool/question row: 1 line (wrapped if needed)
    // Description/details: up to 4 lines (wrapped)
    // Options: 1 line per option
    // Footer: 1 line
    // Borders: top + bottom = 2
    // Padding: 1 top + 1 bottom between sections = 2
    let num_options = options.len().max(1);
    let content_lines = 1 /* tool/question */ + 4 /* description max */ + num_options + 1 /* footer */;
    let modal_height = (content_lines as u16 + 4) /* borders + padding */.min(area.height.saturating_sub(4));
    let modal_width = 60.min(area.width.saturating_sub(4));

    // Ensure minimum dimensions
    if modal_width < 30 || modal_height < 8 {
        return;
    }

    let popup = centered_rect(modal_width, modal_height, area);

    // ── Clear background ──────────────────────────────────────────────
    f.render_widget(Clear, popup);

    // ── Build title ───────────────────────────────────────────────────
    let (title_icon, title_label, border_color) = if is_permission {
        let counter = if total_pending > 1 {
            format!(" {} pending ", total_pending)
        } else {
            String::new()
        };
        (
            "⚠ ",
            format!(" Permission Request{} ", counter),
            theme.error_color(),
        )
    } else {
        let counter = if total_pending > 1 {
            format!(" {} pending ", total_pending)
        } else {
            String::new()
        };
        (
            "? ",
            format!(" Question{} ", counter),
            theme.question_color(),
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!("{}{}", title_icon, title_label),
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(30, 32, 44)));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // ── Build content lines ───────────────────────────────────────────
    let selected = state.ui.permission_modal_selected_index;
    let mut content_lines: Vec<Line<'_>> = Vec::new();

    // Tool name / question text
    content_lines.push(Line::from(vec![
        Span::styled(
            if is_permission { "Tool: " } else { "Q: " },
            Style::default()
                .fg(if is_permission {
                    theme.error_color()
                } else {
                    theme.question_color()
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            tool_or_question,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Empty line separator
    content_lines.push(Line::from(""));

    // Description / details (wrapped)
    if !description_or_details.is_empty() {
        for line in wrap_text(&description_or_details, inner.width as usize) {
            content_lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(Color::Rgb(200, 200, 215)),
            )));
        }
        // Empty line separator
        content_lines.push(Line::from(""));
    }

    // Options
    for (i, option_text) in options.iter().enumerate() {
        let is_selected = i == selected;
        let cursor = if is_selected { "▶ " } else { "  " };

        let (fg, bg) = if is_selected {
            match (is_permission, i) {
                // Permission: 0 = Yes (green), 1 = No (red)
                (true, 0) => (Color::Green, Color::Rgb(30, 50, 30)),
                (true, _) => (Color::Red, Color::Rgb(50, 30, 30)),
                // Question: all options in cyan when selected
                (false, _) => (Color::Cyan, Color::Rgb(30, 40, 55)),
            }
        } else {
            (Color::Rgb(160, 164, 180), Color::Rgb(30, 32, 44))
        };

        content_lines.push(Line::from(vec![
            Span::styled(cursor.to_string(), Style::default().fg(fg).bg(bg)),
            Span::styled(
                option_text.clone(),
                Style::default().fg(fg).bg(bg),
            ),
        ]));
    }

    // Empty line separator
    content_lines.push(Line::from(""));

    // Footer hints
    let hint_style = Style::default().fg(Color::Rgb(100, 104, 120));
    if is_permission {
        content_lines.push(Line::from(vec![
            Span::styled(" ↑↓", hint_style),
            Span::styled(" navigate  ", hint_style),
            Span::styled("Enter", hint_style),
            Span::styled(" select  ", hint_style),
            Span::styled("y", Style::default().fg(Color::Green)),
            Span::styled("/", hint_style),
            Span::styled("n", Style::default().fg(Color::Red)),
            Span::styled(" quick  ", hint_style),
            Span::styled("Esc", hint_style),
            Span::styled(" close", hint_style),
        ]));
    } else {
        content_lines.push(Line::from(vec![
            Span::styled(" ↑↓", hint_style),
            Span::styled(" navigate  ", hint_style),
            Span::styled("Enter", hint_style),
            Span::styled(" select  ", hint_style),
            Span::styled("1-9", Style::default().fg(Color::Cyan)),
            Span::styled(" quick  ", hint_style),
            Span::styled("Esc", hint_style),
            Span::styled(" close", hint_style),
        ]));
    }

    // Render content as a paragraph with wrapping disabled (we pre-wrapped)
    let paragraph = Paragraph::new(content_lines);
    f.render_widget(paragraph, inner);
}

/// Gather modal data from a session reference.
///
/// Returns `(is_permission, tool_or_question, description_or_details, options, total_pending, current_index)`.
fn gather_modal_data(
    session: Option<&crate::state::types::TaskDetailSession>,
    selected_index: usize,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Vec<String>,
    usize,
    usize,
) {
    let Some(session) = session else {
        return (false, None, None, Vec::new(), 0, 0);
    };

    let has_perms = !session.pending_permissions.is_empty();
    let has_questions = !session.pending_questions.is_empty();

    if has_perms {
        let total = session.pending_permissions.len();
        let idx = selected_index.min(total.saturating_sub(1));
        let perm = &session.pending_permissions[idx];
        let options = vec!["Yes (approve)".to_string(), "No (reject)".to_string()];
        (
            true,
            Some(perm.tool_name.clone()),
            Some(
                perm.details
                    .clone()
                    .unwrap_or_else(|| perm.description.clone()),
            ),
            options,
            total,
            idx,
        )
    } else if has_questions {
        let total = session.pending_questions.len();
        let idx = selected_index.min(total.saturating_sub(1));
        let question = &session.pending_questions[idx];
        let options: Vec<String> = question
            .answers
            .iter()
            .enumerate()
            .map(|(i, a)| format!("[{}] {}", i + 1, a))
            .collect();
        (
            false,
            Some(question.question.clone()),
            None,
            options,
            total,
            idx,
        )
    } else {
        (false, None, None, Vec::new(), 0, 0)
    }
}

/// Create a centered rectangle with fixed dimensions within the given area.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Simple word-wrapping for a text string to fit within `max_width` characters.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                // Word itself exceeds max_width — truncate with ellipsis
                if word.chars().count() > max_width {
                    let truncated: String =
                        word.chars().take(max_width.saturating_sub(1)).collect();
                    lines.push(format!("{}…", truncated));
                    current = String::new();
                } else {
                    current = word.to_string();
                }
            } else if current.len() + 1 + word.len() <= max_width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                // Handle oversized word on its own line
                if word.chars().count() > max_width {
                    let truncated: String =
                        word.chars().take(max_width.saturating_sub(1)).collect();
                    lines.push(format!("{}…", truncated));
                    current = String::new();
                } else {
                    current = word.to_string();
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}
