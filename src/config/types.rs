//! Configuration type definitions for Cortex.
//!
//! These types mirror the TOML config structure and support serde
//! serialization/deserialization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Top-Level Config ───

/// Root Cortex configuration, matching the structure of `cortex.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CortexConfig {
    #[serde(default)]
    pub opencode: OpenCodeConfig,
    #[serde(default)]
    pub columns: ColumnsConfig,
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
    "warn".to_string()
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
    /// Pre-computed list of visible column IDs. Populated by `finalize()`.
    /// Skipped during serialization since it's derived from `definitions`.
    #[serde(skip, default = "Vec::new")]
    pub(crate) visible_ids: Vec<String>,
}

impl Default for ColumnsConfig {
    fn default() -> Self {
        let mut config = Self {
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
                    auto_progress_to: Some("review".to_string()),
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
            visible_ids: Vec::new(),
        };
        config.finalize();
        config
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

    /// Populates the cached visible column IDs from the current definitions.
    /// Must be called after deserialization or any change to `definitions`.
    pub fn finalize(&mut self) {
        self.visible_ids = self
            .definitions
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.id.clone())
            .collect();
    }

    /// Returns the visible columns in definition order (cached, zero-allocation).
    pub fn visible_column_ids(&self) -> &[String] {
        &self.visible_ids
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
    /// Maximum consecutive SSE reconnection attempts before giving up.
    /// Set to 0 to retry forever (not recommended). Defaults to 50.
    #[serde(default = "default_sse_max_retries")]
    pub sse_max_retries: u32,
    /// Read timeout (per-chunk) for SSE event streams, in seconds.
    /// The timer resets after every successful chunk, so the stream
    /// only dies when no data arrives for this duration. Defaults to 120s.
    #[serde(default = "default_sse_read_timeout_secs")]
    pub sse_read_timeout_secs: u64,
    /// Timeout in seconds after which a Running task with no SSE activity
    /// is considered "Hung". Defaults to 300 (5 minutes).
    #[serde(default = "default_hung_agent_timeout_secs")]
    pub hung_agent_timeout_secs: u64,
    /// Number of consecutive agent start failures before the circuit
    /// breaker trips for a project. When tripped, auto-progression is
    /// paused until the user manually retries. Defaults to 3.
    #[serde(default = "default_circuit_breaker_threshold")]
    pub circuit_breaker_threshold: u32,
    /// Cooldown in seconds before a tripped circuit breaker enters half-open
    /// state, allowing a single probe attempt. Defaults to 60.
    #[serde(default = "default_circuit_breaker_cooldown_secs")]
    pub circuit_breaker_cooldown_secs: i64,
    /// Maximum number of concurrent agent sessions per project.
    /// When the limit is reached, new agent starts are queued until a slot opens.
    /// Default: 3
    #[serde(default = "default_max_concurrent_agents")]
    pub max_concurrent_agents: usize,
}

fn default_sse_max_retries() -> u32 {
    50
}

/// Default value for `OpenCodeConfig::sse_read_timeout_secs`.
///
/// Set to 120s (2 minutes) to avoid false reconnection triggers during idle
/// periods. The timer resets after every successful chunk read, so the stream
/// only dies when no data (events, heartbeats, or keep-alive comments) arrives
/// for this duration. A value that's too low (e.g. 60s) causes the SSE client
/// to disconnect and reconnect every ~62 seconds during idle, producing a
/// noticeable yellow "reconnecting" flash in the status bar.
fn default_sse_read_timeout_secs() -> u64 {
    120
}

/// Default value for `OpenCodeConfig::hung_agent_timeout_secs`.
fn default_hung_agent_timeout_secs() -> u64 {
    300
}

/// Default value for `OpenCodeConfig::circuit_breaker_threshold`.
fn default_circuit_breaker_threshold() -> u32 {
    3
}

/// Default value for `OpenCodeConfig::circuit_breaker_cooldown_secs`.
fn default_circuit_breaker_cooldown_secs() -> i64 {
    60
}

/// Default value for `OpenCodeConfig::max_concurrent_agents`.
fn default_max_concurrent_agents() -> usize {
    3
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
            sse_max_retries: default_sse_max_retries(),
            sse_read_timeout_secs: default_sse_read_timeout_secs(),
            hung_agent_timeout_secs: default_hung_agent_timeout_secs(),
            circuit_breaker_threshold: default_circuit_breaker_threshold(),
            circuit_breaker_cooldown_secs: default_circuit_breaker_cooldown_secs(),
            max_concurrent_agents: default_max_concurrent_agents(),
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

// ─── Editor Keybinding Configuration ───

/// Keybinding definitions for the task editor.
///
/// These are the configurable keybindings for actions within the fullscreen
/// task editor (e.g. save, cancel, cycle fields). Standard text-editing keys
/// (arrow keys, backspace, delete, home, end, page up/down) are not
/// configurable since they follow universal conventions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorKeybindingConfig {
    /// Key combo to save the task and close the editor.
    /// Default: "ctrl+s, ctrl+enter"
    #[serde(default = "default_editor_save")]
    pub save: String,
    /// Key combo to cancel editing and discard changes.
    /// Default: "esc"
    #[serde(default = "default_editor_cancel")]
    pub cancel: String,
    /// Key combo to cycle focus between editor fields (e.g. description ↔ column).
    /// Default: "tab"
    #[serde(default = "default_editor_cycle_field")]
    pub cycle_field: String,
    /// Key combo to insert a newline in the description field.
    /// Default: "enter"
    #[serde(default = "default_editor_newline")]
    pub newline: String,
}

fn default_editor_save() -> String {
    "ctrl+s, ctrl+enter".to_string()
}

fn default_editor_cancel() -> String {
    "esc".to_string()
}

fn default_editor_cycle_field() -> String {
    "tab".to_string()
}

fn default_editor_newline() -> String {
    "enter".to_string()
}

impl Default for EditorKeybindingConfig {
    fn default() -> Self {
        Self {
            save: default_editor_save(),
            cancel: default_editor_cancel(),
            cycle_field: default_editor_cycle_field(),
            newline: default_editor_newline(),
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
    #[serde(default = "default_task_open")]
    pub task_open: String,
    #[serde(default = "default_task_delete")]
    pub task_delete: String,
    #[serde(default = "default_prev_project")]
    pub prev_project: String,
    #[serde(default = "default_next_project")]
    pub next_project: String,
    #[serde(default = "default_new_project")]
    pub new_project: String,
    #[serde(default = "default_rename_project")]
    pub rename_project: String,
    #[serde(default = "default_set_working_directory")]
    pub set_working_directory: String,
    #[serde(default = "default_delete_project")]
    pub delete_project: String,
    #[serde(default = "default_abort_session")]
    pub abort_session: String,
    #[serde(default = "default_retry_task")]
    pub retry_task: String,
    #[serde(default = "default_drill_down_subagent")]
    pub drill_down_subagent: String,
    #[serde(default = "default_scroll_kanban_left")]
    pub scroll_kanban_left: String,
    #[serde(default = "default_scroll_kanban_right")]
    pub scroll_kanban_right: String,
    #[serde(default = "default_help_toggle")]
    pub help_toggle: String,
    #[serde(default = "default_quit")]
    pub quit: String,
    #[serde(default = "default_review_changes")]
    pub review_changes: String,
    #[serde(default = "default_task_move_up")]
    pub task_move_up: String,
    #[serde(default = "default_task_move_down")]
    pub task_move_down: String,
    /// Editor-specific keybindings (task editor mode).
    #[serde(default)]
    pub editor: EditorKeybindingConfig,
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
fn default_task_open() -> String {
    "enter".to_string()
}
fn default_task_delete() -> String {
    "x".to_string()
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
fn default_rename_project() -> String {
    "r".to_string()
}
fn default_set_working_directory() -> String {
    "d".to_string()
}
fn default_delete_project() -> String {
    "shift+x".to_string()
}
fn default_abort_session() -> String {
    "ctrl+a a".to_string()
}
fn default_retry_task() -> String {
    "shift+r".to_string()
}
fn default_drill_down_subagent() -> String {
    "ctrl+x".to_string()
}
fn default_scroll_kanban_left() -> String {
    "pageup".to_string()
}
fn default_scroll_kanban_right() -> String {
    "pagedown".to_string()
}
fn default_help_toggle() -> String {
    "?".to_string()
}
fn default_quit() -> String {
    "ctrl+q".to_string()
}
fn default_review_changes() -> String {
    "shift+d".to_string()
}
fn default_task_move_up() -> String {
    "ctrl+k".to_string()
}
fn default_task_move_down() -> String {
    "ctrl+j".to_string()
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
            task_open: default_task_open(),
            task_delete: default_task_delete(),
            prev_project: default_prev_project(),
            next_project: default_next_project(),
            new_project: default_new_project(),
            rename_project: default_rename_project(),
            set_working_directory: default_set_working_directory(),
            delete_project: default_delete_project(),
            abort_session: default_abort_session(),
            retry_task: default_retry_task(),
            drill_down_subagent: default_drill_down_subagent(),
            scroll_kanban_left: default_scroll_kanban_left(),
            scroll_kanban_right: default_scroll_kanban_right(),
            help_toggle: default_help_toggle(),
            quit: default_quit(),
            review_changes: default_review_changes(),
            task_move_up: default_task_move_up(),
            task_move_down: default_task_move_down(),
            editor: EditorKeybindingConfig::default(),
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
    /// Tick rate in milliseconds for the TUI event loop. Default: 100.
    #[serde(default = "default_tick_rate_ms")]
    pub tick_rate_ms: u64,
    #[serde(default = "default_status_working")]
    pub status_working: String,
    #[serde(default = "default_status_done")]
    pub status_done: String,
    #[serde(default = "default_status_question")]
    pub status_question: String,
    #[serde(default = "default_status_error")]
    pub status_error: String,
    /// Status bar color for "connected". Default: "#4CAF50" (green).
    #[serde(default = "default_status_connected")]
    pub status_connected: String,
    /// Status bar color for "disconnected". Default: "#888888" (gray).
    #[serde(default = "default_status_disconnected")]
    pub status_disconnected: String,
    /// Status bar color for "reconnecting". Default: "#FFC107" (amber).
    #[serde(default = "default_status_reconnecting")]
    pub status_reconnecting: String,
}

fn default_sidebar_width() -> u16 {
    20
}
fn default_column_width() -> u16 {
    30
}
fn default_tick_rate_ms() -> u64 {
    100
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
fn default_status_connected() -> String {
    "#4CAF50".to_string()
}
fn default_status_disconnected() -> String {
    "#888888".to_string()
}
fn default_status_reconnecting() -> String {
    "#FFC107".to_string()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            sidebar_width: default_sidebar_width(),
            column_width: default_column_width(),
            tick_rate_ms: default_tick_rate_ms(),
            status_working: default_status_working(),
            status_done: default_status_done(),
            status_question: default_status_question(),
            status_error: default_status_error(),
            status_connected: default_status_connected(),
            status_disconnected: default_status_disconnected(),
            status_reconnecting: default_status_reconnecting(),
        }
    }
}

impl ThemeConfig {
    /// Parse a hex color string (e.g. `"#2196F3"`) into a `ratatui::Color`.
    ///
    /// Returns `None` if the string is not a valid 6-digit hex color.
    /// Falls back to the provided default color on parse failure.
    pub fn color_or(&self, hex: &str, default: ratatui::prelude::Color) -> ratatui::prelude::Color {
        parse_hex_color_or(hex, default)
    }

    /// Status color for running/working agents.
    /// Uses the configured `status_working` hex color, falling back to blue.
    pub fn working_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_working, ratatui::prelude::Color::Rgb(33, 150, 243))
    }

    /// Status color for completed agents.
    /// Uses the configured `status_done` hex color, falling back to green.
    pub fn done_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_done, ratatui::prelude::Color::Rgb(76, 175, 80))
    }

