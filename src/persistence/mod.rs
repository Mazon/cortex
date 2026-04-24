//! SQLite persistence layer for tasks, projects, and kanban order.

pub mod db;

use anyhow::Result;
use crate::state::types::{AppState, CortexTask, KanbanColumn};
use db::Db;
use std::collections::HashMap;

/// Persist all tasks and projects from state to the database.
///
/// All writes are performed inside a single SQLite transaction so that a
/// crash mid-save never leaves the database in an inconsistent state.
///
/// Only tasks whose IDs are in `state.dirty_tasks` are written; unchanged
/// tasks are skipped. The dirty set is cleared on successful commit.
pub fn save_state(state: &mut AppState, db: &Db) -> Result<()> {
    let tx = db.conn.unchecked_transaction()?;

    // Save all projects
    for project in &state.projects {
        db.save_project_with_conn(project, &tx)?;
    }

    // Save only dirty tasks (depends on projects — saved above)
    if state.dirty_tasks.is_empty() {
        // No tasks changed — skip task writes but still save kanban/metadata.
        tracing::debug!("save_state: no dirty tasks, skipping task writes");
    } else {
        for task_id in &state.dirty_tasks {
            if let Some(task) = state.tasks.get(task_id) {
                db.save_task_with_conn(task, &tx)?;
            }
        }
        tracing::debug!(
            "save_state: wrote {} dirty tasks",
            state.dirty_tasks.len()
        );
    }

    // Delete tasks that were removed from in-memory state
    if !state.deleted_tasks.is_empty() {
        for task_id in &state.deleted_tasks {
            db.delete_task_with_conn(task_id, &tx)?;
        }
        tracing::debug!(
            "save_state: deleted {} tasks from database",
            state.deleted_tasks.len()
        );
    }

    // Delete projects that were removed from in-memory state
    if !state.deleted_projects.is_empty() {
        for project_id in &state.deleted_projects {
            db.delete_project_with_conn(project_id, &tx)?;
        }
        tracing::debug!(
            "save_state: deleted {} projects from database",
            state.deleted_projects.len()
        );
    }

    // Save kanban order (depends on tasks — saved above)
    for (column_id, task_ids) in &state.kanban.columns {
        db.save_kanban_order_with_conn(&KanbanColumn(column_id.clone()), task_ids, &tx)?;
    }

    // Save active project
    if let Some(ref pid) = state.active_project_id {
        db.set_metadata_with_conn("active_project_id", pid, &tx)?;
    }

    // Save task number counters
    for (pid, counter) in &state.task_number_counters {
        db.set_metadata_with_conn(&format!("counter_{}", pid), &counter.to_string(), &tx)?;
    }

    tx.commit()?;

    // Clear the dirty set after successful commit
    state.dirty_tasks.clear();
    // Clear the deleted set after successful commit
    state.deleted_tasks.clear();
    // Clear the deleted projects set after successful commit
    state.deleted_projects.clear();

    Ok(())
}

