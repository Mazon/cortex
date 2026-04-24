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

    // Escape → Cancel (with unsaved changes warning)
    if key.code == KeyCode::Esc {
        if editor.discard_warning_shown {
            // Second Esc: user confirmed they want to discard
            return EditorAction::Cancel;
        }
        if editor.has_unsaved_changes {
            // First Esc with unsaved changes: show warning, don't cancel yet
            editor.discard_warning_shown = true;
            return EditorAction::None;
        }
        // No unsaved changes: cancel immediately
        return EditorAction::Cancel;
    }

    // Tab → Cycle focus between Description and Column
    if key.code == KeyCode::Tab {
        match editor.focused_field {
            EditorField::Description => {
                if !editor.available_columns.is_empty() {
                    editor.cycle_column();
                    editor.focused_field = EditorField::Column;
                }
            }
            EditorField::Column => {
                editor.focused_field = EditorField::Description;
            }
        }
        return EditorAction::None;
    }

    // Enter → Insert newline (in description) — always, since there's no title field
    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::CONTROL) {
        match editor.focused_field {
            EditorField::Description => {
                editor.insert_newline();
            }
            EditorField::Column => {}
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
            editor.scroll_offset = (editor.scroll_offset + 5)
                .min(editor.desc_lines.len().saturating_sub(1));
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

    /// Helper to create a fresh editor in create mode (description field focused).
    fn new_editor() -> TaskEditorState {
        TaskEditorState::new_for_create("todo", Vec::new())
    }

    fn new_editor_with_columns() -> TaskEditorState {
        TaskEditorState::new_for_create("todo", vec!["todo".to_string(), "doing".to_string(), "done".to_string()])
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
    fn tab_stays_on_description_when_no_columns() {
        let mut editor = new_editor();
        assert_eq!(editor.focused_field, EditorField::Description);
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    #[test]
    fn tab_cycles_description_to_column() {
        let mut editor = new_editor_with_columns();
        assert_eq!(editor.focused_field, EditorField::Description);
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Column);
    }

    #[test]
    fn tab_cycles_column_back_to_description() {
        let mut editor = new_editor_with_columns();
        editor.focused_field = EditorField::Column;
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    #[test]
    fn tab_cycles_back_and_forth() {
        let mut editor = new_editor_with_columns();
        assert_eq!(editor.focused_field, EditorField::Description);

        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Column);

        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    // ── Enter inserts newline in description ────────────────────────────

    #[test]
    fn enter_inserts_newline_in_description() {
        let mut editor = new_editor();
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
        editor.set_description("line1");
        // Cursor at position 3 (between "lin" and "e1")
        editor.cursor_col = 3;
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.description(), "lin\ne1");
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
    }

    // ── Character insertion ─────────────────────────────────────────────

    #[test]
    fn char_insert_in_description() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, char_key('A'));
        handle_editor_input(&mut editor, char_key('B'));
        assert_eq!(editor.description(), "AB");
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn ctrl_char_is_ignored() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, ctrl_char_key('c'));
        assert_eq!(editor.description(), "");
    }

    #[test]
    fn alt_char_is_ignored() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, key(KeyCode::Char('a'), KeyModifiers::ALT));
        assert_eq!(editor.description(), "");
    }

    // ── Backspace ───────────────────────────────────────────────────────

    #[test]
    fn backspace_deletes_in_description() {
        let mut editor = new_editor();
        editor.set_description("hello");
        editor.cursor_row = 0;
        editor.cursor_col = 3;
        handle_editor_input(&mut editor, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.description(), "helo");
        assert_eq!(editor.cursor_col, 2);
    }

    // ── Delete ──────────────────────────────────────────────────────────

    #[test]
    fn delete_forward_in_description() {
        let mut editor = new_editor();
        editor.set_description("abc");
        editor.cursor_row = 0;
        editor.cursor_col = 1;
        handle_editor_input(&mut editor, key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(editor.description(), "ac");
    }

    // ── Arrow keys ──────────────────────────────────────────────────────

    #[test]
    fn left_arrow_moves_cursor_left_in_description() {
        let mut editor = new_editor();
        editor.set_description("abc");
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
    fn right_arrow_moves_cursor_right_in_description() {
        let mut editor = new_editor();
        editor.set_description("abc");
        editor.cursor_col = 1;
        handle_editor_input(&mut editor, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn right_arrow_does_not_exceed_length() {
        let mut editor = new_editor();
        editor.set_description("abc");
        editor.cursor_col = 3; // at end
        handle_editor_input(&mut editor, key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn home_moves_to_start_of_line() {
        let mut editor = new_editor();
        editor.set_description("abc");
        editor.cursor_col = 2;
        handle_editor_input(&mut editor, key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn end_moves_to_end_of_line() {
        let mut editor = new_editor();
        editor.set_description("abc");
        editor.cursor_col = 0;
        handle_editor_input(&mut editor, key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn up_down_in_description() {
        let mut editor = new_editor();
        editor.set_description("line0\nline1\nline2");
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
        editor.desc_lines = (0..20).map(|i| format!("line {}", i)).collect();
        editor.scroll_offset = 0;
        handle_editor_input(&mut editor, key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(editor.scroll_offset, 5);
    }

    #[test]
    fn pagedown_clamps_at_last_line() {
        let mut editor = new_editor();
        editor.desc_lines = (0..5).map(|i| format!("line {}", i)).collect();
        editor.scroll_offset = 0;
        handle_editor_input(&mut editor, key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(editor.scroll_offset, 4); // clamped to desc_lines.len() - 1
    }

    // ── Unmatched key returns None ──────────────────────────────────────

    #[test]
    fn f1_key_returns_none() {
        let mut editor = new_editor();
        let action = handle_editor_input(&mut editor, key(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
    }

    // ── Integration: type description, switch field, type ───────────────

    #[test]
    fn full_editing_workflow() {
        let mut editor = new_editor_with_columns();

        // Type a description
        for ch in "My Task".chars() {
            handle_editor_input(&mut editor, char_key(ch));
        }
        assert_eq!(editor.description(), "My Task");

        // Tab to column
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Column);

        // Tab back to description
        handle_editor_input(&mut editor, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(editor.focused_field, EditorField::Description);

        // First Esc shows warning (unsaved changes exist)
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
        assert!(editor.discard_warning_shown);

        // Second Esc confirms discard
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Cancel);
    }

    // ── Unsaved changes warning ─────────────────────────────────────────

    #[test]
    fn esc_without_changes_cancels_immediately() {
        let mut editor = new_editor();
        // No edits made — Esc should cancel immediately
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Cancel);
    }

    #[test]
    fn first_esc_with_changes_shows_warning() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, char_key('H'));
        assert!(editor.has_unsaved_changes);

        // First Esc: shows warning, does not cancel
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
        assert!(editor.discard_warning_shown);
    }

    #[test]
    fn second_esc_with_warning_confirms_discard() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, char_key('H'));

        // First Esc: warning
        handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(editor.discard_warning_shown);

        // Second Esc: cancel
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Cancel);
    }

    #[test]
    fn typing_after_warning_clears_discard_flag() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, char_key('A'));

        // First Esc: warning
        handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(editor.discard_warning_shown);

        // Type another character — should clear the warning flag
        handle_editor_input(&mut editor, char_key('B'));
        assert!(!editor.discard_warning_shown);
        assert!(editor.has_unsaved_changes);

        // Next Esc should show warning again (not cancel)
        let action = handle_editor_input(&mut editor, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::None);
        assert!(editor.discard_warning_shown);
    }

    #[test]
    fn backspace_sets_unsaved_changes() {
        let mut editor = new_editor();
        editor.set_description("ab");
        editor.cursor_col = 2;
        handle_editor_input(&mut editor, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(editor.has_unsaved_changes);
    }

    #[test]
    fn delete_forward_sets_unsaved_changes() {
        let mut editor = new_editor();
        editor.set_description("ab");
        editor.cursor_col = 0;
        handle_editor_input(&mut editor, key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(editor.has_unsaved_changes);
    }

    #[test]
    fn newline_sets_unsaved_changes() {
        let mut editor = new_editor();
        handle_editor_input(&mut editor, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(editor.has_unsaved_changes);
    }
}
