//! App state store — core CRUD methods on AppState.
//!
//! This module contains pure CRUD operations (add/remove/move tasks and
//! projects), session data management, dirty flag handling, persistence
//! restore, and internal helpers.
//!
//! SSE processing methods live in [`sse_processor`], navigation/UI methods
//! in [`navigation`], and permission/question handling in [`permissions`].

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
        self.project_registry.projects.push(project);
        self.mark_dirty();
    }

    /// Remove a project and all its tasks. If this was the active project,
    /// falls back to the first remaining project. Marks state dirty.
    pub fn remove_project(&mut self, project_id: &str) {
        self.project_registry.projects.retain(|p| p.id != project_id);

        // Track this project for deletion from the database
        self.dirty_flags.deleted_projects.insert(project_id.to_string());

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
                    self.session_tracker.subagent_to_parent.remove(&sub.session_id);
                    self.session_tracker.subagent_session_data.remove(&sub.session_id);
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
            self.project_registry.active_project_id = self.project_registry.projects.first().map(|p| p.id.clone());
        }
        self.mark_dirty();
    }

    /// Set the active project and rebuild the kanban board for it.
    pub fn select_project(&mut self, project_id: &str) {
        self.project_registry.active_project_id = Some(project_id.to_string());
        // Rebuild kanban for selected project
        self.rebuild_kanban_for_project(project_id);
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
            .project_registry.task_number_counters
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
                    self.session_tracker.subagent_to_parent.remove(&sub.session_id);
                    self.session_tracker.subagent_session_data.remove(&sub.session_id);
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
            tracing::warn!(
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
                self.session_tracker.session_to_task
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
    pub fn rehydrate_task_session(
        &mut self,
        task_id: &str,
        messages: Vec<TaskMessage>,
    ) {
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
            session.session_id = self
                .tasks
                .get(task_id)
                .and_then(|t| t.session_id.clone());
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
        self.session_tracker.session_to_task.get(session_id).map(|s| s.as_str())
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
        if self.session_tracker.subagent_to_parent.contains_key(session_id) {
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
            self.session_tracker.subagent_to_parent
                .get(&parent_session_id)
                .and_then(|ptid| self.session_tracker.subagent_sessions.get(ptid))
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
            error_message: None,
        };

        // Store under parent task
        self.session_tracker.subagent_sessions
            .entry(parent_task_id.to_string())
            .or_default()
            .push(subagent.clone());

        // Reverse index: child session → parent task
        self.session_tracker.subagent_to_parent
            .insert(session_id.to_string(), parent_task_id.to_string());
    }

    /// Mark a subagent session as inactive (completed or errored).
    pub fn mark_subagent_inactive(&mut self, session_id: &str) {
        if let Some(parent_task_id) = self.session_tracker.subagent_to_parent.get(session_id).cloned() {
            if let Some(sessions) = self.session_tracker.subagent_sessions.get_mut(&parent_task_id) {
                for sub in sessions.iter_mut() {
                    if sub.session_id == session_id {
                        sub.active = false;
                        break;
                    }
                }
            }
        }
    }

    /// Record an error on a subagent session and mark it inactive.
    pub fn mark_subagent_error(&mut self, session_id: &str, error: &str) {
        if let Some(parent_task_id) = self.session_tracker.subagent_to_parent.get(session_id).cloned() {
            if let Some(sessions) = self.session_tracker.subagent_sessions.get_mut(&parent_task_id) {
                for sub in sessions.iter_mut() {
                    if sub.session_id == session_id {
                        sub.active = false;
                        sub.error_message = Some(error.to_string());
                        break;
                    }
                }
            }
        }
    }

    /// Get the parent task ID for a subagent session.
    pub fn get_parent_task_for_subagent(&self, session_id: &str) -> Option<&str> {
        self.session_tracker.subagent_to_parent.get(session_id).map(|s| s.as_str())
    }

    /// Get all subagent sessions for a parent task.
    pub fn get_subagent_sessions(&self, parent_task_id: &str) -> &[SubagentSession] {
        self.session_tracker.subagent_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.streaming_text = text;
        session.render_version += 1;
    }

    // ─── Dirty Flag ──────────────────────────────────────────────────────

    /// Mark a specific task as needing to be persisted on the next save.
    /// Also sets the global dirty flag so the persistence loop knows to run.
    pub fn mark_task_dirty(&mut self, task_id: &str) {
        self.dirty_flags.dirty_tasks.insert(task_id.to_string());
        self.dirty_flags.mark_dirty();
    }

    /// Evict stale entries from the streaming render cache.
    ///
    /// Removes cached lines whose key no longer has a corresponding live
    /// session — either a main task in `self.tasks` (keyed by `task_id`)
    /// or a drilled-down subagent in `self.session_tracker.subagent_session_data` (keyed
    /// by `session_id`).  If the cache still exceeds `max_entries` after
    /// that, the oldest half is evicted (by insertion order).
    pub fn prune_streaming_cache(&mut self, max_entries: usize) {
        // Remove entries whose backing session no longer exists.
        // Main sessions are keyed by task_id; subagent sessions by session_id.
        self.session_tracker.cached_streaming_lines
            .retain(|key, _| {
                self.tasks.contains_key(key)
                    || self.session_tracker.subagent_session_data.contains_key(key)
            });

        // Also evict subagent session data for sessions whose parent task no longer exists
        self.session_tracker.subagent_session_data.retain(|session_id, _| {
            self.session_tracker.subagent_to_parent.contains_key(session_id)
        });

        // If still too large, remove the oldest half (first N/2 entries)
        if self.session_tracker.cached_streaming_lines.len() > max_entries {
            let to_remove = self.session_tracker.cached_streaming_lines.len() / 2;
            let keys: Vec<String> = self
                .session_tracker.cached_streaming_lines
                .keys()
                .take(to_remove)
                .cloned()
                .collect();
            for key in keys {
                self.session_tracker.cached_streaming_lines.remove(&key);
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
        self.project_registry.projects = projects;
        self.tasks.clear();
        for task in tasks {
            let id = task.id.clone();
            if let Some(ref sid) = task.session_id {
                self.session_tracker.session_to_task.insert(sid.clone(), id.clone());
            }
            self.tasks.insert(id, task);
        }
        self.kanban.columns = kanban_columns;
        self.project_registry.active_project_id = active_project_id;
        self.project_registry.task_number_counters = counters;

        if let Some(ref pid) = self.project_registry.active_project_id {
            let pid = pid.clone();
            if !self.kanban.columns.is_empty() {
                // We have persisted kanban order — filter to the active project,
                // preserving the order from the database.
                self.filter_kanban_for_project(&pid);
            }
            // If kanban_columns was empty (no persisted order) or after
            // filtering nothing remains for this project, fall back to
            // rebuilding from self.tasks.
            if self.kanban.columns.is_empty() {
                self.rebuild_kanban_for_project(&pid);
            }
        }
    }

    // ─── Internal Helpers ────────────────────────────────────────────────

    /// Filter `self.kanban.columns` in place to only include tasks belonging
    /// to the given project, preserving the persisted order from the database.
    /// Also resets focus state (focused_task_index, scroll offset, etc.).
    ///
    /// This is used during [`Self::restore_state`] to narrow the DB-loaded
    /// kanban (which contains ALL projects' tasks) down to the active project,
    /// without losing the persisted task ordering.
    fn filter_kanban_for_project(&mut self, project_id: &str) {
        // Build a set of task IDs that belong to this project
        let project_task_ids: std::collections::HashSet<&str> = self
            .tasks
            .values()
            .filter(|t| t.project_id == project_id)
            .map(|t| t.id.as_str())
            .collect();

        // Retain only this project's tasks in each column, preserving order
        for task_ids in self.kanban.columns.values_mut() {
            task_ids.retain(|id| project_task_ids.contains(id.as_str()));
        }

        // Remove empty columns (no tasks from this project)
        self.kanban.columns.retain(|_, ids| !ids.is_empty());

        // Reset focus state
        self.kanban.focused_task_index.clear();
        self.kanban.kanban_scroll_offset = 0;

        // Prefer "planning" as the default focused column; fall back to first available
        let focused_col = if self.kanban.columns.contains_key("planning") {
            "planning".to_string()
        } else if self.kanban.columns.contains_key("todo") {
            "todo".to_string()
        } else {
            self.kanban
                .columns
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "planning".to_string())
        };

        self.ui.focused_column = focused_col.clone();
        self.kanban.focused_column_index = 1; // "planning" is index 1 in default config
        self.kanban.focused_task_index.insert(focused_col.clone(), 0);
        self.ui.focused_task_id = self
            .kanban
            .columns
            .get(&focused_col)
            .and_then(|ids| ids.first().cloned());
    }

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
        self.kanban.focused_task_index.clear();
        self.kanban.kanban_scroll_offset = 0;

        // Prefer "planning" as the default focused column; fall back to first available
        let focused_col = if self.kanban.columns.contains_key("planning") {
            "planning".to_string()
        } else if self.kanban.columns.contains_key("todo") {
            "todo".to_string()
        } else {
            self.kanban
                .columns
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "planning".to_string())
        };

        self.ui.focused_column = focused_col.clone();
        self.kanban.focused_column_index = 1; // "planning" is index 1 in default config
        self.kanban.focused_task_index.insert(focused_col.clone(), 0);
        self.ui.focused_task_id = self
            .kanban
            .columns
            .get(&focused_col)
            .and_then(|ids| ids.first().cloned());
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
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("proj-1".to_string());

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
                planning_context: None,
                pending_description: None,
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

        state.open_task_editor_create("todo");
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
        state.open_task_editor_create("todo");

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
        state.open_task_editor_create("todo");

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
            .session_tracker.session_to_task
            .insert("session-abc".to_string(), "task-1".to_string());

        let deleted = state.delete_task("task-1");
        assert_eq!(deleted, Some("session-abc".to_string()));
        assert!(!state.session_tracker.session_to_task.contains_key("session-abc"));
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

        state.move_task("task-0", KanbanColumn("running".to_string()));
        // Should be updated to current time (much larger than 1000)
        assert!(state.tasks.get("task-0").unwrap().entered_column_at > 1000);
    }

    // ── Hung Agent Detection ─────────────────────────────────────────────

    #[test]
    fn check_hung_agents_marks_stale_running_tasks() {
        let mut state = make_state_with_tasks();
        let now = chrono::Utc::now().timestamp();

        // task-0 is Running with very old last_activity_at
        {
            let task = state.tasks.get_mut("task-0").unwrap();
            task.agent_status = AgentStatus::Running;
            task.last_activity_at = now - 600; // 10 minutes ago
        }
        // task-1 is Running but recent
        {
            let task = state.tasks.get_mut("task-1").unwrap();
            task.agent_status = AgentStatus::Running;
            task.last_activity_at = now - 10; // 10 seconds ago
        }
        // task-2 is Complete (should be ignored)
        {
            let task = state.tasks.get_mut("task-2").unwrap();
            task.agent_status = AgentStatus::Complete;
            task.last_activity_at = now - 600;
        }

        // 5 minute timeout
        let newly_hung = state.check_hung_agents(300);
        assert_eq!(newly_hung, 1);
        assert_eq!(state.tasks.get("task-0").unwrap().agent_status, AgentStatus::Hung);
        assert_eq!(state.tasks.get("task-1").unwrap().agent_status, AgentStatus::Running);
        assert_eq!(state.tasks.get("task-2").unwrap().agent_status, AgentStatus::Complete);
    }

    #[test]
    fn check_hung_agents_does_not_re_mark_already_hung() {
        let mut state = make_state_with_tasks();
        let now = chrono::Utc::now().timestamp();

        {
            let task = state.tasks.get_mut("task-0").unwrap();
            task.agent_status = AgentStatus::Hung;
            task.last_activity_at = now - 600;
        }

        let newly_hung = state.check_hung_agents(300);
        assert_eq!(newly_hung, 0);
    }

    // ── Circuit Breaker ──────────────────────────────────────────────────

    #[test]
    fn circuit_breaker_trips_after_threshold() {
        let mut state = AppState::default();

        // 2 failures with threshold 3 — not yet tripped
        assert!(!state.project_registry.record_agent_failure("proj-1", 3));
        assert!(!state.project_registry.record_agent_failure("proj-1", 3));
        // 3rd failure — trips
        assert!(state.project_registry.record_agent_failure("proj-1", 3));
        assert!(state.project_registry.is_circuit_breaker_tripped("proj-1", 3));

        // Reset
        state.project_registry.reset_circuit_breaker("proj-1");
        assert!(!state.project_registry.is_circuit_breaker_tripped("proj-1", 3));
    }

    #[test]
    fn circuit_breaker_resets_on_success() {
        let mut state = AppState::default();

        // 2 failures
        state.project_registry.record_agent_failure("proj-1", 3);
        state.project_registry.record_agent_failure("proj-1", 3);
        // Success resets
        state.project_registry.record_agent_success("proj-1");
        assert_eq!(state.project_registry.circuit_breaker_failures.get("proj-1"), None);

        // 3 more failures should trip again
        state.project_registry.record_agent_failure("proj-1", 3);
        state.project_registry.record_agent_failure("proj-1", 3);
        state.project_registry.record_agent_failure("proj-1", 3);
        assert!(state.project_registry.is_circuit_breaker_tripped("proj-1", 3));
    }

    #[test]
    fn circuit_breaker_is_per_project() {
        let mut state = AppState::default();

        state.project_registry.record_agent_failure("proj-1", 2);
        state.project_registry.record_agent_failure("proj-1", 2);
        assert!(state.project_registry.is_circuit_breaker_tripped("proj-1", 2));

        // proj-2 is unaffected
        assert!(!state.project_registry.is_circuit_breaker_tripped("proj-2", 2));
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
        ..Default::default()
        };
        let p2 = CortexProject {
            id: "p2".to_string(),
            name: "Project 2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        ..Default::default()
        };
        state.add_project(p1);
        state.add_project(p2);

        state.select_project("p2");
        assert_eq!(state.project_registry.active_project_id, Some("p2".to_string()));
    }

    // ── Dirty flag ──────────────────────────────────────────────────────

    #[test]
    fn mark_dirty_sets_flag() {
        let state = AppState::default();
        assert!(!state.dirty_flags.dirty.load(std::sync::atomic::Ordering::Relaxed));
        state.mark_dirty();
        assert!(state.dirty_flags.dirty.load(std::sync::atomic::Ordering::Relaxed));
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
        assert_eq!(state.project_registry.projects[0].name, "New Project Name");
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
        assert_eq!(state.project_registry.projects[0].name, "Test Project");
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
        assert_eq!(state.project_registry.projects[0].name, "Test Project");
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
        assert_eq!(state.project_registry.projects[0].working_directory, "/tmp");
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
        assert_eq!(state.project_registry.projects[0].working_directory, "/tmp");
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
        assert_eq!(state.project_registry.projects[0].working_directory, "/tmp");
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
        assert_eq!(state.project_registry.projects[0].working_directory, "/tmp");
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
        assert_eq!(state.project_registry.projects[0].working_directory, "/tmp");
    }

    // ── Streaming text cap ─────────────────────────────────────────────

    #[test]
    fn streaming_text_truncates_when_cap_exceeded() {
        let mut state = make_state_with_tasks();
        // Set up a session mapping
        let session_id = "session-abc";
        state.tasks.get_mut("task-0").unwrap().session_id = Some(session_id.to_string());
        state.session_tracker.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Fill buffer well past the truncation threshold (cap + 10% = ~1.15MB).
        // Write 1.2MB of ASCII in a single delta.
        let chunk_size = STREAMING_TEXT_CAP_BYTES + STREAMING_TEXT_CAP_BYTES / 5;
        let big_chunk = "x".repeat(chunk_size);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", &big_chunk);

        let text = state.session_tracker.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();

        // Should be truncated to at most the cap size (plus a small
        // tolerance for UTF-8 boundary alignment)
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
        state.session_tracker.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Write data well below the cap
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "hello world");
        state.process_message_part_delta(session_id, "msg-1", "part-2", "text", " and more");

        let text = state.session_tracker.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();
        assert_eq!(text, "hello world and more");
    }

    #[test]
    fn streaming_text_truncation_preserves_utf8_boundary() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";
        state.tasks.get_mut("task-0").unwrap().session_id = Some(session_id.to_string());
        state.session_tracker.session_to_task.insert(session_id.to_string(), "task-0".to_string());

        // Fill past the cap with multi-byte characters (emoji are 4 bytes each)
        let emoji = "🎉"; // 4 bytes
        let count = (STREAMING_TEXT_CAP_BYTES / 4) + 100_000;
        let big_chunk = emoji.repeat(count);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", &big_chunk);

        let text = state.session_tracker.task_sessions.get("task-0").unwrap().streaming_text.as_ref().unwrap();

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
        assert!(!state.project_registry.projects.is_empty());

        state.restore_state(vec![], vec![], HashMap::new(), None, HashMap::new());

        assert!(state.tasks.is_empty());
        assert!(state.project_registry.projects.is_empty());
        assert!(state.project_registry.active_project_id.is_none());
        assert!(state.project_registry.task_number_counters.is_empty());
        assert!(state.session_tracker.session_to_task.is_empty());
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
        ..Default::default()
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
            planning_context: None,
            pending_description: None,
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

        assert_eq!(state.project_registry.projects.len(), 1);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.project_registry.active_project_id, Some("proj-1".to_string()));
        assert_eq!(state.project_registry.task_number_counters.get("proj-1"), Some(&5));
        assert_eq!(
            state.session_tracker.session_to_task.get("sess-1"),
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
        ..Default::default()
        };
        let project2 = CortexProject {
            id: "proj-2".to_string(),
            name: "P2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        ..Default::default()
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
            planning_context: None,
            pending_description: None,
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
            planning_context: None,
            pending_description: None,
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
        ..Default::default()
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
            planning_context: None,
            pending_description: None,
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

        assert!(state.project_registry.active_project_id.is_none());
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
        ..Default::default()
        };
        let task1 = CortexTask {
            id: "task-1".to_string(),
            number: 1,
            title: "T1".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: Some("sess-a".to_string()),
            agent_status: AgentStatus::Running,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
            agent_type: None,
        };
        let task2 = CortexTask {
            id: "task-2".to_string(),
            number: 2,
            title: "T2".to_string(),
            description: String::new(),
            column: KanbanColumn("todo".to_string()),
            session_id: Some("sess-b".to_string()),
            agent_status: AgentStatus::Pending,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
            agent_type: None,
        };

        state.restore_state(
            vec![project],
            vec![task1, task2],
            HashMap::new(),
            Some("proj-1".to_string()),
            HashMap::new(),
        );

        assert_eq!(state.session_tracker.session_to_task.get("sess-a"), Some(&"task-1".to_string()));
        assert_eq!(state.session_tracker.session_to_task.get("sess-b"), Some(&"task-2".to_string()));
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
        ..Default::default()
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

        assert_eq!(state.project_registry.task_number_counters.get("proj-1"), Some(&42));
        assert_eq!(state.project_registry.task_number_counters.get("proj-2"), Some(&7));
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
        ..Default::default()
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
            planning_context: None,
            pending_description: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-2".to_string(),
        };
        state.tasks.insert("task-p2".to_string(), p2_task);

        assert_eq!(state.project_registry.projects.len(), 2);
        assert_eq!(state.tasks.len(), 4); // 3 from proj-1 + 1 from proj-2

        state.remove_project("proj-1");

        assert_eq!(state.project_registry.projects.len(), 1);
        assert_eq!(state.project_registry.projects[0].id, "proj-2");
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
        state.session_tracker.session_to_task.insert("sess-1".to_string(), "task-0".to_string());
        state.session_tracker.task_sessions.insert("task-0".to_string(), TaskDetailSession {
            task_id: "task-0".to_string(),
            session_id: Some("sess-1".to_string()),
            ..Default::default()
        });
        state.session_tracker.cached_streaming_lines.insert("task-0".to_string(), (0, vec![]));

        state.remove_project("proj-1");

        // session_to_task should be cleaned
        assert!(!state.session_tracker.session_to_task.contains_key("sess-1"));
        // task_sessions should be cleaned
        assert!(!state.session_tracker.task_sessions.contains_key("task-0"));
        // cached_streaming_lines should be cleaned
        assert!(!state.session_tracker.cached_streaming_lines.contains_key("task-0"));
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
        state.project_registry.active_project_id = Some("proj-1".to_string());

        let p2 = CortexProject {
            id: "proj-2".to_string(),
            name: "Project 2".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 1,
        ..Default::default()
        };
        state.add_project(p2);

        state.remove_project("proj-1");

        assert_eq!(state.project_registry.active_project_id, Some("proj-2".to_string()));
    }

    #[test]
    fn remove_only_project_clears_active_project() {
        let mut state = make_state_with_tasks();
        state.project_registry.active_project_id = Some("proj-1".to_string());

        state.remove_project("proj-1");

        assert!(state.project_registry.active_project_id.is_none());
    }

    #[test]
    fn remove_nonexistent_project_is_noop() {
        let mut state = make_state_with_tasks();
        let project_count = state.project_registry.projects.len();
        let task_count = state.tasks.len();

        state.remove_project("nonexistent");

        assert_eq!(state.project_registry.projects.len(), project_count);
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
            .session_tracker.session_to_task
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
            .session_tracker.task_sessions
            .get("task-0")
            .unwrap()
            .streaming_text
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(text, "hello world");

        // New session starts on the SAME task — clear_session_data must be
        // called explicitly to clear old streaming_text.
        state.set_task_session_id("task-0", Some(session_2.to_string()));
        state.clear_session_data("task-0");

        // Verify streaming_text is cleared
        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert!(
            session.streaming_text.is_none(),
            "streaming_text should be cleared when clear_session_data is called"
        );
        // Verify messages are also cleared
        assert!(
            session.messages.is_empty(),
            "messages should be cleared when clear_session_data is called"
        );
        // Verify render_version was bumped (cache invalidation)
        assert!(
            session.render_version > 0,
            "render_version should be bumped to invalidate cache"
        );
        // Verify cached_streaming_lines was removed
        assert!(
            !state.session_tracker.cached_streaming_lines.contains_key("task-0"),
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

        // New session starts — clear_session_data should NOT remove the
        // TaskDetailSession entry itself, just clear its fields
        state.set_task_session_id("task-0", Some(session_2.to_string()));
        state.clear_session_data("task-0");

        // The session entry should still exist
        assert!(
            state.session_tracker.task_sessions.contains_key("task-0"),
            "TaskDetailSession entry should still exist for the task"
        );
        // But with cleaned fields
        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert_eq!(session.task_id, "task-0");
        assert!(session.streaming_text.is_none());
        assert!(session.messages.is_empty());
    }

    #[test]
    fn set_task_session_id_does_not_clear_session_data() {
        // Regression test: set_task_session_id should NOT clear streaming data
        // on its own. Callers must call clear_session_data explicitly.
        let mut state = make_state_with_tasks();
        let session_1 = "session-old";
        let session_2 = "session-new";

        setup_session_mapping(&mut state, "task-0", session_1);
        state.process_message_part_delta(session_1, "msg-1", "part-1", "text", "important data");

        // Set new session ID WITHOUT calling clear_session_data
        state.set_task_session_id("task-0", Some(session_2.to_string()));

        // Streaming data should STILL be present
        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert_eq!(
            session.streaming_text.as_deref(),
            Some("important data"),
            "streaming_text should NOT be cleared by set_task_session_id alone"
        );
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
        state.session_tracker.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "subagent output ");

        let entry = state.session_tracker.subagent_session_data.get(sub_session).unwrap();
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
        if let Some(entry) = state.session_tracker.subagent_session_data.get_mut(sub_session) {
            entry.streaming_text = None; // This is what handle_drill_down_subagent does
            entry.render_version += 1;
        }

        // After drill-down, streaming_text should be None (not duplicated)
        let entry = state.session_tracker.subagent_session_data.get(sub_session).unwrap();
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
        state.session_tracker.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Subagent receives deltas: part-1 then part-2
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "hello ");
        state.process_message_part_delta(sub_session, "msg-1", "part-2", "text", "world");

        // Replay of part-1 (earlier part, different key from last) → skipped
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "hello ");

        let text = state
            .session_tracker.subagent_session_data
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
        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
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

        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
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

        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert_eq!(session.messages.len(), 1);
        assert!(session.streaming_text.is_none());
    }

    #[test]
    fn regression_finalize_session_bumps_render_version() {
        let mut state = make_state_with_tasks();
        let session_id = "session-abc";

        setup_session_mapping(&mut state, "task-0", session_id);
        state.process_message_part_delta(session_id, "msg-1", "part-1", "text", "text");

        let version_before = state.session_tracker.task_sessions.get("task-0").unwrap().render_version;

        let messages = vec![make_text_message("msg-1", MessageRole::Assistant, "text")];
        state.finalize_session_streaming("task-0", messages);

        let version_after = state.session_tracker.task_sessions.get("task-0").unwrap().render_version;
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

        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
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
            .session_tracker.task_sessions
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
        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
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
        state.session_tracker.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Subagent receives a reasoning delta — should be ignored
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "reasoning", "thinking...");

        let entry = state.session_tracker.subagent_session_data.get(sub_session).unwrap();
        assert!(
            entry.streaming_text.is_none(),
            "Subagent reasoning deltas should not appear in streaming_text"
        );

        // But a text delta should work
        state.process_message_part_delta(sub_session, "msg-1", "part-2", "text", "actual text");

        let entry = state.session_tracker.subagent_session_data.get(sub_session).unwrap();
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

        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert!(session.streaming_text.is_none());
        assert_eq!(session.messages.len(), 1);

        // Phase 3: New session starts (auto-progression)
        state.set_task_session_id("task-0", Some(session_2.to_string()));
        state.clear_session_data("task-0");

        let session = state.session_tracker.task_sessions.get("task-0").unwrap();
        assert!(session.streaming_text.is_none());
        assert!(session.messages.is_empty(), "messages cleared for new session");

        // Phase 4: Second session streams
        state.process_message_part_delta(session_2, "msg-2", "part-1", "text", "second ");
        state.process_message_part_delta(session_2, "msg-2", "part-2", "text", "run");

        // Simulate reconnection: replay an earlier part (part-1)
        state.process_message_part_delta(session_2, "msg-2", "part-1", "text", "second ");

        let text = state
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
            .session_tracker.task_sessions
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
        state.session_tracker.subagent_to_parent.insert(sub_session.to_string(), "task-0".to_string());

        // Simulate two SSE loops delivering the same subagent delta
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "sub ");
        state.process_message_part_delta(sub_session, "msg-1", "part-1", "text", "sub ");

        let text = state
            .session_tracker.subagent_session_data
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

        let project = state.project_registry.projects.iter().find(|p| p.id == "proj-1").unwrap();
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

        let project = state.project_registry.projects.iter().find(|p| p.id == "proj-1").unwrap();
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

        let project = state.project_registry.projects.iter().find(|p| p.id == "proj-1").unwrap();
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

        let project = state.project_registry.projects.iter().find(|p| p.id == "proj-1").unwrap();
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
        ..Default::default()
        };
        state.add_project(project2);

        // Give task-0 a pending question
        if let Some(task) = state.tasks.get_mut("task-0") {
            task.pending_question_count = 1;
        }

        // Only update proj-1
        state.update_project_status("proj-1");

        let proj1 = state.project_registry.projects.iter().find(|p| p.id == "proj-1").unwrap();
        let proj2 = state.project_registry.projects.iter().find(|p| p.id == "proj-2").unwrap();
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
            .session_tracker.session_to_task
            .insert(session_id.to_string(), "task-0".to_string());

        state.process_session_status(session_id, "busy");

        assert_eq!(
            state.tasks.get("task-0").unwrap().agent_status,
            AgentStatus::Running,
            "'busy' status should map to AgentStatus::Running"
        );
    }

    #[test]
    fn process_session_status_complete_does_not_overwrite_after_session_reassigned() {
        // Simulates the race where auto-progression has already moved the task
        // to a new column, reassigned it a NEW session, and started the review
        // agent. Then a stale SessionStatus "complete" arrives for the OLD
        // session. The guard checks session_id mismatch.
        let mut state = make_state_with_tasks();
        let old_session_id = "session-old";
        let new_session_id = "session-new-review";

        // Set up task as if auto-progression already happened:
        // task has a NEW session_id (the old mapping was cleared and re-created
        // for the new session), but the OLD session mapping still exists in
        // session_to_task (simulating a race where the old mapping wasn't
        // cleared before the stale event arrived).
        state.tasks.get_mut("task-0").unwrap().session_id =
            Some(new_session_id.to_string());
        state.tasks.get_mut("task-0").unwrap().agent_status =
            AgentStatus::Running;
        state.tasks.get_mut("task-0").unwrap().column =
            KanbanColumn("review".to_string());
        // The stale old session still maps to this task (race condition)
        state
            .session_tracker
            .session_to_task
            .insert(old_session_id.to_string(), "task-0".to_string());
        state
            .kanban
            .columns
            .entry("review".to_string())
            .or_default()
            .push("task-0".to_string());

        // Stale SessionStatus "complete" arrives for the OLD session
        state.process_session_status(old_session_id, "complete");

        // Status should remain Running, not be overwritten to Complete
        assert_eq!(
            state.tasks.get("task-0").unwrap().agent_status,
            AgentStatus::Running,
            "stale SessionStatus 'complete' should not overwrite Running when session was reassigned"
        );
    }

    #[test]
    fn process_session_status_complete_still_sets_complete_when_session_matches() {
        // Normal case: the task's session_id matches the incoming event,
        // so "complete" should still apply (the agent actually finished).
        let mut state = make_state_with_tasks();
        let session_id = "session-complete-normal";
        state
            .tasks
            .get_mut("task-0")
            .unwrap()
            .session_id = Some(session_id.to_string());
        state.tasks.get_mut("task-0").unwrap().agent_status =
            AgentStatus::Running;
        state
            .session_tracker
            .session_to_task
            .insert(session_id.to_string(), "task-0".to_string());

        state.process_session_status(session_id, "complete");

        assert_eq!(
            state.tasks.get("task-0").unwrap().agent_status,
            AgentStatus::Complete,
            "'complete' should apply when the task's session matches the incoming event"
        );
    }

    // ── Subagent Error Tracking ───────────────────────────────────────

    #[test]
    fn mark_subagent_error_records_error_and_deactivates() {
        let mut state = AppState::default();
        let sub_session = "sub-session-1";

        state.register_subagent_session("task-0", sub_session, "do");

        // Initially active, no error
        let subs = state.get_subagent_sessions("task-0");
        assert_eq!(subs.len(), 1);
        assert!(subs[0].active);
        assert!(subs[0].error_message.is_none());

        // Record error
        state.mark_subagent_error(sub_session, "Provider auth failed");

        let subs = state.get_subagent_sessions("task-0");
        assert_eq!(subs.len(), 1);
        assert!(!subs[0].active);
        assert_eq!(subs[0].error_message.as_deref(), Some("Provider auth failed"));
    }

    #[test]
    fn mark_subagent_error_does_not_affect_other_subagents() {
        let mut state = AppState::default();
        let sub1 = "sub-session-1";
        let sub2 = "sub-session-2";

        state.register_subagent_session("task-0", sub1, "do");
        state.register_subagent_session("task-0", sub2, "explore");

        state.mark_subagent_error(sub1, "Error");

        let subs = state.get_subagent_sessions("task-0");
        assert_eq!(subs.len(), 2);
        assert!(!subs.iter().find(|s| s.session_id == sub1).unwrap().active);
        assert!(subs.iter().find(|s| s.session_id == sub2).unwrap().active);
    }

    // ── Pending Description & Detail Editor ────────────────────────────

    #[test]
    fn save_detail_description_applies_immediately_when_ready() {
        let mut state = make_state_with_tasks();
        // Set task to Ready status
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Ready;

        // Open detail and edit description
        state.open_task_detail("task-0");
        if let Some(ed) = state.ui.detail_editor.as_mut() {
            ed.set_description("New description");
            ed.is_focused = true;
        }

        // Save
        let result = state.save_detail_description();
        assert!(result.is_ok());

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.description, "New description");
        assert_eq!(task.title, "New description");
        assert!(task.pending_description.is_none());
    }

    #[test]
    fn save_detail_description_applies_immediately_when_complete() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Complete;

        state.open_task_detail("task-0");
        if let Some(ed) = state.ui.detail_editor.as_mut() {
            ed.set_description("Updated when done");
        }

        state.save_detail_description().unwrap();

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.description, "Updated when done");
        assert!(task.pending_description.is_none());
    }

    #[test]
    fn save_detail_description_queues_when_running() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Running;

        state.open_task_detail("task-0");
        if let Some(ed) = state.ui.detail_editor.as_mut() {
            ed.set_description("Queued description");
        }

        state.save_detail_description().unwrap();

        let task = state.tasks.get("task-0").unwrap();
        // Original description should remain unchanged
        assert_eq!(task.description, "");
        // Pending description should be set
        assert_eq!(task.pending_description.as_deref(), Some("Queued description"));
        // Title should still reflect the pending change
        assert_eq!(task.title, "Queued description");
    }

    #[test]
    fn save_detail_description_queues_when_pending() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Pending;

        state.open_task_detail("task-0");
        if let Some(ed) = state.ui.detail_editor.as_mut() {
            ed.set_description("Queued for later");
        }

        state.save_detail_description().unwrap();

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.pending_description.as_deref(), Some("Queued for later"));
    }

    #[test]
    fn save_detail_description_empty_fails() {
        let mut state = make_state_with_tasks();
        state.open_task_detail("task-0");
        // Leave description empty
        let result = state.save_detail_description();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn pending_description_applied_on_status_change_to_ready() {
        let mut state = make_state_with_tasks();
        // Set pending description on a running task
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Running;
        state.tasks.get_mut("task-0").unwrap().pending_description = Some("Will be applied".to_string());

        // Transition to Ready — pending description should be applied
        state.update_task_agent_status("task-0", AgentStatus::Ready);

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.description, "Will be applied");
        assert_eq!(task.title, "Will be applied");
        assert!(task.pending_description.is_none());
    }

    #[test]
    fn pending_description_applied_on_status_change_to_complete() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().agent_status = AgentStatus::Running;
        state.tasks.get_mut("task-0").unwrap().pending_description = Some("Applied on complete".to_string());

        state.update_task_agent_status("task-0", AgentStatus::Complete);

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.description, "Applied on complete");
        assert!(task.pending_description.is_none());
    }

    #[test]
    fn pending_description_not_applied_on_running() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().pending_description = Some("Should not apply".to_string());

        state.update_task_agent_status("task-0", AgentStatus::Running);

        let task = state.tasks.get("task-0").unwrap();
        assert_eq!(task.description, "");
        assert_eq!(task.pending_description.as_deref(), Some("Should not apply"));
    }

    #[test]
    fn open_task_detail_initializes_editor_from_pending_description() {
        let mut state = make_state_with_tasks();
        state.tasks.get_mut("task-0").unwrap().description = "Original".to_string();
        state.tasks.get_mut("task-0").unwrap().pending_description = Some("Pending version".to_string());

        state.open_task_detail("task-0");

        let editor = state.ui.detail_editor.as_ref().unwrap();
        assert_eq!(editor.description(), "Pending version");
    }

    #[test]
    fn close_task_detail_clears_editor() {
        let mut state = make_state_with_tasks();
        state.open_task_detail("task-0");
        assert!(state.ui.detail_editor.is_some());

        state.close_task_detail();
        assert!(state.ui.detail_editor.is_none());
    }
}
