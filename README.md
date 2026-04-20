# Cortex

**Cortex** is a terminal-based Kanban board for AI-powered task management. It integrates with [OpenCode](https://github.com/opencode-ai/opencode) to let you assign AI agents to tasks, stream their output in real time, and track progress through configurable workflow columns — all from your terminal.

```
┌──────────┬────────────┬────────────┬────────────┬────────────┐
│ Projects │   Todo     │   Plan     │   Run      │   Review   │
│          │            │            │            │            │
│ ● Default│ 1. Add ... │ 3. Refac.. │ 5. Fix ..  │            │
│          │            │   ◐ work.. │   ◐ work.. │            │
│          │ 2. Write.. │            │            │            │
│          │            │            │            │            │
│          │            │            │            │            │
├──────────┴────────────┴────────────┴────────────┴────────────┤
│ ● connected  │  Default  │  5 tasks  │  ? for help          │
└──────────────────────────────────────────────────────────────┘
```

## Features

- **Kanban board** — Visual column-based task management in the terminal
- **AI agent integration** — Assign configurable agents (planning, coding, reviewing) to columns; agents are invoked automatically when tasks enter their column
- **Real-time SSE streaming** — Watch agent output stream live in the task detail view via Server-Sent Events
- **Multi-project support** — Switch between projects with separate working directories and independent OpenCode server instances
- **Configurable workflow** — Define custom columns, assign agents, and set up auto-progression rules (e.g. planning completes → move to running)
- **Task persistence** — All state is saved to a local SQLite database and restored on startup
- **Customizable keybindings** — Vim-style navigation by default; fully remappable via config
- **Theming** — Configurable sidebar width, column width, and status indicator colors
- **Tool permission system** — Read-only tools are auto-approved; destructive tools require user confirmation

## Installation

### From source

Requires [Rust](https://www.rust-lang.org/tools/install) 1.70+.

```sh
git clone https://github.com/opencode-ai/cortex.git
cd cortex
cargo install --path .
```

### Build and run directly

```sh
cargo run --release
```

## Usage

### First run

```sh
cortex
```

On first launch, Cortex creates a default project pointing at your current working directory. If [OpenCode](https://github.com/opencode-ai/opencode) is available, a server is started automatically and the connection status is shown in the status bar.

### CLI flags

```
cortex [OPTIONS]

Options:
  --reset          Delete the local database and start fresh
  --config <PATH>  Path to a custom config file
  -h, --help       Show help
  -V, --version    Show version
```

### Basic workflow

1. **Create a task** — Press `n` to open the task editor. Enter a title and optional description, then press `Ctrl+S` to save.
2. **Move tasks** — Press `m` to move the selected task to the next column (e.g. Todo → Plan). If the column has an agent assigned, the agent starts automatically.
3. **View output** — Press `v` to open the task detail view and watch the agent's streaming output in real time.
4. **Auto-progression** — When an agent completes and the column has `auto_progress_to` configured, the task moves forward automatically.

## Configuration

Cortex looks for a config file at `$XDG_CONFIG_HOME/cortex/cortex.toml` (usually `~/.config/cortex/cortex.toml`). If no config file exists, built-in defaults are used.

A fully annotated example configuration is available at [`examples/cortex.toml`](examples/cortex.toml).

### Key config sections

```toml
[opencode]
hostname = "127.0.0.1"
port = 11643

[opencode.model]
id = "glm-5-turbo"
# provider = "openai"
# api_key_env = "OPENAI_API_KEY"

[opencode.agents.planning]
model = "glm-5-turbo"
instructions = "You are a planning assistant."
tools = ["read", "glob", "grep"]
max_turns = 20

[opencode.agents.do]
model = "glm-5-turbo"
instructions = "You are a coding assistant."
tools = ["read", "write", "bash", "glob", "grep"]
max_turns = 50

[columns]
[[columns.definitions]]
id = "todo"
display_name = "Todo"
visible = true

[[columns.definitions]]
id = "planning"
display_name = "Plan"
visible = true
agent = "planning"
auto_progress_to = "running"

[theme]
sidebar_width = 20
column_width = 30
status_working = "#2196F3"
status_done = "#4CAF50"

[log]
level = "info"  # overridden by RUST_LOG env var
```

### Data directory

Persisted data (SQLite database, logs) is stored under `$XDG_DATA_HOME/cortex/` (usually `~/.local/share/cortex/`).

## Keybindings

### Normal mode (configurable)

| Key | Action |
|---|---|
| `h` / `←` | Move focus left (column) |
| `l` / `→` | Move focus right (column) |
| `k` / `↑` | Move focus up (task) |
| `j` / `↓` | Move focus down (task) |
| `n` | Create new task |
| `e` | Edit selected task |
| `m` | Move task forward (next column) |
| `Shift+M` | Move task backward (previous column) |
| `x` | Delete selected task |
| `v` | Open task detail view |
| `?` | Toggle help overlay |
| `Ctrl+J` | Next project |
| `Ctrl+K` | Previous project |
| `Ctrl+N` | New project |
| `Ctrl+A` `A` | Abort active session |
| `R` | Rename active project |
| `D` | Set working directory |
| `Ctrl+Q` | Quit |

### Task editor (fixed)

| Key | Action |
|---|---|
| `Tab` | Cycle field focus (Title ↔ Description) |
| `Enter` | Next field (title) / Newline (description) |
| `Ctrl+S` | Save task |
| `Esc` | Cancel and discard |
| `←` `→` `↑` `↓` | Move cursor |
| `Home` / `End` | Line start / end |
| `PgUp` / `PgDn` | Scroll description |
| `Backspace` | Delete character before cursor |
| `Delete` | Delete character at cursor |

### Task detail view

| Key | Action |
|---|---|
| `Esc` | Return to kanban board |

## Architecture

Cortex is built in Rust and uses a layered architecture:

```
┌─────────────────────────────────────────────┐
│                  TUI Layer                  │
│  ratatui + crossterm                        │
│  ┌─────────┬──────────┬──────────────────┐  │
│  │ Sidebar │  Kanban  │  Task Detail/Ed  │  │
│  └─────────┴──────────┴──────────────────┘  │
├─────────────────────────────────────────────┤
│               State Layer                   │
│  AppState (Mutex) — single source of truth  │
├─────────────────────────────────────────────┤
│           OpenCode Integration              │
│  Client ──► ServerManager ──► OpenCode      │
│  SSE Event Loop ◄── Server (streaming)      │
├─────────────────────────────────────────────┤
│            Persistence Layer                │
│  SQLite (rusqlite) — periodic + shutdown    │
└─────────────────────────────────────────────┘
```

### Key components

- **`src/tui/`** — Terminal UI rendering (ratatui), key handling, and mode management (Normal, TaskEditor, Help)
- **`src/opencode/`** — OpenCode SDK client, server lifecycle management, and SSE event streaming
- **`src/persistence/`** — SQLite-backed state persistence with periodic auto-save
- **`src/state/`** — Core domain types (`AppState`, `CortexTask`, `CortexProject`) and mutation helpers
- **`src/config/`** — TOML configuration loading, validation, and type definitions

### Event flow

1. OpenCode server emits SSE events (session updates, tool calls, messages)
2. The SSE event loop receives events and mutates `AppState` under the mutex
3. The dirty flag is set, triggering periodic persistence to SQLite
4. The TUI render loop reads `AppState` every ~100ms and redraws the interface

## Contributing

Contributions are welcome! Here's how to get started:

1. **Fork** the repository
2. **Create a branch**: `git checkout -b feature/my-feature`
3. **Build and test**: `cargo test`
4. **Format and lint**: `cargo fmt && cargo clippy`
5. **Commit** with a conventional commit message (e.g. `feat: add scroll indicators`)
6. **Open a pull request**

### Development tips

- Logs are written to `$XDG_DATA_HOME/cortex/logs/cortex.log`
- Use `RUST_LOG=debug cortex` for verbose logging
- Use `cortex --reset` to clear all persisted state during development
- An example config with all options documented is at `examples/cortex.toml`
- Always commit `Cargo.lock` — Cortex is a binary application, and the lock file ensures reproducible builds

## License

See [LICENSE](LICENSE) for details.
