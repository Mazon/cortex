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

        // Clamp focused_task_index for source column (may be stale after removal)
        self.clamp_focused_task_index(&from_column.0);

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
        columns_config.visible_column_ids().to_vec()
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
            editor.description = "Some desc".to_string();
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
}
