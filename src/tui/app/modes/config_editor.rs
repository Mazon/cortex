//! Config editor mode key handler — edit cortex.toml with validation and hot-reload.

use super::super::App;

/// Handle key events in ConfigEditor mode.
///
/// - Arrow keys / Home / End / PageUp / PageDown: navigate
/// - Character input: edit text
/// - Enter: insert newline
/// - Backspace / Delete: delete characters
/// - Tab: insert tab (or two spaces)
/// - Ctrl+S: save, validate, and hot-reload if valid
/// - Esc: cancel and return to Normal mode (discard changes)
pub fn handle_config_editor_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    use crate::state::types::CursorDirection;

    // Ctrl+S: save, validate, and hot-reload
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        save_and_reload(app);
        return;
    }

    // Esc: cancel and return to Normal mode
    if key.code == KeyCode::Esc {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.config_editor_state = None;
        state.ui.mode = crate::state::types::AppMode::Normal;
        state.mark_render_dirty();
        return;
    }

    // Navigation and editing keys
    match key.code {
        KeyCode::Up => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::Up);
            }
            state.mark_render_dirty();
        }
        KeyCode::Down => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::Down);
            }
            state.mark_render_dirty();
        }
        KeyCode::Left => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::Left);
            }
            state.mark_render_dirty();
        }
        KeyCode::Right => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::Right);
            }
            state.mark_render_dirty();
        }
        KeyCode::Home => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::Home);
            }
            state.mark_render_dirty();
        }
        KeyCode::End => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.move_cursor(CursorDirection::End);
            }
            state.mark_render_dirty();
        }
        KeyCode::PageUp => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                let jump = 10usize;
                ed.scroll_offset = ed.scroll_offset.saturating_sub(jump);
                if ed.cursor_row > ed.scroll_offset {
                    ed.cursor_row = ed.scroll_offset;
                }
                let line_len = ed.lines.get(ed.cursor_row).map_or(0, |l| l.chars().count());
                ed.cursor_col = ed.cursor_col.min(line_len);
            }
            state.mark_render_dirty();
        }
        KeyCode::PageDown => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                let jump = 10usize;
                let max_scroll = ed.lines.len().saturating_sub(1);
                ed.scroll_offset = (ed.scroll_offset + jump).min(max_scroll);
                ed.cursor_row = ed.cursor_row.saturating_add(jump).min(max_scroll);
                let line_len = ed.lines.get(ed.cursor_row).map_or(0, |l| l.chars().count());
                ed.cursor_col = ed.cursor_col.min(line_len);
            }
            state.mark_render_dirty();
        }
        KeyCode::Char(c) => {
            // Ignore Ctrl/Alt combos (except Tab handled separately)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                return;
            }
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.insert_char(c);
            }
            state.mark_render_dirty();
        }
        KeyCode::Enter => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.insert_newline();
            }
            state.mark_render_dirty();
        }
        KeyCode::Backspace => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.delete_char_back();
            }
            state.mark_render_dirty();
        }
        KeyCode::Delete => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.delete_char_forward();
            }
            state.mark_render_dirty();
        }
        KeyCode::Tab => {
            // Insert 2 spaces (common TOML convention)
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.insert_char(' ');
                ed.insert_char(' ');
            }
            state.mark_render_dirty();
        }
        _ => {}
    }
}

/// Save the config file, validate it, and hot-reload if valid.
///
/// On success:
/// 1. Write content to disk
/// 2. Parse and validate TOML
/// 3. Replace `App.config` with new config
/// 4. Rebuild `KeyMatcher` and `EditorKeyMatcher`
/// 5. Return to Normal mode with success notification
///
/// On failure:
/// 1. Show validation error inline in the editor
/// 2. Stay in ConfigEditor mode so user can fix the error
fn save_and_reload(app: &mut App) {
    let content = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .ui
            .config_editor_state
            .as_ref()
            .map(|ed| ed.content())
    };

    let content = match content {
        Some(c) => c,
        None => return,
    };

    // Write content to disk
    if let Err(e) = std::fs::write(&app.config_path, &content) {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut ed) = state.ui.config_editor_state {
            ed.validation_error = Some(format!("Failed to write file: {}", e));
        }
        state.mark_render_dirty();
        return;
    }

    // Parse and validate
    match crate::config::reload_config(&app.config_path) {
        Ok(new_config) => {
            // Hot-reload: replace config and rebuild key matchers
            app.config = new_config;
            app.key_matcher =
                crate::tui::keys::KeyMatcher::from_config(&app.config.keybindings);
            app.editor_key_matcher =
                crate::tui::keys::EditorKeyMatcher::from_config(&app.config.keybindings.editor);

            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.config_editor_state = None;
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.set_notification(
                "Config saved and reloaded".to_string(),
                crate::state::types::NotificationVariant::Success,
                3000,
            );
            state.mark_render_dirty();
        }
        Err(errors) => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut ed) = state.ui.config_editor_state {
                ed.validation_error = Some(errors.join("; "));
            }
            state.mark_render_dirty();
        }
    }
}

/// Open the config editor by reading the config file and entering ConfigEditor mode.
pub fn open_config_editor(app: &mut App) {
    use crate::state::types::ConfigEditorState;

    let editor_state = match ConfigEditorState::from_path(&app.config_path) {
        Some(s) => s,
        None => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                format!(
                    "Cannot open config file: {}",
                    app.config_path.display()
                ),
                crate::state::types::NotificationVariant::Error,
                3000,
            );
            return;
        }
    };

    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.ui.config_editor_state = Some(editor_state);
    state.ui.mode = crate::state::types::AppMode::ConfigEditor;
    state.mark_render_dirty();
}
