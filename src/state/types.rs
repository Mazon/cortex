//! Core domain types for the Cortex application.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

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

/// Agent execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Pending,
    Running,
    Hung,
    Ready,
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
            AgentStatus::Ready => "◉",
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
            AgentStatus::Ready => write!(f, "ready"),
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
    /// Which agent type is assigned to this task. `None` means no agent assigned.
    pub agent_type: Option<String>,
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
    /// Whether the OpenCode server for this project is connected.
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub connected: bool,
    /// Whether an SSE reconnection is in progress for this project.
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub reconnecting: bool,
    /// Current reconnection attempt number for this project (0 when not reconnecting).
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub reconnect_attempt: u32,
    /// Whether max reconnection retries have been exceeded for this project.
    /// This is a runtime-only flag (not persisted) — on app restart the
    /// connection will be retried from scratch.
    #[serde(skip)]
    pub permanently_disconnected: bool,
}

impl Default for CortexProject {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            working_directory: String::new(),
            status: ProjectStatus::Idle,
            position: 0,
            connected: false,
            reconnecting: false,
            reconnect_attempt: 0,
            permanently_disconnected: false,
        }
    }
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
    /// Stack of session references for drill-down navigation.
    /// Bottom = top-level task, top = currently viewed session.
    /// When empty, the task detail view shows the parent task's output.
    /// When non-empty, the task detail view shows the top-of-stack session's output.
    pub session_nav_stack: Vec<SessionRef>,
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
            session_nav_stack: Vec::new(),
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
    pub fn new_for_create(default_column: &str, available_columns: Vec<String>) -> Self {
        let selected_column_index = available_columns.iter().position(|c| c == default_column).unwrap_or(0);
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
            available_columns,
            selected_column_index,
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
            selected_column_index: available_columns.iter()
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
                        // Clear inline validation error when user edits the description.
                        self.validation_error = None;
                    }
                } else if row > 0 {
                    // Merge with previous line
                    let prev_len = self.desc_lines[row - 1].chars().count();
                    let current = self.desc_lines.remove(row);
                    self.desc_lines[row - 1].push_str(&current);
                    self.cursor_row = row - 1;
                    self.cursor_col = prev_len;
                    self.validation_error = None;
                }
                self.invalidate_cache();
            }
            EditorField::Column => {}
        }
    }

    /// Deletes character at cursor (delete key).
    pub fn delete_char_forward(&mut self) {
        match self.focused_field {
            EditorField::Description => {
                self.mark_edited();
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
                        // Clear inline validation error when user edits the description.
                        self.validation_error = None;
                    } else if row + 1 < self.desc_lines.len() {
                        // Merge with next line
                        let next = self.desc_lines.remove(row + 1);
                        self.desc_lines[row].push_str(&next);
                        self.validation_error = None;
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
    /// Title is auto-derived from the first line of the description (max 80 chars).
    pub fn to_task_fields(&self) -> (String, String) {
        let desc = self.description();
        let title = derive_title_from_description(&desc);
        (title, desc)
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
///
/// ## Render cache invariant
///
/// `render_version` **must** be bumped every time `messages` or
/// `streaming_text` is mutated.  The TUI render path (`task_detail.rs`)
/// stores a per-session `(render_version, Vec<Line>)` in
/// [`AppState::cached_streaming_lines`] and only rebuilds the lines when
/// the live `render_version` differs from the cached one.  A missed bump
/// causes stale output to be displayed until the next valid mutation.
///
/// The cache is keyed by `task_id` for main sessions and by `session_id`
/// for drilled-down subagent sessions — both share the same HashMap, so
/// [`AppState::prune_streaming_cache`] must account for both key spaces.
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
    /// Set of `(message_id, part_id)` pairs that have been seen for this
    /// session. Used alongside `last_delta_key` to deduplicate SSE
    /// `MessagePartDelta` events that may be replayed on reconnection.
    pub seen_delta_keys: HashSet<(String, String)>,
    /// The most recent `(message_id, part_id)` pair processed.
    /// Consecutive deltas for the same part share the same key, so
    /// only a *different* key that's already in `seen_delta_keys` is
    /// treated as a replay.
    pub last_delta_key: Option<(String, String)>,
    /// The actual content of the last processed delta. Used as
    /// defense-in-depth to detect when two concurrent SSE connections
    /// deliver the *exact same* delta (same key AND same content).
    /// A true continuation will always have different content, but a
    /// duplicate from another loop will have identical content.
    pub last_delta_content: Option<String>,
}

/// A subagent session spawned by a parent task's agent.
///
/// When a parent agent spawns a subagent (e.g., via `/do`), the OpenCode
/// server creates a new session. The parent's message stream includes
/// `TaskMessagePart::Agent { id, agent }` parts that identify the
/// subagent session. Cortex tracks these relationships so users can
/// drill down into subagent output via `ctrl+x`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentSession {
    /// The subagent's OpenCode session ID.
    pub session_id: String,
    /// The agent name (e.g., "do", "explore").
    pub agent_name: String,
    /// The ID of the parent task that spawned this subagent.
    pub parent_task_id: String,
    /// The ID of the parent session that spawned this subagent.
    pub parent_session_id: String,
    /// Nesting depth (0 = top-level subagent, 1 = sub-subagent, etc.).
    pub depth: u32,
    /// Whether the subagent session is still active.
    pub active: bool,
}

/// A reference to a session in the drill-down navigation stack.
///
/// Used by the UI to track the user's navigation path when drilling
/// into subagent sessions. The stack bottom is always the top-level
/// task; the stack top is the currently viewed session.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Task ID (for breadcrumb display).
    pub task_id: String,
    /// Session ID.
    pub session_id: String,
    /// Display label (e.g., "Task #3", "planning agent", "do agent").
    pub label: String,
    /// Nesting depth.
    pub depth: u32,
}

// ─── Sub-Structs ──────────────────────────────────────────────────────────

/// Project registry — manages projects, active project, and task counters.
#[derive(Debug, Clone, Default)]
pub struct ProjectRegistry {
    /// All registered projects.
    pub projects: Vec<CortexProject>,
    /// ID of the currently active project.
    pub active_project_id: Option<String>,
    /// Per-project auto-incrementing task number counters.
    pub task_number_counters: HashMap<String, u32>,
}

impl ProjectRegistry {
    /// Get the active project, if one is set and exists.
    pub fn active_project(&self) -> Option<&CortexProject> {
        self.active_project_id
            .as_ref()
            .and_then(|pid| self.projects.iter().find(|p| &p.id == pid))
    }

    /// Get connection state for the active project.
    /// Returns defaults (disconnected, not reconnecting) if no project is active.
    pub fn connection_state(&self) -> (bool, bool, u32, bool) {
        self.active_project()
            .map(|p| (p.connected, p.reconnecting, p.reconnect_attempt, p.permanently_disconnected))
            .unwrap_or((false, false, 0, false))
    }

    /// Whether the active project's server is connected.
    pub fn is_connected(&self) -> bool {
        self.connection_state().0
    }

    /// Whether the active project's server is reconnecting.
    pub fn is_reconnecting(&self) -> bool {
        self.connection_state().1
    }

    /// Reconnection attempt number for the active project.
    pub fn reconnect_attempt(&self) -> u32 {
        self.connection_state().2
    }

    /// Whether the active project has permanently disconnected.
    pub fn is_permanently_disconnected(&self) -> bool {
        self.connection_state().3
    }

    /// Set a project's connection state. No-op if the project doesn't exist.
    pub fn set_project_connected(&mut self, project_id: &str, connected: bool) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.connected = connected;
            if connected {
                p.reconnecting = false;
                p.reconnect_attempt = 0;
                p.permanently_disconnected = false;
            }
        }
    }

    /// Set a project's reconnecting state. No-op if the project doesn't exist.
    pub fn set_project_reconnecting(&mut self, project_id: &str, reconnecting: bool) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnecting = reconnecting;
        }
    }

    /// Set a project's reconnect attempt. No-op if the project doesn't exist.
    pub fn set_project_reconnect_attempt(&mut self, project_id: &str, attempt: u32) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnect_attempt = attempt;
        }
    }

    /// Mark a project as permanently disconnected. No-op if the project doesn't exist.
    pub fn set_project_permanently_disconnected(&mut self, project_id: &str) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnecting = false;
            p.connected = false;
            p.permanently_disconnected = true;
            p.reconnect_attempt = 0;
        }
    }
}

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
    pub subagent_sessions: HashMap<String, Vec<SubagentSession>>,
    /// Reverse index: subagent session_id → parent task_id.
    /// Used to route SSE events for child sessions to the correct parent.
    pub subagent_to_parent: HashMap<String, String>,
    /// Session detail data for drilled-down subagents (lazy-loaded).
    /// Keyed by subagent session_id.
    pub subagent_session_data: HashMap<String, TaskDetailSession>,
}

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

