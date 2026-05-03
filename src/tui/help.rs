//! Help overlay — keybinding reference overlay.

use crate::config::types::{EditorKeybindingConfig, KeybindingConfig};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Render a centered help overlay showing all keybindings at once.
/// The keybindings displayed are pulled from the actual config, so custom
/// bindings are shown instead of a hardcoded default list.
pub fn render_help_overlay(f: &mut Frame, kb: &KeybindingConfig) {
    let area = centered_rect(80, 90, f.area());

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

    // Split inner area into two rows: main content (flex) + footer (fixed 2 lines)
    let main_and_footer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);

    // Split main content into two columns with a 1-cell gap
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Length(1), Constraint::Percentage(50)])
        .split(main_and_footer[0]);

    // Left column: Global + Kanban keys
    let left_text = build_left_column_text(kb);
    let left_para = Paragraph::new(left_text).style(Style::default().fg(Color::White));
    f.render_widget(left_para, columns[0]);

    // Vertical separator
    let separator = Paragraph::new(
        (0..columns[1].height)
            .map(|_| "│")
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .style(Style::default().fg(Color::Rgb(80, 90, 110)));
    f.render_widget(separator, columns[1]);

    // Right column: Review + Task Editor keys
    let right_text = build_right_column_text(kb);
    let right_para = Paragraph::new(right_text).style(Style::default().fg(Color::White));
    f.render_widget(right_para, columns[2]);

    // Footer centered at the bottom
    let footer = Paragraph::new("Press any key to close")
        .style(Style::default().fg(Color::Rgb(150, 160, 180)))
        .alignment(Alignment::Center);
    f.render_widget(footer, main_and_footer[1]);
}

/// Build the left column text: Global + Kanban keys.
fn build_left_column_text(kb: &KeybindingConfig) -> String {
    let mut s = String::new();
    s.push_str(&build_global_text(kb));
    s.push_str(&build_kanban_text(kb));
    s
}

/// Build the right column text: Review + Task Editor keys.
fn build_right_column_text(kb: &KeybindingConfig) -> String {
    let mut s = String::new();
    s.push_str(&build_review_text(kb));
    s.push_str(&build_editor_text(&kb.editor));
    s
}

/// Build the Global keybindings tab.
fn build_global_text(kb: &KeybindingConfig) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(512);
    let _ = writeln!(s);
    let _ = writeln!(s, " Global Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(s, "   {:<16} Quit", format_combo(&kb.quit));
    let _ = writeln!(
        s,
        "   {:<16} Toggle this help overlay",
        format_combo(&kb.help_toggle)
    );
    let _ = writeln!(
        s,
        "   {:<16} Next / Previous project",
        format_combos_slash(&kb.next_project, &kb.prev_project)
    );
    let _ = writeln!(s, "   {:<16} New project", format_combo(&kb.new_project));
    let _ = writeln!(
        s,
        "   {:<16} Rename active project",
        format_combo(&kb.rename_project)
    );
    let _ = writeln!(
        s,
        "   {:<16} Delete active project",
        format_combo(&kb.delete_project)
    );
    let _ = writeln!(s, "   {:<16} Reset circuit breaker", "Ctrl+r");
    let _ = writeln!(s);
    s
}

