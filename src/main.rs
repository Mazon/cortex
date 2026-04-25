pub mod config;
pub mod opencode;
pub mod orchestration;
pub mod persistence;
pub mod state;
pub mod tui;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use config::types::CortexConfig;
use persistence::db::Db;
use state::types::AppState;
use tui::app::App;

/// cortex — TUI Kanban board with OpenCode SDK integration
#[derive(Parser, Debug)]
#[command(name = "cortex", version, about = "TUI Kanban board with OpenCode SDK integration")]
struct Cli {
    /// Reset all persisted state (delete database)
    #[arg(long)]
    reset: bool,

    /// Path to config file
    #[arg(long)]
    config: Option<String>,
}

fn main() -> Result<()> {
    // === Phase 1: Synchronous initialization (before runtime) ===

    // Parse CLI args
    let cli = Cli::parse();

    // Load config (must happen before logger init so we can read config.log.level)
    let config_path = cli
        .config
        .as_ref()
        .map(|p| std::path::PathBuf::from(p))
        .unwrap_or_else(config::default_config_path);

    let mut config = config::load_config(&config_path)?;

    // Initialize tracing — writes to stderr by default
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log.level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Enter alternate screen early to hide any residual startup output
    App::setup_terminal()?;

    // Guard: restore terminal if anything fails before the main loop's teardown runs.
    // Both disable_raw_mode and LeaveAlternateScreen are idempotent, so the double-cleanup
    // on the happy path (teardown + guard Drop) is safe.
    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::terminal::LeaveAlternateScreen
            );
        }
    }
    let _terminal_guard = TerminalGuard;

    // Render initial loading screen so users see feedback immediately
    // instead of a blank alternate screen while servers start up.
    let mut loading_terminal = tui::Terminal::new(tui::CrosstermBackend::new(std::io::stdout()))?;
    let mut spinner_idx: usize = 0;
    tui::loading::render_loading_frame(
        &mut loading_terminal,
        "Starting Cortex...",
        spinner_idx,
    )?;

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
            let mut state = state.lock().unwrap();
            if let Err(e) = persistence::restore_state(&mut state, &db) {
                tracing::error!("Failed to restore persisted state: {}", e);
            }
        }

        // If no projects exist, create a default one
        {
            let mut state = state.lock().unwrap();
            if state.projects.is_empty() {
                let id = uuid::Uuid::new_v4().to_string();
                let project = state::types::CortexProject {
                    id: id.clone(),
                    name: "Default".to_string(),
                    working_directory: std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string()),
                    status: state::types::ProjectStatus::Idle,
                    position: 0,
                };
                state.add_project(project);
                state.select_project(&id);
            }
        }

        // Create shared OpenCode client and server (single process for all projects)
        let mut opencode_clients: HashMap<String, opencode::client::OpenCodeClient> = HashMap::new();
        let mut server_manager = opencode::server::ServerManager::new();

        // Start a single shared server using the first project's working directory.
        let projects_snapshot: Vec<(String, String, String)> = state
            .lock()
            .unwrap()
            .projects
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.working_directory.clone()))
            .collect();

        if let Some((_, project_name, working_dir)) = projects_snapshot.first() {
            spinner_idx = tui::loading::advance_spinner(spinner_idx);
            tui::loading::render_loading_frame(
                &mut loading_terminal,
                &format!("Starting shared server ({}...)...", project_name),
                spinner_idx,
            )?;

            match server_manager.start_shared(&config.opencode, working_dir).await {
                Ok(url) => {
                    // Create a single shared client
                    match opencode::client::OpenCodeClient::new(&url) {
                        Ok(client) => {
                            // Register the same client for every project
                            for (project_id, _, _) in &projects_snapshot {
                                opencode_clients.insert(project_id.clone(), client.clone());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create OpenCode client: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to start OpenCode server: {}", e);
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
        // Group by unique server URL to avoid duplicate SSE subscriptions.
        // All projects sharing the same server use a single event loop,
        // preventing text duplication from multiple loops processing identical events.
        let mut seen_urls: HashSet<String> = HashSet::new();
        for (project_id, client) in &opencode_clients {
            let url = client.base_url().to_string();
            if !seen_urls.insert(url) {
                continue;
            }
            let client = client.clone();
            let state = state.clone();
            let pid = project_id.clone();
            let columns_config = config.columns.clone();
            let opencode_config = config.opencode.clone();
            let shutdown_rx = sse_shutdown_rx.clone();
            let handle = tokio::spawn(async move {
                opencode::events::sse_event_loop(client, state, columns_config, opencode_config, shutdown_rx).await;
            });
            sse_handles.push(handle);
        }

        // Mark as connected if we have at least one client
        if !opencode_clients.is_empty() {
            state.lock().unwrap().connected = true;
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
                        db_error_backoff_ms = (2000u64 * (1 << consecutive_db_errors.min(4))).min(30_000);
                        tracing::error!(
                            "Failed to open DB for save (attempt {}): {}",
                            consecutive_db_errors, e,
                        );
                        continue;
                    }
                };

                let mut state = state_for_save.lock().unwrap();
                if state.take_dirty() {
                    if let Err(e) = persistence::save_state(&mut state, &db) {
                        consecutive_db_errors += 1;
                        db_error_backoff_ms = (2000u64 * (1 << consecutive_db_errors.min(4))).min(30_000);
                        tracing::error!(
                            "Failed to save state (attempt {}): {}",
                            consecutive_db_errors, e,
                        );
                    } else {
                        // Success — reset backoff
                        consecutive_db_errors = 0;
                        db_error_backoff_ms = 0;
                    }
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
            let mut state = state.lock().unwrap();
            let db_path = persistence::db::default_db_path();
            if let Ok(db) = Db::new(&db_path) {
                if let Err(e) = persistence::save_state(&mut state, &db) {
                    tracing::error!("Failed to save state on shutdown: {}", e);
                }
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


