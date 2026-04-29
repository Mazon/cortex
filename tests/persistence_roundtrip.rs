//! Integration tests for persistence round-trip (save → load → verify).
//!
//! These tests exercise the full persistence pipeline: build AppState →
//! save_state → restore_state → assert equality. They complement the
//! unit tests in `src/persistence/mod.rs` by covering additional scenarios.

use cortex::persistence::{db::Db, restore_state, save_state};
use cortex::state::types::*;

/// Helper: create a unique temp database path per test.
/// The returned TempDir must be kept alive for the duration of the test;
/// it cleans up automatically when dropped at the end of the test function.
fn temp_db() -> (std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().join("test.db").to_path_buf();
    (path, dir)
}

/// Helper: build a minimal project.
fn make_project(id: &str, name: &str) -> CortexProject {
    CortexProject {
        id: id.to_string(),
        name: name.to_string(),
        working_directory: format!("/tmp/{}", id),
        status: ProjectStatus::Idle,
        position: 0,
        ..Default::default()
    }
}

/// Helper: build a minimal task.
fn make_task(id: &str, project_id: &str, column: &str) -> CortexTask {
    CortexTask {
        id: id.to_string(),
        number: 1,
        title: format!("Task {}", id),
        description: format!("Description for {}", id),
        column: KanbanColumn(column.to_string()),
        session_id: None,
        agent_type: None,
        agent_status: AgentStatus::Pending,
        entered_column_at: 1000,
        last_activity_at: 1000,
        error_message: None,
        plan_output: None,
        planning_context: None,
        pending_description: None,
        queued_prompt: None,
        pending_permission_count: 0,
        pending_question_count: 0,
        review_status: ReviewStatus::Pending,
        created_at: 1000,
        updated_at: 1000,
        project_id: project_id.to_string(),
    }
}

// ─── Empty state round-trip ─────────────────────────────────────────────

#[test]
fn empty_state_round_trips_cleanly() {
    let (db_path, _dir) = temp_db();
    let _ = std::fs::remove_file(&db_path);
    let db = Db::new(&db_path).unwrap();

    let mut original = AppState::default();
    save_state(&mut original, &db).unwrap();

    let mut restored = AppState::default();
    restore_state(&mut restored, &db).unwrap();

    assert!(restored.project_registry.projects.is_empty());
    assert!(restored.tasks.is_empty());
    assert!(restored.kanban.columns.is_empty());
    assert!(restored.project_registry.active_project_id.is_none());

    let _ = std::fs::remove_file(&db_path);
}

// ─── Multiple projects round-trip ───────────────────────────────────────

#[test]
fn multiple_projects_round_trip() {
    let (db_path, _dir) = temp_db();
    let _ = std::fs::remove_file(&db_path);
    let db = Db::new(&db_path).unwrap();

    let proj_a = make_project("proj-a", "Project A");
    let proj_b = make_project("proj-b", "Project B");
    let proj_c = make_project("proj-c", "Project C");

    let task_a = make_task("task-a1", "proj-a", "todo");
    let task_b = make_task("task-b1", "proj-b", "running");

    let mut original = AppState::default();
    original.add_project(proj_a.clone());
    original.add_project(proj_b.clone());
    original.add_project(proj_c.clone());
    original.tasks.insert(task_a.id.clone(), task_a.clone());
    original.tasks.insert(task_b.id.clone(), task_b.clone());

    // Kanban for active project
    original
        .kanban
        .columns
        .insert("todo".to_string(), vec![task_a.id.clone()]);
    original.project_registry.active_project_id = Some("proj-a".to_string());
    original.project_registry.task_number_counters.insert("proj-a".to_string(), 1);
    original.project_registry.task_number_counters.insert("proj-b".to_string(), 1);

    // Mark all dirty
    original.dirty_flags.dirty_tasks.insert(task_a.id.clone());
    original.dirty_flags.dirty_tasks.insert(task_b.id.clone());
    save_state(&mut original, &db).unwrap();

    let mut restored = AppState::default();
    restore_state(&mut restored, &db).unwrap();

    assert_eq!(restored.project_registry.projects.len(), 3);
    assert_eq!(restored.tasks.len(), 2);
    assert_eq!(
        restored.project_registry.active_project_id,
        Some("proj-a".to_string())
    );

    let _ = std::fs::remove_file(&db_path);
}

// ─── Double save produces consistent state ──────────────────────────────

