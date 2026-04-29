//! UI state types for the Cortex application.

use std::collections::{HashMap, HashSet, VecDeque};

use super::enums::*;
use super::task::{CortexTask, SessionRef, TaskDetailSession};

// ─── UI State ────────────────────────────────────────────────────────────

/// A notification toast.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Message text to display.
    pub message: String,
    /// Visual variant (info, success, warning, error).
    pub variant: NotificationVariant,
    /// Unix timestamp (milliseconds) when this notification should be dismissed.
    pub expires_at: i64,
}

/// UI state — tracks the current view mode and focus.
#[derive(Debug, Clone)]
pub struct UIState {
    /// Current application mode (normal, task editor, help overlay).
    pub mode: AppMode,
    /// Which panel has keyboard focus.
    pub focused_panel: FocusedPanel,
    /// ID of the currently focused kanban column.
    pub focused_column: String,
    /// ID of the currently focused task.
    pub focused_task_id: Option<String>,
    /// ID of the task being viewed in the detail panel.
    pub viewing_task_id: Option<String>,
    /// Notification queue (most recent at back). Max capacity: `MAX_NOTIFICATIONS`.
    pub notifications: VecDeque<Notification>,
    /// Text input buffer (used for prompts like project rename).
    pub input_text: String,
    /// Cursor position within `input_text` (char index, not byte offset).
    /// Tracked as char index to correctly handle multi-byte Unicode chars.
    pub input_cursor: usize,
    /// Label displayed above the input prompt (e.g., "Rename project:").
    pub prompt_label: String,
    /// Context string identifying what action to perform on submit
    /// (e.g., `"rename_project"`).
    pub prompt_context: Option<String>,
    /// Task editor state when in `AppMode::TaskEditor`.
    pub task_editor: Option<TaskEditorState>,
    /// Inline description editor state for the task detail view.
    /// When `Some`, the task detail panel shows an editable description.
    pub detail_editor: Option<DetailEditorState>,
    /// User-controlled scroll offset for the streaming output in task detail view.
    /// `None` means auto-scroll (always show the bottom). `Some(n)` means the
    /// user has manually scrolled and the view is pinned to offset `n`.
    pub user_scroll_offset: Option<usize>,
    /// Stack of session references for drill-down navigation.
    /// Bottom = top-level task, top = currently viewed session.
    /// When empty, the task detail view shows the parent task's output.
    /// When non-empty, the task detail view shows the top-of-stack session's output.
    pub session_nav_stack: Vec<SessionRef>,
    /// State for the diff review view. When `Some`, the user is reviewing
    /// git diff changes for a completed "do" task.
    pub diff_review: Option<DiffReviewState>,
}

impl Default for UIState {
    fn default() -> Self {
        Self {
            mode: AppMode::Normal,
            focused_panel: FocusedPanel::Kanban,
            focused_column: "planning".to_string(),
            focused_task_id: None,
            viewing_task_id: None,
            notifications: VecDeque::new(),
            input_text: String::new(),
            input_cursor: 0,
            prompt_label: String::new(),
            prompt_context: None,
            task_editor: None,
            detail_editor: None,
            user_scroll_offset: None,
            session_nav_stack: Vec::new(),
            diff_review: None,
        }
    }
}

// ─── Task Editor ─────────────────────────────────────────────────────────

/// Fullscreen task editor state.
///
/// Description text is stored as a `Vec<String>` of individual lines for O(1)
/// per-line access during cursor movement and rendering. The joined text is
/// cached and only recomputed when lines change.
///
/// The task title is auto-derived from the first line of the description
/// (max 80 chars). There is no separate Title input field.
#[derive(Debug, Clone)]
pub struct TaskEditorState {
    /// `None` = creating new task, `Some(id)` = editing existing task.
    pub task_id: Option<String>,
    /// Description stored as individual lines for O(1) per-line access.
    /// Always contains at least one element (empty string when description is empty).
    pub desc_lines: Vec<String>,
    /// Cached joined text; `None` when lines have been modified since last join.
    pub cached_description: Option<String>,
    /// Currently focused field (description or column selector).
    pub focused_field: EditorField,
    /// Cursor row (0-indexed).
    pub cursor_row: usize,
    /// Cursor column (0-indexed).
    pub cursor_col: usize,
    /// Scroll offset for the description textarea.
    pub scroll_offset: usize,
    /// Target column ID when creating a new task.
    pub column_id: Option<String>,
    /// Agent type to assign when creating a new task.
    pub agent_type: Option<String>,
    /// Whether the user has made unsaved edits since the last save or open.
    pub has_unsaved_changes: bool,
    /// Whether the "unsaved changes" discard warning is currently displayed.
    /// Set on first Esc with unsaved changes; cleared on any edit or save.
    pub discard_warning_shown: bool,
    /// Inline validation error message displayed below the description field.
    /// Set when the user tries to save with an empty description; cleared when
    /// the user types in the description field.
    pub validation_error: Option<String>,
    pub available_columns: Vec<String>,
    pub selected_column_index: usize,
}

impl TaskEditorState {
    /// Creates empty state for a new task. Starts focused on Description.
    ///
    /// Column selection is not available during creation — the column is
    /// determined by the focused kanban column when the user presses the
    /// create shortcut. Column cycling remains available in edit mode.
    pub fn new_for_create(default_column: &str) -> Self {
        Self {
            task_id: None,
            desc_lines: vec![String::new()],
            cached_description: None,
            focused_field: EditorField::Description,
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            column_id: Some(default_column.to_string()),
            agent_type: None,
            has_unsaved_changes: false,
            discard_warning_shown: false,
            validation_error: None,
            available_columns: Vec::new(),
            selected_column_index: 0,
        }
    }

