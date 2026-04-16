# Cortex Architecture

Cortex is a terminal-based Kanban board application that integrates with [OpenCode](https://opencode.ai) to provide AI-assisted task management. It is written in Rust using [ratatui](https://ratatui.rs/) for the TUI layer and [Tokio](https://tokio.rs/) for async runtime.

## Module Structure

```
src/
├── main.rs              # Entry point: CLI parsing, config loading, server startup, TUI loop
├── error.rs             # Application error types (AppError enum + AppResult alias)
├── config/
│   ├── mod.rs           # Config loading, saving, validation (XDG-compliant paths)
│   ├── types.rs         # Config structs: CortexConfig, ColumnsConfig, OpenCodeConfig, etc.
│   └── defaults.rs      # Default configuration values
├── state/
│   ├── mod.rs           # Re-exports types and store modules
│   ├── types.rs         # Core domain types: AppState, CortexTask, CortexProject, enums
│   └── store.rs         # Mutation methods on AppState (CRUD, navigation, SSE processing)
├── persistence/
│   ├── mod.rs           # save_state / restore_state orchestration
│   └── db.rs            # SQLite database operations (schema, migrations, queries)
├── opencode/
│   ├── mod.rs           # Re-exports client, server, events
│   ├── client.rs        # Thin wrapper around opencode-sdk-rs (session CRUD, type conversions)
│   ├── server.rs        # Per-project OpenCode server process management (spawn, health-check)
│   └── events.rs        # SSE event loop — subscribes to events, dispatches to state
├── orchestration/
│   └── engine.rs        # Multi-agent workflow coordination
└── tui/
    ├── mod.rs           # Layout composition: sidebar + kanban/status bar
    ├── app.rs           # App struct, event loop, key routing, render scheduling
    ├── kanban.rs        # Kanban board widget (columns + task cards)
    ├── task_card.rs     # Individual task card rendering
    ├── task_detail.rs   # Task detail panel (messages, tool calls, streaming output)
    ├── task_editor.rs   # Fullscreen task editor widget
    ├── sidebar.rs       # Project list sidebar
    ├── status_bar.rs    # Bottom status bar
    ├── help.rs          # Help overlay (keybindings reference)
    ├── keys.rs          # Key event parsing and keybinding config
    ├── editor_handler.rs # Key handler for task editor mode
    └── normal_mode.rs   # Key handler for normal mode (navigation, actions)
```

## SSE Event Flow

The core data pipeline follows a unidirectional flow:

```
OpenCode Server
     │
     │  HTTP/SSE (text/event-stream)
     ▼
 opencode::events::sse_event_loop()
     │
     │  EventListResponse variants:
     │    SessionStatus, SessionIdle, SessionError,
     │    MessagePartDelta, PermissionAsked, PermissionReplied,
     │    QuestionAsked, QuestionReplied, ...
     ▼
 process_event() → state mutation
     │
     │  AppState methods:
     │    process_session_status()
     │    process_session_idle()
     │    process_message_part_delta()
     │    process_permission_asked()
     │    ...
     ▼
 AppState (Arc<Mutex<AppState>>)
     │
     │  mark_render_dirty() → atomic flag
     ▼
 TUI event loop (tui::app::run)
     │
     │  Every 100ms tick:
     │    1. take_render_dirty() — check if state changed
     │    2. Clone state snapshot
     │    3. terminal.draw() — render widgets
     ▼
 Terminal (ratatui + crossterm)
```

### Event Types and State Effects

| SSE Event | State Effect |
|-----------|-------------|
| `SessionStatus` | Updates task's `agent_status` (Running, Complete) |
| `SessionIdle` | Marks task complete, shows success notification |
| `SessionError` | Records error message on task, sets status to Error |
| `MessagePartDelta` | Appends text to `streaming_text` buffer |
| `PermissionAsked` | Creates pending permission request; auto-approves safe tools |
| `PermissionReplied` | Removes resolved permission from pending list |
| `QuestionAsked` | Shows warning notification with question preview |

### Auto-Approval of Safe Tools

When a `PermissionAsked` event arrives for a read-only tool (`read`, `glob`, `grep`, `list`), the permission is automatically approved via a fire-and-forget `tokio::spawn` call. Destructive tools (`write`, `bash`, etc.) always require explicit user approval.

## State Management

### AppState

`AppState` is the single source of truth, wrapped in `Arc<Mutex<AppState>>` and shared across:

- **TUI event loop** — reads for rendering, writes for user interactions
- **SSE event loop** — writes for incoming events (one per project)
- **Persistence task** — reads periodically to save dirty state

### Lock Ordering Convention

To prevent deadlocks, the following lock ordering must be maintained:

1. **AppState first** (`state.lock()`) — always acquire the state lock before any other lock
2. **Db second** — if database access is needed while holding the state lock, the `Db` connection is used last (SQLite connections are not wrapped in a Mutex, so this is mostly relevant conceptually for ensuring no lock inversion)

The persistence task opens a fresh `Db` connection each cycle rather than sharing one, avoiding contention entirely.

### Dirty Flags

Two atomic dirty flags drive efficient updates:

- **`dirty`** — set when any domain state changes (tasks, projects, kanban). The periodic persistence task checks this flag and saves to SQLite if set.
- **`render_dirty`** — set when any state change should trigger a UI re-render. The TUI event loop checks this flag and skips rendering when nothing has changed.

## Persistence

State is persisted to SQLite at `$XDG_DATA_HOME/cortex/cortex.db`:

- **Projects** — stored with id, name, working_directory, status, position
- **Tasks** — stored with all fields including session_id, agent_type, timestamps
- **Kanban order** — column_id → ordered list of task IDs
- **Metadata** — active_project_id, per-project task number counters

A periodic task (every 5 seconds) checks the dirty flag and saves if needed. State is also force-saved on graceful shutdown.

## Server Management

Each project gets its own OpenCode server process:

1. `ServerManager::start_for_project()` assigns a unique port (base_port + counter)
2. Spawns `opencode serve` with project-specific config via `OPENCODE_CONFIG_CONTENT` env var
3. Polls the health endpoint (`GET /app`) until the server is ready
4. Retries up to 3 times with 1-second delays

On shutdown, all server processes are killed gracefully (SIGTERM with 5-second timeout).

## Configuration

Configuration is loaded from `$XDG_CONFIG_HOME/cortex/cortex.toml`. If no config file exists, a default one is generated automatically.

Key configuration sections:
- **`[opencode]`** — server settings (port, model, agents, MCP servers, API keys)
- **`[columns]`** — kanban column definitions (id, display_name, visible, agent, auto_progress_to)
- **`[keybindings]`** — custom key mappings
- **`[theme]`** — visual settings (sidebar width, colors)
- **`[log]`** — logging level

See `examples/cortex.toml` for a fully annotated example configuration.
