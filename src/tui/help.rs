//! Help overlay — keybinding reference overlay.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Render a centered help overlay on top of the current view.
pub fn render_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 70, f.area());

    // Clear the area behind the overlay
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Help — Keybindings ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(36, 40, 56)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let help_text = r#"
 Global Keys
 ─────────────────────────────────────────
   Ctrl+Q        Quit
   ?             Toggle this help overlay
   Ctrl+J / K    Next / Previous project
   Ctrl+N        New project
   R             Rename active project
   Ctrl+Shift+D   Delete active project

 Kanban Keys
 ─────────────────────────────────────────
   H / ←         Move focus left (column)
   L / →         Move focus right (column)
   K / ↑         Move focus up (task)
   J / ↓         Move focus down (task)
   N             Create new task
   E             Edit selected task
   M             Move task forward (→ column)
   Shift+M       Move task backward (← column)
   X             Delete selected task
   V             View task detail
   R             Rename project
   D             Set working directory
   Ctrl+A A      Abort running session

 Task Editor Keys (fixed, not configurable)
 ─────────────────────────────────────────
   Tab           Cycle field focus (Title ↔ Description)
   Enter         Next field (title) / Newline (description)
   Ctrl+S        Save task
   Escape        Cancel and discard
   Arrow keys    Move cursor
   Home / End    Line start / end
   Page Up/Down  Scroll description
   Backspace     Delete character before cursor
   Delete        Delete character at cursor

 Press any key to close this overlay.
 "#;

    let help_para = Paragraph::new(help_text).style(Style::default().fg(Color::White));
    f.render_widget(help_para, inner);
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_width = area.width * percent_x / 100;
    let popup_height = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}
