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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::types::{
        AgentStatus, CortexProject, CortexTask, KanbanColumn, ProjectStatus, TaskAgentType,
    };
    use std::collections::HashMap;

    /// Helper: create a temporary database path that is unique per test invocation.
    fn temp_db_path(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("cortex_test_persistence");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("test_{}_{}.db", std::process::id(), suffix))
    }

    /// Helper: build a fully-populated CortexTask for testing.
    fn make_task() -> CortexTask {
        CortexTask {
            id: "task-abc-123".to_string(),
            number: 7,
            title: "Implement round-trip persistence".to_string(),
            description: "Save the task, load it back, assert every field matches exactly.\nMultiline description line 2.".to_string(),
            column: KanbanColumn("running".to_string()),
            session_id: Some("sess-xyz-999".to_string()),
            agent_type: TaskAgentType::Do,
            agent_status: AgentStatus::Running,
            entered_column_at: 1_710_000_000_000,
            last_activity_at: 1_710_001_000_000,
            error_message: Some("previous attempt timed out".to_string()),
            plan_output: Some("Step 1: save\nStep 2: load\nStep 3: verify".to_string()),
            pending_permission_count: 3,
            pending_question_count: 1,
            created_at: 1_710_000_000_000,
            updated_at: 1_710_001_000_000,
            project_id: "proj-1".to_string(),
        }
    }

    /// Helper: build a CortexProject matching the task's project_id.
    fn make_project() -> CortexProject {
        CortexProject {
            id: "proj-1".to_string(),
            name: "Cortex TUI".to_string(),
            working_directory: "/home/user/cortex".to_string(),
            status: ProjectStatus::Working,
            position: 0,
        }
    }

    // ─── Db-level task round-trip ────────────────────────────────────────

    #[test]
    fn db_task_save_load_round_trip() {
        let db_path = temp_db_path("task_crud");
        // Remove any leftover from a previous run with the same PID.
        let _ = std::fs::remove_file(&db_path);

        let db = db::Db::new(&db_path).expect("failed to open test db");

        // Tasks have a FK to projects, so save the project first.
        let project = make_project();
        db.save_project(&project).expect("save_project failed");

        let original = make_task();
        db.save_task(&original).expect("save_task failed");

        let loaded = db
            .load_tasks(&original.project_id)
            .expect("load_tasks failed");

        assert_eq!(loaded.len(), 1, "expected exactly one task");
        let t = &loaded[0];

        assert_eq!(t.id, original.id);
        assert_eq!(t.number, original.number);
        assert_eq!(t.title, original.title);
        assert_eq!(t.description, original.description);
        assert_eq!(t.column, original.column);
        assert_eq!(t.session_id, original.session_id);
        assert_eq!(t.agent_type, original.agent_type);
        assert_eq!(t.agent_status, original.agent_status);
        assert_eq!(t.entered_column_at, original.entered_column_at);
        assert_eq!(t.last_activity_at, original.last_activity_at);
        assert_eq!(t.error_message, original.error_message);
        assert_eq!(t.plan_output, original.plan_output);
        assert_eq!(
            t.pending_permission_count,
            original.pending_permission_count
        );
        assert_eq!(t.pending_question_count, original.pending_question_count);
        assert_eq!(t.created_at, original.created_at);
        assert_eq!(t.updated_at, original.updated_at);
        assert_eq!(t.project_id, original.project_id);

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }

    // ─── Full save_state / restore_state round-trip ──────────────────────

    #[test]
    fn save_state_restore_state_round_trip() {
        let db_path = temp_db_path("save_restore");
        let _ = std::fs::remove_file(&db_path);

        let db = db::Db::new(&db_path).expect("failed to open test db");

        // ── Build the original AppState ──
        let project = make_project();
        let task = make_task();

        let mut tasks: HashMap<String, CortexTask> = HashMap::new();
        tasks.insert(task.id.clone(), task.clone());

        let mut kanban_columns: HashMap<String, Vec<String>> = HashMap::new();
        kanban_columns.insert("todo".to_string(), vec![]);
        kanban_columns.insert("running".to_string(), vec![task.id.clone()]);

        let mut counters: HashMap<String, u32> = HashMap::new();
        counters.insert("proj-1".to_string(), 8);

        let original = AppState {
            projects: vec![project.clone()],
            tasks,
            kanban: crate::state::types::KanbanState {
                columns: kanban_columns.clone(),
                focused_column_index: 1,
                focused_task_index: {
                    let mut m = HashMap::new();
                    m.insert("running".to_string(), 0);
                    m
                },
            },
            ui: crate::state::types::UIState::default(),
            connected: true,
            active_project_id: Some("proj-1".to_string()),
            task_number_counters: counters.clone(),
            session_to_task: {
                let mut m = HashMap::new();
                if let Some(ref sid) = task.session_id {
                    m.insert(sid.clone(), task.id.clone());
                }
                m
            },
            task_sessions: HashMap::new(),
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // ── Save ──
        save_state(&original, &db).expect("save_state failed");

        // ── Restore into a fresh AppState ──
        let mut restored = AppState::default();
        restore_state(&mut restored, &db).expect("restore_state failed");

        // ── Assert projects ──
        assert_eq!(restored.projects.len(), 1);
        let rp = &restored.projects[0];
        assert_eq!(rp.id, project.id);
        assert_eq!(rp.name, project.name);
        assert_eq!(rp.working_directory, project.working_directory);
        assert_eq!(rp.status, project.status);
        assert_eq!(rp.position, project.position);

        // ── Assert tasks (the core round-trip) ──
        assert_eq!(restored.tasks.len(), 1);
        let rt = restored
            .tasks
            .get(&task.id)
            .expect("task not found after restore");
        assert_eq!(rt.id, task.id);
        assert_eq!(rt.number, task.number);
        assert_eq!(rt.title, task.title);
        assert_eq!(rt.description, task.description);
        assert_eq!(rt.column, task.column);
        assert_eq!(rt.session_id, task.session_id);
        assert_eq!(rt.agent_type, task.agent_type);
        assert_eq!(rt.agent_status, task.agent_status);
        assert_eq!(rt.entered_column_at, task.entered_column_at);
        assert_eq!(rt.last_activity_at, task.last_activity_at);
        assert_eq!(rt.error_message, task.error_message);
        assert_eq!(rt.plan_output, task.plan_output);
        assert_eq!(rt.pending_permission_count, task.pending_permission_count);
        assert_eq!(rt.pending_question_count, task.pending_question_count);
        assert_eq!(rt.created_at, task.created_at);
        assert_eq!(rt.updated_at, task.updated_at);
        assert_eq!(rt.project_id, task.project_id);

        // ── Assert kanban order ──
        assert_eq!(
            restored.kanban.columns.get("running"),
            Some(&vec![task.id.clone()])
        );

        // ── Assert active project ──
        assert_eq!(restored.active_project_id, Some("proj-1".to_string()));

        // ── Assert task number counter ──
        assert_eq!(restored.task_number_counters.get("proj-1"), Some(&8u32));

        // ── Assert session-to-task reverse index ──
        assert_eq!(restored.session_to_task.get("sess-xyz-999"), Some(&task.id));

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }

    // ─── Kanban order round-trip ─────────────────────────────────────────

    #[test]
    fn kanban_order_round_trip() {
        let db_path = temp_db_path("kanban_order");
        let _ = std::fs::remove_file(&db_path);

        let db = db::Db::new(&db_path).expect("failed to open test db");

        // Save project + tasks (FK constraint)
        let project = make_project();
        db.save_project(&project).expect("save_project failed");
        let task1 = make_task();
        let mut task2 = make_task();
        task2.id = "task-def-456".to_string();
        task2.number = 8;
        task2.title = "Second task".to_string();
        db.save_task(&task1).expect("save_task 1 failed");
        db.save_task(&task2).expect("save_task 2 failed");

        // Save kanban order with specific ordering
        let column = KanbanColumn("running".to_string());
        let order = vec![task2.id.clone(), task1.id.clone()]; // reversed
        db.save_kanban_order(&column, &order)
            .expect("save_kanban_order failed");

        // Load back
        let loaded = db.load_kanban_order().expect("load_kanban_order failed");
        let loaded_running = loaded.get("running").expect("running column missing");
        assert_eq!(loaded_running, &vec![task2.id, task1.id]);

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }
}