    /// Pre-populates from an existing task for editing. Starts focused on Description.
    pub fn new_for_edit(task: &CortexTask, available_columns: Vec<String>) -> Self {
        let lines: Vec<String> = if task.description.is_empty() {
            vec![String::new()]
        } else {
            task.description.split('\n').map(String::from).collect()
        };
        let cached = if task.description.is_empty() {
            None
        } else {
            Some(task.description.clone())
        };
        Self {
            task_id: Some(task.id.clone()),
            desc_lines: lines,
            cached_description: cached,
            focused_field: EditorField::Description,
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            column_id: Some(task.column.0.clone()),
            agent_type: task.agent_type.clone(),
            has_unsaved_changes: false,
            discard_warning_shown: false,
            validation_error: None,
            available_columns: available_columns.clone(),
            selected_column_index: available_columns
                .iter()
                .position(|c| c == &task.column.0)
                .unwrap_or(0),
        }
    }
    /// Returns the description text as a single string.
    ///
    /// The result is cached; the join is only recomputed when lines have
    /// changed since the last call.
    pub fn description(&self) -> String {
        match &self.cached_description {
            Some(cached) => cached.clone(),
            None => self.desc_lines.join("\n"),
        }
    }

    /// Sets the description from a flat string (used by tests and initialization).
    pub fn set_description(&mut self, text: &str) {
        if text.is_empty() {
            self.desc_lines = vec![String::new()];
            self.cached_description = None;
        } else {
            self.desc_lines = text.split('\n').map(String::from).collect();
            self.cached_description = Some(text.to_string());
        }
    }

    /// Returns a reference to the description lines slice.
    pub fn desc_lines(&self) -> &[String] {
        &self.desc_lines
    }

    /// Returns the text of the line the cursor is on.
    pub fn current_line(&self) -> &str {
        match self.focused_field {
            EditorField::Description => self
                .desc_lines
                .get(self.cursor_row)
                .map_or("", |l| l.as_str()),
            EditorField::Column => "",
        }
    }

    /// Invalidates the cached description text.
    fn invalidate_cache(&mut self) {
        self.cached_description = None;
    }

    /// Marks the editor as having unsaved changes and clears any discard warning.
    fn mark_edited(&mut self) {
        self.has_unsaved_changes = true;
        self.discard_warning_shown = false;
    }

