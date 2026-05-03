//! Inline config editor renderer — fullscreen TOML editor with line numbers.

use crate::config::types::CortexConfig;
use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

/// Width of the line-number gutter.
const LINE_NUM_WIDTH: u16 = 5;

/// Render the fullscreen config editor.
pub fn render_config_editor(f: &mut Frame, state: &AppState, _config: &CortexConfig) {
    let editor = match &state.ui.config_editor_state {
        Some(e) => e,
        None => return,
    };

    let area = f.area();

    // Outer container
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" Config Editor: {} ", editor.config_path.display()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let outer_inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    // Vertical layout: editor area | status bar
    let v_constraints = [
        Constraint::Min(0),   // Editor content
        Constraint::Length(1), // Status/error bar
        Constraint::Length(1), // Help bar
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(outer_inner);

    // ─── Editor area: line numbers + text ───
    let editor_visible_height = v_layout[0].height as usize;
    let editor_visible_width = v_layout[0].width.saturating_sub(LINE_NUM_WIDTH + 1) as usize;

    // Ensure cursor is visible
    let mut editor = editor.clone();
    editor.ensure_cursor_visible(editor_visible_height);

    // Build line-number + content lines
    let total_lines = editor.lines.len();
    let display_lines: Vec<Line> = editor
        .lines
        .iter()
        .enumerate()
        .skip(editor.scroll_offset)
        .take(editor_visible_height)
        .map(|(i, line)| {
            let line_num = i + 1;
            let num_str = format!("{:>4}", line_num);
            let is_cursor_line = i == editor.cursor_row;

            let num_style = if is_cursor_line {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Truncate long lines to fit the editor width
            let truncated: String = line.chars().take(editor_visible_width).collect();

            let mut spans = vec![
                Span::styled(format!("{} ", num_str), num_style),
                Span::raw(truncated),
            ];

            // Insert cursor indicator on the cursor line
            if is_cursor_line {
                let col = editor.cursor_col.min(line.chars().count());
                let mut chars: Vec<char> = line.chars().take(editor_visible_width).collect();
                chars.insert(col.min(chars.len()), '▊');
                let modified_line: String = chars.into_iter().collect();
                spans = vec![
                    Span::styled(format!("{} ", num_str), num_style),
                    Span::styled(modified_line, Style::default().fg(Color::White)),
                ];
            }

            Line::from(spans)
        })
        .collect();

    let editor_block = Block::default();
    let editor_inner = editor_block.inner(v_layout[0]);

    let editor_para = Paragraph::new(display_lines);
    f.render_widget(editor_para, editor_inner);

    // Set cursor position
    {
        let line = editor
            .lines
            .get(editor.cursor_row)
            .map_or("", |l| l.as_str());
        let cursor_x = editor_inner.x
            + LINE_NUM_WIDTH
            + editor.cursor_col.min(line.chars().count()) as u16;
        let cursor_y =
            editor_inner.y + (editor.cursor_row - editor.scroll_offset) as u16;
        if cursor_y < editor_inner.y + editor_inner.height
            && cursor_x < editor_inner.x + editor_inner.width
        {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // ─── Status bar ───
    let status_text = if let Some(ref error) = editor.validation_error {
        Span::styled(
            format!(" Error: {} ", error),
            Style::default().fg(Color::Black).bg(Color::Red),
        )
    } else if editor.has_unsaved_changes {
        Span::styled(
            format!(
                " Unsaved changes | Line {}/{} ",
                editor.cursor_row + 1,
                total_lines
            ),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    } else {
        Span::styled(
            format!(" Line {}/{} ", editor.cursor_row + 1, total_lines),
            Style::default().fg(Color::DarkGray).bg(Color::Rgb(60, 64, 80)),
        )
    };
    let status_para = Paragraph::new(status_text);
    f.render_widget(status_para, v_layout[1]);

    // ─── Help bar ───
    let help_text = "Ctrl+S: save & reload  Esc: cancel (discard)  Tab: indent  ↑↓←→: navigate";
    let help_para = Paragraph::new(Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(help_para, v_layout[2]);
}
