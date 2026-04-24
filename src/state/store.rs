//! App state store — mutation methods on AppState.

use crate::state::types::*;
use std::collections::HashMap;

/// Maximum byte size for `TaskDetailSession::streaming_text`.
/// When a session's streaming buffer exceeds this cap, old text is
/// truncated from the beginning to keep the most recent content.
/// Default: 1 MiB (1,048,576 bytes).
pub const STREAMING_TEXT_CAP_BYTES: usize = 1_048_576;

impl AppState {
    // ─── Project Methods ─────────────────────────────────────────────────

    /// Register a new project. Marks state dirty.
    pub fn add_project(&mut self, project: CortexProject) {
        self.projects.push(project);
        self.mark_dirty();
    }

    /// Remove a project and all its tasks. If this was the active project,
    /// falls back to the first remaining project. Marks state dirty.
    pub fn remove_project(&mut self, project_id: &str) {
        self.projects.retain(|p| p.id != project_id);

        // Track this project for deletion from the database
        self.deleted_projects.insert(project_id.to_string());

        // Collect task IDs for this project before removing them
        let project_task_ids: Vec<String> = self
            .tasks
            .values()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.id.clone())
            .collect();

        // Track tasks for deletion from the database
        for task_id in &project_task_ids {
            self.deleted_tasks.insert(task_id.clone());
        }

        // Remove tasks and clean up associated data
        for task_id in &project_task_ids {
            // Remove session mapping
            if let Some(task) = self.tasks.get(task_id) {
                if let Some(ref sid) = task.session_id {
                    self.session_to_task.remove(sid);
                }
            }
            // Remove session data and streaming cache
            self.task_sessions.remove(task_id);
            self.cached_streaming_lines.remove(task_id);
            self.dirty_tasks.remove(task_id);
            // Clean up subagent data for each task in this project
            if let Some(sessions) = self.subagent_sessions.remove(task_id) {
                for sub in &sessions {
                    self.subagent_to_parent.remove(&sub.session_id);
                    self.subagent_session_data.remove(&sub.session_id);
                }
            }
        }

        // Remove tasks
        self.tasks.retain(|_, t| t.project_id != project_id);

        // Remove tasks from kanban columns
        for tasks in self.kanban.columns.values_mut() {
            tasks.retain(|id| !project_task_ids.contains(id));
        }

        // Clear focused task if it was removed
        if let Some(ref focused_id) = self.ui.focused_task_id {
            if project_task_ids.contains(focused_id) {
                self.ui.focused_task_id = None;
            }
        }

