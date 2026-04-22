//! Core domain types for the Cortex application.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

// ─── Enums ────────────────────────────────────────────────────────────────

/// Kanban column identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KanbanColumn(pub String);

impl KanbanColumn {
    /// Built-in column identifier for "to-do" tasks.
    pub const TODO: &'static str = "todo";
}

impl std::fmt::Display for KanbanColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for KanbanColumn {
    fn from(s: &str) -> Self {
        KanbanColumn(s.to_string())
    }
}

/// Task agent type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskAgentType {
    None,
    Planning,
    Do,
    ReviewerAlpha,
    ReviewerBeta,
    ReviewerGamma,
}

impl TaskAgentType {
    /// Parse an agent type from a string slice. Unrecognized values return [`TaskAgentType::None`].
    pub fn from_str_opt(s: &str) -> Self {
        match s {
            "planning" => TaskAgentType::Planning,
            "do" => TaskAgentType::Do,
            "reviewer-alpha" => TaskAgentType::ReviewerAlpha,
            "reviewer-beta" => TaskAgentType::ReviewerBeta,
            "reviewer-gamma" => TaskAgentType::ReviewerGamma,
            _ => TaskAgentType::None,
        }
    }

    /// Return the string representation of this agent type.
    pub fn as_str(&self) -> &str {
        match self {
            TaskAgentType::None => "none",
            TaskAgentType::Planning => "planning",
            TaskAgentType::Do => "do",
            TaskAgentType::ReviewerAlpha => "reviewer-alpha",
            TaskAgentType::ReviewerBeta => "reviewer-beta",
            TaskAgentType::ReviewerGamma => "reviewer-gamma",
        }
    }
}

/// Agent execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Pending,
    Running,
    Hung,
    Complete,
    Error,
}

impl AgentStatus {
    /// Returns a Unicode icon representing the agent status.
    pub fn icon(&self) -> &'static str {
        match self {
            AgentStatus::Pending => "·",
            AgentStatus::Running => "◐",
            AgentStatus::Hung => "⏸",
            AgentStatus::Complete => "✓",
            AgentStatus::Error => "✗",
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Pending => write!(f, "pending"),
            AgentStatus::Running => write!(f, "working"),
            AgentStatus::Hung => write!(f, "hung"),
            AgentStatus::Complete => write!(f, "done"),
            AgentStatus::Error => write!(f, "failed"),
        }
    }
}

/// Project status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectStatus {
    Disconnected,
    Idle,
    Working,
    Question,
    Done,
    Error,
    Hung,
}

impl ProjectStatus {
    /// Returns a Unicode icon representing the project status.
    pub fn icon(&self) -> &'static str {
        match self {
            ProjectStatus::Disconnected => "○",
            ProjectStatus::Idle => "●",
            ProjectStatus::Working => "◐",
            ProjectStatus::Question => "?",
            ProjectStatus::Done => "✓",
            ProjectStatus::Error => "✗",
            ProjectStatus::Hung => "⏸",
        }
    }
}

/// Tool execution state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolState {
    Pending,
    Running,
    Completed,
    Error,
}

/// A destructive action awaiting user confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmableAction {
    /// Delete the task with the given ID.
    DeleteTask(String),
    /// Delete the project with the given ID.
    DeleteProject(String),
}

/// Application mode — determines rendering and key routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    TaskEditor,
    Help,
    /// Inline text input prompt (e.g., set working directory).
    InputPrompt,
    /// Project rename prompt.
    ProjectRename,
    /// Confirmation dialog for destructive actions.
    ConfirmDialog,
}

/// Which field is focused in the task editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorField {
    Title,
    Description,
    Column,
}

/// Which panel is focused in normal mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPanel {
    Kanban,
    TaskDetail,
}

/// Message role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
}

/// Notification variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationVariant {
    Info,
    Success,
    Warning,
    Error,
}

/// Maximum number of notifications that can be queued simultaneously.
pub const MAX_NOTIFICATIONS: usize = 3;

/// Cursor direction for movement in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirection {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

// ─── Structs ──────────────────────────────────────────────────────────────