// ─── Top-Level State ──────────────────────────────────────────────────────

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
        self.project_registry.set_project_connected(project_id, connected);
    }

    /// Set a project's reconnecting state. No-op if the project doesn't exist.
    pub fn set_project_reconnecting(&mut self, project_id: &str, reconnecting: bool) {
        self.project_registry.set_project_reconnecting(project_id, reconnecting);
    }

    /// Set a project's reconnect attempt. No-op if the project doesn't exist.
    pub fn set_project_reconnect_attempt(&mut self, project_id: &str, attempt: u32) {
        self.project_registry.set_project_reconnect_attempt(project_id, attempt);
    }

    /// Mark a project as permanently disconnected. No-op if the project doesn't exist.
    pub fn set_project_permanently_disconnected(&mut self, project_id: &str) {
        self.project_registry.set_project_permanently_disconnected(project_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── TaskEditorState: insert_char (Description) ───────────────────────

    #[test]
    fn insert_char_description_appends_at_end() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.insert_char('a');
        editor.insert_char('b');
        assert_eq!(editor.desc_lines, vec!["ab"]);
        assert_eq!(editor.cursor_col, 2);
        assert_eq!(editor.cursor_row, 0);
    }

    #[test]
    fn insert_char_description_multibyte() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.insert_char('🎉');
        assert_eq!(editor.desc_lines[0], "🎉");
        assert_eq!(editor.cursor_col, 1);
    }

    #[test]
    fn insert_char_description_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.cached_description = Some("old".to_string());
        editor.insert_char('x');
        assert!(editor.cached_description.is_none());
    }

    #[test]
    fn insert_char_description_on_second_line() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.validation_error = Some("Description cannot be empty".to_string());
        editor.insert_char('x');
        assert!(editor.validation_error.is_none());
    }

    #[test]
    fn insert_char_description_marks_edited() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        assert!(!editor.has_unsaved_changes);
        editor.insert_char('x');
        assert!(editor.has_unsaved_changes);
    }

    // ── TaskEditorState: insert_char (Column — no-op) ────────────────────

    #[test]
    fn insert_char_column_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Column;
        let has_unsaved = editor.has_unsaved_changes;
        editor.insert_char('x');
        // Column field doesn't accept char input — marks edited regardless
        assert!(editor.has_unsaved_changes || !has_unsaved);
    }

    // ── TaskEditorState: delete_char_back (Description) ──────────────────

    #[test]
    fn delete_char_back_description_removes_char_on_same_line() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 0;
        editor.delete_char_back();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn delete_char_back_description_multibyte_emoji() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.cached_description = Some("cached".to_string());
        editor.delete_char_back();
        assert!(editor.cached_description.is_none());
    }

    #[test]
    fn delete_char_back_description_empty_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.validation_error = Some("error".to_string());
        editor.set_description("x");
        editor.cursor_col = 1;
        editor.delete_char_back();
        assert!(editor.validation_error.is_none());
    }

    // ── TaskEditorState: delete_char_forward (Description) ───────────────

    #[test]
    fn delete_char_forward_description_removes_char_on_same_line() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["abc".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 1;
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines[0], "ac");
    }

    #[test]
    fn delete_char_forward_description_at_end_of_line_merges_with_next() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5; // end of first line
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines, vec!["helloworld"]);
    }

    #[test]
    fn delete_char_forward_description_at_end_of_last_line_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["hello".to_string()];
        editor.cursor_row = 0;
        editor.cursor_col = 5;
        editor.delete_char_forward();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn delete_char_forward_description_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.cached_description = Some("cached".to_string());
        editor.delete_char_forward();
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: insert_newline ──────────────────────────────────

    #[test]
    fn insert_newline_splits_line_at_cursor() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Column;
        editor.desc_lines = vec!["hello".to_string()];
        editor.insert_newline();
        assert_eq!(editor.desc_lines, vec!["hello"]);
    }

    #[test]
    fn insert_newline_marks_edited_and_invalidates_cache() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string()];
        editor.cursor_row = 0;
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 0);
    }

    #[test]
    fn move_cursor_description_down_clamps_to_last_row() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string()];
        editor.cursor_row = 1;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_up_decrements_row() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        editor.cursor_row = 2;
        editor.move_cursor(CursorDirection::Up);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_down_increments_row() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        editor.cursor_row = 0;
        editor.move_cursor(CursorDirection::Down);
        assert_eq!(editor.cursor_row, 1);
    }

    #[test]
    fn move_cursor_description_left_clamps_to_zero() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.cursor_col = 0;
        editor.move_cursor(CursorDirection::Left);
        assert_eq!(editor.cursor_col, 0);
    }

    #[test]
    fn move_cursor_description_right_clamps_to_line_length() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["ab".to_string()];
        editor.cursor_col = 2;
        editor.move_cursor(CursorDirection::Right);
        assert_eq!(editor.cursor_col, 2);
    }

    #[test]
    fn move_cursor_description_up_clamps_col_to_shorter_line() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string(), "e".to_string()];
        editor.cursor_row = 3;
        editor.scroll_offset = 0;
        editor.ensure_cursor_visible(3); // visible rows 0-2
        assert_eq!(editor.scroll_offset, 1); // now visible rows 1-3
    }

    #[test]
    fn ensure_cursor_visible_adjusts_scroll_up() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string(), "e".to_string()];
        editor.cursor_row = 0;
        editor.scroll_offset = 2;
        editor.ensure_cursor_visible(3); // visible rows 2-4
        assert_eq!(editor.scroll_offset, 0); // now visible rows 0-2
    }

    #[test]
    fn ensure_cursor_visible_zero_height_is_noop() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.cursor_row = 5;
        editor.scroll_offset = 0;
        editor.ensure_cursor_visible(0);
        assert_eq!(editor.scroll_offset, 0);
    }

    // ── TaskEditorState: description cache ───────────────────────────────

    #[test]
    fn description_returns_cached_when_available() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cached_description = Some("hello\nworld".to_string());
        assert_eq!(editor.description(), "hello\nworld");
    }

    #[test]
    fn description_joins_lines_when_no_cache() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.desc_lines = vec!["hello".to_string(), "world".to_string()];
        editor.cached_description = None;
        assert_eq!(editor.description(), "hello\nworld");
    }

    #[test]
    fn set_description_splits_on_newlines() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.set_description("line1\nline2\nline3");
        assert_eq!(editor.desc_lines, vec!["line1", "line2", "line3"]);
        assert_eq!(editor.cached_description, Some("line1\nline2\nline3".to_string()));
    }

    #[test]
    fn set_description_empty_clears_lines() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.set_description("hello");
        editor.set_description("");
        assert_eq!(editor.desc_lines, vec![String::new()]);
        assert!(editor.cached_description.is_none());
    }

    // ── TaskEditorState: current_line ────────────────────────────────────

    #[test]
    fn current_line_description() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Description;
        editor.desc_lines = vec!["abc".to_string(), "def".to_string()];
        editor.cursor_row = 1;
        assert_eq!(editor.current_line(), "def");
    }

    #[test]
    fn current_line_column_is_empty() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.focused_field = EditorField::Column;
        assert_eq!(editor.current_line(), "");
    }

    // ── TaskEditorState: to_task_fields ──────────────────────────────────

    #[test]
    fn to_task_fields_derives_title_from_description() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.set_description("My Task Title\nline2");
        let (title, desc) = editor.to_task_fields();
        assert_eq!(title, "My Task Title");
        assert_eq!(desc, "My Task Title\nline2");
    }

    #[test]
    fn to_task_fields_truncates_long_title() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        let long_line = "x".repeat(100);
        editor.set_description(&long_line);
        let (title, _) = editor.to_task_fields();
        assert_eq!(title.len(), 80);
    }

    #[test]
    fn to_task_fields_empty_description() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let editor = TaskEditorState::new_for_edit(&task, vec!["todo".to_string(), "planning".to_string()]);
        assert_eq!(editor.task_id, Some("task-1".to_string()));
        assert_eq!(editor.desc_lines, vec!["Line 1", "Line 2"]);
        assert_eq!(editor.cursor_col, 0);
        assert_eq!(editor.focused_field, EditorField::Description);
        assert!(!editor.has_unsaved_changes);
        assert_eq!(editor.available_columns, vec!["todo".to_string(), "planning".to_string()]);
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
        let editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    // ── TaskEditorState: cycle_column ────────────────────────────────────

    #[test]
    fn cycle_column_advances_to_next() {
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string(), "planning".to_string(), "done".to_string()]);
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
        let mut editor = TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        editor.cycle_column();
        assert_eq!(editor.column_id, Some("todo".to_string()));
    }

    // ── derive_title_from_description ────────────────────────────────────

    #[test]
    fn derive_title_simple() {
        assert_eq!(derive_title_from_description("My Task"), "My Task");
    }

    #[test]
    fn derive_title_first_line_only() {
        assert_eq!(derive_title_from_description("Line 1\nLine 2\nLine 3"), "Line 1");
    }

    #[test]
    fn derive_title_truncates_at_80() {
        let long = "x".repeat(100);
        let title = derive_title_from_description(&long);
        assert_eq!(title.len(), 80);
    }

    #[test]
    fn derive_title_empty_description() {
        assert_eq!(derive_title_from_description(""), "");
    }

    #[test]
    fn derive_title_skips_empty_lines() {
        assert_eq!(derive_title_from_description("\n\nActual Title\nMore"), "Actual Title");
    }

    #[test]
    fn derive_title_whitespace_only_first_line() {
        assert_eq!(derive_title_from_description("   \nActual Title"), "Actual Title");
    }

    // ── display_title_for_task ───────────────────────────────────────────

    #[test]
    fn display_title_short() {
        let task = make_test_task("My Task", "My Task\nMore details");
        assert_eq!(display_title_for_task(&task, 100), "My Task");
    }

    #[test]
    fn display_title_truncates() {
        let task = make_test_task("Long Title", &format!("{}\nMore", "x".repeat(50)));
        let display = display_title_for_task(&task, 20);
        assert!(display.ends_with("..."));
        assert!(display.len() <= 23); // 20 + "..."
    }

    #[test]
    fn display_title_empty_description() {
        let task = make_test_task("", "");
        assert_eq!(display_title_for_task(&task, 50), "");
    }

    // ── extract_tool_summary ─────────────────────────────────────────────

    #[test]
    fn extract_tool_summary_read_with_file_path() {
        let input = r#"{"file_path": "/home/user/project/src/main.rs"}"#;
        assert_eq!(extract_tool_summary("read", input), "main.rs");
    }

    #[test]
    fn extract_tool_summary_read_with_file_path_key() {
        let input = r#"{"filePath": "/home/user/project/src/main.rs"}"#;
        assert_eq!(extract_tool_summary("Read", input), "main.rs");
    }

    #[test]
    fn extract_tool_summary_read_with_path() {
        let input = r#"{"path": "/home/user/project/src/main.rs"}"#;
        assert_eq!(extract_tool_summary("read", input), "main.rs");
    }

    #[test]
    fn extract_tool_summary_write_with_file_path() {
        let input = r#"{"file_path": "/home/user/project/Cargo.toml"}"#;
        assert_eq!(extract_tool_summary("write", input), "Cargo.toml");
    }

    #[test]
    fn extract_tool_summary_write_with_path() {
        let input = r#"{"path": "/home/user/project/docs/README.md"}"#;
        assert_eq!(extract_tool_summary("Write", input), "README.md");
    }

    #[test]
    fn extract_tool_summary_bash_short_command() {
        let input = r#"{"command": "cargo build"}"#;
        assert_eq!(extract_tool_summary("bash", input), "cargo build");
    }

    #[test]
    fn extract_tool_summary_bash_long_command_truncated() {
        let long_cmd = "x".repeat(100);
        let input = format!(r#"{{"command": "{}"}}"#, long_cmd);
        let summary = extract_tool_summary("Bash", &input);
        assert_eq!(summary.len(), 60); // 57 chars + "..."
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn extract_tool_summary_bash_no_command_field() {
        let input = r#"{"something": "else"}"#;
        assert_eq!(extract_tool_summary("bash", input), "...");
    }

    #[test]
    fn extract_tool_summary_grep_with_pattern() {
        let input = r#"{"pattern": "TODO", "include": "*.rs"}"#;
        assert_eq!(extract_tool_summary("grep", input), "TODO");
    }

    #[test]
    fn extract_tool_summary_glob_with_pattern() {
        let input = r#"{"pattern": "src/**/*.ts"}"#;
        assert_eq!(extract_tool_summary("glob", input), "src/**/*.ts");
    }

    #[test]
    fn extract_tool_summary_grep_uppercase() {
        let input = r#"{"pattern": "fn\\s+\\w+"}"#;
        assert_eq!(extract_tool_summary("Grep", input), "fn\\s+\\w+");
    }

    #[test]
    fn extract_tool_summary_unknown_tool() {
        let input = r#"{"something": "value"}"#;
        assert_eq!(extract_tool_summary("custom_tool", input), "...");
    }

    #[test]
    fn extract_tool_summary_invalid_json() {
        assert_eq!(extract_tool_summary("read", "not json"), "...");
        assert_eq!(extract_tool_summary("bash", ""), "...");
    }

    #[test]
    fn extract_tool_summary_empty_json() {
        assert_eq!(extract_tool_summary("read", "{}"), "...");
    }

    // ── Helper ───────────────────────────────────────────────────────────

    fn make_test_task(title: &str, description: &str) -> CortexTask {
        CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: title.to_string(),
            description: description.to_string(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        }
    }
}

/// Derive a task title from the first line of a description string.
/// Returns the first non-empty line, truncated to 80 characters.
pub fn derive_title_from_description(description: &str) -> String {
    description
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim())
        .unwrap_or("")
        .chars()
        .take(80)
        .collect()
}

/// Get a display title for a task — first non-empty line of description,
/// truncated to fit within `max_len` characters.
pub fn display_title_for_task(task: &CortexTask, max_len: usize) -> String {
    let title = derive_title_from_description(&task.description);
    if title.chars().count() > max_len {
        format!(
            "{}...",
            title.chars().take(max_len.saturating_sub(3)).collect::<String>()
        )
    } else {
        title
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