        if self.active_project_id.as_deref() == Some(project_id) {
            self.active_project_id = self.projects.first().map(|p| p.id.clone());
        }
        self.mark_dirty();
    }

    /// Set the active project and rebuild the kanban board for it.
    pub fn select_project(&mut self, project_id: &str) {
        self.active_project_id = Some(project_id.to_string());
        // Rebuild kanban for selected project
        self.rebuild_kanban_for_project(project_id);
    }

    /// Open the project rename prompt, pre-populating the input with the
    /// current project name. No-op if no project is active.
    pub fn open_project_rename(&mut self) {
        let current_name = match self.active_project_id.as_ref() {
            Some(pid) => self
                .projects
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

        let project_id = self.active_project_id.clone()?;
        let old_name = self
            .projects
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

    // ─── Task Methods ────────────────────────────────────────────────────

    /// Create a new task in the "todo" column. Title is auto-derived from the
    /// first line of description. Returns the created task.
    /// Increments the project's task number counter. Marks state dirty.
    pub fn create_todo(
        &mut self,
        title: String,
        description: String,
        project_id: &str,
    ) -> CortexTask {
        let id = uuid::Uuid::new_v4().to_string();
        let number = self
            .task_number_counters
            .entry(project_id.to_string())
            .or_insert(0);
        *number += 1;
        let number = *number;

        let now = chrono::Utc::now().timestamp();
        let task = CortexTask {
            id: id.clone(),
            number,
            title,
            description,
            column: KanbanColumn(KanbanColumn::TODO.to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: now,
            last_activity_at: now,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: now,
            updated_at: now,
            project_id: project_id.to_string(),
        };

        // Add to kanban
        self.tasks.insert(id.clone(), task.clone());
        self.kanban
            .columns
            .entry(KanbanColumn::TODO.to_string())
            .or_default()
            .push(id.clone());
        self.mark_task_dirty(&id);
        task
    }

    /// Move a task to a different column. Updates `entered_column_at` and
    /// `last_activity_at` timestamps. Returns `false` if the task doesn't exist.
    /// Marks state dirty.
    pub fn move_task(&mut self, task_id: &str, to_column: KanbanColumn) -> bool {
        let task = match self.tasks.get_mut(task_id) {
            Some(t) => t,
            None => return false,
        };

        let from_column = task.column.clone();
        task.column = to_column.clone();
        task.entered_column_at = chrono::Utc::now().timestamp();
        task.last_activity_at = chrono::Utc::now().timestamp();

        // Remove from old column in kanban
        if let Some(tasks) = self.kanban.columns.get_mut(&from_column.0) {
            tasks.retain(|id| id != task_id);
        }

        // Add to new column
        self.kanban
            .columns
            .entry(to_column.0.clone())
            .or_default()
            .push(task_id.to_string());

        // Clamp focused_task_index for source column (may be stale after removal)
        self.clamp_focused_task_index(&from_column.0);

        self.mark_task_dirty(task_id);
        true
    }

    /// Delete a task by ID. Removes it from the kanban board and session index.
    /// Returns `Some(session_id)` if the deleted task had an active session
    /// (caller should abort it asynchronously), `None` if no session or task not found.
    /// Marks state dirty.
    pub fn delete_task(&mut self, task_id: &str) -> Option<String> {
        if let Some(task) = self.tasks.remove(task_id) {
            // Remove from kanban
            if let Some(tasks) = self.kanban.columns.get_mut(&task.column.0) {
                tasks.retain(|id| id != task_id);
            }
            // Remove session mapping
            let session_id = task.session_id.clone();
            if let Some(ref sid) = session_id {
                self.session_to_task.remove(sid);
            }
            // Remove render cache for deleted task
            self.cached_streaming_lines.remove(task_id);
            // Remove session data for deleted task
            self.task_sessions.remove(task_id);
            // Remove from dirty set (task no longer exists)
            self.dirty_tasks.remove(task_id);
            // Track deletion for persistence — save_state will DELETE from DB
            self.deleted_tasks.insert(task_id.to_string());
            // Clean up subagent data for this task
            if let Some(sessions) = self.subagent_sessions.remove(task_id) {
                for sub in &sessions {
                    self.subagent_to_parent.remove(&sub.session_id);
                    self.subagent_session_data.remove(&sub.session_id);
                }
            }
            // Also clean up subagent session data keyed by this task's own session_id
            // (if this task was a subagent of another)
            if let Some(ref sid) = session_id {
                self.subagent_session_data.remove(sid);
            }
            self.mark_dirty();
            session_id
        } else {
            None
        }
    }

    /// Reorder a task within its column by swapping it with the task above.
    /// Returns `true` if the swap was performed, `false` if already at top
    /// or task not found. Marks state dirty on success.
    pub fn reorder_task_up(&mut self, task_id: &str) -> bool {
        let (col_id, idx) = match self.find_task_position(task_id) {
            Some(pos) => pos,
            None => return false,
        };
        if idx == 0 {
            return false;
        }
        if let Some(tasks) = self.kanban.columns.get_mut(&col_id) {
            tasks.swap(idx, idx - 1);
        }
        if let Some(focused_idx) = self.kanban.focused_task_index.get_mut(&col_id) {
            *focused_idx -= 1;
        }
        self.mark_task_dirty(task_id);
        true
    }

    /// Reorder a task within its column by swapping it with the task below.
    /// Returns `true` if the swap was performed, `false` if already at bottom
    /// or task not found. Marks state dirty on success.
    pub fn reorder_task_down(&mut self, task_id: &str) -> bool {
        let (col_id, idx) = match self.find_task_position(task_id) {
            Some(pos) => pos,
            None => return false,
        };
        let count = self
            .kanban
            .columns
            .get(&col_id)
            .map(|t| t.len())
            .unwrap_or(0);
        if idx + 1 >= count {
            return false;
        }
        if let Some(tasks) = self.kanban.columns.get_mut(&col_id) {
            tasks.swap(idx, idx + 1);
        }
        if let Some(focused_idx) = self.kanban.focused_task_index.get_mut(&col_id) {
            *focused_idx += 1;
        }
        self.mark_task_dirty(task_id);
        true
    }

    /// Find a task's column ID and position index within that column.
    fn find_task_position(&self, task_id: &str) -> Option<(String, usize)> {
        for (col_id, tasks) in &self.kanban.columns {
            if let Some(idx) = tasks.iter().position(|id| id == task_id) {
                return Some((col_id.clone(), idx));
            }
        }
        None
    }

    /// Update a task's agent status and bump `last_activity_at`. Marks state dirty.
    pub fn update_task_agent_status(&mut self, task_id: &str, status: AgentStatus) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.agent_status = status;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_task_dirty(task_id);
        }
    }

    /// Set or clear a task's OpenCode session ID. Maintains the
    /// `session_to_task` reverse index. Marks state dirty.
    pub fn set_task_session_id(&mut self, task_id: &str, session_id: Option<String>) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            // Remove old mapping
            if let Some(ref old_sid) = task.session_id {
                self.session_to_task.remove(old_sid);
            }
            // Set new mapping
            task.session_id = session_id.clone();
            if let Some(ref sid) = session_id {
                self.session_to_task
                    .insert(sid.clone(), task_id.to_string());
            }
            self.mark_task_dirty(task_id);
        }

        // Clear stale streaming state when a new session starts on an existing task.
        // Without this, streaming_text from the previous agent run persists into
        // the new session's output, causing text duplication (Bug 1).
        if session_id.is_some() {
            if let Some(session) = self.task_sessions.get_mut(task_id) {
                session.streaming_text = None;
                session.messages.clear();
                session.render_version += 1;
                self.cached_streaming_lines.remove(task_id);
            }
        }
    }

    /// Record an error on a task and set its agent status to [`AgentStatus::Error`].
    /// Marks state dirty.
    pub fn set_task_error(&mut self, task_id: &str, error: String) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error_message = Some(error);
            task.agent_status = AgentStatus::Error;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_task_dirty(task_id);
        }
    }

    /// Set the agent type on a task (e.g., when an agent is started from
    /// column config). This is informational — it's displayed in the
    /// task detail view and persisted to the database. Marks state dirty.
    pub fn set_task_agent_type(&mut self, task_id: &str, agent_type: Option<String>) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.agent_type = agent_type;
            self.mark_task_dirty(task_id);
        }
    }

    /// Look up the task ID associated with a given OpenCode session ID.
    pub fn get_task_id_by_session(&self, session_id: &str) -> Option<&str> {
        self.session_to_task.get(session_id).map(|s| s.as_str())
    }

    // ─── Navigation ──────────────────────────────────────────────────────

    /// Clamp the focused task index for a column so it stays within bounds.
    /// Syncs `ui.focused_task_id` with the clamped index.
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
            self.ui.focused_task_id = tasks.get(clamped).cloned();
        } else {
            self.ui.focused_task_id = None;
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
    pub fn open_task_detail(&mut self, task_id: &str) {
        self.ui.viewing_task_id = Some(task_id.to_string());
        self.ui.focused_panel = FocusedPanel::TaskDetail;
        self.ui.user_scroll_offset = None;
    }

    /// Close the task detail panel and return focus to the kanban board.
    /// Clears the drill-down navigation stack.
    pub fn close_task_detail(&mut self) {
        self.ui.viewing_task_id = None;
        self.ui.focused_panel = FocusedPanel::Kanban;
        self.ui.user_scroll_offset = None;
        self.ui.session_nav_stack.clear();
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
        available_columns: Vec<String>,
    ) {
        self.ui.task_editor = Some(TaskEditorState::new_for_create(
            default_column,
            available_columns,
        ));
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
                    .active_project_id
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
        let current_dir = match self.active_project_id.as_ref() {
            Some(pid) => self
                .projects
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

        let project_id = match self.active_project_id.clone() {
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

        if let Some(project) = self.projects.iter_mut().find(|p| p.id == project_id) {
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

    // ─── Session Data ────────────────────────────────────────────────────

    /// Register a subagent session detected from a `TaskMessagePart::Agent`.
    ///
    /// When a parent agent spawns a subagent, the parent's message stream
    /// includes `Agent { id, agent }` parts. This method records the
    /// parent→child relationship so the UI can offer drill-down navigation.
    ///
    /// If the subagent session is already registered for this parent task,
    /// this is a no-op (idempotent).
    pub fn register_subagent_session(
        &mut self,
        parent_task_id: &str,
        session_id: &str,
        agent_name: &str,
    ) {
        // Skip if already registered
        if self.subagent_to_parent.contains_key(session_id) {
            return;
        }

        let parent_session_id = self
            .tasks
            .get(parent_task_id)
            .and_then(|t| t.session_id.clone())
            .unwrap_or_default();

        // Calculate depth based on parent chain
        let depth = if parent_session_id.is_empty() {
            1
        } else {
            // If the parent session is itself a subagent, find its depth
            self.subagent_to_parent
                .get(&parent_session_id)
                .and_then(|ptid| self.subagent_sessions.get(ptid))
                .and_then(|sessions| sessions.iter().find(|s| s.session_id == parent_session_id).map(|s| s.depth))
                .map(|d| d + 1)
                .unwrap_or(1)
        };

        let subagent = SubagentSession {
            session_id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            parent_task_id: parent_task_id.to_string(),
            parent_session_id,
            depth,
            active: true,
        };

        // Store under parent task
        self.subagent_sessions
            .entry(parent_task_id.to_string())
            .or_default()
            .push(subagent.clone());

        // Reverse index: child session → parent task
        self.subagent_to_parent
            .insert(session_id.to_string(), parent_task_id.to_string());

        tracing::debug!(
            "Registered subagent session {} (agent: {}) under task {} (depth: {})",
            session_id,
            agent_name,
            parent_task_id,
            depth,
        );
    }

    /// Mark a subagent session as inactive (completed or errored).
    pub fn mark_subagent_inactive(&mut self, session_id: &str) {
        if let Some(parent_task_id) = self.subagent_to_parent.get(session_id).cloned() {
            if let Some(sessions) = self.subagent_sessions.get_mut(&parent_task_id) {
                for sub in sessions.iter_mut() {
                    if sub.session_id == session_id {
                        sub.active = false;
                        break;
                    }
                }
            }
        }
    }

    /// Get the parent task ID for a subagent session.
    pub fn get_parent_task_for_subagent(&self, session_id: &str) -> Option<&str> {
        self.subagent_to_parent.get(session_id).map(|s| s.as_str())
    }

    /// Get all subagent sessions for a parent task.
    pub fn get_subagent_sessions(&self, parent_task_id: &str) -> &[SubagentSession] {
        self.subagent_sessions
            .get(parent_task_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Replace the message history for a task's session.
    /// Also scans for `TaskMessagePart::Agent` entries and registers
    /// subagent sessions so they can be navigated via drill-down.
    pub fn update_session_messages(&mut self, task_id: &str, messages: Vec<TaskMessage>) {
        // Register any subagent sessions found in the message parts
        for msg in &messages {
            for part in &msg.parts {
                if let TaskMessagePart::Agent { id, agent } = part {
                    self.register_subagent_session(task_id, id, agent);
                }
            }
        }

        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.messages = messages;
        session.render_version += 1;
    }

    /// Set or clear the streaming text buffer for a task's session.
    pub fn update_streaming_text(&mut self, task_id: &str, text: Option<String>) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.streaming_text = text;
        session.render_version += 1;
    }

    /// Add a pending permission request to a task's session.
    /// Updates the task's `pending_permission_count`.
    pub fn add_permission_request(&mut self, task_id: &str, request: PermissionRequest) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.pending_permissions.push(request);
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.pending_permission_count = session.pending_permissions.len() as u32;
        }
    }

    /// Resolve (dismiss) a permission request from a task's session.
    /// Updates the task's `pending_permission_count`.
    pub fn resolve_permission_request(
        &mut self,
        task_id: &str,
        permission_id: &str,
        _approved: bool,
    ) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session
            .pending_permissions
            .retain(|p| p.id != permission_id);
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.pending_permission_count = session.pending_permissions.len() as u32;
        }
    }

    /// Add a question request to a task's session.
    /// Updates the task's `pending_question_count`.
    pub fn add_question_request(&mut self, task_id: &str, request: QuestionRequest) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.pending_questions.push(request);
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.pending_question_count = session.pending_questions.len() as u32;
        }
    }

    /// Resolve (dismiss) a question request from a task's session.
    /// Updates the task's `pending_question_count`.
    pub fn resolve_question_request(&mut self, task_id: &str, question_id: &str) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.pending_questions.retain(|q| q.id != question_id);
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.pending_question_count = session.pending_questions.len() as u32;
        }
    }

    // ─── SSE Processing Helpers ──────────────────────────────────────────

    /// Truncate streaming text from the beginning to enforce the cap.
    /// Keeps the most recent content (tail of the buffer).
    /// Handles UTF-8 boundary safety.
    fn enforce_streaming_cap(text: &mut String) {
        if text.len() <= STREAMING_TEXT_CAP_BYTES {
            return;
        }
        let excess = text.len() - STREAMING_TEXT_CAP_BYTES;
        let mut split_at = excess;
        while split_at < text.len() && !text.is_char_boundary(split_at) {
            split_at += 1;
        }
        if split_at < text.len() {
            let _ = text.drain(..split_at);
        }
    }

    /// Handle a `SessionStatus` SSE event — map the status string to
    /// [`AgentStatus`] and update the corresponding task.
    /// Also marks subagent sessions as inactive when they complete.
    pub fn process_session_status(&mut self, session_id: &str, status: &str) {
        // Check if this is a subagent session completing
        if matches!(status, "complete" | "completed" | "error") {
            self.mark_subagent_inactive(session_id);
        }

        // Route to parent task if this is a subagent session
        if let Some(parent_task_id) = self.get_parent_task_for_subagent(session_id).map(|s| s.to_string()) {
            self.process_subagent_status(session_id, &parent_task_id, status);
            return;
        }

        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            let agent_status = match status {
                "running" => AgentStatus::Running,
                "complete" | "completed" => AgentStatus::Complete,
                _ => return,
            };
            self.update_task_agent_status(&task_id, agent_status);
        }
    }

    /// Handle a `SessionIdle` SSE event — mark the task as complete
    /// and show a success notification.
    /// Also marks any subagent session as inactive.
    /// Returns the task ID if a task was found and marked complete, `None` otherwise.
    pub fn process_session_idle(&mut self, session_id: &str) -> Option<String> {
        // Mark subagent as inactive if this is a child session
        self.mark_subagent_inactive(session_id);

        // Route to parent task if this is a subagent session
        if let Some(parent) = self.get_parent_task_for_subagent(session_id) {
            let parent_task_id = parent.to_string();
            self.process_subagent_idle(session_id, &parent_task_id);
            return None; // Don't trigger auto-progression for subagent sessions
        }

        self.get_task_id_by_session(session_id)
            .map(|s| s.to_string())
            .map(|task_id| {
                self.update_task_agent_status(&task_id, AgentStatus::Complete);
                self.set_notification(
                    format!("Task agent completed"),
                    NotificationVariant::Success,
                    5000,
                );
                task_id
            })
    }

    /// Handle a `SessionError` SSE event — record the error on the task.
    pub fn process_session_error(&mut self, session_id: &str, error: &str) {
        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            self.set_task_error(&task_id, error.to_string());
        }
    }

    /// Handle a `MessagePartDelta` SSE event — append text to the
    /// streaming buffer for the corresponding task's session.
    /// When the buffer exceeds `STREAMING_TEXT_CAP_BYTES`, old text
    /// is truncated from the beginning (keeping the most recent content).
    ///
    /// Deduplicates events by tracking `(message_id, part_id)` pairs:
    /// if the same pair is seen again (e.g., after SSE reconnection),
    /// the delta is silently skipped to prevent text duplication.
    ///
    /// If the session_id belongs to a subagent session, the delta is
    /// routed to the subagent's session data in `subagent_session_data`.
    pub fn process_message_part_delta(
        &mut self,
        session_id: &str,
        message_id: &str,
        part_id: &str,
        field: &str,
        delta: &str,
    ) {
        // Route to subagent session data if this is a child session
        if let Some(parent) = self.get_parent_task_for_subagent(session_id) {
            let parent_task_id = parent.to_string();
            self.process_subagent_message_delta(
                session_id,
                &parent_task_id,
                message_id,
                part_id,
                field,
                delta,
            );
            return;
        }

        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            let session = self
                .task_sessions
                .entry(task_id.clone())
                .or_insert_with(|| TaskDetailSession {
                    task_id,
                    ..Default::default()
                });

            // Deduplication: skip if this (message_id, part_id) was already
            // seen for a *previous* part. Consecutive deltas for the same
            // part share the same key and are always accepted (continuation).
            // A different key that's already in the set indicates a replay
            // from SSE reconnection — skip it to prevent text duplication.
            let delta_key = (message_id.to_string(), part_id.to_string());
            let is_continuation = session.last_delta_key.as_ref() == Some(&delta_key);
            if !is_continuation && session.seen_delta_keys.contains(&delta_key) {
                // Key was seen before but is NOT the current part — replay.
                return;
            }
            if !is_continuation {
                // New part we haven't seen — record it.
                session.seen_delta_keys.insert(delta_key.clone());
            }
            session.last_delta_key = Some(delta_key);

            // Only append to streaming_text for "text" field deltas.
            // Other field types (e.g., "reasoning") should be handled
            // separately — they must not pollute the text buffer.
            if field != "text" {
                return;
            }

            match &mut session.streaming_text {
                Some(text) => {
                    text.push_str(delta);
                }
                None => {
                    session.streaming_text = Some(delta.to_string());
                }
            }
            // Enforce cap: truncate from the beginning if over limit.
            // Keep the most recent content (tail of the buffer).
            if let Some(ref mut text) = session.streaming_text {
                Self::enforce_streaming_cap(text);
            }
            session.render_version += 1;
        }
    }

    /// Handle a status event for a subagent session.
    /// Updates the subagent's session data in `subagent_session_data`.
    fn process_subagent_status(&mut self, session_id: &str, _parent_task_id: &str, status: &str) {
        // Ensure subagent session data exists
        let entry = self
            .subagent_session_data
            .entry(session_id.to_string())
            .or_insert_with(TaskDetailSession::default);
        entry.session_id = Some(session_id.to_string());

        match status {
            "complete" | "completed" => {
                tracing::debug!("Subagent session {} completed", session_id);
                // Clear dedup tracking when subagent completes.
                entry.seen_delta_keys.clear();
            }
            "error" => {
                tracing::debug!("Subagent session {} errored", session_id);
                entry.seen_delta_keys.clear();
            }
            _ => {}
        }

        self.mark_render_dirty();
    }

    /// Handle an idle event for a subagent session.
    fn process_subagent_idle(&mut self, session_id: &str, _parent_task_id: &str) {
        tracing::debug!("Subagent session {} went idle", session_id);
        self.mark_subagent_inactive(session_id);
        // Clear dedup tracking for the subagent session.
        if let Some(entry) = self.subagent_session_data.get_mut(session_id) {
            entry.seen_delta_keys.clear();
        }
        self.mark_render_dirty();
    }

    /// Handle a message delta for a subagent session.
    /// Appends to the subagent's streaming text buffer in `subagent_session_data`.
    /// Deduplicates using `(message_id, part_id)` to prevent replay doubling.
    fn process_subagent_message_delta(
        &mut self,
        session_id: &str,
        _parent_task_id: &str,
        message_id: &str,
        part_id: &str,
        field: &str,
        delta: &str,
    ) {
        let entry = self
            .subagent_session_data
            .entry(session_id.to_string())
            .or_insert_with(TaskDetailSession::default);
        entry.session_id = Some(session_id.to_string());

        // Deduplication: skip if this (message_id, part_id) was already
        // seen for a *previous* part. Consecutive deltas for the same
        // part share the same key and are always accepted (continuation).
        let delta_key = (message_id.to_string(), part_id.to_string());
        let is_continuation = entry.last_delta_key.as_ref() == Some(&delta_key);
        if !is_continuation && entry.seen_delta_keys.contains(&delta_key) {
            return;
        }
        if !is_continuation {
            entry.seen_delta_keys.insert(delta_key.clone());
        }
        entry.last_delta_key = Some(delta_key);

        // Only append to streaming_text for "text" field deltas.
        // Other field types (e.g., "reasoning") are ignored for the
        // streaming buffer.
        if field != "text" {
            return;
        }

        match &mut entry.streaming_text {
            Some(text) => {
                text.push_str(delta);
            }
            None => {
                entry.streaming_text = Some(delta.to_string());
            }
        }

        // Enforce cap
        if let Some(ref mut text) = entry.streaming_text {
            Self::enforce_streaming_cap(text);
        }

        entry.render_version += 1;
    }

    /// Handle a `PermissionAsked` SSE event — create a pending permission
    /// request for the task.
    pub fn process_permission_asked(
        &mut self,
        session_id: &str,
        perm_id: &str,
        tool: &str,
        desc: &str,
    ) {
        // Route to parent task if this is a subagent session
        let (task_id, effective_session_id) = if let Some(parent_task_id) =
            self.get_parent_task_for_subagent(session_id).map(|s| s.to_string())
        {
            (parent_task_id, session_id.to_string())
        } else if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            (task_id, session_id.to_string())
        } else {
            return;
        };

        let request = PermissionRequest {
            id: perm_id.to_string(),
            session_id: effective_session_id,
            tool_name: tool.to_string(),
            description: desc.to_string(),
            status: "pending".to_string(),
            details: None,
        };
        self.add_permission_request(&task_id, request);
    }

    // ─── Dirty Flag ──────────────────────────────────────────────────────

    /// Set the persistence dirty flag.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Mark a specific task as needing to be persisted on the next save.
    /// Also sets the global dirty flag so the persistence loop knows to run.
    pub fn mark_task_dirty(&mut self, task_id: &str) {
        self.dirty_tasks.insert(task_id.to_string());
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically take (clear) the persistence dirty flag.
    /// Returns `true` if the flag was set, `false` otherwise.
    pub fn take_dirty(&self) -> bool {
        self.dirty
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Mark that the state has changed and a re-render is needed.
    pub fn mark_render_dirty(&self) {
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically take the render-dirty flag (returns `true` and resets to
    /// `false` if the flag was set; returns `false` otherwise).
    pub fn take_render_dirty(&self) -> bool {
        self.render_dirty
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Evict stale entries from the streaming render cache.
    ///
    /// Removes cached lines for task IDs that no longer exist in `self.tasks`,
    /// and if the cache still exceeds `max_entries`, clears the oldest half
    /// (by insertion order, which roughly correlates with least-recently-viewed).
    pub fn prune_streaming_cache(&mut self, max_entries: usize) {
        // Remove entries for deleted tasks
        self.cached_streaming_lines
            .retain(|task_id, _| self.tasks.contains_key(task_id));

        // Also evict subagent session data for sessions whose parent task no longer exists
        self.subagent_session_data.retain(|session_id, _| {
            self.subagent_to_parent.contains_key(session_id)
        });

        // If still too large, remove the oldest half (first N/2 entries)
        if self.cached_streaming_lines.len() > max_entries {
            let to_remove = self.cached_streaming_lines.len() / 2;
            let keys: Vec<String> = self
                .cached_streaming_lines
                .keys()
                .take(to_remove)
                .cloned()
                .collect();
            for key in keys {
                self.cached_streaming_lines.remove(&key);
            }
        }
    }

    // ─── Persistence Restore ─────────────────────────────────────────────

    /// Bulk-restore state from persistence (projects, tasks, kanban order,
    /// active project, and task number counters).
    pub fn restore_state(
        &mut self,
        projects: Vec<CortexProject>,
        tasks: Vec<CortexTask>,
        kanban_columns: HashMap<String, Vec<String>>,
        active_project_id: Option<String>,
        counters: HashMap<String, u32>,
    ) {
        self.projects = projects;
        self.tasks.clear();
        for task in tasks {
            let id = task.id.clone();
            if let Some(ref sid) = task.session_id {
                self.session_to_task.insert(sid.clone(), id.clone());
            }
            self.tasks.insert(id, task);
        }
        self.kanban.columns = kanban_columns;
        self.active_project_id = active_project_id;
        self.task_number_counters = counters;

        let pid = self.active_project_id.clone();
        if let Some(pid) = pid {
            self.rebuild_kanban_for_project(&pid);
        }
    }

    // ─── Internal Helpers ────────────────────────────────────────────────

    fn rebuild_kanban_for_project(&mut self, project_id: &str) {
        let mut columns: HashMap<String, Vec<String>> = HashMap::new();
        for (id, task) in &self.tasks {
            if task.project_id == project_id {
                columns
                    .entry(task.column.0.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        self.kanban.columns = columns;
        // Reset focused column to first visible
        self.kanban.focused_column_index = 0;
        self.kanban.focused_task_index.clear();

        // Initialize focused_task_id to first task in first column
        let first_col = self.kanban.columns.keys().next().cloned();
        if let Some(ref col) = first_col {
            self.ui.focused_column = col.clone();
            self.kanban.focused_task_index.insert(col.clone(), 0);
            self.ui.focused_task_id = self
                .kanban
                .columns
                .get(col)
                .and_then(|ids| ids.first().cloned());
        }
    }

    /// Get tasks for the active project in a given column.
    pub fn get_tasks_in_column(&self, column_id: &str) -> Vec<&CortexTask> {
        self.kanban
            .columns
            .get(column_id)
            .map(|ids| ids.iter().filter_map(|id| self.tasks.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get the focused task for the current column.
    pub fn get_focused_task(&self) -> Option<&CortexTask> {
        let task_id = self.ui.focused_task_id.as_ref()?;
        self.tasks.get(task_id)
    }

    /// Update project status based on aggregate task states.
    pub fn update_project_status(&mut self, project_id: &str) {
        let has_running = self
            .tasks
            .values()
            .any(|t| t.project_id == project_id && t.agent_status == AgentStatus::Running);
        let has_error = self
            .tasks
            .values()
            .any(|t| t.project_id == project_id && t.agent_status == AgentStatus::Error);
        let has_question = self
            .tasks
            .values()
            .any(|t| t.project_id == project_id && t.pending_question_count > 0);

        let status = if has_error {
            ProjectStatus::Error
        } else if has_question {
            ProjectStatus::Question
        } else if has_running {
            ProjectStatus::Working
        } else {
            ProjectStatus::Idle
        };

        for project in &mut self.projects {
            if project.id == project_id {
                project.status = status;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::types::*;

    /// Helper to create a minimal AppState with a default project and some tasks.
    fn make_state_with_tasks() -> AppState {
        let mut state = AppState::default();

        // Add a project
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        state.add_project(project);
        state.active_project_id = Some("proj-1".to_string());

        // Add tasks to columns
        for i in 0..3 {
            let task = CortexTask {
                id: format!("task-{}", i),
                number: (i + 1) as u32,
                title: format!("Task {}", i),
                description: String::new(),
                column: KanbanColumn("todo".to_string()),
                session_id: None,
                agent_type: None,
                agent_status: AgentStatus::Pending,
                entered_column_at: 1000,
                last_activity_at: 1000,
                error_message: None,
                plan_output: None,
                pending_permission_count: 0,
                pending_question_count: 0,
                created_at: 1000,
                updated_at: 1000,
                project_id: "proj-1".to_string(),
            };
            state.tasks.insert(task.id.clone(), task.clone());
            state
                .kanban
                .columns
                .entry("todo".to_string())
                .or_default()
                .push(task.id);
        }

        // Set initial focus
        state.ui.focused_column = "todo".to_string();
        state.kanban.focused_column_index = 0;
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 0);
        state.ui.focused_task_id = Some("task-0".to_string());

        state
    }

    // ── Detail mode (Escape from detail view) ───────────────────────────

    #[test]
    fn open_task_detail_sets_panel_and_viewing_id() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.focused_panel, FocusedPanel::Kanban);
        assert_eq!(state.ui.viewing_task_id, None);

        state.open_task_detail("task-1");
        assert_eq!(state.ui.focused_panel, FocusedPanel::TaskDetail);
        assert_eq!(state.ui.viewing_task_id, Some("task-1".to_string()));
    }

    #[test]
    fn close_task_detail_resets_to_kanban() {
        let mut state = make_state_with_tasks();
        state.open_task_detail("task-1");
        assert_eq!(state.ui.focused_panel, FocusedPanel::TaskDetail);

        state.close_task_detail();
        assert_eq!(state.ui.focused_panel, FocusedPanel::Kanban);
        assert_eq!(state.ui.viewing_task_id, None);
    }

    #[test]
    fn open_nonexistent_task_detail_still_sets_panel() {
        let mut state = make_state_with_tasks();
        // open_task_detail doesn't check if the task exists
        state.open_task_detail("nonexistent");
        assert_eq!(state.ui.focused_panel, FocusedPanel::TaskDetail);
        assert_eq!(state.ui.viewing_task_id, Some("nonexistent".to_string()));
    }

    // ── Navigation: column focusing ─────────────────────────────────────

    #[test]
    fn set_focused_column_updates_column_and_syncs_task_id() {
        let mut state = make_state_with_tasks();
        // Add tasks to another column
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default()
            .push("task-1".to_string());

        state.set_focused_column("planning");
        assert_eq!(state.ui.focused_column, "planning");
        assert_eq!(state.ui.focused_task_id, Some("task-1".to_string()));
    }

    #[test]
    fn set_focused_column_empty_column_keeps_existing_task_id() {
        let mut state = make_state_with_tasks();
        // Focusing a column that doesn't exist in kanban.columns
        // does NOT clear focused_task_id (the column must exist in kanban)
        state.set_focused_column("empty-column");
        assert_eq!(state.ui.focused_column, "empty-column");
        // focused_task_id is NOT cleared because the column isn't in kanban.columns
        assert_eq!(state.ui.focused_task_id, Some("task-0".to_string()));
    }

    #[test]
    fn clamp_focused_task_index_clamps_to_column_length() {
        let mut state = make_state_with_tasks();
        // "todo" has 3 tasks (indices 0-2)
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 99);

        state.clamp_focused_task_index("todo");
        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&2));
        assert_eq!(state.ui.focused_task_id, Some("task-2".to_string()));
    }

    #[test]
    fn clamp_focused_task_index_empty_column_resets_to_zero() {
        let mut state = make_state_with_tasks();
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 5);
        // Remove all tasks from todo
        state.kanban.columns.get_mut("todo").unwrap().clear();

        state.clamp_focused_task_index("todo");
        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&0));
        assert_eq!(state.ui.focused_task_id, None);
    }

    // ── Navigation: task index movement (simulates NavUp/NavDown) ───────

    #[test]
    fn nav_up_decreases_task_index() {
        let mut state = make_state_with_tasks();
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 2);

        let col_id = state.ui.focused_column.clone();
        let current = state
            .kanban
            .focused_task_index
            .get(&col_id)
            .copied()
            .unwrap_or(0);
        if current > 0 {
            state
                .kanban
                .focused_task_index
                .insert(col_id.clone(), current - 1);
        }

        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&1));
    }

    #[test]
    fn nav_up_does_not_go_below_zero() {
        let mut state = make_state_with_tasks();
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 0);

        let col_id = state.ui.focused_column.clone();
        let current = state
            .kanban
            .focused_task_index
            .get(&col_id)
            .copied()
            .unwrap_or(0);
        if current > 0 {
            state
                .kanban
                .focused_task_index
                .insert(col_id.clone(), current - 1);
        }

        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&0));
    }

    #[test]
    fn nav_down_increases_task_index_within_bounds() {
        let mut state = make_state_with_tasks();
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 0);

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
        if current + 1 < task_count {
            state
                .kanban
                .focused_task_index
                .insert(col_id.clone(), current + 1);
        }

        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&1));
    }

    #[test]
    fn nav_down_does_not_exceed_column_length() {
        let mut state = make_state_with_tasks();
        // 3 tasks, index at last (2)
        state
            .kanban
            .focused_task_index
            .insert("todo".to_string(), 2);

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
        if current + 1 < task_count {
            state
                .kanban
                .focused_task_index
                .insert(col_id.clone(), current + 1);
        }

        // Should remain at 2
        assert_eq!(state.kanban.focused_task_index.get("todo"), Some(&2));
    }

    // ── Task editor: open/create ────────────────────────────────────────

    #[test]
    fn open_task_editor_create_sets_mode_and_editor() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.task_editor.is_none());

        state.open_task_editor_create("todo", vec!["todo".to_string()]);
        assert_eq!(state.ui.mode, AppMode::TaskEditor);
        assert!(state.ui.task_editor.is_some());

        let editor = state.get_task_editor().unwrap();
        assert_eq!(editor.task_id, None); // creating new
        assert_eq!(editor.column_id, Some("todo".to_string()));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    #[test]
    fn open_task_editor_edit_populates_from_existing_task() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("task-1", vec!["todo".to_string()]);

        assert_eq!(state.ui.mode, AppMode::TaskEditor);
        let editor = state.get_task_editor().unwrap();
        assert_eq!(editor.task_id, Some("task-1".to_string()));
        assert_eq!(editor.focused_field, EditorField::Description);
    }

    #[test]
    fn open_task_editor_edit_nonexistent_does_nothing() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("nonexistent", vec!["todo".to_string()]);

        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.task_editor.is_none());
    }

    // ── Task editor: save ───────────────────────────────────────────────

    #[test]
    fn save_task_editor_create_new_task() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo", vec!["todo".to_string()]);

        // Set description via editor
        if let Some(editor) = state.get_task_editor_mut() {
            editor.set_description("Some desc");
        }

        let result = state.save_task_editor();
        assert!(result.is_ok());
        let task_id = result.unwrap();

        // Verify task was created
        assert!(state.tasks.contains_key(&task_id));
        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.title, "Some desc"); // auto-derived from first line of description
        assert_eq!(task.description, "Some desc");
    }

    #[test]
    fn save_task_editor_empty_description_fails() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo", vec!["todo".to_string()]);

        // Leave description empty
        let result = state.save_task_editor();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn save_task_editor_edit_existing() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("task-1", vec!["todo".to_string()]);

        if let Some(editor) = state.get_task_editor_mut() {
            editor.set_description("Updated Description");
        }

        let result = state.save_task_editor();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "task-1");

        let task = state.tasks.get("task-1").unwrap();
        assert_eq!(task.title, "Updated Description"); // auto-derived from description
        assert_eq!(task.description, "Updated Description");
    }

    #[test]
    fn save_task_editor_no_editor_open_fails() {
        let mut state = make_state_with_tasks();
        let result = state.save_task_editor();
        assert!(result.is_err());
    }

    // ── Task editor: cancel ─────────────────────────────────────────────

    #[test]
    fn cancel_task_editor_resets_mode() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo", vec!["todo".to_string()]);
        assert_eq!(state.ui.mode, AppMode::TaskEditor);

        state.cancel_task_editor();
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.task_editor.is_none());
    }

    #[test]
    fn cancel_task_editor_discards_changes() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo", vec!["todo".to_string()]);

        if let Some(editor) = state.get_task_editor_mut() {
            editor.set_description("Discard Me");
        }

        state.cancel_task_editor();
        // The task should NOT have been created
        assert!(!state.tasks.values().any(|t| t.description == "Discard Me"));
    }

    // ── Task operations: delete ─────────────────────────────────────────

    #[test]
    fn delete_task_removes_from_state_and_kanban() {
        let mut state = make_state_with_tasks();
        assert!(state.tasks.contains_key("task-1"));

        let deleted = state.delete_task("task-1");
        assert!(deleted.is_none()); // no session
        assert!(!state.tasks.contains_key("task-1"));
        assert!(!state
            .kanban
            .columns
            .get("todo")
            .unwrap()
            .contains(&"task-1".to_string()));
    }

    #[test]
    fn delete_nonexistent_task_returns_none() {
        let mut state = make_state_with_tasks();
        let deleted = state.delete_task("nonexistent");
        assert!(deleted.is_none());
    }

    #[test]
    fn delete_task_clears_session_mapping() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-1").unwrap().session_id = Some("session-abc".to_string());
        state
            .session_to_task
            .insert("session-abc".to_string(), "task-1".to_string());

        let deleted = state.delete_task("task-1");
        assert_eq!(deleted, Some("session-abc".to_string()));
        assert!(!state.session_to_task.contains_key("session-abc"));
    }

    // ── Task operations: move ───────────────────────────────────────────

    #[test]
    fn move_task_between_columns() {
        let mut state = make_state_with_tasks();
        // Add a planning column
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default();

        let moved = state.move_task("task-0", KanbanColumn("planning".to_string()));
        assert!(moved);

        // Task column updated
        assert_eq!(state.tasks.get("task-0").unwrap().column.0, "planning");
        // Removed from todo
        assert!(!state
            .kanban
            .columns
            .get("todo")
            .unwrap()
            .contains(&"task-0".to_string()));
        // Added to planning
        assert!(state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&"task-0".to_string()));
    }

    #[test]
    fn move_nonexistent_task_returns_false() {
        let mut state = make_state_with_tasks();
        let moved = state.move_task("nonexistent", KanbanColumn("planning".to_string()));
        assert!(!moved);
    }

    #[test]
    fn move_task_updates_entered_column_at() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().entered_column_at = 1000;

        state.move_task("task-0", KanbanColumn("todo".to_string()));
        // Should be updated to current time (much larger than 1000)
        assert!(state.tasks.get("task-0").unwrap().entered_column_at > 1000);
    }

    // ── Notifications ───────────────────────────────────────────────────

    #[test]
    fn set_notification_stores_message_and_variant() {
        let mut state = AppState::default();
        state.set_notification("Hello world".to_string(), NotificationVariant::Info, 5000);

        assert_eq!(state.ui.notifications.len(), 1);
        let notif = state.ui.notifications.back().unwrap();
        assert_eq!(notif.message, "Hello world");
        assert_eq!(notif.variant, NotificationVariant::Info);
    }

    #[test]
    fn set_notification_queues_and_shows_latest() {
        let mut state = AppState::default();
        state.set_notification("First".to_string(), NotificationVariant::Info, 5000);
        state.set_notification("Second".to_string(), NotificationVariant::Warning, 5000);

        assert_eq!(state.ui.notifications.len(), 2);
        // Most recent is at the back
        let notif = state.ui.notifications.back().unwrap();
        assert_eq!(notif.message, "Second");
        assert_eq!(notif.variant, NotificationVariant::Warning);
        // Oldest is at the front
        let oldest = state.ui.notifications.front().unwrap();
        assert_eq!(oldest.message, "First");
    }

    #[test]
    fn set_notification_evicts_oldest_when_full() {
        let mut state = AppState::default();
        // Fill the queue to MAX_NOTIFICATIONS (3)
        state.set_notification("First".to_string(), NotificationVariant::Info, 5000);
        state.set_notification("Second".to_string(), NotificationVariant::Success, 5000);
        state.set_notification("Third".to_string(), NotificationVariant::Warning, 5000);
        assert_eq!(state.ui.notifications.len(), 3);

        // Adding a 4th should evict the oldest
        state.set_notification("Fourth".to_string(), NotificationVariant::Error, 5000);
        assert_eq!(state.ui.notifications.len(), 3);
        let oldest = state.ui.notifications.front().unwrap();
        assert_eq!(oldest.message, "Second");
        let newest = state.ui.notifications.back().unwrap();
        assert_eq!(newest.message, "Fourth");
    }

    #[test]
    fn clear_expired_notifications_removes_old() {
        let mut state = AppState::default();
        // Set notification that expired 1 second ago
        let expires_at = chrono::Utc::now().timestamp_millis() - 1000;
        state.ui.notifications.push_back(Notification {
            message: "Old".to_string(),
            variant: NotificationVariant::Info,
            expires_at,
        });

        state.clear_expired_notifications();
        assert!(state.ui.notifications.is_empty());
    }

    #[test]
    fn clear_expired_notifications_keeps_fresh() {
        let mut state = AppState::default();
        // Set notification that expires far in the future
        let expires_at = chrono::Utc::now().timestamp_millis() + 60_000;
        state.ui.notifications.push_back(Notification {
            message: "Fresh".to_string(),
            variant: NotificationVariant::Success,
            expires_at,
        });

        state.clear_expired_notifications();
        assert!(!state.ui.notifications.is_empty());
        assert_eq!(state.ui.notifications.back().unwrap().message, "Fresh");
    }

    // ── Project selection ───────────────────────────────────────────────

    #[test]
    fn select_project_updates_active_id() {
        let mut state = AppState::default();
        let p1 = CortexProject {
            id: "p1".to_string(),
            name: "Project 1".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let p2 = CortexProject {
            id: "p2".to_string(),
            name: "Project 2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        };
        state.add_project(p1);
        state.add_project(p2);

        state.select_project("p2");
        assert_eq!(state.active_project_id, Some("p2".to_string()));
    }

    // ── Dirty flag ──────────────────────────────────────────────────────

    #[test]
    fn mark_dirty_sets_flag() {
        let state = AppState::default();
        assert!(!state.dirty.load(std::sync::atomic::Ordering::Relaxed));
        state.mark_dirty();
        assert!(state.dirty.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn take_dirty_clears_flag() {
        let state = AppState::default();
        state.mark_dirty();
        assert!(state.take_dirty()); // returns true, clears flag
        assert!(!state.take_dirty()); // returns false
    }

    // ── Focused task helpers ────────────────────────────────────────────

    #[test]
    fn get_focused_task_returns_correct_task() {
        let mut state = make_state_with_tasks();
        state.ui.focused_task_id = Some("task-1".to_string());
        let task = state.get_focused_task().unwrap();
        assert_eq!(task.id, "task-1");
    }

    #[test]
    fn get_focused_task_returns_none_when_unset() {
        let mut state = make_state_with_tasks();
        state.ui.focused_task_id = None;
        assert!(state.get_focused_task().is_none());
    }

    #[test]
    fn set_focused_task_updates_id() {
        let mut state = make_state_with_tasks();
        state.set_focused_task(Some("task-2".to_string()));
        assert_eq!(state.ui.focused_task_id, Some("task-2".to_string()));

        state.set_focused_task(None);
        assert_eq!(state.ui.focused_task_id, None);
    }

    // ── Task editor: get_task_editor_mut ────────────────────────────────

    #[test]
    fn get_task_editor_mut_allows_mutation() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo", vec!["todo".to_string()]);

        if let Some(editor) = state.get_task_editor_mut() {
            editor.set_description("Mutated");
            editor.cursor_col = 7;
        }

        assert_eq!(state.get_task_editor().unwrap().description(), "Mutated");
        assert_eq!(state.get_task_editor().unwrap().cursor_col, 7);
    }

    #[test]
    fn get_task_editor_mut_returns_none_when_closed() {
        let mut state = make_state_with_tasks();
        assert!(state.get_task_editor_mut().is_none());
    }

    // ── Project rename ──────────────────────────────────────────────────

    #[test]
    fn open_project_rename_sets_mode_and_prepopulates_input() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.mode, AppMode::Normal);

        state.open_project_rename();

        assert_eq!(state.ui.mode, AppMode::ProjectRename);
        assert_eq!(state.ui.input_text, "Test Project");
        assert_eq!(state.ui.input_cursor, "Test Project".chars().count());
        assert_eq!(state.ui.prompt_label, "Rename project to:");
        assert_eq!(state.ui.prompt_context, Some("rename_project".to_string()));
    }

    #[test]
    fn open_project_rename_no_active_project_shows_warning() {
        let mut state = AppState::default();
        // No active project set

        state.open_project_rename();

        assert_eq!(state.ui.mode, AppMode::Normal);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("No active project"));
        assert_eq!(notif.variant, NotificationVariant::Warning);
    }

    #[test]
    fn submit_project_rename_updates_name_and_resets_mode() {
        let mut state = make_state_with_tasks();
        state.open_project_rename();
        state.ui.input_text = "New Project Name".to_string();
        state.ui.input_cursor = "New Project Name".chars().count();

        let result = state.submit_project_rename();

        assert_eq!(
            result,
            Some(("Test Project".to_string(), "New Project Name".to_string()))
        );
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert_eq!(state.projects[0].name, "New Project Name");
        assert!(state.ui.input_text.is_empty());
        assert_eq!(state.ui.input_cursor, 0);
        assert!(state.ui.prompt_label.is_empty());
        assert!(state.ui.prompt_context.is_none());
    }

    #[test]
    fn submit_project_rename_empty_name_returns_none() {
        let mut state = make_state_with_tasks();
        state.open_project_rename();
        state.ui.input_text = "   ".to_string(); // whitespace only

        let result = state.submit_project_rename();

        assert_eq!(result, None);
        // Mode should still be ProjectRename (not reset)
        assert_eq!(state.ui.mode, AppMode::ProjectRename);
        // Project name should be unchanged
        assert_eq!(state.projects[0].name, "Test Project");
    }

    #[test]
    fn cancel_project_rename_resets_state() {
        let mut state = make_state_with_tasks();
        state.open_project_rename();
        state.ui.input_text = "Discard Me".to_string();
        assert_eq!(state.ui.mode, AppMode::ProjectRename);

        state.cancel_project_rename();

        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.input_text.is_empty());
        assert_eq!(state.ui.input_cursor, 0);
        assert!(state.ui.prompt_label.is_empty());
        assert!(state.ui.prompt_context.is_none());
        // Project name should be unchanged
        assert_eq!(state.projects[0].name, "Test Project");
    }

    // ── Working directory ───────────────────────────────────────────────

    #[test]
    fn open_set_working_directory_sets_mode_and_prepopulates_input() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.mode, AppMode::Normal);

        state.open_set_working_directory();

        assert_eq!(state.ui.mode, AppMode::InputPrompt);
        assert_eq!(state.ui.input_text, "/tmp");
        assert_eq!(state.ui.input_cursor, "/tmp".chars().count());
        assert_eq!(state.ui.prompt_label, "Set working directory:");
        assert_eq!(
            state.ui.prompt_context,
            Some("set_working_directory".to_string())
        );
    }

    #[test]
    fn open_set_working_directory_no_active_project_shows_warning() {
        let mut state = AppState::default();
        // No active project set

        state.open_set_working_directory();

        assert_eq!(state.ui.mode, AppMode::Normal);
        let notif = state.ui.notifications.back().unwrap();
        assert!(notif.message.contains("No active project"));
        assert_eq!(notif.variant, NotificationVariant::Warning);
    }

    #[test]
    fn submit_working_directory_updates_project_and_resets_mode() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        state.ui.input_text = "/tmp".to_string();

        let result = state.submit_working_directory();

        assert_eq!(result, Ok(true));
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert_eq!(state.projects[0].working_directory, "/tmp");
        assert!(state.ui.input_text.is_empty());
        assert_eq!(state.ui.input_cursor, 0);
        assert!(state.ui.prompt_label.is_empty());
        assert!(state.ui.prompt_context.is_none());
    }

    #[test]
    fn submit_working_directory_empty_returns_false() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        state.ui.input_text = "   ".to_string(); // whitespace only

        let result = state.submit_working_directory();

        assert_eq!(result, Ok(false));
        // Working directory should be unchanged
        assert_eq!(state.projects[0].working_directory, "/tmp");
    }

    #[test]
    fn submit_working_directory_no_active_project_returns_false() {
        let mut state = AppState::default();
        // Simulate being in InputPrompt mode with no active project
        state.ui.mode = AppMode::InputPrompt;
        state.ui.input_text = "/some/path".to_string();

        let result = state.submit_working_directory();

        assert_eq!(result, Ok(false));
    }

    #[test]
    fn submit_working_directory_nonexistent_path_returns_error() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        state.ui.input_text = "/nonexistent/path/that/does/not/exist".to_string();

        let result = state.submit_working_directory();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("does not exist"));
        assert!(err.contains("/nonexistent/path/that/does/not/exist"));
        // Should stay in prompt mode (not reset)
        assert_eq!(state.ui.mode, AppMode::InputPrompt);
        // Working directory should be unchanged
        assert_eq!(state.projects[0].working_directory, "/tmp");
    }

    #[test]
    fn submit_working_directory_file_instead_of_directory_returns_error() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        // /etc/hosts is a file, not a directory, on most Unix systems
        state.ui.input_text = "/etc/hosts".to_string();

        let result = state.submit_working_directory();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("not a directory"));
        assert!(err.contains("/etc/hosts"));
        // Should stay in prompt mode
        assert_eq!(state.ui.mode, AppMode::InputPrompt);
        // Working directory should be unchanged
        assert_eq!(state.projects[0].working_directory, "/tmp");
    }

    #[test]
    fn cancel_working_directory_resets_state() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        state.ui.input_text = "/discard/this".to_string();
        assert_eq!(state.ui.mode, AppMode::InputPrompt);

        state.cancel_working_directory();

        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.input_text.is_empty());
        assert_eq!(state.ui.input_cursor, 0);
        assert!(state.ui.prompt_label.is_empty());
        assert!(state.ui.prompt_context.is_none());
        // Working directory should be unchanged
        assert_eq!(state.projects[0].working_directory, "/tmp");
    }

    // ── Streaming text cap ─────────────────────────────────────────────

    #[test]
    fn streaming_text_truncates_when_cap_exceeded() {
        let mut state = make_state_with_tasks();
        // Set up a session mapping
        let session_id = "session-abc";
        state.tasks.get_mut("task-0").unwrap().session_id = Some(session_id.to_string());
        state.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Fill buffer well past the 1MB cap (write 1.1MB of ASCII)
        let chunk_size = STREAMING_TEXT_CAP_BYTES + 100_000;
        let big_chunk = "x".repeat(chunk_size);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", &big_chunk);

        let text = state.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();

        // Should be truncated to at most the cap size
        assert!(
            text.len() <= STREAMING_TEXT_CAP_BYTES + 10,
            "Expected <= {}, got {}",
            STREAMING_TEXT_CAP_BYTES,
            text.len()
        );
        // Should be valid UTF-8
        assert!(text.is_char_boundary(text.len()));
        // Should contain only the tail (most recent content)
        assert!(text.chars().all(|c| c == 'x'));
    }

    #[test]
    fn streaming_text_no_truncation_below_cap() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        state.tasks.get_mut("task-0").unwrap().session_id = Some(session_id.to_string());
        state.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Write data well below the cap
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "hello world");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", " and more");

        let text = state.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();
        assert_eq!(text, "hello world and more");
    }

    #[test]
    fn streaming_text_truncation_preserves_utf8_boundary() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        state.tasks.get_mut("task-0").unwrap().session_id = Some(session_id.to_string());
        state.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Fill past the cap with multi-byte characters (emoji are 4 bytes each)
        let emoji = "🎉"; // 4 bytes
        let count = (STREAMING_TEXT_CAP_BYTES / 4) + 100_000;
        let big_chunk = emoji.repeat(count);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", &big_chunk);

        let text = state.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();

        // Should be valid UTF-8 (no panic from invalid boundary)
        assert!(text.is_char_boundary(text.len()));
        // All characters should be complete emojis
        assert!(text.chars().all(|c| c == '🎉'));
    }

    // ── restore_state ────────────────────────────────────────────────────

    #[test]
    fn restore_state_empty_input_clears_existing_data() {
        let mut state = make_state_with_tasks();
        assert!(!state.tasks.is_empty());
        assert!(!state.projects.is_empty());

        state.restore_state(vec![], vec![], HashMap::new(), None, HashMap::new());

        assert!(state.tasks.is_empty());
        assert!(state.projects.is_empty());
        assert!(state.active_project_id.is_none());
        assert!(state.task_number_counters.is_empty());
        assert!(state.session_to_task.is_empty());
    }

    #[test]
    fn restore_state_fully_populated() {
        let mut state = AppState::default();

        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let task = CortexTask {
            id: "task-1".to_string(),
            number: 5,
            title: "Restored Task".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: Some("sess-1".to_string()),
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let mut kanban = HashMap::new();
        kanban.insert("todo".to_string(), vec!["task-1".to_string()]);
        let mut counters = HashMap::new();
        counters.insert("proj-1".to_string(), 5u32);

        state.restore_state(
            vec![project],
            vec![task],
            kanban,
            Some("proj-1".to_string()),
            counters,
        );

        assert_eq!(state.projects.len(), 1);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.active_project_id, Some("proj-1".to_string()));
        assert_eq!(state.task_number_counters.get("proj-1"), Some(&5));
        assert_eq!(
            state.session_to_task.get("sess-1"),
            Some(&"task-1".to_string())
        );
        assert_eq!(state.tasks.get("task-1").unwrap().title, "Restored Task");
    }

    #[test]
    fn restore_state_rebuilds_kanban_for_active_project() {
        let mut state = AppState::default();

        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "P1".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let project2 = CortexProject {
            id: "proj-2".to_string(),
            name: "P2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        };
        let task1 = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "Task 1".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let task2 = CortexTask {
            id: "task-2".to_string(),
            number: 1,
            title: "Task 2".to_string(),
            description: String::new(),
            column: KanbanColumn("done".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Complete,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-2".to_string(),
        };

        state.restore_state(
            vec![project, project2],
            vec![task1, task2],
            HashMap::new(),
            Some("proj-1".to_string()),
            HashMap::new(),
        );

        // Kanban should only contain proj-1's tasks
        assert_eq!(state.kanban.columns.get("todo").unwrap().len(), 1);
        assert_eq!(state.kanban.columns.get("todo").unwrap()[0], "task-1");
        assert!(!state.kanban.columns.contains_key("done"));
    }

    #[test]
    fn restore_state_no_active_project_skips_kanban_rebuild() {
        let mut state = AppState::default();

        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "P1".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let task = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "Task".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };

        state.restore_state(
            vec![project],
            vec![task],
            HashMap::new(),
            None,
            HashMap::new(),
        );

        assert!(state.active_project_id.is_none());
        // Kanban columns should be empty since no active project triggered a rebuild
        assert!(state.kanban.columns.is_empty());
    }

    #[test]
    fn restore_state_session_index_rebuilt_from_tasks() {
        let mut state = AppState::default();

        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "P1".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let task1 = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "T1".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: Some("sess-a".to_string()),
            agent_type: None,
            agent_status: AgentStatus::Running,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        let task2 = CortexTask {
            id: "task-2".to_string(),
            number: 2,
            title: "T2".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: Some("sess-b".to_string()),
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };

        state.restore_state(
            vec![project],
            vec![task1, task2],
            HashMap::new(),
            Some("proj-1".to_string()),
            HashMap::new(),
        );

        assert_eq!(state.session_to_task.get("sess-a"), Some(&"task-1".to_string()));
        assert_eq!(state.session_to_task.get("sess-b"), Some(&"task-2".to_string()));
    }

    #[test]
    fn restore_state_counter_restoration() {
        let mut state = AppState::default();

        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "P1".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        let mut counters = HashMap::new();
        counters.insert("proj-1".to_string(), 42u32);
        counters.insert("proj-2".to_string(), 7u32);

        state.restore_state(
            vec![project],
            vec![],
            HashMap::new(),
            Some("proj-1".to_string()),
            counters,
        );

        assert_eq!(state.task_number_counters.get("proj-1"), Some(&42));
        assert_eq!(state.task_number_counters.get("proj-2"), Some(&7));
    }

    // ── remove_project ───────────────────────────────────────────────────

    #[test]
    fn remove_project_deletes_project_and_its_tasks() {
        let mut state = make_state_with_tasks();
        // Add another project
        let p2 = CortexProject {
            id: "proj-2".to_string(),
            name: "Project 2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        };
        state.add_project(p2);
        let p2_task = CortexTask {
            id: "task-p2".to_string(),
            number: 1,
            title: "P2 Task".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: None,
            agent_type: None,
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-2".to_string(),
        };
        state.tasks.insert("task-p2".to_string(), p2_task);

        assert_eq!(state.projects.len(), 2);
        assert_eq!(state.tasks.len(), 4); // 3 from proj-1 + 1 from proj-2

        state.remove_project("proj-1");

        assert_eq!(state.projects.len(), 1);
        assert_eq!(state.projects[0].id, "proj-2");
        // Only the proj-2 task should remain
        assert_eq!(state.tasks.len(), 1);
        assert!(state.tasks.contains_key("task-p2"));
        assert!(!state.tasks.contains_key("task-0"));
    }

    #[test]
    fn remove_project_clears_session_data_for_removed_tasks() {
        let mut state = make_state_with_tasks();
        // Add session data for a task in proj-1
        state.tasks.get_mut("task-0").unwrap().session_id = Some("sess-1".to_string());
        state.session_to_task.insert("sess-1".to_string(), "task-0".to_string());
        state.task_sessions.insert("task-0".to_string(), TaskDetailSession {
            task_id: "task-0".to_string(),
            session_id: Some("sess-1".to_string()),
            ..Default::default()
        });
        state.cached_streaming_lines.insert("task-0".to_string(), (0, vec![]));

        state.remove_project("proj-1");

        // session_to_task should be cleaned
        assert!(!state.session_to_task.contains_key("sess-1"));
        // task_sessions should be cleaned
        assert!(!state.task_sessions.contains_key("task-0"));
        // cached_streaming_lines should be cleaned
        assert!(!state.cached_streaming_lines.contains_key("task-0"));
        // All tasks should be gone
        assert!(state.tasks.is_empty());
    }

    #[test]
    fn remove_project_clears_tasks_from_kanban() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.kanban.columns.get("todo").unwrap().len(), 3);

        state.remove_project("proj-1");

        // All proj-1 tasks should be removed from the kanban
        let todo_tasks = state.kanban.columns.get("todo").cloned().unwrap_or_default();
        assert!(todo_tasks.is_empty(), "Expected empty todo column after removing project, got: {:?}", todo_tasks);
    }

    #[test]
    fn remove_active_project_falls_back_to_first_remaining() {
        let mut state = make_state_with_tasks();
        state.active_project_id = Some("proj-1".to_string());

        let p2 = CortexProject {
            id: "proj-2".to_string(),
            name: "Project 2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        };
        state.add_project(p2);

        state.remove_project("proj-1");

        assert_eq!(state.active_project_id, Some("proj-2".to_string()));
    }

    #[test]
    fn remove_only_project_clears_active_project() {
        let mut state = make_state_with_tasks();
        state.active_project_id = Some("proj-1".to_string());

        state.remove_project("proj-1");

        assert!(state.active_project_id.is_none());
    }

    #[test]
    fn remove_nonexistent_project_is_noop() {
        let mut state = make_state_with_tasks();
        let project_count = state.projects.len();
        let task_count = state.tasks.len();

        state.remove_project("nonexistent");

        assert_eq!(state.projects.len(), project_count);
        assert_eq!(state.tasks.len(), task_count);
    }

    #[test]
    fn remove_project_clears_focused_task_if_removed() {
        let mut state = make_state_with_tasks();
        state.ui.focused_task_id = Some("task-1".to_string());

        state.remove_project("proj-1");

        // Focused task was in the removed project, should be cleared
        assert!(state.ui.focused_task_id.is_none());
    }

    // ── Notification: boundary conditions ────────────────────────────────

    #[test]
    fn clear_expired_notifications_mixed_expired_and_fresh() {
        let mut state = AppState::default();
        let now = chrono::Utc::now().timestamp_millis();

        // Two expired, one fresh
        state.ui.notifications.push_back(Notification {
            message: "Expired1".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now - 2000,
        });
        state.ui.notifications.push_back(Notification {
            message: "Expired2".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now - 1000,
        });
        state.ui.notifications.push_back(Notification {
            message: "Fresh".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now + 10000,
        });

        let removed = state.clear_expired_notifications();
        assert!(removed);
        assert_eq!(state.ui.notifications.len(), 1);
        assert_eq!(state.ui.notifications.front().unwrap().message, "Fresh");
    }

    #[test]
    fn clear_expired_notifications_empty_queue_returns_false() {
        let mut state = AppState::default();
        let removed = state.clear_expired_notifications();
        assert!(!removed);
    }

    #[test]
    fn clear_expired_notifications_all_expired() {
        let mut state = AppState::default();
        let now = chrono::Utc::now().timestamp_millis();

        state.ui.notifications.push_back(Notification {
            message: "A".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now - 5000,
        });
        state.ui.notifications.push_back(Notification {
            message: "B".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now - 3000,
        });

        let removed = state.clear_expired_notifications();
        assert!(removed);
        assert!(state.ui.notifications.is_empty());
    }

    #[test]
    fn clear_expired_notifications_none_expired_returns_false() {
        let mut state = AppState::default();
        let now = chrono::Utc::now().timestamp_millis();

        state.ui.notifications.push_back(Notification {
            message: "Future".to_string(),
            variant: NotificationVariant::Info,
            expires_at: now + 60000,
        });

        let removed = state.clear_expired_notifications();
        assert!(!removed);
        assert_eq!(state.ui.notifications.len(), 1);
    }

    #[test]
    fn set_notification_exactly_at_max() {
        let mut state = AppState::default();
        state.set_notification("A".to_string(), NotificationVariant::Info, 5000);
        state.set_notification("B".to_string(), NotificationVariant::Info, 5000);
        state.set_notification("C".to_string(), NotificationVariant::Info, 5000);
        assert_eq!(state.ui.notifications.len(), MAX_NOTIFICATIONS);

        // Adding one more evicts oldest
        state.set_notification("D".to_string(), NotificationVariant::Info, 5000);
        assert_eq!(state.ui.notifications.len(), MAX_NOTIFICATIONS);
        assert_eq!(state.ui.notifications.front().unwrap().message, "B");
        assert_eq!(state.ui.notifications.back().unwrap().message, "D");
    }

    #[test]
    fn set_notification_multiple_evictions() {
        let mut state = AppState::default();
        for i in 0..10 {
            state.set_notification(format!("N{}", i), NotificationVariant::Info, 5000);
        }
        assert_eq!(state.ui.notifications.len(), MAX_NOTIFICATIONS);
        // Should have the last MAX_NOTIFICATIONS
        assert_eq!(state.ui.notifications.front().unwrap().message, "N7");
        assert_eq!(state.ui.notifications.back().unwrap().message, "N9");
    }
}
