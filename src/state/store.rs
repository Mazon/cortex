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
        let now = chrono::Utc::now().timestamp();
        task.entered_column_at = now;
        task.last_activity_at = now;

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
                session.seen_delta_keys.clear();
                session.last_delta_key = None;
                session.last_delta_content = None;
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

    /// Finalize a completed session's streaming output into persistent
    /// message history.  Called after the agent finishes and the full
    /// message list has been fetched from the OpenCode server.
    ///
    /// Sets `session.messages` (registering any subagent sessions found
    /// in the parts) and clears `streaming_text` so that the completed
    /// messages are rendered without duplication.
    ///
    /// Returns `true` if the session actually had streaming text to
    /// finalize (i.e., this is not a no-op), `false` otherwise.
    pub fn finalize_session_streaming(
        &mut self,
        task_id: &str,
        messages: Vec<TaskMessage>,
    ) -> bool {
        let has_streaming = self
            .task_sessions
            .get(task_id)
            .is_some_and(|s| s.streaming_text.is_some());

        self.update_session_messages(task_id, messages);

        // Extract plan output while session.messages is populated with the full
        // fetched message list. This must happen BEFORE update_streaming_text
        // clears streaming_text, so the fallback path in extract_plan_output
        // can still access it if the messages contain no assistant text parts.
        //
        // Ordering dependency: auto-progression (on_agent_completed → start_agent)
        // is also async, and the "do" agent's build_prompt_for_agent reads
        // task.plan_output. Since both finalization and auto-progression are
        // spawned on the same tokio runtime, the finalization lock-hold completes
        // before start_agent acquires the lock to build the prompt.
        self.extract_plan_output(task_id);

        self.update_streaming_text(task_id, None);

        // Clear dedup tracking — no longer needed after finalization.
        // For main sessions this is the only place these are cleared
        // (subagents also clear on complete/idle/error in
        // process_session_status / process_session_idle).
        if let Some(session) = self.task_sessions.get_mut(task_id) {
            session.seen_delta_keys.clear();
            session.last_delta_key = None;
            session.last_delta_content = None;
        }

        has_streaming
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

    /// Extract plan output from the session's streaming text or message history
    /// and store it in `task.plan_output`. Called when an agent completes,
    /// before auto-progression starts the next agent.
    pub fn extract_plan_output(&mut self, task_id: &str) {
        let plan = if let Some(session) = self.task_sessions.get(task_id) {
            // Prefer finalized messages (assistant text parts)
            let from_messages: String = session.messages.iter()
                .rev()
                .filter_map(|msg| {
                    if matches!(msg.role, MessageRole::Assistant) {
                        msg.parts.iter().filter_map(|p| {
                            if let TaskMessagePart::Text { text } = p {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        }).collect::<Vec<_>>().join("\n").into()
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();

            if !from_messages.is_empty() {
                from_messages
            } else if let Some(ref text) = session.streaming_text {
                text.trim().to_string()
            } else {
                return;
            }
        } else {
            return;
        };

        // Enforce a 50KB cap — keep the tail (most recent content)
        const PLAN_OUTPUT_CAP_BYTES: usize = 50 * 1024;
        let plan = if plan.len() > PLAN_OUTPUT_CAP_BYTES {
            let start = plan.len() - PLAN_OUTPUT_CAP_BYTES;
            let mut i = start;
            while i < plan.len() && !plan.is_char_boundary(i) { i += 1; }
            plan[i..].to_string()
        } else {
            plan
        };

        if let Some(task) = self.tasks.get_mut(task_id) {
            task.plan_output = Some(plan);
            self.mark_task_dirty(task_id);
        }
    }

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
                "running" | "busy" => AgentStatus::Running,
                "complete" | "completed" => AgentStatus::Complete,
                "idle" => {
                    // Don't update status — SessionIdle event handles this
                    // with proper Ready/Complete logic. A SessionStatus "idle"
                    // arriving after SessionIdle would overwrite Ready→Complete.
                    return;
                }
                _ => {
                    tracing::warn!("Unknown session status '{}' for task session, ignoring", status);
                    return;
                }
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

                let agent_label = self.tasks.get(&task_id)
                    .and_then(|t| t.agent_type.clone())
                    .unwrap_or_else(|| "agent".to_string());
                self.set_notification(
                    format!("{} agent completed", agent_label),
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
            //
            // Defense-in-depth: also reject continuations (same key) when the
            // delta content is *identical* to the last processed content.
            // Two concurrent SSE loops delivering the same event will produce
            // the same key AND the same content, while a true continuation
            // (next chunk of the same streaming part) will always differ.
            let delta_key = (message_id.to_string(), part_id.to_string());
            let is_continuation = session.last_delta_key.as_ref() == Some(&delta_key);
            if !is_continuation && session.seen_delta_keys.contains(&delta_key) {
                // Key was seen before but is NOT the current part — replay.
                return;
            }
            if is_continuation
                && session.last_delta_content.as_deref() == Some(delta)
            {
                // Same key AND identical content — duplicate from a second SSE
                // loop, not a genuine continuation. Skip to prevent doubling.
                return;
            }
            if !is_continuation {
                // New part we haven't seen — record it.
                session.seen_delta_keys.insert(delta_key.clone());
            }
            session.last_delta_key = Some(delta_key);
            session.last_delta_content = Some(delta.to_string());

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
                // Clear dedup tracking when subagent completes.
                entry.seen_delta_keys.clear();
                entry.last_delta_key = None;
                entry.last_delta_content = None;
            }
            "error" => {
                entry.seen_delta_keys.clear();
                entry.last_delta_key = None;
                entry.last_delta_content = None;
            }
            _ => {}
        }

        self.mark_render_dirty();
    }

    /// Handle an idle event for a subagent session.
    fn process_subagent_idle(&mut self, session_id: &str, _parent_task_id: &str) {
        self.mark_subagent_inactive(session_id);
        // Clear dedup tracking for the subagent session.
        if let Some(entry) = self.subagent_session_data.get_mut(session_id) {
            entry.seen_delta_keys.clear();
            entry.last_delta_key = None;
            entry.last_delta_content = None;
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
        // Also reject continuations with identical content (defense-in-depth
        // against concurrent SSE connections).
        let delta_key = (message_id.to_string(), part_id.to_string());
        let is_continuation = entry.last_delta_key.as_ref() == Some(&delta_key);
        if !is_continuation && entry.seen_delta_keys.contains(&delta_key) {
            return;
        }
        if is_continuation
            && entry.last_delta_content.as_deref() == Some(delta)
        {
            return;
        }
        if !is_continuation {
            entry.seen_delta_keys.insert(delta_key.clone());
        }
        entry.last_delta_key = Some(delta_key);
        entry.last_delta_content = Some(delta.to_string());

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
    /// Removes cached lines whose key no longer has a corresponding live
    /// session — either a main task in `self.tasks` (keyed by `task_id`)
    /// or a drilled-down subagent in `self.subagent_session_data` (keyed
    /// by `session_id`).  If the cache still exceeds `max_entries` after
    /// that, the oldest half is evicted (by insertion order).
    pub fn prune_streaming_cache(&mut self, max_entries: usize) {
        // Remove entries whose backing session no longer exists.
        // Main sessions are keyed by task_id; subagent sessions by session_id.
        self.cached_streaming_lines
            .retain(|key, _| {
                self.tasks.contains_key(key)
                    || self.subagent_session_data.contains_key(key)
            });

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
        let has_ready = self
            .tasks
            .values()
            .any(|t| t.project_id == project_id && t.agent_status == AgentStatus::Ready);

        let status = if has_error {
            ProjectStatus::Error
        } else if has_question {
            ProjectStatus::Question
        } else if has_running {
            ProjectStatus::Working
        } else if has_ready {
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

    #[test]
    fn clamp_focused_task_index_non_focused_empty_column_does_not_change_focus() {
        let mut state = make_state_with_tasks();
        // focused_column is "todo" with focused_task_id = "task-0"
        assert_eq!(state.ui.focused_column, "todo");
        assert_eq!(state.ui.focused_task_id, Some("task-0".to_string()));

        // Add an empty "planning" column (a non-focused column)
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default();

        // Clamp the non-focused, empty "planning" column
        state.clamp_focused_task_index("planning");

        // focused_task_id must NOT change — we're focused on "todo", not "planning"
        assert_eq!(state.ui.focused_task_id, Some("task-0".to_string()));
        // The planning column's index should still be reset to 0
        assert_eq!(state.kanban.focused_task_index.get("planning"), Some(&0));
    }

    // ── Regression: auto-progression does not corrupt focused_task_id ─────

    /// Regression test for the bug where `clamp_focused_task_index` would
    /// unconditionally overwrite `focused_task_id` even when clamping a
    /// background column. This caused auto-progression events (SSE →
    /// on_agent_completed → move_task) to silently clear the user's focus,
    /// blocking all subsequent keyboard operations (move, edit, delete, view).
    #[test]
    fn regression_auto_progression_does_not_corrupt_focused_task_id() {
        let mut state = make_state_with_tasks();
        // make_state_with_tasks creates 3 tasks (task-0, task-1, task-2) in "todo"
        // with focused_column = "todo" and focused_task_id = "task-0"

        // Simulate: user is focused on "todo" column
        assert_eq!(state.ui.focused_column, "todo");
        assert_eq!(state.ui.focused_task_id, Some("task-0".into()));

        // Step 1: Move task-0 from "todo" to "planning" (user presses 'm')
        // This clamps "todo" (the focused column) — focus updates but stays valid
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default();
        let moved = state.move_task("task-0", KanbanColumn("planning".to_string()));
        assert!(moved);

        // After moving task-0 out of "todo", focus should still be valid
        assert_eq!(state.ui.focused_column, "todo");
        assert!(
            state.ui.focused_task_id.is_some(),
            "focused_task_id should not be None after moving a task out of the focused column"
        );
        let focused = state.ui.focused_task_id.as_ref().unwrap();
        assert!(
            state
                .kanban
                .columns
                .get("todo")
                .unwrap()
                .contains(focused),
            "focused_task_id should point to a task still in the focused 'todo' column"
        );

        // Step 2: Simulate auto-progression — move task-0 from "planning" to "running"
        // This clamps "planning" (NOT the focused column) — must NOT corrupt focused_task_id
        let focus_before = state.ui.focused_task_id.clone();
        state
            .kanban
            .columns
            .entry("running".to_string())
            .or_default();
        let moved = state.move_task("task-0", KanbanColumn("running".to_string()));
        assert!(moved);

        // focused_task_id must NOT have been corrupted by clamping "planning"
        assert_eq!(
            state.ui.focused_task_id, focus_before,
            "Auto-progression in a background column must not change focused_task_id"
        );
        assert_eq!(state.ui.focused_column, "todo");
        let focused = state.ui.focused_task_id.as_ref().unwrap();
        assert!(
            state
                .kanban
                .columns
                .get("todo")
                .unwrap()
                .contains(focused),
            "focused_task_id should still point to a task in 'todo' after background auto-progression"
        );
    }

    /// Direct regression test: calling clamp_focused_task_index on a non-focused
    /// column (even an empty one) must not touch focused_task_id.
    #[test]
    fn regression_clamp_non_focused_empty_column_preserves_focus() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.focused_column, "todo");
        let focus_before = state.ui.focused_task_id.clone();

        // Create an empty "planning" column and clamp it — user is NOT on planning
        state
            .kanban
            .columns
            .entry("planning".to_string())
            .or_default();
        state.clamp_focused_task_index("planning");

        assert_eq!(
            state.ui.focused_task_id, focus_before,
            "Clamping a non-focused empty column must not change focused_task_id"
        );
    }

    /// Direct regression test: calling clamp_focused_task_index on a column
    /// that doesn't exist in kanban.columns must not clear focused_task_id
    /// when it's not the focused column.
    #[test]
    fn regression_clamp_nonexistent_column_preserves_focus() {
        let mut state = make_state_with_tasks();
        assert_eq!(state.ui.focused_column, "todo");
        let focus_before = state.ui.focused_task_id.clone();

        // Clamp a column that doesn't exist at all — must be a no-op for focus
        state.clamp_focused_task_index("nonexistent-column");

        assert_eq!(
            state.ui.focused_task_id, focus_before,
            "Clamping a nonexistent column must not clear focused_task_id"
        );
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

    // ── Regression: streaming duplication bugs ─────────────────────────────

    /// Helper to wire up a session-to-task mapping for streaming tests.
    fn setup_session_mapping(state: &mut AppState, task_id: &str, session_id: &str) {
        state.tasks.get_mut(task_id).unwrap().session_id = Some(session_id.to_string());
        state
            .session_to_task
            .insert(session_id.to_string(), task_id.to_string());
    }

    /// Helper to create a simple text TaskMessage for testing.
    fn make_text_message(id: &str, role: MessageRole, text: &str) -> TaskMessage {
        TaskMessage {
            id: id.to_string(),
            role,
            parts: vec![TaskMessagePart::Text {
                text: text.to_string(),
            }],
            created_at: None,
        }
    }

    // ── Bug 1: streaming_text cleared on new session start ────────────────

    #[test]
    fn regression_streaming_text_cleared_on_new_session_for_same_task() {
        let mut state = make_state_with_tasks();
        let session_1 = "session-old";
        let session_2 = "session-new";

        // Session 1 runs, accumulates streaming text
        setup_session_mapping(&mut state, "task-0", session_1);
        state.process_message_part_delta(session_1, "msg-1", "part-1", "text", "hello ");
        state.process_message_part_delta(session_1, "msg-1", "part-2", "text", "world");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(text, "hello world");

        // New session starts on the SAME task — this is Bug 1's trigger.
        // set_task_session_id should clear the old streaming_text.
        state.set_task_session_id("task-0", Some(session_2.to_string()));

        // Verify streaming_text is cleared
        let session = state.task_sessions.get("task-0").unwrap();
        assert!(
            session.streaming_text.is_none(),
            "streaming_text should be cleared when a new session starts on the same task"
        );
        // Verify messages are also cleared
        assert!(
            session.messages.is_empty(),
            "messages should be cleared when a new session starts on the same task"
        );
        // Verify render_version was bumped (cache invalidation)
        assert!(
            session.render_version > 0,
            "render_version should be bumped to invalidate cache"
        );
        // Verify cached_streaming_lines was removed
        assert!(
            !state.cached_streaming_lines.contains_key("task-0"),
            "cached_streaming_lines should be removed for the task"
        );
    }

    #[test]
    fn regression_new_session_clears_streaming_but_preserves_session_object() {
        let mut state = make_state_with_tasks();
        let session_1 = "session-old";
        let session_2 = "session-new";

        // Set up session 1 with streaming text
        setup_session_mapping(&mut state, "task-0", session_1);
        state.process_message_part_delta(session_1, "msg-1", "part-1", "text", "data");

        // New session starts — should NOT remove the TaskDetailSession entry itself,
        // just clear its fields
        state.set_task_session_id("task-0", Some(session_2.to_string()));

        // The session entry should still exist
        assert!(
            state.task_sessions.contains_key("task-0"),
            "TaskDetailSession entry should still exist for the task"
        );
        // But with cleaned fields
        let session = state.task_sessions.get("task-0").unwrap();
        assert_eq!(session.task_id, "task-0");
        assert!(session.streaming_text.is_none());
        assert!(session.messages.is_empty());
    }

    // ── Bug 2: SSE reconnection replay deduplication ─────────────────────
    //
    // The dedup scheme distinguishes two cases for a given (message_id, part_id):
    //   • Same key as last_delta_key → "continuation" → accepted UNLESS
    //     the delta content is identical to the last processed content
    //     (in which case it's a duplicate from a concurrent SSE loop).
    //   • Different key already in seen_delta_keys → "replay" → skipped
    //     (SSE reconnection replayed an earlier part).
    //   • Different key NOT in seen_delta_keys → "new part" → recorded & accepted.
    //
    // Content-based dedup eliminates the old trade-off where replaying the
    // very last part was indistinguishable from a continuation.

    #[test]
    fn regression_dedup_skips_replayed_earlier_parts() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Normal flow: part-1, then part-2
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "the ");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", "fix");

        let text_before = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(text_before, "the fix");

        // Simulate SSE reconnection: server replays from part-1 again.
        // Part-1 has a different key than last_delta_key (part-2) AND is
        // already in seen_delta_keys → replay → silently skipped.
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "the ");

        let text_after = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text_after, text_before,
            "Replayed earlier part should not duplicate text — got '{}', expected '{}'",
            text_after, text_before
        );
    }

    #[test]
    fn regression_dedup_allows_new_parts_after_replay_skip() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Initial deltas: part-1 then part-2
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "hello ");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", "world");

        // Replay of part-1 (earlier part, different key from last) → skipped
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "hello ");

        // New part from a new message arrives → should be accepted
        state.process_message_part_delta(session_id, "msg-2", "part-1", "text", "!");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "hello world!",
            "New part should be appended after replay skip"
        );
    }

    #[test]
    fn regression_dedup_skips_multiple_earlier_parts_on_replay() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Part-1, part-2, part-3
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "alpha ");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", "beta ");
        state.process_message_part_delta(session_id, "msg-1", "part-3", "text", "gamma");

        // Reconnection replays from part-1 again.
        // part-1: different key from last (part-3), in seen set → skip
        // part-2: different key from last (still part-3), in seen set → skip
        // part-3: same key as last_delta_key AND same content → duplicate → skip
        //   (Content-based dedup catches the last-part trade-off case too.)
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "alpha ");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", "beta ");
        state.process_message_part_delta(session_id, "msg-1", "part-3", "text", "gamma");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        // ALL replayed parts (including the last one) are now skipped thanks
        // to content-based dedup — no more trade-off.
        assert_eq!(
            text, "alpha beta gamma",
            "All replayed parts should be skipped (content-based dedup eliminates the trade-off)"
        );
    }

    #[test]
    fn regression_dedup_continuation_same_key_always_accepted() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Multiple consecutive deltas for the SAME (msg, part) key — all should append
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "a");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "b");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "c");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "d");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "abcd",
            "Consecutive deltas for the same part should all be appended"
        );
    }

    // ── Bug 3: subagent drill-down doesn't double-render ─────────────────

    #[test]
    fn regression_subagent_drilldown_clears_streaming_text() {
        let mut state = make_state_with_tasks();
        let parent_session = "parent-session";
        let sub_session = "sub-session";

        // Set up parent session
        setup_session_mapping(&mut state, "task-0", parent_session);

        // Register a subagent
        state.register_subagent_session("task-0", sub_session, "do");

        // Simulate subagent streaming some text
        state.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "subagent output ");

        let entry = state.subagent_session_data.get(sub_session).unwrap();
        assert_eq!(
            entry.streaming_text.as_ref().unwrap(),
            "subagent output "
        );

        // Simulate drill-down: load complete messages for the subagent.
        // This should clear streaming_text to avoid double-rendering.
        let messages = vec![make_text_message("msg-1", MessageRole::Assistant, "subagent output complete")];
        state.update_session_messages("task-0", messages);

        // The key behavior: when drill-down loads messages, streaming_text
        // should be cleared. In the actual app, this happens in
        // handle_drill_down_subagent(). Here we test that clearing
        // streaming_text prevents double-rendering.
        if let Some(entry) = state.subagent_session_data.get_mut(sub_session) {
            entry.streaming_text = None; // This is what handle_drill_down_subagent does
            entry.render_version += 1;
        }

        // After drill-down, streaming_text should be None (not duplicated)
        let entry = state.subagent_session_data.get(sub_session).unwrap();
        assert!(
            entry.streaming_text.is_none(),
            "streaming_text should be cleared on drill-down to prevent double-rendering"
        );
    }

    #[test]
    fn regression_subagent_delta_deduplication() {
        let mut state = make_state_with_tasks();
        let parent_session = "parent-session";
        let sub_session = "sub-session";

        setup_session_mapping(&mut state, "task-0", parent_session);
        state.register_subagent_session("task-0", sub_session, "do");
        state.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Subagent receives deltas: part-1 then part-2
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "hello ");
        state.process_message_part_delta(sub_session, "msg-1", "part-2", "text", "world");

        // Replay of part-1 (earlier part, different key from last) → skipped
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "hello ");

        let text = state
            .subagent_session_data
            .get(sub_session)
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "hello world",
            "Subagent replayed earlier deltas should be deduplicated, got: '{}'",
            text
        );
    }

    // ── Task 4: session completion finalizes streaming into messages ──────

    #[test]
    fn regression_finalize_session_moves_streaming_to_messages() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Agent streams text
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "the fix");

        // Verify streaming text exists
        let session = state.task_sessions.get("task-0").unwrap();
        assert_eq!(
            session.streaming_text.as_ref().unwrap(),
            "the fix"
        );
        assert!(session.messages.is_empty(), "messages should be empty during streaming");

        // Agent completes — finalize_session_streaming is called with the
        // full message history fetched from the server.
        let messages = vec![make_text_message("msg-1", MessageRole::Assistant, "the fix")];
        let had_streaming = state.finalize_session_streaming("task-0", messages);

        // Should report that there was streaming text to finalize
        assert!(had_streaming, "finalize should report streaming was present");

        let session = state.task_sessions.get("task-0").unwrap();
        // Messages should now contain the completed text
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].id, "msg-1");
        // Streaming text should be cleared
        assert!(
            session.streaming_text.is_none(),
            "streaming_text should be cleared after finalization"
        );
    }

    #[test]
    fn regression_finalize_session_noop_when_no_streaming() {
        let mut state = make_state_with_tasks();

        // No streaming text exists for this task
        let messages = vec![make_text_message("msg-1", MessageRole::Assistant, "existing")];
        let had_streaming = state.finalize_session_streaming("task-0", messages);

        assert!(
            !had_streaming,
            "finalize should report no-op when no streaming text exists"
        );

        let session = state.task_sessions.get("task-0").unwrap();
        assert_eq!(session.messages.len(), 1);
        assert!(session.streaming_text.is_none());
    }

    #[test]
    fn regression_finalize_session_bumps_render_version() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "text");

        let version_before = state.task_sessions.get("task-0").unwrap().render_version;

        let messages = vec![make_text_message("msg-1", MessageRole::Assistant, "text")];
        state.finalize_session_streaming("task-0", messages);

        let version_after = state.task_sessions.get("task-0").unwrap().render_version;
        assert!(
            version_after > version_before,
            "render_version should be bumped after finalization"
        );
    }

    // ── Task 5: non-text field deltas ignored ────────────────────────────

    #[test]
    fn regression_non_text_field_delta_not_appended() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Send a "reasoning" field delta — should NOT be appended to streaming_text
        state.process_message_part_delta(session_id, "msg-1", "part-1", "reasoning", "thinking...");

        let session = state.task_sessions.get("task-0").unwrap();
        assert!(
            session.streaming_text.is_none(),
            "reasoning field deltas should NOT be appended to streaming_text"
        );
    }

    #[test]
    fn regression_mixed_field_deltas_only_text_appended() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Mix of text and non-text deltas
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "visible ");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "reasoning", "hidden");
        state.process_message_part_delta(session_id, "msg-2", "part-1", "text", "text");
        state.process_message_part_delta(session_id, "msg-2", "part-2", "some_other_field", "ignored");
        state.process_message_part_delta(session_id, "msg-3", "part-1", "text", " here");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "visible text here",
            "Only 'text' field deltas should appear in streaming_text, got: '{}'",
            text
        );
    }

    #[test]
    fn regression_non_text_field_delta_still_records_dedup_key() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);

        // Non-text delta should still participate in dedup tracking
        state.process_message_part_delta(session_id, "msg-1", "part-1", "reasoning", "thinking");

        // The key should be recorded even though no text was appended
        let session = state.task_sessions.get("task-0").unwrap();
        assert!(
            session.seen_delta_keys.contains(&(String::from("msg-1"), String::from("part-1"))),
            "Non-text deltas should still record their dedup key"
        );
    }

    // ── Subagent non-text field filtering ─────────────────────────────────

    #[test]
    fn regression_subagent_non_text_field_ignored() {
        let mut state = make_state_with_tasks();
        let parent_session = "parent-session";
        let sub_session = "sub-session";

        setup_session_mapping(&mut state, "task-0", parent_session);
        state.register_subagent_session("task-0", sub_session, "do");
        state.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Subagent receives a reasoning delta — should be ignored
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "reasoning", "thinking...");

        let entry = state.subagent_session_data.get(sub_session).unwrap();
        assert!(
            entry.streaming_text.is_none(),
            "Subagent reasoning deltas should not appear in streaming_text"
        );

        // But a text delta should work
        state.process_message_part_delta(sub_session, "msg-1", "part-2", "text", "actual text");

        let entry = state.subagent_session_data.get(sub_session).unwrap();
        assert_eq!(
            entry.streaming_text.as_ref().unwrap(),
            "actual text"
        );
    }

    // ── Integration: full lifecycle without duplication ───────────────────

    #[test]
    fn regression_full_lifecycle_no_duplication() {
        let mut state = make_state_with_tasks();
        let session_1 = "session-1";
        let session_2 = "session-2";

        // Phase 1: First session runs
        setup_session_mapping(&mut state, "task-0", session_1);
        state.process_message_part_delta(session_1, "msg-1", "part-1", "text", "first ");
        state.process_message_part_delta(session_1, "msg-1", "part-2", "text", "run");
        state.process_message_part_delta(session_1, "msg-1", "part-2", "reasoning", "ignored");

        // Phase 2: Session completes — finalize
        let messages_1 = vec![make_text_message("msg-1", MessageRole::Assistant, "first run")];
        state.finalize_session_streaming("task-0", messages_1);

        let session = state.task_sessions.get("task-0").unwrap();
        assert!(session.streaming_text.is_none());
        assert_eq!(session.messages.len(), 1);

        // Phase 3: New session starts (auto-progression)
        state.set_task_session_id("task-0", Some(session_2.to_string()));

        let session = state.task_sessions.get("task-0").unwrap();
        assert!(session.streaming_text.is_none());
        assert!(session.messages.is_empty(), "messages cleared for new session");

        // Phase 4: Second session streams
        state.process_message_part_delta(session_2, "msg-2", "part-1", "text", "second ");
        state.process_message_part_delta(session_2, "msg-2", "part-2", "text", "run");

        // Simulate reconnection: replay an earlier part (part-1)
        state.process_message_part_delta(session_2, "msg-2", "part-1", "text", "second ");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "second run",
            "Full lifecycle should produce no duplication, got: '{}'",
            text
        );
    }

    // ── Bug: multi-SSE-loop text duplication (defense-in-depth) ────────────
    //
    // When multiple SSE event loops (e.g., one per project) subscribe to
    // the same server, they independently deliver the same MessagePartDelta
    // events. The dedup logic must catch these duplicates even when the
    // second delivery looks like a "continuation" (same delta key as the
    // last processed event). This is achieved by also comparing the actual
    // delta content — a true continuation will always have different content,
    // but a duplicate from another loop will have identical content.

    #[test]
    fn regression_duplicate_sse_loops_dont_double_text() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        setup_session_mapping(&mut state, "task-0", session_id);

        // Simulate two SSE loops processing the same delta.
        // Loop A delivers (msg-1, part-1, "Hello ") — accepted as new part.
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "Hello ");

        // Loop B delivers the exact same delta — should be rejected because
        // it's a continuation (same key) with identical content.
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "Hello ");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "Hello ",
            "Duplicate processing of same delta should not double text, got: '{}'",
            text
        );
    }

    #[test]
    fn regression_duplicate_sse_loops_multiple_deltas() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        setup_session_mapping(&mut state, "task-0", session_id);

        // Simulate interleaved processing from two SSE loops:
        // Loop A: "The " → Loop B: "The " → Loop A: "user " → Loop B: "user "
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "The ");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "The ");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "user ");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "user ");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "The user ",
            "Interleaved duplicate deltas should not double text, got: '{}'",
            text
        );
    }

    #[test]
    fn regression_content_dedup_allows_true_continuations() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        setup_session_mapping(&mut state, "task-0", session_id);

        // Multiple chunks for the same part — all different content.
        // These are true continuations and should ALL be accepted.
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "chunk1 ");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "chunk2 ");
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "chunk3");

        let text = state
            .task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "chunk1 chunk2 chunk3",
            "True continuations with different content should all be accepted"
        );
    }

    #[test]
    fn regression_subagent_duplicate_sse_loops_dont_double_text() {
        let mut state = make_state_with_tasks();
        let parent_session = "parent-session";
        let sub_session = "sub-session";

        setup_session_mapping(&mut state, "task-0", parent_session);
        state.register_subagent_session("task-0", sub_session, "do");
        state.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Simulate two SSE loops delivering the same subagent delta
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "sub ");
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "sub ");

        let text = state
            .subagent_session_data
            .get(sub_session)
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            text, "sub ",
            "Duplicate subagent deltas should not double text, got: '{}'",
            text
        );
    }

    // ── update_project_status ─────────────────────────────────────────

    #[test]
    fn update_project_status_sets_question_when_task_has_pending_questions() {
        let mut state = make_state_with_tasks();

        // Give task-0 a pending question
        if let Some(task) = state.tasks.get_mut("task-0") {
            task.pending_question_count = 2;
        }

        state.update_project_status("proj-1");

        let project = state.projects.iter().find(|p| p.id == "proj-1").unwrap();
        assert_eq!(
            project.status,
            ProjectStatus::Question,
            "Project status should be Question when a task has pending questions"
        );
    }

    #[test]
    fn update_project_status_sets_error_when_task_has_error_status() {
        let mut state = make_state_with_tasks();

        if let Some(task) = state.tasks.get_mut("task-0") {
            task.agent_status = AgentStatus::Error;
        }

        state.update_project_status("proj-1");

        let project = state.projects.iter().find(|p| p.id == "proj-1").unwrap();
        assert_eq!(
            project.status,
            ProjectStatus::Error,
            "Project status should be Error when a task has error agent status"
        );
    }

    #[test]
    fn update_project_status_sets_working_when_task_is_running() {
        let mut state = make_state_with_tasks();

        if let Some(task) = state.tasks.get_mut("task-0") {
            task.agent_status = AgentStatus::Running;
        }

        state.update_project_status("proj-1");

        let project = state.projects.iter().find(|p| p.id == "proj-1").unwrap();
        assert_eq!(
            project.status,
            ProjectStatus::Working,
            "Project status should be Working when a task is running"
        );
    }

    #[test]
    fn update_project_status_sets_idle_when_no_active_tasks() {
        let mut state = make_state_with_tasks();

        // All tasks default to AgentStatus::Pending with pending_question_count = 0
        state.update_project_status("proj-1");

        let project = state.projects.iter().find(|p| p.id == "proj-1").unwrap();
        assert_eq!(
            project.status,
            ProjectStatus::Idle,
            "Project status should be Idle when no tasks are running, errored, or have questions"
        );
    }

    #[test]
    fn update_project_status_does_not_affect_other_projects() {
        let mut state = make_state_with_tasks();

        // Add a second project with no tasks
        let project2 = CortexProject {
            id: "proj-2".to_string(),
            name: "Other Project".to_string(),
            working_directory: "/tmp/other".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        };
        state.add_project(project2);

        // Give task-0 a pending question
        if let Some(task) = state.tasks.get_mut("task-0") {
            task.pending_question_count = 1;
        }

        // Only update proj-1
        state.update_project_status("proj-1");

        let proj1 = state.projects.iter().find(|p| p.id == "proj-1").unwrap();
        let proj2 = state.projects.iter().find(|p| p.id == "proj-2").unwrap();
        assert_eq!(proj1.status, ProjectStatus::Question);
        assert_eq!(
            proj2.status,
            ProjectStatus::Idle,
            "Updating proj-1 should not change proj-2's status"
        );
    }

    // ── Session status: "busy" mapped to Running ───────────────────────────

    #[test]
    fn process_session_status_busy_maps_to_running() {
        let mut state = make_state_with_tasks();
        let session_id = "session-busy-test";
        state
            .tasks
            .get_mut("task-0")
            .unwrap()
            .session_id = Some(session_id.to_string());
        state
            .session_to_task
            .insert(session_id.to_string(), "task-0".to_string());

        state.process_session_status(session_id, "busy");

        assert_eq!(
            state.tasks.get("task-0").unwrap().agent_status,
            AgentStatus::Running,
            "'busy' status should map to AgentStatus::Running"
        );
    }
}
