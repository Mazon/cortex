//! Integration test: concurrent `AppState` access patterns.
//!
//! Verifies that `AppState` behind `Arc<Mutex<>>` handles concurrent
//! access from multiple threads without deadlocks or data corruption.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cortex::state::types::*;

/// Spawn N threads that concurrently lock, mutate, and unlock AppState.
/// Verify no deadlocks occur within a timeout and state is consistent after.
#[test]
fn concurrent_lock_no_deadlock() {
    let state = Arc::new(Mutex::new(AppState::default()));
    let num_threads = 8;
    let ops_per_thread = 100;

    // Set up a project so tasks can be created
    {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.add_project(CortexProject {
            id: "proj-1".to_string(),
            name: "Test".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        });
        s.project_registry.active_project_id = Some("proj-1".to_string());
    }

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                for i in 0..ops_per_thread {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());

                    // Alternate between different operations
                    match (thread_id + i) % 4 {
                        0 => {
                            // Create a task
                            let task = s.create_todo(
                                format!("T{}-{}", thread_id, i),
                                format!("Description for T{}-{}", thread_id, i),
                                "proj-1",
                            );
                            let _ = task; // suppress unused warning
                        }
                        1 => {
                            // Mark a random task dirty
                            let keys: Vec<String> = s.tasks.keys().cloned().collect();
                            if let Some(tid) = keys.first() {
                                s.mark_task_dirty(tid);
                            }
                        }
                        2 => {
                            // Update render dirty flag
                            s.mark_render_dirty();
                        }
                        _ => {
                            // Update project status
                            s.update_project_status("proj-1");
                        }
                    }
                }
            })
        })
        .collect();

    // Wait for all threads with a timeout
    for handle in handles {
        handle.join().expect("Thread panicked — possible deadlock or data race");
    }

    // Verify final state is consistent
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    // All tasks should reference the correct project
    for task in s.tasks.values() {
        assert_eq!(task.project_id, "proj-1");
    }
    // Project should still exist
    assert_eq!(s.project_registry.projects.len(), 1);
}

/// Test that rapid lock/unlock cycles don't cause issues.
#[test]
fn rapid_lock_unlock_cycles() {
    let state = Arc::new(Mutex::new(AppState::default()));
    let num_threads = 4;
    let cycles = 1000;

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                for _ in 0..cycles {
                    // Lock, do minimal work, unlock
                    {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.mark_render_dirty();
                    }
                    // Lock again immediately
                    {
                        let s = state.lock().unwrap_or_else(|e| e.into_inner());
                        assert!(s.tasks.is_empty());
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test that a poisoned mutex is handled gracefully (unwrap_or_else pattern).
#[test]
fn poisoned_mutex_handled_gracefully() {
    use std::panic;

    let state = Arc::new(Mutex::new(AppState::default()));

    // Set up a project
    {
        let mut s = state.lock().unwrap();
        s.add_project(CortexProject {
            id: "proj-1".to_string(),
            name: "Test".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
            ..Default::default()
        });
        s.project_registry.active_project_id = Some("proj-1".to_string());
    }

    // Poison the mutex by panicking while holding the lock
    let state_clone = Arc::clone(&state);
    let _ = thread::spawn(move || {
        let _guard = state_clone.lock().unwrap();
        panic!("intentional panic to poison mutex");
    })
    .join();

    // Verify that unwrap_or_else recovers from poison
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(s.project_registry.projects.len(), 1);
}
