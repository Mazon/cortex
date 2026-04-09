//! App state store — mutation methods on AppState.

use crate::config::types::ColumnsConfig;
use crate::state::types::*;
use std::collections::HashMap;

impl AppState {
    // ─── Project Methods ─────────────────────────────────────────────────

    pub fn add_project(&mut self, project: CortexProject) {
        self.projects.push(project);
        self.mark_dirty();
    }

    pub fn remove_project(&mut self, project_id: &str) {
        self.projects.retain(|p| p.id != project_id);
        self.tasks.retain(|_, t| t.project_id != project_id);
        if self.active_project_id.as_deref() == Some(project_id) {
            self.active_project_id = self.projects.first().map(|p| p.id.clone());
        }
        self.mark_dirty();
    }

    pub fn select_project(&mut self, project_id: &str) {
        self.active_project_id = Some(project_id.to_string());
        // Rebuild kanban for selected project
        self.rebuild_kanban_for_project(project_id);
    }

    // ─── Task Methods ────────────────────────────────────────────────────

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

        self.mark_dirty();
        true
    }

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
            self.mark_dirty();
            true
        } else {
            false
        }
    }

    pub fn update_task_agent_status(&mut self, task_id: &str, status: AgentStatus) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.agent_status = status;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_dirty();
        }
    }

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

    pub fn set_task_error(&mut self, task_id: &str, error: String) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error_message = Some(error);
            task.agent_status = AgentStatus::Error;
            task.last_activity_at = chrono::Utc::now().timestamp();
            self.mark_dirty();
        }
    }

    pub fn get_task_id_by_session(&self, session_id: &str) -> Option<&str> {
        self.session_to_task.get(session_id).map(|s| s.as_str())
    }

    // ─── Navigation ──────────────────────────────────────────────────────

    pub fn set_focused_column(&mut self, column: &str) {
        self.ui.focused_column = column.to_string();
        // Reset focused task index for this column
        let idx = self
            .kanban
            .focused_task_index
            .entry(column.to_string())
            .or_insert(0);
        // Sync focused_task_id with the column's focused index
        if let Some(task_ids) = self.kanban.columns.get(column) {
            self.ui.focused_task_id = task_ids.get(*idx).cloned();
        }
    }

    pub fn set_focused_task(&mut self, task_id: Option<String>) {
        self.ui.focused_task_id = task_id;
    }

    pub fn open_task_detail(&mut self, task_id: &str) {
        self.ui.viewing_task_id = Some(task_id.to_string());
        self.ui.focused_panel = FocusedPanel::TaskDetail;
    }

    pub fn close_task_detail(&mut self) {
        self.ui.viewing_task_id = None;
        self.ui.focused_panel = FocusedPanel::Kanban;
    }

    // ─── Task Editor Mode ────────────────────────────────────────────────

    pub fn open_task_editor_create(&mut self, default_column: &str) {
        self.ui.task_editor = Some(TaskEditorState::new_for_create(default_column));
        self.ui.mode = AppMode::TaskEditor;
    }

    pub fn open_task_editor_edit(&mut self, task_id: &str) {
        if let Some(task) = self.tasks.get(task_id) {
            self.ui.task_editor = Some(TaskEditorState::new_for_edit(task));
            self.ui.mode = AppMode::TaskEditor;
        }
    }

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

    pub fn cancel_task_editor(&mut self) {
        self.ui.task_editor = None;
        self.ui.mode = AppMode::Normal;
    }

    pub fn get_task_editor(&self) -> Option<&TaskEditorState> {
        self.ui.task_editor.as_ref()
    }

    pub fn get_task_editor_mut(&mut self) -> Option<&mut TaskEditorState> {
        self.ui.task_editor.as_mut()
    }

    // ─── Notifications ───────────────────────────────────────────────────

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

    /// Clear expired notifications.
    pub fn clear_expired_notifications(&mut self) {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(ref n) = self.ui.notification {
            if n.expires_at <= now {
                self.ui.notification = None;
            }
        }
    }

    // ─── Session Data ────────────────────────────────────────────────────

    pub fn update_session_messages(&mut self, task_id: &str, messages: Vec<TaskMessage>) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.messages = messages;
    }

    pub fn update_streaming_text(&mut self, task_id: &str, text: Option<String>) {
        let session = self
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.streaming_text = text;
    }

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

    pub fn process_session_idle(&mut self, session_id: &str) {
        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            self.update_task_agent_status(&task_id, AgentStatus::Complete);
            self.set_notification(
                format!("Task agent completed"),
                NotificationVariant::Success,
                5000,
            );
        }
    }

    pub fn process_session_error(&mut self, session_id: &str, error: &str) {
        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            self.set_task_error(&task_id, error.to_string());
        }
    }

    pub fn process_message_updated(&mut self, _session_id: &str, _message: TaskMessage) {
        // Store message in task session data — placeholder
    }

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
        }
    }

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

    pub fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

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

    // ─── Persistence Restore ─────────────────────────────────────────────

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

    /// Get visible column IDs for the current kanban view.
    pub fn get_visible_column_ids(&self, columns_config: &ColumnsConfig) -> Vec<String> {
        columns_config
            .visible_column_ids()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}
