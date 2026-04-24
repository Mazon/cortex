//! TUI App struct and event loop.

use crate::config::types::CortexConfig;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::AppState;
use crate::tui::{CrosstermBackend, Terminal};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::prelude::Size;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Identifies which input prompt is active, so `handle_text_input` can
/// dispatch submit/cancel to the correct state method.
#[derive(Clone, Copy)]
enum InputPrompt {
    RenameProject,
    WorkingDirectory,
}

/// The main TUI application.
pub struct App {
    pub state: Arc<Mutex<AppState>>,
    pub config: CortexConfig,
    pub terminal: Terminal,
    pub should_quit: bool,
    /// OpenCode clients keyed by project ID, used for API calls from the TUI.
    pub opencode_clients: HashMap<String, OpenCodeClient>,
    /// Pre-computed key matcher — built once from config, avoids per-keypress allocation.
    key_matcher: crate::tui::keys::KeyMatcher,
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
        let key_matcher = crate::tui::keys::KeyMatcher::from_config(&config.keybindings);

        Ok(Self {
            state,
            config,
            terminal,
            should_quit: false,
            opencode_clients,
            key_matcher,
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
            //
            // We hold the Mutex lock for the duration of `terminal.draw()`.
            // This is the standard ratatui pattern — the draw closure is fast
            // (it only builds a frame buffer), so the lock is held briefly.
            let needs_render = self.state.lock().unwrap().take_render_dirty();
            if needs_render {
                let config = &self.config;
                let mut state = self.state.lock().unwrap();
                self.terminal.draw(|f| {
                    let state = &mut *state;
                    match state.ui.mode {
                        crate::state::types::AppMode::Normal => {
                            crate::tui::render_normal(f, state, config);
                        }
                        crate::state::types::AppMode::TaskEditor => {
                            crate::tui::task_editor::render_task_editor(f, state, config);
                        }
                        crate::state::types::AppMode::Help => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::help::render_help_overlay(f, &config.keybindings);
                        }
                        crate::state::types::AppMode::ProjectRename => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::prompt::render_input_prompt(f, state);
                        }
                        crate::state::types::AppMode::InputPrompt => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::prompt::render_input_prompt(f, state);
                        }
                        crate::state::types::AppMode::ConfirmDialog => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::prompt::render_confirm_dialog(f, state);
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
            crate::state::types::AppMode::ConfirmDialog => {
                self.handle_confirm_dialog_key(key);
            }
        }
    }

    /// Handle key events in Normal mode.
    ///
    /// Resolves the key to an [`Action`] via the configured keybindings and
    /// dispatches to the appropriate handler method.
    fn handle_normal_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        // Check if we're in task detail view — Escape closes it, y/n approve/reject permissions
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

            // Handle Up/Down arrows and G/g for scrolling output in task detail view
            if matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('G') | KeyCode::Char('g')
            ) && key.modifiers.is_empty()
            {
                let in_detail = {
                    let state = self.state.lock().unwrap();
                    state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                };
                if in_detail {
                    let mut state = self.state.lock().unwrap();
                    let total_lines = state
                        .ui
                        .viewing_task_id
                        .as_ref()
                        .and_then(|tid| state.cached_streaming_lines.get(tid))
                        .map(|(_, lines)| lines.len())
                        .unwrap_or(0);

                    let max_offset = total_lines.saturating_sub(1);

                    match key.code {
                        KeyCode::Up => {
                            if total_lines > 0 {
                                let current = state.ui.user_scroll_offset.unwrap_or(0);
                                let new_offset = current.saturating_sub(1);
                                state.ui.user_scroll_offset = Some(new_offset);
                                state.mark_render_dirty();
                            }
                            return;
                        }
                        KeyCode::Down => {
                            if total_lines > 0 {
                                let current = state.ui.user_scroll_offset.unwrap_or(max_offset);
                                let new_offset = (current + 1).min(max_offset);
                                if new_offset >= max_offset {
                                    state.ui.user_scroll_offset = None;
                                } else {
                                    state.ui.user_scroll_offset = Some(new_offset);
                                }
                                state.mark_render_dirty();
                            }
                            return;
                        }
                        KeyCode::Char('G') => {
                            state.ui.user_scroll_offset = None;
                            state.mark_render_dirty();
                            return;
                        }
                        KeyCode::Char('g') => {
                            if total_lines > 0 {
                                state.ui.user_scroll_offset = Some(0);
                                state.mark_render_dirty();
                            }
                            return;
                        }
                        _ => {}
                    }
                }
            }

