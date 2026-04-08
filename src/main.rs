pub mod config;
pub mod error;
pub mod opencode;
pub mod orchestration;
pub mod persistence;
pub mod state;
pub mod tui;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;

use config::types::CortexConfig;
use persistence::db::Db;
use state::types::AppState;
use tui::app::App;

/// cortex2 — TUI Kanban board with OpenCode SDK integration
#[derive(Parser, Debug)]
#[command(name = "cortex2", version, about = "TUI Kanban board with OpenCode SDK integration")]
struct Cli {
    /// Reset all persisted state (delete database)
    #[arg(long)]
    reset: bool,

    /// Path to config file
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI args
    let cli = Cli::parse();

    // Initialize logger
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .format_timestamp(None)
    .init();

    log::info!("Starting cortex2...");

    // Load config
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

    // Handle --reset flag
    let db_path = persistence::db::default_db_path();
    if cli.reset {
        if db_path.exists() {
            log::info!("Resetting database: {:?}", db_path);
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
            log::warn!("Failed to restore state: {}", e);
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
                log::info!("Server started for project: {}", project.name);
            }
            Err(e) => {
                log::warn!(
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
            log::info!("Starting SSE event loop for project {}", pid);
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
                    log::error!("Failed to open DB for save: {}", e);
                    continue;
                }
            };
            let state = state_for_save.lock().unwrap();
            if state.take_dirty() {
                if let Err(e) = persistence::save_state(&state, &db) {
                    log::error!("Failed to save state: {}", e);
                } else {
                    log::debug!("State saved (periodic)");
                }
            }
        }
    });

    // Run TUI event loop
    let mut app = App::new(state.clone(), config.clone())?;
    let result = app.run().await;

    // Graceful shutdown
    log::info!("Shutting down...");

    // Cancel persistence task
    persistence_handle.abort();

    // Force-save state before exit
    {
        let state = state.lock().unwrap();
        let db_path = persistence::db::default_db_path();
        if let Ok(db) = Db::new(&db_path) {
            if let Err(e) = persistence::save_state(&state, &db) {
                log::error!("Failed to save state on shutdown: {}", e);
            } else {
                log::info!("State saved on shutdown");
            }
        }
    }

    // Force-save state before exit
    {
        let state = state.lock().unwrap();
        let db_path = persistence::db::default_db_path();
        if let Ok(db) = Db::new(&db_path) {
            if let Err(e) = persistence::save_state(&state, &db) {
                log::error!("Failed to save state on shutdown: {}", e);
            } else {
                log::info!("State saved on shutdown");
            }
        }
    }

    // Stop servers
    server_manager.stop_all().await;

    // Teardown terminal
    app.teardown()?;

    result?;
    log::info!("cortex2 exited cleanly");
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