/// Build the Kanban keybindings tab.
fn build_kanban_text(kb: &KeybindingConfig) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(512);
    let _ = writeln!(s);
    let _ = writeln!(s, " Kanban Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(
        s,
        "   {:<16} Move focus left (column)",
        format_combo(&kb.kanban_left)
    );
    let _ = writeln!(
        s,
        "   {:<16} Move focus right (column)",
        format_combo(&kb.kanban_right)
    );
    let _ = writeln!(
        s,
        "   {:<16} Move focus up (task)",
        format_combo(&kb.kanban_up)
    );
    let _ = writeln!(
        s,
        "   {:<16} Move focus down (task)",
        format_combo(&kb.kanban_down)
    );
    let _ = writeln!(s, "   {:<16} Create new task", format_combo(&kb.todo_new));
    let _ = writeln!(s, "   {:<16} Open task detail", format_combo(&kb.task_open));
    let _ = writeln!(
        s,
        "   {:<16} Move task forward (→ column)",
        format_combo(&kb.kanban_move_forward)
    );
    let _ = writeln!(
        s,
        "   {:<16} Move task backward (← column)",
        format_combo(&kb.kanban_move_backward)
    );
    let _ = writeln!(
        s,
        "   {:<16} Delete selected task",
        format_combo(&kb.task_delete)
    );
    let _ = writeln!(
        s,
        "   {:<16} Set working directory",
        format_combo(&kb.set_working_directory)
    );
    let _ = writeln!(
        s,
        "   {:<16} Abort running session",
        format_combo(&kb.abort_session)
    );
    let _ = writeln!(
        s,
        "   {:<16} Retry hung / failed task",
        format_combo(&kb.retry_task)
    );
    let _ = writeln!(
        s,
        "   {:<16} Review changes (diff)",
        format_combo(&kb.review_changes)
    );
    let _ = writeln!(
        s,
        "   {:<16} Reports view",
        format_combo(&kb.reports)
    );
    let _ = writeln!(
        s,
        "   {:<16} Drill down into subagent",
        format_combo(&kb.drill_down_subagent)
    );
    let _ = writeln!(s);
    s
}

/// Build the Review keybindings tab.
fn build_review_text(kb: &KeybindingConfig) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(256);
    let _ = writeln!(s);
    let _ = writeln!(s, " Review Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(
        s,
        "   {:<16} Accept reviewed task (commit + done)",
        format_combo(&kb.accept_review)
    );
    let _ = writeln!(
        s,
        "   {:<16} Reject reviewed task (back to running)",
        format_combo(&kb.reject_review)
    );
    let _ = writeln!(
        s,
        "   {:<16} View git diff for changes",
        format_combo(&kb.review_changes)
    );
    let _ = writeln!(s);
    s
}

/// Build the Task Editor keybindings tab.
fn build_editor_text(ek: &EditorKeybindingConfig) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(512);
    let _ = writeln!(s);
    let _ = writeln!(s, " Task Editor Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(s, "   {:<16} Save task", format_combo(&ek.save));
    let _ = writeln!(s, "   {:<16} Cancel and discard", format_combo(&ek.cancel));
    let _ = writeln!(
        s,
        "   {:<16} Cycle field focus",
        format_combo(&ek.cycle_field)
    );
    let _ = writeln!(
        s,
        "   {:<16} Newline (description)",
        format_combo(&ek.newline)
    );
    let _ = writeln!(s, "   Arrow keys    Move cursor");
    let _ = writeln!(s, "   Home / End    Line start / end");
    let _ = writeln!(s, "   Page Up/Down  Scroll description");
    let _ = writeln!(s, "   Backspace     Delete character before cursor");
    let _ = writeln!(s, "   Delete        Delete character at cursor");
    let _ = writeln!(s);
    s
}

/// Format a combo string (comma-separated alternatives) for display.
/// Example: "ctrl+q" → "Ctrl+q", "h, left" → "h / ←"
pub(crate) fn format_combo(combo: &str) -> String {
    let parts: Vec<&str> = combo.split(',').collect();
    let formatted: Vec<String> = parts
        .iter()
        .map(|p| format_single_combo(p.trim()))
        .collect();
    formatted.join(" / ")
}

/// Format two combo strings with " / " separator (for paired actions).
fn format_combos_slash(a: &str, b: &str) -> String {
    format!("{} / {}", format_combo(a), format_combo(b))
}

/// Format a single key combo into a display string.
/// Examples: "ctrl+q" → "Ctrl+q", "shift+m" → "Shift+M", "left" → "←", "h" → "h"
fn format_single_combo(combo: &str) -> String {
    let parts: Vec<&str> = combo.split('+').collect();
    let has_shift = parts.iter().any(|p| p.eq_ignore_ascii_case("shift"));
    let mut result: Vec<String> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        if is_last {
            match part.to_lowercase().as_str() {
                "left" => result.push("←".to_string()),
                "right" => result.push("→".to_string()),
                "up" => result.push("↑".to_string()),
                "down" => result.push("↓".to_string()),
                k if k.len() == 1 => {
                    let ch = k.chars().next().unwrap();
                    if ch.is_ascii_alphabetic() && has_shift {
                        result.push(ch.to_ascii_uppercase().to_string());
                    } else {
                        result.push((*part).to_string());
                    }
                }
                other => result.push(capitalize(other)),
            }
        } else {
            result.push(capitalize(part));
        }
    }
    result.join("+")
}