/// A task in the kanban board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexTask {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable sequential number within the project.
    pub number: u32,
    /// Task title.
    pub title: String,
    /// Task description (may contain newlines).
    pub description: String,
    /// Current kanban column.
    pub column: KanbanColumn,
    /// OpenCode session ID, if an agent is currently working on this task.
    pub session_id: Option<String>,
    /// Which agent type is assigned to this task.
    pub agent_type: TaskAgentType,
    /// Current execution status of the assigned agent.
    pub agent_status: AgentStatus,
    /// Unix timestamp (seconds) when the task entered its current column.
    pub entered_column_at: i64,
    /// Unix timestamp (seconds) of the last agent activity on this task.
    pub last_activity_at: i64,
    /// Error message from the last failed agent run, if any.
    pub error_message: Option<String>,
    /// Output from the planning phase, if available.
    pub plan_output: Option<String>,
    /// Number of pending permission requests awaiting user approval.
    pub pending_permission_count: u32,
    /// Number of pending questions awaiting user answers.
    pub pending_question_count: u32,
    /// Unix timestamp (seconds) when the task was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) when the task was last updated.
    pub updated_at: i64,
    /// ID of the project this task belongs to.
    pub project_id: String,
}

/// A project in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexProject {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Display name shown in the sidebar.
    pub name: String,
    /// Filesystem working directory for the project.
    pub working_directory: String,
    /// Aggregate status derived from task states.
    pub status: ProjectStatus,
    /// Display order position in the sidebar.
    pub position: usize,
}

/// Kanban board state — column ordering and task placement.
#[derive(Debug, Clone, Default)]
pub struct KanbanState {
    /// Maps column ID → ordered list of task IDs.
    pub columns: HashMap<String, Vec<String>>,
    /// Currently focused column index among visible columns.
    pub focused_column_index: usize,
    /// Per-column focused task index.
    pub focused_task_index: HashMap<String, usize>,
    /// Horizontal scroll offset — index of the first visible column.
    /// 0 means no scrolling (leftmost column is visible).
    pub kanban_scroll_offset: usize,
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
    /// Pending destructive action awaiting confirmation in `AppMode::ConfirmDialog`.
    pub confirm_action: Option<ConfirmableAction>,
    /// User-controlled scroll offset for the streaming output in task detail view.
    /// `None` means auto-scroll (always show the bottom). `Some(n)` means the
    /// user has manually scrolled and the view is pinned to offset `n`.
    pub user_scroll_offset: Option<usize>,
}

impl Default for UIState {
    fn default() -> Self {
        Self {
            mode: AppMode::Normal,
            focused_panel: FocusedPanel::Kanban,
            focused_column: KanbanColumn::TODO.to_string(),
            focused_task_id: None,
            viewing_task_id: None,
            notifications: VecDeque::new(),
            input_text: String::new(),
            input_cursor: 0,
            prompt_label: String::new(),
            prompt_context: None,
            task_editor: None,
            confirm_action: None,
            user_scroll_offset: None,
        }
    }
}

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

/// Fullscreen task editor state.
///
/// Description text is stored as a `Vec<String>` of individual lines for O(1)
/// per-line access during cursor movement and rendering. The joined text is
/// cached and only recomputed when lines change.
#[derive(Debug, Clone)]
pub struct TaskEditorState {
    /// `None` = creating new task, `Some(id)` = editing existing task.
    pub task_id: Option<String>,
    /// Title text buffer.
    pub title: String,
    /// Description stored as individual lines for O(1) per-line access.
    /// Always contains at least one element (empty string when description is empty).
    pub desc_lines: Vec<String>,
    /// Cached joined text; `None` when lines have been modified since last join.
    pub cached_description: Option<String>,
    /// Currently focused field (title or description).
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
    pub agent_type: TaskAgentType,
    /// Whether the user has made unsaved edits since the last save or open.
    pub has_unsaved_changes: bool,
    /// Whether the "unsaved changes" discard warning is currently displayed.
    /// Set on first Esc with unsaved changes; cleared on any edit or save.
    pub discard_warning_shown: bool,
    /// Inline validation error message displayed below the title field.
    /// Set when the user tries to save with an empty title; cleared when
    /// the user types in the title field.
    pub validation_error: Option<String>,
    pub available_columns: Vec<String>,
    pub selected_column_index: usize,
}

