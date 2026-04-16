//! Default configuration values.

use super::types::*;

/// Static TOML string of the default config.
pub const DEFAULT_CONFIG_TOML: &str = r#"
[opencode]
hostname = "127.0.0.1"
port = 11643
request_timeout_secs = 600

[opencode.model]
id = "glm-5-turbo"

[opencode.agents.planning]
model = "claude-sonnet-4-20250514"
instructions = "You are a planner."

[[columns]]
id = "todo"
display_name = "Todo"

[[columns]]
id = "planning"
display_name = "Plan"
agent = "planning"
auto_progress_to = "running"

[[columns]]
id = "running"
display_name = "Run"
agent = "do"

[[columns]]
id = "review"
display_name = "Review"
agent = "reviewer-alpha"

[[columns]]
id = "done"
display_name = "Done"
visible = false

[orchestration]
auto_start = { planning = true, running = true, review = true }
notify_column_empty = ["planning", "running", "review"]

[keybindings]
quit = "ctrl+q"
help_toggle = "?"
kanban_left = "h, left"
kanban_right = "l, right"
kanban_up = "k, up"
kanban_down = "j, down"
kanban_move_forward = "m"
kanban_move_backward = "shift+m"
todo_new = "n"
todo_edit = "e"
task_delete = "x"
task_view = "v"
prev_project = "ctrl+k"
next_project = "ctrl+j"
new_project = "ctrl+n"
abort_session = "ctrl+a a"

[theme]
sidebar_width = 20
column_width = 30

[log]
level = "info"
"#;

/// Returns a sensible default config.
pub fn default_config() -> CortexConfig {
    CortexConfig {
        opencode: OpenCodeConfig::default(),
        columns: ColumnsConfig::default(),
        keybindings: KeybindingConfig::default(),
        theme: ThemeConfig::default(),
        log: LogConfig::default(),
    }
}
