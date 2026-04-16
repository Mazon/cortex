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
use tracing_subscriber::prelude::*;

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

    // Ensure config directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Initialize tracing with file appender
    let log_dir = config::xdg_data_home()
        .join("cortex")
        .join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    let file_appender = tracing_appender::rolling::never(&log_dir, "cortex.log");
    // IMPORTANT: This guard must live until program exit. When dropped, it flushes all buffered log writes to disk.
    let (non_blocking, _log_flush_guard) = tracing_appender::non_blocking(file_appender);

    // Build filter: config sets default, RUST_LOG env var overrides
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log.level));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(false),
        )
        .init();

    tracing::debug!("Logging to {}/cortex.log", log_dir.display());
    tracing::info!("Logger initialized — writing to {}/cortex.log", log_dir.display());
    tracing::info!("Starting cortex...");

    // Redirect stderr to the log file so nothing leaks to the TUI
    #[cfg(unix)]
    {
        let log_file_path = log_dir.join("cortex-stderr.log");
        let stderr_log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path)
            .with_context(|| format!("Failed to open stderr log: {}", log_file_path.display()))?;
        use std::os::unix::io::AsRawFd;
        let fd = stderr_log.as_raw_fd();
        unsafe {
            libc::dup2(fd, 2); // fd 2 = stderr
        }
        // stderr_log handle can be dropped — dup2 duplicates the fd
    }

    // Install custom panic hook that writes to the log file instead of stderr
    let panic_log_dir = log_dir.clone();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("PANIC: {:?}", info);
        let log_file = panic_log_dir.join("cortex.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_file) {
            use std::io::Write;
            let _ = writeln!(f, "{}", msg);
        }
    }));

    // Enter alternate screen early to hide any residual startup output
    App::setup_terminal()?;

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
                tracing::info!("Resetting database: {:?}", db_path);
                std::fs::remove_file(&db_path)?;
            }
        }

        // Open database
        let db = Db::new(&db_path)?;

        // Create app state
        let state = Arc::new(Mutex::new(AppState::default()));

        // Restore persisted state
        {
            let mut state = state.lock().unwrap();
            if let Err(e) = persistence::restore_state(&mut state, &db) {
                tracing::warn!("Failed to restore state: {}", e);
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

        // Create OpenCode client for active project
        let mut opencode_clients: HashMap<String, opencode::client::OpenCodeClient> = HashMap::new();
        let mut server_manager = opencode::server::ServerManager::new(config.opencode.port);

        // Start servers for each project (optional — only if opencode is available)
        for project in state.lock().unwrap().projects.iter() {
            match start_project_server(
                &project.id,
                &project.working_directory,
                &mut config.opencode,
                &mut server_manager,
                &mut opencode_clients,
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("Server started for project: {}", project.name);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to start server for project {}: {}. Continuing without server.",
                        project.name,
                        e
                    );
                }
            }
        }

        // Spawn SSE event loops for active clients
        let mut sse_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        for (project_id, client) in &opencode_clients {
            let client = client.clone();
            let state = state.clone();
            let pid = project_id.clone();
            let handle = tokio::spawn(async move {
                tracing::info!("Starting SSE event loop for project {}", pid);
                opencode::events::sse_event_loop(client, state).await;
            });
            sse_handles.push(handle);
        }

        // Mark as connected if we have at least one client
        if !opencode_clients.is_empty() {
            state.lock().unwrap().connected = true;
        }

        // Spawn periodic persistence save task
        let state_for_save = state.clone();
        let db_path_for_save = persistence::db::default_db_path();
        let persistence_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                // Open a new connection each time for simplicity
                let db = match Db::new(&db_path_for_save) {
                    Ok(db) => db,
                    Err(e) => {
                        tracing::error!("Failed to open DB for save: {}", e);
                        continue;
                    }
                };
                let state = state_for_save.lock().unwrap();
                if state.take_dirty() {
                    if let Err(e) = persistence::save_state(&state, &db) {
                        tracing::error!("Failed to save state: {}", e);
                    } else {
                        tracing::debug!("State saved (periodic)");
                    }
                }
            }
        });

        // Run TUI event loop
        let mut app = App::new(state.clone(), config.clone())?;
        let result = app.run().await;

        // Graceful shutdown
        tracing::info!("Shutting down...");

        // Cancel persistence task
        persistence_handle.abort();

        // Abort SSE event loops
        for handle in sse_handles {
            handle.abort();
        }
        // Give them a moment to clean up
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Force-save state before exit
        {
            let state = state.lock().unwrap();
            let db_path = persistence::db::default_db_path();
            if let Ok(db) = Db::new(&db_path) {
                if let Err(e) = persistence::save_state(&state, &db) {
                    tracing::error!("Failed to save state on shutdown: {}", e);
                } else {
                    tracing::info!("State saved on shutdown");
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
    tracing::info!("cortex exited cleanly");
    Ok(())
}

/// Start an OpenCode server for a project and create a client.
async fn start_project_server(
    project_id: &str,
    working_dir: &str,
    opencode_config: &mut config::types::OpenCodeConfig,
    server_manager: &mut opencode::server::ServerManager,
    clients: &mut HashMap<String, opencode::client::OpenCodeClient>,
) -> Result<()> {
    let url = server_manager
        .start_for_project(project_id, opencode_config, working_dir)
        .await?;

    let client = opencode::client::OpenCodeClient::new(&url)?;
    clients.insert(project_id.to_string(), client);

    Ok(())
}
