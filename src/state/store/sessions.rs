//! Session data and subagent management methods on AppState.

use crate::state::types::*;

impl AppState {
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
        if self
            .session_tracker
            .subagent_to_parent
            .contains_key(session_id)
        {
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
            self.session_tracker
                .subagent_to_parent
                .get(&parent_session_id)
                .and_then(|ptid| self.session_tracker.subagent_sessions.get(ptid))
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|s| s.session_id == parent_session_id)
                        .map(|s| s.depth)
                })
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
        self.session_tracker
            .subagent_sessions
            .entry(parent_task_id.to_string())
            .or_default()
            .push(subagent.clone());

        // Reverse index: child session → parent task
        self.session_tracker
            .subagent_to_parent
            .insert(session_id.to_string(), parent_task_id.to_string());
    }

    /// Mark a subagent session as inactive (completed or errored).
    pub fn mark_subagent_inactive(&mut self, session_id: &str) {
        if let Some(parent_task_id) = self
            .session_tracker
            .subagent_to_parent
            .get(session_id)
            .cloned()
        {
            if let Some(sessions) = self
                .session_tracker
                .subagent_sessions
                .get_mut(&parent_task_id)
            {
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
        if let Some(parent_task_id) = self
            .session_tracker
            .subagent_to_parent
            .get(session_id)
            .cloned()
        {
            if let Some(sessions) = self
                .session_tracker
                .subagent_sessions
                .get_mut(&parent_task_id)
            {
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
        self.session_tracker
            .subagent_to_parent
            .get(session_id)
            .map(|s| s.as_str())
    }

    /// Get all subagent sessions for a parent task.
    pub fn get_subagent_sessions(&self, parent_task_id: &str) -> &[SubagentSession] {
        self.session_tracker
            .subagent_sessions
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
            .session_tracker
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
            .session_tracker
            .task_sessions
            .entry(task_id.to_string())
            .or_insert_with(|| TaskDetailSession {
                task_id: task_id.to_string(),
                ..Default::default()
            });
        session.streaming_text = text;
        session.render_version += 1;
    }
}