/// Restore state from the database into AppState.
pub fn restore_state(state: &mut AppState, db: &Db) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::types::{
        AgentStatus, CortexProject, CortexTask, KanbanColumn, ProjectStatus,
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
            agent_type: Some("do".to_string()),
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

        let mut original = AppState {
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
                kanban_scroll_offset: 0,
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
            cached_streaming_lines: HashMap::new(),
            subagent_sessions: HashMap::new(),
            subagent_to_parent: HashMap::new(),
            subagent_session_data: HashMap::new(),
            reconnecting: false,
            reconnect_attempt: 0,
            permanently_disconnected: false,
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            render_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            dirty_tasks: std::collections::HashSet::new(),
            deleted_tasks: std::collections::HashSet::new(),
            deleted_projects: std::collections::HashSet::new(),
        };

        // ── Save ──
        // Mark the task as dirty so it gets written
        original.dirty_tasks.insert(task.id.clone());
        save_state(&mut original, &db).expect("save_state failed");

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

    // ─── Deleted task persistence ─────────────────────────────────────────

    #[test]
    fn deleted_task_removed_from_db_after_save_and_restore() {
        let db_path = temp_db_path("delete_task_persist");
        let _ = std::fs::remove_file(&db_path);

        let db = db::Db::new(&db_path).expect("failed to open test db");

        // ── Build AppState with two tasks ──
        let project = make_project();
        let task1 = make_task(); // keep this one
        let mut task2 = make_task();
        task2.id = "task-def-456".to_string();
        task2.number = 8;
        task2.title = "To be deleted".to_string();

        let mut tasks: HashMap<String, CortexTask> = HashMap::new();
        tasks.insert(task1.id.clone(), task1.clone());
        tasks.insert(task2.id.clone(), task2.clone());

        let mut kanban_columns: HashMap<String, Vec<String>> = HashMap::new();
        kanban_columns.insert("running".to_string(), vec![task1.id.clone(), task2.id.clone()]);

        let mut original = AppState {
            projects: vec![project.clone()],
            tasks,
            kanban: crate::state::types::KanbanState {
                columns: kanban_columns.clone(),
                focused_column_index: 0,
                focused_task_index: HashMap::new(),
                kanban_scroll_offset: 0,
            },
            ui: crate::state::types::UIState::default(),
            connected: false,
            active_project_id: Some("proj-1".to_string()),
            task_number_counters: HashMap::new(),
            session_to_task: HashMap::new(),
            task_sessions: HashMap::new(),
            cached_streaming_lines: HashMap::new(),
            subagent_sessions: HashMap::new(),
            subagent_to_parent: HashMap::new(),
            subagent_session_data: HashMap::new(),
            reconnecting: false,
            reconnect_attempt: 0,
            permanently_disconnected: false,
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            render_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            dirty_tasks: std::collections::HashSet::new(),
            deleted_tasks: std::collections::HashSet::new(),
            deleted_projects: std::collections::HashSet::new(),
        };

        // ── Save both tasks to DB ──
        original.dirty_tasks.insert(task1.id.clone());
        original.dirty_tasks.insert(task2.id.clone());
        save_state(&mut original, &db).expect("save_state failed (initial)");
        assert!(original.dirty_tasks.is_empty());
        assert!(original.deleted_tasks.is_empty());

        // ── Delete task2 via AppState::delete_task ──
        let _session = original.delete_task(&task2.id);
        assert!(!original.tasks.contains_key(&task2.id));
        assert!(original.deleted_tasks.contains(&task2.id));

        // ── Save again (should flush the deletion to DB) ──
        save_state(&mut original, &db).expect("save_state failed (after delete)");
        assert!(original.deleted_tasks.is_empty());

        // ── Restore into a fresh AppState ──
        let mut restored = AppState::default();
        restore_state(&mut restored, &db).expect("restore_state failed");

        // ── Assert task1 survived, task2 is gone ──
        assert_eq!(restored.tasks.len(), 1, "expected exactly 1 task after restore");
        assert!(restored.tasks.contains_key(&task1.id), "task1 should still exist");
        assert!(!restored.tasks.contains_key(&task2.id), "task2 should be deleted");

        // ── Verify at DB level too ──
        let db_tasks = db.load_tasks("proj-1").expect("load_tasks failed");
        assert_eq!(db_tasks.len(), 1, "DB should contain exactly 1 task");
        assert_eq!(db_tasks[0].id, task1.id);

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }

    // ─── Project delete round-trip ─────────────────────────────────────────

    #[test]
    fn project_delete_round_trip() {
        let db_path = temp_db_path("project_delete_round_trip");
        let _ = std::fs::remove_file(&db_path);

        let db = db::Db::new(&db_path).expect("failed to open test db");

        // ── Build AppState with one project and two tasks ──
        let project = make_project();
        let task1 = make_task();
        let mut task2 = make_task();
        task2.id = "task-def-456".to_string();
        task2.number = 8;
        task2.title = "Second task".to_string();

        let mut tasks: HashMap<String, CortexTask> = HashMap::new();
        tasks.insert(task1.id.clone(), task1.clone());
        tasks.insert(task2.id.clone(), task2.clone());

        let mut kanban_columns: HashMap<String, Vec<String>> = HashMap::new();
        kanban_columns.insert("running".to_string(), vec![task1.id.clone(), task2.id.clone()]);

        let mut counters: HashMap<String, u32> = HashMap::new();
        counters.insert("proj-1".to_string(), 8);

        let mut original = AppState {
            projects: vec![project.clone()],
            tasks,
            kanban: crate::state::types::KanbanState {
                columns: kanban_columns.clone(),
                focused_column_index: 0,
                focused_task_index: HashMap::new(),
                kanban_scroll_offset: 0,
            },
            ui: crate::state::types::UIState::default(),
            connected: false,
            active_project_id: Some("proj-1".to_string()),
            task_number_counters: counters.clone(),
            session_to_task: HashMap::new(),
            task_sessions: HashMap::new(),
            cached_streaming_lines: HashMap::new(),
            subagent_sessions: HashMap::new(),
            subagent_to_parent: HashMap::new(),
            subagent_session_data: HashMap::new(),
            reconnecting: false,
            reconnect_attempt: 0,
            permanently_disconnected: false,
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            render_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            dirty_tasks: std::collections::HashSet::new(),
            deleted_tasks: std::collections::HashSet::new(),
            deleted_projects: std::collections::HashSet::new(),
        };

        // ── Save project + tasks to DB ──
        original.dirty_tasks.insert(task1.id.clone());
        original.dirty_tasks.insert(task2.id.clone());
        save_state(&mut original, &db).expect("save_state failed (initial)");
        assert!(original.dirty_tasks.is_empty(), "dirty_tasks should be cleared after save");
        assert!(original.deleted_tasks.is_empty(), "deleted_tasks should be cleared after save");
        assert!(original.deleted_projects.is_empty(), "deleted_projects should be cleared after save");

        // ── Verify DB has the project and tasks before deletion ──
        let db_projects = db.load_projects().expect("load_projects failed");
        assert_eq!(db_projects.len(), 1, "DB should contain 1 project before delete");
        let db_tasks = db.load_tasks("proj-1").expect("load_tasks failed");
        assert_eq!(db_tasks.len(), 2, "DB should contain 2 tasks before delete");

        // ── Delete the project via remove_project() ──
        original.remove_project("proj-1");
        assert!(original.projects.is_empty(), "projects should be empty after remove");
        assert!(original.tasks.is_empty(), "tasks should be empty after remove");
        assert!(
            original.deleted_projects.contains("proj-1"),
            "deleted_projects should contain proj-1"
        );
        assert!(
            original.deleted_tasks.contains(&task1.id),
            "deleted_tasks should contain task1"
        );
        assert!(
            original.deleted_tasks.contains(&task2.id),
            "deleted_tasks should contain task2"
        );

        // ── Save again (should flush project + task deletions to DB) ──
        save_state(&mut original, &db).expect("save_state failed (after project delete)");
        assert!(
            original.deleted_projects.is_empty(),
            "deleted_projects should be cleared after save"
        );
        assert!(
            original.deleted_tasks.is_empty(),
            "deleted_tasks should be cleared after save"
        );

        // ── Verify at DB level — project and tasks are gone ──
        let db_projects = db.load_projects().expect("load_projects failed");
        assert_eq!(
            db_projects.len(),
            0,
            "DB should contain 0 projects after delete"
        );

        let db_tasks = db.load_tasks("proj-1").expect("load_tasks failed");
        assert_eq!(
            db_tasks.len(),
            0,
            "DB should contain 0 tasks after project delete"
        );

        // ── Restore into a fresh AppState — everything should be gone ──
        let mut restored = AppState::default();
        restore_state(&mut restored, &db).expect("restore_state failed");

        assert_eq!(
            restored.projects.len(),
            0,
            "restored state should have 0 projects"
        );
        assert_eq!(
            restored.tasks.len(),
            0,
            "restored state should have 0 tasks"
        );

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }
}
