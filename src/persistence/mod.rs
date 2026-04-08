//! SQLite persistence layer for tasks, projects, and kanban order.

pub mod db;

use crate::error::AppResult;
use crate::state::types::{AppState, CortexProject, CortexTask, KanbanColumn};
use db::Db;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Persist all tasks and projects from state to the database.
pub fn save_state(state: &AppState, db: &Db) -> AppResult<()> {
    // Save all projects
    for project in &state.projects {
        db.save_project(project)?;
    }

    // Save all tasks (depends on projects — saved above)
    for task in state.tasks.values() {
        db.save_task(task)?;
    }

    // Save kanban order (depends on tasks — saved above)
    for (column_id, task_ids) in &state.kanban.columns {
        db.save_kanban_order(&KanbanColumn(column_id.clone()), task_ids)?;
    }

    // Save active project
    if let Some(ref pid) = state.active_project_id {
        db.set_metadata("active_project_id", pid)?;
    }

    // Save task number counters
    for (pid, counter) in &state.task_number_counters {
        db.set_metadata(&format!("counter_{}", pid), &counter.to_string())?;
    }

    Ok(())
}

/// Restore state from the database into AppState.
pub fn restore_state(state: &mut AppState, db: &Db) -> AppResult<()> {
    // Load projects
    let projects = db.load_projects()?;
    let mut counters: HashMap<String, u32> = HashMap::new();

    // Load tasks per project
    let mut all_tasks: Vec<CortexTask> = Vec::new();
    for project in &projects {
        let tasks = db.load_tasks(&project.id)?;
        all_tasks.extend(tasks);

        // Load counter
        if let Ok(Some(counter_str)) = db.get_metadata(&format!("counter_{}", project.id)) {
            if let Ok(counter) = counter_str.parse::<u32>() {
                counters.insert(project.id.clone(), counter);
            }
        }
    }

    // Load kanban order
    let kanban_order = db.load_kanban_order()?;

    // Load active project
    let active_project_id = db.get_metadata("active_project_id")?;

    // Restore state
    state.restore_state(
        projects,
        all_tasks,
        kanban_order,
        active_project_id,
        counters,
    );

    tracing::info!(
        "Restored state: {} projects, {} tasks",
        state.projects.len(),
        state.tasks.len()
    );

    Ok(())
}

/// Check dirty flag and save if needed. Returns true if saved.
pub fn save_if_dirty(state: &AppState, db: &Db) -> AppResult<bool> {
    if state.take_dirty() {
        save_state(state, db)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