impl TaskEditorState {
    /// Creates empty state for a new task.
    pub fn new_for_create(default_column: &str, available_columns: Vec<String>) -> Self {
        let selected_column_index = available_columns.iter().position(|c| c == default_column).unwrap_or(0);
        Self {
            task_id: None,
            title: String::new(),
            desc_lines: vec![String::new()],
            cached_description: None,
            focused_field: EditorField::Title,
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            column_id: Some(default_column.to_string()),
            agent_type: TaskAgentType::None,
            has_unsaved_changes: false,
            discard_warning_shown: false,
            validation_error: None,
            available_columns,
            selected_column_index,
        }
    }

    /// Pre-populates from an existing task for editing.
    pub fn new_for_edit(task: &CortexTask) -> Self {
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
            title: task.title.clone(),
            desc_lines: lines,
            cached_description: cached,
            focused_field: EditorField::Title,
            cursor_row: 0,
            cursor_col: task.title.chars().count(),
            scroll_offset: 0,
            column_id: Some(task.column.0.clone()),
            agent_type: task.agent_type.clone(),
            has_unsaved_changes: false,
            discard_warning_shown: false,
            validation_error: None,
            available_columns: Vec::new(),
            selected_column_index: 0,
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
            EditorField::Title => &self.title,
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
        self.mark_edited();
        match self.focused_field {
            EditorField::Title => {
                // Clear inline validation error when user edits the title.
                self.validation_error = None;
                // Convert char index to byte offset for String::insert.
                let byte_pos = self
                    .title
                    .char_indices()
                    .nth(self.cursor_col)
                    .map(|(i, _)| i)
                    .unwrap_or(self.title.len());
                self.title.insert(byte_pos, ch);
                self.cursor_col += 1; // char-based, so +1 is always correct
            }
            EditorField::Description => {
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
                }
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Deletes character before cursor (backspace).
    pub fn delete_char_back(&mut self) {
        self.mark_edited();
        match self.focused_field {
            EditorField::Title => {
                // Clear inline validation error when user edits the title.
                self.validation_error = None;
                if self.cursor_col > 0 {
                    // Find byte range of the char at char index (cursor_col - 1).
                    let char_indices: Vec<(usize, char)> = self.title.char_indices().collect();
                    if let Some(&(byte_start, ch)) = char_indices.get(self.cursor_col - 1) {
                        let byte_end = byte_start + ch.len_utf8();
                        self.title.replace_range(byte_start..byte_end, "");
                    }
                    self.cursor_col -= 1;
                }
            }
            EditorField::Description => {
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
                    // Merge with previous line
                    let prev_len = self.desc_lines[row - 1].chars().count();
                    let current = self.desc_lines.remove(row);
                    self.desc_lines[row - 1].push_str(&current);
                    self.cursor_row = row - 1;
                    self.cursor_col = prev_len;
                }
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Deletes character at cursor (delete key).
    pub fn delete_char_forward(&mut self) {
        self.mark_edited();
        match self.focused_field {
            EditorField::Title => {
                // Clear inline validation error when user edits the title.
                self.validation_error = None;
                if self.cursor_col < self.title.chars().count() {
                    let char_indices: Vec<(usize, char)> = self.title.char_indices().collect();
                    if let Some(&(byte_start, ch)) = char_indices.get(self.cursor_col) {
                        let byte_end = byte_start + ch.len_utf8();
                        self.title.replace_range(byte_start..byte_end, "");
                    }
                }
            }
            EditorField::Description => {
                let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
                let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
                let col = self.cursor_col.min(line_len);

                if row < self.desc_lines.len() {
                    let line_char_count = self.desc_lines[row].chars().count();
                    if col < line_char_count {
                        let line = &mut self.desc_lines[row];
                        let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                        if let Some(&(byte_start, ch)) = char_indices.get(col) {
                            let byte_end = byte_start + ch.len_utf8();
                            line.replace_range(byte_start..byte_end, "");
                        }
                    } else if row + 1 < self.desc_lines.len() {
                        // Merge with next line
                        let next = self.desc_lines.remove(row + 1);
                        self.desc_lines[row].push_str(&next);
                    }
                }
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Inserts a newline at cursor (only in description field).
    pub fn insert_newline(&mut self) {
        if self.focused_field != EditorField::Description {
            return;
        }
        self.mark_edited();
        let row = self.cursor_row.min(self.desc_lines.len().saturating_sub(1));
        let line_len = self.desc_lines.get(row).map_or(0, |l| l.chars().count());
        let col = self.cursor_col.min(line_len);

        if row < self.desc_lines.len() {
            // Convert char index to byte offset for split_off.
            let byte_pos = self.desc_lines[row]
                .char_indices()
                .nth(col)
                .map(|(i, _)| i)
                .unwrap_or(self.desc_lines[row].len());
            let rest = self.desc_lines[row].split_off(byte_pos);
            self.desc_lines.insert(row + 1, rest);
        }
        self.cursor_row = row + 1;
        self.cursor_col = 0;
        self.invalidate_cache();
    }

    /// Moves cursor in the given direction, clamped to valid positions.
    pub fn move_cursor(&mut self, direction: CursorDirection) {
        match self.focused_field {
            EditorField::Title => match direction {
                CursorDirection::Left => {
                    self.cursor_col = self.cursor_col.saturating_sub(1);
                }
                CursorDirection::Right => {
                    self.cursor_col = (self.cursor_col + 1).min(self.title.chars().count());
                }
                CursorDirection::Home => {
                    self.cursor_col = 0;
                }
                CursorDirection::End => {
                    self.cursor_col = self.title.chars().count();
                }
                _ => {}
            },
            EditorField::Description => {
                let num_lines = self.desc_lines.len();
                let max_row = num_lines.saturating_sub(1);

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
                        if self.cursor_row < max_row {
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
                        self.cursor_col = (self.cursor_col + 1).min(line_len);
                    }
                    CursorDirection::Home => {
                        self.cursor_col = 0;
                    }
                    CursorDirection::End => {
                        let line_len = self
                            .desc_lines
                            .get(self.cursor_row)
                            .map_or(0, |l| l.chars().count());
                        self.cursor_col = line_len;
                    }
                }
            }
            EditorField::Column => {}
        }
    }

    /// Adjusts scroll_offset so cursor row is within the visible textarea area.
    pub fn ensure_cursor_visible(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + visible_height {
            self.scroll_offset = self.cursor_row - visible_height + 1;
        }
    }

    /// Returns (title, description) for saving.
    pub fn to_task_fields(&self) -> (String, String) {
        (self.title.clone(), self.description())
    }

    pub fn cycle_column(&mut self) {
        if self.available_columns.len() <= 1 { return; }
        self.selected_column_index = (self.selected_column_index + 1) % self.available_columns.len();
        self.column_id = self.available_columns.get(self.selected_column_index).cloned();
        self.has_unsaved_changes = true;
        self.discard_warning_shown = false;
    }
}

/// A message in a task's session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage {
    /// Unique message identifier.
    pub id: String,
    /// Whether this message is from the user or the assistant.
    pub role: MessageRole,
    /// Ordered parts within this message (text, tool calls, steps, etc.).
    pub parts: Vec<TaskMessagePart>,
    /// Creation timestamp string (formatted as `t{unix_seconds}`).
    pub created_at: Option<String>,
}

/// A part within a task message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskMessagePart {
    Text {
        text: String,
    },
    Tool {
        id: String,
        tool: String,
        state: ToolState,
        input: Option<String>,
        output: Option<String>,
        error: Option<String>,
        /// Pre-computed short summary extracted from the tool input JSON.
        /// Populated once when the message is received, so the render path
        /// can display it without re-parsing JSON on every frame.
        cached_summary: Option<String>,
    },
    StepStart {
        id: String,
    },
    StepFinish {
        id: String,
    },
    Agent {
        id: String,
        agent: String,
    },
    Reasoning {
        text: String,
    },
    Unknown,
}

/// A permission request from an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Unique permission request identifier.
    pub id: String,
    /// ID of the session that requested permission.
    pub session_id: String,
    /// Name of the tool being requested (e.g., "bash", "write").
    pub tool_name: String,
    /// Human-readable description of the tool invocation.
    pub description: String,
    /// Current status ("pending", "approved", "rejected").
    pub status: String,
    /// Optional additional details about the request.
    pub details: Option<String>,
}

/// A question request from an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRequest {
    /// Unique question identifier.
    pub id: String,
    /// ID of the session that asked the question.
    pub session_id: String,
    /// The question text.
    pub question: String,
    /// Possible answer choices.
    pub answers: Vec<String>,
    /// Current status ("pending", "answered").
    pub status: String,
}

/// Session data for a task (messages, streaming state).
#[derive(Debug, Clone, Default)]
pub struct TaskDetailSession {
    /// ID of the task this session belongs to.
    pub task_id: String,
    /// OpenCode session ID, if a session has been started.
    pub session_id: Option<String>,
    /// Complete message history for this session.
    pub messages: Vec<TaskMessage>,
    /// Partial text being streamed from the assistant (appended incrementally).
    pub streaming_text: Option<String>,
    /// Outstanding permission requests awaiting user approval.
    pub pending_permissions: Vec<PermissionRequest>,
    /// Outstanding questions awaiting user answers.
    pub pending_questions: Vec<QuestionRequest>,
    /// Monotonically increasing version counter. Incremented whenever
    /// messages or streaming text change. Used by the render path to skip
    /// rebuilding `Vec<Line>` when nothing has changed.
    pub render_version: u64,
}

// ─── Top-Level State ──────────────────────────────────────────────────────

/// The single source of truth for all application state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// All registered projects.
    pub projects: Vec<CortexProject>,
    /// All tasks keyed by task ID.
    pub tasks: HashMap<String, CortexTask>,
    /// Kanban board layout — column ordering and task placement.
    pub kanban: KanbanState,
    /// UI state — current mode, focus, notifications.
    pub ui: UIState,
    /// Whether at least one OpenCode client is connected.
    pub connected: bool,
    /// Whether an SSE reconnection is in progress (exponential backoff).
    pub reconnecting: bool,
    /// Current reconnect attempt number (0 when not reconnecting, 1-based during reconnect).
    pub reconnect_attempt: u32,
    /// ID of the currently active project.
    pub active_project_id: Option<String>,
    /// Per-project auto-incrementing task number counters.
    pub task_number_counters: HashMap<String, u32>,
    /// Reverse index: session_id → task_id for O(1) lookup.
    pub session_to_task: HashMap<String, String>,
    /// Session data for task detail view, keyed by task_id.
    pub task_sessions: HashMap<String, TaskDetailSession>,
    /// Cache for rendered streaming lines in the task detail view.
    /// Maps `task_id → (render_version, lines)`. Only rebuild lines when
    /// the session's `render_version` has changed.
    pub cached_streaming_lines: HashMap<String, (u64, Vec<ratatui::prelude::Line<'static>>)>,
    /// Dirty flag for persistence — set when state changes need to be saved.
    pub dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Dirty flag for render optimization — set when state changes,
    /// checked by the TUI event loop to skip unnecessary full re-renders.
    pub render_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            tasks: HashMap::new(),
            kanban: KanbanState::default(),
            ui: UIState::default(),
            connected: false,
            reconnecting: false,
            reconnect_attempt: 0,
            active_project_id: None,
            task_number_counters: HashMap::new(),
            session_to_task: HashMap::new(),
            task_sessions: HashMap::new(),
            cached_streaming_lines: HashMap::new(),
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            render_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

/// Try to extract a short summary from tool input JSON.
///
/// This is called once when a `TaskMessagePart::Tool` is created (in
/// `convert_sdk_part`), so the render path never has to re-parse JSON.
pub fn extract_tool_summary(tool_name: &str, input: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(input) {
        match tool_name {
            "read" | "Read" => val
                .get("file_path")
                .or_else(|| val.get("filePath"))
                .or_else(|| val.get("path"))
                .and_then(|v| v.as_str())
                .map(|s| s.rsplit('/').next().unwrap_or(s).to_string())
                .unwrap_or_else(|| "...".to_string()),
            "write" | "Write" => val
                .get("file_path")
                .or_else(|| val.get("filePath"))
                .or_else(|| val.get("path"))
                .and_then(|v| v.as_str())
                .map(|s| s.rsplit('/').next().unwrap_or(s).to_string())
                .unwrap_or_else(|| "...".to_string()),
            "grep" | "Grep" | "glob" | "Glob" => val
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("...")
                .to_string(),
            "bash" | "Bash" => val
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.chars().count() > 60 {
                        format!("{}...", s.chars().take(57).collect::<String>())
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "...".to_string()),
            _ => "...".to_string(),
        }
    } else {
        "...".to_string()
    }
}
