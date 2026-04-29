//! Permission and question handling methods on AppState.
//!
//! Extracted from `store.rs` to separate permission/question request
//! management from pure CRUD, navigation, and SSE processing.

use crate::state::types::*;

impl AppState {
    /// Add a pending permission request to a task's session.
    /// Updates the task's `pending_permission_count`.
    pub fn add_permission_request(&mut self, task_id: &str, request: PermissionRequest) {
        let session = self
            .session_tracker
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
            .session_tracker
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
            .session_tracker
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
            .session_tracker
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

    /// After resolving a question, check if the task should transition out
    /// of `AgentStatus::Question`.
    ///
    /// Returns `true` if the task was in `Question` status and all questions
    /// are now resolved (`pending_question_count == 0`). The caller should
    /// then re-apply the Ready/Complete + auto-progression logic.
    pub fn should_reassess_after_question(&self, task_id: &str) -> bool {
        self.tasks.get(task_id).map_or(false, |t| {
            t.agent_status == AgentStatus::Question && t.pending_question_count == 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task_with_status(status: AgentStatus, question_count: u32) -> (AppState, String) {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("proj-1".to_string());

        let task_id = "task-1".to_string();
        let task = CortexTask {
            id: task_id.clone(),
            number: 1,
            title: "Test".to_string(),
            description: String::new(),
            column: KanbanColumn("planning".to_string()),
            session_id: None,
            agent_type: Some("planning".to_string()),
            agent_status: status,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: question_count,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        state.tasks.insert(task_id.clone(), task);
        (state, task_id)
    }

    #[test]
    fn should_reassess_returns_true_when_question_status_and_no_questions() {
        let (state, task_id) = make_task_with_status(AgentStatus::Question, 0);
        assert!(state.should_reassess_after_question(&task_id));
    }

    #[test]
    fn should_reassess_returns_false_when_question_status_but_still_has_questions() {
        let (state, task_id) = make_task_with_status(AgentStatus::Question, 2);
        assert!(!state.should_reassess_after_question(&task_id));
    }

    #[test]
    fn should_reassess_returns_false_when_not_in_question_status() {
        let (state, task_id) = make_task_with_status(AgentStatus::Complete, 0);
        assert!(!state.should_reassess_after_question(&task_id));
    }

    #[test]
    fn should_reassess_returns_false_for_nonexistent_task() {
        let state = AppState::default();
        assert!(!state.should_reassess_after_question("nonexistent"));
    }

    #[test]
    fn resolve_question_decrements_count() {
        let (mut state, task_id) = make_task_with_status(AgentStatus::Question, 2);
        // Add questions to the session
        state.add_question_request(
            &task_id,
            QuestionRequest {
                id: "q1".to_string(),
                session_id: "s1".to_string(),
                question: "Q1".to_string(),
                answers: vec!["A".to_string()],
                status: "pending".to_string(),
            },
        );
        state.add_question_request(
            &task_id,
            QuestionRequest {
                id: "q2".to_string(),
                session_id: "s1".to_string(),
                question: "Q2".to_string(),
                answers: vec!["B".to_string()],
                status: "pending".to_string(),
            },
        );
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 2);

        // Resolve one question
        state.resolve_question_request(&task_id, "q1");
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 1);
        // Still shouldn't reassess (one question remaining)
        assert!(!state.should_reassess_after_question(&task_id));

        // Resolve the second question
        state.resolve_question_request(&task_id, "q2");
        assert_eq!(state.tasks.get(&task_id).unwrap().pending_question_count, 0);
        // Now should reassess
        assert!(state.should_reassess_after_question(&task_id));
    }
}
