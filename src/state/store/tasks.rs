//! Task CRUD methods on AppState.

use crate::state::types::*;

impl AppState {
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
            .project_registry
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
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            review_status: ReviewStatus::Pending,
            had_write_operations: false,
            created_at: now,
            updated_at: now,
            project_id: project_id.to_string(),
            blocked_by: Vec::new(),
        };

        // Add to kanban
        self.tasks.insert(id.clone(), task.clone());
        self.kanban
            .columns
            .entry(KanbanColumn::TODO.to_string())
            .or_default()
            .push(id.clone());
        self.mark_task_dirty(&id);
        self.mark_render_dirty();
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
        if from_column == to_column {
            return false; // No-op if moving to the same column
        }

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
        self.mark_render_dirty();
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
                self.session_tracker.session_to_task.remove(sid);
            }
            // Remove render cache for deleted task
            self.session_tracker.cached_streaming_lines.remove(task_id);
            // Remove session data for deleted task
            self.session_tracker.task_sessions.remove(task_id);
            // Remove from dirty set (task no longer exists)
            self.dirty_flags.dirty_tasks.remove(task_id);
            // Track deletion for persistence — save_state will DELETE from DB
            self.dirty_flags.deleted_tasks.insert(task_id.to_string());
            // Clean up subagent data for this task
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
            // Also clean up subagent session data keyed by this task's own session_id
            // (if this task was a subagent of another)
            if let Some(ref sid) = session_id {
                self.session_tracker.subagent_session_data.remove(sid);
            }
            self.mark_dirty();
            session_id
        } else {
            None
        }
    }

    /// Update a task's agent status and bump `last_activity_at`. Marks state dirty.
    /// When the new status is `Ready` or `Complete` and the task has a
    /// `pending_description`, it is applied to the task's `description`
    /// (and `pending_description` is cleared).
    pub fn update_task_agent_status(&mut self, task_id: &str, status: AgentStatus) {
        // Check before moving status into the task
        let should_apply_pending = matches!(status, AgentStatus::Ready | AgentStatus::Complete);

        if let Some(task) = self.tasks.get_mut(task_id) {
            task.agent_status = status;
            task.last_activity_at = chrono::Utc::now().timestamp();

            // Apply pending description when task reaches Ready or Complete
            if should_apply_pending {
                if let Some(ref pending_desc) = task.pending_description {
                    let trimmed = pending_desc.trim().to_string();
                    if !trimmed.is_empty() {
                        task.description = trimmed;
                        task.title = derive_title_from_description(&task.description);
                        task.pending_description = None;
                    }
                }
            }

            self.mark_task_dirty(task_id);
        }
    }

    /// Check all Running tasks for hung-agent detection.
    /// A task is considered "Hung" if it has been Running for longer than
    /// `timeout_secs` without any SSE activity (based on `last_activity_at`).
    ///
    /// Returns the number of tasks that were newly marked as Hung.
    pub fn check_hung_agents(&mut self, timeout_secs: i64) -> usize {
        let now = chrono::Utc::now().timestamp();
        let mut newly_hung = 0;
        let hung_task_ids: Vec<String> = self
            .tasks
            .iter()
            .filter(|(_, task)| {
                task.agent_status == AgentStatus::Running
                    && (now - task.last_activity_at) > timeout_secs
            })
            .map(|(id, _)| id.clone())
            .collect();

        for task_id in &hung_task_ids {
            tracing::debug!(
                task_id = %task_id,
                idle_secs = now - self.tasks.get(task_id).map(|t| t.last_activity_at).unwrap_or(0),
                "Marking task as Hung — no activity for {}s",
                timeout_secs
            );
            self.update_task_agent_status(task_id, AgentStatus::Hung);
            newly_hung += 1;
        }

        newly_hung
    }

    /// Set or clear a task's OpenCode session ID. Maintains the
    /// `session_to_task` reverse index. Marks state dirty.
    ///
    /// **Important:** This method does NOT clear session streaming data
    /// (streaming_text, messages, etc.). Callers that need to clear stale
    /// session state must call [`Self::clear_session_data`] explicitly.
    /// This separation prevents the finalize task from losing streaming
    /// data when the next agent's session starts concurrently.
    pub fn set_task_session_id(&mut self, task_id: &str, session_id: Option<String>) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            // Remove old mapping
            if let Some(ref old_sid) = task.session_id {
                self.session_tracker.session_to_task.remove(old_sid);
            }
            // Set new mapping
            task.session_id = session_id.clone();
            if let Some(ref sid) = session_id {
                self.session_tracker
                    .session_to_task
                    .insert(sid.clone(), task_id.to_string());
            }
            self.mark_task_dirty(task_id);
        }
    }

    /// Clear stale streaming state for a task's session.
    ///
    /// Call this explicitly when a new session starts on an existing task
    /// and the old session's streaming data should not persist (e.g., after
    /// the old session has been finalized).
    ///
    /// This is separated from [`Self::set_task_session_id`] so that callers
    /// can set a new session ID without wiping streaming data that the
    /// finalize task may still need to read.
    pub fn clear_session_data(&mut self, task_id: &str) {
        if let Some(session) = self.session_tracker.task_sessions.get_mut(task_id) {
            session.streaming_text = None;
            session.messages.clear();
            session.seen_delta_keys.clear();
            session.last_delta_key = None;
            session.last_delta_content = None;
            session.render_version += 1;
            self.session_tracker.cached_streaming_lines.remove(task_id);
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

    /// Populate a task's session data from fetched messages.
    ///
    /// Used on startup to restore agent output for tasks that were Running
    /// (or Question/Error) when the application was restarted. The full
    /// message history is fetched from the OpenCode server and injected into
    /// `task_sessions` so the task detail panel can render the output.
    pub fn rehydrate_task_session(&mut self, task_id: &str, messages: Vec<TaskMessage>) {
        let session = self
            .session_tracker
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });

        // Carry over the session_id from the task if we have one
        if session.session_id.is_none() {
            session.session_id = self.tasks.get(task_id).and_then(|t| t.session_id.clone());
        }

        session.messages = messages;
        // Clear any stale streaming state — messages replace it
        session.streaming_text = None;
        session.render_version += 1;
        self.mark_render_dirty();
    }

    /// Mark a running task whose session no longer exists on the server as Error.
    ///
    /// This can happen when the application is restarted while an agent was
    /// running and the OpenCode server no longer has the session data (e.g.,
    /// the session was never persisted, or the data directory was cleaned).
    pub fn mark_orphaned_running_task(&mut self, task_id: &str) {
        self.set_task_error(
            task_id,
            "Agent session lost — the session no longer exists on the server. \
             This can happen when the application is restarted while an agent \
             was running."
                .to_string(),
        );
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
        self.session_tracker
            .session_to_task
            .get(session_id)
            .map(|s| s.as_str())
    }
}
