//! TUI App struct and event loop.

use crate::config::types::CortexConfig;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::AppState;
use crate::tui::{CrosstermBackend, Terminal};
use crossterm::event::{self, Event, KeyEventKind};
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The main TUI application.
pub struct App {
    pub state: Arc<Mutex<AppState>>,
    pub config: CortexConfig,
    pub terminal: Terminal,
    pub should_quit: bool,
    /// OpenCode clients keyed by project ID, used for API calls from the TUI.
    pub opencode_clients: HashMap<String, OpenCodeClient>,
}

impl App {
    /// Setup the terminal: enable raw mode and enter alternate screen.
    ///
    /// Call this early in `main()` (before server startup) to hide any
    /// residual log output from the primary terminal buffer.
    pub fn setup_terminal() -> anyhow::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        std::io::stdout().flush()?;
        Ok(())
    }

    /// Create a new App instance.
    pub fn new(
        state: Arc<Mutex<AppState>>,
        config: CortexConfig,
        opencode_clients: HashMap<String, OpenCodeClient>,
    ) -> anyhow::Result<Self> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let terminal = ratatui::Terminal::new(backend)?;

        Ok(Self {
            state,
            config,
            terminal,
            should_quit: false,
            opencode_clients,
        })
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(100);

        // Set up graceful shutdown via a background signal-handler task.
        // Listens for SIGINT (Ctrl+C) and SIGTERM, then notifies the event
        // loop so `should_quit` is set and the existing shutdown sequence in
        // main.rs runs cleanly (save state, stop servers, teardown terminal)
        // instead of leaving the terminal in raw mode / alternate screen.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                let mut sigterm = match tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Failed to register SIGTERM handler: {}", e);
                        let _ = tokio::signal::ctrl_c().await;
                        tracing::info!("Received SIGINT — shutting down gracefully");
                        let _ = shutdown_tx.send(()).await;
                        return;
                    }
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("Received SIGINT — shutting down gracefully");
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("Received SIGTERM — shutting down gracefully");
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("Received SIGINT — shutting down gracefully");
            }
            let _ = shutdown_tx.send(()).await;
        });

        loop {
            if self.should_quit {
                break;
            }

            // Use tokio::select for async + sync event handling
            tokio::select! {
                // Poll for crossterm events
                result = tokio::task::spawn_blocking(move || {
                    event::poll(tick_rate)
                }) => {
                    match result {
                        Ok(Ok(true)) => {
                            // Event available, read it
                            if let Ok(event) = event::read() {
                                match event {
                                    Event::Key(key) => {
                                        if key.kind == KeyEventKind::Press {
                                            self.handle_key_event(key);
                                        }
                                    }
                                    Event::Resize(_width, _height) => {
                                        // Terminal was resized — force a full
                                        // redraw so layout adapts to the new
                                        // dimensions.
                                        self.state
                                            .lock()
                                            .unwrap()
                                            .mark_render_dirty();
                                    }
                                    _ => {} // Ignore mouse / paste events
                                }
                            }
                        }
                        _ => {} // Timeout or error, just re-render
                    }
                }
                // Graceful shutdown on SIGINT / SIGTERM (from signal handler task).
                _ = shutdown_rx.recv() => {
                    self.should_quit = true;
                }
            }

            // Clear expired notifications (may dirty the render state)
            {
                let mut state = self.state.lock().unwrap();
                if state.clear_expired_notifications() {
                    state.mark_render_dirty();
                }
            }

            // Render — only if the state has changed since the last frame.
            // This avoids expensive full UI re-renders every 100 ms tick when
            // nothing has changed.
            let needs_render = self.state.lock().unwrap().take_render_dirty();
            if needs_render {
                let state_snapshot = self.state.lock().unwrap().clone();
                let config = &self.config;
                self.terminal.draw(|f| {
                    match state_snapshot.ui.mode {
                        crate::state::types::AppMode::Normal => {
                            crate::tui::render_normal(f, &state_snapshot, config);
                        }
                        crate::state::types::AppMode::TaskEditor => {
                            crate::tui::task_editor::render_task_editor(f, &state_snapshot);
                        }
                        crate::state::types::AppMode::Help => {
                            crate::tui::render_normal(f, &state_snapshot, config);
                            crate::tui::help::render_help_overlay(f);
                        }
                        crate::state::types::AppMode::ProjectRename => {
                            crate::tui::render_normal(f, &state_snapshot, config);
                            crate::tui::prompt::render_input_prompt(f, &state_snapshot);
                        }
                        crate::state::types::AppMode::InputPrompt => {
                            crate::tui::render_normal(f, &state_snapshot, config);
                            crate::tui::prompt::render_input_prompt(f, &state_snapshot);
                        }
                    }
                })?;
            }
        }

        Ok(())
    }

    /// Handle a key event based on current mode.
    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        // Any key press potentially changes state — mark for re-render.
        self.state.lock().unwrap().mark_render_dirty();

        let mode = {
            let state = self.state.lock().unwrap();
            state.ui.mode.clone()
        };

        match mode {
            crate::state::types::AppMode::Normal => {
                self.handle_normal_key(key);
            }
            crate::state::types::AppMode::TaskEditor => {
                self.handle_editor_key(key);
            }
            crate::state::types::AppMode::Help => {
                // Any key dismisses help
                let mut state = self.state.lock().unwrap();
                state.ui.mode = crate::state::types::AppMode::Normal;
            }
            crate::state::types::AppMode::ProjectRename => {
                self.handle_rename_key(key);
            }
            crate::state::types::AppMode::InputPrompt => {
                self.handle_input_prompt_key(key);
            }
        }
    }

    /// Handle key events in Normal mode.
    fn handle_normal_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        // Check if we're in task detail view — Escape closes it
        {
            let is_detail_escape = {
                let state = self.state.lock().unwrap();
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail && key.code == KeyCode::Esc
            };
            // First lock dropped here
            if is_detail_escape {
                let mut state = self.state.lock().unwrap();
                state.close_task_detail();
                return;
            }
            // TODO: handle y/n for permission approval here
        }

        use crate::tui::keys::{Action, KeyMatcher};

        let key_matcher = KeyMatcher::from_config(&self.config.keybindings);
        let action = key_matcher.match_key(key);

        match action {
            Some(Action::Quit) => {
                self.should_quit = true;
            }
            Some(Action::HelpToggle) => {
                let mut state = self.state.lock().unwrap();
                state.ui.mode = crate::state::types::AppMode::Help;
            }
            Some(Action::PrevProject) => {
                let mut state = self.state.lock().unwrap();
                let len = state.projects.len();
                if len <= 1 {
                    return;
                }
                let current_idx = state
                    .active_project_id
                    .as_ref()
                    .and_then(|id| state.projects.iter().position(|p| &p.id == id))
                    .unwrap_or(0);
                let new_idx = if current_idx == 0 { len - 1 } else { current_idx - 1 };
                let new_id = state.projects[new_idx].id.clone();
                state.select_project(&new_id);
            }
            Some(Action::NextProject) => {
                let mut state = self.state.lock().unwrap();
                let len = state.projects.len();
                if len <= 1 {
                    return;
                }
                let current_idx = state
                    .active_project_id
                    .as_ref()
                    .and_then(|id| state.projects.iter().position(|p| &p.id == id))
                    .unwrap_or(0);
                let new_idx = (current_idx + 1) % len;
                let new_id = state.projects[new_idx].id.clone();
                state.select_project(&new_id);
            }
            Some(Action::NewProject) => {
                // For now, create a default project
                let mut state = self.state.lock().unwrap();
                let id = uuid::Uuid::new_v4().to_string();
                let pos = state.projects.len();
                let project = crate::state::types::CortexProject {
                    id: id.clone(),
                    name: format!("Project {}", pos + 1),
                    working_directory: std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string()),
                    status: crate::state::types::ProjectStatus::Idle,
                    position: pos,
                };
                state.add_project(project);
                state.select_project(&id);
                state.set_notification(
                    format!("Created project {}", pos + 1),
                    crate::state::types::NotificationVariant::Success,
                    3000,
                );
            }
            Some(Action::RenameProject) => {
                let mut state = self.state.lock().unwrap();
                state.open_project_rename();
            }
            Some(Action::SetWorkingDirectory) => {
                let mut state = self.state.lock().unwrap();
                state.open_set_working_directory();
            }
            Some(Action::NavLeft) => {
                let visible = self.config.columns.visible_column_ids();
                let mut state = self.state.lock().unwrap();
                if state.kanban.focused_column_index > 0 {
                    state.kanban.focused_column_index -= 1;
                    if let Some(col_id) = visible.get(state.kanban.focused_column_index) {
                        state.set_focused_column(col_id);
                    }
                }
            }
            Some(Action::NavRight) => {
                let visible = self.config.columns.visible_column_ids();
                let mut state = self.state.lock().unwrap();
                if state.kanban.focused_column_index + 1 < visible.len() {
                    state.kanban.focused_column_index += 1;
                    if let Some(col_id) = visible.get(state.kanban.focused_column_index) {
                        state.set_focused_column(col_id);
                    }
                }
            }
            Some(Action::NavUp) => {
                let mut state = self.state.lock().unwrap();
                let col_id = state.ui.focused_column.clone();
                let current = state.kanban.focused_task_index.get(&col_id).copied().unwrap_or(0);
                if current > 0 {
                    state.kanban.focused_task_index.insert(col_id.clone(), current - 1);
                    update_focused_task_id(&mut state, &col_id);
                }
            }
            Some(Action::NavDown) => {
                let mut state = self.state.lock().unwrap();
                let col_id = state.ui.focused_column.clone();
                let task_count = state
                    .kanban
                    .columns
                    .get(&col_id)
                    .map(|v| v.len())
                    .unwrap_or(0);
                let current = state.kanban.focused_task_index.get(&col_id).copied().unwrap_or(0);
                if current + 1 < task_count {
                    state.kanban.focused_task_index.insert(col_id.clone(), current + 1);
                    update_focused_task_id(&mut state, &col_id);
                }
            }
            Some(Action::CreateTask) => {
                let col_id = {
                    let state = self.state.lock().unwrap();
                    state.ui.focused_column.clone()
                };
                let mut state = self.state.lock().unwrap();
                state.open_task_editor_create(&col_id);
            }
            Some(Action::EditTask) => {
                let task_id = {
                    let state = self.state.lock().unwrap();
                    state.ui.focused_task_id.clone()
                };
                if let Some(id) = task_id {
                    let mut state = self.state.lock().unwrap();
                    state.open_task_editor_edit(&id);
                }
            }
            Some(Action::MoveForward) => {
                let visible = self.config.columns.visible_column_ids();
                let (task_id, current_col_idx) = {
                    let state = self.state.lock().unwrap();
                    let tid = state.ui.focused_task_id.clone();
                    let idx = state.kanban.focused_column_index;
                    (tid, idx)
                };
                if let Some(tid) = task_id {
                    if current_col_idx + 1 < visible.len() {
                        let target_col = visible[current_col_idx + 1].clone();
                        let mut state = self.state.lock().unwrap();
                        state.move_task(&tid, crate::state::types::KanbanColumn(target_col));
                    } else {
                        let mut state = self.state.lock().unwrap();
                        state.set_notification(
                            "Already at the last column".to_string(),
                            crate::state::types::NotificationVariant::Info,
                            2000,
                        );
                    }
                }
            }
            Some(Action::MoveBackward) => {
                let visible = self.config.columns.visible_column_ids();
                let (task_id, current_col_idx) = {
                    let state = self.state.lock().unwrap();
                    let tid = state.ui.focused_task_id.clone();
                    let idx = state.kanban.focused_column_index;
                    (tid, idx)
                };
                if let Some(tid) = task_id {
                    if current_col_idx > 0 {
                        let target_col = visible[current_col_idx - 1].clone();
                        let mut state = self.state.lock().unwrap();
                        state.move_task(
                            &tid,
                            crate::state::types::KanbanColumn(target_col),
                        );
                    } else {
                        let mut state = self.state.lock().unwrap();
                        state.set_notification(
                            "Already at the first column".to_string(),
                            crate::state::types::NotificationVariant::Info,
                            2000,
                        );
                    }
                }
            }
            Some(Action::DeleteTask) => {
                let task_id = {
                    let state = self.state.lock().unwrap();
                    state.ui.focused_task_id.clone()
                };
                if let Some(tid) = task_id {
                    let mut state = self.state.lock().unwrap();
                    state.delete_task(&tid);
                    state.set_notification(
                        "Task deleted".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        3000,
                    );
                }
            }
            Some(Action::ViewTask) => {
                let task_id = {
                    let state = self.state.lock().unwrap();
                    state.ui.focused_task_id.clone()
                };
                if let Some(tid) = task_id {
                    let mut state = self.state.lock().unwrap();
                    state.open_task_detail(&tid);
                }
            }
            Some(Action::AbortSession) => {
                let session_id = {
                    let state = self.state.lock().unwrap();
                    state
                        .ui
                        .focused_task_id
                        .as_ref()
                        .and_then(|tid| state.tasks.get(tid))
                        .and_then(|t| t.session_id.clone())
                };
                if let Some(sid) = session_id {
                    tracing::info!("Abort session requested: {}", sid);

                    // Find the client for the active project and spawn an abort task.
                    let client = {
                        let state = self.state.lock().unwrap();
                        state
                            .active_project_id
                            .as_ref()
                            .and_then(|pid| self.opencode_clients.get(pid))
                            .cloned()
                    };

                    if let Some(client) = client {
                        let state = self.state.clone();
                        tokio::spawn(async move {
                            match client.abort_session(&sid).await {
                                Ok(aborted) => {
                                    if aborted {
                                        tracing::info!("Session {} aborted successfully", sid);
                                    } else {
                                        tracing::warn!(
                                            "Session {} abort returned false (may already be done)",
                                            sid
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to abort session {}: {}", sid, e);
                                }
                            }
                            // Update notification after attempt
                            let mut state = state.lock().unwrap();
                            state.set_notification(
                                format!("Session abort requested: {}", sid),
                                crate::state::types::NotificationVariant::Warning,
                                3000,
                            );
                            state.mark_render_dirty();
                        });
                    } else {
                        tracing::warn!(
                            "No OpenCode client available for aborting session {}",
                            sid
                        );
                        let mut state = self.state.lock().unwrap();
                        state.set_notification(
                            "No client available to abort session".to_string(),
                            crate::state::types::NotificationVariant::Error,
                            3000,
                        );
                    }
                }
            }
            None => {} // Unmatched key, ignore
        }
    }

    /// Handle key events in ProjectRename mode.
    ///
    /// Supports basic text editing: character insertion, backspace, delete,
    /// cursor movement, Enter to confirm, and Escape to cancel.
    fn handle_rename_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                let mut state = self.state.lock().unwrap();
                match state.submit_project_rename() {
                    Some((_old, new)) => {
                        state.set_notification(
                            format!("Project renamed to \"{}\"", new),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );
                    }
                    None => {
                        // Empty name — show warning and stay in rename mode
                        state.set_notification(
                            "Project name cannot be empty".to_string(),
                            crate::state::types::NotificationVariant::Warning,
                            2000,
                        );
                    }
                }
            }
            KeyCode::Esc => {
                let mut state = self.state.lock().unwrap();
                state.cancel_project_rename();
            }
            KeyCode::Char(c) => {
                let mut state = self.state.lock().unwrap();
                let pos = state.ui.input_cursor.min(state.ui.input_text.len());
                state.ui.input_text.insert(pos, c);
                state.ui.input_cursor = pos + 1;
            }
            KeyCode::Backspace => {
                let mut state = self.state.lock().unwrap();
                if state.ui.input_cursor > 0 {
                    state.ui.input_cursor -= 1;
                    let pos = state.ui.input_cursor;
                    state.ui.input_text.remove(pos);
                }
            }
            KeyCode::Delete => {
                let mut state = self.state.lock().unwrap();
                let pos = state.ui.input_cursor;
                if pos < state.ui.input_text.len() {
                    state.ui.input_text.remove(pos);
                }
            }
            KeyCode::Left => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let mut state = self.state.lock().unwrap();
                let new_pos = state.ui.input_cursor + 1;
                state.ui.input_cursor = new_pos.min(state.ui.input_text.len());
            }
            KeyCode::Home => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = 0;
            }
            KeyCode::End => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_text.len();
            }
            _ => {} // Ignore other keys
        }
    }

    /// Handle key events in InputPrompt mode (used for working directory).
    fn handle_input_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                let mut state = self.state.lock().unwrap();
                if state.submit_working_directory() {
                    state.set_notification(
                        "Working directory updated".to_string(),
                        crate::state::types::NotificationVariant::Success,
                        3000,
                    );
                } else {
                    state.set_notification(
                        "Working directory cannot be empty".to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        2000,
                    );
                }
            }
            KeyCode::Esc => {
                let mut state = self.state.lock().unwrap();
                state.cancel_working_directory();
            }
            KeyCode::Char(c) => {
                let mut state = self.state.lock().unwrap();
                let pos = state.ui.input_cursor.min(state.ui.input_text.len());
                state.ui.input_text.insert(pos, c);
                state.ui.input_cursor = pos + 1;
            }
            KeyCode::Backspace => {
                let mut state = self.state.lock().unwrap();
                if state.ui.input_cursor > 0 {
                    state.ui.input_cursor -= 1;
                    let pos = state.ui.input_cursor;
                    state.ui.input_text.remove(pos);
                }
            }
            KeyCode::Delete => {
                let mut state = self.state.lock().unwrap();
                let pos = state.ui.input_cursor;
                if pos < state.ui.input_text.len() {
                    state.ui.input_text.remove(pos);
                }
            }
            KeyCode::Left => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let mut state = self.state.lock().unwrap();
                let new_pos = state.ui.input_cursor + 1;
                state.ui.input_cursor = new_pos.min(state.ui.input_text.len());
            }
            KeyCode::Home => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = 0;
            }
            KeyCode::End => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_text.len();
            }
            _ => {}
        }
    }

    /// Handle key events in TaskEditor mode.
    fn handle_editor_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::tui::editor_handler::{handle_editor_input, EditorAction};

        let action = {
            let mut state = self.state.lock().unwrap();
            if let Some(editor) = state.get_task_editor_mut() {
                handle_editor_input(editor, key)
            } else {
                EditorAction::None
            }
        };

        match action {
            EditorAction::Save => {
                let mut state = self.state.lock().unwrap();
                match state.save_task_editor() {
                    Ok(task_id) => {
                        state.set_notification(
                            format!("Task saved: {}", task_id),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );
                    }
                    Err(e) => {
                        state.set_notification(
                            format!("Save failed: {}", e),
                            crate::state::types::NotificationVariant::Error,
                            3000,
                        );
                    }
                }
            }
            EditorAction::Cancel => {
                let mut state = self.state.lock().unwrap();
                state.cancel_task_editor();
            }
            EditorAction::None => {}
        }
    }

    /// Teardown the terminal. Call this on shutdown.
    pub fn teardown(&mut self) -> anyhow::Result<()> {
        crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        )?;
        crossterm::terminal::disable_raw_mode()?;
        Ok(())
    }
}

/// Update the focused task ID based on the column's focused task index.
fn update_focused_task_id(state: &mut AppState, col_id: &str) {
    let idx = state.kanban.focused_task_index.get(col_id).copied().unwrap_or(0);
    if let Some(task_ids) = state.kanban.columns.get(col_id) {
        let clamped = idx.min(task_ids.len().saturating_sub(1));
        state.ui.focused_task_id = task_ids.get(clamped).cloned();
    }
}
