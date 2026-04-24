//! Input prompt and confirmation dialog overlays.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Render a centered input prompt overlay on top of the current view.
///
/// Displays the prompt label and current input text with a visible cursor.
/// The user can type to edit and press Enter to confirm or Escape to cancel.
/// The title is derived from the prompt context (e.g., "Rename Project",
/// "Set Working Directory").
pub fn render_input_prompt(f: &mut Frame, state: &crate::state::types::AppState) {
    let area = centered_rect(50, 20, f.area());

    // Derive the dialog title from the prompt context
    let title = match state.ui.prompt_context.as_deref() {
        Some("rename_project") => " Rename Project ",
        Some("set_working_directory") => " Set Working Directory ",
        _ => " Input ",
    };

    // Clear the area behind the overlay
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(36, 40, 56)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Layout inside the block: label | input | hint
    let constraints = [
        Constraint::Length(1), // Label
        Constraint::Length(3), // Input field
        Constraint::Length(1), // Hint
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Label
    let label =
        Paragraph::new(state.ui.prompt_label.as_str()).style(Style::default().fg(Color::White));
    f.render_widget(label, v_layout[0]);

    // Input field with styled block
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .style(Style::default().bg(Color::Rgb(24, 28, 40)));

    let input_inner = input_block.inner(v_layout[1]);
    f.render_widget(input_block, v_layout[1]);

    // Build the input text with a visible cursor (█ at cursor position).
    // `input_cursor` is a char index (not byte offset) to correctly handle
    // multi-byte Unicode characters (e.g. emoji in project names).
    let text = &state.ui.input_text;
    let cursor_char_idx = state.ui.input_cursor;

    // Convert char index to byte offset for split_at.
    let cursor_byte_pos = text
        .char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    // Split text into before-cursor and after-cursor portions
    let (before, after) = text.split_at(cursor_byte_pos);

    let input_line = Span::styled(
        format!("{}{}", before, after),
        Style::default().fg(Color::White),
    );

    let input = Paragraph::new(input_line).style(Style::default().fg(Color::White));
    f.render_widget(input, input_inner);

    // Show the cursor as a block-style character by setting cursor position.
    // Use the char index for x-position since each character occupies one
    // terminal cell (CJK wide characters aside — acceptable approximation).
    let char_count = text.chars().count();
    f.set_cursor_position((
        input_inner.x + cursor_char_idx.min(char_count) as u16,
        input_inner.y,
    ));

    // Hint
    let hint = Paragraph::new(Span::styled(
        "Enter: confirm  |  Esc: cancel",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(hint, v_layout[2]);
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_width = area.width * percent_x / 100;
    let popup_height = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

/// Render a centered confirmation dialog overlay on top of the current view.
///
/// Displays the confirmation message and `[y: delete / n: cancel]` hint.
/// The user presses `y` to confirm or `n`/`Esc` to cancel.
pub fn render_confirm_dialog(f: &mut Frame, state: &crate::state::types::AppState) {
    let area = centered_rect(50, 20, f.area());

    // Build the confirmation message from the pending action
    let (title, message, hint) = match &state.ui.confirm_action {
        Some(crate::state::types::ConfirmableAction::DeleteProject(project_id)) => {
            let project_name = state
                .projects
                .iter()
                .find(|p| p.id == *project_id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| project_id.clone());
            (
                " Delete Project ".to_string(),
                format!("Delete project \"{}\"?", project_name),
                "y: delete  |  n/Esc: cancel".to_string(),
            )
        }
        None => (
            " Confirm ".to_string(),
            "Are you sure?".to_string(),
            "y: confirm  |  n/Esc: cancel".to_string(),
        ),
    };

    // Clear the area behind the overlay
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(36, 40, 56)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Layout inside the block: message | hint
    let constraints = [
        Constraint::Length(1), // Message
        Constraint::Length(1), // Hint
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Message
    let msg = Paragraph::new(Span::styled(
        message,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(msg, v_layout[0]);

    // Hint
    let hint_widget = Paragraph::new(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(hint_widget, v_layout[1]);
}
