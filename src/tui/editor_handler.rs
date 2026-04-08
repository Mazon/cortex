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
