//! Configuration type definitions for Cortex2.
//!
//! These types mirror the TOML config structure and support serde
//! serialization/deserialization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Top-Level Config ───

/// Root Cortex configuration, matching the structure of `cortex.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexConfig {
    #[serde(default)]
    pub opencode: OpenCodeConfig,
    #[serde(default)]
    pub columns: ColumnsConfig,
    #[serde(default)]
    pub orchestration: OrchestrationRulesConfig,
    #[serde(default)]
    pub keybindings: KeybindingConfig,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub log: LogConfig,
}

// ─── Log Configuration ───

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Log level: "trace", "debug", "info", "warn", "error".
    /// Overridden by the `RUST_LOG` environment variable if set.
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

// ─── Columns Configuration ───

/// Default value for `ColumnConfig::visible`.
fn default_true() -> bool {
    true
}

/// Configuration for a single kanban column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnConfig {
    /// Unique identifier (lowercase, e.g. "planning").
    pub id: String,
    /// Optional human-readable name override.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Whether this column is visible on the kanban board.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Agent to invoke when a task enters this column.
    #[serde(default)]
    pub agent: Option<String>,
    /// Optional auto-progression target when agent completes.
    #[serde(default)]
    pub auto_progress_to: Option<String>,
}

/// Top-level columns configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnsConfig {
    /// Ordered list of column definitions. Order determines board layout.
    #[serde(default)]
    pub definitions: Vec<ColumnConfig>,
}

impl Default for ColumnsConfig {
    fn default() -> Self {
        Self {
            definitions: vec![
                ColumnConfig {
                    id: "todo".to_string(),
                    display_name: Some("Todo".to_string()),
                    visible: true,
                    agent: None,
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "planning".to_string(),
                    display_name: Some("Plan".to_string()),
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some("running".to_string()),
                },
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: Some("Run".to_string()),
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: Some("Review".to_string()),
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "done".to_string(),
                    display_name: Some("Done".to_string()),
                    visible: false,
                    agent: None,
                    auto_progress_to: None,
                },
            ],
        }
    }
}

impl ColumnsConfig {
    /// Returns the display name for a column ID, falling back to the ID itself.
    pub fn display_name_for(&self, column_id: &str) -> String {
        self.definitions
            .iter()
            .find(|c| c.id == column_id)
            .and_then(|c| c.display_name.clone())
            .unwrap_or_else(|| column_id.to_string())
    }

    /// Returns the agent name for a column, if configured.
    pub fn agent_for_column(&self, column_id: &str) -> Option<String> {
        self.definitions
            .iter()
            .find(|c| c.id == column_id)
            .and_then(|c| c.agent.clone())
    }

    /// Returns the auto-progress target for a column, if configured.
    pub fn auto_progress_for(&self, column_id: &str) -> Option<String> {
        self.definitions
            .iter()
            .find(|c| c.id == column_id)
            .and_then(|c| c.auto_progress_to.clone())
    }

    /// Returns the visible columns in definition order.
    pub fn visible_column_ids(&self) -> Vec<&str> {
        self.definitions
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.id.as_str())
            .collect()
    }

    /// Returns all column IDs in definition order.
    pub fn all_column_ids(&self) -> Vec<&str> {
        self.definitions.iter().map(|c| c.id.as_str()).collect()
    }
}

// ─── OpenCode Configuration ───

/// Default value for `OpenCodeConfig::request_timeout_secs`.
fn default_request_timeout_secs() -> u64 {
    600
}

/// OpenCode server connection and agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeConfig {
    /// Server hostname.
    #[serde(default = "default_hostname")]
    pub hostname: String,
    /// Server port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Default model configuration.
    #[serde(default)]
    pub model: OpenCodeModelConfig,
    /// Named agent configurations keyed by agent name.
    #[serde(default)]
    pub agents: HashMap<String, OpenCodeAgentConfig>,
    /// MCP server definitions keyed by server name.
    #[serde(default, rename = "mcp_servers")]
    pub mcp_servers: HashMap<String, OpenCodeMcpServerConfig>,
    /// HTTP request timeout in seconds.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

fn default_hostname() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    11643
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            hostname: default_hostname(),
            port: default_port(),
            model: Default::default(),
            agents: HashMap::new(),
            mcp_servers: HashMap::new(),
            request_timeout_secs: default_request_timeout_secs(),
        }
    }
}

/// Per-agent configuration. All fields are optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeAgentConfig {
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub tools: Option<Vec<String>>,
    pub max_turns: Option<u32>,
    pub disable: Option<bool>,
    #[serde(default)]
    pub permission: Option<HashMap<String, serde_json::Value>>,
}

/// Model configuration specifying which LLM to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeModelConfig {
    /// Model identifier (e.g. "glm-5-turbo").
    #[serde(default = "default_model_id")]
    pub id: String,
    /// Provider name.
    #[serde(default)]
    pub provider: Option<String>,
    /// Environment variable name containing the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

fn default_model_id() -> String {
    "glm-5-turbo".to_string()
}

impl Default for OpenCodeModelConfig {
    fn default() -> Self {
        Self {
            id: default_model_id(),
            provider: None,
            api_key_env: None,
        }
    }
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeMcpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

// ─── Orchestration Configuration ───

/// Rules for automatic task progression and workflow automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationRulesConfig {
    /// Auto-progression: maps column name → target column name.
    #[serde(default)]
    pub ifdone: HashMap<String, String>,
    /// Auto-start agents when tasks enter columns.
    #[serde(default)]
    pub auto_start: HashMap<String, bool>,
    /// Notify when a column becomes empty.
    #[serde(default)]
    pub notify_column_empty: Vec<String>,
}

