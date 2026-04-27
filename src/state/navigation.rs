//! UI/navigation methods on AppState.
//!
//! Extracted from `store.rs` to separate navigation, focus management,
//! task detail view, subagent drill-down, task editor, and prompt handling
//! from pure CRUD and SSE processing.

use crate::state::types::*;

impl AppState {
    // ─── Navigation ──────────────────────────────────────────────────────

    /// Clamp the focused task index for a column so it stays within bounds.
    /// Syncs `ui.focused_task_id` with the clamped index **only** when the
    /// column being clamped is the user's currently focused column.
    /// Auto-progression events can move tasks in background columns —
    /// those must not corrupt the user's active focus.
    pub fn clamp_focused_task_index(&mut self, col_id: &str) {
        let idx = self
            .kanban
            .focused_task_index
            .get(col_id)
            .copied()
            .unwrap_or(0);
        if let Some(tasks) = self.kanban.columns.get(col_id) {
            let clamped = if tasks.is_empty() {
                0
            } else {
                idx.min(tasks.len() - 1)
            };
            self.kanban
                .focused_task_index
                .insert(col_id.to_string(), clamped);
            // Only update focused_task_id when clamping the currently focused column.
            if self.ui.focused_column == col_id {
                self.ui.focused_task_id = tasks.get(clamped).cloned();
            }
        } else {
            if self.ui.focused_column == col_id {
                self.ui.focused_task_id = None;
            }
        }
    }

    /// Set the focused column and sync the focused task ID.
    pub fn set_focused_column(&mut self, column: &str) {
        self.ui.focused_column = column.to_string();
        // Reset focused task index for this column
        let idx = self
            .kanban
            .focused_task_index
            .entry(column.to_string())
            .or_insert(0);
        // Sync focused_task_id with the column's focused index (clamped)
        if let Some(task_ids) = self.kanban.columns.get(column) {
            let clamped = (*idx).min(task_ids.len().saturating_sub(1));
            self.ui.focused_task_id = task_ids.get(clamped).cloned();
        }
    }

    /// Set the focused task ID directly.
    pub fn set_focused_task(&mut self, task_id: Option<String>) {
        self.ui.focused_task_id = task_id;
    }

    /// Open the task detail panel for a given task.
    /// Initializes the inline description editor from the task's current description
    /// (or `pending_description` if set).
    pub fn open_task_detail(&mut self, task_id: &str) {
        self.ui.viewing_task_id = Some(task_id.to_string());
        self.ui.focused_panel = FocusedPanel::TaskDetail;
        self.ui.user_scroll_offset = None;

        // Initialize the inline description editor from the task's description
        if let Some(task) = self.tasks.get(task_id) {
            let desc = task.pending_description.as_deref().unwrap_or(&task.description);
            self.ui.detail_editor = Some(DetailEditorState::new_from_description(desc));
        }
    }

    /// Close the task detail panel and return focus to the kanban board.
    /// Clears the drill-down navigation stack and the inline editor state.
    pub fn close_task_detail(&mut self) {
        self.ui.viewing_task_id = None;
        self.ui.focused_panel = FocusedPanel::Kanban;
        self.ui.user_scroll_offset = None;
        self.ui.session_nav_stack.clear();
        self.ui.detail_editor = None;
    }

    /// Save the description from the inline detail editor.
    ///
    /// If the task is in `Ready` or `Complete` status, the description is applied
    /// immediately (and `pending_description` is cleared). Otherwise, the new
    /// description is stored in `pending_description` and will be applied when
    /// the task reaches a terminal state.
    ///
    /// Returns the task ID on success, or an error if validation fails.
    pub fn save_detail_description(&mut self) -> anyhow::Result<String> {
        let task_id = match &self.ui.viewing_task_id {
            Some(id) => id.clone(),
            None => anyhow::bail!("No task being viewed"),
        };

        let description = match &self.ui.detail_editor {
            Some(editor) => editor.description(),
            None => anyhow::bail!("No detail editor open"),
        };

        let description = description.trim().to_string();
        if description.is_empty() {
            if let Some(ed) = self.ui.detail_editor.as_mut() {
                ed.validation_error = Some("Description cannot be empty".to_string());
            }
            anyhow::bail!("Task description cannot be empty");
        }

        let title = derive_title_from_description(&description);
        let now = chrono::Utc::now().timestamp();

        if let Some(task) = self.tasks.get_mut(&task_id) {
            let can_apply_immediately = matches!(
                task.agent_status,
                AgentStatus::Ready | AgentStatus::Complete
            );

            if can_apply_immediately {
                // Apply immediately — task is in a terminal state
                task.description = description;
                task.title = title;
                task.pending_description = None;
            } else {
                // Queue for later — store in pending_description
                task.pending_description = Some(description.clone());
                // Also update title to reflect the pending change
                task.title = title;
            }
            task.updated_at = now;
            self.mark_task_dirty(&task_id);

            // Reset unsaved changes flags
            if let Some(ed) = self.ui.detail_editor.as_mut() {
                ed.has_unsaved_changes = false;
                ed.discard_warning_shown = false;
                ed.validation_error = None;
            }
        } else {
            anyhow::bail!("Task not found: {}", task_id)
        }

        Ok(task_id)
    }

