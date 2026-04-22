//! Help overlay — keybinding reference overlay.

use crate::config::types::KeybindingConfig;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Render a centered help overlay on top of the current view.
/// The keybindings displayed are pulled from the actual config, so custom
/// bindings are shown instead of a hardcoded default list.
pub fn render_help_overlay(f: &mut Frame, kb: &KeybindingConfig) {
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

    let help_text = build_help_text(kb);

    let help_para = Paragraph::new(help_text).style(Style::default().fg(Color::White));
    f.render_widget(help_para, inner);
}

/// Build the help text dynamically from the configured keybindings.
fn build_help_text(kb: &KeybindingConfig) -> String {
    use std::fmt::Write;

    let mut s = String::with_capacity(1024);

    let _ = writeln!(s);
    let _ = writeln!(s, " Global Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(s, "   {:<16} Quit", format_combo(&kb.quit));
    let _ = writeln!(s, "   {:<16} Toggle this help overlay", format_combo(&kb.help_toggle));
    let _ = writeln!(s, "   {:<16} Next / Previous project", format_combos_slash(&kb.next_project, &kb.prev_project));
    let _ = writeln!(s, "   {:<16} New project", format_combo(&kb.new_project));
    let _ = writeln!(s, "   {:<16} Rename active project", format_combo(&kb.rename_project));

    let _ = writeln!(s);
    let _ = writeln!(s, " Kanban Keys");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(s, "   {:<16} Move focus left (column)", format_combo(&kb.kanban_left));
    let _ = writeln!(s, "   {:<16} Move focus right (column)", format_combo(&kb.kanban_right));
    let _ = writeln!(s, "   {:<16} Move focus up (task)", format_combo(&kb.kanban_up));
    let _ = writeln!(s, "   {:<16} Move focus down (task)", format_combo(&kb.kanban_down));
    let _ = writeln!(s, "   {:<16} Create new task", format_combo(&kb.todo_new));
    let _ = writeln!(s, "   {:<16} Edit selected task", format_combo(&kb.todo_edit));
    let _ = writeln!(s, "   {:<16} Move task forward (→ column)", format_combo(&kb.kanban_move_forward));
    let _ = writeln!(s, "   {:<16} Move task backward (← column)", format_combo(&kb.kanban_move_backward));
    let _ = writeln!(s, "   {:<16} Delete selected task", format_combo(&kb.task_delete));
    let _ = writeln!(s, "   {:<16} View task detail", format_combo(&kb.task_view));
    let _ = writeln!(s, "   {:<16} Set working directory", format_combo(&kb.set_working_directory));
    let _ = writeln!(s, "   {:<16} Abort running session", format_combo(&kb.abort_session));

    let _ = writeln!(s);
    let _ = writeln!(s, " Task Editor Keys (fixed, not configurable)");
    let _ = writeln!(s, " ──────────────────────────────────────");
    let _ = writeln!(s, "   Tab           Cycle field focus (Title ↔ Description)");
    let _ = writeln!(s, "   Enter         Next field (title) / Newline (description)");
    let _ = writeln!(s, "   Ctrl+S        Save task");
    let _ = writeln!(s, "   Escape        Cancel and discard");
    let _ = writeln!(s, "   Arrow keys    Move cursor");
    let _ = writeln!(s, "   Home / End    Line start / end");
    let _ = writeln!(s, "   Page Up/Down  Scroll description");
    let _ = writeln!(s, "   Backspace     Delete character before cursor");
    let _ = writeln!(s, "   Delete        Delete character at cursor");

    let _ = writeln!(s);
    let _ = writeln!(s, " Press any key to close this overlay.");
    let _ = writeln!(s, " ");

    s
}

/// Format a combo string (comma-separated alternatives) for display.
/// Example: "ctrl+q" → "Ctrl+Q", "h, left" → "H / ←"
fn format_combo(combo: &str) -> String {
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
/// Examples: "ctrl+q" → "Ctrl+Q", "left" → "←", "h" → "H"
fn format_single_combo(combo: &str) -> String {
    let parts: Vec<&str> = combo.split('+').collect();
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
                    if ch.is_ascii_alphabetic() {
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
        assert_eq!(format_single_combo("ctrl+q"), "Ctrl+Q");
        assert_eq!(format_single_combo("shift+m"), "Shift+M");
        assert_eq!(format_single_combo("h"), "H");
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
        assert_eq!(format_combo("h, left"), "H / ←");
        assert_eq!(format_combo("l, right"), "L / →");
        assert_eq!(format_combo("k, up"), "K / ↑");
        assert_eq!(format_combo("j, down"), "J / ↓");
    }

    #[test]
    fn test_format_combos_slash() {
        assert_eq!(format_combos_slash("ctrl+j", "ctrl+k"), "Ctrl+J / Ctrl+K");
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("ctrl"), "Ctrl");
        assert_eq!(capitalize("shift"), "Shift");
        assert_eq!(capitalize("alt"), "Alt");
    }

    #[test]
    fn test_build_help_text_default_config() {
        let kb = KeybindingConfig::default();
        let text = build_help_text(&kb);
        assert!(text.contains("Global Keys"));
        assert!(text.contains("Kanban Keys"));
        assert!(text.contains("Task Editor Keys"));
        assert!(text.contains("Ctrl+Q"));
        assert!(text.contains("H / ←"));
        assert!(text.contains("Shift+M"));
    }

    #[test]
    fn test_build_help_text_custom_config() {
        let mut kb = KeybindingConfig::default();
        kb.quit = "ctrl+x".to_string();
        kb.kanban_left = "a".to_string();
        let text = build_help_text(&kb);
        assert!(text.contains("Ctrl+X"));
        assert!(text.contains("A"));
        assert!(!text.contains("Ctrl+Q"));
        assert!(!text.contains("H / ←"));
    }
}
