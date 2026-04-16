//! Editor key handler — fixed (non-configurable) keybindings for the task editor.

use crate::state::types::{CursorDirection, EditorField, TaskEditorState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Result of handling an editor key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    None,
    Save,
    Cancel,
}

/// Handle a key event in the task editor.
/// Returns an EditorAction if the key triggers a mode transition.
pub fn handle_editor_input(editor: &mut TaskEditorState, key: KeyEvent) -> EditorAction {
    // Ctrl+S or Ctrl+Enter → Save
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('s') || key.code == KeyCode::Enter)
    {
        return EditorAction::Save;
    }

    // Escape → Cancel
    if key.code == KeyCode::Esc {
        return EditorAction::Cancel;
    }

    // Tab → Cycle focus
    if key.code == KeyCode::Tab {
        editor.focused_field = match editor.focused_field {
            EditorField::Title => EditorField::Description,
            EditorField::Description => EditorField::Title,
        };
        return EditorAction::None;
    }

    // Enter → Next field (from title) or newline (in description)
    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::CONTROL) {
        match editor.focused_field {
            EditorField::Title => {
                editor.focused_field = EditorField::Description;
                editor.cursor_row = 0;
                editor.cursor_col = 0;
            }
            EditorField::Description => {
                editor.insert_newline();
            }
        }
        return EditorAction::None;
    }

    // Backspace
    if key.code == KeyCode::Backspace {
        editor.delete_char_back();
        return EditorAction::None;
    }

    // Delete
    if key.code == KeyCode::Delete {
        editor.delete_char_forward();
        return EditorAction::None;
    }

    // Arrow keys
    match key.code {
        KeyCode::Up => {
            editor.move_cursor(CursorDirection::Up);
        }
        KeyCode::Down => {
            editor.move_cursor(CursorDirection::Down);
        }
        KeyCode::Left => {
            editor.move_cursor(CursorDirection::Left);
        }
        KeyCode::Right => {
            editor.move_cursor(CursorDirection::Right);
        }
        KeyCode::Home => {
            editor.move_cursor(CursorDirection::Home);
        }
        KeyCode::End => {
            editor.move_cursor(CursorDirection::End);
        }
        KeyCode::PageUp => {
            editor.scroll_offset = editor.scroll_offset.saturating_sub(5);
        }
        KeyCode::PageDown => {
            editor.scroll_offset = editor.scroll_offset + 5;
        }
        _ => {}
    }

    // Printable characters
    if let KeyCode::Char(ch) = key.code {
        // Ignore ctrl/alt combinations for printable chars
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return EditorAction::None;
        }
        editor.insert_char(ch);
    }

    EditorAction::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::types::{EditorField, TaskEditorState};

    /// Helper to create a fresh editor in create mode (title field focused).
    fn new_editor() -> TaskEditorState {
        TaskEditorState::new_for_create("todo")
    }

    /// Helper to build a crossterm KeyEvent.
    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn char_key(ch: char) -> KeyEvent {
        key(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    fn ctrl_char_key(ch: char) -> KeyEvent {
        key(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    // ── Save / Cancel ───────────────────────────────────────────────────

    #[test]
    fn escape_returns_cancel() {
        let mut editor = new_editor();
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Cancel);
    }

    #[test]
    fn ctrl_s_returns_save() {
        let mut editor = new_editor();
        let action = handle_editor_input(&mut editor, ctrl_char_key('s'));
        assert_eq!(action, EditorAction::Save);
    }

    #[test]
    fn ctrl_enter_returns_save() {
        let mut editor = new_editor();
        let action = handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::Save);
    }

    // ── Tab focus cycling ───────────────────────────────────────────────

    #[test]
    fn tab_toggles_title_to_description() {
        let mut editor = new_editor();
        assert_eq!(editor.focused_field, EditorField::Title);
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    #[test]
    fn tab_toggles_description_to_title() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Title);
    }

    #[test]
    fn tab_cycles_back_and_forth() {
        let mut editor = new_editor();
        assert_eq!(editor.focused_field, EditorField::Title);

        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);

        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Title);
    }

    // ── Enter field switching ───────────────────────────────────────────

    #[test]
    fn enter_moves_from_title_to_description() {
        let mut editor = new_editor();
        assert_eq!(editor.focused_field, EditorField::Title);
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn enter_inserts_newline_in_description() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        editor.set_description("line1");
        // Cursor at end of "line1" (col=5)
        editor.cursor_col = 5;
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.description(), "line1\n");
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn enter_splits_line_at_cursor() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        editor.description = "line1".to_string();
        // Cursor at position 3 (between "lin" and "e1")
        editor.cursor_col = 3;
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.description, "lin\ne1");
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
    }

    // ── Character insertion ─────────────────────────────────────────────

    #[test]
    fn char_insert_in_title() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, char_key('H'));
        handle_editor_input(&mut editor, char_key('i'));
        assert_eq!(editor.title, "Hi");
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn char_insert_in_description() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        handle_editor_input(&mut editor, char_key('A'));
        handle_editor_input(&mut editor, char_key('B'));
        assert_eq!(editor.description, "AB");
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn ctrl_char_is_ignored() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, ctrl_char_key('c'));
        assert_eq!(editor.title, "");
    }

    #[test]
    fn alt_char_is_ignored() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, key(KeyCode::Char('a'), KeyModifiers::ALT));
        assert_eq!(editor.title, "");
    }

    // ── Backspace ───────────────────────────────────────────────────────

    #[test]
    fn backspace_deletes_in_title() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 2;
        handle_editor_input(&mut editor, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.title, "ac");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn backspace_at_start_of_title_is_noop() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 0;
        handle_editor_input(&mut editor, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.title, "abc");
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn backspace_deletes_in_description() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        editor.description = "hello".to_string();
        editor.cursor_row = 0;
        editor.cursor_col = 3;
        handle_editor_input(&mut editor, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.description, "helo");
        assert_eq!(editor.cursor_col, 2);
    }

    // ── Delete ──────────────────────────────────────────────────────────

    #[test]
    fn delete_forward_in_title() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 1;
        handle_editor_input(&mut editor, key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(editor.title, "ac");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn delete_at_end_of_title_is_noop() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 3; // at end
        handle_editor_input(&mut editor, key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(editor.title, "abc");
    }

    // ── Arrow keys ──────────────────────────────────────────────────────

    #[test]
    fn left_arrow_moves_cursor_left_in_title() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 2;
        handle_editor_input(&mut editor, key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn left_arrow_does_not_go_negative() {
        let mut editor = new_editor();
        editor.cursor_col = 0;
        handle_editor_input(&mut editor, key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn right_arrow_moves_cursor_right_in_title() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 1;
        handle_editor_input(&mut editor, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn right_arrow_does_not_exceed_length() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 3; // at end
        handle_editor_input(&mut editor, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn home_moves_to_start_of_line() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 2;
        handle_editor_input(&mut editor, key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn end_moves_to_end_of_line() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_col = 0;
        handle_editor_input(&mut editor, key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn up_down_in_description() {
        let mut editor = new_editor();
        editor.focused_field = EditorField::Description;
        editor.description = "line0\nline1\nline2".to_string();
        editor.cursor_row = 2;
        editor.cursor_col = 3;

        // Move up
        handle_editor_input(&mut editor, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 1);

        // Move up again
        handle_editor_input(&mut editor, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 0);

        // Can't move up from row 0
        handle_editor_input(&mut editor, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 0);

        // Move down
        handle_editor_input(&mut editor, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 1);

        // Move down
        handle_editor_input(&mut editor, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 2);

        // Can't move down past last line
        handle_editor_input(&mut editor, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 2);
    }

    #[test]
    fn up_down_does_nothing_in_title_field() {
        let mut editor = new_editor();
        editor.title = "abc".to_string();
        editor.cursor_row = 0;
        // Up/Down should be no-ops in title field
        handle_editor_input(&mut editor, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 0);
        handle_editor_input(&mut editor, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(editor.cursor_row, 0);
    }

    // ── PageUp / PageDown scrolling ─────────────────────────────────────

    #[test]
    fn pageup_decreases_scroll_offset() {
        let mut editor = new_editor();
        editor.scroll_offset = 10;
        handle_editor_input(&mut editor, key(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(editor.scroll_offset, 5);
    }

    #[test]
    fn pageup_clamps_at_zero() {
        let mut editor = new_editor();
        editor.scroll_offset = 3;
        handle_editor_input(&mut editor, key(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(editor.scroll_offset, 0);
    }

    #[test]
    fn pagedown_increases_scroll_offset() {
        let mut editor = new_editor();
        editor.scroll_offset = 0;
        handle_editor_input(&mut editor, key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(editor.scroll_offset, 5);
    }

    // ── Unmatched key returns None ──────────────────────────────────────

    #[test]
    fn f1_key_returns_none() {
        let mut editor = new_editor();
        let action = handle_editor_input(&mut editor, key(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
    }

    // ── Integration: type title, switch field, type description ─────────

    #[test]
    fn full_editing_workflow() {
        let mut editor = new_editor();

        // Type a title
        for ch in "My Task".chars() {
            handle_editor_input(&mut editor, char_key(ch));
        }
        assert_eq!(editor.title, "My Task");

        // Enter to switch to description
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);

        // Type description
        for ch in "Do something".chars() {
            handle_editor_input(&mut editor, char_key(ch));
        }
        assert_eq!(editor.description, "Do something");

        // Tab back to title
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Title);

        // Cancel
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Cancel);
    }
}
