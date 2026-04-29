//! Project CRUD methods on AppState.

use crate::state::types::*;

impl AppState {
    // ─── Project Methods ─────────────────────────────────────────────────

    /// Register a new project. Marks state dirty.
    pub fn add_project(&mut self, project: CortexProject) {
        self.project_registry.projects.push(project);
        self.mark_dirty();
    }

    /// Remove a project and all its tasks. If this was the active project,
    /// falls back to the first remaining project. Marks state dirty.
    pub fn remove_project(&mut self, project_id: &str) {
        self.project_registry
            .projects
            .retain(|p| p.id != project_id);

        // Track this project for deletion from the database
        self.dirty_flags
            .deleted_projects
            .insert(project_id.to_string());

        // Collect task IDs for this project before removing them
        let project_task_ids: Vec<String> = self
            .tasks
            .values()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.id.clone())
            .collect();

        // Track tasks for deletion from the database
        for task_id in &project_task_ids {
            self.dirty_flags.deleted_tasks.insert(task_id.clone());
        }

        // Remove tasks and clean up associated data
        for task_id in &project_task_ids {
            // Remove session mapping
            if let Some(task) = self.tasks.get(task_id) {
                if let Some(ref sid) = task.session_id {
                    self.session_tracker.session_to_task.remove(sid);
                }
            }
            // Remove session data and streaming cache
            self.session_tracker.task_sessions.remove(task_id);
            self.session_tracker.cached_streaming_lines.remove(task_id);
            self.dirty_flags.dirty_tasks.remove(task_id);
            // Clean up subagent data for each task in this project
            if let Some(sessions) = self.session_tracker.subagent_sessions.remove(task_id) {
                for sub in &sessions {
                    self.session_tracker
                        .subagent_to_parent
                        .remove(&sub.session_id);
                    self.session_tracker
                        .subagent_session_data
                        .remove(&sub.session_id);
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

        if self.project_registry.active_project_id.as_deref() == Some(project_id) {
            self.project_registry.active_project_id =
                self.project_registry.projects.first().map(|p| p.id.clone());
        }
        self.mark_dirty();
    }

    /// Set the active project and rebuild the kanban board for it.
    pub fn select_project(&mut self, project_id: &str) {
        self.project_registry.active_project_id = Some(project_id.to_string());
        // Rebuild kanban for selected project
        self.rebuild_kanban_for_project(project_id);
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

        for project in &mut self.project_registry.projects {
            if project.id == project_id {
                project.status = status;
                break;
            }
        }
    }
}
