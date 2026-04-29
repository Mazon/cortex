//! Status bar renderer — bottom bar showing connection status, project info,
//! notifications, and a `?:help` indicator.

use crate::config::types::ThemeConfig;
use crate::state::types::{
    AgentStatus, AppState, NotificationVariant, MAX_NOTIFICATIONS,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::sync::atomic::Ordering;

/// Render the status bar at the bottom of the terminal, spanning the full width.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState, theme: &ThemeConfig) {
    // Render a full-width top border as a visual separator between content and status bar.
    let status_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let area = status_block.inner(area);
    f.render_widget(status_block, area);
    // Count pending permissions and questions across all tasks for the active project
    let (total_permissions, total_questions) = state
        .tasks
        .values()
        .filter(|t| {
            state
                .project_registry
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

    // Check if a persistence save is in progress
    let is_saving = state.dirty_flags.saving_in_progress.load(Ordering::Relaxed);

    // Connection status (left) — uses per-project connection state from the active project
    let (conn_text, conn_color) = if state.is_permanently_disconnected() {
        (
            "✕ disconnected (max retries exceeded — restart to retry)".to_string(),
            theme.error_color(),
        )
    } else if state.is_reconnecting() {
        let attempt = state.reconnect_attempt();
        if attempt > 0 {
            (
                format!("◐ reconnecting ({})...", attempt),
                theme.reconnecting_color(),
            )
        } else {
            ("◐ reconnecting...".to_string(), theme.reconnecting_color())
        }
    } else if state.is_connected() {
        ("● connected".to_string(), theme.connected_color())
    } else {
        ("○ disconnected".to_string(), theme.disconnected_color())
    };

    // Active project name and task count as separate fields
    let (project_name, task_label) = state
        .project_registry
        .active_project_id
        .as_ref()
        .and_then(|pid| {
            state
                .project_registry
                .projects
                .iter()
                .find(|p| &p.id == pid)
                .map(|p| {
                    let task_count = state
                        .tasks
                        .values()
                        .filter(|t| {
                            t.project_id == *pid && matches!(t.agent_status, AgentStatus::Running)
                        })
                        .count();
                    let label = if task_count == 1 {
                        "1 task".to_string()
                    } else {
                        format!("{} tasks", task_count)
                    };
                    (p.name.clone(), label)
                })
        })
        .unwrap_or_default();

    // Notification (center) — show most recent with queue count indicator
    // Uses configurable theme colors
    let (notif_text, notif_color) = if let Some(n) = state.ui.notifications.back() {
        let color = match n.variant {
            NotificationVariant::Info => theme.info_color(),
            NotificationVariant::Success => theme.done_color(),
            NotificationVariant::Warning => theme.question_color(),
            NotificationVariant::Error => theme.error_color(),
        };
        let count = state.ui.notifications.len();
        if count > 1 {
            let display = format!("({}/{}) {}", count, MAX_NOTIFICATIONS, n.message);
            (display, color)
        } else {
            (n.message.clone(), color)
        }
    } else if is_saving {
        // Show "saving..." indicator when persistence is active
        ("saving...".to_string(), theme.info_color())
    } else {
        (String::new(), Color::Reset)
    };

    // The status bar always shows just "?" to indicate help is available.
    let hints = "?:help";

    // Build the status bar using a horizontal layout
    let total_width = area.width as usize;

    // Connection status width is dynamic: "● connected" (13) to "◐ reconnecting (99)..." (23)
    let conn_width = conn_text.chars().count().max(14) as u16;

    // Project info width (hide on narrow terminals)
    let has_project = !project_name.is_empty();
    let show_project = has_project && total_width >= 70;
    // Width for: "│ project_name │ task_label │"
    let proj_name_len = project_name.chars().count();
    let task_label_len = task_label.chars().count();
    let proj_width = if show_project {
        // "│ " + name + " │ " + label + " │"
        (2 + proj_name_len + 3 + task_label_len + 2) as u16
    } else {
        0
    };

    // Attention indicator takes precedence over notification in the center area
    let has_center_text = has_attention_items || !notif_text.is_empty();

    // Available space for center text + hints
    let remaining = total_width
        .saturating_sub(conn_width as usize)
        .saturating_sub(proj_width as usize);

    // Choose whether to show hints based on available space.
    let hints = if has_center_text {
        let hint_budget = remaining.saturating_sub(20);
        if hint_budget >= hints.chars().count() {
            hints
        } else {
            ""
        }
    } else {
        if remaining >= hints.chars().count() {
            hints
        } else {
            ""
        }
    };

    let hints_width = hints.chars().count() as u16;

    // Layout: connection (fixed) | project name (fixed) | task count (fixed) | notification (flex) | hints (fixed)
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(conn_width), // Connection status (left, fixed)
    ];
    if show_project {
        // "│ " + name + " │ " + label + " │" as separate slots for even spacing
        constraints.push(Constraint::Length((2 + proj_name_len) as u16)); // "│ name"
        constraints.push(Constraint::Length((3 + task_label_len) as u16)); // " │ label │"
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

    // Project name and task count with │ separators
    if show_project {
        let name_widget = Paragraph::new(Span::styled(
            format!("│ {}", project_name),
            Style::default().fg(Color::Cyan),
        ));
        f.render_widget(name_widget, h_layout[slot]);
        slot += 1;

        let count_widget = Paragraph::new(Span::styled(
            format!(" │ {} │", task_label),
            Style::default().fg(Color::Cyan),
        ));
        f.render_widget(count_widget, h_layout[slot]);
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

