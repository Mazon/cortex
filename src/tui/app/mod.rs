//! TUI App struct and event loop.
//!
//! This module is the entry point for the TUI application. The `App` struct
//! owns the terminal, application state, and configuration. The event loop
//! dispatches to mode-specific handlers defined in sub-modules.
//!
//! # Module layout
//!
//! - [`mouse`] — mouse event handling (click, scroll)
//! - [`handlers`] — action handlers (project CRUD, kanban nav, task ops, review, subagent)
//! - [`modes`] — mode-specific key handlers (normal, editor, diff review, reports)
//! - [`permission`] — permission modal helpers (resolve, confirm, sync)
//! - [`utils`] — shared utilities (git diff parsing, question resolution)

mod handlers;
mod modes;
mod mouse;
mod permission;
pub mod utils;

use crate::config::types::CortexConfig;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::AppState;
use crate::tui::{CrosstermBackend, Terminal};
use crossterm::event::{self, Event, KeyEventKind};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
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
    /// Path to the cortex.toml configuration file.
    pub config_path: PathBuf,
    /// Pre-computed key matcher — built once from config, avoids per-keypress allocation.
    pub(crate) key_matcher: crate::tui::keys::KeyMatcher,
    /// Pre-computed editor key matcher — built once from config, avoids per-keypress allocation.
    pub(crate) editor_key_matcher: crate::tui::keys::EditorKeyMatcher,
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
        config_path: PathBuf,
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
            config_path,
            opencode_clients,
            key_matcher,
            editor_key_matcher,
        })
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(self.config.theme.tick_rate_ms);

        // Set up graceful shutdown via a background signal-handler task.
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
                                        mouse::handle_mouse_event(self, mouse);
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
            let needs_render = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take_render_dirty();
            if needs_render {
                let config = &self.config;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // Auto-open permission modal when permissions/questions arrive
                let dismissed_recently = state.ui.permission_modal_dismissed_at
                    .map(|t| t.elapsed().as_secs() < 5)
                    .unwrap_or(false);
                if !state.ui.permission_modal_active
                    && !dismissed_recently
                    && state.ui.mode == crate::state::types::AppMode::Normal
                    && state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail
                {
                    if let Some(ref tid) = state.ui.viewing_task_id {
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
                        crate::state::types::AppMode::ConfirmDelete => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::confirm_dialog::render_confirm_delete_dialog(
                                f, f.area(), state,
                            );
                        }
                        crate::state::types::AppMode::Archive => {
                            crate::tui::render_normal(f, state, config);
                            crate::tui::archive_viewer::render_archive_viewer(
                                f, f.area(), state, &config.theme,
                            );
                        }
                        crate::state::types::AppMode::ConfigEditor => {
                            crate::tui::config_editor::render_config_editor(
                                f, state, config,
                            );
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
                modes::normal::handle_normal_key(self, key);
            }
            crate::state::types::AppMode::TaskEditor => {
                modes::editor::handle_editor_key(self, key);
            }
            crate::state::types::AppMode::Help => {
                // Any key dismisses the help overlay
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.mode = crate::state::types::AppMode::Normal;
            }
            crate::state::types::AppMode::ProjectRename => {
                modes::editor::handle_rename_key(self, key);
            }
            crate::state::types::AppMode::InputPrompt => {
                modes::editor::handle_input_prompt_key(self, key);
            }
            crate::state::types::AppMode::DiffReview => {
                modes::diff_review::handle_diff_review_key(self, key);
            }
            crate::state::types::AppMode::Reports => {
                modes::reports::handle_reports_key(self, key);
            }
            crate::state::types::AppMode::ConfirmDelete => {
                modes::confirm::handle_confirm_delete_key(self, key);
            }
            crate::state::types::AppMode::Archive => {
                modes::archive::handle_archive_key(self, key);
            }
            crate::state::types::AppMode::ConfigEditor => {
                modes::config_editor::handle_config_editor_key(self, key);
            }
        }
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
}
