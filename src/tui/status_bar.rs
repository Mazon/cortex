//! Status bar renderer — bottom bar showing connection status, notifications, key hints.

use crate::state::types::{AppState, NotificationVariant};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Render the status bar at the bottom of the kanban area.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    // Connection status (left)
    let conn_text = if state.connected {
        "● connected"
    } else {
        "○ disconnected"
    };
    let conn_color = if state.connected {
        Color::Green
    } else {
        Color::DarkGray
    };

    // Notification (center)
    let (notif_text, notif_color) = if let Some(ref n) = state.ui.notification {
        let color = match n.variant {
            NotificationVariant::Info => Color::Blue,
            NotificationVariant::Success => Color::Green,
            NotificationVariant::Warning => Color::Yellow,
            NotificationVariant::Error => Color::Red,
        };
        (n.message.as_str(), color)
    } else {
        ("", Color::Reset)
    };

    // Key hints (right)
    let hints = "?:help  n:new  e:edit  m:move  x:del  r:rename  d:dir  ^j/^k:proj  ^q:quit";

    // Build the status bar using a horizontal layout
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(14), // Connection status
            Constraint::Min(0),     // Notification (center)
            Constraint::Length(52), // Key hints
        ])
        .split(area);

    // Left: connection status
    let left = Paragraph::new(Span::styled(conn_text, Style::default().fg(conn_color)));
    f.render_widget(left, h_layout[0]);

    // Center: notification
    if !notif_text.is_empty() {
        let inner = h_layout[1].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        let center = Paragraph::new(Span::styled(notif_text, Style::default().fg(notif_color)));
        f.render_widget(center, inner);
    }

    // Right: key hints
    let right = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    f.render_widget(right, h_layout[2]);
}
