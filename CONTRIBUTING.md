# Contributing to Cortex

Thank you for your interest in contributing to Cortex! This guide covers the development workflow, coding conventions, and PR process.

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.70 or later
- [just](https://github.com/casey/just) (optional, but recommended — `cargo install just`)
- [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit) (optional — `cargo install cargo-audit`)
- [cargo-watch](https://github.com/passcod/cargo-watch) (optional — `cargo install cargo-watch`)

## Development Setup

1. **Clone the repository:**
   ```sh
   git clone https://github.com/opencode-ai/cortex.git
   cd cortex
   ```

2. **Build and run:**
   ```sh
   cargo run
   ```

3. **Run in release mode** (faster startup, better performance):
   ```sh
   cargo run --release
   ```

On first launch, Cortex creates a default project and a SQLite database under `$XDG_DATA_HOME/cortex/`. Use `cortex --reset` during development to clear persisted state and start fresh.

## Development Commands

The [justfile](./justfile) provides convenient shortcuts:

| Command | Description |
|---------|-------------|
| `just check` | Run `cargo check` |
| `just test` | Run all tests |
| `just clippy` | Run clippy with warnings as errors |
| `just fmt` | Format code in-place |
| `just fmt-check` | Check formatting without modifying files |
| `just ci` | Run full CI pipeline locally (fmt-check + clippy + test) |
| `just audit` | Audit dependencies for known vulnerabilities |
| `just docs` | Build and open documentation |
| `just watch` | Auto-check + test on file changes |

## Testing

- **Unit tests:** `cargo test`
- **Integration tests:** `cargo test --test config_pipeline`
- **Benchmarks:** `cargo bench`
- **Full CI locally:** `just ci`

Tests live as inline `#[cfg(test)]` modules within source files. Integration tests that exercise cross-module behavior are in the `tests/` directory.

### Debug logging

```sh
RUST_LOG=debug cargo run
```

Logs are written to `$XDG_DATA_HOME/cortex/logs/cortex.log`.

## Code Style

### Formatting

Cortex uses `rustfmt` with a [`rustfmt.toml`](./rustfmt.toml) configuration. Always format before committing:

```sh
cargo fmt
```

### Linting

Clippy is configured via [`clippy.toml`](./clippy.toml) with a cognitive complexity threshold of 30. All warnings are treated as errors:

```sh
cargo clippy -- -D warnings
```

### Conventions

- **Error handling:** Use `anyhow::Result` for fallible operations. The `AppError` type in `persistence/db.rs` is the only exception (type-safe DB errors at the persistence boundary).
- **Mutex usage:** Always use `unwrap_or_else(|e| e.into_inner())` for poison recovery — never `.unwrap()` on a mutex.
- **State mutations:** All `AppState` mutations should go through methods on `AppState` (in `state/store.rs`) to keep invariants consistent.
- **Dirty tracking:** Task mutations must call `mark_task_dirty()` to ensure persistence writes only changed tasks.
- **Lock ordering:** When acquiring multiple locks, always lock `AppState` before any other lockable resource.
- **Serde:** Prefer `#[serde(rename)]` over manual JSON construction for config serialization.

## PR Checklist

Before submitting a pull request, ensure:

- [ ] `cargo fmt --check` passes (code is formatted)
- [ ] `cargo clippy --all-targets -- -D warnings` passes (no lint warnings)
- [ ] `cargo test --all-targets` passes (all tests pass)
- [ ] New public types and functions have doc comments
- [ ] `cargo doc --no-deps` builds without warnings (no broken doc links)
- [ ] `Cargo.lock` is committed (Cortex is a binary application — lock file ensures reproducible builds)
- [ ] Commit messages follow [conventional commits](https://www.conventionalcommits.org/) (e.g., `feat: add scroll indicators`)

## Architecture Overview

```
src/
├── config/       # TOML config loading, validation, types
├── opencode/     # OpenCode SDK client, server management, SSE events
├── orchestration/ # Agent dispatch and auto-progression logic
├── persistence/  # SQLite-backed state persistence
├── state/        # Core domain types (AppState, CortexTask, CortexProject)
└── tui/          # Terminal UI rendering, key handling, mode management
```

See [ARCHITECTURE.md](./ARCHITECTURE.md) for detailed design documentation.

## Getting Help

- Open a [GitHub Issue](https://github.com/opencode-ai/cortex/issues) for bugs or feature requests.
- Check the example config at [`examples/cortex.toml`](./examples/cortex.toml) for all available options.
- Use `?` in the running application to see the keybinding reference.