    // ─── Subagent Drill-Down Navigation ──────────────────────────────────

    /// Push a subagent session onto the drill-down navigation stack.
    ///
    /// When the user drills into a subagent (e.g., via `ctrl+x`), the
    /// session reference is pushed onto the stack. The task detail view
    /// then renders the top-of-stack session's output instead of the
    /// parent task's output.
    pub fn push_subagent_drilldown(&mut self, session_ref: SessionRef) {
        self.ui.session_nav_stack.push(session_ref);
        // Reset scroll to auto-scroll when drilling into a new session
        self.ui.user_scroll_offset = None;
        self.mark_render_dirty();
    }

    /// Pop the top session from the drill-down navigation stack.
    ///
    /// Returns the popped `SessionRef` if the stack was non-empty,
    /// or `None` if already at the top level (viewing the parent task).
    pub fn pop_subagent_drilldown(&mut self) -> Option<SessionRef> {
        let popped = self.ui.session_nav_stack.pop();
        if popped.is_some() {
            // Reset scroll to auto-scroll when navigating back
            self.ui.user_scroll_offset = None;
            self.mark_render_dirty();
        }
        popped
    }

    /// Clear the entire drill-down navigation stack.
    pub fn clear_subagent_drilldown(&mut self) {
        self.ui.session_nav_stack.clear();
        self.ui.user_scroll_offset = None;
    }

    /// Get the session ID of the currently drilled-down subagent, if any.
    ///
    /// Returns `None` if the stack is empty (viewing the parent task).
    pub fn get_drilldown_session_id(&self) -> Option<&str> {
        self.ui.session_nav_stack.last().map(|r| r.session_id.as_str())
    }

    /// Check if the user is currently drilled into a subagent.
    pub fn is_drilled_into_subagent(&self) -> bool {
        !self.ui.session_nav_stack.is_empty()
    }

    /// Get the navigation stack as a breadcrumb string (e.g., "Task #3 > planning > do").
    pub fn get_drilldown_breadcrumb(&self) -> String {
        if self.ui.session_nav_stack.is_empty() {
            return String::new();
        }
        self.ui
            .session_nav_stack
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<&str>>()
            .join(" > ")
    }

    // ─── Task Editor Mode ────────────────────────────────────────────────

    /// Open the task editor in "create" mode.
    pub fn open_task_editor_create(
        &mut self,
        default_column: &str,
    ) {
        self.ui.task_editor = Some(TaskEditorState::new_for_create(default_column));
        self.ui.mode = AppMode::TaskEditor;
    }

    /// Open the task editor in "edit" mode for an existing task.
    /// Does nothing if the task doesn't exist.
    pub fn open_task_editor_edit(&mut self, task_id: &str, available_columns: Vec<String>) {
        if let Some(task) = self.tasks.get(task_id) {
            self.ui.task_editor = Some(TaskEditorState::new_for_edit(task, available_columns));
            self.ui.mode = AppMode::TaskEditor;
        }
    }

