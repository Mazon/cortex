# Plan: Fix Tracing Log Leak — Root Cause Fix

## Purpose

Startup log lines are leaking into the TUI because the file-based tracing pipeline silently fails: `create_dir_all` errors are swallowed with `.ok()`, `LogTracer::init()` errors are swallowed, `#[tokio::main]` creates a race condition, and there is no stderr redirect during TUI operation. The user confirmed **no log file is created** at `~/.local/share/cortex/logs/`. This plan fixes the root cause and secondary issues.

## Dependency Graph

```mermaid
graph TD
    W1A[W1A: Fix create_dir_all + verify log file] --> W2A[W2A: Replace #[tokio::main] with manual runtime]
    W1B[W1B: Fix LogTracer::init error handling] --> W2A
    W1C[W1C: Rename _guard + remove duplicate shutdown] --> W2A
    W2A --> W2B[W2B: Redirect stderr to log file + panic hook]
    W2A --> W2C[W2C: Fix child process pipe deadlock]
    W2A --> W2D[W2D: Abort SSE handles during shutdown]
```

## Progress

### Wave 1 — Fix the core logging pipeline (root cause)
- [x] **W1A:** Fix `create_dir_all` — replace `.ok()` with `?`, and verify the log file is writable by writing a test entry immediately after creating the appender
- [x] **W1B:** Fix `LogTracer::init()` — replace `.ok()` with proper error handling (log a warning if it fails, but don't crash — use `if let Err`)
- [x] **W1C:** Rename `_guard` to `_log_flush_guard` with a doc comment about its lifetime; remove the duplicate "Force-save state before exit" block (lines 224–235 in main.rs are an exact copy of lines 211–222)

### Wave 2 — Fix secondary issues (init order, stderr, pipes, shutdown)
- [x] **W2A:** Replace `#[tokio::main]` with a manual `tokio::runtime::Builder::new_multi_thread().enable_all().build()?` so that tracing init, log dir creation, and stderr redirect all happen **before** the async runtime is created. Move the runtime creation to just before the first `.await` (the server startup loop). Also move `App::setup_terminal()` to after tracing init but before runtime creation.
- [x] **W2B:** Redirect stderr to the log file during TUI operation — use `std::fs::File` + `unsafe { libc::dup2() }` on the stderr fd after log dir is confirmed writable, before entering alternate screen. Also install a custom `std::panic::set_hook()` that writes to the log file instead of stderr. Note: add `libc` to `Cargo.toml` as a dependency (or use a portable `std::fs::File` approach via `dup2` from the `libc` crate, gated behind `#[cfg(unix)]`). Fallback for non-unix: just ensure tracing captures panics.
- [x] **W2C:** Fix child process pipe deadlock in `server.rs` — the `Stdio::piped()` for stdout/stderr creates pipes that are never read, which can deadlock when the pipe buffer fills. Change to `Stdio::null()` for stdout, and for stderr either `Stdio::null()` or spawn a drain task that writes stderr to the log via `tracing::warn!`.
- [x] **W2D:** Abort SSE handles during shutdown — before `server_manager.stop_all()`, abort all `sse_handles` with `.abort()`, then `await` them (or just abort and drop).

## Detailed Specifications

### W1A: Fix create_dir_all + verify log file

**File:** `src/main.rs` (lines 54–62)

Current code:
```rust
let log_dir = config::dirs_or_home()
    .join(".local")
    .join("share")
    .join("cortex")
    .join("logs");
std::fs::create_dir_all(&log_dir).ok();

let file_appender = tracing_appender::rolling::never(&log_dir, "cortex.log");
let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
```

Replace with:
```rust
let log_dir = config::dirs_or_home()
    .join(".local")
    .join("share")
    .join("cortex")
    .join("logs");
std::fs::create_dir_all(&log_dir)
    .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

let file_appender = tracing_appender::rolling::never(&log_dir, "cortex.log");
let (non_blocking, _log_flush_guard) = tracing_appender::non_blocking(file_appender);
```

Also add a verification write after tracing is initialized:
```rust
tracing::info!("Logger initialized — writing to {}/cortex.log", log_dir.display());
```
This serves as both a confirmation message and a functional test that the appender works. If this line doesn't appear in the log file, the pipeline is broken.

### W1B: Fix LogTracer::init

**File:** `src/main.rs` (line 79)

Replace:
```rust
tracing_log::LogTracer::init().ok();
```

With:
```rust
if let Err(e) = tracing_log::LogTracer::init() {
    // Already initialized by another subscriber — not fatal, but log it
    eprintln!("Warning: LogTracer bridge failed to initialize: {}", e);
}
```

Note: `eprintln!` is intentional here — if tracing isn't working, this goes to stderr. After W2B is done, stderr goes to the log file anyway.

### W1C: Rename guard + remove duplicate

**File:** `src/main.rs`

1. Rename `_guard` to `_log_flush_guard` everywhere (line 62 and any other reference).
2. Add a comment: `// IMPORTANT: This guard must live until program exit. When dropped, it flushes all buffered log writes to disk.`
3. Remove the duplicate "Force-save state before exit" block at lines 224–235 (keep the one at 211–222).

### W2A: Replace #[tokio::main] with manual runtime

**File:** `src/main.rs`

Replace:
```rust
#[tokio::main]
async fn main() -> Result<()> {
    // ... everything ...
}
```

With:
```rust
fn main() -> Result<()> {
    // === Phase 1: Synchronous initialization (before runtime) ===
    let cli = Cli::parse();
    let config = ...;
    
    // Create log dir, init tracing, init LogTracer, redirect stderr
    // (all synchronous, no runtime needed)
    let log_dir = ...;
    std::fs::create_dir_all(&log_dir)?;
    // ... tracing init ...
    // ... stderr redirect ...
    
    // Enter alternate screen BEFORE runtime creation
    App::setup_terminal()?;
    
    // === Phase 2: Async runtime ===
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create Tokio runtime")?;
    
    runtime.block_on(async {
        // ... all the async code (server startup, TUI loop, shutdown) ...
    })
}
```

Key points:
- Tracing is fully initialized before any `.await` point
- `App::setup_terminal()` is called before runtime, ensuring the alternate screen hides any stderr that might slip through
- The `_log_flush_guard` must live inside `runtime.block_on()` or be returned and held in the outer scope

### W2B: Redirect stderr to log file + panic hook

**File:** `src/main.rs` (after tracing init, before `setup_terminal`)

```rust
// Redirect stderr to the log file so nothing leaks to the TUI
#[cfg(unix)]
{
    let log_file_path = log_dir.join("cortex-stderr.log");
    let stderr_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to open stderr log: {}", log_file_path.display()))?;
    let fd = stderr_log.as_raw_fd();
    unsafe {
        libc::dup2(fd, 2); // fd 2 = stderr
    }
    // stderr_log handle can be dropped — dup2 duplicates the fd
}
```

For the panic hook:
```rust
let panic_log_dir = log_dir.clone();
std::panic::set_hook(Box::new(move |info| {
    let msg = format!("PANIC: {:?}", info);
    // Try to write to the log file
    let log_file = panic_log_dir.join("cortex.log");
    if let Ok(mut f = std::fs::OpenOptions::new().append(true).open(&log_file) {
        use std::io::Write;
        let _ = writeln!(f, "{}", msg);
    }
}));
```

**File:** `Cargo.toml` — add:
```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

### W2C: Fix child process pipe deadlock

**File:** `src/opencode/server.rs` (lines 84–93)

The current code uses `Stdio::piped()` for both stdout and stderr, but never reads from those pipes. Once the OS pipe buffer fills (~64KB on Linux), the child process blocks on write, causing a deadlock during health checks.

Option A (simplest, recommended): Use `Stdio::null()`:
```rust
let child = Command::new("opencode")
    .arg("serve")
    .arg(format!("--hostname={}", host))
    .arg(format!("--port={}", port))
    .env("OPENCODE_CONFIG_CONTENT", config_json)
    .current_dir(working_dir)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .context("Failed to spawn 'opencode serve'. Is it installed?")?;
```

This discards all child output. Since we have health checks and log files, this is fine.

Option B (if child stderr is useful for debugging): Use `Stdio::piped()` for stderr only, and spawn a drain task:
```rust
.stderr(std::process::Stdio::piped())
// Then after spawn:
if let Some(stderr) = child.stderr.take() {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(target: "opencode::child", "{}", line);
        }
    });
}
```

**Recommendation:** Use Option A (`Stdio::null()`) for now. It's simpler and eliminates the deadlock completely. If child output is needed later, Option B can be added.

### W2D: Abort SSE handles during shutdown

**File:** `src/main.rs` (in the shutdown section, before `server_manager.stop_all()`)

Add:
```rust
// Abort SSE event loops
for handle in sse_handles {
    handle.abort();
}
// Give them a moment to clean up
tokio::time::sleep(std::time::Duration::from_millis(100)).await;
```

## Surprises & Discoveries

1. The `create_dir_all` at line 59 calls `.ok()` while line 50 uses `?` — inconsistent error handling in the same function.
2. The duplicate shutdown block (lines 211–222 and 224–235) is an exact copy-paste. No logical difference between them.
3. `_guard` from `tracing_appender::non_blocking` is a `WorkerGuard` — when it drops, it flushes all buffered writes. If it drops too early, log lines are lost. The rename and comment make this critical lifetime visible.
4. `tracing_appender::rolling::never()` creates the file lazily on first write — so even if `create_dir_all` succeeds, the file won't exist until the first `tracing::info!()` call. The user may have checked for the file before any trace event was emitted.
5. The `setup_terminal()` call at line 85 is already in the right position (after tracing init). The real fix is ensuring the runtime doesn't exist yet at that point (W2A).

## Decision Log

| Decision | Rationale |
|----------|-----------|
| Use `?` for `create_dir_all` instead of graceful fallback | If we can't write logs, the TUI leak problem can't be fixed. Better to fail fast with a clear error. |
| Use `Stdio::null()` for child processes (Option A) | Simplest fix that eliminates the deadlock. Child output isn't critical — health checks tell us if the server is up. |
| Add `libc` as a Unix-only dependency | `dup2` is the most reliable way to redirect stderr at the fd level. The `#[cfg(unix)]` gate ensures Windows compatibility isn't broken (it just won't redirect on Windows). |
| Manual runtime instead of `#[tokio::main]` | This gives us complete control over initialization order. The `#[tokio::main]` macro hides the runtime creation, making it impossible to run synchronous init code "before" the runtime. |
| Keep `LogTracer` failure as warning, not fatal | If the bridge fails, it means another logger is already installed (e.g., by a test framework). Not worth crashing over. |

