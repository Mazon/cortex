//! TUI App struct and event loop.

use crate::config::types::CortexConfig;
use crate::state::types::AppState;
use crate::tui::{CrosstermBackend, Terminal};
use crossterm::event::{self, Event, KeyEventKind};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The main TUI application.
pub struct App {
    pub state: Arc<Mutex<AppState>>,
    pub config: CortexConfig,
    pub terminal: Terminal,
    pub should_quit: bool,
}

impl App {
    /// Create a new App instance.
    pub fn new(state: Arc<Mutex<AppState>>, config: CortexConfig) -> anyhow::Result<Self> {
        // Setup terminal
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(std::io::stdout());
        let terminal = ratatui::Terminal::new(backend)?;

        Ok(Self {
            state,
            config,
            terminal,
            should_quit: false,
        })
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(100);

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
                            if let Ok(Event::Key(key)) = event::read() {
                                if key.kind == KeyEventKind::Press {
                                    self.handle_key_event(key);
                                }
                            }
                        }
                        _ => {} // Timeout or error, just re-render
                    }
                }
            }

            // Clear expired notifications
            {
                let mut state = self.state.lock().unwrap();
                state.clear_expired_notifications();
            }

            // Render
            {
                let state = &self.state;
                let config = &self.config;
                self.terminal.draw(|f| {
                    let locked = state.lock().unwrap();
                    match locked.ui.mode {
                        crate::state::types::AppMode::Normal => {
                            crate::tui::render_normal(f, &locked, config);
                        }
                        crate::state::types::AppMode::TaskEditor => {
                            crate::tui::task_editor::render_task_editor(f, &locked);
                        }
                        crate::state::types::AppMode::Help => {
                            crate::tui::render_normal(f, &locked, config);
                            crate::tui::help::render_help_overlay(f);
                        }
                    }
                })?;
            }
        }

        Ok(())
    }

    /// Handle a key event based on current mode.
    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
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
        }
    }

    /// Handle key events in Normal mode.
    fn handle_normal_key(&mut self, key: crossterm::event::KeyEvent) {
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
            Some(Action::NavLeft) => {
                let visible = self.config.columns.visible_column_ids();
                let mut state = self.state.lock().unwrap();
                if state.kanban.focused_column_index > 0 {
                    state.kanban.focused_column_index -= 1;
                    if let Some(col_id) = visible.get(state.kanban.focused_column_index) {
                        state.set_focused_column(col_id);
                        state.ui.focused_column = col_id.to_string();
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
                        state.ui.focused_column = col_id.to_string();
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
                        let target_col = visible[current_col_idx + 1];
                        let mut state = self.state.lock().unwrap();
                        state.move_task(&tid, crate::state::types::KanbanColumn(target_col.to_string()));
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
                        let target_col = visible[current_col_idx - 1];
                        let mut state = self.state.lock().unwrap();
                        state.move_task(
                            &tid,
                            crate::state::types::KanbanColumn(target_col.to_string()),
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
                    log::info!("Abort session requested: {}", sid);
                    let mut state = self.state.lock().unwrap();
                    state.set_notification(
                        "Session abort requested".to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
                }
            }
            None => {} // Unmatched key, ignore
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
        let task_id = task_ids.get(idx).cloned();
        state.ui.focused_task_id = task_id;
    }
}
