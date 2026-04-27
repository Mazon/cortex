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
}
