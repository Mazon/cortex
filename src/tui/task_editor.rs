//! Fullscreen task editor renderer.

use crate::state::types::{AppState, EditorField, TaskEditorState};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

/// Render the fullscreen task editor.
pub fn render_task_editor(f: &mut Frame, state: &AppState) {
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

    // Vertical layout: header | title label + input | spacer | description label + textarea | footer
    let v_constraints = [
        Constraint::Length(1), // Optional header (for edit mode)
        Constraint::Length(1), // Title label
        Constraint::Length(3), // Title input field
        Constraint::Length(1), // Spacer
        Constraint::Length(1), // Description label
        Constraint::Min(0),    // Description textarea
        Constraint::Length(1), // Footer hint
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(outer_inner);

    let x_margin = 2; // Horizontal margin inside the outer block

    // Header (edit mode indicator)
    if let Some(ref task_id) = editor.task_id {
        if let Some(task) = state.tasks.get(task_id) {
            let header_text = format!("[Editing #{}] {}", task.number, task.title);
            let header = Paragraph::new(Span::styled(
                header_text,
                Style::default().fg(Color::DarkGray),
            ));
            f.render_widget(header, v_layout[0]);
        }
    }

    // Title label
    let title_label = Paragraph::new(Span::styled(
        "Title:",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        title_label,
        Rect {
            x: v_layout[1].x + x_margin,
            y: v_layout[1].y,
            width: v_layout[1].width.saturating_sub(x_margin * 2),
            height: 1,
        },
    );

    // Title input field
    let title_focused = editor.focused_field == EditorField::Title;
    let title_border_color = if title_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(title_border_color));
    let title_inner = title_block.inner(v_layout[2]);
    f.render_widget(title_block, v_layout[2]);

    // Render title text with cursor
    let title_para = if title_focused && !editor.title.is_empty() {
        let col = editor.cursor_col.min(editor.title.len());
        Paragraph::new(format!("{}▊", editor.title))
    } else if title_focused {
        Paragraph::new("▊")
    } else if editor.title.is_empty() {
        Paragraph::new(Span::styled(
            "Enter title...",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Paragraph::new(editor.title.clone())
    };
    f.render_widget(title_para, title_inner);

    // Set cursor position for title field
    if title_focused {
        let cursor_x = title_inner.x + editor.cursor_col.min(editor.title.len()) as u16;
        let cursor_y = title_inner.y;
        f.set_cursor_position((cursor_x, cursor_y));
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
            x: v_layout[4].x + x_margin,
            y: v_layout[4].y,
            width: v_layout[4].width.saturating_sub(x_margin * 2),
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
    let desc_inner = desc_block.inner(v_layout[5]);
    f.render_widget(desc_block, v_layout[5]);

    // Render description text with scroll (uses pre-computed lines, no split per frame)
    let visible_height = desc_inner.height as usize;
    let desc_lines = editor.desc_lines();
    let lines: Vec<Line> = if desc_lines.is_empty() && !desc_focused {
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

    // Add cursor character if focused
    let display_lines = if desc_focused {
        let cursor_row = editor.cursor_row;
        let actual_visible_row = cursor_row.saturating_sub(editor.scroll_offset);

        let mut result = lines;
        if actual_visible_row < result.len() {
            let line = desc_lines.get(cursor_row).map_or("", |l| l.as_str());
            let col = editor.cursor_col.min(line.len());
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
        let cursor_x = desc_inner.x + editor.cursor_col.min(line.len()) as u16;
        let cursor_y = desc_inner.y + (editor.cursor_row - editor.scroll_offset) as u16;
        if cursor_y < desc_inner.y + desc_inner.height {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // Footer hint
    let (footer_text, footer_style) = if editor.discard_warning_shown {
        (
            "Unsaved changes! Press Esc again to discard or Ctrl+S to save",
            Style::default().fg(Color::Yellow),
        )
    } else {
        (
            "Ctrl+S: save  Esc: cancel  Tab: next field",
            Style::default().fg(Color::DarkGray),
        )
    };
    let footer =
        Paragraph::new(Span::styled(footer_text, footer_style)).alignment(Alignment::Center);
    f.render_widget(footer, v_layout[6]);
}
