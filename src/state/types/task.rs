//! Task-related types for the Cortex application.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::enums::*;

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
    /// Pending description update queued by the user. When the task reaches
    /// `Ready` or `Complete` status, this value is applied to `description`
    /// and cleared. Only the latest submitted description is kept.
    pub pending_description: Option<String>,
    /// A follow-up prompt queued by the user while the agent is running.
    /// Sent to the agent after the current prompt completes.
    /// Only the most recent queued prompt is kept.
    pub queued_prompt: Option<String>,
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
    /// Key findings from the planning phase (file paths, structures, etc.).
    /// Passed to the do agent to avoid redundant discovery. Populated by
    /// [`extract_planning_context`](crate::state::sse_processor::AppState::extract_planning_context).
    pub planning_context: Option<String>,
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
    /// The prompt text that generated the current streaming output.
    /// Displayed as a pinned header at the top of the agent output.
    /// Set when a prompt is sent and cleared when the session is reset.
    pub active_prompt: Option<String>,
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
    /// Error message if the subagent failed, `None` otherwise.
    pub error_message: Option<String>,
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
            title
                .chars()
                .take(max_len.saturating_sub(3))
                .collect::<String>()
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

// ── Task tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        }
    }

    // ── AgentStatus::Question ──────────────────────────────────────────

    #[test]
    fn question_status_is_terminal() {
        assert!(AgentStatus::Question.is_terminal());
    }

    #[test]
    fn question_status_display() {
        assert_eq!(AgentStatus::Question.to_string(), "question");
    }

    #[test]
    fn question_status_icon() {
        assert_eq!(AgentStatus::Question.icon(), "?");
    }

    #[test]
    fn running_status_is_not_terminal() {
        assert!(!AgentStatus::Running.is_terminal());
        assert!(!AgentStatus::Pending.is_terminal());
    }

    // ── derive_title_from_description ────────────────────────────────────

    #[test]
    fn derive_title_simple() {
        assert_eq!(derive_title_from_description("My Task"), "My Task");
    }

    #[test]
    fn derive_title_first_line_only() {
        assert_eq!(
            derive_title_from_description("Line 1\nLine 2\nLine 3"),
            "Line 1"
        );
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
        assert_eq!(
            derive_title_from_description("\n\nActual Title\nMore"),
            "Actual Title"
        );
    }

    #[test]
    fn derive_title_whitespace_only_first_line() {
        assert_eq!(
            derive_title_from_description("   \nActual Title"),
            "Actual Title"
        );
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
}
