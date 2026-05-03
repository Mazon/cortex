//! Enum type definitions for the Cortex application.

use serde::{Deserialize, Serialize};

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
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Pending,
    Running,
    Hung,
    Question,
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
            AgentStatus::Question => "?",
            AgentStatus::Ready => "◉",
            AgentStatus::Complete => "✓",
            AgentStatus::Error => "✗",
        }
    }

    /// Returns `true` if the agent has reached a terminal state and is no
    /// longer actively working. Tasks in terminal states should have their
    /// timer frozen rather than continuing to tick.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentStatus::Complete
                | AgentStatus::Ready
                | AgentStatus::Hung
                | AgentStatus::Question
                | AgentStatus::Error
        )
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Pending => write!(f, "pending"),
            AgentStatus::Running => write!(f, "working"),
            AgentStatus::Hung => write!(f, "hung"),
            AgentStatus::Question => write!(f, "question"),
            AgentStatus::Ready => write!(f, "ready"),
            AgentStatus::Complete => write!(f, "done"),
            AgentStatus::Error => write!(f, "failed"),
        }
    }
}

/// Project status.
#[non_exhaustive]
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

/// Review decision for tasks in the review column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewStatus {
    /// Review not yet started or still in progress.
    Pending,
    /// Reviewer agent has completed; awaiting human decision.
    AwaitingDecision,
    /// User approved the changes.
    Approved,
    /// User rejected the changes.
    Rejected,
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
    /// Diff review mode — view git diff for a completed "do" task.
    DiffReview,
    /// Reports mode — view project statistics and recent git commits.
    Reports,
    /// Confirm delete mode — y/n confirmation dialog for task deletion.
    ConfirmDelete,
    /// Archive viewer — view and manage archived tasks.
    Archive,
    /// Config editor — inline TOML editor for cortex.toml.
    ConfigEditor,
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

/// Help overlay section identifiers (kept for potential future use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTab {
    Global,
    Kanban,
    Review,
    Editor,
}

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

/// Kind of diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Context line (unchanged).
    Context,
    /// Addition line (added).
    Addition,
    /// Removal line (deleted).
    Removal,
    /// Hunk header (e.g., `@@ -10,6 +10,15 @@`).
    HunkHeader {
        old_start: u32,
        old_count: u32,
        new_start: u32,
        new_count: u32,
    },
    /// "\ No newline at end of file" marker.
    NoNewlineAtEndOfFile,
}