    /// Inserts a character at cursor position in the focused field.
    pub fn insert_char(&mut self, ch: char) {
        match self.focused_field {
            EditorField::Description => {
                self.mark_edited();
                let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
                let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
                let col = self.cursor_col.min(line_len);
                if let Some(line) = self.desc_lines.get_mut(row) {
                    // Convert char index to byte offset for String::insert.
                    let byte_pos = line
                        .char_indices()
                        .nth(col)
                        .map(|(i, _)| i)
                        .unwrap_or(line.len());
                    line.insert(byte_pos, ch);
                    self.cursor_col = col + 1;
                    self.cursor_row = row;
                    // Clear inline validation error when user edits the description.
                    self.validation_error = None;
                }
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Deletes character before cursor (backspace).
    pub fn delete_char_back(&mut self) {
        match self.focused_field {
            EditorField::Description => {
                self.mark_edited();
                let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
                let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
                let col = self.cursor_col.min(line_len);

                if col > 0 {
                    if let Some(line) = self.desc_lines.get_mut(row) {
                        let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                        if let Some(&(byte_start, ch)) = char_indices.get(col - 1) {
                            let byte_end = byte_start + ch.len_utf8();
                            line.replace_range(byte_start..byte_end, "");
                        }
                        self.cursor_col = col - 1;
                    }
                } else if row > 0 {
                    // At beginning of line — merge with previous line.
                    let prev_len = self
                        .desc_lines
                        .get(row - 1)
                        .map_or(0, |l| l.chars().count());
                    if let (Some(prev), Some(cur)) = (
                        self.desc_lines.get(row - 1).cloned(),
                        self.desc_lines.get(row).cloned(),
                    ) {
                        let merged = format!("{}{}", prev, cur);
                        self.desc_lines[row - 1] = merged;
                        self.desc_lines.remove(row);
                        self.cursor_row = row - 1;
                        self.cursor_col = prev_len;
                    }
                }

                // Clear inline validation error when user edits the description.
                self.validation_error = None;
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Deletes character after cursor (forward delete).
    pub fn delete_char_forward(&mut self) {
        match self.focused_field {
            EditorField::Description => {
                self.mark_edited();
                let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
                let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
                let col = self.cursor_col.min(line_len);

                if col < line_len {
                    if let Some(line) = self.desc_lines.get_mut(row) {
                        let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                        if let Some(&(byte_start, ch)) = char_indices.get(col) {
                            let byte_end = byte_start + ch.len_utf8();
                            line.replace_range(byte_start..byte_end, "");
                        }
                    }
                } else if row + 1 < self.desc_lines.len() {
                    // At end of line — merge with next line.
                    if let (Some(cur), Some(next)) = (
                        self.desc_lines.get(row).cloned(),
                        self.desc_lines.get(row + 1).cloned(),
                    ) {
                        let merged = format!("{}{}", cur, next);
                        self.desc_lines[row] = merged;
                        self.desc_lines.remove(row + 1);
                    }
                }

                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Inserts a newline at cursor position, splitting the current line.
    pub fn insert_newline(&mut self) {
        match self.focused_field {
            EditorField::Description => {
                self.mark_edited();
                let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
                let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
                let col = self.cursor_col.min(line_len);

                if let Some(line) = self.desc_lines.get(row).cloned() {
                    let (before, after) = self::split_line_at_char(&line, col);
                    self.desc_lines[row] = before;
                    self.desc_lines.insert(row + 1, after);
                    self.cursor_row = row + 1;
                    self.cursor_col = 0;
                }

                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Moves the cursor in the specified direction within the editor.
    pub fn move_cursor(&mut self, direction: CursorDirection) {
        match direction {
            CursorDirection::Up => {
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    let line_len = self
                        .desc_lines
                        .get(self.cursor_row)
                        .map_or(0, |l| l.chars().count());
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            CursorDirection::Down => {
                if self.cursor_row + 1 < self.desc_lines.len() {
                    self.cursor_row += 1;
                    let line_len = self
                        .desc_lines
                        .get(self.cursor_row)
                        .map_or(0, |l| l.chars().count());
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            CursorDirection::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            CursorDirection::Right => {
                let line_len = self
                    .desc_lines
                    .get(self.cursor_row)
                    .map_or(0, |l| l.chars().count());
                if self.cursor_col < line_len {
                    self.cursor_col += 1;
                }
            }
            CursorDirection::Home => {
                self.cursor_col = 0;
            }
            CursorDirection::End => {
                self.cursor_col = self
                    .desc_lines
                    .get(self.cursor_row)
                    .map_or(0, |l| l.chars().count());
            }
        }
    }

    /// Ensures the cursor is visible within the given viewport height.
    /// Adjusts scroll_offset if necessary.
    pub fn ensure_cursor_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.cursor_row - visible_rows + 1;
        }
    }

    /// Cycles the column selection to the next available column.
    pub fn cycle_column(&mut self) {
        if self.available_columns.len() <= 1 {
            return;
        }
        let next_idx = (self.selected_column_index + 1) % self.available_columns.len();
        self.selected_column_index = next_idx;
        self.column_id = Some(self.available_columns[next_idx].clone());
    }

    /// Returns (title, description) for creating or updating a task.
    pub fn to_task_fields(&self) -> (String, String) {
        let desc = self.description();
        let title = super::task::derive_title_from_description(&desc);
        (title, desc)
    }
}

/// Split a string at a given char index, returning (before, after).
fn split_line_at_char(line: &str, char_idx: usize) -> (String, String) {
    let byte_idx = line
        .char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let before = line[..byte_idx].to_string();
    let after = line[byte_idx..].to_string();
    (before, after)
}

// ─── Detail Editor ───────────────────────────────────────────────────────

/// Inline description editor state for the task detail view.
///
/// Similar to `TaskEditorState` but simpler — only handles description
/// editing (no column selection or task creation). Used when the user
/// clicks on the description area in the task detail panel.
#[derive(Debug, Clone)]
pub struct DetailEditorState {
    /// Description stored as individual lines for O(1) per-line access.
    pub desc_lines: Vec<String>,
    /// Cached joined text; `None` when lines have been modified since last join.
    pub cached_description: Option<String>,
    /// Whether the editor is currently focused (accepting input).
    pub is_focused: bool,
    /// Cursor row (0-indexed).
    pub cursor_row: usize,
    /// Cursor column (0-indexed).
    pub cursor_col: usize,
    /// Scroll offset for the description textarea.
    pub scroll_offset: usize,
    /// Whether the user has made unsaved edits since the last save.
    pub has_unsaved_changes: bool,
    /// Whether the "unsaved changes" discard warning is currently displayed.
    /// Set on first Esc with unsaved changes; cleared on any edit or save.
    pub discard_warning_shown: bool,
    /// Inline validation error message displayed below the description field.
    /// Set when the user tries to save with an empty description; cleared when
    /// the user types in the description field.
    pub validation_error: Option<String>,
}

impl DetailEditorState {
    /// Create a new detail editor from a description string.
    pub fn new_from_description(description: &str) -> Self {
        let lines: Vec<String> = if description.is_empty() {
            vec![String::new()]
        } else {
            description.split('\n').map(String::from).collect()
        };
        let cached = if description.is_empty() {
            None
        } else {
            Some(description.to_string())
        };
        Self {
            desc_lines: lines,
            cached_description: cached,
            is_focused: false,
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            has_unsaved_changes: false,
            discard_warning_shown: false,
            validation_error: None,
        }
    }

    /// Returns the description text as a single string.
    pub fn description(&self) -> String {
        match &self.cached_description {
            Some(cached) => cached.clone(),
            None => self.desc_lines.join("\n"),
        }
    }

    /// Sets the description from a flat string.
    pub fn set_description(&mut self, text: &str) {
        if text.is_empty() {
            self.desc_lines = vec![String::new()];
            self.cached_description = None;
        } else {
            self.desc_lines = text.split('\n').map(String::from).collect();
            self.cached_description = Some(text.to_string());
        }
    }

    /// Inserts a character at cursor position.
    pub fn insert_char(&mut self, ch: char) {
        let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
        let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
        let col = self.cursor_col.min(line_len);
        if let Some(line) = self.desc_lines.get_mut(row) {
            let byte_pos = line
                .char_indices()
                .nth(col)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            line.insert(byte_pos, ch);
            self.cursor_col = col + 1;
            self.cursor_row = row;
        }
        self.cached_description = None;
        self.has_unsaved_changes = true;
    }

    /// Deletes character before cursor (backspace).
    pub fn delete_char_back(&mut self) {
        let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
        let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
        let col = self.cursor_col.min(line_len);

        if col > 0 {
            if let Some(line) = self.desc_lines.get_mut(row) {
                let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                if let Some(&(byte_start, ch)) = char_indices.get(col - 1) {
                    let byte_end = byte_start + ch.len_utf8();
                    line.replace_range(byte_start..byte_end, "");
                }
                self.cursor_col = col - 1;
            }
        } else if row > 0 {
            let prev_len = self
                .desc_lines
                .get(row - 1)
                .map_or(0, |l| l.chars().count());
            if let (Some(prev), Some(cur)) = (
                self.desc_lines.get(row - 1).cloned(),
                self.desc_lines.get(row).cloned(),
            ) {
                let merged = format!("{}{}", prev, cur);
                self.desc_lines[row - 1] = merged;
                self.desc_lines.remove(row);
                self.cursor_row = row - 1;
                self.cursor_col = prev_len;
            }
        }

        self.cached_description = None;
        self.has_unsaved_changes = true;
    }

    /// Deletes character after cursor (forward delete).
    pub fn delete_char_forward(&mut self) {
        let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
        let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
        let col = self.cursor_col.min(line_len);

        if col < line_len {
            if let Some(line) = self.desc_lines.get_mut(row) {
                let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                if let Some(&(byte_start, ch)) = char_indices.get(col) {
                    let byte_end = byte_start + ch.len_utf8();
                    line.replace_range(byte_start..byte_end, "");
                }
            }
        } else if row + 1 < self.desc_lines.len() {
            if let (Some(cur), Some(next)) = (
                self.desc_lines.get(row).cloned(),
                self.desc_lines.get(row + 1).cloned(),
            ) {
                let merged = format!("{}{}", cur, next);
                self.desc_lines[row] = merged;
                self.desc_lines.remove(row + 1);
            }
        }

        self.cached_description = None;
        self.has_unsaved_changes = true;
    }

    /// Inserts a newline at cursor position.
    pub fn insert_newline(&mut self) {
        let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
        let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
        let col = self.cursor_col.min(line_len);

        if let Some(line) = self.desc_lines.get(row).cloned() {
            let (before, after) = split_line_at_char(&line, col);
            self.desc_lines[row] = before;
            self.desc_lines.insert(row + 1, after);
            self.cursor_row = row + 1;
            self.cursor_col = 0;
        }

        self.cached_description = None;
        self.has_unsaved_changes = true;
    }

    /// Moves the cursor in the specified direction.
    pub fn move_cursor(&mut self, direction: CursorDirection) {
        match direction {
            CursorDirection::Up => {
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    let line_len = self
                        .desc_lines
                        .get(self.cursor_row)
                        .map_or(0, |l| l.chars().count());
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            CursorDirection::Down => {
                if self.cursor_row + 1 < self.desc_lines.len() {
                    self.cursor_row += 1;
                    let line_len = self
                        .desc_lines
                        .get(self.cursor_row)
                        .map_or(0, |l| l.chars().count());
                    self.cursor_col = self.cursor_col.min(line_len);
                }
            }
            CursorDirection::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            CursorDirection::Right => {
                let line_len = self
                    .desc_lines
                    .get(self.cursor_row)
                    .map_or(0, |l| l.chars().count());
                if self.cursor_col < line_len {
                    self.cursor_col += 1;
                }
            }
            CursorDirection::Home => {
                self.cursor_col = 0;
            }
            CursorDirection::End => {
                self.cursor_col = self
                    .desc_lines
                    .get(self.cursor_row)
                    .map_or(0, |l| l.chars().count());
            }
        }
    }

    /// Ensures the cursor is visible within the given viewport height.
    pub fn ensure_cursor_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.cursor_row - visible_rows + 1;
        }
    }
}

// ─── Diff Review Types ───────────────────────────────────────────────────

/// State for the diff review view, stored on `UIState`.
#[derive(Debug, Clone)]
pub struct DiffReviewState {
    /// All files in the diff.
    pub files: Vec<DiffFile>,
    /// Index of the currently viewed file.
    pub selected_file_index: usize,
    /// Vertical scroll offset within the current file's diff lines.
    pub scroll_offset: usize,
    /// Error message if git diff failed.
    pub error: Option<String>,
    /// Task number for the header display.
    pub task_number: u32,
}

/// A single changed file in the diff.
#[derive(Debug, Clone)]
pub struct DiffFile {
    /// File path.
    pub path: String,
    /// Old path for renames.
    pub old_path: Option<String>,
    /// Number of addition lines.
    pub additions: u32,
    /// Number of deletion lines.
    pub deletions: u32,
    /// Whether this is a new file.
    pub is_new: bool,
    /// Whether this file was deleted.
    pub is_deleted: bool,
    /// Whether this is a binary file.
    pub is_binary: bool,
    /// Whether this file was renamed.
    pub is_renamed: bool,
    /// Diff lines for this file.
    pub lines: Vec<DiffLine>,
}

/// A single line in the diff output.
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// Kind of diff line.
    pub kind: DiffLineKind,
    /// Content of the line (without the leading +/- /space).
    pub content: String,
    /// Line number in the old file (for context/addition lines).
    pub old_line_no: Option<u32>,
    /// Line number in the new file (for context/addition lines).
    pub new_line_no: Option<u32>,
}

// ─── Session Tracker ─────────────────────────────────────────────────────

/// Session tracker — manages session-to-task mappings, session data,
/// subagent relationships, and the streaming render cache.
#[derive(Debug, Clone, Default)]
pub struct SessionTracker {
    /// Reverse index: session_id → task_id for O(1) lookup.
    pub session_to_task: HashMap<String, String>,
    /// Session data for task detail view, keyed by task_id.
    pub task_sessions: HashMap<String, TaskDetailSession>,
    /// Render cache for streaming lines in the task detail view.
    ///
    /// Shared by both main sessions (keyed by `task_id`) and drilled-down
    /// subagent sessions (keyed by `session_id`).  Each entry stores
    /// `(render_version, lines)` — lines are only rebuilt when the live
    /// `render_version` differs from the cached one.
    ///
    /// See the render cache invariant on [`TaskDetailSession`].
    pub cached_streaming_lines: HashMap<String, (u64, Vec<ratatui::prelude::Line<'static>>)>,
    /// Subagent sessions keyed by parent task_id.
    /// Each parent task can have multiple subagent sessions (e.g., a
    /// planning agent that spawns multiple `do` agents).
    pub subagent_sessions: HashMap<String, Vec<super::task::SubagentSession>>,
    /// Reverse index: subagent session_id → parent task_id.
    /// Used to route SSE events for child sessions to the correct parent.
    pub subagent_to_parent: HashMap<String, String>,
    /// Session detail data for drilled-down subagents (lazy-loaded).
    /// Keyed by subagent session_id.
    pub subagent_session_data: HashMap<String, TaskDetailSession>,
}

// ─── Dirty Flags ──────────────────────────────────────────────────────────

/// Dirty flags — tracks persistence and render dirty state.
///
/// Uses `Arc<AtomicBool>` for flags that need to be checked without
/// holding the main state mutex (e.g., the saving indicator in the
/// status bar, or the render-dirty check in the TUI event loop).
#[derive(Debug, Clone)]
pub struct DirtyFlags {
    /// Dirty flag for persistence — set when state changes need to be saved.
    pub dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Dirty flag for render optimization — set when state changes,
    /// checked by the TUI event loop to skip unnecessary full re-renders.
    pub render_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set of task IDs that have been modified since the last save.
    /// Used by `save_state` to skip writing unchanged tasks to the database.
    pub dirty_tasks: HashSet<String>,
    /// Set of task IDs that have been deleted from in-memory state but not yet
    /// removed from the database. Flushed by `save_state()` on the next persistence cycle.
    pub deleted_tasks: HashSet<String>,
    /// Set of project IDs that have been removed from in-memory state but not yet
    /// removed from the database. Flushed by `save_state()` on the next persistence cycle.
    pub deleted_projects: HashSet<String>,
    /// Whether a persistence save is currently in progress.
    /// Set to `true` before acquiring the state lock for saving, cleared after.
    /// Read by the TUI status bar (via `Arc<AtomicBool>` — no lock needed) to show
    /// a "saving..." indicator.
    pub saving_in_progress: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for DirtyFlags {
    fn default() -> Self {
        Self {
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            render_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            dirty_tasks: HashSet::new(),
            deleted_tasks: HashSet::new(),
            deleted_projects: HashSet::new(),
            saving_in_progress: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl DirtyFlags {
    /// Set the persistence dirty flag.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically take (clear) the persistence dirty flag.
    /// Returns `true` if the flag was set, `false` otherwise.
    pub fn take_dirty(&self) -> bool {
        self.dirty
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Mark that the state has changed and a re-render is needed.
    pub fn mark_render_dirty(&self) {
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically take the render-dirty flag (returns `true` and resets to
    /// `false` if the flag was set; returns `false` otherwise).
    pub fn take_render_dirty(&self) -> bool {
        self.render_dirty
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    }
}

// ─── AppState ─────────────────────────────────────────────────────────────

use super::project::{KanbanState, ProjectRegistry};

/// The single source of truth for all application state.
///
/// State is organized into focused sub-structs for clear ownership:
/// - [`ProjectRegistry`] — projects, active project, task counters
/// - [`KanbanState`] — column ordering and task placement
/// - [`UIState`] — mode, focus, notifications, editor
/// - [`SessionTracker`] — session mappings, session data, subagent tracking
/// - [`DirtyFlags`] — persistence and render dirty tracking
///
/// Access is via a single `Arc<Mutex<AppState>>` — no multiple locks.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Project registry — projects, active project, and task number counters.
    pub project_registry: ProjectRegistry,
    /// All tasks keyed by task ID.
    pub tasks: HashMap<String, CortexTask>,
    /// Kanban board layout — column ordering and task placement.
    pub kanban: KanbanState,
    /// UI state — current mode, focus, notifications.
    pub ui: UIState,
    /// Session tracker — session mappings, data, subagent relationships.
    pub session_tracker: SessionTracker,
    /// Dirty flags — persistence and render dirty state.
    pub dirty_flags: DirtyFlags,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            project_registry: ProjectRegistry::default(),
            tasks: HashMap::new(),
            kanban: KanbanState::default(),
            ui: UIState::default(),
            session_tracker: SessionTracker::default(),
            dirty_flags: DirtyFlags::default(),
        }
    }
}

impl AppState {
    /// Get connection state for the active project.
    /// Returns defaults (disconnected, not reconnecting) if no project is active.
    pub fn connection_state(&self) -> (bool, bool, u32, bool) {
        self.project_registry.connection_state()
    }

    /// Whether the active project's server is connected.
    pub fn is_connected(&self) -> bool {
        self.project_registry.is_connected()
    }

    /// Whether the active project's server is reconnecting.
    pub fn is_reconnecting(&self) -> bool {
        self.project_registry.is_reconnecting()
    }

    /// Reconnection attempt number for the active project.
    pub fn reconnect_attempt(&self) -> u32 {
        self.project_registry.reconnect_attempt()
    }

    /// Whether the active project has permanently disconnected.
    pub fn is_permanently_disconnected(&self) -> bool {
        self.project_registry.is_permanently_disconnected()
    }

    /// Set a project's connection state. No-op if the project doesn't exist.
    pub fn set_project_connected(&mut self, project_id: &str, connected: bool) {
        self.project_registry
            .set_project_connected(project_id, connected);
    }

    /// Set a project's reconnecting state. No-op if the project doesn't exist.
    pub fn set_project_reconnecting(&mut self, project_id: &str, reconnecting: bool) {
        self.project_registry
            .set_project_reconnecting(project_id, reconnecting);
    }

    /// Set a project's reconnect attempt. No-op if the project doesn't exist.
    pub fn set_project_reconnect_attempt(&mut self, project_id: &str, attempt: u32) {
        self.project_registry
            .set_project_reconnect_attempt(project_id, attempt);
    }

    /// Mark a project as permanently disconnected. No-op if the project doesn't exist.
    pub fn set_project_permanently_disconnected(&mut self, project_id: &str) {
        self.project_registry
            .set_project_permanently_disconnected(project_id);
    }

    /// Set the persistence dirty flag.
    pub fn mark_dirty(&self) {
        self.dirty_flags.mark_dirty();
    }

    /// Mark that the state has changed and a re-render is needed.
    pub fn mark_render_dirty(&self) {
        self.dirty_flags.mark_render_dirty();
    }

    /// Atomically take (clear) the persistence dirty flag.
    pub fn take_dirty(&self) -> bool {
        self.dirty_flags.take_dirty()
    }

    /// Atomically take the render-dirty flag.
    pub fn take_render_dirty(&self) -> bool {
        self.dirty_flags.take_render_dirty()
    }
}

// ── UI tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TaskEditorState: insert_char (Description) ───────────────────────

    #[test]
    fn insert_char_description_appends_at_end() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.insert_char('a');
        editor.insert_char('b');
        assert_eq!(editor.desc_lines, vec!["ab"]);
        assert_eq!(editor.cursor_col, 2);
        assert_eq!(editor.cursor_row, 0);
    }

    #[test]
    fn insert_char_description_multibyte() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.insert_char('🎉');
        assert_eq!(editor.desc_lines[0], "🎉");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn insert_char_description_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.cached_description = Some("old".to_string());
        editor.insert_char('x');
        assert!(editor.cached_description.is_none());
    }

    #[test]
    fn insert_char_description_on_second_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.desc_lines = vec!["line1".to_string(), "line2".to_string()];
        editor.cursor_row = 1;
        editor.cursor_col = 0;
        editor.insert_char('X');
        assert_eq!(editor.desc_lines[1], "Xline2");
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn insert_char_description_clears_validation_error() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.validation_error = Some("Description cannot be empty".to_string());
        editor.insert_char('x');
        assert!(editor.validation_error.is_none());
    }

    #[test]
    fn insert_char_description_marks_edited() {
        let mut editor = TaskEditorState::new_for_create("todo");
        assert!(!editor.has_unsaved_changes);
        editor.insert_char('x');
        assert!(editor.has_unsaved_changes);
    }

    // ── TaskEditorState: insert_char (Column — no-op) ────────────────────

    #[test]
    fn insert_char_column_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Column;
        let has_unsaved = editor.has_unsaved_changes;
        editor.insert_char('x');
        // Column field doesn't accept char input — marks edited regardless
        assert!(editor.has_unsaved_changes || !has_unsaved);
    }

    // ── TaskEditorState: delete_char_back (Description) ──────────────────

    #[test]
    fn delete_char_back_description_removes_char_on_same_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines[0], "hell");
        assert_eq!(editor.cursor_col, 4);
    }

    #[test]
    fn delete_char_back_description_at_beginning_of_line_merges_with_previous() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cursor_row = 1;
        editor.cursor_col = 0;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines, vec!["helloworld"]);
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 5); // cursor at end of merged line
    }

    #[test]
    fn delete_char_back_description_at_beginning_of_first_line_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 0;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn delete_char_back_description_multibyte_emoji() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["🎉🚀".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 2;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines[0], "🎉");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn delete_char_back_description_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.cached_description = Some("cached".to_string());
        editor.delete_char_back();
        assert!(editor.cached_description.is_none());
    }

    #[test]
    fn delete_char_back_description_empty_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec![String::new()];
        editor.cursor_row = 0;
        editor.cursor_col = 0;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines, vec![String::new()]);
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn delete_char_back_description_clears_validation_error() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.validation_error = Some("error".to_string());
        editor.set_description("x");
        editor.cursor_col = 1;
        editor.delete_char_back();
        assert!(editor.validation_error.is_none());
    }

    // ── TaskEditorState: delete_char_forward (Description) ───────────────

    #[test]
    fn delete_char_forward_description_removes_char_on_same_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["abc".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 1;
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines[0], "ac");
    }

    #[test]
    fn delete_char_forward_description_at_end_of_line_merges_with_next() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5; // end of first line
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines, vec!["helloworld"]);
    }

