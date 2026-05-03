//! Criterion benchmarks for hot paths in Cortex.
//!
//! Run with: cargo bench

use cortex::config::types::CortexConfig;
use cortex::state::types::{
    AgentStatus, AppState, CortexProject, CortexTask, CursorDirection, KanbanColumn,
    TaskEditorState,
};
use cortex::tui::keys::KeyMatcher;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Helper to build a default config.
fn default_config() -> CortexConfig {
    CortexConfig::default()
}

/// Helper to create a minimal CortexTask for benchmarking.
fn make_task(id: &str, title: &str) -> CortexTask {
    CortexTask {
        id: id.to_string(),
        number: 1,
        title: title.to_string(),
        description: String::new(),
        column: KanbanColumn("todo".to_string()),
        agent_type: None,
        agent_status: AgentStatus::Pending,
        session_id: None,
        error_message: None,
        plan_output: None,
        planning_context: None,
        pending_description: None,
        queued_prompt: None,
        pending_permission_count: 0,
        pending_question_count: 0,
        review_status: cortex::state::types::ReviewStatus::Pending,
        had_write_operations: false,
        entered_column_at: 0,
        last_activity_at: 0,
        created_at: 0,
        updated_at: 0,
        project_id: "bench-project".to_string(),
        blocked_by: Vec::new(),
    }
}

/// Helper to create an AppState with N tasks in a HashMap.
fn state_with_tasks(n: usize) -> AppState {
    let mut state = AppState::default();
    let project = CortexProject {
        id: "bench-project".to_string(),
        name: "Bench Project".to_string(),
        working_directory: "/tmp".to_string(),
        status: cortex::state::types::ProjectStatus::Idle,
        position: 0,
        ..Default::default()
    };
    state.project_registry.projects.push(project);
    state.project_registry.active_project_id = Some("bench-project".to_string());

    for i in 0..n {
        let task_id = format!("task-{i}");
        let task = make_task(&task_id, &format!("Benchmark Task {i}"));
        state.tasks.insert(task_id, task);
        state.dirty_flags.dirty_tasks.insert(format!("task-{i}"));
    }
    state
}

// ─── save_state benchmark ─────────────────────────────────────────────────

fn bench_save_state(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bench.db");

    let mut group = c.benchmark_group("save_state");

    for size in [1, 10, 50, 100] {
        let mut state = state_with_tasks(size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            // Re-mark tasks dirty for each iteration
            for task_id in state.tasks.keys() {
                state.dirty_flags.dirty_tasks.insert(task_id.clone());
            }

            let db = cortex::persistence::db::Db::new(&db_path).unwrap();
            b.iter(|| {
                cortex::persistence::save_state(black_box(&mut state), black_box(&db)).unwrap();
            });
        });
    }

    group.finish();
}

// ─── KeyMatcher::match_key benchmark ─────────────────────────────────────

fn bench_match_key(c: &mut Criterion) {
    let config = default_config();
    let matcher = KeyMatcher::from_config(&config.keybindings);

    let mut group = c.benchmark_group("match_key");

    // Benchmark a hit (first binding in the list — best case)
    let key_quit = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    group.bench_function("hit_first_binding", |b| {
        b.iter(|| black_box(matcher.match_key(black_box(key_quit))));
    });

    // Benchmark a miss (no matching binding — worst case, full scan)
    let key_miss = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    group.bench_function("miss_full_scan", |b| {
        b.iter(|| black_box(matcher.match_key(black_box(key_miss))));
    });

    // Benchmark a hit in the middle of the bindings list
    let key_down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    group.bench_function("hit_mid_binding", |b| {
        b.iter(|| black_box(matcher.match_key(black_box(key_down))));
    });

    group.finish();
}

// ─── TaskEditorState benchmarks ──────────────────────────────────────────

fn bench_task_editor(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_editor");

    group.bench_function("insert_char_end", |b| {
        let mut editor = TaskEditorState::new_for_create("todo");
        b.iter(|| {
            editor.insert_char('a');
            // Reset to prevent unbounded growth
            if editor.description().len() > 500 {
                editor = TaskEditorState::new_for_create("todo");
            }
        });
    });

    group.bench_function("insert_char_middle", |b| {
        let mut editor = TaskEditorState::new_for_create("todo");
        // Build a string first
        for _ in 0..100 {
            editor.insert_char('x');
        }
        b.iter(|| {
            // Move cursor to middle then insert
            editor.move_cursor(CursorDirection::Home);
            for _ in 0..50 {
                editor.move_cursor(CursorDirection::Right);
            }
            editor.insert_char('a');
        });
    });

    group.bench_function("new_for_create", |b| {
        b.iter(|| {
            black_box(TaskEditorState::new_for_create("todo"));
        });
    });

    group.finish();
}

// ─── Store CRUD benchmarks ─────────────────────────────────────────────

fn bench_add_task(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_crud");

    group.bench_function("add_task", |b| {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "bench-project".to_string(),
            name: "Bench Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: cortex::state::types::ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("bench-project".to_string());

        let mut counter = 0u32;
        b.iter(|| {
            counter += 1;
            black_box(state.create_todo(
                format!("Benchmark Task {}", counter),
                format!("Description for task {}", counter),
                "bench-project",
            ));
        });
    });

    group.bench_function("move_task", |b| {
        let mut state = state_with_tasks(10);
        // Pre-create columns
        state.kanban.columns.insert("running".to_string(), Vec::new());
        let task_id = state.tasks.keys().next().cloned().unwrap();

        b.iter(|| {
            black_box(state.move_task(&task_id, KanbanColumn("running".to_string())));
        });
    });

    group.bench_function("delete_task", |b| {
        b.iter_batched(
            || {
                let mut state = state_with_tasks(5);
                let task_id = state.tasks.keys().next().cloned().unwrap();
                (state, task_id)
            },
            |(mut state, task_id)| {
                black_box(state.delete_task(&task_id));
                black_box(state);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.bench_function("remove_project", |b| {
        b.iter_batched(
            || {
                let mut state = AppState::default();
                for i in 0..5 {
                    let task = make_task(&format!("task-{i}"), &format!("Task {i}"));
                    state.tasks.insert(task.id.clone(), task);
                }
                state.add_project(CortexProject {
                    id: "bench-project".to_string(),
                    name: "Bench".to_string(),
                    working_directory: "/tmp".to_string(),
                    status: cortex::state::types::ProjectStatus::Idle,
                    position: 0,
                    ..Default::default()
                });
                state
            },
            |mut state| {
                black_box(state.remove_project("bench-project"));
                black_box(state);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.bench_function("update_session_messages", |b| {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "bench-project".to_string(),
            name: "Bench".to_string(),
            working_directory: "/tmp".to_string(),
            status: cortex::state::types::ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("bench-project".to_string());

        let task = make_task("task-1", "Task 1");
        state.tasks.insert(task.id.clone(), task.clone());
        state.session_tracker.session_to_task.insert("sess-1".to_string(), task.id.clone());

        // Pre-build messages to reuse across iterations
        let messages = vec![cortex::state::types::TaskMessage {
            id: "msg-bench-1".to_string(),
            role: cortex::state::types::MessageRole::Assistant,
            parts: vec![cortex::state::types::TaskMessagePart::Text {
                text: "Some streaming text content ".repeat(100),
            }],
            created_at: None,
        }];

        b.iter(|| {
            black_box(state.update_session_messages("task-1", messages.clone()));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_save_state,
    bench_match_key,
    bench_task_editor,
    bench_add_task,
);
criterion_main!(benches);
