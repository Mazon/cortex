//! Persistence restore methods on AppState.

use crate::state::types::*;
use std::collections::HashMap;

impl AppState {
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
                self.session_tracker
                    .session_to_task
                    .insert(sid.clone(), id.clone());
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

    /// Reset focus state to sensible defaults after a kanban rebuild/filter.
    ///
    /// Clears per-column task focus indices, resets scroll offset, and
    /// picks a default focused column (preferring "planning", then "todo",
    /// then the first available column).
    fn reset_focus_state(&mut self) {
        self.kanban.focused_task_index.clear();
        self.kanban.kanban_scroll_offset = 0;

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
        self.kanban
            .focused_task_index
            .insert(focused_col.clone(), 0);
        self.ui.focused_task_id = self
            .kanban
            .columns
            .get(&focused_col)
            .and_then(|ids| ids.first().cloned());
    }

    /// Filter `self.kanban.columns` in place to only include tasks belonging
    /// to the given project, preserving the persisted order from the database.
    /// Also resets focus state (focused_task_index, scroll offset, etc.).
    ///
    /// This is used during [`Self::restore_state`] to narrow the DB-loaded
    /// kanban (which contains ALL projects' tasks) down to the active project,
    /// without losing the persisted task ordering.
    pub(crate) fn filter_kanban_for_project(&mut self, project_id: &str) {
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
        self.reset_focus_state();
    }

    pub(crate) fn rebuild_kanban_for_project(&mut self, project_id: &str) {
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
        self.reset_focus_state();
    }
}