            // Handle y/n for permission approval when in task detail view
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('n')) {
                let approve = key.code == KeyCode::Char('y');
                // Batch read: get pending permission, task_id, and client in one lock.
                let (pending_perm, task_id, client): (
                    Option<crate::state::types::PermissionRequest>,
                    Option<String>,
                    Option<OpenCodeClient>,
                ) = {
                    let state = self.state.lock().unwrap();
                    if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                        (None, None, None)
                    } else if let Some(ref tid) = state.ui.viewing_task_id {
                        let perm = state
                            .task_sessions
                            .get(tid)
                            .and_then(|s| s.pending_permissions.first().cloned());
                        let client = state
                            .active_project_id
                            .as_ref()
                            .and_then(|pid| self.opencode_clients.get(pid))
                            .cloned();
                        (perm, Some(tid.clone()), client)
                    } else {
                        (None, None, None)
                    }
                };

                if let (Some(perm), Some(tid)) = (pending_perm, task_id) {
                    if let Some(client) = client {
                        let state = self.state.clone();
                        let perm_id = perm.id.clone();
                        let session_id = perm.session_id.clone();
                        let tool_name = perm.tool_name.clone();
                        tokio::spawn(async move {
                            match client.resolve_permission(&session_id, &perm_id, approve).await {
                                Ok(()) => {
                                    let action_word = if approve { "approved" } else { "rejected" };
                                    tracing::info!(
                                        "Permission {} {} for tool {}",
                                        perm_id, action_word, tool_name
                                    );
                                    // Only remove from pending list on success
                                    let mut s = state.lock().unwrap();
                                    s.resolve_permission_request(&tid, &perm_id, approve);
                                    s.mark_render_dirty();
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to resolve permission {}: {}",
                                        perm_id, e
                                    );
                                    // Keep the permission in the pending list so the user can retry
                                    let mut s = state.lock().unwrap();
                                    s.set_notification(
                                        format!("Failed to resolve permission: {}", e),
                                        crate::state::types::NotificationVariant::Error,
                                        5000,
                                    );
                                    s.mark_render_dirty();
                                }
                            }
                        });
                    }
                } else {
                    // In TaskDetail view with no pending permission — consume y/n
                    // to prevent fallthrough to keybinding dispatch (e.g. n → CreateTask)
                    let state = self.state.lock().unwrap();
                    if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                        return;
                    }
                }
            }
        }

        // Handle 1-9 for answering questions when in task detail view
        if let KeyCode::Char(c) = key.code {
            if c.is_ascii_digit() && c != '0' {
                let answer_index = (c as usize) - ('1' as usize);
                // Batch read: get pending question, task_id, and client in one lock.
                let (pending_question, task_id, client): (
                    Option<crate::state::types::QuestionRequest>,
                    Option<String>,
                    Option<OpenCodeClient>,
                ) = {
                    let state = self.state.lock().unwrap();
                    if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                        (None, None, None)
                    } else if let Some(ref tid) = state.ui.viewing_task_id {
                        let question = state
                            .task_sessions
                            .get(tid)
                            .and_then(|s| s.pending_questions.first().cloned())
                            .filter(|q| answer_index < q.answers.len());
                        let client = state
                            .active_project_id
                            .as_ref()
                            .and_then(|pid| self.opencode_clients.get(pid))
                            .cloned();
                        (question, Some(tid.clone()), client)
                    } else {
                        (None, None, None)
                    }
                };

                if let (Some(question), Some(tid)) = (pending_question, task_id) {
                    let answer = question.answers[answer_index].clone();
                    if let Some(client) = client {
                        let state = self.state.clone();
                        let question_id = question.id.clone();
                        let session_id = question.session_id.clone();
                        let answer_preview = answer.chars().take(30).collect::<String>();
                        tokio::spawn(async move {
                            match client.resolve_question(&session_id, &question_id, &answer).await {
                                Ok(()) => {
                                    tracing::info!(
                                        "Question {} answered with: {}",
                                        question_id, answer_preview
                                    );
                                    let mut s = state.lock().unwrap();
                                    s.resolve_question_request(&tid, &question_id);
                                    s.set_notification(
                                        format!("Answered: {}", answer_preview),
                                        crate::state::types::NotificationVariant::Success,
                                        3000,
                                    );
                                    s.mark_render_dirty();
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to resolve question {}: {}",
                                        question_id, e
                                    );
                                    let mut s = state.lock().unwrap();
                                    s.set_notification(
                                        format!("Failed to answer question: {}", e),
                                        crate::state::types::NotificationVariant::Error,
                                        5000,
                                    );
                                    s.mark_render_dirty();
                                }
                            }
                        });
                    }
                    return;
                } else {
                    let state = self.state.lock().unwrap();
                    if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                        if let Some(ref tid) = state.ui.viewing_task_id {
                            if state
                                .task_sessions
                                .get(tid)
                                .map(|s| !s.pending_questions.is_empty())
                                .unwrap_or(false)
                            {
                                return;
                            }
                        }
                    }
                }
            }
        }
        use crate::tui::keys::Action;

        let action = self.key_matcher.match_key(key);

        match action {
            Some(Action::Quit) => self.handle_quit(),
            Some(Action::HelpToggle) => self.handle_help_toggle(),
            Some(Action::PrevProject) => self.handle_prev_project(),
            Some(Action::NextProject) => self.handle_next_project(),
            Some(Action::NewProject) => self.handle_new_project(),
            Some(Action::RenameProject) => self.handle_rename_project(),
            Some(Action::SetWorkingDirectory) => self.handle_set_working_directory(),
            Some(Action::DeleteProject) => self.handle_delete_project(),
            Some(Action::NavLeft) => self.handle_nav_column(-1),
            Some(Action::NavRight) => self.handle_nav_column(1),
            Some(Action::NavUp) => self.handle_nav_task(-1),
            Some(Action::NavDown) => self.handle_nav_task(1),
            Some(Action::CreateTask) => self.handle_create_task(),
            Some(Action::EditTask) => self.handle_edit_task(),
            Some(Action::MoveForward) => self.handle_move_task(1),
            Some(Action::MoveBackward) => self.handle_move_task(-1),
            Some(Action::MoveTaskUp) => self.handle_reorder_task(-1),
            Some(Action::MoveTaskDown) => self.handle_reorder_task(1),
            Some(Action::DeleteTask) => self.handle_delete_task(),
            Some(Action::ViewTask) => self.handle_view_task(),
            Some(Action::AbortSession) => self.handle_abort_session(),
            Some(Action::ScrollKanbanLeft) => self.handle_scroll_kanban(-1),
            Some(Action::ScrollKanbanRight) => self.handle_scroll_kanban(1),
            None => {} // Unmatched key, ignore
        }
    }

    // ── Individual action handlers (extracted from handle_normal_key) ──

    fn handle_quit(&mut self) {
        self.should_quit = true;
    }

    fn handle_help_toggle(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.ui.mode = crate::state::types::AppMode::Help;
    }

    fn handle_prev_project(&mut self) {
        self.switch_project_offset(-1);
    }

    fn handle_next_project(&mut self) {
        self.switch_project_offset(1);
    }

    fn handle_new_project(&mut self) {
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

    fn handle_rename_project(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.open_project_rename();
    }

    fn handle_set_working_directory(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.open_set_working_directory();
    }

    fn handle_delete_project(&mut self) {
        let (project_id, _project_name) = {
            let state = self.state.lock().unwrap();
            match state.active_project_id.as_ref() {
                Some(pid) => {
                    let name = state
                        .projects
                        .iter()
                        .find(|p| &p.id == pid)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    (Some(pid.clone()), name)
                }
                None => (None, String::new()),
            }
        };

        let mut state = self.state.lock().unwrap();
        match project_id {
            Some(pid) => {
                state.ui.confirm_action =
                    Some(crate::state::types::ConfirmableAction::DeleteProject(pid));
                state.ui.mode = crate::state::types::AppMode::ConfirmDialog;
            }
            None => {
                state.set_notification(
                    "No active project to delete".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
            }
        }
    }

    /// Move the focused column left or right by `direction` (-1 or +1).
    /// Auto-scrolls the kanban view to keep the focused column visible.
    fn handle_nav_column(&mut self, direction: i32) {
        let visible = self.config.columns.visible_column_ids();
        let mut state = self.state.lock().unwrap();
        let new_idx = state.kanban.focused_column_index as i32 + direction;
        if new_idx >= 0 && (new_idx as usize) < visible.len() {
            state.kanban.focused_column_index = new_idx as usize;
            if let Some(col_id) = visible.get(state.kanban.focused_column_index) {
                state.set_focused_column(col_id);
            }
            // Auto-scroll to keep the focused column visible.
            Self::ensure_column_visible(&mut state, &self.config, &self.terminal);
        }
    }

    /// Move the focused task up or down by `direction` (-1 or +1).
    fn handle_nav_task(&mut self, direction: i32) {
        let mut state = self.state.lock().unwrap();
        let col_id = state.ui.focused_column.clone();
        let task_count = state
            .kanban
            .columns
            .get(&col_id)
            .map(|v| v.len())
            .unwrap_or(0);
        let current = state.kanban.focused_task_index.get(&col_id).copied().unwrap_or(0);
        let new_idx = current as i32 + direction;
        if new_idx >= 0 && (new_idx as usize) < task_count {
            state.kanban.focused_task_index.insert(col_id.clone(), new_idx as usize);
            update_focused_task_id(&mut state, &col_id);
        }
    }

    fn handle_create_task(&mut self) {
        let mut state = self.state.lock().unwrap();
        let col_id = state.ui.focused_column.clone();
        let available_columns: Vec<String> = self.config.columns.visible_column_ids().to_vec();
        state.open_task_editor_create(&col_id, available_columns);
    }

    fn handle_edit_task(&mut self) {
        let task_id = {
            let state = self.state.lock().unwrap();
            state.ui.focused_task_id.clone()
        };
        let mut state = self.state.lock().unwrap();
        match task_id {
            Some(id) => {
                let available_columns: Vec<String> = self.config.columns.visible_column_ids().to_vec();
                state.open_task_editor_edit(&id, available_columns);
            }
            None => state.set_notification(
                "No task selected to edit".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            ),
        }
    }

    /// Move the focused task forward or backward by `direction` columns (+1 or -1).
    fn handle_move_task(&mut self, direction: i32) {
        let visible = self.config.columns.visible_column_ids();
        let (task_id, current_col_idx) = {
            let state = self.state.lock().unwrap();
            let tid = state.ui.focused_task_id.clone();
            let idx = state.kanban.focused_column_index;
            (tid, idx)
        };
        let mut state = self.state.lock().unwrap();
        match task_id {
            Some(tid) => {
                let target_idx = current_col_idx as i32 + direction;
                if target_idx >= 0 && (target_idx as usize) < visible.len() {
                    let target_col = visible[target_idx as usize].clone();
                    state.move_task(&tid, crate::state::types::KanbanColumn(target_col.clone()));

                    // Trigger orchestration engine if the target column has an agent configured
                    if let Some(_agent) = self.config.columns.agent_for_column(&target_col) {
                        let already_running = state.tasks.get(&tid)
                            .map(|t| matches!(t.agent_status,
                                crate::state::types::AgentStatus::Running
                                | crate::state::types::AgentStatus::Hung))
                            .unwrap_or(false);
                        if !already_running {
                            if let Some(project_id) = state.active_project_id.clone() {
                                if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
                                    // Set status to Running while holding the lock to close the race window
                                    state.update_task_agent_status(&tid, crate::state::types::AgentStatus::Running);
                                    state.set_task_agent_type(&tid, self.config.columns.agent_for_column(&target_col));
                                    drop(state); // Release lock before spawning async
                                    crate::orchestration::engine::on_task_moved(
                                        &tid,
                                        &crate::state::types::KanbanColumn(target_col),
                                        &self.state,
                                        &client,
                                        &self.config.columns,
                                        &self.config.opencode,
                                    );
                                    return; // Lock already dropped
                                } else {
                                    state.set_notification(
                                        "No OpenCode client for this project".to_string(),
                                        crate::state::types::NotificationVariant::Warning,
                                        3000,
                                    );
                                }
                            } else {
                                state.set_notification(
                                    "No active project — agent dispatch skipped".to_string(),
                                    crate::state::types::NotificationVariant::Warning,
                                    3000,
                                );
                            }
                        }
                    }
                } else {
                    let msg = if direction > 0 {
                        "Already at the last column"
                    } else {
                        "Already at the first column"
                    };
                    state.set_notification(
                        msg.to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        2000,
                    );
                }
            }
            None => {
                state.set_notification(
                    "No task selected to move".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    2000,
                );
            }
        }
    }


    /// Reorder the focused task within its column by swapping with a neighbor.
    /// `direction` is -1 (move up) or +1 (move down).
    fn handle_reorder_task(&mut self, direction: i32) {
        let task_id = {
            let state = self.state.lock().unwrap();
            state.ui.focused_task_id.clone()
        };
        let mut state = self.state.lock().unwrap();
        match task_id {
            Some(tid) => {
                let moved = if direction < 0 {
                    state.reorder_task_up(&tid)
                } else {
                    state.reorder_task_down(&tid)
                };
                let (msg, variant) = if moved {
                    if direction < 0 {
                        ("Task moved up".to_string(), crate::state::types::NotificationVariant::Info)
                    } else {
                        ("Task moved down".to_string(), crate::state::types::NotificationVariant::Info)
                    }
                } else {
                    if direction < 0 {
                        ("Already at top".to_string(), crate::state::types::NotificationVariant::Warning)
                    } else {
                        ("Already at bottom".to_string(), crate::state::types::NotificationVariant::Warning)
                    }
                };
                state.set_notification(msg, variant, 1500);
            }
            None => {
                state.set_notification(
                    "No task selected to reorder".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
            }
        }
    }

    fn handle_delete_task(&mut self) {
        let task_id = {
            let state = self.state.lock().unwrap();
            state.ui.focused_task_id.clone()
        };
        let project_id = {
            let state = self.state.lock().unwrap();
            state.active_project_id.clone()
        };
        let mut state = self.state.lock().unwrap();
        match task_id {
            Some(tid) => {
                let deleted_session_id = state.delete_task(&tid);

                // Clamp focused task index for the column
                let col_id = state.ui.focused_column.clone();
                state.clamp_focused_task_index(&col_id);

                // Close detail view if viewing the deleted task
                if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                    state.close_task_detail();
                }

                state.set_notification(
                    "Task deleted".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    3000,
                );

                // Abort the remote session if one existed
                if let Some(session_id) = deleted_session_id {
                    if let Some(pid) = &project_id {
                        if let Some(client) = self.opencode_clients.get(pid).cloned() {
                            tokio::spawn(async move {
                                if let Err(e) = client.abort_session(&session_id).await {
                                    tracing::warn!("Failed to abort remote session {}: {}", session_id, e);
                                }
                            });
                        }
                    }
                }
            }
            None => {
                state.set_notification(
                    "No task selected to delete".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
            }
        }
    }

    fn handle_view_task(&mut self) {
        let task_id = {
            let state = self.state.lock().unwrap();
            state.ui.focused_task_id.clone()
        };
        let mut state = self.state.lock().unwrap();
        match task_id {
            Some(tid) => state.open_task_detail(&tid),
            None => state.set_notification(
                "No task selected to view".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            ),
        }
    }

    fn handle_abort_session(&mut self) {
        // Batch read: extract session_id and client in a single lock hold.
        let (session_id, client) = {
            let state = self.state.lock().unwrap();
            let session_id = state
                .ui
                .focused_task_id
                .as_ref()
                .and_then(|tid| state.tasks.get(tid))
                .and_then(|t| t.session_id.clone());
            let client = state
                .active_project_id
                .as_ref()
                .and_then(|pid| self.opencode_clients.get(pid))
                .cloned();
            (session_id, client)
        };

        if let Some(sid) = session_id {
            tracing::info!("Abort session requested: {}", sid);

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
        } else {
            let mut state = self.state.lock().unwrap();
            state.set_notification(
                "No active session to abort".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
    }

    /// Scroll the kanban view left or right without changing the focused column.
    /// Bound to PageUp (left) and PageDown (right) by default.
    fn handle_scroll_kanban(&mut self, direction: i32) {
        let total_cols = self.config.columns.visible_column_ids().len();
        if total_cols == 0 {
            return;
        }

        let max_visible = Self::max_visible_columns(&self.config, &self.terminal);
        if total_cols <= max_visible {
            return;
        }

        let mut state = self.state.lock().unwrap();
        let current = state.kanban.kanban_scroll_offset as i32;
        let max_offset = (total_cols.saturating_sub(max_visible)) as i32;
        let new_offset = (current + direction).clamp(0, max_offset);
        state.kanban.kanban_scroll_offset = new_offset as usize;
    }

    /// Handle key events in ConfirmDialog mode.
    ///
    /// `y` confirms the pending action, `n` or `Esc` cancels.
    fn handle_confirm_dialog_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        use crate::state::types::ConfirmableAction;

        match key.code {
            KeyCode::Char('y') => {
                // Confirm the pending action
                let action = {
                    let mut state = self.state.lock().unwrap();
                    state.ui.confirm_action.take()
                };
                if let Some(action) = action {
                    match action {
                        ConfirmableAction::DeleteProject(project_id) => {
                            // Collect active session IDs and project name before removing anything.
                            // We must abort remote sessions BEFORE destroying the client.
                            let (sessions_to_abort, project_name) = {
                                let state = self.state.lock().unwrap();
                                let project_name = state
                                    .projects
                                    .iter()
                                    .find(|p| p.id == project_id)
                                    .map(|p| p.name.clone())
                                    .unwrap_or_else(|| project_id.clone());
                                // Gather all session IDs for this project's tasks
                                let session_ids: Vec<String> = state.tasks.values()
                                    .filter(|t| t.project_id == project_id)
                                    .filter_map(|t| t.session_id.clone())
                                    .collect();
                                (session_ids, project_name)
                            };

                            // Abort all active sessions asynchronously using the client
                            // (which we still have at this point).
                            if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
                                let sessions = sessions_to_abort;
                                tokio::spawn(async move {
                                    for sid in &sessions {
                                        if let Err(e) = client.abort_session(sid).await {
                                            tracing::warn!(
                                                "Failed to abort session {} during project deletion: {}",
                                                sid, e
                                            );
                                        }
                                    }
                                });
                            }

                            // Now safe to remove the client
                            self.opencode_clients.remove(&project_id);

                            let mut state = self.state.lock().unwrap();
                            state.remove_project(&project_id);

                            // If there are remaining projects, select the first one.
                            let first_id = state.projects.first().map(|p| p.id.clone());
                            if let Some(id) = first_id {
                                state.select_project(&id);
                            }

                            state.set_notification(
                                format!("Project \"{}\" deleted", project_name),
                                crate::state::types::NotificationVariant::Info,
                                3000,
                            );
                        }
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                // Cancel — return to Normal mode
                let mut state = self.state.lock().unwrap();
                state.ui.confirm_action = None;
                state.ui.mode = crate::state::types::AppMode::Normal;
            }
            _ => {} // Ignore other keys
        }
    }

    // ── Shared helpers ──

    /// Switch to the previous/next project by an offset (-1 or +1).
    /// Wraps around at the boundaries.
    fn switch_project_offset(&mut self, direction: i32) {
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
        let new_idx = (current_idx as i32 + direction).rem_euclid(len as i32) as usize;
        let new_id = state.projects[new_idx].id.clone();
        state.select_project(&new_id);
    }

    /// Shared text-input key handler for single-line input prompts.
    ///
    /// Used by both the project-rename and working-directory prompts.
    /// Handles character insertion, backspace, delete, cursor movement,
    /// Home/End, Enter (submit), and Escape (cancel).
    fn handle_text_input(
        &mut self,
        key: crossterm::event::KeyEvent,
        prompt: InputPrompt,
    ) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                let mut state = self.state.lock().unwrap();
                match prompt {
                    InputPrompt::RenameProject => {
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
                    InputPrompt::WorkingDirectory => {
                        match state.submit_working_directory() {
                            Ok(true) => {
                                state.set_notification(
                                    "Working directory updated".to_string(),
                                    crate::state::types::NotificationVariant::Success,
                                    3000,
                                );
                            }
                            Ok(false) => {
                                state.set_notification(
                                    "Working directory cannot be empty".to_string(),
                                    crate::state::types::NotificationVariant::Warning,
                                    2000,
                                );
                            }
                            Err(msg) => {
                                state.set_notification(
                                    msg,
                                    crate::state::types::NotificationVariant::Error,
                                    3000,
                                );
                            }
                        }
                    }
                }
            }
            KeyCode::Esc => {
                let mut state = self.state.lock().unwrap();
                match prompt {
                    InputPrompt::RenameProject => state.cancel_project_rename(),
                    InputPrompt::WorkingDirectory => state.cancel_working_directory(),
                }
            }
            KeyCode::Char(c) => {
                let mut state = self.state.lock().unwrap();
                let char_count = state.ui.input_text.chars().count();
                let cursor = state.ui.input_cursor.min(char_count);
                // Convert char index to byte offset for insertion.
                let byte_pos = state.ui.input_text
                    .char_indices()
                    .nth(cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(state.ui.input_text.len());
                state.ui.input_text.insert(byte_pos, c);
                // The inserted char is exactly 1 char wide; advance cursor.
                state.ui.input_cursor = cursor + 1;
            }
            KeyCode::Backspace => {
                let mut state = self.state.lock().unwrap();
                if state.ui.input_cursor > 0 {
                    let cursor = state.ui.input_cursor;
                    // Find the byte range of the char just before the cursor.
                    let char_indices: Vec<(usize, char)> =
                        state.ui.input_text.char_indices().collect();
                    if let Some(&(byte_start, ch)) = char_indices.get(cursor - 1) {
                        let byte_end = byte_start + ch.len_utf8();
                        state.ui.input_text.replace_range(byte_start..byte_end, "");
                    }
                    state.ui.input_cursor = cursor - 1;
                }
            }
            KeyCode::Delete => {
                let mut state = self.state.lock().unwrap();
                let char_count = state.ui.input_text.chars().count();
                if state.ui.input_cursor < char_count {
                    let cursor = state.ui.input_cursor;
                    let char_indices: Vec<(usize, char)> =
                        state.ui.input_text.char_indices().collect();
                    if let Some(&(byte_start, ch)) = char_indices.get(cursor) {
                        let byte_end = byte_start + ch.len_utf8();
                        state.ui.input_text.replace_range(byte_start..byte_end, "");
                    }
                }
            }
            KeyCode::Left => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let mut state = self.state.lock().unwrap();
                let char_count = state.ui.input_text.chars().count();
                let new_pos = state.ui.input_cursor + 1;
                state.ui.input_cursor = new_pos.min(char_count);
            }
            KeyCode::Home => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = 0;
            }
            KeyCode::End => {
                let mut state = self.state.lock().unwrap();
                state.ui.input_cursor = state.ui.input_text.chars().count();
            }
            _ => {} // Ignore other keys
        }
    }

    /// Handle key events in ProjectRename mode.
    fn handle_rename_key(&mut self, key: crossterm::event::KeyEvent) {
        self.handle_text_input(key, InputPrompt::RenameProject);
    }

    /// Handle key events in InputPrompt mode (used for working directory).
    fn handle_input_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        self.handle_text_input(key, InputPrompt::WorkingDirectory);
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
                        // Extract column ID before closing editor
                        let column_id = state.get_task_editor()
                            .and_then(|ed| ed.column_id.clone());

                        // Close the editor and return to normal mode
                        state.cancel_task_editor();
                        state.set_notification(
                            format!("Task saved: {}", task_id),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );

                        // Focus the newly created/saved task
                        state.ui.focused_task_id = Some(task_id.clone());

                        // Update focused column to match the saved task's column
                        if let Some(ref col_id) = column_id {
                            let visible = self.config.columns.visible_column_ids();
                            if let Some(idx) = visible.iter().position(|c| c == col_id) {
                                state.ui.focused_column = col_id.clone();
                                state.kanban.focused_column_index = idx;
                            }
                        }

                        // Auto-launch agent if column has one configured
                        if let Some(ref col_id) = column_id {
                            if let Some(_agent) = self.config.columns.agent_for_column(col_id) {
                                // Check if task already has a running agent
                                let already_running = state.tasks.get(&task_id)
                                    .map(|t| matches!(t.agent_status,
                                        crate::state::types::AgentStatus::Running
                                        | crate::state::types::AgentStatus::Hung))
                                    .unwrap_or(false);

                                if !already_running {
                                    if let Some(project_id) = state.active_project_id.clone() {
                                        if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
                                            // Set status to Running while holding the lock to close the race window
                                            state.update_task_agent_status(&task_id, crate::state::types::AgentStatus::Running);
                                            state.set_task_agent_type(&task_id, self.config.columns.agent_for_column(col_id));
                                            drop(state); // Release lock before spawning async
                                            crate::orchestration::engine::on_task_moved(
                                                &task_id,
                                                &crate::state::types::KanbanColumn(col_id.clone()),
                                                &self.state,
                                                &client,
                                                &self.config.columns,
                                                &self.config.opencode,
                                            );
                                        } else {
                                            state.set_notification(
                                                "No OpenCode client for this project".to_string(),
                                                crate::state::types::NotificationVariant::Warning,
                                                3000,
                                            );
                                        }
                                    } else {
                                        state.set_notification(
                                            "No active project — agent dispatch skipped".to_string(),
                                            crate::state::types::NotificationVariant::Warning,
                                            3000,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Only show a notification toast if there's no inline
                        // validation error (which is already visible in the editor).
                        // Validation errors (e.g. empty description) are shown inline
                        // and don't need a transient notification.
                        let has_inline_error = state
                            .get_task_editor()
                            .map_or(false, |ed| ed.validation_error.is_some());
                        if !has_inline_error {
                            state.set_notification(
                                format!("Save failed: {}", e),
                                crate::state::types::NotificationVariant::Error,
                                3000,
                            );
                        }
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

    // ── Horizontal scroll helpers ──

    /// Calculate the maximum number of kanban columns that can fit.
    fn max_visible_columns(config: &CortexConfig, terminal: &Terminal) -> usize {
        let term_width = terminal
            .size()
            .unwrap_or(Size::new(80, 24))
            .width;
        let sidebar_width = config.theme.sidebar_width;
        let kanban_width = term_width.saturating_sub(sidebar_width);
        let available = kanban_width.saturating_sub(6);
        let col_width = config.theme.column_width;
        std::cmp::max(1, (available / col_width) as usize)
    }

    /// Ensure the focused column is visible by adjusting the scroll offset.
    fn ensure_column_visible(
        state: &mut AppState,
        config: &CortexConfig,
        terminal: &Terminal,
    ) {
        let total_cols = config.columns.visible_column_ids().len();
        if total_cols == 0 {
            return;
        }

        let max_visible = Self::max_visible_columns(config, terminal);

        if total_cols <= max_visible {
            state.kanban.kanban_scroll_offset = 0;
            return;
        }

        let focused = state.kanban.focused_column_index;
        let offset = &mut state.kanban.kanban_scroll_offset;

        if focused < *offset {
            *offset = focused;
        } else if focused >= *offset + max_visible {
            *offset = focused - max_visible + 1;
        }

        let max_offset = total_cols.saturating_sub(max_visible);
        *offset = (*offset).min(max_offset);
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