    /// Status color for question/hung states.
    /// Uses the configured `status_question` hex color, falling back to orange.
    pub fn question_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_question, ratatui::prelude::Color::Rgb(255, 152, 0))
    }

    /// Status color for error states.
    /// Uses the configured `status_error` hex color, falling back to red.
    pub fn error_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_error, ratatui::prelude::Color::Rgb(244, 67, 54))
    }

    /// Status bar color for "connected". Default: green.
    pub fn connected_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_connected, ratatui::prelude::Color::Rgb(76, 175, 80))
    }

    /// Status bar color for "disconnected". Default: gray.
    pub fn disconnected_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_disconnected, ratatui::prelude::Color::Rgb(136, 136, 136))
    }

    /// Status bar color for "reconnecting". Default: amber.
    pub fn reconnecting_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_reconnecting, ratatui::prelude::Color::Rgb(255, 193, 7))
    }

    /// Notification color for info messages. Reuses `status_working` (blue).
    pub fn info_color(&self) -> ratatui::prelude::Color {
        parse_hex_color_or(&self.status_working, ratatui::prelude::Color::Rgb(33, 150, 243))
    }
}

/// Parse a `#RRGGBB` hex string into a [`ratatui::prelude::Color::Rgb`].
///
/// Returns the provided `default` color if the string is not exactly 7 characters
/// (`#` + 6 hex digits) or if any digit is not valid hex.
pub fn parse_hex_color_or(hex: &str, default: ratatui::prelude::Color) -> ratatui::prelude::Color {
    parse_hex_color(hex).unwrap_or(default)
}

