//! Status bar renderer — bottom bar showing connection status, notifications, key hints.

use crate::state::types::{AppState, NotificationVariant, MAX_NOTIFICATIONS};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Render the status bar at the bottom of the kanban area.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    // Connection status (left)
    let (conn_text, conn_color) = if state.reconnecting {
        let attempt = state.reconnect_attempt;
        if attempt > 0 {
            (format!("◐ reconnecting ({})...", attempt), Color::Yellow)
        } else {
            ("◐ reconnecting...".to_string(), Color::Yellow)
        }
    } else if state.connected {
        ("● connected".to_string(), Color::Green)
    } else {
        ("○ disconnected".to_string(), Color::DarkGray)
    };

    // Notification (center) — show most recent with queue count indicator
    let (notif_text, notif_color) = if let Some(n) = state.ui.notifications.back() {
        let color = match n.variant {
            NotificationVariant::Info => Color::Blue,
            NotificationVariant::Success => Color::Green,
            NotificationVariant::Warning => Color::Yellow,
            NotificationVariant::Error => Color::Red,
        };
        let count = state.ui.notifications.len();
        if count > 1 {
            let display = format!("({}/{}) {}", count, MAX_NOTIFICATIONS, n.message);
            (display, color)
        } else {
            (n.message.clone(), color)
        }
    } else {
        (String::new(), Color::Reset)
    };

    // Key hints (right)
    let hints = "?:help  n:new  e:edit  m:move  x:del  r:rename  d:dir  ^j/^k:proj  ^q:quit";

    // Build the status bar using a horizontal layout
    // Connection status width is dynamic: "● connected" (13) to "◐ reconnecting (99)..." (23)
    let conn_width = conn_text.chars().count().max(14) as u16;
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(conn_width), // Connection status (dynamic)
            Constraint::Min(0),             // Notification (center)
            Constraint::Length(52),         // Key hints
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
