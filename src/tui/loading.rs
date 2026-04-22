//! Loading screen rendered during startup before the main event loop.
//!
//! Provides a centered loading indicator with a braille spinner animation
//! to give users visual feedback while servers are starting up.

use crate::tui::Terminal;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Braille spinner characters — each frame cycles to the next character.
const SPINNER_CHARS: &[char] = &[
    '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏',
];

/// Render a centered loading screen with a spinner animation.
///
/// Draws a branded "Cortex" title and the provided status message with a
/// rotating braille spinner, both horizontally and vertically centered
/// in the terminal.
pub fn render_loading_frame(
    terminal: &mut Terminal,
    message: &str,
    spinner_index: usize,
) -> std::io::Result<()> {
    let spinner = SPINNER_CHARS[spinner_index % SPINNER_CHARS.len()];

    terminal.draw(|f| {
        let area = f.area();

        // Build the content lines
        let title = Line::from(Span::styled(
            "Cortex",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

        let spinner_text = format!("{} {}", spinner, message);
        let status = Line::from(Span::styled(spinner_text, Style::default().fg(Color::Gray)));

        // Vertically center two lines within the available area.
        let v_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1), // title
                Constraint::Length(1), // status (spinner + message)
                Constraint::Fill(1),
            ])
            .split(area);

        // Horizontally center each line.
        for (idx, line) in [title, status].into_iter().enumerate() {
            let h_layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Fill(1),
                    Constraint::Length(line.width() as u16),
                    Constraint::Fill(1),
                ])
                .split(v_layout[idx + 1]);

            f.render_widget(Paragraph::new(line), h_layout[1]);
        }
    })?;

    Ok(())
}

/// Advance the spinner index by one step.
pub fn advance_spinner(index: usize) -> usize {
    (index + 1) % SPINNER_CHARS.len()
}