impl Default for OrchestrationRulesConfig {
    fn default() -> Self {
        let mut auto_start = HashMap::new();
        auto_start.insert("planning".to_string(), true);
        auto_start.insert("running".to_string(), true);
        auto_start.insert("review".to_string(), true);

        Self {
            ifdone: HashMap::new(),
            auto_start,
            notify_column_empty: vec![
                "planning".to_string(),
                "running".to_string(),
                "review".to_string(),
            ],
        }
    }
}

// ─── Keybinding Configuration ───

/// Keybinding definitions. Each value is a comma-separated list of key combos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindingConfig {
    #[serde(default = "default_leader")]
    pub leader: String,
    #[serde(default = "default_kanban_left")]
    pub kanban_left: String,
    #[serde(default = "default_kanban_right")]
    pub kanban_right: String,
    #[serde(default = "default_kanban_up")]
    pub kanban_up: String,
    #[serde(default = "default_kanban_down")]
    pub kanban_down: String,
    #[serde(default = "default_kanban_move_forward")]
    pub kanban_move_forward: String,
    #[serde(default = "default_kanban_move_backward")]
    pub kanban_move_backward: String,
    #[serde(default = "default_todo_new")]
    pub todo_new: String,
    #[serde(default = "default_todo_edit")]
    pub todo_edit: String,
    #[serde(default = "default_task_delete")]
    pub task_delete: String,
    #[serde(default = "default_task_view")]
    pub task_view: String,
    #[serde(default = "default_prev_project")]
    pub prev_project: String,
    #[serde(default = "default_next_project")]
    pub next_project: String,
    #[serde(default = "default_new_project")]
    pub new_project: String,
    #[serde(default = "default_abort_session")]
    pub abort_session: String,
    #[serde(default = "default_help_toggle")]
    pub help_toggle: String,
    #[serde(default = "default_quit")]
    pub quit: String,
}

// Default keybinding values
fn default_leader() -> String {
    "ctrl+a".to_string()
}
fn default_kanban_left() -> String {
    "h, left".to_string()
}
fn default_kanban_right() -> String {
    "l, right".to_string()
}
fn default_kanban_up() -> String {
    "k, up".to_string()
}
fn default_kanban_down() -> String {
    "j, down".to_string()
}
fn default_kanban_move_forward() -> String {
    "m".to_string()
}
fn default_kanban_move_backward() -> String {
    "shift+m".to_string()
}
fn default_todo_new() -> String {
    "n".to_string()
}
fn default_todo_edit() -> String {
    "e".to_string()
}
fn default_task_delete() -> String {
    "x".to_string()
}
fn default_task_view() -> String {
    "v".to_string()
}
fn default_prev_project() -> String {
    "ctrl+k".to_string()
}
fn default_next_project() -> String {
    "ctrl+j".to_string()
}
fn default_new_project() -> String {
    "ctrl+n".to_string()
}
fn default_abort_session() -> String {
    "ctrl+a a".to_string()
}
fn default_help_toggle() -> String {
    "?".to_string()
}
fn default_quit() -> String {
    "ctrl+q".to_string()
}

impl Default for KeybindingConfig {
    fn default() -> Self {
        Self {
            leader: default_leader(),
            kanban_left: default_kanban_left(),
            kanban_right: default_kanban_right(),
            kanban_up: default_kanban_up(),
            kanban_down: default_kanban_down(),
            kanban_move_forward: default_kanban_move_forward(),
            kanban_move_backward: default_kanban_move_backward(),
            todo_new: default_todo_new(),
            todo_edit: default_todo_edit(),
            task_delete: default_task_delete(),
            task_view: default_task_view(),
            prev_project: default_prev_project(),
            next_project: default_next_project(),
            new_project: default_new_project(),
            abort_session: default_abort_session(),
            help_toggle: default_help_toggle(),
            quit: default_quit(),
        }
    }
}

// ─── Theme Configuration ───

/// Visual theme settings controlling colors and dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u16,
    #[serde(default = "default_column_width")]
    pub column_width: u16,
    #[serde(default = "default_status_working")]
    pub status_working: String,
    #[serde(default = "default_status_done")]
    pub status_done: String,
    #[serde(default = "default_status_question")]
    pub status_question: String,
    #[serde(default = "default_status_error")]
    pub status_error: String,
    #[serde(default = "default_status_hung")]
    pub status_hung: String,
}

fn default_sidebar_width() -> u16 {
    20
}
fn default_column_width() -> u16 {
    30
}
fn default_status_working() -> String {
    "#2196F3".to_string()
}
fn default_status_done() -> String {
    "#4CAF50".to_string()
}
fn default_status_question() -> String {
    "#FF9800".to_string()
}
fn default_status_error() -> String {
    "#F44336".to_string()
}
fn default_status_hung() -> String {
    "#FF5722".to_string()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            sidebar_width: default_sidebar_width(),
            column_width: default_column_width(),
            status_working: default_status_working(),
            status_done: default_status_done(),
            status_question: default_status_question(),
            status_error: default_status_error(),
            status_hung: default_status_hung(),
        }
    }
}
