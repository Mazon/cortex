//! App state store — mutation methods on AppState.

use crate::state::types::*;
use std::collections::HashMap;

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
        self.tasks.retain(|_, t| t.project_id != project_id);
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

    /// Create a new task in the "todo" column. Returns the created task.
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
            agent_type: TaskAgentType::None,
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
        self.mark_dirty();
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

        self.mark_dirty();
        true
    }

    /// Delete a task by ID. Removes it from the kanban board and session index.
    /// Returns `false` if the task doesn't exist. Marks state dirty.
    pub fn delete_task(&mut self, task_id: &str) -> bool {
        if let Some(task) = self.tasks.remove(task_id) {
            // Remove from kanban
            if let Some(tasks) = self.kanban.columns.get_mut(&task.column.0) {
                tasks.retain(|id| id != task_id);
            }
            // Remove session mapping
            if let Some(sid) = &task.session_id {
                self.session_to_task.remove(sid);
            }
            // Remove render cache for deleted task
            self.cached_streaming_lines.remove(task_id);
            self.mark_dirty();
            true
        } else {
            false
        }
    }

    /// Update a task's agent status and bump `last_activity_at`. Marks state dirty.
    pub fn update_task_agent_status(&mut self, task_id: &str, status: AgentStatus) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.agent_status = status;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_dirty();
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
            self.mark_dirty();
        }
    }

    /// Record an error on a task and set its agent status to [`AgentStatus::Error`].
    /// Marks state dirty.
    pub fn set_task_error(&mut self, task_id: &str, error: String) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error_message = Some(error);
            task.agent_status = AgentStatus::Error;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_dirty();
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
    }

    /// Close the task detail panel and return focus to the kanban board.
    pub fn close_task_detail(&mut self) {
        self.ui.viewing_task_id = None;
        self.ui.focused_panel = FocusedPanel::Kanban;
    }

    // ─── Task Editor Mode ────────────────────────────────────────────────

    /// Open the task editor in "create" mode.
    pub fn open_task_editor_create(&mut self, default_column: &str) {
        self.ui.task_editor = Some(TaskEditorState::new_for_create(default_column));
        self.ui.mode = AppMode::TaskEditor;
    }

    /// Open the task editor in "edit" mode for an existing task.
    /// Does nothing if the task doesn't exist.
    pub fn open_task_editor_edit(&mut self, task_id: &str) {
        if let Some(task) = self.tasks.get(task_id) {
            self.ui.task_editor = Some(TaskEditorState::new_for_edit(task));
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
        let title = title.trim().to_string();
        if title.is_empty() {
            anyhow::bail!("Task title cannot be empty");
        }

        let now = chrono::Utc::now().timestamp();
        let column_id = editor.column_id.as_deref().unwrap_or(KanbanColumn::TODO);

        match &editor.task_id {
            Some(task_id) => {
                // Editing existing task
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.title = title;
                    task.description = description;
                    task.updated_at = now;
                    self.mark_dirty();
                    Ok(task_id.clone())
                } else {
                    anyhow::bail!("Task not found: {}", task_id)
                }
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
    /// active project. Returns `false` if the path is empty or no project is
    /// active.
    pub fn submit_working_directory(&mut self) -> bool {
        let dir = if self.ui.input_text.trim().is_empty() {
            return false;
        } else {
            self.ui.input_text.trim().to_string()
        };

        let project_id = match self.active_project_id.clone() {
            Some(id) => id,
            None => return false,
        };

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
            true
        } else {
            false
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
    /// Replaces any existing notification.
    pub fn set_notification(
        &mut self,
        message: String,
        variant: NotificationVariant,
        duration_ms: i64,
    ) {
        let expires_at = chrono::Utc::now().timestamp_millis() + duration_ms;
        self.ui.notification = Some(Notification {
            message,
            variant,
            expires_at,
        });
    }

    /// Clear expired notifications. Returns `true` if a notification was removed.
    pub fn clear_expired_notifications(&mut self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(ref n) = self.ui.notification {
            if n.expires_at <= now {
                self.ui.notification = None;
                return true;
            }
        }
        false
    }

    // ─── Session Data ────────────────────────────────────────────────────

    /// Replace the message history for a task's session.
    pub fn update_session_messages(&mut self, task_id: &str, messages: Vec<TaskMessage>) {
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

    // ─── SSE Processing Helpers ──────────────────────────────────────────

    /// Handle a `SessionStatus` SSE event — map the status string to
    /// [`AgentStatus`] and update the corresponding task.
    pub fn process_session_status(&mut self, session_id: &str, status: &str) {
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
    /// Returns the task ID if a task was found and marked complete, `None` otherwise.
    pub fn process_session_idle(&mut self, session_id: &str) -> Option<String> {
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
    pub fn process_message_part_delta(&mut self, session_id: &str, delta: &str) {
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
            match &mut session.streaming_text {
                Some(text) => text.push_str(delta),
                None => {
                    session.streaming_text = Some(delta.to_string());
                }
            }
            session.render_version += 1;
        }
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
        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            let request = PermissionRequest {
                id: perm_id.to_string(),
                session_id: session_id.to_string(),
                tool_name: tool.to_string(),
                description: desc.to_string(),
                status: "pending".to_string(),
                details: None,
            };
            self.add_permission_request(&task_id, request);
        }
    }

    // ─── Dirty Flag ──────────────────────────────────────────────────────

    /// Set the persistence dirty flag.
    pub fn mark_dirty(&self) {
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
                agent_type: TaskAgentType::None,
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

        state.open_task_editor_create("todo");
        assert_eq!(state.ui.mode, AppMode::TaskEditor);
        assert!(state.ui.task_editor.is_some());

        let editor = state.get_task_editor().unwrap();
        assert_eq!(editor.task_id, None); // creating new
        assert_eq!(editor.column_id, Some("todo".to_string()));
        assert_eq!(editor.focused_field, EditorField::Title);
    }

    #[test]
    fn open_task_editor_edit_populates_from_existing_task() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("task-1");

        assert_eq!(state.ui.mode, AppMode::TaskEditor);
        let editor = state.get_task_editor().unwrap();
        assert_eq!(editor.task_id, Some("task-1".to_string()));
        assert_eq!(editor.title, "Task 1");
        assert_eq!(editor.focused_field, EditorField::Title);
    }

    #[test]
    fn open_task_editor_edit_nonexistent_does_nothing() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("nonexistent");

        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.task_editor.is_none());
    }

    // ── Task editor: save ───────────────────────────────────────────────

    #[test]
    fn save_task_editor_create_new_task() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo");

        // Set title and description via editor
        if let Some(editor) = state.get_task_editor_mut() {
            editor.title = "New Task".to_string();
            editor.set_description("Some desc");
        }

        let result = state.save_task_editor();
        assert!(result.is_ok());
        let task_id = result.unwrap();

        // Mode should be back to normal (save doesn't change mode, but the
        // App handle_editor_key method does that after save)
        // Verify task was created
        assert!(state.tasks.contains_key(&task_id));
        let task = state.tasks.get(&task_id).unwrap();
        assert_eq!(task.title, "New Task");
        assert_eq!(task.description, "Some desc");
    }

    #[test]
    fn save_task_editor_empty_title_fails() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo");

        // Leave title empty
        let result = state.save_task_editor();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn save_task_editor_edit_existing() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_edit("task-1");

        if let Some(editor) = state.get_task_editor_mut() {
            editor.title = "Updated Title".to_string();
        }

        let result = state.save_task_editor();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "task-1");

        let task = state.tasks.get("task-1").unwrap();
        assert_eq!(task.title, "Updated Title");
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
        state.open_task_editor_create("todo");
        assert_eq!(state.ui.mode, AppMode::TaskEditor);

        state.cancel_task_editor();
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert!(state.ui.task_editor.is_none());
    }

    #[test]
    fn cancel_task_editor_discards_changes() {
        let mut state = make_state_with_tasks();
        state.open_task_editor_create("todo");

        if let Some(editor) = state.get_task_editor_mut() {
            editor.title = "Discard Me".to_string();
        }

        state.cancel_task_editor();
        // The task should NOT have been created
        assert!(!state.tasks.values().any(|t| t.title == "Discard Me"));
    }

    // ── Task operations: delete ─────────────────────────────────────────

    #[test]
    fn delete_task_removes_from_state_and_kanban() {
        let mut state = make_state_with_tasks();
        assert!(state.tasks.contains_key("task-1"));

        let deleted = state.delete_task("task-1");
        assert!(deleted);
        assert!(!state.tasks.contains_key("task-1"));
        assert!(!state
            .kanban
            .columns
            .get("todo")
            .unwrap()
            .contains(&"task-1".to_string()));
    }

    #[test]
    fn delete_nonexistent_task_returns_false() {
        let mut state = make_state_with_tasks();
        let deleted = state.delete_task("nonexistent");
        assert!(!deleted);
    }

    #[test]
    fn delete_task_clears_session_mapping() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-1").unwrap().session_id = Some("session-abc".to_string());
        state
            .session_to_task
            .insert("session-abc".to_string(), "task-1".to_string());

        state.delete_task("task-1");
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

        let notif = state.ui.notification.as_ref().unwrap();
        assert_eq!(notif.message, "Hello world");
        assert_eq!(notif.variant, NotificationVariant::Info);
    }

    #[test]
    fn set_notification_overwrites_previous() {
        let mut state = AppState::default();
        state.set_notification("First".to_string(), NotificationVariant::Info, 5000);
        state.set_notification("Second".to_string(), NotificationVariant::Warning, 5000);

        let notif = state.ui.notification.as_ref().unwrap();
        assert_eq!(notif.message, "Second");
        assert_eq!(notif.variant, NotificationVariant::Warning);
    }

    #[test]
    fn clear_expired_notifications_removes_old() {
        let mut state = AppState::default();
        // Set notification that expired 1 second ago
        let expires_at = chrono::Utc::now().timestamp_millis() - 1000;
        state.ui.notification = Some(Notification {
            message: "Old".to_string(),
            variant: NotificationVariant::Info,
            expires_at,
        });

        state.clear_expired_notifications();
        assert!(state.ui.notification.is_none());
    }

    #[test]
    fn clear_expired_notifications_keeps_fresh() {
        let mut state = AppState::default();
        // Set notification that expires far in the future
        let expires_at = chrono::Utc::now().timestamp_millis() + 60_000;
        state.ui.notification = Some(Notification {
            message: "Fresh".to_string(),
            variant: NotificationVariant::Success,
            expires_at,
        });

        state.clear_expired_notifications();
        assert!(state.ui.notification.is_some());
        assert_eq!(state.ui.notification.as_ref().unwrap().message, "Fresh");
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
        state.open_task_editor_create("todo");

        if let Some(editor) = state.get_task_editor_mut() {
            editor.title = "Mutated".to_string();
            editor.cursor_col = 7;
        }

        assert_eq!(state.get_task_editor().unwrap().title, "Mutated");
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
        let notif = state.ui.notification.as_ref().unwrap();
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
        let notif = state.ui.notification.as_ref().unwrap();
        assert!(notif.message.contains("No active project"));
        assert_eq!(notif.variant, NotificationVariant::Warning);
    }

    #[test]
    fn submit_working_directory_updates_project_and_resets_mode() {
        let mut state = make_state_with_tasks();
        state.open_set_working_directory();
        state.ui.input_text = "/home/user/project".to_string();

        let result = state.submit_working_directory();

        assert!(result);
        assert_eq!(state.ui.mode, AppMode::Normal);
        assert_eq!(state.projects[0].working_directory, "/home/user/project");
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

        assert!(!result);
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

        assert!(!result);
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
}
