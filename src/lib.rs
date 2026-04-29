pub mod config;
pub mod error;
pub mod opencode;
pub mod orchestration;
pub mod persistence;
pub mod state;
pub mod tui;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use futures::stream::{FuturesUnordered, StreamExt};
use persistence::db::Db;
use state::types::{AgentStatus, AppState};
use tracing_subscriber::prelude::*;
use tui::app::App;

/// cortex — TUI Kanban board with OpenCode SDK integration
#[derive(Parser, Debug)]
#[command(
    name = "cortex",
    version,
    about = "TUI Kanban board with OpenCode SDK integration"
)]
struct Cli {
    /// Reset all persisted state (delete database)
    #[arg(long)]
    reset: bool,

    /// Path to config file
    #[arg(long)]
    config: Option<String>,
}

/// Main entry point for the Cortex application.
///
/// Parses CLI arguments, loads configuration, initializes the TUI,
/// and runs the async event loop.
pub fn run() -> Result<()> {
    // === Phase 1: Synchronous initialization (before runtime) ===

    // Parse CLI args
    let cli = Cli::parse();

    // Load config (must happen before logger init so we can read config.log.level)
    let config_path = cli
        .config
        .as_ref()
        .map(|p| std::path::PathBuf::from(p))
        .unwrap_or_else(config::default_config_path);

    let config = config::load_config(&config_path)?;

    // Initialize tracing — writes structured logs to a file (not stderr,
    // which would corrupt the TUI), plus routes Warn/Error events to the
    // TUI notification bar once AppState exists.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log.level));

    // ─── Log file for structured tracing output ─────────────────────
    // The fmt layer MUST NOT write to stderr/stdout in a TUI application.
    // crossterm's alternate screen only captures stdout; stderr writes
    // to the primary screen buffer and corrupts the TUI display.
    // Instead, write structured logs to $XDG_DATA_HOME/cortex/cortex.log.
    let log_path = config::xdg_data_home().join("cortex").join("cortex.log");
    let log_file: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
        std::fs::create_dir_all(log_path.parent().unwrap_or_else(|| std::path::Path::new(".")))
            .ok()
            .and_then(|_| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .ok()
                    .map(|f| std::sync::Arc::new(std::sync::Mutex::new(f)))
            });

    if let Some(ref _lf) = log_file {
        tracing::debug!("Log file: {}", log_path.display());
    }

    let tui_layer = tui::tracing_layer::TuiNotificationLayer::new();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(move || {
            match &log_file {
                Some(file_arc) => {
                    struct MutexFileWriter(std::sync::Arc<std::sync::Mutex<std::fs::File>>);
                    impl std::io::Write for MutexFileWriter {
                        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                            let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
                            guard.write(buf)
                        }
                        fn flush(&mut self) -> std::io::Result<()> {
                            let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
                            guard.flush()
                        }
                    }
                    Box::new(MutexFileWriter(std::sync::Arc::clone(file_arc)))
                        as Box<dyn std::io::Write>
                }
                None => Box::new(std::io::sink()) as Box<dyn std::io::Write>,
            }
        });

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(tui_layer.clone())
        .init();

    // Enter alternate screen early to hide any residual startup output
    App::setup_terminal()?;

    // Guard: restore terminal if anything fails before the main loop's teardown runs.
    // Both disable_raw_mode and LeaveAlternateScreen are idempotent, so the double-cleanup
    // on the happy path (teardown + guard Drop) is safe.
    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = crossterm::event::DisableMouseCapture;
            let _ = crossterm::terminal::disable_raw_mode();
            let _ =
                crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        }
    }
    let _terminal_guard = TerminalGuard;

    // Render initial loading screen so users see feedback immediately
    // instead of a blank alternate screen while servers start up.
    let mut loading_terminal = tui::Terminal::new(tui::CrosstermBackend::new(std::io::stdout()))?;
    let mut spinner_idx: usize = 0;
    tui::loading::render_loading_frame(&mut loading_terminal, "Starting Cortex...", spinner_idx)?;

    // === Phase 2: Async runtime ===
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create Tokio runtime")?;

    let result = runtime.block_on(async {
        // Handle --reset flag
        let db_path = persistence::db::default_db_path();
        if cli.reset {
            if db_path.exists() {
                std::fs::remove_file(&db_path)?;
            }
        }

        // Open database
        let db = Db::new(&db_path)?;

        // Create app state
        let state = Arc::new(Mutex::new(AppState::default()));

        // Wire the tracing layer to the app state so Warn/Error events
        // are automatically pushed to the notification bar.
        tui_layer.set_state(&state);

        // === Lock ordering convention ===
        //
        // To prevent deadlocks, always acquire locks in this order:
        //
        //   1. AppState (`state.lock()`) — the primary application state mutex.
        //   2. Db — SQLite connections are opened fresh per operation (not wrapped
        //      in a Mutex), so there is no second lock to contend with. However,
        //      if a Db were ever shared behind a Mutex, it must always be locked
        //      *after* AppState.
        //
        // Never hold AppState while awaiting an async operation that may
        // internally need the lock (e.g., SSE event loop dispatches lock
        // state briefly per event, never across await points).

        // Restore persisted state
        {
            let mut state = state.lock().unwrap_or_else(|e| {
                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                e.into_inner()
            });
            if let Err(e) = persistence::restore_state(&mut state, &db) {
                tracing::error!("Failed to restore persisted state: {}", e);
            }
        }

        // If no projects exist, create a default one
        {
            let mut state = state.lock().unwrap_or_else(|e| {
                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                e.into_inner()
            });
            if state.project_registry.projects.is_empty() {
                let id = uuid::Uuid::new_v4().to_string();
                let project = state::types::CortexProject {
                    id: id.clone(),
                    name: "Default".to_string(),
                    working_directory: std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string()),
                    status: state::types::ProjectStatus::Idle,
                    position: 0,
                    ..Default::default()
                };
                state.add_project(project);
                state.select_project(&id);
            }
        }

        // Create shared OpenCode client and server (single process for all projects)
        let mut opencode_clients: HashMap<String, opencode::client::OpenCodeClient> =
            HashMap::new();
        let mut server_manager = opencode::server::ServerManager::new();

        // Start a single shared server using the first project's working directory.
        let projects_snapshot: Vec<(String, String, String)> = state
            .lock()
            .unwrap()
            .project_registry
            .projects
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.working_directory.clone()))
            .collect();

        if let Some((_, _, working_dir)) = projects_snapshot.first() {
            spinner_idx = tui::loading::advance_spinner(spinner_idx);
            tui::loading::render_loading_frame(
                &mut loading_terminal,
                "Starting server...",
                spinner_idx,
            )?;

            // Pin the server-start future so we can poll it via tokio::select!
            // while concurrently animating the loading spinner.
            let server_fut = server_manager.start_shared(&config.opencode, working_dir);
            tokio::pin!(server_fut);

            const STARTUP_TIMEOUT: std::time::Duration =
                std::time::Duration::from_secs(15);
            let start_time = std::time::Instant::now();
            let mut spinner_ticker =
                tokio::time::interval(std::time::Duration::from_millis(150));

            let result = loop {
                tokio::select! {
                    result = &mut server_fut => {
                        break result;
                    }
                    _ = spinner_ticker.tick() => {
                        spinner_idx = tui::loading::advance_spinner(spinner_idx);
                        let elapsed = start_time.elapsed();
                        let msg = if elapsed < std::time::Duration::from_secs(5) {
                            "Starting server..."
                        } else {
                            "Starting server (still waiting)..."
                        };
                        let _ = tui::loading::render_loading_frame(
                            &mut loading_terminal,
                            msg,
                            spinner_idx,
                        );
                    }
                }

                // Hard timeout — bail out if the server still hasn't started.
                if start_time.elapsed() > STARTUP_TIMEOUT {
                    break Err(anyhow::anyhow!(
                        "Server startup timed out after {}s",
                        STARTUP_TIMEOUT.as_secs()
                    ));
                }
            };

            match result {
                Ok(url) => {
                    // Create a single shared client using config values (timeouts)
                    // but connected to the actual server URL (which may use a
                    // random port picked by `opencode serve`).
                    match opencode::client::OpenCodeClient::from_config_with_url(
                        &config.opencode,
                        &url,
                    ) {
                        Ok(client) => {
                            // Register the same client for every project
                            for (project_id, _, _) in &projects_snapshot {
                                opencode_clients.insert(project_id.clone(), client.clone());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create OpenCode client: {}", e);
                            let mut s = state.lock().unwrap_or_else(|e| {
                                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                                e.into_inner()
                            });
                            s.set_notification(
                                format!("Failed to connect to OpenCode server: {}", e),
                                crate::state::types::NotificationVariant::Error,
                                8000,
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to start OpenCode server: {}", e);
                    let mut s = state.lock().unwrap_or_else(|e| {
                        tracing::error!("AppState mutex poisoned, recovering: {}", e);
                        e.into_inner()
                    });
                    s.set_notification(
                        format!("Failed to start OpenCode server: {}", e),
                        crate::state::types::NotificationVariant::Error,
                        8000,
                    );
                }
            }
        }

        // Show "connected" status after all servers have been started
        spinner_idx = tui::loading::advance_spinner(spinner_idx);
        tui::loading::render_loading_frame(
            &mut loading_terminal,
            "Connected. Loading...",
            spinner_idx,
        )?;

        // Spawn SSE event loops for active clients
        // Create a shared shutdown watch channel for SSE event loops.
        // All loops share the same receiver; sending `true` on the sender
        // causes every loop to break out cleanly.
        let (sse_shutdown_tx, sse_shutdown_rx) = tokio::sync::watch::channel(false);

        let mut sse_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        // Group projects by server URL. All projects sharing the same URL use
        // a single event loop, preventing text duplication from multiple loops
        // processing identical events. Connection state changes (connected,
        // reconnecting, permanently_disconnected) are propagated to every
        // project in the group so the status bar stays consistent.
        let mut url_to_projects: HashMap<String, Vec<String>> = HashMap::new();
        for (project_id, client) in &opencode_clients {
            let url = client.base_url().to_string();
            url_to_projects
                .entry(url)
                .or_default()
                .push(project_id.clone());
        }
        for (_url, project_ids) in &url_to_projects {
            let client = opencode_clients
                .get(project_ids.first().unwrap())
                .unwrap()
                .clone();
            let state = state.clone();
            let pids = project_ids.clone();
            let columns_config = config.columns.clone();
            let opencode_config = config.opencode.clone();
            let shutdown_rx = sse_shutdown_rx.clone();
            let handle = tokio::spawn(async move {
                opencode::events::sse_event_loop(
                    client,
                    state,
                    columns_config,
                    opencode_config,
                    shutdown_rx,
                    pids,
                )
                .await;
            });
            sse_handles.push(handle);
        }

        // Mark all projects as connected since we have at least one client
        if !opencode_clients.is_empty() {
            let project_ids: Vec<String> = state
                .lock()
                .unwrap()
                .project_registry
                .projects
                .iter()
                .map(|p| p.id.clone())
                .collect();
            let mut state = state.lock().unwrap_or_else(|e| {
                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                e.into_inner()
            });
            for pid in &project_ids {
                state.set_project_connected(pid, true);
            }
        }

        // Rehydrate sessions for tasks that were active at shutdown.
        // After restart, `task_sessions` is empty (transient runtime state),
        // so tasks that were Running/Question/Error show no agent output.
        // We fetch their full message history from the OpenCode server to
        // restore the display.
        {
            let rehydrate_tasks: Vec<(String, String)> = {
                let state = state.lock().unwrap_or_else(|e| {
                    tracing::error!("AppState mutex poisoned, recovering: {}", e);
                    e.into_inner()
                });
                state
                    .tasks
                    .iter()
                    .filter(|(_, t)| {
                        matches!(
                            t.agent_status,
                            AgentStatus::Running | AgentStatus::Question | AgentStatus::Error
                        ) && t.session_id.is_some()
                    })
                    .map(|(id, t)| (id.clone(), t.session_id.clone().unwrap()))
                    .collect()
            };

            if !rehydrate_tasks.is_empty() {
                tracing::info!(
                    count = rehydrate_tasks.len(),
                    "Rehydrating sessions for active tasks after restart"
                );

                let client = opencode_clients.values().next().unwrap().clone();
                let mut futures = FuturesUnordered::new();

                for (task_id, session_id) in &rehydrate_tasks {
                    let client = client.clone();
                    let task_id = task_id.clone();
                    let session_id = session_id.clone();
                    futures.push(async move {
                        let result = client.fetch_session_messages(&session_id).await;
                        (task_id, session_id, result)
                    });
                }

                while let Some((task_id, session_id, result)) = futures.next().await {
                    match result {
                        Ok(messages) => {
                            let mut state = state.lock().unwrap_or_else(|e| {
                                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                                e.into_inner()
                            });
                            state.rehydrate_task_session(&task_id, messages.clone());
                            tracing::info!(
                                task_id = %task_id,
                                session_id = %session_id,
                                msg_count = messages.len(),
                                "Rehydrated session for active task"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task_id,
                                session_id = %session_id,
                                error = %e,
                                "Session not found after restart — marking task as Error"
                            );
                            let mut state = state.lock().unwrap_or_else(|e| {
                                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                                e.into_inner()
                            });
                            state.mark_orphaned_running_task(&task_id);
                        }
                    }
                }
            }
        }

        // Drop the loading terminal before creating the App, which
        // constructs its own Terminal wrapping stdout.
        drop(loading_terminal);

        // Spawn periodic persistence save task.
        // Opens a fresh Db connection each cycle (no lock contention with AppState).
        // Lock ordering: AppState → Db (Db is opened after state is read).
        // On repeated DB errors, applies exponential backoff (2s → 4s → 8s → max 30s)
        // instead of retrying every 5 seconds unconditionally.
        let state_for_save = state.clone();
        let db_path_for_save = persistence::db::default_db_path();
        let persistence_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut db_error_backoff_ms: u64 = 0; // 0 = no backoff, use normal interval
            let mut consecutive_db_errors: u32 = 0;

            loop {
                interval.tick().await;

                // If we're in backoff mode, sleep extra before retrying.
                if db_error_backoff_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(db_error_backoff_ms)).await;
                }

                let db = match Db::new(&db_path_for_save) {
                    Ok(db) => db,
                    Err(e) => {
                        consecutive_db_errors += 1;
                        db_error_backoff_ms =
                            (2000u64 * (1 << consecutive_db_errors.min(4))).min(30_000);
                        tracing::error!(
                            "Failed to open DB for save (attempt {}): {}",
                            consecutive_db_errors,
                            e,
                        );
                        continue;
                    }
                };

                let mut state = state_for_save.lock().unwrap_or_else(|e| {
                    tracing::error!("AppState mutex poisoned, recovering: {}", e);
                    e.into_inner()
                });
                if state.take_dirty() {
                    state
                        .dirty_flags
                        .saving_in_progress
                        // Relaxed is sufficient because this flag is only used as a
                        // hint by the persistence loop itself — no other thread
                        // synchronizes on it.
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Err(e) = persistence::save_state(&mut state, &db) {
                        consecutive_db_errors += 1;
                        db_error_backoff_ms =
                            (2000u64 * (1 << consecutive_db_errors.min(4))).min(30_000);
                        tracing::error!(
                            "Failed to save state (attempt {}): {}",
                            consecutive_db_errors,
                            e,
                        );
                    } else {
                        // Success — reset backoff
                        consecutive_db_errors = 0;
                        db_error_backoff_ms = 0;
                    }
                    state
                        .dirty_flags
                        .saving_in_progress
                        // Relaxed is sufficient — see comment on store(true) above.
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });

        // Run TUI event loop
        let mut app = App::new(state.clone(), config.clone(), opencode_clients)?;
        let result = app.run().await;

        // Graceful shutdown

        // Cancel persistence task
        persistence_handle.abort();

        // Abort SSE event loops
        // Signal graceful shutdown first, then fall back to abort after a timeout.
        let _ = sse_shutdown_tx.send(true);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        for handle in sse_handles {
            if !handle.is_finished() {
                handle.abort();
            }
        }

        // Force-save state before exit
        {
            let mut state = state.lock().unwrap_or_else(|e| {
                tracing::error!("AppState mutex poisoned, recovering: {}", e);
                e.into_inner()
            });
            let db_path = persistence::db::default_db_path();
            if let Ok(db) = Db::new(&db_path) {
                state
                    .dirty_flags
                    .saving_in_progress
                    // Relaxed is sufficient — see comment on store(true) in persistence loop.
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                if let Err(e) = persistence::save_state(&mut state, &db) {
                    tracing::error!("Failed to save state on shutdown: {}", e);
                }
                state
                    .dirty_flags
                    .saving_in_progress
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Stop servers
        server_manager.stop_all().await;

        // Teardown terminal
        app.teardown()?;

        result
    });

    result?;
    Ok(())
}
