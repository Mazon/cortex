//! Archive and dependency methods on AppState.
//!
//! Archive functionality is a new feature scaffold — `get_archived_task_ids`
//! and `unarchive_task` are stubs that return empty results since there is no
//! "archived" concept in the existing data model.

use crate::state::types::*;

impl AppState {
    /// Returns task IDs that have been archived for the given project.
    ///
    /// Stub: always returns an empty `Vec` since the archive feature is
    /// entirely new and there is no "archived" concept in the data model yet.
    pub fn get_archived_task_ids(&self, _project_id: &str) -> Vec<String> {
        Vec::new()
    }

    /// Unarchive a task by ID.
    ///
    /// Stub: always returns `false` since there is no archive mechanism yet.
    pub fn unarchive_task(&mut self, _task_id: &str) -> bool {
        false
    }

    /// Add a dependency: marks `task_id` as blocked by `dep_id`.
    ///
    /// Validates that:
    /// - Both tasks exist
    /// - No self-dependency (task_id != dep_id)
    /// - No duplicate dependency
    pub fn add_dependency(&mut self, task_id: &str, dep_id: &str) -> Result<(), String> {
        if task_id == dep_id {
            return Err("Cannot add self-dependency".to_string());
        }

        if !self.tasks.contains_key(task_id) {
            return Err(format!("Task '{}' not found", task_id));
        }

        if !self.tasks.contains_key(dep_id) {
            return Err(format!("Dependency task '{}' not found", dep_id));
        }

        if let Some(task) = self.tasks.get_mut(task_id) {
            if task.blocked_by.contains(&dep_id.to_string()) {
                return Err("Dependency already exists".to_string());
            }
            task.blocked_by.push(dep_id.to_string());
        }

        self.mark_task_dirty(task_id);
        self.mark_render_dirty();
        Ok(())
    }

    /// Remove a dependency: removes `dep_id` from `task_id`'s blocked_by list.
    pub fn remove_dependency(&mut self, task_id: &str, dep_id: &str) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.blocked_by.retain(|d| d != dep_id);
        }
        self.mark_task_dirty(task_id);
        self.mark_render_dirty();
    }
}
