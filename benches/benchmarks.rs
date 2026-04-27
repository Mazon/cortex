//! Criterion benchmarks for hot paths in Cortex.
//!
//! Run with: cargo bench

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use cortex::config::types::CortexConfig;
use cortex::state::types::{
    AgentStatus, AppState, CortexProject, CortexTask, CursorDirection, KanbanColumn,
    TaskEditorState,
};
use cortex::tui::keys::KeyMatcher;
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
        pending_permission_count: 0,
        pending_question_count: 0,
        entered_column_at: 0,
        last_activity_at: 0,
        created_at: 0,
        updated_at: 0,
        project_id: "bench-project".to_string(),
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
    };
    state.projects.push(project);
    state.active_project_id = Some("bench-project".to_string());

    for i in 0..n {
        let task_id = format!("task-{i}");
        let task = make_task(&task_id, &format!("Benchmark Task {i}"));
        state.tasks.insert(task_id, task);
        state.dirty_tasks.insert(format!("task-{i}"));
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
                state.dirty_tasks.insert(task_id.clone());
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
        let mut editor =
            TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
        b.iter(|| {
            editor.insert_char('a');
            // Reset to prevent unbounded growth
            if editor.description().len() > 500 {
                editor =
                    TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
            }
        });
    });

    group.bench_function("insert_char_middle", |b| {
        let mut editor =
            TaskEditorState::new_for_create("todo", vec!["todo".to_string()]);
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
            black_box(TaskEditorState::new_for_create(
                "todo",
                vec!["todo".to_string()],
            ));
        });
    });

    group.finish();
}

criterion_group!(benches, bench_save_state, bench_match_key, bench_task_editor);
criterion_main!(benches);
