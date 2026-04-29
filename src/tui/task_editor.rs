//! Fullscreen task editor renderer.

use crate::config::types::CortexConfig;
use crate::state::types::{AppState, EditorField};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use super::help::format_combo;

/// Render the fullscreen task editor.
pub fn render_task_editor(f: &mut Frame, state: &AppState, config: &CortexConfig) {
    let editor = match &state.ui.task_editor {
        Some(e) => e,
        None => return,
    };

    let area = f.area();

    // Outer container
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Rgb(36, 40, 56)));

    let outer_inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    // Vertical layout: header | description label + textarea | column selector | footer
    let show_column_selector = !editor.available_columns.is_empty();
    let v_constraints = if show_column_selector {
        vec![
            Constraint::Length(1), // Optional header (for edit mode)
            Constraint::Length(1), // Description label
            Constraint::Min(0),    // Description textarea
            Constraint::Length(2), // Column selector (label + pills)
            Constraint::Length(1), // Footer hint
        ]
    } else {
        vec![
            Constraint::Length(1), // Optional header (for edit mode)
            Constraint::Length(1), // Description label
            Constraint::Min(0),    // Description textarea
            Constraint::Length(1), // Footer hint
        ]
    };
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(outer_inner);

    let x_margin = 2; // Horizontal margin inside the outer block

    // Header (edit mode indicator)
    if let Some(ref task_id) = editor.task_id {
        if let Some(task) = state.tasks.get(task_id) {
            let display_title =
                crate::state::types::derive_title_from_description(&task.description);
            let header_text = format!("[Editing #{}] {}", task.number, display_title);
            let header = Paragraph::new(Span::styled(
                header_text,
                Style::default().fg(Color::DarkGray),
            ));
            f.render_widget(header, v_layout[0]);
        }
    }

    // Description label
    let desc_label = Paragraph::new(Span::styled(
        "Description:",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        desc_label,
        Rect {
            x: v_layout[1].x + x_margin,
            y: v_layout[1].y,
            width: v_layout[1].width.saturating_sub(x_margin * 2),
            height: 1,
        },
    );

    // Description textarea
    let desc_focused = editor.focused_field == EditorField::Description;
    let desc_border_color = if desc_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let desc_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(desc_border_color));
    let desc_inner = desc_block.inner(v_layout[2]);
    f.render_widget(desc_block, v_layout[2]);

    // Render description text with scroll (uses pre-computed lines, no split per frame)
    let visible_height = desc_inner.height as usize;
    let desc_lines = editor.desc_lines();
    let desc_is_empty = desc_lines.len() == 1 && desc_lines[0].is_empty();
    let lines: Vec<Line> = if desc_is_empty {
        vec![Line::from(Span::styled(
            "Enter description...",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        desc_lines
            .iter()
            .skip(editor.scroll_offset)
            .take(visible_height)
            .map(|s| Line::from(s.as_str()))
            .collect()
    };

    // Add cursor character if focused and content exists
    let display_lines = if desc_focused && !desc_is_empty {
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
        lines
    };

    let desc_para = Paragraph::new(display_lines).wrap(Wrap { trim: false });
    f.render_widget(desc_para, desc_inner);

    // Set cursor position for description field
    if desc_focused {
        let line = desc_lines.get(editor.cursor_row).map_or("", |l| l.as_str());
        let cursor_x = desc_inner.x + editor.cursor_col.min(line.chars().count()) as u16;
        let cursor_y = desc_inner.y + (editor.cursor_row - editor.scroll_offset) as u16;
        if cursor_y < desc_inner.y + desc_inner.height {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // Column selector (only shown in create mode when columns are available)
    if show_column_selector {
        render_column_selector(f, editor, config, v_layout[3], x_margin);
    }

    // Validation error (shown in footer area when present)
    if let Some(ref error) = editor.validation_error {
        let footer_idx = if show_column_selector { 4 } else { 3 };
        let error_widget = Paragraph::new(Span::styled(
            format!("⚠ {}", error),
            Style::default().fg(Color::Red),
        ));
        f.render_widget(error_widget, v_layout[footer_idx]);
    }

    // Footer hint (only when no validation error — validation error replaces it)
    if editor.validation_error.is_none() {
        let ek = &config.keybindings.editor;
        let (footer_text, footer_style) = if editor.discard_warning_shown {
            (
                format!(
                    "Unsaved changes! Press {} again to discard or {} to save",
                    format_combo(&ek.cancel),
                    format_combo(&ek.save),
                ),
                Style::default().fg(Color::Yellow),
            )
        } else {
            (
                format!(
                    "{}: save  {}: cancel  {}: next field",
                    format_combo(&ek.save),
                    format_combo(&ek.cancel),
                    format_combo(&ek.cycle_field),
                ),
                Style::default().fg(Color::DarkGray),
            )
        };
        let footer_idx = if show_column_selector { 4 } else { 3 };
        let footer =
            Paragraph::new(Span::styled(footer_text, footer_style)).alignment(Alignment::Center);
        f.render_widget(footer, v_layout[footer_idx]);
    }
}

fn render_column_selector(
    f: &mut Frame,
    editor: &crate::state::types::TaskEditorState,
    config: &CortexConfig,
    area: Rect,
    x_margin: u16,
) {
    let col_focused = editor.focused_field == EditorField::Column;
    let label = Paragraph::new(Span::styled(
        "Column:",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        label,
        Rect {
            x: area.x + x_margin,
            y: area.y,
            width: area.width.saturating_sub(x_margin * 2),
            height: 1,
        },
    );
    let mut spans: Vec<Span> = Vec::new();
    for (i, col_id) in editor.available_columns.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let is_selected = Some(i)
            == editor
                .column_id
                .as_ref()
                .and_then(|cid| editor.available_columns.iter().position(|c| c == cid));
        let pill = format!(" {} ", config.columns.display_name_for(col_id));
        if is_selected {
            spans.push(Span::styled(
                pill,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                pill,
                Style::default()
                    .fg(if col_focused {
                        Color::White
                    } else {
                        Color::DarkGray
                    })
                    .bg(Color::Rgb(60, 64, 80)),
            ));
        }
    }
    if col_focused && editor.available_columns.len() > 1 {
        spans.push(Span::styled(
            "  Tab to cycle",
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: area.x + x_margin,
            y: area.y + 1,
            width: area.width.saturating_sub(x_margin * 2),
            height: 1,
        },
    );
}
