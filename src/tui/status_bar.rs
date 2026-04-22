//! Status bar renderer — bottom bar showing connection status, project info,
//! notifications, attention indicators, and key hints.

use crate::state::types::{AppState, NotificationVariant, MAX_NOTIFICATIONS};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Render the status bar at the bottom of the kanban area.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    // Count pending permissions and questions across all tasks for the active project
    let (total_permissions, total_questions) = state
        .tasks
        .values()
        .filter(|t| {
            state
                .active_project_id
                .as_ref()
                .map_or(false, |pid| t.project_id == *pid)
        })
        .fold((0u32, 0u32), |(perm, quest), t| {
            (
                perm + t.pending_permission_count,
                quest + t.pending_question_count,
            )
        });
    let has_attention_items = total_permissions > 0 || total_questions > 0;

    // Build attention indicator text (shown prominently when there are pending items)
    let attention_text = if total_permissions > 0 && total_questions > 0 {
        format!(
            "\u{26A0} {} perm{}, {} quest{} \u{2014} press v",
            total_permissions,
            if total_permissions == 1 { "" } else { "s" },
            total_questions,
            if total_questions == 1 { "" } else { "s" },
        )
    } else if total_permissions > 0 {
        format!(
            "\u{26A0} {} permission{} pending \u{2014} press v",
            total_permissions,
            if total_permissions == 1 { "" } else { "s" },
        )
    } else if total_questions > 0 {
        format!(
            "\u{26A0} {} question{} pending \u{2014} press v",
            total_questions,
            if total_questions == 1 { "" } else { "s" },
        )
    } else {
        String::new()
    };

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

    // Active project name + task count (displayed between connection status and notifications)
    let project_info = state
        .active_project_id
        .as_ref()
        .and_then(|pid| {
            state.projects.iter().find(|p| &p.id == pid).map(|p| {
                let task_count = state
                    .tasks
                    .values()
                    .filter(|t| t.project_id == *pid)
                    .count();
                let label = if task_count == 1 {
                    "1 task".to_string()
                } else {
                    format!("{} tasks", task_count)
                };
                format!(" │ {} ({})", p.name, label)
            })
        })
        .unwrap_or_default();

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

    // Key hints tiers — from longest to shortest, chosen based on available space.
    const HINTS_FULL: &str =
        "?:help  n:new  e:edit  m:move  x:del  r:rename  d:dir  ^j/^k:proj  ^q:quit";
    const HINTS_MEDIUM: &str = "?:help  n:new  e:edit  m:move  x:del  ^q:quit";
    const HINTS_SHORT: &str = "?:help  ^q:quit";
    const HINTS_MINIMAL: &str = "?:help";

    // Build the status bar using a horizontal layout
    let total_width = area.width as usize;

    // Connection status width is dynamic: "● connected" (13) to "◐ reconnecting (99)..." (23)
    let conn_width = conn_text.chars().count().max(14) as u16;

    // Project info width (hide on narrow terminals)
    let proj_len = project_info.chars().count();
    let show_project = !project_info.is_empty() && total_width >= 70;
    let proj_width = if show_project { proj_len as u16 } else { 0 };

    // Attention indicator takes precedence over notification in the center area
    let has_center_text = has_attention_items || !notif_text.is_empty();

    // Available space for center text + hints
    let remaining = total_width
        .saturating_sub(conn_width as usize)
        .saturating_sub(proj_width as usize);

    // Choose the appropriate hint tier based on available space.
    let hints = if has_center_text {
        let hint_budget = remaining.saturating_sub(20);
        if hint_budget >= HINTS_FULL.chars().count() {
            HINTS_FULL
        } else if hint_budget >= HINTS_MEDIUM.chars().count() {
            HINTS_MEDIUM
        } else if hint_budget >= HINTS_SHORT.chars().count() {
            HINTS_SHORT
        } else {
            HINTS_MINIMAL
        }
    } else {
        if remaining >= HINTS_FULL.chars().count() {
            HINTS_FULL
        } else if remaining >= HINTS_MEDIUM.chars().count() {
            HINTS_MEDIUM
        } else if remaining >= HINTS_SHORT.chars().count() {
            HINTS_SHORT
        } else if total_width >= 60 {
            HINTS_MINIMAL
        } else {
            ""
        }
    };

    let hints_width = hints.chars().count() as u16;

    // Layout: connection (fixed) | project (fixed, conditional) | notification (flex) | hints (fixed)
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(conn_width), // Connection status (left, fixed)
    ];
    if show_project {
        constraints.push(Constraint::Length(proj_width)); // Project name + task count
    }
    constraints.push(Constraint::Min(0)); // Attention / Notification (center, flexible)
    constraints.push(Constraint::Length(hints_width)); // Key hints (right)

    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    let mut slot = 0;

    // Left: connection status
    let left = Paragraph::new(Span::styled(conn_text, Style::default().fg(conn_color)));
    f.render_widget(left, h_layout[slot]);
    slot += 1;

    // Project info
    if show_project {
        let proj_widget =
            Paragraph::new(Span::styled(project_info, Style::default().fg(Color::Cyan)));
        f.render_widget(proj_widget, h_layout[slot]);
        slot += 1;
    }

    // Center: attention indicator (takes precedence) or notification
    if has_attention_items {
        let inner = h_layout[slot].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        let center = Paragraph::new(Span::styled(
            attention_text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        f.render_widget(center, inner);
    } else if !notif_text.is_empty() {
        let inner = h_layout[slot].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        let center = Paragraph::new(Span::styled(notif_text, Style::default().fg(notif_color)));
        f.render_widget(center, inner);
    }
    slot += 1;

    // Right: key hints
    if !hints.is_empty() {
        let right = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
        f.render_widget(right, h_layout[slot]);
    }
}