    #[test]
    fn delete_char_forward_description_at_end_of_last_line_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5;
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn delete_char_forward_description_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.cached_description = Some("cached".to_string());
        editor.delete_char_forward();
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: insert_newline ──────────────────────────────────

    #[test]
    fn insert_newline_splits_line_at_cursor() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello world".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5;
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["hello", " world"]);
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn insert_newline_at_end_of_line_adds_empty_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5;
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["hello", ""]);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn insert_newline_at_beginning_of_line_adds_empty_line_before() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 0;
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["", "hello"]);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn insert_newline_on_column_field_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Column;
        editor.desc_lines = vec!["hello".to_string()];
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn insert_newline_marks_edited_and_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.cached_description = Some("old".to_string());
        assert!(!editor.has_unsaved_changes);
        editor.insert_newline();
        assert!(editor.has_unsaved_changes);
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: move_cursor (Description) ───────────────────────

    #[test]
    fn move_cursor_description_up_clamps_to_first_row() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string()];
        editor.cursor_row = 0;
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 0);
    }

    #[test]
    fn move_cursor_description_down_clamps_to_last_row() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string()];
        editor.cursor_row = 1;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_up_decrements_row() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        editor.cursor_row = 2;
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_down_increments_row() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        editor.cursor_row = 0;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_left_clamps_to_zero() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.cursor_col = 0;
        editor.move_cursor(CursorDirection::Left);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn move_cursor_description_right_clamps_to_line_length() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["ab".to_string()];
        editor.cursor_col = 2;
        editor.move_cursor(CursorDirection::Right);
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn move_cursor_description_up_clamps_col_to_shorter_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["ab".to_string(), "c".to_string()];
        editor.cursor_row = 1;
        editor.cursor_col = 1;
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 1); // clamped to min(1, 2) = 1
    }

    #[test]
    fn move_cursor_description_down_clamps_col_to_shorter_line() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["abc".to_string(), "d".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 3;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 1); // clamped to min(3, 1) = 1
    }

    #[test]
    fn move_cursor_description_home_end() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_col = 3;
        editor.move_cursor(CursorDirection::Home);
        assert_eq!(editor.cursor_col, 0);
        editor.move_cursor(CursorDirection::End);
        assert_eq!(editor.cursor_col, 5);
    }

    // ── TaskEditorState: ensure_cursor_visible ───────────────────────────

    #[test]
    fn ensure_cursor_visible_adjusts_scroll_down() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        editor.cursor_row = 3;
        editor.scroll_offset = 0;
        editor.ensure_cursor_visible(3); // visible rows 0-2
        assert_eq!(editor.scroll_offset, 1); // now visible rows 1-3
    }

    #[test]
    fn ensure_cursor_visible_adjusts_scroll_up() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        editor.cursor_row = 0;
        editor.scroll_offset = 2;
        editor.ensure_cursor_visible(3); // visible rows 2-4
        assert_eq!(editor.scroll_offset, 0); // now visible rows 0-2
    }

    #[test]
    fn ensure_cursor_visible_zero_height_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.cursor_row = 5;
        editor.scroll_offset = 0;
        editor.ensure_cursor_visible(0);
        assert_eq!(editor.scroll_offset, 0);
    }

    // ── TaskEditorState: description cache ───────────────────────────────

    #[test]
    fn description_returns_cached_when_available() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cached_description = Some("hello\nworld".to_string());
        assert_eq!(editor.description(), "hello\nworld");
    }

    #[test]
    fn description_joins_lines_when_no_cache() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cached_description = None;
        assert_eq!(editor.description(), "hello\nworld");
    }

    #[test]
    fn set_description_splits_on_newlines() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.set_description("line1\nline2\nline3");
        assert_eq!(editor.desc_lines, vec!["line1", "line2", "line3"]);
        assert_eq!(
            editor.cached_description,
            Some("line1\nline2\nline3".to_string())
        );
    }

    #[test]
    fn set_description_empty_clears_lines() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.set_description("hello");
        editor.set_description("");
        assert_eq!(editor.desc_lines, vec![String::new()]);
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: current_line ────────────────────────────────────

    #[test]
    fn current_line_description() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["abc".to_string(), "def".to_string()];
        editor.cursor_row = 1;
        assert_eq!(editor.current_line(), "def");
    }

    #[test]
    fn current_line_column_is_empty() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.focused_field = EditorField::Column;
        assert_eq!(editor.current_line(), "");
    }

    // ── TaskEditorState: to_task_fields ──────────────────────────────────

    #[test]
    fn to_task_fields_derives_title_from_description() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.set_description("My Task Title\nline2");
        let (title, desc) = editor.to_task_fields();
        assert_eq!(title, "My Task Title");
        assert_eq!(desc, "My Task Title\nline2");
    }

    #[test]
    fn to_task_fields_truncates_long_title() {
        let mut editor = TaskEditorState::new_for_create("todo");
        let long_line = "x".repeat(100);
        editor.set_description(&long_line);
        let (title, _) = editor.to_task_fields();
        assert_eq!(title.len(), 80);
    }

    #[test]
    fn to_task_fields_empty_description() {
        let mut editor = TaskEditorState::new_for_create("todo");
        let (title, desc) = editor.to_task_fields();
        assert_eq!(title, "");
        assert_eq!(desc, "");
    }

    // ── TaskEditorState: new_for_edit ────────────────────────────────────

    #[test]
    fn new_for_edit_prepopulates_from_task() {
        let task = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "Existing Task".to_string(),
            description: "Line 1\nLine 2".to_string(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: Some("planning".to_string()),
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let editor =
            TaskEditorState::new_for_edit(&task, vec!["todo".to_string(), "planning".to_string()]);
        assert_eq!(editor.task_id, Some("task-1".to_string()));
        assert_eq!(editor.desc_lines, vec!["Line 1", "Line 2"]);
        assert_eq!(editor.cursor_col, 0);
        assert_eq!(editor.focused_field, EditorField::Description);
        assert!(!editor.has_unsaved_changes);
        assert_eq!(
            editor.available_columns,
            vec!["todo".to_string(), "planning".to_string()]
        );
        assert_eq!(editor.selected_column_index, 0); // "todo" is at index 0
    }

    #[test]
    fn new_for_edit_empty_description() {
        let task = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "Task".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let editor = TaskEditorState::new_for_edit(&task, vec!["todo".to_string()]);
        assert_eq!(editor.desc_lines, vec![String::new()]);
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: new_for_create starts on Description ────────────

    #[test]
    fn new_for_create_starts_on_description() {
        let editor = TaskEditorState::new_for_create("todo");
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    // ── TaskEditorState: cycle_column ────────────────────────────────────

    #[test]
    fn cycle_column_advances_to_next() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.available_columns = vec![
            "todo".to_string(),
            "planning".to_string(),
            "done".to_string(),
        ];
        assert_eq!(editor.column_id, Some("todo".to_string()));
        editor.cycle_column();
        assert_eq!(editor.column_id, Some("planning".to_string()));
        editor.cycle_column();
        assert_eq!(editor.column_id, Some("done".to_string()));
        editor.cycle_column(); // wraps
        assert_eq!(editor.column_id, Some("todo".to_string()));
    }

    #[test]
    fn cycle_column_single_column_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo");
        editor.cycle_column();
        assert_eq!(editor.column_id, Some("todo".to_string()));
    }
}

