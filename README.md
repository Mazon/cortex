# Cortex

A terminal Kanban board that assigns AI agents to your tasks. Built in Rust, integrates with [OpenCode](https://github.com/opencode-ai/opencode).

```
┌──────────┬────────────┬────────────┬────────────┬────────────┐
│ Projects │   Todo     │   Plan     │   Run      │   Review   │
│          │            │            │            │            │
│ ● Default│ 1. Add ... │ 3. Refac.. │ 5. Fix ..  │            │
│          │            │   ◐ work.. │   ◐ work.. │            │
│          │ 2. Write.. │            │            │            │
├──────────┴────────────┴────────────┴────────────┴────────────┤
│ ● connected  │  Default  │  5 tasks  │  ? for help          │
└──────────────────────────────────────────────────────────────┘
```

## Install

Requires [Rust](https://www.rust-lang.org/tools/install) 1.80+.

```sh
git clone https://github.com/opencode-ai/cortex.git
cd cortex
cargo install --path .
```

Then run `cortex` in any project directory.

## How it works

1. **Create a task** — press `n`, write a title, save with `Ctrl+S`
2. **Move it to a column** — press `m` to advance (e.g. Todo → Plan)
3. **Agent runs automatically** — when a task enters a column with an assigned agent, it starts working
4. **Watch the output** — press `v` to stream agent results in real time
5. **Tasks auto-progress** — when an agent finishes, the task moves to the next column

## Features

- **Kanban board** — column-based task management, entirely in the terminal
- **AI agent integration** — assign planning, coding, or review agents to columns
- **Real-time streaming** — watch agent output as it happens via SSE
- **Multi-project** — separate working directories, each with its own OpenCode server
- **Configurable workflow** — custom columns, agents, and auto-progression rules
- **Vim-style keybindings** — fully remappable via config
- **Persistent state** — SQLite-backed, survives restarts

## Configuration

Config lives at `$XDG_CONFIG_HOME/cortex/cortex.toml` (`~/.config/cortex/cortex.toml`). If it doesn't exist, sensible defaults are used.

A fully annotated example is at [`examples/cortex.toml`](examples/cortex.toml).

### Quick config example

```toml
[opencode.model]
id = "glm-5-turbo"

[opencode.agents.do]
model = "glm-5-turbo"
instructions = "You are a coding assistant."
tools = ["read", "write", "bash", "glob", "grep"]
max_turns = 50

[[columns.definitions]]
id = "running"
display_name = "Run"
agent = "do"
auto_progress_to = "review"
```

## Key shortcuts

| Key | Action |
|-----|--------|
| `n` | New task |
| `e` | Edit task |
| `m` / `Shift+M` | Move forward / backward |
| `v` | View task detail & stream |
| `x` | Delete task |
| `h` `j` `k` `l` | Navigate |
| `?` | Help overlay |
| `Ctrl+Q` | Quit |

Full keybinding reference is available in the app with `?`. All bindings are remappable — see the [example config](examples/cortex.toml).

## CLI

```sh
cortex                  # Launch the board
cortex --reset          # Wipe database and start fresh
cortex --config <PATH>  # Use a custom config file
```

## Learn more

- [Architecture](ARCHITECTURE.md) — module layout, event flow, state management
- [Contributing](CONTRIBUTING.md) — development setup, code style, PR process

## License

MIT
