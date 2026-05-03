//! Diff review mode key handler.

use super::super::App;

/// Handle key events in DiffReview mode.
pub fn handle_diff_review_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            let source = state.ui.diff_review_source.take();
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.ui.diff_review = None;
            // If we came from task detail, restore that focus
            if source == Some(crate::state::types::FocusedPanel::TaskDetail) {
                state.ui.focused_panel = crate::state::types::FocusedPanel::TaskDetail;
                // viewing_task_id should still be set
            }
        }
        KeyCode::Tab => {
            // Toggle focus between file list and diff content
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                review.files_list_focused = !review.files_list_focused;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                if review.files_list_focused {
                    // Navigate files in the list
                    let max = review.files.len().saturating_sub(1);
                    if review.selected_file_index < max {
                        review.selected_file_index += 1;
                        review.scroll_offset = 0;
                    }
                } else {
                    crate::tui::diff_view::scroll_diff(review, 1);
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                if review.files_list_focused {
                    // Navigate files in the list
                    if review.selected_file_index > 0 {
                        review.selected_file_index -= 1;
                        review.scroll_offset = 0;
                    }
                } else {
                    crate::tui::diff_view::scroll_diff(review, -1);
                }
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                if review.files_list_focused {
                    // Switch focus to diff content
                    review.files_list_focused = false;
                } else {
                    crate::tui::diff_view::next_file(review);
                }
            }
        }
        KeyCode::Char('h') | KeyCode::Left => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                if !review.files_list_focused {
                    // Switch focus to file list
                    review.files_list_focused = true;
                } else {
                    crate::tui::diff_view::prev_file(review);
                }
            }
        }
        // Keep existing bracket navigation
        KeyCode::Char(']') => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                crate::tui::diff_view::next_file(review);
            }
        }
        KeyCode::Char('[') | KeyCode::Backspace => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut review) = state.ui.diff_review {
                crate::tui::diff_view::prev_file(review);
            }
        }
        _ => {}
    }
}