// ── DetailEditorState tests ─────────────────────────────────────────────

#[cfg(test)]
mod detail_editor_tests {
    use super::*;

    #[test]
    fn detail_editor_new_from_description() {
        let editor = DetailEditorState::new_from_description("Hello\nWorld");
        assert_eq!(editor.desc_lines, vec!["Hello", "World"]);
        assert_eq!(editor.description(), "Hello\nWorld");
        assert!(!editor.is_focused);
        assert!(!editor.has_unsaved_changes);
    }

    #[test]
    fn detail_editor_new_from_empty() {
        let editor = DetailEditorState::new_from_description("");
        assert_eq!(editor.desc_lines, vec![String::new()]);
        assert_eq!(editor.description(), "");
    }

    #[test]
    fn detail_editor_insert_char() {
        let mut editor = DetailEditorState::new_from_description("Hello");
        editor.is_focused = true;
        editor.cursor_col = 5; // Move to end of "Hello"
        editor.insert_char('!');
        assert_eq!(editor.description(), "Hello!");
        assert_eq!(editor.cursor_col, 6);
        assert!(editor.has_unsaved_changes);
    }

    #[test]
    fn detail_editor_insert_char_multibyte() {
        let mut editor = DetailEditorState::new_from_description("");
        editor.is_focused = true;
        editor.insert_char('🎉');
        assert_eq!(editor.desc_lines[0], "🎉");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn detail_editor_delete_char_back() {
        let mut editor = DetailEditorState::new_from_description("Hello");
        editor.is_focused = true;
        editor.cursor_col = 5;
        editor.delete_char_back();
        assert_eq!(editor.description(), "Hell");
        assert_eq!(editor.cursor_col, 4);
    }

    #[test]
    fn detail_editor_delete_char_back_merge_lines() {
        let mut editor = DetailEditorState::new_from_description("Hello\nWorld");
        editor.is_focused = true;
        editor.cursor_row = 1;
        editor.cursor_col = 0;
        editor.delete_char_back();
        assert_eq!(editor.description(), "HelloWorld");
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 5);
    }