/// Capitalize the first letter of a word.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
    }
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_width = area.width * percent_x / 100;
    let popup_height = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_single_combo_basic() {
        assert_eq!(format_single_combo("ctrl+q"), "Ctrl+q");
        assert_eq!(format_single_combo("shift+m"), "Shift+M");
        assert_eq!(format_single_combo("h"), "h");
        assert_eq!(format_single_combo("?"), "?");
    }

    #[test]
    fn test_format_single_combo_arrows() {
        assert_eq!(format_single_combo("left"), "←");
        assert_eq!(format_single_combo("right"), "→");
        assert_eq!(format_single_combo("up"), "↑");
        assert_eq!(format_single_combo("down"), "↓");
    }

    #[test]
    fn test_format_combo_multi() {
        assert_eq!(format_combo("h, left"), "h / ←");
        assert_eq!(format_combo("l, right"), "l / →");
        assert_eq!(format_combo("k, up"), "k / ↑");
        assert_eq!(format_combo("j, down"), "j / ↓");
    }

    #[test]
    fn test_format_combos_slash() {
        assert_eq!(format_combos_slash("ctrl+j", "ctrl+k"), "Ctrl+j / Ctrl+k");
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("ctrl"), "Ctrl");
        assert_eq!(capitalize("shift"), "Shift");
        assert_eq!(capitalize("alt"), "Alt");
    }

    #[test]
    fn test_build_global_text_default_config() {
        let kb = KeybindingConfig::default();
        let text = build_global_text(&kb);
        assert!(text.contains("Global Keys"));
        assert!(text.contains("Ctrl+q"));
        assert!(text.contains("Ctrl+j / Ctrl+k")); // next/prev project
    }

    #[test]
    fn test_build_kanban_text_default_config() {
        let kb = KeybindingConfig::default();
        let text = build_kanban_text(&kb);
        assert!(text.contains("Kanban Keys"));
        assert!(text.contains("h / ←"));
    }

    #[test]
    fn test_build_review_text_default_config() {
        let kb = KeybindingConfig::default();
        let text = build_review_text(&kb);
        assert!(text.contains("Review Keys"));
    }

    #[test]
    fn test_build_editor_text_default_config() {
        let kb = KeybindingConfig::default();
        let text = build_editor_text(&kb.editor);
        assert!(text.contains("Task Editor Keys"));
    }

    #[test]
    fn test_build_global_text_custom_config() {
        let mut kb = KeybindingConfig::default();
        kb.quit = "ctrl+x".to_string();
        let text = build_global_text(&kb);
        assert!(text.contains("Ctrl+x"));
        assert!(!text.contains("Ctrl+q"));
    }

    #[test]
    fn test_build_editor_text_custom_config() {
        let mut kb = KeybindingConfig::default();
        kb.editor.save = "ctrl+w".to_string();
        kb.editor.cancel = "ctrl+g".to_string();
        let text = build_editor_text(&kb.editor);
        assert!(text.contains("Ctrl+w"));
        assert!(text.contains("Ctrl+g"));
    }

    #[test]
    fn test_build_left_column_text() {
        let kb = KeybindingConfig::default();
        let text = build_left_column_text(&kb);
        assert!(!text.is_empty(), "Left column text should not be empty");
        assert!(text.contains("Global Keys"));
        assert!(text.contains("Kanban Keys"));
        assert!(!text.contains("Review Keys"));
        assert!(!text.contains("Task Editor Keys"));
    }

    #[test]
    fn test_build_right_column_text() {
        let kb = KeybindingConfig::default();
        let text = build_right_column_text(&kb);
        assert!(!text.is_empty(), "Right column text should not be empty");
        assert!(text.contains("Review Keys"));
        assert!(text.contains("Task Editor Keys"));
        assert!(!text.contains("Global Keys"));
        assert!(!text.contains("Kanban Keys"));
    }
}