## Outcomes & Retrospective

**Status: ✅ All 7 tasks completed successfully.**

### Commits
1. `973f1a2` — `fix: fix tracing pipeline — handle errors, rename guard, remove duplicate shutdown` (Wave 1: W1A, W1B, W1C)
2. `04a1bdf` — `fix: redirect stderr, manual runtime, fix pipe deadlock, abort SSE handles` (Wave 2: W2A, W2B, W2C, W2D)

### Files Modified
- `src/main.rs` — W1A (error handling), W1B (LogTracer), W1C (guard rename + dedup), W2A (manual runtime), W2B (stderr redirect + panic hook), W2D (SSE abort)
- `src/opencode/server.rs` — W2C (Stdio::null())
- `Cargo.toml` — W2B (libc dependency)

### Key Changes
- `create_dir_all` now fails with context instead of silently swallowing errors
- `LogTracer::init()` now logs a warning on failure instead of `.ok()`
- `_log_flush_guard` renamed with doc comment about critical lifetime
- Duplicate shutdown block removed
- `#[tokio::main]` replaced with manual runtime builder — tracing init, stderr redirect, and terminal setup all happen synchronously before runtime creation
- stderr redirected to `~/.local/share/cortex/logs/cortex-stderr.log` via `libc::dup2` (Unix only)
- Custom panic hook writes to cortex.log instead of stderr
- Child process stdout/stderr changed from `Stdio::piped()` to `Stdio::null()` to prevent pipe deadlock
- SSE handles aborted during shutdown before server stop

### Verification
- `cargo check` passes after both waves (no new warnings)