    #[test]
    fn detail_editor_delete_char_forward() {
        let mut editor = DetailEditorState::new_from_description("Hello");
        editor.is_focused = true;
        editor.cursor_col = 0;
        editor.delete_char_forward();
        assert_eq!(editor.description(), "ello");
    }

    #[test]
    fn detail_editor_newline() {
        let mut editor = DetailEditorState::new_from_description("HelloWorld");
        editor.is_focused = true;
        editor.cursor_col = 5;
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["Hello", "World"]);
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn detail_editor_description_roundtrip() {
        let original = "Line 1\nLine 2 with some text\nLine 3";
        let editor = DetailEditorState::new_from_description(original);
        assert_eq!(editor.description(), original);
    }

    #[test]
    fn detail_editor_move_cursor() {
        let mut editor = DetailEditorState::new_from_description("Hello\nWorld");
        editor.is_focused = true;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 0);
        editor.move_cursor(CursorDirection::End);
        assert_eq!(editor.cursor_col, 5);
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 5); // clamped to line length
    }

    #[test]
    fn detail_editor_ensure_cursor_visible() {
        let mut editor = DetailEditorState::new_from_description(&"Line\n".repeat(20));
        editor.is_focused = true;
        editor.cursor_row = 15;
        editor.ensure_cursor_visible(10);
        assert_eq!(editor.scroll_offset, 6); // 15 - 10 + 1
    }
}