#[test]
fn double_save_produces_consistent_state() {
    let (db_path, _dir) = temp_db();
    let _ = std::fs::remove_file(&db_path);
    let db = Db::new(&db_path).unwrap();

    let project = make_project("proj-1", "Project");
    let task = make_task("task-1", "proj-1", "todo");

    let mut original = AppState::default();
    original.add_project(project.clone());
    original.tasks.insert(task.id.clone(), task.clone());
    original.kanban.columns.insert("todo".to_string(), vec![task.id.clone()]);
    original.project_registry.active_project_id = Some("proj-1".to_string());

    // First save
    original.dirty_flags.dirty_tasks.insert(task.id.clone());
    save_state(&mut original, &db).unwrap();

    // Second save (nothing changed — dirty set is now empty)
    save_state(&mut original, &db).unwrap();

    // Restore and verify
    let mut restored = AppState::default();
    restore_state(&mut restored, &db).unwrap();

    assert_eq!(restored.tasks.len(), 1);
    assert_eq!(
        restored.tasks.get(&task.id).unwrap().title,
        task.title
    );

    let _ = std::fs::remove_file(&db_path);
}

// ─── Task with all optional fields populated ────────────────────────────

#[test]
fn task_with_all_fields_round_trips() {
    let (db_path, _dir) = temp_db();
    let _ = std::fs::remove_file(&db_path);
    let db = Db::new(&db_path).unwrap();

    let project = make_project("proj-1", "Project");

    let task = CortexTask {
        id: "full-task".to_string(),
        number: 42,
        title: "Full Featured Task".to_string(),
        description: "Line 1\nLine 2\nLine 3".to_string(),
        column: KanbanColumn("running".to_string()),
        session_id: Some("sess-abc".to_string()),
        agent_type: Some("do".to_string()),
        agent_status: AgentStatus::Running,
        entered_column_at: 1_700_000_000,
        last_activity_at: 1_700_000_100,
        error_message: Some("Something went wrong".to_string()),
        plan_output: Some("Step 1: Do X\nStep 2: Do Y".to_string()),
        planning_context: None,
        pending_description: Some("Pending description".to_string()),
        queued_prompt: None,
        pending_permission_count: 5,
        pending_question_count: 2,
        review_status: ReviewStatus::Pending,
        created_at: 1_699_999_000_000,
        updated_at: 1_700_000_100_000,
        project_id: "proj-1".to_string(),
    };

    let mut original = AppState::default();
    original.add_project(project);
    original.tasks.insert(task.id.clone(), task.clone());
    original.kanban.columns.insert("running".to_string(), vec![task.id.clone()]);
    original.project_registry.active_project_id = Some("proj-1".to_string());
    original.dirty_flags.dirty_tasks.insert(task.id.clone());
    save_state(&mut original, &db).unwrap();

    let mut restored = AppState::default();
    restore_state(&mut restored, &db).unwrap();

    let rt = restored.tasks.get(&task.id).unwrap();
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

    let _ = std::fs::remove_file(&db_path);
}

// ─── Save after restore preserves all data ──────────────────────────────

#[test]
fn save_after_restore_preserves_data() {
    let (db_path, _dir) = temp_db();
    let _ = std::fs::remove_file(&db_path);
    let db = Db::new(&db_path).unwrap();

    let project = make_project("proj-1", "Project");
    let task1 = make_task("task-1", "proj-1", "todo");
    let task2 = make_task("task-2", "proj-1", "running");

    let mut original = AppState::default();
    original.add_project(project);
    original.tasks.insert(task1.id.clone(), task1.clone());
    original.tasks.insert(task2.id.clone(), task2.clone());
    original.kanban.columns.insert("todo".to_string(), vec![task1.id.clone()]);
    original.kanban.columns.insert("running".to_string(), vec![task2.id.clone()]);
    original.project_registry.active_project_id = Some("proj-1".to_string());
    original.dirty_flags.dirty_tasks.insert(task1.id.clone());
    original.dirty_flags.dirty_tasks.insert(task2.id.clone());
    save_state(&mut original, &db).unwrap();

    // Restore
    let mut restored = AppState::default();
    restore_state(&mut restored, &db).unwrap();
    assert_eq!(restored.tasks.len(), 2);

    // Save again
    restored.dirty_flags.dirty_tasks.insert(task1.id.clone());
    save_state(&mut restored, &db).unwrap();

    // Restore again — should still have 2 tasks
    let mut restored2 = AppState::default();
    restore_state(&mut restored2, &db).unwrap();
    assert_eq!(restored2.tasks.len(), 2);

    let _ = std::fs::remove_file(&db_path);
}
