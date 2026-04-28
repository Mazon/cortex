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
                let mut sigterm = match tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                ) {
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
                                            .unwrap()
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
            let needs_render = self.state.lock().unwrap_or_else(|e| e.into_inner()).take_render_dirty();
            if needs_render {
                let config = &self.config;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
                        crate::state::types::AppMode::DiffReview => {
                            crate::tui::diff_view::render_diff_review(f, f.area(), state, &config.theme);
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

        // Ignore clicks in the status bar (last row)
        if mouse.row >= area.height.saturating_sub(1) {
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
                state.kanban.kanban_scroll_offset.min(visible.len().saturating_sub(max_visible))
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
                state.kanban.focused_task_index.insert(clicked_col_id.clone(), task_index);
                state.ui.focused_task_id = Some(task_id);
            }
            state.mark_render_dirty();
        }
    }

    /// Handle a key event based on current mode.
    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        // Any key press potentially changes state — mark for re-render.
        self.state.lock().unwrap_or_else(|e| e.into_inner()).mark_render_dirty();

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
                // Any key dismisses help
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.mode = crate::state::types::AppMode::Normal;
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
                    && state.ui.detail_editor.as_ref().map_or(false, |e| e.is_focused)
            };

            if is_detail_editor_focused {
                use crate::state::types::CursorDirection;
                use crate::tui::keys::EditorKeyAction;

                // Check configurable editor keybindings first (Ctrl+S, Esc, Tab, Enter)
                let editor_action = {
                    let _state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    self.editor_key_matcher.match_key(key)
                };

                if let Some(action) = editor_action {
                    match action {
                        EditorKeyAction::Save => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            match state.save_detail_description() {
                                Ok(task_id) => {
                                    // Unfocus the editor after saving
                                    if let Some(ed) = state.ui.detail_editor.as_mut() {
                                        ed.is_focused = false;
                                    }
                                    state.set_notification(
                                        format!("Description saved for task {}", task_id),
                                        crate::state::types::NotificationVariant::Success,
                                        3000,
                                    );
                                    state.mark_render_dirty();
                                }
                                Err(e) => {
                                    state.set_notification(
                                        e.to_string(),
                                        crate::state::types::NotificationVariant::Error,
                                        3000,
                                    );
                                    state.mark_render_dirty();
                                }
                            }
                            return;
                        }
                        EditorKeyAction::Cancel => {
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            let should_revert = state.ui.detail_editor.as_ref().map_or(false, |ed| ed.discard_warning_shown);
                            let has_unsaved = state.ui.detail_editor.as_ref().map_or(false, |ed| ed.has_unsaved_changes);

                            if should_revert {
                                // Second cancel: revert and unfocus
                                // Extract description before mutating editor
                                let revert_desc = state.ui.viewing_task_id.as_ref().and_then(|id| {
                                    state.tasks.get(id).map(|t| {
                                        t.pending_description.clone().unwrap_or_else(|| t.description.clone())
                                    })
                                });

                                if let (Some(desc), Some(ed)) = (revert_desc, state.ui.detail_editor.as_mut()) {
                                    let fresh = crate::state::types::DetailEditorState::new_from_description(&desc);
                                    ed.desc_lines = fresh.desc_lines;
                                    ed.cached_description = fresh.cached_description;
                                    ed.cursor_row = 0;
                                    ed.cursor_col = 0;
                                    ed.scroll_offset = 0;
                                    ed.has_unsaved_changes = false;
                                    ed.discard_warning_shown = false;
                                    ed.validation_error = None;
                                    ed.is_focused = false;
                                }
                            } else if has_unsaved {
                                // First cancel with unsaved changes: show warning
                                if let Some(ed) = state.ui.detail_editor.as_mut() {
                                    ed.discard_warning_shown = true;
                                }
                            } else {
                                // No unsaved changes: just unfocus
                                if let Some(ed) = state.ui.detail_editor.as_mut() {
                                    ed.is_focused = false;
                                }
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
                        EditorKeyAction::Newline => {
                            // Enter: insert newline (only without ctrl/alt modifiers)
                            if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                                // Ctrl+Enter = save, don't insert newline
                                return;
                            }
                            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(ed) = state.ui.detail_editor.as_mut() {
                                ed.insert_newline();
                            }
                            state.mark_render_dirty();
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
                        (KeyCode::Char(ch), modifiers) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
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
                    && state.ui.detail_editor.as_ref().map_or(false, |e| !e.is_focused)
            };
            if is_detail_not_focused && key.code == KeyCode::Tab && key.modifiers.is_empty() {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ed) = state.ui.detail_editor.as_mut() {
                    ed.is_focused = true;
                }
                state.mark_render_dirty();
                return;
            }
        }

        // Check if we're in task detail view — Escape pops subagent stack or closes detail
        {
            let is_detail_escape = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail && key.code == KeyCode::Esc
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
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Char('G') | KeyCode::Char('g')
            );
            let modifiers_ok = if matches!(key.code, KeyCode::Char('G')) {
                key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
            } else {
                key.modifiers.is_empty()
            };
            if is_scroll_key && modifiers_ok
            {
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
                        let perm = state
                            .session_tracker.task_sessions
                            .get(tid)
                            .and_then(|s| s.pending_permissions.first().cloned());
                        let client = state
                            .project_registry.active_project_id
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
                            match client.resolve_permission(&session_id, &perm_id, approve).await {
                                Ok(()) => {
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.resolve_permission_request(&tid, &perm_id, approve);
                                    s.mark_render_dirty();
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to resolve permission {}: {}",
                                        perm_id, e
                                    );
                                    // Keep the permission in the pending list so the user can retry
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
                        let question = state
                            .session_tracker.task_sessions
                            .get(tid)
                            .and_then(|s| s.pending_questions.first().cloned())
                            .filter(|q| answer_index < q.answers.len());
                        let client = state
                            .project_registry.active_project_id
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
                            match client.resolve_question(&session_id, &question_id, &answer).await {
                                Ok(()) => {
                                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.resolve_question_request(&tid, &question_id);

                                    // Check if the task should transition out of Question status
                                    let needs_reassess = s.should_reassess_after_question(&tid);
                                    if needs_reassess {
                                        // Set back to Complete, then re-apply Ready/Complete logic
                                        // and auto-progression (same as SessionIdle handler).
                                        s.update_task_agent_status(&tid, crate::state::types::AgentStatus::Complete);
                                        if let Some(ref col) = s.tasks.get(&tid).map(|t| t.column.clone()) {
                                            let has_auto_progress =
                                                columns_config.auto_progress_for(&col.0).is_some();
                                            let has_plan = s
                                                .tasks
                                                .get(&tid)
                                                .and_then(|t| t.plan_output.as_ref())
                                                .map(|p| !p.trim().is_empty())
                                                .unwrap_or(false);
                                            if has_auto_progress || has_plan {
                                                s.update_task_agent_status(&tid, crate::state::types::AgentStatus::Ready);
                                            }
                                        }
                                        let action = crate::orchestration::engine::on_agent_completed(
                                            &tid, &mut s, &columns_config,
                                        );
                                        if let Some(a) = action {
                                            let col = a.target_column.clone();
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
                                        question_id, e
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
                    return;
                } else {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.ui.focused_panel == crate::state::types::FocusedPanel::TaskDetail {
                        if let Some(ref tid) = state.ui.viewing_task_id {
                            if state
                                .session_tracker.task_sessions
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
                    let was_tripped = state.project_registry.is_circuit_breaker_tripped(pid, self.config.opencode.circuit_breaker_threshold);
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
            Some(Action::ScrollKanbanLeft) => self.handle_scroll_kanban(-1),
            Some(Action::ScrollKanbanRight) => self.handle_scroll_kanban(1),
            Some(Action::MoveTaskUp) => self.handle_move_task_vertical(-1),
            Some(Action::MoveTaskDown) => self.handle_move_task_vertical(1),
            None => {} // Unmatched key, ignore
        }
    }

    // ── Individual action handlers (extracted from handle_normal_key) ──

    fn handle_quit(&mut self) {
        self.should_quit = true;
    }

    fn handle_help_toggle(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        let id = uuid::Uuid::new_v4().to_string();
        let pos = state.project_registry.projects.len();
        let project = crate::state::types::CortexProject {
            id: id.clone(),
            name: format!("Project {}", pos + 1),
            working_directory: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            status: crate::state::types::ProjectStatus::Idle,
            position: pos,
            ..Default::default()
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
                        .project_registry.projects
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
        if let Some(id) = state.project_registry.projects.first().map(|p| p.id.clone()) {
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
        let current = state.kanban.focused_task_index.get(&col_id).copied().unwrap_or(0);
        let new_idx = current as i32 + direction;
        if new_idx >= 0 && (new_idx as usize) < task_count {
            state.kanban.focused_task_index.insert(col_id.clone(), new_idx as usize);
            update_focused_task_id(&mut state, &col_id);
        }
    }

    fn handle_create_task(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let col_id = state.ui.focused_column.clone();
        state.open_task_editor_create(&col_id);
    }

    fn handle_open_task_detail(&mut self) {
        let task_id = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.focused_task_id.clone()
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match task_id {
            Some(id) => state.open_task_detail(&id),
            None => state.set_notification(
                "No task selected".to_string(),
                crate::state::types::NotificationVariant::Info,
                2000,
            ),
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
                        let already_running = state.tasks.get(&tid)
                            .map(|t| matches!(t.agent_status,
                                crate::state::types::AgentStatus::Running
                                | crate::state::types::AgentStatus::Hung))
                            .unwrap_or(false);
                        if already_running {
                            let status = state.tasks.get(&tid)
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
                            if let Some(project_id) = state.project_registry.active_project_id.clone() {
                                if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
                                    // Capture the PREVIOUS agent type before overwriting it,
                                    // so start_agent can detect the change and create a fresh session.
                                    let previous_agent = state.tasks.get(&tid)
                                        .and_then(|t| t.agent_type.clone());
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
                                if let Err(_e) = client.abort_session(&session_id).await {
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
                .project_registry.active_project_id
                .as_ref()
                .and_then(|pid| self.opencode_clients.get(pid))
                .cloned();
            (session_id, client)
        };

        if let Some(sid) = session_id {
            if let Some(client) = client {
                let state = self.state.clone();
                tokio::spawn(async move {
                    match client.abort_session(&sid).await {
                        Ok(aborted) => {
                            let _ = aborted;
                        }
                        Err(e) => {
                            tracing::error!("Failed to abort session {}: {}", sid, e);
                        }
                    }
                    // Update notification after attempt
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.set_notification(
                        format!("Session abort requested: {}", sid),
                        crate::state::types::NotificationVariant::Warning,
                        3000,
                    );
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
                .project_registry.active_project_id
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
                if let Some(task) = state.tasks.get_mut(&task_id) {
                    task.error_message = None;
                }
                state.update_task_agent_status(&task_id, AgentStatus::Running);
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
                s.session_tracker.subagent_session_data.get(&session_id)
                    .map(|d| d.messages.is_empty())
                    .unwrap_or(true)
            };

            if needs_fetch {
                if let Some(client) = client {
                    match client.fetch_subagent_messages(&session_id).await {
                        Ok(messages) => {
                            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                            let entry = s
                                .session_tracker.subagent_session_data
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
            let already_on_stack = s.ui.session_nav_stack
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

    /// Open the diff review view for the focused task.
    ///
    /// Runs `git diff HEAD` in the project's working directory, parses the
    /// output, and switches to `AppMode::DiffReview`.  The git command runs
    /// on a background thread so the UI stays responsive.
    fn handle_review_changes(&mut self) {
        use crate::state::types::{AgentStatus, AppMode, DiffReviewState};

        // Batch-read everything we need while holding the lock once.
        let (working_dir, task_number) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());

            let tid = match state.ui.focused_task_id.as_ref() {
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
                let current_depth = state.ui.session_nav_stack.last().map(|r| r.depth).unwrap_or(0);
                for msg in &session_data.messages {
                    for part in &msg.parts {
                        if let crate::state::types::TaskMessagePart::Agent { id, agent } = part {
                            let already_in_stack = state
                                .ui
                                .session_nav_stack
                                .iter()
                                .any(|r| r.session_id == *id);
                            if !already_in_stack {
                                return Some((id.clone(), agent.clone(), task_id, current_depth + 1));
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
                            if let crate::state::types::TaskMessagePart::Agent { id, agent } = part {
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

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = state.kanban.kanban_scroll_offset as i32;
        let max_offset = (total_cols.saturating_sub(max_visible)) as i32;
        let new_offset = (current + direction).clamp(0, max_offset);
        state.kanban.kanban_scroll_offset = new_offset as usize;
    }

    // ── Shared helpers ──

    /// Handle key events in DiffReview mode.
    fn handle_diff_review_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.ui.mode = crate::state::types::AppMode::Normal;
                state.ui.diff_review = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::scroll_diff(review, 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::scroll_diff(review, -1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::next_file(review);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut review) = state.ui.diff_review {
                    crate::tui::diff_view::prev_file(review);
                }
            }
            _ => {}
        }
    }

    /// Reorder a task within its column by swapping position with its neighbor.
    fn handle_move_task_vertical(&mut self, direction: i32) {
        let (task_id, column_id) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let tid = state.ui.focused_task_id.clone();
            let col = state.ui.focused_column.clone();
            (tid, col)
        };
        let task_id = match task_id {
            Some(id) => id,
            None => return,
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tasks) = state.kanban.columns.get_mut(&column_id) {
            if let Some(pos) = tasks.iter().position(|id| id == &task_id) {
                let new_pos = pos as i32 + direction;
                if new_pos >= 0 && (new_pos as usize) < tasks.len() {
                    tasks.swap(pos, new_pos as usize);
                }
            }
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
            .project_registry.active_project_id
            .as_ref()
            .and_then(|id| state.project_registry.projects.iter().position(|p| &p.id == id))
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
    fn handle_text_input(
        &mut self,
        key: crossterm::event::KeyEvent,
        prompt: InputPrompt,
    ) {
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match prompt {
                    InputPrompt::RenameProject => state.cancel_project_rename(),
                    InputPrompt::WorkingDirectory => state.cancel_working_directory(),
                }
            }
            KeyCode::Char(c) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Handle key events in InputPrompt mode (used for working directory).
    fn handle_input_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        self.handle_text_input(key, InputPrompt::WorkingDirectory);
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
                            let agent_name = self.config.columns.agent_for_column(col_id);
                            tracing::debug!(
                                "Task {} saved in column '{}', agent_for_column={:?}",
                                task_id, col_id, agent_name
                            );
                            if let Some(_agent) = agent_name {
                                // Check if task already has a running agent
                                let already_running = state.tasks.get(&task_id)
                                    .map(|t| matches!(t.agent_status,
                                        crate::state::types::AgentStatus::Running
                                        | crate::state::types::AgentStatus::Hung))
                                    .unwrap_or(false);

                                if already_running {
                                    let status = state.tasks.get(&task_id)
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
                                    if let Some(project_id) = state.project_registry.active_project_id.clone() {
                                        if let Some(client) = self.opencode_clients.get(&project_id).cloned() {
                                            // Capture the PREVIOUS agent type before overwriting it,
                                            // so start_agent can detect the change and create a fresh session.
                                            let previous_agent = state.tasks.get(&task_id)
                                                .and_then(|t| t.agent_type.clone());
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
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Get the OpenCode client for the active project, or `None` if unavailable.
    fn get_active_client(&self) -> Option<OpenCodeClient> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .project_registry.active_project_id
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
}

/// Update the focused task ID based on the column's focused task index.
fn update_focused_task_id(state: &mut AppState, col_id: &str) {
    let idx = state.kanban.focused_task_index.get(col_id).copied().unwrap_or(0);
    if let Some(task_ids) = state.kanban.columns.get(col_id) {
        let clamped = idx.min(task_ids.len().saturating_sub(1));
        state.ui.focused_task_id = task_ids.get(clamped).cloned();
    }
}
