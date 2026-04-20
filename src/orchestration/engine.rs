//! Orchestration engine — config-driven task progression rules.

use std::sync::{Arc, Mutex};

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::opencode::client::OpenCodeClient;
use crate::state::types::{AgentStatus, AppState, KanbanColumn, TaskAgentType};

/// Called when a task is moved to a new column.
/// Starts an agent if the column has one configured.
pub fn on_task_moved(
    task_id: &str,
    to_column: &KanbanColumn,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    columns_config: &ColumnsConfig,
    opencode_config: &OpenCodeConfig,
) {
    if let Some(agent) = columns_config.agent_for_column(&to_column.0) {
        start_agent(task_id, &agent, state, client, opencode_config);
    }
}

/// Start an agent for a task.
fn start_agent(
    task_id: &str,
    agent: &str,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    opencode_config: &OpenCodeConfig,
) {
    // Update status immediately for UI feedback
    {
        let mut s = state.lock().unwrap();
        s.update_task_agent_status(task_id, AgentStatus::Running);
    }

    // Clone what we need, spawn async work
    let state = state.clone();
    let client = client.clone();
    let agent = agent.to_string();
    let task_id = task_id.to_string();
    let model_id = opencode_config.model.id.clone();
    let model_provider = opencode_config.model.provider.clone();

    // Check if the agent has a specific model configured
    let agent_model = {
        let s = state.lock().unwrap();
        s.tasks.get(&task_id).map(|_| {
            opencode_config.agents.get(&agent).and_then(|a| a.model.clone())
        }).flatten()
    };

    tokio::spawn(async move {
        // Build prompt from task
        let (prompt, session_id, task_id_clone) = {
            let mut s = state.lock().unwrap();
            let task = match s.tasks.get(&task_id) {
                Some(t) => t.clone(),
                None => {
                    tracing::warn!("Task {} not found for agent start", task_id);
                    return;
                }
            };

            let prompt = OpenCodeClient::build_prompt_for_agent(&task, &agent, None);
            (prompt, task.session_id.clone(), task.id.clone())
        };

        // Create session if needed
        let sid = if let Some(ref existing_sid) = session_id {
            existing_sid.clone()
        } else {
            match client.create_session().await {
                Ok(session) => {
                    let sid = session.id.clone();
                    // Store session ID
                    {
                        let mut s = state.lock().unwrap();
                        s.set_task_session_id(&task_id_clone, Some(sid.clone()));
                    }
                    sid
                }
                Err(e) => {
                    tracing::error!("Failed to create session: {}", e);
                    let mut s = state.lock().unwrap();
                    s.set_task_error(&task_id_clone, format!("Failed to create session: {}", e));
                    return;
                }
            }
        };

        // Determine model
        let model = agent_model.as_deref().map(|m| {
            if m.contains('/') {
                m.to_string()
            } else {
                let provider = model_provider.as_deref().unwrap_or("z.ai");
                format!("{}/{}", provider, m)
            }
        }).or_else(|| {
            let provider = model_provider.as_deref().unwrap_or("z.ai");
            Some(format!("{}/{}", provider, model_id))
        });

        // Send prompt
        match client
            .send_prompt(&sid, &prompt, Some(&agent), model.as_deref())
            .await
        {
            Ok(_) => {
                tracing::info!("Prompt sent to agent '{}' for task {}", agent, task_id_clone);
            }
            Err(e) => {
                tracing::error!("Failed to send prompt: {}", e);
                let mut s = state.lock().unwrap();
                s.set_task_error(
                    &task_id_clone,
                    format!("Failed to send prompt: {}", e),
                );
            }
        }
    });
}

/// Called when an agent completes (from SSE SessionIdle).
/// Auto-progresses if the column configures it.
pub fn on_agent_completed(
    task_id: &str,
    state: &mut AppState,
    columns_config: &ColumnsConfig,
) {
    let column = state
        .tasks
        .get(task_id)
        .map(|t| t.column.clone());
    if let Some(col) = column {
        if let Some(target) = columns_config.auto_progress_for(&col.0) {
            tracing::info!(
                "Auto-progressing task {} from {} to {}",
                task_id,
                col.0,
                target
            );
            state.move_task(task_id, KanbanColumn(target));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{ColumnConfig, ColumnsConfig};
    use crate::state::types::*;

    /// Build a minimal `AppState` with a task in a given column.
    fn make_state_with_task_in_column(column: &str) -> (AppState, String) {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        state.add_project(project);
        state.active_project_id = Some("proj-1".to_string());

        let task_id = "task-1".to_string();
        let task = CortexTask {
            id: task_id.clone(),
            number: 1,
            title: "Test Task".to_string(),
            description: String::new(),
            column: KanbanColumn(column.to_string()),
            session_id: None,
            agent_type: TaskAgentType::Planning,
            agent_status: AgentStatus::Complete,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        state.tasks.insert(task_id.clone(), task);
        state
            .kanban
            .columns
            .entry(column.to_string())
            .or_default()
            .push(task_id.clone());

        (state, task_id)
    }

    /// Build a `ColumnsConfig` where `from_col` auto-progresses to `to_col`.
    fn make_columns_with_auto_progress(from_col: &str, to_col: &str) -> ColumnsConfig {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: from_col.to_string(),
                    display_name: Some(from_col.to_string()),
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some(to_col.to_string()),
                },
                ColumnConfig {
                    id: to_col.to_string(),
                    display_name: Some(to_col.to_string()),
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        config
    }

    // ── Auto-progression ────────────────────────────────────────────────

    #[test]
    fn auto_progress_moves_task_to_target_column() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should now be in "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        assert!(!state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&task_id));
    }

    #[test]
    fn auto_progress_updates_entered_column_at() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        let old_entered = state.tasks.get(&task_id).unwrap().entered_column_at;
        on_agent_completed(&task_id, &mut state, &columns_config);

        let new_entered = state.tasks.get(&task_id).unwrap().entered_column_at;
        assert!(new_entered >= old_entered);
    }

    #[test]
    fn no_auto_progress_when_not_configured() {
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config where "running" has no auto_progress_to
        let mut columns_config = make_columns_with_auto_progress("planning", "running");
        // Override "running" to have no auto-progression
        columns_config.definitions[1].auto_progress_to = None;

        on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
    }

    #[test]
    fn no_auto_progress_for_nonexistent_task() {
        let (mut state, _task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        // Should not panic for a nonexistent task
        on_agent_completed("nonexistent-task", &mut state, &columns_config);

        // Original task should still be in planning
        assert!(state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&_task_id));
    }

    #[test]
    fn auto_progress_chains_through_multiple_columns() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");

        // Chain: planning → running → review
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "planning".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some("running".to_string()),
                },
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: Some("review".to_string()),
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        // First completion: planning → running
        on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");

        // Second completion: running → review
        on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");

        // Third completion: review stays (no auto_progress_to)
        on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
    }
}
