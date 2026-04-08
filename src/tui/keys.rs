//! Keybinding handler — parse config keybindings, match crossterm events to actions.

use crate::config::types::KeybindingConfig;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Actions that can be triggered by keybindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Global
    Quit,
    HelpToggle,
    PrevProject,
    NextProject,
    NewProject,
    // Kanban navigation
    NavLeft,
    NavRight,
    NavUp,
    NavDown,
    // Task operations
    CreateTask,
    EditTask,
    MoveForward,
    MoveBackward,
    DeleteTask,
    ViewTask,
    AbortSession,
}

/// Matches crossterm KeyEvents to Actions based on config.
pub struct KeyMatcher {
    bindings: Vec<(KeyEvent, Action)>,
}

impl KeyMatcher {
    /// Build a KeyMatcher from the config keybindings.
    pub fn from_config(config: &KeybindingConfig) -> Self {
        let mut bindings = Vec::new();

        // Parse each binding string (comma-separated alternatives)
        parse_and_add(&mut bindings, &config.quit, Action::Quit);
        parse_and_add(&mut bindings, &config.help_toggle, Action::HelpToggle);
        parse_and_add(&mut bindings, &config.prev_project, Action::PrevProject);
        parse_and_add(&mut bindings, &config.next_project, Action::NextProject);
        parse_and_add(&mut bindings, &config.new_project, Action::NewProject);
        parse_and_add(&mut bindings, &config.kanban_left, Action::NavLeft);
        parse_and_add(&mut bindings, &config.kanban_right, Action::NavRight);
        parse_and_add(&mut bindings, &config.kanban_up, Action::NavUp);
        parse_and_add(&mut bindings, &config.kanban_down, Action::NavDown);
        parse_and_add(&mut bindings, &config.todo_new, Action::CreateTask);
        parse_and_add(&mut bindings, &config.todo_edit, Action::EditTask);
        parse_and_add(
            &mut bindings,
            &config.kanban_move_forward,
            Action::MoveForward,
        );
        parse_and_add(
            &mut bindings,
            &config.kanban_move_backward,
            Action::MoveBackward,
        );
        parse_and_add(&mut bindings, &config.task_delete, Action::DeleteTask);
        parse_and_add(&mut bindings, &config.task_view, Action::ViewTask);
        parse_and_add(&mut bindings, &config.abort_session, Action::AbortSession);

        Self { bindings }
    }

    /// Match a KeyEvent to an Action. Returns None if no match.
    pub fn match_key(&self, key: KeyEvent) -> Option<Action> {
        for (binding_key, action) in &self.bindings {
            if keys_match(binding_key, &key) {
                return Some(action.clone());
            }
        }
        None
    }
}

/// Parse a comma-separated key combo string and add bindings.
fn parse_and_add(bindings: &mut Vec<(KeyEvent, Action)>, combo_str: &str, action: Action) {
    for combo in combo_str.split(',') {
        let combo = combo.trim();
        if let Some(key) = parse_key_combo(combo) {
            bindings.push((key, action.clone()));
        }
    }
}

/// Parse a single key combo string like "ctrl+q", "shift+m", "left", "h".
fn parse_key_combo(combo: &str) -> Option<KeyEvent> {
    let parts: Vec<&str> = combo.split('+').collect();
    let mut modifiers = KeyModifiers::NONE;
    let mut code = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" => modifiers |= KeyModifiers::ALT,
            "left" => code = Some(KeyCode::Left),
            "right" => code = Some(KeyCode::Right),
            "up" => code = Some(KeyCode::Up),
            "down" => code = Some(KeyCode::Down),
            "enter" => code = Some(KeyCode::Enter),
            "tab" => code = Some(KeyCode::Tab),
            "esc" | "escape" => code = Some(KeyCode::Esc),
            "backspace" => code = Some(KeyCode::Backspace),
            "delete" => code = Some(KeyCode::Delete),
            "home" => code = Some(KeyCode::Home),
            "end" => code = Some(KeyCode::End),
            "pageup" => code = Some(KeyCode::PageUp),
            "pagedown" => code = Some(KeyCode::PageDown),
            " " | "space" => code = Some(KeyCode::Char(' ')),
            c if c.len() == 1 => code = Some(KeyCode::Char(c.chars().next().unwrap())),
            _ => return None,
        }
    }

    code.map(|c| KeyEvent::new(c, modifiers))
}

/// Check if two KeyEvents match (ignoring char case when Shift is not held).
fn keys_match(a: &KeyEvent, b: &KeyEvent) -> bool {
    if a.modifiers != b.modifiers {
        return false;
    }
    match (a.code, b.code) {
        (KeyCode::Char(a_char), KeyCode::Char(b_char)) => {
            // Case-insensitive matching for letters (without shift)
            if a.modifiers.contains(KeyModifiers::SHIFT) {
                a_char == b_char
            } else {
                a_char.eq_ignore_ascii_case(&b_char)
            }
        }
        (a_code, b_code) => a_code == b_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_combo() {
        let key = parse_key_combo("ctrl+q").unwrap();
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
        assert_eq!(key.code, KeyCode::Char('q'));

        let key = parse_key_combo("shift+m").unwrap();
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.code, KeyCode::Char('m'));

        let key = parse_key_combo("left").unwrap();
        assert_eq!(key.code, KeyCode::Left);
        assert_eq!(key.modifiers, KeyModifiers::NONE);

        let key = parse_key_combo("h").unwrap();
        assert_eq!(key.code, KeyCode::Char('h'));
    }

    #[test]
    fn test_key_matcher() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(matcher.match_key(key), Some(Action::Quit));

        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(matcher.match_key(key), Some(Action::NavLeft));

        let key = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(matcher.match_key(key), Some(Action::NavLeft));
    }
}
