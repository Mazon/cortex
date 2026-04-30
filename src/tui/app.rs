//! TUI App struct and event loop.

use crate::config::types::CortexConfig;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::AppState;
use crate::tui::{CrosstermBackend, Terminal};
use crossterm::event::{self, Event, KeyEventKind, MouseEvent};
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
    NewProjectDirectory,
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
    /// Pre-computed editor key matcher — built once from config, avoids per-keypress allocation.
    editor_key_matcher: crate::tui::keys::EditorKeyMatcher,
}

impl App {
    /// Setup the terminal: enable raw mode, mouse capture, and enter alternate screen.
    ///
    /// Call this early in `main()` (before server startup) to hide any
    /// residual log output from the primary terminal buffer.
    pub fn setup_terminal() -> anyhow::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::event::EnableMouseCapture,
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
        let editor_key_matcher =
            crate::tui::keys::EditorKeyMatcher::from_config(&config.keybindings.editor);

        Ok(Self {
            state,
            config,
            terminal,
            should_quit: false,
            opencode_clients,
            key_matcher,
            editor_key_matcher,
        })
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(self.config.theme.tick_rate_ms);

        // Set up graceful shutdown via a background signal-handler task.
        // Listens for SIGINT (Ctrl+C) and SIGTERM, then notifies the event
        // loop so `should_quit` is set and the existing shutdown sequence in
        // main.rs runs cleanly (save state, stop servers, teardown terminal)
        // instead of leaving the terminal in raw mode / alternate screen.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                let mut sigterm =
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    {
                        Ok(s) => s,
                        Err(_e) => {
                            let _ = tokio::signal::ctrl_c().await;
                            let _ = shutdown_tx.send(()).await;
                            return;
                        }
                    };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                    }
                    _ = sigterm.recv() => {
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
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
                                            .unwrap_or_else(|e| e.into_inner())
                                            .mark_render_dirty();
                                    }
                                    Event::Mouse(mouse) => {
                                        self.handle_mouse_event(mouse);
                                    }
                                    _ => {} // Ignore paste events
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.clear_expired_notifications() {
                    state.mark_render_dirty();
                }
                if state.clear_expired_highlight() {
                    state.mark_render_dirty();
                }
            }

            // Periodic hung-agent detection
            {
                let timeout_secs = self.config.opencode.hung_agent_timeout_secs as i64;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let newly_hung = state.check_hung_agents(timeout_secs);
                if newly_hung > 0 {
                    state.set_notification(
                        format!("{} task(s) marked as Hung — no activity for {}s", newly_hung, timeout_secs),
                        crate::state::types::NotificationVariant::Warning,
                        5000,
                    );
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
            //
            // Alternative approaches considered:
            // - Snapshot approach: clone AppState before draw, release lock, then
            //   draw from snapshot. This reduces contention but adds clone overhead
            //   (~μs for a typical state) and complexity. Given the draw cycle is
            //   ~16 ms at 60 fps and the lock is held for <1 ms, the snapshot
            //   approach is unnecessary.
            // - Fine-grained locking: separate Mutex per subsystem. Adds
            //   significant complexity with marginal benefit.
            //
            // The current approach is correct and performant for this application.
            let needs_render = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take_render_dirty();
            if needs_render {
                let config = &self.config;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // Auto-open permission modal when permissions/questions arrive
                if !state.ui.permission_modal_active
                    && state.ui.mode == crate::state::types::AppMode::Normal
                    && state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                {
                    if let Some(ref tid) = state.ui.viewing_task_id {
                        // Check both the main task session and the drilled-in
                        // subagent session for pending permissions/questions.
                        let main_has_pending = state
                            .session_tracker
                            .task_sessions
                            .get(tid)
                            .map(|s| {
                                !s.pending_permissions.is_empty()
                                    || !s.pending_questions.is_empty()
                            })
                            .unwrap_or(false);
                        let sub_has_pending = state
                            .get_drilldown_session_id()
                            .and_then(|sid| {
                                state
                                    .session_tracker
                                    .subagent_session_data
                                    .get(sid)
                            })
                            .map(|s| {
                                !s.pending_permissions.is_empty()
                                    || !s.pending_questions.is_empty()
                            })
                            .unwrap_or(false);
                        if main_has_pending || sub_has_pending {
                            state.ui.permission_modal_active = true;
                            state.ui.permission_modal_selected_index = 0;
                        }
                    }
                }
                self.terminal.draw(|f| {
                    let state = &mut *state;
                    match state.ui.mode {
                        crate::state::types::AppMode::Normal => {
                            crate::tui::render_normal(f, state, config);
                            // Render permission/question modal overlay if active
                            if state.ui.permission_modal_active {
                                crate::tui::permission_modal::render_permission_modal(
                                    f, f.area(), state, &config.theme,
                                );
                                // Re-render the status bar on top of the modal so
                                // notifications (e.g. API errors) remain visible to the user.
                                let area = f.area();
                                let v_constraints = ratatui::layout::Constraint::from_lengths([
                                    0, // content area (min, we just need the status bar)
                                    2, // status bar height
                                ]);
                                let v_layout = ratatui::layout::Layout::default()
                                    .direction(ratatui::layout::Direction::Vertical)
                                    .constraints(v_constraints)
                                    .split(area);
                                let status_area = v_layout[1].inner(
                                    ratatui::layout::Margin { horizontal: 1, vertical: 0 },
                                );
                                crate::tui::status_bar::render_status_bar(
                                    f, status_area, state, &config.theme,
                                );
                            }
                        }
                        crate::state::types::AppMode::TaskEditor => {
                            crate::tui::task_editor::render_task_editor(f, state, config);
                        }
                        crate::state::types::AppMode::Help => {
                            crate::tui::render_normal(f, state, config);
                            let help_tab = state.ui.help_tab;
                            crate::tui::help::render_help_overlay(f, &config.keybindings, help_tab);
                        }
                        crate::state::types::AppMode::ProjectRename => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::prompt::render_input_prompt(f, state);
                        }
                        crate::state::types::AppMode::InputPrompt => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::prompt::render_input_prompt(f, state);
                        }
                        crate::state::types::AppMode::DiffReview => {
                            crate::tui::diff_view::render_diff_review(
                                f,
                                f.area(),
                                state,
                                &config.theme,
                            );
                        }
                        crate::state::types::AppMode::Reports => {
                            crate::tui::reports::render_reports(
                                f,
                                f.area(),
                                state,
                                &config.theme,
                            );
                        }
                    }
                })?;
            }
        }

        Ok(())
    }

    /// Handle a mouse event — left-click to focus tasks and columns.
    ///
    /// Supports:
    /// - Click on a kanban column header → focus that column
    /// - Click on a task card → focus that task
    /// - Scroll wheel → navigate tasks up/down within the focused column
    fn handle_mouse_event(&mut self, mouse: MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        // We only handle left-button press (not release, drag, etc.)
        let MouseEventKind::Down(MouseButton::Left) = mouse.kind else {
            // Handle scroll wheel for task navigation
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.handle_nav_task(-1);
                    return;
                }
                MouseEventKind::ScrollDown => {
                    self.handle_nav_task(1);
                    return;
                }
                _ => return,
            }
        };

        let area = match self.terminal.size() {
            Ok(size) => size,
            Err(_) => return,
        };

        let sidebar_width = self.config.theme.sidebar_width;
        let col_width = self.config.theme.column_width;

        // Ignore clicks in the sidebar area
        if mouse.column < sidebar_width {
            return;
        }

        // Ignore clicks in the status bar (last 2 rows: top border + content)
        if mouse.row >= area.height.saturating_sub(2) {
            return;
        }

        let kanban_x = mouse.column - sidebar_width;
        let visible = self.config.columns.visible_column_ids();

        // Account for scroll indicators
        let available_for_columns = area.width.saturating_sub(sidebar_width).saturating_sub(6);
        let max_visible = std::cmp::max(1, (available_for_columns / col_width) as usize);
        let can_show_all = visible.len() <= max_visible;

        let has_left_indicator = !can_show_all && {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.kanban.kanban_scroll_offset > 0
        };

        let scroll_offset = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if can_show_all {
                0
            } else {
                state
                    .kanban
                    .kanban_scroll_offset
                    .min(visible.len().saturating_sub(max_visible))
            }
        };

        let x_offset: u16 = if has_left_indicator { 3 } else { 0 };

        // Determine which column was clicked
        let col_index = if kanban_x >= x_offset {
            ((kanban_x - x_offset) / col_width) as usize
        } else {
            return;
        };

        if col_index >= max_visible || col_index + scroll_offset >= visible.len() {
            return;
        }

        let clicked_col_id = &visible[col_index + scroll_offset];

        // Determine if the click was on the column header (row 0 or 1)
        // or on a task card (row >= 2)
        let is_header_click = mouse.row < 2;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // Always focus the clicked column
        let col_idx = col_index + scroll_offset;
        if let Some(col_id) = visible.get(col_idx) {
            state.kanban.focused_column_index = col_idx;
            state.set_focused_column(col_id);
        }
        Self::ensure_column_visible(&mut state, &self.config, &self.terminal);

        if is_header_click {
            // Click on column header — just focus the column (already done above)
            state.mark_render_dirty();
        } else {
            // Click in the task area — determine which task was clicked
            // Tasks start at row 2 (after the header), each task card is 6 rows
            // (5 rows for card + 1 row gap)
            let task_row = (mouse.row - 2) as usize;
            let card_height = 6usize; // 5 rows for card + 1 row gap
            let task_index = task_row / card_height;

            let task_id = {
                let task_ids = state.kanban.columns.get(clicked_col_id.as_str());
                if let Some(task_ids) = task_ids {
                    if task_index < task_ids.len() {
                        task_ids.get(task_index).cloned()
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(task_id) = task_id {
                state
                    .kanban
                    .focused_task_index
                    .insert(clicked_col_id.clone(), task_index);
                state.ui.focused_task_id = Some(task_id);
            }
            state.mark_render_dirty();
        }
    }

    /// Handle a key event based on current mode.
    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        // Any key press potentially changes state — mark for re-render.
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_render_dirty();

        let mode = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                use crate::state::types::HelpTab;
                use crossterm::event::{KeyCode, KeyModifiers};
                match (key.code, key.modifiers) {
                    // Tab or Right/l → next tab
                    (KeyCode::Tab, KeyModifiers::NONE)
                    | (KeyCode::Right, KeyModifiers::NONE)
                    | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = state.ui.help_tab.next();
                    }
                    // Shift+Tab or Left/h → previous tab
                    (KeyCode::BackTab, _)
                    | (KeyCode::Left, KeyModifiers::NONE)
                    | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = state.ui.help_tab.prev();
                    }
                    // Number keys 1-4 → jump directly to tab
                    (KeyCode::Char('1'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = HelpTab::Global;
                    }
                    (KeyCode::Char('2'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = HelpTab::Kanban;
                    }
                    (KeyCode::Char('3'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = HelpTab::Review;
                    }
                    (KeyCode::Char('4'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.help_tab = HelpTab::Editor;
                    }
                    _ => {
                        // Any other key dismisses help
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.mode = crate::state::types::AppMode::Normal;
                    }
                }
            }
            crate::state::types::AppMode::ProjectRename => {
                self.handle_rename_key(key);
            }
            crate::state::types::AppMode::InputPrompt => {
                self.handle_input_prompt_key(key);
            }
            crate::state::types::AppMode::DiffReview => {
                self.handle_diff_review_key(key);
            }
            crate::state::types::AppMode::Reports => {
                self.handle_reports_key(key);
            }
        }
    }

    /// Handle key events in Normal mode.
    ///
    /// Resolves the key to an [`Action`] via the configured keybindings and
    /// dispatches to the appropriate handler method.
    fn handle_normal_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // ── Detail editor inline editing ──────────────────────────────
        // When the detail editor is focused, intercept editing keys.
        // This must come before all other handlers to prevent key conflicts.
        {
            let is_detail_editor_focused = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                    && state
                        .ui
                        .detail_editor
                        .as_ref()
                        .map_or(false, |e| e.is_focused)
            };

            if is_detail_editor_focused {
                use crate::state::types::{AgentStatus, CursorDirection};
                use crate::tui::keys::EditorKeyAction;

                // Check configurable editor keybindings first (Ctrl+S, Esc, Tab, Enter)
                let editor_action = {
                    let _state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    self.editor_key_matcher.match_key(key)
                };

                if let Some(action) = editor_action {
                    match action {
                        EditorKeyAction::Save => {
                            // Ctrl+S in detail view: submit the prompt (same as Enter)
                            // Reuse the same logic by treating this as a submit
                            // Fall through to Newline handler by not returning early
                            // Actually, we need to duplicate the submit logic here
                            // since Ctrl+S is handled separately from Enter.
                            // For simplicity, just unfocus the editor.
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.is_focused = false;
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        EditorKeyAction::Cancel => {
                            // Esc: unfocus the prompt input
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.is_focused = false;
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        EditorKeyAction::CycleField => {
                            // Tab: toggle focus on/off
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                if ed.discard_warning_shown {
                                    // Clear warning on Tab
                                    ed.discard_warning_shown = false;
                                }
                                ed.is_focused = false;
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        EditorKeyAction::Submit => {
                            // Submit is handled via Newline (Enter) in the detail view
                            // This case shouldn't be reached since Submit isn't mapped
                            // to a separate key in the EditorKeyMatcher
                            return;
                        }
                        EditorKeyAction::Newline => {
                            // Enter: submit prompt (only without ctrl/alt modifiers)
                            if key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            {
                                // Ctrl+Enter = save description, don't submit prompt
                                return;
                            }
                            // Submit the prompt from the input field
                            {
                                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                                let (prompt_text, task_id, agent_status, session_id, agent_type) = {
                                    let task_id = state.ui.viewing_task_id.clone();
                                    let prompt = state
                                        .ui
                                        .detail_editor
                                        .as_ref()
                                        .map(|ed| ed.description())
                                        .unwrap_or_default();
                                    let prompt = prompt.trim().to_string();
                                    let (status, sid, agent) =
                                        task_id.as_ref().and_then(|tid| {
                                            state.tasks.get(tid).map(|t| {
                                                (
                                                    t.agent_status.clone(),
                                                    t.session_id.clone(),
                                                    t.agent_type.clone(),
                                                )
                                            })
                                        }).unwrap_or((AgentStatus::Pending, None, None));
                                    (prompt, task_id, status, sid, agent)
                                };

                                if prompt_text.is_empty() {
                                    // Don't submit empty prompts
                                    drop(state);
                                    return;
                                }

                                let task_id = match task_id {
                                    Some(id) => id,
                                    None => {
                                        drop(state);
                                        return;
                                    }
                                };

                                match agent_status {
                                    AgentStatus::Running | AgentStatus::Pending => {
                                        // Agent is running — queue the prompt
                                        if let Some(task) = state.tasks.get_mut(&task_id) {
                                            task.queued_prompt = Some(prompt_text.clone());
                                        }
                                        // Clear the input field
                                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                                            ed.desc_lines = vec![String::new()];
                                            ed.cached_description = None;
                                            ed.cursor_row = 0;
                                            ed.cursor_col = 0;
                                            ed.has_unsaved_changes = false;
                                            ed.is_focused = false;
                                        }
                                        state.set_notification(
                                            "Prompt queued — will be sent after current prompt completes".to_string(),
                                            crate::state::types::NotificationVariant::Info,
                                            3000,
                                        );
                                        state.mark_render_dirty();
                                    }
                                    AgentStatus::Ready | AgentStatus::Complete | AgentStatus::Question => {
                                        // Agent is idle — send immediately
                                        if session_id.is_some() {
                                            // Set active_prompt on the session
                                            if let Some(ref sid) = session_id {
                                                if let Some(session) =
                                                    state.session_tracker.task_sessions.get_mut(sid)
                                                {
                                                    session.active_prompt = Some(prompt_text.clone());
                                                    session.render_version += 1;
                                                }
                                            }
                                            // Update agent status to Running
                                            state.update_task_agent_status(&task_id, AgentStatus::Running);
                                            // Clear the input field
                                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                                ed.desc_lines = vec![String::new()];
                                                ed.cached_description = None;
                                                ed.cursor_row = 0;
                                                ed.cursor_col = 0;
                                                ed.has_unsaved_changes = false;
                                                ed.is_focused = false;
                                            }
                                            state.set_notification(
                                                "Sending prompt...".to_string(),
                                                crate::state::types::NotificationVariant::Info,
                                                2000,
                                            );
                                            state.mark_render_dirty();

                                            // Spawn async task to send the prompt
                                            let state_clone = self.state.clone();
                                            let sid = session_id.unwrap();
                                            let agent = agent_type.clone();
                                            let opencode_config = self.config.opencode.clone();
                                            let prompt = prompt_text;

                                            // Get the client for this task's project
                                            let client = {
                                                let pid = state
                                                    .tasks
                                                    .get(&task_id)
                                                    .map(|t| t.project_id.clone());
                                                drop(state);
                                                pid.and_then(|pid| self.opencode_clients.get(&pid).cloned())
                                            };

                                            if let Some(client) = client {
                                                tokio::spawn(async move {
                                                    let agent_model = opencode_config
                                                        .agents
                                                        .get(agent.as_deref().unwrap_or(""))
                                                        .and_then(|a| a.model.clone());
                                                    let model = agent_model
                                                        .as_deref()
                                                        .map(|m| {
                                                            if m.contains('/') {
                                                                m.to_string()
                                                            } else {
                                                                let provider = opencode_config
                                                                    .model
                                                                    .provider
                                                                    .as_deref()
                                                                    .unwrap_or("z.ai");
                                                                format!("{}/{}", provider, m)
                                                            }
                                                        })
                                                        .or_else(|| {
                                                            let provider = opencode_config
                                                                .model
                                                                .provider
                                                                .as_deref()
                                                                .unwrap_or("z.ai");
                                                            Some(format!(
                                                                "{}/{}",
                                                                provider,
                                                                opencode_config.model.id
                                                            ))
                                                        });

                                                    match client
                                                        .send_prompt(
                                                            &sid,
                                                            &prompt,
                                                            agent.as_deref(),
                                                            model.as_deref(),
                                                        )
                                                        .await
                                                    {
                                                        Ok(_) => {
                                                            tracing::debug!(
                                                                "Follow-up prompt sent: session={}",
                                                                sid
                                                            );
                                                        }
                                                        Err(e) => {
                                                            tracing::error!(
                                                                "Failed to send follow-up prompt: {}",
                                                                e
                                                            );
                                                            let mut s = state_clone
                                                                .lock()
                                                                .unwrap_or_else(|e| e.into_inner());
                                                            s.set_notification(
                                                                format!("Failed to send prompt: {}", e),
                                                                crate::state::types::NotificationVariant::Error,
                                                                5000,
                                                            );
                                                            s.mark_render_dirty();
                                                        }
                                                    }
                                                });
                                            }
                                        } else {
                                            // No session — can't send
                                            state.set_notification(
                                                "No active session — cannot send prompt".to_string(),
                                                crate::state::types::NotificationVariant::Warning,
                                                3000,
                                            );
                                            state.mark_render_dirty();
                                        }
                                    }
                                    AgentStatus::Error => {
                                        state.set_notification(
                                            "Cannot send prompt to errored agent".to_string(),
                                            crate::state::types::NotificationVariant::Warning,
                                            3000,
                                        );
                                        state.mark_render_dirty();
                                    }
                                    AgentStatus::Hung => {
                                        state.set_notification(
                                            "Agent appears hung — try aborting first (ctrl+a)".to_string(),
                                            crate::state::types::NotificationVariant::Warning,
                                            5000,
                                        );
                                        state.mark_render_dirty();
                                    }
                                }
                            }
                            return;
                        }
                    }
                } else {
                    // Handle non-configurable editing keys
                    match (key.code, key.modifiers) {
                        // Arrow keys
                        (KeyCode::Up, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::Up);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::Down, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::Down);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::Left, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::Left);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::Right, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::Right);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::Home, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::Home);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::End, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.move_cursor(CursorDirection::End);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::PageUp, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.scroll_offset = ed.scroll_offset.saturating_sub(5);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        (KeyCode::PageDown, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.scroll_offset = ed.scroll_offset + 5;
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        // Backspace
                        (KeyCode::Backspace, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.delete_char_back();
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        // Delete
                        (KeyCode::Delete, KeyModifiers::NONE) => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.delete_char_forward();
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        // Printable characters
                        (KeyCode::Char(ch), modifiers)
                            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.insert_char(ch);
                            }
                            state.mark_render_dirty();
                            return;
                        }
                        _ => {
                            // Unrecognized key while editor is focused — consume it
                            return;
                        }
                    }
                }
            }

            // When in detail view but editor is NOT focused, handle Tab to focus
            let is_detail_not_focused = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                    && !state.is_drilled_into_subagent()
                    && state
                        .ui
                        .detail_editor
                        .as_ref()
                        .map_or(false, |e| !e.is_focused)
            };
            if is_detail_not_focused && key.code == KeyCode::Tab && key.modifiers.is_empty() {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // Cycle focus: output scroll → changed files (if available) → prompt input
                let has_changed_files = state
                    .ui
                    .changed_files
                    .as_ref()
                    .map_or(false, |f| !f.is_empty());
                if has_changed_files && !state.ui.changed_files_focused {
                    state.ui.changed_files_focused = true;
                } else {
                    state.ui.changed_files_focused = false;
                    if let Some(ed) = state.ui.detail_editor.as_mut() {
                        ed.is_focused = true;
                    }
                }
                state.mark_render_dirty();
                return;
            }
        }

        // Handle changed-files sidebar focus: intercept keys when it's focused
        {
            let is_changed_files_focused = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                    && state.ui.changed_files_focused
                    && !state
                        .ui
                        .detail_editor
                        .as_ref()
                        .map_or(false, |e| e.is_focused)
            };
            if is_changed_files_focused {
                use crossterm::event::KeyCode;
                match (key.code, key.modifiers) {
                    (KeyCode::Up, KeyModifiers::NONE)
                    | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.ui.selected_changed_file_index > 0 {
                            state.ui.selected_changed_file_index -= 1;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Down, KeyModifiers::NONE)
                    | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        let max = state
                            .ui
                            .changed_files
                            .as_ref()
                            .map_or(0, |f| f.len().saturating_sub(1));
                        if state.ui.selected_changed_file_index < max {
                            state.ui.selected_changed_file_index += 1;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    (KeyCode::Enter, KeyModifiers::NONE) => {
                        // Open diff review for the selected file
                        self.handle_open_file_diff();
                        return;
                    }
                    (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::Esc, KeyModifiers::NONE) => {
                        // Unfocus changed files → focus prompt input
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.changed_files_focused = false;
                        if let Some(ed) = state.ui.detail_editor.as_mut() {
                            ed.is_focused = true;
                        }
                        state.mark_render_dirty();
                        return;
                    }
                    _ => {
                        // Consume other keys while focused on changed files
                        return;
                    }
                }
            }
        }

        // ── Modal key interception ────────────────────────────────────
        {
            let modal_active = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.permission_modal_active
            };
            if modal_active {
                use crossterm::event::KeyCode;
                match key.code {
                    // Arrow keys / vim keys for navigation
                    KeyCode::Up | KeyCode::Char('k') => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.ui.permission_modal_selected_index > 0 {
                            state.ui.permission_modal_selected_index -= 1;
                            state.mark_render_dirty();
                        }
                        return;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        let max_options = self.get_modal_option_count(&state);
                        if state.ui.permission_modal_selected_index + 1 < max_options {
                            state.ui.permission_modal_selected_index += 1;
                            state.mark_render_dirty();
                        }
                        return;
                    }
                    // Enter — execute selected option
                    KeyCode::Enter => {
                        self.handle_modal_confirm();
                        return;
                    }
                    // Esc — close modal
                    KeyCode::Esc => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.ui.permission_modal_active = false;
                        state.ui.permission_modal_selected_index = 0;
                        state.mark_render_dirty();
                        return;
                    }
                    // Quick shortcuts
                    KeyCode::Char('y') => {
                        // Quick-approve (only for permissions)
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if self.has_pending_permission(&state) {
                            state.ui.permission_modal_selected_index = 0; // Yes
                            drop(state);
                            self.handle_modal_confirm();
                        }
                        return;
                    }
                    KeyCode::Char('n') => {
                        // Quick-reject (only for permissions)
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if self.has_pending_permission(&state) {
                            state.ui.permission_modal_selected_index = 1; // No
                            drop(state);
                            self.handle_modal_confirm();
                        }
                        return;
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        // Quick-select answer by number (only for questions)
                        let idx = (c as usize) - ('1' as usize);
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if !self.has_pending_permission(&state)
                            && idx < self.get_modal_option_count(&state)
                        {
                            state.ui.permission_modal_selected_index = idx;
                            drop(state);
                            self.handle_modal_confirm();
                        }
                        return;
                    }
                    _ => {
                        // Consume all other keys while modal is active
                        return;
                    }
                }
            }
        }

        // Check if we're in task detail view — Escape pops subagent stack or closes detail
        {
            let is_detail_escape = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                    && key.code == KeyCode::Esc
            };
            // First lock dropped here
            if is_detail_escape {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // If drilled into a subagent, pop back one level
                if state.is_drilled_into_subagent() {
                    state.pop_subagent_drilldown();
                    return;
                }
                // Otherwise, close the task detail view
                state.close_task_detail();
                return;
            }

            // Handle Up/Down arrows, j/k, and G/g for scrolling output in task detail view
            let is_scroll_key = matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Char('j')
                    | KeyCode::Char('k')
                    | KeyCode::Char('G')
                    | KeyCode::Char('g')
            );
            let modifiers_ok = if matches!(key.code, KeyCode::Char('G')) {
                key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
            } else {
                key.modifiers.is_empty()
            };
            if is_scroll_key && modifiers_ok {
                let in_detail = {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                };
                if in_detail {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let total_lines = state
                        .ui
                        .viewing_task_id
                        .as_ref()
                        .and_then(|tid| state.session_tracker.cached_streaming_lines.get(tid))
                        .map(|(_, lines)| lines.len())
                        .unwrap_or(0);

                    let max_offset = total_lines.saturating_sub(1);

                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if total_lines > 0 {
                                let current = state.ui.user_scroll_offset.unwrap_or(0);
                                let new_offset = current.saturating_sub(1);
                                state.ui.user_scroll_offset = Some(new_offset);
                                state.mark_render_dirty();
                            }
                            return;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
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
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                        (None, None, None)
                    } else if let Some(ref tid) = state.ui.viewing_task_id {
                        let session = self.get_effective_session(&state);
                        let perm = session
                            .and_then(|s| s.pending_permissions.first().cloned());
                        let client = state
                            .project_registry
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
                        tokio::spawn(async move {
                            match client
                                .resolve_permission(&session_id, &perm_id, approve)
                                .await
                            {
                                Ok(()) => {
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.resolve_permission_request(&tid, &perm_id, approve);
                                    s.mark_render_dirty();
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to resolve permission {}: {}",
                                        perm_id,
                                        e
                                    );
                                    // Keep the permission in the pending list so the user can retry
                                    // Note: tracing layer also captures this error automatically
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                        (None, None, None)
                    } else if let Some(ref tid) = state.ui.viewing_task_id {
                        let session = self.get_effective_session(&state);
                        let question = session
                            .and_then(|s| s.pending_questions.first().cloned())
                            .filter(|q| answer_index < q.answers.len());
                        let client = state
                            .project_registry
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
                        let columns_config = self.config.columns.clone();
                        let opencode_config = self.config.opencode.clone();
                        let question_id = question.id.clone();
                        let session_id = question.session_id.clone();
                        let answer_preview = answer.chars().take(30).collect::<String>();
                        tokio::spawn(async move {
                            match client
                                .resolve_question(&session_id, &question_id, &answer)
                                .await
                            {
                                Ok(()) => {
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.resolve_question_request(&tid, &question_id);

                                    // Check if the task should transition out of Question status
                                    let needs_reassess = s.should_reassess_after_question(&tid);
                                    if needs_reassess {
                                        // Determine Ready vs Complete + whether to auto-progress
                                        // (same logic as SessionIdle handler in dispatch.rs).
                                        let (status, should_progress) =
                                            crate::opencode::events::determine_completion_status(
                                                &mut s, &tid,
                                            );
                                        s.update_task_agent_status(&tid, status);

                                        if should_progress {
                                            let action =
                                                crate::orchestration::engine::on_agent_completed(
                                                    &tid,
                                                    &mut s,
                                                    &columns_config,
                                                );
                                            if let Some(a) = action {
                                                match a {
                                                    crate::orchestration::engine::AgentCompletionAction::AutoProgress(ap) => {
                                                        let col = ap.target_column.clone();
                                                        let tid_clone = tid.clone();
                                                        drop(s);
                                                        crate::orchestration::engine::on_task_moved(
                                                            &tid_clone,
                                                            &col,
                                                            &state,
                                                            &client,
                                                            &columns_config,
                                                            &opencode_config,
                                                            None,
                                                        );
                                                    }
                                                    crate::orchestration::engine::AgentCompletionAction::SendQueuedPrompt {
                                                        task_id: qp_tid,
                                                        prompt: qp_prompt,
                                                        session_id: qp_sid,
                                                        agent_type: qp_agent,
                                                    } => {
                                                        drop(s);
                                                        crate::orchestration::engine::send_follow_up_prompt(
                                                            &qp_tid,
                                                            &qp_prompt,
                                                            &qp_sid,
                                                            &qp_agent,
                                                            &state,
                                                            &client,
                                                            &opencode_config,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
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
                                        question_id,
                                        e
                                    );
                                    // Note: tracing layer also captures this error automatically
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                        if let Some(ref tid) = state.ui.viewing_task_id {
                            if state
                                .session_tracker
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

        // Handle vim-style keys that bypass the configurable keybinding system
        match (key.code, key.modifiers) {
            // Ctrl+R — reset circuit breaker for active project
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let pid = state.project_registry.active_project_id.clone();
                if let Some(ref pid) = pid {
                    let was_tripped = state.project_registry.is_circuit_breaker_tripped(
                        pid,
                        self.config.opencode.circuit_breaker_threshold,
                    );
                    state.project_registry.reset_circuit_breaker(pid);
                    if was_tripped {
                        state.set_notification(
                            "Circuit breaker reset — auto-progression resumed.".to_string(),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );
                    } else {
                        state.set_notification(
                            "Circuit breaker was not tripped.".to_string(),
                            crate::state::types::NotificationVariant::Info,
                            2000,
                        );
                    }
                }
                return;
            }
            _ => {}
        }

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
            Some(Action::OpenTaskDetail) => self.handle_open_task_detail(),
            Some(Action::MoveForward) => self.handle_move_task(1),
            Some(Action::MoveBackward) => self.handle_move_task(-1),
            Some(Action::DeleteTask) => self.handle_delete_task(),
            Some(Action::AbortSession) => self.handle_abort_session(),
            Some(Action::RetryTask) => self.handle_retry_task(),
            Some(Action::DrillDownSubagent) => self.handle_drill_down_subagent(),
            Some(Action::ReviewChanges) => self.handle_review_changes(),
            Some(Action::AcceptReview) => self.handle_accept_review(),
            Some(Action::RejectReview) => self.handle_reject_review(),
            Some(Action::Reports) => self.handle_reports_toggle(),
            None => {} // Unmatched key, ignore
        }
    }

    // ── Individual action handlers (extracted from handle_normal_key) ──

    fn handle_quit(&mut self) {
        self.should_quit = true;
    }

    fn handle_help_toggle(&mut self) {
        use crate::state::types::HelpTab;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ui.help_tab = HelpTab::Global;
        state.ui.mode = crate::state::types::AppMode::Help;
    }

    fn handle_prev_project(&mut self) {
        self.switch_project_offset(-1);
    }

    fn handle_next_project(&mut self) {
        self.switch_project_offset(1);
    }

    fn handle_new_project(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.open_new_project_directory();
    }

    fn handle_rename_project(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.open_project_rename();
    }

    fn handle_set_working_directory(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.open_set_working_directory();
    }

    fn handle_delete_project(&mut self) {
        // Collect project ID, name, and session IDs while holding the lock.
        let (project_id, project_name, sessions_to_abort) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match state.project_registry.active_project_id.as_ref() {
                Some(pid) => {
                    let name = state
                        .project_registry
                        .projects
                        .iter()
                        .find(|p| &p.id == pid)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| pid.clone());
                    // Gather all session IDs for this project's tasks
                    let session_ids: Vec<String> = state
                        .tasks
                        .values()
                        .filter(|t| t.project_id == *pid)
                        .filter_map(|t| t.session_id.clone())
                        .collect();
                    (Some(pid.clone()), name, session_ids)
                }
                None => (None, String::new(), Vec::new()),
            }
        };

        let Some(project_id) = project_id else {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No active project to delete".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
            return;
        };

        // Abort all active sessions asynchronously using the client
        // (which we still have at this point).
        if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
            tokio::spawn(async move {
                for sid in &sessions_to_abort {
                    if let Err(_e) = client.abort_session(sid).await {}
                }
            });
        }

        // Now safe to remove the client
        self.opencode_clients.remove(&project_id);

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.remove_project(&project_id);

        // If there are remaining projects, select the first one.
        if let Some(id) = state
            .project_registry
            .projects
            .first()
            .map(|p| p.id.clone())
        {
            state.select_project(&id);
        }

        state.set_notification(
            format!("Project \"{}\" deleted", project_name),
            crate::state::types::NotificationVariant::Info,
            3000,
        );

        // If the user just deleted the last project, show a prominent notification.
        if state.project_registry.projects.is_empty() {
            state.set_notification(
                "All projects deleted. Press Ctrl+N to create a new one.".to_string(),
                crate::state::types::NotificationVariant::Info,
                10000,
            );
        }
    }

    /// Move the focused column left or right by `direction` (-1 or +1).
    /// Auto-scrolls the kanban view to keep the focused column visible.
    fn handle_nav_column(&mut self, direction: i32) {
        let visible = self.config.columns.visible_column_ids();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let col_id = state.ui.focused_column.clone();
        let task_count = state
            .kanban
            .columns
            .get(&col_id)
            .map(|v| v.len())
            .unwrap_or(0);
        let current = state
            .kanban
            .focused_task_index
            .get(&col_id)
            .copied()
            .unwrap_or(0);
        let new_idx = current as i32 + direction;
        if new_idx >= 0 && (new_idx as usize) < task_count {
            state
                .kanban
                .focused_task_index
                .insert(col_id.clone(), new_idx as usize);
            update_focused_task_id(&mut state, &col_id);
        }
    }

    fn handle_create_task(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let col_id = state.ui.focused_column.clone();
        state.open_task_editor_create(&col_id);
    }

    fn handle_open_task_detail(&mut self) {
        use crate::state::types::FocusedPanel;

        let (task_id, is_reviewable, working_dir) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let tid = state.ui.focused_task_id.clone();

            let (reviewable, wd) = tid.as_ref().and_then(|id| {
                let task = state.tasks.get(id)?;
                let reviewable = matches!(
                    task.agent_status,
                    crate::state::types::AgentStatus::Complete
                        | crate::state::types::AgentStatus::Ready
                );
                let project = state
                    .project_registry
                    .projects
                    .iter()
                    .find(|p| p.id == task.project_id);
                let wd = project
                    .filter(|p| !p.working_directory.is_empty())
                    .map(|p| p.working_directory.clone());
                Some((reviewable, wd))
            }).unwrap_or((false, None));

            (tid, reviewable, wd)
        };

        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match task_id {
                Some(ref id) => {
                    state.open_task_detail(id);
                    // Set diff review source so Esc returns to task detail
                    state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                }
                None => state.set_notification(
                    "No task selected".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                ),
            }
        }

        // Async load changed files for reviewable tasks
        if is_reviewable {
            if let (Some(tid), Some(wd)) = (task_id, working_dir) {
                let state = self.state.clone();
                tokio::task::spawn_blocking(move || {
                    let numstat = std::process::Command::new("git")
                        .args(["diff", "--numstat", "HEAD"])
                        .current_dir(&wd)
                        .output();
                    let name_status = std::process::Command::new("git")
                        .args(["diff", "--name-status", "HEAD"])
                        .current_dir(&wd)
                        .output();

                    let mut files = Vec::new();

                    if let (Ok(ns_out), Ok(ns_stat)) = (numstat, name_status) {
                        if ns_out.status.success() && ns_stat.status.success() {
                            use crate::state::types::{ChangedFileInfo, FileChangeStatus};

                            // Parse name-status into a map: path -> status + old_path
                            let mut status_map: std::collections::HashMap<String, (FileChangeStatus, Option<String>)> =
                                std::collections::HashMap::new();
                            for line in String::from_utf8_lossy(&ns_stat.stdout).lines() {
                                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                                if parts.len() >= 2 {
                                    let status = match parts[0] {
                                        "A" => FileChangeStatus::Added,
                                        "M" => FileChangeStatus::Modified,
                                        "D" => FileChangeStatus::Deleted,
                                        "R" => FileChangeStatus::Renamed,
                                        "C" => FileChangeStatus::Copied,
                                        _ => FileChangeStatus::Modified,
                                    };
                                    let old_path = if (status == FileChangeStatus::Renamed
                                        || status == FileChangeStatus::Copied)
                                        && parts.len() >= 3
                                    {
                                        Some(parts[1].to_string())
                                    } else {
                                        None
                                    };
                                    let path = if old_path.is_some() {
                                        parts.get(2).unwrap_or(&parts[1]).to_string()
                                    } else {
                                        parts[1].to_string()
                                    };
                                    status_map.insert(path, (status, old_path));
                                }
                            }

                            // Parse numstat into counts
                            let mut count_map: std::collections::HashMap<String, (u32, u32)> =
                                std::collections::HashMap::new();
                            for line in String::from_utf8_lossy(&ns_out.stdout).lines() {
                                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                                if parts.len() >= 3 {
                                    let adds: u32 = parts[0].parse().unwrap_or(0);
                                    let dels: u32 = parts[1].parse().unwrap_or(0);
                                    // Binary files show "-\t-\tpath"
                                    let real_adds = if parts[0] == "-" { 0 } else { adds };
                                    let real_dels = if parts[1] == "-" { 0 } else { dels };
                                    count_map.insert(parts[2].to_string(), (real_adds, real_dels));
                                }
                            }

                            // Merge: use status_map as the source of truth for paths
                            for (path, (status, old_path)) in &status_map {
                                let (additions, deletions) =
                                    count_map.get(path).copied().unwrap_or((0, 0));
                                files.push(ChangedFileInfo {
                                    path: path.clone(),
                                    old_path: old_path.clone(),
                                    status: status.clone(),
                                    additions,
                                    deletions,
                                });
                            }
                        }
                    }

                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                        state.ui.changed_files = if files.is_empty() {
                            None
                        } else {
                            Some(files)
                        };
                        state.ui.selected_changed_file_index = 0;
                        state.mark_render_dirty();
                    }
                });
            }
        }
    }

    /// Move the focused task forward or backward by `direction` columns (+1 or -1).
    fn handle_move_task(&mut self, direction: i32) {
        let visible = self.config.columns.visible_column_ids();
        let (task_id, current_col_idx) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let tid = state.ui.focused_task_id.clone();
            let idx = state.kanban.focused_column_index;
            (tid, idx)
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match task_id {
            Some(tid) => {
                let target_idx = current_col_idx as i32 + direction;
                if target_idx >= 0 && (target_idx as usize) < visible.len() {
                    let target_col = visible[target_idx as usize].clone();
                    state.move_task(&tid, crate::state::types::KanbanColumn(target_col.clone()));

                    // Trigger orchestration engine if the target column has an agent configured
                    if let Some(_agent) = self.config.columns.agent_for_column(&target_col) {
                        let already_running = state
                            .tasks
                            .get(&tid)
                            .map(|t| {
                                matches!(
                                    t.agent_status,
                                    crate::state::types::AgentStatus::Running
                                        | crate::state::types::AgentStatus::Hung
                                )
                            })
                            .unwrap_or(false);
                        if already_running {
                            let status = state
                                .tasks
                                .get(&tid)
                                .map(|t| t.agent_status.clone())
                                .unwrap_or(crate::state::types::AgentStatus::Pending);
                            if status == crate::state::types::AgentStatus::Hung {
                                state.set_notification(
                                    "Task is hung — abort the session before re-dispatching"
                                        .to_string(),
                                    crate::state::types::NotificationVariant::Warning,
                                    5000,
                                );
                            }
                        } else {
                            if let Some(project_id) =
                                state.project_registry.active_project_id.clone()
                            {
                                if let Some(client) =
                                    self.opencode_clients.get(&project_id).cloned()
                                {
                                    // Capture the PREVIOUS agent type before overwriting it,
                                    // so start_agent can detect the change and create a fresh session.
                                    let previous_agent =
                                        state.tasks.get(&tid).and_then(|t| t.agent_type.clone());
                                    // Set status to Running while holding the lock to close the race window
                                    state.update_task_agent_status(
                                        &tid,
                                        crate::state::types::AgentStatus::Running,
                                    );
                                    state.set_task_agent_type(
                                        &tid,
                                        self.config.columns.agent_for_column(&target_col),
                                    );
                                    drop(state); // Release lock before spawning async
                                    crate::orchestration::engine::on_task_moved(
                                        &tid,
                                        &crate::state::types::KanbanColumn(target_col),
                                        &self.state,
                                        &client,
                                        &self.config.columns,
                                        &self.config.opencode,
                                        previous_agent,
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

    fn handle_delete_task(&mut self) {
        let task_id = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_task_id.clone()
        };
        let project_id = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.project_registry.active_project_id.clone()
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                                if let Err(_e) = client.abort_session(&session_id).await {}
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

    fn handle_abort_session(&mut self) {
        // Batch read: extract session_id and client in a single lock hold.
        let (session_id, client) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let session_id = state
                .ui
                .focused_task_id
                .as_ref()
                .and_then(|tid| state.tasks.get(tid))
                .and_then(|t| t.session_id.clone());
            let client = state
                .project_registry
                .active_project_id
                .as_ref()
                .and_then(|pid| self.opencode_clients.get(pid))
                .cloned();
            (session_id, client)
        };

        if let Some(sid) = session_id {
            if let Some(client) = client {
                let state = self.state.clone();
                tokio::spawn(async move {
                    let abort_failed = match client.abort_session(&sid).await {
                        Ok(aborted) => {
                            let _ = aborted;
                            false
                        }
                        Err(e) => {
                            tracing::error!("Failed to abort session {}: {}", sid, e);
                            // Tracing layer will also push a notification automatically
                            true
                        }
                    };
                    // Update notification after attempt
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if abort_failed {
                        state.set_notification(
                            format!("Failed to abort session: {}", sid),
                            crate::state::types::NotificationVariant::Error,
                            5000,
                        );
                    } else {
                        state.set_notification(
                            format!("Session abort requested: {}", sid),
                            crate::state::types::NotificationVariant::Warning,
                            3000,
                        );
                    }
                    state.mark_render_dirty();
                });
            } else {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No client available to abort session".to_string(),
                    crate::state::types::NotificationVariant::Error,
                    3000,
                );
            }
        } else {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No active session to abort".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
        }
    }

    /// Retry a hung or errored task — abort the old session, clear stale state,
    /// and re-dispatch the agent for the task's current column.
    fn handle_retry_task(&mut self) {
        use crate::state::types::AgentStatus;

        // Batch read: extract task info, client, and column config in one lock hold.
        let (task_info, client) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let tid = state.ui.focused_task_id.clone();
            let info = tid.as_ref().and_then(|id| {
                let task = state.tasks.get(id)?;
                Some((
                    id.clone(),
                    task.agent_status.clone(),
                    task.session_id.clone(),
                    task.column.0.clone(),
                ))
            });
            let client = state
                .project_registry
                .active_project_id
                .as_ref()
                .and_then(|pid| self.opencode_clients.get(pid))
                .cloned();
            (info, client)
        };

        let Some((task_id, agent_status, session_id, column_id)) = task_info else {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No task selected to retry".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            );
            return;
        };

        // Only allow retry for Hung or Error tasks
        if !matches!(agent_status, AgentStatus::Hung | AgentStatus::Error) {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                format!(
                    "Cannot retry — task status is {:?} (only Hung/Error can be retried)",
                    agent_status
                ),
                crate::state::types::NotificationVariant::Info,
                3000,
            );
            return;
        }

        // Require an OpenCode client for the active project
        let Some(client) = client else {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No OpenCode client for this project".to_string(),
                crate::state::types::NotificationVariant::Warning,
                3000,
            );
            return;
        };

        // Require the current column to have an agent configured
        if self.config.columns.agent_for_column(&column_id).is_none() {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "No agent configured for this column — cannot retry".to_string(),
                crate::state::types::NotificationVariant::Warning,
                3000,
            );
            return;
        }

        let state = self.state.clone();
        let columns_config = self.config.columns.clone();
        let opencode_config = self.config.opencode.clone();

        tokio::spawn(async move {
            // 1. Abort the old session if one exists
            if let Some(ref sid) = session_id {
                match client.abort_session(sid).await {
                    Ok(_) => {
                        tracing::info!("Retry: aborted old session {}", sid);
                    }
                    Err(e) => {
                        tracing::warn!("Retry: failed to abort old session {}: {}", sid, e);
                    }
                }
            }

            // 2. Clear session data, reset error state, set status to Running
            let previous_agent = {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_task_session_id(&task_id, None);
                state.clear_session_data(&task_id);

                // Fix 1: Reset stale pending counts so indicators don't linger
                if let Some(task) = state.tasks.get_mut(&task_id) {
                    task.error_message = None;
                    task.pending_permission_count = 0;
                    task.pending_question_count = 0;
                }

                // Fix 2: Clean up stale subagent sessions from previous run
                if let Some(sessions) = state.session_tracker.subagent_sessions.remove(&task_id) {
                    for sub in &sessions {
                        state
                            .session_tracker
                            .subagent_to_parent
                            .remove(&sub.session_id);
                        state
                            .session_tracker
                            .subagent_session_data
                            .remove(&sub.session_id);
                    }
                }

                state.update_task_agent_status(&task_id, AgentStatus::Running);

                // Fix 3: Recalculate project status so it reflects Running
                if let Some(task) = state.tasks.get(&task_id) {
                    let project_id = task.project_id.clone();
                    state.update_project_status(&project_id);
                }

                // Fix 4: Clear navigation stack if it references this task
                if state
                    .ui
                    .session_nav_stack
                    .iter()
                    .any(|r| r.task_id == task_id)
                {
                    state.ui.session_nav_stack.clear();
                    // If we were viewing this task's detail, close it since
                    // the old session data is now invalid
                    if state.ui.viewing_task_id.as_deref() == Some(&task_id) {
                        state.close_task_detail();
                    }
                }

                state.mark_render_dirty();

                // Capture previous agent type for on_task_moved
                state.tasks.get(&task_id).and_then(|t| t.agent_type.clone())
            };

            // 3. Re-dispatch the agent for the task's current column
            crate::orchestration::engine::on_task_moved(
                &task_id,
                &crate::state::types::KanbanColumn(column_id.clone()),
                &state,
                &client,
                &columns_config,
                &opencode_config,
                previous_agent,
            );

            // 4. Notify the user
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "Task retry — re-dispatching agent".to_string(),
                crate::state::types::NotificationVariant::Success,
                3000,
            );
            state.mark_render_dirty();
        });
    }

    /// Handle drill-down into a subagent session (ctrl+x).
    ///
    /// When in the task detail view, looks for `TaskMessagePart::Agent` parts
    /// in the currently viewed session's messages. If a subagent is found,
    /// fetches its messages (lazy-load) and pushes onto the navigation stack.
    fn handle_drill_down_subagent(&mut self) {
        // Must be in task detail view
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                return;
            }
        }

        // Find the first navigable Agent part in the current view.
        // We extract the needed data while holding the lock, then drop it.
        let found = Self::find_drillable_subagent(&self.state);

        let (session_id, agent, task_id, depth) = match found {
            Some(f) => f,
            None => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "No subagent to drill into".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    2000,
                );
                return;
            }
        };

        // Fetch subagent messages lazily
        let client = self.get_active_client();
        let state = self.state.clone();

        tokio::spawn(async move {
            // Check if we already have cached data
            let needs_fetch = {
                let s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.session_tracker
                    .subagent_session_data
                    .get(&session_id)
                    .map(|d| d.messages.is_empty())
                    .unwrap_or(true)
            };

            if needs_fetch {
                if let Some(client) = client {
                    match client.fetch_subagent_messages(&session_id).await {
                        Ok(messages) => {
                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                            let entry = s
                                .session_tracker
                                .subagent_session_data
                                .entry(session_id.clone())
                                .or_insert_with(crate::state::types::TaskDetailSession::default);
                            entry.session_id = Some(session_id.clone());
                            entry.task_id = task_id.clone();
                            entry.streaming_text = None; // Clear to avoid double-rendering with messages
                            entry.messages = messages;
                            entry.render_version += 1;
                        }
                        Err(e) => {
                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                            s.set_notification(
                                format!("Failed to load subagent: {}", e),
                                crate::state::types::NotificationVariant::Error,
                                3000,
                            );
                            return;
                        }
                    }
                } else {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_notification(
                        "No OpenCode client available".to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
                    return;
                }
            }

            // Push onto navigation stack
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            // Guard against duplicate push from rapid key presses
            let already_on_stack =
                s.ui.session_nav_stack
                    .iter()
                    .any(|r| r.session_id == session_id);
            if already_on_stack {
                return; // Already pushed by a prior keypress
            }
            // For nested drill-downs, use only the agent name to avoid
            // repeating the task label (e.g., "Task #3 > planning > do"
            // instead of "Task #3 > planning > Task #3 > do").
            let label = if s.is_drilled_into_subagent() {
                agent.clone()
            } else {
                let task_label = s
                    .tasks
                    .get(&task_id)
                    .map(|t| format!("Task #{}", t.number))
                    .unwrap_or_else(|| task_id.clone());
                format!("{} > {}", task_label, agent)
            };
            let session_ref = crate::state::types::SessionRef {
                task_id: task_id.clone(),
                session_id: session_id.clone(),
                label,
                depth,
            };
            s.push_subagent_drilldown(session_ref);
        });
    }

    /// Open the diff review view focused on a specific file from the changed-files sidebar.
    ///
    /// Runs `git diff HEAD -- <path>` in the project's working directory, parses the
    /// output, and switches to `AppMode::DiffReview` with the selected file pre-loaded.
    fn handle_open_file_diff(&mut self) {
        use crate::state::types::{AppMode, DiffReviewState, FocusedPanel};

        let (working_dir, file_path, task_number) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let files = match &state.ui.changed_files {
                Some(f) if !f.is_empty() => f.clone(),
                _ => return,
            };
            let idx = state.ui.selected_changed_file_index.min(files.len() - 1);
            let file = &files[idx];
            let path = file.path.clone();

            let task_id = match &state.ui.viewing_task_id {
                Some(id) => id.clone(),
                None => return,
            };

            let task = match state.tasks.get(&task_id) {
                Some(t) => t,
                None => return,
            };

            let project_id = task.project_id.clone();
            let task_number = task.number;

            let project = state
                .project_registry
                .projects
                .iter()
                .find(|p| p.id == project_id);

            let wd = match project {
                Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
                _ => return,
            };

            (wd, path, task_number)
        };

        // Spawn git diff for the specific file on a blocking thread
        let state = self.state.clone();
        let target_file_path = file_path.clone();
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new("git")
                .args(["diff", "HEAD", "--", &target_file_path])
                .current_dir(&working_dir)
                .output();

            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());

            match output {
                Ok(out) if out.status.success() => {
                    let raw = String::from_utf8_lossy(&out.stdout);
                    let files = crate::tui::diff_view::parse_git_diff(&raw);

                    // Select the file matching the target path
                    let selected_idx = files
                        .iter()
                        .position(|f| f.path == target_file_path)
                        .unwrap_or(0);

                    state.ui.diff_review = Some(DiffReviewState {
                        files,
                        selected_file_index: selected_idx,
                        scroll_offset: 0,
                        error: None,
                        task_number,
                        files_list_focused: false,
                    });
                    state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                    state.ui.mode = AppMode::DiffReview;
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    state.ui.diff_review = Some(DiffReviewState {
                        files: Vec::new(),
                        selected_file_index: 0,
                        scroll_offset: 0,
                        error: Some(stderr.to_string()),
                        task_number,
                        files_list_focused: false,
                    });
                    state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                    state.ui.mode = AppMode::DiffReview;
                }
                Err(e) => {
                    state.set_notification(
                        format!("Failed to run git diff: {}", e),
                        crate::state::types::NotificationVariant::Error,
                        3000,
                    );
                }
            }
        });
    }

    /// Open the diff review view for the focused task.
    ///
    /// Runs `git diff HEAD` in the project's working directory, parses the
    /// output, and switches to `AppMode::DiffReview`.  The git command runs
    /// on a background thread so the UI stays responsive.
    fn handle_review_changes(&mut self) {
        use crate::state::types::{AgentStatus, AppMode, DiffReviewState, FocusedPanel};

        // Batch-read everything we need while holding the lock once.
        let (working_dir, task_number) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());

            // When viewing the task detail panel, prefer viewing_task_id (the task
            // whose detail page is open) over focused_task_id (the kanban cursor).
            let tid = if state.ui.focused_panel == FocusedPanel::TaskDetail {
                state
                    .ui
                    .viewing_task_id
                    .as_ref()
                    .or(state.ui.focused_task_id.as_ref())
            } else {
                state.ui.focused_task_id.as_ref()
            };

            let tid = match tid {
                Some(id) => id.clone(),
                None => {
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        "No task selected".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        2000,
                    );
                    return;
                }
            };

            let task = match state.tasks.get(&tid) {
                Some(t) => t,
                None => return,
            };

            // Only allow review for tasks that are done / ready / complete.
            let is_reviewable = matches!(
                task.agent_status,
                AgentStatus::Complete | AgentStatus::Ready
            );
            let agent_status_display = task.agent_status.clone();
            let project_id = task.project_id.clone();
            let task_number = task.number;

            if !is_reviewable {
                drop(state);
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    format!(
                        "Cannot review — task is {:?} (only Ready/Complete tasks can be reviewed)",
                        agent_status_display
                    ),
                    crate::state::types::NotificationVariant::Info,
                    3000,
                );
                return;
            }

            let project = state
                .project_registry
                .projects
                .iter()
                .find(|p| p.id == project_id);

            let wd = match project {
                Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
                _ => {
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        "No working directory configured for this project".to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
                    return;
                }
            };

            (wd, task_number)
        };

        // Remember where we came from so Esc can return correctly
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.diff_review_source = Some(state.ui.focused_panel.clone());
        }

        // Spawn git diff on a blocking thread so we don't freeze the UI.
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new("git")
                .args(["diff", "HEAD"])
                .current_dir(&working_dir)
                .output();

            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());

            match output {
                Ok(out) if out.status.success() => {
                    let raw = String::from_utf8_lossy(&out.stdout);
                    let files = crate::tui::diff_view::parse_git_diff(&raw);

                    if files.is_empty() && raw.trim().is_empty() {
                        state.set_notification(
                            "No uncommitted changes to review".to_string(),
                            crate::state::types::NotificationVariant::Info,
                            3000,
                        );
                        return;
                    }

                    state.ui.diff_review = Some(DiffReviewState {
                        files,
                        selected_file_index: 0,
                        scroll_offset: 0,
                        error: None,
                        task_number,
                        files_list_focused: false,
                    });
                    state.ui.mode = AppMode::DiffReview;
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    state.ui.diff_review = Some(DiffReviewState {
                        files: Vec::new(),
                        selected_file_index: 0,
                        scroll_offset: 0,
                        error: Some(stderr.to_string()),
                        task_number,
                        files_list_focused: false,
                    });
                    state.ui.mode = AppMode::DiffReview;
                }
                Err(e) => {
                    state.set_notification(
                        format!("Failed to run git diff: {}", e),
                        crate::state::types::NotificationVariant::Error,
                        3000,
                    );
                }
            }
        });
    }

    /// Accept a reviewed task — run `git add -A && git commit` natively,
    /// then move the task to the "done" column.
    fn handle_accept_review(&mut self) {
        use crate::state::types::{AgentStatus, ReviewStatus};

        // Batch-read: extract task info and working directory in one lock hold.
        let (task_info, working_dir) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

            // Prefer viewing_task_id (task detail panel) over focused_task_id (kanban cursor).
            let tid = if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                state
                    .ui
                    .viewing_task_id
                    .as_ref()
                    .or(state.ui.focused_task_id.as_ref())
                    .cloned()
            } else {
                state.ui.focused_task_id.clone()
            };

            let tid = match tid {
                Some(id) => id,
                None => {
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        "No task selected".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        2000,
                    );
                    return;
                }
            };

            let task = match state.tasks.get(&tid) {
                Some(t) => t,
                None => return,
            };

            // Validate: must be in review column with AwaitingDecision status
            if task.column.0 != "review" || task.review_status != ReviewStatus::AwaitingDecision {
                drop(state);
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "Cannot accept — task is not awaiting review".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    3000,
                );
                return;
            }

            let project_id = task.project_id.clone();
            let task_number = task.number;
            let description = task.description.clone();
            let task_id = tid;

            let project = state
                .project_registry
                .projects
                .iter()
                .find(|p| p.id == project_id);
            let wd = match project {
                Some(p) if !p.working_directory.is_empty() => p.working_directory.clone(),
                _ => {
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        "No working directory configured".to_string(),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
                    return;
                }
            };

            // Mark as Approved + Running so the user sees activity
            if let Some(task) = state.tasks.get_mut(&task_id) {
                task.review_status = ReviewStatus::Approved;
            }
            state.update_task_agent_status(&task_id, AgentStatus::Running);

            ((task_id, task_number, description, project_id), wd)
        };

        let (task_id, task_number, description, project_id) = task_info;

        // Derive a commit message from the task title
        let commit_msg = {
            let title = crate::state::types::derive_title_from_description(&description);
            if title.is_empty() {
                format!("feat: task #{}", task_number)
            } else {
                // Capitalize first letter
                let mut msg = title;
                if let Some(first) = msg.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                format!("feat: {}", msg)
            }
        };

        let state = self.state.clone();
        let columns_config = self.config.columns.clone();
        let opencode_config = self.config.opencode.clone();

        // Spawn git add + commit on a blocking thread
        tokio::task::spawn_blocking(move || {
            // 1. git add -A
            let add_result = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&working_dir)
                .output();

            match add_result {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("git add failed: {}", e);
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_notification(
                        format!("git add failed: {}", e),
                        crate::state::types::NotificationVariant::Error,
                        5000,
                    );
                    // Revert review status
                    if let Some(task) = s.tasks.get_mut(&task_id) {
                        task.review_status = ReviewStatus::AwaitingDecision;
                        task.agent_status = AgentStatus::Complete;
                    }
                    s.mark_render_dirty();
                    return;
                }
            }

            // 2. Check if there are changes to commit
            let status_result = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&working_dir)
                .output();

            let has_changes = match &status_result {
                Ok(out) => !out.stdout.is_empty(),
                Err(_) => true, // Assume there are changes if we can't check
            };

            if !has_changes {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    "No changes to commit — moving to done".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    3000,
                );
                // Move to done anyway
                s.move_task(&task_id, crate::state::types::KanbanColumn("done".to_string()));
                s.update_task_agent_status(&task_id, AgentStatus::Complete);
                s.mark_render_dirty();
                return;
            }

            // 3. git commit
            let commit_result = std::process::Command::new("git")
                .args(["commit", "-m", &commit_msg])
                .current_dir(&working_dir)
                .output();

            match commit_result {
                Ok(out) if out.status.success() => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.move_task(&task_id, crate::state::types::KanbanColumn("done".to_string()));
                    s.update_task_agent_status(&task_id, AgentStatus::Complete);
                    s.set_notification(
                        format!("Task #{} committed and moved to done", task_number),
                        crate::state::types::NotificationVariant::Success,
                        3000,
                    );
                    s.mark_render_dirty();
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_notification(
                        format!("git commit failed: {}", stderr.trim()),
                        crate::state::types::NotificationVariant::Error,
                        5000,
                    );
                    // Revert review status
                    if let Some(task) = s.tasks.get_mut(&task_id) {
                        task.review_status = ReviewStatus::AwaitingDecision;
                        task.agent_status = AgentStatus::Complete;
                    }
                    s.mark_render_dirty();
                }
                Err(e) => {
                    tracing::error!("git commit failed: {}", e);
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_notification(
                        format!("git commit failed: {}", e),
                        crate::state::types::NotificationVariant::Error,
                        5000,
                    );
                    // Revert review status
                    if let Some(task) = s.tasks.get_mut(&task_id) {
                        task.review_status = ReviewStatus::AwaitingDecision;
                        task.agent_status = AgentStatus::Complete;
                    }
                    s.mark_render_dirty();
                }
            }

            // Drop unused variables to suppress warnings
            let _ = (columns_config, opencode_config, project_id);
        });
    }

    /// Reject a reviewed task — move it back to the "running" column for re-work.
    fn handle_reject_review(&mut self) {
        use crate::state::types::ReviewStatus;

        // Batch-read: extract task info in one lock hold.
        let task_info = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());

            let tid = if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                state
                    .ui
                    .viewing_task_id
                    .as_ref()
                    .or(state.ui.focused_task_id.as_ref())
                    .cloned()
            } else {
                state.ui.focused_task_id.clone()
            };

            let tid = match tid {
                Some(id) => id,
                None => {
                    drop(state);
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        "No task selected".to_string(),
                        crate::state::types::NotificationVariant::Info,
                        2000,
                    );
                    return;
                }
            };

            let task = match state.tasks.get(&tid) {
                Some(t) => t,
                None => return,
            };

            // Validate: must be in review column with AwaitingDecision status
            if task.column.0 != "review" || task.review_status != ReviewStatus::AwaitingDecision {
                drop(state);
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.set_notification(
                    "Cannot reject — task is not awaiting review".to_string(),
                    crate::state::types::NotificationVariant::Info,
                    3000,
                );
                return;
            }

            let task_id = tid;
            let task_number = task.number;
            let session_id = task.session_id.clone();
            let previous_agent = task.agent_type.clone();
            let project_id = task.project_id.clone();

            (task_id, task_number, session_id, previous_agent, project_id)
        };

        let (task_id, task_number, session_id, previous_agent, project_id) = task_info;

        // Get the OpenCode client for this project
        let client = {
            let _state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.opencode_clients.get(&project_id).cloned()
        };

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // Mark as Rejected
        if let Some(task) = state.tasks.get_mut(&task_id) {
            task.review_status = ReviewStatus::Rejected;
        }

        // Clear review agent session data so a fresh review runs next time
        state.clear_session_data(&task_id);
        // Also clear the session ID so the do agent gets a fresh session
        state.set_task_session_id(&task_id, None);

        // Move back to "running" column
        state.move_task(&task_id, crate::state::types::KanbanColumn("running".to_string()));

        // Reset review_status to Pending for the re-work cycle
        if let Some(task) = state.tasks.get_mut(&task_id) {
            task.review_status = ReviewStatus::Pending;
        }

        state.set_notification(
            format!("Task #{} rejected — sent back for re-work", task_number),
            crate::state::types::NotificationVariant::Info,
            3000,
        );
        state.mark_render_dirty();

        // Trigger orchestration: start the do agent for the running column
        if let Some(client) = client {
            state.update_task_agent_status(&task_id, crate::state::types::AgentStatus::Running);
            state.set_task_agent_type(
                &task_id,
                self.config.columns.agent_for_column("running"),
            );
            drop(state);

            crate::orchestration::engine::on_task_moved(
                &task_id,
                &crate::state::types::KanbanColumn("running".to_string()),
                &self.state,
                &client,
                &self.config.columns,
                &self.config.opencode,
                previous_agent,
            );
        } else {
            state.set_notification(
                "No OpenCode client — agent dispatch skipped".to_string(),
                crate::state::types::NotificationVariant::Warning,
                3000,
            );
        }

        // Suppress unused variable warning
        let _ = session_id;
    }

    /// Scan the current view for a drillable subagent `Agent` part.
    ///
    /// Returns `Some((session_id, agent_name, parent_task_id, depth))` if a
    /// navigable subagent is found, or `None` otherwise.
    fn find_drillable_subagent(
        state: &Arc<Mutex<AppState>>,
    ) -> Option<(String, String, String, u32)> {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());

        let session_id_to_scan = state.get_drilldown_session_id().map(|s| s.to_string());

        if let Some(scan_id) = session_id_to_scan {
            // Scanning subagent session data
            if let Some(session_data) = state.session_tracker.subagent_session_data.get(&scan_id) {
                let task_id = state.ui.viewing_task_id.clone().unwrap_or_default();
                let current_depth = state
                    .ui
                    .session_nav_stack
                    .last()
                    .map(|r| r.depth)
                    .unwrap_or(0);
                for msg in &session_data.messages {
                    for part in &msg.parts {
                        if let crate::state::types::TaskMessagePart::Agent { id, agent } = part {
                            let already_in_stack = state
                                .ui
                                .session_nav_stack
                                .iter()
                                .any(|r| r.session_id == *id);
                            if !already_in_stack {
                                return Some((
                                    id.clone(),
                                    agent.clone(),
                                    task_id,
                                    current_depth + 1,
                                ));
                            }
                        }
                    }
                }
            }
        } else {
            // Scanning parent task's messages
            if let Some(ref tid) = state.ui.viewing_task_id {
                if let Some(session) = state.session_tracker.task_sessions.get(tid) {
                    let task_id = tid.clone();
                    for msg in &session.messages {
                        for part in &msg.parts {
                            if let crate::state::types::TaskMessagePart::Agent { id, agent } = part
                            {
                                let already_in_stack = state
                                    .ui
                                    .session_nav_stack
                                    .iter()
                                    .any(|r| r.session_id == *id);
                                if !already_in_stack {
                                    return Some((id.clone(), agent.clone(), task_id, 1));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    // ── Shared helpers ──

    /// Handle key events in DiffReview mode.
    fn handle_diff_review_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let source = state.ui.diff_review_source.take();
                state.ui.mode = crate::state::types::AppMode::Normal;
                state.ui.diff_review = None;
                // If we came from task detail, restore that focus
                if source == Some(crate::state::types::FocusedPanel::TaskDetail) {
                    state.ui.focused_panel = crate::state::types::FocusedPanel::TaskDetail;
                    // viewing_task_id should still be set
                }
            }
            KeyCode::Tab => {
                // Toggle focus between file list and diff content
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    review.files_list_focused = !review.files_list_focused;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    if review.files_list_focused {
                        // Navigate files in the list
                        let max = review.files.len().saturating_sub(1);
                        if review.selected_file_index < max {
                            review.selected_file_index += 1;
                            review.scroll_offset = 0;
                        }
                    } else {
                        crate::tui::diff_view::scroll_diff(review, 1);
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    if review.files_list_focused {
                        // Navigate files in the list
                        if review.selected_file_index > 0 {
                            review.selected_file_index -= 1;
                            review.scroll_offset = 0;
                        }
                    } else {
                        crate::tui::diff_view::scroll_diff(review, -1);
                    }
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    if review.files_list_focused {
                        // Switch focus to diff content
                        review.files_list_focused = false;
                    } else {
                        crate::tui::diff_view::next_file(review);
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    if !review.files_list_focused {
                        // Switch focus to file list
                        review.files_list_focused = true;
                    } else {
                        crate::tui::diff_view::prev_file(review);
                    }
                }
            }
            // Keep existing bracket navigation
            KeyCode::Char(']') => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::next_file(review);
                }
            }
            KeyCode::Char('[') | KeyCode::Backspace => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::prev_file(review);
                }
            }
            _ => {}
        }
    }

    /// Toggle the reports view on/off.
    fn handle_reports_toggle(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.ui.mode == crate::state::types::AppMode::Reports {
            // Already in reports — close it
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.ui.reports = None;
        } else {
            // Enter reports mode
            state.ui.mode = crate::state::types::AppMode::Reports;
            drop(state);
            crate::tui::reports::load_reports_data(&self.state);
        }
    }

    /// Handle key events in Reports mode.
    fn handle_reports_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        use crate::state::types::FocusedPanel;
        use crate::state::types::ReportsFocusedPane;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.mode = crate::state::types::AppMode::Normal;
                state.ui.reports = None;
            }
            KeyCode::Tab => {
                // Toggle focused_pane between Tasks and Commits
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut reports) = state.ui.reports {
                    reports.focused_pane = match reports.focused_pane {
                        ReportsFocusedPane::Tasks => ReportsFocusedPane::Commits,
                        ReportsFocusedPane::Commits => ReportsFocusedPane::Tasks,
                    };
                }
            }
            KeyCode::Enter => {
                // Open task detail for the selected task (only when tasks pane is focused)
                let result: Option<(String, bool, Option<String>)> = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ref reports) = state.ui.reports {
                        if reports.focused_pane == ReportsFocusedPane::Tasks {
                            if let Some(tid) = reports.task_ids.get(reports.selected_task_index) {
                                let tid = tid.clone();
                                let (reviewable, wd) = {
                                    let task = state.tasks.get(&tid);
                                    let reviewable = task.map(|t| {
                                        matches!(
                                            t.agent_status,
                                            crate::state::types::AgentStatus::Complete
                                                | crate::state::types::AgentStatus::Ready
                                        )
                                    }).unwrap_or(false);
                                    let wd = task.and_then(|t| {
                                        state
                                            .project_registry
                                            .projects
                                            .iter()
                                            .find(|p| p.id == t.project_id)
                                            .filter(|p| !p.working_directory.is_empty())
                                            .map(|p| p.working_directory.clone())
                                    });
                                    (reviewable, wd)
                                };
                                // Exit reports mode
                                state.ui.mode = crate::state::types::AppMode::Normal;
                                state.ui.reports = None;
                                // Set focused task
                                state.ui.focused_task_id = Some(tid.clone());
                                // Update kanban focused index for the task's column
                                if let Some(task) = state.tasks.get(&tid) {
                                    let col_id = task.column.0.clone();
                                    if let Some(task_ids) = state.kanban.columns.get(&col_id) {
                                        if let Some(idx) = task_ids.iter().position(|id| id == &tid) {
                                            state.kanban.focused_task_index.insert(col_id.clone(), idx);
                                        }
                                    }
                                    state.ui.focused_column = col_id;
                                }
                                // Open task detail
                                state.open_task_detail(&tid);
                                state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                                Some((tid, reviewable, wd))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                // Async load changed files for reviewable tasks
                if let Some((tid, reviewable, wd)) = result {
                    if reviewable {
                        if let Some(wd) = wd {
                            let state = self.state.clone();
                            tokio::task::spawn_blocking(move || {
                                let numstat = std::process::Command::new("git")
                                    .args(["diff", "--numstat", "HEAD"])
                                    .current_dir(&wd)
                                    .output();
                                let name_status = std::process::Command::new("git")
                                    .args(["diff", "--name-status", "HEAD"])
                                    .current_dir(&wd)
                                    .output();

                                let mut files = Vec::new();

                                if let (Ok(ns_out), Ok(ns_stat)) = (numstat, name_status) {
                                    if ns_out.status.success() && ns_stat.status.success() {
                                        use crate::state::types::{ChangedFileInfo, FileChangeStatus};

                                        let mut status_map: std::collections::HashMap<String, (FileChangeStatus, Option<String>)> =
                                            std::collections::HashMap::new();
                                        for line in String::from_utf8_lossy(&ns_stat.stdout).lines() {
                                            let parts: Vec<&str> = line.splitn(3, '\t').collect();
                                            if parts.len() >= 2 {
                                                let status = match parts[0] {
                                                    "A" => FileChangeStatus::Added,
                                                    "M" => FileChangeStatus::Modified,
                                                    "D" => FileChangeStatus::Deleted,
                                                    "R" => FileChangeStatus::Renamed,
                                                    "C" => FileChangeStatus::Copied,
                                                    _ => FileChangeStatus::Modified,
                                                };
                                                let old_path = if (status == FileChangeStatus::Renamed
                                                    || status == FileChangeStatus::Copied)
                                                    && parts.len() >= 3
                                                {
                                                    Some(parts[1].to_string())
                                                } else {
                                                    None
                                                };
                                                let path = if old_path.is_some() {
                                                    parts.get(2).unwrap_or(&parts[1]).to_string()
                                                } else {
                                                    parts[1].to_string()
                                                };
                                                status_map.insert(path, (status, old_path));
                                            }
                                        }

                                        let mut count_map: std::collections::HashMap<String, (u32, u32)> =
                                            std::collections::HashMap::new();
                                        for line in String::from_utf8_lossy(&ns_out.stdout).lines() {
                                            let parts: Vec<&str> = line.splitn(3, '\t').collect();
                                            if parts.len() >= 3 {
                                                let adds: u32 = parts[0].parse().unwrap_or(0);
                                                let dels: u32 = parts[1].parse().unwrap_or(0);
                                                let real_adds = if parts[0] == "-" { 0 } else { adds };
                                                let real_dels = if parts[1] == "-" { 0 } else { dels };
                                                count_map.insert(parts[2].to_string(), (real_adds, real_dels));
                                            }
                                        }

                                        for (path, (status, old_path)) in &status_map {
                                            let (additions, deletions) =
                                                count_map.get(path).copied().unwrap_or((0, 0));
                                            files.push(ChangedFileInfo {
                                                path: path.clone(),
                                                old_path: old_path.clone(),
                                                status: status.clone(),
                                                additions,
                                                deletions,
                                            });
                                        }
                                    }
                                }

                                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                                    state.ui.changed_files = if files.is_empty() {
                                        None
                                    } else {
                                        Some(files)
                                    };
                                    state.ui.selected_changed_file_index = 0;
                                    state.mark_render_dirty();
                                }
                            });
                        }
                    }
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut reports) = state.ui.reports {
                    match reports.focused_pane {
                        ReportsFocusedPane::Tasks => {
                            crate::tui::reports::scroll_tasks(reports, 1);
                        }
                        ReportsFocusedPane::Commits => {
                            crate::tui::reports::scroll_reports(reports, 1);
                        }
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut reports) = state.ui.reports {
                    match reports.focused_pane {
                        ReportsFocusedPane::Tasks => {
                            crate::tui::reports::scroll_tasks(reports, -1);
                        }
                        ReportsFocusedPane::Commits => {
                            crate::tui::reports::scroll_reports(reports, -1);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Switch to the previous/next project by an offset (-1 or +1).
    /// Wraps around at the boundaries.
    fn switch_project_offset(&mut self, direction: i32) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let len = state.project_registry.projects.len();
        if len <= 1 {
            return;
        }
        let current_idx = state
            .project_registry
            .active_project_id
            .as_ref()
            .and_then(|id| {
                state
                    .project_registry
                    .projects
                    .iter()
                    .position(|p| &p.id == id)
            })
            .unwrap_or(0);
        let new_idx = (current_idx as i32 + direction).rem_euclid(len as i32) as usize;
        let new_id = state.project_registry.projects[new_idx].id.clone();
        state.select_project(&new_id);
    }

    /// Shared text-input key handler for single-line input prompts.
    ///
    /// Used by both the project-rename and working-directory prompts.
    /// Handles character insertion, backspace, delete, cursor movement,
    /// Home/End, Enter (submit), and Escape (cancel).
    fn handle_text_input(&mut self, key: crossterm::event::KeyEvent, prompt: InputPrompt) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Enter => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                    InputPrompt::WorkingDirectory => match state.submit_working_directory() {
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
                    },
                    InputPrompt::NewProjectDirectory => {
                        match state.submit_new_project_directory() {
                            Ok(name) => {
                                // Register the shared OpenCode client for the new project.
                                // All projects share a single server, so we clone any existing client.
                                if let Some(new_pid) =
                                    state.project_registry.active_project_id.clone()
                                {
                                    if let Some(existing_client) =
                                        self.opencode_clients.values().next()
                                    {
                                        self.opencode_clients
                                            .insert(new_pid.clone(), existing_client.clone());
                                        state.set_project_connected(&new_pid, true);
                                    }
                                }
                                state.set_notification(
                                    format!("Created project \"{}\"", name),
                                    crate::state::types::NotificationVariant::Success,
                                    3000,
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match prompt {
                    InputPrompt::RenameProject => state.cancel_project_rename(),
                    InputPrompt::WorkingDirectory => state.cancel_working_directory(),
                    InputPrompt::NewProjectDirectory => state.cancel_new_project_directory(),
                }
            }
            KeyCode::Char(c) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let char_count = state.ui.input_text.chars().count();
                let cursor = state.ui.input_cursor.min(char_count);
                // Convert char index to byte offset for insertion.
                let byte_pos = state
                    .ui
                    .input_text
                    .char_indices()
                    .nth(cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(state.ui.input_text.len());
                state.ui.input_text.insert(byte_pos, c);
                // The inserted char is exactly 1 char wide; advance cursor.
                state.ui.input_cursor = cursor + 1;
            }
            KeyCode::Backspace => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.input_cursor = state.ui.input_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let char_count = state.ui.input_text.chars().count();
                let new_pos = state.ui.input_cursor + 1;
                state.ui.input_cursor = new_pos.min(char_count);
            }
            KeyCode::Home => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.input_cursor = 0;
            }
            KeyCode::End => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.input_cursor = state.ui.input_text.chars().count();
            }
            _ => {} // Ignore other keys
        }
    }

    /// Handle key events in ProjectRename mode.
    fn handle_rename_key(&mut self, key: crossterm::event::KeyEvent) {
        self.handle_text_input(key, InputPrompt::RenameProject);
    }

    /// Handle key events in InputPrompt mode (used for working directory and
    /// new project directory).
    fn handle_input_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        let prompt_type = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.prompt_context.as_deref().map(|c| match c {
                "new_project_directory" => InputPrompt::NewProjectDirectory,
                "set_working_directory" => InputPrompt::WorkingDirectory,
                _ => InputPrompt::WorkingDirectory,
            })
        };
        if let Some(pt) = prompt_type {
            self.handle_text_input(key, pt);
        }
    }

    /// Handle key events in TaskEditor mode.
    fn handle_editor_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::tui::editor_handler::{handle_editor_input, EditorAction};

        let action = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(editor) = state.get_task_editor_mut() {
                handle_editor_input(editor, key, &self.editor_key_matcher)
            } else {
                EditorAction::None
            }
        };

        match action {
            EditorAction::Save => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match state.save_task_editor() {
                    Ok(task_id) => {
                        // Extract column ID before closing editor
                        let column_id = state.get_task_editor().and_then(|ed| ed.column_id.clone());

                        // Close the editor and return to normal mode
                        state.cancel_task_editor();
                        state.set_notification(
                            format!("Task saved: {}", task_id),
                            crate::state::types::NotificationVariant::Success,
                            3000,
                        );

                        // Focus the newly created/saved task
                        state.ui.focused_task_id = Some(task_id.clone());

                        // Highlight the saved task for visual feedback
                        state.highlight_task(task_id.clone(), 3000);

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
                            let agent_name = self.config.columns.agent_for_column(col_id);
                            tracing::debug!(
                                "Task {} saved in column '{}', agent_for_column={:?}",
                                task_id,
                                col_id,
                                agent_name
                            );
                            if let Some(_agent) = agent_name {
                                // Check if task already has a running agent
                                let already_running = state
                                    .tasks
                                    .get(&task_id)
                                    .map(|t| {
                                        matches!(
                                            t.agent_status,
                                            crate::state::types::AgentStatus::Running
                                                | crate::state::types::AgentStatus::Hung
                                        )
                                    })
                                    .unwrap_or(false);

                                if already_running {
                                    let status = state
                                        .tasks
                                        .get(&task_id)
                                        .map(|t| t.agent_status.clone())
                                        .unwrap_or(crate::state::types::AgentStatus::Pending);
                                    if status == crate::state::types::AgentStatus::Hung {
                                        state.set_notification(
                                            "Task is hung — abort the session before re-dispatching".to_string(),
                                            crate::state::types::NotificationVariant::Warning,
                                            5000,
                                        );
                                    }
                                } else {
                                    if let Some(project_id) =
                                        state.project_registry.active_project_id.clone()
                                    {
                                        if let Some(client) =
                                            self.opencode_clients.get(&project_id).cloned()
                                        {
                                            // Capture the PREVIOUS agent type before overwriting it,
                                            // so start_agent can detect the change and create a fresh session.
                                            let previous_agent = state
                                                .tasks
                                                .get(&task_id)
                                                .and_then(|t| t.agent_type.clone());
                                            // Set status to Running while holding the lock to close the race window
                                            state.update_task_agent_status(
                                                &task_id,
                                                crate::state::types::AgentStatus::Running,
                                            );
                                            state.set_task_agent_type(
                                                &task_id,
                                                self.config.columns.agent_for_column(col_id),
                                            );
                                            drop(state); // Release lock before spawning async
                                            crate::orchestration::engine::on_task_moved(
                                                &task_id,
                                                &crate::state::types::KanbanColumn(col_id.clone()),
                                                &self.state,
                                                &client,
                                                &self.config.columns,
                                                &self.config.opencode,
                                                previous_agent,
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
                                            "No active project — agent dispatch skipped"
                                                .to_string(),
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.cancel_task_editor();
            }
            EditorAction::None => {}
        }
    }

    // ── Horizontal scroll helpers ──

    /// Calculate the maximum number of kanban columns that can fit.
    fn max_visible_columns(config: &CortexConfig, terminal: &Terminal) -> usize {
        let term_width = terminal.size().unwrap_or(Size::new(80, 24)).width;
        let sidebar_width = config.theme.sidebar_width;
        let kanban_width = term_width.saturating_sub(sidebar_width);
        let available = kanban_width.saturating_sub(6);
        let col_width = config.theme.column_width;
        std::cmp::max(1, (available / col_width) as usize)
    }

    /// Ensure the focused column is visible by adjusting the scroll offset.
    fn ensure_column_visible(state: &mut AppState, config: &CortexConfig, terminal: &Terminal) {
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

    /// Get the OpenCode client for the active project, or `None` if unavailable.
    fn get_active_client(&self) -> Option<OpenCodeClient> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .project_registry
            .active_project_id
            .as_ref()
            .and_then(|pid| self.opencode_clients.get(pid))
            .cloned()
    }

    /// Teardown the terminal. Call this on shutdown.
    pub fn teardown(&mut self) -> anyhow::Result<()> {
        crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
        )?;
        crossterm::terminal::disable_raw_mode()?;
        Ok(())
    }

    // ── Permission Modal helpers ──────────────────────────────────────

    /// Get the effective session for the current view context.
    ///
    /// When drilled into a subagent, returns the subagent's session data
    /// (keyed by session ID in `subagent_session_data`).
    /// Otherwise, returns the main task's session data
    /// (keyed by task ID in `task_sessions`).
    ///
    /// This mirrors the data-source selection used by the modal renderer
    /// in `permission_modal.rs`.
    fn get_effective_session<'a>(
        &self,
        state: &'a AppState,
    ) -> Option<&'a crate::state::types::TaskDetailSession> {
        if let Some(sid) = state.get_drilldown_session_id() {
            state.session_tracker.subagent_session_data.get(sid)
        } else if let Some(ref tid) = state.ui.viewing_task_id {
            state.session_tracker.task_sessions.get(tid)
        } else {
            None
        }
    }

    /// Check if there's a pending permission (vs question) for the current view.
    fn has_pending_permission(&self, state: &AppState) -> bool {
        if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
            return false;
        }
        self.get_effective_session(state)
            .map(|s| !s.pending_permissions.is_empty())
            .unwrap_or(false)
    }

    /// Get the number of options in the current modal.
    fn get_modal_option_count(&self, state: &AppState) -> usize {
        if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
            return 0;
        }
        self.get_effective_session(state)
            .map(|s| {
                if !s.pending_permissions.is_empty() {
                    2 // Yes, No
                } else if !s.pending_questions.is_empty() {
                    s.pending_questions[0].answers.len()
                } else {
                    0
                }
            })
            .unwrap_or(0)
    }

    /// Execute the currently selected modal option (approve/reject permission or answer question).
    fn handle_modal_confirm(&self) {
        tracing::debug!("handle_modal_confirm: invoked");
        // Determine what to do based on what's pending
        let (pending_perm, pending_question, task_id, client): (
            Option<crate::state::types::PermissionRequest>,
            Option<crate::state::types::QuestionRequest>,
            Option<String>,
            Option<OpenCodeClient>,
        ) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.ui.focused_panel != crate::state::types::FocusedPanel::TaskDetail {
                (None, None, None, None)
            } else {
                let session = self.get_effective_session(&state);
                let perm = session
                    .and_then(|s| s.pending_permissions.first().cloned());
                let question = session
                    .and_then(|s| s.pending_questions.first().cloned());
                let client = state
                    .project_registry
                    .active_project_id
                    .as_ref()
                    .and_then(|pid| self.opencode_clients.get(pid))
                    .cloned();
                // task_id is the viewing_task_id (main task) — permissions are
                // always resolved against the parent task regardless of drill-down.
                let tid = state.ui.viewing_task_id.clone();
                (perm, question, tid, client)
            }
        };

        let selected_index = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.permission_modal_selected_index
        };

        // --- Precondition checks with user-visible feedback ---

        if pending_perm.is_none() && pending_question.is_none() {
            if task_id.is_some() {
                tracing::warn!(
                    "handle_modal_confirm: no pending permission/question found for task {:?}",
                    task_id
                );
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.permission_modal_active = false;
                state.ui.permission_modal_selected_index = 0;
                state.set_notification(
                    "No pending permission or question to resolve".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    3000,
                );
                state.mark_render_dirty();
            } else {
                tracing::debug!("handle_modal_confirm: no active task context");
            }
            return;
        }

        if client.is_none() {
            tracing::error!("handle_modal_confirm: no OpenCode client for active project");
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.set_notification(
                "Cannot resolve: no server connection for this project".to_string(),
                crate::state::types::NotificationVariant::Error,
                5000,
            );
            state.mark_render_dirty();
            return;
        }

        // At this point: client, task_id, and at least one of pending_perm/pending_question are Some.
        let client = client.unwrap();
        let task_id = task_id.unwrap();

        if let Some(perm) = pending_perm {
            // Permission — selected_index 0 = Yes (approve), 1+ = No (reject)
            let approve = selected_index == 0;
            let state = self.state.clone();
            let perm_id = perm.id.clone();
            let session_id = perm.session_id.clone();
            let tid = task_id.clone();
            tokio::spawn(async move {
                match client
                    .resolve_permission(&session_id, &perm_id, approve)
                    .await
                {
                    Ok(()) => {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.resolve_permission_request(&tid, &perm_id, approve);
                        // Check if more permissions/questions remain
                        let has_more = s
                            .session_tracker
                            .task_sessions
                            .get(&tid)
                            .map(|sess| {
                                !sess.pending_permissions.is_empty()
                                    || !sess.pending_questions.is_empty()
                            })
                            .unwrap_or(false);
                        if has_more {
                            s.ui.permission_modal_selected_index = 0;
                        } else {
                            s.ui.permission_modal_active = false;
                            s.ui.permission_modal_selected_index = 0;
                        }
                        s.mark_render_dirty();
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to resolve permission {}: {}",
                            perm_id,
                            e
                        );
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.set_notification(
                            format!("Failed to resolve permission: {}", e),
                            crate::state::types::NotificationVariant::Error,
                            5000,
                        );
                        s.mark_render_dirty();
                    }
                }
            });
        } else if let Some(question) = pending_question {
            // Question — selected_index maps to answer index
            if selected_index < question.answers.len() {
                let answer = question.answers[selected_index].clone();
                let state = self.state.clone();
                let columns_config = self.config.columns.clone();
                let opencode_config = self.config.opencode.clone();
                let question_id = question.id.clone();
                let session_id = question.session_id.clone();
                let answer_preview = answer.chars().take(30).collect::<String>();
                let tid = task_id.clone();
                tokio::spawn(async move {
                    match client
                        .resolve_question(&session_id, &question_id, &answer)
                        .await
                    {
                        Ok(()) => {
                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                            s.resolve_question_request(&tid, &question_id);

                            // Check if task should transition out of Question status
                            let needs_reassess = s.should_reassess_after_question(&tid);
                            if needs_reassess {
                                let (status, should_progress) =
                                    crate::opencode::events::determine_completion_status(
                                        &mut s, &tid,
                                    );
                                s.update_task_agent_status(&tid, status);

                                if should_progress {
                                    let action =
                                        crate::orchestration::engine::on_agent_completed(
                                            &tid,
                                            &mut s,
                                            &columns_config,
                                        );
                                    if let Some(a) = action {
                                        match a {
                                            crate::orchestration::engine::AgentCompletionAction::AutoProgress(ap) => {
                                                let col = ap.target_column.clone();
                                                let tid_clone = tid.clone();
                                                drop(s);
                                                crate::orchestration::engine::on_task_moved(
                                                    &tid_clone,
                                                    &col,
                                                    &state,
                                                    &client,
                                                    &columns_config,
                                                    &opencode_config,
                                                    None,
                                                );
                                            }
                                            crate::orchestration::engine::AgentCompletionAction::SendQueuedPrompt {
                                                task_id: qp_tid,
                                                prompt: qp_prompt,
                                                session_id: qp_sid,
                                                agent_type: qp_agent,
                                            } => {
                                                drop(s);
                                                crate::orchestration::engine::send_follow_up_prompt(
                                                    &qp_tid,
                                                    &qp_prompt,
                                                    &qp_sid,
                                                    &qp_agent,
                                                    &state,
                                                    &client,
                                                    &opencode_config,
                                                );
                                            }
                                        }
                                    }
                                }
                            }

                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                            // Check if more questions remain
                            let has_more = s
                                .session_tracker
                                .task_sessions
                                .get(&tid)
                                .map(|sess| {
                                    !sess.pending_questions.is_empty()
                                        || !sess.pending_permissions.is_empty()
                                })
                                .unwrap_or(false);
                            if has_more {
                                s.ui.permission_modal_selected_index = 0;
                            } else {
                                s.ui.permission_modal_active = false;
                                s.ui.permission_modal_selected_index = 0;
                            }
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
                                question_id,
                                e
                            );
                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
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
        }
    }
}

/// Update the focused task ID based on the column's focused task index.
fn update_focused_task_id(state: &mut AppState, col_id: &str) {
    let idx = state
        .kanban
        .focused_task_index
        .get(col_id)
        .copied()
        .unwrap_or(0);
    if let Some(task_ids) = state.kanban.columns.get(col_id) {
        let clamped = idx.min(task_ids.len().saturating_sub(1));
        state.ui.focused_task_id = task_ids.get(clamped).cloned();
    }
}
