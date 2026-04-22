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
    RenameProject,
    // Kanban navigation
    NavLeft,
    NavRight,
    NavUp,
    NavDown,
    // Kanban horizontal scroll
    ScrollKanbanLeft,
    ScrollKanbanRight,
    // Task operations
    CreateTask,
    EditTask,
    MoveForward,
    MoveBackward,
    MoveTaskUp,
    MoveTaskDown,
    DeleteTask,
    ViewTask,
    AbortSession,
    // Project operations
    SetWorkingDirectory,
    DeleteProject,
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
        parse_and_add(&mut bindings, &config.rename_project, Action::RenameProject);
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
        parse_and_add(&mut bindings, &config.task_move_up, Action::MoveTaskUp);
        parse_and_add(&mut bindings, &config.task_move_down, Action::MoveTaskDown);
        parse_and_add(&mut bindings, &config.task_delete, Action::DeleteTask);
        parse_and_add(&mut bindings, &config.task_view, Action::ViewTask);
        parse_and_add(&mut bindings, &config.abort_session, Action::AbortSession);
        parse_and_add(
            &mut bindings,
            &config.scroll_kanban_left,
            Action::ScrollKanbanLeft,
        );
        parse_and_add(
            &mut bindings,
            &config.scroll_kanban_right,
            Action::ScrollKanbanRight,
        );
        parse_and_add(
            &mut bindings,
            &config.set_working_directory,
            Action::SetWorkingDirectory,
        );
        parse_and_add(
            &mut bindings,
            &config.delete_project,
            Action::DeleteProject,
        );

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

    // ── Default keybinding action matching ──────────────────────────────

    #[test]
    fn all_default_actions_match_expected_keys() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        // Quit: ctrl+q
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );

        // NavLeft: h or Left
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            Some(Action::NavLeft)
        );
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::NavLeft)
        );

        // NavRight: l or Right
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::NavRight)
        );
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(Action::NavRight)
        );

        // NavUp: k or Up
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(Action::NavUp)
        );
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::NavUp)
        );

        // NavDown: j or Down
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(Action::NavDown)
        );
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(Action::NavDown)
        );

        // CreateTask: n
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(Action::CreateTask)
        );

        // EditTask: e
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            Some(Action::EditTask)
        );

        // MoveForward: m
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
            Some(Action::MoveForward)
        );

        // MoveBackward: shift+m
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::SHIFT)),
            Some(Action::MoveBackward)
        );

        // DeleteTask: x
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(Action::DeleteTask)
        );

        // ViewTask: v
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)),
            Some(Action::ViewTask)
        );

        // HelpToggle: ?
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
            Some(Action::HelpToggle)
        );

        // PrevProject: ctrl+k
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Action::PrevProject)
        );

        // NextProject: ctrl+j
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(Action::NextProject)
        );

        // NewProject: ctrl+n
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Action::NewProject)
        );
    }

    #[test]
    fn unmatched_key_returns_none() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        // 'z' has no binding
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
            None
        );

        // F1 has no binding
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            None
        );

        // ctrl+z has no binding
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn case_insensitive_matching_without_shift() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        // 'N' (uppercase without explicit shift) should still match
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE)),
            Some(Action::CreateTask)
        );
    }

    #[test]
    fn shift_modifier_requires_exact_case() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        // shift+m matches MoveBackward (binding is "shift+m" → Char('m') + SHIFT)
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::SHIFT)),
            Some(Action::MoveBackward)
        );

        // When shift is held, matching is case-EXACT, so 'M' + SHIFT does NOT
        // match the 'm' + SHIFT binding.
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT)),
            None
        );
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let config = KeybindingConfig::default();
        let matcher = KeyMatcher::from_config(&config);

        // ctrl+h should NOT match NavLeft (which is just 'h' with no modifiers)
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            None
        );

        // alt+h should NOT match NavLeft
        assert_eq!(
            matcher.match_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT)),
            None
        );
    }

    // ── Parse edge cases ────────────────────────────────────────────────

    #[test]
    fn parse_key_combo_with_spaces_returns_none() {
        // The parser splits on '+' without trimming, so "ctrl + q" won't parse.
        // "ctrl " and " q" don't match known keys.
        assert!(parse_key_combo("ctrl + q").is_none());
    }

    #[test]
    fn parse_key_combo_unknown_returns_none() {
        assert!(parse_key_combo("frodo").is_none());
        assert!(parse_key_combo("").is_none());
    }

    #[test]
    fn parse_key_combo_space_key() {
        let key = parse_key_combo("space").unwrap();
        assert_eq!(key.code, KeyCode::Char(' '));
        assert_eq!(key.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn parse_key_combo_special_keys() {
        assert!(parse_key_combo("enter").is_some());
        assert!(parse_key_combo("tab").is_some());
        assert!(parse_key_combo("escape").is_some());
        assert!(parse_key_combo("esc").is_some());
        assert!(parse_key_combo("backspace").is_some());
        assert!(parse_key_combo("delete").is_some());
        assert!(parse_key_combo("home").is_some());
        assert!(parse_key_combo("end").is_some());
        assert!(parse_key_combo("pageup").is_some());
        assert!(parse_key_combo("pagedown").is_some());
    }
}