/// Parse a `#RRGGBB` hex string into a [`ratatui::prelude::Color::Rgb`].
///
/// Returns `None` if the string is not exactly 7 characters (`#` + 6 hex digits)
/// or if any digit is not valid hex.
pub fn parse_hex_color(hex: &str) -> Option<ratatui::prelude::Color> {
    if hex.len() != 7 || hex.as_bytes()[0] != b'#' {
        return None;
    }
    let r = u8::from_str_radix(&hex[1..3], 16).ok()?;
    let g = u8::from_str_radix(&hex[3..5], 16).ok()?;
    let b = u8::from_str_radix(&hex[5..7], 16).ok()?;
    Some(ratatui::prelude::Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ColumnsConfig::finalize ──────────────────────────────────────────

    #[test]
    fn finalize_all_visible() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "a".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "b".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert_eq!(config.visible_ids, vec!["a", "b"]);
    }

    #[test]
    fn finalize_skips_hidden_columns() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "a".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "b".into(), display_name: None, visible: false, agent: None, auto_progress_to: None },
                ColumnConfig { id: "c".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert_eq!(config.visible_ids, vec!["a", "c"]);
    }

    #[test]
    fn finalize_all_hidden() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "a".into(), display_name: None, visible: false, agent: None, auto_progress_to: None },
                ColumnConfig { id: "b".into(), display_name: None, visible: false, agent: None, auto_progress_to: None },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert!(config.visible_ids.is_empty());
    }

    #[test]
    fn finalize_empty_definitions() {
        let mut config = ColumnsConfig {
            definitions: vec![],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert!(config.visible_ids.is_empty());
    }

    #[test]
    fn finalize_overwrites_previous_visible_ids() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "x".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: vec!["old1".to_string(), "old2".to_string()],
        };
        config.finalize();
        assert_eq!(config.visible_ids, vec!["x"]);
    }

    #[test]
    fn finalize_preserves_definition_order() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "z".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "a".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "m".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert_eq!(config.visible_ids, vec!["z", "a", "m"]);
    }

    #[test]
    fn visible_column_ids_returns_cached_slice() {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "a".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "b".into(), display_name: None, visible: false, agent: None, auto_progress_to: None },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        assert_eq!(config.visible_column_ids(), &["a"]);
    }

    #[test]
    fn all_column_ids_returns_all() {
        let config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "a".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
                ColumnConfig { id: "b".into(), display_name: None, visible: false, agent: None, auto_progress_to: None },
            ],
            visible_ids: vec!["a".to_string()],
        };
        assert_eq!(config.all_column_ids(), vec!["a", "b"]);
    }

    // ── parse_hex_color ──────────────────────────────────────────────────

    #[test]
    fn parse_hex_color_valid_black() {
        assert_eq!(parse_hex_color("#000000"), Some(ratatui::prelude::Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn parse_hex_color_valid_white() {
        assert_eq!(parse_hex_color("#FFFFFF"), Some(ratatui::prelude::Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn parse_hex_color_valid_mixed() {
        assert_eq!(parse_hex_color("#2196F3"), Some(ratatui::prelude::Color::Rgb(0x21, 0x96, 0xF3)));
    }

    #[test]
    fn parse_hex_color_valid_lowercase() {
        assert_eq!(parse_hex_color("#ff00aa"), Some(ratatui::prelude::Color::Rgb(255, 0, 170)));
    }

    #[test]
    fn parse_hex_color_valid_mixed_case() {
        assert_eq!(parse_hex_color("#AbCdEf"), Some(ratatui::prelude::Color::Rgb(0xAB, 0xCD, 0xEF)));
    }

    #[test]
    fn parse_hex_color_empty_string() {
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn parse_hex_color_too_short() {
        assert_eq!(parse_hex_color("#FFF"), None);
        assert_eq!(parse_hex_color("#FF"), None);
        assert_eq!(parse_hex_color("#"), None);
    }

    #[test]
    fn parse_hex_color_too_long() {
        assert_eq!(parse_hex_color("#FFFFFF00"), None);
    }

    #[test]
    fn parse_hex_color_wrong_prefix() {
        assert_eq!(parse_hex_color("FFFFFF"), None);
        assert_eq!(parse_hex_color("0xFFFFFF"), None);
        assert_eq!(parse_hex_color("0xFFFFFF"), None);
    }

    #[test]
    fn parse_hex_color_invalid_hex_chars() {
        assert_eq!(parse_hex_color("#GGHHII"), None);
        assert_eq!(parse_hex_color("#ZZZZZZ"), None);
        assert_eq!(parse_hex_color("#12345G"), None);
    }

    // ── parse_hex_color_or ───────────────────────────────────────────────

    #[test]
    fn parse_hex_color_or_valid_returns_parsed() {
        let result = parse_hex_color_or("#FF0000", ratatui::prelude::Color::Blue);
        assert_eq!(result, ratatui::prelude::Color::Rgb(255, 0, 0));
    }

    #[test]
    fn parse_hex_color_or_invalid_returns_default() {
        let default = ratatui::prelude::Color::Rgb(33, 150, 243);
        let result = parse_hex_color_or("not-a-color", default);
        assert_eq!(result, default);
    }

    #[test]
    fn parse_hex_color_or_empty_returns_default() {
        let default = ratatui::prelude::Color::Rgb(76, 175, 80);
        let result = parse_hex_color_or("", default);
        assert_eq!(result, default);
    }

    // ── display_name_for ─────────────────────────────────────────────────

    #[test]
    fn display_name_for_with_display_name() {
        let config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "todo".into(), display_name: Some("Todo List".into()), visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: vec!["todo".into()],
        };
        assert_eq!(config.display_name_for("todo"), "Todo List");
    }

    #[test]
    fn display_name_for_falls_back_to_id() {
        let config = ColumnsConfig {
            definitions: vec![
                ColumnConfig { id: "custom".into(), display_name: None, visible: true, agent: None, auto_progress_to: None },
            ],
            visible_ids: vec!["custom".into()],
        };
        assert_eq!(config.display_name_for("custom"), "custom");
    }

    #[test]
    fn display_name_for_unknown_column() {
        let config = ColumnsConfig {
            definitions: vec![],
            visible_ids: vec![],
        };
        assert_eq!(config.display_name_for("nonexistent"), "nonexistent");
    }
}
