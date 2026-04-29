//! Project sidebar renderer.

use crate::config::types::{parse_hex_color_or, CortexConfig};
use crate::state::types::AppState;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Render the project sidebar in the given area.
pub fn render_sidebar(f: &mut Frame, area: Rect, state: &AppState, _config: &CortexConfig) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::RIGHT | Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " Projects ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.project_registry.projects.is_empty() {
        let empty_text = Paragraph::new("No projects")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty_text, inner);
        return;
    }

    let mut y = inner.y;
    for project in &state.project_registry.projects {
        if y >= inner.y + inner.height {
            break;
        }

        let is_active =
            state.project_registry.active_project_id.as_deref() == Some(project.id.as_str());
        let icon = project.status.icon();

        let name_style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let icon_color = match project.status {
            crate::state::types::ProjectStatus::Working => {
                parse_hex_color_or(&_config.theme.status_working, Color::Blue)
            }
            crate::state::types::ProjectStatus::Error => {
                parse_hex_color_or(&_config.theme.status_error, Color::Red)
            }
            crate::state::types::ProjectStatus::Question => {
                parse_hex_color_or(&_config.theme.status_question, Color::Yellow)
            }
            crate::state::types::ProjectStatus::Done => {
                parse_hex_color_or(&_config.theme.status_done, Color::Green)
            }
            crate::state::types::ProjectStatus::Hung => Color::Rgb(255, 87, 34),
            _ => Color::DarkGray,
        };

        // Truncate name to fit
        let max_name_len = (inner.width as usize).saturating_sub(4); // icon + space + padding
        let display_name = if project.name.chars().count() > max_name_len {
            let truncated: String = project
                .name
                .chars()
                .take(max_name_len.saturating_sub(3))
                .collect();
            format!("{}...", truncated)
        } else {
            project.name.clone()
        };

        let text = Paragraph::new(Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color)),
            Span::styled(format!(" {}", display_name), name_style),
        ]));

        let text_area = Rect {
            x: inner.x + 1,
            y,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(text, text_area);
        y += 1;
    }
}