    /// Save the current task editor contents. Creates a new task or updates
    /// an existing one. Returns the task ID on success.
    pub fn save_task_editor(&mut self) -> anyhow::Result<String> {
        let editor = match &self.ui.task_editor {
            Some(e) => e.clone(),
            None => anyhow::bail!("No task editor open"),
        };

        let (title, description) = editor.to_task_fields();
        let description = description.trim().to_string();
        if description.is_empty() {
            // Set inline validation error so it's visible in the editor UI,
            // instead of relying solely on a transient notification toast.
            if let Some(ed) = self.ui.task_editor.as_mut() {
                ed.validation_error = Some("Description cannot be empty".to_string());
            }
            anyhow::bail!("Task description cannot be empty");
        }

        let now = chrono::Utc::now().timestamp();
        let column_id = editor.column_id.as_deref().unwrap_or(KanbanColumn::TODO);

        match &editor.task_id {
            Some(task_id) => {
                // Editing existing task — check if column changed before mutable borrow
                let needs_column_move = {
                    let task = self.tasks.get(task_id);
                    task.map(|t| t.column.0.as_str() != column_id).unwrap_or(false)
                };

                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.title = title;
                    task.description = description;
                    task.updated_at = now;
                    self.mark_task_dirty(task_id);
                    // Reset unsaved changes flags after successful save
                    if let Some(ed) = self.ui.task_editor.as_mut() {
                        ed.has_unsaved_changes = false;
                        ed.discard_warning_shown = false;
                        ed.validation_error = None;
                    }
                } else {
                    anyhow::bail!("Task not found: {}", task_id)
                }

                // Apply column change if the editor has a different column selected
                if needs_column_move {
                    self.move_task(task_id, KanbanColumn(column_id.to_string()));
                }
                Ok(task_id.clone())
            }
            None => {
                // Creating new task
                let project_id = self
                    .project_registry.active_project_id
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                let task = self.create_todo(title, description, &project_id);
                // Move to target column if not todo
                if column_id != KanbanColumn::TODO {
                    self.move_task(&task.id, KanbanColumn(column_id.to_string()));
                }
                // Reset unsaved changes flags after successful save
                if let Some(ed) = self.ui.task_editor.as_mut() {
                    ed.has_unsaved_changes = false;
                    ed.discard_warning_shown = false;
                    ed.validation_error = None;
                }
                Ok(task.id)
            }
        }
    }

    /// Close the task editor and return to normal mode, discarding changes.
    pub fn cancel_task_editor(&mut self) {
        self.ui.task_editor = None;
        self.ui.mode = AppMode::Normal;
    }

    /// Get an immutable reference to the current task editor state, if open.
    pub fn get_task_editor(&self) -> Option<&TaskEditorState> {
        self.ui.task_editor.as_ref()
    }

    /// Get a mutable reference to the current task editor state, if open.
    pub fn get_task_editor_mut(&mut self) -> Option<&mut TaskEditorState> {
        self.ui.task_editor.as_mut()
    }

    // ─── Working Directory ────────────────────────────────────────────────

    /// Open an input prompt to set the active project's working directory.
    /// Pre-populates the input with the current working directory.
    /// No-op if no project is active.
    pub fn open_set_working_directory(&mut self) {
        let current_dir = match self.project_registry.active_project_id.as_ref() {
            Some(pid) => self
                .project_registry.projects
                .iter()
                .find(|p| &p.id == pid)
                .map(|p| p.working_directory.clone()),
            None => None,
        };

        match current_dir {
            Some(dir) => {
                self.ui.input_text = dir;
                self.ui.input_cursor = self.ui.input_text.chars().count();
                self.ui.prompt_label = "Set working directory:".to_string();
                self.ui.prompt_context = Some("set_working_directory".to_string());
                self.ui.mode = AppMode::InputPrompt;
            }
            None => {
                self.set_notification(
                    "No active project".to_string(),
                    NotificationVariant::Warning,
                    3000,
                );
            }
        }
    }

    /// Submit the working directory change. Applies the entered path to the
    /// active project.
    ///
    /// Returns:
    /// - `Ok(true)` if the directory was accepted and applied.
    /// - `Ok(false)` if the path is empty or no project is active.
    /// - `Err(message)` if the path is invalid (doesn't exist or isn't a directory).
    pub fn submit_working_directory(&mut self) -> Result<bool, String> {
        let dir = if self.ui.input_text.trim().is_empty() {
            return Ok(false);
        } else {
            self.ui.input_text.trim().to_string()
        };

        let project_id = match self.project_registry.active_project_id.clone() {
            Some(id) => id,
            None => return Ok(false),
        };

        // Validate the path before accepting it.
        let path = std::path::Path::new(&dir);
        if !path.exists() {
            return Err(format!("Directory does not exist: {}", dir));
        }
        if !path.is_dir() {
            return Err(format!("Path is not a directory: {}", dir));
        }

        if let Some(project) = self.project_registry.projects.iter_mut().find(|p| p.id == project_id) {
            project.working_directory = dir;
            // Reset prompt state and return to normal mode
            self.ui.input_text.clear();
            self.ui.input_cursor = 0;
            self.ui.prompt_label.clear();
            self.ui.prompt_context = None;
            self.ui.mode = AppMode::Normal;
            self.mark_dirty();
            self.mark_render_dirty();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Cancel the working directory prompt, discarding changes.
    pub fn cancel_working_directory(&mut self) {
        self.ui.input_text.clear();
        self.ui.input_cursor = 0;
        self.ui.prompt_label.clear();
        self.ui.prompt_context = None;
        self.ui.mode = AppMode::Normal;
    }

    // ─── Project Rename ─────────────────────────────────────────────────

    /// Open the project rename prompt, pre-populating the input with the
    /// current project name. No-op if no project is active.
    pub fn open_project_rename(&mut self) {
        let current_name = match self.project_registry.active_project_id.as_ref() {
            Some(pid) => self
                .project_registry.projects
                .iter()
                .find(|p| &p.id == pid)
                .map(|p| p.name.clone()),
            None => None,
        };

        match current_name {
            Some(name) => {
                self.ui.input_text = name;
                self.ui.input_cursor = self.ui.input_text.chars().count();
                self.ui.prompt_label = "Rename project to:".to_string();
                self.ui.prompt_context = Some("rename_project".to_string());
                self.ui.mode = AppMode::ProjectRename;
            }
            None => {
                self.set_notification(
                    "No active project to rename".to_string(),
                    NotificationVariant::Warning,
                    3000,
                );
            }
        }
    }

    /// Submit the project rename. Applies the new name to the active project.
    /// Returns the old and new names, or `None` if no project is active.
    pub fn submit_project_rename(&mut self) -> Option<(String, String)> {
        let new_name = if self.ui.input_text.trim().is_empty() {
            return None;
        } else {
            self.ui.input_text.trim().to_string()
        };

        let project_id = self.project_registry.active_project_id.clone()?;
        let old_name = self
            .project_registry.projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .map(|p| {
                let old = p.name.clone();
                p.name = new_name.clone();
                old
            })?;

        // Reset prompt state and return to normal mode
        self.ui.input_text.clear();
        self.ui.input_cursor = 0;
        self.ui.prompt_label.clear();
        self.ui.prompt_context = None;
        self.ui.mode = AppMode::Normal;
        self.mark_dirty();
        self.mark_render_dirty();
        Some((old_name, new_name))
    }

    /// Cancel the project rename prompt, discarding changes.
    pub fn cancel_project_rename(&mut self) {
        self.ui.input_text.clear();
        self.ui.input_cursor = 0;
        self.ui.prompt_label.clear();
        self.ui.prompt_context = None;
        self.ui.mode = AppMode::Normal;
    }

    // ─── Notifications ───────────────────────────────────────────────────

    /// Display a notification toast with the given message, variant, and duration.
    /// Adds to the notification queue; if the queue is full, the oldest notification
    /// is removed.
    pub fn set_notification(
        &mut self,
        message: String,
        variant: NotificationVariant,
        duration_ms: i64,
    ) {
        let expires_at = chrono::Utc::now().timestamp_millis() + duration_ms;
        let notification = Notification {
            message,
            variant,
            expires_at,
        };
        if self.ui.notifications.len() >= MAX_NOTIFICATIONS {
            self.ui.notifications.pop_front();
        }
        self.ui.notifications.push_back(notification);
    }

    /// Clear expired notifications from the front of the queue. Returns `true`
    /// if any notification was removed.
    pub fn clear_expired_notifications(&mut self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let before = self.ui.notifications.len();
        // Remove all expired notifications from the front.
        // Since notifications are added chronologically, expired ones are always
        // at the front of the deque.
        while let Some(ref n) = self.ui.notifications.front() {
            if n.expires_at <= now {
                self.ui.notifications.pop_front();
            } else {
                break;
            }
        }
        self.ui.notifications.len() != before
    }
}
